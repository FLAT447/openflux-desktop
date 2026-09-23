//! Windows TUN implementation: Wintun 0.14 adapter + netsh routing (the same
//! 0.0.0.0/1 + 128.0.0.0/1 trick as Unix, added via `netsh` instead of `ip`).
//!
//! A Wintun session handle behaves like a regular file handle - wintun's own api.c
//! implements packet I/O with ReadFile/WriteFile on it - so the Rust side (launcher,
//! elevated) only creates the adapter+session and passes the session handle to the Go
//! engine as `--tun-read`/`--tun-write`. The engine owns the handle, so session and
//! interface lifetime follow the engine process exactly as the TUN fd does on Unix.

use anyhow::{bail, Context, Result};
use std::ffi::{c_void, CString};
use std::os::windows::ffi::OsStrExt;
use std::process::{Command, Stdio};
use std::time::Duration;

use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE, SetHandleInformation};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::state;

const WINTUN_ADAPTER_NAME: &str = "OpenFlux";
const WINTUN_INSTANCE_NAME: &str = "OpenFlux";
const WINTUN_SESSION_CAPACITY: u32 = 0x400000; // 4 MiB ring
const HANDLE_FLAG_INHERIT: u32 = 0x1;

#[repr(C)]
#[derive(Clone, Copy)]
struct WintunGuid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

type WintunCreateAdapter =
    unsafe extern "system" fn(*const u16, *const u16, *const WintunGuid) -> *mut c_void;
type WintunOpenAdapter = unsafe extern "system" fn(*const u16) -> *mut c_void;
type WintunCloseAdapter = unsafe extern "system" fn(*mut c_void);
type WintunStartSession = unsafe extern "system" fn(*mut c_void, u32) -> *mut c_void;
type WintunEndSession = unsafe extern "system" fn(*mut c_void);

/// Handle-based subset of the Wintun API needed to bring a session up.
struct Wintun {
    _lib: HMODULE,
    create_adapter: WintunCreateAdapter,
    open_adapter: WintunOpenAdapter,
    close_adapter: WintunCloseAdapter,
    start_session: WintunStartSession,
    end_session: WintunEndSession,
}

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

fn get_proc_raw(lib: HMODULE, name: &str) -> Result<usize> {
    let cname = CString::new(name).context("symbol name has NUL")?;
    // SAFETY: static NUL-terminated name, live module handle. PCSTR is *const u8.
    let addr = unsafe { GetProcAddress(lib, cname.as_ptr() as *const u8) }
        .ok_or_else(|| anyhow::anyhow!("wintun.dll is missing the '{name}' export"))?;
    // FARPROC (Option<extern fn() -> isize>) is pointer-sized; the concrete fn type is too.
    Ok(unsafe { std::mem::transmute_copy(&addr) })
}

fn get_proc<F>(lib: HMODULE, name: &str) -> Result<F> {
    let raw = get_proc_raw(lib, name)?;
    Ok(unsafe { std::mem::transmute_copy::<usize, F>(&raw) })
}

impl Wintun {
    fn load() -> Result<Self> {
        let wname = wide("wintun.dll");
        // SAFETY: loads from the exe dir / search path; static wide name.
        let lib = unsafe { LoadLibraryW(wname.as_ptr()) };
        if lib.is_null() {
            bail!(
                "wintun.dll was not found; place WireGuard's Wintun 0.14 DLL next to the executable"
            );
        }
        Ok(Wintun {
            _lib: lib,
            create_adapter: get_proc(lib, "WintunCreateAdapter")?,
            open_adapter: get_proc(lib, "WintunOpenAdapter")?,
            close_adapter: get_proc(lib, "WintunCloseAdapter")?,
            start_session: get_proc(lib, "WintunStartSession")?,
            end_session: get_proc(lib, "WintunEndSession")?,
        })
    }

    fn lib(&self) -> HMODULE {
        self._lib
    }
}

fn last_error() -> String {
    std::io::Error::last_os_error().to_string()
}

/// True when the current process runs in an elevated (Administrator) context.
pub fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::HANDLE;
    // SAFETY: standard token plumbing; any failure reports "not elevated".
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elev = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elev as *mut _ as *mut c_void,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        ) != 0
            && elev.TokenIsElevated != 0;
        windows_sys::Win32::Foundation::CloseHandle(token);
        ok
    }
}

pub fn require_root() -> Result<()> {
    if is_elevated() {
        Ok(())
    } else {
        bail!(
            "TUN mode needs an elevated (Administrator) process on Windows; use the GUI TUN toggle (requests UAC) or run from an elevated shell"
        )
    }
}

fn run(prog: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(prog).args(args).output().with_context(|| format!("run {prog}"))?;
    if !out.status.success() {
        bail!(
            "`{prog} {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn run_ok(prog: &str, args: &[&str]) {
    let _ = Command::new(prog).args(args).output();
}

/// "10.0.0.1/24" -> ("10.0.0.1", "255.255.255.0"). Fallible only on bad input.
fn split_tun_addr(value: &str) -> Result<(String, String)> {
    let (addr, prefix) = value
        .split_once('/')
        .with_context(|| format!("tun_addr '{value}' must be ip/prefix"))?;
    let bits: u8 = prefix.parse().context("bad prefix length")?;
    if bits > 32 {
        bail!("bad prefix length {bits}");
    }
    let mask = if bits == 0 {
        0u32
    } else {
        u32::MAX.checked_shl(32 - bits as u32).unwrap_or(u32::MAX)
    };
    let mask = format!(
        "{}.{}.{}.{}",
        (mask >> 24) & 0xff,
        (mask >> 16) & 0xff,
        (mask >> 8) & 0xff,
        mask & 0xff
    );
    Ok((addr.to_string(), mask))
}

fn add_policy_routing(iface: &str, addr: &str, mtu: u32) -> Result<()> {
    let (ip, mask) = split_tun_addr(addr)?;
    run(
        "netsh",
        &[
            "interface", "ipv4", "set", "address", "interface", iface, "source=static",
            &format!("address={ip}"), &format!("mask={mask}"), "gateway=none",
        ],
    )
    .context("set adapter address")?;
    // MTU must be set after the address exists and be in netsh's allowed range.
    run(
        "netsh",
        &[
            "interface", "ipv4", "set", "subinterface", iface, "mtu",
            &mtu.clamp(576, 1400).to_string(), "store=persistent",
        ],
    )
    .context("set adapter MTU")?;
    for route in ["0.0.0.0/1", "128.0.0.0/1"] {
        run(
            "netsh",
            &[
                "interface", "ipv4", "add", "route", route, iface, &ip,
            ],
        )
        .with_context(|| format!("add route {route}"))?;
    }
    Ok(())
}

fn del_policy_routing(iface: &str) {
    // Best effort: teardown must not fail because the routes are already half-gone.
    for route in ["0.0.0.0/1", "128.0.0.0/1"] {
        run_ok("netsh", &["interface", "ipv4", "delete", "route", route, iface]);
    }
}

/// Bring TUN mode up. Returns the engine pid.
pub fn on(cfg: &super::TunConfig) -> Result<i32> {
    require_root()?;

    if let Some(pid) = state::read_pid(&cfg.engine_pid) {
        if state::is_alive(pid) && state::pid_meta(&cfg.engine_pid, "mode").as_deref() == Some("tun") {
            return Ok(pid); // already up
        }
    }

    let wintun = Wintun::load()?;
    let wname = wide(WINTUN_ADAPTER_NAME);
    let iname = wide(WINTUN_INSTANCE_NAME);

    // SAFETY: null GUID = default; static wide names.
    let mut adapter =
        unsafe { (wintun.create_adapter)(wname.as_ptr(), iname.as_ptr(), std::ptr::null()) };
    if adapter.is_null() {
        adapter = unsafe { (wintun.open_adapter)(wname.as_ptr()) };
        if adapter.is_null() {
            let err = last_error();
            // SAFETY: nothing opened.
            unsafe { FreeLibrary(wintun.lib()) };
            bail!(
                "Wintun: cannot create/open adapter '{WINTUN_ADAPTER_NAME}' ({err}); run as Administrator"
            );
        }
    }
    // Rollback scope: everything after here must be undone on failure.
    let session = unsafe { (wintun.start_session)(adapter, WINTUN_SESSION_CAPACITY) };
    if session.is_null() {
        let err = last_error();
        // SAFETY: failed session start only needs adapter close.
        unsafe {
            (wintun.close_adapter)(adapter);
            FreeLibrary(wintun.lib());
        }
        bail!("WintunStartSession failed: {err}");
    }
    // Driver-created handles are not inheritable by default; mark it so the engine
    // process receives the same handle value. Without this the child would inherit a
    // stale number and its socket traffic would go nowhere.
    // SAFETY: session is a live handle we own.
    if unsafe {
        SetHandleInformation(session, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT)
    } == 0 {
        let err = last_error();
        // SAFETY: undo the session we did start.
        unsafe {
            (wintun.end_session)(session);
            (wintun.close_adapter)(adapter);
            FreeLibrary(wintun.lib());
        }
        bail!("cannot mark the Wintun session inheritable ({err})");
    }

    let setup = add_policy_routing(WINTUN_INSTANCE_NAME, &cfg.tun_addr, cfg.mtu);
    if let Err(e) = setup {
        // SAFETY: partial teardown - end the session we did start, close adapter.
        unsafe {
            (wintun.end_session)(session);
            (wintun.close_adapter)(adapter);
            FreeLibrary(wintun.lib());
        }
        del_policy_routing(WINTUN_INSTANCE_NAME);
        return Err(e);
    }

    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&cfg.engine_log)
        .with_context(|| format!("open engine log {}", cfg.engine_log.display()))?;

    let mut cmd = Command::new(&cfg.engine_bin);
    // Same session handle for both directions: it is a file handle the engine reads and
    // writes packets through (Wintun implements packet I/O with ReadFile/WriteFile).
    let handle = (session as windows_sys::Win32::Foundation::HANDLE as i64).to_string();
    cmd.arg("--tun-read")
        .arg(&handle)
        .arg("--tun-write")
        .arg(&handle)
        .arg("--url")
        .arg(&cfg.url)
        .arg("--dns")
        .arg(&cfg.dns)
        .arg("--mtu")
        .arg(cfg.mtu.to_string());
    if cfg.streams != 1 {
        cmd.arg("--streams").arg(cfg.streams.to_string());
    }
    if !cfg.split_mode.is_empty() {
        cmd.arg("--split-mode").arg(&cfg.split_mode);
        let sites = cfg
            .split_sites
            .iter()
            .map(|s| s.trim().trim_start_matches('.').to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(",");
        if !sites.is_empty() {
            cmd.arg("--split-sites").arg(sites);
        }
    }
    if let Some(token) = &cfg.token {
        if !token.is_empty() {
            cmd.arg("--token").arg(token);
        }
    }
    if cfg.debug {
        cmd.arg("--debug");
    }
    cmd.stdout(Stdio::from(log.try_clone().context("clone engine log")?))
        .stderr(Stdio::from(log));

    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            // SAFETY: full teardown of the freshly opened session.
            unsafe {
                (wintun.end_session)(session);
                (wintun.close_adapter)(adapter);
                FreeLibrary(wintun.lib());
            }
            del_policy_routing(WINTUN_INSTANCE_NAME);
            return Err(e).with_context(|| format!("spawn engine {}", cfg.engine_bin.display()));
        }
    };
    let pid = child.id() as i32;

    // Give the gateway a moment; surface an immediate crash like Unix does. The engine
    // holds the session handle now, so our close+FreeLibrary do not disturb packet flow.
    std::thread::sleep(Duration::from_millis(600));
    let alive = state::is_alive(pid);
    if !alive {
        unsafe {
            (wintun.end_session)(session);
            (wintun.close_adapter)(adapter);
            FreeLibrary(wintun.lib());
        }
        del_policy_routing(WINTUN_INSTANCE_NAME);
        state::remove_pid(&cfg.engine_pid);
        bail!("engine exited during TUN startup; see {}", cfg.engine_log.display());
    }
    state::write_pid(&cfg.engine_pid, pid, Some("mode=tun".into()))?;

    // The launcher's job is done; only its handle copies close. The engine keeps the
    // session alive for its whole lifetime, exactly like the inherited TUN fd on Unix.
    unsafe {
        (wintun.end_session)(session);
        (wintun.close_adapter)(adapter);
        FreeLibrary(wintun.lib());
    }
    Ok(pid)
}

/// Tear TUN mode down: stop the engine (its exit closes the session handle, which the
/// Wintun driver observes as the session ending) and drop the /1 routes.
pub fn off(cfg: &super::TunConfig) -> Result<()> {
    if let Some(pid) = state::read_pid(&cfg.engine_pid) {
        if state::is_alive(pid) && state::pid_meta(&cfg.engine_pid, "mode").as_deref() == Some("tun") {
            match state::terminate(pid, Duration::from_secs(5)) {
                Ok(()) => state::remove_pid(&cfg.engine_pid),
                Err(e) => {
                    if state::is_alive(pid) {
                        eprintln!("warning: could not stop TUN engine pid {pid} ({e:#}); stop it from an elevated process");
                    }
                }
            }
        }
    }
    del_policy_routing(WINTUN_INSTANCE_NAME);
    Ok(())
}

/// Whether TUN mode is currently up (engine pidfile records mode=tun and is alive).
pub fn is_up(cfg: &super::TunConfig) -> bool {
    matches!(state::read_pid(&cfg.engine_pid), Some(pid) if state::is_alive(pid)
        && state::pid_meta(&cfg.engine_pid, "mode").as_deref() == Some("tun"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_tun_addr_parses() {
        assert_eq!(
            split_tun_addr("10.0.0.1/24").unwrap(),
            ("10.0.0.1".into(), "255.255.255.0".into())
        );
        assert_eq!(
            split_tun_addr("172.16.0.2/16").unwrap(),
            ("172.16.0.2".into(), "255.255.0.0".into())
        );
        assert!(split_tun_addr("10.0.0.1").is_err());
        assert!(split_tun_addr("10.0.0.1/33").is_err());
    }
}