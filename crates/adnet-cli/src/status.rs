//! `adnet status` command — convenience wrapper around
//! `storage::run_status` exposed as its own top-level
//! subcommand so operators can type `adnet status`
//! without going through `adnet storage …`.

use std::path::Path;

use anyhow::{Context, Result};

use crate::storage::run_status as storage_run_status;

/// Dispatch `adnet status [--json]`. Equivalent to
/// `adnet storage info --json` plus replication counters
/// from the global metric registry. Offline-only — does
/// not start the node.
pub fn run_status(data_dir: &Path, json: bool) -> Result<()> {
    storage_run_status(data_dir, json)
        .with_context(|| format!("status snapshot for {}", data_dir.display()))
}