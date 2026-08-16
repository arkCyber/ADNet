//! Ed25519 signing / verification for [`PeeringGrant`] envelopes
//! (RFC-0007 §5.2).
//!
//! The signature scheme is **plain Ed25519** over the grant's
//! BLAKE3 signing preimage: no EIP-191 wrapper, no domain
//! separator beyond the preimage tail already inside
//! [`PeeringGrant::signing_preimage`]. This matches the A3Net
//! convention that the mesh identity is Ed25519-derived
//! (see [`a3net_types::mesh`]).
//!
//! ## Why not EIP-191 over secp256k1?
//!
//! `a3net-identity` provides a `sign_personal` /
//! `recover_personal` pair over secp256k1, but the mesh
//! identity is documented as Ed25519-derived. Adding a
//! second scheme for the same logical key would force
//! operators to ship two keypairs and pick the right one
//! per envelope, which is an unnecessary foot-gun. Ed25519
//! is the canonical scheme for peerings.
//!
//! ## Recovery
//!
//! Ed25519 does **not** support public-key recovery — the
//! verifier must know the expected pubkey up front. The
//! expected pubkey is whatever key the source mesh's
//! coordinator advertises as its identity; the
//! [`CoordinatorPubkeyRegistry`] trait abstracts the
//! lookup so callers can back it with a roster, gossip
//! cache, or hard-coded trust store.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use a3net_types::MeshNetworkId;

use crate::error::{CoordinatorError, CoordinatorResult};
use crate::peering::PeeringGrant;

/// Lookup from source mesh to the coordinator's Ed25519
/// pubkey. The verifier refuses any grant whose `source`
/// mesh is not registered.
pub trait CoordinatorPubkeyRegistry: Send + Sync {
    fn pubkey_for(&self, network: &MeshNetworkId) -> Option<[u8; 32]>;
}

/// Ed25519 signer bound to a coordinator. Cheap to clone
/// (the inner `SigningKey` is 32 bytes).
#[derive(Debug, Clone)]
pub struct PeeringGrantSigner {
    signing_key: SigningKey,
}

impl PeeringGrantSigner {
    /// Build from a 32-byte Ed25519 secret key.
    pub fn from_bytes(secret: &[u8; 32]) -> CoordinatorResult<Self> {
        let key = SigningKey::from_bytes(secret);
        Ok(Self { signing_key: key })
    }

    /// Generate a fresh random signing key.
    pub fn generate() -> Self {
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        Self {
            signing_key: SigningKey::from_bytes(&bytes),
        }
    }

    /// The 32-byte public key (verified output, not a
    /// secret).
    pub fn public_key(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    /// Sign a grant. Returns the input with `signature`
    /// populated (hex-encoded 64-byte Ed25519 signature).
    pub fn sign(&self, mut grant: PeeringGrant) -> CoordinatorResult<PeeringGrant> {
        let digest = grant.signing_digest();
        let sig = self.signing_key.sign(&digest);
        grant.signature = hex::encode(sig.to_bytes());
        Ok(grant)
    }
}

/// Stateless verifier.
#[derive(Debug, Default, Clone, Copy)]
pub struct PeeringGrantVerifier;

impl PeeringGrantVerifier {
    pub fn new() -> Self {
        Self
    }

    /// Verify `grant.signature` against the pubkey
    /// returned by `registry` for `grant.source`. Returns
    /// `Ok(())` on success, an `Err` describing the
    /// failure on mismatch.
    pub fn verify<R: CoordinatorPubkeyRegistry>(
        &self,
        grant: &PeeringGrant,
        registry: &R,
        now: chrono::DateTime<chrono::Utc>,
    ) -> CoordinatorResult<()> {
        // 1. Cheap syntactic checks first.
        if grant.source == grant.target {
            return Err(CoordinatorError::PeeringSelfLoop);
        }
        if grant.signature.is_empty() {
            return Err(CoordinatorError::PeeringSignatureInvalid(
                "empty signature".into(),
            ));
        }
        if grant.is_expired(now) {
            return Err(CoordinatorError::PeeringExpired {
                grant_id: grant.grant_id.to_string(),
                valid_until: grant.valid_until,
            });
        }

        // 2. Decode the signature.
        let sig_bytes = hex::decode(&grant.signature).map_err(|e| {
            CoordinatorError::PeeringSignatureInvalid(format!("hex decode: {e}"))
        })?;
        if sig_bytes.len() != 64 {
            return Err(CoordinatorError::PeeringSignatureInvalid(format!(
                "expected 64 bytes, got {}",
                sig_bytes.len()
            )));
        }
        let sig_arr: [u8; 64] = sig_bytes
            .as_slice()
            .try_into()
            .expect("length checked above");
        let sig = Signature::from_bytes(&sig_arr);

        // 3. Look up the expected pubkey.
        let expected_pubkey = registry.pubkey_for(&grant.source).ok_or_else(|| {
            CoordinatorError::PeeringUnknownCoordinator(grant.source.to_string())
        })?;
        let verifying_key = VerifyingKey::from_bytes(&expected_pubkey).map_err(|e| {
            CoordinatorError::PeeringSignatureInvalid(format!("malformed pubkey: {e}"))
        })?;

        // 4. Verify.
        let digest = grant.signing_digest();
        verifying_key.verify(&digest, &sig).map_err(|e| {
            CoordinatorError::PeeringSignatureInvalid(format!("ed25519 verify: {e}"))
        })?;

        Ok(())
    }
}

/// A trivial in-memory registry. Useful for tests and
/// for the bootstrap path before the gossip-fed roster
/// is populated.
#[derive(Debug, Clone, Default)]
pub struct StaticPubkeyRegistry {
    inner: std::collections::HashMap<MeshNetworkId, [u8; 32]>,
}

impl StaticPubkeyRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a (network, pubkey) pair. Re-registering
    /// the same network overwrites the previous key.
    pub fn register(
        &mut self,
        network: MeshNetworkId,
        pubkey: [u8; 32],
    ) {
        self.inner.insert(network, pubkey);
    }
}

impl CoordinatorPubkeyRegistry for StaticPubkeyRegistry {
    fn pubkey_for(&self, network: &MeshNetworkId) -> Option<[u8; 32]> {
        self.inner.get(network).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peering::PeeringGrant;
    use a3net_types::NodeId;
    use chrono::Utc;
    use std::time::Duration;

    fn nid(seed: u8) -> MeshNetworkId {
        MeshNetworkId::from_bytes(&[seed; 32]).unwrap()
    }

    fn node(seed: u8) -> NodeId {
        NodeId::from_bytes(&[seed; 32]).unwrap()
    }

    fn grant(source: u8, target: u8, grantor: u8) -> PeeringGrant {
        PeeringGrant::new_unsigned(
            nid(source),
            nid(target),
            node(grantor),
            Duration::from_secs(60),
        )
        .unwrap()
    }

    #[test]
    fn signer_exposes_public_key() {
        let s = PeeringGrantSigner::generate();
        let pk = s.public_key();
        assert_eq!(pk.len(), 32);
    }

    #[test]
    fn sign_then_verify_round_trip() {
        let signer = PeeringGrantSigner::generate();
        let pk = signer.public_key();
        let mut reg = StaticPubkeyRegistry::new();
        reg.register(nid(1), pk);

        let g = grant(1, 2, 7);
        let signed = signer.sign(g).unwrap();
        // The signature is now populated.
        assert_eq!(signed.signature.len(), 128); // 64 bytes hex
        // Verify accepts it.
        PeeringGrantVerifier::new()
            .verify(&signed, &reg, Utc::now())
            .unwrap();
    }

    #[test]
    fn verify_rejects_unknown_network() {
        let signer = PeeringGrantSigner::generate();
        let g = grant(1, 2, 7);
        let signed = signer.sign(g).unwrap();
        // Empty registry — no network registered.
        let reg = StaticPubkeyRegistry::new();
        let err = PeeringGrantVerifier::new()
            .verify(&signed, &reg, Utc::now())
            .unwrap_err();
        assert!(matches!(err, CoordinatorError::PeeringUnknownCoordinator(_)));
    }

    #[test]
    fn verify_rejects_wrong_pubkey() {
        let signer_a = PeeringGrantSigner::generate();
        let signer_b = PeeringGrantSigner::generate();
        let mut reg = StaticPubkeyRegistry::new();
        // Register A's pubkey, but sign with B.
        reg.register(nid(1), signer_a.public_key());
        let g = grant(1, 2, 7);
        let signed = signer_b.sign(g).unwrap();
        let err = PeeringGrantVerifier::new()
            .verify(&signed, &reg, Utc::now())
            .unwrap_err();
        assert!(matches!(err, CoordinatorError::PeeringSignatureInvalid(_)));
    }

    #[test]
    fn verify_rejects_truncated_signature() {
        let signer = PeeringGrantSigner::generate();
        let pk = signer.public_key();
        let mut reg = StaticPubkeyRegistry::new();
        reg.register(nid(1), pk);

        let mut g = grant(1, 2, 7);
        g.signature = "abc".into();
        let err = PeeringGrantVerifier::new()
            .verify(&g, &reg, Utc::now())
            .unwrap_err();
        assert!(matches!(err, CoordinatorError::PeeringSignatureInvalid(_)));
    }

    #[test]
    fn verify_rejects_64_byte_wrong_signature() {
        let signer = PeeringGrantSigner::generate();
        let pk = signer.public_key();
        let mut reg = StaticPubkeyRegistry::new();
        reg.register(nid(1), pk);

        // Sign with a *different* key, then try to verify
        // against the first pubkey.
        let signer2 = PeeringGrantSigner::generate();
        let g = grant(1, 2, 7);
        let signed = signer2.sign(g).unwrap();
        let err = PeeringGrantVerifier::new()
            .verify(&signed, &reg, Utc::now())
            .unwrap_err();
        assert!(matches!(err, CoordinatorError::PeeringSignatureInvalid(_)));
    }

    #[test]
    fn verify_rejects_expired_grant() {
        let signer = PeeringGrantSigner::generate();
        let pk = signer.public_key();
        let mut reg = StaticPubkeyRegistry::new();
        reg.register(nid(1), pk);

        let mut g = grant(1, 2, 7);
        let signed = signer.sign(g.clone()).unwrap();
        g.signature = signed.signature.clone();
        g.valid_until = Utc::now() - chrono::Duration::seconds(1);
        let err = PeeringGrantVerifier::new()
            .verify(&g, &reg, Utc::now())
            .unwrap_err();
        assert!(matches!(err, CoordinatorError::PeeringExpired { .. }));
    }

    #[test]
    fn verify_rejects_self_loop() {
        let signer = PeeringGrantSigner::generate();
        let pk = signer.public_key();
        let mut reg = StaticPubkeyRegistry::new();
        reg.register(nid(1), pk);

        let mut g = grant(1, 2, 7);
        // Build a self-loop by post-mutation.
        g.source = nid(2);
        g.target = nid(2);
        let signed = signer.sign(g).unwrap();
        let err = PeeringGrantVerifier::new()
            .verify(&signed, &reg, Utc::now())
            .unwrap_err();
        assert!(matches!(err, CoordinatorError::PeeringSelfLoop));
    }

    #[test]
    fn verify_rejects_empty_signature() {
        let signer = PeeringGrantSigner::generate();
        let pk = signer.public_key();
        let mut reg = StaticPubkeyRegistry::new();
        reg.register(nid(1), pk);

        let g = grant(1, 2, 7); // signature empty
        let err = PeeringGrantVerifier::new()
            .verify(&g, &reg, Utc::now())
            .unwrap_err();
        match err {
            CoordinatorError::PeeringSignatureInvalid(s) => assert!(s.contains("empty")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn sign_then_tamper_with_target_fails_verify() {
        let signer = PeeringGrantSigner::generate();
        let pk = signer.public_key();
        let mut reg = StaticPubkeyRegistry::new();
        reg.register(nid(1), pk);

        let g = grant(1, 2, 7);
        let mut signed = signer.sign(g).unwrap();
        // Mutate the target without re-signing.
        signed.target = nid(3);
        let err = PeeringGrantVerifier::new()
            .verify(&signed, &reg, Utc::now())
            .unwrap_err();
        assert!(matches!(err, CoordinatorError::PeeringSignatureInvalid(_)));
    }

    #[test]
    fn sign_then_tamper_with_signature_fails_verify() {
        let signer = PeeringGrantSigner::generate();
        let pk = signer.public_key();
        let mut reg = StaticPubkeyRegistry::new();
        reg.register(nid(1), pk);

        let g = grant(1, 2, 7);
        let mut signed = signer.sign(g).unwrap();
        // Flip a byte in the signature.
        let mut sig_bytes = hex::decode(&signed.signature).unwrap();
        sig_bytes[0] ^= 0xFF;
        signed.signature = hex::encode(sig_bytes);
        let err = PeeringGrantVerifier::new()
            .verify(&signed, &reg, Utc::now())
            .unwrap_err();
        assert!(matches!(err, CoordinatorError::PeeringSignatureInvalid(_)));
    }

    #[test]
    fn signer_from_bytes_round_trip() {
        let secret = [7u8; 32];
        let s1 = PeeringGrantSigner::from_bytes(&secret).unwrap();
        let s2 = PeeringGrantSigner::from_bytes(&secret).unwrap();
        assert_eq!(s1.public_key(), s2.public_key());
    }

    #[test]
    fn static_registry_overwrites_on_re_register() {
        let mut reg = StaticPubkeyRegistry::new();
        reg.register(nid(1), [1u8; 32]);
        reg.register(nid(1), [2u8; 32]);
        assert_eq!(reg.pubkey_for(&nid(1)), Some([2u8; 32]));
    }

    #[test]
    fn static_registry_returns_none_for_unknown() {
        let reg = StaticPubkeyRegistry::new();
        assert_eq!(reg.pubkey_for(&nid(1)), None);
    }
}
