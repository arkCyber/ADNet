//! SQLite persistence for the channel / public-account namespace
//! (F-09).
//!
//! Mirrors the design of [`a3net_chatstore::storage::ChatStorage`]
//! and [`a3net_socialfeed::storage::SocialFeedStorage`]: one
//! `ChannelStorage` per node, single SQLite file under
//! `config.storage_dir`, WAL + `foreign_keys=ON` +
//! `synchronous=NORMAL` at open time, and a `PRAGMA integrity_check`
//! probe at startup so a corrupt DB refuses to open instead of
//! returning per-row errors.
//!
//! # Tables
//!
//! - `public_accounts`     — one row per [`PublicAccount`].
//! - `account_subscriptions` — join table `(subscriber_id,
//!   account_id) → Subscription`; unique on
//!   `(subscriber_id, account_id)` so a duplicate `subscribe` call
//!   is a no-op (matching the WeChat UX).
//! - `feed_items`          — one row per [`FeedItem`].
//! - `feed_read_cursors`   — `(subscriber_id, account_id) →
//!   last_read_seq`; cheap unread-count without scanning the
//!   timeline.
//! - `feed_recipients`     — one row per (subscriber, feed_id)
//!   recorded on `mark_read`; used to compute `read_count` /
//!   `unique_readers` audit stats.
//!
//! All public methods are synchronous — the `Mutex<Connection>` is
//! never held across `.await` points, mirroring the rest of the
//! app's storage layer.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use tracing::{debug, info};

use a3chat_core::channel::{
    AccountKind, FeedAttachment, FeedItem, PublicAccount, Subscription, VerificationLevel,
    ACCOUNT_ID_PREFIX, FEED_ID_PREFIX,
};
use a3chat_core::error::A3chatError;

use crate::error::{AppError, AppResult};

/// Current schema version. Bump on every schema change.
pub const SCHEMA_VERSION: u32 = 1;

/// Configuration for [`ChannelStorage`].
#[derive(Debug, Clone)]
pub struct ChannelStorageConfig {
    /// Directory holding the SQLite file. Created if missing.
    pub storage_dir: PathBuf,
    /// Override the SQLite file name (mainly used by tests so
    /// parallel runs can't collide).
    pub filename: String,
}

impl ChannelStorageConfig {
    /// Build a config rooted under `<base>/channel`. The
    /// `a3chat-app` bootstrap calls this from the chat-storage
    /// base dir so channel data lives next to (but not inside) the
    /// chat DB.
    pub fn under_base(base: &Path) -> Self {
        Self {
            storage_dir: base.join("channel"),
            filename: "channel.db".into(),
        }
    }

    fn db_path(&self) -> PathBuf {
        self.storage_dir.join(&self.filename)
    }
}

impl Default for ChannelStorageConfig {
    fn default() -> Self {
        let mut storage_dir = std::env::temp_dir();
        storage_dir.push("a3chat_channel");
        Self {
            storage_dir,
            filename: "channel.db".into(),
        }
    }
}

/// SQLite-backed channel persistence. Cheap to clone — it holds a
/// `Arc<Mutex<Connection>>` so the lock is process-wide.
#[derive(Debug, Clone)]
pub struct ChannelStorage {
    db: Arc<Mutex<Connection>>,
    config: ChannelStorageConfig,
}

impl ChannelStorage {
    /// Open (or create) the database at the configured path.
    pub fn open(config: ChannelStorageConfig) -> AppResult<Self> {
        std::fs::create_dir_all(&config.storage_dir).map_err(AppError::from)?;
        let db_path = config.db_path();
        let conn = Connection::open(&db_path).map_err(AppError::from)?;
        configure_connection(&conn).map_err(AppError::from)?;
        apply_schema(&conn).map_err(AppError::from)?;

        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(AppError::from)?;
        if integrity != "ok" {
            return Err(AppError::Storage(format!(
                "channel integrity_check failed: {integrity}"
            )));
        }

        info!(path = %db_path.display(), "channel storage opened");
        Ok(Self {
            db: Arc::new(Mutex::new(conn)),
            config,
        })
    }

    /// Configuration this storage was opened with.
    pub fn config(&self) -> &ChannelStorageConfig {
        &self.config
    }

    /// Storage path (file path of the SQLite database).
    pub fn db_path(&self) -> PathBuf {
        self.config.db_path()
    }

    /// Storage directory (parent of the SQLite file).
    pub fn storage_dir(&self) -> &Path {
        &self.config.storage_dir
    }

    /// Acquire the underlying mutex. Provided for callers that need
    /// to chain transactions (e.g. service tests).
    pub fn handle(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|poison| {
            tracing::error!(error = %poison, "channel storage mutex poisoned");
            poison.into_inner()
        })
    }

    // ── Public accounts ─────────────────────────────────────────────

    /// Insert or replace a public account. Returns the persisted
    /// account (with the caller-supplied fields).
    pub fn put_account(&self, account: &PublicAccount) -> AppResult<()> {
        account.validate().map_err(AppError::from)?;
        let conn = self.handle();
        let tags_json = serde_json::to_string(&account.tags).map_err(AppError::from)?;
        // Plain `INSERT` (not `INSERT OR REPLACE`) so a second
        // account with the same `owner_node_id` but a different
        // `account_id` surfaces as a `UNIQUE` constraint failure
        // — the service layer turns that into a 409-style domain
        // error. The caller is expected to call `get_account_by_owner`
        // first if "re-register same owner" is the intent.
        conn.execute(
            "INSERT INTO public_accounts
                (account_id, owner_node_id, name, bio, avatar_hash, tags_json,
                 kind, verification, sequence, subscriber_count,
                 created_at_unix, updated_at_unix)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(account_id) DO UPDATE SET
                 owner_node_id    = excluded.owner_node_id,
                 name             = excluded.name,
                 bio              = excluded.bio,
                 avatar_hash      = excluded.avatar_hash,
                 tags_json        = excluded.tags_json,
                 kind             = excluded.kind,
                 verification     = excluded.verification,
                 updated_at_unix  = excluded.updated_at_unix",
            params![
                account.account_id,
                account.owner_node_id,
                account.name,
                account.bio,
                account.avatar_hash,
                tags_json,
                account.kind.as_str(),
                account.verification.as_str(),
                account.sequence as i64,
                account.subscriber_count as i64,
                account.created_at.timestamp(),
                account.updated_at.timestamp(),
            ],
        )
        .map_err(AppError::from)?;
        debug!(account_id = %account.account_id, "put_account committed");
        Ok(())
    }

    /// Fetch a public account by id. `Ok(None)` if not present.
    pub fn get_account(&self, account_id: &str) -> AppResult<Option<PublicAccount>> {
        let conn = self.handle();
        let row = conn
            .query_row(
                "SELECT account_id, owner_node_id, name, bio, avatar_hash, tags_json,
                        kind, verification, sequence, subscriber_count,
                        created_at_unix, updated_at_unix
                 FROM public_accounts WHERE account_id = ?1",
                params![account_id],
                |row| {
                    let tags_json: String = row.get(5)?;
                    Ok(account_row_to_record(row, tags_json))
                },
            )
            .optional()
            .map_err(AppError::from)?;
        match row {
            None => Ok(None),
            Some((acc, _tags)) => {
                acc.validate().map_err(AppError::from)?;
                Ok(Some(acc))
            }
        }
    }

    /// Fetch the account owned by `owner_node_id`. There can be at
    /// most one account per owner — the service layer rejects a
    /// second `register` from the same node, but the storage layer
    /// also enforces it via `UNIQUE(owner_node_id)`.
    pub fn get_account_by_owner(&self, owner_node_id: &str) -> AppResult<Option<PublicAccount>> {
        let conn = self.handle();
        let row = conn
            .query_row(
                "SELECT account_id, owner_node_id, name, bio, avatar_hash, tags_json,
                        kind, verification, sequence, subscriber_count,
                        created_at_unix, updated_at_unix
                 FROM public_accounts WHERE owner_node_id = ?1",
                params![owner_node_id],
                |row| {
                    let tags_json: String = row.get(5)?;
                    Ok(account_row_to_record(row, tags_json))
                },
            )
            .optional()
            .map_err(AppError::from)?;
        match row {
            None => Ok(None),
            Some((acc, _tags)) => {
                acc.validate().map_err(AppError::from)?;
                Ok(Some(acc))
            }
        }
    }

    /// List accounts, newest first, with an optional cap.
    pub fn list_accounts(&self, limit: u32) -> AppResult<Vec<PublicAccount>> {
        let limit = limit.clamp(1, 1000);
        let conn = self.handle();
        let mut stmt = conn
            .prepare(
                "SELECT account_id, owner_node_id, name, bio, avatar_hash, tags_json,
                        kind, verification, sequence, subscriber_count,
                        created_at_unix, updated_at_unix
                 FROM public_accounts
                 ORDER BY created_at_unix DESC
                 LIMIT ?1",
            )
            .map_err(AppError::from)?;
        let rows = stmt
            .query_map(params![limit as i64], |row| {
                let tags_json: String = row.get(5)?;
                Ok(account_row_to_record(row, tags_json))
            })
            .map_err(AppError::from)?;
        let mut out = Vec::new();
        for r in rows {
            let (a, _) = r.map_err(AppError::from)?;
            a.validate().map_err(AppError::from)?;
            out.push(a);
        }
        Ok(out)
    }

    /// Case-insensitive substring search over name / bio / owner.
    /// Caller is responsible for the needle length / safety check
    /// (the service does this in `validate`).
    pub fn search_accounts(
        &self,
        needle: &str,
        limit: u32,
    ) -> AppResult<Vec<PublicAccount>> {
        let limit = limit.clamp(1, 1000);
        let pat = format!("%{}%", needle.to_ascii_lowercase());
        let conn = self.handle();
        let mut stmt = conn
            .prepare(
                "SELECT account_id, owner_node_id, name, bio, avatar_hash, tags_json,
                        kind, verification, sequence, subscriber_count,
                        created_at_unix, updated_at_unix
                 FROM public_accounts
                 WHERE LOWER(name) LIKE ?1
                    OR LOWER(IFNULL(bio, '')) LIKE ?1
                    OR LOWER(owner_node_id) LIKE ?1
                 ORDER BY subscriber_count DESC, created_at_unix DESC
                 LIMIT ?2",
            )
            .map_err(AppError::from)?;
        let rows = stmt
            .query_map(params![pat, limit as i64], |row| {
                let tags_json: String = row.get(5)?;
                Ok(account_row_to_record(row, tags_json))
            })
            .map_err(AppError::from)?;
        let mut out = Vec::new();
        for r in rows {
            let (a, _) = r.map_err(AppError::from)?;
            a.validate().map_err(AppError::from)?;
            out.push(a);
        }
        Ok(out)
    }

    /// Delete a public account by id. Returns `true` if a row was
    /// actually removed.
    pub fn delete_account(&self, account_id: &str) -> AppResult<bool> {
        let conn = self.handle();
        let tx = conn.unchecked_transaction().map_err(AppError::from)?;
        let n = tx
            .execute(
                "DELETE FROM public_accounts WHERE account_id = ?1",
                params![account_id],
            )
            .map_err(AppError::from)?;
        // Cascade: drop subscriptions + feed rows so a re-register
        // does not see stale state. The owner is responsible for
        // any gossip retraction at the network layer.
        let _ = tx.execute(
            "DELETE FROM account_subscriptions WHERE account_id = ?1",
            params![account_id],
        );
        let _ = tx.execute(
            "DELETE FROM feed_items WHERE account_id = ?1",
            params![account_id],
        );
        let _ = tx.execute(
            "DELETE FROM feed_read_cursors WHERE account_id = ?1",
            params![account_id],
        );
        let _ = tx.execute(
            "DELETE FROM feed_recipients WHERE account_id = ?1",
            params![account_id],
        );
        tx.commit().map_err(AppError::from)?;
        Ok(n > 0)
    }

    /// Bump `sequence` and `updated_at` for an account after a feed
    /// publish. Returns the new sequence number.
    pub fn bump_account_sequence(
        &self,
        account_id: &str,
        now: DateTime<Utc>,
    ) -> AppResult<u32> {
        let conn = self.handle();
        let tx = conn.unchecked_transaction().map_err(AppError::from)?;
        // Take the next sequence atomically. `UPDATE … RETURNING`
        // keeps the read-then-write in a single statement so two
        // concurrent publishes can't mint the same sequence.
        let next: i64 = tx
            .query_row(
                "UPDATE public_accounts
                 SET sequence = sequence + 1,
                     updated_at_unix = ?1
                 WHERE account_id = ?2
                 RETURNING sequence",
                params![now.timestamp(), account_id],
                |row| row.get(0),
            )
            .map_err(AppError::from)?;
        tx.commit().map_err(AppError::from)?;
        Ok(next as u32)
    }

    /// Update the `subscriber_count` field to mirror the actual
    /// `account_subscriptions` row count. Called after
    /// subscribe / unsubscribe so a publish-time query gets an
    /// accurate "X followers" badge.
    pub fn recompute_subscriber_count(&self, account_id: &str) -> AppResult<u32> {
        let conn = self.handle();
        let tx = conn.unchecked_transaction().map_err(AppError::from)?;
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM account_subscriptions WHERE account_id = ?1",
                params![account_id],
                |row| row.get(0),
            )
            .map_err(AppError::from)?;
        tx.execute(
            "UPDATE public_accounts SET subscriber_count = ?1 WHERE account_id = ?2",
            params![count, account_id],
        )
        .map_err(AppError::from)?;
        tx.commit().map_err(AppError::from)?;
        Ok(count as u32)
    }

    // ── Subscriptions ───────────────────────────────────────────────

    /// Insert (or update) a subscription. The unique constraint
    /// `(subscriber_id, account_id)` turns a re-`subscribe` into a
    /// no-op for the `account_id`; the `alias` / `notify_mode` /
    /// `is_pinned` columns are still updated.
    pub fn put_subscription(&self, sub: &Subscription) -> AppResult<()> {
        if sub.subscriber_id.is_empty() {
            return Err(AppError::Domain("subscription.subscriber_id: empty".into()));
        }
        if sub.account_id.is_empty() {
            return Err(AppError::Domain("subscription.account_id: empty".into()));
        }
        let conn = self.handle();
        conn.execute(
            "INSERT INTO account_subscriptions
                (subscriber_id, account_id, alias, notify_mode, is_muted, is_pinned,
                 subscribed_at_unix, last_read_seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(subscriber_id, account_id) DO UPDATE SET
                 alias          = excluded.alias,
                 notify_mode    = excluded.notify_mode,
                 is_muted       = excluded.is_muted,
                 is_pinned      = excluded.is_pinned,
                 last_read_seq  = MAX(account_subscriptions.last_read_seq,
                                      excluded.last_read_seq)",
            params![
                sub.subscriber_id,
                sub.account_id,
                sub.alias,
                sub.notify_mode,
                sub.is_muted,
                sub.is_pinned,
                sub.subscribed_at.timestamp(),
                sub.last_read_seq as i64,
            ],
        )
        .map_err(AppError::from)?;
        Ok(())
    }

    /// Look up a single subscription. `Ok(None)` if not present.
    pub fn get_subscription(
        &self,
        subscriber_id: &str,
        account_id: &str,
    ) -> AppResult<Option<Subscription>> {
        let conn = self.handle();
        let row = conn
            .query_row(
                "SELECT subscriber_id, account_id, alias, notify_mode, is_muted, is_pinned,
                        subscribed_at_unix, last_read_seq
                 FROM account_subscriptions
                 WHERE subscriber_id = ?1 AND account_id = ?2",
                params![subscriber_id, account_id],
                |row| Ok(subscription_row_to_record(row)),
            )
            .optional()
            .map_err(AppError::from)?;
        Ok(row)
    }

    /// All subscriptions of a subscriber (newest first).
    pub fn list_subscriptions(&self, subscriber_id: &str) -> AppResult<Vec<Subscription>> {
        let conn = self.handle();
        let mut stmt = conn
            .prepare(
                "SELECT subscriber_id, account_id, alias, notify_mode, is_muted, is_pinned,
                        subscribed_at_unix, last_read_seq
                 FROM account_subscriptions
                 WHERE subscriber_id = ?1
                 ORDER BY subscribed_at_unix DESC",
            )
            .map_err(AppError::from)?;
        let rows = stmt
            .query_map(params![subscriber_id], |row| {
                Ok(subscription_row_to_record(row))
            })
            .map_err(AppError::from)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(AppError::from)?);
        }
        Ok(out)
    }

    /// All subscribers of an account (newest first).
    pub fn list_subscribers_of(&self, account_id: &str) -> AppResult<Vec<Subscription>> {
        let conn = self.handle();
        let mut stmt = conn
            .prepare(
                "SELECT subscriber_id, account_id, alias, notify_mode, is_muted, is_pinned,
                        subscribed_at_unix, last_read_seq
                 FROM account_subscriptions
                 WHERE account_id = ?1
                 ORDER BY subscribed_at_unix DESC",
            )
            .map_err(AppError::from)?;
        let rows = stmt
            .query_map(params![account_id], |row| {
                Ok(subscription_row_to_record(row))
            })
            .map_err(AppError::from)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(AppError::from)?);
        }
        Ok(out)
    }

    /// Number of subscribers for an account.
    pub fn count_subscribers(&self, account_id: &str) -> AppResult<u32> {
        let conn = self.handle();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM account_subscriptions WHERE account_id = ?1",
                params![account_id],
                |row| row.get(0),
            )
            .map_err(AppError::from)?;
        Ok(n as u32)
    }

    /// Delete a subscription. Returns `true` if a row was removed.
    pub fn delete_subscription(
        &self,
        subscriber_id: &str,
        account_id: &str,
    ) -> AppResult<bool> {
        let conn = self.handle();
        let n = conn
            .execute(
                "DELETE FROM account_subscriptions
                 WHERE subscriber_id = ?1 AND account_id = ?2",
                params![subscriber_id, account_id],
            )
            .map_err(AppError::from)?;
        Ok(n > 0)
    }

    // ── Feed items ──────────────────────────────────────────────────

    /// Insert or replace a feed item. The service layer is
    /// responsible for calling `bump_account_sequence` *before*
    /// this so the `sequence` on the row matches the account's
    /// monotonic counter.
    pub fn put_feed_item(&self, item: &FeedItem) -> AppResult<()> {
        item.validate().map_err(AppError::from)?;
        let conn = self.handle();
        let tags_json = serde_json::to_string(&item.tags).map_err(AppError::from)?;
        let attachments_json = serde_json::to_string(&item.attachments).map_err(AppError::from)?;
        conn.execute(
            "INSERT OR REPLACE INTO feed_items
                (feed_id, account_id, sequence, title, summary, body,
                 cover_url, attachments_json, tags_json,
                 is_pinned, is_retracted, retraction_reason,
                 created_at_unix, updated_at_unix)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                item.feed_id,
                item.account_id,
                item.sequence as i64,
                item.title,
                item.summary,
                item.body,
                item.cover_url,
                attachments_json,
                tags_json,
                item.is_pinned,
                item.is_retracted,
                item.retraction_reason,
                item.created_at.timestamp(),
                item.updated_at.timestamp(),
            ],
        )
        .map_err(AppError::from)?;
        Ok(())
    }

    /// Look up a feed item by id.
    pub fn get_feed_item(
        &self,
        account_id: &str,
        feed_id: &str,
    ) -> AppResult<Option<FeedItem>> {
        let conn = self.handle();
        let row = conn
            .query_row(
                "SELECT feed_id, account_id, sequence, title, summary, body,
                        cover_url, attachments_json, tags_json,
                        is_pinned, is_retracted, retraction_reason,
                        created_at_unix, updated_at_unix
                 FROM feed_items
                 WHERE account_id = ?1 AND feed_id = ?2",
                params![account_id, feed_id],
                |row| Ok(feed_row_to_record(row)),
            )
            .optional()
            .map_err(AppError::from)?;
        match row {
            None => Ok(None),
            Some(item) => {
                item.validate().map_err(AppError::from)?;
                Ok(Some(item))
            }
        }
    }

    /// All feed items for an account, newest first, with an
    /// optional `before_sequence` cursor for pagination. The
    /// service layer also exposes the public-account-level
    /// timeline (`list_timeline`) which takes a list of
    /// subscriptions.
    pub fn list_feed_items(
        &self,
        account_id: &str,
        before_sequence: Option<u32>,
        limit: u32,
    ) -> AppResult<Vec<FeedItem>> {
        let limit = limit.clamp(1, 200);
        let conn = self.handle();
        let (sql, use_cursor): (&str, bool) = if before_sequence.is_some() {
            (
                "SELECT feed_id, account_id, sequence, title, summary, body,
                        cover_url, attachments_json, tags_json,
                        is_pinned, is_retracted, retraction_reason,
                        created_at_unix, updated_at_unix
                 FROM feed_items
                 WHERE account_id = ?1 AND sequence < ?2
                   AND is_retracted = 0
                 ORDER BY sequence DESC
                 LIMIT ?3",
                true,
            )
        } else {
            (
                "SELECT feed_id, account_id, sequence, title, summary, body,
                        cover_url, attachments_json, tags_json,
                        is_pinned, is_retracted, retraction_reason,
                        created_at_unix, updated_at_unix
                 FROM feed_items
                 WHERE account_id = ?1
                   AND is_retracted = 0
                 ORDER BY sequence DESC
                 LIMIT ?2",
                false,
            )
        };
        let mut stmt = conn.prepare(sql).map_err(AppError::from)?;
        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<FeedItem> {
            Ok(feed_row_to_record(row))
        };
        let rows = if use_cursor {
            stmt.query_map(
                params![account_id, before_sequence.unwrap() as i64, limit as i64],
                map_row,
            )
            .map_err(AppError::from)?
        } else {
            stmt.query_map(params![account_id, limit as i64], map_row)
                .map_err(AppError::from)?
        };
        let mut out = Vec::new();
        for r in rows {
            let item = r.map_err(AppError::from)?;
            item.validate().map_err(AppError::from)?;
            out.push(item);
        }
        Ok(out)
    }

    /// Mark a feed item retracted. The row is kept in storage but
    /// excluded from the public timeline (`list_feed_items`).
    pub fn retract_feed_item(
        &self,
        account_id: &str,
        feed_id: &str,
        reason: &str,
        now: DateTime<Utc>,
    ) -> AppResult<()> {
        let conn = self.handle();
        let n = conn
            .execute(
                "UPDATE feed_items
                 SET is_retracted = 1,
                     retraction_reason = ?1,
                     updated_at_unix = ?2
                 WHERE account_id = ?3 AND feed_id = ?4",
                params![reason, now.timestamp(), account_id, feed_id],
            )
            .map_err(AppError::from)?;
        if n == 0 {
            return Err(AppError::Domain(format!(
                "feed item not found: {feed_id} (account {account_id})"
            )));
        }
        Ok(())
    }

    // ── Read cursors / unread ───────────────────────────────────────

    /// Record `subscriber_id` having read up to `last_read_seq` for
    /// `account_id`. Idempotent — the cursor only ever moves
    /// forward.
    pub fn mark_read(
        &self,
        subscriber_id: &str,
        account_id: &str,
        last_read_seq: u32,
        feed_id: &str,
    ) -> AppResult<()> {
        let conn = self.handle();
        let tx = conn.unchecked_transaction().map_err(AppError::from)?;
        tx.execute(
            "INSERT INTO feed_read_cursors
                (subscriber_id, account_id, last_read_seq, last_read_at_unix)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(subscriber_id, account_id) DO UPDATE SET
                 last_read_seq    = MAX(feed_read_cursors.last_read_seq,
                                        excluded.last_read_seq),
                 last_read_at_unix = excluded.last_read_at_unix",
            params![
                subscriber_id,
                account_id,
                last_read_seq as i64,
                Utc::now().timestamp(),
            ],
        )
        .map_err(AppError::from)?;
        // Record the per-feed "X has read Y" mark for analytics.
        let _ = tx.execute(
            "INSERT OR IGNORE INTO feed_recipients
                (subscriber_id, account_id, feed_id, read_at_unix)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                subscriber_id,
                account_id,
                feed_id,
                Utc::now().timestamp()
            ],
        );
        tx.commit().map_err(AppError::from)?;
        Ok(())
    }

    /// Unread count for `(subscriber, account)`: `max(0,
    /// account.sequence - last_read_seq)`. Returns 0 if the
    /// subscriber has no cursor row yet.
    pub fn unread_count(
        &self,
        subscriber_id: &str,
        account_id: &str,
    ) -> AppResult<u32> {
        let conn = self.handle();
        let (account_seq, last_read): (Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT a.sequence, c.last_read_seq
                 FROM public_accounts a
                 LEFT JOIN feed_read_cursors c
                   ON c.subscriber_id = ?1 AND c.account_id = a.account_id
                 WHERE a.account_id = ?2",
                params![subscriber_id, account_id],
                |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .optional()
            .map_err(AppError::from)?
            .unwrap_or((None, None));
        let total = account_seq.unwrap_or(0).max(0) as u32;
        let read = last_read.unwrap_or(0).max(0) as u32;
        Ok(total.saturating_sub(read))
    }

    /// Number of distinct subscribers who have read a given feed
    /// item. Used by the audit / analytics path.
    pub fn read_recipient_count(
        &self,
        account_id: &str,
        feed_id: &str,
    ) -> AppResult<u32> {
        let conn = self.handle();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM feed_recipients
                 WHERE account_id = ?1 AND feed_id = ?2",
                params![account_id, feed_id],
                |row| row.get(0),
            )
            .map_err(AppError::from)?;
        Ok(n as u32)
    }
}

// ── Internal helpers ────────────────────────────────────────────

fn configure_connection(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    Ok(())
}

fn apply_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS public_accounts (
            account_id        TEXT PRIMARY KEY,
            owner_node_id     TEXT NOT NULL UNIQUE,
            name              TEXT NOT NULL,
            bio               TEXT NOT NULL DEFAULT '',
            avatar_hash       TEXT,
            tags_json         TEXT NOT NULL DEFAULT '[]',
            kind              TEXT NOT NULL DEFAULT 'subscription',
            verification      TEXT NOT NULL DEFAULT 'none',
            sequence          INTEGER NOT NULL DEFAULT 0,
            subscriber_count  INTEGER NOT NULL DEFAULT 0,
            created_at_unix   INTEGER NOT NULL,
            updated_at_unix   INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS public_accounts_kind_idx
            ON public_accounts(kind);
        CREATE INDEX IF NOT EXISTS public_accounts_created_idx
            ON public_accounts(created_at_unix DESC);

        CREATE TABLE IF NOT EXISTS account_subscriptions (
            subscriber_id      TEXT NOT NULL,
            account_id         TEXT NOT NULL,
            alias              TEXT NOT NULL DEFAULT '',
            notify_mode        TEXT NOT NULL DEFAULT 'normal',
            is_muted           INTEGER NOT NULL DEFAULT 0,
            is_pinned          INTEGER NOT NULL DEFAULT 0,
            subscribed_at_unix INTEGER NOT NULL,
            last_read_seq      INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (subscriber_id, account_id)
        );

        CREATE INDEX IF NOT EXISTS account_subscriptions_account_idx
            ON account_subscriptions(account_id, subscribed_at_unix DESC);

        CREATE TABLE IF NOT EXISTS feed_items (
            feed_id            TEXT NOT NULL,
            account_id         TEXT NOT NULL,
            sequence           INTEGER NOT NULL,
            title              TEXT NOT NULL,
            summary            TEXT NOT NULL DEFAULT '',
            body               TEXT NOT NULL,
            cover_url          TEXT,
            attachments_json   TEXT NOT NULL DEFAULT '[]',
            tags_json          TEXT NOT NULL DEFAULT '[]',
            is_pinned          INTEGER NOT NULL DEFAULT 0,
            is_retracted       INTEGER NOT NULL DEFAULT 0,
            retraction_reason  TEXT,
            created_at_unix    INTEGER NOT NULL,
            updated_at_unix    INTEGER NOT NULL,
            PRIMARY KEY (account_id, feed_id)
        );

        CREATE UNIQUE INDEX IF NOT EXISTS feed_items_seq_idx
            ON feed_items(account_id, sequence);
        CREATE INDEX IF NOT EXISTS feed_items_created_idx
            ON feed_items(account_id, created_at_unix DESC);

        CREATE TABLE IF NOT EXISTS feed_read_cursors (
            subscriber_id      TEXT NOT NULL,
            account_id         TEXT NOT NULL,
            last_read_seq      INTEGER NOT NULL DEFAULT 0,
            last_read_at_unix  INTEGER NOT NULL,
            PRIMARY KEY (subscriber_id, account_id)
        );

        CREATE TABLE IF NOT EXISTS feed_recipients (
            subscriber_id      TEXT NOT NULL,
            account_id         TEXT NOT NULL,
            feed_id            TEXT NOT NULL,
            read_at_unix       INTEGER NOT NULL,
            PRIMARY KEY (subscriber_id, account_id, feed_id)
        );

        CREATE TABLE IF NOT EXISTS schema_version (
            id     INTEGER PRIMARY KEY,
            version INTEGER NOT NULL
        );
        INSERT OR IGNORE INTO schema_version (id, version)
            VALUES (1, ?1);
        "#,
    )?;
    Ok(())
}

#[allow(clippy::type_complexity)]
fn account_row_to_record(
    row: &rusqlite::Row<'_>,
    tags_json: String,
) -> (PublicAccount, String) {
    let kind_str: String = row.get(6).unwrap_or_else(|_| "subscription".into());
    let verification_str: String = row.get(7).unwrap_or_else(|_| "none".into());
    let sequence: i64 = row.get(8).unwrap_or(0);
    let subscriber_count: i64 = row.get(9).unwrap_or(0);
    let created_at_unix: i64 = row.get(10).unwrap_or(0);
    let updated_at_unix: i64 = row.get(11).unwrap_or(0);
    let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
    let account = PublicAccount {
        account_id: row.get(0).unwrap_or_default(),
        owner_node_id: row.get(1).unwrap_or_default(),
        name: row.get(2).unwrap_or_default(),
        bio: row.get(3).unwrap_or_default(),
        avatar_hash: row.get(4).unwrap_or(None),
        tags,
        kind: AccountKind::parse(&kind_str),
        verification: VerificationLevel::parse(&verification_str),
        sequence: sequence.max(0) as u32,
        subscriber_count: subscriber_count.max(0) as u32,
        created_at: ts_from_unix(created_at_unix),
        updated_at: ts_from_unix(updated_at_unix),
    };
    (account, tags_json)
}

fn subscription_row_to_record(row: &rusqlite::Row<'_>) -> Subscription {
    let subscribed_at_unix: i64 = row.get(6).unwrap_or(0);
    let last_read_seq: i64 = row.get(7).unwrap_or(0);
    Subscription {
        subscriber_id: row.get(0).unwrap_or_default(),
        account_id: row.get(1).unwrap_or_default(),
        alias: row.get(2).unwrap_or_default(),
        notify_mode: row.get(3).unwrap_or_default(),
        is_muted: row.get::<_, i64>(4).unwrap_or(0) != 0,
        is_pinned: row.get::<_, i64>(5).unwrap_or(0) != 0,
        subscribed_at: ts_from_unix(subscribed_at_unix),
        last_read_seq: last_read_seq.max(0) as u32,
    }
}

fn feed_row_to_record(row: &rusqlite::Row<'_>) -> FeedItem {
    let sequence: i64 = row.get(2).unwrap_or(0);
    let attachments_json: String = row.get(7).unwrap_or_else(|_| "[]".into());
    let tags_json: String = row.get(8).unwrap_or_else(|_| "[]".into());
    let is_pinned: i64 = row.get(9).unwrap_or(0);
    let is_retracted: i64 = row.get(10).unwrap_or(0);
    let retraction_reason: Option<String> = row.get(11).unwrap_or(None);
    let created_at_unix: i64 = row.get(12).unwrap_or(0);
    let updated_at_unix: i64 = row.get(13).unwrap_or(0);
    let attachments: Vec<FeedAttachment> = serde_json::from_str(&attachments_json).unwrap_or_default();
    let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
    FeedItem {
        feed_id: row.get(0).unwrap_or_default(),
        account_id: row.get(1).unwrap_or_default(),
        sequence: sequence.max(0) as u32,
        title: row.get(3).unwrap_or_default(),
        summary: row.get(4).unwrap_or_default(),
        body: row.get(5).unwrap_or_default(),
        cover_url: row.get(6).unwrap_or(None),
        attachments,
        tags,
        is_pinned: is_pinned != 0,
        is_retracted: is_retracted != 0,
        retraction_reason,
        created_at: ts_from_unix(created_at_unix),
        updated_at: ts_from_unix(updated_at_unix),
    }
}

fn ts_from_unix(unix: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(unix, 0).unwrap_or_else(Utc::now)
}

// ── Suppress unused-import warning when this file is used by
// binaries that don't pull in the variant types via public APIs.
#[allow(dead_code)]
const _ACCOUNT_ID_PREFIX: &str = ACCOUNT_ID_PREFIX;
#[allow(dead_code)]
const _FEED_ID_PREFIX: &str = FEED_ID_PREFIX;

// Suppress unused-import on A3chatError when downstream consumers
// turn it into an `AppError` via the `?` operator.
#[allow(dead_code)]
fn _ae_marker(_: A3chatError) {}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn open_temp() -> (tempfile::TempDir, ChannelStorage) {
        let dir = tempdir().expect("tempdir");
        let cfg = ChannelStorageConfig {
            storage_dir: dir.path().to_path_buf(),
            filename: "channel-test.db".into(),
        };
        let storage = ChannelStorage::open(cfg).expect("open");
        (dir, storage)
    }

    fn sample_account(owner: &str) -> PublicAccount {
        let now = Utc::now();
        PublicAccount {
            account_id: format!("{ACCOUNT_ID_PREFIX}{}", hex::encode(&[0xab; 12])),
            owner_node_id: owner.into(),
            name: "Tech News".into(),
            bio: "roundup of a3chat releases".into(),
            avatar_hash: Some("0123abcd".into()),
            tags: vec!["tech".into(), "news".into()],
            kind: AccountKind::Service,
            verification: VerificationLevel::OwnerVerified,
            sequence: 0,
            subscriber_count: 0,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn put_then_get_account_round_trip() {
        let (_dir, storage) = open_temp();
        let a = sample_account("user:alice");
        storage.put_account(&a).expect("put_account");
        let back = storage
            .get_account(&a.account_id)
            .expect("get_account")
            .expect("present");
        assert_eq!(back.account_id, a.account_id);
        assert_eq!(back.owner_node_id, a.owner_node_id);
        assert_eq!(back.kind, AccountKind::Service);
        assert_eq!(back.verification, VerificationLevel::OwnerVerified);
        assert_eq!(back.tags, vec!["tech", "news"]);
    }

    #[test]
    fn get_account_by_owner_returns_unique_row() {
        let (_dir, storage) = open_temp();
        let a = sample_account("user:alice");
        storage.put_account(&a).expect("put_account");
        let back = storage
            .get_account_by_owner("user:alice")
            .expect("ok")
            .expect("present");
        assert_eq!(back.account_id, a.account_id);
    }

    #[test]
    fn second_register_for_same_owner_is_rejected_by_unique_index() {
        let (_dir, storage) = open_temp();
        let a = sample_account("user:alice");
        storage.put_account(&a).expect("put_account");
        let mut b = sample_account("user:alice");
        // Different account_id, same owner — should fail at the
        // unique index. The service layer turns this into a
        // domain error.
        b.account_id = format!("{ACCOUNT_ID_PREFIX}{}", hex::encode(&[0xcd; 12]));
        b.name = "Other".into();
        let r = storage.put_account(&b);
        assert!(r.is_err(), "expected unique-index rejection, got {r:?}");
    }

    #[test]
    fn bump_account_sequence_is_monotonic() {
        let (_dir, storage) = open_temp();
        let a = sample_account("user:alice");
        storage.put_account(&a).expect("put_account");
        let s1 = storage
            .bump_account_sequence(&a.account_id, Utc::now())
            .expect("bump1");
        let s2 = storage
            .bump_account_sequence(&a.account_id, Utc::now())
            .expect("bump2");
        let s3 = storage
            .bump_account_sequence(&a.account_id, Utc::now())
            .expect("bump3");
        assert_eq!((s1, s2, s3), (1, 2, 3));
    }

    #[test]
    fn subscribe_and_unsubscribe_keeps_subscriber_count_in_sync() {
        let (_dir, storage) = open_temp();
        let a = sample_account("user:alice");
        storage.put_account(&a).expect("put_account");
        let sub_a = Subscription {
            subscriber_id: "user:bob".into(),
            account_id: a.account_id.clone(),
            alias: "work".into(),
            notify_mode: "normal".into(),
            is_muted: false,
            is_pinned: false,
            subscribed_at: Utc::now(),
            last_read_seq: 0,
        };
        storage.put_subscription(&sub_a).expect("subscribe");
        let count = storage.recompute_subscriber_count(&a.account_id).expect("count");
        assert_eq!(count, 1);

        storage
            .delete_subscription("user:bob", &a.account_id)
            .expect("delete");
        let count = storage.recompute_subscriber_count(&a.account_id).expect("count");
        assert_eq!(count, 0);
    }

    #[test]
    fn re_subscribe_is_idempotent_and_preserves_last_read_seq() {
        let (_dir, storage) = open_temp();
        let a = sample_account("user:alice");
        storage.put_account(&a).expect("put_account");
        let mut sub = Subscription {
            subscriber_id: "user:bob".into(),
            account_id: a.account_id.clone(),
            alias: "".into(),
            notify_mode: "normal".into(),
            is_muted: false,
            is_pinned: false,
            subscribed_at: Utc::now(),
            last_read_seq: 0,
        };
        storage.put_subscription(&sub).expect("subscribe");
        sub.last_read_seq = 5;
        sub.is_pinned = true;
        storage.put_subscription(&sub).expect("re-subscribe");
        let back = storage
            .get_subscription("user:bob", &a.account_id)
            .expect("get")
            .expect("present");
        assert!(back.is_pinned);
        assert_eq!(back.last_read_seq, 5);

        // Re-subscribe with a smaller seq must NOT roll the cursor
        // backwards.
        sub.last_read_seq = 1;
        storage.put_subscription(&sub).expect("re-subscribe lower");
        let back = storage
            .get_subscription("user:bob", &a.account_id)
            .expect("get")
            .expect("present");
        assert_eq!(back.last_read_seq, 5);
    }

    #[test]
    fn feed_item_publish_and_list_skips_retracted() {
        let (_dir, storage) = open_temp();
        let a = sample_account("user:alice");
        storage.put_account(&a).expect("put_account");
        let now = Utc::now();
        let items: Vec<FeedItem> = (0..3)
            .map(|i| FeedItem {
                feed_id: format!(
                    "{FEED_ID_PREFIX}{}",
                    hex::encode(format!("feed-{i}").as_bytes())
                ),
                account_id: a.account_id.clone(),
                sequence: i as u32,
                title: format!("Item {i}"),
                summary: "summary".into(),
                body: "body".into(),
                cover_url: None,
                attachments: vec![],
                tags: vec![],
                is_pinned: false,
                is_retracted: false,
                retraction_reason: None,
                created_at: now,
                updated_at: now,
            })
            .collect();
        for it in &items {
            storage.put_feed_item(it).expect("put_feed");
        }
        let listed = storage
            .list_feed_items(&a.account_id, None, 50)
            .expect("list");
        assert_eq!(listed.len(), 3);
        // Newest first.
        assert_eq!(listed[0].sequence, 2);

        storage
            .retract_feed_item(&a.account_id, &items[1].feed_id, "duplicate", Utc::now())
            .expect("retract");
        let listed = storage
            .list_feed_items(&a.account_id, None, 50)
            .expect("list");
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().all(|i| i.sequence != 1));
    }

    #[test]
    fn mark_read_advances_cursor_and_counts_unread() {
        let (_dir, storage) = open_temp();
        let a = sample_account("user:alice");
        storage.put_account(&a).expect("put_account");
        let now = Utc::now();
        let item = FeedItem {
            feed_id: format!("{FEED_ID_PREFIX}abc"),
            account_id: a.account_id.clone(),
            sequence: 1,
            title: "ok".into(),
            summary: "ok".into(),
            body: "ok".into(),
            cover_url: None,
            attachments: vec![],
            tags: vec![],
            is_pinned: false,
            is_retracted: false,
            retraction_reason: None,
            created_at: now,
            updated_at: now,
        };
        storage.put_feed_item(&item).expect("put_feed");
        // Account has 7 publishes total — the latest is the one we
        // just stored, with sequence = 7.
        for _ in 0..7 {
            storage
                .bump_account_sequence(&a.account_id, now)
                .expect("bump");
        }

        // Initial unread: 7
        let unread = storage
            .unread_count("user:bob", &a.account_id)
            .expect("unread");
        assert_eq!(unread, 7);

        storage
            .mark_read("user:bob", &a.account_id, 3, &item.feed_id)
            .expect("mark");
        let unread = storage
            .unread_count("user:bob", &a.account_id)
            .expect("unread");
        assert_eq!(unread, 4);

        // Marking with a smaller cursor must NOT regress.
        storage
            .mark_read("user:bob", &a.account_id, 1, &item.feed_id)
            .expect("mark lower");
        let unread = storage
            .unread_count("user:bob", &a.account_id)
            .expect("unread");
        assert_eq!(unread, 4);

        let read_recipients = storage
            .read_recipient_count(&a.account_id, &item.feed_id)
            .expect("read_recipients");
        assert_eq!(read_recipients, 1);
    }

    #[test]
    fn delete_account_cascades_to_subs_and_feed() {
        let (_dir, storage) = open_temp();
        let a = sample_account("user:alice");
        storage.put_account(&a).expect("put_account");
        let sub = Subscription {
            subscriber_id: "user:bob".into(),
            account_id: a.account_id.clone(),
            alias: "".into(),
            notify_mode: "normal".into(),
            is_muted: false,
            is_pinned: false,
            subscribed_at: Utc::now(),
            last_read_seq: 0,
        };
        storage.put_subscription(&sub).expect("subscribe");
        let now = Utc::now();
        let item = FeedItem {
            feed_id: format!("{FEED_ID_PREFIX}abc"),
            account_id: a.account_id.clone(),
            sequence: 1,
            title: "ok".into(),
            summary: "ok".into(),
            body: "ok".into(),
            cover_url: None,
            attachments: vec![],
            tags: vec![],
            is_pinned: false,
            is_retracted: false,
            retraction_reason: None,
            created_at: now,
            updated_at: now,
        };
        storage.put_feed_item(&item).expect("put_feed");

        let removed = storage.delete_account(&a.account_id).expect("delete");
        assert!(removed);
        assert!(storage.get_account(&a.account_id).expect("get").is_none());
        assert!(storage.get_subscription("user:bob", &a.account_id).expect("get").is_none());
        assert!(storage
            .get_feed_item(&a.account_id, &item.feed_id)
            .expect("get")
            .is_none());
    }

    #[test]
    fn search_finds_account_by_name() {
        let (_dir, storage) = open_temp();
        let a = sample_account("user:alice");
        storage.put_account(&a).expect("put_account");
        let hits = storage.search_accounts("tech", 50).expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].account_id, a.account_id);
    }
}