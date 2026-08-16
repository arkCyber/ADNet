//! `PkarrRecord` — pkarr-compatible key-value record wire format.
//!
//! This module exposes a typed wrapper around the pkarr wire format that
//! A3Net publishes to and resolves from DNS / HTTP relays. The shape
//! mirrors [`iroh_pkarr::PkarrRecord`](https://docs.rs/iroh_pkarr) and
//! the `pkarr::SignedPacket` it embeds, so:
//!
//! - A3Net nodes can publish their own `[NodeAddr][crate::node::NodeAddr]`
//!   (carried as the underlying `EndpointInfo`'s TXT payload) and have it
//!   parsed verbatim by stock `iroh::address_lookup::PkarrResolver`
//!   consumers.
//! - A3Net's self-hosted DNS server can serve records at the same keys
//!   (`_iroh.<z32-encoded-pubkey>.<zone>` TXT) as the upstream
//!   `iroh-dns-server` does, so PKARR clients can switch transparently.
//!
//! ## Wire format
//!
//! ```text
//! <public-key>: 32 bytes  (ed25519; z32-encoded in DNS names)
//! <signature> : 64 bytes
//! <timestamp> : u64 big-endian (seconds since the unix epoch)
//! <sequence>  : varint (per-resource records inside the DNS packet)
//! <resources> : sequence of DNS resource records (TXT, A, AAAA, ...)
//! ```
//!
//! The whole thing is a CBOR / DNS-message envelope in pkarr proper, but
//! A3Net only cares about the (record, ttl_secs, zone) tuple exposed via
//! the HTTP relay and DNS-TXT layers. The bytes here are an opaque blob
//! representing a `pkarr::SignedPacket` payload as it would be fetched
//! from a relay at `GET /<z32-encoded-pubkey>`.
//!
//! ## Zones
//!
//! A pkarr record is bound to a *zone* (the DNS suffix the relay serves).
//! `iroh-dns-server` defaults to `dns.iroh.link`; A3Net's self-hosted
//! server defaults to whatever the operator configures (typically
//! `a3net.local` for tests, `a3net.example` for production). The
//! [`PkarrRecord::zone`] field captures this so a resolver can route
//! queries to the right upstream.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{AdnetError, Result};

/// Default pkarr zone used by `iroh-dns-server` in production
/// (`dns.iroh.link`). Kept as a constant so A3Net code that wants to
/// interop with stock iroh nodes can fall back to this zone when
/// discovering peers.
pub const IROH_PKARR_ZONE: &str = "dns.iroh.link";

/// The well-known suffix that separates the z-base-32 encoded public key
/// from the zone in a pkarr DNS-TXT name.
///
/// Stock iroh publishes under `_iroh.<z32>.<zone>`. A3Net's self-hosted
/// server uses the same suffix so existing pkarr resolvers pick the
/// records up unchanged.
pub const PKARR_NAME_PREFIX: &str = "_iroh";

/// Maximum ttl we'll accept for an inbound pkarr record. Pkarr's
/// `default_ttl` is 30 seconds; we cap at 1 hour to bound the cache
/// staleness on the relay side. Values larger than this are clamped
/// silently by [`PkarrRecord::with_ttl_secs`].
pub const MAX_TTL_SECONDS: u32 = 3_600;

/// Wire-format pkarr record.
///
/// This is the A3Net-side twin of `iroh_pkarr::PkarrRecord`. Callers
/// publish instances of this to a pkarr relay (HTTP PUT) and resolve
/// them back via a pkarr resolver (HTTP GET / DNS-TXT). The
/// [`packet`](Self::packet) field carries the opaque signed packet that
/// pkarr uses as its single-source-of-truth encoding; it is what stock
/// iroh clients speak, so the wire is fully interoperable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PkarrRecord {
    /// DNS / HTTP relay zone the record was published to (e.g.
    /// `dns.iroh.link` or `a3net.example`). Lower-case ASCII, no
    /// trailing dot.
    pub zone: String,
    /// Opaque signed packet bytes (pkarr `SignedPacket` wire format).
    ///
    /// We deliberately keep this as `Vec<u8>` rather than wrapping
    /// `pkarr::SignedPacket` directly — doing so would force every
    /// consumer of `a3net-types` (including FFI, the CLI, the share
    /// crate, and the storage backend) to pull `pkarr` as a hard
    /// dependency. The wire bytes are the only thing A3Net itself
    /// needs to forward; structural parsing happens at the
    /// `iroh::endpoint_info::EndpointInfo::from_pkarr_signed_packet`
    /// boundary.
    #[serde(with = "pkarr_packet_bytes")]
    pub packet: Vec<u8>,
    /// TTL of the record at the relay, in seconds. Clamped to
    /// [`MAX_TTL_SECONDS`] on construction.
    pub ttl_secs: u32,
}

impl PkarrRecord {
    /// Build a record with a default zone (`dns.iroh.link`) and the
    /// supplied signed packet bytes / TTL.
    pub fn new(packet: Vec<u8>, ttl_secs: u32) -> Self {
        Self {
            zone: IROH_PKARR_ZONE.to_string(),
            packet,
            ttl_secs: ttl_secs.min(MAX_TTL_SECONDS),
        }
    }

    /// Build a record bound to a specific operator-supplied zone.
    pub fn in_zone(zone: impl Into<String>, packet: Vec<u8>, ttl_secs: u32) -> Result<Self> {
        let zone = zone.into();
        validate_zone(&zone)?;
        Ok(Self {
            zone,
            packet,
            ttl_secs: ttl_secs.min(MAX_TTL_SECONDS),
        })
    }

    /// Override the zone. The supplied value is validated; see
    /// [`validate_zone`].
    pub fn with_zone(mut self, zone: impl Into<String>) -> Result<Self> {
        let zone = zone.into();
        validate_zone(&zone)?;
        self.zone = zone;
        Ok(self)
    }

    /// Override the TTL, clamped to [`MAX_TTL_SECONDS`].
    pub fn with_ttl_secs(mut self, ttl: u32) -> Self {
        self.ttl_secs = ttl.min(MAX_TTL_SECONDS);
        self
    }

    /// Replace the packet bytes.
    pub fn with_packet(mut self, packet: Vec<u8>) -> Self {
        self.packet = packet;
        self
    }

    /// True if the record carries no packet payload. Empty packets
    /// are not invalid per se (a publisher may explicitly retract a
    /// previous record by publishing a zero-length body), but
    /// downstream resolvers MUST treat them as "no record".
    pub fn is_empty(&self) -> bool {
        self.packet.is_empty()
    }

    /// Wire-format length of the underlying packet, in bytes.
    pub fn packet_len(&self) -> usize {
        self.packet.len()
    }

    /// The DNS-TXT name this record is served under for `zone`.
    /// Format: `<PKARR_NAME_PREFIX>.<z32-encoded-pubkey>.<zone>.`
    ///
    /// `z32_pubkey` is the z-base-32 encoded ed25519 public key of
    /// the publisher (32 bytes → 52 z32 chars). The trailing dot is
    /// required by DNS, and the `_iroh` prefix is what stock iroh
    /// uses to identify pkarr records on the wire.
    pub fn dns_txt_name(zone: &str, z32_pubkey: &str) -> String {
        let zone = zone.trim_end_matches('.');
        format!("{PKARR_NAME_PREFIX}.{z32_pubkey}.{zone}.")
    }

    /// The HTTP path this record is served under on a pkarr relay
    /// (e.g. `GET https://pkarr.example/<z32>`). The trailing slash
    /// is omitted — pkarr relays serve the bare z32 key.
    pub fn http_path(z32_pubkey: &str) -> String {
        format!("/{z32_pubkey}")
    }
}

impl fmt::Display for PkarrRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pkarr(zone={}, ttl={}s, packet_bytes={})",
            self.zone,
            self.ttl_secs,
            self.packet.len()
        )
    }
}

/// Validate that a zone name is acceptable for pkarr DNS publishing.
///
/// Rules (loose, deliberately — DNS itself will reject malformed
/// names at the wire layer):
/// - non-empty
/// - no leading or trailing dot
/// - no whitespace
/// - ASCII-only (so we can safely embed in DNS labels without
///   punycode round-trips)
/// - at least one dot (so a zone like `local` is rejected; pkarr
///   expects an FQDN-style suffix)
pub fn validate_zone(zone: &str) -> Result<()> {
    if zone.is_empty() {
        return Err(AdnetError::Validation("pkarr zone: empty".into()));
    }
    if zone.starts_with('.') || zone.ends_with('.') {
        return Err(AdnetError::Validation(format!(
            "pkarr zone: leading/trailing dot in {zone:?}"
        )));
    }
    if zone.chars().any(|c| c.is_whitespace()) {
        return Err(AdnetError::Validation(format!(
            "pkarr zone: whitespace in {zone:?}"
        )));
    }
    if !zone.is_ascii() {
        return Err(AdnetError::Validation(format!(
            "pkarr zone: non-ASCII characters in {zone:?}"
        )));
    }
    if !zone.contains('.') {
        return Err(AdnetError::Validation(format!(
            "pkarr zone: expected FQDN-style suffix, got {zone:?}"
        )));
    }
    Ok(())
}

mod pkarr_packet_bytes {
    //! Serde adapter for the packet bytes. `Vec<u8>` serializes
    //! differently across formats (base64 for JSON, byte array for
    //! bincode), so we explicitly go through a serde sequence to keep
    //! the encoding stable: callers that round-trip a PkarrRecord via
    //! JSON get the same bytes back regardless of the underlying
    //! serializer.
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        bytes.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        Vec::<u8>::deserialize(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_packet() -> Vec<u8> {
        // Not a valid pkarr packet — just opaque bytes for tests.
        vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]
    }

    #[test]
    fn new_uses_default_zone() {
        let rec = PkarrRecord::new(sample_packet(), 30);
        assert_eq!(rec.zone, IROH_PKARR_ZONE);
        assert_eq!(rec.ttl_secs, 30);
        assert_eq!(rec.packet, sample_packet());
    }

    #[test]
    fn new_clamps_oversized_ttl() {
        let rec = PkarrRecord::new(sample_packet(), MAX_TTL_SECONDS + 1);
        assert_eq!(rec.ttl_secs, MAX_TTL_SECONDS);
    }

    #[test]
    fn in_zone_sets_zone_and_validates() {
        let rec = PkarrRecord::in_zone("a3net.example", sample_packet(), 60).unwrap();
        assert_eq!(rec.zone, "a3net.example");
        assert_eq!(rec.ttl_secs, 60);
    }

    #[test]
    fn in_zone_rejects_empty() {
        let err = PkarrRecord::in_zone("", sample_packet(), 30).unwrap_err();
        assert!(err.to_string().contains("pkarr zone"));
    }

    #[test]
    fn in_zone_rejects_bare_label() {
        let err = PkarrRecord::in_zone("local", sample_packet(), 30).unwrap_err();
        assert!(err.to_string().contains("expected FQDN"));
    }

    #[test]
    fn in_zone_rejects_whitespace() {
        let err = PkarrRecord::in_zone("foo bar.test", sample_packet(), 30).unwrap_err();
        assert!(err.to_string().contains("whitespace"));
    }

    #[test]
    fn in_zone_rejects_leading_dot() {
        let err = PkarrRecord::in_zone(".foo.test", sample_packet(), 30).unwrap_err();
        assert!(err.to_string().contains("leading/trailing dot"));
    }

    #[test]
    fn in_zone_rejects_trailing_dot() {
        let err = PkarrRecord::in_zone("foo.test.", sample_packet(), 30).unwrap_err();
        assert!(err.to_string().contains("leading/trailing dot"));
    }

    #[test]
    fn in_zone_rejects_non_ascii() {
        let err = PkarrRecord::in_zone("foo.π.test", sample_packet(), 30).unwrap_err();
        assert!(err.to_string().contains("non-ASCII"));
    }

    #[test]
    fn validate_zone_accepts_typical_zones() {
        for z in [
            "dns.iroh.link",
            "a3net.local",
            "a.b.c.d.example",
            "127.0.0.1.nip.io",
        ] {
            validate_zone(z).unwrap_or_else(|e| panic!("zone {z} rejected: {e}"));
        }
    }

    #[test]
    fn with_zone_overrides_and_validates() {
        let rec = PkarrRecord::new(sample_packet(), 30)
            .with_zone("my.test")
            .unwrap();
        assert_eq!(rec.zone, "my.test");
    }

    #[test]
    fn with_zone_propagates_validation_error() {
        let err = PkarrRecord::new(sample_packet(), 30)
            .with_zone("bad zone")
            .unwrap_err();
        assert!(err.to_string().contains("pkarr zone"));
    }

    #[test]
    fn with_ttl_secs_clamps_at_max() {
        let rec = PkarrRecord::new(sample_packet(), 60).with_ttl_secs(u32::MAX);
        assert_eq!(rec.ttl_secs, MAX_TTL_SECONDS);
    }

    #[test]
    fn with_ttl_secs_passes_through_lower_values() {
        let rec = PkarrRecord::new(sample_packet(), 60).with_ttl_secs(120);
        assert_eq!(rec.ttl_secs, 120);
    }

    #[test]
    fn with_packet_replaces_payload() {
        let rec = PkarrRecord::new(sample_packet(), 30).with_packet(vec![0xAA, 0xBB]);
        assert_eq!(rec.packet, vec![0xAA, 0xBB]);
    }

    #[test]
    fn is_empty_reports_zero_packet() {
        let rec = PkarrRecord::new(vec![], 30);
        assert!(rec.is_empty());
        let rec = PkarrRecord::new(vec![0x01], 30);
        assert!(!rec.is_empty());
    }

    #[test]
    fn packet_len_returns_byte_length() {
        let rec = PkarrRecord::new(vec![0u8; 42], 30);
        assert_eq!(rec.packet_len(), 42);
    }

    #[test]
    fn dns_txt_name_well_formed() {
        let n = PkarrRecord::dns_txt_name("a3net.example", "abc123");
        assert_eq!(n, "_iroh.abc123.a3net.example.");
    }

    #[test]
    fn dns_txt_name_strips_trailing_dot() {
        let n = PkarrRecord::dns_txt_name("a3net.example.", "abc123");
        assert_eq!(n, "_iroh.abc123.a3net.example.");
    }

    #[test]
    fn http_path_format() {
        assert_eq!(
            PkarrRecord::http_path("z32-key"),
            "/z32-key"
        );
    }

    #[test]
    fn display_includes_zone_and_ttl() {
        let rec = PkarrRecord::new(sample_packet(), 30);
        let s = format!("{rec}");
        assert!(s.contains("pkarr"));
        assert!(s.contains("dns.iroh.link"));
        assert!(s.contains("ttl=30s"));
    }

    #[test]
    fn serde_roundtrip() {
        let rec = PkarrRecord::in_zone("a3net.test", sample_packet(), 120).unwrap();
        let json = serde_json::to_string(&rec).unwrap();
        let back: PkarrRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rec);
    }

    #[test]
    fn serde_roundtrip_with_empty_packet() {
        let rec = PkarrRecord::in_zone("a3net.test", vec![], 30).unwrap();
        let json = serde_json::to_string(&rec).unwrap();
        let back: PkarrRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rec);
        assert!(back.is_empty());
    }

    #[test]
    fn hash_works() {
        use std::collections::HashSet;
        let a = PkarrRecord::new(sample_packet(), 30);
        let b = PkarrRecord::new(sample_packet(), 30);
        let mut set = HashSet::new();
        set.insert(a);
        set.insert(b);
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn clone_preserves_all_fields() {
        let rec = PkarrRecord::in_zone("a3net.test", sample_packet(), 60).unwrap();
        let cloned = rec.clone();
        assert_eq!(cloned, rec);
    }
}
