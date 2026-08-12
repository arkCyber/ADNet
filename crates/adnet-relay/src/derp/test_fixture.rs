//! Test fixture helpers for the DERP submodule.
//!
//! The `iroh_relay::server::ClientRequest` constructor takes
//! `http::request::Parts`, which is normally produced by Hyper.
//! Tests want a zero-dependency way to build a `ClientRequest` for
//! a given `EndpointId` so they can exercise the access hooks
//! without a live TLS listener. This module centralises that
//! helper so the same builder is used in `mod.rs::tests` and
//! `access.rs::tests` (and any future test that needs it).
//!
//! Production code never reaches into this module — every
//! function is gated behind `#[cfg(test)]` and lives in a
//! `pub(crate)` namespace so it cannot leak into the public
//! API surface.

#![cfg(test)]

use iroh_base::EndpointId;
use iroh_relay::http::ProtocolVersion;
use iroh_relay::server::ClientRequest;

/// Build a minimal [`ClientRequest`] for a given `EndpointId`.
///
/// The `http::request::Parts` we hand back is empty (no headers,
/// no URI, no body) — the access hooks only inspect
/// `ClientRequest::endpoint_id()`, so the exact contents are
/// irrelevant. The empty `Request::default()` path is the
/// cheapest way to materialise a `Parts` value because
/// `http::request::Parts` does not implement `Default`.
pub(crate) fn for_test_endpoint(id: EndpointId) -> ClientRequest {
    let req: http::Request<()> = http::Request::default();
    let (parts, _body) = req.into_parts();
    ClientRequest::new(id, ProtocolVersion::default(), parts)
}
