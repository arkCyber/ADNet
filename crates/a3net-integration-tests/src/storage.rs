// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Storage integration tests.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{init_tracing, temp_dir, wait_for, run_with_timeout};
    use a3net_blobstore::{BlobStore, BlobImporter, BlobReader, CHUNK_SIZE};
    use a3net_types::RangeSpec;

    fn make_payload(size: usize) -> Vec<u8> {
        (0..size).map(|i| (i % 251) as u8).collect()
    }

    // ────────────────────────────────────────────────────────────────────
    // BlobStore Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_blobstore_create() {
        init_tracing();
        let dir = temp_dir();
        let _store = BlobStore::new(dir.path()).expect("failed to create blobstore");
    }

    #[tokio::test]
    async fn test_blobstore_import_small_file() {
        init_tracing();
        let dir = temp_dir();
        let store = BlobStore::new(dir.path()).expect("failed to create blobstore");

        // Create a small test file (under chunk size)
        let payload = make_payload(1024); // 1 KB
        let source = dir.path().join("test.bin");
        std::fs::write(&source, &payload).expect("failed to write test file");

        // Import
        let (hash, size) = store.import_file_sync(&source).expect("import failed");
        assert_eq!(size, 1024);

        // Verify exists
        assert!(store.has_complete(&hash));
    }

    #[tokio::test]
    async fn test_blobstore_import_large_file() {
        init_tracing();
        let dir = temp_dir();
        let store = BlobStore::new(dir.path()).expect("failed to create blobstore");

        // Create a large test file (multiple chunks)
        let payload = make_payload(1 * 1024 * 1024); // 1 MB
        let source = dir.path().join("large.bin");
        std::fs::write(&source, &payload).expect("failed to write test file");

        // Import
        let (hash, size) = store.import_file_sync(&source).expect("import failed");
        assert_eq!(size, 1 * 1024 * 1024);

        // Verify exists
        assert!(store.has_complete(&hash));
    }

    #[tokio::test]
    async fn test_blobstore_read_range() {
        init_tracing();
        let dir = temp_dir();
        let store = BlobStore::new(dir.path()).expect("failed to create blobstore");

        // Create and import a file
        let payload = make_payload(64 * 1024); // 64 KB
        let source = dir.path().join("test.bin");
        std::fs::write(&source, &payload).expect("failed to write test file");

        let (hash, _) = store.import_file_sync(&source).expect("import failed");

        // Read first 4KB
        let range = RangeSpec::single(0, 4096).unwrap();
        let data = BlobReader::read_range(&*store, &hash, range).await.expect("read failed");
        assert_eq!(data.len(), 4096);

        // Verify content
        assert_eq!(&data[..10], &payload[..10]);
    }

    #[tokio::test]
    async fn test_blobstore_parallel_reads() {
        init_tracing();
        let dir = temp_dir();
        let store = BlobStore::new(dir.path()).expect("failed to create blobstore");

        // Create and import a file
        let payload = make_payload(1 * 1024 * 1024); // 1 MB
        let source = dir.path().join("test.bin");
        std::fs::write(&source, &payload).expect("failed to write test file");

        let (hash, _) = store.import_file_sync(&source).expect("import failed");

        // Read in parallel from multiple offsets
        let num_readers = 4;
        let range_size = 16 * 1024; // 16 KB per reader

        let store = Arc::new(store);
        let mut handles = Vec::new();

        for i in 0..num_readers {
            let store = Arc::clone(&store);
            let hash = hash.clone();
            let handle = tokio::spawn(async move {
                let offset = (i * range_size) as u64;
                let range = RangeSpec::single(offset, range_size as u64).unwrap();
                BlobReader::read_range(&*store, &hash, range).await
            });
            handles.push(handle);
        }

        // Collect results
        for handle in handles {
            let result = handle.await.expect("task join failed");
            let data = result.expect("read failed");
            assert_eq!(data.len(), range_size);
        }
    }

    #[tokio::test]
    async fn test_blobstore_complete_check() {
        init_tracing();
        let dir = temp_dir();
        let store = BlobStore::new(dir.path()).expect("failed to create blobstore");

        // Import a file
        let payload = make_payload(32 * 1024); // 32 KB
        let source = dir.path().join("test.bin");
        std::fs::write(&source, &payload).expect("failed to write test file");

        let (hash, _) = store.import_file_sync(&source).expect("import failed");

        // Check completeness
        assert!(store.has_complete(&hash));

        // Non-existent hash should return false
        let fake_hash = a3net_types::ContentHash::from_bytes(b"fake-content-not-existing");
        assert!(!store.has_complete(&fake_hash));
    }

    // ────────────────────────────────────────────────────────────────────
    // Chunk Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_chunk_boundaries() {
        init_tracing();
        let dir = temp_dir();
        let store = BlobStore::new(dir.path()).expect("failed to create blobstore");

        // Test exactly at chunk boundary
        let payload = make_payload(CHUNK_SIZE); // Exactly one chunk
        let source = dir.path().join("chunk1.bin");
        std::fs::write(&source, &payload).expect("failed to write test file");

        let (hash, _) = store.import_file_sync(&source).expect("import failed");
        assert!(store.has_complete(&hash));

        // Test at chunk boundary + 1 byte
        let payload = make_payload(CHUNK_SIZE + 1);
        let source = dir.path().join("chunk2.bin");
        std::fs::write(&source, &payload).expect("failed to write test file");

        let (hash, _) = store.import_file_sync(&source).expect("import failed");
        assert!(store.has_complete(&hash));

        // Test spanning two chunks
        let payload = make_payload(CHUNK_SIZE * 2 - 1);
        let source = dir.path().join("chunk3.bin");
        std::fs::write(&source, &payload).expect("failed to write test file");

        let (hash, _) = store.import_file_sync(&source).expect("import failed");
        assert!(store.has_complete(&hash));
    }

    // ────────────────────────────────────────────────────────────────────
    // GC Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_blobstore_gc() {
        init_tracing();
        let dir = temp_dir();
        let store = BlobStore::new(dir.path()).expect("failed to create blobstore");

        // Import some files
        let mut hashes = Vec::new();
        for i in 0..5 {
            let payload = make_payload(4096 * (i + 1));
            let source = dir.path().join(format!("gc_test_{}.bin", i));
            std::fs::write(&source, &payload).expect("failed to write test file");
            let (hash, _) = store.import_file_sync(&source).expect("import failed");
            hashes.push(hash);
        }

        // All should exist
        for hash in &hashes {
            assert!(store.has_complete(hash));
        }

        // GC should remove nothing (all pinned or complete)
        let removed = store.gc().expect("gc failed");
        assert_eq!(removed, 0);

        // All should still exist
        for hash in &hashes {
            assert!(store.has_complete(hash));
        }
    }

    // ────────────────────────────────────────────────────────────────────
    // Error Handling Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_blobstore_nonexistent_read() {
        init_tracing();
        let dir = temp_dir();
        let store = BlobStore::new(dir.path()).expect("failed to create blobstore");

        let fake_hash = a3net_types::ContentHash::from_bytes(b"nonexistent-content");
        let range = RangeSpec::single(0, 1024).unwrap();

        // Should fail gracefully
        let result = BlobReader::read_range(&*store, &fake_hash, range).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_blobstore_import_nonexistent_file() {
        init_tracing();
        let dir = temp_dir();
        let store = BlobStore::new(dir.path()).expect("failed to create blobstore");

        let nonexistent = dir.path().join("does_not_exist.bin");
        let result = store.import_file_sync(&nonexistent);

        // Should fail
        assert!(result.is_err());
    }

    // ────────────────────────────────────────────────────────────────────
    // Cross-component Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_blobstore_with_gossip() {
        init_tracing();

        // Create blob store
        let dir = temp_dir();
        let store = BlobStore::new(dir.path()).expect("failed to create blobstore");

        // Import a file
        let payload = make_payload(16 * 1024);
        let source = dir.path().join("shared.bin");
        std::fs::write(&source, &payload).expect("failed to write test file");

        let (hash, _) = store.import_file_sync(&source).expect("import failed");
        let hash_hex = hash.to_hex();

        // Create gossip bus
        let transport = Arc::new(a3net_gossip::InProcessGossip::new());
        let node_id = a3net_types::NodeId::random();
        let bus = a3net_gossip::GossipBus::new(node_id, transport);

        let room: a3net_types::RoomId = "content-room".into();
        bus.join_room(&room).await.expect("join failed");

        // Announce the content via gossip
        let ann = a3net_types::Announcement {
            room_id: room.clone(),
            content_hash: hash,
            node_id: a3net_types::NodeId::random(),
            title: format!("Shared Content: {}", &hash_hex[..16]),
            kind: a3net_types::CdnContentKind::Article,
            size_bytes: payload.len() as u64,
            mime_type: Some("application/octet-stream".to_string()),
            source_url: None,
            ticket: None,
            timestamp: chrono::Utc::now(),
            message_id: None,
            ttl_secs: None,
            signer: None,
            signature: None,
        };

        bus.publish(&room, &ann).await.expect("publish failed");

        // Content is in both blob store and announced via gossip
        assert!(store.has_complete(&hash));
    }
}

use std::sync::Arc;
