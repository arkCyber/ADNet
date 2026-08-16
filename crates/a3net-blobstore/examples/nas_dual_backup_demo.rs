//! NAS 双备份存储演示
//!
//! 演示如何使用双备份存储来确保数据安全：
//! - 每次写入同时写入主副本和镜像副本
//! - 主副本损坏时自动从镜像副本恢复
//! - 定期健康检查并自动修复
//!
//! 运行方式：
//! ```bash
//! cargo run --example nas_dual_backup_demo
//! ```

use std::sync::Arc;

use a3net_blobstore::{DualBackupConfig, DualBackupStore};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 设置日志
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("=== NAS 双备份存储演示 ===\n");

    // 创建临时目录
    let temp = tempfile::tempdir()?;
    let primary_dir = temp.path().join("primary");
    let mirror_dir = temp.path().join("mirror_disk");

    println!("配置信息：");
    println!("  主副本路径: {}", primary_dir.display());
    println!(
        "  镜像副本路径: {} (建议挂载到不同物理磁盘)",
        mirror_dir.display()
    );
    println!("  严格模式: 开启（主或镜像任一失败则整体失败）\n");

    // 创建双备份配置
    let config = DualBackupConfig {
        mirror_path: mirror_dir.clone(),
        strict_mode: true,
        health_check_interval: std::time::Duration::from_secs(60),
    };

    // 创建双备份存储
    let store = Arc::new(DualBackupStore::new(&primary_dir, config)?);

    println!("步骤 1: 写入测试数据（双写）");
    println!("----------------------------------------");

    // 写入一些测试文件
    let binary_data = vec![0x42u8; 1024 * 50];
    let test_files: Vec<(&str, &[u8])> = vec![
        (
            "重要文档.txt",
            "这是一份非常重要的文档，需要双备份保护".as_bytes(),
        ),
        (
            "配置文件.json",
            r#"{"app": "a3net", "version": "1.0"}"#.as_bytes(),
        ),
        ("关键数据.bin", &binary_data), // 50 KB 二进制数据
    ];

    let mut hashes = Vec::new();
    for (name, data) in &test_files {
        let hash = store.put_bytes_dual(data)?;
        hashes.push((name.to_string(), hash.clone()));
        println!("  ✓ {} ({} 字节) - 已写入两个副本", name, data.len());
    }

    // 验证两个副本都存在
    println!("\n验证副本完整性:");
    for (name, hash) in &hashes {
        let primary_exists = store.primary().has_complete(hash);
        let mirror_exists = store.mirror().has_complete(hash);
        println!("  {} - 主副本: ✓, 镜像副本: ✓", name);
        assert!(primary_exists && mirror_exists, "双副本应该都存在");
    }

    // 显示健康摘要
    let summary = store.health_summary();
    println!("\n健康状态:");
    println!("  总计: {} 个文件", summary.total);
    println!("  完全健康: {} 个 ✓", summary.fully_healthy);
    println!("  降级运行: {} 个 ⚠", summary.degraded);
    println!("  有风险: {} 个 ✗", summary.at_risk);

    println!("\n步骤 2: 模拟主副本损坏");
    println!("----------------------------------------");

    // 人为损坏第一个文件的主副本
    let (corrupt_name, corrupt_hash) = &hashes[0];
    println!("  损坏文件: {}", corrupt_name);

    let chunk_path = store
        .primary()
        .blob_dir(corrupt_hash)
        .join("chunks")
        .join("000000");
    let mut chunk_bytes = std::fs::read(&chunk_path)?;
    chunk_bytes[0] ^= 0xFF; // 翻转第一个字节
    std::fs::write(&chunk_path, &chunk_bytes)?;

    println!("  ✓ 主副本已被损坏（模拟磁盘错误）\n");

    println!("步骤 3: 读取数据（自动故障切换）");
    println!("----------------------------------------");

    println!("  尝试读取损坏的文件: {}", corrupt_name);
    match store.read_blob(corrupt_hash) {
        Ok(data) => {
            println!("  ✓ 成功读取 {} 字节", data.len());
            println!("  ✓ 系统自动从镜像副本读取");
            println!("  ✓ 主副本已被自动修复\n");

            // 验证数据正确性
            assert_eq!(&data, test_files[0].1);
        }
        Err(e) => {
            println!("  ✗ 读取失败: {}", e);
            return Err(e.into());
        }
    }

    println!("步骤 4: 执行健康检查");
    println!("----------------------------------------");

    // 再次损坏一个文件的镜像副本
    let (_, second_hash) = &hashes[1];
    let mirror_chunk = store
        .mirror()
        .blob_dir(second_hash)
        .join("chunks")
        .join("000000");
    let mut mirror_bytes = std::fs::read(&mirror_chunk)?;
    mirror_bytes[0] ^= 0xFF;
    std::fs::write(&mirror_chunk, &mirror_bytes)?;

    println!("  人为损坏一个镜像副本用于测试...");

    let report = store.health_check();
    println!("\n健康检查报告:");
    println!("  完全健康: {} 个", report.healthy);
    println!("  主副本降级: {} 个", report.primary_degraded);
    println!("  镜像副本降级: {} 个", report.mirror_degraded);
    println!("  主副本缺失: {} 个", report.primary_missing);
    println!("  双副本损坏: {} 个", report.both_corrupted);
    println!("  已修复: {} 个 ✓", report.repaired);

    // 最终健康摘要
    let summary = store.health_summary();
    println!("\n最终健康状态:");
    println!("  总计: {} 个文件", summary.total);
    println!(
        "  完全健康: {} 个 ({}%)",
        summary.fully_healthy,
        summary.fully_healthy * 100 / summary.total.max(1)
    );

    println!("\n步骤 5: 验证所有文件可读");
    println!("----------------------------------------");

    for (name, hash) in &hashes {
        match store.read_blob(hash) {
            Ok(data) => println!("  ✓ {} ({} 字节)", name, data.len()),
            Err(e) => println!("  ✗ {} 读取失败: {}", name, e),
        }
    }

    println!("\n=== 演示完成 ===");
    println!("\n💡 双备份存储的优势：");
    println!("  ✓ 单磁盘故障时数据不丢失");
    println!("  ✓ 自动检测并修复损坏的副本");
    println!("  ✓ 主副本故障时自动切换到镜像副本");
    println!("  ✓ 适合存储重要数据和关键配置");

    println!("\n💡 生产环境建议：");
    println!("  • 将镜像副本配置到不同物理磁盘");
    println!("  • 使用 RAID 或网络 NAS 作为镜像存储");
    println!("  • 启用后台健康检查任务");
    println!("  • 定期查看健康报告并告警");

    Ok(())
}
