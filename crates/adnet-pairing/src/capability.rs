//! Capability grants — what a paired device is allowed to do.
//!
//! Every [`crate::wire::PairingInvitation`] lists the capabilities
//! the issuer is willing to grant. Every [`crate::trusted_device::TrustedDeviceRecord`]
//! in the store records what was actually granted. At handshake
//! time the transport checks `CapabilitySet::contains` against each
//! incoming request.
//!
//! The set is a small `u32` bitfield so it can travel in JSON, in a
//! QR code, and in on-disk records without ceremony. Adding a new
//! capability is a one-line change; remove one and every older
//! grant becomes meaningless (the verifier will reject it).
//!
//! Capabilities deliberately do **not** include "admin" — admin
//! rights (revoke, rotate, change capabilities of others) live on
//! the wallet that owns the [`crate::invitation::SignedInvitation`],
//! not on the transport peer. Lost-device revocation is therefore a
//! wallet-level action: the human takes a new QR from their wallet
//! and re-pairs, while the store is told to drop the old record.

use serde::{Deserialize, Serialize};

/// A capability is a stable 16-bit tag. The wire format encodes it
/// as a `u16` in JSON so the on-disk shape doesn't change when a
/// new tag is added (an older reader will simply reject an unknown
/// tag, which is the correct behaviour).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Capability(pub u16);

impl Capability {
    // Stable IDs. Add new ones at the bottom of this list; never
    // renumber an existing one. `pub` because downstream crates
    // (CLI, IPC) want to reference them by name.
    pub const CHAT: Self = Self(0x0001);
    pub const FILES_READ: Self = Self(0x0010);
    pub const FILES_WRITE: Self = Self(0x0011);
    pub const SYNC: Self = Self(0x0020);
    pub const PRESENCE: Self = Self(0x0030);
    pub const GOSSIP_PUBLISH: Self = Self(0x0040);
    pub const DOCS_READ: Self = Self(0x0050);
    pub const DOCS_WRITE: Self = Self(0x0051);

    /// Sentinel covering every capability known to this build. Used
    /// by tests / dev tooling — never grant it to a real device.
    pub const ALL_KNOWN: Self = Self(0x00FF);

    pub fn name(self) -> &'static str {
        match self {
            Self::CHAT => "chat",
            Self::FILES_READ => "files.read",
            Self::FILES_WRITE => "files.write",
            Self::SYNC => "sync",
            Self::PRESENCE => "presence",
            Self::GOSSIP_PUBLISH => "gossip.publish",
            Self::DOCS_READ => "docs.read",
            Self::DOCS_WRITE => "docs.write",
            Self::ALL_KNOWN => "all-known",
            _ => "unknown",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "chat" => Self::CHAT,
            "files.read" => Self::FILES_READ,
            "files.write" => Self::FILES_WRITE,
            "sync" => Self::SYNC,
            "presence" => Self::PRESENCE,
            "gossip.publish" => Self::GOSSIP_PUBLISH,
            "docs.read" => Self::DOCS_READ,
            "docs.write" => Self::DOCS_WRITE,
            _ => return None,
        })
    }

    /// Return `true` if this capability is recognised by the current
    /// build. Used by verifiers to refuse future-only grants.
    pub fn is_known(self) -> bool {
        self.name() != "unknown"
    }
}

/// Compact set of capability flags. Stored as a sorted
/// `Vec<Capability>` so JSON output is stable and human-readable.
/// [`CapabilitySet::bitmask`] lets us do fast intersection checks
/// without iterating.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapabilitySet {
    caps: Vec<Capability>,
}

impl std::fmt::Debug for CapabilitySet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Print each capability as its canonical name so logs and error
        // messages are immediately meaningful (e.g. `["chat", "files.read"]`
        // rather than `caps: [1, 16]`).
        f.debug_list()
            .entries(self.caps.iter().map(|c| c.name()))
            .finish()
    }
}

impl CapabilitySet {
    pub const fn empty() -> Self {
        Self { caps: Vec::new() }
    }

    /// Construct a [`CapabilitySet`] from an iterator of names.
    pub fn from_names<I: IntoIterator<Item = &'static str>>(it: I) -> Self {
        Self::from_iter(it.into_iter().filter_map(Capability::from_name))
    }

    /// Construct a [`CapabilitySet`] from an iterator of capability constants.
    #[allow(clippy::should_implement_trait)]
    pub fn from_iter<I: IntoIterator<Item = Capability>>(it: I) -> Self {
        let mut caps: Vec<Capability> = it.into_iter().collect();
        caps.sort_unstable();
        caps.dedup();
        Self { caps }
    }

    pub fn insert(&mut self, cap: Capability) {
        if let Err(idx) = self.caps.binary_search(&cap) {
            self.caps.insert(idx, cap);
        }
    }

    pub fn remove(&mut self, cap: Capability) -> bool {
        if let Ok(idx) = self.caps.binary_search(&cap) {
            self.caps.remove(idx);
            true
        } else {
            false
        }
    }

    pub fn contains(&self, cap: Capability) -> bool {
        self.caps.binary_search(&cap).is_ok()
    }

    pub fn intersects(&self, other: &Self) -> bool {
        let (a, b) = if self.caps.len() <= other.caps.len() {
            (&self.caps, &other.caps)
        } else {
            (&other.caps, &self.caps)
        };
        for cap in a {
            if b.binary_search(cap).is_ok() {
                return true;
            }
        }
        false
    }

    pub fn is_empty(&self) -> bool {
        self.caps.is_empty()
    }

    pub fn len(&self) -> usize {
        self.caps.len()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Capability> {
        self.caps.iter()
    }

    /// Return a `u64` bitmask covering the lower 64 capabilities.
    /// Used by the transport handshake's fast-path check — callers
    /// must still call `contains` for capabilities beyond bit 63.
    pub fn bitmask(&self) -> u64 {
        let mut mask = 0u64;
        for cap in &self.caps {
            if (cap.0 as u32) < 64 {
                mask |= 1u64 << (cap.0 as u32);
            }
        }
        mask
    }

    /// Stable canonical form for signing. The output is
    /// `<bit>:hex(u64 mask),<bit>:hex(u64 mask)`, hex of the
    /// `bitmask` of each capability, sorted lexicographically.
    pub fn canonical(&self) -> String {
        let mask = self.bitmask();
        format!("caps:{:016x}", mask)
    }
}

impl Default for CapabilitySet {
    fn default() -> Self {
        Self::empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for &c in &[
            Capability::CHAT,
            Capability::FILES_READ,
            Capability::FILES_WRITE,
            Capability::SYNC,
        ] {
            assert_eq!(Capability::from_name(c.name()), Some(c));
        }
    }

    #[test]
    fn set_contains_and_intersects() {
        let a = CapabilitySet::from_names(["chat", "files.read", "sync"]);
        let b = CapabilitySet::from_names(["chat", "files.write"]);
        assert!(a.contains(Capability::CHAT));
        assert!(!a.contains(Capability::FILES_WRITE));
        assert!(a.intersects(&b)); // chat is shared
        let c = CapabilitySet::from_names(["files.write"]);
        assert!(!a.intersects(&c));
    }

    #[test]
    fn dedup_is_stable() {
        let s = CapabilitySet::from_iter([
            Capability::CHAT,
            Capability::CHAT,
            Capability::FILES_READ,
            Capability::CHAT,
        ]);
        assert_eq!(s.len(), 2);
        let v: Vec<_> = s.iter().copied().collect();
        assert_eq!(v, vec![Capability::CHAT, Capability::FILES_READ]);
    }

    #[test]
    fn bitmask_round_trip() {
        // CHAT = 0x0001 → bit 1 (LSB is bit 0)
        // FILES_READ = 0x0010 → bit 16
        let s = CapabilitySet::from_names(["chat", "files.read"]);
        assert_ne!(s.bitmask() & (1 << 1), 0, "CHAT bit must be set");
        assert_ne!(s.bitmask() & (1u64 << 16), 0, "FILES_READ bit must be set");
        assert_eq!(
            s.bitmask() & (1 << 0),
            0,
            "bit 0 must not be set (cap 0 reserved)"
        );
    }

    #[test]
    fn unknown_capability_rejected() {
        let s = CapabilitySet::from_iter([Capability(0xABCD)]);
        assert!(!Capability(0xABCD).is_known());
        assert!(!s.contains(Capability::CHAT));
    }

    #[test]
    fn debug_shows_names_not_numbers() {
        let s = CapabilitySet::from_names(["chat", "files.read"]);
        let debug = format!("{:?}", s);
        assert!(debug.contains("\"chat\""), "debug must show name: {debug}");
        assert!(
            debug.contains("files.read"),
            "debug must show name: {debug}"
        );
        assert!(
            !debug.contains("1"),
            "debug must NOT show bit index: {debug}"
        );
    }
}
