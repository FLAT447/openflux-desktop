//! Profile import via `openflux://import?data=<base64url-json>`, mirroring the Android
//! app's `ProfileDeepLink` (payload = JSON without the profile id, base64url without
//! padding). The desktop client stores all transport-specific fields supplied by the link.

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use serde::Deserialize;

use crate::config::{
    Codec, Profile, ProfileMode, Transport, DEFAULT_MTU,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct ImportProfile {
    #[serde(default)]
    name: String,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    control_url: String,
    #[serde(default)]
    key_token: String,
    #[serde(default)]
    transport: String,
    #[serde(default)]
    codec: String,
    #[serde(default)]
    doc_url: String,
    #[serde(default)]
    doc_urls: Vec<String>,
    #[serde(default)]
    max_token: String,
    #[serde(default)]
    max_uid: Option<StringOrNumber>,
    #[serde(default = "default_mtu")]
    mtu: u32,
    /// DNS upstream for TUN mode. Global, not profile data: the import hands it back
    /// separately. `Option` so "the link said nothing" stays distinguishable from "the link
    /// said the default"; `alias` keeps links built by older versions working.
    #[serde(default, alias = "dns")]
    dns_upstream: Option<String>,
    #[serde(default)]
    e2e_encryption: bool,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum StringOrNumber {
    String(String),
    Number(i64),
}

fn default_mtu() -> u32 {
    DEFAULT_MTU
}
/// What a share link produced: the profile, plus the global settings it carried.
#[derive(Debug)]
pub struct Imported {
    pub profile: Profile,
    /// DNS upstream, when the link specified one.
    pub dns: Option<String>,
}

/// Import a profile from an `openflux://import?data=...` deep link or a raw base64url
/// string.
pub fn import_link(input: &str) -> Result<Imported> {
    let b64 = extract_data_param(input);
    let json = decode_base64(&b64)?;
    let parsed: ImportProfile = serde_json::from_slice(&json).context("parse import payload")?;
    if parsed.name.trim().is_empty() {
        bail!("import payload misses a profile name");
    }
    let mode = if parsed.mode == "manual" {
        ProfileMode::Manual
    } else {
        ProfileMode::Key
    };
    let transport = if parsed.transport.trim().is_empty() {
        Transport::Yandex
    } else {
        parsed
            .transport
            .parse::<Transport>()
            .map_err(anyhow::Error::msg)
            .context("invalid transport in import payload")?
    };
    let codec = if parsed.codec.trim().is_empty() {
        Codec::Legacy
    } else {
        parsed
            .codec
            .parse::<Codec>()
            .map_err(anyhow::Error::msg)
            .context("invalid codec in import payload")?
    };
    let max_uid = parsed
        .max_uid
        .map(|value| match value {
            StringOrNumber::String(value) => value,
            StringOrNumber::Number(value) => value.to_string(),
        })
        .unwrap_or_default();
    let doc_urls = parsed.doc_urls;
    let streams = if transport == Transport::YandexMultistream && doc_urls.len() >= 2 {
        u16::try_from(doc_urls.len()).unwrap_or(u16::MAX)
    } else {
        1
    };
    let profile = Profile {
        name: parsed.name,
        mode,
        doc_url: parsed.doc_url,
        doc_urls,
        control_url: parsed.control_url,
        key_token: parsed.key_token,
        transport,
        codec,
        max_token: parsed.max_token,
        max_uid,
        e2e_encryption: parsed.e2e_encryption,
        mtu: parsed.mtu,
        streams,
        ..Profile::default()
    };
    profile.validate()?;
    Ok(Imported {
        profile,
        dns: parsed
            .dns_upstream
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty()),
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
        let imported = import_link(&format!("openflux://import?data={SAMPLE}")).unwrap();
        let p = imported.profile;
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
        let p = import_link(SAMPLE).unwrap().profile;
        assert_eq!(p.name, "demo");
    }

    #[test]
    fn percent_escape_is_decoded() {
        let p = import_link(&format!("openflux://import?data={SAMPLE}%3D"))
            .unwrap()
            .profile;
        assert_eq!(p.name, "demo");
    }

    #[test]
    fn imports_transport_specific_fields() {
        let payload = URL_SAFE_NO_PAD.encode(
            br#"{"name":"max","mode":"manual","transport":"oneme","codec":"batched","max_token":"token","max_uid":42}"#,
        );
        let profile = import_link(&payload).unwrap().profile;
        assert_eq!(profile.transport, Transport::Oneme);
        assert_eq!(profile.codec, Codec::Batched);
        assert_eq!(profile.max_token, "token");
        assert_eq!(profile.max_uid, "42");
    }

    #[test]
    fn imports_multistream_urls_and_sets_stream_count() {
        let payload = URL_SAFE_NO_PAD.encode(
            br#"{"name":"multi","mode":"manual","transport":"yandex_multistream","doc_urls":["https://docs.yandex.ru/i/a","https://docs.yandex.ru/i/b"]}"#,
        );
        let profile = import_link(&payload).unwrap().profile;
        assert_eq!(profile.transport, Transport::YandexMultistream);
        assert_eq!(profile.streams, 2);
        assert!(profile.is_connectable());
    }

    #[test]
    fn legacy_payload_without_transport_stays_oneme_free_yandex() {
        let profile = import_link(SAMPLE).unwrap().profile;
        assert_eq!(profile.transport, Transport::Yandex);
        assert_eq!(profile.codec, Codec::Legacy);
        assert_eq!(profile.streams, 1);
    }

    #[test]
    fn key_oneme_payload_without_credentials_is_rejected() {
        let payload = URL_SAFE_NO_PAD.encode(
            br#"{"name":"remote","mode":"key","transport":"oneme","control_url":"https://control.example.com","key_token":"t"}"#,
        );
        assert!(import_link(&payload).is_err());

        let with_credentials = URL_SAFE_NO_PAD.encode(
            br#"{"name":"remote","mode":"key","transport":"oneme","control_url":"https://control.example.com","key_token":"t","max_token":"max-t","max_uid":7}"#,
        );
        let profile = import_link(&with_credentials).unwrap().profile;
        assert_eq!(profile.max_token, "max-t");
        assert_eq!(profile.max_uid, "7");
        assert!(profile.is_connectable());
    }

    #[test]
    fn a_link_can_carry_the_global_dns() {
        let payload = URL_SAFE_NO_PAD.encode(
            br#"{"name":"dns","mode":"manual","doc_url":"https://docs.yandex.ru/i/a","dns":"1.1.1.1"}"#,
        );
        let imported = import_link(&payload).unwrap();
        assert_eq!(imported.dns.as_deref(), Some("1.1.1.1"));
        // Older links spelled the same key `dns_upstream`; both are accepted.
        let legacy = URL_SAFE_NO_PAD.encode(
            br#"{"name":"dns","mode":"manual","doc_url":"https://docs.yandex.ru/i/a","dns_upstream":"9.9.9.9"}"#,
        );
        assert_eq!(
            import_link(&legacy).unwrap().dns.as_deref(),
            Some("9.9.9.9")
        );
        // A link that says nothing leaves the global alone.
        assert_eq!(import_link(SAMPLE).unwrap().dns, None);
    }

    #[test]
    fn rejects_unknown_transport_and_codec() {
        let transport = URL_SAFE_NO_PAD.encode(
            br#"{"name":"x","mode":"manual","transport":"carrier-pigeon","doc_url":"https://d"}"#,
        );
        assert!(import_link(&transport).is_err());

        let codec = URL_SAFE_NO_PAD
            .encode(br#"{"name":"x","mode":"manual","codec":"zstd","doc_url":"https://d"}"#);
        assert!(import_link(&codec).is_err());
    }

    #[test]
    fn rejects_multistream_payload_with_a_single_url() {
        let payload = URL_SAFE_NO_PAD.encode(
            br#"{"name":"multi","mode":"manual","transport":"yandex_multistream","doc_urls":["https://docs.yandex.ru/i/a"]}"#,
        );
        assert!(import_link(&payload).is_err());
    }
}
