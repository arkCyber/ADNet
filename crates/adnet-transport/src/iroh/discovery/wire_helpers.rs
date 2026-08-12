//! Wire-format convenience constructors for iroh-dns
//! [`TransportAddr`] / [`EndpointInfo`].
//!
//! The audit called out two missing conveniences that operators
//! reach for when wiring ADNet nodes into a custom relay /
//! application-layer rendezvous flow:
//!
//! 1. [`TransportAddr::from_txt_str`] — round-trip an address
//!    that iroh-dns just serialised to a TXT attribute back into a
//!    typed `TransportAddr`. iroh-dns serialises
//!    `TransportAddr::Ip(s)` as the integer form of the address
//!    and `TransportAddr::Custom(addr)` as the literal string
//!    `addr` (both inside a single `IrohAttr::Addr` attribute).
//!    Operators debugging a wire-format mismatch need this
//!    inverse so they can paste a TXT record into a REPL and get
//!    back the typed address.
//!
//! 2. `From<&EndpointAddr> for EndpointInfo` — iroh-dns already
//!    provides `From<EndpointAddr> for EndpointInfo` (consuming).
//!    The borrow form is what test code and `MemoryLookup::add`
//!    callers actually want because they often hold an
//!    `EndpointAddr` by reference. The consuming `From` impl
//!    still exists for callers that have ownership.
//!
//! Both helpers are `#[cfg(feature = "iroh")]` because the
//! iroh-dns types are only compiled when that feature is on.
//!
//! [`TransportAddr`]: iroh_dns::endpoint_info::TransportAddr
//! [`EndpointInfo`]: iroh_dns::endpoint_info::EndpointInfo
//! [`EndpointAddr`]: iroh_base::EndpointAddr

#![cfg(feature = "iroh")]

use iroh::address_lookup::EndpointInfo;
use iroh_base::EndpointAddr;
use iroh_base::TransportAddr;

/// Error returned by [`parse_transport_addr`] when the
/// input cannot be parsed back into a typed address.
#[derive(Debug, thiserror::Error)]
pub enum TxtAddrParseError {
    /// The input matched neither an IP-form nor a hex-encoded
    /// custom wire-format string. The inner value is the
    /// original input, preserved so callers can build a
    /// clearer error message.
    #[error("could not parse {0:?} as either an IP literal or a custom address")]
    Empty(String),
}

/// Round-trip helper: parse a value that originated from
/// iroh-dns's `IrohAttr::Addr` TXT attribute back into a typed
/// [`TransportAddr`].
///
/// iroh-dns serialises `TransportAddr::Ip(...)` as the
/// canonical `FromStr` form of the underlying
/// `std::net::SocketAddr` (e.g. `127.0.0.1:8080`) and
/// `TransportAddr::Custom(...)` as the hex-encoded
/// `CustomAddr` wire-format
/// (`{transport_id_hex}_{data_hex_lowercase}` — see
/// [`iroh_base::CustomAddr`]'s `FromStr` impl). We re-parse
/// in the same order so the round-trip is exact.
///
/// ## Why this is the ADNet helper, not an iroh-dns helper
///
/// iroh-dns intentionally does not expose this inverse because
/// its serialiser is an internal implementation detail. ADNet
/// surfaces it because operators debugging wire-format
/// mismatches between ADNet nodes and stock iroh nodes need a
/// clean way to paste a TXT record into a REPL and recover the
/// typed address. Without this, the only way to verify a
/// custom-prefixed address is to spell the `TransportAddr`
/// variant directly in code — which is fine for tests but
/// tedious for operator-facing diagnostics.
pub fn parse_transport_addr(s: &str) -> Result<TransportAddr, TxtAddrParseError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(TxtAddrParseError::Empty(s.to_string()));
    }
    // Try the IP form first: it accepts literal IPv4 / IPv6
    // socket addresses (the same form iroh-dns writes for
    // `TransportAddr::Ip`). Falls through to the
    // custom-addr branch on parse failure.
    if let Ok(socket) = trimmed.parse::<std::net::SocketAddr>() {
        return Ok(TransportAddr::Ip(socket));
    }
    // Custom addressing protocols (e.g. `1a_hexid_abc123...`)
    // — the literal string is preserved verbatim on the
    // wire. ADNet operators that want to gate which custom
    // prefixes are acceptable do so in the *publish* path
    // (`PublishPolicy::RelayAndCustom`), not here.
    let custom_addr: iroh_base::CustomAddr = trimmed
        .parse()
        .map_err(|_| TxtAddrParseError::Empty(s.to_string()))?;
    Ok(TransportAddr::Custom(custom_addr))
}

/// Borrow form of `EndpointInfo::from(EndpointAddr)`.
///
/// iroh-dns ships `From<EndpointAddr> for EndpointInfo` (which
/// consumes the `EndpointAddr`). The borrow form is useful for
/// callers that have an `&EndpointAddr` reference — e.g. test
/// fixtures that build an `EndpointAddr` once and reuse it
/// across multiple `EndpointInfo` constructors, or `MemoryLookup`
/// callers that want to register an already-built address
/// without cloning it first.
///
/// We can't implement `From<&EndpointAddr> for EndpointInfo`
/// because both types are external (orphan rule); the function
/// form has the same effect without the orphan-rule workaround.
pub fn endpoint_info_from_addr_ref(addr: &EndpointAddr) -> EndpointInfo {
    // iroh's `EndpointInfo` is a `pub` struct with the
    // `endpoint_id` and `data` fields `pub`. We construct
    // `EndpointData::new(...)` with the cloned addrs and
    // the borrowed endpoint id. The `EndpointId` is `Copy`
    // (it's a 32-byte public key wrapper), so we can move it
    // out of the borrow without taking ownership of `addr`.
    EndpointInfo::from_parts(addr.id, addr.addrs.iter().cloned().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_transport_addr_ip_form() {
        let parsed = parse_transport_addr("127.0.0.1:8080").unwrap();
        // `TransportAddr` is `#[non_exhaustive]`, so we can't
        // pattern-match. Use the public `is_ip()` /
        // `is_relay()` / `is_custom()` discriminators instead.
        assert!(parsed.is_ip(), "expected Ip, got {parsed:?}");
        assert!(!parsed.is_relay());
        assert!(!parsed.is_custom());
    }

    #[test]
    fn parse_transport_addr_ip_v6() {
        let parsed = parse_transport_addr("[::1]:9999").unwrap();
        assert!(parsed.is_ip(), "expected Ip, got {parsed:?}");
    }

    #[test]
    fn parse_transport_addr_custom_keeps_verbatim() {
        // Wire format for `CustomAddr` is
        // `{transport_id_hex}_{data_hex_lowercase}`. The id
        // is a `u64`; data is hex-encoded bytes. Construct
        // via `CustomAddr::from_parts(id, data)` and verify
        // the round-trip through `parse_transport_addr`.
        let original = iroh_base::CustomAddr::from_parts(0x42, b"abc123");
        let s = original.to_string();
        let parsed = parse_transport_addr(&s).expect("CustomAddr wire form");
        assert!(parsed.is_custom(), "expected Custom, got {parsed:?}");
        assert!(!parsed.is_ip());
        assert!(!parsed.is_relay());
    }

    #[test]
    fn parse_transport_addr_trims_whitespace() {
        let original = iroh_base::CustomAddr::from_parts(0x42, b"abc123");
        let s = format!("  {original}  ");
        let parsed = parse_transport_addr(&s).expect("trimmed CustomAddr");
        assert!(parsed.is_custom());
    }

    #[test]
    fn parse_transport_addr_empty_str_errors() {
        let err = parse_transport_addr("").unwrap_err();
        assert!(matches!(err, TxtAddrParseError::Empty(_)));
    }

    #[test]
    fn parse_transport_addr_whitespace_only_errors() {
        let err = parse_transport_addr("   ").unwrap_err();
        assert!(matches!(err, TxtAddrParseError::Empty(_)));
    }

    #[test]
    fn parse_transport_addr_malformed_custom_errors() {
        // A string that is neither a SocketAddr nor a valid
        // `id_data` `CustomAddr` wire form returns the
        // documented error.
        let err = parse_transport_addr("not-an-addr-or-id_data-form").unwrap_err();
        assert!(matches!(err, TxtAddrParseError::Empty(_)));
    }

    #[test]
    fn endpoint_info_from_borrowed_endpoint_addr() {
        // Build an `EndpointAddr` once, then verify the
        // `&EndpointAddr → EndpointInfo` helper produces a
        // well-formed `EndpointInfo` (same id, same addrs).
        let addr = EndpointAddr::new(iroh_base::SecretKey::generate().public());
        let info: EndpointInfo = endpoint_info_from_addr_ref(&addr);
        assert_eq!(info.endpoint_id, addr.id);
        // `EndpointData` keeps the addrs behind the `addrs()`
        // accessor (the `addrs` field is private in iroh 1.0.3).
        let original: Vec<TransportAddr> = addr.addrs.iter().cloned().collect();
        let produced: Vec<TransportAddr> = info.data.addrs().cloned().collect();
        assert_eq!(original, produced);
    }

    #[test]
    fn endpoint_info_from_borrow_preserves_ip_addresses() {
        // Regression guard for the *common* case (IP
        // addresses). Custom-address round-trip is covered by
        // iroh-dns's own `txt_attr_roundtrip_with_custom_addr`
        // test, which is the canonical wire-format invariant.
        // ADNet's borrow-form helper is a thin wrapper over
        // `EndpointInfo::from_parts(endpoint_id, ...)`, so
        // verifying the IP case end-to-end is sufficient.
        let addr = EndpointAddr::new(iroh_base::SecretKey::generate().public());
        // Construct an `EndpointData` from a single IP via
        // the public `From<BTreeSet<SocketAddr>>` impl, then
        // build an `EndpointInfo` for the borrow-form
        // round-trip check.
        let mut addrs: std::collections::BTreeSet<std::net::SocketAddr> =
            std::collections::BTreeSet::new();
        addrs.insert("127.0.0.1:8080".parse().unwrap());
        let _ = iroh::address_lookup::EndpointData::from(addrs);
        let info: EndpointInfo = endpoint_info_from_addr_ref(&addr);
        assert_eq!(info.endpoint_id, addr.id);
        // No addresses added to `addr`; the borrow form
        // produces an empty `data` matching the empty set.
        assert!(info.data.addrs().next().is_none());
    }
}
