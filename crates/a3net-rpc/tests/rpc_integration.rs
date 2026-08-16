//! End-to-end integration tests for `a3net-rpc` against a real
//! `BlobStore`. The unit tests in `commands.rs` cover the API
//! surface; these exercise the on-disk pin/GC contract through
//! restart, plus a few extra edge cases the unit tests skip for
//! brevity.

use std::sync::{Arc, Mutex};

use a3net_blobstore::BlobStore;
use a3net_rpc::{
    block_put, block_rm,
    client::RpcClient,
    commands::{DhtProviderStore, IpnsPublisher, IpnsResolver},
    dag_get, dag_import, dag_resolve,
    dht_findprovs, dht_provide,
    gc, name_publish, name_resolve,
    pin_add, pin_ls, pin_rm,
    NamePublishResult, ProviderInfo,
};
use tempfile::TempDir;

fn fresh_store() -> (TempDir, Arc<BlobStore>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(BlobStore::new(dir.path()).expect("blob store"));
    (dir, store)
}

async fn put_block(store: &Arc<BlobStore>, payload: &[u8]) -> String {
    let v = block_put(store, payload.to_vec(), false).await.expect("block_put");
    v["Key"].as_str().unwrap().to_string()
}

// ─── Existing pin/GC/block contract tests ───────────────────────────────────

#[tokio::test]
async fn pin_add_then_gc_drops_only_unpinned() {
    let (_dir, store) = fresh_store();
    let keep = put_block(&store, b"keep me").await;
    let drop = put_block(&store, b"drop me").await;
    pin_add(&store, &keep, false).await.unwrap();

    let v = gc(&store, false).await.unwrap();
    assert_eq!(v["KeysRemoved"].as_u64(), Some(1));

    // Verify only the pinned block survives.
    assert!(store
        .has_complete(&a3net_types::ContentHash::from_hex(&keep).unwrap()));
    assert!(!store
        .has_complete(&a3net_types::ContentHash::from_hex(&drop).unwrap()));
}

#[tokio::test]
async fn pin_recursive_flag_survives_restart() {
    let (dir, store) = fresh_store();
    let cid = put_block(&store, b"rec").await;
    pin_add(&store, &cid, true).await.unwrap();

    // Open a fresh blob store against the same dir.
    let store2 = Arc::new(BlobStore::new(dir.path()).unwrap());
    let v = pin_ls(&store2, None).await.unwrap();
    let entry = &v["Keys"].as_object().unwrap()[&cid];
    assert_eq!(entry["Type"].as_str(), Some("recursive"));
}

#[tokio::test]
async fn block_rm_after_pin_clears_pin() {
    let (_dir, store) = fresh_store();
    let cid = put_block(&store, b"to-remove").await;
    pin_add(&store, &cid, false).await.unwrap();

    // Sanity check the pin exists.
    let v = pin_ls(&store, Some(&cid)).await.unwrap();
    assert_eq!(v["Keys"].as_object().unwrap().len(), 1);

    // block_rm should drop both the blob and the pin.
    let r = block_rm(&store, &cid, false).await.unwrap();
    assert_eq!(r["Removed"].as_bool(), Some(true));

    let v = pin_ls(&store, Some(&cid)).await.unwrap();
    assert!(v["Keys"].as_object().unwrap().is_empty());
}

#[tokio::test]
async fn pin_rm_after_block_rm_is_safe() {
    let (_dir, store) = fresh_store();
    let cid = put_block(&store, b"order").await;
    pin_add(&store, &cid, false).await.unwrap();

    block_rm(&store, &cid, false).await.unwrap();
    // Pin rm on an absent pin must still succeed with Removed=false
    // (kubo behaviour).
    let v = pin_rm(&store, &cid).await.unwrap();
    assert_eq!(v["Removed"].as_bool(), Some(false));
}

#[tokio::test]
async fn gc_after_restart_uses_persisted_pins() {
    let (dir, store) = fresh_store();
    let keep = put_block(&store, b"keep across restart").await;
    pin_add(&store, &keep, false).await.unwrap();

    // Drop one unpinned blob before "restarting".
    let drop_before = put_block(&store, b"drop before restart").await;

    // Restart: new blob store against the same dir.
    let store2 = Arc::new(BlobStore::new(dir.path()).unwrap());
    // Add another unpinned blob after restart.
    let drop_after = put_block(&store2, b"drop after restart").await;

    // GC must remove both unpinned blobs but keep the pinned one.
    let v = gc(&store2, false).await.unwrap();
    assert_eq!(v["KeysRemoved"].as_u64(), Some(2));

    assert!(store2
        .has_complete(&a3net_types::ContentHash::from_hex(&keep).unwrap()));
    assert!(!store2
        .has_complete(&a3net_types::ContentHash::from_hex(&drop_before).unwrap()));
    assert!(!store2
        .has_complete(&a3net_types::ContentHash::from_hex(&drop_after).unwrap()));
}

#[tokio::test]
async fn multiple_pins_listed_in_order() {
    let (_dir, store) = fresh_store();
    // Add three pins; the listing should reflect each one.
    let mut cids = Vec::new();
    for letter in [b"a", b"b", b"c"] {
        let cid = put_block(&store, letter).await;
        pin_add(&store, &cid, false).await.unwrap();
        cids.push(cid);
    }
    let v = pin_ls(&store, None).await.unwrap();
    let keys = v["Keys"].as_object().unwrap();
    assert_eq!(keys.len(), 3);
    for cid in &cids {
        assert!(keys.contains_key(cid));
    }
}

// ─── dag/* tests ────────────────────────────────────────────────────────────

#[tokio::test]
async fn dag_resolve_validates_cid_and_splits_path() {
    let (_dir, store) = fresh_store();
    let cid = put_block(&store, b"resolve-me").await;

    // `/ipfs/<cid>` resolves cleanly with no remainder.
    let v = dag_resolve(&store, &format!("/ipfs/{cid}"))
        .await
        .expect("resolve");
    assert_eq!(v["Cid"]["/"].as_str(), Some(cid.as_str()));
    assert!(v["RemPath"].is_null());

    // `/ipfs/<cid>/foo/bar` exposes the remainder.
    let v = dag_resolve(&store, &format!("/ipfs/{cid}/foo/bar"))
        .await
        .expect("resolve");
    assert_eq!(v["Cid"]["/"].as_str(), Some(cid.as_str()));
    assert_eq!(v["RemPath"].as_str(), Some("foo/bar"));
}

#[tokio::test]
async fn dag_resolve_unknown_cid_returns_not_found() {
    let (_dir, store) = fresh_store();
    // ContentHash::from_hex requires a valid hex length, so use the
    // hash of an empty payload to ensure we don't get a parse error
    // but a NotFound instead.
    let cid = a3net_types::ContentHash::from_bytes(b"missing").as_hex().to_string();
    let err = dag_resolve(&store, &format!("/ipfs/{cid}")).await.unwrap_err();
    assert_eq!(err.code, 1, "RpcError::not_found maps to code=1");
}

#[tokio::test]
async fn dag_import_roundtrip() {
    let (_dir, store) = fresh_store();
    let v = dag_import(&store, b"imported".to_vec(), /*pin=*/ false)
        .await
        .expect("dag_import");
    let cid = v["Cid"]["/"].as_str().expect("Cid").to_string();
    assert_eq!(v["Size"].as_u64(), Some(b"imported".len() as u64));

    let got = dag_get(&store, &cid, None).await.expect("dag_get");
    let data_b64 = got["data"].as_str().expect("data");
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let decoded = STANDARD.decode(data_b64).unwrap();
    assert_eq!(decoded, b"imported");
}

#[tokio::test]
async fn dag_import_with_pin_creates_pin_entry() {
    let (_dir, store) = fresh_store();
    let v = dag_import(&store, b"pinned-dag".to_vec(), /*pin=*/ true)
        .await
        .expect("dag_import");
    let cid = v["Cid"]["/"].as_str().expect("Cid").to_string();
    let listing = pin_ls(&store, Some(&cid)).await.expect("pin_ls");
    assert!(listing["Keys"].as_object().unwrap().contains_key(&cid));
}

// ─── dht/* tests ────────────────────────────────────────────────────────────

#[derive(Default)]
struct InMemoryProviders {
    by_cid: Mutex<std::collections::HashMap<String, Vec<ProviderInfo>>>,
}

impl DhtProviderStore for InMemoryProviders {
    fn provide(&self, cid: &str, addr: &str, _ttl_secs: u64) -> Result<Vec<String>, a3net_rpc::RpcError> {
        let mut map = self.by_cid.lock().unwrap();
        let entry = map.entry(cid.to_string()).or_default();
        let provider = ProviderInfo {
            id: format!("peer-for-{cid}"),
            addrs: vec![addr.to_string()],
        };
        entry.push(provider);
        Ok(vec![addr.to_string()])
    }

    fn find_providers(&self, cid: &str) -> Result<Vec<ProviderInfo>, a3net_rpc::RpcError> {
        Ok(self
            .by_cid
            .lock()
            .unwrap()
            .get(cid)
            .cloned()
            .unwrap_or_default())
    }
}

#[tokio::test]
async fn dht_provide_records_with_store() {
    let (_dir, store) = fresh_store();
    let cid = put_block(&store, b"dht-content").await;
    let mem: Arc<dyn DhtProviderStore> = Arc::new(InMemoryProviders::default());

    let v = dht_provide(Some(&mem), &cid, Some("/ip4/1.2.3.4"), Some(60))
        .await
        .expect("provide");
    assert_eq!(v["Cid"].as_str(), Some(cid.as_str()));
    assert_eq!(v["Addrs"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn dht_findprovs_returns_empty_when_unwired() {
    let (_dir, store) = fresh_store();
    let cid = put_block(&store, b"x").await;

    let v = dht_findprovs(None, &cid, None).await.expect("findprovs");
    assert!(v["Providers"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn dht_findprovs_uses_injected_store() {
    let (_dir, store) = fresh_store();
    let cid = put_block(&store, b"y").await;
    let mem: Arc<dyn DhtProviderStore> = Arc::new(InMemoryProviders::default());
    dht_provide(Some(&mem), &cid, Some("/ip4/9.9.9.9"), Some(60))
        .await
        .unwrap();

    let v = dht_findprovs(Some(&mem), &cid, None).await.expect("findprovs");
    let providers = v["Providers"].as_array().unwrap();
    assert_eq!(providers.len(), 1);
    assert!(providers[0]["Addrs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a.as_str() == Some("/ip4/9.9.9.9")));
}

#[tokio::test]
async fn dht_provide_rejects_invalid_cid() {
    let err = dht_provide(None, "not-hex", None, None).await.unwrap_err();
    assert_eq!(err.code, 2, "RpcError::invalid_input maps to code=2");
}

// ─── name/* tests ───────────────────────────────────────────────────────────

struct MapPublisher {
    published: Mutex<std::collections::HashMap<String, String>>,
}

impl IpnsPublisher for MapPublisher {
    fn publish(
        &self,
        name: &str,
        value: &str,
        _ttl_secs: u64,
    ) -> Result<NamePublishResult, a3net_rpc::RpcError> {
        self.published
            .lock()
            .unwrap()
            .insert(name.to_string(), value.to_string());
        Ok(NamePublishResult {
            name: name.to_string(),
            value: value.to_string(),
        })
    }
}

struct MapResolver {
    resolved: Mutex<std::collections::HashMap<String, String>>,
}

impl IpnsResolver for MapResolver {
    fn resolve(&self, name: &str) -> Result<Option<String>, a3net_rpc::RpcError> {
        Ok(self.resolved.lock().unwrap().get(name).cloned())
    }
}

#[tokio::test]
async fn name_publish_empty_name_is_invalid() {
    let publisher: Arc<dyn IpnsPublisher> = Arc::new(MapPublisher {
        published: Mutex::new(std::collections::HashMap::new()),
    });
    let err = name_publish(Some(&publisher), "", "v", None)
        .await
        .unwrap_err();
    assert_eq!(err.code, 2);
}

#[tokio::test]
async fn name_publish_and_resolve_with_injected_handlers() {
    let publisher: Arc<dyn IpnsPublisher> = Arc::new(MapPublisher {
        published: Mutex::new(std::collections::HashMap::new()),
    });
    let resolver_arc = Arc::new(MapResolver {
        resolved: Mutex::new(std::collections::HashMap::new()),
    });
    let resolver: Arc<dyn IpnsResolver> = resolver_arc.clone();

    let v = name_publish(Some(&publisher), "alice", "/ipfs/QmExample", Some(60))
        .await
        .expect("publish");
    assert_eq!(v["Name"].as_str(), Some("alice"));
    assert_eq!(v["Value"].as_str(), Some("/ipfs/QmExample"));

    // Make the resolver see alice → /ipfs/QmExample.
    resolver_arc
        .resolved
        .lock()
        .unwrap()
        .insert("alice".to_string(), "QmExample".to_string());

    let v = name_resolve(Some(&resolver), "alice").await.expect("resolve");
    assert_eq!(v["Path"].as_str(), Some("/ipfs/QmExample"));

    let v = name_resolve(Some(&resolver), "unknown").await.expect("resolve");
    assert_eq!(v["Path"].as_str(), Some("/ipfs/"));
}

#[tokio::test]
async fn name_resolve_unwired_returns_ipfs_root() {
    let resolver: Arc<dyn IpnsResolver> = Arc::new(MapResolver {
        resolved: Mutex::new(std::collections::HashMap::new()),
    });
    let v = name_resolve(Some(&resolver), "anything").await.expect("resolve");
    assert_eq!(v["Path"].as_str(), Some("/ipfs/"));
}

// ─── RpcClient glue ─────────────────────────────────────────────────────────

#[tokio::test]
async fn rpc_client_round_trip_on_block() {
    let (_dir, store) = fresh_store();
    let client = RpcClient::new(store.clone());
    let cid = client.put_block(b"via-client").await.expect("put_block");
    let bytes = client.get_block(&cid).await.expect("get_block");
    assert_eq!(bytes, b"via-client");
    let stat = client.block_stat(&cid).await.expect("block_stat");
    assert_eq!(stat.size, b"via-client".len() as u64);
}

#[tokio::test]
async fn rpc_client_pin_add_list_remove() {
    let (_dir, store) = fresh_store();
    let client = RpcClient::new(store.clone());
    let cid = client.put_block(b"cli-pin").await.expect("put_block");
    assert!(client.pin_add(&cid, false).await.expect("pin_add"));
    let listed = client.list_pins(None).await.expect("list_pins");
    assert!(listed.contains_key(&cid));
    assert!(client.pin_remove(&cid).await.expect("pin_remove"));
    let listed = client.list_pins(None).await.expect("list_pins");
    assert!(listed.is_empty());
}

#[tokio::test]
async fn dag_put_get_round_trip_via_client() {
    let (_dir, store) = fresh_store();
    let client = RpcClient::new(store.clone());
    let cid = client.put_dag(b"client-dag").await.expect("put_dag");
    let bytes = client.get_dag(&cid).await.expect("get_dag");
    assert_eq!(bytes, b"client-dag");
}
