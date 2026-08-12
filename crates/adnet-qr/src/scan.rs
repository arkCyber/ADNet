//! Top-level QR scanner entry point.
//!
//! [`check_qr`] takes the raw text produced by a QR scanner and returns
//! a [`QrPayload`]. Unknown payloads are not an error — they come back
//! as [`QrPayload::Text`] / [`QrPayload::Url`] so the UI can decide what
//! to do with them (open the browser, copy to clipboard, ignore, ...).
//!
//! This mirrors `chatmail@core::qr::check_qr` but with three ADNet
//! additions:
//!  1. We classify `adnet-…` URLs first (peer / blob / signed / token),
//!     so a chatmail-only client can't accidentally misread them.
//!  2. We refuse unknown schemes that look chatmail-like (any scheme
//!     starting with `DC` we don't recognise), so version skew yields
//!     a typed `UnsupportedVersion` error instead of a silent `Text`.
//!  3. We never panic on input — every step is total and returns a
//!     payload or a [`QrError`].

use crate::adnet;
use crate::chatmail::{decode_dcaccount, decode_dcbackup, decode_dclogin, decode_openpgp};
use crate::error::{ParseLimits, QrError, Result};
use crate::payload::{DcLoginOptions, OpenPgpGroup, QrPayload};

/// Scan a raw QR string and classify it, applying the default
/// [`ParseLimits`].
///
/// # Errors
///
/// Returns [`QrError::Malformed`] when the input matches a known scheme
/// but the payload is broken (bad base64, missing required parameter,
/// etc.). Unknown schemes return `Ok(QrPayload::Text { .. })` rather
/// than an error — the UI can still decide to ignore or surface them.
pub fn check_qr(raw: &str) -> Result<QrPayload> {
    check_qr_with_limits(raw, &ParseLimits::default())
}

/// Like [`check_qr`] but with explicit [`ParseLimits`]. Callers wiring
/// the crate into an untrusted surface (e.g. a public kiosk) should
/// tighten the defaults; trusted scanners can leave them alone.
pub fn check_qr_with_limits(raw: &str, limits: &ParseLimits) -> Result<QrPayload> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(QrError::Malformed {
            scheme: "(empty)",
            reason: "scanned text is empty".into(),
        });
    }
    if trimmed.len() > limits.max_raw_bytes {
        return Err(QrError::ContentTooLarge {
            actual: trimmed.len(),
            limit: limits.max_raw_bytes,
        });
    }

    // ADNet-native URIs get first crack at the input — their prefixes
    // are unique and never collide with chatmail schemes.
    #[cfg(feature = "adnet-types")]
    if let Some(payload) = adnet::try_parse_adnet_ticket(trimmed)? {
        return Ok(payload);
    }
    #[cfg(feature = "pairing")]
    if let Some(payload) = adnet::try_parse_adnet_pairing(trimmed)? {
        return Ok(payload);
    }
    #[cfg(feature = "adnet-token")]
    if let Some(payload) = adnet::try_parse_adnet_token(trimmed)? {
        return Ok(payload);
    }

    // chatmail-compatible schemes. Order is significant: longer prefixes
    // first, and the OPENPGP4FPR / i.delta.chat check has to run before
    // any HTTP(S) fallback.
    let upper = trimmed.to_ascii_uppercase();
    if upper.starts_with(crate::dclogin_scheme::DCLOGIN_SCHEME) || upper.starts_with("DCLOGIN://") {
        return decode_dclogin(trimmed, limits);
    }
    if upper.starts_with(crate::chatmail::DCACCOUNT_SCHEME) || upper.starts_with("DCACCOUNT://") {
        return decode_dcaccount(trimmed);
    }
    if upper.starts_with(crate::chatmail::DCBACKUP_SCHEME_PREFIX) {
        return decode_dcbackup(trimmed, limits);
    }
    if upper.starts_with(crate::chatmail::OPENPGP4FPR_SCHEME) {
        return Ok(QrPayload::OpenPgp4Fpr(decode_openpgp(trimmed, limits)?));
    }
    if let Some(rest) = trimmed
        .strip_prefix(crate::chatmail::IDELTACHAT_SCHEME)
        .or_else(|| trimmed.strip_prefix(crate::chatmail::IDELTACHAT_NOSLASH_SCHEME))
    {
        // The `https://i.delta.chat[/]#…` form is parsed identically to
        // OPENPGP4FPR, so we just rewrite the prefix and recurse.
        let rewrote = format!("{}{rest}", crate::chatmail::OPENPGP4FPR_SCHEME);
        return Ok(QrPayload::OpenPgp4Fpr(decode_openpgp(&rewrote, limits)?));
    }
    if let Some(payload) = crate::chatmail::decode_address_schemes(trimmed, limits)? {
        return Ok(payload);
    }
    if let Some(payload) = crate::chatmail::decode_proxy_url(trimmed, limits) {
        return Ok(payload);
    }

    // Anything that didn't match a known scheme is returned as a
    // free-form text payload. We deliberately do NOT try to URL-parse
    // here — chatmail does, but it has corner cases (e.g. plain host
    // strings containing `:`) that produce noisy `Url` variants for
    // things that are clearly just text. The caller can re-try with
    // `check_qr_url_only` if it really wants URL parsing.
    Ok(QrPayload::Text {
        text: trimmed.to_string(),
    })
}

/// Variant of [`check_qr`] that tries to URL-parse the input as a last
/// resort and returns [`QrPayload::Url`] instead of [`QrPayload::Text`].
pub fn check_qr_url_only(raw: &str) -> Result<QrPayload> {
    check_qr_url_only_with_limits(raw, &ParseLimits::default())
}

/// URL-aware variant of [`check_qr_with_limits`].
pub fn check_qr_url_only_with_limits(raw: &str, limits: &ParseLimits) -> Result<QrPayload> {
    let payload = check_qr_with_limits(raw, limits)?;
    if let QrPayload::Text { text } = &payload
        && url::Url::parse(text).is_ok()
    {
        return Ok(QrPayload::Url { url: text.clone() });
    }
    Ok(payload)
}

/// Convert the parsed payload back into the canonical string form that
/// should be encoded as a QR. Inverse of [`check_qr`] for every
/// variant.
pub fn encode_qr(payload: &QrPayload) -> Result<String> {
    match payload {
        QrPayload::Email {
            address,
            subject,
            body,
        } => {
            let mut s = format!("mailto:{address}");
            let mut params = Vec::new();
            if let Some(subj) = subject {
                params.push(format!(
                    "subject={}",
                    percent_encoding::utf8_percent_encode(subj, percent_encoding::NON_ALPHANUMERIC)
                ));
            }
            if let Some(body) = body {
                params.push(format!(
                    "body={}",
                    percent_encoding::utf8_percent_encode(body, percent_encoding::NON_ALPHANUMERIC)
                ));
            }
            if !params.is_empty() {
                s.push('?');
                s.push_str(&params.join("&"));
            }
            Ok(s)
        }
        QrPayload::Matmsg {
            address,
            subject,
            body,
        } => {
            let mut s = format!("MATMSG:TO:{address};");
            if let Some(subj) = subject {
                s.push_str(&format!(
                    "SUB:{};",
                    percent_encoding::utf8_percent_encode(subj, percent_encoding::NON_ALPHANUMERIC)
                ));
            }
            if let Some(body) = body {
                s.push_str(&format!(
                    "BODY:{};",
                    percent_encoding::utf8_percent_encode(body, percent_encoding::NON_ALPHANUMERIC)
                ));
            }
            Ok(s)
        }
        QrPayload::Vcard { name, address } => {
            // Minimal VCARD: split name into first/last, single EMAIL.
            let mut parts = name.splitn(2, ' ');
            let first = parts.next().unwrap_or("");
            let last = parts.next().unwrap_or("");
            Ok(format!(
                "BEGIN:VCARD\nN:{last};{first};;;\nEMAIL:{address}\nEND:VCARD"
            ))
        }
        QrPayload::Smtp { address } => Ok(format!("SMTP:{address}:")),
        QrPayload::DcAccount { domain } => Ok(format!("DCACCOUNT:{domain}")),
        QrPayload::DcLogin { address, options } => encode_dclogin(address, options),
        QrPayload::DcBackup {
            version,
            node_addr_json,
            auth_token,
        } => Ok(format!("DCBACKUP{version}:{auth_token}&{node_addr_json}")),
        QrPayload::BackupTooNew { .. } => Err(QrError::Malformed {
            scheme: "DCBACKUP",
            reason: "cannot encode BackupTooNew variant".into(),
        }),
        QrPayload::OpenPgp4Fpr(fields) => Ok(encode_openpgp4fpr(
            &fields.fingerprint,
            fields.address.as_deref(),
            fields.name.as_deref(),
            fields.invitenumber.as_deref(),
            fields.authcode.as_deref(),
            fields.group.as_ref(),
        )),
        QrPayload::Proxy { url, .. } => Ok(url.clone()),
        QrPayload::Shadowsocks { url, .. } => Ok(url.clone()),
        QrPayload::Url { url } => Ok(url.clone()),
        QrPayload::Text { text } => Ok(text.clone()),
        #[cfg(feature = "adnet-types")]
        QrPayload::AdnetPeer { ticket } => Ok(adnet_types::PeerTicket::encode(
            &ticket.node_id,
            &ticket.endpoint,
        )),
        #[cfg(feature = "adnet-types")]
        QrPayload::AdnetAddr { ticket } => Ok(adnet_types::NodeAddrTicket::encode(&ticket.0)),
        #[cfg(feature = "adnet-types")]
        QrPayload::AdnetBlob { ticket } => Ok(ticket.encode()),
        #[cfg(feature = "adnet-types")]
        QrPayload::AdnetSignedPeer { ticket } => Ok(ticket.encode()),
        #[cfg(feature = "adnet-token")]
        QrPayload::AdnetToken { pledge } => Ok(pledge.to_url()),
        #[cfg(feature = "pairing")]
        QrPayload::AdnetPairing { invitation } => match invitation {
            adnet_pairing::wire::PairingInvitation::Url(encoded) => {
                // Round-trip: the URL form must start with the pairing
                // scheme prefix or `encode_qr` is not the inverse of
                // `check_qr` for this variant.
                if !encoded.starts_with("adnet-pairing://") {
                    Ok(format!("adnet-pairing://{encoded}"))
                } else {
                    Ok(encoded.clone())
                }
            }
            adnet_pairing::wire::PairingInvitation::Json(inv) => {
                adnet_pairing::wire::PairingInvitation::to_url(inv).map_err(|e| {
                    crate::error::QrError::Malformed {
                        scheme: "adnet-pairing",
                        reason: format!("PairingInvitation::to_url: {e}"),
                    }
                })
            }
        },
    }
}

fn encode_dclogin(addr: &str, options: &DcLoginOptions) -> Result<String> {
    use crate::payload::{DcCertificateChecks, DcLoginSecurity};
    let mut query_pairs: Vec<(String, String)> = Vec::new();
    query_pairs.push(("p".into(), options.mail_pw.clone()));
    query_pairs.push(("v".into(), "1".into()));
    if let Some(h) = &options.imap_host {
        query_pairs.push(("ih".into(), h.clone()));
    }
    if let Some(p) = options.imap_port {
        query_pairs.push(("ip".into(), p.to_string()));
    }
    if let Some(u) = &options.imap_username {
        query_pairs.push(("iu".into(), u.clone()));
    }
    if let Some(p) = &options.imap_password {
        query_pairs.push(("ipw".into(), p.clone()));
    }
    if let Some(s) = options.imap_security {
        let code = match s {
            DcLoginSecurity::Ssl => "ssl",
            DcLoginSecurity::Starttls => "starttls",
            DcLoginSecurity::Plain => "plain",
            DcLoginSecurity::Default => "default",
        };
        query_pairs.push(("is".into(), code.into()));
    }
    if let Some(h) = &options.smtp_host {
        query_pairs.push(("sh".into(), h.clone()));
    }
    if let Some(p) = options.smtp_port {
        query_pairs.push(("sp".into(), p.to_string()));
    }
    if let Some(u) = &options.smtp_username {
        query_pairs.push(("su".into(), u.clone()));
    }
    if let Some(p) = &options.smtp_password {
        query_pairs.push(("spw".into(), p.clone()));
    }
    if let Some(s) = options.smtp_security {
        let code = match s {
            DcLoginSecurity::Ssl => "ssl",
            DcLoginSecurity::Starttls => "starttls",
            DcLoginSecurity::Plain => "plain",
            DcLoginSecurity::Default => "default",
        };
        query_pairs.push(("ss".into(), code.into()));
    }
    if let Some(c) = options.certificate_checks {
        let code = match c {
            DcCertificateChecks::Automatic => "0",
            DcCertificateChecks::Strict => "1",
            DcCertificateChecks::AcceptInvalid => "2",
        };
        query_pairs.push(("ic".into(), code.into()));
    }
    let query = query_pairs
        .into_iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                percent_encoding::utf8_percent_encode(
                    k.as_str(),
                    percent_encoding::NON_ALPHANUMERIC
                ),
                percent_encoding::utf8_percent_encode(
                    v.as_str(),
                    percent_encoding::NON_ALPHANUMERIC
                ),
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    Ok(format!("dclogin://{addr}?{query}"))
}

fn encode_openpgp4fpr(
    fingerprint: &str,
    address: Option<&str>,
    name: Option<&str>,
    invitenumber: Option<&str>,
    authcode: Option<&str>,
    group: Option<&OpenPgpGroup>,
) -> String {
    let mut params: Vec<(String, String)> = Vec::new();
    if let Some(addr) = address {
        params.push((
            "a".into(),
            percent_encoding::utf8_percent_encode(addr, percent_encoding::NON_ALPHANUMERIC)
                .to_string(),
        ));
    }
    if let Some(n) = name {
        params.push((
            "n".into(),
            percent_encoding::utf8_percent_encode(n, percent_encoding::NON_ALPHANUMERIC)
                .to_string(),
        ));
    }
    if let Some(i) = invitenumber {
        params.push(("i".into(), i.into()));
    }
    if let Some(s) = authcode {
        params.push(("s".into(), s.into()));
    }
    if let Some(g) = group {
        match g {
            OpenPgpGroup::Group { grpname, grpid } => {
                params.push((
                    "g".into(),
                    percent_encoding::utf8_percent_encode(
                        grpname,
                        percent_encoding::NON_ALPHANUMERIC,
                    )
                    .to_string(),
                ));
                params.push(("x".into(), grpid.clone()));
            }
            OpenPgpGroup::Broadcast { name, grpid } => {
                params.push((
                    "b".into(),
                    percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC)
                        .to_string(),
                ));
                params.push(("x".into(), grpid.clone()));
            }
        }
    }
    let query = params
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    if query.is_empty() {
        format!("OPENPGP4FPR:{fingerprint}")
    } else {
        format!("OPENPGP4FPR:{fingerprint}#{query}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::QrPayload;

    #[test]
    fn empty_input_is_malformed() {
        assert!(check_qr("").is_err());
        assert!(check_qr("   ").is_err());
    }

    #[test]
    fn mailto_round_trip() {
        let raw = "mailto:alice@example.com?subject=Hi&body=Hello%20there";
        let parsed = check_qr(raw).unwrap();
        match &parsed {
            QrPayload::Email {
                address,
                subject,
                body,
            } => {
                assert_eq!(address, "alice@example.com");
                assert_eq!(subject.as_deref(), Some("Hi"));
                // Body is percent-decoded so callers see real text;
                // the encoder will re-percent-encode the space.
                assert_eq!(body.as_deref(), Some("Hello there"));
            }
            other => panic!("expected Email, got {other:?}"),
        }
        let encoded = encode_qr(&parsed).unwrap();
        // Re-parsing the encoded form must yield the same parsed value.
        let reparsed = check_qr(&encoded).unwrap();
        assert_eq!(reparsed, parsed);
        assert!(encoded.starts_with("mailto:alice@example.com?"));
        assert!(encoded.contains("subject=Hi"));
        assert!(encoded.contains("body=Hello"));
    }

    #[test]
    fn unknown_input_falls_back_to_text() {
        let payload = check_qr("not a qr code").unwrap();
        match payload {
            QrPayload::Text { text } => assert_eq!(text, "not a qr code"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn dclogin_round_trip() {
        let raw = "dclogin://email@host.tld?p=secret&v=1&ih=imap.host.tld&ip=4000&is=ssl&ic=1";
        let parsed = check_qr(raw).unwrap();
        assert!(matches!(parsed, QrPayload::DcLogin { .. }));
        let encoded = encode_qr(&parsed).unwrap();
        let reparsed = check_qr(&encoded).unwrap();
        assert_eq!(parsed, reparsed);
    }
}
