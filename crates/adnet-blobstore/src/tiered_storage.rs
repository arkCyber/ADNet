//! 冷热数据分离存储（Tiered Storage）
//!
//! 根据访问频率自动在内部存储（热）和外部存储（冷）之间迁移数据：
//! - **热数据层**：内部高速存储（SSD/NVMe）— 频繁访问的数据
//! - **冷数据层**：外部经济存储（HDD/NAS）— 不常访问的数据
//!
//! ## 策略
//!
//! - 访问频率跟踪：每次读取更新 `last_access` 时间戳
//! - 自动迁移：周期性扫描，将 N 天未访问的数据迁移到冷存储
//! - 智能回温：冷数据被访问时自动提升回热存储
//!
//! ## 布局
//!
//! ```text
//! <data_dir>/
//!   hot/          # 内部高速存储
//!     <hash>/...
//!   cold/         # 外部经济存储（可配置为外部挂载点）
//!     <hash>/...
//!   tier_meta.json  # 冷热数据元数据
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use adnet_types::ContentHash;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{info, warn};

use crate::store::BlobStore;

/// 冷热数据层级
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StorageTier {
    /// 热数据：内部高速存储（频繁访问）
    Hot,
    /// 冷数据：外部经济存储（不常访问）
    Cold,
}

/// 单个 blob 的分层元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierMetadata {
    pub hash: ContentHash,
    pub tier: StorageTier,
    /// 最后访问时间（Unix 毫秒）
    pub last_access_ms: u64,
    /// 访问次数（用于热度评估）
    pub access_count: u64,
    /// 数据大小（字节）
    pub size_bytes: u64,
}

/// 冷热数据配置策略
#[derive(Debug, Clone)]
pub struct TieringPolicy {
    /// 多少天未访问后迁移到冷存储
    pub cold_threshold_days: u32,
    /// 扫描间隔
    pub scan_interval: Duration,
    /// 热存储配额（字节）— 超过后强制迁移最冷的数据
    pub hot_quota_bytes: u64,
    /// 外部冷存储路径（可以是外部挂载点）
    pub cold_storage_path: PathBuf,
}

impl Default for TieringPolicy {
    fn default() -> Self {
        Self {
            cold_threshold_days: 30,
            scan_interval: Duration::from_secs(3600), // 1小时扫描一次
            hot_quota_bytes: 100 * 1024 * 1024 * 1024, // 100 GB
            cold_storage_path: PathBuf::from("cold"),
        }
    }
}

#[derive(Debug, Error)]
pub enum TieringError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("blob not found: {0}")]
    BlobNotFound(String),
    #[error("migration failed: {0}")]
    MigrationFailed(String),
    #[error("serialization error: {0}")]
    Serialization(String),
}

/// 分层存储服务 — 管理冷热数据自动迁移
///
/// `hot_store` / `cold_store` 不对外公开：所有写入必须经过
/// [`Self::put_bytes`] / [`Self::import_file`]，否则元数据
/// （层级、访问时间、大小）会与实际磁盘状态失去同步，导致
/// `scan_and_migrate` 的配额计算和迁移决策出错。
pub struct TieredStorageService {
    hot_store: Arc<BlobStore>,
    cold_store: Arc<BlobStore>,
    metadata: Arc<RwLock<HashMap<ContentHash, TierMetadata>>>,
    policy: TieringPolicy,
    meta_path: PathBuf,
}

impl TieredStorageService {
    /// 创建分层存储服务
    ///
    /// - `hot_dir`: 内部高速存储目录
    /// - `policy`: 冷热数据策略
    pub fn new(hot_dir: &Path, policy: TieringPolicy) -> std::io::Result<Self> {
        let hot_store = Arc::new(BlobStore::new(hot_dir)?);

        // 冷存储可以配置为外部挂载点
        let cold_dir = if policy.cold_storage_path.is_absolute() {
            policy.cold_storage_path.clone()
        } else {
            hot_dir
                .parent()
                .unwrap_or(hot_dir)
                .join(&policy.cold_storage_path)
        };

        let cold_store = Arc::new(BlobStore::new(&cold_dir)?);

        let meta_path = hot_dir.parent().unwrap_or(hot_dir).join("tier_meta.json");

        // 加载现有元数据
        let metadata = if meta_path.exists() {
            let raw = std::fs::read_to_string(&meta_path)?;
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            HashMap::new()
        };

        let mut metadata = metadata;

        // 对账：磁盘上存在但元数据缺失的 blob（例如进程在上次
        // `persist_metadata` 之前异常退出）需要被重新发现，否则
        // 它们会对 `scan_and_migrate` 永久不可见，既不会被迁移
        // 也不会计入配额统计。
        let mut reconciled = 0usize;
        if let Ok(hot_hashes) = hot_store.list_complete() {
            for hash in hot_hashes {
                metadata.entry(hash.clone()).or_insert_with(|| {
                    reconciled += 1;
                    let size = hot_store.meta(&hash).map(|(s, _)| s).unwrap_or(0);
                    TierMetadata {
                        hash,
                        tier: StorageTier::Hot,
                        last_access_ms: current_millis(),
                        access_count: 0,
                        size_bytes: size,
                    }
                });
            }
        }
        if let Ok(cold_hashes) = cold_store.list_complete() {
            for hash in cold_hashes {
                metadata.entry(hash.clone()).or_insert_with(|| {
                    reconciled += 1;
                    let size = cold_store.meta(&hash).map(|(s, _)| s).unwrap_or(0);
                    TierMetadata {
                        hash,
                        tier: StorageTier::Cold,
                        last_access_ms: current_millis(),
                        access_count: 0,
                        size_bytes: size,
                    }
                });
            }
        }

        info!(
            hot_dir = %hot_dir.display(),
            cold_dir = %cold_dir.display(),
            cold_threshold_days = policy.cold_threshold_days,
            hot_quota_gb = policy.hot_quota_bytes / (1024 * 1024 * 1024),
            reconciled_untracked_blobs = reconciled,
            "分层存储服务已启动"
        );

        let service = Self {
            hot_store,
            cold_store,
            metadata: Arc::new(RwLock::new(metadata)),
            policy,
            meta_path,
        };
        if reconciled > 0 {
            let _ = service.persist_metadata();
        }
        Ok(service)
    }

    /// 写入原始字节到热存储，并注册分层元数据。
    ///
    /// 这是 `hot_store.put_bytes_sync` 的唯一推荐入口——直接绕过
    /// 本方法写热存储会让新 blob 对 `scan_and_migrate` 不可见。
    pub fn put_bytes(&self, data: &[u8]) -> Result<ContentHash, TieringError> {
        let (hash, size) = self.hot_store.put_bytes_sync(data)?;
        self.register_new_blob(&hash, size);
        Ok(hash)
    }

    /// 从文件导入到热存储，并注册分层元数据。
    pub fn import_file(&self, source: &Path) -> Result<(ContentHash, u64), TieringError> {
        let (hash, size) = self.hot_store.import_file_sync(source)?;
        self.register_new_blob(&hash, size);
        Ok((hash, size))
    }

    fn register_new_blob(&self, hash: &ContentHash, size: u64) {
        let mut meta = self.metadata.write();
        meta.entry(hash.clone()).or_insert_with(|| TierMetadata {
            hash: hash.clone(),
            tier: StorageTier::Hot,
            last_access_ms: current_millis(),
            access_count: 1,
            size_bytes: size,
        });
    }

    /// 记录一次访问（更新热度）
    pub fn record_access(&self, hash: &ContentHash) {
        let mut meta = self.metadata.write();
        if let Some(entry) = meta.get_mut(hash) {
            entry.last_access_ms = current_millis();
            entry.access_count += 1;
        } else {
            // 第一次访问，创建元数据
            let size = self
                .hot_store
                .meta(hash)
                .or_else(|_| self.cold_store.meta(hash))
                .map(|(s, _)| s)
                .unwrap_or(0);

            let tier = if self.hot_store.has_complete(hash) {
                StorageTier::Hot
            } else {
                StorageTier::Cold
            };

            meta.insert(
                hash.clone(),
                TierMetadata {
                    hash: hash.clone(),
                    tier,
                    last_access_ms: current_millis(),
                    access_count: 1,
                    size_bytes: size,
                },
            );
        }
    }

    /// 获取 blob 所在的层级
    pub fn get_tier(&self, hash: &ContentHash) -> Option<StorageTier> {
        self.metadata.read().get(hash).map(|m| m.tier)
    }

    /// 读取 blob（自动处理冷热数据）
    pub fn read_blob(&self, hash: &ContentHash) -> Result<Vec<u8>, TieringError> {
        self.record_access(hash);

        let tier = self.get_tier(hash).unwrap_or(StorageTier::Hot);

        match tier {
            StorageTier::Hot => {
                let (size, count) = self
                    .hot_store
                    .meta(hash)
                    .map_err(|_| TieringError::BlobNotFound(hash.to_string()))?;
                let mut buf = Vec::with_capacity(size as usize);
                for i in 0..count {
                    let chunk = self.hot_store.read_chunk_sync(hash, i)?;
                    buf.extend_from_slice(&chunk);
                }
                Ok(buf)
            }
            StorageTier::Cold => {
                // 冷数据被访问 → 自动回温到热存储
                info!(hash = %hash, "冷数据被访问，自动回温到热存储");
                self.promote_to_hot(hash)?;

                let (size, count) = self
                    .hot_store
                    .meta(hash)
                    .map_err(|_| TieringError::BlobNotFound(hash.to_string()))?;
                let mut buf = Vec::with_capacity(size as usize);
                for i in 0..count {
                    let chunk = self.hot_store.read_chunk_sync(hash, i)?;
                    buf.extend_from_slice(&chunk);
                }
                Ok(buf)
            }
        }
    }

    /// 将数据提升到热存储
    ///
    /// **安全顺序**：先把数据完整写入热存储并通过端到端哈希验证，
    /// 成功后才更新元数据。冷存储副本始终保留——回温是"复制"而
    /// 非"移动"，一旦写热存储失败，冷存储数据不受影响。
    fn promote_to_hot(&self, hash: &ContentHash) -> Result<(), TieringError> {
        if self.hot_store.verify_complete(hash) {
            return Ok(()); // 已经在热存储中且完好
        }

        // 从冷存储复制到热存储
        let (size, count) = self
            .cold_store
            .meta(hash)
            .map_err(|e| TieringError::MigrationFailed(format!("读取冷存储元数据失败: {e}")))?;

        copy_blob_between_stores(&self.cold_store, &self.hot_store, hash, count)?;

        // 端到端校验：拷贝完成后必须能通过完整哈希验证，
        // 否则宁可保留冷存储副本、放弃这次回温。
        if !self.hot_store.verify_complete(hash) {
            let _ = std::fs::remove_dir_all(self.hot_store.blob_dir(hash));
            return Err(TieringError::MigrationFailed(format!(
                "回温后哈希校验失败: {hash}"
            )));
        }

        // 更新元数据
        let mut meta_map = self.metadata.write();
        if let Some(entry) = meta_map.get_mut(hash) {
            entry.tier = StorageTier::Hot;
        }

        info!(hash = %hash, size_mb = size / (1024 * 1024), "数据已提升到热存储");
        Ok(())
    }

    /// 将数据降级到冷存储
    ///
    /// **安全顺序**：先确保冷存储持有一份经过校验的完整副本，
    /// 只有校验通过后才删除热存储副本。任何步骤失败都不会删除
    /// 热存储数据，避免出现"两边都没有"的数据丢失窗口。
    fn demote_to_cold(&self, hash: &ContentHash) -> Result<(), TieringError> {
        if !self.cold_store.verify_complete(hash) {
            // 冷存储没有健康副本 → 从热存储复制过去
            let (_size, count) = self
                .hot_store
                .meta(hash)
                .map_err(|e| TieringError::MigrationFailed(format!("读取热存储元数据失败: {e}")))?;

            copy_blob_between_stores(&self.hot_store, &self.cold_store, hash, count)?;

            // 端到端校验：冷存储副本必须完整可信，否则拒绝删除热存储数据。
            if !self.cold_store.verify_complete(hash) {
                let _ = std::fs::remove_dir_all(self.cold_store.blob_dir(hash));
                return Err(TieringError::MigrationFailed(format!(
                    "降温后冷存储哈希校验失败，保留热存储副本: {hash}"
                )));
            }
        }

        // 冷存储副本已确认健康——现在才能安全删除热存储副本。
        let size = self.hot_store.meta(hash).map(|(s, _)| s).unwrap_or(0);
        let hot_dir = self.hot_store.blob_dir(hash);
        if hot_dir.exists() {
            std::fs::remove_dir_all(&hot_dir)?;
        }

        // 更新元数据
        let mut meta_map = self.metadata.write();
        if let Some(entry) = meta_map.get_mut(hash) {
            entry.tier = StorageTier::Cold;
        }

        info!(hash = %hash, size_mb = size / (1024 * 1024), "数据已降级到冷存储");
        Ok(())
    }

    /// 执行一次冷热数据迁移扫描
    pub fn scan_and_migrate(&self) -> Result<MigrationStats, TieringError> {
        let now_ms = current_millis();
        let threshold_ms = self.policy.cold_threshold_days as u64 * 86_400_000;

        let mut stats = MigrationStats::default();

        // 计算热存储当前使用量
        let hot_usage = self.hot_store.total_size()?;
        stats.hot_usage_bytes = hot_usage;

        // 收集需要迁移的候选数据
        let mut candidates: Vec<(ContentHash, u64, u64)> = Vec::new();

        {
            let meta = self.metadata.read();
            for entry in meta.values() {
                if entry.tier == StorageTier::Hot {
                    let age_ms = now_ms.saturating_sub(entry.last_access_ms);

                    // 策略 1: 超过阈值天数未访问
                    if age_ms > threshold_ms {
                        candidates.push((
                            entry.hash.clone(),
                            entry.last_access_ms,
                            entry.size_bytes,
                        ));
                    }
                }
            }
        }

        // 策略 2: 如果热存储超过配额，强制迁移最冷的数据
        if hot_usage > self.policy.hot_quota_bytes {
            let quota_exceeded = hot_usage - self.policy.hot_quota_bytes;
            info!(
                hot_usage_gb = hot_usage / (1024 * 1024 * 1024),
                quota_gb = self.policy.hot_quota_bytes / (1024 * 1024 * 1024),
                exceeded_gb = quota_exceeded / (1024 * 1024 * 1024),
                "热存储超过配额，强制迁移最冷数据"
            );

            // 按最后访问时间排序，最老的优先迁移
            let meta = self.metadata.read();
            let mut all_hot: Vec<_> = meta
                .values()
                .filter(|e| e.tier == StorageTier::Hot)
                .collect();
            all_hot.sort_by_key(|e| e.last_access_ms);

            let mut freed = 0u64;
            for entry in all_hot {
                if freed >= quota_exceeded {
                    break;
                }
                if !candidates.iter().any(|(h, _, _)| h == &entry.hash) {
                    candidates.push((entry.hash.clone(), entry.last_access_ms, entry.size_bytes));
                    freed += entry.size_bytes;
                }
            }
        }

        // 执行迁移
        for (hash, _last_access, size) in candidates {
            match self.demote_to_cold(&hash) {
                Ok(()) => {
                    stats.migrated_to_cold += 1;
                    stats.migrated_bytes += size;
                }
                Err(e) => {
                    warn!(hash = %hash, error = %e, "迁移到冷存储失败");
                    stats.migration_errors += 1;
                }
            }
        }

        // 持久化元数据
        self.persist_metadata()?;

        info!(
            migrated_to_cold = stats.migrated_to_cold,
            migrated_mb = stats.migrated_bytes / (1024 * 1024),
            errors = stats.migration_errors,
            "冷热数据迁移扫描完成"
        );

        Ok(stats)
    }

    /// 持久化元数据
    fn persist_metadata(&self) -> std::io::Result<()> {
        let meta = self.metadata.read();
        let json = serde_json::to_string_pretty(&*meta)?;
        let tmp = self.meta_path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &self.meta_path)?;
        Ok(())
    }

    /// 启动后台扫描任务
    pub fn start_background_scan(
        self: Arc<Self>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let interval = self.policy.scan_interval;
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            info!(?interval, "冷热数据迁移后台任务已启动");

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        let service = self.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            if let Err(e) = service.scan_and_migrate() {
                                warn!(error = %e, "冷热数据扫描失败");
                            }
                        }).await;
                    }
                    _ = shutdown.changed() => {
                        if *shutdown.borrow() {
                            info!("冷热数据迁移后台任务已停止");
                            return;
                        }
                    }
                }
            }
        })
    }

    /// 获取统计信息
    pub fn stats(&self) -> TieringStats {
        let meta = self.metadata.read();
        let mut stats = TieringStats::default();

        for entry in meta.values() {
            match entry.tier {
                StorageTier::Hot => {
                    stats.hot_blobs += 1;
                    stats.hot_bytes += entry.size_bytes;
                }
                StorageTier::Cold => {
                    stats.cold_blobs += 1;
                    stats.cold_bytes += entry.size_bytes;
                }
            }
        }

        stats
    }
}

#[derive(Debug, Default)]
pub struct MigrationStats {
    pub hot_usage_bytes: u64,
    pub migrated_to_cold: usize,
    pub migrated_bytes: u64,
    pub migration_errors: usize,
}

#[derive(Debug, Default)]
pub struct TieringStats {
    pub hot_blobs: usize,
    pub hot_bytes: u64,
    pub cold_blobs: usize,
    pub cold_bytes: u64,
}

impl TieringStats {
    pub fn total_blobs(&self) -> usize {
        self.hot_blobs + self.cold_blobs
    }

    pub fn total_bytes(&self) -> u64 {
        self.hot_bytes + self.cold_bytes
    }

    pub fn hot_ratio(&self) -> f64 {
        if self.total_bytes() == 0 {
            0.0
        } else {
            self.hot_bytes as f64 / self.total_bytes() as f64
        }
    }
}

fn current_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 在两个 [`BlobStore`] 之间复制一个 blob 的全部 chunk + meta +
/// complete 标志。纯粹的字节搬运，调用方负责在复制前后做
/// 完整性校验（`verify_complete`）。
fn copy_blob_between_stores(
    src: &BlobStore,
    dst: &BlobStore,
    hash: &ContentHash,
    count: u32,
) -> Result<(), TieringError> {
    let dest_dir = dst.blob_dir(hash);
    std::fs::create_dir_all(dest_dir.join("chunks"))?;

    let (size, _) = src
        .meta(hash)
        .map_err(|e| TieringError::MigrationFailed(format!("读取源元数据失败: {e}")))?;

    for i in 0..count {
        let chunk = src.read_chunk_sync(hash, i)?;
        let chunk_path = dest_dir.join("chunks").join(format!("{i:06}"));
        std::fs::write(&chunk_path, &chunk)?;
        std::fs::write(
            chunk_path.with_extension("sha"),
            blake3::hash(&chunk).to_hex().as_bytes(),
        )?;
    }

    let meta = serde_json::json!({
        "hash": hash.as_hex(),
        "sizeBytes": size,
        "chunkCount": count,
    });
    let meta_bytes =
        serde_json::to_vec(&meta).map_err(|e| TieringError::Serialization(e.to_string()))?;
    std::fs::write(dest_dir.join("meta.json"), meta_bytes)?;
    std::fs::write(dest_dir.join("complete"), b"1")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiering_metadata_serialization() {
        let meta = TierMetadata {
            hash: ContentHash::from_bytes(b"test"),
            tier: StorageTier::Hot,
            last_access_ms: 1234567890,
            access_count: 42,
            size_bytes: 1024,
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: TierMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(meta.hash, back.hash);
        assert_eq!(meta.tier, back.tier);
    }

    #[test]
    fn tiering_stats_ratios() {
        let mut stats = TieringStats::default();
        stats.hot_bytes = 100 * 1024 * 1024; // 100 MB
        stats.cold_bytes = 900 * 1024 * 1024; // 900 MB
        assert_eq!(stats.total_bytes(), 1000 * 1024 * 1024);
        assert!((stats.hot_ratio() - 0.1).abs() < 0.01);
    }

    fn make_service(dir: &Path, policy: TieringPolicy) -> TieredStorageService {
        TieredStorageService::new(&dir.join("hot"), policy).unwrap()
    }

    /// `put_bytes` 必须让新 blob 立即对 `stats()` / `get_tier` 可见，
    /// 否则 `scan_and_migrate` 永远不会考虑迁移它。
    #[test]
    fn put_bytes_registers_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let policy = TieringPolicy {
            cold_storage_path: dir.path().join("cold"),
            ..Default::default()
        };
        let service = make_service(dir.path(), policy);
        let hash = service.put_bytes(b"hello tiering").unwrap();
        assert_eq!(service.get_tier(&hash), Some(StorageTier::Hot));
        let stats = service.stats();
        assert_eq!(stats.hot_blobs, 1);
        assert_eq!(stats.hot_bytes, 13);
    }

    /// 降温后再回温必须能拿回原始字节，且经过端到端哈希校验。
    #[test]
    fn demote_then_promote_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let policy = TieringPolicy {
            cold_storage_path: dir.path().join("cold"),
            ..Default::default()
        };
        let service = make_service(dir.path(), policy);
        let data = b"roundtrip payload for tiering";
        let hash = service.put_bytes(data).unwrap();

        service.demote_to_cold(&hash).unwrap();
        assert_eq!(service.get_tier(&hash), Some(StorageTier::Cold));
        assert!(!service.hot_store.has_complete(&hash));
        assert!(service.cold_store.verify_complete(&hash));

        // 读取时应自动回温并返回正确数据。
        let read_back = service.read_blob(&hash).unwrap();
        assert_eq!(read_back, data);
        assert_eq!(service.get_tier(&hash), Some(StorageTier::Hot));
    }

    /// `demote_to_cold` 在冷存储写入失败前绝不能删除热存储副本
    /// （否则会出现两边都没有数据的窗口）。这里通过把 cold_dir
    /// 的父目录设为只读文件来模拟“冷存储不可写”。
    #[cfg(unix)]
    #[test]
    fn demote_keeps_hot_copy_when_cold_write_fails() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let cold_path = dir.path().join("cold");
        let policy = TieringPolicy {
            cold_storage_path: cold_path.clone(),
            ..Default::default()
        };
        let service = make_service(dir.path(), policy);
        let hash = service.put_bytes(b"protect me").unwrap();

        // 让冷存储目录只读，使得后续在其下创建 blob 子目录失败。
        let mut perms = std::fs::metadata(&cold_path).unwrap().permissions();
        perms.set_mode(0o444);
        std::fs::set_permissions(&cold_path, perms).unwrap();

        let result = service.demote_to_cold(&hash);

        // 恢复权限，方便临时目录清理。
        let mut perms = std::fs::metadata(&cold_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&cold_path, perms).unwrap();

        assert!(
            result.is_err(),
            "demote should fail when cold store is unwritable"
        );
        assert!(
            service.hot_store.has_complete(&hash),
            "hot copy must survive a failed demote"
        );
    }

    /// 重启（重新构造 `TieredStorageService`）后，直接落在磁盘上但
    /// 未被上次元数据持久化记录的 blob 必须被自动发现，否则会永久
    /// 逃脱迁移扫描。
    #[test]
    fn restart_reconciles_untracked_hot_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let policy = TieringPolicy {
            cold_storage_path: dir.path().join("cold"),
            ..Default::default()
        };
        {
            let service = make_service(dir.path(), policy.clone());
            // 绕过服务直接写入热存储，模拟"上次持久化之前进程崩溃"。
            service.hot_store.put_bytes_sync(b"orphaned blob").unwrap();
        }
        let service = make_service(dir.path(), policy);
        let stats = service.stats();
        assert_eq!(
            stats.hot_blobs, 1,
            "orphaned blob must be reconciled on restart"
        );
    }
}
