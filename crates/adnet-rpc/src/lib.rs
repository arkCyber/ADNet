//! `adnet-rpc` — Unified RPC API compatible with IPFS HTTP API.
//!
//! This crate provides a unified RPC interface that is compatible with the IPFS
//! HTTP API specification. It can be used both as an HTTP API (via the gateway)
//! and as a local RPC interface.
//!
//! ## Supported API Commands
//!
//! | Command | Category | Description |
//! |---------|----------|-------------|
//! | `dag/put` | DAG | Add a DAG node |
//! | `dag/get` | DAG | Get a DAG node |
//! | `dag/resolve` | DAG | Resolve a DAG path |
//! | `dag/import` | DAG | Import a UnixFS DAG |
//! | `block/put` | Block | Add a raw block |
//! | `block/get` | Block | Get a raw block |
//! | `block/stat` | Block | Get block statistics |
//! | `block/rm` | Block | Remove a block |
//! | `pin/add` | Pin | Pin content |
//! | `pin/rm` | Pin | Unpin content |
//! | `pin/ls` | Pin | List pins |
//! | `gc` | GC | Run garbage collection |
//! | `dht/findprovs` | DHT | Find providers |
//! | `dht/provide` | DHT | Announce provider |
//! | `name/publish` | IPNS | Publish IPNS record |
//! | `name/resolve` | IPNS | Resolve IPNS name |

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod commands;
pub mod results;
pub mod client;

pub use commands::{
    dag_put, dag_get, block_put, block_get, block_stat, block_rm,
    pin_add, pin_rm, pin_ls, gc, node_id, version,
};
pub use results::{RpcResult, RpcError};
