//! Profile storage (TOML at `~/.config/openflux/openflux.toml`). Secrets (key token,
//! doc URL) live in the same file; the file is written mode 0600.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const DEFAULT_MTU: u32 = 1400;
pub const DEFAULT_DNS: &str = "77.88.8.8";
pub const DEFAULT_SOCKS_PORT: u16 = 1080;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProfileMode {
    #[default]
    Manual,
    Key,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub mode: ProfileMode,
    #[serde(default)]
    pub doc_url: String,
    #[serde(default)]
    pub doc_urls: Vec<String>,
    #[serde(default)]
    pub control_url: String,
    #[serde(default)]
    pub key_token: String,
    #[serde(default)]
    pub e2e_encryption: bool,
    #[serde(default = "DEFAULT_MTU_u")]
    pub mtu: u32,
    #[serde(default = "DEFAULT_DNS_str")]
    pub dns_upstream: String,
    #[serde(default = "DEFAULT_SOCKS_PORT_u")]
    pub socks_port: u16,
    /// Parallel WebSocket streams (multistream). 1 = single stream.
    #[serde(default = "default_streams")]
    pub streams: u16,
    /// Tunnel split mode: "" (off), "exclude" (listed sites bypass the tunnel) or
    /// "include" (only listed sites use the tunnel).
    #[serde(default)]
    pub split_mode: String,
    /// Domains/IPs for `split_mode`; suffix wildcards like "*.ru" allowed.
    #[serde(default)]
    pub split_sites: Vec<String>,
}

impl Profile {
    pub fn manual(name: &str, doc_url: &str) -> Self {
        Self {
            name: name.to_string(),
            mode: ProfileMode::Manual,
            doc_url: doc_url.to_string(),
            ..Profile::default()
        }
    }

    pub fn key(name: &str, control_url: &str, key_token: &str) -> Self {
        Self {
            name: name.to_string(),
            mode: ProfileMode::Key,
            control_url: control_url.to_string(),
            key_token: key_token.to_string(),
            ..Profile::default()
        }
    }

    /// A profile can connect once its effective doc_url is known (manual profiles store
    /// it directly; key profiles only after `check-key` resolves it).
    pub fn is_connectable(&self) -> bool {
        !self.doc_url.trim().is_empty()
    }

    pub fn validate(&self) -> Result<()> {
        if !(1..=8).contains(&self.streams) {
            bail!("streams must be between 1 and 8, got {}", self.streams);
        }
        match self.split_mode.as_str() {
            "" | "exclude" | "include" => {}
            other => bail!("split_mode must be empty, 'exclude' or 'include', got '{other}'"),
        }
        Ok(())
    }
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            name: String::new(),
            mode: ProfileMode::Manual,
            doc_url: String::new(),
            doc_urls: Vec::new(),
            control_url: String::new(),
            key_token: String::new(),
            e2e_encryption: false,
            mtu: DEFAULT_MTU,
            dns_upstream: DEFAULT_DNS.to_string(),
            socks_port: DEFAULT_SOCKS_PORT,
            streams: 1,
            split_mode: String::new(),
            split_sites: Vec::new(),
        }
    }
}

#[allow(non_snake_case)]
fn DEFAULT_MTU_u() -> u32 {
    DEFAULT_MTU
}
#[allow(non_snake_case)]
fn DEFAULT_DNS_str() -> String {
    DEFAULT_DNS.to_string()
}
#[allow(non_snake_case)]
fn DEFAULT_SOCKS_PORT_u() -> u16 {
    DEFAULT_SOCKS_PORT
}

fn default_streams() -> u16 {
    1
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub active_profile: Option<String>,
    #[serde(default)]
    pub profiles: Vec<Profile>,
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(content) => toml::from_str(&content).context("parse config"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AppConfig::default()),
            Err(e) => Err(e).context("read config"),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;
        let content = toml::to_string_pretty(self).context("serialize config")?;
        std::fs::write(path, content).context("write config")?;
        // Secrets live in this file; keep it owner-readable only (best effort on Windows,
        // where the filesystem enforces its own ACLs instead of a 0600 mode).
        #[cfg(unix)]
        {
            let perms = std::fs::metadata(path).context("stat config")?.permissions();
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(perms.mode() | 0o600));
        }
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == name)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Profile> {
        self.profiles.iter_mut().find(|p| p.name == name)
    }

    pub fn add(&mut self, profile: Profile) -> Result<()> {
        if self.get(&profile.name).is_some() {
            bail!("profile '{}' already exists", profile.name);
        }
        profile.validate()?;
        if self.profiles.is_empty() {
            self.active_profile = Some(profile.name.clone());
        }
        self.profiles.push(profile);
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Result<()> {
        let before = self.profiles.len();
        self.profiles.retain(|p| p.name != name);
        if self.profiles.len() == before {
            bail!("profile '{}' not found", name);
        }
        if self.active_profile.as_deref() == Some(name) {
            self.active_profile = self.profiles.first().map(|p| p.name.clone());
        }
        Ok(())
    }

    pub fn set_active(&mut self, name: &str) -> Result<()> {
        if self.get(name).is_none() {
            bail!("profile '{}' not found", name);
        }
        self.active_profile = Some(name.to_string());
        Ok(())
    }

    pub fn active(&self) -> Result<&Profile> {
        let name = self
            .active_profile
            .as_deref()
            .context("no active profile set; use `openflux set-active <name>`")?;
        self.get(name)
            .with_context(|| format!("active profile '{name}' not found"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_profile_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("openflux.toml");

        let mut cfg = AppConfig::default();
        cfg.add(Profile {
            name: "home".into(),
            mode: ProfileMode::Key,
            doc_url: "https://disk.yandex.ru/i/abc".into(),
            control_url: "https://control.example.com".into(),
            key_token: "key_secret".into(),
            mtu: 1350,
            ..Profile::default()
        })
        .unwrap();
        cfg.save(&path).unwrap();

        let loaded = AppConfig::load(&path).unwrap();
        let p = loaded.get("home").unwrap();
        assert_eq!(p.mode, ProfileMode::Key);
        assert_eq!(p.doc_url, "https://disk.yandex.ru/i/abc");
        assert_eq!(p.mtu, 1350);
        assert_eq!(p.socks_port, DEFAULT_SOCKS_PORT);
        assert!(p.is_connectable());
    }

    #[test]
    fn manual_profile_without_doc_url_is_not_connectable() {
        let p = Profile::manual("x", "");
        assert!(!p.is_connectable());
    }

    #[test]
    fn new_stream_and_split_fields_roundtrip_through_storage() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("openflux.toml");

        let mut cfg = AppConfig::default();
        let mut p = Profile::manual("dual", "https://x/i/d");
        p.streams = 4;
        p.split_mode = "exclude".to_string();
        p.split_sites = vec!["*.ya.ru".to_string(), "example.com".to_string()];
        cfg.add(p).unwrap();
        cfg.save(&path).unwrap();

        let loaded = AppConfig::load(&path).unwrap();
        let p = loaded.get("dual").unwrap();
        assert_eq!(p.streams, 4);
        assert_eq!(p.split_mode, "exclude");
        assert_eq!(p.split_sites, vec!["*.ya.ru".to_string(), "example.com".to_string()]);
    }

    #[test]
    fn validate_rejects_bad_stream_count_and_split_mode() {
        let mut p = Profile::manual("x", "https://x/i/a");
        p.streams = 0;
        assert!(p.validate().is_err());
        p.streams = 9;
        assert!(p.validate().is_err());
        p.streams = 8;
        assert!(p.validate().is_ok());

        let mut p = Profile::manual("y", "https://x/i/b");
        p.split_mode = "banana".to_string();
        assert!(p.validate().is_err());
        p.split_mode = "include".to_string();
        assert!(p.validate().is_ok());
    }
}