//! Unix TUN implementation (Linux): TUN device + policy routing (table 100), engine
//! spawned as root holding the fd, then dropped back to the invoking user when possible.
//!
//! Routing layout (table 100):
//!   ip route add 0.0.0.0/1   dev <tun> table 100
//!   ip route add 128.0.0.0/1 dev <tun> table 100
//!   ip rule  add pref 100 lookup 100          # everything -> tun
//!   ip rule  add pref 90  to <lan> lookup main  # LAN stays direct
//!   ip rule  add pref 1   fwmark 0x2547 lookup main  # engine's own sockets stay direct
//! The /1 split overrides the existing default route without deleting it, so teardown is
//! just rule/table removal.

use anyhow::{bail, Context, Result};
use std::ffi::CString;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::state;

pub const FWMARK_RULE_PRIO: u32 = 1;
pub const LAN_RULE_PRIO: u32 = 90;
pub const TUN_TABLE_PRIO: u32 = 100;
pub const TUN_ROUTE_TABLE: u32 = 100;

const TUNSETIFF: libc::c_ulong = 0x4004_54ca;
const IFF_TUN: libc::c_short = 0x0001;
const IFF_NO_PI: libc::c_short = 0x1000;

#[repr(C)]
struct Ifreq {
    name: [libc::c_char; 16],
    flags: libc::c_short,
    _pad: [u8; 22],
}

pub fn require_root() -> Result<()> {
    if !nix::unistd::geteuid().is_root() {
        bail!("TUN mode needs root; re-run as `pkexec openflux tun on`");
    }
    Ok(())
}

fn open_tun(name: &str) -> Result<i32> {
    let cname = CString::new(name).context("tun name has NUL")?;
    if cname.as_bytes().len() >= 16 {
        bail!("tun name '{name}' too long (max 15)");
    }
    let path = CString::new("/dev/net/tun").unwrap();
    // Deliberately no O_CLOEXEC: the child engine inherits this fd by number.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR) };
    if fd < 0 {
        bail!("open /dev/net/tun: {}", std::io::Error::last_os_error());
    }

    let mut ifr: Ifreq = unsafe { std::mem::zeroed() };
    for (i, b) in cname.as_bytes().iter().enumerate() {
        ifr.name[i] = *b as libc::c_char;
    }
    ifr.flags = IFF_TUN | IFF_NO_PI;

    let rc = unsafe { libc::ioctl(fd, TUNSETIFF, &ifr as *const Ifreq) };
    if rc < 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        if err.raw_os_error() == Some(libc::EBUSY) {
            bail!("{}", device_busy_message(name));
        }
        bail!("TUNSETIFF {name}: {err}");
    }
    Ok(fd)
}

/// Whether a network interface with this name currently exists.
fn interface_exists(name: &str) -> bool {
    std::path::Path::new("/sys/class/net").join(name).exists()
}

/// Processes holding an open fd on /dev/net/tun. Readable only as root, which is the context
/// `on()` runs in; used to name the process behind "Device or resource busy".
fn tun_device_holders() -> Vec<i32> {
    let mut holders = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return holders;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(pid) = name.parse::<i32>() else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue;
        };
        let holds = fds.flatten().any(|fd| {
            std::fs::read_link(fd.path())
                .map(|t| t == std::path::Path::new("/dev/net/tun"))
                .unwrap_or(false)
        });
        if holds {
            holders.push(pid);
        }
    }
    holders
}

/// A TUN device is created non-persistent, so a leftover `openflux` interface always means
/// somebody still holds it: a session that was never torn down. Say who, and how to stop it,
/// instead of letting the raw EBUSY surface ("Text file busy"/"Device or resource busy").
fn device_busy_message(name: &str) -> String {
    let holders = tun_device_holders();
    let who = if holders.is_empty() {
        "another process (its /dev/net/tun fd is not visible from here)".to_string()
    } else {
        holders
            .iter()
            .map(
                |p| match std::fs::read_to_string(format!("/proc/{p}/comm")) {
                    Ok(comm) if !comm.trim().is_empty() => format!("{p} ({})", comm.trim()),
                    _ => p.to_string(),
                },
            )
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "the {name} TUN device is already in use by {who}.\n\
         A TUN session from before is still running: stop it before starting a new one.\n  \
         pkexec openflux tun off\n\
         If that reports nothing, the holder survived a crash: kill it (sudo kill <pid>), then\n\
         run `pkexec openflux tun off` once more to drop the routing rules it left behind."
    )
}

fn run(prog: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(prog)
        .args(args)
        .output()
        .with_context(|| format!("run {prog}"))?;
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

/// CIDR of the IPv4 address on the interface that owns the default route, if any.
pub fn lan_cidr() -> Option<String> {
    let route = run("ip", &["-4", "route", "show", "default"]).ok()?;
    let iface = route.lines().find_map(|l| {
        let mut tokens = l.split_whitespace();
        while let Some(t) = tokens.next() {
            if t == "dev" {
                return tokens.next().map(str::to_string);
            }
        }
        None
    })?;
    let addr = run("ip", &["-4", "-o", "addr", "show", "dev", &iface]).ok()?;
    // "-o" prints: "N: iface    inet <cidr> brd ... scope ..."
    addr.lines()
        .next()
        .and_then(|l| l.split_whitespace().find(|t| t.contains('/')))
        .map(str::to_string)
}

fn add_policy_routing(tun_name: &str) -> Result<()> {
    let lan = lan_cidr();
    // idempotent (ignore "File exists").
    run_ok(
        "ip",
        &[
            "route",
            "add",
            "0.0.0.0/1",
            "dev",
            tun_name,
            "table",
            &TUN_ROUTE_TABLE.to_string(),
        ],
    );
    run_ok(
        "ip",
        &[
            "route",
            "add",
            "128.0.0.0/1",
            "dev",
            tun_name,
            "table",
            &TUN_ROUTE_TABLE.to_string(),
        ],
    );
    run_ok(
        "ip",
        &[
            "rule",
            "add",
            "pref",
            &TUN_TABLE_PRIO.to_string(),
            "lookup",
            &TUN_ROUTE_TABLE.to_string(),
        ],
    );
    run_ok(
        "ip",
        &[
            "rule",
            "add",
            "pref",
            &FWMARK_RULE_PRIO.to_string(),
            "fwmark",
            &format!("{:#x}", crate::FWMARK),
            "lookup",
            "main",
        ],
    );
    if let Some(lan) = lan {
        run_ok(
            "ip",
            &[
                "rule",
                "add",
                "pref",
                &LAN_RULE_PRIO.to_string(),
                "to",
                &lan,
                "lookup",
                "main",
            ],
        );
    }
    Ok(())
}

fn del_policy_routing(tun_name: &str) {
    run_ok(
        "ip",
        &[
            "rule",
            "del",
            "pref",
            &FWMARK_RULE_PRIO.to_string(),
            "fwmark",
            &format!("{:#x}", crate::FWMARK),
            "lookup",
            "main",
        ],
    );
    run_ok(
        "ip",
        &[
            "rule",
            "del",
            "pref",
            &TUN_TABLE_PRIO.to_string(),
            "lookup",
            &TUN_ROUTE_TABLE.to_string(),
        ],
    );
    if let Some(lan) = lan_cidr() {
        run_ok(
            "ip",
            &[
                "rule",
                "del",
                "pref",
                &LAN_RULE_PRIO.to_string(),
                "to",
                &lan,
                "lookup",
                "main",
            ],
        );
    }
    run_ok(
        "ip",
        &["route", "flush", "table", &TUN_ROUTE_TABLE.to_string()],
    );
    let _ = tun_name;
}

/// The uid/gid of the user that invoked pkexec/sudo, so the engine can run unprivileged.
fn invoking_ids() -> Option<(u32, u32)> {
    let uid = std::env::var("SUDO_UID")
        .ok()
        .or_else(|| std::env::var("PKEXEC_UID").ok())
        .and_then(|v| v.parse::<u32>().ok())?;
    let user = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid)).ok()??;
    Some((uid, user.gid.as_raw()))
}

/// Bring TUN mode up. Must run as root. Returns the engine pid.
pub fn on(cfg: &super::TunConfig) -> Result<i32> {
    require_root()?;

    if let Some(pid) = state::read_pid(&cfg.engine_pid) {
        if state::is_alive(pid)
            && state::pid_meta(&cfg.engine_pid, "mode").as_deref() == Some("tun")
        {
            return Ok(pid); // already up
        }
    }

    // A previous session's interface outlives it only while something still holds the
    // device. Catching that here turns an opaque EBUSY from the ioctl into an explanation
    // plus the command that clears it.
    if interface_exists(&cfg.tun_name) {
        bail!("{}", device_busy_message(&cfg.tun_name));
    }

    let fd = open_tun(&cfg.tun_name)?;

    // Configure the interface while we still hold the fd (the interface lives as long as
    // some process holds it).
    let setup = (|| -> Result<()> {
        run_ok("ip", &["addr", "add", &cfg.tun_addr, "dev", &cfg.tun_name]);
        run(
            "ip",
            &[
                "link",
                "set",
                &cfg.tun_name,
                "mtu",
                &cfg.mtu.to_string(),
                "up",
            ],
        )?;
        add_policy_routing(&cfg.tun_name)
    })();
    if let Err(e) = setup {
        unsafe { libc::close(fd) };
        return Err(e);
    }

    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&cfg.engine_log)
        .with_context(|| format!("open engine log {}", cfg.engine_log.display()))?;

    let build = |drop: Option<(u32, u32)>| -> Result<Command> {
        let mut cmd = Command::new(&cfg.engine_bin);
        cmd.arg("--tun-fd").arg(fd.to_string());
        cfg.transport.apply(&mut cmd);
        cmd.arg("--dns")
            .arg(&cfg.dns)
            .arg("--sock-mark")
            .arg(format!("{:#x}", crate::FWMARK))
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
        // Drop to the invoking user when possible: the fd is already open and the binary
        // carries CAP_NET_ADMIN, so the engine needs no root of its own.
        if let Some((uid, gid)) = drop {
            cmd.uid(uid).gid(gid);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().context("clone engine log")?))
            .stderr(Stdio::from(log.try_clone().context("clone engine log")?));
        Ok(cmd)
    };

    // Dropping privileges can fail with EAGAIN when the invoking user is already at its
    // RLIMIT_NPROC (each thread counts). Fall back to staying root, which still works
    // because we only need the already-open fd.
    let spawned = (|| -> Result<std::process::Child> {
        match build(invoking_ids()).and_then(|mut c| {
            c.spawn()
                .with_context(|| format!("spawn engine {}", cfg.engine_bin.display()))
        }) {
            Ok(child) => Ok(child),
            Err(e)
                if e.root_cause()
                    .downcast_ref::<std::io::Error>()
                    .and_then(std::io::Error::raw_os_error)
                    == Some(libc::EAGAIN) =>
            {
                eprintln!(
                    "warning: cannot drop privileges for the engine ({e}); running it as root"
                );
                build(None)?
                    .spawn()
                    .with_context(|| format!("spawn engine {}", cfg.engine_bin.display()))
            }
            Err(e) => Err(e),
        }
    })();
    let child = match spawned {
        Ok(child) => child,
        Err(e) => {
            del_policy_routing(&cfg.tun_name);
            unsafe { libc::close(fd) };
            return Err(e);
        }
    };
    let pid = child.id() as i32;
    if let Err(e) = state::write_pid(&cfg.engine_pid, pid, Some("mode=tun".into())) {
        // Without a pidfile nothing can ever stop this engine again, and the device stays
        // held, so the next `tun on` fails with EBUSY. Never leave that behind.
        let _ = state::terminate(pid, Duration::from_secs(5));
        del_policy_routing(&cfg.tun_name);
        unsafe { libc::close(fd) };
        return Err(e).context("record the TUN engine pid");
    }

    // Give the gateway a moment; if it dies immediately, surface it.
    std::thread::sleep(Duration::from_millis(600));
    if !state::is_alive(pid) {
        del_policy_routing(&cfg.tun_name);
        state::remove_pid(&cfg.engine_pid);
        unsafe { libc::close(fd) };
        bail!(
            "engine exited during TUN startup; see {}",
            cfg.engine_log.display()
        );
    }

    // The parent no longer needs its fd copy; the engine owns it now.
    unsafe { libc::close(fd) };
    Ok(pid)
}

/// Tear TUN mode down: stop the engine (releasing the TUN device) and remove routing.
/// Leaves a SOCKS-mode engine untouched. When the engine runs as root and this helper is
/// unprivileged (TUI d / `disconnect`), signalling it fails with EPERM - the routing is
/// still removed so traffic is restored, and the pidfile is kept so the privileged teardown
/// (`pkexec openflux tun off`) can still find the engine.
pub fn off(cfg: &super::TunConfig) -> Result<()> {
    if let Some(pid) = state::read_pid(&cfg.engine_pid) {
        if state::is_alive(pid)
            && state::pid_meta(&cfg.engine_pid, "mode").as_deref() == Some("tun")
        {
            match state::terminate(pid, Duration::from_secs(5)) {
                Ok(()) => state::remove_pid(&cfg.engine_pid),
                Err(e) => {
                    if state::is_alive(pid) {
                        eprintln!("warning: could not stop TUN engine pid {pid} ({e:#}); it runs as root - stop it with `pkexec openflux tun off`");
                    }
                }
            }
        }
    }
    del_policy_routing(&cfg.tun_name);
    Ok(())
}

/// Whether TUN mode is currently up: either the pidfile records a live TUN engine, or -
/// when that record is missing - a live engine holds the device (`--tun-fd`). The second
/// half matters because the TUN engine is spawned by a root helper, so its pidfile is not
/// always where the unprivileged UI looks, and "not up" then means the UI cannot switch the
/// tunnel off.
pub fn is_up(cfg: &super::TunConfig) -> bool {
    if matches!(state::read_pid(&cfg.engine_pid), Some(pid) if state::is_alive(pid)
        && state::pid_meta(&cfg.engine_pid, "mode").as_deref() == Some("tun"))
    {
        return true;
    }
    if !interface_exists(&cfg.tun_name) {
        return false;
    }
    crate::engine::discover_running(&cfg.engine_pid).is_some_and(|r| r.mode == "tun")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_that_is_not_an_interface_is_reported_absent() {
        assert!(!interface_exists("openflux-nonexistent-iface"));
    }

    #[test]
    fn the_tun_holder_scan_is_safe_to_call_unprivileged() {
        // We do not hold /dev/net/tun here, so the list must be empty rather than an error;
        // the point is that the diagnostic path never panics or fails the caller.
        let holders = tun_device_holders();
        assert!(!holders.contains(&(std::process::id() as i32)));
    }

    #[test]
    fn the_busy_message_names_the_way_out() {
        let msg = device_busy_message("openflux");
        assert!(msg.contains("already in use"), "{msg}");
        assert!(msg.contains("pkexec openflux tun off"), "{msg}");
        println!("{msg}");
    }

    #[test]
    fn lan_cidr_parses_when_default_route_exists() {
        // On a machine without a default route this returns None; only assert shape.
        if let Some(cidr) = lan_cidr() {
            assert!(cidr.contains('/'), "expected CIDR, got {cidr}");
        }
    }
}
