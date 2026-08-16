//! `a3net tag <sub>` — manage tag→hash mappings (iroh `tags` parity).
//!
//! Tags are human-meaningful labels (e.g. `latest` or `site-v3`)
//! that resolve to a single `ContentHash`. Persisted to
//! `{data_dir}/tags.json` as a `BTreeMap<String, TagEntry>` so the
//! lookups are deterministic and the file is stable for diffs.
//!
//! iroh 1.0's `iroh tags` family is the reference: it ships
//! `add / list / rm / show` and that's exactly what we expose.
//! A3Net additionally supports `--force` on `add` (iroh allows
//! overwriting without a flag, we want it explicit).

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use a3net_types::ContentHash;

use crate::cli::TagCmd;

/// On-disk record for one tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagEntry {
    pub hash: String,
    /// RFC3339 timestamp (UTC) when the tag was last set.
    pub updated_at: String,
}

/// Whole-store snapshot (the file format).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TagStore {
    /// BTreeMap so the on-disk JSON is deterministic.
    #[serde(default)]
    pub tags: BTreeMap<String, TagEntry>,
}

impl TagStore {
    /// Canonical file path inside the data-dir.
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("tags.json")
    }

    /// Load the store; an empty store is returned when the file
    /// does not exist (first run).
    pub fn load(data_dir: &Path) -> Result<Self> {
        let p = Self::path(data_dir);
        if !p.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(&p)
            .with_context(|| format!("read {}", p.display()))?;
        let store: TagStore = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse {}", p.display()))?;
        Ok(store)
    }

    /// Persist the store atomically: write to a sibling
    /// `tags.json.tmp` then `rename` into place. This avoids
    /// partially-written files if the daemon is killed mid-write.
    pub fn save(&self, data_dir: &Path) -> Result<()> {
        let p = Self::path(data_dir);
        let tmp = p.with_extension("json.tmp");
        let mut f = fs::File::create(&tmp)
            .with_context(|| format!("create {}", tmp.display()))?;
        let bytes = serde_json::to_vec_pretty(self)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        // Best-effort: on Windows rename-over-existing needs
        // `fs::rename` which is atomic on POSIX.
        fs::rename(&tmp, &p)?;
        Ok(())
    }
}

/// Top-level dispatch — `a3net tag <sub>`.
pub fn run_tag(sub: &TagCmd, data_dir: &Path) -> Result<()> {
    match sub {
        TagCmd::Add { name, hash, force, json } => {
            let normalized = normalize_hash(hash)?;
            let mut store = TagStore::load(data_dir)?;
            if store.tags.contains_key(name) && !force {
                bail!(
                    "tag `{}` already exists — pass --force to overwrite",
                    name
                );
            }
            store.tags.insert(
                name.clone(),
                TagEntry {
                    hash: normalized.clone(),
                    updated_at: now_rfc3339(),
                },
            );
            store.save(data_dir)?;
            if *json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "name": name,
                        "hash": normalized,
                        "updated_at": store.tags[name].updated_at,
                    }))?
                );
            } else {
                println!(
                    "tagged `{}` → {} ({}→)",
                    name,
                    short_hash(&normalized),
                    normalized
                );
            }
            Ok(())
        }
        TagCmd::Ls { json } => {
            let store = TagStore::load(data_dir)?;
            if *json {
                let rows: Vec<serde_json::Value> = store
                    .tags
                    .iter()
                    .map(|(name, entry)| {
                        serde_json::json!({
                            "name": name,
                            "hash": entry.hash,
                            "updated_at": entry.updated_at,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else if store.tags.is_empty() {
                println!("(no tags)");
            } else {
                println!("{:<32}  {}", "NAME", "HASH (short → full)");
                for (name, entry) in &store.tags {
                    println!(
                        "{:<32}  {} → {}",
                        name,
                        short_hash(&entry.hash),
                        entry.hash
                    );
                }
            }
            Ok(())
        }
        TagCmd::Show { name, json } => {
            let store = TagStore::load(data_dir)?;
            let entry = store
                .tags
                .get(name)
                .ok_or_else(|| anyhow::anyhow!("unknown tag: {}", name))?;
            if *json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "name": name,
                        "hash": entry.hash,
                        "updated_at": entry.updated_at,
                    }))?
                );
            } else {
                println!(
                    "{} → {} (set {})",
                    name,
                    entry.hash,
                    entry.updated_at
                );
            }
            Ok(())
        }
        TagCmd::Rm { name, yes, json } => {
            let mut store = TagStore::load(data_dir)?;
            if !store.tags.contains_key(name) {
                bail!("unknown tag: {}", name);
            }
            if !yes {
                let hash = &store.tags[name].hash;
                eprint!("remove tag `{}` → {}? [y/N] ", name, short_hash(hash));
                io::stderr().flush()?;
                let mut line = String::new();
                io::stdin().read_line(&mut line)?;
                if !line.trim().eq_ignore_ascii_case("y") {
                    println!("aborted");
                    return Ok(());
                }
            }
            store.tags.remove(name);
            store.save(data_dir)?;
            if *json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "ok": true,
                        "removed": name,
                    }))?
                );
            } else {
                println!("removed tag `{}`", name);
            }
            Ok(())
        }
    }
}

/// Strip an optional `b3:` prefix and validate the hash parses
/// as a 32-byte content hash. Returns the canonical 64-char hex.
fn normalize_hash(raw: &str) -> Result<String> {
    let s = raw.trim().trim_start_matches("b3:");
    let hash = ContentHash::from_hex(s)
        .with_context(|| format!("not a valid content hash: {}", raw))?;
    let hex = hash.as_hex();
    Ok(hex.to_string())
}

/// First 8 chars of the hex hash; enough to disambiguate.
fn short_hash(hex: &str) -> String {
    hex.chars().take(8).collect()
}

fn now_rfc3339() -> String {
    // chrono is already a workspace dep — reuse it instead of
    // pulling in time / time-format. UTC + RFC3339 mirrors what
    // iroh uses.
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn add_cmd(name: &str, hash: &str) -> TagCmd {
        TagCmd::Add {
            name: name.to_string(),
            hash: hash.to_string(),
            force: false,
            json: true,
        }
    }

    fn rm_cmd(name: &str) -> TagCmd {
        TagCmd::Rm {
            name: name.to_string(),
            yes: true,
            json: true,
        }
    }

    #[test]
    fn normalize_hash_accepts_b3_prefix() {
        let raw = "b3:0000000000000000000000000000000000000000000000000000000000000000";
        let normalized = normalize_hash(raw).unwrap();
        assert_eq!(normalized.len(), 64);
        assert!(!normalized.starts_with("b3:"));
    }

    #[test]
    fn normalize_hash_rejects_garbage() {
        assert!(normalize_hash("not-a-hash").is_err());
        assert!(normalize_hash("b3:zz").is_err());
    }

    #[test]
    fn add_then_show_round_trip() {
        let dir = tempdir().unwrap();
        let h = "0000000000000000000000000000000000000000000000000000000000000000";
        run_tag(&add_cmd("latest", h), dir.path()).unwrap();
        let show = TagCmd::Show {
            name: "latest".into(),
            json: true,
        };
        run_tag(&show, dir.path()).unwrap();
    }

    #[test]
    fn add_without_force_rejects_duplicate() {
        let dir = tempdir().unwrap();
        let h = "0000000000000000000000000000000000000000000000000000000000000000";
        run_tag(&add_cmd("dup", h), dir.path()).unwrap();
        assert!(run_tag(&add_cmd("dup", h), dir.path()).is_err());
    }

    #[test]
    fn rm_removes_tag() {
        let dir = tempdir().unwrap();
        let h = "0000000000000000000000000000000000000000000000000000000000000000";
        run_tag(&add_cmd("gone", h), dir.path()).unwrap();
        run_tag(&rm_cmd("gone"), dir.path()).unwrap();
        let show = TagCmd::Show {
            name: "gone".into(),
            json: true,
        };
        assert!(run_tag(&show, dir.path()).is_err());
    }

    #[test]
    fn save_is_atomic_when_target_dir_exists() {
        let dir = tempdir().unwrap();
        let h = "0000000000000000000000000000000000000000000000000000000000000000";
        run_tag(&add_cmd("atomic", h), dir.path()).unwrap();
        // tmp file should not linger after save
        let tmp = TagStore::path(dir.path()).with_extension("json.tmp");
        assert!(!tmp.exists());
    }
}
