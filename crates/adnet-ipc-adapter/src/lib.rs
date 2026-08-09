//! `adnet-ipc-adapter` — exposes an [`adnet_node::Node`] over a JSON-RPC
//! Unix socket so external programs (TUI, Tauri, web frontends, scripts)
//! can drive an ADNet runtime without linking it in-process.
//!
//! Two pieces:
//!
//! - [`NodeRpc`] — implements [`adnet_ipc::RpcHandler`]. Method names
//!   are stable strings (see [`NodeRpc::METHODS`]) and map 1:1 to
//!   [`adnet_node::Node`] public operations.
//! - [`start_daemon`] — convenience: wrap a `Node` in a `NodeRpc`,
//!   bind a Unix socket, and serve until the handle is dropped. Use
//!   the returned `NotificationForwarderHandle` to access the
//!   notifier for ad-hoc pushes.
//!
//! # Method reference
//!
//! | method        | params                                    | returns                    |
//! |---------------|-------------------------------------------|----------------------------|
//! | `init`        | `{}`                                      | `NodeInfo` snapshot        |
//! | `info`        | `{}`                                      | `NodeInfo` snapshot        |
//! | `list_rooms`  | `{}`                                      | `[string]` room ids        |
//! | `join`        | `{room: string}`                          | `{}`                       |
//! | `leave`       | `{room: string}`                          | `{}`                       |
//! | `feed`        | `{room: string}`                          | `RoomFeed` (see adnet_node)|
//! | `announce`    | `{room, file, title?, kind?}`             | `{hash, ticket, sizeBytes}`|
//! | `peers_for`   | `{hash: string}`                          | `[string]` ticket strings  |
//! | `make_ticket` | `{hash: string}`                          | `string` ticket            |
//!
//! # Notification reference
//!
//! The forwarder task spawned by [`start_daemon`] pushes these
//! notifications to every connected client:
//!
//! - `announcement` — params = the serialised
//!   [`adnet_types::Announcement`] plus `room_id`. Emitted on every
//!   remote publish seen by `subscribe_room` (local publishes are
//!   not forwarded — they are already known to the publishing
//!   client).
//!
//! `join` and `leave` are pure state changes; clients that care
//! about room membership should call `list_rooms` after receiving
//! the corresponding `announcement` (or simply re-poll on demand).

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod rpc;

pub use rpc::{NodeRpc, ANNOUNCEMENT_METHOD};

use std::path::PathBuf;

use adnet_ipc::JsonRpcServerHandle;
use adnet_node::Node;
use anyhow::Result;
use tracing::info;

/// Start a daemon: bind `socket_path`, start serving `node` over
/// JSON-RPC, and launch a background task that bridges every joined
/// room's `subscribe_room` stream into server-pushed notifications.
///
/// Returns the running server handle. Drop it to shut the server
/// down; the forwarder task will exit when the broadcast channel is
/// dropped along with the handle.
pub async fn start_daemon(socket_path: PathBuf, node: Node) -> Result<JsonRpcServerHandle> {
    let handler = std::sync::Arc::new(NodeRpc::new(node));
    let handle = adnet_ipc::JsonRpcServer::start(socket_path.clone(), handler.clone())
        .await
        .map_err(|e| anyhow::anyhow!("json-rpc server: {e}"))?;
    handler.serve_with_notifier(handle.notifier()).await;
    info!("adnet daemon listening at {}", socket_path.display());
    Ok(handle)
}
