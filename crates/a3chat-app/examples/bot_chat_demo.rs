//! A3Chat Bot 多轮对话演示程序
//!
//! 这是一个完整的演示程序，展示两个聊天机器人 (Alice Bot 和 Bob Bot)
//! 如何在 A3Chat 应用中相互对话，支持多轮对话、上下文感知回复等。
//!
//! 运行方式:
//! ```bash
//! cargo run --example bot_chat_demo -p a3chat-app
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use a3chat_app::chat_service::ChatService;
use a3chat_app::notification_bus::NotificationBus;
use a3chat_app::storage::{ChatStorage, StorageConfig};

use a3chat_core::id::{ConversationId, UserId};
use a3chat_core::message::{MessageBody, MessageEnvelope, MessageType};

// ============================================================================
// Bot 定义
// ============================================================================

/// Bot 状态统计
#[derive(Debug, Default)]
struct BotStats {
    messages_sent: u32,
    messages_received: u32,
    typing_count: u32,
}

/// Bot 配置
#[derive(Debug, Clone)]
struct BotConfig {
    id: UserId,
    name: String,
    personality: BotPersonality,
}

impl BotConfig {
    fn new(id: &str, name: &str, personality: BotPersonality) -> Self {
        Self {
            id: UserId::from(id.to_string()),
            name: name.to_string(),
            personality,
        }
    }
}

/// Bot 个性
#[derive(Debug, Clone)]
enum BotPersonality {
    Friendly,   // 友好型
    Technical,   // 技术型
    Humorous,   // 幽默型
    Curious,    // 好奇型
}

/// Bot 结构体
struct Bot {
    config: BotConfig,
    service: ChatService,
    stats: BotStats,
    reply_gen: Arc<tokio::sync::Mutex<SmartReplyGenerator>>,
    /// 当前对话上下文
    current_topic: Option<String>,
    /// 对话历史
    conversation_history: Vec<ConversationEntry>,
}

/// 对话条目
#[derive(Debug, Clone)]
struct ConversationEntry {
    speaker: String,
    content: String,
    timestamp: i64,
    topic: Option<String>,
}

impl Bot {
    /// 创建新的 Bot
    async fn new(
        user_id: &str,
        display_name: &str,
        personality: BotPersonality,
        base_dir: &std::path::Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let user_id = UserId::from(user_id.to_string());
        let keyring = a3chat_app::E2eKeyring::new(user_id.clone());
        let storage = ChatStorage::new(StorageConfig::new(base_dir.to_path_buf()), keyring);
        storage.init_user(&user_id).await?;

        let bus = NotificationBus::new(64);
        let service = ChatService::new(storage, bus);

        let personality_clone = personality.clone();
        Ok(Self {
            config: BotConfig::new(user_id.as_str(), display_name, personality_clone),
            service,
            stats: BotStats::default(),
            reply_gen: Arc::new(tokio::sync::Mutex::new(SmartReplyGenerator::new(personality))),
            current_topic: None,
            conversation_history: Vec::new(),
        })
    }

    /// 发送消息
    async fn send_message(
        &mut self,
        receiver_id: &UserId,
        conversation_id: &ConversationId,
        content: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.stats.messages_sent += 1;

        let envelope = MessageEnvelope {
            conversation_id: conversation_id.clone(),
            receiver_id: receiver_id.clone(),
            message_type: MessageType::Text,
            body: MessageBody::Plain {
                content: content.to_string(),
            },
            attachments: vec![],
            reply_to: None,
            sequence: self.stats.messages_sent,
            timestamp: chrono::Utc::now().timestamp(),
        };

        self.service.send_message(&self.config.id, &envelope).await?;

        // 添加到对话历史
        self.conversation_history.push(ConversationEntry {
            speaker: self.config.name.clone(),
            content: content.to_string(),
            timestamp: chrono::Utc::now().timestamp(),
            topic: self.current_topic.clone(),
        });

        Ok(())
    }

    /// 生成回复
    async fn generate_reply(&mut self, input: &str, context: &[ConversationEntry]) -> String {
        let mut reply_gen = self.reply_gen.lock().await;
        let reply = reply_gen.generate(input, context, &self.config.personality);
        self.current_topic = reply_gen.current_topic.clone();
        reply
    }

    /// 模拟打字
    async fn simulate_typing(&mut self, content_len: usize) {
        self.stats.typing_count += 1;
        let typing_time = (content_len as u64) * 20 + 100;
        tokio::time::sleep(Duration::from_millis(typing_time)).await;
    }

    /// 获取统计
    fn stats(&self) -> &BotStats {
        &self.stats
    }

    /// 获取 ID
    fn id(&self) -> &UserId {
        &self.config.id
    }

    /// 获取名字
    fn name(&self) -> &str {
        &self.config.name
    }

    /// 获取对话历史
    fn history(&self) -> &[ConversationEntry] {
        &self.conversation_history
    }
}

// ============================================================================
// 智能回复生成器 (增强版)
// ============================================================================

/// 回复类型
#[derive(Debug, Clone, Eq, Hash, PartialEq)]
enum ReplyCategory {
    Greeting,
    Weather,
    Work,
    Programming,
    Question,
    Affirmation,
    Humor,
    TopicShift,
    Default,
}

impl ReplyCategory {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Greeting => "问候",
            Self::Weather => "天气",
            Self::Work => "工作",
            Self::Programming => "编程",
            Self::Question => "提问",
            Self::Affirmation => "确认",
            Self::Humor => "幽默",
            Self::TopicShift => "话题转换",
            Self::Default => "通用",
        }
    }
}

/// 智能回复生成器
struct SmartReplyGenerator {
    current_topic: Option<String>,
    topic_history: Vec<String>,
    replies: HashMap<ReplyCategory, Vec<&'static str>>,
}

impl SmartReplyGenerator {
    fn new(_personality: BotPersonality) -> Self {
        let mut replies = HashMap::new();

        replies.insert(ReplyCategory::Greeting, vec![
            "你好!很高兴见到你! 😊",
            "嗨，你好呀!",
            "你好!今天怎么样?",
            "嗨! 最近好吗?",
            "嘿! 很高兴认识你!",
        ]);

        replies.insert(ReplyCategory::Weather, vec![
            "今天天气很棒!",
            "希望天气一直这么好!",
            "天气确实不错，适合散步!",
            "是啊，天气真好!",
            "这天气让人心情愉悦!",
        ]);

        replies.insert(ReplyCategory::Work, vec![
            "工作顺利吗?",
            "注意休息，别太累了!",
            "工作很重要，但也要照顾好自己!",
            "有什么我可以帮忙的吗?",
            "工作顺利最重要!",
        ]);

        replies.insert(ReplyCategory::Programming, vec![
            "Rust 是一门很棒的语言! 🚀",
            "编程确实很有趣!",
            "你最喜欢什么编程语言?",
            "代码就是艺术!",
            "写代码很有成就感!",
            "我也喜欢研究新技术!",
        ]);

        replies.insert(ReplyCategory::Question, vec![
            "这是个有趣的问题! 🤔",
            "让我想想...这需要一些考虑。",
            "好问题!我认为...",
            "有意思，我也经常思考这个问题。",
            "这是个值得深入探讨的话题!",
        ]);

        replies.insert(ReplyCategory::Affirmation, vec![
            "好的，明白了!",
            "没错，你说得对!",
            "我也这么想!",
            "完全同意!",
            "你说得很有道理!",
        ]);

        replies.insert(ReplyCategory::Humor, vec![
            "哈哈，太有趣了! 😄",
            "这让我笑了!",
            "太逗了!",
            "有意思的笑话!",
            "你真幽默!",
        ]);

        replies.insert(ReplyCategory::TopicShift, vec![
            "说起来，我们换个话题吧。",
            "对了，你知道吗...",
            "这个话题很有趣，但我想到另一个问题...",
            "让我问你一件事...",
            "不过我们也可以聊聊别的...",
        ]);

        replies.insert(ReplyCategory::Default, vec![
            "嗯，我明白了。",
            "有意思!",
            "继续说吧，我听着呢。",
            "好的!",
            "这个话题很有趣。",
            "我同意你的观点。",
            "让我想想...",
            "继续!",
            "这很有见地!",
        ]);

        Self {
            current_topic: None,
            topic_history: Vec::new(),
            replies,
        }
    }

    /// 生成回复
    fn generate(&mut self, input: &str, context: &[ConversationEntry], personality: &BotPersonality) -> String {
        let input_lower = input.to_lowercase();

        // 检测话题
        let category = self.detect_category(&input_lower);

        // 根据个性调整回复
        let base_reply = self.get_reply_for_category(&category);

        let reply = match personality {
            BotPersonality::Friendly => self.apply_friendly_tone(&base_reply),
            BotPersonality::Technical => self.apply_technical_tone(&base_reply, &input_lower),
            BotPersonality::Humorous => self.apply_humor_tone(&base_reply),
            BotPersonality::Curious => self.apply_curious_tone(&base_reply, &input_lower),
        };

        // 更新当前话题
        self.update_topic(&category);

        reply
    }

    fn detect_category(&self, input: &str) -> ReplyCategory {
        if input.contains("你好") || input.contains("嗨") || input.contains("hi") || input.contains("hello") {
            ReplyCategory::Greeting
        } else if input.contains("天气") {
            ReplyCategory::Weather
        } else if input.contains("工作") || input.contains("忙") || input.contains("加班") {
            ReplyCategory::Work
        } else if input.contains("编程") || input.contains("代码") || input.contains("rust") 
                  || input.contains("python") || input.contains("javascript") {
            ReplyCategory::Programming
        } else if input.contains('?') || input.contains("怎么") || input.contains("什么") 
                  || input.contains("为什么") || input.contains("如何") {
            ReplyCategory::Question
        } else if input.contains("好的") || input.contains("ok") || input.contains("yes") || input.contains("对") {
            ReplyCategory::Affirmation
        } else if input.contains("哈哈") || input.contains("笑") || input.contains("有趣") {
            ReplyCategory::Humor
        } else {
            ReplyCategory::Default
        }
    }

    fn get_reply_for_category(&self, category: &ReplyCategory) -> String {
        if let Some(replies) = self.replies.get(category) {
            let idx = random_index(replies.len());
            replies[idx].to_string()
        } else {
            "嗯，我明白了。".to_string()
        }
    }

    fn apply_friendly_tone(&self, base: &str) -> String {
        let friendly_prefixes = ["太好了!", "太棒了!", "真不错!"];
        let prefix = friendly_prefixes[random_index(friendly_prefixes.len())];
        format!("{} {}", prefix, base)
    }

    fn apply_technical_tone(&self, base: &str, input: &str) -> String {
        if input.contains("编程") || input.contains("代码") {
            format!("从技术角度来看，{}", base)
        } else {
            base.to_string()
        }
    }

    fn apply_humor_tone(&self, base: &str) -> String {
        let humor_suffixes = ["😄", "😂", "🤣"];
        let suffix = humor_suffixes[random_index(humor_suffixes.len())];
        format!("{} {}", base, suffix)
    }

    fn apply_curious_tone(&self, base: &str, input: &str) -> String {
        if !input.contains('?') {
            format!("{} 对了，你有什么看法?", base)
        } else {
            format!("这是个很好的问题! {}", base)
        }
    }

    fn update_topic(&mut self, category: &ReplyCategory) {
        let topic = category.as_str();
        if self.topic_history.is_empty() || self.topic_history.last() != Some(&topic.to_string()) {
            self.topic_history.push(topic.to_string());
            if self.topic_history.len() > 10 {
                self.topic_history.remove(0);
            }
        }
        self.current_topic = Some(topic.to_string());
    }
}

fn random_index(max: usize) -> usize {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    (nanos as usize) % max
}

// ============================================================================
// 打印格式化
// ============================================================================

fn print_header(title: &str) {
    println!();
    println!("╔═══════════════════════════════════════════════════════════════════╗");
    println!("║ {:^63} ║", title);
    println!("╚═══════════════════════════════════════════════════════════════════╝");
}

fn print_round(round: u32) {
    println!();
    println!("┌─────────────────────────────────────────────────────────────────┐");
    println!("│                         第 {} 轮对话                               │", round);
    println!("└─────────────────────────────────────────────────────────────────┘");
}

fn print_bot_message(speaker: &str, content: &str) {
    println!("  📤 [{}]: {}", speaker, content);
}

fn print_bot_stats(name: &str, stats: &BotStats) {
    println!("  📊 {} - 发送: {} | 收到: {} | 打字: {}", name, stats.messages_sent, stats.messages_received, stats.typing_count);
}

fn print_topic(topic: &str) {
    println!("     💬 话题: {}", topic);
}

fn print_separator() {
    println!("─────────────────────────────────────────────────────────────────────");
}

// ============================================================================
// 主程序
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    print_header("A3Chat Bot 多轮对话演示 🔥");

    // 创建临时目录用于存储
    let base_dir = tempfile::tempdir()?;
    println!("\n📁 存储目录: {:?}", base_dir.path());

    // 创建两个 Bot (使用不同的个性)
    println!("\n🤖 初始化机器人...");

    let mut alice = Bot::new(
        "alice-bot",
        "Alice",
        BotPersonality::Friendly,
        base_dir.path(),
    ).await?;

    let mut bob = Bot::new(
        "bob-bot",
        "Bob",
        BotPersonality::Technical,
        base_dir.path(),
    ).await?;

    println!("   ✅ Alice Bot (友善型) - ID: {}", alice.id());
    println!("   ✅ Bob Bot (技术型) - ID: {}", bob.id());

    // 创建对话
    let conversation_id = ConversationId::from(format!("dm:{}:{}", alice.id(), bob.id()));
    println!("\n💬 对话ID: {}", conversation_id);

    // 多轮对话主题
    let dialogue_topics = vec![
        ("你好 Bob!", "greeting"),
        ("今天的天气真不错!", "weather"),
        ("你最近工作忙吗?", "work"),
        ("我最近在学习 Rust 编程", "programming"),
        ("你最喜欢什么编程语言?", "programming"),
        ("Rust 确实很棒，它的类型系统很强大!", "programming"),
        ("没错!而且 Rust 的内存安全保证让人很放心", "programming"),
        ("是啊，我们换个话题吧。你周末有什么计划?", "topic_shift"),
        ("还没想好，可能会在家休息", "general"),
        ("好的，好好休息很重要!", "general"),
        ("我们继续聊天吧!", "general"),
        ("好的，你还想聊什么?", "question"),
        ("聊聊 AI 吧，你对 AI 怎么看?", "tech"),
        ("AI 真的很神奇，特别是大语言模型", "tech"),
    ];

    print_separator();
    println!("                         🌟 开始多轮对话 🌟");
    print_separator();

    // 运行多轮对话
    for (round, (alice_msg, topic)) in dialogue_topics.iter().enumerate() {
        let round_num = round + 1;
        print_round(round_num as u32);

        // === Alice 发送消息 ===
        print_bot_message(alice.name(), alice_msg);
        alice.send_message(bob.id(), &conversation_id, alice_msg).await?;
        print_topic(topic);

        // 模拟 Alice 打字后 Bob 收到并回复
        bob.simulate_typing(30).await;
        bob.stats.messages_received += 1;

        // Bob 生成回复 (基于 Alice 的消息和对话历史)
        let bob_reply = bob.generate_reply(alice_msg, alice.history()).await;
        bob.simulate_typing(bob_reply.len()).await;
        bob.send_message(alice.id(), &conversation_id, &bob_reply).await?;
        print_bot_message(bob.name(), &bob_reply);

        // Alice 收到 Bob 的回复
        alice.stats.messages_received += 1;

        // Alice 生成回复
        let alice_reply = alice.generate_reply(&bob_reply, bob.history()).await;
        alice.simulate_typing(alice_reply.len()).await;
        alice.send_message(bob.id(), &conversation_id, &alice_reply).await?;
        print_bot_message(alice.name(), &alice_reply);

        // Bob 收到 Alice 的回复
        bob.stats.messages_received += 1;

        print_separator();
    }

    // 打印对话统计
    print_header("📊 对话统计");

    println!();
    print_bot_stats(alice.name(), alice.stats());
    print_bot_stats(bob.name(), bob.stats());

    // 对话历史分析
    println!("\n📜 对话历史摘要 (最近 6 条):");
    for (i, entry) in alice.history().iter().rev().take(6).enumerate() {
        println!("   {}. [{}] {}", i + 1, entry.speaker, entry.content);
    }

    // 话题分布
    println!("\n💡 涉及的话题:");
    let mut topic_counts: HashMap<String, u32> = HashMap::new();
    for entry in alice.history().iter().chain(bob.history().iter()) {
        if let Some(topic) = &entry.topic {
            *topic_counts.entry(topic.clone()).or_insert(0) += 1;
        }
    }
    let mut topics: Vec<_> = topic_counts.iter().collect();
    topics.sort_by(|a, b| b.1.cmp(a.1));
    for (topic, count) in topics {
        println!("   - {}: {} 条消息", topic, count);
    }

    print_header("✅ 多轮对话演示完成!");

    println!("\n🎉 两个 Bot 成功完成了 {} 轮对话!", dialogue_topics.len());
    println!("   Alice 发送了 {} 条消息", alice.stats().messages_sent);
    println!("   Bob 发送了 {} 条消息", bob.stats().messages_sent);
    println!("   总消息交换: {} 条\n", alice.stats().messages_sent + bob.stats().messages_sent);

    Ok(())
}
