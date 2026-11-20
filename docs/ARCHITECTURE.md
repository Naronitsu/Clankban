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
│ Clankban window (Slint, Rust) │
│ Board, drawer, agent runs     │
└──────────────┬───────────────┘
               │ same Store, in-process
┌──────────────▼───────────────┐
│ Rust core                     │
│ SQLite store + run queue      │
│ REST API on 127.0.0.1:4187    │
│ %APPDATA%clankbanclankban.sqlite │
└──────────────┬───────────────┘
               │
┌──────────────▼───────────────┐
│ MCP stdio bridge (--mcp)      │
│ tools + import/update/review  │
│ prompts + run-request tools   │
└──────────────┬───────────────┘
               │
     Claude Code, Codex, other
     local MCP-capable agents
```

The window and the agent API share one `Store`, so a change made on the board is visible to agents immediately and the other way round. The window also re-reads the store every two seconds to pick up agent changes. The MCP bridge is launched by an agent as a stdio process. It forwards requests to the API when `CLANKBAN_API_URL` is set, or starts its own API over the same data folder when it is not.

## Packaging and updates

- Desktop UI: native Rust with [Slint](https://slint.dev). The layout is in `app/ui/app.slint`, and the logic is in `app/src/gui.rs`. There is no web view.
- One binary, four modes: the window (default), `--api` (headless API), `--mcp` (stdio MCP bridge), and `--verify-update <installer> <sig>`.
- Installer: NSIS script at `app/installer/clankban.nsi`, built with `makensis`. It installs per user to `%LOCALAPPDATA%Clankban`. Silent switches: `/S` (quiet), `/R` (start the app afterwards).
- Release: `scripts/release.ps1 -Version X.Y.Z` builds the release exe, creates and signs the installer, writes `latest.json`, and prints the `gh release create` command. It does not publish.
- In-app updates: the app reads `latest.json` from the newest GitHub release, downloads the installer, checks its minisign signature against the public key built into the app, and only then runs it silently. A tampered installer is refused.
- Platform target: Windows x64 for the first release.
- Data location: `%APPDATA%clankbanclankban.sqlite`. The same folder the earlier builds used, so existing data opens unchanged. Override with `CLANKBAN_DATA_DIR`. Uninstalling keeps this folder.
- Installers and the portable exe are unsigned for Windows, so SmartScreen may warn on first launch.

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
| GET | `/api/skills` | List the built-in agent skills (name, description, arguments). |
| GET | `/api/run-requests?status=pending` | List skill run requests, oldest first. `status` is optional. |
| POST | `/api/run-requests` | Ask a connected agent to run a skill. Body: `{skill, arguments}`. Unknown skills and missing required arguments return 400. |
| PATCH | `/api/run-requests/:id` | Update a run request. Body: `{status, result?}`, where status is `pending`, `running`, `done`, `failed`, or `cancelled`. |

`/api/agent-context` intentionally returns a reduced payload: workspace, schema version, counts, and active tasks with IDs, titles, type, priority, status, due date, and subtasks. Agents should use it first, then request individual tasks only when needed.

## MCP contract

The local stdio server exposes:

- `get_agent_context(status?)` — compact planning context.
- `get_task(id)` — focused task lookup.
- `create_task(title, description?, type?, priority?, status?, due?, parentId?, subtasks?)`.
- `update_task(id, title?, description?, priority?, status?, due?, parentId?)`.

The bridge also serves MCP prompts, which are agent workflows compiled into `clankban.exe` from `app/skills/`. They are listed with `prompts/list` and rendered with `prompts/get`. They are not local Claude skills, so any MCP client connected to the bridge gets them:

- `import_tasks(source)` — reads the board, builds an inventory, and creates items only after approval, with `parentId` set explicitly.
- `update_tasks(changes)` — reads each item with `get_task`, shows planned field changes, and applies them after approval.
- `review_tasks(scope?)` — read-only health check: overdue, stuck, missing context, orphans, priority mismatches, and duplicates.

Skills are defined once in `app/src/skills.rs`. The MCP bridge serves them as prompts, and the board's **Agent runs** view sends them to an agent.

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
