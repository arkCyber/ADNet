//! A3Chat 群聊功能演示程序
//!
//! 展示群组创建、成员管理、角色变更、公告设置等核心功能。
//!
//! 运行方式:
//! ```bash
//! cargo run --example group_chat_demo -p a3chat-app
//! ```
//!
//! 功能特性:
//! - 创建群组并设置基本信息
//! - 添加和移除群组成员
//! - 角色变更 (Member -> Admin -> Owner)
//! - 设置群公告
//! - 获取群组成员列表

use std::sync::Arc;

use a3chat_app::notification_bus::NotificationBus;
use a3chat_app::storage::{ChatStorage, StorageConfig};
use a3chat_app::group_service::GroupService;
use a3chat_app::group_service_types::CreateGroupRequest;
use a3chat_app::chat_service::ChatService;

use a3chat_core::id::UserId;
use a3chat_core::message::{MessageBody, MessageEnvelope, MessageType};

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

// ============================================================================
// 主程序
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    print_header("A3Chat 群聊功能演示 🔥".to_string());

    // 创建临时目录用于存储
    let base_dir = tempfile::tempdir()?;
    println!("\n📁 存储目录: {:?}", base_dir.path());

    // 创建共享的 ImManager (群组信息的权威存储)
    let hub_path = base_dir.path().join("hub.sqlite");
    let im_manager = a3net_chatstore::ImManager::new(hub_path)?;

    // 在 ImManager 中创建用户
    let alice_hub_user = im_manager.create_user("alice", "Alice").await?;
    let alice_uid = UserId::from(alice_hub_user.id.clone());
    let bob_hub_user = im_manager.create_user("bob", "Bob").await?;
    let bob_uid = UserId::from(bob_hub_user.id.clone());
    let charlie_hub_user = im_manager.create_user("charlie", "Charlie").await?;
    let charlie_uid = UserId::from(charlie_hub_user.id.clone());

    let im_manager = Arc::new(im_manager); // 包装成 Arc
    println!("📡 ImManager 已创建并注册用户");

    // ========================================================================
    // 初始化用户存储
    // ========================================================================
    print_section("初始化用户".to_string());
    println!();

    // 为每个用户创建独立的 ChatStorage
    let alice_storage = ChatStorage::new(
        StorageConfig::new(base_dir.path().join("alice").to_path_buf()),
        a3chat_app::E2eKeyring::new(alice_uid.clone()),
    );
    alice_storage.init_user(&alice_uid).await?;
    print_success(format!("Alice 用户已初始化 (ID: {})", alice_uid));

    let bob_storage = ChatStorage::new(
        StorageConfig::new(base_dir.path().join("bob").to_path_buf()),
        a3chat_app::E2eKeyring::new(bob_uid.clone()),
    );
    bob_storage.init_user(&bob_uid).await?;
    print_success(format!("Bob 用户已初始化 (ID: {})", bob_uid));

    let charlie_storage = ChatStorage::new(
        StorageConfig::new(base_dir.path().join("charlie").to_path_buf()),
        a3chat_app::E2eKeyring::new(charlie_uid.clone()),
    );
    charlie_storage.init_user(&charlie_uid).await?;
    print_success(format!("Charlie 用户已初始化 (ID: {})", charlie_uid));

    // ========================================================================
    // 创建 GroupService 实例并注入 hub
    // ========================================================================
    let alice_bus = NotificationBus::new(64);
    let bob_bus = NotificationBus::new(64);
    let charlie_bus = NotificationBus::new(64);

    let alice_group = Arc::new(GroupService::new(alice_bus.clone()))
        .with_storage(Arc::new(alice_storage.clone()))
        .with_hub(im_manager.clone());

    let bob_group = Arc::new(GroupService::new(bob_bus.clone()))
        .with_storage(Arc::new(bob_storage.clone()))
        .with_hub(im_manager.clone());

    let charlie_group = Arc::new(GroupService::new(charlie_bus.clone()))
        .with_storage(Arc::new(charlie_storage.clone()))
        .with_hub(im_manager.clone());

    // ========================================================================
    // 1. 创建群组
    // ========================================================================
    print_section("1. 创建群组".to_string());

    let create_req = CreateGroupRequest {
        name: "Rust 开发交流群".to_string(),
        description: "欢迎 Rust 开发者加入交流".to_string(),
        avatar_url: None,
        is_private: false,
    };

    let create_resp = alice_group.create(&alice_uid, create_req).await?;

    print_success(format!("群组已创建: {}", create_resp.group.name));
    print_info("群组ID", create_resp.group.conversation_id.as_str().to_string());
    print_info("群主", create_resp.group.owner_id.as_str().to_string());
    print_info("成员数", create_resp.group.member_count.to_string());
    print_info("私群", if create_resp.group.is_private { "是" } else { "否" }.to_string());

    let group_id = create_resp.group.conversation_id.clone();

    // ========================================================================
    // 2. 添加成员
    // ========================================================================
    print_section("2. 添加成员".to_string());

    // Alice 添加 Bob 为成员
    print_info("添加 Bob", "正在添加 Bob 到群组...".to_string());
    let bob_member = alice_group.add_member(&alice_uid, &group_id, &bob_uid).await?;
    print_success(format!("Bob 已加入群组 (角色: {:?})", bob_member.role));

    // Alice 添加 Charlie 为成员
    print_info("添加 Charlie", "正在添加 Charlie 到群组...".to_string());
    let charlie_member = alice_group.add_member(&alice_uid, &group_id, &charlie_uid).await?;
    print_success(format!("Charlie 已加入群组 (角色: {:?})", charlie_member.role));

    // ========================================================================
    // 3. 获取成员列表
    // ========================================================================
    print_section("3. 获取成员列表".to_string());

    let members = alice_group.list_members(&group_id).await?;
    print_info("成员数量", members.len().to_string());
    println!();
    for member in &members {
        println!("  👤 {} ({:?})", member.display_name, member.role);
    }

    // ========================================================================
    // 4. 角色变更
    // ========================================================================
    print_section("4. 角色变更".to_string());

    // Alice 将 Bob 提升为管理员
    print_info("提升 Bob", "正在将 Bob 从 Member 提升为 Admin...".to_string());
    let bob_updated = alice_group
        .set_role(&alice_uid, &group_id, &bob_uid, a3chat_core::group::MemberRole::Admin)
        .await?;
    print_success(format!("Bob 已成为 Admin (角色: {:?})", bob_updated.role));

    // ========================================================================
    // 5. 设置群公告
    // ========================================================================
    print_section("5. 设置群公告".to_string());

    let announcement = "🎉 欢迎大家加入 Rust 开发交流群！\n请遵守群规，积极交流。";
    print_info("公告内容", announcement.to_string());
    alice_group
        .set_announcement(&alice_uid, &group_id, announcement.to_string())
        .await?;
    print_success("群公告已设置".to_string());

    // ========================================================================
    // 6. 群内发送消息
    // ========================================================================
    print_section("6. 群内发送消息".to_string());

    // Alice 发送群消息
    let alice_envelope = MessageEnvelope {
        conversation_id: group_id.clone(),
        receiver_id: bob_uid.clone(),
        message_type: MessageType::Text,
        body: MessageBody::Plain {
            content: "大家好，欢迎来到 Rust 开发交流群!".to_string(),
        },
        attachments: vec![],
        reply_to: None,
        sequence: 1,
        timestamp: chrono::Utc::now().timestamp(),
    };

    let alice_chat_service = ChatService::new(alice_storage.clone(), alice_bus.clone());
    alice_chat_service.send_message(&alice_uid, &alice_envelope).await?;
    print_success("Alice 发送了群消息".to_string());

    // ========================================================================
    // 7. 转让群主
    // ========================================================================
    print_section("7. 转让群主".to_string());

    print_info("当前群主", "Alice".to_string());
    print_info("新群主", "Bob".to_string());

    alice_group
        .transfer_ownership(&alice_uid, &group_id, &bob_uid)
        .await?;
    print_success("群主已转让给 Bob".to_string());

    // 验证新群主
    let new_owner = alice_group.get_member(&group_id, &bob_uid).await?;
    print_info("Bob 新角色", format!("{:?}", new_owner.role));

    // ========================================================================
    // 8. 移除成员
    // ========================================================================
    print_section("8. 移除成员".to_string());

    // 新群主 Bob 移除 Charlie
    print_info("操作者", "Bob".to_string());
    print_info("目标", "Charlie".to_string());

    bob_group
        .remove_member(&bob_uid, &group_id, &charlie_uid)
        .await?;
    print_success("Charlie 已被移出群组".to_string());

    // 验证成员列表
    let remaining_members = bob_group.list_members(&group_id).await?;
    print_info("剩余成员数量", remaining_members.len().to_string());

    // ========================================================================
    // 9. 列出所有群组
    // ========================================================================
    print_section("9. 列出我的群组".to_string());

    let groups = bob_group.list(&bob_uid).await?;
    print_info("群组数量", groups.len().to_string());
    for group in &groups {
        println!("  📢 {} (ID: {})", group.name, group.conversation_id.as_str());
    }

    // ========================================================================
    // 完成
    // ========================================================================
    print_header("✅ 群聊功能演示完成!".to_string());

    println!();
    println!("📊 功能演示总结:");
    println!("  • 创建群组: 1 个");
    println!("  • 添加成员: 2 人");
    println!("  • 角色变更: 1 次");
    println!("  • 设置公告: 1 次");
    println!("  • 转让群主: 1 次");
    println!("  • 移除成员: 1 人");
    println!();

    Ok(())
}
