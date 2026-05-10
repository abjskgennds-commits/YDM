// src/browser.rs – browser detection + registry-policy extension installation
//
// Writes HKCU registry keys that tell Chrome/Edge/Brave/Opera to
// auto-install the YDM extension on next browser launch.
// No browser process is launched – entirely passive.

use anyhow::Result;
use std::path::{Path, PathBuf};

// Replace with your real extension ID from chrome://extensions
pub const EXTENSION_ID:  &str = "efdfoecbgnmpmmfchhnabejiknoejaeb";
pub const CRX_FILENAME:  &str = "ydm_extension.crx";
pub const CRX_VERSION:   &str = "1.0.0";

// ─── Browser info ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct BrowserInfo {
    pub key:          &'static str,
    pub name:         &'static str,
    pub exe_paths:    Vec<PathBuf>,
    pub profile_base: PathBuf,
    pub reg_tpl:      &'static str,   // {id} substituted at runtime
}

impl BrowserInfo {
    pub fn is_installed(&self) -> bool {
        self.exe_paths.iter().any(|p| p.exists())
    }

    pub fn extension_in_profile(&self, ext_id: &str) -> bool {
        let base = &self.profile_base;
        if !base.exists() { return false; }
        for profile in &["Default", "Profile 1", "Profile 2", "Guest Profile"] {
            let p = base.join(profile).join("Extensions").join(ext_id);
            if p.exists() { return true; }
        }
        false
    }
}

pub fn all_browsers() -> Vec<BrowserInfo> {
    let local  = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let appd   = std::env::var("APPDATA").unwrap_or_default();
    let pf     = std::env::var("PROGRAMFILES").unwrap_or_else(|_| r"C:\Program Files".to_string());
    let pf86   = std::env::var("PROGRAMFILES(X86)").unwrap_or_else(|_| r"C:\Program Files (x86)".to_string());

    vec![
        BrowserInfo {
            key:  "chrome",
            name: "Google Chrome",
            exe_paths: vec![
                PathBuf::from(&local).join(r"Google\Chrome\Application\chrome.exe"),
                PathBuf::from(&pf).join(r"Google\Chrome\Application\chrome.exe"),
                PathBuf::from(&pf86).join(r"Google\Chrome\Application\chrome.exe"),
            ],
            profile_base: PathBuf::from(&local).join(r"Google\Chrome\User Data"),
            reg_tpl: r"Software\Google\Chrome\Extensions\{id}",
        },
        BrowserInfo {
            key:  "edge",
            name: "Microsoft Edge",
            exe_paths: vec![
                PathBuf::from(&pf).join(r"Microsoft\Edge\Application\msedge.exe"),
                PathBuf::from(&pf86).join(r"Microsoft\Edge\Application\msedge.exe"),
                PathBuf::from(&local).join(r"Microsoft\Edge\Application\msedge.exe"),
            ],
            profile_base: PathBuf::from(&local).join(r"Microsoft\Edge\User Data"),
            reg_tpl: r"Software\Microsoft\Edge\Extensions\{id}",
        },
        BrowserInfo {
            key:  "brave",
            name: "Brave Browser",
            exe_paths: vec![
                PathBuf::from(&pf).join(r"BraveSoftware\Brave-Browser\Application\brave.exe"),
                PathBuf::from(&pf86).join(r"BraveSoftware\Brave-Browser\Application\brave.exe"),
                PathBuf::from(&local).join(r"BraveSoftware\Brave-Browser\Application\brave.exe"),
            ],
            profile_base: PathBuf::from(&appd).join(r"BraveSoftware\Brave-Browser\User Data"),
            reg_tpl: r"Software\Google\Chrome\Extensions\{id}",
        },
        BrowserInfo {
            key:  "opera",
            name: "Opera",
            exe_paths: vec![
                PathBuf::from(&appd).join(r"Opera Software\Opera Stable\opera.exe"),
                PathBuf::from(&local).join(r"Programs\Opera\opera.exe"),
                PathBuf::from(&pf).join(r"Opera\opera.exe"),
                PathBuf::from(&pf86).join(r"Opera\opera.exe"),
            ],
            profile_base: PathBuf::from(&appd).join(r"Opera Software\Opera Stable"),
            reg_tpl: r"Software\Google\Chrome\Extensions\{id}",
        },
    ]
}

// ─── Detect installed browsers ────────────────────────────────────────────────

pub fn detect_installed() -> Vec<BrowserInfo> {
    all_browsers().into_iter().filter(|b| b.is_installed()).collect()
}

// ─── Extension status per browser ─────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ExtensionStatus {
    pub browser:       BrowserInfo,
    pub in_profile:    bool,
    pub in_registry:   bool,
}

impl ExtensionStatus {
    pub fn is_installed(&self) -> bool {
        self.in_profile || self.in_registry
    }
}

pub fn check_all_extensions(ext_id: &str) -> Vec<ExtensionStatus> {
    detect_installed()
        .into_iter()
        .map(|b| {
            let in_profile  = b.extension_in_profile(ext_id);
            let in_registry = check_registry(&b.reg_tpl.replace("{id}", ext_id));
            ExtensionStatus { browser: b, in_profile, in_registry }
        })
        .collect()
}

#[cfg(windows)]
fn check_registry(key_path: &str) -> bool {
    use winreg::enums::*;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    hkcu.open_subkey(key_path).is_ok()
}

#[cfg(not(windows))]
fn check_registry(_key_path: &str) -> bool { false }

// ─── Write registry policy key ────────────────────────────────────────────────

#[cfg(windows)]
pub fn write_policy_key(browser: &BrowserInfo, crx_path: &Path, ext_id: &str) -> Result<()> {
    use winreg::enums::*;
    use winreg::RegKey;

    let key_path = browser.reg_tpl.replace("{id}", ext_id);
    let hkcu     = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu.create_subkey(&key_path)?;
    key.set_value("path",    &crx_path.to_string_lossy().to_string())?;
    key.set_value("version", &CRX_VERSION.to_string())?;
    tracing::info!("Registry key written: HKCU\\{}", key_path);
    Ok(())
}

#[cfg(not(windows))]
pub fn write_policy_key(_browser: &BrowserInfo, _crx_path: &Path, _ext_id: &str) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
pub fn delete_policy_key(browser: &BrowserInfo, ext_id: &str) -> Result<()> {
    use winreg::enums::*;
    use winreg::RegKey;
    let key_path = browser.reg_tpl.replace("{id}", ext_id);
    let hkcu     = RegKey::predef(HKEY_CURRENT_USER);
    let _ = hkcu.delete_subkey_all(&key_path);
    Ok(())
}

#[cfg(not(windows))]
pub fn delete_policy_key(_browser: &BrowserInfo, _ext_id: &str) -> Result<()> { Ok(()) }

// ─── Integration runner (called at startup) ───────────────────────────────────

pub struct IntegrationReport {
    pub statuses:    Vec<ExtensionStatus>,
    pub wrote_keys:  Vec<String>,   // browser names that got registry key written
    pub no_crx:      bool,
}

pub fn run_integration(exe_dir: &Path) -> IntegrationReport {
    let crx_path = exe_dir.join(CRX_FILENAME);
    let statuses = check_all_extensions(EXTENSION_ID);
    let mut wrote_keys = vec![];
    let no_crx = !crx_path.exists();

    for st in &statuses {
        if !st.is_installed() {
            if no_crx {
                tracing::warn!("{} missing extension but {} not found", st.browser.name, CRX_FILENAME);
                continue;
            }
            match write_policy_key(&st.browser, &crx_path, EXTENSION_ID) {
                Ok(_)  => {
                    wrote_keys.push(st.browser.name.to_string());
                    tracing::info!("Extension policy set for {}", st.browser.name);
                }
                Err(e) => {
                    tracing::error!("Failed to set policy for {}: {e}", st.browser.name);
                }
            }
        }
    }

    IntegrationReport { statuses, wrote_keys, no_crx }
}

// ─── Sentinel (don't repeat on every launch) ─────────────────────────────────

fn sentinel_path() -> PathBuf {
    let mut p = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push("YDM");
    p.push(".integration_done");
    p
}

pub fn integration_done() -> bool {
    sentinel_path().exists()
}

pub fn mark_integration_done() {
    let path = sentinel_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, chrono::Utc::now().to_rfc3339());
}

pub fn reset_integration_sentinel() {
    let _ = std::fs::remove_file(sentinel_path());
}
