//! High-level Matter controller client.
//!
//! Wraps the `matter-controller` crate to commission and control Matter devices.
//! Both Xiaomi MIoT and Matter devices feed into the same `HubEvent` broadcast
//! channel, so automations are protocol-agnostic.

use crate::device::{Device, DeviceType};
use crate::error::{Result, SmartHomeError};
use crate::matter::error::MatterError;
use crate::matter::types::MatterClusterSpec;
use std::collections::HashMap as StdHashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use matter_controller::{
    AttributePath, AttestationTrust, CommandPath, FabricConfig, FileStore,
    MatterController, MatterTime, NodeInfo, ReadPath, Subscription,
};
use matter_codec::Tag;

/// Configuration for the Matter controller.
#[derive(Debug, Clone)]
pub struct MatterHubConfig {
    pub store_path: String,
    pub attestation_trust: MatterAttestationTrust,
    /// CSA-assigned vendor ID (e.g. 0xFFF1 for test devices).
    pub admin_vendor_id: u16,
    /// `if_nametoindex` of the LAN-facing IPv6 interface for group multicast.
    /// `None` = kernel default.
    pub multicast_ifindex: Option<u32>,
}

impl MatterHubConfig {
    /// Development / test configuration (only works with CSA test devices).
    pub fn development() -> Self {
        Self {
            store_path: "data/smarthome/matter-store.json".into(),
            attestation_trust: MatterAttestationTrust::ExampleRoots,
            admin_vendor_id: 0xFFF1,
            multicast_ifindex: None,
        }
    }

    /// Production configuration.
    pub fn production(store_path: String, paa_dir: String, cd_signer_dir: String) -> Self {
        Self {
            store_path,
            attestation_trust: MatterAttestationTrust::FromDirs { paa_dir, cd_signer_dir },
            admin_vendor_id: 0xFFF1,
            multicast_ifindex: None,
        }
    }
}

/// Which trust anchors to use for device attestation verification.
#[derive(Debug, Clone)]
pub enum MatterAttestationTrust {
    /// CSA example/test roots — works with certified test devices.
    ExampleRoots,
    /// Load PAA and CD signing root certificates from directories.
    FromDirs { paa_dir: String, cd_signer_dir: String },
}

/// A commissioned Matter device node.
#[derive(Debug, Clone)]
pub struct MatterNode {
    pub node_id: u64,
    pub label: Option<String>,
    pub vendor_id: u16,
    pub product_id: u16,
    pub spec: MatterClusterSpec,
}

impl MatterNode {
    /// Convert to an ADNet [`Device`].
    pub fn into_device(self) -> Device {
        use crate::device::DeviceCapability;
        let capabilities: Vec<DeviceCapability> = self
            .spec
            .to_capabilities()
            .into_iter()
            .collect();

        let name = self
            .label
            .unwrap_or_else(|| self.spec.primary_device_type().to_string());

        let mut dev = Device::new(
            format!("matter:{}", self.node_id),
            name,
            DeviceType::Matter,
        );
        dev.model = Some(self.spec.primary_device_type().to_string());
        dev.capabilities = capabilities;
        dev.online = true;
        dev.last_seen = Some(SystemTime::now());
        dev
    }
}

/// The Matter controller client.
pub struct MatterClient {
    controller: Arc<MatterController>,
    /// Send handles for active subscriptions, keyed by node_id.
    #[allow(dead_code)]
    sub_senders: std::sync::Mutex<StdHashMap<u64, tokio::sync::broadcast::Sender<crate::hub::HubEvent>>>,
}

unsafe impl Send for MatterClient {}
unsafe impl Sync for MatterClient {}

impl MatterClient {
    /// Build a new controller. Call `ensure_fabric()` before commissioning.
    pub async fn new(config: MatterHubConfig) -> Result<Self> {
        let store = Arc::new(FileStore::new(&config.store_path));
        let trust = Self::build_trust(&config.attestation_trust)?;

        let mut builder = MatterController::builder(store)
            .attestation_trust(trust)
            .admin_vendor_id(config.admin_vendor_id);

        if let Some(idx) = config.multicast_ifindex {
            builder = builder.multicast_interface(idx);
        }

        let controller = builder.build().await.map_err(|e| {
            SmartHomeError::Protocol(format!("build Matter controller: {}", e))
        })?;

        Ok(Self {
            controller: Arc::new(controller),
            sub_senders: std::sync::Mutex::new(StdHashMap::new()),
        })
    }

    fn build_trust(trust: &MatterAttestationTrust) -> Result<AttestationTrust> {
        match trust {
            MatterAttestationTrust::ExampleRoots => Ok(AttestationTrust::example_device_roots()),
            MatterAttestationTrust::FromDirs { paa_dir, cd_signer_dir } => {
                AttestationTrust::from_dirs(Path::new(paa_dir), Path::new(cd_signer_dir))
                    .map_err(|e| SmartHomeError::Protocol(format!("load attestation roots: {}", e)))
            }
        }
    }

    /// Create or load the Matter fabric. Idempotent — safe to call on restart.
    /// Returns the fabric ID.
    pub async fn ensure_fabric(&self, fabric_id: u64) -> Result<u64> {
        let cfg = FabricConfig::new(
            fabric_id,
            1, // rcac_id
            1, // commissioner_node_id
            (MatterTime::from_unix_secs(0), MatterTime::NO_EXPIRY),
        );
        let id = self.controller.create_fabric(cfg).await.map_err(MatterError::from)?;
        tracing::info!("Matter fabric {} ready", id);
        Ok(id)
    }

    /// Commission a device from a QR code (`MT:…`) or manual pairing code.
    pub async fn commission(
        &self,
        payload: &str,
        label: Option<String>,
    ) -> Result<MatterNode> {
        let info = self.controller
            .commission(payload, label.clone())
            .await
            .map_err(MatterError::from)?;
        self.sync_node_info(info).await
    }

    /// Reload all commissioned nodes from the persistent store.
    pub async fn sync_nodes(&self) -> Result<Vec<MatterNode>> {
        let nodes: Vec<NodeInfo> = self.controller.nodes().await.map_err(MatterError::from)?;
        let mut result = Vec::new();
        for info in nodes {
            result.push(self.sync_node_info(info).await?);
        }
        Ok(result)
    }

    async fn sync_node_info(&self, info: NodeInfo) -> Result<MatterNode> {
        let spec = self.describe_node(info.node_id).await?;
        Ok(MatterNode {
            node_id: info.node_id,
            label: info.label,
            vendor_id: info.vendor_id.unwrap_or(0),
            product_id: info.product_id.unwrap_or(0),
            spec,
        })
    }

    /// Introspect a commissioned device — read its Descriptor cluster to
    /// discover all exposed clusters and device types.
    pub async fn describe_node(&self, node_id: u64) -> Result<MatterClusterSpec> {
        let node = self.controller.node(node_id);

        // Descriptor cluster (0x0029) on endpoint 0 gives serverList + deviceTypeList
        let path = ReadPath::cluster(0, 0x0029);
        let report = node.read(&[path]).await.map_err(MatterError::from)?;

        let mut cluster_ids = Vec::new();
        let mut device_type_ids = Vec::new();

        for (_path, value) in report {
            if let matter_controller::Value::Structure(fields) = value {
                for (tag, field_val) in fields {
                    match tag {
                        Tag::Context(2) => {
                            // serverList — list of cluster IDs
                            if let matter_controller::Value::List(items) = field_val {
                                for (tag, item) in items {
                                    if matches!(tag, Tag::Context(_)) {
                                        if let matter_controller::Value::Uint(id) = item {
                                            cluster_ids.push(id as u32);
                                        }
                                    }
                                }
                            }
                        }
                        Tag::Context(4) => {
                            // deviceTypeList — list of { deviceType (tag 0), revision (tag 1) }
                            if let matter_controller::Value::List(items) = field_val {
                                for (tag, item) in items {
                                    if matches!(tag, Tag::Context(_)) {
                                        if let matter_controller::Value::Structure(dt_fields) = item {
                                            for (dt_tag, dt_val) in dt_fields {
                                                if matches!(dt_tag, Tag::Context(0)) {
                                                    if let matter_controller::Value::Uint(id) = dt_val {
                                                        device_type_ids.push(id as u16);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(MatterClusterSpec {
            node_id,
            label: None,
            vendor_id: 0,
            product_id: 0,
            cluster_ids,
            device_type_ids,
        })
    }

    /// Remove a commissioned device locally (no device contact).
    pub async fn forget_node(&self, node_id: u64) -> Result<()> {
        self.controller.forget_node(node_id).await.map_err(MatterError::from)?;
        self.sub_senders.lock().unwrap().remove(&node_id);
        Ok(())
    }

    /// List all commissioned node IDs.
    pub async fn list_nodes(&self) -> Result<Vec<u64>> {
        let nodes: Vec<NodeInfo> = self.controller.nodes().await.map_err(MatterError::from)?;
        Ok(nodes.into_iter().map(|n| n.node_id).collect())
    }

    /// Subscribe to attribute changes on a device.
    ///
    /// `min_interval_s` / `max_interval_s` — subscription min/max interval in seconds.
    /// Returns a `Subscription` — call `.next().await` to receive reports.
    /// The stream auto-resubscribes on session loss or device reboot.
    pub async fn subscribe(
        &self,
        node_id: u64,
        endpoint: u16,
        cluster_id: u32,
        attribute_id: u32,
        min_interval_s: u16,
        max_interval_s: u16,
    ) -> Result<Subscription> {
        let node = self.controller.node(node_id);
        let path = ReadPath::concrete(endpoint, cluster_id, attribute_id);
        let sub = node.subscribe(&[path], &[], min_interval_s, max_interval_s).await
            .map_err(MatterError::from)?;
        Ok(sub)
    }

    /// Read a single attribute from a device.
    pub async fn read_attribute(
        &self,
        node_id: u64,
        endpoint: u16,
        cluster_id: u32,
        attribute_id: u32,
    ) -> Result<matter_controller::Value> {
        let node = self.controller.node(node_id);
        let path = ReadPath::concrete(endpoint, cluster_id, attribute_id);
        let report = node.read(&[path]).await.map_err(MatterError::from)?;
        for (_, value) in report {
            return Ok(value);
        }
        Err(SmartHomeError::Protocol("attribute not found".into()))
    }

    /// Write a single attribute on a device.
    pub async fn write_attribute(
        &self,
        node_id: u64,
        endpoint: u16,
        cluster_id: u32,
        attribute_id: u32,
        value: matter_controller::Value,
    ) -> Result<()> {
        let node = self.controller.node(node_id);
        let path = AttributePath { endpoint, cluster: cluster_id, attribute: attribute_id };
        node.write(&[(path, value)]).await.map_err(MatterError::from)?;
        Ok(())
    }

    /// Invoke a command on a device.
    pub async fn invoke_command(
        &self,
        node_id: u64,
        endpoint: u16,
        cluster_id: u32,
        command_id: u32,
        fields: matter_controller::Value,
    ) -> Result<matter_controller::Value> {
        let node = self.controller.node(node_id);
        let path = CommandPath { endpoint, cluster: cluster_id, command: command_id };
        let result = node.invoke(path, fields).await.map_err(MatterError::from)?;
        match result {
            matter_controller::InvokeResult::Data { fields, .. } => Ok(fields),
            matter_controller::InvokeResult::Status(status) => {
                Err(SmartHomeError::Protocol(format!("command failed: {:?}", status)))
            }
            _ => Err(SmartHomeError::Protocol("unexpected invoke result".into())),
        }
    }
}

/// Convert a Matter TLV `Value` into a `serde_json::Value`.
pub fn decode_matter_value(v: &matter_controller::Value) -> serde_json::Value {
    use matter_controller::Value as MV;
    match v {
        MV::Bool(b) => serde_json::json!({ "type": "bool", "value": b }),
        MV::Uint(u) => serde_json::json!({ "type": "uint", "value": u }),
        MV::Int(s) => serde_json::json!({ "type": "sint", "value": s }),
        MV::Float(f) => serde_json::json!({ "type": "float", "value": f }),
        MV::Double(d) => serde_json::json!({ "type": "double", "value": d }),
        MV::Utf8(s) => serde_json::json!({ "type": "string", "value": s }),
        MV::Bytes(b) => serde_json::json!({ "type": "bytes", "value": hex::encode(b) }),
        MV::Structure(fields) => {
            let mut map = serde_json::Map::new();
            for (tag, val) in fields {
                let key = format!("{:?}", tag);
                map.insert(key, decode_matter_value(val));
            }
            serde_json::json!({ "type": "struct", "fields": map })
        }
        MV::Array(items) => serde_json::json!({
            "type": "array",
            "items": items.iter().map(decode_matter_value).collect::<Vec<_>>()
        }),
        MV::List(items) => serde_json::json!({
            "type": "list",
            "items": items.iter()
                .map(|(_, val)| decode_matter_value(val))
                .collect::<Vec<_>>()
        }),
        MV::Null => serde_json::Value::Null,
        _ => serde_json::json!({ "type": "unknown" }),
    }
}
