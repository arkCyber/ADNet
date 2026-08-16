//! AdnetFeedAdapter — Eliza agent adapter for the A3Net news/bulletin system.
//!
//! This adapter lets Eliza agents subscribe to industry topics, publish
//! AI-generated reports, and react to peer bulletins. The flow is:
//!
//! 1. `connect()` → opens the underlying `NewsService`, sends a
//!    presence announcement.
//! 2. `subscribe(topic)` → joins the corresponding gossip topic and
//!    starts receiving bulletins.
//! 3. `publish_report(...)` → builds a `BulletinItem`, persists it via
//!    the news service, broadcasts over gossip, and emits a
//!    `FeedEvent::Published`.
//!
//! ## Topic naming
//!
//! Topics follow the `a3net-news-{room}` convention. Direct category
//! subscriptions (`subscribe_to_category`) automatically map to the
//! canonical room for that category.

use a3net_types::bulletin::{
    BulletinItem, BulletinCategory, BulletinKind, BulletinSeverity, BulletinId, BulletinAttachment,
};
use a3net_types::node::NodeId;
use a3net_types::room::RoomId;
use a3net_types::topic::{Topic, topic_name};
use a3net_types::announce::AnnouncementPayload;
use a3net_types::content::ContentHash;
use a3net_gossip::GossipTransport;
use a3net_news::{NewsService, BulletinEnvelope, BulletinEvent};
use a3net_news::service::NewsServiceConfig;
use a3net_blobstore::BlobStore;
use crate::identity::AdnetIdentity;
use crate::error::{BridgeError, BridgeResult};
use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};
use tokio::task::JoinHandle;
use uuid::Uuid;

/// Domain-separated tag for feed-protocol hash functions.
const FEED_HASH_TAG: &[u8] = b"a3net-eliza-bridge/v1/feed";

/// News item received from the feed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedItem {
    pub id: String,
    pub author_id: NodeId,
    pub author_name: String,
    pub title: String,
    pub body: String,
    pub summary: String,
    pub category: BulletinCategory,
    pub severity: BulletinSeverity,
    pub tags: Vec<String>,
    pub published_at: i64,
    pub attachments: Vec<BulletinAttachment>,
}

impl From<BulletinItem> for FeedItem {
    fn from(item: BulletinItem) -> Self {
        Self {
            id: item.bulletin_id.to_string(),
            author_id: item.author_id,
            author_name: item.author_name,
            title: item.title,
            body: item.body,
            summary: item.summary,
            category: item.category,
            severity: item.severity,
            tags: item.tags,
            published_at: item.created_at.timestamp_millis(),
            attachments: item.attachments,
        }
    }
}

/// Feed event types.
#[derive(Debug, Clone)]
pub enum FeedEvent {
    NewItem(FeedItem),
    Subscribed { topic: String },
    Unsubscribed { topic: String },
    Published { item_id: String, category: BulletinCategory },
    Correction { original_id: String, corrected: FeedItem },
    Retraction { original_id: String, retraction: FeedItem },
    Tipped { item_id: String, from_node: NodeId, amount: String },
    Error(String),
}

#[derive(Debug, Clone)]
pub struct FeedConfig {
    pub default_categories: Vec<BulletinCategory>,
    pub display_name: String,
    pub room_id: RoomId,
    pub min_post_interval_secs: u64,
    pub max_cache_size: usize,
    pub auto_subscribe_trending: bool,
    pub event_capacity: usize,
    pub show_ai_badge: bool,
    pub agent_id_label: String,
    pub validate_publish: bool,
}

impl Default for FeedConfig {
    fn default() -> Self {
        Self {
            default_categories: vec![BulletinCategory::Tech, BulletinCategory::General],
            display_name: "A3Net AI".to_string(),
            room_id: RoomId::new("general"),
            min_post_interval_secs: 60,
            max_cache_size: 1000,
            auto_subscribe_trending: false,
            event_capacity: 512,
            show_ai_badge: true,
            agent_id_label: "eliza-agent".to_string(),
            validate_publish: true,
        }
    }
}

/// Per-topic subscription state.
#[derive(Debug, Clone)]
struct TopicSub {
    topic: Topic,
    name: String,
    last_post_at: Option<i64>,
}

impl TopicSub {
    /// Empty placeholder used by `or_insert_with` when registering
    /// a new topic for the first time.
    fn placeholder() -> Self {
        Self {
            topic: Topic::from_label(""),
            name: String::new(),
            last_post_at: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TipRecord {
    pub item_id: String,
    pub from_node: NodeId,
    pub amount: String,
    pub timestamp: i64,
}

/// Adapter that exposes the A3Net news system to Eliza agents.
pub struct AdnetFeedAdapter {
    identity: Arc<AdnetIdentity>,
    config: FeedConfig,
    news_service: Arc<RwLock<Option<Arc<NewsService>>>>,
    gossip_transport: Arc<RwLock<Option<Arc<dyn GossipTransport>>>>,
    blob_store: Arc<RwLock<Option<BlobStore>>>,
    event_sender: broadcast::Sender<FeedEvent>,
    subscriptions: Arc<RwLock<HashSet<String>>>,
    topic_index: Arc<RwLock<std::collections::HashMap<String, TopicSub>>>,
    item_cache: Arc<RwLock<std::collections::HashMap<String, FeedItem>>>,
    tips: Arc<RwLock<Vec<TipRecord>>>,
    connected: Arc<RwLock<bool>>,
    listener_tasks: Arc<RwLock<Vec<JoinHandle<()>>>>,
    cancel: Arc<RwLock<Option<broadcast::Sender<()>>>>,
}

impl AdnetFeedAdapter {
    pub async fn new(identity: AdnetIdentity, config: FeedConfig) -> BridgeResult<Self> {
        let (tx, _rx) = broadcast::channel(config.event_capacity);
        Ok(Self {
            identity: Arc::new(identity),
            config,
            news_service: Arc::new(RwLock::new(None)),
            gossip_transport: Arc::new(RwLock::new(None)),
            blob_store: Arc::new(RwLock::new(None)),
            event_sender: tx,
            subscriptions: Arc::new(RwLock::new(HashSet::new())),
            topic_index: Arc::new(RwLock::new(std::collections::HashMap::new())),
            item_cache: Arc::new(RwLock::new(std::collections::HashMap::new())),
            tips: Arc::new(RwLock::new(Vec::new())),
            connected: Arc::new(RwLock::new(false)),
            listener_tasks: Arc::new(RwLock::new(Vec::new())),
            cancel: Arc::new(RwLock::new(None)),
        })
    }

    /// Attach a `NewsService` (preferred when the host already has
    /// one running).
    pub async fn with_news_service(self, service: Arc<NewsService>) -> Self {
        *self.news_service.write().await = Some(service);
        self
    }

    /// Attach a gossip transport for direct broadcast/inbound
    /// wrapping. The `NewsService` already subscribes to the
    /// gossip topic, so this is optional.
    pub async fn with_gossip_transport(self, transport: Arc<dyn GossipTransport>) -> Self {
        *self.gossip_transport.write().await = Some(transport);
        self
    }

    /// Attach a blob store for large attachments.
    pub async fn with_blob_store(self, store: BlobStore) -> Self {
        *self.blob_store.write().await = Some(store);
        self
    }

    /// Connect to the news network.
    pub async fn connect(&self) -> BridgeResult<()> {
        if *self.connected.read().await {
            return Ok(());
        }

        // Auto-subscribe to default categories unless they were
        // already explicitly subscribed.
        let categories = self.config.default_categories.clone();
        for cat in categories {
            self.subscribe_to_category(cat).await?;
        }

        // Set up cancel channel.
        let (cancel_tx, _) = broadcast::channel(1);
        *self.cancel.write().await = Some(cancel_tx);

        let mut connected = self.connected.write().await;
        *connected = true;
        tracing::info!(node_id = %self.identity.node_id(), "Feed Adapter connected");
        Ok(())
    }

    /// Disconnect from the news network.
    pub async fn disconnect(&self) -> BridgeResult<()> {
        if !*self.connected.read().await {
            return Ok(());
        }

        if let Some(tx) = self.cancel.read().await.as_ref() {
            let _ = tx.send(());
        }
        let mut tasks = self.listener_tasks.write().await;
        for h in tasks.drain(..) {
            h.abort();
        }

        let mut subs = self.subscriptions.write().await;
        subs.clear();
        let mut idx = self.topic_index.write().await;
        idx.clear();

        let mut connected = self.connected.write().await;
        *connected = false;
        tracing::info!(node_id = %self.identity.node_id(), "Feed Adapter disconnected");
        Ok(())
    }

    pub async fn is_connected(&self) -> bool {
        *self.connected.read().await
    }

    pub fn node_id(&self) -> NodeId {
        self.identity.node_id()
    }

    // ============================================================
    // Topic naming
    // ============================================================

    /// Build a topic id for a category.
    pub fn category_topic(category: BulletinCategory) -> Topic {
        let room = format!("{:?}", category).to_lowercase();
        let label = topic_name("news", &room);
        Topic::from_label(&label)
    }

    /// Build a topic id for a room.
    pub fn room_topic(room: &RoomId) -> Topic {
        let label = topic_name("news", room.as_str());
        Topic::from_label(&label)
    }

    // ============================================================
    // Subscriptions
    // ============================================================

    pub async fn subscribe(&self, topic: &str) -> BridgeResult<()> {
        let topic_str = topic.to_lowercase();
        let topic_id = Topic::from_label(&topic_str);

        // Already subscribed.
        {
            let mut subs = self.subscriptions.write().await;
            if !subs.insert(topic_str.clone()) {
                return Ok(());
            }
        }

        // Join the gossip topic.
        if let Some(transport) = self.gossip_transport.read().await.as_ref() {
            transport
                .join(topic_id.clone(), self.node_id())
                .await
                .map_err(|e| BridgeError::Gossip(format!("join topic: {e}")))?;
        }

        // Track the topic.
        self.topic_index
            .write()
            .await
            .insert(topic_str.clone(), TopicSub {
                topic: topic_id,
                name: topic_str.clone(),
                last_post_at: None,
            });

        let _ = self.event_sender.send(FeedEvent::Subscribed {
            topic: topic_str.clone(),
        });
        tracing::info!(
            node_id = %self.node_id(),
            topic = %topic_str,
            "Subscribed to topic"
        );
        Ok(())
    }

    pub async fn subscribe_to_category(&self, category: BulletinCategory) -> BridgeResult<()> {
        let topic_label = format!("news-{}", category.as_str());
        self.subscribe(&topic_label).await
    }

    pub async fn unsubscribe(&self, topic: &str) -> BridgeResult<()> {
        let topic_str = topic.to_lowercase();
        let mut subs = self.subscriptions.write().await;
        if subs.remove(&topic_str) {
            if let Some(idx) = self.topic_index.write().await.remove(&topic_str) {
                if let Some(transport) = self.gossip_transport.read().await.as_ref() {
                    let _ = transport.leave(idx.topic).await;
                }
            }
            let _ = self.event_sender.send(FeedEvent::Unsubscribed { topic: topic_str.clone() });
        }
        Ok(())
    }

    pub async fn get_subscriptions(&self) -> Vec<String> {
        let subs = self.subscriptions.read().await;
        subs.iter().cloned().collect()
    }

    // ============================================================
    // Publishing
    // ============================================================

    /// Publish a report to the news system.
    pub async fn publish_report(
        &self,
        title: &str,
        body: &str,
        category: BulletinCategory,
        tags: Vec<String>,
    ) -> BridgeResult<String> {
        self.ensure_connected().await?;
        self.validate_title(title)?;
        self.validate_body(body)?;
        self.check_rate_limit().await?;

        let uuid = Uuid::new_v4();
        let nonce = uuid.as_bytes();
        let node_id = self.node_id();
        let timestamp = Utc::now();

        // Build summary (first 240 chars of body).
        let summary = if body.len() > 240 {
            let cut = body.char_indices().take_while(|(i, _)| *i < 240).last();
            let end = cut.map(|(i, c)| i + c.len_utf8()).unwrap_or(0);
            format!("{}…", &body[..end])
        } else {
            body.to_string()
        };

        // Reports are routed to the category's canonical room so that
        // callers can fetch them back via `get_feed("news-{category}")`.
        let room_id = RoomId::new(category.as_str());

        let item = BulletinItem::new(
            BulletinKind::NewsArticle,
            category,
            BulletinSeverity::Info,
            room_id,
            node_id.clone(),
            title.to_string(),
            summary,
            body.to_string(),
            nonce,
            None,
        )
        .map_err(|e| BridgeError::NewsService(e.to_string()))?;

        let item = item
            .with_author_name(&self.config.display_name)
            .with_tags(sanitize_tags(&tags))
            .with_lang("en");

        // Stamp integrity hash so receivers can verify body.
        let mut item = item;
        item.stamp_integrity_hash();

        // If a news service is attached, persist + broadcast via it.
        let item_id = if let Some(svc) = self.news_service.read().await.as_ref() {
            let stored = svc
                .publish(item.clone())
                .await
                .map_err(|e| BridgeError::NewsService(e.to_string()))?;
            stored.bulletin_id.to_string()
        } else {
            // No service: broadcast over gossip directly.
            self.broadcast_bulletin(&item).await?;
            item.bulletin_id.to_string()
        };

        // Update last-post timestamp for rate limiting.
        let key = self.config.room_id.as_str().to_string();
        let mut guard = self.topic_index.write().await;
        let entry = guard.entry(key).or_insert_with(TopicSub::placeholder);
        entry.last_post_at = Some(timestamp.timestamp());
        drop(guard);

        let _ = self.event_sender.send(FeedEvent::Published {
            item_id: item_id.clone(),
            category,
        });

        tracing::info!(
            node_id = %node_id,
            title = %title,
            category = ?category,
            "Published report"
        );
        Ok(item_id)
    }

    /// Publish an alert (urgent bulletin).
    pub async fn publish_alert(
        &self,
        title: &str,
        body: &str,
        severity: BulletinSeverity,
        tags: Vec<String>,
    ) -> BridgeResult<String> {
        self.ensure_connected().await?;
        self.validate_title(title)?;
        self.validate_body(body)?;
        self.check_rate_limit().await?;

        let uuid = Uuid::new_v4();
        let nonce = uuid.as_bytes();
        let node_id = self.node_id();
        let timestamp = Utc::now();

        // Alerts are routed to the General category's room so callers
        // can fetch them via `get_feed("news-general")`.
        let room_id = RoomId::new(BulletinCategory::General.as_str());

        let mut item = BulletinItem::new(
            BulletinKind::Announcement,
            BulletinCategory::General,
            severity,
            room_id,
            node_id,
            title.to_string(),
            format!("[{}] {}", severity.as_str(), body),
            body.to_string(),
            nonce,
            None,
        )
        .map_err(|e| BridgeError::NewsService(e.to_string()))?;

        item = item
            .with_author_name(&self.config.display_name)
            .with_tags(sanitize_tags(&tags));

        let item_id = if let Some(svc) = self.news_service.read().await.as_ref() {
            let stored = svc
                .publish(item.clone())
                .await
                .map_err(|e| BridgeError::NewsService(e.to_string()))?;
            stored.bulletin_id.to_string()
        } else {
            self.broadcast_bulletin(&item).await?;
            item.bulletin_id.to_string()
        };

        let key = self.config.room_id.as_str().to_string();
        let mut guard = self.topic_index.write().await;
        let entry = guard.entry(key).or_insert_with(TopicSub::placeholder);
        entry.last_post_at = Some(timestamp.timestamp());
        drop(guard);

        let _ = self.event_sender.send(FeedEvent::Published {
            item_id: item_id.clone(),
            category: BulletinCategory::General,
        });
        Ok(item_id)
    }

    /// Publish a report with blob attachments.
    pub async fn publish_with_blobs(
        &self,
        title: &str,
        body: &str,
        category: BulletinCategory,
        tags: Vec<String>,
        blobs: Vec<(String, Bytes, String)>, // (name, bytes, mime)
    ) -> BridgeResult<(String, Vec<String>)> {
        self.ensure_connected().await?;
        let mut attachment_ids: Vec<String> = Vec::new();
        let mut attachments: Vec<BulletinAttachment> = Vec::new();

        let store = self.blob_store.read().await.clone();
        for (file_name, bytes, mime) in blobs {
            if file_name.is_empty() {
                return Err(BridgeError::InvalidMessage("attachment file_name empty".into()));
            }
            let id = match store.as_ref() {
                Some(s) => {
                    let (content_hash, _) = s
                        .put_bytes_sync(&bytes)
                        .map_err(|e| BridgeError::Gossip(format!("blob put: {e}")))?;
                    let id = content_hash.as_hex().to_string();
                    let attachment = BulletinAttachment {
                        attachment_id: id.clone(),
                        content_hash,
                        mime_type: mime,
                        file_name,
                        file_size: bytes.len() as u64,
                        caption: None,
                    };
                    attachment_ids.push(id.clone());
                    attachment
                }
                None => {
                    // No blob store — store inline via content_hash.
                    let content_hash = ContentHash::from_bytes(blake3::hash(&bytes).as_bytes());
                    let att = BulletinAttachment {
                        attachment_id: Uuid::new_v4().to_string(),
                        content_hash,
                        mime_type: mime,
                        file_name,
                        file_size: bytes.len() as u64,
                        caption: None,
                    };
                    let id = att.attachment_id.clone();
                    attachment_ids.push(id.clone());
                    att
                }
            };
            attachments.push(id);
        }

        let mut item = BulletinItem::new(
            BulletinKind::NewsArticle,
            category,
            BulletinSeverity::Info,
            RoomId::new(category.as_str()),
            self.node_id(),
            title.to_string(),
            // Summary must be non-empty per BulletinItem::validate;
            // fall back to a truncated body when caller did not
            // supply one. We synthesise a summary from the body so
            // the persistence layer accepts the record.
            {
                let cap = body.len().min(200);
                body[..cap].to_string()
            },
            body.to_string(),
            Uuid::new_v4().as_bytes(),
            None,
        )
        .map_err(|e| BridgeError::NewsService(e.to_string()))?;

        item = item
            .with_author_name(&self.config.display_name)
            .with_tags(sanitize_tags(&tags))
            .with_attachments(attachments);

        let item_id = self.publish_bulletin(item).await?;
        Ok((item_id, attachment_ids))
    }

    /// Publish a correction that supersedes an existing bulletin.
    pub async fn publish_correction(
        &self,
        target_id: &BulletinId,
        title: &str,
        body: &str,
        tags: Vec<String>,
    ) -> BridgeResult<String> {
        self.ensure_connected().await?;
        let item = BulletinItem::new(
            BulletinKind::Correction,
            BulletinCategory::General,
            BulletinSeverity::Info,
            RoomId::new(BulletinCategory::General.as_str()),
            self.node_id(),
            title.to_string(),
            "Correction".to_string(),
            body.to_string(),
            Uuid::new_v4().as_bytes(),
            Some(target_id.clone()),
        )
        .map_err(|e| BridgeError::NewsService(e.to_string()))?
        .with_author_name(&self.config.display_name)
        .with_tags(sanitize_tags(&tags));
        self.publish_bulletin(item).await
    }

    /// Internal helper to publish a pre-built `BulletinItem`.
    async fn publish_bulletin(&self, item: BulletinItem) -> BridgeResult<String> {
        let item_id = if let Some(svc) = self.news_service.read().await.as_ref() {
            let stored = svc
                .publish(item.clone())
                .await
                .map_err(|e| BridgeError::NewsService(e.to_string()))?;
            stored.bulletin_id.to_string()
        } else {
            self.broadcast_bulletin(&item).await?;
            item.bulletin_id.to_string()
        };
        let _ = self.event_sender.send(FeedEvent::Published {
            item_id: item_id.clone(),
            category: item.category,
        });
        Ok(item_id)
    }

    /// Publish a retraction superseding a previous bulletin.
    pub async fn publish_retraction(
        &self,
        target_id: &BulletinId,
        reason: &str,
    ) -> BridgeResult<String> {
        self.ensure_connected().await?;
        let item = BulletinItem::new(
            BulletinKind::Retraction,
            BulletinCategory::General,
            BulletinSeverity::Critical,
            RoomId::new(BulletinCategory::General.as_str()),
            self.node_id(),
            "Retraction".to_string(),
            format!("Retracted: {reason}"),
            reason.to_string(),
            Uuid::new_v4().as_bytes(),
            Some(target_id.clone()),
        )
        .map_err(|e| BridgeError::NewsService(e.to_string()))?
        .with_author_name(&self.config.display_name)
        .with_retraction_reason(reason);
        self.publish_bulletin(item).await
    }

    // ============================================================
    // Reading
    // ============================================================

    /// Get recent items from a topic.
    pub async fn get_feed(&self, topic: &str, limit: usize) -> BridgeResult<Vec<FeedItem>> {
        if let Some(svc) = self.news_service.read().await.as_ref() {
            let room = RoomId::new(topic.trim_start_matches("news-"));
            let items = svc
                .timeline(&room, None, limit.max(1))
                .map_err(|e| BridgeError::NewsService(e.to_string()))?;
            return Ok(items.into_iter().map(|s| s.item.into()).collect());
        }

        // Fallback: in-memory cache.
        let cache = self.item_cache.read().await;
        let mut out: Vec<FeedItem> = cache
            .values()
            .filter(|item| {
                let topic_label = format!("news-{}", item.category.as_str());
                topic_label == topic || topic == "news-all"
            })
            .cloned()
            .collect();
        out.sort_by(|a, b| b.published_at.cmp(&a.published_at));
        out.truncate(limit);
        Ok(out)
    }

    /// Get items by category.
    pub async fn get_by_category(
        &self,
        category: BulletinCategory,
        limit: usize,
    ) -> BridgeResult<Vec<FeedItem>> {
        self.get_feed(&format!("news-{}", category.as_str()), limit).await
    }

    /// Search items by keyword across title, body, and tags.
    pub async fn search(&self, query: &str, limit: usize) -> BridgeResult<Vec<FeedItem>> {
        let q = query.to_lowercase();
        let cache = self.item_cache.read().await;
        let mut out: Vec<FeedItem> = cache
            .values()
            .filter(|item| {
                item.title.to_lowercase().contains(&q)
                    || item.body.to_lowercase().contains(&q)
                    || item.summary.to_lowercase().contains(&q)
                    || item.tags.iter().any(|t| t.to_lowercase().contains(&q))
            })
            .cloned()
            .collect();
        out.sort_by(|a, b| b.published_at.cmp(&a.published_at));
        out.truncate(limit);
        Ok(out)
    }

    /// Resolve a specific bulletin by id.
    pub async fn get_by_id(&self, id: &BulletinId) -> BridgeResult<Option<FeedItem>> {
        if let Some(svc) = self.news_service.read().await.as_ref() {
            // Try the configured room first; fall back to scanning
            // every room because we publish into category-specific
            // rooms rather than always into `self.config.room_id`.
            if let Some(stored) = svc
                .get(&self.config.room_id, id)
                .map_err(|e| BridgeError::NewsService(e.to_string()))?
            {
                return Ok(Some(stored.item.into()));
            }
            if let Some(stored) = svc
                .get_any(id)
                .map_err(|e| BridgeError::NewsService(e.to_string()))?
            {
                return Ok(Some(stored.item.into()));
            }
            return Ok(None);
        }
        Ok(self.item_cache.read().await.get(&id.to_string()).cloned())
    }

    /// Mark a bulletin as read.
    pub async fn mark_read(&self, id: &BulletinId) -> BridgeResult<()> {
        if let Some(svc) = self.news_service.read().await.as_ref() {
            svc.mark_read(&self.config.room_id, id)
                .map_err(|e| BridgeError::NewsService(e.to_string()))?;
        }
        Ok(())
    }

    // ============================================================
    // Tips
    // ============================================================

    /// Receive a tip (registered in the local ledger).
    pub fn record_tip(&self, item_id: String, from_node: NodeId, amount: String) {
        let tip = TipRecord {
            item_id: item_id.clone(),
            from_node: from_node.clone(),
            amount: amount.clone(),
            timestamp: Utc::now().timestamp(),
        };
        if let Ok(mut tips) = self.tips.try_write() {
            tips.push(tip);
        }
        let _ = self.event_sender.send(FeedEvent::Tipped {
            item_id,
            from_node,
            amount,
        });
    }

    /// List all tips received.
    pub async fn tips(&self) -> Vec<TipRecord> {
        self.tips.read().await.clone()
    }

    // ============================================================
    // Subscriptions / listeners
    // ============================================================

    pub async fn subscribe_events(&self) -> broadcast::Receiver<FeedEvent> {
        self.event_sender.subscribe()
    }

    /// Forward feed events to a [`FeedEventHandler`].
    pub async fn set_event_handler<H>(self, handler: Arc<H>) -> BridgeResult<Self>
    where
        H: FeedEventHandler + Send + Sync + 'static,
    {
        let mut receiver = self.event_sender.subscribe();
        tokio::spawn(async move {
            while let Ok(ev) = receiver.recv().await {
                handler.on_feed_event(ev).await;
            }
        });
        Ok(self)
    }

    /// Start the feed listener: pull bulletins from the news service
    /// and emit them as `FeedEvent`s.
    pub async fn start_listener(&self) -> BridgeResult<()> {
        self.ensure_connected().await?;
        let news_service = self
            .news_service
            .read()
            .await
            .clone()
            .ok_or_else(|| BridgeError::NotConnected)?;

        let (cancel_tx, mut cancel_rx) = broadcast::channel(1);
        *self.cancel.write().await = Some(cancel_tx);

        let event_sender = self.event_sender.clone();
        let item_cache = self.item_cache.clone();
        let max_cache = self.config.max_cache_size;

        let mut news_rx = news_service.subscribe();
        let mut tasks = self.listener_tasks.write().await;
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel_rx.recv() => break,
                    ev = news_rx.recv() => {
                        let Ok(ev) = ev else { break };
                        let (item, event) = match ev {
                            BulletinEvent::Insert(item) => {
                                let feed_item: FeedItem = item.clone().into();
                                (Some(feed_item.clone()), FeedEvent::NewItem(feed_item))
                            }
                            BulletinEvent::Correction { corrected, .. } => {
                                let feed_item: FeedItem = corrected.clone().into();
                                (Some(feed_item.clone()), FeedEvent::Correction {
                                    original_id: corrected.bulletin_id.to_string(),
                                    corrected: feed_item,
                                })
                            }
                            BulletinEvent::Retraction { retraction, .. } => {
                                let feed_item: FeedItem = retraction.clone().into();
                                (Some(feed_item.clone()), FeedEvent::Retraction {
                                    original_id: retraction.bulletin_id.to_string(),
                                    retraction: feed_item,
                                })
                            }
                            _ => continue,
                        };
                        if let Some(item) = item {
                            let mut cache = item_cache.write().await;
                            if cache.len() >= max_cache {
                                // Drop oldest by timestamp.
                                if let Some(oldest) = cache
                                    .iter()
                                    .min_by_key(|(_, v)| v.published_at)
                                    .map(|(k, _)| k.clone())
                                {
                                    cache.remove(&oldest);
                                }
                            }
                            cache.insert(item.id.clone(), item);
                        }
                        let _ = event_sender.send(event);
                    }
                }
            }
        });
        tasks.push(handle);

        // Listener for direct gossip payloads (when no NewsService is wired).
        if let Some(transport) = self.gossip_transport.read().await.clone() {
            for topic_str in self.subscriptions.read().await.iter() {
                let topic = Topic::from_label(topic_str);
                let mut rx = transport.subscribe(topic.clone());
                let event_sender = self.event_sender.clone();
                let item_cache = self.item_cache.clone();
                let max_cache = self.config.max_cache_size;
                let mut cancel_rx = self
                    .cancel
                    .read()
                    .await
                    .as_ref()
                    .map(|tx| tx.subscribe())
                    .unwrap_or_else(|| broadcast::channel(1).1);
                let handle = tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            _ = cancel_rx.recv() => break,
                            payload = rx.recv() => {
                                let Ok(payload) = payload else { break };
                                let parsed: Result<BulletinEnvelope, _> =
                                    serde_json::from_value(payload.payload.clone());
                                if let Ok(env) = parsed {
                                    let item: FeedItem = env.item.into();
                                    let mut cache = item_cache.write().await;
                                    if cache.len() >= max_cache {
                                        if let Some(oldest) = cache
                                            .iter()
                                            .min_by_key(|(_, v)| v.published_at)
                                            .map(|(k, _)| k.clone())
                                        {
                                            cache.remove(&oldest);
                                        }
                                    }
                                    cache.insert(item.id.clone(), item.clone());
                                    let _ = event_sender.send(FeedEvent::NewItem(item));
                                }
                            }
                        }
                    }
                });
                tasks.push(handle);
            }
        }

        tracing::info!(node_id = %self.node_id(), "Feed listener started");
        Ok(())
    }

    pub async fn stop_listener(&self) {
        if let Some(tx) = self.cancel.read().await.as_ref() {
            let _ = tx.send(());
        }
        let mut tasks = self.listener_tasks.write().await;
        for h in tasks.drain(..) {
            h.abort();
        }
        tracing::info!(node_id = %self.node_id(), "Feed listener stopped");
    }

    // ============================================================
    // Helpers
    // ============================================================

    async fn broadcast_bulletin(&self, item: &BulletinItem) -> BridgeResult<()> {
        let transport = self
            .gossip_transport
            .read()
            .await
            .clone()
            .ok_or_else(|| BridgeError::NotConnected)?;
        let envelope = BulletinEnvelope::wrap(item.clone(), self.node_id());
        let payload = AnnouncementPayload {
            from_node: self.node_id(),
            payload: serde_json::to_value(&envelope).map_err(BridgeError::Serialization)?,
        };
        let topic_str = topic_name("news", self.config.room_id.as_str());
        let topic = Topic::from_label(&topic_str);
        let _ = transport.join(topic.clone(), self.node_id()).await;
        transport
            .broadcast(topic, payload)
            .await
            .map_err(|e| BridgeError::Gossip(format!("bulletin broadcast: {e}")))?;
        Ok(())
    }

    async fn check_rate_limit(&self) -> BridgeResult<()> {
        let interval = self.config.min_post_interval_secs as i64;
        if interval <= 0 {
            return Ok(());
        }
        let key = self.config.room_id.as_str().to_string();
        let now = Utc::now().timestamp();
        let mut idx = self.topic_index.write().await;
        let entry = idx.entry(key).or_insert_with(TopicSub::placeholder);
        if let Some(last) = entry.last_post_at {
            if now - last < interval {
                return Err(BridgeError::RateLimited(format!(
                    "wait {}s before next post",
                    interval - (now - last)
                )));
            }
        }
        Ok(())
    }

    fn validate_title(&self, title: &str) -> BridgeResult<()> {
        if title.is_empty() {
            return Err(BridgeError::InvalidMessage("empty title".into()));
        }
        if title.len() > 256 {
            return Err(BridgeError::InvalidMessage("title too long".into()));
        }
        Ok(())
    }

    fn validate_body(&self, body: &str) -> BridgeResult<()> {
        if body.is_empty() {
            return Err(BridgeError::InvalidMessage("empty body".into()));
        }
        if body.len() > 256 * 1024 {
            return Err(BridgeError::InvalidMessage("body exceeds 256 KiB".into()));
        }
        Ok(())
    }

    async fn ensure_connected(&self) -> BridgeResult<()> {
        if !*self.connected.read().await {
            return Err(BridgeError::NotConnected);
        }
        Ok(())
    }

    /// Synchronous wait helper.
    pub async fn wait_for_event(&self, timeout: Duration) -> BridgeResult<FeedEvent> {
        let mut rx = self.event_sender.subscribe();
        match tokio::time::timeout(timeout, rx.recv()).await {
            Ok(Ok(ev)) => Ok(ev),
            Ok(Err(_)) => Err(BridgeError::Cancelled),
            Err(_) => Err(BridgeError::Timeout(timeout.as_secs())),
        }
    }

    /// Generate tools for Eliza agent registration.
    pub fn generate_eliza_tools(&self) -> Vec<FeedTool> {
        vec![
            FeedTool {
                name: "subscribe_topic".to_string(),
                description: "Subscribe to a news topic".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "topic": {"type": "string", "description": "Topic name"}
                    },
                    "required": ["topic"]
                }),
            },
            FeedTool {
                name: "subscribe_category".to_string(),
                description: "Subscribe to a bulletin category".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "category": {"type": "string", "description": "Category (tech, general, economy, ...)"}
                    },
                    "required": ["category"]
                }),
            },
            FeedTool {
                name: "get_feed".to_string(),
                description: "Get recent items from a topic".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "topic": {"type": "string"},
                        "limit": {"type": "number"}
                    },
                    "required": ["topic"]
                }),
            },
            FeedTool {
                name: "search_news".to_string(),
                description: "Search bulletins by keyword".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"},
                        "limit": {"type": "number"}
                    },
                    "required": ["query"]
                }),
            },
            FeedTool {
                name: "publish_report".to_string(),
                description: "Publish an AI-generated report".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "title": {"type": "string"},
                        "body": {"type": "string"},
                        "category": {"type": "string"},
                        "tags": {"type": "array", "items": {"type": "string"}}
                    },
                    "required": ["title", "body", "category"]
                }),
            },
            FeedTool {
                name: "publish_alert".to_string(),
                description: "Publish a breaking news alert".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "title": {"type": "string"},
                        "body": {"type": "string"},
                        "severity": {"type": "string", "description": "info|notable|important|critical"},
                        "tags": {"type": "array", "items": {"type": "string"}}
                    },
                    "required": ["title", "body", "severity"]
                }),
            },
            FeedTool {
                name: "publish_correction".to_string(),
                description: "Publish a correction to a previous bulletin".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "target_id": {"type": "string", "description": "BulletinId of original"},
                        "title": {"type": "string"},
                        "body": {"type": "string"}
                    },
                    "required": ["target_id", "title", "body"]
                }),
            },
            FeedTool {
                name: "publish_with_blobs".to_string(),
                description: "Publish a report with blob attachments".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "title": {"type": "string"},
                        "body": {"type": "string"},
                        "category": {"type": "string"},
                        "tags": {"type": "array", "items": {"type": "string"}},
                        "blobs": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "file_name": {"type": "string"},
                                    "data_base64": {"type": "string"},
                                    "mime_type": {"type": "string"}
                                }
                            }
                        }
                    },
                    "required": ["title", "body", "category", "blobs"]
                }),
            },
            FeedTool {
                name: "get_subscriptions".to_string(),
                description: "List current topic subscriptions".to_string(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            },
            FeedTool {
                name: "mark_read".to_string(),
                description: "Mark a bulletin as read".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "item_id": {"type": "string"}
                    },
                    "required": ["item_id"]
                }),
            },
        ]
    }
}

/// Builder for `AdnetFeedAdapter`.
pub struct FeedAdapterBuilder {
    identity: AdnetIdentity,
    config: FeedConfig,
    news_service: Option<Arc<NewsService>>,
    gossip_transport: Option<Arc<dyn GossipTransport>>,
    blob_store: Option<BlobStore>,
}

impl FeedAdapterBuilder {
    pub fn new(identity: AdnetIdentity) -> Self {
        Self {
            identity,
            config: FeedConfig::default(),
            news_service: None,
            gossip_transport: None,
            blob_store: None,
        }
    }

    pub fn display_name(mut self, name: impl Into<String>) -> Self {
        self.config.display_name = name.into();
        self
    }

    pub fn categories(mut self, categories: Vec<BulletinCategory>) -> Self {
        self.config.default_categories = categories;
        self
    }

    pub fn min_post_interval(mut self, secs: u64) -> Self {
        self.config.min_post_interval_secs = secs;
        self
    }

    pub fn room_id(mut self, room: RoomId) -> Self {
        self.config.room_id = room;
        self
    }

    pub fn with_news_service(mut self, svc: Arc<NewsService>) -> Self {
        self.news_service = Some(svc);
        self
    }

    pub fn with_gossip_transport(mut self, t: Arc<dyn GossipTransport>) -> Self {
        self.gossip_transport = Some(t);
        self
    }

    pub fn with_blob_store(mut self, store: BlobStore) -> Self {
        self.blob_store = Some(store);
        self
    }

    pub async fn build(self) -> BridgeResult<AdnetFeedAdapter> {
        let mut adapter = AdnetFeedAdapter::new(self.identity, self.config).await?;
        if let Some(svc) = self.news_service {
            adapter = adapter.with_news_service(svc).await;
        }
        if let Some(t) = self.gossip_transport {
            adapter = adapter.with_gossip_transport(t).await;
        }
        if let Some(store) = self.blob_store {
            adapter = adapter.with_blob_store(store).await;
        }
        Ok(adapter)
    }
}

/// Builder for `NewsService`.
pub fn build_news_service(
    local_node: NodeId,
    transport: Arc<dyn GossipTransport>,
    store_dir: std::path::PathBuf,
) -> BridgeResult<Arc<NewsService>> {
    let cfg = NewsServiceConfig {
        store_dir,
        event_channel_capacity: 512,
        policy: Default::default(),
    };
    let svc = NewsService::open(local_node, transport, cfg)
        .map_err(|e| BridgeError::NewsService(e.to_string()))?;
    Ok(Arc::new(svc))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedTool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl Clone for AdnetFeedAdapter {
    fn clone(&self) -> Self {
        Self {
            identity: self.identity.clone(),
            config: self.config.clone(),
            news_service: self.news_service.clone(),
            gossip_transport: self.gossip_transport.clone(),
            blob_store: self.blob_store.clone(),
            event_sender: self.event_sender.clone(),
            subscriptions: self.subscriptions.clone(),
            topic_index: self.topic_index.clone(),
            item_cache: self.item_cache.clone(),
            tips: self.tips.clone(),
            connected: self.connected.clone(),
            listener_tasks: self.listener_tasks.clone(),
            cancel: self.cancel.clone(),
        }
    }
}

/// Strip control characters and enforce a max tag length.
fn sanitize_tags(input: &[String]) -> Vec<String> {
    input
        .iter()
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty() && t.len() <= 64)
        .filter(|t| t.chars().all(|c| !c.is_control()))
        .take(16)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_gossip::InProcessGossip;
    use crate::error::BridgeError;

    #[test]
    fn test_sanitize_tags() {
        let tags = vec![
            "defi".to_string(),
            "DeFi".to_string(),
            "".to_string(),
            "  ".to_string(),
            "an".repeat(40),
        ];
        let out = sanitize_tags(&tags);
        assert!(out.iter().any(|t| t == "defi"));
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn test_category_topic_is_deterministic() {
        let a = AdnetFeedAdapter::category_topic(BulletinCategory::Tech);
        let b = AdnetFeedAdapter::category_topic(BulletinCategory::Tech);
        assert_eq!(a, b);
    }

    #[tokio::test]
    async fn test_feed_adapter_creation() {
        let dir = tempfile::tempdir().unwrap();
        let identity = crate::identity::AdnetIdentity::new(
            dir.path().to_path_buf(),
            "agent-test",
        ).await.unwrap();
        let adapter = AdnetFeedAdapter::new(identity, FeedConfig::default()).await.unwrap();
        assert!(!adapter.is_connected().await);
    }

    #[tokio::test]
    async fn test_subscribe_emits_event() {
        let dir = tempfile::tempdir().unwrap();
        let identity = crate::identity::AdnetIdentity::new(
            dir.path().to_path_buf(),
            "agent-test",
        ).await.unwrap();
        let adapter = AdnetFeedAdapter::new(identity, FeedConfig::default()).await.unwrap();
        // Subscribe BEFORE connecting so we don't miss the auto-subscribe events.
        let mut rx = adapter.subscribe_events().await;
        adapter.connect().await.unwrap();
        let event = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("subscribe event delivery")
            .expect("subscribe event is a result");
        match event {
            FeedEvent::Subscribed { topic } => {
                assert!(topic.starts_with("news-"), "got topic {topic}");
            }
            _ => panic!("expected Subscribed event, got a different variant"),
        }
    }

    #[tokio::test]
    async fn test_validate_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let identity = crate::identity::AdnetIdentity::new(
            dir.path().to_path_buf(),
            "agent-test",
        ).await.unwrap();
        let adapter = AdnetFeedAdapter::new(identity, FeedConfig::default()).await.unwrap();
        assert!(adapter.validate_title("").is_err());
        assert!(adapter.validate_title("Good title").is_ok());
        assert!(adapter.validate_body("").is_err());
        assert!(adapter.validate_body("body").is_ok());
    }

    #[tokio::test]
    async fn test_eliza_tools_generated() {
        let dir = tempfile::tempdir().unwrap();
        let identity = crate::identity::AdnetIdentity::new(
            dir.path().to_path_buf(),
            "agent-test",
        ).await.unwrap();
        let adapter = AdnetFeedAdapter::new(identity, FeedConfig::default()).await.unwrap();
        let tools = adapter.generate_eliza_tools();
        assert!(tools.len() >= 8);
        assert!(tools.iter().any(|t| t.name == "publish_report"));
    }

    // ----------------------------------------------------------------
    // Helpers (test fixtures)
    // ----------------------------------------------------------------

    async fn mk_identity(name: &str) -> (tempfile::TempDir, crate::identity::AdnetIdentity) {
        let dir = tempfile::tempdir().unwrap();
        let id = crate::identity::AdnetIdentity::new(
            dir.path().to_path_buf(),
            name,
        ).await.unwrap();
        (dir, id)
    }

    async fn mk_adapter(name: &str) -> (tempfile::TempDir, AdnetFeedAdapter) {
        let (dir, id) = mk_identity(name).await;
        let adapter = AdnetFeedAdapter::new(id, FeedConfig::default()).await.unwrap();
        (dir, adapter)
    }

    async fn mk_adapter_with_gossip(
        name: &str,
    ) -> (tempfile::TempDir, AdnetFeedAdapter, Arc<InProcessGossip>) {
        let (dir, id) = mk_identity(name).await;
        let gossip = Arc::new(InProcessGossip::new());
        let adapter = FeedAdapterBuilder::new(id)
            .with_gossip_transport(gossip.clone())
            .build()
            .await
            .unwrap();
        (dir, adapter, gossip)
    }

    // ----------------------------------------------------------------
    // FeedConfig / From impls / topic naming
    // ----------------------------------------------------------------

    #[test]
    fn feed_config_default_values() {
        let cfg = FeedConfig::default();
        assert_eq!(cfg.default_categories, vec![BulletinCategory::Tech, BulletinCategory::General]);
        assert_eq!(cfg.display_name, "A3Net AI");
        // The default room id is the topic-room (without the `news-`
        // prefix); callers prefix it when building the gossip topic.
        assert_eq!(cfg.room_id.as_str(), "general");
        assert_eq!(cfg.min_post_interval_secs, 60);
        assert_eq!(cfg.max_cache_size, 1000);
        assert!(!cfg.auto_subscribe_trending);
        assert_eq!(cfg.event_capacity, 512);
        assert!(cfg.show_ai_badge);
        assert_eq!(cfg.agent_id_label, "eliza-agent");
        assert!(cfg.validate_publish);
    }

    #[test]
    fn feed_item_from_bulletin_item_preserves_fields() {
        let dir = tempfile::tempdir().unwrap();
        let nonce: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
        let item = a3net_news::BulletinItem::new(
            a3net_news::BulletinKind::NewsArticle,
            a3net_news::BulletinCategory::Tech,
            a3net_news::BulletinSeverity::Notable,
            a3net_types::room::RoomId::new("lobby"),
            a3net_types::NodeId::random(),
            "Hello".to_string(),
            "Summary".to_string(),
            "Body".to_string(),
            &nonce,
            None,
        )
        .unwrap()
        .with_author_name("Author")
        .with_tags(vec!["t".to_string()]);
        let feed: FeedItem = item.clone().into();
        assert_eq!(feed.id, item.bulletin_id.to_string());
        assert_eq!(feed.author_id, item.author_id);
        assert_eq!(feed.title, "Hello");
        assert_eq!(feed.body, "Body");
        assert_eq!(feed.summary, "Summary");
        assert!(matches!(feed.category, BulletinCategory::Tech));
        assert!(matches!(feed.severity, BulletinSeverity::Notable));
        assert_eq!(feed.tags, vec!["t".to_string()]);
        let _ = dir;
    }

    #[test]
    fn room_topic_is_stable() {
        let room = a3net_types::room::RoomId::new("lobby");
        let t1 = AdnetFeedAdapter::room_topic(&room);
        let t2 = AdnetFeedAdapter::room_topic(&room);
        assert_eq!(t1, t2);
        assert_eq!(t1.as_hex().len(), 64);
        let other = AdnetFeedAdapter::room_topic(&a3net_types::room::RoomId::new("other"));
        assert_ne!(t1, other);
    }

    #[test]
    fn category_topic_differs_per_category() {
        let tech = AdnetFeedAdapter::category_topic(BulletinCategory::Tech);
        let general = AdnetFeedAdapter::category_topic(BulletinCategory::General);
        assert_ne!(tech, general);
    }

    // ----------------------------------------------------------------
    // Adapter state accessors / lifecycle
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn feed_adapter_node_id_matches_identity() {
        let (_d, id) = mk_identity("fa-nid").await;
        let node = id.node_id();
        let adapter = AdnetFeedAdapter::new(id, FeedConfig::default()).await.unwrap();
        assert_eq!(adapter.node_id(), node);
    }

    #[tokio::test]
    async fn feed_adapter_is_connected_round_trip() {
        let (_d, adapter, _g) = mk_adapter_with_gossip("fa-conn").await;
        assert!(!adapter.is_connected().await);
        adapter.connect().await.unwrap();
        assert!(adapter.is_connected().await);
        // Idempotent.
        adapter.connect().await.unwrap();
        assert!(adapter.is_connected().await);
        adapter.disconnect().await.unwrap();
        assert!(!adapter.is_connected().await);
        // Idempotent again.
        adapter.disconnect().await.unwrap();
        assert!(!adapter.is_connected().await);
    }

    // ----------------------------------------------------------------
    // Builder
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn feed_adapter_builder_defaults() {
        let (_d, id) = mk_identity("b-default").await;
        let adapter = FeedAdapterBuilder::new(id).build().await.unwrap();
        assert!(!adapter.is_connected().await);
        assert_eq!(adapter.node_id().as_hex().len(), 64);
    }

    #[tokio::test]
    async fn feed_adapter_builder_display_name() {
        let (_d, id) = mk_identity("b-name").await;
        let adapter = FeedAdapterBuilder::new(id)
            .display_name("MyReporter")
            .build()
            .await
            .unwrap();
        assert!(!adapter.is_connected().await);
    }

    #[tokio::test]
    async fn feed_adapter_builder_categories() {
        let (_d, id) = mk_identity("b-cat").await;
        let adapter = FeedAdapterBuilder::new(id)
            .categories(vec![BulletinCategory::Economy])
            .build()
            .await
            .unwrap();
        assert!(!adapter.is_connected().await);
    }

    #[tokio::test]
    async fn feed_adapter_builder_min_post_interval() {
        let (_d, id) = mk_identity("b-int").await;
        let adapter = FeedAdapterBuilder::new(id)
            .min_post_interval(5)
            .build()
            .await
            .unwrap();
        assert!(!adapter.is_connected().await);
    }

    #[tokio::test]
    async fn feed_adapter_builder_room_id() {
        let (_d, id) = mk_identity("b-room").await;
        let adapter = FeedAdapterBuilder::new(id)
            .room_id(a3net_types::room::RoomId::new("custom-room"))
            .build()
            .await
            .unwrap();
        assert!(!adapter.is_connected().await);
    }

    #[tokio::test]
    async fn feed_adapter_builder_with_news_service() {
        let (_d, id) = mk_identity("b-news").await;
        let gossip = Arc::new(InProcessGossip::new());
        let svc_dir = tempfile::tempdir().unwrap();
        let svc = build_news_service(id.node_id(), gossip.clone(), svc_dir.path().to_path_buf()).unwrap();
        let adapter = FeedAdapterBuilder::new(id)
            .with_news_service(svc)
            .build()
            .await
            .unwrap();
        assert!(!adapter.is_connected().await);
    }

    #[tokio::test]
    async fn feed_adapter_builder_with_blob_store() {
        let (_d, id) = mk_identity("b-blob").await;
        let blob_dir = tempfile::tempdir().unwrap();
        let store = a3net_blobstore::BlobStore::new(blob_dir.path()).unwrap();
        let adapter = FeedAdapterBuilder::new(id)
            .with_blob_store(store)
            .build()
            .await
            .unwrap();
        assert!(!adapter.is_connected().await);
    }

    #[tokio::test]
    async fn feed_adapter_builder_all_options() {
        let (_d, id) = mk_identity("b-all").await;
        let gossip = Arc::new(InProcessGossip::new());
        let svc_dir = tempfile::tempdir().unwrap();
        let svc = build_news_service(id.node_id(), gossip.clone(), svc_dir.path().to_path_buf()).unwrap();
        let blob_dir = tempfile::tempdir().unwrap();
        let store = a3net_blobstore::BlobStore::new(blob_dir.path()).unwrap();
        let adapter = FeedAdapterBuilder::new(id)
            .display_name("X")
            .categories(vec![BulletinCategory::Tech])
            .min_post_interval(1)
            .room_id(a3net_types::room::RoomId::new("r"))
            .with_news_service(svc)
            .with_gossip_transport(gossip)
            .with_blob_store(store)
            .build()
            .await
            .unwrap();
        assert!(!adapter.is_connected().await);
    }

    // ----------------------------------------------------------------
    // Subscriptions
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn subscribe_to_category_uses_news_prefix() {
        let (_d, adapter, _g) = mk_adapter_with_gossip("sub-cat").await;
        let mut rx = adapter.subscribe_events().await;
        adapter
            .subscribe_to_category(BulletinCategory::Tech)
            .await
            .unwrap();
        let ev = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match ev {
            FeedEvent::Subscribed { topic } => assert_eq!(topic, "news-tech"),
            other => panic!("expected Subscribed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn subscribe_is_idempotent() {
        let (_d, adapter, _g) = mk_adapter_with_gossip("sub-idem").await;
        adapter.subscribe("news-test").await.unwrap();
        let subs = adapter.get_subscriptions().await;
        assert_eq!(subs.len(), 1);
        // Second call should be a no-op (no extra event).
        let mut rx = adapter.subscribe_events().await;
        adapter.subscribe("news-test").await.unwrap();
        let res = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await;
        assert!(res.is_err(), "second subscribe should not emit another event");
    }

    #[tokio::test]
    async fn unsubscribe_emits_event_and_clears() {
        let (_d, adapter, _g) = mk_adapter_with_gossip("unsub").await;
        adapter.subscribe("news-x").await.unwrap();
        let mut rx = adapter.subscribe_events().await;
        adapter.unsubscribe("news-x").await.unwrap();
        let ev = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match ev {
            FeedEvent::Unsubscribed { topic } => assert_eq!(topic, "news-x"),
            other => panic!("expected Unsubscribed, got {other:?}"),
        }
        assert!(adapter.get_subscriptions().await.is_empty());
    }

    #[tokio::test]
    async fn unsubscribe_unknown_topic_is_noop() {
        let (_d, adapter, _g) = mk_adapter_with_gossip("unsub-noop").await;
        adapter.unsubscribe("never-subscribed").await.unwrap();
    }

    #[tokio::test]
    async fn get_subscriptions_lists_all() {
        let (_d, adapter, _g) = mk_adapter_with_gossip("get-subs").await;
        adapter.subscribe("news-a").await.unwrap();
        adapter.subscribe("news-b").await.unwrap();
        let mut subs = adapter.get_subscriptions().await;
        subs.sort();
        assert_eq!(subs, vec!["news-a".to_string(), "news-b".to_string()]);
    }

    // ----------------------------------------------------------------
    // Validation helpers
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn validate_title_length_limits() {
        let (_d, adapter) = mk_adapter("vt").await;
        assert!(adapter.validate_title("ok").is_ok());
        assert!(adapter.validate_title("").is_err());
        assert!(adapter.validate_title(&"x".repeat(257)).is_err());
    }

    #[tokio::test]
    async fn validate_body_length_limits() {
        let (_d, adapter) = mk_adapter("vb").await;
        assert!(adapter.validate_body("ok").is_ok());
        assert!(adapter.validate_body("").is_err());
        assert!(adapter.validate_body(&"x".repeat(256 * 1024 + 1)).is_err());
    }

    // ----------------------------------------------------------------
    // Rate limiting on publish
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn publish_report_rate_limited_by_default() {
        let (_d, adapter, _g) = mk_adapter_with_gossip("pr-rl").await;
        adapter.connect().await.unwrap();
        // First publish should succeed; the second within
        // `min_post_interval_secs=60` should be rate-limited.
        let id1 = adapter
            .publish_report("Title", "Body", BulletinCategory::Tech, vec![])
            .await
            .unwrap();
        assert!(!id1.is_empty());
        let err = adapter
            .publish_report("Title2", "Body2", BulletinCategory::Tech, vec![])
            .await
            .unwrap_err();
        assert!(matches!(err, crate::error::BridgeError::RateLimited(_)));
    }

    #[tokio::test]
    async fn publish_report_rejects_empty_inputs() {
        let (_d, adapter, _g) = mk_adapter_with_gossip("pr-v").await;
        adapter.connect().await.unwrap();
        assert!(adapter
            .publish_report("", "Body", BulletinCategory::Tech, vec![])
            .await
            .is_err());
        assert!(adapter
            .publish_report("Title", "", BulletinCategory::Tech, vec![])
            .await
            .is_err());
    }

    // ----------------------------------------------------------------
    // Publish paths
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn publish_report_emits_event_and_returns_id() {
        let (_d, adapter, _g) = mk_adapter_with_gossip("pr-evt").await;
        adapter.connect().await.unwrap();
        let mut rx = adapter.subscribe_events().await;
        let id = adapter
            .publish_report(
                "Hi",
                "world",
                BulletinCategory::General,
                vec!["alpha".to_string(), "  ".to_string()],
            )
            .await
            .unwrap();
        assert!(!id.is_empty());
        let ev = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match ev {
            FeedEvent::Published { item_id, category } => {
                assert_eq!(item_id, id);
                assert!(matches!(category, BulletinCategory::General));
            }
            other => panic!("expected Published, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn publish_alert_succeeds() {
        let (_d, adapter, _g) = mk_adapter_with_gossip("pa").await;
        adapter.connect().await.unwrap();
        let id = adapter
            .publish_alert(
                "Breaking",
                "body",
                BulletinSeverity::Critical,
                vec!["breaking".to_string()],
            )
            .await
            .unwrap();
        assert!(!id.is_empty());
    }

    #[tokio::test]
    async fn publish_correction_succeeds() {
        let (_d, adapter, _g) = mk_adapter_with_gossip("pc").await;
        adapter.connect().await.unwrap();
        let target = a3net_news::BulletinId::from_hex(
            "0000000000000000000000000000000000000000000000000000000000000000",
        ).unwrap();
        let id = adapter
            .publish_correction(&target, "Fix", "Corrected body", vec![])
            .await
            .unwrap();
        assert!(!id.is_empty());
    }

    #[tokio::test]
    async fn publish_retraction_succeeds() {
        let (_d, adapter, _g) = mk_adapter_with_gossip("pr").await;
        adapter.connect().await.unwrap();
        let target = a3net_news::BulletinId::from_hex(
            "0000000000000000000000000000000000000000000000000000000000000001",
        ).unwrap();
        let id = adapter
            .publish_retraction(&target, "reason")
            .await
            .unwrap();
        assert!(!id.is_empty());
    }

    #[tokio::test]
    async fn publish_with_blobs_without_store_returns_attachment_ids() {
        let (_d, adapter, _g) = mk_adapter_with_gossip("pwb").await;
        adapter.connect().await.unwrap();
        let blobs = vec![
            ("file1.txt".to_string(), Bytes::from_static(b"hello"), "text/plain".to_string()),
        ];
        let (id, att_ids) = adapter
            .publish_with_blobs(
                "Title",
                "Body",
                BulletinCategory::Tech,
                vec![],
                blobs,
            )
            .await
            .unwrap();
        assert!(!id.is_empty());
        assert_eq!(att_ids.len(), 1);
    }

    #[tokio::test]
    async fn publish_with_blobs_rejects_empty_file_name() {
        let (_d, adapter, _g) = mk_adapter_with_gossip("pwb-bad").await;
        adapter.connect().await.unwrap();
        let blobs = vec![
            ("".to_string(), Bytes::from_static(b"data"), "text/plain".to_string()),
        ];
        let err = adapter
            .publish_with_blobs("T", "B", BulletinCategory::Tech, vec![], blobs)
            .await
            .unwrap_err();
        assert!(matches!(err, crate::error::BridgeError::InvalidMessage(_)));
    }

    // ----------------------------------------------------------------
    // Reading helpers
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn get_by_category_routes_to_feed() {
        let (_d, adapter, _g) = mk_adapter_with_gossip("gbc").await;
        adapter.connect().await.unwrap();
        // No items yet, but should not error.
        let out = adapter.get_by_category(BulletinCategory::Tech, 10).await.unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn get_by_id_returns_none_when_unknown() {
        let (_d, adapter, _g) = mk_adapter_with_gossip("gbi").await;
        adapter.connect().await.unwrap();
        let bid = a3net_news::BulletinId::from_hex(
            "0000000000000000000000000000000000000000000000000000000000000000",
        ).unwrap();
        let res = adapter.get_by_id(&bid).await.unwrap();
        assert!(res.is_none());
    }

    #[tokio::test]
    async fn search_finds_matching_items_in_cache() {
        let (_d, adapter, _g) = mk_adapter_with_gossip("search").await;
        adapter.connect().await.unwrap();
        let out = adapter.search("nothing-matches", 10).await.unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn mark_read_is_noop_without_service() {
        let (_d, adapter, _g) = mk_adapter_with_gossip("mr").await;
        adapter.connect().await.unwrap();
        let bid = a3net_news::BulletinId::from_hex(
            "0000000000000000000000000000000000000000000000000000000000000000",
        ).unwrap();
        adapter.mark_read(&bid).await.unwrap();
    }

    // ----------------------------------------------------------------
    // Tips
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn record_tip_appends_and_emits() {
        let (_d, adapter) = mk_adapter("tip").await;
        let mut rx = adapter.subscribe_events().await;
        adapter.record_tip("item-1".to_string(), NodeId::random(), "10".to_string());
        let tips = adapter.tips().await;
        assert_eq!(tips.len(), 1);
        assert_eq!(tips[0].item_id, "item-1");
        assert_eq!(tips[0].amount, "10");
        let ev = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match ev {
            FeedEvent::Tipped { item_id, amount, .. } => {
                assert_eq!(item_id, "item-1");
                assert_eq!(amount, "10");
            }
            other => panic!("expected Tipped, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tips_initially_empty() {
        let (_d, adapter) = mk_adapter("tip-empty").await;
        assert!(adapter.tips().await.is_empty());
    }

    // ----------------------------------------------------------------
    // Listener / event subscription
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn subscribe_events_returns_receiver() {
        let (_d, adapter) = mk_adapter("sub-evt").await;
        let mut rx = adapter.subscribe_events().await;
        assert_eq!(rx.len(), 0);
        // Don't recv — no events emitted yet.
    }

    #[tokio::test]
    async fn stop_listener_is_safe_when_no_tasks() {
        let (_d, adapter) = mk_adapter("stop-listener").await;
        adapter.stop_listener().await;
    }

    #[tokio::test]
    async fn start_listener_fails_without_news_service() {
        let (_d, adapter, _g) = mk_adapter_with_gossip("start-listener-noop").await;
        adapter.connect().await.unwrap();
        let err = adapter.start_listener().await.unwrap_err();
        assert!(matches!(err, crate::error::BridgeError::NotConnected));
    }

    #[tokio::test]
    async fn start_listener_succeeds_with_news_service() {
        let (_d, id) = mk_identity("start-listener").await;
        let gossip = Arc::new(InProcessGossip::new());
        let svc_dir = tempfile::tempdir().unwrap();
        let svc = build_news_service(id.node_id(), gossip.clone(), svc_dir.path().to_path_buf()).unwrap();
        let adapter = FeedAdapterBuilder::new(id)
            .with_news_service(svc)
            .with_gossip_transport(gossip)
            .build()
            .await
            .unwrap();
        adapter.connect().await.unwrap();
        adapter.start_listener().await.unwrap();
        adapter.stop_listener().await;
    }

    // ----------------------------------------------------------------
    // set_event_handler
    // ----------------------------------------------------------------

    struct CapturingFeedHandler {
        received: std::sync::Arc<tokio::sync::Mutex<Vec<FeedEvent>>>,
    }

    #[async_trait::async_trait]
    impl FeedEventHandler for CapturingFeedHandler {
        async fn on_feed_event(&self, event: FeedEvent) {
            self.received.lock().await.push(event);
        }
    }

    #[tokio::test]
    async fn set_event_handler_dispatches() {
        let (_d, adapter, _g) = mk_adapter_with_gossip("set-evt").await;
        let received = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let handler = std::sync::Arc::new(CapturingFeedHandler {
            received: received.clone(),
        });
        let adapter = adapter.set_event_handler(handler).await.unwrap();
        adapter.connect().await.unwrap();
        // The auto-subscribe should produce Subscribed events.
        // Wait briefly for them to propagate.
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if !received.lock().await.is_empty() {
                break;
            }
        }
        assert!(!received.lock().await.is_empty(), "no event dispatched");
    }

    // ----------------------------------------------------------------
    // wait_for_event
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn wait_for_event_times_out() {
        let (_d, adapter) = mk_adapter("wfe-timeout").await;
        let err = adapter
            .wait_for_event(Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(matches!(err, crate::error::BridgeError::Timeout(secs) if secs == 0));
    }

    #[tokio::test]
    async fn wait_for_event_receives_event() {
        let (_d, adapter) = mk_adapter("wfe-ok").await;
        let cloned = adapter.clone();
        let mut rx = adapter.subscribe_events().await;
        tokio::spawn(async move {
            cloned.connect().await.unwrap();
        });
        let ev = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("event delivered")
            .expect("event ok");
        match ev {
            FeedEvent::Subscribed { .. } => {}
            other => panic!("expected Subscribed, got {other:?}"),
        }
    }

    // ----------------------------------------------------------------
    // Clone
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn feed_adapter_clone_preserves_node_id() {
        let (_d, adapter) = mk_adapter("clone").await;
        let cloned = adapter.clone();
        assert_eq!(adapter.node_id(), cloned.node_id());
    }

    // ----------------------------------------------------------------
    // NewsService integration (covers publish + get_feed + mark_read)
    // ----------------------------------------------------------------

    fn make_in_memory_news_service(
        local_node: NodeId,
        transport: Arc<dyn a3net_gossip::GossipTransport>,
    ) -> Arc<a3net_news::NewsService> {
        Arc::new(
            a3net_news::NewsService::open_in_memory(local_node, transport, Default::default())
                .unwrap(),
        )
    }

    #[tokio::test]
    async fn publish_report_via_news_service() {
        use a3net_types::bulletin::BulletinCategory;
        let (_d, id) = mk_identity("svc-report").await;
        let transport = Arc::new(InProcessGossip::new());
        let svc = make_in_memory_news_service(id.node_id(), transport.clone());
        let adapter = AdnetFeedAdapter::new(id, FeedConfig::default())
            .await
            .unwrap()
            .with_news_service(svc)
            .await
            .with_gossip_transport(transport)
            .await;
        adapter.connect().await.unwrap();
        let item_id = adapter
            .publish_report(
                "News Service",
                "Body via news service",
                BulletinCategory::Tech,
                vec!["tag1".into()],
            )
            .await
            .unwrap();
        assert!(!item_id.is_empty());
        // Read it back via get_feed.
        let items = adapter.get_feed("news-tech", 10).await.unwrap();
        assert!(items.iter().any(|i| i.id == item_id));
        adapter.disconnect().await.unwrap();
    }

    #[tokio::test]
    async fn publish_report_truncates_long_summary() {
        use a3net_types::bulletin::BulletinCategory;
        let (_d, id) = mk_identity("svc-summary").await;
        let transport = Arc::new(InProcessGossip::new());
        let svc = make_in_memory_news_service(id.node_id(), transport.clone());
        let adapter = AdnetFeedAdapter::new(id, FeedConfig::default())
            .await
            .unwrap()
            .with_news_service(svc)
            .await
            .with_gossip_transport(transport)
            .await;
        adapter.connect().await.unwrap();
        // Body longer than 240 chars triggers the truncation path.
        let long_body = "a".repeat(400);
        let _ = adapter
            .publish_report("Long", &long_body, BulletinCategory::General, vec![])
            .await
            .unwrap();
        let items = adapter.get_feed("news-general", 10).await.unwrap();
        assert!(!items.is_empty());
        // Summary should be truncated with the ellipsis.
        assert!(items[0].summary.contains('…'));
        adapter.disconnect().await.unwrap();
    }

    #[tokio::test]
    async fn publish_alert_via_news_service() {
        use a3net_types::bulletin::BulletinSeverity;
        let (_d, id) = mk_identity("svc-alert").await;
        let transport = Arc::new(InProcessGossip::new());
        let svc = make_in_memory_news_service(id.node_id(), transport.clone());
        let adapter = AdnetFeedAdapter::new(id, FeedConfig::default())
            .await
            .unwrap()
            .with_news_service(svc)
            .await
            .with_gossip_transport(transport)
            .await;
        adapter.connect().await.unwrap();
        let id = adapter
            .publish_alert("Alert!", "Body", BulletinSeverity::Critical, vec![])
            .await
            .unwrap();
        assert!(!id.is_empty());
        let items = adapter.get_feed("news-general", 10).await.unwrap();
        assert!(items.iter().any(|i| i.id == id));
        adapter.disconnect().await.unwrap();
    }

    #[tokio::test]
    async fn publish_correction_via_news_service() {
        use a3net_types::bulletin::{BulletinCategory, BulletinId};
        let (_d, id) = mk_identity("svc-corr").await;
        let transport = Arc::new(InProcessGossip::new());
        let svc = make_in_memory_news_service(id.node_id(), transport.clone());
        let adapter = AdnetFeedAdapter::new(id, FeedConfig::default())
            .await
            .unwrap()
            .with_news_service(svc)
            .await
            .with_gossip_transport(transport)
            .await;
        adapter.connect().await.unwrap();
        let target = BulletinId::from_hex(&format!("{:064x}", 0u64)).unwrap();
        let new_id = adapter
            .publish_correction(&target, "Correction Title", "Corrected body", vec!["fix".into()])
            .await
            .unwrap();
        assert!(!new_id.is_empty());
        adapter.disconnect().await.unwrap();
        // Suppress unused warning.
        let _ = BulletinCategory::General;
    }

    #[tokio::test]
    async fn publish_retraction_via_news_service() {
        use a3net_types::bulletin::BulletinId;
        let (_d, id) = mk_identity("svc-retr").await;
        let transport = Arc::new(InProcessGossip::new());
        let svc = make_in_memory_news_service(id.node_id(), transport.clone());
        let adapter = AdnetFeedAdapter::new(id, FeedConfig::default())
            .await
            .unwrap()
            .with_news_service(svc)
            .await
            .with_gossip_transport(transport)
            .await;
        adapter.connect().await.unwrap();
        let target = BulletinId::from_hex(&format!("{:064x}", 0u64)).unwrap();
        let id = adapter
            .publish_retraction(&target, "Wrong info")
            .await
            .unwrap();
        assert!(!id.is_empty());
        adapter.disconnect().await.unwrap();
    }

    #[tokio::test]
    async fn publish_with_blobs_uses_blob_store() {
        use a3net_types::bulletin::BulletinCategory;
        let (_d, id) = mk_identity("svc-blob").await;
        let transport = Arc::new(InProcessGossip::new());
        let svc = make_in_memory_news_service(id.node_id(), transport.clone());
        // Build a small in-memory blob store via the public API.
        let blob_dir = tempfile::tempdir().unwrap();
        let blob_store = a3net_blobstore::BlobStore::new(blob_dir.path()).unwrap();
        let adapter = AdnetFeedAdapter::new(id, FeedConfig::default())
            .await
            .unwrap()
            .with_news_service(svc)
            .await
            .with_gossip_transport(transport)
            .await
            .with_blob_store(blob_store)
            .await;
        adapter.connect().await.unwrap();
        let (item_id, attachment_ids) = adapter
            .publish_with_blobs(
                "With blobs",
                "Body",
                BulletinCategory::Tech,
                vec![],
                vec![(
                    "hello.txt".to_string(),
                    bytes::Bytes::from_static(b"hello world"),
                    "text/plain".to_string(),
                )],
            )
            .await
            .unwrap();
        assert!(!item_id.is_empty());
        assert_eq!(attachment_ids.len(), 1);
        adapter.disconnect().await.unwrap();
    }

    #[tokio::test]
    async fn publish_with_blobs_via_news_service_rejects_empty_file_name() {
        use a3net_types::bulletin::BulletinCategory;
        let (_d, id) = mk_identity("svc-blob-bad").await;
        let transport = Arc::new(InProcessGossip::new());
        let svc = make_in_memory_news_service(id.node_id(), transport.clone());
        let adapter = AdnetFeedAdapter::new(id, FeedConfig::default())
            .await
            .unwrap()
            .with_news_service(svc)
            .await
            .with_gossip_transport(transport)
            .await;
        adapter.connect().await.unwrap();
        let err = adapter
            .publish_with_blobs(
                "T",
                "B",
                BulletinCategory::Tech,
                vec![],
                vec![(
                    "".to_string(),
                    bytes::Bytes::from_static(b"x"),
                    "text/plain".to_string(),
                )],
            )
            .await
            .unwrap_err();
        match err {
            BridgeError::InvalidMessage(msg) => assert!(msg.contains("file_name")),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn publish_with_blobs_without_store_inlines() {
        use a3net_types::bulletin::BulletinCategory;
        let (_d, id) = mk_identity("svc-blob-inline").await;
        let transport = Arc::new(InProcessGossip::new());
        let svc = make_in_memory_news_service(id.node_id(), transport.clone());
        let adapter = AdnetFeedAdapter::new(id, FeedConfig::default())
            .await
            .unwrap()
            .with_news_service(svc)
            .await
            .with_gossip_transport(transport)
            .await;
        adapter.connect().await.unwrap();
        let (_id, att) = adapter
            .publish_with_blobs(
                "NoBlob",
                "B",
                BulletinCategory::General,
                vec![],
                vec![(
                    "a.txt".to_string(),
                    bytes::Bytes::from_static(b"data"),
                    "text/plain".to_string(),
                )],
            )
            .await
            .unwrap();
        assert_eq!(att.len(), 1);
    }

    #[tokio::test]
    async fn get_by_category_returns_matching_items() {
        use a3net_types::bulletin::BulletinCategory;
        let (_d, id) = mk_identity("svc-cat").await;
        let transport = Arc::new(InProcessGossip::new());
        let svc = make_in_memory_news_service(id.node_id(), transport.clone());
        let cfg = FeedConfig {
            min_post_interval_secs: 0,
            ..Default::default()
        };
        let adapter = AdnetFeedAdapter::new(id, cfg)
            .await
            .unwrap()
            .with_news_service(svc)
            .await
            .with_gossip_transport(transport)
            .await;
        adapter.connect().await.unwrap();
        adapter
            .publish_report("Tech1", "b1", BulletinCategory::Tech, vec![])
            .await
            .unwrap();
        adapter
            .publish_report("Tech2", "b2", BulletinCategory::Tech, vec![])
            .await
            .unwrap();
        let tech = adapter
            .get_by_category(BulletinCategory::Tech, 10)
            .await
            .unwrap();
        assert!(tech.len() >= 2);
    }

    #[tokio::test]
    async fn get_by_id_via_news_service() {
        use a3net_types::bulletin::{BulletinCategory, BulletinId};
        let (_d, id) = mk_identity("svc-getid").await;
        let transport = Arc::new(InProcessGossip::new());
        let svc = make_in_memory_news_service(id.node_id(), transport.clone());
        let adapter = AdnetFeedAdapter::new(id, FeedConfig::default())
            .await
            .unwrap()
            .with_news_service(svc)
            .await
            .with_gossip_transport(transport)
            .await;
        adapter.connect().await.unwrap();
        let mid = adapter
            .publish_report("T", "b", BulletinCategory::Tech, vec![])
            .await
            .unwrap();
        // Construct BulletinId from the hex string we got back.
        let bid = BulletinId::from_hex(&mid).unwrap();
        let item = adapter.get_by_id(&bid).await.unwrap();
        assert!(item.is_some());
    }

    #[tokio::test]
    async fn mark_read_via_news_service() {
        use a3net_types::bulletin::{BulletinCategory, BulletinId};
        let (_d, id) = mk_identity("svc-mr").await;
        let transport = Arc::new(InProcessGossip::new());
        let svc = make_in_memory_news_service(id.node_id(), transport.clone());
        let adapter = AdnetFeedAdapter::new(id, FeedConfig::default())
            .await
            .unwrap()
            .with_news_service(svc)
            .await
            .with_gossip_transport(transport)
            .await;
        adapter.connect().await.unwrap();
        let mid = adapter
            .publish_report("T", "b", BulletinCategory::Tech, vec![])
            .await
            .unwrap();
        let bid = BulletinId::from_hex(&mid).unwrap();
        adapter.mark_read(&bid).await.unwrap();
    }

    #[tokio::test]
    async fn get_feed_in_memory_cache_fallback() {
        let (_d, adapter) = mk_adapter("cache-fb").await;
        // No NewsService → use in-memory cache. With no items, the
        // cache lookup should still succeed and return an empty list.
        let items = adapter.get_feed("news-all", 10).await.unwrap();
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn search_in_memory_cache_finds_by_tag() {
        let (_d, id) = mk_identity("cache-search").await;
        let transport = Arc::new(InProcessGossip::new());
        let adapter = AdnetFeedAdapter::new(id, FeedConfig::default())
            .await
            .unwrap()
            .with_gossip_transport(transport)
            .await;
        adapter.connect().await.unwrap();
        // Manually insert items into the cache for search to find.
        let mut cache = adapter.item_cache.write().await;
        cache.insert(
            "a".to_string(),
            FeedItem {
                id: "a".to_string(),
                author_id: NodeId::random(),
                author_name: "A".into(),
                title: "DeFi explained".into(),
                body: "body".into(),
                summary: "summary".into(),
                category: BulletinCategory::General,
                severity: BulletinSeverity::Info,
                tags: vec!["defi".into()],
                published_at: 0,
                attachments: vec![],
            },
        );
        cache.insert(
            "b".to_string(),
            FeedItem {
                id: "b".to_string(),
                author_id: NodeId::random(),
                author_name: "B".into(),
                title: "Other".into(),
                body: "no match at all here".into(),
                summary: "s".into(),
                category: BulletinCategory::Tech,
                severity: BulletinSeverity::Info,
                tags: vec!["other".into()],
                published_at: 0,
                attachments: vec![],
            },
        );
        drop(cache);
        let hits = adapter.search("DeFi", 10).await.unwrap();
        assert!(hits.iter().any(|i| i.id == "a"));
        assert!(!hits.iter().any(|i| i.id == "b"));
        // The cache search matches against title/body/summary/tags.
        let hits2 = adapter.search("no defi", 10).await.unwrap();
        assert!(hits2.is_empty(), "should not match anything");
    }

    #[tokio::test]
    async fn get_by_id_in_memory_cache() {
        let (_d, id) = mk_identity("cache-getid").await;
        let adapter = AdnetFeedAdapter::new(id, FeedConfig::default())
            .await
            .unwrap();
        // Insert into cache.
        let mut cache = adapter.item_cache.write().await;
        let full_hex = format!("{:064x}", 1u64);
        cache.insert(
            full_hex.clone(),
            FeedItem {
                id: full_hex.clone(),
                author_id: NodeId::random(),
                author_name: "X".into(),
                title: "T".into(),
                body: "b".into(),
                summary: "s".into(),
                category: BulletinCategory::General,
                severity: BulletinSeverity::Info,
                tags: vec![],
                published_at: 0,
                attachments: vec![],
            },
        );
        drop(cache);
        let bid = a3net_types::bulletin::BulletinId::from_hex(&full_hex).unwrap();
        let item = adapter.get_by_id(&bid).await.unwrap();
        assert!(item.is_some());
        let missing_hex = format!("{:064x}", 2u64);
        let missing = adapter
            .get_by_id(&a3net_types::bulletin::BulletinId::from_hex(&missing_hex).unwrap())
            .await
            .unwrap();
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn record_tip_appends() {
        let (_d, adapter) = mk_adapter("tips").await;
        adapter.record_tip("item-1".into(), NodeId::random(), "10".into());
        adapter.record_tip("item-2".into(), NodeId::random(), "20".into());
        let tips = adapter.tips().await;
        assert_eq!(tips.len(), 2);
    }

    #[tokio::test]
    async fn publish_alert_without_news_service_broadcasts() {
        use a3net_types::bulletin::BulletinSeverity;
        let (_d, id) = mk_identity("alert-bcast").await;
        let transport = Arc::new(InProcessGossip::new());
        let adapter = AdnetFeedAdapter::new(id, FeedConfig::default())
            .await
            .unwrap()
            .with_gossip_transport(transport)
            .await;
        adapter.connect().await.unwrap();
        let mid = adapter
            .publish_alert("Alert", "Body", BulletinSeverity::Info, vec![])
            .await
            .unwrap();
        assert!(!mid.is_empty());
    }

    #[tokio::test]
    async fn publish_correction_without_news_service() {
        use a3net_types::bulletin::BulletinId;
        let (_d, id) = mk_identity("corr-bcast").await;
        let transport = Arc::new(InProcessGossip::new());
        let adapter = AdnetFeedAdapter::new(id, FeedConfig::default())
            .await
            .unwrap()
            .with_gossip_transport(transport)
            .await;
        adapter.connect().await.unwrap();
        let target = BulletinId::from_hex(&format!("{:064x}", 0u64)).unwrap();
        let mid = adapter
            .publish_correction(&target, "T", "B", vec![])
            .await
            .unwrap();
        assert!(!mid.is_empty());
    }

    #[tokio::test]
    async fn publish_retraction_without_news_service() {
        use a3net_types::bulletin::BulletinId;
        let (_d, id) = mk_identity("retr-bcast").await;
        let transport = Arc::new(InProcessGossip::new());
        let adapter = AdnetFeedAdapter::new(id, FeedConfig::default())
            .await
            .unwrap()
            .with_gossip_transport(transport)
            .await;
        adapter.connect().await.unwrap();
        let target = BulletinId::from_hex(&format!("{:064x}", 0u64)).unwrap();
        let mid = adapter
            .publish_retraction(&target, "Wrong")
            .await
            .unwrap();
        assert!(!mid.is_empty());
    }

    #[tokio::test]
    async fn rate_limit_blocks_subsequent_post() {
        use a3net_types::bulletin::BulletinCategory;
        let (_d, id) = mk_identity("rl").await;
        let transport = Arc::new(InProcessGossip::new());
        let cfg = FeedConfig {
            min_post_interval_secs: 60,
            ..Default::default()
        };
        let adapter = AdnetFeedAdapter::new(id, cfg)
            .await
            .unwrap()
            .with_gossip_transport(transport)
            .await;
        adapter.connect().await.unwrap();
        // Explicitly subscribe to the configured room so the rate
        // limiter can find an entry in the topic index.
        adapter
            .subscribe(&adapter.config.room_id.as_str().to_string())
            .await
            .unwrap();
        adapter
            .publish_report("T1", "B1", BulletinCategory::Tech, vec![])
            .await
            .unwrap();
        let err = adapter
            .publish_report("T2", "B2", BulletinCategory::Tech, vec![])
            .await
            .unwrap_err();
        assert!(matches!(err, BridgeError::RateLimited(_)), "got: {err:?}");
    }

    #[tokio::test]
    async fn rate_limit_zero_means_no_throttle() {
        use a3net_types::bulletin::BulletinCategory;
        let (_d, id) = mk_identity("rl-zero").await;
        let transport = Arc::new(InProcessGossip::new());
        let cfg = FeedConfig {
            min_post_interval_secs: 0,
            ..Default::default()
        };
        let adapter = AdnetFeedAdapter::new(id, cfg)
            .await
            .unwrap()
            .with_gossip_transport(transport)
            .await;
        adapter.connect().await.unwrap();
        // Two posts back-to-back should both succeed.
        for n in 0..2 {
            adapter
                .publish_report(
                    &format!("T{n}"),
                    "B",
                    BulletinCategory::General,
                    vec![],
                )
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn publish_operations_fail_when_not_connected() {
        use a3net_types::bulletin::{BulletinCategory, BulletinId, BulletinSeverity};
        let (_d, id) = mk_identity("not-conn").await;
        let transport = Arc::new(InProcessGossip::new());
        let adapter = AdnetFeedAdapter::new(id, FeedConfig::default())
            .await
            .unwrap()
            .with_gossip_transport(transport)
            .await;
        // Note: NOT connected.
        let r1 = adapter
            .publish_report("T", "B", BulletinCategory::Tech, vec![])
            .await;
        assert!(matches!(r1.unwrap_err(), BridgeError::NotConnected));
        let r2 = adapter
            .publish_alert("T", "B", BulletinSeverity::Info, vec![])
            .await;
        assert!(matches!(r2.unwrap_err(), BridgeError::NotConnected));
        let r3 = adapter
            .publish_correction(&BulletinId::from_hex(&format!("{:064x}", 0u64)).unwrap(), "T", "B", vec![])
            .await;
        assert!(matches!(r3.unwrap_err(), BridgeError::NotConnected));
        let r4 = adapter
            .publish_retraction(&BulletinId::from_hex(&format!("{:064x}", 0u64)).unwrap(), "B")
            .await;
        assert!(matches!(r4.unwrap_err(), BridgeError::NotConnected));
    }

    #[tokio::test]
    async fn publish_with_blobs_fails_when_not_connected() {
        use a3net_types::bulletin::BulletinCategory;
        let (_d, id) = mk_identity("not-conn-blob").await;
        let transport = Arc::new(InProcessGossip::new());
        let adapter = AdnetFeedAdapter::new(id, FeedConfig::default())
            .await
            .unwrap()
            .with_gossip_transport(transport)
            .await;
        let r = adapter
            .publish_with_blobs(
                "T",
                "B",
                BulletinCategory::Tech,
                vec![],
                vec![(
                    "f.txt".to_string(),
                    bytes::Bytes::from_static(b"x"),
                    "text/plain".to_string(),
                )],
            )
            .await;
        assert!(matches!(r.unwrap_err(), BridgeError::NotConnected));
    }

    #[tokio::test]
    async fn start_listener_requires_news_service() {
        let (_d, adapter) = mk_adapter("listener-nosvc").await;
        // No NewsService attached.
        let err = adapter.start_listener().await.unwrap_err();
        assert!(matches!(err, BridgeError::NotConnected));
    }

    #[tokio::test]
    async fn start_listener_with_news_service_emits_events() {
        use a3net_types::bulletin::BulletinCategory;
        let (_d, id) = mk_identity("listener-ok").await;
        let transport = Arc::new(InProcessGossip::new());
        let svc = make_in_memory_news_service(id.node_id(), transport.clone());
        let adapter = AdnetFeedAdapter::new(id, FeedConfig::default())
            .await
            .unwrap()
            .with_news_service(svc.clone())
            .await;
        adapter.connect().await.unwrap();
        adapter.start_listener().await.unwrap();
        // Publish via the news service to drive an event.
        let item = a3net_types::bulletin::BulletinItem::new(
            a3net_types::bulletin::BulletinKind::NewsArticle,
            BulletinCategory::Tech,
            a3net_types::bulletin::BulletinSeverity::Info,
            adapter.config.room_id.clone(),
            adapter.node_id(),
            "Live".to_string(),
            "S".to_string(),
            "B".to_string(),
            uuid::Uuid::new_v4().as_bytes(),
            None,
        )
        .unwrap();
        svc.publish(item).await.unwrap();
        // Wait briefly for the listener to relay the event.
        let ev = tokio::time::timeout(Duration::from_millis(500), async {
            let mut rx = adapter.subscribe_events().await;
            rx.recv().await
        })
        .await
        .expect("event received")
        .expect("event ok");
        match ev {
            FeedEvent::NewItem(_) => {}
            other => panic!("expected NewItem, got {other:?}"),
        }
        adapter.stop_listener().await;
    }

    #[tokio::test]
    async fn set_event_handler_forwards_events() {
        struct Capture {
            log: std::sync::Arc<tokio::sync::Mutex<Vec<FeedEvent>>>,
        }
        #[async_trait::async_trait]
        impl crate::feed_adapter::FeedEventHandler for Capture {
            async fn on_feed_event(&self, event: FeedEvent) {
                self.log.lock().await.push(event);
            }
        }
        let (_d, adapter) = mk_adapter("handler").await;
        let log = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let h = std::sync::Arc::new(Capture { log: log.clone() });
        let _ = adapter.clone().set_event_handler(h).await.unwrap();
        adapter.connect().await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let snapshot = log.lock().await.clone();
        assert!(!snapshot.is_empty(), "handler should have observed events");
    }

    #[tokio::test]
    async fn build_news_service_via_helper() {
        let dir = tempfile::tempdir().unwrap();
        let node = NodeId::random();
        let transport = Arc::new(InProcessGossip::new());
        let svc = build_news_service(node, transport, dir.path().to_path_buf()).unwrap();
        // The service is usable: subscribe() doesn't panic.
        let _rx = svc.subscribe();
    }

    #[tokio::test]
    async fn search_in_memory_finds_by_summary() {
        let (_d, id) = mk_identity("cache-search-sum").await;
        let adapter = AdnetFeedAdapter::new(id, FeedConfig::default())
            .await
            .unwrap();
        let mut cache = adapter.item_cache.write().await;
        cache.insert(
            "k".to_string(),
            FeedItem {
                id: "k".to_string(),
                author_id: NodeId::random(),
                author_name: "A".into(),
                title: "no match".into(),
                body: "no match".into(),
                summary: "marker-tag-1234".into(),
                category: BulletinCategory::Tech,
                severity: BulletinSeverity::Info,
                tags: vec![],
                published_at: 0,
                attachments: vec![],
            },
        );
        drop(cache);
        let hits = adapter.search("marker-tag", 10).await.unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[tokio::test]
    async fn get_feed_falls_back_to_cache_with_all_topic() {
        let (_d, id) = mk_identity("cache-all").await;
        let adapter = AdnetFeedAdapter::new(id, FeedConfig::default())
            .await
            .unwrap();
        // Insert items from two categories.
        let mut cache = adapter.item_cache.write().await;
        cache.insert(
            "t1".to_string(),
            FeedItem {
                id: "t1".to_string(),
                author_id: NodeId::random(),
                author_name: "A".into(),
                title: "T".into(),
                body: "b".into(),
                summary: "s".into(),
                category: BulletinCategory::Tech,
                severity: BulletinSeverity::Info,
                tags: vec![],
                published_at: 100,
                attachments: vec![],
            },
        );
        cache.insert(
            "g1".to_string(),
            FeedItem {
                id: "g1".to_string(),
                author_id: NodeId::random(),
                author_name: "B".into(),
                title: "T".into(),
                body: "b".into(),
                summary: "s".into(),
                category: BulletinCategory::General,
                severity: BulletinSeverity::Info,
                tags: vec![],
                published_at: 50,
                attachments: vec![],
            },
        );
        drop(cache);
        let all = adapter.get_feed("news-all", 10).await.unwrap();
        assert_eq!(all.len(), 2);
        // Sorted by published_at desc: t1 (100) then g1 (50).
        assert_eq!(all[0].id, "t1");
        assert_eq!(all[1].id, "g1");
        let tech = adapter.get_feed("news-tech", 10).await.unwrap();
        assert_eq!(tech.len(), 1);
        assert_eq!(tech[0].id, "t1");
    }
}

/// Callback trait for Eliza agent to handle feed events.
#[async_trait]
pub trait FeedEventHandler: Send + Sync {
    async fn on_feed_event(&self, event: FeedEvent);
}
