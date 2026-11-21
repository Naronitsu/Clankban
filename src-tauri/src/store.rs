//! SQLite-backed store: tasks, workspaces, change history, and the agent run queue.
//! Keeps the schema, seed data, and normalisation rules of the original Node `server.js`,
//! so existing `clankban.sqlite` files open unchanged.

use chrono::{Duration, NaiveDate, SecondsFormat, Utc};
use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

pub const DB_FILE: &str = "clankban.sqlite";
pub const SCHEMA_VERSION: &str = "1.1";
pub const DEFAULT_WORKSPACE: &str = "default";

const SELECT_COLUMNS: &str = "id, title, description, type, priority, status, due, created, subtasks, updated, parent_id, workspace_id, docs, tags, suggested_model, blocked_by, relates_to, standalone, standalone_note, links, sprint_id";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Subtask {
    pub title: String,
    #[serde(default)]
    pub done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub priority: String,
    pub status: String,
    pub due: String,
    pub created: String,
    pub updated: String,
    pub subtasks: Vec<Subtask>,
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    pub workspace: String,
    /// Project documents this task relates to, as paths relative to the project folder.
    pub docs: Vec<String>,
    /// Labels such as "important" or "on hold". Lowercase, no duplicates.
    pub tags: Vec<String>,
    /// A model the person or an agent suggests for this task. Advisory only; empty when none.
    #[serde(rename = "suggestedModel")]
    pub suggested_model: String,
    /// Tasks that must be finished before this one can start.
    #[serde(rename = "blockedBy")]
    pub blocked_by: Vec<String>,
    /// Tasks related to this one, with no order between them.
    #[serde(rename = "relatesTo")]
    pub relates_to: Vec<String>,
    /// False when this task cannot be picked up on its own. `standalone_note` says why.
    pub standalone: bool,
    #[serde(rename = "standaloneNote")]
    pub standalone_note: String,
    /// Related links. Only http and https addresses are stored.
    pub links: Vec<String>,
    /// The sprint this task is in, or none.
    pub sprint: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub created: String,
    pub tasks: i64,
    /// Absolute path of the project folder, when one is set. Read-only for the board.
    #[serde(rename = "projectRoot")]
    pub project_root: Option<String>,
    /// A folder an agent proposed. Only a person can accept it, which connects it.
    #[serde(rename = "suggestedRoot")]
    pub suggested_root: Option<String>,
    /// Who proposed the folder, for example "claude-code 2.1.0 (claude-haiku-5-5)".
    #[serde(rename = "suggestedBy")]
    pub suggested_by: Option<String>,
    /// The default length of a new sprint on this board, in days.
    #[serde(rename = "sprintDays")]
    pub sprint_days: i64,
}

/// Who made a change. `kind` is "human" (the board), "agent" (an MCP client), or "api".
#[derive(Debug, Clone, PartialEq)]
pub struct Actor {
    pub kind: String,
    pub name: String,
    pub model: String,
}

impl Actor {
    #[cfg(test)]
    pub fn human() -> Self {
        Actor { kind: "human".into(), name: "Board".into(), model: String::new() }
    }


    /// e.g. "Claude Code (claude-haiku-5-5)"
    pub fn label(&self) -> String {
        if self.model.is_empty() {
            self.name.clone()
        } else {
            format!("{} ({})", self.name, self.model)
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct HistoryEntry {
    pub id: i64,
    #[serde(rename = "taskId")]
    pub task_id: String,
    pub at: String,
    pub action: String,
    #[serde(rename = "actorKind")]
    pub actor_kind: String,
    #[serde(rename = "actorName")]
    pub actor_name: String,
    #[serde(rename = "actorModel")]
    pub actor_model: String,
    pub changes: Value,
    pub summary: String,
}

#[derive(Debug)]
pub enum ApiError {
    BadRequest(String),
    NotFound,
    Conflict(String),
    Internal(String),
}

impl From<rusqlite::Error> for ApiError {
    fn from(error: rusqlite::Error) -> Self {
        ApiError::Internal(error.to_string())
    }
}

pub struct Store {
    conn: Mutex<Connection>,
}

/// A request from the board for a connected agent to run a skill.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunRequest {
    pub id: i64,
    pub skill: String,
    pub arguments: Value,
    pub status: String,
    pub result: String,
    pub created: String,
    pub updated: String,
}

pub const RUN_STATUSES: [&str; 5] = ["pending", "running", "done", "failed", "cancelled"];

const RUN_COLUMNS: &str = "id, skill, arguments, status, result, created, updated";
const HISTORY_COLUMNS: &str = "id, task_id, at, action, actor_kind, actor_name, actor_model, changes, summary";

fn row_to_run(row: &Row) -> rusqlite::Result<RunRequest> {
    let arguments_raw: String = row.get::<_, Option<String>>(2)?.unwrap_or_else(|| "{}".into());
    Ok(RunRequest {
        id: row.get(0)?,
        skill: row.get(1)?,
        arguments: serde_json::from_str(&arguments_raw).unwrap_or_else(|_| json!({})),
        status: row.get(3)?,
        result: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
        created: row.get(5)?,
        updated: row.get(6)?,
    })
}

fn read_run(conn: &Connection, id: i64) -> Result<Option<RunRequest>, ApiError> {
    let run = conn
        .query_row(
            &format!("SELECT {RUN_COLUMNS} FROM run_requests WHERE id = ?1"),
            [id],
            row_to_run,
        )
        .optional()?;
    Ok(run)
}

/// The filters a view can save. Anything else in a request is dropped.
const VIEW_FILTER_KEYS: &[&str] = &["priority", "type", "status", "tag", "sprint"];

/// A saved layout for one board: a list or a board, with filters already applied.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct View {
    pub id: i64,
    pub workspace: String,
    pub name: String,
    /// `board` or `list`.
    pub layout: String,
    /// Filter values by name: priority, type, status, tag, and sprint (a sprint id, or "none").
    pub filters: std::collections::BTreeMap<String, String>,
}

const VIEW_COLUMNS: &str = "id, workspace_id, name, layout, filters";

fn row_to_view(row: &Row) -> rusqlite::Result<View> {
    let raw: String = row.get::<_, Option<String>>(4)?.unwrap_or_else(|| "{}".into());
    Ok(View {
        id: row.get(0)?,
        workspace: row.get(1)?,
        name: row.get(2)?,
        layout: row.get(3)?,
        filters: serde_json::from_str(&raw).unwrap_or_default(),
    })
}

/// What agents may do on the board, and the default for each. A person changes these in Settings.
pub const AGENT_SETTINGS: &[(&str, bool)] = &[
    ("agents_plan_sprints", true),
    ("agents_run_sprints", true),
    ("agents_comment", true),
];

/// The longest view name, in characters. Names show in the sidebar, so they stay short.
pub const VIEW_NAME_MAX: usize = 20;

/// The filters a request asks to save, keeping only the known keys with non-empty text values.
fn view_filters(input: Option<&Value>) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    if let Some(Value::Object(map)) = input {
        for key in VIEW_FILTER_KEYS {
            if let Some(value) = map.get(*key).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()) {
                out.insert((*key).to_string(), value.to_string());
            }
        }
    }
    out
}

fn view_name(text: Option<&str>) -> Result<String, ApiError> {
    let name = text.unwrap_or("").trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("view name is required".into()));
    }
    if name.chars().count() > VIEW_NAME_MAX {
        return Err(ApiError::BadRequest(format!("view names are limited to {VIEW_NAME_MAX} characters")));
    }
    Ok(name.to_string())
}

fn view_layout(text: Option<&str>) -> Result<String, ApiError> {
    match text.map(str::trim) {
        Some(layout @ ("board" | "list")) => Ok(layout.to_string()),
        _ => Err(ApiError::BadRequest("a view shows a board or a list".into())),
    }
}

/// The default and the largest sprint length, in days.
pub const DEFAULT_SPRINT_DAYS: i64 = 14;
const MAX_SPRINT_DAYS: i64 = 90;

/// A time-boxed run of work on one board. One sprint at a time is running in a board.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Sprint {
    pub id: i64,
    pub workspace: String,
    pub name: String,
    #[serde(rename = "startDate")]
    pub start_date: String,
    #[serde(rename = "endDate")]
    pub end_date: String,
    /// `planned`, `active` (running), or `closed`.
    pub status: String,
    /// How many tasks are in this sprint.
    pub tasks: i64,
}

const SPRINT_COLUMNS: &str =
    "s.id, s.workspace_id, s.name, s.start_date, s.end_date, s.status, (SELECT COUNT(*) FROM tasks t WHERE t.sprint_id = s.id)";

fn row_to_sprint(row: &Row) -> rusqlite::Result<Sprint> {
    Ok(Sprint {
        id: row.get(0)?,
        workspace: row.get(1)?,
        name: row.get(2)?,
        start_date: row.get(3)?,
        end_date: row.get(4)?,
        status: row.get(5)?,
        tasks: row.get(6)?,
    })
}

fn read_sprint(conn: &Connection, id: i64) -> Result<Option<Sprint>, ApiError> {
    Ok(conn
        .query_row(&format!("SELECT {SPRINT_COLUMNS} FROM sprints s WHERE s.id = ?1"), [id], row_to_sprint)
        .optional()?)
}

/// A calendar day written as YYYY-MM-DD.
fn parse_day(text: &str) -> Result<NaiveDate, ApiError> {
    NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d")
        .map_err(|_| ApiError::BadRequest(format!("dates must look like 2026-12-01, not {text}")))
}

/// The last day of a sprint. A typed end date wins; otherwise the typed length, otherwise the board's default.
fn resolve_sprint_end(
    conn: &Connection,
    workspace: &str,
    start: NaiveDate,
    length_days: Option<i64>,
    end_text: Option<&str>,
) -> Result<NaiveDate, ApiError> {
    if let Some(text) = end_text.map(str::trim).filter(|t| !t.is_empty()) {
        let end = parse_day(text)?;
        if end < start {
            return Err(ApiError::BadRequest("the end date is before the start date".into()));
        }
        if (end - start).num_days() + 1 > MAX_SPRINT_DAYS {
            return Err(ApiError::BadRequest(format!("sprints can be at most {MAX_SPRINT_DAYS} days")));
        }
        return Ok(end);
    }
    let days = match length_days {
        Some(days) => days,
        None => conn
            .query_row("SELECT sprint_days FROM workspaces WHERE id = ?1", [workspace], |row| row.get::<_, i64>(0))
            .optional()?
            .unwrap_or(DEFAULT_SPRINT_DAYS),
    };
    if !(1..=MAX_SPRINT_DAYS).contains(&days) {
        return Err(ApiError::BadRequest(format!("sprint length must be between 1 and {MAX_SPRINT_DAYS} days")));
    }
    Ok(start + Duration::days(days - 1))
}

/// A note on a task, from a person or an agent.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Comment {
    pub id: i64,
    #[serde(rename = "taskId")]
    pub task_id: String,
    pub at: String,
    #[serde(rename = "actorKind")]
    pub actor_kind: String,
    #[serde(rename = "actorName")]
    pub actor_name: String,
    #[serde(rename = "actorModel")]
    pub actor_model: String,
    pub body: String,
}

const COMMENT_COLUMNS: &str = "id, task_id, at, actor_kind, actor_name, actor_model, body";

fn row_to_comment(row: &Row) -> rusqlite::Result<Comment> {
    Ok(Comment {
        id: row.get(0)?,
        task_id: row.get(1)?,
        at: row.get(2)?,
        actor_kind: row.get(3)?,
        actor_name: row.get(4)?,
        actor_model: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
        body: row.get(6)?,
    })
}

/// A request from an agent for a new board. Pending until a person approves or rejects it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BoardRequest {
    pub id: i64,
    pub name: String,
    pub folder: Option<String>,
    #[serde(rename = "requestedBy")]
    pub requested_by: String,
    /// `pending`, `approved`, or `rejected`.
    pub status: String,
    pub created: String,
    #[serde(rename = "workspaceId")]
    pub workspace_id: Option<String>,
}

/// What asking for a board produced: a board that already exists for the folder, or a request.
#[derive(Debug, Clone)]
pub enum BoardRequestResult {
    Existing(Workspace),
    Pending { request: BoardRequest, is_new: bool },
}

const BOARD_REQUEST_COLUMNS: &str = "id, name, folder, requested_by, status, created, workspace_id";

fn row_to_board_request(row: &Row) -> rusqlite::Result<BoardRequest> {
    Ok(BoardRequest {
        id: row.get(0)?,
        name: row.get(1)?,
        folder: row.get::<_, Option<String>>(2)?.filter(|s| !s.is_empty()),
        requested_by: row.get(3)?,
        status: row.get(4)?,
        created: row.get(5)?,
        workspace_id: row.get::<_, Option<String>>(6)?.filter(|s| !s.is_empty()),
    })
}

fn read_board_request(conn: &Connection, id: i64) -> Result<Option<BoardRequest>, ApiError> {
    Ok(conn
        .query_row(
            &format!("SELECT {BOARD_REQUEST_COLUMNS} FROM board_requests WHERE id = ?1"),
            [id],
            row_to_board_request,
        )
        .optional()?)
}

/// Folder paths compare case-insensitively on Windows, with either slash and no trailing slash.
fn normalise_folder(path: &str) -> String {
    let mut text = path.trim().replace('/', "\\").to_lowercase();
    while text.len() > 3 && text.ends_with('\\') {
        text.pop();
    }
    text
}

/// True when `folder` is `root` itself or sits somewhere inside it.
fn folder_is_within(folder: &str, root: &str) -> bool {
    let folder = normalise_folder(folder);
    let root = normalise_folder(root);
    folder == root || folder.starts_with(&format!("{root}\\"))
}

/// A history entry plus the current title of the task it changed, for the Recent view.
#[derive(Debug, Clone, Serialize)]
pub struct RecentHistoryEntry {
    #[serde(flatten)]
    pub entry: HistoryEntry,
    #[serde(rename = "taskTitle")]
    pub task_title: String,
    #[serde(rename = "taskWorkspace")]
    pub task_workspace: String,
}

fn row_to_history(row: &Row) -> rusqlite::Result<HistoryEntry> {
    let changes_raw: String = row.get::<_, Option<String>>(7)?.unwrap_or_else(|| "{}".into());
    Ok(HistoryEntry {
        id: row.get(0)?,
        task_id: row.get(1)?,
        at: row.get(2)?,
        action: row.get(3)?,
        actor_kind: row.get(4)?,
        actor_name: row.get(5)?,
        actor_model: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
        changes: serde_json::from_str(&changes_raw).unwrap_or_else(|_| json!({})),
        summary: row.get::<_, Option<String>>(8)?.unwrap_or_default(),
    })
}

/// (id, title, description, type, priority, status, due, created, subtasks JSON)
const SEED: [(&str, &str, &str, &str, &str, &str, &str, &str, &str); 8] = [
    ("AT-101", "Define project scope & success metrics", "Align on the first release boundary and document how we will know it is working.", "epic", "high", "backlog", "2026-11-24", "2026-11-17", r#"[{"title":"Draft the one-page brief","done":true},{"title":"Review with stakeholders","done":false}]"#),
    ("AT-104", "Design the agent context panel", "Give local agents a clean, inspectable view of board state, task relationships, and recent changes.", "task", "high", "progress", "2026-11-22", "2026-11-18", r#"[{"title":"Map the JSON shape","done":true},{"title":"Add copy action","done":false},{"title":"Document the read contract","done":false}]"#),
    ("AT-107", "Build drag-and-drop board view", "Make work feel tangible: move cards between statuses and preserve the state locally.", "task", "medium", "progress", "2026-11-28", "2026-11-18", "[]"),
    ("AT-109", "Fix empty state on new workspace", "New workspaces should explain the next step instead of showing a blank canvas.", "bug", "high", "progress", "2026-11-21", "2026-11-19", "[]"),
    ("AT-112", "Write local API adapter notes", "Capture the endpoints an agent bridge will need to read and mutate the board.", "subtask", "low", "review", "2026-11-30", "2026-11-19", "[]"),
    ("AT-115", "Choose a stable task identifier format", "Document how identifiers are generated and how they should be referenced in agent output.", "task", "medium", "review", "2026-12-01", "2026-11-19", "[]"),
    ("AT-098", "Set up local persistence", "Keep the board useful between sessions with browser-local storage.", "task", "medium", "done", "2026-11-18", "2026-11-14", "[]"),
    ("AT-093", "Create visual language tokens", "Establish color, type, spacing, and status patterns for the workspace.", "task", "low", "done", "2026-11-16", "2026-11-12", "[]"),
];

fn parent_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(?:Parent|Epic):\s*(AT-\d+)").expect("valid regex"))
}

pub fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn today() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

/// Mirrors the JS rule: `body.status === 'todo' ? 'backlog' : body.status || 'backlog'`.
fn normalize_status(raw: Option<String>) -> String {
    match raw {
        Some(s) if s != "todo" => s,
        _ => "backlog".to_string(),
    }
}

/// Legacy parent inference from description text such as `Parent: AT-131`.
fn infer_parent(description: &str) -> Option<String> {
    parent_regex()
        .captures(description)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

fn row_to_task(row: &Row) -> rusqlite::Result<Task> {
    let subtasks_raw: String = row.get::<_, Option<String>>(8)?.unwrap_or_else(|| "[]".into());
    let subtasks: Vec<Subtask> = serde_json::from_str(&subtasks_raw).unwrap_or_default();
    let description: String = row.get::<_, Option<String>>(2)?.unwrap_or_default();
    let stored_parent: Option<String> = row.get::<_, Option<String>>(10)?.filter(|s| !s.is_empty());
    let parent_id = stored_parent.or_else(|| infer_parent(&description));

    Ok(Task {
        id: row.get(0)?,
        title: row.get(1)?,
        description,
        kind: row.get(3)?,
        priority: row.get(4)?,
        status: normalize_status(row.get::<_, Option<String>>(5)?),
        due: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
        created: row.get(7)?,
        updated: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
        subtasks,
        parent_id,
        workspace: row
            .get::<_, Option<String>>(11)?
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_WORKSPACE.into()),
        docs: row
            .get::<_, Option<String>>(12)?
            .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
            .unwrap_or_default(),
        tags: row
            .get::<_, Option<String>>(13)?
            .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
            .unwrap_or_default(),
        suggested_model: row.get::<_, Option<String>>(14)?.unwrap_or_default(),
        blocked_by: json_list(row.get::<_, Option<String>>(15)?),
        relates_to: json_list(row.get::<_, Option<String>>(16)?),
        standalone: row.get::<_, Option<i64>>(17)?.unwrap_or(1) != 0,
        standalone_note: row.get::<_, Option<String>>(18)?.unwrap_or_default(),
        links: json_list(row.get::<_, Option<String>>(19)?),
        sprint: row.get::<_, Option<i64>>(20)?,
    })
}

/// A JSON array stored in a text column, read back as a list. Bad or missing data reads as empty.
fn json_list(raw: Option<String>) -> Vec<String> {
    raw.and_then(|r| serde_json::from_str::<Vec<String>>(&r).ok()).unwrap_or_default()
}

/// Links must be plain http or https addresses. Anything else is refused, so a link can never
/// open a file, a script, or another scheme.
fn normalise_links(merged: &Map<String, Value>) -> Result<Vec<String>, ApiError> {
    let Some(Value::Array(items)) = merged.get("links") else {
        return Ok(Vec::new());
    };
    let mut links: Vec<String> = Vec::new();
    for item in items.iter().filter_map(Value::as_str) {
        let link = item.trim();
        if link.is_empty() {
            continue;
        }
        let lower = link.to_ascii_lowercase();
        let web = lower.starts_with("http://") || lower.starts_with("https://");
        if !web || link.len() > 2000 || link.contains(char::is_whitespace) {
            return Err(ApiError::BadRequest(format!("links must be http or https addresses: {link}")));
        }
        if !links.iter().any(|known| known == link) {
            links.push(link.to_string());
        }
    }
    Ok(links)
}

/// The JSON text for the relationship and link columns.
fn relation_columns(task: &Task) -> Result<(String, String, String), ApiError> {
    let encode = |list: &Vec<String>| serde_json::to_string(list).map_err(|e| ApiError::Internal(e.to_string()));
    Ok((encode(&task.blocked_by)?, encode(&task.relates_to)?, encode(&task.links)?))
}

/// Blocking and relating links must point at tasks that exist.
fn check_references(conn: &Connection, task: &Task) -> Result<(), ApiError> {
    for other in task.blocked_by.iter().chain(task.relates_to.iter()) {
        if read_one(conn, other)?.is_none() {
            return Err(ApiError::BadRequest(format!("Unknown task: {other}")));
        }
    }
    if let Some(sprint) = task.sprint {
        let board: Option<String> = conn
            .query_row("SELECT workspace_id FROM sprints WHERE id = ?1", [sprint], |row| row.get(0))
            .optional()?;
        match board {
            Some(board) if board == task.workspace => {}
            Some(_) => return Err(ApiError::BadRequest("that sprint belongs to another board".into())),
            None => return Err(ApiError::BadRequest(format!("Unknown sprint: {sprint}"))),
        }
    }
    Ok(())
}

/// Tags are short labels. They are trimmed, lowercased, de-duplicated, and capped in count and length.
fn normalise_tags(items: impl Iterator<Item = String>) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for raw in items {
        let tag: String = raw.trim().to_lowercase().chars().take(32).collect();
        if !tag.is_empty() && !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    tags.truncate(12);
    tags
}

fn read_all(conn: &Connection, workspace: Option<&str>) -> Result<Vec<Task>, ApiError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLUMNS} FROM tasks WHERE (?1 IS NULL OR workspace_id = ?1) ORDER BY created DESC"
    ))?;
    let tasks = stmt
        .query_map([workspace], row_to_task)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(tasks)
}

fn read_one(conn: &Connection, id: &str) -> Result<Option<Task>, ApiError> {
    let task = conn
        .query_row(
            &format!("SELECT {SELECT_COLUMNS} FROM tasks WHERE id = ?1"),
            [id],
            row_to_task,
        )
        .optional()?;
    Ok(task)
}

fn workspace_exists(conn: &Connection, id: &str) -> Result<bool, ApiError> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM workspaces WHERE id = ?1", [id], |r| r.get(0))?;
    Ok(count > 0)
}

/// `AT-<max numeric suffix + 1>`, same as the original server. IDs are global across workspaces.
fn next_id(tasks: &[Task]) -> String {
    let max = tasks
        .iter()
        .filter_map(|t| t.id.strip_prefix("AT-").and_then(|n| n.parse::<u64>().ok()))
        .max()
        .unwrap_or(0);
    format!("AT-{}", max + 1)
}

/// Builds a task from a request body, merged over an optional base task.
/// Mirrors `taskFromBody({...old, ...body, id})` in the original server.
fn build_task(
    input: &Map<String, Value>,
    base: Option<&Task>,
    id: &str,
    now: &str,
) -> Result<Task, ApiError> {
    let mut merged: Map<String, Value> = match base {
        Some(task) => match serde_json::to_value(task) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        },
        None => Map::new(),
    };
    for (key, value) in input {
        merged.insert(key.clone(), value.clone());
    }

    let text = |key: &str| -> Option<String> {
        merged
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };

    let title = text("title").ok_or_else(|| ApiError::BadRequest("title is required".into()))?;
    let subtasks = match merged.get("subtasks") {
        Some(Value::Array(items)) => serde_json::from_value::<Vec<Subtask>>(Value::Array(items.clone()))
            .map_err(|e| ApiError::BadRequest(format!("subtasks: {e}")))?,
        _ => Vec::new(),
    };

    let docs = match merged.get("docs") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().filter(|s| !s.is_empty()).map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };

    let tags = match merged.get("tags") {
        Some(Value::Array(items)) => normalise_tags(items.iter().filter_map(|v| v.as_str().map(str::to_string))),
        _ => Vec::new(),
    };

    // Task ids, without blanks, duplicates, or a link from the task to itself.
    let task_ids = |key: &str| -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        if let Some(Value::Array(items)) = merged.get(key) {
            for raw in items.iter().filter_map(Value::as_str) {
                let other = raw.trim().to_string();
                if !other.is_empty() && other != id && !ids.contains(&other) {
                    ids.push(other);
                }
            }
        }
        ids
    };
    let blocked_by = task_ids("blockedBy");
    let relates_to = task_ids("relatesTo");
    let links = normalise_links(&merged)?;

    Ok(Task {
        id: id.to_string(),
        title,
        description: text("description").unwrap_or_default(),
        kind: text("type").unwrap_or_else(|| "task".into()),
        priority: text("priority").unwrap_or_else(|| "medium".into()),
        status: normalize_status(text("status")),
        due: text("due").unwrap_or_default(),
        created: text("created").unwrap_or_else(today),
        updated: now.to_string(),
        subtasks,
        parent_id: text("parentId"),
        workspace: text("workspace").unwrap_or_else(|| DEFAULT_WORKSPACE.into()),
        docs,
        tags,
        suggested_model: text("suggestedModel").map(|m| m.trim().to_string()).unwrap_or_default(),
        blocked_by,
        relates_to,
        standalone: merged.get("standalone").and_then(Value::as_bool).unwrap_or(true),
        standalone_note: text("standaloneNote").map(|n| n.trim().to_string()).unwrap_or_default(),
        links,
        sprint: merged.get("sprint").and_then(Value::as_i64),
    })
}

/// Field-by-field differences between two versions of a task, as `{field: {from, to}}`.
fn diff_tasks(old: &Task, new: &Task) -> Map<String, Value> {
    let mut changes = Map::new();
    let mut note = |field: &str, from: Value, to: Value| {
        if from != to {
            changes.insert(field.into(), json!({ "from": from, "to": to }));
        }
    };
    note("title", json!(old.title), json!(new.title));
    note("description", json!(old.description), json!(new.description));
    note("type", json!(old.kind), json!(new.kind));
    note("priority", json!(old.priority), json!(new.priority));
    note("status", json!(old.status), json!(new.status));
    note("due", json!(old.due), json!(new.due));
    note("parentId", json!(old.parent_id), json!(new.parent_id));
    note("workspace", json!(old.workspace), json!(new.workspace));
    note("tags", json!(old.tags.join(", ")), json!(new.tags.join(", ")));
    note("suggestedModel", json!(old.suggested_model), json!(new.suggested_model));
    note("blockedBy", json!(old.blocked_by.join(", ")), json!(new.blocked_by.join(", ")));
    note("relatesTo", json!(old.relates_to.join(", ")), json!(new.relates_to.join(", ")));
    note("standalone", json!(old.standalone), json!(new.standalone));
    note("standaloneNote", json!(old.standalone_note), json!(new.standalone_note));
    note("links", json!(old.links.join(", ")), json!(new.links.join(", ")));
    note("sprint", json!(old.sprint), json!(new.sprint));
    if old.docs != new.docs {
        changes.insert(
            "docs".into(),
            json!({ "from": old.docs.join(", "), "to": new.docs.join(", ") }),
        );
    }
    if old.subtasks != new.subtasks {
        let done = |t: &Task| t.subtasks.iter().filter(|s| s.done).count();
        changes.insert(
            "subtasks".into(),
            json!({
                "from": format!("{}/{} done", done(old), old.subtasks.len()),
                "to": format!("{}/{} done", done(new), new.subtasks.len()),
            }),
        );
    }
    changes
}

fn describe_change(field: &str, value: &Value) -> String {
    let from = value["from"].as_str().unwrap_or("").to_string();
    let to = value["to"].as_str().unwrap_or("").to_string();
    if from.is_empty() && to.is_empty() {
        field.to_string()
    } else {
        format!("{field} {from} → {to}")
    }
}

fn summarize(actor: &Actor, action: &str, changes: &Map<String, Value>) -> String {
    let who = actor.label();
    match action {
        "created" => format!("{who} created this task"),
        "deleted" => format!("{who} deleted this task"),
        _ => {
            let parts: Vec<String> = changes.iter().take(3).map(|(k, v)| describe_change(k, v)).collect();
            let more = changes.len().saturating_sub(3);
            let tail = if more > 0 { format!(" and {more} more") } else { String::new() };
            format!("{who} changed {}{tail}", parts.join(", "))
        }
    }
}

fn slugify(name: &str) -> String {
    let mut out = String::new();
    for ch in name.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let trimmed = out.trim_end_matches('-');
    trimmed.chars().take(40).collect::<String>().trim_end_matches('-').to_string()
}

fn record_history(
    conn: &Connection,
    task_id: &str,
    action: &str,
    actor: &Actor,
    changes: &Map<String, Value>,
) -> Result<(), ApiError> {
    conn.execute(
        "INSERT INTO task_history (task_id, at, action, actor_kind, actor_name, actor_model, changes, summary)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            task_id,
            now_iso(),
            action,
            actor.kind,
            actor.name,
            actor.model,
            Value::Object(changes.clone()).to_string(),
            summarize(actor, action, changes),
        ],
    )?;
    Ok(())
}

/// Before the first workspace-aware schema change, copies an existing database into `backups/`.
fn backup_before_migration(path: &Path) -> Result<(), ApiError> {
    if !path.exists() {
        return Ok(());
    }
    let needs = {
        let conn = Connection::open(path)?;
        let has_tasks: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'tasks'",
            [],
            |r| r.get(0),
        )?;
        let mut stmt = conn.prepare("PRAGMA table_info(tasks)")?;
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        has_tasks > 0 && !columns.iter().any(|c| c == "workspace_id")
    };
    if needs {
        let dir = path.parent().unwrap_or(Path::new(".")).join("backups");
        std::fs::create_dir_all(&dir).map_err(|e| ApiError::Internal(e.to_string()))?;
        let stamp = Utc::now().format("%Y%m%d-%H%M%S");
        std::fs::copy(path, dir.join(format!("clankban-before-workspaces-{stamp}.sqlite")))
            .map_err(|e| ApiError::Internal(format!("could not back up before migrating: {e}")))?;
    }
    Ok(())
}

impl Store {
    pub fn open(data_dir: &Path) -> Result<Self, ApiError> {
        std::fs::create_dir_all(data_dir).map_err(|e| ApiError::Internal(e.to_string()))?;
        let path = data_dir.join(DB_FILE);
        backup_before_migration(&path)?;
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, ApiError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, ApiError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                description TEXT DEFAULT '',
                type TEXT NOT NULL,
                priority TEXT NOT NULL,
                status TEXT NOT NULL,
                due TEXT,
                created TEXT NOT NULL,
                subtasks TEXT NOT NULL DEFAULT '[]',
                updated TEXT NOT NULL,
                parent_id TEXT,
                workspace_id TEXT NOT NULL DEFAULT 'default',
                docs TEXT NOT NULL DEFAULT '[]',
                tags TEXT NOT NULL DEFAULT '[]',
                suggested_model TEXT NOT NULL DEFAULT '',
                blocked_by TEXT NOT NULL DEFAULT '[]',
                relates_to TEXT NOT NULL DEFAULT '[]',
                standalone INTEGER NOT NULL DEFAULT 1,
                standalone_note TEXT NOT NULL DEFAULT '',
                links TEXT NOT NULL DEFAULT '[]'
            );
            CREATE TABLE IF NOT EXISTS task_comments (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id TEXT NOT NULL,
                at TEXT NOT NULL,
                actor_kind TEXT NOT NULL,
                actor_name TEXT NOT NULL,
                actor_model TEXT NOT NULL DEFAULT '',
                body TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS task_comments_by_task ON task_comments (task_id, id);
            CREATE TABLE IF NOT EXISTS workspaces (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                created TEXT NOT NULL,
                project_root TEXT,
                suggested_root TEXT,
                suggested_by TEXT
            );
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS views (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workspace_id TEXT NOT NULL,
                name TEXT NOT NULL,
                layout TEXT NOT NULL,
                filters TEXT NOT NULL DEFAULT '{}',
                created TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS sprints (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workspace_id TEXT NOT NULL,
                name TEXT NOT NULL,
                start_date TEXT NOT NULL,
                end_date TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'planned',
                created TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS task_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id TEXT NOT NULL,
                at TEXT NOT NULL,
                action TEXT NOT NULL,
                actor_kind TEXT NOT NULL,
                actor_name TEXT NOT NULL,
                actor_model TEXT NOT NULL DEFAULT '',
                changes TEXT NOT NULL DEFAULT '{}',
                summary TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS task_history_by_task ON task_history (task_id, id);
            CREATE TABLE IF NOT EXISTS board_requests (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                folder TEXT,
                requested_by TEXT NOT NULL,
                status TEXT NOT NULL,
                created TEXT NOT NULL,
                workspace_id TEXT
            );
            CREATE TABLE IF NOT EXISTS run_requests (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                skill TEXT NOT NULL,
                arguments TEXT NOT NULL DEFAULT '{}',
                status TEXT NOT NULL,
                result TEXT NOT NULL DEFAULT '',
                created TEXT NOT NULL,
                updated TEXT NOT NULL
            );",
        )?;

        let columns: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(tasks)")?;
            let names = stmt
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            names
        };
        if !columns.iter().any(|n| n == "parent_id") {
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN parent_id TEXT;")?;
        }
        if !columns.iter().any(|n| n == "workspace_id") {
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN workspace_id TEXT NOT NULL DEFAULT 'default';")?;
        }
        if !columns.iter().any(|n| n == "docs") {
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN docs TEXT NOT NULL DEFAULT '[]';")?;
        }
        if !columns.iter().any(|n| n == "tags") {
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN tags TEXT NOT NULL DEFAULT '[]';")?;
        }
        if !columns.iter().any(|n| n == "suggested_model") {
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN suggested_model TEXT NOT NULL DEFAULT '';")?;
        }
        if !columns.iter().any(|n| n == "blocked_by") {
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN blocked_by TEXT NOT NULL DEFAULT '[]';")?;
        }
        if !columns.iter().any(|n| n == "relates_to") {
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN relates_to TEXT NOT NULL DEFAULT '[]';")?;
        }
        if !columns.iter().any(|n| n == "standalone") {
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN standalone INTEGER NOT NULL DEFAULT 1;")?;
        }
        if !columns.iter().any(|n| n == "standalone_note") {
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN standalone_note TEXT NOT NULL DEFAULT '';")?;
        }
        if !columns.iter().any(|n| n == "sprint_id") {
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN sprint_id INTEGER;")?;
        }
        if !columns.iter().any(|n| n == "links") {
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN links TEXT NOT NULL DEFAULT '[]';")?;
        }
        let workspace_columns: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(workspaces)")?;
            let names = stmt
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            names
        };
        if !workspace_columns.iter().any(|n| n == "project_root") {
            conn.execute_batch("ALTER TABLE workspaces ADD COLUMN project_root TEXT;")?;
        }
        if !workspace_columns.iter().any(|n| n == "suggested_root") {
            conn.execute_batch("ALTER TABLE workspaces ADD COLUMN suggested_root TEXT;")?;
        }
        if !workspace_columns.iter().any(|n| n == "suggested_by") {
            conn.execute_batch("ALTER TABLE workspaces ADD COLUMN suggested_by TEXT;")?;
        }
        if !workspace_columns.iter().any(|n| n == "sprint_days") {
            conn.execute_batch("ALTER TABLE workspaces ADD COLUMN sprint_days INTEGER NOT NULL DEFAULT 14;")?;
        }

        conn.execute(
            "INSERT OR IGNORE INTO workspaces (id, name, created) VALUES (?1, 'Default', '1970-01-01T00:00:00.000Z')",
            [DEFAULT_WORKSPACE],
        )?;

        let count: i64 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))?;
        if count == 0 {
            let now = now_iso();
            for (id, title, description, kind, priority, status, due, created, subtasks) in SEED {
                conn.execute(
                    "INSERT INTO tasks (id, title, description, type, priority, status, due, created, subtasks, updated, parent_id, workspace_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, 'default')",
                    params![id, title, description, kind, priority, status, due, created, subtasks, now],
                )?;
            }
        }

        Ok(Store { conn: Mutex::new(conn) })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// All tasks, or only those in `workspace` when given.
    pub fn list(&self, workspace: Option<&str>) -> Result<Vec<Task>, ApiError> {
        read_all(&self.lock(), workspace)
    }

    pub fn get(&self, id: &str) -> Result<Option<Task>, ApiError> {
        read_one(&self.lock(), id)
    }

    pub fn create(&self, input: &Map<String, Value>, actor: &Actor) -> Result<Task, ApiError> {
        let conn = self.lock();
        let id = match input.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            Some(id) => id.to_string(),
            None => next_id(&read_all(&conn, None)?),
        };
        if read_one(&conn, &id)?.is_some() {
            return Err(ApiError::Conflict(format!("Task {id} already exists")));
        }

        let task = build_task(input, None, &id, &now_iso())?;
        if !workspace_exists(&conn, &task.workspace)? {
            return Err(ApiError::BadRequest(format!("Unknown workspace: {}", task.workspace)));
        }
        let subtasks = serde_json::to_string(&task.subtasks).map_err(|e| ApiError::Internal(e.to_string()))?;
        let docs = serde_json::to_string(&task.docs).map_err(|e| ApiError::Internal(e.to_string()))?;
        let tags = serde_json::to_string(&task.tags).map_err(|e| ApiError::Internal(e.to_string()))?;
        let (blocked, related, links) = relation_columns(&task)?;
        check_references(&conn, &task)?;
        conn.execute(
            "INSERT INTO tasks (id, title, description, type, priority, status, due, created, subtasks, updated, parent_id, workspace_id, docs, tags, suggested_model, blocked_by, relates_to, standalone, standalone_note, links)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            params![
                task.id, task.title, task.description, task.kind, task.priority, task.status,
                task.due, task.created, subtasks, task.updated, task.parent_id, task.workspace, docs,
                tags, task.suggested_model, blocked, related, task.standalone as i64, task.standalone_note, links
            ],
        )?;
        conn.execute("UPDATE tasks SET sprint_id = ?2 WHERE id = ?1", params![task.id, task.sprint])?;
        record_history(&conn, &task.id, "created", actor, &Map::new())?;
        Ok(task)
    }

    pub fn update(&self, id: &str, input: &Map<String, Value>, actor: &Actor) -> Result<Task, ApiError> {
        let conn = self.lock();
        let old = read_one(&conn, id)?.ok_or(ApiError::NotFound)?;
        let task = build_task(input, Some(&old), id, &now_iso())?;
        if !workspace_exists(&conn, &task.workspace)? {
            return Err(ApiError::BadRequest(format!("Unknown workspace: {}", task.workspace)));
        }
        let subtasks = serde_json::to_string(&task.subtasks).map_err(|e| ApiError::Internal(e.to_string()))?;
        let docs = serde_json::to_string(&task.docs).map_err(|e| ApiError::Internal(e.to_string()))?;
        let tags = serde_json::to_string(&task.tags).map_err(|e| ApiError::Internal(e.to_string()))?;
        let (blocked, related, links) = relation_columns(&task)?;
        check_references(&conn, &task)?;
        conn.execute(
            "UPDATE tasks SET title = ?2, description = ?3, type = ?4, priority = ?5, status = ?6,
                due = ?7, subtasks = ?8, updated = ?9, parent_id = ?10, workspace_id = ?11, docs = ?12,
                tags = ?13, suggested_model = ?14, blocked_by = ?15, relates_to = ?16, standalone = ?17,
                standalone_note = ?18, links = ?19
             WHERE id = ?1",
            params![
                id, task.title, task.description, task.kind, task.priority, task.status,
                task.due, subtasks, task.updated, task.parent_id, task.workspace, docs,
                tags, task.suggested_model, blocked, related, task.standalone as i64, task.standalone_note, links
            ],
        )?;
        conn.execute("UPDATE tasks SET sprint_id = ?2 WHERE id = ?1", params![id, task.sprint])?;
        let changes = diff_tasks(&old, &task);
        if !changes.is_empty() {
            record_history(&conn, id, "updated", actor, &changes)?;
        }
        Ok(task)
    }

    /// Deletes a task. Its history stays, so the record of what happened is kept.
    pub fn delete(&self, id: &str, actor: &Actor) -> Result<(), ApiError> {
        let conn = self.lock();
        if read_one(&conn, id)?.is_some() {
            conn.execute("DELETE FROM tasks WHERE id = ?1", [id])?;
            conn.execute("DELETE FROM task_comments WHERE task_id = ?1", [id])?;
            record_history(&conn, id, "deleted", actor, &Map::new())?;
        }
        Ok(())
    }

    /// Adds a comment. Comments are limited in length, and the task must exist.
    pub fn add_comment(&self, task_id: &str, body: &str, actor: &Actor) -> Result<Comment, ApiError> {
        let body = body.trim();
        if body.is_empty() {
            return Err(ApiError::BadRequest("comment is empty".into()));
        }
        if body.chars().count() > 4000 {
            return Err(ApiError::BadRequest("comments are limited to 4000 characters".into()));
        }
        let conn = self.lock();
        if read_one(&conn, task_id)?.is_none() {
            return Err(ApiError::NotFound);
        }
        conn.execute(
            "INSERT INTO task_comments (task_id, at, actor_kind, actor_name, actor_model, body) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![task_id, now_iso(), actor.kind, actor.name, actor.model, body],
        )?;
        let id = conn.last_insert_rowid();
        conn.query_row(
            &format!("SELECT {COMMENT_COLUMNS} FROM task_comments WHERE id = ?1"),
            [id],
            row_to_comment,
        )
        .optional()?
        .ok_or(ApiError::NotFound)
    }

    /// Newest first.
    pub fn list_comments(&self, task_id: &str) -> Result<Vec<Comment>, ApiError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COMMENT_COLUMNS} FROM task_comments WHERE task_id = ?1 ORDER BY id DESC"
        ))?;
        let rows = stmt.query_map([task_id], row_to_comment)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Newest first.
    pub fn history(&self, task_id: &str) -> Result<Vec<HistoryEntry>, ApiError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {HISTORY_COLUMNS} FROM task_history WHERE task_id = ?1 ORDER BY id DESC"
        ))?;
        let entries = stmt
            .query_map([task_id], row_to_history)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(entries)
    }

    /// The board whose connected project folder holds `folder`. When folders are nested, the most
    /// specific one wins, so a sub-project gets its own board.
    pub fn workspace_for_folder(&self, folder: &str) -> Result<Option<Workspace>, ApiError> {
        let best = self
            .list_workspaces()?
            .into_iter()
            .filter_map(|w| {
                let root_len = w.project_root.as_deref().filter(|root| folder_is_within(folder, root))?.len();
                Some((root_len, w))
            })
            .max_by_key(|(root_len, _)| *root_len)
            .map(|(_, w)| w);
        Ok(best)
    }

    /// The board that already has this folder connected, or has it suggested. A second board for
    /// the same folder is refused, so agents do not end up with two boards for one project.
    pub fn workspace_claiming_folder(&self, folder: &str) -> Result<Option<Workspace>, ApiError> {
        let target = normalise_folder(folder);
        let found = self.list_workspaces()?.into_iter().find(|w| {
            let connected = w.project_root.as_deref().map(normalise_folder);
            let suggested = w.suggested_root.as_deref().map(normalise_folder);
            connected.as_deref() == Some(target.as_str()) || suggested.as_deref() == Some(target.as_str())
        });
        Ok(found)
    }

    /// Newest first, across the tasks in one workspace. Each entry carries its task's current title.
    /// Entries for deleted tasks are left out, because a deleted task no longer belongs to a workspace.
    pub fn recent_history(&self, workspace: Option<&str>, limit: usize) -> Result<Vec<RecentHistoryEntry>, ApiError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT h.id, h.task_id, h.at, h.action, h.actor_kind, h.actor_name, h.actor_model, h.changes, h.summary, t.title, t.workspace_id
             FROM task_history h JOIN tasks t ON t.id = h.task_id
             WHERE (?1 IS NULL OR t.workspace_id = ?1)
             ORDER BY h.id DESC LIMIT ?2",
        )?;
        let entries = stmt
            .query_map(params![workspace, limit as i64], |row| {
                Ok(RecentHistoryEntry {
                    entry: row_to_history(row)?,
                    task_title: row.get::<_, String>(9)?,
                    task_workspace: row.get::<_, String>(10)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(entries)
    }

    /// The dashboard: counts for each board, the next due tasks, and recent activity.
    /// `workspace` limits everything to one board; `None` covers every board.
    pub fn dashboard(&self, workspace: Option<&str>) -> Result<Value, ApiError> {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let week_out = (Utc::now() + Duration::days(7)).format("%Y-%m-%d").to_string();
        let boards: Vec<Workspace> = self
            .list_workspaces()?
            .into_iter()
            .filter(|w| workspace.map_or(true, |id| w.id == id))
            .collect();

        let mut summaries = Vec::new();
        let mut upcoming: Vec<Value> = Vec::new();
        let mut totals = [0u64; 5]; // active, high priority, due within 7 days, overdue, blocked
        for board in &boards {
            let tasks = self.list(Some(&board.id))?;
            let statuses: std::collections::HashMap<&str, &str> =
                tasks.iter().map(|t| (t.id.as_str(), t.status.as_str())).collect();
            let blocked_now = |t: &Task| {
                t.blocked_by.iter().any(|b| statuses.get(b.as_str()).map_or(true, |s| *s != "done"))
            };
            let active: Vec<&Task> = tasks.iter().filter(|t| t.status != "done").collect();
            let count = |status: &str| tasks.iter().filter(|t| t.status == status).count();
            let high = active.iter().filter(|t| t.priority == "high").count();
            let due_soon: Vec<&&Task> = active.iter().filter(|t| !t.due.is_empty() && t.due <= week_out).collect();
            let overdue = active.iter().filter(|t| !t.due.is_empty() && t.due < today).count();
            let blocked = active.iter().filter(|t| blocked_now(t)).count();
            let last_updated = tasks.iter().map(|t| t.updated.as_str()).max().unwrap_or("").to_string();

            totals[0] += active.len() as u64;
            totals[1] += high as u64;
            totals[2] += due_soon.len() as u64;
            totals[3] += overdue as u64;
            totals[4] += blocked as u64;
            for t in &due_soon {
                upcoming.push(json!({
                    "id": t.id, "title": t.title, "due": t.due, "status": t.status,
                    "priority": t.priority, "workspace": board.id, "workspaceName": board.name,
                }));
            }
            summaries.push(json!({
                "id": board.id,
                "name": board.name,
                "projectRoot": board.project_root,
                "active": active.len(),
                "byStatus": {
                    "backlog": count("backlog"),
                    "progress": count("progress"),
                    "review": count("review"),
                    "done": count("done"),
                },
                "highPriority": high,
                "dueSoon": due_soon.len(),
                "overdue": overdue,
                "blocked": blocked,
                "lastUpdated": last_updated,
            }));
        }
        upcoming.sort_by(|a, b| a["due"].as_str().cmp(&b["due"].as_str()));
        upcoming.truncate(12);
        let recent = self.recent_history(workspace, 25)?;

        Ok(json!({
            "scope": workspace.unwrap_or("all"),
            "totals": {
                "active": totals[0],
                "highPriority": totals[1],
                "dueSoon": totals[2],
                "overdue": totals[3],
                "blocked": totals[4],
            },
            "workspaces": summaries,
            "upcoming": upcoming,
            "recent": recent,
        }))
    }

    /// The sprints of one board, newest first.
    pub fn list_sprints(&self, workspace: &str) -> Result<Vec<Sprint>, ApiError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {SPRINT_COLUMNS} FROM sprints s WHERE s.workspace_id = ?1 ORDER BY s.start_date DESC, s.id DESC"
        ))?;
        let rows = stmt.query_map([workspace], row_to_sprint)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Creates a planned sprint. The start defaults to today. The end comes from `endDate`, from
    /// `lengthDays`, or from the board's default length, in that order.
    pub fn create_sprint(
        &self,
        workspace: &str,
        name: &str,
        start: Option<&str>,
        length_days: Option<i64>,
        end: Option<&str>,
    ) -> Result<Sprint, ApiError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(ApiError::BadRequest("name is required".into()));
        }
        let conn = self.lock();
        if !workspace_exists(&conn, workspace)? {
            return Err(ApiError::NotFound);
        }
        let start_day = match start.map(str::trim).filter(|t| !t.is_empty()) {
            Some(text) => parse_day(text)?,
            None => Utc::now().date_naive(),
        };
        let end_day = resolve_sprint_end(&conn, workspace, start_day, length_days, end)?;
        conn.execute(
            "INSERT INTO sprints (workspace_id, name, start_date, end_date, status, created) VALUES (?1, ?2, ?3, ?4, 'planned', ?5)",
            params![workspace, name, start_day.to_string(), end_day.to_string(), now_iso()],
        )?;
        let id = conn.last_insert_rowid();
        read_sprint(&conn, id)?.ok_or(ApiError::NotFound)
    }

    /// Edits a sprint. Setting `status` to `active` starts it; only one sprint runs at a time in a board,
    /// so close the running one first.
    pub fn update_sprint(&self, id: i64, input: &Map<String, Value>) -> Result<Sprint, ApiError> {
        let conn = self.lock();
        let current = read_sprint(&conn, id)?.ok_or(ApiError::NotFound)?;
        let name = match input.get("name").and_then(Value::as_str).map(str::trim) {
            Some("") => return Err(ApiError::BadRequest("name is required".into())),
            Some(name) => name.to_string(),
            None => current.name.clone(),
        };
        let current_start = parse_day(&current.start_date)?;
        let current_length = (parse_day(&current.end_date)? - current_start).num_days() + 1;
        let start = match input.get("startDate").and_then(Value::as_str) {
            Some(text) => parse_day(text)?,
            None => current_start,
        };
        let end_text = input.get("endDate").and_then(Value::as_str);
        // Moving the start keeps the length, unless a new length or end date is given.
        let length = input
            .get("lengthDays")
            .and_then(Value::as_i64)
            .or(if end_text.map_or(true, |t| t.trim().is_empty()) { Some(current_length) } else { None });
        let end = resolve_sprint_end(&conn, &current.workspace, start, length, end_text)?;
        let status = match input.get("status").and_then(Value::as_str).map(str::trim) {
            Some(status @ ("planned" | "active" | "closed")) => status.to_string(),
            Some(other) => return Err(ApiError::BadRequest(format!("unknown sprint status: {other}"))),
            None => current.status.clone(),
        };
        if status == "active" && current.status != "active" {
            let running: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sprints WHERE workspace_id = ?1 AND status = 'active' AND id != ?2",
                params![current.workspace, id],
                |row| row.get(0),
            )?;
            if running > 0 {
                return Err(ApiError::Conflict("another sprint is already running in this board. Close it first.".into()));
            }
        }
        conn.execute(
            "UPDATE sprints SET name = ?2, start_date = ?3, end_date = ?4, status = ?5 WHERE id = ?1",
            params![id, name, start.to_string(), end.to_string(), status],
        )?;
        read_sprint(&conn, id)?.ok_or(ApiError::NotFound)
    }

    /// Deletes a sprint. Its tasks stay on the board, just out of the sprint.
    pub fn delete_sprint(&self, id: i64) -> Result<(), ApiError> {
        let conn = self.lock();
        if read_sprint(&conn, id)?.is_none() {
            return Err(ApiError::NotFound);
        }
        conn.execute("UPDATE tasks SET sprint_id = NULL WHERE sprint_id = ?1", [id])?;
        conn.execute("DELETE FROM sprints WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Sets the default length of new sprints on a board.
    pub fn set_sprint_days(&self, workspace: &str, days: i64) -> Result<Workspace, ApiError> {
        if !(1..=MAX_SPRINT_DAYS).contains(&days) {
            return Err(ApiError::BadRequest(format!("sprint length must be between 1 and {MAX_SPRINT_DAYS} days")));
        }
        {
            let conn = self.lock();
            if !workspace_exists(&conn, workspace)? {
                return Err(ApiError::NotFound);
            }
            conn.execute("UPDATE workspaces SET sprint_days = ?2 WHERE id = ?1", params![workspace, days])?;
        }
        self.list_workspaces()?
            .into_iter()
            .find(|w| w.id == workspace)
            .ok_or(ApiError::NotFound)
    }

    /// The views saved on one board, by name.
    pub fn list_views(&self, workspace: &str) -> Result<Vec<View>, ApiError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!("SELECT {VIEW_COLUMNS} FROM views WHERE workspace_id = ?1 ORDER BY name COLLATE NOCASE, id"))?;
        let rows = stmt.query_map([workspace], row_to_view)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Saves a view: a name, a layout (board or list), and the filters to apply.
    pub fn create_view(&self, workspace: &str, name: Option<&str>, layout: Option<&str>, filters: Option<&Value>) -> Result<View, ApiError> {
        let name = view_name(name)?;
        let layout = view_layout(layout)?;
        let filters = view_filters(filters);
        let conn = self.lock();
        if !workspace_exists(&conn, workspace)? {
            return Err(ApiError::NotFound);
        }
        let stored = serde_json::to_string(&filters).map_err(|e| ApiError::Internal(e.to_string()))?;
        conn.execute(
            "INSERT INTO views (workspace_id, name, layout, filters, created) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![workspace, name, layout, stored, now_iso()],
        )?;
        let id = conn.last_insert_rowid();
        conn.query_row(&format!("SELECT {VIEW_COLUMNS} FROM views WHERE id = ?1"), [id], row_to_view)
            .optional()?
            .ok_or(ApiError::NotFound)
    }

    /// Changes a view's name, layout or filters. Fields left out keep their values.
    pub fn update_view(&self, id: i64, input: &Map<String, Value>) -> Result<View, ApiError> {
        let conn = self.lock();
        let current = conn
            .query_row(&format!("SELECT {VIEW_COLUMNS} FROM views WHERE id = ?1"), [id], row_to_view)
            .optional()?
            .ok_or(ApiError::NotFound)?;
        let name = match input.get("name") {
            Some(value) => view_name(value.as_str())?,
            None => current.name.clone(),
        };
        let layout = match input.get("layout") {
            Some(value) => view_layout(value.as_str())?,
            None => current.layout.clone(),
        };
        let filters = if input.contains_key("filters") { view_filters(input.get("filters")) } else { current.filters.clone() };
        let stored = serde_json::to_string(&filters).map_err(|e| ApiError::Internal(e.to_string()))?;
        conn.execute("UPDATE views SET name = ?2, layout = ?3, filters = ?4 WHERE id = ?1", params![id, name, layout, stored])?;
        conn.query_row(&format!("SELECT {VIEW_COLUMNS} FROM views WHERE id = ?1"), [id], row_to_view)
            .optional()?
            .ok_or(ApiError::NotFound)
    }

    /// Deletes a saved view. Its tasks and sprints are not touched.
    pub fn delete_view(&self, id: i64) -> Result<(), ApiError> {
        let conn = self.lock();
        let removed = conn.execute("DELETE FROM views WHERE id = ?1", [id])?;
        if removed == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    /// Every setting, with its default where none is saved.
    pub fn get_settings(&self) -> Result<std::collections::BTreeMap<String, bool>, ApiError> {
        let conn = self.lock();
        let mut settings: std::collections::BTreeMap<String, bool> =
            AGENT_SETTINGS.iter().map(|(key, default)| (key.to_string(), *default)).collect();
        let mut stmt = conn.prepare("SELECT key, value FROM settings")?;
        let saved = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (key, value) in saved {
            if settings.contains_key(&key) {
                settings.insert(key, value != 0);
            }
        }
        Ok(settings)
    }

    /// Changes one setting. Unknown names are refused.
    pub fn set_setting(&self, key: &str, enabled: bool) -> Result<std::collections::BTreeMap<String, bool>, ApiError> {
        if !AGENT_SETTINGS.iter().any(|(known, _)| *known == key) {
            return Err(ApiError::BadRequest(format!("unknown setting: {key}")));
        }
        {
            let conn = self.lock();
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, enabled as i64],
            )?;
        }
        self.get_settings()
    }

    pub fn list_workspaces(&self) -> Result<Vec<Workspace>, ApiError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT w.id, w.name, w.created,
                    (SELECT COUNT(*) FROM tasks t WHERE t.workspace_id = w.id),
                    w.project_root, w.suggested_root, w.suggested_by, w.sprint_days
             FROM workspaces w ORDER BY w.created ASC, w.name ASC",
        )?;
        let workspaces = stmt
            .query_map([], |row| {
                Ok(Workspace {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    created: row.get(2)?,
                    tasks: row.get(3)?,
                    project_root: row.get::<_, Option<String>>(4)?.filter(|s| !s.is_empty()),
                    suggested_root: row.get::<_, Option<String>>(5)?.filter(|s| !s.is_empty()),
                    suggested_by: row.get::<_, Option<String>>(6)?.filter(|s| !s.is_empty()),
                    sprint_days: row.get::<_, Option<i64>>(7)?.unwrap_or(DEFAULT_SPRINT_DAYS),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(workspaces)
    }

    /// The project folder a workspace's documents are read from. `None` clears it.
    /// The path is checked to be an existing folder and stored in its absolute form.
    pub fn set_project_root(&self, id: &str, root: Option<&str>) -> Result<Option<String>, ApiError> {
        let conn = self.lock();
        if !workspace_exists(&conn, id)? {
            return Err(ApiError::NotFound);
        }
        let stored = match root.map(str::trim).filter(|s| !s.is_empty()) {
            Some(path) => Some(crate::docs::validate_root(path).map_err(ApiError::BadRequest)?),
            None => None,
        };
        // Connecting a folder also clears any suggestion that was waiting for an answer.
        conn.execute(
            "UPDATE workspaces SET project_root = ?2, suggested_root = NULL, suggested_by = NULL WHERE id = ?1",
            params![id, stored],
        )?;
        Ok(stored)
    }

    /// An agent proposes a project folder. The board shows the proposal, and nothing connects
    /// until a person accepts it. The folder must exist. A new suggestion replaces the old one.
    pub fn suggest_project_root(&self, id: &str, root: &str, by: &str) -> Result<String, ApiError> {
        let conn = self.lock();
        if !workspace_exists(&conn, id)? {
            return Err(ApiError::NotFound);
        }
        let stored = crate::docs::validate_root(root.trim()).map_err(ApiError::BadRequest)?;
        conn.execute(
            "UPDATE workspaces SET suggested_root = ?2, suggested_by = ?3 WHERE id = ?1",
            params![id, stored, by],
        )?;
        Ok(stored)
    }

    /// Removes a pending suggestion. The connected folder is left as it is.
    pub fn dismiss_suggestion(&self, id: &str) -> Result<(), ApiError> {
        let conn = self.lock();
        if !workspace_exists(&conn, id)? {
            return Err(ApiError::NotFound);
        }
        conn.execute(
            "UPDATE workspaces SET suggested_root = NULL, suggested_by = NULL WHERE id = ?1",
            [id],
        )?;
        Ok(())
    }

    pub fn project_root(&self, id: &str) -> Result<Option<String>, ApiError> {
        let conn = self.lock();
        let root: Option<String> = conn
            .query_row("SELECT project_root FROM workspaces WHERE id = ?1", [id], |r| r.get(0))
            .optional()?
            .ok_or(ApiError::NotFound)?;
        Ok(root.filter(|s| !s.is_empty()))
    }

    /// The id is derived from the name ("Home renovation" → "home-renovation").
    pub fn create_workspace(&self, name: &str) -> Result<Workspace, ApiError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(ApiError::BadRequest("name is required".into()));
        }
        let id = slugify(name);
        if id.is_empty() {
            return Err(ApiError::BadRequest("name needs at least one letter or number".into()));
        }
        let conn = self.lock();
        if workspace_exists(&conn, &id)? {
            return Err(ApiError::Conflict(format!("Workspace {id} already exists")));
        }
        let created = now_iso();
        conn.execute(
            "INSERT INTO workspaces (id, name, created) VALUES (?1, ?2, ?3)",
            params![id, name, created],
        )?;
        Ok(Workspace {
            id,
            name: name.into(),
            created,
            tasks: 0,
            project_root: None,
            suggested_root: None,
            suggested_by: None,
            sprint_days: DEFAULT_SPRINT_DAYS,
        })
    }

    pub fn rename_workspace(&self, id: &str, name: &str) -> Result<(), ApiError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(ApiError::BadRequest("name is required".into()));
        }
        let conn = self.lock();
        if !workspace_exists(&conn, id)? {
            return Err(ApiError::NotFound);
        }
        conn.execute("UPDATE workspaces SET name = ?2 WHERE id = ?1", params![id, name])?;
        Ok(())
    }

    /// Only empty workspaces can be deleted, and the default workspace never can.
    pub fn delete_workspace(&self, id: &str) -> Result<(), ApiError> {
        if id == DEFAULT_WORKSPACE {
            return Err(ApiError::BadRequest("The default workspace cannot be deleted".into()));
        }
        let conn = self.lock();
        if !workspace_exists(&conn, id)? {
            return Err(ApiError::NotFound);
        }
        let tasks: i64 = conn.query_row("SELECT COUNT(*) FROM tasks WHERE workspace_id = ?1", [id], |r| r.get(0))?;
        if tasks > 0 {
            return Err(ApiError::Conflict("Move or delete this workspace's tasks first".into()));
        }
        conn.execute("DELETE FROM workspaces WHERE id = ?1", [id])?;
        Ok(())
    }

    /// An agent asks for a new board. Nothing is created here: a person approves the request on
    /// the board first. A folder that already has a board returns that board, and a folder that
    /// already has a pending request returns that request, so nothing is duplicated.
    pub fn request_board(&self, name: &str, folder: Option<&str>, requested_by: &str) -> Result<BoardRequestResult, ApiError> {
        let folder = match folder.map(str::trim).filter(|f| !f.is_empty()) {
            Some(path) => Some(crate::docs::validate_root(path).map_err(ApiError::BadRequest)?),
            None => None,
        };
        if let Some(folder) = &folder {
            if let Some(existing) = self.workspace_claiming_folder(folder)? {
                return Ok(BoardRequestResult::Existing(existing));
            }
        }
        let name = match (name.trim(), &folder) {
            ("", Some(folder)) => Path::new(folder)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Project".into()),
            ("", None) => return Err(ApiError::BadRequest("a name or a project folder is required".into())),
            (name, _) => name.to_string(),
        };
        let conn = self.lock();
        let pending = match &folder {
            Some(folder) => conn
                .query_row(
                    &format!("SELECT {BOARD_REQUEST_COLUMNS} FROM board_requests WHERE folder = ?1 AND status = 'pending' ORDER BY id DESC LIMIT 1"),
                    [folder],
                    row_to_board_request,
                )
                .optional()?,
            None => None,
        };
        if let Some(request) = pending {
            return Ok(BoardRequestResult::Pending { request, is_new: false });
        }
        conn.execute(
            "INSERT INTO board_requests (name, folder, requested_by, status, created) VALUES (?1, ?2, ?3, 'pending', ?4)",
            params![name, folder, requested_by, now_iso()],
        )?;
        let id = conn.last_insert_rowid();
        let request = read_board_request(&conn, id)?.ok_or(ApiError::NotFound)?;
        Ok(BoardRequestResult::Pending { request, is_new: true })
    }

    /// Requests newest first. `pending_only` limits the list to requests still waiting for a person.
    pub fn list_board_requests(&self, pending_only: bool) -> Result<Vec<BoardRequest>, ApiError> {
        let conn = self.lock();
        let filter = if pending_only { "WHERE status = 'pending'" } else { "" };
        let mut stmt = conn.prepare(&format!("SELECT {BOARD_REQUEST_COLUMNS} FROM board_requests {filter} ORDER BY id DESC"))?;
        let rows = stmt.query_map([], row_to_board_request)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// A person approves a request: the board is created with the folder suggested, and a person
    /// still accepts the folder before it connects. If the folder got a board meanwhile, that board is used.
    pub fn approve_board_request(&self, id: i64) -> Result<(BoardRequest, Workspace), ApiError> {
        let request = {
            let conn = self.lock();
            read_board_request(&conn, id)?.ok_or(ApiError::NotFound)?
        };
        if request.status != "pending" {
            return Err(ApiError::Conflict(format!("request {id} was already {}", request.status)));
        }
        let existing = match &request.folder {
            Some(folder) => self.workspace_claiming_folder(folder)?,
            None => None,
        };
        let workspace = match existing {
            Some(workspace) => workspace,
            None => {
                let created = self.create_workspace(&request.name)?;
                if let Some(folder) = &request.folder {
                    self.suggest_project_root(&created.id, folder, &request.requested_by)?;
                }
                self.list_workspaces()?
                    .into_iter()
                    .find(|w| w.id == created.id)
                    .ok_or(ApiError::NotFound)?
            }
        };
        let conn = self.lock();
        conn.execute(
            "UPDATE board_requests SET status = 'approved', workspace_id = ?2 WHERE id = ?1",
            params![id, workspace.id],
        )?;
        let decided = read_board_request(&conn, id)?.ok_or(ApiError::NotFound)?;
        Ok((decided, workspace))
    }

    /// A person declines a request. Nothing is created.
    pub fn reject_board_request(&self, id: i64) -> Result<BoardRequest, ApiError> {
        let conn = self.lock();
        let request = read_board_request(&conn, id)?.ok_or(ApiError::NotFound)?;
        if request.status != "pending" {
            return Err(ApiError::Conflict(format!("request {id} was already {}", request.status)));
        }
        conn.execute("UPDATE board_requests SET status = 'rejected' WHERE id = ?1", [id])?;
        read_board_request(&conn, id)?.ok_or(ApiError::NotFound)
    }

    pub fn create_run(&self, skill: &str, arguments: &Value) -> Result<RunRequest, ApiError> {
        let now = now_iso();
        let args = serde_json::to_string(arguments).map_err(|e| ApiError::Internal(e.to_string()))?;
        let conn = self.lock();
        conn.execute(
            "INSERT INTO run_requests (skill, arguments, status, result, created, updated)
             VALUES (?1, ?2, 'pending', '', ?3, ?3)",
            params![skill, args, now],
        )?;
        let id = conn.last_insert_rowid();
        read_run(&conn, id)?.ok_or(ApiError::NotFound)
    }

    /// Requests oldest first, so an agent works the queue in order.
    pub fn list_runs(&self, status: Option<&str>) -> Result<Vec<RunRequest>, ApiError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM run_requests WHERE (?1 IS NULL OR status = ?1) ORDER BY id ASC"
        ))?;
        let runs = stmt
            .query_map([status], row_to_run)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(runs)
    }

    pub fn update_run(&self, id: i64, status: &str, result: Option<&str>) -> Result<RunRequest, ApiError> {
        if !RUN_STATUSES.contains(&status) {
            return Err(ApiError::BadRequest(format!("Unknown status: {status}")));
        }
        let conn = self.lock();
        let current = read_run(&conn, id)?.ok_or(ApiError::NotFound)?;
        let result = result.map(str::to_string).unwrap_or(current.result);
        conn.execute(
            "UPDATE run_requests SET status = ?2, result = ?3, updated = ?4 WHERE id = ?1",
            params![id, status, result, now_iso()],
        )?;
        read_run(&conn, id)?.ok_or(ApiError::NotFound)
    }

    /// Compact active-work payload for planning agents, for one workspace.
    pub fn agent_context(&self, workspace: &str) -> Result<Value, ApiError> {
        let tasks = self.list(Some(workspace))?;
        let week_out = (Utc::now() + Duration::days(7)).format("%Y-%m-%d").to_string();
        let active: Vec<&Task> = tasks.iter().filter(|t| t.status != "done").collect();
        let due_soon = active
            .iter()
            .filter(|t| !t.due.is_empty() && t.due <= week_out)
            .count();
        // A blocker counts as open unless it is done. A blocker that no longer exists also counts as open.
        let statuses: std::collections::HashMap<&str, &str> =
            tasks.iter().map(|t| (t.id.as_str(), t.status.as_str())).collect();
        let open_blockers = |t: &Task| -> Vec<String> {
            t.blocked_by
                .iter()
                .filter(|b| statuses.get(b.as_str()).map_or(true, |s| *s != "done"))
                .cloned()
                .collect()
        };
        let blocked = active.iter().filter(|t| !open_blockers(t).is_empty()).count();
        let sprints = self.list_sprints(workspace)?;

        Ok(serde_json::json!({
            "workspace": workspace,
            "schemaVersion": SCHEMA_VERSION,
            "summary": { "active": active.len(), "blocked": blocked, "dueSoon": due_soon },
            "sprints": sprints,
            "tasks": active.iter().map(|t| serde_json::json!({
                "id": t.id,
                "title": t.title,
                "type": t.kind,
                "priority": t.priority,
                "status": t.status,
                "due": t.due,
                "subtasks": t.subtasks,
                "tags": t.tags,
                "suggestedModel": t.suggested_model,
                "parentId": t.parent_id,
                "blockedBy": t.blocked_by,
                "openBlockers": open_blockers(t),
                "standalone": t.standalone,
                "standaloneNote": t.standalone_note,
                "sprint": t.sprint,
            })).collect::<Vec<_>>(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("object")
    }

    fn who() -> Actor {
        Actor::human()
    }

    fn agent(model: &str) -> Actor {
        Actor { kind: "agent".into(), name: "Claude Code".into(), model: model.into() }
    }

    #[test]
    fn seeds_empty_database() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.list(None).unwrap().len(), SEED.len());
    }

    #[test]
    fn next_id_follows_highest_existing() {
        let store = Store::open_in_memory().unwrap();
        let task = store.create(&obj(json!({ "title": "New" })), &who()).unwrap();
        assert_eq!(task.id, "AT-116");
        assert_eq!(task.status, "backlog");
        assert_eq!(task.workspace, DEFAULT_WORKSPACE);
    }

    #[test]
    fn todo_status_is_normalised_to_backlog() {
        let store = Store::open_in_memory().unwrap();
        let task = store.create(&obj(json!({ "title": "Imported", "status": "todo" })), &who()).unwrap();
        assert_eq!(task.status, "backlog");
        assert_eq!(store.get(&task.id).unwrap().unwrap().status, "backlog");
    }

    #[test]
    fn parent_is_inferred_from_legacy_description() {
        let store = Store::open_in_memory().unwrap();
        let task = store
            .create(&obj(json!({ "title": "Child", "description": "Parent: AT-101" })), &who())
            .unwrap();
        assert_eq!(store.get(&task.id).unwrap().unwrap().parent_id.as_deref(), Some("AT-101"));
    }

    #[test]
    fn explicit_parent_id_is_stored() {
        let store = Store::open_in_memory().unwrap();
        let task = store.create(&obj(json!({ "title": "Child", "parentId": "AT-104" })), &who()).unwrap();
        assert_eq!(task.parent_id.as_deref(), Some("AT-104"));
    }

    #[test]
    fn update_keeps_created_and_inferred_parent() {
        let store = Store::open_in_memory().unwrap();
        let created = store
            .create(&obj(json!({ "title": "Child", "description": "Epic: AT-101", "created": "2026-01-02" })), &who())
            .unwrap();
        let updated = store.update(&created.id, &obj(json!({ "title": "Renamed" })), &who()).unwrap();
        assert_eq!(updated.title, "Renamed");
        assert_eq!(updated.created, "2026-01-02");
        assert_eq!(updated.parent_id.as_deref(), Some("AT-101"));
    }

    #[test]
    fn duplicate_id_conflicts() {
        let store = Store::open_in_memory().unwrap();
        let result = store.create(&obj(json!({ "id": "AT-101", "title": "Dup" })), &who());
        assert!(matches!(result, Err(ApiError::Conflict(_))));
    }

    #[test]
    fn title_is_required() {
        let store = Store::open_in_memory().unwrap();
        let result = store.create(&obj(json!({ "description": "no title" })), &who());
        assert!(matches!(result, Err(ApiError::BadRequest(_))));
    }

    #[test]
    fn update_missing_task_is_not_found() {
        let store = Store::open_in_memory().unwrap();
        let result = store.update("AT-999", &obj(json!({ "title": "x" })), &who());
        assert!(matches!(result, Err(ApiError::NotFound)));
    }

    #[test]
    fn new_run_request_is_pending_with_arguments() {
        let store = Store::open_in_memory().unwrap();
        let run = store.create_run("import_tasks", &json!({ "source": "notes" })).unwrap();
        assert_eq!(run.status, "pending");
        assert_eq!(run.arguments["source"], "notes");
        assert_eq!(run.result, "");
    }

    #[test]
    fn pending_filter_and_oldest_first_order() {
        let store = Store::open_in_memory().unwrap();
        let first = store.create_run("review_tasks", &json!({})).unwrap();
        let second = store.create_run("import_tasks", &json!({ "source": "x" })).unwrap();
        store.update_run(first.id, "done", Some("reviewed 3 items")).unwrap();

        let pending = store.list_runs(Some("pending")).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, second.id);
        assert_eq!(store.list_runs(None).unwrap().len(), 2);
    }

    #[test]
    fn run_status_updates_keep_previous_result_when_omitted() {
        let store = Store::open_in_memory().unwrap();
        let run = store.create_run("review_tasks", &json!({})).unwrap();
        store.update_run(run.id, "running", None).unwrap();
        let done = store.update_run(run.id, "done", Some("ok")).unwrap();
        assert_eq!(done.status, "done");
        assert_eq!(done.result, "ok");
        let again = store.update_run(run.id, "cancelled", None).unwrap();
        assert_eq!(again.result, "ok");
    }

    #[test]
    fn unknown_run_status_and_missing_run_are_rejected() {
        let store = Store::open_in_memory().unwrap();
        let run = store.create_run("review_tasks", &json!({})).unwrap();
        assert!(matches!(store.update_run(run.id, "paused", None), Err(ApiError::BadRequest(_))));
        assert!(matches!(store.update_run(9999, "done", None), Err(ApiError::NotFound)));
    }

    #[test]
    fn creating_records_who_made_it() {
        let store = Store::open_in_memory().unwrap();
        let task = store.create(&obj(json!({ "title": "Agent task" })), &agent("claude-haiku-5-5")).unwrap();
        let history = store.history(&task.id).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].action, "created");
        assert_eq!(history[0].actor_kind, "agent");
        assert_eq!(history[0].actor_name, "Claude Code");
        assert_eq!(history[0].actor_model, "claude-haiku-5-5");
        assert!(history[0].summary.contains("claude-haiku-5-5"));
    }

    #[test]
    fn updates_record_field_changes_newest_first() {
        let store = Store::open_in_memory().unwrap();
        let task = store.create(&obj(json!({ "title": "Move me" })), &who()).unwrap();
        store.update(&task.id, &obj(json!({ "status": "progress" })), &agent("model-a")).unwrap();
        store.update(&task.id, &obj(json!({ "priority": "high" })), &who()).unwrap();

        let history = store.history(&task.id).unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].changes["priority"]["to"], "high");
        assert_eq!(history[1].changes["status"]["from"], "backlog");
        assert_eq!(history[1].changes["status"]["to"], "progress");
        assert_eq!(history[1].actor_model, "model-a");
        assert!(history[1].summary.contains("status backlog → progress"));
    }

    #[test]
    fn unchanged_update_adds_no_history() {
        let store = Store::open_in_memory().unwrap();
        let task = store.create(&obj(json!({ "title": "Same" })), &who()).unwrap();
        store.update(&task.id, &obj(json!({ "title": "Same" })), &who()).unwrap();
        assert_eq!(store.history(&task.id).unwrap().len(), 1);
    }

    #[test]
    fn history_survives_deletion() {
        let store = Store::open_in_memory().unwrap();
        let task = store.create(&obj(json!({ "title": "Gone soon" })), &who()).unwrap();
        store.delete(&task.id, &agent("model-b")).unwrap();
        assert!(store.get(&task.id).unwrap().is_none());
        let history = store.history(&task.id).unwrap();
        assert_eq!(history[0].action, "deleted");
        assert_eq!(history[0].actor_model, "model-b");
    }

    #[test]
    fn workspaces_separate_tasks() {
        let store = Store::open_in_memory().unwrap();
        let home = store.create_workspace("Home renovation").unwrap();
        assert_eq!(home.id, "home-renovation");
        let in_home = store.create(&obj(json!({ "title": "Paint", "workspace": home.id })), &who()).unwrap();
        store.create(&obj(json!({ "title": "Work item" })), &who()).unwrap();

        let home_tasks = store.list(Some("home-renovation")).unwrap();
        assert_eq!(home_tasks.len(), 1);
        assert_eq!(home_tasks[0].id, in_home.id);
        assert_eq!(store.list(Some(DEFAULT_WORKSPACE)).unwrap().len(), SEED.len() + 1);
        assert_eq!(store.list(None).unwrap().len(), SEED.len() + 2);
    }

    #[test]
    fn tags_and_suggested_model_are_stored_and_recorded() {
        let store = Store::open_in_memory().unwrap();
        let task = store
            .create(&obj(json!({ "title": "Tagged", "tags": [" Important ", "on hold", "important", ""] })), &who())
            .unwrap();
        assert_eq!(task.tags, ["important", "on hold"], "tags are trimmed, lowercased and de-duplicated");

        let updated = store
            .update(&task.id, &obj(json!({ "tags": ["on hold"], "suggestedModel": " claude-haiku-5-5 " })), &agent("model-c"))
            .unwrap();
        assert_eq!(updated.tags, ["on hold"]);
        assert_eq!(updated.suggested_model, "claude-haiku-5-5");
        let history = store.history(&task.id).unwrap();
        assert_eq!(history[0].changes["tags"]["to"], "on hold");
        assert_eq!(history[0].changes["suggestedModel"]["to"], "claude-haiku-5-5");

        // Clearing the suggestion leaves no model behind.
        let cleared = store.update(&task.id, &obj(json!({ "suggestedModel": "" })), &who()).unwrap();
        assert_eq!(cleared.suggested_model, "");
        assert_eq!(store.get(&task.id).unwrap().unwrap().suggested_model, "");
    }

    #[test]
    fn blocked_by_and_relates_to_must_point_at_existing_tasks() {
        let store = Store::open_in_memory().unwrap();
        let first = store.create(&obj(json!({ "title": "First" })), &who()).unwrap();
        let second = store
            .create(&obj(json!({ "title": "Second", "blockedBy": [first.id.clone(), " "] })), &who())
            .unwrap_or_else(|e| panic!("create failed: {e:?}"));
        assert_eq!(second.blocked_by, [first.id.clone()], "blank entries are dropped");
        assert!(matches!(
            store.create(&obj(json!({ "title": "Bad", "blockedBy": ["AT-999"] })), &who()),
            Err(ApiError::BadRequest(_))
        ));
        assert!(
            matches!(store.create(&obj(json!({ "title": "Bad link", "relatesTo": [first.id.clone(), "x"] })), &who()), Err(ApiError::BadRequest(_))),
            "an unknown task id is refused"
        );
        let kept = store
            .create(&obj(json!({ "title": "Relates", "relatesTo": [first.id.clone()] })), &who())
            .unwrap();
        assert_eq!(kept.relates_to, [first.id.clone()]);
        let own = store
            .update(&kept.id, &obj(json!({ "relatesTo": [first.id.clone(), kept.id.clone()] })), &who())
            .unwrap();
        assert_eq!(own.relates_to, [first.id.clone()], "a task never relates to itself");
    }

    #[test]
    fn pick_up_note_and_links_are_stored_and_checked() {
        let store = Store::open_in_memory().unwrap();
        let task = store
            .create(&obj(json!({ "title": "Subtask", "standalone": false, "standaloneNote": "Needs the parent design first" })), &who())
            .unwrap();
        assert!(!task.standalone);
        assert_eq!(task.standalone_note, "Needs the parent design first");
        let default = store.create(&obj(json!({ "title": "Normal" })), &who()).unwrap();
        assert!(default.standalone, "tasks can be picked up alone unless said otherwise");

        let linked = store
            .update(&task.id, &obj(json!({ "links": ["https://example.com/spec", "http://example.com/a", "https://example.com/spec"] })), &who())
            .unwrap();
        assert_eq!(linked.links, ["https://example.com/spec", "http://example.com/a"]);
        for bad in ["javascript:alert(1)", "file:///C:/secret.txt", "example.com", "https://a b"] {
            assert!(
                matches!(store.update(&task.id, &obj(json!({ "links": [bad] })), &who()), Err(ApiError::BadRequest(_))),
                "{bad} should be refused"
            );
        }
        let history = store.history(&task.id).unwrap();
        assert_eq!(history[0].changes["links"]["to"], "https://example.com/spec, http://example.com/a");
    }

    #[test]
    fn comments_are_stored_newest_first_with_their_author() {
        let store = Store::open_in_memory().unwrap();
        let task = store.create(&obj(json!({ "title": "Discuss" })), &who()).unwrap();
        store.add_comment(&task.id, "First thought", &who()).unwrap();
        let second = store.add_comment(&task.id, "  Second, from an agent  ", &agent("model-c")).unwrap();
        assert_eq!(second.body, "Second, from an agent");
        assert_eq!(second.actor_model, "model-c");
        let listed = store.list_comments(&task.id).unwrap();
        assert_eq!(listed[0].body, "Second, from an agent");
        assert_eq!(listed.len(), 2);
        assert!(matches!(store.add_comment(&task.id, "   ", &who()), Err(ApiError::BadRequest(_))));
        assert!(matches!(store.add_comment("AT-999", "hello", &who()), Err(ApiError::NotFound)));
        assert!(store.add_comment(&task.id, &"x".repeat(4001), &who()).is_err());
    }

    #[test]
    fn agent_context_counts_blocked_tasks_and_flags_pick_up() {
        let store = Store::open_in_memory().unwrap();
        let blocker = store.create(&obj(json!({ "title": "Blocker", "status": "progress" })), &who()).unwrap();
        let blocked = store
            .create(&obj(json!({ "title": "Waits", "blockedBy": [blocker.id.clone()] })), &who())
            .unwrap();
        store.create(&obj(json!({ "title": "Solo", "standalone": false, "standaloneNote": "Needs its parent" })), &who()).unwrap();
        let context = store.agent_context(DEFAULT_WORKSPACE).unwrap();
        let entry = |id: &str| context["tasks"].as_array().unwrap().iter().find(|t| t["id"] == id).cloned().unwrap();
        assert_eq!(entry(&blocked.id)["openBlockers"][0], blocker.id.as_str());
        assert_eq!(entry(&blocker.id)["openBlockers"].as_array().unwrap().len(), 0);
        assert_eq!(context["summary"]["blocked"].as_u64().unwrap(), 1);
        let solo = context["tasks"].as_array().unwrap().iter().find(|t| t["title"] == "Solo").unwrap().clone();
        assert_eq!(solo["standalone"], false);
        assert_eq!(solo["standaloneNote"], "Needs its parent");

        // Once the blocker is done, the waiting task is free.
        store.update(&blocker.id, &obj(json!({ "status": "done" })), &who()).unwrap();
        let after = store.agent_context(DEFAULT_WORKSPACE).unwrap();
        assert_eq!(after["summary"]["blocked"].as_u64().unwrap(), 0);
    }

    #[test]
    fn sprint_length_comes_from_the_board_or_a_typed_value() {
        let store = Store::open_in_memory().unwrap();
        let board = store.create_workspace("Sprint board").unwrap();
        let default = store.create_sprint(&board.id, "Sprint 1", Some("2026-12-01"), None, None).unwrap();
        assert_eq!(default.status, "planned");
        assert_eq!(default.start_date, "2026-12-01");
        assert_eq!(default.end_date, "2026-12-14", "the default length is 14 days, both ends counted");

        let changed = store.set_sprint_days(&board.id, 10).unwrap();
        assert_eq!(changed.sprint_days, 10);
        let shorter = store.create_sprint(&board.id, "Sprint 2", Some("2026-12-15"), None, None).unwrap();
        assert_eq!(shorter.end_date, "2026-12-24");

        let typed_length = store.create_sprint(&board.id, "Sprint 3", Some("2026-12-25"), Some(3), None).unwrap();
        assert_eq!(typed_length.end_date, "2026-12-27");
        let typed_end = store.create_sprint(&board.id, "Sprint 4", Some("2026-12-28"), Some(3), Some("2027-01-02")).unwrap();
        assert_eq!(typed_end.end_date, "2027-01-02", "a typed end date wins over a length");

        assert!(matches!(store.create_sprint(&board.id, "Backwards", Some("2026-12-10"), None, Some("2026-12-01")), Err(ApiError::BadRequest(_))));
        assert!(matches!(store.create_sprint(&board.id, "Too long", Some("2026-12-01"), Some(91), None), Err(ApiError::BadRequest(_))));
        assert!(matches!(store.create_sprint(&board.id, "Bad date", Some("1st December"), None, None), Err(ApiError::BadRequest(_))));
        assert!(matches!(store.create_sprint(&board.id, "  ", None, None, None), Err(ApiError::BadRequest(_))));
        assert!(matches!(store.set_sprint_days(&board.id, 0), Err(ApiError::BadRequest(_))));
        assert_eq!(store.list_sprints(&board.id).unwrap().len(), 4);
    }

    #[test]
    fn only_one_sprint_runs_at_a_time_in_a_board() {
        let store = Store::open_in_memory().unwrap();
        let board = store.create_workspace("Running board").unwrap();
        let first = store.create_sprint(&board.id, "First", Some("2026-12-01"), None, None).unwrap();
        let second = store.create_sprint(&board.id, "Second", Some("2026-12-15"), None, None).unwrap();
        let mut start = Map::new();
        start.insert("status".into(), json!("active"));
        store.update_sprint(first.id, &start).unwrap();
        assert!(matches!(store.update_sprint(second.id, &start), Err(ApiError::Conflict(_))));
        let mut close = Map::new();
        close.insert("status".into(), json!("closed"));
        store.update_sprint(first.id, &close).unwrap();
        assert_eq!(store.update_sprint(second.id, &start).unwrap().status, "active");

        // Moving the start keeps the 14-day length, so the end moves with it.
        let mut move_start = Map::new();
        move_start.insert("startDate".into(), json!("2026-12-20"));
        let moved = store.update_sprint(second.id, &move_start).unwrap();
        assert_eq!(moved.start_date, "2026-12-20");
        assert_eq!(moved.end_date, "2027-01-02");
    }

    #[test]
    fn tasks_join_a_sprint_and_leave_it_when_the_sprint_is_deleted() {
        let store = Store::open_in_memory().unwrap();
        let board = store.create_workspace("Joining board").unwrap();
        let other = store.create_workspace("Other board").unwrap();
        let sprint = store.create_sprint(&board.id, "Sprint", Some("2026-12-01"), None, None).unwrap();
        let elsewhere = store.create_sprint(&other.id, "Elsewhere", Some("2026-12-01"), None, None).unwrap();

        let task = store
            .create(&obj(json!({ "title": "In sprint", "workspace": board.id.clone(), "sprint": sprint.id })), &who())
            .unwrap();
        assert_eq!(task.sprint, Some(sprint.id));
        assert_eq!(store.list_sprints(&board.id).unwrap()[0].tasks, 1);
        assert!(matches!(
            store.create(&obj(json!({ "title": "Wrong board", "workspace": board.id.clone(), "sprint": elsewhere.id })), &who()),
            Err(ApiError::BadRequest(_))
        ));

        let moved = store.update(&task.id, &obj(json!({ "sprint": null })), &who()).unwrap();
        assert_eq!(moved.sprint, None);
        assert_eq!(store.history(&task.id).unwrap()[0].changes["sprint"]["to"], Value::Null);
        store.update(&task.id, &obj(json!({ "sprint": sprint.id })), &who()).unwrap();
        store.delete_sprint(sprint.id).unwrap();
        assert_eq!(store.get(&task.id).unwrap().unwrap().sprint, None, "deleting a sprint keeps its tasks");
        assert!(matches!(store.delete_sprint(sprint.id), Err(ApiError::NotFound)));
    }

    #[test]
    fn agent_context_lists_the_board_sprints_and_each_tasks_sprint() {
        let store = Store::open_in_memory().unwrap();
        let board = store.create_workspace("Context board").unwrap();
        let sprint = store.create_sprint(&board.id, "Running", Some("2026-12-01"), None, None).unwrap();
        store.create(&obj(json!({ "title": "Sprinted", "workspace": board.id.clone(), "sprint": sprint.id })), &who()).unwrap();
        let context = store.agent_context(&board.id).unwrap();
        assert_eq!(context["sprints"][0]["name"], "Running");
        let task = context["tasks"].as_array().unwrap().iter().find(|t| t["title"] == "Sprinted").unwrap().clone();
        assert_eq!(task["sprint"], sprint.id);
    }

    #[test]
    fn views_save_a_layout_and_filters_per_board() {
        let store = Store::open_in_memory().unwrap();
        let board = store.create_workspace("View board").unwrap();
        let other = store.create_workspace("Other views").unwrap();
        let saved = store
            .create_view(&board.id, Some("  Sprint 4 list "), Some("list"), Some(&json!({ "sprint": "3", "priority": "high", "colour": "pink", "tag": "" })))
            .unwrap();
        assert_eq!(saved.name, "Sprint 4 list");
        assert_eq!(saved.layout, "list");
        assert_eq!(saved.filters.get("sprint").map(String::as_str), Some("3"));
        assert!(!saved.filters.contains_key("colour"), "unknown filters are dropped");
        assert!(!saved.filters.contains_key("tag"), "empty filters are not saved");
        assert_eq!(store.list_views(&board.id).unwrap().len(), 1);
        assert!(store.list_views(&other.id).unwrap().is_empty(), "views belong to one board");

        let mut change = Map::new();
        change.insert("layout".into(), json!("board"));
        let updated = store.update_view(saved.id, &change).unwrap();
        assert_eq!(updated.layout, "board");
        assert_eq!(updated.filters.get("priority").map(String::as_str), Some("high"), "filters are kept when not sent");
        let mut replace = Map::new();
        replace.insert("filters".into(), json!({}));
        assert!(store.update_view(saved.id, &replace).unwrap().filters.is_empty());

        assert!(matches!(store.create_view(&board.id, Some("Bad"), Some("calendar"), None), Err(ApiError::BadRequest(_))));
        assert!(matches!(store.create_view(&board.id, Some("   "), Some("board"), None), Err(ApiError::BadRequest(_))));
        assert!(matches!(store.create_view(&board.id, Some(&"x".repeat(21)), Some("board"), None), Err(ApiError::BadRequest(_))), "names are at most 20 characters");
        assert!(store.create_view(&board.id, Some(&"x".repeat(20)), Some("board"), None).is_ok());
        assert!(matches!(store.create_view("nope", Some("x"), Some("board"), None), Err(ApiError::NotFound)));
        store.delete_view(saved.id).unwrap();
        assert!(matches!(store.delete_view(saved.id), Err(ApiError::NotFound)));
    }

    #[test]
    fn settings_default_to_allowed_and_can_be_changed() {
        let store = Store::open_in_memory().unwrap();
        let defaults = store.get_settings().unwrap();
        assert!(AGENT_SETTINGS.iter().all(|(key, _)| defaults[*key]), "agents keep their current rights by default");
        let changed = store.set_setting("agents_comment", false).unwrap();
        assert!(!changed["agents_comment"]);
        assert!(changed["agents_plan_sprints"], "other settings are untouched");
        assert!(store.get_settings().unwrap()["agents_comment"] == false, "the change is kept");
        assert!(matches!(store.set_setting("made_up", true), Err(ApiError::BadRequest(_))));
    }

    #[test]
    fn agent_board_requests_wait_for_approval() {
        let store = Store::open_in_memory().unwrap();
        let dir = std::env::temp_dir().join(format!("clankban-request-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let folder = std::fs::canonicalize(&dir).unwrap().to_string_lossy().trim_start_matches(r"\\?\").to_string();
        let boards_before = store.list_workspaces().unwrap().len();

        // Asking creates a request, not a board.
        let garden = match store.request_board("Garden", None, "agent").unwrap() {
            BoardRequestResult::Pending { request, is_new } => {
                assert!(is_new);
                request
            }
            other => panic!("expected a pending request, got {other:?}"),
        };
        assert_eq!(garden.status, "pending");
        assert_eq!(store.list_workspaces().unwrap().len(), boards_before, "no board until a person approves");

        // The same folder asked twice gives the same pending request.
        let first = match store.request_board("", Some(&folder), "agent").unwrap() {
            BoardRequestResult::Pending { request, is_new: true } => request,
            other => panic!("expected a new request, got {other:?}"),
        };
        match store.request_board("", Some(&folder), "agent").unwrap() {
            BoardRequestResult::Pending { request, is_new } => {
                assert!(!is_new);
                assert_eq!(request.id, first.id);
            }
            other => panic!("expected the same request, got {other:?}"),
        }

        // Rejecting creates nothing, and a decided request cannot be decided again.
        assert_eq!(store.reject_board_request(garden.id).unwrap().status, "rejected");
        assert!(matches!(store.reject_board_request(garden.id), Err(ApiError::Conflict(_))));
        assert_eq!(store.list_workspaces().unwrap().len(), boards_before);

        // Approving creates the board with the folder suggested, not connected.
        let (decided, board) = store.approve_board_request(first.id).unwrap();
        assert_eq!(decided.status, "approved");
        assert_eq!(decided.workspace_id.as_deref(), Some(board.id.as_str()));
        assert_eq!(board.suggested_root.as_deref(), Some(folder.as_str()));
        assert_eq!(board.project_root, None);
        assert_eq!(store.list_workspaces().unwrap().len(), boards_before + 1);

        // Once the folder has a board, asking for it returns that board.
        match store.request_board("", Some(&folder), "agent").unwrap() {
            BoardRequestResult::Existing(existing) => assert_eq!(existing.id, board.id),
            other => panic!("expected the existing board, got {other:?}"),
        }
        assert!(store.list_board_requests(true).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folders_resolve_to_their_most_specific_board() {
        let store = Store::open_in_memory().unwrap();
        let base = std::env::temp_dir().join(format!("clankban-match-{}", std::process::id()));
        let nested = base.join("sub");
        std::fs::create_dir_all(&nested).unwrap();
        // Compare against canonical paths, as the store saves them (without the \\?\ prefix).
        let plain = |p: &std::path::Path| {
            let text = std::fs::canonicalize(p).unwrap().to_string_lossy().into_owned();
            text.trim_start_matches(r"\\?\").to_string()
        };
        let (base_path, nested_path) = (plain(&base), plain(&nested));
        let outer = store.create_workspace("Outer").unwrap();
        let inner = store.create_workspace("Inner").unwrap();
        store.set_project_root(&outer.id, Some(&base_path)).unwrap();
        store.set_project_root(&inner.id, Some(&nested_path)).unwrap();

        let owner = |folder: &str| store.workspace_for_folder(folder).unwrap().map(|w| w.id);
        assert_eq!(owner(&format!("{nested_path}\\src")), Some(inner.id.clone()));
        assert_eq!(owner(&base_path), Some(outer.id.clone()));
        assert_eq!(owner(&format!("{}\\", base_path.to_uppercase())), Some(outer.id.clone()));
        assert_eq!(owner(&format!("{base_path}-other")), None, "a folder that only shares a prefix is not inside");

        assert_eq!(store.workspace_claiming_folder(&base_path).unwrap().map(|w| w.id), Some(outer.id.clone()));
        assert!(store.workspace_claiming_folder(&format!("{base_path}-other")).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn dashboard_counts_each_board_and_covers_all_of_them() {
        let store = Store::open_in_memory().unwrap();
        let garden = store.create_workspace("Garden").unwrap();
        store.create(&obj(json!({ "title": "High", "priority": "high", "workspace": garden.id })), &who()).unwrap();
        store.create(&obj(json!({ "title": "Late", "due": "2000-01-01", "workspace": garden.id })), &who()).unwrap();
        let blocker = store.create(&obj(json!({ "title": "Blocker" })), &who()).unwrap();
        store.create(&obj(json!({ "title": "Waits", "blockedBy": [blocker.id.clone()] })), &who()).unwrap();

        let all = store.dashboard(None).unwrap();
        assert_eq!(all["scope"], "all");
        assert!(all["totals"]["overdue"].as_u64().unwrap() >= 1);
        assert_eq!(all["workspaces"].as_array().unwrap().len(), store.list_workspaces().unwrap().len());
        let garden_card = all["workspaces"].as_array().unwrap().iter().find(|w| w["id"] == garden.id.as_str()).unwrap().clone();
        assert_eq!(garden_card["highPriority"], 1);
        assert_eq!(garden_card["overdue"], 1);
        assert_eq!(garden_card["byStatus"]["backlog"], 2);

        let one = store.dashboard(Some(&garden.id)).unwrap();
        assert_eq!(one["workspaces"].as_array().unwrap().len(), 1);
        assert_eq!(one["totals"]["active"], 2);
        assert!(one["upcoming"].as_array().unwrap().iter().all(|u| u["workspace"] == garden.id.as_str()));
        assert!(one["recent"].as_array().unwrap().iter().all(|e| e["taskWorkspace"] == garden.id.as_str()));
    }

    #[test]
    fn recent_history_is_newest_first_and_stays_in_its_workspace() {
        let store = Store::open_in_memory().unwrap();
        let garden = store.create_workspace("Garden").unwrap();
        let mow = store.create(&obj(json!({ "title": "Mow", "workspace": garden.id })), &who()).unwrap();
        store.create(&obj(json!({ "title": "Elsewhere" })), &who()).unwrap();
        store.update(&mow.id, &obj(json!({ "status": "progress" })), &agent("model-c")).unwrap();

        let recent = store.recent_history(Some(&garden.id), 10).unwrap();
        assert!(recent.iter().all(|e| e.entry.task_id == mow.id));
        assert_eq!(recent[0].entry.action, "updated");
        assert_eq!(recent[0].entry.actor_model, "model-c");
        assert_eq!(recent[0].task_title, "Mow");
        assert_eq!(recent.last().unwrap().entry.action, "created");
        assert_eq!(store.recent_history(Some(&garden.id), 1).unwrap().len(), 1);
        assert!(store.recent_history(Some(DEFAULT_WORKSPACE), 50).unwrap().iter().all(|e| e.entry.task_id != mow.id));
    }

    #[test]
    fn moving_a_task_is_recorded() {
        let store = Store::open_in_memory().unwrap();
        let ws = store.create_workspace("Garden").unwrap();
        let task = store.create(&obj(json!({ "title": "Mow" })), &who()).unwrap();
        store.update(&task.id, &obj(json!({ "workspace": ws.id })), &agent("model-c")).unwrap();
        let history = store.history(&task.id).unwrap();
        assert_eq!(history[0].changes["workspace"]["to"], "garden");
    }

    #[test]
    fn unknown_workspace_is_rejected() {
        let store = Store::open_in_memory().unwrap();
        let result = store.create(&obj(json!({ "title": "Lost", "workspace": "nowhere" })), &who());
        assert!(matches!(result, Err(ApiError::BadRequest(_))));
    }

    #[test]
    fn workspace_rules() {
        let store = Store::open_in_memory().unwrap();
        assert!(matches!(store.delete_workspace(DEFAULT_WORKSPACE), Err(ApiError::BadRequest(_))));
        let ws = store.create_workspace("Temp").unwrap();
        assert!(matches!(store.create_workspace("temp"), Err(ApiError::Conflict(_))));
        store.create(&obj(json!({ "title": "Blocker", "workspace": ws.id })), &who()).unwrap();
        assert!(matches!(store.delete_workspace(&ws.id), Err(ApiError::Conflict(_))));
        store.rename_workspace(&ws.id, "Renamed").unwrap();
        assert_eq!(store.list_workspaces().unwrap().iter().find(|w| w.id == ws.id).unwrap().name, "Renamed");
    }

    #[test]
    fn agent_context_is_scoped_to_its_workspace() {
        let store = Store::open_in_memory().unwrap();
        let ws = store.create_workspace("Side project").unwrap();
        store.create(&obj(json!({ "title": "Only here", "workspace": ws.id })), &who()).unwrap();
        let context = store.agent_context(&ws.id).unwrap();
        assert_eq!(context["workspace"], ws.id);
        assert_eq!(context["summary"]["active"], 1);
    }

    #[test]
    fn existing_database_is_backed_up_and_migrated() {
        let dir = std::env::temp_dir().join(format!("clankban-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        {
            // A database as written by the previous version: no workspace column yet.
            let old = Connection::open(dir.join(DB_FILE)).unwrap();
            old.execute_batch(
                "CREATE TABLE tasks (id TEXT PRIMARY KEY, title TEXT NOT NULL, description TEXT DEFAULT '', type TEXT NOT NULL,
                 priority TEXT NOT NULL, status TEXT NOT NULL, due TEXT, created TEXT NOT NULL, subtasks TEXT NOT NULL DEFAULT '[]',
                 updated TEXT NOT NULL, parent_id TEXT);
                 INSERT INTO tasks (id, title, type, priority, status, created, updated) VALUES ('AT-7', 'Old task', 'task', 'low', 'backlog', '2026-01-01', '2026-01-01');",
            )
            .unwrap();
        }
        let store = Store::open(&dir).unwrap();
        let task = store.get("AT-7").unwrap().unwrap();
        assert_eq!(task.workspace, DEFAULT_WORKSPACE);
        let backups: Vec<_> = std::fs::read_dir(dir.join("backups")).unwrap().collect();
        assert_eq!(backups.len(), 1);
        drop(store);
        // Reopening an already-migrated database makes no further backup.
        let _ = Store::open(&dir).unwrap();
        assert_eq!(std::fs::read_dir(dir.join("backups")).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn task_docs_are_stored_and_changes_are_recorded() {
        let store = Store::open_in_memory().unwrap();
        let task = store
            .create(&obj(json!({ "title": "Doc task", "docs": ["docs/TASKS.md", "README.md"] })), &who())
            .unwrap();
        assert_eq!(task.docs, ["docs/TASKS.md", "README.md"]);
        store.update(&task.id, &obj(json!({ "docs": ["docs/ARCHITECTURE.md"] })), &who()).unwrap();
        let history = store.history(&task.id).unwrap();
        assert_eq!(history[0].changes["docs"]["to"], "docs/ARCHITECTURE.md");
        assert_eq!(store.get(&task.id).unwrap().unwrap().docs, ["docs/ARCHITECTURE.md"]);
    }

    #[test]
    fn suggestion_waits_for_acceptance_and_can_be_dismissed() {
        let store = Store::open_in_memory().unwrap();
        let folder = std::env::temp_dir();
        let proposed = store
            .suggest_project_root(DEFAULT_WORKSPACE, folder.to_str().unwrap(), "claude-code")
            .unwrap();
        let before = store.list_workspaces().unwrap().into_iter().find(|w| w.id == DEFAULT_WORKSPACE).unwrap();
        assert_eq!(before.suggested_root.as_deref(), Some(proposed.as_str()));
        assert_eq!(before.suggested_by.as_deref(), Some("claude-code"));
        assert_eq!(before.project_root, None, "a suggestion must not connect the folder");

        store.dismiss_suggestion(DEFAULT_WORKSPACE).unwrap();
        let dismissed = store.list_workspaces().unwrap().into_iter().find(|w| w.id == DEFAULT_WORKSPACE).unwrap();
        assert_eq!(dismissed.suggested_root, None);
    }

    #[test]
    fn accepting_a_folder_clears_the_suggestion() {
        let store = Store::open_in_memory().unwrap();
        let folder = std::env::temp_dir();
        store.suggest_project_root(DEFAULT_WORKSPACE, folder.to_str().unwrap(), "claude-code").unwrap();
        store.set_project_root(DEFAULT_WORKSPACE, Some(folder.to_str().unwrap())).unwrap();
        let w = store.list_workspaces().unwrap().into_iter().find(|w| w.id == DEFAULT_WORKSPACE).unwrap();
        assert!(w.project_root.is_some());
        assert_eq!(w.suggested_root, None);
        assert_eq!(w.suggested_by, None);
    }

    #[test]
    fn suggestion_must_be_an_existing_folder() {
        let store = Store::open_in_memory().unwrap();
        assert!(matches!(
            store.suggest_project_root(DEFAULT_WORKSPACE, "/no/such/folder", "agent"),
            Err(ApiError::BadRequest(_))
        ));
    }

    #[test]
    fn project_folder_is_stored_for_a_workspace_and_must_exist() {
        let store = Store::open_in_memory().unwrap();
        let folder = std::env::temp_dir();
        let stored = store
            .set_project_root(DEFAULT_WORKSPACE, Some(folder.to_str().unwrap()))
            .unwrap()
            .unwrap();
        assert_eq!(store.project_root(DEFAULT_WORKSPACE).unwrap().as_deref(), Some(stored.as_str()));
        let default = store.list_workspaces().unwrap().into_iter().find(|w| w.id == DEFAULT_WORKSPACE).unwrap();
        assert_eq!(default.project_root.as_deref(), Some(stored.as_str()));
        assert!(matches!(
            store.set_project_root(DEFAULT_WORKSPACE, Some("/definitely/not/a/folder")),
            Err(ApiError::BadRequest(_))
        ));
        assert_eq!(store.set_project_root(DEFAULT_WORKSPACE, None).unwrap(), None);
        assert_eq!(store.project_root(DEFAULT_WORKSPACE).unwrap(), None);
    }
}
