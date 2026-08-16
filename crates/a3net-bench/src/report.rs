// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Benchmark report generation and analysis.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

/// Summary statistics for a benchmark run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkResult {
    pub name: String,
    pub group: String,
    pub mean: Duration,
    pub median: Duration,
    pub std_dev: Duration,
    pub min: Duration,
    pub max: Duration,
    pub samples: usize,
    pub throughput: Option<ThroughputMetric>,
}

/// Throughput metric (e.g., bytes/second, ops/second).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThroughputMetric {
    pub value: f64,
    pub unit: String,
}

/// Complete benchmark report for a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchReport {
    pub config: super::BenchConfig,
    pub results: Vec<BenchmarkResult>,
    pub comparisons: HashMap<String, Comparison>,
    pub generated_at: chrono::DateTime<chrono::Utc>,
}

/// Comparison with a baseline (previous run).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comparison {
    pub baseline_mean: Duration,
    pub current_mean: Duration,
    pub change_pct: f64,
    pub significant: bool,
}

impl BenchReport {
    /// Generate a markdown summary.
    pub fn markdown_summary(&self) -> String {
        let mut md = String::new();
        md.push_str("# A3Net Benchmark Report\n\n");
        md.push_str(&format!("Generated: {}\n\n", self.generated_at));
        md.push_str("## Results\n\n");

        let mut groups: HashMap<&str, Vec<&BenchmarkResult>> = HashMap::new();
        for result in &self.results {
            groups.entry(result.group.as_str()).or_default().push(result);
        }

        for (group, results) in groups {
            md.push_str(&format!("### {}\n\n", group));
            md.push_str("| Benchmark | Mean | Median | Std Dev | Throughput |\n");
            md.push_str("|-----------|------|--------|---------|------------|\n");
            for r in results {
                let throughput = r.throughput.as_ref()
                    .map(|t| format!("{:.2} {}", t.value, t.unit))
                    .unwrap_or_default();
                md.push_str(&format!(
                    "| {} | {:?} | {:?} | {:?} | {} |\n",
                    r.name, r.mean, r.median, r.std_dev, throughput
                ));
            }
            md.push_str("\n");
        }

        md.push_str("## Changes vs Baseline\n\n");
        for (name, comp) in &self.comparisons {
            let sign = if comp.change_pct > 0.0 { "+" } else { "" };
            md.push_str(&format!(
                "- {}: {} {:.1}% {}\n",
                name,
                if comp.significant { "**SIGNIFICANT**" } else { "stable" },
                comp.change_pct,
                if comp.change_pct > 0.0 { "slower" } else { "faster" }
            ));
        }

        md
    }

    /// Write report to files.
    pub fn write(&self, prefix: &str) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self).unwrap();
        std::fs::write(format!("{prefix}.json"), json)?;

        let md = self.markdown_summary();
        std::fs::write(format!("{prefix}.md"), md)?;

        Ok(())
    }
}
