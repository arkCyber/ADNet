//! Example: config validation and editing
//!
//! Demonstrates how to:
//! - Validate config files programmatically
//! - Use the config edit workflow
//! - Handle validation errors gracefully
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-cli --example config_validator
//! ```

use adnet_cli::config::{self, AppConfig};
use anyhow::Result;
use std::fs;

fn main() -> Result<()> {
    println!("=== ADNet Config Validator Demo ===\n");

    // 1. Validate a valid config
    println!("1. Validating a correct config:");
    let valid_path = valid_config_file()?;
    match config::validate(&valid_path) {
        Ok(cfg) => {
            println!("   ✓ Config is valid");
            println!("   Data dir: {}", cfg.data_dir.display());
        }
        Err(e) => println!("   ✗ Error: {e}"),
    }

    // 2. Validate an invalid config
    println!("\n2. Validating an invalid config:");
    let invalid_path = invalid_config_file()?;
    match config::validate(&invalid_path) {
        Ok(_) => println!("   ✗ Should have failed"),
        Err(e) => {
            println!("   ✓ Expected error:");
            // Show first line of error
            let err_string = e.to_string();
            let first_line = err_string.lines().next().unwrap_or("unknown error");
            println!("   {first_line}");
        }
    }

    // 3. Config path resolution
    println!("\n3. Config path resolution:");
    println!("   Platform default path:");
    let resolved = config::resolve(None).unwrap();
    println!("      Source: {:?}", resolved.source);
    if let Some(p) = &resolved.path {
        println!("      Path: {}", p.display());
    }

    // 4. Config loading workflow
    println!("\n4. Full config loading workflow:");
    let loaded = config::load(Some(&valid_path))?;
    println!(
        "   Config loaded from: {}",
        loaded
            .source
            .path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<in-memory>".to_string())
    );
    println!("   Written template: {}", loaded.source.written_template);

    // 5. Apply changes and save
    println!("\n5. Modifying config:");
    let modified_path = temp_path("modified-config.json")?;
    config::set_value(&modified_path, "log.level", "\"debug\"")?;
    println!("   ✓ Set log.level = debug");

    config::set_value(&modified_path, "default_room", "\"research\"")?;
    println!("   ✓ Set default_room = research");

    // 6. Verify changes
    println!("\n6. Verifying changes:");
    let verified = config::validate(&modified_path)?;
    println!("   log.level: {}", verified.log.level);
    if let Some(ref room) = verified.default_room {
        println!("   default_room: {room}");
    }

    // 7. Show known keys
    println!("\n7. Known config keys (whitelist):");
    let keys = vec![
        "dataDir",
        "log.level",
        "log.format",
        "defaultRoom",
        "repl.prompt",
        "mesh.port",
        "iroh.enabled",
        "qr.ecLevel",
    ];
    for key in keys.iter().take(8) {
        println!("   - {key}");
    }
    println!("   ... and more");

    // 8. Show effective config
    println!("\n8. Effective config JSON:");
    let json = config::show_effective(&verified)?;
    for line in json.lines().take(5) {
        println!("   {line}");
    }
    println!("   ...");

    // Cleanup
    let _ = fs::remove_file(&valid_path);
    let _ = fs::remove_file(&invalid_path);
    let _ = fs::remove_file(&modified_path);

    println!("\n=== Config Validator Demo Complete ===");
    Ok(())
}

fn valid_config_file() -> Result<std::path::PathBuf> {
    let path = temp_path("valid-config.json")?;
    let content = r#"{
        "dataDir": "/tmp/adnet",
        "log": {
            "level": "info",
            "format": "compact"
        },
        "repl": {
            "prompt": "adnet> "
        }
    }"#;
    fs::write(&path, content)?;
    Ok(path)
}

fn invalid_config_file() -> Result<std::path::PathBuf> {
    let path = temp_path("invalid-config.json")?;
    let content = r#"{
        "dataDir": "/tmp/adnet",
        "log": {
            "level": "info
        }
    }"#; // Missing closing quote
    fs::write(&path, content)?;
    Ok(path)
}

fn create_modified_config() -> Result<AppConfig> {
    Ok(AppConfig {
        data_dir: std::path::PathBuf::from("/custom/data"),
        log: adnet_cli::config::LogConfig {
            level: "info".to_string(),
            format: adnet_cli::config::LogFormat::Json,
        },
        default_room: None,
        repl: adnet_cli::config::ReplConfig::default(),
        mesh: None,
        relay: None,
        gossip_validation: None,
        iroh: None,
        #[cfg(feature = "qr")]
        qr: None,
    })
}

fn temp_path(name: &str) -> Result<std::path::PathBuf> {
    let mut path = std::env::temp_dir();
    path.push(format!("adnet-validator-demo-{}", name));
    Ok(path)
}
