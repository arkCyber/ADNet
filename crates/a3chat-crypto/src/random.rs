//! Secure-random helpers. Wraps `rand::thread_rng` so callers don't
//! have to thread a `Rng` through every call site.
//!
//! Both functions panic if the OS RNG is unreachable — that is the
//! correct behaviour for cryptographic code: failing loudly is much
//! better than returning deterministic bytes.

use rand::RngCore;

/// Generate `n` cryptographically-secure random bytes using the OS
/// RNG (via `rand::thread_rng` → `getrandom`).
pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut out = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut out);
    out
}

/// Generate a 12-byte ChaCha20-Poly1305 nonce (returned as raw bytes;
/// `MessageBody` expects hex).
pub fn random_nonce() -> [u8; 12] {
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    nonce
}

/// Generate a 32-byte ChaCha20-Poly1305 symmetric key. Used when
/// bootstrapping a session before Noise has produced the first
/// shared secret.
pub fn random_key_32() -> [u8; 32] {
    let mut k = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut k);
    k
}

/// Generate a 16-byte Argon2id salt.
pub fn random_salt_16() -> [u8; 16] {
    let mut s = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut s);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_bytes_has_requested_len() {
        assert_eq!(random_bytes(0).len(), 0);
        assert_eq!(random_bytes(7).len(), 7);
        assert_eq!(random_bytes(4096).len(), 4096);
    }

    #[test]
    fn random_bytes_are_distinct_across_calls() {
        let a = random_bytes(32);
        let b = random_bytes(32);
        assert_ne!(a, b, "two consecutive random buffers collided");
    }

    #[test]
    fn random_nonce_has_12_bytes() {
        assert_eq!(random_nonce().len(), 12);
    }

    #[test]
    fn random_key_32_has_32_bytes() {
        assert_eq!(random_key_32().len(), 32);
    }

    #[test]
    fn random_salt_16_has_16_bytes() {
        assert_eq!(random_salt_16().len(), 16);
    }
}
