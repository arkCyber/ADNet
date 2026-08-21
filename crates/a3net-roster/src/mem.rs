//! `HashMap`-backed in-memory [`RosterStore`] implementation.
//!
//! Useful for tests and short-lived processes. No IO happens — process
//! restart loses all data.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use tracing::warn;

use crate::error::{RosterError, RosterResult};
use crate::group::ContactGroup;
use crate::mapping::DigitMapping;
use crate::model::Contact;
use crate::request::PersistedContactRequest;
use crate::settings::FriendRequestSetting;
use crate::store::{RosterStore, RosterStoreInfo};

/// Each table is guarded by its own `std::sync::Mutex`. The skeleton is
/// fine for low-contention workloads; production callers should swap to
/// `parking_lot` (or `tokio::sync::Mutex` if they need async-aware
/// semantics) once contention matters.
type Shard<T> = Arc<Mutex<T>>;

/// Default implementation of [`RosterStore`].
#[derive(Default, Clone)]
pub struct InMemoryRosterStore {
    contacts: Shard<HashMap<String, Contact>>,
    groups: Shard<HashMap<String, ContactGroup>>,
    digit_to_node: Shard<HashMap<String, String>>,
    node_to_digit: Shard<HashMap<String, String>>,
    friend_request_settings: Shard<HashMap<String, FriendRequestSetting>>,
    contact_requests: Shard<HashMap<String, PersistedContactRequest>>,
}

impl InMemoryRosterStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock<T>(m: &Shard<T>) -> RosterResult<MutexGuard<'_, T>> {
        m.lock().map_err(|e| RosterError::Lock {
            reason: format!("mutex poisoned: {e}"),
        })
    }
}

#[async_trait]
impl RosterStore for InMemoryRosterStore {
    async fn put_contact(&self, contact: Contact) -> RosterResult<()> {
        let mut g = Self::lock(&self.contacts)?;
        g.insert(contact.contact_id.clone(), contact);
        Ok(())
    }

    async fn delete_contact(&self, contact_id: &str) -> RosterResult<bool> {
        let mut g = Self::lock(&self.contacts)?;
        Ok(g.remove(contact_id).is_some())
    }

    async fn get_contact(&self, contact_id: &str) -> RosterResult<Option<Contact>> {
        let g = Self::lock(&self.contacts)?;
        Ok(g.get(contact_id).cloned())
    }

    async fn list_contacts(&self) -> RosterResult<Vec<Contact>> {
        let g = Self::lock(&self.contacts)?;
        Ok(g.values().cloned().collect())
    }

    async fn search_contacts(&self, query: &str) -> RosterResult<Vec<Contact>> {
        let g = Self::lock(&self.contacts)?;
        let q = query.to_lowercase();
        let hits = g
            .values()
            .filter(|c| {
                if q.is_empty() {
                    return true;
                }
                c.name.to_lowercase().contains(&q)
                    || c.tags.iter().any(|t| t.to_lowercase().contains(&q))
                    || c.notes.to_lowercase().contains(&q)
            })
            .cloned()
            .collect();
        Ok(hits)
    }

    async fn toggle_favorite(&self, contact_id: &str) -> RosterResult<Option<bool>> {
        let mut g = Self::lock(&self.contacts)?;
        if let Some(c) = g.get_mut(contact_id) {
            c.is_favorite = !c.is_favorite;
            Ok(Some(c.is_favorite))
        } else {
            Ok(None)
        }
    }

    async fn set_blocked(&self, contact_id: &str, blocked: bool) -> RosterResult<Option<bool>> {
        let mut g = Self::lock(&self.contacts)?;
        if let Some(c) = g.get_mut(contact_id) {
            c.is_blocked = blocked;
            Ok(Some(blocked))
        } else {
            Ok(None)
        }
    }

    async fn put_group(&self, group: ContactGroup) -> RosterResult<()> {
        group.validate()?;
        let mut g = Self::lock(&self.groups)?;
        g.insert(group.group_id.clone(), group);
        Ok(())
    }

    async fn delete_group(&self, group_id: &str) -> RosterResult<bool> {
        let mut g = Self::lock(&self.groups)?;
        Ok(g.remove(group_id).is_some())
    }

    async fn get_group(&self, group_id: &str) -> RosterResult<Option<ContactGroup>> {
        let g = Self::lock(&self.groups)?;
        Ok(g.get(group_id).cloned())
    }

    async fn list_groups(&self) -> RosterResult<Vec<ContactGroup>> {
        let g = Self::lock(&self.groups)?;
        Ok(g.values().cloned().collect())
    }

    async fn put_digit_mapping(&self, mapping: DigitMapping) -> RosterResult<()> {
        crate::digit::validate_digit_id(&mapping.digit_id)?;
        let mut d2n = Self::lock(&self.digit_to_node)?;
        let mut n2d = Self::lock(&self.node_to_digit)?;
        d2n.insert(mapping.digit_id.clone(), mapping.node_id.clone());
        n2d.insert(mapping.node_id, mapping.digit_id);
        Ok(())
    }

    async fn resolve_digit_to_node(&self, digit_id: &str) -> RosterResult<Option<String>> {
        let g = Self::lock(&self.digit_to_node)?;
        Ok(g.get(digit_id).cloned())
    }

    async fn resolve_node_to_digit(&self, node_id: &str) -> RosterResult<Option<String>> {
        let g = Self::lock(&self.node_to_digit)?;
        Ok(g.get(node_id).cloned())
    }

    async fn list_digit_mappings(&self) -> RosterResult<Vec<DigitMapping>> {
        let d2n = Self::lock(&self.digit_to_node)?;
        Ok(d2n
            .iter()
            .map(|(d, n)| DigitMapping {
                digit_id: d.clone(),
                node_id: n.clone(),
                created_at: 0,
            })
            .collect())
    }

    async fn put_friend_request_setting(
        &self,
        setting: FriendRequestSetting,
    ) -> RosterResult<()> {
        let mut g = Self::lock(&self.friend_request_settings)?;
        g.insert(setting.user_id.clone(), setting);
        Ok(())
    }

    async fn get_friend_request_setting(
        &self,
        user_id: &str,
    ) -> RosterResult<Option<FriendRequestSetting>> {
        let g = Self::lock(&self.friend_request_settings)?;
        Ok(g.get(user_id).cloned())
    }

    async fn put_contact_request(
        &self,
        request: PersistedContactRequest,
    ) -> RosterResult<()> {
        let mut g = Self::lock(&self.contact_requests)?;
        g.insert(request.request_id.clone(), request);
        Ok(())
    }

    async fn get_contact_request(
        &self,
        request_id: &str,
    ) -> RosterResult<Option<PersistedContactRequest>> {
        let g = Self::lock(&self.contact_requests)?;
        Ok(g.get(request_id).cloned())
    }

    async fn delete_contact_request(
        &self,
        request_id: &str,
    ) -> RosterResult<Option<PersistedContactRequest>> {
        let mut g = Self::lock(&self.contact_requests)?;
        Ok(g.remove(request_id))
    }

    async fn list_contact_requests_for(
        &self,
        user_id: &str,
    ) -> RosterResult<Vec<PersistedContactRequest>> {
        let g = Self::lock(&self.contact_requests)?;
        Ok(g.values()
            .filter(|r| r.to_user_id == user_id)
            .cloned()
            .collect())
    }
}

impl InMemoryRosterStore {
    /// Snapshot counts for diagnostics.
    pub fn info(&self) -> RosterStoreInfo {
        let contacts = Self::lock(&self.contacts)
            .map(|g| g.len())
            .unwrap_or_else(|e| {
                warn!("contacts mutex poisoned: {e}");
                0
            });
        let groups = Self::lock(&self.groups)
            .map(|g| g.len())
            .unwrap_or_else(|e| {
                warn!("groups mutex poisoned: {e}");
                0
            });
        let digit_mapping_count = Self::lock(&self.digit_to_node)
            .map(|g| g.len())
            .unwrap_or_else(|e| {
                warn!("digit_to_node mutex poisoned: {e}");
                0
            });
        RosterStoreInfo {
            backend: "memory",
            location: None,
            contact_count: contacts,
            group_count: groups,
            digit_mapping_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Contact;

    #[tokio::test]
    async fn round_trip_contact() {
        let s = InMemoryRosterStore::new();
        let c = Contact::new_human("c1", "Alice");
        s.put_contact(c.clone()).await.unwrap();
        let got = s.get_contact("c1").await.unwrap().unwrap();
        assert_eq!(got.name, "Alice");
        assert!(s.delete_contact("c1").await.unwrap());
        assert!(s.get_contact("c1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn digit_round_trip() {
        let s = InMemoryRosterStore::new();
        s.put_digit_mapping(DigitMapping::new("123456789012", "node-x"))
            .await
            .unwrap();
        assert_eq!(
            s.resolve_digit_to_node("123456789012").await.unwrap(),
            Some("node-x".to_string())
        );
        assert_eq!(
            s.resolve_node_to_digit("node-x").await.unwrap(),
            Some("123456789012".to_string())
        );
    }

    #[tokio::test]
    async fn digit_mapping_validates() {
        let s = InMemoryRosterStore::new();
        assert!(s
            .put_digit_mapping(DigitMapping::new("not-digits", "n"))
            .await
            .is_err());
    }
}