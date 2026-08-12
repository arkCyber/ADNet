//! `adnet` CLI entry point.

use std::path::PathBuf;

use adnet_cli::feed_view::feed_for_humans;
use adnet_cli::{Cli, Cmd};
use adnet_node::{Node, NodeConfig};
use adnet_types::{CdnContentKind, ContentHash, RoomId};
use anyhow::Result;
use clap::Parser;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let data_dir = PathBuf::from(&cli.data_dir);
    std::fs::create_dir_all(&data_dir)?;
    // Persist node_id across restarts so tickets and gossip addresses
    // remain stable — mirrors iroh's per-process `SecretKey` persistence.
    let cfg = NodeConfig::load_or_create(&data_dir)?;
    let node_id = cfg.node_id.clone();
    let node = Node::builder(cfg).build().await?;

    info!(
        "adnet node {} (data: {})",
        node_id.short(),
        data_dir.display()
    );

    match cli.cmd {
        Cmd::Init => {
            println!("node_id  = {}", node_id);
            println!("short    = adnet-{}", node_id.short());
            println!("data_dir = {}", data_dir.display());
        }

        Cmd::Serve => {
            let ep = node.ensure_mesh().await?;
            println!("mesh listening on http://{}/blobs/<hash>", ep);
            // Graceful shutdown on SIGINT / SIGTERM — the prior
            // implementation blocked forever with
            // `std::future::pending()` which prevented Ctrl-C from
            // tearing the server down cleanly.
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("received Ctrl-C, shutting down");
                }
                _ = std::future::pending::<()>() => {}
            }
            node.shutdown().await?;
        }

        Cmd::Announce {
            room,
            file,
            title,
            kind,
        } => {
            let room: RoomId = room.into();
            node.join_room(&room).await?;
            let kind = CdnContentKind::from_str_loose(&kind)
                .ok_or_else(|| anyhow::anyhow!("unknown kind: {kind}"))?;
            let path = std::path::PathBuf::from(&file);
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
            let room: RoomId = room.into();
            node.join_room(&room).await?;
            let feed = node.room_feed(&room).await?;
            let json = serde_json::to_string_pretty(&feed_for_humans(&feed))?;
            println!("{json}");
        }

        Cmd::Echo { room } => {
            let room: RoomId = room.into();
            node.join_room(&room).await?;
            let hash = ContentHash::from_bytes(format!("echo:{room}").as_bytes());
            let ann = adnet_types::Announcement {
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
            };
            node.announce(&room, &ann).await?;
            println!("echoed into {room}");
        }

        Cmd::Run => {
            // Start the mesh server up front so the REPL can talk
            // about `/mesh`, `/announce`, etc. without each command
            // having to lazily trigger `ensure_mesh` on first use.
            if let Err(e) = node.ensure_mesh().await {
                info!(error = %e, "mesh not started (continuing without it)");
            }
            // Hand the node over to the REPL. The REPL is responsible
            // for tearing it down on `/quit` / EOF.
            let repl_result = adnet_cli::run_repl(data_dir.clone(), node).await;
            info!("REPL ended, exiting");
            repl_result?;
        }

        // ─── Audit V6: file / pin / repo / routing / dht / swarm ───
        // These 9 commands run offline against the local blob
        // store topology — they don't touch the running node and
        // exit as soon as the on-disk state is updated.

        Cmd::Add {
            path,
            recursive,
            wrap_in_dir,
            pin,
            json,
        } => {
            let topo = adnet_blobstore::scope::StorageTopology::open(
                &data_dir,
                adnet_blobstore::scope::QuotaPolicy::default_split(
                    1024u64 * 1024 * 1024 * 1024,
                ),
            )?;
            let args = adnet_cli::file_ops::AddArgs {
                path: std::path::PathBuf::from(path),
                recursive,
                wrap_in_dir,
                pin,
                json,
            };
            adnet_cli::file_ops::run_add(&args, &topo)?;
        }

        Cmd::Get { cid, output, json } => {
            let topo = adnet_blobstore::scope::StorageTopology::open(
                &data_dir,
                adnet_blobstore::scope::QuotaPolicy::default_split(
                    1024u64 * 1024 * 1024 * 1024,
                ),
            )?;
            let args = adnet_cli::file_ops::GetArgs {
                cid,
                output: output.map(std::path::PathBuf::from),
                json,
            };
            adnet_cli::file_ops::run_get(&args, &topo)?;
        }

        Cmd::Cat { cid, json } => {
            let topo = adnet_blobstore::scope::StorageTopology::open(
                &data_dir,
                adnet_blobstore::scope::QuotaPolicy::default_split(
                    1024u64 * 1024 * 1024 * 1024,
                ),
            )?;
            let args = adnet_cli::file_ops::CatArgs { cid, json };
            adnet_cli::file_ops::run_cat(&args, &topo)?;
        }

        Cmd::Ls { cid, json } => {
            let topo = adnet_blobstore::scope::StorageTopology::open(
                &data_dir,
                adnet_blobstore::scope::QuotaPolicy::default_split(
                    1024u64 * 1024 * 1024 * 1024,
                ),
            )?;
            let args = adnet_cli::file_ops::LsArgs { cid, json };
            adnet_cli::file_ops::run_ls(&args, &topo)?;
        }

        Cmd::Pin { sub } => {
            let topo = adnet_blobstore::scope::StorageTopology::open(
                &data_dir,
                adnet_blobstore::scope::QuotaPolicy::default_split(
                    1024u64 * 1024 * 1024 * 1024,
                ),
            )?;
            let pin_cmd = match sub {
                adnet_cli::cli::PinCmd::Add { cid, recursive } => {
                    adnet_cli::file_ops::PinCmd::Add {
                        cid: cid.clone(),
                        recursive: *recursive,
                    }
                }
                adnet_cli::cli::PinCmd::Rm { cid } => {
                    adnet_cli::file_ops::PinCmd::Rm { cid: cid.clone() }
                }
                adnet_cli::cli::PinCmd::Ls { cid, json } => {
                    adnet_cli::file_ops::PinCmd::Ls {
                        cid: cid.clone(),
                        json: *json,
                    }
                }
                adnet_cli::cli::PinCmd::Verify { cid } => {
                    adnet_cli::file_ops::PinCmd::Verify { cid: cid.clone() }
                }
            };
            adnet_cli::file_ops::run_pin(&pin_cmd, &topo, &data_dir)?;
        }

        Cmd::Repo { sub } => {
            let topo = adnet_blobstore::scope::StorageTopology::open(
                &data_dir,
                adnet_blobstore::scope::QuotaPolicy::default_split(
                    1024u64 * 1024 * 1024 * 1024,
                ),
            )?;
            let repo_cmd = match sub {
                adnet_cli::cli::RepoCmd::Stat { json } => {
                    adnet_cli::file_ops::RepoCmd::Stat { json: *json }
                }
                adnet_cli::cli::RepoCmd::Ls { json } => {
                    adnet_cli::file_ops::RepoCmd::Ls { json: *json }
                }
                adnet_cli::cli::RepoCmd::Gc { dry_run, json } => {
                    adnet_cli::file_ops::RepoCmd::Gc {
                        dry_run: *dry_run,
                        json: *json,
                    }
                }
                adnet_cli::cli::RepoCmd::Verify { json } => {
                    adnet_cli::file_ops::RepoCmd::Verify { json: *json }
                }
            };
            adnet_cli::file_ops::run_repo(&repo_cmd, &topo)?;
        }

        Cmd::Routing { sub } => {
            let routing_cmd = match sub {
                adnet_cli::cli::RoutingCmd::FindProvs { cid, num, json } => {
                    adnet_cli::routing_ops::RoutingCmd::FindProvs {
                        cid: cid.clone(),
                        num: *num,
                        json: *json,
                    }
                }
                adnet_cli::cli::RoutingCmd::FindPeer { peer_id, json } => {
                    adnet_cli::routing_ops::RoutingCmd::FindPeer {
                        peer_id: peer_id.clone(),
                        json: *json,
                    }
                }
                adnet_cli::cli::RoutingCmd::Get { key, json } => {
                    adnet_cli::routing_ops::RoutingCmd::Get {
                        key: key.clone(),
                        json: *json,
                    }
                }
                adnet_cli::cli::RoutingCmd::Put { key, value, json } => {
                    adnet_cli::routing_ops::RoutingCmd::Put {
                        key: key.clone(),
                        value: value.clone(),
                        json: *json,
                    }
                }
            };
            adnet_cli::routing_ops::run_routing(&routing_cmd, &node).await?;
        }

        Cmd::Dht { sub } => {
            let dht_cmd = match sub {
                adnet_cli::cli::DhtExtraCmd::FindPeer { peer_id, json } => {
                    adnet_cli::routing_ops::DhtExtraCmd::FindPeer {
                        peer_id: peer_id.clone(),
                        json: *json,
                    }
                }
                adnet_cli::cli::DhtExtraCmd::Query { target, json } => {
                    adnet_cli::routing_ops::DhtExtraCmd::Query {
                        target: target.clone(),
                        json: *json,
                    }
                }
                adnet_cli::cli::DhtExtraCmd::Put { key, value, json } => {
                    adnet_cli::routing_ops::DhtExtraCmd::Put {
                        key: key.clone(),
                        value: value.clone(),
                        json: *json,
                    }
                }
                adnet_cli::cli::DhtExtraCmd::Get { key, json } => {
                    adnet_cli::routing_ops::DhtExtraCmd::Get {
                        key: key.clone(),
                        json: *json,
                    }
                }
            };
            adnet_cli::routing_ops::run_dht_extra(&dht_cmd, &node).await?;
        }

        Cmd::Swarm { sub } => {
            let swarm_cmd = match sub {
                adnet_cli::cli::SwarmCmd::Peers { json } => {
                    adnet_cli::routing_ops::SwarmCmd::Peers { json: *json }
                }
                adnet_cli::cli::SwarmCmd::Connect { addr } => {
                    adnet_cli::routing_ops::SwarmCmd::Connect { addr: addr.clone() }
                }
                adnet_cli::cli::SwarmCmd::Disconnect { peer_id } => {
                    adnet_cli::routing_ops::SwarmCmd::Disconnect {
                        peer_id: peer_id.clone(),
                    }
                }
                adnet_cli::cli::SwarmCmd::Addrs { json } => {
                    adnet_cli::routing_ops::SwarmCmd::Addrs { json: *json }
                }
                adnet_cli::cli::SwarmCmd::Filters { json } => {
                    adnet_cli::routing_ops::SwarmCmd::Filters { json: *json }
                }
            };
            adnet_cli::routing_ops::run_swarm(&swarm_cmd, &data_dir).await?;
        }
    }

    Ok(())
}
