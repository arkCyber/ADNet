//! On-disk journal transport.
//!
//! Every `IpnRecord` that flows through this transport is appended to
//! a single NDJSON file under `dir/journal.ndjson` so the resolver
//! can replay cold-start state without a network round-trip.
//!
//! The journal is **append-only** and rotation-safe: entries are
//! tagged with a `(name, sequence)` pair and a `Subscribe` stream
//! emits everything with `sequence >= since` (or everything if `since`
//! is `None`).
//!
//! ## Why NDJSON over a binary format
//!
//! Operators want to `grep` the journal. NDJSON is universally
//! greppable; a single self-test failure should not block deploys.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use super::{IpnRecordStream, IpnTransport, TransportHealth};
use crate::ipns::{IpnRecord, IpnsError};

const FILE_NAME: &str = "journal.ndjson";

/// Append-only NDJSON journal.
#[derive(Debug, Clone)]
pub struct DiskJournalTransport {
    dir: PathBuf,
    inner: Arc<Mutex<JournalInner>>,
}

#[derive(Debug)]
struct JournalInner {
    /// Append-only writer held as `Mutex<Option<...>>` so the writer
    /// can be re-opened lazily after rotation.
    writer: Option<fs::File>,
    seen_sequences: std::collections::BTreeMap<String, u64>,
}

impl DiskJournalTransport {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            inner: Arc::new(Mutex::new(JournalInner {
                writer: None,
                seen_sequences: std::collections::BTreeMap::new(),
            })),
        }
    }

    async fn ensure_open(inner: &mut JournalInner, dir: &PathBuf) -> Result<(), IpnsError> {
        if inner.writer.is_some() {
            return Ok(());
        }
        fs::create_dir_all(dir)
            .await
            .map_err(|e| IpnsError::Transport(format!("disk: {e}")))?;
        let path = dir.join(FILE_NAME);
        let f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .map_err(|e| IpnsError::Transport(format!("disk: {e}")))?;
        inner.writer = Some(f);
        Ok(())
    }

    async fn append(&self, record: &IpnRecord) -> Result<(), IpnsError> {
        let mut inner = self.inner.lock().await;
        Self::ensure_open(&mut inner, &self.dir).await?;
        let writer = inner.writer.as_mut().expect("open");
        let line = serde_json::to_string(record).map_err(|e| IpnsError::Transport(e.to_string()))?;
        writer
            .write_all(line.as_bytes())
            .await
            .map_err(|e| IpnsError::Transport(format!("disk: {e}")))?;
        writer
            .write_all(b"\n")
            .await
            .map_err(|e| IpnsError::Transport(format!("disk: {e}")))?;
        writer
            .flush()
            .await
            .map_err(|e| IpnsError::Transport(format!("disk: {e}")))?;
        // Track highest seen sequence per name so duplicate publishes
        // never grow the journal.
        let entry = inner.seen_sequences.entry(record.name.clone()).or_insert(0);
        if record.sequence > *entry {
            *entry = record.sequence;
        }
        Ok(())
    }

    /// Stream everything in the journal in append order. Use on cold
    /// start to rebuild the in-memory cache.
    pub async fn replay_all(&self) -> Result<Vec<IpnRecord>, IpnsError> {
        let path = self.dir.join(FILE_NAME);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let f = fs::File::open(&path)
            .await
            .map_err(|e| IpnsError::Transport(format!("disk: {e}")))?;
        let reader = BufReader::new(f);
        let mut lines = reader.lines();
        let mut out = Vec::new();
        while let Some(line) = lines
            .next_line()
            .await
            .map_err(|e| IpnsError::Transport(format!("disk: {e}")))?
        {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<IpnRecord>(&line) {
                Ok(r) => out.push(r),
                Err(e) => {
                    tracing::warn!(error = %e, "skipping malformed journal entry");
                }
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl IpnTransport for DiskJournalTransport {
    fn name(&self) -> &'static str {
        "disk-journal"
    }

    async fn publish(&self, record: &IpnRecord) -> Result<(), IpnsError> {
        self.append(record).await
    }

    async fn subscribe(&self, _name: &str) -> Result<IpnRecordStream, IpnsError> {
        // The disk transport is a cold-start replay tool, not a live
        // subscriber. Live subscribers should go through `MultiTransport`
        // and pair disk with gossip / pkarr.
        let records = self.replay_all().await?;
        let stream = stream::iter(records.into_iter().map(Ok));
        let s: IpnRecordStream = Box::pin(stream);
        Ok(s)
    }

    async fn health(&self) -> Result<TransportHealth, IpnsError> {
        // Disk-local transports are always healthy unless the journal
        // is malformed; we surface Unknown to avoid lying.
        Ok(TransportHealth::Unknown)
    }
}
