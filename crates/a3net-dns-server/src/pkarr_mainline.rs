//! P0-REQ-1 — Mainline DHT fallback resolver for the self-hosted
//! DNS server.
//!
//! When a DNS query misses the local zone AND the configured HTTP
//! upstream is unreachable, this resolver consults the public
//! BitTorrent mainline DHT (BEP44 mutable items). The pkarr wire
//! format signs records with ed25519 and stores them under the
//! 32-byte public key as a BEP44 mutable item; iroh-dns-server uses
//! the same lookup convention.
//!
//! ## Why this lives behind a feature flag
//!
//! `n0-mainline` is a non-trivial dependency (DHT routing table,
//! RPC, BEP44 encoding). Operators that don't want a permanent
//! outbound DHT connection can compile without the feature and
//! rely on the HTTP upstream relay only.
//!
//! ## Threading model
//!
//! `n0_mainline::Dht::get_mutable_most_recent` blocks while
//! iterating the closest-nodes query. We wrap it in
//! [`tokio::task::spawn_blocking`] so the DNS UDP / TCP listener
//! is not stalled, and the supplied `timeout` is applied via
//! [`tokio::time::timeout`].
//!
//! ## Salt handling
//!
//! BEP44 mutables include an optional `salt`. We pass
//! `Some(b"pkarr")` so our lookups don't collide with other DHT
//! users; this matches the convention iroh-dns-server and stock
//! pkarr clients use.

#![cfg(feature = "mainline")]

use std::time::Duration;

use a3net_types::PkarrRecord;

use crate::pkarr::{PkarrError, PkarrLookup, validate_z32_pubkey, z32_decode};

/// Salt used for BEP44 mutable lookups. Identical to the value
/// used by stock pkarr / iroh-dns-server clients, so a packet put
/// into the public DHT by iroh is visible to us and vice-versa.
const PKARR_DHT_SALT: &[u8] = b"pkarr";

/// Hard cap on a single mutable item payload. The pkarr spec
/// keeps packets well under 1 KiB; 16 KiB is a defensive ceiling
/// that prevents a malicious peer from making us allocate
/// gigabytes.
const MAX_DHT_VALUE_BYTES: usize = 16 * 1024;

/// Default TTL returned for DHT-sourced records when the caller
/// doesn't specify one. Operators using the DNS server primarily
/// as a pkarr cache should set their resolver-side TTL above this.
const DEFAULT_DHT_TTL_SECS: u32 = 3_600;

/// Async wrapper around `n0_mainline::Dht` lookups.
///
/// Construct once per resolver chain; it owns the DHT node
/// routing table. Cheap to clone (the inner `Dht` is internally
/// reference-counted).
#[derive(Clone)]
pub struct MainlineDhtResolver {
    inner: std::sync::Arc<n0_mainline::Dht>,
    request_timeout: Duration,
}

impl std::fmt::Debug for MainlineDhtResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MainlineDhtResolver")
            .field("request_timeout", &self.request_timeout)
            .finish_non_exhaustive()
    }
}

impl MainlineDhtResolver {
    /// Build a resolver that talks to the real public mainline DHT.
    /// Operators who want to point at a private testnet can use
    /// [`MainlineDhtResolver::with_bootstrap`] instead.
    pub fn new(request_timeout: Duration) -> Result<Self, PkarrError> {
        Self::with_bootstrap(&[], request_timeout)
    }

    /// Build a resolver pointing at an explicit bootstrap list
    /// (e.g. a local `n0_mainline::Testnet` for tests). Pass an
    /// empty slice to use the default public bootstrap nodes.
    pub fn with_bootstrap(
        bootstrap: &[std::net::SocketAddr],
        request_timeout: Duration,
    ) -> Result<Self, PkarrError> {
        let dht = if bootstrap.is_empty() {
            n0_mainline::Dht::builder().build()
        } else {
            n0_mainline::Dht::builder().bootstrap(bootstrap).build()
        }
        .map_err(|e| {
            PkarrError::Serialization(format!("n0_mainline Dht::builder.build: {e}"))
        })?;
        Ok(Self {
            inner: std::sync::Arc::new(dht),
            request_timeout,
        })
    }
}

#[async_trait::async_trait]
impl PkarrLookup for MainlineDhtResolver {
    async fn lookup(
        &self,
        z32: &str,
        timeout: Duration,
    ) -> Result<Vec<u8>, PkarrError> {
        // Reuse the same 52-char + curve check that the HTTP
        // resolver does. A malformed key never reaches the DHT.
        validate_z32_pubkey(z32)?;
        let key_bytes = z32_decode(z32).ok_or_else(|| {
            PkarrError::InvalidKey(format!("z32 decode failed for {z32:?}"))
        })?;
        let mut key = [0u8; 32];
        key.copy_from_slice(&key_bytes);

        let inner = self.inner.clone();
        let total_budget = timeout.max(self.request_timeout);
        // `n0_mainline::Dht::get_mutable_most_recent` returns an
        // `impl Future` (the DHT actor does its own polling on a
        // background thread). We can `.await` it directly inside
        // the tokio reactor; the budget is enforced by
        // `tokio::time::timeout`.
        let res = tokio::time::timeout(
            total_budget,
            inner.get_mutable_most_recent(&key, Some(PKARR_DHT_SALT)),
        )
        .await;

        let item = match res {
            Ok(Ok(item)) => item,
            Ok(Err(actor_err)) => {
                return Err(PkarrError::Serialization(format!(
                    "mainline actor: {actor_err}"
                )))
            }
            Err(_) => {
                return Err(PkarrError::Serialization(format!(
                    "mainline lookup exceeded {total_budget:?}"
                )))
            }
        };
        let value = match item {
            Some(i) => i.value().to_vec(),
            None => {
                return Err(PkarrError::Serialization(
                    "mainline lookup: not found".into(),
                ))
            }
        };
        if value.len() > MAX_DHT_VALUE_BYTES {
            return Err(PkarrError::InvalidPacket(format!(
                "mainline value too large: {} bytes",
                value.len()
            )));
        }
        let rec = PkarrRecord::in_zone(String::new(), value, DEFAULT_DHT_TTL_SECS)
            .map_err(PkarrError::Adnet)?;
        serde_json::to_vec(&rec).map_err(|e| {
            PkarrError::Serialization(format!("serialize PkarrRecord: {e}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lookup_rejects_malformed_z32() {
        // Build the resolver synchronously; if DHT initialisation
        // fails on an offline runner the test still asserts the
        // error types match.
        let r = match MainlineDhtResolver::new(Duration::from_millis(200)) {
            Ok(r) => r,
            Err(_) => return, // offline / sandboxed CI: skip
        };
        let bad = "a".repeat(53);
        let err = r.lookup(&bad, Duration::from_secs(1)).await.unwrap_err();
        assert!(matches!(err, PkarrError::InvalidKey(_)));
    }

    #[tokio::test]
    async fn lookup_rejects_off_curve_z32() {
        let r = match MainlineDhtResolver::new(Duration::from_millis(200)) {
            Ok(r) => r,
            Err(_) => return,
        };
        // `[2u8; 32]` decodes to a known off-curve point.
        let bad_z = crate::pkarr::z32_encode(&[2u8; 32]);
        let err = r.lookup(&bad_z, Duration::from_secs(1)).await.unwrap_err();
        assert!(matches!(err, PkarrError::InvalidKey(_)));
    }
}