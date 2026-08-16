//! A3Net-native QR payloads.
//!
//! Every function in this module is a thin wrapper over the
//! `a3net-types` / `a3net-token` codecs; the QR format is just the URL
//! representation those crates already produce.
//!
//! | QR URL prefix         | Source crate        | Encoding               |
//! |-----------------------|---------------------|------------------------|
//! | `a3net-peer://`       | `a3net_types`       | `PeerTicket::encode`   |
//! | `a3net-addr://`       | `a3net_types`       | `NodeAddrTicket::encode`|
//! | `a3net-blob://`       | `a3net_types`       | `BlobTicket::encode`   |
//! | `a3net-signed-peer://`| `a3net_types`       | `SignedPeerTicket::encode`|
//! | `a3net-token://`      | `a3net_token`       | `Pledge::to_url`       |

use crate::error::{QrError, Result};
use crate::payload::QrPayload;

/// Try to parse `raw` as one of the `a3net-…` URLs. Returns:
///
/// - `Ok(Some(payload))` if the input matched an A3Net prefix and
///   parsed cleanly,
/// - `Ok(None)` if the input is not an A3Net URL at all (caller should
///   continue with other schemes),
/// - `Err(QrError::Malformed { .. })` if the input *looks* A3Net-shaped
///   but the payload is broken (e.g. `a3net-blob://garbage`).
#[cfg(feature = "a3net-types")]
pub fn try_parse_a3net_ticket(raw: &str) -> Result<Option<QrPayload>> {
    if raw.starts_with("a3net-peer://") {
        let ticket = a3net_types::PeerTicket::parse(raw).map_err(|e| QrError::Malformed {
            scheme: "a3net-peer",
            reason: format!("PeerTicket::parse: {e}"),
        })?;
        return Ok(Some(QrPayload::AdnetPeer { ticket }));
    }
    if raw.starts_with("a3net-addr://") {
        let ticket = a3net_types::NodeAddrTicket::parse(raw).map_err(|e| QrError::Malformed {
            scheme: "a3net-addr",
            reason: format!("NodeAddrTicket::parse: {e}"),
        })?;
        return Ok(Some(QrPayload::AdnetAddr { ticket }));
    }
    if raw.starts_with("a3net-blob://") {
        let ticket = a3net_types::BlobTicket::parse(raw).map_err(|e| QrError::Malformed {
            scheme: "a3net-blob",
            reason: format!("BlobTicket::parse: {e}"),
        })?;
        return Ok(Some(QrPayload::AdnetBlob { ticket }));
    }
    if raw.starts_with("a3net-signed-peer://") {
        let ticket = a3net_types::SignedPeerTicket::parse(raw).map_err(|e| QrError::Malformed {
            scheme: "a3net-signed-peer",
            reason: format!("SignedPeerTicket::parse: {e}"),
        })?;
        return Ok(Some(QrPayload::AdnetSignedPeer { ticket }));
    }
    Ok(None)
}

/// Try to parse `raw` as an `a3net-token://` URL (a relay-payment
/// pledge).
#[cfg(feature = "a3net-token")]
pub fn try_parse_a3net_token(raw: &str) -> Result<Option<QrPayload>> {
    if !raw.starts_with("a3net-token://") {
        return Ok(None);
    }
    let pledge = a3net_token::Pledge::from_url(raw).map_err(|e| QrError::Malformed {
        scheme: "a3net-token",
        reason: format!("Pledge::from_url: {e}"),
    })?;
    Ok(Some(QrPayload::AdnetToken { pledge }))
}

/// Try to parse `raw` as an `a3net-pairing://…` URL carrying a
/// `PairingInvitation`.
///
/// Returns:
/// - `Ok(Some(payload))` if the input matched the pairing prefix and
///   parsed cleanly,
/// - `Ok(None)` if the input is not a pairing URL (caller should
///   continue with other schemes),
/// - `Err(QrError::Malformed { .. })` if the input *looks* like a
///   pairing URL but the payload is broken (bad base64url, oversized,
///   bad JSON).
///
/// Verification of the inner `SignedInvitation` wallet signature is
/// **not** performed here — the caller should follow up with
/// `invitation.verify(now_unix)` (or, for the on-wire ceremony,
/// `pairing_invitation_digest` + wallet recovery) before treating the
/// envelope as authoritative. We deliberately keep `check_qr` total
/// so a malformed URL doesn't get promoted to a silent `Text`
/// fallback.
#[cfg(feature = "pairing")]
pub fn try_parse_a3net_pairing(raw: &str) -> Result<Option<QrPayload>> {
    if !raw.starts_with("a3net-pairing://") {
        return Ok(None);
    }
    let invitation =
        a3net_pairing::wire::PairingInvitation::parse_url(raw).map_err(|e| QrError::Malformed {
            scheme: "a3net-pairing",
            reason: format!("PairingInvitation::parse_url: {e}"),
        })?;
    Ok(Some(QrPayload::AdnetPairing { invitation }))
}

#[cfg(test)]
#[cfg(feature = "a3net-types")]
mod tests {
    use super::*;
    use a3net_types::{ContentHash, Endpoint, NodeAddr, NodeId, RangeSpec};

    #[test]
    fn parses_a3net_peer() {
        let id = NodeId::random();
        let addr = NodeAddr::new(id.clone()).with_direct(Endpoint::new("1.2.3.4", 5678));
        let raw = a3net_types::PeerTicket::encode(&id, &addr);
        let parsed = try_parse_a3net_ticket(&raw).unwrap().unwrap();
        assert!(matches!(parsed, QrPayload::AdnetPeer { .. }));
    }

    #[test]
    fn parses_a3net_blob() {
        let id = NodeId::random();
        let addr = NodeAddr::new(id.clone()).with_direct(Endpoint::new("1.2.3.4", 5678));
        let hash = ContentHash::from_bytes(b"hello");
        let ticket = a3net_types::BlobTicket::whole(&id, &addr, &hash);
        let raw = ticket.encode();
        let parsed = try_parse_a3net_ticket(&raw).unwrap().unwrap();
        assert!(matches!(parsed, QrPayload::AdnetBlob { .. }));
    }

    #[test]
    fn rejects_malformed_a3net_peer() {
        let err = try_parse_a3net_ticket("a3net-peer://garbage").unwrap_err();
        assert!(matches!(err, QrError::Malformed { .. }));
    }

    #[test]
    fn returns_none_for_unrelated_input() {
        let parsed = try_parse_a3net_ticket("mailto:foo@bar.com").unwrap();
        assert!(parsed.is_none());
    }

    #[test]
    fn multi_range_blob_round_trips() {
        let id = NodeId::random();
        let addr = NodeAddr::new(id.clone()).with_direct(Endpoint::new("1.2.3.4", 5678));
        let hash = ContentHash::from_bytes(b"x");
        let ticket =
            a3net_types::BlobTicket::whole(&id, &addr, &hash).with_range(RangeSpec::Multi(vec![
                a3net_types::ByteRange::new(0, 5).unwrap(),
                a3net_types::ByteRange::new(100, 200).unwrap(),
            ]));
        let raw = ticket.encode();
        let parsed = try_parse_a3net_ticket(&raw).unwrap().unwrap();
        match parsed {
            QrPayload::AdnetBlob { ticket: t } => {
                assert_eq!(t.content_hash, hash);
                assert!(matches!(t.range, RangeSpec::Multi(_)));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }
}
