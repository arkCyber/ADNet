//! A3Chat Bot 对话演示程序
//!
//! 这是一个完整的演示程序，展示两个聊天机器人 (Alice Bot 和 Bob Bot)
//! 如何在 A3Chat 应用中相互对话。
//!
//! 运行方式:
//! ```bash
//! cargo run --example bot_chat_demo
//! ```
//!
//! 或者运行测试:
//! ```bash
//! cargo test --test bot_conversation_demo
//! ```

use std::sync::Arc;
use std::time::Duration;

use a3chat_app::chat_service::ChatService;
use a3chat_app::notification_bus::NotificationBus;
use a3chat_app::storage::{ChatStorage, StorageConfig};

use a3chat_core::event::A3chatEvent;
use a3chat_core::id::{ConversationId, UserId};
use a3chat_core::message::{ChatMessage, MessageBody, MessageEnvelope, MessageType};

// ============================================================================
// Bot 定义
// ============================================================================

/// Bot 配置
struct Bot {
    id: UserId,
    name: String,
    service: ChatService,
    sequence: u32,
}

impl Bot {
    /// 创建新的 Bot
    async fn new(name: &str, base_dir: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
        let id = UserId::from(format!("{}-bot", name.to_lowercase()));
        let keyring = a3chat_app::E2eKeyring::new(id.clone());
        let storage = ChatStorage::new(StorageConfig::new(base_dir.to_path_buf()), keyring);
        storage.init_user(&id).await?;
        let bus = NotificationBus::new(64);
        let service = ChatService::new(storage, bus);

        Ok(Self {
            id,
            name: name.to_string(),
            service,
            sequence: 0,
        })
    }

    /// 发送消息
    async fn send_message(
        &mut self,
        receiver_id: &UserId,
        conversation_id: &ConversationId,
        content: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.sequence += 1;
        let envelope = MessageEnvelope {
            conversation_id: conversation_id.clone(),
            receiver_id: receiver_id.clone(),
            message_type: MessageType::Text,
            body: MessageBody::Plain {
                content: content.to_string(),
            },
            attachments: vec![],
            reply_to: None,
            sequence: self.sequence,
            timestamp: chrono::Utc::now().timestamp(),
        };

        self.service.send_message(&self.id, &envelope).await?;
        Ok(())
    }

    /// 订阅消息事件
    fn subscribe(&self) -> a3chat_app::NotificationReceiver {
        self.service.bus().subscribe()
    }
}

// ============================================================================
// 回复生成器
// ============================================================================

/// 智能回复生成器
struct SmartReplyGenerator {
    conversation_history: Vec<(String, String)>, // (speaker, message)
}

impl SmartReplyGenerator {
    fn new() -> Self {
        Self {
            conversation_history: Vec::new(),
        }
    }

    /// 添加对话历史
    fn add_message(&mut self, speaker: &str, message: &str) {
        self.conversation_history
            .push((speaker.to_string(), message.to_string()));
        // 保留最近 20 条消息
        if self.conversation_history.len() > 20 {
            self.conversation_history.remove(0);
        }
    }

    /// 生成回复
    fn generate_reply(&self, speaker: &str, input: &str) -> String {
        let input_lower = input.to_lowercase();

        // 检测输入类型
        if input_lower.contains("你好")
            || input_lower.contains("嗨")
            || input_lower.contains("hi")
            || input_lower.contains("hello")
        {
            let greetings = vec![
                "你好!很高兴见到你!",
                "嗨，你好呀!",
                "你好!今天怎么样?",
                "嗨! 最近好吗?",
            ];
            return greetings[random_index(greetings.len())].to_string();
        }

        if input_lower.contains("天气") {
            let weather_replies = vec![
                "今天天气很棒!",
                "希望天气一直这么好!",
                "天气确实不错，适合聊天!",
                "是啊，天气真好!",
            ];
            return weather_replies[random_index(weather_replies.len())].to_string();
        }

        if input_lower.contains("工作") || input_lower.contains("忙") {
            let work_replies = vec![
                "工作顺利吗?",
                "注意休息，别太累了!",
                "工作很重要，但也要照顾好自己!",
                "有什么我可以帮忙的吗?",
            ];
            return work_replies[random_index(work_replies.len())].to_string();
        }

        if input_lower.contains("编程") || input_lower.contains("代码") || input_lower.contains("rust") {
            let code_replies = vec![
                "Rust 是一门很棒的语言!",
                "编程确实很有趣!",
                "你最喜欢什么编程语言?",
                "代码就是艺术!",
            ];
            return code_replies[random_index(code_replies.len())].to_string();
        }

        if input_lower.contains('?')
            || input_lower.contains("怎么")
            || input_lower.contains("什么")
            || input_lower.contains("为什么")
        {
            let question_replies = vec![
                "这是个有趣的问题!",
                "让我想想...这需要一些考虑。",
                "好问题!我认为...",
                "有意思，我也经常思考这个问题。",
            ];
            return question_replies[random_index(question_replies.len())].to_string();
        }

        // 确认类
        if input_lower.contains("好的")
            || input_lower.contains("ok")
            || input_lower.contains("yes")
        {
            return "好的，明白了!".to_string();
        }

        // 检查对话历史，生成上下文相关的回复
        if self.conversation_history.len() > 2 {
            if let Some((_, last_msg)) = self.conversation_history.iter().rev().nth(1) {
                if last_msg.contains("天气") {
                    return "是啊，天气确实不错!".to_string();
                }
                if last_msg.contains("工作") || last_msg.contains("忙") {
                    return "记得照顾好自己!".to_string();
                }
                if last_msg.contains("编程") || last_msg.contains("代码") {
                    return "你平时喜欢写什么代码?".to_string();
                }
            }
        }

        // 默认回复
        let defaults = vec![
            "嗯，我明白了。",
            "有意思!",
            "继续说吧，我听着呢。",
            "好的!",
            "这个话题很有趣。",
            "我同意你的观点。",
            "让我想想...",
            "继续!",
        ];
        defaults[random_index(defaults.len())].to_string()
    }
}

fn random_index(max: usize) -> usize {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    ((nanos as usize) % max)
}

// ============================================================================
// 主程序
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n");
    println!("╔══════════════════════════════════════════════════════════════════╗");
    println!("║                                                                  ║");
    println!("║           🔥 A3Chat Bot 对话演示 🔥                              ║");
    println!("║                                                                  ║");
    println!("║     两个智能聊天机器人 Alice 和 Bob 的自动对话                   ║");
    println!("║                                                                  ║");
    println!("╚══════════════════════════════════════════════════════════════════╝");
    println!();

    // 创建临时目录用于存储
    let base_dir = tempfile::tempdir()?;
    println!("📁 存储目录: {:?}", base_dir.path());
    println!();

    // 创建两个 Bot
    println!("🤖 初始化机器人...");
    let mut alice = Bot::new("Alice", base_dir.path()).await?;
    let mut bob = Bot::new("Bob", base_dir.path()).await?;

    println!("   ✅ Alice Bot 创建完成 (ID: {})", alice.id);
    println!("   ✅ Bob Bot 创建完成 (ID: {})", bob.id);
    println!();

    // 创建对话
    let conversation_id = ConversationId::from(format!("dm:{}:{}", alice.id, bob.id));
    println!("💬 创建对话: {}", conversation_id);
    println!();

    // 订阅事件
    let mut alice_rx = alice.subscribe();
    let mut bob_rx = bob.subscribe();

    // 回复生成器
    let mut alice_replies = SmartReplyGenerator::new();
    let mut bob_replies = SmartReplyGenerator::new();

    // 对话主题
    let dialogue_topics = vec![
        ("你好 Bob!", "greeting"),
        ("今天的天气真不错!", "weather"),
        ("你最近工作忙吗?", "work"),
        ("我最近在学习 Rust 编程", "code"),
        ("你最喜欢什么编程语言?", "question"),
        ("Rust 确实很棒，它的类型系统很强大!", "code"),
        ("没错!而且 Rust 的内存安全保证让人很放心", "code"),
        ("是啊，我们换个话题吧。你周末有什么计划?", "question"),
        ("还没想好，可能会在家休息", "general"),
        ("好的，好好休息很重要!", "general"),
        ("我们继续聊天吧!", "general"),
        ("好的，你还想聊什么?", "question"),
        ("聊聊 AI 吧，你对 AI 怎么看?", "tech"),
        ("AI 真的很神奇，特别是大语言模型", "tech"),
    ];

    println!("═══════════════════════════════════════════════════════════════════");
    println!("                        🌟 开始对话 🌟");
    println!("═══════════════════════════════════════════════════════════════════");
    println!();

    let mut is_alice_turn = true;
    let mut topic_index = 0;

    // 运行多轮对话
    let max_rounds = 15;

    for round in 1..=max_rounds {
        println!("┌──────────────────────────────────────────────────────────────┐");
        println!("│                        第 {} 轮对话                            │", round);
        println!("└──────────────────────────────────────────────────────────────┘");

        if is_alice_turn {
            // Alice 发送消息
            let (content, topic) = if topic_index < dialogue_topics.len() {
                topic_index += 1;
                (&dialogue_topics[topic_index - 1])
            } else {
                // 超过预定义话题后，让 Alice 随机回复
                let last_bob_msg = bob_replies.conversation_history.last();
                if let Some((_, msg)) = last_bob_msg {
                    let reply = alice_replies.generate_reply("Bob", msg);
                    (Box::leak(reply.into_boxed_str()), "random")
                } else {
                    ("我们继续聊天吧!", "general")
                }
            };

            alice.send_message(&bob.id, &conversation_id, content).await?;
            alice_replies.add_message("Alice", content);
            println!("  📤 [Alice] >>> {}", content);
            println!("       📋 话题: {}", topic);

            // Alice 接收 Bob 的回复（如果有）
            if let Ok(Some(evt)) =
                tokio::time::timeout(Duration::from_millis(500), bob_rx.recv()).await
            {
                if let A3chatEvent::ChatMessageReceived { message, .. } = evt {
                    let content = message.body.preview();
                    alice_replies.add_message("Bob", &content);
                    println!("  📥 [Bob]     <<< {}", content);
                }
            }
        } else {
            // Bob 接收 Alice 的消息
            if let Ok(Some(evt)) =
                tokio::time::timeout(Duration::from_secs(1), bob_rx.recv()).await
            {
                if let A3chatEvent::ChatMessageReceived { message, .. } = evt {
                    let content = message.body.preview();
                    bob_replies.add_message("Alice", &content);

                    // 生成并发送回复
                    let reply = bob_replies.generate_reply("Alice", &content);
                    bob.send_message(&alice.id, &conversation_id, &reply).await?;
                    bob_replies.add_message("Bob", &reply);

                    println!("  📥 [Bob]     <<< {}", content);
                    println!("  📤 [Bob]     >>> {}", reply);

                    // Bob 也接收 Alice 的回复
                    if let Ok(Some(evt)) =
                        tokio::time::timeout(Duration::from_millis(500), alice_rx.recv()).await
                    {
                        if let A3chatEvent::ChatMessageReceived { message, .. } = evt {
                            let content = message.body.preview();
                            alice_replies.add_message("Bob", &content);
                            println!("  📥 [Alice]   <<< {}", content);
                        }
                    }
                }
            }
        }

        is_alice_turn = !is_alice_turn;
        println!();

        // 添加小延迟让输出更易读
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    // 展示对话统计
    println!("═══════════════════════════════════════════════════════════════════");
    println!("                        📊 对话统计");
    println!("═══════════════════════════════════════════════════════════════════");
    println!();
    println!("  🤖 Alice Bot:");
    println!("      - 用户 ID: {}", alice.id);
    println!("      - 发送消息数: {}", alice.sequence);
    println!("      - 对话历史长度: {} 条", alice_replies.conversation_history.len());
    println!();
    println!("  🤖 Bob Bot:");
    println!("      - 用户 ID: {}", bob.id);
    println!("      - 发送消息数: {}", bob.sequence);
    println!("      - 对话历史长度: {} 条", bob_replies.conversation_history.len());
    println!();

    // 验证对话完整性
    println!("═══════════════════════════════════════════════════════════════════");
    println!("                        ✅ 验证结果");
    println!("═══════════════════════════════════════════════════════════════════");
    println!();

    let mut all_passed = true;

    // 验证 Alice 发送了消息
    if alice.sequence > 0 {
        println!("  ✅ Alice 成功发送了 {} 条消息", alice.sequence);
    } else {
        println!("  ❌ Alice 没有发送任何消息");
        all_passed = false;
    }

    // 验证 Bob 发送了消息
    if bob.sequence > 0 {
        println!("  ✅ Bob 成功发送了 {} 条消息", bob.sequence);
    } else {
        println!("  ❌ Bob 没有发送任何消息");
        all_passed = false;
    }

    // 验证对话历史
    if alice_replies.conversation_history.len() >= 4 {
        println!(
            "  ✅ 对话历史完整 (共 {} 条消息)",
            alice_replies.conversation_history.len()
        );
    } else {
        println!(
            "  ⚠️  对话历史较短 (共 {} 条消息)",
            alice_replies.conversation_history.len()
        );
    }

    println!();

    if all_passed {
        println!("╔══════════════════════════════════════════════════════════════════╗");
        println!("║                                                                  ║");
        println!("║           🎉🎉🎉 所有测试通过! Bot 对话演示成功! 🎉🎉🎉            ║");
        println!("║                                                                  ║");
        println!("║     两个机器人 Alice 和 Bob 成功完成了一轮完整的对话              ║");
        println!("║                                                                  ║");
        println!("╚══════════════════════════════════════════════════════════════════╝");
    } else {
        println!("⚠️  部分测试未通过，请检查日志");
    }

    println!();
    Ok(())
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_smart_reply_generator_greeting() {
        let generator = SmartReplyGenerator::new();
        let reply = generator.generate_reply("Alice", "你好 Bob!");
        assert!(reply.contains("你好") || reply.contains("见到你"));
    }

    #[test]
    fn test_smart_reply_generator_weather() {
        let generator = SmartReplyGenerator::new();
        let reply = generator.generate_reply("Alice", "今天天气真好!");
        assert!(reply.contains("天气") || reply.contains("不错"));
    }

    #[test]
    fn test_smart_reply_generator_code() {
        let generator = SmartReplyGenerator::new();
        let reply = generator.generate_reply("Alice", "我在学习 Rust 编程");
        assert!(
            reply.contains("Rust")
                || reply.contains("编程")
                || reply.contains("语言")
                || reply.contains("代码")
        );
    }

    #[tokio::test]
    async fn test_bot_creation() {
        let dir = tempfile::tempdir().unwrap();
        let bot = Bot::new("TestBot", dir.path()).await;
        assert!(bot.is_ok());
    }

    #[tokio::test]
    async fn test_bot_send_message() {
        let dir = tempfile::tempdir().unwrap();

        // 创建两个 bot
        let mut alice = Bot::new("Alice", dir.path()).await.unwrap();
        let bob = Bot::new("Bob", dir.path()).await.unwrap();

        let conversation_id = ConversationId::from("dm:test:bot");

        // Alice 发送消息
        let result = alice
            .send_message(&bob.id, &conversation_id, "Hello Bob!")
            .await;
        assert!(result.is_ok());
        assert_eq!(alice.sequence, 1);
    }
}
