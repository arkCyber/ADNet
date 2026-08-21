//! Policy layer for the mailbox.
//!
//! Three orthogonal policy axes live here:
//!
//! - [`SizePolicy`]: per-envelope byte cap. Enforced at the HTTP
//!   boundary (before signature check) so a single oversized request
//!   can't waste cycles inside the validator.
//! - [`QuotaPolicy`]: per-recipient message-count and total-bytes
//!   caps. Enforced just before insert so the caller can size their
//!   bursts to the actual headroom.
//! - [`TtlPolicy`]: per-message lifetime. The server's background
//!   sweeper purges expired envelopes via
//!   [`crate::storage::MailboxStore::purge_expired`].
//! - [`RetentionPolicy`]: per-recipient TTL overrides and bulk
//!   retention settings. Enables operator-grade control over how long
//!   specific users' messages are kept (premium tier: longer TTL).


use serde::{Deserialize, Serialize};

use crate::error::{MailboxError, MailboxResult};
use crate::storage::StoredEnvelope;

/// Size policy — single per-envelope byte cap.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SizePolicy {
    /// Maximum envelope size in bytes (including ciphertext and
    /// signature header).
    pub max_envelope_bytes: usize,
}

impl SizePolicy {
    /// Build a policy with the given byte cap.
    pub fn new(max_envelope_bytes: usize) -> Self {
        Self { max_envelope_bytes }
    }

    /// Check that `size` does not exceed the configured cap.
    pub fn check(&self, env: &StoredEnvelope) -> MailboxResult<()> {
        let size = env.wire_size() as usize;
        if size > self.max_envelope_bytes {
            return Err(MailboxError::EnvelopeTooLarge {
                size,
                max: self.max_envelope_bytes,
            });
        }
        Ok(())
    }
}

/// Quota policy — per-recipient message-count and total-bytes caps.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct QuotaPolicy {
    pub max_inflight_per_user: usize,
    pub max_total_bytes_per_user: u64,
}

/// Per-call quota check input.
#[derive(Debug, Clone, Copy)]
pub struct QuotaCheck {
    pub current_message_count: usize,
    pub current_total_bytes: u64,
    pub incoming_envelope_bytes: u64,
}

/// Per-call quota check outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaDecision {
    Accept,
    Reject { reason: &'static str },
}

impl QuotaPolicy {
    /// Construct a policy with the given caps.
    pub fn new(max_inflight_per_user: usize, max_total_bytes_per_user: u64) -> Self {
        Self {
            max_inflight_per_user,
            max_total_bytes_per_user,
        }
    }

    /// Decide whether an incoming envelope is allowed under the
    /// current [`QuotaUsage`].
    ///
    /// `Allow when the post-insert state would still be within caps.`
    /// Both checks happen against the *current* state plus the
    /// incoming envelope, so a single envelope that pushes the count
    /// from `max_inflight_per_user - 1` to `max_inflight_per_user` is
    /// accepted.
    pub fn check(&self, c: QuotaCheck) -> QuotaDecision {
        if c.current_message_count + 1 > self.max_inflight_per_user {
            return QuotaDecision::Reject {
                reason: "inflight message count exceeded",
            };
        }
        // Use saturating add so an absurd `incoming_envelope_bytes`
        // (or a buggy caller) doesn't wrap around.
        let next_bytes = c.current_total_bytes.saturating_add(c.incoming_envelope_bytes);
        if next_bytes > self.max_total_bytes_per_user {
            return QuotaDecision::Reject {
                reason: "total bytes exceeded",
            };
        }
        QuotaDecision::Accept
    }
}

/// TTL policy — message lifetime.
///
/// The server's background sweeper queries
/// [`crate::storage::MailboxStore::purge_expired`] periodically; the
/// TTL itself is stored *inside* each envelope (`expires_at`) so the
/// store doesn't need to know the policy at write time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TtlPolicy {
    pub default_ttl: std::time::Duration,
    /// How often the sweeper runs. The default is 5 minutes.
    pub sweep_interval: std::time::Duration,
}

impl Default for TtlPolicy {
    fn default() -> Self {
        Self {
            default_ttl: std::time::Duration::from_secs(30 * 24 * 60 * 60),
            sweep_interval: std::time::Duration::from_secs(5 * 60),
        }
    }
}

// ---------------------------------------------------------------------------
// RetentionPolicy (P3-8): per-recipient TTL override
// ---------------------------------------------------------------------------

/// Per-recipient TTL override entry. Maps a recipient address to a
/// custom TTL. Override takes precedence over [`TtlPolicy::default_ttl`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RecipientTtlOverride {
    /// Recipient address (EIP-55 checksummed).
    pub recipient: String,
    /// TTL in seconds for this recipient's messages.
    pub ttl_secs: u64,
    /// Optional Unix timestamp when this override expires (auto-cleanup).
    /// `None` means permanent for the duration of the sweeper run.
    pub expires_at_unix: Option<i64>,
    /// Who set this override (operator address or "billing:tier:N").
    pub source: String,
}

/// Retention policy — per-recipient TTL override and global minimum TTL.
///
/// This enables:
/// - **Premium users**: longer TTL (e.g. 90 days instead of 30).
/// - **Premium tier integration**: when `billing` is enabled, the operator
///   can call [`RetentionPolicy::set_recipient_ttl`] after pledge verification
///   to upgrade a user's TTL dynamically.
/// - **Global floor**: `min_ttl_secs` prevents accidental data loss by
///   enforcing a minimum TTL even when the server config changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RetentionPolicy {
    /// Overrides per recipient address (checksummed). O(1) lookup.
    overrides: std::collections::HashMap<String, RecipientTtlOverride>,
    /// Global minimum TTL in seconds. Individual overrides cannot go below
    /// this floor. Default: 1 day.
    pub min_ttl_secs: u64,
    /// Global maximum TTL in seconds. Caps all overrides. Default: 90 days.
    pub max_ttl_secs: u64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            overrides: std::collections::HashMap::new(),
            min_ttl_secs: 24 * 60 * 60,
            max_ttl_secs: 90 * 24 * 60 * 60,
        }
    }
}

impl RetentionPolicy {
    /// Create a new empty retention policy with the given floor and ceiling.
    pub fn new(min_ttl_secs: u64, max_ttl_secs: u64) -> Self {
        Self { overrides: std::collections::HashMap::new(), min_ttl_secs, max_ttl_secs }
    }

    /// Return the effective TTL for `recipient_id`, applying the override
    /// if present and clamping to `[min_ttl_secs, max_ttl_secs]`.
    pub fn effective_ttl(&self, recipient_id: &str, default_ttl: std::time::Duration) -> std::time::Duration {
        if let Some(override_) = self.overrides.get(recipient_id) {
            let ttl_secs = override_.ttl_secs;
            // Clamp to global bounds.
            if ttl_secs < self.min_ttl_secs {
                return std::time::Duration::from_secs(self.min_ttl_secs);
            }
            if ttl_secs > self.max_ttl_secs {
                return std::time::Duration::from_secs(self.max_ttl_secs);
            }
            return std::time::Duration::from_secs(ttl_secs);
        }
        // No override: use the server's default, also clamped.
        let default_secs = default_ttl.as_secs();
        if default_secs < self.min_ttl_secs {
            return std::time::Duration::from_secs(self.min_ttl_secs);
        }
        if default_secs > self.max_ttl_secs {
            return std::time::Duration::from_secs(self.max_ttl_secs);
        }
        default_ttl
    }

    /// Set or update a per-recipient TTL override.
    /// `source` describes who granted the override.
    pub fn set_recipient_ttl(&mut self, mut override_: RecipientTtlOverride) {
        let ttl = override_.ttl_secs;
        let ttl = ttl.clamp(self.min_ttl_secs, self.max_ttl_secs);
        override_.ttl_secs = ttl;
        self.overrides.insert(override_.recipient.clone(), override_);
    }

    /// Remove a per-recipient TTL override (reverts to global default).
    pub fn remove_recipient_ttl(&mut self, recipient_id: &str) -> bool {
        self.overrides.remove(recipient_id).is_some()
    }

    /// Get a snapshot of all overrides.
    pub fn overrides(&self) -> impl Iterator<Item = &RecipientTtlOverride> {
        self.overrides.values()
    }

    /// Number of active overrides.
    pub fn override_count(&self) -> usize {
        self.overrides.len()
    }

    /// Remove overrides that have expired (based on `expires_at_unix`).
    /// Returns the number of removed entries.
    pub fn purge_expired_overrides(&mut self, now_unix: i64) -> usize {
        let before = self.overrides.len();
        self.overrides.retain(|_, v| {
            v.expires_at_unix.is_none_or(|exp| exp > now_unix)
        });
        before - self.overrides.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn envelope(size: usize) -> StoredEnvelope {
        StoredEnvelope {
            sender_id: "0x0000000000000000000000000000000000000001".into(),
            recipient_id: "0x0000000000000000000000000000000000000002".into(),
            msg_id: "a".repeat(64),
            ciphertext: vec![0u8; size],
            sender_signature: vec![0u8; 65],
            sequence: 0,
            queued_at: Utc::now(),
            expires_at: Utc::now() + Duration::days(7),
        }
    }

    #[test]
    fn size_policy_accepts_under_cap() {
        let p = SizePolicy::new(1024);
        let e = envelope(100);
        assert!(p.check(&e).is_ok());
    }

    #[test]
    fn size_policy_rejects_over_cap() {
        let p = SizePolicy::new(1024);
        let e = envelope(2000);
        let r = p.check(&e);
        assert!(matches!(r, Err(MailboxError::EnvelopeTooLarge { .. })));
    }

    #[test]
    fn quota_policy_accepts_within_caps() {
        let p = QuotaPolicy::new(100, 1_000_000);
        let c = QuotaCheck {
            current_message_count: 99,
            current_total_bytes: 999_000,
            incoming_envelope_bytes: 500,
        };
        assert_eq!(p.check(c), QuotaDecision::Accept);
    }

    #[test]
    fn quota_policy_rejects_when_count_full() {
        let p = QuotaPolicy::new(100, 1_000_000);
        let c = QuotaCheck {
            current_message_count: 100,
            current_total_bytes: 0,
            incoming_envelope_bytes: 1,
        };
        assert!(matches!(p.check(c), QuotaDecision::Reject { .. }));
    }

    #[test]
    fn quota_policy_rejects_when_bytes_over() {
        let p = QuotaPolicy::new(100, 1_000_000);
        let c = QuotaCheck {
            current_message_count: 0,
            current_total_bytes: 999_999,
            incoming_envelope_bytes: 2,
        };
        assert!(matches!(p.check(c), QuotaDecision::Reject { .. }));
    }

    #[test]
    fn quota_policy_saturates_on_overflow() {
        let p = QuotaPolicy::new(100, 1_000);
        let c = QuotaCheck {
            current_message_count: 0,
            current_total_bytes: u64::MAX - 1,
            incoming_envelope_bytes: u64::MAX,
        };
        // Should reject (after saturating add == u64::MAX > 1000).
        assert!(matches!(p.check(c), QuotaDecision::Reject { .. }));
    }

    // RetentionPolicy tests (P3-8)

    #[test]
    fn retention_policy_no_override_returns_default() {
        let rp = RetentionPolicy::default();
        let default = std::time::Duration::from_secs(30 * 24 * 60 * 60);
        let ttl = rp.effective_ttl("0xABC", default);
        assert_eq!(ttl, default);
    }

    #[test]
    fn retention_policy_override_applied() {
        let mut rp = RetentionPolicy::default();
        rp.set_recipient_ttl(RecipientTtlOverride {
            recipient: "0xABC".into(),
            ttl_secs: 7 * 24 * 60 * 60,
            expires_at_unix: None,
            source: "operator".into(),
        });
        let ttl = rp.effective_ttl("0xABC", std::time::Duration::from_secs(30 * 24 * 60 * 60));
        assert_eq!(ttl, std::time::Duration::from_secs(7 * 24 * 60 * 60));
    }

    #[test]
    fn retention_policy_override_clamped_to_max() {
        let mut rp = RetentionPolicy::default();
        rp.set_recipient_ttl(RecipientTtlOverride {
            recipient: "0xABC".into(),
            ttl_secs: 200 * 24 * 60 * 60,
            expires_at_unix: None,
            source: "operator".into(),
        });
        let ttl = rp.effective_ttl("0xABC", std::time::Duration::from_secs(30 * 24 * 60 * 60));
        assert_eq!(ttl, std::time::Duration::from_secs(90 * 24 * 60 * 60));
    }

    #[test]
    fn retention_policy_override_clamped_to_min() {
        let mut rp = RetentionPolicy::default();
        rp.set_recipient_ttl(RecipientTtlOverride {
            recipient: "0xABC".into(),
            ttl_secs: 60,
            expires_at_unix: None,
            source: "operator".into(),
        });
        let ttl = rp.effective_ttl("0xABC", std::time::Duration::from_secs(30 * 24 * 60 * 60));
        assert_eq!(ttl, std::time::Duration::from_secs(24 * 60 * 60));
    }

    #[test]
    fn retention_policy_remove_override() {
        let mut rp = RetentionPolicy::default();
        rp.set_recipient_ttl(RecipientTtlOverride {
            recipient: "0xABC".into(),
            ttl_secs: 7 * 24 * 60 * 60,
            expires_at_unix: None,
            source: "operator".into(),
        });
        assert!(rp.remove_recipient_ttl("0xABC"));
        let default = std::time::Duration::from_secs(30 * 24 * 60 * 60);
        assert_eq!(rp.effective_ttl("0xABC", default), default);
    }

    #[test]
    fn retention_policy_purge_expired_overrides() {
        let mut rp = RetentionPolicy::default();
        rp.set_recipient_ttl(RecipientTtlOverride {
            recipient: "0xAAA".into(),
            ttl_secs: 7 * 24 * 60 * 60,
            expires_at_unix: Some(chrono::Utc::now().timestamp() - 1),
            source: "operator".into(),
        });
        rp.set_recipient_ttl(RecipientTtlOverride {
            recipient: "0xBBB".into(),
            ttl_secs: 7 * 24 * 60 * 60,
            expires_at_unix: Some(chrono::Utc::now().timestamp() + 3600),
            source: "operator".into(),
        });
        let removed = rp.purge_expired_overrides(chrono::Utc::now().timestamp());
        assert_eq!(removed, 1);
        assert_eq!(rp.override_count(), 1);
    }
}
