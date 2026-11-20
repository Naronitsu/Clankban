# Clankban

Clankban is a local-first, visual task board for humans and agentic AI. It runs entirely on your machine: a native Rust desktop app (Slint) with a local SQLite database, a loopback HTTP API, and an MCP bridge for coding agents.

## Install (Windows)

Run `Clankban_<version>_x64-setup.exe` from the latest GitHub release. It installs for your user account, with no admin rights needed. The installer is unsigned, so Windows SmartScreen may warn on first launch. The portable `Clankban-<version>-portable.exe` runs without installing.

Your board is stored in `%APPDATA%\clankban\clankban.sqlite`. Set `CLANKBAN_DATA_DIR` to use a different folder.

### Updates

Clankban checks for updates a few seconds after it starts. You can also press **Check for updates** in the sidebar. When a newer release exists, **Install** downloads it, checks its signature against the key built into the app, and runs the installer silently. Clankban restarts afterwards. A download whose signature doesn't match is refused. The portable exe doesn't update itself.

## Build from source

Prerequisites:

- Rust (stable), installed with `rustup`.
- Visual Studio 2022 Build Tools with the "Desktop development with C++" workload.
- NSIS, only for building the installer. The release script looks for it in the Tauri CLI's download location.

From the `app` folder:

```powershell
cargo run               # open the desktop app
cargo test              # run the unit tests
cargo build --release   # build clankban.exe
```

To build a release, run this from the project root. It writes the installer, signature, manifest, and portable exe to `release\v<version>`. It doesn't publish anything:

```powershell
.\scripts\release.ps1 -Version 0.2.0
```

## Command-line modes

The same binary also runs without a window:

```powershell
clankban.exe --api                          # headless local API on 127.0.0.1:4187
clankban.exe --mcp                          # stdio MCP bridge for coding agents
clankban.exe --verify-update <setup.exe> <setup.exe.sig>   # check an installer's signature
```

Set `CLANKBAN_PORT` to change the API port.

## Connect a coding agent

Start the desktop app (or `--api`), then configure the agent to launch the bridge with `--mcp` and point it at the running API:

- Command: `clankban.exe` with the argument `--mcp` (or `cargo run -- --mcp` from `app`).
- Environment: `CLANKBAN_API_URL=http://127.0.0.1:4187`

The bridge exposes:

- Tools: `get_agent_context`, `get_task`, `create_task`, `update_task`, `list_run_requests`, `complete_run_request`.
- Prompts: `import_tasks`, `update_tasks`, `review_tasks`.

Use `get_agent_context` for planning so agents don't load the whole board.

## Agent runs

The **Agent runs** view sends a skill to a connected agent. Clicking **Send to agent** queues a request on the board. The agent picks it up with `list_run_requests`, follows the prompt, and reports back with `complete_run_request`. Requests run only when an agent checks the queue, and the agent still asks for your approval before changing anything.

## What is included

- Board with Backlog, In progress, In review, and Done columns, plus List and Recent views.
- Epics, tasks, subtasks, and bug fixes with stable `AT-###` identifiers, and parent-child links.
- Create, edit, inspect, delete, search, filter, sort, and tick off subtasks.
- Command menu (Ctrl+K), keyboard shortcuts, and a "Copy agent JSON" snapshot.
- Agent runs view and the request queue.
- Local SQLite persistence, a loopback API at `127.0.0.1`, and the MCP bridge.
- Signed in-app updates.

## Agent API

- `GET /api/agent-context`: compact active-work context (the default for planning).
- `GET /api/tasks`, `GET /api/tasks/:id`, `POST /api/tasks`, `PATCH /api/tasks/:id`, `DELETE /api/tasks/:id`: full task access.
- `GET /api/skills`, `GET /api/run-requests`, `POST /api/run-requests`, `PATCH /api/run-requests/:id`: skills and the request queue.
- `GET /api/health`: liveness check.
