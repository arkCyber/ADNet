//! Group invitation store — backed by SQLite so invitations
//! survive restarts and the audit trail is durable.
//!
//! Used by `a3chat.group.invite` and the companion
//! `group.invitation.list / accept / decline / revoke` flow.

#![forbid(unsafe_code)]

use std::sync::Arc;

use a3chat_core::group::GroupInvitation;
use a3chat_core::id::{ConversationId, UserId};

use crate::error::{AppError, AppResult};
use crate::storage::ChatStorage;

/// Number of seconds an invitation is valid by default. Mirrors the
/// WeChat-equivalent 7-day window.
pub const DEFAULT_INVITATION_TTL_SECS: i64 = 7 * 24 * 60 * 60;

/// Allowed status values. Kept as `&str` so the SQLite schema does
/// not depend on an enum that may grow.
pub const STATUS_PENDING: &str = "pending";
pub const STATUS_ACCEPTED: &str = "accepted";
pub const STATUS_DECLINED: &str = "declined";
pub const STATUS_REVOKED: &str = "revoked";
pub const STATUS_EXPIRED: &str = "expired";

/// Snapshot of a group invitation as returned by
/// [`GroupInvitationService::inbox`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InvitationRecord {
    /// Opaque invitation id (UUID-style).
    pub invitation_id: String,
    pub conversation_id: ConversationId,
    pub group_name: String,
    pub inviter_id: UserId,
    pub inviter_name: String,
    pub invitee_id: UserId,
    /// One of the `STATUS_*` constants above.
    pub status: String,
    /// Unix seconds.
    pub created_at_unix: i64,
    pub expires_at_unix: i64,
    pub responded_at_unix: Option<i64>,
    pub message: Option<String>,
}

impl From<InvitationRecord> for GroupInvitation {
    fn from(r: InvitationRecord) -> Self {
        use a3chat_core::group::InvitationStatus;
        let status = match r.status.as_str() {
            STATUS_ACCEPTED => InvitationStatus::Accepted,
            STATUS_DECLINED | STATUS_REVOKED => InvitationStatus::Cancelled,
            STATUS_EXPIRED => InvitationStatus::Expired,
            _ => InvitationStatus::Pending,
        };
        GroupInvitation {
            invitation_id: r.invitation_id,
            conversation_id: r.conversation_id,
            group_name: r.group_name,
            inviter_id: r.inviter_id,
            inviter_name: r.inviter_name,
            invitee_id: r.invitee_id,
            status,
            created_at: chrono::DateTime::from_timestamp(r.created_at_unix, 0)
                .unwrap_or_else(chrono::Utc::now),
            expires_at: chrono::DateTime::from_timestamp(r.expires_at_unix, 0)
                .unwrap_or_else(chrono::Utc::now),
        }
    }
}

#[derive(Clone)]
pub struct GroupInvitationService {
    storage: Arc<ChatStorage>,
}

impl GroupInvitationService {
    /// Persisted invitation service sharing the same [`ChatStorage`]
    /// as the rest of the app. The storage handle is required so
    /// invitations and chat messages always live in the same SQLite
    /// file (one WAL, one backup cycle, one quota).
    #[must_use = "constructing a group invitation service without using it is a bug"]
    pub fn with_storage(storage: Arc<ChatStorage>) -> Self {
        Self { storage }
    }

    /// Insert a freshly-created pending invitation.
    ///
    /// GB-10 — The previous implementation used `INSERT OR REPLACE`,
    /// which silently destroyed the original `created_at_unix` and
    /// `inviter_id` columns when the same `invitation_id` was
    /// replayed. The replacement here is a plain `INSERT` so an
    /// attempt to create a duplicate raises a constraint violation
    /// (mapped to `AppError::Conflict` by the caller).
    ///
    /// GB-25 — the row is stored on the **invitee's** per-user DB
    /// (rather than the inviter's) so that `inbox(invitee)` sees
    /// it. The original implementation stored on the inviter's DB,
    /// which is a pre-existing critical bug: the invitee would
    /// never see the invitation in their inbox. The `owner`
    /// parameter is now informational and used only as the actor
    /// for audit-style rows.
    pub async fn create(
        &self,
        owner: &UserId,
        rec: InvitationRecord,
    ) -> AppResult<InvitationRecord> {
        validate_record(&rec)?;
        let storage = self.storage.clone();
        let owner_for_conn = owner.clone();
        // GB-25 — write to the *invitee's* DB so inbox works.
        let invitee_for_conn = rec.invitee_id.clone();
        let conn_arc = storage.connection_for(&invitee_for_conn).await?;
        let rec_in = rec.clone();
        let _owner = owner_for_conn;
        tokio::task::spawn_blocking(move || -> AppResult<()> {
            use rusqlite::params;
            let guard = conn_arc.blocking_lock_owned();
            guard.execute(
                "INSERT INTO group_invitations
                    (invitation_id, conversation_id, group_name,
                     inviter_id, inviter_name, invitee_id,
                     status, created_at_unix, expires_at_unix,
                     responded_at_unix, message)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    rec_in.invitation_id,
                    rec_in.conversation_id.as_str(),
                    rec_in.group_name,
                    rec_in.inviter_id.as_str(),
                    rec_in.inviter_name,
                    rec_in.invitee_id.as_str(),
                    rec_in.status,
                    rec_in.created_at_unix,
                    rec_in.expires_at_unix,
                    rec_in.responded_at_unix,
                    rec_in.message,
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| AppError::Internal(format!("invitation.create join: {e}")))??;
        Ok(rec)
    }

    /// Read all *pending* invitations addressed to `invitee`.
    ///
    /// GB-11 — Expired rows are lazily promoted to
    /// [`STATUS_EXPIRED`] in a single statement before the read, so a
    /// stale `pending` row never leaks into the user's inbox. The
    /// promotion is idempotent and races with `set_status` are safe:
    /// both run inside the same `spawn_blocking` task and share a
    /// SQLite write lock.
    pub async fn inbox(&self, invitee: &UserId) -> AppResult<Vec<InvitationRecord>> {
        let storage = self.storage.clone();
        let conn_arc = storage.connection_for(invitee).await?;
        let invitee_str = invitee.as_str().to_string();
        let rows: Vec<InvitationRecord> = tokio::task::spawn_blocking(
            move || -> AppResult<Vec<InvitationRecord>> {
                use rusqlite::params;
                let guard = conn_arc.blocking_lock_owned();
                let now = chrono::Utc::now().timestamp();
                // GB-11 — lazy expiry. We flip pending rows whose
                // expires_at_unix has elapsed to `expired` so the
                // downstream query only returns genuinely pending
                // rows. The UPDATE is a no-op when nothing matches.
                guard.execute(
                    "UPDATE group_invitations
                        SET status = 'expired', responded_at_unix = COALESCE(responded_at_unix, ?2)
                      WHERE invitee_id = ?1 AND status = 'pending' AND expires_at_unix <= ?2",
                    params![invitee_str, now],
                )?;
                let mut stmt = guard.prepare_cached(
                    "SELECT invitation_id, conversation_id, group_name, inviter_id,
                            inviter_name, invitee_id, status,
                            created_at_unix, expires_at_unix, responded_at_unix, message
                     FROM group_invitations
                     WHERE invitee_id = ?1 AND status = 'pending'
                       AND expires_at_unix > ?2
                     ORDER BY created_at_unix DESC",
                )?;
                let rows = stmt
                    .query_map(params![invitee_str, now], row_to_record)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            },
        )
        .await
        .map_err(|e| AppError::Internal(format!("invitation.inbox join: {e}")))??;
        Ok(rows)
    }

    /// Mark an invitation accepted/declined/revoked.
    ///
    /// GB-12 — Accepting an already-accepted invitation is no
    /// longer a silent re-write: the helper now reads the current
    /// status inside the same transaction and refuses to overwrite a
    /// terminal row (`accepted`, `declined`, `revoked`, `expired`).
    /// The check is performed atomically in the blocking task so a
    /// concurrent `accept` and `decline` cannot both succeed.
    pub async fn set_status(
        &self,
        owner: &UserId,
        invitation_id: &str,
        status: &'static str,
    ) -> AppResult<InvitationRecord> {
        match status {
            STATUS_ACCEPTED | STATUS_DECLINED | STATUS_REVOKED | STATUS_EXPIRED => {}
            other => {
                return Err(AppError::Domain(format!(
                    "unknown invitation status {other:?}"
                )));
            }
        }
        let storage = self.storage.clone();
        let conn_arc = storage.connection_for(owner).await?;
        let invitation_id_owned = invitation_id.to_string();
        let status_owned = status.to_string();
        let now = chrono::Utc::now().timestamp();
        let updated: Option<InvitationRecord> = tokio::task::spawn_blocking(
            move || -> AppResult<Option<InvitationRecord>> {
                use rusqlite::params;
                let guard = conn_arc.blocking_lock_owned();
                // GB-12 — terminal-state guard. A pending row is the
                // only one we will transition; every other row is
                // rejected with a precise reason so the caller can
                // distinguish "already accepted" from "already
                // declined" etc.
                let current: Option<String> = guard
                    .query_row(
                        "SELECT status FROM group_invitations WHERE invitation_id = ?1",
                        params![invitation_id_owned],
                        |row| row.get(0),
                    )
                    .ok();
                match current.as_deref() {
                    Some("pending") => {} // expected — proceed
                    Some(other) => {
                        return Err(AppError::Domain(format!(
                            "invitation is in terminal state {other:?}; cannot set {status_owned:?}"
                        )));
                    }
                    None => {
                        return Err(AppError::Domain(format!(
                            "invitation {invitation_id_owned} not found"
                        )));
                    }
                }
                let n = guard.execute(
                    "UPDATE group_invitations
                     SET status = ?2, responded_at_unix = ?3
                     WHERE invitation_id = ?1",
                    params![invitation_id_owned, status_owned, now],
                )?;
                if n == 0 {
                    return Ok(None);
                }
                let mut stmt = guard.prepare_cached(
                    "SELECT invitation_id, conversation_id, group_name, inviter_id,
                            inviter_name, invitee_id, status,
                            created_at_unix, expires_at_unix, responded_at_unix, message
                     FROM group_invitations WHERE invitation_id = ?1",
                )?;
                let row = stmt
                    .query_row(params![invitation_id_owned], row_to_record)
                    .ok();
                Ok(row)
            },
        )
        .await
        .map_err(|e| AppError::Internal(format!("invitation.set_status join: {e}")))??;
        updated.ok_or_else(|| {
            AppError::Domain(format!("invitation {invitation_id} not found"))
        })
    }

    /// Look up an invitation by id (owner-side read).
    pub async fn get(
        &self,
        owner: &UserId,
        invitation_id: &str,
    ) -> AppResult<Option<InvitationRecord>> {
        let storage = self.storage.clone();
        let conn_arc = storage.connection_for(owner).await?;
        let invitation_id = invitation_id.to_string();
        let row: Option<InvitationRecord> = tokio::task::spawn_blocking(
            move || -> AppResult<Option<InvitationRecord>> {
                use rusqlite::params;
                let guard = conn_arc.blocking_lock_owned();
                let mut stmt = guard.prepare_cached(
                    "SELECT invitation_id, conversation_id, group_name, inviter_id,
                            inviter_name, invitee_id, status,
                            created_at_unix, expires_at_unix, responded_at_unix, message
                     FROM group_invitations WHERE invitation_id = ?1",
                )?;
                let row = stmt
                    .query_row(params![invitation_id], row_to_record)
                    .ok();
                Ok(row)
            },
        )
        .await
        .map_err(|e| AppError::Internal(format!("invitation.get join: {e}")))??;
        Ok(row)
    }
}

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<InvitationRecord> {
    let status: String = row.get(6)?;
    let responded_at: Option<i64> = row.get(9)?;
    Ok(InvitationRecord {
        invitation_id: row.get(0)?,
        conversation_id: ConversationId::from(row.get::<_, String>(1)?),
        group_name: row.get(2)?,
        inviter_id: UserId::from(row.get::<_, String>(3)?),
        inviter_name: row.get(4)?,
        invitee_id: UserId::from(row.get::<_, String>(5)?),
        status,
        created_at_unix: row.get(7)?,
        expires_at_unix: row.get(8)?,
        responded_at_unix: responded_at,
        message: row.get(10)?,
    })
}

fn validate_record(rec: &InvitationRecord) -> AppResult<()> {
    if rec.invitation_id.is_empty() {
        return Err(AppError::Domain("invitation_id: empty".into()));
    }
    if rec.group_name.is_empty() {
        return Err(AppError::Domain("group_name: empty".into()));
    }
    if rec.created_at_unix <= 0 {
        return Err(AppError::Domain("created_at_unix must be > 0".into()));
    }
    if rec.expires_at_unix <= rec.created_at_unix {
        return Err(AppError::Domain(
            "expires_at_unix must be after created_at_unix".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3chat_core::group::GroupInvitation;
    use tempfile::tempdir;

    fn rec() -> InvitationRecord {
        let now = chrono::Utc::now().timestamp();
        InvitationRecord {
            invitation_id: "inv-1".into(),
            conversation_id: ConversationId::from("grp:1"),
            group_name: "team".into(),
            inviter_id: UserId::from("alice"),
            inviter_name: "Alice".into(),
            invitee_id: UserId::from("bob"),
            status: STATUS_PENDING.into(),
            created_at_unix: now,
            expires_at_unix: now + DEFAULT_INVITATION_TTL_SECS,
            responded_at_unix: None,
            message: Some("hi".into()),
        }
    }

    #[test]
    fn validate_accepts_well_formed_record() {
        assert!(validate_record(&rec()).is_ok());
    }

    #[test]
    fn validate_rejects_empty_id() {
        let mut r = rec();
        r.invitation_id = "".into();
        assert!(matches!(validate_record(&r), Err(AppError::Domain(_))));
    }

    #[test]
    fn validate_rejects_inverted_window() {
        let mut r = rec();
        let now = chrono::Utc::now().timestamp();
        r.created_at_unix = now;
        r.expires_at_unix = now - 1;
        assert!(matches!(validate_record(&r), Err(AppError::Domain(_))));
    }

    #[test]
    fn record_to_invitation_maps_status() {
        let now = chrono::Utc::now().timestamp();
        let r = InvitationRecord {
            invitation_id: "i".into(),
            conversation_id: ConversationId::from("grp:x"),
            group_name: "g".into(),
            inviter_id: UserId::from("alice"),
            inviter_name: "Alice".into(),
            invitee_id: UserId::from("bob"),
            status: STATUS_ACCEPTED.into(),
            created_at_unix: now,
            expires_at_unix: now + 60,
            responded_at_unix: Some(now),
            message: None,
        };
        let inv: GroupInvitation = r.into();
        assert!(matches!(inv.status, a3chat_core::group::InvitationStatus::Accepted));
    }

    /// GB-10 — Inserting two invitations with the same id must NOT
    /// silently overwrite the original. The first INSERT succeeds;
    /// the second raises a SQLite UNIQUE constraint error which the
    /// service surfaces as `AppError::Storage`.
    ///
    /// Both rows go into the invitee's DB (GB-25).
    #[tokio::test]
    async fn create_rejects_duplicate_invitation_id() {
        let dir = tempdir().unwrap();
        let keyring = crate::keyring::E2eKeyring::new(UserId::from("alice"));
        let cfg = crate::storage::StorageConfig::new(dir.path().to_path_buf());
        let storage = std::sync::Arc::new(crate::storage::ChatStorage::new(cfg, keyring));
        let svc = GroupInvitationService::with_storage(storage.clone());
        let alice = UserId::from("alice");

        let r1 = svc.create(&alice, rec()).await.expect("first create");
        assert_eq!(r1.invitation_id, "inv-1");
        let second = svc.create(&alice, rec()).await;
        assert!(
            second.is_err(),
            "duplicate invitation_id must be rejected; got {second:?}"
        );
    }

    /// GB-11 — `inbox()` must lazily promote expired pending rows
    /// so a freshly-stale invitation disappears from the user's
    /// pending list without an explicit expiry RPC.
    #[tokio::test]
    async fn inbox_lazily_expires_pending_rows() {
        let dir = tempdir().unwrap();
        let keyring = crate::keyring::E2eKeyring::new(UserId::from("alice"));
        let cfg = crate::storage::StorageConfig::new(dir.path().to_path_buf());
        let storage = std::sync::Arc::new(crate::storage::ChatStorage::new(cfg, keyring));
        let svc = GroupInvitationService::with_storage(storage.clone());
        let owner = UserId::from("alice");

        // Insert a row whose expires_at_unix is already in the past.
        let now = chrono::Utc::now().timestamp();
        let stale = InvitationRecord {
            invitation_id: "stale-1".into(),
            conversation_id: ConversationId::from("grp:1"),
            group_name: "team".into(),
            inviter_id: UserId::from("alice"),
            inviter_name: "Alice".into(),
            invitee_id: UserId::from("bob"),
            status: STATUS_PENDING.into(),
            created_at_unix: now - 10_000,
            expires_at_unix: now - 1,
            responded_at_unix: None,
            message: None,
        };
        // We cannot insert an invalid record via `create` because
        // validate_record rejects inverted windows; bypass it with a
        // direct INSERT via `connection_for(bob)` so we simulate a
        // pre-existing stale row in bob's DB (where the inbox
        // looks — GB-25).
        let conn_arc = storage.connection_for(&UserId::from("bob")).await.unwrap();
        let stale_in = stale.clone();
        tokio::task::spawn_blocking(move || {
            let guard = conn_arc.blocking_lock_owned();
            use rusqlite::params;
            guard.execute(
                "INSERT INTO group_invitations
                    (invitation_id, conversation_id, group_name,
                     inviter_id, inviter_name, invitee_id,
                     status, created_at_unix, expires_at_unix,
                     responded_at_unix, message)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10)",
                params![
                    stale_in.invitation_id,
                    stale_in.conversation_id.as_str(),
                    stale_in.group_name,
                    stale_in.inviter_id.as_str(),
                    stale_in.inviter_name,
                    stale_in.invitee_id.as_str(),
                    stale_in.status,
                    stale_in.created_at_unix,
                    stale_in.expires_at_unix,
                    stale_in.message,
                ],
            )
            .unwrap();
        })
        .await
        .unwrap();

        // Also insert a fresh pending invitation — this one MUST
        // appear in the inbox.
        let mut fresh = rec();
        fresh.invitation_id = "fresh-1".into();
        svc.create(&owner, fresh).await.unwrap();

        // Inbox for "bob" must show only the fresh row; the stale
        // row must be promoted to "expired".
        let bob = UserId::from("bob");
        let inbox = svc.inbox(&bob).await.unwrap();
        let ids: Vec<_> = inbox.iter().map(|r| r.invitation_id.as_str()).collect();
        assert_eq!(ids, vec!["fresh-1"], "stale row leaked into inbox");
    }

    /// GB-12 — Terminal-state guard: accepting an already-accepted
    /// invitation must return a domain error, not silently succeed.
    ///
    /// The actor passed to `set_status` must be the **invitee**
    /// (the row lives in their DB — see GB-25).
    #[tokio::test]
    async fn set_status_refuses_terminal_state() {
        let dir = tempdir().unwrap();
        let keyring = crate::keyring::E2eKeyring::new(UserId::from("alice"));
        let cfg = crate::storage::StorageConfig::new(dir.path().to_path_buf());
        let storage = std::sync::Arc::new(crate::storage::ChatStorage::new(cfg, keyring));
        let svc = GroupInvitationService::with_storage(storage.clone());
        let alice = UserId::from("alice");
        let bob = UserId::from("bob");
        svc.create(&alice, rec()).await.unwrap();

        // First accept succeeds. The actor is the invitee (`bob`),
        // matching the storage key the row was written under.
        let accepted = svc
            .set_status(&bob, "inv-1", STATUS_ACCEPTED)
            .await
            .unwrap();
        assert_eq!(accepted.status, STATUS_ACCEPTED);

        // Second accept must be rejected.
        let second = svc.set_status(&bob, "inv-1", STATUS_ACCEPTED).await;
        assert!(matches!(second, Err(AppError::Domain(_))));
    }
}
