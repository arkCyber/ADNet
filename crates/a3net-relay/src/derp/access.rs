//! Access-control implementations for the embedded DERP server.
//!
//! Wraps [`iroh_relay::server::AccessControl`] into the
//! A3Net-friendly allowlist / denylist tri-state. Each
//! implementation is `Arc`-shareable (via the upstream `DynAccessControl`
//! trait) and `Debug`-printable.
//!
//! ## Authentication contract
//!
//! The iroh-relay server authenticates the client's `EndpointId`
//! through a TLS-exported-keying-material handshake before invoking
//! the access hook. That means `on_connect` is only called for
//! clients that proved possession of the matching Ed25519 secret.
//! Spoofing a peer's `EndpointId` requires forging a TLS exporter
//! signature, which is equivalent to breaking TLS — i.e. the access
//! control is the only meaningful policy boundary.
//!
//! [`ClientRequest::endpoint_id`](iroh_relay::server::ClientRequest::endpoint_id)
//! is therefore authentic by construction.

use std::fmt;

use iroh_base::EndpointId;
use iroh_relay::server::{Access, AccessControl, ClientRequest};
use tracing::trace;

use crate::derp::AccessConfig;

/// A3Net-flavoured accessor — names the three supported modes
/// without exposing concrete types so callers can dispatch on a
/// single shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeAccessControl {
    /// Allow every endpoint.
    Everyone,
    /// Allow only endpoints in a closed set.
    Allowlist,
    /// Allow every endpoint except those in a closed set.
    Denylist,
}

impl From<&AccessConfig> for NodeAccessControl {
    fn from(c: &AccessConfig) -> Self {
        match c {
            AccessConfig::Everyone => Self::Everyone,
            AccessConfig::Allowlist { .. } => Self::Allowlist,
            AccessConfig::Denylist { .. } => Self::Denylist,
        }
    }
}

/// `AccessControl` that admits only endpoints whose Ed25519
/// public key appears in `allow`.
#[derive(Clone)]
pub struct AllowlistAccess {
    allow: Vec<EndpointId>,
}

impl AllowlistAccess {
    pub fn new(allow: Vec<EndpointId>) -> Self {
        // Dedup defensively: a misconfigured operator with the same
        // id twice would double-cost an `O(n)` lookup. We don't
        // expect this to fire in production but the cost of
        // deduping is negligible and the test surface is nice.
        let mut deduped = allow;
        deduped.sort();
        deduped.dedup();
        Self { allow: deduped }
    }

    /// Borrow the underlying set as a slice (useful for
    /// diagnostics — `/access` admin endpoint, etc.).
    pub fn allow(&self) -> &[EndpointId] {
        &self.allow
    }
}

impl fmt::Debug for AllowlistAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AllowlistAccess")
            .field("allow_count", &self.allow.len())
            .finish_non_exhaustive()
    }
}

impl AccessControl for AllowlistAccess {
    async fn on_connect(&self, request: &ClientRequest) -> Access {
        let id = request.endpoint_id();
        if self.allow.contains(&id) {
            trace!(endpoint = %id.fmt_short(), "a3net-relay: allowlist admit");
            Access::Allow
        } else {
            trace!(endpoint = %id.fmt_short(), "a3net-relay: allowlist deny");
            Access::Deny { reason: None }
        }
    }
}

/// `AccessControl` that admits every endpoint except those whose
/// Ed25519 public key appears in `deny`.
#[derive(Clone)]
pub struct DenylistAccess {
    deny: Vec<EndpointId>,
}

impl DenylistAccess {
    pub fn new(deny: Vec<EndpointId>) -> Self {
        let mut deduped = deny;
        deduped.sort();
        deduped.dedup();
        Self { deny: deduped }
    }

    pub fn deny(&self) -> &[EndpointId] {
        &self.deny
    }
}

impl fmt::Debug for DenylistAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DenylistAccess")
            .field("deny_count", &self.deny.len())
            .finish_non_exhaustive()
    }
}

impl AccessControl for DenylistAccess {
    async fn on_connect(&self, request: &ClientRequest) -> Access {
        let id = request.endpoint_id();
        if self.deny.contains(&id) {
            trace!(endpoint = %id.fmt_short(), "a3net-relay: denylist reject");
            Access::Deny { reason: None }
        } else {
            trace!(endpoint = %id.fmt_short(), "a3net-relay: denylist admit");
            Access::Allow
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use iroh_relay::server::DynAccessControl;

    use super::*;
    use crate::derp::test_fixture::for_test_endpoint as for_test;

    #[tokio::test]
    async fn allowlist_allows_listed_only() {
        let pk_a = iroh_base::SecretKey::from_bytes(&[1u8; 32]).public();
        let pk_b = iroh_base::SecretKey::from_bytes(&[2u8; 32]).public();

        let allow = AllowlistAccess::new(vec![pk_a]);
        assert!(matches!(
            DynAccessControl::on_connect(&allow, &for_test(pk_a)).await,
            Access::Allow
        ));
        assert!(matches!(
            DynAccessControl::on_connect(&allow, &for_test(pk_b)).await,
            Access::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn denylist_blocks_listed_only() {
        let pk_a = iroh_base::SecretKey::from_bytes(&[1u8; 32]).public();
        let pk_b = iroh_base::SecretKey::from_bytes(&[2u8; 32]).public();

        let deny = DenylistAccess::new(vec![pk_a]);
        assert!(matches!(
            DynAccessControl::on_connect(&deny, &for_test(pk_a)).await,
            Access::Deny { .. }
        ));
        assert!(matches!(
            DynAccessControl::on_connect(&deny, &for_test(pk_b)).await,
            Access::Allow
        ));
    }

    #[tokio::test]
    async fn empty_allowlist_denies_everyone() {
        let pk = iroh_base::SecretKey::from_bytes(&[7u8; 32]).public();
        let allow = AllowlistAccess::new(Vec::new());
        // An empty allowlist is closed: even an authenticated
        // peer can't connect. This matches the "closed group"
        // semantics — accidentally configuring an empty allowlist
        // is a *fail-closed* outcome, which is what operators
        // generally want.
        assert!(matches!(
            DynAccessControl::on_connect(&allow, &for_test(pk)).await,
            Access::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn denylist_dedups() {
        let pk = iroh_base::SecretKey::from_bytes(&[3u8; 32]).public();
        let deny = DenylistAccess::new(vec![pk, pk, pk]);
        assert_eq!(
            deny.deny().len(),
            1,
            "dedup must collapse to a single entry"
        );
    }

    #[tokio::test]
    async fn allowlist_dedups() {
        let pk = iroh_base::SecretKey::from_bytes(&[3u8; 32]).public();
        let allow = AllowlistAccess::new(vec![pk, pk, pk]);
        assert_eq!(
            allow.allow().len(),
            1,
            "dedup must collapse to a single entry"
        );
    }

    #[test]
    fn from_access_config_dispatches_correctly() {
        let pk = iroh_base::SecretKey::from_bytes(&[4u8; 32]).public();
        let cases = [
            (AccessConfig::Everyone, NodeAccessControl::Everyone),
            (
                AccessConfig::Allowlist { allow: vec![pk] },
                NodeAccessControl::Allowlist,
            ),
            (
                AccessConfig::Denylist { deny: vec![pk] },
                NodeAccessControl::Denylist,
            ),
        ];
        for (cfg, expected) in cases {
            assert_eq!(NodeAccessControl::from(&cfg), expected);
        }
    }

    #[test]
    fn into_arc_dyn_trait_object() {
        let allow = AllowlistAccess::new(Vec::new());
        // `AccessControl` is not dyn-compatible (it returns
        // `impl Future`), so the trait-object type is
        // `DynAccessControl`. Both are blanket-impl'd for
        // `AllowlistAccess`, so the conversion is well-typed.
        let arc: Arc<dyn DynAccessControl> = Arc::new(allow);
        // Just ensure the round-trip is well-typed.
        let _ = arc;
    }
}
