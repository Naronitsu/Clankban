# Clankban agent instructions

Clankban is a local-first desktop task workspace for humans and coding agents. Read `docs/ARCHITECTURE.md` before changing the application. The Clankban board is the project's task list: check it before starting work (see Board workflow below).

## Working rules

1. Treat the visual board and the SQLite-backed local API as the same source of truth.
2. Use Clankban stable IDs (`AT-###`) when referring to work.
3. Preserve the human workflow: agents may propose, create, and update work, but destructive actions and broad migrations require confirmation.
4. Keep agent responses compact. Prefer `get_agent_context` for planning and `get_task` for focused work.
5. Use the canonical statuses `backlog`, `progress`, `review`, and `done`. `todo` is an input alias for `backlog`, not a display status.
6. Preserve parent-child relationships with `parentId`; do not encode hierarchy only in prose. Set `parentId` explicitly for child tasks.
7. Before changing the data model, update the API, MCP schema, UI behavior, and documentation together.
8. Verify changes with `cargo test`, an API smoke test (`cargo run -- --api`), and an MCP handshake (`cargo run -- --mcp`) when relevant.
9. When you first work in a project folder on this board, call `suggest_project_folder` once (no path, so it uses the folder you are running in) so the Docs tab can show its Markdown. Do not repeat it in the same session. Never try to connect the folder yourself: a person accepts the suggestion.
10. Inspect the current code before proposing a new framework or storage layer.
11. Prefer small migrations and verify with API/MCP smoke tests.
12. When importing external tasks, first produce an inventory and wait for approval.

## Board workflow

The board is the task source for this project. `docs/TASKS.md` is only a pointer to it.

1. **Start of work:** call `get_agent_context` for the `default` workspace. Pick the next task by priority: `high` before `medium` before `low`. Within one priority, prefer `progress` over `backlog`. Skip `review` and `done`. Skip epics; work on their children.
2. **Before starting a task:** set it to `progress` with `update_task`. Pass `model` on every write so the change history records which model made it.
3. **While working:** keep the board current. Tick subtasks as they finish, and add a short progress note to the description when something is decided, blocked, or verified. Do not wait until the end to update.
4. **New work you discover:** create it on the board with `create_task` before implementing it. Set `parentId` for child work and add subtasks for the steps.
5. **Finishing:** set `review` when a person must check the result. Set `done` only after the verification in rule 8 passes and nothing is waiting on a person.
6. **Blockers:** if a task cannot proceed, leave it in `progress` and note the blocker in its description. Tell the person in your reply.
7. Use `backlog`, not `todo`, for new work.

## Local commands

The canonical runtime is the Rust/Tauri app in `src-tauri/`. Run these from `src-tauri`:

```powershell
cargo tauri dev           # desktop app (Tauri window + API on 127.0.0.1:4187)
cargo run -- --api        # headless API on 127.0.0.1:4187
cargo run -- --mcp        # stdio MCP bridge (uses CLANKBAN_API_URL if set)
cargo test                # store unit tests
cargo tauri build         # Windows NSIS installer
```

From the repo root, `scripts\release.ps1 -Version X.Y.Z` builds and signs a release into `release/`. It does not publish.

When testing Claude Code against the desktop app, keep Clankban open and configure the MCP process with `CLANKBAN_API_URL=http://127.0.0.1:4187`.

## Important files

- `src-tauri/src/main.rs` — app entry: Tauri window, headless `--api`, and `--mcp` modes.
- `src-tauri/src/store.rs` — SQLite schema, seed data, normalisation, parent inference, unit tests.
- `src-tauri/src/api.rs` — HTTP API routes (same contract as the Node server).
- `src-tauri/src/mcp.rs` — stdio MCP server exposing the agent tools.
- `src-tauri/tauri.conf.json` — Tauri app and installer configuration.
- `ui/index.html`, `ui/styles.css`, `ui/app.js` — human-facing board UI.
- `docs/ARCHITECTURE.md` — system design and data contracts.
- `docs/TASKS.md` — pointer to the board, with the old-ID mapping.
