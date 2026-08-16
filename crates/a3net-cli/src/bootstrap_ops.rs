//! `a3net bootstrap <sub>` — manage bootstrap peer list.
//!
//! Mirrors Kubo `ipfs bootstrap`:
//! <https://docs.ipfs.tech/reference/kubo/cli/#ipfs-bootstrap>

use std::path::Path;
use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::cli::BootstrapCmd;

/// Bootstrap list stored at `<data_dir>/bootstrap_peers.json`.
const BOOTSTRAP_FILE: &str = "bootstrap_peers.json";

/// A single bootstrap peer entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapPeer {
    /// Multiaddr string, e.g. `/ip4/1.2.3.4/udp/4560/quic`.
    pub addr: String,
    /// Optional human-readable alias.
    #[serde(default)]
    pub alias: Option<String>,
}

impl BootstrapPeer {
    pub fn new(addr: String) -> Self {
        Self { addr, alias: None }
    }
}

/// Load the bootstrap peer list from disk.
fn load(data_dir: &Path) -> anyhow::Result<Vec<BootstrapPeer>> {
    let path = data_dir.join(BOOTSTRAP_FILE);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading bootstrap file at {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| "parsing bootstrap_peers.json")
}

/// Persist the bootstrap peer list to disk atomically
/// (write-temp + rename).
fn save(data_dir: &Path, peers: &[BootstrapPeer]) -> anyhow::Result<()> {
    let path = data_dir.join(BOOTSTRAP_FILE);
    let tmp = path.with_extension("tmp");
    let text = serde_json::to_string_pretty(peers)?;
    std::fs::write(&tmp, text)
        .with_context(|| format!("writing bootstrap temp file {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("committing bootstrap file to {}", path.display()))?;
    Ok(())
}

pub fn run_bootstrap(sub: &BootstrapCmd, data_dir: &Path) -> anyhow::Result<()> {
    match sub {
        BootstrapCmd::List { json } => cmd_list(data_dir, *json),
        BootstrapCmd::Add { addr, alias } => cmd_add(data_dir, addr, alias.as_deref()),
        BootstrapCmd::Rm { addr } => cmd_rm(data_dir, addr),
        BootstrapCmd::Clear => cmd_clear(data_dir),
    }
}

fn cmd_list(data_dir: &Path, json: bool) -> anyhow::Result<()> {
    let peers = load(data_dir)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&peers)?);
    } else if peers.is_empty() {
        println!("no bootstrap peers configured");
    } else {
        println!("bootstrap peers ({}):", peers.len());
        for p in &peers {
            if let Some(alias) = &p.alias {
                println!("  {}  ({})", p.addr, alias);
            } else {
                println!("  {}", p.addr);
            }
        }
    }
    Ok(())
}

fn cmd_add(data_dir: &Path, addr: &str, alias: Option<&str>) -> anyhow::Result<()> {
    if !addr.starts_with('/') {
        anyhow::bail!("address must be a valid multiaddr (start with '/'): {addr}");
    }
    let mut peers = load(data_dir)?;
    if peers.iter().any(|p| &p.addr == addr) {
        eprintln!("address already in bootstrap list: {addr}");
        return Ok(());
    }
    let peer = BootstrapPeer {
        addr: addr.to_string(),
        alias: alias.map(String::from),
    };
    peers.push(peer);
    save(data_dir, &peers)?;
    println!("added: {addr}");
    Ok(())
}

fn cmd_rm(data_dir: &Path, addr: &str) -> anyhow::Result<()> {
    let mut peers = load(data_dir)?;
    let before = peers.len();
    peers.retain(|p| &p.addr != addr);
    if peers.len() == before {
        anyhow::bail!("address not in bootstrap list: {addr}");
    }
    save(data_dir, &peers)?;
    println!("removed: {addr}");
    Ok(())
}

fn cmd_clear(data_dir: &Path) -> anyhow::Result<()> {
    let peers = load(data_dir)?;
    if peers.is_empty() {
        println!("bootstrap list already empty");
        return Ok(());
    }
    println!("clearing {} bootstrap peer(s)", peers.len());
    save(data_dir, &[])?;
    println!("done");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn list_empty() {
        let dir = tempdir().unwrap();
        let peers = load(dir.path()).unwrap();
        assert!(peers.is_empty());
    }

    #[test]
    fn add_and_list() {
        let dir = tempdir().unwrap();
        cmd_add(dir.path(), "/ip4/1.2.3.4/udp/4560/quic", Some("alice")).unwrap();
        let peers = load(dir.path()).unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].addr, "/ip4/1.2.3.4/udp/4560/quic");
        assert_eq!(peers[0].alias.as_deref(), Some("alice"));
    }

    #[test]
    fn add_idempotent() {
        let dir = tempdir().unwrap();
        cmd_add(dir.path(), "/ip4/1.2.3.4/udp/4560/quic", None).unwrap();
        cmd_add(dir.path(), "/ip4/1.2.3.4/udp/4560/quic", None).unwrap();
        let peers = load(dir.path()).unwrap();
        assert_eq!(peers.len(), 1);
    }

    #[test]
    fn add_rejects_bad_multiaddr() {
        let dir = tempdir().unwrap();
        let r = cmd_add(dir.path(), "not-a-multiaddr", None);
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("must be a valid multiaddr"));
    }

    #[test]
    fn rm_and_clear() {
        let dir = tempdir().unwrap();
        cmd_add(dir.path(), "/ip4/1.2.3.4/udp/4560/quic", None).unwrap();
        cmd_add(dir.path(), "/ip4/5.6.7.8/udp/9999/quic", None).unwrap();
        cmd_rm(dir.path(), "/ip4/1.2.3.4/udp/4560/quic").unwrap();
        let peers = load(dir.path()).unwrap();
        assert_eq!(peers.len(), 1);
        cmd_clear(dir.path()).unwrap();
        let peers = load(dir.path()).unwrap();
        assert!(peers.is_empty());
    }

    #[test]
    fn rm_nonexistent_fails() {
        let dir = tempdir().unwrap();
        let r = cmd_rm(dir.path(), "/ip4/1.2.3.4/udp/4560/quic");
        assert!(r.is_err());
    }
}
