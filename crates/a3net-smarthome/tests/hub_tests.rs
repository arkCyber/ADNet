//! Hub module tests.
//!
//! Tests for:
//! - HubConfig construction and defaults
//! - HubEvent serialization
//! - SmartHomeHub creation without MIoT
//! - Scene operations through hub
//! - Automation operations through hub

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use a3net_smarthome::automation::{Automation, AutomationAction, Trigger};
use a3net_smarthome::device::{Device, DeviceType};
use a3net_smarthome::hub::{HubConfig, HubEvent, SmartHomeHub};
use a3net_smarthome::scene::{Scene, SceneAction};

// ── HubConfig Tests ──────────────────────────────────────────────────────────

#[test]
fn hub_config_default() {
    let config = HubConfig::default();
    assert_eq!(config.bind_addr, "127.0.0.1:8781".parse::<SocketAddr>().unwrap());
    assert!(config.enable_discovery);
    assert_eq!(config.discovery_interval_secs, 300);
    assert_eq!(config.heartbeat_timeout_secs, 120);
    assert_eq!(config.data_dir, "./data/smarthome");
}

#[test]
fn hub_config_custom() {
    let config = HubConfig {
        bind_addr: "0.0.0.0:9000".parse().unwrap(),
        enable_discovery: false,
        discovery_interval_secs: 600,
        heartbeat_timeout_secs: 300,
        data_dir: "/tmp/hub-data".to_string(),
    };
    
    assert_eq!(config.bind_addr, "0.0.0.0:9000".parse().unwrap());
    assert!(!config.enable_discovery);
    assert_eq!(config.discovery_interval_secs, 600);
    assert_eq!(config.heartbeat_timeout_secs, 300);
    assert_eq!(config.data_dir, "/tmp/hub-data");
}

#[test]
fn hub_config_clone() {
    let config = HubConfig::default();
    let cloned = config.clone();
    assert_eq!(cloned.bind_addr, config.bind_addr);
    assert_eq!(cloned.data_dir, config.data_dir);
}

#[test]
fn hub_config_debug() {
    let config = HubConfig::default();
    let debug_str = format!("{:?}", config);
    assert!(debug_str.contains("HubConfig"));
}

// ── HubEvent Tests ───────────────────────────────────────────────────────────

#[test]
fn hub_event_device_discovered() {
    let device = Device::new("dev-1", "Test Device", DeviceType::Matter);
    let event = HubEvent::DeviceDiscovered { device };
    
    let json = serde_json::to_string(&event).unwrap();
    // Check for key fields without assuming specific naming convention
    assert!(json.contains("dev-1"));
    assert!(json.contains("Test Device"));
}

#[test]
fn hub_event_device_online() {
    let event = HubEvent::DeviceOnline { device_id: "dev-1".to_string() };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("dev-1"));
}

#[test]
fn hub_event_device_offline() {
    let event = HubEvent::DeviceOffline { device_id: "dev-1".to_string() };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("dev-1"));
}

#[test]
fn hub_event_property_changed() {
    let event = HubEvent::PropertyChanged {
        device_id: "dev-1".to_string(),
        property: "2.1".to_string(),
        value: serde_json::json!(true),
    };
    
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("dev-1"));
    assert!(json.contains("2.1"));
}

#[test]
fn hub_event_device_removed() {
    let event = HubEvent::DeviceRemoved { device_id: "dev-1".to_string() };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("dev-1"));
}

#[test]
fn hub_event_roundtrip_serialization() {
    let events = vec![
        HubEvent::DeviceDiscovered { device: Device::new("d1", "D1", DeviceType::Xiaomi) },
        HubEvent::DeviceOnline { device_id: "d1".to_string() },
        HubEvent::DeviceOffline { device_id: "d1".to_string() },
        HubEvent::PropertyChanged { device_id: "d1".into(), property: "p1".into(), value: serde_json::json!(42) },
        HubEvent::DeviceRemoved { device_id: "d1".into() },
    ];
    
    for event in events {
        let json = serde_json::to_string(&event).unwrap();
        // Should not panic
        let _ = serde_json::from_str::<HubEvent>(&json).unwrap();
    }
}

// ── SmartHomeHub Basic Tests ─────────────────────────────────────────────────

#[tokio::test]
async fn hub_new_without_miot() {
    let data_dir = temp_dir();
    let config = HubConfig {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        enable_discovery: false,
        discovery_interval_secs: 3600,
        heartbeat_timeout_secs: 3600,
        data_dir: data_dir.clone(),
    };
    
    let hub = SmartHomeHub::new(config);
    
    // Should have empty device list
    let devices = hub.list_devices().await;
    assert!(devices.is_empty());
    
    // Should have no scenes
    let scenes = hub.list_scenes().await;
    assert!(scenes.is_empty());
    
    // Should have no automations
    let autos = hub.list_automations().await;
    assert!(autos.is_empty());
    
    let _ = tokio::fs::remove_dir_all(&data_dir).await;
}

#[tokio::test]
async fn hub_subscribe() {
    let data_dir = temp_dir();
    let config = HubConfig {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        enable_discovery: false,
        discovery_interval_secs: 3600,
        heartbeat_timeout_secs: 3600,
        data_dir: data_dir.clone(),
    };
    
    let hub = SmartHomeHub::new(config);
    let mut rx = hub.subscribe();
    
    // Send an event
    hub.register_device(Device::new("dev-1", "Test", DeviceType::Matter))
        .await
        .unwrap();
    
    // Should receive the event
    let result = tokio::time::timeout(Duration::from_millis(100), rx.recv()).await;
    assert!(result.is_ok());
    
    let _ = tokio::fs::remove_dir_all(&data_dir).await;
}

// ── Device Operations Through Hub ─────────────────────────────────────────────

#[tokio::test]
async fn hub_register_and_list_devices() {
    let config = hub_config();
    let hub = Arc::new(SmartHomeHub::new(config.clone()));
    
    // Register devices
    hub.register_device(Device::new("dev-1", "Device 1", DeviceType::Xiaomi))
        .await
        .unwrap();
    hub.register_device(Device::new("dev-2", "Device 2", DeviceType::Matter))
        .await
        .unwrap();
    
    let devices = hub.list_devices().await;
    assert_eq!(devices.len(), 2);
    
    let _ = tokio::fs::remove_dir_all(&config.data_dir).await;
}

#[tokio::test]
async fn hub_get_device() {
    let config = hub_config();
    let hub = Arc::new(SmartHomeHub::new(config.clone()));
    
    hub.register_device(Device::new("dev-1", "Device 1", DeviceType::Matter))
        .await
        .unwrap();
    
    let device = hub.get_device("dev-1").await;
    assert!(device.is_some());
    assert_eq!(device.unwrap().name, "Device 1");
    
    let missing = hub.get_device("nonexistent").await;
    assert!(missing.is_none());
    
    let _ = tokio::fs::remove_dir_all(&config.data_dir).await;
}

#[tokio::test]
async fn hub_remove_device() {
    let data_dir = temp_dir();
    let config = HubConfig {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        enable_discovery: false,
        discovery_interval_secs: 3600,
        heartbeat_timeout_secs: 3600,
        data_dir: data_dir.clone(),
    };
    let hub = Arc::new(SmartHomeHub::new(config));
    
    hub.register_device(Device::new("dev-1", "Device 1", DeviceType::Matter))
        .await
        .unwrap();
    
    assert!(hub.get_device("dev-1").await.is_some());
    
    hub.remove_device("dev-1").await.unwrap();
    
    assert!(hub.get_device("dev-1").await.is_none());
    
    // Removing non-existent device should fail
    let result = hub.remove_device("nonexistent").await;
    assert!(result.is_err());
    
    let _ = tokio::fs::remove_dir_all(&data_dir).await;
}

#[tokio::test]
async fn hub_register_device_emits_event() {
    let config = hub_config();
    let hub = Arc::new(SmartHomeHub::new(config.clone()));
    let mut events = hub.subscribe();
    
    hub.register_device(Device::new("dev-1", "Device 1", DeviceType::Matter))
        .await
        .unwrap();
    
    let result = tokio::time::timeout(Duration::from_millis(100), events.recv()).await;
    assert!(result.is_ok());
    
    let _ = tokio::fs::remove_dir_all(&config.data_dir).await;
}

#[tokio::test]
async fn hub_remove_device_emits_event() {
    let config = hub_config();
    let hub = Arc::new(SmartHomeHub::new(config.clone()));
    let mut events = hub.subscribe();
    
    hub.register_device(Device::new("dev-1", "Device 1", DeviceType::Matter))
        .await
        .unwrap();
    
    // Skip the DeviceDiscovered event
    let _ = tokio::time::timeout(Duration::from_millis(100), events.recv()).await;
    
    hub.remove_device("dev-1").await.unwrap();
    
    let result = tokio::time::timeout(Duration::from_millis(100), events.recv()).await;
    assert!(result.is_ok());
    
    let _ = tokio::fs::remove_dir_all(&config.data_dir).await;
}

// ── Scene Operations Through Hub ──────────────────────────────────────────────

#[tokio::test]
async fn hub_add_and_get_scene() {
    let config = hub_config();
    let hub = Arc::new(SmartHomeHub::new(config.clone()));
    
    let scene = Scene::new("morning", "Good Morning")
        .with_room("Bedroom")
        .add_action(SceneAction::Delay { millis: 1000 });
    
    hub.add_scene(scene).await.unwrap();
    
    let found = hub.get_scene("morning").await;
    assert!(found.is_some());
    assert_eq!(found.unwrap().name, "Good Morning");
    
    let _ = tokio::fs::remove_dir_all(&config.data_dir).await;
}

#[tokio::test]
async fn hub_list_scenes() {
    let config = hub_config();
    let hub = Arc::new(SmartHomeHub::new(config.clone()));
    
    hub.add_scene(Scene::new("s1", "Scene 1")).await.unwrap();
    hub.add_scene(Scene::new("s2", "Scene 2")).await.unwrap();
    hub.add_scene(Scene::new("s3", "Scene 3")).await.unwrap();
    
    let scenes = hub.list_scenes().await;
    assert_eq!(scenes.len(), 3);
    
    let _ = tokio::fs::remove_dir_all(&config.data_dir).await;
}

#[tokio::test]
async fn hub_remove_scene() {
    let config = hub_config();
    let hub = Arc::new(SmartHomeHub::new(config.clone()));
    
    hub.add_scene(Scene::new("s1", "Scene 1")).await.unwrap();
    assert!(hub.get_scene("s1").await.is_some());
    
    hub.remove_scene("s1").await.unwrap();
    assert!(hub.get_scene("s1").await.is_none());
    
    // Removing non-existent scene should fail
    let result = hub.remove_scene("nonexistent").await;
    assert!(result.is_err());
    
    let _ = tokio::fs::remove_dir_all(&config.data_dir).await;
}

#[tokio::test]
async fn hub_activate_scene_with_delay() {
    let config = hub_config();
    let hub = Arc::new(SmartHomeHub::new(config.clone()));
    
    // Create scene with just a delay (no MIoT required)
    let scene = Scene::new("test", "Test Scene")
        .add_action(SceneAction::Delay { millis: 10 });
    
    hub.add_scene(scene).await.unwrap();
    
    // Activate should succeed (delay should complete quickly)
    let start = std::time::Instant::now();
    hub.activate_scene("test").await.unwrap();
    let elapsed = start.elapsed();
    
    // Should have waited at least 10ms
    assert!(elapsed >= Duration::from_millis(10));
    
    // Scene should be marked active
    let active = hub.get_scene("test").await.unwrap();
    assert!(active.active);
    
    let _ = tokio::fs::remove_dir_all(&config.data_dir).await;
}

#[tokio::test]
async fn hub_activate_nonexistent_scene() {
    let config = hub_config();
    let hub = Arc::new(SmartHomeHub::new(config.clone()));
    
    let result = hub.activate_scene("nonexistent").await;
    assert!(result.is_err());
    
    let _ = tokio::fs::remove_dir_all(&config.data_dir).await;
}

// ── Automation Operations Through Hub ────────────────────────────────────────

#[tokio::test]
async fn hub_add_and_list_automations() {
    let config = hub_config();
    let hub = Arc::new(SmartHomeHub::new(config.clone()));
    
    let auto = Automation {
        id: "auto-1".into(),
        name: "Test Automation".into(),
        enabled: true,
        trigger: Trigger::Manual,
        conditions: vec![],
        actions: vec![AutomationAction::Notify { message: "test".into() }],
    };
    
    hub.add_automation(auto).await.unwrap();
    
    let autos = hub.list_automations().await;
    assert_eq!(autos.len(), 1);
    assert_eq!(autos[0].id, "auto-1");
    
    let _ = tokio::fs::remove_dir_all(&config.data_dir).await;
}

#[tokio::test]
async fn hub_remove_automation() {
    let config = hub_config();
    let hub = Arc::new(SmartHomeHub::new(config.clone()));
    
    hub.add_automation(Automation {
        id: "auto-1".into(),
        name: "Test".into(),
        enabled: true,
        trigger: Trigger::Manual,
        conditions: vec![],
        actions: vec![],
    })
    .await
    .unwrap();
    
    assert_eq!(hub.list_automations().await.len(), 1);
    
    hub.remove_automation("auto-1").await.unwrap();
    
    assert!(hub.list_automations().await.is_empty());
    
    let _ = tokio::fs::remove_dir_all(&config.data_dir).await;
}

// ── Hub Without MIoT Tests ───────────────────────────────────────────────────

#[tokio::test]
async fn hub_set_property_without_miot_returns_not_supported() {
    let config = hub_config();
    let hub = SmartHomeHub::new(config.clone());
    
    hub.register_device(Device::new("dev-1", "Device 1", DeviceType::Matter))
        .await
        .unwrap();
    
    let result = hub
        .set_property("dev-1", 2, 1, serde_json::json!(true))
        .await;
    
    assert!(result.is_err());
    match result.unwrap_err() {
        a3net_smarthome::SmartHomeError::NotSupported(_) => {}
        e => panic!("Expected NotSupported, got {:?}", e),
    }
    
    let _ = tokio::fs::remove_dir_all(&config.data_dir).await;
}

#[tokio::test]
async fn hub_get_properties_without_miot_returns_not_supported() {
    let config = hub_config();
    let hub = SmartHomeHub::new(config.clone());
    
    let result = hub
        .get_properties("dev-1", vec![])
        .await;
    
    assert!(result.is_err());
    match result.unwrap_err() {
        a3net_smarthome::SmartHomeError::NotSupported(_) => {}
        e => panic!("Expected NotSupported, got {:?}", e),
    }
    
    let _ = tokio::fs::remove_dir_all(&config.data_dir).await;
}

#[tokio::test]
async fn hub_invoke_action_without_miot_returns_not_supported() {
    let config = hub_config();
    let hub = SmartHomeHub::new(config.clone());
    
    let result = hub
        .invoke_action("dev-1", 2, 1, vec![])
        .await;
    
    assert!(result.is_err());
    match result.unwrap_err() {
        a3net_smarthome::SmartHomeError::NotSupported(_) => {}
        e => panic!("Expected NotSupported, got {:?}", e),
    }
    
    let _ = tokio::fs::remove_dir_all(&config.data_dir).await;
}

#[tokio::test]
async fn hub_matter_commission_without_matter_returns_not_supported() {
    let config = hub_config();
    let hub = SmartHomeHub::new(config.clone());
    
    let result = hub.matter_commission("MT:...", None).await;
    
    assert!(result.is_err());
    match result.unwrap_err() {
        a3net_smarthome::SmartHomeError::NotSupported(_) => {}
        e => panic!("Expected NotSupported, got {:?}", e),
    }
    
    let _ = tokio::fs::remove_dir_all(&config.data_dir).await;
}

#[tokio::test]
async fn hub_list_matter_nodes_without_matter_returns_error() {
    let config = hub_config();
    let hub = SmartHomeHub::new(config.clone());
    
    let result = hub.list_matter_nodes().await;
    
    assert!(result.is_err());
    match result.unwrap_err() {
        a3net_smarthome::SmartHomeError::NotSupported(_) => {}
        e => panic!("Expected NotSupported, got {:?}", e),
    }
    
    let _ = tokio::fs::remove_dir_all(&config.data_dir).await;
}

// ── HomeKit Through Hub ──────────────────────────────────────────────────────

#[tokio::test]
async fn hub_list_homekit_accessories() {
    let config = hub_config();
    let hub = Arc::new(SmartHomeHub::new(config.clone()));
    
    let accessories = hub.list_homekit_accessories().await;
    // Should start empty
    assert!(accessories.is_empty());
    
    let _ = tokio::fs::remove_dir_all(&config.data_dir).await;
}

// ── Helper Functions ─────────────────────────────────────────────────────────

fn hub_config() -> HubConfig {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let suffix: String = (0..8).map(|_| format!("{:x}", rng.gen_range(0..16u8))).collect();
    
    HubConfig {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        enable_discovery: false,
        discovery_interval_secs: 3600,
        heartbeat_timeout_secs: 3600,
        data_dir: std::env::temp_dir()
            .join(format!("a3net-hub-test-{}", suffix))
            .to_string_lossy()
            .into_owned(),
    }
}

fn temp_dir() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let suffix: String = (0..8).map(|_| format!("{:x}", rng.gen_range(0..16u8))).collect();
    std::env::temp_dir()
        .join(format!("a3net-hub-test-{}", suffix))
        .to_string_lossy()
        .into_owned()
}
