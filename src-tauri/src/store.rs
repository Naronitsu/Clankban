//! SQLite-backed task store. Mirrors the schema, seed data and normalisation
//! rules of the original Node `server.js` so existing `clankban.sqlite` files
//! open unchanged.

use chrono::{Duration, SecondsFormat, Utc};
use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

pub const DB_FILE: &str = "clankban.sqlite";
pub const SCHEMA_VERSION: &str = "1.0";
pub const WORKSPACE: &str = "atlas";

const SELECT_COLUMNS: &str =
    "id, title, description, type, priority, status, due, created, subtasks, updated, parent_id";

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
    })
}

fn read_all(conn: &Connection) -> Result<Vec<Task>, ApiError> {
    let mut stmt = conn.prepare(&format!("SELECT {SELECT_COLUMNS} FROM tasks ORDER BY created DESC"))?;
    let tasks = stmt.query_map([], row_to_task)?.collect::<rusqlite::Result<Vec<_>>>()?;
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

/// `AT-<max numeric suffix + 1>`, same as the original server.
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
    })
}

fn next_id_from(conn: &Connection) -> Result<String, ApiError> {
    Ok(next_id(&read_all(conn)?))
}

impl Store {
    pub fn open(data_dir: &Path) -> Result<Self, ApiError> {
        std::fs::create_dir_all(data_dir).map_err(|e| ApiError::Internal(e.to_string()))?;
        let conn = Connection::open(data_dir.join(DB_FILE))?;
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
                parent_id TEXT
            );",
        )?;

        let has_parent_id = {
            let mut stmt = conn.prepare("PRAGMA table_info(tasks)")?;
            let names = stmt
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            names.iter().any(|n| n == "parent_id")
        };
        if !has_parent_id {
            conn.execute_batch("ALTER TABLE tasks ADD COLUMN parent_id TEXT;")?;
        }

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS run_requests (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                skill TEXT NOT NULL,
                arguments TEXT NOT NULL DEFAULT '{}',
                status TEXT NOT NULL,
                result TEXT NOT NULL DEFAULT '',
                created TEXT NOT NULL,
                updated TEXT NOT NULL
            );",
        )?;

        let count: i64 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))?;
        if count == 0 {
            let now = now_iso();
            for (id, title, description, kind, priority, status, due, created, subtasks) in SEED {
                conn.execute(
                    "INSERT INTO tasks (id, title, description, type, priority, status, due, created, subtasks, updated, parent_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL)",
                    params![id, title, description, kind, priority, status, due, created, subtasks, now],
                )?;
            }
        }

        Ok(Store { conn: Mutex::new(conn) })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn list(&self) -> Result<Vec<Task>, ApiError> {
        read_all(&self.lock())
    }

    pub fn get(&self, id: &str) -> Result<Option<Task>, ApiError> {
        read_one(&self.lock(), id)
    }

    pub fn create(&self, input: &Map<String, Value>) -> Result<Task, ApiError> {
        let conn = self.lock();
        let id = match input.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            Some(id) => id.to_string(),
            None => next_id_from(&conn)?,
        };
        if read_one(&conn, &id)?.is_some() {
            return Err(ApiError::Conflict(format!("Task {id} already exists")));
        }

        let task = build_task(input, None, &id, &now_iso())?;
        let subtasks = serde_json::to_string(&task.subtasks).map_err(|e| ApiError::Internal(e.to_string()))?;
        conn.execute(
            "INSERT INTO tasks (id, title, description, type, priority, status, due, created, subtasks, updated, parent_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                task.id, task.title, task.description, task.kind, task.priority, task.status,
                task.due, task.created, subtasks, task.updated, task.parent_id
            ],
        )?;
        Ok(task)
    }

    pub fn update(&self, id: &str, input: &Map<String, Value>) -> Result<Task, ApiError> {
        let conn = self.lock();
        let old = read_one(&conn, id)?.ok_or(ApiError::NotFound)?;
        let task = build_task(input, Some(&old), id, &now_iso())?;
        let subtasks = serde_json::to_string(&task.subtasks).map_err(|e| ApiError::Internal(e.to_string()))?;
        conn.execute(
            "UPDATE tasks SET title = ?2, description = ?3, type = ?4, priority = ?5, status = ?6,
                due = ?7, subtasks = ?8, updated = ?9, parent_id = ?10
             WHERE id = ?1",
            params![
                id, task.title, task.description, task.kind, task.priority, task.status,
                task.due, subtasks, task.updated, task.parent_id
            ],
        )?;
        Ok(task)
    }

    pub fn delete(&self, id: &str) -> Result<(), ApiError> {
        self.lock().execute("DELETE FROM tasks WHERE id = ?1", [id])?;
        Ok(())
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

    /// Compact active-work payload for planning agents.
    pub fn agent_context(&self) -> Result<Value, ApiError> {
        let tasks = self.list()?;
        let week_out = (Utc::now() + Duration::days(7)).format("%Y-%m-%d").to_string();
        let active: Vec<&Task> = tasks.iter().filter(|t| t.status != "done").collect();
        let due_soon = active
            .iter()
            .filter(|t| !t.due.is_empty() && t.due <= week_out)
            .count();

        Ok(serde_json::json!({
            "workspace": WORKSPACE,
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

    #[test]
    fn seeds_empty_database() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.list().unwrap().len(), SEED.len());
    }

    #[test]
    fn next_id_follows_highest_existing() {
        let store = Store::open_in_memory().unwrap();
        let task = store.create(&obj(serde_json::json!({ "title": "New" }))).unwrap();
        assert_eq!(task.id, "AT-116");
        assert_eq!(task.status, "backlog");
    }

    #[test]
    fn todo_status_is_normalised_to_backlog() {
        let store = Store::open_in_memory().unwrap();
        let task = store
            .create(&obj(serde_json::json!({ "title": "Imported", "status": "todo" })))
            .unwrap();
        assert_eq!(task.status, "backlog");
        assert_eq!(store.get(&task.id).unwrap().unwrap().status, "backlog");
    }

    #[test]
    fn parent_is_inferred_from_legacy_description() {
        let store = Store::open_in_memory().unwrap();
        let task = store
            .create(&obj(serde_json::json!({ "title": "Child", "description": "Parent: AT-101" })))
            .unwrap();
        assert_eq!(store.get(&task.id).unwrap().unwrap().parent_id.as_deref(), Some("AT-101"));
    }

    #[test]
    fn explicit_parent_id_is_stored() {
        let store = Store::open_in_memory().unwrap();
        let task = store
            .create(&obj(serde_json::json!({ "title": "Child", "parentId": "AT-104" })))
            .unwrap();
        assert_eq!(task.parent_id.as_deref(), Some("AT-104"));
    }

    #[test]
    fn update_keeps_created_and_inferred_parent() {
        let store = Store::open_in_memory().unwrap();
        let created = store
            .create(&obj(serde_json::json!({ "title": "Child", "description": "Epic: AT-101", "created": "2026-01-02" })))
            .unwrap();
        let updated = store
            .update(&created.id, &obj(serde_json::json!({ "title": "Renamed" })))
            .unwrap();
        assert_eq!(updated.title, "Renamed");
        assert_eq!(updated.created, "2026-01-02");
        assert_eq!(updated.parent_id.as_deref(), Some("AT-101"));
    }

    #[test]
    fn duplicate_id_conflicts() {
        let store = Store::open_in_memory().unwrap();
        let result = store.create(&obj(serde_json::json!({ "id": "AT-101", "title": "Dup" })));
        assert!(matches!(result, Err(ApiError::Conflict(_))));
    }

    #[test]
    fn title_is_required() {
        let store = Store::open_in_memory().unwrap();
        let result = store.create(&obj(serde_json::json!({ "description": "no title" })));
        assert!(matches!(result, Err(ApiError::BadRequest(_))));
    }

    #[test]
    fn update_missing_task_is_not_found() {
        let store = Store::open_in_memory().unwrap();
        let result = store.update("AT-999", &obj(serde_json::json!({ "title": "x" })));
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
}
