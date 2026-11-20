//! Local HTTP API, identical in routes and payloads to the original Node server,
//! plus skill discovery and the agent run-request queue.

use std::sync::Arc;

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{header, HeaderValue, Method, StatusCode, Uri},
    response::Response,
    Router,
};
use percent_encoding::percent_decode_str;
use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::skills;
use crate::store::{ApiError, Store};

pub type SharedStore = Arc<Store>;

pub fn router(store: SharedStore) -> Router {
    Router::new().fallback(handle).with_state(store)
}

pub async fn serve(listener: tokio::net::TcpListener, store: SharedStore) -> std::io::Result<()> {
    axum::serve(listener, router(store)).await
}

async fn handle(State(store): State<SharedStore>, method: Method, uri: Uri, body: Bytes) -> Response {
    if method == Method::OPTIONS {
        return preflight();
    }
    let target = uri
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| uri.path().to_string());
    match tokio::task::spawn_blocking(move || route(&store, &method, &target, &body)).await {
        Ok((status, value)) => respond(status, &value),
        Err(_) => respond(StatusCode::INTERNAL_SERVER_ERROR, &json!({ "error": "Internal error" })),
    }
}

fn route(store: &Store, method: &Method, target: &str, body: &[u8]) -> (StatusCode, Value) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let method = method.as_str();

    match (method, path) {
        ("GET", "/api/health") => {
            return (StatusCode::OK, json!({ "ok": true, "app": "clankban", "version": "0.1.0" }));
        }
        ("GET", "/api/tasks") => {
            return reply(store.list().map(|tasks| json!({ "tasks": tasks })), StatusCode::OK);
        }
        ("GET", "/api/agent-context") => {
            return reply(store.agent_context(), StatusCode::OK);
        }
        ("POST", "/api/tasks") => {
            return match parse_body(body) {
                Ok(input) => reply(store.create(&input), StatusCode::CREATED),
                Err(response) => response,
            };
        }
        ("GET", "/api/skills") => {
            return (StatusCode::OK, json!({ "skills": skills::describe_all() }));
        }
        ("GET", "/api/run-requests") => {
            let status = query_param(query, "status");
            return reply(
                store.list_runs(status.as_deref()).map(|runs| json!({ "runs": runs })),
                StatusCode::OK,
            );
        }
        ("POST", "/api/run-requests") => {
            return match parse_body(body) {
                Ok(input) => create_run(store, &input),
                Err(response) => response,
            };
        }
        _ => {}
    }

    if let Some(raw_id) = path.strip_prefix("/api/run-requests/") {
        return match (method, raw_id.parse::<i64>()) {
            ("PATCH", Ok(id)) => match parse_body(body) {
                Ok(input) => update_run(store, id, &input),
                Err(response) => response,
            },
            _ => (StatusCode::NOT_FOUND, json!({ "error": "Route not found" })),
        };
    }

    if let Some(raw_id) = path.strip_prefix("/api/tasks/") {
        if !raw_id.is_empty() && !raw_id.contains('/') {
            let id = percent_decode_str(raw_id).decode_utf8_lossy().into_owned();
            return match method {
                "GET" => match store.get(&id) {
                    Ok(Some(task)) => reply(Ok(task), StatusCode::OK),
                    Ok(None) => error(ApiError::NotFound),
                    Err(e) => error(e),
                },
                "PATCH" => match parse_body(body) {
                    Ok(input) => reply(store.update(&id, &input), StatusCode::OK),
                    Err(response) => response,
                },
                "DELETE" => match store.delete(&id) {
                    Ok(()) => (StatusCode::OK, json!({ "deleted": id })),
                    Err(e) => error(e),
                },
                _ => (StatusCode::NOT_FOUND, json!({ "error": "Route not found" })),
            };
        }
    }

    (StatusCode::NOT_FOUND, json!({ "error": "Route not found" }))
}

fn create_run(store: &Store, input: &Map<String, Value>) -> (StatusCode, Value) {
    let requested = input.get("skill").and_then(Value::as_str).unwrap_or("");
    let Some(skill) = skills::find(requested) else {
        return error(ApiError::BadRequest(format!("Unknown skill: {requested}")));
    };
    let arguments = input.get("arguments").cloned().unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return error(ApiError::BadRequest("arguments must be an object".into()));
    }
    if let Some(missing) = skills::missing_required(skill, &arguments) {
        return error(ApiError::BadRequest(format!("Missing required argument: {missing}")));
    }
    reply(store.create_run(skill.name, &arguments), StatusCode::CREATED)
}

fn update_run(store: &Store, id: i64, input: &Map<String, Value>) -> (StatusCode, Value) {
    let Some(status) = input.get("status").and_then(Value::as_str) else {
        return error(ApiError::BadRequest("status is required".into()));
    };
    let result = input.get("result").and_then(Value::as_str);
    reply(store.update_run(id, status, result), StatusCode::OK)
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| percent_decode_str(v).decode_utf8_lossy().into_owned())
    })
}

fn reply<T: Serialize>(result: Result<T, ApiError>, ok_status: StatusCode) -> (StatusCode, Value) {
    match result {
        Ok(value) => (ok_status, serde_json::to_value(value).unwrap_or(Value::Null)),
        Err(e) => error(e),
    }
}

fn error(e: ApiError) -> (StatusCode, Value) {
    match e {
        ApiError::BadRequest(message) => (StatusCode::BAD_REQUEST, json!({ "error": message })),
        ApiError::NotFound => (StatusCode::NOT_FOUND, json!({ "error": "Task not found" })),
        ApiError::Conflict(message) => (StatusCode::CONFLICT, json!({ "error": message })),
        ApiError::Internal(message) => (StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": message })),
    }
}

/// Empty body means `{}`; anything that is not a JSON object is rejected.
fn parse_body(body: &[u8]) -> Result<Map<String, Value>, (StatusCode, Value)> {
    if body.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(Map::new());
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Err((StatusCode::BAD_REQUEST, json!({ "error": "Invalid JSON" }))),
    }
}

fn respond(status: StatusCode, value: &Value) -> Response {
    let mut response = Response::new(Body::from(value.to_string()));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("content-type"));
    response
}

fn preflight() -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    let headers = response.headers_mut();
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET,POST,PATCH,DELETE,OPTIONS"),
    );
    headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("content-type"));
    response
}
