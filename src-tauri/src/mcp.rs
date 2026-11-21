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

/// The project folder a tool means: the `path` argument, or the folder the bridge was started in.
fn project_folder(args: &Value) -> Result<String, String> {
    match args.get("path").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty()) {
        Some(path) => Ok(path.to_string()),
        None => Ok(std::env::current_dir().map_err(|e| e.to_string())?.to_string_lossy().into_owned()),
    }
}

/// The board a tool works on: the `workspace` argument when given, otherwise the board connected
/// to this agent's project folder. Never silently the default board: when no board matches, the
/// error says what to do and nothing is written.
async fn resolve_board(args: &Value, base: &str) -> Result<String, String> {
    if let Some(workspace) = args.get("workspace").and_then(Value::as_str).map(str::trim).filter(|w| !w.is_empty()) {
        return Ok(workspace.to_string());
    }
    let folder = project_folder(&Value::Null)?;
    let encoded = utf8_percent_encode(&folder, NON_ALPHANUMERIC);
    let found = http("GET", format!("{base}/api/workspace-match?folder={encoded}"), None, Vec::new()).await?;
    match found.get("workspace").and_then(|w| w.get("id")).and_then(Value::as_str) {
        Some(id) => Ok(id.to_string()),
        None if found.get("pending").map(|p| !p.is_null()).unwrap_or(false) => Err(format!(
            "A board for this folder ({folder}) is waiting for a person to accept the folder in its Docs tab. Nothing was written. Ask the person to accept it, then try again."
        )),
        None => Err(format!(
            "No board is connected to this folder ({folder}). Nothing was written. Call resolve_workspace to see the boards. Pass `workspace` to use one of them, or ask for a board for this folder with create_workspace and this folder's path. A person approves the request on the board first."
        )),
    }
}

async fn call_tool(base: &str, client: &str, params: &Value) -> Result<Value, String> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let mut args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let model = take_model(&mut args);
    let identity = identity_headers(client, &model);

    match name {
        "get_agent_context" => {
            let workspace = resolve_board(&args, base).await?;
            let encoded = utf8_percent_encode(&workspace, NON_ALPHANUMERIC);
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
            let id = required_str(&args, "id")?.to_string();
            let mut task = http("GET", task_url(base, &id), None, Vec::new()).await?;
            let comments = http("GET", format!("{}/comments", task_url(base, &id)), None, Vec::new()).await?;
            task["comments"] = comments.get("comments").cloned().unwrap_or_else(|| json!([]));
            Ok(task)
        }
        "add_comment" => {
            let id = required_str(&args, "id")?.to_string();
            let body = required_str(&args, "body")?.to_string();
            http("POST", format!("{}/comments", task_url(base, &id)), Some(json!({ "body": body })), identity).await
        }
        "get_task_history" => {
            let id = required_str(&args, "id")?;
            http("GET", format!("{}/history", task_url(base, id)), None, Vec::new()).await
        }
        "create_task" => {
            let workspace = resolve_board(&args, base).await?;
            let mut body = args.clone();
            body["workspace"] = json!(workspace);
            http("POST", format!("{base}/api/tasks"), Some(body), identity).await
        }
        "update_task" => {
            let id = required_str(&args, "id")?.to_string();
            let mut patch = args.clone();
            if let Some(map) = patch.as_object_mut() {
                map.remove("id");
            }
            http("PATCH", task_url(base, &id), Some(patch), identity).await
        }
        "list_workspaces" => http("GET", format!("{base}/api/workspaces"), None, Vec::new()).await,
        "suggest_project_folder" => {
            let workspace = resolve_board(&args, base).await?;
            // Without a path, suggest the folder the bridge was started in: the project the agent is working on.
            let folder = match args.get("path").and_then(Value::as_str) {
                Some(path) => path.to_string(),
                None => std::env::current_dir()
                    .map_err(|e| e.to_string())?
                    .to_string_lossy()
                    .into_owned(),
            };
            let encoded = utf8_percent_encode(&workspace, NON_ALPHANUMERIC);
            http(
                "PUT",
                format!("{base}/api/workspaces/{encoded}/suggestion"),
                Some(json!({ "root": folder })),
                identity,
            )
            .await
        }
        "list_project_docs" => {
            let workspace = resolve_board(&args, base).await?;
            let encoded = utf8_percent_encode(&workspace, NON_ALPHANUMERIC);
            http("GET", format!("{base}/api/workspaces/{encoded}/docs"), None, Vec::new()).await
        }
        "read_project_doc" => {
            let path = required_str(&args, "path")?;
            let workspace = resolve_board(&args, base).await?;
            let encoded_workspace = utf8_percent_encode(&workspace, NON_ALPHANUMERIC);
            let encoded_path = utf8_percent_encode(path, NON_ALPHANUMERIC);
            http(
                "GET",
                format!("{base}/api/workspaces/{encoded_workspace}/docs/read?path={encoded_path}"),
                None,
                Vec::new(),
            )
            .await
        }
        "resolve_workspace" => {
            let folder = project_folder(&args)?;
            let encoded = utf8_percent_encode(&folder, NON_ALPHANUMERIC);
            http("GET", format!("{base}/api/workspace-match?folder={encoded}"), None, Vec::new()).await
        }
        "create_workspace" => {
            // A folder makes a board for that project. A folder that already has a board returns that board.
            let name = args.get("name").and_then(Value::as_str).unwrap_or("");
            let path = args.get("path").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty());
            if path.is_none() && name.trim().is_empty() {
                return Err("name is required".to_string());
            }
            let mut body = json!({ "name": name });
            if let Some(path) = path {
                body["path"] = json!(path);
            }
            // Files a request for a person to approve on the board. No board is created here.
            http("POST", format!("{base}/api/board-requests"), Some(body), identity).await
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
const WORKSPACE_NOTE: &str = " `workspace` is optional: when it is left out, the board connected to this agent's project folder is used. If no board matches, nothing is written and the error says what to do. Call resolve_workspace to see the boards.";

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
                    "docs": { "type": "array", "items": { "type": "string" }, "description": "Project Markdown files this task relates to, relative to the project folder, for example docs/TASKS.md" },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "Labels such as important or on hold. Replaces the list." },
                    "suggestedModel": { "type": "string", "description": "A model suggested for this task, for example claude-haiku-5-5. A suggestion only; the agent may choose another. Empty clears it." },
                    "blockedBy": { "type": "array", "items": { "type": "string" }, "description": "Task ids that must be finished before this task can start. Replaces the list." },
                    "relatesTo": { "type": "array", "items": { "type": "string" }, "description": "Task ids this task relates to, with no order between them. Replaces the list." },
                    "standalone": { "type": "boolean", "description": "False when this task cannot be picked up on its own; standaloneNote says why." },
                    "standaloneNote": { "type": "string", "description": "Why the task cannot be picked up on its own. Empty when it can." },
                    "links": { "type": "array", "items": { "type": "string" }, "description": "Related links, http or https only. Replaces the list." },
                    "sprint": { "type": ["integer", "null"], "description": "The sprint this task is in: a sprint id from the board, or null to take it out of its sprint." },
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
                    "docs": { "type": "array", "items": { "type": "string" }, "description": "Project Markdown files this task relates to, relative to the project folder, for example docs/TASKS.md" },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "Labels such as important or on hold. Replaces the list." },
                    "suggestedModel": { "type": "string", "description": "A model suggested for this task, for example claude-haiku-5-5. A suggestion only; the agent may choose another. Empty clears it." },
                    "blockedBy": { "type": "array", "items": { "type": "string" }, "description": "Task ids that must be finished before this task can start. Replaces the list." },
                    "relatesTo": { "type": "array", "items": { "type": "string" }, "description": "Task ids this task relates to, with no order between them. Replaces the list." },
                    "standalone": { "type": "boolean", "description": "False when this task cannot be picked up on its own; standaloneNote says why." },
                    "standaloneNote": { "type": "string", "description": "Why the task cannot be picked up on its own. Empty when it can." },
                    "links": { "type": "array", "items": { "type": "string" }, "description": "Related links, http or https only. Replaces the list." },
                    "sprint": { "type": ["integer", "null"], "description": "The sprint this task is in: a sprint id from the board, or null to take it out of its sprint." },
                    "workspace": { "type": "string", "description": "Move the task to this workspace" },
                    "subtasks": {
                        "type": "array",
                        "description": "Replaces the whole checklist; send every item, with its done flag",
                        "items": {
                            "type": "object",
                            "properties": { "title": { "type": "string" }, "done": { "type": "boolean" } },
                            "required": ["title"]
                        }
                    },
                    "model": { "type": "string" }
                },
                "required": ["id"]
            }
        },
        {
            "name": "suggest_project_folder",
            "description": "Suggest the project folder for a workspace, so the board can show its documents. Nothing connects until a person accepts the suggestion in the Docs tab. Without a path, suggests the folder this bridge was started in.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path to the project folder. Optional." },
                    "workspace": { "type": "string", "description": "Defaults to \"default\"" }
                }
            }
        },
        {
            "name": "list_project_docs",
            "description": "List the Markdown and HTML files in the project folder connected to a workspace. Paths are relative to the project folder. Read-only.",
            "inputSchema": {
                "type": "object",
                "properties": { "workspace": { "type": "string", "description": "Defaults to \"default\"" } }
            }
        },
        {
            "name": "read_project_doc",
            "description": "Read one Markdown or HTML file from the project folder connected to a workspace. Pass a path from list_project_docs. Read-only.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path, for example docs/ARCHITECTURE.md" },
                    "workspace": { "type": "string", "description": "Defaults to \"default\"" }
                },
                "required": ["path"]
            }
        },
        {
            "name": "add_comment",
            "description": format!("Add a comment to a task, for people and agents to read. Use it for progress notes, questions, and decisions.{MODEL_NOTE}"),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Task id, for example AT-40" },
                    "body": { "type": "string", "description": "The comment text. Up to 4000 characters." },
                    "model": { "type": "string" }
                },
                "required": ["id", "body"]
            }
        },
        {
            "name": "list_workspaces",
            "description": "List the workspaces on this board, with how many tasks each holds.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "resolve_workspace",
            "description": "Find the board for a project folder. Call this first when unsure which board to use. Without a path, uses the folder this bridge was started in. Returns the matching board, or null with the list of boards.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Project folder. Optional; defaults to the folder the bridge was started in." }
                }
            }
        },
        {
            "name": "create_workspace",
            "description": format!("Ask for a new board. Nothing is created until a person approves the request on the board. Pass `path` (the project folder) for a board for that folder: if the folder already has a board, that board is returned instead. Approved boards suggest the folder; a person accepts it in the Docs tab.{MODEL_NOTE}"),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Board name. Optional when a path is given; defaults to the folder name." },
                    "path": { "type": "string", "description": "Project folder for this board." },
                    "model": { "type": "string" }
                }
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
        for expected in ["list_workspaces", "add_comment", "resolve_workspace", "create_workspace", "get_task_history", "list_project_docs", "read_project_doc"] {
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
