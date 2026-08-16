//! `a3net` CLI entry point.

use std::path::PathBuf;

use a3net_cli::feed_view::feed_for_humans;
use a3net_cli::{Cli, Cmd};
use a3net_node::{Node, NodeConfig};
use a3net_types::{CdnContentKind, ContentHash, RoomId};
use anyhow::Result;
use clap::Parser;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize i18n based on --lang flag
    init_i18n_from_cli(&cli);

    // Initialize tracing based on CLI flags
    init_tracing_from_cli(&cli);

    let data_dir = PathBuf::from(&cli.data_dir);
    std::fs::create_dir_all(&data_dir)?;

    // ── Commands that do NOT need a running Node ──────────────────────────────
    // These are "offline" commands: they open files/SQLite and exit without
    // touching the network. We construct a NodeConfig only to get the NodeId.
    // These run BEFORE the Node is built so we avoid spinning up the runtime.

    let offline_data_dir = data_dir.clone();

    // Resolve the storage budget once for the whole CLI session. The
    // config is loaded here even for commands that don't need storage,
    // because `cabs` like `Cmd::Add` need the resolved total to open
    // the `StorageTopology` with the right hard cap. We use the
    // platform-default config path so `app.toml` is read on every
    // invocation; `--config` is handled at the data-dir wiring layer.
    let config_path = offline_data_dir.join("config.json");
    let app_config = a3net_cli::config::load_for_cli(Some(&config_path))
        .map(|l| l.config)
        .unwrap_or_default();
    let storage_total_bytes = match app_config.storage.resolved_total_bytes() {
        Ok(n) => n,
        Err(e) => {
            eprintln!(
                "a3net: storage.totalBytes is invalid ({e}); falling back to {} bytes",
                a3net_cli::bytes::DEFAULT_TOTAL_BYTES
            );
            a3net_cli::bytes::DEFAULT_TOTAL_BYTES
        }
    };

    match &cli.cmd {
        // ── Offline: config ──────────────────────────────────────────────
        Cmd::Config { sub } => {
            a3net_cli::config::run_config(sub, &offline_data_dir)?;
            return Ok(());
        }

        // ── Offline: storage ─────────────────────────────────────────────
        Cmd::Storage { sub } => {
            let storage_cmd: a3net_cli::storage::StorageCmd = sub.into();
            a3net_cli::storage::run_storage(&offline_data_dir, &storage_cmd)?;
            return Ok(());
        }

        // ── Offline: status ──────────────────────────────────────────────
        Cmd::Status { json, compact, watch } => {
            if *watch == Some(0) || watch.is_none() {
                // Single shot mode
                if *json {
                    a3net_cli::status::run_status(&offline_data_dir, *json)?;
                } else if *compact {
                    a3net_cli::status::run_status_compact(&offline_data_dir)?;
                } else {
                    a3net_cli::status::run_status_rich(&offline_data_dir)?;
                }
            } else {
                // Watch mode - loop with interval
                let interval = watch.unwrap_or(5);
                eprintln!("Watching status every {} seconds (Ctrl+C to stop)...", interval);
                loop {
                    print!("\x1b[2J\x1b[H"); // Clear screen
                    if *json {
                        a3net_cli::status::run_status(&offline_data_dir, *json)?;
                    } else if *compact {
                        a3net_cli::status::run_status_compact(&offline_data_dir)?;
                    } else {
                        a3net_cli::status::run_status_rich(&offline_data_dir)?;
                    }
                    std::thread::sleep(std::time::Duration::from_secs(interval));
                }
            }
            return Ok(());
        }

        // ── Offline: diagnostics ──────────────────────────────────────────
        Cmd::Diagnostics { json } => {
            a3net_cli::diagnostics::run_diagnostics(&offline_data_dir, *json)?;
            return Ok(());
        }

        // ── Offline: bandwidth ─────────────────────────────────────────────
        Cmd::Bandwidth { json } => {
            a3net_cli::bandwidth::run_bandwidth(&offline_data_dir, *json)?;
            return Ok(());
        }

        // ── Offline: profile ───────────────────────────────────────────────
        Cmd::Profile { sub } => {
            a3net_cli::profile::run_profile(sub, &offline_data_dir)?;
            return Ok(());
        }

        // ── Offline: roster ───────────────────────────────────────────────
        Cmd::Roster { sub } => {
            futures::executor::block_on(a3net_cli::roster::run(sub, &offline_data_dir))?;
            return Ok(());
        }

        // ── Offline: user ─────────────────────────────────────────────────
        Cmd::User { sub } => {
            futures::executor::block_on(a3net_cli::userstore::run(sub, &offline_data_dir))?;
            return Ok(());
        }

        // ── Offline: identity (local node's self-description) ─────────────
        Cmd::Identity { sub } => {
            let op_cmd = a3net_cli::IdentityOpsCmd::from_cli(sub);
            a3net_cli::run_identity(&op_cmd, &offline_data_dir)?;
            return Ok(());
        }

        // ── Offline: contacts (local address book) ────────────────────────
        Cmd::Contacts { sub } => {
            let op_cmd = a3net_cli::ContactsOpsCmd::from_cli(sub);
            a3net_cli::run_contacts(&op_cmd, &offline_data_dir)?;
            return Ok(());
        }

        // ── Offline: profile-page (HTML render) ───────────────────────────
        Cmd::ProfilePage { sub } => {
            let op_cmd = a3net_cli::ProfilePageOpsCmd::from_cli(sub);
            a3net_cli::run_profile_page(&op_cmd, &offline_data_dir)?;
            return Ok(());
        }

        // ── Offline: moments ───────────────────────────────────────────────
        Cmd::Moments { sub } => {
            futures::executor::block_on(a3net_cli::moments::run(sub, &offline_data_dir))?;
            return Ok(());
        }

        // ── Offline: news ─────────────────────────────────────────────────
        Cmd::News { sub } => {
            futures::executor::block_on(a3net_cli::news::run(sub, &offline_data_dir))?;
            return Ok(());
        }

        // ── Offline: share ─────────────────────────────────────────────────
        Cmd::Share { sub } => {
            futures::executor::block_on(a3net_cli::share::run(sub, &offline_data_dir))?;
            return Ok(());
        }

        // ── Offline: mdns ─────────────────────────────────────────────────
        Cmd::Mdns { sub } => {
            a3net_cli::mdns::run_mdns(sub, &offline_data_dir)?;
            return Ok(());
        }

        // ── Offline: device pairing / invitations / QR / mesh admission ──
        Cmd::Pair { sub } => {
            futures::executor::block_on(a3net_cli::pairing_ops::run_pair(sub, &offline_data_dir))?;
            return Ok(());
        }
        Cmd::Invite { sub } => {
            futures::executor::block_on(a3net_cli::pairing_ops::run_invite(sub, &offline_data_dir))?;
            return Ok(());
        }
        Cmd::Qr { sub } => {
            futures::executor::block_on(a3net_cli::pairing_ops::run_qr(sub, &offline_data_dir))?;
            return Ok(());
        }
        Cmd::Mesh { sub } => {
            futures::executor::block_on(a3net_cli::pairing_ops::run_mesh(sub, &offline_data_dir))?;
            return Ok(());
        }

        // ── Offline: webhook config management ─────────────────────────────
        Cmd::Webhook { sub } => {
            futures::executor::block_on(a3net_cli::webhook_ops::run_webhook(sub, &offline_data_dir))?;
            return Ok(());
        }

        // ── Offline: name / key ────────────────────────────────────────────
        Cmd::Name { sub } => {
            a3net_cli::ipns_ops::run_name(sub, &offline_data_dir)?;
            return Ok(());
        }

        Cmd::Key { sub } => {
            a3net_cli::ipns_ops::run_key(sub, &offline_data_dir)?;
            return Ok(());
        }

        // ── Init (offline, no node needed) ───────────────────────────────
        Cmd::Init => {
            let cfg = NodeConfig::load_or_create(&data_dir)?;
            let node_id = cfg.node_id.clone();
            println!("node_id  = {}", node_id);
            println!("short    = a3net-{}", node_id.short());
            println!("data_dir = {}", data_dir.display());
            return Ok(());
        }

        // ── Everything below needs a running Node ──────────────────────────
        _ => {}
    }

    // ── Build the Node ──────────────────────────────────────────────────────
    let cfg = NodeConfig::load_or_create(&data_dir)?;
    let node_id = cfg.node_id.clone();
    let node = Node::builder(cfg).build().await?;

    info!(
        "a3net node {} (data: {})",
        node_id.short(),
        data_dir.display()
    );

    match &cli.cmd {
        // ══ Node-required commands ════════════════════════════════════════

        Cmd::Serve { metrics_addr } => {
            let ep = node.ensure_mesh().await?;
            println!("mesh listening on http://{}/blobs/<hash>", ep);
            // Start the Prometheus exporter on the requested
            // address, if the operator passed `--metrics-addr`.
            // The handle is dropped on shutdown, which stops the
            // axum task.
            let _metrics_handle = if let Some(addr) = metrics_addr {
                let addr: std::net::SocketAddr = addr
                    .parse()
                    .map_err(|e: std::net::AddrParseError| anyhow::anyhow!("parse --metrics-addr {addr}: {e}"))?;
                match a3net_observability::http::serve(
                    a3net_observability::http::MetricsServerConfig {
                        bind_addr: addr,
                        registry: None,
                    },
                )
                .await
                {
                    Ok(handle) => {
                        println!("metrics listening on http://{}/metrics", handle.local_addr());
                        Some(handle)
                    }
                    Err(e) => {
                        eprintln!("failed to start metrics server on {addr}: {e}");
                        None
                    }
                }
            } else {
                None
            };
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("received Ctrl-C, shutting down");
                }
                _ = std::future::pending::<()>() => {}
            }
            node.shutdown().await?;
        }

        Cmd::MetricsServer { metrics_addr } => {
            // Standalone Prometheus /metrics endpoint, useful for
            // gateway-only deployments. Blocks on Ctrl-C.
            let addr: std::net::SocketAddr = metrics_addr
                .parse()
                .map_err(|e: std::net::AddrParseError| anyhow::anyhow!("parse --metrics-addr {metrics_addr}: {e}"))?;
            let handle = a3net_observability::http::serve(
                a3net_observability::http::MetricsServerConfig {
                    bind_addr: addr,
                    registry: None,
                },
            )
            .await?;
            println!(
                "metrics listening on http://{}/metrics (Ctrl-C to stop)",
                handle.local_addr()
            );
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("received Ctrl-C, shutting down");
                }
                _ = std::future::pending::<()>() => {}
            }
        }

        Cmd::Announce {
            room,
            file,
            title,
            kind,
        } => {
            let room: RoomId = RoomId::new(room);
            node.join_room(&room).await?;
            let kind = CdnContentKind::from_str_loose(kind)
                .ok_or_else(|| anyhow::anyhow!("unknown kind: {kind}"))?;
            let path = std::path::PathBuf::from(file);
            let ann = node.import_and_announce(&room, &path, title, kind).await?;
            let ticket = ann.ticket.as_ref().map(|t| t.encode()).unwrap_or_default();
            println!(
                "{}",
                serde_json::json!({
                    "room": room.as_str(),
                    "hash": ann.content_hash.as_hex(),
                    "sizeBytes": ann.size_bytes,
                    "ticket": ticket,
                })
            );
        }

        Cmd::Feed { room } => {
            let room: RoomId = RoomId::new(room);
            node.join_room(&room).await?;
            let feed = node.room_feed(&room).await?;
            let json = serde_json::to_string_pretty(&feed_for_humans(&feed))?;
            println!("{json}");
        }

        Cmd::Echo { room } => {
            let room: RoomId = RoomId::new(room);
            node.join_room(&room).await?;
            let hash = ContentHash::from_bytes(format!("echo:{room}").as_bytes());
            let ann = a3net_types::Announcement {
                room_id: room.clone(),
                content_hash: hash,
                node_id: node_id.clone(),
                title: format!("echo into {room}"),
                kind: CdnContentKind::GenericFile,
                size_bytes: 0,
                mime_type: None,
                source_url: None,
                ticket: None,
                timestamp: chrono::Utc::now(),
                signer: None,
                signature: None,
                message_id: None,
                ttl_secs: None,
            };
            node.announce(&room, &ann).await?;
            println!("echoed into {room}");
        }

        Cmd::Run => {
            if let Err(e) = node.ensure_mesh().await {
                info!(error = %e, "mesh not started (continuing without it)");
            }
            let repl_result = a3net_cli::run_repl(data_dir.clone(), node).await;
            info!("REPL ended, exiting");
            repl_result?;
        }

        // ─── File / Pin / Repo commands ────────────────────────────────────
        Cmd::Add { path, recursive, wrap_in_dir, pin, json } => {
            let topo = a3net_cli::storage::open_topology_with_total_bytes(
                &data_dir,
                storage_total_bytes,
            )?;
            let args = a3net_cli::file_ops::AddArgs {
                path: std::path::PathBuf::from(path),
                recursive: *recursive,
                wrap_in_dir: *wrap_in_dir,
                pin: *pin,
                json: *json,
            };
            a3net_cli::file_ops::run_add(&args, &topo)?;
        }

        Cmd::Get { cid, output, json } => {
            let topo = a3net_cli::storage::open_topology_with_total_bytes(
                &data_dir,
                storage_total_bytes,
            )?;
            let args = a3net_cli::file_ops::GetArgs {
                cid: cid.clone(),
                output: output.clone().map(std::path::PathBuf::from),
                json: *json,
            };
            a3net_cli::file_ops::run_get(&args, &topo)?;
        }

        Cmd::Cat { cid, json } => {
            let topo = a3net_cli::storage::open_topology_with_total_bytes(
                &data_dir,
                storage_total_bytes,
            )?;
            let args = a3net_cli::file_ops::CatArgs {
                cid: cid.clone(),
                json: *json,
            };
            a3net_cli::file_ops::run_cat(&args, &topo)?;
        }

        Cmd::Ls { cid, json } => {
            let topo = a3net_cli::storage::open_topology_with_total_bytes(
                &data_dir,
                storage_total_bytes,
            )?;
            let args = a3net_cli::file_ops::LsArgs {
                cid: cid.clone(),
                json: *json,
            };
            a3net_cli::file_ops::run_ls(&args, &topo)?;
        }

        Cmd::Pin { sub } => {
            let topo = a3net_cli::storage::open_topology_with_total_bytes(
                &data_dir,
                storage_total_bytes,
            )?;
            let pin_cmd = match sub {
                a3net_cli::cli::PinCmd::Add { cid, recursive } => {
                    a3net_cli::file_ops::PinCmd::Add { cid: cid.clone(), recursive: *recursive }
                }
                a3net_cli::cli::PinCmd::Rm { cid } => {
                    a3net_cli::file_ops::PinCmd::Rm { cid: cid.clone() }
                }
                a3net_cli::cli::PinCmd::Ls { cid, json } => {
                    a3net_cli::file_ops::PinCmd::Ls { cid: cid.clone(), json: *json }
                }
                a3net_cli::cli::PinCmd::Verify { cid } => {
                    a3net_cli::file_ops::PinCmd::Verify { cid: cid.clone() }
                }
                a3net_cli::cli::PinCmd::Gc => {
                    a3net_cli::file_ops::PinCmd::Gc
                }
            };
            a3net_cli::file_ops::run_pin(&pin_cmd, &topo, &data_dir)?;
        }

        Cmd::Repo { sub } => {
            let topo = a3net_cli::storage::open_topology_with_total_bytes(
                &data_dir,
                storage_total_bytes,
            )?;
            let repo_cmd = match sub {
                a3net_cli::cli::RepoCmd::Stat { json } => {
                    a3net_cli::file_ops::RepoCmd::Stat { json: *json }
                }
                a3net_cli::cli::RepoCmd::Ls { json } => {
                    a3net_cli::file_ops::RepoCmd::Ls { json: *json }
                }
                a3net_cli::cli::RepoCmd::Gc {
                    dry_run,
                    prune_unpinned,
                    prune_all,
                    i_know_what_i_am_doing,
                    json,
                } => a3net_cli::file_ops::RepoCmd::Gc {
                    dry_run: *dry_run,
                    prune_unpinned: *prune_unpinned,
                    prune_all: *prune_all,
                    i_know_what_i_am_doing: *i_know_what_i_am_doing,
                    json: *json,
                },
                a3net_cli::cli::RepoCmd::Verify { json } => {
                    a3net_cli::file_ops::RepoCmd::Verify { json: *json }
                }
            };
            a3net_cli::file_ops::run_repo(&repo_cmd, &topo, &data_dir)?;
        }

        // ─── Routing commands ─────────────────────────────────────────────
        Cmd::Routing { sub } => {
            let routing_cmd = match sub {
                a3net_cli::cli::RoutingCmd::FindProvs { cid, num, json } => {
                    a3net_cli::routing_ops::RoutingCmd::FindProvs {
                        cid: cid.clone(),
                        num: num.clone(),
                        json: *json,
                    }
                }
                a3net_cli::cli::RoutingCmd::FindPeer { peer_id, json } => {
                    a3net_cli::routing_ops::RoutingCmd::FindPeer {
                        peer_id: peer_id.clone(),
                        json: *json,
                    }
                }
                a3net_cli::cli::RoutingCmd::Get { key, json } => {
                    a3net_cli::routing_ops::RoutingCmd::Get {
                        key: key.clone(),
                        json: *json,
                    }
                }
                a3net_cli::cli::RoutingCmd::Put { key, value, json } => {
                    a3net_cli::routing_ops::RoutingCmd::Put {
                        key: key.clone(),
                        value: value.clone(),
                        json: *json,
                    }
                }
            };
            a3net_cli::routing_ops::run_routing(&routing_cmd, &node).await?;
        }

        // ─── DHT commands ─────────────────────────────────────────────────
        Cmd::Dht { sub } => {
            let dht_cmd = match sub {
                a3net_cli::cli::DhtExtraCmd::FindPeer { peer_id, json } => {
                    a3net_cli::routing_ops::DhtExtraCmd::FindPeer {
                        peer_id: peer_id.clone(),
                        json: *json,
                    }
                }
                a3net_cli::cli::DhtExtraCmd::Query { target, json } => {
                    a3net_cli::routing_ops::DhtExtraCmd::Query {
                        target: target.clone(),
                        json: *json,
                    }
                }
                a3net_cli::cli::DhtExtraCmd::Put { key, value, json } => {
                    a3net_cli::routing_ops::DhtExtraCmd::Put {
                        key: key.clone(),
                        value: value.clone(),
                        json: *json,
                    }
                }
                a3net_cli::cli::DhtExtraCmd::Get { key, json } => {
                    a3net_cli::routing_ops::DhtExtraCmd::Get {
                        key: key.clone(),
                        json: *json,
                    }
                }
            };
            a3net_cli::routing_ops::run_dht_extra(&dht_cmd, &node).await?;
        }

        // ─── Swarm commands ───────────────────────────────────────────────
        Cmd::Swarm { sub } => {
            let swarm_cmd = match sub {
                a3net_cli::cli::SwarmCmd::Peers { json } => {
                    a3net_cli::routing_ops::SwarmCmd::Peers { json: *json }
                }
                a3net_cli::cli::SwarmCmd::Connect { addr } => {
                    a3net_cli::routing_ops::SwarmCmd::Connect { addr: addr.clone() }
                }
                a3net_cli::cli::SwarmCmd::Disconnect { peer_id } => {
                    a3net_cli::routing_ops::SwarmCmd::Disconnect {
                        peer_id: peer_id.clone(),
                    }
                }
                a3net_cli::cli::SwarmCmd::Addrs { json } => {
                    a3net_cli::routing_ops::SwarmCmd::Addrs { json: *json }
                }
                a3net_cli::cli::SwarmCmd::Filters { json } => {
                    a3net_cli::routing_ops::SwarmCmd::Filters { json: *json }
                }
            };
            a3net_cli::routing_ops::run_swarm(&swarm_cmd, &data_dir, &node).await?;
        }

        // ─── Bitswap commands ─────────────────────────────────────────────
        Cmd::Bitswap { sub } => {
            a3net_cli::bitswap_ops::run_bitswap(sub, &node, &data_dir).await?;
        }

        // ─── Channel commands ─────────────────────────────────────────────
        Cmd::Channel { sub } => {
            let args: a3net_cli::channel_ops::ChannelArgs = sub.into();
            a3net_cli::channel_ops::run_channel(&args, &node).await?;
        }

        // ─── News commands ─────────────────────────────────────────────────
        Cmd::News { sub } => {
            // News is online (needs gossip) but we can run via block_on
            futures::executor::block_on(a3net_cli::news::run(sub, &data_dir))?;
        }

        // ─── Agent commands ────────────────────────────────────────────────
        Cmd::Ask { peer, question, json } => {
            // `ask` talks to the local daemon over IPC/RPC (no running Node needed
            // for the daemon RPC call itself — the daemon handles the P2P transport).
            let data_dir = std::path::PathBuf::from(&cli.data_dir);
            a3net_cli::ask_ops::run_ask(&data_dir, peer.to_owned(), question.to_owned(), *json).await?;
        }

        // ══ End of commands that need a running Node ═════════════════════
        _ => {}
    }

    Ok(())
}

/// Initialize tracing based on CLI flags.
///
/// If `--trace` is set, initializes OpenTelemetry tracing with the configured
/// endpoint and sampling ratio. Falls back to console-only logging otherwise.
fn init_tracing_from_cli(cli: &Cli) {
    // Set up console logging
    let env_filter = cli
        .log_filter
        .as_ref()
        .map(|f| tracing_subscriber::EnvFilter::new(f.clone()))
        .unwrap_or_else(|| {
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
        });

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(true)
        .with_thread_ids(cli.verbose)
        .with_thread_names(cli.verbose)
        .with_file(cli.verbose)
        .with_line_number(cli.verbose)
        .init();

    // Initialize OTLP tracing if configured
    #[cfg(any(feature = "otlp-grpc", feature = "otlp-http"))]
    if cli.trace {
        use a3net_observability::tracing::{init_tracing, TracingConfig};

        let mut config = TracingConfig::new("a3net-cli")
            .with_enabled(true);

        if let Some(endpoint) = &cli.trace_endpoint {
            config = config.with_otlp_endpoint(endpoint.clone());
        }

        if let Some(ratio) = cli.trace_sample {
            config = config.with_sampling_ratio(ratio);
        }

        if cli.verbose {
            config = config.with_verbose_console();
        }

        if let Some(filter) = &cli.log_filter {
            config = config.with_log_filter(filter.clone());
        }

        if let Err(e) = init_tracing(&config) {
            eprintln!("Warning: Failed to initialize tracing: {}", e);
        }
    }
}

/// Initialize i18n based on --lang CLI flag.
fn init_i18n_from_cli(cli: &Cli) {
    use a3net_tui::i18n::{set_locale, Locale};

    match cli.lang.to_lowercase().as_str() {
        "zh" | "zh-cn" | "zh_cn" => {
            set_locale(Locale::ZhCn);
        }
        _ => {
            set_locale(Locale::En);
        }
    }
}
