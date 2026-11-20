//! Stdio MCP bridge for local agents. Every tool call is forwarded to the
//! local HTTP API, so the board and agents share one source of truth.

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

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
            "prompts/list" => Ok(json!({ "prompts": prompt_list() })),
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

/// Agent workflows shipped inside the binary. Clients list them with
/// `prompts/list` and fetch a filled-in version with `prompts/get`.
struct PromptArg {
    name: &'static str,
    description: &'static str,
    required: bool,
}

struct Prompt {
    name: &'static str,
    description: &'static str,
    args: &'static [PromptArg],
    template: &'static str,
}

const PROMPTS: &[Prompt] = &[
    Prompt {
        name: "import_tasks",
        description: "Import work items from notes or a list into the board. Shows an inventory and waits for approval before creating anything.",
        args: &[PromptArg {
            name: "source",
            description: "Pasted notes, a list, or file contents to import",
            required: true,
        }],
        template: include_str!("../skills/import_tasks.md"),
    },
    Prompt {
        name: "update_tasks",
        description: "Change existing items. Reads each item first, shows the planned changes, and applies them after approval.",
        args: &[PromptArg {
            name: "changes",
            description: "Which items to change and how",
            required: true,
        }],
        template: include_str!("../skills/update_tasks.md"),
    },
    Prompt {
        name: "review_tasks",
        description: "Read-only review of board health: overdue, stuck, orphaned, and duplicate items.",
        args: &[PromptArg {
            name: "scope",
            description: "Optional status or area to focus on",
            required: false,
        }],
        template: include_str!("../skills/review_tasks.md"),
    },
];

fn prompt_list() -> Value {
    Value::Array(
        PROMPTS
            .iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "description": p.description,
                    "arguments": p.args.iter().map(|a| json!({
                        "name": a.name,
                        "description": a.description,
                        "required": a.required,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

fn non_empty_arg<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn render_prompt(prompt: &Prompt, args: &Value) -> String {
    let mut text = prompt.template.to_string();
    for arg in prompt.args {
        let value = non_empty_arg(args, arg.name).unwrap_or("(not provided)");
        text = text.replace(&format!("{{{{{}}}}}", arg.name), value);
    }
    text
}

fn prompt_get(params: &Value) -> Result<Value, (i64, String)> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let prompt = PROMPTS
        .iter()
        .find(|p| p.name == name)
        .ok_or((-32602, format!("Unknown prompt: {name}")))?;
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    if let Some(missing) = prompt
        .args
        .iter()
        .find(|a| a.required && non_empty_arg(&args, a.name).is_none())
    {
        return Err((-32602, format!("Missing required argument: {}", missing.name)));
    }
    Ok(json!({
        "description": prompt.description,
        "messages": [{
            "role": "user",
            "content": { "type": "text", "text": render_prompt(prompt, &args) }
        }]
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_the_three_workflows() {
        let names: Vec<&str> = PROMPTS.iter().map(|p| p.name).collect();
        assert_eq!(names, ["import_tasks", "update_tasks", "review_tasks"]);
    }

    #[test]
    fn every_template_renders_without_placeholders_left() {
        for prompt in PROMPTS {
            let mut args = serde_json::Map::new();
            for arg in prompt.args {
                args.insert(arg.name.to_string(), json!("value"));
            }
            let text = render_prompt(prompt, &Value::Object(args));
            assert!(!text.contains("{{"), "{} has an unfilled placeholder", prompt.name);
        }
    }

    #[test]
    fn missing_required_argument_is_rejected() {
        let result = prompt_get(&json!({ "name": "import_tasks", "arguments": {} }));
        assert!(matches!(result, Err((-32602, _))));
    }

    #[test]
    fn optional_argument_may_be_omitted() {
        let result = prompt_get(&json!({ "name": "review_tasks" })).unwrap();
        let text = result["messages"][0]["content"]["text"].as_str().unwrap();
        assert!(text.contains("(not provided)"));
    }

    #[test]
    fn unknown_prompt_is_rejected() {
        let result = prompt_get(&json!({ "name": "nope" }));
        assert!(matches!(result, Err((-32602, _))));
    }
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
        }
    ])
}
