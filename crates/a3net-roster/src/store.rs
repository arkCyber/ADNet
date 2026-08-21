//! Store trait + info record for the contact roster.
//!
//! This is the *single* surface other A3Net crates should depend on. Two
//! reference implementations are provided:
//!
//! - [`crate::mem::InMemoryRosterStore`] — `HashMap`-backed, no IO. Useful
//!   for tests and short-lived processes.
//! - [`crate::sqlite::SqliteRosterStore`] — SQLite via `rusqlite` (bundled),
//!   suitable for cross-restart persistence.
//!
//! The trait is intentionally **`async_trait`** so downstream users can hold
//! it behind `Arc<dyn RosterStore>` and call into it from a tokio task
//! without blocking the reactor. Reference implementations expose the same
//! methods through their concrete type (sync under the hood) — async
//! adapters for embedded / WASM profiles can provide their own executor.

use async_trait::async_trait;

use crate::error::RosterResult;
use crate::group::ContactGroup;
use crate::mapping::DigitMapping;
use crate::model::Contact;
use crate::request::PersistedContactRequest;
use crate::settings::FriendRequestSetting;

/// Storage capabilities required by every roster backend.
#[async_trait]
pub trait RosterStore: Send + Sync {
    // ------- Contacts -------

    /// Insert or fully-replace a contact keyed by `contact_id`.
    async fn put_contact(&self, contact: Contact) -> RosterResult<()>;

    /// Remove a contact by id. Returns `Ok(false)` if the contact did not
    /// exist.
    async fn delete_contact(&self, contact_id: &str) -> RosterResult<bool>;

    /// Fetch a contact by id.
    async fn get_contact(&self, contact_id: &str) -> RosterResult<Option<Contact>>;

    /// List every contact (no pagination — caller must apply limits if
    /// they care about memory).
    async fn list_contacts(&self) -> RosterResult<Vec<Contact>>;

    /// Search contacts by case-insensitive substring over name / tags /
    /// notes. Empty query returns the full list.
    async fn search_contacts(&self, query: &str) -> RosterResult<Vec<Contact>>;

    /// Toggle favorite on a contact and return the new value. Returns
    /// `None` if the contact does not exist.
    async fn toggle_favorite(&self, contact_id: &str) -> RosterResult<Option<bool>>;

    /// Toggle blocked on a contact and return the new value. Returns
    /// `None` if the contact does not exist.
    async fn set_blocked(&self, contact_id: &str, blocked: bool) -> RosterResult<Option<bool>>;

    // ------- Groups -------

    async fn put_group(&self, group: ContactGroup) -> RosterResult<()>;
    async fn delete_group(&self, group_id: &str) -> RosterResult<bool>;
    async fn get_group(&self, group_id: &str) -> RosterResult<Option<ContactGroup>>;
    async fn list_groups(&self) -> RosterResult<Vec<ContactGroup>>;

    // ------- Digit ↔ Node mappings -------

    async fn put_digit_mapping(&self, mapping: DigitMapping) -> RosterResult<()>;
    async fn resolve_digit_to_node(&self, digit_id: &str) -> RosterResult<Option<String>>;
    async fn resolve_node_to_digit(&self, node_id: &str) -> RosterResult<Option<String>>;
    async fn list_digit_mappings(&self) -> RosterResult<Vec<DigitMapping>>;

    // ------- Friend-request settings -------

    async fn put_friend_request_setting(
        &self,
        setting: FriendRequestSetting,
    ) -> RosterResult<()>;
    async fn get_friend_request_setting(
        &self,
        user_id: &str,
    ) -> RosterResult<Option<FriendRequestSetting>>;

    // ------- Friend requests (persistence slice) -------
    //
    // Friend requests are owned by the local user. `put_contact_request`
    // is idempotent on `request_id`; `get_contact_request` returns
    // `Ok(None)` when the row is absent; `delete_contact_request` is
    // idempotent and returns the row that was just removed (so the
    // caller can audit "did this accept really consume the row it
    // claimed?"); `list_contact_requests_for` returns every request
    // where `to_user_id == user_id` — used to power an inbox view.
    //
    // Implementations MUST serialise concurrent writes by the same
    // `request_id` to last-writer-wins, NOT reject them: callers
    // update `status` in place.

    /// Insert or fully-replace a contact request keyed by `request_id`.
    async fn put_contact_request(
        &self,
        request: PersistedContactRequest,
    ) -> RosterResult<()>;

    /// Fetch a contact request by id. Returns `Ok(None)` when absent.
    async fn get_contact_request(
        &self,
        request_id: &str,
    ) -> RosterResult<Option<PersistedContactRequest>>;

    /// Remove a contact request by id. Returns the row that was
    /// removed, or `Ok(None)` if it didn't exist.
    async fn delete_contact_request(
        &self,
        request_id: &str,
    ) -> RosterResult<Option<PersistedContactRequest>>;

    /// List every contact request addressed to `user_id`, regardless
    /// of status. The caller is responsible for filtering by status
    /// (e.g. to render an inbox of pending requests).
    async fn list_contact_requests_for(
        &self,
        user_id: &str,
    ) -> RosterResult<Vec<PersistedContactRequest>>;
}

/// Information a store exposes about its backing medium. Useful for
/// diagnostics and for UI surfaces that want to display "where is my data".
#[derive(Debug, Clone)]
pub struct RosterStoreInfo {
    /// Human-readable backend name, e.g. `"sqlite"` or `"memory"`.
    pub backend: &'static str,
    /// Path / identifier of the underlying storage, when applicable.
    pub location: Option<String>,
    /// Number of contacts currently stored.
    pub contact_count: usize,
    /// Number of contact groups currently stored.
    pub group_count: usize,
    /// Number of digit mappings currently stored.
    pub digit_mapping_count: usize,
}

impl RosterStoreInfo {
    pub fn new(backend: &'static str) -> Self {
        Self {
            backend,
            location: None,
            contact_count: 0,
            group_count: 0,
            digit_mapping_count: 0,
        }
    }
}