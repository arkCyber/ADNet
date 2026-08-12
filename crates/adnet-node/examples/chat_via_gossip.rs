//! Two ADNet nodes chatting through a shared `InProcessGossip`.
//!
//! This example is the gossip-flavoured companion to `chat_two_nodes.rs`
//! (which runs over real QUIC). The two files together demonstrate both
//! backends that `adnet-node` can use for "node A sends message X to
//! node B":
//!
//! - `chat_two_nodes.rs` — full QUIC, two real OS processes, arbitrary
//!   framed bytes (any `Frame::text(...)` content).
//! - `chat_via_gossip.rs` — in-process gossip broadcast, **single
//!   process, two named local nodes**, payload limited to
//!   [`adnet_types::Announcement`] (this is what gossip carries).
//!
//! Why "in-process" and not "across two terminals"? The default
//! `InProcessGossip` backend is intentionally a process-local pub/sub
//! bus — it's the unit-test transport. Production cross-process gossip
//! uses `iroh-gossip`'s HyParView+PlumTree overlay (gated behind the
//! `--features iroh` flag in `adnet-gossip`). Since this demo stays on
//! the default build, both nodes live inside the same process and share
//! one `Arc<InProcessGossip>`.
//!
//! Run it with:
//!
//! ```bash
//! cargo run -p adnet-node --example chat_via_gossip
//! ```
//!
//! Both nodes join the `chat-room` topic. The script drives them in a
//! round-trip: alice types, bob receives and replies, and so on, until
//! the max round count is reached or either side types `/quit`.
//!
//! ## Pairing ceremony
//!
//! Before any chat message is published, alice and bob run a full
//! mutual transport-identity verification using `adnet-pairing`:
//!
//!  1. alice generates a `SignedInvitation`, serialises it into the
//!     `chat-room` topic as the very first `Announcement`.
//!  2. bob receives it, decodes the envelope, and replies with a
//!     signed `PairingRequest`.
//!  3. alice verifies the request, signs a `PairingResponse`, and
//!     publishes it.
//!  4. bob verifies the response.
//!  5. Both sides persist a `TrustedDeviceRecord` and the chat loop
//!     only proceeds once both records are on disk.
//!
//! Implementation notes:
//! - We use `InProcessGossip`'s topic-based broadcast: each line sent is
//!   wrapped in an [`Announcement`] with `title = "<node>: <text>"`.
//! - Pairing envelopes ride the same topic as chat lines, distinguished
//!   by an explicit `[pair]` prefix in `title` (so the demo's print
//!   loop can route them to the verifier).
//! - Because `InProcessGossip` is process-local, two terminals would NOT
//!   see each other. To run across terminals, switch to the
//!   `IrohGossipTransport` (`adnet-gossip` with `--features iroh`).

use std::sync::Arc;
use std::time::Duration;

use adnet_gossip::{GossipBus, InProcessGossip};
use adnet_pairing::invitation::SignedInvitation;
use adnet_pairing::{
    capability::CapabilitySet,
    transport_identity::{
        Ed25519Signer, PairingRequest, PairingRequestBuilder, PairingResponse,
        PairingResponseBuilder, derive_credential_id, verify_pairing_request,
        verify_pairing_response,
    },
    trusted_device::{TrustedDeviceRecord, TrustedDeviceRole, TrustedDeviceStatus},
};
use adnet_types::{Announcement as CdnAnnouncement, CdnContentKind, ContentHash, NodeId, RoomId};
use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::broadcast;
use tracing::{Level, info};

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,adnet=info".into()),
        )
        .init();

    let room_id: RoomId = "chat-room".into();
    info!("chat room   : {room_id}");

    // Both nodes share one InProcessGossip — process-local broadcast bus.
    let transport: Arc<InProcessGossip> = Arc::new(InProcessGossip::default());

    let alice_id = NodeId::random();
    let bob_id = NodeId::random();
    info!("alice id    : {}", alice_id.short());
    info!("bob id      : {}", bob_id.short());

    // Alice and bob generate Ed25519 keypairs FIRST; the transport
    // NodeId they use in the pairing protocol is derived from the
    // keypair's public key. This matches `verify_pairing_request`'s
    // invariant that `node_id == transport_pubkey`.
    let alice_signer = Ed25519Signer::generate();
    let bob_signer = Ed25519Signer::generate();
    let alice_id_from_key = NodeId::from_bytes(&alice_signer.public_key())
        .expect("32-byte Ed25519 pubkey always decodes");
    let bob_id_from_key = NodeId::from_bytes(&bob_signer.public_key())
        .expect("32-byte Ed25519 pubkey always decodes");
    if alice_id_from_key.as_bytes() != alice_id.as_bytes() {
        info!("alice id (keypair-derived): {}", alice_id_from_key.short());
    }
    if bob_id_from_key.as_bytes() != bob_id.as_bytes() {
        info!("bob id (keypair-derived):   {}", bob_id_from_key.short());
    }

    let alice_bus = GossipBus::new(alice_id.clone(), transport.clone());
    let bob_bus = GossipBus::new(bob_id.clone(), transport.clone());
    alice_bus.join_room(&room_id).await?;
    bob_bus.join_room(&room_id).await?;

    // Both subscribe before we publish anything, so the demo loop sees
    // every message.
    let mut alice_rx: broadcast::Receiver<CdnAnnouncement> = alice_bus.subscribe(&room_id);
    let mut bob_rx: broadcast::Receiver<CdnAnnouncement> = bob_bus.subscribe(&room_id);

    // --- Pairing ceremony ---------------------------------------------
    // alice generates a SignedInvitation, signs it with a wallet, and
    // publishes it as the first announcement in the room.
    let alice_invitation = SignedInvitation::create(
        &alice_id_from_key,
        &adnet_identity::wallet::Wallet::generate(),
        CapabilitySet::from_names(["chat"]),
        900,
        Some("alice's gossip".into()),
    )
    .context("create alice invitation")?;
    publish_pair_envelope(
        &alice_bus,
        &room_id,
        &alice_id_from_key,
        "invitation",
        &alice_invitation.to_json()?,
    )
    .await?;
    println!(
        "[alice] [pair] published SignedInvitation (issuer={})",
        alice_id_from_key.short()
    );

    // bob picks it up, derives credential_id, and replies with a signed
    // PairingRequest.
    let bob_request_pickup = wait_for_envelope(&mut bob_rx, "invitation").await?;
    let bob_invitation: SignedInvitation =
        SignedInvitation::from_json(&bob_request_pickup).context("bob decode invitation")?;
    bob_invitation
        .verify(Utc::now().timestamp())
        .context("bob verify invitation")?;
    println!(
        "[bob]   [pair] invitation_verified: issuer={} note={:?}",
        bob_invitation.payload.issuer_node_id.short(),
        bob_invitation.payload.note
    );

    let mut bob_salt = [0u8; 32];
    if bob_invitation.payload.salt.len() != 32 {
        anyhow::bail!("invitation salt wrong length");
    }
    bob_salt.copy_from_slice(&bob_invitation.payload.salt);
    let credential_id = derive_credential_id(
        &bob_invitation.payload.issuer_node_id,
        &bob_id_from_key,
        &bob_salt,
    );
    let bob_pub = bob_signer.public_key();
    let bob_request = PairingRequestBuilder {
        credential_id,
        node_id: &bob_id_from_key,
        transport_pubkey: &bob_pub,
        requested_capabilities: bob_invitation.payload.capabilities.clone(),
        ttl_seconds: 60,
    }
    .build(&bob_signer)
    .context("bob build PairingRequest")?;
    publish_pair_envelope(
        &bob_bus,
        &room_id,
        &bob_id,
        "request",
        &serde_json::to_string(&bob_request)?,
    )
    .await?;
    println!(
        "[bob]   [pair] sent PairingRequest (credential_id={})",
        hex::encode(credential_id)
    );

    // alice verifies the request and ships a PairingResponse.
    let alice_request_pickup = wait_for_envelope(&mut alice_rx, "request").await?;
    let alice_seen_request: PairingRequest =
        serde_json::from_str(&alice_request_pickup).context("alice decode request")?;
    verify_pairing_request(&alice_seen_request, Utc::now().timestamp())
        .context("alice verify request")?;
    println!(
        "[alice] [pair] invitee_request_verified: invitee_node={}",
        alice_seen_request.node_id.short()
    );

    let alice_pub = alice_signer.public_key();
    let alice_response = PairingResponseBuilder {
        request: &alice_seen_request,
        issuer_node_id: &alice_id_from_key,
        issuer_pubkey: &alice_pub,
        granted_capabilities: alice_seen_request.requested_capabilities.clone(),
        ttl_seconds: 0,
        issuer_wallet: &adnet_identity::wallet::Wallet::generate(),
    }
    .build()
    .context("alice build PairingResponse")?;
    publish_pair_envelope(
        &alice_bus,
        &room_id,
        &alice_id,
        "response",
        &serde_json::to_string(&alice_response)?,
    )
    .await?;
    println!(
        "[alice] [pair] sent PairingResponse (granted_caps={:?})",
        alice_response.granted_capabilities
    );

    // bob verifies the response and persists its TrustedDeviceRecord.
    let bob_response_pickup = wait_for_envelope(&mut bob_rx, "response").await?;
    let bob_seen_response: PairingResponse =
        serde_json::from_str(&bob_response_pickup).context("bob decode response")?;
    verify_pairing_response(&bob_seen_response, &bob_request, Utc::now().timestamp())
        .context("bob verify response")?;
    let now = Utc::now().timestamp();
    let bob_record = TrustedDeviceRecord {
        credential_id,
        role: TrustedDeviceRole::Invitee,
        device_name: format!("issuer({})", bob_seen_response.issuer_node_id.short()),
        paired_at_unix: now,
        expires_at_unix: bob_seen_response.expires_at_unix,
        last_seen_unix: now,
        node_id: bob_seen_response.issuer_node_id.as_hex().to_string(),
        transport_pubkey: bob_seen_response.issuer_pubkey.clone(),
        wallet_address: None,
        capabilities: bob_seen_response.granted_capabilities.clone(),
        status: TrustedDeviceStatus::Active,
        record_version: 1,
        issuer_node_id: bob_seen_response.issuer_node_id.as_hex().to_string(),
        revoked_at_unix: 0,
    };
    persist_pair_record("bob", &bob_record)?;
    println!(
        "[bob]   [pair] peer_verified: invitee({}) ↔ issuer({})",
        bob_id_from_key.short(),
        bob_seen_response.issuer_node_id.short()
    );

    // alice persists its mirror record.
    let alice_record = TrustedDeviceRecord {
        credential_id,
        role: TrustedDeviceRole::Issuer,
        device_name: format!("invitee({})", alice_seen_request.node_id.short()),
        paired_at_unix: now,
        expires_at_unix: alice_response.expires_at_unix,
        last_seen_unix: now,
        node_id: alice_seen_request.node_id.as_hex().to_string(),
        transport_pubkey: alice_seen_request.transport_pubkey.clone(),
        wallet_address: None,
        capabilities: alice_seen_request.requested_capabilities.clone(),
        status: TrustedDeviceStatus::Active,
        record_version: 1,
        issuer_node_id: alice_id_from_key.as_hex().to_string(),
        revoked_at_unix: 0,
    };
    persist_pair_record("alice", &alice_record)?;
    println!(
        "[alice] [pair] peer_verified: issuer({}) ↔ invitee({})",
        alice_id_from_key.short(),
        alice_seen_request.node_id.short()
    );

    // Drain anything still in flight before we start the chat rounds.
    drain_one(&mut alice_rx);
    drain_one(&mut bob_rx);

    // Drive the scripted round-trip:
    let scripted = [
        (
            alice_bus.clone(),
            alice_id.clone(),
            "alice",
            "hello bob, are you there?",
        ),
        (
            bob_bus.clone(),
            bob_id.clone(),
            "bob",
            "yes alice — got you loud and clear.",
        ),
        (
            alice_bus.clone(),
            alice_id.clone(),
            "alice",
            "great. ship the design doc when you can.",
        ),
        (bob_bus.clone(), bob_id.clone(), "bob", "on its way. /quit"),
    ];

    for (i, (bus, from_id, label, text)) in scripted.iter().enumerate() {
        let ann = CdnAnnouncement {
            room_id: room_id.clone(),
            content_hash: ContentHash::from_bytes(text.as_bytes()),
            node_id: from_id.clone(),
            title: format!("{label}: {text}"),
            kind: CdnContentKind::Article,
            size_bytes: text.len() as u64,
            mime_type: Some("text/plain".into()),
            source_url: None,
            ticket: None,
            timestamp: Utc::now(),
            signer: None,
            signature: None,
        };
        // Publish — both subscribers (including ourselves) will see it.
        bus.publish(&room_id, &ann).await?;

        // Drain both receivers because InProcessGossip broadcasts to every
        // subscriber, including our own `bus`. We only want to render the
        // one that the *other* party sent, plus keep the pipes empty so
        // `recv().await` doesn't lag.
        drain_one(&mut alice_rx);
        drain_one(&mut bob_rx);

        // Then explicitly read whichever side sent it just now, to show
        // the round-trip in real time. The other side's next message
        // arrives in the next iteration.
        let from_other = if label == &"alice" {
            &mut bob_rx
        } else {
            &mut alice_rx
        };
        match tokio::time::timeout(Duration::from_secs(2), from_other.recv()).await {
            Ok(Ok(a)) => println!(
                "[recv on {}] {}",
                if label == &"alice" { "bob" } else { "alice" },
                a.title
            ),
            Ok(Err(e)) => eprintln!("[recv error] {e}"),
            Err(_) => eprintln!("[recv timeout] peer didn't answer in time"),
        }

        println!("[sent from {label}] {text}");
        // Tiny pause so the test feels like an interactive chat.
        tokio::time::sleep(Duration::from_millis(120)).await;

        let _ = i; // round counter
        if text.contains("/quit") {
            break;
        }
    }

    alice_bus.leave_room(&room_id).await?;
    bob_bus.leave_room(&room_id).await?;
    println!("\nALL OK");
    Ok(())
}

/// Pull exactly one frame off the broadcast queue without awaiting — if
/// nothing is buffered yet, just return. Used to drop a node's own echo
/// after `publish` so the subsequent `recv().await` only sees the peer's
/// frame.
fn drain_one(rx: &mut broadcast::Receiver<CdnAnnouncement>) {
    while let Ok(a) = rx.try_recv() {
        // Just discard; we explicitly read the next frame below.
        let _ = a;
    }
    // Also clear any lagged items.
    while let Err(broadcast::error::TryRecvError::Lagged(_)) = rx.try_recv() {}
}

/// Publish a pairing-control envelope onto the shared gossip topic.
/// `kind` is one of "invitation", "request", "response"; the
/// `Announcement::title` is encoded as `[pair] <kind> <payload>` so the
/// other side's `wait_for_envelope` can pull it out unambiguously.
async fn publish_pair_envelope(
    bus: &GossipBus,
    room_id: &RoomId,
    from_id: &NodeId,
    kind: &str,
    payload: &str,
) -> Result<()> {
    let ann = CdnAnnouncement {
        room_id: room_id.clone(),
        content_hash: ContentHash::from_bytes(payload.as_bytes()),
        node_id: from_id.clone(),
        title: format!("[pair] {kind} {payload}"),
        kind: CdnContentKind::Article,
        size_bytes: payload.len() as u64,
        mime_type: Some("application/x-adnet-pairing".into()),
        source_url: None,
        ticket: None,
        timestamp: Utc::now(),
        signer: None,
        signature: None,
    };
    bus.publish(room_id, &ann).await?;
    Ok(())
}

/// Pull the next pairing envelope of `kind` off `rx`, ignoring our own
/// echoes and unrelated chat lines. Returns the JSON payload (the part
/// after `[pair] <kind> `) so the caller can decode it into a typed
/// pairing struct.
async fn wait_for_envelope(
    rx: &mut broadcast::Receiver<CdnAnnouncement>,
    kind: &str,
) -> Result<String> {
    let needle = format!("[pair] {kind} ");
    loop {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Ok(ann)) => {
                if ann.title.starts_with(&needle) {
                    return Ok(ann.title[needle.len()..].to_string());
                }
                // Not for us; skip and keep looking.
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(e)) => return Err(anyhow::anyhow!("recv error: {e}")),
            Err(_) => return Err(anyhow!("timed out waiting for [pair] {kind}")),
        }
    }
}

/// Write a `TrustedDeviceRecord` to $TMPDIR/adnet_gossip_pairing/ so
/// the test harness can assert it exists after the demo runs.
fn persist_pair_record(owner: &str, rec: &TrustedDeviceRecord) -> Result<()> {
    let path = std::env::temp_dir()
        .join("adnet_gossip_pairing")
        .join(format!("{owner}-{}.json", hex::encode(rec.credential_id)));
    std::fs::create_dir_all(path.parent().unwrap())?;
    let json = serde_json::to_string_pretty(rec)?;
    std::fs::write(&path, json)?;
    println!(
        "[{owner}] [pair] trusted_device_record saved to {}",
        path.display()
    );
    Ok(())
}

/// Bonus: a tiny helper if you want to wire stdin into the loop instead
/// of the scripted dialogue above. Currently unused — kept here as a
/// copy-paste hint when porting the demo into your own binary.
///
/// ```ignore
/// async fn drive_stdin_node(rx: broadcast::Receiver<Announcement>, bus: GossipBus, name: &str) -> Result<()> {
///     let stdin = tokio::io::stdin();
///     let mut lines = BufReader::new(stdin).lines();
///     loop {
///         tokio::select! {
///             _ = bus.join_room(&RoomId::new("chat-room")) => {}
///             line = lines.next_line() => {
///                 let line = line?.unwrap_or_default();
///                 if line.trim() == "/quit" { break; }
///                 let ann = Announcement {
///                     room_id: RoomId::new("chat-room"),
///                     content_hash: ContentHash::from_bytes(line.as_bytes()),
///                     node_id: bus.local_node().clone(),
///                     title: format!("{name}: {line}"),
///                     kind: CdnContentKind::Article,
///                     size_bytes: line.len() as u64,
///                     mime_type: Some("text/plain".into()),
///                     source_url: None, ticket: None,
///                     timestamp: chrono::Utc::now(),
///                     signer: None, signature: None,
///                 };
///                 bus.publish(&RoomId::new("chat-room"), &ann).await?;
///             }
///             ann = rx.recv() => {
///                 if let Ok(a) = ann {
///                     println!("[recv] {}", a.title);
///                 }
///             }
///         }
///     }
///     Ok(())
/// }
/// ```
#[allow(dead_code)]
fn _doc_stdin_helper_unused() {
    use std::io::Write as _;
    let _ = BufReader::new(tokio::io::stdin()).lines();
    let _ = std::io::stdout().flush();
}
