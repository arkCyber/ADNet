//! Mock transport — an in-process "network" used to test the
//! replication algorithm without an iroh endpoint.
//!
//! This is the **test double** for [`ReplicatorTransport`]. It
//! keeps a per-peer receiver inbox and faithfully behaves like
//! a real network:
//! - **in-order delivery**, at most once per `push_block`.
//! - **byzantine injectors**: a peer can be flagged to always
//!   reject, to accept but corrupt, or to never respond. Used
//!   by the aerospace tests to inject failures.
//! - **record call history**: every push is recorded so tests
//!   can assert on traffic patterns.
//!
//! The transport is intentionally **stateless across the
//! receiver's verification** — the receiver always re-hashes
//! the inbound message and only commits if the digest matches
//! the declared block hash. That is the single safety barrier
//! against a byzantine sender.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

use a3net_types::ContentHash;

use crate::replicator::{
    NodeAddr, ReplicaAck, ReplicaMessage, ReplicatorError, ReplicatorTransport,
};

/// Persistent store on the receiver side. Mirrors the public
/// surface of `BlobStore` we need to verify a block landed.
pub trait ReceiverStore: std::fmt::Debug + Send + Sync {
    fn has(&self, blob: &ContentHash) -> bool;
    fn put_block(
        &self,
        blob: &ContentHash,
        block: &ContentHash,
        bytes: &[u8],
    ) -> Result<(), ReplicatorError>;
}

/// Behaviour injected via the per-peer flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PeerBehaviour {
    /// Default: re-hash, verify, ack.
    #[default]
    Honest,
    /// Byzanine: ack the message but the bytes we keep are
    /// unchanged — useful to trigger SR-1 verification on the
    /// *receiver* side. (Note: the test verifies dishonesty by
    /// comparing the block hash the receiver saw vs the bytes
    /// it stored — they MUST differ for a byzantine to be
    /// detected.)
    Dishonest,
    /// Always reject.
    Refusing,
    /// Pretend the peer is unreachable.
    Unreachable,
}

#[derive(Debug, Default)]
struct PeerEntry {
    behaviour: PeerBehaviour,
    delivered: Vec<ReplicaMessage>,
    blocks: HashMap<ContentHash, Vec<u8>>,
    blobs: HashMap<ContentHash, ()>,
}

/// A single "node" in the mock network, wrapped in `Arc` so the
/// same instance can be shared across the swarm.
#[derive(Debug, Clone)]
pub struct MockNode {
    pub id: NodeAddr,
    inner: Arc<Mutex<PeerEntry>>,
    pub byzantine_log: Arc<Mutex<Vec<(NodeAddr, ContentHash, ReplicatorError)>>>,
}

impl MockNode {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: NodeAddr::new(id),
            inner: Arc::new(Mutex::new(PeerEntry::default())),
            byzantine_log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn set_behaviour(&self, b: PeerBehaviour) {
        self.inner.lock().behaviour = b;
    }

    /// All blocks this node has accepted from the swarm.
    pub fn blocks(&self) -> HashMap<ContentHash, Vec<u8>> {
        self.inner.lock().blocks.clone()
    }

    /// All blob-hashes this node has accepted.
    pub fn blobs(&self) -> HashMap<ContentHash, ()> {
        self.inner.lock().blobs.clone()
    }

    /// All messages ever delivered to this node.
    pub fn delivered(&self) -> Vec<ReplicaMessage> {
        self.inner.lock().delivered.clone()
    }

    fn receive(&self, msg: ReplicaMessage) -> Result<ReplicaAck, ReplicatorError> {
        let mut g = self.inner.lock();
        // Check behaviour first - unreachable peers don't deliver.
        match g.behaviour {
            PeerBehaviour::Unreachable => {
                return Err(ReplicatorError::Transport("unreachable".into()));
            }
            PeerBehaviour::Refusing => {
                return Err(ReplicatorError::Refused("refusing".into()));
            }
            PeerBehaviour::Honest | PeerBehaviour::Dishonest => {}
        }
        // Record the delivered message.
        g.delivered.push(msg.clone());
        // Re-hash the bytes (always — this is the SR-1
        // boundary on the receiver side).
        let actual = ContentHash::from_bytes(&msg.bytes);
        if actual != msg.block {
            // Byzanine-flavored: log the attempt and refuse.
            let sender = self.id.clone();
            let block = msg.block.clone();
            let actual_for_log = actual.clone();
            drop(g);
            self.byzantine_log.lock().push((
                sender,
                block.clone(),
                ReplicatorError::HashMismatch {
                    expected: block,
                    actual: actual_for_log,
                },
            ));
            return Err(ReplicatorError::HashMismatch {
                expected: msg.block.clone(),
                actual,
            });
        }
        // Commit.
        g.blocks.insert(msg.block.clone(), msg.bytes.clone());
        g.blobs.insert(msg.blob.clone(), ());
        Ok(ReplicaAck {
            blob: msg.blob,
            block: msg.block,
            received_bytes: msg.bytes.len() as u64,
        })
    }
}

/// The mock transport — every sender has one, and it knows
/// about every receiver in the swarm so it can pick destinations.
#[derive(Debug, Clone)]
pub struct MockTransport {
    pub self_node: NodeAddr,
    nodes: Arc<Mutex<HashMap<String, MockNode>>>,
}

impl MockTransport {
    pub fn new(self_node: impl Into<String>) -> Self {
        Self {
            self_node: NodeAddr::new(self_node),
            nodes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn register(&self, node: &MockNode) {
        self.nodes.lock().insert(node.id.0.clone(), node.clone());
    }

    pub fn node(&self, id: &str) -> Option<MockNode> {
        self.nodes.lock().get(id).cloned()
    }

    /// All registered peers EXCEPT ourselves.
    pub fn peers(&self) -> Vec<MockNode> {
        let g = self.nodes.lock();
        g.values()
            .filter(|n| n.id != self.self_node)
            .cloned()
            .collect()
    }

    /// Network-wide view: how many of `nodes` have a given
    /// block? Returns the count.
    pub fn replicated_count(&self, _blob: &ContentHash, block: &ContentHash) -> usize {
        self.nodes
            .lock()
            .values()
            .filter(|n| n.inner.lock().blocks.contains_key(block))
            .count()
    }

    /// Disconnect a peer — make it unreachable.
    pub fn drop_peer(&self, id: &str) {
        if let Some(n) = self.nodes.lock().get(id).cloned() {
            n.set_behaviour(PeerBehaviour::Unreachable);
        }
    }

    /// Re-attach a previously-dropped peer.
    pub fn restore_peer(&self, id: &str) {
        if let Some(n) = self.nodes.lock().get(id).cloned() {
            n.set_behaviour(PeerBehaviour::Honest);
        }
    }
}

#[async_trait::async_trait]
impl ReplicatorTransport for MockTransport {
    async fn push_block(
        &self,
        peer: &NodeAddr,
        msg: ReplicaMessage,
    ) -> Result<ReplicaAck, ReplicatorError> {
        let node = self
            .nodes
            .lock()
            .get(peer.as_str())
            .cloned()
            .ok_or_else(|| ReplicatorError::Transport(format!("unknown peer {}", peer.as_str())))?;
        node.receive(msg)
    }

    async fn ping(&self, peer: &NodeAddr) -> Result<(), ReplicatorError> {
        let node = self
            .nodes
            .lock()
            .get(peer.as_str())
            .cloned()
            .ok_or_else(|| ReplicatorError::Transport(format!("unknown peer {}", peer.as_str())))?;
        match node.inner.lock().behaviour {
            PeerBehaviour::Unreachable => Err(ReplicatorError::Transport("unreachable".into())),
            _ => Ok(()),
        }
    }

    async fn get_ec_meta(
        &self,
        peer: &NodeAddr,
        content_hash: &ContentHash,
    ) -> Result<Option<Vec<u8>>, ReplicatorError> {
        let node = self
            .nodes
            .lock()
            .get(peer.as_str())
            .cloned()
            .ok_or_else(|| ReplicatorError::Transport(format!("unknown peer {}", peer.as_str())))?;

        // Check if peer has this content
        let has_content = node.inner.lock().blobs.contains_key(content_hash);
        if !has_content {
            return Ok(None);
        }

        // In a full implementation, this would request actual metadata
        // For the mock, we return a placeholder that can be detected
        let placeholder = serde_json::json!({
            "type": "ec_meta_request",
            "content_hash": content_hash.to_string(),
        });
        Ok(Some(serde_json::to_vec(&placeholder).unwrap_or_default()))
    }

    async fn get_shard(
        &self,
        peer: &NodeAddr,
        content_hash: &ContentHash,
        _shard_index: u8,
    ) -> Result<Option<Vec<u8>>, ReplicatorError> {
        let node = self
            .nodes
            .lock()
            .get(peer.as_str())
            .cloned()
            .ok_or_else(|| ReplicatorError::Transport(format!("unknown peer {}", peer.as_str())))?;

        // Check if peer has this content
        let has_content = node.inner.lock().blobs.contains_key(content_hash);
        if !has_content {
            return Ok(None);
        }

        // In a full implementation, this would return the actual shard data
        // For the mock, we return a placeholder
        let placeholder = serde_json::json!({
            "type": "shard_request",
            "content_hash": content_hash.to_string(),
        });
        Ok(Some(serde_json::to_vec(&placeholder).unwrap_or_default()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(blob: &[u8], block: &[u8]) -> ReplicaMessage {
        ReplicaMessage {
            blob: ContentHash::from_bytes(blob),
            block: ContentHash::from_bytes(block),
            index: 0,
            bytes: block.to_vec(),
        }
    }

    #[tokio::test]
    async fn push_block_to_honest_peer_succeeds() {
        let t = MockTransport::new("a");
        let b = MockNode::new("b");
        b.set_behaviour(PeerBehaviour::Honest);
        t.register(&b);
        let r = t
            .push_block(&b.id, msg(b"blob", b"block-bytes"))
            .await
            .unwrap();
        assert_eq!(r.received_bytes, b"block-bytes".len() as u64);
    }

    #[tokio::test]
    async fn push_block_to_refusing_peer_errors() {
        let t = MockTransport::new("a");
        let b = MockNode::new("b");
        b.set_behaviour(PeerBehaviour::Refusing);
        t.register(&b);
        let err = t
            .push_block(&b.id, msg(b"blob", b"block"))
            .await
            .unwrap_err();
        assert!(matches!(err, ReplicatorError::Refused(_)));
    }

    #[tokio::test]
    async fn push_block_to_unreachable_peer_errors() {
        let t = MockTransport::new("a");
        let b = MockNode::new("b");
        b.set_behaviour(PeerBehaviour::Unreachable);
        t.register(&b);
        let err = t
            .push_block(&b.id, msg(b"blob", b"block"))
            .await
            .unwrap_err();
        assert!(matches!(err, ReplicatorError::Transport(_)));
    }

    #[tokio::test]
    async fn push_block_to_unknown_peer_errors() {
        let t = MockTransport::new("a");
        let err = t
            .push_block(&NodeAddr::new("nope"), msg(b"blob", b"block"))
            .await
            .unwrap_err();
        assert!(matches!(err, ReplicatorError::Transport(_)));
    }

    #[tokio::test]
    async fn push_block_with_wrong_hash_is_rejected() {
        let t = MockTransport::new("a");
        let b = MockNode::new("b");
        b.set_behaviour(PeerBehaviour::Honest);
        t.register(&b);
        // Build a message that lies about the block hash.
        let mut bad = msg(b"blob", b"block-bytes");
        bad.block = ContentHash::from_bytes(b"DIFFERENT-HASH");
        let err = t.push_block(&b.id, bad).await.unwrap_err();
        assert!(matches!(err, ReplicatorError::HashMismatch { .. }));
        // The receiver did NOT commit the block.
        assert!(b.blocks().is_empty());
    }

    #[tokio::test]
    async fn concurrent_pushes_to_honest_peer_succeed() {
        let t = MockTransport::new("a");
        let b = MockNode::new("b");
        b.set_behaviour(PeerBehaviour::Honest);
        t.register(&b);
        let mut handles = vec![];
        for i in 0..16u8 {
            let t = t.clone();
            let b_id = b.id.clone();
            handles.push(tokio::spawn(async move {
                t.push_block(
                    &b_id,
                    ReplicaMessage {
                        blob: ContentHash::from_bytes(b"blob"),
                        block: ContentHash::from_bytes(&[i]),
                        index: 0,
                        bytes: vec![i],
                    },
                )
                .await
            }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }
        assert_eq!(b.blocks().len(), 16);
    }
}
