//! 数据双备份存储（Dual-Replica Storage）
//!
//! 为每个写入的 blob 在本地维护两个独立物理副本，确保单盘故障时数据不丢失：
//!
//! - **主副本**（primary）：正常读写路径
//! - **镜像副本**（mirror）：每次写入时同步写入，用于故障恢复
//!
//! ## 设计原则
//!
//! 1. **同步写入**：主副本和镜像副本在同一次操作中写入，成功才返回
//! 2. **自动故障切换**：主副本读取失败时，自动从镜像副本恢复
//! 3. **完整性校验**：定期对比两个副本的一致性
//! 4. **修复机制**：发现不一致时，用健康副本修复损坏副本
//!
//! ## 布局
//!
//! ```text
//! <data_dir>/
//!   primary/      # 主副本
//!     <hash>/
//!       meta.json
//!       complete
//!       chunks/000000 ...
//!   mirror/       # 镜像副本（物理独立，建议挂载在不同磁盘）
//!     <hash>/
//!       meta.json
//!       complete
//!       chunks/000000 ...
//!   mirror_health.json  # 双备份健康状态记录
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use a3net_types::ContentHash;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{info, warn};

use crate::store::BlobStore;

// ─────────────────────────────────────────────────────────────────
// 错误类型
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum DualBackupError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("blob not found in either replica: {0}")]
    NotFound(String),
    #[error("primary write failed: {0}")]
    PrimaryWriteFailed(String),
    #[error("mirror write failed: {0}")]
    MirrorWriteFailed(String),
    #[error("both replicas corrupted: {0}")]
    BothCorrupted(String),
    #[error("repair failed: {0}")]
    RepairFailed(String),
}

// ─────────────────────────────────────────────────────────────────
// 副本健康状态
// ─────────────────────────────────────────────────────────────────

/// 单个副本的健康状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplicaHealth {
    /// 健康
    Healthy,
    /// 损坏（hash 不匹配）
    Corrupted,
    /// 缺失
    Missing,
}

/// 一个 blob 的双备份健康报告
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DualReplicaStatus {
    pub hash: ContentHash,
    pub primary: ReplicaHealth,
    pub mirror: ReplicaHealth,
    /// 最后一次健康检查时间（Unix 毫秒）
    pub last_check_ms: u64,
    /// 是否已被修复
    pub repaired: bool,
}

impl DualReplicaStatus {
    /// 数据是否安全（至少一个副本健康）
    pub fn is_safe(&self) -> bool {
        self.primary == ReplicaHealth::Healthy || self.mirror == ReplicaHealth::Healthy
    }

    /// 是否完全健康（两个副本都健康）
    pub fn is_fully_healthy(&self) -> bool {
        self.primary == ReplicaHealth::Healthy && self.mirror == ReplicaHealth::Healthy
    }
}

// ─────────────────────────────────────────────────────────────────
// 配置
// ─────────────────────────────────────────────────────────────────

/// 双备份存储配置
#[derive(Debug, Clone)]
pub struct DualBackupConfig {
    /// 镜像副本存储路径（建议挂载到不同物理磁盘以获得最大保护）
    pub mirror_path: PathBuf,
    /// 是否在写入失败时回滚（严格模式）
    /// - `true`：主或镜像任一写入失败则整个操作失败
    /// - `false`：主副本写入成功即返回成功，镜像失败仅告警
    pub strict_mode: bool,
    /// 健康检查间隔
    pub health_check_interval: std::time::Duration,
}

impl DualBackupConfig {
    pub fn new(mirror_path: PathBuf) -> Self {
        Self {
            mirror_path,
            strict_mode: true,
            health_check_interval: std::time::Duration::from_secs(3600),
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// 双备份存储服务
// ─────────────────────────────────────────────────────────────────

/// 双备份存储服务
///
/// 对外暴露与 `BlobStore` 相同的写入接口，内部同步维护
/// 主副本和镜像副本。
pub struct DualBackupStore {
    primary: Arc<BlobStore>,
    mirror: Arc<BlobStore>,
    config: DualBackupConfig,
    /// 健康状态缓存（hash → status）
    health_cache: Arc<RwLock<std::collections::HashMap<ContentHash, DualReplicaStatus>>>,
    health_path: PathBuf,
}

impl DualBackupStore {
    /// 创建双备份存储
    ///
    /// - `primary_dir`：主副本目录（内部高速存储）
    /// - `config`：双备份配置（含镜像目录，建议配置到不同物理磁盘）
    pub fn new(primary_dir: &Path, config: DualBackupConfig) -> std::io::Result<Self> {
        let primary = Arc::new(BlobStore::new(primary_dir)?);

        let mirror_dir = if config.mirror_path.is_absolute() {
            config.mirror_path.clone()
        } else {
            primary_dir
                .parent()
                .unwrap_or(primary_dir)
                .join(&config.mirror_path)
        };

        let mirror = Arc::new(BlobStore::new(&mirror_dir)?);

        let health_path = primary_dir
            .parent()
            .unwrap_or(primary_dir)
            .join("mirror_health.json");

        let health_cache = if health_path.exists() {
            let raw = std::fs::read_to_string(&health_path)?;
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            std::collections::HashMap::new()
        };

        info!(
            primary_dir = %primary_dir.display(),
            mirror_dir = %mirror_dir.display(),
            strict_mode = config.strict_mode,
            "双备份存储已初始化"
        );

        Ok(Self {
            primary,
            mirror,
            config,
            health_cache: Arc::new(RwLock::new(health_cache)),
            health_path,
        })
    }

    /// 双写：将数据同步写入主副本和镜像副本
    pub fn put_bytes_dual(&self, data: &[u8]) -> Result<ContentHash, DualBackupError> {
        // 1. 写入主副本
        let (hash, _size) = self
            .primary
            .put_bytes_sync(data)
            .map_err(|e| DualBackupError::PrimaryWriteFailed(e.to_string()))?;

        // 2. 写入镜像副本
        match self.mirror.put_bytes_sync(data) {
            Ok(_) => {
                self.update_health(&hash, ReplicaHealth::Healthy, ReplicaHealth::Healthy);
                info!(hash = %hash, bytes = data.len(), "双备份写入成功");
                Ok(hash)
            }
            Err(e) => {
                warn!(hash = %hash, error = %e, "镜像副本写入失败");
                self.update_health(&hash, ReplicaHealth::Healthy, ReplicaHealth::Missing);

                if self.config.strict_mode {
                    // 严格模式：回滚主副本写入
                    let _ = self.primary.remove(&hash);
                    Err(DualBackupError::MirrorWriteFailed(e.to_string()))
                } else {
                    // 非严格模式：记录告警但不失败
                    warn!(hash = %hash, "镜像写入失败，仅主副本可用（降级模式）");
                    Ok(hash)
                }
            }
        }
    }

    /// 双写文件：从文件导入，同步写入两个副本
    pub fn import_file_dual(&self, source: &Path) -> Result<(ContentHash, u64), DualBackupError> {
        // 1. 写入主副本
        let (hash, size) = self
            .primary
            .import_file_sync(source)
            .map_err(|e| DualBackupError::PrimaryWriteFailed(e.to_string()))?;

        // 2. 写入镜像副本
        match self.mirror.import_file_sync(source) {
            Ok(_) => {
                self.update_health(&hash, ReplicaHealth::Healthy, ReplicaHealth::Healthy);
                info!(hash = %hash, size_bytes = size, "双备份文件导入成功");
                Ok((hash, size))
            }
            Err(e) => {
                warn!(hash = %hash, error = %e, "镜像副本文件导入失败");
                self.update_health(&hash, ReplicaHealth::Healthy, ReplicaHealth::Missing);

                if self.config.strict_mode {
                    let _ = self.primary.remove(&hash);
                    Err(DualBackupError::MirrorWriteFailed(e.to_string()))
                } else {
                    Ok((hash, size))
                }
            }
        }
    }

    /// 读取数据（自动故障切换）
    pub fn read_blob(&self, hash: &ContentHash) -> Result<Vec<u8>, DualBackupError> {
        // 优先从主副本读取
        let primary_result = try_read_all(&self.primary, hash);

        match primary_result {
            Ok(data) => Ok(data),
            Err(primary_err) => {
                warn!(hash = %hash, error = %primary_err, "主副本读取失败，尝试镜像副本");

                // 故障切换到镜像副本 —— 只有镜像读取真正成功后才
                // 把它标记为 Healthy；标记必须发生在 try_read_all
                // 返回之后，不能提前假设镜像是好的。
                match try_read_all(&self.mirror, hash) {
                    Ok(data) => {
                        info!(hash = %hash, "从镜像副本成功读取，触发主副本修复");
                        self.update_health(hash, ReplicaHealth::Corrupted, ReplicaHealth::Healthy);
                        // 修复失败仅记录日志，不影响本次读取结果——
                        // 数据已经安全返回给调用方，修复是锦上添花。
                        if let Err(e) = self.repair_primary(hash, &data) {
                            warn!(hash = %hash, error = %e, "主副本修复失败，将在下次健康检查时重试");
                        }
                        Ok(data)
                    }
                    Err(mirror_err) => {
                        warn!(hash = %hash, error = %mirror_err, "镜像副本读取也失败，两个副本均损坏");
                        self.update_health(
                            hash,
                            ReplicaHealth::Corrupted,
                            ReplicaHealth::Corrupted,
                        );
                        Err(DualBackupError::BothCorrupted(hash.to_string()))
                    }
                }
            }
        }
    }

    /// 执行健康检查：扫描所有 blob，对比两个副本的一致性
    pub fn health_check(&self) -> HealthCheckReport {
        let primary_blobs = self.primary.list_complete().unwrap_or_default();
        let mirror_blobs = self.mirror.list_complete().unwrap_or_default();

        let primary_set: std::collections::HashSet<_> = primary_blobs.iter().cloned().collect();
        let mirror_set: std::collections::HashSet<_> = mirror_blobs.iter().cloned().collect();

        let mut report = HealthCheckReport::default();

        // 检查所有在主副本中的 blob
        for hash in &primary_blobs {
            let primary_ok = verify_blob_integrity(&self.primary, hash);
            let mirror_ok = if mirror_set.contains(hash) {
                verify_blob_integrity(&self.mirror, hash)
            } else {
                false
            };

            // 修复成功后副本状态应反映"修复后的现实"（Healthy），
            // 而不是修复前的诊断结果——否则 `health_summary` 会
            // 一直把已修复的 blob 计入 degraded/at_risk。
            let (primary_health, mirror_health) = match (primary_ok, mirror_ok) {
                (true, true) => {
                    report.healthy += 1;
                    (ReplicaHealth::Healthy, ReplicaHealth::Healthy)
                }
                (true, false) => {
                    report.mirror_degraded += 1;
                    // 用主副本修复镜像
                    if let Ok(data) = try_read_all(&self.primary, hash) {
                        match self.mirror.put_bytes_sync(&data) {
                            Ok(_) => {
                                report.repaired += 1;
                                info!(hash = %hash, "镜像副本已修复");
                                (ReplicaHealth::Healthy, ReplicaHealth::Healthy)
                            }
                            Err(e) => {
                                warn!(hash = %hash, error = %e, "镜像副本修复失败");
                                (ReplicaHealth::Healthy, ReplicaHealth::Corrupted)
                            }
                        }
                    } else {
                        (ReplicaHealth::Healthy, ReplicaHealth::Corrupted)
                    }
                }
                (false, true) => {
                    report.primary_degraded += 1;
                    // 用镜像副本修复主副本
                    if let Ok(data) = try_read_all(&self.mirror, hash) {
                        match self.primary.put_bytes_sync(&data) {
                            Ok(_) => {
                                report.repaired += 1;
                                info!(hash = %hash, "主副本已修复");
                                (ReplicaHealth::Healthy, ReplicaHealth::Healthy)
                            }
                            Err(e) => {
                                warn!(hash = %hash, error = %e, "主副本修复失败");
                                (ReplicaHealth::Corrupted, ReplicaHealth::Healthy)
                            }
                        }
                    } else {
                        (ReplicaHealth::Corrupted, ReplicaHealth::Healthy)
                    }
                }
                (false, false) => {
                    report.both_corrupted += 1;
                    warn!(hash = %hash, "两个副本均损坏，数据丢失");
                    (ReplicaHealth::Corrupted, ReplicaHealth::Corrupted)
                }
            };

            self.update_health(hash, primary_health, mirror_health);
        }

        // 检查只在镜像中存在的 blob（主副本丢失）
        for hash in mirror_set.difference(&primary_set) {
            report.primary_missing += 1;
            warn!(hash = %hash, "主副本缺失，从镜像恢复");

            let primary_health = if let Ok(data) = try_read_all(&self.mirror, hash) {
                match self.primary.put_bytes_sync(&data) {
                    Ok(_) => {
                        report.repaired += 1;
                        info!(hash = %hash, "主副本从镜像成功恢复");
                        ReplicaHealth::Healthy
                    }
                    Err(e) => {
                        warn!(hash = %hash, error = %e, "主副本恢复失败");
                        ReplicaHealth::Missing
                    }
                }
            } else {
                ReplicaHealth::Missing
            };

            self.update_health(hash, primary_health, ReplicaHealth::Healthy);
        }

        let _ = self.persist_health();
        report
    }

    /// 修复主副本（从镜像数据重写）
    fn repair_primary(&self, hash: &ContentHash, data: &[u8]) -> Result<(), DualBackupError> {
        self.primary
            .put_bytes_sync(data)
            .map(|_| {
                self.update_health(hash, ReplicaHealth::Healthy, ReplicaHealth::Healthy);
                info!(hash = %hash, "主副本修复成功");
            })
            .map_err(|e| DualBackupError::RepairFailed(e.to_string()))
    }

    fn update_health(&self, hash: &ContentHash, primary: ReplicaHealth, mirror: ReplicaHealth) {
        let mut cache = self.health_cache.write();
        let entry = cache.entry(hash.clone()).or_insert(DualReplicaStatus {
            hash: hash.clone(),
            primary: ReplicaHealth::Healthy,
            mirror: ReplicaHealth::Healthy,
            last_check_ms: 0,
            repaired: false,
        });
        entry.primary = primary;
        entry.mirror = mirror;
        entry.last_check_ms = current_millis();
    }

    fn persist_health(&self) -> std::io::Result<()> {
        let cache = self.health_cache.read();
        let json = serde_json::to_string_pretty(&*cache)?;
        let tmp = self.health_path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &self.health_path)?;
        Ok(())
    }

    /// 获取健康摘要
    pub fn health_summary(&self) -> DualBackupHealthSummary {
        let cache = self.health_cache.read();
        let mut summary = DualBackupHealthSummary::default();

        for status in cache.values() {
            summary.total += 1;
            if status.is_fully_healthy() {
                summary.fully_healthy += 1;
            } else if status.is_safe() {
                summary.degraded += 1;
            } else {
                summary.at_risk += 1;
            }
        }

        summary
    }

    /// 暴露主副本存储（用于与其他模块集成）
    pub fn primary(&self) -> Arc<BlobStore> {
        self.primary.clone()
    }

    /// 暴露镜像副本存储
    pub fn mirror(&self) -> Arc<BlobStore> {
        self.mirror.clone()
    }

    /// 启动后台健康检查任务
    pub fn start_health_check_task(
        self: Arc<Self>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let interval = self.config.health_check_interval;
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            info!(?interval, "双备份健康检查后台任务已启动");

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        let store = self.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            let report = store.health_check();
                            info!(
                                healthy = report.healthy,
                                repaired = report.repaired,
                                at_risk = report.both_corrupted,
                                "双备份健康检查完成"
                            );
                        }).await;
                    }
                    _ = shutdown.changed() => {
                        if *shutdown.borrow() {
                            info!("双备份健康检查后台任务已停止");
                            return;
                        }
                    }
                }
            }
        })
    }
}

// ─────────────────────────────────────────────────────────────────
// 健康检查报告
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct HealthCheckReport {
    pub healthy: usize,
    pub mirror_degraded: usize,
    pub primary_degraded: usize,
    pub both_corrupted: usize,
    pub primary_missing: usize,
    pub repaired: usize,
}

#[derive(Debug, Default)]
pub struct DualBackupHealthSummary {
    pub total: usize,
    pub fully_healthy: usize,
    pub degraded: usize,
    pub at_risk: usize,
}

// ─────────────────────────────────────────────────────────────────
// 内部辅助函数
// ─────────────────────────────────────────────────────────────────

fn try_read_all(store: &BlobStore, hash: &ContentHash) -> Result<Vec<u8>, std::io::Error> {
    if !store.has_complete(hash) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("blob not found: {hash}"),
        ));
    }
    let (_, count) = store
        .meta(hash)
        .map_err(|e| std::io::Error::other(e.to_string()))?;

    let mut buf = Vec::new();
    let mut hasher = blake3::Hasher::new();
    for i in 0..count {
        let chunk = store.read_chunk_sync(hash, i)?;
        hasher.update(&chunk);
        buf.extend_from_slice(&chunk);
    }

    // 验证完整性：确保读取的数据的哈希与预期一致
    let actual_hash = ContentHash::from_hex(hasher.finalize().to_hex().as_ref())
        .expect("blake3 hex is always 64 chars");
    if &actual_hash != hash {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("hash mismatch: expected {hash}, got {actual_hash}"),
        ));
    }

    Ok(buf)
}

/// 通过重新计算 BLAKE3 哈希验证副本完整性
fn verify_blob_integrity(store: &BlobStore, hash: &ContentHash) -> bool {
    store.verify_complete(hash)
}

fn current_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ─────────────────────────────────────────────────────────────────
// 测试
// ─────────────────────────────────────────────────────────────────

#[cfg(all(test, feature = "iroh"))]
mod tests {
    use super::*;

    fn make_dual_store(dir: &Path) -> DualBackupStore {
        let primary_dir = dir.join("primary");
        let mirror_dir = dir.join("mirror");
        let config = DualBackupConfig {
            mirror_path: mirror_dir,
            strict_mode: true,
            health_check_interval: std::time::Duration::from_secs(60),
        };
        DualBackupStore::new(&primary_dir, config).unwrap()
    }

    #[test]
    fn dual_write_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_dual_store(dir.path());

        let data = b"hello dual backup";
        let hash = store.put_bytes_dual(data).unwrap();

        // 两个副本都有
        assert!(store.primary.has_complete(&hash));
        assert!(store.mirror.has_complete(&hash));

        let read_back = store.read_blob(&hash).unwrap();
        assert_eq!(read_back, data);
    }

    #[test]
    fn failover_from_mirror_when_primary_corrupted() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_dual_store(dir.path());

        let data = b"failover test payload";
        let hash = store.put_bytes_dual(data).unwrap();

        // 损坏主副本的第一个 chunk
        let chunk_path = store.primary.blob_dir(&hash).join("chunks").join("000000");
        let mut chunk_bytes = std::fs::read(&chunk_path).unwrap();
        chunk_bytes[0] ^= 0xFF;
        std::fs::write(&chunk_path, &chunk_bytes).unwrap();

        // 主副本损坏，但从镜像能读到正确数据
        let read_back = store.read_blob(&hash).unwrap();
        assert_eq!(read_back, data);
    }

    #[test]
    fn health_check_detects_and_repairs_degraded() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_dual_store(dir.path());

        // 写入数据
        let hash = store.put_bytes_dual(b"health check test").unwrap();

        // 删除镜像副本，模拟镜像损坏
        let mirror_dir = store.mirror.blob_dir(&hash);
        std::fs::remove_dir_all(&mirror_dir).unwrap();

        let report = store.health_check();
        // 镜像损坏被检测到，并被修复
        assert_eq!(report.mirror_degraded, 1);
        assert_eq!(report.repaired, 1);

        // 修复后镜像副本应恢复
        assert!(store.mirror.has_complete(&hash));
    }

    /// `health_check` 修复镜像副本成功后，健康状态必须反映“修复
    /// 后”的现实（两者皆健康），而不是修复前的诊断结果。否则
    /// `health_summary` 会把已经修复好的 blob 永久计入 degraded。
    #[test]
    fn health_check_marks_repaired_blob_as_fully_healthy() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_dual_store(dir.path());
        let hash = store.put_bytes_dual(b"repair status test").unwrap();

        std::fs::remove_dir_all(store.mirror.blob_dir(&hash)).unwrap();
        let report = store.health_check();
        assert_eq!(report.repaired, 1);

        let summary = store.health_summary();
        assert_eq!(summary.fully_healthy, 1);
        assert_eq!(summary.degraded, 0);
        assert_eq!(summary.at_risk, 0);
    }

    /// 当主副本损坏但镜像读取也失败时（两者皆损坏），健康状态
    /// 必须标记为 at_risk，绝不能停留在“乐观地把镜像标记健康”的
    /// 中间状态。
    #[test]
    fn read_blob_both_corrupted_reports_at_risk() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_dual_store(dir.path());
        let hash = store.put_bytes_dual(b"both corrupted test").unwrap();

        for replica in [&store.primary, &store.mirror] {
            let chunk_path = replica.blob_dir(&hash).join("chunks").join("000000");
            let mut bytes = std::fs::read(&chunk_path).unwrap();
            bytes[0] ^= 0xFF;
            std::fs::write(&chunk_path, &bytes).unwrap();
        }

        let err = store.read_blob(&hash).unwrap_err();
        assert!(matches!(err, DualBackupError::BothCorrupted(_)));

        let summary = store.health_summary();
        assert_eq!(summary.at_risk, 1);
        assert_eq!(summary.fully_healthy, 0);
    }

    #[test]
    fn dual_backup_status_safety() {
        let status = DualReplicaStatus {
            hash: ContentHash::from_bytes(b"test"),
            primary: ReplicaHealth::Healthy,
            mirror: ReplicaHealth::Corrupted,
            last_check_ms: 0,
            repaired: false,
        };
        assert!(status.is_safe());
        assert!(!status.is_fully_healthy());

        let both_corrupted = DualReplicaStatus {
            hash: ContentHash::from_bytes(b"bad"),
            primary: ReplicaHealth::Corrupted,
            mirror: ReplicaHealth::Corrupted,
            last_check_ms: 0,
            repaired: false,
        };
        assert!(!both_corrupted.is_safe());
    }
}
