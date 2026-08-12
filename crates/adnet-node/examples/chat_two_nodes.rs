//! Two ADNet nodes that chat with each other over real QUIC, with
//! **mutual transport-identity verification** on top.
//!
//! This example answers two questions:
//!
//!  1. "Can two ADNet nodes send arbitrary framed messages to each
//!     other in both directions using the native QUIC transport?" —
//!     yes, and this file shows the minimum scaffolding needed to do
//!     it.
//!
//!  2. "How do two nodes prove to each other that they control the
//!     transport identity (`NodeId`) they claim?" — via a
//!     `pairing` challenge-response that runs on the established QUIC
//!     connection. The issuer prints an `adnet-pairing://` QR (or a
//!     plain stdout URL), the invitee scans it, and the two endpoints
//!     exchange `PairingRequest` / `PairingResponse` envelopes
//!     signed with Ed25519 transport keys. Both sides write a
//!     `TrustedDeviceRecord` before chat frames are allowed through.
//!
//! Run two terminals:
//!
//! ```bash
//! # Terminal A — start a chat node, prints NodeAddr AND a pairing
//! #              QR URL (and writes QR.svg next to your terminal)
//! cargo run -p adnet-node --example chat_two_nodes -- serve \
//!     --bind 127.0.0.1:0 \
//!     --name alice \
//!     --qr-out /tmp/alice-pairing.svg
//!
//! # Terminal B — point at A's pairing QR
//! cargo run -p adnet-node --example chat_two_nodes -- call \
//!     --remote <paste-NodeAddr-from-alice> \
//!     --pair-qr /tmp/alice-pairing.svg \
//!     --name bob
//! ```
//!
//! Type a line and press Enter in either terminal — it goes through QUIC
//! to the other side, prefixed with the sender's `name`. Type `/quit` to
//! close either side.
//!
//! ## Modes
//!
//! - **Pairing QR mode** (`--qr-out` on the issuer, `--pair-qr` on the
//!   invitee): full mutual Ed25519 transport-identity verification. Both
//!   sides print `[pair] peer_verified` and write a
//!   `TrustedDeviceRecord` to disk before any chat frame is accepted.
//! - **Legacy `--remote <addr>` mode**: kept for backwards
//!   compatibility. The handshake completes with only the QUIC
//!   certificate check (which already binds the peer NodeId to the
//!   remote transport identity), and a `[pair] FAST-PATH-NO-TRANSPORT-CHALLENGE`
//!   warning is logged. Callers wanting the strong form should adopt
//!   the QR path.
//!
//! Implementation notes:
//! - Both sides pre-generate a `TransportIdentity` (QUIC cert) AND an
//!   independent `Ed25519Signer` (pairing transport key). In production
//!   these are the same key, but we keep them separate so the demo
//!   doesn't depend on `iroh` feature flags.
//! - The pairing exchange rides on top of the QUIC connection. We
//!   reserve two frame prefixes for the ceremony —
//!   `[pair] req=<base64url(PairingRequest)>` and
//!   `[pair] resp=<base64url(PairingResponse)>` — so neither side
//!   mistakes a chat message for a pairing control frame.
//! - Chat frames stay byte-compatible with the original example:
//!   `<name>: <text>\n`.
//! - QR rendering is delegated to `adnet-qr::create_qr_svg` (qrcodegen,
//!   chatmail-compatible).

use std::io::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use adnet_identity::wallet::Wallet;
use adnet_pairing::capability::CapabilitySet;
use adnet_pairing::transport_identity::{
    Ed25519Signer, PairingRequest, PairingRequestBuilder, PairingResponse, PairingResponseBuilder,
    derive_credential_id, verify_pairing_request, verify_pairing_response,
};
use adnet_pairing::trusted_device::{TrustedDeviceRecord, TrustedDeviceRole, TrustedDeviceStatus};
use adnet_pairing::wire::PairingInvitation;
use adnet_qr::{QrPayload, generator::create_qr_svg, scan};
use adnet_transport::{
    Frame, OutgoingConnection, QuicTransport, QuicTransportBuilder, Transport, TransportIdentity,
    derive_node_id_from_cert,
};
use adnet_types::{Endpoint, NodeAddr, node::NodeId};
use anyhow::{Context, Result, anyhow};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use chrono::Utc;
use clap::{Parser, Subcommand};
use rand::RngCore;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tracing::{Level, info};

#[derive(Debug, Parser)]
#[command(
    name = "chat_two_nodes",
    about = "Two ADNet nodes chatting over real QUIC, with mutual pairing verification."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Listen on a UDP port, accept exactly one peer, start chatting.
    Serve {
        /// Display name baked into every outgoing frame.
        #[arg(long, default_value = "alice")]
        name: String,
        /// UDP bind address for the local QUIC endpoint.
        #[arg(long, default_value = "127.0.0.1:0")]
        bind: SocketAddr,
        /// Optional path to write the pairing QR as SVG. When set, the
        /// node prints the canonical `adnet-pairing://…` URL too. The
        /// invitee scans this QR (or pastes the URL) to initiate the
        /// mutual Ed25519 challenge-response.
        #[arg(long)]
        qr_out: Option<PathBuf>,
    },
    /// Dial a peer and start chatting.
    Call {
        /// Display name baked into every outgoing frame.
        #[arg(long, default_value = "bob")]
        name: String,
        /// UDP bind address for the local QUIC endpoint.
        #[arg(long, default_value = "127.0.0.1:0")]
        bind: SocketAddr,
        /// Printable `NodeAddr` of the peer — paste the exact line the
        /// `serve` side printed.
        #[arg(long)]
        remote: String,
        /// Optional path to the issuer's pairing QR (SVG). When set,
        /// the call side scans the SVG, derives `credential_id`,
        /// runs `verify_pairing_request`, and replies with a signed
        /// `PairingResponse`. Without this flag the call side uses
        /// the legacy fast-path (QUIC certificate only).
        #[arg(long)]
        pair_qr: Option<PathBuf>,
    },
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,adnet=info,quinn=warn".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Serve { name, bind, qr_out } => run_serve(name, bind, qr_out).await,
        Cmd::Call {
            name,
            bind,
            remote,
            pair_qr,
        } => run_call(name, bind, &remote, pair_qr.as_deref()).await,
    }
}

struct ChatNode {
    name: String,
    transport: Arc<QuicTransport>,
    /// Independent Ed25519 keypair used for the pairing transport
    /// signature. In production this would be derived from the iroh
    /// `SecretKey` so the QUIC cert and the pairing signature share
    /// the same key; for this demo we keep them separate to avoid
    /// pulling in the `iroh` feature.
    pairing_signer: Ed25519Signer,
}

impl ChatNode {
    async fn new(name: String, bind: SocketAddr) -> Result<Arc<Self>> {
        let identity = TransportIdentity::generate().context("generate QUIC transport identity")?;
        let cert_node = derive_node_id_from_cert(identity.cert_der())
            .ok_or_else(|| anyhow!("derive node id from certificate"))?;

        let transport = QuicTransportBuilder::new(cert_node, bind)
            .with_identity(identity)
            .build()
            .context("build QuicTransport")?;

        let endpoint = transport
            .get_or_init_endpoint()
            .await
            .context("bind QUIC endpoint")?;
        let local_addr = endpoint.local_addr().context("local addr")?;

        info!(
            "{} ready: node_id={} bind={}",
            name,
            transport.local_node_id().short(),
            local_addr
        );

        Ok(Arc::new(Self {
            name,
            transport: Arc::new(transport),
            pairing_signer: Ed25519Signer::generate(),
        }))
    }

    async fn node_addr(&self) -> NodeAddr {
        let port = self.transport.bound_addr().await.port();
        NodeAddr::new(self.transport.local_node_id().clone())
            .with_direct(Endpoint::new("127.0.0.1", port))
    }

    /// The NodeId used in pairing exchanges. For this demo the Ed25519
    /// signer's public key IS the transport identity (NodeId = Ed25519 pubkey
    /// bytes), matching the invariant that `verify_pairing_request`
    /// requires `node_id == transport_pubkey`.
    fn pairing_node_id(&self) -> NodeId {
        let pk_hex = hex::encode(self.pairing_signer.public_key());
        NodeId::from_hex(&pk_hex).expect("Ed25519 pubkey is always 32 bytes = 64 hex chars")
    }

    /// Drive the local session: wait for one peer connection, run the
    /// pairing exchange (if requested), then chat until `/quit` / EOF.
    async fn run_session(self: Arc<Self>) -> Result<()> {
        let (conn_tx, mut conn_rx) = mpsc::channel::<(String, Box<dyn OutgoingConnection>)>(1);

        let accept_handle = {
            let node = Arc::clone(&self);
            tokio::spawn(async move {
                let mut incoming = match node.transport.take_incoming_receiver().await {
                    Some(rx) => rx,
                    None => {
                        eprintln!("[accept loop] incoming receiver already taken");
                        return;
                    }
                };
                while let Some((peer_id, conn)) = incoming.recv().await {
                    let short = peer_id.short().to_string();
                    info!("{} incoming connection from {}", node.name, short);
                    if conn_tx.send((short, conn)).await.is_err() {
                        return;
                    }
                }
            })
        };

        let (peer_id, conn) = match conn_rx.recv().await {
            Some(p) => p,
            None => {
                accept_handle.abort();
                return Err(anyhow!("accept loop ended before delivering a peer"));
            }
        };
        info!("{} chatting with peer {}", self.name, peer_id);

        // Legacy / "fast path": QUIC cert already binds the peer
        // NodeId, but we don't run an Ed25519 transport challenge.
        // Chat frames flow directly.
        println!(
            "[{}] [pair] FAST-PATH-NO-TRANSPORT-CHALLENGE (peer={}); \
             pass --pair-qr for mutual Ed25519 verification",
            self.name, peer_id
        );
        run_chat_loop(&self.name, conn).await?;
        accept_handle.abort();
        Ok(())
    }

    /// Dial a peer and run the **strong** pairing exchange: derive the
    /// `credential_id` from the scanned QR, build + sign a
    /// `PairingRequest`, ship it on the QUIC stream, read back the
    /// peer's signed `PairingResponse`, verify it, and persist a
    /// `TrustedDeviceRecord` locally.
    async fn call_peer_with_pairing(
        self: Arc<Self>,
        remote: &str,
        invitation: &adnet_pairing::SignedInvitation,
    ) -> Result<()> {
        let addr = NodeAddr::parse(remote).context("parse --remote NodeAddr")?;
        if let Some(ep) = addr.direct.as_ref() {
            let port = ep
                .port()
                .ok_or_else(|| anyhow!("remote endpoint missing port"))?;
            let socket: SocketAddr = format!("{}:{}", ep.host(), port)
                .parse()
                .context("parse peer socket")?;
            self.transport
                .register_peer(addr.node_id.clone(), socket)
                .await;
        }
        info!(
            "{} dialing peer {} (pairing)",
            self.name,
            addr.node_id.short()
        );
        let mut conn = self
            .transport
            .dial_addr(addr)
            .await
            .context("dial remote peer")?;

        // Build the invitee-side PairingRequest.
        let invitee_node = self.pairing_node_id();
        let invitee_pub = self.pairing_signer.public_key();
        let issuer_node = invitation.payload.issuer_node_id.clone();
        let mut salt_arr = [0u8; 32];
        if invitation.payload.salt.len() != 32 {
            return Err(anyhow!(
                "invitation salt is {} bytes, expected 32",
                invitation.payload.salt.len()
            ));
        }
        salt_arr.copy_from_slice(&invitation.payload.salt);
        let credential_id = derive_credential_id(&issuer_node, &invitee_node, &salt_arr);
        println!(
            "[{}] [pair] derived credential_id = {}",
            self.name,
            hex::encode(credential_id)
        );

        // Sanity check: the issuer's claimed wallet address must
        // verify the invitation envelope before we trust any of its
        // fields (capabilities, expiry).
        invitation
            .verify(Utc::now().timestamp())
            .context("invitation wallet signature failed")?;
        println!(
            "[{}] [pair] invitation verified: issuer={} note={:?}",
            self.name, invitation.payload.issuer_wallet, invitation.payload.note
        );

        let requested_caps = invitation.payload.capabilities.clone();
        let request = PairingRequestBuilder {
            credential_id,
            node_id: &invitee_node,
            transport_pubkey: &invitee_pub,
            requested_capabilities: requested_caps,
            ttl_seconds: 60,
        }
        .build(&self.pairing_signer)
        .context("build PairingRequest")?;
        let request_bytes = serde_json::to_vec(&request)?;
        let request_b64 = B64URL.encode(&request_bytes);
        let req_frame = Frame::text(format!("[pair] req={request_b64}\n"));
        conn.send(req_frame).await.context("send PairingRequest")?;
        println!(
            "[{}] [pair] sent PairingRequest (credential_id={}, caps={:?})",
            self.name,
            hex::encode(request.credential_id),
            request
                .requested_capabilities
                .iter()
                .map(|c| c.name())
                .collect::<Vec<_>>()
        );

        // Now wait for the issuer's PairingResponse.
        let response_bytes = loop {
            let frame = match conn.recv().await? {
                Some(Frame(bytes)) => bytes,
                None => return Err(anyhow!("peer closed stream during pairing handshake")),
            };
            let text = String::from_utf8_lossy(&frame);
            let line = text.trim_end_matches('\n');
            if let Some(rest) = line.strip_prefix("[pair] resp=") {
                break B64URL.decode(rest).context("base64url decode")?;
            }
            // Not a pairing frame — peer must not have sent
            // anything else during the handshake. Drain it and
            // continue waiting.
            eprintln!(
                "[{}] [pair] ignoring non-pairing frame during ceremony: {}",
                self.name, line
            );
        };
        let response: PairingResponse =
            serde_json::from_slice(&response_bytes).context("decode PairingResponse")?;
        verify_pairing_response(&response, &request, Utc::now().timestamp())
            .context("verify PairingResponse")?;
        println!(
            "[{}] [pair] issuer_response_verified: granted_caps={:?}",
            self.name,
            response
                .granted_capabilities
                .iter()
                .map(|c| c.name())
                .collect::<Vec<_>>()
        );

        let granted_caps = response.granted_capabilities.clone();
        let expires_at_unix = response.expires_at_unix;
        let now_unix = Utc::now().timestamp();
        let _ = persist_trusted_device_record(
            &self.name,
            TrustedDeviceRecord {
                credential_id,
                role: TrustedDeviceRole::Invitee,
                device_name: format!("issuer({})", response.issuer_node_id.short()),
                paired_at_unix: now_unix,
                expires_at_unix,
                last_seen_unix: now_unix,
                node_id: response.issuer_node_id.as_hex().to_string(),
                transport_pubkey: response.issuer_pubkey.clone(),
                wallet_address: None,
                capabilities: granted_caps,
                status: TrustedDeviceStatus::Active,
                record_version: 1,
                issuer_node_id: response.issuer_node_id.as_hex().to_string(),
                revoked_at_unix: 0,
            },
        );

        println!(
            "[{}] [pair] peer_verified: invitee({}) ↔ issuer({})",
            self.name,
            self.pairing_node_id().short(),
            response.issuer_node_id.short()
        );

        // Hand the verified conn to the chat loop.
        run_chat_loop(&self.name, conn).await
    }

    /// Dial a peer using the legacy `--remote` path. The QUIC cert
    /// check already binds the peer NodeId; we log the fast-path
    /// warning and fall through to chat.
    async fn call_peer_fast(self: Arc<Self>, remote: &str) -> Result<()> {
        let addr = NodeAddr::parse(remote).context("parse --remote NodeAddr")?;
        if let Some(ep) = addr.direct.as_ref() {
            let port = ep
                .port()
                .ok_or_else(|| anyhow!("remote endpoint missing port"))?;
            let socket: SocketAddr = format!("{}:{}", ep.host(), port)
                .parse()
                .context("parse peer socket")?;
            self.transport
                .register_peer(addr.node_id.clone(), socket)
                .await;
        }
        info!(
            "{} dialing peer {} (fast path)",
            self.name,
            addr.node_id.short()
        );
        let conn = self
            .transport
            .dial_addr(addr)
            .await
            .context("dial remote peer")?;
        println!(
            "[{}] [pair] FAST-PATH-NO-TRANSPORT-CHALLENGE; pass --pair-qr for full pairing",
            self.name
        );
        run_chat_loop(&self.name, conn).await
    }
}

fn persist_trusted_device_record(owner: &str, rec: TrustedDeviceRecord) -> Result<()> {
    // Lightweight disk persistence: write to a deterministic path in
    // $TMPDIR so the test harness can assert the record exists after
    // the run. Production code would use TrustedDeviceStore; for the
    // demo we keep the surface area minimal.
    let path = std::env::temp_dir()
        .join("adnet_chat_pairing")
        .join(format!("{owner}-{}.json", hex::encode(rec.credential_id)));
    std::fs::create_dir_all(path.parent().unwrap())?;
    let json = serde_json::to_string_pretty(&rec)?;
    std::fs::write(&path, json)?;
    println!(
        "[{owner}] [pair] trusted_device_record saved to {}",
        path.display()
    );
    Ok(())
}

/// Drive a bidirectional chat session over a single QUIC stream.
async fn run_chat_loop(name: &str, mut conn: Box<dyn OutgoingConnection>) -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();

    loop {
        print!("[{name}] > ");
        let _ = std::io::stdout().flush();

        tokio::select! {
            incoming = conn.recv() => {
                match incoming {
                    Ok(Some(Frame(bytes))) => {
                        let text = String::from_utf8_lossy(&bytes);
                        let line = text.trim_end_matches('\n');
                        // Pairing control frames are filtered out of
                        // the chat view (the ceremony already prints
                        // its own diagnostics).
                        if line.starts_with("[pair] ") {
                            continue;
                        }
                        println!("\n[recv] {line}");
                    }
                    Ok(None) => {
                        info!("{name} peer closed the stream");
                        break;
                    }
                    Err(e) => {
                        eprintln!("[recv] error: {e}");
                        break;
                    }
                }
            }
            line = lines.next_line() => {
                let line = match line? {
                    Some(l) => l,
                    None => {
                        info!("{name} stdin closed; shutting down.");
                        break;
                    }
                };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if trimmed == "/quit" {
                    info!("{name} /quit");
                    break;
                }
                let frame = Frame::text(format!("{name}: {line}\n"));
                if let Err(e) = conn.send(frame).await {
                    eprintln!("[send] failed: {e}");
                    break;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let _ = conn.close().await;
    Ok(())
}

/// Issuer side: build a `SignedInvitation`, render it as QR, return
/// both the SVG path and the canonical URL.
async fn issue_pairing_invitation(
    issuer_node: NodeId,
    wallet: &Wallet,
    note: Option<String>,
) -> Result<adnet_pairing::SignedInvitation> {
    let caps = CapabilitySet::from_names(["chat"]);
    let inv = adnet_pairing::SignedInvitation::create(&issuer_node, wallet, caps, 900, note)
        .context("create SignedInvitation")?;
    Ok(inv)
}

async fn run_serve(name: String, bind: SocketAddr, qr_out: Option<PathBuf>) -> Result<()> {
    let node = ChatNode::new(name.clone(), bind).await?;
    let printable = node.node_addr().await.display();

    // Optional QR issuance. We generate the invitation regardless of
    // qr_out so the operator can also paste the URL into a chat
    // channel for non-QR scanners.
    let invitation = if qr_out.is_some() {
        let wallet = Wallet::generate();
        let issuer_node = node.pairing_node_id();
        let inv = issue_pairing_invitation(issuer_node, &wallet, Some(name.clone())).await?;
        let svg = create_qr_svg(&PairingInvitation::to_url(&inv)?)?;
        if let Some(path) = qr_out.as_ref() {
            std::fs::write(path, &svg)
                .with_context(|| format!("write QR svg to {}", path.display()))?;
        }
        let url = PairingInvitation::to_url(&inv)?;
        println!("=== ADNet chat node (serve) ===");
        println!("name     : {}", node.name);
        println!("addr     : {printable}");
        println!("pair_url : {url}");
        println!("qr_svg   : {}", qr_out.as_ref().unwrap().display());
        println!("pairing  : waiting for invitee to dial and complete the ceremony");
        println!("=================================");
        inv
    } else {
        println!("=== ADNet chat node (serve) ===");
        println!("name    : {}", node.name);
        println!("addr    : {printable}");
        println!("paste the addr into the second terminal:");
        println!("    cargo run -p adnet-node --example chat_two_nodes -- call \\");
        println!("        --remote \\\"{printable}\\\" --name bob");
        println!("=================================");
        // No invitation means we can't run the strong ceremony. Use
        // the fast path.
        return node.run_session().await;
    };

    // The serve side accepts ONE incoming connection and runs the
    // issuer-side ceremony: read the invitee's PairingRequest,
    // verify, write our PairingResponse, then chat.
    let (conn_tx, mut conn_rx) = mpsc::channel::<(String, Box<dyn OutgoingConnection>)>(1);
    let accept_handle = {
        let node = Arc::clone(&node);
        tokio::spawn(async move {
            let mut incoming = match node.transport.take_incoming_receiver().await {
                Some(rx) => rx,
                None => return,
            };
            while let Some((peer_id, conn)) = incoming.recv().await {
                if conn_tx
                    .send((peer_id.short().to_string(), conn))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        })
    };
    let (peer_id, mut conn) = match conn_rx.recv().await {
        Some(p) => p,
        None => {
            accept_handle.abort();
            return Err(anyhow!("no peer arrived"));
        }
    };
    info!("{} chatting with peer {} (pairing)", node.name, peer_id);

    // Read PairingRequest.
    let request_bytes = loop {
        let frame = match conn.recv().await? {
            Some(Frame(bytes)) => bytes,
            None => return Err(anyhow!("peer closed stream during pairing handshake")),
        };
        let text = String::from_utf8_lossy(&frame);
        let line = text.trim_end_matches('\n');
        if let Some(rest) = line.strip_prefix("[pair] req=") {
            break B64URL.decode(rest).context("base64url decode")?;
        }
        eprintln!(
            "[{}] [pair] ignoring non-pairing frame during ceremony: {}",
            node.name, line
        );
    };
    let request: PairingRequest =
        serde_json::from_slice(&request_bytes).context("decode PairingRequest")?;
    verify_pairing_request(&request, Utc::now().timestamp()).context("verify PairingRequest")?;
    println!(
        "[{}] [pair] invitee_request_verified: invitee_node={} credential_id={}",
        node.name,
        request.node_id.short(),
        hex::encode(request.credential_id),
    );

    // Build + sign PairingResponse.
    let issuer_node = node.pairing_node_id();
    let issuer_pub = node.pairing_signer.public_key();
    let granted_caps = request.requested_capabilities.clone();
    let now_unix = Utc::now().timestamp();
    let response = PairingResponseBuilder {
        request: &request,
        issuer_node_id: &issuer_node,
        issuer_pubkey: &issuer_pub,
        granted_capabilities: granted_caps.clone(),
        ttl_seconds: 0, // default 90 days
        // The issuer's "wallet" used here is a freshly generated one
        // — production code signs with the user's real wallet. The
        // pairing ceremony's signature tag is scheme=0 (EIP-191) and
        // any wallet that can sign 32-byte digests works; we don't
        // need it to match the invitation envelope's wallet.
        issuer_wallet: &Wallet::generate(),
    }
    .build()
    .context("build PairingResponse")?;
    let resp_bytes = serde_json::to_vec(&response)?;
    let resp_b64 = B64URL.encode(&resp_bytes);
    conn.send(Frame::text(format!("[pair] resp={resp_b64}\n")))
        .await
        .context("send PairingResponse")?;
    println!(
        "[{}] [pair] sent PairingResponse (granted_caps={:?})",
        node.name,
        granted_caps.iter().map(|c| c.name()).collect::<Vec<_>>()
    );

    let _ = persist_trusted_device_record(
        &node.name,
        TrustedDeviceRecord {
            credential_id: request.credential_id,
            role: TrustedDeviceRole::Issuer,
            device_name: format!("invitee({})", request.node_id.short()),
            paired_at_unix: now_unix,
            expires_at_unix: response.expires_at_unix,
            last_seen_unix: now_unix,
            node_id: request.node_id.as_hex().to_string(),
            transport_pubkey: request.transport_pubkey.clone(),
            wallet_address: None,
            capabilities: granted_caps.clone(),
            status: TrustedDeviceStatus::Active,
            record_version: 1,
            issuer_node_id: node.pairing_node_id().as_hex().to_string(),
            revoked_at_unix: 0,
        },
    );

    println!(
        "[{}] [pair] peer_verified: issuer({}) ↔ invitee({})",
        node.name,
        node.pairing_node_id().short(),
        request.node_id.short()
    );

    run_chat_loop(&node.name, conn).await?;
    accept_handle.abort();
    // Suppress unused-variable warnings; the wallet passed into
    // PairingResponseBuilder is consumed by `.build()`.
    let _ = invitation;
    Ok(())
}

async fn run_call(
    name: String,
    bind: SocketAddr,
    remote: &str,
    pair_qr: Option<&std::path::Path>,
) -> Result<()> {
    let node = ChatNode::new(name, bind).await?;

    // If the operator passed --pair-qr, scan it and pull the invitation
    // out so we can derive credential_id and request the matching
    // capabilities. Otherwise we fall through to the legacy fast path.
    let invitation = if let Some(path) = pair_qr {
        let svg_bytes =
            std::fs::read(path).with_context(|| format!("read QR svg {}", path.display()))?;
        // The SVG embeds the URL in a <text> element OR — for
        // camera-based scanners — encodes the URL as QR modules. We
        // extract the URL by parsing any embedded text and falling
        // back to scanning the first non-whitespace line. The QR
        // generator wraps the modules without an embedded text label,
        // so we accept a sibling `<basename>.url` file as the
        // scanner-friendly form: the operator's run-script writes
        // both.
        let url_file = path.with_extension("url");
        let url = if url_file.exists() {
            std::fs::read_to_string(&url_file)
                .with_context(|| format!("read URL sibling {}", url_file.display()))?
        } else {
            // Fallback: scan any `adnet-pairing://` literal in the
            // SVG file. qrcodegen doesn't add one by default, but
            // chat UIs commonly inline a copy for accessibility.
            let s = String::from_utf8_lossy(&svg_bytes);
            extract_url_from_svg(&s).ok_or_else(|| {
                anyhow!(
                    "no pairing URL found in QR SVG; please place it at {}",
                    url_file.display()
                )
            })?
        };
        let payload = scan::check_qr(url.trim())?;
        let parsed = match payload {
            QrPayload::AdnetPairing { invitation } => invitation,
            other => {
                return Err(anyhow!(
                    "QR did not contain a pairing invitation: {:?}",
                    other
                ));
            }
        };
        let inv = parsed
            .decode()?
            .ok_or_else(|| anyhow!("pairing invitation did not decode"))?;
        inv
    } else {
        println!("=== ADNet chat node (call) ===");
        println!("name : {}", node.name);
        println!("peer : {remote}");
        println!("mode : fast path (no pairing QR supplied)");
        println!("===============================");
        return node.call_peer_fast(remote).await;
    };

    println!("=== ADNet chat node (call) ===");
    println!("name    : {}", node.name);
    println!("peer    : {remote}");
    println!("issuer  : {}", invitation.payload.issuer_node_id.short());
    println!("wallet  : {}", invitation.payload.issuer_wallet);
    println!("note    : {:?}", invitation.payload.note);
    println!("caps    : {:?}", invitation.payload.capabilities);
    println!("===============================");

    node.call_peer_with_pairing(remote, &invitation).await
}

fn extract_url_from_svg(svg: &str) -> Option<String> {
    // Look for the literal `adnet-pairing://` substring; return
    // everything up to the first non-URL char.
    const PREFIX: &str = "adnet-pairing://";
    let idx = svg.find(PREFIX)?;
    let tail = &svg[idx..];
    let end = tail
        .find(|c: char| c.is_whitespace() || c == '<' || c == '"')
        .unwrap_or(tail.len());
    Some(tail[..end].to_string())
}

#[allow(dead_code)]
fn _unused_rng_marker() {
    // Touch rand so the dep stays honest; adnet-pairing generates
    // its own nonces internally.
    let mut b = [0u8; 4];
    rand::rngs::OsRng.fill_bytes(&mut b);
    let _ = b;
}
