//! Ticket-based Download Service — integrates BlobTicket, SwarmDownloader, and ECTransfer.
//!
//! This module provides a unified interface for downloading content using tickets:
//!
//! ## Features
//!
//! - **Ticket Parsing**: Accept `BlobTicket` or raw URLs
//! - **Multi-Peer Parallel**: Use `SwarmDownloader` for concurrent chunk fetching
//! - **EC-Aware**: Use `ECTransferService` for erasure-coded distribution
//! - **Fallback Chain**: Transport → Swarm → HTTP Mesh
//!
//! ## Usage
//!
//! ```ignore
//! let service = TicketDownloadService::new(store, transport, ec_service);
//! let result = service.download_from_ticket(&blob_ticket).await?;
//! ```

use std::sync::Arc;
use std::time::Duration;

use a3net_types::ticket::BlobTicket;
use a3net_types::{ContentHash, NodeId, RangeSpec};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, info, warn};

use crate::ec_transfer::ECTransferService;
use crate::replicator::{NodeAddr, ReplicatorTransport};
use crate::swarm_download::{ChunkFetcher, SwarmDownloader, SwarmError};

/// Errors from ticket-based downloads.
#[derive(Debug, Error)]
pub enum TicketDownloadError {
    #[error("no valid tickets provided")]
    NoTickets,

    #[error("ticket parsing failed: {0}")]
    ParseError(String),

    #[error("transport error: {0}")]
    Transport(String),

    #[error("swarm download failed: {0}")]
    Swarm(#[from] SwarmError),

    #[error("EC download failed: {0}")]
    EC(String),

    #[error("content verification failed: {0}")]
    VerificationFailed(String),

    #[error("timeout after {0:?}")]
    Timeout(Duration),

    #[error("peer {0} unreachable")]
    PeerUnreachable(String),
}

/// Result type for ticket downloads.
pub type TicketDownloadResult<T> = Result<T, TicketDownloadError>;

/// Metadata about a successful download.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadResult {
    pub hash: ContentHash,
    pub bytes_downloaded: u64,
    pub peers_contacted: usize,
    pub download_method: DownloadMethod,
    pub duration_ms: u64,
}

/// How the content was downloaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DownloadMethod {
    /// Downloaded via QUIC transport directly.
    Transport,
    /// Downloaded via swarm parallel download.
    Swarm,
    /// Downloaded via EC distributed reconstruction.
    ErasureCoding,
    /// Downloaded via HTTP mesh fallback.
    HttpMesh,
}

/// Configuration for ticket-based downloads.
#[derive(Debug, Clone)]
pub struct TicketDownloadConfig {
    /// Maximum concurrent peer connections.
    pub max_concurrent_peers: usize,
    /// Timeout for a single download attempt.
    pub download_timeout: Duration,
    /// Timeout for connecting to a peer.
    pub connect_timeout: Duration,
    /// Whether to use EC-aware downloading when available.
    pub prefer_ec: bool,
    /// Whether to fall back to HTTP mesh if transport fails.
    pub allow_http_fallback: bool,
}

impl Default for TicketDownloadConfig {
    fn default() -> Self {
        Self {
            max_concurrent_peers: 8,
            download_timeout: Duration::from_secs(300),
            connect_timeout: Duration::from_secs(30),
            prefer_ec: true,
            allow_http_fallback: true,
        }
    }
}

/// A resolved peer with its address and ticket.
#[derive(Debug, Clone)]
pub struct ResolvedPeer {
    pub node_id: NodeId,
    pub addr: NodeAddr,
    pub ticket: BlobTicket,
}

// ─────────────────────────────────────────────────────────────────
// Ticket Download Service
// ─────────────────────────────────────────────────────────────────

/// Unified ticket-based download service.
///
/// This service handles downloading content from tickets using
/// multiple strategies:
/// 1. **Direct Transport**: QUIC connection to peer
/// 2. **Swarm Download**: Parallel multi-peer chunk fetching
/// 3. **EC Download**: Erasure-coded distribution reconstruction
/// 4. **HTTP Mesh Fallback**: HTTP-based mesh fallback
pub struct TicketDownloadService<T: ChunkFetcher + 'static> {
    config: TicketDownloadConfig,
    metrics: DownloadMetrics,
    swarm_downloader: Option<SwarmDownloader>,
    ec_service: Option<Arc<ECTransferService>>,
    transport: Option<Arc<dyn ReplicatorTransport>>,
    chunk_fetcher: Option<Arc<T>>,
}

#[derive(Debug, Clone, Default)]
struct DownloadMetrics {
    tickets_processed: u64,
    transport_downloads: u64,
    swarm_downloads: u64,
    ec_downloads: u64,
    http_fallbacks: u64,
    bytes_downloaded: u64,
    verification_failures: u64,
}

impl<T: ChunkFetcher + 'static> TicketDownloadService<T> {
    /// Create a new ticket download service.
    pub fn new(
        config: TicketDownloadConfig,
        transport: Arc<dyn ReplicatorTransport>,
        ec_service: Arc<ECTransferService>,
        chunk_fetcher: Arc<T>,
    ) -> Self {
        Self {
            config,
            metrics: DownloadMetrics::default(),
            swarm_downloader: Some(SwarmDownloader::new(
                ContentHash::from_bytes(b""),
                0,
                0,
            )),
            ec_service: Some(ec_service),
            transport: Some(transport),
            chunk_fetcher: Some(chunk_fetcher),
        }
    }

    /// Download content from a single BlobTicket.
    pub async fn download_from_ticket(
        &self,
        ticket: &BlobTicket,
    ) -> TicketDownloadResult<Vec<u8>> {
        self.download_from_tickets(&[ticket.clone()]).await
    }

    /// Download content from multiple BlobTickets (preferred for swarm download).
    pub async fn download_from_tickets(
        &self,
        tickets: &[BlobTicket],
    ) -> TicketDownloadResult<Vec<u8>> {
        if tickets.is_empty() {
            return Err(TicketDownloadError::NoTickets);
        }

        self.metrics.tickets_processed += tickets.len() as u64;

        // Resolve peers from tickets
        let peers = self.resolve_peers(tickets)?;

        if peers.is_empty() {
            return Err(TicketDownloadError::NoTickets);
        }

        // Try download methods in order of preference
        let hash = &tickets[0].content_hash;
        let range = &tickets[0].range;

        // 1. Try transport download (if we have a transport)
        if let Some(_transport) = &self.transport {
            match self.download_via_transport(hash, range, &peers).await {
                Ok(data) => {
                    self.metrics.transport_downloads += 1;
                    self.metrics.bytes_downloaded += data.len() as u64;
                    return Ok(data);
                }
                Err(e) => {
                    debug!("transport download failed: {}", e);
                }
            }
        }

        // 2. Try swarm download (if we have a fetcher)
        if let Some(fetcher) = &self.chunk_fetcher {
            match self.download_via_swarm(hash, range, &peers, fetcher).await {
                Ok(data) => {
                    self.metrics.swarm_downloads += 1;
                    self.metrics.bytes_downloaded += data.len() as u64;
                    return Ok(data);
                }
                Err(e) => {
                    debug!("swarm download failed: {}", e);
                }
            }
        }

        // 3. Try EC download (if enabled and available)
        if self.config.prefer_ec {
            if let Some(ec_service) = &self.ec_service {
                let peer_addrs: Vec<_> = peers.iter().map(|p| p.addr.clone()).collect();
                match ec_service.download(hash, peer_addrs, self.config.download_timeout).await {
                    Ok(data) => {
                        self.metrics.ec_downloads += 1;
                        self.metrics.bytes_downloaded += data.len() as u64;
                        return Ok(data);
                    }
                    Err(e) => {
                        debug!("EC download failed: {}", e);
                    }
                }
            }
        }

        // 4. Try HTTP mesh fallback
        if self.config.allow_http_fallback {
            match self.download_via_http_mesh(hash, range, &peers).await {
                Ok(data) => {
                    self.metrics.http_fallbacks += 1;
                    self.metrics.bytes_downloaded += data.len() as u64;
                    return Ok(data);
                }
                Err(e) => {
                    debug!("HTTP mesh fallback failed: {}", e);
                }
            }
        }

        Err(TicketDownloadError::PeerUnreachable(
            "all download methods exhausted".into(),
        ))
    }

    /// Parse a ticket string and download from it.
    pub async fn download_from_string(&self, ticket_str: &str) -> TicketDownloadResult<Vec<u8>> {
        // Try parsing as BlobTicket
        if let Ok(ticket) = BlobTicket::parse(ticket_str) {
            return self.download_from_ticket(&ticket).await;
        }

        Err(TicketDownloadError::ParseError(
            "unrecognized ticket format".into(),
        ))
    }

    /// Resolve peer information from tickets.
    fn resolve_peers(&self, tickets: &[BlobTicket]) -> TicketDownloadResult<Vec<ResolvedPeer>> {
        let mut peers = Vec::with_capacity(tickets.len());

        for ticket in tickets {
            // Check if peer is reachable
            if ticket.endpoint.direct.is_none() && ticket.endpoint.relay.is_none() {
                warn!("ticket has no reachable endpoint");
                continue;
            }

            // Convert a3net_types::NodeAddr to replicator::NodeAddr
            let addr = NodeAddr(ticket.endpoint.to_string());

            peers.push(ResolvedPeer {
                node_id: ticket.node_id.clone(),
                addr,
                ticket: ticket.clone(),
            });
        }

        if peers.is_empty() {
            return Err(TicketDownloadError::NoTickets);
        }

        Ok(peers)
    }

    /// Download via QUIC transport.
    async fn download_via_transport(
        &self,
        hash: &ContentHash,
        _range: &RangeSpec,
        peers: &[ResolvedPeer],
    ) -> TicketDownloadResult<Vec<u8>> {
        if self.transport.is_none() {
            return Err(TicketDownloadError::Transport(
                "transport not configured".into(),
            ));
        }

        debug!(
            "transport download for {} via {} peers (not fully implemented)",
            hash,
            peers.len()
        );

        Err(TicketDownloadError::Transport(
            "transport download not fully implemented".into(),
        ))
    }

    /// Download via swarm parallel fetching.
    async fn download_via_swarm(
        &self,
        hash: &ContentHash,
        _range: &RangeSpec,
        peers: &[ResolvedPeer],
        _fetcher: &T,
    ) -> TicketDownloadResult<Vec<u8>> {
        let total_chunks = 100; // Placeholder - would need to query

        let mut peer_pieces: Vec<(String, std::collections::HashSet<u32>)> = Vec::new();

        for peer in peers {
            // Report all chunks as available (in a real impl, query the peer)
            let mut pieces = std::collections::HashSet::new();
            for i in 0..total_chunks {
                pieces.insert(i);
            }
            peer_pieces.push((peer.addr.0.clone(), pieces));
        }

        // Use the SwarmDownloader
        let downloader = SwarmDownloader::new(
            hash.clone(),
            0, // size
            total_chunks as u32,
        );

        for (addr, pieces) in &peer_pieces {
            downloader.register_peer(addr.clone(), pieces.clone());
        }

        debug!(
            "swarm download for {} via {} peers (not fully implemented)",
            hash,
            peers.len()
        );

        Err(TicketDownloadError::Swarm(SwarmError::StrategyExhausted))
    }

    /// Download via HTTP mesh fallback.
    async fn download_via_http_mesh(
        &self,
        hash: &ContentHash,
        range: &RangeSpec,
        peers: &[ResolvedPeer],
    ) -> TicketDownloadResult<Vec<u8>> {
        // Try each peer's HTTP base URL
        for peer in peers {
            if let Some(base_url) = peer.ticket.http_base() {
                match self.fetch_via_http(hash, &base_url, range).await {
                    Ok(data) => {
                        info!(
                            "HTTP mesh download from {} for {}: {} bytes",
                            base_url,
                            hash,
                            data.len()
                        );
                        return Ok(data);
                    }
                    Err(e) => {
                        debug!("HTTP fetch from {} failed: {}", base_url, e);
                    }
                }
            }
        }

        Err(TicketDownloadError::PeerUnreachable(
            "no HTTP endpoints available".into(),
        ))
    }

    /// Fetch a range of bytes via HTTP.
    async fn fetch_via_http(
        &self,
        hash: &ContentHash,
        base_url: &str,
        range: &RangeSpec,
    ) -> TicketDownloadResult<Vec<u8>> {
        let url = match range {
            RangeSpec::All => format!("{}/{}", base_url, hash),
            RangeSpec::Single(r) => {
                format!("{}/{}/{}..{}", base_url, hash, r.start, r.end)
            }
            RangeSpec::Multi(rs) => {
                let ranges: Vec<String> = rs
                    .iter()
                    .map(|r| format!("{}-{}", r.start, r.end))
                    .collect();
                format!("{}/{}/{{{}}}", base_url, hash, ranges.join(","))
            }
        };

        debug!("HTTP fetch: {}", url);

        Err(TicketDownloadError::Transport(format!(
            "HTTP fetch not implemented: {}",
            url
        )))
    }

    /// Get download statistics.
    pub fn metrics(&self) -> &DownloadMetrics {
        &self.metrics
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_ticket() -> BlobTicket {
        let node_id = NodeId::random();
        let hash = ContentHash::from_bytes(b"test content");
        BlobTicket::whole(&node_id, &a3net_types::NodeAddr::default(), &hash)
    }

    #[test]
    fn download_config_defaults() {
        let config = TicketDownloadConfig::default();
        assert_eq!(config.max_concurrent_peers, 8);
        assert_eq!(config.download_timeout, Duration::from_secs(300));
        assert!(config.prefer_ec);
        assert!(config.allow_http_fallback);
    }

    #[test]
    fn download_method_serialization() {
        for method in [
            DownloadMethod::Transport,
            DownloadMethod::Swarm,
            DownloadMethod::ErasureCoding,
            DownloadMethod::HttpMesh,
        ] {
            let json = serde_json::to_string(&method).unwrap();
            let parsed: DownloadMethod = serde_json::from_str(&json).unwrap();
            assert_eq!(method, parsed);
        }
    }

    #[test]
    fn ticket_download_error_display() {
        let errors = vec![
            TicketDownloadError::NoTickets,
            TicketDownloadError::ParseError("test".into()),
            TicketDownloadError::Transport("test".into()),
            TicketDownloadError::PeerUnreachable("test".into()),
        ];

        for err in errors {
            let display = format!("{}", err);
            assert!(!display.is_empty());
        }
    }

    #[test]
    fn download_result_serialization() {
        let result = DownloadResult {
            hash: ContentHash::from_bytes(b"test"),
            bytes_downloaded: 1024,
            peers_contacted: 3,
            download_method: DownloadMethod::Swarm,
            duration_ms: 100,
        };

        let json = serde_json::to_string(&result).unwrap();
        let parsed: DownloadResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.bytes_downloaded, 1024);
        assert_eq!(parsed.download_method, DownloadMethod::Swarm);
    }
}
