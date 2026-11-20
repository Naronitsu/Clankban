//! Stdio MCP bridge for local agents. Every tool call is forwarded to the
//! local HTTP API, so the board and agents share one source of truth.

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::skills;

const PROTOCOL_VERSION: &str = "2025-06-18";

pub async fn run(api_url: String) -> std::io::Result<()> {
    let base = api_url.trim_end_matches('/').to_string();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();

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
            "initialize" => Ok(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {}, "prompts": {} },
                "serverInfo": { "name": "clankban", "version": "0.1.0" }
            })),
            "tools/list" => Ok(json!({ "tools": tool_definitions() })),
            "tools/call" => {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                match call_tool(&base, &params).await {
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

async fn call_tool(base: &str, params: &Value) -> Result<Value, String> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));

    match name {
        "get_agent_context" => {
            let mut context = http("GET", format!("{base}/api/agent-context"), None).await?;
            if let Some(status) = args.get("status").and_then(Value::as_str) {
                if let Some(Value::Array(tasks)) = context.get_mut("tasks") {
                    tasks.retain(|t| t.get("status").and_then(Value::as_str) == Some(status));
                }
            }
            Ok(context)
        }
        "get_task" => {
            let id = required_str(&args, "id")?;
            http("GET", task_url(base, id), None).await
        }
        "create_task" => http("POST", format!("{base}/api/tasks"), Some(args)).await,
        "update_task" => {
            let id = required_str(&args, "id")?.to_string();
            let mut patch = args.clone();
            if let Some(map) = patch.as_object_mut() {
                map.remove("id");
            }
            http("PATCH", task_url(base, &id), Some(patch)).await
        }
        "list_run_requests" => {
            let status = args.get("status").and_then(Value::as_str).unwrap_or("pending");
            let encoded = utf8_percent_encode(status, NON_ALPHANUMERIC);
            let mut body = http("GET", format!("{base}/api/run-requests?status={encoded}"), None).await?;
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
            http("PATCH", format!("{base}/api/run-requests/{id}"), Some(patch)).await
        }
        _ => Err("Unknown tool".to_string()),
    }
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
async fn http(method: &'static str, url: String, body: Option<Value>) -> Result<Value, String> {
    tokio::task::spawn_blocking(move || {
        let request = ureq::request(method, &url).set("content-type", "application/json");
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

fn tool_definitions() -> Value {
    json!([
        {
            "name": "get_agent_context",
            "description": "Get a compact summary of active Clankban work for planning or delegation.",
            "inputSchema": {
                "type": "object",
                "properties": { "status": { "type": "string", "description": "Optional status filter" } }
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
            "name": "create_task",
            "description": "Create an epic, task, subtask, or bug fix in the local board. Use status backlog, progress, review, or done, include parentId for child tasks, and include subtasks when known.",
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
            "description": "Update a local work item by ID, including its parent relationship.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "title": { "type": "string" },
                    "description": { "type": "string" },
                    "priority": { "type": "string" },
                    "status": { "type": "string" },
                    "due": { "type": "string" },
                    "parentId": { "type": "string" }
                },
                "required": ["id"]
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
                    "result": { "type": "string" }
                },
                "required": ["id", "status"]
            }
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_list_includes_run_request_tools() {
        let tools = tool_definitions();
        let names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"list_run_requests"));
        assert!(names.contains(&"complete_run_request"));
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
