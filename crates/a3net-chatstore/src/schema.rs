//! Canonical SQL schema for the chat store.
//!
//! Two families of tables live side-by-side in the same SQLite file:
//!
//! 1. **Per-user chat history** (originally
//!    `Exodus/src-backup/src-tauri/src/microservice/chat_storage.rs`):
//!   - `friends(user_id, friend_id)` — per-user friend list.
//!   - `direct_messages(user_id, chat_id, ...)` — 1-to-1 message log,
//!     partitioned by `user_id` so multiple local users coexist.
//!   - `group_messages(user_id, group_id, ...)` — group message log
//!     replicated per recipient.
//!   - `sequences(user_id, target_id, sequence_type, last_sequence)` —
//!     last seen sequence per (user, target, kind).
//!   - `message_receipts(user_id, message_id, ...)` — delivery receipts.
//!
//! 2. **Hub-server canonical store** (originally
//!    `Exodus/src-backup/exodus-hub-server/src/manager.rs`):
//!   - `users`, `conversations`, `group_members`, `messages`,
//!     `sender_sequences`, `user_sequences`, `pending_messages`,
//!     `hub_message_receipts`.
//!
//! Both share the same WAL-mode connection so transactions can be
//! cross-cutting.
//!
//! # Schema versioning
//!
//! The current [`SCHEMA_VERSION`] is stamped into the
//! `schema_version` table on first apply. On subsequent opens,
//! [`apply_schema`] checks the stored version against the build's
//! version and returns [`ChatStoreError::SchemaVersion`]
//! if the file is **newer** than what we understand. The migration
//! ladder lives in `migrate_to`.

use rusqlite::Connection;

use crate::error::{ChatStoreError, Result};

/// Current schema version. Bump on every schema change and add a
/// migration step in [`migrate_to`].
pub const SCHEMA_VERSION: u32 = 4;

/// All `CREATE TABLE IF NOT EXISTS` / `CREATE INDEX IF NOT EXISTS`
/// statements bundled together so the bootstrap path is a single
/// function call. Idempotent for empty databases.
pub(super) const CREATE_STATEMENTS: &[&str] = &[
    // ---- per-user chat history -------------------------------------------
    "CREATE TABLE IF NOT EXISTS friends (
        user_id     TEXT NOT NULL,
        friend_id   TEXT NOT NULL,
        name        TEXT NOT NULL,
        avatar_url  TEXT,
        status      TEXT,
        last_seen   INTEGER,
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL,
        PRIMARY KEY (user_id, friend_id)
    )",
    "CREATE TABLE IF NOT EXISTS direct_messages (
        message_id     TEXT NOT NULL,
        user_id        TEXT NOT NULL,
        chat_id        TEXT NOT NULL,
        sender_id      TEXT NOT NULL,
        receiver_id    TEXT NOT NULL,
        content        TEXT NOT NULL,
        message_type   TEXT NOT NULL,
        attachments    TEXT,
        reply_to       TEXT,
        sequence       INTEGER NOT NULL,
        timestamp      INTEGER NOT NULL,
        integrity_hash TEXT,
        is_edited      INTEGER NOT NULL DEFAULT 0,
        edited_at      INTEGER,
        direction      TEXT NOT NULL,
        PRIMARY KEY (message_id, user_id)
    )",
    "CREATE TABLE IF NOT EXISTS group_messages (
        message_id     TEXT NOT NULL,
        user_id        TEXT NOT NULL,
        group_id       TEXT NOT NULL,
        sender_id      TEXT NOT NULL,
        sender_name    TEXT NOT NULL,
        content        TEXT NOT NULL,
        message_type   TEXT NOT NULL,
        attachments    TEXT,
        reply_to       TEXT,
        mentions       TEXT,
        sequence       INTEGER NOT NULL,
        timestamp      INTEGER NOT NULL,
        integrity_hash TEXT,
        is_edited      INTEGER NOT NULL DEFAULT 0,
        edited_at      INTEGER,
        PRIMARY KEY (message_id, user_id)
    )",
    "CREATE TABLE IF NOT EXISTS sequences (
        user_id        TEXT NOT NULL,
        target_id      TEXT NOT NULL,
        sequence_type  TEXT NOT NULL,
        last_sequence  INTEGER NOT NULL DEFAULT 0,
        updated_at     INTEGER NOT NULL,
        PRIMARY KEY (user_id, target_id, sequence_type)
    )",
    "CREATE TABLE IF NOT EXISTS message_receipts (
        receipt_id   TEXT PRIMARY KEY,
        message_id   TEXT NOT NULL,
        user_id      TEXT NOT NULL,
        receiver_id  TEXT NOT NULL,
        sequence     INTEGER NOT NULL,
        received_at  INTEGER NOT NULL
    )",
    // ---- hub-server canonical store --------------------------------------
    "CREATE TABLE IF NOT EXISTS users (
        id            TEXT PRIMARY KEY,
        username      TEXT NOT NULL UNIQUE,
        display_name  TEXT NOT NULL,
        created_at    TEXT NOT NULL,
        last_seen     TEXT
    )",
    "CREATE TABLE IF NOT EXISTS conversations (
        id             TEXT PRIMARY KEY,
        chat_type      TEXT NOT NULL,
        title          TEXT NOT NULL,
        description    TEXT,
        announcement   TEXT,
        is_private     INTEGER NOT NULL DEFAULT 1,
        is_dissolved   INTEGER NOT NULL DEFAULT 0,
        created_at     TEXT NOT NULL,
        updated_at     TEXT NOT NULL,
        message_count  INTEGER NOT NULL DEFAULT 0,
        last_sequence  INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS group_members (
        id              TEXT PRIMARY KEY,
        conversation_id TEXT NOT NULL,
        user_id         TEXT NOT NULL,
        joined_at       TEXT NOT NULL,
        role            TEXT NOT NULL DEFAULT 'member',
        FOREIGN KEY (conversation_id) REFERENCES conversations(id) ON DELETE CASCADE,
        FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
        UNIQUE(conversation_id, user_id)
    )",
    "CREATE TABLE IF NOT EXISTS messages (
        id              TEXT PRIMARY KEY,
        conversation_id TEXT NOT NULL,
        sender_id       TEXT NOT NULL,
        receiver_id     TEXT,
        content         TEXT NOT NULL,
        timestamp       TEXT NOT NULL,
        sequence        INTEGER,
        reply_to        TEXT,
        integrity_hash  TEXT,
        is_edited       INTEGER NOT NULL DEFAULT 0,
        edited_at       TEXT,
        FOREIGN KEY (conversation_id) REFERENCES conversations(id) ON DELETE CASCADE,
        FOREIGN KEY (sender_id) REFERENCES users(id),
        FOREIGN KEY (reply_to) REFERENCES messages(id)
    )",
    "CREATE TABLE IF NOT EXISTS sender_sequences (
        id            TEXT PRIMARY KEY,
        sender_id     TEXT NOT NULL,
        last_sequence INTEGER NOT NULL DEFAULT 0,
        updated_at    TEXT NOT NULL,
        FOREIGN KEY (sender_id) REFERENCES users(id) ON DELETE CASCADE,
        UNIQUE(sender_id)
    )",
    "CREATE TABLE IF NOT EXISTS user_sequences (
        id            TEXT PRIMARY KEY,
        user_id       TEXT NOT NULL,
        sender_id     TEXT NOT NULL,
        last_sequence INTEGER NOT NULL DEFAULT 0,
        updated_at    TEXT NOT NULL,
        FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
        FOREIGN KEY (sender_id) REFERENCES users(id) ON DELETE CASCADE,
        UNIQUE(user_id, sender_id)
    )",
    "CREATE TABLE IF NOT EXISTS pending_messages (
        id              TEXT PRIMARY KEY,
        message_id      TEXT NOT NULL,
        receiver_id     TEXT NOT NULL,
        conversation_id TEXT NOT NULL,
        created_at      TEXT NOT NULL,
        FOREIGN KEY (message_id)      REFERENCES messages(id)      ON DELETE CASCADE,
        FOREIGN KEY (receiver_id)     REFERENCES users(id)         ON DELETE CASCADE,
        FOREIGN KEY (conversation_id) REFERENCES conversations(id) ON DELETE CASCADE
    )",
    // ── Chat trust (peer-feedback) ─────────────────────────────────
    // The `chat_trust` table is owned by `trust::ChatTrustStore` but
    // its schema is applied here so the table exists in every
    // chatstore SQLite file. Level is constrained to [-3, +3] (see
    // `a3net_reputation::TrustLevel`) at the application layer; the
    // table itself accepts any i8 so older / corrupted rows remain
    // readable.
    "CREATE TABLE IF NOT EXISTS chat_trust (
        owner_user_id    TEXT NOT NULL,
        target_user_id   TEXT NOT NULL,
        level            INTEGER NOT NULL,
        last_event_unix  INTEGER NOT NULL,
        event_count      INTEGER NOT NULL DEFAULT 0,
        notes            TEXT,
        PRIMARY KEY (owner_user_id, target_user_id)
    )",
    "CREATE INDEX IF NOT EXISTS idx_chat_trust_owner
        ON chat_trust(owner_user_id, level DESC)",
    // The per-user `message_receipts` table already exists above; to
    // keep the two layers cleanly separated we use a different table
    // name here and an FK to the canonical `messages` table.
    "CREATE TABLE IF NOT EXISTS hub_message_receipts (
        id           TEXT PRIMARY KEY,
        message_id   TEXT NOT NULL,
        receiver_id  TEXT NOT NULL,
        sequence     INTEGER NOT NULL,
        received_at  TEXT NOT NULL,
        FOREIGN KEY (message_id)  REFERENCES messages(id)  ON DELETE CASCADE,
        FOREIGN KEY (receiver_id) REFERENCES users(id)     ON DELETE CASCADE
    )",
];

/// All `CREATE INDEX IF NOT EXISTS` statements.
pub(super) const INDEX_STATEMENTS: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS idx_friends_user_id           ON friends(user_id)",
    "CREATE INDEX IF NOT EXISTS idx_direct_messages_user_id   ON direct_messages(user_id)",
    "CREATE INDEX IF NOT EXISTS idx_direct_messages_chat_id   ON direct_messages(chat_id)",
    "CREATE INDEX IF NOT EXISTS idx_direct_messages_timestamp ON direct_messages(timestamp)",
    "CREATE INDEX IF NOT EXISTS idx_direct_messages_sequence  ON direct_messages(sequence)",
    "CREATE INDEX IF NOT EXISTS idx_group_messages_user_id    ON group_messages(user_id)",
    "CREATE INDEX IF NOT EXISTS idx_group_messages_group_id   ON group_messages(group_id)",
    "CREATE INDEX IF NOT EXISTS idx_group_messages_timestamp  ON group_messages(timestamp)",
    "CREATE INDEX IF NOT EXISTS idx_group_messages_sequence   ON group_messages(sequence)",
    "CREATE INDEX IF NOT EXISTS idx_message_receipts_message_id ON message_receipts(message_id)",
    "CREATE INDEX IF NOT EXISTS idx_message_receipts_user_id    ON message_receipts(user_id)",
    "CREATE INDEX IF NOT EXISTS idx_sequences_user_id         ON sequences(user_id)",
    "CREATE INDEX IF NOT EXISTS idx_users_username            ON users(username)",
    "CREATE INDEX IF NOT EXISTS idx_pending_messages_receiver ON pending_messages(receiver_id)",
    "CREATE INDEX IF NOT EXISTS idx_messages_conversation     ON messages(conversation_id)",
    "CREATE INDEX IF NOT EXISTS idx_messages_sender           ON messages(sender_id)",
    "CREATE INDEX IF NOT EXISTS idx_messages_conv_sequence    ON messages(conversation_id, sequence)",
    "CREATE INDEX IF NOT EXISTS idx_group_members_conv        ON group_members(conversation_id)",
    "CREATE INDEX IF NOT EXISTS idx_group_members_user        ON group_members(user_id)",
    "CREATE INDEX IF NOT EXISTS idx_user_sequences_user       ON user_sequences(user_id)",
    "CREATE INDEX IF NOT EXISTS idx_hub_receipts_message      ON hub_message_receipts(message_id)",
    // ---- link bookmarks (per-user URL archive) -----------------------------
    "CREATE TABLE IF NOT EXISTS link_bookmarks (
        bookmark_id      TEXT NOT NULL,
        owner_id         TEXT NOT NULL,
        url              TEXT NOT NULL,
        title            TEXT NOT NULL,
        description      TEXT,
        favicon_hash     TEXT,
        folder           TEXT NOT NULL DEFAULT '/',
        tags_json        TEXT NOT NULL DEFAULT '[]',
        is_pinned        INTEGER NOT NULL DEFAULT 0,
        is_archived      INTEGER NOT NULL DEFAULT 0,
        snapshot_text    TEXT,
        source           TEXT NOT NULL DEFAULT 'manual',
        created_at_unix  INTEGER NOT NULL,
        updated_at_unix  INTEGER NOT NULL,
        last_visited_unix INTEGER,
        visit_count      INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (owner_id, bookmark_id)
     )",
    "CREATE INDEX IF NOT EXISTS idx_link_bookmarks_owner        ON link_bookmarks(owner_id)",
    "CREATE INDEX IF NOT EXISTS idx_link_bookmarks_owner_url    ON link_bookmarks(owner_id, url)",
    "CREATE INDEX IF NOT EXISTS idx_link_bookmarks_owner_folder ON link_bookmarks(owner_id, folder)",
    "CREATE INDEX IF NOT EXISTS idx_link_bookmarks_owner_pinned ON link_bookmarks(owner_id, is_pinned)",
    "CREATE INDEX IF NOT EXISTS idx_link_bookmarks_owner_archived ON link_bookmarks(owner_id, is_archived)",
];

/// The `schema_version` table is created before everything else so
/// the migration machinery can detect the on-disk version
/// independently of the rest of the schema.
const CREATE_VERSION_TABLE: &str = "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL)";

/// Enable WAL mode and FK enforcement. Call this once per
/// connection. The PRAGMA statements don't return rows in general
/// (the journal-mode one does), so we use `execute_batch` for the
/// idempotent ones and only `query_row` for `journal_mode` so we can
/// observe the result.
pub(super) fn configure_connection(conn: &Connection) -> rusqlite::Result<()> {
    // `journal_mode` returns one row with the resulting mode name;
    // we don't care about the value, we just need the side effect.
    let _mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON")?;
    Ok(())
}

/// Look up the on-disk schema version (0 if the table is absent or
/// empty).
pub(super) fn current_version(conn: &Connection) -> rusqlite::Result<u32> {
    conn.execute_batch(CREATE_VERSION_TABLE)?;
    let v: Option<i64> = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get::<_, Option<i64>>(0)
        })
        .ok()
        .flatten();
    // `MAX(version)` is `NULL` only when the table is empty, which
    // we handled with `.unwrap_or(0)`. The inner value is non-negative
    // because `version` is an `INTEGER` and SQLite always coerces to
    // a signed representation; we cast through `i64` first to avoid
    // a `as u32` truncation surprise.
    Ok(v.unwrap_or(0).max(0) as u32)
}

/// Run all `migrate_to(v)` steps for `from..SCHEMA_VERSION`. Each
/// migration runs in its own transaction; if any step fails the
/// whole chain is aborted. New migrations should be appended to
/// [`migrate_to`].
fn apply_migrations(conn: &mut Connection, from: u32) -> rusqlite::Result<()> {
    // We need `&mut Connection` so each migration can open its own
    // transaction; `execute_batch` would not let us open a tx.
    for v in (from + 1)..=SCHEMA_VERSION {
        let tx = conn.transaction()?;
        migrate_to(&tx, v)?;
        tx.commit()?;
    }
    Ok(())
}

/// Migration runner. **Never** edit a migration after it has been
/// shipped — append a new step instead. A migration may freely
/// modify / drop / add columns and rows because no production data
/// yet references the new layout.
fn migrate_to(conn: &rusqlite::Transaction<'_>, target: u32) -> rusqlite::Result<()> {
    match target {
        1 => {
            // Initial schema — created via CREATE_STATEMENTS.
        }
        2 => {
            // Add edit-tracking columns to the hub-canonical
            // `messages` table. Both are nullable / have safe
            // defaults so existing rows remain valid.
            //
            // We use a `try_add_column` helper because SQLite has
            // no `ADD COLUMN IF NOT EXISTS` and the column may
            // already exist on freshly-created databases (the
            // CREATE_STATEMENTS at the top of [`apply_schema`]
            // includes them). The migration runs *after* the
            // CREATE statements so we expect the columns to already
            // exist; we simply swallow the "duplicate column" error.
            try_add_column(conn, "messages", "is_edited", "INTEGER NOT NULL DEFAULT 0")?;
            try_add_column(conn, "messages", "edited_at", "TEXT")?;
        }
        3 => {
            // Add the `chat_trust` table (peer-feedback bridge to
            // `a3net-reputation`). The CREATE statement in
            // CREATE_STATEMENTS already creates it on fresh DBs;
            // this migration is a no-op for fresh DBs and a
            // back-fill for upgrades from v2. We re-run the same
            // CREATE here inside the migration transaction so it
            // is safe to upgrade in place.
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS chat_trust (
                    owner_user_id    TEXT NOT NULL,
                    target_user_id   TEXT NOT NULL,
                    level            INTEGER NOT NULL,
                    last_event_unix  INTEGER NOT NULL,
                    event_count      INTEGER NOT NULL DEFAULT 0,
                    notes            TEXT,
                    PRIMARY KEY (owner_user_id, target_user_id)
                 );
                 CREATE INDEX IF NOT EXISTS idx_chat_trust_owner
                     ON chat_trust(owner_user_id, level DESC);",
            )?;
        }
        4 => {
            // Add group-metadata columns to `conversations`.
            // Each try_add_column call is idempotent: it silently succeeds
            // if the column already exists (safe for fresh v4 DBs and
            // for re-runs of the migration on already-upgraded DBs).
            try_add_column(conn, "conversations", "description", "TEXT")?;
            try_add_column(conn, "conversations", "announcement", "TEXT")?;
            try_add_column(conn, "conversations", "is_private", "INTEGER NOT NULL DEFAULT 1")?;
            try_add_column(conn, "conversations", "is_dissolved", "INTEGER NOT NULL DEFAULT 0")?;
        }
        _ => {
            // Unknown future version. Should be unreachable because
            // [`apply_schema`] guards against it via
            // [`ChatStoreError::SchemaVersion`]. If we ever get
            // here, refuse to make any change rather than silently
            // committing half a migration.
            return Err(rusqlite::Error::InvalidQuery);
        }
    }
    conn.execute(
        "INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (?1, ?2)",
        rusqlite::params![target, chrono::Utc::now().timestamp()],
    )?;
    Ok(())
}

/// Best-effort `ALTER TABLE ... ADD COLUMN`. SQLite raises
/// `duplicate column name` when the column already exists; we
/// swallow that one specific error so a migration is idempotent
/// across freshly-created and previously-upgraded databases.
fn try_add_column(
    conn: &rusqlite::Transaction<'_>,
    table: &str,
    column: &str,
    decl: &str,
) -> rusqlite::Result<()> {
    let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {decl}");
    match conn.execute(&sql, []) {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(err, msg))
            if err.code == rusqlite::ErrorCode::Unknown
                && msg
                    .as_deref()
                    .is_some_and(|m| m.contains("duplicate column")) =>
        {
            // Column already present — treat as success.
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Apply the schema (tables + indexes). Idempotent.
///
/// Returns [`ChatStoreError::SchemaVersion`] when the on-disk
/// version is **newer** than what this build of `a3net-chatstore`
/// understands — the caller should refuse to operate on the file
/// rather than silently downgrading.
pub(super) fn apply_schema(conn: &mut Connection) -> Result<()> {
    // Always make sure the version table exists so we can detect
    // pre-versioning databases (they'll show version 0).
    conn.execute_batch(CREATE_VERSION_TABLE)
        .map_err(ChatStoreError::Sqlite)?;
    let stored = current_version(conn).map_err(ChatStoreError::Sqlite)?;

    if stored > SCHEMA_VERSION {
        return Err(ChatStoreError::SchemaVersion {
            stored,
            supported: SCHEMA_VERSION,
        });
    }

    for stmt in CREATE_STATEMENTS {
        conn.execute_batch(stmt).map_err(ChatStoreError::Sqlite)?;
    }
    for stmt in INDEX_STATEMENTS {
        conn.execute_batch(stmt).map_err(ChatStoreError::Sqlite)?;
    }

    if stored < SCHEMA_VERSION {
        apply_migrations(conn, stored).map_err(ChatStoreError::Sqlite)?;
    }
    Ok(())
}
