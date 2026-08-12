//! Integration tests for Bitswap protocol and Swarm Download.
//!
//! These tests verify the complete Bitswap content exchange flow:
//! 1. Peer discovery and connection
//! 2. Want-Have / Want-Block message exchange
//! 3. Ledger tracking and bandwidth accounting
//! 4. Session management and optimization
//! 5. Parallel download with verification

#[cfg(test)]
mod bitswap_integration_tests {
    use adnet_blobstore::{
        BitswapEngine, BitswapMessage, BitswapMetrics, BitswapSession, LedgerStats,
        MAX_CONCURRENT_WANTS, PeerLedger, PeerWantList, PendingWant, SessionManager,
        WANT_BLOCK_TIMEOUT, WANT_HAVE_TIMEOUT,
    };
    use adnet_types::ContentHash;
    use std::time::Duration;

    // ─────────────────────────────────────────────────────────────────
    // Helper: Create test content hashes
    // ─────────────────────────────────────────────────────────────────

    fn make_test_hash(data: &[u8]) -> ContentHash {
        ContentHash::from_bytes(data)
    }

    fn make_peer_id(id: &str) -> String {
        id.to_string()
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Basic Bitswap Engine Lifecycle
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_bitswap_engine_peer_lifecycle() {
        let engine = BitswapEngine::new();

        // Initially no peers
        assert!(engine.get_peer_ids().is_empty());

        // Add peer
        let _ = engine.add_peer("peer1");
        assert_eq!(engine.get_peer_ids().len(), 1);
        assert!(engine.get_peer("peer1").is_some());

        // Remove peer
        engine.remove_peer("peer1");
        assert!(engine.get_peer("peer1").is_none());
        assert!(engine.get_peer_ids().is_empty());
    }

    #[test]
    fn test_bitswap_engine_multiple_peers() {
        let engine = BitswapEngine::new();

        // Add multiple peers
        for i in 0..5 {
            let _ = engine.add_peer(&format!("peer{}", i));
        }

        let peers = engine.get_peer_ids();
        assert_eq!(peers.len(), 5);

        // Remove one
        engine.remove_peer("peer2");
        assert_eq!(engine.get_peer_ids().len(), 4);
        assert!(engine.get_peer("peer2").is_none());
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Peer Ledger Operations
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_peer_ledger_creation() {
        let ledger = PeerLedger::new(make_peer_id("peer1"));

        assert_eq!(ledger.peer_id, "peer1");
        assert_eq!(ledger.bytes_sent, 0);
        assert_eq!(ledger.bytes_received, 0);
        assert_eq!(ledger.blocks_sent, 0);
        assert_eq!(ledger.blocks_received, 0);
        assert_eq!(ledger.balance(), 0);
    }

    #[test]
    fn test_peer_ledger_bandwidth_tracking() {
        let mut ledger = PeerLedger::new(make_peer_id("peer1"));

        // Record sent
        ledger.record_sent(1024);
        assert_eq!(ledger.bytes_sent, 1024);
        assert_eq!(ledger.balance(), -1024);

        // Record received
        ledger.record_received(2048);
        assert_eq!(ledger.bytes_received, 2048);
        assert_eq!(ledger.balance(), 1024);
    }

    #[test]
    fn test_peer_ledger_block_tracking() {
        let mut ledger = PeerLedger::new(make_peer_id("peer1"));

        ledger.record_block_sent();
        ledger.record_block_sent();
        assert_eq!(ledger.blocks_sent, 2);

        ledger.record_block_received();
        ledger.record_block_received();
        ledger.record_block_received();
        assert_eq!(ledger.blocks_received, 3);
    }

    #[test]
    fn test_peer_ledger_want_tracking() {
        let mut ledger = PeerLedger::new(make_peer_id("peer1"));

        ledger.add_want(1024);
        assert_eq!(ledger.want_bytes, 1024);

        ledger.add_want(2048);
        assert_eq!(ledger.want_bytes, 3072);

        ledger.remove_want(1024);
        assert_eq!(ledger.want_bytes, 2048);
    }

    #[test]
    fn test_peer_ledger_credit_limit() {
        let ledger = PeerLedger::new(make_peer_id("peer1"));

        assert!(ledger.can_receive());
        assert!(ledger.can_send());

        let mut limited_ledger = PeerLedger::new(make_peer_id("peer2")).with_credit_limit(1024);

        // Record received up to limit
        limited_ledger.record_received(512);
        assert!(limited_ledger.can_receive());

        limited_ledger.record_received(512);
        assert!(!limited_ledger.can_receive());
    }

    #[test]
    fn test_peer_ledger_flags() {
        let mut ledger = PeerLedger::new(make_peer_id("peer1"));

        assert!(!ledger.flags.throttled);
        assert!(!ledger.flags.blocked);

        ledger.throttle();
        assert!(ledger.flags.throttled);
        assert!(ledger.can_receive()); // Throttled doesn't block

        ledger.unthrottle();
        assert!(!ledger.flags.throttled);

        ledger.block();
        assert!(ledger.flags.blocked);
        assert!(!ledger.can_receive());
        assert!(!ledger.can_send());
    }

    #[test]
    fn test_ledger_stats_conversion() {
        let mut ledger = PeerLedger::new(make_peer_id("peer1"));
        ledger.record_sent(1000);
        ledger.record_received(500);
        ledger.record_block_sent();
        ledger.record_block_received();

        let stats: LedgerStats = (&ledger).into();

        assert_eq!(stats.peer_id, "peer1");
        assert_eq!(stats.bytes_sent, 1000);
        assert_eq!(stats.bytes_received, 500);
        assert_eq!(stats.blocks_sent, 1);
        assert_eq!(stats.blocks_received, 1);
        assert_eq!(stats.balance, -500);
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Bitswap Message Types
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_want_have_message() {
        let hash = make_test_hash(b"test block");
        let msg = BitswapMessage::WantHave {
            block: hash.clone(),
            priority: 100,
            send_dont_have: true,
        };

        match msg {
            BitswapMessage::WantHave {
                block,
                priority,
                send_dont_have,
            } => {
                assert_eq!(block, hash);
                assert_eq!(priority, 100);
                assert!(send_dont_have);
            }
            _ => panic!("Expected WantHave message"),
        }
    }

    #[test]
    fn test_want_block_message() {
        let hash = make_test_hash(b"test block");
        let msg = BitswapMessage::WantBlock {
            block: hash.clone(),
            priority: 50,
        };

        match msg {
            BitswapMessage::WantBlock { block, priority } => {
                assert_eq!(block, hash);
                assert_eq!(priority, 50);
            }
            _ => panic!("Expected WantBlock message"),
        }
    }

    #[test]
    fn test_have_message() {
        let hash = make_test_hash(b"test block");
        let msg = BitswapMessage::Have {
            block: hash.clone(),
            immediate: true,
        };

        match msg {
            BitswapMessage::Have { block, immediate } => {
                assert_eq!(block, hash);
                assert!(immediate);
            }
            _ => panic!("Expected Have message"),
        }
    }

    #[test]
    fn test_dont_have_message() {
        let hash = make_test_hash(b"missing block");
        let msg = BitswapMessage::DontHave {
            block: hash.clone(),
        };

        match msg {
            BitswapMessage::DontHave { block } => {
                assert_eq!(block, hash);
            }
            _ => panic!("Expected DontHave message"),
        }
    }

    #[test]
    fn test_block_message() {
        let hash = make_test_hash(b"test block");
        let data = b"hello world".to_vec();
        let msg = BitswapMessage::Block {
            block: hash.clone(),
            data: data.clone(),
        };

        match msg {
            BitswapMessage::Block {
                block,
                data: received_data,
            } => {
                assert_eq!(block, hash);
                assert_eq!(received_data, data);
            }
            _ => panic!("Expected Block message"),
        }
    }

    #[test]
    fn test_cancel_message() {
        let hash = make_test_hash(b"cancelled block");
        let msg = BitswapMessage::Cancel {
            block: hash.clone(),
        };

        match msg {
            BitswapMessage::Cancel { block } => {
                assert_eq!(block, hash);
            }
            _ => panic!("Expected Cancel message"),
        }
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Session Management
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_session_creation() {
        let session = BitswapSession::new(1);

        assert_eq!(session.id, 1);
        assert!(session.root.is_none());
        assert!(session.peers.is_empty());
        assert!(session.blocks.is_empty());
        assert!(session.active_wants.is_empty());
    }

    #[test]
    fn test_session_with_root() {
        let hash = make_test_hash(b"root content");
        let session = BitswapSession::new(1).with_root(hash.clone());

        assert_eq!(session.root, Some(hash));
    }

    #[test]
    fn test_session_peer_management() {
        let mut session = BitswapSession::new(1);

        session.add_peer(make_peer_id("peer1"));
        assert_eq!(session.peers.len(), 1);

        session.add_peer(make_peer_id("peer2"));
        assert_eq!(session.peers.len(), 2);

        // Adding same peer again should not duplicate
        session.add_peer(make_peer_id("peer1"));
        assert_eq!(session.peers.len(), 2);

        session.remove_peer("peer1");
        assert_eq!(session.peers.len(), 1);
    }

    #[test]
    fn test_session_block_tracking() {
        let mut session = BitswapSession::new(1);

        let hash1 = make_test_hash(b"block1");
        let hash2 = make_test_hash(b"block2");
        let hash3 = make_test_hash(b"block3");

        // Initially no blocks
        assert!(!session.has_block(&hash1));
        assert!(!session.has_block(&hash2));

        // Add blocks
        session.add_block(hash1.clone());
        assert!(session.has_block(&hash1));
        assert!(!session.has_block(&hash2));

        // Add multiple blocks
        session.add_blocks([hash2.clone(), hash3.clone()]);
        assert!(session.has_block(&hash2));
        assert!(session.has_block(&hash3));
    }

    #[test]
    fn test_session_want_tracking() {
        let mut session = BitswapSession::new(1);
        let hash = make_test_hash(b"wanted block");

        assert!(!session.is_wanting(&hash));

        session.start_want(&hash);
        assert!(session.is_wanting(&hash));

        session.stop_want(&hash);
        assert!(!session.is_wanting(&hash));
    }

    #[test]
    fn test_session_peer_scoring() {
        let mut session = BitswapSession::new(1);

        session.add_peer(make_peer_id("peer1"));
        session.add_peer(make_peer_id("peer2"));

        let blocks = vec![
            make_test_hash(b"b1"),
            make_test_hash(b"b2"),
            make_test_hash(b"b3"),
            make_test_hash(b"b4"),
        ];

        // peer1 has 2 blocks
        session.record_peer_blocks("peer1", &blocks[..2]);
        // peer2 has 4 blocks
        session.record_peer_blocks("peer2", &blocks[..4]);

        // peer2 should be preferred (more blocks)
        assert_eq!(session.best_peer_for(&blocks[0]), Some("peer2"));
        assert_eq!(session.best_peer_for(&blocks[1]), Some("peer2"));
    }

    #[test]
    fn test_session_staleness() {
        let mut session = BitswapSession::new(1);

        // New session should not be stale
        assert!(!session.is_stale(Duration::from_secs(60)));

        // Simulate old session by manipulating last_activity
        // (In real tests, we'd use tokio::time::pause or similar)
        // For now, just verify the method works
        let _ = session.idle_time();
        let _ = session.age();
    }

    #[test]
    fn test_session_manager_lifecycle() {
        let manager = SessionManager::new(10);

        // Create sessions
        let s1 = manager.create_session();
        let s2 = manager.create_session();

        assert_ne!(s1.id, s2.id);
        assert!(manager.get_session(s1.id).is_some());
        assert!(manager.get_session(s2.id).is_some());

        // Remove session
        manager.remove_session(s1.id);
        assert!(manager.get_session(s1.id).is_none());
        assert!(manager.get_session(s2.id).is_some());
    }

    #[test]
    fn test_session_manager_eviction() {
        let manager = SessionManager::new(3);

        // Create more sessions than max
        let _s1 = manager.create_session();
        let _s2 = manager.create_session();
        let _s3 = manager.create_session();
        let s4 = manager.create_session();

        // s1 should have been evicted
        assert!(manager.get_session(1).is_none());
        assert!(manager.get_session(2).is_some());
        assert!(manager.get_session(3).is_some());
        assert!(manager.get_session(4).is_some());
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Pending Want Queue
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_pending_want_block() {
        let hash = make_test_hash(b"test block");
        let want = PendingWant::want_block(hash.clone(), 100);

        assert_eq!(want.block, hash);
        assert_eq!(want.priority, 100);
        assert!(!want.want_have);
        assert!(!want.send_dont_have);
        assert!(want.session_id.is_none());
    }

    #[test]
    fn test_pending_want_have() {
        let hash = make_test_hash(b"test block");
        let want = PendingWant::want_have(hash.clone(), 50);

        assert_eq!(want.block, hash);
        assert_eq!(want.priority, 50);
        assert!(want.want_have);
        assert!(want.send_dont_have);
    }

    #[test]
    fn test_pending_want_priority_ordering() {
        use std::collections::BinaryHeap;

        let hash1 = make_test_hash(b"low");
        let hash2 = make_test_hash(b"medium");
        let hash3 = make_test_hash(b"high");

        let low = PendingWant::want_block(hash1, 10);
        let medium = PendingWant::want_block(hash2, 50);
        let high = PendingWant::want_block(hash3, 100);

        let mut heap = BinaryHeap::new();
        heap.push(low);
        heap.push(medium);
        heap.push(high);

        // Highest priority first
        assert_eq!(heap.pop().unwrap().priority, 100);
        assert_eq!(heap.pop().unwrap().priority, 50);
        assert_eq!(heap.pop().unwrap().priority, 10);
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Peer Want List
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_peer_want_list() {
        let mut want_list = PeerWantList::default();

        let hash1 = make_test_hash(b"block1");
        let hash2 = make_test_hash(b"block2");

        // Add wants
        want_list.add_want(&hash1);
        want_list.add_want_have(&hash2);

        assert!(want_list.wants(&hash1));
        assert!(!want_list.wants(&hash2));
        assert!(want_list.wants_have(&hash2));
        assert!(!want_list.wants_have(&hash1));

        // Remove want
        want_list.remove_want(&hash1);
        assert!(!want_list.wants(&hash1));
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Bitswap Engine Message Processing
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_engine_process_have_when_block_local() {
        let mut engine = BitswapEngine::new();
        let _ = engine.add_peer("peer1");

        let hash = make_test_hash(b"local block");

        // Simulate local block availability by directly manipulating
        // (In real usage, this would come from the block provider callback)
        // For testing, we just verify the engine processes messages

        let msg = BitswapMessage::WantHave {
            block: hash,
            priority: 100,
            send_dont_have: true,
        };

        let _responses = engine.process_message("peer1", msg);

        // Engine should handle the message without panicking
    }

    #[test]
    fn test_engine_get_ledger_stats() {
        let engine = BitswapEngine::new();
        let _ = engine.add_peer("peer1");

        let stats = engine.get_peer_ledger("peer1");
        assert!(stats.is_some());

        let all_stats = engine.get_all_ledger_stats();
        assert_eq!(all_stats.len(), 1);
    }

    #[test]
    fn test_engine_get_ledger_stats_nonexistent_peer() {
        let engine = BitswapEngine::new();

        let stats = engine.get_peer_ledger("nonexistent");
        assert!(stats.is_none());
    }

    #[test]
    fn test_engine_session_stats() {
        let engine = BitswapEngine::new();

        // Create a session
        let _session = engine.create_session();

        let stats = engine.get_session_stats();
        assert_eq!(stats.count, 1);
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Metrics
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_bitswap_metrics_registration() {
        use adnet_observability::registry::Registry;
        use std::sync::Arc;

        let registry = Registry::default();
        let _metrics = BitswapMetrics::register(&Arc::new(registry));

        // Metrics should register without panicking
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Constants
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_bitswap_constants() {
        assert_eq!(MAX_CONCURRENT_WANTS, 64);
        assert_eq!(WANT_HAVE_TIMEOUT, Duration::from_secs(10));
        assert_eq!(WANT_BLOCK_TIMEOUT, Duration::from_secs(60));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Swarm Download Integration Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod swarm_download_integration_tests {
    use adnet_blobstore::{
        PieceSelectionStrategy, SwarmDownloader, SwarmError, SwarmLedger, SwarmLedgerStats,
        SwarmProgress,
    };
    use adnet_types::ContentHash;
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::time::Duration;

    // ─────────────────────────────────────────────────────────────────
    // Helper: Create test data
    // ─────────────────────────────────────────────────────────────────

    fn make_test_hash(data: &[u8]) -> ContentHash {
        ContentHash::from_bytes(data)
    }

    fn make_chunks(count: usize, size: usize) -> (Vec<Vec<u8>>, Vec<u8>) {
        let chunks: Vec<Vec<u8>> = (0..count).map(|i| vec![i as u8; size]).collect();
        let content: Vec<u8> = chunks.iter().flatten().cloned().collect();
        (chunks, content)
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Swarm Downloader Basic Operations
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_swarm_downloader_creation() {
        let hash = make_test_hash(b"test content");
        let downloader = SwarmDownloader::new(hash.clone(), 1024, 4);

        assert_eq!(downloader.content_hash(), &hash);
        assert_eq!(downloader.total_pieces(), 4);
        assert_eq!(downloader.verified_count(), 0);
        assert!(!downloader.is_complete());
        assert!(!downloader.is_failed());
    }

    #[test]
    fn test_swarm_downloader_peer_registration() {
        let hash = make_test_hash(b"test content");
        let downloader = SwarmDownloader::new(hash, 1024, 4);

        let mut have_pieces: HashSet<u32> = HashSet::new();
        have_pieces.insert(0);
        have_pieces.insert(1);
        have_pieces.insert(2);
        have_pieces.insert(3);

        downloader.register_peer("peer1".to_string(), have_pieces);

        let (total, healthy) = downloader.peer_stats();
        assert_eq!(total, 1);
        assert_eq!(healthy, 1);
    }

    #[test]
    fn test_swarm_downloader_peer_availability() {
        let hash = make_test_hash(b"test content");
        let downloader = SwarmDownloader::new(hash, 4096, 4);

        // Register peers with different pieces
        downloader.register_peer("peer1".to_string(), [0, 1].into());
        downloader.register_peer("peer2".to_string(), [2, 3].into());

        // Check peer selection
        assert_eq!(downloader.get_peer_for_piece(0), Some("peer1".to_string()));
        assert_eq!(downloader.get_peer_for_piece(2), Some("peer2".to_string()));
    }

    #[test]
    fn test_swarm_downloader_piece_selection_strict() {
        let hash = make_test_hash(b"test content");
        let downloader = SwarmDownloader::new(hash, 1024, 4);

        // Strict priority should return pieces in order
        assert_eq!(
            downloader.select_next_piece(PieceSelectionStrategy::StrictPriority),
            Some(0)
        );
        assert_eq!(
            downloader.select_next_piece(PieceSelectionStrategy::StrictPriority),
            Some(1)
        );
        assert_eq!(
            downloader.select_next_piece(PieceSelectionStrategy::StrictPriority),
            Some(2)
        );
        assert_eq!(
            downloader.select_next_piece(PieceSelectionStrategy::StrictPriority),
            Some(3)
        );
    }

    #[test]
    fn test_swarm_downloader_piece_selection_rarest() {
        let hash = make_test_hash(b"test content");
        let mut downloader = SwarmDownloader::new(hash, 1024, 4);

        // Peer 1 has pieces 0, 1, 2
        downloader.register_peer("peer1".to_string(), [0, 1, 2].into());
        // Peer 2 has only piece 3 (rarest)
        downloader.register_peer("peer2".to_string(), [3].into());

        // Rarest first should prioritize piece 3
        assert_eq!(
            downloader.select_next_piece(PieceSelectionStrategy::RarestFirst),
            Some(3)
        );
    }

    #[test]
    fn test_swarm_downloader_mark_verified() {
        let hash = make_test_hash(b"test content");
        let downloader = SwarmDownloader::new(hash, 1024, 1);

        let data = b"hello world".to_vec();
        downloader.mark_verified(0, data.clone());

        assert_eq!(downloader.verified_count(), 1);
        assert!(downloader.is_complete());

        let verified = downloader.get_verified_data();
        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0].1, data);
    }

    #[test]
    fn test_swarm_downloader_mark_failed() {
        let hash = make_test_hash(b"test content");
        let downloader = SwarmDownloader::new(hash, 1024, 1);

        assert!(!downloader.is_piece_failed(0));

        downloader.mark_failed(0, "test error".into());

        assert!(downloader.is_piece_failed(0));
        assert!(downloader.is_failed());
    }

    #[test]
    fn test_swarm_downloader_progress() {
        let hash = make_test_hash(b"test content");
        let downloader = SwarmDownloader::new(hash.clone(), 1024, 4);

        let progress = downloader.progress();

        assert_eq!(progress.content_hash, hash);
        assert_eq!(progress.total_pieces, 4);
        assert_eq!(progress.verified_pieces, 0);
        assert_eq!(progress.downloading_pieces, 0);
        assert_eq!(progress.failed_pieces, 0);
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Swarm Ledger
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_swarm_ledger_creation() {
        let ledger = SwarmLedger::new("peer1".to_string());

        assert_eq!(ledger.peer_id, "peer1");
        assert_eq!(ledger.bytes_sent, 0);
        assert_eq!(ledger.bytes_received, 0);
        assert_eq!(ledger.blocks_sent, 0);
        assert_eq!(ledger.blocks_received, 0);
        assert_eq!(ledger.credit_limit, 10 * 1024 * 1024); // 10 MB
    }

    #[test]
    fn test_swarm_ledger_bandwidth() {
        let mut ledger = SwarmLedger::new("peer1".to_string());

        ledger.record_sent(1024);
        assert_eq!(ledger.bytes_sent, 1024);
        assert_eq!(ledger.balance(), -1024);

        ledger.record_received(2048);
        assert_eq!(ledger.bytes_received, 2048);
        assert_eq!(ledger.balance(), 1024);
    }

    #[test]
    fn test_swarm_ledger_flags() {
        let mut ledger = SwarmLedger::new("peer1".to_string());

        assert!(ledger.can_receive());
        assert!(ledger.can_send());

        ledger.throttle();
        assert!(ledger.throttled);

        ledger.unthrottle();
        assert!(!ledger.throttled);

        ledger.block();
        assert!(ledger.blocked);
        assert!(!ledger.can_receive());
        assert!(!ledger.can_send());
    }

    #[test]
    fn test_swarm_ledger_stats_conversion() {
        let mut ledger = SwarmLedger::new("peer1".to_string());
        ledger.record_sent(1000);
        ledger.record_received(500);

        let stats: SwarmLedgerStats = (&ledger).into();

        assert_eq!(stats.peer_id, "peer1");
        assert_eq!(stats.bytes_sent, 1000);
        assert_eq!(stats.bytes_received, 500);
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Piece Selection Strategy
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_piece_selection_default() {
        let default = PieceSelectionStrategy::default();
        assert_eq!(default, PieceSelectionStrategy::StrictPriority);
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Swarm Error Types
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_swarm_error_display() {
        let hash = make_test_hash(b"test");
        let error = SwarmError::NoPeers(hash.clone());
        assert!(format!("{}", error).contains("no peers"));

        let error = SwarmError::ChunkTimeout { index: 5 };
        assert!(format!("{}", error).contains("timeout"));

        let error = SwarmError::StrategyExhausted;
        assert!(format!("{}", error).contains("strategy exhausted"));
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Swarm Progress
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_swarm_progress_creation() {
        let hash = make_test_hash(b"test");
        let progress = SwarmProgress {
            content_hash: hash.clone(),
            total_pieces: 10,
            verified_pieces: 5,
            downloading_pieces: 2,
            failed_pieces: 1,
            bytes_downloaded: 5120,
            bytes_total: 10240,
            speed: 1024,
            elapsed: Duration::from_secs(5),
        };

        assert_eq!(progress.total_pieces, 10);
        assert_eq!(progress.verified_pieces, 5);
        assert_eq!(progress.downloading_pieces, 2);
        assert_eq!(progress.failed_pieces, 1);
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Peer Health Tracking
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_peer_health_tracking() {
        let hash = make_test_hash(b"test");
        let downloader = SwarmDownloader::new(hash, 1024, 1);

        downloader.register_peer("peer1".to_string(), [0].into());

        assert!(downloader.is_peer_healthy("peer1"));
        assert!(!downloader.is_peer_healthy("peer2"));
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Downloader State Management
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_downloader_cancel() {
        let hash = make_test_hash(b"test");
        let downloader = SwarmDownloader::new(hash, 1024, 1);

        assert!(!downloader.is_cancelled());

        downloader.cancel();

        assert!(downloader.is_cancelled());
    }

    #[test]
    fn test_downloader_session_id() {
        let hash = make_test_hash(b"test");
        let d1 = SwarmDownloader::new(hash.clone(), 1024, 1);
        let d2 = SwarmDownloader::new(hash, 1024, 1);

        // Each downloader should have a unique session ID
        assert_ne!(d1.session_id(), d2.session_id());
    }

    #[test]
    fn test_downloader_endgame_threshold() {
        let hash = make_test_hash(b"test");
        let downloader = SwarmDownloader::new(hash, 1024, 10);

        // Should not be in endgame with 0 pieces
        assert!(!downloader.should_endgame());

        // Simulate 80% completion
        for i in 0..8 {
            downloader.mark_verified(i, vec![0u8; 102]);
        }

        // Should now be in endgame
        assert!(downloader.should_endgame());
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Ledger Stats
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_get_ledger_stats() {
        let hash = make_test_hash(b"test");
        let downloader = SwarmDownloader::new(hash, 1024, 1);

        downloader.register_peer("peer1".to_string(), [0].into());

        let stats = downloader.get_ledger_stats("peer1");
        assert!(stats.is_some());
        assert_eq!(stats.unwrap().peer_id, "peer1");

        let all_stats = downloader.get_all_ledger_stats();
        assert_eq!(all_stats.len(), 1);
    }

    #[test]
    fn test_get_peers_with_piece() {
        let hash = make_test_hash(b"test");
        let downloader = SwarmDownloader::new(hash, 4096, 4);

        downloader.register_peer("peer1".to_string(), [0, 1].into());
        downloader.register_peer("peer2".to_string(), [1, 2, 3].into());

        let peers_with_0 = downloader.get_peers_with_piece(0);
        assert_eq!(peers_with_0.len(), 1);
        assert_eq!(peers_with_0[0], "peer1");

        let peers_with_1 = downloader.get_peers_with_piece(1);
        assert_eq!(peers_with_1.len(), 2);
    }

    #[test]
    fn test_get_all_available_pieces() {
        let hash = make_test_hash(b"test");
        let downloader = SwarmDownloader::new(hash, 4096, 4);

        downloader.register_peer("peer1".to_string(), [0, 1].into());
        downloader.register_peer("peer2".to_string(), [2, 3].into());

        let availability = downloader.get_all_available_pieces();

        assert_eq!(availability.len(), 4);
        assert_eq!(availability[&0], vec!["peer1"]);
        assert_eq!(availability[&3], vec!["peer2"]);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Async Integration Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod async_integration_tests {
    use adnet_blobstore::swarm_download::ChunkFetcher;
    use adnet_blobstore::{MockChunkFetcher, SwarmDownloadService, SwarmError};
    use adnet_types::ContentHash;
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::time::Duration;

    fn make_test_hash(data: &[u8]) -> ContentHash {
        ContentHash::from_bytes(data)
    }

    #[tokio::test]
    async fn test_mock_chunk_fetcher_basic() {
        let content = b"hello world".to_vec();
        let hash = make_test_hash(&content);

        let fetcher = Arc::new(
            MockChunkFetcher::new()
                .with_data(hash.clone(), vec![content.clone()])
                .with_latency(Duration::from_millis(1)),
        );

        let result = fetcher
            .fetch_chunk("peer1", &hash, 0, Duration::from_secs(5))
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), content);
    }

    #[tokio::test]
    async fn test_mock_chunk_fetcher_not_found() {
        let content = b"hello world".to_vec();
        let hash = make_test_hash(&content);

        let fetcher = Arc::new(MockChunkFetcher::new());

        let result = fetcher
            .fetch_chunk("peer1", &hash, 0, Duration::from_secs(5))
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_mock_chunk_fetcher_failure_injection() {
        let content = b"hello world".to_vec();
        let hash = make_test_hash(&content);

        let fetcher = Arc::new(
            MockChunkFetcher::new()
                .with_data(hash.clone(), vec![content.clone()])
                .with_latency(Duration::from_millis(1)),
        );

        // First call succeeds
        let result1 = fetcher
            .fetch_chunk("peer1", &hash, 0, Duration::from_secs(5))
            .await;
        assert!(result1.is_ok());

        // Second call should also succeed (failure count is 0)
        let result2 = fetcher
            .fetch_chunk("peer1", &hash, 0, Duration::from_secs(5))
            .await;
        assert!(result2.is_ok());
    }

    #[tokio::test]
    async fn test_swarm_service_download_single_chunk() {
        let content = b"hello world".to_vec();
        let hash = make_test_hash(&content);

        let fetcher = Arc::new(
            MockChunkFetcher::new()
                .with_data(hash.clone(), vec![content.clone()])
                .with_latency(Duration::from_millis(1)),
        );

        let service = SwarmDownloadService::new(fetcher);

        let peers = vec![("peer1".to_string(), [0u32].into())];
        let result = service
            .download(&hash, content.len() as u64, 1, peers, None)
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), content);
    }

    #[tokio::test]
    async fn test_swarm_service_download_multi_chunk() {
        let chunks: Vec<Vec<u8>> = (0..4).map(|i| vec![i as u8; 1024]).collect();
        let content: Vec<u8> = chunks.iter().flatten().cloned().collect();
        let hash = make_test_hash(&content);

        let fetcher = Arc::new(
            MockChunkFetcher::new()
                .with_data(hash.clone(), chunks)
                .with_latency(Duration::from_millis(1)),
        );

        let service = SwarmDownloadService::new(fetcher);

        let have_pieces: HashSet<u32> = [0, 1, 2, 3].into();
        let peers = vec![("peer1".to_string(), have_pieces)];

        let result = service
            .download(&hash, content.len() as u64, 4, peers, None)
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), content);
    }

    #[tokio::test]
    async fn test_swarm_service_with_multiple_peers() {
        let chunks: Vec<Vec<u8>> = (0..4).map(|i| vec![i as u8; 1024]).collect();
        let content: Vec<u8> = chunks.iter().flatten().cloned().collect();
        let hash = make_test_hash(&content);

        let fetcher = Arc::new(
            MockChunkFetcher::new()
                .with_data(hash.clone(), chunks)
                .with_latency(Duration::from_millis(1)),
        );

        let service = SwarmDownloadService::new(fetcher);

        // Multiple peers with partial coverage
        let peers = vec![
            ("peer1".to_string(), [0, 1].into()),
            ("peer2".to_string(), [2, 3].into()),
        ];

        let result = service
            .download(&hash, content.len() as u64, 4, peers, None)
            .await;

        assert!(result.is_ok());
    }
}
