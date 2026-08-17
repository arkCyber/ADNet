//! `a3chat` Link bookmark / favorites service (F-08).
//!
//! Bridges the [`a3net_chatstore::LinkBookmarkStore`] (per-user
//! SQLite-backed store) onto the `a3chat-app` facade so it can be
//! called via the `a3chat.link.bookmark.*` JSON-RPC namespace.
//!
//! # Responsibilities
//!
//! 1. **Validation**: every write passes through
//!    [`UpsertLinkBookmarkRequest::validate`] before SQLite.
//! 2. **Bus publication**: every successful write fires a
//!    `LinkBookmarkAdded` / `LinkBookmarkUpdated` /
//!    `LinkBookmarkDeleted` event on the in-process
//!    [`NotificationBus`] so SSE clients refresh their in-memory
//!    state without polling.
//! 3. **Resource clamping**: `list`, `search`, `tags`, `folders`
//!    each enforce their own caps so a malicious / buggy client
//!    can't blow out memory.
//!
//! # What this service deliberately does **not** do
//!
//! - Cross-device sync (flagged for a later release; the
//!   `(owner_id, url, created_at_unix)` identity makes dedupe
//!   trivial to add later).
//! - URL fetching / snapshotting / favicon download. The store
//!   accepts those fields but the `a3chat.app.link_bookmark.fetch`
//!   helper that populates them lives in a separate module and is
//!   only invoked when the caller opts in.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};

use a3chat_core::error::A3chatError;
use a3chat_core::event::A3chatEvent;
use a3chat_core::id::UserId;
use a3chat_core::link_bookmark::{
    self, BookmarkSource as CoreBookmarkSource, LinkBookmarkCount, LinkBookmarkListFilter,
    LinkBookmarkSearchQuery, LinkFolderNode, LinkTagCount, UpsertLinkBookmarkRequest,
};
use a3chat_core::rpc::A3chatRpcMethod;
use a3net_chatstore::LinkBookmark;

use crate::error::{AppError, AppResult};
use crate::notification_bus::NotificationBus;
use crate::storage::ChatStorage;

/// RPC method-name constants owned by this module. Mirror of
/// `a3chat_core::rpc::A3chatRpcMethod::LINK_BOOKMARK_*` so the
/// dispatcher can pattern-match without re-importing every call
/// site.
pub const METHODS: &[&str] = &[
    A3chatRpcMethod::LINK_BOOKMARK_ADD,
    A3chatRpcMethod::LINK_BOOKMARK_UPDATE,
    A3chatRpcMethod::LINK_BOOKMARK_GET,
    A3chatRpcMethod::LINK_BOOKMARK_GET_BY_URL,
    A3chatRpcMethod::LINK_BOOKMARK_LIST,
    A3chatRpcMethod::LINK_BOOKMARK_SEARCH,
    A3chatRpcMethod::LINK_BOOKMARK_DELETE,
    A3chatRpcMethod::LINK_BOOKMARK_SET_PINNED,
    A3chatRpcMethod::LINK_BOOKMARK_SET_ARCHIVED,
    A3chatRpcMethod::LINK_BOOKMARK_TOUCH_VISIT,
    A3chatRpcMethod::LINK_BOOKMARK_TAGS,
    A3chatRpcMethod::LINK_BOOKMARK_FOLDERS,
    A3chatRpcMethod::LINK_BOOKMARK_COUNT,
];

/// Default `limit` for [`LinkBookmarkService::list`] when the
/// caller doesn't set one.
const DEFAULT_LIST_LIMIT: u32 = 200;

/// Hard cap for [`LinkBookmarkService::list`]. Above this we
/// reject the request rather than silently truncating — the
/// frontend should paginate.
const MAX_LIST_LIMIT: u32 = 1000;

/// Default `limit` for [`LinkBookmarkService::search`].
const DEFAULT_SEARCH_LIMIT: u32 = 50;

/// Hard cap for [`LinkBookmarkService::search`].
const MAX_SEARCH_LIMIT: u32 = 200;

/// Configuration for [`LinkBookmarkService`].
#[derive(Debug, Clone)]
pub struct LinkBookmarkConfig {
    /// Base directory. Bookmarks share the per-user chat DB so this
    /// field is only used for diagnostics; the actual file is owned
    /// by [`ChatStorage`].
    pub base_dir: PathBuf,
}

impl LinkBookmarkConfig {
    /// Build a config under `<base>` — currently just an alias; the
    /// real directory is the chat-storage base.
    pub fn under_base(base: &Path) -> Self {
        Self {
            base_dir: base.to_path_buf(),
        }
    }
}

/// Cheap-clone handle to the link-bookmark runtime.
///
/// Internally holds a [`ChatStorage`] handle (which itself is a
/// cheap `Arc`) so we always re-use the per-user SQLite connection
/// the chat service opened — no second file, no schema drift.
#[derive(Clone)]
pub struct LinkBookmarkService {
    storage: ChatStorage,
    bus: NotificationBus,
    #[allow(dead_code)]
    config: LinkBookmarkConfig,
}

impl LinkBookmarkService {
    /// Construct from a [`ChatStorage`] handle and the
    /// [`NotificationBus`]. The storage handle already owns the
    /// per-user SQLite file; this service just borrows it.
    pub fn new(storage: ChatStorage, bus: NotificationBus, config: LinkBookmarkConfig) -> Self {
        Self {
            storage,
            bus,
            config,
        }
    }

    /// Borrow the underlying chat storage. Mostly for tests.
    pub fn storage(&self) -> &ChatStorage {
        &self.storage
    }

    /// Borrow the bus (used by the dispatcher for tests that assert
    /// on event publication).
    pub fn bus(&self) -> &NotificationBus {
        &self.bus
    }

    /// Borrow the config (used by tests / diagnostics).
    pub fn config(&self) -> &LinkBookmarkConfig {
        &self.config
    }

    async fn store(&self, owner: &UserId) -> AppResult<a3net_chatstore::LinkBookmarkStore> {
        self.storage.link_bookmark_store(owner).await
    }

    /// `a3chat.link.bookmark.add` — insert a new bookmark. If a row
    /// with the same `(owner_id, url, created_at_unix)` already
    /// exists, it is overwritten (so identical retries dedupe).
    pub async fn add(
        &self,
        owner: &UserId,
        request: UpsertLinkBookmarkRequest,
    ) -> AppResult<LinkBookmark> {
        request.validate().map_err(AppError::from)?;
        let now = chrono::Utc::now();
        let bookmark = build_storage_bookmark_from_request(
            request,
            owner.as_str(),
            now,
        )
        .map_err(AppError::from)?;
        let store = self.store(owner).await?;
        store.put(bookmark.clone()).map_err(AppError::from)?;
        self.bus.publish(A3chatEvent::LinkBookmarkAdded {
            user_id: owner.clone(),
            bookmark: bookmark.clone(),
        });
        Ok(bookmark)
    }

    /// `a3chat.link.bookmark.update` — update fields on an existing
    /// bookmark. The client supplies the entire desired state;
    /// missing/empty fields are taken from the stored row.
    pub async fn update(
        &self,
        owner: &UserId,
        bookmark_id: &str,
        request: UpsertLinkBookmarkRequest,
    ) -> AppResult<LinkBookmark> {
        request.validate().map_err(AppError::from)?;
        let store = self.store(owner).await?;
        let existing = store
            .get(owner.as_str(), bookmark_id)
            .map_err(AppError::from)?
            .ok_or_else(|| {
                AppError::Domain(format!(
                    "bookmark not found: {bookmark_id} (owner {})",
                    owner.as_str()
                ))
            })?;
        let merged = merge_bookmark(existing, request, owner.as_str())?;
        merged.validate().map_err(AppError::from)?;
        store.put(merged.clone()).map_err(AppError::from)?;
        self.bus.publish(A3chatEvent::LinkBookmarkUpdated {
            user_id: owner.clone(),
            bookmark: merged.clone(),
        });
        Ok(merged)
    }

    /// `a3chat.link.bookmark.get` — fetch by `(owner_id,
    /// bookmark_id)`.
    pub async fn get(
        &self,
        owner: &UserId,
        bookmark_id: &str,
    ) -> AppResult<LinkBookmark> {
        let store = self.store(owner).await?;
        store
            .get(owner.as_str(), bookmark_id)
            .map_err(AppError::from)?
            .ok_or_else(|| {
                AppError::Domain(format!(
                    "bookmark not found: {bookmark_id} (owner {})",
                    owner.as_str()
                ))
            })
    }

    /// `a3chat.link.bookmark.get_by_url` — fetch by URL.
    pub async fn get_by_url(
        &self,
        owner: &UserId,
        url: &str,
    ) -> AppResult<Option<LinkBookmark>> {
        let store = self.store(owner).await?;
        store.get_by_url(owner.as_str(), url).map_err(AppError::from)
    }

    /// `a3chat.link.bookmark.list` — list bookmarks for the owner,
    /// honouring the supplied filter.
    pub async fn list(
        &self,
        owner: &UserId,
        mut filter: LinkBookmarkListFilter,
    ) -> AppResult<Vec<LinkBookmark>> {
        let limit = filter.limit.unwrap_or(DEFAULT_LIST_LIMIT);
        if limit == 0 || limit > MAX_LIST_LIMIT {
            return Err(AppError::Domain(format!(
                "limit {limit} not in 1..={MAX_LIST_LIMIT}"
            )));
        }
        filter.limit = Some(limit);
        // Coerce `is_archived` to `Some(false)` by default so the
        // archived drawer doesn't accidentally leak into the main
        // view. Clients that *want* archived rows pass `Some(true)`.
        if filter.is_archived.is_none() {
            filter.is_archived = Some(false);
        }
        let store = self.store(owner).await?;
        store
            .list(owner.as_str(), build_list_filter(filter))
            .map_err(AppError::from)
    }

    /// `a3chat.link.bookmark.search` — fuzzy keyword search across
    /// title / description / url / tags.
    pub async fn search(
        &self,
        owner: &UserId,
        mut query: LinkBookmarkSearchQuery,
    ) -> AppResult<Vec<LinkBookmark>> {
        query.validate().map_err(AppError::from)?;
        let limit = query.limit.unwrap_or(DEFAULT_SEARCH_LIMIT);
        if limit == 0 || limit > MAX_SEARCH_LIMIT {
            return Err(AppError::Domain(format!(
                "limit {limit} not in 1..={MAX_SEARCH_LIMIT}"
            )));
        }
        query.limit = Some(limit);
        let store = self.store(owner).await?;
        store
            .search(owner.as_str(), &query.needle, limit)
            .map_err(AppError::from)
    }

    /// `a3chat.link.bookmark.delete` — remove a bookmark by id.
    pub async fn delete(
        &self,
        owner: &UserId,
        bookmark_id: &str,
    ) -> AppResult<()> {
        let store = self.store(owner).await?;
        // Resolve the URL first so we can emit it on the deletion
        // event. Clients use that as a hint to invalidate their
        // preview cache.
        let url = store
            .get(owner.as_str(), bookmark_id)
            .map_err(AppError::from)?
            .map(|b| b.url)
            .unwrap_or_default();
        store
            .delete(owner.as_str(), bookmark_id)
            .map_err(AppError::from)?;
        self.bus.publish(A3chatEvent::LinkBookmarkDeleted {
            user_id: owner.clone(),
            bookmark_id: bookmark_id.to_string(),
            url,
        });
        Ok(())
    }

    /// `a3chat.link.bookmark.set_pinned` — toggle the pinned flag.
    pub async fn set_pinned(
        &self,
        owner: &UserId,
        bookmark_id: &str,
        is_pinned: bool,
    ) -> AppResult<LinkBookmark> {
        let store = self.store(owner).await?;
        let updated = store
            .set_pinned(owner.as_str(), bookmark_id, is_pinned)
            .map_err(AppError::from)?;
        self.bus.publish(A3chatEvent::LinkBookmarkUpdated {
            user_id: owner.clone(),
            bookmark: updated.clone(),
        });
        Ok(updated)
    }

    /// `a3chat.link.bookmark.set_archived` — toggle the archived
    /// flag.
    pub async fn set_archived(
        &self,
        owner: &UserId,
        bookmark_id: &str,
        is_archived: bool,
    ) -> AppResult<LinkBookmark> {
        let store = self.store(owner).await?;
        let updated = store
            .set_archived(owner.as_str(), bookmark_id, is_archived)
            .map_err(AppError::from)?;
        self.bus.publish(A3chatEvent::LinkBookmarkUpdated {
            user_id: owner.clone(),
            bookmark: updated.clone(),
        });
        Ok(updated)
    }

    /// `a3chat.link.bookmark.touch_visit` — record that the user
    /// opened this bookmark. Bumps `visit_count` and sets
    /// `last_visited_at`.
    pub async fn touch_visit(
        &self,
        owner: &UserId,
        bookmark_id: &str,
    ) -> AppResult<LinkBookmark> {
        let store = self.store(owner).await?;
        let updated = store
            .touch_visit(owner.as_str(), bookmark_id)
            .map_err(AppError::from)?;
        self.bus.publish(A3chatEvent::LinkBookmarkUpdated {
            user_id: owner.clone(),
            bookmark: updated.clone(),
        });
        Ok(updated)
    }

    /// `a3chat.link.bookmark.tags` — list every distinct tag with
    /// its row count, sorted by count desc.
    pub async fn tags(&self, owner: &UserId) -> AppResult<Vec<LinkTagCount>> {
        let store = self.store(owner).await?;
        store.tags(owner.as_str()).map_err(AppError::from)
    }

    /// `a3chat.link.bookmark.folders` — list every folder path
    /// (including the root `/`) with the number of direct children.
    pub async fn folders(&self, owner: &UserId) -> AppResult<Vec<LinkFolderNode>> {
        let store = self.store(owner).await?;
        store.folders(owner.as_str()).map_err(AppError::from)
    }

    /// `a3chat.link.bookmark.count` — total / pinned / archived
    /// counts for the owner.
    pub async fn count(&self, owner: &UserId) -> AppResult<LinkBookmarkCount> {
        let store = self.store(owner).await?;
        let total = store
            .count(owner.as_str(), a3net_chatstore::link_bookmark::CountFilter::All)
            .map_err(AppError::from)?;
        let pinned = store
            .count(
                owner.as_str(),
                a3net_chatstore::link_bookmark::CountFilter::Pinned,
            )
            .map_err(AppError::from)?;
        let archived = store
            .count(
                owner.as_str(),
                a3net_chatstore::link_bookmark::CountFilter::Archived,
            )
            .map_err(AppError::from)?;
        Ok(LinkBookmarkCount {
            total,
            pinned,
            archived,
        })
    }
}

/// Convert the wire-level [`LinkBookmarkListFilter`] into the
/// storage-level [`a3net_chatstore::link_bookmark::ListFilter`].
/// Field names differ (snake_case wire vs. shorter names internally)
/// and three-state `Option<bool>`s collapse to the boolean flags the
/// SQL layer needs.
///
/// Free function (not `impl From`) because the orphan rule forbids
/// providing a foreign trait impl for a foreign type from a
/// third-party crate.
fn build_list_filter(wire: LinkBookmarkListFilter) -> a3net_chatstore::link_bookmark::ListFilter {
    let only_pinned = matches!(wire.is_pinned, Some(true));
    let only_archived = matches!(wire.is_archived, Some(true));
    let folder_prefix = wire.folder.filter(|s| !s.is_empty());
    a3net_chatstore::link_bookmark::ListFilter {
        folder_prefix,
        include_subfolders: wire.include_subfolders,
        tags: wire.tags,
        only_pinned,
        only_archived,
        limit: wire.limit.unwrap_or(DEFAULT_LIST_LIMIT),
        before_unix: None,
    }
}

/// Build a fresh [`a3net_chatstore::LinkBookmark`] from an
/// incoming [`UpsertLinkBookmarkRequest`]. Mirrors
/// [`UpsertLinkBookmarkRequest::into_bookmark`] but is structured
/// as a free function so the storage call sites can stay
/// uniform (and so we can re-use it from `merge_bookmark`).
fn build_storage_bookmark_from_request(
    request: UpsertLinkBookmarkRequest,
    owner_id: &str,
    created_at: DateTime<Utc>,
) -> Result<LinkBookmark, A3chatError> {
    let normalized_tags = link_bookmark::normalize_tags(request.tags.iter())?;
    // Truncate timestamps to whole-second precision so they round-trip
    // cleanly through SQLite (which stores `created_at_unix` as a Unix
    // epoch second). Without this, `add()` would return a bookmark with
    // sub-second precision that does not match the row read back from
    // `get()` after storage.
    let created_at_seconds = Utc
        .timestamp_opt(created_at.timestamp(), 0)
        .single()
        .unwrap_or(created_at);
    let now_seconds = Utc
        .timestamp_opt(Utc::now().timestamp(), 0)
        .single()
        .unwrap_or_else(Utc::now);
    let bookmark_id = a3net_chatstore::compute_bookmark_id(
        owner_id,
        &request.url,
        created_at_seconds.timestamp(),
    );
    Ok(LinkBookmark {
        bookmark_id,
        owner_id: owner_id.to_string(),
        url: request.url,
        title: request.title,
        description: request.description,
        favicon_hash: request.favicon_hash,
        folder: if request.folder.is_empty() {
            link_bookmark::DEFAULT_FOLDER.to_string()
        } else {
            request.folder
        },
        tags: normalized_tags,
        is_pinned: request.is_pinned,
        is_archived: request.is_archived,
        snapshot_text: request.snapshot_text,
        source: request.source,
        created_at: created_at_seconds,
        updated_at: now_seconds,
        last_visited_at: None,
        visit_count: 0,
    })
}

/// Merge an incoming [`UpsertLinkBookmarkRequest`] over an existing
/// stored [`LinkBookmark`]. The merged record preserves the
/// original `bookmark_id`, `created_at`, `visit_count` and
/// `last_visited_at` — only the user-controlled fields change.
fn merge_bookmark(
    existing: LinkBookmark,
    request: UpsertLinkBookmarkRequest,
    owner_id: &str,
) -> Result<LinkBookmark, A3chatError> {
    // Re-derive the bookmark_id from the *original* created_at so
    // identity stays stable across edits.
    let bookmark_id = a3net_chatstore::compute_bookmark_id(
        owner_id,
        &existing.url,
        existing.created_at.timestamp(),
    );
    let normalized_tags = link_bookmark::normalize_tags(request.tags.iter())?;
    Ok(LinkBookmark {
        bookmark_id,
        owner_id: owner_id.to_string(),
        url: request.url,
        title: request.title,
        description: request.description.or(existing.description),
        favicon_hash: request.favicon_hash.or(existing.favicon_hash),
        folder: if request.folder.is_empty() {
            existing.folder
        } else {
            request.folder
        },
        tags: normalized_tags,
        is_pinned: request.is_pinned,
        is_archived: request.is_archived,
        snapshot_text: request.snapshot_text.or(existing.snapshot_text),
        source: if matches!(request.source, CoreBookmarkSource::User) {
            existing.source
        } else {
            request.source
        },
        created_at: existing.created_at,
        updated_at: Utc
            .timestamp_opt(Utc::now().timestamp(), 0)
            .single()
            .unwrap_or_else(Utc::now),
        last_visited_at: existing.last_visited_at,
        visit_count: existing.visit_count,
    })
}

/// JSON-RPC dispatcher — invoked from
/// `A3chatApp::dispatch` when the method starts with
/// `a3chat.link.bookmark.*`.
pub async fn dispatch(
    svc: Arc<LinkBookmarkService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    match method {
        A3chatRpcMethod::LINK_BOOKMARK_ADD => {
            let req: UpsertLinkBookmarkRequest =
                serde_json::from_value(params).map_err(|e| {
                    A3chatError::InvalidInput(format!("invalid add payload: {e}"))
                })?;
            let b = svc.add(owner, req).await.map_err(A3chatError::from)?;
            serde_json::to_value(b).map_err(A3chatError::from)
        }
        A3chatRpcMethod::LINK_BOOKMARK_UPDATE => {
            let bookmark_id: String = params
                .get("bookmark_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("bookmark_id missing".into()))?
                .to_string();
            let req: UpsertLinkBookmarkRequest = serde_json::from_value(
                params
                    .get("request")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("request missing".into()))?,
            )
            .map_err(|e| A3chatError::InvalidInput(format!("invalid update payload: {e}")))?;
            let b = svc
                .update(owner, &bookmark_id, req)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(b).map_err(A3chatError::from)
        }
        A3chatRpcMethod::LINK_BOOKMARK_GET => {
            let bookmark_id: String = params
                .get("bookmark_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("bookmark_id missing".into()))?
                .to_string();
            let b = svc.get(owner, &bookmark_id).await.map_err(A3chatError::from)?;
            serde_json::to_value(b).map_err(A3chatError::from)
        }
        A3chatRpcMethod::LINK_BOOKMARK_GET_BY_URL => {
            let url: String = params
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("url missing".into()))?
                .to_string();
            let b = svc
                .get_by_url(owner, &url)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(b).map_err(A3chatError::from)
        }
        A3chatRpcMethod::LINK_BOOKMARK_LIST => {
            let filter: LinkBookmarkListFilter = serde_json::from_value(
                params.get("filter").cloned().unwrap_or(serde_json::json!({})),
            )
            .map_err(|e| A3chatError::InvalidInput(format!("invalid filter: {e}")))?;
            let rows = svc.list(owner, filter).await.map_err(A3chatError::from)?;
            serde_json::to_value(rows).map_err(A3chatError::from)
        }
        A3chatRpcMethod::LINK_BOOKMARK_SEARCH => {
            let query: LinkBookmarkSearchQuery =
                serde_json::from_value(params).map_err(|e| {
                    A3chatError::InvalidInput(format!("invalid search payload: {e}"))
                })?;
            let rows = svc.search(owner, query).await.map_err(A3chatError::from)?;
            serde_json::to_value(rows).map_err(A3chatError::from)
        }
        A3chatRpcMethod::LINK_BOOKMARK_DELETE => {
            let bookmark_id: String = params
                .get("bookmark_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("bookmark_id missing".into()))?
                .to_string();
            svc.delete(owner, &bookmark_id)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        A3chatRpcMethod::LINK_BOOKMARK_SET_PINNED => {
            let bookmark_id: String = params
                .get("bookmark_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("bookmark_id missing".into()))?
                .to_string();
            let is_pinned: bool = params
                .get("is_pinned")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| A3chatError::InvalidInput("is_pinned missing".into()))?;
            let b = svc
                .set_pinned(owner, &bookmark_id, is_pinned)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(b).map_err(A3chatError::from)
        }
        A3chatRpcMethod::LINK_BOOKMARK_SET_ARCHIVED => {
            let bookmark_id: String = params
                .get("bookmark_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("bookmark_id missing".into()))?
                .to_string();
            let is_archived: bool = params
                .get("is_archived")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| A3chatError::InvalidInput("is_archived missing".into()))?;
            let b = svc
                .set_archived(owner, &bookmark_id, is_archived)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(b).map_err(A3chatError::from)
        }
        A3chatRpcMethod::LINK_BOOKMARK_TOUCH_VISIT => {
            let bookmark_id: String = params
                .get("bookmark_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("bookmark_id missing".into()))?
                .to_string();
            let b = svc
                .touch_visit(owner, &bookmark_id)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(b).map_err(A3chatError::from)
        }
        A3chatRpcMethod::LINK_BOOKMARK_TAGS => {
            let rows = svc.tags(owner).await.map_err(A3chatError::from)?;
            serde_json::to_value(rows).map_err(A3chatError::from)
        }
        A3chatRpcMethod::LINK_BOOKMARK_FOLDERS => {
            let rows = svc.folders(owner).await.map_err(A3chatError::from)?;
            serde_json::to_value(rows).map_err(A3chatError::from)
        }
        A3CHAT_METHOD_LINK_BOOKMARK_COUNT => {
            let row = svc.count(owner).await.map_err(A3chatError::from)?;
            serde_json::to_value(row).map_err(A3chatError::from)
        }
        _ => Err(A3chatError::Internal(format!(
            "LinkBookmarkService does not handle {method}"
        ))),
    }
}

/// `A3chatRpcMethod::LINK_BOOKMARK_COUNT` re-imported under a local
/// name so the match arm above stays short.
const A3CHAT_METHOD_LINK_BOOKMARK_COUNT: &str = A3chatRpcMethod::LINK_BOOKMARK_COUNT;

// `link_bookmark::DEFAULT_FOLDER` is re-exported through `lib.rs`
// but we keep a local alias to make the test fixture below less
// verbose.
#[allow(dead_code)]
const DEFAULT_FOLDER: &str = link_bookmark::DEFAULT_FOLDER;

#[cfg(test)]
mod tests {
    use super::*;
    use a3chat_core::link_bookmark::DEFAULT_FOLDER;
    use tempfile::tempdir;

    fn sample_request(url: &str, title: &str) -> UpsertLinkBookmarkRequest {
        UpsertLinkBookmarkRequest {
            url: url.to_string(),
            title: title.to_string(),
            description: Some("note".into()),
            favicon_hash: None,
            folder: DEFAULT_FOLDER.to_string(),
            tags: vec!["Rust".into(), "docs".into()],
            is_pinned: false,
            is_archived: false,
            snapshot_text: None,
            source: CoreBookmarkSource::User,
        }
    }

    fn build_service() -> (tempfile::TempDir, LinkBookmarkService, UserId) {
        let dir = tempdir().expect("tempdir");
        let cfg = crate::storage::StorageConfig::new(dir.path().to_path_buf());
        let owner = UserId::from("user:alice");
        let keyring = crate::keyring::E2eKeyring::new(owner.clone());
        let storage = ChatStorage::new(cfg, keyring);
        let bus = NotificationBus::new(NotificationBus::default_capacity());
        let cfg = LinkBookmarkConfig::under_base(dir.path());
        (
            dir,
            LinkBookmarkService::new(storage, bus, cfg),
            owner,
        )
    }

    #[tokio::test]
    async fn add_then_get_round_trip() -> AppResult<()> {
        let (_dir, svc, owner) = build_service();
        let req = sample_request("https://example.com", "Example");
        let b = svc.add(&owner, req).await?;
        assert_eq!(b.url, "https://example.com");
        assert_eq!(b.tags, vec!["rust", "docs"]);

        let fetched = svc.get(&owner, &b.bookmark_id).await?;
        assert_eq!(fetched, b);

        // Also retrievable by URL.
        let by_url = svc
            .get_by_url(&owner, "https://example.com")
            .await?
            .expect("present");
        assert_eq!(by_url.bookmark_id, b.bookmark_id);
        Ok(())
    }

    #[tokio::test]
    async fn update_merges_fields_and_publishes_event() -> AppResult<()> {
        let (_dir, svc, owner) = build_service();
        let initial = svc
            .add(
                &owner,
                sample_request("https://example.com", "Example"),
            )
            .await?;
        let mut rx = svc.bus().subscribe_for(owner.clone());

        let mut req = sample_request("https://example.com", "Example v2");
        req.is_pinned = true;
        req.tags = vec!["important".into()];
        let updated = svc.update(&owner, &initial.bookmark_id, req).await?;
        assert_eq!(updated.title, "Example v2");
        assert!(updated.is_pinned);
        assert_eq!(updated.tags, vec!["important"]);
        assert_eq!(
            updated.created_at, initial.created_at,
            "created_at must be stable across updates"
        );
        assert_eq!(updated.bookmark_id, initial.bookmark_id);

        // The bus must publish a LinkBookmarkUpdated event.
        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("event arrives within 100ms")
            .expect("event is not lagged");
        match evt {
            A3chatEvent::LinkBookmarkUpdated { bookmark, .. } => {
                assert_eq!(bookmark.bookmark_id, initial.bookmark_id);
            }
            other => panic!("unexpected event: {other:?}"),
        }
        Ok(())
    }

    #[tokio::test]
    async fn delete_removes_and_emits_event() -> AppResult<()> {
        let (_dir, svc, owner) = build_service();
        let b = svc
            .add(&owner, sample_request("https://example.com", "x"))
            .await?;
        let mut rx = svc.bus().subscribe_for(owner.clone());

        svc.delete(&owner, &b.bookmark_id).await?;
        assert!(svc.get(&owner, &b.bookmark_id).await.is_err());

        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match evt {
            A3chatEvent::LinkBookmarkDeleted {
                bookmark_id, url, ..
            } => {
                assert_eq!(bookmark_id, b.bookmark_id);
                assert_eq!(url, b.url);
            }
            other => panic!("unexpected event: {other:?}"),
        }
        Ok(())
    }

    #[tokio::test]
    async fn set_pinned_and_archived_round_trip() -> AppResult<()> {
        let (_dir, svc, owner) = build_service();
        let b = svc
            .add(&owner, sample_request("https://example.com", "x"))
            .await?;
        let pinned = svc.set_pinned(&owner, &b.bookmark_id, true).await?;
        assert!(pinned.is_pinned);

        let archived = svc
            .set_archived(&owner, &b.bookmark_id, true)
            .await?;
        assert!(archived.is_archived);

        let touched = svc.touch_visit(&owner, &b.bookmark_id).await?;
        assert_eq!(touched.visit_count, 1);
        assert!(touched.last_visited_at.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn list_applies_default_archive_filter() -> AppResult<()> {
        let (_dir, svc, owner) = build_service();
        let a = svc
            .add(&owner, sample_request("https://a.example", "A"))
            .await?;
        let mut b_req = sample_request("https://b.example", "B");
        b_req.is_archived = true;
        let _b = svc.add(&owner, b_req).await?;

        let rows = svc.list(&owner, Default::default()).await.unwrap();
        // Archive filter defaults to `Some(false)` — only `a` shows.
        let ids: Vec<&str> = rows.iter().map(|r| r.bookmark_id.as_str()).collect();
        assert_eq!(ids, vec![a.bookmark_id.as_str()]);
        Ok(())
    }

    #[tokio::test]
    async fn list_rejects_oversized_limit() -> AppResult<()> {
        let (_dir, svc, owner) = build_service();
        let mut filter = LinkBookmarkListFilter::default();
        filter.limit = Some(MAX_LIST_LIMIT + 1);
        assert!(svc.list(&owner, filter).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn search_finds_by_title_and_returns_limit() -> AppResult<()> {
        let (_dir, svc, owner) = build_service();
        svc.add(&owner, sample_request("https://rust-lang.org", "Rust home"))
            .await
            .unwrap();
        // Use a custom request without the "Rust" tag so the second
        // bookmark does not match the search needle via its tags.
        let mut other = sample_request("https://crates.io", "Crates");
        other.tags = vec!["packages".into()];
        svc.add(&owner, other).await.unwrap();

        let hits = svc
            .search(
                &owner,
                LinkBookmarkSearchQuery {
                    needle: "rust".into(),
                    folder: None,
                    limit: Some(10),
                },
            )
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].title.contains("Rust"));
        Ok(())
    }

    #[tokio::test]
    async fn tags_returns_lowercased_counts() -> AppResult<()> {
        let (_dir, svc, owner) = build_service();
        svc.add(&owner, sample_request("https://a.example", "A"))
            .await
            .unwrap();
        svc.add(&owner, sample_request("https://b.example", "B"))
            .await
            .unwrap();
        let tags = svc.tags(&owner).await.unwrap();
        assert!(!tags.is_empty());
        assert!(tags.iter().any(|t| t.tag == "rust" && t.count >= 1));
        Ok(())
    }

    #[tokio::test]
    async fn count_returns_totals() -> AppResult<()> {
        let (_dir, svc, owner) = build_service();
        let a = svc
            .add(&owner, sample_request("https://a.example", "A"))
            .await
            .unwrap();
        let _b = svc.add(&owner, sample_request("https://b.example", "B")).await.unwrap();
        svc.set_pinned(&owner, &a.bookmark_id, true).await.unwrap();
        let c = svc.count(&owner).await.unwrap();
        assert_eq!(c.total, 2);
        assert_eq!(c.pinned, 1);
        assert_eq!(c.archived, 0);
        Ok(())
    }

    #[tokio::test]
    async fn dispatch_routes_to_service() -> AppResult<()> {
        let (_dir, svc, owner) = build_service();
        let req = sample_request("https://example.com", "x");
        let v = dispatch(
            Arc::new(svc.clone()),
            A3chatRpcMethod::LINK_BOOKMARK_ADD,
            &owner,
            serde_json::to_value(&req).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(v["url"], "https://example.com");
        Ok(())
    }
}