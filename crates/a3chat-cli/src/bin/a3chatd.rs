//! `a3chatd` — minimal a3chat-rpc daemon launcher.
//!
//! Spins up an in-process `a3chat_app::A3chatApp` + `a3chat_rpc::RpcServer`
//! on a loopback bind (`127.0.0.1:<port>`, default `127.0.0.1:53421`),
//! so the `a3chat` CLI can be exercised end-to-end without standing
//! up `a3chat-tauri`.
//!
//! Usage:
//!   a3chatd [--bind ADDR] [--owner HEX] [--storage PATH] [--stop-after SECS]
//!            [--enable-iroh]
//!
//! When `--stop-after` is set, the daemon shuts itself down after
//! that many seconds — useful for one-shot integration runs.
//! When `--enable-iroh` is set (and the binary was compiled with the
//! `enable-iroh` feature), the daemon also boots an `IrohDocsChat`
//! bridge backed by an `iroh-docs::Doc` engine and the iroh-blobs
//! `FsStore`. The bridge is constructed but not (yet) wired into the
//! chat write path — that integration is a follow-up. This is the
//! first step: prove the iroh engine runs under the daemon at all.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use a3chat_app::storage::StorageConfig;
use a3chat_app::A3chatApp;
use a3chat_cli::lockfile::{self, LockFile};
use a3chat_core::id::UserId;
use a3chat_rpc::{RpcServer, RpcServerConfig};

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut bind: SocketAddr = "127.0.0.1:53421".parse().unwrap();
    let mut owner_hex =
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string();
    let mut storage_dir: Option<PathBuf> = None;
    let mut stop_after_secs: Option<u64> = None;
    let mut log_requests = false;
    let mut request_timeout_ms: Option<u64> = None;
    let mut enable_iroh = false;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--bind" => {
                i += 1;
                bind = args[i].parse().expect("invalid --bind");
            }
            "--owner" => {
                i += 1;
                owner_hex = args[i].clone();
            }
            "--storage" => {
                i += 1;
                storage_dir = Some(PathBuf::from(&args[i]));
            }
            "--stop-after" => {
                i += 1;
                stop_after_secs = Some(args[i].parse().expect("invalid --stop-after"));
            }
            "--log-requests" => log_requests = true,
            "--request-timeout-ms" => {
                i += 1;
                request_timeout_ms = Some(args[i].parse().expect("invalid --request-timeout-ms"));
            }
            "--enable-iroh" => enable_iroh = true,
            "-h" | "--help" => {
                print_help();
                return;
            }
            other => {
                eprintln!("a3chatd: unknown arg {other}");
                print_help();
                std::process::exit(2);
            }
        }
        i += 1;
    }

    let owner = UserId::from(owner_hex.as_str());
    if owner.as_str().len() != 64 {
        eprintln!("a3chatd: --owner must be 64 hex chars");
        std::process::exit(2);
    }

    let storage_dir = storage_dir.unwrap_or_else(|| {
        let mut p = std::env::temp_dir();
        p.push(format!("a3chatd-storage-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).expect("create storage dir");
        p
    });

    let app = match A3chatApp::new(StorageConfig::new(storage_dir.clone()), owner.clone()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("a3chatd: failed to build app: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = app.init_user(&owner).await {
        eprintln!("a3chatd: init_user failed: {e}");
        std::process::exit(1);
    }

    let mut cfg = RpcServerConfig::new(bind);
    cfg.log_requests = log_requests;
    if let Some(ms) = request_timeout_ms {
        cfg.request_timeout = Duration::from_millis(ms);
    }
    let server = RpcServer::new(app, cfg);
    let handle = match server.start().await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("a3chatd: server start failed: {e}");
            std::process::exit(1);
        }
    };

    // Acquire the daemon lock file so a second `a3chatd` cannot
    // silently bind the same loopback port. The lock records the
    // PID, bind address, owner, and storage dir so `a3chat doctor
    // --self-heal` can recover from a stale lock automatically.
    let lock_path = lockfile::lock_path();
    let lock_body = LockFile::new(
        std::process::id(),
        handle.local_addr.to_string(),
        owner.as_str(),
        storage_dir.display().to_string(),
        env!("CARGO_PKG_VERSION"),
    );
    if let Err(e) = lockfile::acquire_lock(&lock_path, &lock_body) {
        eprintln!("a3chatd: lock acquisition failed: {e}");
        eprintln!(
            "a3chatd: hint: another daemon may already be running (lock file {}).",
            lock_path.display()
        );
        handle.stop().await;
        std::process::exit(75); // EX_TEMPFAIL — operator action probably needed
    }

    eprintln!(
        "a3chatd: listening on http://{}  owner={}  storage={}",
        handle.local_addr,
        owner.as_str(),
        storage_dir.display()
    );
    eprintln!("a3chatd: lock file {}", lock_path.display());

    if enable_iroh {
        match try_enable_iroh(&storage_dir).await {
            Ok(author) => {
                eprintln!("a3chatd: iroh-docs bridge ready (default author {author})");
            }
            Err(e) => {
                eprintln!("a3chatd: --enable-iroh failed: {e}");
                handle.stop().await;
                if let Err(e) = lockfile::release_lock(&lock_path, std::process::id()) {
                    eprintln!("a3chatd: lock release warning: {e}");
                }
                std::process::exit(1);
            }
        }
    } else {
        eprintln!("a3chatd: iroh-docs bridge disabled (pass --enable-iroh to enable)");
    }

    eprintln!("a3chatd: ready");

    if let Some(secs) = stop_after_secs {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(secs)).await;
            eprintln!("a3chatd: --stop-after {secs}s elapsed, shutting down");
            std::process::exit(0);
        });
    }

    // Wait for SIGINT/SIGTERM.
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("install SIGTERM");
        let mut intr = signal(SignalKind::interrupt()).expect("install SIGINT");
        tokio::select! {
            _ = term.recv() => {},
            _ = intr.recv() => {},
        }
        eprintln!("a3chatd: received signal, shutting down");
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        eprintln!("a3chatd: ctrl-c, shutting down");
    }

    let local_addr = handle.local_addr;
    handle.stop().await;
    if let Err(e) = lockfile::release_lock(&lock_path, std::process::id()) {
        eprintln!("a3chatd: lock release warning: {e}");
    }
    eprintln!("a3chatd: stopped (addr={local_addr})");
}

fn print_help() {
    println!(
        "a3chatd — minimal a3chat-rpc daemon launcher\n\n\
USAGE:\n  \
a3chatd [OPTIONS]\n\n\
OPTIONS:\n  \
--bind ADDR                Bind address (default 127.0.0.1:53421)\n  \
--owner HEX                64-hex NodeId (default test owner)\n  \
--storage PATH             Storage dir (default $TMP/a3chatd-storage-<uuid>)\n  \
--stop-after SECS          Auto-stop after N seconds (useful for tests)\n  \
--request-timeout-ms MS    Per-request timeout (default 30000)\n  \
--log-requests             Log every request line\n  \
--enable-iroh              Boot the iroh-docs/iroh-blobs bridge alongside the RPC server\n                           (requires `a3chat-cli` to be compiled with `--features enable-iroh`)\n  \
-h, --help                 Print this help\n"
    );
}

/// Boot the iroh-docs + iroh-blobs bridge when the binary was built
/// with the `enable-iroh` feature. Returns the bridge's default
/// author id on success; rejects the request when the feature is
/// off so `--enable-iroh` doesn't silently no-op in a slim build.
///
/// We guard the heavy `use` statements behind `#[cfg(feature =
/// "enable-iroh")]` so the lean default build (no iroh, no
/// iroh-blobs) still compiles.
#[cfg_attr(not(feature = "enable-iroh"), allow(unused_variables))]
async fn try_enable_iroh(storage_dir: &std::path::Path) -> anyhow::Result<String> {
    #[cfg(feature = "enable-iroh")]
    {
        use a3net_blobstore::IrohBlobStore;
        use a3net_chatstore::IrohDocsChat;
        use iroh::endpoint::presets::N0;
        use iroh_docs::api::DocsApi;
        use iroh_docs::protocol::Docs;
        use iroh_gossip::net::Gossip;

        // Build the engine the same way the in-tree iroh_docs_chat
        // smoke test does: a bound Endpoint + Gossip + an in-memory
        // docs replica on top of the FsStore-backed blobs. The
        // FsStore lives under `<storage_dir>/iroh-blobs/` so it
        // shares the same root as the SQLite stores — same backup /
        // cleanup story.
        let blob_store = IrohBlobStore::open(storage_dir).await?;
        let endpoint = iroh::Endpoint::bind(N0).await?;
        let gossip = Gossip::builder().spawn(endpoint.clone());
        let fs: iroh_blobs::api::Store = (*blob_store.handle()).clone().into();
        let docs = Docs::memory()
            .spawn(endpoint.clone(), fs, gossip)
            .await?;
        let api: DocsApi = docs.api().clone();
        // Touching the bridge is enough — proves the engine
        // constructs end-to-end and the default author is
        // mints. We hold no reference intentionally; the bridge's
        // `Drop` impl aborts the subscription tasks, and the docs
        // engine lives until process exit. Wiring it into the chat
        // write path is the next step (see A3chatApp).
        let bridge = IrohDocsChat::new(std::sync::Arc::new(api), blob_store).await?;
        Ok(bridge.default_author().to_string())
    }
    #[cfg(not(feature = "enable-iroh"))]
    {
        let _ = storage_dir;
        anyhow::bail!(
            "this `a3chatd` was built without the `enable-iroh` feature; rebuild with \
             `cargo build -p a3chat-cli --bin a3chatd --features enable-iroh`"
        )
    }
}