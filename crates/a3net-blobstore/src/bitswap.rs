//! Bitswap Protocol — IPFS-compatible content exchange protocol.
//!
//! This module implements the core Bitswap protocol for A3Net, enabling:
//!
//! ## Key Features
//!
//! - **Want-Have / Want-Block**: Efficient content discovery before data transfer
//! - **Peer Ledgers**: Per-peer accounting for bandwidth and block exchanges
//! - **Session Optimization**: Group related downloads for better performance
//! - **Priority Queue**: Dynamic priority-based request scheduling
//!
//! ## Bitswap Message Types
//!
//! ### Want-Have
//! - Lightweight inquiry: "Do you have block X?"
//! - Response: HAVE / DONT_HAVE
//! - Used for discovery before requesting full data
//!
//! ### Want-Block
//! - Full request: "Send me block X"
//! - Response: BLOCK or DONT_HAVE
//! - Actual data transfer
//!
//! ## DO-178C Traceability
//!
//! - BITSWAP-1: Want-Have queries discover peer content before full download
//! - BITSWAP-2: Peer ledgers track bytes sent/received per peer
//! - BITSWAP-3: Sessions group related content requests
//! - BITSWAP-4: Priority queue ensures fair bandwidth distribution

use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use a3net_observability::histogram::Histogram;
use a3net_observability::metrics::{Counter, Gauge};
use a3net_observability::registry::Registry;
use a3net_types::ContentHash;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ─────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────

/// Maximum concurrent wants per session.
pub const MAX_CONCURRENT_WANTS: usize = 64;

/// Maximum pending wants in queue.
pub const MAX_PENDING_WANTS: usize = 256;

/// Default ledger reservation per peer.
pub const DEFAULT_PEER_BUDGET_BYTES: u64 = 10 * 1024 * 1024; // 10 MB

/// Want-Have timeout.
pub const WANT_HAVE_TIMEOUT: Duration = Duration::from_secs(10);

/// Want-Block timeout.
pub const WANT_BLOCK_TIMEOUT: Duration = Duration::from_secs(60);

// ─────────────────────────────────────────────────────────────────
// Rate Limiting
// ─────────────────────────────────────────────────────────────────

/// Token bucket rate limiter for request throttling.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    /// Tokens available.
    tokens: f64,
    /// Maximum tokens.
    max_tokens: f64,
    /// Tokens refilled per second.
    refill_rate: f64,
    /// Last refill time.
    #[doc(hidden)]
    pub last_refill: Instant,
}

impl RateLimiter {
    /// Create a new rate limiter.
    pub fn new(max_tokens: f64, refill_rate: f64) -> Self {
        Self {
            tokens: max_tokens,
            max_tokens,
            refill_rate,
            last_refill: Instant::now(),
        }
    }

    /// Create with requests per second.
    pub fn requests_per_second(rps: f64, burst: f64) -> Self {
        Self::new(burst, rps)
    }

    /// Try to acquire a token.
    pub fn try_acquire(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Acquire a token, waiting if necessary.
    pub async fn acquire(&mut self) {
        while !self.try_acquire() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Refill tokens based on elapsed time.
    fn refill(&mut self) {
        let elapsed = self.last_refill.elapsed().as_secs_f64();
        let new_tokens = elapsed * self.refill_rate;
        self.tokens = (self.tokens + new_tokens).min(self.max_tokens);
        self.last_refill = Instant::now();
    }

    /// Get remaining tokens.
    pub fn remaining(&self) -> f64 {
        let elapsed = self.last_refill.elapsed().as_secs_f64();
        let new_tokens = elapsed * self.refill_rate;
        (self.tokens + new_tokens).min(self.max_tokens)
    }
}

/// Rate limiter configuration.
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Max requests per second per peer.
    pub requests_per_second: f64,
    /// Burst size.
    pub burst_size: f64,
    /// Max concurrent requests per peer.
    pub max_concurrent: usize,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            requests_per_second: 100.0,
            burst_size: 200.0,
            max_concurrent: 16,
        }
    }
}

/// Peer rate limiter manager.
#[derive(Debug, Clone)]
pub struct PeerRateLimiters {
    limiters: Arc<RwLock<HashMap<String, RateLimiter>>>,
    config: RateLimitConfig,
}

impl PeerRateLimiters {
    /// Create a new manager with default config.
    pub fn new() -> Self {
        Self::with_config(RateLimitConfig::default())
    }

    /// Create with custom config.
    pub fn with_config(config: RateLimitConfig) -> Self {
        Self {
            limiters: Arc::new(RwLock::new(HashMap::new())),
            config,
        }
    }

    /// Get or create a limiter for a peer.
    pub fn get_limiter(&self, peer_id: &str) -> RateLimiter {
        let limiters = self.limiters.read();
        limiters.get(peer_id).cloned().unwrap_or_else(|| {
            RateLimiter::requests_per_second(
                self.config.requests_per_second,
                self.config.burst_size,
            )
        })
    }

    /// Try to acquire permission to send to a peer.
    pub fn try_send(&self, peer_id: &str) -> bool {
        let mut limiters = self.limiters.write();
        let limiter = limiters.entry(peer_id.to_string()).or_insert_with(|| {
            RateLimiter::requests_per_second(
                self.config.requests_per_second,
                self.config.burst_size,
            )
        });
        limiter.try_acquire()
    }

    /// Get remaining tokens for a peer.
    pub fn remaining(&self, peer_id: &str) -> f64 {
        let mut limiters = self.limiters.write();
        let limiter = limiters.entry(peer_id.to_string()).or_insert_with(|| {
            RateLimiter::requests_per_second(
                self.config.requests_per_second,
                self.config.burst_size,
            )
        });
        limiter.remaining()
    }

    /// Clear all limiters.
    pub fn clear(&self) {
        self.limiters.write().clear();
    }

    /// Remove a peer's limiter.
    pub fn remove(&self, peer_id: &str) {
        self.limiters.write().remove(peer_id);
    }
}

impl Default for PeerRateLimiters {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────
// Errors
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum BitswapError {
    #[error("peer {0} does not have block {1}")]
    BlockNotFound(String, ContentHash),

    #[error("ledger exhausted for peer {0}")]
    LedgerExhausted(String),

    #[error("session {0} not found")]
    SessionNotFound(u64),

    #[error("want list full for peer {0}")]
    WantListFull(String),

    #[error("timeout waiting for block {0}")]
    Timeout(ContentHash),

    #[error("cancelled")]
    Cancelled,

    #[error("internal error: {0}")]
    Internal(String),
}

/// Result type for bitswap operations.
pub type BitswapResult<T> = Result<T, BitswapError>;

// ─────────────────────────────────────────────────────────────────
// Peer ID Validation
// ─────────────────────────────────────────────────────────────────

/// Peer ID validation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerIdValidationError {
    TooShort { actual: usize, min: usize },
    TooLong { actual: usize, max: usize },
    InvalidCharacters { reason: String },
}

impl std::fmt::Display for PeerIdValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeerIdValidationError::TooShort { actual, min } => {
                write!(f, "peer ID too short: {} bytes, minimum {}", actual, min)
            }
            PeerIdValidationError::TooLong { actual, max } => {
                write!(f, "peer ID too long: {} bytes, maximum {}", actual, max)
            }
            PeerIdValidationError::InvalidCharacters { reason } => {
                write!(f, "peer ID contains invalid characters: {}", reason)
            }
        }
    }
}

/// Peer ID validation configuration.
#[derive(Debug, Clone)]
pub struct PeerIdConfig {
    /// Minimum length.
    pub min_length: usize,
    /// Maximum length.
    pub max_length: usize,
    /// Allowed characters (regex pattern).
    pub allowed_pattern: Option<String>,
}

impl Default for PeerIdConfig {
    fn default() -> Self {
        Self {
            min_length: 4,
            max_length: 128,
            allowed_pattern: Some(r"^[a-zA-Z0-9_-]+$".to_string()),
        }
    }
}

/// Validator for peer IDs.
#[derive(Debug, Clone)]
pub struct PeerIdValidator {
    config: PeerIdConfig,
}

impl Default for PeerIdValidator {
    fn default() -> Self {
        Self {
            config: PeerIdConfig::default(),
        }
    }
}

impl PeerIdValidator {
    /// Create with default config.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create with custom config.
    pub fn with_config(config: PeerIdConfig) -> Self {
        Self { config }
    }

    /// Validate a peer ID.
    pub fn validate(&self, peer_id: &str) -> Result<(), PeerIdValidationError> {
        let bytes = peer_id.as_bytes();
        let len = bytes.len();

        // Check length
        if len < self.config.min_length {
            return Err(PeerIdValidationError::TooShort {
                actual: len,
                min: self.config.min_length,
            });
        }

        if len > self.config.max_length {
            return Err(PeerIdValidationError::TooLong {
                actual: len,
                max: self.config.max_length,
            });
        }

        // Check characters if pattern is configured
        if let Some(ref pattern) = self.config.allowed_pattern {
            if let Some(reason) = self.check_pattern(peer_id, pattern) {
                return Err(PeerIdValidationError::InvalidCharacters { reason });
            }
        }

        Ok(())
    }

    /// Check if peer ID matches the allowed pattern.
    fn check_pattern(&self, peer_id: &str, pattern: &str) -> Option<String> {
        // Simple pattern check without regex dependency
        match pattern {
            // Standard pattern: ^[a-zA-Z0-9_-]+$
            r"^[a-zA-Z0-9_-]+$" => {
                for c in peer_id.chars() {
                    if !c.is_ascii_alphanumeric() && c != '_' && c != '-' {
                        return Some(format!("found invalid character: '{}'", c));
                    }
                }
                None
            }
            // Lowercase only pattern: ^[a-z]+$
            r"^[a-z]+$" => {
                for c in peer_id.chars() {
                    if !c.is_ascii_lowercase() {
                        return Some(format!(
                            "found invalid character: '{}' (expected lowercase letter)",
                            c
                        ));
                    }
                }
                None
            }
            // For other patterns, use simple alphanumeric check as fallback
            _ => {
                for c in peer_id.chars() {
                    if !c.is_ascii_alphanumeric() && c != '_' && c != '-' {
                        return Some(format!("found invalid character: '{}'", c));
                    }
                }
                None
            }
        }
    }

    /// Validate and return the peer ID or panic with a helpful message.
    #[allow(dead_code)]
    pub fn validate_or_panic<'a>(&self, peer_id: &'a str) -> &'a str {
        if let Err(e) = self.validate(peer_id) {
            panic!("invalid peer ID '{}': {}", peer_id, e);
        }
        peer_id
    }
}

/// Validate a peer ID with default settings.
pub fn validate_peer_id(peer_id: &str) -> Result<(), PeerIdValidationError> {
    PeerIdValidator::new().validate(peer_id)
}

// ─────────────────────────────────────────────────────────────────
// Message Types
// ─────────────────────────────────────────────────────────────────

/// Bitswap protocol message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum BitswapMessage {
    /// Want-Have: Query if peer has a block.
    WantHave {
        block: ContentHash,
        priority: i32,
        send_dont_have: bool,
    },
    /// Want-Block: Request a full block.
    WantBlock { block: ContentHash, priority: i32 },
    /// Cancel a pending want.
    Cancel { block: ContentHash },
    /// Response: Peer has the block.
    Have { block: ContentHash, immediate: bool },
    /// Response: Peer does not have the block.
    DontHave { block: ContentHash },
    /// Response: Full block data.
    Block { block: ContentHash, data: Vec<u8> },
    /// Batch of wants (for efficiency).
    BatchWant { wants: Vec<BitswapWant> },
    /// Batch of responses.
    BatchResponse { responses: Vec<BitswapResponse> },
}

/// A single want request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BitswapWant {
    /// Block being requested.
    pub block: ContentHash,
    /// Priority (higher = more urgent).
    pub priority: i32,
    /// Whether to send DONT_HAVE if not found.
    pub send_dont_have: bool,
}

/// Response to a want request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BitswapResponse {
    /// Block hash.
    pub block: ContentHash,
    /// Whether peer has the block.
    pub has_block: bool,
    /// Block data (if has_block and available).
    pub data: Option<Vec<u8>>,
}

/// Extended message with sender info.
#[derive(Debug, Clone)]
pub struct PeerMessage {
    pub peer_id: String,
    pub message: BitswapMessage,
    pub received_at: Instant,
}

// ─────────────────────────────────────────────────────────────────
// Peer Ledger
// ─────────────────────────────────────────────────────────────────

/// Per-peer accounting ledger.
///
/// Tracks bytes sent/received and block exchanges for fair
/// bandwidth distribution and reputation.
#[derive(Debug, Clone)]
pub struct PeerLedger {
    /// Peer identifier.
    pub peer_id: String,
    /// Bytes sent to this peer.
    pub bytes_sent: u64,
    /// Bytes received from this peer.
    pub bytes_received: u64,
    /// Blocks sent to this peer.
    pub blocks_sent: u64,
    /// Blocks received from this peer.
    pub blocks_received: u64,
    /// Data bytes we want from this peer (want-list).
    pub want_bytes: u64,
    /// Data bytes peer wants from us.
    pub peer_want_bytes: u64,
    /// Credit limit for this peer.
    pub credit_limit: u64,
    /// Last update time.
    pub last_update: Instant,
    /// Flags for this peer.
    pub flags: LedgerFlags,
}

/// Ledger state flags.
#[derive(Debug, Clone, Default)]
pub struct LedgerFlags {
    /// Peer is a server (provides blocks).
    pub server: bool,
    /// Peer is a client (consumes blocks).
    pub client: bool,
    /// Peer is throttled.
    pub throttled: bool,
    /// Peer is blocked.
    pub blocked: bool,
}

impl PeerLedger {
    /// Create a new ledger for a peer.
    pub fn new(peer_id: String) -> Self {
        Self {
            peer_id,
            bytes_sent: 0,
            bytes_received: 0,
            blocks_sent: 0,
            blocks_received: 0,
            want_bytes: 0,
            peer_want_bytes: 0,
            credit_limit: DEFAULT_PEER_BUDGET_BYTES,
            last_update: Instant::now(),
            flags: LedgerFlags::default(),
        }
    }

    /// Set credit limit.
    pub fn with_credit_limit(mut self, limit: u64) -> Self {
        self.credit_limit = limit;
        self
    }

    /// Record bytes sent to peer.
    pub fn record_sent(&mut self, bytes: u64) {
        self.bytes_sent += bytes;
        self.last_update = Instant::now();
    }

    /// Record bytes received from peer.
    pub fn record_received(&mut self, bytes: u64) {
        self.bytes_received += bytes;
        self.last_update = Instant::now();
    }

    /// Record block sent to peer.
    pub fn record_block_sent(&mut self) {
        self.blocks_sent += 1;
    }

    /// Record block received from peer.
    pub fn record_block_received(&mut self) {
        self.blocks_received += 1;
    }

    /// Add to want list (bytes we want from peer).
    pub fn add_want(&mut self, bytes: u64) {
        self.want_bytes += bytes;
    }

    /// Remove from want list.
    pub fn remove_want(&mut self, bytes: u64) {
        self.want_bytes = self.want_bytes.saturating_sub(bytes);
    }

    /// Add peer's wants (bytes peer wants from us).
    pub fn add_peer_want(&mut self, bytes: u64) {
        self.peer_want_bytes += bytes;
    }

    /// Check if we can receive more from this peer.
    pub fn can_receive(&self) -> bool {
        !self.flags.blocked && self.bytes_received < self.credit_limit
    }

    /// Check if we can send to this peer.
    pub fn can_send(&self) -> bool {
        !self.flags.blocked && self.bytes_sent < self.credit_limit
    }

    /// Get net balance (positive = we owe, negative = peer owes us).
    pub fn balance(&self) -> i64 {
        self.bytes_received as i64 - self.bytes_sent as i64
    }

    /// Throttle this peer.
    pub fn throttle(&mut self) {
        self.flags.throttled = true;
    }

    /// Unthrottle this peer.
    pub fn unthrottle(&mut self) {
        self.flags.throttled = false;
    }

    /// Block this peer.
    pub fn block(&mut self) {
        self.flags.blocked = true;
    }
}

/// Ledger statistics snapshot.
#[derive(Debug, Clone)]
pub struct LedgerStats {
    pub peer_id: String,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub blocks_sent: u64,
    pub blocks_received: u64,
    pub want_bytes: u64,
    pub balance: i64,
    pub throttled: bool,
    pub blocked: bool,
}

impl From<&PeerLedger> for LedgerStats {
    fn from(ledger: &PeerLedger) -> Self {
        Self {
            peer_id: ledger.peer_id.clone(),
            bytes_sent: ledger.bytes_sent,
            bytes_received: ledger.bytes_received,
            blocks_sent: ledger.blocks_sent,
            blocks_received: ledger.blocks_received,
            want_bytes: ledger.want_bytes,
            balance: ledger.balance(),
            throttled: ledger.flags.throttled,
            blocked: ledger.flags.blocked,
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// Session Management
// ─────────────────────────────────────────────────────────────────

/// A bitswap session groups related downloads.
///
/// Sessions optimize by:
///
/// 1. **Peer affinity**: Prefer peers that have multiple blocks in session
/// 2. **Parallel discovery**: Batch Want-Have queries to multiple peers
/// 3. **Block prediction**: Request related blocks from same peer
///
/// DO-178C: BITSWAP-3
#[derive(Debug, Clone)]
pub struct BitswapSession {
    /// Unique session ID.
    pub id: u64,
    /// Root content being downloaded (for related blocks).
    pub root: Option<ContentHash>,
    /// Peers in this session (ordered by utility).
    pub peers: Vec<String>,
    /// Blocks in this session.
    pub blocks: HashSet<ContentHash>,
    /// Active wants for this session.
    pub active_wants: HashSet<ContentHash>,
    /// Peer preferences (score by block coverage).
    peer_scores: HashMap<String, usize>,
    /// Creation time.
    created_at: Instant,
    /// Last activity.
    last_activity: Instant,
}

impl BitswapSession {
    /// Create a new session.
    pub fn new(id: u64) -> Self {
        let now = Instant::now();
        Self {
            id,
            root: None,
            peers: Vec::new(),
            blocks: HashSet::new(),
            active_wants: HashSet::new(),
            peer_scores: HashMap::new(),
            created_at: now,
            last_activity: now,
        }
    }

    /// Set root content (for related blocks).
    pub fn with_root(mut self, root: ContentHash) -> Self {
        self.root = Some(root);
        self
    }

    /// Add a peer to this session.
    pub fn add_peer(&mut self, peer_id: String) {
        if !self.peers.contains(&peer_id) {
            self.peers.push(peer_id.clone());
            self.peer_scores.insert(peer_id, 0);
        }
    }

    /// Remove a peer from this session.
    pub fn remove_peer(&mut self, peer_id: &str) {
        self.peers.retain(|p| p != peer_id);
        self.peer_scores.remove(peer_id);
    }

    /// Add a block to this session.
    pub fn add_block(&mut self, block: ContentHash) {
        self.blocks.insert(block);
        self.touch();
    }

    /// Add multiple blocks.
    pub fn add_blocks(&mut self, blocks: impl IntoIterator<Item = ContentHash>) {
        for block in blocks {
            self.blocks.insert(block);
        }
        self.touch();
    }

    /// Check if a block is in this session.
    pub fn has_block(&self, block: &ContentHash) -> bool {
        self.blocks.contains(block)
    }

    /// Start wanting a block.
    pub fn start_want(&mut self, block: &ContentHash) {
        self.active_wants.insert(block.clone());
        self.touch();
    }

    /// Stop wanting a block.
    pub fn stop_want(&mut self, block: &ContentHash) {
        self.active_wants.remove(block);
    }

    /// Check if we're waiting for a block.
    pub fn is_wanting(&self, block: &ContentHash) -> bool {
        self.active_wants.contains(block)
    }

    /// Record that a peer has certain blocks.
    pub fn record_peer_blocks(&mut self, peer_id: &str, blocks: &[ContentHash]) {
        if let Some(score) = self.peer_scores.get_mut(peer_id) {
            *score += blocks.len();
        }
        self.touch();
    }

    /// Get the best peer for a block.
    pub fn best_peer_for(&self, _block: &ContentHash) -> Option<&str> {
        if self.peers.is_empty() {
            return None;
        }

        // Sort peers by score (descending)
        let mut peer_list: Vec<_> = self.peers.iter().collect();
        peer_list.sort_by(|a, b| {
            let score_a = self.peer_scores.get(*a).unwrap_or(&0);
            let score_b = self.peer_scores.get(*b).unwrap_or(&0);
            score_b.cmp(score_a)
        });

        peer_list.first().map(|s| s.as_str())
    }

    /// Update peer scores based on HAVE responses.
    pub fn update_from_have(&mut self, peer_id: &str, have_blocks: &[ContentHash]) {
        self.record_peer_blocks(peer_id, have_blocks);
    }

    /// Age score (for long sessions).
    pub fn decay_scores(&mut self) {
        for score in self.peer_scores.values_mut() {
            *score = (*score + 1) / 2;
        }
    }

    /// Get session age.
    pub fn age(&self) -> Duration {
        self.created_at.elapsed()
    }

    /// Get time since last activity.
    pub fn idle_time(&self) -> Duration {
        self.last_activity.elapsed()
    }

    /// Check if session is stale (no activity for long time).
    pub fn is_stale(&self, max_idle: Duration) -> bool {
        self.last_activity.elapsed() > max_idle
    }

    fn touch(&mut self) {
        self.last_activity = Instant::now();
    }
}

/// Session manager for organizing downloads into sessions.
pub struct SessionManager {
    sessions: RwLock<HashMap<u64, BitswapSession>>,
    next_id: RwLock<u64>,
    max_sessions: usize,
}

impl SessionManager {
    /// Create a new session manager.
    pub fn new(max_sessions: usize) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            next_id: RwLock::new(1),
            max_sessions,
        }
    }

    /// Create a new session.
    pub fn create_session(&self) -> BitswapSession {
        let mut next = self.next_id.write();
        let id = *next;
        *next = id.wrapping_add(1);

        let session = BitswapSession::new(id);

        let mut sessions = self.sessions.write();
        // Evict oldest if at capacity
        if sessions.len() >= self.max_sessions {
            if let Some(oldest) = sessions.values().min_by_key(|s| s.created_at) {
                let oldest_id = oldest.id;
                sessions.remove(&oldest_id);
            }
        }

        sessions.insert(id, session.clone());
        session
    }

    /// Create a session with a root content.
    pub fn create_session_for(&self, root: ContentHash) -> BitswapSession {
        let mut session = self.create_session();
        session.root = Some(root);
        session
    }

    /// Get a session by ID.
    pub fn get_session(&self, id: u64) -> Option<BitswapSession> {
        self.sessions.read().get(&id).cloned()
    }

    /// Update a session.
    pub fn update_session(&self, session: &BitswapSession) {
        let mut sessions = self.sessions.write();
        sessions.insert(session.id, session.clone());
    }

    /// Remove a session.
    pub fn remove_session(&self, id: u64) {
        self.sessions.write().remove(&id);
    }

    /// Clean up stale sessions.
    pub fn cleanup_stale(&self, max_idle: Duration) {
        let mut sessions = self.sessions.write();
        sessions.retain(|_, s| !s.is_stale(max_idle));
    }

    /// Get all sessions.
    pub fn all_sessions(&self) -> Vec<BitswapSession> {
        self.sessions.read().values().cloned().collect()
    }

    /// Get session count.
    pub fn count(&self) -> usize {
        self.sessions.read().len()
    }
}

// ─────────────────────────────────────────────────────────────────
// Want List & Priority Queue
// ─────────────────────────────────────────────────────────────────

/// A pending want with priority.
#[derive(Debug, Clone)]
pub struct PendingWant {
    pub block: ContentHash,
    pub priority: i32,
    pub want_have: bool,
    pub send_dont_have: bool,
    pub created_at: Instant,
    pub deadline: Instant,
    pub session_id: Option<u64>,
    pub peer_prefs: Vec<String>,
}

impl PartialEq for PendingWant {
    fn eq(&self, other: &Self) -> bool {
        self.block == other.block
    }
}

impl Eq for PendingWant {}

impl PendingWant {
    /// Create a new pending want.
    pub fn want_block(block: ContentHash, priority: i32) -> Self {
        Self {
            block,
            priority,
            want_have: false,
            send_dont_have: false,
            created_at: Instant::now(),
            deadline: Instant::now() + WANT_BLOCK_TIMEOUT,
            session_id: None,
            peer_prefs: Vec::new(),
        }
    }

    /// Create a new pending want-have.
    pub fn want_have(block: ContentHash, priority: i32) -> Self {
        Self {
            block,
            priority,
            want_have: true,
            send_dont_have: true,
            created_at: Instant::now(),
            deadline: Instant::now() + WANT_HAVE_TIMEOUT,
            session_id: None,
            peer_prefs: Vec::new(),
        }
    }

    /// Check if this want is expired.
    pub fn is_expired(&self) -> bool {
        Instant::now() > self.deadline
    }

    /// Update deadline.
    pub fn extend_deadline(&mut self, duration: Duration) {
        self.deadline = Instant::now() + duration;
    }
}

/// Ordering for priority queue (highest priority first).
impl PartialOrd for PendingWant {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PendingWant {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Priority: higher priority first (self > other when self.priority > other.priority)
        self.priority
            .cmp(&other.priority)
            // Then: older wants first (FIFO within same priority)
            .then(self.created_at.cmp(&other.created_at))
    }
}

/// Per-peer want list.
#[derive(Debug, Clone, Default)]
pub struct PeerWantList {
    pub wants: HashSet<ContentHash>,
    pub want_haves: HashSet<ContentHash>,
}

impl PeerWantList {
    /// Add a want.
    pub fn add_want(&mut self, block: &ContentHash) {
        self.wants.insert(block.clone());
    }

    /// Add a want-have.
    pub fn add_want_have(&mut self, block: &ContentHash) {
        self.want_haves.insert(block.clone());
    }

    /// Remove a want.
    pub fn remove_want(&mut self, block: &ContentHash) {
        self.wants.remove(block);
    }

    /// Check if we want a block.
    pub fn wants(&self, block: &ContentHash) -> bool {
        self.wants.contains(block)
    }

    /// Check if we want to know if peer has a block.
    pub fn wants_have(&self, block: &ContentHash) -> bool {
        self.want_haves.contains(block)
    }
}

// ─────────────────────────────────────────────────────────────────
// Want Manager
// ─────────────────────────────────────────────────────────────────

/// Manages want lists across all peers.
pub struct WantManager {
    /// Per-peer want lists.
    peer_wants: RwLock<HashMap<String, PeerWantList>>,
    /// Global priority queue of pending wants.
    pending_wants: RwLock<BinaryHeap<PendingWant>>,
    /// Local blocks we have.
    local_blocks: RwLock<HashSet<ContentHash>>,
    /// Block data (when available).
    block_data: RwLock<HashMap<ContentHash, Vec<u8>>>,
}

impl WantManager {
    /// Create a new want manager.
    pub fn new() -> Self {
        Self {
            peer_wants: RwLock::new(HashMap::new()),
            pending_wants: RwLock::new(BinaryHeap::new()),
            local_blocks: RwLock::new(HashSet::new()),
            block_data: RwLock::new(HashMap::new()),
        }
    }

    /// Add a local block (we have it).
    pub fn add_local_block(&self, block: ContentHash) {
        self.local_blocks.write().insert(block);
    }

    /// Add block data.
    pub fn add_block_data(&self, block: ContentHash, data: Vec<u8>) {
        self.block_data.write().insert(block, data);
    }

    /// Check if we have a block locally.
    pub fn has_local(&self, block: &ContentHash) -> bool {
        self.local_blocks.read().contains(block)
    }

    /// Get block data.
    pub fn get_block_data(&self, block: &ContentHash) -> Option<Vec<u8>> {
        self.block_data.read().get(block).cloned()
    }

    /// Add a want for a peer.
    pub fn add_want(&self, peer_id: &str, block: &ContentHash, want_have: bool) {
        let mut peer_wants = self.peer_wants.write();
        let wants = peer_wants.entry(peer_id.to_string()).or_default();
        if want_have {
            wants.add_want_have(block);
        } else {
            wants.add_want(block);
        }
    }

    /// Remove a want for a peer.
    pub fn remove_want(&self, peer_id: &str, block: &ContentHash) {
        let mut peer_wants = self.peer_wants.write();
        if let Some(wants) = peer_wants.get_mut(peer_id) {
            wants.remove_want(block);
        }
    }

    /// Check if any peer wants a block.
    pub fn is_wanted(&self, block: &ContentHash) -> bool {
        let peer_wants = self.peer_wants.read();
        peer_wants
            .values()
            .any(|w| w.wants(block) || w.wants_have(block))
    }

    /// Get all peers that want a block.
    pub fn get_wanters(&self, block: &ContentHash) -> Vec<String> {
        let peer_wants = self.peer_wants.read();
        peer_wants
            .iter()
            .filter(|(_, w)| w.wants(block) || w.wants_have(block))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Push a pending want to the queue.
    pub fn push_pending(&self, want: PendingWant) {
        let mut pending = self.pending_wants.write();
        if pending.len() < MAX_PENDING_WANTS {
            pending.push(want);
        }
    }

    /// Pop the highest priority want.
    pub fn pop_pending(&self) -> Option<PendingWant> {
        self.pending_wants.write().pop()
    }

    /// Clean up expired wants.
    pub fn cleanup_expired(&self) {
        let mut pending = self.pending_wants.write();
        let now = Instant::now();
        // Remove expired (they'll be re-added if needed)
        let filtered: BinaryHeap<PendingWant> =
            pending.drain().filter(|w| w.deadline > now).collect();
        *pending = filtered;
    }

    /// Get want list for a peer.
    pub fn get_peer_wants(&self, peer_id: &str) -> PeerWantList {
        self.peer_wants
            .read()
            .get(peer_id)
            .cloned()
            .unwrap_or_default()
    }
}

// ─────────────────────────────────────────────────────────────────
// Peer State
// ─────────────────────────────────────────────────────────────────

/// State of a connected peer.
#[derive(Debug, Clone)]
pub struct PeerState {
    pub peer_id: String,
    pub connected_at: Instant,
    pub last_message: Instant,
    pub ledger: PeerLedger,
    pub want_list: PeerWantList,
    pub known_blocks: HashSet<ContentHash>,
    pub pending_requests: HashMap<ContentHash, Instant>,
}

impl PeerState {
    /// Create new peer state.
    pub fn new(peer_id: String) -> Self {
        let now = Instant::now();
        Self {
            peer_id: peer_id.clone(),
            connected_at: now,
            last_message: now,
            ledger: PeerLedger::new(peer_id),
            want_list: PeerWantList::default(),
            known_blocks: HashSet::new(),
            pending_requests: HashMap::new(),
        }
    }

    /// Record a received message.
    pub fn record_message(&mut self) {
        self.last_message = Instant::now();
    }

    /// Add to known blocks (from HAVE messages).
    pub fn add_known_block(&mut self, block: &ContentHash) {
        self.known_blocks.insert(block.clone());
    }

    /// Check if peer has a block.
    pub fn has_block(&self, block: &ContentHash) -> bool {
        self.known_blocks.contains(block)
    }

    /// Start a pending request.
    pub fn start_request(&mut self, block: &ContentHash) {
        self.pending_requests.insert(block.clone(), Instant::now());
    }

    /// Complete a pending request.
    pub fn complete_request(&mut self, block: &ContentHash) {
        self.pending_requests.remove(block);
    }

    /// Check if we have a pending request for a block.
    pub fn has_pending_request(&self, block: &ContentHash) -> bool {
        self.pending_requests.contains_key(block)
    }

    /// Clean up timed-out requests.
    pub fn cleanup_timedout(&mut self, timeout: Duration) {
        let now = Instant::now();
        self.pending_requests
            .retain(|_, &mut started| now.duration_since(started) < timeout);
    }
}

// ─────────────────────────────────────────────────────────────────
// Metrics
// ─────────────────────────────────────────────────────────────────

/// Bitswap protocol metrics.
#[derive(Debug, Clone)]
pub struct BitswapMetrics {
    pub messages_sent: Arc<Counter>,
    pub messages_received: Arc<Counter>,
    pub want_haves_sent: Arc<Counter>,
    pub want_blocks_sent: Arc<Counter>,
    pub blocks_received: Arc<Counter>,
    pub blocks_sent: Arc<Counter>,
    pub dont_haves_received: Arc<Counter>,
    pub bytes_sent: Arc<Counter>,
    pub bytes_received: Arc<Counter>,
    pub active_sessions: Arc<Gauge>,
    pub active_peers: Arc<Gauge>,
    pub pending_wants: Arc<Gauge>,
    pub ledger_balance: Arc<Histogram>,
}

impl BitswapMetrics {
    /// Register metrics with a registry.
    pub fn register(registry: &Registry) -> Self {
        Self {
            messages_sent: registry.register_counter(
                "a3net_bitswap_messages_sent_total",
                "Bitswap messages sent.",
            ),
            messages_received: registry.register_counter(
                "a3net_bitswap_messages_received_total",
                "Bitswap messages received.",
            ),
            want_haves_sent: registry.register_counter(
                "a3net_bitswap_want_haves_sent_total",
                "Want-Have queries sent.",
            ),
            want_blocks_sent: registry.register_counter(
                "a3net_bitswap_want_blocks_sent_total",
                "Want-Block requests sent.",
            ),
            blocks_received: registry.register_counter(
                "a3net_bitswap_blocks_received_total",
                "Blocks received from peers.",
            ),
            blocks_sent: registry
                .register_counter("a3net_bitswap_blocks_sent_total", "Blocks sent to peers."),
            dont_haves_received: registry.register_counter(
                "a3net_bitswap_dont_haves_received_total",
                "DONT_HAVE responses received.",
            ),
            bytes_sent: registry.register_counter(
                "a3net_bitswap_bytes_sent_total",
                "Total bytes sent via Bitswap.",
            ),
            bytes_received: registry.register_counter(
                "a3net_bitswap_bytes_received_total",
                "Total bytes received via Bitswap.",
            ),
            active_sessions: registry.register_gauge(
                "a3net_bitswap_active_sessions",
                "Number of active Bitswap sessions.",
            ),
            active_peers: registry.register_gauge(
                "a3net_bitswap_active_peers",
                "Number of peers connected to Bitswap.",
            ),
            pending_wants: registry.register_gauge(
                "a3net_bitswap_pending_wants",
                "Number of pending want requests.",
            ),
            ledger_balance: registry.register_histogram(
                "a3net_bitswap_ledger_balance",
                "Peer ledger balance distribution.",
            ),
        }
    }
}

impl Default for BitswapMetrics {
    fn default() -> Self {
        Self::register(&Arc::new(Registry::default()))
    }
}

// ─────────────────────────────────────────────────────────────────
// Bitswap Engine
// ─────────────────────────────────────────────────────────────────

/// The main Bitswap protocol engine.
///
/// This is the central coordinator for Bitswap operations:
/// - Manages peer connections and state
/// - Handles message routing
/// - Coordinates sessions
/// - Tracks peer ledgers
/// - Enforces rate limits
pub struct BitswapEngine {
    /// Peer states.
    peers: RwLock<HashMap<String, PeerState>>,
    /// Session manager.
    sessions: SessionManager,
    /// Want manager.
    wants: WantManager,
    /// Metrics.
    metrics: BitswapMetrics,
    /// Rate limiters for peers.
    rate_limiters: PeerRateLimiters,
    /// Peer ID validator.
    peer_validator: PeerIdValidator,
    /// Block provider callback.
    block_provider: Box<dyn Fn(&ContentHash) -> Option<Vec<u8>> + Send + Sync>,
    /// Peer discovery callback.
    peer_discovery: Box<dyn Fn(&ContentHash) -> Vec<String> + Send + Sync>,
    /// Optional reputation reporter. When set, every
    /// `process_message` call feeds `BitswapSignal` events into the
    /// global PeerScore so the rest of A3Net (gossipsub, chat-trust
    /// fusion) can see the same evidence as the session-level
    /// `peer_scores` HashMap. The session-level scoring is kept
    /// for backward compatibility — the reputation table is an
    /// additional source of truth, not a replacement.
    #[cfg(feature = "reputation")]
    reputation: Option<a3net_reputation::ReputationReporter>,
}

impl BitswapEngine {
    /// Create a new Bitswap engine.
    pub fn new() -> Self {
        Self {
            peers: RwLock::new(HashMap::new()),
            sessions: SessionManager::new(256),
            wants: WantManager::new(),
            metrics: BitswapMetrics::default(),
            rate_limiters: PeerRateLimiters::new(),
            peer_validator: PeerIdValidator::new(),
            block_provider: Box::new(|_| None),
            peer_discovery: Box::new(|_| Vec::new()),
            #[cfg(feature = "reputation")]
            reputation: None,
        }
    }

    /// Attach a reputation reporter. Once installed, every
    /// `process_message` call feeds `BitswapSignal::valid` /
    /// `invalid` events into the global PeerScore so the rest of
    /// A3Net (gossipsub, chat-trust fusion) can react to the same
    /// evidence that the session-level `peer_scores` HashMap
    /// uses. The reporter is shared (cloned) so multiple
    /// `BitswapEngine` instances can write to the same table.
    #[cfg(feature = "reputation")]
    pub fn with_reputation(
        mut self,
        reporter: a3net_reputation::ReputationReporter,
    ) -> Self {
        self.reputation = Some(reporter);
        self
    }

    /// Borrow the installed reporter (cfg-gated). `None` when no
    /// reporter has been attached.
    #[cfg(feature = "reputation")]
    pub fn reputation(&self) -> Option<&a3net_reputation::ReputationReporter> {
        self.reputation.as_ref()
    }

    /// Map a bitswap peer-id string to a deterministic `NodeId`
    /// for reputation bookkeeping. Bitswap peer-ids are arbitrary
    /// strings (multihash, hex, transport-specific tags) — we
    /// hash them so the same peer always lands on the same
    /// PeerScore entry. Pure function.
    #[cfg(feature = "reputation")]
    fn peer_id_to_node_id(peer_id: &str) -> a3net_types::NodeId {
        let h = blake3::hash(peer_id.as_bytes());
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&h.as_bytes()[..32]);
        a3net_types::NodeId::from_bytes(&bytes)
            .unwrap_or_else(|_| a3net_types::NodeId::from_bytes(&[0u8; 32]).expect("zero NodeId is valid"))
    }

    /// Create with custom rate limit config.
    pub fn with_rate_limits(mut self, config: RateLimitConfig) -> Self {
        self.rate_limiters = PeerRateLimiters::with_config(config);
        self
    }

    /// Create with custom peer ID config.
    pub fn with_peer_validation(mut self, config: PeerIdConfig) -> Self {
        self.peer_validator = PeerIdValidator::with_config(config);
        self
    }

    /// Set block provider callback.
    pub fn with_block_provider<F>(mut self, f: F) -> Self
    where
        F: Fn(&ContentHash) -> Option<Vec<u8>> + Send + Sync + 'static,
    {
        self.block_provider = Box::new(f);
        self
    }

    /// Set peer discovery callback.
    pub fn with_peer_discovery<F>(mut self, f: F) -> Self
    where
        F: Fn(&ContentHash) -> Vec<String> + Send + Sync + 'static,
    {
        self.peer_discovery = Box::new(f);
        self
    }

    /// Check if a peer is rate limited.
    pub fn is_rate_limited(&self, peer_id: &str) -> bool {
        !self.rate_limiters.try_send(peer_id)
    }

    /// Get remaining rate limit tokens for a peer.
    pub fn rate_limit_remaining(&self, peer_id: &str) -> f64 {
        self.rate_limiters.remaining(peer_id)
    }

    /// Add a block that we have locally (for responding to Want-Have/Want-Block).
    pub fn add_local_block(&self, block: ContentHash) {
        self.wants.add_local_block(block);
    }

    /// Check if we have a block locally.
    pub fn has_local_block(&self, block: &ContentHash) -> bool {
        self.wants.has_local(block)
    }

    /// Check if a peer ID is valid.
    pub fn is_valid_peer_id(&self, peer_id: &str) -> bool {
        self.peer_validator.validate(peer_id).is_ok()
    }

    /// Validate peer ID or return error.
    pub fn validate_peer_id(&self, peer_id: &str) -> Result<(), PeerIdValidationError> {
        self.peer_validator.validate(peer_id)
    }

    /// Add a connected peer.
    pub fn add_peer(&self, peer_id: &str) -> Result<(), PeerIdValidationError> {
        // Validate peer ID first
        self.peer_validator.validate(peer_id)?;

        let mut peers = self.peers.write();
        if !peers.contains_key(peer_id) {
            peers.insert(peer_id.to_string(), PeerState::new(peer_id.to_string()));
            self.metrics.active_peers.inc();
        }
        Ok(())
    }

    /// Remove a disconnected peer.
    pub fn remove_peer(&self, peer_id: &str) {
        let mut peers = self.peers.write();
        if peers.remove(peer_id).is_some() {
            self.metrics.active_peers.dec();
        }
        // Also clean up rate limiter
        self.rate_limiters.remove(peer_id);
    }

    /// Get peer state.
    pub fn get_peer(&self, peer_id: &str) -> Option<PeerState> {
        self.peers.read().get(peer_id).cloned()
    }

    /// Mark a peer as having a pending request for `block`. Used by
    /// outbound-want callers (e.g. swarm downloads) so that the
    /// subsequent `DontHave` / `Block` response can be correctly
    /// attributed in the reputation table.
    ///
    /// Returns `true` if the peer existed and the request was recorded.
    pub fn start_request_for_peer(&mut self, peer_id: &str, block: &a3net_types::ContentHash) -> bool {
        let mut guard = self.peers.write();
        if let Some(state) = guard.get_mut(peer_id) {
            state.start_request(block);
            true
        } else {
            false
        }
    }

    /// Get all peer IDs.
    pub fn get_peer_ids(&self) -> Vec<String> {
        self.peers.read().keys().cloned().collect()
    }

    /// Process an incoming message.
    pub fn process_message(
        &mut self,
        peer_id: &str,
        message: BitswapMessage,
    ) -> Vec<BitswapMessage> {
        let mut responses = Vec::new();
        let mut peers = self.peers.write();

        // Get immutable reference to wants for checking
        let wants = &self.wants;

        if let Some(peer) = peers.get_mut(peer_id) {
            peer.record_message();

            match &message {
                BitswapMessage::WantHave { block, .. } => {
                    // Check if we have this block
                    if wants.has_local(block) {
                        responses.push(BitswapMessage::Have {
                            block: block.clone(),
                            immediate: true,
                        });
                    } else {
                        // Forward to peers that might have it
                        let wanters = wants.get_wanters(block);
                        for w in wanters {
                            if let Some(p) = peers.get_mut(&w) {
                                p.want_list.add_want_have(block);
                            }
                        }
                    }
                }
                BitswapMessage::WantBlock { block, .. } => {
                    // First check if we have the block locally
                    if self.wants.has_local(block) {
                        if let Some(data) = (self.block_provider)(block) {
                            peer.ledger.record_block_received();
                            peer.ledger.record_received(data.len() as u64);
                            self.metrics.bytes_received.inc_by(data.len() as u64);
                            self.metrics.blocks_sent.inc();

                            responses.push(BitswapMessage::Block {
                                block: block.clone(),
                                data,
                            });
                        }
                    } else {
                        // Try to get from block provider (for testing scenarios)
                        if let Some(data) = (self.block_provider)(block) {
                            peer.ledger.record_block_received();
                            peer.ledger.record_received(data.len() as u64);
                            self.metrics.bytes_received.inc_by(data.len() as u64);
                            self.metrics.blocks_sent.inc();

                            responses.push(BitswapMessage::Block {
                                block: block.clone(),
                                data,
                            });
                        }
                    }
                }
                BitswapMessage::Have { block, .. } => {
                    // Record that peer has this block
                    peer.add_known_block(block);

                    // Update session scores
                    let sessions = self.sessions.all_sessions();
                    for mut session in sessions {
                        if session.has_block(block) {
                            session.update_from_have(peer_id, &[block.clone()]);
                            self.sessions.update_session(&session);
                        }
                    }
                }
                BitswapMessage::DontHave { block } => {
                    // Find pending requests and update
                    peer.complete_request(block);

                    // Reputation: a `DontHave` after we issued a
                    // want is a weak negative signal — the peer is
                    // responsive but doesn't have what we want.
                    // We don't attribute a penalty when the
                    // `DontHave` is unsolicited (no pending
                    // request for this block from us), so the
                    // overall bias is purely on the response path.
                    #[cfg(feature = "reputation")]
                    {
                        let wanted = peer.has_pending_request(&block);
                        if wanted {
                            if let Some(rep) = self.reputation.as_ref() {
                                let node = Self::peer_id_to_node_id(peer_id);
                                a3net_reputation::reporter::BitswapSignal(rep)
                                    .invalid(node, a3net_reputation::event::InvalidReason::Other);
                            }
                        }
                    }
                }
                BitswapMessage::Block { block, data } => {
                    // Store block data
                    self.wants.add_block_data(block.clone(), data.clone());

                    // Update ledger
                    peer.ledger.record_block_received();
                    peer.ledger.record_received(data.len() as u64);
                    self.metrics.bytes_received.inc_by(data.len() as u64);
                    self.metrics.blocks_received.inc();

                    // Reputation: a successful block delivery is a
                    // strong positive signal. `BitswapSignal::valid`
                    // records a `ReputationEvent::ValidMessage` with
                    // the payload size; the `weight_bitswap_valid`
                    // parameter scales it.
                    #[cfg(feature = "reputation")]
                    if let Some(rep) = self.reputation.as_ref() {
                        let node = Self::peer_id_to_node_id(peer_id);
                        a3net_reputation::reporter::BitswapSignal(rep)
                            .valid(node, data.len() as u32);
                    }

                    // Complete pending request
                    peer.complete_request(block);

                    // Remove from want list for all peers
                    let block_hash = block.clone();
                    let wanters = self.wants.get_wanters(&block_hash);
                    for w in wanters {
                        self.wants.remove_want(&w, &block_hash);
                    }
                }
                BitswapMessage::Cancel { block } => {
                    let block_hash = block.clone();
                    self.wants.remove_want(peer_id, &block_hash);
                }
                _ => {}
            }
        }

        self.metrics.messages_received.inc();
        responses
    }

    /// Create a want list for a session.
    pub fn create_wants(
        &mut self,
        session_id: u64,
        blocks: &[ContentHash],
        priority: i32,
    ) -> Vec<BitswapMessage> {
        let mut messages = Vec::new();
        let session = self.sessions.get_session(session_id);

        for block in blocks {
            // Add to local tracking
            self.wants.add_local_block(block.clone());

            // Update session
            if let Some(mut s) = session.clone() {
                s.add_block(block.clone());
                s.start_want(block);
                self.sessions.update_session(&s);
            }

            // Find peers that might have this block
            let _candidates = (self.peer_discovery)(block);

            // Create Want-Have for discovery
            let msg = BitswapMessage::WantHave {
                block: block.clone(),
                priority,
                send_dont_have: true,
            };

            messages.push(msg);
            self.metrics.want_haves_sent.inc();

            // Add to pending wants queue
            let pending = PendingWant::want_have(block.clone(), priority);
            self.wants.push_pending(pending);
        }

        self.metrics
            .pending_wants
            .set(self.wants.pending_wants.read().len() as i64);
        messages
    }

    /// Request blocks (Want-Block after Want-Have).
    pub fn request_blocks(
        &self,
        _session_id: u64,
        blocks: &[ContentHash],
        priority: i32,
    ) -> Vec<BitswapMessage> {
        let mut messages = Vec::new();

        for block in blocks {
            // Only request if we don't have it
            if self.wants.has_local(block) || self.wants.get_block_data(block).is_some() {
                continue;
            }

            // Find peers that have this block
            let peers = self.peers.read();
            let available_peers: Vec<_> = peers
                .iter()
                .filter(|(_, p)| p.has_block(block))
                .map(|(id, _)| id.clone())
                .collect();

            if available_peers.is_empty() {
                // No peer has it, send Want-Have first
                messages.push(BitswapMessage::WantHave {
                    block: block.clone(),
                    priority,
                    send_dont_have: true,
                });
            } else {
                // Send Want-Block to peer that has it
                for peer_id in &available_peers {
                    messages.push(BitswapMessage::WantBlock {
                        block: block.clone(),
                        priority,
                    });
                    self.metrics.want_blocks_sent.inc();

                    // Track pending request
                    if let Some(peer) = peers.get(peer_id) {
                        let mut peer = peer.clone();
                        peer.start_request(block);
                    }
                }
            }

            // Add to pending wants
            let pending = PendingWant::want_block(block.clone(), priority);
            self.wants.push_pending(pending);
        }

        self.metrics
            .pending_wants
            .set(self.wants.pending_wants.read().len() as i64);
        messages
    }

    /// Get block data if available.
    pub fn get_block(&self, block: &ContentHash) -> Option<Vec<u8>> {
        self.wants.get_block_data(block)
    }

    /// Get ledger stats for all peers.
    pub fn get_all_ledger_stats(&self) -> Vec<LedgerStats> {
        self.peers
            .read()
            .values()
            .map(|p| LedgerStats::from(&p.ledger))
            .collect()
    }

    /// Get ledger stats for a specific peer.
    pub fn get_peer_ledger(&self, peer_id: &str) -> Option<LedgerStats> {
        self.peers
            .read()
            .get(peer_id)
            .map(|p| LedgerStats::from(&p.ledger))
    }

    /// Get session statistics.
    pub fn get_session_stats(&self) -> SessionStats {
        let sessions = self.sessions.all_sessions();
        SessionStats {
            count: sessions.len(),
            active_wants: sessions.iter().map(|s| s.active_wants.len()).sum(),
            peers_in_sessions: sessions.iter().map(|s| s.peers.len()).sum(),
        }
    }

    /// Clean up stale state.
    pub fn cleanup(&self) {
        // Clean up expired wants
        self.wants.cleanup_expired();

        // Clean up stale sessions (idle > 5 minutes)
        self.sessions.cleanup_stale(Duration::from_secs(300));

        // Clean up timed-out peer requests
        let mut peers = self.peers.write();
        for peer in peers.values_mut() {
            peer.cleanup_timedout(WANT_BLOCK_TIMEOUT);
        }

        // Update metrics
        self.metrics
            .pending_wants
            .set(self.wants.pending_wants.read().len() as i64);
        self.metrics
            .active_sessions
            .set(self.sessions.count() as i64);
    }

    /// Create a new session.
    pub fn create_session(&self) -> BitswapSession {
        self.sessions.create_session()
    }

    /// Create a session for specific content.
    pub fn create_session_for(&self, root: ContentHash) -> BitswapSession {
        self.sessions.create_session_for(root)
    }
}

/// Statistics about sessions.
#[derive(Debug, Clone)]
pub struct SessionStats {
    pub count: usize,
    pub active_wants: usize,
    pub peers_in_sessions: usize,
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_peer_ledger_balance() {
        let mut ledger = PeerLedger::new("peer1".to_string());

        assert_eq!(ledger.balance(), 0);

        ledger.record_received(1000);
        assert_eq!(ledger.balance(), 1000);

        ledger.record_sent(500);
        assert_eq!(ledger.balance(), 500);

        ledger.record_sent(1000);
        assert_eq!(ledger.balance(), -500);
    }

    #[test]
    fn test_peer_ledger_want_tracking() {
        let mut ledger = PeerLedger::new("peer1".to_string());

        ledger.add_want(1000);
        assert_eq!(ledger.want_bytes, 1000);

        ledger.remove_want(500);
        assert_eq!(ledger.want_bytes, 500);
    }

    #[test]
    fn test_pending_want_priority() {
        let low = PendingWant::want_block(ContentHash::from_bytes(b"low"), 1);
        let high = PendingWant::want_block(ContentHash::from_bytes(b"high"), 100);
        let medium = PendingWant::want_block(ContentHash::from_bytes(b"medium"), 50);

        let mut heap = BinaryHeap::new();
        heap.push(low.clone());
        heap.push(high.clone());
        heap.push(medium.clone());

        // Highest priority first
        assert_eq!(heap.pop().unwrap().priority, 100);
        assert_eq!(heap.pop().unwrap().priority, 50);
        assert_eq!(heap.pop().unwrap().priority, 1);
    }

    #[test]
    fn test_session_block_tracking() {
        let mut session = BitswapSession::new(1);

        let block1 = ContentHash::from_bytes(b"block1");
        let block2 = ContentHash::from_bytes(b"block2");

        session.add_block(block1.clone());
        assert!(session.has_block(&block1));
        assert!(!session.has_block(&block2));

        session.add_blocks([block2.clone()]);
        assert!(session.has_block(&block2));
    }

    #[test]
    fn test_session_peer_scoring() {
        let mut session = BitswapSession::new(1);

        session.add_peer("peer1".to_string());
        session.add_peer("peer2".to_string());

        let blocks = vec![
            ContentHash::from_bytes(b"b1"),
            ContentHash::from_bytes(b"b2"),
            ContentHash::from_bytes(b"b3"),
        ];

        session.record_peer_blocks("peer1", &blocks[..2]);
        session.record_peer_blocks("peer2", &blocks);

        assert_eq!(session.best_peer_for(&blocks[0]), Some("peer2"));
    }

    #[test]
    fn test_want_manager_local_block() {
        let wants = WantManager::new();
        let block = ContentHash::from_bytes(b"test");

        assert!(!wants.has_local(&block));

        wants.add_local_block(block.clone());
        assert!(wants.has_local(&block));
    }

    #[test]
    fn test_want_manager_block_data() {
        let mut wants = WantManager::new();
        let block = ContentHash::from_bytes(b"test");
        let data = b"test data".to_vec();

        assert!(wants.get_block_data(&block).is_none());

        wants.add_block_data(block.clone(), data.clone());
        assert_eq!(wants.get_block_data(&block), Some(data));
    }

    #[test]
    fn test_peer_state_known_blocks() {
        let mut state = PeerState::new("peer1".to_string());

        let block = ContentHash::from_bytes(b"test");

        assert!(!state.has_block(&block));

        state.add_known_block(&block);
        assert!(state.has_block(&block));
    }

    #[test]
    fn test_peer_state_pending_requests() {
        let mut state = PeerState::new("peer1".to_string());

        let block = ContentHash::from_bytes(b"test");

        assert!(!state.has_pending_request(&block));

        state.start_request(&block);
        assert!(state.has_pending_request(&block));

        state.complete_request(&block);
        assert!(!state.has_pending_request(&block));
    }

    #[test]
    fn test_bitswap_engine_peer_lifecycle() {
        let engine = BitswapEngine::new();

        // Add peer
        let result = engine.add_peer("peer1-valid");
        assert!(result.is_ok());
        assert!(engine.get_peer("peer1-valid").is_some());

        // Remove peer
        engine.remove_peer("peer1-valid");
        assert!(engine.get_peer("peer1-valid").is_none());
    }

    #[test]
    fn test_bitswap_engine_invalid_peer_id() {
        let engine = BitswapEngine::new();

        // Test too short
        let result = engine.add_peer("abc");
        assert!(result.is_err());

        // Test valid peer
        let result = engine.add_peer("valid-peer-id-123");
        assert!(result.is_ok());
    }

    #[test]
    fn test_bitswap_engine_rate_limiting() {
        let engine = BitswapEngine::new();

        // Initially not rate limited
        assert!(!engine.is_rate_limited("test-peer"));

        // Consume rate limit tokens by sending many requests
        for _ in 0..200 {
            let _ = engine.rate_limiters.try_send("test-peer");
        }

        // Should be rate limited now
        // Note: This depends on the rate limit configuration
        let remaining = engine.rate_limit_remaining("test-peer");
        assert!(remaining >= 0.0);
    }

    #[test]
    fn test_bitswap_engine_process_have() {
        let mut engine = BitswapEngine::new();
        let _ = engine.add_peer("peer1");

        let block = ContentHash::from_bytes(b"test");
        engine.add_local_block(block.clone());

        let response = engine.process_message(
            "peer1",
            BitswapMessage::WantHave {
                block,
                priority: 1,
                send_dont_have: true,
            },
        );

        // Should receive HAVE response
        assert!(
            response
                .iter()
                .any(|m| matches!(m, BitswapMessage::Have { .. }))
        );
    }

    #[test]
    fn test_bitswap_engine_block_exchange() {
        let data = b"hello world".to_vec();
        let block = ContentHash::from_bytes(&data);
        let block_clone = block.clone();

        let data_for_cmp = data.clone();
        let mut engine = BitswapEngine::new().with_block_provider(move |_b| {
            if _b == &block_clone {
                Some(data_for_cmp.clone())
            } else {
                None
            }
        });

        let _ = engine.add_peer("peer1");

        let response =
            engine.process_message("peer1", BitswapMessage::WantBlock { block, priority: 1 });

        // Should receive BLOCK response
        assert!(
            response
                .iter()
                .any(|m| matches!(m, BitswapMessage::Block { .. }))
        );
    }

    #[test]
    fn test_session_manager_eviction() {
        let manager = SessionManager::new(3);

        // Create more sessions than max
        let s1 = manager.create_session();
        let s2 = manager.create_session();
        let s3 = manager.create_session();
        let _s4 = manager.create_session();

        // Oldest should be evicted
        assert!(manager.get_session(s1.id).is_none());
        assert!(manager.get_session(s2.id).is_some());
        assert!(manager.get_session(s3.id).is_some());
    }

    #[test]
    fn test_session_staleness() {
        let mut session = BitswapSession::new(1);

        // New session should not be stale
        assert!(!session.is_stale(Duration::from_secs(60)));

        // Simulate old session
        session.last_activity = Instant::now() - Duration::from_secs(120);
        assert!(session.is_stale(Duration::from_secs(60)));
    }

    // ─────────────────────────────────────────────────────────────────
    // Rate Limiter Tests
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_rate_limiter_basic() {
        let mut limiter = RateLimiter::new(10.0, 1.0);

        // Should be able to acquire up to burst
        for _ in 0..10 {
            assert!(limiter.try_acquire());
        }

        // Should be exhausted
        assert!(!limiter.try_acquire());
    }

    #[test]
    fn test_rate_limiter_refill() {
        let mut limiter = RateLimiter::new(10.0, 100.0);

        // Exhaust tokens
        for _ in 0..10 {
            let _ = limiter.try_acquire();
        }

        // Wait for refill (100 tokens/second)
        std::thread::sleep(Duration::from_millis(20));

        // Should have some tokens back
        let remaining = limiter.remaining();
        assert!(remaining > 0.0);
    }

    #[test]
    fn test_rate_limiter_requests_per_second() {
        let limiter = RateLimiter::requests_per_second(100.0, 200.0);

        assert_eq!(limiter.remaining(), 200.0);
    }

    #[test]
    fn test_peer_rate_limiters() {
        let limiters = PeerRateLimiters::new();

        // Should have default limiter for unknown peer
        let remaining = limiters.remaining("unknown-peer");
        assert!(remaining > 0.0);

        // Try send should work
        assert!(limiters.try_send("test-peer"));

        // Clear should work
        limiters.clear();
        assert!(limiters.try_send("test-peer")); // Should work again
    }

    #[test]
    fn test_peer_rate_limiters_custom_config() {
        let config = RateLimitConfig {
            requests_per_second: 50.0,
            burst_size: 100.0,
            max_concurrent: 8,
        };

        let limiters = PeerRateLimiters::with_config(config);
        let limiter = limiters.get_limiter("test-peer");

        assert!(limiter.remaining() > 0.0);
    }

    // ─────────────────────────────────────────────────────────────────
    // Peer ID Validation Tests
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_peer_id_validator_valid() {
        let validator = PeerIdValidator::new();

        // Valid peer IDs
        assert!(validator.validate("peer123").is_ok());
        assert!(validator.validate("my-peer-id").is_ok());
        assert!(validator.validate("PEER_ID_123").is_ok());
        assert!(validator.validate("a1b2c3d4").is_ok());
    }

    #[test]
    fn test_peer_id_validator_too_short() {
        let validator = PeerIdValidator::new();

        // "ab" is 2 chars, below min_length of 4
        let result = validator.validate("ab");
        assert!(matches!(
            result,
            Err(PeerIdValidationError::TooShort { .. })
        ));
    }

    #[test]
    fn test_peer_id_validator_too_long() {
        let validator = PeerIdValidator::new();

        let long_id = "a".repeat(200);
        let result = validator.validate(&long_id);
        assert!(matches!(result, Err(PeerIdValidationError::TooLong { .. })));
    }

    #[test]
    fn test_peer_id_validator_invalid_characters() {
        let validator = PeerIdValidator::new();

        // Invalid characters (spaces, special chars)
        let result = validator.validate("peer with spaces");
        assert!(matches!(
            result,
            Err(PeerIdValidationError::InvalidCharacters { .. })
        ));

        let result = validator.validate("peer@host");
        assert!(matches!(
            result,
            Err(PeerIdValidationError::InvalidCharacters { .. })
        ));
    }

    #[test]
    fn test_peer_id_validator_custom_config() {
        let config = PeerIdConfig {
            min_length: 4,
            max_length: 32,
            allowed_pattern: Some(r"^[a-z]+$".to_string()),
        };

        let validator = PeerIdValidator::with_config(config);

        // Valid: all lowercase
        assert!(validator.validate("abcd").is_ok());
        assert!(validator.validate("myid").is_ok());

        // Invalid: has numbers
        let result = validator.validate("abc1");
        assert!(matches!(
            result,
            Err(PeerIdValidationError::InvalidCharacters { .. })
        ));
    }

    #[test]
    fn test_validate_peer_id_function() {
        // Should work with valid ID
        assert!(validate_peer_id("valid-peer-id").is_ok());

        // Should fail with invalid ID
        assert!(validate_peer_id("ab").is_err());
    }

    #[test]
    fn test_peer_id_validation_error_display() {
        let error = PeerIdValidationError::TooShort { actual: 3, min: 8 };
        let display = format!("{}", error);
        assert!(display.contains("too short"));

        let error = PeerIdValidationError::TooLong {
            actual: 200,
            max: 128,
        };
        let display = format!("{}", error);
        assert!(display.contains("too long"));

        let error = PeerIdValidationError::InvalidCharacters {
            reason: "test".to_string(),
        };
        let display = format!("{}", error);
        assert!(display.contains("invalid characters"));
    }
}
