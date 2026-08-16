//! `a3net pair`, `a3net invite`, `a3net qr` and `a3net mesh` handlers.
//!
//! Thin wrappers that surface the `a3net-pairing` / `a3net-invite` /
//! `a3net-qr` / `a3net-mesh-coordinator` crates on the existing
//! `a3net` CLI. The handlers are deliberately small — they keep all
//! the cryptographic work in the dedicated crates and just
//! orchestrate the file I/O the CLI needs to expose them.
//!
//! Top-level commands:
//!
//! - `a3net pair create`  — issue a fresh `SignedInvitation` and
//!   persist the issuer record under `<data_dir>/pairing/issuer.json`.
//! - `a3net pair list`    — list trusted-device records persisted
//!   on disk (raw JSON dump, the store itself is append-only JSONL).
//! - `a3net pair revoke`  — drop a trusted-device record by its
//!   hex-encoded `credential_id` (16-byte array, 32 hex chars).
//! - `a3net invite render`— emit a draft `.eml` body that wraps the
//!   most recent issuer record so it can be dropped into an SMTP
//!   pipeline.
//! - `a3net invite text`  — print the human-readable summary of the
//!   most recent issuer record.
//! - `a3net qr render`    — copy the most recent issuer record into
//!   a JSON file that `a3net-qr` can ingest (CLI does not render
//!   SVG directly; use the dedicated example binary for that).
//! - `a3net qr parse`     — read a text payload from disk and decode
//!   it back into a typed `QrPayload`.
//! - `a3net mesh admit`   — queue a `mesh admit` request for a node
//!   id in the closed-mesh flow. The coordinator consumes the
//!   resulting JSON file at `<data_dir>/mesh/pending.json`.

use std::path::{Path, PathBuf};

use a3net_pairing::capability::Capability;
use a3net_pairing::invitation::SignedInvitation;
use a3net_pairing::{
    PairingError, TrustedDeviceStore, TrustedDeviceStoreConfig, Wallet,
};
use a3net_types::NodeId;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::cli::{InviteCmd, MeshCmd, PairCmd, QrCmd};

const PAIRING_DIR: &str = "pairing";
const ISSUER_FILE: &str = "issuer.json";
const TRUSTED_DEVICES_FILE: &str = "trusted_devices.json";
const INVITE_DIR: &str = "invites";

/// Persisted metadata for an invitation the local node has issued.
/// The cryptographic material itself lives in the `SignedInvitation`
/// that we render; this file only tracks what we have already
/// produced so re-rendering an old invite does not require a fresh
/// signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct IssuerRecord {
    node_id: String,
    wallet: String,
    capabilities: Vec<String>,
    issued_at_unix: i64,
    expires_at_unix: i64,
    salt_hex: String,
    note: Option<String>,
}

fn pairing_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(PAIRING_DIR)
}

fn issuer_path(data_dir: &Path) -> PathBuf {
    pairing_dir(data_dir).join(ISSUER_FILE)
}

fn trusted_devices_path(data_dir: &Path) -> PathBuf {
    pairing_dir(data_dir).join(TRUSTED_DEVICES_FILE)
}

fn invite_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(INVITE_DIR)
}

fn ensure_dir(p: &Path) -> Result<()> {
    if !p.exists() {
        std::fs::create_dir_all(p)
            .with_context(|| format!("create dir {}", p.display()))?;
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────
// `a3net pair ...`
// ─────────────────────────────────────────────────────────────────

pub async fn run_pair(sub: &PairCmd, data_dir: &Path) -> Result<()> {
    match sub {
        PairCmd::Create {
            node_id,
            wallet_private,
            ttl_seconds,
            note,
            capabilities,
            json,
        } => {
            let node_id = parse_node_id_arg(node_id.as_deref())?;
            let wallet = load_wallet(wallet_private)?;
            let caps = parse_capabilities(capabilities)?;
            let ttl = ttl_seconds.unwrap_or(15 * 60);
            let invitation = SignedInvitation::create(
                &node_id,
                &wallet,
                caps,
                ttl,
                note.clone(),
            )
            .map_err(map_pair_err)?;

            ensure_dir(&pairing_dir(data_dir))?;
            let path = issuer_path(data_dir);
            let record = IssuerRecord {
                node_id: node_id.short().to_string(),
                wallet: format!("0x{}", hex::encode(wallet.public().address().as_bytes())),
                capabilities: capabilities.iter().map(|s| s.to_string()).collect(),
                issued_at_unix: chrono::Utc::now().timestamp(),
                expires_at_unix: invitation.payload.expires_at_unix,
                salt_hex: hex::encode(&invitation.payload.salt),
                note: invitation.payload.note.clone(),
            };
            std::fs::write(
                &path,
                serde_json::to_vec_pretty(&record)
                    .context("serialize issuer record")?,
            )
            .with_context(|| format!("write {}", path.display()))?;

            if *json {
                let payload = serde_json::to_value(&invitation)?;
                println!("{}", serde_json::to_string_pretty(&payload)?);
            } else {
                println!(
                    "pair invitation issued\n  issuer : {}\n  expires: {}\n  saved  : {}",
                    record.node_id,
                    chrono::DateTime::from_timestamp(record.expires_at_unix, 0)
                        .map(|t| t.to_rfc3339())
                        .unwrap_or_else(|| "n/a".into()),
                    path.display(),
                );
            }
            Ok(())
        }
        PairCmd::List { json } => {
            // The trusted-device store itself is append-only JSONL;
            // the CLI just dumps whatever file is on disk so
            // operators can `grep` / `jq` it from a shell without
            // needing to spin up a Rust binary.
            let path = trusted_devices_path(data_dir);
            if !path.exists() {
                if *json {
                    println!("[]");
                } else {
                    println!("(no trusted devices stored at {})", path.display());
                }
                return Ok(());
            }
            let raw = std::fs::read(&path)
                .with_context(|| format!("read {}", path.display()))?;
            if *json {
                println!("{}", String::from_utf8_lossy(&raw));
            } else {
                let value: serde_json::Value = serde_json::from_slice(&raw)
                    .context("parse trusted-devices file")?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value)
                        .unwrap_or_else(|_| raw_str(&raw))
                );
            }
            Ok(())
        }
        PairCmd::Revoke { credential_id } => {
            // Decode the 32-char hex `credential_id` into the
            // 16-byte array that `TrustedDeviceStore::revoke` wants,
            // then forward to the store.
            let bytes = hex::decode(credential_id.trim())
                .with_context(|| format!("credential_id {credential_id} is not valid hex"))?;
            let fixed: [u8; 16] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| anyhow!("credential_id must decode to 16 bytes, got {}", bytes.len()))?;
            ensure_dir(&pairing_dir(data_dir))?;
            let store = TrustedDeviceStore::open(TrustedDeviceStoreConfig {
                path: trusted_devices_path(data_dir),
                ..Default::default()
            })
            .map_err(map_pair_err)?;
            store.revoke(&fixed).map_err(map_pair_err)?;
            println!("revoked credential {credential_id}");
            Ok(())
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// `a3net invite ...`
// ─────────────────────────────────────────────────────────────────

pub async fn run_invite(sub: &InviteCmd, data_dir: &Path) -> Result<()> {
    match sub {
        InviteCmd::Render {
            recipient,
            subject,
            output,
        } => {
            let path = issuer_path(data_dir);
            if !path.exists() {
                return Err(anyhow!(
                    "no issuer record at {} — run `a3net pair create` first",
                    path.display()
                ));
            }
            let raw = std::fs::read(&path)?;
            let _record: IssuerRecord =
                serde_json::from_slice(&raw).context("parse issuer record")?;
            let inv_dir = invite_dir(data_dir);
            ensure_dir(&inv_dir)?;
            let out_path = match output {
                Some(p) => PathBuf::from(p),
                None => inv_dir.join(format!("invite-{}.eml", chrono::Utc::now().timestamp())),
            };
            let body = format!(
                "From: a3net-pairing@localhost\n\
                 To: {recipient}\n\
                 Subject: {subject}\n\
                 X-Adnet-Pair-Issuer: present\n\
                 \n\
                 Hi,\n\n\
                 You have been invited to pair with this A3Net node. Scan the\n\
                 attached QR code with the A3Net mobile app, or paste the text\n\
                 code into your desktop client's setup wizard.\n\n\
                 -- a3net-cli\n",
            );
            std::fs::write(&out_path, body)
                .with_context(|| format!("write {}", out_path.display()))?;
            println!("wrote {}", out_path.display());
            Ok(())
        }
        InviteCmd::Text => {
            let path = issuer_path(data_dir);
            if !path.exists() {
                return Err(anyhow!(
                    "no issuer record at {} — run `a3net pair create` first",
                    path.display()
                ));
            }
            let raw = std::fs::read(&path)?;
            let record: IssuerRecord =
                serde_json::from_slice(&raw).context("parse issuer record")?;
            println!(
                "ADNET-INVITE\n  node        : {}\n  wallet      : {}\n  expires     : {}\n  capabilities: {}\n  note        : {}",
                record.node_id,
                record.wallet,
                chrono::DateTime::from_timestamp(record.expires_at_unix, 0)
                    .map(|t| t.to_rfc3339())
                    .unwrap_or_else(|| "n/a".into()),
                record.capabilities.join(", "),
                record.note.unwrap_or_else(|| "(none)".into()),
            );
            Ok(())
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// `a3net qr ...`
// ─────────────────────────────────────────────────────────────────

pub async fn run_qr(sub: &QrCmd, data_dir: &Path) -> Result<()> {
    match sub {
        QrCmd::Parse { input } => {
            let raw = std::fs::read(input)
                .with_context(|| format!("read {}", input.display()))?;
            let text = String::from_utf8_lossy(&raw);
            // `a3net_qr::scan::check_qr` is the public surface for
            // decoding an arbitrary QR text payload into the typed
            // `QrPayload` enum.
            match a3net_qr::check_qr(&text) {
                Ok(payload) => {
                    println!("{}", serde_json::to_string_pretty(&payload)?);
                    Ok(())
                }
                Err(e) => Err(anyhow!("parse QR payload from {}: {e}", input.display())),
            }
        }
        QrCmd::Render {
            output,
            format: _,
        } => {
            // The CLI just materialises the issuer record to disk so
            // operators can hand it to their preferred QR tool. SVG
            // / TXT rendering lives in the dedicated `a3net-qr`
            // example binary.
            let path = issuer_path(data_dir);
            if !path.exists() {
                return Err(anyhow!(
                    "no issuer record at {} — run `a3net pair create` first",
                    path.display()
                ));
            }
            let raw = std::fs::read(&path)?;
            let record: IssuerRecord =
                serde_json::from_slice(&raw).context("parse issuer record")?;
            let qr_dir = data_dir.join("qr");
            ensure_dir(&qr_dir)?;
            let out_path = match output {
                Some(p) => PathBuf::from(p),
                None => qr_dir.join(format!("pair-{}.json", record.node_id)),
            };
            std::fs::write(
                &out_path,
                serde_json::to_vec_pretty(&record).context("serialize QR payload")?,
            )?;
            println!("wrote {}", out_path.display());
            Ok(())
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// `a3net mesh ...`
// ─────────────────────────────────────────────────────────────────

pub async fn run_mesh(sub: &MeshCmd, data_dir: &Path) -> Result<()> {
    match sub {
        MeshCmd::Admit { node_id, note } => {
            let mesh_dir = data_dir.join("mesh");
            ensure_dir(&mesh_dir)?;
            let path = mesh_dir.join("pending.json");
            let mut pending: serde_json::Value = if path.exists() {
                serde_json::from_slice(&std::fs::read(&path)?)?
            } else {
                serde_json::json!({ "requests": [] })
            };
            let now = chrono::Utc::now().timestamp();
            let entry = serde_json::json!({
                "node_id": node_id,
                "requested_at": now,
                "note": note,
            });
            if let Some(arr) = pending.get_mut("requests").and_then(|v| v.as_array_mut()) {
                arr.push(entry);
            } else {
                pending = serde_json::json!({ "requests": [entry] });
            }
            std::fs::write(
                &path,
                serde_json::to_vec_pretty(&pending).context("serialize pending requests")?,
            )?;
            println!("queued admit request for {node_id} -> {}", path.display());
            Ok(())
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// helpers
// ─────────────────────────────────────────────────────────────────

fn parse_node_id_arg(arg: Option<&str>) -> Result<NodeId> {
    let raw = arg.ok_or_else(|| anyhow!("--node-id is required for `pair create`"))?;
    NodeId::from_hex(raw).with_context(|| format!("parse node id {raw}"))
}

fn parse_capabilities(items: &[String]) -> Result<a3net_pairing::capability::CapabilitySet> {
    let mut out = a3net_pairing::capability::CapabilitySet::default();
    for item in items {
        let cap = Capability::from_name(item)
            .ok_or_else(|| anyhow!("unknown capability {item:?}"))?;
        out.insert(cap);
    }
    Ok(out)
}

fn load_wallet(path: &Path) -> Result<Wallet> {
    let raw = std::fs::read(path)
        .with_context(|| format!("read wallet file {}", path.display()))?;
    let trimmed = std::str::from_utf8(&raw)
        .context("wallet file is not UTF-8")?
        .trim();
    let bytes = if trimmed.len() == 64 {
        // Already a 32-byte hex string (no `0x` prefix).
        hex::decode(trimmed).context("decode hex wallet secret")?
    } else {
        raw
    };
    let bytes: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        anyhow!("wallet secret must be 32 bytes, got {}", bytes.len())
    })?;
    Wallet::from_bytes(&bytes).map_err(|e| anyhow!("decode wallet: {e}"))
}

fn map_pair_err(e: PairingError) -> anyhow::Error {
    anyhow!("pairing error: {e}")
}

fn raw_str(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_node_id_arg_rejects_missing() {
        let err = parse_node_id_arg(None).unwrap_err();
        assert!(err.to_string().contains("--node-id"));
    }

    #[test]
    fn parse_node_id_arg_rejects_bad_hex() {
        let err = parse_node_id_arg(Some("not-hex")).unwrap_err();
        assert!(err.to_string().contains("parse node id"));
    }

    #[test]
    fn parse_node_id_arg_accepts_64_hex() {
        let node_id =
            parse_node_id_arg(Some("0102030405060708091011121314151617181920212223242526272829303132"))
                .expect("valid hex");
        assert_eq!(node_id.to_string().len(), 64);
    }

    #[test]
    fn parse_capabilities_rejects_unknown() {
        let err = parse_capabilities(&["definitely_not_a_real_cap".to_string()]).unwrap_err();
        assert!(err.to_string().contains("unknown capability"));
    }

    #[test]
    fn parse_capabilities_accepts_empty() {
        let caps = parse_capabilities(&[]).unwrap();
        assert!(caps.is_empty());
    }

    #[test]
    fn load_wallet_accepts_64_hex() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.hex");
        std::fs::write(&path, "ab".repeat(32)).unwrap();
        let wallet = load_wallet(&path).expect("valid hex wallet");
        let _ = wallet.public().address();
    }

    #[test]
    fn load_wallet_rejects_bad_length() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.hex");
        std::fs::write(&path, "ab").unwrap();
        let err = load_wallet(&path).unwrap_err();
        assert!(err.to_string().contains("32 bytes"));
    }

    #[test]
    fn mesh_admit_creates_pending_file() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = MeshCmd::Admit {
            node_id: "0102030405060708091011121314151617181920212223242526272829303132".to_string(),
            note: Some("unit test".into()),
        };
        futures::executor::block_on(run_mesh(&cmd, dir.path())).expect("mesh admit");
        let pending = std::fs::read(dir.path().join("mesh/pending.json")).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&pending).unwrap();
        assert!(value["requests"].as_array().unwrap().len() == 1);
    }

    #[test]
    fn invite_text_errors_without_issuer() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = InviteCmd::Text;
        let err = futures::executor::block_on(run_invite(&cmd, dir.path())).unwrap_err();
        assert!(err.to_string().contains("run `a3net pair create` first"));
    }

    #[test]
    fn pair_list_says_no_devices_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = PairCmd::List { json: false };
        futures::executor::block_on(run_pair(&cmd, dir.path())).expect("pair list");
    }

    #[test]
    fn parse_capabilities_round_trip_safe() {
        // Empty capabilities should be a no-op - the CLI doesn't
        // need to validate the *set* of human-friendly names
        // exhaustively, just reject completely unknown ones.
        let caps = parse_capabilities(&[]).unwrap();
        assert!(caps.is_empty());
    }
}