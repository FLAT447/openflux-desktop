//! Tauri GUI backend for OpenFlux. Reuses the same `openflux::actions` operations as the
//! CLI/TUI, so the three frontends can never drift apart. Privileged TUN toggles are
//! delegated to `pkexec <self> tun on|off`, which re-invokes this same binary headlessly
//! (see `gui/src/main.rs`).

use std::path::PathBuf;
use std::sync::Mutex;

use serde::Serialize;
use tauri::Manager;

use openflux::actions;
use openflux::config::{AppConfig, Codec, Profile, ProfileMode, Transport};
use openflux::engine;
use openflux::paths::Paths;
use openflux::{FWMARK, TUN_NAME};

#[derive(Debug)]
pub struct Ctx {
    pub paths: Paths,
    pub engine_bin: PathBuf,
    /// Deep-link / startup message (e.g. "imported profile …") surfaced to the frontend
    /// once via `take_notice`.
    pub notice: Mutex<Option<String>>,
}

/// Engine lookup: installed setcap'd binary first (Unix), then the engine shipped beside
/// this binary (portable release layout), then PATH. Honors the platform executable
/// suffix so the sibling lookup works for `openflux-engine.exe` on Windows too.
pub fn resolve_engine() -> PathBuf {
    let prog = format!("openflux-engine{}", std::env::consts::EXE_SUFFIX);
    let mut candidates: Vec<PathBuf> = Vec::new();
    #[cfg(unix)]
    candidates.push(PathBuf::from("/usr/local/lib/openflux/openflux-engine"));
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.to_path_buf()))
    {
        candidates.push(dir.join(&prog));
    }
    if let Some(hit) = candidates.iter().find(|c| c.exists()) {
        return hit.clone();
    }
    PathBuf::from(prog)
}

#[cfg(unix)]
fn ensure_root_path() {
    let path = std::env::var("PATH").unwrap_or_default();
    if path.split(':').any(|p| p == "/usr/sbin" || p == "/sbin") {
        return;
    }
    std::env::set_var("PATH", format!("/usr/sbin:/sbin:{path}"));
}

#[derive(Serialize)]
struct ProfileView {
    name: String,
    mode: String,
    transport: String,
    codec: String,
    doc_url: String,
    doc_urls: Vec<String>,
    active: bool,
}

#[derive(Serialize)]
struct EngineView {
    pid: i32,
    mode: String,
    port: u16,
}

#[derive(Serialize)]
struct StatusView {
    active_profile: Option<String>,
    engine: Option<EngineView>,
    tun_up: bool,
    /// True when the running engine is an exit node (no SOCKS listener).
    exit_up: bool,
    proxy_on: bool,
    proxy_system_mode: Option<String>,
    /// Tunnel problem read from the engine log (reason/detail pair, localized in the UI).
    issue_reason: Option<String>,
    issue_detail: Option<String>,
    /// Persisted "verbose engine log" preference.
    debug: bool,
    /// "active" while packets move, "dropping" while the transport is down and everything
    /// routed into the tunnel is being discarded. None until the engine logs a verdict.
    tunnel_state: Option<String>,
}

fn to_rich<T>(result: Result<T, anyhow::Error>) -> Result<T, String> {
    result.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn profiles(ctx: tauri::State<'_, Ctx>) -> Result<Vec<ProfileView>, String> {
    let cfg = to_rich(AppConfig::load(&ctx.paths.config_file))?;
    Ok(cfg
        .profiles
        .iter()
        .map(|p| ProfileView {
            name: p.name.clone(),
            mode: match p.mode {
                ProfileMode::Manual => "manual".to_string(),
                ProfileMode::Key => "key".to_string(),
            },
            transport: p.transport.to_string(),
            codec: p.codec.to_string(),
            doc_url: p.doc_url.clone(),
            doc_urls: p.doc_urls.clone(),
            active: cfg.active_profile.as_deref() == Some(p.name.as_str()),
        })
        .collect())
}

#[tauri::command]
fn status(ctx: tauri::State<'_, Ctx>) -> Result<StatusView, String> {
    let s = to_rich(actions::status(&ctx.paths))?;
    Ok(StatusView {
        active_profile: s.active_profile.clone(),
        engine: s.engine.map(|e| EngineView {
            pid: e.pid,
            mode: e.mode.clone(),
            port: e.port,
        }),
        tun_up: s.tun_up,
        exit_up: s.exit_up,
        proxy_on: s.proxy_on,
        proxy_system_mode: s.proxy_system_mode.clone(),
        issue_reason: s.issue.as_ref().map(|i| i.reason.clone()),
        issue_detail: s.issue.as_ref().map(|i| i.detail.clone()),
        debug: actions::load(&ctx.paths).map(|c| c.debug).unwrap_or(false),
        tunnel_state: engine::tunnel_state(&ctx.paths.engine_log).map(|s| s.as_str().to_string()),
    })
}

/// Persist the verbose-logging preference. It applies to the next engine spawn, so the
/// engine must be restarted for it to take effect.
/// The machine-wide settings: SOCKS5 port, TUN DNS and split tunneling. They describe the
/// host rather than an account, so every profile uses them.
#[derive(serde::Serialize)]
struct Settings {
    socks_port: u16,
    dns: String,
    split_mode: String,
    split_domains: String,
}

#[tauri::command]
fn settings(ctx: tauri::State<'_, Ctx>) -> Result<Settings, String> {
    let cfg = to_rich(actions::load(&ctx.paths))?;
    let split_mode = if cfg.split_enabled() {
        cfg.split_mode.clone()
    } else {
        "none".to_string()
    };
    Ok(Settings {
        socks_port: cfg.socks_port,
        dns: cfg.dns.clone(),
        split_mode,
        split_domains: cfg.split_csv(),
    })
}

#[tauri::command]
fn settings_set(
    ctx: tauri::State<'_, Ctx>,
    socks_port: Option<u16>,
    dns: Option<String>,
    split_mode: Option<String>,
    split_domains: Option<String>,
) -> Result<String, String> {
    let mut cfg = to_rich(actions::load(&ctx.paths))?;
    if let Some(port) = socks_port {
        cfg.socks_port = port;
    }
    if let Some(dns) = dns {
        let dns = dns.trim().to_string();
        if dns.is_empty() {
            return Err("DNS must not be empty".to_string());
        }
        cfg.dns = dns;
    }
    if let Some(mode) = split_mode {
        cfg.split_mode = if mode == "none" { String::new() } else { mode };
        if cfg.split_mode.is_empty() {
            cfg.split_sites.clear();
        }
    }
    if let Some(domains) = split_domains {
        cfg.split_sites = openflux::config::normalize_split_domains(&domains);
    }
    to_rich(cfg.validate())?;
    if !cfg.split_enabled() {
        cfg.split_sites.clear();
    }
    to_rich(actions::save(&ctx.paths, &cfg))?;
    Ok(actions::settings_summary(&cfg))
}

#[tauri::command]
fn set_debug(ctx: tauri::State<'_, Ctx>, enabled: bool) -> Result<bool, String> {
    let mut cfg = to_rich(actions::load(&ctx.paths))?;
    cfg.debug = enabled;
    to_rich(actions::save(&ctx.paths, &cfg))?;
    Ok(enabled)
}

#[tauri::command]
fn set_active(ctx: tauri::State<'_, Ctx>, name: String) -> Result<String, String> {
    to_rich(actions::set_active(&ctx.paths, &name))
}

fn parse_transport_or_default(value: Option<&str>) -> Result<Transport, String> {
    Ok(value
        .map(|value| {
            value
                .parse::<Transport>()
                .map_err(|error| error.to_string())
        })
        .transpose()?
        .unwrap_or_default())
}

fn parse_codec_or_default(value: Option<&str>) -> Result<Codec, String> {
    Ok(value
        .map(|value| value.parse::<Codec>().map_err(|error| error.to_string()))
        .transpose()?
        .unwrap_or_default())
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
fn add_profile(
    ctx: tauri::State<'_, Ctx>,
    name: String,
    mode: String,
    transport: Option<String>,
    codec: Option<String>,
    doc_urls: Option<String>,
    max_token: Option<String>,
    max_uid: Option<String>,
    doc_url: Option<String>,
    control_url: Option<String>,
    key_token: Option<String>,
    mtu: Option<u32>,
    streams: Option<u16>,
    captcha_solve_mode: Option<String>,
) -> Result<String, String> {
    let transport = parse_transport_or_default(transport.as_deref())?;
    let codec = parse_codec_or_default(codec.as_deref())?;
    let doc_urls = doc_urls
        .as_deref()
        .map(openflux::config::parse_doc_urls)
        .unwrap_or_default();
    let mut cfg = to_rich(actions::load(&ctx.paths))?;
    if cfg.get(&name).is_some() {
        return Err(format!("profile '{name}' already exists"));
    }
    if name.trim().is_empty() {
        return Err("profile name is required".to_string());
    }
    let mut profile = match mode.as_str() {
        "manual" => {
            let url = if transport == Transport::Oneme || transport == Transport::YandexMultistream
            {
                String::new()
            } else {
                let url = doc_url.unwrap_or_default();
                if url.trim().is_empty() {
                    return Err("doc URL is required for this transport".to_string());
                }
                url
            };
            Profile::manual(&name, &url)
        }
        "key" => {
            let control = control_url.unwrap_or_default();
            let token = key_token.unwrap_or_default();
            if control.trim().is_empty() || token.trim().is_empty() {
                return Err("control URL and key token are required for key profiles".to_string());
            }
            Profile::key(&name, &control, &token)
        }
        other => return Err(format!("unknown mode '{other}'")),
    };
    profile.transport = transport;
    profile.codec = codec;
    profile.doc_urls = doc_urls;
    if let Some(value) = max_token {
        profile.max_token = value;
    }
    if let Some(value) = max_uid {
        profile.max_uid = value;
    }
    if let Some(m) = mtu {
        profile.mtu = m;
    }
    if let Some(s) = streams {
        profile.streams = s;
    } else if transport == Transport::YandexMultistream && profile.doc_urls.len() >= 2 {
        profile.streams = u16::try_from(profile.doc_urls.len()).unwrap_or(u16::MAX);
    }
    if let Some(mode) = captcha_solve_mode {
        profile.captcha_solve_mode = openflux::config::normalize_captcha_mode(&mode);
    }
    let profile_transport = profile.transport;
    to_rich(cfg.add(profile))?;
    to_rich(actions::save(&ctx.paths, &cfg))?;
    Ok(format!(
        "profile '{name}' added (transport={profile_transport})"
    ))
}

#[tauri::command]
fn remove_profile(ctx: tauri::State<'_, Ctx>, name: String) -> Result<String, String> {
    let mut cfg = to_rich(actions::load(&ctx.paths))?;
    to_rich(cfg.remove(&name))?;
    to_rich(actions::save(&ctx.paths, &cfg))?;
    Ok(format!("profile '{name}' removed"))
}

fn mode_str(mode: &ProfileMode) -> &'static str {
    match mode {
        ProfileMode::Manual => "manual",
        ProfileMode::Key => "key",
    }
}

/// Import a profile from an `openflux://import?data=...` link or a raw base64url payload.
/// Mirrors the CLI `openflux import <link>`.
#[tauri::command]
fn import_link(ctx: tauri::State<'_, Ctx>, link: String) -> Result<String, String> {
    let imported = to_rich(openflux::import::import_link(&link))?;
    let profile = imported.profile;
    let mut cfg = to_rich(actions::load(&ctx.paths))?;
    if cfg.get(&profile.name).is_some() {
        return Err(format!("profile '{}' already exists", profile.name));
    }
    to_rich(cfg.add(profile.clone()))?;
    // A link may carry a DNS upstream; that is a machine-wide setting, so it is stored
    // globally rather than on the profile.
    let dns_note = match imported.dns.filter(|d| !d.trim().is_empty()) {
        Some(dns) => {
            cfg.dns = dns.trim().to_string();
            Some(format!("\n  dns={}", cfg.dns))
        }
        None => None,
    };
    to_rich(cfg.validate())?;
    to_rich(actions::save(&ctx.paths, &cfg))?;
    Ok(format!(
        "imported profile '{}' ({}):\n  control_url={}\n  transport={}\n  codec={}\n  doc_url={}\n  e2e={}{}",
        profile.name,
        mode_str(&profile.mode),
        profile.control_url,
        profile.transport,
        profile.codec,
        profile.doc_url,
        profile.e2e_encryption,
        dns_note.as_deref().unwrap_or("")
    ))
}

/// Returns messages already consumed, leaving the notice empty for the next deep link.
#[tauri::command]
fn take_notice(ctx: tauri::State<'_, Ctx>) -> Option<String> {
    ctx.notice.lock().ok().and_then(|mut n| n.take())
}

// Long-running operations (engine spawn waits up to READY_TIMEOUT, pkexec blocks until the
// polkit dialogue is answered) must never run on the webview's main thread — that is what
// made the UI freeze for the whole TUN setup. They are async so Tauri schedules them on the
// async runtime while the window keeps painting.
#[tauri::command]
async fn connect(ctx: tauri::State<'_, Ctx>, name: Option<String>) -> Result<String, String> {
    to_rich(actions::connect(
        &ctx.paths,
        &ctx.engine_bin,
        name.as_deref(),
        None,
        false,
    ))
    .map(|o| {
        format!(
            "connected '{}' (pid {}), socks 127.0.0.1:{}\n  next: TUN (needs root) or `proxy on`",
            o.profile, o.pid, o.port
        )
    })
}

#[tauri::command]
async fn disconnect(ctx: tauri::State<'_, Ctx>) -> Result<String, String> {
    // A TUN engine runs as root, so an unprivileged SIGTERM to it is refused (EPERM) and the
    // button would only print a warning while the tunnel kept holding the machine's routing.
    // Hand the whole teardown to the privileged entry point, the same one the TUN toggle uses.
    #[cfg(unix)]
    let mut privileged = String::new();
    #[cfg(unix)]
    {
        let st = to_rich(actions::status(&ctx.paths))?;
        if st.tun_up {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let out = std::process::Command::new("pkexec")
                .arg(&exe)
                .args(["disconnect"])
                .output()
                .map_err(|e| format!("spawn pkexec: {e}"))?;
            if !out.status.success() {
                let text = String::from_utf8_lossy(&out.stderr).trim().to_string();
                return Err(if text.is_empty() {
                    format!("pkexec exited with {}", out.status)
                } else {
                    text
                });
            }
            privileged = String::from_utf8_lossy(&out.stdout).trim().to_string();
        }
    }
    // Keep the unprivileged pass even after the privileged one: under pkexec HOME points at
    // root, so the system proxy and any pidfile of ours live in the *user's* state dir and
    // only this pass can restore them. The engine itself is already gone by then, and
    // `discover_running` finds nothing even if the privileged run used a different state dir.
    let local = to_rich(actions::disconnect(&ctx.paths))?;
    Ok(match (privileged.is_empty(), local.is_empty()) {
        (true, true) => local,
        (true, false) => local,
        (false, true) => privileged,
        (false, false) => format!("{privileged}\n{local}"),
    })
}

#[tauri::command]
async fn proxy_toggle(ctx: tauri::State<'_, Ctx>) -> Result<String, String> {
    let on = to_rich(actions::status(&ctx.paths))?.proxy_on;
    if on {
        return Ok(match to_rich(actions::proxy_down(&ctx.paths))? {
            true => "system proxy restored".to_string(),
            false => "system proxy not enabled".to_string(),
        });
    }
    match to_rich(actions::proxy_up(&ctx.paths))? {
        None => Ok("system proxy already enabled".to_string()),
        Some(p) => Ok(format!("system proxy enabled: socks 127.0.0.1:{}", p.port)),
    }
}

#[tauri::command]
async fn tun_toggle(app: tauri::AppHandle) -> Result<String, String> {
    // Unix: delegate the privileged toggle to `pkexec <self> tun on|off` (root PATH fix
    // in `headless_command`). Windows: TUN is not implemented; degrade to an honest message
    // until the Wintun integration lands.
    #[cfg(unix)]
    {
        let ctx = app.state::<Ctx>();
        let st = to_rich(actions::status(&ctx.paths))?;
        let sub = if st.tun_up { "off" } else { "on" };
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let out = std::process::Command::new("pkexec")
            .arg(&exe)
            .args(["tun", sub])
            .output()
            .map_err(|e| format!("spawn pkexec: {e}"))?;
        if !out.status.success() {
            let text = String::from_utf8_lossy(&out.stderr);
            let text = text.trim();
            return Err(if text.is_empty() {
                format!("pkexec exited with {}", out.status)
            } else {
                text.to_string()
            });
        }
        Ok(format!(
            "TUN mode {}",
            if sub == "on" {
                format!("up: interface {TUN_NAME}, fwmark {FWMARK:#x}")
            } else {
                "down".to_string()
            }
        ))
    }
    #[cfg(windows)]
    {
        // Windows TUN needs an elevated process, which the GUI is not. Delegate the
        // privileged toggle to `ShellExecuteW("runas") <self> tun on|off` (the same
        // headless entry point pkexec uses on Unix, UAC-gated). We cannot capture the
        // child's stderr, so success is declared optimistically and confirmed by a status
        // poll; failures surface via the engine log / status.
        use std::os::windows::ffi::OsStrExt;
        let ctx = app.state::<Ctx>();
        let st = to_rich(actions::status(&ctx.paths))?;
        let sub = if st.tun_up { "off" } else { "on" };
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let params = format!("tun {sub}");
        let mut exe_w: Vec<u16> = exe
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut params_w: Vec<u16> = std::ffi::OsStr::new(&params)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut verb: Vec<u16> = "runas".encode_utf16().chain(std::iter::once(0)).collect();
        let mut dir: Vec<u16> = vec![0];
        // SAFETY: all argument buffers are NUL-terminated wide strings; hwnd nil.
        let rc = unsafe {
            windows_sys::Win32::UI::Shell::ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_mut_ptr(),
                exe_w.as_mut_ptr(),
                params_w.as_mut_ptr(),
                dir.as_mut_ptr(),
                windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
            ) as isize
        };
        if rc <= 32 {
            let msg = match rc as i32 {
                5 => "UAC elevation was declined (access denied)".to_string(),
                33 => "the elevated helper could not be located".to_string(),
                _ => format!("ShellExecuteW(runas) failed with code {rc}"),
            };
            return Err(msg);
        }
        // The elevated child runs in its own process; give it a moment to appear in the
        // pidfile so the frontend refreshes truthful state instead of a race.
        for _ in 0..15 {
            let s = to_rich(actions::status(&ctx.paths))?;
            if s.tun_up != st.tun_up {
                return Ok(if s.tun_up {
                    "TUN mode up (elevated)".to_string()
                } else {
                    "TUN mode down".to_string()
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        Ok(format!(
            "TUN {} requested; if it does not appear shortly, see `openflux logs`",
            sub
        ))
    }
}

#[tauri::command]
fn log_tail(_ctx: tauri::State<'_, Ctx>, lines: Option<usize>) -> Result<String, String> {
    let ctx = _ctx.inner();
    let path = &ctx.paths.engine_log;
    if !path.exists() {
        return Ok(String::new());
    }
    let count = lines.unwrap_or(300).max(1);
    let text = to_rich(engine::tail(path, count * 120))?;
    let printed: Vec<&str> = text
        .lines()
        .skip(text.lines().count().saturating_sub(count))
        .collect();
    Ok(printed.join("\n"))
}

fn run_gui(ctx: Ctx) {
    tauri::Builder::default()
        .manage(ctx)
        .invoke_handler(tauri::generate_handler![
            profiles,
            status,
            set_active,
            add_profile,
            remove_profile,
            import_link,
            take_notice,
            connect,
            disconnect,
            proxy_toggle,
            tun_toggle,
            settings,
            settings_set,
            set_debug,
            log_tail
        ])
        .run(tauri::generate_context!())
        .expect("failed to run the OpenFlux GUI");
}

/// Entry point for the interactive GUI. `notice` carries a deep-link import result (or an
/// openflux:// URL that was already imported) to surface in the window.
pub fn run(notice: Option<String>) {
    // pkexec (and some sudo configs) trim root PATH, dropping /usr/sbin:/sbin where `ip`
    // lives; ensure it so the headless pkexec child can find iproute2. Unix-only: Windows
    // has no pkexec/iproute2 in this code path.
    #[cfg(unix)]
    if nix::unistd::geteuid().is_root() {
        ensure_root_path();
    }
    let paths = match Paths::discover() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("openflux: can't resolve config paths: {e:#}");
            std::process::exit(1);
        }
    };
    if let Err(e) = paths.ensure_dirs() {
        eprintln!("openflux: {e:#}");
        std::process::exit(1);
    }
    run_gui(Ctx {
        paths,
        engine_bin: resolve_engine(),
        notice: Mutex::new(notice),
    });
}

/// Headless deep-link import: `openflux-gui openflux://import?data=...`. Stores the
/// profile like a click on the config Import link would, and returns a human-readable
/// result message (errors prefixed with "openflux:").
pub fn headless_import(link: &str) -> String {
    let paths = match Paths::discover() {
        Ok(p) => p,
        Err(e) => return format!("openflux: {e:#}"),
    };
    if let Err(e) = paths.ensure_dirs() {
        return format!("openflux: {e:#}");
    }
    let imported = match openflux::import::import_link(link) {
        Ok(p) => p,
        Err(e) => return format!("openflux: {e:#}"),
    };
    let profile = imported.profile;
    let mut cfg = match actions::load(&paths) {
        Ok(c) => c,
        Err(e) => return format!("openflux: {e:#}"),
    };
    if cfg.get(&profile.name).is_some() {
        return format!("openflux: profile '{}' already exists", profile.name);
    }
    if let Err(e) = cfg.add(profile.clone()) {
        return format!("openflux: {e:#}");
    }
    if let Some(dns) = imported.dns.filter(|d| !d.trim().is_empty()) {
        cfg.dns = dns.trim().to_string();
    }
    if let Err(e) = cfg.validate() {
        return format!("openflux: {e:#}");
    }
    if let Err(e) = actions::save(&paths, &cfg) {
        return format!("openflux: {e:#}");
    }
    format!(
        "imported profile '{}' ({}):\n  control_url={}\n  transport={}\n  codec={}\n  doc_url={}\n  e2e={}\n  active: {}\n  settings: {}",
        profile.name,
        mode_str(&profile.mode),
        profile.control_url,
        profile.transport,
        profile.codec,
        profile.doc_url,
        profile.e2e_encryption,
        cfg.active_profile.as_deref().unwrap_or("<none>"),
        actions::settings_summary(&cfg)
    )
}

/// Headless privileged path: `openflux-gui tun on|off` / `openflux-gui disconnect` invoked by
/// the GUI through pkexec. The Disconnect button needs it because a TUN engine runs as root,
/// and an unprivileged SIGTERM to it is refused - the button would report a warning while the
/// tunnel kept holding the machine's routing.
pub fn headless_command() -> i32 {
    #[cfg(unix)]
    if nix::unistd::geteuid().is_root() {
        ensure_root_path();
    }
    let paths = match Paths::discover() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("openflux: {e:#}");
            return 1;
        }
    };
    if let Err(e) = paths.ensure_dirs() {
        eprintln!("openflux: {e:#}");
        return 1;
    }
    let engine_bin = resolve_engine();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("tun") => {}
        Some("disconnect") => {
            // Full teardown as root: routing, system proxy and the engine itself.
            return match actions::disconnect(&paths) {
                Ok(text) => {
                    if !text.is_empty() {
                        println!("{text}");
                    }
                    0
                }
                Err(e) => {
                    eprintln!("openflux: {e:#}");
                    1
                }
            };
        }
        _ => {
            eprintln!("openflux: headless mode expects `tun on` / `tun off` / `disconnect`");
            return 2;
        }
    }
    match args.get(1).map(String::as_str) {
        Some("on") => {
            let debug = std::env::var_os("OPENFLUX_DEBUG").is_some();
            match actions::tun_up(&paths, &engine_bin, None, debug) {
                Ok(t) => {
                    println!(
                        "TUN mode up (pid {}), interface '{TUN_NAME}', fwmark {FWMARK:#x}, dns {}",
                        t.pid, t.dns
                    );
                    0
                }
                Err(e) => {
                    eprintln!("openflux: {e:#}");
                    1
                }
            }
        }
        Some("off") => match actions::tun_down(&paths) {
            Ok(down) => {
                if down.was_up {
                    println!("TUN mode down");
                } else {
                    println!("TUN already down");
                }
                if let Some(pid) = down.engine_stopped {
                    println!("engine stopped (was pid {pid})");
                }
                if let Some(w) = down.engine_warning {
                    println!("warning: {w}");
                }
                0
            }
            Err(e) => {
                eprintln!("openflux: {e:#}");
                1
            }
        },
        other => {
            eprintln!("openflux: unknown tun subcommand {other:?}");
            2
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_parsing_covers_every_known_value() {
        for transport in Transport::ALL {
            assert_eq!(
                parse_transport_or_default(Some(transport.as_str())).unwrap(),
                *transport
            );
        }
        assert_eq!(parse_transport_or_default(None).unwrap(), Transport::Yandex);
        assert!(parse_transport_or_default(Some("carrier-pigeon")).is_err());
    }

    #[test]
    fn codec_parsing_defaults_to_legacy() {
        assert_eq!(parse_codec_or_default(None).unwrap(), Codec::Legacy);
        assert_eq!(
            parse_codec_or_default(Some("batched")).unwrap(),
            Codec::Batched
        );
        assert!(parse_codec_or_default(Some("zstd")).is_err());
    }

    #[test]
    fn doc_url_and_split_lists_use_the_shared_normalisation() {
        assert_eq!(
            openflux::config::parse_doc_urls(" https://a , ,https://b "),
            vec!["https://a".to_string(), "https://b".to_string()]
        );
        assert_eq!(
            openflux::config::normalize_split_domains("*.ya.ru, .yandex.ru"),
            vec!["*.ya.ru".to_string(), "yandex.ru".to_string()]
        );
    }

    #[test]
    fn captcha_mode_from_the_form_is_normalised_before_storage() {
        let mode = openflux::config::normalize_captcha_mode;
        assert_eq!(mode("off"), "");
        assert_eq!(mode(" headless_browser "), "headless_browser");

        let mut profile = Profile::manual("gui", "https://disk.yandex.ru/i/a");
        profile.transport = Transport::Mailru;
        profile.captcha_solve_mode = mode("headless_browser");
        assert!(profile.validate().is_err(), "mail.ru has no captcha solver");
    }
}
