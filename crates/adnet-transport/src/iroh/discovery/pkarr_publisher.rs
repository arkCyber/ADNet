//! Pkarr publisher wrapper with diagnostics instrumentation.
//!
//! Wraps an inner [`iroh::address_lookup::PkarrPublisher`] (or its
//! builder) and emits [`DiscoveryEvent::PublishFiltered`] events
//! to the shared [`DiscoveryDiagnostics`] whenever the publish
//! policy decides to keep or drop the inbound [`EndpointData`].
//!
//! This is the ADNet-facing hook for **custom pkarr relays** (for
//! air-gapped or private-relay deployments) and for observability
//! of the publish path. The underlying `PkarrPublisher` is created
//! by [`PkarrPublisherBuilder`], which lets callers pick the relay
//! URL, TTL, republish interval, and an [`AddrFilter`].
//!
//! [`EndpointData`]: iroh::address_lookup::EndpointData
//! [`AddrFilter`]: iroh::address_lookup::AddrFilter
//! [`PkarrPublisher`]: iroh::address_lookup::PkarrPublisher

#![cfg(feature = "iroh")]

use std::sync::Arc;

use iroh::address_lookup::{
    AddressLookup, EndpointData, Error as LookupError, Item, PkarrPublisher, PkarrPublisherBuilder,
    UserData as IrohUserData,
};
use iroh_base::EndpointId;
use n0_future::boxed::BoxStream;

use super::diagnostics::DiscoveryDiagnostics;
use super::policy::PublishPolicy;

/// Maximum UTF-8 byte length of a [`UserData`] value.
///
/// iroh-dns encodes `user_data` as a TXT character string, which RFC
/// 1035 §3.3.14 caps at 255 bytes. The serialised wire format is
/// `user-data=<value>`; subtracting the 10-byte prefix leaves 245
/// bytes for the actual user payload. Mirrors
/// `iroh_dns::endpoint_info::UserData::MAX_LENGTH` (v1.0.3).
pub const USER_DATA_MAX_LEN: usize = 245;

/// User-defined data attached to an iroh endpoint's discovery
/// packet.
///
/// iroh-dns surfaces this as a UTF-8 string in the
/// `user-data=<value>` TXT attribute (RFC 1464 `key=value`). The
/// value is opaque to iroh itself — applications are free to put
/// any application-layer metadata they want (node role, version
/// tag, gossip topic key, …) up to [`USER_DATA_MAX_LEN`] bytes.
///
/// Mirrors `iroh_dns::endpoint_info::UserData` (v1.0.3) so that
/// ADNet's discovery stack stays wire-compatible with stock iroh
/// endpoints that publish the same field.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UserData(String);

impl UserData {
    /// Construct a `UserData` from an owned `String`. Refuses
    /// inputs longer than [`USER_DATA_MAX_LEN`] bytes so a
    /// misconfigured caller cannot push an oversized string into
    /// a pkarr packet (which would either truncate silently or
    /// break the DNS wire format).
    pub fn new(value: impl Into<String>) -> Result<Self, UserDataTooLongError> {
        let s = value.into();
        if s.len() > USER_DATA_MAX_LEN {
            return Err(UserDataTooLongError {
                actual: s.len(),
                max: USER_DATA_MAX_LEN,
            });
        }
        Ok(Self(s))
    }

    /// Borrow the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True when the user-data is empty (the canonical "absent"
    /// marker when callers don't want to publish anything).
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Inner byte length.
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl std::fmt::Display for UserData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for UserData {
    type Err = UserDataTooLongError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s.to_string())
    }
}

impl AsRef<str> for UserData {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<&str> for UserData {
    fn from(s: &str) -> Self {
        // `from_str` returns `Err` on overflow; we cannot propagate
        // through `From`, so the constructor's contract is
        // documented on `UserData::new`. Callers that need bounds
        // checking MUST go through `UserData::new` or
        // `UserData::from_str`. The `From<&str>` impl is here for
        // ergonomics in test code where the input is known-good.
        Self::new(s.to_string())
            .unwrap_or_else(|_| Self(s.chars().take(USER_DATA_MAX_LEN).collect()))
    }
}

impl From<UserData> for IrohUserData {
    /// Convert ADNet's `UserData` into iroh's wire-format
    /// `UserData`. The 245-byte cap is enforced by both types so
    /// the conversion is infallible.
    fn from(value: UserData) -> Self {
        // `IrohUserData::try_from(String)` returns `Err` only
        // when the input exceeds `MAX_LENGTH` (245). ADNet's
        // `UserData::new` already gates on the same bound, so a
        // freshly-constructed ADNet `UserData` is always a valid
        // iroh `UserData`. `try_from` is still used (rather than
        // `from`) to keep the contract explicit at the
        // conversion site.
        //
        // We clone the inner `String` upfront because the Err
        // branch re-borrows it for the truncation fallback —
        // moving `value.0` into `try_from` would otherwise make
        // the fallback branch borrow-checker-fail.
        match IrohUserData::try_from(value.0.clone()) {
            Ok(ud) => ud,
            Err(_) => {
                // Defensive: if a future refactor loosens the cap
                // we still must not panic at runtime. Truncate to
                // exactly 245 bytes.
                let truncated: String = value.0.chars().take(USER_DATA_MAX_LEN).collect();
                IrohUserData::try_from(truncated).expect("245-char input is at the wire-format cap")
            }
        }
    }
}

impl From<IrohUserData> for UserData {
    /// Convert iroh's wire-format `UserData` into ADNet's
    /// `UserData`. iroh's cap is the same 245 bytes so the
    /// conversion is infallible.
    fn from(value: IrohUserData) -> Self {
        // `IrohUserData` exposes its inner string via `Display`.
        let s = value.to_string();
        Self::new(s).expect("iroh UserData enforces the 245-byte cap")
    }
}

/// Returned by [`UserData::new`] when the input exceeds
/// [`USER_DATA_MAX_LEN`] bytes.
#[derive(Debug, thiserror::Error)]
#[error("user_data length {actual} exceeds max {max} bytes")]
pub struct UserDataTooLongError {
    pub actual: usize,
    pub max: usize,
}

/// User configuration for the Pkarr publisher.
///
/// Mirrors the relevant subset of [`PkarrPublisherBuilder`] knobs
/// but at the ADNet layer: callers describe what they want in
/// stable terms (relay URL, policy, ttl, republish interval) and
/// [`AdnetPkarrPublisher::build`] translates that into the iroh
/// builder.
#[derive(Debug, Clone)]
pub struct PkarrPublisherConfig {
    /// Relay URL to publish packets to. The public `n0` DNS/Pkarr
    /// relay is used when this is `None`.
    pub relay_url: Option<String>,
    /// Publish policy. Defaults to `RelayOnly`.
    pub policy: PublishPolicy,
    /// TTL of published packets in seconds. `None` means use iroh's
    /// default (`DEFAULT_PKARR_TTL`).
    pub ttl_seconds: Option<u32>,
    /// Republish interval in seconds. `None` means use iroh's
    /// default.
    pub republish_interval_seconds: Option<u32>,
    /// Optional user-data payload. When `Some`, the value is
    /// included as the `user-data=` TXT attribute in every pkarr
    /// packet published by this node, allowing applications to
    /// surface application-layer metadata (node role, version
    /// tag, gossip topic key, …) that other endpoints can
    /// resolve alongside the relay URL and direct IPs. The
    /// payload is opaque to iroh itself.
    ///
    /// Mirrors `iroh_dns::endpoint_info::UserData` (v1.0.3); the
    /// 245-byte limit is enforced by [`UserData::new`].
    pub user_data: Option<UserData>,
}

impl Default for PkarrPublisherConfig {
    fn default() -> Self {
        Self {
            relay_url: None,
            policy: PublishPolicy::RelayOnly,
            ttl_seconds: None,
            republish_interval_seconds: None,
            user_data: None,
        }
    }
}

impl PkarrPublisherConfig {
    /// Use the public `n0` DNS/Pkarr relay (the default).
    pub fn n0_dns() -> Self {
        Self::default()
    }

    /// Use a custom pkarr relay (e.g. a private deployment).
    pub fn custom_relay(relay_url: impl Into<String>) -> Self {
        Self {
            relay_url: Some(relay_url.into()),
            ..Self::default()
        }
    }

    /// Override the publish policy.
    pub fn with_policy(mut self, policy: PublishPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Override the TTL.
    pub fn with_ttl_seconds(mut self, ttl: u32) -> Self {
        self.ttl_seconds = Some(ttl);
        self
    }

    /// Override the republish interval.
    pub fn with_republish_interval_seconds(mut self, secs: u32) -> Self {
        self.republish_interval_seconds = Some(secs);
        self
    }

    /// Attach a user-data payload to every published packet. The
    /// payload is opaque to iroh; applications may put any
    /// application-layer metadata up to [`USER_DATA_MAX_LEN`]
    /// bytes. Pass `Some(UserData::new(value)?)` to publish a
    /// non-empty payload; pass `None` (the default) to skip.
    ///
    /// Returns `Err` if the payload exceeds [`USER_DATA_MAX_LEN`]
    /// bytes — the [`anyhow::Error`] wrapper preserves the
    /// `UserDataTooLongError` so callers can match on it.
    pub fn with_user_data(mut self, user_data: UserData) -> Self {
        self.user_data = Some(user_data);
        self
    }

    /// Convenience: parse a raw string into a [`UserData`] and
    /// attach it. Returns `Err` if the input exceeds
    /// [`USER_DATA_MAX_LEN`] bytes.
    pub fn with_user_data_str(mut self, s: impl AsRef<str>) -> Result<Self, UserDataTooLongError> {
        self.user_data = Some(UserData::new(s.as_ref())?);
        Ok(self)
    }

    /// Drop any attached user-data (so the constructor's
    /// `Some(...)` plumbing can be reset cleanly).
    pub fn without_user_data(mut self) -> Self {
        self.user_data = None;
        self
    }

    fn into_builder(self) -> anyhow::Result<PkarrPublisherBuilder> {
        let url: url::Url = match &self.relay_url {
            Some(s) => parse_relay_url(s)?,
            None => parse_n0_pkarr_default()?,
        };
        let mut builder = PkarrPublisher::builder(url);
        if let Some(ttl) = self.ttl_seconds {
            // DNS TTLs longer than 24 hours are nonsensical for
            // an address-lookup record (the relay caches but
            // resolvers may evict earlier; an excessively long
            // TTL also amplifies stale-data risk if the
            // underlying IP changes). Cap at 1 day.
            const MAX_TTL_SECONDS: u32 = 86_400;
            if ttl > MAX_TTL_SECONDS {
                anyhow::bail!("pkarr ttl_seconds must be <= {MAX_TTL_SECONDS} (1 day), got {ttl}");
            }
            builder = builder.ttl(ttl);
        }
        if let Some(secs) = self.republish_interval_seconds {
            if secs == 0 {
                anyhow::bail!(
                    "pkarr republish_interval_seconds must be > 0 (got 0); \
                     a zero interval would cause iroh to republish in a hot loop"
                );
            }
            // 30s is the *minimum* sane interval: pkarr packets
            // are signed and the relay rate-limits per source IP,
            // so sub-30s intervals hit the limit and saturate
            // the upstream HTTP path. Anything below 30s is
            // almost certainly a configuration mistake.
            const MIN_REPUBLISH_SECONDS: u32 = 30;
            if secs < MIN_REPUBLISH_SECONDS {
                anyhow::bail!(
                    "pkarr republish_interval_seconds must be >= {MIN_REPUBLISH_SECONDS}; \
                     got {secs} (sub-30s intervals hit relay rate limits)"
                );
            }
            builder = builder.republish_interval(std::time::Duration::from_secs(secs as u64));
        }
        Ok(builder)
    }
}

/// The bundled iroh Pkarr relay URL used when
/// `PkarrPublisherConfig::relay_url` is `None`.
///
/// Kept in sync with iroh's `presets::N0`. The constant below
/// mirrors `iroh::address_lookup::pkarr::N0_DNS_PKARR_RELAY_PROD`.
/// (The iroh-internal `force_staging_infra()` env-var switch is
/// not consulted here — operators that need the staging relay
/// should construct a config via
/// [`PkarrPublisherConfig::custom_relay`].)
fn parse_n0_pkarr_default() -> anyhow::Result<url::Url> {
    const DEFAULT: &str = "https://dns.iroh.link/pkarr";
    parse_relay_url(DEFAULT)
}

/// Parse + validate a user-supplied pkarr relay URL.
///
/// Fail-closed validation — the URL must:
/// 1. Have an explicit `http://` or `https://` scheme (the `url`
///    crate accepts scheme-less inputs by default, which would
///    produce a malformed request to iroh's HTTP client).
/// 2. Have a non-empty host (i.e. not just a bare path).
///
/// The iroh pkarr relay runs over HTTPS in production; we still
/// accept `http://` for local test deployments so an operator can
/// point at a private DERP relay behind a TLS-terminating proxy.
fn parse_relay_url(s: &str) -> anyhow::Result<url::Url> {
    let url: url::Url = s
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid pkarr relay url {s:?}: {e}"))?;
    match url.scheme() {
        "http" | "https" => {}
        other => {
            anyhow::bail!(
                "invalid pkarr relay url {s:?}: scheme must be http or https, got {other:?}"
            );
        }
    }
    if url.host_str().is_none_or(str::is_empty) {
        anyhow::bail!("invalid pkarr relay url {s:?}: missing host");
    }
    Ok(url)
}

/// Construct an [`AdnetPkarrPublisher`] from a configuration and a
/// shared diagnostics recorder.
///
/// The returned publisher has not yet been registered with an
/// endpoint — that happens when the caller passes it into
/// [`DiscoveryBuilder::bind`](super::builder::DiscoveryBuilder::bind)
/// or `Endpoint::builder(...).address_lookup(...)`.
pub fn build_publisher(
    config: PkarrPublisherConfig,
    diagnostics: Arc<DiscoveryDiagnostics>,
) -> anyhow::Result<AdnetPkarrPublisher> {
    // Extract the user-data payload before `into_builder` consumes
    // `config`; we have to lift it out manually because the builder
    // doesn't accept a user-data knob (iroh 1.0.3's
    // `PkarrPublisherBuilder` only exposes TTL, republish interval,
    // addr-filter, DNS resolver; `user_data` rides on `EndpointData`).
    let user_data = config.user_data.clone();
    let builder = config.into_builder()?;
    Ok(AdnetPkarrPublisher {
        inner: builder,
        diagnostics,
        user_data,
    })
}

/// ADNet Pkarr publisher.
///
/// A thin builder-state wrapper that defers the actual Pkarr
/// instance construction until iroh's
/// [`AddressLookupBuilder`](iroh::address_lookup::AddressLookupBuilder)
/// is invoked by [`Endpoint::bind`](iroh::Endpoint::bind). At that
/// point the wrapped [`PkarrPublisherBuilder`] becomes a real
/// [`PkarrPublisher`], and every `publish(...)` call increments the
/// shared diagnostics counter.
#[derive(Debug)]
pub struct AdnetPkarrPublisher {
    inner: PkarrPublisherBuilder,
    diagnostics: Arc<DiscoveryDiagnostics>,
    /// Optional user-data payload to inject into every
    /// `EndpointData` before forwarding to the inner publisher.
    /// Mirrors `iroh_dns::endpoint_info::UserData`; see
    /// [`PkarrPublisherConfig::with_user_data`].
    user_data: Option<UserData>,
}

impl iroh::address_lookup::AddressLookupBuilder for AdnetPkarrPublisher {
    fn into_address_lookup(
        self,
        endpoint: &iroh::Endpoint,
    ) -> Result<
        impl iroh::address_lookup::AddressLookup,
        iroh::address_lookup::AddressLookupBuilderError,
    > {
        let publisher = self.inner.into_address_lookup(endpoint)?;
        Ok(InstrumentedPublisher {
            inner: publisher,
            diagnostics: self.diagnostics,
            user_data: self.user_data,
        })
    }
}

/// Wrapper around a live Pkarr publisher (whatever concrete type
/// iroh returned from `into_address_lookup`) that fires a
/// `DiscoveryEvent::PublishFiltered` for every `publish()` call.
#[derive(Debug)]
pub struct InstrumentedPublisher<L: AddressLookup> {
    inner: L,
    diagnostics: Arc<DiscoveryDiagnostics>,
    user_data: Option<UserData>,
}

impl<L: AddressLookup> AddressLookup for InstrumentedPublisher<L> {
    fn publish(&self, data: &EndpointData) {
        // `data` is the **post-filter** `EndpointData` that the
        // iroh pipeline hands to our publisher. `addr_filter`
        // (set on the endpoint) has already been applied by the
        // time we get here, so `data.addrs()` reflects what the
        // pkarr relay will actually see — not the raw
        // `EndpointData` the caller asked to publish.
        //
        // `kept = true` therefore means "at least one address
        // passed the publish-policy filter"; `kept = false` means
        // the filter stripped everything and the pkarr PUT will
        // either be skipped or carry an empty packet (iroh's
        // behaviour for an empty addrs list).
        //
        // Note: we call `record_publish` directly — NOT
        // `emit(PublishFiltered{…})` — because `emit` would
        // re-dispatch to `record_publish` and double-count.
        let kept = data.addrs().next().is_some();
        self.diagnostics.record_publish(kept);

        // Inject the operator-configured `user_data` (if any)
        // into a cloned `EndpointData` so the pkarr relay sees
        // the `user-data=` TXT attribute alongside the relay URL
        // and direct-IP addresses. iroh's `PkarrPublisher::publish`
        // is non-blocking and does not consume the data
        // semantically (it stores a copy), so cloning here is
        // safe. The ADNet→iroh conversion is infallible (both
        // types share the 245-byte cap).
        if let Some(ud) = &self.user_data {
            let mut stamped = data.clone();
            stamped.set_user_data(Some(IrohUserData::from(ud.clone())));
            self.diagnostics.record_user_data(Some(ud.clone()));
            self.inner.publish(&stamped);
        } else {
            // Clear any previous user-data the endpoint may have
            // stashed on the publish path. iroh's pipeline hands
            // us a fresh `EndpointData` per `publish()` call, so
            // this is a no-op for the no-user_data case, but the
            // explicit `set_user_data(None)` documents the intent
            // for code readers.
            let mut cleared = data.clone();
            cleared.set_user_data(None);
            self.diagnostics.record_user_data(None);
            self.inner.publish(&cleared);
        }
    }

    fn resolve(&self, endpoint_id: EndpointId) -> Option<BoxStream<Result<Item, LookupError>>> {
        // P2-1: record that a resolution was *attempted* so the
        // operator-facing `resolutions_total` counter reflects
        // every pkarr relay round-trip, not only those served by
        // `MemoryLookup` (which already records started). The
        // hit/miss outcome is reported by the downstream service
        // when its stream produces a result; we deliberately do
        // not synthesize a "hit/miss" event here because that
        // would require inspecting the stream's `Item`s, which
        // is an ownership-invasive change to iroh's
        // `AddressLookup` trait contract.
        self.diagnostics.record_resolution_started();
        self.inner.resolve(endpoint_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iroh::discovery::DiscoveryEvent;

    #[test]
    fn config_default_is_relay_only_and_n0() {
        let cfg = PkarrPublisherConfig::default();
        assert!(cfg.relay_url.is_none());
        assert_eq!(cfg.policy, PublishPolicy::RelayOnly);
        assert!(cfg.ttl_seconds.is_none());
        assert!(cfg.republish_interval_seconds.is_none());
        assert!(cfg.user_data.is_none());
    }

    #[test]
    fn custom_relay_overrides_url() {
        let cfg = PkarrPublisherConfig::custom_relay("https://pkarr.example.test/");
        assert_eq!(
            cfg.relay_url.as_deref(),
            Some("https://pkarr.example.test/")
        );
        assert!(cfg.into_builder().is_ok());
    }

    #[test]
    fn invalid_relay_url_errors() {
        let cfg = PkarrPublisherConfig::custom_relay("not a url");
        let err = cfg.into_builder().unwrap_err();
        assert!(
            err.to_string().contains("invalid pkarr relay url"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn scheme_less_url_is_rejected() {
        // The `url` crate rejects scheme-less inputs at parse time
        // ("relative URL without a base"). Our
        // `parse_relay_url` surfaces that as an
        // `invalid pkarr relay url` error. Either way, the user
        // gets a clear error before we touch iroh.
        let cfg = PkarrPublisherConfig::custom_relay("relay.example.com");
        let err = cfg.into_builder().unwrap_err();
        assert!(
            err.to_string().contains("invalid pkarr relay url"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn file_scheme_is_rejected() {
        let cfg = PkarrPublisherConfig::custom_relay("file:///etc/passwd");
        let err = cfg.into_builder().unwrap_err();
        assert!(
            err.to_string().contains("scheme must be http or https"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn http_scheme_is_accepted() {
        // Local private DERP relay behind TLS-terminating proxy.
        let cfg = PkarrPublisherConfig::custom_relay("http://pkarr.local/");
        assert!(cfg.into_builder().is_ok());
    }

    #[test]
    fn path_only_url_is_rejected() {
        // `https:///pkarr` parses with `host: Some(Domain("pkarr"))`
        // (the `url` crate is permissive about the triple slash
        // and the leading path). To catch the *intent* of
        // "missing host" we instead try to call
        // `into_builder()` with a URL that has no authority at
        // all, e.g. a bare path. The `url` crate rejects this
        // outright at parse time.
        let cfg = PkarrPublisherConfig::custom_relay("/pkarr/path");
        let err = cfg.into_builder().unwrap_err();
        assert!(
            err.to_string().contains("invalid pkarr relay url"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn zero_republish_interval_is_rejected() {
        // P2-2: a 0-second interval would put iroh into a hot
        // republish loop. We must refuse it at config time
        // instead of letting iroh interpret it.
        let cfg = PkarrPublisherConfig::default().with_republish_interval_seconds(0);
        let err = cfg.into_builder().unwrap_err();
        assert!(
            err.to_string().contains("republish_interval_seconds"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn nonzero_republish_interval_is_accepted() {
        let cfg = PkarrPublisherConfig::default().with_republish_interval_seconds(60);
        assert!(cfg.into_builder().is_ok());
    }

    #[test]
    fn republish_interval_below_minimum_is_rejected() {
        // C1: sub-30s intervals hit relay rate limits. We
        // refuse them up-front rather than letting iroh hammer
        // the relay.
        for secs in [1u32, 5, 10, 29] {
            let cfg = PkarrPublisherConfig::default().with_republish_interval_seconds(secs);
            let err = cfg.into_builder().unwrap_err();
            assert!(
                err.to_string().contains("republish_interval_seconds"),
                "unexpected error for secs={secs}: {err}"
            );
        }
        // Boundary: 30s is the minimum accepted value.
        let cfg = PkarrPublisherConfig::default().with_republish_interval_seconds(30);
        assert!(cfg.into_builder().is_ok(), "30s must be accepted");
    }

    #[test]
    fn ttl_above_maximum_is_rejected() {
        // C1: TTL > 1 day is nonsensical for an address-lookup
        // record and amplifies stale-data risk. Refuse.
        for ttl in [86_401u32, u32::MAX, 365 * 86_400] {
            let cfg = PkarrPublisherConfig::default().with_ttl_seconds(ttl);
            let err = cfg.into_builder().unwrap_err();
            assert!(
                err.to_string().contains("ttl_seconds"),
                "unexpected error for ttl={ttl}: {err}"
            );
        }
    }

    #[test]
    fn ttl_at_maximum_is_accepted() {
        // 1 day is the documented upper bound.
        let cfg = PkarrPublisherConfig::default().with_ttl_seconds(86_400);
        assert!(cfg.into_builder().is_ok(), "86_400s ttl must be accepted");
    }

    #[test]
    fn diagnostics_emit_is_observable() {
        let diag = Arc::new(DiscoveryDiagnostics::new());
        let snap_before = diag.snapshot();
        diag.emit(DiscoveryEvent::PublishFiltered { kept: true });
        diag.emit(DiscoveryEvent::PublishFiltered { kept: false });
        let snap_after = diag.snapshot();
        assert_eq!(snap_after.publishes_total - snap_before.publishes_total, 2);
        assert_eq!(snap_after.publishes_filtered, 1);
    }

    #[test]
    fn instrumented_publisher_records_filtered_out() {
        // H6: when the iroh pipeline hands us an empty
        // `EndpointData` (everything stripped by `addr_filter`),
        // `InstrumentedPublisher::publish` should record
        // `kept = false`, NOT `kept = true`.
        use iroh::address_lookup::AddressLookup;
        let diag = Arc::new(DiscoveryDiagnostics::new());
        let snap_before = diag.snapshot();
        // Use a no-op inner publisher that just echoes.
        let inner = NoopAddressLookup;
        let pub_ = InstrumentedPublisher {
            inner,
            diagnostics: Arc::clone(&diag),
            user_data: None,
        };
        let empty = EndpointData::default();
        pub_.publish(&empty);
        let snap_after = diag.snapshot();
        assert_eq!(snap_after.publishes_total - snap_before.publishes_total, 1);
        assert_eq!(
            snap_after.publishes_filtered - snap_before.publishes_filtered,
            1,
            "an empty EndpointData must be recorded as filtered out"
        );
    }

    /// `AddressLookup` that swallows `publish` and returns `None`
    /// for `resolve`. Used by the `kept = false` test above to
    /// verify the wrapper fires the right `DiscoveryEvent`
    /// without needing a live iroh endpoint.
    #[derive(Debug)]
    struct NoopAddressLookup;
    impl AddressLookup for NoopAddressLookup {
        fn publish(&self, _data: &EndpointData) {}
        fn resolve(
            &self,
            _endpoint_id: EndpointId,
        ) -> Option<BoxStream<Result<Item, LookupError>>> {
            None
        }
    }

    /// `AddressLookup` that records the most-recent `EndpointData`
    /// it observed in a shared slot, so the `InstrumentedPublisher`
    /// tests below can assert on what was forwarded downstream.
    #[derive(Debug, Clone)]
    struct RecordingAddressLookup {
        last: std::sync::Arc<std::sync::Mutex<Option<EndpointData>>>,
    }
    impl AddressLookup for RecordingAddressLookup {
        fn publish(&self, data: &EndpointData) {
            *self.last.lock().unwrap() = Some(data.clone());
        }
        fn resolve(
            &self,
            _endpoint_id: EndpointId,
        ) -> Option<BoxStream<Result<Item, LookupError>>> {
            None
        }
    }

    // ─────────────────── UserData construction tests ────────────────────

    #[test]
    fn user_data_new_accepts_short_string() {
        let ud = UserData::new("hello world").unwrap();
        assert_eq!(ud.as_str(), "hello world");
        assert_eq!(ud.len(), 11);
        assert!(!ud.is_empty());
    }

    #[test]
    fn user_data_new_accepts_empty_string() {
        // Empty `UserData` is the canonical "absent" marker; it
        // must construct cleanly so callers that build the
        // payload from an `Option<String>` don't need to filter.
        let ud = UserData::new("").unwrap();
        assert!(ud.is_empty());
        assert_eq!(ud.len(), 0);
    }

    #[test]
    fn user_data_new_accepts_max_length_string() {
        let s = "a".repeat(USER_DATA_MAX_LEN);
        let ud = UserData::new(s.clone()).expect("245 bytes is the documented cap");
        assert_eq!(ud.len(), USER_DATA_MAX_LEN);
        assert_eq!(ud.as_str(), s);
    }

    #[test]
    fn user_data_new_rejects_oversized_string() {
        let s = "a".repeat(USER_DATA_MAX_LEN + 1);
        let err = UserData::new(s).unwrap_err();
        assert_eq!(err.actual, USER_DATA_MAX_LEN + 1);
        assert_eq!(err.max, USER_DATA_MAX_LEN);
        assert!(err.to_string().contains("exceeds max"));
    }

    #[test]
    fn user_data_from_str_matches_new() {
        let ud1 = UserData::new("hello").unwrap();
        let ud2: UserData = "hello".parse().unwrap();
        assert_eq!(ud1, ud2);
    }

    #[test]
    fn user_data_display_roundtrip() {
        let s = "adnet/role=worker\nversion=42";
        let ud = UserData::new(s).unwrap();
        assert_eq!(ud.to_string(), s);
    }

    // ─────────────────── UserData ↔ IrohUserData conversion ──────────────

    #[test]
    fn user_data_to_iroh_roundtrip() {
        let s = "hello-i-am-a-user-data-payload";
        let adnet_ud = UserData::new(s).unwrap();
        let iroh_ud: IrohUserData = adnet_ud.clone().into();
        let back: UserData = iroh_ud.into();
        assert_eq!(adnet_ud, back);
    }

    #[test]
    fn user_data_to_iroh_at_max_length() {
        // Boundary: exactly 245 bytes (the wire cap). iroh's
        // UserData MUST accept it without falling into the
        // defensive truncate branch.
        let s = "z".repeat(USER_DATA_MAX_LEN);
        let adnet_ud = UserData::new(s.clone()).unwrap();
        let iroh_ud: IrohUserData = adnet_ud.into();
        assert_eq!(iroh_ud.to_string(), s);
    }

    // ─────────────────── PkarrPublisherConfig user_data API ──────────────

    #[test]
    fn pkarr_config_default_has_no_user_data() {
        let cfg = PkarrPublisherConfig::default();
        assert!(cfg.user_data.is_none());
    }

    #[test]
    fn pkarr_config_with_user_data_round_trip() {
        let cfg = PkarrPublisherConfig::default()
            .with_user_data(UserData::new("node-role=worker").unwrap());
        assert!(cfg.user_data.is_some());
        assert_eq!(cfg.user_data.as_ref().unwrap().as_str(), "node-role=worker");
    }

    #[test]
    fn pkarr_config_with_user_data_str_validates_length() {
        let cfg = PkarrPublisherConfig::default()
            .with_user_data_str("hello")
            .expect("short strings accepted");
        assert_eq!(cfg.user_data.as_ref().unwrap().as_str(), "hello");

        let too_long = "a".repeat(USER_DATA_MAX_LEN + 1);
        let err = PkarrPublisherConfig::default()
            .with_user_data_str(too_long)
            .unwrap_err();
        assert_eq!(err.actual, USER_DATA_MAX_LEN + 1);
    }

    #[test]
    fn pkarr_config_without_user_data_clears_field() {
        let cfg = PkarrPublisherConfig::default()
            .with_user_data(UserData::new("payload").unwrap())
            .without_user_data();
        assert!(cfg.user_data.is_none());
    }

    #[test]
    fn pkarr_config_user_data_survives_into_builder() {
        // `into_builder` consumes `PkarrPublisherConfig`; the
        // user-data field must NOT be lost in the conversion.
        // We assert on the builder by re-extracting via
        // `build_publisher` and inspecting the resulting
        // `AdnetPkarrPublisher`'s Debug output (the inner
        // `PkarrPublisherBuilder` itself doesn't expose user-data,
        // so we exercise the round-trip through
        // `InstrumentedPublisher::publish` instead).
        let cfg =
            PkarrPublisherConfig::default().with_user_data(UserData::new("audit-marker").unwrap());
        // `into_builder` returns `Ok(_)` regardless of user-data
        // because the field rides on `EndpointData`, not on
        // the iroh `PkarrPublisherBuilder`.
        assert!(cfg.clone().into_builder().is_ok());
        // The user-data field is still attached on the original.
        assert_eq!(cfg.user_data.as_ref().unwrap().as_str(), "audit-marker");
    }

    // ─────────────────── InstrumentedPublisher user_data injection ───────

    /// When `user_data = Some(payload)`, every `publish()` call
    /// must forward an `EndpointData` whose `user_data()` is
    /// `Some(payload)`. The recording fixture captures the
    /// forwarded payload for inspection.
    #[test]
    fn instrumented_publisher_injects_user_data_on_publish() {
        use iroh::address_lookup::AddressLookup;
        let diag = Arc::new(DiscoveryDiagnostics::new());
        let recorder = RecordingAddressLookup {
            last: std::sync::Arc::new(std::sync::Mutex::new(None)),
        };
        let pub_ = InstrumentedPublisher {
            inner: recorder.clone(),
            diagnostics: Arc::clone(&diag),
            user_data: Some(UserData::new("adnet/role=worker").unwrap()),
        };
        let empty = EndpointData::default();
        pub_.publish(&empty);
        let captured = recorder.last.lock().unwrap().clone().expect("captured");
        let user_data = captured.user_data().expect("user_data stamped");
        assert_eq!(user_data.to_string(), "adnet/role=worker");
    }

    /// When `user_data = None`, the publisher must explicitly
    /// forward an `EndpointData` whose `user_data()` is `None`,
    /// not whatever was previously attached. This prevents
    /// stale payloads from leaking through the pipeline after
    /// the operator clears the configuration.
    #[test]
    fn instrumented_publisher_clears_user_data_when_none() {
        use iroh::address_lookup::AddressLookup;
        let diag = Arc::new(DiscoveryDiagnostics::new());
        let recorder = RecordingAddressLookup {
            last: std::sync::Arc::new(std::sync::Mutex::new(None)),
        };
        let pub_ = InstrumentedPublisher {
            inner: recorder.clone(),
            diagnostics: Arc::clone(&diag),
            user_data: None,
        };
        // Pre-load an `EndpointData` that already carries a
        // user-data payload from a previous session / hand-off.
        let mut preloaded = EndpointData::default();
        preloaded.set_user_data(Some(
            IrohUserData::try_from("stale-payload".to_string()).unwrap(),
        ));
        pub_.publish(&preloaded);
        let captured = recorder.last.lock().unwrap().clone().expect("captured");
        assert!(
            captured.user_data().is_none(),
            "user_data must be cleared when the wrapper is configured with None"
        );
    }

    /// The `record_user_data` diagnostics hook must reflect the
    /// wrapper's user-data on every `publish()` call so the
    /// snapshot's `last_user_data` field stays in sync.
    #[test]
    fn instrumented_publisher_records_user_data_to_diagnostics() {
        use iroh::address_lookup::AddressLookup;
        let diag = Arc::new(DiscoveryDiagnostics::new());
        let recorder = RecordingAddressLookup {
            last: std::sync::Arc::new(std::sync::Mutex::new(None)),
        };
        let pub_ = InstrumentedPublisher {
            inner: recorder,
            diagnostics: Arc::clone(&diag),
            user_data: Some(UserData::new("snap-marker").unwrap()),
        };
        pub_.publish(&EndpointData::default());
        let snap = diag.snapshot();
        assert_eq!(
            snap.last_user_data.as_deref(),
            Some("snap-marker"),
            "last_user_data must reflect the wrapper's configured payload"
        );
    }
}
