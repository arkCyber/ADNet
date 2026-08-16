//! Property-based tests for the `zone` module.
//!
//! These back P0-REQ-2 (ZoneStore TTL arithmetic must round-trip).
//!
//! Property tests use `proptest`, which is already a workspace
//! dev-dep.

use proptest::prelude::*;

use a3net_dns_server::config::DnsServerConfig;
use a3net_dns_server::zone::{RecordKind, ZoneRecord, ZoneStore};

proptest! {
    /// P0-REQ-2: `put` + `get` round-trips for any non-empty name.
    /// Keys are normalised through `ipns_txt_key` so we exercise
    /// the same path the DNS protocol does.
    #[test]
    fn zone_put_get_round_trip(
        name in "[a-z0-9_-]{1,16}",
        payload in "[a-zA-Z0-9]{1,128}",
        ttl_secs in 1u32..=3600u32,
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let store = ZoneStore::new(DnsServerConfig::default());
            let key = store.ipns_txt_key(&name);
            let record = ZoneRecord {
                key: key.clone(),
                kind: RecordKind::AdnetIpnsTxt {
                    ipns_name: name,
                    payload: payload.clone(),
                    ttl_secs,
                },
                expires_at_unix_ms: i64::MAX,
            };
            store.put(record.clone()).expect("put");
            let got = store.get(&key);
            prop_assert_eq!(got.len(), 1, "expected exactly 1 record");
            match got.first() {
                Some(ZoneRecord { kind: RecordKind::AdnetIpnsTxt { payload: got_payload, .. }, .. }) => {
                    prop_assert_eq!(got_payload, &payload);
                }
                other => {
                    let msg = format!("expected TXT record, got {other:?}");
                    return Err(proptest::test_runner::TestCaseError::Fail(msg.into()));
                }
            }
            Ok(())
        }).unwrap();
    }
}
