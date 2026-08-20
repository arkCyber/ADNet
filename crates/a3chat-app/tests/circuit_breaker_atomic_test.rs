// Simple integration test for atomic circuit breaker
#[cfg(feature = "iroh")]
use std::sync::Arc;
#[cfg(feature = "iroh")]
use std::thread;
#[cfg(feature = "iroh")]
use std::time::Duration;

#[cfg(feature = "iroh")]
use a3chat_app::group_sync_service::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState};

#[cfg(all(test, feature = "iroh"))]
mod circuit_breaker_atomic_tests {
    use super::*;

    #[test]
    fn test_atomic_basic_state() {
        let breaker = CircuitBreaker::new();
        assert_eq!(breaker.state(), CircuitState::Closed);
        assert!(breaker.allow_request());
        println!("✅ Basic state test passed");
    }

    #[test]
    fn test_atomic_failure_counting() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            success_threshold: 2,
            timeout: Duration::from_secs(60),
        };
        let breaker = CircuitBreaker::with_config(config);

        breaker.record_failure();
        assert_eq!(breaker.consecutive_failures(), 1);
        assert_eq!(breaker.state(), CircuitState::Closed);

        breaker.record_failure();
        assert_eq!(breaker.consecutive_failures(), 2);
        assert_eq!(breaker.state(), CircuitState::Closed);

        breaker.record_failure();
        assert_eq!(breaker.consecutive_failures(), 3);
        assert_eq!(breaker.state(), CircuitState::Open);
        println!("✅ Failure counting test passed");
    }

    #[test]
    fn test_atomic_concurrent_access() {
        let breaker = Arc::new(CircuitBreaker::new());
        let mut handles = vec![];

        // Spawn multiple threads to record failures
        for _ in 0..5 {
            let breaker_clone = breaker.clone();
            let handle = thread::spawn(move || {
                breaker_clone.record_failure();
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // Should have 5 failures recorded
        assert_eq!(breaker.consecutive_failures(), 5);
        assert_eq!(breaker.state(), CircuitState::Open);
        println!("✅ Concurrent access test passed");
    }

    #[test]
    fn test_atomic_reset() {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            success_threshold: 2,
            timeout: Duration::from_secs(60),
        };
        let breaker = CircuitBreaker::with_config(config);

        breaker.record_failure();
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Open);

        breaker.reset();
        assert_eq!(breaker.state(), CircuitState::Closed);
        assert_eq!(breaker.consecutive_failures(), 0);
        println!("✅ Reset test passed");
    }
}
