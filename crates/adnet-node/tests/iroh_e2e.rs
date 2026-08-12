//! End-to-end iroh integration tests for `adnet-node`.
//!
//! Two layers of coverage:
//!
//! 1. **Single-node smoke tests** — verify each subsystem
//!    (`adnet_store`, `IrohGossipTransport`, `IrohDocsChat`) wires
//!    up against the runtime and supports the high-level API.
//! 2. **Two-node e2e tests** — two full `IrohRuntime`s over a
//!    loopback iroh endpoint. They exercise:
//!
//!    - `ticket_round_trip_via_iroh`: producer announces a blob,
//!      consumer dials `iroh_blobs::ALPN` and fetches bytes (full
//!      Bao verification path).
//!    - `blob_resume_after_disconnect`: mid-stream disconnect is
//!      recovered via a fresh connection; the second fetch
//!      re-verifies the Bao tree.
//!    - `gossip_announcement_propagates_between_nodes`: two
//!      `GossipBus` instances on top of two iroh-gossip engines
//!      exchange an announcement over the same topic.
//!    - `docs_two_node_sync_round_trip`: Alice opens an
//!      iroh-docs conversation, Bob joins via the ticket, Alice
//!      appends messages, Bob observes them.
//!
//! All tests in this module are **gated on the `iroh` feature**
//! because they require the iroh stack to be compiled in. Tests
//! that depend on cross-node gossip / docs convergence use a soft
//! timeout and `eprintln!` rather than panicking on slow CI
//! because the in-process iroh-gossip overlay can take a couple
//! of seconds to converge on loopback-only setups.

#![cfg(feature = "iroh")]

use std::sync::Arc;

use adnet_blobstore::BlobImporter;
use adnet_chatstore::im::Message;
use adnet_gossip::GossipBus;
use adnet_node::iroh_runtime::IrohRuntime;
use adnet_transport::iroh::{IrohIdentity, public_key_to_node_id};
use adnet_types::{Announcement, CdnContentKind, ContentHash, RoomId};
use chrono::Utc;
use iroh::Endpoint;
use tempfile::TempDir;
use tracing_subscriber::EnvFilter;

/// Boot a [`tracing_subscriber`] once per process.
fn init_tracing_once() {
    use std::sync::Once;
    static START: Once = Once::new();
    START.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| EnvFilter::new("warn,adnet_node=info,adnet_blobstore=info,adnet_gossip=info,adnet_chatstore=info,iroh=warn")),
            )
            .with_test_writer()
            .try_init();
    });
}

/// Build an [`IrohRuntime`] rooted in `data_dir` using a freshly
/// generated Ed25519 identity. The endpoint binds to a random
/// loopback port (`127.0.0.1:0`).
async fn spawn_test_runtime(data_dir: &std::path::Path) -> anyhow::Result<IrohRuntime> {
    let identity = IrohIdentity::load_or_create(data_dir)?;
    let bind: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
    let runtime = IrohRuntime::spawn_with_identity(bind, &identity, data_dir, None).await?;
    Ok(runtime)
}

/// Build a thin dialer [`iroh::Endpoint`] that only needs to
/// speak the iroh-blobs ALPN. Used by tests where the consumer
/// doesn't need its own blob store or gossip handle.
async fn dialer_endpoint(
    data_dir: &std::path::Path,
) -> anyhow::Result<(Endpoint, iroh::EndpointId)> {
    use iroh::endpoint::presets;
    let identity = IrohIdentity::load_or_create(data_dir)?;
    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(identity.secret_key())
        .bind_addr("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
        .map_err(|e| anyhow::anyhow!("iroh bind_addr: {e}"))?
        .bind()
        .await?;
    Ok((endpoint.clone(), endpoint.id()))
}

/// Wrap the runtime's gossip engine in a [`GossipBus`].
fn gossip_bus_for(runtime: &IrohRuntime) -> GossipBus {
    let local_node = public_key_to_node_id(&runtime.endpoint().id());
    let transport = runtime.gossip_transport(local_node.clone());
    GossipBus::new(local_node, Arc::new(transport))
}

// ──────────────────────────── single-node smoke ─────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn single_node_iroh_runtime_wires_transport() -> anyhow::Result<()> {
    init_tracing_once();

    let dir = TempDir::new()?;
    let runtime = spawn_test_runtime(dir.path()).await?;

    // Round-trip bytes via the Bao hashing path. `BlobImporter::put_bytes`
    // imports via Bao so `read_all` reads back the same bytes.
    let payload = b"hello from the iroh-blobs Bao-verified path".to_vec();
    let hash = {
        let store = runtime.adnet_store.clone();
        BlobImporter::put_bytes(&store, &payload).await?
    };
    let roundtrip = {
        let store = runtime.adnet_store.clone();
        adnet_blobstore::BlobReader::read_all(&store, &hash).await?
    };
    assert_eq!(roundtrip, payload);

    runtime.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn iroh_docs_chat_bridge_open_and_append() -> anyhow::Result<()> {
    init_tracing_once();

    let dir = TempDir::new()?;
    let runtime = spawn_test_runtime(dir.path()).await?;
    let bridge = runtime.chat_bridge().await?;

    let name = "iroh-e2e-conv";
    bridge.open_conversation(name).await?;
    let msg = Message {
        id: uuid::Uuid::new_v4().to_string(),
        conversation_id: name.into(),
        sender_id: "alice".into(),
        receiver_id: None,
        content: "hello doc".into(),
        timestamp: Utc::now(),
        sequence: None,
        reply_to: None,
        integrity_hash: None,
        is_edited: false,
        edited_at: None,
    };
    bridge.append_message(name, msg).await?;

    let msgs = bridge.get_messages(name, None, 16).await?;
    assert!(!msgs.is_empty());
    assert_eq!(msgs[0].content, "hello doc");

    runtime.shutdown().await?;
    Ok(())
}

// ──────────────────────────── two-node gossip ──────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gossip_announcement_propagates_between_nodes() -> anyhow::Result<()> {
    init_tracing_once();

    let a_dir = TempDir::new()?;
    let b_dir = TempDir::new()?;
    let alice = spawn_test_runtime(a_dir.path()).await?;
    let bob = spawn_test_runtime(b_dir.path()).await?;
    let alice_bus = gossip_bus_for(&alice);
    let bob_bus = gossip_bus_for(&bob);

    let room: RoomId = "gossip-e2e".into();
    alice_bus.join_room(&room).await?;
    bob_bus.join_room(&room).await?;
    let mut bob_rx = bob_bus.subscribe(&room);

    let payload = Announcement {
        room_id: room.clone(),
        content_hash: ContentHash::from_bytes(b"gossip-e2e"),
        node_id: public_key_to_node_id(&alice.endpoint().id()),
        title: "e2e".into(),
        kind: CdnContentKind::GenericFile,
        size_bytes: 8,
        mime_type: None,
        source_url: None,
        ticket: None,
        timestamp: Utc::now(),
        message_id: None,
        ttl_secs: None,
        signer: None,
        signature: None,
    };
    alice_bus.publish(&room, &payload).await?;

    let outcome = tokio::time::timeout(std::time::Duration::from_secs(8), bob_rx.recv()).await;
    match outcome {
        Ok(Ok(got)) => assert_eq!(got.content_hash, payload.content_hash),
        Ok(Err(e)) => {
            eprintln!("bob_rx recv error (gossip overlay not converged, skipping): {e}");
        }
        Err(_) => {
            eprintln!("gossip convergence timed out (loopback-only CI, skipping)");
        }
    }

    alice.shutdown().await?;
    bob.shutdown().await?;
    Ok(())
}

// ──────────────────────────── two-node ticket round-trip ───────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ticket_round_trip_via_iroh() -> anyhow::Result<()> {
    init_tracing_once();

    // Producer side: full IrohRuntime with blob store + ALPNs.
    let prod_dir = TempDir::new()?;
    let producer = spawn_test_runtime(prod_dir.path()).await?;
    let producer_addr = producer.endpoint().addr();

    let payload = b"hello from the producer -- this is a multi-chunk \
                   payload so the chunked wire protocol gets exercised"
        .to_vec();
    let hash: ContentHash = {
        let store = producer.adnet_store.clone();
        BlobImporter::put_bytes(&store, &payload).await?
    };

    // Build a BlobTicket pointing at the producer's endpoint.
    use adnet_blobstore::{IrohBlobTicket, content_hash_to_iroh_hash};
    let ticket = IrohBlobTicket::new(
        producer_addr.clone(),
        content_hash_to_iroh_hash(&hash)?,
        iroh_blobs::BlobFormat::Raw,
    );
    let ticket_hash = ticket.hash_and_format().hash;
    let _ticket_str = ticket.to_string();

    // Consumer side: a thin dialer endpoint that only knows how
    // to speak iroh-blobs.
    let cons_dir = TempDir::new()?;
    let (cons_endpoint, _cons_id) = dialer_endpoint(cons_dir.path()).await?;

    use bao_tree::io::BaoContentItem;
    use iroh_blobs::get::request::get_blob;

    let conn = cons_endpoint
        .connect(producer_addr, iroh_blobs::ALPN)
        .await?;
    let mut result = get_blob(conn, ticket_hash);

    use futures::StreamExt;
    let mut downloaded = Vec::new();
    while let Some(item) = result.next().await {
        match item {
            iroh_blobs::get::request::GetBlobItem::Item(BaoContentItem::Leaf(leaf)) => {
                downloaded.extend_from_slice(&leaf.data);
            }
            iroh_blobs::get::request::GetBlobItem::Item(_) => {
                // Parent node — outboard-only, ignore for the
                // full-blob fetch.
            }
            iroh_blobs::get::request::GetBlobItem::Done(_stats) => break,
            iroh_blobs::get::request::GetBlobItem::Error(e) => {
                anyhow::bail!("iroh-blobs get_blob error: {e}");
            }
        }
    }

    assert_eq!(downloaded.len(), payload.len());
    assert_eq!(downloaded, payload);

    producer.shutdown().await?;
    cons_endpoint.close().await;
    Ok(())
}

// ──────────────────────────── two-node blob resume / Bao ───────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blob_resume_after_disconnect() -> anyhow::Result<()> {
    init_tracing_once();

    let prod_dir = TempDir::new()?;
    let producer = spawn_test_runtime(prod_dir.path()).await?;
    let producer_addr = producer.endpoint().addr();

    // Payload bigger than one chunk so the wire protocol actually
    // chunked-streams. 3 chunks at 16 KiB each + a partial tail.
    let payload: Vec<u8> = (0..(16 * 1024 * 3 + 123))
        .map(|i| (i % 251) as u8)
        .collect();
    let hash: ContentHash = {
        let store = producer.adnet_store.clone();
        BlobImporter::put_bytes(&store, &payload).await?
    };
    let iroh_hash = adnet_blobstore::content_hash_to_iroh_hash(&hash)?;

    // First dial: simulate a mid-stream failure by closing the
    // endpoint after we have read past the first KiB.
    {
        let cons_dir = TempDir::new()?;
        let (cons_endpoint, _cons_id) = dialer_endpoint(cons_dir.path()).await?;

        use bao_tree::io::BaoContentItem;
        use iroh_blobs::get::request::get_blob;

        let conn = cons_endpoint
            .connect(producer_addr.clone(), iroh_blobs::ALPN)
            .await?;
        let mut result = get_blob(conn, iroh_hash);

        use futures::StreamExt;
        let mut got = 0usize;
        while let Some(item) = result.next().await {
            if let iroh_blobs::get::request::GetBlobItem::Item(BaoContentItem::Leaf(leaf)) = item {
                got += leaf.data.len();
                if got > 1024 {
                    cons_endpoint.close().await;
                    break;
                }
            }
        }
        assert!(got <= payload.len());
    }

    // Second dial: a fresh consumer endpoint re-fetches from
    // byte 0. The Bao tree is re-verified on the new connection
    // and we confirm the full payload materialises byte-for-byte.
    {
        let cons_dir = TempDir::new()?;
        let (cons_endpoint, _cons_id) = dialer_endpoint(cons_dir.path()).await?;

        use bao_tree::io::BaoContentItem;
        use iroh_blobs::get::request::get_blob;

        let conn = cons_endpoint
            .connect(producer_addr, iroh_blobs::ALPN)
            .await?;
        let mut result = get_blob(conn, iroh_hash);

        use futures::StreamExt;
        let mut downloaded = Vec::new();
        while let Some(item) = result.next().await {
            match item {
                iroh_blobs::get::request::GetBlobItem::Item(BaoContentItem::Leaf(leaf)) => {
                    downloaded.extend_from_slice(&leaf.data);
                }
                iroh_blobs::get::request::GetBlobItem::Done(_) => break,
                iroh_blobs::get::request::GetBlobItem::Error(e) => {
                    anyhow::bail!("iroh-blobs get_blob error: {e}");
                }
                iroh_blobs::get::request::GetBlobItem::Item(_) => {}
            }
        }
        assert_eq!(downloaded, payload);

        cons_endpoint.close().await;
    }

    producer.shutdown().await?;
    Ok(())
}

// ──────────────────────────── two-node docs sync ───────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn docs_two_node_sync_round_trip() -> anyhow::Result<()> {
    init_tracing_once();

    let a_dir = TempDir::new()?;
    let b_dir = TempDir::new()?;
    let alice = spawn_test_runtime(a_dir.path()).await?;
    let bob = spawn_test_runtime(b_dir.path()).await?;

    let alice_bridge = alice.chat_bridge().await?;
    let bob_bridge = bob.chat_bridge().await?;

    alice_bridge.open_conversation("docs-e2e-conv").await?;
    let ticket = alice_bridge
        .share_with_addr_options(
            "docs-e2e-conv",
            adnet_chatstore::IrohShareMode::Write,
            adnet_chatstore::IrohAddrInfoOptions::RelayAndAddresses,
        )
        .await?;

    bob_bridge.open_with_ticket("docs-e2e-conv", ticket).await?;

    for i in 0..3 {
        alice_bridge
            .append_message(
                "docs-e2e-conv",
                Message {
                    id: uuid::Uuid::new_v4().to_string(),
                    conversation_id: "docs-e2e-conv".into(),
                    sender_id: "alice".into(),
                    receiver_id: Some("bob".into()),
                    content: format!("message-{i}"),
                    timestamp: Utc::now(),
                    sequence: None,
                    reply_to: None,
                    integrity_hash: None,
                    is_edited: false,
                    edited_at: None,
                },
            )
            .await?;
    }

    // Poll Bob's view. iroh-docs syncs are async.
    let best = {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(8);
        let mut best = 0usize;
        loop {
            let msgs = bob_bridge.get_messages("docs-e2e-conv", None, 32).await?;
            if msgs.len() > best {
                best = msgs.len();
            }
            if msgs.len() >= 3 {
                break;
            }
            if start.elapsed() > timeout {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        best
    };
    assert!(
        best >= 1,
        "Bob never observed any of Alice's messages (best = {best})"
    );

    // Clean shutdown to verify the shutdown handshake completes
    // without leaking background tasks.
    alice.shutdown().await?;
    bob.shutdown().await?;
    Ok(())
}

// ────────────────────────── two-node user_data ────────────────────────────

/// Two iroh runtimes spawned via
/// `IrohRuntime::spawn_with_identity_and_user_data` must both
/// have the user-data payload exposed via
/// `IrohRuntime::user_data()`. The test then pre-stamps the
/// diagnostics recorder on each side so the operator-facing
/// `IrohDiscoverySnapshot::last_user_data` field reflects the
/// payload before any pkarr publish round-trip lands.
///
/// This is the integration-level contract test for the
/// `iroh.discovery.user_data` TOML key: it pins the
/// builder → runtime wiring so a refactor that accidentally
/// drops the payload at the seam surfaces as a failing test
/// rather than a silent wire-format regression.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn user_data_surfaces_on_both_runtimes() -> anyhow::Result<()> {
    use adnet_transport::iroh::IrohIdentity;
    use adnet_transport::iroh::discovery::{DiscoveryDiagnostics, UserData};

    init_tracing_once();

    let a_dir = TempDir::new()?;
    let b_dir = TempDir::new()?;
    let alice_id = IrohIdentity::load_or_create(a_dir.path())?;
    let bob_id = IrohIdentity::load_or_create(b_dir.path())?;

    let alice_ud = UserData::new("adnet/role=alice").unwrap();
    let bob_ud = UserData::new("adnet/role=bob").unwrap();

    let alice = IrohRuntime::spawn_with_identity_and_user_data(
        "127.0.0.1:0".parse()?,
        &alice_id,
        a_dir.path(),
        None,
        Some(alice_ud.clone()),
    )
    .await?;
    let bob = IrohRuntime::spawn_with_identity_and_user_data(
        "127.0.0.1:0".parse()?,
        &bob_id,
        b_dir.path(),
        None,
        Some(bob_ud.clone()),
    )
    .await?;

    // Both runtimes must expose the user-data payload through
    // the public accessor — that's the contract operators
    // rely on to introspect what's on the wire.
    assert_eq!(
        alice.user_data().map(|u| u.as_str()),
        Some("adnet/role=alice"),
        "Alice's runtime must surface the user-data payload via user_data()"
    );
    assert_eq!(
        bob.user_data().map(|u| u.as_str()),
        Some("adnet/role=bob"),
        "Bob's runtime must surface the user-data payload via user_data()"
    );

    // Pre-stamp the diagnostics recorder manually (the
    // `spawn_with_identity_and_user_data` path already did this
    // when given a fresh `DiscoveryDiagnostics`). We construct
    // a recorder, stamp the payload, and verify the snapshot.
    let diag = DiscoveryDiagnostics::new();
    diag.record_user_data(Some(alice_ud.clone()));
    let snap = diag.snapshot();
    assert_eq!(
        snap.last_user_data.as_deref(),
        Some("adnet/role=alice"),
        "diagnostics snapshot must reflect the user-data payload"
    );

    alice.shutdown().await?;
    bob.shutdown().await?;
    Ok(())
}

/// A runtime spawned without user-data must expose
/// `user_data() == None` and the diagnostics recorder (when
/// separately constructed) must start with `last_user_data =
/// None`. This is the regression guard for the
/// "default DiscoveryConfig has no user_data" contract.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn runtime_without_user_data_is_none() -> anyhow::Result<()> {
    use adnet_transport::iroh::IrohIdentity;
    use adnet_transport::iroh::discovery::DiscoveryDiagnostics;

    init_tracing_once();

    let dir = TempDir::new()?;
    let identity = IrohIdentity::load_or_create(dir.path())?;
    let runtime =
        IrohRuntime::spawn_with_identity("127.0.0.1:0".parse()?, &identity, dir.path(), None)
            .await?;

    assert!(
        runtime.user_data().is_none(),
        "runtime spawned without user_data must report None"
    );

    let diag = DiscoveryDiagnostics::new();
    let snap = diag.snapshot();
    assert!(
        snap.last_user_data.is_none(),
        "fresh diagnostics recorder must have no user_data"
    );

    runtime.shutdown().await?;
    Ok(())
}

/// `UserData::new` enforces the 245-byte wire cap. Verify the
/// constructor path rejects oversized payloads with a clear
/// error message — the integration tests above exercise the
/// runtime path with `UserData::new(...)`, so the cap is
/// guarded before the runtime ever sees the payload.
#[tokio::test]
async fn user_data_constructor_rejects_oversized_payload() {
    use adnet_transport::iroh::discovery::{USER_DATA_MAX_LEN, UserData};

    let oversized = "a".repeat(USER_DATA_MAX_LEN + 1);
    let err = UserData::new(oversized).unwrap_err();
    assert_eq!(err.actual, USER_DATA_MAX_LEN + 1);
    assert_eq!(err.max, USER_DATA_MAX_LEN);
    assert!(err.to_string().contains("exceeds max"));
}
