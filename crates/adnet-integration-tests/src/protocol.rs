// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Protocol-specific tests (Bitswap, GraphSync, IPNS).

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{init_tracing};

    // ────────────────────────────────────────────────────────────────────
    // Bitswap Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_bitswap_wantlist() {
        init_tracing();

        // This would test Bitswap protocol operations
        // Requires Bitswap feature to be enabled
        let _ = std::any::type_name::<adnet_blobstore::BitswapEngine>();
    }

    // ────────────────────────────────────────────────────────────────────
    // GraphSync Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_graphsync_requester() {
        init_tracing();

        // Test GraphSync requester functionality
        let _ = std::any::type_name::<adnet_blobstore::GraphSyncRequester>();
    }

    // ────────────────────────────────────────────────────────────────────
    // IPNS Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_ipns_record_creation() {
        init_tracing();

        // Test IPNS record creation and resolution
        // This would use adnet_namespace functionality
    }

    // ────────────────────────────────────────────────────────────────────
    // CAR File Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_car_roundtrip() {
        init_tracing();

        // Create a blob and export to CAR
        let dir = temp_dir();
        let store = adnet_blobstore::BlobStore::new(dir.path())
            .expect("failed to create blobstore");

        // Import a file
        let payload = (0..4096).map(|i| (i % 256) as u8).collect::<Vec<_>>();
        let source = dir.path().join("test.bin");
        std::fs::write(&source, &payload).expect("failed to write test file");

        let (hash, _) = store.import_file_sync(&source).expect("import failed");

        // CAR export would be tested here
        assert!(store.has_complete(&hash));
    }

    // ────────────────────────────────────────────────────────────────────
    // Multi-Protocol Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_bitswap_with_graphsync() {
        init_tracing();

        // Test integration between Bitswap and GraphSync
        // Content discovered via GraphSync can be fetched via Bitswap
    }

    #[tokio::test]
    async fn test_ipns_resolution_flow() {
        init_tracing();

        // Test the full IPNS resolution flow
        // 1. Publish IPNS record
        // 2. Resolve IPNS name
        // 3. Fetch content via DAG
    }
}
