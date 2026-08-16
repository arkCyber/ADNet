//! Tests for the IPC client module.

use std::path::PathBuf;

#[test]
fn ipc_client_socket_path_default() {
    let client = a3net_cli::IpcClient::connect("/tmp/a3net-data");
    let path = client.socket_path();
    assert_eq!(path, Some(PathBuf::from("/tmp/a3net-data/ipc.sock")));
}

#[test]
fn ipc_client_socket_path_with_nested_dir() {
    let client = a3net_cli::IpcClient::connect("/home/user/.a3net/my-node");
    let path = client.socket_path();
    assert_eq!(path, Some(PathBuf::from("/home/user/.a3net/my-node/ipc.sock")));
}

#[test]
fn ipc_client_is_not_running_when_socket_missing() {
    let client = a3net_cli::IpcClient::connect("/nonexistent/path/12345");
    assert!(!client.is_daemon_running());
}

#[test]
fn ipc_client_clone_is_independent() {
    let client1 = a3net_cli::IpcClient::connect("/tmp/test");
    let client2 = client1.clone();
    assert_eq!(client1.socket_path(), client2.socket_path());
}

#[test]
fn ipc_client_http_construction() {
    let client = a3net_cli::IpcClient::http("127.0.0.1");
    assert_eq!(client.as_http_url(), Some("http://127.0.0.1:11436".to_string()));
}

#[test]
fn ipc_client_http_custom_url() {
    let client = a3net_cli::IpcClient::http_url("http://localhost:8080");
    assert_eq!(client.as_http_url(), Some("http://localhost:8080".to_string()));
    assert!(client.socket_path().is_none());
}

#[test]
fn ipc_client_client_from_cli_http() {
    let client = a3net_cli::client_from_cli(None, Some("127.0.0.1"), None).unwrap();
    assert_eq!(client.as_http_url(), Some("http://127.0.0.1:11436".to_string()));
}

#[test]
fn ipc_client_client_from_cli_data_dir() {
    let client = a3net_cli::client_from_cli(Some("/tmp/my-data"), None, None).unwrap();
    assert_eq!(client.socket_path(), Some(PathBuf::from("/tmp/my-data/ipc.sock")));
}

#[test]
fn ipc_client_client_from_cli_http_with_port() {
    let client = a3net_cli::client_from_cli(None, Some("localhost"), Some(8080)).unwrap();
    assert_eq!(client.as_http_url(), Some("http://localhost:8080".to_string()));
}
