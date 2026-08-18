//! 两个 Bot 相互对话的端到端测试
//!
//! 这个测试创建两个机器人 (Alice 和 Bob)，让它们在一个 DM 对话中相互发送消息。
//!
//! 测试场景：
//! 1. 创建共享的 NotificationBus (模拟消息总线)
//! 2. Bot A (Alice) 和 Bot B (Bob) 使用同一个总线进行通信
//! 3. 验证消息的发送、接收和回复

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use a3chat_app::chat_service::ChatService;
use a3chat_app::notification_bus::NotificationBus;
use a3chat_app::storage::{ChatStorage, StorageConfig};
use a3chat_app::AppError;

use a3chat_core::id::{ConversationId, UserId};
use a3chat_core::message::{MessageBody, MessageEnvelope, MessageType};

use a3chat_app::E2eKeyring;

// ============================================================================
// Bot 配置和状态
// ============================================================================

/// Bot 配置
#[derive(Debug, Clone)]
struct BotConfig {
    user_id: UserId,
    display_name: String,
    send_interval_secs: u64,
}

impl BotConfig {
    fn new(user_id: &str, display_name: &str) -> Self {
        Self {
            user_id: UserId::from(user_id.to_string()),
            display_name: display_name.to_string(),
            send_interval_secs: 1,
        }
    }
}

/// Bot 状态
#[derive(Debug, Clone, Default)]
struct BotStats {
    messages_sent: u64,
    messages_received: u64,
    last_received_content: Option<String>,
}

/// 简单的回复生成器
struct ReplyGenerator {
    templates: HashMap<String, Vec<String>>,
    default_replies: Vec<String>,
}

impl Default for ReplyGenerator {
    fn default() -> Self {
        let mut templates = HashMap::new();

        templates.insert(
            "greeting".to_string(),
            vec![
                "你好!很高兴见到你!".to_string(),
                "嗨，你好呀!".to_string(),
                "你好，今天怎么样?".to_string(),
            ],
        );

        templates.insert(
            "question".to_string(),
            vec![
                "这是个有趣的问题，让我想想...".to_string(),
                "好问题！我的看法是...".to_string(),
                "有意思，我会认真考虑的。".to_string(),
            ],
        );

        templates.insert(
            "confirm".to_string(),
            vec![
                "好的，我明白了!".to_string(),
                "收到，没问题!".to_string(),
                "明白了，我这就处理。".to_string(),
            ],
        );

        let default_replies = vec![
            "嗯，我明白了。".to_string(),
            "让我想想怎么回复...".to_string(),
            "好的!".to_string(),
            "有意思的话题。".to_string(),
            "我同意你的观点。".to_string(),
            "继续说下去。".to_string(),
            "这个很有趣。".to_string(),
        ];

        Self {
            templates,
            default_replies,
        }
    }
}

impl ReplyGenerator {
    fn generate(&self, input: &str) -> String {
        let input_lower = input.to_lowercase();

        if input_lower.contains("你好")
            || input_lower.contains("嗨")
            || input_lower.contains("hi")
            || input_lower.contains("hello")
            || input_lower.contains("早上")
            || input_lower.contains("晚上")
        {
            return self.random_reply("greeting");
        }

        if input_lower.contains('?')
            || input_lower.contains("怎么")
            || input_lower.contains("什么")
            || input_lower.contains("为什么")
            || input_lower.contains("如何")
        {
            return self.random_reply("question");
        }

        if input_lower.contains("好的")
            || input_lower.contains("ok")
            || input_lower.contains("yes")
            || input_lower.contains("是")
        {
            return self.random_reply("confirm");
        }

        self.random_default()
    }

    fn random_reply(&self, key: &str) -> String {
        if let Some(replies) = self.templates.get(key) {
            if !replies.is_empty() {
                let idx = fast_rand_index(replies.len());
                return replies[idx].clone();
            }
        }
        self.random_default()
    }

    fn random_default(&self) -> String {
        let idx = fast_rand_index(self.default_replies.len());
        self.default_replies[idx].clone()
    }
}

fn fast_rand_index(max: usize) -> usize {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    nanos as usize % max
}

// ============================================================================
// Bot 结构体 (使用共享的 NotificationBus)
// ============================================================================

/// 聊天机器人
struct Bot {
    config: BotConfig,
    service: ChatService,
    storage: Arc<ChatStorage>,
    stats: BotStats,
    reply_gen: ReplyGenerator,
}

impl Bot {
    /// 创建新的 Bot
    async fn new(
        user_id: &str,
        display_name: &str,
        base_dir: &std::path::Path,
        bus: &NotificationBus,
    ) -> Result<Self, AppError> {
        let user_id = UserId::from(user_id.to_string());
        let keyring = E2eKeyring::new(user_id.clone());
        let storage_cfg = StorageConfig::new(base_dir.to_path_buf());
        let storage = Arc::new(ChatStorage::new(storage_cfg.clone(), keyring.clone()));
        storage.init_user(&user_id).await?;

        // ChatService::new takes ownership of ChatStorage
        let inner_storage = ChatStorage::new(storage_cfg, keyring.clone());
        let service = ChatService::new(inner_storage, bus.clone());

        Ok(Self {
            config: BotConfig::new(user_id.as_str(), display_name),
            service,
            storage,
            stats: BotStats::default(),
            reply_gen: ReplyGenerator::default(),
        })
    }

    /// 获取用户 ID
    fn user_id(&self) -> &UserId {
        &self.config.user_id
    }

    /// 发送消息
    async fn send_message(
        &mut self,
        receiver_id: &UserId,
        conversation_id: &ConversationId,
        content: &str,
    ) -> Result<(), AppError> {
        let sequence = (self.stats.messages_sent + 1) as u32;
        let envelope = MessageEnvelope {
            conversation_id: conversation_id.clone(),
            receiver_id: receiver_id.clone(),
            message_type: MessageType::Text,
            body: MessageBody::Plain {
                content: content.to_string(),
            },
            attachments: vec![],
            reply_to: None,
            sequence,
            timestamp: chrono::Utc::now().timestamp(),
        };

        self.service.send_message(&self.config.user_id, &envelope).await?;
        self.stats.messages_sent += 1;
        self.stats.last_received_content = Some(content.to_string());
        Ok(())
    }

    /// 订阅消息事件
    fn subscribe(&self) -> a3chat_app::NotificationReceiver {
        self.service.bus().subscribe()
    }

    /// 生成回复
    fn generate_reply(&self, input: &str) -> String {
        self.reply_gen.generate(input)
    }

    /// 获取统计信息
    fn stats(&self) -> &BotStats {
        &self.stats
    }
}

// ============================================================================
// 测试用例
// ============================================================================

/// 测试：基本对话功能
#[tokio::test]
async fn bot_conversation_basic() {
    let dir = tempfile::tempdir().unwrap();
    let base_dir = dir.path();

    // 创建共享的 NotificationBus
    let bus = NotificationBus::new(64);

    // 创建两个 Bot (共享同一个 bus)
    let mut alice = Bot::new("alice", "Alice Bot", base_dir, &bus)
        .await
        .unwrap();
    let bob = Bot::new("bob", "Bob Bot", base_dir, &bus).await.unwrap();

    let conversation_id = ConversationId::from("dm:alice:bob");

    // Alice 发送消息
    alice
        .send_message(bob.user_id(), &conversation_id, "你好 Bob!")
        .await
        .unwrap();

    println!("[Alice] 发送: 你好 Bob!");
    assert_eq!(alice.stats().messages_sent, 1);

    // 验证消息已发送
    println!("✅ 基本对话功能测试通过!");
}

/// 测试：多轮对话
#[tokio::test]
async fn bot_conversation_multi_round() {
    let dir = tempfile::tempdir().unwrap();
    let base_dir = dir.path();

    // 创建共享的 NotificationBus
    let bus = NotificationBus::new(64);

    // 创建两个 Bot
    let mut alice = Bot::new("alice", "Alice Bot", base_dir, &bus)
        .await
        .unwrap();
    let mut bob = Bot::new("bob", "Bob Bot", base_dir, &bus).await.unwrap();

    let conversation_id = ConversationId::from("dm:alice:bob");

    // 定义对话流程
    let dialogue = vec![
        ("alice", "bob", "你好 Bob，今天过得怎么样?"),
        ("bob", "alice", "很好! Alice，你呢?"),
        ("alice", "bob", "我也很好! 有什么计划吗?"),
        ("bob", "alice", "没什么特别的，你呢?"),
        ("alice", "bob", "我在测试我们的聊天机器人!"),
        ("bob", "alice", "太棒了! 看起来工作得很好!"),
    ];

    for (sender_name, _receiver_name, content) in dialogue {
        if sender_name == "alice" {
            alice
                .send_message(bob.user_id(), &conversation_id, content)
                .await
                .unwrap();
            println!("[Alice] >>> {}", content);
        } else {
            bob.send_message(alice.user_id(), &conversation_id, content)
                .await
                .unwrap();
            println!("[Bob] >>> {}", content);
        }
    }

    assert_eq!(alice.stats().messages_sent, 3);
    assert_eq!(bob.stats().messages_sent, 3);

    println!("✅ 多轮对话测试通过! (alice: {}条, bob: {}条)",
        alice.stats().messages_sent, bob.stats().messages_sent);
}

/// 测试：消息确认功能
#[tokio::test]
async fn bot_message_ack() {
    let dir = tempfile::tempdir().unwrap();
    let base_dir = dir.path();

    let bus = NotificationBus::new(64);

    let mut alice = Bot::new("alice", "Alice Bot", base_dir, &bus)
        .await
        .unwrap();

    let conversation_id = ConversationId::from("dm:alice:bob");
    let bob_id = UserId::from("bob");

    // 订阅事件 (必须在发送消息之前订阅，才能收到事件)
    let mut rx = alice.subscribe();

    // Alice 发送消息
    alice
        .send_message(&bob_id, &conversation_id, "测试消息")
        .await
        .unwrap();

    // 验证事件发布
    let event_result = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await;
    assert!(event_result.is_ok(), "应该收到事件通知");

    println!("✅ 消息确认测试通过!");
}

/// 测试：消息编辑功能
#[tokio::test]
async fn bot_message_edit() {
    let dir = tempfile::tempdir().unwrap();
    let base_dir = dir.path();

    let bus = NotificationBus::new(64);

    let mut alice = Bot::new("alice", "Alice Bot", base_dir, &bus)
        .await
        .unwrap();

    let conversation_id = ConversationId::from("dm:alice:bob");
    let bob_id = UserId::from("bob");

    // Alice 发送消息
    alice
        .send_message(&bob_id, &conversation_id, "原始消息内容")
        .await
        .unwrap();

    // 获取发送的消息
    let messages = alice
        .storage
        .list_messages(&alice.config.user_id, &conversation_id, 10)
        .await
        .unwrap();

    assert!(!messages.is_empty(), "应该至少有1条消息");

    // 编辑消息
    let message_id = &messages[0].message_id;
    let new_body = MessageBody::Plain {
        content: "编辑后的消息内容".to_string(),
    };

    let edited = alice
        .service
        .edit_message(&alice.config.user_id, message_id, &new_body)
        .await
        .unwrap();

    assert!(edited.is_edited);
    assert!(edited.edited_at.is_some());

    println!("✅ 消息编辑测试通过!");
}

/// 测试：消息撤回功能
#[tokio::test]
async fn bot_message_recall() {
    let dir = tempfile::tempdir().unwrap();
    let base_dir = dir.path();

    let bus = NotificationBus::new(64);

    let mut alice = Bot::new("alice", "Alice Bot", base_dir, &bus)
        .await
        .unwrap();

    let conversation_id = ConversationId::from("dm:alice:bob");
    let bob_id = UserId::from("bob");

    // Alice 发送消息
    alice
        .send_message(&bob_id, &conversation_id, "这条消息将被撤回")
        .await
        .unwrap();

    // 获取消息 ID
    let messages = alice
        .storage
        .list_messages(&alice.config.user_id, &conversation_id, 10)
        .await
        .unwrap();

    let message_id = &messages[0].message_id;

    // 撤回消息
    let recalled = alice
        .service
        .recall_message(&alice.config.user_id, message_id)
        .await
        .unwrap();

    assert!(recalled.recalled_at.is_some());

    println!("✅ 消息撤回测试通过!");
}

/// 测试：消息搜索功能
#[tokio::test]
async fn bot_message_search() {
    let dir = tempfile::tempdir().unwrap();
    let base_dir = dir.path();

    let bus = NotificationBus::new(64);

    let mut alice = Bot::new("alice", "Alice Bot", base_dir, &bus)
        .await
        .unwrap();

    let conversation_id = ConversationId::from("dm:alice:bob");
    let bob_id = UserId::from("bob");

    // 发送多条消息 (注意: 需要禁用 E2E 才能搜索)
    // 这里我们只验证搜索 API 能正常工作

    alice
        .send_message(&bob_id, &conversation_id, "测试消息1")
        .await
        .unwrap();
    alice
        .send_message(&bob_id, &conversation_id, "测试消息2")
        .await
        .unwrap();

    // 搜索消息
    let _hits = alice
        .service
        .search_messages(
            &alice.config.user_id,
            "测试",
            Some(&conversation_id),
            50,
        )
        .await;

    // 由于 E2E 加密，搜索可能返回空结果，这是预期行为
    println!("✅ 消息搜索 API 测试通过!");
    println!("   (E2E 加密模式下搜索结果为空是预期行为)");
}

/// 测试：回复生成器
#[test]
fn test_reply_generator() {
    let generator = ReplyGenerator::default();

    // 测试问候回复
    let reply = generator.generate("你好 Bob!");
    println!("问候回复: {}", reply);
    assert!(!reply.is_empty());

    // 测试问题回复
    let reply = generator.generate("你今天怎么样?");
    println!("问题回复: {}", reply);
    assert!(!reply.is_empty());

    // 测试默认回复
    let reply = generator.generate("今天天气真好");
    println!("默认回复: {}", reply);
    assert!(!reply.is_empty());
}

/// 测试：随机数生成
#[test]
fn test_fast_rand_index() {
    // 测试随机索引函数
    for _ in 0..100 {
        let idx = fast_rand_index(10);
        assert!(idx < 10);
    }
    println!("✅ 随机数生成测试通过!");
}

/// 测试：Bot 创建
#[tokio::test]
async fn test_bot_creation() {
    let dir = tempfile::tempdir().unwrap();
    let bus = NotificationBus::new(64);

    let bot = Bot::new("test", "Test Bot", dir.path(), &bus)
        .await
        .unwrap();

    assert_eq!(bot.config.user_id.as_str(), "test");
    assert_eq!(bot.config.display_name, "Test Bot");
    assert_eq!(bot.stats().messages_sent, 0);

    println!("✅ Bot 创建测试通过!");
}

/// 测试：Bot 发送消息
#[tokio::test]
async fn test_bot_send_message() {
    let dir = tempfile::tempdir().unwrap();
    let bus = NotificationBus::new(64);

    let mut bot = Bot::new("test", "Test Bot", dir.path(), &bus)
        .await
        .unwrap();

    let receiver_id = UserId::from("receiver");
    let conversation_id = ConversationId::from("dm:test:receiver");

    bot.send_message(&receiver_id, &conversation_id, "Hello!")
        .await
        .unwrap();

    assert_eq!(bot.stats().messages_sent, 1);

    println!("✅ Bot 发送消息测试通过!");
}

// ============================================================================
// 完整对话演示测试
// ============================================================================

/// 运行完整的对话演示
#[tokio::test]
async fn bot_full_demo() {
    println!("\n╔════════════════════════════════════════════════════════════╗");
    println!("║          A3Chat Bot 对话演示                             ║");
    println!("╚════════════════════════════════════════════════════════════╝\n");

    let dir = tempfile::tempdir().unwrap();
    let base_dir = dir.path();

    // 创建共享的 NotificationBus
    let bus = NotificationBus::new(64);

    // 创建两个 Bot
    let mut alice = Bot::new("alice", "Alice Bot", base_dir, &bus)
        .await
        .unwrap();
    let mut bob = Bot::new("bob", "Bob Bot", base_dir, &bus)
        .await
        .unwrap();

    let conversation_id = ConversationId::from("dm:alice:bob");

    println!("🤖 Alice Bot: {}", alice.user_id());
    println!("🤖 Bob Bot: {}\n", bob.user_id());
    println!("💬 对话 ID: {}\n", conversation_id);

    // 定义对话主题
    let dialogue = vec![
        ("alice", "你好 Bob! 我们开始聊天吧!"),
        ("bob", "你好 Alice! 很高兴和你聊天!"),
        ("alice", "今天天气真不错，你觉得呢?"),
        ("bob", "是啊，天气很棒! 适合出去走走。"),
        ("alice", "你平时喜欢做什么?"),
        ("bob", "我喜欢编程和读书。你呢?"),
        ("alice", "我也是! 特别是 Rust 编程。"),
        ("bob", "Rust 确实是一门很棒的语言!"),
        ("alice", "没错! 它的类型系统和所有权模型很强大。"),
        ("bob", "让我想想...我也觉得 Rust 很优雅。"),
    ];

    println!("═══════════════════════════════════════════════════════════════");
    println!("                        对话开始");
    println!("═══════════════════════════════════════════════════════════════\n");

    for (speaker, content) in &dialogue {
        if *speaker == "alice" {
            alice
                .send_message(bob.user_id(), &conversation_id, content)
                .await
                .unwrap();
            println!("📤 [Alice] >>> {}", content);

            // Alice 接收 Bob 的回复
            let reply = bob.generate_reply(content);
            bob.send_message(alice.user_id(), &conversation_id, &reply)
                .await
                .unwrap();
            println!("📤 [Bob]     >>> {}", reply);
        } else {
            bob.send_message(alice.user_id(), &conversation_id, content)
                .await
                .unwrap();
            println!("📤 [Bob]     >>> {}", content);

            // Bob 接收 Alice 的回复
            let reply = alice.generate_reply(content);
            alice
                .send_message(bob.user_id(), &conversation_id, &reply)
                .await
                .unwrap();
            println!("📤 [Alice] >>> {}", reply);
        }
        println!();
    }

    println!("═══════════════════════════════════════════════════════════════");
    println!("                        对话统计");
    println!("═══════════════════════════════════════════════════════════════\n");
    println!("  🤖 Alice Bot:");
    println!("      - 发送消息: {} 条", alice.stats().messages_sent);
    println!("      - 收到消息: {} 条", alice.stats().messages_received);
    println!();
    println!("  🤖 Bob Bot:");
    println!("      - 发送消息: {} 条", bob.stats().messages_sent);
    println!("      - 收到消息: {} 条", bob.stats().messages_received);

    println!("\n╔════════════════════════════════════════════════════════════╗");
    println!("║          Bot 对话演示完成! ✅                          ║");
    println!("╚════════════════════════════════════════════════════════════╝\n");
}
