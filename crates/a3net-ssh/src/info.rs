//! `info` command — print the SSH tunnel endpoint id a friend would dial.
//!
//! This is the A3Net equivalent of iroh-ssh's `info` subcommand.
//! It reuses the persistent identity so the value printed here
//! matches the value the rest of the A3Net runtime publishes
//! (gossip topics, blob tickets, etc.).

use std::fmt::Write as _;
use std::path::Path;

use crate::error::SshResult;

#[cfg(feature = "iroh")]
use crate::keys::persistent_identity;

/// Render the SSH-tunnel invitation as a human-readable block.
///
/// Returns a multi-line string suitable for `println!`-ing from
/// the CLI or the REPL. The block contains:
///
/// - the crate version
/// - the persistent server endpoint id
/// - the `a3net-ssh user@<id>` invitation line the operator copies
///   into chat
///
/// # Example
///
/// ```no_run
/// # #[cfg(not(feature = "iroh"))]
/// # {
/// // When the `iroh` feature is disabled, the banner explains
/// // the feature gap instead of returning identity info.
/// use a3net_ssh::info::render_invite;
/// let dir = std::env::temp_dir();
/// let out = render_invite(&dir).unwrap();
/// assert!(out.contains("built without `iroh` feature"));
/// # }
/// ```
pub fn render_invite(data_dir: &Path) -> SshResult<String> {
    // We use `?` instead of `let _ =` so a future `fmt::Write`
    // impl that *can* fail surfaces the error rather than
    // silently swallowing it. `String` never errors today, so
    // this is purely a future-proofing change.
    let mut out = String::new();
    writeln!(out, "a3net-ssh {}", env!("CARGO_PKG_VERSION"))?;
    writeln!(
        out,
        "https://github.com/rustonbsd/iroh-ssh (vendored as a3net-ssh)"
    )?;
    writeln!(out)?;

    #[cfg(feature = "iroh")]
    {
        let identity = persistent_identity(data_dir)?;
        let endpoint_id = identity.endpoint_id();
        let node_id = identity.node_id();
        let short = node_id.short();
        let user = current_user();
        writeln!(out, "Your a3net-ssh endpoint id: {endpoint_id}")?;
        writeln!(out, "  (short: a3net-{short})")?;
        writeln!(out)?;
        writeln!(out, "Your server a3net-ssh invite:")?;
        writeln!(out, "  a3net ssh connect {user}@{endpoint_id}")?;
        writeln!(out)?;
        writeln!(out, "Identity file: {}", identity.path().display())?;
    }
    #[cfg(not(feature = "iroh"))]
    {
        // `data_dir` is unused in this branch but we still want
        // the parameter for API parity with the `iroh`-feature
        // version. Touch it explicitly so the `unused_variables`
        // lint stays clean across feature flags.
        let _ = data_dir;
        writeln!(out, "(built without `iroh` feature — identity unavailable)")?;
        writeln!(
            out,
            "Enable the `iroh` feature on this crate to render an endpoint id."
        )?;
    }
    Ok(out)
}

/// Best-effort current shell user. Falls back to `"UNKNOWN_USER"`
/// when `whoami` can't determine it.
#[cfg(feature = "iroh")]
fn current_user() -> String {
    whoami::fallible::username().unwrap_or_else(|_| "UNKNOWN_USER".to_string())
}

/// Stub used when the `iroh` feature is off (e.g. from a
/// documentation build). The invite text is still useful for
/// explaining the surface area, just without the user component.
#[cfg(not(feature = "iroh"))]
#[allow(dead_code)]
fn current_user() -> String {
    "user".to_string()
}
