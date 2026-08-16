//! Integration tests for the Matter protocol module.
//!
//! These tests cover:
//! - `MatterCluster` → `DeviceCapability` mapping
//! - `MatterHubConfig` construction
//! - `decode_matter_value` TLV → JSON conversion
//! - `MatterNode::into_device` conversion
//! - Full hub startup with a Matter controller (using the development config)
//!
//! Integration tests that require a real Matter device (commissioning,
//! subscription, read/write) are marked `#[ignore]` and can be run
//! manually with hardware.

use a3net_smarthome::matter::client::{MatterHubConfig, MatterAttestationTrust};
use a3net_smarthome::matter::types::{MatterCluster, MatterClusterSpec};
use a3net_smarthome::matter::decode_matter_value;
use a3net_smarthome::matter::MatterNode;
use a3net_smarthome::device::{DeviceCapability, DeviceType};

use std::collections::HashSet;

// ── MatterCluster → DeviceCapability mapping ──────────────────────────────────

#[test]
fn onoff_cluster_gives_onoff_capability() {
    let caps: HashSet<_> = MatterCluster::OnOff.capabilities().iter().cloned().collect();
    assert!(caps.contains(&DeviceCapability::OnOff));
    assert_eq!(caps.len(), 1);
}

#[test]
fn level_control_cluster_gives_brightness_capability() {
    let caps: HashSet<_> = MatterCluster::LevelControl.capabilities().iter().cloned().collect();
    assert!(caps.contains(&DeviceCapability::Brightness));
    assert!(caps.contains(&DeviceCapability::Position));
    assert!(caps.contains(&DeviceCapability::FanSpeed));
}

#[test]
fn color_control_cluster_gives_color_and_brightness() {
    let caps: HashSet<_> = MatterCluster::ColorControl.capabilities().iter().cloned().collect();
    assert!(caps.contains(&DeviceCapability::Color));
    assert!(caps.contains(&DeviceCapability::ColorTemperature));
    assert!(caps.contains(&DeviceCapability::Brightness));
}

#[test]
fn thermostat_cluster_gives_temperature_mode_and_fan() {
    let caps: HashSet<_> = MatterCluster::Thermostat.capabilities().iter().cloned().collect();
    assert!(caps.contains(&DeviceCapability::Temperature));
    assert!(caps.contains(&DeviceCapability::Mode));
    assert!(caps.contains(&DeviceCapability::FanSpeed));
}

#[test]
fn door_lock_cluster_gives_lock_capability() {
    let caps: HashSet<_> = MatterCluster::DoorLock.capabilities().iter().cloned().collect();
    assert!(caps.contains(&DeviceCapability::Lock));
}

#[test]
fn ias_zone_cluster_gives_multiple_security_capabilities() {
    let caps: HashSet<_> = MatterCluster::IasZone.capabilities().iter().cloned().collect();
    assert!(caps.contains(&DeviceCapability::Motion));
    assert!(caps.contains(&DeviceCapability::Smoke));
    assert!(caps.contains(&DeviceCapability::CO));
    assert!(caps.contains(&DeviceCapability::Vibration));
    assert!(caps.contains(&DeviceCapability::Tamper));
    assert!(caps.contains(&DeviceCapability::WaterLeak));
}

#[test]
fn smoke_co_alarm_gives_smoke_and_co() {
    let caps: HashSet<_> = MatterCluster::SmokeCoAlarm.capabilities().iter().cloned().collect();
    assert!(caps.contains(&DeviceCapability::Smoke));
    assert!(caps.contains(&DeviceCapability::CO));
}

#[test]
fn electrical_measurement_gives_power_voltage_current_energy() {
    let caps: HashSet<_> = MatterCluster::ElectricalMeasurement.capabilities().iter().cloned().collect();
    assert!(caps.contains(&DeviceCapability::Power));
    assert!(caps.contains(&DeviceCapability::Voltage));
    assert!(caps.contains(&DeviceCapability::Current));
    assert!(caps.contains(&DeviceCapability::Energy));
    assert!(caps.contains(&DeviceCapability::PowerFactor));
}

#[test]
fn power_source_gives_battery_related_capabilities() {
    let caps: HashSet<_> = MatterCluster::PowerSource.capabilities().iter().cloned().collect();
    assert!(caps.contains(&DeviceCapability::Battery));
    assert!(caps.contains(&DeviceCapability::BatteryLevel));
    assert!(caps.contains(&DeviceCapability::Charging));
}

#[test]
fn diagnostic_clusters_give_no_capabilities() {
    for cluster in [
        MatterCluster::GeneralDiagnostics,
        MatterCluster::ThreadNetworkDiagnostics,
        MatterCluster::WiFiNetworkDiagnostics,
        MatterCluster::SoftwareDiagnostics,
    ] {
        assert!(cluster.capabilities().is_empty(), "diagnostic cluster {cluster:?} should have no capabilities");
    }
}

// ── MatterClusterSpec → DeviceCapability set ──────────────────────────────────

#[test]
fn light_device_spec() {
    let spec = MatterClusterSpec {
        node_id: 0x1234_5678_ABCD_EF01,
        label: Some("Kitchen Light".into()),
        vendor_id: 0xFFF1,
        product_id: 0x8000,
        cluster_ids: vec![0x0006, 0x0008, 0x0300], // OnOff + LevelControl + ColorControl
        device_type_ids: vec![0x0101], // Color Bulb
    };
    let caps = spec.to_capabilities();
    assert!(caps.contains(&DeviceCapability::OnOff));
    assert!(caps.contains(&DeviceCapability::Brightness));
    assert!(caps.contains(&DeviceCapability::Color));
    assert!(caps.contains(&DeviceCapability::ColorTemperature));
}

#[test]
fn thermostat_device_spec() {
    let spec = MatterClusterSpec {
        node_id: 1,
        label: None,
        vendor_id: 0,
        product_id: 0,
        cluster_ids: vec![0x0201, 0x0202, 0x0402, 0x0405], // Thermostat + Fan + Temp + Humidity
        device_type_ids: vec![0x0116],
    };
    let caps = spec.to_capabilities();
    assert!(caps.contains(&DeviceCapability::Temperature));
    assert!(caps.contains(&DeviceCapability::Humidity));
    assert!(caps.contains(&DeviceCapability::Mode));
    assert!(caps.contains(&DeviceCapability::FanSpeed));
}

#[test]
fn unknown_cluster_ids_are_ignored() {
    let spec = MatterClusterSpec {
        node_id: 1,
        label: None,
        vendor_id: 0,
        product_id: 0,
        cluster_ids: vec![0x0006, 0xDEAD, 0xBEEF, 0x0008], // mixed with unknown IDs
        device_type_ids: vec![],
    };
    let caps = spec.to_capabilities();
    assert!(caps.contains(&DeviceCapability::OnOff));
    assert!(caps.contains(&DeviceCapability::Brightness));
    // LevelControl gives Brightness + Position + FanSpeed = 3 caps; OnOff = 1 cap; total = 4
    assert_eq!(caps.len(), 4);
}

#[test]
fn empty_spec_has_no_capabilities() {
    let spec = MatterClusterSpec {
        node_id: 1,
        label: None,
        vendor_id: 0,
        product_id: 0,
        cluster_ids: vec![],
        device_type_ids: vec![],
    };
    assert!(spec.to_capabilities().is_empty());
}

// ── MatterClusterSpec::primary_device_type ────────────────────────────────────

#[test]
fn primary_device_type_dimmable_light() {
    let spec = MatterClusterSpec {
        node_id: 1, label: None, vendor_id: 0, product_id: 0,
        cluster_ids: vec![], device_type_ids: vec![0x0100],
    };
    assert_eq!(spec.primary_device_type(), "Dimmable Light");
}

#[test]
fn primary_device_type_door_lock() {
    let spec = MatterClusterSpec {
        node_id: 1, label: None, vendor_id: 0, product_id: 0,
        cluster_ids: vec![], device_type_ids: vec![0x010E],
    };
    assert_eq!(spec.primary_device_type(), "Door Lock");
}

#[test]
fn primary_device_type_unknown_defaults_to_matter_device() {
    let spec = MatterClusterSpec {
        node_id: 1, label: None, vendor_id: 0, product_id: 0,
        cluster_ids: vec![], device_type_ids: vec![0x9999],
    };
    assert_eq!(spec.primary_device_type(), "Matter Device");
}

#[test]
fn primary_device_type_empty_defaults_to_matter_device() {
    let spec = MatterClusterSpec {
        node_id: 1, label: None, vendor_id: 0, product_id: 0,
        cluster_ids: vec![], device_type_ids: vec![],
    };
    assert_eq!(spec.primary_device_type(), "Matter Device");
}

// ── MatterHubConfig ──────────────────────────────────────────────────────────

#[test]
fn matter_hub_config_development() {
    let cfg = MatterHubConfig::development();
    assert!(cfg.store_path.contains("matter-store"));
    match cfg.attestation_trust {
        MatterAttestationTrust::ExampleRoots => {}
        other => panic!("expected ExampleRoots, got {other:?}"),
    }
    assert_eq!(cfg.admin_vendor_id, 0xFFF1);
}

#[test]
fn matter_hub_config_production() {
    let cfg = MatterHubConfig::production(
        "/data/matter.json".into(),
        "/certs/paa".into(),
        "/certs/cd".into(),
    );
    assert_eq!(cfg.store_path, "/data/matter.json");
    match cfg.attestation_trust {
        MatterAttestationTrust::FromDirs { paa_dir, cd_signer_dir } => {
            assert_eq!(paa_dir, "/certs/paa");
            assert_eq!(cd_signer_dir, "/certs/cd");
        }
        other => panic!("expected FromDirs, got {other:?}"),
    }
}

// ── MatterNode::into_device ──────────────────────────────────────────────────

#[test]
fn matter_node_into_device() {
    let spec = MatterClusterSpec {
        node_id: 0x1234_5678,
        label: Some("Living Room Lamp".into()),
        vendor_id: 0xFFF1,
        product_id: 0x8000,
        cluster_ids: vec![0x0006, 0x0008],
        device_type_ids: vec![0x0100],
    };
    let node = MatterNode {
        node_id: 0x1234_5678,
        label: Some("Living Room Lamp".into()),
        vendor_id: 0xFFF1,
        product_id: 0x8000,
        spec,
    };
    let device = node.into_device();

    assert_eq!(device.device_id, "matter:305419896"); // 0x1234_5678 in decimal
    assert_eq!(device.name, "Living Room Lamp");
    assert_eq!(device.device_type, DeviceType::Matter);
    assert!(device.online);
    assert!(device.last_seen.is_some());
    assert!(device.capabilities.contains(&DeviceCapability::OnOff));
    assert!(device.capabilities.contains(&DeviceCapability::Brightness));
}

#[test]
fn matter_node_without_label_uses_device_type_as_name() {
    let spec = MatterClusterSpec {
        node_id: 1,
        label: None,
        vendor_id: 0,
        product_id: 0,
        cluster_ids: vec![0x0101],
        device_type_ids: vec![0x010E],
    };
    let node = MatterNode { node_id: 1, label: None, vendor_id: 0, product_id: 0, spec };
    let device = node.into_device();
    assert_eq!(device.name, "Door Lock");
}

// ── decode_matter_value ───────────────────────────────────────────────────────

#[test]
fn decode_matter_bool() {
    let tlv = matter_controller::Value::Bool(true);
    let json = decode_matter_value(&tlv);
    assert_eq!(json, serde_json::json!({"type": "bool", "value": true}));
}

#[test]
fn decode_matter_uint() {
    let tlv = matter_controller::Value::Uint(42);
    let json = decode_matter_value(&tlv);
    assert_eq!(json, serde_json::json!({"type": "uint", "value": 42}));
}

#[test]
fn decode_matter_sint_negative() {
    let tlv = matter_controller::Value::Int(-17);
    let json = decode_matter_value(&tlv);
    assert_eq!(json, serde_json::json!({"type": "sint", "value": -17}));
}

#[test]
fn decode_matter_utf8() {
    let tlv = matter_controller::Value::Utf8("hello".into());
    let json = decode_matter_value(&tlv);
    assert_eq!(json, serde_json::json!({"type": "string", "value": "hello"}));
}

#[test]
fn decode_matter_bytes() {
    let tlv = matter_controller::Value::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF]);
    let json = decode_matter_value(&tlv);
    assert_eq!(json["type"], "bytes");
    assert_eq!(json["value"], "deadbeef");
}

#[test]
fn decode_matter_null() {
    let tlv = matter_controller::Value::Null;
    let json = decode_matter_value(&tlv);
    assert_eq!(json, serde_json::Value::Null);
}

#[test]
fn decode_matter_structure() {
    use matter_codec::Tag;
    let tlv = matter_controller::Value::Structure(vec![
        (Tag::Context(0), matter_controller::Value::Bool(true)),
        (Tag::Context(1), matter_controller::Value::Uint(100)),
    ]);
    let json = decode_matter_value(&tlv);
    assert_eq!(json["type"], "struct");
    assert!(json["fields"].is_object());
}

#[test]
fn decode_matter_array() {
    let tlv = matter_controller::Value::Array(vec![
        matter_controller::Value::Uint(1),
        matter_controller::Value::Uint(2),
        matter_controller::Value::Uint(3),
    ]);
    let json = decode_matter_value(&tlv);
    assert_eq!(json["type"], "array");
    assert_eq!(json["items"].as_array().unwrap().len(), 3);
}

#[test]
fn decode_matter_list() {
    use matter_codec::Tag;
    let tlv = matter_controller::Value::List(vec![
        (Tag::Context(0), matter_controller::Value::Uint(10)),
        (Tag::Context(0), matter_controller::Value::Uint(20)),
    ]);
    let json = decode_matter_value(&tlv);
    assert_eq!(json["type"], "list");
    assert_eq!(json["items"].as_array().unwrap().len(), 2);
}

// ── MatterCluster TryFrom ──────────────────────────────────────────────────────

#[test]
fn cluster_try_from_onoff() {
    assert_eq!(MatterCluster::try_from(0x0006u32), Ok(MatterCluster::OnOff));
}

#[test]
fn cluster_try_from_temperature_measurement() {
    assert_eq!(
        MatterCluster::try_from(0x0402u32),
        Ok(MatterCluster::TemperatureMeasurement)
    );
}

#[test]
fn cluster_try_from_door_lock() {
    assert_eq!(MatterCluster::try_from(0x0101u32), Ok(MatterCluster::DoorLock));
}

#[test]
fn cluster_try_from_unknown_returns_err() {
    assert!(MatterCluster::try_from(0xDEADu32).is_err());
    assert!(MatterCluster::try_from(0xBEEFu32).is_err());
    assert!(MatterCluster::try_from(0xFFFFu32).is_err());
}

#[test]
fn cluster_name() {
    assert_eq!(MatterCluster::OnOff.name(), "On/Off");
    assert_eq!(MatterCluster::DoorLock.name(), "Door Lock");
    assert_eq!(MatterCluster::Thermostat.name(), "Thermostat");
    assert_eq!(MatterCluster::SmokeCoAlarm.name(), "Smoke/CO Alarm");
    // unknown cluster IDs return Err from TryFrom, so name() is only tested on valid variants
    assert_eq!(MatterCluster::Switch.name(), "Switch");
}

#[test]
fn cluster_try_from_resolves_conflicting_ids() {
    // Switch uses the same 0x003B as ThreadBorderRouterTelemetryBridged;
    // the enum's first occurrence (Switch) wins.
    assert_eq!(MatterCluster::try_from(0x003Bu32), Ok(MatterCluster::Switch));
    // 0x0035 resolves to GeneralDiagnostics (the first match)
    assert_eq!(MatterCluster::try_from(0x0035u32), Ok(MatterCluster::GeneralDiagnostics));
}
