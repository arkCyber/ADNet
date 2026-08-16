//! MQTT message types and topic conventions.

use serde::{Deserialize, Serialize};

/// MQTT topic namespace for A3Net smart home devices.
///
/// Topics follow the pattern: `a3net/{device_id}/{property}`
/// Commands use: `a3net/{device_id}/set`
#[derive(Debug, Clone)]
pub struct MqttTopic {
    pub base: String,
}

impl MqttTopic {
    pub fn new(base: impl Into<String>) -> Self {
        Self { base: base.into() }
    }

    /// Topic for publishing a device property state.
    /// Example: `a3net/lamp-1/2.1` (for siid.piid)
    pub fn state(&self, device_id: &str, property: &str) -> String {
        format!("{}/{}", self.device_prefix(device_id), property)
    }

    /// Topic for subscribing to property set commands.
    pub fn set(&self, device_id: &str) -> String {
        format!("{}/set", self.device_prefix(device_id))
    }

    /// Topic for device availability (online/offline).
    pub fn availability(&self, device_id: &str) -> String {
        format!("{}/availability", self.device_prefix(device_id))
    }

    /// Topic for device discovery announcements.
    pub fn discovery(&self) -> String {
        format!("{}/discovery", self.base)
    }

    fn device_prefix(&self, device_id: &str) -> String {
        format!("{}/{}", self.base, device_id)
    }
}

/// MQTT message payload for device state updates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MqttStatePayload {
    /// Device unique identifier
    pub device_id: String,
    /// Property identifier (e.g., "2.1" for siid.piid)
    pub property: String,
    /// Property value (JSON)
    pub value: serde_json::Value,
    /// Timestamp (RFC 3339)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// MQTT message payload for property set commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MqttSetPayload {
    /// Property identifier
    pub property: String,
    /// New value
    pub value: serde_json::Value,
}

/// MQTT message payload for device discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MqttDiscoveryPayload {
    /// Device unique identifier
    pub device_id: String,
    /// Human-readable name
    pub name: String,
    /// Device model
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Manufacturer
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    /// Supported properties
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub properties: Vec<String>,
    /// Device capabilities
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub capabilities: Vec<String>,
}

/// MQTT message payload for availability (online/offline).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MqttAvailabilityPayload {
    pub device_id: String,
    pub available: bool,
}
