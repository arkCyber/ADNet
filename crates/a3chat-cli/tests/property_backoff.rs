//! Property-based tests for the retry/backoff policy.

use a3chat_cli::rpc_client::BackoffPolicy;

#[test]
fn backoff_is_monotonic_until_cap() {
    let p = BackoffPolicy::default();
    let mut prev = 0u128;
    for i in 1..=20u32 {
        let d = p.delay(i).as_millis();
        assert!(d >= prev, "backoff decreased at attempt {i}: {d} < {prev}");
        prev = d;
    }
}

#[test]
fn backoff_caps_at_max_ms() {
    let p = BackoffPolicy {
        base_ms: 100,
        factor: 2,
        max_ms: 1_000,
    };
    for i in 1..=50u32 {
        let d = p.delay(i).as_millis();
        assert!(d <= 1_000, "exceeded cap at attempt {i}: {d}");
    }
}

#[test]
fn backoff_zero_attempt_is_zero() {
    let p = BackoffPolicy::default();
    assert_eq!(p.delay(0).as_millis(), 0);
}

#[test]
fn backoff_saturating_factor_does_not_panic() {
    // Huge exponent — must not overflow.
    let p = BackoffPolicy {
        base_ms: 100,
        factor: 10,
        max_ms: 500,
    };
    for i in 0..=100u32 {
        let _ = p.delay(i);
    }
}