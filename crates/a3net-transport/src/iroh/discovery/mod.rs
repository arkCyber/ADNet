//! Address-discovery surface for the iroh-backed transport.
//!
//! iroh 1.0 ships a pluggable [`AddressLookup`] mechanism for
//! publishing / resolving addressing information (relay URL +
//! optional direct IP addresses). [`iroh::endpoint::presets::N0`]
//! already wires `PkarrPublisher::n0_dns` + `PkarrResolver::n0_dns`
//! + `DnsAddressLookup::n0_dns`, which covers the public DNS/Pkarr
//! path. This module layers **composable, auditable** controls on top
//! of that default:
//!
//! | Knob | What it does | Default |
//! |------|--------------|---------|
//! | [`PublishPolicy`] | Filter addresses before they are published to *any* lookup (relay-only by default — never leak direct IPs) | [`PublishPolicy::RelayOnly`] |
//! | [`MemoryLookup`] | In-process out-of-band address book (e.g. from `PeerTicket`s) | disabled |
//! | [`MainlineLookup`] | Mainline-DHT address lookup via the `pkarr` crate's built-in DHT backend | disabled |
//! | [`DiscoveryBuilder::with_extra_lookup`] | Hook for callers that want to drop in `iroh-mdns-address-lookup` or any custom `AddressLookup` | n/a |
//!
//! Diagnostics surface:
//! - [`DiscoveryDiagnostics`] tracks per-event counters
//!   (publishes / resolutions / publish policy filter pass-through)
//! - [`DiscoveryBuilder::into_snapshot`] produces an
//!   [`IrohDiscoverySnapshot`] suitable for `/discovery` admin output
//!
//! [`AddressLookup`]: iroh::address_lookup::AddressLookup

#![cfg(feature = "iroh")]

pub mod builder;
pub mod diagnostics;
pub mod lookup;
#[cfg(feature = "mdns")]
pub mod mdns;
pub mod pkarr_publisher;
pub mod policy;
pub mod wire_helpers;

pub use builder::{DiscoveryBuilder, DiscoveryConfig};
pub use diagnostics::{DiscoveryDiagnostics, DiscoveryEvent, IrohDiscoverySnapshot};
pub use lookup::{MainlineLookup, MemoryLookup};
#[cfg(feature = "mdns")]
pub use mdns::{
    MDNS_PROVENANCE, MdnsAddressLookup, collect_events, node_id_to_endpoint_id,
    MdnsMetrics, MdnsMetricsSnapshot, PeerCache, DiscoveredPeer,
    MdnsHealthCheck, MdnsHealthStatus, MdnsFailureRecovery, MdnsRecoveryConfig,
    RecoveryState,
    MDNS_SERVICE_NAME, MDNS_PORT, MDNS_MULTICAST_V4, MDNS_MULTICAST_V6,
    MAX_PEER_CACHE_SIZE, DEFAULT_PEER_TTL_SECS,
};
pub use pkarr_publisher::{
    AdnetPkarrPublisher, PkarrPublisherConfig, USER_DATA_MAX_LEN, UserData, UserDataTooLongError,
    build_publisher,
};
pub use policy::PublishPolicy;
pub use wire_helpers::{TxtAddrParseError, parse_transport_addr};
