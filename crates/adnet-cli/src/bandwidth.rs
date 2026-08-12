//! Bandwidth Management CLI
//!
//! Provides command-line interface for managing multi-tenant bandwidth controls.

use clap::{Parser, Subcommand};
use std::str::FromStr;

use adnet_blobstore::{
    BandwidthPolicy, GlobalBandwidthLimits,
    TenantBandwidthManager, TenantId, TenantPriority,
};

/// Parse bandwidth value from human-readable string (e.g., "10MB/s", "1GB/s").
fn parse_bandwidth(s: &str) -> Result<u64, String> {
    let s = s.trim().to_uppercase();
    let (value_str, unit): (&str, &str) = if let Some(pos) = s.find(|c: char| !c.is_ascii_digit() && c != '.') {
        (&s[..pos], &s[pos..])
    } else {
        (&s, "")
    };

    let value: f64 = value_str
        .parse()
        .map_err(|_| format!("invalid number: {}", value_str))?;

    let multiplier = match unit {
        "" | "B" | "B/s" | "B/S" => 1,
        "KB" | "KB/s" | "KB/S" => 1024,
        "MB" | "MB/s" | "MB/S" => 1024 * 1024,
        "GB" | "GB/s" | "GB/S" => 1024 * 1024 * 1024,
        "K" | "K/s" | "K/S" => 1000,
        "M" | "M/s" | "M/S" => 1000 * 1000,
        "G" | "G/s" | "G/S" => 1000 * 1000 * 1000,
        _ => return Err(format!("unknown unit: {}", unit)),
    };

    Ok((value * multiplier as f64) as u64)
}

/// Parse duration from human-readable string (e.g., "5s", "10m", "1h").
fn parse_duration(s: &str) -> Result<u64, String> {
    let s = s.trim().to_lowercase();
    let (value_str, unit): (&str, &str) = if let Some(pos) = s.find(|c: char| !c.is_ascii_digit()) {
        (&s[..pos], &s[pos..])
    } else {
        (&s, "s")
    };

    let value: u64 = value_str
        .parse()
        .map_err(|_| format!("invalid number: {}", value_str))?;

    let multiplier = match unit {
        "s" | "sec" | "secs" => 1,
        "m" | "min" | "mins" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
        _ => return Err(format!("unknown unit: {}", unit)),
    };

    Ok(value * multiplier)
}

/// Priority level for tenant bandwidth allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriorityArg {
    Low,
    Normal,
    High,
    Critical,
}

impl FromStr for PriorityArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "low" | "l" => Ok(PriorityArg::Low),
            "normal" | "n" | "default" => Ok(PriorityArg::Normal),
            "high" | "h" => Ok(PriorityArg::High),
            "critical" | "c" => Ok(PriorityArg::Critical),
            _ => Err(format!("unknown priority: {}", s)),
        }
    }
}

impl From<PriorityArg> for TenantPriority {
    fn from(p: PriorityArg) -> Self {
        match p {
            PriorityArg::Low => TenantPriority::Low,
            PriorityArg::Normal => TenantPriority::Normal,
            PriorityArg::High => TenantPriority::High,
            PriorityArg::Critical => TenantPriority::Critical,
        }
    }
}

/// Bandwidth management subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum BandwidthCmd {
    /// Show global bandwidth limits and current usage.
    Status {
        /// Output as JSON for machine parsing.
        #[arg(long)]
        json: bool,
    },

    /// List all registered tenants and their policies.
    List {
        /// Show detailed information including current rates.
        #[arg(long, short)]
        verbose: bool,
    },

    /// Add a new tenant with bandwidth policy.
    Add {
        /// Unique tenant identifier.
        tenant_id: String,

        /// Maximum upload bandwidth (e.g., "10MB/s", "1GB/s").
        #[arg(long, short = 'u')]
        upload: Option<String>,

        /// Maximum download bandwidth.
        #[arg(long, short = 'd')]
        download: Option<String>,

        /// Priority level (low, normal, high, critical).
        #[arg(long, short = 'p', default_value = "normal")]
        priority: PriorityArg,

        /// Allow using reserved bandwidth when system is saturated.
        #[arg(long, short = 'r')]
        reserved: bool,

        /// Burst multiplier (default: 1.0, allows short bursts).
        #[arg(long, default_value = "1.0")]
        burst: f64,
    },

    /// Remove a tenant from bandwidth management.
    Remove {
        /// Tenant identifier to remove.
        tenant_id: String,
    },

    /// Update a tenant's bandwidth policy.
    Update {
        /// Tenant identifier to update.
        tenant_id: String,

        /// New maximum upload bandwidth.
        #[arg(long)]
        upload: Option<String>,

        /// New maximum download bandwidth.
        #[arg(long)]
        download: Option<String>,

        /// New priority level.
        #[arg(long)]
        priority: Option<PriorityArg>,

        /// Allow using reserved bandwidth.
        #[arg(long)]
        reserved: Option<bool>,

        /// New burst multiplier.
        #[arg(long)]
        burst: Option<f64>,
    },

    /// Show detailed status for a specific tenant.
    Tenant {
        /// Tenant identifier.
        tenant_id: String,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Set global bandwidth limits.
    SetLimits {
        /// Maximum total upload bandwidth for all P2P transfers.
        #[arg(long)]
        max_upload: String,

        /// Maximum total download bandwidth.
        #[arg(long)]
        max_download: String,

        /// Bandwidth reserved for system operations.
        #[arg(long)]
        reserved: Option<String>,
    },

    /// Apply a predefined bandwidth profile.
    Profile {
        /// Profile name to apply.
        profile: String,

        /// Show what would change without applying.
        #[arg(long, short = 'n')]
        dry_run: bool,
    },
}

/// Run bandwidth management command.
pub async fn run_bandwidth_cmd(
    cmd: BandwidthCmd,
    manager: std::sync::Arc<TenantBandwidthManager>,
) -> Result<(), String> {
    match cmd {
        BandwidthCmd::Status { json } => {
            let status = manager.get_global_status();
            if json {
                println!("{}", serde_json::to_string_pretty(&status).map_err(|e| e.to_string())?);
            } else {
                print_status(&status);
            }
        }

        BandwidthCmd::List { verbose } => {
            let tenants = manager.list_tenants();
            if tenants.is_empty() {
                println!("No tenants registered.");
                return Ok(());
            }

            println!("{:<20} {:>12} {:>12} {:>8}", "Tenant", "Upload", "Download", "Priority");
            println!("{}", "-".repeat(60));

            for tenant_id in tenants {
                let status = manager.get_tenant_status(&tenant_id).map_err(|e| e.to_string())?;
                let upload = format_bps(status.policy.max_upload_bps);
                let download = format_bps(status.policy.max_download_bps);
                let priority = format!("{:?}", status.policy.priority);

                println!("{:<20} {:>12} {:>12} {:>8}", tenant_id.as_ref(), upload, download, priority);

                if verbose {
                    if status.current_upload_bps > 0 || status.current_download_bps > 0 {
                        println!("  Current: upload={} B/s, download={} B/s",
                                 format_bps(status.current_upload_bps),
                                 format_bps(status.current_download_bps));
                    }
                    if status.policy.can_use_reserved {
                        println!("  Reserved bandwidth: allowed");
                    }
                    if status.policy.burst_multiplier != 1.0 {
                        println!("  Burst multiplier: {}", status.policy.burst_multiplier);
                    }
                }
            }
        }

        BandwidthCmd::Add { tenant_id, upload, download, priority, reserved, burst } => {
            let tenant = TenantId::new(tenant_id.clone());
            let policy = BandwidthPolicy {
                max_upload_bps: upload.map(|s| parse_bandwidth(&s)).transpose()?.unwrap_or(0),
                max_download_bps: download.map(|s| parse_bandwidth(&s)).transpose()?.unwrap_or(0),
                priority: priority.into(),
                can_use_reserved: reserved,
                burst_multiplier: burst,
            };

            manager.add_tenant(tenant, policy).map_err(|e| e.to_string())?;
            println!("Added tenant: {}", tenant_id);
        }

        BandwidthCmd::Remove { tenant_id } => {
            let tenant = TenantId::new(tenant_id);
            manager.remove_tenant(&tenant).map_err(|e| e.to_string())?;
            println!("Removed tenant: {}", tenant.as_ref());
        }

        BandwidthCmd::Update { tenant_id, upload, download, priority, reserved, burst } => {
            let tenant = TenantId::new(tenant_id);
            let mut policy = manager.get_policy(&tenant).map_err(|e| e.to_string())?;

            if let Some(u) = upload {
                policy.max_upload_bps = parse_bandwidth(&u)?;
            }
            if let Some(d) = download {
                policy.max_download_bps = parse_bandwidth(&d)?;
            }
            if let Some(p) = priority {
                policy.priority = p.into();
            }
            if let Some(r) = reserved {
                policy.can_use_reserved = r;
            }
            if let Some(b) = burst {
                policy.burst_multiplier = b;
            }

            manager.update_policy(&tenant, policy).map_err(|e| e.to_string())?;
            println!("Updated tenant: {}", tenant.as_ref());
        }

        BandwidthCmd::Tenant { tenant_id, json } => {
            let tenant = TenantId::new(tenant_id);
            let status = manager.get_tenant_status(&tenant).map_err(|e| e.to_string())?;

            if json {
                println!("{}", serde_json::to_string_pretty(&status).map_err(|e| e.to_string())?);
            } else {
                print_tenant_status(&status);
            }
        }

        BandwidthCmd::SetLimits { max_upload, max_download, reserved } => {
            let limits = GlobalBandwidthLimits {
                max_upload_bps: parse_bandwidth(&max_upload)?,
                max_download_bps: parse_bandwidth(&max_download)?,
                reserved_for_system_bps: reserved.map(|s| parse_bandwidth(&s)).transpose()?.unwrap_or(0),
                ..Default::default()
            };

            manager.update_limits(limits.clone());
            println!("Updated global bandwidth limits:");
            println!("  Max upload: {}", format_bps(limits.max_upload_bps));
            println!("  Max download: {}", format_bps(limits.max_download_bps));
            println!("  Reserved: {}", format_bps(limits.reserved_for_system_bps));
            println!("  Usable upload: {}", format_bps(limits.usable_upload_bps()));
            println!("  Usable download: {}", format_bps(limits.usable_download_bps()));
        }

        BandwidthCmd::Profile { profile, dry_run } => {
            let (limits, default_upload, default_download) = match profile.to_lowercase().as_str() {
                "family" | "home" => (
                    GlobalBandwidthLimits::new(
                        20 * 1024 * 1024,   // 20 MB/s
                        50 * 1024 * 1024,   // 50 MB/s
                        10 * 1024 * 1024,   // 10 MB/s reserved
                    ),
                    BandwidthPolicy::new(5 * 1024 * 1024, 20 * 1024 * 1024),    // 5 MB/s up, 20 MB/s down
                    BandwidthPolicy::new(20 * 1024 * 1024, 50 * 1024 * 1024), // 20 MB/s up, 50 MB/s down
                ),
                "enterprise" | "biz" => (
                    GlobalBandwidthLimits::new(
                        100 * 1024 * 1024,   // 100 MB/s
                        200 * 1024 * 1024,   // 200 MB/s
                        50 * 1024 * 1024,    // 50 MB/s reserved
                    ),
                    BandwidthPolicy::new(20 * 1024 * 1024, 100 * 1024 * 1024),
                    BandwidthPolicy::new(100 * 1024 * 1024, 200 * 1024 * 1024),
                ),
                "guest" | "limited" => (
                    GlobalBandwidthLimits::new(
                        5 * 1024 * 1024,    // 5 MB/s
                        20 * 1024 * 1024,   // 20 MB/s
                        2 * 1024 * 1024,    // 2 MB/s reserved
                    ),
                    BandwidthPolicy::new(512 * 1024, 2 * 1024 * 1024).with_priority(TenantPriority::Low),
                    BandwidthPolicy::new(2 * 1024 * 1024, 10 * 1024 * 1024).with_priority(TenantPriority::Low),
                ),
                "unlimited" | "full" => (
                    GlobalBandwidthLimits::new(
                        u64::MAX / 2,
                        u64::MAX / 2,
                        0,
                    ),
                    BandwidthPolicy::default(),
                    BandwidthPolicy::default(),
                ),
                _ => return Err(format!("unknown profile: {}. Available: family, enterprise, guest, unlimited", profile)),
            };

            println!("Profile '{}' configuration:", profile);
            println!("  Global limits:");
            println!("    Max upload: {}", format_bps(limits.max_upload_bps));
            println!("    Max download: {}", format_bps(limits.max_download_bps));
            println!("    Reserved: {}", format_bps(limits.reserved_for_system_bps));
            println!("  Default upload policy:");
            println!("    Max upload: {}", format_bps(default_upload.max_upload_bps));
            println!("    Max download: {}", format_bps(default_upload.max_download_bps));
            println!("  Default download policy:");
            println!("    Max upload: {}", format_bps(default_download.max_upload_bps));
            println!("    Max download: {}", format_bps(default_download.max_download_bps));

            if !dry_run {
                manager.update_limits(limits);
                println!("\nApplied profile '{}'.", profile);
            } else {
                println!("\n(Dry run - no changes made)");
            }
        }
    }

    Ok(())
}

fn print_status(status: &adnet_blobstore::GlobalBandwidthStatus) {
    println!("=== Global Bandwidth Status ===");
    println!();
    println!("Limits:");
    println!("  Max upload:       {}", format_bps(status.limits.max_upload_bps));
    println!("  Max download:     {}", format_bps(status.limits.max_download_bps));
    println!("  Reserved:         {}", format_bps(status.limits.reserved_for_system_bps));
    println!();
    println!("Usage:");
    println!("  Current upload:   {}", format_bps(status.total_upload_bps));
    println!("  Current download: {}", format_bps(status.total_download_bps));
    println!("  Remaining upload: {}", format_bps(status.remaining_upload_bps));
    println!("  Remaining download: {}", format_bps(status.remaining_download_bps));
    println!();
    println!("Tenants: {}", status.active_tenants);
}

fn print_tenant_status(status: &adnet_blobstore::TenantBandwidthStatus) {
    println!("=== Tenant: {} ===", status.tenant_id);
    println!();
    println!("Policy:");
    println!("  Max upload:       {}", format_bps(status.policy.max_upload_bps));
    println!("  Max download:     {}", format_bps(status.policy.max_download_bps));
    println!("  Priority:         {:?}", status.policy.priority);
    println!("  Reserved:         {}", if status.policy.can_use_reserved { "yes" } else { "no" });
    println!("  Burst multiplier: {}", status.policy.burst_multiplier);
    println!();
    println!("Current Usage:");
    println!("  Upload rate:     {}", format_bps(status.current_upload_bps));
    println!("  Download rate:   {}", format_bps(status.current_download_bps));
}

fn format_bps(bps: u64) -> String {
    if bps >= 1024 * 1024 * 1024 {
        format!("{:.1} GB/s", bps as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bps >= 1024 * 1024 {
        format!("{:.1} MB/s", bps as f64 / (1024.0 * 1024.0))
    } else if bps >= 1024 {
        format!("{:.1} KB/s", bps as f64 / 1024.0)
    } else if bps > 0 {
        format!("{} B/s", bps)
    } else {
        "unlimited".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_bandwidth() {
        assert_eq!(parse_bandwidth("10MB/s").unwrap(), 10 * 1024 * 1024);
        assert_eq!(parse_bandwidth("1GB").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_bandwidth("100KB").unwrap(), 100 * 1024);
        assert_eq!(parse_bandwidth("1000").unwrap(), 1000);
    }

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration("30s").unwrap(), 30);
        assert_eq!(parse_duration("5m").unwrap(), 300);
        assert_eq!(parse_duration("1h").unwrap(), 3600);
    }

    #[test]
    fn test_format_bps() {
        assert_eq!(format_bps(0), "unlimited");
        assert_eq!(format_bps(512), "512 B/s");
        assert_eq!(format_bps(1024 * 100), "100.0 KB/s");
        assert_eq!(format_bps(1024 * 1024), "1.0 MB/s");
        assert_eq!(format_bps(1024 * 1024 * 1024), "1.0 GB/s");
    }
}
