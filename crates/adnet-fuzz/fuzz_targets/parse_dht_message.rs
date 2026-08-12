// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Fuzz target for DHT wire protocol messages.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Try various DHT message formats

    // 1. Try as protobuf (common wire format)
    if let Ok(msg) = protobuf::Message::parse_from_bytes::<adnet_dht::DhtWireMessage>(data) {
        // Validate message structure
        match msg.get_field_type() {
            0 => {} // Unknown, valid
            1 => { // Ping
                let _ = msg.get_ping();
            }
            2 => { // Pong
                let _ = msg.get_pong();
            }
            3 => { // FindNode
                let _ = msg.get_find_node();
            }
            4 => { // Nodes
                let _ = msg.get_nodes();
            }
            5 => { // GetProviders
                let _ = msg.get_get_providers();
            }
            6 => { // Providers
                let _ = msg.get_providers();
            }
            7 => { // AddProvider
                let _ = msg.get_add_provider();
            }
            8 => { // GetValue
                let _ = msg.get_get_value();
            }
            9 => { // Value
                let _ = msg.get_value();
            }
            10 => { // PutValue
                let _ = msg.get_put_value();
            }
            _ => {}
        }

        // Try to serialize back
        let mut bytes = Vec::new();
        let _ = msg.write_to_vec(&mut bytes);
    }

    // 2. Try as postcard binary format
    if let Ok(msg) = postcard::from_bytes::<adnet_dht::DhtMessage>(data) {
        // Validate message
        match msg {
            adnet_dht::DhtMessage::Ping => {}
            adnet_dht::DhtMessage::Pong => {}
            adnet_dht::DhtMessage::FindNode { target, .. } => {
                let _ = target.as_bytes();
            }
            adnet_dht::DhtMessage::Nodes { closer, .. } => {
                assert!(closer.len() < 1000); // Reasonable bound
            }
            adnet_dht::DhtMessage::GetProviders { cid, .. } => {
                let _ = cid.as_bytes();
            }
            adnet_dht::DhtMessage::Providers { records, .. } => {
                assert!(records.len() < 1000);
            }
            adnet_dht::DhtMessage::AddProvider { record, .. } => {
                let _ = record;
            }
            adnet_dht::DhtMessage::GetValue { key, .. } => {
                let _ = key.as_bytes();
            }
            adnet_dht::DhtMessage::PutValue { key, value, .. } => {
                let _ = (key.as_bytes(), value.as_bytes());
            }
        }

        // Try serialization roundtrip
        let encoded = postcard::to_allocvec(&msg);
        if let Ok(encoded) = encoded {
            let _: adnet_dht::DhtMessage = postcard::from_bytes(&encoded).unwrap();
        }
    }

    // 3. Try K-bucket operations with random IDs
    if data.len() >= 64 {
        let local_id = adnet_types::NodeId::random();
        let mut table = adnet_dht::RoutingTable::new(local_id.clone());

        // Add contacts with random IDs derived from data
        for i in 0..8.min(data.len() / 8) {
            let start = i * 8;
            let end = start + 8.min(data.len() - start);
            if end > start {
                let mut bytes = [0u8; 32];
                bytes[..end-start].copy_from_slice(&data[start..end]);
                if let Ok(node_id) = adnet_types::NodeId::from_bytes(&bytes) {
                    let contact = adnet_dht::Contact::new(
                        node_id,
                        "127.0.0.1:8080".parse().unwrap(),
                    );
                    let _ = table.insert(contact);
                }
            }
        }

        // Try queries on the table
        let target = adnet_types::NodeId::random();
        let _ = table.get_closest(&target, 10);
        let _ = table.size();
    }
});
