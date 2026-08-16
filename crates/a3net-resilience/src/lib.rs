//! `a3net-resilience` — retry, circuit breaker, cancellation, backpressure, resilient HTTP.
//!
//! Lifted from `Exodus@src-backup/.../microservice/resilience.rs` (720
//! lines) and split into focused modules:
//!
//! - [`retry`] — `retry_with_backoff`, [`RetryConfig`], [`RetryPolicy`]
//! - [`circuit_breaker`] — [`CircuitBreaker`] / [`CircuitBreakerConfig`]
//! - [`http`] — [`ResilientHttpClient`] combining both
//! - [`cancellation`] — [`CancellationScope`] / [`CancellationToken`] for coordinated shutdown
//! - [`resource`] — [`ResourceLimiter`] global + per-key concurrency caps
//!
//! This crate has **no** A3Net types — it's transport-agnostic so it can be
//! used by `a3net-mesh`, `a3net-ipc`, `a3net-relay`, and downstream apps
//! without dragging in P2P plumbing.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod cancellation;
pub mod circuit_breaker;
pub mod http;
pub mod resource;
pub mod retry;

pub use cancellation::{
    CancellationScope, CancellationToken, JoinSummary, DEFAULT_SHUTDOWN_TIMEOUT,
};
pub use circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState};
pub use http::{ResilientHttpClient, ResilientHttpConfig};
pub use resource::{
    default_peer_config, default_room_config, default_tag_config, AcquireError, LimiterMetrics,
    LimiterMetricsSnapshot, PeerLimiter, ResourceConfig, ResourceLimiter, ResourcePermit,
    RoomLimiter, TagLimiter,
};
pub use retry::{RetryConfig, RetryPolicy, retry_with_backoff};
