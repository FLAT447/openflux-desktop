//! OpenFlux desktop client library: config, controlplane resolve, engine/tun/proxy
//! lifecycle. The Go binary in `engine/` runs the actual tunnel; this crate is the CLI.

pub mod actions;
pub mod config;
pub mod engine;
pub mod import;
pub mod paths;
pub mod proxy;
pub mod resolve;
pub mod state;
pub mod tun;

pub const FWMARK: u32 = 0x2547;
pub const TUN_NAME: &str = "openflux";

/// Rust's runtime ignores SIGPIPE, so a closed stdout (say `openflux logs | head`) makes every
/// later `println!` panic with "failed printing to stdout: Broken pipe". Restoring the default
/// disposition kills the process with the signal instead, which is what every other Unix tool
/// does and what the exit status of a pipeline is expected to mean. Windows has no SIGPIPE, and
/// there a closed handle already surfaces as an ordinary write error.
#[cfg(unix)]
pub fn restore_default_sigpipe() {
    use nix::sys::signal::{signal, SigHandler, Signal};
    // SAFETY: `signal` only stores the handler in the kernel table. SIG_DFL takes no pointer and
    // is async-signal-safe, so there is no memory the caller must keep alive.
    if let Err(e) = unsafe { signal(Signal::SIGPIPE, SigHandler::SigDfl) } {
        eprintln!("openflux: can't reset SIGPIPE disposition: {e}");
    }
}

#[cfg(not(unix))]
pub fn restore_default_sigpipe() {}
