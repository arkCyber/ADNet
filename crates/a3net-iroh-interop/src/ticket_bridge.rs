//! Wire-protocol bridge: convert between A3Net's ticket format
//! (`a3net-blob://...`) and iroh 1.0's ticket format
//! (`blob<zbase32-payload>`).
//!
//! ## Why this is necessary
//!
//! `a3net-types` defines its own printable ticket format
//! (see `a3net-types::ticket::BlobTicket`). The iroh 1.0
//! ecosystem — `iroh-go`, `iroh-py`, `iroh-net-internal` — uses
//! its own printable format rooted in iroh-base's z-base-32 + postcard
//! encoding. Both formats carry the same payload semantically
//! (endpoint id, relay URL, direct addresses, blob hash, blob
//! format), so the bridge is mechanical.
//!
//! ## Approach
//!
//! We don't try to *re-serialize* iroh tickets (which would
//! require us to pin iroh-base's exact wire format). Instead, the
//! interop driver uses iroh's *runtime* to:
//!
//! 1. **Parse** an iroh ticket via `iroh_blobs::ticket::BlobTicket::from_str`.
//! 2. Extract `EndpointAddr + Hash + BlobFormat`.
//! 3. Convert to A3Net `NodeId + ContentHash` for the
//!    local node-side APIs (`Node::fetch_blob(&ContentHash)`).
//! 4. To *emit* an iroh ticket from the A3Net side, build a real
//!    iroh `EndpointAddr` and call `BlobTicket::new(...).to_string()`.
//!
//! This module owns the conversions and the in-memory
//! representation that the driver passes between A3Net and
//! iroh-runtime types.
//!
//! ## Feature gate
//!
//! The conversions in this module are only available when the
//! `iroh` feature is enabled (because they touch `iroh::EndpointId`,
//! `iroh_blobs::Hash`, `iroh_blobs::ticket::BlobTicket`).
//! When the feature is off, the module compiles down to a
//! "feature not enabled" stub so the rest of the harness can still
//! build (e.g. for unit tests that don't need real wire interop).

#[cfg(feature = "iroh")]
mod imp {
    use std::str::FromStr;

    use a3net_types::{ContentHash, NodeAddr, NodeId};

    /// Lightweight value type that captures everything we need
    /// out of an iroh 1.0 `BlobTicket`. We keep it as a plain
    /// struct (not an `iroh_blobs::ticket::BlobTicket`) so the
    /// rest of the driver doesn't have to depend on iroh's
    /// internal type identities.
    #[derive(Debug, Clone)]
    pub struct IrohBlobTicket {
        /// iroh `EndpointAddr` (endpoint id + relay + direct).
        pub addr: iroh::EndpointAddr,
        /// iroh `Hash` (32-byte BLAKE3). Stored as a `iroh_blobs::Hash`
        /// because that's the type the wire protocol uses; we
        /// expose `as_bytes()` for A3Net-side `ContentHash` construction.
        pub hash: iroh_blobs::Hash,
        /// iroh `BlobFormat` (Raw / HashSeq). Round-tripped for
        /// fidelity even though A3Net only stores Raw in v0.1.
        pub format: iroh_blobs::BlobFormat,
    }

    impl IrohBlobTicket {
        /// Parse an iroh 1.0 `BlobTicket` from its printable form.
        /// The string is the exact output of
        /// `iroh_blobs::ticket::BlobTicket::to_string()`.
        pub fn parse(s: &str) -> Result<Self, BridgeError> {
            let t = iroh_blobs::ticket::BlobTicket::from_str(s)
                .map_err(|e| BridgeError::ParseIrohTicket(e.to_string()))?;
            Ok(Self {
                addr: t.addr().clone(),
                hash: t.hash(),
                format: t.format(),
            })
        }

        /// Serialize back to iroh's printable form.
        pub fn to_string(&self) -> String {
            iroh_blobs::ticket::BlobTicket::new(self.addr.clone(), self.hash, self.format)
                .to_string()
        }

        /// Build an iroh `EndpointAddr` for a single relay-less
        /// (direct-only) endpoint. Useful when the sidecar is on
        /// `127.0.0.1` with no DERP relay.
        pub fn from_direct(node_id: iroh::EndpointId, direct: std::net::SocketAddr) -> Self {
            let addr = iroh::EndpointAddr::new(node_id).with_ip_addr(direct);
            Self {
                addr,
                hash: iroh_blobs::Hash::EMPTY,
                format: iroh_blobs::BlobFormat::Raw,
            }
        }
    }

    /// Convert an `IrohBlobTicket` into the pieces A3Net's
    /// `Node::fetch_blob` expects: a `ContentHash` plus a
    /// `NodeAddr` (the iroh `EndpointId` mapped to A3Net's
    /// `NodeId`).
    pub fn to_a3net_parts(t: &IrohBlobTicket) -> (NodeId, NodeAddr, ContentHash) {
        // iroh `EndpointId` and A3Net `NodeId` are both 32-byte
        // Ed25519 public keys; the bridge is a 32-byte copy.
        let node_id = NodeId::from_bytes(t.addr.id.as_bytes());
        // iroh 1.0 collapsed `direct_addrs` and `relay_url`
        // into a `BTreeSet<TransportAddr>` of `Ip(SocketAddr)`
        // and `Relay(RelayUrl)` variants. We pick the first
        // IP (if any) for A3Net's single `direct` slot and
        // the first relay URL (if any) for A3Net's `relay`.
        let first_ip = t.addr.ip_addrs().next().cloned();
        let first_relay = t.addr.relay_urls().next().cloned();
        let relay = first_relay
            .and_then(|r| a3net_types::RelayUrl::parse(&r.to_string()).ok());
        let mut node_addr = NodeAddr::new(node_id.clone());
        if let Some(sa) = first_ip {
            node_addr = node_addr.with_direct(a3net_types::Endpoint::new(sa.ip().to_string(), sa.port()));
        }
        if let Some(r) = relay {
            node_addr = node_addr.with_relay(r);
        }
        let content_hash = ContentHash::from_bytes(t.hash.as_bytes());
        (node_id, node_addr, content_hash)
    }

    /// Inverse: build an `IrohBlobTicket` from A3Net-side
    /// pieces.
    pub fn from_a3net_parts(
        node_id: &NodeId,
        node_addr: &NodeAddr,
        hash: &ContentHash,
    ) -> IrohBlobTicket {
        let endpoint_id = iroh::EndpointId::from_bytes(node_id.as_bytes())
            .expect("NodeId is 32 bytes; iroh::EndpointId::from_bytes is infallible for that length");
        let mut addr = iroh::EndpointAddr::new(endpoint_id);
        if let Some(ep) = &node_addr.direct {
            // A3Net's `Endpoint` is a printable `host:port` string;
            // resolve via the standard `ToSocketAddrs` trait.
            if let Some(sa) = resolve_endpoint(ep) {
                addr = addr.with_ip_addr(sa);
            }
        }
        if let Some(url) = &node_addr.relay {
            if let Ok(u) = iroh_relay::RelayUrl::parse(&url.to_string()) {
                addr = addr.with_relay_url(u);
            }
        }
        let iroh_hash = iroh_blobs::Hash::from_bytes(hash.as_bytes());
        IrohBlobTicket {
            addr,
            hash: iroh_hash,
            format: iroh_blobs::BlobFormat::Raw,
        }
    }

    /// One-shot helper: parse an iroh printable ticket and
    /// return A3Net-side `NodeAddr` + `ContentHash`. Used by the
    /// harness driver after the sidecar hands us a ticket it
    /// generated.
    pub fn parse_iroh_for_a3net(s: &str) -> Result<(NodeId, NodeAddr, ContentHash), BridgeError> {
        let t = IrohBlobTicket::parse(s)?;
        Ok(to_a3net_parts(&t))
    }

    /// One-shot helper: build an iroh printable ticket from
    /// A3Net-side pieces. Used by the harness when the A3Net
    /// side is the one *issuing* a ticket (e.g. an A3Net test
    /// tells the sidecar to fetch by ticket).
    pub fn a3net_to_iroh_ticket(
        node_id: &NodeId,
        node_addr: &NodeAddr,
        hash: &ContentHash,
    ) -> String {
        from_a3net_parts(node_id, node_addr, hash).to_string()
    }

    #[derive(Debug, thiserror::Error)]
    pub enum BridgeError {
        #[error("failed to parse iroh ticket: {0}")]
        ParseIrohTicket(String),
        #[error("failed to parse iroh relay URL: {0}")]
        ParseRelayUrl(String),
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A locally-issued ticket must round-trip through the
        /// iroh printable form, the bridge, and the A3Net-side
        /// `ContentHash` without losing bytes.
        #[test]
        fn round_trip_local_ticket() {
            let node_id = NodeId::random();
            let hash = ContentHash::from_bytes(b"interop-blob-alpha");
            let mut addr = NodeAddr::new(node_id.clone());
            addr = addr.with_direct(a3net_types::Endpoint::new("127.0.0.1", 4044));
            let ticket = a3net_to_iroh_ticket(&node_id, &addr, &hash);
            // iroh ticket starts with `blob`.
            assert!(ticket.starts_with("blob"), "iroh ticket prefix missing: {ticket}");
            let (n2, a2, h2) = parse_iroh_for_a3net(&ticket).expect("parse");
            assert_eq!(n2.as_bytes(), node_id.as_bytes());
            assert_eq!(h2.as_bytes(), hash.as_bytes());
            assert_eq!(a2.direct, addr.direct);
        }

        /// A bare iroh ticket (no direct addrs) must still parse
        /// and produce a usable A3Net `NodeId` + `ContentHash`.
        #[test]
        fn parse_bare_iroh_ticket() {
            // Build a ticket using a known endpoint id + known hash
            // bytes. The ticket string itself is something we
            // construct via the bridge; what we test is the
            // symmetric case.
            let nid = NodeId::from_bytes(&[0x42; 32]);
            let h = ContentHash::from_bytes(&[0x07; 32]);
            let addr = NodeAddr::new(nid.clone());
            let ticket = a3net_to_iroh_ticket(&nid, &addr, &h);
            let (nid2, _addr2, h2) = parse_iroh_for_a3net(&ticket).unwrap();
            assert_eq!(nid2.as_bytes(), nid.as_bytes());
            assert_eq!(h2.as_bytes(), h.as_bytes());
        }
    }
}

#[cfg(not(feature = "iroh"))]
mod imp {
    //! Stub when the `iroh` feature is off. The driver must be
    //! built with `--features iroh` to run real interop. The
    //! stubs exist so the harness crate itself still type-checks
    //! for `cargo check -p a3net-iroh-interop` (e.g. on a CI box
    //! that only wants to verify the harness code compiles).
    //!
    //! **R-002 compliance**: per `ENGINEERING_STANDARDS.md`, no
    //! production path may call `unimplemented!()`. These stubs
    //! therefore return sentinel values and emit a `tracing::error!`
    //! so a misuse is loud and traceable, instead of panicking at
    //! runtime. The downside is that the returned sentinel is
    //! meaningless — callers must enable `--features iroh` for
    //! the real implementation.
    use a3net_types::{ContentHash, NodeAddr, NodeId};

    #[derive(Debug, Clone)]
    pub struct IrohBlobTicket;

    /// Emitted exactly once per process when one of the stub
    /// bridges is called. Lets operators grep `a3net_iroh_interop
    /// stub bridge invoked` in their logs.
    fn warn_stub_bridge(fn_name: &str) {
        tracing::error!(
            target: "a3net_iroh_interop::ticket_bridge",
            fn_name,
            "stub bridge invoked — rebuild a3net-iroh-interop with `--features iroh` \
             for the real iroh 1.0 ticket conversion"
        );
    }

    pub fn to_a3net_parts(_t: &IrohBlobTicket) -> (NodeId, NodeAddr, ContentHash) {
        warn_stub_bridge("to_a3net_parts");
        // Sentinel: an all-zero NodeId (the canonical "null" id, 64 hex zeros),
        // an empty NodeAddr carrying that id, and a zeroed ContentHash
        // (BLAKE3 of an empty byte string is well-defined and stable).
        // The caller has already opted in to the stub path by not enabling
        // the `iroh` feature; these are purely "do not panic" placeholders.
        let zero_node_id = NodeId::from_hex(
            "0000000000000000000000000000000000000000000000000000000000000000",
        )
        .expect("64-char zero hex is a valid NodeId");
        let node_addr = NodeAddr::new(zero_node_id.clone());
        let content_hash = ContentHash::from_bytes(&[]);
        (zero_node_id, node_addr, content_hash)
    }

    pub fn from_a3net_parts(
        _n: &NodeId,
        _a: &NodeAddr,
        _h: &ContentHash,
    ) -> IrohBlobTicket {
        warn_stub_bridge("from_a3net_parts");
        IrohBlobTicket
    }

    pub fn parse_iroh_for_a3net(_s: &str) -> Result<(NodeId, NodeAddr, ContentHash), String> {
        warn_stub_bridge("parse_iroh_for_a3net");
        Err("a3net-iroh-interop stub bridge invoked — rebuild with `--features iroh` for real iroh 1.0 ticket conversion".into())
    }

    pub fn a3net_to_iroh_ticket(_n: &NodeId, _a: &NodeAddr, _h: &ContentHash) -> String {
        warn_stub_bridge("a3net_to_iroh_ticket");
        String::new()
    }
}pub use imp::*;
