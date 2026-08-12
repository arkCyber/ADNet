//! `adnet roster …` — offline contact-directory subcommands.
//!
//! Each subcommand opens the SQLite-backed [`SqliteRosterStore`]
//! under `<data_dir>/roster.sqlite`, runs the requested operation,
//! and prints either a one-line message or the requested JSON
//! payload. The store is opened and closed per invocation so the
//! CLI never holds a long-lived database handle (matches the same
//! pattern as `adnet config`).
//!
//! ## Sync vs async
//!
//! The [`RosterStore`] trait is `async_trait` for downstream
//! consumers that need non-blocking I/O. The CLI does not — every
//! operation is a single SQLite call that holds the connection
//! mutex for microseconds. We use the concrete `SqliteRosterStore`
//! type directly. The dispatched [`run`] is `async`, but the
//! binary's `#[tokio::main]` (current-thread) drives it through
//! `futures::executor::block_on` at the dispatcher boundary in
//! `main.rs` — *not* via `Handle::block_on`, which would panic on
//! the current-thread runtime.
//!
//! ## Input validation
//!
//! `contact_id`, `digit_id`, and `node_id` are user-supplied
//! identifiers. We length-bound them against the same
//! `MAX_CONTACT_NAME_LEN` / digit limits used by the SQLite
//! validators, so a deployment script that holds a 1 MB id is
//! rejected up-front rather than after a wasteful round-trip.

use std::io::Read;
use std::path::Path;

use adnet_roster::{
    Contact, ContactGroup, ContactType, DigitMapping, RosterError, RosterStore,
    SqliteRosterStore, SqliteRosterStoreConfig,
    digit::validate_digit_id,
    model::MAX_CONTACT_NAME_LEN,
};
use anyhow::{Context, Result, anyhow, bail};

/// Maximum length for a `contact_id` / `node_id` supplied on the
/// CLI. The store itself does not enforce this — it caps at the
/// `SQLITE_MAX_LENGTH` ceiling (1 GB by default) — but a CLI
/// input beyond this is almost certainly a copy-paste mistake.
const MAX_ID_LEN: usize = 256;

/// Validation helper for CLI-supplied identifiers. Returns the
/// trimmed string on success; rejects whitespace-only, empty,
/// or over-long inputs with a clear error message.
fn validate_cli_id(label: &str, raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("{label} must be non-empty");
    }
    if trimmed.len() > MAX_ID_LEN {
        bail!(
            "{label} is {n} bytes; the CLI rejects ids longer than {max}",
            n = trimmed.len(),
            max = MAX_ID_LEN
        );
    }
    Ok(trimmed.to_string())
}

/// Apply `cmd` against the roster store rooted at `data_dir`.
pub async fn run(cmd: &super::cli::RosterCmd, data_dir: &Path) -> Result<()> {
    let store = SqliteRosterStore::open(SqliteRosterStoreConfig::under_app_data(data_dir))
        .context("opening roster sqlite")?;
    match cmd {
        super::cli::RosterCmd::Add { from_file } => {
            let contact = read_contact_from_arg(from_file.as_deref())?;
            // Defensive: the store validates the same fields,
            // but a CLI-side check makes the error surface
            // (parser line) and the storage error message
            // aligned.
            if contact.contact_id.is_empty() {
                bail!("contact.contactId must be non-empty");
            }
            if contact.name.len() > MAX_CONTACT_NAME_LEN {
                bail!(
                    "contact.name is {n} chars; capped at {max}",
                    n = contact.name.len(),
                    max = MAX_CONTACT_NAME_LEN
                );
            }
            let id = contact.contact_id.clone();
            store.put_contact(contact).await?;
            println!("added contact {id}");
        }
        super::cli::RosterCmd::List { r#type, json } => {
            let mut all = store.list_contacts().await?;
            if let Some(t) = r#type {
                ContactType::from_str(t)
                    .with_context(|| format!("invalid --type {t:?}; expected human|agent|iot"))?;
                all.retain(|c| c.contact_type == *t);
            }
            if *json {
                println!("{}", serde_json::to_string_pretty(&all)?);
            } else {
                println_pretty(&all);
            }
        }
        super::cli::RosterCmd::Search { query, json } => {
            let hits = store.search_contacts(query).await?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&hits)?);
            } else {
                println_pretty(&hits);
            }
        }
        super::cli::RosterCmd::Show { contact_id, json } => {
            let id = validate_cli_id("contact_id", contact_id)?;
            let c = store
                .get_contact(&id)
                .await?
                .ok_or_else(|| anyhow!("contact not found: {id}"))?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&c)?);
            } else {
                println_pretty(&[c]);
            }
        }
        super::cli::RosterCmd::Delete { contact_id, yes } => {
            let id = validate_cli_id("contact_id", contact_id)?;
            if !yes {
                eprintln!("adnet: about to delete contact {id}");
                eprintln!("hint: pass --yes to skip this prompt");
                eprintln!("continue? [y/N]");
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                if !matches!(line.trim().to_lowercase().as_str(), "y" | "yes") {
                    eprintln!("aborted");
                    return Ok(());
                }
            }
            if store.delete_contact(&id).await? {
                println!("deleted contact {id}");
            } else {
                bail!("contact not found: {id}");
            }
        }
        super::cli::RosterCmd::GroupCreate { from_file } => {
            let group = read_group_from_arg(from_file.as_deref())?;
            if group.group_id.is_empty() {
                bail!("group.groupId must be non-empty");
            }
            let id = group.group_id.clone();
            store.put_group(group).await?;
            println!("added group {id}");
        }
        super::cli::RosterCmd::GroupList { json } => {
            let groups = store.list_groups().await?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&groups)?);
            } else {
                println!("{:<24} {:<24} {:<8} created_at", "group_id", "name", "color");
                for g in groups {
                    println!(
                        "{:<24} {:<24} {:<8} {}",
                        g.group_id, g.name, g.color, g.created_at
                    );
                }
            }
        }
        super::cli::RosterCmd::DigitAdd { digit_id, node_id } => {
            let digit = validate_cli_id("digit_id", digit_id)?;
            let node = validate_cli_id("node_id", node_id)?;
            validate_digit_id(&digit)
                .map_err(|e| anyhow!("invalid digit_id {digit:?}: {e}"))?;
            let mapping = DigitMapping::new(digit.clone(), node.clone());
            match store.put_digit_mapping(mapping).await {
                Ok(()) => println!("mapped {digit} -> {node}"),
                Err(RosterError::AlreadyExists { id, .. }) => {
                    bail!("a mapping for {id} already exists")
                }
                Err(e) => return Err(e.into()),
            }
        }
        super::cli::RosterCmd::DigitResolve { digit_id } => {
            let digit = validate_cli_id("digit_id", digit_id)?;
            match store.resolve_digit_to_node(&digit).await? {
                Some(node) => println!("{digit} -> {node}"),
                None => bail!("no mapping for {digit}"),
            }
        }
        super::cli::RosterCmd::Info => {
            let info = store.info();
            println!("backend: {}", info.backend);
            if let Some(loc) = info.location {
                println!("location: {loc}");
            }
            println!("contacts: {}", info.contact_count);
            println!("groups: {}", info.group_count);
            println!("digit mappings: {}", info.digit_mapping_count);
        }
    }
    Ok(())
}

fn read_contact_from_arg(path: Option<&str>) -> Result<Contact> {
    let raw = read_stdin_or_file(path)?;
    serde_json::from_str(&raw).with_context(|| "parsing contact JSON")
}

fn read_group_from_arg(path: Option<&str>) -> Result<ContactGroup> {
    let raw = read_stdin_or_file(path)?;
    serde_json::from_str(&raw).with_context(|| "parsing group JSON")
}

fn read_stdin_or_file(path: Option<&str>) -> Result<String> {
    if let Some(p) = path {
        std::fs::read_to_string(p).with_context(|| format!("reading {p}"))
    } else {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        let trimmed = buf.trim();
        if trimmed.is_empty() {
            bail!("expected JSON on stdin, got empty input");
        }
        Ok(trimmed.to_string())
    }
}

fn println_pretty(contacts: &[Contact]) {
    println!(
        "{:<24} {:<24} {:<8} {:<20} node_id",
        "contact_id", "name", "type", "groups"
    );
    for c in contacts {
        let groups = if c.groups.is_empty() {
            "-".to_string()
        } else {
            c.groups.join(",")
        };
        println!(
            "{:<24} {:<24} {:<8} {:<20} {}",
            &truncate(&c.contact_id, 24),
            &truncate(&c.name, 24),
            &c.contact_type,
            &truncate(&groups, 20),
            &c.node_id,
        );
    }
}

/// Truncate a string to `max` *characters* (not bytes), appending
/// `…` when truncation happened. Keeps table alignment on
/// multi-byte UTF-8 inputs.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_handles_multibyte() {
        let s = "中文测试中文测试";
        let t = truncate(s, 5);
        assert_eq!(t.chars().count(), 5);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn truncate_passes_through_short() {
        assert_eq!(truncate("hi", 5), "hi");
    }

    #[test]
    fn validate_cli_id_rejects_empty() {
        let err = validate_cli_id("contact_id", "").unwrap_err();
        assert!(err.to_string().contains("non-empty"));
    }

    #[test]
    fn validate_cli_id_rejects_whitespace_only() {
        let err = validate_cli_id("contact_id", "  \t\n ").unwrap_err();
        assert!(err.to_string().contains("non-empty"));
    }

    #[test]
    fn validate_cli_id_rejects_oversized() {
        let too_long = "a".repeat(MAX_ID_LEN + 1);
        let err = validate_cli_id("contact_id", &too_long).unwrap_err();
        assert!(err.to_string().contains("CLI rejects ids longer than"));
    }

    #[test]
    fn validate_cli_id_trims_and_accepts() {
        let got = validate_cli_id("contact_id", "  alice  ").unwrap();
        assert_eq!(got, "alice");
    }
}