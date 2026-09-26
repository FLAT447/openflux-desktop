//! High-level operations shared by the CLI and the TUI: profile selection, engine
//! connect/disconnect, TUN and system-proxy toggles, and a status snapshot. The CLI
//! wrappers stay thin and only format the returned values.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::{AppConfig, Profile, ProfileMode};
use crate::engine::{self, EngineConfig, TransportConfig};
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

/// One-line summary of the machine-wide settings (SOCKS5 port, TUN DNS, split tunneling).
/// Shared by the CLI, the TUI and the GUI so all three describe the same knobs identically.
pub fn settings_summary(cfg: &AppConfig) -> String {
    let split = if cfg.split_enabled() {
        cfg.split_mode.as_str()
    } else {
        "none"
    };
    let sites = if cfg.split_sites.is_empty() {
        String::new()
    } else {
        format!(" [{}]", cfg.split_csv())
    };
    format!(
        "socks_port={} dns={} split={split}{sites}",
        cfg.socks_port, cfg.dns
    )
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
    /// What the engine log currently says is wrong (e.g. fetch_failed on an unreachable
    /// doc), when the engine is up but the tunnel has not connected since the failure.
    pub issue: Option<engine::ConnectionIssue>,
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
        transport: TransportConfig::default(),
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
    debug: bool,
) -> Result<ConnectOutcome> {
    let mut profile = pick_profile(paths, name)?;

    if profile.mode == ProfileMode::Key && !profile.is_connectable() {
        if profile.control_url.is_empty() || profile.key_token.is_empty() {
            bail!(
                "profile '{}' is missing control_url/key_token",
                profile.name
            );
        }
        let result = resolve::resolve_key(&profile.control_url, &profile.key_token)?;
        profile.doc_url = result.doc_url;
        profile.doc_urls = result.doc_urls;
        profile.transport = result.transport;
        profile.e2e_encryption = result.e2e_encryption;
        if profile.transport == crate::config::Transport::YandexMultistream
            && profile.doc_urls.len() >= 2
        {
            profile.streams = u16::try_from(profile.doc_urls.len()).unwrap_or(u16::MAX);
        }
        // Persist the resolution so `status` and later runs see a wired profile.
        let mut cfg = load(paths)?;
        if let Some(p) = cfg.get_mut(&profile.name) {
            p.doc_url = profile.doc_url.clone();
            p.doc_urls = profile.doc_urls.clone();
            p.transport = profile.transport;
            p.streams = profile.streams;
            p.e2e_encryption = profile.e2e_encryption;
        }
        save(paths, &cfg)?;
    }

    profile.validate()?;
    if !profile.is_connectable() {
        bail!(
            "profile '{}' is not ready; run `openflux check-key {}`",
            profile.name,
            profile.name
        );
    }

    let port = socks_port.unwrap_or(load(paths)?.socks_port);
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

    let app_cfg = load(paths)?;
    let cfg = EngineConfig {
        bin: engine_bin.to_path_buf(),
        transport: TransportConfig::from_profile(&profile),
        socks_port: port,
        token: token_for(&profile),
        mtu: Some(profile.mtu),
        streams: profile.streams,
        debug: debug || app_cfg.debug || env_debug(),
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

    // Prefer a /proc-discovered engine over the pidfile: a live engine whose pidfile is
    // missing (TUN spawns it from a root helper) used to skip this branch entirely, so the
    // button reported "engine not running" while the tunnel was still holding the routing.
    match engine::discover_running(&paths.engine_pid) {
        Some(run) => match state::terminate(run.pid, READY_TIMEOUT) {
            Ok(()) => {
                state::remove_pid(&paths.engine_pid);
                lines.push(format!("engine stopped (was pid {})", run.pid));
            }
            // Typical when the engine holds TUN as root and we are unprivileged; the
            // routing is already down, but the engine itself needs the root teardown.
            Err(e) => lines.push(format!(
                "warning: engine pid {} could not be stopped ({e:#}); it may be running as root - use `pkexec openflux tun off`",
                run.pid
            )),
        },
        None => lines.push("engine not running".to_string()),
    }

    Ok(lines.join("\n"))
}

/// OPENFLUX_DEBUG=1 turns on verbose engine logging for a single run, without touching
/// the persisted `debug` setting in openflux.toml.
fn env_debug() -> bool {
    std::env::var_os("OPENFLUX_DEBUG").is_some()
}

/// Bring TUN mode up for a profile (must run as root). Stops a SOCKS engine first since
/// both modes share the engine pidfile.
pub fn tun_up(paths: &Paths, engine_bin: &Path, name: Option<&str>, debug: bool) -> Result<TunUp> {
    let profile = pick_profile(paths, name)?;
    profile.validate()?;
    if !profile.is_connectable() {
        bail!(
            "profile '{}' is not ready; run `openflux check-key {}` first",
            profile.name,
            profile.name
        );
    }

    if engine::current(&paths.engine_pid).is_some()
        && engine::current_mode(&paths.engine_pid).as_deref() != Some("tun")
    {
        engine::stop(&paths.engine_pid, READY_TIMEOUT)?;
    }

    let app_cfg = load(paths)?;
    let cfg = TunConfig {
        engine_bin: engine_bin.to_path_buf(),
        transport: TransportConfig::from_profile(&profile),
        token: token_for(&profile),
        tun_name: TUN_NAME.to_string(),
        tun_addr: "10.0.0.1/24".to_string(),
        mtu: profile.mtu,
        dns: app_cfg.dns.clone(),
        streams: profile.streams,
        split_mode: app_cfg.split_mode.clone(),
        split_sites: app_cfg.split_sites.clone(),
        debug: debug || app_cfg.debug || env_debug(),
        engine_log: paths.engine_log.clone(),
        engine_pid: paths.engine_pid.clone(),
    };
    let pid = tun::on(&cfg)?;
    Ok(TunUp {
        pid,
        dns: app_cfg.dns,
    })
}

/// Tear TUN mode down. Returns whether it had been up.
/// What a TUN teardown achieved. Routing comes down first and unconditionally; the engine is
/// stopped as well whenever we are allowed to signal it, and the failure is reported instead
/// of hidden - "TUN off" that leaves a root engine holding the device is the state this whole
/// function exists to avoid.
pub struct TunDown {
    pub was_up: bool,
    pub engine_stopped: Option<i32>,
    pub engine_warning: Option<String>,
}

pub fn tun_down(paths: &Paths) -> Result<TunDown> {
    let was_up = tun::is_up(&tun_shell(paths));
    tun::off(&tun_shell(paths))?;
    let mut out = TunDown {
        was_up,
        engine_stopped: None,
        engine_warning: None,
    };
    // Only touch a TUN-mode engine: a SOCKS engine on the same port must survive a TUN toggle.
    let running = engine::discover_running(&paths.engine_pid).filter(|r| r.mode == "tun");
    if let Some(run) = running {
        match state::terminate(run.pid, READY_TIMEOUT) {
            Ok(()) => {
                state::remove_pid(&paths.engine_pid);
                out.engine_stopped = Some(run.pid);
            }
            Err(e) if was_up => {
                out.engine_warning = Some(format!(
                    "engine pid {} still holds the device ({e:#}); it runs as root - use `pkexec openflux tun off`",
                    run.pid
                ));
            }
            Err(_) => {}
        }
    }
    Ok(out)
}

/// Start the engine as an exit node for a profile. A SOCKS/TUN engine (which would clash
/// on the shared pidfile) is stopped first, mirroring `tun_up`. There is no local listener,
/// so readiness is the engine's first established tunnel.
pub fn exit_up(
    paths: &Paths,
    engine_bin: &Path,
    name: Option<&str>,
    debug: bool,
) -> Result<ExitUp> {
    let profile = pick_profile(paths, name)?;
    profile.validate()?;
    if !profile.is_connectable() {
        bail!(
            "profile '{}' is not ready; run `openflux check-key {}` first",
            profile.name,
            profile.name
        );
    }
    if tun::is_up(&tun_shell(paths)) {
        bail!("TUN mode is active; stop it with `pkexec openflux tun off` before starting an exit node");
    }
    if let Some(mode) = engine::current_mode(&paths.engine_pid) {
        if mode.as_str() != "exit" {
            engine::stop(&paths.engine_pid, READY_TIMEOUT)?;
        }
    }

    let app_cfg = load(paths)?;
    let cfg = EngineConfig {
        bin: engine_bin.to_path_buf(),
        transport: TransportConfig::from_profile(&profile),
        socks_port: app_cfg.socks_port,
        token: token_for(&profile),
        mtu: Some(profile.mtu),
        streams: profile.streams,
        debug: debug || app_cfg.debug || env_debug(),
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
    // Fall back to a /proc scan: without it a live engine whose pidfile is missing (the TUN
    // engine is spawned by a root helper) is reported as "nothing running", which turns the
    // Disconnect button into a Connect button and hides the tunnel that is holding the
    // machine's routing.
    let running = engine::discover_running(&paths.engine_pid);
    let profile_port = cfg.socks_port;
    let engine = running.as_ref().map(|r| EngineStatus {
        pid: r.pid,
        mode: r.mode.clone(),
        // A /proc-discovered engine has no metadata file, so fall back to the recorded port
        // and then to the profile's own; never report 0, which the UI would print as
        // "socks5 127.0.0.1:0".
        port: r
            .socks_port
            .unwrap_or_else(|| engine::current_port(&paths.engine_pid, profile_port)),
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
        exit_up: running.as_ref().map(|r| r.mode == "exit").unwrap_or(false),
        proxy_on,
        proxy_system_mode,
        issue: engine::connection_issue(&paths.engine_log),
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
    fn tun_down_is_safe_when_nothing_is_up() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(tmp.path());
        let down = tun_down(&p).unwrap();
        assert!(!down.was_up);
        assert_eq!(down.engine_stopped, None);
    }

    #[test]
    fn status_reports_stopped_when_nothing_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(tmp.path());
        let s = status(&p).unwrap();
        // `status` falls back to a /proc scan, so a *real* engine left running on the
        // developer's machine is legitimately reported. What must never happen is an engine
        // invented out of the empty state dir, so the invariant is: anything reported has to
        // be a live `openflux-engine` process.
        if let Some(e) = &s.engine {
            let cmdline = std::fs::read(format!("/proc/{}/cmdline", e.pid)).unwrap();
            assert!(
                String::from_utf8_lossy(&cmdline).contains("openflux-engine"),
                "engine {e:?} is not a live engine process"
            );
        }
        assert!(!s.tun_up);
        assert!(!s.proxy_on);
    }
}
