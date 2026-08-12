//! RPC result types for the unified IPFS-compatible API.

use serde::{Deserialize, Serialize};

/// Base result type alias.
pub type RpcResult = Result<serde_json::Value, RpcError>;

/// RPC error type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub message: String,
    pub code: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
}

impl RpcError {
    pub fn new(message: impl Into<String>, code: u32) -> Self {
        Self {
            message: message.into(),
            code,
            r#type: Some("error".to_string()),
        }
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::new(msg, 1)
    }

    pub fn invalid_input(msg: impl Into<String>) -> Self {
        Self::new(msg, 2)
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self::new(msg, 3)
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code={})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

/// DAG operation result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagResult {
    #[serde(rename = "Cid")]
    pub cid: String,
    #[serde(rename = "Size")]
    pub size: u64,
}

/// DAG resolve result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagResolveResult {
    #[serde(rename = "Cid")]
    pub cid: String,
    #[serde(rename = "RemPath", skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Block operation result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockResult {
    #[serde(rename = "Key")]
    pub key: String,
    #[serde(rename = "Size")]
    pub size: u64,
}

/// Block stat result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockStatResult {
    #[serde(rename = "Key")]
    pub key: String,
    #[serde(rename = "Size")]
    pub size: u64,
    #[serde(rename = "Cid")]
    pub cid: String,
}

/// Block rm result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockRmResult {
    #[serde(rename = "Hash")]
    pub hash: String,
    #[serde(rename = "Removed")]
    pub removed: bool,
}

/// Pin add result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinAddResult {
    #[serde(rename = "Pins")]
    pub pins: Vec<String>,
}

/// Pin rm result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinRmResult {
    #[serde(rename = "Pins")]
    pub pins: Vec<String>,
}

/// Pin list result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinLsResult {
    #[serde(rename = "Keys")]
    pub keys: std::collections::HashMap<String, PinInfoResult>,
}

/// Pin info in list result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinInfoResult {
    #[serde(rename = "Type")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin_type: Option<String>,
    #[serde(rename = "NumLinks", skip_serializing_if = "Option::is_none")]
    pub num_links: Option<u32>,
    #[serde(rename = "BlockSize", skip_serializing_if = "Option::is_none")]
    pub block_size: Option<u32>,
    #[serde(rename = "Size", skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(rename = "WithPathFlag", skip_serializing_if = "Option::is_none")]
    pub with_path_flag: Option<bool>,
}

/// Pin verify result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinVerifyResult {
    #[serde(rename = "Cid")]
    pub cid: String,
    #[serde(rename = "PinStatus")]
    pub status: String,
    #[serde(rename = "Pinned")]
    pub pinned: bool,
}

/// DHT find providers result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DhtFindProvsResult {
    #[serde(rename = "Cid")]
    pub cid: String,
    #[serde(rename = "Providers")]
    pub providers: Vec<ProviderInfo>,
}

/// Provider information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "Addrs")]
    pub addrs: Vec<String>,
}

/// DHT provide result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DhtProvideResult {
    #[serde(rename = "Cid")]
    pub cid: String,
}

/// Name publish result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamePublishResult {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Value")]
    pub value: String,
}

/// Name resolve result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NameResolveResult {
    #[serde(rename = "Path")]
    pub path: String,
}

/// GC result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcResult {
    #[serde(rename = "KeysRemoved")]
    pub keys_removed: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

/// Node ID result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeIdResult {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "PublicKey")]
    pub public_key: String,
    #[serde(rename = "Addresses")]
    pub addresses: Vec<String>,
    #[serde(rename = "AgentVersion")]
    pub agent_version: String,
    #[serde(rename = "ProtocolVersion")]
    pub protocol_version: String,
}

/// Node version result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeVersionResult {
    #[serde(rename = "Version")]
    pub version: String,
    #[serde(rename = "Commit")]
    pub commit: String,
    #[serde(rename = "Repo")]
    pub repo: String,
    #[serde(rename = "System")]
    pub system: String,
    #[serde(rename = "Golang")]
    pub golang: String,
}
