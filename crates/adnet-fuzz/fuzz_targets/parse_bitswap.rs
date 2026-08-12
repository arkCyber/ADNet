// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Fuzz target for Bitswap protocol messages.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Try to parse Bitswap protocol messages

    // 1. Try protobuf format
    if let Ok(msg) = adnet_types::pb::Message::parse_from_bytes(data) {
        // Validate message structure
        match msg.r#type() {
            adnet_types::pb::message::Type::Want => {
                if let Some(wantlist) = msg.wantlist {
                    assert!(wantlist.entries.len() < 10000); // Reasonable bound

                    for entry in &wantlist.entries {
                        assert!(!entry.block.is_empty() || entry.cancel || entry.have);
                    }
                }
            }
            adnet_types::pb::message::Type::Block => {
                // Block message - check size bounds
                assert!(msg.payload.len() < 100);
                for block in &msg.payload {
                    assert!(block.data.len() < 100_000_000); // Max 100MB per block
                }
            }
            adnet_types::pb::message::Type::DontHave => {
                // Just need to parse successfully
            }
            _ => {}
        }
    }

    // 2. Try BitswapCodec for our custom format
    if let Ok(msg) = adnet_blobstore::BitswapCodec::decode(data) {
        match msg {
            adnet_blobstore::BitswapMessage::WantHave { block, priority, .. } => {
                // Validate entry fields - block is a ContentHash
                assert!(!block.as_bytes().is_empty() || priority == 0);
            }
            adnet_blobstore::BitswapMessage::WantBlock { block, priority } => {
                // Validate entry fields
                assert!(!block.as_bytes().is_empty() || priority == 0);
            }
            adnet_blobstore::BitswapMessage::Have { block, .. } => {
                // Just need to parse successfully
                assert!(!block.as_bytes().is_empty());
            }
            adnet_blobstore::BitswapMessage::DontHave { block } => {
                assert!(!block.as_bytes().is_empty());
            }
            adnet_blobstore::BitswapMessage::Block { block, data } => {
                assert!(!block.as_bytes().is_empty());
                assert!(data.len() < 1_000_000); // Max 1MB per block in fuzzing
            }
            adnet_blobstore::BitswapMessage::Cancel { block } => {
                assert!(!block.as_bytes().is_empty());
            }
            adnet_blobstore::BitswapMessage::BatchWant { wants } => {
                assert!(wants.len() < 1000);
                for want in &wants {
                    assert!(!want.block.as_bytes().is_empty());
                }
            }
            adnet_blobstore::BitswapMessage::BatchResponse { responses } => {
                assert!(responses.len() < 1000);
            }
        }

        // Try re-encoding
        let encoded = adnet_blobstore::BitswapCodec::encode(&msg);
        assert!(encoded.is_ok());
    }

    // 3. Test wantlist operations
    if data.len() >= 8 {
        let wantlist = adnet_blobstore::WantlistManager::new();
        let _ = wantlist.stats();

        // Add some want entries
        for i in 0..data.len().min(100) {
            let cid = adnet_types::Cid::default();
            let entry = adnet_blobstore::WantEntry {
                cid: cid.to_string(),
                priority: (data[i] as i32).abs(),
                want_type: if data[i] % 2 == 0 {
                    adnet_blobstore::WantType::Have
                } else {
                    adnet_blobstore::WantType::Block
                },
                send_dont_have: true,
                cancel: false,
            };
            wantlist.add(entry);
        }
    }
});
