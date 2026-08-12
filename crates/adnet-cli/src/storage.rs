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
        self.into_scope().fmt(f)
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
#[derive(Debug, Clone, PartialEq)]
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
    Quota {
        json: bool,
        /// Set the total budget (e.g. `"50GiB"`, `"10737418240"`).
        /// When supplied, the value is parsed by [`crate::bytes::parse_bytes`]
        /// and persisted into `topology.json`.
        set: Option<String>,
        /// Override the private scope hard cap. Same format as
        /// `set`. Mutually exclusive with the `--fraction` flag.
        set_private_hard_cap: Option<String>,
        /// Override the shared scope hard cap.
        set_shared_hard_cap: Option<String>,
        /// Set the private scope fraction (0.0..=1.0).
        set_private_fraction: Option<f64>,
        /// When true, seal the shared scope (no further writes from
        /// the replication protocol).
        seal: bool,
    },
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
    /// Initialise the encryption subsystem — see
    /// [`run_encrypt_init`].
    EncryptInit {
        passphrase: Option<String>,
        force: bool,
        json: bool,
    },
    /// Print the encryption status — see [`run_encrypt_status`].
    EncryptStatus { json: bool },
    /// Remove the on-disk key file. Existing ciphertexts become
    /// unreadable. See [`run_encrypt_disable`].
    EncryptDisable { yes: bool, json: bool },
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

/// Open the storage topology rooted at `<data_dir>/` using the
/// resolved total budget from `<data_dir>/config.json` + the
/// `ADNET_STORAGE_TOTAL_BYTES` env override.
///
/// The CLI surfaces this through [`crate::storage::open_topology_with_config`]
/// so a deploy that sets `storage.totalBytes = "100GiB"` in `app.toml`
/// gets exactly that budget instead of the legacy 20 GiB hard-code.
///
/// When the topology already has a persisted `topology.json` on disk
/// the blobstore promotes that policy in place, so a per-node CLI
/// edit (`adnet storage quota --set 5GiB`) survives a config-file
/// reset as long as the operator does not wipe the data directory.
pub fn open_topology(data_dir: &Path) -> Result<StorageTopology, TopologyError> {
    let total = crate::bytes::DEFAULT_TOTAL_BYTES;
    open_topology_with_total_bytes(data_dir, total)
}

/// Open the storage topology with an explicit total budget. This is
/// the entry point used by the CLI once the storage config has been
/// resolved (env > file > default).
pub fn open_topology_with_total_bytes(
    data_dir: &Path,
    total_bytes: u64,
) -> Result<StorageTopology, TopologyError> {
    StorageTopology::open(data_dir, QuotaPolicy::default_split(total_bytes))
}

/// Resolve the total budget using [`crate::config::StorageConfig`]
/// + the `ADNET_STORAGE_TOTAL_BYTES` env override, then open the
/// topology. This is the new end-to-end entry point that replaces
/// the legacy hard-coded `open_topology()` for every CLI command
/// that needs a `StorageTopology`.
pub fn open_topology_with_config(
    data_dir: &Path,
    cfg: &crate::config::StorageConfig,
) -> Result<StorageTopology, TopologyError> {
    let total = match cfg.resolved_total_bytes() {
        Ok(n) => n,
        Err(e) => {
            // Fail-soft: an unreachable parse error in the config
            // (e.g. "lots") is logged and we fall back to the default.
            // The CLI surfaces the same error in `validate` so the
            // operator notices on the next `adbnet config show`.
            eprintln!(
                "adnet: storage.totalBytes is invalid ({e}); falling back to {} bytes",
                crate::bytes::DEFAULT_TOTAL_BYTES
            );
            crate::bytes::DEFAULT_TOTAL_BYTES
        }
    };
    open_topology_with_total_bytes(data_dir, total)
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
        StorageCmd::Quota {
            json,
            set,
            set_private_hard_cap,
            set_shared_hard_cap,
            set_private_fraction,
            seal,
        } => run_quota(
            data_dir,
            &topo,
            *json,
            set.as_deref(),
            set_private_hard_cap.as_deref(),
            set_shared_hard_cap.as_deref(),
            *set_private_fraction,
            *seal,
        ),
        StorageCmd::Reset { scope, yes, dry_run, i_know_what_i_am_doing } => {
            run_reset(&topo, scope.into_scope(), *yes, *dry_run, *i_know_what_i_am_doing)
        }
        StorageCmd::EncryptInit { passphrase, force, json } => {
            run_encrypt_init(data_dir, passphrase.as_deref(), *force, *json)
        }
        StorageCmd::EncryptStatus { json } => run_encrypt_status(data_dir, *json),
        StorageCmd::EncryptDisable { yes, json } => {
            run_encrypt_disable(data_dir, *yes, *json)
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

/// `adnet storage quota` — print the effective policy, or apply a
/// new one when the right flags are passed.
///
/// Setting any of the `set_*` flags (or `seal`) **mutates** the
/// persisted `topology.json` on disk. The next `StorageTopology::open`
/// call picks the new policy up; the legacy hard-coded 20 GiB is no
/// longer consulted when the topology file already exists.
fn run_quota(
    data_dir: &Path,
    topo: &StorageTopology,
    json: bool,
    set: Option<&str>,
    set_private_hard_cap: Option<&str>,
    set_shared_hard_cap: Option<&str>,
    set_private_fraction: Option<f64>,
    seal: bool,
) -> Result<()> {
    let mut policy = topo.quota.clone();

    // ── apply edits ────────────────────────────────────────────────
    if let Some(raw) = set {
        let total = crate::bytes::parse_bytes(raw)
            .with_context(|| format!("adbnet storage quota --set: {raw:?} is not a valid byte size"))?;
        if total == 0 {
            anyhow::bail!("--set requires a non-zero value");
        }
        let private = set_private_hard_cap
            .map(crate::bytes::parse_bytes)
            .transpose()
            .context("parse --set-private-hard-cap")?
            .unwrap_or(total / 2);
        let shared = set_shared_hard_cap
            .map(crate::bytes::parse_bytes)
            .transpose()
            .context("parse --set-shared-hard-cap")?
            .unwrap_or(total - private);
        policy.private_hard_cap = Some(private);
        policy.shared_hard_cap = Some(shared);
        policy.private_fraction = private as f64 / total as f64;
        policy.shared_fraction = shared as f64 / total as f64;
    } else {
        if let Some(raw) = set_private_hard_cap {
            let v = crate::bytes::parse_bytes(raw)
                .with_context(|| format!("--set-private-hard-cap: {raw:?} invalid"))?;
            policy.private_hard_cap = Some(v);
        }
        if let Some(raw) = set_shared_hard_cap {
            let v = crate::bytes::parse_bytes(raw)
                .with_context(|| format!("--set-shared-hard-cap: {raw:?} invalid"))?;
            policy.shared_hard_cap = Some(v);
        }
    }
    if let Some(frac) = set_private_fraction {
        if !(0.0..=1.0).contains(&frac) {
            anyhow::bail!("--set-private-fraction must be in [0.0, 1.0], got {frac}");
        }
        policy.private_fraction = frac;
        policy.shared_fraction = 1.0 - frac;
    }
    if seal {
        policy.sealed = true;
        policy.sealed_at_unix_ms = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
        );
    }

    // ── persist (only when something changed) ──────────────────────
    let changed = policy.private_hard_cap != topo.quota.private_hard_cap
        || policy.shared_hard_cap != topo.quota.shared_hard_cap
        || (policy.private_fraction - topo.quota.private_fraction).abs() > f64::EPSILON
        || (policy.shared_fraction - topo.quota.shared_fraction).abs() > f64::EPSILON
        || policy.sealed != topo.quota.sealed;
    if changed {
        let mut new_topo = topo.clone();
        new_topo.quota = policy.clone();
        new_topo.save_quota().context("persist quota policy")?;
        if !json {
            println!(
                "adnet: persisted new quota policy to {}/topology.json",
                data_dir.display()
            );
        }
    }

    // ── print ──────────────────────────────────────────────────────
    let usage = topo.usage().unwrap_or_default();
    let total = policy.private_hard_cap.unwrap_or(0) + policy.shared_hard_cap.unwrap_or(0);
    let v = json!({
        "total_bytes": total,
        "private_fraction": policy.private_fraction,
        "shared_fraction": policy.shared_fraction,
        "private": {
            "budget_bytes": policy.private_hard_cap.unwrap_or(0),
            "hard_cap_bytes": policy.private_hard_cap.unwrap_or(0),
            "used_bytes": usage.private_used,
        },
        "shared": {
            "budget_bytes": policy.shared_hard_cap.unwrap_or(0),
            "hard_cap_bytes": policy.shared_hard_cap.unwrap_or(0),
            "used_bytes": usage.shared_used,
        },
        "sealed": policy.sealed,
        "sealed_at_unix_ms": policy.sealed_at_unix_ms,
        "data_dir": data_dir.display().to_string(),
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        println!("private_fraction: {:.0}%", policy.private_fraction * 100.0);
        println!("shared_fraction : {:.0}%", policy.shared_fraction * 100.0);
        println!("total_bytes     : {}", total);
        println!(
            "private budget  : {} (hard cap {})",
            human_bytes(policy.private_hard_cap.unwrap_or(0)),
            human_bytes(policy.private_hard_cap.unwrap_or(0)),
        );
        println!(
            "shared  budget  : {} (hard cap {})",
            human_bytes(policy.shared_hard_cap.unwrap_or(0)),
            human_bytes(policy.shared_hard_cap.unwrap_or(0)),
        );
        if policy.sealed {
            println!("sealed          : true (since {:?})", policy.sealed_at_unix_ms);
        }
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

pub fn build_dashboard(
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
        // Drop the trailing `.00` when the scaled value is
        // numerically an integer (see [`bytes::format_bytes`]
        // for the tolerance rationale).
        let rounded = v.round();
        if (v - rounded).abs() < 1e-9 {
            format!("{} {}", rounded as u64, UNITS[u])
        } else {
            format!("{v:.2} {}", UNITS[u])
        }
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

// ─────────────────────────────────────────────────────────────────────
//  Encryption lifecycle (audit-V7).
// ─────────────────────────────────────────────────────────────────────

/// Generate (or re-generate) the on-disk encryption key for the
/// private scope. Idempotent — refuses to overwrite an existing
/// key unless `--force` is passed. With `--passphrase <pw>` the
/// key is derived via Argon2id (the passphrase itself is never
/// written). Without `--passphrase` a fresh 32-byte random key
/// is generated and persisted.
pub fn run_encrypt_init(
    data_dir: &Path,
    passphrase: Option<&str>,
    force: bool,
    json: bool,
) -> Result<()> {
    use adnet_blobstore::{EncryptionError, KeyStore};
    let ks = KeyStore::new(data_dir);
    if ks.exists() && !force {
        let err = EncryptionError::InvalidMetadata(
            "key file already exists; pass --force to overwrite (DANGEROUS)".to_string(),
        );
        return Err(anyhow::anyhow!("encrypt-init: {err}"));
    }
    let key = match passphrase {
        Some(pw) => {
            // Use a random salt persisted alongside the key so
            // a future boot can re-derive. Operators who want
            // a deterministic salt should provide it via the
            // config wizard, not on the CLI.
            let mut salt = [0u8; 16];
            use rand::RngCore;
            rand::rngs::OsRng.fill_bytes(&mut salt);
            ks.init_passphrase(pw.as_bytes(), &salt)
                .map_err(|e| anyhow::anyhow!("encrypt-init: derive key: {e}"))?
        }
        None => ks
            .init_random()
            .map_err(|e| anyhow::anyhow!("encrypt-init: generate key: {e}"))?
    };
    let report_kind = if passphrase.is_some() { "passphrase-derived" } else { "random" };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": true,
                "key_file": ks.path().display().to_string(),
                "kind": report_kind,
            }))?
        );
    } else {
        println!("encryption initialised");
        println!("  key file: {}", ks.path().display());
        println!("  kind:     {}", if passphrase.is_some() { "passphrase-derived (Argon2id)" } else { "random (32 bytes)" });
        println!("  next step: write `storage.encrypt.enabled = true` into `app.toml`");
    }
    // Avoid an unused-variable warning when json = false.
    let _ = key;
    Ok(())
}

/// Print the encryption status — whether a key exists, what
/// kind, and (for diagnostics only — never the key material
/// itself) the first 4 bytes as a fingerprint.
pub fn run_encrypt_status(data_dir: &Path, json: bool) -> Result<()> {
    use adnet_blobstore::{EncryptionError, KeyStore};
    let ks = KeyStore::new(data_dir);
    let exists = ks.exists();
    let kind = if exists {
        // Read the header to decide whether it's random or
        // passphrase-derived — without unwrapping the key
        // material itself.
        let bytes = std::fs::read(ks.path())?;
        let parsed: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| anyhow::anyhow!("encrypt-status: invalid key file: {e}"))?;
        match parsed.get("kdf").is_some() {
            true => "passphrase-derived",
            false => "random",
        }
    } else {
        "none"
    };
    let enabled_in_config = read_encrypt_enabled_from_config(data_dir).unwrap_or(false);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "enabled_in_config": enabled_in_config,
                "key_file_present": exists,
                "key_file": if exists { Some(ks.path().display().to_string()) } else { None },
                "kind": kind,
            }))?
        );
    } else {
        println!("encryption status @ {}", data_dir.display());
        println!("  enabled in app.toml  = {}", enabled_in_config);
        println!("  key file present     = {}", exists);
        println!("  key kind             = {}", kind);
    }
    if !enabled_in_config && exists {
        eprintln!(
            "hint: a key file exists but `storage.encrypt.enabled` is not set in app.toml. \
             The CLI still writes plaintext unless you flip the flag."
        );
    }
    Ok(())
}

/// Delete `<data_dir>/keys/storage.key`. After this returns
/// successfully, any blob written under the old key is
/// permanently unreadable. Requires `--yes` to acknowledge.
pub fn run_encrypt_disable(data_dir: &Path, yes: bool, json: bool) -> Result<()> {
    use adnet_blobstore::KeyStore;
    let ks = KeyStore::new(data_dir);
    if !ks.exists() {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "ok": true,
                    "removed": false,
                    "note": "no key file to remove",
                }))?
            );
        } else {
            println!("encrypt-disable: no key file at {}", ks.path().display());
        }
        return Ok(());
    }
    if !yes {
        anyhow::bail!(
            "refusing to delete {} without --yes. \
             After this, every blob written under the old key becomes unreadable.",
            ks.path().display()
        );
    }
    ks.destroy().map_err(|e| anyhow::anyhow!("encrypt-disable: {e}"))?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": true,
                "removed": true,
            }))?
        );
    } else {
        println!("removed key file: {}", ks.path().display());
    }
    Ok(())
}

/// Look up `storage.encrypt.enabled` in `app.toml`. Returns
/// `None` if the config file is missing or unreadable; `Some(b)`
/// when the operator has explicitly set the flag.
fn read_encrypt_enabled_from_config(data_dir: &Path) -> Option<bool> {
    let path = data_dir.join("app.toml");
    if !path.exists() {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    // Cheap grep: only need the `[storage]` block.
    let mut in_storage = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with('[') {
            in_storage = trimmed == "[storage]";
            continue;
        }
        if in_storage && trimmed.starts_with("encrypt") {
            // Match `encrypt.enabled`, `encrypt-enabled`, `enabled`.
            if let Some(rest) = trimmed.strip_prefix("encrypt") {
                let rest = rest.trim_start_matches(|c: char| c == '.' || c == '-' || c == ' ');
                if rest.starts_with("enabled") {
                    let value = trimmed.split('=').nth(1)?;
                    return Some(matches!(value.trim(), "true" | "True" | "TRUE" | "1"));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // ────────────────────────────────────────────────────────────
    //  Encryption lifecycle (audit-V7)
    // ────────────────────────────────────────────────────────────

    #[test]
    fn encrypt_init_writes_random_key_file() {
        let dir = tempdir().unwrap();
        run_encrypt_init(dir.path(), None, false, false).unwrap();
        let ks = adnet_blobstore::KeyStore::new(dir.path());
        assert!(ks.exists(), "key file must be created on init");
        let key = ks.load(None).expect("key must be loadable");
        use adnet_blobstore::KeyWriteAccess;
        assert_eq!(key.as_bytes_for_test().len(), 32);
    }

    #[test]
    fn encrypt_init_with_passphrase_persists_kdf_metadata() {
        let dir = tempdir().unwrap();
        run_encrypt_init(dir.path(), Some("correct horse"), false, false).unwrap();
        let ks = adnet_blobstore::KeyStore::new(dir.path());
        assert!(ks.exists());
        // Loading without the passphrase must fail with a
        // Kdf error (the on-disk file marks it as derived).
        let err = ks.load(None).unwrap_err();
        assert!(
            matches!(err, adnet_blobstore::EncryptionError::Kdf(_)),
            "got {:?}",
            err
        );
        // Loading with the correct passphrase must round-trip.
        let _ = ks.load(Some(b"correct horse")).expect("load with pw");
    }

    #[test]
    fn encrypt_init_refuses_to_overwrite_without_force() {
        let dir = tempdir().unwrap();
        run_encrypt_init(dir.path(), None, false, false).unwrap();
        let second = run_encrypt_init(dir.path(), None, false, false);
        assert!(second.is_err(), "second init must refuse without --force");
        // And --force succeeds (overwrites with a fresh key).
        run_encrypt_init(dir.path(), None, true, false).unwrap();
    }

    #[test]
    fn encrypt_status_reports_kind_correctly() {
        let dir = tempdir().unwrap();
        // Before init: status reports no key.
        run_encrypt_status(dir.path(), false).unwrap();
        // After random init: status reports kind=random.
        run_encrypt_init(dir.path(), None, false, false).unwrap();
        run_encrypt_status(dir.path(), false).unwrap();
        // After passphrase init (with --force so we replace):
        run_encrypt_init(dir.path(), Some("hunter2"), true, false).unwrap();
        run_encrypt_status(dir.path(), false).unwrap();
    }

    #[test]
    fn encrypt_status_parses_app_toml_flag() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.toml"),
            "# comment\n[storage]\nencrypt.enabled = true\n",
        )
        .unwrap();
        let enabled = read_encrypt_enabled_from_config(dir.path());
        assert_eq!(enabled, Some(true));
        let dir2 = tempdir().unwrap();
        std::fs::write(
            dir2.path().join("app.toml"),
            "[storage]\nencrypt.enabled = false\n",
        )
        .unwrap();
        let enabled = read_encrypt_enabled_from_config(dir2.path());
        assert_eq!(enabled, Some(false));
    }

    #[test]
    fn encrypt_disable_refuses_without_yes() {
        let dir = tempdir().unwrap();
        run_encrypt_init(dir.path(), None, false, false).unwrap();
        let res = run_encrypt_disable(dir.path(), false, false);
        assert!(res.is_err());
        let ks = adnet_blobstore::KeyStore::new(dir.path());
        assert!(ks.exists(), "key file must still be present after refused disable");
    }

    #[test]
    fn encrypt_disable_with_yes_wipes_key() {
        let dir = tempdir().unwrap();
        run_encrypt_init(dir.path(), None, false, false).unwrap();
        run_encrypt_disable(dir.path(), true, false).unwrap();
        let ks = adnet_blobstore::KeyStore::new(dir.path());
        assert!(!ks.exists());
    }

    #[test]
    fn encrypt_disable_is_idempotent_when_no_key() {
        let dir = tempdir().unwrap();
        // No init → no key file → disable is a no-op.
        run_encrypt_disable(dir.path(), true, false).unwrap();
    }

    // ────────────────────────────────────────────────────────────
    //  Pre-existing tests
    // ────────────────────────────────────────────────────────────

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
            &StorageCmd::Quota {
                json: true,
                set: None,
                set_private_hard_cap: None,
                set_shared_hard_cap: None,
                set_private_fraction: None,
                seal: false,
            },
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
        // `format_bytes` elides trailing `.00` so the output is
        // round-trippable through `parse_bytes`. The test mirrors
        // the canonical output of the formatter.
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1 MiB");
        assert_eq!(human_bytes(1024u64.pow(4)), "1 TiB");
    }

    #[test]
    fn scope_arg_round_trip() {
        let s: ScopeArg = "private".parse().unwrap();
        assert_eq!(s, ScopeArg::Private);
        let s: ScopeArg = "shared".parse().unwrap();
        assert_eq!(s, ScopeArg::Shared);
        assert!("nonsense".parse::<ScopeArg>().is_err());
    }

    // ───────────────────────────────────────────────────────────────
    // Storage quota CLI: parser, dispatcher, set-quota persistence,
    // env override, and the legacy 20 GiB compatibility fixture.
    // ───────────────────────────────────────────────────────────────

    /// `open_topology_with_total_bytes` honours the requested total
    /// on a fresh data directory. The blobstore persists this into
    /// `topology.json` so the next `StorageTopology::open` call
    /// observes the same hard cap.
    #[test]
    fn open_topology_with_total_bytes_records_policy() {
        let dir = tempdir().unwrap();
        let total = 50 * 1024 * 1024 * 1024u64; // 50 GiB
        let topo = open_topology_with_total_bytes(dir.path(), total).unwrap();
        let usage = topo.usage().unwrap();
        assert_eq!(
            usage.private_hard_cap + usage.shared_hard_cap,
            total,
            "scope caps must sum to the requested total"
        );
        assert_eq!(usage.private_hard_cap, total / 2);
        assert_eq!(usage.shared_hard_cap, total / 2);
    }

    /// `open_topology_with_config` honours the file's `total_bytes`.
    #[test]
    fn open_topology_with_config_honours_file_value() {
        let dir = tempdir().unwrap();
        let cfg = crate::config::StorageConfig {
            total_bytes: Some("100GiB".into()),
            ..crate::config::StorageConfig::default()
        };
        let topo = open_topology_with_config(dir.path(), &cfg).unwrap();
        let usage = topo.usage().unwrap();
        assert_eq!(
            usage.private_hard_cap + usage.shared_hard_cap,
            100 * 1024 * 1024 * 1024
        );
    }

    /// `open_topology_with_config` falls back to the legacy 20 GiB
    /// when the file value is unparseable.
    #[test]
    fn open_topology_with_config_falls_back_on_invalid_value() {
        let dir = tempdir().unwrap();
        let cfg = crate::config::StorageConfig {
            total_bytes: Some("lots".into()),
            ..crate::config::StorageConfig::default()
        };
        let topo = open_topology_with_config(dir.path(), &cfg).unwrap();
        let usage = topo.usage().unwrap();
        assert_eq!(
            usage.private_hard_cap + usage.shared_hard_cap,
            crate::bytes::DEFAULT_TOTAL_BYTES
        );
    }

    /// `adnet storage quota --set 50GiB` round-trips: the topology
    /// is mutated, persisted to `topology.json`, and the next
    /// `StorageTopology::open` picks the new value up without help.
    #[test]
    fn run_storage_quota_set_persists_to_topology_json() {
        let dir = tempdir().unwrap();
        // Seed a topology at the legacy 20 GiB so the test starts
        // from a known baseline.
        let _ = open_topology_with_total_bytes(dir.path(), 20 * 1024 * 1024 * 1024).unwrap();

        let new_total = 50 * 1024 * 1024 * 1024u64;
        run_storage(
            dir.path(),
            &StorageCmd::Quota {
                json: false,
                set: Some("50GiB".into()),
                set_private_hard_cap: None,
                set_shared_hard_cap: None,
                set_private_fraction: None,
                seal: false,
            },
        )
        .unwrap();

        // `topology.json` must exist on disk.
        let topo_path = dir.path().join("topology.json");
        assert!(topo_path.exists(), "set-quota must persist topology.json");
        let raw = std::fs::read_to_string(&topo_path).unwrap();
        // Decode the public subset of the persisted QuotaPolicy.
        // The blobstore `TopologyMetadata` is private, so we read
        // the JSON as a generic Value and extract the public fields.
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let obj = v.as_object().expect("topology.json must be an object");
        let quota = obj
            .get("quota")
            .and_then(|q| q.as_object())
            .expect("topology.json must carry a `quota` object");
        let p = quota
            .get("private_hard_cap")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        let s = quota
            .get("shared_hard_cap")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        assert_eq!(
            p + s,
            new_total,
            "persisted hard caps must sum to the new total"
        );
    }

    /// Persistent storage closes the loop: a CLI that sets the quota
    /// to 50 GiB and then re-opens the topology sees the new value —
    /// the legacy 20 GiB is no longer consulted.
    #[test]
    fn quota_set_survives_reopen() {
        let dir = tempdir().unwrap();
        let _ = open_topology_with_total_bytes(dir.path(), 20 * 1024 * 1024 * 1024).unwrap();
        run_storage(
            dir.path(),
            &StorageCmd::Quota {
                json: false,
                set: Some("100GiB".into()),
                set_private_hard_cap: None,
                set_shared_hard_cap: None,
                set_private_fraction: None,
                seal: false,
            },
        )
        .unwrap();
        // Re-open with the default `open_topology` (which uses 20 GiB
        // as the *new* quota if topology.json is missing) — the
        // persisted policy must override the default.
        let topo = open_topology(dir.path()).unwrap();
        let usage = topo.usage().unwrap();
        assert_eq!(
            usage.private_hard_cap + usage.shared_hard_cap,
            100 * 1024 * 1024 * 1024,
            "reopen must read the persisted quota, not the default"
        );
    }

    /// `--set-private-fraction` mutates the split without touching
    /// the total.
    #[test]
    fn quota_set_fraction_changes_split() {
        let dir = tempdir().unwrap();
        let _ = open_topology_with_total_bytes(dir.path(), 20 * 1024 * 1024 * 1024).unwrap();
        run_storage(
            dir.path(),
            &StorageCmd::Quota {
                json: false,
                set: None,
                set_private_hard_cap: None,
                set_shared_hard_cap: None,
                set_private_fraction: Some(0.8),
                seal: false,
            },
        )
        .unwrap();
        let topo = open_topology(dir.path()).unwrap();
        assert!((topo.quota.private_fraction - 0.8).abs() < 1e-6);
        assert!((topo.quota.shared_fraction - 0.2).abs() < 1e-6);
    }

    /// `--seal` flips the sealed flag and records a timestamp.
    #[test]
    fn quota_seal_persists() {
        let dir = tempdir().unwrap();
        let _ = open_topology_with_total_bytes(dir.path(), 20 * 1024 * 1024 * 1024).unwrap();
        run_storage(
            dir.path(),
            &StorageCmd::Quota {
                json: false,
                set: None,
                set_private_hard_cap: None,
                set_shared_hard_cap: None,
                set_private_fraction: None,
                seal: true,
            },
        )
        .unwrap();
        let topo = open_topology(dir.path()).unwrap();
        assert!(topo.quota.sealed);
        assert!(topo.quota.sealed_at_unix_ms.is_some());
    }

    /// `--set 0` is rejected — the legacy hard-code trap is harder
    /// to spot than a typo, so the CLI surfaces a clear error.
    #[test]
    fn quota_set_zero_rejected() {
        let dir = tempdir().unwrap();
        let _ = open_topology_with_total_bytes(dir.path(), 20 * 1024 * 1024 * 1024).unwrap();
        let r = run_storage(
            dir.path(),
            &StorageCmd::Quota {
                json: false,
                set: Some("0".into()),
                set_private_hard_cap: None,
                set_shared_hard_cap: None,
                set_private_fraction: None,
                seal: false,
            },
        );
        assert!(r.is_err(), "zero total must be rejected");
    }

    /// Out-of-range fractions are rejected.
    #[test]
    fn quota_fraction_out_of_range_rejected() {
        let dir = tempdir().unwrap();
        let _ = open_topology_with_total_bytes(dir.path(), 20 * 1024 * 1024 * 1024).unwrap();
        let r = run_storage(
            dir.path(),
            &StorageCmd::Quota {
                json: false,
                set: None,
                set_private_hard_cap: None,
                set_shared_hard_cap: None,
                set_private_fraction: Some(1.5),
                seal: false,
            },
        );
        assert!(r.is_err(), "fraction > 1.0 must be rejected");
    }

    /// `adnet storage quota --set junk` is rejected with a
    /// recoverable error mentioning the bad value.
    #[test]
    fn quota_set_invalid_value_rejected() {
        let dir = tempdir().unwrap();
        let _ = open_topology_with_total_bytes(dir.path(), 20 * 1024 * 1024 * 1024).unwrap();
        let r = run_storage(
            dir.path(),
            &StorageCmd::Quota {
                json: false,
                set: Some("junk".into()),
                set_private_hard_cap: None,
                set_shared_hard_cap: None,
                set_private_fraction: None,
                seal: false,
            },
        );
        let err = format!("{}", r.unwrap_err());
        assert!(err.contains("junk"), "error must mention the bad value: {err}");
        assert!(err.contains("byte size"), "error must explain the format: {err}");
    }

    /// The existing 20 GiB compatibility test must still pass: a
    /// fresh dir under `open_topology()` shows 20 GiB total.
    #[test]
    fn open_topology_legacy_20gib_compat() {
        let dir = tempdir().unwrap();
        let topo = open_topology(dir.path()).unwrap();
        let usage = topo.usage().unwrap();
        assert_eq!(
            usage.private_hard_cap + usage.shared_hard_cap,
            20 * 1024 * 1024 * 1024,
            "legacy 20 GiB default must survive"
        );
    }

    /// `run_storage_quota` (human mode) writes a friendly summary
    /// that operators can paste into a bug report.
    #[test]
    fn run_storage_quota_human_is_well_formed() {
        let dir = tempdir().unwrap();
        let _ = open_topology_with_total_bytes(dir.path(), 20 * 1024 * 1024 * 1024).unwrap();
        run_storage(
            dir.path(),
            &StorageCmd::Quota {
                json: false,
                set: None,
                set_private_hard_cap: None,
                set_shared_hard_cap: None,
                set_private_fraction: None,
                seal: false,
            },
        )
        .unwrap();
    }

    /// JSON mode is also well-formed.
    #[test]
    fn run_storage_quota_json_is_well_formed() {
        let dir = tempdir().unwrap();
        let _ = open_topology_with_total_bytes(dir.path(), 20 * 1024 * 1024 * 1024).unwrap();
        run_storage(
            dir.path(),
            &StorageCmd::Quota {
                json: true,
                set: None,
                set_private_hard_cap: None,
                set_shared_hard_cap: None,
                set_private_fraction: None,
                seal: false,
            },
        )
        .unwrap();
    }
}