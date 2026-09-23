//! `openflux` CLI entrypoint. Keeps the CLI layer thin; real logic lives in the library
//! modules. The Go engine binary does the tunnel work; this binary manages it plus TUN
//! and system-proxy plumbing.

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use std::time::Duration;

use openflux::actions;
use openflux::config::{AppConfig, Profile, ProfileMode};
use openflux::engine;
use openflux::paths::Paths;
use openflux::proxy::Backend;
use openflux::resolve;
use openflux::{FWMARK, TUN_NAME};

mod tui;

#[derive(Parser)]
#[command(name = "openflux", version, about = "OpenFlux desktop client: TUN mode and system proxy on/off")]
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
    /// Edit tunnel options of an existing profile (streams, DNS, split).
    EditProfile(EditProfileArgs),
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
    /// Profile mode: manual (a doc URL) or key (controlplane token).
    #[arg(long, value_enum, default_value_t = ProfileModeArg::Manual)]
    mode: ProfileModeArg,
    /// Manual mode: Yandex Docs URL.
    #[arg(long)]
    doc_url: Option<String>,
    /// Key mode: controlplane base URL.
    #[arg(long)]
    control_url: Option<String>,
    /// Key mode: key token (the Yandex Docs shared-link token).
    #[arg(long)]
    key_token: Option<String>,
    /// Override the SOCKS5 listen port.
    #[arg(long)]
    socks_port: Option<u16>,
    /// Override the tunnel MTU.
    #[arg(long)]
    mtu: Option<u32>,
    /// Parallel WebSocket streams (multistream, 1-8).
    #[arg(long)]
    streams: Option<u16>,
    /// DNS upstream for TUN mode: plain "ip[:port]" or DoT/DoH via "tls://host" /
    /// "https://host/path".
    #[arg(long)]
    dns: Option<String>,
    /// TUN split mode: "exclude" (listed sites bypass the tunnel) or "include" (only
    /// listed sites use it).
    #[arg(long, value_parser = ["exclude", "include"])]
    split_mode: Option<String>,
    /// Comma-separated domains/IPs for --split-mode ("*.ya.ru,example.com").
    #[arg(long)]
    split_domains: Option<String>,
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
    /// Parallel WebSocket streams (multistream, 1-8).
    #[arg(long)]
    streams: Option<u16>,
    /// DNS upstream for TUN mode: plain "ip[:port]" or DoT/DoH ("tls://host",
    /// "https://host/path").
    #[arg(long)]
    dns: Option<String>,
    /// TUN split mode: "none", "exclude" or "include".
    #[arg(long, value_parser = ["none", "exclude", "include"])]
    split_mode: Option<String>,
    /// Comma-separated domains/IPs for --split-mode ("" clears the list).
    #[arg(long)]
    split_domains: Option<String>,
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
            )?;
            println!(
                "engine running (pid {}), SOCKS5 at 127.0.0.1:{}\n  next: `openflux tun on` (sudo) or `openflux proxy on`",
                o.pid, o.port
            );
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
                if actions::tun_down(&ctx.paths)? {
                    println!("TUN mode down");
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

fn add_profile(ctx: &Ctx, a: AddProfileArgs) -> Result<()> {
    let mut cfg = load_config(ctx)?;
    let mut profile = match a.mode.into() {
        ProfileMode::Manual => {
            let url = a
                .doc_url
                .context("manual profiles need --doc-url")?;
            Profile::manual(&a.name, &url)
        }
        ProfileMode::Key => {
            let control = a
                .control_url
                .context("key profiles need --control-url")?;
            let token = a
                .key_token
                .context("key profiles need --key-token")?;
            Profile::key(&a.name, &control, &token)
        }
    };
    if let Some(port) = a.socks_port {
        profile.socks_port = port;
    }
    if let Some(mtu) = a.mtu {
        profile.mtu = mtu;
    }
    if let Some(streams) = a.streams {
        profile.streams = streams;
    }
    if let Some(dns) = a.dns {
        profile.dns_upstream = dns;
    }
    if let Some(mode) = a.split_mode {
        profile.split_mode = mode;
    }
    if let Some(domains) = a.split_domains {
        profile.split_sites = parse_split_domains(&domains);
    }
    cfg.add(profile)?;
    save_config(ctx, &cfg)?;
    println!("profile '{}' added", a.name);
    Ok(())
}

fn edit_profile(ctx: &Ctx, a: EditProfileArgs) -> Result<()> {
    let mut cfg = load_config(ctx)?;
    let profile = cfg
        .get_mut(&a.name)
        .with_context(|| format!("profile '{}' not found", a.name))?;
    if let Some(streams) = a.streams {
        profile.streams = streams;
    }
    if let Some(dns) = a.dns {
        profile.dns_upstream = dns;
    }
    if let Some(mode) = a.split_mode {
        profile.split_mode = if mode == "none" { String::new() } else { mode };
        if profile.split_mode.is_empty() {
            profile.split_sites.clear();
        }
    }
    if let Some(domains) = a.split_domains {
        profile.split_sites = parse_split_domains(&domains);
    }
    profile.validate()?;
    if profile.split_mode.is_empty() {
        profile.split_sites.clear();
    }
    let (streams, dns, split_mode, split_sites) = (
        profile.streams,
        profile.dns_upstream.clone(),
        profile.split_mode.clone(),
        profile.split_sites.clone(),
    );
    save_config(ctx, &cfg)?;
    println!(
        "profile '{}' updated: streams={}, dns={}, split_mode={}, split_sites=[{}]",
        a.name,
        streams,
        dns,
        if split_mode.is_empty() { "off" } else { &split_mode },
        split_sites.join(","),
    );
    Ok(())
}

/// Parse a comma-separated split list, trimming leading dots ("*.ya.ru, .yandex.ru"
/// normalise to "*.ya.ru,yandex.ru").
fn parse_split_domains(csv: &str) -> Vec<String> {
    csv.split(',')
        .map(|s| s.trim().trim_start_matches('.').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn import_profile(ctx: &Ctx, a: ImportArgs) -> Result<()> {
    let profile = openflux::import::import_link(&a.link)?;
    let mut cfg = load_config(ctx)?;
    if cfg.get(&profile.name).is_some() {
        bail!(
            "profile '{}' already exists; remove it first (`openflux rm-profile {}`)",
            profile.name,
            profile.name
        );
    }
    cfg.add(profile.clone())?;
    save_config(ctx, &cfg)?;
    println!(
        "imported profile '{}' ({}):\n  control_url={}\n  doc_url={}\n  e2e={}\n  active: {}",
        profile.name,
        match profile.mode {
            ProfileMode::Manual => "manual",
            ProfileMode::Key => "key",
        },
        profile.control_url,
        profile.doc_url,
        profile.e2e_encryption,
        cfg.active_profile.as_deref() == Some(&profile.name),
    );
    Ok(())
}

fn list_profiles(ctx: &Ctx) -> Result<()> {
    let cfg = load_config(ctx)?;
    if cfg.profiles.is_empty() {
        println!("no profiles; use `openflux add-profile`");
        return Ok(());
    }
    for p in &cfg.profiles {
        let active = if cfg.active_profile.as_deref() == Some(&p.name) { " (active)" } else { "" };
        let mode = match p.mode {
            ProfileMode::Manual => "manual",
            ProfileMode::Key => "key",
        };
        let doc = match &p.mode {
            ProfileMode::Manual => p.doc_url.clone(),
            ProfileMode::Key => p
                .doc_url
                .clone()
                .pipe_empty("(not resolved; run `openflux check-key`)"),
        };
        println!("- {}{}\n    mode={mode}\n    doc_url={doc}\n    socks_port={}",
            p.name, active, p.socks_port);
    }
    Ok(())
}

trait PipeEmpty {
    fn pipe_empty(self, alt: &str) -> String;
}

impl PipeEmpty for String {
    fn pipe_empty(self, alt: &str) -> String {
        if self.is_empty() {
            alt.to_string()
        } else {
            self
        }
    }
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
    let name = a.name.unwrap_or_else(|| cfg.active_profile.clone().unwrap_or_default());
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
    {
        let p = cfg.get_mut(&name).unwrap();
        p.doc_url = result.doc_url.clone();
        p.doc_urls = result.doc_urls.clone();
        p.e2e_encryption = e2e;
    }
    save_config(ctx, &cfg)?;

    println!("key resolved for '{}':", name);
    println!("  doc_url={}", result.doc_url);
    println!("  e2e_encryption={e2e}");
    println!("key is active; run `openflux connect {name}`");
    Ok(())
}

fn status(ctx: &Ctx) -> Result<()> {
    let s = actions::status(&ctx.paths)?;
    println!("profile: {}", s.active_profile.as_deref().unwrap_or("<none>"));

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
    let printed: Vec<&str> = text.lines().skip(text.lines().count().saturating_sub(lines)).collect();
    println!("{}", printed.join("\n"));
    if a.follow {
        engine::follow(path, Duration::from_millis(500))?;
    }
    Ok(())
}