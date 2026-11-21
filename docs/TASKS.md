# Clankban task plan

This is the handoff backlog for the local-first desktop application. Statuses use the Clankban vocabulary: `done`, `progress`, `backlog`, and `review`.

## Completed foundation

- [x] AT-001 Create visual Kanban board with Backlog, In progress, In review, and Done.
- [x] AT-002 Support epics, tasks, subtasks, and bug fixes.
- [x] AT-003 Generate stable `AT-###` identifiers.
- [x] AT-004 Add human create, edit, delete, search, sort, and checklist interactions.
- [x] AT-005 Add browser-local fallback storage.
- [x] AT-006 Add Electron desktop shell and NSIS installer.
- [x] AT-007 Add Node local API on `127.0.0.1:4187`.
- [x] AT-008 Add native Node SQLite persistence in the Electron user-data directory.
- [x] AT-009 Add compact `/api/agent-context` endpoint.
- [x] AT-010 Add stdio MCP bridge for local agents.
- [x] AT-011 Add Claude Code connection instructions and shared live API mode.
- [x] AT-012 Add automatic five-second board refresh and manual refresh.
- [x] AT-013 Add parent-child task links and display related child tasks in the drawer.
- [x] Project docs: a read-only Docs tab for each workspace's project folder, task links to documents, and MCP tools to list and read them. No board ID yet.
- [x] Separate workspaces (`workspace_id`, sidebar picker, MCP `workspace` argument) and per-task change history with actor, client, and model (`task_history`, drawer panel). No board ID yet.
- [x] Rewrite the runtime in Rust with Tauri 2 (`src-tauri/`): SQLite store, HTTP API, stdio MCP bridge, and the existing board UI moved to `ui/`. Unit tests, API smoke test, and MCP handshake pass. Desktop window launch is not yet verified on-screen. The Electron/Node runtime has been removed. No board ID yet.

## Immediate next milestone: trustworthy delegation

### AT-020 Add explicit delegation state — backlog

Add fields for `assigneeType` (`human` or `agent`), `assigneeId`, and `delegationStatus` (`unassigned`, `proposed`, `approved`, `working`, `blocked`, `awaiting_review`, `completed`). Show these on cards and in the drawer.

### AT-021 Add human approval gates — backlog

Add an approval record and UI actions for approve, reject, request changes, and cancel. Agents may propose changes, but destructive or broad mutations should remain pending until a human approves them.

### AT-022 Add append-only activity log — backlog

Create an `activity` table capturing actor type, actor ID, action, target ID, summary, timestamp, and optional structured metadata. Replace the drawer placeholder activity entry with real history.

### AT-023 Add comments and progress updates — backlog

Allow humans and agents to append short comments, decisions, blockers, and evidence to a task without overwriting its description.

### AT-024 Add dependency relationships — backlog

Create a `task_dependencies` table with `blockedBy`/`blocks` semantics. Render dependency state and include only relevant blockers in compact agent context.

### AT-025 Add compact context modes — backlog

Support context requests such as `focusTaskId`, `status`, `assignedTo`, `includeChildren`, and `includeActivitySince`. Return only fields needed for the selected planning operation.

## Product quality milestone

### AT-030 Add versioned database migrations — backlog

Move schema changes out of ad hoc startup checks into ordered migration files with a schema version table and backup-before-migration behavior.

### AT-031 Add import/export — backlog

Export the full workspace to a versioned JSON backup and import it with validation, duplicate handling, and a dry-run report. Preserve IDs and parent links.

### AT-032 Add task detail editing for child links — backlog

Let humans create a child task from the parent drawer and choose a parent in the create/edit form.

### AT-033 Add filters and saved views — backlog

Filter by type, priority, status, assignee, parent, due date, and agent state. Allow saved views for common human review workflows.

### AT-034 Add tests — backlog

Cover API CRUD, migrations, status normalization, parent inference, MCP tool discovery, and UI refresh behavior.

### AT-035 Add installer polish — backlog

Add app icon, publisher metadata, code signing, version update behavior, and a clean uninstall/data-retention choice.

## Agent integration milestone

### AT-040 Add Codex setup documentation — backlog

Document the equivalent local MCP setup for Codex and validate tool discovery and shared API behavior.

### AT-041 Add agent activity handoff — backlog

Expose current agent session, last action, current task, and latest progress message in the board UI.

### AT-042 Add bulk import tool — backlog

Add a safe `import_tasks` MCP tool that accepts a validated array, previews duplicates and relationships, and requires an explicit confirmation token before writing.

### AT-043 Add agent task templates — backlog

Support templates for common work such as feature, bug fix, documentation, investigation, and release tasks.

## Handoff checklist for Claude

- Read `CLAUDE.md` and `docs/ARCHITECTURE.md`.
- Inspect the current code before proposing a new framework or storage layer.
- Keep the desktop app, REST API, MCP bridge, and docs in sync.
- Prefer small migrations and verify with API/MCP smoke tests.
- When importing external tasks, first produce an inventory and wait for approval.
- Set `parentId` explicitly for child tasks.
- Use `backlog`, not `todo`, for new Clankban work.
- Update this file when a task is completed or a design decision changes.
