// src/config.rs – load/save config from disk

use crate::types::Config;
use anyhow::Result;
use std::path::PathBuf;

fn config_path() -> PathBuf {
    let mut p = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push("YDM");
    p.push("config.json");
    p
}

pub fn load() -> Config {
    let path = config_path();
    if let Ok(data) = std::fs::read_to_string(&path) {
        if let Ok(cfg) = serde_json::from_str::<Config>(&data) {
            return cfg;
        }
    }
    Config::default()
}

pub fn save(cfg: &Config) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(cfg)?;
    std::fs::write(&path, data)?;
    Ok(())
}

// ─── History persistence ──────────────────────────────────────────────────────

fn history_path() -> PathBuf {
    let mut p = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push("YDM");
    p.push("history.json");
    p
}

pub fn load_history() -> Vec<crate::types::DownloadItem> {
    let path = history_path();
    if let Ok(data) = std::fs::read_to_string(&path) {
        if let Ok(items) = serde_json::from_str::<Vec<crate::types::DownloadItem>>(&data) {
            return items;
        }
    }
    vec![]
}

pub fn save_history(items: &[crate::types::DownloadItem]) -> Result<()> {
    let path = history_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Keep last 500 completed items
    let completed: Vec<_> = items
        .iter()
        .filter(|i| i.status == crate::types::DownloadStatus::Completed)
        .rev()
        .take(500)
        .cloned()
        .collect();
    let data = serde_json::to_string_pretty(&completed)?;
    std::fs::write(&path, data)?;
    Ok(())
}

// ─── Windows startup registry ─────────────────────────────────────────────────

#[cfg(windows)]
pub fn set_startup(enable: bool) -> Result<()> {
    use winreg::enums::*;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run_key = hkcu.open_subkey_with_flags(
        r"Software\Microsoft\Windows\CurrentVersion\Run",
        KEY_SET_VALUE | KEY_QUERY_VALUE,
    )?;

    if enable {
        let exe = std::env::current_exe()?;
        let val = format!("\"{}\" --tray", exe.to_string_lossy());
        run_key.set_value("YDM", &val)?;
    } else {
        let _ = run_key.delete_value("YDM");
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn set_startup(_enable: bool) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
pub fn is_startup_enabled() -> bool {
    use winreg::enums::*;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(key) = hkcu.open_subkey_with_flags(
        r"Software\Microsoft\Windows\CurrentVersion\Run",
        KEY_QUERY_VALUE,
    ) {
        return key.get_value::<String, _>("YDM").is_ok();
    }
    false
}

#[cfg(not(windows))]
pub fn is_startup_enabled() -> bool {
    false
}
