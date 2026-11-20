// Release builds run as a GUI app without a console window; debug builds keep one for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod gui;
mod mcp;
mod presenter;
mod skills;
mod store;
mod updater;

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;

use store::Store;

const DEFAULT_PORT: u16 = 4187;

/// Same folder the earlier builds used (`%APPDATA%\clankban`), so existing data opens unchanged.
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
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--mcp") {
        exit_on_error(run_mcp(), "MCP bridge");
        return;
    }
    if let Some(pos) = args.iter().position(|a| a == "--verify-update") {
        exit_on_error(verify_update(&args[pos + 1..]), "verify");
        return;
    }
    if args.iter().any(|a| a == "--api") {
        exit_on_error(run_headless_api(), "API");
        return;
    }
    exit_on_error(run_gui(), "Clankban");
}

fn exit_on_error(result: Result<(), String>, what: &str) {
    if let Err(message) = result {
        eprintln!("{what} failed: {message}");
        std::process::exit(1);
    }
}

fn run_gui() -> Result<(), String> {
    let store = Arc::new(Store::open(&data_dir()).map_err(|e| format!("{e:?}"))?);
    let api = start_api_background(store.clone(), api_port());
    gui::run(store, api).map_err(|e| e.to_string())
}

/// Serves the agent API on a background thread. Returns the URL, or why it could not start
/// (for example, another copy of Clankban already holds the port).
fn start_api_background(store: Arc<Store>, port: u16) -> Result<String, String> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("Could not bind 127.0.0.1:{port}: {e}"))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        runtime.block_on(async move {
            match tokio::net::TcpListener::from_std(listener) {
                Ok(listener) => {
                    if let Err(e) = api::serve(listener, store).await {
                        eprintln!("Clankban API stopped: {e}");
                    }
                }
                Err(e) => eprintln!("Clankban API could not start: {e}"),
            }
        });
    });
    Ok(format!("http://127.0.0.1:{port}"))
}

/// `clankban --verify-update <installer> <installer.sig>`: checks a release installer against
/// the built-in public key, the same check the in-app updater runs before installing.
fn verify_update(args: &[String]) -> Result<(), String> {
    let (installer, sig) = match args {
        [installer, sig, ..] => (installer, sig),
        _ => return Err("usage: clankban --verify-update <installer> <installer.sig>".into()),
    };
    let bytes = std::fs::read(installer).map_err(|e| e.to_string())?;
    let signature = std::fs::read_to_string(sig).map_err(|e| e.to_string())?;
    updater::verify_signature(&bytes, signature.trim())?;
    println!("OK: signature matches {installer}");
    Ok(())
}

/// Headless API without a window, the equivalent of `npm run api` before the rewrite.
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

/// Stdio MCP bridge. Uses `CLANKBAN_API_URL` when set, otherwise starts an embedded API on a
/// free loopback port over the same data folder.
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
