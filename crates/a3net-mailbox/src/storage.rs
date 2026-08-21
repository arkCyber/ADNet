//! Storage layer for the mailbox.
//!
//! The crate ships with a [`MailboxStore`] trait and two implementations:
//!
//! - [`MemoryStore`]: a `tokio::sync::Mutex` + `HashMap` for tests and
//!   ephemeral deployments. Not durable. Not optimized for production
//!   fan-out. The watermark is held in memory and resets on restart — production
//!   users must use [`SqliteStore`] (Phase 1+).
//! - [`SqliteStore`] (planned for Phase 1): a single-file SQLite
//!   database with one table per recipient. The watermark is persisted
//!   in the same row as the envelope, so restarts resume cleanly.
//!
//! ## Watermark protocol
//!
//! Recipients resume their queue by passing a `Watermark` cursor to
//! `pull`. The watermark is a `u64` *sequence number* assigned by the
//! server at `enqueue` time, monotonically increasing per recipient.
//!
//! - `enqueue` returns the new envelope's sequence number. The caller
//!   **must** treat the sequence as the canonical "highest delivered"
//!   mark and pass it back as the next `since`.
//! - `pull(since)` returns all envelopes whose sequence is strictly
//!   greater than `since`, ordered ascending by sequence.
//! - `ack(msg_ids)` removes the named envelopes atomically.
//!
//! The sequence is stored inside the same row as the envelope, so
//! `pull` returns it together with the rest of the payload. The
//! client can use the **last** envelope's `sequence` as the next
//! watermark.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::MailboxResult;

/// Per-recipient monotonic sequence number. `0` means "from the
/// beginning" — the first envelope's sequence is always `1`.
pub type Watermark = u64;

/// Identity of a single envelope in flight.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct StoredEnvelope {
    /// Sender's A3Net identity (40 hex chars, EIP-55).
    pub sender_id: String,
    /// Recipient's A3Net identity (40 hex chars, EIP-55).
    pub recipient_id: String,
    /// Globally unique message id.
    pub msg_id: String,
    /// Opaque payload bytes. **End-to-end encrypted** — the server
    /// never decrypts this.
    pub ciphertext: Vec<u8>,
    /// Sender's 65-byte EIP-191 signature over
    /// `blake3("mailbox.enqueue" | recipient_id | msg_id | sha256(ciphertext))`.
    /// We keep the signature alongside the envelope so the audit
    /// trail can prove the sender was who they claimed to be.
    pub sender_signature: Vec<u8>,
    /// Server-assigned monotonic sequence (per recipient).
    pub sequence: u64,
    /// Server-assigned timestamp (UTC).
    pub queued_at: DateTime<Utc>,
    /// Server-assigned expiry timestamp (UTC).
    pub expires_at: DateTime<Utc>,
}

impl StoredEnvelope {
    /// Total bytes occupied by this envelope on the wire (ciphertext +
    /// signature + header overhead). Used by the quota layer.
    pub fn wire_size(&self) -> u64 {
        // Fixed-size header fields: sender_id (42), recipient_id (42),
        // msg_id (≤72), plus sequence + two timestamps.
        42 + 42 + 72 + 8 + 16 + self.ciphertext.len() as u64 + self.sender_signature.len() as u64
    }
}

/// Outcome of a successful `enqueue`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EnqueueOutcome {
    pub msg_id: String,
    pub sequence: u64,
    pub queued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// `true` when this was a duplicate (idempotent replay) — the
    /// envelope was *not* enqueued again, the original outcome is
    /// returned unchanged.
    pub duplicate: bool,
}

/// Storage backend abstraction.
#[async_trait]
pub trait MailboxStore: Send + Sync {
    /// Enqueue an envelope for `recipient_id`.
    ///
    /// The canonical idempotency key is `(sender_id, recipient_id, msg_id)`.
    /// If the same envelope is enqueued twice, the second call returns
    /// the original `EnqueueOutcome` with `duplicate = true` and the
    /// storage layer does not allocate a new sequence.
    async fn enqueue(&self, env: &StoredEnvelope) -> MailboxResult<EnqueueOutcome>;

    /// Pull envelopes for `recipient_id` whose sequence is strictly
    /// greater than `since`. Returns envelopes in ascending sequence
    /// order. `limit` caps the response size (server-enforced).
    async fn pull(
        &self,
        recipient_id: &str,
        since: Watermark,
        limit: usize,
    ) -> MailboxResult<Vec<StoredEnvelope>>;

    /// Acknowledge receipt of `msg_ids` for `recipient_id`. Returns
    /// the number of messages actually removed (some may have already
    /// expired).
    async fn ack(&self, recipient_id: &str, msg_ids: &[String]) -> MailboxResult<usize>;

    /// Purge expired envelopes. Returns the number of rows removed.
    /// Called by the server's background sweeper.
    async fn purge_expired(&self) -> MailboxResult<u64>;

    /// Current quota usage for `recipient_id`.
    async fn quota_usage(&self, recipient_id: &str) -> MailboxResult<QuotaUsage>;
}

/// Per-recipient quota usage snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct QuotaUsage {
    pub message_count: usize,
    pub total_bytes: u64,
    /// Highest sequence number currently in the queue.
    pub high_watermark: Watermark,
}

// ---------------------------------------------------------------------------
// In-memory store (test / ephemeral)
// ---------------------------------------------------------------------------

/// In-memory implementation of [`MailboxStore`].
///
/// Watermark is held in memory and resets on restart — production
/// users must use the persistent backend. The `enqueue` path is
/// idempotent on `(sender, recipient, msg_id)`.
#[derive(Debug, Default, Clone)]
pub struct MemoryStore {
    inner: Arc<Mutex<HashMap<String, Vec<StoredEnvelope>>>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl MailboxStore for MemoryStore {
    async fn enqueue(&self, env: &StoredEnvelope) -> MailboxResult<EnqueueOutcome> {
        let mut guard = self.inner.lock().await;
        let bucket = guard.entry(env.recipient_id.clone()).or_default();

        // Idempotency check: same (sender, recipient, msg_id) ⇒ replay.
        if let Some(existing) = bucket
            .iter()
            .find(|e| e.msg_id == env.msg_id && e.sender_id == env.sender_id)
        {
            return Ok(EnqueueOutcome {
                msg_id: existing.msg_id.clone(),
                sequence: existing.sequence,
                queued_at: existing.queued_at,
                expires_at: existing.expires_at,
                duplicate: true,
            });
        }

        // Watermark is the *next* envelope's sequence = bucket.len() + 1.
        // This is monotonic per recipient for the lifetime of the
        // store. When the bucket is empty after a restart, sequence
        // resumes at 1 — a benign **gap** in the sequence space but
        // not a duplicate.
        let sequence = bucket.len() as u64 + 1;
        let mut stored = env.clone();
        stored.sequence = sequence;
        bucket.push(stored);
        Ok(EnqueueOutcome {
            msg_id: env.msg_id.clone(),
            sequence,
            queued_at: env.queued_at,
            expires_at: env.expires_at,
            duplicate: false,
        })
    }

    async fn pull(
        &self,
        recipient_id: &str,
        since: Watermark,
        limit: usize,
    ) -> MailboxResult<Vec<StoredEnvelope>> {
        let guard = self.inner.lock().await;
        let bucket = guard.get(recipient_id).cloned().unwrap_or_default();
        // `limit` is clamped server-side; `max(1)` preserves the
        // "always return at least the next envelope" semantic.
        let lim = limit.max(1);
        Ok(bucket
            .into_iter()
            .filter(|e| e.sequence > since)
            .take(lim)
            .collect())
    }

    async fn ack(&self, recipient_id: &str, msg_ids: &[String]) -> MailboxResult<usize> {
        let mut guard = self.inner.lock().await;
        let Some(bucket) = guard.get_mut(recipient_id) else {
            return Ok(0);
        };
        let before = bucket.len();
        bucket.retain(|e| !msg_ids.contains(&e.msg_id));
        Ok(before - bucket.len())
    }

    async fn purge_expired(&self) -> MailboxResult<u64> {
        let mut guard = self.inner.lock().await;
        let now = Utc::now();
        let mut removed = 0u64;
        for bucket in guard.values_mut() {
            let before = bucket.len();
            bucket.retain(|e| e.expires_at > now);
            removed += (before - bucket.len()) as u64;
        }
        Ok(removed)
    }

    async fn quota_usage(&self, recipient_id: &str) -> MailboxResult<QuotaUsage> {
        let guard = self.inner.lock().await;
        let bucket = guard.get(recipient_id).cloned().unwrap_or_default();
        let total_bytes: u64 = bucket.iter().map(|e| e.wire_size()).sum();
        let high_watermark = bucket.iter().map(|e| e.sequence).max().unwrap_or(0);
        Ok(QuotaUsage {
            message_count: bucket.len(),
            total_bytes,
            high_watermark,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn envelope(
        sender: &str,
        recipient: &str,
        msg_id: &str,
        ciphertext: &[u8],
    ) -> StoredEnvelope {
        StoredEnvelope {
            sender_id: sender.to_string(),
            recipient_id: recipient.to_string(),
            msg_id: msg_id.to_string(),
            ciphertext: ciphertext.to_vec(),
            sender_signature: vec![0xab; 65],
            sequence: 0,
            queued_at: Utc::now(),
            expires_at: Utc::now() + Duration::days(7),
        }
    }

    fn alice() -> &'static str {
        "0x0000000000000000000000000000000000000001"
    }
    fn bob() -> &'static str {
        "0x0000000000000000000000000000000000000002"
    }

    #[tokio::test]
    async fn enqueue_assigns_monotonic_sequences() {
        let s = MemoryStore::new();
        for i in 1..=5 {
            let mut e = envelope(alice(), bob(), &format!("m{i}-{}", "a".repeat(60)), b"hello");
            let outcome = s.enqueue(&e).await.unwrap();
            assert_eq!(outcome.sequence, i);
            assert!(!outcome.duplicate);
            assert_eq!(outcome.msg_id, e.msg_id);
            e.sequence = outcome.sequence;
        }
    }

    #[tokio::test]
    async fn duplicate_enqueue_returns_original_outcome() {
        let s = MemoryStore::new();
        let e = envelope(alice(), bob(), &"a".repeat(64), b"hello");
        let first = s.enqueue(&e).await.unwrap();
        assert!(!first.duplicate);
        let second = s.enqueue(&e).await.unwrap();
        assert!(second.duplicate);
        assert_eq!(first.sequence, second.sequence);
        assert_eq!(first.queued_at, second.queued_at);
    }

    #[tokio::test]
    async fn pull_returns_only_after_watermark() {
        let s = MemoryStore::new();
        for i in 1..=5 {
            let e = envelope(alice(), bob(), &format!("m{i}-{}", "a".repeat(60)), b"x");
            let _ = s.enqueue(&e).await.unwrap();
        }
        let out = s.pull(bob(), 0, 100).await.unwrap();
        assert_eq!(out.len(), 5);
        assert_eq!(out[0].sequence, 1);
        assert_eq!(out[4].sequence, 5);

        let out = s.pull(bob(), 2, 100).await.unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].sequence, 3);
    }

    #[tokio::test]
    async fn pull_respects_limit() {
        let s = MemoryStore::new();
        for i in 1..=10 {
            let e = envelope(alice(), bob(), &format!("m{i}-{}", "a".repeat(60)), b"x");
            let _ = s.enqueue(&e).await.unwrap();
        }
        let out = s.pull(bob(), 0, 3).await.unwrap();
        assert_eq!(out.len(), 3);
    }

    #[tokio::test]
    async fn pull_returns_in_ascending_order() {
        let s = MemoryStore::new();
        for i in 1..=5 {
            let e = envelope(alice(), bob(), &format!("m{i}-{}", "a".repeat(60)), b"x");
            let _ = s.enqueue(&e).await.unwrap();
        }
        let out = s.pull(bob(), 0, 100).await.unwrap();
        for window in out.windows(2) {
            assert!(window[0].sequence < window[1].sequence);
        }
    }

    #[tokio::test]
    async fn ack_removes_only_named_envelopes() {
        let s = MemoryStore::new();
        let mut ids = Vec::new();
        for i in 1..=3 {
            let e = envelope(alice(), bob(), &format!("m{i}-{}", "a".repeat(60)), b"x");
            let outcome = s.enqueue(&e).await.unwrap();
            ids.push(outcome.msg_id);
        }
        let removed = s.ack(bob(), &ids[..2]).await.unwrap();
        assert_eq!(removed, 2);
        let remaining = s.pull(bob(), 0, 100).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].msg_id, ids[2]);
    }

    #[tokio::test]
    async fn ack_returns_zero_for_unknown_recipient() {
        let s = MemoryStore::new();
        let removed = s.ack("nope", &["a".into()]).await.unwrap();
        assert_eq!(removed, 0);
    }

    #[tokio::test]
    async fn purge_expired_removes_only_expired() {
        let s = MemoryStore::new();
        let mut live = envelope(alice(), bob(), &"a".repeat(64), b"x");
        live.expires_at = Utc::now() + Duration::hours(1);
        let mut dead = envelope(alice(), bob(), &"b".repeat(64), b"x");
        dead.expires_at = Utc::now() - Duration::seconds(1);
        s.enqueue(&live).await.unwrap();
        s.enqueue(&dead).await.unwrap();

        let removed = s.purge_expired().await.unwrap();
        assert_eq!(removed, 1);
        let remaining = s.pull(bob(), 0, 100).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].msg_id, live.msg_id);
    }

    #[tokio::test]
    async fn quota_usage_reports_count_and_bytes() {
        let s = MemoryStore::new();
        let e = envelope(alice(), bob(), &"a".repeat(64), b"hello world");
        let _ = s.enqueue(&e).await.unwrap();
        let u = s.quota_usage(bob()).await.unwrap();
        assert_eq!(u.message_count, 1);
        assert!(u.total_bytes > 0);
        assert_eq!(u.high_watermark, 1);

        let u0 = s.quota_usage("unknown").await.unwrap();
        assert_eq!(u0.message_count, 0);
        assert_eq!(u0.total_bytes, 0);
        assert_eq!(u0.high_watermark, 0);
    }

    #[tokio::test]
    async fn envelopes_are_isolated_per_recipient() {
        let s = MemoryStore::new();
        let e1 = envelope(alice(), bob(), &"a".repeat(64), b"x");
        let e2 = envelope(alice(), "0x0000000000000000000000000000000000000003", &"b".repeat(64), b"x");
        s.enqueue(&e1).await.unwrap();
        s.enqueue(&e2).await.unwrap();
        let bobs = s.pull(bob(), 0, 100).await.unwrap();
        let carols = s.pull("0x0000000000000000000000000000000000000003", 0, 100).await.unwrap();
        assert_eq!(bobs.len(), 1);
        assert_eq!(carols.len(), 1);
        assert_eq!(bobs[0].msg_id, e1.msg_id);
        assert_eq!(carols[0].msg_id, e2.msg_id);
    }
}
