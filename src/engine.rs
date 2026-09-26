//! Lifecycle of the Go engine child process (openflux-engine). The Rust CLI spawns it as a
//! detached daemon, waits for its SOCKS5 listener to come up, and records its pid so later
//! invocations can stop it or reuse the same port for the TUN/proxy layers.

use anyhow::{bail, Context, Result};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::config::{Codec, Profile, Transport, CAPTCHA_SOLVE_HEADLESS};
use crate::state;

/// A handle to the running engine's stdin, kept so a long-lived process that spawned the
/// engine (the Tauri GUI) can hand it solved-browser cookies as a `ProvideCookies` JSON
/// line. `None` until an engine is actually spawned with a piped stdin, so `provide_cookies`
/// fails early in CLI-only sessions, whose engine stdin closes when the CLI exits.
static ENGINE_STDIN: OnceLock<Mutex<Option<ChildStdin>>> = OnceLock::new();

/// Send a `ProvideCookies` command to the live engine: the given cookie string (collected
/// from a solved browser session) is fed to the engine, which applies it to subsequent
/// Yandex Doc fetches and reconnects. Returns an error when no engine stdin is piped, e.g.
/// the engine was started by a short-lived CLI process.
pub fn provide_cookies(cookie_str: &str) -> anyhow::Result<()> {
    use std::io::Write;
    let guard = ENGINE_STDIN
        .get()
        .ok_or_else(|| anyhow::anyhow!("engine stdin not piped (was it started by the CLI?)"))?;
    let mut stdin = guard.lock().unwrap_or_else(|p| p.into_inner());
    let stdin = stdin
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("engine stdin closed"))?;
    let line = serde_json::json!({
        "cmd": "ProvideCookies",
        "cookie_str": cookie_str,
    })
    .to_string();
    stdin
        .write_all(line.as_bytes())
        .and_then(|_| stdin.write_all(b"\n"))
        .and_then(|_| stdin.flush())
        .context("write ProvideCookies to engine stdin")?;
    Ok(())
}

pub const SOCK_MARK: i32 = crate::FWMARK as i32;

#[derive(Debug, Clone)]
pub struct TransportConfig {
    pub transport: Transport,
    pub codec: Codec,
    pub url: String,
    pub doc_urls: Vec<String>,
    pub max_token: String,
    pub max_uid: String,
    pub captcha_solve_mode: String,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            transport: Transport::Yandex,
            codec: Codec::Legacy,
            url: String::new(),
            doc_urls: Vec::new(),
            max_token: String::new(),
            max_uid: String::new(),
            captcha_solve_mode: String::new(),
        }
    }
}

impl TransportConfig {
    pub fn from_profile(profile: &Profile) -> Self {
        Self {
            transport: profile.transport,
            codec: profile.codec,
            url: profile.doc_url.clone(),
            doc_urls: profile.doc_urls.clone(),
            max_token: profile.max_token.clone(),
            max_uid: profile.max_uid.clone(),
            captcha_solve_mode: profile.captcha_solve_mode.trim().to_string(),
        }
    }

    pub fn apply(&self, cmd: &mut Command) {
        cmd.arg("--transport").arg(self.transport.as_str());
        cmd.arg("--codec").arg(self.codec.as_str());
        if self.transport == Transport::YandexMultistream {
            cmd.arg("--urls").arg(self.doc_urls.join(","));
        } else if !self.url.trim().is_empty() {
            cmd.arg("--url").arg(&self.url);
        }
        if !self.max_token.trim().is_empty() {
            cmd.arg("--max-token").arg(&self.max_token);
        }
        if !self.max_uid.trim().is_empty() {
            cmd.arg("--max-uid").arg(&self.max_uid);
        }
        if self.captcha_solve_mode.trim() == CAPTCHA_SOLVE_HEADLESS {
            cmd.arg("--captcha-solve-mode").arg(CAPTCHA_SOLVE_HEADLESS);
        }
    }
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub bin: PathBuf,
    pub transport: TransportConfig,
    pub socks_port: u16,
    pub token: Option<String>,
    pub mtu: Option<u32>,
    /// Parallel WebSocket streams; 1 means single-stream (the classic behaviour).
    pub streams: u16,
    pub debug: bool,
    pub log_file: PathBuf,
}

pub fn socks_addr(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

pub fn is_port_open(port: u16) -> bool {
    TcpStream::connect_timeout(
        &SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(500),
    )
    .is_ok()
}

/// Spawn the engine, wait until the SOCKS5 listener answers (or the process dies), then
/// persist the pid + port. Blocks until ready or `timeout`.
pub fn start(cfg: &EngineConfig, timeout: Duration) -> Result<i32> {
    // A previous invocation may already have this engine (and port) up.
    let pidfile = engine_pidfile(cfg);
    if let Some(pid) = current(&pidfile) {
        // Never let a SOCKS spawn clobber a live TUN session's pidfile - doing so used to
        // orphan the TUN engine and put its port out of reach of stop/disconnect.
        if current_mode(&pidfile).as_deref() == Some("tun") {
            bail!(
                "TUN mode is active (pid {pid}); stop it with `pkexec openflux tun off` (or t in the TUI) before connecting"
            );
        }
        if current_mode(&pidfile).as_deref() == Some("exit") {
            bail!(
                "an exit node is running (pid {pid}); stop it with `openflux exit off` before connecting"
            );
        }
        return Ok(pid);
    }

    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&cfg.log_file)
        .with_context(|| format!("open engine log {}", cfg.log_file.display()))?;

    let mut cmd = Command::new(&cfg.bin);
    cfg.transport.apply(&mut cmd);
    cmd.args(["--socks5", &socks_addr(cfg.socks_port)]);
    if SOCK_MARK != 0 {
        // Always mark: harmless without the TUN fwmark rule, required when TUN mode is on.
        cmd.arg("--sock-mark").arg(SOCK_MARK.to_string());
    }
    if let Some(token) = &cfg.token {
        if !token.is_empty() {
            cmd.arg("--token").arg(token);
        }
    }
    if let Some(mtu) = cfg.mtu {
        cmd.arg("--mtu").arg(mtu.to_string());
    }
    if cfg.streams != 1 {
        cmd.arg("--streams").arg(cfg.streams.to_string());
    }
    if cfg.debug {
        cmd.arg("--debug");
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::from(log.try_clone().context("clone engine log")?))
        .stderr(Stdio::from(log));

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn engine {}", cfg.bin.display()))?;
    *ENGINE_STDIN
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap() = child.stdin.take();

    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().context("wait engine")? {
            bail!(
                "engine exited early with {status}{}",
                engine_failure_hint(&cfg.log_file)
            );
        }
        if is_port_open(cfg.socks_port) {
            let pid = child.id() as i32;
            let meta = format!("socks_port={} mode=socks", cfg.socks_port);
            state::write_pid(&engine_pidfile(cfg), pid, Some(meta))?;
            return Ok(pid);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!(
                "engine did not become ready within {timeout:?}{}",
                engine_failure_hint(&cfg.log_file)
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Whether the tail of the engine log (from `pos` onward) already contains `needle`.
/// Advances `pos` so it can be polled cheaply without re-reading the whole file.
fn log_tail_contains(path: &Path, pos: &mut u64, needle: &str) -> std::io::Result<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::OpenOptions::new().read(true).open(path)?;
    let len = file.metadata()?.len();
    file.seek(SeekFrom::Start(*pos))?;
    let mut buf = Vec::with_capacity((len.saturating_sub(*pos)) as usize);
    file.take(len.saturating_sub(*pos)).read_to_end(&mut buf)?;
    *pos = len;
    Ok(buf.windows(needle.len()).any(|w| w == needle.as_bytes()))
}

/// Spawn the engine as an exit node: no SOCKS5 listener is created (there is no port to
/// probe), so readiness is declared once the first tunnel appears in the log. The pid is
/// recorded with `mode=exit`.
pub fn start_exit(cfg: &EngineConfig, timeout: Duration) -> Result<i32> {
    let pidfile = engine_pidfile(cfg);
    if let Some(pid) = current(&pidfile) {
        match current_mode(&pidfile).as_deref() {
            Some("exit") => return Ok(pid),
            Some(m) => {
                bail!("another engine mode is active ({m}); stop it before starting an exit node")
            }
            None => bail!("an untracked engine is running (pid {pid}); stop it first"),
        }
    }

    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&cfg.log_file)
        .with_context(|| format!("open engine log {}", cfg.log_file.display()))?;

    let mut cmd = Command::new(&cfg.bin);
    cfg.transport.apply(&mut cmd);
    cmd.arg("--exit");
    if let Some(token) = &cfg.token {
        if !token.is_empty() {
            cmd.arg("--token").arg(token);
        }
    }
    if let Some(mtu) = cfg.mtu {
        cmd.arg("--mtu").arg(mtu.to_string());
    }
    if cfg.streams != 1 {
        cmd.arg("--streams").arg(cfg.streams.to_string());
    }
    if cfg.debug {
        cmd.arg("--debug");
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::from(log.try_clone().context("clone engine log")?))
        .stderr(Stdio::from(log));

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn engine {}", cfg.bin.display()))?;

    *ENGINE_STDIN
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap() = child.stdin.take();

    let mut pos = std::fs::metadata(&cfg.log_file)
        .map(|m| m.len())
        .unwrap_or(0);
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().context("wait engine")? {
            bail!(
                "engine exited early with {status}{}",
                engine_failure_hint(&cfg.log_file)
            );
        }
        if log_tail_contains(&cfg.log_file, &mut pos, "event connected").unwrap_or(false) {
            let pid = child.id() as i32;
            state::write_pid(&engine_pidfile(cfg), pid, Some("mode=exit".to_string()))?;
            return Ok(pid);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!(
                "engine (exit node) did not become ready within {timeout:?}{}",
                engine_failure_hint(&cfg.log_file)
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Appends the engine's own fatal line to a spawn error, so a bad document URL for a new
/// transport ("boards: no hash in URL …", "cupsonline: …") reaches the user instead of a
/// bare exit status.
fn engine_failure_hint(log: &Path) -> String {
    match last_engine_error(log) {
        Some(line) => format!(": {line}"),
        None => String::new(),
    }
}

/// The most recent `[ENGINE]` line in the log, if the engine wrote one.
fn last_engine_error(log: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(log).ok()?;
    let len = file.metadata().ok()?.len();
    let tail = len.min(8192);
    file.seek(SeekFrom::Start(len - tail)).ok()?;
    let mut buf = vec![0u8; tail as usize];
    file.read_exact(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    text.lines()
        .rev()
        .filter_map(|line| line.split_once("[ENGINE]"))
        .map(|(_, rest)| format!("[ENGINE]{}", rest.trim_end()))
        .find(|line| !line.trim_start_matches("[ENGINE]").trim().is_empty())
}

/// A connection problem extracted from the engine log tail: the most recent
/// `event retrying (...)` / `event config_error (...)` that the tunnel has not
/// since recovered from, plus the raw cause carried by the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionIssue {
    /// Engine reason code (e.g. "fetch_failed", "config_error").
    pub reason: String,
    /// Engine-supplied detail (e.g. "config not found").
    pub detail: String,
}

impl ConnectionIssue {
    /// Human-readable summary. English by design (CLI/TUI); the GUI localizes the
    /// reason/detail pair itself.
    pub fn describe(&self) -> String {
        const HINT: &str = "the doc_url must be an accessible Yandex Docs editor page - disk.yandex.ru links, captcha and login screens have no client-config and can never work";
        match (self.reason.as_str(), self.detail.as_str()) {
            ("fetch_failed", d) if d.contains("bot check") => {
                "Yandex is serving the browser-verification page instead of the doc (a bot-check gate on the way to docs.yandex.ru): the engine cannot run its JS. The doc_url itself is likely fine - retry later or use a different network/IP/Cookies; if it stays, open the link in your browser once to warm it".to_string()
            }
            ("fetch_failed", d) if d.contains("config not found") => {
                format!("cannot fetch the doc from Yandex (loaded page has no client-config): {HINT}")
            }
            ("config_error", d) => format!("the doc config was never obtained ({d}): {HINT}"),
            (r, "") => format!("tunnel not connecting ({r})"),
            (r, d) => format!("tunnel not connecting ({r}: {d})"),
        }
    }
}

/// Scan the tail of the engine log for the most recent failing/retrying tunnel state.
/// Returns `None` when nothing is failing, or when a later `event connected` superseded
/// the last failure (so stale messages from a recovered session don't surface).
pub fn connection_issue(log_file: &Path) -> Option<ConnectionIssue> {
    const TAIL: u64 = 256 * 1024;
    let data = std::fs::read(log_file).ok()?;
    let text = &data[data.len().saturating_sub(TAIL as usize)..];

    let mut last_retry: Option<(usize, ConnectionIssue)> = None;
    let mut last_connected: Option<usize> = None;
    for (idx, line) in text.split(|b| *b == b'\n').enumerate() {
        let line = String::from_utf8_lossy(line);
        if let Some(issue) = parse_engine_issue(&line) {
            last_retry = Some((idx, issue));
        } else if line.contains("event connected (") {
            last_connected = Some(idx);
        }
    }

    let (idx, issue) = last_retry?;
    if last_connected.is_some_and(|c| c > idx) {
        return None;
    }
    Some(issue)
}

/// Live transport state derived from the engine's watchdog output. A provider that closes
/// the session on a timer (Mail.ru caps it at ~60s) makes short dropouts normal, and
/// "TUN is up but the internet is dead" is otherwise indistinguishable from a broken
/// install - so the last watchdog verdict is surfaced verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelState {
    /// Packets are flowing.
    Active,
    /// The transport is down: everything routed into the tunnel is being dropped.
    Dropping,
}

impl TunnelState {
    pub fn as_str(self) -> &'static str {
        match self {
            TunnelState::Active => "active",
            TunnelState::Dropping => "dropping",
        }
    }
}

/// The watchdog prints `tun active: …` while packets move and
/// `tun idle and transport disconnected …` while the transport is down; the last such line
/// wins. `None` when the log has no verdict yet.
pub fn tunnel_state(log_file: &Path) -> Option<TunnelState> {
    const TAIL: u64 = 64 * 1024;
    let data = std::fs::read(log_file).ok()?;
    let text = &data[data.len().saturating_sub(TAIL as usize)..];
    let mut state = None;
    for line in text.split(|b| *b == b'\n') {
        let line = String::from_utf8_lossy(line);
        if line.contains("] tun active:") || line.contains("] socks active:") {
            state = Some(TunnelState::Active);
        } else if line.contains("idle and transport disconnected") {
            state = Some(TunnelState::Dropping);
        }
    }
    state
}

/// Parse an engine event line into a (reason, detail) pair, or `None` for unrelated
/// lines. Understands `event retrying (attempt N|delay|reason|cause)` and the terminal
/// `event config_error (cause)` emitted by the core when a config fetch is a dead end.
fn parse_engine_issue(line: &str) -> Option<ConnectionIssue> {
    const RETRY: &str = "event retrying (attempt ";
    const CONFIG_ERR: &str = "event config_error (";
    if let Some(rest) = line.split_once(RETRY) {
        let inner = rest.1.strip_suffix(')')?;
        let fields: Vec<&str> = inner.splitn(4, '|').collect();
        if fields.len() >= 3 {
            return Some(ConnectionIssue {
                reason: fields[2].to_string(),
                detail: fields.get(3).copied().unwrap_or_default().to_string(),
            });
        }
    }
    if let Some(rest) = line.split_once(CONFIG_ERR) {
        return Some(ConnectionIssue {
            reason: "config_error".to_string(),
            detail: rest.1.strip_suffix(')').unwrap_or(rest.1).to_string(),
        });
    }
    None
}

/// The pidfile path lives next to the engine log's directory.
fn engine_pidfile(cfg: &EngineConfig) -> PathBuf {
    cfg.log_file.with_file_name("engine.pid")
}

/// Current engine pid if the recorded process is still alive and, optionally, the port
/// matches. Returns `None` when nothing is running or the pidfile is stale.
pub fn current(pidfile: &Path) -> Option<i32> {
    let pid = state::read_pid(pidfile)?;
    state::is_alive(pid).then_some(pid)
}

/// Read the engine's recorded port, falling back to the default.
pub fn current_port(pidfile: &Path, default: u16) -> u16 {
    state::pid_meta(pidfile, "socks_port")
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// The engine mode recorded in the pidfile ("socks", "tun" or "exit"); `None` for
/// legacy/absent.
pub fn current_mode(pidfile: &Path) -> Option<String> {
    state::pid_meta(pidfile, "mode")
}

/// Stop whatever `discover_running` reports (pidfile or /proc) and drop the pidfile.
/// Returns the pid it stopped. This is what the UI layer wants: `stop` only knows about the
/// pidfile, which is exactly the record that goes missing for a privileged engine.
pub fn stop_discovered(pidfile: &Path, timeout: Duration) -> Result<Option<i32>> {
    let Some(run) = discover_running(pidfile) else {
        return Ok(None);
    };
    state::terminate(run.pid, timeout)?;
    state::remove_pid(pidfile);
    Ok(Some(run.pid))
}

/// Stop the running engine (SIGTERM, then SIGKILL) and drop its pidfile.
pub fn stop(pidfile: &Path, timeout: Duration) -> Result<()> {
    if let Some(pid) = current(pidfile) {
        state::terminate(pid, timeout)?;
        state::remove_pid(pidfile);
    }
    if let Some(guard) = ENGINE_STDIN.get() {
        *guard.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
    Ok(())
}

/// Whether a `/proc/*/cmdline` payload belongs to one of our own SOCKS-mode engines,
/// i.e. the engine binary started with `--socks5 127.0.0.1:<port>` and no `--tun-fd`.
#[cfg(unix)]
pub fn cmdline_is_our_socks_engine(cmdline: &str, port: u16) -> bool {
    // NUL-separated argv, so the flag and its value must be adjacent tokens.
    let socks = format!("--socks5\0127.0.0.1:{port}\0");
    let socks1 = format!("-socks5\0127.0.0.1:{port}\0");
    cmdline.contains("openflux-engine")
        && !cmdline.contains("--tun-fd")
        && (cmdline.contains(&socks) || cmdline.contains(&socks1))
}

/// A running engine, as seen by the UI layer: the pidfile is authoritative, but a /proc scan
/// stands in for it when it is missing. That combination is not exotic: the TUN engine is
/// spawned by a root helper, so its pidfile lands wherever that helper resolved the state
/// dir, and a lost or unlinkable pidfile otherwise makes the app believe nothing is running
/// (a live tunnel that the Disconnect button then cannot turn off).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Running {
    pub pid: i32,
    /// "socks", "tun" or "exit", from the pidfile when present, else from the flags.
    pub mode: String,
    pub socks_port: Option<u16>,
}

/// Locate a running engine: pidfile first, then a /proc scan. TUN-mode engines win over
/// SOCKS ones when both are somehow alive, because TUN is the state that holds the machine's
/// routing and must be reported (and stopped) first.
#[cfg(unix)]
pub fn discover_running(pidfile: &Path) -> Option<Running> {
    if let Some(pid) = current(pidfile) {
        let mode = current_mode(pidfile).unwrap_or_else(|| "socks".to_string());
        return Some(Running {
            pid,
            mode,
            socks_port: current_port_opt(pidfile),
        });
    }
    scan_running_engines().into_iter().next()
}

/// Windows has no /proc, so the pidfile is the only source.
#[cfg(windows)]
pub fn discover_running(pidfile: &Path) -> Option<Running> {
    current(pidfile).map(|pid| Running {
        pid,
        mode: current_mode(pidfile).unwrap_or_else(|| "socks".to_string()),
        socks_port: current_port_opt(pidfile),
    })
}

fn current_port_opt(pidfile: &Path) -> Option<u16> {
    state::pid_meta(pidfile, "socks_port").and_then(|v| v.parse().ok())
}

/// Every live `openflux-engine` process, TUN first, newest pid last within a group.
#[cfg(unix)]
fn scan_running_engines() -> Vec<Running> {
    let mut socks = Vec::new();
    let mut tun = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() as i32 {
            continue;
        }
        let Ok(raw) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let Some((argv0, cmdline)) = split_argv0(&raw) else {
            continue;
        };
        if !is_our_engine(&argv0) {
            continue;
        }
        let port = extract_flag_port(&cmdline, "socks5");
        let run = if has_flag(&cmdline, "tun-fd") {
            Running {
                pid,
                mode: "tun".to_string(),
                socks_port: None,
            }
        } else if has_flag(&cmdline, "exit") || has_flag(&cmdline, "exit-node") {
            Running {
                pid,
                mode: "exit".to_string(),
                socks_port: None,
            }
        } else {
            Running {
                pid,
                mode: "socks".to_string(),
                socks_port: port,
            }
        };
        if run.mode == "tun" {
            tun.push(run);
        } else {
            socks.push(run);
        }
    }
    tun.extend(socks);
    tun
}

/// argv[0] (NUL separated) and the whole command line as readable text.
#[cfg(unix)]
fn split_argv0(raw: &[u8]) -> Option<(String, String)> {
    let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
    let argv0 = String::from_utf8_lossy(&raw[..end]).into_owned();
    if argv0.is_empty() {
        return None;
    }
    Some((argv0, String::from_utf8_lossy(raw).replace('\0', " ")))
}

/// Whether a process is one of our engines, judged **only** by argv[0]'s basename.
/// Matching the name anywhere in the command line also matches the shell that launched the
/// engine (`sh -c '/usr/local/lib/openflux/openflux-engine ...'`), which is how a "running
/// engine" gets reported as the wrapper's own pid. The basename is stable even when the
/// installer replaced the file under a running process: argv[0] keeps the original name,
/// whereas a `readlink` on /proc/<pid>/exe would come back as `... (deleted)`.
#[cfg(unix)]
fn is_our_engine(argv0: &str) -> bool {
    std::path::Path::new(argv0)
        .file_name()
        .is_some_and(|n| n == "openflux-engine")
}

/// Whether a `name` flag is present. The engine is Go, so its `flag` package accepts both
/// `-name` and `--name`, and the value may be attached with `=`; comparing whole tokens
/// keeps this from matching a flag name that merely appears inside a URL.
#[cfg(unix)]
fn has_flag(cmdline: &str, name: &str) -> bool {
    cmdline
        .split_whitespace()
        .any(|t| flag_name(t) == Some(name))
}

/// The port from a `--socks5 127.0.0.1:1080`-style argument.
#[cfg(unix)]
fn extract_flag_port(cmdline: &str, name: &str) -> Option<u16> {
    let mut tokens = cmdline.split_whitespace();
    while let Some(tok) = tokens.next() {
        if flag_name(tok) == Some(name) {
            let value = match tok.split_once('=') {
                Some((_, inline)) => inline,
                None => tokens.next()?,
            };
            return value.rsplit(':').next()?.parse().ok();
        }
    }
    None
}

/// The flag name a token carries, without leading dashes: `--socks5` and `-socks5` both
/// yield `socks5`, while a positional argument yields `None`.
#[cfg(unix)]
fn flag_name(token: &str) -> Option<&str> {
    let (name, _) = token.split_once('=').unwrap_or((token, ""));
    let name = name.strip_prefix("--").or_else(|| name.strip_prefix('-'))?;
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Live pids of *our* SOCKS-mode engines bound to `port` but no longer tracked by the
/// pidfile (orphans from a crashed/overlapping session). `connect` reclaims these instead
/// of refusing on an inexplicably busy port.
#[cfg(unix)]
pub fn find_orphan_socks_engines(port: u16) -> Vec<i32> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() as i32 {
            continue;
        }
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if cmdline_is_our_socks_engine(&String::from_utf8_lossy(&cmdline), port) {
            found.push(pid);
        }
    }
    found
}

/// Windows has no `/proc`; orphan reclaiming is not available. `connect` simply fails on
/// a busy port, which is the honest behaviour until a win32 equivalent (toolhelp snapshot)
/// is wired up.
#[cfg(windows)]
pub fn find_orphan_socks_engines(_port: u16) -> Vec<i32> {
    Vec::new()
}

/// Drain (up to) the last `max_bytes` of a log file for `openflux logs`.
pub fn tail(path: &Path, max_bytes: usize) -> Result<String> {
    let data = std::fs::read(path).context("read engine log")?;
    let start = data.len().saturating_sub(max_bytes);
    let text = String::from_utf8_lossy(&data[start..]).into_owned();
    Ok(if start == 0 {
        text
    } else {
        // Trim the possibly-split first line.
        text.split_once('\n')
            .map(|(_, rest)| rest.to_string())
            .unwrap_or(text)
    })
}

/// Blocking follow: print new lines as they're appended. A simple poll loop keeps this
/// std-only and dependency-free; the file is re-opened if it is rotated (size shrink).
pub fn follow(path: &Path, poll: Duration) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .open(path)
        .context("open log")?;
    let mut pos = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut buf = Vec::with_capacity(64 * 1024);
    let mut stdout = std::io::stdout().lock();
    loop {
        let meta_len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if meta_len > pos {
            file.seek(SeekFrom::Start(pos))?;
            buf.clear();
            std::io::Read::by_ref(&mut file)
                .take(64 * 1024)
                .read_to_end(&mut buf)?;
            pos += buf.len() as u64;
            stdout.write_all(&buf)?;
            stdout.flush()?;
        } else if meta_len < pos {
            // Log rotated or truncated: restart from the top.
            pos = 0;
        }
        std::thread::sleep(poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socks_addr_has_correct_shape() {
        assert_eq!(socks_addr(1080), "127.0.0.1:1080");
    }

    #[test]
    fn a_live_pidfile_wins_over_the_proc_scan() {
        let tmp = tempfile::tempdir().unwrap();
        let pidfile = tmp.path().join("engine.pid");
        // Our own pid is alive and has no "openflux-engine" in its cmdline, so the pidfile
        // entry must be reported verbatim (mode from the metadata, not guessed).
        std::fs::write(
            &pidfile,
            format!("{}\tmode=tun\tsocks_port=1080\n", std::process::id()),
        )
        .unwrap();
        let run = discover_running(&pidfile).expect("pidfile engine");
        assert_eq!(run.pid, std::process::id() as i32);
        assert_eq!(run.mode, "tun");
        assert_eq!(run.socks_port, Some(1080));
    }

    #[test]
    fn a_missing_pidfile_falls_back_to_scanning_proc() {
        let tmp = tempfile::tempdir().unwrap();
        let pidfile = tmp.path().join("engine.pid");
        // Nothing on record: the result must come from the /proc scan alone. On this machine
        // an openflux-engine is often running, so only assert the invariant that holds either
        // way: whatever is reported must really be alive and really be one of our engines.
        if let Some(run) = discover_running(&pidfile) {
            assert!(
                state::is_alive(run.pid),
                "reported pid {} is not alive",
                run.pid
            );
            let cmdline = std::fs::read(format!("/proc/{}/cmdline", run.pid)).unwrap();
            assert!(
                String::from_utf8_lossy(&cmdline).contains("openflux-engine"),
                "reported pid {} is not an engine: {cmdline:?}",
                run.pid
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn flags_are_parsed_whatever_the_engine_accepted() {
        // The engine is Go: its `flag` package takes `-socks5` and `--socks5` alike, and the
        // value may be attached with `=`.
        assert_eq!(
            extract_flag_port(
                "/usr/local/lib/openflux/openflux-engine --socks5 127.0.0.1:1080 -debug",
                "socks5"
            ),
            Some(1080)
        );
        assert_eq!(
            extract_flag_port(
                "/usr/local/lib/openflux/openflux-engine -socks5 127.0.0.1:18098",
                "socks5"
            ),
            Some(18098)
        );
        assert_eq!(
            extract_flag_port(
                "/usr/local/lib/openflux/openflux-engine --socks5=127.0.0.1:1081",
                "socks5"
            ),
            Some(1081)
        );
        // A flag name inside a URL must not be mistaken for the flag itself.
        assert_eq!(
            extract_flag_port(
                "/x/openflux-engine -url wss://h/socks5/abc -socks5 127.0.0.1:1080",
                "socks5"
            ),
            Some(1080)
        );
        assert_eq!(
            extract_flag_port("/x/openflux-engine --tun-fd 3", "socks5"),
            None
        );
        assert!(has_flag("/x/openflux-engine --tun-fd 3", "tun-fd"));
        assert!(has_flag("/x/openflux-engine -tun-fd=3", "tun-fd"));
        assert!(!has_flag(
            "/x/openflux-engine -socks5 127.0.0.1:1",
            "tun-fd"
        ));
        assert!(!has_flag(
            "/x/openflux-engine -url wss://h/tun-fd",
            "tun-fd"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn only_a_process_invoked_as_the_engine_counts_as_one() {
        // A shell that merely mentions the engine path is not the engine: matching the name
        // anywhere in the command line is what made `status` report the wrapper's own pid.
        assert!(is_our_engine("/usr/local/lib/openflux/openflux-engine"));
        assert!(is_our_engine("./openflux-engine"));
        assert!(!is_our_engine("/usr/bin/bash"));
        assert!(!is_our_engine(
            "bash -c /usr/local/lib/openflux/openflux-engine -socks5 127.0.0.1:1080"
        ));
        assert!(!is_our_engine(""));
    }

    #[test]
    fn tunnel_state_follows_the_last_watchdog_verdict() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("engine.log");

        std::fs::write(
            &log,
            "2026/09/26 12:00:00 [TRAFFIC] tun active: 12 pkts/5s (total 12)\n\
             2026/09/26 12:00:05 [TRAFFIC] tun idle and transport disconnected - traffic is being dropped\n",
        )
        .unwrap();
        assert_eq!(tunnel_state(&log), Some(TunnelState::Dropping));

        std::fs::write(
            &log,
            "2026/09/26 12:00:05 [TRAFFIC] tun idle and transport disconnected - traffic is being dropped\n\
             2026/09/26 12:00:10 [TRAFFIC] tun active: 3 pkts/5s (total 15)\n",
        )
        .unwrap();
        assert_eq!(tunnel_state(&log), Some(TunnelState::Active));

        std::fs::write(
            &log,
            "2026/09/26 12:00:10 [TUNNEL] event connected (attempt )\n",
        )
        .unwrap();
        assert_eq!(tunnel_state(&log), None, "no watchdog verdict yet");
    }

    #[test]
    fn engine_failure_hint_surfaces_the_last_engine_error() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("engine.log");
        assert_eq!(engine_failure_hint(&log), "", "missing log yields no hint");

        std::fs::write(
            &log,
            "2026/09/26 10:00:00 [ENGINE] multistream: 2 streams\n",
        )
        .unwrap();
        assert_eq!(
            engine_failure_hint(&log),
            ": [ENGINE] multistream: 2 streams"
        );

        std::fs::write(
            &log,
            "2026/09/26 10:00:00 [ENGINE] multistream: 2 streams\n\
             2026/09/26 10:00:01 [TUNNEL] event retrying (1|3|fetch_failed)\n\
             2026/09/26 10:00:02 [ENGINE] start transport: boards: no hash in URL \"x\"\n",
        )
        .unwrap();
        assert_eq!(
            engine_failure_hint(&log),
            ": [ENGINE] start transport: boards: no hash in URL \"x\""
        );

        std::fs::write(&log, "2026/09/26 10:00:00 [ENGINE]\n").unwrap();
        assert_eq!(
            engine_failure_hint(&log),
            "",
            "empty marker lines are ignored"
        );
    }

    #[test]
    fn captcha_flag_is_only_passed_when_solving_is_enabled() {
        let args = |config: &TransportConfig| {
            let mut cmd = Command::new("engine");
            config.apply(&mut cmd);
            let args = cmd
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            args.windows(2)
                .filter(|pair| pair[0] == "--captcha-solve-mode")
                .map(|pair| pair[1].clone())
                .collect::<Vec<_>>()
        };

        assert!(args(&TransportConfig::default()).is_empty());
        assert!(args(&TransportConfig {
            captcha_solve_mode: "off".into(),
            ..TransportConfig::default()
        })
        .is_empty());

        let mut profile = Profile::manual("y", "https://disk.yandex.ru/i/a");
        profile.captcha_solve_mode = " headless_browser ".into();
        assert_eq!(
            args(&TransportConfig::from_profile(&profile)),
            ["headless_browser"]
        );
    }

    #[test]
    fn connection_issue_surfaces_unsuperseded_retries() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("engine.log");
        let mut f = std::fs::File::create(&log).unwrap();
        writeln!(f, "01:00:00 [TUNNEL] event connected (attempt 1)").unwrap();
        writeln!(
            f,
            "01:01:00 [TUNNEL] event retrying (attempt 2|3|fetch_failed|config not found)"
        )
        .unwrap();
        f.flush().unwrap();
        let issue = connection_issue(&log);
        assert!(issue.is_some(), "latest line is a retry, must surface it");
        let issue = issue.unwrap();
        assert_eq!(issue.reason, "fetch_failed");
        assert_eq!(issue.detail, "config not found");

        writeln!(
            f,
            "01:02:00 [TUNNEL] event connected (attempt 3) and traffic flows again"
        )
        .unwrap();
        f.flush().unwrap();
        assert_eq!(
            connection_issue(&log),
            None,
            "recovery supersedes the retry"
        );
    }

    #[test]
    fn connection_issue_parses_terminal_config_error() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("engine.log");
        let mut f = std::fs::File::create(&log).unwrap();
        writeln!(f, "01:00:00 [TUNNEL] event config_error (config not found)").unwrap();
        f.flush().unwrap();
        let issue = connection_issue(&log).expect("config_error is an issue");
        assert_eq!(issue.reason, "config_error");
        assert_eq!(issue.detail, "config not found");
    }

    #[test]
    fn describe_distinguishes_bot_check_from_wrong_url() {
        let bot = ConnectionIssue {
            reason: "fetch_failed".to_string(),
            detail: "bot check: Yandex shows the \"верификация\" browser-verification page"
                .to_string(),
        };
        assert!(bot.describe().contains("browser-verification"));

        let wrong = ConnectionIssue {
            reason: "fetch_failed".to_string(),
            detail: "config not found".to_string(),
        };
        assert!(wrong.describe().contains("client-config"));
    }

    #[test]
    fn current_returns_none_for_missing_pidfile() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(current(&tmp.path().join("engine.pid")).is_none());
    }

    #[test]
    fn transport_config_adds_transport_specific_arguments() {
        let profile = Profile {
            name: "multi".into(),
            transport: Transport::YandexMultistream,
            codec: Codec::Batched,
            doc_urls: vec!["https://a".into(), "https://b".into()],
            max_token: "token".into(),
            max_uid: "42".into(),
            ..Profile::default()
        };
        let mut command = Command::new("openflux-engine");
        TransportConfig::from_profile(&profile).apply(&mut command);
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--transport", "yandex_multistream"]));
        assert!(args.windows(2).any(|pair| pair == ["--codec", "batched"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--urls", "https://a,https://b"]));
        assert!(args.windows(2).any(|pair| pair == ["--max-token", "token"]));
        assert!(args.windows(2).any(|pair| pair == ["--max-uid", "42"]));
    }

    #[cfg(unix)]
    #[test]
    fn cmdline_matcher_recognizes_only_our_socks_engines() {
        // cmdline args are NUL-separated in /proc; keep each NUL at a literal boundary so
        // Rust does not parse `\0<digit>` as an octal escape.
        let ours = "/usr/local/lib/openflux/openflux-engine\0".to_owned()
            + "--url\0"
            + "https://d/\0"
            + "--socks5\0"
            + "127.0.0.1:1080\0"
            + "--sock-mark\0"
            + "9543\0"
            + "--mtu\0"
            + "1400";
        assert!(cmdline_is_our_socks_engine(&ours, 1080));
        assert!(
            !cmdline_is_our_socks_engine(&ours, 1081),
            "wrong port must not match"
        );

        let tun = "/usr/local/lib/openflux/openflux-engine\0".to_owned()
            + "--tun-fd\0"
            + "3\0"
            + "--url\0"
            + "https://d/\0"
            + "--sock-mark\0"
            + "0x2547";
        assert!(
            !cmdline_is_our_socks_engine(&tun, 1080),
            "TUN engine must not match"
        );

        let unrelated = "sshd:\0".to_owned() + "/usr/sbin/sshd\0" + "-D\0" + "-p\0" + "1080";
        assert!(!cmdline_is_our_socks_engine(&unrelated, 1080));
    }
}
