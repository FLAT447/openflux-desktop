//! Profile import via `openflux://import?data=<base64url-json>`, mirroring the Android
//! app's `ProfileDeepLink` (payload = JSON without the profile id, base64url without
//! padding). The v1 desktop client keeps the same fields and ignores what it has no use
//! for yet (max_token/max_uid/transport).

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use serde::Deserialize;

use crate::config::{Profile, ProfileMode, DEFAULT_DNS, DEFAULT_MTU, DEFAULT_SOCKS_PORT};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct ImportProfile {
    #[serde(default)]
    name: String,
    /// `"manual"` → manual profile, anything else is treated as a key profile.
    #[serde(default)]
    mode: String,
    #[serde(default)]
    control_url: String,
    #[serde(default)]
    key_token: String,
    #[serde(default)]
    transport: String,
    #[serde(default)]
    doc_url: String,
    #[serde(default)]
    doc_urls: Vec<String>,
    #[serde(default = "default_mtu")]
    mtu: u32,
    #[serde(default = "default_dns")]
    dns_upstream: String,
    #[serde(default)]
    e2e_encryption: bool,
}

fn default_mtu() -> u32 {
    DEFAULT_MTU
}
fn default_dns() -> String {
    DEFAULT_DNS.to_string()
}

/// Import a profile from an `openflux://import?data=...` deep link or a raw base64url
/// string.
pub fn import_link(input: &str) -> Result<Profile> {
    let b64 = extract_data_param(input);
    let json = decode_base64(&b64)?;
    let parsed: ImportProfile = serde_json::from_slice(&json).context("parse import payload")?;
    if parsed.name.trim().is_empty() {
        bail!("import payload misses a profile name");
    }
    let mode = if parsed.mode == "manual" { ProfileMode::Manual } else { ProfileMode::Key };
    if !parsed.transport.is_empty() && parsed.transport != "yandex" {
        bail!(
            "import payload uses unsupported transport '{}' (v1 supports only yandex)",
            parsed.transport
        );
    }
    Ok(Profile {
        name: parsed.name,
        mode,
        doc_url: parsed.doc_url,
        doc_urls: parsed.doc_urls,
        control_url: parsed.control_url,
        key_token: parsed.key_token,
        e2e_encryption: parsed.e2e_encryption,
        mtu: parsed.mtu,
        dns_upstream: parsed.dns_upstream,
        socks_port: DEFAULT_SOCKS_PORT,
        ..Profile::default()
    })
}

/// Pull the `data` query parameter out of an openflux:// link; bare base64 passes through
/// untouched. Percent-escapes (from a URL paste) are decoded on the extracted value.
fn extract_data_param(input: &str) -> String {
    let raw = if let Some(pos) = input.find('?') {
        // Naive query parse: find the `data=` value, stop at `&`.
        let mut value = "";
        for part in input[pos + 1..].split('&') {
            if let Some(rest) = part.strip_prefix("data=") {
                value = rest;
                break;
            }
        }
        value.to_string()
    } else {
        input.to_string()
    };
    percent_decode(&raw)
}

fn percent_decode(input: &str) -> String {
    if !input.contains('%') {
        return input.to_string();
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = hex_val(bytes[i + 1]);
            let lo = hex_val(bytes[i + 2]);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push(hi << 4 | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// The Android app uses URL_SAFE|NO_PADDING; accept the common variants too.
fn decode_base64(input: &str) -> Result<Vec<u8>> {
    let trimmed = input.trim().to_string();
    for engine in [URL_SAFE_NO_PAD, URL_SAFE, STANDARD_NO_PAD, STANDARD] {
        if let Ok(bytes) = engine.decode(&trimmed) {
            return Ok(bytes);
        }
    }
    // A round-tripped link may carry a trailing %3D that leaves a dangling pad; retry
    // without it.
    let trimmed = trimmed.trim_end_matches('=');
    for engine in [URL_SAFE, STANDARD] {
        if let Ok(bytes) = engine.decode(trimmed) {
            return Ok(bytes);
        }
    }
    bail!("invalid base64 data in import link")
}

#[cfg(test)]
mod tests {
    use super::*;

    // The exact payload shape the Android app emits for a share link; values are fake
    // placeholders (a real key token must never be committed).
    const SAMPLE: &str = "eyJjb250cm9sX3VybCI6Imh0dHBzOi8vMjAzLjAuMTEzLjEwIiwiZG9jX3VybCI6Imh0dHBzOi8vZG9jcy55YW5kZXgucnUvaS9kZW1vMDAwIiwia2V5X3Rva2VuIjoia2V5X3RfMDAwMDAwMDAwMDAwMDAwMDAwMDAwMCIsIm1vZGUiOiJrZXkiLCJuYW1lIjoiZGVtbyIsInRyYW5zcG9ydCI6InlhbmRleCJ9";

    #[test]
    fn decodes_sample_deep_link() {
        let p = import_link(&format!("openflux://import?data={SAMPLE}")).unwrap();
        assert_eq!(p.name, "demo");
        assert_eq!(p.mode, ProfileMode::Key);
        assert_eq!(p.control_url, "https://203.0.113.10");
        assert_eq!(p.doc_url, "https://docs.yandex.ru/i/demo000");
        assert_eq!(p.key_token, "key_t_0000000000000000000000");
        assert_eq!(p.mtu, DEFAULT_MTU); // payload carries no mtu → default 1400
        assert!(!p.e2e_encryption);
    }

    #[test]
    fn raw_base64_also_works() {
        let p = import_link(SAMPLE).unwrap();
        assert_eq!(p.name, "demo");
    }

    #[test]
    fn percent_escape_is_decoded() {
        let p = import_link(&format!("openflux://import?data={SAMPLE}%3D")).unwrap();
        assert_eq!(p.name, "demo");
    }
}