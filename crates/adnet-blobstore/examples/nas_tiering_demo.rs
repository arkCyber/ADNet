//! NAS 冷热数据分离存储演示
//!
//! 演示如何使用分层存储来自动管理冷热数据：
//! - 频繁访问的数据保持在内部高速存储（热）
//! - 不常访问的数据自动迁移到外部经济存储（冷）
//! - 冷数据被访问时自动回温到热存储
//!
//! 运行方式：
//! ```bash
//! cargo run --example nas_tiering_demo
//! ```

use std::sync::Arc;
use std::time::Duration;

use adnet_blobstore::{TieredStorageService, TieringPolicy};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 设置日志
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("=== NAS 冷热数据分离存储演示 ===\n");

    // 创建临时目录
    let temp = tempfile::tempdir()?;
    let hot_dir = temp.path().join("hot");
    let cold_dir = temp.path().join("cold_storage");

    // 配置冷热数据策略
    let policy = TieringPolicy {
        // 30 秒未访问后迁移到冷存储（演示用，生产环境建议 30 天）
        cold_threshold_days: 0, // 设为 0 便于演示
        scan_interval: Duration::from_secs(5),
        // 热存储配额：50 MB（超过后强制迁移）
        hot_quota_bytes: 50 * 1024 * 1024,
        cold_storage_path: cold_dir.clone(),
    };

    println!("配置信息：");
    println!("  热存储路径: {}", hot_dir.display());
    println!("  冷存储路径: {}", cold_dir.display());
    println!(
        "  热存储配额: {} MB",
        policy.hot_quota_bytes / (1024 * 1024)
    );
    println!("  冷迁移阈值: {} 天\n", policy.cold_threshold_days);

    // 创建分层存储服务
    let service = Arc::new(TieredStorageService::new(&hot_dir, policy)?);

    println!("步骤 1: 写入测试数据到热存储");
    println!("----------------------------------------");

    // 写入一些测试文件
    let files = vec![
        (
            "document1.txt",
            "这是一个频繁访问的文档".as_bytes().to_vec(),
        ),
        ("image1.jpg", vec![0xFFu8; 1024 * 100]), // 100 KB 图片
        ("video1.mp4", vec![0xAAu8; 1024 * 1024 * 10]), // 10 MB 视频
        ("archive.zip", vec![0x55u8; 1024 * 1024 * 5]), // 5 MB 压缩包
    ];

    let mut hashes = Vec::new();
    for (name, data) in &files {
        let src = temp.path().join(name);
        std::fs::write(&src, data)?;

        // 通过服务导入（自动注册分层元数据）
        let (hash, size) = service.import_file(&src)?;
        hashes.push((name.to_string(), hash.clone()));

        println!("  ✓ {} ({})", name, format_bytes(size));
    }

    // 显示初始统计
    let stats = service.stats();
    println!("\n初始状态:");
    println!(
        "  热存储: {} 个文件, {}",
        stats.hot_blobs,
        format_bytes(stats.hot_bytes)
    );
    println!(
        "  冷存储: {} 个文件, {}",
        stats.cold_blobs,
        format_bytes(stats.cold_bytes)
    );
    println!("  热数据比例: {:.1}%\n", stats.hot_ratio() * 100.0);

    println!("步骤 2: 模拟访问模式");
    println!("----------------------------------------");
    println!("  频繁访问 document1.txt（保持在热存储）");
    for _ in 0..5 {
        let hash = &hashes[0].1;
        let _ = service.read_blob(hash)?;
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("  ✓ 访问 5 次\n");

    println!("步骤 3: 执行冷热数据迁移扫描");
    println!("----------------------------------------");

    // 等待一段时间，让数据"变冷"
    std::thread::sleep(Duration::from_secs(2));

    let migration_stats = service.scan_and_migrate()?;
    println!(
        "  热存储使用: {}",
        format_bytes(migration_stats.hot_usage_bytes)
    );
    println!(
        "  迁移到冷存储: {} 个文件",
        migration_stats.migrated_to_cold
    );
    println!(
        "  迁移数据量: {}",
        format_bytes(migration_stats.migrated_bytes)
    );

    if migration_stats.migration_errors > 0 {
        println!("  ⚠ 迁移错误: {}", migration_stats.migration_errors);
    }

    // 显示迁移后统计
    let stats = service.stats();
    println!("\n迁移后状态:");
    println!(
        "  热存储: {} 个文件, {}",
        stats.hot_blobs,
        format_bytes(stats.hot_bytes)
    );
    println!(
        "  冷存储: {} 个文件, {}",
        stats.cold_blobs,
        format_bytes(stats.cold_bytes)
    );
    println!("  热数据比例: {:.1}%\n", stats.hot_ratio() * 100.0);

    println!("步骤 4: 访问冷数据（自动回温）");
    println!("----------------------------------------");

    // 访问一个已经被迁移到冷存储的文件
    if let Some((name, hash)) = hashes.iter().find(|(n, _)| n != "document1.txt") {
        println!("  访问 {} (当前在冷存储)", name);
        let data = service.read_blob(hash)?;
        println!("  ✓ 成功读取 {} 字节", data.len());
        println!("  ✓ 数据已自动回温到热存储\n");
    }

    // 最终统计
    let stats = service.stats();
    println!("\n最终状态:");
    println!(
        "  热存储: {} 个文件, {}",
        stats.hot_blobs,
        format_bytes(stats.hot_bytes)
    );
    println!(
        "  冷存储: {} 个文件, {}",
        stats.cold_blobs,
        format_bytes(stats.cold_bytes)
    );
    println!(
        "  总计: {} 个文件, {}",
        stats.total_blobs(),
        format_bytes(stats.total_bytes())
    );
    println!("  热数据比例: {:.1}%", stats.hot_ratio() * 100.0);

    println!("\n=== 演示完成 ===");
    println!("\n💡 生产环境建议：");
    println!("  • 将冷存储配置到外部便宜的HDD或NAS");
    println!("  • 设置合理的冷迁移阈值（如30天）");
    println!("  • 根据业务需求调整热存储配额");
    println!("  • 启用后台自动扫描任务");

    Ok(())
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}
