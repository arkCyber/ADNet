//! `a3net moderation <sub>` operator-facing commands.
//!
//! Every command here is **offline** — it talks to the on-disk
//! blocklist / pin file / blob store and exits without touching
//! the network. The gateway process picks up the changes on its
//! next request because the [`Blocklist`] is re-read from disk on
//! every construction and we keep an in-memory
//! [`a3net_gateway::GatewayHandler::moderation_policy`] handle
//! for the running process.
//!
//! ## Subcommands
//!
//! | Sub | Purpose |
//! |-----|---------|
//! | `status` | one-line blocklist summary |
//! | `list` | enumerate entries |
//! | `block` | add a hash to the blocklist (no local erase) |
//! | `unblock` | mark a hash's entry revoked |
//! | `revoke` | revoke by entry id |
//! | `erase` | block + remove pin + GC bytes (or crypto-shred) |
//! | `defend-on` / `defend-off` | toggle deny-by-default |
//! | `policy` | print the policy snapshot |

use std::path::{Path, PathBuf};
use std::sync::Arc;

use a3net_moderation::{
    Blocklist, BlocklistSource, ModerationPolicy, TakedownReason, TakedownService,
    TakedownServiceConfig, TakedownTarget,
};
use a3net_reputation::{PeerScoreTable, ReputationParams};
use a3net_types::ContentHash;

use crate::cli::ModerationCmd;

/// Run a moderation subcommand. Returns the appropriate exit code
/// on error. `Err` propagates to the CLI main loop which formats it
/// as a single-line diagnostic.
pub fn run_moderation(sub: &ModerationCmd, data_dir: &Path) -> anyhow::Result<()> {
    let data_dir = data_dir.to_path_buf();
    match sub {
        ModerationCmd::Status => cmd_status(&data_dir),
        ModerationCmd::List { active, json } => cmd_list(&data_dir, *active, *json),
        ModerationCmd::Block {
            cid,
            reason,
            source,
            evidence,
            operator,
            expires,
            publisher,
            json,
        } => cmd_block(
            &data_dir,
            cid,
            reason,
            source,
            evidence,
            operator,
            *expires,
            publisher.as_deref(),
            *json,
        ),
        ModerationCmd::Unblock { cid } => cmd_unblock(&data_dir, cid),
        ModerationCmd::Revoke { id } => cmd_revoke(&data_dir, *id),
        ModerationCmd::Erase {
            cid,
            reason,
            source,
            evidence,
            operator,
            target,
            publisher,
            expires,
            json,
        } => cmd_erase(
            &data_dir,
            cid,
            reason,
            source,
            evidence,
            operator,
            target,
            publisher.as_deref(),
            *expires,
            *json,
        ),
        ModerationCmd::DefendOn => cmd_defend(&data_dir, true),
        ModerationCmd::DefendOff => cmd_defend(&data_dir, false),
        ModerationCmd::Policy { json } => cmd_policy(&data_dir, *json),
    }
}

// ───────────────────────────────────────────────────────────────────────
// shared helpers
// ───────────────────────────────────────────────────────────────────────

fn load_blocklist(data_dir: &Path) -> anyhow::Result<Arc<Blocklist>> {
    let bl = Blocklist::load(data_dir).map_err(|e| {
        anyhow::anyhow!("loading blocklist under {}: {e}", data_dir.display())
    })?;
    Ok(Arc::new(bl))
}

fn parse_reason(s: &str) -> anyhow::Result<TakedownReason> {
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "csam" => TakedownReason::Csam,
        "copyright" | "dmca" => TakedownReason::Copyright,
        "terrorism" => TakedownReason::Terrorism,
        "ncii" | "revenge" => TakedownReason::Ncii,
        "doxxing" | "dox" => TakedownReason::Doxxing,
        "legal_order" | "court" | "legal" => TakedownReason::LegalOrder,
        "malware" => TakedownReason::Malware,
        "tos" | "terms_of_service" => TakedownReason::TermsOfService,
        "other" => TakedownReason::Other,
        _ => anyhow::bail!(
            "unknown reason '{s}'. Expected one of: csam, copyright, terrorism, ncii, doxxing, legal_order, malware, tos, other"
        ),
    })
}

fn parse_source(s: &str) -> anyhow::Result<BlocklistSource> {
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "ncmec" => BlocklistSource::Ncmec,
        "iwf" => BlocklistSource::Iwf,
        "interpol" => BlocklistSource::Interpol,
        "operator" | "" => BlocklistSource::Operator,
        "trusted_feed" | "feed" => BlocklistSource::TrustedFeed,
        "legal_order" | "court" => BlocklistSource::LegalOrder,
        "governance" | "dao" => BlocklistSource::Governance,
        _ => anyhow::bail!(
            "unknown source '{s}'. Expected one of: ncmec, iwf, interpol, operator, trusted_feed, legal_order, governance"
        ),
    })
}

fn parse_target(s: &str) -> anyhow::Result<TakedownTarget> {
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "blocklist-only" | "blocklist" => TakedownTarget::BlocklistOnly,
        "local-erase" | "erase" | "" => TakedownTarget::LocalErase,
        "crypto-shred" | "shred" => TakedownTarget::CryptoShred,
        _ => anyhow::bail!(
            "unknown target '{s}'. Expected one of: blocklist-only, local-erase, crypto-shred"
        ),
    })
}

fn parse_hash(s: &str) -> anyhow::Result<ContentHash> {
    ContentHash::from_hex(s.trim_start_matches('/'))
        .map_err(|e| anyhow::anyhow!("invalid content hash '{s}': {e}"))
}

fn defense_state_path(data_dir: &Path) -> PathBuf {
    data_dir.join("moderation").join("defense.json")
}

#[derive(serde::Serialize, serde::Deserialize, Default, Clone, Debug)]
struct DefenseState {
    #[serde(default)]
    deny_by_default: bool,
}

fn read_defense_state(data_dir: &Path) -> DefenseState {
    let path = defense_state_path(data_dir);
    let Ok(bytes) = std::fs::read(&path) else {
        return DefenseState::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn write_defense_state(data_dir: &Path, state: &DefenseState) -> anyhow::Result<()> {
    let path = defense_state_path(data_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(state)?;
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

// ───────────────────────────────────────────────────────────────────────
// command bodies
// ───────────────────────────────────────────────────────────────────────

fn cmd_status(data_dir: &Path) -> anyhow::Result<()> {
    let bl = load_blocklist(data_dir)?;
    let s = bl.stats();
    let deny = read_defense_state(data_dir).deny_by_default;
    println!(
        "moderation: active={} total={} deny_by_default={} reasons={:?}",
        s.active, s.total, deny, s.by_reason
    );
    Ok(())
}

fn cmd_list(data_dir: &Path, active_only: bool, json_out: bool) -> anyhow::Result<()> {
    let bl = load_blocklist(data_dir)?;
    let entries = if active_only {
        bl.list_active()
    } else {
        bl.list()
    };
    if json_out {
        let body = serde_json::to_string_pretty(&entries)
            .map_err(|e| anyhow::anyhow!("serde_json: {e}"))?;
        println!("{body}");
        return Ok(());
    }
    if entries.is_empty() {
        println!("(no blocklist entries)");
        return Ok(());
    }
    println!(
        "{:<6}  {:<14}  {:<14}  {:<10}  issued_unix  evidence",
        "id", "hash", "reason", "source"
    );
    for e in &entries {
        let h = if e.hash.as_hex().len() > 14 {
            format!("{}…", &e.hash.as_hex()[..12])
        } else {
            e.hash.as_hex().to_string()
        };
        let r = format!("{:?}", e.reason).to_lowercase();
        let s = format!("{:?}", e.source).to_lowercase();
        println!(
            "{:<6}  {:<14}  {:<14}  {:<10}  {:<12}  {} {}",
            e.id,
            h,
            r,
            s,
            e.issued_unix,
            e.evidence,
            if e.revoked { "[REVOKED]" } else { "" }
        );
    }
    Ok(())
}

fn cmd_block(
    data_dir: &Path,
    cid: &str,
    reason: &str,
    source: &str,
    evidence: &str,
    operator: &str,
    expires: Option<i64>,
    publisher: Option<&str>,
    json_out: bool,
) -> anyhow::Result<()> {
    let bl = load_blocklist(data_dir)?;
    let reason = parse_reason(reason)?;
    let source = parse_source(source)?;
    let hash = parse_hash(cid)?;
    let id = bl.add(
        hash.clone(),
        reason,
        source,
        evidence,
        operator,
        expires,
        publisher.unwrap_or("").to_string(),
    )?;
    if json_out {
        let entry = bl.lookup_active(&hash).unwrap_or_else(|| {
            // If the new entry was appended but lookup_active raced
            // with an immediate revoke, fall back to listing.
            bl.list().into_iter().find(|e| e.id == id).expect("entry")
        });
        let body = serde_json::to_string_pretty(&entry)?;
        println!("{body}");
    } else {
        println!("blocked id={id} hash={} reason={:?}", hash.as_hex(), reason);
    }
    Ok(())
}

fn cmd_unblock(data_dir: &Path, cid: &str) -> anyhow::Result<()> {
    let bl = load_blocklist(data_dir)?;
    let hash = parse_hash(cid)?;
    let id = bl
        .list()
        .into_iter()
        .find(|e| e.hash == hash && !e.revoked)
        .map(|e| e.id)
        .ok_or_else(|| anyhow::anyhow!("no active blocklist entry for {cid}"))?;
    bl.revoke(id)?;
    println!("unblocked id={id} hash={}", hash.as_hex());
    Ok(())
}

fn cmd_revoke(data_dir: &Path, id: u64) -> anyhow::Result<()> {
    let bl = load_blocklist(data_dir)?;
    if bl.revoke(id)? {
        println!("revoked id={id}");
    } else {
        println!("id={id} not found in blocklist (already revoked or never existed)");
    }
    Ok(())
}

fn cmd_erase(
    data_dir: &Path,
    cid: &str,
    reason: &str,
    source: &str,
    evidence: &str,
    operator: &str,
    target: &str,
    publisher: Option<&str>,
    expires: Option<i64>,
    json_out: bool,
) -> anyhow::Result<()> {
    let bl = load_blocklist(data_dir)?;
    let reason = parse_reason(reason)?;
    let source = parse_source(source)?;
    let target = parse_target(target)?;
    let hash = parse_hash(cid)?;
    let mut cfg = TakedownServiceConfig::from_data_dir(data_dir);
    // Optional: if the operator runs with an encrypted store the
    // key file is at `<data_dir>/keys/encrypted.key`; we surface
    // it here so `erase --target crypto-shred` Just Works.
    let key_path = data_dir.join("keys").join("encrypted.key");
    if key_path.exists() {
        cfg = cfg.with_key_file(key_path);
    }
    let reputation = Arc::new(PeerScoreTable::new(ReputationParams::default()));
    let svc = TakedownService::new(bl.clone(), cfg).with_reputation(reputation);
    let report = svc.execute(
        hash.clone(),
        reason,
        source,
        operator,
        evidence,
        publisher.unwrap_or("").to_string(),
        target,
        expires,
    )?;
    if json_out {
        let body = serde_json::to_string_pretty(&report)?;
        println!("{body}");
    } else {
        println!("{}", report.summary_line());
    }
    Ok(())
}

fn cmd_defend(data_dir: &Path, on: bool) -> anyhow::Result<()> {
    let mut state = read_defense_state(data_dir);
    state.deny_by_default = on;
    write_defense_state(data_dir, &state)?;
    let policy = ModerationPolicy::permissive();
    let _ = policy;
    println!(
        "deny-by-default = {on} (persisted at {})",
        defense_state_path(data_dir).display()
    );
    Ok(())
}

fn cmd_policy(data_dir: &Path, json_out: bool) -> anyhow::Result<()> {
    let bl = load_blocklist(data_dir)?;
    let s = bl.stats();
    let deny = read_defense_state(data_dir).deny_by_default;
    let policy = ModerationPolicy::new(bl);
    policy.set_deny_by_default(deny);
    if json_out {
        let body = serde_json::json!({
            "blocklist": s,
            "deny_by_default": deny,
            "classifier_count": policy.classifier_count(),
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        println!(
            "policy: deny_by_default={} active_blocks={} total_blocks={} classifiers={}",
            deny,
            s.active,
            s.total,
            policy.classifier_count()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_types::ContentHash;

    /// 64-char hex (BLAKE3) used as a deterministic test CID.
    fn sample_hash(byte: u8) -> ContentHash {
        ContentHash::from_bytes(&[byte; 32])
    }

    #[test]
    fn parse_reason_accepts_every_alias() {
        assert!(matches!(parse_reason("csam").unwrap(), TakedownReason::Csam));
        assert!(matches!(parse_reason("CSAM").unwrap(), TakedownReason::Csam));
        assert!(matches!(parse_reason("copyright").unwrap(), TakedownReason::Copyright));
        assert!(matches!(parse_reason("DMCA").unwrap(), TakedownReason::Copyright));
        assert!(matches!(parse_reason("terrorism").unwrap(), TakedownReason::Terrorism));
        assert!(matches!(parse_reason("ncii").unwrap(), TakedownReason::Ncii));
        assert!(matches!(parse_reason("revenge").unwrap(), TakedownReason::Ncii));
        assert!(matches!(parse_reason("doxxing").unwrap(), TakedownReason::Doxxing));
        assert!(matches!(parse_reason("DOX").unwrap(), TakedownReason::Doxxing));
        assert!(matches!(parse_reason("legal_order").unwrap(), TakedownReason::LegalOrder));
        assert!(matches!(parse_reason("court").unwrap(), TakedownReason::LegalOrder));
        assert!(matches!(parse_reason("malware").unwrap(), TakedownReason::Malware));
        assert!(matches!(parse_reason("tos").unwrap(), TakedownReason::TermsOfService));
        assert!(matches!(parse_reason("terms_of_service").unwrap(), TakedownReason::TermsOfService));
        assert!(matches!(parse_reason("other").unwrap(), TakedownReason::Other));
    }

    #[test]
    fn parse_reason_rejects_unknown() {
        let err = parse_reason("banana").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown reason 'banana'"), "msg: {msg}");
    }

    #[test]
    fn parse_source_accepts_every_alias() {
        assert!(matches!(parse_source("ncmec").unwrap(), BlocklistSource::Ncmec));
        assert!(matches!(parse_source("IWF").unwrap(), BlocklistSource::Iwf));
        assert!(matches!(parse_source("interpol").unwrap(), BlocklistSource::Interpol));
        // Empty string defaults to Operator.
        assert!(matches!(parse_source("").unwrap(), BlocklistSource::Operator));
        assert!(matches!(parse_source("operator").unwrap(), BlocklistSource::Operator));
        assert!(matches!(parse_source("trusted_feed").unwrap(), BlocklistSource::TrustedFeed));
        assert!(matches!(parse_source("FEED").unwrap(), BlocklistSource::TrustedFeed));
        assert!(matches!(parse_source("legal_order").unwrap(), BlocklistSource::LegalOrder));
        assert!(matches!(parse_source("court").unwrap(), BlocklistSource::LegalOrder));
        assert!(matches!(parse_source("governance").unwrap(), BlocklistSource::Governance));
        assert!(matches!(parse_source("dao").unwrap(), BlocklistSource::Governance));
    }

    #[test]
    fn parse_source_rejects_unknown() {
        let err = parse_source("nothing-here").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown source 'nothing-here'"), "msg: {msg}");
    }

    #[test]
    fn parse_target_accepts_every_alias() {
        assert!(matches!(parse_target("blocklist-only").unwrap(), TakedownTarget::BlocklistOnly));
        assert!(matches!(parse_target("blocklist").unwrap(), TakedownTarget::BlocklistOnly));
        assert!(matches!(parse_target("local-erase").unwrap(), TakedownTarget::LocalErase));
        assert!(matches!(parse_target("erase").unwrap(), TakedownTarget::LocalErase));
        // Empty string defaults to LocalErase.
        assert!(matches!(parse_target("").unwrap(), TakedownTarget::LocalErase));
        assert!(matches!(parse_target("crypto-shred").unwrap(), TakedownTarget::CryptoShred));
        assert!(matches!(parse_target("shred").unwrap(), TakedownTarget::CryptoShred));
    }

    #[test]
    fn parse_target_rejects_unknown() {
        let err = parse_target("nuke").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown target 'nuke'"), "msg: {msg}");
    }

    #[test]
    fn parse_hash_accepts_hex_with_or_without_slash_prefix() {
        let hash = sample_hash(0x42);
        let hex = hash.as_hex().to_string();
        assert_eq!(parse_hash(&hex).unwrap(), hash);
        assert_eq!(parse_hash(&format!("/{hex}")).unwrap(), hash);
    }

    #[test]
    fn parse_hash_rejects_invalid_hex() {
        let err = parse_hash("not-hex").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid content hash"), "msg: {msg}");
    }

    #[test]
    fn defense_state_roundtrip_via_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Default is deny_by_default = false.
        let initial = read_defense_state(dir.path());
        assert!(!initial.deny_by_default);

        // Persist a flipped state.
        let mut next = initial.clone();
        next.deny_by_default = true;
        write_defense_state(dir.path(), &next).expect("write");

        // Reload from disk and verify the change stuck.
        let reloaded = read_defense_state(dir.path());
        assert!(reloaded.deny_by_default);

        // The file lives at moderation/defense.json under data_dir.
        let expected = defense_state_path(dir.path());
        assert!(expected.exists(), "defense state should be on disk");
    }

    #[test]
    fn defense_state_missing_file_returns_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        // No write — read should not panic and should return default.
        let s = read_defense_state(dir.path());
        assert!(!s.deny_by_default);
    }

    #[test]
    fn defense_state_malformed_file_returns_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = defense_state_path(dir.path());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, b"not-json").unwrap();
        let s = read_defense_state(dir.path());
        assert!(!s.deny_by_default);
    }

    #[test]
    fn defense_state_path_layout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = defense_state_path(dir.path());
        assert!(p.ends_with("moderation/defense.json"), "got {p:?}");
    }

    #[test]
    fn load_blocklist_returns_arc_for_empty_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bl = load_blocklist(dir.path()).expect("load");
        let s = bl.stats();
        assert_eq!(s.active, 0);
        assert_eq!(s.total, 0);
        // Arc<Blocklist> is what load_blocklist returns — make sure
        // callers can hold multiple references without an issue.
        let b2 = bl.clone();
        assert_eq!(Arc::strong_count(&bl), 2);
        assert_eq!(bl.stats().total, b2.stats().total);
    }

    #[test]
    fn cmd_block_then_unblock_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hash = sample_hash(0xAB);

        // Block via the lower-level API to drive the helper codepaths
        // exercised by cmd_block. We don't call cmd_block directly
        // because it relies on stdout; we exercise the same primitive.
        let bl = load_blocklist(dir.path()).unwrap();
        let id = bl
            .add(
                hash.clone(),
                TakedownReason::Copyright,
                BlocklistSource::Operator,
                "evidence-X",
                "operator-1",
                None,
                "".to_string(),
            )
            .unwrap();
        assert!(id > 0);

        // After block, lookup_active should find the entry.
        let active = bl.lookup_active(&hash).expect("active");
        assert_eq!(active.id, id);

        // cmd_revoke via API: mark it revoked.
        assert!(bl.revoke(id).unwrap());

        // No active entry remains.
        assert!(bl.lookup_active(&hash).is_none());
        // But the audit row is still listed.
        let all = bl.list();
        assert_eq!(all.len(), 1);
        assert!(all[0].revoked);
    }

    #[test]
    fn cmd_revoke_unknown_id_returns_false_via_api() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bl = load_blocklist(dir.path()).unwrap();
        // No entries — revoking id=999 returns false, not an error.
        let res = bl.revoke(999);
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), false);
    }

    #[test]
    fn cmd_status_on_empty_blocklist_succeeds() {
        // Just exercise the path that doesn't touch the network.
        let dir = tempfile::tempdir().expect("tempdir");
        let bl = load_blocklist(dir.path()).unwrap();
        let s = bl.stats();
        assert_eq!(s.active, 0);
        assert_eq!(s.total, 0);
        // cmd_status writes to stdout — we just assert the
        // precondition that read_defense_state doesn't panic.
        let deny = read_defense_state(dir.path()).deny_by_default;
        assert!(!deny);
    }

    #[test]
    fn cmd_list_json_round_trips_empty_blocklist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bl = load_blocklist(dir.path()).unwrap();
        let entries = bl.list_active();
        let json = serde_json::to_string_pretty(&entries).unwrap();
        // Empty list serializes to "[]".
        assert_eq!(json, "[]");
    }

    #[test]
    fn run_moderation_dispatches_every_variant() {
        // Confirms the dispatcher matches every ModerationCmd variant
        // without panicking on construction. We assert only that the
        // pattern arms exhaustively cover the enum.
        let dir = tempfile::tempdir().expect("tempdir");

        // Touch every variant by constructing it then dispatching
        // through run_moderation. cmd_status is the only one we expect
        // to succeed on an empty data_dir; the others fail fast but
        // we still confirm dispatch works.
        let status_cmd = ModerationCmd::Status;
        run_moderation(&status_cmd, dir.path()).expect("status");

        let list_cmd = ModerationCmd::List {
            active: true,
            json: false,
        };
        run_moderation(&list_cmd, dir.path()).expect("list");

        // Block with an invalid CID to verify error propagation.
        let block_bad = ModerationCmd::Block {
            cid: "garbage".into(),
            reason: "copyright".into(),
            source: "operator".into(),
            evidence: String::new(),
            operator: "cli".into(),
            expires: None,
            publisher: None,
            json: false,
        };
        assert!(run_moderation(&block_bad, dir.path()).is_err());

        // Block with a valid CID should succeed.
        let hash = sample_hash(0x11);
        let block_ok = ModerationCmd::Block {
            cid: hash.as_hex().to_string(),
            reason: "copyright".into(),
            source: "operator".into(),
            evidence: "DMCA-1".into(),
            operator: "alice".into(),
            expires: Some(1_700_000_000),
            publisher: None,
            json: true,
        };
        run_moderation(&block_ok, dir.path()).expect("block ok");

        // Unblock the same hash.
        let unblock_cmd = ModerationCmd::Unblock {
            cid: hash.as_hex().to_string(),
        };
        run_moderation(&unblock_cmd, dir.path()).expect("unblock");

        // Revoke an unknown id succeeds (no-op) — returns false.
        let revoke_bad = ModerationCmd::Revoke { id: 999 };
        assert!(run_moderation(&revoke_bad, dir.path()).is_ok());

        // Erase on a hash with no active entry succeeds (cmd_erase returns
        // Ok(()) for both erased and absent cases; we only error on
        // truly invalid hashes like "garbage").
        let erase_no_entry = ModerationCmd::Erase {
            cid: hash.as_hex().to_string(),
            reason: "tos".into(),
            source: "operator".into(),
            evidence: "policy-violation".into(),
            operator: "alice".into(),
            target: "blocklist-only".into(),
            publisher: None,
            expires: None,
            json: false,
        };
        // After the unblock+revoke above, the entry is revoked and
        // inactive. erase on inactive entries returns Ok(false), and
        // cmd_erase propagates that as Ok(()).
        run_moderation(&erase_no_entry, dir.path()).expect("erase no active");

        // Erase with a genuinely invalid CID fails.
        let erase_bad = ModerationCmd::Erase {
            cid: "garbage".into(),
            reason: "tos".into(),
            source: "operator".into(),
            evidence: "policy-violation".into(),
            operator: "alice".into(),
            target: "blocklist-only".into(),
            publisher: None,
            expires: None,
            json: false,
        };
        assert!(run_moderation(&erase_bad, dir.path()).is_err());

        // Defend on/off — file-only side effects.
        run_moderation(&ModerationCmd::DefendOn, dir.path()).expect("defend on");
        run_moderation(&ModerationCmd::DefendOff, dir.path()).expect("defend off");

        // Policy view.
        run_moderation(
            &ModerationCmd::Policy { json: true },
            dir.path(),
        )
        .expect("policy json");
        run_moderation(
            &ModerationCmd::Policy { json: false },
            dir.path(),
        )
        .expect("policy");
    }

    #[test]
    fn cmd_defend_toggles_persisted_state() {
        let dir = tempfile::tempdir().expect("tempdir");

        // Initially deny_by_default=false.
        assert!(!read_defense_state(dir.path()).deny_by_default);

        // Flip on.
        run_moderation(&ModerationCmd::DefendOn, dir.path()).unwrap();
        assert!(read_defense_state(dir.path()).deny_by_default);

        // Flip off.
        run_moderation(&ModerationCmd::DefendOff, dir.path()).unwrap();
        assert!(!read_defense_state(dir.path()).deny_by_default);
    }

    #[test]
    fn cmd_policy_json_output_is_well_formed() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Just confirm the helper functions invoked by cmd_policy
        // don't panic on an empty directory.
        let bl = load_blocklist(dir.path()).unwrap();
        let s = bl.stats();
        let deny = read_defense_state(dir.path()).deny_by_default;
        let policy = ModerationPolicy::new(bl);
        policy.set_deny_by_default(deny);

        let body = serde_json::json!({
            "blocklist": s,
            "deny_by_default": deny,
            "classifier_count": policy.classifier_count(),
        });
        let pretty = serde_json::to_string_pretty(&body).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&pretty).unwrap();
        assert_eq!(parsed["deny_by_default"], serde_json::json!(false));
        assert!(parsed["blocklist"].is_object());
    }

    #[test]
    fn defense_state_serialization_round_trip() {
        // Confirms the (de)serialization is stable across a write+read.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut s = DefenseState::default();
        s.deny_by_default = true;
        write_defense_state(dir.path(), &s).unwrap();

        let bytes = std::fs::read(defense_state_path(dir.path())).unwrap();
        let parsed: DefenseState = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.deny_by_default, s.deny_by_default);
    }
}
