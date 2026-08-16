//! In-memory device registry with optional persistence

use crate::device::Device;
use crate::error::{Result, SmartHomeError};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::RwLock;
use tracing::{debug, info};

/// Thread-safe in-memory registry with simple JSON persistence
pub struct DeviceRegistry {
    devices: Arc<RwLock<HashMap<String, Device>>>,
    data_dir: String,
}

impl DeviceRegistry {
    pub fn new(data_dir: impl Into<String>) -> Self {
        Self {
            devices: Arc::new(RwLock::new(HashMap::new())),
            data_dir: data_dir.into(),
        }
    }

    /// Load devices from disk
    pub async fn load(&self) -> Result<()> {
        let path = format!("{}/devices.json", self.data_dir);
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => {
                let devices: Vec<Device> = serde_json::from_str(&content)
                    .map_err(|e| SmartHomeError::Storage(format!("parse devices.json: {}", e)))?;
                let mut guard = self.devices.write().await;
                for d in devices {
                    guard.insert(d.device_id.clone(), d);
                }
                info!("Loaded {} devices from {}", guard.len(), path);
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                debug!("No devices.json found, starting fresh");
                Ok(())
            }
            Err(e) => Err(SmartHomeError::Storage(format!("read devices.json: {}", e))),
        }
    }

    /// Persist devices to disk
    pub async fn save(&self) -> Result<()> {
        let guard = self.devices.read().await;
        let devices: Vec<&Device> = guard.values().collect();
        let content = serde_json::to_string_pretty(&devices)?;
        let path = format!("{}/devices.json", self.data_dir);
        tokio::fs::create_dir_all(&self.data_dir).await
            .map_err(|e| SmartHomeError::Storage(format!("create data dir: {}", e)))?;
        tokio::fs::write(&path, content).await
            .map_err(|e| SmartHomeError::Storage(format!("write devices.json: {}", e)))?;
        debug!("Saved {} devices to {}", devices.len(), path);
        Ok(())
    }

    /// Add or replace a device, then persist to disk
    pub async fn add(&self, device: Device) -> Result<()> {
        let id = device.device_id.clone();
        self.devices.write().await.insert(id.clone(), device);
        debug!("Registry: added/updated device {}", id);
        self.save().await
    }

    /// Get a device by ID
    pub async fn get(&self, device_id: &str) -> Option<Device> {
        self.devices.read().await.get(device_id).cloned()
    }

    /// Remove a device, then persist to disk
    pub async fn remove(&self, device_id: &str) -> Result<()> {
        let removed = self.devices.write().await.remove(device_id).is_some();
        if !removed {
            return Err(SmartHomeError::DeviceNotFound(device_id.to_string()));
        }
        debug!("Registry: removed device {}", device_id);
        self.save().await
    }

    /// List all devices
    pub async fn list_all(&self) -> Vec<Device> {
        self.devices.read().await.values().cloned().collect()
    }

    /// Mark a device as seen now and online. Returns `true` if the
    /// device transitioned from offline to online (so callers can
    /// decide whether to broadcast a `DeviceOnline` event).
    pub async fn mark_online(&self, device_id: &str) -> bool {
        let mut guard = self.devices.write().await;
        if let Some(d) = guard.get_mut(device_id) {
            let was_offline = !d.online;
            d.online = true;
            d.last_seen = Some(SystemTime::now());
            was_offline
        } else {
            false
        }
    }

    /// Mark a device as offline. Returns `true` if the device was
    /// previously online.
    pub async fn mark_offline(&self, device_id: &str) -> bool {
        let mut guard = self.devices.write().await;
        if let Some(d) = guard.get_mut(device_id) {
            let was_online = d.online;
            d.online = false;
            was_online
        } else {
            false
        }
    }

    /// Update a device's state properties
    pub async fn update_state(&self, device_id: &str, key: impl Into<String>, value: serde_json::Value) {
        let mut guard = self.devices.write().await;
        if let Some(d) = guard.get_mut(device_id) {
            d.state.set(key, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::DeviceType;

    fn temp_dir() -> String {
        let dir = std::env::temp_dir().join(format!("a3net-smarthome-test-{}", uuid_like()));
        dir.to_string_lossy().into_owned()
    }

    fn uuid_like() -> String {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        (0..16).map(|_| format!("{:x}", rng.gen_range(0..16u8))).collect()
    }

    #[tokio::test]
    async fn add_and_get_roundtrip() {
        let reg = DeviceRegistry::new(temp_dir());
        let dev = Device::new("dev-1", "Lamp", DeviceType::Xiaomi);
        reg.add(dev.clone()).await.unwrap();

        let got = reg.get("dev-1").await.unwrap();
        assert_eq!(got.device_id, "dev-1");
        assert_eq!(got.name, "Lamp");
    }

    #[tokio::test]
    async fn remove_missing_device_errors() {
        let reg = DeviceRegistry::new(temp_dir());
        let err = reg.remove("nope").await.unwrap_err();
        assert!(matches!(err, SmartHomeError::DeviceNotFound(_)));
    }

    #[tokio::test]
    async fn mark_online_offline_transitions() {
        let reg = DeviceRegistry::new(temp_dir());
        let dev = Device::new("dev-1", "Lamp", DeviceType::Xiaomi);
        reg.add(dev).await.unwrap();

        // Device starts offline (Device::new defaults online=false).
        assert!(reg.mark_online("dev-1").await, "should transition offline -> online");
        assert!(!reg.mark_online("dev-1").await, "already online, no transition");

        assert!(reg.mark_offline("dev-1").await, "should transition online -> offline");
        assert!(!reg.mark_offline("dev-1").await, "already offline, no transition");
    }

    #[tokio::test]
    async fn mark_online_offline_on_unknown_device_is_noop() {
        let reg = DeviceRegistry::new(temp_dir());
        assert!(!reg.mark_online("ghost").await);
        assert!(!reg.mark_offline("ghost").await);
    }

    #[tokio::test]
    async fn persists_across_reload() {
        let dir = temp_dir();
        {
            let reg = DeviceRegistry::new(dir.clone());
            reg.add(Device::new("dev-1", "Lamp", DeviceType::Xiaomi)).await.unwrap();
            reg.add(Device::new("dev-2", "Fan", DeviceType::Xiaomi)).await.unwrap();
        }

        let reg2 = DeviceRegistry::new(dir.clone());
        reg2.load().await.unwrap();
        let mut all = reg2.list_all().await;
        all.sort_by(|a, b| a.device_id.cmp(&b.device_id));
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].device_id, "dev-1");
        assert_eq!(all[1].device_id, "dev-2");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn remove_persists_deletion_across_reload() {
        let dir = temp_dir();
        {
            let reg = DeviceRegistry::new(dir.clone());
            reg.add(Device::new("dev-1", "Lamp", DeviceType::Xiaomi)).await.unwrap();
            reg.remove("dev-1").await.unwrap();
        }

        let reg2 = DeviceRegistry::new(dir.clone());
        reg2.load().await.unwrap();
        assert!(reg2.list_all().await.is_empty());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn update_state_sets_key() {
        let reg = DeviceRegistry::new(temp_dir());
        reg.add(Device::new("dev-1", "Lamp", DeviceType::Xiaomi)).await.unwrap();
        reg.update_state("dev-1", "2.1", serde_json::json!(true)).await;

        let dev = reg.get("dev-1").await.unwrap();
        assert_eq!(dev.state.get("2.1"), Some(&serde_json::json!(true)));
    }

    #[tokio::test]
    async fn load_missing_file_is_ok() {
        let reg = DeviceRegistry::new(temp_dir());
        reg.load().await.unwrap();
        assert!(reg.list_all().await.is_empty());
    }
}
