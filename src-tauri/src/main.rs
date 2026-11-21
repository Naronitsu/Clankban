// Release builds run as a GUI app; debug builds keep a console for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod mcp;
mod skills;
mod store;

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;

use tauri::{WebviewUrl, WebviewWindowBuilder};

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
