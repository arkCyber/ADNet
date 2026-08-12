// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Fuzz target for Announcement parsing.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Try to parse as JSON
    if let Ok(announcement) = serde_json::from_slice::<adnet_types::Announcement>(data) {
        // Validate the parsed announcement
        assert!(!announcement.room_id.as_str().is_empty());
        assert!(!announcement.title.is_empty());

        // Try to serialize back
        let json = serde_json::to_string(&announcement);
        assert!(json.is_ok());

        // Try binary serialization
        let bin = postcard::to_allocvec(&announcement);
        assert!(bin.is_ok());

        // Try to deserialize from binary
        if let Ok(bin_data) = bin {
            let _: adnet_types::Announcement = postcard::from_bytes(&bin_data).unwrap();
        }
    }

    // Try to parse as CBOR (common alternative format)
    let _ = serde_cbor::from_slice::<adnet_types::Announcement>(data);

    // Try to parse as MessagePack
    let _ = rmp_serde::from_slice::<adnet_types::Announcement>(data);
});
