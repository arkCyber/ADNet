//! GraphSync requester — manages outgoing DAG sync requests.
//!
//! This module provides a high-level async API for fetching DAGs
//! from remote peers using the GraphSync protocol.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use a3net_types::cid::Cid;
use a3net_types::graphsync::{selector, BlockStore};
use parking_lot::RwLock;

/// Maximum concurrent requests per peer.
pub const MAX_CONCURRENT_REQUESTS: usize = 32;

/// Request priority levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Low = 0,
    Normal = 1,
    High = 2,
    Critical = 3,
}

impl Default for Priority {
    fn default() -> Self {
        Priority::Normal
    }
}

/// Status of a GraphSync request.
#[derive(Debug, Clone)]
pub enum RequestStatus {
    Pending,
    InProgress {
        blocks_received: u64,
        last_progress: Instant,
    },
    Completed {
        blocks_received: u64,
        elapsed: Duration,
    },
    Partial {
        blocks_received: u64,
        blocks_expected: Option<u64>,
        missing_cids: Vec<Cid>,
        elapsed: Duration,
    },
    Failed(String),
    Cancelled,
}

/// Statistics for a completed or in-progress request.
#[derive(Debug, Clone)]
pub struct RequestStats {
    pub request_id: u64,
    pub root_cid: Cid,
    pub status: RequestStatus,
    pub priority: Priority,
    pub started_at: Instant,
    pub blocks_received: u64,
}

impl RequestStats {
    pub fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }
}

/// Options for configuring a GraphSync request.
#[derive(Debug, Clone)]
pub struct RequestOptions {
    pub priority: Priority,
    pub max_blocks: Option<u64>,
    pub timeout: Option<Duration>,
    pub track_missing: bool,
    pub selector: Vec<u8>,
}

impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            priority: Priority::default(),
            max_blocks: None,
            timeout: Some(Duration::from_secs(60)),
            track_missing: true,
            selector: selector::match_all(),
        }
    }
}

impl RequestOptions {
    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn with_max_blocks(mut self, max: u64) -> Self {
        self.max_blocks = Some(max);
        self
    }

    pub fn with_selector(mut self, selector: Vec<u8>) -> Self {
        self.selector = selector;
        self
    }
}

#[derive(Debug)]
struct ActiveRequest {
    request_id: u64,
    root_cid: Cid,
    options: RequestOptions,
    status: RequestStatus,
    started_at: Instant,
    blocks_received: u64,
    missing_cids: Vec<Cid>,
    expected_cids: Option<Vec<Cid>>,
}

/// Events emitted by the GraphSync requester.
#[derive(Debug)]
pub enum RequesterEvent {
    RequestStarted { request_id: u64, root_cid: Cid },
    BlockReceived { request_id: u64, cid: Cid, size: usize },
    RequestCompleted { request_id: u64, stats: RequestStats },
    RequestPartial { request_id: u64, stats: RequestStats },
    RequestFailed { request_id: u64, error: String },
}

/// Error types for GraphSync requests.
#[derive(Debug, thiserror::Error)]
pub enum RequesterError {
    #[error("peer not connected: {0}")]
    PeerNotConnected(String),

    #[error("request timeout after {0:?}")]
    Timeout(Duration),

    #[error("too many concurrent requests: {0}")]
    TooManyRequests(usize),

    #[error("request not found: {0}")]
    RequestNotFound(u64),

    #[error("peer disconnected")]
    PeerDisconnected,

    #[error("internal error: {0}")]
    Internal(String),
}

/// GraphSync requester state manager.
///
/// This struct tracks active requests and provides statistics.
#[derive(Clone)]
pub struct GraphSyncRequester {
    inner: Arc<Inner>,
}

struct Inner {
    store: Arc<dyn BlockStore>,
    requests: RwLock<HashMap<u64, ActiveRequest>>,
    events: RwLock<Vec<RequesterEvent>>,
}

impl GraphSyncRequester {
    /// Create a new GraphSync requester with the given block store.
    pub fn new(store: Arc<dyn BlockStore>) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                requests: RwLock::new(HashMap::new()),
                events: RwLock::new(Vec::new()),
            }),
        }
    }

    /// Register a new request.
    pub fn register_request(&self, request_id: u64, root_cid: Cid, options: RequestOptions) {
        let active_request = ActiveRequest {
            request_id,
            root_cid: root_cid.clone(),
            options,
            status: RequestStatus::Pending,
            started_at: Instant::now(),
            blocks_received: 0,
            missing_cids: Vec::new(),
            expected_cids: None,
        };

        let mut requests = self.inner.requests.write();
        requests.insert(request_id, active_request);

        self.emit_event(RequesterEvent::RequestStarted {
            request_id,
            root_cid,
        });
    }

    /// Update the status of a request to InProgress.
    pub fn start_request(&self, request_id: u64) {
        self.update_status(request_id, RequestStatus::InProgress {
            blocks_received: 0,
            last_progress: Instant::now(),
        });
    }

    /// Record a block received for a request.
    pub fn on_block_received(&self, request_id: u64, cid: &Cid, data: &[u8]) {
        // Store the block
        self.inner.store.put(cid, data);

        // Update stats
        let mut requests = self.inner.requests.write();
        if let Some(req) = requests.get_mut(&request_id) {
            req.blocks_received += 1;
            req.status = RequestStatus::InProgress {
                blocks_received: req.blocks_received,
                last_progress: Instant::now(),
            };
        }
        drop(requests);

        // Emit event
        self.emit_event(RequesterEvent::BlockReceived {
            request_id,
            cid: cid.clone(),
            size: data.len(),
        });
    }

    /// Mark a request as completed.
    pub fn complete_request(&self, request_id: u64) {
        let stats = self.get_stats(request_id);
        self.update_status(request_id, RequestStatus::Completed {
            blocks_received: stats.blocks_received,
            elapsed: stats.elapsed(),
        });
        self.emit_event(RequesterEvent::RequestCompleted {
            request_id,
            stats,
        });
    }

    /// Mark a request as failed.
    pub fn fail_request(&self, request_id: u64, error: String) {
        self.update_status(request_id, RequestStatus::Failed(error.clone()));
        self.emit_event(RequesterEvent::RequestFailed {
            request_id,
            error,
        });
    }

    /// Cancel a request.
    pub fn cancel_request(&self, request_id: u64) -> bool {
        let mut requests = self.inner.requests.write();
        if let Some(req) = requests.get_mut(&request_id) {
            req.status = RequestStatus::Cancelled;
            true
        } else {
            false
        }
    }

    fn update_status(&self, request_id: u64, status: RequestStatus) {
        let mut requests = self.inner.requests.write();
        if let Some(req) = requests.get_mut(&request_id) {
            req.status = status;
        }
    }

    /// Get statistics for a request.
    pub fn get_stats(&self, request_id: u64) -> RequestStats {
        let requests = self.inner.requests.read();
        let req = match requests.get(&request_id) {
            Some(r) => r,
            None => {
                return RequestStats {
                    request_id,
                    root_cid: Cid::from_content_blake3(b"unknown"),
                    status: RequestStatus::Failed("request not found".to_string()),
                    priority: Priority::default(),
                    started_at: Instant::now(),
                    blocks_received: 0,
                };
            }
        };

        RequestStats {
            request_id,
            root_cid: req.root_cid.clone(),
            status: req.status.clone(),
            priority: req.options.priority,
            started_at: req.started_at,
            blocks_received: req.blocks_received,
        }
    }

    /// Get all active requests.
    pub fn active_requests(&self) -> Vec<RequestStats> {
        let requests = self.inner.requests.read();
        requests
            .values()
            .map(|r| RequestStats {
                request_id: r.request_id,
                root_cid: r.root_cid.clone(),
                status: r.status.clone(),
                priority: r.options.priority,
                started_at: r.started_at,
                blocks_received: r.blocks_received,
            })
            .collect()
    }

    /// Drain all accumulated events.
    pub fn drain_events(&self) -> Vec<RequesterEvent> {
        let mut events = self.inner.events.write();
        std::mem::take(&mut events)
    }

    fn emit_event(&self, event: RequesterEvent) {
        let mut events = self.inner.events.write();
        events.push(event);
        if events.len() > 1000 {
            events.drain(0..500);
        }
    }

    /// Get the block store used by this requester.
    pub fn block_store(&self) -> Arc<dyn BlockStore> {
        self.inner.store.clone()
    }

    /// Check concurrent request limit.
    pub fn can_start_request(&self) -> bool {
        let requests = self.inner.requests.read();
        let in_progress = requests
            .values()
            .filter(|r| matches!(r.status, RequestStatus::InProgress { .. }))
            .count();
        in_progress < MAX_CONCURRENT_REQUESTS
    }

    /// Get count of in-progress requests.
    pub fn in_progress_count(&self) -> usize {
        let requests = self.inner.requests.read();
        requests
            .values()
            .filter(|r| matches!(r.status, RequestStatus::InProgress { .. }))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default)]
    struct MockBlockStore {
        blocks: parking_lot::RwLock<std::collections::HashMap<Cid, Vec<u8>>>,
    }

    impl BlockStore for MockBlockStore {
        fn get(&self, cid: &Cid) -> Option<Vec<u8>> {
            self.blocks.read().get(cid).cloned()
        }

        fn put(&self, cid: &Cid, block: &[u8]) {
            self.blocks.write().insert(cid.clone(), block.to_vec());
        }

        fn has(&self, cid: &Cid) -> bool {
            self.blocks.read().contains_key(cid)
        }

        fn links(&self, _cid: &Cid) -> Vec<Cid> {
            Vec::new()
        }
    }

    #[test]
    fn test_request_options_defaults() {
        let opts = RequestOptions::default();
        assert_eq!(opts.priority, Priority::Normal);
        assert!(opts.timeout.is_some());
        assert!(opts.max_blocks.is_none());
        assert!(!opts.selector.is_empty());
    }

    #[test]
    fn test_request_options_builder() {
        let opts = RequestOptions::default()
            .with_priority(Priority::High)
            .with_timeout(Duration::from_secs(30))
            .with_max_blocks(100);

        assert_eq!(opts.priority, Priority::High);
        assert_eq!(opts.timeout, Some(Duration::from_secs(30)));
        assert_eq!(opts.max_blocks, Some(100));
    }

    #[test]
    fn test_priority_ordering() {
        assert!(Priority::Critical > Priority::High);
        assert!(Priority::High > Priority::Normal);
        assert!(Priority::Normal > Priority::Low);
    }

    #[test]
    fn test_requester_lifecycle() {
        let store = Arc::new(MockBlockStore::default());
        let requester = GraphSyncRequester::new(store.clone());

        let root = Cid::from_content_blake3(b"root");
        let options = RequestOptions::default();

        // Register a request
        requester.register_request(1, root.clone(), options);
        assert_eq!(requester.in_progress_count(), 0);

        // Start it
        requester.start_request(1);
        assert_eq!(requester.in_progress_count(), 1);

        // Receive a block
        let cid = Cid::from_content_blake3(b"block1");
        requester.on_block_received(1, &cid, b"data");
        let stats = requester.get_stats(1);
        assert_eq!(stats.blocks_received, 1);
        assert!(store.has(&cid));

        // Complete it
        requester.complete_request(1);
        let stats = requester.get_stats(1);
        assert!(matches!(stats.status, RequestStatus::Completed { .. }));

        // Check events
        let events = requester.drain_events();
        assert!(events.iter().any(|e| matches!(e, RequesterEvent::RequestCompleted { .. })));
    }

    #[test]
    fn test_request_limit() {
        let store = Arc::new(MockBlockStore::default());
        let requester = GraphSyncRequester::new(store.clone());

        // Start max concurrent requests
        for i in 0..MAX_CONCURRENT_REQUESTS {
            requester.register_request(i as u64, Cid::from_content_blake3(b"root"), RequestOptions::default());
            requester.start_request(i as u64);
        }
        assert_eq!(requester.in_progress_count(), MAX_CONCURRENT_REQUESTS);

        // Should not be able to start more
        assert!(!requester.can_start_request());
    }
}
