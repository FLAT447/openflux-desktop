//! High-level operations shared by the CLI and the TUI: profile selection, engine
//! connect/disconnect, TUN and system-proxy toggles, and a status snapshot. The CLI
//! wrappers stay thin and only format the returned values.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::{AppConfig, Profile, ProfileMode};
use crate::engine::{self, EngineConfig};
use crate::paths::Paths;
use crate::proxy::{self, Backend};
use crate::resolve;
use crate::state;
use crate::tun::{self, TunConfig};
use crate::TUN_NAME;

pub const READY_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct ConnectOutcome {
    pub pid: i32,
    pub port: u16,
    pub profile: String,
}

#[derive(Debug, Clone)]
pub struct TunUp {
    pub pid: i32,
    pub dns: String,
}

#[derive(Debug, Clone)]
pub struct ExitUp {
    pub pid: i32,
    pub streams: u16,
}

#[derive(Debug, Clone)]
pub struct ProxyUp {
    pub backend: Backend,
    pub port: u16,
}

#[derive(Debug, Clone)]
pub struct EngineStatus {
    pub pid: i32,
    pub mode: String,
    pub port: u16,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub active_profile: Option<String>,
    pub engine: Option<EngineStatus>,
    pub tun_up: bool,
    /// True when the running engine is in exit-node mode (no SOCKS listener).
    pub exit_up: bool,
    pub proxy_on: bool,
    /// Current GNOME proxy mode when the proxy is not managed by us ("none", "manual", ...).
    pub proxy_system_mode: Option<String>,
}

pub fn load(paths: &Paths) -> Result<AppConfig> {
    AppConfig::load(&paths.config_file)
}

pub fn save(paths: &Paths, cfg: &AppConfig) -> Result<()> {
    cfg.save(&paths.config_file)
}

pub fn pick_profile(paths: &Paths, name: Option<&str>) -> Result<Profile> {
    let cfg = load(paths)?;
    match name {
        Some(n) => cfg
            .get(n)
            .cloned()
            .with_context(|| format!("profile '{n}' not found")),
        None => cfg.active().cloned(),
    }
}

pub fn set_active(paths: &Paths, name: &str) -> Result<String> {
    let mut cfg = load(paths)?;
    cfg.set_active(name)?;
    save(paths, &cfg)?;
    Ok(format!("active profile: {name}"))
}

/// A TunConfig shell for the operations that only need the pidfile/tun name (status,
/// teardown, mutual-exclusion checks).
fn tun_shell(paths: &Paths) -> TunConfig {
    TunConfig {
        engine_bin: PathBuf::new(),
        url: String::new(),
        token: None,
        tun_name: TUN_NAME.to_string(),
        tun_addr: String::new(),
        mtu: 0,
        dns: String::new(),
        streams: 1,
        split_mode: String::new(),
        split_sites: Vec::new(),
        debug: false,
        engine_log: paths.engine_log.clone(),
        engine_pid: paths.engine_pid.clone(),
    }
}

/// The port of a running SOCKS-mode engine, or `None` when nothing (or a TUN/exit
/// engine) runs.
pub fn engine_port(paths: &Paths) -> Option<u16> {
    match engine::current_mode(&paths.engine_pid).as_deref() {
        Some("tun") | Some("exit") => None,
        Some(_) | None if engine::current(&paths.engine_pid).is_some() => {
            Some(engine::current_port(&paths.engine_pid, 0))
        }
        _ => None,
    }
}

fn token_for(profile: &Profile) -> Option<String> {
    match profile.mode {
        ProfileMode::Key if profile.e2e_encryption => Some(profile.key_token.clone()),
        _ => None,
    }
}

/// Start the SOCKS5 engine for a profile, resolving the key first when necessary.
pub fn connect(
    paths: &Paths,
    engine_bin: &Path,
    name: Option<&str>,
    socks_port: Option<u16>,
) -> Result<ConnectOutcome> {
    let mut profile = pick_profile(paths, name)?;

    if profile.mode == ProfileMode::Key && profile.doc_url.is_empty() {
        if profile.control_url.is_empty() || profile.key_token.is_empty() {
            bail!("profile '{}' is missing control_url/key_token", profile.name);
        }
        let result = resolve::resolve_key(&profile.control_url, &profile.key_token)?;
        profile.doc_url = result.doc_url;
        profile.doc_urls = result.doc_urls;
        profile.e2e_encryption = result.e2e_encryption;
        // Persist the resolution so `status` and later runs see a wired profile.
        let mut cfg = load(paths)?;
        if let Some(p) = cfg.get_mut(&profile.name) {
            p.doc_url = profile.doc_url.clone();
            p.doc_urls = profile.doc_urls.clone();
            p.e2e_encryption = profile.e2e_encryption;
        }
        save(paths, &cfg)?;
    }

    if !profile.is_connectable() {
        bail!(
            "profile '{}' has no doc_url; run `openflux check-key {}`",
            profile.name,
            profile.name
        );
    }

    let port = socks_port.unwrap_or(profile.socks_port);
    if tun::is_up(&tun_shell(paths)) {
        bail!("TUN mode is active; stop it with `pkexec openflux tun off` (or t in the TUI) before using `connect`");
    }
    if engine::is_port_open(port) && engine::current(&paths.engine_pid).is_none() {
        // Port is held but no live engine is on record: usually our own SOCKS engine left
        // over from an overlapping session. Reclaim it instead of refusing on a port we
        // have no other way to free (the old behavior left the user permanently stuck).
        let orphans = engine::find_orphan_socks_engines(port);
        if orphans.is_empty() {
            bail!("port {port} is already in use by something else; disconnect it first");
        }
        for pid in &orphans {
            let _ = state::terminate(*pid, Duration::from_secs(3));
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    let cfg = EngineConfig {
        bin: engine_bin.to_path_buf(),
        url: profile.doc_url.clone(),
        socks_port: port,
        token: token_for(&profile),
        mtu: Some(profile.mtu),
        streams: profile.streams,
        debug: false,
        log_file: paths.engine_log.clone(),
    };
    let pid = engine::start(&cfg, READY_TIMEOUT)?;
    Ok(ConnectOutcome {
        pid,
        port,
        profile: profile.name,
    })
}

/// Stop everything: TUN, system proxy, engine. Returns a human-readable summary.
pub fn disconnect(paths: &Paths) -> Result<String> {
    let mut lines = Vec::new();

    let tun_was = tun::is_up(&tun_shell(paths));
    tun::off(&tun_shell(paths))?;
    if tun_was {
        if tun::is_up(&tun_shell(paths)) {
            lines.push(
                "TUN routing removed; the engine still runs as root - stop it with `pkexec openflux tun off` (or t in the TUI)"
                    .to_string(),
            );
        } else {
            lines.push("TUN mode down".to_string());
        }
    }

    if proxy::is_on(&paths.proxy_state) {
        match proxy::off(&paths.proxy_state) {
            Ok(()) => lines.push("system proxy restored".to_string()),
            Err(e) => lines.push(format!("warning: {e:#}")),
        }
    }

    match engine::current(&paths.engine_pid) {
        Some(pid) => match state::terminate(pid, READY_TIMEOUT) {
            Ok(()) => {
                state::remove_pid(&paths.engine_pid);
                lines.push(format!("engine stopped (was pid {pid})"));
            }
            // Typical when the engine holds TUN as root and we are unprivileged; the
            // routing is already down, but the engine itself needs the root teardown.
            Err(e) => lines.push(format!(
                "warning: engine pid {pid} could not be stopped ({e:#}); it may be running as root - use `pkexec openflux tun off`"
            )),
        },
        None => lines.push("engine not running".to_string()),
    }

    Ok(lines.join("\n"))
}

/// Bring TUN mode up for a profile (must run as root). Stops a SOCKS engine first since
/// both modes share the engine pidfile.
pub fn tun_up(paths: &Paths, engine_bin: &Path, name: Option<&str>, debug: bool) -> Result<TunUp> {
    let profile = pick_profile(paths, name)?;
    if profile.mode == ProfileMode::Key && profile.doc_url.is_empty() {
        bail!(
            "profile '{}' has no doc_url; run `openflux check-key` first",
            profile.name
        );
    }
    if !profile.is_connectable() {
        bail!("profile '{}' has no doc_url", profile.name);
    }

    if engine::current(&paths.engine_pid).is_some()
        && engine::current_mode(&paths.engine_pid).as_deref() != Some("tun")
    {
        engine::stop(&paths.engine_pid, READY_TIMEOUT)?;
    }

    let cfg = TunConfig {
        engine_bin: engine_bin.to_path_buf(),
        url: profile.doc_url.clone(),
        token: token_for(&profile),
        tun_name: TUN_NAME.to_string(),
        tun_addr: "10.0.0.1/24".to_string(),
        mtu: profile.mtu,
        dns: profile.dns_upstream.clone(),
        streams: profile.streams,
        split_mode: profile.split_mode.clone(),
        split_sites: profile.split_sites.clone(),
        debug,
        engine_log: paths.engine_log.clone(),
        engine_pid: paths.engine_pid.clone(),
    };
    let pid = tun::on(&cfg)?;
    Ok(TunUp {
        pid,
        dns: profile.dns_upstream,
    })
}

/// Tear TUN mode down. Returns whether it had been up.
pub fn tun_down(paths: &Paths) -> Result<bool> {
    let was_up = tun::is_up(&tun_shell(paths));
    tun::off(&tun_shell(paths))?;
    Ok(was_up)
}

/// Start the engine as an exit node for a profile. A SOCKS/TUN engine (which would clash
/// on the shared pidfile) is stopped first, mirroring `tun_up`. There is no local listener,
/// so readiness is the engine's first established tunnel.
pub fn exit_up(paths: &Paths, engine_bin: &Path, name: Option<&str>, debug: bool) -> Result<ExitUp> {
    let profile = pick_profile(paths, name)?;
    if !profile.is_connectable() {
        bail!("profile '{}' has no doc_url", profile.name);
    }
    if tun::is_up(&tun_shell(paths)) {
        bail!("TUN mode is active; stop it with `pkexec openflux tun off` before starting an exit node");
    }
    if let Some(mode) = engine::current_mode(&paths.engine_pid) {
        if mode.as_str() != "exit" {
            engine::stop(&paths.engine_pid, READY_TIMEOUT)?;
        }
    }

    let cfg = EngineConfig {
        bin: engine_bin.to_path_buf(),
        url: profile.doc_url.clone(),
        socks_port: profile.socks_port,
        token: token_for(&profile),
        mtu: Some(profile.mtu),
        streams: profile.streams,
        debug,
        log_file: paths.engine_log.clone(),
    };
    let pid = engine::start_exit(&cfg, READY_TIMEOUT)?;
    Ok(ExitUp {
        pid,
        streams: profile.streams,
    })
}

/// Stop a running exit node. Returns whether one was running.
pub fn exit_down(paths: &Paths) -> Result<bool> {
    if engine::current_mode(&paths.engine_pid).as_deref() == Some("exit") {
        engine::stop(&paths.engine_pid, READY_TIMEOUT)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Point the system proxy at the running SOCKS engine.
pub fn proxy_up(paths: &Paths) -> Result<Option<ProxyUp>> {
    if proxy::is_on(&paths.proxy_state) {
        return Ok(None);
    }
    if tun::is_up(&tun_shell(paths)) {
        bail!("TUN mode is active; stop it with `pkexec openflux tun off` before using `proxy on`");
    }
    let port = engine_port(paths)
        .with_context(|| "engine is not running; run `openflux connect` first")?;
    let backend = proxy::on(port, &paths.proxy_state)?;
    Ok(Some(ProxyUp { backend, port }))
}

/// Restore the previous system proxy settings. Returns whether it had been enabled.
pub fn proxy_down(paths: &Paths) -> Result<bool> {
    if !proxy::is_on(&paths.proxy_state) {
        return Ok(false);
    }
    proxy::off(&paths.proxy_state)?;
    Ok(true)
}

pub fn status(paths: &Paths) -> Result<Status> {
    let cfg = load(paths)?;
    let engine = engine::current(&paths.engine_pid).map(|pid| EngineStatus {
        pid,
        mode: engine::current_mode(&paths.engine_pid).unwrap_or_else(|| "socks".to_string()),
        port: engine::current_port(&paths.engine_pid, 0),
    });
    let proxy_on = proxy::is_on(&paths.proxy_state);
    let proxy_system_mode = if proxy_on {
        None
    } else {
        proxy::current_gsettings_mode()
    };
    Ok(Status {
        active_profile: cfg.active_profile.clone(),
        engine,
        tun_up: tun::is_up(&tun_shell(paths)),
        exit_up: engine::current_mode(&paths.engine_pid).as_deref() == Some("exit"),
        proxy_on,
        proxy_system_mode,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(tmp: &std::path::Path) -> Paths {
        let p = Paths::under(tmp.to_path_buf());
        p.ensure_dirs().unwrap();
        p
    }

    #[test]
    fn set_active_persists_and_pick_uses_it() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(tmp.path());

        let mut cfg = AppConfig::default();
        cfg.add(Profile::manual("a", "https://x/i/a")).unwrap();
        cfg.add(Profile::manual("b", "https://x/i/b")).unwrap();
        save(&p, &cfg).unwrap();

        set_active(&p, "b").unwrap();
        assert_eq!(pick_profile(&p, None).unwrap().name, "b");
        assert_eq!(pick_profile(&p, Some("a")).unwrap().name, "a");
        assert!(pick_profile(&p, Some("missing")).is_err());
    }

    #[test]
    fn engine_port_is_none_without_a_pidfile() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(tmp.path());
        assert_eq!(engine_port(&p), None);
    }

    #[test]
    fn status_reports_stopped_when_nothing_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(tmp.path());
        let s = status(&p).unwrap();
        assert!(s.engine.is_none());
        assert!(!s.tun_up);
        assert!(!s.proxy_on);
    }
}

