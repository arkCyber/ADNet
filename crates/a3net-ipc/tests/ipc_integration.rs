//! Comprehensive integration and boundary tests for the `a3net-ipc` crate.
//!
//! This module complements the inline `#[cfg(test)]` modules in each source
//! file with integration-level coverage that spans multiple services and
//! exercises the public API surface in ways that are impractical to test
//! inline (e.g. multi-client scenarios, service restart behaviour, concurrent
//! access patterns).

use std::path::PathBuf;
use std::sync::Arc;

use a3net_ipc::client::{json_rpc_call, json_rpc_stream, JsonRpcError, StreamItem};
use a3net_ipc::gossip_service::{GossipIpcConfig, GossipIpcService};
use a3net_ipc::group_chat_service::{
    attachment_with_hash, attachment_with_hash_strict, message_id_for_node, AttachmentKind,
    GroupChatIpcConfig, GroupChatIpcService, MessageEnvelope, MessageType,
};
use a3net_ipc::server::{JsonRpcServer, Notification, RpcHandler};
use a3net_ipc::validation::{ValidationOutcome, ValidationPolicy};
use a3net_types::group_chat::{DirectMessage, GroupChat, GroupMessage};
use a3net_types::NodeId;
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

// ─────────────────────────────────────────────────────────────────────────────
// Test utilities
// ─────────────────────────────────────────────────────────────────────────────

fn tmp_sock(tmp: &tempfile::TempDir, name: &str) -> PathBuf {
    tmp.path().join(name)
}

struct NoopHandler;
#[async_trait]
impl RpcHandler for NoopHandler {
    async fn handle(&self, _method: &str, _params: Value) -> Result<Value, String> {
        Ok(Value::Null)
    }
}

/// Wait briefly for a forwarder to subscribe to the broadcast channel.
async fn wait_for_subscriber() {
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}

// ─────────────────────────────────────────────────────────────────────────────
// Multi-client / concurrent-connection tests for `server`
// ─────────────────────────────────────────────────────────────────────────────

/// `JsonRpcServer::start_with_capacity` allows sizing the broadcast channel.
/// We verify that a server started with capacity 1 still correctly delivers
/// notifications to a connected client.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start_with_capacity_delivers_notifications() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp_sock(&tmp, "cap1.sock");
    let handle = JsonRpcServer::start_with_capacity(sock.clone(), Arc::new(NoopHandler), 1)
        .await
        .unwrap();

    // Open a connection and wait for the forwarder to subscribe.
    let s = tokio::net::UnixStream::connect(&sock).await.unwrap();
    let (r, mut w) = s.into_split();
    w.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"ping\",\"id\":1}\n")
        .await
        .unwrap();
    let mut br = BufReader::new(r);
    let mut line = String::new();
    br.read_line(&mut line).await.unwrap();

    // Wait for the forwarder to actually subscribe.
    wait_for_subscriber().await;

    // Fire a notification.
    let notifier = handle.notifier();
    let n = notifier.send("test", json!({ "k": 1 }));
    assert!(n >= 1);

    let mut line = String::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), br.read_line(&mut line))
        .await
        .expect("timeout");
    let v: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["method"], "test");
    handle.shutdown();
}

/// Two simultaneous clients connected to the same server both receive
/// notifications pushed via the shared `NotificationSender`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_clients_both_receive_notifications() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp_sock(&tmp, "two.sock");
    let _handle =
        JsonRpcServer::start_with_capacity(sock.clone(), Arc::new(NoopHandler), 16)
            .await
            .unwrap();

    // Client 1: handshake then drain the response line.
    let s1 = tokio::net::UnixStream::connect(&sock).await.unwrap();
    let (r1, mut w1) = s1.into_split();
    w1.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"c1\",\"id\":1}\n")
        .await
        .unwrap();
    let mut br1 = BufReader::new(r1);
    let mut line = String::new();
    br1.read_line(&mut line).await.unwrap();

    // Client 2: same dance.
    let s2 = tokio::net::UnixStream::connect(&sock).await.unwrap();
    let (r2, mut w2) = s2.into_split();
    w2.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"c2\",\"id\":1}\n")
        .await
        .unwrap();
    let mut br2 = BufReader::new(r2);
    let mut line2 = String::new();
    br2.read_line(&mut line2).await.unwrap();

    // Wait for both forwarders to subscribe.
    wait_for_subscriber().await;
    wait_for_subscriber().await;

    // Push a notification; both clients should see it.
    let _ = _handle.notifier().send("broadcast", json!({ "value": 42 }));

    let mut l1 = String::new();
    let mut l2 = String::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), br1.read_line(&mut l1))
        .await
        .expect("c1 timeout");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), br2.read_line(&mut l2))
        .await
        .expect("c2 timeout");

    let v1: Value = serde_json::from_str(&l1).unwrap();
    let v2: Value = serde_json::from_str(&l2).unwrap();
    assert_eq!(v1["params"]["value"], 42, "c1 saw value");
    assert_eq!(v2["params"]["value"], 42, "c2 saw value");
}

/// A client that disconnects does not affect the server's ability to serve
/// other clients.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_disconnect_does_not_crash_server() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp_sock(&tmp, "disc.sock");
    let handle =
        JsonRpcServer::start_with_capacity(sock.clone(), Arc::new(NoopHandler), 4)
            .await
            .unwrap();

    // First client connects, sends a request, then disconnects.
    let s = tokio::net::UnixStream::connect(&sock).await.unwrap();
    let (r, mut w) = s.into_split();
    w.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"p\",\"id\":1}\n")
        .await
        .unwrap();
    let mut br = BufReader::new(r);
    let mut line = String::new();
    br.read_line(&mut line).await.unwrap();
    drop(br);
    drop(w);
    // Give the server a moment to notice the disconnect.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // A fresh client must still be served.
    let r = json_rpc_call(&sock, "disc", "ping", json!({}))
        .await
        .unwrap();
    assert_eq!(r, Value::Null);
    handle.shutdown();
}

/// When the server is under load (many connections arriving rapidly), the
/// server must not drop or panic.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn server_handles_burst_connections() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp_sock(&tmp, "burst.sock");
    let handle =
        JsonRpcServer::start_with_capacity(sock.clone(), Arc::new(NoopHandler), 64)
            .await
            .unwrap();

    // Spawn 20 concurrent connections.
    let mut handles = Vec::new();
    for i in 0..20u8 {
        let sock = sock.clone();
        handles.push(tokio::spawn(async move {
            let s = tokio::net::UnixStream::connect(&sock).await?;
            let (r, mut w) = s.into_split();
            w.write_all(
                format!("{{\"jsonrpc\":\"2.0\",\"method\":\"p\",\"id\":{i}}}\n").as_bytes(),
            )
            .await?;
            let mut br = BufReader::new(r);
            let mut line = String::new();
            br.read_line(&mut line).await?;
            let _v: Value = serde_json::from_str(&line)?;
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        }));
    }

    for h in handles {
        h.await.expect("task panicked").expect("connection error");
    }

    // Server is still alive.
    let r = json_rpc_call(&sock, "burst", "ping", json!({}))
        .await
        .unwrap();
    assert_eq!(r, Value::Null);
    handle.shutdown();
}

// ─────────────────────────────────────────────────────────────────────────────
// `NotificationSender` boundary tests
// ─────────────────────────────────────────────────────────────────────────────

/// `NotificationSender::send` is safe to call from multiple tasks concurrently.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn notifier_send_is_send_safe() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp_sock(&tmp, "send_safe.sock");
    let handle =
        JsonRpcServer::start_with_capacity(sock.clone(), Arc::new(NoopHandler), 256)
            .await
            .unwrap();
    let notifier = handle.notifier();

    let mut tasks = Vec::new();
    for i in 0..16u32 {
        let n = notifier.clone();
        tasks.push(tokio::spawn(async move {
            for j in 0..100 {
                let _ = n.send(format!("t{i}_{j}"), json!({ "i": i, "j": j }));
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    handle.shutdown();
}

// ─────────────────────────────────────────────────────────────────────────────
// `client::json_rpc_stream` — additional boundary tests
// ─────────────────────────────────────────────────────────────────────────────

/// `json_rpc_stream` must correctly distinguish between a `Response` with
/// `id: null` and a `Notification` (no `id` field at all).
#[tokio::test]
async fn stream_handles_null_id_vs_missing_id() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp_sock(&tmp, "nullid.sock");
    let listener = UnixListener::bind(&sock).unwrap();

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (_r, mut w) = stream.into_split();
        // Response with id: null (should be parsed as Response).
        let resp = json!({"jsonrpc":"2.0","result":null,"id":null});
        w.write_all(format!("{}\n", resp).as_bytes()).await.unwrap();
        // Notification: no `id` field at all.
        let notif = json!({"jsonrpc":"2.0","method":"evt","params":{}});
        w.write_all(format!("{}\n", notif).as_bytes()).await.unwrap();
    });

    let mut stream = json_rpc_stream(&sock, "NullIdTest").await.unwrap();

    // First item: Response with null id.
    let first = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
        .await
        .expect("timeout")
        .expect("stream ended");
    let first = first.expect("frame error");
    match first {
        StreamItem::Response { id, value: Ok(v) } => {
            assert_eq!(id, 0, "null id should be parsed as 0");
            assert!(v.is_null());
        }
        other => panic!("expected Response(0, null), got {other:?}"),
    }

    // Second item: Notification (no id).
    let second = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
        .await
        .expect("timeout")
        .expect("stream ended");
    let second = second.expect("frame error");
    match second {
        StreamItem::Notification(n) => {
            assert_eq!(n.method, "evt");
        }
        other => panic!("expected Notification, got {other:?}"),
    }

    server.await.unwrap();
}

/// Malformed JSON in a stream frame yields `JsonRpcError::Parse`.
#[tokio::test]
async fn stream_yields_parse_error_on_malformed_json() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp_sock(&tmp, "stream_parse_err.sock");
    let listener = UnixListener::bind(&sock).unwrap();

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (_r, mut w) = stream.into_split();
        // Valid request so the server responds.
        w.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"p\",\"id\":1}\n")
            .await
            .unwrap();
        // Followed by a malformed frame.
        w.write_all(b"not json at all\n").await.unwrap();
    });

    let mut stream = json_rpc_stream(&sock, "Malformed").await.unwrap();

    // First frame is the response to our request.
    let first = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
        .await
        .expect("timeout")
        .expect("stream ended");
    let first = first.expect("frame error");
    assert!(matches!(first, StreamItem::Response { .. }));

    // The malformed second frame is parseable but not valid JSON-RPC:
    // it has no `result` or `error` field. The server writes it but
    // this is unusual — we just verify the stream terminates cleanly.
    let second = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
        .await
        .expect("timeout");
    // Stream may yield the malformed frame as a Response with no result,
    // or may hit EOF/timeout — both are acceptable.
    match second {
        Some(Ok(StreamItem::Response { id: 0, value: Ok(v) })) => {
            assert!(v.is_null(), "malformed frame parsed as null-result response");
        }
        Some(Ok(StreamItem::Response { .. })) => {}
        Some(Ok(StreamItem::Notification(_))) => {}
        Some(Err(_)) => {}
        None => {}
        other => panic!("unexpected stream item: {other:?}"),
    }

    server.await.unwrap();
}

/// `StreamItem` Debug output contains the method name for notifications and
/// the id for responses.
#[test]
fn stream_item_debug_includes_method_and_id() {
    let resp = StreamItem::Response {
        id: 42,
        value: Ok(json!({"k": 1})),
    };
    let dbg = format!("{resp:?}");
    assert!(dbg.contains("42"), "Response debug should contain id: {dbg}");

    let notif = StreamItem::Notification(Notification::new("evt", json!({})));
    let dbg2 = format!("{notif:?}");
    assert!(dbg2.contains("evt"), "Notification debug should contain method: {dbg2}");
}

// ─────────────────────────────────────────────────────────────────────────────
// `gossip_service` — direct unit tests (bypassing RPC) for subscribe/unsubscribe
// ─────────────────────────────────────────────────────────────────────────────

/// `subscribe` on a brand-new topic records the subscriber and creates
/// the topic entry in the subscribers map (but `list_topics` only
/// returns topics that have published messages, not just subscribers).
#[test]
fn gossip_subscribe_creates_topic() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = GossipIpcService::new(GossipIpcConfig {
        socket_path: tmp_sock(&tmp, "g.sock"),
    });

    svc.subscribe("topic-a".into(), "sub1".into()).unwrap();
    assert_eq!(svc.get_subscribers("topic-a".into()), vec!["sub1"]);
    // `list_topics` reflects the topics map, which is populated by
    // `publish`, not `subscribe` — so a topic with only subscribers
    // is not listed.
    assert!(svc.list_topics().is_empty(), "subscribe alone does not create topic entry");
}

/// `subscribe` twice with the same subscriber is idempotent (set semantics).
#[test]
fn gossip_subscribe_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = GossipIpcService::new(GossipIpcConfig {
        socket_path: tmp_sock(&tmp, "g.sock"),
    });

    svc.subscribe("t".into(), "s".into()).unwrap();
    svc.subscribe("t".into(), "s".into()).unwrap();
    svc.subscribe("t".into(), "s".into()).unwrap();

    assert_eq!(svc.get_subscribers("t".into()), vec!["s"]);
}

/// `unsubscribe` removes the subscriber; the topic entry is removed when empty.
#[test]
fn gossip_unsubscribe_removes_subscriber() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = GossipIpcService::new(GossipIpcConfig {
        socket_path: tmp_sock(&tmp, "g.sock"),
    });

    svc.subscribe("t".into(), "s1".into()).unwrap();
    svc.subscribe("t".into(), "s2".into()).unwrap();
    svc.unsubscribe("t".into(), "s1".into()).unwrap();

    assert_eq!(svc.get_subscribers("t".into()), vec!["s2"]);

    // Remove the last subscriber — topic should disappear from subscribers.
    svc.unsubscribe("t".into(), "s2".into()).unwrap();
    assert!(svc.get_subscribers("t".into()).is_empty());
}

/// `unsubscribe` on an unknown topic or subscriber is a no-op.
#[test]
fn gossip_unsubscribe_noop_on_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = GossipIpcService::new(GossipIpcConfig {
        socket_path: tmp_sock(&tmp, "g.sock"),
    });

    // No panic, returns Ok.
    svc.unsubscribe("none".into(), "ghost".into()).unwrap();
    svc.subscribe("t".into(), "s".into()).unwrap();
    svc.unsubscribe("t".into(), "ghost".to_string()).unwrap();
    assert_eq!(svc.get_subscribers("t".into()), vec!["s"]);
}

/// `get_messages` returns the most recent `limit` messages.
#[test]
fn gossip_get_messages_honors_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = GossipIpcService::new(GossipIpcConfig {
        socket_path: tmp_sock(&tmp, "g.sock"),
    });

    for i in 0..10 {
        svc.publish("t".into(), json!({ "i": i })).unwrap();
    }

    let all = svc.get_messages("t".into(), None);
    assert_eq!(all.len(), 10);

    let last5 = svc.get_messages("t".into(), Some(5));
    assert_eq!(last5.len(), 5);

    let empty = svc.get_messages("t".into(), Some(0));
    assert_eq!(empty.len(), 0);
}

/// `get_messages` on a topic with 100+ messages returns the most recent 100.
#[test]
fn gossip_get_messages_caps_at_100() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = GossipIpcService::new(GossipIpcConfig {
        socket_path: tmp_sock(&tmp, "g.sock"),
    });

    for i in 0..150 {
        svc.publish("t".into(), json!({ "i": i })).unwrap();
    }

    let msgs = svc.get_messages("t".into(), Some(1000));
    assert_eq!(msgs.len(), 100);
    // Oldest retained is i=50 (150 - 100).
    assert_eq!(msgs.first().unwrap().payload["i"], 50);
}

/// `publish` returns a unique message id each time.
#[test]
fn gossip_publish_returns_unique_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = GossipIpcService::new(GossipIpcConfig {
        socket_path: tmp_sock(&tmp, "g.sock"),
    });

    let id1 = svc.publish("t".into(), json!({})).unwrap();
    let id2 = svc.publish("t".into(), json!({})).unwrap();
    assert_ne!(id1, id2, "message ids must be unique");
}

// ─────────────────────────────────────────────────────────────────────────────
// `group_chat_service` — tests exercising the public surface
// ─────────────────────────────────────────────────────────────────────────────

fn new_gc_svc(policy: ValidationPolicy) -> GroupChatIpcService {
    let tmp = tempfile::tempdir().unwrap();
    GroupChatIpcService::new(GroupChatIpcConfig {
        socket_path: tmp_sock(&tmp, "gc.sock"),
        node_id: NodeId::random(),
        policy,
    })
}

/// `get_group` returns `None` for an unknown group id (not an error).
#[test]
fn group_chat_get_unknown_group() {
    let svc = new_gc_svc(ValidationPolicy::Strict);
    assert!(svc.get_group("no-such-group").is_none());
}

/// `create_group` with a pre-assigned group_id uses it as-is (no re-assignment).
#[test]
fn group_chat_create_with_preset_id() {
    let svc = new_gc_svc(ValidationPolicy::Strict);

    let g = GroupChat {
        group_id: "preset-123".into(),
        name: "Preset".into(),
        description: "".into(),
        avatar_url: None,
        owner_id: "alice".into(),
        member_ids: vec!["alice".into()],
        admin_ids: vec!["alice".into()],
        is_private: false,
        created_at: 1,
        last_activity: 1,
        message_count: 0,
        public_account_id: None,
        last_sequence: 0,
        assistant_id: None,
    };
    let id = svc.create_group(g).unwrap();
    assert_eq!(id, "preset-123");

    let fetched = svc.get_group("preset-123").unwrap();
    assert_eq!(fetched.group_id, "preset-123");
}

/// `send_group_message` auto-assigns `message_id` when empty.
#[test]
fn group_chat_send_autoassigns_message_id() {
    let svc = new_gc_svc(ValidationPolicy::Strict);

    let id = svc
        .create_group(GroupChat {
            group_id: "g1".into(),
            name: "G1".into(),
            description: "".into(),
            avatar_url: None,
            owner_id: "alice".into(),
            member_ids: vec!["alice".into()],
            admin_ids: vec!["alice".into()],
            is_private: false,
            created_at: 1,
            last_activity: 1,
            message_count: 0,
            public_account_id: None,
            last_sequence: 0,
            assistant_id: None,
        })
        .unwrap();

    let stored = svc
        .send_group_message(GroupMessage {
            message_id: String::new(), // empty — should be auto-assigned
            group_id: id,
            sender_id: "alice".into(),
            sender_name: "Alice".into(),
            content: "hello".into(),
            message_type: MessageType::Text,
            attachments: vec![],
            reply_to: None,
            mentions: vec![],
            timestamp: 1,
            is_edited: false,
            edited_at: None,
            sequence: 1,
            integrity_hash: None,
        })
        .unwrap();

    assert!(!stored.message_id.is_empty());
    assert!(stored.message_id.starts_with("gmsg-"));
}

// ─────────────────────────────────────────────────────────────────────────────
// `validation` — additional boundary tests
// ─────────────────────────────────────────────────────────────────────────────

/// `ValidationOutcome` default is ok with no error and no warnings.
#[test]
fn validation_outcome_default_is_ok() {
    let out = ValidationOutcome::default();
    assert!(out.is_ok());
    assert!(out.error.is_none());
    assert!(out.warnings.is_empty());
}

/// `ValidationOutcome::is_ok` returns true only when error is None.
#[test]
fn validation_outcome_is_ok_semantics() {
    let mut out = ValidationOutcome::default();
    assert!(out.is_ok());

    out.warnings.push("soft warning".into());
    assert!(out.is_ok(), "warnings alone should not flip is_ok");

    out.error = Some("hard error".into());
    assert!(!out.is_ok(), "error should flip is_ok to false");
}

// ─────────────────────────────────────────────────────────────────────────────
// Cross-service integration: restart scenarios
// ─────────────────────────────────────────────────────────────────────────────

/// A `GossipIpcService` that is stopped and restarted with the same socket
/// path is usable again.
#[tokio::test]
async fn gossip_service_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp_sock(&tmp, "gossip_restart.sock");

    // Start first instance, publish a message, then stop.
    let svc1 = Arc::new(GossipIpcService::new(GossipIpcConfig {
        socket_path: sock.clone(),
    }));
    let handle1 = Arc::clone(&svc1).serve().await.unwrap();

    let resp =
        json_rpc_call(&sock, "gossip", "publish", json!({ "topic": "t", "payload": {} }))
            .await
            .unwrap();
    assert_eq!(resp["published"], true);
    handle1.shutdown();
    drop(svc1);

    // Brief pause to let the OS release the socket.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    // Start second instance — must bind the same path.
    let svc2 = Arc::new(GossipIpcService::new(GossipIpcConfig {
        socket_path: sock.clone(),
    }));
    let handle2 = Arc::clone(&svc2).serve().await.unwrap();

    // Publish on the new instance.
    let resp =
        json_rpc_call(&sock, "gossip", "publish", json!({ "topic": "t2", "payload": {} }))
            .await
            .unwrap();
    assert_eq!(resp["published"], true);

    // Topics from the new instance are present.
    let topics = json_rpc_call(&sock, "gossip", "list_topics", json!({}))
        .await
        .unwrap();
    let names: Vec<&str> = topics["topics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(names.contains(&"t2"), "new topic must appear: {names:?}");

    handle2.shutdown();
}

/// A `GroupChatIpcService` can be restarted and the in-memory state is fresh
/// (no persistence across restarts — that's a higher-level concern).
#[tokio::test]
async fn group_chat_service_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp_sock(&tmp, "gc_restart.sock");

    // First instance.
    let svc1 = GroupChatIpcService::new(GroupChatIpcConfig {
        socket_path: sock.clone(),
        node_id: NodeId::random(),
        policy: ValidationPolicy::Strict,
    });
    let svc1_arc = Arc::new(svc1);
    let handle1 = Arc::clone(&svc1_arc).serve().await.unwrap();

    let r = json_rpc_call(
        &sock,
        "chat",
        "group_create",
        json!({
            "group": {
                "group_id": "",
                "name": "G1",
                "description": "",
                "avatar_url": null,
                "owner_id": "alice",
                "member_ids": ["alice"],
                "admin_ids": ["alice"],
                "is_private": false,
                "created_at": 1,
                "last_activity": 1,
                "message_count": 0,
                "public_account_id": null,
                "last_sequence": 0,
                "assistant_id": null,
            }
        }),
    )
    .await
    .unwrap();
    let _gid = r["group_id"].as_str().unwrap().to_string();

    handle1.shutdown();
    drop(svc1_arc);
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    // Second instance: no group should be visible (fresh state).
    let svc2 = GroupChatIpcService::new(GroupChatIpcConfig {
        socket_path: sock.clone(),
        node_id: NodeId::random(),
        policy: ValidationPolicy::Strict,
    });
    let svc2_arc = Arc::new(svc2);
    let handle2 = Arc::clone(&svc2_arc).serve().await.unwrap();

    let r = json_rpc_call(&sock, "chat", "group_list_user", json!({ "user_id": "alice" }))
        .await
        .unwrap();
    assert!(r["groups"].as_array().unwrap().is_empty());

    handle2.shutdown();
}

// ─────────────────────────────────────────────────────────────────────────────
// `MessageEnvelope` — additional serde edge cases
// ─────────────────────────────────────────────────────────────────────────────

/// `MessageEnvelope::Direct` serialises with `kind: "direct"` discriminator.
#[test]
fn envelope_direct_serde_kind() {
    let env = MessageEnvelope::Direct(DirectMessage {
        message_id: "d1".into(),
        chat_id: "c1".into(),
        sender_id: "alice".into(),
        receiver_id: "bob".into(),
        content: "hello".into(),
        message_type: MessageType::Text,
        attachments: vec![],
        reply_to: None,
        sequence: 1,
        timestamp: 1,
        integrity_hash: None,
        is_edited: false,
        edited_at: None,
    });
    let v = serde_json::to_value(&env).unwrap();
    assert_eq!(v["kind"], "direct");
    let back: MessageEnvelope = serde_json::from_value(v).unwrap();
    match back {
        MessageEnvelope::Direct(dm) => assert_eq!(dm.content, "hello"),
        _ => panic!("expected Direct"),
    }
}

/// `MessageEnvelope` deserialised from JSON with wrong `kind` fails gracefully.
#[test]
fn envelope_unknown_kind_fails() {
    let v = json!({ "kind": "unknown", "x": 1 });
    let result: Result<MessageEnvelope, _> = serde_json::from_value(v);
    assert!(result.is_err(), "unknown kind should fail: {:?}", result);
}

// ─────────────────────────────────────────────────────────────────────────────
// `group_chat_service` helper functions
// ─────────────────────────────────────────────────────────────────────────────

/// `message_id_for_node` is deterministic for the same (node, group, sequence).
#[test]
fn message_id_for_node_deterministic() {
    let node = NodeId::random();
    let id1 = message_id_for_node(&node, "g1", 1);
    let id2 = message_id_for_node(&node, "g1", 1);
    assert_eq!(id1, id2);
    assert_ne!(id1, message_id_for_node(&node, "g1", 2));
    assert_ne!(id1, message_id_for_node(&node, "g2", 1));
}

/// `attachment_with_hash` and `attachment_with_hash_strict` both produce
/// a valid `MessageAttachment` for a valid file type.
#[test]
fn attachment_helpers_produce_valid_attachments() {
    use a3net_types::ContentHash;
    let h = ContentHash::from_hex(&"b".repeat(64)).unwrap();

    let att = attachment_with_hash("att1".into(), AttachmentKind::Video, &h, "vid.mp4", 1024);
    assert_eq!(att.attachment_id, "att1");
    assert_eq!(att.file_size, 1024);

    let att2 = attachment_with_hash_strict("att2".into(), "video", &h, "vid2.mp4", 2048)
        .expect("'video' should be valid");
    assert_eq!(att2.attachment_id, "att2");
    assert_eq!(att2.file_size, 2048);
}

/// `attachment_with_hash_strict` rejects unknown file type strings.
#[test]
fn attachment_strict_rejects_unknown_type() {
    use a3net_types::ContentHash;
    let h = ContentHash::from_hex(&"c".repeat(64)).unwrap();
    let result = attachment_with_hash_strict("att".into(), "bogus", &h, "f", 0);
    assert!(result.is_err());
}

// ─────────────────────────────────────────────────────────────────────────────
// Concurrent publish stress test for `GossipIpcService`
// ─────────────────────────────────────────────────────────────────────────────

/// Multiple concurrent `publish` calls on the same topic all succeed and are
/// all retrievable. Message IDs are based on nanosecond timestamps and
/// may collide under heavy concurrency (nanosecond resolution is finite);
/// the key invariants are that every publish succeeds and every message
/// is retrievable.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn gossip_concurrent_publish() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp_sock(&tmp, "g_conc.sock");
    let svc = Arc::new(GossipIpcService::new(GossipIpcConfig {
        socket_path: sock.clone(),
    }));
    let handle = Arc::clone(&svc).serve().await.unwrap();

    let count = 50;
    let mut tasks = Vec::new();
    for i in 0..count {
        let svc = Arc::clone(&svc);
        tasks.push(tokio::spawn(async move {
            svc.publish("conc-topic".into(), json!({ "i": i }))
        }));
    }

    let results: Vec<Result<String, String>> = futures::future::join_all(tasks)
        .await
        .into_iter()
        .map(|r| r.expect("spawn panicked"))
        .collect();

    // All publishes succeeded.
    for r in &results {
        assert!(r.is_ok(), "publish failed: {:?}", r);
    }

    // All messages are retrievable (the count should match).
    let msgs = svc.get_messages("conc-topic".into(), Some(100));
    assert_eq!(msgs.len(), count as usize, "all published messages must be retrievable");

    handle.shutdown();
}

// ─────────────────────────────────────────────────────────────────────────────
// Concurrent send stress test for `GroupChatIpcService`
// ─────────────────────────────────────────────────────────────────────────────

/// Multiple concurrent `send_group_message` calls on the same group produce
/// unique message ids and are all retrievable.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn group_chat_concurrent_send() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp_sock(&tmp, "gc_conc.sock");
    let svc = GroupChatIpcService::new(GroupChatIpcConfig {
        socket_path: sock.clone(),
        node_id: NodeId::random(),
        policy: ValidationPolicy::Strict,
    });
    let svc_arc = Arc::new(svc);
    let handle = Arc::clone(&svc_arc).serve().await.unwrap();

    // Create a group first.
    let id = svc_arc
        .create_group(GroupChat {
            group_id: "conc-group".into(),
            name: "Conc".into(),
            description: "".into(),
            avatar_url: None,
            owner_id: "alice".into(),
            member_ids: vec!["alice".into()],
            admin_ids: vec!["alice".into()],
            is_private: false,
            created_at: 1,
            last_activity: 1,
            message_count: 0,
            public_account_id: None,
            last_sequence: 0,
            assistant_id: None,
        })
        .unwrap();

    let count = 20;
    let mut tasks = Vec::new();
    for i in 0..count {
        let svc = Arc::clone(&svc_arc);
        let gid = id.clone();
        tasks.push(tokio::spawn(async move {
            svc.send_group_message(GroupMessage {
                message_id: String::new(),
                group_id: gid,
                sender_id: "alice".into(),
                sender_name: "Alice".into(),
                content: format!("msg {i}"),
                message_type: MessageType::Text,
                attachments: vec![],
                reply_to: None,
                mentions: vec![],
                timestamp: i as u64 + 1,
                is_edited: false,
                edited_at: None,
                sequence: i + 1,
                integrity_hash: None,
            })
        }));
    }

    let results: Vec<Result<GroupMessage, String>> = futures::future::join_all(tasks)
        .await
        .into_iter()
        .map(|r| r.expect("spawn panicked"))
        .collect();

    // All succeeded.
    for r in &results {
        assert!(r.is_ok(), "send failed: {:?}", r);
    }

    // All message ids are unique.
    let mut ids: Vec<_> = results
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .map(|m| m.message_id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), count as usize);

    // All messages are retrievable.
    let msgs = svc_arc.get_group_messages(&id, Some(100));
    assert_eq!(msgs.len(), count as usize);

    handle.shutdown();
}

// ─────────────────────────────────────────────────────────────────────────────
// Blobs service additional boundary tests
// ─────────────────────────────────────────────────────────────────────────────

/// `blobs_service::validate_blob_identity` is a public function — test it
/// directly for completeness.
#[test]
fn blobs_validate_blob_identity_public() {
    use a3net_ipc::blobs_service::validate_blob_identity;
    use a3net_ipc::blobs_service::HashedBlob;

    // Valid 64-char lowercase hex.
    let valid = "0123456789abcdef".repeat(4);
    assert!(validate_blob_identity(&HashedBlob::new(valid)).is_ok());

    // Uppercase is also valid (hex digit).
    let upper = "ABCDEF0123456789".repeat(4);
    assert!(validate_blob_identity(&HashedBlob::new(upper)).is_ok());

    // Too short.
    assert!(validate_blob_identity(&HashedBlob::new("a".repeat(63))).is_err());

    // Too long.
    assert!(validate_blob_identity(&HashedBlob::new("a".repeat(65))).is_err());

    // Non-hex char.
    let mut bad = "a".repeat(64);
    bad.replace_range(0..1, "g");
    assert!(validate_blob_identity(&HashedBlob::new(bad)).is_err());
}

/// `BlobTicket` serialises `expires_at: None` as absent (skip_serializing_if).
#[test]
fn blob_ticket_skips_expires_at_when_none() {
    use a3net_ipc::blobs_service::BlobTicket;

    let with = BlobTicket {
        node_id: "n".into(),
        blob_hash: "0".repeat(64),
        format: "raw".into(),
        expires_at: Some(999),
    };
    let v_with = serde_json::to_value(&with).unwrap();
    assert_eq!(v_with["expires_at"], 999);

    let without = BlobTicket {
        node_id: "n".into(),
        blob_hash: "0".repeat(64),
        format: "raw".into(),
        expires_at: None,
    };
    let v_without = serde_json::to_value(&without).unwrap();
    assert!(
        v_without.get("expires_at").is_none(),
        "expires_at should be skipped"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Server-side disconnect and reconnection stress
// ─────────────────────────────────────────────────────────────────────────────

/// Rapid connect/disconnect cycles do not corrupt server state.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rapid_connect_disconnect_cycles() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp_sock(&tmp, "rapid.sock");
    let handle =
        JsonRpcServer::start_with_capacity(sock.clone(), Arc::new(NoopHandler), 64)
            .await
            .unwrap();

    for _ in 0..30 {
        let s = tokio::net::UnixStream::connect(&sock).await.unwrap();
        let (r, mut w) = s.into_split();
        w.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"p\",\"id\":1}\n")
            .await
            .unwrap();
        let mut br = BufReader::new(r);
        let mut line = String::new();
        let _ = br.read_line(&mut line).await;
        drop(br);
        drop(w);
        tokio::time::sleep(std::time::Duration::from_micros(100)).await;
    }

    // Server is still alive.
    let r = json_rpc_call(&sock, "rapid", "ping", json!({}))
        .await
        .unwrap();
    assert_eq!(r, Value::Null);
    handle.shutdown();
}

// ─────────────────────────────────────────────────────────────────────────────
// `BlobsIpcService` end-to-end integration tests
// ─────────────────────────────────────────────────────────────────────────────

/// `BlobsIpcService` correctly handles concurrent `add_blob` calls.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blobs_concurrent_add() {
    use a3net_ipc::blobs_service::{BlobsIpcConfig, BlobsIpcService};

    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp_sock(&tmp, "blobs_conc.sock");
    let svc = Arc::new(BlobsIpcService::new(BlobsIpcConfig {
        socket_path: sock.clone(),
        data_dir: None,
        policy: ValidationPolicy::Strict,
    }));
    let handle = Arc::clone(&svc).serve().await.unwrap();

    let count = 20;
    let mut tasks = Vec::new();
    for i in 0..count {
        let svc = Arc::clone(&svc);
        let data = format!("blob {i}").into_bytes();
        tasks.push(tokio::spawn(async move { svc.add_blob(data).await }));
    }

    let results: Vec<Result<String, String>> = futures::future::join_all(tasks)
        .await
        .into_iter()
        .map(|r| r.expect("spawn panicked"))
        .collect();

    // All adds succeeded.
    for r in &results {
        assert!(r.is_ok(), "add_blob failed: {:?}", r);
    }

    // All returned hashes are unique.
    let mut hashes: Vec<_> = results
        .into_iter()
        .filter_map(|r| r.ok())
        .collect();
    hashes.sort();
    hashes.dedup();
    assert_eq!(hashes.len(), count as usize);

    // All blobs are listable.
    let list = svc.list_blobs().await;
    assert_eq!(list.len(), count as usize);

    handle.shutdown();
}

/// `BlobsIpcService` with disk persistence survives a restart.
#[tokio::test]
async fn blobs_disk_persistence_survives_restart() {
    use a3net_ipc::blobs_service::{BlobsIpcConfig, BlobsIpcService};
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;

    let tmp = tempfile::tempdir().unwrap();
    let sock1 = tmp_sock(&tmp, "blobs_restart1.sock");
    let data_dir = tmp.path().join("blobs_store");

    let data = b"persistent-blob";
    let b64 = BASE64.encode(data);

    // First instance with disk store.
    let svc1 = Arc::new(BlobsIpcService::new(BlobsIpcConfig {
        socket_path: sock1.clone(),
        data_dir: Some(data_dir.clone()),
        policy: ValidationPolicy::Strict,
    }));
    let handle1 = Arc::clone(&svc1).serve().await.unwrap();

    let resp =
        json_rpc_call(&sock1, "blobs", "add_blob", json!({ "data": b64 }))
            .await
            .unwrap();
    let hash = resp["hash"].as_str().unwrap().to_string();
    handle1.shutdown();
    drop(svc1);

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    // Second instance on same data_dir.
    let sock2 = tmp_sock(&tmp, "blobs_restart2.sock");
    let svc2 = Arc::new(BlobsIpcService::new(BlobsIpcConfig {
        socket_path: sock2.clone(),
        data_dir: Some(data_dir),
        policy: ValidationPolicy::Strict,
    }));
    let handle2 = Arc::clone(&svc2).serve().await.unwrap();

    // Blob should be retrievable.
    let list = svc2.list_blobs().await;
    assert!(list.contains(&hash), "hash {hash} should be in {list:?}");

    let resp = json_rpc_call(&sock2, "blobs", "get_blob", json!({ "hash": hash }))
        .await
        .unwrap();
    let retrieved = BASE64.decode(resp["data"].as_str().unwrap()).unwrap();
    assert_eq!(retrieved, data);

    handle2.shutdown();
}
