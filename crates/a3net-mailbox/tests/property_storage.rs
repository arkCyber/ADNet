//! Property tests for the mailbox.
//!
//! Mirrors `a3chat-app/tests/property_storage.rs`. We exercise the
//! `MemoryStore` because the property is "the storage layer is
//! deterministic" — switching to `SqliteStore` later requires the
//! same properties to hold.

use a3net_mailbox::storage::{MailboxStore, MemoryStore, QuotaUsage, StoredEnvelope};
use a3net_mailbox::MailboxError;
use chrono::{Duration, Utc};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use rand::Rng;

/// Strategy: any 64-char hex string (canonical msg_id).
fn arb_msg_id() -> impl Strategy<Value = String> {
    prop::string::string_regex("[0-9a-f]{64}").unwrap()
}

/// Strategy: any 42-char recipient id (EIP-55-style).
fn arb_recipient() -> impl Strategy<Value = String> {
    prop::string::string_regex("0x[0-9a-fA-F]{40}").unwrap()
}

/// Strategy: any sender id — same shape as recipient.
fn arb_sender() -> impl Strategy<Value = String> {
    arb_recipient()
}

/// Strategy: a fresh envelope with random sender / recipient / msg_id
/// and a small ciphertext.
fn arb_envelope() -> impl Strategy<Value = StoredEnvelope> {
    (
        arb_sender(),
        arb_recipient(),
        arb_msg_id(),
        prop::collection::vec(any::<u8>(), 0..256),
    )
        .prop_map(|(sender, recipient, msg_id, ciphertext)| StoredEnvelope {
            sender_id: sender,
            recipient_id: recipient,
            msg_id,
            ciphertext,
            sender_signature: vec![0xab; 65],
            sequence: 0,
            queued_at: Utc::now(),
            expires_at: Utc::now() + Duration::days(7),
        })
}

/// Helper: run an async block inside a tokio runtime and convert
/// `prop_assert!` failures into `TestCaseError` so `proptest!`
/// can roll them up.
fn run<F>(fut: F) -> Result<(), TestCaseError>
where
    F: std::future::Future<Output = Result<(), TestCaseError>>,
{
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(fut)
}

proptest! {
    /// For any stream of enqueues to a single recipient, the assigned
    /// sequence numbers are 1, 2, 3, …, n in insertion order.
    #[allow(clippy::redundant_pattern_matching)]
    #[test]
    fn prop_sequences_are_dense_and_monotonic(
        envelopes in prop::collection::vec(arb_envelope(), 1..50),
    )
    {
        let recipient = format!("0x{:040x}", 0xdead_beefu64);
        let s = MemoryStore::new();
        let mut seq: u64 = 1;
        run(async move {
            for mut e in envelopes {
                e.recipient_id = recipient.clone();
                let out = s.enqueue(&e).await.unwrap();
                prop_assert_eq!(out.sequence, seq);
                seq = seq.saturating_add(1);
            }
            Ok(())
        })?;
    }

    /// For any envelope, enqueueing it twice yields the same sequence
    /// number and `duplicate = true` on the second call.
    #[test]
    fn prop_duplicate_enqueue_is_idempotent(
        sender in arb_sender(),
        recipient in arb_recipient(),
        msg_id in arb_msg_id(),
        ciphertext in prop::collection::vec(any::<u8>(), 0..256),
    )
    {
        let s = MemoryStore::new();
        let env = StoredEnvelope {
            sender_id: sender,
            recipient_id: recipient,
            msg_id,
            ciphertext,
            sender_signature: vec![0xab; 65],
            sequence: 0,
            queued_at: Utc::now(),
            expires_at: Utc::now() + Duration::days(7),
        };
        run(async move {
            let first = s.enqueue(&env).await.unwrap();
            prop_assert!(!first.duplicate);
            let second = s.enqueue(&env).await.unwrap();
            prop_assert!(second.duplicate);
            prop_assert_eq!(first.sequence, second.sequence);
            prop_assert_eq!(first.queued_at, second.queued_at);
            Ok(())
        })?;
    }

    /// For any set of envelopes plus any `since` cursor, the
    /// `pull(since, ∞)` output is exactly the envelopes whose
    /// sequence is strictly greater than `since`, sorted ascending.
    #[test]
    fn prop_pull_respects_watermark_and_order(
        envelopes in prop::collection::vec(arb_envelope(), 1..30),
        since in 0u64..50,
    )
    {
        let recipient = format!("0x{:040x}", 0xcafe_babeu64);
        let s = MemoryStore::new();
        run(async move {
            let mut n = 0u64;
            for mut e in envelopes {
                e.recipient_id = recipient.clone();
                let _ = s.enqueue(&e).await.unwrap();
                n += 1;
            }
            let out = s.pull(&recipient, since, 1000).await.unwrap();
            let observed: Vec<u64> = out.iter().map(|e| e.sequence).collect();
            let expected: Vec<u64> = ((since + 1)..=n).collect();
            prop_assert_eq!(observed, expected);
            Ok(())
        })?;
    }

    /// `pull(limit)` never returns more than `limit` envelopes.
    #[test]
    fn prop_pull_respects_limit(
        envelopes in prop::collection::vec(arb_envelope(), 1..50),
        limit in 1usize..10,
    )
    {
        let recipient = format!("0x{:040x}", 0x1234_5678u64);
        let s = MemoryStore::new();
        run(async move {
            for mut e in envelopes {
                e.recipient_id = recipient.clone();
                let _ = s.enqueue(&e).await.unwrap();
            }
            let out = s.pull(&recipient, 0, limit).await.unwrap();
            prop_assert!(out.len() <= limit);
            Ok(())
        })?;
    }

    /// Acking a subset of envelopes removes exactly those envelopes
    /// and leaves the rest untouched.
    #[test]
    fn prop_ack_removes_only_named_envelopes(
        envelopes in prop::collection::vec(arb_envelope(), 1..30),
        keep_indices in prop::collection::btree_set(0usize..30, 0..10),
    )
    {
        let recipient = format!("0x{:040x}", 0xface_5555u64);
        let s = MemoryStore::new();
        run(async move {
            let mut ids = Vec::new();
            for mut e in envelopes {
                e.recipient_id = recipient.clone();
                let out = s.enqueue(&e).await.unwrap();
                ids.push(out.msg_id);
            }
            let ack_ids: Vec<_> = ids.iter().enumerate()
                .filter(|(i, _)| !keep_indices.contains(i))
                .map(|(_, id)| id.clone())
                .collect();
            let removed = s.ack(&recipient, &ack_ids).await.unwrap();
            prop_assert_eq!(removed, ack_ids.len());
            let remaining = s.pull(&recipient, 0, 1000).await.unwrap();
            prop_assert_eq!(remaining.len(), ids.len() - ack_ids.len());
            let mut all_ok = true;
            for r in &remaining {
                if ack_ids.contains(&r.msg_id) {
                    all_ok = false;
                }
            }
            prop_assert!(all_ok);
            Ok(())
        })?;
    }

    /// `quota_usage` always agrees with the live state.
    #[test]
    fn prop_quota_usage_tracks_state(
        envelopes in prop::collection::vec(arb_envelope(), 0..20),
    )
    {
        let recipient = format!("0x{:040x}", 0xabcd_1234u64);
        let s = MemoryStore::new();
        run(async move {
            for mut e in envelopes {
                e.recipient_id = recipient.clone();
                let _ = s.enqueue(&e).await.unwrap();
            }
            let u: QuotaUsage = s.quota_usage(&recipient).await.unwrap();
            let live = s.pull(&recipient, 0, 100_000).await.unwrap();
            prop_assert_eq!(u.message_count, live.len());
            let total: u64 = live.iter().map(|e| e.wire_size()).sum();
            prop_assert_eq!(u.total_bytes, total);
            Ok(())
        })?;
    }

    /// `purge_expired` removes exactly the envelopes whose
    /// `expires_at` is in the past.
    #[test]
    fn prop_purge_expired_is_accurate(
        envelopes in prop::collection::vec(arb_envelope(), 0..30),
        expired_ratio in 0.0f64..1.0,
    )
    {
        let recipient = format!("0x{:040x}", 0xbeef_deadu64);
        let s = MemoryStore::new();
        run(async move {
            let cutoff = Utc::now();
            let mut expired = 0usize;
            let mut rng = rand::thread_rng();
            for (mut e, do_expire) in envelopes.into_iter().zip(
                std::iter::repeat_with(|| rng.r#gen::<f64>() < expired_ratio)
            ) {
                e.recipient_id = recipient.clone();
                if do_expire {
                    e.expires_at = cutoff - Duration::seconds(1);
                    expired += 1;
                } else {
                    e.expires_at = cutoff + Duration::seconds(60);
                }
                let _ = s.enqueue(&e).await.unwrap();
            }
            let removed = s.purge_expired().await.unwrap();
            prop_assert_eq!(removed as usize, expired);
            let remaining = s.pull(&recipient, 0, 100_000).await.unwrap();
            let mut all_ok = true;
            for r in remaining {
                if r.expires_at <= cutoff {
                    all_ok = false;
                }
            }
            prop_assert!(all_ok);
            Ok(())
        })?;
    }
}

#[tokio::test]
async fn duplicate_keyed_on_sender_too() {
    use a3net_mailbox::storage::MailboxStore;
    let s = MemoryStore::new();
    let recipient = "0x0000000000000000000000000000000000000001";
    let mut env = StoredEnvelope {
        sender_id: "0x0000000000000000000000000000000000000002".into(),
        recipient_id: recipient.into(),
        msg_id: "a".repeat(64),
        ciphertext: vec![1; 64],
        sender_signature: vec![0xab; 65],
        sequence: 0,
        queued_at: Utc::now(),
        expires_at: Utc::now() + Duration::days(7),
    };
    let first = s.enqueue(&env).await.unwrap();
    assert!(!first.duplicate);

    // Same msg_id, *different* sender — should *not* be a duplicate.
    env.sender_id = "0x0000000000000000000000000000000000000003".into();
    let second = s.enqueue(&env).await.unwrap();
    assert!(!second.duplicate);
    assert_ne!(first.sequence, second.sequence);
}

#[tokio::test]
async fn error_class_examples_hold() {
    let e = MailboxError::QuotaExceeded("inflight".into());
    assert_eq!(e.error_class(), a3net_mailbox::MailboxErrorClass::Security);
    let e = MailboxError::Transport("dns".into());
    assert_eq!(e.error_class(), a3net_mailbox::MailboxErrorClass::Transient);
    let e = MailboxError::Storage("disk".into());
    assert_eq!(e.error_class(), a3net_mailbox::MailboxErrorClass::Internal);
}
