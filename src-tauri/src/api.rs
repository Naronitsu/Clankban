//! Local HTTP API, identical in routes and payloads to the original Node server.

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
    let path = uri.path().to_string();
    match tokio::task::spawn_blocking(move || route(&store, &method, &path, &body)).await {
        Ok((status, value)) => respond(status, &value),
        Err(_) => respond(StatusCode::INTERNAL_SERVER_ERROR, &json!({ "error": "Internal error" })),
    }
}

fn route(store: &Store, method: &Method, path: &str, body: &[u8]) -> (StatusCode, Value) {
    match (method.as_str(), path) {
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
        _ => {}
    }

    if let Some(raw_id) = path.strip_prefix("/api/tasks/") {
        if !raw_id.is_empty() && !raw_id.contains('/') {
            let id = percent_decode_str(raw_id).decode_utf8_lossy().into_owned();
            return match method.as_str() {
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
