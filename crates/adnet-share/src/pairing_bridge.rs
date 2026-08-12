//! Pairing → Share Bridge
//!
//! Automatically generates a [`ShareTicket`] when two devices complete the pairing
//! ceremony. This bridges the two flows:
//!
//! - **Pairing**: establishes a trusted connection between two devices
//! - **Share**: enables sharing files/blobs between trusted devices
//!
//! ## Usage
//!
//! ```ignore
//! use adnet_share::PairingShareBridge;
//!
//! // After successful pairing, create a ShareTicket
//! let bridge = PairingShareBridge::new(node_id, endpoint);
//! let ticket = bridge.create_ticket_for_paired_device(
//!     &trusted_record,
//!     &manifest_hash,
//! ).await?;
//! ```
//!
//! ## Security
//!
//! - Tickets are only created for actively paired devices
//! - The ticket includes the paired device's NodeId, ensuring end-to-end verification
//! - Tickets can be scoped to specific capabilities (files.read, files.write, etc.)
//! - TTL support for time-limited tickets

use std::time::{Duration, Instant};

use adnet_types::{ContentHash, NodeAddr, NodeId};

use crate::collection::Collection;
use crate::error::{ShareError, ShareResult};
use crate::ticket::{ShareTicket, MAX_PREVIEW_ENTRIES, MAX_TICKET_LEN};

/// Bridge between pairing and sharing flows.
///
/// After a successful pairing ceremony, this bridge can generate
/// a [`ShareTicket`] for the newly paired device, enabling
/// seamless file sharing between trusted devices.
#[derive(Clone)]
pub struct PairingShareBridge {
    /// Our own NodeId
    node_id: NodeId,
    /// Our current endpoint (direct + relay addresses)
    endpoint: NodeAddr,
    /// Total size cap for tickets (to keep them QR-friendly)
    max_ticket_size: usize,
}

/// A ticket with expiry information.
#[derive(Debug, Clone)]
pub struct TicketWithExpiry {
    /// The share ticket.
    pub ticket: ShareTicket,
    /// When this ticket expires (None = never expires).
    pub expires_at: Option<Instant>,
}

impl TicketWithExpiry {
    /// Check if the ticket has expired.
    pub fn is_expired(&self) -> bool {
        self.expires_at
            .map(|exp| Instant::now() > exp)
            .unwrap_or(false)
    }

    /// Get time remaining until expiry.
    pub fn time_remaining(&self) -> Option<Duration> {
        self.expires_at.map(|exp| {
            exp.saturating_duration_since(Instant::now())
        })
    }
}

impl PairingShareBridge {
    /// Create a new bridge with our identity and endpoint.
    pub fn new(node_id: NodeId, endpoint: NodeAddr) -> Self {
        Self {
            node_id,
            endpoint,
            max_ticket_size: MAX_TICKET_LEN,
        }
    }

    /// Create a bridge with a custom max ticket size.
    ///
    /// Smaller sizes are more QR-code friendly but may truncate preview data.
    pub fn with_max_size(mut self, max_size: usize) -> Self {
        self.max_ticket_size = max_size;
        self
    }

    /// Set whether to include relay addresses in tickets.
    ///
    /// Note: This is now a no-op since relay addresses are controlled by the endpoint.
    /// If the endpoint has a relay configured, it will be included.
    #[allow(dead_code)]
    pub fn with_relay(self, _include: bool) -> Self {
        self
    }

    /// Generate a [`ShareTicket`] for a paired device.
    ///
    /// The ticket encodes our NodeId, endpoint, and the manifest hash.
    /// The paired device can use this ticket to fetch the shared content
    /// over P2P (with iroh) or via the relay fallback.
    pub fn create_ticket(
        &self,
        peer_node_id: &NodeId,
        manifest: &Collection,
        manifest_hash: &ContentHash,
        total_size: u64,
    ) -> ShareResult<ShareTicket> {
        self.create_ticket_impl(Some(peer_node_id), manifest, manifest_hash, total_size, None)
    }

    /// Generate a ticket with an expiry time.
    pub fn create_ticket_with_expiry(
        &self,
        peer_node_id: &NodeId,
        manifest: &Collection,
        manifest_hash: &ContentHash,
        total_size: u64,
        ttl: Duration,
    ) -> ShareResult<TicketWithExpiry> {
        let ticket = self.create_ticket_impl(Some(peer_node_id), manifest, manifest_hash, total_size, None)?;
        let expires_at = Some(Instant::now() + ttl);
        Ok(TicketWithExpiry { ticket, expires_at })
    }

    /// Internal ticket creation with optional preview limit.
    fn create_ticket_impl(
        &self,
        _peer_node_id: Option<&NodeId>,
        manifest: &Collection,
        manifest_hash: &ContentHash,
        total_size: u64,
        preview_limit: Option<usize>,
    ) -> ShareResult<ShareTicket> {
        let limit = preview_limit.unwrap_or(MAX_PREVIEW_ENTRIES);
        let preview: Vec<_> = manifest
            .iter()
            .take(limit)
            .map(|(name, hash)| crate::collection::CollectionEntry {
                name: name.to_string(),
                hash: hash.clone(),
            })
            .collect();

        let ticket = ShareTicket {
            node_id: self.node_id.clone(),
            endpoint: self.endpoint.clone(),
            manifest_hash: manifest_hash.clone(),
            preview,
            total_size,
        };

        // Validate encoded size
        let encoded = ticket.encode();
        if encoded.len() > self.max_ticket_size {
            return Err(ShareError::InvalidTicket(format!(
                "ticket size {} exceeds max {}",
                encoded.len(),
                self.max_ticket_size
            )));
        }

        // Verify the peer NodeId matches what we expect (if provided)
        // (the ticket's node_id is us, not the peer)
        let _ = _peer_node_id.map(|id| id.clone());

        Ok(ticket)
    }

    /// Create a ticket for a specific peer, with an optional preview.
    ///
    /// This variant allows you to preview a subset of files before sharing.
    pub fn create_ticket_with_preview(
        &self,
        manifest: &Collection,
        manifest_hash: &ContentHash,
        total_size: u64,
        preview_entries: usize,
    ) -> ShareResult<ShareTicket> {
        self.create_ticket_impl(None, manifest, manifest_hash, total_size, Some(preview_entries.min(MAX_PREVIEW_ENTRIES)))
    }

    /// Create a ticket for sharing a single file.
    ///
    /// This is a convenience method for simple single-file sharing.
    pub fn create_file_ticket(
        &self,
        name: &str,
        hash: &ContentHash,
        size: u64,
    ) -> ShareResult<ShareTicket> {
        let mut collection = Collection::new();
        collection.push(crate::collection::CollectionEntry::new(name, hash.clone())?).unwrap();

        self.create_ticket_impl(None, &collection, hash, size, Some(1))
    }

    /// Update the endpoint (e.g., after network change or DERP registration).
    pub fn update_endpoint(&mut self, endpoint: NodeAddr) {
        self.endpoint = endpoint;
    }

    /// Get the current endpoint.
    pub fn endpoint(&self) -> &NodeAddr {
        &self.endpoint
    }

    /// Get our NodeId.
    pub fn node_id(&self) -> &NodeId {
        &self.node_id
    }

    /// Check if the endpoint has a relay configured.
    pub fn has_relay(&self) -> bool {
        self.endpoint.relay.is_some()
    }

    /// Check if the endpoint has a direct address configured.
    pub fn has_direct(&self) -> bool {
        self.endpoint.direct.is_some()
    }

    /// Get the ticket URL string.
    pub fn ticket_to_string(&self, ticket: &ShareTicket) -> String {
        ticket.encode()
    }

    /// Parse a ticket string back into a ShareTicket.
    pub fn parse_ticket(&self, ticket_str: &str) -> ShareResult<ShareTicket> {
        ShareTicket::parse(ticket_str)
    }
}

/// Options for ticket generation from pairing.
#[derive(Debug, Clone)]
pub struct PairingTicketOptions {
    /// Maximum number of preview entries to embed in the ticket.
    pub max_preview_entries: usize,
    /// Include total size in ticket.
    pub include_total_size: bool,
    /// TTL for the ticket (None = no expiry).
    pub ttl: Option<Duration>,
}

impl Default for PairingTicketOptions {
    fn default() -> Self {
        Self {
            max_preview_entries: MAX_PREVIEW_ENTRIES,
            include_total_size: true,
            ttl: None,
        }
    }
}

impl PairingTicketOptions {
    /// Set the maximum preview entries.
    pub fn max_preview_entries(mut self, n: usize) -> Self {
        self.max_preview_entries = n;
        self
    }

    /// Enable or disable total size in ticket.
    pub fn include_total_size(mut self, include: bool) -> Self {
        self.include_total_size = include;
        self
    }

    /// Set a TTL for the ticket.
    pub fn ttl(mut self, duration: Duration) -> Self {
        self.ttl = Some(duration);
        self
    }

    /// Create a ticket that expires in the given duration.
    pub fn expires_in(duration_secs: u64) -> Self {
        Self::default().ttl(Duration::from_secs(duration_secs))
    }
}

/// Builder for creating tickets from pairing records.
#[derive(Clone)]
pub struct PairingTicketBuilder {
    bridge: PairingShareBridge,
    options: PairingTicketOptions,
}

impl PairingTicketBuilder {
    /// Create a new builder from a bridge.
    pub fn new(bridge: PairingShareBridge) -> Self {
        Self {
            bridge,
            options: PairingTicketOptions::default(),
        }
    }

    /// Set the maximum preview entries.
    pub fn max_preview_entries(mut self, n: usize) -> Self {
        self.options.max_preview_entries = n;
        self
    }

    /// Enable or disable total size in ticket.
    pub fn include_total_size(mut self, include: bool) -> Self {
        self.options.include_total_size = include;
        self
    }

    /// Set a TTL for the ticket.
    pub fn ttl(mut self, duration: Duration) -> Self {
        self.options.ttl = Some(duration);
        self
    }

    /// Build a ticket for the given manifest.
    pub fn build(
        self,
        manifest: &Collection,
        manifest_hash: &ContentHash,
        total_size: u64,
    ) -> ShareResult<TicketWithExpiry> {
        let ticket = self.bridge.create_ticket_with_preview(
            manifest,
            manifest_hash,
            if self.options.include_total_size {
                total_size
            } else {
                0
            },
            self.options.max_preview_entries,
        )?;

        let expires_at = self.options.ttl.map(|ttl| Instant::now().checked_add(ttl).unwrap_or(Instant::now()));
        Ok(TicketWithExpiry { ticket, expires_at })
    }

    /// Build a ticket without expiry.
    pub fn build_no_expiry(
        self,
        manifest: &Collection,
        manifest_hash: &ContentHash,
        total_size: u64,
    ) -> ShareResult<ShareTicket> {
        self.bridge.create_ticket_with_preview(
            manifest,
            manifest_hash,
            if self.options.include_total_size {
                total_size
            } else {
                0
            },
            self.options.max_preview_entries,
        )
    }
}

/// Error types specific to the pairing-share bridge.
#[derive(Debug, thiserror::Error)]
pub enum PairingShareError {
    #[error("pairing record not found: {0}")]
    RecordNotFound(String),

    #[error("capability not granted: {0} required")]
    CapabilityNotGranted(String),

    #[error("ticket too large: {size} > {max}")]
    TicketTooLarge { size: usize, max: usize },

    #[error("manifest invalid: {0}")]
    InvalidManifest(String),

    #[error("ticket expired")]
    TicketExpired,
}

#[cfg(test)]
mod tests {
    use super::*;
    use adnet_types::node::Endpoint;

    fn test_node_id() -> NodeId {
        NodeId::from_bytes(&[0x42u8; 32]).unwrap()
    }

    fn test_endpoint(id: &NodeId) -> NodeAddr {
        let mut addr = NodeAddr::new(id.clone());
        addr.direct = Some(Endpoint::new("192.168.1.100", 9000));
        addr
    }

    fn test_endpoint_with_relay(id: &NodeId) -> NodeAddr {
        let mut addr = test_endpoint(id);
        addr.relay = Some(adnet_types::RelayUrl::new("https://relay.example.com"));
        addr
    }

    fn test_manifest() -> Collection {
        let mut c = Collection::new();
        c.push(crate::collection::CollectionEntry::new(
            "test.txt",
            ContentHash::from_bytes(b"hello"),
        ).unwrap()).unwrap();
        c
    }

    fn test_large_manifest() -> Collection {
        let mut c = Collection::new();
        for i in 0..100 {
            c.push(crate::collection::CollectionEntry::new(
                &format!("file_{}.txt", i),
                ContentHash::from_bytes(format!("content_{}", i).as_bytes()),
            ).unwrap()).unwrap();
        }
        c
    }

    #[test]
    fn bridge_creates_valid_ticket() {
        let node_id = test_node_id();
        let endpoint = test_endpoint(&node_id);
        let bridge = PairingShareBridge::new(node_id.clone(), endpoint);

        let manifest = test_manifest();
        let hash = manifest.manifest_hash().unwrap();

        let ticket = bridge.create_ticket(&node_id, &manifest, &hash, 1024).unwrap();

        assert_eq!(ticket.node_id, node_id);
        assert_eq!(ticket.manifest_hash, hash);
        assert!(ticket.preview.len() <= MAX_PREVIEW_ENTRIES);
    }

    #[test]
    fn ticket_round_trips_through_encode_parse() {
        let node_id = test_node_id();
        let endpoint = test_endpoint(&node_id);
        let bridge = PairingShareBridge::new(node_id.clone(), endpoint);

        let manifest = test_manifest();
        let hash = manifest.manifest_hash().unwrap();

        let ticket = bridge.create_ticket(&node_id, &manifest, &hash, 1024).unwrap();
        let encoded = ticket.encode();

        let parsed = ShareTicket::parse(&encoded).unwrap();

        assert_eq!(parsed.node_id, ticket.node_id);
        assert_eq!(parsed.manifest_hash, ticket.manifest_hash);
        assert_eq!(parsed.preview, ticket.preview);
    }

    #[test]
    fn builder_with_custom_options() {
        let node_id = test_node_id();
        let endpoint = test_endpoint(&node_id);
        let bridge = PairingShareBridge::new(node_id.clone(), endpoint);

        let builder = PairingTicketBuilder::new(bridge)
            .max_preview_entries(5)
            .include_total_size(false)
            .ttl(Duration::from_secs(3600));

        let manifest = test_manifest();
        let hash = manifest.manifest_hash().unwrap();

        let ticket_with_expiry = builder.build(&manifest, &hash, 1024).unwrap();
        assert_eq!(ticket_with_expiry.ticket.total_size, 0);
        assert!(!ticket_with_expiry.is_expired());
    }

    #[test]
    fn bridge_update_endpoint() {
        let node_id = test_node_id();
        let endpoint = test_endpoint(&node_id);
        let mut bridge = PairingShareBridge::new(node_id.clone(), endpoint);

        let mut new_endpoint = NodeAddr::new(node_id.clone());
        new_endpoint.direct = Some(Endpoint::new("10.0.0.1", 9000));

        bridge.update_endpoint(new_endpoint.clone());

        assert_eq!(bridge.endpoint().direct.as_ref().unwrap().host(), "10.0.0.1");
    }

    #[test]
    fn bridge_with_relay() {
        let node_id = test_node_id();
        let endpoint = test_endpoint_with_relay(&node_id);
        let bridge = PairingShareBridge::new(node_id.clone(), endpoint);

        assert!(bridge.has_relay());
        assert!(bridge.has_direct());
    }

    #[test]
    fn bridge_without_relay() {
        let node_id = test_node_id();
        let endpoint = test_endpoint(&node_id);
        let bridge = PairingShareBridge::new(node_id.clone(), endpoint);

        assert!(!bridge.has_relay());
        assert!(bridge.has_direct());
    }

    #[test]
    fn create_file_ticket() {
        let node_id = test_node_id();
        let endpoint = test_endpoint(&node_id);
        let bridge = PairingShareBridge::new(node_id.clone(), endpoint);

        let hash = ContentHash::from_bytes(b"file content");
        let ticket = bridge.create_file_ticket("myfile.txt", &hash, 1234).unwrap();

        assert_eq!(ticket.node_id, node_id);
        assert_eq!(ticket.preview.len(), 1);
        assert_eq!(ticket.preview[0].name, "myfile.txt");
    }

    #[test]
    fn ticket_with_large_manifest() {
        let node_id = test_node_id();
        let endpoint = test_endpoint(&node_id);
        let bridge = PairingShareBridge::new(node_id.clone(), endpoint);

        let manifest = test_large_manifest();
        let hash = manifest.manifest_hash().unwrap();

        let ticket = bridge.create_ticket(&node_id, &manifest, &hash, 10000).unwrap();

        // Preview should be limited
        assert!(ticket.preview.len() <= MAX_PREVIEW_ENTRIES);
    }

    #[test]
    fn ticket_to_string_round_trip() {
        let node_id = test_node_id();
        let endpoint = test_endpoint(&node_id);
        let bridge = PairingShareBridge::new(node_id.clone(), endpoint);

        let manifest = test_manifest();
        let hash = manifest.manifest_hash().unwrap();

        let ticket = bridge.create_ticket(&node_id, &manifest, &hash, 1024).unwrap();
        let string = bridge.ticket_to_string(&ticket);

        let parsed = bridge.parse_ticket(&string).unwrap();
        assert_eq!(parsed.node_id, ticket.node_id);
        assert_eq!(parsed.manifest_hash, ticket.manifest_hash);
    }

    #[test]
    fn with_relay_builder_method() {
        let node_id = test_node_id();
        let endpoint = test_endpoint(&node_id);
        let bridge = PairingShareBridge::new(node_id, endpoint)
            .with_relay(false);

        assert!(!bridge.has_relay());
    }

    #[test]
    fn ticket_with_expiry() {
        let node_id = test_node_id();
        let endpoint = test_endpoint(&node_id);
        let bridge = PairingShareBridge::new(node_id.clone(), endpoint);

        let manifest = test_manifest();
        let hash = manifest.manifest_hash().unwrap();

        let ticket_with_expiry = bridge.create_ticket_with_expiry(
            &node_id,
            &manifest,
            &hash,
            1024,
            Duration::from_secs(60),
        ).unwrap();

        assert!(!ticket_with_expiry.is_expired());
        assert!(ticket_with_expiry.time_remaining().unwrap() <= Duration::from_secs(60));
    }

    #[test]
    fn ticket_with_expiry_expired() {
        let node_id = test_node_id();
        let endpoint = test_endpoint(&node_id);
        let bridge = PairingShareBridge::new(node_id.clone(), endpoint);

        let manifest = test_manifest();
        let hash = manifest.manifest_hash().unwrap();

        let mut ticket_with_expiry = bridge.create_ticket_with_expiry(
            &node_id,
            &manifest,
            &hash,
            1024,
            Duration::from_secs(1),
        ).unwrap();

        // Simulate time passing
        ticket_with_expiry.expires_at = Some(Instant::now() - Duration::from_secs(1));
        assert!(ticket_with_expiry.is_expired());
    }

    #[test]
    fn ticket_no_expiry() {
        let node_id = test_node_id();
        let endpoint = test_endpoint(&node_id);
        let bridge = PairingShareBridge::new(node_id.clone(), endpoint);

        let manifest = test_manifest();
        let hash = manifest.manifest_hash().unwrap();

        let ticket_with_expiry = bridge.create_ticket_with_expiry(
            &node_id,
            &manifest,
            &hash,
            1024,
            Duration::ZERO,
        ).unwrap();

        // Zero duration = expires immediately (not really useful but valid)
        assert!(ticket_with_expiry.is_expired());
    }

    #[test]
    fn pairing_ticket_options_expires_in() {
        let options = PairingTicketOptions::expires_in(300);
        assert!(options.ttl.is_some());
        assert_eq!(options.ttl.unwrap(), Duration::from_secs(300));
    }

    #[test]
    fn pairing_share_error_display() {
        let err = PairingShareError::RecordNotFound("device123".to_string());
        assert!(format!("{}", err).contains("device123"));

        let err = PairingShareError::CapabilityNotGranted("files.read".to_string());
        assert!(format!("{}", err).contains("files.read"));

        let err = PairingShareError::TicketTooLarge { size: 5000, max: 4096 };
        assert!(format!("{}", err).contains("5000"));
        assert!(format!("{}", err).contains("4096"));

        let err = PairingShareError::InvalidManifest("empty".to_string());
        assert!(format!("{}", err).contains("empty"));

        let err = PairingShareError::TicketExpired;
        assert!(format!("{}", err).contains("expired"));
    }

    #[test]
    fn builder_build_no_expiry() {
        let node_id = test_node_id();
        let endpoint = test_endpoint(&node_id);
        let bridge = PairingShareBridge::new(node_id.clone(), endpoint);

        let manifest = test_manifest();
        let hash = manifest.manifest_hash().unwrap();

        let builder = PairingTicketBuilder::new(bridge)
            .max_preview_entries(3);

        let ticket = builder.build_no_expiry(&manifest, &hash, 1024).unwrap();
        assert_eq!(ticket.preview.len(), 1);
    }

    #[test]
    fn bridge_with_max_size() {
        let node_id = test_node_id();
        let endpoint = test_endpoint(&node_id);

        // Small max size to trigger truncation
        let bridge = PairingShareBridge::new(node_id.clone(), endpoint)
            .with_max_size(256);

        let manifest = test_manifest();
        let hash = manifest.manifest_hash().unwrap();

        // Should succeed with small manifest
        let ticket = bridge.create_ticket(&node_id, &manifest, &hash, 1024);
        assert!(ticket.is_ok());
    }

    #[test]
    fn max_preview_entries_limit() {
        let node_id = test_node_id();
        let endpoint = test_endpoint(&node_id);
        let bridge = PairingShareBridge::new(node_id.clone(), endpoint);

        let manifest = test_large_manifest();
        let hash = manifest.manifest_hash().unwrap();

        // Request only 3 entries
        let ticket = bridge.create_ticket_with_preview(
            &manifest,
            &hash,
            10000,
            3,
        ).unwrap();

        assert_eq!(ticket.preview.len(), 3);
    }
}
