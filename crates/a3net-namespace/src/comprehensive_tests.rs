//! Comprehensive integration tests for a3net-namespace module.
//!
//! This file provides comprehensive test coverage for all public functions
//! in the a3net-namespace crate, targeting 100% function coverage.

use a3net_gossip::transport::InProcessGossip;
use a3net_gossip::GossipBus;
use a3net_types::NodeId;
use std::sync::Arc;
use std::time::Duration;

// ============================================================================
// TESTS FOR pubsub.rs
// ============================================================================

mod pubsub_tests {
    use super::*;
    use crate::ipns::{Ed25519SecretKey, IpnPublisher};
    use crate::pubsub::{
        IpnGossipPayload, PubsubIpnsResolver, IPNS_PUBSUB_ROOM, publish_payload,
    };

    fn make_signed_record(name: &str, value: &str) -> crate::ipns::IpnRecord {
        let secret = Ed25519SecretKey::generate();
        let publisher = IpnPublisher::new(Arc::new(secret));
        futures::executor::block_on(async {
            publisher
                .publish(name, value.to_string(), Duration::from_secs(60))
                .await
                .expect("publish")
        })
    }

    #[test]
    fn ipn_gossip_payload_from_record_sets_kind_and_from_node() {
        let record = make_signed_record("test-name", "test-value");
        let env = IpnGossipPayload::from_record(&record, "node123").expect("from_record");

        assert_eq!(env.kind, "ipns");
        assert_eq!(env.from_node, "node123");
        assert!(!env.payload.is_null());
    }

    #[test]
    fn ipn_gossip_payload_into_announcement_value_roundtrips() {
        let record = make_signed_record("roundtrip", "value");
        let env = IpnGossipPayload::from_record(&record, "node").expect("from_record");

        let value = env.clone().into_announcement_value().expect("into_value");
        let parsed: IpnGossipPayload = serde_json::from_value(value).expect("parse_back");

        assert_eq!(parsed.kind, env.kind);
        assert_eq!(parsed.from_node, env.from_node);
    }

    #[test]
    fn ipn_gossip_payload_decode_record_extracts_ipns_fields() {
        let record = make_signed_record("decode-test", "/ipfs/QmTest");
        let env = IpnGossipPayload::from_record(&record, "node").expect("from_record");

        let decoded = env.decode_record().expect("decode");
        assert_eq!(decoded.name, "decode-test");
        assert_eq!(decoded.value, "/ipfs/QmTest");
        assert_eq!(decoded.sequence, 1);
    }

    #[test]
    fn ipn_gossip_payload_decode_record_rejects_wrong_kind() {
        let env = IpnGossipPayload {
            kind: "content".to_string(),
            from_node: "node".to_string(),
            payload: serde_json::json!({}),
        };

        let err = env.decode_record().unwrap_err();
        assert!(matches!(err, crate::ipns::IpnsError::Deserialize(_)));
    }

    #[test]
    fn ipn_gossip_payload_into_announcement_payload_sets_from_node() {
        let record = make_signed_record("ann-payload", "v");
        let node_id = NodeId::random();
        let gossip_env = IpnGossipPayload::from_record(&record, "node").expect("from_record");
        let payload = gossip_env.into_announcement_payload(node_id.clone()).expect("into_payload");

        assert_eq!(payload.from_node, node_id);
    }

    #[test]
    fn publish_payload_creates_valid_announcement() {
        let record = make_signed_record("pub-payload", "/ipfs/Qm");
        let node_id = NodeId::random();
        let payload = publish_payload(&record, "from-node", node_id).expect("publish_payload");

        let env: IpnGossipPayload =
            serde_json::from_value(payload.payload).expect("parse_envelope");
        assert_eq!(env.kind, "ipns");
        assert_eq!(env.from_node, "from-node");
    }

    #[test]
    fn ipns_pubsub_room_constant_value() {
        assert_eq!(IPNS_PUBSUB_ROOM, "a3net-ipns-v1");
    }

    #[test]
    fn pubsub_ipns_resolver_new_uses_default_room() {
        let resolver = Arc::new(crate::ipns::IpnResolver::new(Duration::from_secs(60)));
        let pubsub = PubsubIpnsResolver::new(resolver.clone());

        assert_eq!(pubsub.room_id().as_str(), IPNS_PUBSUB_ROOM);
    }

    #[test]
    fn pubsub_ipns_resolver_with_room_uses_custom_room() {
        let resolver = Arc::new(crate::ipns::IpnResolver::new(Duration::from_secs(60)));
        let pubsub = PubsubIpnsResolver::with_room(resolver, "custom-room".into());

        assert_eq!(pubsub.room_id().as_str(), "custom-room");
    }

    #[test]
    fn pubsub_ipns_resolver_resolver_accessor() {
        let resolver = Arc::new(crate::ipns::IpnResolver::new(Duration::from_secs(120)));
        let pubsub = PubsubIpnsResolver::new(resolver.clone());

        assert_eq!(pubsub.resolver().cache_ttl(), Duration::from_secs(120));
    }

    #[tokio::test]
    async fn pubsub_resolver_run_and_shutdown_cycle() {
        let transport = Arc::new(InProcessGossip::new());
        let bus = GossipBus::new(NodeId::random(), transport);
        let resolver = Arc::new(crate::ipns::IpnResolver::new(Duration::from_secs(60)));
        let pubsub = PubsubIpnsResolver::new(resolver);

        let sub = pubsub.run(bus);
        tokio::time::sleep(Duration::from_millis(10)).await;
        sub.shutdown().await;
    }

    #[tokio::test]
    async fn pubsub_resolver_ingests_record_end_to_end() {
        let transport = Arc::new(InProcessGossip::new());
        let bus_a = GossipBus::new(NodeId::random(), transport.clone());
        let bus_b = GossipBus::new(NodeId::random(), transport);

        let resolver = Arc::new(crate::ipns::IpnResolver::new(Duration::from_secs(60)));
        let pubsub = PubsubIpnsResolver::new(resolver.clone());
        let room = pubsub.room_id().clone();

        let _sub = pubsub.run(bus_b);
        tokio::time::sleep(Duration::from_millis(20)).await;

        let secret = Ed25519SecretKey::generate();
        let name = secret.ipns_name();
        let publisher = IpnPublisher::new(Arc::new(secret));
        let record = publisher
            .publish(&name, "e2e-value".to_string(), Duration::from_secs(60))
            .await
            .expect("publish");

        let from_node_str = format!("{}", bus_a.local_node().short());
        let payload = publish_payload(&record, &from_node_str, bus_a.local_node().clone())
            .expect("payload");
        bus_a
            .transport()
            .broadcast(bus_a.topic_for(&room), payload)
            .await
            .expect("broadcast");

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if resolver.get_cached(&name).is_some() {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("resolver did not ingest within 2s");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let cached = resolver.get_cached(&name).expect("cached");
        assert_eq!(cached.value, "e2e-value");
    }

    #[tokio::test]
    async fn pubsub_resolver_shutdown_stops_ingestion() {
        let transport = Arc::new(InProcessGossip::new());
        let bus_a = GossipBus::new(NodeId::random(), transport.clone());
        let bus_b = GossipBus::new(NodeId::random(), transport);

        let resolver = Arc::new(crate::ipns::IpnResolver::new(Duration::from_secs(60)));
        let pubsub = PubsubIpnsResolver::new(resolver.clone());
        let room = pubsub.room_id().clone();

        let sub = pubsub.run(bus_b);
        tokio::time::sleep(Duration::from_millis(20)).await;
        sub.shutdown().await;

        let secret = Ed25519SecretKey::generate();
        let name = secret.ipns_name();
        let publisher = IpnPublisher::new(Arc::new(secret));
        let record = publisher
            .publish(&name, "after-shutdown".to_string(), Duration::from_secs(60))
            .await
            .expect("publish");
        let payload = publish_payload(&record, "node", NodeId::random()).expect("payload");
        let _ = bus_a
            .transport()
            .broadcast(bus_a.topic_for(&room), payload)
            .await;

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            resolver.get_cached(&name).is_none(),
            "post-shutdown records must not be ingested"
        );
    }

    #[tokio::test]
    async fn pubsub_resolver_skips_non_ipns_envelopes() {
        let transport = Arc::new(InProcessGossip::new());
        let bus_a = GossipBus::new(NodeId::random(), transport.clone());
        let bus_b = GossipBus::new(NodeId::random(), transport);

        let resolver = Arc::new(crate::ipns::IpnResolver::new(Duration::from_secs(60)));
        let pubsub = PubsubIpnsResolver::new(resolver.clone());
        let room = pubsub.room_id().clone();

        let _sub = pubsub.run(bus_b);
        tokio::time::sleep(Duration::from_millis(20)).await;

        let bogus = a3net_types::AnnouncementPayload {
            from_node: NodeId::random(),
            payload: serde_json::json!({
                "kind": "not-ipns",
                "from_node": "n",
                "payload": {},
            }),
        };
        bus_a
            .transport()
            .broadcast(bus_a.topic_for(&room), bogus)
            .await
            .expect("broadcast");

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(resolver.get_cached("anything").is_none());
    }
}

// ============================================================================
// TESTS FOR dnslink.rs
// ============================================================================

mod dnslink_tests {
    use crate::dnslink::{
        DnsLinkPath, DnsLinkResolver, DnsLinkError, DnsLookup, InMemoryLookup,
    };
    use std::sync::Arc;

    #[test]
    fn in_memory_lookup_new_and_insert() {
        let lookup = InMemoryLookup::new();
        lookup.insert("_dnslink.test.com".to_string(), vec!["dnslink=/ipfs/test".to_string()]);

        let records = lookup.lookup_txt("_dnslink.test.com");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0], "dnslink=/ipfs/test");
    }

    #[test]
    fn in_memory_lookup_insert_dnslink() {
        let lookup = InMemoryLookup::new();
        lookup.insert_dnslink("mydomain.com", "/ipfs/bafytest");

        let records = lookup.lookup_txt("_dnslink.mydomain.com");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0], "dnslink=/ipfs/bafytest");
    }

    #[test]
    fn in_memory_lookup_overwrites_previous() {
        let lookup = InMemoryLookup::new();
        lookup.insert_dnslink("overwrite.com", "/ipfs/v1");
        lookup.insert_dnslink("overwrite.com", "/ipfs/v2");

        let records = lookup.lookup_txt("_dnslink.overwrite.com");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0], "dnslink=/ipfs/v2");
    }

    #[test]
    fn in_memory_lookup_returns_empty_for_unknown() {
        let lookup = InMemoryLookup::new();
        let records = lookup.lookup_txt("_dnslink.unknown.com");
        assert!(records.is_empty());
    }

    #[test]
    fn in_memory_lookup_as_in_memory() {
        let lookup = InMemoryLookup::new();
        let inner = lookup.as_in_memory();
        assert!(inner.is_some());
    }

    #[test]
    fn in_memory_lookup_default() {
        let lookup = InMemoryLookup::default();
        let records = lookup.lookup_txt("_dnslink.anything.com");
        assert!(records.is_empty());
    }

    #[test]
    fn dns_link_resolver_new() {
        let resolver = DnsLinkResolver::new();
        assert!(resolver.in_memory().is_some());
    }

    #[test]
    fn dns_link_resolver_with_lookup() {
        let custom_lookup = InMemoryLookup::new();
        custom_lookup.insert_dnslink("custom.com", "/ipfs/custom");

        let resolver = DnsLinkResolver::with_lookup(Arc::new(custom_lookup));
        let path = resolver.resolve("custom.com").expect("resolve");
        assert_eq!(path, "/ipfs/custom");
    }

    #[test]
    fn dns_link_resolver_with_lookup_in_memory_returns_none() {
        #[derive(Debug)]
        struct CustomLookup;
        impl crate::dnslink::DnsLookup for CustomLookup {
            fn lookup_txt(&self, _fqdn: &str) -> Vec<String> {
                vec![]
            }
        }

        let resolver = DnsLinkResolver::with_lookup(Arc::new(CustomLookup));
        assert!(resolver.in_memory().is_none());
    }

    #[test]
    fn dns_link_resolver_default() {
        let resolver = DnsLinkResolver::default();
        assert!(resolver.in_memory().is_some());
    }

    #[test]
    fn dns_link_resolver_resolve_success() {
        let resolver = DnsLinkResolver::new();
        resolver
            .in_memory()
            .unwrap()
            .insert_dnslink("resolve-test.com", "/ipfs/QmTest");

        let path = resolver.resolve("resolve-test.com").expect("resolve");
        assert_eq!(path, "/ipfs/QmTest");
    }

    #[test]
    fn dns_link_resolver_resolve_case_insensitive_domain() {
        let resolver = DnsLinkResolver::new();
        resolver
            .in_memory()
            .unwrap()
            .insert_dnslink("case.com", "/ipfs/case");

        let path = resolver.resolve("CASE.COM").expect("resolve");
        assert_eq!(path, "/ipfs/case");
    }

    #[test]
    fn dns_link_resolver_resolve_trims_trailing_dot() {
        let resolver = DnsLinkResolver::new();
        resolver
            .in_memory()
            .unwrap()
            .insert_dnslink("trailing.com", "/ipfs/trailing");

        let path = resolver.resolve("trailing.com.").expect("resolve");
        assert_eq!(path, "/ipfs/trailing");
    }

    #[test]
    fn dns_link_resolver_resolve_not_found() {
        let resolver = DnsLinkResolver::new();
        let err = resolver.resolve("notfound.com").unwrap_err();
        assert!(matches!(err, DnsLinkError::NotFound(fqdn) if fqdn == "_dnslink.notfound.com"));
    }

    #[test]
    fn dns_link_resolver_resolve_no_link_in_records() {
        let resolver = DnsLinkResolver::new();
        resolver
            .in_memory()
            .unwrap()
            .insert("_dnslink.nolink.com", vec!["v=spf1 -all".to_string()]);

        let err = resolver.resolve("nolink.com").unwrap_err();
        assert!(matches!(err, DnsLinkError::NoLink(_)));
    }

    #[test]
    fn dns_link_resolver_resolve_empty_domain() {
        let resolver = DnsLinkResolver::new();
        let err = resolver.resolve("").unwrap_err();
        assert!(matches!(err, DnsLinkError::InvalidDomain));
    }

    #[test]
    fn dns_link_resolver_resolve_path_success() {
        let resolver = DnsLinkResolver::new();
        resolver
            .in_memory()
            .unwrap()
            .insert_dnslink("path-test.com", "/ipfs/QmPath");

        let path = resolver.resolve_path("path-test.com").expect("resolve_path");
        assert_eq!(path, DnsLinkPath::Ipfs("QmPath".to_string()));
    }

    #[test]
    fn dns_link_resolver_resolve_path_ipns() {
        let resolver = DnsLinkResolver::new();
        resolver
            .in_memory()
            .unwrap()
            .insert_dnslink("ipns-test.com", "/ipns/k51qzi...");

        let path = resolver.resolve_path("ipns-test.com").expect("resolve_path");
        assert_eq!(path, DnsLinkPath::Ipns("k51qzi...".to_string()));
    }

    #[test]
    fn dns_link_resolver_resolve_path_relative() {
        let resolver = DnsLinkResolver::new();
        resolver
            .in_memory()
            .unwrap()
            .insert_dnslink("relative.com", "/some/relative/path");

        let path = resolver.resolve_path("relative.com").expect("resolve_path");
        assert_eq!(path, DnsLinkPath::Relative("/some/relative/path".to_string()));
    }

    #[test]
    fn dns_link_path_parse_ipfs() {
        let path = DnsLinkPath::parse("/ipfs/QmTest123").expect("parse");
        assert_eq!(path, DnsLinkPath::Ipfs("QmTest123".to_string()));
    }

    #[test]
    fn dns_link_path_parse_ipfs_with_slashes() {
        let path = DnsLinkPath::parse("/ipfs/cid/path").expect("parse");
        assert_eq!(path, DnsLinkPath::Ipfs("cid/path".to_string()));
    }

    #[test]
    fn dns_link_path_parse_ipns() {
        let path = DnsLinkPath::parse("/ipns/k51qzi...").expect("parse");
        assert_eq!(path, DnsLinkPath::Ipns("k51qzi...".to_string()));
    }

    #[test]
    fn dns_link_path_parse_relative() {
        let path = DnsLinkPath::parse("relative/path").expect("parse");
        assert_eq!(path, DnsLinkPath::Relative("relative/path".to_string()));
    }

    #[test]
    fn dns_link_path_parse_whitespace_trimmed() {
        let path = DnsLinkPath::parse("  /ipfs/cid  ").expect("parse");
        assert_eq!(path, DnsLinkPath::Ipfs("cid".to_string()));
    }

    #[test]
    fn dns_link_path_as_str() {
        let ipfs = DnsLinkPath::Ipfs("QmTest".to_string());
        assert_eq!(ipfs.as_str(), "QmTest");

        let ipns = DnsLinkPath::Ipns("k51...".to_string());
        assert_eq!(ipns.as_str(), "k51...");

        let relative = DnsLinkPath::Relative("/path".to_string());
        assert_eq!(relative.as_str(), "/path");
    }

    #[test]
    fn dns_link_path_debug() {
        let path = DnsLinkPath::Ipfs("QmTest".to_string());
        let debug_str = format!("{:?}", path);
        assert!(debug_str.contains("Ipfs"));
    }

    #[test]
    fn dns_link_error_display() {
        let err = DnsLinkError::InvalidDomain;
        assert_eq!(err.to_string(), "domain name is empty");

        let err = DnsLinkError::NotFound("test.com".to_string());
        assert!(err.to_string().contains("test.com"));

        let err = DnsLinkError::NoLink("_dnslink.test.com".to_string());
        assert!(err.to_string().contains("_dnslink.test.com"));

        let err = DnsLinkError::InvalidPath("/bad".to_string());
        assert!(err.to_string().contains("/bad"));
    }
}

// ============================================================================
// TESTS FOR transport/mod.rs
// ============================================================================

mod transport_mod_tests {
    use crate::ipns::IpnRecord;
    use crate::transport::{default_transports, SharedIpnBus, TransportHealth};
    use futures::StreamExt;
    use std::time::Duration;

    fn fresh_record(name: &str, value: &str) -> IpnRecord {
        IpnRecord::with_name_value(name.to_string(), value.to_string())
    }

    #[test]
    fn shared_ipn_bus_new_with_zero_capacity() {
        let bus = SharedIpnBus::new(0);
        let record = fresh_record("n", "v");
        bus.publish(&record).expect("publish");
    }

    #[test]
    fn shared_ipn_bus_publish_multiple_listeners() {
        let bus = SharedIpnBus::new(8);
        let r1 = fresh_record("a", "1");
        let r2 = fresh_record("b", "2");

        let mut s1 = bus.subscribe();
        let mut s2 = bus.subscribe();

        bus.publish(&r1).unwrap();
        bus.publish(&r2).unwrap();

        let first_a = futures::executor::block_on(async { s1.next().await }).unwrap().unwrap();
        let first_b = futures::executor::block_on(async { s2.next().await }).unwrap().unwrap();
        assert_eq!(first_a.name, "a");
        assert_eq!(first_b.name, "a");
    }

    #[tokio::test]
    async fn shared_ipn_bus_subscribe_stream_completes_on_close() {
        let bus = SharedIpnBus::new(1);
        let mut s = bus.subscribe();
        drop(bus);

        let result = tokio::time::timeout(Duration::from_millis(100), s.next()).await;
        assert!(result.is_err() || result.unwrap().is_none());
    }

    #[test]
    fn shared_ipn_bus_sender_clone_works() {
        let bus = SharedIpnBus::new(4);
        let tx = bus.sender();
        let tx2 = tx.clone();
        let mut rx1 = tx.subscribe();
        let mut rx2 = tx2.subscribe();

        let r = fresh_record("clone", "test");
        tx.send(r.clone()).expect("send");

        let _ = rx1.try_recv();
        let _ = rx2.try_recv();
    }

    #[test]
    fn transport_health_variants() {
        assert_eq!(TransportHealth::Healthy, TransportHealth::Healthy);
        assert_eq!(TransportHealth::Degraded, TransportHealth::Degraded);
        assert_eq!(TransportHealth::Down, TransportHealth::Down);
        assert_eq!(TransportHealth::Unknown, TransportHealth::Unknown);
    }

    #[test]
    fn transport_health_debug() {
        let health = TransportHealth::Healthy;
        let debug_str = format!("{:?}", health);
        assert!(debug_str.contains("Healthy"));
    }

    #[test]
    fn transport_health_partial_eq() {
        assert_eq!(TransportHealth::Healthy, TransportHealth::Healthy);
        assert_ne!(TransportHealth::Healthy, TransportHealth::Degraded);
        assert_ne!(TransportHealth::Degraded, TransportHealth::Down);
    }

    #[test]
    fn default_transports_none_returns_pkarr() {
        let v = default_transports(None);
        let names: Vec<&'static str> = v.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"pkarr"));
        assert!(!names.contains(&"disk-journal"));
    }

    #[test]
    fn default_transports_with_disk_includes_both() {
        let dir = tempfile::TempDir::new().unwrap();
        let v = default_transports(Some(dir.path().to_path_buf()));
        let names: Vec<&'static str> = v.iter().map(|t| t.name()).collect();

        assert!(names.contains(&"pkarr"));
        assert!(names.contains(&"disk-journal"));
        assert_eq!(names[0], "disk-journal");
    }
}

// ============================================================================
// TESTS FOR transport/disk.rs
// ============================================================================

mod disk_transport_tests {
    use crate::ipns::{Ed25519SecretKey, IpnRecord};
    use crate::transport::disk::DiskJournalTransport;
    use crate::transport::IpnTransport;
    use futures::StreamExt;
    use std::time::Duration;

    fn sign_record(name: &str, value: &str) -> IpnRecord {
        let key = Ed25519SecretKey::generate();
        let mut r = IpnRecord::with_name_value(name.to_string(), value.to_string());
        r.sign(&key).expect("sign");
        r
    }

    #[tokio::test]
    async fn disk_journal_new() {
        let dir = tempfile::TempDir::new().unwrap();
        let t = DiskJournalTransport::new(dir.path().to_path_buf());
        assert_eq!(t.name(), "disk-journal");
    }

    #[tokio::test]
    async fn disk_journal_append_multiple_records() {
        let dir = tempfile::TempDir::new().unwrap();
        let t = DiskJournalTransport::new(dir.path().to_path_buf());

        for i in 0..5 {
            let r = sign_record(&format!("name-{}", i), &format!("value-{}", i));
            t.publish(&r).await.expect("publish");
        }

        let replay = t.replay_all().await.expect("replay");
        assert_eq!(replay.len(), 5);
    }

    #[tokio::test]
    async fn disk_journal_replay_all_order_preserved() {
        let dir = tempfile::TempDir::new().unwrap();
        let t = DiskJournalTransport::new(dir.path().to_path_buf());

        let r1 = sign_record("first", "v1");
        let r2 = sign_record("second", "v2");
        let r3 = sign_record("third", "v3");

        t.publish(&r1).await.unwrap();
        t.publish(&r2).await.unwrap();
        t.publish(&r3).await.unwrap();

        let replay = t.replay_all().await.unwrap();
        assert_eq!(replay[0].name, "first");
        assert_eq!(replay[1].name, "second");
        assert_eq!(replay[2].name, "third");
    }

    #[tokio::test]
    async fn disk_journal_subscribe_emits_replayed_records() {
        let dir = tempfile::TempDir::new().unwrap();
        let t = DiskJournalTransport::new(dir.path().to_path_buf());

        let r1 = sign_record("replay-a", "v1");
        let r2 = sign_record("replay-b", "v2");
        t.publish(&r1).await.unwrap();
        t.publish(&r2).await.unwrap();

        let mut s = t.subscribe("any").await.unwrap();
        let first = s.next().await.expect("first").expect("ok");
        let second = s.next().await.expect("second").expect("ok");

        assert_eq!(first.name, "replay-a");
        assert_eq!(second.name, "replay-b");
    }

    #[tokio::test]
    async fn disk_journal_health_returns_unknown() {
        let dir = tempfile::TempDir::new().unwrap();
        let t = DiskJournalTransport::new(dir.path().to_path_buf());
        let h = t.health().await.unwrap();
        assert_eq!(h, crate::transport::TransportHealth::Unknown);
    }
}

// ============================================================================
// TESTS FOR transport/gossip.rs
// ============================================================================

mod gossip_transport_tests {
    use crate::ipns::Ed25519SecretKey;
    use crate::transport::gossip::{GossipIpnTransport, IPNS_TOPIC};
    use crate::transport::{IpnTransport, SharedIpnBus, TransportHealth};
    use futures::StreamExt;
    use std::time::Duration;

    fn signed_record(name: &str, value: &str) -> crate::ipns::IpnRecord {
        let key = Ed25519SecretKey::generate();
        let mut r = crate::ipns::IpnRecord::with_name_value(name.to_string(), value.to_string());
        r.sign(&key).expect("sign");
        r
    }

    #[test]
    fn gossip_ipn_transport_default() {
        let t = GossipIpnTransport::default();
        assert_eq!(t.name(), "gossip");
    }

    #[test]
    fn gossip_ipn_transport_noop() {
        let t = GossipIpnTransport::noop();
        assert_eq!(t.name(), "gossip");
    }

    #[test]
    fn gossip_ipn_transport_ipns_topic() {
        assert_eq!(IPNS_TOPIC, "a3net-ipns-v1");
    }

    #[tokio::test]
    async fn gossip_transport_with_broadcast_sender() {
        let bus = SharedIpnBus::new(8);
        let t = GossipIpnTransport::new(bus.sender());
        assert_eq!(t.health().await.unwrap(), TransportHealth::Healthy);
    }

    #[tokio::test]
    async fn gossip_transport_publish_without_bus() {
        let t = GossipIpnTransport::default();
        let r = signed_record("n", "v");
        t.publish(&r).await.expect("publish");
    }

    #[tokio::test]
    async fn gossip_transport_subscribe_with_no_bus() {
        let t = GossipIpnTransport::noop();
        let mut s = t.subscribe("n").await.unwrap();
        assert!(s.next().await.is_none());
    }

    #[tokio::test]
    async fn gossip_transport_multiple_subscribers() {
        let bus = SharedIpnBus::new(8);
        let t = GossipIpnTransport::new(bus.sender());

        let mut s1 = t.subscribe("any").await.unwrap();
        let mut s2 = t.subscribe("any").await.unwrap();

        let r = signed_record("multi", "v");
        t.publish(&r).await.unwrap();

        let first = s1.next().await.expect("s1-first").expect("ok");
        let second = s2.next().await.expect("s2-first").expect("ok");
        assert_eq!(first.name, "multi");
        assert_eq!(second.name, "multi");
    }

    #[tokio::test]
    async fn gossip_transport_forwards_valid_signatures() {
        let bus = SharedIpnBus::new(8);
        let t = GossipIpnTransport::new(bus.sender());

        // Subscribe BEFORE publishing to ensure we receive the message
        let mut s = t.subscribe("any").await.unwrap();

        let r = signed_record("valid-sig", "v");
        t.publish(&r).await.unwrap();

        let received = s.next().await.expect("record").expect("ok");
        assert_eq!(received.name, "valid-sig");
    }

    #[tokio::test]
    async fn gossip_transport_drops_empty_signature() {
        let bus = SharedIpnBus::new(8);
        let t = GossipIpnTransport::new(bus.sender());

        let mut r = crate::ipns::IpnRecord::with_name_value("empty-sig".into(), "v".into());
        r.signature.clear();

        t.publish(&r).await.unwrap();

        let mut s = t.subscribe("any").await.unwrap();
        let result = tokio::time::timeout(Duration::from_millis(50), s.next()).await;
        assert!(result.is_err() || result.unwrap().is_none());
    }

    #[test]
    fn gossip_transport_debug() {
        let t = GossipIpnTransport::default();
        let debug_str = format!("{:?}", t);
        assert!(debug_str.contains("GossipIpnTransport"));
    }
}

// ============================================================================
// TESTS FOR transport/pkarr.rs
// ============================================================================

mod pkarr_transport_tests {
    use crate::ipns::Ed25519SecretKey;
    use crate::transport::pkarr::{PkarrConfig, PkarrRelay, PkarrTransport};
    use crate::transport::{IpnTransport, TransportHealth};
    use async_trait::async_trait;
    use futures::StreamExt;
    use std::time::Duration;

    fn signed_record(name: &str, value: &str) -> crate::ipns::IpnRecord {
        let key = Ed25519SecretKey::generate();
        let mut r = crate::ipns::IpnRecord::with_name_value(name.to_string(), value.to_string());
        r.sign(&key).expect("sign");
        r
    }

    #[test]
    fn pkarr_relay_public_has_https() {
        let relay = PkarrRelay::public();
        assert_eq!(relay.url.scheme(), "https");
    }

    #[test]
    fn pkarr_relay_debug() {
        let relay = PkarrRelay::public();
        let debug_str = format!("{:?}", relay);
        assert!(debug_str.contains("pkarr.pub"));
    }

    #[test]
    fn pkarr_config_default() {
        let cfg = PkarrConfig::default();
        assert_eq!(cfg.relays.len(), 1);
        assert_eq!(cfg.request_timeout, Duration::from_secs(10));
    }

    #[test]
    fn pkarr_config_debug() {
        let cfg = PkarrConfig::default();
        let debug_str = format!("{:?}", cfg);
        assert!(debug_str.contains("relays"));
    }

    #[test]
    fn pkarr_transport_new() {
        let cfg = PkarrConfig::default();
        let t = PkarrTransport::new(cfg);
        assert_eq!(t.name(), "pkarr");
    }

    #[test]
    fn pkarr_transport_debug() {
        let t = PkarrTransport::default();
        let debug_str = format!("{:?}", t);
        assert!(debug_str.contains("PkarrTransport"));
    }

    #[test]
    fn pkarr_transport_with_relays_multiple() {
        let relays = vec![
            PkarrRelay::public(),
            PkarrRelay::public(),
        ];
        let t = PkarrTransport::with_relays(relays);
        assert_eq!(t.name(), "pkarr");
    }

    #[test]
    fn pkarr_transport_dns_name_various_inputs() {
        assert_eq!(PkarrTransport::dns_name("abc"), "_a3net.abc");
        assert_eq!(PkarrTransport::dns_name("k51qzi5u"), "_a3net.k51qzi5u");
        assert_eq!(PkarrTransport::dns_name("123"), "_a3net.123");
    }

    #[test]
    fn pkarr_encode_packet_version_byte() {
        let t = PkarrTransport::default();
        let r = signed_record("encode-test", "v");
        let bytes = t.encode_packet(&r).expect("encode");
        assert_eq!(bytes[0], 0x01);
    }

    #[test]
    fn pkarr_encode_decode_preserves_all_fields() {
        let t = PkarrTransport::default();
        let r = signed_record("full-test", "/ipfs/QmFull");
        let bytes = t.encode_packet(&r).expect("encode");

        let decoded = PkarrTransport::decode_packet("full-test", &bytes).expect("decode");
        assert_eq!(decoded.name, r.name);
        assert_eq!(decoded.value, r.value);
        assert_eq!(decoded.sequence, r.sequence);
        assert_eq!(decoded.ttl_secs, r.ttl_secs);
        assert_eq!(decoded.signature, r.signature);
    }

    #[test]
    fn pkarr_decode_empty_packet() {
        let err = PkarrTransport::decode_packet("name", &[]).unwrap_err();
        assert!(matches!(err, crate::ipns::IpnsError::Transport(_)));
    }

    #[test]
    fn pkarr_decode_wrong_version() {
        let err = PkarrTransport::decode_packet("name", &[0x99, 1, 2, 3]).unwrap_err();
        assert!(matches!(err, crate::ipns::IpnsError::Transport(_)));
    }

    #[test]
    fn pkarr_decode_invalid_json() {
        let bytes = vec![0x01, b'{', b'n', b'o', b't', b'}'];
        let err = PkarrTransport::decode_packet("name", &bytes).unwrap_err();
        assert!(matches!(err, crate::ipns::IpnsError::Transport(_)));
    }

    #[test]
    fn pkarr_decode_name_mismatch() {
        let t = PkarrTransport::default();
        let r = signed_record("correct-name", "v");
        let bytes = t.encode_packet(&r).expect("encode");

        let err = PkarrTransport::decode_packet("wrong-name", &bytes).unwrap_err();
        assert!(matches!(err, crate::ipns::IpnsError::Transport(_)));
    }

    #[tokio::test]
    async fn pkarr_transport_publish_caches() {
        let t = PkarrTransport::default();
        let r = signed_record("cache-test", "v");
        t.publish(&r).await.expect("publish");

        let mut s = t.subscribe("cache-test").await.unwrap();
        let cached = s.next().await.expect("record").expect("ok");
        assert_eq!(cached.name, "cache-test");
    }

    #[tokio::test]
    async fn pkarr_transport_subscribe_empty_for_unknown() {
        let t = PkarrTransport::default();
        let mut s = t.subscribe("unknown").await.unwrap();
        assert!(s.next().await.is_none());
    }

    #[tokio::test]
    async fn pkarr_transport_resolve_now_not_found() {
        let t = PkarrTransport::default();
        let err = t.resolve_now("missing").await.unwrap_err();
        assert!(matches!(err, crate::ipns::IpnsError::NotFound));
    }

    #[tokio::test]
    async fn pkarr_transport_resolve_now_returns_cached() {
        let t = PkarrTransport::default();
        let r = signed_record("cached-resolve", "v");
        t.publish(&r).await.unwrap();

        let resolved = t.resolve_now("cached-resolve").await.unwrap();
        assert_eq!(resolved.value, "v");
    }

    #[tokio::test]
    async fn pkarr_transport_set_client_wired() {
        let t = PkarrTransport::default();
        assert_eq!(t.health().await.unwrap(), TransportHealth::Down);

        t.set_client_wired().await;
        assert_eq!(t.health().await.unwrap(), TransportHealth::Degraded);
    }

    #[tokio::test]
    async fn pkarr_transport_insert_verified() {
        let t = PkarrTransport::default();
        let r = signed_record("verified", "v");
        t.insert_verified(r.clone()).await;

        let handle = t.cache_handle().await;
        let guard = handle.read().await;
        assert!(guard.contains_key("verified"));
    }

    #[tokio::test]
    async fn pkarr_transport_cache_handle_snapshot() {
        let t = PkarrTransport::default();
        let r1 = signed_record("snap1", "v1");
        let r2 = signed_record("snap2", "v2");

        t.insert_verified(r1).await;
        let handle1 = t.cache_handle().await;

        t.insert_verified(r2).await;
        let handle2 = t.cache_handle().await;

        let guard1 = handle1.read().await;
        let guard2 = handle2.read().await;
        assert!(guard1.contains_key("snap1"));
        assert!(guard2.contains_key("snap1"));
        assert!(guard2.contains_key("snap2"));
    }

    #[tokio::test]
    async fn pkarr_resolve_now_with_client_success() {
        let t = PkarrTransport::default();
        let r = signed_record("client-resolve", "/ipfs/QmClient");
        let bytes = t.encode_packet(&r).expect("encode");

        struct MockLookup(Vec<u8>);
        #[async_trait]
        impl crate::transport::pkarr::PkarrLookup for MockLookup {
            async fn lookup(&self, _: &str, _: Duration) -> Result<Vec<u8>, crate::ipns::IpnsError> {
                Ok(self.0.clone())
            }
        }

        let resolved = t
            .resolve_now_with_client("client-resolve", &MockLookup(bytes))
            .await
            .expect("resolve");
        assert_eq!(resolved.value, "/ipfs/QmClient");
    }

    #[tokio::test]
    async fn pkarr_resolve_now_with_client_lookup_error() {
        let t = PkarrTransport::default();

        struct FailLookup;
        #[async_trait]
        impl crate::transport::pkarr::PkarrLookup for FailLookup {
            async fn lookup(&self, _: &str, _: Duration) -> Result<Vec<u8>, crate::ipns::IpnsError> {
                Err(crate::ipns::IpnsError::Transport("lookup failed".into()))
            }
        }

        let err = t
            .resolve_now_with_client("any", &FailLookup)
            .await
            .unwrap_err();
        assert!(matches!(err, crate::ipns::IpnsError::Transport(_)));
    }

    #[tokio::test]
    async fn pkarr_publish_with_client_first_success() {
        let t = PkarrTransport::default();
        let r = signed_record("publish-client", "v");

        struct SuccessPublisher;
        #[async_trait]
        impl crate::transport::pkarr::PkarrPublisher for SuccessPublisher {
            async fn publish(
                &self,
                _: &url::Url,
                _: &str,
                _: &[u8],
                _: Duration,
            ) -> Result<(), crate::ipns::IpnsError> {
                Ok(())
            }
        }

        t.publish_with_client(&r, &SuccessPublisher)
            .await
            .expect("publish");
    }

    #[tokio::test]
    async fn pkarr_publish_with_client_all_fail() {
        let t = PkarrTransport::with_relays(vec![PkarrRelay::public()]);
        let r = signed_record("fail-publish", "v");

        struct FailPublisher;
        #[async_trait]
        impl crate::transport::pkarr::PkarrPublisher for FailPublisher {
            async fn publish(
                &self,
                _: &url::Url,
                _: &str,
                _: &[u8],
                _: Duration,
            ) -> Result<(), crate::ipns::IpnsError> {
                Err(crate::ipns::IpnsError::Transport("failed".into()))
            }
        }

        let err = t
            .publish_with_client(&r, &FailPublisher)
            .await
            .unwrap_err();
        assert!(matches!(err, crate::ipns::IpnsError::Transport(_)));
    }

    #[tokio::test]
    async fn pkarr_health_down_when_empty_no_client() {
        let t = PkarrTransport::default();
        assert_eq!(t.health().await.unwrap(), TransportHealth::Down);
    }

    #[tokio::test]
    async fn pkarr_health_degraded_with_cached_no_client() {
        let t = PkarrTransport::default();
        let r = signed_record("cached", "v");
        t.publish(&r).await.unwrap();
        assert_eq!(t.health().await.unwrap(), TransportHealth::Degraded);
    }

    #[tokio::test]
    async fn pkarr_health_degraded_client_wired_no_activity() {
        let t = PkarrTransport::default();
        t.set_client_wired().await;
        assert_eq!(t.health().await.unwrap(), TransportHealth::Degraded);
    }
}

// ============================================================================
// TESTS FOR transport/multi.rs
// ============================================================================

mod multi_transport_tests {
    use crate::ipns::IpnRecord;
    use crate::transport::{IpnTransport, IpnRecordStream, TransportHealth};
    use crate::MultiTransport;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    fn rec() -> IpnRecord {
        IpnRecord::with_name_value("n".into(), "v".into())
    }

    struct TestTransport {
        name: &'static str,
        calls: Arc<AtomicUsize>,
        fail: bool,
    }

    #[async_trait]
    impl IpnTransport for TestTransport {
        fn name(&self) -> &'static str {
            self.name
        }

        async fn publish(&self, _: &IpnRecord) -> Result<(), crate::ipns::IpnsError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err(crate::ipns::IpnsError::Transport(format!("{} failed", self.name)))
            } else {
                Ok(())
            }
        }

        async fn subscribe(&self, _: &str) -> Result<IpnRecordStream, crate::ipns::IpnsError> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[test]
    fn multi_transport_new() {
        let t: Arc<dyn IpnTransport> = Arc::new(TestTransport {
            name: "t1",
            calls: Arc::new(AtomicUsize::new(0)),
            fail: false,
        });
        let mt = MultiTransport::new(vec![t]);
        assert_eq!(mt.name(), "multi");
    }

    #[test]
    fn multi_transport_with_capacity() {
        let t: Arc<dyn IpnTransport> = Arc::new(TestTransport {
            name: "t1",
            calls: Arc::new(AtomicUsize::new(0)),
            fail: false,
        });
        let mt = MultiTransport::with_capacity(vec![t], 100);
        assert_eq!(mt.name(), "multi");
    }

    #[test]
    fn multi_transport_backends() {
        let a: Arc<dyn IpnTransport> = Arc::new(TestTransport {
            name: "backend-a",
            calls: Arc::new(AtomicUsize::new(0)),
            fail: false,
        });
        let b: Arc<dyn IpnTransport> = Arc::new(TestTransport {
            name: "backend-b",
            calls: Arc::new(AtomicUsize::new(0)),
            fail: false,
        });
        let mt = MultiTransport::new(vec![a, b]);
        assert_eq!(mt.backends(), vec!["backend-a", "backend-b"]);
    }

    #[test]
    fn multi_transport_debug() {
        let t: Arc<dyn IpnTransport> = Arc::new(TestTransport {
            name: "debug-test",
            calls: Arc::new(AtomicUsize::new(0)),
            fail: false,
        });
        let mt = MultiTransport::new(vec![t]);
        let debug_str = format!("{:?}", mt);
        assert!(debug_str.contains("MultiTransport"));
        assert!(debug_str.contains("debug-test"));
    }

    #[tokio::test]
    async fn multi_transport_publish_all_backends() {
        let calls_a = Arc::new(AtomicUsize::new(0));
        let calls_b = Arc::new(AtomicUsize::new(0));
        let a: Arc<dyn IpnTransport> = Arc::new(TestTransport {
            name: "multi-a",
            calls: calls_a.clone(),
            fail: false,
        });
        let b: Arc<dyn IpnTransport> = Arc::new(TestTransport {
            name: "multi-b",
            calls: calls_b.clone(),
            fail: false,
        });
        let mt = MultiTransport::new(vec![a, b]);

        mt.publish(&rec()).await.expect("publish");
        assert_eq!(calls_a.load(Ordering::SeqCst), 1);
        assert_eq!(calls_b.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn multi_transport_publish_partial_failure() {
        let calls_success = Arc::new(AtomicUsize::new(0));
        let success: Arc<dyn IpnTransport> = Arc::new(TestTransport {
            name: "success",
            calls: calls_success.clone(),
            fail: false,
        });
        let fail: Arc<dyn IpnTransport> = Arc::new(TestTransport {
            name: "fail",
            calls: Arc::new(AtomicUsize::new(0)),
            fail: true,
        });
        let mt = MultiTransport::new(vec![fail, success]);

        mt.publish(&rec()).await.expect("publish");
        assert_eq!(calls_success.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn multi_transport_publish_all_fail() {
        let fail_a: Arc<dyn IpnTransport> = Arc::new(TestTransport {
            name: "fail-a",
            calls: Arc::new(AtomicUsize::new(0)),
            fail: true,
        });
        let fail_b: Arc<dyn IpnTransport> = Arc::new(TestTransport {
            name: "fail-b",
            calls: Arc::new(AtomicUsize::new(0)),
            fail: true,
        });
        let mt = MultiTransport::new(vec![fail_a, fail_b]);

        let err = mt.publish(&rec()).await.unwrap_err();
        assert!(matches!(err, crate::ipns::IpnsError::Transport(_)));
    }

    struct HealthTransport {
        health: TransportHealth,
    }

    #[async_trait]
    impl IpnTransport for HealthTransport {
        fn name(&self) -> &'static str {
            "health"
        }

        async fn publish(&self, _: &IpnRecord) -> Result<(), crate::ipns::IpnsError> {
            Ok(())
        }

        async fn subscribe(&self, _: &str) -> Result<IpnRecordStream, crate::ipns::IpnsError> {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn health(&self) -> Result<TransportHealth, crate::ipns::IpnsError> {
            Ok(self.health)
        }
    }

    #[tokio::test]
    async fn multi_transport_health_any_healthy() {
        let mt = MultiTransport::new(vec![
            Arc::new(HealthTransport { health: TransportHealth::Down }),
            Arc::new(HealthTransport { health: TransportHealth::Healthy }),
            Arc::new(HealthTransport { health: TransportHealth::Down }),
        ]);
        assert_eq!(mt.health().await.unwrap(), TransportHealth::Healthy);
    }

    #[tokio::test]
    async fn multi_transport_health_degraded_no_healthy() {
        let mt = MultiTransport::new(vec![
            Arc::new(HealthTransport { health: TransportHealth::Degraded }),
            Arc::new(HealthTransport { health: TransportHealth::Down }),
        ]);
        assert_eq!(mt.health().await.unwrap(), TransportHealth::Degraded);
    }

    #[tokio::test]
    async fn multi_transport_health_down_only() {
        let mt = MultiTransport::new(vec![
            Arc::new(HealthTransport { health: TransportHealth::Down }),
            Arc::new(HealthTransport { health: TransportHealth::Down }),
        ]);
        assert_eq!(mt.health().await.unwrap(), TransportHealth::Down);
    }

    #[tokio::test]
    async fn multi_transport_health_unknown_when_no_healthy_or_degraded() {
        let mt = MultiTransport::new(vec![
            Arc::new(HealthTransport { health: TransportHealth::Unknown }),
            Arc::new(HealthTransport { health: TransportHealth::Down }),
        ]);
        assert_eq!(mt.health().await.unwrap(), TransportHealth::Unknown);
    }

    #[tokio::test]
    async fn multi_transport_disk_journal_runs_last() {
        use crate::transport::disk::DiskJournalTransport;
        use std::sync::Mutex;

        let dir = tempfile::TempDir::new().unwrap();
        let disk: Arc<dyn IpnTransport> =
            Arc::new(DiskJournalTransport::new(dir.path().to_path_buf()));

        let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

        struct LoggingTransport {
            name: &'static str,
            log: Arc<Mutex<Vec<&'static str>>>,
        }
        #[async_trait]
        impl IpnTransport for LoggingTransport {
            fn name(&self) -> &'static str {
                self.name
            }
            async fn publish(&self, _: &IpnRecord) -> Result<(), crate::ipns::IpnsError> {
                self.log.lock().unwrap().push(self.name);
                tokio::time::sleep(Duration::from_millis(5)).await;
                Ok(())
            }
            async fn subscribe(&self, _: &str) -> Result<IpnRecordStream, crate::ipns::IpnsError> {
                Ok(Box::pin(futures::stream::empty()))
            }
        }

        let net = Arc::new(LoggingTransport {
            name: "network",
            log: log.clone(),
        });

        let mt = MultiTransport::new(vec![net, disk]);
        mt.publish(&rec()).await.unwrap();

        let order = log.lock().unwrap();
        assert_eq!(order.last(), Some(&"network"));
    }
}

// ============================================================================
// TESTS FOR transport/dht.rs
// ============================================================================

#[cfg(feature = "dht")]
mod dht_transport_tests {
    use crate::ipns::{Ed25519SecretKey, IpnPublisher, IpnRecord};
    use crate::transport::dht::{
        decode_ipns_record, encode_ipns_record, publish_with_retries,
        DhtIpnTransport,
    };
    use crate::transport::{IpnTransport, TransportHealth};
    use async_trait::async_trait;
    use futures::StreamExt;
    use std::sync::Arc;
    use std::time::Duration;

    fn key_for_name(name: &str) -> a3net_dht::record::DhtKey {
        DhtIpnTransport::key_for_name(name)
    }

    fn signed_record(name: &str, value: &str) -> IpnRecord {
        let key = Ed25519SecretKey::generate();
        let mut r = IpnRecord::new(name.to_string(), value.to_string(), Duration::from_secs(60));
        r.sign(&key).expect("sign");
        r
    }

    #[test]
    fn encode_ipns_record_includes_tag() {
        let rec = signed_record("encode", "v");
        let payload = encode_ipns_record(&rec).expect("encode");
        assert_eq!(payload[0], 0x01);
    }

    #[test]
    fn decode_ipns_record_rejects_empty() {
        let err = decode_ipns_record("name", &[]).unwrap_err();
        assert!(matches!(err, crate::ipns::IpnsError::Transport(_)));
    }

    #[test]
    fn decode_ipns_record_rejects_wrong_tag() {
        let payload = vec![0x99, 1, 2, 3];
        let err = decode_ipns_record("name", &payload).unwrap_err();
        assert!(matches!(err, crate::ipns::IpnsError::Transport(_)));
    }

    #[test]
    fn decode_ipns_record_rejects_invalid_json() {
        let mut payload = vec![0x01];
        payload.extend_from_slice(b"not-json");
        let err = decode_ipns_record("name", &payload).unwrap_err();
        assert!(matches!(err, crate::ipns::IpnsError::Transport(_)));
    }

    #[test]
    fn decode_ipns_record_rejects_name_mismatch() {
        let rec = signed_record("correct-name", "v");
        let payload = encode_ipns_record(&rec).expect("encode");
        let err = decode_ipns_record("wrong-name", &payload).unwrap_err();
        assert!(matches!(err, crate::ipns::IpnsError::Transport(_)));
    }

    #[test]
    fn key_for_name_deterministic() {
        let k1 = key_for_name("test-name");
        let k2 = key_for_name("test-name");
        assert_eq!(k1.as_bytes(), k2.as_bytes());
    }

    #[test]
    fn key_for_name_32_bytes() {
        let k = key_for_name("any-name");
        assert_eq!(k.as_bytes().len(), 32);
    }

    #[test]
    fn key_for_name_different_names_different_keys() {
        let k1 = key_for_name("name-a");
        let k2 = key_for_name("name-b");
        assert_ne!(k1.as_bytes(), k2.as_bytes());
    }

    #[test]
    fn dht_ipn_transport_new() {
        let store = a3net_dht::store::new_in_memory_store();
        let t = DhtIpnTransport::new(Arc::new(
            crate::transport::dht::LocalDhtBackend::new(store),
        ));
        assert_eq!(t.name(), "dht");
    }

    #[test]
    fn dht_ipn_transport_with_ttl() {
        let store = a3net_dht::store::new_in_memory_store();
        let t = DhtIpnTransport::with_ttl(
            Arc::new(crate::transport::dht::LocalDhtBackend::new(store)),
            Duration::from_secs(3600),
        );
        assert_eq!(t.ttl, Duration::from_secs(3600));
    }

    #[test]
    fn dht_ipn_transport_local() {
        let store = a3net_dht::store::new_in_memory_store();
        let t = DhtIpnTransport::local(store);
        assert_eq!(t.name(), "dht");
    }

    #[test]
    fn dht_ipn_transport_debug() {
        let store = a3net_dht::store::new_in_memory_store();
        let t = DhtIpnTransport::local(store);
        let debug_str = format!("{:?}", t);
        assert!(debug_str.contains("DhtIpnTransport"));
    }

    #[tokio::test]
    async fn dht_ipn_transport_publish_and_subscribe() {
        let store = a3net_dht::store::new_in_memory_store();
        let t = DhtIpnTransport::local(store);

        let key = Ed25519SecretKey::generate();
        let name = key.ipns_name();
        let publisher = IpnPublisher::new(Arc::new(key));
        let record = publisher
            .publish(&name, "/ipfs/QmPublish".into(), Duration::from_secs(60))
            .await
            .expect("publish");

        t.publish(&record).await.expect("publish");

        let mut s = t.subscribe(&name).await.expect("subscribe");
        let received = s.next().await.expect("record").expect("ok");
        assert_eq!(received.value, "/ipfs/QmPublish");
    }

    #[tokio::test]
    async fn dht_ipn_transport_subscribe_unknown_name() {
        let store = a3net_dht::store::new_in_memory_store();
        let t = DhtIpnTransport::local(store);

        let mut s = t.subscribe("unknown-name").await.expect("subscribe");
        assert!(s.next().await.is_none());
    }

    #[tokio::test]
    async fn dht_ipn_transport_health_degraded_with_no_peers() {
        let store = a3net_dht::store::new_in_memory_store();
        let t = DhtIpnTransport::local(store);

        let h = t.health().await.unwrap();
        assert_eq!(h, TransportHealth::Degraded);
    }

    #[tokio::test]
    async fn local_dht_backend_put_get() {
        let store = a3net_dht::store::new_in_memory_store();
        let backend = crate::transport::dht::LocalDhtBackend::new(store);

        let key = key_for_name("backend-test");
        backend
            .put_value(&key, vec![1, 2, 3], Duration::from_secs(60))
            .await;

        let value = backend.get_value(&key).await;
        assert_eq!(value, Some(vec![1, 2, 3]));
    }

    #[tokio::test]
    async fn local_dht_backend_get_unknown() {
        let store = a3net_dht::store::new_in_memory_store();
        let backend = crate::transport::dht::LocalDhtBackend::new(store);

        let key = key_for_name("unknown");
        let value = backend.get_value(&key).await;
        assert!(value.is_none());
    }

    #[tokio::test]
    async fn local_dht_backend_is_healthy() {
        let store = a3net_dht::store::new_in_memory_store();
        let backend = crate::transport::dht::LocalDhtBackend::new(store);
        assert!(backend.is_healthy().await);
    }

    #[tokio::test]
    async fn local_dht_backend_peer_count() {
        let store = a3net_dht::store::new_in_memory_store();
        let backend = crate::transport::dht::LocalDhtBackend::new(store);
        assert_eq!(backend.peer_count().await, 0);
    }

    #[tokio::test]
    async fn publish_with_retries_swallows_backend_errors() {
        struct FailingBackend;
        #[async_trait]
        impl crate::transport::dht::DhtBackend for FailingBackend {
            async fn put_value(&self, _: &a3net_dht::record::DhtKey, _: Vec<u8>, _: Duration) {}
            async fn get_value(&self, _: &a3net_dht::record::DhtKey) -> Option<Vec<u8>> {
                None
            }
            async fn is_healthy(&self) -> bool {
                false
            }
            async fn peer_count(&self) -> usize {
                0
            }
        }

        let backend: Arc<dyn crate::transport::dht::DhtBackend> = Arc::new(FailingBackend);
        let key = key_for_name("retry-test");
        let payload = encode_ipns_record(&signed_record("retry", "v")).unwrap();

        let result = publish_with_retries(&*backend, &key, payload, "retry").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn dht_transport_sequence_ordering() {
        let store = a3net_dht::store::new_in_memory_store();
        let t = DhtIpnTransport::local(store);

        let key = Ed25519SecretKey::generate();
        let name = key.ipns_name();
        let publisher = IpnPublisher::new(Arc::new(key));

        let v1 = publisher
            .publish(&name, "/ipfs/v1".into(), Duration::from_secs(60))
            .await
            .unwrap();
        t.publish(&v1).await.unwrap();

        let v2 = publisher
            .publish(&name, "/ipfs/v2".into(), Duration::from_secs(60))
            .await
            .unwrap();
        t.publish(&v2).await.unwrap();

        let mut s = t.subscribe(&name).await.unwrap();
        let first = s.next().await.unwrap().unwrap();
        let second = s.next().await.unwrap().unwrap();

        assert_eq!(first.value, "/ipfs/v1");
        assert_eq!(first.sequence, 1);
        assert_eq!(second.value, "/ipfs/v2");
        assert_eq!(second.sequence, 2);
    }
}

// ============================================================================
// TESTS FOR ipns.rs (additional coverage)
// ============================================================================

mod ipns_additional_tests {
    use crate::ipns::{
        Ed25519SecretKey, Ed25519Verifier, IpnPublisher, IpnRecord, IpnResolver, IpnsError,
        SecretKey, TrustLevel, Verifier,
    };
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn trust_level_variants() {
        assert_eq!(TrustLevel::Trusted, TrustLevel::Trusted);
        assert_eq!(TrustLevel::Network, TrustLevel::Network);
        assert_eq!(TrustLevel::Verified, TrustLevel::Verified);
    }

    #[test]
    fn trust_level_debug() {
        let tl = TrustLevel::Trusted;
        let debug_str = format!("{:?}", tl);
        assert!(debug_str.contains("Trusted"));
    }

    #[tokio::test]
    async fn ipn_resolver_cache_ttl() {
        let resolver = IpnResolver::new(Duration::from_secs(300));
        assert_eq!(resolver.cache_ttl(), Duration::from_secs(300));
    }

    #[tokio::test]
    async fn ipn_resolver_resolve_expired_returns_not_found() {
        let resolver = IpnResolver::new(Duration::from_secs(60));
        let mut record = IpnRecord::with_name_value("expired".into(), "v".into());
        record.expires = 0;
        record.created = 0;
        resolver.cache_record(record);

        let err = resolver.resolve("expired").await.unwrap_err();
        assert!(matches!(err, IpnsError::NotFound));
    }

    #[tokio::test]
    async fn ipn_resolver_cache_record_verified_trusted() {
        let resolver = IpnResolver::new(Duration::from_secs(60));
        let record = IpnRecord::with_name_value("trusted".into(), "v".into());
        resolver.cache_record_verified(record.clone(), TrustLevel::Trusted);

        let cached = resolver.get_cached("trusted").unwrap();
        assert_eq!(cached.value, "v");
    }

    #[tokio::test]
    async fn ipn_resolver_cache_record_verified_network_unsigned() {
        let resolver = IpnResolver::new(Duration::from_secs(60));
        let mut record = IpnRecord::with_name_value("network".into(), "v".into());
        record.signature.clear();

        resolver.cache_record_verified(record, TrustLevel::Network);
        assert!(resolver.get_cached("network").is_none());
    }

    #[tokio::test]
    async fn ipn_resolver_clear_expired() {
        let resolver = IpnResolver::new(Duration::from_secs(60));

        let fresh = IpnRecord::with_name_value("fresh".into(), "v".into());
        let mut expired = IpnRecord::with_name_value("expired".into(), "v".into());
        expired.expires = 0;
        expired.created = 0;

        resolver.cache_record(fresh);
        resolver.cache_record(expired);
        assert!(resolver.get_cached("fresh").is_some());
        assert!(resolver.get_cached("expired").is_none());

        resolver.clear_expired();
        assert!(resolver.get_cached("fresh").is_some());
        assert!(resolver.get_cached("expired").is_none());
    }

    #[tokio::test]
    async fn ipn_publisher_create_empty_namespace() {
        let key = Arc::new(Ed25519SecretKey::generate());
        let publisher = IpnPublisher::new(key.clone());

        let record = publisher
            .create_empty_namespace("empty-ns", Duration::from_secs(60))
            .await
            .unwrap();

        assert!(record.is_empty());
        assert!(!record.signature.is_empty());
    }

    #[tokio::test]
    async fn ipn_publisher_reserve_namespace() {
        let key = Arc::new(Ed25519SecretKey::generate());
        let publisher = IpnPublisher::new(key.clone());

        let record = publisher.reserve_namespace("reserved").unwrap();

        assert!(record.is_empty());
        assert!(!record.signature.is_empty());
        assert!(publisher.get_local("reserved").is_none());
    }

    #[tokio::test]
    async fn ipn_publisher_sign_record() {
        let key = Arc::new(Ed25519SecretKey::generate());
        let publisher = IpnPublisher::new(key.clone());

        let record = IpnRecord::with_name_value("sign-me".into(), "v".into());
        let signed = publisher.sign_record(record).unwrap();

        assert!(!signed.signature.is_empty());
        assert!(publisher.get_local("sign-me").is_none());
    }

    #[tokio::test]
    async fn ipn_publisher_list_local() {
        let key = Arc::new(Ed25519SecretKey::generate());
        let publisher = IpnPublisher::new(key.clone());

        publisher
            .publish("local-a", "v1".into(), Duration::from_secs(60))
            .await
            .unwrap();
        publisher
            .publish("local-b", "v2".into(), Duration::from_secs(60))
            .await
            .unwrap();

        let locals = publisher.list_local();
        assert_eq!(locals.len(), 2);
    }

    #[tokio::test]
    async fn ipn_publisher_list_all_local() {
        let key = Arc::new(Ed25519SecretKey::generate());
        let publisher = IpnPublisher::new(key.clone());

        publisher
            .publish("all-a", "v1".into(), Duration::from_secs(60))
            .await
            .unwrap();
        publisher
            .publish("all-b", "v2".into(), Duration::from_secs(60))
            .await
            .unwrap();

        let all = publisher.list_all_local();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn ipn_publisher_get_or_create_new() {
        let key = Arc::new(Ed25519SecretKey::generate());
        let publisher = IpnPublisher::new(key.clone());

        let record = publisher.get_or_create("new-record", Duration::from_secs(60)).unwrap();
        assert!(record.is_empty());
        assert_eq!(record.name, "new-record");
    }

    #[tokio::test]
    async fn ipn_publisher_get_or_create_existing() {
        let key = Arc::new(Ed25519SecretKey::generate());
        let publisher = IpnPublisher::new(key.clone());

        let first = publisher.get_or_create("existing", Duration::from_secs(60)).unwrap();
        let second = publisher.get_or_create("existing", Duration::from_secs(120)).unwrap();

        assert_eq!(first.sequence, second.sequence);
    }

    #[test]
    fn ipns_error_display() {
        let err = IpnsError::InvalidSignature;
        assert_eq!(err.to_string(), "Signature verification failed");

        let err = IpnsError::Expired;
        assert_eq!(err.to_string(), "Record expired");

        let err = IpnsError::NotAuthorized;
        assert_eq!(err.to_string(), "Not authorized");

        let err = IpnsError::NotFound;
        assert_eq!(err.to_string(), "Record not found");

        let err = IpnsError::Unavailable;
        assert_eq!(err.to_string(), "transport unavailable");
    }

    #[test]
    fn ipn_record_update_preserves_ttl() {
        let mut record = IpnRecord::with_name_value("test".into(), "v1".into());
        let original_ttl = record.ttl_secs;

        record.update("v2".into());
        assert_eq!(record.ttl_secs, original_ttl);
    }

    #[test]
    fn ipn_record_patch_value() {
        let mut record = IpnRecord::with_name_value("patch".into(), "v1".into());
        let original_seq = record.sequence;

        record.patch_value("v2".into());
        assert_eq!(record.sequence, original_seq);
        assert_eq!(record.value, "v2");
    }

    #[test]
    fn ipn_record_set_ttl() {
        let mut record = IpnRecord::with_name_value("ttl".into(), "v".into());
        record.set_ttl(Duration::from_secs(7200));
        assert_eq!(record.ttl_secs, 7200);
        assert_eq!(record.validity_offset, 7200);
    }

    #[test]
    fn ipn_record_is_expired() {
        let mut record = IpnRecord::with_name_value("expiry".into(), "v".into());
        assert!(!record.is_expired());

        record.expires = 0;
        assert!(record.is_expired());
    }

    #[test]
    fn ipn_record_is_newer_than() {
        let mut a = IpnRecord::with_name_value("k".into(), "a".into());
        let mut b = IpnRecord::with_name_value("k".into(), "b".into());

        a.sequence = 1;
        b.sequence = 2;
        assert!(b.is_newer_than(&a));

        a.sequence = 2;
        b.sequence = 2;
        b.created = a.created + 1;
        assert!(b.is_newer_than(&a));
    }

    #[test]
    fn ipn_record_sign_and_verify() {
        let secret = Ed25519SecretKey::generate();
        let pk: [u8; 32] = secret.public_key_bytes().as_slice().try_into().unwrap();
        let verifier = Ed25519Verifier::from_bytes(&pk).unwrap();

        let mut record = IpnRecord::with_name_value("sign-test".into(), "v".into());
        record.sign(&secret).unwrap();

        assert!(record.verify_signature(&verifier));
    }

    #[test]
    fn ipn_record_verify_rejects_wrong_key() {
        let secret1 = Ed25519SecretKey::generate();
        let secret2 = Ed25519SecretKey::generate();
        let pk2: [u8; 32] = secret2.public_key_bytes().as_slice().try_into().unwrap();
        let verifier2 = Ed25519Verifier::from_bytes(&pk2).unwrap();

        let mut record = IpnRecord::with_name_value("wrong-key".into(), "v".into());
        record.sign(&secret1).unwrap();

        assert!(!record.verify_signature(&verifier2));
    }

    #[test]
    fn ipn_record_to_bytes_roundtrip() {
        let secret = Ed25519SecretKey::generate();
        let mut record = IpnRecord::with_name_value("bytes-test".into(), "v".into());
        record.sign(&secret).unwrap();

        let bytes = record.to_bytes();
        let decoded = IpnRecord::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.name, record.name);
        assert_eq!(decoded.value, record.value);
        assert_eq!(decoded.sequence, record.sequence);
    }

    #[test]
    fn ipn_record_from_bytes_invalid() {
        let err = IpnRecord::from_bytes(b"invalid").unwrap_err();
        assert!(matches!(err, IpnsError::Deserialize(_)));
    }

    #[test]
    fn ipn_record_to_json_roundtrip() {
        let secret = Ed25519SecretKey::generate();
        let mut record = IpnRecord::with_name_value("json-test".into(), "v".into());
        record.sign(&secret).unwrap();

        let json = record.to_json().unwrap();
        let decoded = IpnRecord::from_json(&json).unwrap();

        assert_eq!(decoded.name, record.name);
        assert_eq!(decoded.value, record.value);
    }

    #[test]
    fn ipn_record_from_json_invalid() {
        let err = IpnRecord::from_json("not json").unwrap_err();
        assert!(matches!(err, IpnsError::Deserialize(_)));
    }

    #[test]
    fn ipn_record_is_empty() {
        let empty = IpnRecord::new_empty("empty".into(), Duration::from_secs(60));
        assert!(empty.is_empty());

        let non_empty = IpnRecord::with_name_value("non-empty".into(), "v".into());
        assert!(!non_empty.is_empty());
    }

    #[test]
    fn ipn_record_ipns_name_formatted() {
        let mut record = IpnRecord::with_name_value("k51test123".into(), "v".into());
        assert_eq!(record.ipns_name_formatted(), "k51test123");

        // 64-char hex string should get k51 prefix
        let hex_name = "a".repeat(64);
        record.name = hex_name.clone();
        assert_eq!(record.ipns_name_formatted(), format!("k51{}", hex_name));

        // Other names stay as-is
        record.name = "other".to_string();
        assert_eq!(record.ipns_name_formatted(), "other");
    }

    #[test]
    fn ed25519_secret_key_generate() {
        let key1 = Ed25519SecretKey::generate();
        let key2 = Ed25519SecretKey::generate();
        assert_ne!(key1.to_bytes(), key2.to_bytes());
    }

    #[test]
    fn ed25519_secret_key_to_from_bytes() {
        let original = Ed25519SecretKey::generate();
        let bytes = original.to_bytes();
        let restored = Ed25519SecretKey::from_bytes(&bytes).unwrap();
        assert_eq!(original.to_bytes(), restored.to_bytes());
        assert_eq!(original.public_key_bytes(), restored.public_key_bytes());
    }

    #[test]
    fn ed25519_secret_key_ipns_name() {
        let key = Ed25519SecretKey::generate();
        let name = key.ipns_name();
        assert_eq!(name.len(), 64);
        assert!(name.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn ed25519_verifier_from_bytes() {
        let key = Ed25519SecretKey::generate();
        let pk: [u8; 32] = key.public_key_bytes().as_slice().try_into().unwrap();
        let verifier = Ed25519Verifier::from_bytes(&pk).unwrap();
        let signature = key.sign(b"test");
        assert!(verifier.verify(b"test", &signature));
    }
}
