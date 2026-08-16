// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Unit tests for a3net-dht. This file is intentionally `cfg(test)` only
// so it lives behind the lib test binary and is not part of the
// production compilation surface.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Contact, DhtKey, DhtNode, DhtStorage, IpnRecord, KBUCKET_SIZE, KBucket, ProviderRecord,
        RoutingTable, new_in_memory_store,
    };
    use crate::bucket::InsertError;
    use a3net_types::NodeId;
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
    use std::time::{Duration, Instant};

    fn make_node_id(seed: u8) -> NodeId {
        let bytes = [seed; 32];
        NodeId::from_bytes(&bytes).unwrap()
    }

    fn make_contact(seed: u8) -> Contact {
        let id = make_node_id(seed);
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9000 + seed as u16));
        Contact::new(id, addr)
    }

    // ────────────────────────────────────────────────────────────────────
    // KBucket tests
    // ────────────────────────────────────────────────────────────────────

    #[test]
    fn kbucket_empty_has_no_contacts() {
        let bucket = KBucket::new();
        assert!(bucket.is_empty());
        assert_eq!(bucket.len(), 0);
        assert!(!bucket.is_full());
    }

    #[test]
    fn kbucket_insert_single_contact() {
        let mut bucket = KBucket::new();
        let contact = make_contact(1);

        assert!(bucket.insert(contact.clone()).is_ok());
        assert_eq!(bucket.len(), 1);
        assert!(bucket.contains(&contact.id));
    }

    #[test]
    fn kbucket_insert_until_full_then_rejects() {
        let mut bucket = KBucket::new();

        for i in 0..KBUCKET_SIZE {
            let contact = make_contact(i as u8);
            assert!(bucket.insert(contact).is_ok(), "i={}", i);
        }

        assert!(bucket.is_full());
        assert_eq!(bucket.len(), KBUCKET_SIZE);

        // Bucket is full → next insert must error without panicking.
        let overflow = make_contact(99);
        assert!(bucket.insert(overflow).is_err());
    }

    #[test]
    fn kbucket_mark_seen_updates_last_seen() {
        let mut bucket = KBucket::new();
        let contact = make_contact(1);
        bucket.insert(contact.clone()).unwrap();

        let initial = bucket.find(&contact.id).unwrap().last_seen;
        std::thread::sleep(Duration::from_millis(5));
        bucket.mark_seen(&contact.id);

        let updated = bucket.find(&contact.id).unwrap().last_seen;
        assert!(updated >= initial);
    }

    #[test]
    fn kbucket_remove_drops_contact() {
        let mut bucket = KBucket::new();
        let contact = make_contact(1);
        bucket.insert(contact.clone()).unwrap();

        assert!(bucket.remove(&contact.id));
        assert!(bucket.is_empty());
        // Idempotent on missing id.
        assert!(!bucket.remove(&contact.id));
    }

    #[test]
    fn kbucket_closest_to_returns_sorted() {
        let mut bucket = KBucket::new();
        let local_id = make_node_id(128);
        for i in 0..10 {
            bucket.insert(make_contact(i)).unwrap();
        }
        let target = make_node_id(130);
        let closest = bucket.closest_to(&target);
        assert_eq!(closest.len(), 10);
        // First element must be at least as close as second.
        let d0 = closest[0].id.xor_distance(&target);
        let d1 = closest[1].id.xor_distance(&target);
        assert!(d0 <= d1);
        let _ = local_id;
    }

    // ────────────────────────────────────────────────────────────────────
    // RoutingTable tests
    // ────────────────────────────────────────────────────────────────────

    #[test]
    fn routing_table_new_is_empty() {
        let local_id = make_node_id(42);
        let table = RoutingTable::new(local_id.clone());
        assert_eq!(table.local_id(), &local_id);
        assert_eq!(table.num_contacts(), 0);
    }

    #[test]
    fn routing_table_insert_and_find() {
        let local_id = make_node_id(128);
        let mut table = RoutingTable::new(local_id);

        for i in 0..20 {
            let contact = make_contact(i);
            assert!(table.insert(contact).is_ok());
        }
        assert!(table.num_contacts() > 0);
    }

    #[test]
    fn routing_table_closest_returns_k_sorted() {
        let local_id = make_node_id(128);
        let mut table = RoutingTable::new(local_id);

        for i in 0..100 {
            // Insert succeeds or hits the K-bucket-full gate; either is OK
            // — we just need the table to be populated.
            let _ = table.insert(make_contact(i));
        }

        let target = make_node_id(150);
        let closest = table.closest(&target, 20);
        assert!(closest.len() <= 20);
        // Sorted ascending by XOR distance.
        for w in closest.windows(2) {
            let d0 = w[0].id.xor_distance(&target);
            let d1 = w[1].id.xor_distance(&target);
            assert!(d0 <= d1);
        }
    }

    #[test]
    fn routing_table_rejects_self_contact() {
        let local_id = make_node_id(1);
        let mut table = RoutingTable::new(local_id.clone());
        let self_contact = Contact::new(local_id, "127.0.0.1:8080".parse().unwrap());
        let result = table.insert(self_contact);
        assert!(matches!(
            result,
            Err(InsertError::SelfContact)
        ));
    }

    #[test]
    fn routing_table_remove_dead_contacts_drops_stale() {
        let local_id = make_node_id(1);
        let mut table = RoutingTable::new(local_id);
        let mut stale = make_contact(2);
        stale.last_seen = Instant::now() - Duration::from_secs(7200);
        table.insert(stale.clone()).unwrap();

        let removed = table.remove_dead_contacts();
        assert!(removed.contains(&stale.id));
    }

    // ────────────────────────────────────────────────────────────────────
    // DhtKey tests
    // ────────────────────────────────────────────────────────────────────

    #[test]
    fn dht_key_from_hex_roundtrips() {
        let hex = "deadbeef";
        let key = DhtKey::from_content_hash_hex(hex);
        assert_eq!(key.as_hex(), hex);
    }

    #[test]
    fn dht_key_xor_distance_is_symmetric() {
        let k1 = DhtKey::from_bytes(vec![0u8; 32]);
        let k2 = DhtKey::from_bytes(vec![1u8; 32]);
        let d12 = k1.xor_distance(&k2);
        let d21 = k2.xor_distance(&k1);
        assert_eq!(d12, d21);
    }

    #[test]
    fn dht_key_xor_self_is_zero() {
        let k = DhtKey::from_bytes(vec![7u8; 32]);
        let d = k.xor_distance(&k);
        assert!(d.iter().all(|b| *b == 0));
    }

    #[test]
    fn dht_key_log_distance_zero_for_equal() {
        let k = DhtKey::from_bytes(vec![1u8; 32]);
        assert_eq!(k.log_distance(&k), Some(0));
    }

    // ────────────────────────────────────────────────────────────────────
    // DhtStorage tests (in-memory backend)
    // ────────────────────────────────────────────────────────────────────

    #[test]
    fn storage_provider_round_trip() {
        let store = new_in_memory_store();
        let key = DhtKey::from_bytes(vec![0u8; 32]);
        let node_id = NodeId::random();
        let record = ProviderRecord::new(
            key.clone(),
            node_id.clone(),
            "127.0.0.1:8080".to_string(),
        );
        assert!(store.put_provider(&key, record));

        let got = store.get_providers(&key);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].provider_id, node_id);
    }

    #[test]
    fn storage_provider_dedup_per_provider_id() {
        let store = new_in_memory_store();
        let key = DhtKey::from_bytes(vec![0u8; 32]);
        let node_id = NodeId::random();
        let mut a = ProviderRecord::new(key.clone(), node_id.clone(), "addr-a".into());
        let b = ProviderRecord::new(key.clone(), node_id.clone(), "addr-b".into());
        // Disable timestamp-based dedup race by forcing identical created_at.
        a.created_at = b.created_at;
        store.put_provider(&key, a);
        store.put_provider(&key, b);

        let got = store.get_providers(&key);
        // Per-provider dedup → exactly one record, latest wins.
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].provider_addr, "addr-b");
    }

    #[test]
    fn storage_ipns_sequence_ordering() {
        let store = new_in_memory_store();
        let key = DhtKey::from_bytes(vec![1u8; 32]);
        let mut older = IpnRecord::new(key.clone(), "v1".into());
        older.sequence = 1;
        let mut newer = IpnRecord::new(key.clone(), "v2".into());
        newer.sequence = 2;
        assert!(store.put_ipns(&key, older));
        // Newer must override older.
        assert!(store.put_ipns(&key, newer.clone()));
        assert_eq!(store.get_ipns(&key).unwrap().value, "v2");
        // Re-inserting older must be rejected (sequence guard).
        let mut stale = IpnRecord::new(key.clone(), "v0".into());
        stale.sequence = 1;
        assert!(!store.put_ipns(&key, stale));
    }

    #[test]
    fn storage_counts_match_implementation() {
        let store = new_in_memory_store();
        let key = DhtKey::from_bytes(vec![2u8; 32]);
        let node_id = NodeId::random();
        let record = ProviderRecord::new(
            key.clone(),
            node_id,
            "127.0.0.1:8080".to_string(),
        );
        store.put_provider(&key, record);
        store.put_ipns(&key, IpnRecord::new(key.clone(), "/ipfs/v1".into()));

        // We can't easily build a DhtValue here without the `record`
        // module's `DhtValue` (not exported by `a3net_dht::*`). So we
        // just exercise the provider + ipns counts.
        assert_eq!(store.get_all_provider_count(), 1);
        assert_eq!(store.get_ipns_count(), 1);
        assert_eq!(store.get_values_count(), 0);
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn storage_clear_drops_everything() {
        let store = new_in_memory_store();
        let key = DhtKey::from_bytes(vec![3u8; 32]);
        let node_id = NodeId::random();
        store.put_provider(
            &key,
            ProviderRecord::new(key.clone(), node_id, "127.0.0.1:8080".into()),
        );
        store.put_ipns(&key, IpnRecord::new(key.clone(), "v1".into()));

        store.clear();
        assert_eq!(store.get_all_provider_count(), 0);
        assert_eq!(store.get_ipns_count(), 0);
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn storage_remove_expired_providers_cleans_up() {
        let store = new_in_memory_store();
        let key = DhtKey::from_bytes(vec![4u8; 32]);
        let node_id = NodeId::random();
        let mut expired = ProviderRecord::new(
            key.clone(),
            node_id,
            "127.0.0.1:8080".into(),
        );
        // Backdate created_at so the record is already expired.
        expired.created_at = 1;
        store.put_provider(&key, expired);
        let removed = store.remove_expired_providers();
        assert_eq!(removed, 1);
        assert_eq!(store.get_all_provider_count(), 0);
    }

    // ────────────────────────────────────────────────────────────────────
    // DhtNode tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn dht_node_local_announce_and_find() {
        let node = DhtNode::with_id(NodeId::random());
        let key = DhtKey::from_bytes(vec![9u8; 32]);
        node.announce_content(&key).await;

        let providers = node.find_providers(&key).await;
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].provider_id, *node.local_id());
    }

    #[tokio::test]
    async fn dht_node_local_find_returns_empty_for_unknown_key() {
        let node = DhtNode::with_id(NodeId::random());
        let providers = node
            .find_providers(&DhtKey::from_bytes(vec![7u8; 32]))
            .await;
        assert!(providers.is_empty());
    }

    #[tokio::test]
    async fn dht_node_external_addr_round_trips() {
        let node = DhtNode::with_id(NodeId::random());
        assert!(node.local_addr_str().is_none());
        node.set_local_addr("/ip4/1.2.3.4/tcp/4001".into());
        assert_eq!(
            node.local_addr_str().as_deref(),
            Some("/ip4/1.2.3.4/tcp/4001")
        );
    }

    #[test]
    fn dht_config_default_has_reasonable_values() {
        let cfg = crate::DhtConfig::default();
        assert_eq!(cfg.k, 20);
        assert!(cfg.bootstrap_nodes.is_empty());
    }
}