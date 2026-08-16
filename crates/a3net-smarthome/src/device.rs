//! Device model for Smart Home devices

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::SystemTime;

/// Device type enumeration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceType {
    Xiaomi,
    Matter,
    Zigbee,
    ZWave,
    #[serde(rename = "wifi")]
    WiFi,
    Bluetooth,
    Custom(String),
}

/// Device capability
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceCapability {
    // ── binary / switch ─────────────────────────────────────────────
    OnOff,
    /// Lock state (locked / unlocked)
    Lock,
    /// Contact sensor (door, window)
    Contact,
    /// Water leak sensor
    WaterLeak,
    /// Carbon monoxide (CO) detector
    CO,
    /// Smoke detector
    Smoke,
    /// Vibration sensor
    Vibration,
    /// Tamper alert
    Tamper,
    /// Motion / occupancy detection
    Motion,
    /// Human-presence detection (mmWave)
    Presence,

    // ── analog / numeric ──────────────────────────────────────────────
    Brightness,
    ColorTemperature,
    Color,
    Temperature,
    Humidity,
    Battery,
    /// Remaining battery percentage (0–100)
    BatteryLevel,
    /// Battery voltage (V)
    BatteryVoltage,
    /// Electric current (A)
    Current,
    /// Voltage (V)
    Voltage,
    /// Instantaneous power draw (W)
    Power,
    /// Cumulative energy consumption (kWh)
    Energy,
    /// Power factor (0–1)
    PowerFactor,
    /// Illuminance / ambient light (lux)
    Illuminance,
    /// PM2.5 air quality index (µg/m³)
    PM25,
    /// Carbon dioxide concentration (ppm)
    CO2,
    /// Formaldehyde concentration (µg/m³)
    Formaldehyde,
    /// Filter remaining life percentage
    FilterLife,
    /// Charging state
    Charging,

    // ── control ──────────────────────────────────────────────────────
    FanSpeed,
    Mode,
    Volume,
    Position,
    /// Irrigation / valve position
    Irrigation,
    /// Air quality index (generic)
    AirQuality,
    /// Scene / preset recall
    Scene,
    /// Timer / schedule on the device itself
    Timer,

    // ── firmware / management ────────────────────────────────────────
    /// Identify (used during Matter/Thread pairing)
    Identify,
    /// Over-the-air firmware update
    OTA,
}

/// Device state - dynamic key-value pairs of current sensor/actuator values
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceState(pub HashMap<String, serde_json::Value>);

impl DeviceState {
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.0.get(key)
    }

    pub fn set(&mut self, key: impl Into<String>, value: serde_json::Value) {
        self.0.insert(key.into(), value);
    }
}

/// A smart home device
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    /// Unique device identifier
    pub device_id: String,
    /// Human-readable device name
    pub name: String,
    /// Device type / protocol
    pub device_type: DeviceType,
    /// Whether the device is reachable
    pub online: bool,
    /// Supported capabilities
    pub capabilities: Vec<DeviceCapability>,
    /// Current state snapshot
    pub state: DeviceState,
    /// Room / location
    pub room: Option<String>,
    /// Manufacturer model string
    pub model: Option<String>,
    /// Manufacturer name
    pub manufacturer: Option<String>,
    /// Firmware version
    pub firmware_version: Option<String>,
    /// Local IP address (if applicable)
    pub ip_address: Option<String>,
    /// When the device was last seen online
    pub last_seen: Option<SystemTime>,
    /// When the device was registered with this hub
    pub registered_at: SystemTime,
}

impl Device {
    pub fn new(device_id: impl Into<String>, name: impl Into<String>, device_type: DeviceType) -> Self {
        Self {
            device_id: device_id.into(),
            name: name.into(),
            device_type,
            online: false,
            capabilities: Vec::new(),
            state: DeviceState::new(),
            room: None,
            model: None,
            manufacturer: None,
            firmware_version: None,
            ip_address: None,
            last_seen: None,
            registered_at: SystemTime::now(),
        }
    }

    pub fn with_capabilities(mut self, caps: Vec<DeviceCapability>) -> Self {
        self.capabilities = caps;
        self
    }

    pub fn with_room(mut self, room: impl Into<String>) -> Self {
        self.room = Some(room.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>, manufacturer: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self.manufacturer = Some(manufacturer.into());
        self
    }
}
