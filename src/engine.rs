//! Lifecycle of the Go engine child process (openflux-engine). The Rust CLI spawns it as a
//! detached daemon, waits for its SOCKS5 listener to come up, and records its pid so later
//! invocations can stop it or reuse the same port for the TUN/proxy layers.

use anyhow::{bail, Context, Result};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::state;

pub const SOCK_MARK: i32 = crate::FWMARK as i32;

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub bin: PathBuf,
    pub url: String,
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
    TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(500))
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
    cmd.args(["--url", &cfg.url, "--socks5", &socks_addr(cfg.socks_port)]);
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
    cmd.stdout(Stdio::from(log.try_clone().context("clone engine log")?))
        .stderr(Stdio::from(log));

    let mut child = cmd.spawn().with_context(|| format!("spawn engine {}", cfg.bin.display()))?;

    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().context("wait engine")? {
            bail!("engine exited early with {status}");
        }
        if is_port_open(cfg.socks_port) {
            let pid = child.id() as i32;
            let meta = format!("socks_port={} mode=socks", cfg.socks_port);
            state::write_pid(&engine_pidfile(cfg), pid, Some(meta))?;
            return Ok(pid);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!("engine did not become ready within {timeout:?}");
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
            Some(m) => bail!("another engine mode is active ({m}); stop it before starting an exit node"),
            None => bail!("an untracked engine is running (pid {pid}); stop it first"),
        }
    }

    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&cfg.log_file)
        .with_context(|| format!("open engine log {}", cfg.log_file.display()))?;

    let mut cmd = Command::new(&cfg.bin);
    cmd.args(["--url", &cfg.url, "--exit"]);
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
    cmd.stdout(Stdio::from(log.try_clone().context("clone engine log")?))
        .stderr(Stdio::from(log));

    let mut child = cmd.spawn().with_context(|| format!("spawn engine {}", cfg.bin.display()))?;

    let mut pos = std::fs::metadata(&cfg.log_file).map(|m| m.len()).unwrap_or(0);
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().context("wait engine")? {
            bail!("engine exited early with {status}");
        }
        if log_tail_contains(&cfg.log_file, &mut pos, "event connected").unwrap_or(false) {
            let pid = child.id() as i32;
            state::write_pid(&engine_pidfile(cfg), pid, Some("mode=exit".to_string()))?;
            return Ok(pid);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!("engine (exit node) did not become ready within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
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

/// Stop the running engine (SIGTERM, then SIGKILL) and drop its pidfile.
pub fn stop(pidfile: &Path, timeout: Duration) -> Result<()> {
    if let Some(pid) = current(pidfile) {
        state::terminate(pid, timeout)?;
        state::remove_pid(pidfile);
    }
    Ok(())
}

/// Whether a `/proc/*/cmdline` payload belongs to one of our own SOCKS-mode engines,
/// i.e. the engine binary started with `--socks5 127.0.0.1:<port>` and no `--tun-fd`.
#[cfg(unix)]
pub fn cmdline_is_our_socks_engine(cmdline: &str, port: u16) -> bool {
    let needle = format!("--socks5\0127.0.0.1:{port}");
    cmdline.contains("openflux-engine") && !cmdline.contains("--tun-fd") && cmdline.contains(&needle)
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
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<i32>().ok()) else {
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
    let mut file = std::fs::OpenOptions::new().read(true).open(path).context("open log")?;
    let mut pos = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut buf = Vec::with_capacity(64 * 1024);
    let mut stdout = std::io::stdout().lock();
    loop {
        let meta_len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if meta_len > pos {
            file.seek(SeekFrom::Start(pos))?;
            buf.clear();
            std::io::Read::by_ref(&mut file).take(64 * 1024).read_to_end(&mut buf)?;
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
    fn connection_issue_surfaces_unsuperseded_retries() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("engine.log");
        let mut f = std::fs::File::create(&log).unwrap();
        writeln!(f, "01:00:00 [TUNNEL] event connected (attempt 1)").unwrap();
        writeln!(f, "01:01:00 [TUNNEL] event retrying (attempt 2|3|fetch_failed|config not found)").unwrap();
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
        assert_eq!(connection_issue(&log), None, "recovery supersedes the retry");
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

    #[cfg(unix)]
    #[test]
    fn cmdline_matcher_recognizes_only_our_socks_engines() {
        // cmdline args are NUL-separated in /proc; keep each NUL at a literal boundary so
        // Rust does not parse `\0<digit>` as an octal escape.
        let ours = "/usr/local/lib/openflux/openflux-engine\0"
            .to_owned()
            + "--url\0"
            + "https://d/\0"
            + "--socks5\0"
            + "127.0.0.1:1080\0"
            + "--sock-mark\0"
            + "9543\0"
            + "--mtu\0"
            + "1400";
        assert!(cmdline_is_our_socks_engine(&ours, 1080));
        assert!(!cmdline_is_our_socks_engine(&ours, 1081), "wrong port must not match");

        let tun = "/usr/local/lib/openflux/openflux-engine\0".to_owned()
            + "--tun-fd\0"
            + "3\0"
            + "--url\0"
            + "https://d/\0"
            + "--sock-mark\0"
            + "0x2547";
        assert!(!cmdline_is_our_socks_engine(&tun, 1080), "TUN engine must not match");

        let unrelated = "sshd:\0".to_owned() + "/usr/sbin/sshd\0" + "-D\0" + "-p\0" + "1080";
        assert!(!cmdline_is_our_socks_engine(&unrelated, 1080));
    }
}