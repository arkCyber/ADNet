//! Pre-serve / pre-write policy decision engine.
//!
//! The [`ModerationPolicy`] is the layer the gateway calls between
//! an inbound request and the underlying blob store. It is **the
//! only code path that can turn a `GET /ipfs/<cid>` into a 451**.
//!
//! ## Decision flow
//!
//! ```text
//!                ┌─────────────────────────────┐
//!                │      ModerationPolicy       │
//!                │                             │
//!   check_read   │  1. blocklist.is_blocked?  │── yes ─▶ Deny(reason)
//!       │        │  2. deny_by_default?        │── yes ─▶ Deny(default)
//!       │        │  3. classifier hooks (opt)  │── yes ─▶ Deny(classifier)
//!       │        │  4. otherwise               │── no  ─▶ Allow
//!       │        └─────────────────────────────┘
//!       ▼
//!    blob store
//! ```
//!
//! The gateway asks the policy **before** touching the blob store on
//! reads and before accepting bytes on writes (`/api/v0/dag/put`,
//! `/api/v0/block/put`, `/api/v0/pin/add`).
//!
//! ## Default-deny mode
//!
//! Operators can flip [`ModerationPolicy::deny_by_default`] to `true`
//! during a crisis (e.g. a known-bad actor flood) to refuse every
//! read until the blocklist is reviewed. The CLI exposes this under
//! `a3net moderation defend-on` / `defend-off`.

use std::sync::Arc;

use a3net_types::ContentHash;
use parking_lot::RwLock;

use crate::blocklist::{Blocklist, BlocklistEntry};

/// The outcome of a policy check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDecision {
    /// Whether the operation is allowed.
    pub kind: PolicyDecisionKind,
    /// Human-readable reason for the decision. Always set when
    /// `kind != Allow`.
    pub reason: String,
    /// When the block was an external feed entry, the originating
    /// entry is returned so the gateway can attach it to the audit
    /// log.
    pub source_entry: Option<BlocklistEntry>,
    /// When the decision is `Allow`, the optional `recommendation`
    /// string is for ops tooling to log (e.g. "default allow"). Kept
    /// on `Allow` too so the audit log has a single shape.
    pub recommendation: Option<String>,
}

impl PolicyDecision {
    /// Quick constructor for the allow path.
    pub fn allow() -> Self {
        Self {
            kind: PolicyDecisionKind::Allow,
            reason: String::new(),
            source_entry: None,
            recommendation: None,
        }
    }

    /// Quick constructor for the deny path.
    pub fn deny(reason: impl Into<String>) -> Self {
        Self {
            kind: PolicyDecisionKind::Deny,
            reason: reason.into(),
            source_entry: None,
            recommendation: None,
        }
    }

    /// `true` when the request may proceed.
    pub fn is_allowed(&self) -> bool {
        matches!(self.kind, PolicyDecisionKind::Allow)
    }
}

/// Why the policy denied (or allowed) a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyDecisionKind {
    /// Allowed.
    Allow,
    /// Denied.
    Deny,
}

/// Optional classifier hook. The moderation crate ships with the
/// blocklist-only path; classifiers (NSFW, PhotoDNA, …) plug in
/// here. The hook returns `Some(reason)` to deny and `None` to pass.
pub type ClassifierHook = Arc<dyn Fn(&ContentHash) -> Option<String> + Send + Sync>;

/// The full moderation policy.
pub struct ModerationPolicy {
    blocklist: Arc<Blocklist>,
    deny_by_default: RwLock<bool>,
    classifiers: RwLock<Vec<ClassifierHook>>,
}

impl std::fmt::Debug for ModerationPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModerationPolicy")
            .field("blocklist", &self.blocklist)
            .field("deny_by_default", &*self.deny_by_default.read())
            .field("classifiers_count", &self.classifiers.read().len())
            .finish()
    }
}

impl ModerationPolicy {
    /// Construct a policy backed by `blocklist`.
    pub fn new(blocklist: Arc<Blocklist>) -> Self {
        Self {
            blocklist,
            deny_by_default: RwLock::new(false),
            classifiers: RwLock::new(Vec::new()),
        }
    }

    /// Construct a policy with no blocklist (off-by-default).
    /// `is_blocked` always returns `false` in this mode.
    pub fn permissive() -> Self {
        Self::new(Arc::new(Blocklist::in_memory()))
    }

    /// Return the blocklist underneath.
    pub fn blocklist(&self) -> &Arc<Blocklist> {
        &self.blocklist
    }

    /// Flip the deny-by-default switch. When `true`, every read
    /// request is denied unless the blocklist explicitly whitelists
    /// the hash (a `revoked` entry).
    pub fn set_deny_by_default(&self, on: bool) {
        *self.deny_by_default.write() = on;
    }

    /// Current deny-by-default state.
    pub fn deny_by_default(&self) -> bool {
        *self.deny_by_default.read()
    }

    /// Register a classifier hook. Called in registration order; the
    /// first hook that returns `Some(reason)` wins.
    pub fn register_classifier(&self, hook: ClassifierHook) {
        self.classifiers.write().push(hook);
    }

    /// Number of registered classifier hooks — used by the CLI
    /// `a3net moderation status` command.
    pub fn classifier_count(&self) -> usize {
        self.classifiers.read().len()
    }

    /// Policy check for a read request (`GET /ipfs/<cid>` and
    /// friends). When the blocklist denies, the originating entry
    /// is attached to the decision so the gateway can render the
    /// `451` body with provenance.
    pub fn check_read(&self, hash: &ContentHash) -> PolicyDecision {
        if let Some(entry) = self.blocklist.lookup_active(hash) {
            let reason_slug = reason_slug(entry.reason);
            let source_slug = source_slug(entry.source);
            return PolicyDecision {
                kind: PolicyDecisionKind::Deny,
                reason: format!(
                    "content blocked by policy: reason={} source={} evidence={}",
                    reason_slug, source_slug, entry.evidence
                ),
                source_entry: Some(entry),
                recommendation: None,
            };
        }

        if *self.deny_by_default.read() {
            return PolicyDecision::deny(
                "default-deny enabled: content not on the allow-list",
            );
        }

        self.run_classifiers(hash)
    }

    /// Policy check for a write request (`POST /api/v0/dag/put`,
    /// `/api/v0/block/put`, `/api/v0/pin/add`). The blocklist
    /// takes precedence; classifiers fire **only on write** if all
    /// the existing checks pass — that way an admin can `pin add`
    /// a known-blacklisted hash from an `Operator` role to clean
    /// up after a takedown.
    pub fn check_write(&self, hash: &ContentHash) -> PolicyDecision {
        if let Some(entry) = self.blocklist.lookup_active(hash) {
            let reason_slug = reason_slug(entry.reason);
            let source_slug = source_slug(entry.source);
            return PolicyDecision {
                kind: PolicyDecisionKind::Deny,
                reason: format!(
                    "write blocked by policy: reason={} source={} evidence={}",
                    reason_slug, source_slug, entry.evidence
                ),
                source_entry: Some(entry),
                recommendation: None,
            };
        }
        self.run_classifiers(hash)
    }

    fn run_classifiers(&self, hash: &ContentHash) -> PolicyDecision {
        let classifiers = self.classifiers.read();
        for hook in classifiers.iter() {
            if let Some(reason) = hook(hash) {
                return PolicyDecision {
                    kind: PolicyDecisionKind::Deny,
                    reason: format!("classifier denied: {}", reason),
                    source_entry: None,
                    recommendation: None,
                };
            }
        }
        PolicyDecision::allow()
    }
}

pub(crate) fn reason_slug(reason: crate::blocklist::TakedownReason) -> &'static str {
    use crate::blocklist::TakedownReason;
    match reason {
        TakedownReason::Csam => "csam",
        TakedownReason::Copyright => "copyright",
        TakedownReason::Terrorism => "terrorism",
        TakedownReason::Ncii => "ncii",
        TakedownReason::Doxxing => "doxxing",
        TakedownReason::LegalOrder => "legal_order",
        TakedownReason::Malware => "malware",
        TakedownReason::TermsOfService => "tos",
        TakedownReason::Other => "other",
    }
}

fn source_slug(source: crate::blocklist::BlocklistSource) -> &'static str {
    use crate::blocklist::BlocklistSource;
    match source {
        BlocklistSource::Ncmec => "ncmec",
        BlocklistSource::Iwf => "iwf",
        BlocklistSource::Interpol => "interpol",
        BlocklistSource::Operator => "operator",
        BlocklistSource::TrustedFeed => "trusted_feed",
        BlocklistSource::LegalOrder => "legal_order",
        BlocklistSource::Governance => "governance",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn hash(b: &[u8]) -> ContentHash {
        ContentHash::from_bytes(b)
    }

    #[test]
    fn empty_policy_allows_everything() {
        let p = ModerationPolicy::permissive();
        assert!(p.check_read(&hash(b"x")).is_allowed());
        assert!(p.check_write(&hash(b"x")).is_allowed());
    }

    #[test]
    fn blocklisted_hash_is_denied() {
        let dir = tempdir().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
        let p = ModerationPolicy::new(bl.clone());
        let h = hash(b"x");
        bl.add(
            h.clone(),
            crate::blocklist::TakedownReason::Csam,
            crate::blocklist::BlocklistSource::Ncmec,
            "case 12",
            "alice",
            None,
            "",
        )
        .unwrap();
        let d = p.check_read(&h);
        assert!(!d.is_allowed());
        assert!(d.reason.contains("csam"));
        assert!(d.source_entry.is_some());
    }

    #[test]
    fn deny_by_default_blocks_unlisted() {
        let dir = tempdir().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
        let p = ModerationPolicy::new(bl);
        p.set_deny_by_default(true);
        assert!(!p.check_read(&hash(b"x")).is_allowed());
    }

    #[test]
    fn classifier_can_deny() {
        let dir = tempdir().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
        let p = ModerationPolicy::new(bl);
        let target = Arc::new(hash(b"definitely-bad"));
        let target_for_hook = target.clone();
        let denyx: ClassifierHook = Arc::new(move |h: &ContentHash| {
            if h == target_for_hook.as_ref() {
                Some("nsfw_score>=0.95".to_string())
            } else {
                None
            }
        });
        p.register_classifier(denyx);
        let d = p.check_read(&hash(b"good"));
        assert!(d.is_allowed(), "non-matching hash is allowed");
        let d2 = p.check_read(target.as_ref());
        assert!(!d2.is_allowed());
        assert!(d2.reason.contains("nsfw"));
    }

    #[test]
    fn revoke_unblocks() {
        let dir = tempdir().unwrap();
        let bl = Arc::new(Blocklist::load(dir.path()).unwrap());
        let p = ModerationPolicy::new(bl.clone());
        let h = hash(b"x");
        let id = bl
            .add(
                h.clone(),
                crate::blocklist::TakedownReason::Other,
                crate::blocklist::BlocklistSource::Operator,
                "",
                "alice",
                None,
                "",
            )
            .unwrap();
        assert!(!p.check_read(&h).is_allowed());
        bl.revoke(id).unwrap();
        assert!(p.check_read(&h).is_allowed());
    }
}
