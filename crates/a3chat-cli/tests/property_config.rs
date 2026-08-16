//! Property tests for the config layer.

use std::path::PathBuf;

use a3chat_cli::config::{validate_owner, CliConfig, OutputFormat};

#[test]
fn validate_owner_accepts_any_64_char_hex() {
    // Build all 64-char hex strings of the same letter — should
    // all pass. Use a few representative digits.
    for c in "0123456789abcdefABCDEF".chars() {
        let s: String = std::iter::repeat(c).take(64).collect();
        assert!(validate_owner(&s).is_ok(), "rejected valid hex {c}");
    }
}

#[test]
fn validate_owner_rejects_when_char_count_off_by_one() {
    let mut s: String = std::iter::repeat('a').take(64).collect();
    assert!(validate_owner(&s).is_ok());
    s.push('a');
    assert!(validate_owner(&s).is_err(), "len=65 must fail");
    s.pop();
    s.pop();
    assert!(validate_owner(&s).is_err(), "len=63 must fail");
}

#[test]
fn effective_values_fall_back_to_constants_for_default() {
    let c = CliConfig::default();
    assert_eq!(c.effective_output(), OutputFormat::Table);
}

#[test]
fn validate_daemon_url_rejects_garbage() {
    let mut c = CliConfig::default();
    for bad in ["", "abc", "ftp://x", "file:///etc/passwd"] {
        c.daemon_url = Some(bad.to_string());
        assert!(c.validate().is_err(), "should reject {bad}");
    }
}

#[test]
fn default_config_path_returns_a_path() {
    // XDG_CONFIG_HOME may be unset in test env, but HOME always is
    // for a normal Cargo test invocation.
    let p: PathBuf = a3chat_cli::config::default_config_path().unwrap();
    assert!(p.ends_with("config.toml"));
}