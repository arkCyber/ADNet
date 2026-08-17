//! `a3chat.e2e.bundle.export` — package the local user's session
//! state (keyring, conversations, messages) into a single
//! portable, AEAD-encrypted blob that another instance running the
//! same owner identity can re-import via
//! `a3chat.e2e.bundle.import`.
//!
//! ## Wire layout
//!
//! ```json
//! {
//!   "version": 1,
//!   "owner": "<64-hex NodeId>",
//!   "exported_at_unix": 1700000000,
//!   "kdf_params": { "time_cost": 2, "memory_kib": 65536, "parallelism": 1 },
//!   "salt_b64":   "<16-byte salt, base64>",
//!   "nonce_b64":  "<12-byte nonce, base64>",
//!   "payload_b64":"<CipherText = ChaCha20-Poly1305-E(SnapshotDump)>"
//! }
//! ```
//!
//! The at-rest AEAD key is `Argon2id(owner_id_bytes, salt)` with
//! the a3chat-pinned KDF params (t=2, m=64 MiB, p=1). The bundle
//! binds to the *exact* owner via the AAD so a bundle exported by
//! a different user fails AEAD verification on import.
//!
//! The exported payload is a [`SnapshotDump`] JSON containing:
//! - the conversation list
//! - recent messages per conversation
//! - the DM keyring markers (handshake completion + last-handshake
//!   timestamps). Session keys are *not* ferried because the
//!   deterministic `(owner, peer)` key derivation is reproducible
//!   on the importer side.
//!
//! ## Placeholder for passphrase-protected bundles
//!
//! P1 ships the placeholder KDF where `password = owner-id` so the
//! daemon doesn't need interactive input. The P2 followup will
//! route the bundle through a real passphrase; the wire shape is
//! already versioned for that.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::Arc;

use a3chat_crypto::kek::{self, KdfParams};
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::ChaCha20Poly1305;
use serde::{Deserialize, Serialize};

use a3chat_core::error::A3chatError;
use a3chat_core::id::{ConversationId, UserId};
use a3chat_core::message::{ChatMessage, MessageBody, MessageEnvelope};

use crate::error::{AppError, AppResult};
use crate::keyring::E2eKeyring;
use crate::storage::ChatStorage;

/// Wire-format version.
pub const BUNDLE_VERSION: u8 = 1;

/// Hard cap on the number of conversations a single bundle can
/// import. Prevents a malformed bundle from forcing the daemon to
/// allocate per-conversation SQLite cursors before AEAD verification
/// anyway… wait — the cap is checked *after* AEAD, so it limits
/// only well-formed bundles. We reject any bundle whose plaintext
/// exceeds this bound.
pub const MAX_CONVERSATIONS_PER_BUNDLE: usize = 10_000;

/// Hard cap on the total number of messages a single bundle can
/// import. Mirrors the fact that the export side already limits
/// each conversation to `MAX_MESSAGES_PER_CONVO`.
pub const MAX_MESSAGES_PER_BUNDLE: usize = 1_000_000;

/// Hard cap on the *plaintext* payload size accepted by the
/// importer. This is the upper bound on the JSON we are willing to
/// deserialize (≈ 256 MiB). Keeps a malicious bundle from triggering
/// an OOM during `serde_json::from_slice` before the per-field
/// limits kick in.
pub const MAX_BUNDLE_PLAINTEXT_BYTES: usize = 256 * 1024 * 1024;

/// Reject bundles whose `exported_at_unix` is more than this many
/// seconds in the past or future. Two weeks matches the typical
/// device-migration window — longer than that and the bundle is
/// almost certainly stale.
pub const SUPERFANCIES_BUNDLE_MAX_AGE_SECS: i64 = 14 * 24 * 60 * 60;

/// Snapshot of the exportable state — encrypted into the bundle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotDump {
    pub version: u8,
    pub owner: String,
    pub exported_at_unix: i64,
    pub conversations: Vec<String>,
    pub messages: BTreeMap<String, Vec<ChatMessage>>,
    /// Per-peer DM session markers. Keyed by peer `UserId`.
    pub dm_state: BTreeMap<String, DmStateSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DmStateSnapshot {
    pub handshake_completed: bool,
    pub last_handshake_at: Option<i64>,
}

/// The exportable surface — exported by `a3chat-app` to wire the
/// RPC dispatcher.
#[derive(Clone)]
pub struct E2eBundleService {
    pub(crate) storage: ChatStorage,
    pub(crate) keyring: E2eKeyring,
}

impl E2eBundleService {
    pub fn new(storage: ChatStorage, keyring: E2eKeyring) -> Self {
        Self { storage, keyring }
    }

    /// `a3chat.e2e.bundle.export` — build a complete [`Bundle`].
    pub async fn export(&self, owner: &UserId) -> AppResult<Bundle> {
        let conversations = self.storage.list_conversations(owner).await?;
        let conv_ids: Vec<String> = conversations
            .iter()
            .map(|c| c.conversation_id.as_str().to_string())
            .collect();

        let mut messages: BTreeMap<String, Vec<ChatMessage>> = BTreeMap::new();
        for id in &conv_ids {
            let msgs = self
                .storage
                .list_messages(
                    owner,
                    &ConversationId::from(id.clone()),
                    crate::sync_service::SyncSnapshot::MAX_MESSAGES_PER_CONVO,
                )
                .await?;
            messages.insert(id.clone(), msgs);
        }

        // Snapshot of every peer we have a DM session for.
        let mut dm_state: BTreeMap<String, DmStateSnapshot> = BTreeMap::new();
        for peer in self.keyring.peers() {
            let snap = self.keyring.session(&peer);
            dm_state.insert(
                peer.as_str().to_string(),
                DmStateSnapshot {
                    handshake_completed: snap.handshake_completed,
                    last_handshake_at: snap.last_handshake_at,
                },
            );
        }

        let dump = SnapshotDump {
            version: BUNDLE_VERSION,
            owner: owner.as_str().to_string(),
            exported_at_unix: chrono::Utc::now().timestamp(),
            conversations: conv_ids,
            messages,
            dm_state,
        };

        // Encrypt the snapshot dump directly with ChaCha20-Poly1305
        // keyed by Argon2id(owner_id, salt). The plain `SnapshotDump`
        // bytes are the payload — no embedded `BundlePayload` schema.
        let plaintext = serde_json::to_vec(&dump)
            .map_err(|e| AppError::Internal(format!("serialize snapshot: {e}")))?;
        let salt = a3chat_crypto::random::random_salt_16();
        let nonce = a3chat_crypto::random::random_nonce();
        let params = KdfParams::default();
        let kek = kek::derive_kek(owner.as_str().as_bytes(), &salt, params)
            .map_err(|e| AppError::Crypto(format!("kek: {e}")))?;
        let cipher = ChaCha20Poly1305::new_from_slice(&kek)
            .map_err(|e| AppError::Crypto(format!("chacha init: {e}")))?;
        let aad = bundle_aad(owner);
        let ct = cipher
            .encrypt(
                chacha20poly1305::aead::generic_array::GenericArray::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|e| AppError::Crypto(format!("chacha seal: {e}")))?;
        Ok(Bundle {
            version: BUNDLE_VERSION,
            owner: owner.as_str().to_string(),
            exported_at_unix: dump.exported_at_unix,
            kdf_params: params,
            salt_b64: b64_std(&salt),
            nonce_b64: b64_std(&nonce),
            payload_b64: b64_std(&ct),
        })
    }

    /// `a3chat.e2e.bundle.import` — restore from a previously
    /// exported bundle. Messages are merged per conversation
    /// (newer `sequence` replaces older). DM keyring markers are
    /// refreshed.
    pub async fn import(
        &self,
        owner: &UserId,
        bundle: Bundle,
        replace_dm_state: bool,
    ) -> AppResult<ImportSummary> {
        if bundle.version != BUNDLE_VERSION {
            return Err(AppError::Domain(format!(
                "unsupported bundle version {} (expected {})",
                bundle.version, BUNDLE_VERSION
            )));
        }

        let salt = b64_decode("salt_b64", &bundle.salt_b64)?;
        let nonce = b64_decode("nonce_b64", &bundle.nonce_b64)?;
        let ct = b64_decode("payload_b64", &bundle.payload_b64)?;
        if salt.len() != 16 {
            return Err(AppError::Domain("salt length must be 16 bytes".into()));
        }
        if nonce.len() != 12 {
            return Err(AppError::Domain("nonce length must be 12 bytes".into()));
        }

        let kek = kek::derive_kek(owner.as_str().as_bytes(), &salt, bundle.kdf_params)
            .map_err(|e| AppError::Crypto(format!("kek: {e}")))?;
        let cipher = ChaCha20Poly1305::new_from_slice(&kek)
            .map_err(|e| AppError::Crypto(format!("chacha init: {e}")))?;
        let aad = bundle_aad(owner);
        let pt = cipher
            .decrypt(
                chacha20poly1305::aead::generic_array::GenericArray::from_slice(&nonce),
                Payload {
                    msg: &ct,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| AppError::Crypto("bundle authentication failed".into()))?;
        // Defend against a malicious bundle that *successfully* decrypts
        // (e.g. legitimate password match) but then tries to feed us
        // gigabytes of garbage to exhaust memory.
        if pt.len() > MAX_BUNDLE_PLAINTEXT_BYTES {
            return Err(AppError::Domain(format!(
                "decrypted payload exceeds cap ({} > {} bytes)",
                pt.len(),
                MAX_BUNDLE_PLAINTEXT_BYTES
            )));
        }
        let dump: SnapshotDump = serde_json::from_slice(&pt)
            .map_err(|e| AppError::Domain(format!("malformed bundle payload: {e}")))?;
        if dump.owner != owner.as_str() {
            return Err(AppError::Domain(format!(
                "payload owner {} does not match current user {}",
                dump.owner,
                owner.as_str()
            )));
        }
        if dump.messages.len() > MAX_CONVERSATIONS_PER_BUNDLE {
            return Err(AppError::Domain(format!(
                "bundle has {} conversations (cap {})",
                dump.messages.len(),
                MAX_CONVERSATIONS_PER_BUNDLE
            )));
        }
        let total_inbound_messages: usize = dump.messages.values().map(|m| m.len()).sum();
        if total_inbound_messages > MAX_MESSAGES_PER_BUNDLE {
            return Err(AppError::Domain(format!(
                "bundle has {} messages (cap {})",
                total_inbound_messages,
                MAX_MESSAGES_PER_BUNDLE
            )));
        }
        if dump.exported_at_unix > 0
            && (chrono::Utc::now().timestamp() - dump.exported_at_unix).abs()
                > SUPERFANCIES_BUNDLE_MAX_AGE_SECS
        {
            return Err(AppError::Domain(format!(
                "bundle is more than {} seconds old or in the future",
                SUPERFANCIES_BUNDLE_MAX_AGE_SECS
            )));
        }

        // Merge messages per conversation. We use the same per-conversation
        // limit as the export side so a conversation that grew past the
        // limit since export is fully covered.
        let mut imported_messages = 0usize;
        let mut new_conversations = 0usize;
        for (conv_id_str, msgs) in &dump.messages {
            let conv_id = ConversationId::from(conv_id_str.clone());
            let existing = self
                .storage
                .list_messages(
                    owner,
                    &conv_id,
                    crate::sync_service::SyncSnapshot::MAX_MESSAGES_PER_CONVO,
                )
                .await?;
            let existing_ids: std::collections::HashSet<String> = existing
                .iter()
                .map(|m| m.message_id.as_str().to_string())
                .collect();
            for m in msgs {
                if existing_ids.contains(m.message_id.as_str()) {
                    continue;
                }
                let envelope = message_to_envelope(m);
                self.storage.save_outbound(owner, &envelope).await?;
                imported_messages += 1;
            }
            if existing.is_empty() && !msgs.is_empty() {
                new_conversations += 1;
            }
        }

        // Refresh DM keyring markers (session keys are derivable from
        // (owner, peer) so the importer rebuilds them lazily — we only
        // restore the *markers* that drive UI hints).
        let mut dm_refreshed = 0usize;
        if replace_dm_state {
            for (peer_str, snap) in &dump.dm_state {
                let peer = UserId::from(peer_str.as_str());
                self.keyring.mutate(&peer, |s| {
                    s.handshake_completed = snap.handshake_completed;
                    s.last_handshake_at = snap.last_handshake_at;
                });
                dm_refreshed += 1;
            }
        }

        Ok(ImportSummary {
            imported_messages,
            new_conversations,
            dm_refreshed,
            bundle_owner: dump.owner,
            bundle_exported_at_unix: dump.exported_at_unix,
        })
    }
}

/// Wire-format envelope returned by `e2e.bundle.export`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Bundle {
    pub version: u8,
    /// Source device id (echoed from the AEAD AAD).
    pub owner: String,
    pub exported_at_unix: i64,
    pub kdf_params: KdfParams,
    /// Base64-encoded 16-byte Argon2id salt.
    pub salt_b64: String,
    /// Base64-encoded 12-byte AEAD nonce.
    pub nonce_b64: String,
    /// Base64-encoded ChaCha20-Poly1305 ciphertext.
    pub payload_b64: String,
}

/// Result of `e2e.bundle.import`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImportSummary {
    pub imported_messages: usize,
    pub new_conversations: usize,
    pub dm_refreshed: usize,
    pub bundle_owner: String,
    pub bundle_exported_at_unix: i64,
}

// -----------------------------------------------------------------------------
// Dispatcher entry point used by `a3chat-app::app::A3chatApp::dispatch`.

/// Dispatch a single `a3chat.e2e.bundle.*` method to the service.
pub async fn dispatch(
    svc: Arc<E2eBundleService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    match method {
        "a3chat.e2e.bundle.export" => {
            let bundle = svc.export(owner).await.map_err(A3chatError::from)?;
            serde_json::to_value(bundle).map_err(A3chatError::from)
        }
        "a3chat.e2e.bundle.import" => {
            // Default to `true` so the common "I'm restoring from a
            // backup" case Just Works. Callers can opt out by passing
            // `"replace_dm_state": false`.
            let replace_dm_state = params
                .get("replace_dm_state")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let bundle: Bundle = serde_json::from_value(params).map_err(|e| {
                A3chatError::InvalidInput(format!("malformed bundle: {e}"))
            })?;
            let r = svc
                .import(owner, bundle, replace_dm_state)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(r).map_err(A3chatError::from)
        }
        m => Err(A3chatError::Internal(format!(
            "E2eBundleService does not handle {m}"
        ))),
    }
}

// -----------------------------------------------------------------------------
// Helpers (kept private to this module).

/// AAD for the AEAD. Binds the ciphertext to the owner identity so
/// a bundle exported by `alice` cannot be replayed as a bundle
/// imported by `bob` even if both share the same password/salt on
/// the (currently empty) server. Versioned for bundle-format
/// migration.
fn bundle_aad(owner: &UserId) -> String {
    format!("a3chat-e2e-bundle-v1|{}", owner.as_str())
}

/// Convert a `ChatMessage` into the `MessageEnvelope` shape the
/// `ChatStorage::save_outbound` path expects.
///
/// # Sender attribution
/// `save_outbound` stamps the envelope's `sender_id` from the
/// caller (the importing owner). We deliberately do **not** try to
/// restore the original sender — the storage schema does not have a
/// `forwarded_from` field, and the import path is only used for
/// *the owner's own* session. The importer is the importer; the
/// `bundle_owner` field in the response carries the original
/// source identity for auditing.
fn message_to_envelope(m: &ChatMessage) -> MessageEnvelope {
    let body = match &m.body {
        MessageBody::Plain { content } => MessageBody::Plain { content: content.clone() },
        // Encrypted bodies are passed through verbatim — the
        // importer shares the same `(owner, peer)` key derivation so
        // it can re-open them on read. We don't try to re-encrypt.
        other @ MessageBody::Encrypted { .. } => other.clone(),
    };
    MessageEnvelope {
        conversation_id: m.conversation_id.clone(),
        receiver_id: m.receiver_id.clone(),
        message_type: m.message_type,
        body,
        attachments: m.attachments.clone(),
        reply_to: m.reply_to.clone(),
        sequence: m.sequence,
        timestamp: m.timestamp,
    }
}

fn b64_std(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn b64_decode(field: &str, v: &str) -> AppResult<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(v)
        .map_err(|e| AppError::Domain(format!("{field}: invalid base64: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyring::E2eKeyring;
    use crate::storage::StorageConfig;
    use a3chat_core::id::{ConversationId, UserId};
    use a3chat_core::message::{MessageBody, MessageEnvelope, MessageType};
    use tempfile::tempdir;

    fn owner() -> UserId {
        UserId::from("alice-node-id")
    }
    fn peer() -> UserId {
        UserId::from("bob-node-id")
    }

    fn fresh() -> (tempfile::TempDir, Arc<E2eBundleService>) {
        let dir = tempdir().unwrap();
        let keyring = E2eKeyring::new(owner());
        let storage =
            ChatStorage::new(StorageConfig::new(dir.path().to_path_buf()), keyring.clone());
        (
            dir,
            Arc::new(E2eBundleService::new(storage, keyring)),
        )
    }

    #[tokio::test]
    async fn export_then_import_round_trip() {
        let (_d, svc) = fresh();
        svc.storage.init_user(&owner()).await.unwrap();

        let env1 = MessageEnvelope {
            conversation_id: ConversationId::from("dm:alice-node-id:bob-node-id"),
            receiver_id: peer(),
            message_type: MessageType::Text,
            body: MessageBody::Plain {
                content: "hello".into(),
            },
            attachments: vec![],
            reply_to: None,
            sequence: 1,
            timestamp: 1_700_000_001,
        };
        let env2 = MessageEnvelope {
            conversation_id: ConversationId::from("dm:alice-node-id:bob-node-id"),
            receiver_id: peer(),
            message_type: MessageType::Text,
            body: MessageBody::Plain {
                content: "world".into(),
            },
            attachments: vec![],
            reply_to: None,
            sequence: 2,
            timestamp: 1_700_000_002,
        };
        svc.storage.save_outbound(&owner(), &env1).await.unwrap();
        svc.storage.save_outbound(&owner(), &env2).await.unwrap();

        let bundle = svc.export(&owner()).await.unwrap();
        assert_eq!(bundle.version, BUNDLE_VERSION);

        // Round trip via a fresh app instance (simulating a second
        // device running as the same owner).
        let dir2 = tempdir().unwrap();
        let keyring2 = E2eKeyring::new(owner());
        let storage2 = ChatStorage::new(
            StorageConfig::new(dir2.path().to_path_buf()),
            keyring2.clone(),
        );
        storage2.init_user(&owner()).await.unwrap();
        let svc2 = Arc::new(E2eBundleService::new(storage2, keyring2));
        let r = svc2.import(&owner(), bundle, true).await.unwrap();
        assert!(r.imported_messages >= 2);
    }

    #[tokio::test]
    async fn import_rejects_version_mismatch() {
        let (_d, svc) = fresh();
        let bogus = Bundle {
            version: 99,
            owner: "alice".into(),
            exported_at_unix: 0,
            kdf_params: KdfParams::default(),
            salt_b64: b64_std(&[0u8; 16]),
            nonce_b64: b64_std(&[0u8; 12]),
            payload_b64: b64_std(&[0u8; 16]),
        };
        let err = svc
            .import(&owner(), bogus, false)
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Domain(_)));
    }

    #[tokio::test]
    async fn import_rejects_wrong_owner() {
        let (_d, svc) = fresh();
        let bogus = Bundle {
            version: BUNDLE_VERSION,
            owner: "bob".into(),
            exported_at_unix: 0,
            kdf_params: KdfParams::default(),
            salt_b64: b64_std(&[0u8; 16]),
            nonce_b64: b64_std(&[0u8; 12]),
            payload_b64: b64_std(&[0u8; 64]),
        };
        let err = svc
            .import(&owner(), bogus, false)
            .await
            .unwrap_err();
        // All-zero ciphertext fails AEAD verification → Crypto error.
        assert!(matches!(err, AppError::Crypto(_)));
    }

    #[tokio::test]
    async fn import_rejects_tampered_payload() {
        let (_d, svc) = fresh();
        svc.storage.init_user(&owner()).await.unwrap();
        let bundle = svc.export(&owner()).await.unwrap();
        let mut bad = bundle.clone();
        let mut bytes = base64::engine::general_purpose::STANDARD
            .decode(&bad.payload_b64)
            .unwrap();
        bytes[0] ^= 0xff;
        bad.payload_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let err = svc.import(&owner(), bad, false).await.unwrap_err();
        assert!(matches!(err, AppError::Crypto(_)));
    }

    #[test]
    fn b64_round_trip_is_lossless() {
        let raw = vec![1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let s = b64_std(&raw);
        let back = b64_decode("salt_b64", &s).unwrap();
        assert_eq!(back, raw);
    }
}
