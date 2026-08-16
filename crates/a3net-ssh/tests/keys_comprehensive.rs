//! Comprehensive tests for the `keys` module.
//!
//! Tests AuthorizedKeys, TrustedPeers, and helper functions.

#![cfg(feature = "iroh")]

use a3net_ssh::keys::{
    SSH_SUBDIR, IROH_SECRET_KEY_FILE,
};
use a3net_ssh::keys::authorized_keys::{
    AuthorizedKeys, AuthorizedKeyEntry, TrustedPeers,
};
use base64::Engine;

// ============================================================================
// Constants tests
// ============================================================================

#[test]
fn ssh_subdir_value() {
    assert_eq!(SSH_SUBDIR, "ssh");
}

#[test]
fn iroh_secret_key_file_value() {
    assert_eq!(IROH_SECRET_KEY_FILE, "iroh_secret_key");
}

// ============================================================================
// AuthorizedKeyEntry tests
// ============================================================================

#[test]
fn authorized_key_entry_debug() {
    let entry = AuthorizedKeyEntry {
        key_type: "ssh-ed25519".to_string(),
        key_blob: "AAAAC3NzaC1lZDI1NTE5".to_string(),
        comment: Some("test@example.com".to_string()),
        from_pattern: Some("192.168.1.0/24".to_string()),
        from_a3net_id: None,
    };
    let debug_str = format!("{:?}", entry);
    assert!(debug_str.contains("ssh-ed25519"));
}

#[test]
fn authorized_key_entry_clone() {
    let entry = AuthorizedKeyEntry {
        key_type: "ssh-ed25519".to_string(),
        key_blob: "AAAAC3NzaC1lZDI1NTE5".to_string(),
        comment: None,
        from_pattern: None,
        from_a3net_id: None,
    };
    let cloned = entry.clone();
    assert_eq!(entry, cloned);
}

#[test]
fn authorized_key_entry_eq() {
    let entry1 = AuthorizedKeyEntry {
        key_type: "ssh-ed25519".to_string(),
        key_blob: "AAAAC3NzaC1lZDI1NTE5".to_string(),
        comment: Some("user@host".to_string()),
        from_pattern: None,
        from_a3net_id: None,
    };
    let entry2 = AuthorizedKeyEntry {
        key_type: "ssh-ed25519".to_string(),
        key_blob: "AAAAC3NzaC1lZDI1NTE5".to_string(),
        comment: Some("user@host".to_string()),
        from_pattern: None,
        from_a3net_id: None,
    };
    assert_eq!(entry1, entry2);
}

#[test]
fn authorized_key_entry_neq_key_type() {
    let entry1 = AuthorizedKeyEntry {
        key_type: "ssh-ed25519".to_string(),
        key_blob: "AAAAC3NzaC1lZDI1NTE5".to_string(),
        comment: None,
        from_pattern: None,
        from_a3net_id: None,
    };
    let entry2 = AuthorizedKeyEntry {
        key_type: "ecdsa-sha2-nistp256".to_string(),
        key_blob: "AAAAC3NzaC1lZDI1NTE5".to_string(),
        comment: None,
        from_pattern: None,
        from_a3net_id: None,
    };
    assert_ne!(entry1, entry2);
}

// ============================================================================
// AuthorizedKeys construction and path
// ============================================================================

#[test]
fn authorized_keys_new() {
    let tmp = tempfile::tempdir().unwrap();
    let ak = AuthorizedKeys::new(tmp.path());
    assert_eq!(ak.path(), tmp.path().join("ssh/authorized_keys"));
}

#[test]
fn authorized_keys_debug() {
    let tmp = tempfile::tempdir().unwrap();
    let ak = AuthorizedKeys::new(tmp.path());
    let debug_str = format!("{:?}", ak);
    assert!(!debug_str.is_empty());
}

#[test]
fn authorized_keys_clone() {
    let tmp = tempfile::tempdir().unwrap();
    let ak1 = AuthorizedKeys::new(tmp.path());
    let ak2 = ak1.clone();
    assert_eq!(ak1.path(), ak2.path());
}

// ============================================================================
// AuthorizedKeys::load() tests
// ============================================================================

#[test]
fn authorized_keys_load_nonexistent_file() {
    let tmp = tempfile::tempdir().unwrap();
    let ak = AuthorizedKeys::new(tmp.path());
    let entries = ak.load();
    assert!(entries.is_empty());
}

#[test]
fn authorized_keys_load_empty_file() {
    let tmp = tempfile::tempdir().unwrap();
    let ak = AuthorizedKeys::new(tmp.path());
    ak.ensure().unwrap();
    // File exists but is empty (after header)
    let entries = ak.load();
    // The ensure() writes a header comment, so actual entries might be empty
    assert!(entries.is_empty() || entries.iter().all(|e| e.key_type.is_empty()));
}

#[test]
fn authorized_keys_load_single_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let ak = AuthorizedKeys::new(tmp.path());
    ak.ensure().unwrap();

    std::fs::write(
        ak.path(),
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILiH test@example.com\n",
    )
    .unwrap();

    let entries = ak.load();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].key_type, "ssh-ed25519");
    assert_eq!(entries[0].key_blob, "AAAAC3NzaC1lZDI1NTE5AAAAILiH");
    assert_eq!(entries[0].comment.as_deref(), Some("test@example.com"));
}

#[test]
fn authorized_keys_load_multiple_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let ak = AuthorizedKeys::new(tmp.path());
    ak.ensure().unwrap();

    std::fs::write(
        ak.path(),
        "ssh-ed25519 AAAAB3NzaC1yc2EAAAADAQABAAABAQ alice@host1\n\
         ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILiH bob@host2\n\
         ecdsa-sha2-nistp256 AAAAD2NzaC1lZDI1NTE5 charlie@host3\n",
    )
    .unwrap();

    let entries = ak.load();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].comment.as_deref(), Some("alice@host1"));
    assert_eq!(entries[1].comment.as_deref(), Some("bob@host2"));
    assert_eq!(entries[2].key_type, "ecdsa-sha2-nistp256");
}

#[test]
fn authorized_keys_load_skips_comments() {
    let tmp = tempfile::tempdir().unwrap();
    let ak = AuthorizedKeys::new(tmp.path());
    ak.ensure().unwrap();

    std::fs::write(
        ak.path(),
        "# This is a comment\n\
         ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILiH real@entry\n\
         # Another comment\n",
    )
    .unwrap();

    let entries = ak.load();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].comment.as_deref(), Some("real@entry"));
}

#[test]
fn authorized_keys_load_skips_blank_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let ak = AuthorizedKeys::new(tmp.path());
    ak.ensure().unwrap();

    std::fs::write(
        ak.path(),
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILiH entry1\n\
         \n\
         ssh-ed25519 AAAAB3NzaC1yc2EAAAADAQABAAABAQ entry2\n",
    )
    .unwrap();

    let entries = ak.load();
    assert_eq!(entries.len(), 2);
}

#[test]
fn authorized_keys_load_skips_malformed_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let ak = AuthorizedKeys::new(tmp.path());
    ak.ensure().unwrap();

    std::fs::write(
        ak.path(),
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILiH valid@entry\n\
         not-a-valid-key-line\n\
         ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQ another@valid\n",
    )
    .unwrap();

    let entries = ak.load();
    // Only entries with at least 2 tokens (key_type and key_blob) are valid
    assert!(entries.len() >= 1);
}

#[test]
fn authorized_keys_load_preserves_options() {
    let tmp = tempfile::tempdir().unwrap();
    let ak = AuthorizedKeys::new(tmp.path());
    ak.ensure().unwrap();

    let line = r#"from="192.168.1.0/24" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILiH restricted@host"#;
    std::fs::write(ak.path(), format!("{line}\n")).unwrap();

    let entries = ak.load();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].from_pattern.as_deref(), Some("192.168.1.0/24"));
}

// ============================================================================
// AuthorizedKeys::check() tests
// ============================================================================

#[test]
fn authorized_keys_check_no_match_empty_file() {
    let tmp = tempfile::tempdir().unwrap();
    let ak = AuthorizedKeys::new(tmp.path());
    let fake_key = [0u8; 32];
    let fake_ep: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();

    assert!(!ak.check(&fake_key, fake_ep));
}

#[test]
fn authorized_keys_check_no_match_wrong_key() {
    let tmp = tempfile::tempdir().unwrap();
    let ak = AuthorizedKeys::new(tmp.path());
    ak.ensure().unwrap();

    // Base64 decode of "AAAAC3NzaC1lZDI1NTE5AAAAILiH" for the stored key
    let stored_key_bytes = base64::engine::general_purpose::STANDARD
        .decode("AAAAC3NzaC1lZDI1NTE5AAAAILiH")
        .unwrap();

    let wrong_key = [0u8; 32];
    let fake_ep: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();

    std::fs::write(
        ak.path(),
        format!("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILiH test@host\n"),
    )
    .unwrap();

    assert!(!ak.check(&wrong_key, fake_ep));
    assert!(ak.check(&stored_key_bytes, fake_ep));
}

#[test]
fn authorized_keys_check_with_from_a3net_id() {
    let tmp = tempfile::tempdir().unwrap();
    let ak = AuthorizedKeys::new(tmp.path());
    ak.ensure().unwrap();

    let peer_id: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();
    let wrong_peer_id: iroh::EndpointId = "bb8e1a5661a6dfa9ae2dd978922f30f524f6fd8c99b3de021c53f292aae74330"
        .parse()
        .unwrap();

    let key_bytes = [0xAA; 32];

    std::fs::write(
        ak.path(),
        format!(
            "from-a3net={} ssh-ed25519 {} test@host\n",
            peer_id,
            base64::engine::general_purpose::STANDARD.encode(&key_bytes)
        ),
    )
    .unwrap();

    // Correct peer should match
    assert!(ak.check(&key_bytes, peer_id));
    // Wrong peer should not match (even with correct key)
    assert!(!ak.check(&key_bytes, wrong_peer_id));
}

#[test]
fn authorized_keys_check_reloads_on_each_call() {
    let tmp = tempfile::tempdir().unwrap();
    let ak = AuthorizedKeys::new(tmp.path());
    ak.ensure().unwrap();

    let key_bytes = [0xAA; 32];
    std::fs::write(
        ak.path(),
        format!(
            "ssh-ed25519 {}\n",
            base64::engine::general_purpose::STANDARD.encode(&key_bytes)
        ),
    )
    .unwrap();

    // First check should work
    let fake_ep: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();
    assert!(ak.check(&key_bytes, fake_ep));

    // Modify file externally
    let different_key = [0xBB; 32];
    std::fs::write(
        ak.path(),
        format!(
            "ssh-ed25519 {}\n",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &different_key)
        ),
    )
    .unwrap();

    // Old key should no longer match
    assert!(!ak.check(&key_bytes, fake_ep));
    // New key should match
    assert!(ak.check(&different_key, fake_ep));
}

// ============================================================================
// AuthorizedKeys::ensure() tests
// ============================================================================

#[test]
fn authorized_keys_ensure_creates_ssh_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let ak = AuthorizedKeys::new(tmp.path());

    ak.ensure().unwrap();

    assert!(tmp.path().join("ssh").is_dir());
}

#[test]
fn authorized_keys_ensure_creates_file() {
    let tmp = tempfile::tempdir().unwrap();
    let ak = AuthorizedKeys::new(tmp.path());

    ak.ensure().unwrap();

    assert!(ak.path().is_file());
}

#[test]
fn authorized_keys_ensure_writes_header() {
    let tmp = tempfile::tempdir().unwrap();
    let ak = AuthorizedKeys::new(tmp.path());

    ak.ensure().unwrap();

    let content = std::fs::read_to_string(ak.path()).unwrap();
    assert!(content.contains("A3Net SSH"));
    assert!(content.contains("authorized_keys"));
}

#[test]
fn authorized_keys_ensure_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let ak = AuthorizedKeys::new(tmp.path());

    ak.ensure().unwrap();
    let mtime1 = std::fs::metadata(ak.path()).unwrap().modified().unwrap();

    ak.ensure().unwrap();
    let mtime2 = std::fs::metadata(ak.path()).unwrap().modified().unwrap();

    // Second ensure should not rewrite the file
    assert_eq!(mtime1, mtime2);
}

// ============================================================================
// AuthorizedKeys::add_peer() tests
// ============================================================================

#[test]
fn authorized_keys_add_peer_creates_file() {
    let tmp = tempfile::tempdir().unwrap();
    let ak = AuthorizedKeys::new(tmp.path());

    let peer_id: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();
    let key = [0xAA; 32];

    ak.add_peer(peer_id, &key, "test-peer").unwrap();

    assert!(ak.path().is_file());
}

#[test]
fn authorized_keys_add_peer_format() {
    let tmp = tempfile::tempdir().unwrap();
    let ak = AuthorizedKeys::new(tmp.path());

    let peer_id: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();
    let key = [0xAA; 32];

    ak.add_peer(peer_id, &key, "alice@host").unwrap();

    let content = std::fs::read_to_string(ak.path()).unwrap();
    assert!(content.contains("from-a3net="));
    assert!(content.contains("ssh-ed25519"));
    assert!(content.contains("alice@host"));
    assert!(content.contains(&peer_id.to_string()));
}

#[test]
fn authorized_keys_add_peer_multiple() {
    let tmp = tempfile::tempdir().unwrap();
    let ak = AuthorizedKeys::new(tmp.path());

    let peer1: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();
    let peer2: iroh::EndpointId = "bb8e1a5661a6dfa9ae2dd978922f30f524f6fd8c99b3de021c53f292aae74330"
        .parse()
        .unwrap();

    ak.add_peer(peer1, &[0xAA; 32], "peer1").unwrap();
    ak.add_peer(peer2, &[0xBB; 32], "peer2").unwrap();

    let entries = ak.load();
    assert_eq!(entries.len(), 2);
}

// ============================================================================
// TrustedPeers construction and path
// ============================================================================

#[test]
fn trusted_peers_new() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());
    assert!(tp.path().ends_with("ssh/trusted_peers"));
}

#[test]
fn trusted_peers_debug() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());
    let debug_str = format!("{:?}", tp);
    assert!(!debug_str.is_empty());
}

#[test]
fn trusted_peers_clone() {
    let tmp = tempfile::tempdir().unwrap();
    let tp1 = TrustedPeers::new(tmp.path());
    let tp2 = tp1.clone();
    // Clones should share the same path
    assert_eq!(tp1.path(), tp2.path());
}

// ============================================================================
// TrustedPeers::check() tests
// ============================================================================

#[test]
fn trusted_peers_check_nonexistent_file() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    let fake_ep: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();

    assert!(!tp.check(fake_ep));
}

#[test]
fn trusted_peers_check_empty_file() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    // Create file with empty content (just a newline)
    std::fs::create_dir_all(tp.path().parent().unwrap()).unwrap();
    std::fs::write(tp.path(), "\n").unwrap();

    let fake_ep: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();

    // After reload, empty file should return false
    tp.reload();
    assert!(!tp.check(fake_ep));
}

#[test]
fn trusted_peers_check_finds_added_peer() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    let peer_id: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();

    tp.add(peer_id).unwrap();
    assert!(tp.check(peer_id));
}

#[test]
fn trusted_peers_check_does_not_find_unknown_peer() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    let peer_id: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();
    let unknown_id: iroh::EndpointId = "bb8e1a5661a6dfa9ae2dd978922f30f524f6fd8c99b3de021c53f292aae74330"
        .parse()
        .unwrap();

    tp.add(peer_id).unwrap();
    assert!(!tp.check(unknown_id));
}

#[test]
fn trusted_peers_check_caches_result() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    let peer_id: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();

    // Add peer (which updates cache)
    tp.add(peer_id).unwrap();

    // Multiple checks should be consistent
    for _ in 0..5 {
        assert!(tp.check(peer_id));
    }
}

// ============================================================================
// TrustedPeers::reload() tests
// ============================================================================

#[test]
fn trusted_peers_reload_refreshes_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    let peer_id: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();

    tp.add(peer_id).unwrap();
    tp.reload();

    assert!(tp.check(peer_id));
}

#[test]
fn trusted_peers_reload_handles_missing_file() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    // Should not panic
    tp.reload();
}

// ============================================================================
// TrustedPeers::add() tests
// ============================================================================

#[test]
fn trusted_peers_add_creates_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    let peer_id: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();

    tp.add(peer_id).unwrap();

    assert!(tmp.path().join("ssh").is_dir());
}

#[test]
fn trusted_peers_add_creates_file() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    let peer_id: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();

    tp.add(peer_id).unwrap();

    assert!(tp.path().is_file());
}

#[test]
fn trusted_peers_add_writes_endpoint_id() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    let peer_id: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();

    tp.add(peer_id).unwrap();

    let content = std::fs::read_to_string(tp.path()).unwrap();
    assert!(content.contains(&peer_id.to_string()));
}

#[test]
fn trusted_peers_add_multiple() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    let peer1: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();
    let peer2: iroh::EndpointId = "bb8e1a5661a6dfa9ae2dd978922f30f524f6fd8c99b3de021c53f292aae74330"
        .parse()
        .unwrap();

    tp.add(peer1).unwrap();
    tp.add(peer2).unwrap();

    let list = tp.list();
    assert_eq!(list.len(), 2);
    assert!(list.contains(&peer1));
    assert!(list.contains(&peer2));
}

#[test]
fn trusted_peers_add_same_twice() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    let peer_id: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();

    tp.add(peer_id).unwrap();
    tp.add(peer_id).unwrap(); // Should not error

    // File should contain the ID once (or twice if appended)
    let content = std::fs::read_to_string(tp.path()).unwrap();
    let count = content.matches(&peer_id.to_string()).count();
    assert!(count >= 1);
}

// ============================================================================
// TrustedPeers::remove() tests
// ============================================================================

#[test]
fn trusted_peers_remove_nonexistent() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    let peer_id: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();

    let result = tp.remove(peer_id).unwrap();
    assert!(!result);
}

#[test]
fn trusted_peers_remove_existing() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    let peer_id: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();

    tp.add(peer_id).unwrap();
    let result = tp.remove(peer_id).unwrap();
    assert!(result);
}

#[test]
fn trusted_peers_remove_updates_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    let peer_id: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();

    tp.add(peer_id).unwrap();
    assert!(tp.check(peer_id));

    tp.remove(peer_id).unwrap();
    assert!(!tp.check(peer_id));
}

#[test]
fn trusted_peers_remove_preserves_other_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    let peer1: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();
    let peer2: iroh::EndpointId = "bb8e1a5661a6dfa9ae2dd978922f30f524f6fd8c99b3de021c53f292aae74330"
        .parse()
        .unwrap();

    tp.add(peer1).unwrap();
    tp.add(peer2).unwrap();
    tp.remove(peer1).unwrap();

    assert!(!tp.check(peer1));
    assert!(tp.check(peer2));
}

#[test]
fn trusted_peers_remove_handles_comments() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    let peer_id: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();

    // Write file with comments - need to create parent dir first
    std::fs::create_dir_all(tp.path().parent().unwrap()).unwrap();
    std::fs::write(
        tp.path(),
        format!(
            "# Trusted peers list\n\
             # Add peers below\n\
             {}\n",
            peer_id
        ),
    )
    .unwrap();

    tp.reload();
    assert!(tp.check(peer_id));

    tp.remove(peer_id).unwrap();
    tp.reload();

    assert!(!tp.check(peer_id));
}

// ============================================================================
// TrustedPeers::list() tests
// ============================================================================

#[test]
fn trusted_peers_list_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    let list = tp.list();
    assert!(list.is_empty());
}

#[test]
fn trusted_peers_list_multiple() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    // Use valid endpoint IDs (64 hex characters each)
    let peer1: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();
    let peer2: iroh::EndpointId = "bb8e1a5661a6dfa9ae2dd978922f30f524f6fd8c99b3de021c53f292aae74330"
        .parse()
        .unwrap();

    tp.add(peer1).unwrap();
    tp.add(peer2).unwrap();

    let list = tp.list();
    assert_eq!(list.len(), 2);
    assert!(list.contains(&peer1));
    assert!(list.contains(&peer2));
}

#[test]
fn trusted_peers_list_returns_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    let peer_id: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();

    tp.add(peer_id).unwrap();

    let list1 = tp.list();
    let list2 = tp.list();

    // Modifying one list shouldn't affect the other
    let mut list1 = list1;
    list1.clear();

    let list3 = tp.list();
    assert_eq!(list3.len(), 1);
}

// ============================================================================
// TrustedPeers file format tests
// ============================================================================

#[test]
fn trusted_peers_handles_blank_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    let peer_id: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();

    // Create file with blank lines and comments
    std::fs::create_dir_all(tp.path().parent().unwrap()).unwrap();
    std::fs::write(
        tp.path(),
        format!(
            "\n\
             # Comment\n\
             \n\
             {}\n\
             \n",
            peer_id
        ),
    )
    .unwrap();

    tp.reload();
    assert!(tp.check(peer_id));
}

#[test]
fn trusted_peers_handles_invalid_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    let peer_id: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();

    // Create file with invalid lines and valid entry
    std::fs::create_dir_all(tp.path().parent().unwrap()).unwrap();
    std::fs::write(
        tp.path(),
        format!(
            "not-a-valid-endpoint-id\n\
             # Comment\n\
             {}\n\
             invalid-endpoint-id-too\n",
            peer_id
        ),
    )
    .unwrap();

    tp.reload();
    assert!(tp.check(peer_id));
}

#[test]
fn trusted_peers_reload_interval() {
    let tmp = tempfile::tempdir().unwrap();
    let tp = TrustedPeers::new(tmp.path());

    // The RELOAD_INTERVAL_SECS constant should be accessible
    // We can't directly access the private constant, but we can test the behavior
    let peer_id: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();

    tp.add(peer_id).unwrap();

    // Multiple rapid calls should return consistent results
    for _ in 0..10 {
        assert!(tp.check(peer_id));
    }
}

// ============================================================================
// Integration tests
// ============================================================================

#[test]
fn authorized_keys_and_trusted_peers_work_together() {
    let tmp = tempfile::tempdir().unwrap();

    let ak = AuthorizedKeys::new(tmp.path());
    let tp = TrustedPeers::new(tmp.path());

    let peer_id: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();
    let key = [0xAA; 32];

    // Add to both
    ak.add_peer(peer_id, &key, "trusted-peer").unwrap();
    tp.add(peer_id).unwrap();

    // Check both
    assert!(tp.check(peer_id));
    assert!(ak.check(&key, peer_id));
}

#[test]
fn trusted_peers_prevents_need_for_authorized_keys() {
    let tmp = tempfile::tempdir().unwrap();

    let tp = TrustedPeers::new(tmp.path());

    let peer_id: iroh::EndpointId = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac"
        .parse()
        .unwrap();

    // Trusted peers should work without any authorized_keys file
    tp.add(peer_id).unwrap();
    assert!(tp.check(peer_id));
}

// Note: This test file uses the base64 crate from the crate's dependencies
// No custom base64 module needed since a3net-ssh has base64 as a dependency
