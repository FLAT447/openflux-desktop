//! Profile storage (TOML at `~/.config/openflux/openflux.toml`). Secrets (key token,
//! doc URL) live in the same file; the file is written mode 0600.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;
use std::str::FromStr;

pub const DEFAULT_MTU: u32 = 1400;
pub const DEFAULT_DNS: &str = "77.88.8.8";
pub const DEFAULT_SOCKS_PORT: u16 = 1080;
/// `captcha_solve_mode` values understood by the engine's `--captcha-solve-mode` flag.
pub const CAPTCHA_SOLVE_OFF: &str = "off";
pub const CAPTCHA_SOLVE_HEADLESS: &str = "headless_browser";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProfileMode {
    #[default]
    Manual,
    Key,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    #[default]
    Yandex,
    Volga,
    Oneme,
    YandexMultistream,
    Cupsonline,
    Mailru,
    Boards,
}

impl Transport {
    pub const ALL: &'static [Self] = &[
        Self::Yandex,
        Self::Volga,
        Self::Oneme,
        Self::YandexMultistream,
        Self::Cupsonline,
        Self::Mailru,
        Self::Boards,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Yandex => "yandex",
            Self::Volga => "volga",
            Self::Oneme => "oneme",
            Self::YandexMultistream => "yandex_multistream",
            Self::Cupsonline => "cupsonline",
            Self::Mailru => "mailru",
            Self::Boards => "boards",
        }
    }

    pub fn requires_doc_url(self) -> bool {
        !matches!(self, Self::Oneme)
    }

    pub fn supports_e2e(self) -> bool {
        matches!(self, Self::Yandex | Self::YandexMultistream)
    }

    /// Only the Yandex Docs transports have a CAPTCHA path the engine can solve with a
    /// local headless browser.
    pub fn supports_captcha_solve(self) -> bool {
        matches!(self, Self::Yandex | Self::YandexMultistream)
    }
}

impl FromStr for Transport {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "yandex" => Ok(Self::Yandex),
            "volga" => Ok(Self::Volga),
            "oneme" => Ok(Self::Oneme),
            "yandex_multistream" => Ok(Self::YandexMultistream),
            "cupsonline" => Ok(Self::Cupsonline),
            "mailru" => Ok(Self::Mailru),
            "boards" => Ok(Self::Boards),
            other => Err(format!(
                "unsupported transport '{other}' (expected one of: {})",
                Self::ALL
                    .iter()
                    .map(|transport| transport.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

impl fmt::Display for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    #[default]
    Legacy,
    Batched,
}

impl Codec {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Batched => "batched",
        }
    }
}

impl FromStr for Codec {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "legacy" => Ok(Self::Legacy),
            "batched" => Ok(Self::Batched),
            other => Err(format!(
                "unsupported codec '{other}' (expected legacy or batched)"
            )),
        }
    }
}

impl fmt::Display for Codec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
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
    pub transport: Transport,
    #[serde(default)]
    pub codec: Codec,
    #[serde(default)]
    pub max_token: String,
    #[serde(default)]
    pub max_uid: String,
    #[serde(default)]
    pub e2e_encryption: bool,
    /// How the engine reacts to a Yandex bot check: ""/"off" disables solving,
    /// "headless_browser" opens the doc in a local Chrome and solves the check.
    #[serde(default)]
    pub captcha_solve_mode: String,
    #[serde(default = "DEFAULT_MTU_u")]
    pub mtu: u32,
    /// Parallel WebSocket streams (multistream). 1 = single stream.
    #[serde(default = "default_streams")]
    pub streams: u16,
    /// Tuning that used to be stored per profile. These four keys are read so an older
    /// `openflux.toml` keeps its values - `AppConfig::load` folds them into the global
    /// settings - and they are never written back.
    #[serde(default, rename = "dns_upstream", skip_serializing)]
    pub legacy_dns: String,
    #[serde(default, rename = "socks_port", skip_serializing)]
    pub legacy_socks_port: u16,
    #[serde(default, rename = "split_mode", skip_serializing)]
    pub legacy_split_mode: String,
    #[serde(default, rename = "split_sites", skip_serializing)]
    pub legacy_split_sites: Vec<String>,
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

    pub fn is_connectable(&self) -> bool {
        match self.transport {
            Transport::Oneme => {
                !self.max_token.trim().is_empty() && !self.max_uid.trim().is_empty()
            }
            Transport::YandexMultistream => self.doc_urls.len() >= 2,
            _ => !self.doc_url.trim().is_empty(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            bail!("profile name is required");
        }
        if !(1..=8).contains(&self.streams) {
            bail!("streams must be between 1 and 8, got {}", self.streams);
        }
        if self.transport == Transport::YandexMultistream && !self.doc_urls.is_empty() {
            if self.doc_urls.len() < 2 {
                bail!("yandex_multistream requires at least two doc_urls");
            }
            if self.doc_urls.len() > 8 {
                bail!("yandex_multistream supports at most eight doc_urls");
            }
            if self.doc_urls.iter().any(|url| url.trim().is_empty()) {
                bail!("doc_urls must not contain empty URLs");
            }
        }
        if self.mode == ProfileMode::Manual
            && self.transport == Transport::YandexMultistream
            && self.doc_urls.len() < 2
        {
            bail!("manual yandex_multistream profiles require --doc-urls with at least two URLs");
        }
        if self.transport == Transport::Oneme {
            if self.max_token.trim().is_empty() {
                bail!("OneMe profiles require max_token");
            }
            let uid = self
                .max_uid
                .trim()
                .parse::<i64>()
                .context("OneMe max_uid must be a positive integer")?;
            if uid <= 0 {
                bail!("OneMe max_uid must be a positive integer");
            }
        }
        if self.e2e_encryption && !self.transport.supports_e2e() {
            bail!(
                "e2e encryption is not supported by transport '{}'",
                self.transport
            );
        }
        match self.captcha_solve_mode.trim() {
            "" | CAPTCHA_SOLVE_OFF | CAPTCHA_SOLVE_HEADLESS => {}
            other => bail!(
                "captcha_solve_mode must be empty, 'off' or 'headless_browser', got '{other}'"
            ),
        }
        if self.captcha_solve_mode.trim() == CAPTCHA_SOLVE_HEADLESS
            && !self.transport.supports_captcha_solve()
        {
            bail!(
                "captcha_solve_mode is not supported by transport '{}'",
                self.transport
            );
        }
        if self.mode == ProfileMode::Manual
            && self.transport.requires_doc_url()
            && self.doc_url.trim().is_empty()
            && self.doc_urls.is_empty()
        {
            bail!(
                "manual profiles require a doc URL for transport '{}'",
                self.transport
            );
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
            transport: Transport::Yandex,
            codec: Codec::Legacy,
            max_token: String::new(),
            max_uid: String::new(),
            e2e_encryption: false,
            captcha_solve_mode: String::new(),
            mtu: DEFAULT_MTU,
            streams: 1,
            legacy_dns: String::new(),
            legacy_socks_port: 0,
            legacy_split_mode: String::new(),
            legacy_split_sites: Vec::new(),
        }
    }
}

/// Split a comma-separated site list the way the CLI and the GUI both expect: trims
/// whitespace and leading dots, drops empty entries.
pub fn normalize_split_domains(csv: &str) -> Vec<String> {
    csv.split(',')
        .map(|site| site.trim().trim_start_matches('.').to_string())
        .filter(|site| !site.is_empty())
        .collect()
}

/// Split a comma-separated list of document URLs, trimming and dropping empty entries.
pub fn parse_doc_urls(csv: &str) -> Vec<String> {
    csv.split(',')
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_string)
        .collect()
}

/// The engine treats "off" and "" the same way, so profiles store only the empty default.
pub fn normalize_captcha_mode(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() || value == CAPTCHA_SOLVE_OFF {
        String::new()
    } else {
        value.to_string()
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

#[derive(Debug, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub active_profile: Option<String>,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    /// Local SOCKS5 listen port, shared by every profile: one engine serves one port, so
    /// this describes the machine rather than the account.
    #[serde(default = "DEFAULT_SOCKS_PORT_u")]
    pub socks_port: u16,
    /// Upstream DNS for TUN mode ("ip[:port]", "tls://host", "https://host/path").
    #[serde(default = "DEFAULT_DNS_str")]
    pub dns: String,
    /// Split-tunnel mode: "" (off), "exclude" (listed sites bypass the tunnel) or
    /// "include" (only listed sites use the tunnel).
    #[serde(default)]
    pub split_mode: String,
    /// Domains/IPs for `split_mode`; suffix wildcards like "*.ru" allowed.
    #[serde(default)]
    pub split_sites: Vec<String>,
    /// Verbose engine logging: every spawn gets `--debug`, so engine.log carries the
    /// transport's own diagnostics ([M-DOCS], [TUNNEL], packet traces) and not just the
    /// event stream. Also enabled by OPENFLUX_DEBUG=1 for a single run.
    #[serde(default)]
    pub debug: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            active_profile: None,
            profiles: Vec::new(),
            socks_port: DEFAULT_SOCKS_PORT,
            dns: DEFAULT_DNS.to_string(),
            split_mode: String::new(),
            split_sites: Vec::new(),
            debug: false,
        }
    }
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let mut cfg: AppConfig = match std::fs::read_to_string(path) {
            Ok(content) => toml::from_str(&content).context("parse config")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(AppConfig::default()),
            Err(e) => return Err(e).context("read config"),
        };
        cfg.adopt_legacy_profile_tuning();
        Ok(cfg)
    }

    /// Fold per-profile DNS/port/split values written by older versions into the globals.
    /// The active profile comes first, because it is the one the user was actually running;
    /// each knob then takes the first value found, so anything the active profile left unset
    /// (an empty split mode, typically) falls back to a profile that did set it instead of
    /// being lost.
    fn adopt_legacy_profile_tuning(&mut self) {
        let active_name = self.active_profile.clone();
        let order: Vec<usize> = (0..self.profiles.len())
            .filter(|i| Some(&self.profiles[*i].name) == active_name.as_ref())
            .chain(
                (0..self.profiles.len())
                    .filter(|i| Some(&self.profiles[*i].name) != active_name.as_ref()),
            )
            .collect();
        // A flag per knob, not "is the global still at its default?": the active profile may
        // legitimately hold exactly the default, and a later profile would then overwrite it.
        let (mut took_port, mut took_dns, mut took_split) = (false, false, false);
        for i in order {
            let profile = &mut self.profiles[i];
            if !took_port && profile.legacy_socks_port != 0 {
                self.socks_port = profile.legacy_socks_port;
                took_port = true;
            }
            if !took_dns && !profile.legacy_dns.trim().is_empty() {
                self.dns = profile.legacy_dns.trim().to_string();
                took_dns = true;
            }
            if !took_split && !profile.legacy_split_mode.trim().is_empty() {
                self.split_mode = profile.legacy_split_mode.trim().to_string();
                self.split_sites = std::mem::take(&mut profile.legacy_split_sites);
                took_split = true;
            }
        }
        for profile in &mut self.profiles {
            profile.legacy_dns.clear();
            profile.legacy_socks_port = 0;
            profile.legacy_split_mode.clear();
            profile.legacy_split_sites.clear();
        }
    }

    /// Check the global settings. Split mode and its list are machine-wide, so this is no
    /// longer part of `Profile::validate`.
    pub fn validate(&self) -> Result<()> {
        match self.split_mode.as_str() {
            "" | "exclude" | "include" => {}
            other => bail!("split_mode must be empty, 'exclude' or 'include', got '{other}'"),
        }
        if self.socks_port == 0 {
            bail!("socks_port must be between 1 and 65535, got 0");
        }
        if self.dns.trim().is_empty() {
            bail!("dns must not be empty");
        }
        Ok(())
    }

    /// The split list as the comma-separated form the CLI, TUI and GUI all edit.
    pub fn split_csv(&self) -> String {
        self.split_sites.join(",")
    }

    /// Whether split tunneling is active and therefore needs a site list.
    pub fn split_enabled(&self) -> bool {
        !self.split_mode.is_empty()
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
            let perms = std::fs::metadata(path)
                .context("stat config")?
                .permissions();
            let _ = std::fs::set_permissions(
                path,
                std::fs::Permissions::from_mode(perms.mode() | 0o600),
            );
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
        assert!(p.is_connectable());
        // The port lives on the config now, and a profile that never mentioned it keeps the
        // default.
        assert_eq!(loaded.socks_port, DEFAULT_SOCKS_PORT);
    }

    #[test]
    fn manual_profile_without_doc_url_is_not_connectable() {
        let p = Profile::manual("x", "");
        assert!(!p.is_connectable());
    }

    #[test]
    fn streams_stay_per_profile_while_split_is_global() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("openflux.toml");

        let mut cfg = AppConfig::default();
        let mut p = Profile::manual("dual", "https://x/i/d");
        p.streams = 4;
        cfg.add(p).unwrap();
        cfg.split_mode = "exclude".into();
        cfg.split_sites = vec!["*.ya.ru".to_string(), "example.com".to_string()];
        cfg.save(&path).unwrap();

        let loaded = AppConfig::load(&path).unwrap();
        assert_eq!(loaded.get("dual").unwrap().streams, 4);
        assert_eq!(loaded.split_mode, "exclude");
        assert_eq!(
            loaded.split_sites,
            vec!["*.ya.ru".to_string(), "example.com".to_string()]
        );
        // The knobs are stored once, at the top level, not inside every profile table.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("split_mode = \"exclude\""));
        assert_eq!(text.matches("split_mode").count(), 1);
        assert_eq!(text.matches("socks_port").count(), 1);
        assert_eq!(text.matches("dns_upstream").count(), 0);
    }

    #[test]
    fn per_profile_tuning_from_an_older_config_becomes_global() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("openflux.toml");
        // What version 0.1.0 wrote: the values lived on each profile.
        std::fs::write(
            &path,
            r#"
active_profile = "mail"
debug = true

[[profiles]]
name = "yandex-old"
mode = "manual"
doc_url = "https://docs.yandex.ru/i/a"
socks_port = 2080
dns_upstream = "1.1.1.1"
split_mode = "include"
split_sites = ["*.ya.ru"]

[[profiles]]
name = "mail"
mode = "manual"
doc_url = "https://docs.mail.ru/mail/doc"
socks_port = 1080
dns_upstream = "77.88.8.8"
split_mode = ""
split_sites = []
"#,
        )
        .unwrap();

        let cfg = AppConfig::load(&path).unwrap();
        // The old format always wrote a port and a DNS for every profile, so the active
        // profile's values are the ones the user was actually running and they win.
        assert_eq!(cfg.dns, "77.88.8.8");
        assert_eq!(cfg.socks_port, 1080);
        // The active profile had split turned off, so the fallback picks up the only profile
        // that configured a list - otherwise the list would be silently lost.
        assert_eq!(cfg.split_mode, "include");
        assert_eq!(cfg.split_sites, vec!["*.ya.ru".to_string()]);
        assert!(cfg.debug, "unrelated settings survive the migration");
        assert!(cfg.validate().is_ok());

        // And the migrated file no longer carries the per-profile copies.
        let path2 = tmp.path().join("migrated.toml");
        cfg.save(&path2).unwrap();
        let text = std::fs::read_to_string(&path2).unwrap();
        assert!(!text.contains("dns_upstream"));
        assert_eq!(text.matches("socks_port").count(), 1);
    }

    #[test]
    fn the_active_profiles_split_list_wins_over_another_profiles() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("openflux.toml");
        std::fs::write(
            &path,
            r#"
active_profile = "mail"

[[profiles]]
name = "other"
mode = "manual"
doc_url = "https://docs.yandex.ru/i/a"
split_mode = "exclude"
split_sites = ["*.ya.ru"]

[[profiles]]
name = "mail"
mode = "manual"
doc_url = "https://docs.mail.ru/mail/doc"
split_mode = "include"
split_sites = ["corp.example"]
"#,
        )
        .unwrap();

        let cfg = AppConfig::load(&path).unwrap();
        assert_eq!(cfg.split_mode, "include");
        assert_eq!(cfg.split_sites, vec!["corp.example".to_string()]);
    }

    #[test]
    fn global_settings_are_validated() {
        let mut cfg = AppConfig::default();
        assert!(cfg.validate().is_ok());
        cfg.split_mode = "sideways".into();
        assert!(cfg.validate().is_err());
        cfg.split_mode = String::new();
        cfg.socks_port = 0;
        assert!(cfg.validate().is_err());
        cfg.socks_port = DEFAULT_SOCKS_PORT;
        cfg.dns = "  ".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn legacy_config_without_transport_fields_still_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("openflux.toml");
        std::fs::write(
            &path,
            r#"
active_profile = "old"

[[profiles]]
name = "old"
mode = "manual"
doc_url = "https://disk.yandex.ru/i/legacy"
socks_port = 1080
mtu = 1400
dns_upstream = "77.88.8.8"
split_sites = []
streams = 1
e2e_encryption = false
key_token = ""
control_url = ""
doc_urls = []
split_mode = ""
"#,
        )
        .unwrap();

        let cfg = AppConfig::load(&path).unwrap();
        let p = cfg.get("old").unwrap();
        assert_eq!(p.transport, Transport::Yandex);
        assert_eq!(p.codec, Codec::Legacy);
        assert!(p.max_token.is_empty());
        assert!(p.max_uid.is_empty());
        assert!(p.validate().is_ok());
    }

    #[test]
    fn multistream_rejects_bad_url_lists() {
        let mut profile = Profile::manual("multi", "");
        profile.transport = Transport::YandexMultistream;
        profile.streams = 2;
        profile.doc_urls = vec!["https://docs.yandex.ru/i/a".into(), " ".into()];
        assert!(profile.validate().is_err());

        profile.doc_urls = vec!["https://docs.yandex.ru/i/a".into()];
        assert!(profile.validate().is_err());

        profile.doc_urls = (0..9)
            .map(|i| format!("https://docs.yandex.ru/i/{i}"))
            .collect();
        assert!(profile.validate().is_err());
    }

    #[test]
    fn multistream_allows_e2e_but_other_transports_do_not() {
        let mut profile = Profile::manual("multi", "");
        profile.transport = Transport::YandexMultistream;
        profile.doc_urls = vec![
            "https://docs.yandex.ru/i/a".into(),
            "https://docs.yandex.ru/i/b".into(),
        ];
        profile.streams = 2;
        profile.e2e_encryption = true;
        assert!(profile.validate().is_ok());

        profile.transport = Transport::Volga;
        profile.doc_url = "https://docs.yandex.ru/i/a".into();
        profile.doc_urls.clear();
        assert!(profile.validate().is_err());
    }

    #[test]
    fn oneme_uid_must_be_numeric_and_positive() {
        let mut profile = Profile::manual("max", "");
        profile.transport = Transport::Oneme;
        profile.max_token = "token".into();
        for uid in ["0", "-1", "abc", "1.5", ""] {
            profile.max_uid = uid.into();
            assert!(profile.validate().is_err(), "uid {uid:?} must be rejected");
        }
        profile.max_uid = "42".into();
        assert!(profile.validate().is_ok());
    }

    #[test]
    fn key_profiles_do_not_require_doc_url_or_credentials() {
        let mut profile = Profile::key("remote", "https://control.example.com", "token");
        assert!(profile.validate().is_ok());
        assert!(!profile.is_connectable());

        profile.transport = Transport::YandexMultistream;
        profile.doc_urls = vec![
            "https://docs.yandex.ru/i/a".into(),
            "https://docs.yandex.ru/i/b".into(),
        ];
        assert!(profile.validate().is_ok());
        assert!(profile.is_connectable());

        profile.transport = Transport::Oneme;
        profile.doc_urls.clear();
        assert!(profile.validate().is_err());
        profile.max_token = "t".into();
        profile.max_uid = "7".into();
        assert!(profile.validate().is_ok());
        assert!(profile.is_connectable());
    }

    #[test]
    fn validate_requires_a_profile_name() {
        let mut profile = Profile::manual(" ", "https://disk.yandex.ru/i/a");
        assert!(profile.validate().is_err());
        profile.name = "ok".into();
        assert!(profile.validate().is_ok());
    }

    #[test]
    fn split_domain_and_doc_url_lists_share_one_normalisation() {
        assert_eq!(
            normalize_split_domains("*.ya.ru, .yandex.ru ,,example.com"),
            vec![
                "*.ya.ru".to_string(),
                "yandex.ru".to_string(),
                "example.com".to_string()
            ]
        );
        assert!(normalize_split_domains("  , ").is_empty());
        assert_eq!(
            parse_doc_urls(" https://a , ,https://b "),
            vec!["https://a".to_string(), "https://b".to_string()]
        );
        assert!(parse_doc_urls("  ").is_empty());
    }

    #[test]
    fn transport_capabilities_match_what_the_engine_accepts() {
        // engine/main.go only calls EnableEncryptedSelfCompression/SetCaptchaSolveMode on
        // YandexDocsTransport, and every transport except oneme needs a document URL.
        for transport in Transport::ALL {
            assert_eq!(
                transport.supports_e2e(),
                matches!(transport, Transport::Yandex | Transport::YandexMultistream),
                "{transport} e2e support"
            );
            assert_eq!(
                transport.supports_captcha_solve(),
                transport.supports_e2e(),
                "{transport} captcha support"
            );
            assert_eq!(
                transport.requires_doc_url(),
                *transport != Transport::Oneme,
                "{transport} doc URL requirement"
            );
        }
        assert_eq!(
            Transport::ALL.len(),
            7,
            "engine switch has seven transports"
        );
    }

    #[test]
    fn captcha_solve_mode_is_limited_to_yandex_transports() {
        let mut profile = Profile::manual("y", "https://disk.yandex.ru/i/a");
        assert!(profile.captcha_solve_mode.is_empty());
        assert!(profile.validate().is_ok());

        profile.captcha_solve_mode = CAPTCHA_SOLVE_OFF.into();
        assert!(profile.validate().is_ok());

        profile.captcha_solve_mode = CAPTCHA_SOLVE_HEADLESS.into();
        assert!(profile.validate().is_ok());
        assert!(Transport::Yandex.supports_captcha_solve());
        assert!(Transport::YandexMultistream.supports_captcha_solve());

        for transport in Transport::ALL {
            // Every transport needs its own otherwise-valid profile: multistream wants two
            // doc URLs, OneMe wants credentials, the rest a single doc URL.
            let mut candidate = Profile::manual("p", "https://docs.yandex.ru/i/a");
            candidate.transport = *transport;
            candidate.captcha_solve_mode = CAPTCHA_SOLVE_HEADLESS.into();
            match transport {
                Transport::YandexMultistream => {
                    candidate.doc_url.clear();
                    candidate.doc_urls = vec!["https://a".into(), "https://b".into()];
                }
                Transport::Oneme => {
                    candidate.doc_url.clear();
                    candidate.max_token = "token".into();
                    candidate.max_uid = "7".into();
                }
                _ => {}
            }
            if transport.supports_captcha_solve() {
                assert!(
                    candidate.validate().is_ok(),
                    "{transport} must accept the mode"
                );
            } else {
                assert!(
                    candidate.validate().is_err(),
                    "{transport} must reject the mode: {:?}",
                    candidate.validate().unwrap_err().to_string()
                );
            }
        }

        profile.transport = Transport::Yandex;
        profile.captcha_solve_mode = "selenium-grid".into();
        assert!(profile.validate().is_err());

        assert_eq!(normalize_captcha_mode("off"), "");
        assert_eq!(normalize_captcha_mode(""), "");
        assert_eq!(
            normalize_captcha_mode(" headless_browser "),
            CAPTCHA_SOLVE_HEADLESS
        );
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

        // Split mode is machine-wide now, so it is validated with the rest of the globals
        // rather than per profile (covered by `global_settings_are_validated`).
        assert!(Profile::manual("y", "https://x/i/b").validate().is_ok());
    }

    #[test]
    fn default_transport_and_codec_are_compatible() {
        let profile = Profile::manual("default", "https://docs.yandex.ru/i/a");
        assert_eq!(profile.transport, Transport::Yandex);
        assert_eq!(profile.codec, Codec::Legacy);
        assert!(profile.validate().is_ok());
    }

    #[test]
    fn debug_flag_defaults_off_and_survives_a_config_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("openflux.toml");

        let mut cfg = AppConfig::load(&path).expect("fresh config");
        assert!(!cfg.debug, "verbose logging must stay opt-in");
        cfg.profiles
            .push(Profile::manual("p", "https://docs.yandex.ru/i/a"));
        cfg.debug = true;
        cfg.save(&path).expect("save");

        let reloaded = AppConfig::load(&path).expect("reload");
        assert!(
            reloaded.debug,
            "debug flag must round-trip through the file"
        );
        assert_eq!(reloaded.profiles.len(), 1);
    }

    #[test]
    fn a_config_without_the_debug_key_still_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("openflux.toml");
        std::fs::write(
            &path,
            "[[profiles]]\nname = \"legacy\"\nmode = \"manual\"\ntransport = \"yandex\"\ncodec = \"legacy\"\ndoc_url = \"https://docs.yandex.ru/i/a\"\n",
        )
        .expect("write");

        let cfg = AppConfig::load(&path).expect("legacy config without debug key");
        assert!(!cfg.debug);
        assert_eq!(cfg.profiles[0].name, "legacy");
    }

    #[test]
    fn validates_multistream_and_oneme_requirements() {
        let mut multistream = Profile::manual("multi", "");
        multistream.transport = Transport::YandexMultistream;
        assert!(multistream.validate().is_err());
        multistream.doc_urls = vec![
            "https://docs.yandex.ru/i/a".into(),
            "https://docs.yandex.ru/i/b".into(),
        ];
        multistream.streams = 2;
        assert!(multistream.validate().is_ok());
        assert!(multistream.is_connectable());

        let mut oneme = Profile::manual("max", "");
        oneme.transport = Transport::Oneme;
        assert!(oneme.validate().is_err());
        oneme.max_token = "token".into();
        oneme.max_uid = "42".into();
        assert!(oneme.validate().is_ok());
        assert!(oneme.is_connectable());
    }

    #[test]
    fn rejects_e2e_for_unsupported_transport() {
        let mut profile = Profile::manual("mail", "https://mail.ru/doc/1");
        profile.transport = Transport::Mailru;
        profile.e2e_encryption = true;
        assert!(profile.validate().is_err());
    }
}
