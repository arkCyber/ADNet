//! WebSocket support for real-time subscriptions and notifications.
//!
//! This module provides:
//! - PubSubService for event broadcasting
//! - Event types for subscriptions
//!
//! ## Message Format
//!
//! All messages are JSON-encoded with the following structure:
//! ```json
//! {
//!   "type": "message_type",
//!   "data": { ... }
//! }
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};

/// WebSocket message types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsMessage {
    /// Subscribe to a topic.
    Subscribe { topic: String },
    /// Unsubscribe from a topic.
    Unsubscribe { topic: String },
    /// Publish a message to a topic.
    Publish { topic: String, data: serde_json::Value },
    /// Ping message.
    Ping { data: Option<String> },
    /// Pong response.
    Pong { data: Option<String> },
    /// Error message.
    Error { message: String, code: u32 },
    /// Event notification.
    Event { topic: String, data: serde_json::Value },
}

/// Event topics for subscriptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventTopic {
    DhtProviderFound,
    DhtProviderAnnounced,
    BitswapWantlist,
    BitswapBlockReceived,
    SwarmPeerConnected,
    SwarmPeerDisconnected,
    PinStatusChanged,
    RepoGcStarted,
    RepoGcCompleted,
}

impl EventTopic {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventTopic::DhtProviderFound => "dht/provider-found",
            EventTopic::DhtProviderAnnounced => "dht/provider-announced",
            EventTopic::BitswapWantlist => "bitswap/wantlist",
            EventTopic::BitswapBlockReceived => "bitswap/block-received",
            EventTopic::SwarmPeerConnected => "swarm/peer-connected",
            EventTopic::SwarmPeerDisconnected => "swarm/peer-disconnected",
            EventTopic::PinStatusChanged => "pin/status-changed",
            EventTopic::RepoGcStarted => "repo/gc-started",
            EventTopic::RepoGcCompleted => "repo/gc-completed",
        }
    }
}

/// Event data for pub/sub notifications.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub topic: String,
    pub timestamp: u64,
    pub data: serde_json::Value,
}

/// Subscription for a WebSocket client.
#[derive(Debug, Clone, Default)]
pub struct Subscription {
    pub topics: Vec<String>,
}

/// Global pub/sub service for broadcasting events.
#[derive(Clone, Default)]
pub struct PubSubService {
    topics: Arc<RwLock<HashMap<String, broadcast::Sender<Event>>>>,
    client_counter: Arc<std::sync::atomic::AtomicUsize>,
}

impl PubSubService {
    /// Create a new pub/sub service.
    pub fn new() -> Self {
        Self {
            topics: Arc::new(RwLock::new(HashMap::new())),
            client_counter: Arc::new(std::sync::atomic::AtomicUsize::new(1)),
        }
    }

    /// Subscribe a client to a topic.
    pub async fn subscribe(&self, _client_id: usize, topic: &str) {
        let mut topics = self.topics.write().await;
        if !topics.contains_key(topic) {
            let (tx, _) = broadcast::channel(100);
            topics.insert(topic.to_string(), tx);
        }
    }

    /// Publish an event to a topic.
    pub async fn publish(&self, topic: &str, data: serde_json::Value) {
        let topics = self.topics.read().await;
        if let Some(tx) = topics.get(topic) {
            let event = Event {
                topic: topic.to_string(),
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
                data,
            };
            let _ = tx.send(event);
        }
    }

    /// Register a new client.
    pub async fn register_client(&self) -> usize {
        self.client_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// Get the broadcast sender for a topic.
    pub async fn get_topic_sender(&self, topic: &str) -> Option<broadcast::Sender<Event>> {
        let topics = self.topics.read().await;
        topics.get(topic).cloned()
    }

    /// Get all topics.
    pub async fn get_topics(&self) -> Vec<String> {
        let topics = self.topics.read().await;
        topics.keys().cloned().collect()
    }
}

/// Start the WebSocket server (placeholder for future implementation).
pub async fn start_websocket_server(
    _config: crate::GatewayConfig,
    pubsub: PubSubService,
    bind_addr: std::net::SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use bytes::Bytes;
    use http_body_util::Full;
    use hyper::{Request, Response, body::Incoming};
    use hyper_util::rt::TokioIo;
    use tracing::info;

    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    info!("Starting WebSocket server placeholder on {}", bind_addr);
    info!("PubSubService topics: {:?}", pubsub.get_topics().await);

    loop {
        let (conn, _addr) = listener.accept().await?;
        let io = TokioIo::new(conn);
        let pubsub = pubsub.clone();

        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |_req: Request<Incoming>| {
                let pubsub = pubsub.clone();
                async move {
                    let response = Response::builder()
                        .status(200)
                        .header("Content-Type", "text/html")
                        .body(Full::new(Bytes::from(r#"
                            <html><body>
                            <h1>ADNet WebSocket API</h1>
                            <p>WebSocket endpoint - use ws://host:port/</p>
                            </body></html>
                        "#)))
                        .unwrap();
                    Ok::<_, std::convert::Infallible>(response)
                }
            });

            if let Err(e) = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .await
            {
                tracing::error!("Connection error: {}", e);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ws_message_serialization() {
        let msg = WsMessage::Subscribe { topic: "test".to_string() };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("subscribe"));
        assert!(json.contains("test"));
    }

    #[test]
    fn test_ws_message_deserialization() {
        let json = r#"{"type":"subscribe","topic":"test"}"#;
        let msg: WsMessage = serde_json::from_str(json).unwrap();
        match msg {
            WsMessage::Subscribe { topic } => assert_eq!(topic, "test"),
            _ => panic!("expected Subscribe"),
        }
    }

    #[tokio::test]
    async fn test_pubsub_service() {
        let service = PubSubService::new();

        // First subscribe to create the topic
        service.subscribe(1, "test").await;

        // Then publish
        service.publish("test", serde_json::json!({"hello": "world"})).await;

        // Verify topic exists
        let sender = service.get_topic_sender("test").await;
        assert!(sender.is_some());

        // Verify all topics
        let topics = service.get_topics().await;
        assert!(topics.contains(&"test".to_string()));
    }

    #[test]
    fn test_event_topic() {
        assert_eq!(EventTopic::DhtProviderFound.as_str(), "dht/provider-found");
        assert_eq!(EventTopic::BitswapWantlist.as_str(), "bitswap/wantlist");
        assert_eq!(EventTopic::SwarmPeerConnected.as_str(), "swarm/peer-connected");
    }
}
