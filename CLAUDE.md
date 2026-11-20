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

The desktop app is native Rust (Slint) in `app/`. Run these from `app`:

```powershell
cargo run                 # desktop app (API on 127.0.0.1:4187 starts with it)
cargo run -- --api        # headless API on 127.0.0.1:4187
cargo run -- --mcp        # stdio MCP bridge (uses CLANKBAN_API_URL if set)
cargo test                # unit tests
cargo build --release     # release exe
```

Release, from the project root (builds, signs, and writes files; does not publish):

```powershell
.\scripts\release.ps1 -Version 0.2.0
```

When testing Claude Code against the desktop app, keep Clankban open and configure the MCP process with `CLANKBAN_API_URL=http://127.0.0.1:4187`.

## Important files

- `app/src/main.rs` — entry point: window, `--api`, `--mcp`, and `--verify-update`.
- `app/src/gui.rs` — window logic: models, callbacks, refresh timer.
- `app/ui/app.slint` — the window layout and components.
- `app/src/presenter.rs` — filtering, sorting, labels, and stats (pure, unit tested).
- `app/src/store.rs` — SQLite schema, seed data, normalisation, parent inference, run queue.
- `app/src/api.rs` — HTTP API routes (same contract as the original Node server).
- `app/src/mcp.rs` — stdio MCP server: agent tools.
- `app/src/skills.rs` and `app/skills/*.md` — import, update, and review workflows.
- `app/src/updater.rs` — signed-manifest update check and install.
- `app/installer/clankban.nsi` — Windows installer.
- `scripts/release.ps1` — release build, installer, signature, and manifest.
- `docs/ARCHITECTURE.md` — system design and data contracts.
- `docs/TASKS.md` — product roadmap and implementation tasks.
