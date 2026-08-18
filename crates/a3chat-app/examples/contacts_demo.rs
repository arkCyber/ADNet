//! A3Chat 联系人管理功能演示程序
//!
//! 展示联系人请求、黑名单、二维码邀请等核心功能。
//!
//! 运行方式:
//! ```bash
//! cargo run --example contacts_demo -p a3chat-app
//! ```
//!
//! 功能特性:
//! - 查看联系人列表
//! - 发送好友请求
//! - 拉黑/解除拉黑用户
//! - 生成二维码邀请链接
//! - 联系人事件通知

use std::sync::Arc;

use a3chat_app::notification_bus::NotificationBus;
use a3chat_app::contact_service::ContactService;
use a3chat_app::contact_service::ContactServiceConfig;

use a3chat_core::id::UserId;

// ============================================================================
// 辅助函数
// ============================================================================

fn print_header(title: &str) {
    println!();
    println!("╔═══════════════════════════════════════════════════════════════════╗");
    println!("║ {:^63} ║", title);
    println!("╚═══════════════════════════════════════════════════════════════════╝");
}

fn print_section(title: &str) {
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

// ============================================================================
// 用户结构体
// ============================================================================

struct ContactUser {
    id: UserId,
    name: String,
    contact_service: ContactService,
    _subscriber: a3chat_app::NotificationReceiver,
}

impl ContactUser {
    async fn new(
        user_id: &str,
        display_name: &str,
        base_dir: &std::path::Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let id = UserId::from(user_id.to_string());
        let bus = NotificationBus::new(64);
        let contact_config = ContactServiceConfig::under_base(base_dir);
        let contact_service = ContactService::new_unowned(contact_config);
        let subscriber = bus.subscribe();

        Ok(Self {
            id,
            name: display_name.to_string(),
            contact_service,
            _subscriber: subscriber,
        })
    }

    fn id(&self) -> &UserId {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }
}

// ============================================================================
// 主程序
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    print_header("A3Chat 联系人管理功能演示 🔥");

    // 创建临时目录
    let base_dir = tempfile::tempdir()?;
    println!("\n📁 存储目录: {:?}", base_dir.path());

    // ========================================================================
    // 初始化用户
    // ========================================================================
    print_section("初始化用户");
    println!();

    let alice = ContactUser::new("alice", "Alice", base_dir.path()).await?;
    print_success(format!("Alice 用户已创建 (ID: {})", alice.id()));

    let bob = ContactUser::new("bob", "Bob", base_dir.path()).await?;
    print_success(format!("Bob 用户已创建 (ID: {})", bob.id()));

    let charlie = ContactUser::new("charlie", "Charlie", base_dir.path()).await?;
    print_success(format!("Charlie 用户已创建 (ID: {})", charlie.id()));

    // ========================================================================
    // 1. 查看联系人列表
    // ========================================================================
    print_section("1. 查看联系人列表");

    let alice_contacts = alice.contact_service.list(&alice.id).await?;
    print_info("联系人数量", alice_contacts.contacts.len().to_string());
    print_info("黑名单数量", alice_contacts.blocklist.len().to_string());

    if alice_contacts.contacts.is_empty() {
        println!("  (暂无联系人)");
    }
    for contact in &alice_contacts.contacts {
        println!("  👤 {} ({})", contact.display_name, contact.user_id);
    }

    // ========================================================================
    // 2. 发送好友请求
    // ========================================================================
    print_section("2. 发送好友请求");

    // Alice 发送好友请求给 Bob
    let alice_to_bob = alice.contact_service
        .add_request(&alice.id, bob.id(), "你好 Bob，我是 Alice！想加你为好友。".to_string(), None)
        .await?;
    print_success(format!("Alice 发送好友请求给 {} (请求ID: {})", bob.name(), alice_to_bob.request_id));
    print_info("请求状态", format!("{:?}", alice_to_bob.status));
    print_info("附言", alice_to_bob.message);

    // Alice 发送好友请求给 Charlie
    let alice_to_charlie = alice.contact_service
        .add_request(&alice.id, charlie.id(), "嗨 Charlie，我是 Alice。".to_string(), None)
        .await?;
    print_success(format!("Alice 发送好友请求给 {} (请求ID: {})", charlie.name(), alice_to_charlie.request_id));

    // Bob 发送好友请求给 Charlie
    let bob_to_charlie = bob.contact_service
        .add_request(&bob.id, charlie.id(), "你好 Charlie，我是 Bob！".to_string(), None)
        .await?;
    print_success(format!("Bob 发送好友请求给 {} (请求ID: {})", charlie.name(), bob_to_charlie.request_id));

    // ========================================================================
    // 3. 拉黑用户
    // ========================================================================
    print_section("3. 拉黑用户");

    // Alice 拉黑某个用户 (假设是 spam_user)
    let spam_user_id = UserId::from("spam_user");
    let alice_blocked = alice.contact_service
        .block(&alice.id, &spam_user_id)
        .await?;
    print_success(format!("Alice 拉黑了 {} (拉黑时间: {})", spam_user_id, alice_blocked.blocked_at));

    // 验证黑名单
    let alice_contacts_after = alice.contact_service.list(&alice.id).await?;
    print_info("黑名单数量 (拉黑后)", alice_contacts_after.blocklist.len().to_string());
    for entry in &alice_contacts_after.blocklist {
        println!("  🚫 {} (拉黑时间: {})", entry.display_name, entry.blocked_at);
    }

    // ========================================================================
    // 4. 解除拉黑
    // ========================================================================
    print_section("4. 解除拉黑");

    alice.contact_service.unblock(&alice.id, &spam_user_id).await?;
    print_success(format!("Alice 解除拉黑了 {}", spam_user_id));

    let alice_contacts_unblocked = alice.contact_service.list(&alice.id).await?;
    print_info("黑名单数量 (解除后)", alice_contacts_unblocked.blocklist.len().to_string());

    // ========================================================================
    // 5. 生成二维码邀请链接
    // ========================================================================
    print_section("5. 生成二维码邀请链接");

    // Alice 生成邀请链接
    let alice_qr = alice.contact_service.qr_invite(&alice.id).await?;
    print_success("Alice 生成了二维码邀请链接".to_string());
    // 显示邀请链接的前50个字符
    let qr_preview = if alice_qr.len() > 50 {
        format!("{}...", &alice_qr[..50])
    } else {
        alice_qr.clone()
    };
    print_info("邀请链接 (预览)", qr_preview);

    // Bob 生成邀请链接
    let bob_qr = bob.contact_service.qr_invite(&bob.id).await?;
    print_success("Bob 生成了二维码邀请链接".to_string());

    // Charlie 生成邀请链接
    let charlie_qr = charlie.contact_service.qr_invite(&charlie.id).await?;
    print_success("Charlie 生成了二维码邀请链接".to_string());

    // ========================================================================
    // 6. 通过 RPC Dispatcher 调用
    // ========================================================================
    print_section("6. 通过 RPC Dispatcher 调用");

    // 使用 dispatch 方法
    let alice_contacts_via_dispatch = a3chat_app::contact_service::dispatch(
        Arc::new(alice.contact_service.clone()),
        a3chat_core::rpc::A3chatRpcMethod::CONTACT_LIST,
        &alice.id,
        serde_json::json!({}),
    ).await?;
    print_success("通过 RPC Dispatcher 获取联系人列表".to_string());
    println!("  响应: {}", serde_json::to_string_pretty(&alice_contacts_via_dispatch)?);

    // ========================================================================
    // 7. 联系人快照结构
    // ========================================================================
    print_section("7. 联系人快照结构");

    let snapshot = alice.contact_service.list(&alice.id).await?;
    println!("  📋 联系人快照:");
    println!("     contacts: {:?}", snapshot.contacts);
    println!("     blocklist: {:?}", snapshot.blocklist);

    // ========================================================================
    // 完成
    // ========================================================================
    print_header("✅ 联系人管理功能演示完成!");

    println!();
    println!("📊 功能演示总结:");
    println!("  • 查看联系人列表: 3 次");
    println!("  • 发送好友请求: 3 次");
    println!("  • 拉黑用户: 1 次");
    println!("  • 解除拉黑: 1 次");
    println!("  • 生成邀请链接: 3 次");
    println!("  • RPC Dispatcher 调用: 1 次");
    println!();

    Ok(())
}
