//! Phase 5d: E2E integration tests for group sync.
//!
//! ## Modules
//!
//! - [`mod@e2e`] — E2E tests with real DERP relay
//! - [`mod@network_partition`] — Network partition simulation
//!
//! ## Running Tests
//!
//! ```bash
//! # Run all Phase 5d tests
//! cargo test -p a3net-chatstore --all-features --test phase5d
//!
//! # Run specific test
//! cargo test -p a3net-chatstore --all-features --test phase5d benchmark
//! ```

#![cfg(all(feature = "iroh", feature = "derp"))]

pub mod e2e;
pub mod network_partition;
