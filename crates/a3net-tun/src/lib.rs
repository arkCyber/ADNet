//! `a3net-tun` — cross-platform TUN device abstraction for the
//! A3Net mesh VPN.
//!
//! ## Layering
//!
//! The crate exposes a single trait — [`TunDevice`] — and three
//! implementations:
//!
//! - [`UserspaceTun`] (always available) — a tokio-channel-backed
//!   virtual TUN. Packets "received from the kernel" are
//!   pushed in via [`UserspaceTun::inject_from_kernel`]; packets
//!   read via [`TunDevice::recv`] are routed into the mesh
//!   stack. Outbound packets — written via
//!   [`TunDevice::send`] — are queued and drained via
//!   [`UserspaceTun::drain_to_kernel`].
//!
//!   This is the default backend. It is what tests use, what CI
//!   uses, and what an unprivileged A3Net process uses. The
//!   rest of the mesh stack (firewall, exit-node routing,
//!   coordinator) is built against the trait, never against a
//!   concrete backend, so it remains backend-agnostic.
//!
//! - [`NativeTun`] (with `--features native`) — backed by the
//!   `tun` crate. Opens a real OS TUN device. macOS / iOS use
//!   `utun`; Linux uses `/dev/net/tun`; Windows uses wintun.
//!
//! - Tests can also construct a fresh `UserspaceTun` to simulate
//!   a full kernel ↔ userspace loop without any privileged
//!   state.
//!
//! ## Why a trait, not a single concrete type
//!
//! Splitting the abstraction makes three properties testable:
//!
//! 1. The mesh stack can be unit-tested without root (CI never
//!    opens a real TUN).
//! 2. The packet round-trip path is exercised end-to-end in
//!    tests without flakiness from OS-level state.
//! 3. New platforms (Android, FreeBSD) can plug in by
//!    implementing the trait without touching the rest of the
//!    workspace.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod device;
pub mod error;
pub mod packet;
pub mod userspace;

#[cfg(feature = "native")]
pub mod native;

pub use device::{DeviceInfo, DeviceState, TunDevice};
pub use error::{TunError, TunResult};
pub use packet::{
    IPV4_HEADER_MIN, IPV6_HEADER_MIN, IpProtocol, IpVersion, ParsedPacket, parse_packet,
    packet_to_bytes,
};
pub use userspace::{UserspaceTun, UserspaceTunConfig};
