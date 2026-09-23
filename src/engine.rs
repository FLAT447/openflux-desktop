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