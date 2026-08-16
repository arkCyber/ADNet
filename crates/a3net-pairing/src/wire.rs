//! QR-code / deep-link wire format for pairing invitations.
//!
//! The [`PairingInvitation`] enum unifies all the ways a pairing
//! invitation can be presented to the user. The A3Net QR scanner
//! (`a3net-qr`) will classify `a3net-pairing://` URLs as this type,
//! and the email invitation (`a3net-invite`) will attach a
//! `application/x-a3net-pairing` JSON blob.
//!
//! Supported URL schemes:
//!
//! | Scheme                        | Context              |
//! |-------------------------------|----------------------|
//! | `a3net-pairing://<base64url>` | QR code, deep link   |
//!
//! The base64url payload is `base64url_encode(json_bytes(SignedInvitation))`
//! with no padding, matching the existing `SignedPeerTicket::encode` style.

use serde::{Deserialize, Serialize};

use crate::error::{PairingError, PairingResult};
use crate::invitation::SignedInvitation;

const PAIRING_URL_PREFIX: &str = "a3net-pairing://";

/// Unified QR / email invitation type. `a3net-qr`'s scanner will
/// classify any `a3net-pairing://` URL as this variant.
///
/// Variants:
/// - `Url(base64url_encoded_json)` — scanned QR / deep link
/// - `Json(SignedInvitation)` — parsed from email attachment
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PairingInvitation {
    /// Raw base64url-encoded invitation (from QR/deep-link).
    /// Deserialize this to get the full [`SignedInvitation`].
    Url(String),
    /// Full decoded invitation (from email attachment or programmatic use).
    Json(SignedInvitation),
}

impl PairingInvitation {
    /// Parse an `a3net-pairing://<base64url>` URL into a
    /// `PairingInvitation::Url`.
    ///
    /// Does NOT verify the invitation signature — call
    /// `decode().and_then(|i| i.verify(now_unix))` to do that.
    pub fn parse_url(raw: &str) -> PairingResult<Self> {
        let url = raw
            .strip_prefix(PAIRING_URL_PREFIX)
            .ok_or_else(|| PairingError::Malformed {
                what: "pairing_url",
                reason: format!(
                    "URL must start with '{}'",
                    PAIRING_URL_PREFIX.trim_end_matches("://")
                ),
            })?;
        if url.len() > 2048 {
            return Err(PairingError::Malformed {
                what: "pairing_url",
                reason: "URL payload exceeds 2048 bytes".into(),
            });
        }
        // Validate it's valid base64url before accepting it.
        let _ = base64url_decode(url)?;
        Ok(Self::Url(url.to_string()))
    }

    /// Build a QR-compatible URL string from a `SignedInvitation`.
    pub fn to_url(inv: &SignedInvitation) -> PairingResult<String> {
        let json = inv.to_json()?;
        let encoded = base64url_encode(json.as_bytes());
        // Sanity check: reject absurdly large invitations before
        // they hit the QR content limit. A well-formed invitation
        // with the current protocol version is never this large.
        const MAX_URL_PAYLOAD: usize = 4096;
        if encoded.len() > MAX_URL_PAYLOAD {
            return Err(PairingError::Malformed {
                what: "signed_invitation",
                reason: format!(
                    "encoded payload exceeds {} bytes (got {}): \
                     invitation is too large to render as a QR code",
                    MAX_URL_PAYLOAD,
                    encoded.len()
                ),
            });
        }
        Ok(format!("{PAIRING_URL_PREFIX}{encoded}"))
    }

    /// Decode the URL variant to a `SignedInvitation`.
    /// Returns `None` if this is a `Json` variant (already decoded).
    pub fn decode(&self) -> PairingResult<Option<SignedInvitation>> {
        match self {
            Self::Url(encoded) => {
                let bytes = base64url_decode(encoded)?;
                let inv: SignedInvitation =
                    serde_json::from_slice(&bytes).map_err(|e| PairingError::Malformed {
                        what: "pairing_invitation.json",
                        reason: format!("JSON parse failed: {e}"),
                    })?;
                Ok(Some(inv))
            }
            Self::Json(inv) => Ok(Some(inv.clone())),
        }
    }

    /// Verify the invitation's wallet signature. Convenience wrapper
    /// around `decode().and_then(|i| i.verify(now_unix))`.
    pub fn verify(&self, now_unix: i64) -> PairingResult<()> {
        self.decode()
            .and_then(|opt| {
                opt.ok_or_else(|| PairingError::Malformed {
                    what: "pairing_invitation",
                    reason: "no decoded invitation".into(),
                })
            })?
            .verify(now_unix)
    }
}

// Minimal base64url (no padding) — matches the style already in
// `a3net-types::ticket`. We could depend on `base64` but this is
// < 30 lines and keeps the crate dependency-free for the QR path.

const B64_URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64url_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity((input.len() * 4).div_ceil(3));
    let mut i = 0;
    while i + 3 <= input.len() {
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8) | (input[i + 2] as u32);
        out.push(B64_URL[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64_URL[((n >> 12) & 0x3f) as usize] as char);
        out.push(B64_URL[((n >> 6) & 0x3f) as usize] as char);
        out.push(B64_URL[(n & 0x3f) as usize] as char);
        i += 3;
    }
    let rem = input.len() - i;
    if rem == 1 {
        // 8 bits → 2 digits at shifts 18 and 12, both masked with 0x3F
        let n = (input[i] as u32) << 16;
        out.push(B64_URL[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64_URL[((n >> 12) & 0x3f) as usize] as char);
    } else if rem == 2 {
        // 16 bits → 3 digits at shifts 18, 12, 6, all masked with 0x3F
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8);
        out.push(B64_URL[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64_URL[((n >> 12) & 0x3f) as usize] as char);
        out.push(B64_URL[((n >> 6) & 0x3f) as usize] as char);
    }
    out
}

fn decode_char(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'-' => Some(62),
        b'_' => Some(63),
        _ => None,
    }
}

fn base64url_decode(input: &str) -> PairingResult<Vec<u8>> {
    let bytes = input.as_bytes();
    // Length cap: base64 decodes to ~75% of input length. An
    // unbounded decode allows a malicious caller to allocate arbitrarily
    // large output by feeding a very long base64 string. The
    // `parse_url` URL-length check (2048) limits the wire form, but
    // we cap the decoded bytes too so the inner decode is also bounded.
    const MAX_INPUT_LEN: usize = 2048;
    if bytes.len() > MAX_INPUT_LEN {
        return Err(PairingError::Malformed {
            what: "base64url",
            reason: format!(
                "input exceeds {} bytes (got {})",
                MAX_INPUT_LEN,
                bytes.len()
            ),
        });
    }
    let groups = bytes.len() / 4;
    let rem = bytes.len() % 4;
    let out_len = groups * 3
        + match rem {
            0 | 1 => 0,
            2 => 1,
            _ => 2,
        };
    let mut out = Vec::with_capacity(out_len);
    let mut i = 0;
    while i + 4 <= bytes.len() {
        let a = decode_char(bytes[i]).ok_or_else(|| PairingError::Malformed {
            what: "base64url",
            reason: format!("invalid char '{}'", bytes[i] as char),
        })?;
        let b = decode_char(bytes[i + 1]).ok_or_else(|| PairingError::Malformed {
            what: "base64url",
            reason: format!("invalid char '{}'", bytes[i + 1] as char),
        })?;
        let c = decode_char(bytes[i + 2]).ok_or_else(|| PairingError::Malformed {
            what: "base64url",
            reason: format!("invalid char '{}'", bytes[i + 2] as char),
        })?;
        let d = decode_char(bytes[i + 3]).ok_or_else(|| PairingError::Malformed {
            what: "base64url",
            reason: format!("invalid char '{}'", bytes[i + 3] as char),
        })?;
        out.push(((a as usize) << 2 | (b as usize) >> 4) as u8);
        out.push(((b as usize) << 4 | (c as usize) >> 2) as u8);
        out.push(((c as usize) << 6 | d as usize) as u8);
        i += 4;
    }
    match rem {
        0 => {}
        2 => {
            let a = decode_char(bytes[i]).ok_or_else(|| PairingError::Malformed {
                what: "base64url",
                reason: format!("invalid char '{}'", bytes[i] as char),
            })?;
            let b = decode_char(bytes[i + 1]).ok_or_else(|| PairingError::Malformed {
                what: "base64url",
                reason: format!("invalid char '{}'", bytes[i + 1] as char),
            })?;
            out.push(((a as usize) << 2 | (b as usize) >> 4) as u8);
        }
        3 => {
            let a = decode_char(bytes[i]).ok_or_else(|| PairingError::Malformed {
                what: "base64url",
                reason: format!("invalid char '{}'", bytes[i] as char),
            })?;
            let b = decode_char(bytes[i + 1]).ok_or_else(|| PairingError::Malformed {
                what: "base64url",
                reason: format!("invalid char '{}'", bytes[i + 1] as char),
            })?;
            let c = decode_char(bytes[i + 2]).ok_or_else(|| PairingError::Malformed {
                what: "base64url",
                reason: format!("invalid char '{}'", bytes[i + 2] as char),
            })?;
            out.push(((a as usize) << 2 | (b as usize) >> 4) as u8);
            out.push(((b as usize) << 4 | (c as usize) >> 2) as u8);
        }
        _ => {
            return Err(PairingError::Malformed {
                what: "base64url",
                reason: "invalid length".into(),
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::CapabilitySet;
    use a3net_identity::wallet::Wallet;
    use a3net_types::node::NodeId;

    fn make_inv() -> SignedInvitation {
        let node_id = NodeId::from_bytes(&[0xDDu8; 32]).unwrap();
        let wallet = Wallet::generate();
        SignedInvitation::create(
            &node_id,
            &wallet,
            CapabilitySet::from_names(["chat"]),
            600,
            None,
        )
        .unwrap()
    }

    #[test]
    fn round_trip_url() {
        let inv = make_inv();
        let url = PairingInvitation::to_url(&inv).unwrap();
        assert!(url.starts_with(PAIRING_URL_PREFIX));
        let parsed = PairingInvitation::parse_url(&url).unwrap();
        let decoded = parsed.decode().unwrap().unwrap();
        assert_eq!(decoded.payload.issuer_wallet, inv.payload.issuer_wallet);
    }

    #[test]
    fn round_trip_json() {
        let inv = make_inv();
        let pi = PairingInvitation::Json(inv.clone());
        let json = serde_json::to_string(&pi).unwrap();
        let back: PairingInvitation = serde_json::from_str(&json).unwrap();
        let decoded = back.decode().unwrap().unwrap();
        assert_eq!(decoded.payload.issuer_wallet, inv.payload.issuer_wallet);
    }

    #[test]
    fn invalid_url_prefix_rejected() {
        let err = PairingInvitation::parse_url("https://example.com").unwrap_err();
        assert!(matches!(
            err,
            PairingError::Malformed {
                what: "pairing_url",
                ..
            }
        ));
    }

    #[test]
    fn base64url_round_trip() {
        let original = b"Hello, World! \xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e";
        let encoded = base64url_encode(original);
        let decoded = base64url_decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn base64url_encode_correct_padding_rem1() {
        // 1-byte input: 8 bits → 2 base64 digits + "=="
        let original = [0xFF];
        let encoded = base64url_encode(&original);
        assert_eq!(encoded, "_w");
        let decoded = base64url_decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn base64url_encode_correct_padding_rem2() {
        // 2-byte input: 16 bits → 3 base64 digits + "="
        let original = [0xFF, 0xFE];
        let encoded = base64url_encode(&original);
        assert_eq!(encoded, "__4");
        let decoded = base64url_decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn base64url_decode_accepts_uppercase_and_lowercase() {
        // Verify encode+decode round-trips through the full base64url alphabet.
        let original: Vec<u8> = (b'A'..=b'Z')
            .chain(b'a'..=b'z')
            .chain(b'0'..=b'9')
            .collect();
        let encoded = base64url_encode(&original);
        let decoded = base64url_decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn base64url_decode_rejects_invalid_chars() {
        // '+' is valid base64 but invalid base64url.
        let err = base64url_decode("QUJD+").unwrap_err();
        assert!(matches!(
            err,
            PairingError::Malformed {
                what: "base64url",
                ..
            }
        ));
    }

    #[test]
    fn base64url_decode_length_cap() {
        // Feed a very long input to confirm the cap fires.
        let long = "A".repeat(2049);
        let err = base64url_decode(&long).unwrap_err();
        assert!(matches!(
            err,
            PairingError::Malformed {
                what: "base64url",
                ..
            }
        ));
        assert!(format!("{err}").contains("2048"));
    }

    #[test]
    fn verify_after_decode() {
        let inv = make_inv();
        let pi = PairingInvitation::Json(inv.clone());
        let now = chrono::Utc::now().timestamp();
        pi.verify(now).unwrap();
    }

    #[test]
    fn to_url_rejects_oversized_invitation() {
        // Manually build an invitation with a note that's far too large.
        let mut inv = make_inv();
        inv.payload.note = Some("A".repeat(3000));
        // The JSON + base64url should push over our 4096 cap.
        let err = PairingInvitation::to_url(&inv).unwrap_err();
        assert!(matches!(
            err,
            PairingError::Malformed {
                what: "signed_invitation",
                ..
            }
        ));
        assert!(format!("{err}").contains("4096"));
    }
}
