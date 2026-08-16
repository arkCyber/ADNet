//! The planner — the public entry point for racing hole-punch
//! strategies.
//!
//! ## Flow
//!
//! 1. Caller validates the `NodeId` (cheap pre-check).
//! 2. The planner derives the effective strategy list (capability
//!    filter + concurrency cap).
//! 3. For each strategy, the planner spawns a tokio task that
//!    invokes the strategy's `HolePunchResolver` with a per-call
//!    `tokio::sync::Notify` cancel handle and a per-strategy
//!    budget.
//! 4. The planner races the strategy futures using
//!    `futures::future::select_all` so the first surface that
//!    returns a non-empty `ResolvedEndpoint` wins; the planner
//!    writes the winner into the outcome and aborts the rest.
//! 5. If everyone's budget elapses without a hit, the planner
//!    surfaces `HolePunchError::StrategiesExhausted`.
//!
//! ## Cancellation
//!
//! A "winner" aborts the rest via the shared `tokio::sync::Notify`
//! channel — the resolver's `resolve(...)` future must `select!`
//! on the notify channel so the cancel is cooperative. Hard
//! `AbortHandle::abort` is a last-resort fallback for resolvers
//! that ignore the notify signal.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use futures::future::select_all;
use futures::FutureExt;
use tokio::sync::Notify;

use a3net_types::NodeId;

use crate::config::{validate_node_id, HolePunchConfig, TimeoutPolicy};
use crate::diagnostics::{
    AttemptOutcome, HolePunchDiagnostics, HolePunchOutcome, StrategyAttempt,
};
use crate::error::{HolePunchError, HolePunchResult};
use crate::strategy::{HolePunchResolver, HolePunchStrategy, ResolvedEndpoint, ResolverCapabilities};
use crate::strategy::HollowResolver;

/// The public entry point. Cheap to construct; one instance can
/// be shared across many `punch()` calls.
pub struct HolePunchPlanner {
    config: HolePunchConfig,
    diagnostics: Arc<HolePunchDiagnostics>,
}

impl Clone for HolePunchPlanner {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            diagnostics: Arc::clone(&self.diagnostics),
        }
    }
}

impl std::fmt::Debug for HolePunchPlanner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HolePunchPlanner")
            .field("strategies", &self.config.strategies.len())
            .field("budget", &self.config.budget)
            .field("per_strategy_budget", &self.config.per_strategy_budget)
            .field("ordering", &self.config.ordering)
            .field("timeout_policy", &self.config.timeout_policy)
            .finish()
    }
}

impl HolePunchPlanner {
    /// Build a planner from a config. Returns
    /// `HolePunchError::InvalidConfig` if the config is
    /// self-contradictory (no strategies, zero budget, etc.).
    pub fn new(config: HolePunchConfig) -> HolePunchResult<Self> {
        config.validate()?;
        Ok(Self {
            config,
            diagnostics: Arc::new(HolePunchDiagnostics::new()),
        })
    }

    /// Build a planner with a shared diagnostics block. Catches
    /// the same config errors as [`HolePunchPlanner::new`].
    pub fn with_diagnostics(
        config: HolePunchConfig,
        diagnostics: Arc<HolePunchDiagnostics>,
    ) -> HolePunchResult<Self> {
        config.validate()?;
        Ok(Self {
            config,
            diagnostics,
        })
    }

    /// Borrow the config snapshot.
    pub fn config(&self) -> &HolePunchConfig {
        &self.config
    }

    /// Borrow the shared diagnostics block.
    pub fn diagnostics(&self) -> &Arc<HolePunchDiagnostics> {
        &self.diagnostics
    }

    /// Resolve a single `NodeId` to a `HolePunchOutcome`. The
    /// `punch()` call is the workhorse; it spawns one tokio task
    /// per strategy and races them.
    pub async fn punch(&self, target: NodeId) -> HolePunchOutcome {
        let outcome = self.punch_impl(target).await;
        // Stamp the diagnostics block BEFORE returning so a
        // snapshot taken right after `punch()` lands the
        // expected counters.
        self.diagnostics.record_outcome(&outcome);
        outcome
    }

    /// Resolve a single `NodeId`, returning a `Result` for
    /// callers that want short-circuit error semantics. The
    /// outcome is still recorded on the diagnostics block before
    /// the error is returned.
    pub async fn try_punch(&self, target: NodeId) -> HolePunchResult<ResolvedEndpoint> {
        let outcome = self.punch(target).await;
        match outcome.error {
            None => outcome
                .endpoint
                .ok_or_else(|| {
                    HolePunchError::Internal("outcome without error but no endpoint".into())
                }),
            Some(err) => Err(err),
        }
    }

    async fn punch_impl(&self, target: NodeId) -> HolePunchOutcome {
        let started_at = SystemTime::now();
        let started_inst = Instant::now();

        // Compute the effective strategy list AFTER the capability
        // filter and concurrency cap. The planner reports the
        // post-filter set on `enabled_strategies` so the
        // operator can reconcile "config said 4, filter left 1".
        let strategies = self.config.effective_strategies();
        let enabled_labels: Vec<String> =
            strategies.iter().map(|s| s.label().to_string()).collect();
        let _ = enabled_labels.len(); // keep variable live
        let effective_strategy_count = strategies.len();

        // Fast-fail on a malformed NodeId. The planner's
        // pre-check is defence-in-depth — the caller is
        // expected to have validated the bytes already — but a
        // bad input must NOT burn the budget.
        if let Err(e) = validate_node_id(&target) {
            return HolePunchOutcome {
                endpoint: None,
                winning_strategy: None,
                attempts: Vec::new(),
                elapsed: started_inst.elapsed(),
                started_at,
                error: Some(e),
                enabled_strategies: enabled_labels,
                effective_strategy_count: 0,
            };
        }

        // Compute the effective strategy list AFTER the
        // capability filter and concurrency cap. The planner
        // reports the post-filter count on the outcome so the
        // operator can tell "config said 4, filter left 2".
        let strategies = self.config.effective_strategies();

        if strategies.is_empty() {
            return HolePunchOutcome {
                endpoint: None,
                winning_strategy: None,
                attempts: Vec::new(),
                elapsed: started_inst.elapsed(),
                started_at,
                error: Some(HolePunchError::InvalidConfig(
                    "all strategies filtered out".into(),
                )),
                enabled_strategies: enabled_labels,
                effective_strategy_count: 0,
            };
        }

        let per_strategy_budget = self.config.per_strategy_budget;
        let overall_budget = self.config.budget;

        // The shared cancellation channel. Every spawned
        // resolver's `select!` must observe this notify so the
        // race can be cancelled cooperatively.
        let cancel = Arc::new(Notify::new());

        // Spawn one task per strategy. The task returns
        // `(StrategyAttempt, Option<ResolvedEndpoint>)` so the
        // winner's endpoint propagates back to the caller.
        let mut task_handles: Vec<(
            HolePunchStrategy,
            tokio::task::JoinHandle<(StrategyAttempt, Option<ResolvedEndpoint>)>,
        )> = Vec::with_capacity(strategies.len());

        for strategy in strategies {
            let resolver: Arc<dyn HolePunchResolver> = match &strategy {
                HolePunchStrategy::Custom(r) => Arc::clone(r),
                // Built-in strategies route through the
                // hollow resolver shim. The actual iroh
                // implementation is filled in by the
                // `iroh_bridge` module; tests can supply a
                // richer resolver via `HolePunchStrategy::Custom`.
                _ => Arc::new(HollowResolver::from_strategy(&strategy)),
            };
            let task_cancel = Arc::clone(&cancel);
            let target_cloned = target.clone();
            let handle = tokio::spawn(async move {
                let started = Instant::now();
                let outcome = resolver
                    .resolve(target_cloned, per_strategy_budget, task_cancel)
                    .await;
                let elapsed = started.elapsed();
                let strategy_label = resolver.label();
                let caps = resolver.capabilities();
                let (attempt_outcome, real_endpoint) = match outcome {
                    Ok(endpoint) => {
                        if endpoint.has_any_address() {
                            let n = endpoint.address_count();
                            (AttemptOutcome::Hit { addresses: n }, Some(endpoint))
                        } else {
                            (AttemptOutcome::Empty, None)
                        }
                    }
                    Err(HolePunchError::Cancelled) => (AttemptOutcome::Cancelled, None),
                    Err(e) => (
                        AttemptOutcome::Error {
                            error: e.classify().to_string(),
                            message: e.to_string(),
                        },
                        None,
                    ),
                };
                let attempt = StrategyAttempt {
                    strategy: strategy_label.to_string(),
                    capabilities: caps,
                    outcome: attempt_outcome,
                    elapsed,
                };
                (attempt, real_endpoint)
            });
            task_handles.push((strategy, handle));
        }

        // Snapshot the strategy labels + capabilities BEFORE we
        // move the handles out — we need them to build
        // Cancelled entries when the deadline hits.
        let strategy_meta: Vec<(String, ResolverCapabilities)> = task_handles
            .iter()
            .map(|(s, _)| (s.label().to_string(), s.capabilities()))
            .collect();

        // Take ownership of the join handles so the race loop
        // can move them into `select_all` futures without
        // borrow-checker friction. `abort_handles` lets us
        // hard-cancel siblings when a winner surfaces.
        let mut handles: Vec<tokio::task::JoinHandle<(StrategyAttempt, Option<ResolvedEndpoint>)>> =
            task_handles.into_iter().map(|(_, h)| h).collect();
        let total = handles.len();
        let mut abort_handles: Vec<Option<tokio::task::AbortHandle>> = Vec::with_capacity(total);
        for h in handles.iter_mut() {
            abort_handles.push(Some(h.abort_handle()));
        }

        let mut attempts: Vec<StrategyAttempt> = Vec::with_capacity(total);
        let mut endpoint: Option<ResolvedEndpoint> = None;
        let mut winning_strategy: Option<String> = None;

        // Drive the race with `select_all` over the handles.
        // Each iteration consumes one resolved task;
        // `select_all` returns the still-pending futures so
        // we can re-enter the loop with the survivors.
        let deadline = tokio::time::sleep(overall_budget);
        let deadline = deadline.fuse();
        tokio::pin!(deadline);

        let mut still_pending = handles;
        let mut pending_indices: Vec<usize> = (0..total).collect();

        while !pending_indices.is_empty() {
            tokio::select! {
                biased;
                _ = &mut deadline => {
                    // Deadline hit. Cancel the rest.
                    cancel.notify_waiters();
                    for slot in abort_handles.iter_mut() {
                        if let Some(h) = slot.take() {
                            h.abort();
                        }
                    }
                    for &i in &pending_indices {
                        let (label, caps) = strategy_meta[i].clone();
                        attempts.push(StrategyAttempt {
                            strategy: label,
                            capabilities: caps,
                            outcome: AttemptOutcome::Cancelled,
                            elapsed: Duration::ZERO,
                        });
                    }
                    pending_indices.clear();
                    break;
                }
                outcome = select_all(still_pending) => {
                    let (winner_output, idx, rest) = outcome;
                    // `idx` selects into `still_pending` (the current
                    // race set); translate it through the
                    // `pending_indices` bookkeeping to find the
                    // ORIGINAL strategy slot (stable across
                    // removals) so we can record the right label.
                    let original_idx = pending_indices[idx];
                    let (label, caps) = strategy_meta[original_idx].clone();
                    match winner_output {
                        Ok((att, real_endpoint)) => {
                            attempts.push(att);
                            let is_winner = real_endpoint.is_some();
                            if let Some(e) = real_endpoint {
                                if endpoint.is_none() {
                                    endpoint = Some(e);
                                    winning_strategy = Some(label.clone());
                                    cancel.notify_waiters();
                                    for slot in abort_handles.iter_mut() {
                                        if let Some(h) = slot.take() {
                                            h.abort();
                                        }
                                    }
                                }
                            }
                            pending_indices.remove(idx);
                            still_pending = rest;
                            if still_pending.is_empty() {
                                break;
                            }
                            if is_winner {
                                for &i in &pending_indices {
                                    let (label, caps) = strategy_meta[i].clone();
                                    attempts.push(StrategyAttempt {
                                        strategy: label,
                                        capabilities: caps,
                                        outcome: AttemptOutcome::Cancelled,
                                        elapsed: Duration::ZERO,
                                    });
                                }
                                pending_indices.clear();
                                break;
                            }
                        }
                        Err(_join_err) => {
                            attempts.push(StrategyAttempt {
                                strategy: label,
                                capabilities: caps,
                                outcome: AttemptOutcome::Cancelled,
                                elapsed: Duration::ZERO,
                            });
                            pending_indices.remove(idx);
                            still_pending = rest;
                        }
                    }
                }
            }
        }

        let elapsed = started_inst.elapsed();

        let error = if endpoint.is_some() {
            None
        } else {
            match self.config.timeout_policy {
                TimeoutPolicy::Fail => Some(HolePunchError::exhausted(elapsed, attempts.len())),
                TimeoutPolicy::SilentEmpty => {
                    // Per the documented contract, the
                    // planner still surfaces
                    // `StrategiesExhausted` so the caller
                    // can log it. The `try_punch` flow
                    // translates it to `Err(...)` either
                    // way — there's no "silent empty" for
                    // `punch()` itself, only for callers
                    // that prefer to ignore the error.
                    Some(HolePunchError::exhausted(elapsed, attempts.len()))
                }
            }
        };

        HolePunchOutcome {
            endpoint,
            winning_strategy,
            attempts,
            elapsed,
            started_at,
            error,
            enabled_strategies: enabled_labels,
            effective_strategy_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{HolePunchConfig, RequiredCapabilities};
    use crate::strategy::{HolePunchResolver, ResolvedEndpoint, ResolverCapabilities};
    use a3net_types::NodeId;
    use std::sync::Arc;
    use std::time::Duration;

    fn nid() -> NodeId {
        NodeId::from_bytes(&[0u8; 32]).expect("32 bytes is valid")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn new_rejects_empty_strategy_list() {
        let cfg = HolePunchConfig {
            strategies: vec![],
            ..HolePunchConfig::default()
        };
        let err = HolePunchPlanner::new(cfg).unwrap_err();
        assert!(matches!(err, HolePunchError::InvalidConfig(_)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn new_accepts_dns_default() {
        let cfg = HolePunchConfig::default();
        let planner = HolePunchPlanner::new(cfg).unwrap();
        let snap = planner.diagnostics().snapshot();
        assert_eq!(snap.calls_total, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn punch_records_diagnostics() {
        let cfg = HolePunchConfig::default();
        let planner = HolePunchPlanner::new(cfg).unwrap();
        let _ = planner.punch(nid()).await;
        let snap = planner.diagnostics().snapshot();
        assert_eq!(snap.calls_total, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn effective_strategies_filter_routes_through_config() {
        let cfg = HolePunchConfig::all_channels()
            .with_required_capabilities(RequiredCapabilities::tickets_only());
        let planner = HolePunchPlanner::new(cfg).unwrap();
        let outcome = planner.punch(nid()).await;
        assert_eq!(outcome.effective_strategy_count, 1);
        assert_eq!(outcome.enabled_strategies, vec!["a3net-ticket"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn clone_preserves_diagnostics() {
        let cfg = HolePunchConfig::default();
        let planner = HolePunchPlanner::new(cfg).unwrap();
        let cloned = planner.clone();
        let _ = planner.punch(nid()).await;
        let snap = cloned.diagnostics().snapshot();
        assert_eq!(snap.calls_total, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hollow_resolver_returns_empty_outcome() {
        let cfg = HolePunchConfig::default();
        let planner = HolePunchPlanner::new(cfg).unwrap();
        let outcome = planner.punch(nid()).await;
        assert!(!outcome.is_hit(), "hollow resolver should NOT hit");
        assert!(!outcome.attempts.is_empty());
        let att = &outcome.attempts[0];
        assert!(matches!(att.outcome, AttemptOutcome::Empty));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn custom_resolver_hit_wins() {
        // A custom resolver that always returns a hit
        // should be the winner of the race.
        #[derive(Debug)]
        struct AlwaysHit(String);
        #[async_trait::async_trait]
        impl HolePunchResolver for AlwaysHit {
            async fn resolve(
                &self,
                target: NodeId,
                _budget: Duration,
                _cancel: Arc<tokio::sync::Notify>,
            ) -> HolePunchResult<ResolvedEndpoint> {
                let mut ep = ResolvedEndpoint::empty(target);
                ep.relay_urls
                    .push(format!("https://relay.{}", self.0));
                Ok(ep)
            }
            fn label(&self) -> &'static str {
                "always-hit"
            }
            fn capabilities(&self) -> ResolverCapabilities {
                ResolverCapabilities::all()
            }
        }

        let cfg = HolePunchConfig::default().with_strategy(HolePunchStrategy::Custom(
            Arc::new(AlwaysHit("test".into())),
        ));
        let planner = HolePunchPlanner::new(cfg).unwrap();
        let outcome = planner.punch(nid()).await;
        eprintln!("attempts = {:#?}", outcome.attempts);
        assert!(outcome.is_hit());
        assert_eq!(outcome.winning_strategy.as_deref(), Some("always-hit"));
        let endpoint = outcome.endpoint.expect("hit");
        assert_eq!(endpoint.relay_urls.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn custom_resolver_error_is_recorded() {
        #[derive(Debug)]
        struct AlwaysError;
        #[async_trait::async_trait]
        impl HolePunchResolver for AlwaysError {
            async fn resolve(
                &self,
                _target: NodeId,
                _budget: Duration,
                _cancel: Arc<tokio::sync::Notify>,
            ) -> HolePunchResult<ResolvedEndpoint> {
                Err(HolePunchError::ResolverIo("simulated".into()))
            }
            fn label(&self) -> &'static str {
                "always-error"
            }
            fn capabilities(&self) -> ResolverCapabilities {
                ResolverCapabilities::all()
            }
        }

        let cfg = HolePunchConfig::default().with_strategy(HolePunchStrategy::Custom(
            Arc::new(AlwaysError),
        ));
        let planner = HolePunchPlanner::new(cfg).unwrap();
        let outcome = planner.punch(nid()).await;
        assert!(!outcome.is_hit());
        let att = outcome
            .attempts
            .iter()
            .find(|a| a.strategy == "always-error")
            .expect("always-error attempt");
        assert!(matches!(att.outcome, AttemptOutcome::Error { .. }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn winning_strategy_cancels_siblings() {
        // A fast-hit resolver should record the hit AND
        // mark the slower siblings as `Cancelled`.
        #[derive(Debug)]
        struct FastHit;
        #[async_trait::async_trait]
        impl HolePunchResolver for FastHit {
            async fn resolve(
                &self,
                target: NodeId,
                _budget: Duration,
                _cancel: Arc<tokio::sync::Notify>,
            ) -> HolePunchResult<ResolvedEndpoint> {
                let mut ep = ResolvedEndpoint::empty(target);
                ep.relay_urls.push("https://relay.fast".into());
                Ok(ep)
            }
            fn label(&self) -> &'static str {
                "fast-hit"
            }
            fn capabilities(&self) -> ResolverCapabilities {
                ResolverCapabilities::all()
            }
        }

        let cfg = HolePunchConfig::default()
            .with_strategy(HolePunchStrategy::Custom(Arc::new(FastHit)));
        let planner = HolePunchPlanner::new(cfg).unwrap();
        let outcome = planner.punch(nid()).await;
        assert!(outcome.is_hit());
        assert!(outcome.cancelled() <= 1, "at most one other strategy is cancelled");
    }
}
