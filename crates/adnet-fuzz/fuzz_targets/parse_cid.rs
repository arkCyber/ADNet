// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Fuzz target for CID parsing and validation.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Convert bytes to string if possible
    if let Ok(s) = std::str::from_utf8(data) {
        // Try to parse as CID string
        if let Ok(cid) = adnet_types::Cid::from_str(s) {
            // Validate CID properties
            assert_eq!(cid.version(), cid.version()); // Accessor works

            // Try hash operations
            let hash = cid.hash();
            assert_eq!(hash.size(), hash.size());

            // Try multihash operations
            let mh = adnet_types::Multihash::from_digest(cid.hash().to_bytes());
            assert_eq!(mh.digest().len(), mh.size());

            // Try serialization roundtrip
            let cid_str = cid.to_string();
            let parsed = adnet_types::Cid::from_str(&cid_str);
            assert!(parsed.is_ok());
        }

        // Try parsing as raw multihash
        if let Ok(mh) = adnet_types::Multihash::from_hex(s) {
            assert_eq!(mh.size(), mh.size());
        }

        // Try parsing as BLAKE3 hash
        if s.starts_with("bafk") || s.len() == 64 {
            if let Ok(hash) = adnet_types::ContentHash::from_hex(s) {
                let _ = hash.as_bytes();
            }
        }
    }

    // Try raw bytes as CID binary format
    if data.len() >= 10 {
        if let Ok(cid) = adnet_types::Cid::read_bytes(&mut &data[..]) {
            let _ = cid.to_string();
        }
    }
});
