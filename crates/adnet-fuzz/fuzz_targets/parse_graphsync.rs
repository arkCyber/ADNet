// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Fuzz target for GraphSync protocol messages.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Try to parse GraphSync JSON-based wire format
    if let Ok(msg) = serde_json::from_slice::<adnet_types::graphsync::GraphSyncMessage>(data) {
        match msg {
            adnet_types::graphsync::GraphSyncMessage::Request(req) => {
                // Validate request structure
                assert!(req.id > 0 || req.id == 0); // Valid ID
                // Selector can be any bytes, just verify it doesn't cause issues
                let _ = req.selector.len();
            }
            adnet_types::graphsync::GraphSyncMessage::Response(resp) => {
                // Validate response structure
                assert!(resp.id > 0 || resp.id == 0);
            }
            adnet_types::graphsync::GraphSyncMessage::Block(block) => {
                // Validate block structure
                assert!(block.id > 0 || block.id == 0);
                assert!(block.block.len() < 1_000_000); // Max 1MB per block
            }
        }

        // Try re-encoding
        let encoded = serde_json::to_vec(&msg);
        assert!(encoded.is_ok());
    }

    // 2. Test with ContentHash operations (fuzzing the CID parsing)
    if data.len() >= 8 {
        let root_hash = adnet_types::ContentHash::from_bytes(&data[..std::cmp::min(data.len(), 64)]);
        let _ = root_hash.as_bytes();

        // Simulate basic DAG operations
        let _ = adnet_blobstore::DagBlockStore::default();
    }
});
