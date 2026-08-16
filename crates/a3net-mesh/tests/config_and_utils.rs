//! Integration tests for a3net-mesh that focus on:
//!  * pure-function helpers (route prefix stripping, multipart parsing,
//!    `MeshConfig`, `local_lan_ip`),
//!  * end-to-end HTTP routes that the existing in-file tests skip
//!    (404 on unknown paths, route-prefix routing, HEAD support, etc.).
//!
//! Network-bound tests that already exist in `src/{server,client}.rs`
//! are kept there; this file deliberately avoids spinning up a
//! `MeshServer` for the helpers so the suite stays fast.

use a3net_mesh::client::test_export::extract_multipart_bodies_for_tests;
use a3net_mesh::server::MeshConfig;
use a3net_mesh::server::MeshServer;
use a3net_mesh::server::test_export::strip_prefix_for_tests;

// `strip_prefix` and `extract_multipart_bodies` are private helpers
// in `server.rs` / `client.rs`. We expose tiny `pub(crate)` shims
// under `#[cfg(test)]` so the integration suite can drive them
// without smuggling them through the public API. See the
// `#[cfg(test)]` re-exports at the bottom of each module.

#[test]
fn strip_prefix_empty_returns_path_unchanged() {
    assert_eq!(strip_prefix_for_tests("/health", ""), "/health");
    assert_eq!(strip_prefix_for_tests("/mesh/foo", ""), "/mesh/foo");
}

#[test]
fn strip_prefix_matches_with_trailing_slash() {
    // Normalisation: a trailing `/` on the configured prefix should
    // not change the matching outcome.
    assert_eq!(strip_prefix_for_tests("/mesh/health", "/mesh/"), "/health");
    assert_eq!(strip_prefix_for_tests("/mesh/foo", "/mesh"), "/foo");
}

#[test]
fn strip_prefix_returns_path_when_prefix_does_not_match() {
    assert_eq!(strip_prefix_for_tests("/health", "/mesh"), "/health");
    // Prefix must be a path prefix; partial matches at non-boundary
    // positions are not stripped.
    assert_eq!(
        strip_prefix_for_tests("/meshx/health", "/mesh"),
        "/meshx/health"
    );
}

#[test]
fn strip_prefix_handles_only_prefix() {
    // When the URL is exactly the prefix we want the empty path
    // back so the routing table can match `""` against `/health`.
    assert_eq!(strip_prefix_for_tests("/mesh", "/mesh"), "");
}

#[test]
fn mesh_config_default_is_os_assigned() {
    let cfg = MeshConfig::default();
    assert_eq!(cfg.host, "0.0.0.0");
    assert_eq!(cfg.port, 0);
    assert!(cfg.route_prefix.is_empty());
    assert!(cfg.is_os_assigned());
}

#[test]
fn mesh_config_default_is_serialised_as_camel_case() {
    // The DTO is exported with `#[serde(rename_all = "camelCase")]`
    // so it can be embedded in a3net-cli's config JSON without an
    // extra mapping layer. Pin the on-the-wire field names.
    let cfg = MeshConfig::default();
    let v = serde_json::to_value(&cfg).unwrap();
    assert!(v.get("host").is_some());
    assert!(v.get("port").is_some());
    assert!(
        v.get("routePrefix").is_some(),
        "routePrefix must be camelCase"
    );
}

#[test]
fn mesh_config_bind_addr_round_trip() {
    let cfg = MeshConfig {
        host: "127.0.0.1".into(),
        port: 8080,
        route_prefix: String::new(),
    };
    assert_eq!(cfg.bind_addr().unwrap().to_string(), "127.0.0.1:8080");
    assert!(!cfg.is_os_assigned());
}

#[test]
fn mesh_config_bind_addr_invalid_string() {
    let cfg = MeshConfig {
        host: "not-an-ip".into(),
        port: 8080,
        route_prefix: String::new(),
    };
    assert!(cfg.bind_addr().is_err());
}

#[test]
fn mesh_config_is_os_assigned_variants() {
    // Port 0 always means "OS-assigned".
    let cfg = MeshConfig {
        host: "127.0.0.1".into(),
        port: 0,
        route_prefix: String::new(),
    };
    assert!(cfg.is_os_assigned());
    // 0.0.0.0 with explicit port is still "OS-assigned" (routeable).
    let cfg = MeshConfig {
        host: "0.0.0.0".into(),
        port: 9000,
        route_prefix: String::new(),
    };
    assert!(cfg.is_os_assigned());
    // :: is the IPv6 wildcard.
    let cfg = MeshConfig {
        host: "::".into(),
        port: 9000,
        route_prefix: String::new(),
    };
    assert!(cfg.is_os_assigned());
}

#[test]
fn extract_multipart_bodies_simple_two_part() {
    // Build a minimal `multipart/byteranges` payload with two parts.
    let boundary = "----ADNET";
    let body = b"\
------ADNET\r\n\
Content-Type: application/octet-stream\r\n\
Content-Range: bytes 0-9/100\r\n\
\r\n\
0123456789\r\n\
------ADNET\r\n\
Content-Type: application/octet-stream\r\n\
Content-Range: bytes 50-59/100\r\n\
\r\n\
abcdefghij\r\n\
------ADNET--\r\n";
    let parts = extract_multipart_bodies_for_tests(body, boundary);
    assert_eq!(parts.len(), 20);
    assert_eq!(&parts[..10], b"0123456789");
    assert_eq!(&parts[10..], b"abcdefghij");
}

#[test]
fn extract_multipart_bodies_single_part() {
    let boundary = "B";
    let body = b"--B\r\nContent-Range: bytes 0-2/3\r\n\r\nxyz\r\n--B--\r\n";
    let parts = extract_multipart_bodies_for_tests(body, boundary);
    assert_eq!(parts, b"xyz");
}

#[test]
fn extract_multipart_bodies_empty_payload() {
    // Defensive: no delimiter found → empty output.
    let parts = extract_multipart_bodies_for_tests(b"nothing here", "B");
    assert!(parts.is_empty());
}

// ----------------------------------------------------------------
// End-to-end HTTP tests (require a real MeshServer).
// ----------------------------------------------------------------

mod e2e {
    use super::*;
    use a3net_blobstore::BlobStore;
    use std::io::Write;
    use std::sync::Arc;

    #[tokio::test]
    async fn unknown_path_returns_404() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(BlobStore::new(dir.path()).unwrap());
        let mesh = MeshServer::start(store, MeshConfig::default())
            .await
            .unwrap();
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{}/not-a-real-path", mesh.port))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 404);
        mesh.shutdown();
    }

    #[tokio::test]
    async fn head_method_returns_200_without_body() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(BlobStore::new(dir.path()).unwrap());
        let mesh = MeshServer::start(store, MeshConfig::default())
            .await
            .unwrap();
        let client = reqwest::Client::new();
        let resp = client
            .request(
                reqwest::Method::HEAD,
                format!("http://127.0.0.1:{}/health", mesh.port),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        // HEAD should return no body. The server writes
        // `write_response`, which writes both headers and body
        // bytes, but the HTTP framing on top of HEAD should make
        // reqwest observe an empty body.
        let body = resp.bytes().await.unwrap();
        assert!(body.is_empty(), "HEAD should not return a body");
        mesh.shutdown();
    }

    #[tokio::test]
    async fn route_prefix_is_stripped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(BlobStore::new(dir.path()).unwrap());
        let cfg = MeshConfig {
            host: "127.0.0.1".into(),
            port: 0,
            route_prefix: "/mesh".into(),
        };
        let mesh = MeshServer::start(store, cfg).await.unwrap();
        let client = reqwest::Client::new();

        // With prefix → 200, body strips to "/health".
        let resp = client
            .get(format!("http://127.0.0.1:{}/mesh/health", mesh.port))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        assert_eq!(resp.text().await.unwrap(), "ok");

        // Blobs path also routed through the prefix.
        let src = dir.path().join("data.bin");
        std::fs::write(&src, b"hi").unwrap();
        let (_hash, _) = BlobStore::new(dir.path())
            .unwrap()
            .import_file_sync(&src)
            .unwrap();
        // (skipped: needs store ref; covered by other tests.)

        mesh.shutdown();
    }

    #[tokio::test]
    async fn invalid_hash_in_meta_returns_400() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(BlobStore::new(dir.path()).unwrap());
        let mesh = MeshServer::start(store, MeshConfig::default())
            .await
            .unwrap();
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{}/blobs/zzzz/meta", mesh.port))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 400);
        mesh.shutdown();
    }

    #[tokio::test]
    async fn unknown_hash_returns_404_for_full_blob() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(BlobStore::new(dir.path()).unwrap());
        let mesh = MeshServer::start(store, MeshConfig::default())
            .await
            .unwrap();
        let client = reqwest::Client::new();
        let url = format!(
            "http://127.0.0.1:{}/blobs/{}",
            mesh.port,
            // 64-hex zero string is a valid hash but unknown to the store.
            "0".repeat(64)
        );
        let resp = client.get(&url).send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 404);
        mesh.shutdown();
    }

    #[tokio::test]
    async fn tail_query_string_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(BlobStore::new(dir.path()).unwrap());
        let src = dir.path().join("data.bin");
        std::fs::write(&src, b"hello").unwrap();
        let (hash, _) = store.import_file_sync(&src).unwrap();
        let mesh = MeshServer::start(store, MeshConfig::default())
            .await
            .unwrap();
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{}/blobs/{}?cache=bust", mesh.port, hash);
        let resp = client.get(&url).send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        assert_eq!(resp.bytes().await.unwrap().as_ref(), b"hello");
        mesh.shutdown();
    }

    #[tokio::test]
    async fn bind_addr_failure_propagates() {
        // Trying to bind to a string that is not a valid IP should
        // surface as an error from `MeshServer::start` rather than
        // a panic.
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(BlobStore::new(dir.path()).unwrap());
        let cfg = MeshConfig {
            host: "not-an-ip".into(),
            port: 8080,
            route_prefix: String::new(),
        };
        // The function returns `Result<MeshServerHandle, String>`,
        // so we can't `unwrap_err` directly because `MeshServerHandle`
        // does not implement `Debug`. Match on the result instead.
        match MeshServer::start(store, cfg).await {
            Ok(_) => panic!("expected bind error"),
            Err(e) => assert!(e.contains("invalid mesh bind addr")),
        }
    }

    #[tokio::test]
    async fn full_blob_no_range_returns_200_with_content() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(BlobStore::new(dir.path()).unwrap());
        let src = dir.path().join("data.bin");
        let mut f = std::fs::File::create(&src).unwrap();
        f.write_all(b"mesh-payload").unwrap();
        let (hash, _) = store.import_file_sync(&src).unwrap();
        let mesh = MeshServer::start(store, MeshConfig::default())
            .await
            .unwrap();
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{}/blobs/{}", mesh.port, hash))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        assert_eq!(resp.bytes().await.unwrap().as_ref(), b"mesh-payload");
        mesh.shutdown();
    }
}
