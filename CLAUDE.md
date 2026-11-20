# Clankban agent instructions

Clankban is a local-first desktop task workspace for humans and coding agents. Read `docs/ARCHITECTURE.md` before changing the application and `docs/TASKS.md` before starting new work.

## Working rules

1. Treat the visual board and the SQLite-backed local API as the same source of truth.
2. Use Clankban stable IDs (`AT-###`) when referring to work.
3. Preserve the human workflow: agents may propose, create, and update work, but destructive actions and broad migrations require confirmation.
4. Keep agent responses compact. Prefer `get_agent_context` for planning and `get_task` for focused work.
5. Use the canonical statuses `backlog`, `progress`, `review`, and `done`. `todo` is an input alias for `backlog`, not a display status.
6. Preserve parent-child relationships with `parentId`; do not encode hierarchy only in prose.
7. Before changing the data model, update the API, MCP schema, UI behavior, and documentation together.
8. Verify changes with `cargo test`, an API smoke test (`cargo run -- --api`), and an MCP handshake (`cargo run -- --mcp`) when relevant.

## Local commands

The canonical runtime is the Rust/Tauri app in `src-tauri/`. Run these from `src-tauri`:

```powershell
cargo tauri dev           # desktop app (Tauri window + API on 127.0.0.1:4187)
cargo run -- --api        # headless API on 127.0.0.1:4187
cargo run -- --mcp        # stdio MCP bridge (uses CLANKBAN_API_URL if set)
cargo test                # store unit tests
cargo tauri build         # Windows NSIS installer
```

When testing Claude Code against the desktop app, keep Clankban open and configure the MCP process with `CLANKBAN_API_URL=http://127.0.0.1:4187`.

## Important files

- `src-tauri/src/main.rs` — app entry: Tauri window, headless `--api`, and `--mcp` modes.
- `src-tauri/src/store.rs` — SQLite schema, seed data, normalisation, parent inference, unit tests.
- `src-tauri/src/api.rs` — HTTP API routes (same contract as the Node server).
- `src-tauri/src/mcp.rs` — stdio MCP server exposing the four agent tools.
- `src-tauri/tauri.conf.json` — Tauri app and installer configuration.
- `ui/index.html`, `ui/styles.css`, `ui/app.js` — human-facing board UI, shared by Tauri and Electron.
- `docs/ARCHITECTURE.md` — system design and data contracts.
- `docs/TASKS.md` — product roadmap and implementation tasks.
