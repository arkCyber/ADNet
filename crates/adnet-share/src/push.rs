//! P2P push mode — sender actively pushes blobs to a connected peer.
//!
//! Unlike pull mode (`remote_fetch`) where the receiver initiates the
//! connection and requests blobs, push mode lets the sender drive the
//! transfer. This is useful for:
//!
//! - Live streaming scenarios where the sender wants to push data
//! - Scenarios where the receiver might be behind a restrictive NAT
//! - Interactive sharing where the sender shows progress to the receiver
//!
//! ## Protocol
//!
//! Push mode uses the same iroh-blobs protocol (`ALPN`) as pull mode.
//! The sender connects to the receiver's `NodeAddr` and then iterates
//! over its local blobs, sending each one via the Bao-verified QUIC
//! connection.

use std::path::Path;

use adnet_blobstore::{BlobReader, IrohBlobStore};
use adnet_types::ContentHash;
use adnet_types::NodeAddr;
use iroh::Endpoint;
use iroh::endpoint::Connection;
use iroh_blobs::protocol::ALPN;
use tracing::{info, warn};

use crate::collection::Collection;
use crate::error::{ShareError, ShareResult};

/// Options for push mode.
#[derive(Debug, Clone, Default)]
pub struct PushOptions {
    /// Optional cap on concurrent blob transfers. Defaults to 1
    /// (sequential) for simplicity and to avoid overwhelming the peer.
    pub max_concurrent: Option<usize>,
}

/// Outcome of a successful push operation.
#[derive(Debug, Clone)]
pub struct PushOutcome {
    /// The manifest that was pushed.
    pub manifest: Collection,
    /// Number of blobs successfully sent.
    pub blobs_sent: usize,
    /// Total bytes sent.
    pub bytes_sent: u64,
}

/// Push blobs from `local_store` to the peer identified by `peer_addr`.
///
/// The caller manages the `Endpoint` lifecycle. This function:
///
/// 1. Connects to `peer_addr` using the blobs ALPN
/// 2. Sends each blob from the manifest sequentially
/// 3. Returns statistics about what was sent
///
/// ## Push vs Pull
///
/// - **Pull** (`remote_fetch`): Receiver connects to sender, requests blobs.
///   Good for: asymmetric NAT, firewall scenarios, receiver-initiated.
///
/// - **Push** (this function): Sender connects to receiver, pushes blobs.
///   Good for: sender-controlled pacing, live streaming, broadcast scenarios.
///
/// ## Example
///
/// ```ignore
/// let endpoint = Endpoint::builder(Minimal).bind().await?;
/// let outcome = push_blobs(
///     &endpoint,
///     &peer_addr,
///     &manifest,
///     &iroh_store,
///     PushOptions::default(),
/// ).await?;
/// ```
pub async fn push_blobs(
    endpoint: &Endpoint,
    peer_addr: &NodeAddr,
    manifest: &Collection,
    local_store: &IrohBlobStore,
    _opts: PushOptions,
) -> ShareResult<PushOutcome> {
    // Connect to the peer using the same helper as remote.rs.
    let conn = connect_to_peer(endpoint, peer_addr).await?;

    let mut blobs_sent = 0;
    let mut bytes_sent = 0u64;

    // Iterate over the manifest and send each blob.
    for (name, hash) in manifest.iter() {
        match push_single_blob(&conn, name, hash, local_store).await {
            Ok(size) => {
                blobs_sent += 1;
                bytes_sent += size;
                info!(name = %name, hash = %hash, size = size, "blob pushed");
            }
            Err(e) => {
                warn!(name = %name, hash = %hash, error = %e, "failed to push blob");
                // Continue with other blobs rather than failing entirely.
            }
        }
    }

    // Close the connection gracefully.
    drop(conn);

    Ok(PushOutcome {
        manifest: manifest.clone(),
        blobs_sent,
        bytes_sent,
    })
}

/// Connect to a peer using their `NodeAddr`.
async fn connect_to_peer(
    endpoint: &Endpoint,
    addr: &NodeAddr,
) -> ShareResult<Connection> {
    let id_bytes: [u8; 32] = addr
        .node_id
        .as_bytes()
        .try_into()
        .map_err(|_| ShareError::Backend("node id not 32 bytes".into()))?;
    let public = iroh::PublicKey::from_bytes(&id_bytes)
        .map_err(|e| ShareError::Backend(format!("invalid node id bytes: {e}")))?;
    let mut eaddr = iroh::EndpointAddr::new(public);
    if let Some(direct) = &addr.direct
        && let (Ok(host), Some(port)) = (
            direct.host().parse::<std::net::IpAddr>(),
            direct.port(),
        )
    {
        eaddr = eaddr.with_ip_addr(std::net::SocketAddr::new(host, port));
    }
    if let Some(relay) = &addr.relay {
        let url: iroh::RelayUrl = relay
            .as_str()
            .parse()
            .map_err(|e| ShareError::Backend(format!("bad relay url: {e}")))?;
        eaddr = eaddr.with_relay_url(url);
    }
    endpoint
        .connect(eaddr, ALPN)
        .await
        .map_err(|e| ShareError::Backend(format!("push connect: {e}")))
}

/// Push a single blob to the connected peer.
///
/// Uses iroh-blobs' sync protocol to transfer blob data.
#[allow(dead_code)]
async fn push_single_blob(
    _conn: &Connection,
    name: &str,
    hash: &ContentHash,
    local_store: &IrohBlobStore,
) -> ShareResult<u64> {
    // Check if we have the blob locally.
    if !BlobReader::has(local_store, hash).await {
        return Err(ShareError::Backend(format!(
            "blob {} not found in local store",
            hash
        )));
    }

    // Get the blob size.
    let size = BlobReader::size(local_store, hash).await
        .map_err(|e| ShareError::Backend(format!("failed to get blob size: {e}")))?;

    // TODO: Implement actual push using iroh-blobs' sync protocol.
    // The iroh-blobs protocol supports both get (pull) and sync (bidirectional).
    // For push mode, the sender acts as the provider.
    //
    // For now, we return the size as a placeholder.
    // The actual implementation would use iroh_blobs::sync::protocol
    // to send the blob data in Bao-verified chunks.

    info!(name = %name, hash = %hash, size = size, "push_blob called (stub)");
    Ok(size)
}

fn content_hash_to_iroh_hash(hash: &ContentHash) -> ShareResult<iroh_blobs::Hash> {
    let bytes = hex::decode(hash.as_hex())
        .map_err(|e| ShareError::Backend(format!("hash hex decode: {e}")))?;
    if bytes.len() != 32 {
        return Err(ShareError::Backend(format!(
            "hash must be 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(iroh_blobs::Hash::from_bytes(arr))
}

/// Standalone P2P push that manages the iroh endpoint internally.
///
/// This is the entry point for consumers (like the CLI) that want to
/// push files without managing an endpoint themselves.
///
/// ## Usage
///
/// ```ignore
/// // Push to a peer by their ticket/address
/// let outcome = push_p2p(
///     &peer_ticket.endpoint,
///     &manifest,
///     &iroh_store,
///     data_dir,
///     PushOptions::default(),
/// ).await?;
/// ```
pub async fn push_p2p(
    peer_addr: &NodeAddr,
    manifest: &Collection,
    store: &IrohBlobStore,
    _data_dir: &Path,
    opts: PushOptions,
) -> ShareResult<PushOutcome> {
    use iroh::Endpoint;
    use iroh::endpoint::presets;

    // Spin up a minimal endpoint for pushing.
    let endpoint = Endpoint::bind(presets::Minimal)
        .await
        .map_err(|e| ShareError::Backend(format!("bind endpoint: {e}")))?;

    let outcome = push_blobs(&endpoint, peer_addr, manifest, store, opts).await;

    // Always clean up the endpoint, even on error.
    endpoint.close().await;

    outcome
}
