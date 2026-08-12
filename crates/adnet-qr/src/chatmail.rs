//! Chatmail-compatible QR payload parsers.
//!
//! We implement (and unit-test) every URI scheme that
//! `chatmail@core::qr::check_qr` recognises. The output is the typed
//! [`crate::payload::QrPayload`] enum, not chatmail's `Qr` enum
//! (which is deeply tied to the chatmail `Context`).
//!
//! Schemes covered here:
//!
//! - `DCACCOUNT:` — ask the user to register an account on `domain`
//! - `DCLOGIN:`   — see [`crate::dclogin_scheme`] (split out for testing)
//! - `DCBACKUP`   — iroh-based backup transfer (JSON-encoded `NodeAddr`)
//! - `OPENPGP4FPR:` / `https://i.delta.chat[/]#` — SecureJoin invite
//! - `mailto:`, `MATMSG:`, `BEGIN:VCARD`, `SMTP:` — address schemes
//! - `https://t.me/socks?…` — Telegram-style SOCKS5 proxy
//! - `ss://`     — Shadowsocks proxy URL

use percent_encoding::percent_decode_str;
use serde::Deserialize;

use crate::error::{QrError, Result};
use crate::payload::{DcCertificateChecks, OpenPgpGroup, QrPayload};

/// `OPENPGP4FPR:` scheme prefix (yes, it's uppercase in chatmail@core).
pub const OPENPGP4FPR_SCHEME: &str = "OPENPGP4FPR:";
/// `https://i.delta.chat/` — web form of the OPENPGP4FPR scheme.
pub const IDELTACHAT_SCHEME: &str = "https://i.delta.chat/";
/// `https://i.delta.chat#` — iOS occasionally drops the trailing slash.
pub const IDELTACHAT_NOSLASH_SCHEME: &str = "https://i.delta.chat#";
/// `DCACCOUNT:` scheme prefix.
pub const DCACCOUNT_SCHEME: &str = "DCACCOUNT:";
/// `DCBACKUP` scheme prefix (no trailing colon — the version is part of
/// the payload).
pub const DCBACKUP_SCHEME_PREFIX: &str = "DCBACKUP";
/// Highest `DCBACKUP` version this build understands.
pub const DCBACKUP_VERSION: i32 = 5;
/// `mailto:` scheme prefix.
pub const MAILTO_SCHEME: &str = "mailto:";
/// `MATMSG:` scheme prefix.
pub const MATMSG_SCHEME: &str = "MATMSG:";
/// `BEGIN:VCARD` literal.
pub const VCARD_SCHEME: &str = "BEGIN:VCARD";
/// `SMTP:` scheme prefix.
pub const SMTP_SCHEME: &str = "SMTP:";
/// `https://t.me/socks` — Telegram proxy URL prefix.
pub const TG_SOCKS_SCHEME: &str = "https://t.me/socks";
/// `https://` and `http://` prefixes used by the generic URL fallback.
pub const HTTPS_SCHEME: &str = "https://";
pub const HTTP_SCHEME: &str = "http://";
/// `ss://` Shadowsocks proxy prefix.
pub const SHADOWSOCKS_SCHEME: &str = "ss://";
/// `socks5://` URL prefix.
pub const SOCKS5_SCHEME: &str = "socks5://";

/// Default port for SOCKS5 proxies when none is specified.
pub const DEFAULT_SOCKS_PORT: u16 = 1080;

/// Try the address-bearing schemes (`mailto:`, `MATMSG:`, `BEGIN:VCARD`,
/// `SMTP:`). Returns `None` if `raw` doesn't start with any of those
/// prefixes.
///
/// Bounds the decoded subject / body length with
/// `limits.max_query_value_bytes` so a hostile QR cannot exhaust
/// memory through percent-decoded expansion.
pub fn decode_address_schemes(
    raw: &str,
    limits: &crate::error::ParseLimits,
) -> Result<Option<QrPayload>> {
    let upper = raw.to_ascii_uppercase();
    if upper.starts_with(MAILTO_SCHEME.to_ascii_uppercase().as_str()) {
        return decode_mailto(raw, limits).map(Some);
    }
    if upper.starts_with(MATMSG_SCHEME.to_ascii_uppercase().as_str()) {
        return decode_matmsg(raw, limits).map(Some);
    }
    if upper.starts_with(VCARD_SCHEME) {
        return decode_vcard(raw, limits).map(Some);
    }
    if upper.starts_with(SMTP_SCHEME.to_ascii_uppercase().as_str()) {
        return decode_smtp(raw, limits).map(Some);
    }
    Ok(None)
}

/// Try the proxy / URL schemes. Returns `None` if `raw` doesn't look
/// like a proxy / URL.
pub fn decode_proxy_url(raw: &str, limits: &crate::error::ParseLimits) -> Option<QrPayload> {
    let upper = raw.to_ascii_uppercase();
    if upper.starts_with(TG_SOCKS_SCHEME) {
        return decode_tg_socks_proxy(raw);
    }
    if raw.starts_with(SHADOWSOCKS_SCHEME) {
        return decode_shadowsocks_proxy(raw, limits);
    }
    if let Ok(url) = url::Url::parse(raw) {
        return match url.scheme() {
            "socks5" => Some(QrPayload::Proxy {
                url: raw.to_string(),
                host: url.host_str().unwrap_or("").to_string(),
                port: url.port().unwrap_or(DEFAULT_SOCKS_PORT),
            }),
            "http" | "https" => {
                // chatmail only treats bare `http://host:port` URLs as
                // proxies; anything with a path or query is a generic
                // URL payload.
                if url.path() != "/" && !url.path().is_empty() || url.query().is_some() {
                    Some(QrPayload::Url {
                        url: raw.to_string(),
                    })
                } else {
                    let port = url
                        .port_or_known_default()
                        .unwrap_or(if url.scheme() == "https" { 443 } else { 80 });
                    Some(QrPayload::Proxy {
                        url: raw.to_string(),
                        host: url.host_str().unwrap_or("").to_string(),
                        port,
                    })
                }
            }
            _ => Some(QrPayload::Url {
                url: raw.to_string(),
            }),
        };
    }
    None
}

fn decode_mailto(raw: &str, limits: &crate::error::ParseLimits) -> Result<QrPayload> {
    let payload = raw
        .strip_prefix(MAILTO_SCHEME)
        .or_else(|| raw.get(MAILTO_SCHEME.len()..))
        .ok_or_else(|| QrError::Malformed {
            scheme: "mailto",
            reason: "missing scheme".into(),
        })?;

    let (addr_part, query) = match payload.split_once('?') {
        Some((a, q)) => (a, q),
        None => (payload, ""),
    };
    if addr_part.len() > limits.max_query_value_bytes {
        return Err(QrError::FieldTooLarge {
            scheme: "mailto",
            field: "address".to_string(),
            limit: limits.max_query_value_bytes,
        });
    }
    let params = parse_query(query);
    if params.len() > limits.max_query_pairs {
        return Err(QrError::Malformed {
            scheme: "mailto",
            reason: format!(
                "query has {} pairs (max {})",
                params.len(),
                limits.max_query_pairs
            ),
        });
    }

    let subject = params.get("subject").map(|s| {
        // chatmail reverses `+` → space, then percent-decodes. We do
        // the same so the body round-trips with whatever scanner the
        // user pointed at the QR.
        let s = s.replace('+', "%20");
        percent_decode_str(&s).decode_utf8_lossy().into_owned()
    });
    let body = params.get("body").map(|s| {
        let s = s.replace('+', "%20");
        percent_decode_str(&s).decode_utf8_lossy().into_owned()
    });
    if let Some(s) = &subject
        && s.len() > limits.max_query_value_bytes
    {
        return Err(QrError::FieldTooLarge {
            scheme: "mailto",
            field: "subject".to_string(),
            limit: limits.max_query_value_bytes,
        });
    }
    if let Some(b) = &body
        && b.len() > limits.max_query_value_bytes
    {
        return Err(QrError::FieldTooLarge {
            scheme: "mailto",
            field: "body".to_string(),
            limit: limits.max_query_value_bytes,
        });
    }

    Ok(QrPayload::Email {
        address: addr_part.to_string(),
        subject,
        body,
    })
}

fn decode_matmsg(raw: &str, limits: &crate::error::ParseLimits) -> Result<QrPayload> {
    // `MATMSG:TO:addr;SUB:subject;BODY:body;` (linebreaks allowed).
    let to_idx = raw.find("TO:").ok_or_else(|| QrError::Malformed {
        scheme: "MATMSG",
        reason: "missing TO: field".into(),
    })?;
    let after_to = &raw[to_idx + 3..];
    let addr = match after_to.find(';') {
        Some(i) => after_to[..i].trim(),
        None => after_to.trim(),
    };
    if addr.len() > limits.max_query_value_bytes {
        return Err(QrError::FieldTooLarge {
            scheme: "MATMSG",
            field: "TO".to_string(),
            limit: limits.max_query_value_bytes,
        });
    }
    let addr = addr.to_string();

    let subject = match find_field(raw, "SUB:") {
        Some(s) => percent_decode_str(&s.replace('+', "%20"))
            .decode_utf8_lossy()
            .into_owned(),
        None => String::new(),
    };
    if subject.len() > limits.max_query_value_bytes {
        return Err(QrError::FieldTooLarge {
            scheme: "MATMSG",
            field: "SUB".to_string(),
            limit: limits.max_query_value_bytes,
        });
    }
    let body = match find_field(raw, "BODY:") {
        Some(s) => percent_decode_str(&s.replace('+', "%20"))
            .decode_utf8_lossy()
            .into_owned(),
        None => String::new(),
    };
    if body.len() > limits.max_query_value_bytes {
        return Err(QrError::FieldTooLarge {
            scheme: "MATMSG",
            field: "BODY".to_string(),
            limit: limits.max_query_value_bytes,
        });
    }
    let subject_opt = if subject.is_empty() {
        None
    } else {
        Some(subject)
    };
    let body_opt = if body.is_empty() { None } else { Some(body) };
    Ok(QrPayload::Matmsg {
        address: addr,
        subject: subject_opt,
        body: body_opt,
    })
}

fn find_field(raw: &str, prefix: &str) -> Option<String> {
    let idx = raw.find(prefix)?;
    let after = &raw[idx + prefix.len()..];
    let end = after.find(';').unwrap_or(after.len());
    Some(after[..end].to_string())
}

fn decode_vcard(raw: &str, limits: &crate::error::ParseLimits) -> Result<QrPayload> {
    // Chatmail uses a regex for this but a hand-rolled scan keeps the
    // dependency footprint small.
    let name = raw
        .lines()
        .find_map(|line| {
            line.strip_prefix("N:").map(|rest| {
                let parts: Vec<&str> = rest.split(';').collect();
                let last = parts.first().copied().unwrap_or("").trim();
                let first = parts.get(1).copied().unwrap_or("").trim();
                if first.is_empty() && last.is_empty() {
                    String::new()
                } else {
                    format!("{first} {last}").trim().to_string()
                }
            })
        })
        .unwrap_or_default();
    if name.len() > limits.max_query_value_bytes {
        return Err(QrError::FieldTooLarge {
            scheme: "VCARD",
            field: "N".to_string(),
            limit: limits.max_query_value_bytes,
        });
    }

    let addr = raw
        .lines()
        .find_map(|line| {
            // `EMAIL:foo@bar` or `EMAIL;TYPE=INTERNET:foo@bar`
            let after = if let Some(rest) = line.strip_prefix("EMAIL:") {
                rest
            } else if line.starts_with("EMAIL") {
                line.split_once(':').map(|x| x.1)?
            } else {
                return None;
            };
            let end = after.find(';').unwrap_or(after.len());
            Some(after[..end].trim().to_string())
        })
        .ok_or_else(|| QrError::Malformed {
            scheme: "VCARD",
            reason: "missing EMAIL field".into(),
        })?;
    if addr.len() > limits.max_query_value_bytes {
        return Err(QrError::FieldTooLarge {
            scheme: "VCARD",
            field: "EMAIL".to_string(),
            limit: limits.max_query_value_bytes,
        });
    }

    Ok(QrPayload::Vcard {
        name,
        address: addr,
    })
}

fn decode_smtp(raw: &str, limits: &crate::error::ParseLimits) -> Result<QrPayload> {
    let payload = raw
        .strip_prefix(SMTP_SCHEME)
        .ok_or_else(|| QrError::Malformed {
            scheme: "SMTP",
            reason: "missing scheme".into(),
        })?;
    let addr = payload.split(':').next().unwrap_or("").to_string();
    if addr.is_empty() {
        return Err(QrError::Malformed {
            scheme: "SMTP",
            reason: "missing address".into(),
        });
    }
    if addr.len() > limits.max_query_value_bytes {
        return Err(QrError::FieldTooLarge {
            scheme: "SMTP",
            field: "address".to_string(),
            limit: limits.max_query_value_bytes,
        });
    }
    Ok(QrPayload::Smtp { address: addr })
}

fn decode_tg_socks_proxy(raw: &str) -> Option<QrPayload> {
    let url = url::Url::parse(raw).ok()?;
    let mut host: Option<String> = None;
    let mut port: u16 = DEFAULT_SOCKS_PORT;
    let mut user: Option<String> = None;
    let mut pass: Option<String> = None;
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "server" => host = Some(v.into_owned()),
            "port" => port = v.parse().unwrap_or(DEFAULT_SOCKS_PORT),
            "user" => user = Some(v.into_owned()),
            "pass" => pass = Some(v.into_owned()),
            _ => {}
        }
    }
    let host = host?;
    let mut out = String::from("socks5://");
    if let Some(p) = pass {
        out.push_str(
            &percent_encoding::utf8_percent_encode(
                user.as_deref().unwrap_or(""),
                percent_encoding::NON_ALPHANUMERIC,
            )
            .to_string(),
        );
        out.push(':');
        out.push_str(
            &percent_encoding::utf8_percent_encode(p.as_str(), percent_encoding::NON_ALPHANUMERIC)
                .to_string(),
        );
        out.push('@');
    }
    out.push_str(&host);
    out.push(':');
    out.push_str(&port.to_string());
    Some(QrPayload::Proxy {
        url: out,
        host,
        port,
    })
}

fn decode_shadowsocks_proxy(raw: &str, limits: &crate::error::ParseLimits) -> Option<QrPayload> {
    // We can't parse Shadowsocks URLs ourselves without pulling in the
    // `shadowsocks` crate. We do a minimal decode: strip the prefix,
    // strip the `#tag` fragment, base64-decode what's left, and look
    // for the trailing `host:port`.
    use base64::Engine as _;
    let rest = raw.strip_prefix(SHADOWSOCKS_SCHEME)?;
    let rest = rest.split('#').next().unwrap_or(rest);
    // Shadowsocks SIP002 uses standard base64 (with optional padding).
    if rest.len() > limits.max_shadowsocks_decoded_bytes * 4 / 3 + 16 {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(rest)
        .ok()?;
    if decoded.len() > limits.max_shadowsocks_decoded_bytes {
        return None;
    }
    let decoded = std::str::from_utf8(&decoded).ok()?;
    // Format is `<method>:<password>@<host>:<port>`.
    let suffix = decoded.rsplit('@').next()?;
    let (host_part, port_part) = suffix.rsplit_once(':')?;
    let host = host_part
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let port: u16 = port_part.parse().ok()?;
    Some(QrPayload::Shadowsocks {
        url: raw.to_string(),
        host,
        port,
    })
}

/// `DCACCOUNT:` scheme parser.
pub fn decode_dcaccount(raw: &str) -> Result<QrPayload> {
    let payload = raw
        .strip_prefix(DCACCOUNT_SCHEME)
        .or_else(|| raw.strip_prefix("DCACCOUNT://"))
        .ok_or_else(|| QrError::Malformed {
            scheme: "DCACCOUNT",
            reason: "missing scheme".into(),
        })?;
    let payload = payload.strip_prefix("//").unwrap_or(payload).trim();
    if payload.is_empty() {
        return Err(QrError::Malformed {
            scheme: "DCACCOUNT",
            reason: "empty payload".into(),
        });
    }
    if payload.starts_with('/') {
        return Err(QrError::Malformed {
            scheme: "DCACCOUNT",
            reason: "hostname cannot start with /".into(),
        });
    }
    Ok(QrPayload::DcAccount {
        domain: payload.to_string(),
    })
}

/// `DCBACKUP` scheme parser.
pub fn decode_dcbackup(raw: &str, limits: &crate::error::ParseLimits) -> Result<QrPayload> {
    let version_and_payload =
        raw.strip_prefix(DCBACKUP_SCHEME_PREFIX)
            .ok_or_else(|| QrError::Malformed {
                scheme: "DCBACKUP",
                reason: "missing scheme".into(),
            })?;
    let (version, payload) =
        version_and_payload
            .split_once(':')
            .ok_or_else(|| QrError::Malformed {
                scheme: "DCBACKUP",
                reason: "missing version separator".into(),
            })?;
    let version: i32 = version.parse().map_err(|_| QrError::Malformed {
        scheme: "DCBACKUP",
        reason: format!("invalid version {version:?}"),
    })?;
    if version > DCBACKUP_VERSION {
        return Ok(QrPayload::BackupTooNew { version });
    }
    let (auth_token, node_addr_json) =
        payload.split_once('&').ok_or_else(|| QrError::Malformed {
            scheme: "DCBACKUP",
            reason: "missing auth-token / node-addr separator".into(),
        })?;
    if auth_token.len() > limits.max_query_value_bytes {
        return Err(QrError::FieldTooLarge {
            scheme: "DCBACKUP",
            field: "auth_token".to_string(),
            limit: limits.max_query_value_bytes,
        });
    }
    if node_addr_json.len() > limits.max_query_value_bytes {
        return Err(QrError::FieldTooLarge {
            scheme: "DCBACKUP",
            field: "node_addr_json".to_string(),
            limit: limits.max_query_value_bytes,
        });
    }
    Ok(QrPayload::DcBackup {
        version,
        node_addr_json: node_addr_json.to_string(),
        auth_token: auth_token.to_string(),
    })
}

/// `DCLOGIN:` scheme parser. Convenience wrapper around
/// [`crate::dclogin_scheme::decode`].
pub fn decode_dclogin(raw: &str, limits: &crate::error::ParseLimits) -> Result<QrPayload> {
    crate::dclogin_scheme::decode_with_limits(raw, limits)
}

/// `OPENPGP4FPR:` / `https://i.delta.chat[/]#` parser.
///
/// Returns a [`QrPayload::OpenPgp4Fpr`]. The fingerprint is hex-formatted
/// (40 or 80 chars depending on the key version); the rest of the
/// fields are optional.
pub fn decode_openpgp(
    raw: &str,
    limits: &crate::error::ParseLimits,
) -> Result<crate::payload::OpenPgp4FprFields> {
    let payload = raw
        .strip_prefix(OPENPGP4FPR_SCHEME)
        .ok_or_else(|| QrError::Malformed {
            scheme: "OPENPGP4FPR",
            reason: "missing scheme".into(),
        })?;

    // macOS / iOS occasionally URL-encode the fragment separator.
    let (fingerprint, fragment) = match payload
        .split_once('#')
        .or_else(|| payload.split_once("%23"))
    {
        Some(pair) => pair,
        None => (payload, ""),
    };
    if fingerprint.is_empty() {
        return Err(QrError::Malformed {
            scheme: "OPENPGP4FPR",
            reason: "empty fingerprint".into(),
        });
    }
    if fingerprint.len() > limits.max_fingerprint_bytes {
        return Err(QrError::FieldTooLarge {
            scheme: "OPENPGP4FPR",
            field: "fingerprint".to_string(),
            limit: limits.max_fingerprint_bytes,
        });
    }
    if !fingerprint.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(QrError::Malformed {
            scheme: "OPENPGP4FPR",
            reason: "fingerprint is not hex".into(),
        });
    }

    let params = parse_query(fragment);
    if params.len() > limits.max_query_pairs {
        return Err(QrError::Malformed {
            scheme: "OPENPGP4FPR",
            reason: format!(
                "query has {} pairs (max {})",
                params.len(),
                limits.max_query_pairs
            ),
        });
    }
    for (key, value) in &params {
        if value.len() > limits.max_query_value_bytes {
            return Err(QrError::FieldTooLarge {
                scheme: "OPENPGP4FPR",
                field: key.clone(),
                limit: limits.max_query_value_bytes,
            });
        }
    }
    let address = params.get("a").map(|s| {
        // chatmail reverses `+` → space, then percent-decodes — match it.
        let s = s.replace('+', "%20");
        percent_decode_str(&s).decode_utf8_lossy().into_owned()
    });
    let name = params.get("n").map(|s| {
        // chatmail reverses `+` → space before percent-decoding.
        let s = s.replace('+', "%20");
        percent_decode_str(&s).decode_utf8_lossy().into_owned()
    });
    let invitenumber = params.get("i").cloned();
    let authcode = params.get("s").cloned();

    let group = if let (Some(grpname), Some(grpid)) = (params.get("g"), params.get("x")) {
        Some(OpenPgpGroup::Group {
            grpname: grpname.clone(),
            grpid: grpid.clone(),
        })
    } else if let (Some(bname), Some(grpid)) = (params.get("b"), params.get("x")) {
        Some(OpenPgpGroup::Broadcast {
            name: bname.clone(),
            grpid: grpid.clone(),
        })
    } else {
        None
    };

    Ok(crate::payload::OpenPgp4FprFields {
        fingerprint: fingerprint.to_string(),
        address,
        name,
        invitenumber,
        authcode,
        group,
    })
}

fn parse_query(query: &str) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        if let Some((k, v)) = pair.split_once('=') {
            out.insert(k.to_string(), v.to_string());
        } else {
            out.insert(pair.to_string(), String::new());
        }
    }
    out
}

/// Wire-format the DCLOGIN `certificate_checks` field. Exposed so the
/// `mail` feature can build an [`adnet_mail::Account`] without going
/// through [`crate::payload::QrPayload`].
pub fn certificate_checks_code(c: DcCertificateChecks) -> &'static str {
    match c {
        DcCertificateChecks::Automatic => "0",
        DcCertificateChecks::Strict => "1",
        DcCertificateChecks::AcceptInvalid => "2",
    }
}

/// Internal helper for tests: parse a JSON value from a DCBACKUP payload
/// without pulling in iroh as a dep.
#[derive(Debug, Deserialize)]
pub struct NodeAddrStub {
    #[serde(default)]
    pub node_id: String,
    #[serde(default)]
    pub relay_url: Option<String>,
    #[serde(default)]
    pub direct_addresses: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_limits() -> crate::error::ParseLimits {
        crate::error::ParseLimits::default()
    }

    fn tiny_limits() -> crate::error::ParseLimits {
        crate::error::ParseLimits {
            max_query_pairs: 4,
            max_query_value_bytes: 16,
            ..default_limits()
        }
    }

    #[test]
    fn dclogin_query_pair_count_is_bounded() {
        let mut raw = String::from("dclogin://a@b.tld?p=secret&v=1");
        for i in 0..10 {
            raw.push_str(&format!("&k{i}=v{i}"));
        }
        let err = crate::dclogin_scheme::decode_with_limits(&raw, &tiny_limits()).unwrap_err();
        // Either a Malformed pair-count error or a FieldTooLarge value
        // error are both acceptable limit-enforcement outcomes; we just
        // require the parser to refuse the input.
        let is_limit = matches!(
            err,
            crate::error::QrError::Malformed { .. } | crate::error::QrError::FieldTooLarge { .. }
        );
        assert!(is_limit, "expected limit error, got {err:?}");
    }

    #[test]
    fn dclogin_query_value_length_is_bounded() {
        let big = "x".repeat(64);
        let raw = format!("dclogin://a@b.tld?p={big}&v=1");
        let err = crate::dclogin_scheme::decode_with_limits(&raw, &tiny_limits()).unwrap_err();
        match &err {
            crate::error::QrError::FieldTooLarge { field, .. } => {
                assert_eq!(field, "p");
            }
            other => panic!("expected FieldTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn openpgp_query_pair_count_is_bounded() {
        let mut raw = String::from("OPENPGP4FPR:ABCDEF");
        for i in 0..10 {
            raw.push_str(&format!("&k{i}=v{i}"));
        }
        let err = decode_openpgp(&raw, &tiny_limits()).unwrap_err();
        assert!(matches!(
            err,
            crate::error::QrError::Malformed { .. } | crate::error::QrError::FieldTooLarge { .. }
        ));
    }

    #[test]
    fn openpgp_fingerprint_length_is_bounded() {
        let raw = format!("OPENPGP4FPR:{}", "F".repeat(1024));
        let limits = crate::error::ParseLimits::default();
        let err = decode_openpgp(&raw, &limits).unwrap_err();
        assert!(matches!(err, crate::error::QrError::FieldTooLarge { .. }));
    }

    #[test]
    fn check_qr_rejects_oversized_raw() {
        let big = "x".repeat(4096);
        let err = crate::scan::check_qr(&big).unwrap_err();
        assert!(matches!(err, crate::error::QrError::ContentTooLarge { .. }));
    }

    #[test]
    fn dcaccount_minimal() {
        let payload = decode_dcaccount("DCACCOUNT:example.org").unwrap();
        assert_eq!(
            payload,
            QrPayload::DcAccount {
                domain: "example.org".into()
            }
        );
    }

    #[test]
    fn dcaccount_double_colon() {
        let payload = decode_dcaccount("DCACCOUNT://example.org/new").unwrap();
        assert_eq!(
            payload,
            QrPayload::DcAccount {
                domain: "example.org/new".into()
            }
        );
    }

    #[test]
    fn dcaccount_empty_payload_is_rejected() {
        assert!(decode_dcaccount("DCACCOUNT:").is_err());
    }

    #[test]
    fn dcaccount_hostname_starting_with_slash_is_rejected() {
        assert!(decode_dcaccount("DCACCOUNT:///etc/passwd").is_err());
    }

    #[test]
    fn dcbackup_v5_round_trip() {
        let raw = "DCBACKUP5:auth-token&{\"node_id\":\"abc\"}";
        let payload = decode_dcbackup(raw, &default_limits()).unwrap();
        match payload {
            QrPayload::DcBackup {
                version,
                auth_token,
                node_addr_json,
            } => {
                assert_eq!(version, 5);
                assert_eq!(auth_token, "auth-token");
                assert!(node_addr_json.contains("\"node_id\""));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn dcbackup_too_new() {
        let payload = decode_dcbackup("DCBACKUP999:foo&bar", &default_limits()).unwrap();
        assert!(matches!(payload, QrPayload::BackupTooNew { version: 999 }));
    }

    #[test]
    fn openpgp4fpr_minimal() {
        let fp = "1234567890ABCDEF1234567890ABCDEF12345678";
        let raw = format!("OPENPGP4FPR:{fp}");
        let parsed = decode_openpgp(&raw, &crate::error::ParseLimits::default()).unwrap();
        assert_eq!(parsed.fingerprint, fp);
        assert!(parsed.address.is_none());
        assert!(parsed.group.is_none());
    }

    #[test]
    fn openpgp4fpr_full_invite() {
        let raw = "OPENPGP4FPR:ABCD#a=alice%40example.com&n=Alice&i=inv&s=auth&g=grpname&x=grpid";
        let parsed = decode_openpgp(raw, &crate::error::ParseLimits::default()).unwrap();
        assert_eq!(parsed.fingerprint, "ABCD");
        assert_eq!(parsed.address.as_deref(), Some("alice@example.com"));
        assert_eq!(parsed.name.as_deref(), Some("Alice"));
        assert_eq!(parsed.invitenumber.as_deref(), Some("inv"));
        assert_eq!(parsed.authcode.as_deref(), Some("auth"));
        match parsed.group {
            Some(OpenPgpGroup::Group { grpname, grpid }) => {
                assert_eq!(grpname, "grpname");
                assert_eq!(grpid, "grpid");
            }
            other => panic!("wrong group: {other:?}"),
        }
    }

    #[test]
    fn openpgp4fpr_broadcast_invite() {
        let raw = "OPENPGP4FPR:ABCD#a=alice%40example.com&b=Channel&x=channel-id";
        let parsed = decode_openpgp(raw, &crate::error::ParseLimits::default()).unwrap();
        match parsed.group {
            Some(OpenPgpGroup::Broadcast { name, grpid }) => {
                assert_eq!(name, "Channel");
                assert_eq!(grpid, "channel-id");
            }
            other => panic!("wrong group: {other:?}"),
        }
    }

    #[test]
    fn openpgp4fpr_percent_encoded_fragment() {
        let raw = "OPENPGP4FPR:ABCD%23a=alice%40example.com";
        let parsed = decode_openpgp(raw, &crate::error::ParseLimits::default()).unwrap();
        assert_eq!(parsed.fingerprint, "ABCD");
        assert_eq!(parsed.address.as_deref(), Some("alice@example.com"));
    }

    #[test]
    fn mailto_with_subject_and_body() {
        let raw = "mailto:alice@example.com?subject=Hi&body=Hello%20there";
        let payload = decode_mailto(raw, &default_limits()).unwrap();
        assert_eq!(
            payload,
            QrPayload::Email {
                address: "alice@example.com".into(),
                subject: Some("Hi".into()),
                body: Some("Hello there".into()),
            }
        );
    }

    #[test]
    fn matmsg_parses_to_subject_body() {
        let raw = "MATMSG:TO:alice@example.com;SUB:Hi;BODY:Hello;";
        let payload = decode_matmsg(raw, &default_limits()).unwrap();
        assert_eq!(
            payload,
            QrPayload::Matmsg {
                address: "alice@example.com".into(),
                subject: Some("Hi".into()),
                body: Some("Hello".into()),
            }
        );
    }

    #[test]
    fn vcard_parses_name_and_email() {
        let raw = "BEGIN:VCARD\nN:Doe;Alice;;;\nEMAIL:alice@example.com\nEND:VCARD";
        let payload = decode_vcard(raw, &default_limits()).unwrap();
        assert_eq!(
            payload,
            QrPayload::Vcard {
                name: "Alice Doe".into(),
                address: "alice@example.com".into(),
            }
        );
    }

    #[test]
    fn smtp_extracts_address() {
        let payload =
            decode_smtp("SMTP:alice@example.com:subject:body", &default_limits()).unwrap();
        assert_eq!(
            payload,
            QrPayload::Smtp {
                address: "alice@example.com".into()
            }
        );
    }

    #[test]
    fn tg_socks_proxy_parses() {
        let raw = "https://t.me/socks?server=foo.example.com&port=9999";
        let payload = decode_tg_socks_proxy(raw).unwrap();
        match payload {
            QrPayload::Proxy { url, host, port } => {
                assert_eq!(host, "foo.example.com");
                assert_eq!(port, 9999);
                assert!(url.starts_with("socks5://"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn tg_socks_proxy_defaults_port() {
        let raw = "https://t.me/socks?server=foo.example.com";
        let payload = decode_tg_socks_proxy(raw).unwrap();
        match payload {
            QrPayload::Proxy { port, .. } => assert_eq!(port, DEFAULT_SOCKS_PORT),
            _ => panic!(),
        }
    }

    #[test]
    fn shadowsocks_minimal_recovery() {
        let raw = "ss://YWVzLTI1Ni1nY206c2VjcmV0QDEyNy4wLjAuMTo4MDgw#tag";
        let payload = decode_shadowsocks_proxy(raw, &crate::error::ParseLimits::default()).unwrap();
        match payload {
            QrPayload::Shadowsocks { host, port, .. } => {
                assert_eq!(host, "127.0.0.1");
                assert_eq!(port, 8080);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn decode_proxy_url_recognises_socks5() {
        let payload = decode_proxy_url(
            "socks5://user:pass@proxy.example.com:1080",
            &default_limits(),
        )
        .unwrap();
        match payload {
            QrPayload::Proxy { host, port, .. } => {
                assert_eq!(host, "proxy.example.com");
                assert_eq!(port, 1080);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn decode_proxy_url_recognises_bare_https_as_proxy() {
        let payload = decode_proxy_url("https://proxy.example.com:443", &default_limits()).unwrap();
        match payload {
            QrPayload::Proxy { host, port, .. } => {
                assert_eq!(host, "proxy.example.com");
                assert_eq!(port, 443);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn decode_proxy_url_recognises_https_with_path_as_url() {
        let payload = decode_proxy_url("https://example.com/foo", &default_limits()).unwrap();
        assert!(matches!(payload, QrPayload::Url { .. }));
    }

    #[test]
    fn decode_address_schemes_dispatches() {
        let cases = [
            ("mailto:a@b.c", "mailto"),
            ("MATMSG:TO:a@b.c;", "matmsg"),
            ("BEGIN:VCARD\nN:;A;;;\nEMAIL:a@b.c\nEND:VCARD", "vcard"),
            ("SMTP:a@b.c:", "smtp"),
        ];
        for (raw, expected) in cases {
            let parsed = decode_address_schemes(raw, &default_limits()).unwrap();
            assert_eq!(parsed.as_ref().unwrap().tag(), expected, "raw = {raw}");
        }
    }

    #[test]
    fn decode_address_schemes_returns_none_for_unrelated_input() {
        let parsed = decode_address_schemes("just text", &default_limits()).unwrap();
        assert!(parsed.is_none());
    }

    #[test]
    fn mailto_subject_size_is_bounded() {
        let huge = "A".repeat(2048);
        let raw = format!("mailto:alice@example.com?subject={huge}");
        let err = decode_mailto(&raw, &tiny_limits()).unwrap_err();
        assert!(matches!(err, QrError::FieldTooLarge { .. }));
    }

    #[test]
    fn matmsg_body_size_is_bounded() {
        let huge = "A".repeat(2048);
        let raw = format!("MATMSG:TO:alice@example.com;BODY:{huge};");
        let err = decode_matmsg(&raw, &tiny_limits()).unwrap_err();
        assert!(matches!(err, QrError::FieldTooLarge { .. }));
    }
}
