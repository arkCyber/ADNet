//! `crates/adnet-namespace/src/transport/mod.rs`
//!
//! Pluggable IPNS record transport backends.
//!
//! Each transport implements [`IpnTransport`] and provides publish / subscribe
//! semantics for an [`IpnRecord`]. The default `MultiTransport` chains
//! several of them so a record published locally also reaches:
//!
//!   * the **Pkarr** relay (DNS-TXT based, federated with irp / pkarr.pub)
//!   * the in-process **GossipBus** topic (Pubsub fallback for users in
//!     the same room / swarm)
//!   * the local **DiskJournal** (durable replay on cold start)
//!
//! Splitting the transport from the record format makes the IPNS name
//! layer transport-agnostic — we are free to add a DHT transport, a
//! web-push transport, etc. in subsequent PRs without touching the
//! record codec.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod disk;
pub mod gossip;
pub mod multi;
pub mod pkarr;
#[cfg(feature = "dht")]
pub mod dht;

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;
use tokio::sync::broadcast;

use crate::ipns::{IpnRecord, IpnsError};

/// Stream of inbound records.
pub type IpnRecordStream =
    Pin<Box<dyn Stream<Item = Result<IpnRecord, IpnsError>> + Send + 'static>>;

/// A single end of an IPNS transport.
///
/// `publish` is idempotent at the protocol level: publishing the same
/// `(name, sequence)` twice produces a single observable record on the
/// resolver side (sequence tiebreak keeps the newest). `subscribe`
/// returns a stream that yields **only** events from the moment of
/// subscription forward — it is not a replay log. Callers that want
/// replay should pair a transport with [`disk::DiskJournalTransport`].
#[async_trait]
pub trait IpnTransport: Send + Sync {
    /// Stable backend identifier (used for diagnostics, metrics labels).
    fn name(&self) -> &'static str;

    /// Publish a record. Implementations should treat this as
    /// "best-effort fanout": failures are logged but should not panic.
    async fn publish(&self, record: &IpnRecord) -> Result<(), IpnsError>;

    /// Subscribe to inbound records for the given name.
    async fn subscribe(&self, name: &str) -> Result<IpnRecordStream, IpnsError>;

    /// Resolve a record from the network now (not from local cache).
    /// Returns `Ok(record)` if found, `Err(IpnsError::NotFound)` if not found.
    async fn resolve_now(&self, name: &str) -> Result<IpnRecord, IpnsError> {
        Err(IpnsError::NotFound)
    }

    /// Best-effort liveness check; `None` means the transport has no
    /// natural health probe.
    async fn health(&self) -> Result<TransportHealth, IpnsError> {
        Ok(TransportHealth::Unknown)
    }
}

/// Health snapshot returned by `IpnTransport::health`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportHealth {
    Healthy,
    Degraded,
    Down,
    Unknown,
}

/// Shared broadcast bus used by transports that want to notify in-process
/// listeners (and the gossip fallback in particular).
#[derive(Debug, Clone)]
pub struct SharedIpnBus {
    tx: broadcast::Sender<IpnRecord>,
}

impl SharedIpnBus {
    pub fn new(capacity: usize) -> Self {
        Self {
            tx: broadcast::channel(capacity.max(1)).0,
        }
    }

    pub fn publish(&self, record: &IpnRecord) -> Result<(), IpnsError> {
        // 0-listener errors are expected when nothing is subscribed, swallow.
        let _ = self.tx.send(record.clone());
        Ok(())
    }

    pub fn subscribe(&self) -> IpnRecordStream {
        let mut rx = self.tx.subscribe();
        Box::pin(async_stream::stream! {
            loop {
                match rx.recv().await {
                    Ok(record) => yield Ok(record),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(lagged = n, "IPN transport bus lagged");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    }

    /// Get the broadcast sender; used by the binary that owns the
    /// shared bus so other transports (gossip, custom push) can hook
    /// into it.
    pub fn sender(&self) -> broadcast::Sender<IpnRecord> {
        self.tx.clone()
    }
}

/// Convenience constructor — returns the platform-default transport
/// chain (Disk + Pkarr + Gossip). Intended for `adnet-node` and the
/// CLI. The Gossip layer is omitted here because it needs a live
/// `GossipBus`; call [`gossip::GossipIpnTransport::new`] directly and
/// push it into the returned vec.
pub fn default_transports(
    disk_dir: Option<std::path::PathBuf>,
) -> Vec<Arc<dyn IpnTransport>> {
    let mut out: Vec<Arc<dyn IpnTransport>> = Vec::new();

    // Disk transport first so subsequent transports see a consistent
    // replayed journal before live events.
    if let Some(dir) = disk_dir {
        out.push(Arc::new(disk::DiskJournalTransport::new(dir)));
    }

    // Pkarr transport always-on; the relay list is hard-coded defaults
    // but `PkarrTransport::with_relays` allows override.
    out.push(Arc::new(pkarr::PkarrTransport::default()));

    out
}
