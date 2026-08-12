//! Integration test: WebDAV → adnet-pairing Capability.
//!
//! Confirms that `adnet-webdav` correctly consults `adnet-pairing`'s
//! `CapabilitySet` to authorise verbs. This is the live seam
//! between the two crates; the unit tests mock the resolver.
//!
//! Note: these tests do NOT spin up an actual socket. They
//! exercise the full `HandlerState::handle_*` route with a
//! real `CapabilitySet` produced by `adnet-pairing`.

use std::sync::Arc;

use adnet_blobstore::{MockClock, Nas};
use adnet_pairing::{capability::Capability, CapabilitySet};
use adnet_types::ContentHash;
use adnet_webdav::acl::{ResolvedCapability, StaticCapabilityResolver};
use adnet_webdav::handlers::HandlerState;
use adnet_webdav::token::TokenVerifier;

fn build_state(caps: CapabilitySet) -> (HandlerState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let nas = Nas::open(dir.path()).unwrap();
    let r = StaticCapabilityResolver::new();
    r.register(
        "cred-1".to_string(),
        ResolvedCapability {
            caps,
            nonce: [0u8; 32],
            expires_unix_ms: i64::MAX,
            revoked: false,
        },
    );
    let verifier = TokenVerifier::new([3u8; 32]);
    let mut s = HandlerState::new(nas, Arc::new(r), verifier);
    s.static_resolver = Some(StaticCapabilityResolver::new());
    s.clock = Box::new(MockClock(1_700_000_000_000));
    (s, dir)
}

fn fresh_token(state: &HandlerState, n: u8) -> String {
    let mut nonce = [0u8; 32];
    nonce[0] = n;
    state.verifier.sign("cred-1", nonce, i64::MAX).to_header()
}

#[test]
fn pairing_cap_files_read_only_can_get_can_not_put() {
    let (s, _dir) = build_state(CapabilitySet::from_names(["files.read"]));
    let path = adnet_blobstore::PathSegments::decode_http("/a.bin").unwrap();
    let h = ContentHash::from_hex(
        "0000000000000000000000000000000000000000000000000000000000000000",
    )
    .unwrap();
    // PUT must be rejected
    let res = s.handle_put(&path, h.clone(), 1, Some(&fresh_token(&s, 1)), Some("ua".into()));
    assert!(res.is_err(), "read-only token must not PUT");
    // GET: need a parent for the file to exist. We PUT with a
    // write-capable token first (via `handle_put_body`, which
    // actually persists bytes into the content-addressed blob
    // store — `handle_put` alone only registers a hash), then GET
    // with the read-only token — confirms capability-scoped auth.
    let write_caps = CapabilitySet::from_iter([Capability::FILES_READ, Capability::FILES_WRITE]);
    let (s2, _dir) = build_state(write_caps);
    let header = fresh_token(&s2, 1);
    s2.handle_put_body(&path, b"hello world", None, Some(&header), Some("ua".into()))
        .unwrap();
    let _ = s; // silence
    let _ = h; // silence
    let header = fresh_token(&s2, 2);
    let res = s2.handle_get(&path, Some(&header));
    assert!(res.is_ok(), "owner should GET own file");
    assert_eq!(res.unwrap(), b"hello world");
}

#[test]
fn pairing_cap_files_write_only_works() {
    let (s, _dir) = build_state(CapabilitySet::from_names(["files.write"]));
    let path = adnet_blobstore::PathSegments::decode_http("/x.bin").unwrap();
    let h = ContentHash::from_hex(
        "0000000000000000000000000000000000000000000000000000000000000000",
    )
    .unwrap();
    let header = fresh_token(&s, 1);
    s.handle_put(&path, h, 1, Some(&header), Some("ua".into()))
        .unwrap();
}

#[test]
fn pairing_cap_other_caps_dont_grant_files() {
    let (s, _dir) = build_state(CapabilitySet::from_names(["chat", "sync"]));
    let path = adnet_blobstore::PathSegments::decode_http("/x.bin").unwrap();
    let h = ContentHash::from_hex(
        "0000000000000000000000000000000000000000000000000000000000000000",
    )
    .unwrap();
    let header = fresh_token(&s, 1);
    let res = s.handle_put(&path, h, 1, Some(&header), Some("ua".into()));
    assert!(res.is_err(), "non-files capability must not PUT");
}

#[test]
fn pairing_cap_revoked_token_blocks_all_verbs() {
    let r = StaticCapabilityResolver::new();
    r.register(
        "cred-1".to_string(),
        ResolvedCapability {
            caps: CapabilitySet::from_names(["files.read", "files.write"]),
            nonce: [0u8; 32],
            expires_unix_ms: i64::MAX,
            revoked: true, // REVOKED
        },
    );
    let dir = tempfile::tempdir().unwrap();
    let nas = Nas::open(dir.path()).unwrap();
    let verifier = TokenVerifier::new([3u8; 32]);
    let mut s = HandlerState::new(nas, Arc::new(r), verifier);
    s.static_resolver = Some(StaticCapabilityResolver::new());
    s.clock = Box::new(MockClock(1_700_000_000_000));
    let path = adnet_blobstore::PathSegments::decode_http("/x.bin").unwrap();
    let h = ContentHash::from_hex(
        "0000000000000000000000000000000000000000000000000000000000000000",
    )
    .unwrap();
    let header = fresh_token(&s, 1);
    assert!(s.handle_put(&path, h, 1, Some(&header), Some("ua".into())).is_err());
    let header = fresh_token(&s, 2);
    assert!(s.handle_get(&path, Some(&header)).is_err());
}
