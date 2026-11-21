//! SQLite-backed store: tasks, workspaces, change history, and the agent run queue.
//! Keeps the schema, seed data, and normalisation rules of the original Node `server.js`,
//! so existing `clankban.sqlite` files open unchanged.

use chrono::{Duration, SecondsFormat, Utc};
use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

pub const DB_FILE: &str = "clankban.sqlite";
pub const SCHEMA_VERSION: &str = "1.1";
pub const DEFAULT_WORKSPACE: &str = "default";

const SELECT_COLUMNS: &str = "id, title, description, type, priority, status, due, created, subtasks, updated, parent_id, workspace_id, docs";

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
    })
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
                docs TEXT NOT NULL DEFAULT '[]'
            );
            CREATE TABLE IF NOT EXISTS workspaces (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                created TEXT NOT NULL,
                project_root TEXT,
                suggested_root TEXT,
                suggested_by TEXT
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
        conn.execute(
            "INSERT INTO tasks (id, title, description, type, priority, status, due, created, subtasks, updated, parent_id, workspace_id, docs)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                task.id, task.title, task.description, task.kind, task.priority, task.status,
                task.due, task.created, subtasks, task.updated, task.parent_id, task.workspace, docs
            ],
        )?;
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
        conn.execute(
            "UPDATE tasks SET title = ?2, description = ?3, type = ?4, priority = ?5, status = ?6,
                due = ?7, subtasks = ?8, updated = ?9, parent_id = ?10, workspace_id = ?11, docs = ?12
             WHERE id = ?1",
            params![
                id, task.title, task.description, task.kind, task.priority, task.status,
                task.due, subtasks, task.updated, task.parent_id, task.workspace, docs
            ],
        )?;
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
            record_history(&conn, id, "deleted", actor, &Map::new())?;
        }
        Ok(())
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
    pub fn recent_history(&self, workspace: &str, limit: usize) -> Result<Vec<RecentHistoryEntry>, ApiError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT h.id, h.task_id, h.at, h.action, h.actor_kind, h.actor_name, h.actor_model, h.changes, h.summary, t.title
             FROM task_history h JOIN tasks t ON t.id = h.task_id
             WHERE t.workspace_id = ?1
             ORDER BY h.id DESC LIMIT ?2",
        )?;
        let entries = stmt
            .query_map(params![workspace, limit as i64], |row| {
                Ok(RecentHistoryEntry {
                    entry: row_to_history(row)?,
                    task_title: row.get::<_, String>(9)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(entries)
    }

    pub fn list_workspaces(&self) -> Result<Vec<Workspace>, ApiError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT w.id, w.name, w.created,
                    (SELECT COUNT(*) FROM tasks t WHERE t.workspace_id = w.id),
                    w.project_root, w.suggested_root, w.suggested_by
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

        Ok(serde_json::json!({
            "workspace": workspace,
            "schemaVersion": SCHEMA_VERSION,
            "summary": { "active": active.len(), "blocked": 0, "dueSoon": due_soon },
            "tasks": active.iter().map(|t| serde_json::json!({
                "id": t.id,
                "title": t.title,
                "type": t.kind,
                "priority": t.priority,
                "status": t.status,
                "due": t.due,
                "subtasks": t.subtasks,
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
    fn recent_history_is_newest_first_and_stays_in_its_workspace() {
        let store = Store::open_in_memory().unwrap();
        let garden = store.create_workspace("Garden").unwrap();
        let mow = store.create(&obj(json!({ "title": "Mow", "workspace": garden.id })), &who()).unwrap();
        store.create(&obj(json!({ "title": "Elsewhere" })), &who()).unwrap();
        store.update(&mow.id, &obj(json!({ "status": "progress" })), &agent("model-c")).unwrap();

        let recent = store.recent_history(&garden.id, 10).unwrap();
        assert!(recent.iter().all(|e| e.entry.task_id == mow.id));
        assert_eq!(recent[0].entry.action, "updated");
        assert_eq!(recent[0].entry.actor_model, "model-c");
        assert_eq!(recent[0].task_title, "Mow");
        assert_eq!(recent.last().unwrap().entry.action, "created");
        assert_eq!(store.recent_history(&garden.id, 1).unwrap().len(), 1);
        assert!(store.recent_history(DEFAULT_WORKSPACE, 50).unwrap().iter().all(|e| e.entry.task_id != mow.id));
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
