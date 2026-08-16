//! `a3chat-rpc` — JSON-RPC 2.0 server for a3chat.
//!
//! See `README.md` for endpoint list.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod dispatch;
pub mod error;
pub mod metrics;
pub mod server;
pub mod sse;

pub use dispatch::dispatch_rpc_call;
pub use error::RpcError;
pub use metrics::{Metrics, RpcOutcome};
pub use server::{RpcServer, RpcServerConfig, RpcServerHandle};
