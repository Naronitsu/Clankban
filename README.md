# Clankban

Clankban is a local-first, visual task board for humans and agentic AI. It runs entirely on your machine: a Rust desktop app (Tauri) with a local SQLite database, a loopback HTTP API, and an MCP bridge for coding agents.

## Install (Windows)

Build the installer once, then run it:

```powershell
cd src-tauri
cargo tauri build
```

Run `src-tauri\target\release\bundle\nsis\Clankban_0.1.0_x64-setup.exe`. The installer is unsigned, so Windows SmartScreen may warn on first launch.

Your data is stored in `%APPDATA%\clankban\clankban.sqlite`. Set `CLANKBAN_DATA_DIR` to use a different folder.

### Prerequisites for building

- Rust (stable), installed with `rustup`.
- Visual Studio 2022 Build Tools with the "Desktop development with C++" workload.
- Microsoft Edge WebView2 Runtime (already installed on Windows 11).
- Tauri CLI, once: `cargo install tauri-cli --version "^2" --locked`.

## Run in development

From `src-tauri`:

```powershell
cargo tauri dev
```

To open the board without building, double-click `start-local.ps1`. This is the browser-only prototype, which stores data in `localStorage` and not in the desktop database.

## Command-line modes

The same binary also runs without a window:

```powershell
cargo run -- --api    # headless local API on 127.0.0.1:4187
cargo run -- --mcp    # stdio MCP bridge for coding agents
```

Set `CLANKBAN_PORT` to change the API port.

## Connect a coding agent

Start the desktop app (or `--api`), then configure the agent to launch the bridge with `--mcp` and point it at the running API:

- Command: the built `clankban.exe` (or `cargo run` from `src-tauri`), with the argument `--mcp`
- Environment: `CLANKBAN_API_URL=http://127.0.0.1:4187`

The bridge exposes `get_agent_context`, `get_task`, `create_task`, and `update_task`. Use `get_agent_context` for planning so agents don't load the whole board.

## What is included

- Kanban board with Backlog, In progress, In review, and Done columns.
- Epics, tasks, subtasks, and bug fixes with stable `AT-###` identifiers.
- Create, edit, inspect, delete, search, sort, and complete subtasks.
- "Copy agent JSON" view exposing a canonical, machine-readable board snapshot.
- Local SQLite persistence and a loopback API at `127.0.0.1` for desktop and headless use.
- MCP stdio bridge for coding agents.

## Agent API

- `GET /api/agent-context`: compact active-work context (the default for planning).
- `GET /api/tasks`, `GET /api/tasks/:id`, `POST /api/tasks`, `PATCH /api/tasks/:id`, `DELETE /api/tasks/:id`: full task access.
- `GET /api/health`: liveness check.
