//! Topic-based pub/sub service over Unix socket JSON-RPC.
//!
//! Mirrors `P2pGossipService` from
//! `Exodus@src-backup/.../microservice/p2p_gossip_service.rs`. Methods:
//!
//! - `subscribe`        : `params.topic, subscriber_id`
//! - `unsubscribe`      : `params.topic, subscriber_id`
//! - `publish`          : `params.topic, payload` → `{published, message_id, timestamp}`
//! - `get_messages`     : `params.topic, limit?`  → `{messages: [...]}`
//! - `list_topics`      : `{}`                   → `{topics: [...]}`
//! - `get_subscribers`  : `params.topic`         → `{subscribers: [...]}`
//! - `node_info`        : `{}`                   → `{node_id, timestamp}`

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::server::{JsonRpcServer, JsonRpcServerHandle, RpcHandler};

#[derive(Debug, Clone)]
pub struct GossipIpcConfig {
    pub socket_path: PathBuf,
}

impl Default for GossipIpcConfig {
    fn default() -> Self {
        let mut socket_path = std::env::temp_dir();
        socket_path.push("adnet_gossip.sock");
        Self { socket_path }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GossipIpcMessage {
    pub topic: String,
    pub payload: Value,
    pub from_node: String,
    pub timestamp: u64,
    pub id: String,
}

/// In-memory pub/sub + JSON-RPC handler.
pub struct GossipIpcService {
    cfg: GossipIpcConfig,
    node_id: String,
    topics: Arc<Mutex<HashMap<String, Vec<GossipIpcMessage>>>>,
    subscribers: Arc<Mutex<HashMap<String, HashSet<String>>>>,
}

impl GossipIpcService {
    pub fn new(cfg: GossipIpcConfig) -> Self {
        Self {
            cfg,
            node_id: generate_node_id(),
            topics: Arc::new(Mutex::new(HashMap::new())),
            subscribers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Start the Unix socket server.
    pub async fn serve(self: Arc<Self>) -> Result<JsonRpcServerHandle, String> {
        JsonRpcServer::start(self.cfg.socket_path.clone(), self).await
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn socket_path(&self) -> &PathBuf {
        &self.cfg.socket_path
    }

    pub fn subscribe(&self, topic: String, subscriber_id: String) -> Result<(), String> {
        let mut subs = self.subscribers.lock().map_err(|e| format!("lock: {e}"))?;
        subs.entry(topic).or_default().insert(subscriber_id);
        Ok(())
    }

    pub fn unsubscribe(&self, topic: String, subscriber_id: String) -> Result<(), String> {
        let mut subs = self.subscribers.lock().map_err(|e| format!("lock: {e}"))?;
        if let Some(s) = subs.get_mut(&topic) {
            s.remove(&subscriber_id);
            if s.is_empty() {
                subs.remove(&topic);
            }
        }
        Ok(())
    }

    pub fn publish(&self, topic: String, payload: Value) -> Result<String, String> {
        let msg = GossipIpcMessage {
            topic: topic.clone(),
            payload,
            from_node: self.node_id.clone(),
            timestamp: now_secs(),
            id: generate_message_id(),
        };
        let mut topics = self.topics.lock().map_err(|e| format!("lock: {e}"))?;
        let entry = topics.entry(topic).or_default();
        entry.push(msg.clone());
        // Retain at most the last 100 messages per topic — matches Exodus.
        if entry.len() > 100 {
            let drain = entry.len() - 100;
            entry.drain(0..drain);
        }
        Ok(msg.id)
    }

    pub fn get_messages(&self, topic: String, limit: Option<usize>) -> Vec<GossipIpcMessage> {
        let topics = self.topics.lock().ok();
        let Some(topics) = topics else {
            return Vec::new();
        };
        let limit = limit.unwrap_or(50);
        topics
            .get(&topic)
            .map(|m| {
                if m.len() > limit {
                    m[m.len() - limit..].to_vec()
                } else {
                    m.clone()
                }
            })
            .unwrap_or_default()
    }

    pub fn list_topics(&self) -> Vec<String> {
        self.topics
            .lock()
            .map(|t| t.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn get_subscribers(&self, topic: String) -> Vec<String> {
        self.subscribers
            .lock()
            .ok()
            .and_then(|s| s.get(&topic).map(|set| set.iter().cloned().collect()))
            .unwrap_or_default()
    }
}

#[async_trait]
impl RpcHandler for GossipIpcService {
    async fn handle(&self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "subscribe" => {
                let topic = params
                    .get("topic")
                    .and_then(|t| t.as_str())
                    .ok_or("missing topic")?
                    .to_string();
                let id = params
                    .get("subscriber_id")
                    .and_then(|s| s.as_str())
                    .ok_or("missing subscriber_id")?
                    .to_string();
                self.subscribe(topic.clone(), id)?;
                Ok(json!({ "subscribed": true, "topic": topic }))
            }
            "unsubscribe" => {
                let topic = params
                    .get("topic")
                    .and_then(|t| t.as_str())
                    .ok_or("missing topic")?
                    .to_string();
                let id = params
                    .get("subscriber_id")
                    .and_then(|s| s.as_str())
                    .ok_or("missing subscriber_id")?
                    .to_string();
                self.unsubscribe(topic.clone(), id)?;
                Ok(json!({ "unsubscribed": true, "topic": topic }))
            }
            "publish" => {
                let topic = params
                    .get("topic")
                    .and_then(|t| t.as_str())
                    .ok_or("missing topic")?
                    .to_string();
                let payload = params.get("payload").cloned().ok_or("missing payload")?;
                let id = self.publish(topic, payload)?;
                Ok(json!({
                    "published": true,
                    "message_id": id,
                    "timestamp": now_secs(),
                }))
            }
            "get_messages" => {
                let topic = params
                    .get("topic")
                    .and_then(|t| t.as_str())
                    .ok_or("missing topic")?
                    .to_string();
                let limit = params
                    .get("limit")
                    .and_then(|l| l.as_u64())
                    .map(|n| n as usize);
                let msgs = self.get_messages(topic, limit);
                Ok(json!({ "messages": msgs }))
            }
            "list_topics" => Ok(json!({ "topics": self.list_topics() })),
            "get_subscribers" => {
                let topic = params
                    .get("topic")
                    .and_then(|t| t.as_str())
                    .ok_or("missing topic")?
                    .to_string();
                Ok(json!({ "subscribers": self.get_subscribers(topic) }))
            }
            "node_info" => Ok(json!({
                "node_id": self.node_id,
                "timestamp": now_secs(),
            })),
            other => Err(format!("unknown method: {other}")),
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn generate_node_id() -> String {
    format!("node_{:x}", now_secs())
}

fn generate_message_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("msg_{nanos:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::json_rpc_call;

    #[tokio::test]
    async fn subscribe_publish_get_messages() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("gossip.sock");
        let svc = Arc::new(GossipIpcService::new(GossipIpcConfig {
            socket_path: sock.clone(),
        }));
        let handle = Arc::clone(&svc).serve().await.unwrap();

        json_rpc_call(
            &sock,
            "gossip",
            "subscribe",
            json!({ "topic": "adnet-room-lobby", "subscriber_id": "c1" }),
        )
        .await
        .unwrap();

        let pub_resp = json_rpc_call(
            &sock,
            "gossip",
            "publish",
            json!({
                "topic": "adnet-room-lobby",
                "payload": {"hello": "world"}
            }),
        )
        .await
        .unwrap();
        assert_eq!(pub_resp["published"], true);

        let msgs = json_rpc_call(
            &sock,
            "gossip",
            "get_messages",
            json!({ "topic": "adnet-room-lobby", "limit": 10 }),
        )
        .await
        .unwrap();
        let arr = msgs["messages"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["payload"]["hello"], "world");

        handle.shutdown();
    }
}
