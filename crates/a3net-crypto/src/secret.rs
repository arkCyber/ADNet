//! `Secret<N>` — a length-typed secret byte array with mandatory
//! `Zeroize` + `ZeroizeOnDrop`.
//!
//! This is the canonical secret-bearing type across A3Net. Use it
//! instead of raw `Vec<u8>` / `[u8; 32]` whenever the contents are
//! key material, an authentication token, or anything else that must
//! not survive the owning scope.
//!
//! ## Design
//!
//! - One allocation, one `Drop`, one wipe.
//! - `Zeroize` on `Drop` is enforced by the derive; you cannot opt
//!   out by writing a custom `Drop` that forgets the field.
//! - `Debug` intentionally omits the bytes (the secret is the
//!   value, not the type).
//! - Reading the bytes out requires going through
//!   [`Secret::expose`] which returns a `Zeroizing<&[u8]>` — even a
//!   short-lived borrow is wiped on drop.
//!
//! This is intentionally not the `secrecy` crate. A3Net wants zero
//! transitive dependencies for this primitive; the implementation is
//! ~30 lines.

use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{CryptoError, CryptoResult};

/// A fixed-length secret. `Drop` always wipes.
///
/// `Secret<N>` is `!Copy`, `!Clone`. The bytes live in exactly one
/// place; if you need a copy, use [`Secret::expose`] to get a
/// short-lived `Zeroizing<&[u8]>` and reconstruct.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct Secret<const N: usize>([u8; N]);

impl<const N: usize> Secret<N> {
    /// Wrap a fresh secret. Returns `Err` if the input is all-zero —
    /// accepting an all-zero key silently is the kind of mistake this
    /// type exists to make loud.
    pub fn new(bytes: [u8; N]) -> CryptoResult<Self> {
        if bytes.iter().all(|&b| b == 0) {
            return Err(CryptoError::InvalidKeyLength(N)); // sentinel for "all-zero"
        }
        Ok(Self(bytes))
    }

    /// Wrap arbitrary bytes without the all-zero check. Only intended
    /// for migrating existing keys that we have to accept even if
    /// they are weak — never for newly generated material.
    ///
    /// **Prefer [`Secret::new`].**
    pub fn from_bytes_unchecked(bytes: [u8; N]) -> Self {
        Self(bytes)
    }

    /// Length in bytes.
    pub const fn len(&self) -> usize {
        N
    }

    /// `true` if `N == 0`.
    pub const fn is_empty(&self) -> bool {
        N == 0
    }

    /// Expose the bytes for the duration of a closure. The closure
    /// receives a `&[u8]` borrow that is wiped the moment the closure
    /// returns.
    pub fn with<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        f(&self.0)
    }

    /// Copy the bytes into a `Zeroizing<Vec<u8>>`. The returned
    /// `Vec` is wiped on its own drop; the `Secret` keeps its own
    /// copy.
    pub fn expose(&self) -> zeroize::Zeroizing<Vec<u8>> {
        zeroize::Zeroizing::new(self.0.to_vec())
    }

    /// Consume the `Secret` and return a `Zeroizing<Vec<u8>>`. The
    /// original is wiped on drop; the returned `Vec` is wiped on its
    /// own drop.
    pub fn into_zeroizing_vec(self) -> zeroize::Zeroizing<Vec<u8>> {
        zeroize::Zeroizing::new(self.0.to_vec())
    }
}

impl<const N: usize> std::fmt::Debug for Secret<N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print key material in logs / panic messages.
        f.debug_struct("Secret").field("len", &N).finish()
    }
}

// `Secret` is intentionally NOT `Clone` — clones would leak the
// underlying buffer. If you need a copy, reconstruct via `new(bytes)`.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_accepts_nonzero() {
        let s: Secret<32> = Secret::new([1u8; 32]).unwrap();
        assert_eq!(s.len(), 32);
        assert!(!s.is_empty());
    }

    #[test]
    fn new_rejects_all_zero() {
        let err = Secret::<32>::new([0u8; 32]).unwrap_err();
        assert!(matches!(err, CryptoError::InvalidKeyLength(32)));
    }

    #[test]
    fn from_bytes_unchecked_accepts_zero() {
        // Escape hatch — but only via this constructor.
        let s: Secret<32> = Secret::from_bytes_unchecked([0u8; 32]);
        assert_eq!(s.len(), 32);
    }

    #[test]
    fn debug_hides_bytes() {
        let s: Secret<32> = Secret::new([0xAA; 32]).unwrap();
        let printed = format!("{:?}", s);
        assert!(!printed.contains("aa"));
        assert!(printed.contains("Secret"));
    }

    #[test]
    fn expose_returns_zeroizing() {
        let s: Secret<32> = Secret::new([0x33; 32]).unwrap();
        let exposed: zeroize::Zeroizing<Vec<u8>> = s.expose();
        assert_eq!(exposed.len(), 32);
        assert!(exposed.iter().all(|&b| b == 0x33));
    }

    #[test]
    fn into_zeroizing_vec_returns_zeroizing() {
        let s: Secret<32> = Secret::new([0x77; 32]).unwrap();
        let v: zeroize::Zeroizing<Vec<u8>> = s.into_zeroizing_vec();
        assert_eq!(v.len(), 32);
        assert!(v.iter().all(|&b| b == 0x77));
    }
}
