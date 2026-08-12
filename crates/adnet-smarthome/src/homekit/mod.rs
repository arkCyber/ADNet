//! HomeKit integration support.
//!
//! ## Integration Strategy
//!
//! ADNet Smart Home Hub supports HomeKit through Matter protocol bridge:
//!
//! - **Matter is the modern successor to HomeKit**: Matter (formerly CHIP) is
//!   designed to be the unified smart home standard, with native Apple HomeKit
//!   support built-in since iOS 16.
//!
//! - **Direct Home App Integration**: Matter-certified devices appear directly in
//!   the Apple Home app without requiring any adapter or bridge.
//!
//! - **Zero-Configuration**: As long as devices are Matter-certified, they
//!   work with Apple Home app out of the box.
//!
//! ## ADNet Matter-HomeKit Mapping
//!
//! ADNet automatically maps device capabilities to Matter clusters, which Apple
//! recognizes as compatible HomeKit functionality:
//!
//! | ADNet Capability | Matter Cluster | HomeKit Equivalent |
//! |------------------|---------------|-------------------|
//! | OnOff           | 0x0006       | Switch/Light       |
//! | Brightness      | 0x0008       | Dimmable Light     |
//! | Color           | 0x0300       | Color Light        |
//! | ColorTemperature | 0x0300       | Tunable White      |
//! | Temperature     | 0x0402       | Temperature Sensor |
//! | Humidity       | 0x0405       | Humidity Sensor   |
//! | Lock           | 0x0101       | Door Lock         |
//! | Motion         | 0x0406       | Occupancy Sensor  |
//!
//! ## Using ADNet with Apple Home
//!
//! 1. Commission Matter devices through ADNet's REST API or CLI
//! 2. The devices become discoverable by iOS Home app
//! 3. Scan the Matter setup QR code or use device ID to add
//! 4. Control devices directly from Apple Home app or Siri
//!
//! ## Direct HAP Accessory Server (Future)
//!
//! For devices that are not Matter-certified, a future enhancement could add
//! a direct HAP accessory server. This would require resolving the
//! dependency conflict with `mdns-sd` (both depend on `if-addrs-sys`).
//!
//! Workaround options:
//! - Use a separate process with `hap` crate
//! - Fork `hap` to use `mdns-sd` instead of `libmdns`
//! - Bridge via Home Assistant MQTT

pub mod types;

use crate::device::{Device, DeviceCapability, DeviceType};
use crate::error::{Result, SmartHomeError};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

/// HomeKit accessory information (logical view through Matter).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeKitAccessory {
    /// Unique accessory ID
    pub aid: u64,
    /// Category identifier
    pub category: HomeKitCategory,
    /// Device associated with this accessory
    pub device_id: String,
    /// Accessory name (as shown in Home app)
    pub name: String,
    /// Manufacturer name
    pub manufacturer: String,
    /// Model name
    pub model: String,
    /// Serial number
    pub serial_number: String,
    /// Firmware revision
    pub firmware_revision: String,
    /// Supported HomeKit characteristics
    pub characteristics: Vec<HomeKitCharacteristic>,
}

impl HomeKitAccessory {
    /// Create a new HomeKit accessory.
    pub fn new(aid: u64, name: impl Into<String>, category: HomeKitCategory) -> Self {
        Self {
            aid,
            category,
            device_id: String::new(),
            name: name.into(),
            manufacturer: "ADNet".into(),
            model: "Smart Hub".into(),
            serial_number: format!("ADNET-{:08X}", aid),
            firmware_revision: "1.0.0".into(),
            characteristics: Vec::new(),
        }
    }

    pub fn with_device(mut self, device_id: impl Into<String>) -> Self {
        self.device_id = device_id.into();
        self
    }

    pub fn with_characteristic(mut self, char: HomeKitCharacteristic) -> Self {
        self.characteristics.push(char);
        self
    }

    /// Convert an ADNet device to HomeKit accessories.
    /// This maps ADNet capabilities through Matter clusters to HomeKit equivalents.
    pub fn from_device(device: &Device) -> Vec<Self> {
        let mut accessories = Vec::new();
        let aid = 1;

        let category = category_from_capabilities(&device.capabilities);
        
        let mut accessory = HomeKitAccessory::new(
            aid,
            device.name.clone(),
            category,
        )
        .with_device(device.device_id.clone());

        // Add characteristics based on device capabilities
        for cap in &device.capabilities {
            if let Some(char) = characteristic_from_capability(cap) {
                accessory = accessory.with_characteristic(char);
            }
        }

        // Add required identify characteristic
        accessory = accessory.with_characteristic(HomeKitCharacteristic::identify());

        accessories.push(accessory);
        accessories
    }
}

/// HomeKit accessory categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HomeKitCategory {
    Lightbulb,
    Switch,
    Thermostat,
    Lock,
    Door,
    Window,
    WindowCovering,
    Sensor,
    SecuritySystem,
    Fan,
    AirPurifier,
    Heater,
    AirConditioner,
    Humidifier,
    Dehumidifier,
    Irrigation,
    Valve,
    Other,
}

impl HomeKitCategory {
    /// Get the HomeKit category ID (matches HAP specification).
    pub fn category_id(&self) -> u32 {
        match self {
            Self::Lightbulb => 6,
            Self::Switch => 7,
            Self::Thermostat => 8,
            Self::Lock => 9,
            Self::Door => 10,
            Self::Window => 11,
            Self::WindowCovering => 13,
            Self::Sensor => 17,
            Self::SecuritySystem => 18,
            Self::Fan => 21,
            Self::AirPurifier => 22,
            Self::Heater => 27,
            Self::AirConditioner => 28,
            Self::Humidifier => 30,
            Self::Dehumidifier => 31,
            Self::Irrigation => 37,
            Self::Valve => 38,
            Self::Other => 0,
        }
    }

    /// Get the Matter device type ID for this category.
    pub fn matter_device_type(&self) -> u16 {
        match self {
            Self::Lightbulb => 0x0100,
            Self::Switch => 0x0104,
            Self::Thermostat => 0x0116,
            Self::Lock => 0x010E,
            Self::Door => 0x010A,
            Self::Window => 0x010B,
            Self::WindowCovering => 0x0200,
            Self::Sensor => 0x0106,
            Self::Fan => 0x0115,
            Self::AirPurifier => 0x0117,
            Self::Humidifier => 0x011D,
            Self::Dehumidifier => 0x011E,
            _ => 0,
        }
    }
}

/// HomeKit characteristic types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HomeKitCharacteristicType {
    On,
    Brightness,
    Hue,
    Saturation,
    ColorTemperature,
    CurrentTemperature,
    TargetTemperature,
    CurrentRelativeHumidity,
    TargetRelativeHumidity,
    LockCurrentState,
    LockTargetState,
    CurrentPosition,
    TargetPosition,
    PositionState,
    Tampered,
    BatteryLevel,
    ChargingState,
    StatusActive,
    MotionDetected,
    ContactState,
    Identify,
}

impl HomeKitCharacteristicType {
    /// Get the HAP UUID for this characteristic.
    pub fn uuid(&self) -> &'static str {
        match self {
            Self::On => "00000025-0000-1000-8000-0026BB765291",
            Self::Brightness => "00000008-0000-1000-8000-0026BB765291",
            Self::Hue => "00000013-0000-1000-8000-0026BB765291",
            Self::Saturation => "00000014-0000-1000-8000-0026BB765291",
            Self::ColorTemperature => "000000CE-0000-1000-8000-0026BB765291",
            Self::CurrentTemperature => "00000011-0000-1000-8000-0026BB765291",
            Self::TargetTemperature => "00000035-0000-1000-8000-0026BB765291",
            Self::CurrentRelativeHumidity => "00000010-0000-1000-8000-0026BB765291",
            Self::TargetRelativeHumidity => "00000034-0000-1000-8000-0026BB765291",
            Self::LockCurrentState => "00000019-0000-1000-8000-0026BB765291",
            Self::LockTargetState => "0000001E-0000-1000-8000-0026BB765291",
            Self::CurrentPosition => "00000031-0000-1000-8000-0026BB765291",
            Self::TargetPosition => "0000007C-0000-1000-8000-0026BB765291",
            Self::PositionState => "00000032-0000-1000-8000-0026BB765291",
            Self::Tampered => "000000A7-0000-1000-8000-0026BB765291",
            Self::BatteryLevel => "00000068-0000-1000-8000-0026BB765291",
            Self::ChargingState => "0000008F-0000-1000-8000-0026BB765291",
            Self::StatusActive => "00000075-0000-1000-8000-0026BB765291",
            Self::MotionDetected => "00000022-0000-1000-8000-0026BB765291",
            Self::ContactState => "0000006A-0000-1000-8000-0026BB765291",
            Self::Identify => "00000014-0000-1000-8000-0026BB765291",
        }
    }

    /// Get the Matter cluster ID for this characteristic.
    pub fn matter_cluster(&self) -> u32 {
        match self {
            Self::On => 0x0006,
            Self::Brightness => 0x0008,
            Self::Hue => 0x0300,
            Self::Saturation => 0x0300,
            Self::ColorTemperature => 0x0300,
            Self::CurrentTemperature => 0x0402,
            Self::TargetTemperature => 0x0201,
            Self::CurrentRelativeHumidity => 0x0405,
            Self::TargetRelativeHumidity => 0x0201,
            Self::LockCurrentState => 0x0101,
            Self::LockTargetState => 0x0101,
            Self::CurrentPosition => 0x0102,
            Self::TargetPosition => 0x0102,
            Self::PositionState => 0x0102,
            Self::Tampered => 0x0500,
            Self::BatteryLevel => 0x002E,
            Self::ChargingState => 0x002E,
            Self::StatusActive => 0x0201,
            Self::MotionDetected => 0x0406,
            Self::ContactState => 0x000E,
            Self::Identify => 0x0000,
        }
    }
}

/// A HomeKit characteristic with value and permissions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeKitCharacteristic {
    /// Characteristic type
    pub char_type: HomeKitCharacteristicType,
    /// Instance ID within the accessory
    pub iid: u64,
    /// Current value
    pub value: serde_json::Value,
    /// Value constraints
    pub format: HomeKitFormat,
    /// Minimum value (for numeric types)
    pub min_value: Option<f64>,
    /// Maximum value (for numeric types)
    pub max_value: Option<f64>,
    /// Step value (for numeric types)
    pub step: Option<f64>,
    /// Unit (e.g., "celsius", "percentage")
    pub unit: Option<String>,
    /// Read permission
    pub read: bool,
    /// Write permission
    pub write: bool,
    /// Notify permission
    pub notify: bool,
}

impl HomeKitCharacteristic {
    /// Create the Identify characteristic (required for all accessories).
    pub fn identify() -> Self {
        Self {
            char_type: HomeKitCharacteristicType::Identify,
            iid: 2,
            value: serde_json::json!(false),
            format: HomeKitFormat::Bool,
            min_value: None,
            max_value: None,
            step: None,
            unit: None,
            read: true,
            write: true,
            notify: false,
        }
    }

    /// Create an On/Off characteristic.
    pub fn on_off() -> Self {
        Self {
            char_type: HomeKitCharacteristicType::On,
            iid: 3,
            value: serde_json::json!(false),
            format: HomeKitFormat::Bool,
            min_value: None,
            max_value: None,
            step: None,
            unit: None,
            read: true,
            write: true,
            notify: true,
        }
    }

    /// Create a brightness characteristic (0-100%).
    pub fn brightness() -> Self {
        Self {
            char_type: HomeKitCharacteristicType::Brightness,
            iid: 4,
            value: serde_json::json!(100),
            format: HomeKitFormat::Int,
            min_value: Some(0.0),
            max_value: Some(100.0),
            step: Some(1.0),
            unit: Some("percentage".into()),
            read: true,
            write: true,
            notify: true,
        }
    }

    /// Create a temperature characteristic.
    pub fn temperature() -> Self {
        Self {
            char_type: HomeKitCharacteristicType::CurrentTemperature,
            iid: 5,
            value: serde_json::json!(20.0),
            format: HomeKitFormat::Float,
            min_value: Some(-273.15),
            max_value: Some(1000.0),
            step: None,
            unit: Some("celsius".into()),
            read: true,
            write: false,
            notify: true,
        }
    }
}

/// HomeKit value formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HomeKitFormat {
    #[serde(rename = "bool")]
    Bool,
    #[serde(rename = "uint8")]
    Uint8,
    #[serde(rename = "uint16")]
    Uint16,
    #[serde(rename = "uint32")]
    Uint32,
    #[serde(rename = "int")]
    Int,
    #[serde(rename = "float")]
    Float,
    #[serde(rename = "string")]
    String,
    #[serde(rename = "tlv8")]
    Tlv8,
}

/// HomeKit bridge configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeKitConfig {
    /// Bridge display name
    pub name: String,
    /// Port for HAP server (default: 51827)
    pub port: u16,
    /// Pin code for pairing (format: XXX-XX-XXX)
    pub pin: String,
    /// Model identifier
    pub model: String,
    /// Manufacturer identifier
    pub manufacturer: String,
    /// Enable Matter bridge mode (recommended)
    pub use_matter_bridge: bool,
}

impl Default for HomeKitConfig {
    fn default() -> Self {
        Self {
            name: "ADNet Smart Hub".into(),
            port: 51827,
            pin: "123-45-678".into(),
            model: "ADNet-Hub-1".into(),
            manufacturer: "ADNet".into(),
            use_matter_bridge: true,
        }
    }
}

/// HomeKit accessory bridge.
///
/// Manages HomeKit accessories through Matter protocol integration.
pub struct HomeKitBridge {
    config: HomeKitConfig,
    accessories: Arc<RwLock<HashMap<u64, HomeKitAccessory>>>,
}

impl HomeKitBridge {
    pub fn new(config: HomeKitConfig) -> Self {
        Self {
            config,
            accessories: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Add an accessory for a device.
    pub async fn add_accessory(&self, device: &Device) -> Result<()> {
        let accessories = HomeKitAccessory::from_device(device);
        let mut guard = self.accessories.write().await;
        for acc in accessories {
            info!("HomeKit accessory registered: {} (AID: {}, via Matter)", acc.name, acc.aid);
            guard.insert(acc.aid, acc);
        }
        Ok(())
    }

    /// Remove an accessory by device ID.
    pub async fn remove_accessory(&self, device_id: &str) -> Result<()> {
        let mut guard = self.accessories.write().await;
        guard.retain(|_, acc| acc.device_id != device_id);
        Ok(())
    }

    /// List all accessories.
    pub async fn list_accessories(&self) -> Vec<HomeKitAccessory> {
        self.accessories.read().await.values().cloned().collect()
    }

    /// Update a characteristic value.
    pub async fn update_value(
        &self,
        aid: u64,
        iid: u64,
        value: serde_json::Value,
    ) -> Result<()> {
        let mut guard = self.accessories.write().await;
        if let Some(acc) = guard.get_mut(&aid) {
            if let Some(char) = acc.characteristics.iter_mut().find(|c| c.iid == iid) {
                char.value = value;
                return Ok(());
            }
        }
        Err(SmartHomeError::DeviceControl(format!(
            "characteristic not found: AID={}, IID={}",
            aid, iid
        )))
    }

    /// Get accessory by AID.
    pub async fn get_accessory(&self, aid: u64) -> Option<HomeKitAccessory> {
        self.accessories.read().await.get(&aid).cloned()
    }

    /// Get configuration.
    pub fn config(&self) -> &HomeKitConfig {
        &self.config
    }
}

/// Convert ADNet device capabilities to a HomeKit category.
fn category_from_capabilities(capabilities: &[DeviceCapability]) -> HomeKitCategory {
    if capabilities.contains(&DeviceCapability::OnOff) {
        if capabilities.contains(&DeviceCapability::Brightness) {
            return HomeKitCategory::Lightbulb;
        }
        return HomeKitCategory::Switch;
    }
    if capabilities.contains(&DeviceCapability::Lock) {
        return HomeKitCategory::Lock;
    }
    if capabilities.contains(&DeviceCapability::Temperature) {
        if capabilities.contains(&DeviceCapability::Humidity) {
            return HomeKitCategory::Sensor;
        }
        return HomeKitCategory::Thermostat;
    }
    if capabilities.contains(&DeviceCapability::Position) {
        return HomeKitCategory::WindowCovering;
    }
    if capabilities.contains(&DeviceCapability::Motion) || capabilities.contains(&DeviceCapability::Contact) {
        return HomeKitCategory::Sensor;
    }
    HomeKitCategory::Other
}

/// Convert ADNet capability to a HomeKit characteristic.
fn characteristic_from_capability(cap: &DeviceCapability) -> Option<HomeKitCharacteristic> {
    match cap {
        DeviceCapability::OnOff => Some(HomeKitCharacteristic::on_off()),
        DeviceCapability::Brightness => Some(HomeKitCharacteristic::brightness()),
        DeviceCapability::Temperature => Some(HomeKitCharacteristic::temperature()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn homekit_category_ids() {
        assert_eq!(HomeKitCategory::Lightbulb.category_id(), 6);
        assert_eq!(HomeKitCategory::Switch.category_id(), 7);
        assert_eq!(HomeKitCategory::Lock.category_id(), 9);
        assert_eq!(HomeKitCategory::Sensor.category_id(), 17);
    }

    #[test]
    fn homekit_category_matter_types() {
        assert_eq!(HomeKitCategory::Lightbulb.matter_device_type(), 0x0100);
        assert_eq!(HomeKitCategory::Lock.matter_device_type(), 0x010E);
        assert_eq!(HomeKitCategory::Thermostat.matter_device_type(), 0x0116);
    }

    #[test]
    fn accessory_from_device_light() {
        let device = Device::new("lamp-1", "Living Room Light", DeviceType::Matter)
            .with_capabilities(vec![DeviceCapability::OnOff, DeviceCapability::Brightness]);

        let accessories = HomeKitAccessory::from_device(&device);
        assert_eq!(accessories.len(), 1);
        assert_eq!(accessories[0].category, HomeKitCategory::Lightbulb);
        assert_eq!(accessories[0].name, "Living Room Light");
    }

    #[test]
    fn accessory_from_device_lock() {
        let device = Device::new("lock-1", "Front Door Lock", DeviceType::Matter)
            .with_capabilities(vec![DeviceCapability::Lock]);

        let accessories = HomeKitAccessory::from_device(&device);
        assert_eq!(accessories.len(), 1);
        assert_eq!(accessories[0].category, HomeKitCategory::Lock);
    }

    #[test]
    fn accessory_from_device_sensor() {
        let device = Device::new("sensor-1", "Motion Sensor", DeviceType::Matter)
            .with_capabilities(vec![DeviceCapability::Motion]);

        let accessories = HomeKitAccessory::from_device(&device);
        assert_eq!(accessories.len(), 1);
        assert_eq!(accessories[0].category, HomeKitCategory::Sensor);
    }

    #[test]
    fn characteristic_uuids() {
        assert!(HomeKitCharacteristicType::On.uuid().starts_with("000000"));
        assert!(HomeKitCharacteristicType::Brightness.uuid().starts_with("000000"));
        assert!(HomeKitCharacteristicType::CurrentTemperature.uuid().starts_with("000000"));
    }

    #[test]
    fn characteristic_matter_clusters() {
        assert_eq!(HomeKitCharacteristicType::On.matter_cluster(), 0x0006);
        assert_eq!(HomeKitCharacteristicType::Brightness.matter_cluster(), 0x0008);
        assert_eq!(HomeKitCharacteristicType::CurrentTemperature.matter_cluster(), 0x0402);
    }

    #[tokio::test]
    async fn add_and_list_accessories() {
        let bridge = HomeKitBridge::new(HomeKitConfig::default());
        let device = Device::new("dev-1", "Test Light", DeviceType::Matter)
            .with_capabilities(vec![DeviceCapability::OnOff]);

        bridge.add_accessory(&device).await.unwrap();
        let accessories = bridge.list_accessories().await;
        assert_eq!(accessories.len(), 1);
    }

    #[tokio::test]
    async fn remove_accessory() {
        let bridge = HomeKitBridge::new(HomeKitConfig::default());
        let device = Device::new("dev-1", "Test Light", DeviceType::Matter)
            .with_capabilities(vec![DeviceCapability::OnOff]);

        bridge.add_accessory(&device).await.unwrap();
        assert_eq!(bridge.list_accessories().await.len(), 1);
        
        bridge.remove_accessory("dev-1").await.unwrap();
        assert_eq!(bridge.list_accessories().await.len(), 0);
    }
}
