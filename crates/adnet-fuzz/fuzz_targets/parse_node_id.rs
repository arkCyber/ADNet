// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Fuzz target for NodeId parsing.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Try to create NodeId from bytes
    if data.len() >= 32 {
        if let Ok(node_id) = adnet_types::NodeId::from_bytes(data) {
            // Validate NodeId
            let bytes = node_id.as_bytes();
            assert_eq!(bytes.len(), 32);

            // Try to get peer ID bytes
            let peer_id = node_id.to_peer_id_bytes();
            assert!(!peer_id.is_empty());

            // Try XOR distance with self
            let dist = node_id.xor_distance(&node_id);
            assert!(dist.is_zero());

            // Try XOR distance with another node
            let other = adnet_types::NodeId::random();
            let dist = node_id.xor_distance(&other);
            assert!(!dist.is_zero());

            // Try kbucket distance calculation
            let dist = node_id.kbucket_distance(&other);
            assert!(dist > 0);
        }
    }

    // Try to parse from hex string
    if let Ok(s) = std::str::from_utf8(data) {
        if let Ok(node_id) = adnet_types::NodeId::from_hex(s) {
            let _ = node_id.to_hex();
        }
    }

    // Try key operations
    if data.len() >= 32 {
        let secret = ed25519_dalek::SigningKey::from_bytes(data.try_into().unwrap_or([0u8; 32]));
        let public = secret.verifying_key();
        let bytes = public.as_bytes();

        if let Ok(node_id) = adnet_types::NodeId::from_bytes(bytes) {
            // Verify signature roundtrip
            let message = b"test message";
            let signature = secret.sign(message);

            // Should verify correctly
            let result = public.verify(message, &signature);
            assert!(result.is_ok());
        }
    }
});
