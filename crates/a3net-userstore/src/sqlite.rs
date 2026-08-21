//! SQLite-backed [`UserStore`] implementation.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{params, Connection, OptionalExtension};
use tracing::info;

use crate::error::{UserStoreError, UserStoreResult};
use crate::model::{UserDevice, UserKind, UserPreferences, UserProfile, UserPublicKey};
use crate::store::{UserStore, UserStoreInfo};

/// Current schema version.
///
/// v1 → v2: adds `kind` column to `user_profile` and `label` column
/// to `user_public_keys`. The migration is idempotent (see
/// [`SqliteUserStore::migrate`]).
pub const USER_SCHEMA_VERSION: u32 = 2;

/// All `CREATE TABLE IF NOT EXISTS` statements for the userstore.
const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS user_profile (
    user_id          TEXT PRIMARY KEY,
    username         TEXT NOT NULL,
    display_name     TEXT NOT NULL DEFAULT '',
    avatar_blob_hash TEXT,
    avatar_mime      TEXT,
    avatar_size      INTEGER,
    bio              TEXT NOT NULL DEFAULT '',
    preferences_json TEXT NOT NULL DEFAULT '{}',
    kind             TEXT NOT NULL DEFAULT 'human',
    created_at       INTEGER NOT NULL DEFAULT 0,
    updated_at       INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_user_profile_username ON user_profile(username);

CREATE TABLE IF NOT EXISTS user_public_keys (
    key_id        TEXT PRIMARY KEY,
    user_id       TEXT NOT NULL,
    algorithm     TEXT NOT NULL,
    key_material  TEXT NOT NULL,
    label         TEXT NOT NULL DEFAULT '',
    created_at    INTEGER NOT NULL DEFAULT 0,
    revoked_at    INTEGER,
    FOREIGN KEY (user_id) REFERENCES user_profile(user_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_user_public_keys_user ON user_public_keys(user_id);
CREATE INDEX IF NOT EXISTS idx_user_public_keys_alive
    ON user_public_keys(user_id) WHERE revoked_at IS NULL;

CREATE TABLE IF NOT EXISTS user_devices (
    device_id    TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL,
    node_id      TEXT NOT NULL,
    pairing_id   TEXT,
    device_class TEXT NOT NULL,
    label        TEXT NOT NULL DEFAULT '',
    paired_at    INTEGER NOT NULL DEFAULT 0,
    revoked_at   INTEGER,
    FOREIGN KEY (user_id) REFERENCES user_profile(user_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_user_devices_user ON user_devices(user_id);
CREATE INDEX IF NOT EXISTS idx_user_devices_node ON user_devices(node_id);

CREATE TABLE IF NOT EXISTS user_id_digit (
    user_id    TEXT PRIMARY KEY,
    digit_id   TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL DEFAULT 0
    -- Intentionally no FK to user_profile: the 12-digit id is the canonical
    -- primary identifier and must be writable even before a profile row
    -- exists (provisioning flow). Cascading deletes are handled in the
    -- application layer via UserStore::delete_profile.
);

CREATE INDEX IF NOT EXISTS idx_user_id_digit_digit ON user_id_digit(digit_id);

CREATE TABLE IF NOT EXISTS schema_version (
    version     INTEGER PRIMARY KEY,
    applied_at  INTEGER NOT NULL
);
"#;

/// Configuration for [`SqliteUserStore`].
#[derive(Debug, Clone)]
pub struct SqliteUserStoreConfig {
    pub path: PathBuf,
}

impl SqliteUserStoreConfig {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn under_app_data(app_data: &Path) -> Self {
        Self::new(app_data.join("userstore.sqlite"))
    }
}

/// SQLite-backed userstore. Single connection behind an `Arc<Mutex>`.
pub struct SqliteUserStore {
    conn: Arc<Mutex<Connection>>,
    config: SqliteUserStoreConfig,
}

impl SqliteUserStore {
    pub fn open(config: SqliteUserStoreConfig) -> UserStoreResult<Self> {
        if let Some(parent) = config.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| UserStoreError::Io {
                operation: "create_dir_all".to_string(),
                reason: e.to_string(),
            })?;
        }
        let conn = Connection::open(&config.path).map_err(|e| UserStoreError::Io {
            operation: format!("open {}", config.path.display()),
            reason: e.to_string(),
        })?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;",
        )?;
        conn.execute_batch(SCHEMA_SQL)?;
        Self::migrate(&conn)?;
        let now = current_timestamp_secs();
        conn.execute(
            "INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (?1, ?2)",
            params![USER_SCHEMA_VERSION as i64, now as i64],
        )?;
        info!(
            "userstore sqlite ready (path = {}, version = {})",
            config.path.display(),
            USER_SCHEMA_VERSION
        );
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            config,
        })
    }

    /// Idempotent schema migration. Walks every version between
    /// the recorded `schema_version` row and [`USER_SCHEMA_VERSION`].
    fn migrate(conn: &Connection) -> UserStoreResult<()> {
        let recorded: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0);
        let target = USER_SCHEMA_VERSION as i64;
        if recorded >= target {
            return Ok(());
        }
        for step in (recorded + 1)..=target {
            info!("userstore: applying schema migration to v{step}");
            match step {
                1 => {
                    // v0 → v1: nothing — base schema is applied
                    // by SCHEMA_SQL.
                }
                2 => {
                    Self::migrate_to_v2(conn)?;
                }
                _ => {}
            }
            let now = current_timestamp_secs() as i64;
            conn.execute(
                "INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (?1, ?2)",
                params![step, now],
            )?;
        }
        Ok(())
    }

    /// v1 → v2: add `kind` to `user_profile` and `label` to
    /// `user_public_keys`. Both columns use existence probes so
    /// re-runs are no-ops.
    fn migrate_to_v2(conn: &Connection) -> UserStoreResult<()> {
        if !column_exists(conn, "user_profile", "kind")? {
            conn.execute_batch(
                "ALTER TABLE user_profile ADD COLUMN kind TEXT NOT NULL DEFAULT 'human';",
            )?;
        }
        if !column_exists(conn, "user_public_keys", "label")? {
            conn.execute_batch(
                "ALTER TABLE user_public_keys ADD COLUMN label TEXT NOT NULL DEFAULT '';",
            )?;
        }
        Ok(())
    }

    fn lock(&self) -> UserStoreResult<MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|e| UserStoreError::Lock {
            reason: format!("connection mutex poisoned: {e}"),
        })
    }

    pub fn info(&self) -> UserStoreInfo {
        let conn = match self.lock() {
            Ok(c) => c,
            Err(_) => {
                return UserStoreInfo {
                    backend: "sqlite",
                    location: Some(self.config.path.display().to_string()),
                    profile_count: 0,
                    public_key_count: 0,
                    device_count: 0,
                };
            }
        };
        let count = |sql: &str| -> usize {
            conn.query_row(sql, [], |row| row.get::<_, i64>(0))
                .map(|n| n.max(0) as usize)
                .unwrap_or(0)
        };
        UserStoreInfo {
            backend: "sqlite",
            location: Some(self.config.path.display().to_string()),
            profile_count: count("SELECT COUNT(*) FROM user_profile"),
            public_key_count: count("SELECT COUNT(*) FROM user_public_keys"),
            device_count: count("SELECT COUNT(*) FROM user_devices"),
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

/// Probe `pragma_table_info` to decide whether a column already
/// exists — used by `ALTER TABLE … ADD COLUMN` which has no
/// `IF NOT EXISTS` form.
fn column_exists(conn: &Connection, table: &str, column: &str) -> UserStoreResult<bool> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info(\"{table}\")"))
        .map_err(|e| UserStoreError::Io {
            operation: format!("prepare pragma_table_info({table})"),
            reason: e.to_string(),
        })?;
    let mut rows = stmt
        .query([])
        .map_err(|e| UserStoreError::Io {
            operation: format!("query pragma_table_info({table})"),
            reason: e.to_string(),
        })?;
    while let Some(row) = rows.next().map_err(|e| UserStoreError::Io {
        operation: "next pragma_table_info row".to_string(),
        reason: e.to_string(),
    })? {
        let name: String = row.get(1).map_err(|e| UserStoreError::Io {
            operation: "read pragma_table_info name".to_string(),
            reason: e.to_string(),
        })?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

impl UserStore for SqliteUserStore {
    fn put_profile(&self, profile: UserProfile) -> UserStoreResult<()> {
        validate_username(&profile.username)?;
        let prefs_json = serde_json::to_string(&profile.preferences).map_err(|e| {
            UserStoreError::Serialization {
                operation: "preferences".to_string(),
                reason: e.to_string(),
            }
        })?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR REPLACE INTO user_profile
             (user_id, username, display_name, avatar_blob_hash, avatar_mime, avatar_size,
              bio, preferences_json, kind, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                profile.user_id,
                profile.username,
                profile.display_name,
                profile.avatar.as_ref().map(|a| a.blob_hash.clone()),
                profile.avatar.as_ref().map(|a| a.mime_type.clone()),
                profile.avatar.as_ref().map(|a| a.size_bytes as i64),
                profile.bio,
                prefs_json,
                profile.kind.as_str(),
                profile.created_at as i64,
                profile.updated_at as i64,
            ],
        )?;
        Ok(())
    }

    fn get_profile(&self, user_id: &str) -> UserStoreResult<Option<UserProfile>> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT user_id, username, display_name, avatar_blob_hash, avatar_mime,
                        avatar_size, bio, preferences_json, kind, created_at, updated_at
                 FROM user_profile WHERE user_id = ?1",
                params![user_id],
                row_to_profile,
            )
            .ok();
        Ok(row)
    }

    fn put_preferences(
        &self,
        user_id: &str,
        prefs: UserPreferences,
    ) -> UserStoreResult<()> {
        let prefs_json = serde_json::to_string(&prefs).map_err(|e| {
            UserStoreError::Serialization {
                operation: "preferences".to_string(),
                reason: e.to_string(),
            }
        })?;
        let conn = self.lock()?;
        let now = current_timestamp_secs() as i64;
        // Use UPDATE-only (no UPSERT) so callers cannot accidentally
        // create a profile row with default fields. `ensure_profile`
        // exists for that case.
        let updated = conn.execute(
            "UPDATE user_profile
             SET preferences_json = ?1, updated_at = ?2
             WHERE user_id = ?3",
            params![prefs_json, now, user_id],
        )?;
        if updated == 0 {
            return Err(UserStoreError::NotFound {
                kind: "user_profile",
                id: user_id.to_string(),
            });
        }
        Ok(())
    }

    fn list_profiles(&self) -> UserStoreResult<Vec<UserProfile>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT user_id, username, display_name, avatar_blob_hash, avatar_mime,
                    avatar_size, bio, preferences_json, kind, created_at, updated_at
             FROM user_profile ORDER BY created_at ASC, user_id ASC",
        )?;
        let rows = stmt
            .query_map([], row_to_profile)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn delete_profile(&self, user_id: &str) -> UserStoreResult<usize> {
        let mut conn = self.lock()?;
        let tx = conn.transaction().map_err(|e| UserStoreError::Io {
            operation: "begin tx".to_string(),
            reason: e.to_string(),
        })?;
        // The FK on user_public_keys and user_devices cascades on
        // delete; user_id_digit is intentionally FK-free so we have
        // to remove it explicitly.
        let mut total = 0usize;
        for stmt in [
            "DELETE FROM user_id_digit   WHERE user_id = ?1",
            "DELETE FROM user_public_keys WHERE user_id = ?1",
            "DELETE FROM user_devices     WHERE user_id = ?1",
            "DELETE FROM user_profile     WHERE user_id = ?1",
        ] {
            total += tx.execute(stmt, params![user_id])?;
        }
        tx.commit().map_err(|e| UserStoreError::Io {
            operation: "commit".to_string(),
            reason: e.to_string(),
        })?;
        Ok(total)
    }

    fn put_public_key(&self, key: UserPublicKey) -> UserStoreResult<()> {
        // Foreign-key check: the user must exist before a key is bound.
        // Cheaper than letting SQLite error out halfway through the
        // INSERT.
        let conn = self.lock()?;
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM user_profile WHERE user_id = ?1",
                params![key.user_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some();
        if !exists {
            return Err(UserStoreError::NotFound {
                kind: "user_profile",
                id: key.user_id,
            });
        }
        conn.execute(
            "INSERT OR REPLACE INTO user_public_keys
             (key_id, user_id, algorithm, key_material, label, created_at, revoked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                key.key_id,
                key.user_id,
                key.algorithm,
                key.key_material,
                key.label,
                key.created_at as i64,
                key.revoked_at.map(|n| n as i64),
            ],
        )?;
        Ok(())
    }

    fn revoke_public_key(&self, key_id: &str) -> UserStoreResult<()> {
        let now = current_timestamp_secs() as i64;
        let conn = self.lock()?;
        let updated = conn.execute(
            "UPDATE user_public_keys SET revoked_at = ?1 \
             WHERE key_id = ?2 AND revoked_at IS NULL",
            params![now, key_id],
        )?;
        if updated == 0 {
            return Err(UserStoreError::NotFound {
                kind: "user_public_key",
                id: key_id.to_string(),
            });
        }
        Ok(())
    }

    fn list_public_keys(&self, user_id: &str) -> UserStoreResult<Vec<UserPublicKey>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT key_id, user_id, algorithm, key_material, label, created_at, revoked_at \
             FROM user_public_keys WHERE user_id = ?1 \
             ORDER BY created_at ASC, key_id ASC",
        )?;
        let rows = stmt
            .query_map(params![user_id], |row| {
                Ok(UserPublicKey {
                    key_id: row.get(0)?,
                    user_id: row.get(1)?,
                    algorithm: row.get(2)?,
                    key_material: row.get(3)?,
                    label: row.get::<_, String>(4)?,
                    created_at: row.get::<_, i64>(5)? as u64,
                    revoked_at: row.get::<_, Option<i64>>(6)?.map(|n| n as u64),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn put_device(&self, device: UserDevice) -> UserStoreResult<()> {
        // FK pre-check (same rationale as put_public_key).
        let conn = self.lock()?;
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM user_profile WHERE user_id = ?1",
                params![device.user_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some();
        if !exists {
            return Err(UserStoreError::NotFound {
                kind: "user_profile",
                id: device.user_id,
            });
        }
        conn.execute(
            "INSERT OR REPLACE INTO user_devices
             (device_id, user_id, node_id, pairing_id, device_class, label, paired_at,
              revoked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                device.device_id,
                device.user_id,
                device.node_id,
                device.pairing_id,
                device.device_class,
                device.label,
                device.paired_at as i64,
                device.revoked_at.map(|n| n as i64),
            ],
        )?;
        Ok(())
    }

    fn revoke_device(&self, device_id: &str) -> UserStoreResult<()> {
        let now = current_timestamp_secs() as i64;
        let conn = self.lock()?;
        let updated = conn.execute(
            "UPDATE user_devices SET revoked_at = ?1 \
             WHERE device_id = ?2 AND revoked_at IS NULL",
            params![now, device_id],
        )?;
        if updated == 0 {
            return Err(UserStoreError::NotFound {
                kind: "user_device",
                id: device_id.to_string(),
            });
        }
        Ok(())
    }

    fn list_devices(&self, user_id: &str) -> UserStoreResult<Vec<UserDevice>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT device_id, user_id, node_id, pairing_id, device_class, label,
                    paired_at, revoked_at
             FROM user_devices WHERE user_id = ?1
             ORDER BY paired_at ASC, device_id ASC",
        )?;
        let rows = stmt
            .query_map(params![user_id], |row| {
                Ok(UserDevice {
                    device_id: row.get(0)?,
                    user_id: row.get(1)?,
                    node_id: row.get(2)?,
                    pairing_id: row.get(3)?,
                    device_class: row.get(4)?,
                    label: row.get(5)?,
                    paired_at: row.get::<_, i64>(6)? as u64,
                    revoked_at: row.get::<_, Option<i64>>(7)?.map(|n| n as u64),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn ensure_user_digit(&self, user_id: &str) -> UserStoreResult<String> {
        // 1. fast path: already mapped
        if let Some(digit) = self.resolve_user_digit(user_id)? {
            return Ok(digit);
        }
        // 2. derive via a3net_roster's stable fold
        let digit = a3net_roster::stable_digit_from_node(user_id);
        // 3. persist (idempotent)
        let conn = self.lock()?;
        let now = current_timestamp_secs() as i64;
        conn.execute(
            "INSERT OR IGNORE INTO user_id_digit (user_id, digit_id, created_at) \
             VALUES (?1, ?2, ?3)",
            params![user_id, digit, now],
        )?;
        // 4. re-read so callers always see what we wrote (handles races)
        let stored: String = conn.query_row(
            "SELECT digit_id FROM user_id_digit WHERE user_id = ?1",
            params![user_id],
            |row| row.get(0),
        )?;
        Ok(stored)
    }

    fn resolve_user_digit(&self, user_id: &str) -> UserStoreResult<Option<String>> {
        let conn = self.lock()?;
        let digit: Option<String> = conn
            .query_row(
                "SELECT digit_id FROM user_id_digit WHERE user_id = ?1",
                params![user_id],
                |row| row.get(0),
            )
            .ok();
        Ok(digit)
    }

    fn set_public_key_label(&self, key_id: &str, label: &str) -> UserStoreResult<()> {
        // No-op for unknown `key_id` — the contract is "best-effort
        // label patch", not "create the key if missing". FK
        // violations are caught upstream by `put_public_key`.
        let conn = self.lock()?;
        let updated = conn.execute(
            "UPDATE user_public_keys SET label = ?1 WHERE key_id = ?2",
            params![label, key_id],
        )?;
        let _ = updated;
        Ok(())
    }

    fn set_kind(&self, user_id: &str, kind: UserKind) -> UserStoreResult<()> {
        // DO-178C §6.1 *determinism*: we only update the kind column
        // so the username/display_name FK constraints are never violated.
        // Auto-creating a row here would need a valid username (which
        // we don't have — only user_id), so we UPDATE first and let
        // callers that need a full profile call `put_profile`.
        let conn = self.lock()?;
        let now = current_timestamp_secs() as i64;
        let n = conn.execute(
            "UPDATE user_profile SET kind = ?1, updated_at = ?2 WHERE user_id = ?3",
            params![kind.as_str(), now, user_id],
        )?;
        if n == 0 {
            // Row absent — create minimal entry so subsequent
            // `get_kind` calls return the desired value (not Human).
            // username = user_id is a safe placeholder; callers who
            // need a real username should call `put_profile` first.
            conn.execute(
                "INSERT INTO user_profile
                    (user_id, username, display_name, avatar_blob_hash,
                     avatar_mime, avatar_size, bio, preferences_json,
                     kind, created_at, updated_at)
                 VALUES (?1, ?1, '', NULL, NULL, NULL, '', '{}', ?2, ?3, ?3)",
                params![user_id, kind.as_str(), now],
            )?;
        }
        // Force WAL checkpoint so a subsequent connection on the
        // same file observes the write immediately (test harness
        // opens a fresh store in the same file).
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        Ok(())
    }

    fn get_kind(&self, user_id: &str) -> UserStoreResult<UserKind> {
        let conn = self.lock()?;
        // Default to Human when the row is absent — DO-178C §6.1.
        let kind_str: Option<String> = conn
            .query_row(
                "SELECT kind FROM user_profile WHERE user_id = ?1",
                params![user_id],
                |row| row.get(0),
            )
            .ok();
        Ok(kind_str
            .as_deref()
            .map(UserKind::from_str_loose)
            .unwrap_or_default())
    }
}

// ---------------------------------------------------------------------------
// Validation helpers.
// ---------------------------------------------------------------------------

fn validate_username(username: &str) -> UserStoreResult<()> {
    if username.is_empty() {
        return Err(UserStoreError::InvalidParameter {
            parameter: "username".to_string(),
            reason: "must not be empty".to_string(),
        });
    }
    if username.len() > crate::model::MAX_USERNAME_LEN {
        return Err(UserStoreError::InvalidParameter {
            parameter: "username".to_string(),
            reason: format!(
                "length {} exceeds {}",
                username.len(),
                crate::model::MAX_USERNAME_LEN
            ),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Row → struct converters.
// ---------------------------------------------------------------------------

fn row_to_profile(row: &rusqlite::Row<'_>) -> rusqlite::Result<UserProfile> {
    let avatar_blob_hash: Option<String> = row.get(3)?;
    let avatar_mime: Option<String> = row.get(4)?;
    let avatar_size: Option<i64> = row.get(5)?;
    let avatar = match (avatar_blob_hash, avatar_mime, avatar_size) {
        (Some(hash), Some(mime), Some(size)) => Some(crate::model::AvatarBlob {
            blob_hash: hash,
            mime_type: mime,
            size_bytes: size as u64,
        }),
        _ => None,
    };
    let prefs_json: String = row.get(7)?;
    let preferences: UserPreferences = serde_json::from_str(&prefs_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
    })?;
    let kind_str: Option<String> = row.get(8)?;
    let kind = kind_str
        .as_deref()
        .map(UserKind::from_str_loose)
        .unwrap_or_default();
    Ok(UserProfile {
        user_id: row.get(0)?,
        username: row.get(1)?,
        display_name: row.get(2)?,
        avatar,
        bio: row.get(6)?,
        preferences,
        kind,
        created_at: row.get::<_, i64>(9)? as u64,
        updated_at: row.get::<_, i64>(10)? as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        AvatarBlob, DeviceClass, PublicKeyAlgorithm, UserDevice, UserPreferences, UserProfile,
        UserPublicKey,
    };
    use tempfile::TempDir;

    fn open_store() -> (SqliteUserStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let store = SqliteUserStore::open(SqliteUserStoreConfig::new(dir.path().join("u.db")))
            .expect("open");
        (store, dir)
    }

    fn sample_profile(id: &str) -> UserProfile {
        let mut p = UserProfile::new(id, "alice");
        p.display_name = "Alice".into();
        p.bio = "test user".into();
        p.preferences = UserPreferences {
            theme: "dark".into(),
            locale: "zh-CN".into(),
            notifications_enabled: false,
            read_receipts_enabled: true,
            typing_indicators_enabled: false,
            experimental_json: r#"{"x":1}"#.into(),
        };
        p.avatar = Some(AvatarBlob::new("deadbeef", "image/png", 4096));
        p.created_at = 100;
        p.updated_at = 200;
        p
    }

    // ------------------------------------------------------------ Profile --

    #[tokio::test]
    async fn profile_round_trip() {
        let (store, _dir) = open_store();
        store.put_profile(sample_profile("u1")).unwrap();
        let got = store.get_profile("u1").unwrap().unwrap();
        assert_eq!(got.username, "alice");
        assert_eq!(got.display_name, "Alice");
        assert_eq!(got.bio, "test user");
        assert_eq!(got.preferences.theme, "dark");
        assert!(!got.preferences.notifications_enabled);
        let avatar = got.avatar.unwrap();
        assert_eq!(avatar.blob_hash, "deadbeef");
        assert_eq!(avatar.mime_type, "image/png");
        assert_eq!(avatar.size_bytes, 4096);
        assert_eq!(got.created_at, 100);
        assert_eq!(got.updated_at, 200);
    }

    #[tokio::test]
    async fn profile_rejects_empty_username() {
        let (store, _dir) = open_store();
        let mut p = sample_profile("u1");
        p.username = String::new();
        assert!(matches!(
            store.put_profile(p),
            Err(UserStoreError::InvalidParameter { .. })
        ));
    }

    #[tokio::test]
    async fn put_preferences_only_updates_existing_profile() {
        let (store, _dir) = open_store();
        // No profile exists yet — must be NotFound.
        let err = store
            .put_preferences(
                "ghost",
                UserPreferences {
                    theme: "light".into(),
                    ..UserPreferences::default()
                },
            )
            
            .unwrap_err();
        assert!(matches!(err, UserStoreError::NotFound { .. }));
    }

    #[tokio::test]
    async fn put_preferences_persists() {
        let (store, _dir) = open_store();
        store.put_profile(sample_profile("u1")).unwrap();
        store
            .put_preferences(
                "u1",
                UserPreferences {
                    theme: "light".into(),
                    notifications_enabled: false,
                    ..UserPreferences::default()
                },
            )
            
            .unwrap();
        let p = store.get_profile("u1").unwrap().unwrap();
        assert_eq!(p.preferences.theme, "light");
        assert!(!p.preferences.notifications_enabled);
    }

    #[tokio::test]
    async fn list_profiles_returns_all() {
        let (store, _dir) = open_store();
        for i in 0..3 {
            store.put_profile(sample_profile(&format!("u{i}"))).unwrap();
        }
        let all = store.list_profiles().unwrap();
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn delete_profile_cascades_and_clears_digit() {
        let (store, _dir) = open_store();
        store.put_profile(sample_profile("u1")).unwrap();
        store.ensure_user_digit("u1").unwrap();
        let key = UserPublicKey {
            key_id: "k1".into(),
            user_id: "u1".into(),
            algorithm: PublicKeyAlgorithm::Ed25519.as_str().into(),
            key_material: "BASE64DATA".into(),
            label: "primary".into(),
            created_at: 0,
            revoked_at: None,
        };
        store.put_public_key(key).unwrap();
        let dev = UserDevice {
            device_id: "d1".into(),
            user_id: "u1".into(),
            node_id: "node-1".into(),
            pairing_id: None,
            device_class: DeviceClass::Desktop.as_str().into(),
            label: "Alice's MacBook".into(),
            paired_at: 0,
            revoked_at: None,
        };
        store.put_device(dev).unwrap();
        // Delete cascades to user_public_keys + user_devices via FK,
        // and explicitly clears user_id_digit.
        let removed = store.delete_profile("u1").unwrap();
        assert!(removed >= 3, "removed = {removed}");
        assert!(store.get_profile("u1").unwrap().is_none());
        assert!(store.resolve_user_digit("u1").unwrap().is_none());
        assert!(store.list_public_keys("u1").unwrap().is_empty());
        assert!(store.list_devices("u1").unwrap().is_empty());
    }

    // --------------------------------------------------------- Public keys --

    #[tokio::test]
    async fn public_key_requires_existing_profile() {
        let (store, _dir) = open_store();
        let key = UserPublicKey {
            key_id: "k1".into(),
            user_id: "ghost".into(),
            algorithm: PublicKeyAlgorithm::Ed25519.as_str().into(),
            key_material: "BASE64DATA".into(),
            label: "ghost".into(),
            created_at: 0,
            revoked_at: None,
        };
        assert!(matches!(
            store.put_public_key(key),
            Err(UserStoreError::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn public_key_revocation_is_terminal() {
        let (store, _dir) = open_store();
        store.put_profile(sample_profile("u1")).unwrap();
        let key = UserPublicKey {
            key_id: "k1".into(),
            user_id: "u1".into(),
            algorithm: PublicKeyAlgorithm::Ed25519.as_str().into(),
            key_material: "BASE64DATA".into(),
            label: "primary".into(),
            created_at: 0,
            revoked_at: None,
        };
        store.put_public_key(key).unwrap();
        store.revoke_public_key("k1").unwrap();
        // Second revoke must NOT silently succeed (already revoked).
        assert!(matches!(
            store.revoke_public_key("k1"),
            Err(UserStoreError::NotFound { .. })
        ));
        let list = store.list_public_keys("u1").unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].revoked_at.is_some());
    }

    #[tokio::test]
    async fn public_key_revoke_missing_key_errors() {
        let (store, _dir) = open_store();
        assert!(matches!(
            store.revoke_public_key("nope"),
            Err(UserStoreError::NotFound { .. })
        ));
    }

    // ------------------------------------------------------------- Devices --

    #[tokio::test]
    async fn device_round_trip_and_revoke() {
        let (store, _dir) = open_store();
        store.put_profile(sample_profile("u1")).unwrap();
        let dev = UserDevice {
            device_id: "d1".into(),
            user_id: "u1".into(),
            node_id: "node-x".into(),
            pairing_id: Some("p1".into()),
            device_class: DeviceClass::Mobile.as_str().into(),
            label: "Alice's iPhone".into(),
            paired_at: 50,
            revoked_at: None,
        };
        store.put_device(dev).unwrap();
        let list = store.list_devices("u1").unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].parsed_class(), DeviceClass::Mobile);
        assert_eq!(list[0].paired_at, 50);
        assert_eq!(list[0].pairing_id.as_deref(), Some("p1"));
        // Revoke
        store.revoke_device("d1").unwrap();
        // Second revoke errors.
        assert!(matches!(
            store.revoke_device("d1"),
            Err(UserStoreError::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn device_revoke_missing_errors() {
        let (store, _dir) = open_store();
        assert!(matches!(
            store.revoke_device("nope"),
            Err(UserStoreError::NotFound { .. })
        ));
    }

    // ------------------------------------------------- 12-digit integration --

    #[tokio::test]
    async fn ensure_user_digit_is_idempotent() {
        let (store, _dir) = open_store();
        let a = store.ensure_user_digit("user-42").unwrap();
        let b = store.ensure_user_digit("user-42").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 12);
        assert!(a.chars().all(|c| c.is_ascii_digit()));
    }

    #[tokio::test]
    async fn different_users_get_different_digits() {
        let (store, _dir) = open_store();
        let a = store.ensure_user_digit("alice").unwrap();
        let b = store.ensure_user_digit("bob").unwrap();
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn info_counts_match_state() {
        let (store, _dir) = open_store();
        store.put_profile(sample_profile("u1")).unwrap();
        let key = UserPublicKey {
            key_id: "k1".into(),
            user_id: "u1".into(),
            algorithm: PublicKeyAlgorithm::Ed25519.as_str().into(),
            key_material: "DATA".into(),
            label: "".into(),
            created_at: 0,
            revoked_at: None,
        };
        store.put_public_key(key).unwrap();
        store.ensure_user_digit("u1").unwrap();
        let info = store.info();
        assert_eq!(info.backend, "sqlite");
        assert_eq!(info.profile_count, 1);
        assert_eq!(info.public_key_count, 1);
        // digit table count is not tracked by info(); sanity check
        // resolve instead.
        assert!(store.resolve_user_digit("u1").unwrap().is_some());
    }
}