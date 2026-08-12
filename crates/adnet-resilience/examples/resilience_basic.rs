//! Tiny example: drive `retry_with_backoff` against a flaky counter and
//! show how a `CircuitBreaker` flips to `Open` after too many failures.
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-resilience --example resilience_basic
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use adnet_resilience::{
    CircuitBreaker, CircuitBreakerConfig, CircuitState, RetryPolicy, retry_with_backoff,
};

#[tokio::main]
async fn main() {
    // 1. Retry with exponential backoff. The closure fails the first
    //    two times and succeeds on the third.
    let calls = Arc::new(AtomicU32::new(0));
    let calls_c = Arc::clone(&calls);
    let result: Result<&'static str, &'static str> = retry_with_backoff(
        move || {
            let c = Arc::clone(&calls_c);
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst) + 1;
                println!("[retry] attempt {n}");
                if n < 3 {
                    Err("transient")
                } else {
                    Ok("ok")
                }
            }
        },
        RetryPolicy::Transient.to_config(),
    )
    .await;
    println!("[retry] result: {result:?}");
    println!("[retry] total calls: {}", calls.load(Ordering::SeqCst));
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    // 2. Circuit breaker — trip after 3 failures, recover after a
    //    short timeout.
    let cb = CircuitBreaker::with_config(CircuitBreakerConfig {
        failure_threshold: 3,
        timeout: Duration::from_millis(50),
        success_threshold: 1,
        window_duration: Duration::from_secs(60),
    });

    for _ in 0..3 {
        cb.record_failure().await;
    }
    let state = cb.state().await;
    println!("[breaker] after 3 failures: state={state:?}, allow_request={}",
        cb.allow_request().await);
    assert_eq!(state, CircuitState::Open);

    // Wait for the breaker to enter Half-Open and accept a probe.
    tokio::time::sleep(Duration::from_millis(80)).await;
    let allows = cb.allow_request().await;
    let state = cb.state().await;
    println!("[breaker] after timeout: state={state:?}, allow_request={allows}");
    assert_eq!(state, CircuitState::HalfOpen);
    assert!(allows);

    // A successful probe closes the breaker.
    cb.record_success().await;
    let state = cb.state().await;
    println!("[breaker] after success: state={state:?}");
    assert_eq!(state, CircuitState::Closed);
}
