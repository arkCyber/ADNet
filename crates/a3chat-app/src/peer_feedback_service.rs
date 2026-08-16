//! `PeerFeedbackService` — the chat-facing surface for peer reputation
//!
//! Bridges two layers that have lived in parallel crates:
//!
//! - [`a3net_chatstore::ChatTrustStore`] — the per-user-per-target
//!   trust table (owned by the chat layer).
//! - [`a3net_reputation`] — the global `PeerScoreTable` shared
//!   across gossip / bitswap / pairing / chat.
//!
//! Operations:
//!
//! - [`set_trust`](Self::set_trust) — upsert a trust record; if a
//!   reputation reporter was wired into the chatstore, the
//!   `ChatTrustSet` event is emitted automatically (see
//!   `ChatTrustStore::with_reputation`).
//! - [`file_report`](Self::file_report) — emit a `ChatTrustReport`
//!   into the global PeerScore (no chatstore table — reports are
//!   ephemeral reputation signals).
//! - [`fused_score`](Self::fused_score) — query the fused
//!   `(chat_trust × global_score)` value used by the chat layer to
//!   decide whether to relay, push notifications, or accept invites
//!   from a peer.
//! - [`list_trust`](Self::list_trust) — list all trust judgements
//!   owned by a user (most-trusted first).
//!
//! RPC surface:
//!
//! - `a3chat.peerfeedback.set_trust`
//! - `a3chat.peerfeedback.file_report`
//! - `a3chat.peerfeedback.fused_score`
//! - `a3chat.peerfeedback.list_trust`
//! - `a3chat.peerfeedback.clear_trust`

use std::sync::Arc;

use a3chat_core::error::A3chatError;
use a3chat_core::id::UserId;
use a3net_chatstore::ChatTrustRecord;
use a3net_reputation::{
    ReputationReporter, ReportKind, TrustFusion, TrustLevel, TrustSignal,
};
use a3net_reputation::reporter::ChatSignal;
use a3net_types::NodeId;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::storage::ChatStorage;

/// Default refusal threshold on the fused `[-1, +1]` score.
///
/// Anything `<= -0.5` is treated as "refuse to relay". This matches
/// the default `TrustFusion` weighting where a `Blocked` (level -3)
/// chat signal alone lands at `-0.7`.
pub const DEFAULT_REFUSAL_THRESHOLD: f64 = -0.5;

/// Cheap-to-clone handle to the peer-feedback service. All RPC
/// handlers go through [`dispatch`].
#[derive(Clone)]
pub struct PeerFeedbackService {
    inner: Arc<tokio::sync::RwLock<PeerFeedbackInner>>,
}

struct PeerFeedbackInner {
    storage: ChatStorage,
    /// Optional reporter. When `None` (test harnesses without a
    /// full reputation stack), `file_report` and `fused_score`
    /// become no-ops returning neutral values.
    reporter: Option<ReputationReporter>,
    fusion: TrustFusion,
    refusal_threshold: f64,
}

impl PeerFeedbackService {
    /// Build a service backed by the same [`ChatStorage`] as the
    /// other services (so it shares per-user SQLite connections).
    pub fn new(storage: ChatStorage) -> Self {
        Self {
            inner: Arc::new(tokio::sync::RwLock::new(PeerFeedbackInner {
                storage,
                reporter: None,
                fusion: TrustFusion::default(),
                refusal_threshold: DEFAULT_REFUSAL_THRESHOLD,
            })),
        }
    }

    /// Attach a reputation reporter so [`file_report`](Self::file_report)
    /// and [`fused_score`](Self::fused_score) can speak to the
    /// global PeerScore. **Also** installs the reporter on every
    /// per-user `ChatTrustStore` built from this point on, so that
    /// subsequent trust writes also feed the global score.
    pub async fn with_reporter(&self, reporter: ReputationReporter) {
        let mut inner = self.inner.write().await;
        inner.reporter = Some(reporter);
    }

    /// Override the fused-score refusal threshold.
    pub async fn with_refusal_threshold(&self, t: f64) {
        let mut inner = self.inner.write().await;
        inner.refusal_threshold = t.clamp(-1.0, 1.0);
    }

    /// Borrow the storage handle (used by `A3chatApp` to build
    /// per-user trust stores during bootstrap).
    pub fn storage(&self) -> ChatStorage {
        // RwLock::blocking_read isn't available without a runtime,
        // so we expose a snapshot via blocking_read using the
        // tokio runtime's blocking feature. Callers that need
        // an async-aware snapshot should use [`storage_async`].
        self.inner.blocking_read().storage.clone()
    }

    /// Async-aware storage snapshot for use from RPC handlers.
    pub async fn storage_async(&self) -> ChatStorage {
        self.inner.read().await.storage.clone()
    }

    /// Borrow the reporter, if one is attached.
    pub async fn reporter(&self) -> Option<ReputationReporter> {
        self.inner.read().await.reporter.clone()
    }

    // ── Trust writes ─────────────────────────────────────────────

    /// Set the local user's trust judgement for `target_user_id`.
    /// The trust record is persisted in `chat_trust` and, if a
    /// reporter is attached, a `ChatTrustSet` event is emitted into
    /// the global PeerScore (see `ChatTrustStore::set`).
    pub async fn set_trust(
        &self,
        owner: &UserId,
        target_user_id: &str,
        level: TrustLevel,
        notes: Option<String>,
    ) -> AppResult<ChatTrustRecord> {
        if owner.as_str() == target_user_id {
            return Err(AppError::Domain(
                "owner and target must differ".into(),
            ));
        }
        let storage = self.inner.read().await.storage.clone();
        let store = storage.trust_store(owner).await?;
        // Make sure subsequent calls into this store emit reputation
        // events. We do this lazily (cheap; the reporter is an Arc).
        if let Some(rep) = self.inner.read().await.reporter.clone() {
            store.with_reputation(rep);
        }
        store
            .set(owner.as_str(), target_user_id, level.as_i8(), notes)
            .await
            .map_err(AppError::from)
    }

    /// Clear a previously-set trust judgement. Does **not** emit a
    /// reputation event — clearing returns the user to neutral and
    /// the global PeerScore already reflects whatever the most
    /// recent signal was.
    pub async fn clear_trust(
        &self,
        owner: &UserId,
        target_user_id: &str,
    ) -> AppResult<bool> {
        let storage = self.inner.read().await.storage.clone();
        let store = storage.trust_store(owner).await?;
        store
            .clear(owner.as_str(), target_user_id)
            .await
            .map_err(AppError::from)
    }

    // ── Reports ──────────────────────────────────────────────────

    /// File a peer-report. Reports are ephemeral reputation signals
    /// (no chatstore table); they land directly in the global
    /// PeerScore via [`ChatSignal::report`].
    ///
    /// Returns `Err` if no reporter is attached — callers in test
    /// harnesses without a full reputation stack should not call
    /// this method.
    pub async fn file_report(
        &self,
        owner: &UserId,
        target_user_id: &str,
        kind: ReportKind,
    ) -> AppResult<()> {
        let rep = self.inner.read().await.reporter.clone().ok_or_else(|| {
            AppError::Internal(
                "PeerFeedbackService has no reputation reporter; cannot file_report"
                    .into(),
            )
        })?;
        let by_user = user_to_u64(owner.as_str());
        let target_node = user_to_node(target_user_id);
        ChatSignal(&rep).report(target_node, by_user, kind);
        Ok(())
    }

    // ── Queries ──────────────────────────────────────────────────

    /// Compute the fused `(chat_trust × global_score)` value used by
    /// the chat layer to decide whether to relay / push / accept
    /// invites from `target_user_id`. Returns [`FusedScore`] with
    /// both components so callers can show users "why".
    pub async fn fused_score(
        &self,
        owner: &UserId,
        target_user_id: &str,
    ) -> AppResult<FusedScore> {
        let storage = self.inner.read().await.storage.clone();
        let store = storage.trust_store(owner).await?;
        let record = store
            .get(owner.as_str(), target_user_id)
            .await
            .map_err(AppError::from)?;
        let target_node = user_to_node(target_user_id);
        let chat_signal = record.as_ref().map(|r| {
            TrustSignal::new(
                user_to_u64(owner.as_str()),
                target_node.clone(),
                r.trust_level(),
                r.notes.clone(),
            )
        });
        let inner = self.inner.read().await;
        let global_score = match inner.reporter.as_ref() {
            Some(rep) => rep.table().score(&target_node).unwrap_or(0.0),
            None => 0.0,
        };
        let fused = inner.fusion.fused(global_score, chat_signal.as_ref());
        Ok(FusedScore {
            target_user_id: target_user_id.to_string(),
            global_score,
            chat_level: record.as_ref().map(|r| r.level),
            fused,
            refusal_threshold: inner.refusal_threshold,
            should_refuse: inner.fusion.should_refuse(fused, inner.refusal_threshold),
        })
    }

    /// List every trust judgement owned by `owner`, most-trusted
    /// first.
    pub async fn list_trust(&self, owner: &UserId) -> AppResult<Vec<ChatTrustRecord>> {
        let storage = self.inner.read().await.storage.clone();
        let store = storage.trust_store(owner).await?;
        store
            .list_for_owner(owner.as_str())
            .await
            .map_err(AppError::from)
    }
}

/// Result of a fused-score query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FusedScore {
    pub target_user_id: String,
    /// Global PeerScore in `[-100, +100]`. `0.0` if no score has
    /// been recorded yet.
    pub global_score: f64,
    /// Per-user chat trust level in `[-3, +3]`, or `None` if no
    /// judgement has been made.
    pub chat_level: Option<i8>,
    /// Fused value in `[-1, +1]` — the single number the chat layer
    /// should threshold against.
    pub fused: f64,
    /// Threshold under which `should_refuse == true`.
    pub refusal_threshold: f64,
    pub should_refuse: bool,
}

// ── String→NodeId / u64 mappings (same scheme as a3net-chatstore) ────

fn user_to_u64(user_id: &str) -> u64 {
    let h = blake3::hash(user_id.as_bytes());
    let bytes = h.as_bytes();
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[..8]);
    u64::from_le_bytes(buf)
}

fn user_to_node(user_id: &str) -> NodeId {
    let h = blake3::hash(user_id.as_bytes());
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&h.as_bytes()[..32]);
    NodeId::from_bytes(&bytes)
        .unwrap_or_else(|_| NodeId::from_bytes(&[0u8; 32]).expect("zero NodeId is valid"))
}

// ── JSON dispatch ────────────────────────────────────────────────────

/// Entry point used by [`crate::app::A3chatApp::dispatch`].
pub async fn dispatch(
    svc: Arc<PeerFeedbackService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    match method {
        "a3chat.peerfeedback.set_trust" => {
            let target = params
                .get("targetUserId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("missing targetUserId".into()))?;
            let level_i = params
                .get("level")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| A3chatError::InvalidInput("missing level".into()))?;
            let level = TrustLevel::from_i8(level_i as i8);
            let notes = params
                .get("notes")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let rec = svc
                .set_trust(owner, target, level, notes)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(rec).map_err(A3chatError::from)
        }
        "a3chat.peerfeedback.clear_trust" => {
            let target = params
                .get("targetUserId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("missing targetUserId".into()))?;
            let removed = svc
                .clear_trust(owner, target)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({"cleared": removed}))
        }
        "a3chat.peerfeedback.file_report" => {
            let target = params
                .get("targetUserId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("missing targetUserId".into()))?;
            let kind_str = params
                .get("kind")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("missing kind".into()))?;
            let kind = parse_report_kind(kind_str)?;
            svc.file_report(owner, target, kind)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({"filed": true, "kind": kind_str}))
        }
        "a3chat.peerfeedback.fused_score" => {
            let target = params
                .get("targetUserId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("missing targetUserId".into()))?;
            let fs = svc
                .fused_score(owner, target)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(fs).map_err(A3chatError::from)
        }
        "a3chat.peerfeedback.list_trust" => {
            let recs = svc.list_trust(owner).await.map_err(A3chatError::from)?;
            serde_json::to_value(recs).map_err(A3chatError::from)
        }
        m => Err(A3chatError::InvalidInput(format!(
            "unknown peerfeedback method: {m}"
        ))),
    }
}

fn parse_report_kind(s: &str) -> Result<ReportKind, A3chatError> {
    match s {
        "spam" => Ok(ReportKind::Spam),
        "harassment" => Ok(ReportKind::Harassment),
        "impersonation" => Ok(ReportKind::Impersonation),
        "phishing" => Ok(ReportKind::Phishing),
        "other" => Ok(ReportKind::Other),
        other => Err(A3chatError::InvalidInput(format!(
            "unknown report kind: {other}"
        ))),
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use a3chat_core::id::generate_user_id;
    use crate::storage::StorageConfig;
    use a3net_reputation::PeerScoreTable;

    fn temp_storage() -> ChatStorage {
        let dir = tempfile::tempdir().unwrap();
        let owner = generate_user_id();
        ChatStorage::new(
            StorageConfig::new(dir.path().to_path_buf()),
            crate::keyring::E2eKeyring::new(owner),
        )
    }

    #[tokio::test]
    async fn set_and_query_trust_roundtrip() {
        let storage = temp_storage();
        let owner = generate_user_id();
        storage.init_user(&owner).await.unwrap();
        let svc = PeerFeedbackService::new(storage);
        let rec = svc
            .set_trust(
                &owner,
                "peer-alice",
                TrustLevel::Friend,
                Some("met at conf".into()),
            )
            .await
            .unwrap();
        assert_eq!(rec.level, 2);
        assert_eq!(rec.event_count, 1);
        let list = svc.list_trust(&owner).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].target_user_id, "peer-alice");
    }

    #[tokio::test]
    async fn cannot_trust_self() {
        let storage = temp_storage();
        let owner = generate_user_id();
        storage.init_user(&owner).await.unwrap();
        let svc = PeerFeedbackService::new(storage);
        let err = svc
            .set_trust(&owner, owner.as_str(), TrustLevel::Neutral, None)
            .await
            .unwrap_err();
        match err {
            AppError::Domain(_) => {}
            e => panic!("expected Domain error, got {e:?}"),
        }
    }

    #[tokio::test]
    async fn file_report_requires_reporter() {
        let storage = temp_storage();
        let owner = generate_user_id();
        let svc = PeerFeedbackService::new(storage);
        let err = svc
            .file_report(&owner, "peer-bob", ReportKind::Spam)
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Internal(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn file_report_emits_reputation_event() {
        let storage = temp_storage();
        let owner = generate_user_id();
        storage.init_user(&owner).await.unwrap();
        let reporter = ReputationReporter::in_memory(PeerScoreTable::new(
            a3net_reputation::ReputationParams::default(),
        ));
        let svc = PeerFeedbackService::new(storage);
        svc.with_reporter(reporter.clone()).await;
        svc.file_report(&owner, "peer-spammer", ReportKind::Spam)
            .await
            .unwrap();
        // Score must have dropped for the target's NodeId.
        let target = user_to_node("peer-spammer");
        let score = reporter.table().score(&target).unwrap_or(0.0);
        assert!(
            score < 0.0,
            "spam report should produce a negative score (got {score})"
        );
    }

    #[tokio::test]
    async fn set_trust_emits_reputation_event() {
        let storage = temp_storage();
        let owner = generate_user_id();
        storage.init_user(&owner).await.unwrap();
        let reporter = ReputationReporter::in_memory(PeerScoreTable::new(
            a3net_reputation::ReputationParams::default(),
        ));
        let svc = PeerFeedbackService::new(storage);
        svc.with_reporter(reporter.clone()).await;
        svc.set_trust(&owner, "peer-friend", TrustLevel::Trusted, None)
            .await
            .unwrap();
        // Score should be positive for the target's NodeId.
        let target = user_to_node("peer-friend");
        let score = reporter.table().score(&target).unwrap_or(0.0);
        assert!(
            score > 0.0,
            "trusted signal should produce a positive score (got {score})"
        );
    }

    #[tokio::test]
    async fn fused_score_with_blocked_refuses() {
        let storage = temp_storage();
        let owner = generate_user_id();
        storage.init_user(&owner).await.unwrap();
        let reporter = ReputationReporter::in_memory(PeerScoreTable::new(
            a3net_reputation::ReputationParams::default(),
        ));
        let svc = PeerFeedbackService::new(storage);
        svc.with_reporter(reporter).await;
        svc.set_trust(&owner, "peer-bad", TrustLevel::Blocked, None)
            .await
            .unwrap();
        let fs = svc.fused_score(&owner, "peer-bad").await.unwrap();
        assert!(fs.fused < 0.0, "fused={}", fs.fused);
        assert!(
            fs.should_refuse,
            "blocked level should drive should_refuse (threshold={})",
            fs.refusal_threshold
        );
    }

    #[tokio::test]
    async fn fused_score_with_neutral_does_not_refuse() {
        let storage = temp_storage();
        let owner = generate_user_id();
        storage.init_user(&owner).await.unwrap();
        let reporter = ReputationReporter::in_memory(PeerScoreTable::new(
            a3net_reputation::ReputationParams::default(),
        ));
        let svc = PeerFeedbackService::new(storage);
        svc.with_reporter(reporter).await;
        let fs = svc.fused_score(&owner, "peer-new").await.unwrap();
        assert_eq!(fs.chat_level, None);
        assert!(!fs.should_refuse, "no signals ⇒ no refusal");
    }

    #[tokio::test]
    async fn custom_refusal_threshold_honoured() {
        let storage = temp_storage();
        let owner = generate_user_id();
        storage.init_user(&owner).await.unwrap();
        let reporter = ReputationReporter::in_memory(PeerScoreTable::new(
            a3net_reputation::ReputationParams::default(),
        ));
        let svc = PeerFeedbackService::new(storage);
        svc.with_reporter(reporter).await;
        svc.with_refusal_threshold(-0.9).await;
        svc.set_trust(&owner, "peer-mild", TrustLevel::Caution, None)
            .await
            .unwrap();
        let fs = svc.fused_score(&owner, "peer-mild").await.unwrap();
        // Caution (-1) lands at -0.7 under default fusion — below -0.9?
        // No, so should_refuse must be false.
        assert!(
            !fs.should_refuse,
            "caution with -0.9 threshold should not refuse (fused={})",
            fs.fused
        );
    }

    #[tokio::test]
    async fn clear_trust_removes_record() {
        let storage = temp_storage();
        let owner = generate_user_id();
        storage.init_user(&owner).await.unwrap();
        let svc = PeerFeedbackService::new(storage);
        svc.set_trust(&owner, "peer-x", TrustLevel::Friend, None)
            .await
            .unwrap();
        assert_eq!(svc.list_trust(&owner).await.unwrap().len(), 1);
        let removed = svc.clear_trust(&owner, "peer-x").await.unwrap();
        assert!(removed);
        assert_eq!(svc.list_trust(&owner).await.unwrap().len(), 0);
    }

    #[test]
    fn parse_report_kind_accepts_known() {
        for s in ["spam", "harassment", "impersonation", "phishing", "other"] {
            assert!(parse_report_kind(s).is_ok(), "{s}");
        }
    }

    #[test]
    fn parse_report_kind_rejects_unknown() {
        assert!(parse_report_kind("bogus").is_err());
    }

    #[test]
    fn user_to_node_is_deterministic() {
        let a = user_to_node("alice");
        let b = user_to_node("alice");
        assert_eq!(a, b);
        let c = user_to_node("bob");
        assert_ne!(a, c);
    }
}