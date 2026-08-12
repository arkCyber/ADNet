//! Example: programmatic config management
//!
//! Demonstrates how to:
//! - Load and modify config programmatically
//! - Set values using dotted keys
//! - Validate and serialize config
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-cli --example config_manager
//! ```

use adnet_cli::config::{self};
use anyhow::Result;

fn main() -> Result<()> {
    println!("=== ADNet Config Manager Demo ===\n");

    // 1. Load default config
    println!("1. Loading default config...");
    let loaded = config::load(None)?;
    println!("   data_dir: {}", loaded.config.data_dir.display());
    println!("   log level: {}", loaded.config.log.level);
    println!("   log format: {:?}", loaded.config.log.format);

    // 2. Show effective config as JSON
    println!("\n2. Effective config (JSON):");
    let json = config::show_effective(&loaded.config)?;
    // Pretty print first few lines
    for line in json.lines().take(10) {
        println!("   {line}");
    }
    println!("   ... (truncated)");

    // 3. Demonstrate config modification
    println!("\n3. Demonstrating config modifications...");

    // Create a temp config for demo
    let temp_config = tempfile_config()?;
    let path = &temp_config;

    // Set log level
    config::set_value(path, "log.level", "\"debug\"")?;
    println!("   Set log.level = debug");

    // Set a nested value
    config::set_value(path, "iroh.enabled", "true")?;
    println!("   Set iroh.enabled = true");

    // Set default room
    config::set_value(path, "defaultRoom", "\"lobby\"")?;
    println!("   Set defaultRoom = lobby");

    // 4. Read back and verify
    println!("\n4. Verifying changes:");
    let modified = config::validate(path)?;
    println!("   log.level: {}", modified.log.level);
    if let Some(iroh) = modified.iroh {
        println!("   iroh.enabled: {}", iroh.enabled);
    }
    if let Some(room) = modified.default_room {
        println!("   default_room: {room}");
    }

    // 5. Validate config file
    println!("\n5. Validating config file...");
    match config::validate(path) {
        Ok(_) => println!("   ✓ Config is valid"),
        Err(e) => println!("   ✗ Config error: {e}"),
    }

    // 6. Resolve config path
    println!("\n6. Config path resolution:");
    let resolved = config::resolve(None).unwrap();
    println!("   source: {:?}", resolved.source);
    if let Some(p) = resolved.path {
        println!("   path: {}", p.display());
    }

    println!("\n=== Config Manager Demo Complete ===");
    Ok(())
}

/// Create a temporary config file for demonstration
fn tempfile_config() -> Result<std::path::PathBuf> {
    use std::fs;
    let mut path = std::env::temp_dir();
    path.push("adnet-config-demo.json");
    let template = config::DEFAULT_CONFIG_TEMPLATE;
    fs::write(&path, template)?;
    Ok(path)
}
