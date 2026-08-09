//! `adnet-mesh` — HTTP mesh transport for ADNet blobs.
//!
//! Two pieces:
//!
//! - [`server`]: a tiny `tokio` HTTP server that serves a [`BlobStore`] at
//!   `GET /health`, `GET /blobs/{hash}/meta`, `GET /blobs/{hash}/chunks/{i}`.
//! - [`client`]: parallel chunk-aware fetcher that talks to a [`MeshServer`].
//!
//! The mesh layer is the fallback transport. Higher-performance QUIC / iroh
//! transports live behind the [`Transport`](adnet_transport) trait in
//! `adnet-transport` and are preferred when available.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod client;
pub mod server;

pub use client::{MeshFetchResult, fetch_from_mesh};
pub use server::{MeshServer, MeshServerHandle};
