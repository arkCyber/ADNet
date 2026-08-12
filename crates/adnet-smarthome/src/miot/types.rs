//! MIoT data types
//! 
//! Type definitions for the Xiaomi MIoT protocol

use serde::{Deserialize, Serialize};

/// A MIoT service property reference (siid.piid)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Property {
    pub siid: u32,
    pub piid: u32,
}

/// A property with a value
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyValue {
    pub siid: u32,
    pub piid: u32,
    pub value: serde_json::Value,
}

/// A MIoT action reference (siid.aiid)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub siid: u32,
    pub aiid: u32,
    #[serde(rename = "in", default)]
    pub input: Vec<serde_json::Value>,
}

/// A MIoT device returned by the cloud API. All identifying fields
/// use `#[serde(default)]` so a malformed or incomplete cloud response
/// yields an empty Device rather than a parse error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiotDevice {
    #[serde(default)]
    pub did: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub localip: String,
    #[serde(default)]
    pub mac: String,
    #[serde(default)]
    pub online: bool,
    #[serde(default)]
    pub token: String,
}

/// Device spec from MIoT Spec catalog
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiotDeviceSpec {
    pub model: String,
    pub description: String,
    #[serde(default)]
    pub services: Vec<MiotServiceSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiotServiceSpec {
    pub siid: u32,
    pub description: String,
    #[serde(default)]
    pub properties: Vec<MiotPropertySpec>,
    #[serde(default)]
    pub actions: Vec<MiotActionSpec>,
    #[serde(default)]
    pub events: Vec<MiotEventSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiotPropertySpec {
    pub piid: u32,
    pub description: String,
    pub format: String,
    #[serde(default)]
    pub access: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiotActionSpec {
    pub aiid: u32,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiotEventSpec {
    pub eiid: u32,
    pub description: String,
}

/// Request body for device RPC
#[derive(Debug, Serialize)]
pub struct MiotDeviceRequest {
    pub did: String,
    pub method: String,
    pub params: Vec<serde_json::Value>,
}

/// Request body for property get
#[derive(Debug, Serialize)]
pub struct MiotPropertyRequest {
    pub did: String,
    pub params: Vec<Property>,
}

/// Request body for property set
#[derive(Debug, Serialize)]
pub struct MiotPropertySetRequest {
    pub did: String,
    pub params: Vec<PropertyValue>,
}

/// Request body for action invoke
#[derive(Debug, Serialize)]
pub struct MiotActionRequest {
    pub did: String,
    pub action: Action,
}
