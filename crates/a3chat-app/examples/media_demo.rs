//! A3Chat 媒体文件上传下载演示程序
//!
//! 展示媒体文件的上传、分块传输、下载、完整性验证等核心功能。
//!
//! 运行方式:
//! ```bash
//! cargo run --example media_demo -p a3chat-app
//! ```
//!
//! 功能特性:
//! - 初始化上传会话
//! - 分块上传数据
//! - 完成上传并获取内容哈希
//! - 下载媒体文件
//! - 健康状态检查
//! - 多种 MIME 类型支持

use a3chat_app::media_service::{MediaService, MediaConfig};

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

// ============================================================================
// 主程序
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    print_header("A3Chat 媒体文件上传下载演示 🔥");

    // 创建临时目录用于存储
    let base_dir = tempfile::tempdir()?;
    println!("\n📁 存储目录: {:?}", base_dir.path());

    // ========================================================================
    // 初始化 Media Service
    // ========================================================================
    print_section("初始化 Media Service");
    println!();

    // 使用本地存储配置 (禁用分布式和纠删码，用于演示)
    let media_dir = base_dir.path().join("media");
    let media_cfg = MediaConfig::local_only_under_base(&media_dir);
    let media_service = MediaService::open(&media_cfg)?;
    print_success("Media Service 已初始化 (本地模式)".to_string());

    let owner = UserId::new("demo-user");
    print_info("所有者", owner.as_str().to_string());

    // ========================================================================
    // 1. 健康状态检查
    // ========================================================================
    print_section("1. 健康状态检查");

    let health = media_service.health();
    println!("  🏥 健康状态:");
    println!("     存储健康: {}", health.store_healthy);
    println!("     数据目录: {}", health.data_dir);
    println!("     最大附件大小: {} bytes", health.max_attachment_bytes);
    println!("     最大分块大小: {} bytes", health.max_chunk_bytes);
    println!("     iroh 启用: {}", health.iroh_enabled);
    println!("     纠删码启用: {}", health.ec_enabled);
    println!("     加密启用: {}", health.encryption_enabled);
    println!("     写入策略: {:?}", health.write_policy);
    println!("     复制因子: {}", health.replication_factor);

    // ========================================================================
    // 2. 上传纯文本文件
    // ========================================================================
    print_section("2. 上传纯文本文件");

    // 初始化上传
    let text_token = media_service
        .upload_init(owner.clone(), Some("text/plain".to_string()))
        .await?;
    print_success("上传会话已初始化".to_string());
    print_info("Token", text_token.clone());

    // 分块上传 (文本文件较小，一次上传)
    let text_content = b"Hello, A3Chat Media Service!\nThis is a test file.".to_vec();
    let chunk_result = media_service
        .upload_chunk(owner.clone(), &text_token, text_content.clone())
        .await?;
    print_success("分块上传成功".to_string());
    print_info("已接收字节", chunk_result.bytes_received.to_string());
    print_info("最大字节", chunk_result.max_bytes.to_string());

    // 完成上传
    let text_finalized = media_service
        .upload_finalize(owner.clone(), &text_token, Some("hello.txt".to_string()))
        .await?;
    print_success("文件上传完成".to_string());
    print_info("内容哈希 (BLAKE3)", text_finalized.hash.clone());
    print_info("文件大小", format!("{} bytes", text_finalized.size));
    print_info("文件名", text_finalized.filename.as_ref().unwrap_or(&"无".to_string()).to_string());

    let text_hash = text_finalized.hash.clone();

    // ========================================================================
    // 3. 上传图片文件 (模拟)
    // ========================================================================
    print_section("3. 上传图片文件 (模拟)");

    // 初始化上传
    let image_token = media_service
        .upload_init(owner.clone(), Some("image/png".to_string()))
        .await?;
    print_success("图片上传会话已初始化".to_string());

    // 模拟图片数据 (PNG 文件头 + 模拟数据)
    let mut image_data = Vec::new();
    // PNG 文件头
    image_data.extend_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
    // 模拟 IHDR chunk (13 bytes)
    image_data.extend_from_slice(&[0x00, 0x00, 0x00, 0x0D]); // length
    image_data.extend_from_slice(b"IHDR");
    image_data.extend_from_slice(&[0x00, 0x00, 0x01, 0x00]); // width 256
    image_data.extend_from_slice(&[0x00, 0x00, 0x01, 0x00]); // height 256
    image_data.extend_from_slice(&[0x08, 0x02, 0x00, 0x00, 0x00]); // bit depth, color type, etc.
    image_data.extend_from_slice(&[0xD3, 0x10, 0x3F, 0x31]); // CRC
    // 模拟 IDAT chunk
    let idat_data = b"A3Chat demo image data - this is a simulated PNG file for testing purposes.";
    image_data.extend_from_slice(&(idat_data.len() as u32).to_be_bytes());
    image_data.extend_from_slice(b"IDAT");
    image_data.extend_from_slice(idat_data);

    let image_chunk = media_service
        .upload_chunk(owner.clone(), &image_token, image_data)
        .await?;
    print_success("图片分块上传成功".to_string());
    print_info("已接收字节", image_chunk.bytes_received.to_string());

    let image_finalized = media_service
        .upload_finalize(owner.clone(), &image_token, Some("demo.png".to_string()))
        .await?;
    print_success("图片上传完成".to_string());
    print_info("内容哈希", image_finalized.hash.clone());
    print_info("文件大小", format!("{} bytes", image_finalized.size));

    let image_hash = image_finalized.hash.clone();

    // ========================================================================
    // 4. 上传音频文件 (模拟)
    // ========================================================================
    print_section("4. 上传音频文件 (模拟)");

    let audio_token = media_service
        .upload_init(owner.clone(), Some("audio/mpeg".to_string()))
        .await?;

    // 模拟 MP3 文件头
    let mut audio_data = Vec::new();
    // MP3 ID3v2 头
    audio_data.extend_from_slice(b"ID3");
    audio_data.extend_from_slice(&[0x04, 0x00, 0x00]); // version
    audio_data.extend_from_slice(&[0x00]); // flags
    audio_data.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // size
    // 模拟音频数据
    audio_data.extend_from_slice(b"A3Chat demo audio data - this is a simulated MP3 file.");

    media_service
        .upload_chunk(owner.clone(), &audio_token, audio_data)
        .await?;

    let audio_finalized = media_service
        .upload_finalize(owner.clone(), &audio_token, Some("demo.mp3".to_string()))
        .await?;
    print_success("音频文件上传完成".to_string());
    print_info("内容哈希", audio_finalized.hash);
    print_info("文件名", audio_finalized.filename.as_ref().unwrap().to_string());

    // ========================================================================
    // 5. 上传视频文件 (模拟)
    // ========================================================================
    print_section("5. 上传视频文件 (模拟)");

    let video_token = media_service
        .upload_init(owner.clone(), Some("video/mp4".to_string()))
        .await?;

    // 模拟 MP4 文件头 (ftyp box)
    let mut video_data = Vec::new();
    // ftyp box
    video_data.extend_from_slice(&[0x00, 0x00, 0x00, 0x20]); // box size
    video_data.extend_from_slice(b"ftyp"); // box type
    video_data.extend_from_slice(b"isom"); // major brand
    video_data.extend_from_slice(&[0x00, 0x00, 0x02, 0x00]); // minor version
    video_data.extend_from_slice(b"isomiso2mp41"); // compatible brands
    // 模拟视频数据
    video_data.extend_from_slice(b"A3Chat demo video data - this is a simulated MP4 file.");

    media_service
        .upload_chunk(owner.clone(), &video_token, video_data)
        .await?;

    let video_finalized = media_service
        .upload_finalize(owner.clone(), &video_token, Some("demo.mp4".to_string()))
        .await?;
    print_success("视频文件上传完成".to_string());
    print_info("内容哈希", video_finalized.hash);

    // ========================================================================
    // 6. 下载文件
    // ========================================================================
    print_section("6. 下载文件");

    // 下载文本文件
    let downloaded_text = media_service
        .download_get(owner.clone(), &text_hash)
        .await?;
    print_success("文本文件下载成功".to_string());
    print_info("哈希", downloaded_text.hash);
    print_info("大小", format!("{} bytes", downloaded_text.size));
    // 将 hex 转换回文本
    let text_hex = downloaded_text.data_hex.clone();
    let decoded_text = hex::decode(&text_hex)?;
    let text_str = String::from_utf8_lossy(&decoded_text);
    print_info("内容", text_str.to_string());

    // 下载图片文件
    let downloaded_image = media_service
        .download_get(owner.clone(), &image_hash)
        .await?;
    print_success("图片文件下载成功".to_string());
    print_info("哈希", downloaded_image.hash);
    print_info("大小", format!("{} bytes", downloaded_image.size));
    // 验证 PNG 头
    let image_hex = &downloaded_image.data_hex;
    let image_start = &image_hex[..16]; // PNG header is 8 bytes, hex = 16 chars
    print_info("文件头 (hex)", image_start.to_string());

    // ========================================================================
    // 7. 尝试下载不存在的文件
    // ========================================================================
    print_section("7. 错误处理 - 下载不存在的文件");

    let fake_hash = "00".repeat(32); // 32 bytes = 64 hex chars
    match media_service.download_get(owner.clone(), &fake_hash).await {
        Ok(_) => println!("  ❌ 意外成功"),
        Err(e) => {
            print_success("正确处理了不存在的文件".to_string());
            println!("  错误: {}", e);
        }
    }

    // ========================================================================
    // 8. 分块大小限制测试
    // ========================================================================
    print_section("8. 分块大小限制测试");

    // 创建一个新的媒体服务，使用较小的分块大小
    let small_chunk_dir = base_dir.path().join("small_chunk");
    let mut small_cfg = MediaConfig::local_only_under_base(&small_chunk_dir);
    small_cfg.max_chunk_bytes = 100; // 限制为 100 bytes

    let small_media = MediaService::open(&small_cfg)?;
    let small_token = small_media.upload_init(owner.clone(), None).await?;

    // 尝试上传超过限制的数据
    let large_chunk = vec![0u8; 200];
    match small_media.upload_chunk(owner.clone(), &small_token, large_chunk).await {
        Ok(_) => println!("  ❌ 意外成功"),
        Err(e) => {
            print_success("正确拒绝了超大分块".to_string());
            println!("  错误: {}", e);
        }
    }

    // ========================================================================
    // 9. 元数据查询
    // ========================================================================
    print_section("9. 元数据查询");

    let text_meta = media_service.lookup_meta(&text_hash);
    if let Some(meta) = text_meta {
        println!("  📋 文本文件元数据:");
        println!("     哈希: {}", meta.hash);
        println!("     文件名: {:?}", meta.filename);
        println!("     MIME 类型: {:?}", meta.mime_type);
        println!("     所有者: {}", meta.owner);
        println!("     完成时间: {}", meta.finalized_at_unix);
        print_success("元数据查询成功".to_string());
    } else {
        println!("  ❌ 未找到元数据");
    }

    // ========================================================================
    // 10. 最终健康状态
    // ========================================================================
    print_section("10. 最终健康状态");

    let final_health = media_service.health();
    println!("  🏥 最终健康状态:");
    println!("     分布式写入尝试: {}", final_health.distributed_writes_attempted);
    println!("     分布式写入成功: {}", final_health.distributed_writes_succeeded);
    println!("     分布式写入失败: {}", final_health.distributed_writes_failed);
    println!("  📋 SR 标签:");
    for tag in &final_health.sr_tags {
        println!("     - {}", tag);
    }

    // ========================================================================
    // 完成
    // ========================================================================
    print_header("✅ 媒体文件上传下载演示完成!");

    println!();
    println!("📊 功能演示总结:");
    println!("  • 上传文件: 4 个");
    println!("     - 文本文件 (text/plain)");
    println!("     - 图片文件 (image/png)");
    println!("     - 音频文件 (audio/mpeg)");
    println!("     - 视频文件 (video/mp4)");
    println!("  • 下载文件: 2 个");
    println!("  • 健康检查: 2 次");
    println!("  • 错误处理测试: 2 次");
    println!("  • 元数据查询: 1 次");
    println!();

    Ok(())
}
