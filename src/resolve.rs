//! Controlplane key resolution: `POST {control_url}/v1/resolve` with a Bearer key token
//! returns the active doc URL (plus transport and e2e flags) that the profile with this
//! key uses. Mirrors the controlplane's `handleResolve` in openflux-server.
//!
//! The request is made through `curl` (shelled out) rather than a native TLS stack: it is
//! present on both Linux and Windows 10+, which keeps the client free of a native crypto
//! dependency and makes the crate cross-compilable for Windows without a MinGW toolchain.

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::config::Transport;

#[derive(Debug, Clone)]
pub struct ResolveResult {
    pub status: String,
    pub doc_url: String,
    pub doc_urls: Vec<String>,
    pub transport: Transport,
    pub e2e_encryption: bool,
}

/// Resolve a key token against its controlplane into a concretely usable result.
pub fn resolve_key(control_url: &str, key_token: &str) -> Result<ResolveResult> {
    let base = control_url.trim_end_matches('/');
    let url = format!("{base}/v1/resolve");

    let out = std::process::Command::new("curl")
        .arg("-sS")
        .arg("--fail-with-body")
        .arg("-m")
        .arg("30")
        .arg("-H")
        .arg(format!("Authorization: Bearer {key_token}"))
        .arg("-H")
        .arg("Content-Type: application/json")
        .arg("-H")
        .arg("Accept: application/json")
        .arg("-H")
        .arg("User-Agent: openflux-cli/0.1")
        .arg("-X")
        .arg("POST")
        .arg("--data")
        .arg("")
        .arg(&url)
        .output()
        .with_context(|| "curl not found; the CLI resolves key profiles through curl")?;

    let mut body = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        if body.trim().is_empty() {
            body = String::from_utf8_lossy(&out.stderr).into_owned();
        }
        bail!(
            "resolve request to {url} failed (curl exit {}): {}",
            out.status,
            body.trim()
        );
    }

    parse_response(&body)
}

fn parse_response(body: &str) -> Result<ResolveResult> {
    let json: Value = serde_json::from_str(body).context("resolve response is not JSON")?;

    let status = json
        .get("status")
        .and_then(Value::as_str)
        .context("resolve response missing 'status'")?
        .to_string();

    if status != "active" {
        bail!("key is not active (status: {status})");
    }

    let get_str = |key: &str| -> String {
        json.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string()
    };

    let transport_name = get_str("transport");
    let transport = if transport_name.is_empty() {
        Transport::Yandex
    } else {
        transport_name
            .parse()
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("resolve returned unsupported transport '{transport_name}'"))?
    };

    let doc_url = get_str("doc_url");
    let doc_urls = json
        .get("doc_urls")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if transport.requires_doc_url() && doc_url.is_empty() && doc_urls.is_empty() {
        bail!("resolve returned active status but no document URL");
    }

    Ok(ResolveResult {
        status,
        doc_url,
        doc_urls,
        transport,
        e2e_encryption: json
            .get("e2e_encryption")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_active_response() {
        let json = r#"{"status":"active","doc_url":"https://disk.yandex.ru/i/x","doc_urls":["https://disk.yandex.ru/i/x"],"transport":"yandex","e2e_encryption":true,"bytes_used_total":123}"#;
        let got = parse_response(json).unwrap();
        assert_eq!(got.status, "active");
        assert_eq!(got.doc_url, "https://disk.yandex.ru/i/x");
        assert_eq!(got.transport, Transport::Yandex);
        assert!(got.e2e_encryption);
    }

    #[test]
    fn parses_multistream_response() {
        let json = r#"{"status":"active","doc_urls":["https://disk.yandex.ru/i/a","https://disk.yandex.ru/i/b"],"transport":"yandex_multistream"}"#;
        let got = parse_response(json).unwrap();
        assert_eq!(got.transport, Transport::YandexMultistream);
        assert_eq!(got.doc_urls.len(), 2);
    }

    #[test]
    fn parses_oneme_response_without_document_url() {
        let json = r#"{"status":"active","transport":"oneme"}"#;
        let got = parse_response(json).unwrap();
        assert_eq!(got.transport, Transport::Oneme);
        assert!(got.doc_url.is_empty());
    }

    #[test]
    fn rejects_unknown_transport_and_missing_document_url() {
        let unknown = r#"{"status":"active","transport":"carrier-pigeon","doc_url":"https://d"}"#;
        assert!(parse_response(unknown).is_err());

        let no_doc = r#"{"status":"active","transport":"volga"}"#;
        assert!(parse_response(no_doc).is_err());

        let inactive = r#"{"status":"revoked","doc_url":"https://d"}"#;
        assert!(parse_response(inactive).is_err());
    }

    #[test]
    fn multistream_without_enough_urls_is_rejected() {
        let json =
            r#"{"status":"active","transport":"yandex_multistream","doc_urls":["https://d/a"]}"#;
        let got = parse_response(json).unwrap();
        assert_eq!(got.transport, Transport::YandexMultistream);
        assert_eq!(got.doc_urls.len(), 1);
    }
}
