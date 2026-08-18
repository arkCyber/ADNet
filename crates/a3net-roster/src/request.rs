//! Persisted contact-request record.
//!
//! Lighter-weight than [`a3chat_core::contact::ContactRequest`] — the
//! in-process `ContactRequest` carries presentation fields (display
//! name, message, status timestamps) and is part of the public RPC
//! envelope. This struct is the *persistence* slice: just enough
//! identity + cryptographic fields to
//!
//! 1. Look up a request by `request_id` at accept time.
//! 2. Verify that the inbound `accept_request` came from the same
//!    `from_user_id` that originally issued the request, by checking
//!    the `signature_b64` field against `from_user_id`'s public key.
//! 3. Tell expired requests from live ones (`created_at_unix` + 7d
//!    TTL — see `a3chat_core::contact::REQUEST_TTL_SECS`).
//!
//! The `request_id` is the row primary key in both the in-memory map
//! and the SQLite table.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Persistence record for an outbound or inbound friend request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedContactRequest {
    /// Globally-unique request id (also the DB primary key).
    pub request_id: String,
    /// User id that *originated* the request. The verifier uses
    /// this to look up the sender's public key when checking
    /// `signature_b64`.
    pub from_user_id: String,
    /// User id the request is addressed to.
    pub to_user_id: String,
    /// Short greeting from the sender.
    pub message: String,
    /// Lifecycle state at persistence time. Only `pending`,
    /// `accepted`, `rejected`, `cancelled`, `expired` are recorded
    /// here — the enum is duplicated as a string column to avoid an
    /// extra migration step.
    pub status: String,
    /// Unix timestamp (seconds) when the request was issued.
    pub created_at_unix: i64,
    /// Unix timestamp (seconds) when the request reached a terminal
    /// state. `None` while still `pending`.
    pub responded_at_unix: Option<i64>,
    /// Optional Ed25519 signature (base64) over `signature_payload()`.
    /// When present, [`Self::signature_payload`] can be reconstructed
    /// and verified by [`ContactService::accept_request`][1] using the
    /// sender's public key.
    ///
    /// [1]: ../../a3chat_app/contact_service/struct.ContactService.html
    pub signature_b64: Option<String>,
}

impl PersistedContactRequest {
    /// Convenience: derive `created_at_unix` from a chrono timestamp.
    pub fn from_chrono(
        request_id: String,
        from_user_id: String,
        to_user_id: String,
        message: String,
        status: String,
        created_at: DateTime<Utc>,
        responded_at: Option<DateTime<Utc>>,
        signature_b64: Option<String>,
    ) -> Self {
        Self {
            request_id,
            from_user_id,
            to_user_id,
            message,
            status,
            created_at_unix: created_at.timestamp(),
            responded_at_unix: responded_at.map(|t| t.timestamp()),
            signature_b64,
        }
    }

    /// `created_at` as a `chrono::DateTime<Utc>`. Caller compares
    /// against `Utc::now()` for TTL.
    pub fn created_at(&self) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(self.created_at_unix, 0)
            .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).unwrap())
    }

    /// `responded_at` as a `chrono::DateTime<Utc>`.
    pub fn responded_at(&self) -> Option<DateTime<Utc>> {
        self.responded_at_unix
            .and_then(|t| DateTime::<Utc>::from_timestamp(t, 0))
    }

    /// Canonical byte payload that `signature_b64` is computed over.
    ///
    /// Stable format (do NOT reorder fields):
    ///
    /// ```text
    /// A3NET-CONTACT-REQ-v1
    /// request_id={request_id}
    /// from={from_user_id}
    /// to={to_user_id}
    /// message={message}
    /// created_at_unix={created_at_unix}
    /// ```
    ///
    /// Verifiers re-build this string from the persisted record and
    /// the live `signature_b64`, then run Ed25519. Adding a new field
    /// here is a hard fork — bump `A3NET-CONTACT-REQ-v1` to `v2` to
    /// signal the new schema.
    pub fn signature_payload(&self) -> Vec<u8> {
        format!(
            "A3NET-CONTACT-REQ-v1\nrequest_id={}\nfrom={}\nto={}\nmessage={}\ncreated_at_unix={}\n",
            self.request_id,
            self.from_user_id,
            self.to_user_id,
            self.message,
            self.created_at_unix,
        )
        .into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_payload_is_stable_and_ordered() {
        let r = PersistedContactRequest {
            request_id: "r-1".into(),
            from_user_id: "alice".into(),
            to_user_id: "bob".into(),
            message: "hi".into(),
            status: "pending".into(),
            created_at_unix: 1_700_000_000,
            responded_at_unix: None,
            signature_b64: None,
        };
        let payload = r.signature_payload();
        let s = std::str::from_utf8(&payload).unwrap();
        assert!(s.starts_with("A3NET-CONTACT-REQ-v1\n"));
        assert!(s.contains("request_id=r-1\n"));
        assert!(s.contains("from=alice\n"));
        assert!(s.contains("to=bob\n"));
        assert!(s.contains("message=hi\n"));
        assert!(s.contains("created_at_unix=1700000000\n"));
    }

    #[test]
    fn chrono_round_trip() {
        let r = PersistedContactRequest {
            request_id: "r-2".into(),
            from_user_id: "a".into(),
            to_user_id: "b".into(),
            message: "".into(),
            status: "pending".into(),
            created_at_unix: 1_700_000_000,
            responded_at_unix: Some(1_700_000_500),
            signature_b64: None,
        };
        assert_eq!(r.created_at().timestamp(), 1_700_000_000);
        assert_eq!(r.responded_at().unwrap().timestamp(), 1_700_000_500);
    }
}