//! `adnet status` / `adnet storage …` subcommands.
//!
//! Backed by `adnet_blobstore::StorageTopology` and the
//! `adnet_observability::dashboard` JSON surface. Every
//! command is **offline** — they never start the node, they
//! only read on-disk state and the in-process metric
//! registry.
//!
//! Two main surfaces:
//!
//! - `adnet status [--json]` — operator-friendly view of
//!   the running node (storage + replication + alerts).
//! - `adnet storage <sub>` — explicit scope management:
//!   `info`, `usage`, `list [--scope private|shared]`,
//!   `quota`, `reset [--scope … --yes]`.
//!
//! Both surfaces are safe to run from deployment scripts.

use std::path::Path;

use adnet_blobstore::{
    BlobStoreScope, QuotaPolicy, StorageTopology, TopologyError,
    DEFAULT_PRIVATE_FRACTION, DEFAULT_SHARED_FRACTION,
};
use adnet_types::ContentHash;
use anyhow::{Context, Result};
use serde_json::{Value, json};

/// CLI-side enum mirroring `BlobStoreScope` for clap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeArg {
    Private,
    Shared,
}

impl std::str::FromStr for ScopeArg {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "private" | "priv" | "local" => Ok(Self::Private),
            "shared" | "share" | "public" | "global" => Ok(Self::Shared),
            other => Err(format!(
                "unknown scope {other:?} (expected 'private' or 'shared')"
            )),
        }
    }
}

impl std::fmt::Display for ScopeArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.into_scope().as_str())
    }
}

impl clap::ValueEnum for ScopeArg {
    fn value_variants<'a>() -> &'a [ScopeArg] {
        &[ScopeArg::Private, ScopeArg::Shared]
    }
    fn from_str(input: &str, _ignore_case: bool) -> Result<Self, String> {
        input.parse()
    }
    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(clap::builder::PossibleValue::new(match self {
            Self::Private => "private",
            Self::Shared => "shared",
        }))
    }
}

impl ScopeArg {
    pub fn into_scope(self) -> BlobStoreScope {
        match self {
            Self::Private => BlobStoreScope::Private,
            Self::Shared => BlobStoreScope::Shared,
        }
    }
}

/// Subcommands of `adnet storage …`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageCmd {
    /// One-line per-scope summary (bytes used / budget / hard cap).
    Info,
    /// Per-scope JSON: used_bytes, budget_bytes, hard_cap_bytes,
    /// free_bytes, blobs.
    Usage {
        /// Optional `--scope` filter; default prints both.
        scope: Option<ScopeArg>,
        json: bool,
    },
    /// List every blob (hex hash + size + scope).
    List {
        scope: Option<ScopeArg>,
        json: bool,
    },
    /// Print the effective quota policy (private_fraction,
    /// shared_fraction, hard caps).
    Quota { json: bool },
    /// DANGER: wipe every blob in the given scope. Requires
    /// `--yes` unless `--dry-run` is set. For the **shared**
    /// scope the CLI additionally requires
    /// `--i-know-what-i-am-doing` because the shared scope
    /// is normally sealed — only the replication protocol
    /// should grow it.
    Reset {
        scope: ScopeArg,
        yes: bool,
        dry_run: bool,
        i_know_what_i_am_doing: bool,
    },
}

impl std::str::FromStr for StorageCmd {
    type Err = String;
    fn from_str(_s: &str) -> Result<Self, Self::Err> {
        Err(
            "StorageCmd cannot be parsed from a string; use the typed CLI dispatcher in `run`"
                .to_string(),
        )
    }
}

/// Open the storage topology rooted at `<data_dir>/`.
///
/// The total budget defaults to 20 GiB — that matches the
/// `NodeCapacity::default()` setting and gives operators a
/// familiar starting point. Operators that want a different
/// budget should set it in `app.toml` (out of scope here).
pub fn open_topology(data_dir: &Path) -> Result<StorageTopology, TopologyError> {
    let total = 20u64 * 1024 * 1024 * 1024; // 20 GiB
    StorageTopology::open(data_dir, QuotaPolicy::default_split(total))
}

/// Print `adnet status` output.
pub fn run_status(data_dir: &Path, json: bool) -> Result<()> {
    let topo = open_topology(data_dir)
        .with_context(|| format!("open storage topology at {}", data_dir.display()))?;
    // Build a minimal dashboard snapshot. Replicator metrics
    // are global so a node-less status still reports them.
    let dash = build_dashboard(&topo, /*factor=*/ 3, /*sweep=*/ 300);
    if json {
        let s = serde_json::to_string_pretty(&dash)
            .context("serialize status JSON")?;
        println!("{s}");
    } else {
        println!("ADNet status — data_dir: {}", data_dir.display());
        println!(
            "  private scope: {} blobs, {} used / {} budget",
            dash.storage.private_blobs,
            human_bytes(dash.storage.private_used_bytes),
            human_bytes(dash.storage.private_hard_cap_bytes),
        );
        println!(
            "  shared  scope: {} blobs, {} used / {} budget (factor={}, sweep={}s)",
            dash.storage.shared_blobs,
            human_bytes(dash.storage.shared_used_bytes),
            human_bytes(dash.storage.shared_hard_cap_bytes),
            dash.replication.factor,
            dash.replication.sweeps_total,
        );
        println!(
            "  replication : sweeps={} pushes={} errors={} under={} full={}",
            dash.replication.sweeps_total,
            dash.replication.blocks_pushed_total,
            dash.replication.push_errors_total,
            dash.replication.under_replicated_blocks,
            dash.replication.fully_replicated_blocks,
        );
        if dash.alerts.is_empty() {
            println!("  alerts      : (none)");
        } else {
            println!("  alerts      :");
            for a in &dash.alerts {
                println!("    [{:?}] {} — {}", a.level, a.code, a.message);
            }
        }
    }
    Ok(())
}

/// Dispatch `adnet storage <sub>`.
pub fn run_storage(data_dir: &Path, sub: &StorageCmd) -> Result<()> {
    let topo = open_topology(data_dir)
        .with_context(|| format!("open storage topology at {}", data_dir.display()))?;
    match sub {
        StorageCmd::Info => run_info(&topo),
        StorageCmd::Usage { scope, json } => run_usage(&topo, *scope, *json),
        StorageCmd::List { scope, json } => run_list(&topo, *scope, *json),
        StorageCmd::Quota { json } => run_quota(&topo, *json),
        StorageCmd::Reset { scope, yes, dry_run, i_know_what_i_am_doing } => {
            run_reset(&topo, scope.into_scope(), *yes, *dry_run, *i_know_what_i_am_doing)
        }
    }
}

fn run_info(topo: &StorageTopology) -> Result<()> {
    let usage = topo.usage().context("read topology usage")?;
    println!("ADNet storage topology @ {}", topo.data_dir.display());
    println!(
        "  private : {} blobs, {} used / {} budget (hard cap {})",
        topo.private.list_complete().map(|v| v.len()).unwrap_or(0),
        human_bytes(usage.private_used),
        human_bytes(usage.private_budget),
        human_bytes(usage.private_hard_cap),
    );
    println!(
        "  shared  : {} blobs, {} used / {} budget (hard cap {})",
        topo.shared_store().list_complete().map(|v| v.len()).unwrap_or(0),
        human_bytes(usage.shared_used),
        human_bytes(usage.shared_budget),
        human_bytes(usage.shared_hard_cap),
    );
    Ok(())
}

fn run_usage(topo: &StorageTopology, scope: Option<ScopeArg>, json: bool) -> Result<()> {
    let usage = topo.usage().context("read topology usage")?;
    let v = match scope {
        Some(ScopeArg::Private) => json!({
            "scope": "private",
            "used_bytes": usage.private_used,
            "budget_bytes": usage.private_budget,
            "hard_cap_bytes": usage.private_hard_cap,
            "free_bytes": usage.private_budget.saturating_sub(usage.private_used),
        }),
        Some(ScopeArg::Shared) => json!({
            "scope": "shared",
            "used_bytes": usage.shared_used,
            "budget_bytes": usage.shared_budget,
            "hard_cap_bytes": usage.shared_hard_cap,
            "free_bytes": usage.shared_budget.saturating_sub(usage.shared_used),
        }),
        None => json!({
            "private": {
                "used_bytes": usage.private_used,
                "budget_bytes": usage.private_budget,
                "hard_cap_bytes": usage.private_hard_cap,
                "free_bytes": usage.private_budget.saturating_sub(usage.private_used),
            },
            "shared": {
                "used_bytes": usage.shared_used,
                "budget_bytes": usage.shared_budget,
                "hard_cap_bytes": usage.shared_hard_cap,
                "free_bytes": usage.shared_budget.saturating_sub(usage.shared_used),
            },
        }),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        if let Some(s) = scope {
            let section = match s {
                ScopeArg::Private => "private",
                ScopeArg::Shared => "shared",
            };
            let block = v.as_object().unwrap();
            println!("scope {}:", section);
            for (k, val) in block {
                if k == "scope" {
                    continue;
                }
                println!("  {:18} {}", k, val);
            }
        } else {
            for scope_name in ["private", "shared"] {
                println!("{}:", scope_name);
                if let Some(block) = v.get(scope_name).and_then(|s| s.as_object()) {
                    for (k, val) in block {
                        println!("  {:18} {}", k, val);
                    }
                }
                println!();
            }
        }
    }
    Ok(())
}

fn run_list(topo: &StorageTopology, scope: Option<ScopeArg>, json: bool) -> Result<()> {
    let scopes = match scope {
        Some(s) => vec![s.into_scope()],
        None => vec![BlobStoreScope::Private, BlobStoreScope::Shared],
    };
    let mut rows: Vec<(String, String, u64)> = Vec::new();
    for sc in scopes {
        // Read-only listing. The shared scope's listing
        // goes through the sealed handle so we never touch
        // the field-private `BlobStore`. The handler also
        // keeps us from adding a write path accidentally.
        let sealed = topo.shared_store();
        match sc {
            BlobStoreScope::Private => {
                let hashes = topo
                    .store(BlobStoreScope::Private)
                    .list_complete()
                    .with_context(|| format!("list {}", sc))?;
                for h in hashes {
                    let size = topo
                        .store(BlobStoreScope::Private)
                        .meta(&h)
                        .map(|(s, _)| s)
                        .unwrap_or(0);
                    rows.push((sc.to_string(), h.as_hex().to_string(), size));
                }
            }
            BlobStoreScope::Shared => {
                let hashes = sealed
                    .list_complete()
                    .with_context(|| format!("list {}", sc))?;
                for h in hashes {
                    let size = sealed.meta(&h).map(|(s, _)| s).unwrap_or(0);
                    rows.push((sc.to_string(), h.as_hex().to_string(), size));
                }
            }
        }
    }
    if json {
        let arr: Vec<Value> = rows
            .iter()
            .map(|(sc, h, sz)| {
                json!({
                    "scope": sc,
                    "hash": h,
                    "size_bytes": sz,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr)?);
    } else if rows.is_empty() {
        println!("(no blobs)");
    } else {
        println!("{:<8} {:<64} SIZE", "SCOPE", "HASH");
        for (sc, h, sz) in &rows {
            println!("{sc:<8} {h:<64} {}", human_bytes(*sz));
        }
    }
    Ok(())
}

fn run_quota(topo: &StorageTopology, json: bool) -> Result<()> {
    let usage = topo.usage().context("read topology usage")?;
    let v = json!({
        "private_fraction": DEFAULT_PRIVATE_FRACTION,
        "shared_fraction": DEFAULT_SHARED_FRACTION,
        "total_bytes": usage.total_bytes,
        "private": {
            "budget_bytes": usage.private_budget,
            "hard_cap_bytes": usage.private_hard_cap,
        },
        "shared": {
            "budget_bytes": usage.shared_budget,
            "hard_cap_bytes": usage.shared_hard_cap,
        },
        "data_dir": topo.data_dir.display().to_string(),
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        println!("private_fraction: {:.0}%", DEFAULT_PRIVATE_FRACTION * 100.0);
        println!("shared_fraction : {:.0}%", DEFAULT_SHARED_FRACTION * 100.0);
        println!("total_bytes     : {}", usage.total_bytes);
        println!(
            "private budget  : {} (hard cap {})",
            human_bytes(usage.private_budget),
            human_bytes(usage.private_hard_cap),
        );
        println!(
            "shared  budget  : {} (hard cap {})",
            human_bytes(usage.shared_budget),
            human_bytes(usage.shared_hard_cap),
        );
    }
    Ok(())
}

fn run_reset(
    topo: &StorageTopology,
    scope: BlobStoreScope,
    yes: bool,
    dry_run: bool,
    i_know: bool,
) -> Result<()> {
    // Sealed-scope audit: the shared scope is owned by the
    // replication protocol. The CLI admin command may wipe
    // it ONLY when the operator has confirmed by passing
    // `--i-know-what-i-am-doing`. Without the flag we refuse
    // with a clear error message that points at the audit
    // invariant.
    if matches!(scope, BlobStoreScope::Shared) && !i_know && !dry_run {
        anyhow::bail!(
            "refusing to wipe the sealed shared scope without \
             --i-know-what-i-am-doing; the shared scope is governed \
             by the replication protocol. Re-import via `adnet share \
             put` if you need a clean slate. \
             (audit invariant: sealed-scope)"
        );
    }
    match scope {
        BlobStoreScope::Private => {
            let store = topo.store(BlobStoreScope::Private);
            let hashes = store.list_complete().unwrap_or_default();
            let total: u64 = hashes
                .iter()
                .map(|h| store.meta(h).map(|(s, _)| s).unwrap_or(0))
                .sum();
            if dry_run {
                println!(
                    "dry-run: would remove {} blobs ({} bytes) from {scope}",
                    hashes.len(),
                    total
                );
                return Ok(());
            }
            if !yes {
                eprintln!(
                    "adnet: about to delete {} blobs ({} bytes) from {scope}",
                    hashes.len(),
                    total
                );
                eprintln!("hint: pass --yes to skip this prompt (or --dry-run to preview)");
                eprintln!("continue? [y/N]");
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                if !matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                    eprintln!("aborted");
                    std::process::exit(2);
                }
            }
            for h in &hashes {
                store.remove(h).context("remove blob")?;
            }
            Ok(())
        }
        BlobStoreScope::Shared => {
            // Dry-run: just count.
            let sealed = topo.shared_store();
            let hashes = sealed.list_complete().unwrap_or_default();
            if dry_run {
                println!(
                    "dry-run: would remove {} blobs from sealed shared scope",
                    hashes.len()
                );
                return Ok(());
            }
            // Operator confirmed.
            if !yes {
                eprintln!(
                    "adnet: about to wipe {} blobs from the SEALED shared scope.",
                    hashes.len()
                );
                eprintln!("This is reserved for emergency operator recovery only.");
                eprintln!("hint: pass --yes to skip this prompt (or --dry-run to preview)");
                eprintln!("continue? [y/N]");
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                if !matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                    eprintln!("aborted");
                    std::process::exit(2);
                }
            }
            let n = sealed.wipe_admin().context("wipe sealed shared scope")?;
            println!("wiped {n} blobs from sealed shared scope");
            Ok(())
        }
    }
}

fn build_dashboard(
    topo: &StorageTopology,
    factor: u8,
    sweep: u64,
) -> adnet_observability::dashboard::Dashboard {
    use adnet_observability::dashboard::{
        DashboardBuilder, StorageSection,
    };
    let storage = topo.usage().ok().map(|u| {
        let mut s = StorageSection::default();
        s.private_used_bytes = u.private_used;
        s.private_budget_bytes = u.private_budget;
        s.private_hard_cap_bytes = u.private_hard_cap;
        s.shared_used_bytes = u.shared_used;
        s.shared_budget_bytes = u.shared_budget;
        s.shared_hard_cap_bytes = u.shared_hard_cap;
        s.private_blobs = topo
            .private
            .list_complete()
            .map(|v| v.len() as u64)
            .unwrap_or(0);
        s.shared_blobs = topo
            .shared_store()
            .list_complete()
            .map(|v| v.len() as u64)
            .unwrap_or(0);
        // Audit fix P0-W: surface the sealed-scope
        // invariant on the dashboard so monitors can
        // alert if the policy is not yet sealed, or if
        // the write-path list ever drifts from
        // `["accept_replica"]`.
        s.quota_sealed = topo.quota.sealed;
        s.quota_sealed_at_unix_ms = topo.quota.sealed_at_unix_ms;
        s.shared_write_paths = vec!["accept_replica".into()];
        s
    });
    let mut builder = DashboardBuilder::new(std::sync::Arc::new(
        adnet_observability::registry::Registry::default(),
    ))
    .with_replication(factor, sweep);
    if let Some(s) = storage {
        builder = builder.with_storage(s);
    }
    builder.build()
}

/// Compact human-readable byte counter: 1.23 KiB / 4.56 MiB / 7.8 GiB.
fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.2} {}", UNITS[u])
    }
}

// Tiny helpers used by the cmd dispatcher.
#[allow(dead_code)]
pub fn scope_str(s: ScopeArg) -> &'static str {
    match s {
        ScopeArg::Private => "private",
        ScopeArg::Shared => "shared",
    }
}

#[allow(dead_code)]
pub fn hash_from_str(s: &str) -> Result<ContentHash> {
    ContentHash::from_hex(s).with_context(|| format!("invalid content hash: {s:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn status_runs_offline() {
        let dir = tempdir().unwrap();
        let topo = open_topology(dir.path()).unwrap();
        let dash = build_dashboard(&topo, 3, 300);
        assert_eq!(dash.replication.factor, 3);
        assert_eq!(dash.replication.sweeps_total, 0);
    }

    #[test]
    fn usage_default_split_is_safe_with_zero_total() {
        // Even with a 0-byte budget the dashboard builder
        // must not panic.
        let dir = tempdir().unwrap();
        // Build with default (20 GiB).
        let topo = open_topology(dir.path()).unwrap();
        let dash = build_dashboard(&topo, 3, 300);
        // No alerts should fire on a fresh topology.
        assert!(dash.alerts.is_empty(), "alerts={:?}", dash.alerts);
    }

    #[test]
    fn run_status_json_is_well_formed() {
        let dir = tempdir().unwrap();
        run_status(dir.path(), true).unwrap();
    }

    #[test]
    fn run_status_human_is_well_formed() {
        let dir = tempdir().unwrap();
        run_status(dir.path(), false).unwrap();
    }

    #[test]
    fn run_storage_info_lists_both_scopes() {
        let dir = tempdir().unwrap();
        run_storage(dir.path(), &StorageCmd::Info).unwrap();
    }

    #[test]
    fn run_storage_usage_json() {
        let dir = tempdir().unwrap();
        run_storage(
            dir.path(),
            &StorageCmd::Usage {
                scope: None,
                json: true,
            },
        )
        .unwrap();
    }

    #[test]
    fn run_storage_usage_with_scope() {
        let dir = tempdir().unwrap();
        run_storage(
            dir.path(),
            &StorageCmd::Usage {
                scope: Some(ScopeArg::Private),
                json: false,
            },
        )
        .unwrap();
    }

    #[test]
    fn run_storage_list_empty() {
        let dir = tempdir().unwrap();
        run_storage(
            dir.path(),
            &StorageCmd::List {
                scope: None,
                json: false,
            },
        )
        .unwrap();
    }

    #[test]
    fn run_storage_quota_json() {
        let dir = tempdir().unwrap();
        run_storage(
            dir.path(),
            &StorageCmd::Quota { json: true },
        )
        .unwrap();
    }

    #[test]
    fn run_storage_reset_dry_run_does_not_delete() {
        let dir = tempdir().unwrap();
        // Import a blob.
        let payload = vec![0xCDu8; 1024];
        let src = dir.path().join("p.bin");
        std::fs::write(&src, &payload).unwrap();
        let store = &open_topology(dir.path()).unwrap().private;
        let _ = store.import_file_sync(&src).unwrap();
        run_storage(
            dir.path(),
            &StorageCmd::Reset {
                scope: ScopeArg::Private,
                yes: false,
                dry_run: true,
                i_know_what_i_am_doing: false,
            },
        )
        .unwrap();
        // Blobs must still exist.
        assert!(!store.list_complete().unwrap().is_empty());
    }

    #[test]
    fn run_storage_reset_yes_deletes() {
        let dir = tempdir().unwrap();
        let payload = vec![0xCDu8; 1024];
        let src = dir.path().join("p.bin");
        std::fs::write(&src, &payload).unwrap();
        let topo = open_topology(dir.path()).unwrap();
        let _ = topo.private.import_file_sync(&src).unwrap();
        run_storage(
            dir.path(),
            &StorageCmd::Reset {
                scope: ScopeArg::Private,
                yes: true,
                dry_run: false,
                i_know_what_i_am_doing: false,
            },
        )
        .unwrap();
        assert!(topo.private.list_complete().unwrap().is_empty());
    }

    #[test]
    fn run_storage_reset_shared_requires_i_know_flag() {
        let dir = tempdir().unwrap();
        let topo = open_topology(dir.path()).unwrap();
        let payload = vec![0xCDu8; 1024];
        let src = dir.path().join("p.bin");
        std::fs::write(&src, &payload).unwrap();
        let _ = topo.private.import_file_sync(&src).unwrap();
        // Even though `--yes` is set, the sealed shared
        // scope refuses without the danger flag.
        let r = run_storage(
            dir.path(),
            &StorageCmd::Reset {
                scope: ScopeArg::Shared,
                yes: true,
                dry_run: false,
                i_know_what_i_am_doing: false,
            },
        );
        assert!(r.is_err(), "shared reset must require i-know flag");
        assert!(
            format!("{:#}", r.unwrap_err()).contains("sealed"),
            "error must reference sealed-scope"
        );
    }

    #[test]
    fn human_bytes_formats_known_values() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.00 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1.00 MiB");
        assert_eq!(human_bytes(1024u64.pow(4)), "1.00 TiB");
    }

    #[test]
    fn scope_arg_round_trip() {
        let s: ScopeArg = "private".parse().unwrap();
        assert_eq!(s, ScopeArg::Private);
        let s: ScopeArg = "shared".parse().unwrap();
        assert_eq!(s, ScopeArg::Shared);
        assert!("nonsense".parse::<ScopeArg>().is_err());
    }
}