//! `DCLOGIN:` scheme parser.
//!
//! See <https://github.com/deltachat/interface/blob/master/uri-schemes.md#DCLOGIN>.
//!
//! This is split out from the rest of [`crate::chatmail`] purely to keep
//! the per-scheme test surface small — every chatmail QR scheme has its
//! own quirks and we want each to be unit-testable in isolation.

use percent_encoding::percent_decode_str;
use std::collections::HashMap;

use crate::error::{QrError, Result};
use crate::payload::{DcCertificateChecks, DcLoginOptions, DcLoginSecurity, QrPayload};

/// Scheme prefix. Note that callers accept both `dclogin:` (one colon)
/// and `dclogin://` (double colon) — chatmail@core normalises them by
/// string substitution before parsing.
pub const DCLOGIN_SCHEME: &str = "DCLOGIN:";

/// Decode a `DCLOGIN:` payload.
pub fn decode(raw: &str) -> Result<QrPayload> {
    decode_with_limits(raw, &crate::error::ParseLimits::default())
}

/// Like [`decode`] but with caller-supplied [`ParseLimits`]. Every
/// decoded value is bounded by `limits.max_query_value_bytes`, the
/// total number of query pairs is bounded by `limits.max_query_pairs`,
/// and the original `raw` must already satisfy `limits.max_raw_bytes`.
pub fn decode_with_limits(raw: &str, limits: &crate::error::ParseLimits) -> Result<QrPayload> {
    // Normalise `dclogin://…` → `dclogin:…` so `url::Url::parse` accepts
    // the input. The trailing slash and any path segments are preserved
    // by virtue of being part of the same string.
    let normalized = raw.replacen("://", ":", 1);

    // Validate the scheme by trying to parse the URL. We deliberately
    // ignore the `InvalidIpv6Address` error path: addresses with IPv6
    // literals (e.g. `test@[2001:db8::1]`) are legal in dclogin but the
    // `url` crate rejects them. The query-string parser below has the
    // same blind spot, so we fall back to a manual scan in that case.
    let url = url::Url::parse(&normalized).ok();
    if let Some(u) = &url
        && !u.scheme().eq_ignore_ascii_case("dclogin")
    {
        return Err(QrError::Malformed {
            scheme: "DCLOGIN",
            reason: format!("unexpected scheme {:?}", u.scheme()),
        });
    }

    // The address lives between the scheme and the first `?` query
    // separator (or `/` path segment). chatmail@core reads it the same
    // way — see `src/qr/dclogin_scheme.rs::decode_login`.
    let payload = normalized
        .get(crate::dclogin_scheme::DCLOGIN_SCHEME.len()..)
        .ok_or_else(|| QrError::Malformed {
            scheme: "DCLOGIN",
            reason: "missing payload".into(),
        })?;
    let addr_raw = payload.split(['?', '/']).next().unwrap_or("");
    if addr_raw.is_empty() {
        return Err(QrError::Malformed {
            scheme: "DCLOGIN",
            reason: "address is empty".into(),
        });
    }
    let addr = percent_decode_str(addr_raw)
        .decode_utf8()
        .map_err(|e| QrError::NotUtf8(format!("address: {e}")))?
        .into_owned();

    // Collect query parameters into a HashMap; this makes the parser
    // independent of query-string ordering, which differs between QR
    // generator implementations.
    let params: HashMap<String, String> = match url.as_ref() {
        Some(u) => u
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect(),
        None => parse_query_manual(&normalized),
    };
    enforce_field_limits("DCLOGIN", &params, limits)?;
    if params.is_empty() {
        return Err(QrError::Malformed {
            scheme: "DCLOGIN",
            reason: "missing query parameters".into(),
        });
    }

    let version: u32 = match params.get("v") {
        Some(v) => v
            .parse()
            .map_err(|_| QrError::UnsupportedVersion(format!("DCLOGIN v={v}")))?,
        None => {
            return Err(QrError::Malformed {
                scheme: "DCLOGIN",
                reason: "missing required parameter v".into(),
            });
        }
    };
    if version != 1 {
        // We don't know how to encode v2+; surface as
        // `UnsupportedVersion` so the UI can prompt the user to upgrade.
        let options = DcLoginOptions {
            mail_pw: params.get("p").cloned().unwrap_or_default(),
            imap_host: params.get("ih").cloned(),
            imap_port: params.get("ip").and_then(|s| s.parse().ok()),
            imap_username: params.get("iu").cloned(),
            imap_password: params.get("ipw").cloned(),
            imap_security: parse_security(params.get("is"))?,
            smtp_host: params.get("sh").cloned(),
            smtp_port: params.get("sp").and_then(|s| s.parse().ok()),
            smtp_username: params.get("su").cloned(),
            smtp_password: params.get("spw").cloned(),
            smtp_security: parse_security(params.get("ss"))?,
            certificate_checks: parse_cert_checks(params.get("ic"))?,
        };
        return Ok(QrPayload::DcLogin {
            address: addr,
            options,
        });
    }

    let mail_pw = params.get("p").cloned().ok_or_else(|| QrError::Malformed {
        scheme: "DCLOGIN",
        reason: "missing required parameter p (password)".into(),
    })?;

    Ok(QrPayload::DcLogin {
        address: addr,
        options: DcLoginOptions {
            mail_pw,
            imap_host: params.get("ih").cloned(),
            imap_port: params.get("ip").and_then(|s| s.parse().ok()),
            imap_username: params.get("iu").cloned(),
            imap_password: params.get("ipw").cloned(),
            imap_security: parse_security(params.get("is"))?,
            smtp_host: params.get("sh").cloned(),
            smtp_port: params.get("sp").and_then(|s| s.parse().ok()),
            smtp_username: params.get("su").cloned(),
            smtp_password: params.get("spw").cloned(),
            smtp_security: parse_security(params.get("ss"))?,
            certificate_checks: parse_cert_checks(params.get("ic"))?,
        },
    })
}

fn enforce_field_limits(
    scheme: &'static str,
    params: &HashMap<String, String>,
    limits: &crate::error::ParseLimits,
) -> Result<()> {
    if params.len() > limits.max_query_pairs {
        return Err(QrError::Malformed {
            scheme,
            reason: format!(
                "query has {} pairs (max {})",
                params.len(),
                limits.max_query_pairs
            ),
        });
    }
    for (key, value) in params {
        if value.len() > limits.max_query_value_bytes {
            return Err(QrError::FieldTooLarge {
                scheme,
                field: key.to_string(),
                limit: limits.max_query_value_bytes,
            });
        }
    }
    Ok(())
}

fn parse_security(raw: Option<&String>) -> Result<Option<DcLoginSecurity>> {
    Ok(match raw.map(|s| s.as_str()) {
        Some("ssl") => Some(DcLoginSecurity::Ssl),
        Some("starttls") => Some(DcLoginSecurity::Starttls),
        Some("plain") => Some(DcLoginSecurity::Plain),
        Some("default") => Some(DcLoginSecurity::Default),
        Some(other) => {
            return Err(QrError::Malformed {
                scheme: "DCLOGIN",
                reason: format!("unknown security level: {other:?}"),
            });
        }
        None => None,
    })
}

/// Manual `key=value&key=value` query-string parser used as a fallback
/// when the input is rejected by `url::Url::parse` (e.g. addresses with
/// IPv6 literals).
fn parse_query_manual(raw: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(idx) = raw.find('?') else {
        return out;
    };
    let body = &raw[idx + 1..];
    for pair in body.split('&').filter(|s| !s.is_empty()) {
        if let Some((k, v)) = pair.split_once('=') {
            out.insert(k.to_string(), v.to_string());
        } else {
            out.insert(pair.to_string(), String::new());
        }
    }
    out
}

fn parse_cert_checks(raw: Option<&String>) -> Result<Option<DcCertificateChecks>> {
    Ok(match raw.map(|s| s.as_str()) {
        Some("0") => Some(DcCertificateChecks::Automatic),
        Some("1") => Some(DcCertificateChecks::Strict),
        // chatmail@core has two "accept invalid" variants (codes 2 and
        // 3); they only differ in whether STARTTLS-accepted certs are
        // re-validated. We collapse them to one.
        Some("2") | Some("3") => Some(DcCertificateChecks::AcceptInvalid),
        Some(other) => {
            return Err(QrError::Malformed {
                scheme: "DCLOGIN",
                reason: format!("unknown certificate-check level: {other:?}"),
            });
        }
        None => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_v1() {
        let payload = decode("dclogin://email@host.tld?p=123&v=1").unwrap();
        match payload {
            QrPayload::DcLogin { address, options } => {
                assert_eq!(address, "email@host.tld");
                assert_eq!(options.mail_pw, "123");
                assert!(options.imap_host.is_none());
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn accepts_single_colon_form() {
        let payload = decode("dclogin:email@host.tld?p=123&v=1").unwrap();
        assert!(matches!(payload, QrPayload::DcLogin { .. }));
    }

    #[test]
    fn trailing_path_segments_are_ignored() {
        let payload = decode("dclogin://email@host.tld/ignored/path?p=abc&v=1").unwrap();
        match payload {
            QrPayload::DcLogin { address, options } => {
                assert_eq!(address, "email@host.tld");
                assert_eq!(options.mail_pw, "abc");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn missing_version_is_malformed() {
        assert!(decode("dclogin:email@host.tld?p=123").is_err());
    }

    #[test]
    fn unknown_version_is_unsupported() {
        let payload = decode("dclogin:email@host.tld?p=abc&v=2").unwrap();
        match payload {
            QrPayload::DcLogin { options, .. } => {
                assert_eq!(options.mail_pw, "abc");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn all_advanced_options() {
        let raw = "dclogin:email@host.tld?p=secret&v=1\
                   &ih=imap.host.tld&ip=4000&iu=max&ipw=87654&is=ssl&ic=1\
                   &sh=mail.host.tld&sp=3000&su=max@host.tld&spw=3242HS&ss=plain";
        let payload = decode(raw).unwrap();
        match payload {
            QrPayload::DcLogin { address, options } => {
                assert_eq!(address, "email@host.tld");
                assert_eq!(options.mail_pw, "secret");
                assert_eq!(options.imap_host.as_deref(), Some("imap.host.tld"));
                assert_eq!(options.imap_port, Some(4000));
                assert_eq!(options.imap_username.as_deref(), Some("max"));
                assert_eq!(options.imap_password.as_deref(), Some("87654"));
                assert_eq!(options.imap_security, Some(DcLoginSecurity::Ssl));
                assert_eq!(
                    options.certificate_checks,
                    Some(DcCertificateChecks::Strict)
                );
                assert_eq!(options.smtp_host.as_deref(), Some("mail.host.tld"));
                assert_eq!(options.smtp_port, Some(3000));
                assert_eq!(options.smtp_username.as_deref(), Some("max@host.tld"));
                assert_eq!(options.smtp_password.as_deref(), Some("3242HS"));
                assert_eq!(options.smtp_security, Some(DcLoginSecurity::Plain));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn uri_encoded_address_is_decoded() {
        let payload = decode("dclogin://test%40example.com?p=123&v=1").unwrap();
        match payload {
            QrPayload::DcLogin { address, .. } => {
                assert_eq!(address, "test@example.com");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn ipv6_literal_in_address_is_preserved() {
        // `dclogin://test@[2001:db8::1]?…` — IPv6 literal inside an
        // RFC 5322 address. `url::Url::parse` rejects this (it can't
        // tell that the brackets are inside the local-part of the
        // email address and not an IPv6 host), so we exercise the
        // manual fallback path.
        let payload =
            decode("dclogin://test@%5B2001%3Adb8%3A85a3%3A%3A8a2e%3A370%3A7334%5D?p=123&v=1")
                .unwrap();
        match payload {
            QrPayload::DcLogin { address, .. } => {
                assert_eq!(address, "test@[2001:db8:85a3::8a2e:370:7334]");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_security_level() {
        assert!(decode("dclogin:email@host.tld?p=1&v=1&is=quantum").is_err());
    }

    #[test]
    fn rejects_unknown_cert_check_level() {
        assert!(decode("dclogin:email@host.tld?p=1&v=1&ic=99").is_err());
    }
}
