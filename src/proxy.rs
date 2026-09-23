//! System-proxy mode: point the desktop environment's proxy settings at the engine's
//! local SOCKS5 listener, remembering the previous settings so `proxy off` can restore
//! them exactly. GNOME (gsettings/dconf) and KDE (kioslaverc) are supported; on other
//! environments the toggle degrades to a clear message.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

const GS_SCHEMA: &str = "org.gnome.system.proxy";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Gsettings,
    Kde,
    Unsupported,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyState {
    pub backend: Backend,
    /// Prior values so `off` restores exactly what `on` found.
    pub prev_mode: Option<String>,
    pub prev_socks_host: Option<String>,
    pub prev_socks_port: Option<String>,
}

fn have(bin: &str) -> bool {
    // Cheap PATH probe: run `command -v`.
    if let Ok(out) = Command::new("sh").args(["-c", &format!("command -v {bin}")]).output() {
        return out.status.success() && !out.stdout.is_empty();
    }
    false
}

pub fn detect_backend() -> Backend {
    if have("gsettings") {
        Backend::Gsettings
    } else if have("kwriteconfig6") || have("kwriteconfig5") {
        Backend::Kde
    } else {
        Backend::Unsupported
    }
}

fn gs_get(key: &str) -> Option<String> {
    let out = Command::new("gsettings").args(["get", GS_SCHEMA, key]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Some(v.trim_matches('\'').to_string())
}

fn gs_set(key: &str, value: &str) -> Result<()> {
    let out = Command::new("gsettings")
        .args(["set", GS_SCHEMA, key, value])
        .output()
        .context("run gsettings")?;
    if !out.status.success() {
        anyhow::bail!("gsettings set {key} failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

fn gs_socks_set(key: &str, value: &str) -> Result<()> {
    let out = Command::new("gsettings")
        .args(["set", "org.gnome.system.proxy.socks", key, value])
        .output()
        .context("run gsettings (socks)")?;
    if !out.status.success() {
        anyhow::bail!("gsettings set {key} failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

fn gs_socks_get(key: &str) -> Option<String> {
    let out = Command::new("gsettings").args(["get", "org.gnome.system.proxy.socks", key]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().trim_matches('\'').to_string())
}

pub fn on(socks_port: u16, state_file: &Path) -> Result<Backend> {
    if state_file.exists() {
        let state = load_state(state_file);
        if let Ok(s) = state {
            if s.backend != Backend::Unsupported {
                // Already on; keep the recorded state.
                return Ok(s.backend);
            }
        }
        let _ = std::fs::remove_file(state_file);
    }

    let backend = detect_backend();
    let state = match backend {
        Backend::Gsettings => {
            let prev = ProxyState {
                backend,
                prev_mode: gs_get("mode"),
                prev_socks_host: gs_socks_get("host"),
                prev_socks_port: gs_socks_get("port"),
            };
            gs_set("mode", "manual")?;
            gs_socks_set("host", "127.0.0.1")?;
            gs_socks_set("port", &socks_port.to_string())?;
            prev
        }
        Backend::Kde => {
            let exe = if have("kwriteconfig6") { "kwriteconfig6" } else { "kwriteconfig5" };
            let _ = Command::new(exe)
                .args([
                    "--file", "kioslaverc", "--group", "Proxy Settings", "--key", "ProxyType",
                    "--new", "1",
                ])
                .status();
            let _ = Command::new(exe)
                .args([
                    "--file", "kioslaverc", "--group", "Proxy Settings", "--key", "SocksProxy",
                    "--new", "127.0.0.1",
                ])
                .status();
            let _ = Command::new(exe)
                .args([
                    "--file", "kioslaverc", "--group", "Proxy Settings", "--key", "SocksPort",
                    "--new", &socks_port.to_string(),
                ])
                .status();
            notify_kde();
            ProxyState { backend, prev_mode: None, prev_socks_host: None, prev_socks_port: None }
        }
        Backend::Unsupported => anyhow::bail!(
            "no supported system-proxy backend found (need gsettings or kwriteconfig5/6); \
             point your apps at the local SOCKS5 proxy 127.0.0.1:{socks_port} manually"
        ),
    };

    save_state(state_file, &state)?;
    Ok(backend)
}

pub fn off(state_file: &Path) -> Result<()> {
    let state = match load_state(state_file) {
        Ok(s) => s,
        Err(_) => return Ok(()), // nothing to restore
    };
    match state.backend {
        Backend::Gsettings => {
            if let Some(mode) = &state.prev_mode {
                let _ = gs_set("mode", mode);
            } else {
                let _ = gs_set("mode", "none");
            }
            if let Some(host) = &state.prev_socks_host {
                let _ = gs_socks_set("host", host);
            } else {
                let _ = gs_socks_set("host", "");
            }
            if let Some(port) = &state.prev_socks_port {
                let _ = gs_socks_set("port", port);
            } else {
                let _ = gs_socks_set("port", "0");
            }
        }
        Backend::Kde => {
            let exe = if have("kwriteconfig6") { "kwriteconfig6" } else { "kwriteconfig5" };
            let _ = Command::new(exe)
                .args([
                    "--file", "kioslaverc", "--group", "Proxy Settings", "--key", "ProxyType",
                    "--new", "0",
                ])
                .status();
            notify_kde();
        }
        Backend::Unsupported => {}
    }
    let _ = std::fs::remove_file(state_file);
    Ok(())
}

fn notify_kde() {
    let _ = Command::new("dbus-send")
        .args([
            "--type=signal",
            "/KIO/Scheduler",
            "org.kde.KIO.Scheduler.reparseSlaveConfiguration",
        ])
        .status();
}

fn save_state(path: &Path, state: &ProxyState) -> Result<()> {
    let json = serde_json::to_string_pretty(state).context("serialize proxy state")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).context("create state dir")?;
    }
    std::fs::write(path, json).context("write proxy state")
}

fn load_state(path: &Path) -> Result<ProxyState> {
    let data = std::fs::read_to_string(path).context("read proxy state")?;
    serde_json::from_str(&data).context("parse proxy state")
}

/// Whether the system proxy is currently enabled (state file present and a usable backend).
pub fn is_on(state_file: &Path) -> bool {
    match load_state(state_file) {
        Ok(s) => s.backend != Backend::Unsupported,
        Err(_) => false,
    }
}

/// Current GSettings proxy mode, for `status` output.
pub fn current_gsettings_mode() -> Option<String> {
    gs_get("mode")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("proxy_state.json");
        let state = ProxyState {
            backend: Backend::Gsettings,
            prev_mode: Some("auto".into()),
            prev_socks_host: Some("192.168.1.1".into()),
            prev_socks_port: Some("8888".into()),
        };
        save_state(&f, &state).unwrap();
        let loaded = load_state(&f).unwrap();
        assert_eq!(loaded.backend, Backend::Gsettings);
        assert_eq!(loaded.prev_mode.as_deref(), Some("auto"));
        assert_eq!(loaded.prev_socks_host.as_deref(), Some("192.168.1.1"));
        assert!(is_on(&f));
    }

    #[test]
    fn is_on_is_false_without_state() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_on(&tmp.path().join("proxy_state.json")));
    }
}