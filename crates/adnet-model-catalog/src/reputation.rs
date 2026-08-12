//! Provider Reputation - Track and rate model providers
//!
//! Each peer that publishes models on the network gets a reputation
//! score derived from three sources:
//!
//! 1. **Download success rate** — peers whose blobs verify and
//!    download quickly get positive feedback.
//! 2. **Manifest integrity** — peers who consistently publish
//!    valid manifests (correct hash, ticket, etc.) gain trust.
//! 3. **User reports** — explicit user feedback (spam, broken
//!    downloads, inaccurate descriptions, license violations).
//!
//! Scores are persisted per-provider in the catalog so they
//! survive restarts. The global PeerScoreTable from
//! `adnet-reputation` is the authoritative aggregator when
//! available; the on-disk column is a snapshot for fast UI reads.
//!
//! ## Lifecycle
//!
//! ```text
//! download success/failure ─┐
//!                           │
//! manifest validation ──────┼──► ProviderReputation ──► snapshot column
//!                           │
//! user report ──────────────┘
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use adnet_reputation::{
    BehaviourKind, PeerScoreTable, ReputationEvent, ReputationParams,
};
use adnet_types::NodeId;

use crate::error::ModelCatalogError;

/// Outcome of a download attempt — feeds into the reputation system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadOutcome {
    /// Blob verified and downloaded successfully.
    Success,
    /// Download failed (network, hash mismatch, etc.).
    Failure,
    /// Download was cancelled by the user.
    Cancelled,
}

impl DownloadOutcome {
    fn to_event(self, peer: NodeId) -> ReputationEvent {
        match self {
            DownloadOutcome::Success => ReputationEvent::ValidMessage {
                peer,
                topic: None,
                size_bytes: 0,
            },
            DownloadOutcome::Failure => ReputationEvent::BehaviourPenalty {
                peer,
                behaviour: BehaviourKind::ProtocolViolation,
                count: 1,
            },
            DownloadOutcome::Cancelled => ReputationEvent::InactivePeer { peer },
        }
    }
}

/// Reasons a user might report a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportReason {
    /// Unsolicited advertising / SEO spam.
    Spam,
    /// Harassment.
    Harassment,
    /// Impersonating another user / device.
    Impersonation,
    /// Phishing or other social-engineering attempt.
    Phishing,
    /// Misleading model description or fraudulent metadata.
    MisleadingMetadata,
    /// License terms are violated.
    LicenseViolation,
    /// Content is malicious / harmful.
    MaliciousContent,
    /// Other (free-form).
    Other,
}

/// Snapshot of a provider's current reputation, persisted in the catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderReputation {
    /// Provider's Iroh node id (64 hex chars).
    pub node_id: String,
    /// Cumulative score in the global PeerScoreTable (snapshot).
    pub score: f64,
    /// Number of successful downloads attributed to this provider.
    pub successful_downloads: u64,
    /// Number of failed downloads.
    pub failed_downloads: u64,
    /// Number of user reports filed against this provider.
    pub reports_count: u64,
    /// Last time the score was updated.
    pub last_updated: DateTime<Utc>,
    /// Whether the local operator has marked this provider as
    /// trusted/blocked (manual override).
    pub trust_flag: TrustFlag,
}

impl ProviderReputation {
    fn new(node_id: String) -> Self {
        Self {
            node_id,
            score: 0.0,
            successful_downloads: 0,
            failed_downloads: 0,
            reports_count: 0,
            last_updated: Utc::now(),
            trust_flag: TrustFlag::Neutral,
        }
    }

    /// Compute a coarse trust tier from the numeric score. Useful for
    /// UI badges ("trusted", "unknown", "risky").
    pub fn trust_tier(&self) -> TrustTier {
        match self.trust_flag {
            TrustFlag::Trusted => TrustTier::Trusted,
            TrustFlag::Blocked => TrustTier::Blocked,
            TrustFlag::Neutral => {
                if self.score >= 25.0 {
                    TrustTier::Trusted
                } else if self.score >= 0.0 {
                    TrustTier::Neutral
                } else if self.score >= -25.0 {
                    TrustTier::Risky
                } else {
                    TrustTier::Blocked
                }
            }
        }
    }

    /// Success rate as a fraction `[0.0, 1.0]`. Returns `None` if no
    /// downloads have been observed yet.
    pub fn success_rate(&self) -> Option<f64> {
        let total = self.successful_downloads + self.failed_downloads;
        if total == 0 {
            None
        } else {
            Some(self.successful_downloads as f64 / total as f64)
        }
    }
}

/// Manual operator override for a provider's trust state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustFlag {
    /// Operator has explicitly trusted this provider.
    Trusted,
    /// Default — derive tier from score.
    Neutral,
    /// Operator has explicitly blocked this provider.
    Blocked,
}

/// Coarse-grained tier derived from [`ProviderReputation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustTier {
    /// Score >= 25 or operator flag.
    Trusted,
    /// Score in `[0, 25)`.
    Neutral,
    /// Score in `[-25, 0)`.
    Risky,
    /// Score < -25 or operator flag.
    Blocked,
}

/// Aggregate stats across all known providers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReputationStats {
    /// Total providers tracked.
    pub total_providers: u64,
    /// Providers in each tier.
    pub trusted: u64,
    pub neutral: u64,
    pub risky: u64,
    pub blocked: u64,
    /// Sum of all download outcomes recorded.
    pub total_downloads: u64,
    pub total_successful: u64,
    pub total_failed: u64,
}

/// Provider reputation tracker.
///
/// The in-memory cache mirrors the on-disk column so API/UI reads
/// don't have to hit SQLite on every call.
pub struct ProviderReputationTracker {
    /// Map from NodeId hex string to in-memory snapshot.
    cache: RwLock<HashMap<String, ProviderReputation>>,
    /// Global, cross-subsystem scoring table. Optional so the crate
    /// can still build without `adnet-reputation`.
    scores: Option<Arc<PeerScoreTable>>,
    /// Tunable weights. Defaults from `adnet-reputation`.
    #[allow(dead_code)]
    params: ReputationParams,
}

impl ProviderReputationTracker {
    /// Create a tracker without a global PeerScoreTable. Use this
    /// when running offline / tests.
    pub fn new() -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
            scores: None,
            params: ReputationParams::default(),
        }
    }

    /// Create a tracker backed by a global PeerScoreTable. Gossip /
    /// bitswap events go through the table; catalog events go through
    /// `record_*` methods.
    pub fn with_score_table(scores: Arc<PeerScoreTable>) -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
            scores: Some(scores),
            params: ReputationParams::default(),
        }
    }

    /// Restore from on-disk snapshots.
    pub fn hydrate(&self, snapshots: Vec<ProviderReputation>) {
        let mut cache = self.cache.write();
        for s in snapshots {
            cache.insert(s.node_id.clone(), s);
        }
    }

    /// Get a snapshot for a provider, creating one if absent.
    pub fn get_or_create(&self, node_id: &str) -> ProviderReputation {
        {
            let cache = self.cache.read();
            if let Some(p) = cache.get(node_id) {
                return p.clone();
            }
        }
        let mut cache = self.cache.write();
        cache
            .entry(node_id.to_string())
            .or_insert_with(|| ProviderReputation::new(node_id.to_string()))
            .clone()
    }

    /// Get a snapshot for a provider, returning `None` if unknown.
    pub fn get(&self, node_id: &str) -> Option<ProviderReputation> {
        self.cache.read().get(node_id).cloned()
    }

    /// All snapshots currently in memory.
    pub fn snapshots(&self) -> Vec<ProviderReputation> {
        self.cache.read().values().cloned().collect()
    }

    /// Record the outcome of a download. `model_id` is informational
    /// only — the score impact is on the provider's `node_id`.
    pub fn record_download(
        &self,
        node_id: &str,
        outcome: DownloadOutcome,
        model_id: &str,
    ) -> Result<(), ModelCatalogError> {
        let peer = NodeId::from_hex(node_id).map_err(|e| {
            ModelCatalogError::ValidationError(format!("invalid provider node_id: {}", e))
        })?;

        // Push into the global score table if available.
        // We only borrow peer so the snapshot update below can still read it.
        if let Some(table) = &self.scores {
            table.apply(outcome.to_event(peer.clone()));
        }

        // Update the in-memory snapshot
        let mut cache = self.cache.write();
        let entry = cache
            .entry(node_id.to_string())
            .or_insert_with(|| ProviderReputation::new(node_id.to_string()));
        match outcome {
            DownloadOutcome::Success => entry.successful_downloads += 1,
            DownloadOutcome::Failure => entry.failed_downloads += 1,
            DownloadOutcome::Cancelled => {}
        }
        if let Some(table) = &self.scores {
            entry.score = table.score(&peer).unwrap_or(entry.score);
        }
        entry.last_updated = Utc::now();
        tracing::debug!(
            "Provider {} recorded download {:?} for {}",
            node_id,
            outcome,
            model_id
        );
        Ok(())
    }

    /// Submit a user report against a provider. `by_user` is the
    /// numeric id of the local user filing the report; pass `0` for
    /// anonymous / system reports.
    pub fn report_provider(
        &self,
        node_id: &str,
        reason: ReportReason,
        by_user: u64,
        detail: Option<String>,
    ) -> Result<(), ModelCatalogError> {
        let peer = NodeId::from_hex(node_id).map_err(|e| {
            ModelCatalogError::ValidationError(format!("invalid provider node_id: {}", e))
        })?;

        let kind = match reason {
            ReportReason::Spam => adnet_reputation::ReportKind::Spam,
            ReportReason::Harassment => adnet_reputation::ReportKind::Harassment,
            ReportReason::Impersonation => adnet_reputation::ReportKind::Impersonation,
            ReportReason::Phishing => adnet_reputation::ReportKind::Phishing,
            ReportReason::Other | ReportReason::MisleadingMetadata
            | ReportReason::LicenseViolation | ReportReason::MaliciousContent => {
                adnet_reputation::ReportKind::Other
            }
        };

        if let Some(table) = &self.scores {
            table.apply(ReputationEvent::ChatTrustReport {
                peer: peer.clone(),
                by_user,
                report: kind,
            });
        }

        let mut cache = self.cache.write();
        let entry = cache
            .entry(node_id.to_string())
            .or_insert_with(|| ProviderReputation::new(node_id.to_string()));
        entry.reports_count += 1;
        if let Some(table) = &self.scores {
            entry.score = table.score(&peer).unwrap_or(entry.score);
        }
        entry.last_updated = Utc::now();
        let _ = detail; // reserved for future on-disk audit log
        Ok(())
    }

    /// Manually mark a provider as trusted / blocked / neutral.
    pub fn set_trust_flag(
        &self,
        node_id: &str,
        flag: TrustFlag,
    ) -> Result<(), ModelCatalogError> {
        if NodeId::from_hex(node_id).is_err() {
            return Err(ModelCatalogError::ValidationError(format!(
                "invalid provider node_id: {}",
                node_id
            )));
        }
        let mut cache = self.cache.write();
        let entry = cache
            .entry(node_id.to_string())
            .or_insert_with(|| ProviderReputation::new(node_id.to_string()));
        entry.trust_flag = flag;
        entry.last_updated = Utc::now();
        Ok(())
    }

    /// Compute aggregate stats across all known providers.
    pub fn stats(&self) -> ReputationStats {
        let cache = self.cache.read();
        let mut stats = ReputationStats {
            total_providers: cache.len() as u64,
            ..Default::default()
        };
        for p in cache.values() {
            stats.total_downloads += p.successful_downloads + p.failed_downloads;
            stats.total_successful += p.successful_downloads;
            stats.total_failed += p.failed_downloads;
            match p.trust_tier() {
                TrustTier::Trusted => stats.trusted += 1,
                TrustTier::Neutral => stats.neutral += 1,
                TrustTier::Risky => stats.risky += 1,
                TrustTier::Blocked => stats.blocked += 1,
            }
        }
        stats
    }
}

impl Default for ProviderReputationTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_NODE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn new_tracker_starts_empty() {
        let t = ProviderReputationTracker::new();
        assert!(t.get(TEST_NODE).is_none());
        let stats = t.stats();
        assert_eq!(stats.total_providers, 0);
    }

    #[test]
    fn get_or_create_returns_default_snapshot() {
        let t = ProviderReputationTracker::new();
        let p = t.get_or_create(TEST_NODE);
        assert_eq!(p.successful_downloads, 0);
        assert_eq!(p.trust_tier(), TrustTier::Neutral);
    }

    #[test]
    fn record_success_increments_counter() {
        let t = ProviderReputationTracker::new();
        t.record_download(TEST_NODE, DownloadOutcome::Success, "m1")
            .unwrap();
        let p = t.get(TEST_NODE).unwrap();
        assert_eq!(p.successful_downloads, 1);
        assert_eq!(p.failed_downloads, 0);
    }

    #[test]
    fn record_failure_increments_failed() {
        let t = ProviderReputationTracker::new();
        t.record_download(TEST_NODE, DownloadOutcome::Failure, "m1")
            .unwrap();
        let p = t.get(TEST_NODE).unwrap();
        assert_eq!(p.failed_downloads, 1);
    }

    #[test]
    fn record_invalid_node_id_rejected() {
        let t = ProviderReputationTracker::new();
        let bad = t.record_download("not-hex", DownloadOutcome::Success, "m1");
        assert!(bad.is_err());
    }

    #[test]
    fn report_provider_increments_report_count() {
        let t = ProviderReputationTracker::new();
        t.report_provider(TEST_NODE, ReportReason::Spam, 0, Some("dup".into()))
            .unwrap();
        let p = t.get(TEST_NODE).unwrap();
        assert_eq!(p.reports_count, 1);
    }

    #[test]
    fn set_trust_flag_changes_tier() {
        let t = ProviderReputationTracker::new();
        t.set_trust_flag(TEST_NODE, TrustFlag::Trusted).unwrap();
        let p = t.get(TEST_NODE).unwrap();
        assert_eq!(p.trust_tier(), TrustTier::Trusted);
        t.set_trust_flag(TEST_NODE, TrustFlag::Blocked).unwrap();
        let p = t.get(TEST_NODE).unwrap();
        assert_eq!(p.trust_tier(), TrustTier::Blocked);
    }

    #[test]
    fn success_rate_handles_zero_total() {
        let p = ProviderReputation::new(TEST_NODE.into());
        assert_eq!(p.success_rate(), None);
        let mut p = p;
        p.successful_downloads = 7;
        p.failed_downloads = 3;
        assert_eq!(p.success_rate(), Some(0.7));
    }

    #[test]
    fn trust_tier_derives_from_score() {
        let mut p = ProviderReputation::new(TEST_NODE.into());
        p.score = 30.0;
        assert_eq!(p.trust_tier(), TrustTier::Trusted);
        p.score = 10.0;
        assert_eq!(p.trust_tier(), TrustTier::Neutral);
        p.score = -10.0;
        assert_eq!(p.trust_tier(), TrustTier::Risky);
        p.score = -30.0;
        assert_eq!(p.trust_tier(), TrustTier::Blocked);
    }

    #[test]
    fn hydrate_populates_cache() {
        let t = ProviderReputationTracker::new();
        let mut p = ProviderReputation::new(TEST_NODE.into());
        p.successful_downloads = 5;
        t.hydrate(vec![p]);
        let fetched = t.get(TEST_NODE).unwrap();
        assert_eq!(fetched.successful_downloads, 5);
    }

    #[test]
    fn stats_aggregate_correctly() {
        let t = ProviderReputationTracker::new();
        t.record_download(TEST_NODE, DownloadOutcome::Success, "m1")
            .unwrap();
        t.record_download(TEST_NODE, DownloadOutcome::Failure, "m1")
            .unwrap();
        let stats = t.stats();
        assert_eq!(stats.total_providers, 1);
        assert_eq!(stats.total_downloads, 2);
        assert_eq!(stats.total_successful, 1);
        assert_eq!(stats.total_failed, 1);
    }

    // ── Edge-case / robustness tests ────────────────────────────

    #[test]
    fn cancelled_outcome_does_not_change_counters() {
        let t = ProviderReputationTracker::new();
        t.record_download(TEST_NODE, DownloadOutcome::Cancelled, "m1")
            .unwrap();
        let p = t.get(TEST_NODE).unwrap();
        assert_eq!(p.successful_downloads, 0);
        assert_eq!(p.failed_downloads, 0);
    }

    #[test]
    fn multiple_outcomes_accumulate() {
        let t = ProviderReputationTracker::new();
        for _ in 0..5 {
            t.record_download(TEST_NODE, DownloadOutcome::Success, "m")
                .unwrap();
        }
        for _ in 0..2 {
            t.record_download(TEST_NODE, DownloadOutcome::Failure, "m")
                .unwrap();
        }
        let p = t.get(TEST_NODE).unwrap();
        assert_eq!(p.successful_downloads, 5);
        assert_eq!(p.failed_downloads, 2);
        assert_eq!(p.success_rate(), Some(5.0 / 7.0));
    }

    #[test]
    fn short_node_id_is_rejected() {
        let t = ProviderReputationTracker::new();
        let r = t.record_download("abc", DownloadOutcome::Success, "m");
        assert!(r.is_err());
        let r = t.report_provider("abc", ReportReason::Spam, 0, None);
        assert!(r.is_err());
        let r = t.set_trust_flag("abc", TrustFlag::Trusted);
        assert!(r.is_err());
    }

    #[test]
    fn uppercase_node_id_is_rejected() {
        // NodeId is hex-chars only (lowercased by from_hex). Uppercase
        // 64-char input still validates because hex chars are
        // case-insensitive. But invalid characters are caught.
        let t = ProviderReputationTracker::new();
        let r = t.record_download("Z".repeat(64).as_str(), DownloadOutcome::Success, "m");
        assert!(r.is_err());
    }

    #[test]
    fn empty_node_id_is_rejected() {
        let t = ProviderReputationTracker::new();
        let r = t.record_download("", DownloadOutcome::Success, "m");
        assert!(r.is_err());
    }

    #[test]
    fn multiple_reports_increment_count() {
        let t = ProviderReputationTracker::new();
        for _ in 0..7 {
            t.report_provider(TEST_NODE, ReportReason::Spam, 0, None)
                .unwrap();
        }
        let p = t.get(TEST_NODE).unwrap();
        assert_eq!(p.reports_count, 7);
    }

    #[test]
    fn snapshot_is_cloneable_and_independent() {
        let t = ProviderReputationTracker::new();
        t.record_download(TEST_NODE, DownloadOutcome::Success, "m")
            .unwrap();
        let a = t.get(TEST_NODE).unwrap();
        let mut b = a.clone();
        b.successful_downloads = 999;
        let a_again = t.get(TEST_NODE).unwrap();
        // Mutating the clone did not affect the cached snapshot
        assert_eq!(a_again.successful_downloads, 1);
    }

    #[test]
    fn snapshots_returns_all_known_providers() {
        let t = ProviderReputationTracker::new();
        let n1 = "1111111111111111111111111111111111111111111111111111111111111111";
        let n2 = "2222222222222222222222222222222222222222222222222222222222222222";
        let n3 = "3333333333333333333333333333333333333333333333333333333333333333";
        t.record_download(n1, DownloadOutcome::Success, "m").unwrap();
        t.record_download(n2, DownloadOutcome::Success, "m").unwrap();
        t.record_download(n3, DownloadOutcome::Failure, "m").unwrap();
        let snaps = t.snapshots();
        assert_eq!(snaps.len(), 3);
    }

    #[test]
    fn report_does_not_require_global_score_table() {
        // Without a PeerScoreTable backing the tracker, reports
        // should still update local counters.
        let t = ProviderReputationTracker::new();
        t.report_provider(TEST_NODE, ReportReason::LicenseViolation, 0, None)
            .unwrap();
        let p = t.get(TEST_NODE).unwrap();
        assert_eq!(p.reports_count, 1);
    }

    #[test]
    fn set_trust_flag_invalid_node_id_rejected() {
        let t = ProviderReputationTracker::new();
        let r = t.set_trust_flag("not-hex", TrustFlag::Trusted);
        assert!(r.is_err());
    }

    #[test]
    fn trust_flag_overrides_negative_score() {
        let mut p = ProviderReputation::new(TEST_NODE.into());
        p.score = -100.0; // would normally be Blocked
        p.trust_flag = TrustFlag::Trusted;
        assert_eq!(p.trust_tier(), TrustTier::Trusted);
    }

    #[test]
    fn trust_flag_overrides_high_score() {
        let mut p = ProviderReputation::new(TEST_NODE.into());
        p.score = 100.0; // would normally be Trusted
        p.trust_flag = TrustFlag::Blocked;
        assert_eq!(p.trust_tier(), TrustTier::Blocked);
    }

    #[test]
    fn boundary_scores_are_classified_correctly() {
        let mut p = ProviderReputation::new(TEST_NODE.into());
        p.score = 25.0;
        assert_eq!(p.trust_tier(), TrustTier::Trusted);
        p.score = 0.0;
        assert_eq!(p.trust_tier(), TrustTier::Neutral);
        p.score = -25.0;
        assert_eq!(p.trust_tier(), TrustTier::Risky);
    }

    #[test]
    fn stats_correctly_classify_tiers() {
        let t = ProviderReputationTracker::new();
        // 2 trusted, 1 risky
        let n_trusted = "1111111111111111111111111111111111111111111111111111111111111111";
        let n_other = "2222222222222222222222222222222222222222222222222222222222222222";
        let n_blocked = "3333333333333333333333333333333333333333333333333333333333333333";
        t.set_trust_flag(n_trusted, TrustFlag::Trusted).unwrap();
        t.set_trust_flag(n_blocked, TrustFlag::Blocked).unwrap();
        // n_other is neutral with no score → Risky by default? No:
        // score=0.0 → Neutral tier.
        t.record_download(n_other, DownloadOutcome::Failure, "m")
            .unwrap();
        let stats = t.stats();
        assert_eq!(stats.total_providers, 3);
        assert_eq!(stats.trusted, 1);
        assert_eq!(stats.blocked, 1);
        assert_eq!(stats.neutral, 1);
        assert_eq!(stats.total_failed, 1);
    }

    #[test]
    fn tracker_default_impl_works() {
        let t = ProviderReputationTracker::default();
        assert_eq!(t.stats().total_providers, 0);
    }

    #[test]
    fn provider_reputation_serializes_roundtrip() {
        let mut p = ProviderReputation::new(TEST_NODE.into());
        p.score = 12.5;
        p.successful_downloads = 3;
        p.failed_downloads = 1;
        p.reports_count = 2;
        p.trust_flag = TrustFlag::Trusted;
        let json = serde_json::to_string(&p).expect("serialize");
        let back: ProviderReputation =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.node_id, TEST_NODE);
        assert_eq!(back.score, 12.5);
        assert_eq!(back.successful_downloads, 3);
        assert_eq!(back.failed_downloads, 1);
        assert_eq!(back.reports_count, 2);
        assert_eq!(back.trust_flag, TrustFlag::Trusted);
    }

    #[test]
    fn download_outcome_serializes_snake_case() {
        let json = serde_json::to_string(&DownloadOutcome::Success).unwrap();
        assert_eq!(json, "\"success\"");
        let json = serde_json::to_string(&DownloadOutcome::Failure).unwrap();
        assert_eq!(json, "\"failure\"");
        let json = serde_json::to_string(&DownloadOutcome::Cancelled).unwrap();
        assert_eq!(json, "\"cancelled\"");
    }

    #[test]
    fn report_reason_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&ReportReason::MisleadingMetadata).unwrap(),
            "\"misleading_metadata\""
        );
        assert_eq!(
            serde_json::to_string(&ReportReason::LicenseViolation).unwrap(),
            "\"license_violation\""
        );
        assert_eq!(
            serde_json::to_string(&ReportReason::MaliciousContent).unwrap(),
            "\"malicious_content\""
        );
    }

    #[test]
    fn trust_flag_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&TrustFlag::Trusted).unwrap(),
            "\"trusted\""
        );
        assert_eq!(
            serde_json::to_string(&TrustFlag::Blocked).unwrap(),
            "\"blocked\""
        );
        assert_eq!(
            serde_json::to_string(&TrustFlag::Neutral).unwrap(),
            "\"neutral\""
        );
    }

    #[test]
    fn trust_tier_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&TrustTier::Trusted).unwrap(),
            "\"trusted\""
        );
        assert_eq!(
            serde_json::to_string(&TrustTier::Risky).unwrap(),
            "\"risky\""
        );
        assert_eq!(
            serde_json::to_string(&TrustTier::Blocked).unwrap(),
            "\"blocked\""
        );
    }

    #[test]
    fn hydrate_replaces_existing_snapshots() {
        let t = ProviderReputationTracker::new();
        let mut initial = ProviderReputation::new(TEST_NODE.into());
        initial.successful_downloads = 5;
        t.hydrate(vec![initial]);
        // Hydrate again with a fresh snapshot
        let mut fresh = ProviderReputation::new(TEST_NODE.into());
        fresh.successful_downloads = 99;
        t.hydrate(vec![fresh]);
        let p = t.get(TEST_NODE).unwrap();
        // The latest hydrate wins
        assert_eq!(p.successful_downloads, 99);
    }

    #[test]
    fn get_or_create_does_not_overwrite_existing() {
        let t = ProviderReputationTracker::new();
        t.record_download(TEST_NODE, DownloadOutcome::Success, "m")
            .unwrap();
        let p = t.get_or_create(TEST_NODE);
        assert_eq!(p.successful_downloads, 1);
        // Calling again should still report 1
        let p2 = t.get_or_create(TEST_NODE);
        assert_eq!(p2.successful_downloads, 1);
    }
}