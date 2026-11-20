//! In-app updates. Reads the release manifest published with each GitHub release,
//! downloads the installer, verifies its minisign signature against the public key
//! built into the app, and then runs the installer silently.

use std::io::Read;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine;
use serde_json::Value;

use crate::gui::AppWindow;
use slint::ComponentHandle;

/// Where the release manifest lives. The release script uploads `latest.json` here.
pub const MANIFEST_URL: &str = "https://github.com/Naronitsu/Clankban/releases/latest/download/latest.json";
const PLATFORM: &str = "windows-x86_64";
/// Public key that signs release installers (base64 of the minisign public key file).
pub const PUBLIC_KEY_B64: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDBFQzU3NUU1MjMxNTA4RUQKUldUdENCVWo1WFhGRHNuQS9IZm1jWnJiQ3A0amdUb21SKzF6SWM3bUVtRDZiVkUwL2FrSWxLTXMK";

#[derive(Debug, Clone, PartialEq)]
pub struct Available {
    pub version: String,
    pub url: String,
    pub signature: String,
}

static PENDING: Mutex<Option<Available>> = Mutex::new(None);

/// Numeric x.y.z comparison. Pre-release suffixes are ignored.
pub fn is_newer(remote: &str, local: &str) -> bool {
    fn parts(v: &str) -> Option<[u64; 3]> {
        let core = v.trim().trim_start_matches('v').split(['-', '+']).next()?;
        let mut out = [0u64; 3];
        for (i, part) in core.split('.').take(3).enumerate() {
            out[i] = part.parse().ok()?;
        }
        Some(out)
    }
    match (parts(remote), parts(local)) {
        (Some(r), Some(l)) => r > l,
        _ => false,
    }
}

/// Parses a manifest. A byte-order mark is tolerated.
pub fn parse_manifest(text: &str, local_version: &str) -> Result<Option<Available>, String> {
    let json: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("manifest is not valid JSON: {e}"))?;
    let version = json["version"].as_str().ok_or("manifest has no version")?.to_string();
    if !is_newer(&version, local_version) {
        return Ok(None);
    }
    let entry = &json["platforms"][PLATFORM];
    let url = entry["url"].as_str().ok_or("manifest has no installer URL for this platform")?.to_string();
    let signature = entry["signature"].as_str().ok_or("manifest has no signature for this platform")?.to_string();
    Ok(Some(Available { version, url, signature }))
}

fn fetch_manifest() -> Result<Option<Available>, String> {
    let body = ureq::get(MANIFEST_URL)
        .timeout(Duration::from_secs(15))
        .call()
        .map_err(|e| format!("could not reach the update server: {e}"))?
        .into_string()
        .map_err(|e| e.to_string())?;
    parse_manifest(&body, env!("CARGO_PKG_VERSION"))
}

/// Downloads the installer and checks its signature before anything runs.
pub fn download_verified(update: &Available) -> Result<PathBuf, String> {
    let mut bytes = Vec::new();
    ureq::get(&update.url)
        .timeout(Duration::from_secs(300))
        .call()
        .map_err(|e| format!("download failed: {e}"))?
        .into_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| format!("download failed: {e}"))?;

    verify_signature(&bytes, &update.signature)?;

    let name = update.url.rsplit('/').next().unwrap_or("Clankban-setup.exe");
    let dir = std::env::temp_dir().join("clankban-update");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(name);
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

pub fn verify_signature(bytes: &[u8], signature_b64: &str) -> Result<(), String> {
    let engine = base64::engine::general_purpose::STANDARD;
    let key_text = String::from_utf8(
        engine.decode(PUBLIC_KEY_B64).map_err(|e| format!("bad public key: {e}"))?,
    )
    .map_err(|e| format!("bad public key: {e}"))?;
    let sig_text = String::from_utf8(
        engine.decode(signature_b64.trim()).map_err(|e| format!("bad signature: {e}"))?,
    )
    .map_err(|e| format!("bad signature: {e}"))?;
    let key = minisign_verify::PublicKey::decode(&key_text).map_err(|e| format!("bad public key: {e}"))?;
    let signature = minisign_verify::Signature::decode(&sig_text).map_err(|e| format!("bad signature: {e}"))?;
    key.verify(bytes, &signature, false)
        .map_err(|_| "the update's signature does not match; it was not installed".to_string())
}

/// Checks for a newer release on a background thread and reports the result to the window.
pub fn check(ui: &AppWindow) {
    ui.set_update_text("Checking…".into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let result = fetch_manifest();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            match result {
                Ok(Some(update)) => {
                    ui.set_update_text(format!("Version {} is available.", update.version).into());
                    ui.set_install_label(format!("Install {}", update.version).into());
                    ui.set_update_available(true);
                    *PENDING.lock().unwrap() = Some(update);
                }
                Ok(None) => {
                    ui.set_update_text("You have the latest version.".into());
                    ui.set_update_available(false);
                }
                Err(message) => ui.set_update_text(message.into()),
            }
        });
    });
}

/// Downloads, verifies, and runs the pending installer silently, then closes the app so the
/// installer can replace the files.
pub fn install(ui: &AppWindow) {
    let Some(update) = PENDING.lock().unwrap().clone() else {
        ui.set_update_text("Check for updates first.".into());
        return;
    };
    ui.set_update_text(format!("Downloading {}…", update.version).into());
    ui.set_update_available(false);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let outcome = download_verified(&update).and_then(|path| {
            std::process::Command::new(&path)
                .args(["/S", "/R"])
                .spawn()
                .map(|_| ())
                .map_err(|e| format!("could not start the installer: {e}"))
        });
        match outcome {
            Ok(()) => {
                std::thread::sleep(Duration::from_secs(1));
                std::process::exit(0);
            }
            Err(message) => {
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_update_text(message.into());
                        ui.set_update_available(true);
                    }
                });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("0.2.0", "0.1.1"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(!is_newer("0.1.1", "0.1.1"));
        assert!(!is_newer("0.1.0", "0.1.1"));
        assert!(is_newer("v1.0.0", "0.9.0"));
        assert!(!is_newer("not-a-version", "0.1.1"));
    }

    #[test]
    fn manifest_with_bom_is_parsed() {
        let text = "\u{feff}{\"version\":\"0.3.0\",\"platforms\":{\"windows-x86_64\":{\"url\":\"https://example.com/a-setup.exe\",\"signature\":\"c2ln\"}}}";
        let update = parse_manifest(text, "0.2.0").unwrap().unwrap();
        assert_eq!(update.version, "0.3.0");
        assert_eq!(update.url, "https://example.com/a-setup.exe");
    }

    #[test]
    fn manifest_without_newer_version_reports_nothing() {
        let text = "{\"version\":\"0.2.0\",\"platforms\":{}}";
        assert_eq!(parse_manifest(text, "0.2.0").unwrap(), None);
    }

    #[test]
    fn tampered_installer_fails_verification() {
        let bytes = b"not the signed installer";
        let fake = base64::engine::general_purpose::STANDARD.encode("untrusted comment: x\nRWQ=\n");
        assert!(verify_signature(bytes, &fake).is_err());
    }
}
