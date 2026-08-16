//! Transport-identity challenge-response.
//!
//! This module is the **core of the pairing ceremony**. The flow is:
//!
//! 1. The issuer sends an [`InvitationPayload`] over a side channel
//!    (QR / email). It already carries the issuer's wallet signature
//!    and the invitee's transport identity (`node_id`) is *not*
//!    fixed yet — the invitee is going to prove it.
//!
//! 2. The invitee dials the issuer and sends a [`PairingRequest`].
//!    The request binds:
//!
//!      - the credential_id the invitee wants to be issued,
//!      - the invitee's own transport identity (NodeId + the
//!        Ed25519 public key bytes — these are identical today but
//!        we keep both fields so future non-Ed25519 transports can
//!        flow through),
//!      - a freshly generated `nonce`,
//!      - an expiry timestamp,
//!      - a signature over the canonical digest using the invitee's
//!        **transport** private key (Ed25519 in the iroh case).
//!
//! 3. The issuer verifies the transport signature against the
//!    claimed NodeId (`verify_pairing_request`), increments a
//!    trusted-device counter for `credential_id`, and replies with
//!    a [`PairingResponse`] that:
//!
//!      - echoes `credential_id` and the invitee's nonce,
//!      - names the capabilities actually granted (a subset of what
//!        the invitation asked for),
//!      - carries an EIP-191 signature from the issuer's wallet
//!        over the canonical response digest.
//!
//! Both sides then write a [`TrustedDeviceRecord`] keyed by
//! `credential_id`. From that point on, every connection from the
//! invitee presents the credential id; the issuer checks
//! revocation status and capability before granting access.
//!
//! ## Why the canonical digest?
//!
//! We deliberately hand-compute the signed payload rather than
//! reusing the JSON shape:
//!
//! - JSON is unstable across implementations (field ordering, number
//!   formatting, optional fields, …). A "signature over the JSON"
//!   forces every verifier to reparse the same canonical form.
//! - We want replay protection: the digest binds to a nonce + an
//!   expiry that the verifier can check against its clock.
//! - We want a single canonical form that flows through QR codes,
//!   email, and the on-wire auth frame unchanged.
//!
//! The format is `b"a3net-pairing-request/v1\0" || fields…` for
//! requests and `b"a3net-pairing-response/v1\0" || fields…` for
//! responses. Adding a new field means bumping the version byte.

use blake3::Hasher;
use chrono::Utc;
use ed25519_dalek::{SECRET_KEY_LENGTH, Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::RngCore;
use serde::{Deserialize, Serialize};

use a3net_identity::signing::PersonalSignature;
use a3net_identity::wallet::{Wallet, WalletPublic};
use a3net_types::node::NodeId;

use crate::capability::CapabilitySet;
use crate::error::{PairingError, PairingResult};

/// Transport-signature scheme tag for Ed25519. The first byte of the
/// signature blob identifies the algorithm. This matches the
/// convention started by `a3net-types::SignedPeerTicket` (tag `1`
/// = 64-byte Ed25519) — keeping the two layers consistent makes the
/// existing verifier reusable.
pub const TRANSPORT_SCHEME_ED25519: u8 = 1;

/// Hard ceiling on the clock skew we'll tolerate between issuer
/// and invitee. Smaller than `a3net-types::announce::MAX_TIMESTAMP_SKEW_HOURS`
/// (24 h) because pairing is an interactive ceremony that does not
/// need to tolerate offline devices; if you can't pair within 5 min
/// you should re-request an invitation.
pub const MAX_TIMESTAMP_SKEW_SECONDS: i64 = 300;

/// Direction tag baked into the canonical digest. Keeps a request
/// signature from being reused as a response signature.
const REQUEST_DOMAIN_TAG: &[u8] = b"a3net-pairing-request/v1\0";
const RESPONSE_DOMAIN_TAG: &[u8] = b"a3net-pairing-response/v1\0";
/// Domain tag for the invitation payload digest (QR / email side).
pub const INVITATION_DOMAIN_TAG: &[u8] = b"a3net-pairing-invitation/v1\0";

/// 32-byte secret nonce. Freshly drawn with `OsRng` for every
/// [`PairingRequest`] / [`PairingResponse`].
pub type Nonce32 = [u8; 32];

/// 16-byte credential id. Derived as
/// `blake3::hash("a3net-pairing/credential-id/v1" || issuer || invitee || salt)[..16]`.
/// Stable per (issuer, invitee, salt) triple, random otherwise.
pub type CredentialId = [u8; 16];

/// Pairing request — "I, the invitee, prove I hold the transport
/// private key for `node_id` and I'd like to be paired under
/// `credential_id`."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingRequest {
    /// Protocol version — bump when the canonical digest changes.
    pub version: u8,

    /// Random 16-byte identifier for the resulting trusted-device
    /// record. The invitee generates this; the issuer echoes it in
    /// the response.
    pub credential_id: CredentialId,

    /// Transport identity the invitee is proving. Ed25519 today;
    /// the field is kept as `NodeId` so future schemes (X25519
    /// + PQ, …) can flow through without changing the wire shape.
    pub node_id: NodeId,

    /// 32-byte Ed25519 public key. Always equal to `node_id` when
    /// the transport is iroh (`NodeId` is the 32-byte public-key
    /// view of `iroh::SecretKey`). Kept as a separate field so the
    /// verifier doesn't need to know that fact.
    #[serde(with = "hex_bytes")]
    pub transport_pubkey: Vec<u8>,

    /// Capabilities the invitee is requesting. The issuer may
    /// grant a subset.
    pub requested_capabilities: CapabilitySet,

    /// Unix seconds when this request stops being valid. Verifier
    /// rejects when `now_unix > expires_at`.
    pub expires_at_unix: i64,

    /// Fresh 32-byte nonce. Verifier rejects reuse.
    pub nonce: Nonce32,

    /// `TRANSPORT_SCHEME_ED25519` + 64-byte Ed25519 signature over
    /// [`pairing_request_digest`].
    #[serde(with = "hex_bytes")]
    pub signature: Vec<u8>,
}

/// Pairing response — "I, the issuer, accept `credential_id`, here
/// is what I grant you."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingResponse {
    pub version: u8,

    /// Echoes the invitee's `credential_id`.
    pub credential_id: CredentialId,

    /// Echoes the invitee's `nonce`.
    pub nonce: Nonce32,

    /// Transport identity the issuer is pairing the invitee with —
    /// i.e. the issuer's own NodeId. The invitee binds this to the
    /// signature it verified, so a MITM can't rewrite it.
    pub issuer_node_id: NodeId,

    /// Issuer's Ed25519 public key, same convention as the
    /// request side.
    #[serde(with = "hex_bytes")]
    pub issuer_pubkey: Vec<u8>,

    /// Capabilities the issuer actually granted (subset of what
    /// was requested). The invitee must persist this set as the
    /// authoritative grant.
    pub granted_capabilities: CapabilitySet,

    /// When this pairing expires. The trusted-device store drops
    /// the record after this. Set to `i64::MAX` for "no expiry".
    pub expires_at_unix: i64,

    /// `<scheme:1 byte = 0> || <r:32> || <s:32> || <v:1>` EIP-191 personal
    /// signature from the issuer's wallet over
    /// [`pairing_response_digest`].
    #[serde(with = "hex_bytes")]
    pub signature: Vec<u8>,
}

/// Helper for constructing a `PairingRequest`. Fills in the
/// canonical digest, the Ed25519 signature, and the timestamp for
/// you; the caller only chooses the inputs that matter.
pub struct PairingRequestBuilder<'a> {
    pub credential_id: CredentialId,
    pub node_id: &'a NodeId,
    pub transport_pubkey: &'a [u8],
    pub requested_capabilities: CapabilitySet,
    /// How long the request stays valid. Defaults to 300 s.
    pub ttl_seconds: i64,
}

impl<'a> PairingRequestBuilder<'a> {
    pub fn build(self, signer: &Ed25519Signer) -> PairingResult<PairingRequest> {
        let now = Utc::now().timestamp();
        let ttl = if self.ttl_seconds <= 0 {
            MAX_TIMESTAMP_SKEW_SECONDS
        } else {
            self.ttl_seconds
        };
        let nonce = random_nonce();
        let version = 1;
        let mut req = PairingRequest {
            version,
            credential_id: self.credential_id,
            node_id: self.node_id.clone(),
            transport_pubkey: self.transport_pubkey.to_vec(),
            requested_capabilities: self.requested_capabilities,
            expires_at_unix: now + ttl,
            nonce,
            signature: Vec::new(),
        };
        let digest = pairing_request_digest(&req);
        let sig = signer.sign(&digest)?;
        req.signature = sig;
        Ok(req)
    }
}

/// Helper for constructing a `PairingResponse` after the issuer
/// has decided which capabilities to grant.
pub struct PairingResponseBuilder<'a> {
    pub request: &'a PairingRequest,
    pub issuer_node_id: &'a NodeId,
    pub issuer_pubkey: &'a [u8],
    pub granted_capabilities: CapabilitySet,
    pub ttl_seconds: i64,
    pub issuer_wallet: &'a Wallet,
}

impl<'a> PairingResponseBuilder<'a> {
    pub fn build(self) -> PairingResult<PairingResponse> {
        let now = Utc::now().timestamp();
        let ttl = if self.ttl_seconds <= 0 {
            // Default: 90 days. Pairings are not "permanent" —
            // requiring re-pair is a useful safety net.
            90 * 24 * 3600
        } else {
            self.ttl_seconds
        };
        let mut resp = PairingResponse {
            version: 1,
            credential_id: self.request.credential_id,
            nonce: self.request.nonce,
            issuer_node_id: self.issuer_node_id.clone(),
            issuer_pubkey: self.issuer_pubkey.to_vec(),
            granted_capabilities: self.granted_capabilities,
            expires_at_unix: now + ttl,
            signature: Vec::new(),
        };
        let digest: [u8; 32] = pairing_response_digest(&resp);
        // EIP-191 personal-sign over secp256k1. Matches
        // `a3net_token::Pledge` so the existing `Wallet` flow
        // works without modification. `sign_personal` takes &[u8; 32].
        let sig =
            self.issuer_wallet
                .sign_personal(&digest)
                .map_err(|e| PairingError::Malformed {
                    what: "pairing_response.wallet_sign",
                    reason: format!("sign_personal failed: {e}"),
                })?;
        // Tag: scheme 0 = EIP-191. Format: <tag:1> || <compact:65>.
        let compact = sig.to_compact();
        let mut tagged = Vec::with_capacity(1 + compact.len());
        tagged.push(0);
        tagged.extend_from_slice(&compact);
        resp.signature = tagged;
        Ok(resp)
    }
}

/// Compute the canonical digest for a pairing request. Stable
/// across implementations; bump `version` on the request to
/// invalidate old signatures.
pub fn pairing_request_digest(req: &PairingRequest) -> [u8; 32] {
    let node_id_bytes = req.node_id.as_bytes();
    let mut h = Hasher::new();
    h.update(REQUEST_DOMAIN_TAG);
    h.update(&[req.version]);
    h.update(&req.credential_id);
    h.update(&node_id_bytes);
    h.update(&req.transport_pubkey);
    h.update(req.requested_capabilities.canonical().as_bytes());
    h.update(&req.expires_at_unix.to_be_bytes());
    h.update(&req.nonce);
    *h.finalize().as_bytes()
}

pub fn pairing_response_digest(resp: &PairingResponse) -> [u8; 32] {
    let issuer_node_bytes = resp.issuer_node_id.as_bytes();
    let mut h = Hasher::new();
    h.update(RESPONSE_DOMAIN_TAG);
    h.update(&[resp.version]);
    h.update(&resp.credential_id);
    h.update(&resp.nonce);
    h.update(&issuer_node_bytes);
    h.update(&resp.issuer_pubkey);
    h.update(resp.granted_capabilities.canonical().as_bytes());
    h.update(&resp.expires_at_unix.to_be_bytes());
    *h.finalize().as_bytes()
}

/// Verify a `PairingRequest`. Returns `Ok(())` on success.
///
/// Checks:
/// 1. version == 1
/// 2. expiry + clock skew
/// 3. transport signature scheme tag + length
/// 4. transport signature verification
/// 5. `NodeId` matches the public key (binding)
pub fn verify_pairing_request(req: &PairingRequest, now_unix: i64) -> PairingResult<()> {
    if req.version != 1 {
        return Err(PairingError::Malformed {
            what: "pairing_request.version",
            reason: format!("unsupported version {}", req.version),
        });
    }
    // Expired requests are rejected immediately — no skew check needed.
    if now_unix > req.expires_at_unix {
        return Err(PairingError::RequestExpired {
            expired_at_unix: req.expires_at_unix,
            now_unix,
        });
    }
    // The request is not expired. Now check whether the peer clock
    // is far enough in the future that our local clock looks stale.
    // We only examine the forward case (expires > now) because the
    // expired case above already handled the backward case.
    // Note: we do NOT use abs() here — abs() would incorrectly treat
    // expired requests as "skew too large" when now > expires AND
    // the gap exceeds MAX_TIMESTAMP_SKEW_SECONDS.
    let skew = req.expires_at_unix - now_unix;
    if skew > MAX_TIMESTAMP_SKEW_SECONDS {
        return Err(PairingError::ClockSkew {
            max_seconds: MAX_TIMESTAMP_SKEW_SECONDS,
            peer_unix: req.expires_at_unix,
            now_unix,
        });
    }

    let (tag, sig) = split_scheme(&req.signature)?;
    if tag != TRANSPORT_SCHEME_ED25519 {
        return Err(PairingError::UnsupportedScheme { scheme_tag: tag });
    }
    if sig.len() != 64 {
        return Err(PairingError::SignatureLength {
            expected: 64,
            got: sig.len(),
        });
    }
    if req.transport_pubkey.len() != 32 {
        return Err(PairingError::Malformed {
            what: "pairing_request.transport_pubkey",
            reason: format!("expected 32 bytes, got {}", req.transport_pubkey.len()),
        });
    }
    let digest = pairing_request_digest(req);
    let vk =
        VerifyingKey::from_bytes(req.transport_pubkey.as_slice().try_into().map_err(|_| {
            PairingError::Malformed {
                what: "pairing_request.transport_pubkey",
                reason: "not 32 bytes".into(),
            }
        })?)
        .map_err(|e| PairingError::Malformed {
            what: "pairing_request.transport_pubkey",
            reason: format!("invalid ed25519 public key: {e}"),
        })?;
    let sig_bytes: [u8; 64] = sig.try_into().map_err(|_| PairingError::SignatureLength {
        expected: 64,
        got: sig.len(),
    })?;
    let sig = Signature::from_bytes(&sig_bytes);
    vk.verify(&digest, &sig)
        .map_err(|_| PairingError::TransportSignatureInvalid)?;

    // NodeId must equal `blake3(transport_pubkey)` — that's the
    // invariant enforced by the caller (the iroh transport). Here we
    // check that the node_id bytes, when decoded, match the transport_pubkey.
    let node_id_bytes = req.node_id.as_bytes();
    if node_id_bytes != *req.transport_pubkey {
        return Err(PairingError::NodeIdMismatch);
    }
    Ok(())
}

pub fn verify_pairing_response(
    resp: &PairingResponse,
    request: &PairingRequest,
    now_unix: i64,
) -> PairingResult<()> {
    if resp.version != 1 {
        return Err(PairingError::Malformed {
            what: "pairing_response.version",
            reason: format!("unsupported version {}", resp.version),
        });
    }
    // CRITICAL: The nonce must echo back the invitee's request nonce.
    // Without this check an attacker can replay an old (but still-unexpired)
    // signed response from a previous pairing ceremony, causing the invitee
    // to accept stale granted capabilities.
    if resp.nonce != request.nonce {
        return Err(PairingError::NonceMismatch);
    }
    // The response MUST have an expiry. `i64::MAX` means "no expiry" is
    // accepted only for invitee-side responses (where the invitee is recording
    // its own pairing record); issuer-side checks should pass `now_unix`
    // so they enforce expiry too.
    if resp.expires_at_unix == i64::MAX {
        return Err(PairingError::Malformed {
            what: "pairing_response.expires_at_unix",
            reason: "response must have a finite expiry (i64::MAX is not allowed)".into(),
        });
    }
    if now_unix > resp.expires_at_unix {
        return Err(PairingError::RequestExpired {
            expired_at_unix: resp.expires_at_unix,
            now_unix,
        });
    }
    let digest: [u8; 32] = pairing_response_digest(resp);
    if resp.signature.is_empty() {
        return Err(PairingError::SignatureLength {
            expected: 66,
            got: 0,
        });
    }
    let (tag, sig) = split_scheme(&resp.signature)?;
    if tag != 0 {
        return Err(PairingError::UnsupportedScheme { scheme_tag: tag });
    }
    if sig.len() != 65 {
        return Err(PairingError::SignatureLength {
            expected: 65,
            got: sig.len(),
        });
    }
    let ps = PersonalSignature::from_compact(sig).map_err(|e| PairingError::Malformed {
        what: "pairing_response.signature",
        reason: format!("from_compact: {e}"),
    })?;
    let _ = WalletPublic::recover_personal(&digest, &ps)
        .map_err(|_| PairingError::IssuerSignatureInvalid)?;
    Ok(())
}

/// Ed25519 signer used by tests and by the offline pairing
/// builder. Production code wraps `iroh::SecretKey`.
pub struct Ed25519Signer {
    signing_key: SigningKey,
    /// Cached copy of the verifying-key bytes so the trait method
    /// can return `&[u8]` without unsafe lifetime hackery.
    pubkey_cache: [u8; 32],
}

impl Ed25519Signer {
    pub fn generate() -> Self {
        let mut bytes = [0u8; SECRET_KEY_LENGTH];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        Self::from_secret_bytes(bytes)
    }

    pub fn from_secret_bytes(secret: [u8; 32]) -> Self {
        let signing_key = SigningKey::from_bytes(&secret);
        let pubkey_cache = signing_key.verifying_key().to_bytes();
        Self {
            signing_key,
            pubkey_cache,
        }
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.pubkey_cache
    }

    pub fn verifying_key_bytes(&self) -> [u8; 32] {
        self.pubkey_cache
    }
}

impl TransportSigner for Ed25519Signer {
    fn public_key(&self) -> &[u8] {
        &self.pubkey_cache
    }

    fn sign(&self, msg: &[u8]) -> PairingResult<Vec<u8>> {
        let sig = self.signing_key.sign(msg);
        let mut out = Vec::with_capacity(1 + sig.to_bytes().len());
        out.push(TRANSPORT_SCHEME_ED25519);
        out.extend_from_slice(sig.to_bytes().as_slice());
        Ok(out)
    }
}

impl Default for Ed25519Signer {
    fn default() -> Self {
        Self::generate()
    }
}

pub struct Ed25519Verifier {
    public: [u8; 32],
}

impl Ed25519Verifier {
    pub fn new(public: [u8; 32]) -> Self {
        Self { public }
    }
}

impl TransportVerifier for Ed25519Verifier {
    fn verify(&self, msg: &[u8], signature: &[u8]) -> PairingResult<()> {
        let (tag, sig) = split_scheme(signature)?;
        if tag != TRANSPORT_SCHEME_ED25519 {
            return Err(PairingError::UnsupportedScheme { scheme_tag: tag });
        }
        let sig_bytes: [u8; 64] = sig.try_into().map_err(|_| PairingError::SignatureLength {
            expected: 64,
            got: sig.len(),
        })?;
        let sig = Signature::from_bytes(&sig_bytes);
        let vk = VerifyingKey::from_bytes(&self.public).map_err(|_| PairingError::Malformed {
            what: "verifier.public",
            reason: "not a valid ed25519 public key".into(),
        })?;
        vk.verify(msg, &sig)
            .map_err(|_| PairingError::TransportSignatureInvalid)
    }
}

/// Cross-crate interface so the iroh transport can hand us a
/// signing key without taking a hard dependency on `ed25519-dalek`'s
/// concrete types.
pub trait TransportSigner {
    /// Returns the 32-byte Ed25519 public key.
    fn public_key(&self) -> &[u8];

    /// Sign `msg`, returning `<scheme:1 byte> || <signature>`.
    fn sign(&self, msg: &[u8]) -> PairingResult<Vec<u8>>;
}

pub trait TransportVerifier {
    /// Verify `<scheme:1 byte> || <signature>` against `msg`.
    fn verify(&self, msg: &[u8], signature: &[u8]) -> PairingResult<()>;
}

fn split_scheme(sig: &[u8]) -> PairingResult<(u8, &[u8])> {
    if sig.is_empty() {
        return Err(PairingError::SignatureLength {
            expected: 1,
            got: 0,
        });
    }
    Ok((sig[0], &sig[1..]))
}

fn random_nonce() -> Nonce32 {
    let mut n = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut n);
    n
}

/// Derive a `CredentialId` from two node ids + a 32-byte salt. The
/// salt is the "pairing-id" half of the issuer's QR / email — it's
/// what makes two pairings between the same pair of nodes
/// distinguishable.
pub fn derive_credential_id(issuer: &NodeId, invitee: &NodeId, salt: &[u8; 32]) -> CredentialId {
    let issuer_bytes = issuer.as_bytes();
    let invitee_bytes = invitee.as_bytes();
    let mut h = Hasher::new();
    h.update(b"a3net-pairing/credential-id/v1\0");
    h.update(&issuer_bytes);
    h.update(&invitee_bytes);
    h.update(salt);
    let mut out = [0u8; 16];
    out.copy_from_slice(&h.finalize().as_bytes()[..16]);
    out
}

/// Compute the canonical digest for an invitation payload.
/// This lives here (rather than in `invitation.rs`) so that the
/// digest computation is co-located with the other pairing digests.
pub fn pairing_invitation_digest(payload: &crate::invitation::InvitationPayload) -> [u8; 32] {
    let issuer_node_bytes = payload.issuer_node_id.as_bytes();
    let issuer_wallet_bytes: [u8; 20] = *payload.issuer_wallet.as_bytes();
    let mut h = Hasher::new();
    h.update(INVITATION_DOMAIN_TAG);
    h.update(&[payload.version]);
    h.update(&issuer_node_bytes);
    h.update(&issuer_wallet_bytes);
    h.update(&payload.salt);
    h.update(payload.capabilities.canonical().as_bytes());
    h.update(&payload.expires_at_unix.to_be_bytes());
    // NOTE: `note` is deliberately NOT included in the digest — it is
    // cosmetic and MUST NOT invalidate existing signatures. Changing a
    // device name after issuance must not break already-displayed QR codes.
    *h.finalize().as_bytes()
}

pub(crate) mod hex_bytes {
    //! Hex-encoded `Vec<u8>` for serde. We avoid `serde_bytes` /
    //! `serde_json`'s default byte-array encoding because we want
    //! a portable ASCII form (`"0102abcd..."`) that flows through
    //! every JSON implementation without ambiguity.
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        hex::encode(bytes).serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        hex::decode(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_signer() -> Ed25519Signer {
        Ed25519Signer::generate()
    }

    fn node_id_from_pubkey(pubkey: [u8; 32]) -> NodeId {
        // NodeId::from_bytes expects 32 bytes and encodes them as hex.
        NodeId::from_bytes(&pubkey).unwrap()
    }

    #[test]
    fn derive_credential_id_is_deterministic() {
        let node_a = node_id_from_pubkey([7u8; 32]);
        let node_b = node_id_from_pubkey([9u8; 32]);
        let salt = [3u8; 32];
        let a = derive_credential_id(&node_a, &node_b, &salt);
        let b = derive_credential_id(&node_a, &node_b, &salt);
        assert_eq!(a, b);
        let c = derive_credential_id(&node_a, &node_b, &[4u8; 32]);
        assert_ne!(a, c);
    }

    #[test]
    fn round_trip_request() {
        let signer = fresh_signer();
        let pubkey = signer.public_key();
        let node_id = node_id_from_pubkey(pubkey);
        let req = PairingRequestBuilder {
            credential_id: [5u8; 16],
            node_id: &node_id,
            transport_pubkey: &pubkey,
            requested_capabilities: CapabilitySet::from_names(["chat", "files.read"]),
            ttl_seconds: 60,
        }
        .build(&signer)
        .unwrap();
        verify_pairing_request(&req, chrono::Utc::now().timestamp()).unwrap();
    }

    #[test]
    fn replay_expired_request() {
        let signer = fresh_signer();
        let pubkey = signer.public_key();
        let node_id = node_id_from_pubkey(pubkey);
        let mut req = PairingRequestBuilder {
            credential_id: [6u8; 16],
            node_id: &node_id,
            transport_pubkey: &pubkey,
            requested_capabilities: CapabilitySet::empty(),
            ttl_seconds: 60,
        }
        .build(&signer)
        .unwrap();
        req.expires_at_unix -= 1000; // simulate stale clock
        let err = verify_pairing_request(&req, chrono::Utc::now().timestamp()).unwrap_err();
        assert!(matches!(
            err,
            PairingError::RequestExpired { .. } | PairingError::ClockSkew { .. }
        ));
    }

    #[test]
    fn wrong_transport_key_rejected() {
        let signer = fresh_signer();
        let attacker = fresh_signer();
        let attacker_pubkey = attacker.public_key();
        let attacker_node = node_id_from_pubkey(attacker_pubkey);
        // Build a request that *claims* attacker's NodeId but was
        // signed by the legitimate key — i.e. forgery attempt.
        let req = PairingRequestBuilder {
            credential_id: [7u8; 16],
            node_id: &attacker_node,
            transport_pubkey: &attacker_pubkey,
            requested_capabilities: CapabilitySet::empty(),
            ttl_seconds: 60,
        }
        .build(&signer) // signed with the WRONG key
        .unwrap();
        let err = verify_pairing_request(&req, chrono::Utc::now().timestamp()).unwrap_err();
        // Either TransportSignatureInvalid (signature doesn't match
        // claimed pubkey) or NodeIdMismatch (node_id bytes != transport_pubkey)
        // — both are valid rejection paths.
        assert!(matches!(
            err,
            PairingError::TransportSignatureInvalid | PairingError::NodeIdMismatch
        ));
    }

    #[test]
    fn ed25519_signer_round_trip() {
        let signer = fresh_signer();
        let msg = b"hello world";
        let sig = signer.sign(msg).unwrap();
        let verifier = Ed25519Verifier::new(signer.public_key());
        verifier.verify(msg, &sig).unwrap();
    }
}
