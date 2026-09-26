//! Filesystem layout. Config and state live under the *invoking* user's home even when
//! the CLI is run through `sudo` for TUN mode (TUN needs root): root's HOME would point
//! at a different config, and the navigation subsystem otherwise stores the same profile
//! the user configured as a non-root user.

use anyhow::{Context, Result};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
    pub config_file: PathBuf,
    pub engine_log: PathBuf,
    pub engine_pid: PathBuf,
    pub proxy_state: PathBuf,
}

/// If invoked under sudo/pkexec as root, resolve the real user's home via `SUDO_UID` /
/// `PKEXEC_UID`; otherwise fall back to the current user's home. This mirrors the Android
/// app's "gateway under a different uid" behaviour: config lives with the user who owns it.
#[cfg(unix)]
fn base_home() -> Result<PathBuf> {
    let uid = std::env::var("SUDO_UID")
        .ok()
        .or_else(|| std::env::var("PKEXEC_UID").ok())
        .and_then(|v| v.parse::<u32>().ok());
    if let Some(uid) = uid {
        if let Some(user) = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
            .context("lookup invoking uid user")?
        {
            return Ok(user.dir);
        }
    }
    dirs::home_dir().context("cannot determine home directory")
}

/// Windows has no sudo/pkexec elevation-by-uid: config always lives under the current
/// user's home.
#[cfg(windows)]
fn base_home() -> Result<PathBuf> {
    dirs::home_dir().context("cannot determine home directory")
}

impl Paths {
    pub fn discover() -> Result<Self> {
        if let Some(dir) = std::env::var_os("OPENFLUX_CONFIG_DIR") {
            return Ok(Self::under(PathBuf::from(dir)));
        }
        let home = base_home()?;
        let config_dir = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("openflux");
        let state_dir = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local").join("state"))
            .join("openflux");
        Ok(Self::under_ex(config_dir, state_dir))
    }

    /// All paths anchored at a single directory (used by `--config-dir` and tests).
    pub fn under(dir: PathBuf) -> Self {
        Self::under_ex(dir.clone(), dir)
    }

    fn under_ex(config_dir: PathBuf, state_dir: PathBuf) -> Self {
        Paths {
            config_file: config_dir.join("openflux.toml"),
            engine_log: state_dir.join("engine.log"),
            engine_pid: state_dir.join("engine.pid"),
            proxy_state: state_dir.join("proxy_state.json"),
            config_dir,
            state_dir,
        }
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.config_dir).context("create config dir")?;
        std::fs::create_dir_all(&self.state_dir).context("create state dir")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_honors_config_dir_override() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("OPENFLUX_CONFIG_DIR", tmp.path());
        let paths = Paths::discover().unwrap();
        std::env::remove_var("OPENFLUX_CONFIG_DIR");
        assert_eq!(paths.config_file, tmp.path().join("openflux.toml"));
        assert_eq!(paths.engine_pid, tmp.path().join("engine.pid"));
    }
}
