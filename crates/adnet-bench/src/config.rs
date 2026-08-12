// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Benchmark configuration and presets.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Global benchmark configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchConfig {
    /// Number of iterations for each benchmark.
    pub sample_size: usize,
    /// Warm-up time before measurement.
    pub warm_up_time: Duration,
    /// Measurement time per iteration.
    pub measurement_time: Duration,
    /// Noise threshold (percentage).
    pub noise_threshold: f64,
    /// Number of threads for parallel benchmarks.
    pub threads: usize,
    /// Enable HTML report generation.
    pub html_report: bool,
    /// Verbose output.
    pub verbose: bool,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            sample_size: 100,
            warm_up_time: Duration::from_secs(3),
            measurement_time: Duration::from_secs(10),
            noise_threshold: 0.02,
            threads: num_cpus::get(),
            html_report: true,
            verbose: false,
        }
    }
}

impl BenchConfig {
    /// Create a fast config for CI (fewer samples, shorter times).
    pub fn ci() -> Self {
        Self {
            sample_size: 10,
            warm_up_time: Duration::from_secs(1),
            measurement_time: Duration::from_secs(3),
            noise_threshold: 0.05,
            threads: 1,
            html_report: false,
            verbose: false,
        }
    }

    /// Create a thorough config for local development.
    pub fn thorough() -> Self {
        Self {
            sample_size: 200,
            warm_up_time: Duration::from_secs(5),
            measurement_time: Duration::from_secs(30),
            noise_threshold: 0.01,
            threads: num_cpus::get(),
            html_report: true,
            verbose: true,
        }
    }
}
