//! ADNet-native QR payloads.
//!
//! Every function in this module is a thin wrapper over the
//! `adnet-types` / `adnet-token` codecs; the QR format is just the URL
//! representation those crates already produce.
//!
//! | QR URL prefix         | Source crate        | Encoding               |
//! |-----------------------|---------------------|------------------------|
//! | `adnet-peer://`       | `adnet_types`       | `PeerTicket::encode`   |
//! | `adnet-addr://`       | `adnet_types`       | `NodeAddrTicket::encode`|
//! | `adnet-blob://`       | `adnet_types`       | `BlobTicket::encode`   |
//! | `adnet-signed-peer://`| `adnet_types`       | `SignedPeerTicket::encode`|
//! | `adnet-token://`      | `adnet_token`       | `Pledge::to_url`       |

use crate::error::{QrError, Result};
use crate::payload::QrPayload;

/// Try to parse `raw` as one of the `adnet-…` URLs. Returns:
///
/// - `Ok(Some(payload))` if the input matched an ADNet prefix and
///   parsed cleanly,
/// - `Ok(None)` if the input is not an ADNet URL at all (caller should
///   continue with other schemes),
/// - `Err(QrError::Malformed { .. })` if the input *looks* ADNet-shaped
///   but the payload is broken (e.g. `adnet-blob://garbage`).
#[cfg(feature = "adnet-types")]
pub fn try_parse_adnet_ticket(raw: &str) -> Result<Option<QrPayload>> {
    if raw.starts_with("adnet-peer://") {
        let ticket = adnet_types::PeerTicket::parse(raw).map_err(|e| QrError::Malformed {
            scheme: "adnet-peer",
            reason: format!("PeerTicket::parse: {e}"),
        })?;
        return Ok(Some(QrPayload::AdnetPeer { ticket }));
    }
    if raw.starts_with("adnet-addr://") {
        let ticket = adnet_types::NodeAddrTicket::parse(raw).map_err(|e| QrError::Malformed {
            scheme: "adnet-addr",
            reason: format!("NodeAddrTicket::parse: {e}"),
        })?;
        return Ok(Some(QrPayload::AdnetAddr { ticket }));
    }
    if raw.starts_with("adnet-blob://") {
        let ticket = adnet_types::BlobTicket::parse(raw).map_err(|e| QrError::Malformed {
            scheme: "adnet-blob",
            reason: format!("BlobTicket::parse: {e}"),
        })?;
        return Ok(Some(QrPayload::AdnetBlob { ticket }));
    }
    if raw.starts_with("adnet-signed-peer://") {
        let ticket = adnet_types::SignedPeerTicket::parse(raw).map_err(|e| QrError::Malformed {
            scheme: "adnet-signed-peer",
            reason: format!("SignedPeerTicket::parse: {e}"),
        })?;
        return Ok(Some(QrPayload::AdnetSignedPeer { ticket }));
    }
    Ok(None)
}

/// Try to parse `raw` as an `adnet-token://` URL (a relay-payment
/// pledge).
#[cfg(feature = "adnet-token")]
pub fn try_parse_adnet_token(raw: &str) -> Result<Option<QrPayload>> {
    if !raw.starts_with("adnet-token://") {
        return Ok(None);
    }
    let pledge = adnet_token::Pledge::from_url(raw).map_err(|e| QrError::Malformed {
        scheme: "adnet-token",
        reason: format!("Pledge::from_url: {e}"),
    })?;
    Ok(Some(QrPayload::AdnetToken { pledge }))
}

/// Try to parse `raw` as an `adnet-pairing://…` URL carrying a
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
pub fn try_parse_adnet_pairing(raw: &str) -> Result<Option<QrPayload>> {
    if !raw.starts_with("adnet-pairing://") {
        return Ok(None);
    }
    let invitation =
        adnet_pairing::wire::PairingInvitation::parse_url(raw).map_err(|e| QrError::Malformed {
            scheme: "adnet-pairing",
            reason: format!("PairingInvitation::parse_url: {e}"),
        })?;
    Ok(Some(QrPayload::AdnetPairing { invitation }))
}

#[cfg(test)]
#[cfg(feature = "adnet-types")]
mod tests {
    use super::*;
    use adnet_types::{ContentHash, Endpoint, NodeAddr, NodeId, RangeSpec};

    #[test]
    fn parses_adnet_peer() {
        let id = NodeId::random();
        let addr = NodeAddr::new(id.clone()).with_direct(Endpoint::new("1.2.3.4", 5678));
        let raw = adnet_types::PeerTicket::encode(&id, &addr);
        let parsed = try_parse_adnet_ticket(&raw).unwrap().unwrap();
        assert!(matches!(parsed, QrPayload::AdnetPeer { .. }));
    }

    #[test]
    fn parses_adnet_blob() {
        let id = NodeId::random();
        let addr = NodeAddr::new(id.clone()).with_direct(Endpoint::new("1.2.3.4", 5678));
        let hash = ContentHash::from_bytes(b"hello");
        let ticket = adnet_types::BlobTicket::whole(&id, &addr, &hash);
        let raw = ticket.encode();
        let parsed = try_parse_adnet_ticket(&raw).unwrap().unwrap();
        assert!(matches!(parsed, QrPayload::AdnetBlob { .. }));
    }

    #[test]
    fn rejects_malformed_adnet_peer() {
        let err = try_parse_adnet_ticket("adnet-peer://garbage").unwrap_err();
        assert!(matches!(err, QrError::Malformed { .. }));
    }

    #[test]
    fn returns_none_for_unrelated_input() {
        let parsed = try_parse_adnet_ticket("mailto:foo@bar.com").unwrap();
        assert!(parsed.is_none());
    }

    #[test]
    fn multi_range_blob_round_trips() {
        let id = NodeId::random();
        let addr = NodeAddr::new(id.clone()).with_direct(Endpoint::new("1.2.3.4", 5678));
        let hash = ContentHash::from_bytes(b"x");
        let ticket =
            adnet_types::BlobTicket::whole(&id, &addr, &hash).with_range(RangeSpec::Multi(vec![
                adnet_types::ByteRange::new(0, 5).unwrap(),
                adnet_types::ByteRange::new(100, 200).unwrap(),
            ]));
        let raw = ticket.encode();
        let parsed = try_parse_adnet_ticket(&raw).unwrap().unwrap();
        match parsed {
            QrPayload::AdnetBlob { ticket: t } => {
                assert_eq!(t.content_hash, hash);
                assert!(matches!(t.range, RangeSpec::Multi(_)));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }
}
