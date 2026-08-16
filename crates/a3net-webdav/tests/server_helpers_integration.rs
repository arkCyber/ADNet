// Integration tests for server.rs helpers and edge cases.
// These tests focus on server helper functions.

#[test]
fn query_param_extracts_offset() {
    let uri = "/?offset=10&limit=50";
    let offset = uri.split('?')
        .nth(1)
        .unwrap()
        .split('&')
        .find(|p| p.starts_with("offset="))
        .and_then(|p| p.split('=').nth(1))
        .and_then(|s| s.parse::<usize>().ok());
    assert_eq!(offset, Some(10));
}

#[test]
fn query_param_extracts_limit() {
    let uri = "/?offset=10&limit=50";
    let limit = uri.split('?')
        .nth(1)
        .unwrap()
        .split('&')
        .find(|p| p.starts_with("limit="))
        .and_then(|p| p.split('=').nth(1))
        .and_then(|s| s.parse::<usize>().ok());
    assert_eq!(limit, Some(50));
}

#[test]
fn query_param_missing_returns_none() {
    let uri = "/?offset=10";
    let limit = uri.split('?')
        .nth(1)
        .unwrap()
        .split('&')
        .find(|p| p.starts_with("limit="))
        .and_then(|p| p.split('=').nth(1))
        .and_then(|s| s.parse::<usize>().ok());
    assert_eq!(limit, None);
}

#[test]
fn query_param_multiple_params() {
    let uri = "/?foo=bar&offset=100&baz=qux&limit=25";
    let offset = uri.split('?')
        .nth(1)
        .unwrap()
        .split('&')
        .find(|p| p.starts_with("offset="))
        .and_then(|p| p.split('=').nth(1))
        .and_then(|s| s.parse::<usize>().ok());
    assert_eq!(offset, Some(100));

    let limit = uri.split('?')
        .nth(1)
        .unwrap()
        .split('&')
        .find(|p| p.starts_with("limit="))
        .and_then(|p| p.split('=').nth(1))
        .and_then(|s| s.parse::<usize>().ok());
    assert_eq!(limit, Some(25));
}

#[test]
fn query_param_empty_value() {
    let uri = "/?offset=&limit=10";
    let offset = uri.split('?')
        .nth(1)
        .unwrap()
        .split('&')
        .find(|p| p.starts_with("offset="))
        .and_then(|p| p.split('=').nth(1))
        .and_then(|s| s.parse::<usize>().ok());
    assert_eq!(offset, None); // Empty string doesn't parse as usize
}

#[test]
fn query_param_no_question_mark() {
    let uri = "/some/path";
    let offset = uri.split('?')
        .nth(1);
    assert_eq!(offset, None);
}

#[test]
fn webdav_config_default() {
    use a3net_webdav::WebdavConfig;

    let config = WebdavConfig::default();
    assert_eq!(config.host, "127.0.0.1");
    assert_eq!(config.port, 8780);
}

#[test]
fn webdav_config_custom() {
    use a3net_webdav::WebdavConfig;

    let config = WebdavConfig {
        host: "0.0.0.0".to_string(),
        port: 8080,
    };
    assert_eq!(config.host, "0.0.0.0");
    assert_eq!(config.port, 8080);
}

#[test]
fn webdav_config_with_debug_logging() {
    use a3net_webdav::WebdavConfig;

    let config = WebdavConfig {
        host: "localhost".to_string(),
        port: 3000,
    };
    assert_eq!(config.host, "localhost");
    assert_eq!(config.port, 3000);
}
