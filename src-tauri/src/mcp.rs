//! Stdio MCP bridge for local agents. Every tool call is forwarded to the
//! local HTTP API, so the board and agents share one source of truth.
//!
//! Each call identifies the agent in request headers, so the board's change history can say
//! which client and model made it. The client name comes from the MCP handshake; the model
//! comes from the optional `model` argument on write tools.

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::skills;

const PROTOCOL_VERSION: &str = "2025-06-18";

pub async fn run(api_url: String) -> std::io::Result<()> {
    let base = api_url.trim_end_matches('/').to_string();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    let mut client = String::from("Agent");

    while let Some(line) = lines.next_line().await? {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // Notifications carry no id and get no response.
        let Some(id) = message.get("id").cloned() else {
            continue;
        };
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            continue;
        };

        let outcome: Result<Value, (i64, String)> = match method {
            "initialize" => {
                client = client_name(message.get("params").unwrap_or(&Value::Null));
                Ok(json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "tools": {}, "prompts": {} },
                    "serverInfo": { "name": "clankban", "version": "0.1.0" }
                }))
            }
            "tools/list" => Ok(json!({ "tools": tool_definitions() })),
            "tools/call" => {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                match call_tool(&base, &client, &params).await {
                    Ok(value) => Ok(json!({
                        "content": [{ "type": "text", "text": value.to_string() }]
                    })),
                    Err(message) => Err((-32000, message)),
                }
            }
            "ping" => Ok(json!({})),
            "prompts/list" => Ok(json!({ "prompts": skills::describe_all() })),
            "prompts/get" => prompt_get(message.get("params").unwrap_or(&Value::Null)),
            other => Err((-32601, format!("Method not found: {other}"))),
        };

        let response = match outcome {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": code, "message": message }
            }),
        };
        stdout.write_all(format!("{response}\n").as_bytes()).await?;
        stdout.flush().await?;
    }
    Ok(())
}

/// "name version" from the handshake's clientInfo, e.g. "claude-code 2.1.0".
fn client_name(params: &Value) -> String {
    let name = params["clientInfo"]["name"].as_str().unwrap_or("Agent");
    match params["clientInfo"]["version"].as_str() {
        Some(version) if !version.is_empty() => format!("{name} {version}"),
        _ => name.to_string(),
    }
}

fn prompt_get(params: &Value) -> Result<Value, (i64, String)> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let skill = skills::find(name).ok_or((-32602, format!("Unknown prompt: {name}")))?;
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    if let Some(missing) = skills::missing_required(skill, &args) {
        return Err((-32602, format!("Missing required argument: {missing}")));
    }
    Ok(json!({
        "description": skill.description,
        "messages": [{
            "role": "user",
            "content": { "type": "text", "text": skills::render(skill, &args) }
        }]
    }))
}

/// Removes `model` from the arguments and returns it, so it can travel as a header instead.
fn take_model(args: &mut Value) -> String {
    args.as_object_mut()
        .and_then(|map| map.remove("model"))
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

async fn call_tool(base: &str, client: &str, params: &Value) -> Result<Value, String> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let mut args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let model = take_model(&mut args);
    let identity = identity_headers(client, &model);

    match name {
        "get_agent_context" => {
            let workspace = args.get("workspace").and_then(Value::as_str).unwrap_or("default");
            let encoded = utf8_percent_encode(workspace, NON_ALPHANUMERIC);
            let mut context = http(
                "GET",
                format!("{base}/api/agent-context?workspace={encoded}"),
                None,
                identity,
            )
            .await?;
            if let Some(status) = args.get("status").and_then(Value::as_str) {
                if let Some(Value::Array(tasks)) = context.get_mut("tasks") {
                    tasks.retain(|t| t.get("status").and_then(Value::as_str) == Some(status));
                }
            }
            Ok(context)
        }
        "get_task" => {
            let id = required_str(&args, "id")?;
            http("GET", task_url(base, id), None, Vec::new()).await
        }
        "get_task_history" => {
            let id = required_str(&args, "id")?;
            http("GET", format!("{}/history", task_url(base, id)), None, Vec::new()).await
        }
        "create_task" => http("POST", format!("{base}/api/tasks"), Some(args), identity).await,
        "update_task" => {
            let id = required_str(&args, "id")?.to_string();
            let mut patch = args.clone();
            if let Some(map) = patch.as_object_mut() {
                map.remove("id");
            }
            http("PATCH", task_url(base, &id), Some(patch), identity).await
        }
        "list_workspaces" => http("GET", format!("{base}/api/workspaces"), None, Vec::new()).await,
        "create_workspace" => {
            let name = required_str(&args, "name")?;
            http("POST", format!("{base}/api/workspaces"), Some(json!({ "name": name })), identity).await
        }
        "list_run_requests" => {
            let status = args.get("status").and_then(Value::as_str).unwrap_or("pending");
            let encoded = utf8_percent_encode(status, NON_ALPHANUMERIC);
            let mut body = http("GET", format!("{base}/api/run-requests?status={encoded}"), None, Vec::new()).await?;
            let mut requests = match body.get_mut("runs").map(Value::take) {
                Some(Value::Array(runs)) => runs,
                _ => Vec::new(),
            };
            // Attach the ready-to-follow prompt so the agent can act without another lookup.
            for request in &mut requests {
                let rendered = request
                    .get("skill")
                    .and_then(Value::as_str)
                    .and_then(skills::find)
                    .map(|skill| skills::render(skill, &request["arguments"]));
                if let (Some(text), Some(map)) = (rendered, request.as_object_mut()) {
                    map.insert("prompt".into(), Value::String(text));
                }
            }
            Ok(json!({ "requests": requests }))
        }
        "complete_run_request" => {
            let id = args
                .get("id")
                .and_then(Value::as_i64)
                .ok_or_else(|| "id is required".to_string())?;
            let status = required_str(&args, "status")?;
            let mut patch = json!({ "status": status });
            if let Some(result) = args.get("result").and_then(Value::as_str) {
                patch["result"] = Value::String(result.to_string());
            }
            http("PATCH", format!("{base}/api/run-requests/{id}"), Some(patch), identity).await
        }
        _ => Err("Unknown tool".to_string()),
    }
}

fn identity_headers(client: &str, model: &str) -> Vec<(&'static str, String)> {
    vec![
        ("X-Clankban-Actor", "agent".to_string()),
        ("X-Clankban-Client", client.to_string()),
        ("X-Clankban-Model", model.to_string()),
    ]
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{key} is required"))
}

fn task_url(base: &str, id: &str) -> String {
    format!("{base}/api/tasks/{}", utf8_percent_encode(id, NON_ALPHANUMERIC))
}

/// Performs a blocking HTTP call off the async runtime. Error statuses still
/// return their JSON body, matching how the Node bridge reported them.
async fn http(
    method: &'static str,
    url: String,
    body: Option<Value>,
    headers: Vec<(&'static str, String)>,
) -> Result<Value, String> {
    tokio::task::spawn_blocking(move || {
        let mut request = ureq::request(method, &url).set("content-type", "application/json");
        for (name, value) in headers {
            request = request.set(name, &value);
        }
        let response = match body {
            Some(body) => request.send_json(body),
            None => request.call(),
        };
        let response = match response {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(e) => return Err(e.to_string()),
        };
        response.into_json::<Value>().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

const MODEL_NOTE: &str = " Pass `model` with the model you are running as (for example claude-haiku-5-5), so the board history records who made the change.";
const WORKSPACE_NOTE: &str = " `workspace` is optional and defaults to \"default\".";

fn tool_definitions() -> Value {
    json!([
        {
            "name": "get_agent_context",
            "description": format!("Get a compact summary of active work in one workspace for planning or delegation.{WORKSPACE_NOTE}"),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "status": { "type": "string", "description": "Optional status filter" },
                    "workspace": { "type": "string", "description": "Workspace id; defaults to \"default\"" }
                }
            }
        },
        {
            "name": "get_task",
            "description": "Read one work item by its stable ID.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }
        },
        {
            "name": "get_task_history",
            "description": "List the changes made to a work item, newest first, with who made each one (client and model), the fields that changed, and a summary.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }
        },
        {
            "name": "create_task",
            "description": format!("Create an epic, task, subtask, or bug fix in the local board. Use status backlog, progress, review, or done, include parentId for child tasks, and include subtasks when known.{WORKSPACE_NOTE}{MODEL_NOTE}"),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "description": { "type": "string" },
                    "type": { "type": "string" },
                    "priority": { "type": "string" },
                    "status": { "type": "string", "enum": ["backlog", "progress", "review", "done"] },
                    "due": { "type": "string" },
                    "parentId": { "type": "string" },
                    "workspace": { "type": "string" },
                    "model": { "type": "string" },
                    "subtasks": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": { "title": { "type": "string" }, "done": { "type": "boolean" } },
                            "required": ["title"]
                        }
                    }
                },
                "required": ["title"]
            }
        },
        {
            "name": "update_task",
            "description": format!("Update a local work item by ID, including its parent relationship and workspace.{MODEL_NOTE}"),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "title": { "type": "string" },
                    "description": { "type": "string" },
                    "priority": { "type": "string" },
                    "status": { "type": "string" },
                    "due": { "type": "string" },
                    "parentId": { "type": "string" },
                    "workspace": { "type": "string", "description": "Move the task to this workspace" },
                    "model": { "type": "string" }
                },
                "required": ["id"]
            }
        },
        {
            "name": "list_workspaces",
            "description": "List the workspaces on this board, with how many tasks each holds.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "create_workspace",
            "description": format!("Create a separate workspace for another project or area of work. The id is derived from the name.{MODEL_NOTE}"),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "model": { "type": "string" }
                },
                "required": ["name"]
            }
        },
        {
            "name": "list_run_requests",
            "description": "List skill runs the board has asked a connected agent to perform, oldest first. Each pending request includes a ready-to-follow prompt in its prompt field. Set a request to running before you start and to done or failed when you finish, with a short result.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["pending", "running", "done", "failed", "cancelled"],
                        "description": "Defaults to pending"
                    }
                }
            }
        },
        {
            "name": "complete_run_request",
            "description": "Update a board run request: set it to running when you start, and to done, failed, or cancelled when you finish. Include a short result summary.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer" },
                    "status": { "type": "string", "enum": ["running", "done", "failed", "cancelled"] },
                    "result": { "type": "string" },
                    "model": { "type": "string" }
                },
                "required": ["id", "status"]
            }
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        tool_definitions()
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn tool_list_includes_run_request_tools() {
        let names = names();
        assert!(names.iter().any(|n| n == "list_run_requests"));
        assert!(names.iter().any(|n| n == "complete_run_request"));
    }

    #[test]
    fn tool_list_includes_workspace_and_history_tools() {
        let names = names();
        for expected in ["list_workspaces", "create_workspace", "get_task_history"] {
            assert!(names.iter().any(|n| n == expected), "missing {expected}");
        }
    }

    #[test]
    fn model_is_taken_out_of_the_body_and_becomes_a_header() {
        let mut args = json!({ "title": "x", "model": "claude-haiku-5-5" });
        assert_eq!(take_model(&mut args), "claude-haiku-5-5");
        assert!(args.get("model").is_none());
        assert_eq!(args["title"], "x");
    }

    #[test]
    fn client_name_includes_version_when_present() {
        let params = json!({ "clientInfo": { "name": "claude-code", "version": "2.1.0" } });
        assert_eq!(client_name(&params), "claude-code 2.1.0");
        assert_eq!(client_name(&json!({})), "Agent");
    }

    #[test]
    fn prompt_get_rejects_missing_required_argument() {
        let result = prompt_get(&json!({ "name": "import_tasks", "arguments": {} }));
        assert!(matches!(result, Err((-32602, _))));
    }

    #[test]
    fn prompt_get_allows_omitted_optional_argument() {
        let result = prompt_get(&json!({ "name": "review_tasks" })).unwrap();
        let text = result["messages"][0]["content"]["text"].as_str().unwrap();
        assert!(text.contains("(not provided)"));
    }

    #[test]
    fn prompt_get_rejects_unknown_prompt() {
        let result = prompt_get(&json!({ "name": "nope" }));
        assert!(matches!(result, Err((-32602, _))));
    }
}
