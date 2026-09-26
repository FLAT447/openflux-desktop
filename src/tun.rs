//! TUN mode: full-system capture. The implementation is platform-specific:
//! - Unix: the CLI (running as root via pkexec) creates a TUN device, installs policy
//!   routing that sends everything through it while keeping the engine's own sockets on
//!   the physical link (fwmark), then hands the TUN fd to the Go engine's gateway.
//! - Windows: the elevated launcher brings up a Wintun adapter + /1 netsh routes and hands
//!   the Wintun session handle to the Go engine, so session/interface lifetime follows the
//!   engine process just like the TUN fd does on Unix.

#[derive(Debug, Clone)]
pub struct TunConfig {
    pub engine_bin: std::path::PathBuf,
    pub transport: crate::engine::TransportConfig,
    pub token: Option<String>,
    pub tun_name: String,
    pub tun_addr: String,
    pub mtu: u32,
    pub dns: String,
    /// Parallel WebSocket streams for the TUN tunnel.
    pub streams: u16,
    /// Split mode ("", "exclude", "include") for the passed-through `split_sites`.
    pub split_mode: String,
    pub split_sites: Vec<String>,
    pub debug: bool,
    pub engine_log: std::path::PathBuf,
    pub engine_pid: std::path::PathBuf,
}

#[cfg(unix)]
mod unix_impl;
#[cfg(unix)]
pub use unix_impl::{is_up, lan_cidr, off, on, require_root};

#[cfg(windows)]
mod windows_impl;
#[cfg(windows)]
pub use windows_impl::{is_up, off, on, require_root};
