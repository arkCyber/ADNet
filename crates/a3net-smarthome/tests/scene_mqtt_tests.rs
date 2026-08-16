//! Integration tests for Scene and MQTT functionality.

use a3net_smarthome::scene::{Scene, SceneAction, SceneManager};
use a3net_smarthome::mqtt::{MqttConfig, MqttTopic};
use a3net_smarthome::homekit::{HomeKitConfig, HomeKitBridge, HomeKitCategory, HomeKitAccessory};
use a3net_smarthome::device::{Device, DeviceType, DeviceCapability};

// ── Scene Manager Tests ──────────────────────────────────────────────────────

#[tokio::test]
async fn scene_manager_add_and_get() {
    let mgr = SceneManager::new(temp_dir());
    
    let scene = Scene::new("movie", "Movie Time")
        .with_room("Living Room")
        .with_icon("movie")
        .add_action(SceneAction::SetProperty {
            device_id: "lamp-1".into(),
            siid: 2,
            piid: 1,
            value: serde_json::json!(false),
        })
        .add_action(SceneAction::Delay { millis: 500 })
        .add_action(SceneAction::SetProperty {
            device_id: "tv-1".into(),
            siid: 3,
            piid: 1,
            value: serde_json::json!(true),
        });

    mgr.add(scene).await.unwrap();
    
    let found = mgr.get("movie").await.unwrap();
    assert_eq!(found.name, "Movie Time");
    assert_eq!(found.room, Some("Living Room".to_string()));
    assert_eq!(found.actions.len(), 3);
}

#[tokio::test]
async fn scene_manager_list_all() {
    let mgr = SceneManager::new(temp_dir());
    
    mgr.add(Scene::new("morning", "Good Morning")).await.unwrap();
    mgr.add(Scene::new("night", "Good Night")).await.unwrap();
    mgr.add(Scene::new("away", "Away Mode")).await.unwrap();

    let scenes = mgr.list_all().await;
    assert_eq!(scenes.len(), 3);
}

#[tokio::test]
async fn scene_manager_remove() {
    let mgr = SceneManager::new(temp_dir());
    
    mgr.add(Scene::new("temp", "Temp Scene")).await.unwrap();
    assert!(mgr.get("temp").await.is_some());
    
    mgr.remove("temp").await.unwrap();
    assert!(mgr.get("temp").await.is_none());
}

#[tokio::test]
async fn scene_manager_persistence() {
    let dir = temp_dir();
    
    // Create and save scenes
    {
        let mgr = SceneManager::new(dir.clone());
        mgr.add(Scene::new("s1", "Scene 1")).await.unwrap();
        mgr.add(Scene::new("s2", "Scene 2")).await.unwrap();
        mgr.add(Scene::new("s3", "Scene 3")).await.unwrap();
    }
    
    // Load scenes in new instance
    let mgr2 = SceneManager::new(dir.clone());
    mgr2.load().await.unwrap();
    
    let scenes = mgr2.list_all().await;
    assert_eq!(scenes.len(), 3);
    
    let _ = tokio::fs::remove_dir_all(&dir).await;
}

// ── MQTT Topic Tests ─────────────────────────────────────────────────────────

#[test]
fn mqtt_topic_custom_base() {
    let topic = MqttTopic::new("home");
    assert_eq!(topic.state("light-1", "2.1"), "home/light-1/2.1");
    assert_eq!(topic.set("light-1"), "home/light-1/set");
    assert_eq!(topic.availability("light-1"), "home/light-1/availability");
    assert_eq!(topic.discovery(), "home/discovery");
}

#[test]
fn mqtt_config_default() {
    let config = MqttConfig::default();
    assert_eq!(config.host, "localhost");
    assert_eq!(config.port, 1883);
    assert_eq!(config.topic_base, "a3net");
    assert_eq!(config.keep_alive_secs, 60);
}

#[test]
fn mqtt_config_custom() {
    let config = MqttConfig {
        host: "mqtt.example.com".into(),
        port: 8883,
        client_id: Some("my-hub".into()),
        username: Some("user".into()),
        password: Some("pass".into()),
        topic_base: "smart".into(),
        keep_alive_secs: 120,
    };
    
    assert_eq!(config.host, "mqtt.example.com");
    assert_eq!(config.port, 8883);
    assert_eq!(config.client_id, Some("my-hub".into()));
    assert_eq!(config.username, Some("user".into()));
}

// ── HomeKit Tests ────────────────────────────────────────────────────────────

#[test]
fn homekit_category_names() {
    assert_eq!(HomeKitCategory::Lightbulb.category_id(), 6);
    assert_eq!(HomeKitCategory::Switch.category_id(), 7);
    assert_eq!(HomeKitCategory::Thermostat.category_id(), 8);
    assert_eq!(HomeKitCategory::Lock.category_id(), 9);
    assert_eq!(HomeKitCategory::Door.category_id(), 10);
    assert_eq!(HomeKitCategory::WindowCovering.category_id(), 13);
    assert_eq!(HomeKitCategory::Sensor.category_id(), 17);
    assert_eq!(HomeKitCategory::Fan.category_id(), 21);
}

#[test]
fn homekit_accessory_creation() {
    let acc = HomeKitAccessory::new(1, "My Light", HomeKitCategory::Lightbulb);
    assert_eq!(acc.aid, 1);
    assert_eq!(acc.name, "My Light");
    assert_eq!(acc.category, HomeKitCategory::Lightbulb);
}

#[tokio::test]
async fn homekit_bridge_operations() {
    let bridge = HomeKitBridge::new(HomeKitConfig::default());
    
    // Create a test device
    let device = Device::new("test-lamp", "Test Lamp", DeviceType::Matter)
        .with_model("Lamp Model", "Test Manufacturer")
        .with_capabilities(vec![
            DeviceCapability::OnOff,
            DeviceCapability::Brightness,
            DeviceCapability::Color,
        ]);
    
    // Add accessory
    bridge.add_accessory(&device).await.unwrap();
    
    // List accessories
    let accessories = bridge.list_accessories().await;
    assert!(!accessories.is_empty());
    assert_eq!(accessories[0].device_id, "test-lamp");
}

#[test]
fn homekit_accessory_from_device_light() {
    let device = Device::new("lamp-1", "Bedroom Lamp", DeviceType::Matter)
        .with_capabilities(vec![DeviceCapability::OnOff, DeviceCapability::Brightness]);

    let accessories = HomeKitAccessory::from_device(&device);
    assert_eq!(accessories.len(), 1);
    assert_eq!(accessories[0].category, HomeKitCategory::Lightbulb);
}

#[test]
fn homekit_accessory_from_device_switch() {
    let device = Device::new("switch-1", "Wall Switch", DeviceType::Matter)
        .with_capabilities(vec![DeviceCapability::OnOff]);

    let accessories = HomeKitAccessory::from_device(&device);
    assert_eq!(accessories.len(), 1);
    assert_eq!(accessories[0].category, HomeKitCategory::Switch);
}

#[test]
fn homekit_accessory_from_device_lock() {
    let device = Device::new("lock-1", "Front Door Lock", DeviceType::Matter)
        .with_capabilities(vec![DeviceCapability::Lock]);

    let accessories = HomeKitAccessory::from_device(&device);
    assert_eq!(accessories.len(), 1);
    assert_eq!(accessories[0].category, HomeKitCategory::Lock);
}

#[test]
fn homekit_accessory_from_device_sensor() {
    let device = Device::new("sensor-1", "Motion Sensor", DeviceType::Matter)
        .with_capabilities(vec![DeviceCapability::Motion]);

    let accessories = HomeKitAccessory::from_device(&device);
    assert_eq!(accessories.len(), 1);
    assert_eq!(accessories[0].category, HomeKitCategory::Sensor);
}

// ── Scene Action Tests ────────────────────────────────────────────────────────

#[test]
fn scene_action_serialization() {
    let action = SceneAction::SetProperty {
        device_id: "dev-1".into(),
        siid: 2,
        piid: 1,
        value: serde_json::json!(true),
    };
    
    let json = serde_json::to_string(&action).unwrap();
    assert!(json.contains("set_property"));
    assert!(json.contains("dev-1"));
    
    let back: SceneAction = serde_json::from_str(&json).unwrap();
    match back {
        SceneAction::SetProperty { device_id, .. } => {
            assert_eq!(device_id, "dev-1");
        }
        _ => panic!("Wrong variant"),
    }
}

#[test]
fn scene_delay_serialization() {
    let action = SceneAction::Delay { millis: 1000 };
    
    let json = serde_json::to_string(&action).unwrap();
    assert!(json.contains("delay"));
    
    let back: SceneAction = serde_json::from_str(&json).unwrap();
    match back {
        SceneAction::Delay { millis } => {
            assert_eq!(millis, 1000);
        }
        _ => panic!("Wrong variant"),
    }
}

// ── Helper Functions ─────────────────────────────────────────────────────────

fn temp_dir() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let suffix: String = (0..8).map(|_| format!("{:x}", rng.gen_range(0..16u8))).collect();
    std::env::temp_dir()
        .join(format!("a3net-smarthome-it-{}-{}", "scene-mqtt-hk", suffix))
        .to_string_lossy()
        .into_owned()
}
