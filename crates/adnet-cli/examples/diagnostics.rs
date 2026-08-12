//! Example: node diagnostics
//!
//! Demonstrates how to:
//! - Create a diagnostics snapshot of a data directory
//! - Inspect node identity and configuration
//! - Work with identity files in various formats
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-cli --example diagnostics
//! ```

use adnet_cli::diagnostics::{self, DiagnosticsSnapshot};
use anyhow::Result;
use std::fs;

fn main() -> Result<()> {
    println!("=== ADNet Diagnostics Demo ===\n");

    // 1. Create a temporary data directory with identity
    let dir = temp_dir()?;
    println!("1. Created temp data dir: {}", dir.display());

    // 2. Create a valid 32-byte identity file
    let identity_path = dir.join("identity.key");
    let identity_bytes: Vec<u8> = (0u8..32).collect();
    fs::write(&identity_path, &identity_bytes)?;
    println!("   Written 32-byte identity file");

    // 3. Generate diagnostics snapshot
    println!("\n2. Generating diagnostics snapshot...");
    let snap = diagnostics::diagnostics_snapshot(&dir)?;
    print_snapshot(&snap);

    // 4. Test with legacy hex format
    println!("\n3. Testing legacy hex identity format...");
    let legacy_dir = temp_dir()?;
    let legacy_path = legacy_dir.join("identity.key");
    let hex: String = identity_bytes.iter().map(|b| format!("{b:02x}")).collect();
    fs::write(&legacy_path, format!("{hex}\n"))?;
    let legacy_snap = diagnostics::diagnostics_snapshot(&legacy_dir)?;
    println!("   ✓ Legacy hex format parsed successfully");
    println!("   NodeId: {}...", &legacy_snap.node_id[..16]);

    // 5. Error handling demo
    println!("\n4. Error handling:");
    let missing_dir = std::env::temp_dir().join("nonexistent-adnet-dir-999");
    match diagnostics::diagnostics_snapshot(&missing_dir) {
        Ok(_) => println!("   ✗ Should have failed"),
        Err(e) => println!("   ✓ Expected error: {}", e),
    }

    // 6. Serialization demo
    println!("\n5. Snapshot serialization:");
    let json = serde_json::to_string_pretty(&snap)?;
    println!("   JSON output:");
    for line in json.lines().take(8) {
        println!("   {line}");
    }

    // 7. Compact output format
    println!("\n6. Compact output format:");
    println!("   Node ID:     {}", snap.node_id);
    println!("   Short ID:    adnet-{}", snap.node_id_short);
    println!("   Public Key:  {}", snap.public_key);
    println!("   Data Dir:    {}", snap.data_dir);
    if let Some(url) = snap.mesh_url {
        println!("   Mesh URL:    {url}");
    } else {
        println!("   Mesh URL:    (not configured)");
    }

    // Cleanup
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&legacy_dir);

    println!("\n=== Diagnostics Demo Complete ===");
    Ok(())
}

fn print_snapshot(snap: &DiagnosticsSnapshot) {
    println!("   NodeId:      {}", snap.node_id);
    println!("   Short:       adnet-{}", snap.node_id_short);
    println!("   Public Key: {}", snap.public_key);
    println!("   Data Dir:   {}", snap.data_dir);
    match &snap.mesh_url {
        Some(url) => println!("   Mesh URL:   {url}"),
        None => println!("   Mesh URL:   (none)"),
    }
}

fn temp_dir() -> Result<std::path::PathBuf> {
    let mut path = std::env::temp_dir();
    path.push(format!("adnet-diag-demo-{}", std::process::id()));
    fs::create_dir_all(&path)?;
    Ok(path)
}
