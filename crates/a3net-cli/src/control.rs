//! Control system — monitor and control the running daemon.
//!
//! This module provides a unified interface for:
//! - Daemon health monitoring
//! - System status checks
//! - Process control operations
//!
//! # Usage
//!
//! ```ignore
//! use a3net_cli::control;
//!
//! // Check daemon health via Unix socket
//! let health = control::check_health(data_dir).await?;
//!
//! // Check daemon health via HTTP
//! let client = IpcClient::http("127.0.0.1");
//! let health = control::check_health_with_client(&client).await?;
//! ```

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use a3net_tui::{
    box_drawing::Box,
    color::Color,
    progress::human_bytes,
    widget::{alert_widget, section_header, Table},
};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::ipc_client::IpcClient;

/// Health check result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheck {
    pub ok: bool,
    pub daemon_running: bool,
    pub ipc_socket_exists: bool,
    pub ipc_socket_path: String,
    pub node_id: Option<String>,
    pub peer_count: Option<u32>,
    pub joined_rooms: Option<usize>,
    pub uptime_secs: Option<u64>,
    pub mesh_listening: bool,
    pub relay_running: Option<bool>,
    pub timestamp: String,
    pub latency_ms: Option<u64>,
}

/// System information summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    pub data_dir: String,
    pub daemon: DaemonStatus,
    pub node: Option<NodeSummary>,
    pub storage: Option<StorageInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub running: bool,
    pub ipc_socket: String,
    pub uptime_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeSummary {
    pub node_id: String,
    pub short_id: String,
    pub peer_count: u32,
    pub gossip_topics: u32,
    pub joined_rooms: Vec<String>,
    pub mesh_host: Option<String>,
    pub mesh_port: Option<u16>,
    pub relay_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageInfo {
    pub shared_blobs: u64,
    pub shared_bytes: u64,
    pub private_blobs: u64,
    pub private_bytes: u64,
    pub total_bytes: u64,
}

/// Check daemon health via IPC.
pub async fn check_health(data_dir: &Path) -> Result<HealthCheck> {
    let start = Instant::now();
    let ipc_socket = data_dir.join("ipc.sock");
    let ipc_socket_exists = ipc_socket.exists();

    let health = HealthCheck {
        ok: false,
        daemon_running: false,
        ipc_socket_exists,
        ipc_socket_path: ipc_socket.display().to_string(),
        node_id: None,
        peer_count: None,
        joined_rooms: None,
        uptime_secs: None,
        mesh_listening: false,
        relay_running: None,
        timestamp: chrono::Utc::now().to_rfc3339(),
        latency_ms: None,
    };

    if !ipc_socket_exists {
        return Ok(health);
    }

    let client = IpcClient::connect(data_dir);
    check_health_with_client(&client).await
}

/// Check daemon health using an existing IpcClient (supports HTTP and Unix socket).
pub async fn check_health_with_client(client: &IpcClient) -> Result<HealthCheck> {
    let start = Instant::now();

    // For Unix socket mode, check if socket exists first
    let (ipc_socket_path, ipc_socket_exists) = if let Some(path) = client.socket_path() {
        (path.display().to_string(), path.exists())
    } else {
        ("HTTP".to_string(), true) // HTTP mode - assume running
    };

    let health = HealthCheck {
        ok: false,
        daemon_running: false,
        ipc_socket_exists,
        ipc_socket_path,
        node_id: None,
        peer_count: None,
        joined_rooms: None,
        uptime_secs: None,
        mesh_listening: false,
        relay_running: None,
        timestamp: chrono::Utc::now().to_rfc3339(),
        latency_ms: None,
    };

    // For Unix socket, check socket exists
    if !ipc_socket_exists {
        return Ok(health);
    }

    match client.info().await {
        Ok(info) => {
            let latency_ms = start.elapsed().as_millis() as u64;
            Ok(HealthCheck {
                ok: true,
                daemon_running: true,
                ipc_socket_exists: true,
                ipc_socket_path: health.ipc_socket_path,
                node_id: Some(info.node_id.clone()),
                peer_count: None,
                joined_rooms: Some(info.joined_rooms.len()),
                uptime_secs: info.started_at.as_ref().and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(s).ok()
                }).map(|dt| {
                    (chrono::Utc::now() - dt.with_timezone(&chrono::Utc)).num_seconds() as u64
                }),
                mesh_listening: info.mesh.is_some(),
                relay_running: info.relay.as_ref().map(|_| true),
                timestamp: chrono::Utc::now().to_rfc3339(),
                latency_ms: Some(latency_ms),
            })
        }
        Err(e) => {
            Ok(HealthCheck {
                ok: false,
                daemon_running: true,
                ipc_socket_exists: true,
                ipc_socket_path: health.ipc_socket_path,
                latency_ms: Some(start.elapsed().as_millis() as u64),
                ..health
            })
        }
    }
}

/// Check daemon health via HTTP endpoint.
pub async fn check_health_http(host: &str, port: Option<u16>) -> Result<HealthCheck> {
    let client = IpcClient::http(host);
    if let Some(p) = port {
        let url = format!("http://{}:{}/rpc", host, p);
        let client = IpcClient::http_url(url);
        check_health_with_client(&client).await
    } else {
        check_health_with_client(&client).await
    }
}

/// Get full system information via IPC socket.
pub async fn get_system_info(data_dir: &Path) -> Result<SystemInfo> {
    let client = IpcClient::connect(data_dir);
    get_system_info_with_client(&client, data_dir).await
}

/// Get full system information using an existing IpcClient.
pub async fn get_system_info_with_client(client: &IpcClient, data_dir: &Path) -> Result<SystemInfo> {
    let mut info = SystemInfo {
        data_dir: data_dir.display().to_string(),
        daemon: DaemonStatus {
            running: false,
            ipc_socket: data_dir.join("ipc.sock").display().to_string(),
            uptime_secs: None,
        },
        node: None,
        storage: None,
    };

    // Check storage info (offline)
    if let Ok(storage) = get_storage_info(data_dir) {
        info.storage = Some(storage);
    }

    // Check daemon via IPC
    if !client.is_daemon_running() {
        return Ok(info);
    }

    info.daemon.running = true;

    if let Ok(node_info) = client.info().await {
        info.daemon.uptime_secs = node_info.started_at.as_ref().and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(s).ok()
        }).map(|dt| {
            (chrono::Utc::now() - dt.with_timezone(&chrono::Utc)).num_seconds() as u64
        });

        let short_id = if node_info.node_id.len() > 8 {
            &node_info.node_id[..8]
        } else {
            &node_info.node_id
        };

        info.node = Some(NodeSummary {
            node_id: node_info.node_id.clone(),
            short_id: short_id.to_string(),
            peer_count: 0,
            gossip_topics: 0,
            joined_rooms: node_info.joined_rooms,
            mesh_host: node_info.mesh.as_ref().map(|m| m.host.clone()),
            mesh_port: node_info.mesh.as_ref().map(|m| m.port),
            relay_url: node_info.relay.as_ref().map(|r| r.base_url.clone()),
        });
    }

    Ok(info)
}

/// Get storage information from filesystem.
fn get_storage_info(data_dir: &Path) -> Result<StorageInfo> {
    let shared = count_blobs(&data_dir.join("shared"));
    let private = count_blobs(&data_dir.join("private"));

    Ok(StorageInfo {
        shared_blobs: shared.0,
        shared_bytes: shared.1,
        private_blobs: private.0,
        private_bytes: private.1,
        total_bytes: shared.1 + private.1,
    })
}

fn count_blobs(scope_dir: &Path) -> (u64, u64) {
    let mut count = 0u64;
    let mut bytes = 0u64;

    let blobs = scope_dir.join("blobs");
    if !blobs.exists() {
        return (0, 0);
    }

    if let Ok(entries) = std::fs::read_dir(&blobs) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                count += 1;
                bytes += path.metadata().map(|m| m.len()).unwrap_or(0);
            } else if path.is_dir() {
                let (sub_count, sub_bytes) = count_blobs(&path);
                count += sub_count;
                bytes += sub_bytes;
            }
        }
    }

    (count, bytes)
}

/// Format bytes into human-readable string.
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Pretty print health check result.
pub fn print_health_check(health: &HealthCheck) {
    println!();
    println!(
        "{}",
        Color::Cyan
            .paint("╔═══════════════════════════════════════════════════════════════════╗")
            .bold()
    );
    println!(
        "{}",
        Color::Cyan
            .paint("║                      Daemon Health Check                          ║")
            .bold()
    );
    println!(
        "{}",
        Color::Cyan
            .paint("╚═══════════════════════════════════════════════════════════════════╝")
            .bold()
    );
    println!();

    // Status panel
    let status_color = if health.ok {
        Color::Green.paint("HEALTHY")
    } else {
        Color::Red.paint("UNHEALTHY")
    };
    let status = Box::with_title("Status")
        .add_field("Daemon", if health.daemon_running {
            Color::Green.paint("Running")
        } else {
            Color::Red.paint("Not Running")
        }.plain_text())
        .add_field("Status", status_color.plain_text())
        .add_field("IPC Socket", &health.ipc_socket_path);

    println!("{status}");
    println!();

    // Metrics panel
    let mut metrics = Box::with_title("Metrics");
    if let Some(node_id) = &health.node_id {
        metrics = metrics.add_field("Node ID", node_id);
    }
    if let Some(rooms) = health.joined_rooms {
        metrics = metrics.add_field("Joined Rooms", &rooms.to_string());
    }
    if let Some(uptime) = health.uptime_secs {
        metrics = metrics.add_field("Uptime", &format!("{}s", uptime));
    }
    metrics = metrics.add_field("Mesh", if health.mesh_listening {
        Color::Green.paint("Listening")
    } else {
        Color::Yellow.paint("Not Listening")
    }.plain_text());

    if let Some(latency) = health.latency_ms {
        metrics = metrics.add_field("Latency", &format!("{}ms", latency));
    }

    println!("{metrics}");
    println!();

    // Alerts
    if !health.ok {
        println!("{}", alert_widget("critical", "Daemon health check failed"));
        println!();
    }

    println!(
        "  {}  Updated: {}",
        Color::Dim.paint("ℹ"),
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
    );
    println!();
}

/// Pretty print system info.
pub fn print_system_info(info: &SystemInfo) {
    println!();
    println!(
        "{}",
        Color::Cyan
            .paint("╔═══════════════════════════════════════════════════════════════════╗")
            .bold()
    );
    println!(
        "{}",
        Color::Cyan
            .paint("║                    System Information                            ║")
            .bold()
    );
    println!(
        "{}",
        Color::Cyan
            .paint("╚═══════════════════════════════════════════════════════════════════╝")
            .bold()
    );
    println!();

    // Daemon status
    let daemon = Box::with_title("Daemon")
        .add_field("Status", if info.daemon.running {
            Color::Green.paint("Running")
        } else {
            Color::Red.paint("Not Running")
        }.plain_text())
        .add_field("IPC Socket", &info.daemon.ipc_socket);

    if let Some(uptime) = info.daemon.uptime_secs {
        let daemon = Box::with_title("Daemon")
            .add_field("Status", Color::Green.paint("Running").plain_text())
            .add_field("IPC Socket", &info.daemon.ipc_socket)
            .add_field("Uptime", &format!("{}s", uptime));
        println!("{daemon}");
    } else {
        println!("{daemon}");
    }
    println!();

    // Node info
    if let Some(node) = &info.node {
        let mesh_addr = node.mesh_host.as_ref()
            .map(|h| format!("{}:{}", h, node.mesh_port.unwrap_or(0)))
            .unwrap_or_else(|| "Not configured".to_string());

        let node_box = Box::with_title("Node")
            .add_field("Node ID", &node.node_id)
            .add_field("Short ID", &format!("a3net-{}", node.short_id))
            .add_field("Peers", &node.peer_count.to_string())
            .add_field("Mesh", &mesh_addr);

        println!("{node_box}");
        println!();

        // Rooms
        if !node.joined_rooms.is_empty() {
            println!("{}", section_header("Joined Rooms"));
            for room in &node.joined_rooms {
                println!("  {} {}", Color::Green.paint("•"), room);
            }
            println!();
        }

        // Relay
        if let Some(url) = &node.relay_url {
            println!("{}", section_header("Relay"));
            println!("  {}", url);
            println!();
        }
    } else {
        println!("{}", alert_widget("warn", "Node not reachable - daemon may not be running"));
        println!();
    }

    // Storage
    if let Some(storage) = &info.storage {
        println!("{}", section_header("Storage"));
        println!(
            "  Shared : {} blobs, {}",
            storage.shared_blobs,
            human_bytes(storage.shared_bytes)
        );
        println!(
            "  Private: {} blobs, {}",
            storage.private_blobs,
            human_bytes(storage.private_bytes)
        );
        println!(
            "  Total  : {}",
            human_bytes(storage.total_bytes)
        );
        println!();
    }

    println!(
        "  {}  Updated: {}",
        Color::Dim.paint("ℹ"),
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
    );
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_works() {
        assert_eq!(format_bytes(100), "100 B");
        assert_eq!(format_bytes(1024), "1.00 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.00 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.00 GB");
    }

    #[test]
    fn count_blobs_handles_missing_directory() {
        let result = count_blobs(Path::new("/nonexistent/path"));
        assert_eq!(result, (0, 0));
    }
}
