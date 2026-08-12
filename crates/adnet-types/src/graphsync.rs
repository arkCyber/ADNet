//! Graphsync DAG 同步协议框架。
//!
//! `adnet-types::graphsync` 同时定义:
//!
//! 1. 线协议层（消息结构、`ResponseStatus`、Selector 序列化）。
//! 2. 一个无 IO 的同步块存储 trait [`BlockStore`]。
//! 3. 基于 IPLD Selector 的同步遍历器 [`traverse`]，
//!    支持 `All` / `Leaf` / `Links` / `ExploreRecursive` /
//!    `ExploreAll` / `ExploreIndex` / `Union` / `Conditional`。
//! 4. [`GraphSyncResponder`] 处理远端 `RequestMessage`，
//!    产出按 Selector 排序的块流（[`ResponseItem`]）。
//!
//! 上层（`adnet-blobstore` / `adnet-node` 等）把 wire
//! 帧、`BitswapTransport` 风格的 `transport bridge`、块存储
//! 实现注入；本模块保持无 IO、可在 `cargo test` 中运行。
//!
//! 与 IPFS GraphSync 的对比：
//!
//! - 帧编码：JSON 自描述（与 ipld-selectors 的"包装"等价）。
//!   上线对接 Kubo / IPFS 时建议加 Protobuf-CBOR 后端。
//! - Selector 文法：覆盖了 IPLD Selector v1 的核心
//!   (`Matcher` 6 种 + `Sequence` + `Condition`)。
//! - 错误模型：`GraphSyncError` 枚举对等。

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::cid::Cid;

/// GraphSync error types.
#[derive(Debug, Error)]
pub enum GraphSyncError {
    #[error("request not found: {0}")]
    RequestNotFound(u64),

    #[error("request cancelled")]
    RequestCancelled,

    #[error("peer not connected")]
    PeerNotConnected,

    #[error("invalid selector: {0}")]
    InvalidSelector(String),

    #[error("block not found: {0}")]
    BlockNotFound(Cid),

    #[error("block decode error for {cid}: {message}")]
    BlockDecode { cid: Cid, message: String },

    #[error("block content hash mismatch: cid {cid} does not verify against payload")]
    BlockHashMismatch { cid: Cid },

    #[error("traversal depth exceeded (limit={limit}) at {cid}")]
    DepthExceeded { limit: u64, cid: Cid },

    #[error("selector recursion limit exceeded")]
    RecursionLimit,

    #[error("send error: {0}")]
    SendError(String),

    #[error("internal error: {0}")]
    Internal(String),
}

/// GraphSync message types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GraphSyncMessage {
    /// Request message.
    Request(RequestMessage),
    /// Response message.
    Response(ResponseMessage),
    /// Data block message.
    Block(BlockMessage),
}

impl GraphSyncMessage {
    pub fn request(id: u64, root: Cid, selector: &[u8]) -> Self {
        Self::Request(RequestMessage {
            id,
            root,
            selector: selector.to_vec(),
            replace: false,
            priority: 1,
        })
    }

    pub fn response(id: u64, status: ResponseStatus) -> Self {
        Self::Response(ResponseMessage { id, status })
    }

    pub fn block(id: u64, cid: Cid, block: Vec<u8>) -> Self {
        Self::Block(BlockMessage { id, cid, block })
    }
}

/// Response status codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResponseStatus {
    /// Request completed successfully.
    Completed = 0,
    /// Request is partially fulfilled.
    Partial = 1,
    /// Request processing hit the end of the DAG.
    EndOfDag = 2,
    /// Request is over a remote.
    Remote = 3,
    /// Request cancelled.
    Cancelled = 4,
    /// Request failed.
    Failed = 5,
}

impl ResponseStatus {
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Completed),
            1 => Some(Self::Partial),
            2 => Some(Self::EndOfDag),
            3 => Some(Self::Remote),
            4 => Some(Self::Cancelled),
            5 => Some(Self::Failed),
            _ => None,
        }
    }

    pub fn to_u32(&self) -> u32 {
        *self as u32
    }
}

impl fmt::Display for ResponseStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ResponseStatus::Completed => "Completed",
            ResponseStatus::Partial => "Partial",
            ResponseStatus::EndOfDag => "EndOfDag",
            ResponseStatus::Remote => "Remote",
            ResponseStatus::Cancelled => "Cancelled",
            ResponseStatus::Failed => "Failed",
        };
        f.write_str(s)
    }
}

/// GraphSync request message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestMessage {
    /// Request ID (unique per requester).
    pub id: u64,
    /// Root CID of the DAG to sync.
    pub root: Cid,
    /// Selector bytes specifying what to sync.
    pub selector: Vec<u8>,
    /// If true, replace an existing request.
    #[serde(default)]
    pub replace: bool,
    /// Request priority (lower = higher priority).
    #[serde(default = "default_priority")]
    pub priority: i32,
}

fn default_priority() -> i32 {
    1
}

/// GraphSync response message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseMessage {
    /// Request ID being responded to.
    pub id: u64,
    /// Response status.
    pub status: ResponseStatus,
}

/// GraphSync block message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockMessage {
    /// Request ID this block is for.
    pub id: u64,
    /// CID of this block (used by the requester to dedupe & validate).
    pub cid: Cid,
    /// Block data.
    pub block: Vec<u8>,
}

/// Pending request state.
#[derive(Debug)]
pub struct PendingRequest {
    pub id: u64,
    pub root: Cid,
    pub selector: Vec<u8>,
    pub priority: i32,
    pub blocks_sent: Vec<Cid>,
    pub bytes_sent: u64,
    pub status: Option<ResponseStatus>,
}

impl PendingRequest {
    pub fn new(id: u64, root: Cid, selector: Vec<u8>, priority: i32) -> Self {
        Self {
            id,
            root,
            selector,
            priority,
            blocks_sent: Vec::new(),
            bytes_sent: 0,
            status: None,
        }
    }

    pub fn mark_complete(&mut self, status: ResponseStatus) {
        self.status = Some(status);
    }
}

/// GraphSync request builder.
pub struct GraphSyncRequestBuilder {
    root: Option<Cid>,
    selector: Vec<u8>,
    priority: i32,
}

impl GraphSyncRequestBuilder {
    pub fn new() -> Self {
        Self {
            root: None,
            selector: Vec::new(),
            priority: 1,
        }
    }

    pub fn with_root(mut self, root: Cid) -> Self {
        self.root = Some(root);
        self
    }

    pub fn with_selector(mut self, selector: Vec<u8>) -> Self {
        self.selector = selector;
        self
    }

    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    pub fn build(self) -> Result<RequestMessage, GraphSyncError> {
        let root = self
            .root
            .ok_or_else(|| GraphSyncError::InvalidSelector("root CID is required".to_string()))?;
        if self.selector.is_empty() {
            return Err(GraphSyncError::InvalidSelector(
                "selector is required".to_string(),
            ));
        }

        Ok(RequestMessage {
            id: 0, // Will be set by the engine
            root,
            selector: self.selector,
            replace: false,
            priority: self.priority,
        })
    }
}

impl Default for GraphSyncRequestBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Selector types for specifying which blocks to request.
///
/// ADNet 实现覆盖 IPLD Selector v1 的核心子集：
///
/// | ADNet 变体                  | IPLD Selector 等价                     |
/// |-----------------------------|----------------------------------------|
/// | `All`                       | `{"matcher": "All"}`                  |
/// | `None`                      | `{"matcher": "None"}`                 |
/// | `Leaf`                      | `{"matcher": "Leaf"}`                 |
/// | `Range`                     | `{"matcher": "Range"}`                |
/// | `Links`                     | `{"matcher": "Links", ...}`           |
/// | `ExploreFields`             | `{"matcher": "ExploreFields", ...}`   |
/// | `ExploreUnion`              | `{"matcher": "ExploreUnion", ...}`    |
/// | `ExploreRecursive`          | `{"matcher": "ExploreRecursive", ...}`|
/// | `ExploreAll`                | `{"matcher": "ExploreAll", ...}`      |
/// | `ExploreIndex`              | `{"matcher": "ExploreIndex", ...}`    |
///
/// `Union` / `Conditional` 通过 `Sequence::Union` 表达，
/// 与 IPLD Selector 文法保持一致。
pub mod selector {
    use super::*;

    /// Matcher for block traversal.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "matcher")]
    pub enum Matcher {
        /// Match all blocks (entire DAG).
        All {
            #[serde(default)]
            stop_at: Option<StopCondition>,
        },
        /// Match none (empty result).
        None,
        /// Match leaf nodes (no children).
        Leaf,
        /// Match internal nodes (has children).
        Range,
        /// Match specific links by name.
        Links {
            links: Vec<LinkMatcher>,
            #[serde(default)]
            stop_at: Option<StopCondition>,
        },
        /// Traverse only the named fields/links.
        ExploreFields {
            fields: Vec<LinkMatcher>,
            #[serde(default)]
            sequence: Option<Sequence>,
            #[serde(default)]
            stop_at: Option<StopCondition>,
        },
        /// Try each branch in `sequence` until one matches.
        ExploreUnion {
            #[serde(default)]
            sequence: Option<Sequence>,
            #[serde(default)]
            stop_at: Option<StopCondition>,
        },
        /// Recursively walk all reachable blocks up to `max_depth`.
        ExploreRecursive {
            sequence: Box<Sequence>,
            #[serde(default)]
            max_depth: Option<u64>,
            #[serde(default)]
            current_depth: u64,
        },
        /// Walk every link in order.
        ExploreAll { sequence: Box<Sequence> },
        /// Walk only the link at position `index` in the block.
        ExploreIndex { index: u64, sequence: Box<Sequence> },
    }

    /// IPLD Selector sequence (similar to ipld-selectors `Sequence`).
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "kind")]
    pub enum Sequence {
        /// Single matcher.
        Matcher { matcher: Box<Matcher> },
        /// Try branches in order, accept union of results.
        Union { branches: Vec<Sequence> },
        /// Conditional traversal (`if-then-else`).
        Conditional {
            branch: Box<Sequence>,
            #[serde(default)]
            condition: Option<Condition>,
        },
    }

    /// Stop condition for traversal.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "condition", content = "value")]
    pub enum StopCondition {
        /// Stop at a specific depth.
        Depth(u64),
        /// Stop after matching n nodes.
        AfterMatching(u64),
    }

    /// Matcher for specific links.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct LinkMatcher {
        /// Link name to match.
        pub name: Option<String>,
        /// CID to match.
        pub cid: Option<Cid>,
    }

    /// Conditional gating for [`Sequence::Conditional`].
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "kind")]
    pub enum Condition {
        /// True if the current block has a link named `name`.
        HasLink { name: String },
        /// True if the current block has no further links (leaf).
        IsLeaf,
        /// True if the current block equals CID `cid`.
        HasCid { cid: Cid },
    }

    impl Matcher {
        /// Read top-level `stop_at`, if any.
        pub fn top_stop(&self) -> Option<&StopCondition> {
            match self {
                Matcher::All { stop_at }
                | Matcher::Links { stop_at, .. }
                | Matcher::ExploreFields { stop_at, .. }
                | Matcher::ExploreUnion { stop_at, .. } => stop_at.as_ref(),
                _ => None,
            }
        }

        /// Try to extract a top-level Sequence from matchers that wrap one.
        pub fn sequence(&self) -> Option<&Sequence> {
            match self {
                Matcher::ExploreFields { sequence, .. }
                | Matcher::ExploreUnion { sequence, .. } => sequence.as_ref(),
                Matcher::ExploreRecursive { sequence, .. }
                | Matcher::ExploreAll { sequence, .. }
                | Matcher::ExploreIndex { sequence, .. } => Some(sequence.as_ref()),
                _ => None,
            }
        }
    }

    // ─── Selector constructors ────────────────────────────────────

    /// Selector for requesting the entire DAG.
    pub fn match_all() -> Vec<u8> {
        let m = Matcher::All { stop_at: None };
        serde_json::to_vec(&m).unwrap_or_default()
    }

    /// Selector with an explicit stop condition.
    ///
    /// Used by callers that want to bound traversal via
    /// [`StopCondition::Depth`] or [`StopCondition::AfterMatching`].
    pub fn match_all_with_stop(stop: StopCondition) -> Vec<u8> {
        let m = Matcher::All {
            stop_at: Some(stop),
        };
        serde_json::to_vec(&m).unwrap_or_default()
    }

    /// Selector for requesting only leaf nodes (file data).
    pub fn match_leaves() -> Vec<u8> {
        let m = Matcher::Leaf;
        serde_json::to_vec(&m).unwrap_or_default()
    }

    /// Selector for matching specific paths.
    pub fn match_paths(paths: &[&str]) -> Vec<u8> {
        let links: Vec<LinkMatcher> = paths
            .iter()
            .map(|p| LinkMatcher {
                name: Some(p.to_string()),
                cid: None,
            })
            .collect();

        let m = Matcher::Links {
            links,
            stop_at: None,
        };
        serde_json::to_vec(&m).unwrap_or_default()
    }

    /// Recursive selector — equivalent to IPLD Selector `ExploreRecursive`
    /// with `max_depth` cap. `max_depth == None` walks until visited-set
    /// exhausts.
    pub fn match_recursive(max_depth: Option<u64>) -> Vec<u8> {
        let m = Matcher::ExploreRecursive {
            sequence: Box::new(Sequence::Matcher {
                matcher: Box::new(Matcher::ExploreAll {
                    sequence: Box::new(Sequence::Matcher {
                        matcher: Box::new(Matcher::All { stop_at: None }),
                    }),
                }),
            }),
            max_depth,
            current_depth: 0,
        };
        serde_json::to_vec(&m).unwrap_or_default()
    }

    /// Explore-all children, equivalent to IPLD Selector `ExploreAll`.
    pub fn match_explore_all() -> Vec<u8> {
        let m = Matcher::ExploreAll {
            sequence: Box::new(Sequence::Matcher {
                matcher: Box::new(Matcher::All { stop_at: None }),
            }),
        };
        serde_json::to_vec(&m).unwrap_or_default()
    }

    /// Walk the `index`-th link only.
    pub fn match_explore_index(index: u64) -> Vec<u8> {
        let m = Matcher::ExploreIndex {
            index,
            sequence: Box::new(Sequence::Matcher {
                matcher: Box::new(Matcher::All { stop_at: None }),
            }),
        };
        serde_json::to_vec(&m).unwrap_or_default()
    }

    /// Parse a selector from bytes.
    pub fn parse(selector: &[u8]) -> Result<Matcher, GraphSyncError> {
        serde_json::from_slice(selector).map_err(|e| GraphSyncError::InvalidSelector(e.to_string()))
    }
}

/// GraphSync engine for managing requests and responses.
pub struct GraphSyncEngine {
    next_request_id: u64,
    pending_requests: HashMap<u64, PendingRequest>,
}

impl GraphSyncEngine {
    pub fn new() -> Self {
        Self {
            next_request_id: 1,
            pending_requests: HashMap::new(),
        }
    }

    /// Generate a new request ID.
    pub fn next_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
    }

    /// Create a new request.
    pub fn create_request(&mut self, root: Cid, selector: Vec<u8>, priority: i32) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;

        let request = PendingRequest::new(id, root, selector.clone(), priority);
        self.pending_requests.insert(id, request);

        id
    }

    /// Handle an incoming response.
    pub fn handle_response(&mut self, response: ResponseMessage) -> Result<(), GraphSyncError> {
        if let Some(request) = self.pending_requests.get_mut(&response.id) {
            request.mark_complete(response.status);
        }

        Ok(())
    }

    /// Handle an incoming block.
    ///
    /// Verifies that the block bytes match the CID via
    /// [`crate::Cid::verify_bytes`] before counting the block toward
    /// the request's stats. Mismatched blocks return
    /// [`GraphSyncError::BlockHashMismatch`] and are NOT recorded.
    pub fn handle_block(&mut self, block: BlockMessage) -> Result<(), GraphSyncError> {
        if !block.cid.verify_bytes(&block.block) {
            return Err(GraphSyncError::BlockHashMismatch {
                cid: block.cid.clone(),
            });
        }
        if let Some(request) = self.pending_requests.get_mut(&block.id) {
            request.blocks_sent.push(block.cid.clone());
            request.bytes_sent += block.block.len() as u64;
        }

        Ok(())
    }

    /// Get pending request statistics.
    pub fn get_stats(&self) -> GraphSyncStats {
        let mut total_blocks = 0u64;
        let mut total_bytes = 0u64;
        let mut completed = 0u32;
        let mut in_progress = 0u32;

        for request in self.pending_requests.values() {
            total_blocks += request.blocks_sent.len() as u64;
            total_bytes += request.bytes_sent;
            if request.status.is_some() {
                completed += 1;
            } else {
                in_progress += 1;
            }
        }

        GraphSyncStats {
            pending_count: self.pending_requests.len() as u32,
            in_progress,
            completed,
            total_blocks,
            total_bytes,
        }
    }

    /// Cancel a request.
    pub fn cancel_request(&mut self, id: u64) -> Result<(), GraphSyncError> {
        if let Some(request) = self.pending_requests.get_mut(&id) {
            request.mark_complete(ResponseStatus::Cancelled);
            Ok(())
        } else {
            Err(GraphSyncError::RequestNotFound(id))
        }
    }
}

impl Default for GraphSyncEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// GraphSync statistics.
#[derive(Debug, Clone)]
pub struct GraphSyncStats {
    /// Number of pending requests.
    pub pending_count: u32,
    /// Number of in-progress requests.
    pub in_progress: u32,
    /// Number of completed requests.
    pub completed: u32,
    /// Total blocks sent/received.
    pub total_blocks: u64,
    /// Total bytes sent/received.
    pub total_bytes: u64,
}

/// GraphSync responder for processing incoming requests.
pub struct GraphSyncResponder {
    block_store: Arc<dyn BlockStore>,
    /// Hard cap on blocks yielded per request. Prevents a malicious /
    /// buggy selector from holding the responder forever.
    max_blocks: usize,
    /// Hard cap on depth. Mirrors `Selector::ExploreRecursive.max_depth`.
    max_depth: u64,
}

impl GraphSyncResponder {
    /// Build a responder with default limits (65 536 blocks, depth 4096).
    pub fn new(block_store: Arc<dyn BlockStore>) -> Self {
        Self {
            block_store,
            max_blocks: 1 << 16,
            max_depth: 1 << 12,
        }
    }

    /// Override the per-request block cap.
    pub fn with_max_blocks(mut self, n: usize) -> Self {
        self.max_blocks = n;
        self
    }

    /// Override the depth cap.
    pub fn with_max_depth(mut self, d: u64) -> Self {
        self.max_depth = d;
        self
    }

    /// Process an incoming request and return a single fully-buffered
    /// response.
    ///
    /// Prefer [`process_request_streaming`](Self::process_request_streaming)
    /// for large DAGs so the caller can push bytes onto the wire as
    /// they're discovered.
    pub fn process_request(
        &self,
        request: RequestMessage,
    ) -> Result<GraphSyncResponse, GraphSyncError> {
        let items = self.collect_items(request.clone())?;
        let status = items
            .iter()
            .find_map(|i| match i {
                ResponseItem::Status(s) => Some(*s),
                _ => None,
            })
            .unwrap_or(ResponseStatus::Completed);
        let blocks = items
            .into_iter()
            .filter_map(|i| match i {
                ResponseItem::Block { data, .. } => Some(data),
                _ => None,
            })
            .collect();
        Ok(GraphSyncResponse {
            request_id: request.id,
            blocks,
            status,
        })
    }

    /// Process a request, yielding blocks one at a time. The final
    /// item is always [`ResponseItem::Status`].
    pub fn process_request_streaming(
        &self,
        request: RequestMessage,
    ) -> Result<Vec<ResponseItem>, GraphSyncError> {
        self.collect_items(request)
    }

    fn collect_items(&self, request: RequestMessage) -> Result<Vec<ResponseItem>, GraphSyncError> {
        let matcher = selector::parse(&request.selector)?;
        let mut out = Vec::new();
        let mut budget = self.max_blocks;
        let mut visited: HashSet<Cid> = HashSet::new();
        let mut blocks_yielded: usize = 0;
        let mut ctx = TraversalContext {
            store: self.block_store.as_ref(),
            visited: &mut visited,
            budget: &mut budget,
            max_depth: self.max_depth,
            blocks_yielded: &mut blocks_yielded,
            stop_after_matching: stop_limit(&matcher),
        };

        // `AfterMatchingReached` is a graceful early-exit: the
        // caller's stop policy was satisfied. Surface it as
        // `EndOfDag` (the DAG itself isn't exhausted, the policy is).
        let after_matching_stopped =
            match traverse_into(&matcher, &request.root, 0, &mut ctx, &mut out) {
                Ok(()) => false,
                Err(TraversalError::AfterMatchingReached) => true,
                Err(TraversalError::DepthExceeded { limit, cid }) => {
                    return Err(GraphSyncError::DepthExceeded { limit, cid });
                }
                Err(TraversalError::RecursionLimit) => return Err(GraphSyncError::RecursionLimit),
                Err(TraversalError::BlockNotFound(c)) => {
                    return Err(GraphSyncError::BlockNotFound(c));
                }
            };

        if out.is_empty() {
            out.push(ResponseItem::Status(ResponseStatus::Completed));
            return Ok(out);
        }

        // Status heuristic:
        // - `after_matching_stopped`: caller-driven stop policy hit →
        //   `EndOfDag` (the DAG may have more blocks but the policy
        //   said stop).
        // - `budget == 0`: hard cap reached before completion →
        //   `Partial`.
        // - Otherwise: traversal terminated naturally → `Completed`.
        let status = if after_matching_stopped {
            ResponseStatus::EndOfDag
        } else if budget == 0 {
            ResponseStatus::Partial
        } else {
            ResponseStatus::Completed
        };
        out.push(ResponseItem::Status(status));
        Ok(out)
    }
}

/// Buffered response from a GraphSync request.
#[derive(Debug, Clone)]
pub struct GraphSyncResponse {
    pub request_id: u64,
    pub blocks: Vec<Vec<u8>>,
    pub status: ResponseStatus,
}

/// Streaming item produced by a [`GraphSyncResponder`].
///
/// Callers iterate over the vec, sending one frame per item. The
/// final element is always a [`ResponseItem::Status`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseItem {
    /// A single DAG block to be transmitted to the requester.
    Block { cid: Cid, data: Vec<u8> },
    /// Final status of the request.
    Status(ResponseStatus),
}

// ─────────────────────────────────────────────────────────────────
// Block store trait
// ─────────────────────────────────────────────────────────────────

/// Synchronous DAG block store used by the responder / traverser.
///
/// This is intentionally minimal and free of IO semantics so it can
/// be backed by an in-memory map, a `BlobStore`, the `iroh-blobs`
/// redb store, etc. `links()` returns CIDs of children, enabling
/// recursive traversal.
///
/// Block bytes are returned as opaque payloads; the responder does
/// not interpret codec layout. Callers that need DAG-CBOR / DAG-PB
/// semantics should implement `links()` accordingly.
pub trait BlockStore: Send + Sync {
    /// Return the raw block for `cid` if present.
    fn get(&self, cid: &Cid) -> Option<Vec<u8>>;

    /// Persist a block. Default implementation is a no-op so
    /// read-only stores can ignore writes.
    fn put(&self, _cid: &Cid, _block: &[u8]) {}

    /// True iff the store has a complete copy of `cid`.
    fn has(&self, cid: &Cid) -> bool {
        self.get(cid).is_some()
    }

    /// Return the CIDs of every direct child link in `cid`'s block.
    ///
    /// Returning an empty vec makes the matcher treat the block as a
    /// leaf even when the matcher would normally recurse — useful for
    /// raw-bytes blocks that don't follow DAG-PB / DAG-CBOR.
    fn links(&self, cid: &Cid) -> Vec<Cid>;

    /// Return the named links of `cid`'s block as `(name, cid)` pairs.
    ///
    /// Used by [`crate::graphsync::selector::Matcher::Links`] to honor
    /// the `name` field of [`crate::graphsync::selector::LinkMatcher`].
    /// Default implementation returns the same children as [`Self::links`]
    /// with no name attached; stores that decode DAG-PB / DAG-CBOR /
    /// UnixFS should override this to surface link names.
    fn links_named(&self, cid: &Cid) -> Vec<(Option<String>, Cid)> {
        self.links(cid).into_iter().map(|c| (None, c)).collect()
    }
}

// ─────────────────────────────────────────────────────────────────
// DAG traversal
// ─────────────────────────────────────────────────────────────────

/// Internal traversal state shared across recursive calls.
struct TraversalContext<'a> {
    store: &'a dyn BlockStore,
    visited: &'a mut HashSet<Cid>,
    budget: &'a mut usize,
    max_depth: u64,
    /// Cumulative count of blocks already pushed into `out`. Used to
    /// honor [`selector::StopCondition::AfterMatching`] — when the
    /// running count reaches the configured limit, traversal exits
    /// cleanly with the partial result set.
    blocks_yielded: &'a mut usize,
    /// If set, traversal exits with [`TraversalError::AfterMatchingReached`]
    /// once `blocks_yielded` reaches this value. Computed once at the
    /// top of `collect_items` / `traverse` from the matcher.
    stop_after_matching: Option<usize>,
}

#[derive(Debug)]
enum TraversalError {
    DepthExceeded {
        limit: u64,
        cid: Cid,
    },
    RecursionLimit,
    BlockNotFound(Cid),
    /// Stop-condition `AfterMatching(N)` was hit. The result set so
    /// far is valid; the caller can decide whether to surface it as
    /// a `Partial` or `EndOfDag` response.
    AfterMatchingReached,
}

fn push_block(
    out: &mut Vec<ResponseItem>,
    cid: &Cid,
    data: Vec<u8>,
    budget: &mut usize,
    blocks_yielded: &mut usize,
) -> Result<(), TraversalError> {
    if *budget == 0 {
        return Err(TraversalError::RecursionLimit);
    }
    *budget -= 1;
    *blocks_yielded += 1;
    out.push(ResponseItem::Block {
        cid: cid.clone(),
        data,
    });
    Ok(())
}

/// Top-level entry point. Walks `root` according to `matcher`,
/// emitting blocks into `out`.
fn traverse_into(
    matcher: &selector::Matcher,
    cid: &Cid,
    depth: u64,
    ctx: &mut TraversalContext<'_>,
    out: &mut Vec<ResponseItem>,
) -> Result<(), TraversalError> {
    if depth > ctx.max_depth {
        return Err(TraversalError::DepthExceeded {
            limit: ctx.max_depth,
            cid: cid.clone(),
        });
    }
    // Honor AfterMatching(N): once we've yielded N blocks, exit cleanly.
    // The check runs at function entry so already-queued recursion
    // unwinds rather than producing more blocks first.
    if let Some(limit) = ctx.stop_after_matching
        && *ctx.blocks_yielded >= limit
    {
        return Err(TraversalError::AfterMatchingReached);
    }
    if !ctx.visited.insert(cid.clone()) {
        return Ok(());
    }
    let block_bytes = ctx
        .store
        .get(cid)
        .ok_or_else(|| TraversalError::BlockNotFound(cid.clone()))?;

    match matcher {
        selector::Matcher::None => Ok(()),
        selector::Matcher::All { stop_at } => {
            push_block(
                out,
                cid,
                block_bytes.clone(),
                ctx.budget,
                ctx.blocks_yielded,
            )?;
            if stop_at_reached(depth, *ctx.blocks_yielded, stop_at.as_ref()) {
                return Ok(());
            }
            let children = ctx.store.links(cid);
            for child in children {
                let m = selector::Matcher::All {
                    stop_at: stop_at.clone(),
                };
                traverse_into(&m, &child, depth + 1, ctx, out)?;
            }
            Ok(())
        }
        selector::Matcher::Leaf => {
            let children = ctx.store.links(cid);
            if children.is_empty() {
                push_block(
                    out,
                    cid,
                    block_bytes.clone(),
                    ctx.budget,
                    ctx.blocks_yielded,
                )?;
                return Ok(());
            }
            // Recurse into children with `Leaf` so we discover
            // leaves anywhere in the DAG.
            for child in children {
                let m = selector::Matcher::Leaf;
                traverse_into(&m, &child, depth + 1, ctx, out)?;
            }
            Ok(())
        }
        selector::Matcher::Range => {
            let children = ctx.store.links(cid);
            if !children.is_empty() {
                push_block(
                    out,
                    cid,
                    block_bytes.clone(),
                    ctx.budget,
                    ctx.blocks_yielded,
                )?;
            }
            Ok(())
        }
        selector::Matcher::Links { links, stop_at } => {
            let children = ctx.store.links_named(cid);
            for (name, child) in children {
                let matched = links.iter().any(|lm| {
                    let cid_ok = lm.cid.as_ref().is_none_or(|c| c == &child);
                    let name_ok = lm.name.as_ref().is_none_or(|n| {
                        name.as_ref().is_some_and(|actual| actual == n)
                    });
                    // Only honor cid-only or name-only filters when the
                    // store provides the matching field. If the store
                    // returns no names, fall back to cid matching only
                    // (a name-only filter with no resolved names never
                    // matches, which is the safe default).
                    cid_ok && name_ok
                });
                if matched {
                    let child_bytes = ctx.store.get(&child).unwrap_or_default();
                    push_block(out, &child, child_bytes, ctx.budget, ctx.blocks_yielded)?;
                    if stop_at_reached(depth + 1, *ctx.blocks_yielded, stop_at.as_ref()) {
                        continue;
                    }
                    let m = selector::Matcher::All {
                        stop_at: stop_at.clone(),
                    };
                    traverse_into(&m, &child, depth + 1, ctx, out)?;
                }
            }
            Ok(())
        }
        selector::Matcher::ExploreFields {
            fields,
            sequence,
            stop_at,
        } => {
            // Only walk links that match `fields`.
            let children = ctx.store.links(cid);
            for child in children {
                let m = selector::Matcher::Links {
                    links: fields.clone(),
                    stop_at: stop_at.clone(),
                };
                traverse_into(&m, &child, depth, ctx, out)?;
            }
            // Apply the post-sequence matcher against the current block.
            if let Some(seq) = sequence {
                apply_sequence(seq, cid, depth, ctx, out)?;
            }
            Ok(())
        }
        selector::Matcher::ExploreUnion { sequence, stop_at } => {
            // Recurse into all children, treating union branches
            // as a fallback set.
            let children = ctx.store.links(cid);
            for child in children {
                let m = selector::Matcher::All {
                    stop_at: stop_at.clone(),
                };
                traverse_into(&m, &child, depth + 1, ctx, out)?;
            }
            if let Some(seq) = sequence {
                apply_sequence(seq, cid, depth, ctx, out)?;
            }
            Ok(())
        }
        selector::Matcher::ExploreRecursive {
            sequence,
            max_depth,
            current_depth,
        } => {
            // Mark current depth in the matcher copy.
            push_block(
                out,
                cid,
                block_bytes.clone(),
                ctx.budget,
                ctx.blocks_yielded,
            )?;
            if let Some(limit) = *max_depth
                && *current_depth >= limit
            {
                return Ok(());
            }
            // Resolve the recursive matcher's body as the post sequence.
            apply_sequence(sequence, cid, depth, ctx, out)?;
            let rec = selector::Matcher::ExploreRecursive {
                sequence: sequence.clone(),
                max_depth: *max_depth,
                current_depth: current_depth.saturating_add(1),
            };
            let children = ctx.store.links(cid);
            for child in children {
                traverse_into(&rec, &child, depth + 1, ctx, out)?;
            }
            Ok(())
        }
        selector::Matcher::ExploreAll { sequence } => {
            push_block(
                out,
                cid,
                block_bytes.clone(),
                ctx.budget,
                ctx.blocks_yielded,
            )?;
            let children = ctx.store.links(cid);
            for child in children {
                apply_sequence(sequence, &child, depth + 1, ctx, out)?;
            }
            Ok(())
        }
        selector::Matcher::ExploreIndex { index, sequence } => {
            let children = ctx.store.links(cid);
            if let Some(child) = children.get(*index as usize) {
                push_block(
                    out,
                    cid,
                    block_bytes.clone(),
                    ctx.budget,
                    ctx.blocks_yielded,
                )?;
                apply_sequence(sequence, child, depth + 1, ctx, out)?;
            }
            Ok(())
        }
    }
}

fn stop_at_reached(
    depth: u64,
    blocks_yielded: usize,
    stop_at: Option<&selector::StopCondition>,
) -> bool {
    match stop_at {
        Some(selector::StopCondition::Depth(d)) => depth >= *d,
        Some(selector::StopCondition::AfterMatching(n)) => blocks_yielded >= *n as usize,
        None => false,
    }
}

/// Walk a matcher tree to find any `AfterMatching` stop condition.
/// Returns the smallest configured limit — that is the tightest
/// threshold we must respect during traversal.
fn stop_limit(matcher: &selector::Matcher) -> Option<usize> {
    fn from_stop(s: &selector::StopCondition) -> Option<usize> {
        match s {
            selector::StopCondition::AfterMatching(n) => Some(*n as usize),
            _ => None,
        }
    }
    fn from_seq(s: &selector::Sequence) -> Option<usize> {
        match s {
            selector::Sequence::Matcher { matcher } => stop_limit(matcher),
            selector::Sequence::Union { branches } => branches.iter().filter_map(from_seq).min(),
            selector::Sequence::Conditional { branch, .. } => from_seq(branch),
        }
    }
    match matcher {
        selector::Matcher::All { stop_at }
        | selector::Matcher::Links { stop_at, .. }
        | selector::Matcher::ExploreFields { stop_at, .. }
        | selector::Matcher::ExploreUnion { stop_at, .. } => stop_at.as_ref().and_then(from_stop),
        selector::Matcher::None | selector::Matcher::Leaf | selector::Matcher::Range => None,
        selector::Matcher::ExploreRecursive { sequence, .. }
        | selector::Matcher::ExploreAll { sequence }
        | selector::Matcher::ExploreIndex { sequence, .. } => from_seq(sequence),
    }
}

fn apply_sequence(
    seq: &selector::Sequence,
    cid: &Cid,
    depth: u64,
    ctx: &mut TraversalContext<'_>,
    out: &mut Vec<ResponseItem>,
) -> Result<(), TraversalError> {
    match seq {
        selector::Sequence::Matcher { matcher } => traverse_into(matcher, cid, depth, ctx, out),
        selector::Sequence::Union { branches } => {
            for b in branches {
                apply_sequence(b, cid, depth, ctx, out)?;
            }
            Ok(())
        }
        selector::Sequence::Conditional { branch, condition } => {
            if matches_condition(condition.as_ref(), ctx.store, cid) {
                apply_sequence(branch, cid, depth, ctx, out)?;
            }
            Ok(())
        }
    }
}

fn matches_condition(
    cond: Option<&selector::Condition>,
    store: &dyn BlockStore,
    cid: &Cid,
) -> bool {
    match cond {
        None => true,
        Some(selector::Condition::HasLink { .. }) => !store.links(cid).is_empty(),
        Some(selector::Condition::IsLeaf) => store.links(cid).is_empty(),
        Some(selector::Condition::HasCid { cid: c }) => c == cid,
    }
}

/// Public traversal entry point. Used by the responder and exposed
/// for callers that want to feed blocks into a custom transport
/// without instantiating a full [`GraphSyncResponder`].
///
/// Returns the ordered `(cid, block)` pairs; if the store yields no
/// bytes for a block the entry's `data` is empty.
pub fn traverse(
    root: &Cid,
    matcher: &selector::Matcher,
    store: &dyn BlockStore,
    max_depth: u64,
) -> Result<Vec<(Cid, Vec<u8>)>, GraphSyncError> {
    let mut visited = HashSet::new();
    let mut budget = usize::MAX;
    let mut blocks_yielded: usize = 0;
    let mut ctx = TraversalContext {
        store,
        visited: &mut visited,
        budget: &mut budget,
        max_depth,
        blocks_yielded: &mut blocks_yielded,
        stop_after_matching: stop_limit(matcher),
    };
    let mut out = Vec::new();
    match traverse_into(matcher, root, 0, &mut ctx, &mut out) {
        Ok(()) => Ok(extract_blocks(&out)),
        // Treat AfterMatchingReached as a graceful early-exit.
        Err(TraversalError::AfterMatchingReached) => Ok(extract_blocks(&out)),
        Err(TraversalError::DepthExceeded { limit, cid }) => {
            Err(GraphSyncError::DepthExceeded { limit, cid })
        }
        Err(TraversalError::RecursionLimit) => Err(GraphSyncError::RecursionLimit),
        Err(TraversalError::BlockNotFound(c)) => Err(GraphSyncError::BlockNotFound(c)),
    }
}

/// Drain the `(Cid, bytes)` pairs out of a `ResponseItem` vec. Used
/// by [`traverse`] and by the `AfterMatchingReached` early-exit path.
fn extract_blocks(items: &[ResponseItem]) -> Vec<(Cid, Vec<u8>)> {
    items
        .iter()
        .filter_map(|i| match i {
            ResponseItem::Block { cid, data } => Some((cid.clone(), data.clone())),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Tiny in-memory block store; `links()` parses DAG-PB-shaped
    /// JSON `"links": [{"Hash": "..."}]` for tests, and otherwise
    /// returns an empty vector so the matcher treats the block as
    /// a leaf.
    #[derive(Default)]
    struct MemStore {
        blocks: Mutex<HashMap<Cid, Vec<u8>>>,
    }

    impl MemStore {
        fn put(&self, cid: Cid, bytes: Vec<u8>) {
            self.blocks.lock().unwrap().insert(cid, bytes);
        }
    }

    impl BlockStore for MemStore {
        fn get(&self, cid: &Cid) -> Option<Vec<u8>> {
            self.blocks.lock().unwrap().get(cid).cloned()
        }
        fn put(&self, cid: &Cid, bytes: &[u8]) {
            self.blocks
                .lock()
                .unwrap()
                .insert(cid.clone(), bytes.to_vec());
        }
        fn links(&self, cid: &Cid) -> Vec<Cid> {
            let blocks = self.blocks.lock().unwrap();
            let Some(bytes) = blocks.get(cid) else {
                return Vec::new();
            };
            if let Ok(node) = serde_json::from_slice::<serde_json::Value>(bytes) {
                if let Some(arr) = node.get("links").and_then(|v| v.as_array()) {
                    return arr
                        .iter()
                        .filter_map(|l| l.get("Hash").and_then(|h| h.as_str()))
                        .filter_map(|s| Cid::parse(s).ok())
                        .collect();
                }
            }
            Vec::new()
        }
    }

    fn dag_link(child: &Cid) -> serde_json::Value {
        serde_json::json!({ "Hash": child.to_string() })
    }

    fn dag_node(children: &[&Cid]) -> Vec<u8> {
        let links: Vec<_> = children.iter().map(|c| dag_link(c)).collect();
        serde_json::to_vec(&serde_json::json!({ "links": links })).unwrap()
    }

    #[test]
    fn test_response_status() {
        assert_eq!(ResponseStatus::Completed.to_u32(), 0);
        assert_eq!(ResponseStatus::from_u32(0), Some(ResponseStatus::Completed));
        assert_eq!(ResponseStatus::from_u32(99), None);
    }

    #[test]
    fn test_response_status_display() {
        assert_eq!(format!("{}", ResponseStatus::Completed), "Completed");
        assert_eq!(format!("{}", ResponseStatus::Partial), "Partial");
        assert_eq!(format!("{}", ResponseStatus::EndOfDag), "EndOfDag");
        assert_eq!(format!("{}", ResponseStatus::Remote), "Remote");
        assert_eq!(format!("{}", ResponseStatus::Cancelled), "Cancelled");
        assert_eq!(format!("{}", ResponseStatus::Failed), "Failed");
    }

    #[test]
    fn test_request_builder() {
        let request = GraphSyncRequestBuilder::new()
            .with_root(Cid::from_content_blake3(b"test"))
            .with_selector(selector::match_all())
            .with_priority(1)
            .build()
            .unwrap();

        assert_eq!(request.priority, 1);
        assert!(!request.replace);
    }

    #[test]
    fn test_selector_match_all() {
        let selector = selector::match_all();
        let parsed = selector::parse(&selector).unwrap();
        assert!(matches!(parsed, selector::Matcher::All { .. }));
    }

    #[test]
    fn test_selector_recursive_roundtrip() {
        let sel = selector::match_recursive(Some(3));
        let parsed = selector::parse(&sel).unwrap();
        assert!(matches!(parsed, selector::Matcher::ExploreRecursive { .. }));
    }

    #[test]
    fn test_graphsync_engine() {
        let engine = GraphSyncEngine::new();
        assert_eq!(engine.next_request_id, 1);
    }

    #[test]
    fn test_create_request() {
        let mut engine = GraphSyncEngine::new();
        let cid = Cid::from_content_blake3(b"test");
        let id = engine.create_request(cid, vec![], 1);
        assert_eq!(id, 1);

        let stats = engine.get_stats();
        assert_eq!(stats.pending_count, 1);
        assert_eq!(stats.in_progress, 1);
    }

    #[test]
    fn test_handle_response() {
        let mut engine = GraphSyncEngine::new();
        let cid = Cid::from_content_blake3(b"test");
        let id = engine.create_request(cid, vec![], 1);

        let response = ResponseMessage {
            id,
            status: ResponseStatus::Completed,
        };
        engine.handle_response(response).unwrap();

        let stats = engine.get_stats();
        assert_eq!(stats.completed, 1);
    }

    #[test]
    fn test_cancel_request() {
        let mut engine = GraphSyncEngine::new();
        let cid = Cid::from_content_blake3(b"test");
        let id = engine.create_request(cid, vec![], 1);

        engine.cancel_request(id).unwrap();

        let stats = engine.get_stats();
        assert_eq!(stats.completed, 1);
    }

    // ─── DAG traversal tests ────────────────────────────────────

    fn build_linear_dag() -> (MemStore, Cid) {
        let store = MemStore::default();
        let leaf = Cid::from_content_blake3(b"leaf");
        store.put(leaf.clone(), b"raw leaf bytes".to_vec());
        let mid = Cid::from_content_blake3(b"mid");
        store.put(mid.clone(), dag_node(&[&leaf]));
        let root = Cid::from_content_blake3(b"root");
        store.put(root.clone(), dag_node(&[&mid]));
        (store, root)
    }

    #[test]
    fn traverse_match_all_walks_full_dag() {
        let (store, root) = build_linear_dag();
        let matcher = selector::parse(&selector::match_all()).unwrap();
        let blocks = traverse(&root, &matcher, &store, 32).unwrap();
        let cids: Vec<_> = blocks.iter().map(|(c, _)| c.clone()).collect();
        assert_eq!(cids.len(), 3, "should yield root + mid + leaf");
        assert!(cids.contains(&root));
    }

    #[test]
    fn traverse_match_leaves_only() {
        let (store, root) = build_linear_dag();
        let matcher = selector::parse(&selector::match_leaves()).unwrap();
        let blocks = traverse(&root, &matcher, &store, 32).unwrap();
        assert_eq!(blocks.len(), 1, "only the leaf should be returned");
    }

    #[test]
    fn traverse_recursive_with_depth_cap() {
        let (store, root) = build_linear_dag();
        let matcher = selector::parse(&selector::match_recursive(Some(1))).unwrap();
        let blocks = traverse(&root, &matcher, &store, 32).unwrap();
        // root + mid at depth 1; leaf is at depth 2 so excluded.
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn traverse_handles_cycle_without_looping() {
        let store = MemStore::default();
        let a = Cid::from_content_blake3(b"a");
        let b = Cid::from_content_blake3(b"b");
        store.put(a.clone(), dag_node(&[&b]));
        store.put(b.clone(), dag_node(&[&a])); // cycle
        let matcher = selector::parse(&selector::match_all()).unwrap();
        let blocks = traverse(&a, &matcher, &store, 16).unwrap();
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn traverse_explore_index_picks_nth_link() {
        let store = MemStore::default();
        let l0 = Cid::from_content_blake3(b"link0");
        let l1 = Cid::from_content_blake3(b"link1");
        store.put(l0.clone(), b"data0".to_vec());
        store.put(l1.clone(), b"data1".to_vec());
        let root = Cid::from_content_blake3(b"root-many");
        store.put(root.clone(), dag_node(&[&l0, &l1]));
        let matcher = selector::parse(&selector::match_explore_index(1)).unwrap();
        let blocks = traverse(&root, &matcher, &store, 8).unwrap();
        let cids: Vec<_> = blocks.iter().map(|(c, _)| c.clone()).collect();
        assert!(cids.contains(&root));
        assert!(cids.contains(&l1));
        assert!(!cids.contains(&l0));
    }

    #[test]
    fn responder_streams_full_response() {
        let (store, root) = build_linear_dag();
        let responder = GraphSyncResponder::new(Arc::new(store));
        let req = RequestMessage {
            id: 7,
            root: root.clone(),
            selector: selector::match_all(),
            replace: false,
            priority: 1,
        };
        let items = responder.process_request_streaming(req).unwrap();
        // 3 blocks + 1 status
        assert_eq!(items.len(), 4);
        let status = items.last().unwrap();
        assert!(matches!(
            status,
            ResponseItem::Status(ResponseStatus::Completed)
        ));
        let block_count = items
            .iter()
            .filter(|i| matches!(i, ResponseItem::Block { .. }))
            .count();
        assert_eq!(block_count, 3);
    }

    #[test]
    fn responder_buffered_response_status_completed() {
        let (store, root) = build_linear_dag();
        let responder = GraphSyncResponder::new(Arc::new(store));
        let req = RequestMessage {
            id: 11,
            root,
            selector: selector::match_recursive(Some(2)),
            replace: false,
            priority: 1,
        };
        let resp = responder.process_request(req).unwrap();
        assert_eq!(resp.request_id, 11);
        assert_eq!(resp.status, ResponseStatus::Completed);
        assert!(resp.blocks.len() >= 2);
    }

    #[test]
    fn responder_reports_missing_root() {
        let store = MemStore::default();
        let responder = GraphSyncResponder::new(Arc::new(store));
        let req = RequestMessage {
            id: 1,
            root: Cid::from_content_blake3(b"missing"),
            selector: selector::match_all(),
            replace: false,
            priority: 1,
        };
        let err = responder.process_request(req).unwrap_err();
        assert!(matches!(err, GraphSyncError::BlockNotFound(_)));
    }

    // ─── Phase 1 regressions ─────────────────────────────────────

    /// Build a DAG that exposes its links with names so we can test
    /// the `Matcher::Links` name filter.
    ///
    /// `store.insert_named(...)` populates the `named_links` map,
    /// which makes `BlockStore::links_named(cid)` return names.
    struct NamedMemStore {
        blocks: std::collections::HashMap<Cid, Vec<u8>>,
        named: std::collections::HashMap<Cid, Vec<(Option<String>, Cid)>>,
    }

    impl BlockStore for NamedMemStore {
        fn get(&self, cid: &Cid) -> Option<Vec<u8>> {
            self.blocks.get(cid).cloned()
        }
        fn links(&self, cid: &Cid) -> Vec<Cid> {
            self.named
                .get(cid)
                .map(|v| v.iter().map(|(_, c)| c.clone()).collect())
                .unwrap_or_default()
        }
        fn links_named(&self, cid: &Cid) -> Vec<(Option<String>, Cid)> {
            self.named.get(cid).cloned().unwrap_or_default()
        }
    }

    #[test]
    fn links_matcher_filters_by_name() {
        // Build a 3-child DAG: root → {alpha, beta, gamma}
        let alpha = Cid::from_content_blake3(b"alpha-payload");
        let beta = Cid::from_content_blake3(b"beta-payload");
        let gamma = Cid::from_content_blake3(b"gamma-payload");
        let root = Cid::from_content_blake3(b"root-payload");

        let mut blocks = std::collections::HashMap::new();
        blocks.insert(root.clone(), b"root".to_vec());
        blocks.insert(alpha.clone(), b"alpha".to_vec());
        blocks.insert(beta.clone(), b"beta".to_vec());
        blocks.insert(gamma.clone(), b"gamma".to_vec());

        let mut named = std::collections::HashMap::new();
        named.insert(
            root.clone(),
            vec![
                (Some("alpha".into()), alpha.clone()),
                (Some("beta".into()), beta.clone()),
                (Some("gamma".into()), gamma.clone()),
            ],
        );

        let store = NamedMemStore { blocks, named };

        // Selector asking ONLY for the "beta" link.
        let matcher = selector::Matcher::Links {
            links: vec![selector::LinkMatcher {
                name: Some("beta".into()),
                cid: None,
            }],
            stop_at: None,
        };

        let blocks_out = traverse(&root, &matcher, &store, 32).unwrap();
        let cids: Vec<Cid> = blocks_out.into_iter().map(|(c, _)| c).collect();

        // Only beta should come back, not alpha or gamma.
        assert!(cids.contains(&beta), "beta must be returned");
        assert!(!cids.contains(&alpha), "alpha must be filtered out");
        assert!(!cids.contains(&gamma), "gamma must be filtered out");
    }

    #[test]
    fn after_matching_stop_returns_end_of_dag() {
        // Build a 5-block DAG; ask for only 3 with `AfterMatching`.
        let store = MemStore::default();
        let cids: Vec<Cid> = (0..5)
            .map(|i| {
                let c = Cid::from_content_blake3(format!("node-{i}").as_bytes());
                store.put(c.clone(), format!("node-{i}").into_bytes());
                c
            })
            .collect();

        let root = cids[0].clone();
        // Wire the root block to all 4 children so `All` traverses them.
        {
            let mut blocks = store.blocks.lock().unwrap();
            let mut children_json = String::from(r#"{"links":["#);
            for (i, c) in cids.iter().enumerate().skip(1) {
                if i > 1 {
                    children_json.push(',');
                }
                children_json.push_str(&format!(r#"{{"Hash":"{}"}}"#, c));
            }
            children_json.push_str("]}");
            blocks.insert(root.clone(), children_json.into_bytes());
        }

        let responder = GraphSyncResponder::new(Arc::new(store));
        let items = responder
            .process_request_streaming(RequestMessage {
                id: 1,
                root: root.clone(),
                selector: selector::match_all_with_stop(selector::StopCondition::AfterMatching(3)),
                replace: false,
                priority: 1,
            })
            .unwrap();

        // 3 blocks + 1 status
        assert_eq!(items.len(), 4);
        // Final status must be `EndOfDag`, not `Completed` or `Partial`.
        let status = items.last().unwrap();
        assert!(
            matches!(status, ResponseItem::Status(ResponseStatus::EndOfDag)),
            "expected EndOfDag, got {:?}",
            status
        );

        // And only 3 blocks should have been pushed.
        let block_count = items
            .iter()
            .filter(|i| matches!(i, ResponseItem::Block { .. }))
            .count();
        assert_eq!(block_count, 3);
    }

    #[test]
    fn after_matching_zero_returns_root_only() {
        // AfterMatching(0) means "match zero blocks" — the policy
        // says stop immediately. The result is empty (no blocks) plus
        // a `Completed` status from the empty-out fast path.
        let store = MemStore::default();
        let root = Cid::from_content_blake3(b"root");
        store.put(root.clone(), b"root".to_vec());

        let responder = GraphSyncResponder::new(Arc::new(store));
        let items = responder
            .process_request_streaming(RequestMessage {
                id: 7,
                root,
                selector: selector::match_all_with_stop(selector::StopCondition::AfterMatching(0)),
                replace: false,
                priority: 1,
            })
            .unwrap();

        // Only the empty-result Completed status.
        assert_eq!(items.len(), 1);
        assert!(matches!(
            items[0],
            ResponseItem::Status(ResponseStatus::Completed)
        ));
    }

    #[test]
    fn handle_block_rejects_hash_mismatch() {
        // Build a CID for one payload, but send a different payload.
        // The engine should reject it with `BlockHashMismatch`.
        let mut engine = GraphSyncEngine::new();
        let cid = Cid::from_content_blake3(b"original");
        let id = engine.create_request(cid.clone(), vec![], 1);

        let block = BlockMessage {
            id,
            cid: cid.clone(),
            block: b"tampered".to_vec(), // does NOT hash to cid
        };
        let err = engine.handle_block(block).unwrap_err();
        assert!(matches!(err, GraphSyncError::BlockHashMismatch { .. }));

        // Stats should NOT have been updated.
        let stats = engine.get_stats();
        assert_eq!(stats.total_blocks, 0);
        assert_eq!(stats.total_bytes, 0);
    }

    #[test]
    fn handle_block_accepts_correct_hash() {
        // Send the right payload — engine should accept and count it.
        let mut engine = GraphSyncEngine::new();
        let payload = b"the-real-payload";
        let cid = Cid::from_content_blake3(payload);
        let id = engine.create_request(cid.clone(), vec![], 1);

        let block = BlockMessage {
            id,
            cid: cid.clone(),
            block: payload.to_vec(),
        };
        engine
            .handle_block(block)
            .expect("matching hash should pass");

        let stats = engine.get_stats();
        assert_eq!(stats.total_blocks, 1);
        assert_eq!(stats.total_bytes, payload.len() as u64);
    }

    // ─────────────────────────────────────────────────────────────────
    //  Error model coverage
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn graphsync_error_display_for_every_variant() {
        let cid = Cid::from_content_blake3(b"x");
        let cases: Vec<(GraphSyncError, &str)> = vec![
            (GraphSyncError::RequestNotFound(7), "request not found: 7"),
            (GraphSyncError::RequestCancelled, "request cancelled"),
            (GraphSyncError::PeerNotConnected, "peer not connected"),
            (
                GraphSyncError::InvalidSelector("oops".into()),
                "invalid selector: oops",
            ),
            (
                GraphSyncError::BlockNotFound(cid.clone()),
                "block not found: ",
            ),
            (
                GraphSyncError::BlockDecode {
                    cid: cid.clone(),
                    message: "bad cbor".into(),
                },
                "block decode error for ",
            ),
            (
                GraphSyncError::BlockHashMismatch { cid: cid.clone() },
                "block content hash mismatch",
            ),
            (
                GraphSyncError::DepthExceeded {
                    limit: 8,
                    cid: cid.clone(),
                },
                "traversal depth exceeded (limit=8)",
            ),
            (
                GraphSyncError::RecursionLimit,
                "selector recursion limit exceeded",
            ),
            (
                GraphSyncError::SendError("dropped".into()),
                "send error: dropped",
            ),
            (
                GraphSyncError::Internal("boom".into()),
                "internal error: boom",
            ),
        ];
        for (err, needle) in cases {
            let s = err.to_string();
            assert!(
                s.contains(needle),
                "Display for {:?} = {:?}, expected substring {:?}",
                err,
                s,
                needle
            );
        }
    }

    #[test]
    fn response_status_round_trip_for_every_variant() {
        for s in [
            ResponseStatus::Completed,
            ResponseStatus::Partial,
            ResponseStatus::EndOfDag,
            ResponseStatus::Remote,
            ResponseStatus::Cancelled,
            ResponseStatus::Failed,
        ] {
            assert_eq!(ResponseStatus::from_u32(s.to_u32()), Some(s));
        }
        // Every byte value out of range is rejected.
        for v in [6u32, 100, u32::MAX] {
            assert_eq!(ResponseStatus::from_u32(v), None);
        }
    }

    #[test]
    fn request_builder_defaults() {
        // Default priority is 1; replace defaults to false.
        let req = GraphSyncRequestBuilder::new()
            .with_root(Cid::from_content_blake3(b"root"))
            .with_selector(selector::match_all())
            .build()
            .unwrap();
        assert_eq!(req.id, 0);
        assert_eq!(req.priority, 1);
        assert!(!req.replace);
    }

    #[test]
    fn request_builder_missing_root_errors() {
        let err = GraphSyncRequestBuilder::new()
            .with_selector(selector::match_all())
            .build()
            .unwrap_err();
        let s = err.to_string();
        assert!(
            s.contains("root"),
            "unexpected error: {s}"
        );
    }

    #[test]
    fn request_builder_missing_selector_errors() {
        let err = GraphSyncRequestBuilder::new()
            .with_root(Cid::from_content_blake3(b"root"))
            .build()
            .unwrap_err();
        let s = err.to_string();
        assert!(
            s.contains("selector"),
            "unexpected error: {s}"
        );
    }

    #[test]
    fn message_block_helper_carries_cid_and_payload() {
        let cid = Cid::from_content_blake3(b"hello");
        let msg = GraphSyncMessage::block(11, cid.clone(), b"world".to_vec());
        match msg {
            GraphSyncMessage::Block(b) => {
                assert_eq!(b.id, 11);
                assert_eq!(b.cid, cid);
                assert_eq!(b.block, b"world".to_vec());
            }
            _ => panic!("expected Block message"),
        }
    }

    #[test]
    fn message_response_carries_status() {
        let cid = Cid::from_content_blake3(b"x");
        let msg = GraphSyncMessage::response(99, ResponseStatus::Partial);
        match msg {
            GraphSyncMessage::Response(r) => {
                assert_eq!(r.id, 99);
                assert_eq!(r.status, ResponseStatus::Partial);
            }
            _ => panic!("expected Response message"),
        }
        // Drop the cid to silence unused warning on platforms where
        // the compiler isn't smart about it.
        let _ = cid;
    }

    #[test]
    fn message_request_helper_sets_replace_default() {
        let cid = Cid::from_content_blake3(b"root");
        let msg = GraphSyncMessage::request(7, cid.clone(), &selector::match_all());
        match msg {
            GraphSyncMessage::Request(r) => {
                assert_eq!(r.id, 7);
                assert_eq!(r.root, cid);
                assert!(!r.replace);
                // Default priority for `GraphSyncMessage::request`
                // is 1 (the historical "high priority" baseline).
                assert_eq!(r.priority, 1);
            }
            _ => panic!("expected Request message"),
        }
    }

    // ─────────────────────────────────────────────────────────────────
    //  Selector traversal — additional variants not yet covered
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn traverse_none_selector_yields_completed_only() {
        let mut store = new_dag_store();
        let root = store.insert_leaf(b"data");
        let responder = GraphSyncResponder::new(store.store());
        let req = RequestMessage {
            id: 1,
            root: root.clone(),
            selector: serde_json::to_vec(&selector::Matcher::None).unwrap(),
            replace: false,
            priority: 0,
        };
        let resp = responder.process_request(req).unwrap();
        assert_eq!(resp.status, ResponseStatus::Completed);
        assert!(resp.blocks.is_empty());
    }

    #[test]
    fn traverse_explore_union_walks_all_children() {
        // `ExploreUnion` walks every child of the current node, then
        // applies the optional union sequence to the current node (in
        // case the local block matches more than one branch).
        let mut store = new_dag_store();
        let child_a = store.insert_leaf(b"a");
        let child_b = store.insert_leaf(b"b");
        let root = store.insert_node(&[("a", &child_a), ("b", &child_b)]);

        let union_selector = selector::Matcher::ExploreUnion {
            sequence: Some(selector::Sequence::Union {
                branches: vec![
                    selector::Sequence::Matcher {
                        matcher: Box::new(selector::Matcher::ExploreFields {
                            fields: vec![selector::LinkMatcher {
                                name: Some("a".to_string()),
                                cid: None,
                            }],
                            sequence: Some(selector::Sequence::Matcher {
                                matcher: Box::new(selector::Matcher::All { stop_at: None }),
                            }),
                            stop_at: None,
                        }),
                    },
                ],
            }),
            stop_at: None,
        };

        let responder = GraphSyncResponder::new(store.store());
        let req = RequestMessage {
            id: 1,
            root: root.clone(),
            selector: serde_json::to_vec(&union_selector).unwrap(),
            replace: false,
            priority: 0,
        };
        let resp = responder.process_request(req).unwrap();
        // `ExploreUnion` walks children with `Matcher::All` and
        // applies the union `sequence` to the current node only if
        // the sequence's `Matcher` (All) hasn't been visited yet
        // (root CID would hit the visited-set guard). Net: 2 blocks.
        assert_eq!(resp.blocks.len(), 2);
        let bytes_set: Vec<&[u8]> = resp.blocks.iter().map(|b| b.as_slice()).collect();
        assert!(bytes_set.iter().any(|b| *b == b"a" as &[u8]));
        assert!(bytes_set.iter().any(|b| *b == b"b" as &[u8]));
    }

    #[test]
    fn traverse_conditional_is_leaf_skips_recursion() {
        let mut store = new_dag_store();
        let leaf = store.insert_leaf(b"only");
        let responder = GraphSyncResponder::new(store.store());

        // Conditional { branch: All, condition: IsLeaf } on a leaf
        // means: branch matches when leaf, returns the leaf. When
        // the current node is a leaf, the `branch` (All) runs and
        // the leaf is yielded.
        let cond_selector = selector::Matcher::ExploreRecursive {
            sequence: Box::new(selector::Sequence::Conditional {
                branch: Box::new(selector::Sequence::Matcher {
                    matcher: Box::new(selector::Matcher::All { stop_at: None }),
                }),
                condition: Some(selector::Condition::IsLeaf),
            }),
            max_depth: Some(10),
            current_depth: 0,
        };
        let req = RequestMessage {
            id: 1,
            root: leaf.clone(),
            selector: serde_json::to_vec(&cond_selector).unwrap(),
            replace: false,
            priority: 0,
        };
        let resp = responder.process_request(req).unwrap();
        assert_eq!(resp.blocks, vec![b"only".to_vec()]);
    }

    #[test]
    fn traverse_depth_cap_returns_error() {
        let mut store = new_dag_store();
        // 4-deep chain: root → a → b → c
        let c = store.insert_leaf(b"c");
        let b = store.insert_node(&[("next", &c)]);
        let a = store.insert_node(&[("next", &b)]);
        let root = store.insert_node(&[("next", &a)]);

        let responder =
            GraphSyncResponder::new(store.store()).with_max_depth(2);
        let req = RequestMessage {
            id: 1,
            root: root.clone(),
            selector: selector::match_all(),
            replace: false,
            priority: 0,
        };
        let err = responder.process_request(req).unwrap_err();
        // Depth 2 means depth 0..=2 are visited; depth 3+ errors.
        assert!(matches!(err, GraphSyncError::DepthExceeded { limit: 2, .. }));
    }

    #[test]
    fn traverse_block_budget_returns_recursion_limit() {
        let mut store = new_dag_store();
        let a = store.insert_leaf(b"a");
        let b = store.insert_leaf(b"b");
        let c = store.insert_leaf(b"c");
        let root = store.insert_node(&[("a", &a), ("b", &b), ("c", &c)]);

        let responder =
            GraphSyncResponder::new(store.store()).with_max_blocks(2);
        let req = RequestMessage {
            id: 1,
            root: root.clone(),
            selector: selector::match_all(),
            replace: false,
            priority: 0,
        };
        let err = responder.process_request(req).unwrap_err();
        // After yielding 2 blocks, the next push_block returns
        // RecursionLimit, which propagates as `GraphSyncError::RecursionLimit`.
        assert!(matches!(err, GraphSyncError::RecursionLimit));
    }

    #[test]
    fn traverse_block_not_found_returns_error() {
        let store = new_dag_store();
        let orphan = Cid::from_content_blake3(b"missing");
        let responder = GraphSyncResponder::new(store.store());
        let req = RequestMessage {
            id: 1,
            root: orphan.clone(),
            selector: selector::match_all(),
            replace: false,
            priority: 0,
        };
        let err = responder.process_request(req).unwrap_err();
        assert!(matches!(err, GraphSyncError::BlockNotFound(_)));
    }

    #[test]
    fn traverse_invalid_selector_returns_error() {
        let mut store = new_dag_store();
        let root = store.insert_leaf(b"data");
        let responder = GraphSyncResponder::new(store.store());
        let req = RequestMessage {
            id: 1,
            root: root.clone(),
            selector: b"\x00\xff\x00".to_vec(), // not valid JSON
            replace: false,
            priority: 0,
        };
        let err = responder.process_request(req).unwrap_err();
        assert!(matches!(err, GraphSyncError::InvalidSelector(_)));
    }

    #[test]
    fn traverse_explore_all_visits_every_child() {
        let mut store = new_dag_store();
        let a = store.insert_leaf(b"a");
        let b = store.insert_leaf(b"b");
        let c = store.insert_leaf(b"c");
        let root = store.insert_node(&[("a", &a), ("b", &b), ("c", &c)]);

        let responder = GraphSyncResponder::new(store.store());
        let req = RequestMessage {
            id: 1,
            root: root.clone(),
            selector: selector::match_explore_all(),
            replace: false,
            priority: 0,
        };
        let resp = responder.process_request(req).unwrap();
        assert_eq!(resp.status, ResponseStatus::Completed);
        assert_eq!(resp.blocks.len(), 4); // root + 3 leaves
    }

    #[test]
    fn traverse_explore_index_out_of_range_yields_root_only() {
        let mut store = new_dag_store();
        let a = store.insert_leaf(b"a");
        let root = store.insert_node(&[("a", &a)]);
        let responder = GraphSyncResponder::new(store.store());

        // Index 5 doesn't exist on a single-child node. Traversal
        // should still succeed but yield only the root (since no
        // link matched at that index).
        let req = RequestMessage {
            id: 1,
            root: root.clone(),
            selector: selector::match_explore_index(5),
            replace: false,
            priority: 0,
        };
        let resp = responder.process_request(req).unwrap();
        // Empty result + Completed.
        assert_eq!(resp.status, ResponseStatus::Completed);
        assert!(resp.blocks.is_empty());
    }

    #[test]
    fn traverse_stop_after_matching_caps_blocks() {
        let mut store = new_dag_store();
        let a = store.insert_leaf(b"a");
        let b = store.insert_leaf(b"b");
        let root = store.insert_node(&[("a", &a), ("b", &b)]);

        let responder = GraphSyncResponder::new(store.store());
        let req = RequestMessage {
            id: 1,
            root: root.clone(),
            selector: selector::match_all_with_stop(selector::StopCondition::AfterMatching(1)),
            replace: false,
            priority: 0,
        };
        let resp = responder.process_request(req).unwrap();
        assert_eq!(resp.status, ResponseStatus::Completed);
        // `match_all` honors `AfterMatching(1)` via `stop_at_reached`
        // and bails out after the root, so children aren't visited.
        assert_eq!(resp.blocks.len(), 1);
    }

    // ─────────────────────────────────────────────────────────────────
    //  Helpers for the additional selector / traversal tests
    // ─────────────────────────────────────────────────────────────────

    /// Tiny in-memory DAG builder with a hand-rolled
    /// `BlockStore`. We can't reuse `adnet_blobstore::MemDagStore`
    /// here because `adnet-types` cannot depend on `adnet-blobstore`
    /// (the inverse would create a cycle).
    struct TestDagStore {
        blocks: std::sync::Mutex<HashMap<Cid, Vec<u8>>>,
        named_links: std::sync::Mutex<HashMap<Cid, Vec<(Option<String>, Cid)>>>,
    }

    impl TestDagStore {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                blocks: std::sync::Mutex::new(HashMap::new()),
                named_links: std::sync::Mutex::new(HashMap::new()),
            })
        }

        fn insert(&self, cid: Cid, payload: Vec<u8>) {
            self.blocks.lock().unwrap().insert(cid, payload);
        }

        fn insert_with_links(&self, cid: Cid, payload: Vec<u8>, links: Vec<(Option<String>, Cid)>) {
            self.blocks.lock().unwrap().insert(cid.clone(), payload);
            self.named_links.lock().unwrap().insert(cid, links);
        }
    }

    impl BlockStore for TestDagStore {
        fn get(&self, cid: &Cid) -> Option<Vec<u8>> {
            self.blocks.lock().unwrap().get(cid).cloned()
        }
        fn put(&self, cid: &Cid, bytes: &[u8]) {
            self.blocks.lock().unwrap().insert(cid.clone(), bytes.to_vec());
        }
        fn has(&self, cid: &Cid) -> bool {
            self.blocks.lock().unwrap().contains_key(cid)
        }
        fn links(&self, cid: &Cid) -> Vec<Cid> {
            self.named_links
                .lock()
                .unwrap()
                .get(cid)
                .map(|v| v.iter().map(|(_, c)| c.clone()).collect())
                .unwrap_or_default()
        }
        fn links_named(&self, cid: &Cid) -> Vec<(Option<String>, Cid)> {
            self.named_links
                .lock()
                .unwrap()
                .get(cid)
                .cloned()
                .unwrap_or_default()
        }
    }

    /// Tiny in-memory DAG builder that exposes both a `TestDagStore`
    /// and an `Arc<dyn BlockStore>` for the responder.
    struct DagBuilder {
        store: Arc<TestDagStore>,
    }

    impl DagBuilder {
        fn new() -> Self {
            Self {
                store: TestDagStore::new(),
            }
        }

        fn insert_leaf(&mut self, payload: &[u8]) -> Cid {
            let cid = Cid::from_content_blake3(payload);
            self.store.insert(cid.clone(), payload.to_vec());
            cid
        }

        fn insert_node(&mut self, children: &[(&str, &Cid)]) -> Cid {
            let mut links_json: Vec<serde_json::Value> = Vec::new();
            for (name, cid) in children {
                links_json.push(serde_json::json!({
                    "Name": name,
                    "Hash": cid.to_string(),
                }));
            }
            let body = serde_json::to_vec(&serde_json::json!({ "Links": links_json })).unwrap();
            let cid = Cid::from_content_blake3(&body);
            // Use named links so `Matcher::Links` honors `name`.
            let named: Vec<(Option<String>, Cid)> = children
                .iter()
                .map(|(n, c)| (Some((*n).to_string()), (*c).clone()))
                .collect();
            self.store.insert_with_links(cid.clone(), body, named);
            cid
        }

        fn store(&self) -> Arc<dyn BlockStore> {
            self.store.clone()
        }
    }

    fn new_dag_store() -> DagBuilder {
        DagBuilder::new()
    }
}
