//! `a3net-rpc-irpc::service` — irpc service definition mirroring
//! the IPFS-compatible command surface in `a3net-rpc`.
//!
//! ## Layout (mirrors `crates/a3net-rpc/src/commands.rs`)
//!
//! | a3net-rpc function | irpc variant              | Channel shape           |
//! |--------------------|---------------------------|-------------------------|
//! | `dag_put`          | `DagPut`                  | oneshot `<DagResult>`   |
//! | `dag_get`          | `DagGet`                  | oneshot `<Vec<u8>>`     |
//! | `dag_resolve`      | `DagResolve`              | oneshot `<String>`      |
//! | `dag_import`       | `DagImport`               | rx-streaming `void → DagResult` |
//! | `block_put`        | `BlockPut`                | oneshot `<BlockResult>` |
//! | `block_get`        | `BlockGet`                | oneshot `<Vec<u8>>`     |
//! | `block_stat`       | `BlockStat`               | oneshot `<BlockStat>`   |
//! | `block_rm`         | `BlockRm`                 | oneshot `<BlockRmResult>` |
//! | `pin_add`          | `PinAdd`                  | oneshot `<PinAddResult>` |
//! | `pin_rm`           | `PinRm`                   | oneshot `<PinRmResult>` |
//! | `pin_ls`           | `PinLs`                   | tx-streaming `PinLsResult` |
//! | `gc`               | `Gc`                      | tx-streaming `GcStats` (drained per cycle) |
//! | `node_id`          | `NodeId`                  | oneshot `<NodeInfo>`    |
//! | `version`          | `Version`                 | oneshot `<String>`      |
//!
//! ## Why one big `Protocol` enum and not a per-command enum
//!
//! irpc's `rpc_requests` macro collapses every variant into a single
//! `Protocol` enum + a flat `*Message` enum (one message per variant)
//! that lives next to the protocol. Splitting into per-command
//! enums would require re-implementing dispatch by hand; the macro
//! is the entire value proposition of the crate.

#![allow(clippy::large_enum_variant)]

use irpc::{channel::oneshot, rpc_requests};
use serde::{Deserialize, Serialize};

// ─── Result structs ────────────────────────────────────────────────────────────
//
// Shapes intentionally mirror `crates/a3net-rpc/src/results.rs`. We do
// not `pub use a3net_rpc::results::*` because that would force the
// irpc crate to depend on `a3net-rpc`, which in turn pulls
// `a3net-blobstore` and the full workspace runtime into scope. The
// whole point of this crate is to stay externally observable without
// touching the workspace dependency graph.

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct DagResult {
    pub cid: String,
    pub size: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct BlockResult {
    pub key: String,
    pub size: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct BlockStat {
    pub key: String,
    pub size: u64,
    pub cid: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct BlockRmResult {
    pub removed: bool,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct PinAddResult {
    pub pins: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct PinRmResult {
    pub pins: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct PinLsResult {
    pub cid: String,
    pub r#type: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct GcStats {
    pub blocks_removed: u64,
    pub bytes_freed: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct NodeInfo {
    pub id: String,
    pub addresses: Vec<String>,
}

// ─── Protocol ────────────────────────────────────────────────────────────────

/// The full IPFS-compatible command surface, expressed as a single
/// irpc protocol enum. Each variant that returns a result attaches a
/// typed oneshot channel via the `tx =` attribute.
#[rpc_requests(message = AdnetRpcMessage)]
#[derive(Debug, Serialize, Deserialize)]
pub enum Protocol {
    // ── DAG ──────────────────────────────────────────────────────────
    /// `dag_put` — store a unixfs node, return CID + size.
    #[rpc(tx = oneshot::Sender<DagResult>)]
    #[wrap(DagPut)]
    DagPut { data: Vec<u8> },

    /// `dag_get` — fetch a DAG node by CID. Returns raw bytes
    /// (mirrors `dag_get` in `commands.rs` which returns a base64
    /// blob in JSON; the client wrapper can decode).
    #[rpc(tx = oneshot::Sender<Vec<u8>>)]
    #[wrap(DagGet)]
    DagGet { cid: String, path: Option<String> },

    /// `dag_resolve` — resolve an IPFS path to a CID.
    #[rpc(tx = oneshot::Sender<String>)]
    #[wrap(DagResolve)]
    DagResolve { path: String },

    /// `dag_import` — import a unixfs DAG from a stream of bytes.
    /// rx-streaming shape (server reads chunks until EOF).
    #[rpc(rx = tokio::sync::mpsc::Receiver<Vec<u8>>, tx = oneshot::Sender<DagResult>)]
    #[wrap(DagImport)]
    DagImport,

    // ── Block ────────────────────────────────────────────────────────
    #[rpc(tx = oneshot::Sender<BlockResult>)]
    #[wrap(BlockPut)]
    BlockPut { data: Vec<u8> },

    #[rpc(tx = oneshot::Sender<Vec<u8>>)]
    #[wrap(BlockGet)]
    BlockGet { cid: String },

    #[rpc(tx = oneshot::Sender<BlockStat>)]
    #[wrap(BlockStat)]
    BlockStat { cid: String },

    #[rpc(tx = oneshot::Sender<BlockRmResult>)]
    #[wrap(BlockRm)]
    BlockRm { cid: String, force: bool },

    // ── Pin ──────────────────────────────────────────────────────────
    #[rpc(tx = oneshot::Sender<PinAddResult>)]
    #[wrap(PinAdd)]
    PinAdd { cid: String, r#type: Option<String> },

    #[rpc(tx = oneshot::Sender<PinRmResult>)]
    #[wrap(PinRm)]
    PinRm { cid: String, r#type: Option<String> },

    /// `pin_ls` — list pinned CIDs. tx-streaming because the pin set
    /// can be arbitrarily large.
    #[rpc(tx = tokio::sync::mpsc::Sender<PinLsResult>)]
    #[wrap(PinLs)]
    PinLs { cid: Option<String>, r#type: Option<String> },

    // ── GC / node / version ──────────────────────────────────────────
    /// `gc` — drain a stream of `GcStats` updates as the GC sweep
    /// progresses. tx-streaming (server → client).
    #[rpc(tx = tokio::sync::mpsc::Sender<GcStats>)]
    #[wrap(Gc)]
    Gc { dry_run: bool },

    #[rpc(tx = oneshot::Sender<NodeInfo>)]
    #[wrap(NodeId)]
    NodeId,

    #[rpc(tx = oneshot::Sender<String>)]
    #[wrap(Version)]
    Version,
}
