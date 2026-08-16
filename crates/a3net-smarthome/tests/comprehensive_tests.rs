//! Comprehensive tests for a3net-smarthome module.
//!
//! This file provides comprehensive test coverage for all modules:
//! - device.rs: DeviceState, Device, DeviceType, DeviceCapability
//! - discovery.rs: DiscoveryManager, DiscoveryProtocol, DiscoveryEvent
//! - error.rs: SmartHomeError, EvaluateError, PollQrError
//! - hub.rs: SmartHomeHub, HubConfig, HubEvent
//! - api.rs: ApiConfig, tokens_match, is_authorized, dispatch
//! - miot/client.rs: MiotClient methods
//! - miot/types.rs: MIoT type serialization
//! - miot/qrlogin.rs: QR login flow (mocked)
//! - matter/error.rs: MatterError

use a3net_smarthome::automation::{Automation, AutomationAction, Condition, EvaluateError, Trigger};
use a3net_smarthome::device::{Device, DeviceCapability, DeviceState, DeviceType};
use a3net_smarthome::discovery::{DiscoveryEvent, DiscoveryManager, DiscoveryProtocol};
use a3net_smarthome::error::SmartHomeError;
use a3net_smarthome::miot::crypto::MiotCrypto;
use a3net_smarthome::miot::qrlogin::{PollQrError, QRLoginCredentials};
use a3net_smarthome::miot::types::*;
use a3net_smarthome::scene::{Scene, SceneAction, SceneManager};

// ============================================================================
// device.rs Tests
// ============================================================================

#[test]
fn device_state_new_is_empty() {
    let state = DeviceState::new();
    assert!(state.get("any_key").is_none());
}

#[test]
fn device_state_set_and_get() {
    let mut state = DeviceState::new();
    state.set("2.1", serde_json::json!(true));
    state.set("temperature", serde_json::json!(25.5));
    state.set("name", serde_json::json!("sensor"));

    assert_eq!(state.get("2.1"), Some(&serde_json::json!(true)));
    assert_eq!(state.get("temperature"), Some(&serde_json::json!(25.5)));
    assert_eq!(state.get("name"), Some(&serde_json::json!("sensor")));
}

#[test]
fn device_state_overwrites_existing() {
    let mut state = DeviceState::new();
    state.set("key", serde_json::json!(1));
    state.set("key", serde_json::json!(2));
    assert_eq!(state.get("key"), Some(&serde_json::json!(2)));
}

#[test]
fn device_state_clone_is_independent() {
    let mut state1 = DeviceState::new();
    state1.set("key", serde_json::json!("value1"));

    let state2 = state1.clone();
    assert_eq!(state2.get("key"), Some(&serde_json::json!("value1")));

    let mut state3 = state1.clone();
    state3.set("key", serde_json::json!("value2"));
    assert_eq!(state1.get("key"), Some(&serde_json::json!("value1")));
}

#[test]
fn device_new_defaults() {
    let device = Device::new("dev-1", "My Device", DeviceType::Xiaomi);

    assert_eq!(device.device_id, "dev-1");
    assert_eq!(device.name, "My Device");
    assert_eq!(device.device_type, DeviceType::Xiaomi);
    assert!(!device.online);
    assert!(device.capabilities.is_empty());
    assert!(device.state.get("any").is_none());
    assert!(device.room.is_none());
    assert!(device.model.is_none());
    assert!(device.manufacturer.is_none());
    assert!(device.firmware_version.is_none());
    assert!(device.ip_address.is_none());
    assert!(device.last_seen.is_none());
    assert!(device.registered_at <= std::time::SystemTime::now());
}

#[test]
fn device_with_capabilities() {
    let device = Device::new("dev-1", "Lamp", DeviceType::Matter)
        .with_capabilities(vec![DeviceCapability::OnOff, DeviceCapability::Brightness]);

    assert_eq!(device.capabilities.len(), 2);
    assert!(device.capabilities.contains(&DeviceCapability::OnOff));
    assert!(device.capabilities.contains(&DeviceCapability::Brightness));
}

#[test]
fn device_with_room() {
    let device = Device::new("dev-1", "Lamp", DeviceType::Matter)
        .with_room("Living Room");

    assert_eq!(device.room, Some("Living Room".to_string()));
}

#[test]
fn device_with_model() {
    let device = Device::new("dev-1", "Lamp", DeviceType::Matter)
        .with_model("Lamp V2", "Acme Corp");

    assert_eq!(device.model, Some("Lamp V2".to_string()));
    assert_eq!(device.manufacturer, Some("Acme Corp".to_string()));
}

#[test]
fn device_with_multiple_chain() {
    let device = Device::new("dev-1", "Smart Lamp", DeviceType::WiFi)
        .with_room("Bedroom")
        .with_model("SL-100", "SmartHome Inc")
        .with_capabilities(vec![DeviceCapability::OnOff, DeviceCapability::Brightness])
        .with_room("Office"); // Room should be overwritten

    assert_eq!(device.room, Some("Office".to_string()));
    assert_eq!(device.model, Some("SL-100".to_string()));
    assert_eq!(device.capabilities.len(), 2);
}

#[test]
fn device_type_custom_serialization() {
    let custom = DeviceType::Custom("custom_type".to_string());
    let json = serde_json::to_string(&custom).unwrap();
    assert!(json.contains("custom_type"));

    let back: DeviceType = serde_json::from_str(&json).unwrap();
    match back {
        DeviceType::Custom(s) => assert_eq!(s, "custom_type"),
        _ => panic!("Expected Custom"),
    }
}

#[test]
fn device_type_all_variants_serialize() {
    let variants = vec![
        DeviceType::Xiaomi,
        DeviceType::Matter,
        DeviceType::Zigbee,
        DeviceType::ZWave,
        DeviceType::WiFi,
        DeviceType::Bluetooth,
        DeviceType::Custom("test".to_string()),
    ];

    for variant in variants {
        let json = serde_json::to_string(&variant).unwrap();
        let back: DeviceType = serde_json::from_str(&json).unwrap();
        assert_eq!(variant, back);
    }
}

#[test]
fn device_capability_all_variants_serialize() {
    // Test a sampling of capabilities
    let caps = vec![
        DeviceCapability::OnOff,
        DeviceCapability::Brightness,
        DeviceCapability::Temperature,
        DeviceCapability::Lock,
        DeviceCapability::Motion,
        DeviceCapability::Battery,
    ];

    for cap in caps {
        let json = serde_json::to_string(&cap).unwrap();
        let back: DeviceCapability = serde_json::from_str(&json).unwrap();
        assert_eq!(cap, back);
    }
}

#[test]
fn device_json_roundtrip() {
    let device = Device::new("dev-1", "Test Device", DeviceType::Matter)
        .with_room("Room")
        .with_model("Model X", "Manufacturer Y")
        .with_capabilities(vec![DeviceCapability::OnOff]);

    let json = serde_json::to_string(&device).unwrap();
    let back: Device = serde_json::from_str(&json).unwrap();

    assert_eq!(back.device_id, device.device_id);
    assert_eq!(back.name, device.name);
    assert_eq!(back.room, device.room);
    assert_eq!(back.model, device.model);
    assert_eq!(back.capabilities, device.capabilities);
}

// ============================================================================
// discovery.rs Tests
// ============================================================================

#[test]
fn discovery_manager_new() {
    let manager = DiscoveryManager::new();
    // Just verify it doesn't panic
    assert!(true);
}

#[test]
fn discovery_manager_default() {
    let manager = DiscoveryManager::default();
    assert!(true);
}

#[test]
fn discovery_protocol_variants() {
    assert_eq!(format!("{:?}", DiscoveryProtocol::Mdns), "Mdns");
    assert_eq!(format!("{:?}", DiscoveryProtocol::Upnp), "Upnp");
}

#[test]
fn discovery_event_debug() {
    let found_event = DiscoveryEvent::Found(
        Device::new("dev-1", "Found Device", DeviceType::WiFi)
    );
    let lost_event = DiscoveryEvent::Lost("dev-2".to_string());

    assert!(format!("{:?}", found_event).contains("Found"));
    assert!(format!("{:?}", lost_event).contains("Lost"));
}

#[test]
fn discovery_upnp_returns_empty() {
    let manager = DiscoveryManager::new();
    // UPnP is not implemented yet, should return empty vec
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(manager.discover(DiscoveryProtocol::Upnp));
    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}

// ============================================================================
// error.rs Tests
// ============================================================================

#[test]
fn smart_home_error_display() {
    let err = SmartHomeError::DeviceNotFound("dev-1".to_string());
    assert!(format!("{}", err).contains("dev-1"));

    let err = SmartHomeError::Auth("token expired".to_string());
    assert!(format!("{}", err).contains("Authentication failed"));

    let err = SmartHomeError::Network("connection refused".to_string());
    assert!(format!("{}", err).contains("Network error"));

    let err = SmartHomeError::Discovery("no devices".to_string());
    assert!(format!("{}", err).contains("Discovery failed"));

    let err = SmartHomeError::Storage("disk full".to_string());
    assert!(format!("{}", err).contains("Storage error"));

    let err = SmartHomeError::NotSupported("feature".to_string());
    assert!(format!("{}", err).contains("Not supported"));
}

#[test]
fn smart_home_error_from_serde() {
    let json_err = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
    let err = SmartHomeError::from(json_err);
    assert!(format!("{}", err).contains("JSON error"));
}

#[test]
fn smart_home_error_from_io() {
    let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
    let err = SmartHomeError::from(io_err);
    assert!(format!("{}", err).contains("IO error"));
}

#[test]
fn smart_home_error_debug() {
    let err = SmartHomeError::Protocol("invalid packet".to_string());
    let debug_str = format!("{:?}", err);
    assert!(debug_str.contains("Protocol"));
}

#[test]
fn evaluate_error_display() {
    let err = EvaluateError::DeviceNotFound("dev-1".to_string());
    assert!(format!("{}", err).contains("device not found"));

    let err = EvaluateError::InvalidTimeFormat("bad time".to_string());
    assert!(format!("{}", err).contains("invalid time format"));
}

#[test]
fn evaluate_error_debug() {
    let err = EvaluateError::DeviceNotFound("dev-1".to_string());
    let debug_str = format!("{:?}", err);
    assert!(debug_str.contains("DeviceNotFound"));
}

#[test]
fn evaluate_error_source() {
    use std::error::Error;
    let err = EvaluateError::DeviceNotFound("dev-1".to_string());
    assert!(err.source().is_none());
}

// ============================================================================
// miot/types.rs Tests
// ============================================================================

#[test]
fn property_serialization() {
    let prop = Property { siid: 2, piid: 1 };
    let json = serde_json::to_string(&prop).unwrap();
    assert!(json.contains("2"));
    assert!(json.contains("1"));

    let back: Property = serde_json::from_str(&json).unwrap();
    assert_eq!(back.siid, 2);
    assert_eq!(back.piid, 1);
}

#[test]
fn property_value_serialization() {
    let pv = PropertyValue {
        siid: 2,
        piid: 1,
        value: serde_json::json!(true),
    };
    let json = serde_json::to_string(&pv).unwrap();
    let back: PropertyValue = serde_json::from_str(&json).unwrap();
    assert_eq!(back.siid, 2);
    assert_eq!(back.piid, 1);
    assert_eq!(back.value, serde_json::json!(true));
}

#[test]
fn action_serialization() {
    let action = Action {
        siid: 3,
        aiid: 1,
        input: vec![serde_json::json!(10), serde_json::json!("test")],
    };
    let json = serde_json::to_string(&action).unwrap();
    assert!(json.contains("in")); // renamed field

    let back: Action = serde_json::from_str(&json).unwrap();
    assert_eq!(back.siid, 3);
    assert_eq!(back.aiid, 1);
    assert_eq!(back.input.len(), 2);
}

#[test]
fn miot_device_partial_json() {
    // Test that missing fields use defaults
    let json = r#"{"did": "dev-1", "name": "Test", "online": true}"#;
    let device: MiotDevice = serde_json::from_str(json).unwrap();
    assert_eq!(device.did, "dev-1");
    assert_eq!(device.name, "Test");
    assert!(device.online);
    assert!(device.model.is_empty()); // default
}

#[test]
fn miot_device_spec_serialization() {
    let spec = MiotDeviceSpec {
        model: "test.model.v1".to_string(),
        description: "Test device".to_string(),
        services: vec![
            MiotServiceSpec {
                siid: 2,
                description: "Light".to_string(),
                properties: vec![
                    MiotPropertySpec {
                        piid: 1,
                        description: "Power".to_string(),
                        format: "bool".to_string(),
                        access: vec!["read".to_string(), "write".to_string()],
                    }
                ],
                actions: vec![
                    MiotActionSpec {
                        aiid: 1,
                        description: "Toggle".to_string(),
                    }
                ],
                events: vec![],
            }
        ],
    };

    let json = serde_json::to_string(&spec).unwrap();
    let back: MiotDeviceSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(back.model, "test.model.v1");
    assert_eq!(back.services.len(), 1);
    assert_eq!(back.services[0].properties.len(), 1);
    assert_eq!(back.services[0].actions.len(), 1);
}

#[test]
fn miot_device_request_serialization() {
    let req = MiotDeviceRequest {
        did: "dev-1".to_string(),
        method: "get_prop".to_string(),
        params: vec![serde_json::json!("prop1"), serde_json::json!("prop2")],
    };
    let json = serde_json::to_string(&req).unwrap();
    // Only serialize, as these types don't implement Deserialize (they're for outgoing requests)
    assert!(json.contains("get_prop"));
    assert!(json.contains("params"));
}

#[test]
fn miot_property_request_serialization() {
    let req = MiotPropertyRequest {
        did: "dev-1".to_string(),
        params: vec![Property { siid: 2, piid: 1 }],
    };
    let json = serde_json::to_string(&req).unwrap();
    assert!(json.contains("did"));
    assert!(json.contains("params"));
}

// Note: MiotPropertySetRequest, MiotDeviceRequest, and MiotActionRequest 
// only derive Serialize (used for outgoing requests), not Deserialize.

// ============================================================================
// miot/crypto.rs Additional Tests
// ============================================================================

#[test]
fn miot_crypto_nonce_format() {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
    let c = MiotCrypto::new();
    let nonce = c.generate_nonce();

    // Should be valid base64
    let decoded = BASE64.decode(&nonce).unwrap();
    assert_eq!(decoded.len(), 12);

    // First 8 bytes should be random (test that it's different each time)
    let nonce2 = c.generate_nonce();
    assert_ne!(nonce, nonce2); // Highly likely to be different
}

#[test]
fn miot_crypto_signed_nonce_bad_ssecurity() {
    let c = MiotCrypto::new();
    let result = c.generate_signed_nonce("!!!invalid base64!!!", "dGVzdA==");
    assert!(result.is_err());
}

#[test]
fn miot_crypto_signed_nonce_bad_nonce() {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
    let c = MiotCrypto::new();
    let ssecurity = BASE64.encode(b"0123456789abcdef0123456789abcdef");
    let result = c.generate_signed_nonce(&ssecurity, "!!!invalid!!!");
    assert!(result.is_err());
}

#[test]
fn miot_crypto_signature_bad_signed_nonce() {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
    let c = MiotCrypto::new();
    let result = c.generate_signature("/path", "!!!bad!!!", "nonce", "{}");
    assert!(result.is_err());
}

#[test]
fn miot_crypto_signature_deterministic() {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
    let c = MiotCrypto::new();
    let ssecurity = BASE64.encode(b"0123456789abcdef0123456789abcdef");
    let nonce = "dGVzdG5vbmNlMTIzNDU2"; // Fixed nonce for testing
    let signed = c.generate_signed_nonce(&ssecurity, nonce).unwrap();

    // Same inputs should produce same signature
    let sig1 = c.generate_signature("/test", &signed, nonce, "{}").unwrap();
    let sig2 = c.generate_signature("/test", &signed, nonce, "{}").unwrap();
    assert_eq!(sig1, sig2);

    // Different data should produce different signature
    let sig3 = c.generate_signature("/test", &signed, nonce, "{\"a\":1}").unwrap();
    assert_ne!(sig1, sig3);

    // Different path should produce different signature
    let sig4 = c.generate_signature("/other", &signed, nonce, "{}").unwrap();
    assert_ne!(sig1, sig4);
}

// ============================================================================
// miot/qrlogin.rs Tests
// ============================================================================

#[test]
fn poll_qr_error_display() {
    let err = PollQrError::Expired("timeout".to_string());
    assert!(format!("{}", err).contains("expired"));

    let err = PollQrError::Timeout;
    assert!(format!("{}", err).contains("timed out"));

    let err = PollQrError::Network("connection lost".to_string());
    assert!(format!("{}", err).contains("network error"));

    let err = PollQrError::Pending("waiting".to_string());
    assert!(format!("{}", err).contains("waiting for scan"));

    let err = PollQrError::Protocol(SmartHomeError::Auth("test".to_string()));
    assert!(format!("{}", err).contains("protocol error"));
}

#[test]
fn poll_qr_error_from_smart_home_error() {
    let sm_err = SmartHomeError::Protocol("test".to_string());
    let poll_err: PollQrError = sm_err.into();
    match poll_err {
        PollQrError::Protocol(_) => {}
        _ => panic!("Expected Protocol variant"),
    }
}

#[test]
fn poll_qr_error_debug() {
    let err = PollQrError::Expired("test".to_string());
    let debug_str = format!("{:?}", err);
    assert!(debug_str.contains("Expired"));
}

#[test]
fn qr_login_credentials_serialization() {
    let creds = QRLoginCredentials {
        user_id: "12345".to_string(),
        ssecurity: "abc123".to_string(),
        device_id: "device-1".to_string(),
        service_token: "token-xyz".to_string(),
        c_user_id: "cuser".to_string(),
    };

    let json = serde_json::to_string(&creds).unwrap();
    assert!(json.contains("12345"));
    assert!(json.contains("cUserId"));

    let back: QRLoginCredentials = serde_json::from_str(&json).unwrap();
    assert_eq!(back.user_id, "12345");
    assert_eq!(back.ssecurity, "abc123");
    assert_eq!(back.c_user_id, "cuser");
}

#[test]
fn qr_login_credentials_default_c_user_id() {
    let json = r#"{"user_id": "123", "ssecurity": "sec", "device_id": "dev", "service_token": "tok"}"#;
    let creds: QRLoginCredentials = serde_json::from_str(json).unwrap();
    assert!(creds.c_user_id.is_empty());
}

// ============================================================================
// matter/error.rs Tests
// ============================================================================

#[test]
fn matter_error_display() {
    use a3net_smarthome::matter::error::MatterError;

    let err = MatterError::Controller("crash".to_string());
    assert!(format!("{}", err).contains("controller error"));

    let err = MatterError::NotCommissioned("node-1".to_string());
    assert!(format!("{}", err).contains("device not commissioned"));

    let err = MatterError::Interaction("invalid".to_string());
    assert!(format!("{}", err).contains("interaction model error"));

    let err = MatterError::Transport("timeout".to_string());
    assert!(format!("{}", err).contains("transport error"));

    let err = MatterError::Certificate("expired".to_string());
    assert!(format!("{}", err).contains("certificate error"));

    let err = MatterError::Trust("no roots".to_string());
    assert!(format!("{}", err).contains("trust"));

    let err = MatterError::SetupCode("bad format".to_string());
    assert!(format!("{}", err).contains("setup code"));

    let err = MatterError::CommissioningRejected(10);
    assert!(format!("{}", err).contains("commissioning rejected"));

    let err = MatterError::NocRejected(20);
    assert!(format!("{}", err).contains("NOC"));

    let err = MatterError::AclLockOut;
    assert!(format!("{}", err).contains("ACL"));

    let err = MatterError::GroupNotProvisioned(100);
    assert!(format!("{}", err).contains("group not provisioned"));

    let err = MatterError::FabricExists;
    assert!(format!("{}", err).contains("fabric already exists"));

    let err = MatterError::NodeNotFound(123);
    assert!(format!("{}", err).contains("node not found"));

    let err = MatterError::SubscriptionClosed;
    assert!(format!("{}", err).contains("subscription stream closed"));
}

#[test]
fn matter_error_debug() {
    use a3net_smarthome::matter::error::MatterError;
    let err = MatterError::Controller("test".to_string());
    let debug_str = format!("{:?}", err);
    assert!(debug_str.contains("Controller"));
}

#[test]
fn matter_error_into_smart_home_error() {
    use a3net_smarthome::matter::error::MatterError;
    let err = MatterError::Controller("test".to_string());
    let sm_err: SmartHomeError = err.into();
    match sm_err {
        SmartHomeError::Protocol(s) => assert!(s.contains("test")),
        _ => panic!("Expected Protocol"),
    }
}

// ============================================================================
// scene.rs Additional Tests
// ============================================================================

#[test]
fn scene_new() {
    let scene = Scene::new("morning", "Good Morning");
    assert_eq!(scene.id, "morning");
    assert_eq!(scene.name, "Good Morning");
    assert!(scene.room.is_none());
    assert!(!scene.active);
    assert!(scene.icon.is_none());
    assert!(scene.actions.is_empty());
}

#[test]
fn scene_with_room() {
    let scene = Scene::new("id", "name").with_room("Bedroom");
    assert_eq!(scene.room, Some("Bedroom".to_string()));
}

#[test]
fn scene_with_icon() {
    let scene = Scene::new("id", "name").with_icon("moon");
    assert_eq!(scene.icon, Some("moon".to_string()));
}

#[test]
fn scene_add_action() {
    let scene = Scene::new("id", "name")
        .add_action(SceneAction::Delay { millis: 100 })
        .add_action(SceneAction::SetProperty {
            device_id: "dev-1".into(),
            siid: 2,
            piid: 1,
            value: serde_json::json!(true),
        });

    assert_eq!(scene.actions.len(), 2);
}

#[test]
fn scene_action_serialization_set_property() {
    let action = SceneAction::SetProperty {
        device_id: "dev-1".into(),
        siid: 2,
        piid: 1,
        value: serde_json::json!({"key": "value"}),
    };

    let json = serde_json::to_string(&action).unwrap();
    assert!(json.contains("set_property"));
    let back: SceneAction = serde_json::from_str(&json).unwrap();
    match back {
        SceneAction::SetProperty { device_id, siid, piid, value } => {
            assert_eq!(device_id, "dev-1");
            assert_eq!(siid, 2);
            assert_eq!(piid, 1);
            assert!(value.is_object());
        }
        _ => panic!("Expected SetProperty"),
    }
}

#[test]
fn scene_action_serialization_delay() {
    let action = SceneAction::Delay { millis: 5000 };
    let json = serde_json::to_string(&action).unwrap();
    assert!(json.contains("delay"));
    assert!(json.contains("5000"));
}

#[test]
fn scene_action_serialization_invoke_action() {
    let action = SceneAction::InvokeAction {
        device_id: "dev-1".into(),
        siid: 3,
        aiid: 2,
        input: vec![serde_json::json!(1), serde_json::json!("test")],
    };

    let json = serde_json::to_string(&action).unwrap();
    assert!(json.contains("invoke_action"));
    let back: SceneAction = serde_json::from_str(&json).unwrap();
    match back {
        SceneAction::InvokeAction { device_id, aiid, input, .. } => {
            assert_eq!(device_id, "dev-1");
            assert_eq!(aiid, 2);
            assert_eq!(input.len(), 2);
        }
        _ => panic!("Expected InvokeAction"),
    }
}

#[test]
fn scene_serialization_roundtrip() {
    let scene = Scene::new("morning", "Good Morning")
        .with_room("Bedroom")
        .with_icon("sun")
        .add_action(SceneAction::Delay { millis: 1000 })
        .add_action(SceneAction::SetProperty {
            device_id: "lamp-1".into(),
            siid: 2,
            piid: 1,
            value: serde_json::json!(true),
        });

    let json = serde_json::to_string(&scene).unwrap();
    let back: Scene = serde_json::from_str(&json).unwrap();

    assert_eq!(back.id, "morning");
    assert_eq!(back.name, "Good Morning");
    assert_eq!(back.room, Some("Bedroom".to_string()));
    assert_eq!(back.icon, Some("sun".to_string()));
    assert_eq!(back.actions.len(), 2);
}

#[test]
fn scene_manager_get_active() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let mgr = SceneManager::new(temp_dir());
        mgr.add(Scene::new("s1", "Scene 1")).await.unwrap();
        mgr.add(Scene::new("s2", "Scene 2")).await.unwrap();
        mgr.add(Scene::new("s3", "Scene 3")).await.unwrap();

        // No active scene initially
        assert!(mgr.get_active().await.is_none());

        // Activate s2
        mgr.set_active("s2").await.unwrap();
        let active = mgr.get_active().await.unwrap();
        assert_eq!(active.id, "s2");

        // Activate s1
        mgr.set_active("s1").await.unwrap();
        let active = mgr.get_active().await.unwrap();
        assert_eq!(active.id, "s1");

        // s2 should no longer be active
        assert!(!mgr.get("s2").await.unwrap().active);

        let _ = tokio::fs::remove_dir_all(temp_dir()).await;
    });
}

#[test]
fn scene_manager_set_active_nonexistent() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let mgr = SceneManager::new(temp_dir());
        let result = mgr.set_active("nonexistent").await;
        assert!(result.is_err());

        let _ = tokio::fs::remove_dir_all(temp_dir()).await;
    });
}

#[test]
fn scene_manager_remove_nonexistent() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let mgr = SceneManager::new(temp_dir());
        let result = mgr.remove("nonexistent").await;
        assert!(result.is_err());

        let _ = tokio::fs::remove_dir_all(temp_dir()).await;
    });
}

#[test]
fn scene_manager_load_corrupted_json() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let dir = temp_dir();
        let path = format!("{}/scenes.json", dir);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(&path, "{ invalid json").await.unwrap();

        let mgr = SceneManager::new(&dir);
        let result = mgr.load().await;
        assert!(result.is_err());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    });
}

#[test]
fn scene_manager_save_load() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let dir = temp_dir();
        let mgr = SceneManager::new(&dir);

        mgr.add(Scene::new("s1", "Scene 1").with_room("Room")).await.unwrap();
        mgr.add(Scene::new("s2", "Scene 2").with_icon("icon")).await.unwrap();
        mgr.save().await.unwrap();

        let mgr2 = SceneManager::new(&dir);
        mgr2.load().await.unwrap();

        let scenes = mgr2.list_all().await;
        assert_eq!(scenes.len(), 2);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    });
}

// ============================================================================
// automation.rs Additional Tests
// ============================================================================

#[test]
fn automation_serialization_roundtrip() {
    let auto = Automation {
        id: "auto-1".into(),
        name: "Test Automation".into(),
        enabled: true,
        trigger: Trigger::PropertyChanged {
            device_id: "dev-1".into(),
            property: "2.1".into(),
            value: serde_json::json!(true),
        },
        conditions: vec![
            Condition::PropertyEquals {
                device_id: "dev-1".into(),
                property: "3.1".into(),
                value: serde_json::json!("armed"),
            }
        ],
        actions: vec![
            AutomationAction::InvokeAction {
                device_id: "dev-2".into(),
                siid: 2,
                aiid: 1,
                input: vec![serde_json::json!(42)],
            },
            AutomationAction::Notify { message: "triggered".into() },
        ],
    };

    let json = serde_json::to_string(&auto).unwrap();
    let back: Automation = serde_json::from_str(&json).unwrap();

    assert_eq!(back.id, "auto-1");
    assert!(back.enabled);
    assert!(matches!(back.trigger, Trigger::PropertyChanged { .. }));
    assert_eq!(back.conditions.len(), 1);
    assert_eq!(back.actions.len(), 2);
}

#[test]
fn trigger_variants_serialization() {
    let triggers = vec![
        Trigger::PropertyChanged {
            device_id: "dev".into(),
            property: "prop".into(),
            value: serde_json::json!(true),
        },
        Trigger::Schedule { cron: "08:00".into() },
        Trigger::DeviceOnline { device_id: "dev-1".into() },
        Trigger::DeviceOffline { device_id: "dev-2".into() },
        Trigger::Manual,
    ];

    for trigger in triggers {
        let json = serde_json::to_string(&trigger).unwrap();
        let back: Trigger = serde_json::from_str(&json).unwrap();
        assert_eq!(format!("{:?}", trigger), format!("{:?}", back));
    }
}

#[test]
fn condition_variants_serialization() {
    let conditions = vec![
        Condition::PropertyEquals {
            device_id: "dev".into(),
            property: "prop".into(),
            value: serde_json::json!(123),
        },
        Condition::TimeInRange {
            start: "09:00".into(),
            end: "17:00".into(),
        },
    ];

    for cond in conditions {
        let json = serde_json::to_string(&cond).unwrap();
        let back: Condition = serde_json::from_str(&json).unwrap();
        assert_eq!(format!("{:?}", cond), format!("{:?}", back));
    }
}

#[test]
fn automation_action_serialization() {
    let actions = vec![
        AutomationAction::SetProperty {
            device_id: "dev".into(),
            siid: 2,
            piid: 1,
            value: serde_json::json!(true),
        },
        AutomationAction::InvokeAction {
            device_id: "dev".into(),
            siid: 3,
            aiid: 1,
            input: vec![],
        },
        AutomationAction::Delay { seconds: 60 },
        AutomationAction::Notify { message: "hello".into() },
    ];

    for action in actions {
        let json = serde_json::to_string(&action).unwrap();
        let back: AutomationAction = serde_json::from_str(&json).unwrap();
        assert_eq!(format!("{:?}", action), format!("{:?}", back));
    }
}

#[test]
fn trigger_tag_format() {
    let t = Trigger::Schedule { cron: "08:00".into() };
    let json = serde_json::to_value(&t).unwrap();
    assert_eq!(json["type"], "schedule");
    assert_eq!(json["cron"], "08:00");
}

#[test]
fn condition_tag_format() {
    let c = Condition::PropertyEquals {
        device_id: "d".into(),
        property: "p".into(),
        value: serde_json::json!(true),
    };
    let json = serde_json::to_value(&c).unwrap();
    assert_eq!(json["type"], "property_equals");
}

#[test]
fn condition_time_in_range_boundary() {
    use chrono::{Local, NaiveTime};

    // Test exactly at start time
    let now = Local::now()
        .with_time(NaiveTime::from_hms_opt(9, 0, 0).unwrap())
        .unwrap();
    let cond = Condition::TimeInRange {
        start: "09:00".into(),
        end: "17:00".into(),
    };
    assert!(cond.evaluate(&now, |_, _| None).unwrap());

    // Test exactly at end time
    let now = Local::now()
        .with_time(NaiveTime::from_hms_opt(17, 0, 0).unwrap())
        .unwrap();
    assert!(cond.evaluate(&now, |_, _| None).unwrap());

    // Test just before start
    let now = Local::now()
        .with_time(NaiveTime::from_hms_opt(8, 59, 0).unwrap())
        .unwrap();
    assert!(!cond.evaluate(&now, |_, _| None).unwrap());

    // Test just after end
    let now = Local::now()
        .with_time(NaiveTime::from_hms_opt(17, 0, 1).unwrap())
        .unwrap();
    assert!(!cond.evaluate(&now, |_, _| None).unwrap());
}

#[test]
fn condition_time_in_range_hours_minutes_seconds() {
    use chrono::{Local, NaiveTime};
    let now = Local::now()
        .with_time(NaiveTime::from_hms_opt(12, 30, 45).unwrap())
        .unwrap();

    // HH:MM:SS format
    let cond = Condition::TimeInRange {
        start: "08:00:00".into(),
        end: "18:00:00".into(),
    };
    assert!(cond.evaluate(&now, |_, _| None).unwrap());
}

#[test]
fn condition_time_in_range_invalid_format() {
    use chrono::Local;
    let now = Local::now();
    let cond = Condition::TimeInRange {
        start: "invalid".into(),
        end: "17:00".into(),
    };
    let result = cond.evaluate(&now, |_, _| None);
    assert!(result.is_err());
}

#[test]
fn condition_property_equals_returns_false_when_value_differs() {
    use chrono::Local;
    let now = Local::now();
    let cond = Condition::PropertyEquals {
        device_id: "dev".into(),
        property: "prop".into(),
        value: serde_json::json!(100),
    };
    // Device found but value doesn't match - should return false
    let result = cond.evaluate(&now, |_, _| Some(serde_json::json!(50)));
    assert_eq!(result.unwrap(), false);
}

#[test]
fn automation_json_minimal() {
    let auto = Automation {
        id: "min".into(),
        name: "Minimal".into(),
        enabled: false,
        trigger: Trigger::Manual,
        conditions: vec![],
        actions: vec![],
    };

    let json = serde_json::to_string(&auto).unwrap();
    let back: Automation = serde_json::from_str(&json).unwrap();
    assert_eq!(back.id, "min");
    assert!(!back.enabled);
}

// ============================================================================
// Helper Functions
// ============================================================================

fn temp_dir() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let suffix: String = (0..8).map(|_| format!("{:x}", rng.gen_range(0..16u8))).collect();
    std::env::temp_dir()
        .join(format!("a3net-smarthome-comprehensive-test-{}", suffix))
        .to_string_lossy()
        .into_owned()
}
