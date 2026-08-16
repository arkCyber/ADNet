//! `a3chat whoami` — print the resolved owner identity.

use crate::config::CliConfig;
use crate::error::CliResult;
use crate::output;

/// Print the configured owner identity. Exits non-zero if the
/// placeholder is still in use (DO-178C §8 — fail-safe).
pub async fn run(cfg: &CliConfig) -> CliResult<()> {
    let owner = cfg.effective_owner();
    if owner == crate::config::DEFAULT_OWNER {
        eprintln!(
            "warning: owner is the all-zeros placeholder — set --owner or A3CHAT_OWNER"
        );
    }
    let daemon = cfg.effective_daemon_url();
    output::print(
        cfg.effective_output(),
        &serde_json::json!({
            "owner": owner,
            "daemon_url": daemon,
        }),
    )
}