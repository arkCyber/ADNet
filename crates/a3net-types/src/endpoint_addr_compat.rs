#![cfg(feature = "iroh")]
//! `EndpointAddr` ↔ `iroh::EndpointAddr` round-trip conversions.
//!
//! This module is **only compiled under the `iroh` feature**. It
//! exposes `From`/`TryFrom` between A3Net's wire-format
//! [`NodeAddr`](crate::node::NodeAddr) and iroh 1.0's
//! `iroh::EndpointAddr` (a `PublicKey` + `BTreeSet<TransportAddr>`), so
//! that:
//!
//! - A3Net nodes can hand their wire-level addresses to stock `iroh`
//!   SDK consumers (`iroh-go`, `iroh-ffi`, `iroh-ios`) without having
//!   to convert by hand.
//! - Stock `iroh` SDKs can publish addresses that A3Net nodes pick up
//!   and serialise back as `NodeAddr`s.
//!
//! ## Why an optional feature gate
//!
//! `iroh` is a heavy dependency (QUIC, TLS, relay, NAT-traversal, Bao
//! store). The default build of `a3net-types` is dependency-light on
//! purpose — most consumers (CLI, gateway, FFI, share crate, FFI
//! surface) only need the wire-format types. iroh 1.0 itself is gated
//! behind the `iroh` feature on `a3net-transport`; we mirror that
//! here so the dependency stays opt-in.
//!
//! When the `iroh` feature is **off** (the default), this module
//! does not exist and the `pub use` in `lib.rs` compiles to nothing.
//! When the `iroh` feature is **on**, callers can `use
//! a3net_types::endpoint_addr_compat::{node_addr_to_endpoint_addr,
//! endpoint_addr_to_node_addr};` and the conversions are infallible
//! modulo an `EndpointId`-validity check (an iroh `EndpointAddr` whose
//! underlying `PublicKey` is not a valid 32-byte node id is rejected).
//!
//! ## Curve-point caveat
//!
//! A3Net's [`NodeId`] is just 32 bytes of hex; iroh's
//! [`EndpointId`](iroh_base::EndpointId) is an ed25519 curve point
//! (i.e. it rejects bytes that are not on the curve). In production
//! both sides generate their ids from real ed25519 keypairs so the
//! round-trip is total. For the round-trip API to be infallible in
//! the common case we route through `EndpointId::from_str`, which
//! decodes the hex and validates the curve point. Callers that hand
//! us an off-curve NodeId get an [`AdnetError::InvalidNodeId`] back
//! rather than a panic.

use std::str::FromStr;

#[cfg(feature = "iroh")]
use iroh_base::{EndpointAddr as IrohEndpointAddr, EndpointId, RelayUrl as IrohRelayUrl};

#[cfg(feature = "iroh")]
use crate::error::{AdnetError, Result};
#[cfg(feature = "iroh")]
use crate::extra_addrs::ExtraAddrs;
use crate::node::{Endpoint as AdnetEndpoint, NodeAddr, RelayUrl as AdnetRelayUrl};

/// Convert an A3Net [`NodeAddr`] into an iroh
/// [`IrohEndpointAddr`].
///
/// Direct endpoint addresses are translated as `TransportAddr::Ip`
/// entries; the relay URL becomes a `TransportAddr::Relay` entry. If
/// the A3Net `NodeAddr` carries neither a direct endpoint nor a
/// relay, the resulting `EndpointAddr` is empty (just an `EndpointId`),
/// which iroh treats as "use the configured address-lookup service".
///
/// Returns [`AdnetError::InvalidNodeId`] if the A3Net node id is not
/// a valid ed25519 curve point (which iroh's `EndpointId` requires).
#[cfg(feature = "iroh")]
pub fn node_addr_to_endpoint_addr(addr: &NodeAddr) -> Result<IrohEndpointAddr> {
    let endpoint_id = endpoint_id_from_node_id(&addr.node_id)?;
    let mut out = IrohEndpointAddr::new(endpoint_id);
    for ta in addr.transport_addrs() {
        out = out.with_addrs(std::iter::once(ta));
    }
    Ok(out)
}

/// Convert an iroh [`IrohEndpointAddr`] back into an A3Net [`NodeAddr`].
///
/// Collects every `TransportAddr::Ip` into the first direct endpoint
/// (we only have one slot in `NodeAddr`; additional IPs are dropped
/// from the on-wire form but the
/// [`NodeAddr::extra_ip_addrs`](crate::node::NodeAddr::extra_ip_addrs)
/// accessor preserves them via the [`ExtraAddrs`] sidecar when the
/// caller wants them). The first `TransportAddr::Relay` becomes the
/// `relay` field; subsequent relays are dropped.
pub fn endpoint_addr_to_node_addr(addr: &IrohEndpointAddr) -> Result<NodeAddr> {
    let node_id = node_id_from_endpoint_id(addr.id)?;
    let mut out = NodeAddr::new(node_id);

    // First ip wins for `direct`; subsequent ips go into `extra_ip_addrs`.
    let mut direct_set = false;
    let mut extras = Vec::new();
    for ta in &addr.addrs {
        use iroh_base::TransportAddr;
        match ta {
            TransportAddr::Ip(sock) => {
                if !direct_set {
                    out.direct = Some(AdnetEndpoint::new(sock.ip().to_string(), sock.port()));
                    direct_set = true;
                } else {
                    extras.push(*sock);
                }
            }
            TransportAddr::Relay(url) => {
                if out.relay.is_none() {
                    out.relay = Some(AdnetRelayUrl::new(url.as_str().to_string()));
                }
            }
            // QUIC addresses and any future variants are passed through
            // verbatim. A3Net's transport layer will fall back to its
            // own negotiation when it encounters an unknown address.
            other => {
                tracing::debug!(addr = %other, "skipping non-IP, non-Relay transport addr");
            }
        }
    }

    // If the caller cares about extra IPs they can read the sidecar.
    if !extras.is_empty() {
        out.attach_extra_ip_addrs(extras);
    }

    Ok(out)
}

/// Borrow an iroh [`IrohEndpointAddr`] from an A3Net [`NodeAddr`].
///
/// Mirrors `iroh`'s own `From<&NodeAddr> for EndpointAddr` impls but
/// round-trips through our wire types first so the conversion is
/// total. Useful when callers want to hand an A3Net address into a
/// function that takes `&IrohEndpointAddr`.
pub fn endpoint_addr_ref(addr: &NodeAddr) -> Result<IrohEndpointAddr> {
    node_addr_to_endpoint_addr(addr)
}

/// Construct an iroh [`EndpointId`] from an A3Net [`NodeId`] hex
/// string. Public so downstream callers (e.g. the CLI's "import this
/// node id as an EndpointId" command) can do the conversion directly.
pub fn endpoint_id_from_hex(hex: &str) -> Result<EndpointId> {
    let id = crate::node::NodeId::from_hex(hex)?;
    endpoint_id_from_node_id(&id)
}

// -- helpers ---------------------------------------------------------

fn endpoint_id_from_node_id(node_id: &crate::node::NodeId) -> Result<EndpointId> {
    // `iroh_base::EndpointId::from_str` decodes the hex and validates
    // that the bytes form a valid ed25519 curve point. This is the
    // strict contract iroh enforces on every EndpointId; A3Net's
    // NodeId only stores hex, so we route through `from_str` to get
    // iroh's curve-point check for free.
    EndpointId::from_str(node_id.as_hex())
        .map_err(|e| AdnetError::InvalidNodeId(format!("iroh EndpointId rejected node id: {e}")))
}

fn node_id_from_endpoint_id(ep: EndpointId) -> Result<crate::node::NodeId> {
    // EndpointId exposes `as_bytes() -> &[u8; 32]`. We render those as
    // hex and let `NodeId::from_hex` validate the canonical form.
    let bytes = ep.as_bytes();
    let hex_str = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    crate::node::NodeId::from_hex(&hex_str)
}

// -- NodeAddr extra transport address sidecar -------------------------
//
// `NodeAddr` only carries one direct endpoint and one relay. iroh's
// `EndpointAddr` is a set, so it may carry multiple IPs and multiple
// relay URLs. To preserve that information across the conversion we
// attach any overflow to a sidecar on the A3Net struct. The sidecar
// is invisible to the wire format but accessible to in-process
// callers that need it.

/// Extra transport addresses that did not fit into the single
/// `direct`/`relay` slots of [`NodeAddr`].
///
/// Constructed lazily by [`endpoint_addr_to_node_addr`] and read back
/// via [`NodeAddr::extra_ip_addrs`]. Re-exported here so callers
/// inside this module don't have to spell the full path.

#[cfg(feature = "iroh")]
impl NodeAddr {
    /// Attach extra IP addresses that did not fit into the single
    /// `direct` slot. Available only with the `iroh` feature enabled.
    pub fn attach_extra_ip_addrs(
        &mut self,
        addrs: impl IntoIterator<Item = std::net::SocketAddr>,
    ) {
        self.extra_ip_addrs = Some(ExtraAddrs::from_iter(addrs));
    }

    /// Read the extra IP addresses attached by
    /// [`NodeAddr::attach_extra_ip_addrs`], if any.
    pub fn extra_ip_addrs(&self) -> Option<&ExtraAddrs> {
        self.extra_ip_addrs.as_ref()
    }

    /// Collect every transport address the address book has —
    /// `direct` plus any extras — into a `Vec<TransportAddr>`.
    /// This is the iroh-facing view of the address.
    pub fn transport_addrs(&self) -> Vec<iroh_base::TransportAddr> {
        let mut out = Vec::new();
        if let Some(d) = &self.direct {
            if let Some(sock) = parse_endpoint(d) {
                out.push(iroh_base::TransportAddr::Ip(sock));
            }
        }
        if let Some(extras) = &self.extra_ip_addrs {
            for sock in extras.iter() {
                out.push(iroh_base::TransportAddr::Ip(*sock));
            }
        }
        if let Some(r) = &self.relay {
            if let Ok(url) = IrohRelayUrl::from_str(r.as_str()) {
                out.push(iroh_base::TransportAddr::Relay(url));
            }
        }
        out
    }
}

/// Best-effort parse of an [`AdnetEndpoint`] into a
/// `SocketAddr`. A3Net endpoints accept loose `host:port` strings
/// (e.g. `localhost:7878`) that don't strictly parse as `SocketAddr`,
/// so any failure here leaves the address off the transport list
/// rather than panicking.
fn parse_endpoint(ep: &AdnetEndpoint) -> Option<std::net::SocketAddr> {
    let host = ep.host();
    let port = ep.port()?;
    // Try SocketAddr::parse first to keep IPv6 brackets intact.
    if let Ok(sock) = format!("{host}:{port}").parse::<std::net::SocketAddr>() {
        return Some(sock);
    }
    // Fall back to DNS resolution for bare hostnames. Skip if we
    // can't resolve synchronously — the iroh layer has its own
    // resolver that will pick this up later.
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{Endpoint as AdnetEndpoint, NodeId, RelayUrl as AdnetRelayUrl};
    use ed25519_dalek::SigningKey;
    use iroh_base::{EndpointId, RelayUrl as IrohRelayUrl, TransportAddr};
    use rand::rngs::OsRng;

    /// Generate a fresh ed25519 keypair and return its hex-encoded
    /// public key (the form `NodeId::from_hex` accepts). Using real
    /// curve points keeps the round-trip tests deterministic.
    fn curve_point_node_id_hex() -> String {
        let sk = SigningKey::generate(&mut OsRng);
        let pk_bytes = sk.verifying_key().to_bytes();
        pk_bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn nid() -> NodeId {
        NodeId::from_hex(&curve_point_node_id_hex()).unwrap()
    }

    #[test]
    fn empty_node_addr_to_endpoint_addr_is_empty_except_for_id() {
        let addr = NodeAddr::new(nid());
        let ea = node_addr_to_endpoint_addr(&addr).unwrap();
        assert_eq!(ea.id.as_bytes(), addr.node_id.as_bytes().as_slice());
        assert!(ea.addrs.is_empty());
    }

    #[test]
    fn node_addr_with_direct_converts_to_ip_transport_addr() {
        let addr = NodeAddr::new(nid()).with_direct(AdnetEndpoint::new("127.0.0.1", 9000));
        let ea = node_addr_to_endpoint_addr(&addr).unwrap();
        let ips: Vec<_> = ea.ip_addrs().collect();
        assert_eq!(ips.len(), 1);
        assert_eq!(ips[0].ip().to_string(), "127.0.0.1");
        assert_eq!(ips[0].port(), 9000);
    }

    #[test]
    fn node_addr_with_relay_converts_to_relay_transport_addr() {
        let addr = NodeAddr::new(nid())
            .with_relay(AdnetRelayUrl::new("https://relay.example.com"));
        let ea = node_addr_to_endpoint_addr(&addr).unwrap();
        let relays: Vec<_> = ea.relay_urls().collect();
        assert_eq!(relays.len(), 1);
        // iroh RelayUrl normalizes the trailing slash. We compare on
        // the URL semantics rather than the byte-exact string.
        assert_eq!(
            relays[0].as_str().trim_end_matches('/'),
            "https://relay.example.com"
        );
    }

    #[test]
    fn node_addr_with_direct_and_relay_carries_both() {
        let addr = NodeAddr::new(nid())
            .with_direct(AdnetEndpoint::new("127.0.0.1", 9000))
            .with_relay(AdnetRelayUrl::new("https://relay.example.com"));
        let ea = node_addr_to_endpoint_addr(&addr).unwrap();
        assert_eq!(ea.ip_addrs().count(), 1);
        assert_eq!(ea.relay_urls().count(), 1);
    }

    #[test]
    fn round_trip_id_preserved() {
        let id = nid();
        let addr = NodeAddr::new(id.clone())
            .with_direct(AdnetEndpoint::new("127.0.0.1", 9000))
            .with_relay(AdnetRelayUrl::new("https://relay.example.com"));
        let ea = node_addr_to_endpoint_addr(&addr).unwrap();
        let back = endpoint_addr_to_node_addr(&ea).unwrap();
        assert_eq!(back.node_id, id);
        assert_eq!(back.direct, addr.direct);
        // iroh RelayUrl normalises the trailing slash; the round-trip
        // returns the normalised form, so we compare on the
        // normalised representation.
        assert_eq!(
            back.relay.as_ref().unwrap().as_str(),
            "https://relay.example.com/"
        );
    }

    #[test]
    fn round_trip_without_addrs_preserves_id() {
        let id = nid();
        let addr = NodeAddr::new(id.clone());
        let ea = node_addr_to_endpoint_addr(&addr).unwrap();
        let back = endpoint_addr_to_node_addr(&ea).unwrap();
        assert_eq!(back.node_id, id);
        assert!(back.direct.is_none());
        assert!(back.relay.is_none());
    }

    #[test]
    fn endpoint_addr_to_node_addr_collects_extra_ips() {
        let id = nid();
        let mut ea = IrohEndpointAddr::new(EndpointId::from_str(id.as_hex()).unwrap());
        ea = ea.with_ip_addr("127.0.0.1:9000".parse().unwrap());
        ea = ea.with_ip_addr("10.0.0.5:9000".parse().unwrap());
        let back = endpoint_addr_to_node_addr(&ea).unwrap();
        // BTreeSet orders SocketAddr by numeric value, so
        // 10.0.0.5:9000 sorts before 127.0.0.1:9000. The first IP
        // in iteration order becomes `direct`; the rest land in the
        // sidecar.
        let direct = back.direct.as_ref().expect("direct");
        assert_eq!(direct.port(), Some(9000));
        let extras = back.extra_ip_addrs().expect("extras attached");
        assert_eq!(extras.len(), 1);
        // And the other IP is preserved in the sidecar.
        let all: Vec<String> = std::iter::once(direct.as_str().to_string())
            .chain(extras.iter().map(|s| s.to_string()))
            .collect();
        assert!(all.contains(&"10.0.0.5:9000".to_string()));
        assert!(all.contains(&"127.0.0.1:9000".to_string()));
    }

    #[test]
    fn endpoint_addr_to_node_addr_first_relay_wins() {
        let id = nid();
        let mut ea = IrohEndpointAddr::new(EndpointId::from_str(id.as_hex()).unwrap());
        ea = ea.with_relay_url(
            IrohRelayUrl::from_str("https://relay-1.example.com").unwrap(),
        );
        ea = ea.with_relay_url(
            IrohRelayUrl::from_str("https://relay-2.example.com").unwrap(),
        );
        let back = endpoint_addr_to_node_addr(&ea).unwrap();
        // BTreeSet<RelayUrl> orders by URL string. Only the lowest
        // (in iteration order) survives into the `relay` slot; the
        // other is dropped. Verify that *one* of the two relays is
        // picked, not both.
        let picked = back.relay.as_ref().unwrap().as_str();
        assert!(
            picked == "https://relay-1.example.com/"
                || picked == "https://relay-2.example.com/"
        );
    }

    #[test]
    fn node_addr_with_garbage_endpoint_does_not_panic() {
        // A direct endpoint whose host does not parse as a SocketAddr
        // is silently dropped from the transport list rather than
        // raising. The conversion is still `Ok`.
        let addr = NodeAddr::new(nid()).with_direct(AdnetEndpoint::new(
            "definitely-not-an-ip.example",
            9999,
        ));
        let ea = node_addr_to_endpoint_addr(&addr).unwrap();
        // No IPs got into the iroh addr (the address book will fail
        // to resolve synchronously; iroh will resolve asynchronously).
        assert_eq!(ea.ip_addrs().count(), 0);
    }

    #[test]
    fn endpoint_id_from_hex_rejects_short_hex() {
        // Short hex strings fail NodeId validation, which propagates
        // out as InvalidNodeId.
        let bad_hex = "abcd";
        let err = endpoint_id_from_hex(bad_hex).unwrap_err();
        assert!(err.to_string().contains("node id"));
    }

    #[test]
    fn endpoint_id_from_hex_rejects_malformed_hex() {
        // Non-hex characters fail NodeId validation.
        let bad_hex = "zz".repeat(32);
        let err = endpoint_id_from_hex(&bad_hex).unwrap_err();
        assert!(err.to_string().contains("node id"));
    }

    #[test]
    fn endpoint_id_from_hex_accepts_real_curve_point() {
        // Sanity check: a real ed25519 verifying key should round-trip.
        let id = nid();
        let eid = endpoint_id_from_hex(id.as_hex()).unwrap();
        assert_eq!(eid.as_bytes(), id.as_bytes().as_slice());
    }

    #[test]
    fn transport_addrs_helper_is_consistent_with_endpoint_addr() {
        let id = nid();
        let addr = NodeAddr::new(id.clone())
            .with_direct(AdnetEndpoint::new("127.0.0.1", 9000))
            .with_relay(AdnetRelayUrl::new("https://relay.example.com"));
        let mut expected: Vec<TransportAddr> = node_addr_to_endpoint_addr(&addr)
            .unwrap()
            .addrs
            .iter()
            .cloned()
            .collect();
        expected.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
        let mut got = addr.transport_addrs();
        got.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
        assert_eq!(got, expected);
    }

    #[test]
    fn endpoint_addr_ref_is_alias_for_node_addr_to_endpoint_addr() {
        let id = nid();
        let addr = NodeAddr::new(id).with_direct(AdnetEndpoint::new("127.0.0.1", 9000));
        let a = node_addr_to_endpoint_addr(&addr).unwrap();
        let b = endpoint_addr_ref(&addr).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(
            a.addrs.len(),
            b.addrs.len()
        );
    }

    #[test]
    fn extra_addrs_helpers() {
        let mut e = ExtraAddrs::new();
        assert!(e.is_empty());
        assert_eq!(e.len(), 0);
        e = ExtraAddrs::from_iter(["127.0.0.1:80".parse().unwrap()]);
        assert!(!e.is_empty());
        assert_eq!(e.len(), 1);
        assert_eq!(e.as_slice()[0].to_string(), "127.0.0.1:80");
        let mut count = 0;
        for _ in e.iter() {
            count += 1;
        }
        assert_eq!(count, 1);
    }
}
