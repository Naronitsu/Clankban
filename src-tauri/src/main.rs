// Release builds run as a GUI app; debug builds keep a console for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod docs;
mod mcp;
mod skills;
mod store;

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_updater::UpdaterExt;

use crate::store::Store;

const DEFAULT_PORT: u16 = 4187;

/// Same folder Electron used (`%APPDATA%\Clankban`); Windows paths are case-insensitive.
fn data_dir() -> PathBuf {
    std::env::var_os("CLANKBAN_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("clankban"))
}

fn api_port() -> u16 {
    std::env::var("CLANKBAN_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

fn main() {
    if std::env::args().any(|arg| arg == "--mcp") {
        if let Err(message) = run_mcp() {
            eprintln!("Clankban MCP bridge failed: {message}");
            std::process::exit(1);
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--api") {
        if let Err(message) = run_headless_api() {
            eprintln!("Clankban API failed: {message}");
            std::process::exit(1);
        }
        return;
    }

    let result = tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![check_for_update, install_update, pick_project_folder, get_changelog])
        .setup(|app| {
            let store = Arc::new(Store::open(&data_dir()).map_err(|e| format!("{e:?}"))?);

            let port = api_port();
            let listener = TcpListener::bind(("127.0.0.1", port))
                .map_err(|e| format!("Could not bind 127.0.0.1:{port}: {e}"))?;
            listener.set_nonblocking(true)?;
            let api_url = format!("http://127.0.0.1:{port}");

            tauri::async_runtime::spawn(async move {
                match tokio::net::TcpListener::from_std(listener) {
                    Ok(listener) => {
                        if let Err(e) = api::serve(listener, store).await {
                            eprintln!("Clankban API stopped: {e}");
                        }
                    }
                    Err(e) => eprintln!("Clankban API could not start: {e}"),
                }
            });

            // The UI reads its API location from `window.clankban.apiUrl`, as it did under Electron's preload.
            let init = format!(
                "window.clankban = {{ apiUrl: {}, desktop: true }};",
                serde_json::to_string(&api_url)?
            );
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("Clankban")
                .inner_size(1440.0, 960.0)
                .min_inner_size(980.0, 680.0)
                .background_color(tauri::window::Color(247, 248, 252, 255))
                .initialization_script(&init)
                .build()?;
            spawn_update_checks(app.handle().clone());
            Ok(())
        })
        .run(tauri::generate_context!());

    if let Err(e) = result {
        eprintln!("Clankban failed to start: {e}");
        std::process::exit(1);
    }
}

/// Headless API on 127.0.0.1 (no window), the Rust equivalent of `npm run api`.
fn run_headless_api() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let store = Arc::new(Store::open(&data_dir()).map_err(|e| format!("{e:?}"))?);
        let port = api_port();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| format!("Could not bind 127.0.0.1:{port}: {e}"))?;
        println!("Clankban API listening at http://127.0.0.1:{port}");
        api::serve(listener, store).await.map_err(|e| e.to_string())
    })
}

/// Runs the stdio MCP bridge. Uses `CLANKBAN_API_URL` when set, otherwise
/// starts an embedded API on a free loopback port over the same data folder.
fn run_mcp() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;

    runtime.block_on(async {
        let api_url = match std::env::var("CLANKBAN_API_URL") {
            Ok(url) if !url.is_empty() => url,
            _ => {
                let store = Arc::new(Store::open(&data_dir()).map_err(|e| format!("{e:?}"))?);
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .map_err(|e| e.to_string())?;
                let port = listener.local_addr().map_err(|e| e.to_string())?.port();
                tokio::spawn(api::serve(listener, store));
                format!("http://127.0.0.1:{port}")
            }
        };
        mcp::run(api_url).await.map_err(|e| e.to_string())
    })
}

/// First check a few seconds after launch, then every six hours while the app stays open.
const FIRST_UPDATE_CHECK: Duration = Duration::from_secs(8);
const UPDATE_CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// The changelog bundled with this build. Each release has a `## <version>` section.
const CHANGELOG: &str = include_str!("../../CHANGELOG.md");

/// A newer release, as the page needs it: its version and its release notes.
/// The notes come from `latest.json`, which the release script fills from CHANGELOG.md.
#[derive(Clone, serde::Serialize)]
struct UpdateInfo {
    version: String,
    notes: String,
}

/// Checks for a newer release in the background. When one exists, the page is told
/// through the `update-available` event with its version and notes.
fn spawn_update_checks(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_UPDATE_CHECK).await;
        loop {
            if let Ok(Some(info)) = find_update(&app).await {
                let _ = app.emit("update-available", info);
            }
            tokio::time::sleep(UPDATE_CHECK_EVERY).await;
        }
    });
}

/// The newer release, or `None` when this one is current.
async fn find_update(app: &AppHandle) -> Result<Option<UpdateInfo>, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater.check().await.map_err(|e| e.to_string())?;
    Ok(update.map(|u| UpdateInfo {
        version: u.version,
        notes: u.body.unwrap_or_default(),
    }))
}

#[tauri::command]
async fn check_for_update(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    find_update(&app).await
}

/// The full changelog bundled with this build, for the "What's new" view.
#[tauri::command]
fn get_changelog() -> String {
    CHANGELOG.to_string()
}

#[cfg(test)]
mod changelog_tests {
    #[test]
    fn changelog_has_a_section_for_this_version() {
        let heading = format!("## {}", env!("CARGO_PKG_VERSION"));
        assert!(
            super::CHANGELOG.lines().any(|line| line == heading || line.starts_with(&format!("{heading} "))),
            "CHANGELOG.md needs a '{heading}' section before this version is released"
        );
    }
}

/// Downloads and installs the newest release, then restarts Clankban.
/// The updater verifies the release signature before anything is installed.
#[tauri::command]
async fn install_update(app: AppHandle) -> Result<(), String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let Some(update) = updater.check().await.map_err(|e| e.to_string())? else {
        return Err("No update is available.".into());
    };
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    app.restart();
}

/// Opens the system folder chooser and returns the chosen folder, or `None` if cancelled.
/// Only the path comes back. Saving it on a workspace goes through the API, and the board never writes to the folder.
#[tauri::command]
async fn pick_project_folder(app: AppHandle) -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        app.dialog()
            .file()
            .set_title("Choose the project folder")
            .blocking_pick_folder()
            .and_then(|path| path.into_path().ok())
            .map(|path| path.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| e.to_string())
}
