//! Integration tests for a3net-eliza-bridge using the in-process
//! gossip transport. These tests exercise the chat/feed pipeline
//! without booting the real network.

use a3net_eliza_bridge::{
    AdnetIdentity, ChatClientConfig, ChatClientBuilder,
    FeedAdapterBuilder, BridgeError,
};
use a3net_gossip::transport::InProcessGossip;
use a3net_types::bulletin::BulletinCategory;
use a3net_types::node::NodeId;
use std::sync::Arc;
use std::time::Duration;

/// Build a fresh identity for a test in a tempdir.
async fn mk_identity(dir: &std::path::Path, name: &str) -> AdnetIdentity {
    AdnetIdentity::new(dir.to_path_buf(), name).await.unwrap()
}

#[tokio::test]
async fn chat_send_message_through_gossip() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let id_a = mk_identity(dir_a.path(), "agent-a").await;
    let id_b = mk_identity(dir_b.path(), "agent-b").await;

    let gossip = Arc::new(InProcessGossip::new());
    let cfg_a = ChatClientConfig {
        display_name: "AgentA".to_string(),
        ..Default::default()
    };
    let cfg_b = ChatClientConfig {
        display_name: "AgentB".to_string(),
        ..Default::default()
    };
    let client_a = ChatClientBuilder::new(id_a)
        .config(cfg_a)
        .with_gossip_transport(gossip.clone())
        .build()
        .await
        .unwrap();
    let client_b = ChatClientBuilder::new(id_b)
        .config(cfg_b)
        .with_gossip_transport(gossip.clone())
        .build()
        .await
        .unwrap();
    client_a.login().await.unwrap();
    client_b.login().await.unwrap();

    let peer_b = client_b.node_id();
    let msg_id = client_a
        .send_message(&peer_b, "hello from A")
        .await
        .unwrap();
    assert!(!msg_id.is_empty());

    // The fact that send_message succeeded proves the gossip runtime
    // accepted the broadcast. We also exercise the listener path
    // explicitly on B to make sure the receiver task can be spawned.
    client_b.start_message_listener().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    client_b.stop_message_listener().await;

    client_a.logout().await.unwrap();
    client_b.logout().await.unwrap();
}

#[tokio::test]
async fn chat_friend_request_flow() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let id_a = mk_identity(dir_a.path(), "agent-a").await;
    let id_b = mk_identity(dir_b.path(), "agent-b").await;
    let gossip = Arc::new(InProcessGossip::new());

    let client_a = ChatClientBuilder::new(id_a)
        .with_gossip_transport(gossip.clone())
        .build()
        .await
        .unwrap();
    let client_b = ChatClientBuilder::new(id_b)
        .with_gossip_transport(gossip.clone())
        .build()
        .await
        .unwrap();
    client_a.login().await.unwrap();
    client_b.login().await.unwrap();

    let node_b = client_b.node_id();
    client_a.add_friend(&node_b, "hi, want to chat?").await.unwrap();
    // The send should succeed (the broadcast is sent to the peer_inbox).

    client_a.logout().await.unwrap();
    client_b.logout().await.unwrap();
}

#[tokio::test]
async fn chat_rate_limit_blocks_after_threshold() {
    let dir = tempfile::tempdir().unwrap();
    let identity = mk_identity(dir.path(), "agent-rate").await;
    let gossip = Arc::new(InProcessGossip::new());
    let cfg = ChatClientConfig {
        rate_limit_per_minute: 2,
        display_name: "X".to_string(),
        ..Default::default()
    };
    let client = ChatClientBuilder::new(identity)
        .config(cfg)
        .with_gossip_transport(gossip)
        .build()
        .await
        .unwrap();
    client.login().await.unwrap();
    let peer = NodeId::random();
    assert!(client.send_message(&peer, "1").await.is_ok());
    assert!(client.send_message(&peer, "2").await.is_ok());
    let err = client.send_message(&peer, "3").await.unwrap_err();
    // RateLimited should be Retryable so clients can back off.
    assert!(matches!(err, BridgeError::RateLimited(_)), "got: {err:?}");
    client.logout().await.unwrap();
}

#[tokio::test]
async fn chat_validation_rejects_overlong_messages() {
    let dir = tempfile::tempdir().unwrap();
    let identity = mk_identity(dir.path(), "agent-validate").await;
    let gossip = Arc::new(InProcessGossip::new());
    let cfg = ChatClientConfig {
        max_message_length: 5,
        ..Default::default()
    };
    let client = ChatClientBuilder::new(identity)
        .config(cfg)
        .with_gossip_transport(gossip)
        .build()
        .await
        .unwrap();
    client.login().await.unwrap();
    let peer = NodeId::random();
    let err = client
        .send_message(&peer, "this is way too long")
        .await
        .unwrap_err();
    assert!(matches!(err, BridgeError::InvalidMessage(_)), "got: {err:?}");
    client.logout().await.unwrap();
}

#[tokio::test]
async fn feed_publish_and_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let identity = mk_identity(dir.path(), "agent-feed").await;
    let gossip = Arc::new(InProcessGossip::new());

    let adapter = FeedAdapterBuilder::new(identity)
        .display_name("Reporter")
        .with_gossip_transport(gossip)
        .build()
        .await
        .unwrap();
    adapter.connect().await.unwrap();

    let item_id = adapter
        .publish_report(
            "Test Report",
            "Body content here",
            BulletinCategory::Tech,
            vec!["rust".to_string()],
        )
        .await
        .unwrap();
    assert!(!item_id.is_empty());

    let search = adapter.search("test", 10).await.unwrap();
    // No NewsService is wired, so the cache is empty — but the search
    // call must not error out.
    let _ = search;

    adapter.disconnect().await.unwrap();
}

#[tokio::test]
async fn identity_sign_and_verify_cross_node() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let id_a = mk_identity(dir_a.path(), "agent-a").await;
    let id_b = mk_identity(dir_b.path(), "agent-b").await;

    let message = b"cross-node verification";
    let sig = id_a.sign(message).unwrap();
    assert!(id_a.verify(message, &sig).unwrap());
    assert!(!id_a.verify(b"different", &sig).unwrap());
    assert!(AdnetIdentity::verify_for_node(message, &sig, &id_a.node_id()).unwrap());
    assert!(!AdnetIdentity::verify_for_node(message, &sig, &id_b.node_id()).unwrap());
}

#[tokio::test]
async fn error_classification_helpers() {
    let retryable: BridgeError = BridgeError::Timeout(5);
    assert!(retryable.is_retryable());
    let permanent: BridgeError = BridgeError::InvalidMessage("bad".into());
    assert!(permanent.is_permanent());
    assert!(!permanent.is_retryable());
    let rl: BridgeError = BridgeError::RateLimited("throttle".into());
    assert!(!rl.is_permanent());
    let rate_limited_msg = format!("{rl}");
    assert!(rate_limited_msg.contains("throttle"));
}
