//! A3Chat 真实消息订阅多 Bot 群聊演示程序
//!
//! 展示多个机器人通过共享的 NotificationBus 进行真实的异步消息传递。
//!
//! 运行方式:
//! ```bash
//! cargo run --example multi_bot_group_chat -p a3chat-app
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use a3chat_app::chat_service::ChatService;
use a3chat_app::notification_bus::{A3chatEvent, NotificationBus, NotificationReceiver};
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
}

/// Bot 个性
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BotPersonality {
    Friendly,
    Technical,
    Humorous,
    Curious,
}

/// 群聊 Bot
struct GroupBot {
    id: UserId,
    name: String,
    personality: BotPersonality,
    service: ChatService,
    stats: BotStats,
    reply_gen: Arc<tokio::sync::Mutex<SmartReplyGenerator>>,
    /// 消息接收任务句柄
    _recv_task: tokio::task::JoinHandle<()>,
}

impl GroupBot {
    /// 创建群聊 Bot
    async fn new(
        user_id: &str,
        display_name: &str,
        personality: BotPersonality,
        base_dir: &std::path::Path,
        shared_bus: &NotificationBus,
        message_tx: Arc<tokio::sync::broadcast::Sender<GroupChatMessage>>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let user_id = UserId::from(user_id.to_string());
        let keyring = a3chat_app::E2eKeyring::new(user_id.clone());
        let storage = ChatStorage::new(StorageConfig::new(base_dir.to_path_buf()), keyring);
        storage.init_user(&user_id).await?;

        let bus = shared_bus.clone();
        let service = ChatService::new(storage, bus);

        let reply_gen = Arc::new(tokio::sync::Mutex::new(SmartReplyGenerator::new(personality)));

        // 启动消息接收任务
        let my_id = user_id.clone();
        let my_name = display_name.to_string();
        let rx = service.bus().subscribe_for(user_id.clone());
        let tx = message_tx.clone();

        let recv_task = tokio::spawn(async move {
            Self::message_receiver(my_id, my_name, rx, tx).await;
        });

        Ok(Self {
            id: user_id,
            name: display_name.to_string(),
            personality,
            service,
            stats: BotStats::default(),
            reply_gen,
            _recv_task: recv_task,
        })
    }

    /// 消息接收协程
    async fn message_receiver(
        user_id: UserId,
        bot_name: String,
        mut rx: NotificationReceiver,
        tx: Arc<tokio::sync::broadcast::Sender<GroupChatMessage>>,
    ) {
        println!("  [{}] 消息接收任务已启动", bot_name);
        while let Some(event) = rx.recv().await {
            if let A3chatEvent::ChatMessageReceived { message, .. } = event {
                if message.sender_id == user_id {
                    continue;
                }

                let content = match &message.body {
                    MessageBody::Plain { content } => content.clone(),
                    _ => "[加密消息]".to_string(),
                };

                let _ = tx.send(GroupChatMessage {
                    from_id: message.sender_id,
                    from_name: bot_name.clone(),
                    content,
                    conversation_id: message.conversation_id,
                });
            }
        }
        println!("  [{}] 消息接收任务已结束", bot_name);
    }

    /// 发送消息
    async fn send_message(
        &mut self,
        conversation_id: &ConversationId,
        content: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.stats.messages_sent += 1;

        let envelope = MessageEnvelope {
            conversation_id: conversation_id.clone(),
            receiver_id: self.id.clone(),
            message_type: MessageType::Text,
            body: MessageBody::Plain {
                content: content.to_string(),
            },
            attachments: vec![],
            reply_to: None,
            sequence: self.stats.messages_sent,
            timestamp: chrono::Utc::now().timestamp(),
        };

        self.service.send_message(&self.id, &envelope).await?;
        Ok(())
    }

    /// 生成回复
    async fn generate_reply(&mut self, input: &str) -> String {
        let mut reply_gen = self.reply_gen.lock().await;
        reply_gen.generate(input, self.personality)
    }

    /// 模拟打字
    async fn simulate_typing(&mut self, content_len: usize) {
        let typing_time = (content_len as u64) * 15 + 50;
        tokio::time::sleep(Duration::from_millis(typing_time)).await;
    }

    fn id(&self) -> &UserId {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn stats(&self) -> &BotStats {
        &self.stats
    }
}

/// 群聊消息
#[derive(Debug, Clone)]
struct GroupChatMessage {
    from_id: UserId,
    from_name: String,
    content: String,
    conversation_id: ConversationId,
}

// ============================================================================
// 智能回复生成器
// ============================================================================

struct SmartReplyGenerator {
    replies: HashMap<BotPersonality, Vec<&'static str>>,
}

impl SmartReplyGenerator {
    fn new(personality: BotPersonality) -> Self {
        let mut replies = HashMap::new();

        replies.insert(BotPersonality::Friendly, vec![
            "太好了! 😊",
            "说得对!",
            "我同意!",
            "嗯嗯，继续说",
            "好的!",
        ]);

        replies.insert(BotPersonality::Technical, vec![
            "从技术角度看...",
            "这是个有趣的问题",
            "让我分析一下",
            "代码方面...",
            "技术上来说没问题",
        ]);

        replies.insert(BotPersonality::Humorous, vec![
            "哈哈，太有趣了! 😄",
            "笑死我了! 😂",
            "这太逗了! 🤣",
            "有意思! 😂",
        ]);

        replies.insert(BotPersonality::Curious, vec![
            "真的吗?",
            "为什么呢? 🤔",
            "有意思，能详细说说吗?",
            "我很好奇更多细节",
            "这让我想到了...",
        ]);

        // 填充所有个性的默认回复
        for p in [BotPersonality::Friendly, BotPersonality::Technical, BotPersonality::Humorous, BotPersonality::Curious] {
            replies.entry(p).or_insert(vec!["嗯", "好的", "明白"]);
        }

        Self { replies }
    }

    fn generate(&mut self, input: &str, personality: BotPersonality) -> String {
        let input_lower = input.to_lowercase();

        let replies = if input_lower.contains('?') || input_lower.contains("怎么") || input_lower.contains("什么") || input_lower.contains("为什么") {
            vec!["这是个有趣的问题!", "好问题!", "让我想想...", "有意思!"]
        } else if input_lower.contains("好") || input_lower.contains("对") || input_lower.contains("是") || input_lower.contains("没错") {
            vec!["没错!", "同意!", "说得对!", "好的!"]
        } else if input_lower.contains("哈哈") || input_lower.contains("笑") || input_lower.contains("有趣") {
            vec!["哈哈! 😄", "太逗了! 😂", "笑死我了! 🤣"]
        } else if input_lower.contains("技术") || input_lower.contains("rust") || input_lower.contains("代码") {
            vec!["从技术角度看...", "这是个有趣的技术问题!", "代码确实很棒!"]
        } else {
            self.replies.get(&personality).cloned().unwrap_or(vec!["嗯", "好的", "明白"])
        };

        replies[random_index(replies.len())].to_string()
    }
}

fn random_index(max: usize) -> usize {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    (nanos as usize) % max.max(1)
}

// ============================================================================
// 格式化打印
// ============================================================================

fn print_header(title: &str) {
    println!();
    println!("╔══════════════════════════════════════════════════════════════════════════╗");
    println!("║ {:^68} ║", title);
    println!("╚══════════════════════════════════════════════════════════════════════════╝");
}

fn print_message(speaker: &str, content: &str) {
    println!("  📢 [{}]: {}", speaker, content);
}

// ============================================================================
// 主程序
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    print_header("A3Chat 多 Bot 群聊演示 🔥");

    let base_dir = tempfile::tempdir()?;
    println!("\n📁 存储目录: {:?}", base_dir.path());

    let shared_bus = NotificationBus::new(256);
    println!("📡 共享消息总线已创建");

    let (message_tx, _rx) = tokio::sync::broadcast::channel::<GroupChatMessage>(100);

    println!("\n🤖 初始化群聊机器人...");

    let mut alice = GroupBot::new(
        "alice", "Alice", BotPersonality::Friendly,
        base_dir.path(), &shared_bus, Arc::new(message_tx.clone()),
    ).await?;

    let mut bob = GroupBot::new(
        "bob", "Bob", BotPersonality::Technical,
        base_dir.path(), &shared_bus, Arc::new(message_tx.clone()),
    ).await?;

    let mut charlie = GroupBot::new(
        "charlie", "Charlie", BotPersonality::Humorous,
        base_dir.path(), &shared_bus, Arc::new(message_tx.clone()),
    ).await?;

    let mut diana = GroupBot::new(
        "diana", "Diana", BotPersonality::Curious,
        base_dir.path(), &shared_bus, Arc::new(message_tx.clone()),
    ).await?;

    println!("   ✅ Alice (友善型)");
    println!("   ✅ Bob (技术型)");
    println!("   ✅ Charlie (幽默型)");
    println!("   ✅ Diana (好奇型)");

    let conversation_id = ConversationId::from("group:test-group-001");
    println!("\n👥 群组 ID: {}", conversation_id);

    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut message_rx = message_tx.subscribe();

    print_header("🌟 群聊开始");

    // 群聊话题
    let chat_topics: Vec<(&str, &str)> = vec![
        ("alice", "大家好!今天我们来聊聊技术话题吧!"),
        ("bob", "好的，我最近在研究 Rust 的异步编程"),
        ("charlie", "哈哈，Rust 很难学吗? 😂"),
        ("diana", "我很好奇 Rust 和 Go 比有什么优势?"),
        ("bob", "Rust 的内存安全保证是最吸引我的地方"),
        ("alice", "没错!而且 Rust 的类型系统非常强大"),
        ("charlie", "那 Rust 可以用来写 Web 开发吗?"),
        ("bob", "当然可以! Actix-web 和 Axum 都是很成熟的框架"),
        ("diana", "太好了!我也想学习 Rust"),
        ("alice", "一起学习吧! Rust 社区很友好"),
    ];

    for (i, topic) in chat_topics.iter().enumerate() {
        let round = i + 1;
        println!();
        println!("─── 第 {} 条消息 ───", round);

        let speaker_id = topic.0;
        let content = topic.1;

        // 根据发言者发送消息
        match speaker_id {
            "alice" => {
                print_message(&alice.name(), content);
                alice.send_message(&conversation_id, content).await?;
            }
            "bob" => {
                print_message(&bob.name(), content);
                bob.send_message(&conversation_id, content).await?;
            }
            "charlie" => {
                print_message(&charlie.name(), content);
                charlie.send_message(&conversation_id, content).await?;
            }
            "diana" => {
                print_message(&diana.name(), content);
                diana.send_message(&conversation_id, content).await?;
            }
            _ => {}
        }

        tokio::time::sleep(Duration::from_millis(100)).await;

        // 收集并显示收到的消息
        let mut replies_count = 0;
        while let Ok(msg) = message_rx.try_recv() {
            if replies_count < 2 {
                print_message(&msg.from_name, &msg.content);
                replies_count += 1;
            }
        }
    }

    // 打印统计
    print_header("📊 群聊统计");

    println!();
    println!("  📊 [Alice]: 发送 {} | 收到 {}", alice.stats().messages_sent, alice.stats().messages_received);
    println!("  📊 [Bob]: 发送 {} | 收到 {}", bob.stats().messages_sent, bob.stats().messages_received);
    println!("  📊 [Charlie]: 发送 {} | 收到 {}", charlie.stats().messages_sent, charlie.stats().messages_received);
    println!("  📊 [Diana]: 发送 {} | 收到 {}", diana.stats().messages_sent, diana.stats().messages_received);

    let total_sent = alice.stats().messages_sent + bob.stats().messages_sent + charlie.stats().messages_sent + diana.stats().messages_sent;
    println!("\n🎉 群聊演示完成! 共发送 {} 条消息", total_sent);

    tokio::time::sleep(Duration::from_millis(100)).await;

    Ok(())
}
