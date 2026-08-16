//! Automatic bandwidth management module.
//!
//! This module provides intelligent bandwidth management for video streaming:
//! - [`BandwidthManager`] - Main manager for automatic bandwidth adjustment
//! - [`BandwidthManagerConfig`] - Configuration options
//! - [`BandwidthManagerRunner`] - Helper for running in background
//! - [`BandwidthStats`] - Statistics about bandwidth usage
//! - [`NetworkState`] - Current network condition assessment

pub mod manager;

pub use manager::{
    BandwidthManager, BandwidthManagerConfig, BandwidthManagerRunner, BandwidthStats,
    NetworkState,
};
