//! Extra `SocketAddr`s attached to a [`NodeAddr`] that didn't fit
//! into the single `direct` slot.
//!
//! This is a plain Rust type so it can be referenced unconditionally
//! (the [`crate::node::NodeAddr`] struct carries an
//! `Option<ExtraAddrs>` field with no `cfg` gate). The iroh-aware
//! `From`/`TryFrom` implementations between
//! [`crate::node::NodeAddr`] and `iroh_base::EndpointAddr` live in
//! [`crate::endpoint_addr_compat`] and require the `iroh` feature.

use serde::{Deserialize, Serialize};

/// Wrapper around a `Vec<SocketAddr>` so the field name is
/// self-documenting at call sites.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExtraAddrs(Vec<std::net::SocketAddr>);

impl ExtraAddrs {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn from_iter<I: IntoIterator<Item = std::net::SocketAddr>>(it: I) -> Self {
        Self(it.into_iter().collect())
    }

    pub fn as_slice(&self) -> &[std::net::SocketAddr] {
        &self.0
    }

    pub fn iter(&self) -> impl Iterator<Item = &std::net::SocketAddr> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddr};

    fn addr(a: u8, b: u8, c: u8, d: u8, port: u16) -> SocketAddr {
        SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::new(a, b, c, d)), port)
    }

    #[test]
    fn new_is_empty() {
        let e = ExtraAddrs::new();
        assert!(e.is_empty());
        assert_eq!(e.len(), 0);
        assert_eq!(e.as_slice(), &[] as &[SocketAddr]);
    }

    #[test]
    fn default_is_empty() {
        let e = ExtraAddrs::default();
        assert!(e.is_empty());
    }

    #[test]
    fn from_iter_collects_addresses() {
        let ips = vec![addr(127, 0, 0, 1, 1234), addr(10, 0, 0, 1, 9999)];
        let e = ExtraAddrs::from_iter(ips);
        assert_eq!(e.len(), 2);
        assert_eq!(e.as_slice()[0].port(), 1234);
        assert_eq!(e.as_slice()[1].port(), 9999);
    }

    #[test]
    fn iter_yields_each_socket_addr() {
        let e = ExtraAddrs::from_iter([addr(1, 2, 3, 4, 5)]);
        let collected: Vec<_> = e.iter().collect();
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0].port(), 5);
    }

    #[test]
    fn clone_is_independent() {
        let mut a = ExtraAddrs::from_iter([addr(1, 2, 3, 4, 5)]);
        let b = a.clone();
        // Ensure `b` has the same content but mutating `a` later
        // wouldn't change `b` (different Vec allocations).
        assert_eq!(a, b);
        // sanity: serde round-trip
        let json = serde_json::to_string(&a).unwrap();
        let back: ExtraAddrs = serde_json::from_str(&json).unwrap();
        assert_eq!(back, a);
        // touch `a` to avoid unused warning if features shrink.
        let _ = &mut a;
    }
}