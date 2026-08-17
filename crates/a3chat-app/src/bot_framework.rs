//! Bot 框架 - 用于自动化聊天测试和演示
//!
//! 这个模块提供了两个自动聊天机器人 (Bot A 和 Bot B) 相互对话的功能。
//! 机器人使用简单的规则引擎生成回复，可以用于：
//! - 端到端测试
//! - 演示系统功能
//! - 负载测试

use std::collections::HashMap;
use std::sync::Arc;

use a3chat_core::id::{ConversationId, UserId};
use a3chat_core::message::{MessageBody, MessageEnvelope, MessageType};
use tokio::sync::RwLock;

/// Bot 的角色类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotRole {
    /// 主动发起对话的机器人
    Initiator,
    /// 被动响应消息的机器人
    Responder,
}

/// Bot 的配置
#[derive(Debug, Clone)]
pub struct BotConfig {
    /// Bot 的用户 ID
    pub user_id: UserId,
    /// Bot 的显示名称
    pub display_name: String,
    /// 角色类型
    pub role: BotRole,
    /// 发送消息的间隔 (秒)
    pub send_interval_secs: u64,
    /// 是否启用 E2E 加密
    pub enable_e2e: bool,
}

impl BotConfig {
    pub fn new(user_id: &str, display_name: &str, role: BotRole) -> Self {
        Self {
            user_id: UserId::from(user_id.to_string()),
            display_name: display_name.to_string(),
            role,
            send_interval_secs: 3,
            enable_e2e: false, // 测试时禁用 E2E 以便验证内容
        }
    }

    pub fn as_initiator(user_id: &str, display_name: &str) -> Self {
        Self::new(user_id, display_name, BotRole::Initiator)
    }

    pub fn as_responder(user_id: &str, display_name: &str) -> Self {
        Self::new(user_id, display_name, BotRole::Responder)
    }

    pub fn with_interval(mut self, secs: u64) -> Self {
        self.send_interval_secs = secs;
        self
    }

    pub fn with_e2e(mut self, enabled: bool) -> Self {
        self.enable_e2e = enabled;
        self
    }
}

/// Bot 的状态
#[derive(Debug, Clone)]
pub struct BotState {
    pub config: BotConfig,
    /// 已发送的消息计数
    pub messages_sent: u64,
    /// 已接收的消息计数
    pub messages_received: u64,
    /// 最近的对话内容 (用于生成回复)
    pub recent_messages: Vec<String>,
}

impl BotState {
    pub fn new(config: BotConfig) -> Self {
        Self {
            config,
            messages_sent: 0,
            messages_received: 0,
            recent_messages: Vec::new(),
        }
    }

    pub fn add_received_message(&mut self, content: &str) {
        self.messages_received += 1;
        self.recent_messages.push(content.to_string());
        // 保留最近 10 条消息
        if self.recent_messages.len() > 10 {
            self.recent_messages.remove(0);
        }
    }
}

/// 简单的回复生成器
pub struct ReplyGenerator {
    /// 预定义的回复模板
    templates: HashMap<String, Vec<String>>,
    /// 默认回复
    default_replies: Vec<String>,
}

impl Default for ReplyGenerator {
    fn default() -> Self {
        let mut templates = HashMap::new();

        // 问候类
        templates.insert(
            "greeting".to_string(),
            vec![
                "你好!".to_string(),
                "嗨，很高兴见到你!".to_string(),
                "你好，今天怎么样?".to_string(),
            ],
        );

        // 问题类
        templates.insert(
            "question".to_string(),
            vec![
                "这是个有趣的问题!".to_string(),
                "让我想想...".to_string(),
                "好问题，我需要一点时间来回答。".to_string(),
            ],
        );

        // 确认类
        templates.insert(
            "confirm".to_string(),
            vec![
                "好的，明白了!".to_string(),
                "收到，没问题!".to_string(),
                "明白了，我这就处理。".to_string(),
            ],
        );

        // 随机/默认回复
        let default_replies = vec![
            "嗯，我明白了。".to_string(),
            "让我想想怎么回复...".to_string(),
            "好的!".to_string(),
            "收到!".to_string(),
            "有意思!".to_string(),
            "我同意。".to_string(),
            "让我们继续聊聊吧。".to_string(),
            "这个话题很有趣。".to_string(),
            "继续说下去。".to_string(),
            "嗯嗯，我听着呢。".to_string(),
        ];

        Self {
            templates,
            default_replies,
        }
    }
}

impl ReplyGenerator {
    /// 根据输入生成回复
    pub fn generate_reply(&self, input: &str) -> String {
        let input_lower = input.to_lowercase();

        // 检测输入类型并选择合适的回复模板
        if input_lower.contains("你好")
            || input_lower.contains("嗨")
            || input_lower.contains("hi")
            || input_lower.contains("hello")
        {
            return self.random_reply("greeting");
        }

        if input_lower.contains('?') || input_lower.contains("怎么") || input_lower.contains("什么") {
            return self.random_reply("question");
        }

        if input_lower.contains("好的")
            || input_lower.contains("ok")
            || input_lower.contains("yes")
            || input_lower.contains("是")
        {
            return self.random_reply("confirm");
        }

        // 默认回复
        self.random_default_reply()
    }

    fn random_reply(&self, key: &str) -> String {
        if let Some(replies) = self.templates.get(key) {
            if !replies.is_empty() {
                let idx = rand_index(replies.len());
                return replies[idx].clone();
            }
        }
        self.random_default_reply()
    }

    fn random_default_reply(&self) -> String {
        let idx = rand_index(self.default_replies.len());
        self.default_replies[idx].clone()
    }
}

/// 生成随机索引
fn rand_index(max: usize) -> usize {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    nanos as usize % max
}

/// Bot 发送的消息事件
#[derive(Debug, Clone)]
pub enum BotEvent {
    /// Bot 发送了消息
    MessageSent {
        bot_id: UserId,
        content: String,
        sequence: u32,
    },
    /// Bot 收到了消息
    MessageReceived {
        bot_id: UserId,
        from_user: UserId,
        content: String,
    },
    /// Bot 状态更新
    StateUpdated {
        bot_id: UserId,
        messages_sent: u64,
        messages_received: u64,
    },
}

/// 聊天机器人
pub struct ChatBot {
    config: BotConfig,
    state: Arc<RwLock<BotState>>,
    reply_generator: ReplyGenerator,
    conversation_id: Option<ConversationId>,
    peer_user_id: Option<UserId>,
    sequence: Arc<RwLock<u64>>,
}

impl ChatBot {
    /// 创建新的 Bot
    pub fn new(config: BotConfig) -> Self {
        let config_clone = config.clone();
        Self {
            config,
            state: Arc::new(RwLock::new(BotState::new(config_clone))),
            reply_generator: ReplyGenerator::default(),
            conversation_id: None,
            peer_user_id: None,
            sequence: Arc::new(RwLock::new(0u64)),
        }
    }

    /// 设置对话信息
    pub fn set_conversation(&mut self, conversation_id: ConversationId, peer_user_id: UserId) {
        self.conversation_id = Some(conversation_id);
        self.peer_user_id = Some(peer_user_id);
    }

    /// 获取 Bot 的用户 ID
    pub fn user_id(&self) -> &UserId {
        &self.config.user_id
    }

    /// 获取显示名称
    pub fn display_name(&self) -> &str {
        &self.config.display_name
    }

    /// 获取角色
    pub fn role(&self) -> &BotRole {
        &self.config.role
    }

    /// 获取当前状态
    pub async fn state(&self) -> BotState {
        self.state.read().await.clone()
    }

    /// 记录收到的消息
    pub async fn record_received(&self, content: &str) {
        let mut state = self.state.write().await;
        state.add_received_message(content);
    }

    /// 生成回复内容
    pub fn generate_reply(&self, input: &str) -> String {
        self.reply_generator.generate_reply(input)
    }

    /// 创建要发送的消息信封
    pub async fn create_message_envelope(&self, content: &str) -> Option<MessageEnvelope> {
        let conv_id = self.conversation_id.clone()?;
        let peer = self.peer_user_id.clone()?;

        let mut seq = self.sequence.write().await;
        *seq += 1;
        let current_seq = *seq;

        let timestamp = chrono::Utc::now().timestamp();

        Some(MessageEnvelope {
            conversation_id: conv_id,
            receiver_id: peer,
            message_type: MessageType::Text,
            body: MessageBody::Plain {
                content: content.to_string(),
            },
            attachments: vec![],
            reply_to: None,
            sequence: current_seq as u32,
            timestamp,
        })
    }

    /// 创建消息并更新状态
    pub async fn prepare_message(&self, content: &str) -> Option<(MessageEnvelope, u32)> {
        let envelope = self.create_message_envelope(content).await?;
        let seq = envelope.sequence;

        // 更新发送计数
        {
            let mut state = self.state.write().await;
            state.messages_sent += 1;
        }

        Some((envelope, seq))
    }
}

/// Bot 事件处理器
pub trait BotEventHandler: Send + Sync {
    fn on_message_sent(&self, bot_id: &UserId, content: &str, sequence: u32);
    fn on_message_received(&self, bot_id: &UserId, from_user: &UserId, content: &str);
    fn on_state_updated(&self, bot_id: &UserId, sent: u64, received: u64);
}

impl<F> BotEventHandler for F
where
    F: Fn(&UserId, &str, u32) + Send + Sync,
{
    fn on_message_sent(&self, bot_id: &UserId, content: &str, sequence: u32) {
        self(bot_id, content, sequence)
    }
    fn on_message_received(&self, _bot_id: &UserId, _from_user: &UserId, _content: &str) {}
    fn on_state_updated(&self, _bot_id: &UserId, _sent: u64, _received: u64) {}
}

/// 完整的 Bot 会话管理器
pub struct BotSession {
    /// Bot A (发起者)
    pub bot_a: ChatBot,
    /// Bot B (响应者)
    pub bot_b: ChatBot,
    /// 共享的对话 ID
    pub conversation_id: ConversationId,
    /// 事件处理器
    event_handler: Option<Box<dyn BotEventHandler>>,
}

impl BotSession {
    /// 创建新的 Bot 会话
    pub fn new(bot_a_config: BotConfig, bot_b_config: BotConfig) -> Self {
        let bot_a = ChatBot::new(bot_a_config);
        let bot_b = ChatBot::new(bot_b_config);

        // 生成共享的对话 ID
        let conversation_id = ConversationId::from(format!(
            "dm:{}:{}",
            bot_a.user_id(),
            bot_b.user_id()
        ));

        Self {
            bot_a,
            bot_b,
            conversation_id,
            event_handler: None,
        }
    }

    /// 设置事件处理器
    pub fn with_event_handler(mut self, handler: Box<dyn BotEventHandler>) -> Self {
        self.event_handler = Some(handler);
        self
    }

    /// 获取 Bot A
    pub fn alice(&self) -> &ChatBot {
        &self.bot_a
    }

    /// 获取 Bot B
    pub fn bob(&self) -> &ChatBot {
        &self.bot_b
    }

    /// 获取对话 ID
    pub fn conversation_id(&self) -> &ConversationId {
        &self.conversation_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bot_config() {
        let config = BotConfig::as_initiator("alice", "Alice Bot");
        assert_eq!(config.user_id.as_str(), "alice");
        assert_eq!(config.display_name, "Alice Bot");
        assert_eq!(config.role, BotRole::Initiator);
    }

    #[test]
    fn test_reply_generator_greeting() {
        let generator = ReplyGenerator::default();
        let reply = generator.generate_reply("你好!");
        assert!(reply.contains("你好") || reply.contains("高兴"));
    }

    #[test]
    fn test_reply_generator_question() {
        let generator = ReplyGenerator::default();
        let reply = generator.generate_reply("你今天怎么样?");
        assert!(reply.contains("问题") || reply.contains("想想") || reply.contains("有趣"));
    }

    #[test]
    fn test_reply_generator_default() {
        let generator = ReplyGenerator::default();
        let reply = generator.generate_reply("今天天气真好");
        // 默认回复应该是非空的
        assert!(!reply.is_empty());
    }

    #[tokio::test]
    async fn test_bot_state() {
        let config = BotConfig::as_responder("bob", "Bob Bot");
        let state = BotState::new(config.clone());

        assert_eq!(state.messages_sent, 0);
        assert_eq!(state.messages_received, 0);
        assert!(state.recent_messages.is_empty());

        // 模拟收到消息
        let mut state = state;
        state.add_received_message("Hello");
        assert_eq!(state.messages_received, 1);
        assert_eq!(state.recent_messages.len(), 1);
    }

    #[tokio::test]
    async fn test_bot_prepare_message() {
        let mut config = BotConfig::as_initiator("alice", "Alice");
        config.enable_e2e = false;
        let mut bot = ChatBot::new(config);

        let conv_id = ConversationId::from("dm:alice:bob");
        let peer_id = UserId::from("bob");
        bot.set_conversation(conv_id.clone(), peer_id.clone());

        let (envelope, seq) = bot.prepare_message("Hello Bob!").await.unwrap();

        assert_eq!(envelope.conversation_id, conv_id);
        assert_eq!(envelope.receiver_id, peer_id);
        assert!(matches!(envelope.body, MessageBody::Plain { .. }));
        assert_eq!(seq, 1);
    }
}
