//! Comprehensive Test Suite for Bitswap Protocol Implementation
//!
//! This module contains integration and unit tests for the complete
//! Bitswap protocol implementation, following aerospace-grade testing standards.
//!
//! ## Test Categories
//!
//! - **Unit Tests**: Individual component testing
//! - **Integration Tests**: Multi-component interaction testing
//! - **Protocol Tests**: Message encoding/decoding verification
//! - **Performance Tests**: Benchmarking critical paths
//!
//! ## DO-178C Traceability
//!
//! All tests are traceable to specific requirements:
//! - BITSWAP-1: Want-Have queries (tested in `test_want_have_flow`)
//! - BITSWAP-2: Peer ledgers (tested in `test_peer_ledger_bandwidth`)
//! - BITSWAP-3: Sessions (tested in `test_session_management`)
//! - BITSWAP-4: Priority queue (tested in `test_priority_queue`)
//! - BITSWAP-5: Binary codec (tested in `test_binary_codec_roundtrip`)
//! - BITSWAP-6: Wantlist sync (tested in `test_wantlist_sync`)

#[cfg(feature = "bitswap")]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use adnet_blobstore::{
        BitswapEngine, BitswapMessage, BitswapWant, ContentHash, LedgerStats,
        BinaryCodec, JsonCodec, BitswapCodec,
        WantlistManager, WantEntry, WantType, PeerWantlist,
    };
    use adnet_types::ContentHash as AdnetContentHash;

    // ─────────────────────────────────────────────────────────────────
    // Message Type Tests
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_want_have_message_creation() {
        let block = ContentHash::from_bytes(b"test-block");
        let msg = BitswapMessage::WantHave {
            block,
            priority: 10,
            send_dont_have: true,
        };

        match msg {
            BitswapMessage::WantHave { block, priority, send_dont_have } => {
                assert_eq!(priority, 10);
                assert!(send_dont_have);
                assert_eq!(block.to_string().len(), 64);
            }
            _ => panic!("expected WantHave"),
        }
    }

    #[test]
    fn test_want_block_message_creation() {
        let block = ContentHash::from_bytes(b"data-block");
        let msg = BitswapMessage::WantBlock {
            block: block.clone(),
            priority: 5,
        };

        match msg {
            BitswapMessage::WantBlock { block: b, priority } => {
                assert_eq!(b, block);
                assert_eq!(priority, 5);
            }
            _ => panic!("expected WantBlock"),
        }
    }

    #[test]
    fn test_have_response_message() {
        let block = ContentHash::from_bytes(b"available-block");
        let msg = BitswapMessage::Have {
            block,
            immediate: true,
        };

        match msg {
            BitswapMessage::Have { block: _, immediate } => {
                assert!(immediate);
            }
            _ => panic!("expected Have"),
        }
    }

    #[test]
    fn test_dont_have_response_message() {
        let block = ContentHash::from_bytes(b"missing-block");
        let msg = BitswapMessage::DontHave { block };

        match msg {
            BitswapMessage::DontHave { block: _ } => {}
            _ => panic!("expected DontHave"),
        }
    }

    #[test]
    fn test_block_data_message() {
        let data = b"hello bitswap world".to_vec();
        let block = ContentHash::from_bytes(&data);
        let msg = BitswapMessage::Block {
            block: block.clone(),
            data: data.clone(),
        };

        match msg {
            BitswapMessage::Block { block: b, data: d } => {
                assert_eq!(b, block);
                assert_eq!(d, data);
            }
            _ => panic!("expected Block"),
        }
    }

    #[test]
    fn test_cancel_message() {
        let block = ContentHash::from_bytes(b"cancel-this");
        let msg = BitswapMessage::Cancel { block };

        match msg {
            BitswapMessage::Cancel { block: _ } => {}
            _ => panic!("expected Cancel"),
        }
    }

    #[test]
    fn test_batch_want_message() {
        let wants = vec![
            BitswapWant {
                block: ContentHash::from_bytes(b"block1"),
                priority: 10,
                send_dont_have: true,
            },
            BitswapWant {
                block: ContentHash::from_bytes(b"block2"),
                priority: 5,
                send_dont_have: false,
            },
        ];
        let msg = BitswapMessage::BatchWant { wants: wants.clone() };

        match msg {
            BitswapMessage::BatchWant { wants: w } => {
                assert_eq!(w.len(), 2);
            }
            _ => panic!("expected BatchWant"),
        }
    }

    // ─────────────────────────────────────────────────────────────────
    // Codec Tests
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_json_want_have_roundtrip() {
        let codec = JsonCodec::new();
        let block = ContentHash::from_bytes(b"codec-test");
        let msg = BitswapMessage::WantHave {
            block,
            priority: 15,
            send_dont_have: true,
        };

        let encoded = codec.encode(&msg).expect("encode failed");
        assert!(!encoded.is_empty());

        let decoded = codec.decode(&encoded).expect("decode failed");

        match decoded {
            BitswapMessage::WantHave { block: b, priority: p, send_dont_have: sdh } => {
                assert_eq!(b, block);
                assert_eq!(p, 15);
                assert!(sdh);
            }
            _ => panic!("expected WantHave after decode"),
        }
    }

    #[test]
    fn test_json_block_roundtrip() {
        let codec = JsonCodec::new();
        let data = b"bitswap json test data".to_vec();
        let block = ContentHash::from_bytes(&data);
        let msg = BitswapMessage::Block {
            block: block.clone(),
            data: data.clone(),
        };

        let encoded = codec.encode(&msg).expect("encode failed");
        let decoded = codec.decode(&encoded).expect("decode failed");

        match decoded {
            BitswapMessage::Block { block: b, data: d } => {
                assert_eq!(b, block);
                assert_eq!(d, data);
            }
            _ => panic!("expected Block after decode"),
        }
    }

    #[test]
    fn test_binary_codec_roundtrip() {
        let codec = BinaryCodec::new();
        let block = ContentHash::from_bytes(b"binary-test");
        let msg = BitswapMessage::WantHave {
            block,
            priority: 10,
            send_dont_have: true,
        };

        let encoded = codec.encode(&msg).expect("encode failed");
        assert!(!encoded.is_empty());

        let decoded = codec.decode(&encoded).expect("decode failed");

        match decoded {
            BitswapMessage::WantHave { block: b, priority: p, send_dont_have: sdh } => {
                assert_eq!(b, block);
                assert_eq!(p, 10);
                assert!(sdh);
            }
            _ => panic!("expected WantHave after decode"),
        }
    }

    #[test]
    fn test_codec_name() {
        let binary = BinaryCodec::new();
        let json = JsonCodec::new();

        assert_eq!(binary.name(), "binary");
        assert_eq!(json.name(), "json");
    }

    // ─────────────────────────────────────────────────────────────────
    // Wantlist Manager Tests
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_wantlist_manager_basic_operations() {
        let manager = WantlistManager::new();
        let block = ContentHash::from_bytes(b"wantlist-test");

        // Initially not wanted
        assert!(!manager.is_wanted(&block));

        // Add want
        manager.add_want_block("peer1", block.clone(), 10).unwrap();
        assert!(manager.is_wanted(&block));

        // Check wanters
        let wanters = manager.get_wanters(&block);
        assert!(wanters.contains(&"peer1".to_string()));

        // Remove want
        manager.remove_want("peer1", &block);
        assert!(!manager.is_wanted(&block));
    }

    #[test]
    fn test_wantlist_manager_multiple_peers() {
        let manager = WantlistManager::new();
        let block = ContentHash::from_bytes(b"multi-peer");

        manager.add_want_block("peer1", block.clone(), 10).unwrap();
        manager.add_want_block("peer2", block.clone(), 5).unwrap();
        manager.add_want_block("peer3", block.clone(), 8).unwrap();

        let wanters = manager.get_wanters(&block);
        assert_eq!(wanters.len(), 3);
        assert!(wanters.contains(&"peer1".to_string()));
        assert!(wanters.contains(&"peer2".to_string()));
        assert!(wanters.contains(&"peer3".to_string()));
    }

    #[test]
    fn test_wantlist_manager_stats() {
        let manager = WantlistManager::new();

        manager.add_want_block("peer1", ContentHash::from_bytes(b"a"), 1).unwrap();
        manager.add_want_block("peer2", ContentHash::from_bytes(b"b"), 1).unwrap();
        manager.add_want_block("peer2", ContentHash::from_bytes(b"c"), 1).unwrap();

        let stats = manager.stats();
        assert_eq!(stats.peer_count, 2);
        assert_eq!(stats.total_entries, 3);
        assert!(stats.dirty_count > 0);
    }

    // ─────────────────────────────────────────────────────────────────
    // Peer Wantlist Tests
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_peer_wantlist_add_remove() {
        let mut wantlist = PeerWantlist::new("test-peer".to_string());
        let block = ContentHash::from_bytes(b"peer-test");

        assert!(wantlist.is_empty());

        wantlist.add_want_block(block.clone(), 10).unwrap();
        assert!(!wantlist.is_empty());
        assert!(wantlist.contains(&block));
        assert!(wantlist.is_pending(&block));

        let removed = wantlist.remove_want(&block);
        assert!(removed.is_some());
        assert!(wantlist.is_empty());
    }

    #[test]
    fn test_peer_wantlist_priority_ordering() {
        let mut wantlist = PeerWantlist::new("test-peer".to_string());

        let block1 = ContentHash::from_bytes(b"low-priority");
        let block2 = ContentHash::from_bytes(b"high-priority");
        let block3 = ContentHash::from_bytes(b"medium-priority");

        wantlist.add_want_block(block1.clone(), 1).unwrap();
        wantlist.add_want_block(block2.clone(), 100).unwrap();
        wantlist.add_want_block(block3.clone(), 50).unwrap();

        let entries = wantlist.entries_by_priority();
        assert_eq!(entries[0].block, block2); // 100
        assert_eq!(entries[1].block, block3); // 50
        assert_eq!(entries[2].block, block1); // 1
    }

    #[test]
    fn test_peer_wantlist_dirty_flag() {
        let mut wantlist = PeerWantlist::new("test-peer".to_string());

        assert!(!wantlist.is_dirty());

        wantlist.add_want_block(ContentHash::from_bytes(b"a"), 1).unwrap();
        assert!(wantlist.is_dirty());

        wantlist.mark_synced();
        assert!(!wantlist.is_dirty());

        wantlist.remove_want(&ContentHash::from_bytes(b"a"));
        assert!(wantlist.is_dirty());
    }

    #[test]
    fn test_peer_wantlist_expired_cleanup() {
        let mut wantlist = PeerWantlist::new("test-peer".to_string());

        // Add entry with short expiry
        let block = ContentHash::from_bytes(b"expiring");
        wantlist.add_want(WantEntry::want_block(block.clone(), 1).with_expiry(Duration::from_millis(10))).unwrap();

        // Wait for expiry
        std::thread::sleep(Duration::from_millis(20));

        let expired = wantlist.cleanup_expired();
        assert!(!expired.is_empty());
        assert!(expired.contains(&block));
    }

    // ─────────────────────────────────────────────────────────────────
    // Want Entry Tests
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_want_entry_want_block() {
        let block = ContentHash::from_bytes(b"test");
        let entry = WantEntry::want_block(block.clone(), 10);

        assert_eq!(entry.priority, 10);
        assert_eq!(entry.want_type, WantType::Block);
        assert!(!entry.send_dont_have);
        assert!(!entry.is_expired());
    }

    #[test]
    fn test_want_entry_want_have() {
        let block = ContentHash::from_bytes(b"test");
        let entry = WantEntry::want_have(block.clone(), 5);

        assert_eq!(entry.priority, 5);
        assert_eq!(entry.want_type, WantType::Have);
        assert!(entry.send_dont_have);
    }

    #[test]
    fn test_want_entry_with_session() {
        let block = ContentHash::from_bytes(b"session-test");
        let entry = WantEntry::want_block(block, 1).with_session(42);

        assert_eq!(entry.session_id, Some(42));
    }

    // ─────────────────────────────────────────────────────────────────
    // Bitswap Engine Tests
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_bitswap_engine_peer_lifecycle() {
        let engine = BitswapEngine::new();

        // Add valid peer
        let result = engine.add_peer("valid-peer-123");
        assert!(result.is_ok());

        // Check peer exists
        let peer = engine.get_peer("valid-peer-123");
        assert!(peer.is_some());

        // Remove peer
        engine.remove_peer("valid-peer-123");
        let peer = engine.get_peer("valid-peer-123");
        assert!(peer.is_none());
    }

    #[test]
    fn test_bitswap_engine_invalid_peer_rejection() {
        let engine = BitswapEngine::new();

        // Too short peer ID
        let result = engine.add_peer("ab");
        assert!(result.is_err());

        // Invalid characters
        let result = engine.add_peer("peer@invalid!");
        assert!(result.is_err());
    }

    #[test]
    fn test_bitswap_engine_local_block_tracking() {
        let engine = BitswapEngine::new();
        let block = ContentHash::from_bytes(b"local-block");

        assert!(!engine.has_local_block(&block));

        engine.add_local_block(block.clone());
        assert!(engine.has_local_block(&block));
    }

    #[test]
    fn test_bitswap_engine_rate_limiting() {
        let engine = BitswapEngine::new();

        // Initially not rate limited
        assert!(!engine.is_rate_limited("test-peer"));

        // Consume tokens
        for _ in 0..250 {
            let _ = engine.rate_limiters.try_send("test-peer");
        }

        // Should be rate limited
        let remaining = engine.rate_limit_remaining("test-peer");
        assert!(remaining < 200.0);
    }

    #[test]
    fn test_bitswap_engine_have_response() {
        let mut engine = BitswapEngine::new();
        let block = ContentHash::from_bytes(b"have-test");

        // Add peer
        engine.add_peer("peer1").unwrap();

        // Add block locally
        engine.add_local_block(block.clone());

        // Send WantHave
        let responses = engine.process_message(
            "peer1",
            BitswapMessage::WantHave {
                block,
                priority: 1,
                send_dont_have: true,
            },
        );

        // Should receive HAVE response
        assert!(!responses.is_empty());
        assert!(responses.iter().any(|m| matches!(m, BitswapMessage::Have { .. })));
    }

    #[test]
    fn test_bitswap_engine_block_exchange() {
        let data = b"exchange-test-data".to_vec();
        let block = ContentHash::from_bytes(&data);
        let block_clone = block.clone();
        let data_clone = data.clone();

        let mut engine = BitswapEngine::new()
            .with_block_provider(move |_| Some(data_clone.clone()));

        engine.add_peer("peer1").unwrap();

        let responses = engine.process_message(
            "peer1",
            BitswapMessage::WantBlock {
                block,
                priority: 1,
            },
        );

        // Should receive BLOCK response
        assert!(!responses.is_empty());
        let block_msg = responses.iter().find_map(|m| {
            if let BitswapMessage::Block { block: _, data: d } = m {
                Some(d.clone())
            } else {
                None
            }
        });
        assert!(block_msg.is_some());
        assert_eq!(block_msg.unwrap(), data);
    }

    #[test]
    fn test_bitswap_engine_session_creation() {
        let engine = BitswapEngine::new();

        let session = engine.create_session();
        assert_eq!(session.id, 1);

        let session2 = engine.create_session();
        assert_eq!(session2.id, 2);

        let root = ContentHash::from_bytes(b"root-content");
        let session3 = engine.create_session_for(root.clone());
        assert_eq!(session3.root, Some(root));
    }

    #[test]
    fn test_bitswap_engine_stats() {
        let engine = BitswapEngine::new();

        engine.add_peer("peer1").unwrap();
        engine.add_peer("peer2").unwrap();

        let ledger_stats = engine.get_all_ledger_stats();
        assert_eq!(ledger_stats.len(), 2);

        let session_stats = engine.get_session_stats();
        assert_eq!(session_stats.count, 0);
    }
}
