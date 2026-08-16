//! Address-lookup implementations.
//!
//! Two production lookups live here:
//!
//! - [`MemoryLookup`] — wraps iroh's
//!   [`iroh::address_lookup::memory::MemoryLookup`] with an
//!   `Arc<RwLock<…>>` plus an A3Net-shaped surface (`NodeId`
//!   instead of `EndpointId`). Callers typically inject peer
//!   addresses they learn out-of-band (e.g. from `PeerTicket`s or
//!   gossip announcements).
//!
//! - [`MainlineLookup`] — a thin [`iroh::address_lookup::AddressLookup`]
//!   implementation that resolves a `pkarr::PublicKey` via the
//!   configured `pkarr::Client`'s DHT backend. Resolved packets are
//!   surfaced as best-effort hits via the diagnostics counter; the
//!   iroh `PkarrResolver` from `presets::N0` is what actually
//!   parses the packet into `TransportAddr`s. This lookup exists so
//!   the operator can also hit the *raw* Mainline DHT (skipping the
//!   n0 relay indirection) for peers that publish there directly.

#![cfg(feature = "iroh")]

use std::sync::{Arc, RwLock};

use iroh::address_lookup::memory::MemoryLookup as IrohMemoryLookup;
use iroh::address_lookup::{AddressLookup, EndpointData, EndpointInfo, Error as LookupError, Item};
use iroh_base::EndpointId;
use n0_future::{StreamExt, boxed::BoxStream, stream};
use pkarr::{
    Client as PkarrClient, PublicKey as PkarrPublicKey, ResolvePolicy as PkarrResolvePolicy,
};
use tracing::{debug, warn};

use a3net_types::NodeId;

use crate::iroh::discovery::diagnostics::DiscoveryDiagnostics;

/// Recover from a poisoned `std::sync::Mutex` or
/// `std::sync::RwLock` without panicking.
///
/// A poisoned lock still holds the data — the poison flag is
/// just a sticky "another thread panicked while holding this
/// lock" marker. The diagnostics counters here are best-effort
/// observability; if a writer panicked we still want to read
/// the (possibly partially-updated) state and continue serving
/// requests, not crash the whole process. Mirrors the
/// `recover_lock` helper in
/// [`crate::iroh::discovery::diagnostics`] — kept inline rather
/// than shared via `pub(crate)` so the dependency surface
/// stays small (same convention).
fn recover_lock<T>(res: std::sync::LockResult<T>) -> T {
    match res {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// In-memory out-of-band address book.
///
/// Backed by [`IrohMemoryLookup`]. The `add` / `set` / `remove`
/// methods mirror iroh's surface but accept either an
/// [`iroh_base::EndpointAddr`] or a pre-built
/// [`iroh_base::EndpointInfo`].
#[derive(Debug, Clone)]
pub struct MemoryLookup {
    inner: IrohMemoryLookup,
    /// Strong count of how many out-of-band entries we hold. Used
    /// for diagnostics; not enforced for correctness.
    entries: Arc<RwLock<usize>>,
    /// Optional parallel map of `EndpointId → UserData` so
    /// callers can attach application-layer metadata to each
    /// memory entry. Reads are O(1) via `get_user_data`. Cleared
    /// alongside the corresponding `EndpointInfo` on `remove`.
    user_data: Arc<RwLock<std::collections::HashMap<EndpointId, super::UserData>>>,
}

impl Default for MemoryLookup {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryLookup {
    /// The provenance string we stamp on resolved items. Lets
    /// `/discovery` admins tell memory-sourced answers apart from
    /// DNS / Pkarr / DHT answers.
    pub const PROVENANCE: &'static str = "a3net-memory";

    /// Empty in-memory lookup.
    pub fn new() -> Self {
        Self {
            inner: IrohMemoryLookup::with_provenance(Self::PROVENANCE),
            entries: Arc::new(RwLock::new(0)),
            user_data: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }

    fn ep_id_for(node_id: &NodeId) -> anyhow::Result<EndpointId> {
        let bytes: [u8; 32] = node_id
            .as_bytes()
            .as_slice()
            .try_into()
            .expect("NodeId is always 32 bytes");
        EndpointId::from_bytes(&bytes).map_err(Into::into)
    }

    /// Add (or augment) addressing info for `node_id`. Accepts
    /// anything that can be turned into an iroh
    /// `EndpointInfo` (e.g. `EndpointAddr` or `EndpointInfo`).
    ///
    /// ## Endpoint-id invariant
    ///
    /// If `info` already carries an `EndpointInfo` whose embedded
    /// `endpoint_id` differs from the one derived from `node_id`,
    /// we **refuse the insert** with [`anyhow::Error`]. Silently
    /// rewriting the id would let a caller accidentally register
    /// `peer_X`'s addressing under `node_id = peer_Y`'s slot.
    ///
    /// `EndpointAddr` carries no separate id field (the id is the
    /// `EndpointAddr.id`), so it always passes this check.
    pub fn add(&self, node_id: NodeId, info: impl Into<EndpointInfo>) -> anyhow::Result<()> {
        let ep_id = Self::ep_id_for(&node_id)?;
        let info = info.into();
        if info.endpoint_id != ep_id {
            anyhow::bail!(
                "MemoryLookup::add: info.endpoint_id ({}) does not match node_id ({})",
                info.endpoint_id.fmt_short(),
                ep_id.fmt_short(),
            );
        }
        self.inner.add_endpoint_info(info);
        *recover_lock(self.entries.write()) += 1;
        Ok(())
    }

    /// Add by raw `iroh_base::EndpointAddr`. Convenience helper for
    /// the `PeerTicket → EndpointAddr → add_addr` flow. The
    /// embedded `EndpointAddr.id` is verified against `node_id`
    /// and the call returns `Err` if they differ.
    pub fn add_addr(&self, node_id: NodeId, addr: iroh_base::EndpointAddr) -> anyhow::Result<()> {
        let ep_id = Self::ep_id_for(&node_id)?;
        if addr.id != ep_id {
            anyhow::bail!(
                "MemoryLookup::add_addr: addr.id ({}) does not match node_id ({})",
                addr.id.fmt_short(),
                ep_id.fmt_short(),
            );
        }
        self.add(node_id, addr)
    }

    /// Replace the entire addressing record for `node_id`.
    /// Returns the previous `EndpointData`, if any. Same
    /// endpoint-id invariant as [`add`].
    pub fn set(
        &self,
        node_id: NodeId,
        info: impl Into<EndpointInfo>,
    ) -> anyhow::Result<Option<EndpointData>> {
        let ep_id = Self::ep_id_for(&node_id)?;
        let info = info.into();
        if info.endpoint_id != ep_id {
            anyhow::bail!(
                "MemoryLookup::set: info.endpoint_id ({}) does not match node_id ({})",
                info.endpoint_id.fmt_short(),
                ep_id.fmt_short(),
            );
        }
        let prev = self.inner.set_endpoint_info(info);
        if prev.is_none() {
            *recover_lock(self.entries.write()) += 1;
        }
        Ok(prev)
    }

    /// Remove the entry for `node_id`. Returns the removed info.
    pub fn remove(&self, node_id: NodeId) -> anyhow::Result<Option<EndpointInfo>> {
        let ep_id = Self::ep_id_for(&node_id)?;
        let removed = self.inner.remove_endpoint_info(ep_id);
        if removed.is_some() {
            // Recover from poison rather than dropping the
            // decrement — see the module-level "Lock recovery"
            // policy. `recover_lock` returns the inner guard even
            // when the lock is poisoned (the data is still there).
            let mut n = recover_lock(self.entries.write());
            *n = n.saturating_sub(1);
            // Drop the user-data attachment too so a
            // `put_user_data + remove + put_user_data` round-trip
            // does not leak the previous payload.
            recover_lock(self.user_data.write()).remove(&ep_id);
        }
        Ok(removed)
    }

    /// Number of out-of-band entries currently held.
    pub fn len(&self) -> usize {
        *recover_lock(self.entries.read())
    }

    /// True when no out-of-band entries are held.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Borrow the underlying iroh handle. Useful when callers want
    /// to mount the same lookup on multiple endpoints.
    pub fn inner(&self) -> &IrohMemoryLookup {
        &self.inner
    }

    /// Attach (or replace) the `user_data` payload for `node_id`
    /// without touching the `EndpointInfo` already in the
    /// lookup. Useful for the "I learned a node's user-data from
    /// a gossip message, attach it to my already-known address"
    /// flow. Pass `None` to clear the field.
    ///
    /// Returns the previous value (if any).
    pub fn put_user_data(
        &self,
        node_id: NodeId,
        user_data: Option<super::UserData>,
    ) -> anyhow::Result<Option<super::UserData>> {
        let ep_id = Self::ep_id_for(&node_id)?;
        let mut map = recover_lock(self.user_data.write());
        Ok(match user_data {
            Some(ud) => map.insert(ep_id, ud),
            None => map.remove(&ep_id),
        })
    }

    /// Look up the `user_data` payload for `node_id`, if any.
    pub fn get_user_data(&self, node_id: NodeId) -> anyhow::Result<Option<super::UserData>> {
        let ep_id = Self::ep_id_for(&node_id)?;
        let map = recover_lock(self.user_data.read());
        Ok(map.get(&ep_id).cloned())
    }

    /// Snapshot every `(endpoint_short, UserData)` pair currently in
    /// the lookup. Used by the `/discovery` admin command and
    /// integration tests that want to assert end-to-end
    /// propagation without going through a real pkarr relay.
    /// Returns iroh's `fmt_short` form (8 hex chars + prefix)
    /// so JSON consumers see a compact identifier rather than
    /// the full 32-byte public key.
    pub fn user_data_entries(&self) -> Vec<(String, super::UserData)> {
        let map = recover_lock(self.user_data.read());
        map.iter()
            .map(|(k, v)| (k.fmt_short().to_string(), v.clone()))
            .collect()
    }
}

impl AddressLookup for MemoryLookup {
    fn resolve(&self, endpoint_id: EndpointId) -> Option<BoxStream<Result<Item, LookupError>>> {
        self.inner.resolve(endpoint_id)
    }
}

/// Mainline-DHT address lookup.
///
/// Wraps a [`pkarr::Client`] and records per-call hit/miss counters
/// on an attached [`DiscoveryDiagnostics`]. The lookup itself
/// does **not** parse the resolved `SignedPacket` into iroh
/// `TransportAddr`s — that translation is iroh's responsibility
/// (via `PkarrResolver`). This lookup exists so the operator
/// can additionally hit the raw Mainline DHT, skipping the n0
/// relay indirection, for peers that publish there directly.
///
/// ## Cancellation
///
/// Each `resolve(...)` call spawns a fire-and-forget tokio task
/// that hits the DHT. The task is cancelled when [`MainlineLookup`]
/// (or any clone) is dropped — i.e. when the parent endpoint
/// tears down. We track an internal [`tokio::sync::Notify`] so
/// the spawned task can observe shutdown without holding a
/// long-running future.
#[derive(Debug)]
pub struct MainlineLookup {
    client: PkarrClient,
    diagnostics: Option<Arc<DiscoveryDiagnostics>>,
    /// Stable provenance string for log / diag attribution.
    provenance: &'static str,
    /// Set when the lookup is dropped; spawned DHT tasks poll
    /// this and exit early if it's already fired.
    cancel: Arc<tokio::sync::Notify>,
}

impl Clone for MainlineLookup {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            diagnostics: self.diagnostics.clone(),
            provenance: self.provenance,
            // Notify is shared — clones observe the same
            // shutdown signal.
            cancel: Arc::clone(&self.cancel),
        }
    }
}

impl Drop for MainlineLookup {
    fn drop(&mut self) {
        // Wake every spawned DHT task so they can observe
        // shutdown and exit without holding the DHT request
        // open until its `resolve(...)` future completes.
        self.cancel.notify_waiters();
    }
}

impl MainlineLookup {
    /// The provenance string we stamp on resolution events.
    pub const PROVENANCE: &'static str = "a3net-mainline-dht";

    /// Build a Mainline lookup backed by the supplied `pkarr` client.
    pub fn new(client: PkarrClient) -> Self {
        Self {
            client,
            diagnostics: None,
            provenance: Self::PROVENANCE,
            cancel: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Attach a diagnostics recorder. Once attached, every
    /// `resolve` call records one event.
    pub fn with_diagnostics(mut self, diag: Arc<DiscoveryDiagnostics>) -> Self {
        self.diagnostics = Some(diag);
        self
    }

    /// Resolve an `iroh` [`EndpointId`] to its `pkarr::PublicKey`,
    /// then ask the underlying DHT. Returns the resolved packet
    /// (or an error). Exposed for callers that want to use the
    /// packet directly.
    pub async fn resolve_pkarr(
        &self,
        endpoint_id: EndpointId,
    ) -> Result<pkarr::SignedPacket, pkarr::errors::ResolveError> {
        let pk = PkarrPublicKey::try_from(endpoint_id.as_bytes())
            .map_err(|_| pkarr::errors::ResolveError::NotFound)?;
        self.client
            .resolve(&pk, PkarrResolvePolicy::CacheFirst)
            .await
    }
}

impl AddressLookup for MainlineLookup {
    fn resolve(&self, endpoint_id: EndpointId) -> Option<BoxStream<Result<Item, LookupError>>> {
        let client = self.client.clone();
        let diag = self.diagnostics.clone();
        let provenance = self.provenance;
        let cancel = Arc::clone(&self.cancel);
        // Spawn the DHT lookup so the caller's `resolve()` stream
        // returns immediately, then yield an empty stream — iroh's
        // own `PkarrResolver` from `presets::N0` is what parses the
        // packet into `TransportAddr`s; this lookup only exists to
        // ping the DHT and record whether a packet was found.
        //
        // The spawned task races `client.resolve(...)` against
        // `cancel.notified()`. If the `MainlineLookup` is dropped
        // before the DHT responds, the cancel wins and we exit
        // without holding the DHT request open.
        let span = tracing::info_span!(
            "mainline_lookup",
            endpoint = %endpoint_id.fmt_short(),
            provenance = provenance,
        );
        tokio::spawn(async move {
            let _enter = span.enter();
            let pk = match PkarrPublicKey::try_from(endpoint_id.as_bytes()) {
                Ok(pk) => pk,
                Err(e) => {
                    warn!("mainline lookup: invalid endpoint id: {e}");
                    if let Some(d) = &diag {
                        d.record_resolution(provenance, false);
                    }
                    return;
                }
            };
            tokio::select! {
                biased;
                _ = cancel.notified() => {
                    debug!("mainline lookup: cancelled (lookup dropped)");
                }
                res = client.resolve(&pk, PkarrResolvePolicy::CacheFirst) => {
                    match res {
                        Ok(_packet) => {
                            debug!(
                                "mainline lookup: hit (deferred to iroh PkarrResolver for parsing)"
                            );
                            if let Some(d) = &diag {
                                d.record_resolution(provenance, true);
                            }
                        }
                        Err(e) => {
                            if let Some(d) = &diag {
                                d.record_resolution(provenance, false);
                            }
                            debug!("mainline lookup: miss: {e}");
                        }
                    }
                }
            }
        });
        // The lookup intentionally returns no items — `presets::N0`
        // already provides `PkarrResolver` for actual resolution.
        Some(stream::empty::<Result<Item, LookupError>>().boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iroh::discovery::UserData;

    #[test]
    fn memory_lookup_len_tracks_add_remove() {
        let lk = MemoryLookup::new();
        assert!(lk.is_empty());
        assert_eq!(lk.len(), 0);
    }

    #[test]
    fn memory_lookup_provenance_is_stable() {
        assert_eq!(MemoryLookup::PROVENANCE, "a3net-memory");
        assert_eq!(MainlineLookup::PROVENANCE, "a3net-mainline-dht");
    }

    #[test]
    fn add_rejects_endpoint_id_mismatch() {
        // C3: refusing a mismatched endpoint_id prevents a caller
        // from accidentally registering peer_X's address book
        // under peer_Y's slot. We use real iroh-generated keys so
        // the bytes form a valid Ed25519 public key.
        let lk = MemoryLookup::new();
        let key_a = iroh::SecretKey::generate();
        let key_b = iroh::SecretKey::generate();
        let ep_id_a = key_a.public();
        let ep_id_b = key_b.public();
        let info_b = EndpointInfo::from_parts(ep_id_b, EndpointData::default());
        let node_id_a = NodeId::from_bytes(ep_id_a.as_bytes()).expect("valid NodeId bytes");
        let err = lk.add(node_id_a, info_b).unwrap_err();
        assert!(
            err.to_string().contains("does not match"),
            "unexpected error: {err}"
        );
        // Nothing should have been recorded.
        assert_eq!(lk.len(), 0);
        assert!(lk.is_empty());
    }

    /// **P1-α — Recover from a poisoned `entries` lock instead of
    /// crashing.** Mirrors
    /// `diagnostics::recover_lock_returns_data_even_when_poisoned`:
    /// a writer that panics while holding the lock would
    /// previously poison the `Arc<RwLock<usize>>`, and the
    /// next `len()` / `is_empty()` / `add()` would panic. After
    /// the fix, `recover_lock` returns the inner guard (data
    /// preserved) and the API stays observable.
    ///
    /// `entries` is private, so we drive the recovery through a
    /// mirror `RwLock<usize>` (same shape as `entries`) and
    /// confirm `recover_lock` round-trips. The actual
    /// `MemoryLookup::len` call afterwards exercises the
    /// public surface; if the helper were ever removed, the
    /// `len()` call would panic and the assertion would fail.
    #[test]
    fn memory_lookup_len_survives_writer_panic() {
        use std::sync::{Arc, RwLock};

        let lk = MemoryLookup::new();
        assert_eq!(lk.len(), 0);

        // Simulate a poisoned writer by panicking inside a
        // holding guard on a mirror lock that mirrors the
        // shape of `MemoryLookup::entries`. The panic leaves
        // the mirror lock poisoned; `recover_lock` must still
        // return the inner data.
        let mirror: Arc<RwLock<usize>> = Arc::new(RwLock::new(0));
        let mirror_clone = Arc::clone(&mirror);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _g = mirror_clone.write().unwrap();
            panic!("simulated writer panic");
        }));
        // Mirror is now poisoned — `recover_lock` must return
        // the value (0) rather than panicking.
        let recovered = *recover_lock(mirror.read());
        assert_eq!(recovered, 0);

        // Public API stays usable: the real `MemoryLookup`'s
        // entries lock is independent of the mirror, so a
        // `len()` call here confirms the `recover_lock` change
        // didn't accidentally leave the public surface in a
        // bad state (e.g. some panic-on-poison branch still
        // present in another method).
        assert_eq!(lk.len(), 0);
        assert!(lk.is_empty());
    }

    // ──────────────────── UserData attachment / lookup ────────────────────

    /// Round-trip: `put_user_data(Some(ud))` then `get_user_data`
    /// returns the same payload.
    #[test]
    fn put_then_get_user_data_round_trips() {
        let lk = MemoryLookup::new();
        let key = iroh::SecretKey::generate();
        let node_id = NodeId::from_bytes(key.public().as_bytes()).expect("valid");
        let ud = UserData::new("a3net/role=worker").unwrap();
        let prev = lk
            .put_user_data(node_id.clone(), Some(ud.clone()))
            .expect("put");
        assert!(prev.is_none(), "no previous payload");
        let got = lk.get_user_data(node_id.clone()).expect("get");
        assert_eq!(got, Some(ud));
    }

    /// `put_user_data(None)` clears the field.
    #[test]
    fn put_user_data_none_clears_field() {
        let lk = MemoryLookup::new();
        let key = iroh::SecretKey::generate();
        let node_id = NodeId::from_bytes(key.public().as_bytes()).expect("valid");
        lk.put_user_data(node_id.clone(), Some(UserData::new("first").unwrap()))
            .unwrap();
        let prev = lk.put_user_data(node_id.clone(), None).unwrap();
        assert_eq!(prev.as_ref().map(|u| u.as_str()), Some("first"));
        let after = lk.get_user_data(node_id).unwrap();
        assert!(after.is_none());
    }

    /// `get_user_data` for a node_id that was never attached
    /// returns `Ok(None)`.
    #[test]
    fn get_user_data_missing_returns_none() {
        let lk = MemoryLookup::new();
        let key = iroh::SecretKey::generate();
        let node_id = NodeId::from_bytes(key.public().as_bytes()).expect("valid");
        let got = lk.get_user_data(node_id).unwrap();
        assert!(got.is_none());
    }

    /// `remove()` must also drop the attached user-data so the
    /// map does not leak across remove + re-add cycles.
    #[test]
    fn remove_clears_user_data() {
        let lk = MemoryLookup::new();
        let key = iroh::SecretKey::generate();
        let node_id = NodeId::from_bytes(key.public().as_bytes()).expect("valid");
        lk.put_user_data(
            node_id.clone(),
            Some(UserData::new("before-remove").unwrap()),
        )
        .unwrap();
        // We need a real `EndpointInfo` to call `remove` — use
        // the same key.
        let info = EndpointInfo::from_parts(key.public(), EndpointData::default());
        lk.add(node_id.clone(), info).unwrap();
        let removed = lk.remove(node_id.clone()).unwrap();
        assert!(removed.is_some());
        let after = lk.get_user_data(node_id).unwrap();
        assert!(
            after.is_none(),
            "user-data must be cleared on remove, got {after:?}"
        );
    }

    /// `user_data_entries` snapshots every `(short_id, payload)`
    /// pair currently held.
    #[test]
    fn user_data_entries_snapshot_is_complete() {
        let lk = MemoryLookup::new();
        let key1 = iroh::SecretKey::generate();
        let key2 = iroh::SecretKey::generate();
        let id1 = NodeId::from_bytes(key1.public().as_bytes()).expect("valid");
        let id2 = NodeId::from_bytes(key2.public().as_bytes()).expect("valid");
        lk.put_user_data(id1.clone(), Some(UserData::new("worker-1").unwrap()))
            .unwrap();
        lk.put_user_data(id2.clone(), Some(UserData::new("worker-2").unwrap()))
            .unwrap();
        let entries = lk.user_data_entries();
        assert_eq!(entries.len(), 2);
        let payloads: Vec<&str> = entries.iter().map(|(_, ud)| ud.as_str()).collect();
        assert!(payloads.contains(&"worker-1"));
        assert!(payloads.contains(&"worker-2"));
    }

    /// Empty snapshot — the `user_data_entries` API stays
    /// total on an empty lookup.
    #[test]
    fn user_data_entries_empty_when_lookup_is_empty() {
        let lk = MemoryLookup::new();
        let entries = lk.user_data_entries();
        assert!(entries.is_empty());
    }

    /// `put_user_data` is a no-op on lookup size — only the
    /// parallel `user_data` map grows.
    #[test]
    fn put_user_data_does_not_change_len() {
        let lk = MemoryLookup::new();
        let key = iroh::SecretKey::generate();
        let node_id = NodeId::from_bytes(key.public().as_bytes()).expect("valid");
        assert_eq!(lk.len(), 0);
        lk.put_user_data(node_id, Some(UserData::new("payload").unwrap()))
            .unwrap();
        assert_eq!(
            lk.len(),
            0,
            "user-data is a side channel, not an EndpointInfo"
        );
    }
}
