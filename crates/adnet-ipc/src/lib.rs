//! `adnet-ipc` — JSON-RPC over Unix sockets.
//!
//! Ported from
//! `Exodus@src-backup/.../microservice/{p2p_blobs_service.rs,
//! p2p_gossip_service.rs, gossip_client.rs, group_chat_client.rs}`.
//!
//! Two faces:
//! - [`client`] — generic [`json_rpc_call`] for sending one JSON-RPC request
//!   over a Unix socket.
//! - [`server`] — [`JsonRpcServer`] which dispatches incoming requests to a
//!   user-supplied [`RpcHandler`].
//!
//! On top of those primitives sit [`blobs_service`] and [`gossip_service`]
//! — typed wrappers that mirror the Exodus `P2pBlobsService` /
//! `P2pGossipService` microservices but compose cleanly with `adnet-types`
//! primitives.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod blobs_service;
pub mod client;
pub mod gossip_service;
pub mod group_chat_service;
pub mod server;
pub mod validation;

pub use blobs_service::{BlobsIpcConfig, BlobsIpcService};
pub use client::{json_rpc_call, json_rpc_stream, JsonRpcError, StreamItem};
pub use gossip_service::{GossipIpcConfig, GossipIpcMessage, GossipIpcService};
pub use group_chat_service::{
    attachment_with_hash, group_attachment_from_hash, message_id_for_node, GroupChatIpcConfig,
    GroupChatIpcService, MessageEnvelope, Receipt,
};
pub use server::{
    JsonRpcServer, JsonRpcServerHandle, Notification, NotificationSender, RpcHandler,
};
pub use validation::{Validate, ValidationOutcome, ValidationPolicy};
