//! Offline diagnostics snapshot — describes a node's persistent
//! identity without starting the runtime.
//!
//! The `a3net diagnostics` CLI subcommand surfaces this to
//! operators without spinning up the iroh endpoint, the mesh
//! server, or the gossip overlay. Useful for:
//!
//! - **Support tickets** — "what's your NodeId?"
//! - **Health probes** — confirm the data dir is readable and
//!   the identity file is intact.
//! - **Pre-flight checks** — verify the operator's data_dir
//!   env var points at a real A3Net root before starting the
//!   service.
//!
//! The output is intentionally a tiny struct (not a `Node`):
//! pulling the full `Node` would force us to spin up the
//! tokio runtime and bind sockets, which is not what
//! `diagnostics` is for.

use std::path::Path;

use a3net_types::NodeId;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One-shot snapshot of the persistent identity rooted at
/// `data_dir`. Cheap to construct: it reads the 32-byte
/// identity blob from `<data_dir>/identity.key` and derives
/// the NodeId, but does **not** open the iroh `Endpoint` or
/// any UDP socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticsSnapshot {
    /// Absolute path of the data directory this snapshot
    /// describes.
    pub data_dir: String,
    /// Full 32-byte NodeId (hex-encoded, no `a3net-` prefix).
    pub node_id: String,
    /// Short fingerprint (first 8 bytes hex-encoded). Matches
    /// the format `a3net-<short>` that other A3Net UIs use.
    pub node_id_short: String,
    /// Hex-encoded Ed25519 public key derived from the same
    /// secret. Mirrors `a3net_types::NodeId::public_key_hex()`
    /// for cases where the caller wants the raw key.
    pub public_key: String,
    /// URL the mesh HTTP server was last configured to bind
    /// on. `None` if no config file was found (the default
    /// `127.0.0.1:3300` is reported by the JSON view, but the
    /// human-readable path distinguishes "unset" from "set").
    pub mesh_url: Option<String>,
}

/// Build a snapshot rooted at `data_dir`. Returns
/// `Err(anyhow::Error)` if the identity file is missing or
/// not a valid 32-byte blob.
pub fn diagnostics_snapshot(data_dir: &Path) -> Result<DiagnosticsSnapshot> {
    let node_id = load_node_id(data_dir)
        .with_context(|| format!("load NodeId from {}", data_dir.display()))?;
    Ok(DiagnosticsSnapshot {
        data_dir: data_dir.display().to_string(),
        node_id: node_id.as_hex().to_string(),
        node_id_short: node_id.short().to_string(),
        public_key: node_id.as_hex().to_string(),
        mesh_url: read_mesh_url(data_dir),
    })
}

/// Top-level dispatcher for `a3net diagnostics [--json]`. Offline — does not
/// require a running node.
pub fn run_diagnostics(data_dir: &Path, json: bool) -> Result<()> {
    let snap = diagnostics_snapshot(data_dir)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&snap)?);
    } else {
        println!("A3Net Diagnostics");
        println!("{}", "=".repeat(50));
        println!("  data_dir    : {}", snap.data_dir);
        println!("  node_id     : {}", snap.node_id);
        println!("  short_id    : a3net-{}", snap.node_id_short);
        println!("  public_key  : {}", snap.public_key);
        if let Some(url) = &snap.mesh_url {
            println!("  mesh_url    : {}", url);
        } else {
            println!("  mesh_url    : (not configured)");
        }
    }
    Ok(())
}

/// Read the 32-byte Ed25519 identity blob from
/// `<data_dir>/identity.key` and parse it as a `NodeId`.
///
/// The on-disk format matches what `a3net-identity` writes:
/// raw 32 bytes (no length prefix, no hex). Older formats
/// used a 64-byte hex-encoded public key; we accept that
/// too for backwards compatibility with operator backups.
///
/// Also supports `<data_dir>/node_id` file containing hex string.
fn load_node_id(data_dir: &Path) -> Result<NodeId> {
    // Try identity.key first
    let identity_path = data_dir.join("identity.key");
    if identity_path.exists() {
        let bytes = std::fs::read(&identity_path)
            .with_context(|| format!("read identity file at {}", identity_path.display()))?;

        // Legacy format: 64 ASCII hex chars (optionally
        // surrounded by whitespace / newlines from operators'
        // hand-edits). Detect by checking that the contents are
        // valid hex *and* decode to 32 bytes.
        if let Ok(s) = std::str::from_utf8(&bytes) {
            let trimmed: String = s.chars().filter(|c| !c.is_whitespace()).collect();
            if trimmed.len() == 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
                let raw = hex_decode(&trimmed).with_context(|| "decoding legacy hex identity")?;
                return NodeId::from_bytes(&raw)
                    .with_context(|| "legacy 32-byte identity blob is not a valid NodeId");
            }
        }

        if bytes.len() == 32 {
            return NodeId::from_bytes(&bytes)
                .with_context(|| "32-byte identity blob is not a valid NodeId");
        }
    }

    // Fallback to node_id file (hex string without file extension)
    let node_id_path = data_dir.join("node_id");
    if node_id_path.exists() {
        let content = std::fs::read_to_string(&node_id_path)
            .with_context(|| format!("read node_id file at {}", node_id_path.display()))?;
        let hex = content.trim();
        if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return NodeId::from_hex(hex)
                .context("hex node_id is not a valid NodeId");
        }
    }

    anyhow::bail!(
        "no valid identity found: expected 32-byte identity.key, 64-char hex identity.key, or 64-char hex node_id"
    )
}

/// Best-effort read of the mesh HTTP bind URL from the JSON
/// config file. `None` when the config is missing or the
/// field is unset.
fn read_mesh_url(data_dir: &Path) -> Option<String> {
    let candidates = [data_dir.join("config.json"), data_dir.join("config.json5")];
    for path in &candidates {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        // We deliberately do not pull in a full JSON parser
        // here — the config file is small and a single regex
        // pass is enough for a support-tool diagnostic.
        let needle = "\"meshHttpBind\":";
        let start = text.find(needle)? + needle.len();
        let rest = &text[start..];
        // Skip leading whitespace + the opening quote.
        let rest = rest.trim_start();
        let rest = rest.strip_prefix('"')?;
        let end = rest.find('"')?;
        return Some(rest[..end].to_string());
    }
    None
}

/// Minimal hex decoder for the legacy 64-byte identity
/// files. We avoid pulling in a `hex` crate dep just for this
/// helper.
fn hex_decode(s: &str) -> Result<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        anyhow::bail!("hex string has odd length");
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for pair in bytes.chunks(2) {
        let hi = hex_nibble(pair[0])?;
        let lo = hex_nibble(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Result<u8> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => anyhow::bail!("non-hex character: {b:#04x}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_decode_round_trip() {
        let raw = vec![0u8, 1, 2, 0xfe, 0xff];
        let hex = raw.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let back = hex_decode(&hex).expect("valid hex");
        assert_eq!(back, raw);
    }

    #[test]
    fn hex_decode_rejects_odd_length() {
        assert!(hex_decode("abc").is_err());
    }

    #[test]
    fn hex_decode_rejects_invalid_char() {
        assert!(hex_decode("zz").is_err());
    }

    #[test]
    fn hex_nibble_accepts_uppercase() {
        assert_eq!(hex_nibble(b'A').unwrap(), 10);
        assert_eq!(hex_nibble(b'F').unwrap(), 15);
    }

    #[test]
    fn diagnostics_snapshot_missing_dir_errors() {
        let dir = std::env::temp_dir().join("a3net-nonexistent-zzz");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(diagnostics_snapshot(&dir).is_err());
    }

    #[test]
    fn diagnostics_snapshot_with_valid_identity() {
        // Use a tmp dir; write a 32-byte raw identity; read
        // it back via the snapshot.
        let dir = std::env::temp_dir().join(format!(
            "a3net-diag-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // 32 distinct bytes so it's a valid NodeId.
        let raw: Vec<u8> = (0u8..32).collect();
        std::fs::write(dir.join("identity.key"), &raw).unwrap();
        let snap = diagnostics_snapshot(&dir).expect("snapshot");
        assert_eq!(snap.node_id.len(), 64);
        assert_eq!(snap.node_id_short.len(), 12);
        assert_eq!(snap.public_key, snap.node_id);
        assert!(snap.mesh_url.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diagnostics_snapshot_with_legacy_hex_identity() {
        let dir = std::env::temp_dir().join(format!(
            "a3net-diag-hex-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Legacy format: 64 bytes of hex-encoded raw key +
        // a trailing newline (some operators edit the file
        // manually and add one).
        let raw: Vec<u8> = (0u8..32).collect();
        let hex = raw.iter().map(|b| format!("{b:02x}")).collect::<String>();
        std::fs::write(dir.join("identity.key"), format!("{hex}\n")).unwrap();
        let snap = diagnostics_snapshot(&dir).expect("snapshot");
        assert_eq!(snap.node_id.len(), 64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diagnostics_snapshot_rejects_bad_length() {
        let dir = std::env::temp_dir().join(format!(
            "a3net-diag-bad-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("identity.key"), vec![0u8; 16]).unwrap();
        let err = diagnostics_snapshot(&dir).unwrap_err();
        // Walk the chain — `with_context` wraps the
        // underlying error, so the original message lives on
        // a deeper frame.
        let msg = err.chain().fold(String::new(), |mut acc, e| {
            if !acc.is_empty() {
                acc.push_str(" -> ");
            }
            acc.push_str(&e.to_string());
            acc
        });
        assert!(
            msg.contains("unexpected length")
                || msg.contains("expected 32 bytes")
                || msg.contains("got 16")
                || msg.contains("no valid identity found"),
            "got: {msg}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ────────────────────────────────────────────────────────────
    // Tests for read_mesh_url (previously untested)
    // ────────────────────────────────────────────────────────────

    #[test]
    fn read_mesh_url_finds_config_json() {
        let dir = std::env::temp_dir().join(format!(
            "a3net-diag-mesh-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let content = serde_json::json!({
            "meshHttpBind": "127.0.0.1:8080",
            "dataDir": "/test"
        })
        .to_string();
        std::fs::write(dir.join("config.json"), content).unwrap();

        let url = super::read_mesh_url(&dir);
        assert_eq!(url, Some("127.0.0.1:8080".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_mesh_url_finds_config_json5() {
        let dir = std::env::temp_dir().join(format!(
            "a3net-diag-mesh2-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // JSON5 format with comments
        let content = r#"{
            // comment
            "meshHttpBind": "0.0.0.0:9000"
        }"#;
        std::fs::write(dir.join("config.json5"), content).unwrap();

        let url = super::read_mesh_url(&dir);
        assert_eq!(url, Some("0.0.0.0:9000".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_mesh_url_returns_none_when_no_config() {
        let dir = std::env::temp_dir().join(format!(
            "a3net-diag-noconfig-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // No config file
        let url = super::read_mesh_url(&dir);
        assert_eq!(url, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_mesh_url_returns_none_when_field_missing() {
        let dir = std::env::temp_dir().join(format!(
            "a3net-diag-nomesh-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let content = serde_json::json!({
            "dataDir": "/test",
            "log": { "level": "info" }
        })
        .to_string();
        std::fs::write(dir.join("config.json"), content).unwrap();

        let url = super::read_mesh_url(&dir);
        assert_eq!(url, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_mesh_url_returns_none_on_empty_file() {
        let dir = std::env::temp_dir().join(format!(
            "a3net-diag-empty-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), "").unwrap();

        let url = super::read_mesh_url(&dir);
        assert_eq!(url, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_mesh_url_returns_none_on_malformed_json() {
        let dir = std::env::temp_dir().join(format!(
            "a3net-diag-malformed-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), "{ invalid json }").unwrap();

        let url = super::read_mesh_url(&dir);
        assert_eq!(url, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_mesh_url_prefers_json_over_json5() {
        let dir = std::env::temp_dir().join(format!(
            "a3net-diag-pref-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // config.json has one value
        std::fs::write(
            dir.join("config.json"),
            serde_json::json!({ "meshHttpBind": "127.0.0.1:1111" }).to_string(),
        )
        .unwrap();
        // config.json5 has a different value
        std::fs::write(
            dir.join("config.json5"),
            serde_json::json!({ "meshHttpBind": "127.0.0.1:2222" }).to_string(),
        )
        .unwrap();

        let url = super::read_mesh_url(&dir);
        // Should find config.json first
        assert_eq!(url, Some("127.0.0.1:1111".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_mesh_url_handles_url_with_special_chars() {
        let dir = std::env::temp_dir().join(format!(
            "a3net-diag-url-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let content = serde_json::json!({
            "meshHttpBind": "https://example.com:8080/path"
        })
        .to_string();
        std::fs::write(dir.join("config.json"), content).unwrap();

        let url = super::read_mesh_url(&dir);
        assert_eq!(url, Some("https://example.com:8080/path".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_mesh_url_handles_whitespace_in_value() {
        let dir = std::env::temp_dir().join(format!(
            "a3net-diag-ws-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let content = r#"{
            "meshHttpBind": "127.0.0.1:8080"
        }"#;
        std::fs::write(dir.join("config.json"), content).unwrap();

        let url = super::read_mesh_url(&dir);
        assert_eq!(url, Some("127.0.0.1:8080".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diagnostics_snapshot_serialization_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "a3net-diag-serial-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let raw: Vec<u8> = (0u8..32).collect();
        std::fs::write(dir.join("identity.key"), &raw).unwrap();

        let snap = diagnostics_snapshot(&dir).unwrap();
        let json = serde_json::to_string(&snap).unwrap();
        let parsed: DiagnosticsSnapshot = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.node_id, snap.node_id);
        assert_eq!(parsed.node_id_short, snap.node_id_short);
        assert_eq!(parsed.public_key, snap.public_key);
        assert_eq!(parsed.data_dir, snap.data_dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diagnostics_snapshot_debug_format() {
        let dir = std::env::temp_dir().join(format!(
            "a3net-diag-debug-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let raw: Vec<u8> = (0u8..32).collect();
        std::fs::write(dir.join("identity.key"), &raw).unwrap();

        let snap = diagnostics_snapshot(&dir).unwrap();
        let debug = format!("{:?}", snap);
        assert!(debug.contains("node_id"));
        assert!(debug.contains("node_id_short"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hex_decode_empty_string() {
        // Empty string has even length (0) but is valid hex
        let result = hex_decode("");
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn hex_decode_lowercase_and_mixed_case() {
        let hex = "deadbeef";
        let decoded = hex_decode(hex).unwrap();
        assert_eq!(decoded, vec![0xde, 0xad, 0xbe, 0xef]);

        // Mixed case should also work
        let hex = "DeAdBeEf";
        let decoded = hex_decode(hex).unwrap();
        assert_eq!(decoded, vec![0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn load_node_id_32_byte_raw_format() {
        let dir = std::env::temp_dir().join(format!(
            "a3net-load-32b-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // 32 distinct bytes
        let raw: Vec<u8> = (0u8..32).collect();
        std::fs::write(dir.join("identity.key"), &raw).unwrap();

        let node_id = super::load_node_id(&dir).unwrap();
        assert_eq!(node_id.as_hex().len(), 64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_node_id_legacy_hex_format() {
        let dir = std::env::temp_dir().join(format!(
            "a3net-load-hex-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Raw 32 bytes encoded as hex
        let raw: Vec<u8> = (0u8..32).collect();
        let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        // Add some whitespace to test trimming
        std::fs::write(dir.join("identity.key"), format!("  {hex}  \n")).unwrap();

        let node_id = super::load_node_id(&dir).unwrap();
        assert_eq!(node_id.as_hex().len(), 64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_node_id_error_on_bad_file() {
        let dir = std::env::temp_dir().join(format!(
            "a3net-load-bad-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Empty file
        std::fs::write(dir.join("identity.key"), "").unwrap();

        let err = super::load_node_id(&dir).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unexpected length") || msg.contains("expected 32"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_node_id_error_on_wrong_length() {
        let dir = std::env::temp_dir().join(format!(
            "a3net-load-wrong-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // 64 bytes (not valid as either raw or hex)
        std::fs::write(dir.join("identity.key"), vec![0u8; 64]).unwrap();

        let err = super::load_node_id(&dir).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unexpected length")
                || msg.contains("no valid identity found"),
            "got: {msg}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
