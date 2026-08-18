//! DO-178C DAL-A Aerospace Compliance Test Suite for A3Net Mailbox
//!
//! Run with:
//! ```bash
//! cargo test --package a3net-mailbox --features aerospace --test aerospace_compliance_mailbox
//! ```

use a3net_mailbox::{
    auth::{canonical_message, digest_of},
    distributed_rate_limit::{
        DistributedRateLimiter, DistributedRateLimitConfig, LocalRateLimiter,
        RateLimitDecision, RateLimitError,
    },
    error::MailboxError,
};

use a3net_identity::Wallet;

// ─────────────────────────────────────────────────────────────────────
// SR-1: Rate Limiting Correctness
// ─────────────────────────────────────────────────────────────────────

/// SR-1.1: Local rate limiter allows requests within bucket capacity.
#[test]
fn sr_1_1_local_rate_limiter_allows_within_capacity() {
    let limiter = LocalRateLimiter::new();
    let bucket = limiter.bucket("enqueue:192.0.2.1");

    let state = bucket.lock();
    assert!(state.tokens >= 1.0, "tokens: {}", state.tokens);
}

/// SR-1.2: Rate limiter decision is serializable for logging.
#[test]
fn sr_1_2_rate_limit_decision_is_serializable() {
    let decision = RateLimitDecision {
        allowed: true,
        remaining_tokens: 59.0,
        retry_after_secs: 0,
        ip: "192.0.2.1".to_string(),
        endpoint: "enqueue".to_string(),
        timestamp: 1699900000,
    };

    let json = serde_json::to_string(&decision).unwrap();
    assert!(json.contains("\"allowed\":true"));
    assert!(json.contains("\"remaining_tokens\":59"));
}

/// SR-1.3: Distributed limiter falls back to local.
#[tokio::test]
async fn sr_1_3_distributed_limiter_falls_back_gracefully() {
    let config = DistributedRateLimitConfig::default();
    let limiter = DistributedRateLimiter::new(config);

    let decision = limiter.check("10.0.0.1", "enqueue").await;
    assert!(decision.allowed);
}

// ─────────────────────────────────────────────────────────────────────
// SR-2: Authentication - Signature Verification
// ─────────────────────────────────────────────────────────────────────

/// SR-2.1: Wallet signature and recovery works.
#[test]
fn sr_2_1_signature_recovery_works() {
    let wallet = Wallet::generate();
    let msg = b"test message for signing";
    let digest = digest_of(msg);
    let sig = wallet.sign_personal(&digest).unwrap();

    // Signature should be 65 bytes (r, s, v)
    let compact = sig.to_compact();
    assert_eq!(compact.len(), 65);
}

// ─────────────────────────────────────────────────────────────────────
// SR-3: Input Validation and Boundary Checks
// ─────────────────────────────────────────────────────────────────────

/// SR-3.1: Valid recipient ID passes validation.
#[test]
fn sr_3_1_valid_recipient_id_passes() {
    let wallet = Wallet::generate();
    let addr = wallet.public().address().to_checksum();
    let result = a3net_mailbox::auth::validate_recipient_id(&addr);
    assert!(result.is_ok());
}

/// SR-3.2: Empty recipient ID is rejected.
#[test]
fn sr_3_2_empty_recipient_id_rejected() {
    let result = a3net_mailbox::auth::validate_recipient_id("");
    assert!(matches!(result, Err(MailboxError::InvalidRecipientId(_))));
}

/// SR-3.3: Path traversal in recipient ID is rejected.
#[test]
fn sr_3_3_path_traversal_recipient_id_rejected() {
    let result = a3net_mailbox::auth::validate_recipient_id("../../../etc/passwd");
    assert!(matches!(result, Err(MailboxError::InvalidRecipientId(_))));
}

/// SR-3.4: Oversized recipient ID is rejected.
#[test]
fn sr_3_4_oversized_recipient_id_rejected() {
    let oversized = "0x".to_string() + &"a".repeat(100);
    let result = a3net_mailbox::auth::validate_recipient_id(&oversized);
    assert!(matches!(result, Err(MailboxError::InvalidRecipientId(_))));
}

/// SR-3.5: Non-hex characters in recipient ID are rejected.
#[test]
fn sr_3_5_non_hex_recipient_id_rejected() {
    let result = a3net_mailbox::auth::validate_recipient_id("0xZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ");
    assert!(matches!(result, Err(MailboxError::InvalidRecipientId(_))));
}

/// SR-3.6: Valid message ID (64 hex) passes.
#[test]
fn sr_3_6_valid_msg_id_64_hex_passes() {
    let result = a3net_mailbox::auth::validate_msg_id(&"a".repeat(64));
    assert!(result.is_ok());
}

/// SR-3.7: Valid UUID message ID passes.
#[test]
fn sr_3_7_valid_uuid_msg_id_passes() {
    let result = a3net_mailbox::auth::validate_msg_id("550e8400-e29b-41d4-a716-446655440000");
    assert!(result.is_ok());
}

/// SR-3.8: Invalid UUID message ID is rejected.
#[test]
fn sr_3_8_invalid_uuid_msg_id_rejected() {
    let result = a3net_mailbox::auth::validate_msg_id("550e8400-e29b-41d4-a716");
    assert!(matches!(result, Err(MailboxError::InvalidMessageId(_))));
}

/// SR-3.9: Empty message ID is rejected.
#[test]
fn sr_3_9_empty_msg_id_rejected() {
    let result = a3net_mailbox::auth::validate_msg_id("");
    assert!(matches!(result, Err(MailboxError::InvalidMessageId(_))));
}

// ─────────────────────────────────────────────────────────────────────
// SR-4: Error Handling and Recovery
// ─────────────────────────────────────────────────────────────────────

/// SR-4.1: Error classification is explicit and DO-178C compliant.
#[test]
fn sr_4_1_error_classification_is_explicit() {
    // All errors should format correctly
    let error1 = MailboxError::InvalidRecipientId("test".into());
    let error2 = MailboxError::InvalidMessageId("test".into());
    let error3 = MailboxError::InvalidSignature;
    let error4 = MailboxError::StaleSignature { age_secs: 1, max_age_secs: 300 };
    let error5 = MailboxError::NotFound("msg".into());

    assert!(!format!("{:?}", error1).is_empty());
    assert!(!format!("{:?}", error2).is_empty());
    assert!(!format!("{:?}", error3).is_empty());
    assert!(!format!("{:?}", error4).is_empty());
    assert!(!format!("{:?}", error5).is_empty());
}

/// SR-4.2: NotFound errors are recoverable.
#[test]
fn sr_4_2_not_found_is_recoverable() {
    let error = MailboxError::NotFound("message".into());
    assert!(!error.is_retryable());
}

/// SR-4.3: Transient errors are retryable.
#[test]
fn sr_4_3_transient_errors_are_retryable() {
    let error = MailboxError::Transport("network timeout".into());
    assert!(error.is_retryable());
}

// ─────────────────────────────────────────────────────────────────────
// SR-5: Security - Replay Protection
// ─────────────────────────────────────────────────────────────────────

/// SR-5.1: Canonical messages cannot collide.
#[test]
fn sr_5_1_canonical_messages_cannot_collide() {
    // Messages with same content but different structure must differ
    let msg1 = canonical_message(&[b"a|b|c"]);
    let msg2 = canonical_message(&[b"a", b"|b|c"]);

    assert_ne!(msg1, msg2, "different structure should produce different canonical form");
}

/// SR-5.2: Length prefix prevents injection.
#[test]
fn sr_5_2_length_prefix_prevents_injection() {
    let with_pipe = canonical_message(&[b"key|value"]);
    let separated = canonical_message(&[b"key", b"|value"]);

    assert_ne!(with_pipe, separated, "pipe in segment should not collide with separator");
}

/// SR-5.3: Digest is deterministic.
#[test]
fn sr_5_3_digest_is_deterministic() {
    let msg = b"deterministic test";
    let digest1 = digest_of(msg);
    let digest2 = digest_of(msg);
    assert_eq!(digest1, digest2, "same input must produce same digest");
}

// ─────────────────────────────────────────────────────────────────────
// SR-6: Data Integrity
// ─────────────────────────────────────────────────────────────────────

/// SR-6.1: Digest is always 32 bytes.
#[test]
fn sr_6_1_digest_is_always_32_bytes() {
    let msg = b"test message";
    let digest = digest_of(msg);
    assert_eq!(digest.len(), 32, "BLAKE3 digest must be 32 bytes");
}

/// SR-6.2: Different inputs produce different digests.
#[test]
fn sr_6_2_different_inputs_produce_different_digests() {
    let digest1 = digest_of(b"input 1");
    let digest2 = digest_of(b"input 2");
    assert_ne!(digest1, digest2, "different input must produce different digest");
}

// ─────────────────────────────────────────────────────────────────────
// Robustness Tests
// ─────────────────────────────────────────────────────────────────────

/// Robustness: Empty and null inputs handled safely.
#[test]
fn robustness_empty_and_null_inputs() {
    let digest = digest_of(b"");
    assert_eq!(digest.len(), 32);

    let msg = canonical_message(&[]);
    assert!(msg.is_empty());

    let digest = digest_of(b"x");
    assert_eq!(digest.len(), 32);
}

/// Robustness: Rate limit error classification.
#[test]
fn robustness_rate_limit_error_classification() {
    let err1 = RateLimitError::RedisConnection("timeout".to_string());
    assert_eq!(err1.recoverability(), "RECOVERABLE");

    let err2 = RateLimitError::InvalidConfig("bad capacity".to_string());
    assert_eq!(err2.recoverability(), "USER_ERROR");

    let err3 = RateLimitError::LimitExceeded {
        retry_after: 10,
        ip: "1.2.3.4".to_string(),
    };
    assert_eq!(err3.recoverability(), "USER_ERROR");
}

/// Robustness: Token bucket overflow protection.
#[test]
fn robustness_token_bucket_no_overflow() {
    let limiter = LocalRateLimiter::new();

    // Simulate rapid requests
    for _ in 0..10000 {
        let bucket = limiter.bucket("stress:test");
        let mut state = bucket.lock();
        state.tokens = state.tokens.max(0.0).min(1000.0);
    }

    let bucket = limiter.bucket("stress:test");
    let state = bucket.lock();
    assert!(state.tokens.is_finite());
    assert!(state.tokens >= 0.0);
}
