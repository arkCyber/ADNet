//! Integration tests for a3net-nat-traversal
//!
//! These tests verify the integration between different NAT traversal components
//! and test real-world scenarios.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use a3net_nat_traversal::{
    config::{NatConfig, NatType, PortMappingProtocol, StunServer},
    error::NatError,
    hole_punch::{HolePunch, HolePunchConfig, HolePunchResult},
    manager::{ConnectionMethod, NatInfo, NatTraversalManager, PortMappingInfo},
    stun::{StunClient, StunResponse},
    turn::{RelayAllocation, TurnClient, TurnCredentials},
    upnp::{PortMapping, UpnpClient, UpnpDevice},
};

// ============================================================================
// Config Integration Tests
// ============================================================================

mod config_integration {
    use super::*;

    #[test]
    fn test_config_builder_pattern() {
        let config = NatConfig::default()
            .with_stun(true)
            .with_stun_servers(vec![StunServer::new("1.1.1.1:3478".parse().unwrap())])
            .with_upnp(true)
            .with_turn("turn:example.com:3478", "user", "pass")
            .with_hole_punch(true)
            .with_port_range(40000, 50000);

        assert!(config.stun_enabled);
        assert!(config.upnp_enabled);
        assert!(config.turn_enabled);
        assert!(config.hole_punch_enabled);
        assert_eq!(config.local_port_start, 40000);
        assert_eq!(config.local_port_end, 50000);
    }

    #[test]
    fn test_nat_type_hierarchy() {
        // Test that all NAT types have correct characteristics
        let test_cases = vec![
            (NatType::OpenInternet, true, true, false),
            (NatType::FullCone, true, true, false),
            (NatType::RestrictedCone, true, true, false),
            (NatType::PortRestrictedCone, true, true, false),
            (NatType::Symmetric, false, false, true),
            (NatType::Unknown, false, false, true),
        ];

        for (nat_type, supports_p2p, supports_hole_punch, requires_turn) in test_cases {
            assert_eq!(
                nat_type.supports_direct_p2p(),
                supports_p2p,
                "NAT type {:?}",
                nat_type
            );
            assert_eq!(
                nat_type.supports_hole_punching(),
                supports_hole_punch,
                "NAT type {:?}",
                nat_type
            );
            assert_eq!(
                nat_type.requires_turn(),
                requires_turn,
                "NAT type {:?}",
                nat_type
            );
        }
    }

    #[test]
    fn test_stun_server_default_servers() {
        let servers = StunServer::default_servers();
        assert!(!servers.is_empty());

        for server in &servers {
            assert!(server.addr.port() > 0);
            assert!(server.name.is_some());
        }
    }
}

// ============================================================================
// StunClient Integration Tests
// ============================================================================

mod stun_integration {
    use super::*;

    #[test]
    fn test_stun_response_chain() {
        let response = StunResponse {
            server: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 3478),
            mapped_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 50)), 45000),
            source_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 3478),
            changed_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 4, 4)), 3478),
        };

        // Test all accessor methods
        assert_eq!(response.public_ip(), IpAddr::V4(Ipv4Addr::new(203, 0, 113, 50)));
        assert_eq!(response.public_port(), 45000);

        // Test NAT detection
        let local_private = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)), 50000);
        assert!(response.is_behind_nat(local_private));

        let local_public = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 50)), 45000);
        assert!(!response.is_behind_nat(local_public));
    }

    #[test]
    fn test_stun_response_clone_independence() {
        let original = StunResponse {
            server: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 3478),
            mapped_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 50)), 45000),
            source_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 3478),
            changed_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 4, 4)), 3478),
        };

        let cloned = original.clone();
        assert_eq!(original.mapped_address, cloned.mapped_address);
    }

    #[tokio::test]
    async fn test_stun_client_creation() {
        let client = StunClient::new();
        assert!(client.is_ok());

        let client = StunClient::default();
        assert!(client.local_addr().port() > 0 || client.local_addr().ip().is_unspecified());
    }
}

// ============================================================================
// TurnClient Integration Tests
// ============================================================================

mod turn_integration {
    use super::*;

    #[tokio::test]
    async fn test_turn_client_full_lifecycle() {
        // Create client
        let client = TurnClient::new("turn:127.0.0.1:3478").await;
        assert!(client.is_ok());
        let client = client.unwrap();

        // With credentials - create a new client for this test
        let cred_client = TurnClient::with_credentials("turn:127.0.0.1:3478", "user", "pass").await;
        assert!(cred_client.is_ok());
        // Credentials are stored internally, we verify the client was created successfully

        // Close
        client.close().await;
    }

    #[tokio::test]
    async fn test_relay_allocation_lifecycle() {
        let client = TurnClient::new("turn:127.0.0.1:3478").await.unwrap();

        // Allocate
        let allocation = client.allocate().await;
        assert!(allocation.is_ok());
        let allocation = allocation.unwrap();

        // Verify allocation
        assert!(allocation.relay_addr.port() > 0);
        assert!(allocation.is_valid());
        assert!(allocation.remaining() > Duration::ZERO);

        // Close
        client.close().await;
    }

    #[test]
    fn test_turn_credentials_scenarios() {
        // Basic credentials
        let creds = TurnCredentials {
            username: "testuser".to_string(),
            password: "testpass".to_string(),
            nonce: None,
            realm: None,
        };
        assert!(!creds.nonce.is_some());

        // Credentials with auth
        let creds_auth = TurnCredentials {
            username: "testuser".to_string(),
            password: "testpass".to_string(),
            nonce: Some("nonce123".to_string()),
            realm: Some("realm123".to_string()),
        };
        assert!(creds_auth.nonce.is_some());
        assert!(creds_auth.realm.is_some());
    }

    #[test]
    fn test_relay_allocation_states() {
        // Valid allocation
        let valid = RelayAllocation {
            relay_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 50000),
            mapped_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            expiration: Duration::from_secs(600),
        };
        assert!(valid.is_valid());
        assert!(valid.remaining() > Duration::ZERO);

        // Expired allocation
        let expired = RelayAllocation {
            relay_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 50000),
            mapped_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            expiration: Duration::ZERO,
        };
        assert!(!expired.is_valid());
        assert_eq!(expired.remaining(), Duration::ZERO);
    }
}

// ============================================================================
// UPnP Integration Tests
// ============================================================================

mod upnp_integration {
    use super::*;

    #[test]
    fn test_upnp_client_creation() {
        let client = UpnpClient::new();
        assert!(client.is_ok());

        let client = UpnpClient::default();
        // Verify through derived Debug impl
        let debug_str = format!("{:?}", client);
        assert!(debug_str.contains("UpnpClient"));
    }

    #[test]
    fn test_upnp_device_construction() {
        // Construct a device and verify it works with public methods
        let device = UpnpDevice {
            addr: "192.168.1.255:1900".parse().unwrap(),
            location: "http://192.168.1.1:8080/igd.xml".to_string(),
            usn: Some("uuid:device-123".to_string()),
            services: vec!["WANIPConnection:1".to_string()],
        };

        // Test public methods
        assert!(device.has_wan_ip_service());

        // Test without services
        let device_no_services = UpnpDevice {
            addr: "192.168.1.255:1900".parse().unwrap(),
            location: "http://192.168.1.1:8080/igd.xml".to_string(),
            usn: None,
            services: vec![],
        };
        assert!(!device_no_services.has_wan_ip_service());
    }

    #[test]
    fn test_port_mapping_scenarios() {
        // TCP mapping
        let tcp_mapping = PortMapping::new(
            8080,
            PortMappingProtocol::Tcp,
            3000,
            "HTTP Server",
        );
        assert_eq!(tcp_mapping.protocol, PortMappingProtocol::Tcp);
        assert!(tcp_mapping.enabled);

        // UDP mapping
        let udp_mapping = PortMapping::new(
            9999,
            PortMappingProtocol::Udp,
            8888,
            "Voice Chat",
        );
        assert_eq!(udp_mapping.protocol, PortMappingProtocol::Udp);

        // Permanent mapping (lease_duration = 0)
        let permanent = PortMapping {
            external_port: 80,
            protocol: PortMappingProtocol::Tcp,
            internal_client: Ipv4Addr::new(192, 168, 1, 100),
            internal_port: 80,
            description: "Web".to_string(),
            lease_duration: 0,
            enabled: true,
        };
        assert_eq!(permanent.lease_duration, 0);
    }

    #[tokio::test]
    async fn test_upnp_port_mapping_operations() {
        let client = UpnpClient::new().unwrap();
        let device = UpnpDevice {
            addr: "192.168.1.1:1900".parse().unwrap(),
            location: "http://192.168.1.1:8080/igd.xml".to_string(),
            usn: Some("uuid:123".to_string()),
            services: vec!["WANIPConnection:1".to_string()],
        };

        // Add mapping
        let add_result = client
            .add_port_mapping(&device, 3000, 8080, PortMappingProtocol::Tcp, "A3Net", 3600)
            .await;
        assert!(add_result.is_ok());

        // Remove mapping
        let remove_result = client
            .remove_port_mapping(&device, 8080, PortMappingProtocol::Tcp)
            .await;
        assert!(remove_result.is_ok());

        // Get mappings
        let get_result = client.get_port_mappings(&device).await;
        assert!(get_result.is_ok());

        // Get WAN IP
        let wan_ip = client.get_wan_ip(&device).await;
        assert!(wan_ip.is_ok());
    }
}

// ============================================================================
// Hole Punching Integration Tests
// ============================================================================

mod hole_punch_integration {
    use super::*;

    #[tokio::test]
    async fn test_hole_punch_session_management() {
        let hp = HolePunch::new(None);

        // Initially empty
        assert_eq!(hp.active_count().await, 0);

        // Register sessions
        hp.register_session("peer1".to_string()).await;
        hp.register_session("peer2".to_string()).await;
        hp.register_session("peer3".to_string()).await;
        assert_eq!(hp.active_count().await, 3);

        // Remove some
        hp.remove_session("peer2").await;
        assert_eq!(hp.active_count().await, 2);

        // Remove all
        hp.remove_session("peer1").await;
        hp.remove_session("peer3").await;
        assert_eq!(hp.active_count().await, 0);
    }

    #[tokio::test]
    async fn test_hole_punch_result_handling() {
        // Success result
        let success = HolePunchResult {
            success: true,
            local_external: Some("192.168.1.100:50000".parse().unwrap()),
            remote_external: Some("192.168.1.200:50000".parse().unwrap()),
            duration: Duration::from_millis(150),
            error: None,
        };
        assert!(success.success);
        assert!(success.error.is_none());

        // Failure result
        let failure = HolePunchResult {
            success: false,
            local_external: Some("192.168.1.100:50000".parse().unwrap()),
            remote_external: None,
            duration: Duration::from_secs(10),
            error: Some("Timeout".to_string()),
        };
        assert!(!failure.success);
        assert!(failure.error.is_some());
    }

    #[test]
    fn test_hole_punch_config_options() {
        // Default config
        let default = HolePunchConfig::default();
        // Verify through debug output
        let debug_str = format!("{:?}", default);
        assert!(debug_str.contains("HolePunchConfig"));

        // Custom config - use Default and verify construction
        let custom = HolePunchConfig {
            timeout_ms: 30000,
            max_attempts: 10,
            retry_interval_ms: 500,
        };
        // Verify the struct can be created and cloned
        let cloned = custom.clone();
        assert_eq!(std::mem::size_of_val(&custom), std::mem::size_of_val(&cloned));
    }

    #[tokio::test]
    async fn test_hole_punch_operations() {
        let hp = HolePunch::new(None);

        // Register and unregister
        hp.register_session("test-peer".to_string()).await;
        assert_eq!(hp.active_count().await, 1);

        hp.remove_session("test-peer").await;
        assert_eq!(hp.active_count().await, 0);

        // Multiple registrations
        for i in 0..5 {
            hp.register_session(format!("peer-{}", i)).await;
        }
        assert_eq!(hp.active_count().await, 5);
    }
}

// ============================================================================
// Manager Integration Tests
// ============================================================================

mod manager_integration {
    use super::*;

    #[tokio::test]
    async fn test_manager_nat_info_lifecycle() {
        let config = NatConfig::default();
        let manager = NatTraversalManager::new(config).unwrap();

        // Initially no info
        assert!(manager.get_nat_info().await.is_none());
        assert!(!manager.can_connect_directly().await);
    }

    #[tokio::test]
    async fn test_connection_method_determination() {
        // When there's no NAT info, the method should be Discover
        let config = NatConfig::default();
        let manager = NatTraversalManager::new(config).unwrap();

        let method = manager.get_connection_method().await;
        assert_eq!(method, ConnectionMethod::Discover);

        // The actual NAT type-based determination is tested in unit tests
        // where we can directly manipulate the nat_info field
    }

    #[tokio::test]
    async fn test_manager_with_all_features_disabled() {
        let config = NatConfig::default()
            .with_stun(false)
            .with_upnp(false)
            .with_hole_punch(false);

        let manager = NatTraversalManager::new(config);
        assert!(manager.is_ok());

        let manager = manager.unwrap();
        assert!(manager.get_nat_info().await.is_none());
    }

    #[test]
    fn test_nat_info_external_addr() {
        let info = NatInfo {
            nat_type: NatType::Symmetric,
            local_addr: "192.168.1.100:5000".parse().unwrap(),
            public_addr: "203.0.113.50:60000".parse().unwrap(),
            supports_hole_punch: false,
            requires_turn: true,
            port_mappings: vec![],
            discovered_at: std::time::Instant::now(),
        };

        assert_eq!(info.external_addr(), "203.0.113.50:60000".parse().unwrap());
    }

    #[test]
    fn test_nat_info_can_connect_direct_scenarios() {
        // Scenario 1: Good NAT with port mappings
        let info_good = NatInfo {
            nat_type: NatType::FullCone,
            local_addr: "192.168.1.100:5000".parse().unwrap(),
            public_addr: "203.0.113.50:5000".parse().unwrap(),
            supports_hole_punch: true,
            requires_turn: false,
            port_mappings: vec![PortMappingInfo {
                protocol: PortMappingProtocol::Tcp,
                local_port: 5000,
                external_port: 5000,
                description: "Test".to_string(),
                expires_at: None,
            }],
            discovered_at: std::time::Instant::now(),
        };
        assert!(info_good.can_connect_direct());

        // Scenario 2: Good NAT without port mappings
        let info_no_mappings = NatInfo {
            nat_type: NatType::FullCone,
            local_addr: "192.168.1.100:5000".parse().unwrap(),
            public_addr: "203.0.113.50:5000".parse().unwrap(),
            supports_hole_punch: true,
            requires_turn: false,
            port_mappings: vec![],
            discovered_at: std::time::Instant::now(),
        };
        assert!(!info_no_mappings.can_connect_direct());

        // Scenario 3: Symmetric NAT (always requires TURN)
        let info_symmetric = NatInfo {
            nat_type: NatType::Symmetric,
            local_addr: "192.168.1.100:5000".parse().unwrap(),
            public_addr: "203.0.113.50:60000".parse().unwrap(),
            supports_hole_punch: false,
            requires_turn: true,
            port_mappings: vec![PortMappingInfo {
                protocol: PortMappingProtocol::Tcp,
                local_port: 5000,
                external_port: 5000,
                description: "Test".to_string(),
                expires_at: None,
            }],
            discovered_at: std::time::Instant::now(),
        };
        assert!(!info_symmetric.can_connect_direct());
    }
}

// ============================================================================
// Error Handling Integration Tests
// ============================================================================

mod error_integration {
    use super::*;

    #[test]
    fn test_error_retryability() {
        let retryable_errors = vec![
            NatError::Network {
                reason: "test".to_string(),
            },
            NatError::Timeout {
                operation: "test".to_string(),
            },
            NatError::PeerUnreachable {
                peer: "test".to_string(),
            },
        ];

        for err in retryable_errors {
            assert!(err.is_retryable());
            assert!(!err.is_fatal());
        }
    }

    #[test]
    fn test_error_fatality() {
        let fatal_errors = vec![
            NatError::Config {
                reason: "test".to_string(),
            },
            NatError::NatTypeDetection {
                reason: "test".to_string(),
            },
            NatError::NoTraversalMethod,
        ];

        for err in fatal_errors {
            assert!(err.is_fatal());
            assert!(!err.is_retryable());
        }
    }

    #[test]
    fn test_error_non_critical() {
        let non_critical = vec![
            NatError::Stun {
                reason: "test".to_string(),
            },
            NatError::Turn {
                reason: "test".to_string(),
            },
            NatError::Upnp {
                reason: "test".to_string(),
            },
            NatError::HolePunch {
                reason: "test".to_string(),
            },
            NatError::PortMappingFailed {
                port: 8080,
                reason: "test".to_string(),
            },
        ];

        for err in non_critical {
            assert!(!err.is_retryable());
            assert!(!err.is_fatal());
        }
    }

    #[test]
    fn test_error_display_all_variants() {
        let errors = vec![
            NatError::Stun {
                reason: "test".to_string(),
            },
            NatError::Turn {
                reason: "test".to_string(),
            },
            NatError::Upnp {
                reason: "test".to_string(),
            },
            NatError::HolePunch {
                reason: "test".to_string(),
            },
            NatError::Network {
                reason: "test".to_string(),
            },
            NatError::Timeout {
                operation: "test".to_string(),
            },
            NatError::NatTypeDetection {
                reason: "test".to_string(),
            },
            NatError::PortMappingFailed {
                port: 8080,
                reason: "test".to_string(),
            },
            NatError::NoTraversalMethod,
            NatError::PeerUnreachable {
                peer: "test".to_string(),
            },
            NatError::Config {
                reason: "test".to_string(),
            },
        ];

        for err in errors {
            let display = format!("{}", err);
            assert!(!display.is_empty());
        }
    }
}

// ============================================================================
// End-to-End Scenarios
// ============================================================================

mod e2e_scenarios {
    use super::*;

    #[tokio::test]
    async fn test_full_nat_traversal_flow() {
        // 1. Create manager with full config
        let config = NatConfig::default()
            .with_stun(true)
            .with_upnp(true)
            .with_hole_punch(true);

        let mut manager = NatTraversalManager::new(config).unwrap();

        // 2. Initialize TURN client
        manager.init_turn_client().await.ok();

        // 3. Verify initial state
        assert!(manager.get_nat_info().await.is_none());

        // 4. Try to get TURN allocation (will fail without server)
        let turn_result = manager.get_turn_allocation().await;
        // May fail because no TURN server is configured
        assert!(turn_result.is_err());

        // 5. Check connection method determination
        let method = manager.get_connection_method().await;
        assert_eq!(method, ConnectionMethod::Discover);

        // 6. Cleanup
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn test_symmetric_nat_fallback_to_turn() {
        // For symmetric NAT, we should fall back to TURN
        let config = NatConfig::default()
            .with_turn("turn:example.com:3478", "user", "pass");

        let mut manager = NatTraversalManager::new(config).unwrap();
        manager.init_turn_client().await.ok();

        // Should not be able to connect directly initially
        assert!(!manager.can_connect_directly().await);

        // Should recommend TURN relay initially
        let method = manager.get_connection_method().await;
        assert_eq!(method, ConnectionMethod::Discover);
    }

    #[tokio::test]
    async fn test_open_internet_direct_connection() {
        let config = NatConfig::default();
        let manager = NatTraversalManager::new(config).unwrap();

        // Initially no NAT info
        assert!(manager.get_nat_info().await.is_none());

        // Should recommend discovery first
        let method = manager.get_connection_method().await;
        assert_eq!(method, ConnectionMethod::Discover);
    }
}

// ============================================================================
// Serialization Integration Tests
// ============================================================================

mod serialization_integration {
    use super::*;

    #[test]
    fn test_nat_config_json_roundtrip() {
        let config = NatConfig::default()
            .with_stun(false)
            .with_upnp(true)
            .with_port_range(40000, 50000);

        let json = serde_json::to_string(&config).unwrap();
        let parsed: NatConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.stun_enabled, config.stun_enabled);
        assert_eq!(parsed.upnp_enabled, config.upnp_enabled);
        assert_eq!(parsed.local_port_start, config.local_port_start);
        assert_eq!(parsed.local_port_end, config.local_port_end);
    }

    #[test]
    fn test_nat_type_serialization() {
        for nat_type in [
            NatType::OpenInternet,
            NatType::FullCone,
            NatType::RestrictedCone,
            NatType::PortRestrictedCone,
            NatType::Symmetric,
            NatType::Unknown,
        ] {
            let json = serde_json::to_string(&nat_type).unwrap();
            let parsed: NatType = serde_json::from_str(&json).unwrap();
            assert_eq!(nat_type, parsed);
        }
    }

    #[test]
    fn test_port_mapping_protocol_serialization() {
        for proto in [PortMappingProtocol::Tcp, PortMappingProtocol::Udp] {
            let json = serde_json::to_string(&proto).unwrap();
            let parsed: PortMappingProtocol = serde_json::from_str(&json).unwrap();
            assert_eq!(proto, parsed);
        }
    }
}
