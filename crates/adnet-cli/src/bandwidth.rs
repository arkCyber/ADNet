//! Bandwidth Management CLI
//!
//! Provides command-line interface for viewing bandwidth statistics.

/// Run `adnet bandwidth [--json]`. Offline — does not require a running node.
pub fn run_bandwidth(_data_dir: &std::path::Path, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::json!({
            "global_limit_bytes_per_sec": 0,
            "per_tenant_limit_bytes": 0,
            "note": "bandwidth tracking requires a running node"
        }));
    } else {
        println!("ADNet Bandwidth");
        println!("{}", "=".repeat(50));
        println!("  global_limit_bytes_per_sec : 0 (unlimited)");
        println!("  per_tenant_limit_bytes    : 0 (unlimited)");
        println!("  (bandwidth tracking requires a running node for live stats)");
    }
    Ok(())
}
