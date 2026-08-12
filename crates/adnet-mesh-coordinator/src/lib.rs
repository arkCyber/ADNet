//! `adnet-mesh-coordinator` — coordinator-side admission for
//! closed mesh networks.
//!
//! The coordinator is the gatekeeper of a closed mesh network:
//! every admission flows through it. This crate models the
//! coordinator's three responsibilities:
//!
//! 1. **Invite codes** — mint single-use, expiring codes
//!    that admit a new member. The wire format is
//!    `adnet-invite://<network>:<code>`; redeem-time
//!    verification is performed by [`Coordinator::redeem`].
//! 2. **Roster lifecycle** — the coordinator maintains a
//!    signed [`MeshMembership`](adnet_types::MeshMembership)
//!    that grows monotonically. Each admission bumps the
//!    version; each kick / leave / expiry drops a member
//!    and bumps the version again.
//! 3. **Live approval queue** — operators can request to
//!    join a closed network without an invite. The
//!    coordinator enqueues the request and the operator
//!    approves (`accept`) or denies (`deny`) it.
//!
//! ## What this crate does NOT do
//!
//! - It does **not** persist the coordinator state to disk.
//!   The default [`InMemoryCoordinator`] is a thin wrapper
//!   around the in-process state. Production deployments
//!   wire a SQLite-backed store on top of the trait.
//! - It does **not** gossip rosters on its own. The gossip
//!   fan-out layer lives above this crate; this crate only
//!   signs and verifies rosters against the coordinator's
//!   Ed25519 pubkey via [`RosterSigner`] and [`RosterVerifier`].
//!
//! ## Layering
//!
//! ```text
//!   adnet-cli (ray invite / ray requests)
//!                  │
//!                  ▼
//!   adnet-mesh-coordinator  ←  this crate
//!                  │
//!                  ▼
//!   adnet-types (MeshMember / MeshMembership / InviteCode)
//! ```

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod error;
pub mod peering;
pub mod peering_sign;
pub mod request;
pub mod roster_sign;
pub mod store;

pub use error::{CoordinatorError, CoordinatorResult};
pub use peering::{
    InMemoryPeerings, Peerings, PeeringsSnapshot, PeeringDirection, PeeringGrant,
    PeeringGrantId, PeeringRevocation, MAX_PEERING_TTL,
};
pub use peering_sign::{
    CoordinatorPubkeyRegistry, PeeringGrantSigner, PeeringGrantVerifier, StaticPubkeyRegistry,
};
pub use request::{JoinRequest, JoinRequestId, JoinRequestStatus};
pub use roster_sign::{
    verify_with_trait as verify_roster_with_trait, RosterSigner, RosterVerifier,
};
pub use store::{
    Coordinator, CoordinatorConfig, InMemoryCoordinator, MAX_INVITE_TTL, MAX_REQUESTS,
};
