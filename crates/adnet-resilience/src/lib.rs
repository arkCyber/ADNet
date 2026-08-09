//! `adnet-resilience` — retry, circuit breaker, resilient HTTP.
//!
//! Lifted from `Exodus@src-backup/.../microservice/resilience.rs` (720
//! lines) and split into focused modules:
//!
//! - [`retry`] — `retry_with_backoff`, [`RetryConfig`], [`RetryPolicy`]
//! - [`circuit_breaker`] — [`CircuitBreaker`] / [`CircuitBreakerConfig`]
//! - [`http`] — [`ResilientHttpClient`] combining both
//!
//! This crate has **no** ADNet types — it's transport-agnostic so it can be
//! used by `adnet-mesh`, `adnet-ipc`, `adnet-relay`, and downstream apps
//! without dragging in P2P plumbing.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod circuit_breaker;
pub mod http;
pub mod retry;

pub use circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState};
pub use http::{ResilientHttpClient, ResilientHttpConfig};
pub use retry::{RetryConfig, RetryPolicy, retry_with_backoff};
