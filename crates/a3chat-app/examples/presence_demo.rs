//! A3Chat 在线状态服务演示程序
//!
//! 展示用户在线状态发布、订阅、管理等核心功能。
//!
//! 运行方式:
//! ```bash
//! cargo run --example presence_demo -p a3chat-app
//! ```
//!
//! 功能特性:
//! - 发布用户在线状态
//! - 订阅好友状态
//! - 获取批量用户状态
//! - 状态事件通知
//! - 多种状态类型支持 (在线、离线、忙碌、离开等)

use std::sync::Arc;
use std::time::Duration;

use a3chat_app::notification_bus::NotificationBus;
use a3chat_app::storage::{ChatStorage, StorageConfig};
use a3chat_app::presence_service::PresenceService;

use a3chat_core::presence::{PresenceStatus, Presence};
use a3chat_core::id::UserId;

// ============================================================================
// 辅助函数
// ============================================================================

fn print_header(title: String) {
    println!();
    println!("╔═══════════════════════════════════════════════════════════════════╗");
    println!("║ {:^63} ║", title);
    println!("╚═══════════════════════════════════════════════════════════════════╝");
}

fn print_section(title: String) {
    println!();
    println!("┌─────────────────────────────────────────────────────────────────┐");
    println!("│ {:^63} │", title);
    println!("└─────────────────────────────────────────────────────────────────┘");
}

fn print_success(msg: String) {
    println!("  ✅ {}", msg);
}

fn print_info(label: &str, msg: String) {
    println!("  📌 {}: {}", label, msg);
}

fn print_separator() {
    println!("─────────────────────────────────────────────────────────────────────");
}

fn print_presence(p: &Presence) {
    println!("  👤 用户: {}", p.user_id);
    println!("     状态: {:?}", p.status);
    if let Some(ref msg) = p.status_message {
        println!("     状态消息: {}", msg);
    }
    println!("     最后变更: {}", p.last_changed);
}

// ============================================================================
// 用户结构体
// ============================================================================

struct PresenceUser {
    id: UserId,
    name: String,
    storage: ChatStorage,
    presence_service: PresenceService,
    _subscriber: a3chat_app::NotificationReceiver,
}

impl PresenceUser {
    async fn new(
        user_id: &str,
        display_name: &str,
        base_dir: &std::path::Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let id = UserId::from(user_id.to_string());
        let keyring = a3chat_app::E2eKeyring::new(id.clone());
        let storage = ChatStorage::new(
            StorageConfig::new(base_dir.to_path_buf()),
            keyring,
        );
        storage.init_user(&id).await?;

        let bus = NotificationBus::new(64);
        let presence_service = PresenceService::new(storage.clone(), bus.clone());
        let subscriber = bus.subscribe();

        Ok(Self {
            id,
            name: display_name.to_string(),
            storage,
            presence_service,
            _subscriber: subscriber,
        })
    }

    fn id(&self) -> &UserId {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    async fn set_online(&self, status_message: Option<&str>) -> Result<Presence, Box<dyn std::error::Error>> {
        let msg = status_message.map(|s| s.to_string());
        let p = self.presence_service
            .publish(&self.id, PresenceStatus::Online, msg)
            .await?;
        Ok(p)
    }

    async fn set_offline(&self) -> Result<Presence, Box<dyn std::error::Error>> {
        let p = self.presence_service
            .publish(&self.id, PresenceStatus::Offline, None)
            .await?;
        Ok(p)
    }

    async fn set_away(&self, status_message: Option<&str>) -> Result<Presence, Box<dyn std::error::Error>> {
        let msg = status_message.map(|s| s.to_string());
        let p = self.presence_service
            .publish(&self.id, PresenceStatus::Away, msg)
            .await?;
        Ok(p)
    }
}

// ============================================================================
// 主程序
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    print_header("A3Chat 在线状态服务演示 🔥".to_string());

    // 创建临时目录用于存储
    let base_dir = tempfile::tempdir()?;
    println!("\n📁 存储目录: {:?}", base_dir.path());

    // ========================================================================
    // 初始化用户
    // ========================================================================
    print_section("初始化用户".to_string());
    println!();

    let alice = PresenceUser::new("alice", "Alice", base_dir.path()).await?;
    print_success(format!("Alice 用户已创建 (ID: {})", alice.id()));

    let bob = PresenceUser::new("bob", "Bob", base_dir.path()).await?;
    print_success(format!("Bob 用户已创建 (ID: {})", bob.id()));

    let charlie = PresenceUser::new("charlie", "Charlie", base_dir.path()).await?;
    print_success(format!("Charlie 用户已创建 (ID: {})", charlie.id()));

    // ========================================================================
    // 1. 发布在线状态
    // ========================================================================
    print_section("1. 发布在线状态".to_string());

    // Alice 上线
    let alice_online = alice.set_online(Some("我在线，可以聊天！")).await?;
    print_success(format!("Alice 设置为在线"));
    print_presence(&alice_online);

    // Bob 上线
    let bob_online = bob.set_online(Some("忙碌中")).await?;
    print_success(format!("Bob 设置为在线"));
    print_presence(&bob_online);

    // Charlie 上线
    let charlie_online = charlie.set_online(Some("正在开会")).await?;
    print_success(format!("Charlie 设置为在线"));
    print_presence(&charlie_online);

    // ========================================================================
    // 2. 发布其他状态
    // ========================================================================
    print_section("2. 发布其他状态".to_string());

    // Alice 离开
    let alice_away = alice.set_away(Some("暂时离开")).await?;
    print_success(format!("Alice 设置为离开"));
    print_info("状态", format!("{:?}", alice_away.status));
    print_info("状态消息", alice_away.status_message.as_ref().unwrap_or(&"无".to_string()).to_string());

    // Bob 隐身 (使用 Invisible 作为忙碌的替代)
    let bob_invisible = bob.presence_service
        .publish(&bob.id, PresenceStatus::Invisible, Some("正在写代码".to_string()))
        .await?;
    print_success(format!("Bob 设置为隐身"));
    print_info("状态", format!("{:?}", bob_invisible.status));

    // Alice 回来
    let alice_back = alice.set_online(Some("回来了！")).await?;
    print_success(format!("Alice 回来了"));
    print_presence(&alice_back);

    // ========================================================================
    // 3. 订阅好友状态
    // ========================================================================
    print_section("3. 订阅好友状态".to_string());

    // Alice 订阅 Bob 和 Charlie 的状态
    let peers_for_alice = vec![bob.id().clone(), charlie.id().clone()];
    let alice_sees = alice.presence_service
        .subscribe(&alice.id, &peers_for_alice)
        .await?;

    print_info("Alice 看到的状态数量", alice_sees.len().to_string());
    for presence in &alice_sees {
        println!();
        println!("  👀 Alice 看到 {} 的状态:", presence.user_id);
        println!("     状态: {:?}", presence.status);
        if let Some(ref msg) = presence.status_message {
            println!("     消息: {}", msg);
        }
    }

    // Bob 订阅 Alice 的状态
    let peers_for_bob = vec![alice.id().clone()];
    let bob_sees = bob.presence_service
        .subscribe(&bob.id, &peers_for_bob)
        .await?;

    print_info("Bob 看到的状态数量", bob_sees.len().to_string());
    for presence in &bob_sees {
        println!();
        println!("  👀 Bob 看到 {} 的状态:", presence.user_id);
        println!("     状态: {:?}", presence.status);
    }

    // ========================================================================
    // 4. 查询不存在的用户状态
    // ========================================================================
    print_section("4. 查询不存在的用户状态".to_string());

    let unknown_id = UserId::from("unknown_user");
    let peers_with_unknown = vec![alice.id().clone(), unknown_id.clone()];
    let mixed_sees = alice.presence_service
        .subscribe(&alice.id, &peers_with_unknown)
        .await?;

    print_info("返回的状态数量", mixed_sees.len().to_string());
    for presence in &mixed_sees {
        println!();
        println!("  👤 {}: {:?}", presence.user_id, presence.status);
        if presence.status == PresenceStatus::Offline && presence.user_id == unknown_id {
            println!("     (不存在的用户显示为离线)");
        }
    }

    // ========================================================================
    // 5. 用户下线
    // ========================================================================
    print_section("5. 用户下线".to_string());

    charlie.set_offline().await?;
    print_success(format!("Charlie 下线了"));

    // Alice 再次查看
    let alice_sees_after_charlie_offline = alice.presence_service
        .subscribe(&alice.id, &peers_for_alice)
        .await?;

    print_info("Charlie 下线后的状态:", "".to_string());
    for presence in &alice_sees_after_charlie_offline {
        if presence.user_id == *charlie.id() {
            println!("  👀 Charlie 状态: {:?}", presence.status);
        }
    }

    // ========================================================================
    // 6. 通过 RPC Dispatcher 调用
    // ========================================================================
    print_section("6. 通过 RPC Dispatcher 调用".to_string());

    // 使用 dispatch 方法发布状态
    let dispatch_result = a3chat_app::presence_service::dispatch(
        Arc::new(alice.presence_service.clone()),
        a3chat_core::rpc::A3chatRpcMethod::PRESENCE_PUBLISH,
        &alice.id,
        serde_json::json!({
            "status": "online",
            "status_message": "正在通过 RPC 演示"
        }),
    ).await?;

    print_success("通过 RPC Dispatcher 发布状态".to_string());
    println!("  响应: {}", serde_json::to_string_pretty(&dispatch_result)?);

    // 使用 dispatch 方法订阅状态
    let dispatch_subscribe = a3chat_app::presence_service::dispatch(
        Arc::new(bob.presence_service.clone()),
        a3chat_core::rpc::A3chatRpcMethod::PRESENCE_SUBSCRIBE,
        &bob.id,
        serde_json::json!({
            "peers": [alice.id().as_str(), charlie.id().as_str()]
        }),
    ).await?;

    print_success("通过 RPC Dispatcher 订阅状态".to_string());
    println!("  响应: {}", serde_json::to_string_pretty(&dispatch_subscribe)?);

    // ========================================================================
    // 7. 状态类型枚举
    // ========================================================================
    print_section("7. 状态类型枚举".to_string());

    println!("  📋 支持的状态类型:");
    println!("     Online - 在线");
    println!("     Offline - 离线");
    println!("     Away - 离开");
    println!("     Invisible - 隐身");

    // 测试状态解析
    print_separator();
    println!("  🔍 状态解析测试:");

    let test_statuses = vec![
        ("online", PresenceStatus::Online),
        ("offline", PresenceStatus::Offline),
        ("away", PresenceStatus::Away),
        ("invisible", PresenceStatus::Invisible),
    ];

    for (name, status) in test_statuses {
        let parsed = PresenceStatus::parse(name);
        if let Some(parsed_status) = parsed {
            let match_result = if parsed_status == status {
                "✓"
            } else {
                "✗"
            };
            println!("     {} '{}' -> {:?}", match_result, name, parsed_status);
        } else {
            println!("     ✗ '{}' 解析失败", name);
        }
    }

    // ========================================================================
    // 8. 错误处理 - 状态消息过长
    // ========================================================================
    print_section("8. 错误处理 - 状态消息过长".to_string());

    let long_message = "x".repeat(257); // 超过 256 字符限制
    match alice.presence_service
        .publish(&alice.id, PresenceStatus::Online, Some(long_message))
        .await
    {
        Ok(_) => println!("  ❌ 意外成功"),
        Err(e) => {
            print_success("正确拒绝了过长的状态消息".to_string());
            println!("  错误: {}", e);
        }
    }

    // ========================================================================
    // 完成
    // ========================================================================
    print_header("✅ 在线状态服务演示完成!".to_string());

    println!();
    println!("📊 功能演示总结:");
    println!("  • 发布状态: 7 次");
    println!("     - Alice: 3 次 (online -> away -> online)");
    println!("     - Bob: 2 次 (online -> invisible)");
    println!("     - Charlie: 2 次 (online -> offline)");
    println!("  • 订阅状态: 4 次");
    println!("  • RPC Dispatcher 调用: 2 次");
    println!("  • 状态解析测试: 5 次");
    println!("  • 错误处理测试: 1 次");
    println!();

    Ok(())
}
