//! Trusted-device record — the authoritative grant produced by a
//! successful pairing ceremony.
//!
//! A [`TrustedDeviceRecord`] is what both the issuer and the invitee
//! write to their [`TrustedDeviceStore`] after a
//! [`PairingResponse`] is verified. It is the durable proof that the
//! device has been paired, what capabilities it has, and whether it
//! has been revoked.
//!
//! ## Revocation model
//!
//! Revocation is a **wallet-level** action, not a per-device one:
//! the human uses their wallet (or another trusted device that holds
//! the wallet key) to invalidate a `credential_id`. The store simply
//! drops the record; there is no revocation list / CRL to maintain.
//! This matches how Signal and Delta Chat handle lost devices.

use serde::{Deserialize, Serialize};

use adnet_types::wallet_address::WalletAddress;

use crate::capability::CapabilitySet;
use crate::error::{PairingError, PairingResult};
use crate::transport_identity::CredentialId;

/// Status of a trusted-device record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedDeviceStatus {
    /// Fully paired. Capabilities are active.
    Active,
    /// Paired but temporarily suspended (e.g. device locked, not
    /// yet confirmed). Not usable for connection acceptance.
    Suspended,
    /// Revoked. The `TrustedDeviceStore` MUST NOT treat this record
    /// as valid for any purpose.
    Revoked,
}

impl TrustedDeviceStatus {
    /// Return `true` if the record is usable for connection acceptance.
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }
}

/// A single trusted-device entry in the persistent store.
///
/// Both the issuer and the invitee write one of these after a
/// successful pairing exchange. The two records differ only in their
/// `role`: the issuer records the invitee as a `Peer`, while the
/// invitee records the issuer as a `Controller`.
///
/// ⚠️ `Debug` is manually implemented and redacts the transport public
/// key so it never appears in logs.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedDeviceRecord {
    /// Stable identifier derived from the issuer + invitee node ids
    /// plus a random salt. This is the primary lookup key.
    pub credential_id: CredentialId,

    /// Which side of the pairing this record represents.
    pub role: TrustedDeviceRole,

    /// Human-readable device name chosen at pairing time.
    /// e.g. `"Alice's MacBook Pro"`.
    pub device_name: String,

    /// When the pairing was established (Unix seconds).
    pub paired_at_unix: i64,

    /// When the pairing record itself expires. The record is
    /// automatically dropped after this. `i64::MAX` = "no expiry".
    pub expires_at_unix: i64,

    /// When the device was last seen active (Unix seconds).
    pub last_seen_unix: i64,

    /// The transport identity (NodeId) of the remote device.
    /// Stored as a lowercase hex string to match `NodeId::as_hex()`.
    /// This makes the wire format consistent with `transport_pubkey`'s
    /// hex encoding and avoids ambiguity between raw bytes and hex.
    pub node_id: String,

    /// The Ed25519 public key of the remote device. Must equal the
    /// `node_id` bytes when the transport is iroh.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transport_pubkey: Vec<u8>,

    /// The wallet address of the remote device's owner. Stored as a
    /// typed [`WalletAddress`] for consistent validation; the on-disk
    /// format is lowercase hex (via `WalletAddress::as_hex()`).
    ///
    /// ⚠️ `Debug` on this type prints the address in full so it is
    /// safe for logs / UI display — wallet addresses are public
    /// identifiers and leaking them carries no security risk.
    pub wallet_address: Option<WalletAddress>,

    /// What this device is allowed to do.
    pub capabilities: CapabilitySet,

    /// Whether the record is currently valid.
    pub status: TrustedDeviceStatus,

    /// Monotonically increasing counter.
    pub record_version: u64,

    /// Issuer's transport NodeId (lowercase hex).
    pub issuer_node_id: String,

    /// When the record was revoked (Unix seconds). Set only when
    /// `status == Revoked`; left as `0` otherwise.
    #[serde(default)]
    pub revoked_at_unix: i64,
}

impl std::fmt::Debug for TrustedDeviceRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrustedDeviceRecord")
            .field("credential_id", &hex::encode(&self.credential_id[..4]))
            .field("role", &self.role)
            .field("device_name", &self.device_name)
            .field("paired_at_unix", &self.paired_at_unix)
            .field("expires_at_unix", &self.expires_at_unix)
            .field("last_seen_unix", &self.last_seen_unix)
            .field("node_id", &self.node_id)
            .field("transport_pubkey", &"<redacted>")
            .field("wallet_address", &self.wallet_address)
            .field("capabilities", &self.capabilities)
            .field("status", &self.status)
            .field("record_version", &self.record_version)
            .field("issuer_node_id", &self.issuer_node_id)
            .field("revoked_at_unix", &self.revoked_at_unix)
            .finish()
    }
}

/// Which side of the pairing this record represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedDeviceRole {
    /// This node is the pairing issuer; the remote is a `Peer`.
    Issuer,
    /// This node is the pairing invitee; the remote is the `Controller`.
    Invitee,
}

impl TrustedDeviceRecord {
    /// Check whether a capability is currently granted and the record
    /// is active.
    pub fn has_capability(&self, cap: crate::capability::Capability) -> bool {
        self.status.is_active() && self.capabilities.contains(cap)
    }

    /// Returns `true` if the record is expired as of `now_unix`.
    pub fn is_expired(&self, now_unix: i64) -> bool {
        self.expires_at_unix != i64::MAX && now_unix > self.expires_at_unix
    }

    /// Advance `last_seen_unix` and bump `record_version` atomically.
    pub fn touch(&mut self, now_unix: i64) {
        self.last_seen_unix = now_unix;
        self.record_version += 1;
    }

    /// Revoke this record. Sets `revoked_at_unix` to `now_unix`
    /// and bumps `record_version`.
    pub fn revoke(&mut self, now_unix: i64) {
        self.status = TrustedDeviceStatus::Revoked;
        self.revoked_at_unix = now_unix;
        self.record_version += 1;
    }

    /// Update the granted capabilities (e.g. after a re-pairing).
    pub fn update_capabilities(&mut self, caps: CapabilitySet) -> PairingResult<()> {
        if self.status != TrustedDeviceStatus::Active {
            return Err(crate::error::PairingError::DeviceRevoked(
                self.credential_id,
            ));
        }
        self.capabilities = caps;
        self.record_version += 1;
        Ok(())
    }

    /// Update the device name.
    pub fn rename(&mut self, name: String) {
        self.device_name = name;
        self.record_version += 1;
    }

    /// Validate the record's identifier fields.
    /// Call this after loading from disk or deserialising from untrusted input.
    /// Returns `Err` with the field name if any identifier is malformed.
    ///
    /// **`wallet_address`** is NOT re-validated here: it is already
    /// type-enforced by [`WalletAddress`] (which uses `TryFrom<String>`
    /// to guarantee the `0x` + 40-hex-char invariant at deserialisation
    /// time). The JSON schema `#[serde(try_from = "String", into = "String")]`
    /// on the type means a malformed wallet address will surface as a
    /// serde parse error *before* this method is ever called.
    pub fn validate(&self) -> Result<(), PairingError> {
        // CredentialId: must be non-zero (zero-id records cause
        // HashMap collisions and ambiguous lookups).
        if self.credential_id.iter().all(|&b| b == 0) {
            return Err(PairingError::Malformed {
                what: "trusted_device_record.credential_id",
                reason: "must not be all zeros (zero-id is invalid)".into(),
            });
        }
        // node_id: must be 64 lowercase hex chars.
        if self.node_id.len() != 64 {
            return Err(PairingError::Malformed {
                what: "trusted_device_record.node_id",
                reason: format!("must be 64 hex chars, got {}", self.node_id.len()),
            });
        }
        if !self.node_id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(PairingError::Malformed {
                what: "trusted_device_record.node_id",
                reason: "must be lowercase hex".into(),
            });
        }
        // issuer_node_id: must be 64 lowercase hex chars.
        if self.issuer_node_id.len() != 64 {
            return Err(PairingError::Malformed {
                what: "trusted_device_record.issuer_node_id",
                reason: format!("must be 64 hex chars, got {}", self.issuer_node_id.len()),
            });
        }
        if !self.issuer_node_id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(PairingError::Malformed {
                what: "trusted_device_record.issuer_node_id",
                reason: "must be lowercase hex".into(),
            });
        }
        // transport_pubkey: must be exactly 32 bytes (Ed25519 public key).
        // An empty vec means the field was skipped during serialisation —
        // this is the `#[serde(default)]` for old records, which is
        // a valid state for pre-iroh migrations (transport_pubkey was
        // added later). We allow empty for backwards compat but reject
        // any other non-32 length.
        if !self.transport_pubkey.is_empty() && self.transport_pubkey.len() != 32 {
            return Err(PairingError::Malformed {
                what: "trusted_device_record.transport_pubkey",
                reason: format!(
                    "must be 32 bytes (Ed25519 pubkey) or empty for backwards-compat, got {}",
                    self.transport_pubkey.len()
                ),
            });
        }
        // record_version: must be >= 1. Version 0 is produced only by
        // a default-constructed struct, never by the builder or store.
        if self.record_version == 0 {
            return Err(PairingError::Malformed {
                what: "trusted_device_record.record_version",
                reason: "must be >= 1 (zero is a default-construct sentinel)".into(),
            });
        }
        // wallet_address: skip — WalletAddress::try_from(String) already
        // enforced the format at deserialisation time.
        // Temporal ordering.
        if self.expires_at_unix != i64::MAX && self.expires_at_unix < self.paired_at_unix {
            return Err(PairingError::Malformed {
                what: "trusted_device_record.expires_at_unix",
                reason: format!(
                    "expires_at ({}) must be after paired_at ({})",
                    self.expires_at_unix, self.paired_at_unix
                ),
            });
        }
        if self.last_seen_unix < self.paired_at_unix {
            return Err(PairingError::Malformed {
                what: "trusted_device_record.last_seen_unix",
                reason: format!(
                    "last_seen ({}) must be >= paired_at ({})",
                    self.last_seen_unix, self.paired_at_unix
                ),
            });
        }
        if self.revoked_at_unix != 0 && self.revoked_at_unix < self.paired_at_unix {
            return Err(PairingError::Malformed {
                what: "trusted_device_record.revoked_at_unix",
                reason: format!(
                    "revoked_at ({}) must be >= paired_at ({})",
                    self.revoked_at_unix, self.paired_at_unix
                ),
            });
        }
        if self.status == TrustedDeviceStatus::Revoked && self.revoked_at_unix == 0 {
            return Err(PairingError::Malformed {
                what: "trusted_device_record.revoked_at_unix",
                reason: "revoked records must have revoked_at_unix > 0".into(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adnet_types::node::NodeId;

    fn node_id(hex: u8) -> String {
        NodeId::from_bytes(&[hex; 32]).unwrap().to_string()
    }

    fn make_record() -> TrustedDeviceRecord {
        TrustedDeviceRecord {
            credential_id: [
                0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
                0x0f, 0x10,
            ],
            role: TrustedDeviceRole::Issuer,
            device_name: "Test Device".into(),
            paired_at_unix: 1_700_000_000,
            expires_at_unix: i64::MAX,
            last_seen_unix: 1_700_000_000,
            node_id: node_id(0x01),
            transport_pubkey: vec![0x01u8; 32],
            wallet_address: None,
            capabilities: CapabilitySet::from_names(["chat", "files.read"]),
            status: TrustedDeviceStatus::Active,
            record_version: 1,
            issuer_node_id: node_id(0x02),
            revoked_at_unix: 0,
        }
    }

    #[test]
    fn has_capability_checks_status() {
        let mut r = make_record();
        assert!(r.has_capability(crate::capability::Capability::CHAT));
        r.status = TrustedDeviceStatus::Revoked;
        assert!(!r.has_capability(crate::capability::Capability::CHAT));
    }

    #[test]
    fn is_expired() {
        let mut r = make_record();
        assert!(!r.is_expired(i64::MAX));
        r.expires_at_unix = 1_700_000_000;
        assert!(r.is_expired(1_700_000_001));
        assert!(!r.is_expired(1_699_999_999));
    }

    #[test]
    fn touch_bumps_version() {
        let mut r = make_record();
        let v = r.record_version;
        r.touch(1_800_000_000);
        assert_eq!(r.record_version, v + 1);
    }

    #[test]
    fn revoke_prevents_capability_change() {
        let mut r = make_record();
        r.revoke(1_800_000_000);
        let err = r.update_capabilities(CapabilitySet::empty()).unwrap_err();
        assert!(matches!(err, crate::error::PairingError::DeviceRevoked(_)));
    }

    #[test]
    fn revoke_sets_revoked_at() {
        let mut r = make_record();
        assert_eq!(r.revoked_at_unix, 0);
        r.revoke(1_900_000_000);
        assert_eq!(r.revoked_at_unix, 1_900_000_000);
        assert_eq!(r.status, TrustedDeviceStatus::Revoked);
    }

    #[test]
    fn serde_round_trip() {
        let r = make_record();
        let json = serde_json::to_string(&r).unwrap();
        let back: TrustedDeviceRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }

    // ── validate() edge cases ─────────────────────────────────────

    #[test]
    fn validate_accepts_valid_record() {
        let r = make_record();
        assert!(r.validate().is_ok());
    }

    #[test]
    fn validate_rejects_zero_credential_id() {
        let mut r = make_record();
        r.credential_id = [0u8; 16];
        let err = r.validate().unwrap_err();
        assert!(format!("{err}").contains("credential_id"));
    }

    #[test]
    fn validate_rejects_non_hex_node_id() {
        let mut r = make_record();
        r.node_id = "zzzz".repeat(16); // 'z' is not hex
        let err = r.validate().unwrap_err();
        assert!(format!("{err}").contains("node_id"));
    }

    #[test]
    fn validate_rejects_short_node_id() {
        let mut r = make_record();
        r.node_id = "a".repeat(63);
        let err = r.validate().unwrap_err();
        assert!(format!("{err}").contains("node_id"));
    }

    #[test]
    fn validate_accepts_empty_transport_pubkey() {
        // Empty transport_pubkey is allowed for backwards-compat with
        // pre-iroh records that skipped the field.
        let mut r = make_record();
        r.transport_pubkey = vec![];
        assert!(r.validate().is_ok());
    }

    #[test]
    fn validate_rejects_wrong_length_transport_pubkey() {
        let mut r = make_record();
        r.transport_pubkey = vec![0u8; 31]; // wrong length
        let err = r.validate().unwrap_err();
        assert!(format!("{err}").contains("transport_pubkey"));
    }

    #[test]
    fn validate_rejects_zero_record_version() {
        let mut r = make_record();
        r.record_version = 0;
        let err = r.validate().unwrap_err();
        assert!(format!("{err}").contains("record_version"));
    }

    #[test]
    fn validate_rejects_revoked_before_paired() {
        let mut r = make_record();
        r.status = TrustedDeviceStatus::Revoked;
        r.revoked_at_unix = r.paired_at_unix - 1;
        let err = r.validate().unwrap_err();
        assert!(format!("{err}").contains("revoked_at"));
    }

    #[test]
    fn validate_rejects_revoked_without_revoked_at() {
        let mut r = make_record();
        r.status = TrustedDeviceStatus::Revoked;
        r.revoked_at_unix = 0; // zero not allowed for revoked
        let err = r.validate().unwrap_err();
        assert!(format!("{err}").contains("revoked_at_unix"));
    }

    #[test]
    fn validate_rejects_last_seen_before_paired() {
        let mut r = make_record();
        r.last_seen_unix = r.paired_at_unix - 1;
        let err = r.validate().unwrap_err();
        assert!(format!("{err}").contains("last_seen"));
    }
}
