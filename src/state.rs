//! Process state: pid files + liveness/termination helpers shared by the engine and the
//! tun2proxy relay management. Liveness/termination are platform-specific: signals on
//! Unix, win32 process handles on Windows.

use anyhow::{Context, Result};
use std::path::Path;
use std::time::Duration;

pub fn write_pid(path: &Path, pid: i32, metadata: Option<String>) -> Result<()> {
    let line = match metadata {
        Some(meta) => format!("{pid}\t{meta}\n"),
        None => format!("{pid}\n"),
    };
    std::fs::write(path, line).context("write pidfile")
}

/// A pidfile stores the pid in the first whitespace-delimited token; any trailing
/// metadata (e.g. `socks_port=1080`) is ignored here.
pub fn read_pid(path: &Path) -> Option<i32> {
    let content = std::fs::read_to_string(path).ok()?;
    content.split_whitespace().next()?.parse().ok()
}

pub fn remove_pid(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Whether a process with this pid currently exists (existence check, no signal sent).
/// EPERM counts as alive: a process that exists but belongs to another user (e.g. a TUN
/// engine running as root, queried by the unprivileged CLI) must not look like it is gone.
#[cfg(unix)]
pub fn is_alive(pid: i32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    if pid <= 0 {
        return false;
    }
    match kill(Pid::from_raw(pid), None) {
        Ok(()) => true,
        Err(nix::errno::Errno::EPERM) => true,
        Err(_) => false,
    }
}

/// Send SIGTERM, wait up to `timeout` for the process to disappear, then SIGKILL. Errors
/// (e.g. EPERM when the target runs as a different effective user) are surfaced so callers
/// can degrade gracefully instead of silently believing the process is gone.
#[cfg(unix)]
pub fn terminate(pid: i32, timeout: Duration) -> Result<()> {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    use std::time::Instant;
    let pid = Pid::from_raw(pid);
    kill(pid, None).with_context(|| format!("liveness probe for pid {pid}"))?;
    match kill(pid, Signal::SIGTERM) {
        Ok(()) => {}
        Err(nix::errno::Errno::ESRCH) => return Ok(()), // already gone
        Err(e) => return Err(e).with_context(|| format!("SIGTERM to pid {pid}")),
    }
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if kill(pid, None).is_err() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = kill(pid, Signal::SIGKILL);
    Ok(())
}

#[cfg(windows)]
mod imp {
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, HANDLE};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };

    fn open(pid: i32, access: u32) -> Option<HANDLE> {
        if pid <= 0 {
            return None;
        }
        let h = unsafe { OpenProcess(access, 0, pid as u32) };
        (!h.is_null()).then_some(h)
    }

    pub fn is_alive(pid: i32) -> bool {
        match open(pid, PROCESS_QUERY_LIMITED_INFORMATION) {
            Some(h) => {
                let _ = unsafe { CloseHandle(h) };
                true
            }
            None => false,
        }
    }

    pub fn terminate(pid: i32, timeout: Duration) -> std::io::Result<()> {
        let h = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid as u32) };
        if h.is_null() {
            let err = std::io::Error::last_os_error();
            return if err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
                Err(err) // exists but owned by a higher-integrity context, like Unix EPERM
            } else {
                Ok(()) // already gone
            };
        }
        let ok = unsafe { TerminateProcess(h, 1) };
        let _ = unsafe { CloseHandle(h) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !is_alive(pid) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }
}

/// Whether a process with this pid currently exists (Windows).
#[cfg(windows)]
pub fn is_alive(pid: i32) -> bool {
    imp::is_alive(pid)
}

/// Terminate a process on Windows (no SIGTERM exists; TerminateProcess is the portable
/// close). Surfaces access-denied like Unix EPERM.
#[cfg(windows)]
pub fn terminate(pid: i32, timeout: Duration) -> Result<()> {
    imp::terminate(pid, timeout).context("terminate process")
}

/// Read the meta field of a pidfile line (`key=value` after the pid).
pub fn pid_meta(path: &Path, key: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    for token in content.split_whitespace().skip(1) {
        if let Some((k, v)) = token.split_once('=') {
            if k == key {
                return Some(v.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pidfile_roundtrip_with_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("engine.pid");
        write_pid(&f, 4321, Some("socks_port=1080".into())).unwrap();
        assert_eq!(read_pid(&f), Some(4321));
        assert_eq!(pid_meta(&f, "socks_port"), Some("1080".into()));
        remove_pid(&f);
        assert_eq!(read_pid(&f), None);
    }
}