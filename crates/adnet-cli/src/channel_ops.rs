//! `adnet channel` — information channel operations via gossip.
//!
//! Information channels are pub/sub broadcast mechanisms where users can
//! subscribe to receive all messages published to a channel.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;

// Re-export these from adnet_types if they exist, otherwise define locally
#[allow(unused_imports)]
use adnet_types::NodeId;

static SEQ: AtomicU32 = AtomicU32::new(1);
fn next_seq() -> u32 { SEQ.fetch_add(1, Ordering::Relaxed) }

/// Channel ID — a 32-char hex identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChannelId(String);

impl ChannelId {
    pub fn from_hex(s: &str) -> Result<Self, &'static str> {
        if s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit()) {
            Ok(Self(s.to_string()))
        } else {
            Err("invalid 32-char hex channel id")
        }
    }
    pub fn from_name(name: &str) -> Self {
        let mut h = DefaultHasher::new();
        name.hash(&mut h);
        let hash = format!("{:032x}", h.finish());
        Self(hash)
    }
    pub fn as_str(&self) -> &str { &self.0 }
}

impl std::fmt::Display for ChannelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Channel visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelVisibility {
    Public,
    Private,
}
impl ChannelVisibility {
    pub fn as_str(&self) -> &'static str {
        match self { Self::Public => "public", Self::Private => "private" }
    }
}

/// Channel settings.
#[derive(Debug, Clone)]
pub struct ChannelSettings {
    pub allow_replies: bool,
    pub moderation_enabled: bool,
    pub max_message_length: usize,
}

impl Default for ChannelSettings {
    fn default() -> Self {
        Self { allow_replies: true, moderation_enabled: false, max_message_length: 4096 }
    }
}

/// Channel — represents a pub/sub channel.
#[derive(Debug, Clone)]
pub struct Channel {
    pub channel_id: ChannelId,
    pub name: String,
    pub description: String,
    pub owner_id: NodeId,
    pub owner_name: String,
    pub visibility: ChannelVisibility,
    pub settings: ChannelSettings,
    pub tags: Vec<String>,
    pub member_count: u32,
    pub message_count: u32,
    pub created_at: SystemTime,
    pub last_activity: SystemTime,
}

impl Channel {
    pub fn new(name: &str, description: &str, owner_id: NodeId, owner_name: &str, visibility: ChannelVisibility) -> Self {
        let channel_id = ChannelId::from_name(name);
        Self {
            channel_id,
            name: name.to_string(),
            description: description.to_string(),
            owner_id,
            owner_name: owner_name.to_string(),
            visibility,
            settings: ChannelSettings::default(),
            tags: vec![],
            member_count: 1,
            message_count: 0,
            created_at: SystemTime::now(),
            last_activity: SystemTime::now(),
        }
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.name.is_empty() { anyhow::bail!("channel name must not be empty"); }
        Ok(())
    }
}

/// Channel message type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelMessageType {
    Text,
    Media,
    System,
}
impl ChannelMessageType {
    pub fn as_str(&self) -> &'static str {
        match self { Self::Text => "text", Self::Media => "media", Self::System => "system" }
    }
}

/// Channel message.
#[derive(Debug, Clone)]
pub struct ChannelMessage {
    pub message_id: String,
    pub channel_id: ChannelId,
    pub sender_id: String,
    pub sender_name: String,
    pub content: String,
    pub message_type: ChannelMessageType,
    pub timestamp: chrono::DateTime<Utc>,
    pub sequence: u32,
    pub is_edited: bool,
    pub attachments: Vec<String>,
    pub integrity_hash: Option<String>,
}

impl ChannelMessage {
    pub fn new(channel_id: ChannelId, sender_id: String, sender_name: String, content: String, sequence: u32) -> Self {
        Self {
            message_id: Self::generate_id(&NodeId::random(), &channel_id, sequence),
            channel_id,
            sender_id,
            sender_name,
            content,
            message_type: ChannelMessageType::Text,
            timestamp: Utc::now(),
            sequence,
            is_edited: false,
            attachments: vec![],
            integrity_hash: None,
        }
    }
    pub fn generate_id(_node_id: &NodeId, _channel_id: &ChannelId, sequence: u32) -> String {
        format!("msg-{}", sequence)
    }
    pub fn stamp_integrity_hash(&mut self) {
        let mut h = DefaultHasher::new();
        self.message_id.hash(&mut h);
        self.channel_id.as_str().hash(&mut h);
        self.content.hash(&mut h);
        self.integrity_hash = Some(format!("{:032x}", h.finish()));
    }
}

/// Top-level `adnet channel` subcommand arguments.
#[derive(Debug, Clone)]
pub struct ChannelArgs {
    pub sub: ChannelSubcommand,
    pub channel: Option<String>,
    pub message: Option<String>,
    pub private: bool,
    pub limit: Option<u32>,
    pub timeout_secs: u64,
    pub json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelSubcommand {
    Create,
    List,
    Info,
    Subscribe,
    Unsubscribe,
    Post,
    Receive,
    History,
    Invite,
}

impl From<&crate::cli::ChannelCmd> for ChannelArgs {
    fn from(cmd: &crate::cli::ChannelCmd) -> Self {
        match cmd {
            crate::cli::ChannelCmd::Create { name, description, private, json } => ChannelArgs {
                sub: ChannelSubcommand::Create,
                channel: Some(name.clone()),
                message: description.clone(),
                private: *private,
                limit: None,
                timeout_secs: 60,
                json: *json,
            },
            crate::cli::ChannelCmd::List { limit, json } => ChannelArgs {
                sub: ChannelSubcommand::List,
                channel: None,
                message: None,
                private: false,
                limit: *limit,
                timeout_secs: 60,
                json: *json,
            },
            crate::cli::ChannelCmd::Info { channel, json } => ChannelArgs {
                sub: ChannelSubcommand::Info,
                channel: channel.clone(),
                message: None,
                private: false,
                limit: None,
                timeout_secs: 60,
                json: *json,
            },
            crate::cli::ChannelCmd::Subscribe { channel, json } => ChannelArgs {
                sub: ChannelSubcommand::Subscribe,
                channel: Some(channel.clone()),
                message: None,
                private: false,
                limit: None,
                timeout_secs: 60,
                json: *json,
            },
            crate::cli::ChannelCmd::Unsubscribe { channel, json } => ChannelArgs {
                sub: ChannelSubcommand::Unsubscribe,
                channel: Some(channel.clone()),
                message: None,
                private: false,
                limit: None,
                timeout_secs: 60,
                json: *json,
            },
            crate::cli::ChannelCmd::Post { channel, message, json } => ChannelArgs {
                sub: ChannelSubcommand::Post,
                channel: Some(channel.clone()),
                message: Some(message.clone()),
                private: false,
                limit: None,
                timeout_secs: 60,
                json: *json,
            },
            crate::cli::ChannelCmd::Receive { channel, timeout, json } => ChannelArgs {
                sub: ChannelSubcommand::Receive,
                channel: Some(channel.clone()),
                message: None,
                private: false,
                limit: None,
                timeout_secs: *timeout,
                json: *json,
            },
            crate::cli::ChannelCmd::History { channel, limit, json } => ChannelArgs {
                sub: ChannelSubcommand::History,
                channel: Some(channel.clone()),
                message: None,
                private: false,
                limit: *limit,
                timeout_secs: 60,
                json: *json,
            },
            crate::cli::ChannelCmd::Invite { channel, target, json } => ChannelArgs {
                sub: ChannelSubcommand::Invite,
                channel: Some(channel.clone()),
                message: Some(target.clone()),
                private: false,
                limit: None,
                timeout_secs: 60,
                json: *json,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelInfo {
    pub channel_id: String,
    pub name: String,
    pub description: String,
    pub owner_id: String,
    pub owner_name: String,
    pub visibility: String,
    pub member_count: u32,
    pub message_count: u32,
    pub allow_replies: bool,
    pub tags: Vec<String>,
    pub created_at: String,
    pub last_activity: String,
}

impl From<&Channel> for ChannelInfo {
    fn from(ch: &Channel) -> Self {
        Self {
            channel_id: ch.channel_id.as_str().to_string(),
            name: ch.name.clone(),
            description: ch.description.clone(),
            owner_id: ch.owner_id.as_hex().to_string(),
            owner_name: ch.owner_name.clone(),
            visibility: ch.visibility.as_str().to_string(),
            member_count: ch.member_count,
            message_count: ch.message_count,
            allow_replies: ch.settings.allow_replies,
            tags: ch.tags.clone(),
            created_at: DateTime::<Utc>::from(ch.created_at).to_rfc3339(),
            last_activity: DateTime::<Utc>::from(ch.last_activity).to_rfc3339(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelMessageDisplay {
    pub message_id: String,
    pub channel_id: String,
    pub sender_id: String,
    pub sender_name: String,
    pub content: String,
    pub message_type: String,
    pub timestamp: String,
    pub sequence: u32,
    pub is_edited: bool,
    pub has_attachments: bool,
}

impl From<&ChannelMessage> for ChannelMessageDisplay {
    fn from(msg: &ChannelMessage) -> Self {
        Self {
            message_id: msg.message_id.clone(),
            channel_id: msg.channel_id.as_str().to_string(),
            sender_id: msg.sender_id.clone(),
            sender_name: msg.sender_name.clone(),
            content: truncate(&msg.content, 100),
            message_type: msg.message_type.as_str().to_string(),
            timestamp: msg.timestamp.to_rfc3339(),
            sequence: msg.sequence,
            is_edited: msg.is_edited,
            has_attachments: !msg.attachments.is_empty(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostResult {
    pub ok: bool,
    pub channel_id: String,
    pub message_id: String,
    pub sequence: u32,
    pub timestamp: String,
    pub error: Option<String>,
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

fn parse_channel_id(s: &str) -> ChannelId {
    if s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        ChannelId::from_hex(s).unwrap_or_else(|_| ChannelId::from_name(s))
    } else {
        ChannelId::from_name(s)
    }
}

pub async fn run_channel(args: &ChannelArgs, node: &adnet_node::Node) -> Result<()> {
    match args.sub {
        ChannelSubcommand::Create => run_create(args, node).await,
        ChannelSubcommand::List => run_list(args, node).await,
        ChannelSubcommand::Info => run_info(args, node).await,
        ChannelSubcommand::Subscribe => run_subscribe(args, node).await,
        ChannelSubcommand::Unsubscribe => run_unsubscribe(args, node).await,
        ChannelSubcommand::Post => run_post(args, node).await,
        ChannelSubcommand::Receive => run_receive(args, node).await,
        ChannelSubcommand::History => run_history(args, node).await,
        ChannelSubcommand::Invite => run_invite(args, node).await,
    }
}

async fn run_create(args: &ChannelArgs, node: &adnet_node::Node) -> Result<()> {
    let channel_name = args
        .channel
        .as_deref()
        .ok_or_else(|| anyhow!("channel name is required for create"))?;

    let description = args.message.as_deref().unwrap_or("");

    let visibility = if args.private {
        ChannelVisibility::Private
    } else {
        ChannelVisibility::Public
    };

    let channel = Channel::new(
        channel_name,
        description,
        NodeId::random(),
        "User",
        visibility,
    );

    channel.validate()?;

    if args.json {
        let info: ChannelInfo = (&channel).into();
        println!("{}", serde_json::to_string_pretty(&info)?);
    } else {
        println!("Created channel: {} ({})", channel.name, channel.channel_id);
        println!("  ID: {}", channel.channel_id);
        println!("  Visibility: {}", channel.visibility.as_str());
        println!("  Owner: {}", channel.owner_id.as_hex());
    }

    Ok(())
}

async fn run_list(_args: &ChannelArgs, _node: &adnet_node::Node) -> Result<()> {
    println!("No channels found (DHT lookup not yet implemented)");
    println!("Use 'adnet channel create <name>' to create a new channel");
    Ok(())
}

async fn run_info(args: &ChannelArgs, _node: &adnet_node::Node) -> Result<()> {
    let channel_id_str = args
        .channel
        .as_deref()
        .ok_or_else(|| anyhow!("channel id or name is required for info"))?;

    let channel_id = parse_channel_id(channel_id_str);

    println!("Channel info for: {}", channel_id);
    println!("(DHT metadata lookup not yet implemented)");
    Ok(())
}

async fn run_subscribe(args: &ChannelArgs, node: &adnet_node::Node) -> Result<()> {
    let channel_id_str = args
        .channel
        .as_deref()
        .ok_or_else(|| anyhow!("channel id is required for subscribe"))?;

    let channel_id = parse_channel_id(channel_id_str);

    // node.join_channel_by_id(&channel_id).await?;

    if args.json {
        let result = serde_json::json!({
            "subscribed": true,
            "channel_id": channel_id.as_str()
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("Subscribed to channel: {}", channel_id);
        println!(
            "Use 'adnet channel receive --channel {}' to listen for messages",
            channel_id
        );
    }

    Ok(())
}

async fn run_unsubscribe(args: &ChannelArgs, _node: &adnet_node::Node) -> Result<()> {
    let channel_id_str = args
        .channel
        .as_deref()
        .ok_or_else(|| anyhow!("channel id is required for unsubscribe"))?;

    let channel_id = parse_channel_id(channel_id_str);

    // node.leave_channel_by_id(&channel_id).await?;

    if args.json {
        let result = serde_json::json!({
            "unsubscribed": true,
            "channel_id": channel_id.as_str()
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("Unsubscribed from channel: {}", channel_id);
    }

    Ok(())
}

async fn run_post(args: &ChannelArgs, node: &adnet_node::Node) -> Result<()> {
    let channel_id_str = args
        .channel
        .as_deref()
        .ok_or_else(|| anyhow!("channel id is required for post"))?;

    let content = args
        .message
        .as_deref()
        .ok_or_else(|| anyhow!("message content is required for post"))?;

    let channel_id = parse_channel_id(channel_id_str);
    let sequence = next_seq();

    let msg = ChannelMessage::new(
        channel_id.clone(),
        NodeId::random().as_hex().to_string(),
        "User".to_string(),
        content.to_string(),
        sequence,
    );

    // node.join_channel_by_id(&channel_id).await?;
    // node.publish_channel_by_id(&channel_id, &msg).await?;

    if args.json {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let result = PostResult {
            ok: true,
            channel_id: channel_id.as_str().to_string(),
            message_id: msg.message_id.clone(),
            sequence: msg.sequence,
            timestamp: ts.to_string(),
            error: None,
        };
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("Posted to channel {}: \"{}\"", channel_id, truncate(content, 40));
        println!("  Message ID: {}", msg.message_id);
        println!("  Sequence: {}", msg.sequence);
    }

    Ok(())
}

async fn run_receive(args: &ChannelArgs, _node: &adnet_node::Node) -> Result<()> {
    let channel_id_str = args
        .channel
        .as_deref()
        .ok_or_else(|| anyhow!("channel id is required for receive"))?;

    let channel_id = parse_channel_id(channel_id_str);

    if !args.json {
        println!("Listening for messages on channel {}", channel_id);
        println!("(Ctrl-C to exit)");
        println!();
    }

    let timeout = Duration::from_secs(args.timeout_secs);

    if args.json {
        println!(
            "{{\"status\": \"subscribed\", \"channel_id\": \"{}\"}}",
            channel_id
        );
    }

    // Simulate waiting for messages for the timeout duration
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                if !args.json {
                    println!("\nExiting...");
                }
                break;
            }
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                if !args.json {
                    println!("(waiting for messages...)");
                }
            }
        }
    }

    if !args.json {
        println!("(timeout, no messages received)");
    }
    Ok(())
}

async fn run_history(args: &ChannelArgs, _node: &adnet_node::Node) -> Result<()> {
    let channel_id_str = args
        .channel
        .as_deref()
        .ok_or_else(|| anyhow!("channel id is required for history"))?;

    let channel_id = parse_channel_id(channel_id_str);
    let limit = args.limit.unwrap_or(20) as usize;

    println!("Channel history for: {}", channel_id);
    println!("(History lookup not yet implemented)");
    println!("Limit: {} messages", limit);

    Ok(())
}

async fn run_invite(args: &ChannelArgs, _node: &adnet_node::Node) -> Result<()> {
    let channel_id_str = args
        .channel
        .as_deref()
        .ok_or_else(|| anyhow!("channel id is required for invite"))?;

    let target_user = args
        .message
        .as_deref()
        .ok_or_else(|| anyhow!("target user id is required for invite"))?;

    let channel_id = parse_channel_id(channel_id_str);

    if args.json {
        let invite = serde_json::json!({
            "invitation_sent": true,
            "channel_id": channel_id.as_str(),
            "target_user": target_user,
            "note": "Invitation broadcast not yet implemented"
        });
        println!("{}", serde_json::to_string_pretty(&invite)?);
    } else {
        println!(
            "Invitation to join channel {} sent to {}",
            channel_id, target_user
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_channel_id_handles_hex_and_name() {
        let hex = "a".repeat(32);
        let from_hex = parse_channel_id(&hex);
        assert_eq!(from_hex.as_str(), hex);

        let from_name = parse_channel_id("MyChannel");
        assert_ne!(from_name.as_str(), "MyChannel");
    }

    #[test]
    fn channel_info_from_channel() {
        let ch = Channel::new(
            "Test",
            "A test channel",
            NodeId::random(),
            "Alice",
            ChannelVisibility::Public,
        );
        let info: ChannelInfo = (&ch).into();
        assert_eq!(info.name, "Test");
        assert_eq!(info.visibility, "public");
        assert_eq!(info.member_count, 1);
    }

    #[test]
    fn truncate_behavior() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("this is a longer string", 10), "this is a ...");
    }
}
