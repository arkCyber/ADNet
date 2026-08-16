//! Integration tests for the moderation hooks in `a3net-gateway`.
//!
//! These tests cover two surfaces:
//!
//! 1. **HTTP status mapping** — [`GatewayError::ContentBlocked`]
//!    must round-trip to **HTTP 451 Unavailable For Legal Reasons**
//!    (RFC 7725). This is the contract every gateway backend
//!    depends on.
//!
//! 2. **Policy enforcement** — a [`ModerationPolicy`] backing
//!    a [`Blocklist`] denies blocked hashes (read AND write) and
//!    allows everything else.
//!
//! 3. **Role gating** — the [`Role::Moderator`] variant is wired
//!    through [`AuthService::authorize`] so the
//!    `/api/v0/moderation/*` endpoint family only accepts
//!    Admin / Moderator sessions.
//!
//! The end-to-end HTTP roundtrip through `hyper` is exercised by
//! the `a3net-cli` `moderation` smoke test in `smoke.rs` so we
//! don't duplicate the loopback dance here.

#![allow(clippy::redundant_clone)]

use std::sync::Arc;

use a3net_blobstore::BlobStore;
use a3net_gateway::{AuthorizationResult, GatewayError, Role};
use a3net_moderation::{Blocklist, BlocklistSource, ModerationPolicy, TakedownReason};
use a3net_types::ContentHash;
use http::StatusCode;

// ───────────────────────────────────────────────────────────────────────
// 1) HTTP status mapping
// ───────────────────────────────────────────────────────────────────────

#[test]
fn content_blocked_maps_to_http_451() {
    let err = GatewayError::ContentBlocked("csam takedown case 12".to_string());
    assert_eq!(
        err.status_code(),
        StatusCode::from_u16(451).unwrap(),
        "blocked content must surface as 451 per RFC 7725"
    );
    let body = err.to_json_response();
    assert_eq!(body["Code"], 451);
}

#[test]
fn other_error_statuses_are_unchanged() {
    // Sanity: ensure we didn't perturb sibling variants.
    assert_eq!(GatewayError::NotFound("x".into()).status_code(), StatusCode::NOT_FOUND);
    assert_eq!(GatewayError::InvalidPath("x".into()).status_code(), StatusCode::BAD_REQUEST);
    assert_eq!(GatewayError::InvalidCid("x".into()).status_code(), StatusCode::BAD_REQUEST);
    assert_eq!(GatewayError::Timeout.status_code(), StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(
        GatewayError::PayloadTooLarge(10).status_code(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        GatewayError::Internal("x".into()).status_code(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        GatewayError::MethodNotAllowed("x".into()).status_code(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(GatewayError::CorsNotAllowed.status_code(), StatusCode::FORBIDDEN);
}

// ───────────────────────────────────────────────────────────────────────
// 2) Policy + blocklist enforcement
// ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn policy_blocks_blocklisted_hash() {
    let dir = tempfile::tempdir().unwrap();
    let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
    let policy = ModerationPolicy::new(bl.clone());

    let h = ContentHash::from_bytes(b"blocked-hash");
    // Empty blocklist → allowed.
    assert!(policy.check_read(&h).is_allowed());
    assert!(policy.check_write(&h).is_allowed());

    // Populate the blocklist.
    bl.add(
        h.clone(),
        TakedownReason::Csam,
        BlocklistSource::Ncmec,
        "case 12",
        "alice",
        None,
        "",
    )
    .unwrap();

    // Now denied for both read and write.
    let d = policy.check_read(&h);
    assert!(!d.is_allowed());
    assert!(d.reason.contains("csam"));
    assert!(d.source_entry.is_some());
    let d = policy.check_write(&h);
    assert!(!d.is_allowed());
}

#[tokio::test]
async fn policy_allows_after_revoke() {
    let dir = tempfile::tempdir().unwrap();
    let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
    let policy = ModerationPolicy::new(bl.clone());
    let h = ContentHash::from_bytes(b"once-bad");
    let id = bl
        .add(
            h.clone(),
            TakedownReason::Copyright,
            BlocklistSource::Operator,
            "",
            "alice",
            None,
            "",
        )
        .unwrap();
    assert!(!policy.check_read(&h).is_allowed());
    assert!(bl.revoke(id).unwrap());
    assert!(policy.check_read(&h).is_allowed());
}

#[tokio::test]
async fn policy_default_deny_blocks_unlisted() {
    let dir = tempfile::tempdir().unwrap();
    let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
    let policy = ModerationPolicy::new(bl);
    policy.set_deny_by_default(true);
    let h = ContentHash::from_bytes(b"unlisted");
    assert!(!policy.check_read(&h).is_allowed());
    let d = policy.check_read(&h);
    assert!(d.reason.contains("default-deny"));
}

#[tokio::test]
async fn policy_round_trips_through_persistence() {
    // The blocklist should survive a reload — the gateway reloads
    // its in-memory view from disk on every change, so the on-disk
    // shape is the contract.
    let dir = tempfile::tempdir().unwrap();
    let blob_dir = dir.path().join("blobs");
    let _ = BlobStore::new(&blob_dir).expect("blob store");
    let bl1 = Blocklist::load(dir.path()).unwrap();
    bl1.add(
        ContentHash::from_bytes(b"x"),
        TakedownReason::Csam,
        BlocklistSource::Ncmec,
        "case 12",
        "alice",
        None,
        "",
    )
    .unwrap();
    drop(bl1);

    let bl2 = Arc::new(Blocklist::load(dir.path()).unwrap());
    let policy = ModerationPolicy::new(bl2.clone());
    let h = ContentHash::from_bytes(b"x");
    assert!(!policy.check_read(&h).is_allowed());

    // Stats reflect a single active block.
    let s = bl2.stats();
    assert_eq!(s.active, 1);
    assert_eq!(s.total, 1);
}

// ───────────────────────────────────────────────────────────────────────
// 3) Role gating for the moderation endpoint family
// ───────────────────────────────────────────────────────────────────────

#[test]
fn admin_can_call_moderation_endpoints() {
    let r = a3net_gateway::AuthService::authorize(
        Role::Admin,
        "POST",
        "/api/v0/moderation/block",
    );
    assert!(matches!(r, AuthorizationResult::Allowed));
}

#[test]
fn moderator_can_call_moderation_endpoints() {
    let r = a3net_gateway::AuthService::authorize(
        Role::Moderator,
        "POST",
        "/api/v0/moderation/block",
    );
    assert!(matches!(r, AuthorizationResult::Allowed));
    let r = a3net_gateway::AuthService::authorize(
        Role::Moderator,
        "GET",
        "/api/v0/moderation/list",
    );
    assert!(matches!(r, AuthorizationResult::Allowed));
}

#[test]
fn writer_cannot_call_moderation_endpoints() {
    let r = a3net_gateway::AuthService::authorize(
        Role::Write,
        "POST",
        "/api/v0/moderation/block",
    );
    assert!(matches!(r, AuthorizationResult::Denied { .. }));
}

#[test]
fn reader_cannot_call_moderation_endpoints() {
    let r = a3net_gateway::AuthService::authorize(
        Role::Read,
        "GET",
        "/api/v0/moderation/list",
    );
    assert!(matches!(r, AuthorizationResult::Denied { .. }));
}

#[test]
fn moderator_cannot_call_admin_endpoints() {
    // The /api/v0/admin/* surface stays Admin-only even for Moderator.
    let r = a3net_gateway::AuthService::authorize(
        Role::Moderator,
        "GET",
        "/api/v0/admin/users",
    );
    assert!(matches!(r, AuthorizationResult::Denied { .. }));
}

#[test]
fn moderator_role_round_trip_json() {
    // The Role enum's `serde(rename_all = "lowercase")` mapping
    // must round-trip cleanly so `config.json` values stay portable.
    let json = serde_json::to_string(&Role::Moderator).unwrap();
    assert_eq!(json, "\"moderator\"");
    let parsed: Role = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, Role::Moderator);
}

// ───────────────────────────────────────────────────────────────────────
// 4) CLI binary smoke test (lightweight, no listener required)
// ───────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod cli_smoke {
    use std::process::Command;

    /// Resolve the `a3net` binary. In release-mode CI we run from
    /// `target/release/a3net`, in dev mode from
    /// `target/debug/a3net`.
    fn a3net_binary() -> std::path::PathBuf {
        let manifest_dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir
            .ancestors()
            .nth(2)
            .expect("workspace root")
            .to_path_buf();
        let debug = workspace_root.join("target/debug/a3net");
        let release = workspace_root.join("target/release/a3net");
        if release.exists() {
            release
        } else {
            debug
        }
    }

    #[test]
    fn moderation_help_lists_subcommands() {
        let bin = a3net_binary();
        if !bin.exists() {
            // Skip silently when the binary hasn't been built —
            // happens in the doc-test crate set.
            eprintln!("skipping: a3net binary not found at {}", bin.display());
            return;
        }
        let out = Command::new(&bin)
            .args(["moderation", "--help"])
            .output()
            .expect("run a3net moderation --help");
        assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("block"), "missing `block` sub: {}", stdout);
        assert!(stdout.contains("unblock"), "missing `unblock` sub");
        assert!(stdout.contains("erase"), "missing `erase` sub");
        assert!(stdout.contains("defend-on"), "missing `defend-on` sub");
        assert!(stdout.contains("defend-off"), "missing `defend-off` sub");
        assert!(stdout.contains("policy"), "missing `policy` sub");
    }
}
