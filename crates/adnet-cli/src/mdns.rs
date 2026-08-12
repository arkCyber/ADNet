//! mDNS LAN Discovery CLI commands.
//!
//! Provides commands for monitoring and managing mDNS discovery:
//! - `adnet mdns status` — show mDNS discovery status
//! - `adnet mdns peers` — list discovered peers
//! - `adnet mdns health` — run health check

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Subcommand;
use serde::{Deserialize, Serialize};

/// mDNS CLI subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum MdnsCmd {
    /// Show mDNS discovery status.
    Status {
        /// Emit JSON instead of the human-readable form.
        #[arg(long)]
        json: bool,
    },
    /// List discovered peers on the LAN.
    Peers {
        /// Emit JSON instead of the human-readable form.
        #[arg(long)]
        json: bool,
    },
    /// Run mDNS health check.
    Health {
        /// Emit JSON instead of the human-readable form.
        #[arg(long)]
        json: bool,
    },
}

/// mDNS status output for CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MdnsStatus {
    /// Whether mDNS is enabled.
    pub enabled: bool,
    /// Whether mDNS service is healthy.
    pub healthy: bool,
    /// Number of currently discovered peers.
    pub peer_count: usize,
    /// mDNS service name.
    pub service_name: String,
    /// mDNS multicast address.
    pub multicast_addr: String,
    /// mDNS port.
    pub port: u16,
    /// Status message.
    pub message: String,
}

/// mDNS peer information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MdnsPeerInfo {
    /// Short endpoint ID.
    pub endpoint_id_short: String,
    /// Full endpoint ID.
    pub endpoint_id: String,
    /// Peer addresses.
    pub addresses: Vec<String>,
    /// Time since last seen.
    pub last_seen: String,
    /// TTL remaining.
    pub ttl_remaining_secs: Option<u64>,
    /// Whether peer is expired.
    pub expired: bool,
}

/// Run `adnet mdns <sub>` command.
pub fn run_mdns_subcmd(sub: &MdnsCmd, data_dir: &Path) -> Result<()> {
    match sub {
        MdnsCmd::Status { json } => run_mdns_status(data_dir, *json),
        MdnsCmd::Peers { json } => run_mdns_peers(data_dir, *json),
        MdnsCmd::Health { json } => run_mdns_health(data_dir, *json),
    }
}

/// Run `adnet mdns <sub>` (top-level dispatcher).
pub fn run_mdns(sub: &crate::cli::MdnsCmd, data_dir: &std::path::Path) -> anyhow::Result<()> {
    use crate::cli::MdnsCmd as CliMdnsCmd;
    match sub {
        CliMdnsCmd::Discover { timeout } => {
            // Discover peers via mDNS — shows what would be discovered
            if *timeout > 0 {
                println!("mDNS discovery for {}s...", timeout);
                println!("(mDNS discovery requires a running node)");
            }
            println!("discovered peers: (none — mDNS requires runtime)");
            Ok(())
        }
        CliMdnsCmd::Announce { info } => {
            // Announce via mDNS
            if let Some(i) = info {
                println!("mDNS announce: {}", i);
            } else {
                println!("mDNS announce (default)");
            }
            println!("(mDNS announce requires a running node)");
            Ok(())
        }
    }
}
/// Shows current mDNS discovery status.
pub fn run_mdns_status(data_dir: &Path, json: bool) -> Result<()> {
    // Load config to check if mDNS is enabled
    let config_path = data_dir.join("config.json");
    let mdns_enabled = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)?;
        content.contains("\"mdnsEnabled\": true") || content.contains("\"mdnsEnabled\":true")
    } else {
        false
    };

    let status = MdnsStatus {
        enabled: mdns_enabled,
        healthy: mdns_enabled, // Assume healthy if enabled
        peer_count: 0,        // Would require live connection
        service_name: "_adnet._udp.local".to_string(),
        multicast_addr: "224.0.0.251".to_string(),
        port: 5353,
        message: if mdns_enabled {
            "mDNS is enabled. Connect to running node for live status.".to_string()
        } else {
            "mDNS is disabled. Use --mdns flag to enable.".to_string()
        },
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        println!("mDNS LAN Discovery Status");
        println!("=========================");
        println!("Enabled:  {}", if status.enabled { "yes" } else { "no" });
        println!("Service:  {}", status.service_name);
        println!("Multicast: {}:{}", status.multicast_addr, status.port);
        println!();
        println!("{}", status.message);
    }

    Ok(())
}

/// Run `adnet mdns peers` command.
/// Shows discovered peers on the LAN.
pub fn run_mdns_peers(data_dir: &Path, json: bool) -> Result<()> {
    // This would require a live connection to the running node
    // to fetch actual peer information.
    // For now, show the expected structure.

    if json {
        let peers: Vec<MdnsPeerInfo> = Vec::new();
        println!("{}", serde_json::to_string_pretty(&peers)?);
    } else {
        println!("mDNS Discovered Peers");
        println!("====================");
        println!();
        println!("No active mDNS peers.");
        println!("(Connect to a running node to see live peer information)");
    }

    Ok(())
}

/// Run `adnet mdns health` command.
/// Shows mDNS health status.
pub fn run_mdns_health(data_dir: &Path, json: bool) -> Result<()> {
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct HealthOutput {
        check_name: String,
        status: String,
        message: String,
        details: HealthDetails,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct HealthDetails {
        multicast_bound: bool,
        peer_count: u64,
        success_rate: String,
        recovery_state: String,
    }

    // Check if node is running by checking for socket files
    let running = data_dir.join(".node_running").exists();

    let output = HealthOutput {
        check_name: "mdns_discovery".to_string(),
        status: if running { "ok" } else { "unknown" }.to_string(),
        message: if running {
            "Connect to running node for live health check".to_string()
        } else {
            "Node not running. Start node to perform health check.".to_string()
        },
        details: HealthDetails {
            multicast_bound: false,
            peer_count: 0,
            success_rate: "0%".to_string(),
            recovery_state: "nominal".to_string(),
        },
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("mDNS Health Check");
        println!("=================");
        println!("Check:   {}", output.check_name);
        println!("Status:  {}", output.status);
        println!("Message: {}", output.message);
    }

    Ok(())
}

/// Format a duration as human-readable string.
pub fn format_duration(duration: std::time::Duration) -> String {
    let secs = duration.as_secs();
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_duration_seconds() {
        assert_eq!(format_duration(std::time::Duration::from_secs(30)), "30s");
    }

    #[test]
    fn format_duration_minutes() {
        assert_eq!(format_duration(std::time::Duration::from_secs(125)), "2m 5s");
    }

    #[test]
    fn format_duration_hours() {
        assert_eq!(format_duration(std::time::Duration::from_secs(3665)), "1h 1m");
    }

    #[test]
    fn mdns_status_serialization() {
        let status = MdnsStatus {
            enabled: true,
            healthy: true,
            peer_count: 5,
            service_name: "_adnet._udp.local".to_string(),
            multicast_addr: "224.0.0.251".to_string(),
            port: 5353,
            message: "operational".to_string(),
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"enabled\":true"));
        assert!(json.contains("\"peer_count\":5"));
    }

    #[test]
    fn mdns_peer_info_serialization() {
        let peer = MdnsPeerInfo {
            endpoint_id_short: "01aaaaaa".to_string(),
            endpoint_id: "01aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            addresses: vec!["192.168.1.100:0".to_string()],
            last_seen: "5s ago".to_string(),
            ttl_remaining_secs: Some(115),
            expired: false,
        };
        let json = serde_json::to_string(&peer).unwrap();
        assert!(json.contains("\"endpoint_id_short\":\"01aaaaaa\""));
        assert!(json.contains("\"ttl_remaining_secs\":115"));
    }
}
