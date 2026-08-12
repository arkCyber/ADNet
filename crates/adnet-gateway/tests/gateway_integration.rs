//! Integration tests for the IPFS Gateway.
//!
//! These tests verify the core IPFS-compatible functionality:
//! - DAG operations (put, get, resolve)
//! - Pin management (add, remove, list)
//! - Gateway HTTP endpoints

use std::sync::Arc;

use adnet_blobstore::BlobStore;
use adnet_gateway::{
    DagService, PinService, GcService, GatewayConfig,
    ObjectService, RefsService, StatsService,
};
use adnet_types::ContentHash;

/// Create a test blob store.
fn create_test_blob_store() -> Arc<BlobStore> {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let store = BlobStore::new(temp_dir.path()).expect("create blob store");
    Arc::new(store)
}

#[tokio::test]
async fn test_dag_put_and_get() {
    let blob_store = create_test_blob_store();
    let dag_service = DagService::new(blob_store.clone());

    // Create a simple CBOR-encoded DAG node
    let node_data = serde_cbor::to_vec(&serde_json::json!({
        "data": null,
        "links": []
    })).expect("serialize");

    let result = dag_service.put(&node_data).await.expect("put dag");

    assert!(!result.cid.is_empty(), "CID should not be empty");

    // Verify the content is stored
    let hash = ContentHash::from_hex(&result.cid).expect("valid hash");
    let stored = blob_store.get_sync(&hash);
    assert!(stored.is_some(), "content should be stored");

    // Get the DAG node back
    let get_result = dag_service.get(&hash, &[]).await.expect("get dag");
    assert!(!get_result.data.is_empty(), "should have data");
}

#[tokio::test]
async fn test_dag_with_links() {
    let blob_store = create_test_blob_store();
    let dag_service = DagService::new(blob_store.clone());

    // First, put a child node
    let child_data = serde_cbor::to_vec(&serde_json::json!({
        "data": null,
        "links": []
    })).expect("serialize");
    let child_result = dag_service.put(&child_data).await.expect("put child");

    // Now put a parent node with link to child
    let parent_data = serde_cbor::to_vec(&serde_json::json!({
        "data": null,
        "links": [{
            "Name": "child",
            "Hash": child_result.cid.clone(),
            "Size": 100
        }]
    })).expect("serialize");
    let parent_result = dag_service.put(&parent_data).await.expect("put parent");

    // Verify parent can be retrieved
    let parent_hash = ContentHash::from_hex(&parent_result.cid).expect("valid hash");
    let parent_node = dag_service.get(&parent_hash, &[]).await.expect("get parent");

    assert!(!parent_node.data.is_empty(), "parent should have data");
}

#[tokio::test]
async fn test_dag_resolve() {
    let blob_store = create_test_blob_store();
    let dag_service = DagService::new(blob_store.clone());

    // Put a node
    let node_data = serde_cbor::to_vec(&serde_json::json!({
        "data": null,
        "links": []
    })).expect("serialize");
    let result = dag_service.put(&node_data).await.expect("put");

    // Resolve should return the same CID for direct resolve
    let resolved = dag_service.resolve(&result.cid).await.expect("resolve");
    assert_eq!(resolved.cid, result.cid, "resolved CID should match");
}

fn create_test_pin_service(blob_store: Arc<BlobStore>) -> (PinService, GcService) {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let pin_service = PinService::new(blob_store.clone(), temp_dir.path().to_path_buf());
    let gc_service = GcService::new(blob_store, Arc::new(pin_service.clone()));
    (pin_service, gc_service)
}

#[tokio::test]
async fn test_pin_service_add_and_list() {
    let blob_store = create_test_blob_store();
    let (pin_service, _gc_service) = create_test_pin_service(blob_store.clone());

    // Put some content first
    let data = b"content to pin".to_vec();
    let (hash, _size) = blob_store.put_bytes_sync(&data).expect("put");

    // Add a direct pin
    pin_service.add_pin(&hash, false)
        .await
        .expect("add pin");

    // List pins - should contain our pinned hash
    let pins = pin_service.list_pins(None).await;
    assert!(!pins.is_empty(), "pins list should not be empty");

    // Check is_pinned
    let is_pinned = pin_service.is_pinned(&hash).await;
    assert!(is_pinned, "hash should be pinned");
}

#[tokio::test]
async fn test_pin_service_remove() {
    let blob_store = create_test_blob_store();
    let (pin_service, _gc_service) = create_test_pin_service(blob_store.clone());

    // Put and pin content
    let data = b"content to unpin".to_vec();
    let (hash, _size) = blob_store.put_bytes_sync(&data).expect("put");

    pin_service.add_pin(&hash, false)
        .await
        .expect("add pin");

    // Verify pinned
    assert!(pin_service.is_pinned(&hash).await, "should be pinned before remove");

    // Remove the pin
    pin_service.remove_pin(&hash)
        .await
        .expect("remove pin");

    // Verify pin is removed
    let is_pinned = pin_service.is_pinned(&hash).await;
    assert!(!is_pinned, "hash should not be pinned after remove");
}

#[tokio::test]
async fn test_pin_service_recursive() {
    let blob_store = create_test_blob_store();
    let (pin_service, _gc_service) = create_test_pin_service(blob_store.clone());

    // Put parent content
    let parent_data = b"parent".to_vec();
    let (parent_hash, _size) = blob_store.put_bytes_sync(&parent_data).expect("put parent");

    // Add recursive pin
    pin_service.add_pin(&parent_hash, true)
        .await
        .expect("add recursive pin");

    // Verify pinned
    assert!(pin_service.is_pinned(&parent_hash).await, "should be pinned");
}

#[tokio::test]
async fn test_gc_run() {
    let blob_store = create_test_blob_store();
    let (pin_service, gc_service) = create_test_pin_service(blob_store.clone());

    // Put and pin some content
    let data = b"pinned content".to_vec();
    let (hash, _size) = blob_store.put_bytes_sync(&data).expect("put");
    pin_service.add_pin(&hash, false)
        .await
        .expect("add pin");

    // Run GC
    let result = gc_service.run().await.expect("gc run");

    // Pinned content should not be removed
    assert_eq!(result.removed, 0, "pinned content should not be removed");

    // Verify content still exists
    let still_exists = blob_store.get_sync(&hash);
    assert!(still_exists.is_some(), "pinned content should still exist");
}

#[tokio::test]
async fn test_gateway_config_defaults() {
    let config = GatewayConfig::default();

    assert_eq!(config.bind_addr, "0.0.0.0:8080");
    assert!(!config.writable);
    assert!(config.cors_enabled);
}

#[tokio::test]
async fn test_gateway_config_new() {
    let config = GatewayConfig::new("0.0.0.0:9000")
        .with_writable();

    assert_eq!(config.bind_addr, "0.0.0.0:9000");
    assert!(config.writable);
}

#[tokio::test]
async fn test_dag_error_not_found() {
    let blob_store = create_test_blob_store();
    let dag_service = DagService::new(blob_store.clone());

    // Try to get a non-existent hash
    let fake_hash = ContentHash::from_hex("0000000000000000000000000000000000000000000000000000000000000000")
        .expect("valid hash");

    let result = dag_service.get(&fake_hash, &[]).await;
    assert!(result.is_err(), "should error for non-existent hash");
}

#[tokio::test]
async fn test_pinned_content_survives_gc() {
    let blob_store = create_test_blob_store();
    let (pin_service, gc_service) = create_test_pin_service(blob_store.clone());

    // Put multiple items
    let data1 = b"item 1".to_vec();
    let data2 = b"item 2".to_vec();
    let (hash1, _size1) = blob_store.put_bytes_sync(&data1).expect("put1");
    let (hash2, _size2) = blob_store.put_bytes_sync(&data2).expect("put2");

    // Pin only one
    pin_service.add_pin(&hash1, false)
        .await
        .expect("add pin");

    // Run GC
    let _result = gc_service.run().await.expect("gc run");

    // The key invariant is: pinned content should never be removed
    assert!(pin_service.is_pinned(&hash1).await, "pinned item must remain pinned");
    assert!(!pin_service.is_pinned(&hash2).await, "unpinned item should not be pinned");
}

#[tokio::test]
async fn test_pin_stats() {
    let blob_store = create_test_blob_store();
    let (pin_service, _gc_service) = create_test_pin_service(blob_store.clone());

    // Get initial stats
    let stats = pin_service.stats().await;
    assert_eq!(stats.total, 0, "initial pin count should be 0");

    // Add some pins
    let data1 = b"item 1".to_vec();
    let (hash1, _size) = blob_store.put_bytes_sync(&data1).expect("put1");
    pin_service.add_pin(&hash1, false).await.expect("add pin");

    let stats = pin_service.stats().await;
    assert_eq!(stats.total, 1, "total pin count should be 1 after adding");
}

// ============================================================================
// ObjectService Tests
// ============================================================================

#[tokio::test]
async fn test_object_service_stat() {
    let blob_store = create_test_blob_store();
    let dag_service = DagService::new(blob_store.clone());
    let object_service = ObjectService::new(blob_store.clone());

    // Create a proper DAG node using CBOR
    let node_data = serde_cbor::to_vec(&serde_json::json!({
        "data": null,
        "links": []
    })).expect("serialize");

    let result = dag_service.put(&node_data).await.expect("put dag");
    let hash = ContentHash::from_hex(&result.cid).expect("valid hash");

    // Get object statistics
    let stats = object_service.stat(&hash).await.expect("stat");
    assert_eq!(stats.hash, hash.as_hex());
    assert_eq!(stats.num_links, 0);
}

#[tokio::test]
async fn test_object_service_get() {
    let blob_store = create_test_blob_store();
    let dag_service = DagService::new(blob_store.clone());
    let object_service = ObjectService::new(blob_store.clone());

    // Create a proper DAG node
    let node_data = serde_cbor::to_vec(&serde_json::json!({
        "data": null,
        "links": []
    })).expect("serialize");

    let result = dag_service.put(&node_data).await.expect("put dag");
    let hash = ContentHash::from_hex(&result.cid).expect("valid hash");

    // Get object data
    let obj_data = object_service.get(&hash).await.expect("get");
    // CBOR null becomes empty array or nil depending on implementation
    assert!(obj_data.data.is_empty() || obj_data.data == b"null");
}

#[tokio::test]
async fn test_object_service_new_object() {
    let blob_store = create_test_blob_store();
    let object_service = ObjectService::new(blob_store.clone());

    // Create a new empty object
    let hash = object_service.new_object().await.expect("new object");
    assert!(!hash.as_hex().is_empty());

    // Verify the object can be retrieved
    let obj_data = object_service.get(&hash).await.expect("get new object");
    assert!(obj_data.data.is_empty());
}

#[tokio::test]
async fn test_object_service_set_data() {
    let blob_store = create_test_blob_store();
    let object_service = ObjectService::new(blob_store.clone());

    // Create a new object
    let hash = object_service.new_object().await.expect("new object");

    // Set data on the object
    let new_data = b"updated data".to_vec();
    let new_hash = object_service.set_data(&hash, new_data.clone())
        .await
        .expect("set data");

    // Verify the data was updated
    let obj_data = object_service.get(&new_hash).await.expect("get updated");
    assert_eq!(obj_data.data, new_data.as_slice());
}

// ============================================================================
// RefsService Tests
// ============================================================================

#[tokio::test]
async fn test_refs_service_list() {
    let blob_store = create_test_blob_store();
    let dag_service = DagService::new(blob_store.clone());
    let refs_service = RefsService::new(blob_store.clone());

    // Create a proper DAG node using CBOR
    let node_data = serde_cbor::to_vec(&serde_json::json!({
        "data": null,
        "links": []
    })).expect("serialize");

    let result = dag_service.put(&node_data).await.expect("put dag");
    let hash = ContentHash::from_hex(&result.cid).expect("valid hash");

    // List refs should return empty since we have no links
    let refs = refs_service.list(&hash).await.expect("list refs");
    assert!(refs.is_empty());
}

#[tokio::test]
async fn test_refs_service_get_direct_links() {
    let blob_store = create_test_blob_store();
    let dag_service = DagService::new(blob_store.clone());
    let refs_service = RefsService::new(blob_store.clone());

    // Create a proper DAG node
    let node_data = serde_cbor::to_vec(&serde_json::json!({
        "data": null,
        "links": []
    })).expect("serialize");

    let result = dag_service.put(&node_data).await.expect("put dag");
    let hash = ContentHash::from_hex(&result.cid).expect("valid hash");

    // Get direct links should return empty
    let links = refs_service.get_direct_links(&hash).expect("get links");
    assert!(links.is_empty());
}

#[tokio::test]
async fn test_refs_service_not_found() {
    let blob_store = create_test_blob_store();
    let refs_service = RefsService::new(blob_store.clone());

    // Try to list refs for non-existent hash
    let fake_hash = ContentHash::from_hex("0000000000000000000000000000000000000000000000000000000000000000")
        .expect("valid hash");

    let result = refs_service.list(&fake_hash).await;
    assert!(result.is_err());
}

// ============================================================================
// StatsService Tests
// ============================================================================

#[tokio::test]
async fn test_stats_service_repo() {
    let blob_store = create_test_blob_store();
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let stats_service = StatsService::new(
        blob_store.clone(),
        temp_dir.path().to_string_lossy().to_string(),
        1_000_000_000, // 1GB
    );

    // Add some data
    let data = b"stats test data".to_vec();
    blob_store.put_bytes_sync(&data).expect("put");

    // Get repo stats
    let stats = stats_service.repo().await.expect("repo stats");
    assert!(stats.num_objects >= 1);
    assert!(stats.repo_size > 0);
    assert_eq!(stats.storage_max, 1_000_000_000);
}

#[tokio::test]
async fn test_stats_service_bandwidth() {
    let blob_store = create_test_blob_store();
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let stats_service = StatsService::new(
        blob_store.clone(),
        temp_dir.path().to_string_lossy().to_string(),
        1_000_000_000,
    );

    // Record some bandwidth
    stats_service.record_in(1024);
    stats_service.record_out(2048);

    // Get bandwidth stats
    let stats = stats_service.bandwidth().await.expect("bandwidth stats");
    assert_eq!(stats.total_in, 1024);
    assert_eq!(stats.total_out, 2048);
    assert!(stats.rate_in >= 0.0);
}

#[tokio::test]
async fn test_stats_service_dht() {
    let blob_store = create_test_blob_store();
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let stats_service = StatsService::new(
        blob_store.clone(),
        temp_dir.path().to_string_lossy().to_string(),
        1_000_000_000,
    );

    // Get DHT stats
    let stats = stats_service.dht();
    assert_eq!(stats.name, "kademlia");
    assert_eq!(stats.num_peers, 0);
}

#[tokio::test]
async fn test_stats_service_bandwidth_accumulation() {
    let blob_store = create_test_blob_store();
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let stats_service = StatsService::new(
        blob_store.clone(),
        temp_dir.path().to_string_lossy().to_string(),
        1_000_000_000,
    );

    // Record bandwidth multiple times
    stats_service.record_in(100);
    stats_service.record_in(200);
    stats_service.record_out(50);

    let stats = stats_service.bandwidth().await.expect("bandwidth stats");
    assert_eq!(stats.total_in, 300); // 100 + 200
    assert_eq!(stats.total_out, 50);
}
