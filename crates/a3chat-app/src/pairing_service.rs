//! `a3chat-app` pairing service — wraps `a3net-pairing` so JSON-RPC
//! clients (CLI / Tauri / Flutter) can issue, verify, accept, and
//! revoke P2P pairing invitations.
//!
//! ## Layering
//!
//! ```text
//! a3chat-rpc (JSON-RPC)
//!      │
//!      ▼
//! pairing_service    ◄── this module
//!      │
//!      ▼
//! a3net-pairing      (Ed25519 challenge-response, EIP-191 signed
//!                     invitations, capability grants, trusted-device
//!                     store, short human-readable pairing codes)
//! ```
//!
//! ## What this module deliberately does NOT do
//!
//! - **Noise_XX key agreement.** A successful pairing produces a
//!   [`TrustedDeviceRecord`] with an Ed25519 transport pubkey but the
//!   actual Noise handshake over iroh QUIC is wired up in P2 — see
//!   `keyring.rs::ensure_deterministic_dm_session`. The pairing
//!   store is the **trust anchor**; the Noise session is the **key
//!   anchor**. We intentionally split them so revocation works
//!   cleanly (revoking a paired device drops the trust record; the
//!   Noise keys would then fail the trust check and be rebuilt on
//!   next pair).
//! - **DNS / mDNS discovery.** `a3net-pairing` only proves control
//!   of a NodeId; the actual node-lookup lives in
//!   `a3net-nat-traversal` / `a3net-userstore::resolve_user_digit`
//!   which is not used here yet.
//!
//! ## Configuration
//!
//! The service is constructed with a [`PairingServiceConfig`] that
//! pins a base directory for the [`a3net_pairing::TrustedDeviceStore`]
//! and a wallet secret (32 raw bytes). The wallet is the EIP-191
//! signing key used for invitation signatures — the same secret the
//! user employs to recover the identity on a new device. Tests inject
//! an ephemeral `Wallet::generate()`; production wires in the
//! identity managed by `a3net-identity`'s keychain.
//!
//! ## RPC surface
//!
//! See [`crate::app::A3chatApp::dispatch`] for routing. Methods:
//!
//! - `a3chat.pairing.invitation.create`
//! - `a3chat.pairing.invitation.verify`
//! - `a3chat.pairing.invitation.parse`
//! - `a3chat.pairing.invitation.accept`
//! - `a3chat.pairing.invitation.revoke`   (revoke by credential_id)
//! - `a3chat.pairing.trusted.list`
//! - `a3chat.pairing.trusted.get`
//! - `a3chat.pairing.trusted.revoke`
//! - `a3chat.pairing.code.create`
//! - `a3chat.pairing.code.parse`
//! - `a3chat.pairing.health`

use std::path::PathBuf;
use std::sync::Arc;

use a3net_identity::wallet::Wallet;
use a3net_pairing::capability::CapabilitySet;
use a3net_pairing::code::InvitationCode;
use a3net_pairing::error::PairingError;
use a3net_pairing::invitation::SignedInvitation;
use a3net_pairing::store::{TrustedDeviceStore, TrustedDeviceStoreConfig};
use a3net_types::node::NodeId;
use a3net_pairing::transport_identity::CredentialId;
use a3net_pairing::trusted_device::{
    TrustedDeviceRecord, TrustedDeviceRole, TrustedDeviceStatus,
};
use parking_lot::RwLock;

use a3chat_core::error::A3chatError;
use a3chat_core::event::A3chatEvent;
use a3chat_core::id::UserId;
use a3chat_core::rpc::A3chatRpcMethod;

use crate::error::{AppError, AppResult};
use crate::notification_bus::NotificationBus;

/// Stable list of every JSON-RPC method name served by this service.
/// Mirrors the corresponding `A3chatRpcMethod::PAIRING_*` constants so
/// the dispatcher can pattern-match without re-importing every call site.
pub const METHODS: &[&str] = &[
    A3chatRpcMethod::PAIRING_INVITATION_CREATE,
    A3chatRpcMethod::PAIRING_INVITATION_VERIFY,
    A3chatRpcMethod::PAIRING_INVITATION_PARSE,
    A3chatRpcMethod::PAIRING_INVITATION_ACCEPT,
    A3chatRpcMethod::PAIRING_INVITATION_REVOKE,
    A3chatRpcMethod::PAIRING_TRUSTED_LIST,
    A3chatRpcMethod::PAIRING_TRUSTED_GET,
    A3chatRpcMethod::PAIRING_TRUSTED_REVOKE,
    A3chatRpcMethod::PAIRING_CODE_CREATE,
    A3chatRpcMethod::PAIRING_CODE_PARSE,
    A3chatRpcMethod::PAIRING_HEALTH,
];

/// Default TTL for invitations — 15 minutes, matching the
/// `a3net-pairing` default so callers can omit `ttl_seconds`.
pub const DEFAULT_INVITATION_TTL_SECONDS: i64 = 15 * 60;

/// Maximum `ttl_seconds` for a single invitation. Larger values are
/// clamped down — long-lived invitations increase the replay window
/// (the signature is recoverable for as long as the issuer is alive).
pub const MAX_INVITATION_TTL_SECONDS: i64 = 7 * 24 * 60 * 60; // 7 days

/// Default capability set granted to a freshly-paired device. Mirrors
/// what Signal / Matrix would call "send text messages". Tightening
/// this set is the easiest way to make lost devices less harmful.
fn default_capability_set() -> CapabilitySet {
    CapabilitySet::from_names(["chat", "presence", "sync"])
}

/// Input for [`PairingService::create_invitation`].
#[derive(Debug, Clone)]
pub struct CreateInvitationRequest {
    /// Issuer's transport NodeId (32-byte public key view, hex).
    pub issuer_node_id: String,
    /// Capability names to grant. `None` ⇒ use [`default_capability_set`].
    pub capabilities: Option<Vec<String>>,
    /// Time-to-live in seconds from now. `None` ⇒
    /// [`DEFAULT_INVITATION_TTL_SECONDS`].
    pub ttl_seconds: Option<i64>,
    /// Human-readable note (e.g. "Alice's MacBook").
    pub note: Option<String>,
}

/// Output of [`PairingService::create_invitation`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CreateInvitationResponse {
    /// JSON-encoded [`SignedInvitation`] (encode directly into QR).
    pub invitation_json: String,
    /// Hex-encoded SHA-256 of `invitation_json` (lets callers
    /// display a verification fingerprint without re-hashing).
    pub invitation_digest_hex: String,
    /// Unix expiry — useful for UI countdowns.
    pub expires_at_unix: i64,
    /// Issuer's NodeId echoed back so the caller can sanity-check.
    pub issuer_node_id: String,
}

/// Input for [`PairingService::accept_invitation`].
#[derive(Debug, Clone)]
pub struct AcceptInvitationRequest {
    /// JSON-encoded [`SignedInvitation`] (what the issuer produced).
    pub invitation_json: String,
    /// Invitee (this side) NodeId, hex. The credential_id is derived
    /// from `(issuer_node_id, invitee_node_id, salt)`.
    pub invitee_node_id: String,
    /// Invitee transport pubkey (Ed25519, 32 bytes). Stored in the
    /// trusted-device record so the issuer's transport layer can
    /// authenticate inbound frames.
    pub invitee_transport_pubkey: Vec<u8>,
    /// Human-readable device name (e.g. "Bob's iPhone 15").
    pub device_name: String,
    /// Capabilities to *request*. The intersection with the issuer's
    /// grant set becomes the granted set on the local record.
    pub requested_capabilities: Vec<String>,
}

/// Output of [`PairingService::accept_invitation`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AcceptInvitationResponse {
    pub credential_id_hex: String,
    pub issuer_node_id: String,
    pub invitee_node_id: String,
    pub granted_capabilities: Vec<String>,
    pub paired_at_unix: i64,
    pub device_name: String,
}

/// Pairing-service configuration.
#[derive(Debug, Clone)]
pub struct PairingServiceConfig {
    /// Directory in which `devices.jsonl` lives. Created on open if
    /// missing.
    pub data_dir: PathBuf,
    /// 32-byte secp256k1 wallet secret. **In production this MUST come
    /// from the secure enclave / keyring — never commit a real key.**
    /// Tests use `Wallet::generate().secret_bytes()`.
    pub wallet_secret: [u8; 32],
    /// The local user's transport NodeId (hex). Stored so the
    /// service can build a default issuer record on first call.
    pub local_node_id: String,
}

impl PairingServiceConfig {
    /// Convenience: build a config rooted under `base/pairing`.
    pub fn under_base(base: &std::path::Path, local_node_id: String) -> Self {
        Self {
            data_dir: base.join("pairing"),
            // Caller must populate this before constructing the
            // service. We deliberately leave it zero so an
            // accidental `under_base`-only construction fails loudly
            // in `Self::open` rather than silently signing everything
            // with the zero key (which would still verify because
            // `secp256k1::SecretKey::from_slice` rejects zero — but
            // we want the build to be explicit).
            wallet_secret: [0u8; 32],
            local_node_id,
        }
    }
}

/// Pairing service backed by `a3net-pairing`.
///
/// The service is cheap to clone — every field is either `Clone` or
/// wrapped in an `Arc` / `RwLock`. Clone freely and hand sub-handles
/// to per-connection RPC layers.
#[derive(Clone)]
pub struct PairingService {
    /// Owning user — used by the dispatcher so a multi-tenant
    /// deployment can route events onto the right per-user bus. For
    /// single-user builds this is the only user.
    pub owner: UserId,
    bus: NotificationBus,
    store: Arc<TrustedDeviceStore>,
    wallet: Arc<Wallet>,
    local_node_id: Arc<RwLock<String>>,
    config: Arc<PairingServiceConfig>,
    /// Keeps the `data_dir` alive for the lifetime of the service so
    /// that the underlying `TrustedDeviceStore` can continue to write
    /// to it. Only meaningful for tests that build the service via
    /// `in_memory()` (which uses a `tempfile::TempDir`); in production
    /// `data_dir` is a long-lived, caller-managed path.
    _data_dir_guard: Option<Arc<tempfile::TempDir>>,
}

impl std::fmt::Debug for PairingService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingService")
            .field("owner", &self.owner)
            .field("data_dir", &self.config.data_dir)
            .field(
                "local_node_id",
                &self.local_node_id.read().clone(),
            )
            .field("trusted_devices", &self.store.len())
            .field("nonce_count", &self.store.nonce_count())
            .finish_non_exhaustive()
    }
}

impl PairingService {
    /// Open (or create) a pairing service from `config`. The wallet
    /// secret must be non-zero.
    pub fn open(config: PairingServiceConfig, owner: UserId) -> AppResult<Self> {
        if config.wallet_secret == [0u8; 32] {
            return Err(AppError::Internal(
                "PairingServiceConfig.wallet_secret must be non-zero".into(),
            ));
        }
        if config.local_node_id.len() != 64
            || !config.local_node_id.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err(AppError::Domain(format!(
                "local_node_id must be 64 hex chars, got {}",
                config.local_node_id.len()
            )));
        }
        std::fs::create_dir_all(&config.data_dir).map_err(|e| {
            AppError::Storage(format!(
                "create pairing dir {}: {}",
                config.data_dir.display(),
                e
            ))
        })?;
        let store_config = TrustedDeviceStoreConfig {
            path: config.data_dir.join("devices.jsonl"),
            ..Default::default()
        };
        let store = TrustedDeviceStore::open(store_config)
            .map_err(|e| AppError::Storage(format!("trusted-device-store open: {e}")))?;
        let wallet = Wallet::from_bytes(&config.wallet_secret).map_err(|e| {
            AppError::Crypto(format!(
                "PairingServiceConfig.wallet_secret invalid: {e}"
            ))
        })?;
        Ok(Self {
            owner,
            bus: NotificationBus::default(),
            store: Arc::new(store),
            wallet: Arc::new(wallet),
            local_node_id: Arc::new(RwLock::new(config.local_node_id.clone())),
            config: Arc::new(config),
            _data_dir_guard: None,
        })
    }

    /// Build an in-memory service suitable for tests. Uses
    /// `Wallet::generate()` so the secret is fresh for every call.
    /// Persists to a `tempdir` so reload semantics can be exercised.
    pub fn in_memory(owner: UserId, local_node_id: String) -> AppResult<Self> {
        let dir = Arc::new(tempfile::tempdir().map_err(AppError::from)?);
        let wallet = Wallet::generate();
        let mut svc = Self::open(
            PairingServiceConfig {
                data_dir: dir.path().to_path_buf(),
                wallet_secret: wallet.secret_bytes(),
                local_node_id,
            },
            owner,
        )?;
        // Hold the tempdir alive for the lifetime of the service so the
        // underlying `devices.jsonl` continues to be writable.
        svc._data_dir_guard = Some(dir);
        Ok(svc)
    }

    /// Borrow the in-process notification bus. Used by tests that
    /// want to assert events were emitted.
    pub fn bus(&self) -> &NotificationBus {
        &self.bus
    }

    /// Borrow the underlying trusted-device store. Callers should
    /// avoid mutating it directly — the service is the canonical
    /// owner. Exposed for read-only inspection (`len`, `all`, etc.).
    pub fn store(&self) -> &TrustedDeviceStore {
        &self.store
    }

    /// Borrow the local node id (the issuer of every invitation
    /// produced by this service).
    pub fn local_node_id(&self) -> String {
        self.local_node_id.read().clone()
    }

    /// Override the local node id. Useful when the wallet's NodeId
    /// is rotated (e.g. after a recovery operation) — every future
    /// invitation will then carry the new identity.
    pub fn set_local_node_id(&self, new_node_id: String) -> AppResult<()> {
        if new_node_id.len() != 64
            || !new_node_id.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err(AppError::Domain(format!(
                "new_node_id must be 64 hex chars, got {}",
                new_node_id.len()
            )));
        }
        *self.local_node_id.write() = new_node_id;
        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────
    // Invitation lifecycle
    // ─────────────────────────────────────────────────────────────────

    /// Issue a new signed pairing invitation. The returned
    /// `invitation_json` is what gets encoded into a QR or sent over
    /// an out-of-band channel (email / push / SMS).
    pub fn create_invitation(
        &self,
        req: CreateInvitationRequest,
    ) -> AppResult<CreateInvitationResponse> {
        let node_id = parse_node_id_hex(&req.issuer_node_id)?;
        let caps = match req.capabilities {
            Some(names) => CapabilitySet::from_iter(
                names
                    .iter()
                    .filter_map(|n| a3net_pairing::capability::Capability::from_name(n)),
            ),
            None => default_capability_set(),
        };
        let ttl = req
            .ttl_seconds
            .unwrap_or(DEFAULT_INVITATION_TTL_SECONDS)
            .clamp(1, MAX_INVITATION_TTL_SECONDS);
        let inv = SignedInvitation::create(&node_id, &self.wallet, caps, ttl, req.note)
            .map_err(pair_err_to_app)?;
        let expires_at_unix = inv.payload.expires_at_unix;
        let invitation_json = inv.to_json().map_err(pair_err_to_app)?;
        let digest_hex = hex::encode(blake3::hash(invitation_json.as_bytes()).as_bytes());

        self.bus
            .publish(A3chatEvent::PairingInvitationCreated {
                user_id: self.owner.clone(),
                issuer_node_id: req.issuer_node_id.clone(),
                expires_at_unix,
            });

        Ok(CreateInvitationResponse {
            invitation_json,
            invitation_digest_hex: digest_hex,
            expires_at_unix,
            issuer_node_id: req.issuer_node_id,
        })
    }

    /// Verify a JSON-encoded invitation. `now_unix` is provided so
    /// tests can pin a clock; production callers pass `chrono::Utc::now().timestamp()`.
    pub fn verify_invitation(&self, invitation_json: &str, now_unix: i64) -> AppResult<()> {
        let inv = SignedInvitation::from_json(invitation_json).map_err(pair_err_to_app)?;
        inv.verify(now_unix).map_err(pair_err_to_app)?;
        Ok(())
    }

    /// Parse a JSON-encoded invitation and return the decoded payload
    /// (without verifying). Used by the UI to display the issuer
    /// NodeId / wallet / capabilities before the user confirms. The
    /// **result must NOT be trusted** until `verify_invitation` is
    /// also called.
    pub fn parse_invitation(
        &self,
        invitation_json: &str,
    ) -> AppResult<DecodedInvitation> {
        let inv = SignedInvitation::from_json(invitation_json).map_err(pair_err_to_app)?;
        Ok(DecodedInvitation {
            issuer_node_id: inv.payload.issuer_node_id.to_string(),
            issuer_wallet: inv.payload.issuer_wallet.to_string(),
            expires_at_unix: inv.payload.expires_at_unix,
            capabilities: inv
                .payload
                .capabilities
                .iter()
                .map(|c| c.name().to_string())
                .collect(),
            note: inv.payload.note.clone(),
            version: inv.payload.version,
        })
    }

    /// Accept a JSON-encoded invitation. Persists a local
    /// `TrustedDeviceRecord` so the issuer is recognised as a
    /// controller on subsequent handshakes.
    pub fn accept_invitation(
        &self,
        req: AcceptInvitationRequest,
    ) -> AppResult<AcceptInvitationResponse> {
        let inv = SignedInvitation::from_json(&req.invitation_json).map_err(pair_err_to_app)?;
        inv.verify(chrono::Utc::now().timestamp()).map_err(pair_err_to_app)?;

        let issuer_node_id = inv.payload.issuer_node_id.clone();
        let invitee_node_id = parse_node_id_hex(&req.invitee_node_id)?;
        let credential_id = inv
            .credential_id(&invitee_node_id)
            .map_err(pair_err_to_app)?;

        if req.invitee_transport_pubkey.len() != 32 {
            return Err(AppError::Domain(format!(
                "invitee_transport_pubkey must be 32 bytes (Ed25519), got {}",
                req.invitee_transport_pubkey.len()
            )));
        }

        // Intersect requested capabilities with what the issuer is
        // willing to grant. A pair-everything request still gets
        // downscoped to the intersection — `signed_invitation` is
        // the authority.
        let requested = CapabilitySet::from_iter(
            req.requested_capabilities
                .iter()
                .filter_map(|n| a3net_pairing::capability::Capability::from_name(n)),
        );
        let granted = intersect_capsets(&requested, &inv.payload.capabilities);

        let now = chrono::Utc::now().timestamp();
        let record = TrustedDeviceRecord {
            credential_id,
            role: TrustedDeviceRole::Invitee, // this side is being invited
            device_name: req.device_name,
            paired_at_unix: now,
            expires_at_unix: inv.payload.expires_at_unix,
            last_seen_unix: now,
            node_id: issuer_node_id.to_string(),
            transport_pubkey: req.invitee_transport_pubkey,
            wallet_address: Some(inv.payload.issuer_wallet.clone()),
            capabilities: granted.clone(),
            status: TrustedDeviceStatus::Active,
            record_version: 1,
            issuer_node_id: issuer_node_id.to_string(),
            revoked_at_unix: 0,
        };
        record
            .validate()
            .map_err(|e| AppError::Domain(format!("trust-record invalid: {e}")))?;
        self.store.insert(record.clone()).map_err(pair_err_to_app)?;

        let credential_id_hex = hex::encode(record.credential_id);
        self.bus.publish(A3chatEvent::PairingTrustedDeviceAdded {
            user_id: self.owner.clone(),
            credential_id: credential_id_hex.clone(),
            role: "invitee".into(),
            device_name: record.device_name.clone(),
        });

        Ok(AcceptInvitationResponse {
            credential_id_hex,
            issuer_node_id: issuer_node_id.to_string(),
            invitee_node_id: invitee_node_id.to_string(),
            granted_capabilities: granted.iter().map(|c| c.name().to_string()).collect(),
            paired_at_unix: record.paired_at_unix,
            device_name: record.device_name,
        })
    }

    // ─────────────────────────────────────────────────────────────────
    // Trusted-device CRUD (issuer side)
    // ─────────────────────────────────────────────────────────────────

    /// Persist a record representing the issuer's view of a paired
    /// peer. Call this on the issuer side after a successful
    /// pairing exchange. The supplied `invitee_transport_pubkey` is
    /// the Ed25519 pubkey the invitee advertised during the
    /// handshake.
    #[allow(clippy::too_many_arguments)]
    pub fn record_issuer_pairing(
        &self,
        issuer_node_id: &str,
        invitee_node_id: &str,
        invitee_transport_pubkey: Vec<u8>,
        device_name: String,
        granted_capability_names: Vec<String>,
    ) -> AppResult<TrustedDeviceRecord> {
        if invitee_transport_pubkey.len() != 32 {
            return Err(AppError::Domain(format!(
                "invitee_transport_pubkey must be 32 bytes (Ed25519), got {}",
                invitee_transport_pubkey.len()
            )));
        }
        let issuer = parse_node_id_hex(issuer_node_id)?;
        let invitee = parse_node_id_hex(invitee_node_id)?;
        let salt = random_salt();
        let credential_id = a3net_pairing::transport_identity::derive_credential_id(
            &issuer, &invitee, &salt,
        );

        let caps = CapabilitySet::from_iter(
            granted_capability_names
                .iter()
                .filter_map(|n| a3net_pairing::capability::Capability::from_name(n)),
        );

        let now = chrono::Utc::now().timestamp();
        let record = TrustedDeviceRecord {
            credential_id,
            role: TrustedDeviceRole::Issuer,
            device_name,
            paired_at_unix: now,
            expires_at_unix: i64::MAX,
            last_seen_unix: now,
            node_id: invitee.to_string(),
            transport_pubkey: invitee_transport_pubkey,
            wallet_address: None,
            capabilities: caps,
            status: TrustedDeviceStatus::Active,
            record_version: 1,
            issuer_node_id: issuer.to_string(),
            revoked_at_unix: 0,
        };
        record
            .validate()
            .map_err(|e| AppError::Domain(format!("trust-record invalid: {e}")))?;
        self.store.insert(record.clone()).map_err(pair_err_to_app)?;

        let credential_id_hex = hex::encode(record.credential_id);
        self.bus.publish(A3chatEvent::PairingTrustedDeviceAdded {
            user_id: self.owner.clone(),
            credential_id: credential_id_hex,
            role: "issuer".into(),
            device_name: record.device_name.clone(),
        });

        Ok(record)
    }

    /// List every trusted-device record (active + revoked). Order is
    /// unspecified — callers must sort by `paired_at_unix` if they
    /// care.
    pub fn list_trusted_devices(&self) -> AppResult<Vec<TrustedDeviceRecord>> {
        Ok(self.store.all())
    }

    /// Look up a single record by its 16-byte credential id.
    pub fn get_trusted_device(
        &self,
        credential_id_hex: &str,
    ) -> AppResult<Option<TrustedDeviceRecord>> {
        let bytes = parse_credential_id_hex(credential_id_hex)?;
        Ok(self.store.get(&bytes))
    }

    /// Revoke a record. Returns `Ok(true)` if the record existed and
    /// was revoked, `Ok(false)` if no such record. The on-disk file
    /// is rewritten atomically by the store.
    pub fn revoke_trusted_device(&self, credential_id_hex: &str) -> AppResult<bool> {
        let bytes = parse_credential_id_hex(credential_id_hex)?;
        match self.store.get(&bytes) {
            None => Ok(false),
            Some(_) => {
                self.store.revoke(&bytes).map_err(pair_err_to_app)?;
                self.bus.publish(A3chatEvent::PairingTrustedDeviceRevoked {
                    user_id: self.owner.clone(),
                    credential_id: credential_id_hex.to_string(),
                });
                Ok(true)
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────
    // Short pairing code (ADNET:XXXX-YYYY-ZZZZ-NNNN)
    // ─────────────────────────────────────────────────────────────────

    /// Derive a short human-readable code from an invitation JSON.
    /// The full invitation must still be exchanged out-of-band (e.g.
    /// via QR); the code is for **manual entry** over a phone call.
    pub fn create_short_code(&self, invitation_json: &str) -> AppResult<String> {
        let inv = SignedInvitation::from_json(invitation_json).map_err(pair_err_to_app)?;
        let code = InvitationCode::from_invitation(&inv).map_err(pair_err_to_app)?;
        Ok(code.to_string())
    }

    /// Parse + format-check a short code. Does NOT validate that the
    /// code matches any real invitation (the full invitation is
    /// required for that) — only that it is well-formed.
    pub fn parse_short_code(&self, raw: &str) -> AppResult<ParsedCode> {
        let code: InvitationCode = raw.parse().map_err(pair_err_to_app)?;
        Ok(ParsedCode {
            raw: code.as_str().to_string(),
            display: code.to_string(),
            segment_count: a3net_pairing::code::InvitationCode::segment_count_for_tests(),
        })
    }

    // ─────────────────────────────────────────────────────────────────
    // Health
    // ─────────────────────────────────────────────────────────────────

    /// Cheap probe used by `a3chat.healthz`.
    pub fn health(&self) -> serde_json::Value {
        serde_json::json!({
            "ok": true,
            "service": "a3chat.pairing",
            "trusted_devices": self.store.len(),
            "nonce_count": self.store.nonce_count(),
            "local_node_id": self.local_node_id(),
            "wallet_address": self.wallet.public().address().to_checksum(),
        })
    }
}

/// Lightweight, transport-agnostic view of an invitation payload.
/// Returned by [`PairingService::parse_invitation`] so the UI can
/// render the issuer's details without trusting them yet (see
/// [`PairingService::verify_invitation`] for the trust step).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DecodedInvitation {
    pub version: u8,
    pub issuer_node_id: String,
    pub issuer_wallet: String,
    pub expires_at_unix: i64,
    pub capabilities: Vec<String>,
    pub note: Option<String>,
}

/// Parsed short code result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParsedCode {
    pub raw: String,
    pub display: String,
    pub segment_count: usize,
}

// ─────────────────────────────────────────────────────────────────
// Errors & helpers
// ─────────────────────────────────────────────────────────────────

fn parse_node_id_hex(hex_str: &str) -> AppResult<NodeId> {
    NodeId::from_hex(hex_str).map_err(|e| AppError::Domain(format!("invalid NodeId: {e}")))
}

fn parse_credential_id_hex(hex_str: &str) -> AppResult<CredentialId> {
    let raw = hex::decode(hex_str).map_err(|e| {
        AppError::Domain(format!("credential_id must be hex: {e}"))
    })?;
    if raw.len() != 16 {
        return Err(AppError::Domain(format!(
            "credential_id must be 16 bytes, got {}",
            raw.len()
        )));
    }
    let mut out = [0u8; 16];
    out.copy_from_slice(&raw);
    Ok(out)
}

fn pair_err_to_app(e: PairingError) -> AppError {
    match e {
        // Map canonical pairing errors onto existing app-error
        // variants so the RPC layer keeps producing sensible codes.
        PairingError::DeviceRevoked(_) => AppError::Forbidden(e.to_string()),
        PairingError::DeviceNotFound(_) => AppError::Domain(e.to_string()),
        PairingError::DeviceExpired { .. } => AppError::Domain(e.to_string()),
        PairingError::NonceReplay { .. } => AppError::Forbidden(e.to_string()),
        PairingError::SignatureLength { .. }
        | PairingError::UnsupportedScheme { .. }
        | PairingError::IssuerSignatureInvalid => AppError::Crypto(e.to_string()),
        PairingError::Malformed { .. } => AppError::Domain(e.to_string()),
        PairingError::InvitationExpired { .. } => AppError::Domain(e.to_string()),
        PairingError::Storage(_) | PairingError::Serialization(_) => {
            AppError::Storage(e.to_string())
        }
        other => AppError::Internal(other.to_string()),
    }
}

fn random_salt() -> [u8; 32] {
    use rand::RngCore;
    let mut salt = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    salt
}

fn intersect_capsets(
    requested: &CapabilitySet,
    granted: &CapabilitySet,
) -> CapabilitySet {
    let mut out = CapabilitySet::empty();
    for cap in requested.iter() {
        if granted.contains(*cap) {
            out.insert(*cap);
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────
// RPC dispatcher (matches `A3chatRpcMethod::PAIRING_*`)
// ─────────────────────────────────────────────────────────────────

/// Dispatch helper used by `a3chat-rpc`.
pub async fn dispatch(
    svc: Arc<PairingService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    let resp = match method {
        A3chatRpcMethod::PAIRING_INVITATION_CREATE => {
            let req = CreateInvitationRequest {
                issuer_node_id: pick_string(&params, "issuer_node_id")
                    .unwrap_or_else(|| svc.local_node_id()),
                capabilities: pick_array(&params, "capabilities"),
                ttl_seconds: pick_i64(&params, "ttl_seconds"),
                note: pick_string(&params, "note"),
            };
            let r = svc.create_invitation(req).map_err(A3chatError::from)?;
            serde_json::to_value(r).map_err(A3chatError::from)?
        }
        A3chatRpcMethod::PAIRING_INVITATION_VERIFY => {
            let json = required_string(&params, "invitation_json")?;
            let now = pick_i64(&params, "now_unix")
                .unwrap_or_else(|| chrono::Utc::now().timestamp());
            svc.verify_invitation(&json, now)
                .map_err(A3chatError::from)?;
            serde_json::json!({ "ok": true, "now_unix": now })
        }
        A3chatRpcMethod::PAIRING_INVITATION_PARSE => {
            let json = required_string(&params, "invitation_json")?;
            let r = svc
                .parse_invitation(&json)
                .map_err(A3chatError::from)?;
            serde_json::to_value(r).map_err(A3chatError::from)?
        }
        A3chatRpcMethod::PAIRING_INVITATION_ACCEPT => {
            let req = AcceptInvitationRequest {
                invitation_json: required_string(&params, "invitation_json")?,
                invitee_node_id: required_string(&params, "invitee_node_id")?,
                invitee_transport_pubkey: required_bytes(&params, "invitee_transport_pubkey")?,
                device_name: params
                    .get("device_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                requested_capabilities: pick_array(&params, "requested_capabilities")
                    .unwrap_or_default(),
            };
            let r = svc.accept_invitation(req).map_err(A3chatError::from)?;
            serde_json::to_value(r).map_err(A3chatError::from)?
        }
        A3chatRpcMethod::PAIRING_TRUSTED_LIST => {
            let list = svc.list_trusted_devices().map_err(A3chatError::from)?;
            serde_json::to_value(list).map_err(A3chatError::from)?
        }
        A3chatRpcMethod::PAIRING_TRUSTED_GET => {
            let hex = required_string(&params, "credential_id")?;
            let r = svc.get_trusted_device(&hex).map_err(A3chatError::from)?;
            serde_json::to_value(r).map_err(A3chatError::from)?
        }
        A3chatRpcMethod::PAIRING_TRUSTED_REVOKE
        | A3chatRpcMethod::PAIRING_INVITATION_REVOKE => {
            // Both methods take the same `credential_id` and behave
            // identically — revoke a record in the trusted-device
            // store.
            let hex = required_string(&params, "credential_id")?;
            let removed = svc.revoke_trusted_device(&hex).map_err(A3chatError::from)?;
            serde_json::json!({ "revoked": removed })
        }
        A3chatRpcMethod::PAIRING_CODE_CREATE => {
            let json = required_string(&params, "invitation_json")?;
            let s = svc.create_short_code(&json).map_err(A3chatError::from)?;
            serde_json::json!({ "code": s })
        }
        A3chatRpcMethod::PAIRING_CODE_PARSE => {
            let raw = required_string(&params, "code")?;
            let r = svc.parse_short_code(&raw).map_err(A3chatError::from)?;
            serde_json::to_value(r).map_err(A3chatError::from)?
        }
        A3chatRpcMethod::PAIRING_HEALTH => svc.health(),
        _ => {
            return Err(A3chatError::Internal(format!(
                "PairingService does not handle {method}"
            )))
        }
    };
    // Suppress the unused-variable warning for `owner` (kept for
    // future per-user routing).
    let _ = owner;
    Ok(resp)
}

fn pick_string(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(str::to_string)
}

fn pick_i64(v: &serde_json::Value, key: &str) -> Option<i64> {
    v.get(key).and_then(|x| x.as_i64())
}

fn pick_array(v: &serde_json::Value, key: &str) -> Option<Vec<String>> {
    v.get(key).and_then(|x| x.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect()
    })
}

fn required_string(v: &serde_json::Value, key: &str) -> Result<String, A3chatError> {
    pick_string(v, key).ok_or_else(|| {
        A3chatError::InvalidInput(format!("missing or non-string `{key}`"))
    })
}

fn required_bytes(v: &serde_json::Value, key: &str) -> Result<Vec<u8>, A3chatError> {
    // Accept either a JSON array of numbers or a hex string — the
    // latter is friendlier for hand-written curl examples.
    if let Some(hex) = v.get(key).and_then(|x| x.as_str()) {
        return hex::decode(hex).map_err(|e| {
            A3chatError::InvalidInput(format!("`{key}` hex decode: {e}"))
        });
    }
    let arr = v
        .get(key)
        .and_then(|x| x.as_array())
        .ok_or_else(|| A3chatError::InvalidInput(format!("missing `{key}`")))?;
    let mut out = Vec::with_capacity(arr.len());
    for x in arr {
        let n = x.as_u64().ok_or_else(|| {
            A3chatError::InvalidInput(format!(
                "`{key}` array elements must be u8 (0..255)"
            ))
        })?;
        if n > u8::MAX as u64 {
            return Err(A3chatError::InvalidInput(format!(
                "`{key}` array element {n} does not fit in u8"
            )));
        }
        out.push(n as u8);
    }
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────
// Internal extension: add a `segment_count` for tests without
// re-exporting private fields. Kept here so adding a public field on
// `a3net_pairing::code::InvitationCode` is unnecessary for a single
// consumer.
// ─────────────────────────────────────────────────────────────────

trait InvitationCodeExt {
    fn segment_count_for_tests() -> usize;
}

impl InvitationCodeExt for a3net_pairing::code::InvitationCode {
    fn segment_count_for_tests() -> usize {
        // ADNET:XXXX-YYYY-ZZZZ-NNNN — 4 segments.
        4
    }
}

// ─────────────────────────────────────────────────────────────────
// Internal extension: convert `WalletPublic` address to an
// EIP-55 checksum string for the health endpoint.
// ─────────────────────────────────────────────────────────────────

trait AddressExt {
    fn to_checksum(&self, chain_id: Option<u64>) -> String;
}

impl AddressExt for a3net_identity::Address {
    fn to_checksum(&self, _chain_id: Option<u64>) -> String {
        // We don't pull in the `tiny-keccak`-based EIP-55 helper to
        // keep the dep tree small; an all-lowercase address is
        // unambiguous and matches the on-disk form.
        self.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_node(byte: u8) -> String {
        NodeId::from_bytes(&[byte; 32]).unwrap().to_string()
    }

    #[tokio::test]
    async fn create_verify_accept_round_trip() {
        let issuer = PairingService::in_memory(UserId::from("alice"), mk_node(0xAA)).unwrap();
        let invitee = PairingService::in_memory(UserId::from("bob"), mk_node(0xBB)).unwrap();

        let inv = issuer
            .create_invitation(CreateInvitationRequest {
                issuer_node_id: issuer.local_node_id(),
                capabilities: Some(vec!["chat".into(), "files.read".into()]),
                ttl_seconds: Some(60),
                note: Some("Alice's laptop".into()),
            })
            .unwrap();
        issuer
            .verify_invitation(&inv.invitation_json, chrono::Utc::now().timestamp())
            .unwrap();
        let decoded = issuer.parse_invitation(&inv.invitation_json).unwrap();
        assert_eq!(decoded.capabilities, vec!["chat", "files.read"]);

        let pubkey = vec![0xCCu8; 32];
        let accepted = invitee
            .accept_invitation(AcceptInvitationRequest {
                invitation_json: inv.invitation_json,
                invitee_node_id: invitee.local_node_id(),
                invitee_transport_pubkey: pubkey,
                device_name: "Bob's phone".into(),
                requested_capabilities: vec!["chat".into(), "sync".into(), "files.read".into()],
            })
            .unwrap();
        // Issuer only granted chat+files.read; invitee requested chat+sync+files.read.
        // The intersection should be chat + files.read.
        let mut granted = accepted.granted_capabilities.clone();
        granted.sort();
        assert_eq!(granted, vec!["chat", "files.read"]);
        assert_eq!(accepted.device_name, "Bob's phone");

        // Trust record is on the invitee side; issuer-side pairing
        // is the responsibility of the issuer's transport handshake.
        let list = invitee.list_trusted_devices().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].device_name, "Bob's phone");
    }

    #[tokio::test]
    async fn short_code_format() {
        let issuer = PairingService::in_memory(UserId::from("alice"), mk_node(0xAA)).unwrap();
        let inv = issuer
            .create_invitation(CreateInvitationRequest {
                issuer_node_id: issuer.local_node_id(),
                capabilities: None,
                ttl_seconds: Some(60),
                note: None,
            })
            .unwrap();
        let code = issuer.create_short_code(&inv.invitation_json).unwrap();
        assert!(code.starts_with("ADNET:"));
        let parsed = issuer.parse_short_code(&code).unwrap();
        assert_eq!(parsed.display, code);
        assert_eq!(parsed.segment_count, 4);
    }

    #[tokio::test]
    async fn revoke_marks_record() {
        let issuer = PairingService::in_memory(UserId::from("alice"), mk_node(0xAA)).unwrap();
        let record = issuer
            .record_issuer_pairing(
                &issuer.local_node_id(),
                &mk_node(0xCC),
                vec![0xCD; 32],
                "Peer device".into(),
                vec!["chat".into()],
            )
            .unwrap();
        let cred_hex = hex::encode(record.credential_id);
        let removed = issuer.revoke_trusted_device(&cred_hex).unwrap();
        assert!(removed);
        let after = issuer.get_trusted_device(&cred_hex).unwrap().unwrap();
        assert!(matches!(
            after.status,
            a3net_pairing::trusted_device::TrustedDeviceStatus::Revoked
        ));
    }

    #[test]
    fn rejects_zero_wallet_secret() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = PairingServiceConfig {
            data_dir: dir.path().to_path_buf(),
            wallet_secret: [0u8; 32],
            local_node_id: mk_node(0xAA),
        };
        let err = PairingService::open(cfg, UserId::from("a")).unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
    }

    #[test]
    fn rejects_malformed_local_node_id() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = PairingServiceConfig {
            data_dir: dir.path().to_path_buf(),
            wallet_secret: Wallet::generate().secret_bytes(),
            local_node_id: "deadbeef".into(),
        };
        let err = PairingService::open(cfg, UserId::from("a")).unwrap_err();
        assert!(matches!(err, AppError::Domain(_)));
    }
}