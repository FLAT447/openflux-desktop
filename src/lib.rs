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