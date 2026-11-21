# Clankban architecture

Status: implemented local desktop foundation

Clankban is a self-contained Windows desktop application that gives humans a visual board and gives local coding agents a compact, structured interface to the same work state.

## Goals

- Make agent work visible and understandable to a human.
- Let humans create, organise, prioritise, review, and delete work.
- Let local agents read and mutate work without copying a large board into context.
- Keep the application useful offline and local-first.
- Preserve stable identifiers and an auditable task history as the system grows.

## Runtime shape

```text
┌──────────────────────────────┐
│ Electron desktop window       │
│ Board UI: HTML/CSS/JS         │
└──────────────┬───────────────┘
               │ HTTP 127.0.0.1:4187
┌──────────────▼───────────────┐
│ Node local service            │
│ REST API + SQLite             │
│ app userData/clankban.sqlite  │
└──────────────┬───────────────┘
               │
┌──────────────▼───────────────┐
│ MCP stdio bridge              │
│ get_agent_context             │
│ get_task / create_task        │
│ update_task                   │
└──────────────┬───────────────┘
               │
     Claude Code, Codex, other
     local MCP-capable agents
```

The desktop app starts the API on `127.0.0.1:4187`. The MCP bridge is launched by an agent as a stdio process and forwards requests to that API when `CLANKBAN_API_URL` is set. This keeps agent access separate from the UI while preserving one live source of truth.

## Packaging

- Desktop runtime: Rust with Tauri 2 (`src-tauri/`). The board UI in `ui/` runs in the Tauri WebView and talks to the local API.
- Single binary, three modes: the desktop window (default), `--api` (headless API), and `--mcp` (stdio MCP bridge).
- The Rust port keeps the same SQLite schema, routes, JSON payloads, and MCP tools as the original Node/Electron version.
- Installer: Tauri NSIS bundle (`cargo tauri build`).
- Platform target: Windows x64 for the first release.
- Data location: `%APPDATA%\clankban\clankban.sqlite` (same folder the Electron build used, so existing data opens unchanged). Override with `CLANKBAN_DATA_DIR`.
- The installer is currently unsigned; SmartScreen may warn on first launch.
- The original Electron/Node runtime has been removed. Its behaviour is preserved in the Rust port.
- Browser-only fallback remains available by opening `ui/index.html`, but it uses localStorage and is not the canonical desktop runtime.

## Data model

The current SQLite `tasks` table contains:

| Field | Meaning |
| --- | --- |
| `id` | Stable identifier such as `AT-131`. Primary key. |
| `title` | Short human-readable work item title. |
| `description` | Context, acceptance notes, evidence, and decisions. |
| `type` | `epic`, `task`, `subtask`, or `bug`. |
| `priority` | `high`, `medium`, or `low`. |
| `status` | `backlog`, `progress`, `review`, or `done`. |
| `due` | Optional ISO date. |
| `created` | ISO date. |
| `subtasks` | JSON array for embedded checklist items. |
| `updated` | ISO timestamp for the latest mutation. |
| `parent_id` | Optional stable ID of a parent task. |
| `workspace_id` | The workspace the task belongs to. Defaults to `default`. |
| `docs` | JSON array of Markdown paths, relative to the project folder, that the task relates to. |

`workspaces` holds `id`, `name`, and `created`. The `default` workspace always exists and cannot be deleted. A workspace can only be deleted once it has no tasks.

`task_history` is append-only. Each row records the task, the time, the action (`created`, `updated`, or `deleted`), the actor (`kind`, `name`, `model`), the field changes as `{field: {from, to}}`, and a readable summary. History survives deletion of its task, so the record of what happened is kept.

The actor comes from request headers: `X-Clankban-Actor` (`human` for the board, `agent` for MCP clients, `api` by default), `X-Clankban-Client` (the client name; for MCP clients, the name and version from the handshake), and `X-Clankban-Model` (the model that made the change, when the client reports it).

`todo` is accepted as an input alias and returned as `backlog` so imported tasks never become invisible to the board.

Parent-child relationships should use `parent_id`/`parentId`. The server currently also infers a parent from legacy description text such as `Parent: AT-131` or `Epic: AT-122` so older imports remain visible. New integrations must send `parentId` explicitly.

## API contract

Base URL: `http://127.0.0.1:4187`

| Method | Route | Purpose |
| --- | --- | --- |
| GET | `/api/health` | Liveness check. |
| GET | `/api/tasks` | Read all tasks. |
| GET | `/api/tasks/:id` | Read one task. |
| GET | `/api/agent-context` | Compact active-work context. |
| POST | `/api/tasks` | Create a task. |
| PATCH | `/api/tasks/:id` | Update a task. |
| DELETE | `/api/tasks/:id` | Delete a task. |
| GET | `/api/tasks/:id/history` | Change history for a task, newest first. |
| GET | `/api/workspaces` | List workspaces with their task counts. |
| POST | `/api/workspaces` | Create a workspace. Body: `{name}`. The id is derived from the name. |
| PATCH | `/api/workspaces/:id` | Rename a workspace. Body: `{name}`. |
| DELETE | `/api/workspaces/:id` | Delete an empty workspace. The default workspace cannot be deleted. |
| PUT | `/api/workspaces/:id/project` | Set or clear the workspace's project folder. Body: `{root}`, or `{root: null}` to clear. Used by the board's folder picker. |
| GET | `/api/workspaces/:id/docs` | List the Markdown files in the project folder: relative path, first heading, size. |
| GET | `/api/workspaces/:id/docs/read?path=` | Read one Markdown file. Paths outside the folder, non-Markdown files, and files over 1 MB are refused. |
| GET | `/api/skills` | List the built-in agent skills (name, description, arguments). |
| GET | `/api/run-requests?status=pending` | List skill run requests, oldest first. `status` is optional. |
| POST | `/api/run-requests` | Ask a connected agent to run a skill. Body: `{skill, arguments}`. Unknown skills and missing required arguments return 400. |
| PATCH | `/api/run-requests/:id` | Update a run request. Body: `{status, result?}`, where status is `pending`, `running`, `done`, `failed`, or `cancelled`. |

`/api/agent-context` intentionally returns a reduced payload: workspace, schema version, counts, and active tasks with IDs, titles, type, priority, status, due date, and subtasks. Agents should use it first, then request individual tasks only when needed.

## MCP contract

The local stdio server exposes:

- `get_agent_context(status?, workspace?)` — compact planning context for one workspace (default `default`).
- `get_task(id)` — focused task lookup.
- `get_task_history(id)` — who changed the task, when, which fields, and the model they reported.
- `create_task(title, description?, type?, priority?, status?, due?, parentId?, subtasks?, workspace?, model?)`.
- `update_task(id, title?, description?, priority?, status?, due?, parentId?, workspace?, model?)`. Setting `workspace` moves the task.
- `list_workspaces()` and `create_workspace(name, model?)`.
- `list_project_docs(workspace?)` and `read_project_doc(path, workspace?)`. Read-only views of the workspace's project folder.

## Project docs

Each workspace can point at a project folder, chosen by a person through the board's folder picker. The board and the agent tools only read from it, and never write to it. Reads are confined to the folder: paths are relative, `..` and absolute paths are refused, symbolic links are not followed, and only `.md` files are returned. Folders such as `node_modules`, `target`, `dist`, and hidden folders are skipped.

Tasks link to documents with the `docs` list. An agent passes the relevant paths when it creates or updates a task, and the task drawer shows them as links that open the document in the Docs tab. The list only points at files and never decides which files exist.

The MCP client's name and version come from the handshake. The model comes from the `model` argument, which the bridge sends as a header and does not store in the task itself. Write tools record both in the task's history.

The bridge also serves MCP prompts, which are agent workflows compiled into `clankban.exe` from `src-tauri/skills/`. They are listed with `prompts/list` and rendered with `prompts/get`. They are not local Claude skills, so any MCP client connected to the bridge gets them:

- `import_tasks(source)` — reads the board, builds an inventory, and creates items only after approval, with `parentId` set explicitly.
- `update_tasks(changes)` — reads each item with `get_task`, shows planned field changes, and applies them after approval.
- `review_tasks(scope?)` — read-only health check: overdue, stuck, missing context, orphans, priority mismatches, and duplicates.

Skills are defined once in `src-tauri/src/skills.rs`. The MCP bridge serves them as prompts, and the board's **Agent runs** view sends them to an agent.

**Run queue.** Sending a skill from the board stores a request in the `run_requests` table with status `pending`. The bridge exposes two tools for working the queue:

- `list_run_requests(status?)` — returns pending requests by default. Each one includes a rendered `prompt` the agent can follow.
- `complete_run_request(id, status, result?)` — the agent sets a request to `running` when it starts, then to `done`, `failed`, or `cancelled` with a short result. The board shows each status, and a pending request can be cancelled from the board.

Requests only run when a connected agent checks the queue, so nothing runs on its own. The skill's approval steps still apply.

Agent tools should remain small and composable. Avoid a single tool that returns the whole workspace by default. Future tools should support delegation, comments, approvals, and activity without making every call expensive.

## Human interaction model

The board is the human control surface:

- Columns represent workflow state.
- Cards show type, priority, stable ID, due date, and checklist progress.
- The task drawer shows description, metadata, embedded subtasks, related child tasks, and activity.
- Humans can create, edit, delete, search, sort, and complete checklist items.
- The board refreshes from the API on a five-second interval and has a manual refresh button.

The intended future delegation loop is:

```text
Human defines or approves work
        ↓
Agent receives compact task context
        ↓
Agent proposes plan / asks for approval
        ↓
Agent executes in the code project
        ↓
Agent updates task, subtasks, evidence, and status
        ↓
Human reviews the visible result
```

## Current limitations

- No authentication is needed because the first release is local-only; access control is not yet implemented.
- Activity is currently a UI placeholder rather than an append-only event log.
- Agent assignment, approval gates, comments, labels, dependencies, and notifications are not yet first-class tables.
- Parent links are supported, but the UI does not yet offer a dedicated “create child task” flow.
- The database migration approach is intentionally lightweight and lives in `server.js`; it should move to versioned migrations before multi-version production use.
- The installer has no code-signing certificate.

## Design decisions for future work

1. Keep the API local and bind only to loopback by default.
2. Keep SQLite as the canonical store for the desktop app.
3. Treat MCP as an adapter, not as a second data store.
4. Make every agent mutation attributable to an agent identity and timestamp.
5. Require human confirmation for deletion, bulk changes, and actions outside the task database.
6. Prefer structured fields for relationships and status; use descriptions for human context, not hidden control data.
