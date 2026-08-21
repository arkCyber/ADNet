//! A3Chat 朋友圈功能演示程序
//!
//! 展示朋友圈发帖、评论、点赞、关注等核心功能。
//!
//! 运行方式:
//! ```bash
//! cargo run --example moments_demo -p a3chat-app
//! ```
//!
//! 功能特性:
//! - 发布朋友圈动态
//! - 查看用户动态列表
//! - 添加评论
//! - 点赞/取消点赞
//! - 关注/取消关注用户
//! - 获取时间线

use a3chat_app::moments_service::MomentsService;
use a3chat_app::moments_service::MomentsConfig;

use a3net_socialfeed::TimelineQuery;
use a3net_types::invariants::{Visibility, ReactionType, ReactionTarget};
use a3net_types::social_feed::{SocialPost, SocialComment, SocialReaction};

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

fn print_post(post: &SocialPost) {
    println!("  📝 动态 ID: {}", post.post_id);
    println!("     作者: {}", post.author_name);
    println!("     内容: {}", post.content);
    println!("     可见性: {:?}", post.visibility);
    println!("     点赞数: {}", post.like_count);
    println!("     评论数: {}", post.comment_count);
}

// ============================================================================
// 主程序
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    print_header("A3Chat 朋友圈功能演示 🔥");

    // 创建临时目录用于存储
    let base_dir = tempfile::tempdir()?;
    println!("\n📁 存储目录: {:?}", base_dir.path());

    // 创建共享的 MomentsService
    let moments_dir = base_dir.path().join("moments");
    let moments_cfg = MomentsConfig {
        data_dir: moments_dir,
    };
    let moments_service = MomentsService::open(&moments_cfg)?;
    print_success("共享 Moments Service 已初始化".to_string());

    // ========================================================================
    // 初始化用户
    // ========================================================================
    print_section("初始化用户");
    println!();

    let alice_id = UserId::from("alice");
    let bob_id = UserId::from("bob");
    let charlie_id = UserId::from("charlie");
    print_success(format!("Alice 用户已创建 (ID: {})", alice_id));
    print_success(format!("Bob 用户已创建 (ID: {})", bob_id));
    print_success(format!("Charlie 用户已创建 (ID: {})", charlie_id));

    // ========================================================================
    // 1. 发布朋友圈动态
    // ========================================================================
    print_section("1. 发布朋友圈动态");

    // Alice 发布一条动态
    let alice_post = {
        let post = SocialPost {
            post_id: String::new(),
            author_id: alice_id.to_string(),
            author_name: "Alice".to_string(),
            author_avatar: None,
            content: "今天学习了 Rust 编程语言，感觉非常有趣！🚀".to_string(),
            attachments: vec![],
            tags: vec![],
            visibility: Visibility::Public,
            location: None,
            mentions: vec![],
            created_at: 0,
            updated_at: 0,
            like_count: 0,
            comment_count: 0,
            share_count: 0,
            public_account_id: None,
            integrity_hash: None,
            sequence: 1,
            is_edited: false,
            edited_at: None,
        };
        moments_service.create_post(&alice_id, post).await?
    };
    print_success("Alice 发布了朋友圈动态".to_string());
    print_post(&alice_post);

    // Bob 发布一条动态
    let bob_post = {
        let post = SocialPost {
            post_id: String::new(),
            author_id: bob_id.to_string(),
            author_name: "Bob".to_string(),
            author_avatar: None,
            content: "周末去爬山，空气真好！🏔️".to_string(),
            attachments: vec![],
            tags: vec![],
            visibility: Visibility::Public,
            location: None,
            mentions: vec![],
            created_at: 0,
            updated_at: 0,
            like_count: 0,
            comment_count: 0,
            share_count: 0,
            public_account_id: None,
            integrity_hash: None,
            sequence: 1,
            is_edited: false,
            edited_at: None,
        };
        moments_service.create_post(&bob_id, post).await?
    };
    print_success("Bob 发布了朋友圈动态".to_string());
    print_post(&bob_post);

    // Charlie 发布一条动态
    let charlie_post = {
        let post = SocialPost {
            post_id: String::new(),
            author_id: charlie_id.to_string(),
            author_name: "Charlie".to_string(),
            author_avatar: None,
            content: "新项目已经上线，欢迎大家试用！🎉".to_string(),
            attachments: vec![],
            tags: vec![],
            visibility: Visibility::Public,
            location: None,
            mentions: vec![],
            created_at: 0,
            updated_at: 0,
            like_count: 0,
            comment_count: 0,
            share_count: 0,
            public_account_id: None,
            integrity_hash: None,
            sequence: 1,
            is_edited: false,
            edited_at: None,
        };
        moments_service.create_post(&charlie_id, post).await?
    };
    print_success("Charlie 发布了朋友圈动态".to_string());
    print_post(&charlie_post);

    // ========================================================================
    // 2. 查看用户动态列表
    // ========================================================================
    print_section("2. 查看用户动态列表");

    let alice_posts = moments_service.list_user_posts(alice_id.as_str())?;
    print_info("Alice 动态数量", alice_posts.len().to_string());

    let bob_posts = moments_service.list_user_posts(bob_id.as_str())?;
    print_info("Bob 动态数量", bob_posts.len().to_string());

    // ========================================================================
    // 3. 添加评论
    // ========================================================================
    print_section("3. 添加评论");

    // Bob 评论 Alice 的动态
    let bob_comment = {
        let comment = SocialComment {
            comment_id: String::new(),
            post_id: alice_post.post_id.clone(),
            author_id: bob_id.to_string(),
            author_name: "Bob".to_string(),
            author_avatar: None,
            content: "Rust 确实很棒！学习曲线有点陡，但值得。".to_string(),
            parent_id: None,
            mentions: vec![],
            created_at: 0,
            updated_at: 0,
            like_count: 0,
            reply_count: 0,
            is_edited: false,
            edited_at: None,
        };
        moments_service.comment_post(&bob_id, comment).await?
    };
    print_success(format!("Bob 评论了 Alice 的动态 (评论ID: {})", bob_comment.comment_id));

    // Charlie 评论 Alice 的动态
    let charlie_comment = {
        let comment = SocialComment {
            comment_id: String::new(),
            post_id: alice_post.post_id.clone(),
            author_id: charlie_id.to_string(),
            author_name: "Charlie".to_string(),
            author_avatar: None,
            content: "同感！我也在学习 Rust。".to_string(),
            parent_id: None,
            mentions: vec![],
            created_at: 0,
            updated_at: 0,
            like_count: 0,
            reply_count: 0,
            is_edited: false,
            edited_at: None,
        };
        moments_service.comment_post(&charlie_id, comment).await?
    };
    print_success(format!("Charlie 评论了 Alice 的动态 (评论ID: {})", charlie_comment.comment_id));

    // Alice 评论 Bob 的动态
    let alice_comment = {
        let comment = SocialComment {
            comment_id: String::new(),
            post_id: bob_post.post_id.clone(),
            author_id: alice_id.to_string(),
            author_name: "Alice".to_string(),
            author_avatar: None,
            content: "爬山要注意安全哦！".to_string(),
            parent_id: None,
            mentions: vec![],
            created_at: 0,
            updated_at: 0,
            like_count: 0,
            reply_count: 0,
            is_edited: false,
            edited_at: None,
        };
        moments_service.comment_post(&alice_id, comment).await?
    };
    print_success(format!("Alice 评论了 Bob 的动态 (评论ID: {})", alice_comment.comment_id));

    // 查看评论列表
    let alice_post_comments = moments_service.list_post_comments(&alice_post.post_id)?;
    print_info("Alice 动态评论数", alice_post_comments.len().to_string());
    for comment in &alice_post_comments {
        println!("  💬 {}: {}", comment.author_name, comment.content);
    }

    // ========================================================================
    // 4. 点赞功能
    // ========================================================================
    print_section("4. 点赞功能");

    // Alice 点赞 Bob 的动态
    let alice_liked = {
        let reaction = SocialReaction {
            reaction_id: String::new(),
            target_id: bob_post.post_id.clone(),
            target_type: ReactionTarget::Post,
            user_id: alice_id.to_string(),
            reaction_type: ReactionType::Like,
            created_at: 0,
        };
        moments_service.react(&alice_id, reaction).await?
    };
    print_success(format!("Alice {}了 Bob 的动态", if alice_liked { "点赞" } else { "取消点赞" }));

    // Charlie 点赞 Bob 的动态
    let charlie_liked = {
        let reaction = SocialReaction {
            reaction_id: String::new(),
            target_id: bob_post.post_id.clone(),
            target_type: ReactionTarget::Post,
            user_id: charlie_id.to_string(),
            reaction_type: ReactionType::Like,
            created_at: 0,
        };
        moments_service.react(&charlie_id, reaction).await?
    };
    print_success(format!("Charlie {}了 Bob 的动态", if charlie_liked { "点赞" } else { "取消点赞" }));

    // Charlie 点赞 Alice 的动态
    let charlie_liked_alice = {
        let reaction = SocialReaction {
            reaction_id: String::new(),
            target_id: alice_post.post_id.clone(),
            target_type: ReactionTarget::Post,
            user_id: charlie_id.to_string(),
            reaction_type: ReactionType::Like,
            created_at: 0,
        };
        moments_service.react(&charlie_id, reaction).await?
    };
    print_success(format!("Charlie {}了 Alice 的动态", if charlie_liked_alice { "点赞" } else { "取消点赞" }));

    // 查看点赞列表
    let bob_post_reactions = moments_service.list_reactions(&bob_post.post_id)?;
    print_info("Bob 动态点赞数", bob_post_reactions.len().to_string());
    for reaction in &bob_post_reactions {
        println!("  ❤️ {} 赞了这个动态", reaction.user_id);
    }

    // ========================================================================
    // 5. 关注功能
    // ========================================================================
    print_section("5. 关注功能");

    // Alice 关注 Bob
    moments_service.follow(alice_id.as_str(), bob_id.as_str())?;
    print_success(format!("Alice 关注了 Bob"));

    // Alice 关注 Charlie
    moments_service.follow(alice_id.as_str(), charlie_id.as_str())?;
    print_success(format!("Alice 关注了 Charlie"));

    // Bob 关注 Alice
    moments_service.follow(bob_id.as_str(), alice_id.as_str())?;
    print_success(format!("Bob 关注了 Alice"));

    // 查看关注列表
    let alice_following = moments_service.list_following(alice_id.as_str())?;
    print_info("Alice 关注列表", alice_following.join(", "));

    let bob_following = moments_service.list_following(bob_id.as_str())?;
    print_info("Bob 关注列表", bob_following.join(", "));

    // 检查关注关系
    let alice_follows_bob = moments_service.is_following(alice_id.as_str(), bob_id.as_str())?;
    print_info("Alice 是否关注 Bob", if alice_follows_bob { "是" } else { "否" }.to_string());

    // ========================================================================
    // 6. 取消关注
    // ========================================================================
    print_section("6. 取消关注");

    // Alice 取消关注 Charlie
    moments_service.unfollow(alice_id.as_str(), charlie_id.as_str())?;
    print_success(format!("Alice 取消关注了 Charlie"));

    let alice_following_after = moments_service.list_following(alice_id.as_str())?;
    print_info("Alice 关注列表 (取消后)", alice_following_after.join(", "));

    // ========================================================================
    // 7. 获取时间线
    // ========================================================================
    print_section("7. 获取时间线");

    let timeline_query = TimelineQuery {
        viewer_id: alice_id.to_string(),
        scope: a3net_socialfeed::TimelineScope::ForViewer,
        limit: Some(10),
        before_cursor: None,
        before_ts: None,
        author_id: None,
    };

    let timeline = moments_service.timeline(timeline_query)?;
    print_info("时间线动态数量", timeline.posts.len().to_string());

    for post in &timeline.posts {
        println!("  📝 {}: {}", post.author_name, post.content);
    }

    // ========================================================================
    // 8. 节点信息
    // ========================================================================
    print_section("8. 节点信息");

    let node_info = moments_service.node_info();
    print_info("节点 ID", node_info.node_id);
    print_info("Schema 版本", node_info.schema_version.to_string());

    // ========================================================================
    // 9. 完整性验证
    // ========================================================================
    print_section("9. 完整性验证");

    let alice_post_valid = moments_service.verify_post_integrity(&alice_post);
    print_info("Alice 动态完整性", if alice_post_valid { "有效" } else { "无效" }.to_string());

    let bob_comment_valid = moments_service.verify_comment_integrity(&bob_comment);
    print_info("Bob 评论完整性", if bob_comment_valid { "有效" } else { "无效" }.to_string());

    // ========================================================================
    // 完成
    // ========================================================================
    print_header("✅ 朋友圈功能演示完成!");

    println!();
    println!("📊 功能演示总结:");
    println!("  • 发布动态: 3 条");
    println!("  • 添加评论: 3 条");
    println!("  • 点赞操作: 3 次");
    println!("  • 关注操作: 3 次");
    println!("  • 取消关注: 1 次");
    println!("  • 完整性验证: 2 次");
    println!();

    Ok(())
}
