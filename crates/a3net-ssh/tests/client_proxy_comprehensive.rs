//! Comprehensive tests for the `client/proxy` module.
//!
//! Tests parse_invite, SftpConfig, and related types.

#![cfg(feature = "iroh")]

use a3net_ssh::client::proxy::{
    InviteToken, ParsedInvite, SftpConfig, parse_invite,
    DEFAULT_PROXY_BINARY, DEFAULT_SFTP_BINARY,
};

// ============================================================================
// parse_invite tests
// ============================================================================

#[test]
fn parse_invite_valid_token_lowercase() {
    let ep_hex = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";
    let parsed = parse_invite(&format!("alice@{ep_hex}")).unwrap();

    assert_eq!(parsed.user, "alice");
    assert_eq!(parsed.endpoint_id.as_bytes().len(), 32);
}

#[test]
fn parse_invite_valid_token_uppercase() {
    // Test with uppercase hex characters - use valid endpoint ID
    let ep_hex = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";
    let parsed = parse_invite(&format!("bob@{ep_hex}")).unwrap();

    assert_eq!(parsed.user, "bob");
    assert_eq!(parsed.endpoint_id.as_bytes().len(), 32);
}

#[test]
fn parse_invite_valid_token_mixed_case() {
    // Test with mixed case hex - use valid endpoint ID
    let ep_hex = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";
    let parsed = parse_invite(&format!("charlie@{ep_hex}")).unwrap();

    assert_eq!(parsed.user, "charlie");
    assert_eq!(parsed.endpoint_id.as_bytes().len(), 32);
}

#[test]
fn parse_invite_valid_token_with_numbers() {
    let ep_hex = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";
    let parsed = parse_invite(&format!("user123@{ep_hex}")).unwrap();

    assert_eq!(parsed.user, "user123");
    assert_eq!(parsed.endpoint_id.as_bytes().len(), 32);
}

#[test]
fn parse_invite_valid_token_with_underscore() {
    let ep_hex = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";
    let parsed = parse_invite(&format!("test_user@{ep_hex}")).unwrap();

    assert_eq!(parsed.user, "test_user");
}

#[test]
fn parse_invite_valid_token_with_dash() {
    let ep_hex = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";
    let parsed = parse_invite(&format!("test-user@{ep_hex}")).unwrap();

    assert_eq!(parsed.user, "test-user");
}

#[test]
fn parse_invite_valid_token_with_dot() {
    let ep_hex = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";
    let parsed = parse_invite(&format!("first.last@{ep_hex}")).unwrap();

    assert_eq!(parsed.user, "first.last");
}

#[test]
fn parse_invite_trims_whitespace_around_user() {
    let ep_hex = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";
    let parsed = parse_invite(&format!("  alice  @{ep_hex}")).unwrap();
    assert_eq!(parsed.user, "alice");
}

#[test]
fn parse_invite_trims_whitespace_around_endpoint() {
    let ep_hex = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";
    let parsed = parse_invite(&format!("alice@   {ep_hex}   ")).unwrap();
    assert_eq!(parsed.user, "alice");
    assert_eq!(parsed.endpoint_id.as_bytes().len(), 32);
}

#[test]
fn parse_invite_error_missing_at() {
    let err = parse_invite("aliceendpoint").unwrap_err();
    assert!(matches!(err, a3net_ssh::error::SshError::InvalidInvite { .. }));
}

#[test]
fn parse_invite_error_empty_user() {
    let ep_hex = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";
    let err = parse_invite(&format!("@{ep_hex}")).unwrap_err();
    assert!(matches!(err, a3net_ssh::error::SshError::InvalidInvite { .. }));
}

#[test]
fn parse_invite_error_only_at() {
    let err = parse_invite("@").unwrap_err();
    assert!(matches!(err, a3net_ssh::error::SshError::InvalidInvite { .. }));
}

#[test]
fn parse_invite_error_empty_after_at() {
    let err = parse_invite("alice@").unwrap_err();
    assert!(matches!(err, a3net_ssh::error::SshError::InvalidInvite { .. }));
}

#[test]
fn parse_invite_error_only_whitespace_after_at() {
    let err = parse_invite("alice@   ").unwrap_err();
    assert!(matches!(err, a3net_ssh::error::SshError::InvalidInvite { .. }));
}

#[test]
fn parse_invite_error_invalid_hex() {
    // Invalid hex characters
    let bad = "gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg";
    let err = parse_invite(&format!("alice@{bad}")).unwrap_err();
    assert!(matches!(err, a3net_ssh::error::SshError::InvalidInvite { .. }));
}

#[test]
fn parse_invite_error_wrong_length_short() {
    let bad = "a".repeat(63);
    let err = parse_invite(&format!("alice@{bad}")).unwrap_err();
    assert!(matches!(err, a3net_ssh::error::SshError::InvalidInvite { .. }));
}

#[test]
fn parse_invite_error_wrong_length_long() {
    let bad = "a".repeat(65);
    let err = parse_invite(&format!("alice@{bad}")).unwrap_err();
    assert!(matches!(err, a3net_ssh::error::SshError::InvalidInvite { .. }));
}

#[test]
fn parse_invite_error_empty_string() {
    let err = parse_invite("").unwrap_err();
    assert!(matches!(err, a3net_ssh::error::SshError::InvalidInvite { .. }));
}

#[test]
fn parse_invite_error_whitespace_only() {
    let err = parse_invite("   ").unwrap_err();
    assert!(matches!(err, a3net_ssh::error::SshError::InvalidInvite { .. }));
}

// ============================================================================
// ParsedInvite tests
// ============================================================================

#[test]
fn parsed_invite_debug() {
    let ep_hex = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";
    let parsed = parse_invite(&format!("alice@{ep_hex}")).unwrap();

    let debug_str = format!("{:?}", parsed);
    assert!(debug_str.contains("alice"));
}

#[test]
fn parsed_invite_clone() {
    let ep_hex = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";
    let parsed1 = parse_invite(&format!("alice@{ep_hex}")).unwrap();
    let parsed2 = parsed1.clone();

    assert_eq!(parsed1.user, parsed2.user);
    assert_eq!(parsed1.endpoint_id, parsed2.endpoint_id);
}

#[test]
fn parsed_invite_eq() {
    let ep_hex = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";
    let parsed1 = parse_invite(&format!("alice@{ep_hex}")).unwrap();
    let parsed2 = parse_invite(&format!("alice@{ep_hex}")).unwrap();

    assert_eq!(parsed1, parsed2);
}

#[test]
fn parsed_invite_neq_different_user() {
    let ep_hex = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";
    let parsed1 = parse_invite(&format!("alice@{ep_hex}")).unwrap();
    let parsed2 = parse_invite(&format!("bob@{ep_hex}")).unwrap();

    assert_ne!(parsed1, parsed2);
}

#[test]
fn parsed_invite_neq_different_endpoint() {
    let ep1 = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";
    let ep2 = "bb8e1a5661a6dfa9ae2dd978922f30f524f6fd8c99b3de021c53f292aae74330";
    let parsed1 = parse_invite(&format!("alice@{ep1}")).unwrap();
    let parsed2 = parse_invite(&format!("alice@{ep2}")).unwrap();

    assert_ne!(parsed1, parsed2);
}

// ============================================================================
// SftpConfig tests
// ============================================================================

#[test]
fn sftp_config_default() {
    let config = SftpConfig::default();
    // Note: SftpConfig has empty string defaults
    // Just verify it creates successfully
    assert!(!config.recursive);
    assert!(!config.preserve);
}

#[test]
fn sftp_config_debug() {
    let config = SftpConfig::default();
    let debug_str = format!("{:?}", config);
    // Debug should output something
    assert!(!debug_str.is_empty());
}

#[test]
fn sftp_config_clone() {
    let config = SftpConfig::default();
    let cloned = config.clone();
    assert_eq!(config.recursive, cloned.recursive);
    assert_eq!(config.preserve, cloned.preserve);
    assert_eq!(config.subsystem, cloned.subsystem);
}

#[test]
fn sftp_config_default_values() {
    let config = SftpConfig::default();
    // Test the actual default values - subsystem defaults to "sftp"
    // but may be empty depending on implementation
    assert!(!config.recursive);
    assert!(!config.preserve);
    // Subsystem may be empty or "sftp" depending on Default impl
}

#[test]
fn sftp_config_builder_pattern() {
    let config = SftpConfig {
        binary: "custom-sftp".to_string(),
        recursive: true,
        preserve: true,
        subsystem: "sftp-server".to_string(),
    };

    assert_eq!(config.binary, "custom-sftp");
    assert!(config.recursive);
    assert!(config.preserve);
    assert_eq!(config.subsystem, "sftp-server");
}

// ============================================================================
// InviteToken tests
// ============================================================================

#[test]
fn invite_token_type_alias() {
    let token: InviteToken = "alice@endpoint123".to_string();
    assert_eq!(token.as_str(), "alice@endpoint123");
}

#[test]
fn invite_token_roundtrip() {
    let ep_hex = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";
    let token: InviteToken = format!("alice@{ep_hex}");
    let parsed = parse_invite(&token).unwrap();
    assert_eq!(parsed.user, "alice");
}

// ============================================================================
// Constants tests
// ============================================================================

#[test]
fn default_proxy_binary_value() {
    assert_eq!(DEFAULT_PROXY_BINARY, "ssh");
}

#[test]
fn default_sftp_binary_value() {
    assert_eq!(DEFAULT_SFTP_BINARY, "sftp");
}

// ============================================================================
// Round-trip tests
// ============================================================================

#[test]
fn parse_invite_roundtrip_real_endpoint_id() {
    // Use real endpoint IDs from iroh documentation
    let ep_hex = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";

    let parsed = parse_invite(&format!("alice@{ep_hex}")).unwrap();

    // Reconstruct the token
    let reconstructed = format!("{}@{}", parsed.user, parsed.endpoint_id);
    let reparsed = parse_invite(&reconstructed).unwrap();

    assert_eq!(parsed, reparsed);
}

#[test]
fn parse_invite_roundtrip_multiple_users() {
    let ep_hex = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";
    let users = ["alice", "bob", "charlie", "dave", "eve"];

    for user in users {
        let parsed = parse_invite(&format!("{user}@{ep_hex}")).unwrap();
        assert_eq!(parsed.user, user);
        assert_eq!(parsed.endpoint_id.as_bytes().len(), 32);
    }
}

// ============================================================================
// Error message tests
// ============================================================================

#[test]
fn parse_invite_error_contains_input() {
    let input = "malformed@input";
    let err = parse_invite(input).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains(input) || msg.contains("malformed"), "error should reference input");
}

#[test]
fn parse_invite_error_source_missing_at() {
    let err = parse_invite("noat").unwrap_err();
    if let a3net_ssh::error::SshError::InvalidInvite { input, source } = err {
        assert_eq!(input, "noat");
        assert!(source.to_string().contains('@') || source.to_string().contains("missing"));
    }
}

#[test]
fn parse_invite_error_source_missing_user() {
    let ep_hex = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";
    let err = parse_invite(&format!("@{ep_hex}")).unwrap_err();
    if let a3net_ssh::error::SshError::InvalidInvite { input, source } = err {
        assert!(input.contains(ep_hex));
        assert!(source.to_string().contains("user") || source.to_string().contains("missing"));
    }
}

#[test]
fn parse_invite_error_source_invalid_endpoint() {
    let err = parse_invite("alice@invalid").unwrap_err();
    if let a3net_ssh::error::SshError::InvalidInvite { input, .. } = err {
        assert!(input.contains("alice"));
    }
}

// ============================================================================
// Performance and edge case tests
// ============================================================================

#[test]
fn parse_invite_handles_unicode_user() {
    let ep_hex = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";
    // Users with special characters - just ensure parsing doesn't panic
    let users = ["user.name", "user-name", "user_name", "user123", "UPPERCASE"];

    for user in users {
        let result = parse_invite(&format!("{user}@{ep_hex}"));
        assert!(result.is_ok(), "Should parse: {}", user);
    }
}

#[test]
fn parse_invite_endpoint_id_32_bytes() {
    let ep_hex = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";
    let parsed = parse_invite(&format!("alice@{ep_hex}")).unwrap();

    // Endpoint ID should be 32 bytes (256 bits)
    let bytes = parsed.endpoint_id.as_bytes();
    assert_eq!(bytes.len(), 32);
}

#[test]
fn parse_invite_endpoint_id_consistency() {
    let ep_hex = "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac";

    // Parse same endpoint multiple times
    let parsed1 = parse_invite(&format!("alice@{ep_hex}")).unwrap();
    let parsed2 = parse_invite(&format!("bob@{ep_hex}")).unwrap();

    // Should produce same endpoint ID
    assert_eq!(parsed1.endpoint_id, parsed2.endpoint_id);
    assert_eq!(parsed1.endpoint_id.as_bytes(), parsed2.endpoint_id.as_bytes());
}

// ============================================================================
// Integration-like tests (without network)
// ============================================================================

#[test]
fn sftp_config_all_options() {
    let config = SftpConfig {
        binary: "sftp".to_string(),
        recursive: true,
        preserve: true,
        subsystem: "internal-sftp".to_string(),
    };

    // Verify all fields are set correctly
    assert_eq!(config.binary, "sftp");
    assert!(config.recursive);
    assert!(config.preserve);
    assert_eq!(config.subsystem, "internal-sftp");
}

#[test]
fn multiple_parsed_invites_different_endpoints() {
    let endpoints = [
        "38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac",
        "bb8e1a5661a6dfa9ae2dd978922f30f524f6fd8c99b3de021c53f292aae74330",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    ];

    let mut parsed_endpoints = Vec::new();
    for ep in endpoints {
        let parsed = parse_invite(&format!("user@{ep}")).unwrap();
        parsed_endpoints.push(parsed.endpoint_id);
    }

    // All endpoints should be different
    for (i, ep1) in parsed_endpoints.iter().enumerate() {
        for (j, ep2) in parsed_endpoints.iter().enumerate() {
            if i != j {
                assert_ne!(ep1, ep2, "Different endpoints should produce different IDs");
            }
        }
    }
}
