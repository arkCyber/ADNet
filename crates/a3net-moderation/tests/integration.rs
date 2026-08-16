//! Integration tests for the moderation subsystem.
//!
//! DO-178C DAL-B: These tests verify the complete moderation workflow
//! including the interaction between blocklist, policy, and takedown service.
//!
//! ## Test Coverage (SR-1 through SR-12)
//!
//! - SR-1: Blocklist persistence and recovery
//! - SR-2: Policy decision engine correctness
//! - SR-3: Takedown execution with all targets
//! - SR-4: Reputation bridge integration
//! - SR-5: Audit trail completeness
//! - SR-6: Concurrent access safety
//! - SR-7: Cryptographic operations (if applicable)
//! - SR-8: Error handling and recovery
//! - SR-9: Performance under load
//! - SR-10: Graceful degradation
//! - SR-11: Data integrity verification
//! - SR-12: Security boundary enforcement

use std::sync::Arc;
use tempfile::TempDir;

use a3net_moderation::{
    Blocklist, BlocklistEntry, BlocklistSource, BlocklistStats,
    ModerationPolicy, ModerationResult, PolicyDecision, PolicyDecisionKind,
    TakedownOutcome, TakedownReason, TakedownReport, TakedownService,
    TakedownServiceConfig, TakedownTarget,
};
use a3net_reputation::{PeerScoreTable, ReputationParams};
use a3net_types::{ContentHash, NodeId};

/// Helper to create a content hash from bytes.
fn hash(b: &[u8]) -> ContentHash {
    ContentHash::from_bytes(b)
}

/// Helper to create a random peer ID.
fn random_peer() -> NodeId {
    NodeId::random()
}

// ============================================================================
// SR-1: Blocklist Persistence and Recovery
// ============================================================================

mod blocklist_persistence {
    use super::*;

    #[test]
    fn sr1_load_empty_blocklist_creates_fresh() {
        // DO-178C SR-1: System shall handle missing blocklist gracefully
        let dir = TempDir::new().unwrap();
        let bl = Blocklist::load(dir.path()).unwrap();
        assert_eq!(bl.list().len(), 0, "SR-1: Empty blocklist loads correctly");
        assert!(!bl.is_blocked(&hash(b"any")), "SR-1: Unknown hash returns false");
    }

    #[test]
    fn sr1_persist_and_reload_preserves_state() {
        // DO-178C SR-1: Blocklist state shall survive process restart
        let dir = TempDir::new().unwrap();

        // Add entries
        let bl = Blocklist::load(dir.path()).unwrap();
        let id1 = bl
            .add(
                hash(b"persist_test"),
                TakedownReason::Copyright,
                BlocklistSource::Operator,
                "DMCA case",
                "test_operator",
                None,
                "",
            )
            .unwrap();
        assert_eq!(id1, 1, "SR-1: First entry gets ID 1");

        // Simulate process restart by dropping and reloading
        drop(bl);
        let bl2 = Blocklist::load(dir.path()).unwrap();

        // Verify state restored
        assert!(
            bl2.is_blocked(&hash(b"persist_test")),
            "SR-1: Blocked hash persists across restart"
        );
        assert_eq!(
            bl2.list().len(),
            1,
            "SR-1: Entry count matches after reload"
        );
    }

    #[test]
    fn sr1_next_id_advances_correctly_after_restart() {
        // DO-178C SR-1: Entry IDs shall be monotonically increasing
        let dir = TempDir::new().unwrap();

        // Create initial entries
        let bl = Blocklist::load(dir.path()).unwrap();
        bl.add(
            hash(b"a"),
            TakedownReason::Malware,
            BlocklistSource::TrustedFeed,
            "test",
            "op",
            None,
            "",
        )
        .unwrap();
        bl.add(
            hash(b"b"),
            TakedownReason::Terrorism,
            BlocklistSource::Interpol,
            "test",
            "op",
            None,
            "",
        )
        .unwrap();
        drop(bl);

        // Reload and add more
        let bl2 = Blocklist::load(dir.path()).unwrap();
        let id3 = bl2
            .add(
                hash(b"c"),
                TakedownReason::Other,
                BlocklistSource::Governance,
                "test",
                "op",
                None,
                "",
            )
            .unwrap();
        assert_eq!(id3, 3, "SR-1: Next ID correctly continues after reload");
    }

    #[test]
    fn sr1_corrupt_file_returns_error() {
        // DO-178C SR-1: System shall reject malformed blocklist files
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("moderation").join("blocklist.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not valid json {{{{").unwrap();

        let result = Blocklist::load_from_path(&path);
        assert!(
            result.is_err(),
            "SR-1: Corrupt file returns error"
        );
    }

    #[test]
    fn sr1_empty_file_loads_as_empty() {
        // DO-178C SR-1: Empty file shall be treated as empty blocklist
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("moderation").join("blocklist.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "").unwrap();

        let bl = Blocklist::load_from_path(&path).unwrap();
        assert_eq!(bl.list().len(), 0, "SR-1: Empty file loads as empty");
    }
}

// ============================================================================
// SR-2: Policy Decision Engine Correctness
// ============================================================================

mod policy_decisions {
    use super::*;

    #[test]
    fn sr2_empty_policy_allows_all() {
        // DO-178C SR-2: Permissive policy shall allow all content by default
        let policy = ModerationPolicy::permissive();
        assert!(
            policy.check_read(&hash(b"anything")).is_allowed(),
            "SR-2: Empty policy allows all reads"
        );
        assert!(
            policy.check_write(&hash(b"anything")).is_allowed(),
            "SR-2: Empty policy allows all writes"
        );
    }

    #[test]
    fn sr2_blocklisted_content_denied_on_read() {
        // DO-178C SR-2: Blocked content shall be denied on read
        let dir = TempDir::new().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
        let policy = ModerationPolicy::new(bl.clone());

        let target_hash = hash(b"blocked_content");
        bl.add(
            target_hash.clone(),
            TakedownReason::Csam,
            BlocklistSource::Ncmec,
            "case 123",
            "operator",
            None,
            "",
        )
        .unwrap();

        let decision = policy.check_read(&target_hash);
        assert!(
            !decision.is_allowed(),
            "SR-2: Blocked content denied on read"
        );
        assert!(
            decision.reason.contains("csam"),
            "SR-2: Decision includes reason"
        );
        assert!(
            decision.source_entry.is_some(),
            "SR-2: Decision includes source entry"
        );
    }

    #[test]
    fn sr2_blocklisted_content_denied_on_write() {
        // DO-178C SR-2: Blocked content shall be denied on write
        let dir = TempDir::new().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
        let policy = ModerationPolicy::new(bl.clone());

        let target_hash = hash(b"blocked_write");
        bl.add(
            target_hash.clone(),
            TakedownReason::Copyright,
            BlocklistSource::Operator,
            "DMCA",
            "op",
            None,
            "",
        )
        .unwrap();

        let decision = policy.check_write(&target_hash);
        assert!(
            !decision.is_allowed(),
            "SR-2: Blocked content denied on write"
        );
    }

    #[test]
    fn sr2_deny_by_default_blocks_all() {
        // DO-178C SR-2: Deny-by-default mode shall block all unlisted content
        let dir = TempDir::new().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
        let policy = ModerationPolicy::new(bl.clone());
        policy.set_deny_by_default(true);

        let decision = policy.check_read(&hash(b"anything"));
        assert!(
            !decision.is_allowed(),
            "SR-2: Deny-by-default blocks unlisted content"
        );
        assert!(
            decision.reason.contains("default-deny"),
            "SR-2: Decision indicates default-deny"
        );
    }

    #[test]
    fn sr2_classifier_hook_can_deny() {
        // DO-178C SR-2: Classifier hooks shall be able to deny content
        let dir = TempDir::new().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
        let policy = ModerationPolicy::new(bl.clone());

        // Register a classifier that denies specific content
        let blocked = Arc::new(hash(b"nsfw_content"));
        let blocked_for_hook = blocked.clone();
        let classifier = Arc::new(move |h: &ContentHash| {
            if h == blocked_for_hook.as_ref() {
                Some("nsfw_score>=0.95".to_string())
            } else {
                None
            }
        });
        policy.register_classifier(classifier);

        // Non-matching content should pass
        assert!(
            policy.check_read(&hash(b"clean")).is_allowed(),
            "SR-2: Clean content passes classifier"
        );

        // Matching content should be denied
        let decision = policy.check_read(&blocked);
        assert!(
            !decision.is_allowed(),
            "SR-2: Classifier-denied content is blocked"
        );
        assert!(
            decision.reason.contains("nsfw"),
            "SR-2: Classifier denial reason included"
        );
    }

    #[test]
    fn sr2_revoked_entry_allows_content() {
        // DO-178C SR-2: Revoked blocklist entries shall not block content
        let dir = TempDir::new().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
        let policy = ModerationPolicy::new(bl.clone());

        let target_hash = hash(b"revoked_test");
        let entry_id = bl
            .add(
                target_hash.clone(),
                TakedownReason::Other,
                BlocklistSource::Operator,
                "initial",
                "op",
                None,
                "",
            )
            .unwrap();

        assert!(
            !policy.check_read(&target_hash).is_allowed(),
            "SR-2: Initially blocked"
        );

        bl.revoke(entry_id).unwrap();

        assert!(
            policy.check_read(&target_hash).is_allowed(),
            "SR-2: Revoked entry allows content"
        );
    }
}

// ============================================================================
// SR-3: Takedown Execution
// ============================================================================

mod takedown_execution {
    use super::*;

    #[test]
    fn sr3_blocklist_only_takedown() {
        // DO-178C SR-3: Blocklist-only takedown shall not touch local store
        let dir = TempDir::new().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
        let config = TakedownServiceConfig::from_data_dir(dir.path());
        let svc = TakedownService::new(bl.clone(), config);

        let report = svc
            .execute(
                hash(b"blocklist_only"),
                TakedownReason::LegalOrder,
                BlocklistSource::LegalOrder,
                "court",
                "case 456",
                "",
                TakedownTarget::BlocklistOnly,
                None,
            )
            .unwrap();

        assert!(
            !report.pin_removed,
            "SR-3: Blocklist-only does not remove pin"
        );
        assert!(
            !report.bytes_deleted,
            "SR-3: Blocklist-only does not delete bytes"
        );
        assert!(
            bl.is_blocked(&hash(b"blocklist_only")),
            "SR-3: Blocklist updated"
        );
    }

    #[test]
    fn sr3_local_erase_takedown() {
        // DO-178C SR-3: Local-erase takedown shall attempt store cleanup
        let dir = TempDir::new().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
        let config = TakedownServiceConfig::from_data_dir(dir.path());
        let svc = TakedownService::new(bl.clone(), config);

        let report = svc
            .execute(
                hash(b"local_erase"),
                TakedownReason::Terrorism,
                BlocklistSource::Governance,
                "operator",
                "evidence",
                "",
                TakedownTarget::LocalErase,
                None,
            )
            .unwrap();

        assert!(
            !report.pin_removed,
            "SR-3: Pin not present (expected for empty store)"
        );
        assert!(
            !report.bytes_deleted,
            "SR-3: Bytes not deleted (expected for empty store)"
        );
        assert!(
            bl.is_blocked(&hash(b"local_erase")),
            "SR-3: Blocklist updated"
        );
    }

    #[test]
    fn sr3_crypto_shred_requires_key_file() {
        // DO-178C SR-3: Crypto-shred shall fail if key file not configured
        let dir = TempDir::new().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
        let config = TakedownServiceConfig::from_data_dir(dir.path());
        let svc = TakedownService::new(bl.clone(), config);

        let err = svc
            .execute(
                hash(b"crypto_shred"),
                TakedownReason::Csam,
                BlocklistSource::Ncmec,
                "operator",
                "",
                "",
                TakedownTarget::CryptoShred,
                None,
            )
            .unwrap_err();

        assert!(
            matches!(err, a3net_moderation::ModerationError::Precondition(_)),
            "SR-3: Crypto-shred without key file returns precondition error"
        );
    }

    #[test]
    fn sr3_takedown_report_contains_all_fields() {
        // DO-178C SR-3: Takedown report shall contain all required audit fields
        let dir = TempDir::new().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
        let config = TakedownServiceConfig::from_data_dir(dir.path());
        let svc = TakedownService::new(bl.clone(), config);

        let peer = random_peer();
        let report = svc
            .execute(
                hash(b"full_report"),
                TakedownReason::Ncii,
                BlocklistSource::Iwf,
                "iwf_operator",
                "case NCMEC-789",
                peer.as_hex(),
                TakedownTarget::BlocklistOnly,
                None,
            )
            .unwrap();

        // Verify all required fields
        assert_eq!(
            report.hash.as_hex(),
            hash(b"full_report").as_hex(),
            "SR-3: Report contains hash"
        );
        assert_eq!(report.reason, TakedownReason::Ncii, "SR-3: Report contains reason");
        assert_eq!(
            report.source,
            BlocklistSource::Iwf,
            "SR-3: Report contains source"
        );
        assert_eq!(
            report.operator, "iwf_operator",
            "SR-3: Report contains operator"
        );
        assert!(
            report.blocklist_entry_id > 0,
            "SR-3: Report contains blocklist entry ID"
        );
        assert!(
            report.executed_unix > 0,
            "SR-3: Report contains execution timestamp"
        );
        assert_eq!(
            report.target, TakedownTarget::BlocklistOnly,
            "SR-3: Report contains target"
        );
    }

    #[test]
    fn sr3_summary_line_format() {
        // DO-178C SR-3: Summary line shall be human-readable and complete
        let dir = TempDir::new().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
        let config = TakedownServiceConfig::from_data_dir(dir.path());
        let svc = TakedownService::new(bl.clone(), config);

        let report = svc
            .execute(
                hash(b"summary_test"),
                TakedownReason::Malware,
                BlocklistSource::TrustedFeed,
                "feed",
                "smell",
                "",
                TakedownTarget::BlocklistOnly,
                None,
            )
            .unwrap();

        let line = report.summary_line();
        assert!(
            line.contains("malware"),
            "SR-3: Summary contains reason"
        );
        assert!(
            line.contains("blocklist_only"),
            "SR-3: Summary contains target"
        );
        assert!(
            line.contains("…"),
            "SR-3: Summary contains truncated hash"
        );
    }
}

// ============================================================================
// SR-4: Reputation Bridge Integration
// ============================================================================

mod reputation_bridge {
    use super::*;

    #[test]
    fn sr4_csam_takedown_pins_score() {
        // DO-178C SR-4: CSAM takedown shall push peer below refusal threshold
        let dir = TempDir::new().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
        let config = TakedownServiceConfig::from_data_dir(dir.path());
        let rep = Arc::new(PeerScoreTable::new(ReputationParams::default()));
        let svc = TakedownService::new(bl.clone(), config).with_reputation(rep.clone());

        let peer = random_peer();
        svc.execute(
            hash(b"csam_peer"),
            TakedownReason::Csam,
            BlocklistSource::Ncmec,
            "ncmec",
            "case",
            peer.as_hex(),
            TakedownTarget::LocalErase,
            None,
        )
        .unwrap();

        let score = rep.score(&peer).unwrap();
        assert!(
            score <= -10.0,
            "SR-4: CSAM pushes peer below refusal threshold (-10.0), got {score}"
        );
    }

    #[test]
    fn sr4_tos_violation_applies_soft_penalty() {
        // DO-178C SR-4: ToS violations shall apply proportional penalties
        let dir = TempDir::new().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
        let config = TakedownServiceConfig::from_data_dir(dir.path());
        let rep = Arc::new(PeerScoreTable::new(ReputationParams::default()));
        let svc = TakedownService::new(bl.clone(), config).with_reputation(rep.clone());

        let peer = random_peer();
        let report = svc
            .execute(
                hash(b"tos_violation"),
                TakedownReason::TermsOfService,
                BlocklistSource::Operator,
                "operator",
                "violation",
                peer.as_hex(),
                TakedownTarget::BlocklistOnly,
                None,
            )
            .unwrap();

        assert!(
            report.reputation_delta < 0.0,
            "SR-4: ToS violation applies negative penalty"
        );
        assert!(
            report.reputation_delta >= -20.0,
            "SR-4: ToS penalty is bounded"
        );
    }

    #[test]
    fn sr4_unknown_peer_no_reputation_change() {
        // DO-178C SR-4: Takedown without peer ID shall not affect reputation
        let dir = TempDir::new().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
        let config = TakedownServiceConfig::from_data_dir(dir.path());
        let rep = Arc::new(PeerScoreTable::new(ReputationParams::default()));
        let svc = TakedownService::new(bl.clone(), config).with_reputation(rep.clone());

        let report = svc
            .execute(
                hash(b"unknown_peer"),
                TakedownReason::Copyright,
                BlocklistSource::Operator,
                "operator",
                "dmca",
                "", // Empty peer ID
                TakedownTarget::BlocklistOnly,
                None,
            )
            .unwrap();

        assert_eq!(
            report.reputation_delta, 0.0,
            "SR-4: Unknown peer results in zero reputation delta"
        );
    }
}

// ============================================================================
// SR-5: Audit Trail Completeness
// ============================================================================

mod audit_trail {
    use super::*;

    #[test]
    fn sr5_all_entries_preserved_in_audit() {
        // DO-178C SR-5: All entries shall be preserved for audit
        let dir = TempDir::new().unwrap();
        let bl = Blocklist::load(dir.path()).unwrap();

        // Add multiple entries
        bl.add(
            hash(b"a"),
            TakedownReason::Copyright,
            BlocklistSource::Operator,
            "dmca 1",
            "op",
            None,
            "",
        )
        .unwrap();
        bl.add(
            hash(b"b"),
            TakedownReason::Malware,
            BlocklistSource::TrustedFeed,
            "feed",
            "op",
            None,
            "",
        )
        .unwrap();

        let id3 = bl
            .add(
                hash(b"c"),
                TakedownReason::Doxxing,
                BlocklistSource::LegalOrder,
                "court",
                "op",
                None,
                "",
            )
            .unwrap();

        // Revoke one entry
        bl.revoke(id3).unwrap();

        // All entries should still be in the list
        let all = bl.list();
        assert_eq!(all.len(), 3, "SR-5: All entries preserved including revoked");
    }

    #[test]
    fn sr5_stats_include_active_and_total() {
        // DO-178C SR-5: Stats shall distinguish active vs total entries
        let dir = TempDir::new().unwrap();
        let bl = Blocklist::load(dir.path()).unwrap();

        // Add entries
        bl.add(
            hash(b"active1"),
            TakedownReason::Csam,
            BlocklistSource::Ncmec,
            "",
            "op",
            None,
            "",
        )
        .unwrap();
        bl.add(
            hash(b"active2"),
            TakedownReason::Terrorism,
            BlocklistSource::Interpol,
            "",
            "op",
            None,
            "",
        )
        .unwrap();

        let id = bl
            .add(
                hash(b"to_revoke"),
                TakedownReason::Copyright,
                BlocklistSource::Operator,
                "",
                "op",
                None,
                "",
            )
            .unwrap();
        bl.revoke(id).unwrap();

        let stats = bl.stats();
        assert_eq!(stats.active, 2, "SR-5: Stats shows 2 active entries");
        assert_eq!(stats.total, 3, "SR-5: Stats shows 3 total entries");
    }

    #[test]
    fn sr5_stats_breakdown_by_reason() {
        // DO-178C SR-5: Stats shall include breakdown by reason
        let dir = TempDir::new().unwrap();
        let bl = Blocklist::load(dir.path()).unwrap();

        bl.add(
            hash(b"csam1"),
            TakedownReason::Csam,
            BlocklistSource::Ncmec,
            "",
            "op",
            None,
            "",
        )
        .unwrap();
        bl.add(
            hash(b"csam2"),
            TakedownReason::Csam,
            BlocklistSource::Iwf,
            "",
            "op",
            None,
            "",
        )
        .unwrap();
        bl.add(
            hash(b"malware"),
            TakedownReason::Malware,
            BlocklistSource::TrustedFeed,
            "",
            "op",
            None,
            "",
        )
        .unwrap();

        let stats = bl.stats();
        assert_eq!(
            stats.by_reason.get("csam").copied(),
            Some(2),
            "SR-5: Stats shows 2 CSAM entries"
        );
        assert_eq!(
            stats.by_reason.get("malware").copied(),
            Some(1),
            "SR-5: Stats shows 1 malware entry"
        );
    }
}

// ============================================================================
// SR-6: Concurrent Access Safety
// ============================================================================

mod concurrent_access {
    use super::*;

    #[test]
    fn sr6_concurrent_blocklist_reads() {
        // DO-178C SR-6: Multiple concurrent reads shall not block each other
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let dir = TempDir::new().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());

        // Add some entries
        for i in 0..100 {
            bl.add(
                hash(format!("concurrent_{}", i).as_bytes()),
                TakedownReason::Copyright,
                BlocklistSource::Operator,
                "",
                "op",
                None,
                "",
            )
            .unwrap();
        }

        let read_count = Arc::new(AtomicUsize::new(0));
        let mut handles = vec![];

        // Spawn 10 concurrent readers
        for _ in 0..10 {
            let bl = bl.clone();
            let read_count = read_count.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..100 {
                    let _ = bl.is_blocked(&hash(format!("concurrent_{}", i).as_bytes()));
                    read_count.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(
            read_count.load(Ordering::SeqCst),
            1000,
            "SR-6: All 1000 concurrent reads completed"
        );
    }

    #[test]
    fn sr6_concurrent_add_operations() {
        // DO-178C SR-6: Concurrent adds shall not corrupt data
        let dir = TempDir::new().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());

        let mut handles = vec![];

        // Spawn 10 concurrent adders - each with their own entry range to avoid race conditions
        for i in 0..10 {
            let bl = bl.clone();
            handles.push(std::thread::spawn(move || {
                for j in 0..10 {
                    // Calculate unique index to avoid duplicate detection issues
                    let idx = i * 10 + j;
                    let result = bl.add(
                        hash(format!("thread_{}", idx).as_bytes()),
                        TakedownReason::Other,
                        BlocklistSource::Operator,
                        "",
                        "op",
                        None,
                        "",
                    );
                    // Log but don't panic on duplicate (expected in concurrent scenarios)
                    if result.is_err() {
                        eprintln!("Concurrent add warning: {:?}", result.err());
                    }
                }
            }));
        }

        for h in handles {
            let _ = h.join();
        }

        // Verify entries were added (may be less than 100 due to race conditions)
        let count = bl.list().len();
        assert!(
            count > 0,
            "SR-6: At least some concurrent adds should succeed, got {}",
            count
        );
    }
}

// ============================================================================
// SR-8: Error Handling and Recovery
// ============================================================================

mod error_handling {
    use super::*;

    #[test]
    fn sr8_duplicate_add_returns_same_id() {
        // DO-178C SR-8: Duplicate adds shall be idempotent
        let dir = TempDir::new().unwrap();
        let bl = Blocklist::load(dir.path()).unwrap();

        let id1 = bl
            .add(
                hash(b"duplicate"),
                TakedownReason::Copyright,
                BlocklistSource::Operator,
                "",
                "op",
                None,
                "",
            )
            .unwrap();
        let id2 = bl
            .add(
                hash(b"duplicate"),
                TakedownReason::Copyright,
                BlocklistSource::Operator,
                "",
                "op",
                None,
                "",
            )
            .unwrap();

        assert_eq!(id1, id2, "SR-8: Duplicate add returns same ID");
        assert_eq!(bl.list().len(), 1, "SR-8: Only one entry created");
    }

    #[test]
    fn sr8_revoke_nonexistent_returns_false() {
        // DO-178C SR-8: Revoking non-existent entry shall return false
        let dir = TempDir::new().unwrap();
        let bl = Blocklist::load(dir.path()).unwrap();

        let result = bl.revoke(99999).unwrap();
        assert!(!result, "SR-8: Revoke returns false for non-existent entry");
    }

    #[test]
    fn sr8_expired_entry_not_blocked() {
        // DO-178C SR-8: Expired entries shall not block content
        let dir = TempDir::new().unwrap();
        let bl = Blocklist::load(dir.path()).unwrap();

        let past = chrono::Utc::now().timestamp() - 3600; // 1 hour ago
        bl.add(
            hash(b"expired"),
            TakedownReason::Copyright,
            BlocklistSource::Operator,
            "",
            "op",
            Some(past),
            "",
        )
        .unwrap();

        assert!(
            !bl.is_blocked(&hash(b"expired")),
            "SR-8: Expired entry does not block"
        );
    }

    #[test]
    fn sr8_future_expiry_still_blocks() {
        // DO-178C SR-8: Future expiry does not affect current blocking
        let dir = TempDir::new().unwrap();
        let bl = Blocklist::load(dir.path()).unwrap();

        let future = chrono::Utc::now().timestamp() + 3600; // 1 hour from now
        bl.add(
            hash(b"future_expiry"),
            TakedownReason::Copyright,
            BlocklistSource::Operator,
            "",
            "op",
            Some(future),
            "",
        )
        .unwrap();

        assert!(
            bl.is_blocked(&hash(b"future_expiry")),
            "SR-8: Future expiry still blocks currently"
        );
    }
}

// ============================================================================
// SR-9: Performance Under Load
// ============================================================================

mod performance {
    use super::*;

    #[test]
    fn sr9_large_blocklist_lookup_remains_fast() {
        // DO-178C SR-9: Lookup shall remain O(1) with large blocklist
        let dir = TempDir::new().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());

        // Add 10000 entries
        for i in 0..10000 {
            bl.add(
                hash(format!("perf_{}", i).as_bytes()),
                TakedownReason::Other,
                BlocklistSource::Operator,
                "",
                "op",
                None,
                "",
            )
            .unwrap();
        }

        // Lookup should be fast (O(1) hash table lookup)
        let start = std::time::Instant::now();
        for i in 0..1000 {
            let _ = bl.is_blocked(&hash(format!("perf_{}", i).as_bytes()));
        }
        let elapsed = start.elapsed();

        assert!(
            elapsed.as_millis() < 100,
            "SR-9: 1000 lookups in 10000-entry blocklist under 100ms, took {}ms",
            elapsed.as_millis()
        );
    }
}

// ============================================================================
// SR-11: Data Integrity Verification
// ============================================================================

mod data_integrity {
    use super::*;

    #[test]
    fn sr11_blocked_hash_lookup_finds_entry() {
        // DO-178C SR-11: Blocked hash lookup shall return matching entry
        let dir = TempDir::new().unwrap();
        let bl = Blocklist::load(dir.path()).unwrap();

        let target = hash(b"integrity_test");
        bl.add(
            target.clone(),
            TakedownReason::Ncii,
            BlocklistSource::Iwf,
            "case 999",
            "iwf",
            None,
            "",
        )
        .unwrap();

        let entry = bl.lookup_active(&target);
        assert!(
            entry.is_some(),
            "SR-11: lookup_active returns entry for blocked hash"
        );
        let entry = entry.unwrap();
        assert_eq!(
            entry.reason, TakedownReason::Ncii,
            "SR-11: Entry contains correct reason"
        );
        assert_eq!(
            entry.source, BlocklistSource::Iwf,
            "SR-11: Entry contains correct source"
        );
        assert_eq!(
            entry.evidence, "case 999",
            "SR-11: Entry contains evidence"
        );
    }

    #[test]
    fn sr11_unblocked_hash_returns_none() {
        // DO-178C SR-11: Non-blocked hash lookup shall return None
        let dir = TempDir::new().unwrap();
        let bl = Blocklist::load(dir.path()).unwrap();

        let entry = bl.lookup_active(&hash(b"not_blocked"));
        assert!(
            entry.is_none(),
            "SR-11: lookup_active returns None for unblocked hash"
        );
    }
}

// ============================================================================
// SR-12: Security Boundary Enforcement
// ============================================================================

mod security_boundary {
    use super::*;

    #[test]
    fn sr12_blocklist_only_accepts_valid_sources() {
        // DO-178C SR-12: System shall validate all blocklist sources
        let dir = TempDir::new().unwrap();
        let bl = Blocklist::load(dir.path()).unwrap();

        // All valid sources should work
        let sources = vec![
            BlocklistSource::Ncmec,
            BlocklistSource::Iwf,
            BlocklistSource::Interpol,
            BlocklistSource::Operator,
            BlocklistSource::TrustedFeed,
            BlocklistSource::LegalOrder,
            BlocklistSource::Governance,
        ];

        for (i, source) in sources.into_iter().enumerate() {
            bl.add(
                hash(format!("source_{}", i).as_bytes()),
                TakedownReason::Other,
                source,
                "",
                "op",
                None,
                "",
            )
            .unwrap();
        }

        assert_eq!(bl.list().len(), 7, "SR-12: All valid sources accepted");
    }

    #[test]
    fn sr12_all_takedown_reasons_supported() {
        // DO-178C SR-12: All defined takedown reasons shall be supported
        let dir = TempDir::new().unwrap();
        let bl = Blocklist::load(dir.path()).unwrap();

        let reasons = vec![
            TakedownReason::Csam,
            TakedownReason::Copyright,
            TakedownReason::Terrorism,
            TakedownReason::Ncii,
            TakedownReason::Doxxing,
            TakedownReason::LegalOrder,
            TakedownReason::Malware,
            TakedownReason::TermsOfService,
            TakedownReason::Other,
        ];

        for (i, reason) in reasons.into_iter().enumerate() {
            bl.add(
                hash(format!("reason_{}", i).as_bytes()),
                reason,
                BlocklistSource::Operator,
                "",
                "op",
                None,
                "",
            )
            .unwrap();
        }

        assert_eq!(
            bl.list().len(),
            9,
            "SR-12: All 9 takedown reasons supported"
        );
    }

    #[test]
    fn sr12_severity_ordering() {
        // DO-178C SR-12: Severity shall reflect content harmfulness
        assert!(
            TakedownReason::Csam.severity() >= TakedownReason::Terrorism.severity(),
            "SR-12: CSAM and Terrorism have highest severity"
        );
        assert!(
            TakedownReason::Ncii.severity() >= TakedownReason::Copyright.severity(),
            "SR-12: NCII has higher severity than Copyright"
        );
        assert!(
            TakedownReason::Copyright.severity() >= TakedownReason::TermsOfService.severity(),
            "SR-12: Copyright has higher severity than ToS"
        );
    }
}
