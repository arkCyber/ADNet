//! `NodeRpc` — JSON-RPC handler that exposes an [`a3net_node::Node`]
//! over a Unix socket and bridges its event stream into
//! server-pushed notifications.

use std::path::Path;
use std::sync::Arc;

use a3net_ipc::{NotificationSender, RpcHandler};
use a3net_node::{Node, NodeInfo};
use a3net_types::{Announcement, BlobTicket, CdnContentKind, ContentHash, RoomId};
use anyhow::Context as _;
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tracing::warn;

/// Notification method name pushed to every connected client when the
/// daemon's room subscriber observes a new announcement.
pub const ANNOUNCEMENT_METHOD: &str = "announcement";

/// Stable list of every method name served by [`NodeRpc`]. Kept as
/// `const` so external UIs can programmatically check support
/// without parsing docs.
pub const METHODS: &[&str] = &[
    "init",
    "info",
    "list_rooms",
    "join",
    "leave",
    "feed",
    "announce",
    "peers_for",
    "make_ticket",
    "agent.ask",
    // P2P peer-table surface. Added in 0.4 — exposes the bounded
    // peer manager over RPC so the CLI / dashboards can read liveness
    // without a direct `a3net_node` reference.
    "peer_list",
    "peer_status",
    "peer_stats",
    "peer_heartbeat_ping",
    "peer_heartbeat_stats",
];

/// A `Node` exposed as a JSON-RPC handler. Cheap to clone via `Arc`.
///
/// All methods are async; the handler itself holds the `Node` behind
/// `Arc` so the same instance can be shared with the forwarder task
/// (and with anyone who wants to call into the node directly while
/// the server is running).
pub struct NodeRpc {
    inner: Arc<Inner>,
}

struct Inner {
    node: Arc<Node>,
    /// Forwarder handles keyed by room id. The forwarder task itself
    /// is `AbortOnDrop`; we just keep the join handle so we can wait
    /// for graceful shutdown if needed.
    forwarders: Mutex<Vec<(RoomId, tokio::task::JoinHandle<()>)>>,
}

impl NodeRpc {
    pub fn new(node: Node) -> Self {
        Self {
            inner: Arc::new(Inner {
                node: Arc::new(node),
                forwarders: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Wire this handler up to a [`NotificationSender`] so that every
    /// new announcement on a joined room is pushed to connected
    /// clients. Idempotent — calling twice for the same room is a
    /// no-op. Spawns one supervisor task that polls
    /// `joined_rooms()` and wires new rooms as they appear; when the
    /// supervisor exits the existing forwarders are aborted.
    pub async fn serve_with_notifier(self: Arc<Self>, notifier: NotificationSender) {
        // Snapshot the current joined rooms and spawn a forwarder
        // for each immediately so we don't miss the first events.
        let initial = self.inner.node.joined_rooms().await;
        for room in initial {
            self.spawn_forwarder(room, notifier.clone()).await;
        }
        // Supervisor: every second, scan for newly joined rooms and
        // spawn a forwarder for each. A `join` RPC returns before
        // this tick fires — the next event in that room will be
        // emitted at most ~1s late. For sub-second reactivity the
        // caller can wire the notifier directly into their own
        // `join` handler.
        let inner = Arc::clone(&self.inner);
        let notifier = notifier.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
            tick.tick().await; // consume the immediate tick
            loop {
                tick.tick().await;
                let joined = inner.node.joined_rooms().await;
                let mut guards = inner.forwarders.lock().await;
                for room in joined {
                    if !guards.iter().any(|(r, _)| r == &room) {
                        // Re-acquire a fresh Node reference — the
                        // supervisor is the only owner of this
                        // outer Arc.
                        let node = Arc::clone(&inner.node);
                        let room_owned = room.clone();
                        let room_label = room.clone();
                        let notifier = notifier.clone();
                        let handle = tokio::spawn(async move {
                            let mut rx = node.subscribe_room(&room_owned);
                            loop {
                                match rx.recv().await {
                                    Ok(ann) => {
                                        let payload = match serde_json::to_value(
                                            announcement_payload(&ann),
                                        ) {
                                            Ok(v) => v,
                                            Err(e) => {
                                                warn!("failed to serialise announcement: {e}");
                                                continue;
                                            }
                                        };
                                        notifier.send(ANNOUNCEMENT_METHOD, payload);
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                        warn!(
                                            "room {} subscriber lagged by {n} notifications — skipping ahead",
                                            room_label
                                        );
                                        continue;
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                                }
                            }
                        });
                        guards.push((room, handle));
                    }
                }
            }
        });
    }

    async fn spawn_forwarder(&self, room: RoomId, notifier: NotificationSender) {
        // (Kept as a thin wrapper for any future caller. The
        // supervisor inside `serve_with_notifier` is the only
        // current user and inlines the loop for clarity.)
        let mut guards = self.inner.forwarders.lock().await;
        if guards.iter().any(|(r, _)| r == &room) {
            return; // already wired
        }
        let node = Arc::clone(&self.inner.node);
        let room_owned = room.clone();
        let room_label = room.clone();
        let notifier = notifier.clone();
        let handle = tokio::spawn(async move {
            let mut rx = node.subscribe_room(&room_owned);
            loop {
                match rx.recv().await {
                    Ok(ann) => {
                        let payload =
                            serde_json::to_value(announcement_payload(&ann)).unwrap_or(Value::Null);
                        notifier.send(ANNOUNCEMENT_METHOD, payload);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!(
                            "room {} subscriber lagged by {n} notifications — skipping ahead",
                            room_label
                        );
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        guards.push((room, handle));
    }
}

/// JSON wrapper for a serialised announcement notification. Decouples
/// the wire format from the internal `Announcement` struct so we can
/// add/rename fields without breaking clients. Field names are
/// camelCase to match `NodeInfo` and `RoomAsset`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AnnouncementNotification<'a> {
    room: &'a str,
    hash: String,
    title: &'a str,
    kind: &'a str,
    size_bytes: u64,
    announcer_node_id: String,
    timestamp: String,
    ticket: Option<String>,
}

fn announcement_payload(ann: &Announcement) -> AnnouncementNotification<'_> {
    AnnouncementNotification {
        room: ann.room_id.as_str(),
        hash: ann.content_hash.as_hex().to_string(),
        title: &ann.title,
        kind: ann.kind.as_str(),
        size_bytes: ann.size_bytes,
        announcer_node_id: ann.node_id.as_hex().to_string(),
        timestamp: ann.timestamp.to_rfc3339(),
        ticket: ann.ticket.as_ref().map(|t| t.encode()),
    }
}

#[async_trait]
impl RpcHandler for NodeRpc {
    async fn handle(&self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "init" | "info" => {
                let info: NodeInfo = self.inner.node.info().await;
                serde_json::to_value(info).map_err(|e| e.to_string())
            }
            "list_rooms" => {
                let rooms = self.inner.node.joined_rooms().await;
                let arr: Vec<String> = rooms.iter().map(|r| r.as_str().to_string()).collect();
                Ok(Value::Array(arr.into_iter().map(Value::String).collect()))
            }
            "join" => {
                let room = require_string(&params, "room")?;
                let room_id: RoomId = room.into();
                self.inner
                    .node
                    .join_room(&room_id)
                    .await
                    .map_err(|e| format!("join: {e}"))?;
                // Forwarder wiring happens in the daemon entry
                // point; here we just return success and let the
                // forwarder be re-spawned by an external notifier
                // wiring call. The bundled `start_daemon` does this
                // automatically.
                Ok(json!({}))
            }
            "leave" => {
                let room = require_string(&params, "room")?;
                let room_id: RoomId = room.into();
                self.inner
                    .node
                    .leave_room(&room_id)
                    .await
                    .map_err(|e| format!("leave: {e}"))?;
                // Stop any forwarder task for this room. We don't
                // await its termination — `JoinHandle::abort` is
                // good enough; the broadcast channel will be closed
                // when the daemon drops the notifier.
                let mut guards = self.inner.forwarders.lock().await;
                guards.retain(|(r, h)| {
                    if r == &room_id {
                        h.abort();
                        false
                    } else {
                        true
                    }
                });
                Ok(json!({}))
            }
            "feed" => {
                let room = require_string(&params, "room")?;
                let room_id: RoomId = room.into();
                // Auto-join so the feed is populated by the local
                // mesh if the caller didn't explicitly join first.
                if let Err(e) = self.inner.node.join_room(&room_id).await {
                    warn!(error = %e, room = %room_id, "auto-join for /feed failed");
                }
                let feed = self
                    .inner
                    .node
                    .room_feed(&room_id)
                    .await
                    .map_err(|e| format!("feed: {e}"))?;
                // `RoomFeed` doesn't carry `Serialize`; we hand-roll
                // a stable DTO so the wire format is decoupled from
                // the in-memory representation. Mirrors
                // `a3net_cli::feed_view::feed_for_humans` but keeps
                // peer sources so callers can dial the right
                // endpoint.
                let assets: Vec<Value> = feed
                    .assets
                    .iter()
                    .map(|a| {
                        json!({
                            "hash": a.content_hash.as_hex(),
                            "title": a.title,
                            "kind": a.kind.as_str(),
                            "sizeBytes": a.size_bytes,
                            "mimeType": a.mime_type,
                            "sourceUrl": a.source_url,
                            "announcerNodeId": a.announcer_node_id.as_hex(),
                            "announcedAt": a.announced_at.to_rfc3339(),
                        })
                    })
                    .collect();
                let mut peer_map = serde_json::Map::new();
                for (hash, tickets) in &feed.peer_map {
                    let arr: Vec<Value> =
                        tickets.iter().map(|t| Value::String(t.encode())).collect();
                    peer_map.insert(hash.as_hex().to_string(), Value::Array(arr));
                }
                Ok(json!({
                    "room": feed.room_id.as_str(),
                    "assets": assets,
                    "peerMap": peer_map,
                }))
            }
            "announce" => {
                let room = require_string(&params, "room")?;
                let file = require_string(&params, "file")?;
                let title = params
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("shared file");
                let kind_str = params
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("generic_file");
                let Some(kind) = CdnContentKind::from_str_loose(kind_str) else {
                    return Err(format!("unknown kind: {kind_str}"));
                };
                let room_id: RoomId = room.into();
                self.inner
                    .node
                    .join_room(&room_id)
                    .await
                    .map_err(|e| format!("announce: auto-join: {e}"))?;
                let path = std::path::PathBuf::from(file);
                let ann = self
                    .inner
                    .node
                    .import_and_announce(&room_id, &path, title, kind)
                    .await
                    .map_err(|e| format!("announce: {e}"))?;
                let ticket = ann.ticket.as_ref().map(|t| t.encode()).unwrap_or_default();
                Ok(json!({
                    "room": room_id.as_str(),
                    "hash": ann.content_hash.as_hex(),
                    "sizeBytes": ann.size_bytes,
                    "ticket": ticket,
                }))
            }
            "peers_for" => {
                let hash_hex = require_string(&params, "hash")?;
                let hash =
                    ContentHash::from_hex(&hash_hex).map_err(|e| format!("invalid hash: {e}"))?;
                let tickets: Vec<BlobTicket> = self.inner.node.peers_for(&hash).await;
                let arr: Vec<String> = tickets.iter().map(|t| t.encode()).collect();
                Ok(Value::Array(arr.into_iter().map(Value::String).collect()))
            }
            "make_ticket" => {
                let hash_hex = require_string(&params, "hash")?;
                let hash =
                    ContentHash::from_hex(&hash_hex).map_err(|e| format!("invalid hash: {e}"))?;
                let ticket = self
                    .inner
                    .node
                    .make_ticket(&hash)
                    .await
                    .map_err(|e| format!("make_ticket: {e}"))?;
                Ok(Value::String(ticket.encode()))
            }
            "agent.ask" => self.handle_agent_ask(params).await,
            "peer_list" => {
                let pm = self.inner.node.peer_manager();
                let pm = pm.ok_or_else(|| {
                    "peer manager is disabled on this node (rebuild with with_peer_manager_config)"
                        .to_string()
                })?;
                let snap = pm.list();
                let peers: Vec<Value> = snap
                    .peers
                    .into_iter()
                    .map(|p| {
                        json!({
                            "nodeId": p.node_id.as_hex(),
                            "alias": p.alias,
                            "remoteName": p.remote_name,
                            "status": status_str(&p.status),
                            "connectedAt": p.connected_at.to_rfc3339(),
                            "lastSeenAt": p.last_seen_at.to_rfc3339(),
                            "lastHeartbeatAt": p.last_heartbeat_at.to_rfc3339(),
                            "lastPingSentAt": p.last_ping_sent_at.to_rfc3339(),
                            "lastPingRecvAt": p.last_ping_recv_at.to_rfc3339(),
                            "lastRttMs": p.last_rtt_ms,
                            "avgRttMs": p.avg_rtt_ms,
                            "totalPingsSent": p.total_pings_sent,
                            "totalPingsRecv": p.total_pings_recv,
                            "heartbeatFailures": p.heartbeat_failures,
                            "suspectCount": p.suspect_count,
                            "deadCount": p.dead_count,
                        })
                    })
                    .collect();
                Ok(json!({
                    "capacity": snap.capacity,
                    "aliveCount": snap.alive_count,
                    "deadCount": snap.dead_count,
                    "connectingCount": snap.connecting_count,
                    "peers": peers,
                }))
            }
            "peer_status" => {
                let peer_id_str = require_string(&params, "nodeId")?;
                let peer_id = a3net_types::NodeId::from_hex(&peer_id_str)
                    .map_err(|e| format!("invalid nodeId: {e}"))?;
                let pm = self.inner.node.peer_manager();
                let pm = pm.ok_or_else(|| {
                    "peer manager is disabled on this node".to_string()
                })?;
                Ok(match pm.get(&peer_id) {
                    Some(entry) => json!({
                        "nodeId": entry.node_id.as_hex(),
                        "status": status_str(&entry.status),
                        "lastHeartbeatAt": entry.last_heartbeat_at.to_rfc3339(),
                        "lastPingRecvAt": entry.last_ping_recv_at.to_rfc3339(),
                        "avgRttMs": entry.avg_rtt_ms,
                    }),
                    None => Value::Null,
                })
            }
            "peer_stats" => {
                // Aggregate table-wide counts. Used by the CLI's `peer list`
                // header and by external dashboards.
                let pm = self.inner.node.peer_manager();
                let pm = pm.ok_or_else(|| {
                    "peer manager is disabled on this node".to_string()
                })?;
                let snap = pm.list();
                let cfg = pm.config();
                Ok(json!({
                    "maxPeers": cfg.max_peers,
                    "heartbeatIntervalMs": cfg.heartbeat_interval.as_millis() as u64,
                    "heartbeatTimeoutMs": cfg.heartbeat_timeout.as_millis() as u64,
                    "autoHeartbeat": cfg.auto_heartbeat,
                    "aliveCount": snap.alive_count,
                    "deadCount": snap.dead_count,
                    "connectingCount": snap.connecting_count,
                }))
            }
            "peer_heartbeat_ping" => {
                // Trigger one heartbeat tick synchronously. Useful
                // for CLI-driven manual pings and tests; the auto
                // service normally drives ticks every interval.
                let pm = self.inner.node.peer_manager();
                let pm = pm.ok_or_else(|| {
                    "peer manager is disabled on this node".to_string()
                })?;
                let stats = pm.heartbeat_tick();
                Ok(json!({
                    "pingsSent": stats.pings_sent,
                    "newlyDead": stats.newly_dead,
                    "recovered": stats.recovered,
                    "becameSuspect": stats.became_suspect,
                }))
            }
            "peer_heartbeat_stats" => {
                let peer_id_str = require_string(&params, "nodeId")?;
                let peer_id = a3net_types::NodeId::from_hex(&peer_id_str)
                    .map_err(|e| format!("invalid nodeId: {e}"))?;
                let pm = self.inner.node.peer_manager();
                let pm = pm.ok_or_else(|| {
                    "peer manager is disabled on this node".to_string()
                })?;
                let s = pm.stats_for(&peer_id);
                Ok(json!({
                    "lastRttMs": s.last_rtt_ms,
                    "avgRttMs": s.avg_rtt_ms,
                    "totalPingsSent": s.total_pings_sent,
                    "totalPingsRecv": s.total_pings_recv,
                    "suspectCount": s.suspect_count,
                    "deadCount": s.dead_count,
                }))
            }
            other => Err(format!("unknown method: {other}")),
        }
    }
}

fn status_str(s: &a3net_node::PeerStatus) -> &'static str {
    use a3net_node::PeerStatus::*;
    match s {
        Connecting => "connecting",
        Alive => "alive",
        Suspect => "suspect",
        Dead => "dead",
        Removed => "removed",
    }
}

fn require_string(params: &Value, key: &str) -> Result<String, String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("missing or non-string `{key}`"))
}

impl NodeRpc {
    #[cfg(feature = "agent-v1")]
    async fn handle_agent_ask(&self, params: Value) -> Result<Value, String> {
        use a3net_node::agent::{AgentAclMode, NodeAgentBridge};
        use a3net_agent::{audit::AuditCtx, audit::Outcome, ChatMessage, ChatRequest};
        use a3net_transport::agent_v1::AgentV1ChatResponse;

        let peer = require_string(&params, "peer")?;
        let question = require_string(&params, "question")?;

        // Build a ChatRequest from the question
        let req = ChatRequest::new(vec![ChatMessage::user(question)]);

        // ACL check + model label for audit
        let bridge: &NodeAgentBridge = self.inner.node.agent_bridge();
        let acl = bridge.acl();
        let model_label = bridge
            .model_id()
            .unwrap_or_else(|| "no-model".to_string());

        let permitted = match acl {
            AgentAclMode::AllowAll => true,
            AgentAclMode::DenyAll => false,
            AgentAclMode::AllowList(ref ids) => ids.contains(&peer),
        };
        if !permitted {
            #[cfg(feature = "audit")]
            {
                let mut ctx = AuditCtx::new(peer.clone(), model_label);
                ctx.set_error("acl denied");
                ctx.finish(Outcome::PeerNotPermitted);
            }
            return Err(format!(
                "peer {} is not permitted to use agent (ACL = {:?})",
                peer, acl
            ));
        }

        // No model registered?
        if bridge.model_id().is_none() {
            #[cfg(feature = "audit")]
            {
                let mut ctx = AuditCtx::new(peer.clone(), model_label);
                ctx.set_error("no model registered");
                ctx.finish(Outcome::NoModel);
            }
            return Err("no agent model registered on this node".to_string());
        }

        // Begin audit — model is known to be present
        #[cfg(feature = "audit")]
        let audit_ctx = AuditCtx::new(peer.clone(), model_label.clone());

        // Execute via the bridge
        let resp = bridge
            .handle_v1_chat(self.inner.node.node_id().as_hex(), &peer, req)
            .await;

        #[cfg(feature = "audit")]
        {
            match &resp {
                AgentV1ChatResponse { error: None, body: Some(body), .. } => {
                    let usage = body.metadata.get("usage")
                        .and_then(|v| serde_json::from_value::<a3net_agent::audit::TokenUsage>(v.clone()).ok());
                    let mut ctx = audit_ctx;
                    if let Some(u) = usage {
                        ctx = ctx.with_usage(u);
                    }
                    ctx.finish(a3net_agent::audit::Outcome::Ok);
                }
                AgentV1ChatResponse { error: Some(e), .. } => {
                    let mut ctx = audit_ctx;
                    ctx.set_error(e.clone());
                    ctx.finish(a3net_agent::audit::Outcome::ModelError);
                }
                _ => {
                    let mut ctx = audit_ctx;
                    ctx.set_error("empty response");
                    ctx.finish(a3net_agent::audit::Outcome::ModelError);
                }
            }
        }

        // Convert AgentV1ChatResponse to JSON
        match resp {
            AgentV1ChatResponse {
                body: Some(body), ..
            } => Ok(serde_json::to_value(body).map_err(|e| e.to_string())?),
            AgentV1ChatResponse { error: Some(err), .. } => {
                Err(format!("agent error: {}", err))
            }
            _ => Err("agent returned empty response".to_string()),
        }
    }

    #[cfg(not(feature = "agent-v1"))]
    async fn handle_agent_ask(&self, _params: Value) -> Result<Value, String> {
        Err("agent.v1 is not enabled on this node (rebuild with --features agent-v1)".to_string())
    }
}

impl Inner {
    // Allow callers to grab a fresh `subscribe_room` receiver so
    // out-of-band consumers (e.g. the bundled daemon example) can
    // also listen to room events.
    #[allow(dead_code)]
    pub fn node(&self) -> &Arc<Node> {
        &self.node
    }
    #[allow(dead_code)]
    pub async fn subscribe(&self, room: &RoomId) -> tokio::sync::broadcast::Receiver<Announcement> {
        self.node.subscribe_room(room)
    }
    #[allow(dead_code)]
    pub fn data_dir(&self) -> &Path {
        self.node.data_dir()
    }
    #[allow(dead_code)]
    pub async fn shutdown(&self) -> anyhow::Result<()> {
        self.node.shutdown().await.context("node shutdown")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn make_node(tmp: &tempfile::TempDir) -> Node {
        a3net_node::Node::builder(a3net_node::NodeConfig::new(
            tmp.path(),
            a3net_types::NodeId::random(),
        ))
        .build()
        .await
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handler_dispatches_list_rooms_and_init() {
        let tmp = tempfile::tempdir().unwrap();
        let node = make_node(&tmp).await;
        let rpc = NodeRpc::new(node);

        let r = rpc.handle("init", json!({})).await.unwrap();
        // `NodeInfo` doesn't implement `Deserialize` (only Serialize)
        // because its nested types are socket handles we don't want to
        // round-trip. Validate the wire JSON directly here.
        assert!(r["joinedRooms"].is_array());
        assert!(r["joinedRooms"].as_array().unwrap().is_empty());
        assert!(!r["nodeId"].as_str().unwrap().is_empty());

        let r = rpc.handle("list_rooms", json!({})).await.unwrap();
        assert_eq!(r, json!([]));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handler_join_and_leave_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let node = make_node(&tmp).await;
        let rpc = NodeRpc::new(node);

        rpc.handle("join", json!({"room": "lobby"})).await.unwrap();
        let r = rpc.handle("list_rooms", json!({})).await.unwrap();
        assert_eq!(r, json!(["lobby"]));

        rpc.handle("leave", json!({"room": "lobby"})).await.unwrap();
        let r = rpc.handle("list_rooms", json!({})).await.unwrap();
        assert_eq!(r, json!([]));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handler_unknown_method_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let node = make_node(&tmp).await;
        let rpc = NodeRpc::new(node);
        let err = rpc.handle("nope", json!({})).await.unwrap_err();
        assert!(err.contains("unknown method"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handler_peers_for_unknown_hash_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let node = make_node(&tmp).await;
        let rpc = NodeRpc::new(node);
        // Use a valid 32-byte hash. There are no peers and no
        // local copy so the result is an empty array.
        let h = ContentHash::from_bytes(b"unknown-hash-for-peers-test");
        let r = rpc
            .handle("peers_for", json!({"hash": h.as_hex()}))
            .await
            .unwrap();
        assert_eq!(r, json!([]));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handler_make_ticket_requires_mesh() {
        // `make_ticket` lazily starts the mesh. The test data dir is
        // a tempdir — mesh will pick an ephemeral port.
        let tmp = tempfile::tempdir().unwrap();
        let node = make_node(&tmp).await;
        let rpc = NodeRpc::new(node);
        let hash = ContentHash::from_bytes(b"x");
        let r = rpc
            .handle("make_ticket", json!({"hash": hash.as_hex()}))
            .await
            .unwrap();
        let s = r.as_str().unwrap();
        // The ticket format is opaque; we just check that it
        // round-trips and references the correct hash.
        let parsed = BlobTicket::parse(s).expect("parse ticket");
        assert_eq!(parsed.content_hash, hash);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn feed_method_returns_swarm_index() {
        let tmp = tempfile::tempdir().unwrap();
        let node = make_node(&tmp).await;
        let rpc = NodeRpc::new(node);
        let r = rpc.handle("feed", json!({"room": "lobby"})).await.unwrap();
        // The wire format is hand-rolled JSON, not the in-memory
        // struct. We assert the shape rather than the type.
        assert_eq!(r["room"], "lobby");
        assert_eq!(r["assets"].as_array().unwrap().len(), 0);
        assert!(r["peerMap"].is_object());
    }

    // ── agent.ask integration tests ──────────────────────────────────────────

    #[cfg(feature = "agent-v1")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn agent_ask_deny_when_no_model() {
        use a3net_node::agent::{AgentAclMode, NodeAgentBridge};

        let tmp = tempfile::tempdir().unwrap();
        let node = make_node(&tmp).await;

        // Set ACL to AllowAll so the ACL check passes
        node.agent_bridge().set_acl(AgentAclMode::AllowAll);

        let rpc = NodeRpc::new(node);
        let err = rpc
            .handle("agent.ask", json!({"peer": "abcd1234", "question": "hello?"}))
            .await
            .unwrap_err();
        // Should fail because no model is registered
        assert!(
            err.contains("no agent model registered"),
            "expected 'no agent model registered', got: {err}"
        );
    }

    #[cfg(feature = "agent-v1")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn agent_ask_deny_when_acl_deny_all() {
        use a3net_node::agent::AgentAclMode;

        let tmp = tempfile::tempdir().unwrap();
        let node = make_node(&tmp).await;
        // ACL stays at default DenyAll

        let rpc = NodeRpc::new(node);
        let err = rpc
            .handle(
                "agent.ask",
                json!({"peer": "abcd1234abcd1234abcd1234abcd1234", "question": "hello?"}),
            )
            .await
            .unwrap_err();
        assert!(
            err.contains("not permitted"),
            "expected 'not permitted', got: {err}"
        );
    }

    #[cfg(feature = "agent-v1")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn agent_ask_allows_when_peer_in_allowlist() {
        use a3net_agent::mock::MockChatModel;
        use a3net_node::agent::AgentAclMode;
        use std::sync::Arc;

        let tmp = tempfile::tempdir().unwrap();
        let node = make_node(&tmp).await;

        // Register a mock model
        node.agent_bridge()
            .register_model(Arc::new(MockChatModel::text("hello from mock!")));

        // Grant the specific peer
        node.agent_bridge()
            .grant_peer("peer_abc1234".to_string());

        let rpc = NodeRpc::new(node);
        let result = rpc
            .handle(
                "agent.ask",
                json!({"peer": "peer_abc1234", "question": "say hello"}),
            )
            .await
            .unwrap();

        // The mock returns "hello from mock!" as content
        let content = result.get("content").and_then(|v| v.as_str());
        assert_eq!(
            content, Some("hello from mock!"),
            "expected mock response, got: {result}"
        );
    }

    #[cfg(not(feature = "agent-v1"))]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn agent_ask_reports_feature_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let node = make_node(&tmp).await;
        let rpc = NodeRpc::new(node);
        let err = rpc
            .handle("agent.ask", json!({"peer": "x", "question": "?"}))
            .await
            .unwrap_err();
        assert!(
            err.contains("agent.v1 is not enabled"),
            "expected feature-not-enabled message, got: {err}"
        );
    }
}
