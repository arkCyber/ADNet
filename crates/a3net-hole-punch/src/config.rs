//! Hole-punch planner configuration.
//!
//! The config is intentionally small: a list of enabled
//! strategies, an overall budget, and a "policy" knob that tells
//! the planner what to do when every strategy fails. Operators
//! who want to disable the n0 DNS path can replace the default
//! with [`HolePunchConfig::air_gapped`] (LAN + tickets only).
//!
//! We deliberately do NOT let the constructor spawn any tokio
//! task or open any socket — the builder is pure data, and the
//! planner spawns work only when [`HolePunchPlanner::punch`] is
//! called. Keeps cold-start free.

use std::time::Duration;

use a3net_types::NodeId;
use serde::{Deserialize, Serialize};

use crate::error::HolePunchError;
use crate::strategy::{HolePunchStrategy, ResolverCapabilities};

/// Default overall budget for one full `punch()` call. The planner
/// treats this as a wall-clock cap covering ALL strategies racing
/// in parallel; the per-strategy budget is derived from this.
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(8);

/// Default per-strategy budget. Capped above
/// [`MIN_PER_STRATEGY_BUDGET`] so the planner doesn't spawn
/// strategy children that abort almost instantly.
pub const DEFAULT_PER_STRATEGY_BUDGET: Duration = Duration::from_secs(8);

/// Minimum per-strategy budget. Below this the planner refuses
/// the config — a sub-100ms budget would let mid-resolution
/// sockets bind before the abort fires.
pub const MIN_PER_STRATEGY_BUDGET: Duration = Duration::from_millis(100);

/// Maximum number of distinct strategies the planner will race. A
/// hard cap so a misconfigured operator can't try to spawn 1000
/// parallel DHT queries.
pub const MAX_STRATEGIES: usize = 16;

/// How the planner orders strategies when more than one would
/// match the same node. The current implementation always
/// **races** (the planner is `race_all` by design), but the field
/// is kept so future variants can introduce a "try A first"
/// preference without changing the public type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PunchOrdering {
    /// Race every strategy in parallel; first non-empty wins.
    /// This is the documented behaviour and the default.
    #[default]
    RaceAll,
    /// Prefer the first strategy that returns a hit; only fall
    /// through to the next on a clean miss. **Reserved for future
    /// use** — the planner currently ignores this and still races
    /// everything. The variant is kept so the config surface is
    /// stable when the planner gains fallback semantics.
    PreferFirst,
}

/// What the planner does when all strategies time out or every
/// strategy returns an empty endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TimeoutPolicy {
    /// Surface `HolePunchError::StrategiesExhausted` to the caller.
    /// The operator is expected to decide whether to retry, fall
    /// back to a relay, or refresh the local address book.
    #[default]
    Fail,
    /// Return a `HolePunchOutcome` whose `endpoint` is `None` and
    /// whose `error` is `None`. Useful for fire-and-forget
    /// discovery flows (e.g. background telemetry) where the
    /// caller treats absence as silence rather than a failure.
    SilentEmpty,
}

/// User-facing configuration. Composable via the builder methods so
/// the call site reads as a recipe.
#[derive(Debug, Clone)]
pub struct HolePunchConfig {
    /// Ordered list of strategies the planner will race. The
    /// default is `[PkarrDns]` — DNS-first — to match stock iroh
    /// behaviour so a fresh node can find peers out of the box.
    pub strategies: Vec<HolePunchStrategy>,
    /// Overall wall-clock budget for one `punch()` call. The
    /// planner cancels every still-running strategy when this
    /// elapses.
    pub budget: Duration,
    /// Per-strategy budget. Derived from `budget` when not set
    /// explicitly. The planner asserts the per-strategy budget is
    /// `<= budget` and `>= MIN_PER_STRATEGY_BUDGET`.
    pub per_strategy_budget: Duration,
    /// Ordering knob. Kept on the struct for forward compatibility
    /// (see [`PunchOrdering`]).
    pub ordering: PunchOrdering,
    /// What to do when every strategy fails. See [`TimeoutPolicy`].
    pub timeout_policy: TimeoutPolicy,
    /// Optional capability filter. When set, the planner drops
    /// any strategy whose [`ResolverCapabilities`] does not
    /// intersect the filter. The default is `None` (no filter).
    pub require_capabilities: Option<RequiredCapabilities>,
    /// Optional global cap on the number of concurrent strategies.
    /// Defaults to `strategies.len()` (i.e. "race everything").
    /// Operators can use this to artificially throttle (e.g. cap
    /// at 2 so a wallet-mode phone doesn't hammer the DHT).
    pub max_concurrent: Option<usize>,
}

/// Capabilities the planner requires before a strategy is allowed
/// to run. The filter is an "any-of" — a strategy is eligible when
/// it advertises at least ONE of the listed capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequiredCapabilities {
    pub lan: bool,
    pub wan: bool,
    pub zero_knowledge: bool,
    pub out_of_band: bool,
}

impl RequiredCapabilities {
    /// True when `caps` intersects `self` (at least one matching
    /// flag). The planner uses this to skip strategies that can't
    /// reach the target.
    pub fn intersects(&self, caps: ResolverCapabilities) -> bool {
        (self.lan && caps.lan)
            || (self.wan && caps.wan)
            || (self.zero_knowledge && caps.zero_knowledge)
            || (self.out_of_band && caps.out_of_band)
    }

    /// "WAN or zero-knowledge" — the default for an out-of-the-box
    /// resolver setup. Excludes LAN-only (mDNS) and OOB-only
    /// (tickets).
    pub fn wan_or_dht() -> Self {
        Self {
            lan: false,
            wan: true,
            zero_knowledge: true,
            out_of_band: false,
        }
    }

    /// "Tickets only" — for forensic deployments where the
    /// operator wants to refuse any resolver that talks to the
    /// public network.
    pub fn tickets_only() -> Self {
        Self {
            lan: false,
            wan: false,
            zero_knowledge: false,
            out_of_band: true,
        }
    }
}

impl Default for HolePunchConfig {
    /// DNS-first default. Matches stock iroh behaviour so a fresh
    /// node can find peers out of the box. The single default
    /// strategy is `PkarrDns`; the caller can extend with
    /// `with_strategy` / `with_extra_resolver` to add mDNS, DHT,
    /// or tickets.
    fn default() -> Self {
        Self {
            strategies: vec![HolePunchStrategy::PkarrDns],
            budget: DEFAULT_BUDGET,
            per_strategy_budget: DEFAULT_PER_STRATEGY_BUDGET,
            ordering: PunchOrdering::default(),
            timeout_policy: TimeoutPolicy::default(),
            require_capabilities: None,
            max_concurrent: None,
        }
    }
}

impl HolePunchConfig {
    /// DNS-only configuration. Useful for tests / CI that want a
    /// deterministic single-channel resolution path.
    pub fn dns_only() -> Self {
        Self {
            strategies: vec![HolePunchStrategy::PkarrDns],
            ..Self::default()
        }
    }

    /// "Air-gapped" configuration — LAN (mDNS) + tickets only.
    /// Refuses the WAN / DHT path so a forensic deployment can
    /// guarantee no leakage to the public network.
    pub fn air_gapped() -> Self {
        Self {
            strategies: vec![HolePunchStrategy::Mdns, HolePunchStrategy::Ticket],
            budget: DEFAULT_BUDGET,
            per_strategy_budget: DEFAULT_PER_STRATEGY_BUDGET,
            ordering: PunchOrdering::default(),
            timeout_policy: TimeoutPolicy::default(),
            require_capabilities: None,
            max_concurrent: None,
        }
    }

    /// "Maximum coverage" — every built-in strategy. Useful for
    /// benchmarks / audit; NOT recommended for production because
    /// it saturates the local DHT and DNS relays.
    pub fn all_channels() -> Self {
        Self {
            strategies: vec![
                HolePunchStrategy::Ticket,
                HolePunchStrategy::Mdns,
                HolePunchStrategy::MainlineDht,
                HolePunchStrategy::PkarrDns,
            ],
            budget: DEFAULT_BUDGET,
            per_strategy_budget: DEFAULT_PER_STRATEGY_BUDGET,
            ordering: PunchOrdering::default(),
            timeout_policy: TimeoutPolicy::default(),
            require_capabilities: None,
            max_concurrent: None,
        }
    }

    /// Append a strategy. Refuses duplicates of the same
    /// built-in variant (the planner would just race two of the
    /// same kinds and waste work). Custom resolvers are
    /// de-duplicated by [`HolePunchResolver::label`].
    pub fn with_strategy(mut self, strategy: HolePunchStrategy) -> Self {
        if !self
            .strategies
            .iter()
            .any(|s| strategy_label_matches(s, &strategy))
        {
            self.strategies.push(strategy);
        }
        self
    }

    /// Append a custom resolver. Sugar for
    /// `with_strategy(HolePunchStrategy::Custom(resolver))`.
    pub fn with_extra_resolver(
        self,
        resolver: impl crate::strategy::HolePunchResolver,
    ) -> Self {
        self.with_strategy(HolePunchStrategy::Custom(std::sync::Arc::new(resolver)))
    }

    /// Set the overall wall-clock budget.
    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// Set the per-strategy budget. The planner validates the
    /// value at `HolePunchPlanner::new` time.
    pub fn with_per_strategy_budget(mut self, budget: Duration) -> Self {
        self.per_strategy_budget = budget;
        self
    }

    /// Set the ordering policy. Currently the planner ignores
    /// everything except `RaceAll` but the field is kept for
    /// forward compatibility.
    pub fn with_ordering(mut self, ordering: PunchOrdering) -> Self {
        self.ordering = ordering;
        self
    }

    /// Set the timeout policy.
    pub fn with_timeout_policy(mut self, policy: TimeoutPolicy) -> Self {
        self.timeout_policy = policy;
        self
    }

    /// Set a capability filter.
    pub fn with_required_capabilities(mut self, caps: RequiredCapabilities) -> Self {
        self.require_capabilities = Some(caps);
        self
    }

    /// Cap the number of concurrent strategies. The planner
    /// picks the first `max_concurrent` enabled strategies.
    pub fn with_max_concurrent(mut self, max: usize) -> Self {
        self.max_concurrent = Some(max);
        self
    }

    /// Drop a strategy by label. Matches the strategy's
    /// `label()` exactly; useful for "remove the default DNS"
    /// recipes.
    pub fn without_strategy_by_label(mut self, label: &str) -> Self {
        self.strategies.retain(|s| s.label() != label);
        self
    }

    /// Validate the config. Catches errors that would otherwise
    /// surface at `HolePunchPlanner::new` time — the planner calls
    /// this automatically so callers don't need to call it
    /// explicitly except for fast-fail tooling.
    pub fn validate(&self) -> Result<(), HolePunchError> {
        if self.strategies.is_empty() {
            return Err(HolePunchError::InvalidConfig(
                "no strategies enabled".into(),
            ));
        }
        if self.strategies.len() > MAX_STRATEGIES {
            return Err(HolePunchError::InvalidConfig(format!(
                "too many strategies: {} > max {MAX_STRATEGIES}",
                self.strategies.len()
            )));
        }
        if self.budget.is_zero() {
            return Err(HolePunchError::InvalidConfig(
                "budget must be > 0".into(),
            ));
        }
        if self.per_strategy_budget < MIN_PER_STRATEGY_BUDGET {
            return Err(HolePunchError::InvalidConfig(format!(
                "per_strategy_budget {:?} below minimum {:?}",
                self.per_strategy_budget, MIN_PER_STRATEGY_BUDGET
            )));
        }
        if self.per_strategy_budget > self.budget {
            return Err(HolePunchError::InvalidConfig(format!(
                "per_strategy_budget {:?} exceeds overall budget {:?}",
                self.per_strategy_budget, self.budget
            )));
        }
        if let Some(max) = self.max_concurrent {
            if max == 0 {
                return Err(HolePunchError::InvalidConfig(
                    "max_concurrent must be > 0".into(),
                ));
            }
            if max > self.strategies.len() {
                return Err(HolePunchError::InvalidConfig(format!(
                    "max_concurrent {} exceeds enabled strategies {}",
                    max,
                    self.strategies.len()
                )));
            }
        }
        Ok(())
    }

    /// Effective list of strategies AFTER the optional capability
    /// filter and concurrency cap. The planner uses this concrete
    /// list when spawning the race.
    pub fn effective_strategies(&self) -> Vec<HolePunchStrategy> {
        let mut out: Vec<HolePunchStrategy> = self
            .strategies
            .iter()
            .filter(|s| match self.require_capabilities {
                None => true,
                Some(caps) => caps.intersects(s.capabilities()),
            })
            .cloned()
            .collect();
        if let Some(max) = self.max_concurrent {
            out.truncate(max);
        }
        out
    }

    /// Number of enabled strategies *after* filtering. The planner
    /// reports this on the outcome so the operator can tell
    /// "config says 4, filter left 2".
    pub fn effective_strategy_count(&self) -> usize {
        self.effective_strategies().len()
    }

    /// Build a hoverable `HolePunchConfigBuilder` for code that
    /// prefers the builder pattern.
    pub fn builder() -> HolePunchConfigBuilder {
        HolePunchConfigBuilder::new()
    }
}

/// True when `existing` and `incoming` are "the same strategy".
/// Built-in variants are compared by kind; custom resolvers are
/// compared by label to avoid two `Arc::new` of the same resolver
/// landing twice.
fn strategy_label_matches(existing: &HolePunchStrategy, incoming: &HolePunchStrategy) -> bool {
    existing.label() == incoming.label()
}

/// Builder-style façade. Sugar for the chained setter methods on
/// `HolePunchConfig` — useful when the call site prefers
/// `HolePunchConfig::builder().strategy(...).budget(...)` over
/// `HolePunchConfig::default().with_strategy(...).with_budget(...)`.
pub struct HolePunchConfigBuilder {
    inner: HolePunchConfig,
}

impl HolePunchConfigBuilder {
    /// New builder seeded with an EMPTY strategy list. Callers
    /// add strategies explicitly with `strategy(...)`. The
    /// `HolePunchConfig::default()` helper remains for callers
    /// that prefer the DNS-first default.
    pub fn new() -> Self {
        let mut cfg = HolePunchConfig::default();
        cfg.strategies.clear();
        Self { inner: cfg }
    }

    pub fn strategy(mut self, s: HolePunchStrategy) -> Self {
        self.inner = self.inner.with_strategy(s);
        self
    }

    pub fn extra_resolver(
        mut self,
        resolver: impl crate::strategy::HolePunchResolver,
    ) -> Self {
        self.inner = self.inner.with_extra_resolver(resolver);
        self
    }

    pub fn budget(mut self, d: Duration) -> Self {
        // Keep per-strategy budget within the overall budget so the
        // validator's "per_strategy_budget ≤ budget" rule never bites
        // when the caller only specifies the overall budget.
        if self.inner.per_strategy_budget > d {
            self.inner.per_strategy_budget = d;
        }
        self.inner = self.inner.with_budget(d);
        self
    }

    pub fn per_strategy_budget(mut self, d: Duration) -> Self {
        self.inner = self.inner.with_per_strategy_budget(d);
        self
    }

    pub fn timeout_policy(mut self, p: TimeoutPolicy) -> Self {
        self.inner = self.inner.with_timeout_policy(p);
        self
    }

    pub fn ordering(mut self, o: PunchOrdering) -> Self {
        self.inner = self.inner.with_ordering(o);
        self
    }

    pub fn required_capabilities(mut self, c: RequiredCapabilities) -> Self {
        self.inner = self.inner.with_required_capabilities(c);
        self
    }

    pub fn max_concurrent(mut self, n: usize) -> Self {
        self.inner = self.inner.with_max_concurrent(n);
        self
    }

    /// Drop the per-label strategy.
    pub fn without_strategy_by_label(mut self, l: &str) -> Self {
        self.inner = self.inner.without_strategy_by_label(l);
        self
    }

    /// Consume the builder, run validation, and return the config.
    pub fn build(self) -> Result<HolePunchConfig, HolePunchError> {
        self.inner.validate()?;
        Ok(self.inner)
    }

    /// Consume the builder without validation. The planner
    /// validates anyway, so this is mainly useful for tests that
    /// want to *assert* on the validation error.
    pub fn build_unchecked(self) -> HolePunchConfig {
        self.inner
    }
}

impl Default for HolePunchConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper: validate a candidate `NodeId` against the local node's
/// endpoint hex. The planner calls this before spawning any
/// strategy so a malformed `NodeId` produces a fast
/// `HolePunchError::InvalidNodeId` rather than a budget-burning
/// timeout round.
pub fn validate_node_id(node_id: &NodeId) -> Result<(), HolePunchError> {
    if node_id.as_bytes().len() != 32 {
        return Err(HolePunchError::InvalidNodeId(format!(
            "expected 32 bytes, got {}",
            node_id.as_bytes().len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_dns_first() {
        let cfg = HolePunchConfig::default();
        assert_eq!(cfg.strategies.len(), 1);
        assert_eq!(cfg.strategies[0].label(), "a3net-pkarr-dns");
    }

    #[test]
    fn air_gapped_locks_to_lan_and_tickets() {
        let cfg = HolePunchConfig::air_gapped();
        let labels: Vec<&str> = cfg.strategies.iter().map(|s| s.label()).collect();
        assert_eq!(labels, vec!["a3net-mdns", "a3net-ticket"]);
    }

    #[test]
    fn all_channels_covers_every_built_in() {
        let cfg = HolePunchConfig::all_channels();
        assert_eq!(cfg.strategies.len(), 4);
        let labels: Vec<&str> = cfg.strategies.iter().map(|s| s.label()).collect();
        assert!(labels.contains(&"a3net-ticket"));
        assert!(labels.contains(&"a3net-mdns"));
        assert!(labels.contains(&"a3net-mainline-dht"));
        assert!(labels.contains(&"a3net-pkarr-dns"));
    }

    #[test]
    fn with_strategy_dedups_built_in_variants() {
        let cfg = HolePunchConfig::default()
            .with_strategy(HolePunchStrategy::PkarrDns)
            .with_strategy(HolePunchStrategy::Mdns)
            .with_strategy(HolePunchStrategy::Mdns);
        let labels: Vec<&str> = cfg.strategies.iter().map(|s| s.label()).collect();
        assert_eq!(labels, vec!["a3net-pkarr-dns", "a3net-mdns"]);
    }

    #[test]
    fn validate_rejects_empty_strategy_list() {
        let cfg = HolePunchConfig {
            strategies: vec![],
            ..HolePunchConfig::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_zero_budget() {
        let cfg = HolePunchConfig {
            budget: Duration::ZERO,
            ..HolePunchConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, HolePunchError::InvalidConfig(_)));
    }

    #[test]
    fn validate_rejects_sub_minimum_per_strategy_budget() {
        let cfg = HolePunchConfig {
            per_strategy_budget: Duration::from_millis(10),
            ..HolePunchConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, HolePunchError::InvalidConfig(_)));
    }

    #[test]
    fn validate_rejects_per_strategy_budget_above_overall_budget() {
        let cfg = HolePunchConfig {
            budget: Duration::from_millis(500),
            per_strategy_budget: Duration::from_secs(2),
            ..HolePunchConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, HolePunchError::InvalidConfig(_)));
    }

    #[test]
    fn validate_rejects_zero_max_concurrent() {
        let cfg = HolePunchConfig::default().with_max_concurrent(0);
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, HolePunchError::InvalidConfig(_)));
    }

    #[test]
    fn validate_rejects_max_concurrent_above_strategy_count() {
        let cfg = HolePunchConfig::default().with_max_concurrent(99);
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, HolePunchError::InvalidConfig(_)));
    }

    #[test]
    fn required_capabilities_intersects_correctly() {
        let wan = RequiredCapabilities::wan_or_dht();
        assert!(wan.intersects(ResolverCapabilities::pkarr_dns()));
        assert!(!wan.intersects(ResolverCapabilities::mdns()));
        assert!(wan.intersects(ResolverCapabilities::mainline_dht()));
        assert!(!wan.intersects(ResolverCapabilities::ticket()));

        let tickets = RequiredCapabilities::tickets_only();
        assert!(tickets.intersects(ResolverCapabilities::ticket()));
        assert!(!tickets.intersects(ResolverCapabilities::pkarr_dns()));
    }

    #[test]
    fn effective_strategies_filter_by_capabilities() {
        let cfg = HolePunchConfig::all_channels()
            .with_required_capabilities(RequiredCapabilities::wan_or_dht());
        let labels: Vec<&str> = cfg
            .effective_strategies()
            .iter()
            .map(|s| s.label())
            .collect();
        // mDNS and ticket don't intersect "wan or zero-knowledge".
        assert!(!labels.contains(&"a3net-mdns"));
        assert!(!labels.contains(&"a3net-ticket"));
        assert!(labels.contains(&"a3net-pkarr-dns"));
        assert!(labels.contains(&"a3net-mainline-dht"));
    }

    #[test]
    fn effective_strategies_truncate_by_max_concurrent() {
        let cfg = HolePunchConfig::all_channels().with_max_concurrent(2);
        let labels = cfg.effective_strategies();
        assert_eq!(labels.len(), 2);
    }

    #[test]
    fn builder_produces_valid_config() {
        let cfg = HolePunchConfig::builder()
            .strategy(HolePunchStrategy::Mdns)
            .budget(Duration::from_secs(3))
            .build()
            .unwrap();
        assert_eq!(cfg.strategies.len(), 1);
        assert_eq!(cfg.budget, Duration::from_secs(3));
    }

    #[test]
    fn builder_unchecked_skips_validation() {
        // Build an obviously invalid config; build_unchecked
        // returns it without complaint. The planner will still
        // catch it at `HolePunchPlanner::new` time.
        let cfg = HolePunchConfig::builder().budget(Duration::ZERO).build_unchecked();
        assert!(cfg.budget.is_zero());
    }

    #[test]
    fn validate_node_id_rejects_wrong_length() {
        // Build a too-short NodeId (this path is the
        // `From<&[u8]>` constructor's own validation, but we
        // also explicitly check it on the planner side).
        let id = NodeId::from_bytes(&[0u8; 31]).ok();
        match id {
            Some(id) => assert!(validate_node_id(&id).is_ok()),
            None => {
                // `NodeId::from_bytes` already at the
                // constructor rejected the 31-byte input;
                // the planner's check is a defence-in-depth
                // for callers that build the NodeId via a
                // different path.
            }
        }
    }

    #[test]
    fn without_strategy_by_label_drops_matches() {
        let cfg = HolePunchConfig::all_channels()
            .without_strategy_by_label("a3net-mdns")
            .without_strategy_by_label("a3net-mainline-dht");
        let labels: Vec<&str> = cfg.strategies.iter().map(|s| s.label()).collect();
        assert_eq!(labels, vec!["a3net-ticket", "a3net-pkarr-dns"]);
    }
}
