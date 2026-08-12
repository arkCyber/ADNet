//! Offline node-profile snapshot — describes this node's identity,
//! role, capabilities, and resources without starting the runtime.
//!
//! The `adnet profile` CLI subcommand surfaces this to operators
//! without spinning up the node. Useful for:
//!
//! - **Node inspection** — "what role am I? what can I do?"
//! - **Capability reporting** — advertise to operators what the
//!   node can offer to the network.
//! - **Pre-flight checks** — verify the node_profile.json is
//!   intact before starting the service.
//!
//! This mirrors the pattern of [`diagnostics`](super::diagnostics):
//! both are cheap to construct and do not require the runtime.

use std::path::Path;

use adnet_types::{
    NodeCapability, NodeProfile,
    MAX_PROFILE_DESC_LEN, MAX_PROFILE_TAGS, MAX_TAG_LEN,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Offline snapshot of the local node's profile.
///
/// Built from `{data_dir}/node_profile.json` and the
/// `{data_dir}/identity.key` for the NodeId. Does not require
/// the iroh runtime, tokio, or any UDP binding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSnapshot {
    /// Node identity.
    pub node_id: String,
    /// Short fingerprint.
    pub node_id_short: String,
    /// Data directory this snapshot was built from.
    pub data_dir: String,
    /// Role label.
    pub role: String,
    /// Capabilities as a human-readable list.
    pub capabilities: Vec<String>,
    /// Raw capability bits.
    pub capability_bits: u64,
    /// Resources summary.
    pub resources: Option<ProfileResourcesSnapshot>,
    /// Free-form description.
    pub description: Option<String>,
    /// Tags.
    pub tags: Vec<String>,
    /// Software version.
    pub version: String,
    /// Unix timestamp when profile was last updated.
    pub published_at: u64,
    /// Human-readable age string.
    pub age: String,
    /// True if the profile file existed on disk.
    pub persisted: bool,
}

/// Resource fields from the profile, converted to human-readable form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileResourcesSnapshot {
    pub cpu_cores: Option<u16>,
    pub memory_summary: Option<String>,
    pub storage_summary: Option<String>,
    pub bandwidth_summary: Option<String>,
    pub battery_pct: Option<u8>,
    pub region: Option<String>,
}

/// Build a profile snapshot from the data directory.
/// Returns an error if the identity key is missing or malformed.
/// If the profile JSON is missing, a minimal default profile is synthesised.
pub fn profile_snapshot(data_dir: &Path) -> Result<ProfileSnapshot> {
    let node_id = load_node_id(data_dir)
        .with_context(|| format!("load NodeId from {}", data_dir.display()))?;

    let profile_path = data_dir.join("node_profile.json");
    let (profile, persisted) = if profile_path.exists() {
        let raw = std::fs::read_to_string(&profile_path)
            .with_context(|| format!("read {}", profile_path.display()))?;
        let p: NodeProfile = serde_json::from_str(&raw)
            .with_context(|| format!("parse {}", profile_path.display()))?;
        (p, true)
    } else {
        // Synthesise a minimal standard profile from the NodeId.
        let version = env!("CARGO_PKG_VERSION");
        (NodeProfile::standard(node_id.clone(), version), false)
    };

    let now = current_timestamp();
    let age = chrono_human_relative(now.saturating_sub(profile.published_at));

    let capabilities: Vec<String> = profile.capabilities.iter_standard()
        .map(|cap| cap_to_string(cap).to_string())
        .collect();

    let resources = profile.resources.as_ref().map(|r| {
        const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
        ProfileResourcesSnapshot {
            cpu_cores: r.cpu_cores,
            memory_summary: r.memory_bytes.map(|b| format!("{:.1} GiB", b as f64 / GIB)),
            storage_summary: r.storage_bytes.map(|b| format!("{:.0} GiB", b as f64 / GIB)),
            bandwidth_summary: r.bandwidth_bps.map(|bps| {
                if bps >= 1_000_000_000 {
                    format!("{:.1} Gbps", bps as f64 / 1e9)
                } else if bps >= 1_000_000 {
                    format!("{:.1} Mbps", bps as f64 / 1e6)
                } else {
                    format!("{:.0} Kbps", bps as f64 / 1e3)
                }
            }),
            battery_pct: r.battery_pct,
            region: r.region.clone(),
        }
    });

    Ok(ProfileSnapshot {
        node_id: node_id.as_hex().to_string(),
        node_id_short: node_id.short().to_string(),
        data_dir: data_dir.display().to_string(),
        role: profile.role.label().to_string(),
        capabilities,
        capability_bits: profile.capabilities.bits(),
        resources,
        description: profile.description.clone(),
        tags: profile.tags.clone(),
        version: profile.version.clone(),
        published_at: profile.published_at,
        age,
        persisted,
    })
}

/// Print a human-readable profile summary to stdout.
pub fn print_profile_for_humans(snap: &ProfileSnapshot) {
    println!();
    println!("  ADNet Node Profile");
    println!("  {}", "=".repeat(50));
    println!();
    println!("  Node ID    {}", snap.node_id);
    println!("  Short ID   {}", snap.node_id_short);
    println!("  Role       {}", snap.role);
    println!("  Version    {}", snap.version);
    println!("  Published  {} ago (ts={})", snap.age, snap.published_at);
    println!("  Persisted  {}", if snap.persisted { "yes" } else { "NO (in-memory only)" });
    println!();

    if let Some(ref desc) = snap.description {
        println!("  Description");
        for line in desc.chars().collect::<Vec<_>>().chunks(60) {
            println!("    {}", line.iter().collect::<String>());
        }
        println!();
    }

    println!("  Capabilities ({} flags, bits={:#018x})", snap.capabilities.len(), snap.capability_bits);
    for cap in &snap.capabilities {
        println!("    • {cap}");
    }
    println!();

    if let Some(ref res) = snap.resources {
        println!("  Resources");
        if let Some(cores) = res.cpu_cores {
            println!("    • CPU:   {cores} cores");
        }
        if let Some(ref mem) = res.memory_summary {
            println!("    • RAM:   {mem}");
        }
        if let Some(ref sto) = res.storage_summary {
            println!("    • Disk:  {sto}");
        }
        if let Some(ref bw) = res.bandwidth_summary {
            println!("    • BW:    {bw}");
        }
        if let Some(region) = &res.region {
            println!("    • Region {region}");
        }
        if let Some(pct) = res.battery_pct {
            println!("    • Battery {pct}%");
        }
        println!();
    }

    if !snap.tags.is_empty() {
        println!("  Tags ({})", snap.tags.len());
        for tag in &snap.tags {
            println!("    #{tag}");
        }
        println!();
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn chrono_human_relative(unix_secs: u64) -> String {
    if unix_secs < 60 {
        format!("{unix_secs}s")
    } else if unix_secs < 3600 {
        format!("{}m", unix_secs / 60)
    } else if unix_secs < 86400 {
        format!("{}h", unix_secs / 3600)
    } else {
        format!("{}d", unix_secs / 86400)
    }
}

fn load_node_id(data_dir: &Path) -> Result<adnet_types::NodeId> {
    let path = data_dir.join("identity.key");
    let bytes = std::fs::read(&path)
        .with_context(|| format!("read identity file at {}", path.display()))?;
    if bytes.len() == 32 {
        adnet_types::NodeId::from_bytes(&bytes)
            .context("32-byte identity blob is not a valid NodeId")
    } else {
        anyhow::bail!(
            "identity file has unexpected length {} (expected 32 bytes)",
            bytes.len()
        )
    }
}

fn cap_to_string(cap: NodeCapability) -> &'static str {
    if cap == NodeCapability::RELAY {
        "relay"
    } else if cap == NodeCapability::MQTT_BRIDGE {
        "mqtt_bridge"
    } else if cap == NodeCapability::AI_INFERENCE {
        "ai_inference"
    } else if cap == NodeCapability::BLOB_STORAGE {
        "blob_storage"
    } else if cap == NodeCapability::WORKSPACE_HOST {
        "workspace_host"
    } else if cap == NodeCapability::MESH_MONITOR {
        "mesh_monitor"
    } else if cap == NodeCapability::SSH_GATEWAY {
        "ssh_gateway"
    } else if cap == NodeCapability::DNS_RESOLVER {
        "dns_resolver"
    } else if cap == NodeCapability::EXIT_NODE {
        "exit_node"
    } else if cap == NodeCapability::NAS_SERVER {
        "nas_server"
    } else if cap == NodeCapability::AI_AGENT {
        "ai_agent"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn profile_snapshot_with_valid_identity_no_profile() {
        let dir = std::env::temp_dir().join(format!(
            "adnet-profile-none-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        // Write identity
        let raw: Vec<u8> = (0u8..32).collect();
        fs::write(dir.join("identity.key"), &raw).unwrap();

        let snap = profile_snapshot(&dir).unwrap();
        assert_eq!(snap.node_id.len(), 64);
        assert_eq!(snap.role, "standard");
        assert!(!snap.persisted);
        assert!(snap.capabilities.contains(&"blob_storage".to_string()));
        assert!(snap.capabilities.contains(&"workspace_host".to_string()));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn profile_snapshot_with_profile_json() {
        let dir = std::env::temp_dir().join(format!(
            "adnet-profile-json-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let raw: Vec<u8> = (0u8..32).collect();
        fs::write(dir.join("identity.key"), &raw).unwrap();

        let profile_json = serde_json::json!({
            "nodeId": "00".repeat(32),
            "role": "specialized",
            "capabilities": 4, // AI_INFERENCE
            "resources": {
                "cpuCores": 64,
                "memoryBytes": 137_438_953_472u64,
                "storageBytes": 1_099_511_627_776u64
            },
            "description": "GPU beast node",
            "tags": ["ai", "gpu"],
            "version": "99.99.99",
            "publishedAt": 1_000_000_000u64
        });
        fs::write(dir.join("node_profile.json"), serde_json::to_string_pretty(&profile_json).unwrap()).unwrap();

        let snap = profile_snapshot(&dir).unwrap();
        assert!(snap.persisted);
        assert_eq!(snap.role, "specialized");
        assert_eq!(snap.version, "99.99.99");
        assert_eq!(snap.description.as_deref(), Some("GPU beast node"));
        assert_eq!(snap.tags, vec!["ai", "gpu"]);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn profile_snapshot_missing_identity_errors() {
        let dir = std::env::temp_dir().join(format!("adnet-profile-no-id-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // Explicitly write a valid identity file, then delete it.
        fs::write(dir.join("identity.key"), (0u8..32).collect::<Vec<_>>()).unwrap();
        fs::remove_file(dir.join("identity.key")).unwrap();

        let err = profile_snapshot(&dir).unwrap_err();
        // Traverse the full anyhow chain to collect all error messages.
        let full_msg = err.chain().fold(String::new(), |mut acc, e| {
            if !acc.is_empty() { acc.push_str("; "); }
            acc.push_str(&e.to_string());
            acc
        });
        assert!(full_msg.contains("identity") || full_msg.contains("read") || full_msg.contains("No such"),
            "error should mention the missing file: {full_msg}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cap_to_string_all_known() {
        for cap in [
            NodeCapability::RELAY,
            NodeCapability::MQTT_BRIDGE,
            NodeCapability::AI_INFERENCE,
            NodeCapability::BLOB_STORAGE,
            NodeCapability::WORKSPACE_HOST,
            NodeCapability::MESH_MONITOR,
            NodeCapability::SSH_GATEWAY,
            NodeCapability::DNS_RESOLVER,
            NodeCapability::EXIT_NODE,
            NodeCapability::NAS_SERVER,
            NodeCapability::AI_AGENT,
        ] {
            let s = cap_to_string(cap);
            assert!(!s.is_empty());
            assert_ne!(s, "unknown");
        }
    }

    #[test]
    fn chrono_human_relative_all_ranges() {
        assert_eq!(chrono_human_relative(0), "0s");
        assert_eq!(chrono_human_relative(59), "59s");
        assert_eq!(chrono_human_relative(60), "1m");
        assert_eq!(chrono_human_relative(119), "1m");
        assert_eq!(chrono_human_relative(120), "2m");
        assert_eq!(chrono_human_relative(3599), "59m");
        assert_eq!(chrono_human_relative(3600), "1h");
        assert_eq!(chrono_human_relative(86399), "23h");
        assert_eq!(chrono_human_relative(86400), "1d");
        assert_eq!(chrono_human_relative(172800), "2d");
    }

    #[test]
    fn constants_accessible() {
        assert_eq!(MAX_PROFILE_DESC_LEN, 512);
        assert_eq!(MAX_PROFILE_TAGS, 32);
        assert_eq!(MAX_TAG_LEN, 64);
    }

    #[test]
    fn profile_snapshot_serialization_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "adnet-profile-serial-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let raw: Vec<u8> = (0u8..32).collect();
        fs::write(dir.join("identity.key"), &raw).unwrap();

        let snap = profile_snapshot(&dir).unwrap();
        let json = serde_json::to_string(&snap).unwrap();
        let back: ProfileSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.node_id, snap.node_id);
        assert_eq!(back.role, snap.role);

        fs::remove_dir_all(&dir).ok();
    }
}
