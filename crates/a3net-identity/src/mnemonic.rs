//! BIP-39 mnemonic phrases.
//!
//! Wraps the `bip39` crate so the rest of A3Net sees a typed
//! [`Mnemonic`] that we own. The mnemonic is the *root seed source*;
//! callers always go through [`Mnemonic::to_seed`] which returns a
//! `Zeroizing<[u8; 64]>` so the seed bytes are wiped on drop.
//!
//! Word-list: English. Strength: 128 / 160 / 192 / 224 / 256 bits
//! (12 / 15 / 18 / 21 / 24 words) per the BIP-39 spec.
//!
//! ## No HD here
//!
//! Mnemonic → seed → BIP-32 derivation lives in [`crate::hd`]. This
//! module only owns the word-list / entropy round-trip.

use std::fmt;

use zeroize::Zeroizing;

use crate::error::{IdentityError, Result};

/// Mnemonic word-list sizes supported by A3Net.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MnemonicStrength {
    /// 128 bits of entropy — 12 words.
    Bits128,
    /// 160 bits — 15 words.
    Bits160,
    /// 192 bits — 18 words.
    Bits192,
    /// 224 bits — 21 words.
    Bits224,
    /// 256 bits — 24 words.
    Bits256,
}

impl MnemonicStrength {
    /// Entropy bytes (matches BIP-39 §Entropy table).
    pub fn entropy_bytes(self) -> usize {
        match self {
            Self::Bits128 => 16,
            Self::Bits160 => 20,
            Self::Bits192 => 24,
            Self::Bits224 => 28,
            Self::Bits256 => 32,
        }
    }

    /// Word-list length (12/15/18/21/24).
    pub fn words(self) -> usize {
        match self {
            Self::Bits128 => 12,
            Self::Bits160 => 15,
            Self::Bits192 => 18,
            Self::Bits224 => 21,
            Self::Bits256 => 24,
        }
    }
}

/// A BIP-39 mnemonic phrase.
#[derive(Clone)]
pub struct Mnemonic {
    inner: bip39::Mnemonic,
    phrase: String,
}

impl Mnemonic {
    /// Generate a fresh mnemonic with the given strength. Uses the OS
    /// CSPRNG (`getrandom`).
    pub fn generate(strength: MnemonicStrength) -> Result<Self> {
        let inner = bip39::Mnemonic::generate_in(
            bip39::Language::English,
            strength.words(),
        )
        .map_err(|e| IdentityError::Mnemonic(e.to_string()))?;
        let phrase = inner.to_string();
        Ok(Self { inner, phrase })
    }

    /// Parse a user-supplied phrase. Accepts the 12/15/18/21/24-word
    /// English word-list with any whitespace separator.
    pub fn from_phrase(phrase: &str) -> Result<Self> {
        let inner = bip39::Mnemonic::parse_in(bip39::Language::English, phrase)
            .map_err(|e| IdentityError::Mnemonic(e.to_string()))?;
        let phrase = inner.to_string();
        Ok(Self { inner, phrase })
    }

    /// Deterministic seed: `pbkdf2_hmac_sha512(mnemonic_phrase, "mnemonic" + passphrase)`.
    /// Returns a 64-byte seed wrapped in `Zeroizing` so it is wiped on
    /// drop.
    pub fn to_seed(&self, passphrase: &str) -> Zeroizing<[u8; 64]> {
        // `to_seed` from the bip39 crate returns a `[u8; 64]`. We wrap it
        // in `Zeroizing` so the caller's copy disappears when it goes
        // out of scope (or panics).
        let mut seed = Zeroizing::new([0u8; 64]);
        let raw = self.inner.to_seed(passphrase);
        seed.copy_from_slice(&raw);
        seed
    }

    /// Space-separated lowercase phrase.
    pub fn phrase(&self) -> &str {
        &self.phrase
    }
}

impl fmt::Debug for Mnemonic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the phrase or entropy in `Debug` — they're
        // bearer secrets.
        f.debug_struct("Mnemonic")
            .field("words", &self.phrase.split_whitespace().count())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_12_words() {
        let m = Mnemonic::generate(MnemonicStrength::Bits128).unwrap();
        assert_eq!(m.phrase().split_whitespace().count(), 12);
    }

    #[test]
    fn generate_24_words() {
        let m = Mnemonic::generate(MnemonicStrength::Bits256).unwrap();
        assert_eq!(m.phrase().split_whitespace().count(), 24);
    }

    #[test]
    fn from_phrase_round_trip() {
        // Canonical BIP-39 test vector (trezor).
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let m = Mnemonic::from_phrase(phrase).unwrap();
        assert_eq!(m.phrase(), phrase);
    }

    #[test]
    fn rejects_unknown_word() {
        let bad = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon zzzzz";
        assert!(Mnemonic::from_phrase(bad).is_err());
    }

    #[test]
    fn rejects_bad_checksum() {
        // Swap the last word for one with a wrong checksum.
        let bad = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon";
        assert!(Mnemonic::from_phrase(bad).is_err());
    }

    #[test]
    fn seed_is_deterministic() {
        let m = Mnemonic::from_phrase(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .unwrap();
        let s1 = m.to_seed("");
        let s2 = m.to_seed("");
        assert_eq!(*s1, *s2);
        // Different passphrase → different seed.
        let s3 = m.to_seed("hunter2");
        assert_ne!(*s1, *s3);
    }

    #[test]
    fn debug_does_not_leak_phrase() {
        let m = Mnemonic::generate(MnemonicStrength::Bits128).unwrap();
        let dbg = format!("{m:?}");
        for w in m.phrase().split_whitespace() {
            assert!(!dbg.contains(w), "Debug leaked word: {w}");
        }
    }
}