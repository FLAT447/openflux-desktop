//! `openflux` CLI entrypoint. Keeps the CLI layer thin; real logic lives in the library
//! modules. The Go engine binary does the tunnel work; this binary manages it plus TUN
//! and system-proxy plumbing.

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use std::time::Duration;

use openflux::actions;
use openflux::config::{AppConfig, Codec, Profile, ProfileMode, Transport};
use openflux::engine;
use openflux::paths::Paths;
use openflux::proxy::Backend;
use openflux::resolve;
use openflux::{FWMARK, TUN_NAME};

mod tui;

#[derive(Parser)]
#[command(
    name = "openflux",
    version,
    about = "OpenFlux desktop client: TUN mode and system proxy on/off"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Override the config/state root (useful for tests and custom layouts).
    #[arg(long, global = true, env = "OPENFLUX_CONFIG_DIR")]
    config_dir: Option<PathBuf>,

    /// Path to the Go engine binary.
    #[arg(long, global = true, env = "OPENFLUX_ENGINE")]
    engine_bin: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Add a profile (manual doc URL or key controlplane).
    AddProfile(AddProfileArgs),
    /// Add a profile from an `openflux://import?data=...` share link.
    Import(ImportArgs),
    /// List all profiles.
    ListProfiles,
    /// Remove a profile.
    RmProfile(RmProfileArgs),
    /// Edit tunnel options of an existing profile (streams, MTU, captcha).
    EditProfile(EditProfileArgs),
    /// Machine-wide settings: SOCKS5 port, TUN DNS and split tunneling.
    #[command(subcommand)]
    Settings(SettingsCmd),
    /// Set the active profile.
    SetActive(ActiveArgs),
    /// Resolve the key against the controlplane and store the doc URL.
    CheckKey(ActiveArgs),
    /// Start the tunnel engine (SOCKS5) for the active/selected profile.
    Connect(ConnectArgs),
    /// Stop the engine and tear down any TUN/proxy layers.
    Disconnect,
    /// Exit-node mode: run the client's tunnel as a remote egress (no local proxy).
    #[command(subcommand)]
    Exit(ExitCmd),
    /// TUN (full-system capture) management. Needs root (sudo).
    #[command(subcommand)]
    Tun(TunCmd),
    /// System-proxy management.
    #[command(subcommand)]
    Proxy(ProxyCmd),
    /// Overall status: engine, TUN interface, system proxy, active profile.
    Status,
    /// Interactive terminal UI.
    Tui,
    /// Show engine/tun logs.
    Logs(LogsArgs),
}

#[derive(Args)]
struct AddProfileArgs {
    /// Profile name.
    name: String,
    /// Profile mode: manual (a document URL) or key (controlplane token).
    #[arg(long, value_enum, default_value_t = ProfileModeArg::Manual)]
    mode: ProfileModeArg,
    /// Transport implementation.
    #[arg(long, value_parser = ["yandex", "volga", "oneme", "yandex_multistream", "cupsonline", "mailru", "boards"])]
    transport: Option<String>,
    /// Wire codec for non-Yandex transports.
    #[arg(long, value_parser = ["legacy", "batched"])]
    codec: Option<String>,
    /// Comma-separated document URLs for yandex_multistream.
    #[arg(long)]
    doc_urls: Option<String>,
    /// OneMe MAX token.
    #[arg(long)]
    max_token: Option<String>,
    /// OneMe target user id.
    #[arg(long)]
    max_uid: Option<String>,
    /// Manual mode: document URL.
    #[arg(long)]
    doc_url: Option<String>,
    /// Key mode: controlplane base URL.
    #[arg(long)]
    control_url: Option<String>,
    /// Key mode: key token (the Yandex Docs shared-link token).
    #[arg(long)]
    key_token: Option<String>,
    /// Override the tunnel MTU.
    #[arg(long)]
    mtu: Option<u32>,
    /// Parallel WebSocket streams (multistream, 1-8).
    #[arg(long)]
    streams: Option<u16>,
    /// Yandex bot-check handling: "off" (default) or "headless_browser" (needs a
    /// local Chrome/Chromium install).
    #[arg(long, value_parser = ["off", "headless_browser"])]
    captcha_solve_mode: Option<String>,
}

#[derive(clap::ValueEnum, Clone, Copy)]
enum ProfileModeArg {
    Manual,
    Key,
}

impl From<ProfileModeArg> for ProfileMode {
    fn from(v: ProfileModeArg) -> Self {
        match v {
            ProfileModeArg::Manual => ProfileMode::Manual,
            ProfileModeArg::Key => ProfileMode::Key,
        }
    }
}

#[derive(Args)]
struct SettingsArgs {
    /// Local SOCKS5 listen port.
    #[arg(long)]
    socks_port: Option<u16>,
    /// TUN DNS upstream: plain "ip[:port]" or DoT/DoH via "tls://host" /
    /// "https://host/path".
    #[arg(long)]
    dns: Option<String>,
    /// Split-tunnel mode: "none" (off), "exclude" (listed sites bypass the tunnel) or
    /// "include" (only listed sites use the tunnel).
    #[arg(long, value_parser = ["none", "exclude", "include"])]
    split_mode: Option<String>,
    /// Comma-separated domains/IPs for --split-mode ("*.ya.ru,example.com"; "" clears).
    #[arg(long)]
    split_domains: Option<String>,
}

#[derive(Subcommand)]
enum SettingsCmd {
    /// Print the current global settings.
    Show,
    /// Change the global settings (only the flags you pass are touched).
    Set(SettingsArgs),
}

#[derive(Args)]
struct ActiveArgs {
    /// Profile name (defaults to the active profile).
    name: Option<String>,
}

#[derive(Args)]
struct RmProfileArgs {
    name: String,
}

#[derive(Args)]
struct EditProfileArgs {
    /// Target profile name.
    name: String,
    /// Transport implementation.
    #[arg(long, value_parser = ["yandex", "volga", "oneme", "yandex_multistream", "cupsonline", "mailru", "boards"])]
    transport: Option<String>,
    /// Wire codec for non-Yandex transports.
    #[arg(long, value_parser = ["legacy", "batched"])]
    codec: Option<String>,
    /// Comma-separated document URLs for yandex_multistream.
    #[arg(long)]
    doc_urls: Option<String>,
    /// Manual document URL for single-document transports.
    #[arg(long)]
    doc_url: Option<String>,
    /// OneMe MAX token.
    #[arg(long)]
    max_token: Option<String>,
    /// OneMe target user id.
    #[arg(long)]
    max_uid: Option<String>,
    /// Parallel WebSocket streams (multistream, 1-8).
    #[arg(long)]
    streams: Option<u16>,
    /// Yandex bot-check handling: "off" or "headless_browser" (needs a local
    /// Chrome/Chromium install).
    #[arg(long, value_parser = ["off", "headless_browser"])]
    captcha_solve_mode: Option<String>,
}

#[derive(Subcommand)]
enum ExitCmd {
    /// Start the exit-node engine for the active/selected profile.
    On,
    /// Stop the exit node.
    Off,
}

#[derive(Args)]
struct ImportArgs {
    /// The full openflux://import link, or the bare base64url payload.
    link: String,
}

#[derive(Args)]
struct ConnectArgs {
    /// Profile name (defaults to the active profile).
    name: Option<String>,
    /// Override the SOCKS5 listen port.
    #[arg(long)]
    socks_port: Option<u16>,
    /// Verbose engine logging for this run only (transport internals, packet traces).
    #[arg(long)]
    debug: bool,
}

#[derive(Subcommand)]
enum TunCmd {
    /// Bring the TUN capture up (engine must be running).
    On,
    /// Bring the TUN capture down.
    Off,
}

#[derive(Subcommand)]
enum ProxyCmd {
    /// Point the system proxy settings at the engine's SOCKS5.
    On,
    /// Restore the previous system proxy settings.
    Off,
}

#[derive(Args)]
struct LogsArgs {
    /// Keep tailing the log.
    #[arg(long)]
    follow: bool,
    /// Number of tailing lines to print up front.
    #[arg(long, default_value_t = 200)]
    lines: usize,
}

struct Ctx {
    paths: Paths,
    engine_bin: PathBuf,
}

fn main() -> Result<()> {
    // A closed stdout must end the process, not panic it with "failed printing to stdout".
    openflux::restore_default_sigpipe();
    // pkexec (and some sudo configs) trim the root PATH, dropping /usr/sbin:/sbin where
    // `ip` lives; prepend them so tun.rs can find it as root. Unix-only: Windows has no
    // iproute2 / pkexec equivalent in the same code path.
    #[cfg(unix)]
    if nix::unistd::geteuid().is_root() {
        ensure_root_path();
    }
    let cli = Cli::parse();
    let paths = match &cli.config_dir {
        Some(dir) => Paths::under(dir.clone()),
        None => Paths::discover()?,
    };
    paths.ensure_dirs()?;

    let ctx = Ctx {
        paths,
        engine_bin: cli.engine_bin.unwrap_or_else(resolve_engine),
    };

    match cli.command {
        Command::AddProfile(a) => add_profile(&ctx, a),
        Command::Import(a) => import_profile(&ctx, a),
        Command::ListProfiles => list_profiles(&ctx),
        Command::RmProfile(a) => rm_profile(&ctx, &a.name),
        Command::EditProfile(a) => edit_profile(&ctx, a),
        Command::Settings(cmd) => settings(&ctx, cmd),
        Command::SetActive(a) => {
            let name = a.name.context("set-active needs a profile name")?;
            println!("{}", actions::set_active(&ctx.paths, &name)?);
            Ok(())
        }
        Command::CheckKey(a) => check_key(&ctx, a),
        Command::Connect(a) => {
            let o = actions::connect(
                &ctx.paths,
                &ctx.engine_bin,
                a.name.as_deref(),
                a.socks_port,
                a.debug,
            )?;
            println!(
                "engine running (pid {}), SOCKS5 at 127.0.0.1:{}\n  next: `openflux tun on` (sudo) or `openflux proxy on`",
                o.pid, o.port
            );
            if let Some(issue) = engine::connection_issue(&ctx.paths.engine_log) {
                println!("  warning: {}", issue.describe());
            }
            Ok(())
        }
        Command::Disconnect => {
            println!("{}", actions::disconnect(&ctx.paths)?);
            Ok(())
        }
        Command::Exit(c) => match c {
            ExitCmd::On => {
                let debug = std::env::var_os("OPENFLUX_DEBUG").is_some();
                let e = actions::exit_up(&ctx.paths, &ctx.engine_bin, None, debug)?;
                println!(
                    "exit node up (pid {}), streams {}\n  this machine now egresses all tunnel traffic",
                    e.pid, e.streams
                );
                Ok(())
            }
            ExitCmd::Off => {
                if actions::exit_down(&ctx.paths)? {
                    println!("exit node stopped");
                } else {
                    println!("no exit node running");
                }
                Ok(())
            }
        },
        Command::Tun(c) => match c {
            TunCmd::On => {
                let debug = std::env::var_os("OPENFLUX_DEBUG").is_some();
                let t = actions::tun_up(&ctx.paths, &ctx.engine_bin, None, debug)?;
                println!(
                    "TUN mode up (pid {}), interface '{TUN_NAME}', fwmark {FWMARK:#x}, dns {}",
                    t.pid, t.dns
                );
                Ok(())
            }
            TunCmd::Off => {
                let down = actions::tun_down(&ctx.paths)?;
                if down.was_up {
                    println!("TUN mode down");
                }
                if let Some(pid) = down.engine_stopped {
                    println!("engine stopped (was pid {pid})");
                }
                if let Some(w) = down.engine_warning {
                    println!("warning: {w}");
                }
                Ok(())
            }
        },
        Command::Proxy(c) => match c {
            ProxyCmd::On => {
                match actions::proxy_up(&ctx.paths)? {
                    None => println!("system proxy already enabled"),
                    Some(p) => match p.backend {
                        Backend::Gsettings => {
                            println!("system proxy (GNOME) enabled: socks 127.0.0.1:{}", p.port)
                        }
                        Backend::Kde => {
                            println!("system proxy (KDE) enabled: socks 127.0.0.1:{}", p.port)
                        }
                        Backend::Unsupported => {
                            unreachable!("proxy_up bails for unsupported backend")
                        }
                    },
                }
                Ok(())
            }
            ProxyCmd::Off => {
                if actions::proxy_down(&ctx.paths)? {
                    println!("system proxy restored");
                } else {
                    println!("system proxy not enabled");
                }
                Ok(())
            }
        },
        Command::Status => status(&ctx),
        Command::Tui => tui::run(&ctx),
        Command::Logs(a) => logs(&ctx, a),
    }
}

fn which(binary: &str) -> PathBuf {
    PathBuf::from(binary)
}

fn resolve_engine() -> PathBuf {
    // Prefer the installed, setcap'd binary (Unix); then the engine shipped next to this
    // binary (portable release layout; honors the platform exe suffix); finally PATH.
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
    which(&prog)
}

#[cfg(unix)]
fn ensure_root_path() {
    let path = std::env::var("PATH").unwrap_or_default();
    let has_sbin = path.split(':').any(|p| p == "/usr/sbin" || p == "/sbin");
    if has_sbin {
        return;
    }
    std::env::set_var("PATH", format!("/usr/sbin:/sbin:{path}"));
}

fn load_config(ctx: &Ctx) -> Result<AppConfig> {
    AppConfig::load(&ctx.paths.config_file)
}

fn save_config(ctx: &Ctx, cfg: &AppConfig) -> Result<()> {
    cfg.save(&ctx.paths.config_file)
}

fn parse_transport(value: Option<&str>) -> Result<Transport> {
    value
        .map(|value| value.parse::<Transport>().map_err(anyhow::Error::msg))
        .transpose()?
        .map_or(Ok(Transport::Yandex), Ok)
}

fn parse_codec(value: Option<&str>) -> Result<Codec> {
    value
        .map(|value| value.parse::<Codec>().map_err(anyhow::Error::msg))
        .transpose()?
        .map_or(Ok(Codec::Legacy), Ok)
}

fn parse_csv_values(csv: &str) -> Vec<String> {
    openflux::config::parse_doc_urls(csv)
}

fn add_profile(ctx: &Ctx, a: AddProfileArgs) -> Result<()> {
    let transport = parse_transport(a.transport.as_deref())?;
    let codec = parse_codec(a.codec.as_deref())?;
    let doc_urls = a
        .doc_urls
        .as_deref()
        .map(parse_csv_values)
        .unwrap_or_default();
    let mut cfg = load_config(ctx)?;
    let mut profile = match a.mode.into() {
        ProfileMode::Manual => {
            let url = if transport == Transport::Oneme || transport == Transport::YandexMultistream
            {
                String::new()
            } else {
                a.doc_url
                    .as_deref()
                    .context("manual profiles need --doc-url")?
                    .to_string()
            };
            Profile::manual(&a.name, &url)
        }
        ProfileMode::Key => {
            let control = a
                .control_url
                .as_deref()
                .context("key profiles need --control-url")?
                .to_string();
            let token = a
                .key_token
                .as_deref()
                .context("key profiles need --key-token")?
                .to_string();
            Profile::key(&a.name, &control, &token)
        }
    };
    profile.transport = transport;
    profile.codec = codec;
    profile.doc_urls = doc_urls;
    if let Some(value) = a.max_token {
        profile.max_token = value;
    }
    if let Some(value) = a.max_uid {
        profile.max_uid = value;
    }
    if let Some(mtu) = a.mtu {
        profile.mtu = mtu;
    }
    if let Some(streams) = a.streams {
        profile.streams = streams;
    } else if transport == Transport::YandexMultistream && profile.doc_urls.len() >= 2 {
        profile.streams = u16::try_from(profile.doc_urls.len()).unwrap_or(u16::MAX);
    }
    if let Some(mode) = a.captcha_solve_mode {
        profile.captcha_solve_mode = openflux::config::normalize_captcha_mode(&mode);
    }
    cfg.add(profile)?;
    save_config(ctx, &cfg)?;
    println!("profile '{}' added (transport={})", a.name, transport);
    Ok(())
}

fn edit_profile(ctx: &Ctx, a: EditProfileArgs) -> Result<()> {
    let mut cfg = load_config(ctx)?;
    let profile = cfg
        .get_mut(&a.name)
        .with_context(|| format!("profile '{}' not found", a.name))?;
    if let Some(value) = a.transport {
        profile.transport = parse_transport(Some(&value))?;
    }
    if let Some(value) = a.codec {
        profile.codec = parse_codec(Some(&value))?;
    }
    if let Some(value) = a.doc_url {
        profile.doc_url = value;
    }
    if let Some(value) = a.doc_urls {
        profile.doc_urls = parse_csv_values(&value);
    }
    match profile.transport {
        Transport::YandexMultistream => {
            profile.doc_url.clear();
            if profile.doc_urls.len() >= 2 {
                profile.streams = u16::try_from(profile.doc_urls.len()).unwrap_or(u16::MAX);
            }
        }
        Transport::Oneme => {
            profile.doc_url.clear();
            profile.doc_urls.clear();
        }
        _ => profile.doc_urls.clear(),
    }
    if let Some(value) = a.max_token {
        profile.max_token = value;
    }
    if let Some(value) = a.max_uid {
        profile.max_uid = value;
    }
    if let Some(streams) = a.streams {
        profile.streams = streams;
    }
    match a.captcha_solve_mode {
        Some(value) => {
            profile.captcha_solve_mode = openflux::config::normalize_captcha_mode(&value)
        }
        None if !profile.transport.supports_captcha_solve() => profile.captcha_solve_mode.clear(),
        None => {}
    }
    profile.validate()?;
    let (transport, streams, mtu, captcha_solve_mode) = (
        profile.transport,
        profile.streams,
        profile.mtu,
        profile.captcha_solve_mode.clone(),
    );
    // Every value printed below is copied out above, so the borrow of `cfg` is already dead
    // here and the config can be written back.
    save_config(ctx, &cfg)?;
    println!(
        "profile '{}' updated: transport={transport}, streams={streams}, mtu={mtu}, captcha_solve_mode={}",
        a.name,
        if captcha_solve_mode.is_empty() { "off" } else { &captcha_solve_mode },
    );
    println!("note: DNS, split and the SOCKS5 port are global - see `openflux settings`");
    Ok(())
}

/// Parse a comma-separated split list, trimming leading dots ("*.ya.ru, .yandex.ru"
/// normalise to "*.ya.ru,yandex.ru").
fn parse_split_domains(csv: &str) -> Vec<String> {
    openflux::config::normalize_split_domains(csv)
}

fn import_profile(ctx: &Ctx, a: ImportArgs) -> Result<()> {
    let imported = openflux::import::import_link(&a.link)?;
    let profile = imported.profile;
    let mut cfg = load_config(ctx)?;
    // DNS is a machine-wide setting, so a link that carries one configures it globally
    // instead of ending up as per-profile data.
    let imported_dns = imported.dns.filter(|d| !d.trim().is_empty());
    if cfg.get(&profile.name).is_some() {
        bail!(
            "profile '{}' already exists; remove it first (`openflux rm-profile {}`)",
            profile.name,
            profile.name
        );
    }
    cfg.add(profile.clone())?;
    if let Some(dns) = imported_dns {
        cfg.dns = dns;
    }
    cfg.validate()?;
    save_config(ctx, &cfg)?;
    println!(
        "imported profile '{}' ({}):\n  control_url={}\n  transport={}\n  codec={}\n  doc_url={}\n  e2e={}\n  active: {}",
        profile.name,
        match profile.mode {
            ProfileMode::Manual => "manual",
            ProfileMode::Key => "key",
        },
        profile.control_url,
        profile.transport,
        profile.codec,
        profile.doc_url,
        profile.e2e_encryption,
        cfg.active_profile.as_deref() == Some(&profile.name),
    );
    Ok(())
}

/// Print the machine-wide settings. DNS, split and the SOCKS5 port describe the machine
/// rather than the account, so they are stored once and shared by every profile.
fn show_settings(cfg: &openflux::config::AppConfig) {
    println!("{}", actions::settings_summary(cfg));
    if !cfg.split_enabled() && !cfg.split_sites.is_empty() {
        println!("note: a site list is stored but split is off; it is ignored until you set a mode");
    }
}

fn settings(ctx: &Ctx, cmd: SettingsCmd) -> Result<()> {
    match cmd {
        SettingsCmd::Show => {
            let cfg = load_config(ctx)?;
            show_settings(&cfg);
            Ok(())
        }
        SettingsCmd::Set(a) => {
            let mut cfg = load_config(ctx)?;
            if let Some(port) = a.socks_port {
                cfg.socks_port = port;
            }
            if let Some(dns) = a.dns {
                let dns = dns.trim().to_string();
                if dns.is_empty() {
                    bail!("--dns must not be empty");
                }
                cfg.dns = dns;
            }
            if let Some(mode) = a.split_mode {
                cfg.split_mode = if mode == "none" { String::new() } else { mode };
                if cfg.split_mode.is_empty() {
                    cfg.split_sites.clear();
                }
            }
            if let Some(domains) = a.split_domains {
                cfg.split_sites = parse_split_domains(&domains);
            }
            cfg.validate()?;
            if !cfg.split_enabled() {
                cfg.split_sites.clear();
            }
            save_config(ctx, &cfg)?;
            show_settings(&cfg);
            Ok(())
        }
    }
}

fn list_profiles(ctx: &Ctx) -> Result<()> {
    let cfg = load_config(ctx)?;
    if cfg.profiles.is_empty() {
        println!("no profiles; use `openflux add-profile`");
        return Ok(());
    }
    for p in &cfg.profiles {
        let active = if cfg.active_profile.as_deref() == Some(&p.name) {
            " (active)"
        } else {
            ""
        };
        let mode = match p.mode {
            ProfileMode::Manual => "manual",
            ProfileMode::Key => "key",
        };
        let doc = match p.transport {
            Transport::YandexMultistream if !p.doc_urls.is_empty() => p.doc_urls.join(","),
            _ if p.doc_url.is_empty() && p.mode == ProfileMode::Key => {
                "(not resolved; run `openflux check-key`)".to_string()
            }
            _ => p.doc_url.clone(),
        };
        println!(
            "- {}{}\n    mode={mode}\n    transport={}\n    codec={}\n    doc_url={doc}\n    captcha_solve_mode={}",
            p.name,
            active,
            p.transport,
            p.codec,
            if p.captcha_solve_mode.is_empty() { "off" } else { &p.captcha_solve_mode },
        );
    }
    Ok(())
}

fn rm_profile(ctx: &Ctx, name: &str) -> Result<()> {
    let mut cfg = load_config(ctx)?;
    cfg.remove(name)?;
    save_config(ctx, &cfg)?;
    println!("profile '{name}' removed");
    Ok(())
}

fn check_key(ctx: &Ctx, a: ActiveArgs) -> Result<()> {
    let mut cfg = load_config(ctx)?;
    let name = a
        .name
        .unwrap_or_else(|| cfg.active_profile.clone().unwrap_or_default());
    {
        let profile = cfg
            .get_mut(&name)
            .with_context(|| format!("profile '{name}' not found"))?;
        if profile.mode != ProfileMode::Key {
            bail!("profile '{name}' is a manual profile (no key)");
        }
        if profile.control_url.is_empty() || profile.key_token.is_empty() {
            bail!("profile '{name}' is missing control_url/key_token");
        }
    }

    // Copy the credentials so the mutable borrow doesn't outlive the save below.
    let (control_url, key_token) = {
        let p = cfg.get(&name).unwrap();
        (p.control_url.clone(), p.key_token.clone())
    };
    let result = resolve::resolve_key(&control_url, &key_token)?;
    let e2e = result.e2e_encryption;
    let transport = result.transport;
    {
        let p = cfg.get_mut(&name).unwrap();
        p.doc_url = result.doc_url.clone();
        p.doc_urls = result.doc_urls.clone();
        p.transport = transport;
        p.e2e_encryption = e2e;
        if transport == Transport::YandexMultistream && p.doc_urls.len() >= 2 {
            p.streams = u16::try_from(p.doc_urls.len()).unwrap_or(u16::MAX);
        }
        p.validate()?;
    }
    save_config(ctx, &cfg)?;

    println!("key resolved for '{}':", name);
    println!("  transport={transport}");
    println!("  doc_url={}", result.doc_url);
    println!("  e2e_encryption={e2e}");
    println!("key is active; run `openflux connect {name}`");
    Ok(())
}

fn status(ctx: &Ctx) -> Result<()> {
    let s = actions::status(&ctx.paths)?;
    println!(
        "profile: {}",
        s.active_profile.as_deref().unwrap_or("<none>")
    );

    match &s.engine {
        Some(e) if e.mode == "tun" => println!("engine: running (pid {}), mode=tun", e.pid),
        Some(e) if e.mode == "exit" => println!("engine: running (pid {}), mode=exit-node", e.pid),
        Some(e) => println!(
            "engine: running (pid {}), socks5=127.0.0.1:{}",
            e.pid, e.port
        ),
        None => println!("engine: stopped"),
    }

    println!("tun: {}", if s.tun_up { "up (openflux)" } else { "down" });
    if s.exit_up {
        println!("exit node: up (this machine is the egress for tunnel traffic)");
    }

    let proxy_mode = if s.proxy_on {
        "on".to_string()
    } else {
        match &s.proxy_system_mode {
            Some(m) if m != "none" => format!("off (system mode: {m})"),
            _ => "off".to_string(),
        }
    };
    println!("proxy: {proxy_mode}");
    println!("settings: {}", actions::settings_summary(&load_config(ctx)?));
    Ok(())
}

fn logs(ctx: &Ctx, a: LogsArgs) -> Result<()> {
    let path = &ctx.paths.engine_log;
    if !path.exists() {
        println!("no engine log yet; connect first");
        return Ok(());
    }
    let lines = a.lines.max(1);
    // Rough: 120 bytes per line is a fair average for log output.
    let text = engine::tail(path, lines * 120)?;
    let printed: Vec<&str> = text
        .lines()
        .skip(text.lines().count().saturating_sub(lines))
        .collect();
    println!("{}", printed.join("\n"));
    if a.follow {
        engine::follow(path, Duration::from_millis(500))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_parsing_covers_every_known_value() {
        for transport in Transport::ALL {
            assert_eq!(
                parse_transport(Some(transport.as_str())).unwrap(),
                *transport
            );
        }
        assert_eq!(parse_transport(None).unwrap(), Transport::Yandex);
        assert!(parse_transport(Some("carrier-pigeon")).is_err());
    }

    #[test]
    fn codec_parsing_defaults_to_legacy() {
        assert_eq!(parse_codec(None).unwrap(), Codec::Legacy);
        assert_eq!(parse_codec(Some("legacy")).unwrap(), Codec::Legacy);
        assert_eq!(parse_codec(Some("batched")).unwrap(), Codec::Batched);
        assert!(parse_codec(Some("zstd")).is_err());
    }

    #[test]
    fn csv_parsing_trims_and_drops_empties() {
        let values = parse_csv_values(" https://a , ,https://b,");
        assert_eq!(
            values,
            vec!["https://a".to_string(), "https://b".to_string()]
        );
        assert!(parse_csv_values("  ,  ").is_empty());
    }

    #[test]
    fn split_domain_parsing_normalises_leading_dots() {
        let domains = parse_split_domains("*.ya.ru, .yandex.ru ,example.com");
        assert_eq!(
            domains,
            vec![
                "*.ya.ru".to_string(),
                "yandex.ru".to_string(),
                "example.com".to_string()
            ]
        );
    }

    #[test]
    fn captcha_mode_normalisation_stores_off_as_empty() {
        let mode = openflux::config::normalize_captcha_mode;
        assert_eq!(mode("off"), "");
        assert_eq!(mode(" off "), "");
        assert_eq!(mode(""), "");
        assert_eq!(mode("headless_browser"), "headless_browser");
        assert_eq!(mode(" headless_browser "), "headless_browser");
    }
}
