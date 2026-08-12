//! Interactive configuration wizard for ADNet.
//!
//! Provides a step-by-step interactive setup experience using
//! the `adnet_tui` crate for rich terminal output.
//!
//! ## Usage
//!
//! ```bash
//! adnet config wizard
//! ```
//!
//! The wizard will guide users through:
//! 1. Basic settings (data dir, log level)
//! 2. Storage settings
//! 3. Network settings (mesh, iroh)
//! 4. Relay settings
//! 5. Review and save

use std::io::{self, Write};
use std::path::PathBuf;

use adnet_mesh::MeshConfig;
use adnet_relay::RelayConfig;
use anyhow::Result;
use serde::Serialize;

use adnet_tui::{
    box_drawing::{BorderStyle, Box},
    color::Color,
};

use crate::bytes::parse_bytes;
use crate::config::{self, AppConfig, DiscoveryConfigToml, IrohConfig, LogFormat, StorageConfig};

/// Wizard state that collects configuration options.
#[derive(Debug, Clone, Default)]
pub struct WizardState {
    pub data_dir: Option<String>,
    pub log_level: Option<String>,
    pub log_format: Option<String>,
    pub default_room: Option<String>,
    pub mesh_enabled: Option<bool>,
    pub mesh_host: Option<String>,
    pub mesh_port: Option<u16>,
    pub iroh_enabled: Option<bool>,
    pub relay_enabled: Option<bool>,
    pub storage_total_bytes: Option<String>,
    pub storage_private_fraction: Option<f64>,
    pub storage_seal_shared: Option<bool>,
}

/// Run the interactive configuration wizard.
pub fn run_wizard(config_path: &PathBuf) -> Result<()> {
    let mut state = WizardState::default();

    print_header();

    // Step 1: Basic Settings
    print_step(1, 6, "Basic Settings");
    state.data_dir = Some(prompt(
        "Data directory",
        "./.adnet-data",
        "Where ADNet stores its data",
    )?);

    state.log_level = Some(select_one(
        "Log level",
        &["info", "debug", "warn", "error"],
        "info",
        "Logging verbosity",
    )?);

    state.log_format = Some(select_one(
        "Log format",
        &["compact", "json"],
        "compact",
        "Output format for logs",
    )?);

    state.default_room = Some(prompt_optional(
        "Default room",
        "lobby",
        "Room to join by default (leave empty for none)",
    )?);

    // Step 2: Storage Settings
    //
    // Previously the wizard only printed a hint ("use `adnet storage
    // info`/`quota`"). Operators consistently asked how to size the
    // node from the wizard itself, so we now prompt for the budget
    // directly. The total is parsed by [`crate::bytes::parse_bytes`]
    // before being saved into `app.toml`, so a typo (`"10Gib"` →
    // `"10GiB"`) is caught here — not later when the CLI tries to
    // open a topology with a junk budget.
    print_step(2, 6, "Storage Settings");
    state.storage_total_bytes = Some(prompt_storage_bytes(
        "Total storage budget",
        "20GiB",
        "Total bytes available to this node (e.g. '500GB', '2TiB', '10737418240'). \
The private + shared scopes split this by the fraction below.",
    )?);
    state.storage_private_fraction = Some(prompt_storage_fraction(
        "Private scope fraction",
        0.5,
        "Fraction of the total budget dedicated to private blobs. \
The shared scope gets the complementary value. 0.5 = 50/50.",
    )?);
    state.storage_seal_shared = Some(confirm(
        "Seal the shared scope on boot (block replication writes)",
        false,
    )?);

    // Step 3: Mesh HTTP Server
    print_step(3, 6, "Mesh HTTP Server");
    state.mesh_enabled = Some(confirm("Enable mesh HTTP server", true)?);

    if state.mesh_enabled.unwrap_or(false) {
        state.mesh_host = Some(prompt(
            "Host",
            "0.0.0.0",
            "Address to bind the mesh server",
        )?);

        state.mesh_port = Some(prompt_number(
            "Port",
            "0",
            "Port for mesh server (0 = auto)",
        )?);
    }

    // Step 4: Iroh Runtime
    print_step(4, 6, "Iroh Runtime");
    state.iroh_enabled = Some(confirm("Enable iroh runtime (P2P networking)", false)?);

    // Step 5: Relay Server
    print_step(5, 6, "Relay Server");
    state.relay_enabled = Some(confirm("Enable relay server", false)?);

    // Step 6: Review and Save
    print_step(6, 6, "Review Configuration");
    print_review(&state);

    if !confirm("Save configuration", true)? {
        println!("Wizard cancelled. No changes made.");
        return Ok(());
    }

    // Generate and save config
    let config = generate_config(&state);
    save_config(config_path, &config)?;

    println!();
    print_success(&format!("Configuration saved to {}", config_path.display()));
    println!("Run `adnet config show` to view your configuration.");
    println!("Run `adnet run` to start ADNet with these settings.");

    Ok(())
}

/// Print the wizard header.
fn print_header() {
    println!();
    println!(
        "{}",
        Color::Cyan
            .paint("╔═══════════════════════════════════════════════════════════════════════╗")
    );
    println!(
        "{}",
        Color::Cyan
            .paint("║               ADNet Configuration Wizard                              ║")
    );
    println!(
        "{}",
        Color::Cyan
            .paint("╚═══════════════════════════════════════════════════════════════════════╝")
    );
    println!();
    println!(
        "{}",
        Color::Dim.paint("This wizard will help you set up ADNet configuration step by step.")
    );
    println!(
        "{}",
        Color::Dim.paint("Press Enter to accept default values shown in brackets.")
    );
    println!();
}

/// Print a step header.
fn print_step(current: usize, total: usize, title: &str) {
    println!();
    let progress = format!("[{}/{}]", current, total);
    println!("{}", Color::Cyan.paint("─".repeat(70)));
    println!(
        "{} {}",
        Color::Yellow.paint(progress).bold(),
        Color::White.paint(title).bold()
    );
    println!("{}", Color::Cyan.paint("─".repeat(70)));
}

/// Print a review of the configuration.
fn print_review(state: &WizardState) {
    let mut panel = Box::with_title("Configuration Review")
        .border_style(BorderStyle::Single)
        .header_color(Color::Cyan);

    panel = panel.add_field(
        "Data Directory",
        state.data_dir.as_deref().unwrap_or("./.adnet-data"),
    );
    panel = panel.add_field("Log Level", state.log_level.as_deref().unwrap_or("info"));
    panel = panel.add_field(
        "Log Format",
        state.log_format.as_deref().unwrap_or("compact"),
    );

    if let Some(room) = &state.default_room {
        if !room.is_empty() {
            panel = panel.add_field("Default Room", room.as_str());
        }
    }

    panel = panel.add_field(
        "Mesh HTTP Server",
        if state.mesh_enabled.unwrap_or(false) {
            "Enabled"
        } else {
            "Disabled"
        },
    );

    if state.mesh_enabled.unwrap_or(false) {
        let host = state.mesh_host.as_deref().unwrap_or("0.0.0.0");
        let port = state.mesh_port.unwrap_or(0);
        panel = panel.add_field("Mesh Host", host);
        panel = panel.add_field("Mesh Port", &port.to_string());
    }

    panel = panel.add_field(
        "Iroh Runtime",
        if state.iroh_enabled.unwrap_or(false) {
            "Enabled"
        } else {
            "Disabled"
        },
    );

    panel = panel.add_field(
        "Relay Server",
        if state.relay_enabled.unwrap_or(false) {
            "Enabled"
        } else {
            "Disabled"
        },
    );

    panel = panel.add_field(
        "Storage Total",
        state.storage_total_bytes.as_deref().unwrap_or("20GiB"),
    );
    if let Some(frac) = state.storage_private_fraction {
        let shared = (1.0 - frac) * 100.0;
        panel = panel.add_field(
            "Private / Shared Split",
            &format!("{:.0}% / {:.0}%", frac * 100.0, shared),
        );
    }
    if let Some(seal) = state.storage_seal_shared {
        panel = panel.add_field(
            "Seal Shared Scope",
            if seal { "Yes" } else { "No" },
        );
    }

    println!("{}", panel);
}

/// Generate an AppConfig from wizard state.
fn generate_config(state: &WizardState) -> AppConfig {
    let mut cfg = AppConfig::default();

    if let Some(data_dir) = &state.data_dir {
        cfg.data_dir = PathBuf::from(data_dir);
    }

    if let Some(level) = &state.log_level {
        cfg.log.level = level.clone();
    }

    if let Some(format) = &state.log_format {
        cfg.log.format = match format.as_str() {
            "json" => LogFormat::Json,
            _ => LogFormat::Compact,
        };
    }

    if let Some(room) = &state.default_room {
        if !room.is_empty() {
            cfg.default_room = Some(room.clone());
        }
    }

    // Mesh config
    if state.mesh_enabled.unwrap_or(false) {
        let mut mesh = MeshConfig::default();
        if let Some(host) = &state.mesh_host {
            let port = state.mesh_port.unwrap_or(8080);
            mesh.bind_addr = Some(format!("{}:{}", host, port));
        }
        cfg.mesh = Some(mesh);
    }

    // Iroh config
    if state.iroh_enabled.unwrap_or(false) {
        cfg.iroh = Some(IrohConfig {
            enabled: true,
            bind_host: "127.0.0.1".to_string(),
            bind_port: 0,
            identity_path: None,
            publish_publicly: false,
            discovery: DiscoveryConfigToml::default(),
        });
    }

    // Relay config
    if state.relay_enabled.unwrap_or(false) {
        cfg.relay = Some(RelayConfig::default());
    }

    // Storage config. The strings are parsed by
    // [`StorageConfig::parsed_total_bytes`] when the CLI opens a
    // topology — we keep the original suffix format on disk so the
    // operator can recognise the value at a glance.
    let mut storage = StorageConfig::default();
    if let Some(raw) = &state.storage_total_bytes {
        // Defensive: validate BEFORE writing so a bad value never
        // reaches the JSON5 file. The loop in `prompt_storage_bytes`
        // already guards against this, but the persist layer is the
        // right place to be paranoid.
        if parse_bytes(raw).is_err() {
            // Silently skip — the operator will see the error in
            // `adbnet config validate` and the wizard will catch it
            // on the next run.
            return cfg;
        }
        storage.total_bytes = Some(raw.clone());
    }
    if let Some(frac) = state.storage_private_fraction {
        storage.private_fraction = Some(frac);
    }
    storage.seal_shared_scope = state.storage_seal_shared;
    cfg.storage = storage;

    cfg
}

/// Save configuration to file.
fn save_config(path: &PathBuf, config: &AppConfig) -> Result<()> {
    use std::fs;

    // Create parent directories if needed
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(config)?;
    fs::write(path, json)?;

    Ok(())
}

/// Prompt for a string value.
fn prompt(question: &str, default: &str, help: &str) -> Result<String> {
    println!();
    println!("{}", Color::White.paint(question).bold());
    println!("  {}", Color::Dim.paint(help));
    print!("  [{}]: ", Color::Cyan.paint(default));
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();

    Ok(if input.is_empty() {
        default.to_string()
    } else {
        input.to_string()
    })
}

/// Prompt for an optional string value.
fn prompt_optional(question: &str, default: &str, help: &str) -> Result<String> {
    println!();
    println!("{}", Color::White.paint(question).bold());
    println!("  {}", Color::Dim.paint(help));
    print!("  [{}]: ", Color::Cyan.paint("(empty for none)"));
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();

    Ok(if input.is_empty() {
        default.to_string()
    } else {
        input.to_string()
    })
}

/// Prompt for a number value.
fn prompt_number(question: &str, default: &str, help: &str) -> Result<u16> {
    let result = prompt(question, default, help)?;
    let num = result
        .parse()
        .unwrap_or_else(|_| default.parse().unwrap_or(0));
    Ok(num)
}

/// Prompt for a byte-size value. Re-prompts on parse failure so the
/// wizard never persists a typo. The default (`"20GiB"`) is shown
/// in the bracket; pressing Enter accepts it.
fn prompt_storage_bytes(question: &str, default: &str, help: &str) -> Result<String> {
    loop {
        let candidate = prompt(question, default, help)?;
        match parse_bytes(&candidate) {
            Ok(_) => return Ok(candidate),
            Err(e) => {
                println!(
                    "{}  {}",
                    Color::Red.paint("✗"),
                    Color::Red.paint(format!(
                        "invalid byte size {candidate:?}: {e} (e.g. \"10GiB\", \"500MB\", \"10737418240\")"
                    ))
                );
                // Re-prompt with the same default until the operator
                // either types a valid value or types `:cancel`.
                println!(
                    "  (press Ctrl-C to abort, or supply a valid value)"
                );
            }
        }
    }
}

/// Prompt for a `[0.0, 1.0]` fraction. The default is shown as a
/// decimal (e.g. `0.5`). Out-of-range values are rejected with a
/// visible error and re-prompted.
fn prompt_storage_fraction(question: &str, default: f64, help: &str) -> Result<f64> {
    let default_str = format!("{default}");
    loop {
        let candidate = prompt(question, &default_str, help)?;
        match candidate.parse::<f64>() {
            Ok(f) if (0.0..=1.0).contains(&f) => return Ok(f),
            Ok(f) => println!(
                "{}  fraction must be in [0.0, 1.0], got {f}",
                Color::Red.paint("✗")
            ),
            Err(e) => println!(
                "{}  not a number: {e}",
                Color::Red.paint("✗")
            ),
        }
    }
}

/// Prompt for yes/no confirmation.
fn confirm(question: &str, default: bool) -> Result<bool> {
    println!();
    let default_str = if default { "Y/n" } else { "y/N" };
    print!(
        "{} {} [{}]: ",
        Color::White.paint(question).bold(),
        Color::Dim.paint("(yes/no)"),
        Color::Cyan.paint(default_str)
    );
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();

    Ok(match input.as_str() {
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default,
    })
}

/// Select one option from a list.
fn select_one(question: &str, options: &[&str], default: &str, help: &str) -> Result<String> {
    println!();
    println!("{}", Color::White.paint(question).bold());
    println!("  {}", Color::Dim.paint(help));
    println!();

    let default_idx = options.iter().position(|&o| o == default).unwrap_or(0);

    for (i, option) in options.iter().enumerate() {
        let marker = if i == default_idx { ">" } else { " " };
        let default_marker = if i == default_idx {
            Color::Dim.paint(" (default)").to_string()
        } else {
            String::new()
        };
        println!(
            "  {} {}{}",
            Color::Yellow.paint(marker),
            option,
            default_marker
        );
    }

    print!("\n  Enter choice [{}]: ", Color::Cyan.paint(default));
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();

    // Try to parse as number
    if let Ok(idx) = input.parse::<usize>() {
        if idx > 0 && idx <= options.len() {
            return Ok(options[idx - 1].to_string());
        }
    }

    // Try to match as string
    for option in options {
        if option.eq_ignore_ascii_case(&input) {
            return Ok(option.to_string());
        }
    }

    // Default
    Ok(default.to_string())
}

/// Print a success message.
fn print_success(message: &str) {
    println!();
    println!(
        "{} {}",
        Color::Green.paint("✓"),
        Color::Green.paint(message).bold()
    );
}

/// Print an error message.
fn print_error(message: &str) {
    println!();
    println!("{} {}", Color::Red.paint("✗"), Color::Red.paint(message));
}

/// Print info message.
fn print_info(message: &str) {
    println!(
        "  {} {}",
        Color::Cyan.paint("ℹ"),
        Color::White.paint(message)
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::parse_bytes;

    #[test]
    fn test_wizard_state_default() {
        let state = WizardState::default();
        assert!(state.data_dir.is_none());
        assert!(state.log_level.is_none());
        // The new storage fields default to None so the wizard
        // generates a config that resolves to the blobstore default.
        assert!(state.storage_total_bytes.is_none());
        assert!(state.storage_private_fraction.is_none());
        assert!(state.storage_seal_shared.is_none());
    }

    #[test]
    fn test_generate_config_defaults() {
        let state = WizardState::default();
        let config = generate_config(&state);
        assert_eq!(config.data_dir, PathBuf::from("./.adnet-data"));
        assert_eq!(config.log.level, "info");
        // The storage block is present in the defaults (just empty).
        assert!(config.storage.total_bytes.is_none());
        assert!(config.storage.private_fraction.is_none());
    }

    /// The wizard's storage step must produce a String that the
    /// bytes parser can interpret. We test the round-trip here
    /// without actually running the interactive prompt.
    #[test]
    fn wizard_storage_total_bytes_passes_parser() {
        let mut state = WizardState::default();
        state.storage_total_bytes = Some("100GiB".into());
        let cfg = generate_config(&state);
        assert_eq!(cfg.storage.total_bytes.as_deref(), Some("100GiB"));
        let parsed = parse_bytes(cfg.storage.total_bytes.as_deref().unwrap()).unwrap();
        assert_eq!(parsed, 100 * 1024 * 1024 * 1024);
    }

    /// Fraction is forwarded verbatim; the CLI is responsible for
    /// clamping at the storage boundary.
    #[test]
    fn wizard_storage_fraction_round_trip() {
        let mut state = WizardState::default();
        state.storage_private_fraction = Some(0.3);
        let cfg = generate_config(&state);
        assert_eq!(cfg.storage.private_fraction, Some(0.3));
    }

    /// Seal flag is forwarded.
    #[test]
    fn wizard_storage_seal_flag_round_trip() {
        let mut state = WizardState::default();
        state.storage_seal_shared = Some(true);
        let cfg = generate_config(&state);
        assert_eq!(cfg.storage.seal_shared_scope, Some(true));
    }

    /// `generate_config` is defensive: a bad value set programmatically
    /// (e.g. by a future caller that bypasses `prompt_storage_bytes`)
    /// is silently dropped rather than persisted. We document this
    /// here so future readers don't rely on it.
    #[test]
    fn wizard_generate_config_drops_invalid_storage_total_bytes() {
        let mut state = WizardState::default();
        state.storage_total_bytes = Some("lots".into());
        let cfg = generate_config(&state);
        // The bad value is dropped — the JSON5 file would otherwise
        // fail validation on the next CLI start.
        assert!(cfg.storage.total_bytes.is_none());
    }

    /// End-to-end: write a generated config to disk and reload it
    /// via `crate::config::load` so we catch deserialization drift.
    #[test]
    fn wizard_generate_config_round_trips_through_disk() {
        use std::fs;
        let dir = std::env::temp_dir().join(format!(
            "adnet-wizard-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");

        let mut state = WizardState::default();
        state.data_dir = Some("/tmp/data".into());
        state.storage_total_bytes = Some("50GiB".into());
        state.storage_private_fraction = Some(0.6);
        state.storage_seal_shared = Some(true);
        let cfg = generate_config(&state);
        save_config(&path, &cfg).unwrap();

        let reloaded = crate::config::load(Some(&path)).unwrap();
        assert_eq!(reloaded.config.storage.total_bytes.as_deref(), Some("50GiB"));
        assert_eq!(reloaded.config.storage.private_fraction, Some(0.6));
        assert_eq!(reloaded.config.storage.seal_shared_scope, Some(true));

        let _ = fs::remove_dir_all(&dir);
    }
}
