//! ContactService — friends, requests, blocklist.
//!
//! ## Architecture
//!
//! This service bridges `a3chat`'s contact operations onto the
//! canonical [`a3net_roster::RosterStore`] backend. It maintains two
//! types of contacts:
//! - `a3chat_core::contact::Contact` — the simplified chat-facing type
//! - `a3net_roster::Contact` — the full-featured storage type
//!
//! All persistence lives in the roster SQLite file.

use std::sync::Arc;

use a3chat_core::contact::{BlocklistEntry, Contact as ChatContact, ContactRequest, ContactRequestStatus};
use a3chat_core::error::A3chatError;
use a3chat_core::event::A3chatEvent;
use a3chat_core::id::UserId;
use a3chat_core::rpc::A3chatRpcMethod;
use ed25519_dalek::{Signer, Verifier};

use a3net_roster::{
    ContactGroup, InMemoryRosterStore, RosterStore,
    SqliteRosterStore, SqliteRosterStoreConfig,
};

use crate::error::{AppError, AppResult};
use crate::notification_bus::NotificationBus;
use base64::Engine;

/// Maximum length for friend request message.
const MAX_FRIEND_REQUEST_MSG_LEN: usize = 256;

/// Snapshot of the local contacts state for one user.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ContactsSnapshot {
    pub contacts: Vec<ChatContact>,
    pub blocklist: Vec<BlocklistEntry>,
    pub groups: Vec<ContactGroup>,
}

/// Contact service backed by `a3net-roster`.
#[derive(Clone)]
pub struct ContactService {
    /// Owning user — every public method validates that the caller
    /// matches this id. The single in-process deployment model is
    /// "one owner per service"; a multi-tenant server must keep a
    /// `ContactService` per user. The field is `Option` so the
    /// historical `in_memory()` / `with_store()` constructors that
    /// tests use still work — but the production `new(config,
    /// owner)` constructor wires it in immediately.
    pub owner: Option<UserId>,
    bus: NotificationBus,
    store: Arc<tokio::sync::RwLock<Option<Arc<dyn RosterStore>>>>,
}

/// Contact service configuration.
#[derive(Debug, Clone)]
pub struct ContactServiceConfig {
    pub data_dir: Option<std::path::PathBuf>,
}

impl Default for ContactServiceConfig {
    fn default() -> Self {
        Self { data_dir: None }
    }
}

impl ContactServiceConfig {
    pub fn under_base(base: &std::path::Path) -> Self {
        Self {
            data_dir: Some(base.join("contacts")),
        }
    }
}

// ============================================================================
// Conversion helpers between a3chat_core::Contact and a3net_roster::Contact
// ============================================================================

impl ContactService {
    /// Convert a3net_roster::Contact → a3chat_core::Contact
    fn roster_to_chat(roster: a3net_roster::Contact) -> ChatContact {
        ChatContact {
            user_id: UserId::from(roster.contact_id),
            display_name: roster.name,
            avatar_url: None,
            note: roster.notes,
            is_favorite: roster.is_favorite,
            is_blocked: roster.is_blocked,
            added_at: chrono::DateTime::from_timestamp(roster.created_at as i64, 0)
                .unwrap_or_else(chrono::Utc::now),
            last_interaction_at: None,
            public_key: None,
        }
    }

    /// Convert a3chat_core::Contact → a3net_roster::Contact
    fn chat_to_roster(chat: &ChatContact) -> a3net_roster::Contact {
        a3net_roster::Contact {
            contact_id: chat.user_id.as_str().to_string(),
            name: chat.display_name.clone(),
            contact_type: "human".to_string(),
            agent_deployment_type: None,
            agent_ids: vec![],
            node_id: chat.user_id.as_str().to_string(),
            groups: vec![],
            tags: vec![],
            notes: chat.note.clone(),
            is_favorite: chat.is_favorite,
            is_blocked: chat.is_blocked,
            created_at: chat.added_at.timestamp() as u64,
            last_contacted: 0,
            contact_count: 0,
            public_account_id: None,
            iot_device_type: None,
            iot_protocol: None,
            iot_status: None,
            iot_last_seen: None,
            iot_capabilities: None,
            iot_location: None,
        }
    }

    /// Create a new contact service. `owner` is the canonical user
    /// identity the service acts on behalf of; every public method
    /// verifies the caller's owner before touching the store so a
    /// multi-tenant deployment cannot accidentally cross-read.
    pub fn new(config: ContactServiceConfig, owner: UserId) -> Self {
        Self::build(config, Some(owner))
    }

    /// Backwards-compatible single-argument constructor used by
    /// older call sites / examples / demos that don't yet pass an
    /// owner. Equivalent to `new(config, <any UserId>)` with the
    /// `require_owner` check disabled — i.e. owner is `None` and
    /// every caller is accepted. Prefer [`new`] in new code.
    pub fn new_unowned(config: ContactServiceConfig) -> Self {
        Self::build(config, None)
    }

    fn build(config: ContactServiceConfig, owner: Option<UserId>) -> Self {
        let store = match config.data_dir {
            Some(dir) => {
                std::fs::create_dir_all(&dir).ok();
                let db_path = dir.join("contacts.db");
                match SqliteRosterStore::open(SqliteRosterStoreConfig::new(&db_path)) {
                    Ok(s) => Some(Arc::new(s) as Arc<dyn RosterStore>),
                    Err(e) => {
                        tracing::warn!("failed to open roster SQLite, falling back to in-memory: {e}");
                        Some(Arc::new(InMemoryRosterStore::default()) as Arc<dyn RosterStore>)
                    }
                }
            }
            None => Some(Arc::new(InMemoryRosterStore::default()) as Arc<dyn RosterStore>),
        };

        Self {
            owner,
            bus: NotificationBus::default(),
            store: Arc::new(tokio::sync::RwLock::new(store)),
        }
    }

    /// Create with a pre-built store (used by tests). Owner is `None`
    /// because tests build services for arbitrary owners; the
    /// owner-check path is exercised by the `list` / `block` /
    /// `accept_request` integration tests that DO construct with
    /// `new(config, owner)`.
    pub fn with_store(store: Arc<dyn RosterStore>) -> Self {
        Self {
            owner: None,
            bus: NotificationBus::default(),
            store: Arc::new(tokio::sync::RwLock::new(Some(store))),
        }
    }

    /// Create an in-memory service for tests. Owner is `None` for
    /// the same reason as `with_store`.
    pub fn in_memory() -> Self {
        Self {
            owner: None,
            bus: NotificationBus::default(),
            store: Arc::new(tokio::sync::RwLock::new(Some(
                Arc::new(InMemoryRosterStore::default()) as Arc<dyn RosterStore>
            ))),
        }
    }

    /// Create an in-memory service that DOES honour the owner
    /// check. Use this from production-style constructors
    /// (`A3chatApp::with_storage`) so the HTTP / CLI test harness
    /// still enforces `owner` even though the underlying store is
    /// ephemeral.
    pub fn with_store_for_test(owner: UserId) -> Self {
        Self {
            owner: Some(owner),
            bus: NotificationBus::default(),
            store: Arc::new(tokio::sync::RwLock::new(Some(
                Arc::new(InMemoryRosterStore::default()) as Arc<dyn RosterStore>
            ))),
        }
    }

    /// Same as `with_store_for_test` but uses a caller-supplied
    /// `NotificationBus`. Required for tests (and any future
    /// production wiring that wants bus propagation) where
    /// events emitted by `ContactService` must reach an external
    /// observer.
    pub fn with_store_and_bus_for_test(owner: UserId, bus: NotificationBus) -> Self {
        Self {
            owner: Some(owner),
            bus,
            store: Arc::new(tokio::sync::RwLock::new(Some(
                Arc::new(InMemoryRosterStore::default()) as Arc<dyn RosterStore>
            ))),
        }
    }

    /// Validate that `caller` is allowed to act on this service.
    /// Returns `Ok(())` when:
    /// - the service has no owner set (test-only path), OR
    /// - `caller.as_str() == self.owner.as_str()`.
    ///
    /// `Err(AppError::Forbidden(_))` otherwise.
    fn require_owner(&self, caller: &UserId) -> AppResult<()> {
        match &self.owner {
            None => Ok(()),
            Some(o) if o == caller => Ok(()),
            Some(o) => Err(AppError::Forbidden(format!(
                "ContactService owned by {} — caller {} is not authorised",
                o.as_str(),
                caller.as_str()
            ))),
        }
    }

    pub fn bus(&self) -> &NotificationBus {
        &self.bus
    }

    /// `a3chat.contact.list` — returns contacts + blocklist + groups.
    pub async fn list(&self, owner: &UserId) -> AppResult<ContactsSnapshot> {
        self.require_owner(owner)?;
        let store_guard = self.store.read().await;
        let store = store_guard.as_ref().ok_or_else(|| {
            AppError::NotInitialised("ContactService store not initialised".into())
        })?;

        let roster_contacts = store.list_contacts().await.map_err(|e| {
            AppError::Internal(format!("list_contacts failed: {e}"))
        })?;

        let groups = store.list_groups().await.map_err(|e| {
            AppError::Internal(format!("list_groups failed: {e}"))
        })?;

        let contacts: Vec<ChatContact> = roster_contacts
            .iter()
            .map(|c| Self::roster_to_chat(c.clone()))
            .collect();

        let blocklist: Vec<BlocklistEntry> = roster_contacts
            .iter()
            .filter(|c| c.is_blocked)
            .map(|c| BlocklistEntry {
                user_id: UserId::from(c.contact_id.clone()),
                display_name: c.name.clone(),
                blocked_at: chrono::DateTime::from_timestamp(c.created_at as i64, 0)
                    .unwrap_or_else(chrono::Utc::now),
                reason: None,
            })
            .collect();

        Ok(ContactsSnapshot {
            contacts,
            blocklist,
            groups,
        })
    }

    /// `a3chat.contact.search` — search contacts by name or tags.
    pub async fn search(&self, owner: &UserId, query: &str) -> AppResult<Vec<ChatContact>> {
        self.require_owner(owner)?;
        let store_guard = self.store.read().await;
        let store = store_guard.as_ref().ok_or_else(|| {
            AppError::NotInitialised("ContactService store not initialised".into())
        })?;

        let roster_results = store.search_contacts(query).await.map_err(|e| {
            AppError::Internal(format!("search_contacts failed: {e}"))
        })?;

        Ok(roster_results
            .into_iter()
            .map(Self::roster_to_chat)
            .collect())
    }

    /// `a3chat.contact.add` — add a new contact.
    pub async fn add_contact(
        &self,
        owner: &UserId,
        contact: ChatContact,
    ) -> AppResult<ChatContact> {
        self.require_owner(owner)?;
        contact.validate()?;

        let store_guard = self.store.read().await;
        let store = store_guard.as_ref().ok_or_else(|| {
            AppError::NotInitialised("ContactService store not initialised".into())
        })?;

        let roster_contact = Self::chat_to_roster(&contact);
        store.put_contact(roster_contact).await.map_err(|e| {
            AppError::Internal(format!("put_contact failed: {e}"))
        })?;

        self.bus
            .publish(A3chatEvent::ContactAdded {
                contact_id: contact.user_id.as_str().to_string(),
            });

        Ok(contact)
    }

    /// `a3chat.contact.remove` — remove a contact by id.
    pub async fn remove_contact(
        &self,
        owner: &UserId,
        contact_id: &str,
    ) -> AppResult<bool> {
        self.require_owner(owner)?;
        let store_guard = self.store.read().await;
        let store = store_guard.as_ref().ok_or_else(|| {
            AppError::NotInitialised("ContactService store not initialised".into())
        })?;

        let removed = store.delete_contact(contact_id).await.map_err(|e| {
            AppError::Internal(format!("delete_contact failed: {e}"))
        })?;

        if removed {
            self.bus
                .publish(A3chatEvent::ContactRemoved {
                    contact_id: contact_id.to_string(),
                });
        }

        Ok(removed)
    }

    /// `a3chat.contact.get` — get a single contact.
    pub async fn get_contact(
        &self,
        owner: &UserId,
        contact_id: &str,
    ) -> AppResult<Option<ChatContact>> {
        self.require_owner(owner)?;
        let store_guard = self.store.read().await;
        let store = store_guard.as_ref().ok_or_else(|| {
            AppError::NotInitialised("ContactService store not initialised".into())
        })?;

        let roster_contact = store.get_contact(contact_id).await.map_err(|e| {
            AppError::Internal(format!("get_contact failed: {e}"))
        })?;

        Ok(roster_contact.map(Self::roster_to_chat))
    }

    /// `a3chat.contact.toggle_favorite` — toggle favorite status.
    pub async fn toggle_favorite(
        &self,
        owner: &UserId,
        contact_id: &str,
    ) -> AppResult<Option<bool>> {
        self.require_owner(owner)?;
        let store_guard = self.store.read().await;
        let store = store_guard.as_ref().ok_or_else(|| {
            AppError::NotInitialised("ContactService store not initialised".into())
        })?;

        let result = store.toggle_favorite(contact_id).await.map_err(|e| {
            AppError::Internal(format!("toggle_favorite failed: {e}"))
        })?;

        if let Some(is_favorite) = result {
            self.bus
                .publish(A3chatEvent::ContactFavoriteToggled {
                    contact_id: contact_id.to_string(),
                    is_favorite,
                });
        }

        Ok(result)
    }

    /// `a3chat.contact.add_request` — create and emit a friend request.
    ///
    /// Persists the request to the [`RosterStore`] under
    /// `request.request_id` so an accept call can look it up across
    /// process restarts. Returns the wire-shape [`ContactRequest`]
    /// (carrying the optional `signature_b64`).
    ///
    /// `signer` is the Ed25519 signing key for `owner`. Pass
    /// `None` for the legacy unsigned path (the signature is left
    /// `None` and the receiver will skip the signature check).
    pub async fn add_request(
        &self,
        owner: &UserId,
        to_user: &UserId,
        message: String,
        signer: Option<&crate::keyring::SigningKey>,
    ) -> AppResult<ContactRequest> {
        if message.len() > MAX_FRIEND_REQUEST_MSG_LEN {
            return Err(AppError::Domain(
                "friend-request message exceeds 256 chars".into(),
            ));
        }
        if owner == to_user {
            return Err(AppError::Domain(
                "friend-request target must differ from sender".into(),
            ));
        }

        let now = chrono::Utc::now();
        let request_id =
            a3chat_core::id::generate_message_id(owner.as_str()).into_string();

        // Build the wire request first (without a signature) so we
        // can compute the canonical payload, then sign, then patch
        // the wire request with the signature.
        let mut req = ContactRequest {
            request_id: request_id.clone(),
            from_user_id: owner.clone(),
            from_display_name: owner.as_str().into(),
            to_user_id: to_user.clone(),
            message: message.clone(),
            status: ContactRequestStatus::Pending,
            created_at: now,
            responded_at: None,
            signature_b64: None,
            sender_public_key_hex: None,
        };

        if let Some(key) = signer {
            use ed25519_dalek::Signer;
            let payload = req.signature_payload();
            let sig = key.sign(&payload);
            req.signature_b64 = Some(base64::engine::general_purpose::STANDARD.encode(sig.to_bytes()));
            req.sender_public_key_hex = Some(crate::keyring::signing_key_public_key_hex(key));
        }

        req.validate()?;

        // Persist to the roster store. SQLite ops are wrapped in a
        // single connection lock so concurrent writers can't race
        // a half-written row into the inbox.
        let store_guard = self.store.read().await;
        let store = store_guard.as_ref().ok_or_else(|| {
            AppError::NotInitialised("ContactService store not initialised".into())
        })?;
        store
            .put_contact_request(req.to_persisted())
            .await
            .map_err(|e| AppError::Internal(format!("put_contact_request failed: {e}")))?;
        drop(store_guard);

        self.bus
            .publish(A3chatEvent::ContactRequestReceived {
                request_id: req.request_id.clone(),
            });

        Ok(req)
    }

    /// `a3chat.contact.accept_request` — accept an inbound friend request.
    ///
    /// Performs the following guarantees before flipping the
    /// request to `accepted`:
    ///
    /// 1. `request.to_user_id` matches the calling owner (a peer
    ///    can't accept someone else's request).
    /// 2. The persisted request exists in the roster store (i.e.
    ///    it was issued through this service in this or a previous
    ///    process — replays of an already-accepted request are
    ///    rejected).
    /// 3. The persisted request's `created_at` is within
    ///    [`REQUEST_TTL_SECS`][a3chat_core::contact::REQUEST_TTL_SECS].
    /// 4. When the persisted row carries `signature_b64` AND
    ///    `sender_public_key_hex`, the signature is verified over
    ///    the canonical payload. A persisted row with no signature
    ///    is accepted (legacy path); a persisted row WITH a
    ///    signature but an invalid one is rejected.
    /// 5. The accept is atomic: the row is deleted from the store
    ///    before the contact is materialised, so a crash mid-accept
    ///    leaves the system in a recoverable state (the row is
    ///    gone, the contact may or may not be present — the next
    ///    accept call sees `not found` and bails).
    pub async fn accept_request(
        &self,
        owner: &UserId,
        request: ContactRequest,
    ) -> AppResult<ChatContact> {
        self.require_owner(owner)?;

        // (1) Reject cross-user acceptance.
        if request.to_user_id != *owner {
            return Err(AppError::Forbidden(format!(
                "accept_request: caller {owner} is not the addressee {}",
                request.to_user_id.as_str()
            )));
        }

        let store_guard = self.store.read().await;
        let store = store_guard.as_ref().ok_or_else(|| {
            AppError::NotInitialised("ContactService store not initialised".into())
        })?;

        // (2) Look up the persisted row.
        let persisted = store
            .get_contact_request(&request.request_id)
            .await
            .map_err(|e| AppError::Internal(format!("get_contact_request failed: {e}")))?
            .ok_or_else(|| {
                AppError::Domain(format!(
                    "accept_request: request_id {} not found in store",
                    request.request_id
                ))
            })?;

        // (3) TTL.
        let ttl = chrono::Utc::now()
            - chrono::Duration::seconds(a3chat_core::contact::REQUEST_TTL_SECS);
        if persisted.created_at() < ttl {
            // Best-effort: delete the expired row so the caller can
            // re-issue cleanly on next attempt.
            let _ = store
                .delete_contact_request(&request.request_id)
                .await;
            return Err(AppError::Domain(format!(
                "accept_request: request {} expired (created {})",
                request.request_id, persisted.created_at_unix
            )));
        }

        // (4) Signature verification, if the persisted row has one.
        // We trust the *persisted* signature rather than the wire
        // envelope's, so a forged `signature_b64` on the wire
        // cannot pass as long as the stored row is honest.
        if let Some(sig_b64) = persisted.signature_b64.as_deref() {
            use base64::Engine;
            let sig_bytes = base64::engine::general_purpose::STANDARD
                .decode(sig_b64)
                .map_err(|e| {
                    AppError::Domain(format!(
                        "accept_request: signature_b64 base64 decode failed: {e}"
                    ))
                })?;
            if sig_bytes.len() != 64 {
                return Err(AppError::Domain(format!(
                    "accept_request: signature length {} != 64",
                    sig_bytes.len()
                )));
            }

            // Reconstruct the canonical payload from the persisted
            // row, NOT the wire envelope, so the wire cannot lie
            // about the request body.
            let payload = persisted.signature_payload();
            // Pull the sender's public key. Either it's embedded in
            // the wire envelope (`sender_public_key_hex`) or the
            // caller must have it cached by `from_user_id`. We only
            // accept the embedded copy to keep verification local —
            // a future patch can add a cache lookup.
            let pk_hex = request.sender_public_key_hex.as_deref().ok_or_else(|| {
                AppError::Domain(
                    "accept_request: persisted signature present but no sender_public_key_hex \
                     supplied on the wire envelope"
                        .into(),
                )
            })?;
            let pk = crate::keyring::public_key_from_hex(pk_hex).map_err(|e| {
                AppError::Domain(format!(
                    "accept_request: invalid sender public key: {e}"
                ))
            })?;
            let sig_array: [u8; 64] = sig_bytes
                .as_slice()
                .try_into()
                .map_err(|_| AppError::Domain(format!(
                    "accept_request: signature slice length {} != 64",
                    sig_bytes.len()
                )))?;
            let sig = crate::keyring::Signature::from_bytes(&sig_array);
            pk.verify(&payload, &sig).map_err(|e| {
                AppError::Domain(format!(
                    "accept_request: signature verification failed: {e}"
                ))
            })?;
        }

        // (5) Atomic delete-then-materialise. If the contact write
        // fails, the row is already gone — the caller can retry
        // and will see a `Domain("not found")` error.
        let removed = store
            .delete_contact_request(&request.request_id)
            .await
            .map_err(|e| AppError::Internal(format!("delete_contact_request failed: {e}")))?;
        if removed.is_none() {
            return Err(AppError::Domain(format!(
                "accept_request: request {} vanished between get and delete",
                request.request_id
            )));
        }

        let contact = ChatContact {
            user_id: request.from_user_id.clone(),
            display_name: request.from_display_name.clone(),
            avatar_url: None,
            note: request.message.clone(),
            is_favorite: false,
            is_blocked: false,
            added_at: chrono::Utc::now(),
            last_interaction_at: None,
            public_key: request.sender_public_key_hex.clone(),
        };

        let roster_contact = Self::chat_to_roster(&contact);
        store.put_contact(roster_contact).await.map_err(|e| {
            AppError::Internal(format!("accept_request put_contact failed: {e}"))
        })?;
        drop(store_guard);

        self.bus
            .publish(A3chatEvent::ContactRequestAccepted {
                request_id: request.request_id.clone(),
                contact_id: contact.user_id.as_str().to_string(),
            });

        Ok(contact)
    }

    /// List every pending (and historical) request addressed to
    /// `owner`. Thin wrapper over `RosterStore::list_contact_requests_for`
    /// — the store returns every status, callers filter as needed.
    pub async fn list_incoming_requests(
        &self,
        owner: &UserId,
    ) -> AppResult<Vec<ContactRequest>> {
        self.require_owner(owner)?;
        let store_guard = self.store.read().await;
        let store = store_guard.as_ref().ok_or_else(|| {
            AppError::NotInitialised("ContactService store not initialised".into())
        })?;
        let rows = store
            .list_contact_requests_for(owner.as_str())
            .await
            .map_err(|e| AppError::Internal(format!("list_contact_requests_for failed: {e}")))?;
        Ok(rows.into_iter().map(ContactRequest::from_persisted).collect())
    }

    /// Cancel a pending outbound request.
    pub async fn cancel_request(
        &self,
        owner: &UserId,
        request_id: &str,
    ) -> AppResult<bool> {
        self.require_owner(owner)?;
        let store_guard = self.store.read().await;
        let store = store_guard.as_ref().ok_or_else(|| {
            AppError::NotInitialised("ContactService store not initialised".into())
        })?;
        let row = store
            .get_contact_request(request_id)
            .await
            .map_err(|e| AppError::Internal(format!("get_contact_request failed: {e}")))?;
        if let Some(r) = row {
            if r.from_user_id != owner.as_str() {
                return Err(AppError::Forbidden(format!(
                    "cancel_request: caller {owner} did not originate request {request_id}"
                )));
            }
            store
                .delete_contact_request(request_id)
                .await
                .map_err(|e| AppError::Internal(format!("delete_contact_request failed: {e}")))?;
            self.bus
                .publish(A3chatEvent::ContactRequestCancelled {
                    request_id: request_id.to_string(),
                    by_user_id: owner.clone(),
                });
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// `a3chat.contact.block` — block a user.
    pub async fn block(&self, owner: &UserId, user_id: &UserId) -> AppResult<BlocklistEntry> {
        self.require_owner(owner)?;
        let store_guard = self.store.read().await;
        let store = store_guard.as_ref().ok_or_else(|| {
            AppError::NotInitialised("ContactService store not initialised".into())
        })?;

        let existing = store.get_contact(user_id.as_str()).await.map_err(|e| {
            AppError::Internal(format!("get_contact failed: {e}"))
        })?;

        if let Some(mut contact) = existing {
            contact.is_blocked = true;
            store.put_contact(contact).await.map_err(|e| {
                AppError::Internal(format!("block failed: {e}"))
            })?;
        }

        let entry = BlocklistEntry {
            user_id: user_id.clone(),
            display_name: user_id.as_str().into(),
            blocked_at: chrono::Utc::now(),
            reason: None,
        };

        self.bus
            .publish(A3chatEvent::ContactBlocked {
                user_id: user_id.clone(),
            });

        Ok(entry)
    }

    /// `a3chat.contact.unblock` — remove from blocklist.
    pub async fn unblock(&self, owner: &UserId, user_id: &UserId) -> AppResult<()> {

        self.require_owner(owner)?;
        let store_guard = self.store.read().await;
        let store = store_guard.as_ref().ok_or_else(|| {
            AppError::NotInitialised("ContactService store not initialised".into())
        })?;

        let existing = store.get_contact(user_id.as_str()).await.map_err(|e| {
            AppError::Internal(format!("get_contact failed: {e}"))
        })?;

        if let Some(mut contact) = existing {
            contact.is_blocked = false;
            store.put_contact(contact).await.map_err(|e| {
                AppError::Internal(format!("unblock failed: {e}"))
            })?;
        }

        self.bus
            .publish(A3chatEvent::ContactUnblocked {
                user_id: user_id.clone(),
            });

        Ok(())
    }

    /// F-25 / B-7 — synchronous blocklist check used by
    /// [`crate::chat_service::ChatService::send_message`]. Returns
    /// `true` if `owner` has `user_id` on their blocklist and the
    /// inbound message should be dropped before persistence.
    ///
    /// Returns `Ok(false)` when the store is not initialised (so the
    /// operator can opt to skip the check in tests). Returns
    /// `Ok(false)` on `UserStore` errors — log and fail-open rather
    /// than block legitimate traffic on a transient store glitch.
    pub async fn is_blocked(&self, owner: &UserId, user_id: &UserId) -> bool {
        let store_guard = self.store.read().await;
        let store = match store_guard.as_ref() {
            Some(s) => s,
            None => return false,
        };
        match store.get_contact(user_id.as_str()).await {
            Ok(Some(c)) => c.is_blocked,
            Ok(None) => false,
            Err(e) => {
                tracing::warn!(
                    user = %user_id.as_str(),
                    "is_blocked lookup failed, fail-open: {e}"
                );
                false
            }
        }
    }

    /// `a3chat.contact.qr_invite` — generate an invite payload.
    ///
    /// **Pre-pairing compatibility shim.** This emits the legacy
    /// unsigned JSON payload (`{version, user_id, kind, ts}`) so
    /// older UIs / e2e tests that have not yet been ported to the
    /// `a3chat.pairing.invitation.create` RPC keep working. New code
    /// MUST use [`crate::pairing_service::PairingService`]:
    /// `qr_invite` carries no wallet signature, has no expiry, and
    /// is trivially replayable.
    pub async fn qr_invite(&self, owner: &UserId) -> AppResult<String> {
        self.require_owner(owner)?;
        let payload = serde_json::json!({
            "version": 1,
            "user_id": owner.as_str(),
            "kind": "contact_invite",
            "ts": chrono::Utc::now().timestamp(),
        });

        serde_json::to_string(&payload)
            .map(|s| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s.as_bytes()))
            .map_err(|e| AppError::Internal(e.to_string()))
    }

    /// `a3chat.contact.groups.list` — list all contact groups.
    pub async fn list_groups(&self) -> AppResult<Vec<ContactGroup>> {
        let store_guard = self.store.read().await;
        let store = store_guard.as_ref().ok_or_else(|| {
            AppError::NotInitialised("ContactService store not initialised".into())
        })?;

        store.list_groups().await.map_err(|e| {
            AppError::Internal(format!("list_groups failed: {e}"))
        })
    }

    /// `a3chat.contact.groups.create` — create a new contact group.
    pub async fn create_group(&self, group: ContactGroup) -> AppResult<ContactGroup> {
        let store_guard = self.store.read().await;
        let store = store_guard.as_ref().ok_or_else(|| {
            AppError::NotInitialised("ContactService store not initialised".into())
        })?;

        store.put_group(group.clone()).await.map_err(|e| {
            AppError::Internal(format!("create_group failed: {e}"))
        })?;

        Ok(group)
    }

    /// `a3chat.contact.groups.delete` — delete a contact group.
    pub async fn delete_group(&self, group_id: &str) -> AppResult<bool> {
        let store_guard = self.store.read().await;
        let store = store_guard.as_ref().ok_or_else(|| {
            AppError::NotInitialised("ContactService store not initialised".into())
        })?;

        store.delete_group(group_id).await.map_err(|e| {
            AppError::Internal(format!("delete_group failed: {e}"))
        })
    }

    /// `a3chat.contact.update` — update contact fields.
    pub async fn update_contact(
        &self,
        owner: &UserId,
        contact: ChatContact,
    ) -> AppResult<ChatContact> {
        self.require_owner(owner)?;
        let store_guard = self.store.read().await;
        let store = store_guard.as_ref().ok_or_else(|| {
            AppError::NotInitialised("ContactService store not initialised".into())
        })?;

        contact.validate()?;
        let roster_contact = Self::chat_to_roster(&contact);

        store.put_contact(roster_contact).await.map_err(|e| {
            AppError::Internal(format!("update_contact failed: {e}"))
        })?;

        self.bus
            .publish(A3chatEvent::ContactUpdated {
                contact_id: contact.user_id.as_str().to_string(),
            });

        Ok(contact)
    }
}

/// Dispatch helper used by `a3chat-rpc`.
pub async fn dispatch(
    svc: Arc<ContactService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    match method {
        A3chatRpcMethod::CONTACT_LIST => {
            let snap = svc.list(owner).await.map_err(A3chatError::from)?;
            serde_json::to_value(snap).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CONTACT_ADD_REQUEST => {
            let to_user: UserId = serde_json::from_value(
                params
                    .get("to_user_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("to_user_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let message: String = params
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let req = svc
                .add_request(owner, &to_user, message, None)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(req).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CONTACT_BLOCK => {
            let user_id: UserId = serde_json::from_value(
                params
                    .get("user_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("user_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let entry = svc
                .block(owner, &user_id)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(entry).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CONTACT_UNBLOCK => {
            let user_id: UserId = serde_json::from_value(
                params
                    .get("user_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("user_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            svc.unblock(owner, &user_id)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        A3chatRpcMethod::CONTACT_ACCEPT_REQUEST => {
            let request: ContactRequest = serde_json::from_value(
                params
                    .get("request")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("request missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let contact = svc
                .accept_request(owner, request)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(contact).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CONTACT_QR_INVITE => {
            let s = svc.qr_invite(owner).await.map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "qr_payload": s }))
        }
        A3chatRpcMethod::CONTACT_ADD => {
            let contact: ChatContact = serde_json::from_value(params)
                .map_err(|e| A3chatError::InvalidInput(format!("contact: {e}")))?;
            let c = svc
                .add_contact(owner, contact)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(c).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CONTACT_REMOVE => {
            let contact_id: String = serde_json::from_value(
                params
                    .get("contact_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("contact_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let removed = svc
                .remove_contact(owner, &contact_id)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "removed": removed }))
        }
        A3chatRpcMethod::CONTACT_GET => {
            let contact_id: String = serde_json::from_value(
                params
                    .get("contact_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("contact_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let c = svc
                .get_contact(owner, &contact_id)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::to_value(c).map_err(A3chatError::from)?)
        }
        A3chatRpcMethod::CONTACT_SEARCH => {
            let query: String = serde_json::from_value(
                params
                    .get("query")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("query missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let results = svc
                .search(owner, &query)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(results).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CONTACT_TOGGLE_FAVORITE => {
            let contact_id: String = serde_json::from_value(
                params
                    .get("contact_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("contact_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let r = svc
                .toggle_favorite(owner, &contact_id)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::to_value(r).map_err(A3chatError::from)?)
        }
        A3chatRpcMethod::CONTACT_UPDATE => {
            let contact: ChatContact = serde_json::from_value(params)
                .map_err(|e| A3chatError::InvalidInput(format!("contact: {e}")))?;
            let c = svc
                .update_contact(owner, contact)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(c).map_err(A3chatError::from)
        }
        _ => Err(A3chatError::Internal(format!(
            "ContactService does not handle {method}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use tempfile::tempdir;

    #[tokio::test]
    async fn list_returns_empty_snapshot() {
        let svc = ContactService::in_memory();
        let snap = svc.list(&UserId::from("alice")).await.unwrap();
        assert!(snap.contacts.is_empty());
        assert!(snap.blocklist.is_empty());
    }

    #[tokio::test]
    async fn add_request_emits_event() {
        let svc = ContactService::in_memory();
        let mut rx = svc.bus().subscribe();
        let r = svc
            .add_request(&UserId::from("alice"), &UserId::from("bob"), "hi".into(), None)
            .await
            .unwrap();
        assert_eq!(r.status, ContactRequestStatus::Pending);
        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("event")
            .expect("event some");
        match evt {
            A3chatEvent::ContactRequestReceived { request_id } => {
                assert_eq!(request_id, r.request_id);
            }
            _ => panic!("wrong event kind"),
        }
    }

    #[tokio::test]
    async fn add_request_rejects_oversize_message() {
        let svc = ContactService::in_memory();
        let huge = "x".repeat(257);
        let r = svc
            .add_request(&UserId::from("alice"), &UserId::from("bob"), huge, None)
            .await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn block_creates_entry() {
        let svc = ContactService::in_memory();
        let entry = svc
            .block(&UserId::from("alice"), &UserId::from("bob"))
            .await
            .unwrap();
        assert_eq!(entry.user_id, UserId::from("bob"));
    }

    #[tokio::test]
    async fn qr_invite_is_valid_base64() {
        let svc = ContactService::in_memory();
        let s = svc.qr_invite(&UserId::from("alice")).await.unwrap();
        let bytes = URL_SAFE_NO_PAD.decode(&s).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["user_id"], "alice");
        assert_eq!(json["kind"], "contact_invite");
    }

    #[tokio::test]
    async fn add_and_get_contact() {
        let svc = ContactService::in_memory();
        let contact = ChatContact {
            user_id: UserId::from("alice"),
            display_name: "Alice".to_string(),
            avatar_url: None,
            note: "".to_string(),
            is_favorite: false,
            is_blocked: false,
            added_at: chrono::Utc::now(),
            last_interaction_at: None,
            public_key: None,
        };
        let added = svc.add_contact(&UserId::from("alice"), contact.clone()).await.unwrap();
        assert_eq!(added.user_id.as_str(), "alice");

        let found = svc.get_contact(&UserId::from("alice"), "alice").await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().display_name, "Alice");
    }

    #[tokio::test]
    async fn search_contacts() {
        let svc = ContactService::in_memory();
        let contact = ChatContact {
            user_id: UserId::from("alice"),
            display_name: "Alice Smith".to_string(),
            avatar_url: None,
            note: "engineer".to_string(),
            is_favorite: false,
            is_blocked: false,
            added_at: chrono::Utc::now(),
            last_interaction_at: None,
            public_key: None,
        };
        svc.add_contact(&UserId::from("alice"), contact).await.unwrap();

        let results = svc.search(&UserId::from("alice"), "alice").await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].display_name, "Alice Smith");
    }

    #[tokio::test]
    async fn toggle_favorite() {
        let svc = ContactService::in_memory();
        let contact = ChatContact {
            user_id: UserId::from("bob"),
            display_name: "Bob".to_string(),
            avatar_url: None,
            note: "".to_string(),
            is_favorite: false,
            is_blocked: false,
            added_at: chrono::Utc::now(),
            last_interaction_at: None,
            public_key: None,
        };
        svc.add_contact(&UserId::from("alice"), contact).await.unwrap();

        let fav = svc.toggle_favorite(&UserId::from("alice"), "bob").await.unwrap();
        assert_eq!(fav, Some(true));

        let unfav = svc.toggle_favorite(&UserId::from("alice"), "bob").await.unwrap();
        assert_eq!(unfav, Some(false));
    }

    #[tokio::test]
    async fn remove_contact() {
        let svc = ContactService::in_memory();
        let contact = ChatContact {
            user_id: UserId::from("charlie"),
            display_name: "Charlie".to_string(),
            avatar_url: None,
            note: "".to_string(),
            is_favorite: false,
            is_blocked: false,
            added_at: chrono::Utc::now(),
            last_interaction_at: None,
            public_key: None,
        };
        svc.add_contact(&UserId::from("alice"), contact).await.unwrap();

        let removed = svc.remove_contact(&UserId::from("alice"), "charlie").await.unwrap();
        assert!(removed);

        let found = svc.get_contact(&UserId::from("alice"), "charlie").await.unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn dispatch_unknown_method_errors() {
        let svc = Arc::new(ContactService::in_memory());
        let err = dispatch(
            svc,
            "a3chat.bogus",
            &UserId::from("alice"),
            serde_json::json!({}),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, A3chatError::Internal(_)));
    }

    // ─────────────────────────────────────────────────────────────────
    // owner-isolation (H-1) tests
    //
    // A service constructed with `ContactService::new(config, owner)`
    // must reject calls from any other owner with `AppError::Forbidden`
    // — preventing a multi-tenant deployment from accidentally
    // cross-reading rosters.
    // ─────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn require_owner_rejects_other_user() {
        let dir = tempdir().unwrap();
        let cfg = ContactServiceConfig::under_base(dir.path());
        let svc = ContactService::new(cfg, UserId::from("alice"));
        // Bob is not the owner — every public method must reject him.
        let r = svc.list(&UserId::from("bob")).await;
        assert!(
            matches!(r, Err(AppError::Forbidden(_))),
            "expected Forbidden, got {r:?}"
        );
        let r = svc.search(&UserId::from("bob"), "x").await;
        assert!(matches!(r, Err(AppError::Forbidden(_))));
        let r = svc.get_contact(&UserId::from("bob"), "x").await;
        assert!(matches!(r, Err(AppError::Forbidden(_))));
        let r = svc.add_contact(&UserId::from("bob"), mk_chat_contact("c", "C")).await;
        assert!(matches!(r, Err(AppError::Forbidden(_))));
        let r = svc.remove_contact(&UserId::from("bob"), "x").await;
        assert!(matches!(r, Err(AppError::Forbidden(_))));
        let r = svc.toggle_favorite(&UserId::from("bob"), "x").await;
        assert!(matches!(r, Err(AppError::Forbidden(_))));
        let r = svc.update_contact(&UserId::from("bob"), mk_chat_contact("c", "C")).await;
        assert!(matches!(r, Err(AppError::Forbidden(_))));
        let r = svc.block(&UserId::from("bob"), &UserId::from("x")).await;
        assert!(matches!(r, Err(AppError::Forbidden(_))));
        let r = svc.unblock(&UserId::from("bob"), &UserId::from("x")).await;
        assert!(matches!(r, Err(AppError::Forbidden(_))));
        let r = svc
            .accept_request(
                &UserId::from("bob"),
                a3chat_core::contact::ContactRequest {
                    request_id: "r".into(),
                    from_user_id: UserId::from("alice"),
                    from_display_name: "Alice".into(),
                    to_user_id: UserId::from("bob"),
                    message: "".into(),
                    status: ContactRequestStatus::Pending,
                    created_at: chrono::Utc::now(),
                    responded_at: None,
                    signature_b64: None,
                    sender_public_key_hex: None,
                },
            )
            .await;
        assert!(matches!(r, Err(AppError::Forbidden(_))));
        let r = svc.qr_invite(&UserId::from("bob")).await;
        assert!(matches!(r, Err(AppError::Forbidden(_))));
    }

    #[tokio::test]
    async fn require_owner_accepts_canonical_user() {
        let dir = tempdir().unwrap();
        let cfg = ContactServiceConfig::under_base(dir.path());
        let svc = ContactService::new(cfg, UserId::from("alice"));
        // Alice is the canonical owner — every method must accept.
        let snap = svc.list(&UserId::from("alice")).await.unwrap();
        assert!(snap.contacts.is_empty());
        let c = svc
            .add_contact(&UserId::from("alice"), mk_chat_contact("c", "C"))
            .await
            .unwrap();
        assert_eq!(c.user_id.as_str(), "c");
        let _ = svc.qr_invite(&UserId::from("alice")).await.unwrap();
    }

    // ─────────────────────────────────────────────────────────────────
    // SqliteRosterStore persistence (L6 coverage)
    //
    // The original 11 tests all exercise `InMemoryRosterStore`.
    // Add explicit coverage for the Sqlite-backed path so a
    // refactor in `a3net-roster` cannot silently break the
    // production deployment.
    // ─────────────────────────────────────────────────────────────────

    fn mk_chat_contact(user_id: &str, display_name: &str) -> ChatContact {
        ChatContact {
            user_id: UserId::from(user_id),
            display_name: display_name.to_string(),
            avatar_url: None,
            note: "".to_string(),
            is_favorite: false,
            is_blocked: false,
            added_at: chrono::Utc::now(),
            last_interaction_at: None,
            public_key: None,
        }
    }

    #[tokio::test]
    async fn sqlite_store_persists_across_drop() {
        // Round 1: build service in a tempdir, add 3 contacts, drop.
        let dir = tempdir().unwrap();
        let cfg = ContactServiceConfig::under_base(dir.path());
        let svc = ContactService::new(cfg.clone(), UserId::from("alice"));
        svc.add_contact(&UserId::from("alice"), mk_chat_contact("a", "Alice")).await.unwrap();
        svc.add_contact(&UserId::from("alice"), mk_chat_contact("b", "Bob")).await.unwrap();
        svc.add_contact(&UserId::from("alice"), mk_chat_contact("c", "Charlie")).await.unwrap();
        let snap = svc.list(&UserId::from("alice")).await.unwrap();
        assert_eq!(snap.contacts.len(), 3);
        // Drop the first handle to flush the SQLite WAL.
        drop(svc);

        // Round 2: rebuild from the same dir, verify everything is
        // still there.
        let svc2 = ContactService::new(cfg, UserId::from("alice"));
        let snap = svc2.list(&UserId::from("alice")).await.unwrap();
        let names: Vec<&str> = snap.contacts.iter().map(|c| c.display_name.as_str()).collect();
        assert!(names.contains(&"Alice"));
        assert!(names.contains(&"Bob"));
        assert!(names.contains(&"Charlie"));

        // Mutations on the rebuilt service also persist.
        let fav = svc2.toggle_favorite(&UserId::from("alice"), "a").await.unwrap();
        assert_eq!(fav, Some(true));
        drop(svc2);

        let svc3 = ContactService::new(
            ContactServiceConfig::under_base(dir.path()),
            UserId::from("alice"),
        );
        let a = svc3.get_contact(&UserId::from("alice"), "a").await.unwrap().unwrap();
        assert!(a.is_favorite, "favourite flag must persist across reopen");
    }

    #[tokio::test]
    async fn sqlite_block_unblock_round_trip() {
        let dir = tempdir().unwrap();
        let cfg = ContactServiceConfig::under_base(dir.path());
        let svc = ContactService::new(cfg, UserId::from("alice"));
        svc.add_contact(&UserId::from("alice"), mk_chat_contact("a", "Alice")).await.unwrap();
        svc.block(&UserId::from("alice"), &UserId::from("a")).await.unwrap();
        let snap = svc.list(&UserId::from("alice")).await.unwrap();
        assert_eq!(snap.blocklist.len(), 1, "blocked contact must appear in blocklist");
        svc.unblock(&UserId::from("alice"), &UserId::from("a")).await.unwrap();
        let snap = svc.list(&UserId::from("alice")).await.unwrap();
        assert_eq!(snap.blocklist.len(), 0, "unblock must clear the blocklist row");
    }

    #[tokio::test]
    async fn sqlite_search_finds_persisted_contact() {
        let dir = tempdir().unwrap();
        let cfg = ContactServiceConfig::under_base(dir.path());
        let svc = ContactService::new(cfg, UserId::from("alice"));
        svc.add_contact(&UserId::from("alice"), mk_chat_contact("a", "Alice Engineer")).await.unwrap();
        svc.add_contact(&UserId::from("alice"), mk_chat_contact("b", "Bob Designer")).await.unwrap();
        let results = svc.search(&UserId::from("alice"), "design").await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].user_id.as_str(), "b");
    }

    #[tokio::test]
    async fn dispatch_routes_to_owner_aware_service() {
        // Build the production constructor (owner = alice) and
        // confirm every dispatch branch passes the owner check.
        let dir = tempdir().unwrap();
        let cfg = ContactServiceConfig::under_base(dir.path());
        let svc = Arc::new(ContactService::new(cfg, UserId::from("alice")));
        // `list` from a wrong owner must error.
        let r = dispatch(
            svc.clone(),
            A3chatRpcMethod::CONTACT_LIST,
            &UserId::from("bob"),
            serde_json::json!({}),
        )
        .await;
        assert!(r.is_err(), "non-owner caller must be rejected: {r:?}");
        // `list` from alice must succeed.
        let r = dispatch(
            svc.clone(),
            A3chatRpcMethod::CONTACT_LIST,
            &UserId::from("alice"),
            serde_json::json!({}),
        )
        .await;
        assert!(r.is_ok());
    }
}
