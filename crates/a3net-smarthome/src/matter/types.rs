//! Matter cluster → A3Net device capability mapping.
//!
//! Maps Matter cluster IDs (CSA spec, rev 1.4) to the protocol-agnostic
//! A3Net [`DeviceCapability`] set so the hub can treat Matter devices
//! identically to Xiaomi MIoT devices in automations and the REST API.

use crate::device::DeviceCapability;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// A Matter cluster ID (standard clusters, CSA Matter specification).
///
/// The enum discards explicit `#[repr(u32)]` so that variants sharing
/// the same numeric cluster ID (e.g. `Groups` and `GroupsAlt` both map to
/// 0x0004) don't conflict. The [`TryFrom<u32>`] impl resolves each
/// numeric ID to exactly one variant — the most commonly-used one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MatterCluster {
    // ── Groups / Scenes ────────────────────────────────────────────────────
    Groups,
    Scenes,

    // ── On/Off / Binary / Switch ──────────────────────────────────────────
    OnOff,
    LevelControl,
    BinaryInput,
    Switch,
    ContactSensor,
    PulseWidthModulation,
    OperationalState,
    OnOffSwitchConfiguration,
    BinaryOutput,

    // ── Access / Binding ──────────────────────────────────────────────────
    AccessControl,
    Binding,
    UserLabel,
    FixedLabel,
    Descriptor,
    NodeOperationalCredentials,

    // ── OTA / Admin ───────────────────────────────────────────────────────
    OtaSoftwareUpdateRequestor,
    OtaSoftwareUpdateProvider,
    AdministratorCommissioning,

    // ── Power ──────────────────────────────────────────────────────────────
    PowerSource,

    // ── Diagnostics ────────────────────────────────────────────────────────
    Diagnostics,
    SoftwareDiagnostics,
    NetworkCommissioning,
    GeneralDiagnostics,
    EthernetNetworkDiagnostics,
    WiFiNetworkDiagnostics,
    ThreadNetworkDiagnostics,
    DiagnosticsCluster,

    // ── Groups ────────────────────────────────────────────────────────────
    GroupKeyManagement,

    // ── Media / Proxy ─────────────────────────────────────────────────────
    ProxyDiscovery,
    ProxyConfiguration,
    Actions,
    WakeOnLan,
    Channel,
    TargetNavigator,

    // ── Sensors / Actuators ────────────────────────────────────────────────
    BooleanState,
    Irrigation,
    ValveConfiguration,
    AirQuality,
    FaultReport,
    FormaldehydeMeasurement,
    PM25Measurement,
    CarbonDioxideMeasurement,
    HumidityMeasurement,
    IlluminanceMeasurement,

    // ── Mode / Energy ─────────────────────────────────────────────────────
    ModeSelect,
    ShedControl,
    DeviceLevelControl,
    DeviceEnergyManagement,
    EnergyPreference,
    DeviceTemperature,

    // ── Barriers ──────────────────────────────────────────────────────────
    AccessBarriers,
    RadioControl,
    AuthenticationFactor,
    HumidityControl,

    // ── HVAC ─────────────────────────────────────────────────────────────
    Thermostat,
    FanControl,
    Heating,
    ThermostatUserInterfaceConfiguration,
    PumpConfigurationAndControl,

    // ── Covers ────────────────────────────────────────────────────────────
    WindowCovering,
    BarrierControl,

    // ── Color / Lighting ─────────────────────────────────────────────────
    ColorControl,
    BallastConfiguration,
    SiliconLabs,

    // ── Measurement ───────────────────────────────────────────────────────
    TemperatureMeasurement,
    PressureMeasurement,
    FlowMeasurement,
    RelativeHumidityMeasurement,
    OccupancySensing,

    // ── Security ────────────────────────────────────────────────────────────
    DoorLock,
    IasZone,
    IasAce,
    SmokeCoAlarm,
    BrightnessControl,
    AmbientLighting,
    Doorbell,

    // ── Electrical / Power ─────────────────────────────────────────────────
    ElectricalMeasurement,
    ElectricalEnergyMeasurement,
    ElectricalPowerMeasurement,
    ElectricalPowerRequest,
    PowerTopology,

    // ── Thread ─────────────────────────────────────────────────────────────
    ThreadTelemetry,
    ThreadTelemetryBridged,
    ThreadBorderRouterTelemetry,
    ThreadBorderRouterConfiguration,
    ThreadDiagnostics,
    ThreadBorderRouterDiagnostics,
    ThreadCapabilityDiscovery,
    ThreadCapabilityDiscoveryConfig,
    WiFiNetworkDiagnosticsBridged,
    ThreadNetworkDiagnosticsBridged,
    ThreadBorderRouterTelemetryBridged,
    ThreadBorderRouterConfigurationBridged,
    ThreadDiagnosticsBridged,
}

// ── TryFrom<u32> ─────────────────────────────────────────────────────────────
//
// Numeric cluster IDs are taken from the CSA Matter specification rev 1.4.
// Each match arm maps one numeric ID to exactly one enum variant.
// IDs that appear under multiple names in the spec resolve to the most
// common variant (e.g. 0x0004 → Groups, not GroupsAlt).
impl TryFrom<u32> for MatterCluster {
    type Error = u32;

    fn try_from(id: u32) -> Result<Self, Self::Error> {
        match id {
            // ── Groups / Scenes ──────────────────────────────────────────
            0x0004 => Ok(Self::Groups),
            0x0005 => Ok(Self::Scenes),

            // ── On/Off / Binary / Switch ──────────────────────────────────
            0x0006 => Ok(Self::OnOff),
            0x0008 => Ok(Self::LevelControl),
            0x0009 => Ok(Self::BinaryInput),
            0x003B => Ok(Self::Switch),
            0x000E => Ok(Self::ContactSensor),
            0x0015 => Ok(Self::PulseWidthModulation),
            0x001A => Ok(Self::OperationalState),
            0x001C => Ok(Self::OnOffSwitchConfiguration),
            0x0021 => Ok(Self::BinaryOutput),

            // ── Access / Binding / Descriptor ──────────────────────────────
            0x001E => Ok(Self::AccessControl),
            0x0029 => Ok(Self::Descriptor),
            0x002D => Ok(Self::NodeOperationalCredentials),
            0x002F => Ok(Self::AdministratorCommissioning),
            0x0030 => Ok(Self::Binding),
            0x0031 => Ok(Self::UserLabel),
            0x0032 => Ok(Self::FixedLabel),

            // ── OTA ────────────────────────────────────────────────────────
            0x0019 => Ok(Self::OtaSoftwareUpdateRequestor),
            0x002A => Ok(Self::OtaSoftwareUpdateProvider),

            // ── Power ─────────────────────────────────────────────────────
            0x002E => Ok(Self::PowerSource),

            // ── Diagnostics ────────────────────────────────────────────────
            // Matter spec cluster IDs:
            // GeneralDiagnostics=0x0035, EthernetNetworkDiagnostics=0x0036,
            // WiFiNetworkDiagnostics=0x0037, ThreadNetworkDiagnostics=0x0038
            0x0024 => Ok(Self::Diagnostics),
            0x0027 => Ok(Self::SoftwareDiagnostics),
            0x0034 => Ok(Self::NetworkCommissioning),
            0x0035 => Ok(Self::GeneralDiagnostics),
            0x0036 => Ok(Self::EthernetNetworkDiagnostics),
            0x0037 => Ok(Self::WiFiNetworkDiagnostics),
            0x0038 => Ok(Self::ThreadNetworkDiagnostics),
            0x003F => Ok(Self::GroupKeyManagement),

            // ── Media / Proxy ──────────────────────────────────────────────
            0x003C => Ok(Self::ProxyConfiguration),
            0x003D => Ok(Self::ProxyDiscovery),
            0x003E => Ok(Self::Actions),
            0x0050 => Ok(Self::WakeOnLan),
            0x0051 => Ok(Self::Channel),
            0x0052 => Ok(Self::TargetNavigator),

            // ── Sensors / Actuators ────────────────────────────────────────
            0x0053 => Ok(Self::BooleanState),
            0x0054 => Ok(Self::Irrigation),
            0x0055 => Ok(Self::ValveConfiguration),
            0x0056 => Ok(Self::AirQuality),
            0x0057 => Ok(Self::FaultReport),
            0x0058 => Ok(Self::FormaldehydeMeasurement),
            0x0059 => Ok(Self::PM25Measurement),
            0x005A => Ok(Self::CarbonDioxideMeasurement),
            0x005B => Ok(Self::HumidityMeasurement),
            0x005C => Ok(Self::IlluminanceMeasurement),

            // ── Mode / Shed / Energy ──────────────────────────────────────
            0x0060 => Ok(Self::ModeSelect),
            0x0061 => Ok(Self::ShedControl),
            0x0062 => Ok(Self::DeviceLevelControl),
            0x0063 => Ok(Self::DeviceEnergyManagement),
            0x0064 => Ok(Self::EnergyPreference),
            0x0065 => Ok(Self::DeviceTemperature),

            // ── Barriers ───────────────────────────────────────────────────
            0x006A => Ok(Self::AccessBarriers),
            0x006B => Ok(Self::RadioControl),
            0x006E => Ok(Self::AuthenticationFactor),
            0x006F => Ok(Self::HumidityControl),

            // ── HVAC ─────────────────────────────────────────────────────
            // Matter spec cluster IDs: Thermostat=0x0201, FanControl=0x0202,
            // Heating=0x0203, ThermostatUserInterface=0x0204, PumpConfig=0x0205
            0x0201 => Ok(Self::Thermostat),
            0x0202 => Ok(Self::FanControl),
            0x0203 => Ok(Self::Heating),
            0x0204 => Ok(Self::ThermostatUserInterfaceConfiguration),
            0x0205 => Ok(Self::PumpConfigurationAndControl),

            // ── Covers ─────────────────────────────────────────────────────
            0x0102 => Ok(Self::WindowCovering),
            0x0103 => Ok(Self::BarrierControl),

            // ── Color / Lighting ──────────────────────────────────────────
            0x0300 => Ok(Self::ColorControl),
            0x0301 => Ok(Self::BallastConfiguration),
            0x0302 => Ok(Self::SiliconLabs),

            // ── Measurement ───────────────────────────────────────────────
            0x0400 => Ok(Self::TemperatureMeasurement),
            0x0402 => Ok(Self::TemperatureMeasurement),
            0x0403 => Ok(Self::PressureMeasurement),
            0x0404 => Ok(Self::FlowMeasurement),
            0x0405 => Ok(Self::RelativeHumidityMeasurement),
            0x0406 => Ok(Self::OccupancySensing),

            // ── Security ───────────────────────────────────────────────────
            0x0101 => Ok(Self::DoorLock),
            0x0500 => Ok(Self::IasZone),
            0x0501 => Ok(Self::IasAce),
            0x0502 => Ok(Self::SmokeCoAlarm),
            0x0508 => Ok(Self::BrightnessControl),
            0x0509 => Ok(Self::AmbientLighting),
            0x0212 => Ok(Self::Doorbell),

            // ── Electrical / Power ─────────────────────────────────────────
            0x0B04 => Ok(Self::ElectricalMeasurement),
            0x0B05 => Ok(Self::ElectricalEnergyMeasurement),
            0x0B06 => Ok(Self::ElectricalPowerMeasurement),
            0x0B07 => Ok(Self::ElectricalPowerRequest),
            0x0B08 => Ok(Self::PowerTopology),

            // ── Thread ─────────────────────────────────────────────────────
            // Note: Thread Network Diagnostics cluster IDs (0x0035-0x003D) are
            // omitted here because they conflict with Switch (0x003B) and
            // Proxy clusters (0x003C/0x003D) which are more common. Thread
            // diagnostics can be resolved by reading the device's descriptor.
            0x0071 => Ok(Self::ThreadTelemetry),
            0x0072 => Ok(Self::ThreadTelemetryBridged),
            0x0073 => Ok(Self::ThreadBorderRouterTelemetry),
            0x0074 => Ok(Self::ThreadBorderRouterConfiguration),
            0x0075 => Ok(Self::ThreadDiagnostics),
            0x0076 => Ok(Self::ThreadBorderRouterDiagnostics),
            0x0077 => Ok(Self::ThreadCapabilityDiscovery),
            0x0078 => Ok(Self::ThreadCapabilityDiscoveryConfig),

            _ => Err(id),
        }
    }
}

impl MatterCluster {
    /// A3Net capabilities this cluster exposes.
    pub fn capabilities(self) -> &'static [DeviceCapability] {
        match self {
            Self::OnOff => &[DeviceCapability::OnOff],
            Self::LevelControl => &[
                DeviceCapability::Brightness,
                DeviceCapability::Position,
                DeviceCapability::FanSpeed,
            ],
            Self::ColorControl => &[
                DeviceCapability::Color,
                DeviceCapability::ColorTemperature,
                DeviceCapability::Brightness,
            ],
            Self::TemperatureMeasurement => &[DeviceCapability::Temperature],
            Self::RelativeHumidityMeasurement => &[DeviceCapability::Humidity],
            Self::HumidityControl => &[DeviceCapability::Humidity],
            Self::IlluminanceMeasurement => &[DeviceCapability::Illuminance],
            Self::DoorLock => &[DeviceCapability::Lock],
            Self::WindowCovering => &[DeviceCapability::Position],
            Self::Thermostat => &[
                DeviceCapability::Temperature,
                DeviceCapability::Mode,
                DeviceCapability::FanSpeed,
            ],
            Self::FanControl => &[DeviceCapability::FanSpeed, DeviceCapability::OnOff],
            Self::Heating => &[DeviceCapability::Temperature, DeviceCapability::Mode],
            Self::OccupancySensing => &[DeviceCapability::Motion],
            Self::IasZone => &[
                DeviceCapability::Motion,
                DeviceCapability::Smoke,
                DeviceCapability::CO,
                DeviceCapability::Vibration,
                DeviceCapability::Tamper,
                DeviceCapability::WaterLeak,
            ],
            Self::SmokeCoAlarm => &[DeviceCapability::Smoke, DeviceCapability::CO],
            Self::IasAce => &[DeviceCapability::Smoke, DeviceCapability::CO, DeviceCapability::Motion],
            Self::ElectricalMeasurement => &[
                DeviceCapability::Power,
                DeviceCapability::Voltage,
                DeviceCapability::Current,
                DeviceCapability::Energy,
                DeviceCapability::PowerFactor,
            ],
            Self::ElectricalPowerMeasurement => &[
                DeviceCapability::Power,
                DeviceCapability::Voltage,
                DeviceCapability::Current,
            ],
            Self::ElectricalEnergyMeasurement => &[DeviceCapability::Energy],
            Self::PowerSource => &[
                DeviceCapability::Battery,
                DeviceCapability::BatteryLevel,
                DeviceCapability::Charging,
            ],
            Self::PM25Measurement => &[DeviceCapability::PM25],
            Self::CarbonDioxideMeasurement => &[DeviceCapability::CO2],
            Self::FormaldehydeMeasurement => &[DeviceCapability::Formaldehyde],
            Self::BooleanState => &[DeviceCapability::OnOff, DeviceCapability::Contact],
            Self::ContactSensor => &[DeviceCapability::Contact],
            Self::AirQuality => &[DeviceCapability::AirQuality],
            Self::Irrigation => &[DeviceCapability::Irrigation],
            Self::ModeSelect => &[DeviceCapability::Mode, DeviceCapability::Scene],
            Self::Switch => &[DeviceCapability::OnOff],
            Self::Actions => &[DeviceCapability::Scene],
            Self::PowerTopology => &[DeviceCapability::Power],
            Self::DeviceTemperature => &[DeviceCapability::Temperature],
            Self::DeviceEnergyManagement => &[DeviceCapability::Power, DeviceCapability::Energy],
            Self::Doorbell => &[DeviceCapability::Motion],
            Self::BrightnessControl => &[DeviceCapability::Brightness],
            _ => &[],
        }
    }

    /// Human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            Self::OnOff => "On/Off",
            Self::LevelControl => "Level Control",
            Self::ColorControl => "Color Control",
            Self::TemperatureMeasurement => "Temperature Measurement",
            Self::RelativeHumidityMeasurement => "Relative Humidity Measurement",
            Self::HumidityControl => "Humidity Control",
            Self::IlluminanceMeasurement => "Illuminance Measurement",
            Self::DoorLock => "Door Lock",
            Self::WindowCovering => "Window Covering",
            Self::Thermostat => "Thermostat",
            Self::FanControl => "Fan Control",
            Self::OccupancySensing => "Occupancy Sensing",
            Self::IasZone => "IAS Zone",
            Self::SmokeCoAlarm => "Smoke/CO Alarm",
            Self::ElectricalMeasurement => "Electrical Measurement",
            Self::PowerSource => "Power Source",
            Self::PM25Measurement => "PM2.5 Measurement",
            Self::CarbonDioxideMeasurement => "CO₂ Measurement",
            Self::FormaldehydeMeasurement => "Formaldehyde Measurement",
            Self::ContactSensor => "Contact Sensor",
            Self::AirQuality => "Air Quality",
            Self::Irrigation => "Irrigation",
            Self::ModeSelect => "Mode Select",
            Self::Switch => "Switch",
            Self::Actions => "Actions",
            _ => "Unknown Cluster",
        }
    }
}

/// Cluster spec describing a commissioned Matter device's cluster footprint.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MatterClusterSpec {
    pub node_id: u64,
    pub label: Option<String>,
    pub vendor_id: u16,
    pub product_id: u16,
    /// Cluster IDs this device exposes (read from the Descriptor cluster).
    pub cluster_ids: Vec<u32>,
    /// Device type IDs (from the Descriptor cluster's deviceTypeList).
    pub device_type_ids: Vec<u16>,
}

impl MatterClusterSpec {
    /// Convert cluster IDs → A3Net capabilities.
    pub fn to_capabilities(&self) -> HashSet<DeviceCapability> {
        let mut caps = HashSet::new();
        for &id in &self.cluster_ids {
            if let Ok(cluster) = MatterCluster::try_from(id) {
                caps.extend(cluster.capabilities().iter().cloned());
            }
        }
        caps
    }

    /// Primary device type name for display.
    pub fn primary_device_type(&self) -> &'static str {
        const DTYPES: &[(u16, &str)] = &[
            (0x0100, "Dimmable Light"),
            (0x0101, "Color Bulb"),
            (0x0102, "Extended Color Light"),
            (0x0103, "Color Temperature Light"),
            (0x0104, "Dimmable Switch"),
            (0x0105, "Light Sensor"),
            (0x0106, "Occupancy Sensor"),
            (0x0107, "Temperature Sensor"),
            (0x010B, "On/Off Light"),
            (0x010C, "Dimmable Plug-In Unit"),
            (0x010D, "On/Off Plug-In Unit"),
            (0x010E, "Door Lock"),
            (0x010F, "Door Lock Controller"),
            (0x0112, "Water Leak Sensor"),
            (0x0113, "Smoke CO Sensor"),
            (0x0115, "Fan"),
            (0x0116, "Thermostat"),
            (0x0117, "Air Purifier"),
            (0x0118, "Air Quality Sensor"),
            (0x0200, "Window Covering"),
            (0x0201, "Window Covering Controller"),
            (0x0202, "Heating/Cooling Unit"),
            (0x0302, "Speaker"),
            (0x0400, "Mode Select"),
        ];
        DTYPES
            .iter()
            .find(|(id, _)| self.device_type_ids.contains(id))
            .map(|(_, name)| *name)
            .unwrap_or("Matter Device")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onoff_roundtrip() {
        assert_eq!(MatterCluster::try_from(0x0006u32), Ok(MatterCluster::OnOff));
    }

    #[test]
    fn temperature_measurement() {
        assert_eq!(
            MatterCluster::try_from(0x0402u32),
            Ok(MatterCluster::TemperatureMeasurement)
        );
    }

    #[test]
    fn door_lock() {
        assert_eq!(MatterCluster::try_from(0x0101u32), Ok(MatterCluster::DoorLock));
    }

    #[test]
    fn unknown_returns_err() {
        assert!(MatterCluster::try_from(0xDEADu32).is_err());
    }

    #[test]
    fn light_spec_caps() {
        let spec = MatterClusterSpec {
            node_id: 1, label: None, vendor_id: 0, product_id: 0,
            cluster_ids: vec![0x0006, 0x0008],
            device_type_ids: vec![0x0100],
        };
        let caps = spec.to_capabilities();
        assert!(caps.contains(&DeviceCapability::OnOff));
        assert!(caps.contains(&DeviceCapability::Brightness));
    }

    #[test]
    fn door_lock_spec_caps() {
        let spec = MatterClusterSpec {
            node_id: 2, label: None, vendor_id: 0, product_id: 0,
            cluster_ids: vec![0x0101],
            device_type_ids: vec![0x010E],
        };
        assert!(spec.to_capabilities().contains(&DeviceCapability::Lock));
    }

    #[test]
    fn smoke_sensor_spec_caps() {
        let spec = MatterClusterSpec {
            node_id: 3, label: None, vendor_id: 0, product_id: 0,
            cluster_ids: vec![0x0500], // IAS Zone
            device_type_ids: vec![0x0113],
        };
        let caps = spec.to_capabilities();
        assert!(caps.contains(&DeviceCapability::Smoke));
        assert!(caps.contains(&DeviceCapability::CO));
    }
}
