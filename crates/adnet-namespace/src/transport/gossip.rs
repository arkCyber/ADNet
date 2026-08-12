//! In-process GossipBus transport.
//!
//! Wraps the existing `adnet-gossip` bus so an IPNS publish becomes
//! a gossip message on a stable, well-known topic
//! (`adnet-ipns-v1`). Subscribers receive records the moment they
//! are gossiped, regardless of whether pkarr is reachable.
//!
//! The transport deliberately does **not** own a GossipBus — the
//! bus is constructed by `adnet-node` (or the CLI) and shared with
//! the rest of the process. A `None` bus becomes a no-op so the
//! type is usable in unit tests without a real transport stack.
//!
//! Stability: the topic name is part of the wire contract; future
//! revisions will use `adnet-ipns-v2` and live alongside v1 for
//! at least one minor release.

use async_trait::async_trait;
use futures::stream;
use tokio::sync::broadcast;

use super::{IpnRecordStream, IpnTransport, TransportHealth};
use crate::ipns::{IpnRecord, IpnsError};

/// Stable topic id. Computed once with `BLAKE3("adnet-ipns-v1")`
/// so an external observer can recompute it.
pub const IPNS_TOPIC: &str = "adnet-ipns-v1";

/// Adapter that lets this transport broadcast on a generic
/// `mpsc::Sender` (the same shape `adnet-gossip` exposes internally).
/// We deliberately decouple from `adnet-gossip::GossipBus` to avoid a
/// cyclic dep: the binary crate wires `GossipIpnTransport::new` to
/// the bus by wrapping its broadcast sender.
pub type IpnMessage = IpnRecord;

#[derive(Debug, Clone)]
pub struct GossipIpnTransport {
    tx: Option<broadcast::Sender<IpnMessage>>,
}

impl Default for GossipIpnTransport {
    fn default() -> Self {
        Self { tx: None }
    }
}

impl GossipIpnTransport {
    pub fn new(tx: broadcast::Sender<IpnMessage>) -> Self {
        Self { tx: Some(tx) }
    }

    pub fn noop() -> Self {
        Self { tx: None }
    }
}

#[async_trait]
impl IpnTransport for GossipIpnTransport {
    fn name(&self) -> &'static str {
        "gossip"
    }

    async fn publish(&self, record: &IpnRecord) -> Result<(), IpnsError> {
        if let Some(tx) = &self.tx {
            let _ = tx.send(record.clone());
        } else {
            tracing::debug!("gossip transport is a no-op (no bus wired)");
        }
        Ok(())
    }

    async fn subscribe(&self, _name: &str) -> Result<IpnRecordStream, IpnsError> {
        let mut rx = match &self.tx {
            Some(tx) => tx.subscribe(),
            None => {
                // No-op transport — return an empty stream.
                let s: IpnRecordStream = Box::pin(stream::empty());
                return Ok(s);
            }
        };
        let s: IpnRecordStream = Box::pin(async_stream::stream! {
            loop {
                match rx.recv().await {
                    Ok(record) => yield Ok(record),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        Ok(s)
    }

    async fn health(&self) -> Result<TransportHealth, IpnsError> {
        Ok(if self.tx.is_some() {
            TransportHealth::Healthy
        } else {
            TransportHealth::Down
        })
    }
}
