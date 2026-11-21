//! Per-user settings, stored as `settings.json` in the data folder.
//! The only setting so far is the theme: follow Windows, or force light or dark.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub theme: ThemeChoice,
}

const FILE: &str = "settings.json";

/// Missing or unreadable settings fall back to the defaults.
pub fn load(dir: &Path) -> Settings {
    std::fs::read_to_string(dir.join(FILE))
        .ok()
        .and_then(|text| serde_json::from_str(text.trim_start_matches('\u{feff}')).ok())
        .unwrap_or_default()
}

pub fn save(dir: &Path, settings: &Settings) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(FILE), text).map_err(|e| e.to_string())
}

/// Reads `reg query` output for AppsUseLightTheme. Returns `Some(true)` when Windows
/// apps use the light theme, `Some(false)` for dark, and `None` if the value is absent.
pub fn parse_apps_use_light_theme(output: &str) -> Option<bool> {
    output
        .lines()
        .find(|line| line.trim_start().starts_with("AppsUseLightTheme"))
        .and_then(|line| line.split_whitespace().last())
        .and_then(|value| u32::from_str_radix(value.trim_start_matches("0x"), 16).ok())
        .map(|value| value != 0)
}

/// Whether Windows is set to its dark app theme. Defaults to light when the setting cannot be read.
pub fn system_prefers_dark() -> bool {
    let mut command = std::process::Command::new("reg");
    command.args([
        "query",
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "/v",
        "AppsUseLightTheme",
    ]);
    // Keep the console window from flashing up when the GUI starts.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
        .output()
        .ok()
        .and_then(|out| parse_apps_use_light_theme(&String::from_utf8_lossy(&out.stdout)))
        .map(|light| !light)
        .unwrap_or(false)
}

/// Resolves the choice to dark (true) or light (false).
pub fn resolve_dark(choice: ThemeChoice, system_dark: bool) -> bool {
    match choice {
        ThemeChoice::System => system_dark,
        ThemeChoice::Light => false,
        ThemeChoice::Dark => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_registry_output() {
        let dark = "\r\nHKEY_CURRENT_USER\\...\\Personalize\r\n    AppsUseLightTheme    REG_DWORD    0x0\r\n";
        let light = "    AppsUseLightTheme    REG_DWORD    0x1\r\n";
        assert_eq!(parse_apps_use_light_theme(dark), Some(false));
        assert_eq!(parse_apps_use_light_theme(light), Some(true));
        assert_eq!(parse_apps_use_light_theme("ERROR: not found"), None);
    }

    #[test]
    fn explicit_choice_overrides_system() {
        assert!(resolve_dark(ThemeChoice::Dark, false));
        assert!(!resolve_dark(ThemeChoice::Light, true));
        assert!(resolve_dark(ThemeChoice::System, true));
        assert!(!resolve_dark(ThemeChoice::System, false));
    }

    #[test]
    fn settings_round_trip_and_default_when_missing() {
        let dir = std::env::temp_dir().join(format!("clankban-settings-{}", std::process::id()));
        assert_eq!(load(&dir), Settings::default());
        save(&dir, &Settings { theme: ThemeChoice::Dark }).unwrap();
        assert_eq!(load(&dir).theme, ThemeChoice::Dark);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_settings_fall_back_to_default() {
        let dir = std::env::temp_dir().join(format!("clankban-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE), "{ not json").unwrap();
        assert_eq!(load(&dir), Settings::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
