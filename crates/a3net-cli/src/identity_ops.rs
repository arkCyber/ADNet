//! `a3net identity …`, `a3net contacts …`, `a3net profile-page …` —
//! offline subcommands for managing the local node's identity store
//! and contacts list.
//!
//! These commands open the same `<data_dir>/node_identity.json` and
//! `<data_dir>/contacts.json` files that the daemons load at
//! startup. They are **offline** in the sense that they do not
//! require the mesh / gossip transports to be running — they read
//! and write the local JSON documents directly. This matches the
//! `a3net config` pattern (the store is opened and closed per
//! invocation).
//!
//! The full IPC surface (`contacts.bump_reputation`, `profile.html`,
//! …) is exposed for callers that want to script against a
//! running daemon; the CLI subcommands here are the operator
//! path for the same operations.

use std::path::Path;

use a3net_node::{
    ContactsManager, NodeConfig, NodeIdentityStore, NODE_IDENTITY_FILE_VERSION,
};
use a3net_types::{
    Avatar, ContactEntry, ContactSource, DnsNodeId, NodeId, WalletAddress,
};
use anyhow::{Context, Result, bail};

/// `a3net identity <sub>` subcommands.
#[derive(Debug, Clone)]
pub enum IdentityCmd {
    /// Print the local node's identity as JSON.
    Get,
    /// Set the local node's email address.
    SetEmail { email: String },
    /// Set the local node's nickname.
    SetNickname { nickname: String },
    /// Set the local node's 128-char description.
    SetDescription { description: String },
    /// Set the local node's avatar URL.
    SetAvatar { url: String },
    /// Set the local node's wallet address (0x-prefixed hex).
    SetWallet { wallet: String },
    /// Set the local node's DNS-assigned 12-digit id.
    SetDnsNodeId { dns_node_id: String },
}

impl IdentityCmd {
    pub fn from_cli(
        cli: &super::cli::IdentityCmd,
    ) -> Self {
        match cli {
            super::cli::IdentityCmd::Get => Self::Get,
            super::cli::IdentityCmd::SetEmail { email } => Self::SetEmail {
                email: email.clone(),
            },
            super::cli::IdentityCmd::SetNickname { nickname } => Self::SetNickname {
                nickname: nickname.clone(),
            },
            super::cli::IdentityCmd::SetDescription { description } => {
                Self::SetDescription {
                    description: description.clone(),
                }
            }
            super::cli::IdentityCmd::SetAvatar { url } => Self::SetAvatar {
                url: url.clone(),
            },
            super::cli::IdentityCmd::SetWallet { wallet } => Self::SetWallet {
                wallet: wallet.clone(),
            },
            super::cli::IdentityCmd::SetDnsNodeId { dns_node_id } => {
                Self::SetDnsNodeId {
                    dns_node_id: dns_node_id.clone(),
                }
            }
        }
    }
}

/// `a3net contacts <sub>` subcommands.
#[derive(Debug, Clone)]
pub enum ContactsCmd {
    List { json: bool },
    Get { node_id: String },
    Add { node_id: String, nickname: String },
    Remove { node_id: String },
    Rename { node_id: String, nickname: String },
    SetBlocked { node_id: String, blocked: bool },
    BumpReputation { node_id: String, delta: u32 },
    SetReputation { node_id: String, reputation: u32 },
    ReputationSummary { json: bool },
}

impl ContactsCmd {
    pub fn from_cli(cli: &super::cli::ContactsCmd) -> Self {
        match cli {
            super::cli::ContactsCmd::List { json } => Self::List { json: *json },
            super::cli::ContactsCmd::Get { node_id } => Self::Get {
                node_id: node_id.clone(),
            },
            super::cli::ContactsCmd::Add { node_id, nickname } => Self::Add {
                node_id: node_id.clone(),
                nickname: nickname.clone(),
            },
            super::cli::ContactsCmd::Remove { node_id } => Self::Remove {
                node_id: node_id.clone(),
            },
            super::cli::ContactsCmd::Rename { node_id, nickname } => Self::Rename {
                node_id: node_id.clone(),
                nickname: nickname.clone(),
            },
            super::cli::ContactsCmd::SetBlocked { node_id, blocked } => Self::SetBlocked {
                node_id: node_id.clone(),
                blocked: *blocked,
            },
            super::cli::ContactsCmd::BumpReputation { node_id, delta } => {
                Self::BumpReputation {
                    node_id: node_id.clone(),
                    delta: *delta,
                }
            }
            super::cli::ContactsCmd::SetReputation {
                node_id,
                reputation,
            } => Self::SetReputation {
                node_id: node_id.clone(),
                reputation: *reputation,
            },
            super::cli::ContactsCmd::ReputationSummary { json } => {
                Self::ReputationSummary { json: *json }
            }
        }
    }
}

/// `a3net profile-page <sub>` subcommands.
#[derive(Debug, Clone)]
pub enum ProfilePageCmd {
    /// Render the local profile page and write it to a file.
    Render { out: String },
    /// Print the local profile page to stdout.
    Print,
}

impl ProfilePageCmd {
    pub fn from_cli(cli: &super::cli::ProfilePageCmd) -> Self {
        match cli {
            super::cli::ProfilePageCmd::Render { out } => Self::Render {
                out: out.clone(),
            },
            super::cli::ProfilePageCmd::Print => Self::Print,
        }
    }
}

/// Open the local identity store + contacts manager pair rooted at
/// `data_dir`. Returns `(identity, contacts)`. Both stores open
/// the same way the daemon does — failure to open either is
/// propagated.
fn open_stores(
    data_dir: &Path,
) -> Result<(NodeIdentityStore, ContactsManager)> {
    let cfg = NodeConfig::load_or_create(data_dir)
        .with_context(|| format!("resolving node config from {}", data_dir.display()))?;
    let identity = NodeIdentityStore::open(data_dir, cfg.node_id.clone())
        .context("opening node identity store")?;
    let contacts = ContactsManager::open(data_dir)
        .context("opening contacts manager")?;
    Ok((identity, contacts))
}

/// Dispatch the identity subcommand.
pub fn run_identity(cmd: &IdentityCmd, data_dir: &Path) -> Result<()> {
    let (identity, _contacts) = open_stores(data_dir)?;
    match cmd {
        IdentityCmd::Get => {
            let snap = identity.snapshot();
            println!("{}", serde_json::to_string_pretty(&snap)?);
        }
        IdentityCmd::SetEmail { email } => {
            identity
                .set_email(email.as_str())
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("email set");
        }
        IdentityCmd::SetNickname { nickname } => {
            identity
                .set_nickname(nickname.as_str())
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("nickname set");
        }
        IdentityCmd::SetDescription { description } => {
            identity
                .set_description(description.as_str())
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("description set");
        }
        IdentityCmd::SetAvatar { url } => {
            let avatar = Avatar::from_url(url)
                .map_err(|e| anyhow::anyhow!("invalid avatar URL: {e}"))?;
            identity
                .set_avatar(avatar)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("avatar set");
        }
        IdentityCmd::SetWallet { wallet } => {
            let addr = WalletAddress::from_hex(wallet)
                .map_err(|e| anyhow::anyhow!("invalid wallet hex: {e}"))?;
            identity
                .set_wallet_address(addr)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("wallet set");
        }
        IdentityCmd::SetDnsNodeId { dns_node_id } => {
            let dns = DnsNodeId::parse(dns_node_id)
                .map_err(|e| anyhow::anyhow!("invalid dns_node_id: {e}"))?;
            identity
                .set_dns_node_id(dns)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("dns_node_id set");
        }
    }
    Ok(())
}

/// Dispatch the contacts subcommand.
pub fn run_contacts(cmd: &ContactsCmd, data_dir: &Path) -> Result<()> {
    let (_identity, contacts) = open_stores(data_dir)?;
    match cmd {
        ContactsCmd::List { json } => {
            let entries = contacts.snapshot();
            if *json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else {
                print_pretty(&entries);
            }
        }
        ContactsCmd::Get { node_id } => {
            let id = parse_node_id(node_id)?;
            match contacts.get(&id) {
                Some(entry) => println!("{}", serde_json::to_string_pretty(&entry)?),
                None => bail!("contact not found: {node_id}"),
            }
        }
        ContactsCmd::Add { node_id, nickname } => {
            let id = parse_node_id(node_id)?;
            let entry = contacts
                .upsert_manual(id.clone(), nickname.as_str())
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!(
                "added contact {} ({})",
                entry.node_id.as_hex(),
                entry.nickname
            );
        }
        ContactsCmd::Remove { node_id } => {
            let id = parse_node_id(node_id)?;
            let entry = contacts
                .remove(&id)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("removed contact {}", entry.node_id.as_hex());
        }
        ContactsCmd::Rename { node_id, nickname } => {
            let id = parse_node_id(node_id)?;
            contacts
                .rename(&id, nickname.as_str())
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("renamed");
        }
        ContactsCmd::SetBlocked { node_id, blocked } => {
            let id = parse_node_id(node_id)?;
            contacts
                .set_blocked(&id, *blocked)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!(
                "contact {} {}",
                id.as_hex(),
                if *blocked { "blocked" } else { "unblocked" }
            );
        }
        ContactsCmd::BumpReputation { node_id, delta } => {
            let id = parse_node_id(node_id)?;
            let new = contacts
                .bump_reputation(&id, *delta)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("reputation is now {new} (after +{delta})");
        }
        ContactsCmd::SetReputation {
            node_id,
            reputation,
        } => {
            let id = parse_node_id(node_id)?;
            let new = contacts
                .set_reputation(&id, *reputation)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("reputation set to {new}");
        }
        ContactsCmd::ReputationSummary { json } => {
            let s = contacts.reputation_summary();
            if *json {
                println!("{}", serde_json::to_string_pretty(&s)?);
            } else {
                println!("{s:#?}");
            }
        }
    }
    Ok(())
}

/// Dispatch the profile-page subcommand. The CLI opens the
/// stores in offline mode and re-uses the same renderer the
/// daemon uses. The output is byte-stable for the same identity
/// snapshot, so it's safe to `diff` profile pages between
/// versions.
pub fn run_profile_page(cmd: &ProfilePageCmd, data_dir: &Path) -> Result<()> {
    let (identity, contacts) = open_stores(data_dir)?;
    let id = identity.snapshot();
    let profile = a3net_types::NodeProfile::standard(id.digital_identity.clone(), "");
    let rep = contacts.reputation_summary();
    let count = contacts.len();
    let inputs = a3net_node::ProfilePageInputs::new(
        &id,
        Some(&profile),
        rep,
        count,
    );
    let html = a3net_node::render_profile_html(&inputs);
    match cmd {
        ProfilePageCmd::Render { out } => {
            std::fs::write(out, &html)
                .with_context(|| format!("writing profile to {out}"))?;
            println!(
                "wrote {} bytes to {out}",
                html.len(),
            );
        }
        ProfilePageCmd::Print => {
            // Print to stdout. Pipeline-friendly: `a3net profile-page
            // print > out.html`.
            println!("{html}");
        }
    }
    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────

fn parse_node_id(s: &str) -> Result<NodeId> {
    NodeId::from_hex(s).with_context(|| format!("invalid node_id hex: {s}"))
}

fn print_pretty(entries: &[ContactEntry]) {
    if entries.is_empty() {
        println!("(no contacts)");
        return;
    }
    println!(
        "{:<12} {:<66} {:<24} {:<8} {:<10} source",
        "dns_id", "node_id", "nickname", "rep", "tier"
    );
    println!("{}", "-".repeat(140));
    for entry in entries {
        let dns = entry
            .dns_node_id
            .map(|d| d.to_string())
            .unwrap_or_else(|| "-".to_string());
        let short = node_id_short(&entry.node_id);
        let tier = entry.reputation_tier().as_str();
        println!(
            "{:<12} {:<66} {:<24} {:<8} {:<10} {}",
            dns,
            short,
            truncate(&entry.nickname, 24),
            entry.reputation,
            tier,
            entry.source.as_str(),
        );
    }
}

fn node_id_short(id: &NodeId) -> String {
    let hex = id.as_hex();
    if hex.len() <= 16 {
        hex.to_string()
    } else {
        format!("{}…{}", &hex[..8], &hex[hex.len() - 8..])
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Marker that the local identity file's schema version is
/// what we expect. The CLI rejects a major-version mismatch
/// the same way the daemon does.
pub fn check_schema_version(data_dir: &Path) -> Result<()> {
    let path = data_dir.join("node_identity.json");
    if !path.exists() {
        return Ok(());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let v: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("parsing {}", path.display()))?;
    if let Some(file_version) = v.get("version").and_then(|v| v.as_u64()) {
        if file_version as u32 != NODE_IDENTITY_FILE_VERSION {
            bail!(
                "node_identity.json schema version {file_version} does not match CLI expected version {NODE_IDENTITY_FILE_VERSION}"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_print_does_not_panic() {
        let v: Vec<ContactEntry> = vec![];
        print_pretty(&v);
    }

    #[test]
    fn truncate_works() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hell…");
    }

    #[test]
    fn node_id_short_round_trip() {
        let id = NodeId::random();
        let s = node_id_short(&id);
        // 8 prefix bytes + 3-byte ellipsis + 8 suffix bytes
        assert!(s.len() <= 19, "got: {s:?} (len={})", s.len());
    }
}
