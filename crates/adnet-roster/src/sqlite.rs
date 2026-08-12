//! SQLite-backed [`RosterStore`] implementation.
//!
//! Mirrors the original `Exodus@src-backup/src-tauri/src/microservice/contact_directory_service.rs`
//! persistence model but expressed as a real SQLite schema instead of a
//! single JSON snapshot. Schema version 1.
//!
//! ## Tables
//!
//! - `contacts`                       — one row per [`Contact`].
//! - `contact_groups`                 — one row per [`ContactGroup`].
//! - `digit_mappings`                 — bidirectional `digit_id ↔ node_id`.
//! - `friend_request_settings`        — per-user friend-request mode.
//!
//! ## Conventions
//!
//! - All writes use `INSERT OR REPLACE` (or `UPDATE ... WHERE`).
//! - `Vec<String>` columns (agent_ids / groups / tags / iot_capabilities)
//!   are persisted as JSON text — same approach used by
//!   [`adnet_chatstore::storage`].
//! - Booleans are stored as `INTEGER` (`0`/`1`).
//! - Timestamps are Unix seconds, stored as `INTEGER`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};
use tracing::info;

use crate::error::{RosterError, RosterResult};
use crate::group::ContactGroup;
use crate::mapping::DigitMapping;
use crate::model::Contact;
use crate::settings::FriendRequestSetting;
use crate::store::{RosterStore, RosterStoreInfo};

/// Current schema version. Bump this and add a migration when changing the
/// tables.
pub const SCHEMA_VERSION: u32 = 1;

/// Configuration for [`SqliteRosterStore`].
#[derive(Debug, Clone)]
pub struct SqliteRosterStoreConfig {
    /// Path to the SQLite database file.
    pub path: PathBuf,
}

impl SqliteRosterStoreConfig {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn under_app_data(app_data: &Path) -> Self {
        Self::new(app_data.join("roster.sqlite"))
    }
}

/// All `CREATE TABLE IF NOT EXISTS` statements. Run inside a single
/// transaction by [`SqliteRosterStore::open`].
const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS contacts (
    contact_id           TEXT PRIMARY KEY,
    name                 TEXT NOT NULL,
    contact_type         TEXT NOT NULL,
    agent_deployment_type TEXT,
    agent_ids_json       TEXT NOT NULL DEFAULT '[]',
    node_id              TEXT NOT NULL DEFAULT '',
    groups_json          TEXT NOT NULL DEFAULT '[]',
    tags_json            TEXT NOT NULL DEFAULT '[]',
    notes                TEXT NOT NULL DEFAULT '',
    is_favorite          INTEGER NOT NULL DEFAULT 0,
    is_blocked           INTEGER NOT NULL DEFAULT 0,
    created_at           INTEGER NOT NULL DEFAULT 0,
    last_contacted       INTEGER NOT NULL DEFAULT 0,
    contact_count        INTEGER NOT NULL DEFAULT 0,
    public_account_id    TEXT,
    iot_device_type      TEXT,
    iot_protocol         TEXT,
    iot_status           TEXT,
    iot_last_seen        INTEGER,
    iot_capabilities_json TEXT,
    iot_location         TEXT
);

CREATE INDEX IF NOT EXISTS idx_contacts_node_id     ON contacts(node_id);
CREATE INDEX IF NOT EXISTS idx_contacts_type        ON contacts(contact_type);

CREATE TABLE IF NOT EXISTS contact_groups (
    group_id     TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    description  TEXT NOT NULL DEFAULT '',
    color        TEXT NOT NULL DEFAULT '',
    created_at   INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS digit_mappings (
    digit_id     TEXT PRIMARY KEY,
    node_id      TEXT NOT NULL UNIQUE,
    created_at   INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_digit_mappings_node_id ON digit_mappings(node_id);

CREATE TABLE IF NOT EXISTS friend_request_settings (
    user_id     TEXT PRIMARY KEY,
    mode        TEXT NOT NULL,
    updated_at  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS schema_version (
    version     INTEGER PRIMARY KEY,
    applied_at  INTEGER NOT NULL
);
"#;

/// SQLite-backed roster store. Each instance owns a single `Connection`
/// behind an `Arc<Mutex>`.
pub struct SqliteRosterStore {
    conn: Arc<std::sync::Mutex<Connection>>,
    config: SqliteRosterStoreConfig,
}

impl SqliteRosterStore {
    /// Open (and migrate) the store at `config.path`.
    pub fn open(config: SqliteRosterStoreConfig) -> RosterResult<Self> {
        if let Some(parent) = config.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| RosterError::Io {
                operation: "create_dir_all".to_string(),
                reason: e.to_string(),
            })?;
        }
        let conn = Connection::open(&config.path).map_err(|e| RosterError::Io {
            operation: format!("open {}", config.path.display()),
            reason: e.to_string(),
        })?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;",
        )?;
        conn.execute_batch(SCHEMA_SQL)?;
        let now = current_timestamp_secs();
        conn.execute(
            "INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (?1, ?2)",
            params![SCHEMA_VERSION as i64, now as i64],
        )?;
        info!(
            "roster sqlite ready (path = {}, version = {})",
            config.path.display(),
            SCHEMA_VERSION
        );
        Ok(Self {
            conn: Arc::new(std::sync::Mutex::new(conn)),
            config,
        })
    }

    fn lock(&self) -> RosterResult<std::sync::MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|e| RosterError::Lock {
            reason: format!("connection mutex poisoned: {e}"),
        })
    }

    pub fn info(&self) -> RosterStoreInfo {
        let conn = match self.lock() {
            Ok(c) => c,
            Err(_) => {
                return RosterStoreInfo {
                    backend: "sqlite",
                    location: Some(self.config.path.display().to_string()),
                    contact_count: 0,
                    group_count: 0,
                    digit_mapping_count: 0,
                };
            }
        };
        let count = |sql: &str| -> usize {
            conn.query_row(sql, [], |row| row.get::<_, i64>(0))
                .map(|n| n.max(0) as usize)
                .unwrap_or(0)
        };
        RosterStoreInfo {
            backend: "sqlite",
            location: Some(self.config.path.display().to_string()),
            contact_count: count("SELECT COUNT(*) FROM contacts"),
            group_count: count("SELECT COUNT(*) FROM contact_groups"),
            digit_mapping_count: count("SELECT COUNT(*) FROM digit_mappings"),
        }
    }
}

fn current_timestamp_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Column lists / SQL fragments — kept in one place so the read & write
// paths can never drift out of sync.
// ---------------------------------------------------------------------------

const CONTACT_COLS: &str = "contact_id, name, contact_type, agent_deployment_type, \
     agent_ids_json, node_id, groups_json, tags_json, notes, \
     is_favorite, is_blocked, created_at, last_contacted, contact_count, \
     public_account_id, iot_device_type, iot_protocol, iot_status, \
     iot_last_seen, iot_capabilities_json, iot_location";

// ---------------------------------------------------------------------------
// Trait implementation — full CRUD.
// ---------------------------------------------------------------------------

#[async_trait]
impl RosterStore for SqliteRosterStore {
    async fn put_contact(&self, contact: Contact) -> RosterResult<()> {
        // Validate IoT-specific fields before they touch the DB. The
        // input validator (CD-002) lives in `crate::error::RosterError`,
        // but for now we keep this cheap: a single helper call.
        if let Err(e) = contact.validate_iot_fields() {
            return Err(RosterError::Validation {
                field: "iot_fields".to_string(),
                reason: e,
            });
        }
        let agent_ids_json =
            serde_json::to_string(&contact.agent_ids).map_err(|e| RosterError::Serialization {
                operation: "agent_ids".to_string(),
                reason: e.to_string(),
            })?;
        let groups_json =
            serde_json::to_string(&contact.groups).map_err(|e| RosterError::Serialization {
                operation: "groups".to_string(),
                reason: e.to_string(),
            })?;
        let tags_json =
            serde_json::to_string(&contact.tags).map_err(|e| RosterError::Serialization {
                operation: "tags".to_string(),
                reason: e.to_string(),
            })?;
        let iot_caps_json = match &contact.iot_capabilities {
            Some(c) => serde_json::to_string(c).map(Some).map_err(|e| {
                RosterError::Serialization {
                    operation: "iot_capabilities".to_string(),
                    reason: e.to_string(),
                }
            })?,
            None => None,
        };

        let conn = self.lock()?;
        conn.execute(
            &format!(
                "INSERT OR REPLACE INTO contacts ({CONTACT_COLS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
                         ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)"
            ),
            params![
                contact.contact_id,
                contact.name,
                contact.contact_type,
                contact.agent_deployment_type,
                agent_ids_json,
                contact.node_id,
                groups_json,
                tags_json,
                contact.notes,
                if contact.is_favorite { 1_i64 } else { 0_i64 },
                if contact.is_blocked { 1_i64 } else { 0_i64 },
                contact.created_at as i64,
                contact.last_contacted as i64,
                contact.contact_count as i64,
                contact.public_account_id,
                contact.iot_device_type,
                contact.iot_protocol,
                contact.iot_status,
                contact.iot_last_seen.map(|n| n as i64),
                iot_caps_json,
                contact.iot_location,
            ],
        )?;
        Ok(())
    }

    async fn delete_contact(&self, contact_id: &str) -> RosterResult<bool> {
        let conn = self.lock()?;
        let removed = conn.execute(
            "DELETE FROM contacts WHERE contact_id = ?1",
            params![contact_id],
        )?;
        Ok(removed > 0)
    }

    async fn get_contact(&self, contact_id: &str) -> RosterResult<Option<Contact>> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                &format!("SELECT {CONTACT_COLS} FROM contacts WHERE contact_id = ?1"),
                params![contact_id],
                row_to_contact,
            )
            .optional()?;
        Ok(row)
    }

    async fn list_contacts(&self) -> RosterResult<Vec<Contact>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!("SELECT {CONTACT_COLS} FROM contacts"))?;
        let rows = stmt
            .query_map([], row_to_contact)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    async fn search_contacts(&self, query: &str) -> RosterResult<Vec<Contact>> {
        if query.is_empty() {
            return self.list_contacts().await;
        }
        let pattern = format!("%{}%", escape_like(query));
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {CONTACT_COLS} FROM contacts \
             WHERE name LIKE ?1 ESCAPE '\\' \
                OR notes LIKE ?1 ESCAPE '\\' \
                OR tags_json LIKE ?1 ESCAPE '\\' \
             ORDER BY name ASC"
        ))?;
        let rows = stmt
            .query_map(params![pattern], row_to_contact)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    async fn toggle_favorite(&self, contact_id: &str) -> RosterResult<Option<bool>> {
        let conn = self.lock()?;
        // Single-statement flip: read current, write flipped. We use a
        // single connection under our own mutex so the read-modify-write
        // is atomic w.r.t. other callers on this store.
        let current: Option<i64> = conn
            .query_row(
                "SELECT is_favorite FROM contacts WHERE contact_id = ?1",
                params![contact_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(current) = current else {
            return Ok(None);
        };
        let new_val = if current == 0 { 1_i64 } else { 0_i64 };
        conn.execute(
            "UPDATE contacts SET is_favorite = ?1 WHERE contact_id = ?2",
            params![new_val, contact_id],
        )?;
        Ok(Some(new_val != 0))
    }

    async fn set_blocked(&self, contact_id: &str, blocked: bool) -> RosterResult<Option<bool>> {
        let conn = self.lock()?;
        let updated = conn.execute(
            "UPDATE contacts SET is_blocked = ?1 WHERE contact_id = ?2",
            params![if blocked { 1_i64 } else { 0_i64 }, contact_id],
        )?;
        if updated == 0 {
            Ok(None)
        } else {
            Ok(Some(blocked))
        }
    }

    async fn put_group(&self, group: ContactGroup) -> RosterResult<()> {
        group.validate()?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR REPLACE INTO contact_groups (group_id, name, description, color, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                group.group_id,
                group.name,
                group.description,
                group.color,
                group.created_at as i64,
            ],
        )?;
        Ok(())
    }

    async fn delete_group(&self, group_id: &str) -> RosterResult<bool> {
        let conn = self.lock()?;
        let removed = conn.execute(
            "DELETE FROM contact_groups WHERE group_id = ?1",
            params![group_id],
        )?;
        Ok(removed > 0)
    }

    async fn get_group(&self, group_id: &str) -> RosterResult<Option<ContactGroup>> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT group_id, name, description, color, created_at \
                 FROM contact_groups WHERE group_id = ?1",
                params![group_id],
                row_to_group,
            )
            .optional()?;
        Ok(row)
    }

    async fn list_groups(&self) -> RosterResult<Vec<ContactGroup>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT group_id, name, description, color, created_at \
             FROM contact_groups ORDER BY name ASC",
        )?;
        let rows = stmt
            .query_map([], row_to_group)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    async fn put_digit_mapping(&self, mapping: DigitMapping) -> RosterResult<()> {
        crate::digit::validate_digit_id(&mapping.digit_id)?;
        let mut conn = self.lock()?;
        // The schema enforces `digit_id PRIMARY KEY` AND `node_id
        // UNIQUE`. Use a transaction so a conflict on either side is
        // surfaced as `RosterError::AlreadyExists` (mapped from
        // `rusqlite::Error::SqliteFailure` with `UNIQUE` extended code).
        let tx = conn
            .transaction()
            .map_err(|e| RosterError::Io {
                operation: "begin tx".to_string(),
                reason: e.to_string(),
            })?;
        // Node id uniqueness is enforced via `ON CONFLICT(node_id)`,
        // but for clarity we just delete any pre-existing mapping with
        // the same `node_id` first, then upsert by `digit_id`.
        tx.execute(
            "DELETE FROM digit_mappings WHERE node_id = ?1",
            params![mapping.node_id],
        )?;
        match tx.execute(
            "INSERT INTO digit_mappings (digit_id, node_id, created_at) \
             VALUES (?1, ?2, ?3)",
            params![
                mapping.digit_id,
                mapping.node_id,
                mapping.created_at as i64
            ],
        ) {
            Ok(_) => {
                tx.commit().map_err(|e| RosterError::Io {
                    operation: "commit".to_string(),
                    reason: e.to_string(),
                })?;
                Ok(())
            }
            Err(e) => {
                if let rusqlite::Error::SqliteFailure(err, _msg) = &e {
                    if err.code == rusqlite::ErrorCode::ConstraintViolation {
                        return Err(RosterError::AlreadyExists {
                            kind: "digit_mapping",
                            id: mapping.digit_id,
                        });
                    }
                }
                Err(e.into())
            }
        }
    }

    async fn resolve_digit_to_node(&self, digit_id: &str) -> RosterResult<Option<String>> {
        let conn = self.lock()?;
        let node: Option<String> = conn
            .query_row(
                "SELECT node_id FROM digit_mappings WHERE digit_id = ?1",
                params![digit_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(node)
    }

    async fn resolve_node_to_digit(&self, node_id: &str) -> RosterResult<Option<String>> {
        let conn = self.lock()?;
        let digit: Option<String> = conn
            .query_row(
                "SELECT digit_id FROM digit_mappings WHERE node_id = ?1",
                params![node_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(digit)
    }

    async fn list_digit_mappings(&self) -> RosterResult<Vec<DigitMapping>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT digit_id, node_id, created_at \
             FROM digit_mappings ORDER BY created_at ASC, digit_id ASC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(DigitMapping {
                    digit_id: row.get(0)?,
                    node_id: row.get(1)?,
                    created_at: row.get::<_, i64>(2)? as u64,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    async fn put_friend_request_setting(
        &self,
        setting: FriendRequestSetting,
    ) -> RosterResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR REPLACE INTO friend_request_settings (user_id, mode, updated_at) \
             VALUES (?1, ?2, ?3)",
            params![
                setting.user_id,
                setting.mode,
                setting.updated_at as i64,
            ],
        )?;
        Ok(())
    }

    async fn get_friend_request_setting(
        &self,
        user_id: &str,
    ) -> RosterResult<Option<FriendRequestSetting>> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT user_id, mode, updated_at \
                 FROM friend_request_settings WHERE user_id = ?1",
                params![user_id],
                |row| {
                    Ok(FriendRequestSetting {
                        user_id: row.get(0)?,
                        mode: row.get(1)?,
                        updated_at: row.get::<_, i64>(2)? as u64,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }
}

/// Escape `%` and `_` so user input cannot be interpreted as SQL `LIKE`
/// wildcards. The matcher's `ESCAPE '\\'` clause (used in
/// `search_contacts`) relies on this escaping.
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '%' | '_' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            other => out.push(other),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Row → struct helpers.
// ---------------------------------------------------------------------------

fn row_to_contact(row: &rusqlite::Row<'_>) -> rusqlite::Result<Contact> {
    let agent_ids_json: String = row.get(4)?;
    let groups_json: String = row.get(6)?;
    let tags_json: String = row.get(7)?;
    let iot_caps_json: Option<String> = row.get(19)?;
    let iot_caps = iot_caps_json
        .map(|s| serde_json::from_str::<Vec<String>>(&s))
        .transpose()
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
        })?;
    Ok(Contact {
        contact_id: row.get(0)?,
        name: row.get(1)?,
        contact_type: row.get(2)?,
        agent_deployment_type: row.get(3)?,
        agent_ids: serde_json::from_str(&agent_ids_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
        })?,
        node_id: row.get(5)?,
        groups: serde_json::from_str(&groups_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
        })?,
        tags: serde_json::from_str(&tags_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
        })?,
        notes: row.get(8)?,
        is_favorite: row.get::<_, i64>(9)? != 0,
        is_blocked: row.get::<_, i64>(10)? != 0,
        created_at: row.get::<_, i64>(11)? as u64,
        last_contacted: row.get::<_, i64>(12)? as u64,
        contact_count: row.get::<_, i64>(13)? as u32,
        public_account_id: row.get(14)?,
        iot_device_type: row.get(15)?,
        iot_protocol: row.get(16)?,
        iot_status: row.get(17)?,
        iot_last_seen: row.get::<_, Option<i64>>(18)?.map(|n| n as u64),
        iot_capabilities: iot_caps,
        iot_location: row.get(20)?,
    })
}

fn row_to_group(row: &rusqlite::Row<'_>) -> rusqlite::Result<ContactGroup> {
    Ok(ContactGroup {
        group_id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        color: row.get(3)?,
        created_at: row.get::<_, i64>(4)? as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::ContactGroup;
    use crate::model::{Contact, ContactType};
    use crate::settings::{FriendRequestMode, FriendRequestSetting};
    use tempfile::TempDir;

    /// Helper: open a fresh SqliteRosterStore under a tempdir.
    async fn open_store() -> (SqliteRosterStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let store = SqliteRosterStore::open(SqliteRosterStoreConfig::new(dir.path().join("r.db")))
            .expect("open");
        (store, dir)
    }

    fn sample_contact(id: &str) -> Contact {
        let mut c = Contact::new_human(id, "Alice");
        c.agent_ids = vec!["agent-1".into(), "agent-2".into()];
        c.node_id = "node-alice".into();
        c.groups = vec!["friends".into(), "work".into()];
        c.tags = vec!["vip".into()];
        c.notes = "met at conference".into();
        c.is_favorite = true;
        c.created_at = 100;
        c.last_contacted = 200;
        c.contact_count = 3;
        c.public_account_id = Some("pub-1".into());
        c
    }

    fn sample_iot_contact(id: &str) -> Contact {
        let mut c = sample_contact(id);
        c.name = "Kitchen Lamp".into();
        c.contact_type = ContactType::Iot.as_str().into();
        c.iot_device_type = Some("smart_light".into());
        c.iot_protocol = Some("matter".into());
        c.iot_status = Some("online".into());
        c.iot_capabilities = Some(vec!["on_off".into(), "dimming".into()]);
        c.iot_location = Some("kitchen".into());
        c
    }

    #[tokio::test]
    async fn put_then_get_contact_round_trip() {
        let (store, _dir) = open_store().await;
        let c = sample_contact("c1");
        store.put_contact(c.clone()).await.unwrap();
        let got = store.get_contact("c1").await.unwrap().unwrap();
        assert_eq!(got.name, "Alice");
        assert_eq!(got.node_id, "node-alice");
        assert_eq!(got.agent_ids, vec!["agent-1", "agent-2"]);
        assert_eq!(got.groups, vec!["friends", "work"]);
        assert_eq!(got.tags, vec!["vip"]);
        assert_eq!(got.notes, "met at conference");
        assert!(got.is_favorite);
        assert_eq!(got.created_at, 100);
        assert_eq!(got.last_contacted, 200);
        assert_eq!(got.contact_count, 3);
        assert_eq!(got.public_account_id.as_deref(), Some("pub-1"));
    }

    #[tokio::test]
    async fn iot_contact_round_trip() {
        let (store, _dir) = open_store().await;
        let c = sample_iot_contact("dev-1");
        store.put_contact(c).await.unwrap();
        let got = store.get_contact("dev-1").await.unwrap().unwrap();
        assert!(got.is_iot_online());
        assert_eq!(got.iot_location.as_deref(), Some("kitchen"));
        assert_eq!(
            got.iot_capabilities,
            Some(vec!["on_off".to_string(), "dimming".to_string()])
        );
    }

    #[tokio::test]
    async fn iot_contact_validates_on_put() {
        let (store, _dir) = open_store().await;
        let mut c = sample_contact("dev-bad");
        c.contact_type = ContactType::Iot.as_str().into();
        // missing iot_device_type etc.
        assert!(store.put_contact(c).await.is_err());
    }

    #[tokio::test]
    async fn delete_contact_returns_flag() {
        let (store, _dir) = open_store().await;
        store.put_contact(sample_contact("c1")).await.unwrap();
        assert!(store.delete_contact("c1").await.unwrap());
        assert!(!store.delete_contact("c1").await.unwrap());
        assert!(store.get_contact("c1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn list_contacts_returns_all() {
        let (store, _dir) = open_store().await;
        for i in 0..3 {
            store
                .put_contact(sample_contact(&format!("c{i}")))
                .await
                .unwrap();
        }
        let list = store.list_contacts().await.unwrap();
        assert_eq!(list.len(), 3);
    }

    #[tokio::test]
    async fn search_contacts_substring_matches_name_and_notes() {
        let (store, _dir) = open_store().await;
        store.put_contact(sample_contact("c1")).await.unwrap();
        let mut c2 = sample_contact("c2");
        c2.name = "Bob".into();
        c2.notes = "alice's brother".into();
        store.put_contact(c2).await.unwrap();
        let hits = store.search_contacts("alice").await.unwrap();
        // matches both "Alice" (name) and "alice's brother" (notes).
        assert_eq!(hits.len(), 2);
    }

    #[tokio::test]
    async fn search_contacts_empty_query_returns_all() {
        let (store, _dir) = open_store().await;
        store.put_contact(sample_contact("c1")).await.unwrap();
        let all = store.search_contacts("").await.unwrap();
        assert_eq!(all.len(), 1);
    }

    #[tokio::test]
    async fn search_contacts_escapes_like_wildcards() {
        let (store, _dir) = open_store().await;
        let mut c = sample_contact("c1");
        c.notes = "100% safe".into();
        store.put_contact(c).await.unwrap();
        // '%' must NOT act as a wildcard. Searching "100%" returns
        // exactly the one row whose notes contain the literal "100%".
        let hits = store.search_contacts("100%").await.unwrap();
        assert_eq!(hits.len(), 1);
        // searching "100" would match via "100%" — sanity check.
        let hits2 = store.search_contacts("100").await.unwrap();
        assert_eq!(hits2.len(), 1);
    }

    #[tokio::test]
    async fn toggle_favorite_round_trip() {
        let (store, _dir) = open_store().await;
        store.put_contact(sample_contact("c1")).await.unwrap();
        assert_eq!(store.toggle_favorite("c1").await.unwrap(), Some(false));
        assert_eq!(store.toggle_favorite("c1").await.unwrap(), Some(true));
        assert_eq!(store.toggle_favorite("nope").await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_blocked_returns_flag() {
        let (store, _dir) = open_store().await;
        store.put_contact(sample_contact("c1")).await.unwrap();
        assert_eq!(store.set_blocked("c1", true).await.unwrap(), Some(true));
        assert_eq!(store.set_blocked("nope", true).await.unwrap(), None);
    }

    #[tokio::test]
    async fn group_round_trip() {
        let (store, _dir) = open_store().await;
        let g = ContactGroup {
            group_id: "g1".into(),
            name: "Friends".into(),
            description: "people I know".into(),
            color: "blue".into(),
            created_at: 42,
        };
        store.put_group(g).await.unwrap();
        let got = store.get_group("g1").await.unwrap().unwrap();
        assert_eq!(got.name, "Friends");
        assert_eq!(got.color, "blue");
        assert_eq!(got.created_at, 42);
        let list = store.list_groups().await.unwrap();
        assert_eq!(list.len(), 1);
        assert!(store.delete_group("g1").await.unwrap());
        assert!(!store.delete_group("g1").await.unwrap());
    }

    #[tokio::test]
    async fn group_rejects_empty_id() {
        let (store, _dir) = open_store().await;
        let g = ContactGroup {
            group_id: "".into(),
            name: "x".into(),
            description: "".into(),
            color: "".into(),
            created_at: 0,
        };
        assert!(store.put_group(g).await.is_err());
    }

    #[tokio::test]
    async fn digit_mapping_round_trip() {
        let (store, _dir) = open_store().await;
        let m = DigitMapping::new("123456789012", "node-x").with_created_at(99);
        store.put_digit_mapping(m).await.unwrap();
        assert_eq!(
            store.resolve_digit_to_node("123456789012").await.unwrap(),
            Some("node-x".into())
        );
        assert_eq!(
            store.resolve_node_to_digit("node-x").await.unwrap(),
            Some("123456789012".into())
        );
        let list = store.list_digit_mappings().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].created_at, 99);
    }

    #[tokio::test]
    async fn digit_mapping_rejects_bad_format() {
        let (store, _dir) = open_store().await;
        let m = DigitMapping::new("not-digits", "node-x");
        assert!(store.put_digit_mapping(m).await.is_err());
    }

    #[tokio::test]
    async fn digit_mapping_rejects_duplicate_digit() {
        let (store, _dir) = open_store().await;
        store
            .put_digit_mapping(DigitMapping::new("111111111111", "node-a"))
            .await
            .unwrap();
        let dup = DigitMapping::new("111111111111", "node-b");
        let err = store.put_digit_mapping(dup).await.unwrap_err();
        assert!(matches!(err, RosterError::AlreadyExists { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn digit_mapping_node_id_uniqueness_evicts_old() {
        // node_id has UNIQUE in the schema. Reusing a node_id with a
        // new digit should evict the previous mapping.
        let (store, _dir) = open_store().await;
        store
            .put_digit_mapping(DigitMapping::new("111111111111", "node-shared"))
            .await
            .unwrap();
        store
            .put_digit_mapping(DigitMapping::new("222222222222", "node-shared"))
            .await
            .unwrap();
        assert!(store
            .resolve_digit_to_node("111111111111")
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            store.resolve_node_to_digit("node-shared").await.unwrap(),
            Some("222222222222".into())
        );
    }

    #[tokio::test]
    async fn friend_request_setting_round_trip() {
        let (store, _dir) = open_store().await;
        let s = FriendRequestSetting::new("u-1", FriendRequestMode::RequireConfirmation);
        store.put_friend_request_setting(s).await.unwrap();
        let got = store.get_friend_request_setting("u-1").await.unwrap().unwrap();
        assert_eq!(
            got.parsed_mode(),
            Some(FriendRequestMode::RequireConfirmation)
        );
        assert!(store
            .get_friend_request_setting("nobody")
            .await
            .unwrap()
            .is_none());
    }

    #[test]
    fn open_creates_schema() {
        let dir = TempDir::new().unwrap();
        let store = SqliteRosterStore::open(SqliteRosterStoreConfig::new(dir.path().join("r.db")))
            .expect("open");
        let info = store.info();
        assert_eq!(info.backend, "sqlite");
        assert_eq!(info.contact_count, 0);
    }
}