//! `a3chat` content moderation service.
//!
//! Bridges the chat message-send pipeline onto the audit-friendly
//! [`a3net_moderation::policy::ModerationPolicy`], so every outgoing
//! message (and every attachment hash) is checked against the local
//! blocklist **before** it is enqueued for delivery.
//!
//! # Why `a3net-moderation`
//!
//! Reusing `a3net-moderation` gives `a3chat`:
//!
//! * A **persistent, audit-friendly** blocklist (`blocklist.json` on
//!   disk, line-by-line `#[serde]` so the operator can `cat` it).
//! * A pluggable classifier hook chain — a custom URL-filter,
//!   profanity matcher, or ML hook can be wired in at runtime without
//!   recompiling `a3chat`.
//! * The **deny-by-default** posture (configurable) so a misconfigured
//!   chat client cannot bypass the policy.
//!
//! # Surface
//!
//! Four RPC methods (see [`METHODS`]) back the lifecycle:
//!
//! | RPC                                  | Purpose                                  |
//! |--------------------------------------|------------------------------------------|
//! | `a3chat.moderation.check_content`    | Pre-flight gate for outgoing messages    |
//! | `a3chat.moderation.check_attachment` | Pre-flight gate for attachment hashes    |
//! | `a3chat.moderation.list_blocked`     | List currently-active blocklist entries  |
//! | `a3chat.moderation.set_deny_default` | Toggle deny-by-default at runtime        |

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use a3net_moderation::blocklist::{Blocklist, BlocklistEntry, BlocklistStats};
use a3net_moderation::policy::{
    ClassifierHook, ModerationPolicy, PolicyDecision, PolicyDecisionKind,
};
use a3net_types::content::ContentHash;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use a3chat_core::error::A3chatError;
use a3chat_core::id::UserId;

use crate::error::{AppError, AppResult};

/// RPC method-name constants owned by this module.
pub const METHODS: &[&str] = &[
    "a3chat.moderation.check_content",
    "a3chat.moderation.check_attachment",
    "a3chat.moderation.list_blocked",
    "a3chat.moderation.set_deny_default",
];

/// Configuration for [`ModerationService`].
#[derive(Debug, Clone)]
pub struct ModerationConfig {
    /// Directory where `blocklist.json` lives.
    pub data_dir: PathBuf,
    /// Initial deny-by-default posture.
    pub deny_by_default: bool,
}

impl ModerationConfig {
    /// Build a config under `<base>/moderation`.
    pub fn under_base(base: &Path) -> Self {
        Self {
            data_dir: base.join("moderation"),
            deny_by_default: false,
        }
    }
}

/// Moderation service. Cheap to clone.
#[derive(Clone)]
pub struct ModerationService {
    inner: Arc<ModerationInner>,
}

struct ModerationInner {
    policy: ModerationPolicy,
    blocklist: Arc<Blocklist>,
    /// Toggle is read/write through this mutex.
    deny_default_lock: Mutex<bool>,
}

impl ModerationService {
    /// Open (or create) the service on disk.
    pub fn open(cfg: &ModerationConfig) -> AppResult<Self> {
        std::fs::create_dir_all(&cfg.data_dir)?;
        let blocklist = Arc::new(
            Blocklist::load(&cfg.data_dir)
                .map_err(|e| AppError::Storage(format!("Blocklist::load: {e}")))?,
        );
        let policy = ModerationPolicy::new(blocklist.clone());
        policy.set_deny_by_default(cfg.deny_by_default);
        Ok(Self {
            inner: Arc::new(ModerationInner {
                policy,
                blocklist,
                deny_default_lock: Mutex::new(cfg.deny_by_default),
            }),
        })
    }

    /// Open an in-memory service (test helper).
    pub fn open_in_memory(deny_by_default: bool) -> Self {
        let blocklist = Arc::new(Blocklist::in_memory());
        let policy = ModerationPolicy::new(blocklist.clone());
        policy.set_deny_by_default(deny_by_default);
        Self {
            inner: Arc::new(ModerationInner {
                policy,
                blocklist,
                deny_default_lock: Mutex::new(deny_by_default),
            }),
        }
    }

    /// Register a classifier hook (e.g. profanity matcher, URL filter).
    pub fn register_classifier(&self, hook: ClassifierHook) {
        self.inner.policy.register_classifier(hook);
    }

    /// Toggle the deny-by-default posture at runtime.
    pub fn set_deny_by_default(&self, on: bool) {
        self.inner.policy.set_deny_by_default(on);
        *self.inner.deny_default_lock.lock() = on;
    }

    /// Whether the deny-by-default posture is active.
    pub fn deny_by_default(&self) -> bool {
        *self.inner.deny_default_lock.lock()
    }

    /// Block a content hash (administrative API).
    pub fn block_hash(
        &self,
        hash: &ContentHash,
        reason: impl Into<String>,
    ) -> AppResult<u64> {
        self.inner
            .blocklist
            .add(
                hash.clone(),
                a3net_moderation::blocklist::TakedownReason::TermsOfService,
                a3net_moderation::blocklist::BlocklistSource::Operator,
                reason.into(),
                "a3chat-operator",
                None,
                "",
            )
            .map_err(|e| AppError::Storage(format!("blocklist.add: {e}")))
    }

    /// Run the policy on a chat message body. Returns the decision.
    pub fn check_content(&self, owner: &UserId, text: &str) -> PolicyDecision {
        // For chat content, we hash the text and check the blocklist —
        // keeping the chat-specific policy extension point open
        // (classification hooks).
        let mut decision = self.inner.policy.check_write(&blake3_of(text));
        // For chat, "deny" a message must surface as Forbidden upstream.
        if !decision.is_allowed() {
            decision.reason = format!(
                "{}: chat message from {} blocked: {}",
                decision.reason,
                owner.as_str(),
                truncate(text, 80),
            );
        }
        decision
    }

    /// Run the policy on an attachment content hash.
    pub fn check_attachment(&self, hash: &ContentHash) -> PolicyDecision {
        self.inner.policy.check_write(hash)
    }

    /// List active blocklist entries (for operators).
    pub fn list_blocked(&self) -> Vec<BlocklistEntry> {
        self.inner.blocklist.list_active()
    }

    /// Statistics on the blocklist.
    pub fn stats(&self) -> BlocklistStats {
        self.inner.blocklist.stats()
    }

    /// Policy decision → serialisable outcome.
    pub fn decision_to_outcome(
        &self,
        d: PolicyDecision,
    ) -> ModerationOutcome {
        ModerationOutcome {
            allowed: d.is_allowed(),
            kind: match d.kind {
                PolicyDecisionKind::Allow => "allow".into(),
                PolicyDecisionKind::Deny => "deny".into(),
            },
            reason: if d.reason.is_empty() {
                None
            } else {
                Some(d.reason)
            },
        }
    }
}

// ---------- Helpers ---------------------------------------------------

fn blake3_of(s: &str) -> ContentHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(s.as_bytes());
    let digest = hasher.finalize();
    // `ContentHash` stores the *lowercase hex* of the BLAKE3 digest;
    // we wrap the digest directly to avoid a second hashing pass.
    // (`ContentHash::from_bytes` would re-hash the 32 raw digest
    // bytes, giving us `blake3(blake3(s))`, which is what the
    // previous implementation did and broke `block_hash` matching.)
    ContentHash::from_hex(digest.to_hex().as_str())
        .expect("blake3 hex output is always 64 lowercase hex chars")
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}

// ---------- DTOs ------------------------------------------------------

/// Outcome of a moderation check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModerationOutcome {
    pub allowed: bool,
    pub kind: String,
    pub reason: Option<String>,
}

/// Result of `check_content`/`check_attachment` over RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub outcome: ModerationOutcome,
}

// ---------- RPC dispatch ---------------------------------------------

/// Top-level dispatcher for any RPC method starting with `a3chat.moderation.`.
pub async fn dispatch(
    svc: Arc<ModerationService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    match method {
        "a3chat.moderation.check_content" => {
            let text = params
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("missing 'text'".into()))?;
            let decision = svc.check_content(owner, text);
            if !decision.is_allowed() {
                return Err(A3chatError::PermissionDenied(
                    if decision.reason.is_empty() {
                        "denied".into()
                    } else {
                        decision.reason.clone()
                    },
                ));
            }
            let outcome = svc.decision_to_outcome(decision);
            serde_json::to_value(CheckResult { outcome })
                .map_err(A3chatError::from)
        }
        "a3chat.moderation.check_attachment" => {
            let hash_hex = params
                .get("hash")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("missing 'hash'".into()))?;
            let hash = ContentHash::from_hex(hash_hex)
                .map_err(|e| A3chatError::InvalidInput(format!("bad hash: {e}")))?;
            let decision = svc.check_attachment(&hash);
            let outcome = svc.decision_to_outcome(decision);
            serde_json::to_value(CheckResult { outcome })
                .map_err(A3chatError::from)
        }
        "a3chat.moderation.list_blocked" => {
            let entries = svc.list_blocked();
            serde_json::to_value(entries).map_err(A3chatError::from)
        }
        "a3chat.moderation.set_deny_default" => {
            let on = params
                .get("on")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| A3chatError::InvalidInput("missing 'on' bool".into()))?;
            svc.set_deny_by_default(on);
            Ok(serde_json::json!({"denyByDefault": on}))
        }
        "a3chat.moderation.stats" => {
            serde_json::to_value(svc.stats()).map_err(A3chatError::from)
        }
        m => Err(A3chatError::InvalidInput(format!(
            "unknown moderation method: {m}"
        ))),
    }
}

// ---------- Tests -----------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_moderation::blocklist::{BlocklistSource, TakedownReason};

    fn svc() -> ModerationService {
        ModerationService::open_in_memory(false)
    }

    #[test]
    fn check_content_allows_clean_text() {
        let s = svc();
        let d = s.check_content(&UserId::new("alice"), "hello, world!");
        assert!(d.is_allowed());
    }

    #[test]
    fn list_blocked_is_initially_empty() {
        let s = svc();
        assert_eq!(s.list_blocked().len(), 0);
    }

    #[test]
    fn set_deny_default_toggles() {
        let s = svc();
        assert!(!s.deny_by_default());
        s.set_deny_by_default(true);
        assert!(s.deny_by_default());
        s.set_deny_by_default(false);
        assert!(!s.deny_by_default());
    }

    #[test]
    fn blocklist_add_then_list() {
        let s = svc();
        let hash = blake3_of("forbidden content");
        let id = s
            .block_hash(&hash, "test takedown")
            .expect("block must succeed");
        assert!(id > 0);
        let d = s.check_attachment(&hash);
        assert!(!d.is_allowed());
        assert!(s.list_blocked().len() >= 1);
    }

    #[test]
    fn blocklist_blocks_only_matching_hash() {
        // `in_memory()` is read-only — its path is "<memory>" and
        // `fs::create_dir_all` for a literal name fails. So we
        // build a tempdir-backed `ModerationService` here.
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = ModerationConfig {
            data_dir: tmp.path().to_path_buf(),
            deny_by_default: false,
        };
        let s = ModerationService::open(&cfg).expect("open moderation");
        let blocked = blake3_of("explicit content A");
        let allowed = blake3_of("innocent content B");
        s.block_hash(&blocked, "reason").expect("block must succeed");
        assert!(!s.check_attachment(&blocked).is_allowed());
        assert!(s.check_attachment(&allowed).is_allowed());
    }

    #[test]
    fn dispatch_check_content_allow() {
        let s = Arc::new(svc());
        let owner = UserId::new("alice");
        let v = futures::executor::block_on(dispatch(
            s.clone(),
            "a3chat.moderation.check_content",
            &owner,
            serde_json::json!({"text":"hi"}),
        ))
        .unwrap();
        assert_eq!(v["outcome"]["allowed"], true);
    }

    #[tokio::test]
    async fn dispatch_unknown_method_errors() {
        let s = Arc::new(svc());
        let owner = UserId::new("alice");
        let err = dispatch(
            s.clone(),
            "a3chat.moderation.no_such_method",
            &owner,
            serde_json::json!({}),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, A3chatError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn dispatch_missing_field_errors() {
        let s = Arc::new(svc());
        let owner = UserId::new("alice");
        let err = dispatch(
            s.clone(),
            "a3chat.moderation.check_content",
            &owner,
            serde_json::json!({}),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, A3chatError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn dispatch_set_deny_default_round_trip() {
        let s = Arc::new(svc());
        let owner = UserId::new("alice");
        let v = dispatch(
            s.clone(),
            "a3chat.moderation.set_deny_default",
            &owner,
            serde_json::json!({"on": true}),
        )
        .await
        .unwrap();
        assert_eq!(v["denyByDefault"], true);
        assert!(s.deny_by_default());
    }

    #[tokio::test]
    async fn dispatch_list_blocked_returns_array() {
        let s = Arc::new(svc());
        let owner = UserId::new("alice");
        let v = dispatch(
            s.clone(),
            "a3chat.moderation.list_blocked",
            &owner,
            serde_json::json!({}),
        )
        .await
        .unwrap();
        assert!(v.is_array());
        assert_eq!(v.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn dispatch_check_attachment_allows_clean() {
        let s = Arc::new(svc());
        let owner = UserId::new("alice");
        let hash_hex = hex::encode(blake3::hash(b"some-attachment").as_bytes());
        let v = dispatch(
            s.clone(),
            "a3chat.moderation.check_attachment",
            &owner,
            serde_json::json!({"hash": hash_hex}),
        )
        .await
        .unwrap();
        assert_eq!(v["outcome"]["allowed"], true);
    }

    #[tokio::test]
    async fn dispatch_check_attachment_rejects_bad_hash() {
        let s = Arc::new(svc());
        let owner = UserId::new("alice");
        let err = dispatch(
            s.clone(),
            "a3chat.moderation.check_attachment",
            &owner,
            serde_json::json!({"hash": "not-hex"}),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, A3chatError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn dispatch_method_count_matches_methods_const() {
        assert_eq!(METHODS.len(), 4);
        assert!(METHODS.contains(&"a3chat.moderation.check_content"));
    }

    #[test]
    fn blocklist_struct_import_works() {
        // Smoke-test: the upstream `BlocklistEntry` is reachable from
        // our public surface without re-declaring it.
        let _ = BlocklistSource::Operator;
        let _ = TakedownReason::TermsOfService;
    }
}