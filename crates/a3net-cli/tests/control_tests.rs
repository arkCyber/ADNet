//! Tests for the control module.

use std::path::Path;

#[test]
fn format_bytes_under_kb() {
    assert_eq!(a3net_cli::format_bytes(0), "0 B");
    assert_eq!(a3net_cli::format_bytes(100), "100 B");
    assert_eq!(a3net_cli::format_bytes(1023), "1023 B");
}

#[test]
fn format_bytes_at_kb_boundary() {
    assert_eq!(a3net_cli::format_bytes(1024), "1.00 KB");
    assert_eq!(a3net_cli::format_bytes(1536), "1.50 KB");
}

#[test]
fn format_bytes_at_mb_boundary() {
    assert_eq!(a3net_cli::format_bytes(1024 * 1024), "1.00 MB");
    assert_eq!(a3net_cli::format_bytes(1024 * 1024 * 5), "5.00 MB");
}

#[test]
fn format_bytes_at_gb_boundary() {
    assert_eq!(a3net_cli::format_bytes(1024 * 1024 * 1024), "1.00 GB");
}

#[test]
fn health_check_defaults_when_no_daemon() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(async {
        a3net_cli::check_health(Path::new("/tmp/nonexistent-a3net-health-test"))
            .await
            .unwrap()
    });

    assert!(!result.ok);
    assert!(!result.daemon_running);
    assert!(!result.ipc_socket_exists);
    assert!(result.node_id.is_none());
    assert!(result.latency_ms.is_none());
}

#[test]
fn health_check_json_serialization() {
    let health = a3net_cli::HealthCheck {
        ok: true,
        daemon_running: true,
        ipc_socket_exists: true,
        ipc_socket_path: "/tmp/test/ipc.sock".to_string(),
        node_id: Some("abc123".to_string()),
        peer_count: Some(5),
        joined_rooms: Some(2),
        uptime_secs: Some(3600),
        mesh_listening: true,
        relay_running: Some(false),
        timestamp: "2024-01-01T00:00:00Z".to_string(),
        latency_ms: Some(10),
    };

    let json = serde_json::to_string(&health).unwrap();
    assert!(json.contains("\"ok\":true"));
    assert!(json.contains("\"daemon_running\":true"));
    assert!(json.contains("\"peer_count\":5"));
}

#[test]
fn storage_info_json_serialization() {
    let info = a3net_cli::StorageInfo {
        shared_blobs: 100,
        shared_bytes: 1024 * 1024,
        private_blobs: 50,
        private_bytes: 512 * 1024,
        total_bytes: 1536 * 1024,
    };

    let json = serde_json::to_string(&info).unwrap();
    assert!(json.contains("\"shared_blobs\":100"));
    assert!(json.contains("\"total_bytes\":1572864"));
}

#[test]
fn node_summary_json_serialization() {
    let summary = a3net_cli::NodeSummary {
        node_id: "abcd1234".to_string(),
        short_id: "abcd123".to_string(),
        peer_count: 3,
        gossip_topics: 2,
        joined_rooms: vec!["room1".to_string(), "room2".to_string()],
        mesh_host: Some("127.0.0.1".to_string()),
        mesh_port: Some(8080),
        relay_url: Some("http://relay.example.com".to_string()),
    };

    let json = serde_json::to_string(&summary).unwrap();
    assert!(json.contains("\"node_id\":\"abcd1234\""));
    assert!(json.contains("\"peer_count\":3"));
    assert!(json.contains("\"joined_rooms\":[\"room1\",\"room2\"]"));
}

#[test]
fn system_info_json_serialization() {
    let info = a3net_cli::SystemInfo {
        data_dir: "/tmp/a3net".to_string(),
        daemon: a3net_cli::DaemonStatus {
            running: true,
            ipc_socket: "/tmp/a3net/ipc.sock".to_string(),
            uptime_secs: Some(3600),
        },
        node: None,
        storage: None,
    };

    let json = serde_json::to_string(&info).unwrap();
    assert!(json.contains("\"data_dir\":\"/tmp/a3net\""));
    assert!(json.contains("\"running\":true"));
}
