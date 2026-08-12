//! Core data model for the contact roster.
//!
//! Ports the original `Contact` struct and its IoT sub-types from
//! `Exodus@src-backup/src-tauri/src/microservice/contact_directory_service.rs`
//! (lines 706–930) into ADNet-friendly types with proper enums and
//! invariants instead of free-form strings.

use serde::{Deserialize, Serialize};

/// Discriminator for [`Contact::contact_type`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContactType {
    Human,
    Agent,
    Iot,
}

impl ContactType {
    pub fn as_str(self) -> &'static str {
        match self {
            ContactType::Human => "human",
            ContactType::Agent => "agent",
            ContactType::Iot => "iot",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "human" => Some(ContactType::Human),
            "agent" => Some(ContactType::Agent),
            "iot" => Some(ContactType::Iot),
            _ => None,
        }
    }
}

/// Only meaningful when [`ContactType::Agent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentDeploymentType {
    Service,
    PersonalAssistant,
}

impl AgentDeploymentType {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentDeploymentType::Service => "service",
            AgentDeploymentType::PersonalAssistant => "personal_assistant",
        }
    }
}

/// Limits enforced by [`Contact`] validation. These mirror the
/// `ResourceLimits` / `InputValidator` defaults in the original codebase.
pub const MAX_CONTACT_NAME_LEN: usize = 1024;
pub const MAX_GROUPS_PER_CONTACT: usize = 64;
pub const MAX_TAGS_PER_CONTACT: usize = 64;

/// IoT device class — see `IoTDeviceType` in the original code.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoTDeviceType {
    SmartLight,
    Thermostat,
    Lock,
    Camera,
    Sensor,
    Appliance,
    Switch,
    Outlet,
    Fan,
    Vacuum,
    Speaker,
    Display,
    Controller,
    Gateway,
    Other(String),
}

impl IoTDeviceType {
    pub fn as_str(&self) -> &str {
        match self {
            IoTDeviceType::SmartLight => "smart_light",
            IoTDeviceType::Thermostat => "thermostat",
            IoTDeviceType::Lock => "lock",
            IoTDeviceType::Camera => "camera",
            IoTDeviceType::Sensor => "sensor",
            IoTDeviceType::Appliance => "appliance",
            IoTDeviceType::Switch => "switch",
            IoTDeviceType::Outlet => "outlet",
            IoTDeviceType::Fan => "fan",
            IoTDeviceType::Vacuum => "vacuum",
            IoTDeviceType::Speaker => "speaker",
            IoTDeviceType::Display => "display",
            IoTDeviceType::Controller => "controller",
            IoTDeviceType::Gateway => "gateway",
            IoTDeviceType::Other(s) => s.as_str(),
        }
    }

    pub fn known(s: &str) -> Option<Self> {
        Some(match s {
            "smart_light" => IoTDeviceType::SmartLight,
            "thermostat" => IoTDeviceType::Thermostat,
            "lock" => IoTDeviceType::Lock,
            "camera" => IoTDeviceType::Camera,
            "sensor" => IoTDeviceType::Sensor,
            "appliance" => IoTDeviceType::Appliance,
            "switch" => IoTDeviceType::Switch,
            "outlet" => IoTDeviceType::Outlet,
            "fan" => IoTDeviceType::Fan,
            "vacuum" => IoTDeviceType::Vacuum,
            "speaker" => IoTDeviceType::Speaker,
            "display" => IoTDeviceType::Display,
            "controller" => IoTDeviceType::Controller,
            "gateway" => IoTDeviceType::Gateway,
            other => IoTDeviceType::Other(other.to_string()),
        })
    }
}

/// IoT connectivity protocol.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoTProtocol {
    Mqtt,
    Coap,
    Zigbee,
    Zwave,
    Wifi,
    Ble,
    Matter,
    Thread,
    LoRaWAN,
    Modbus,
    Http,
    Ws,
    Other(String),
}

impl IoTProtocol {
    pub fn as_str(&self) -> &str {
        match self {
            IoTProtocol::Mqtt => "mqtt",
            IoTProtocol::Coap => "coap",
            IoTProtocol::Zigbee => "zigbee",
            IoTProtocol::Zwave => "zwave",
            IoTProtocol::Wifi => "wifi",
            IoTProtocol::Ble => "ble",
            IoTProtocol::Matter => "matter",
            IoTProtocol::Thread => "thread",
            IoTProtocol::LoRaWAN => "lorawan",
            IoTProtocol::Modbus => "modbus",
            IoTProtocol::Http => "http",
            IoTProtocol::Ws => "ws",
            IoTProtocol::Other(s) => s.as_str(),
        }
    }
}

/// IoT device reachability status.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoTStatus {
    Online,
    Offline,
    Error,
    Updating,
    Maintenance,
    Unknown,
}

impl IoTStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            IoTStatus::Online => "online",
            IoTStatus::Offline => "offline",
            IoTStatus::Error => "error",
            IoTStatus::Updating => "updating",
            IoTStatus::Maintenance => "maintenance",
            IoTStatus::Unknown => "unknown",
        }
    }

    pub fn is_online(&self) -> bool {
        matches!(self, IoTStatus::Online)
    }
}

/// IoT device capability tag.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoTCapability {
    OnOff,
    Dimming,
    Color,
    ColorTemperature,
    Brightness,
    MotionDetection,
    Temperature,
    Humidity,
    LockUnlock,
    OpenClose,
    Volume,
    Playback,
    Recording,
    Streaming,
    Automation,
    Scene,
    Schedule,
    EnergyMonitoring,
    Other(String),
}

impl IoTCapability {
    pub fn as_str(&self) -> &str {
        match self {
            IoTCapability::OnOff => "on_off",
            IoTCapability::Dimming => "dimming",
            IoTCapability::Color => "color",
            IoTCapability::ColorTemperature => "color_temperature",
            IoTCapability::Brightness => "brightness",
            IoTCapability::MotionDetection => "motion_detection",
            IoTCapability::Temperature => "temperature",
            IoTCapability::Humidity => "humidity",
            IoTCapability::LockUnlock => "lock_unlock",
            IoTCapability::OpenClose => "open_close",
            IoTCapability::Volume => "volume",
            IoTCapability::Playback => "playback",
            IoTCapability::Recording => "recording",
            IoTCapability::Streaming => "streaming",
            IoTCapability::Automation => "automation",
            IoTCapability::Scene => "scene",
            IoTCapability::Schedule => "schedule",
            IoTCapability::EnergyMonitoring => "energy_monitoring",
            IoTCapability::Other(s) => s.as_str(),
        }
    }
}

/// A single roster entry. Same struct serves humans, AI agents, and IoT
/// devices — `contact_type` discriminates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    pub contact_id: String,
    pub name: String,
    /// Serialised [`ContactType`] — kept as `String` for forward-compatible
    /// persistence matching the original code.
    pub contact_type: String,
    /// Only meaningful when `contact_type == "agent"`.
    pub agent_deployment_type: Option<String>,
    /// Agent ids this contact can be reached at (mesh / p2p).
    pub agent_ids: Vec<String>,
    /// P2P / mesh node id backing this contact.
    pub node_id: String,
    /// Group ids this contact belongs to.
    pub groups: Vec<String>,
    /// Free-form tags for search / filters.
    pub tags: Vec<String>,
    /// Free-form notes.
    pub notes: String,
    pub is_favorite: bool,
    pub is_blocked: bool,
    /// Unix seconds.
    pub created_at: u64,
    /// Unix seconds.
    pub last_contacted: u64,
    pub contact_count: u32,
    /// Optional bound public account id.
    pub public_account_id: Option<String>,

    // IoT-specific (only when `contact_type == "iot"`). Kept as String for
    // forward compatibility with the original code.
    pub iot_device_type: Option<String>,
    pub iot_protocol: Option<String>,
    pub iot_status: Option<String>,
    /// Unix seconds.
    pub iot_last_seen: Option<u64>,
    pub iot_capabilities: Option<Vec<String>>,
    pub iot_location: Option<String>,
}

impl Contact {
    /// Construct an empty human contact — convenient for tests / callers
    /// that will fill in the fields themselves.
    pub fn new_human(contact_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            contact_id: contact_id.into(),
            name: name.into(),
            contact_type: ContactType::Human.as_str().to_string(),
            agent_deployment_type: None,
            agent_ids: Vec::new(),
            node_id: String::new(),
            groups: Vec::new(),
            tags: Vec::new(),
            notes: String::new(),
            is_favorite: false,
            is_blocked: false,
            created_at: 0,
            last_contacted: 0,
            contact_count: 0,
            public_account_id: None,
            iot_device_type: None,
            iot_protocol: None,
            iot_status: None,
            iot_last_seen: None,
            iot_capabilities: None,
            iot_location: None,
        }
    }

    /// Convenience: is this an IoT device currently reporting online?
    pub fn is_iot_online(&self) -> bool {
        self.contact_type == ContactType::Iot.as_str()
            && self
                .iot_status
                .as_deref()
                .and_then(IoTStatus::known_from_str)
                .is_some_and(|s| s.is_online())
    }

    /// Validate IoT-specific fields. Mirrors `Contact::validate_iot_fields`.
    pub fn validate_iot_fields(&self) -> Result<(), String> {
        if self.contact_type == ContactType::Iot.as_str() {
            if self.iot_device_type.is_none() {
                return Err("iot device must have a device_type".to_string());
            }
            if self.iot_protocol.is_none() {
                return Err("iot device must have a protocol".to_string());
            }
            if self.iot_status.is_none() {
                return Err("iot device must have a status".to_string());
            }
            if self.node_id.is_empty() {
                return Err("iot device must have a node_id".to_string());
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// IoTStatus helper for `is_iot_online`.
// ---------------------------------------------------------------------------
impl IoTStatus {
    pub fn known_from_str(s: &str) -> Option<Self> {
        Some(match s {
            "online" => IoTStatus::Online,
            "offline" => IoTStatus::Offline,
            "error" => IoTStatus::Error,
            "updating" => IoTStatus::Updating,
            "maintenance" => IoTStatus::Maintenance,
            _ => return None,
        })
    }
}

/// IoT device event types — used by stores that broadcast changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum IoTEvent {
    DeviceAdded {
        contact_id: String,
        device_type: String,
        name: String,
        timestamp: u64,
    },
    DeviceRemoved {
        contact_id: String,
        device_type: String,
        name: String,
        timestamp: u64,
    },
    StatusChanged {
        contact_id: String,
        old_status: String,
        new_status: String,
        timestamp: u64,
    },
    LocationChanged {
        contact_id: String,
        old_location: Option<String>,
        new_location: Option<String>,
        timestamp: u64,
    },
    CapabilityAdded {
        contact_id: String,
        capability: String,
        timestamp: u64,
    },
    CapabilityRemoved {
        contact_id: String,
        capability: String,
        timestamp: u64,
    },
    Error {
        contact_id: String,
        error_message: String,
        timestamp: u64,
    },
}

impl IoTEvent {
    pub fn event_type(&self) -> &'static str {
        match self {
            IoTEvent::DeviceAdded { .. } => "device_added",
            IoTEvent::DeviceRemoved { .. } => "device_removed",
            IoTEvent::StatusChanged { .. } => "status_changed",
            IoTEvent::LocationChanged { .. } => "location_changed",
            IoTEvent::CapabilityAdded { .. } => "capability_added",
            IoTEvent::CapabilityRemoved { .. } => "capability_removed",
            IoTEvent::Error { .. } => "error",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_type_round_trip() {
        for ct in [ContactType::Human, ContactType::Agent, ContactType::Iot] {
            assert_eq!(ContactType::from_str(ct.as_str()), Some(ct));
        }
        assert_eq!(ContactType::from_str("nope"), None);
    }

    #[test]
    fn iot_online_predicate() {
        let mut c = Contact::new_human("c1", "Sensor");
        c.contact_type = ContactType::Iot.as_str().into();
        c.iot_status = Some(IoTStatus::Online.as_str().into());
        c.iot_device_type = Some(IoTDeviceType::Sensor.as_str().into());
        c.iot_protocol = Some(IoTProtocol::Mqtt.as_str().into());
        assert!(c.is_iot_online());
    }

    #[test]
    fn iot_validation_requires_device_type() {
        let mut c = Contact::new_human("c1", "Sensor");
        c.contact_type = ContactType::Iot.as_str().into();
        // missing iot_* fields
        assert!(c.validate_iot_fields().is_err());
    }
}