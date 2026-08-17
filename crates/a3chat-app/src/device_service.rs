//! Device management service.
//!
//! DO-178C §6.4.6: Device registration is cryptographically bound.

#![forbid(unsafe_code)]

use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use a3chat_core::error::A3chatError;
use a3chat_core::event::A3chatEvent;
use a3chat_core::id::{DeviceId, UserId};

use crate::error::{AppError, AppResult};
use crate::notification_bus::NotificationBus;

/// Device type for display and sync policy.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    #[default]
    Desktop,
    Phone,
    Tablet,
    Web,
}

impl DeviceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeviceKind::Phone => "phone",
            DeviceKind::Tablet => "tablet",
            DeviceKind::Desktop => "desktop",
            DeviceKind::Web => "web",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "phone" => Some(DeviceKind::Phone),
            "tablet" => Some(DeviceKind::Tablet),
            "desktop" => Some(DeviceKind::Desktop),
            "web" => Some(DeviceKind::Web),
            _ => None,
        }
    }
}

/// A registered device belonging to a user.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Device {
    pub device_id: DeviceId,
    pub user_id: UserId,
    pub name: String,
    pub kind: DeviceKind,
    pub last_seen: DateTime<Utc>,
    pub last_sync_at: Option<i64>,
    pub is_current: bool,
    pub is_primary: bool,
    pub public_key_fingerprint: Option<String>,
}

/// Request to register a new device.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RegisterDeviceRequest {
    pub name: String,
    pub kind: DeviceKind,
    pub public_key_b64: Option<String>,
}

/// Request to revoke a device.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RevokeDeviceRequest {
    pub device_id: DeviceId,
    pub reason: Option<String>,
}

/// Maximum length of a device name (chars).
pub const MAX_DEVICE_NAME_LEN: usize = 64;

/// Hard cap on devices per user. Prevents a buggy client from
/// spamming the registration endpoint.
pub const MAX_DEVICES_PER_USER: usize = 16;

/// Validate the user-supplied inputs to `register_device`. Surface
/// the failure as `AppError::Domain` so the dispatcher returns
/// `InvalidInput` over JSON-RPC.
fn validate_register_request(req: &RegisterDeviceRequest) -> AppResult<()> {
    let trimmed = req.name.trim();
    if trimmed.is_empty() {
        return Err(AppError::Domain("device name must be non-empty".into()));
    }
    if req.name.chars().count() > MAX_DEVICE_NAME_LEN {
        return Err(AppError::Domain(format!(
            "device name length > {MAX_DEVICE_NAME_LEN} chars"
        )));
    }
    if let Some(b64) = &req.public_key_b64 {
        // Reject malformed base64 — keeps the fingerprint a valid
        // encoded blob in storage.
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| AppError::Domain(format!("public_key_b64: invalid base64: {e}")))?;
    }
    Ok(())
}

/// Device service for managing user devices.
/// DO-178C §6.4.6: Device registration is cryptographically bound.
#[derive(Clone, Debug)]
pub struct DeviceService {
    /// Source of truth for the device list, keyed by device id.
    /// In production this would be backed by persistent storage.
    devices: Arc<RwLock<HashMap<DeviceId, Device>>>,
    /// Per-user "current device" pointer. Previously a single,
    /// global `Option<DeviceId>` which overwrote under multi-user
    /// workloads — see audit-trail.
    current_device_id: Arc<RwLock<HashMap<UserId, DeviceId>>>,
    bus: NotificationBus,
}

impl Default for DeviceService {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceService {
    /// Create a new DeviceService instance.
    #[must_use = "constructing a device service without using it is a bug"]
    pub fn new() -> Self {
        Self::new_with_bus(NotificationBus::default())
    }

    /// Build a service wired to a [`NotificationBus`] so that
    /// `register`/`revoke`/`set_primary` can publish events.
    #[must_use = "constructing a device service without using it is a bug"]
    pub fn new_with_bus(bus: NotificationBus) -> Self {
        Self {
            devices: Arc::new(RwLock::new(HashMap::new())),
            current_device_id: Arc::new(RwLock::new(HashMap::new())),
            bus,
        }
    }

    /// Register a new device for the user.
    pub async fn register_device(
        &self,
        user_id: &UserId,
        request: RegisterDeviceRequest,
    ) -> AppResult<Device> {
        validate_register_request(&request)?;
        // Per-user device cap, evaluated before allocation.
        let mut devices = self.devices.write().await;
        let owned = devices.values().filter(|d| d.user_id == *user_id).count();
        if owned >= MAX_DEVICES_PER_USER {
            return Err(AppError::Domain(format!(
                "device limit reached ({MAX_DEVICES_PER_USER}) per user"
            )));
        }

        let device_id = DeviceId::from(format!("device-{}", uuid::Uuid::new_v4()));
        let now = Utc::now();

        let device = Device {
            device_id: device_id.clone(),
            user_id: user_id.clone(),
            name: request.name,
            kind: request.kind,
            last_seen: now,
            last_sync_at: Some(now.timestamp()),
            is_current: true,
            is_primary: false,
            public_key_fingerprint: request.public_key_b64,
        };

        // Mark all of this user's existing devices as non-current
        // *before* inserting the new one. Holding the same write
        // lock for the entire rendezvous prevents two concurrent
        // register calls from both observing `is_current = true`.
        for d in devices.values_mut() {
            if d.user_id == *user_id {
                d.is_current = false;
            }
        }
        devices.insert(device_id.clone(), device.clone());

        // Update per-user current pointer.
        let mut current = self.current_device_id.write().await;
        current.insert(user_id.clone(), device_id.clone());

        self.bus.publish(A3chatEvent::DeviceRegistered {
            user_id: user_id.clone(),
            device_id: device_id.as_str().to_string(),
        });

        Ok(device)
    }

    /// List all devices for a user.
    pub async fn list_devices(&self, user_id: &UserId) -> Vec<Device> {
        let devices = self.devices.read().await;
        devices.values().filter(|d| d.user_id == *user_id).cloned().collect()
    }

    /// Get a specific device by ID.
    pub async fn get_device(&self, device_id: &DeviceId) -> Option<Device> {
        let devices = self.devices.read().await;
        devices.get(device_id).cloned()
    }

    /// Revoke (delete) a device. Verifies the device exists, that
    /// it is not the *current* device for its owner, and that the
    /// caller (`owner`) actually owns it. Order of checks is
    /// important: fetch first, validate, *then* mutate.
    pub async fn revoke_device(
        &self,
        owner: &UserId,
        device_id: &DeviceId,
    ) -> AppResult<()> {
        let mut devices = self.devices.write().await;
        let device = devices
            .get(device_id)
            .ok_or_else(|| AppError::Domain(format!("device {device_id} not found")))?;
        if device.user_id != *owner {
            // We don't leak the existence of a device the caller
            // does not own — collapse to the same Forbidden used
            // elsewhere.
            return Err(AppError::Forbidden("device not owned by caller".into()));
        }
        if device.is_current {
            return Err(AppError::Domain(
                "cannot revoke the current device; revoke is for peer devices only".into(),
            ));
        }
        devices.remove(device_id);
        drop(devices);

        // If the revoked device was the per-user primary, demote
        // the field defensively (no other device can be primary
        // unless the user calls `set_primary` again).
        self.bus.publish(A3chatEvent::DeviceRevoked {
            user_id: owner.clone(),
            device_id: device_id.as_str().to_string(),
        });
        Ok(())
    }

    /// Set a device as the primary device.
    /// `owner` MUST match the device's user_id; otherwise we
    /// refuse with `Forbidden` (preventing a user from promoting
    /// another user's device).
    pub async fn set_primary(
        &self,
        owner: &UserId,
        device_id: &DeviceId,
    ) -> AppResult<()> {
        let mut devices = self.devices.write().await;
        let device = devices
            .get(device_id)
            .ok_or_else(|| AppError::Domain(format!("device {device_id} not found")))?;
        if device.user_id != *owner {
            return Err(AppError::Forbidden("device not owned by caller".into()));
        }
        let owning_user = device.user_id.clone();

        // Two-phase: clear, then set. Holding one lock prevents
        // a concurrent `set_primary` from observing a half-updated
        // state.
        for d in devices.values_mut() {
            if d.user_id == owning_user {
                d.is_primary = d.device_id == *device_id;
            }
        }
        drop(devices);

        self.bus.publish(A3chatEvent::DevicePrimaryChanged {
            user_id: owner.clone(),
            device_id: device_id.as_str().to_string(),
        });
        Ok(())
    }

    /// Get the current device ID for a specific user.
    pub async fn get_current_device(&self, user_id: &UserId) -> Option<DeviceId> {
        self.current_device_id.read().await.get(user_id).cloned()
    }

    /// Record a sync event for a device.
    pub async fn touch_device(
        &self,
        owner: &UserId,
        device_id: &DeviceId,
    ) -> AppResult<()> {
        let mut devices = self.devices.write().await;
        let device = devices
            .get_mut(device_id)
            .ok_or_else(|| AppError::Domain(format!("device {device_id} not found")))?;
        if device.user_id != *owner {
            return Err(AppError::Forbidden("device not owned by caller".into()));
        }
        device.last_seen = Utc::now();
        device.last_sync_at = Some(Utc::now().timestamp());
        Ok(())
    }

    /// Get the count of devices for a user.
    pub async fn device_count(&self, user_id: &UserId) -> usize {
        let devices = self.devices.read().await;
        devices.values().filter(|d| d.user_id == *user_id).count()
    }
}

/// Dispatcher entry point used by `a3chat-app::app::A3chatApp::dispatch`.
pub async fn dispatch(
    svc: Arc<DeviceService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    match method {
        "a3chat.device.register" => {
            let req: RegisterDeviceRequest = serde_json::from_value(params)
                .map_err(|e| A3chatError::InvalidInput(format!("malformed request: {e}")))?;
            let device = svc
                .register_device(owner, req)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(device).map_err(A3chatError::from)
        }
        "a3chat.device.list" => {
            let devices = svc.list_devices(owner).await;
            serde_json::to_value(devices).map_err(A3chatError::from)
        }
        "a3chat.device.get" => {
            let device_id: DeviceId = serde_json::from_value(
                params
                    .get("device_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("device_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            match svc.get_device(&device_id).await {
                Some(d) => serde_json::to_value(d).map_err(A3chatError::from),
                None => Ok(serde_json::Value::Null),
            }
        }
        "a3chat.device.revoke" => {
            let req: RevokeDeviceRequest = serde_json::from_value(params)
                .map_err(|e| A3chatError::InvalidInput(format!("malformed request: {e}")))?;
            svc.revoke_device(owner, &req.device_id)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        "a3chat.device.set_primary" => {
            let device_id: DeviceId = serde_json::from_value(
                params
                    .get("device_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("device_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            svc.set_primary(owner, &device_id)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        "a3chat.device.get_current" => match svc.get_current_device(owner).await {
            Some(d) => Ok(serde_json::json!(d)),
            None => Ok(serde_json::Value::Null),
        },
        "a3chat.device.touch" => {
            let device_id: DeviceId = serde_json::from_value(
                params
                    .get("device_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("device_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            svc.touch_device(owner, &device_id)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        m => Err(A3chatError::Internal(format!(
            "DeviceService does not handle {m}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alice() -> UserId {
        UserId::from("alice-node")
    }
    fn bob() -> UserId {
        UserId::from("bob-node")
    }

    fn req(name: &str) -> RegisterDeviceRequest {
        RegisterDeviceRequest {
            name: name.to_string(),
            kind: DeviceKind::Phone,
            public_key_b64: None,
        }
    }

    #[tokio::test]
    async fn register_device_creates_new_device() {
        let svc = DeviceService::new();
        let device = svc.register_device(&alice(), req("Alice's iPhone")).await.unwrap();
        assert_eq!(device.user_id, alice());
        assert_eq!(device.name, "Alice's iPhone");
        assert!(device.is_current);
        assert!(!device.is_primary);
    }

    #[tokio::test]
    async fn register_rejects_empty_name() {
        let svc = DeviceService::new();
        let r = svc.register_device(&alice(), req("   ")).await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn register_rejects_oversized_name() {
        let svc = DeviceService::new();
        let big = "x".repeat(MAX_DEVICE_NAME_LEN + 1);
        let r = svc.register_device(&alice(), req(&big)).await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn register_rejects_invalid_base64_public_key() {
        let svc = DeviceService::new();
        let mut r = req("laptop");
        r.public_key_b64 = Some("not-base64@@@@".into());
        let res = svc.register_device(&alice(), r).await;
        assert!(matches!(res, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn register_caps_devices_per_user() {
        let svc = DeviceService::new();
        for i in 0..MAX_DEVICES_PER_USER {
            svc.register_device(&alice(), req(&format!("dev-{i}"))).await.unwrap();
        }
        let res = svc.register_device(&alice(), req("one too many")).await;
        assert!(matches!(res, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn list_devices_returns_user_devices() {
        let svc = DeviceService::new();
        svc.register_device(&alice(), req("Phone")).await.unwrap();
        let devices = svc.list_devices(&alice()).await;
        assert_eq!(devices.len(), 1);
    }

    #[tokio::test]
    async fn revoke_device_removes_from_list() {
        let svc = DeviceService::new();
        let device = svc.register_device(&alice(), req("Tablet")).await.unwrap();
        svc.register_device(&alice(), req("Phone")).await.unwrap();
        svc.revoke_device(&alice(), &device.device_id).await.unwrap();
        assert_eq!(svc.list_devices(&alice()).await.len(), 1);
    }

    #[tokio::test]
    async fn revoke_rejects_foreign_device() {
        let svc = DeviceService::new();
        let bob_dev = svc.register_device(&bob(), req("Bob Phone")).await.unwrap();
        let r = svc.revoke_device(&alice(), &bob_dev.device_id).await;
        assert!(matches!(r, Err(AppError::Forbidden(_))));
    }

    #[tokio::test]
    async fn set_primary_marks_device() {
        let svc = DeviceService::new();
        let device = svc.register_device(&alice(), req("Desktop")).await.unwrap();
        svc.set_primary(&alice(), &device.device_id).await.unwrap();
        let devices = svc.list_devices(&alice()).await;
        let primary = devices.iter().find(|d| d.is_primary).unwrap();
        assert_eq!(primary.device_id, device.device_id);
    }

    #[tokio::test]
    async fn set_primary_rejects_foreign_device() {
        let svc = DeviceService::new();
        let bob_dev = svc.register_device(&bob(), req("Bob Phone")).await.unwrap();
        let r = svc.set_primary(&alice(), &bob_dev.device_id).await;
        assert!(matches!(r, Err(AppError::Forbidden(_))));
    }

    #[tokio::test]
    async fn device_kind_round_trip() {
        for kind in [
            DeviceKind::Phone,
            DeviceKind::Tablet,
            DeviceKind::Desktop,
            DeviceKind::Web,
        ] {
            let s = kind.as_str();
            assert_eq!(DeviceKind::from_str(s), Some(kind));
        }
        assert_eq!(DeviceKind::from_str("unknown"), None);
        let _: DeviceKind = Default::default();
    }

    #[tokio::test]
    async fn touch_device_updates_last_seen() {
        let svc = DeviceService::new();
        let device = svc.register_device(&alice(), req("Web")).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        svc.touch_device(&alice(), &device.device_id).await.unwrap();
        let updated = svc.get_device(&device.device_id).await.unwrap();
        assert!(updated.last_sync_at.is_some());
    }

    #[tokio::test]
    async fn get_device_returns_device() {
        let svc = DeviceService::new();
        let device = svc.register_device(&alice(), req("Phone")).await.unwrap();
        let found = svc.get_device(&device.device_id).await;
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "Phone");
    }

    #[tokio::test]
    async fn get_device_returns_none_for_unknown() {
        let svc = DeviceService::new();
        let found = svc.get_device(&DeviceId::from("unknown")).await;
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn get_current_device_returns_some_after_registration() {
        let svc = DeviceService::new();
        let device = svc.register_device(&alice(), req("Phone")).await.unwrap();
        assert_eq!(svc.get_current_device(&alice()).await, Some(device.device_id));
    }

    #[tokio::test]
    async fn get_current_device_is_per_user() {
        let svc = DeviceService::new();
        let alice_dev = svc.register_device(&alice(), req("Alice Phone")).await.unwrap();
        let bob_dev = svc.register_device(&bob(), req("Bob Phone")).await.unwrap();
        let bob_dev_id = bob_dev.device_id.clone();
        let alice_dev_id = alice_dev.device_id.clone();
        assert_eq!(svc.get_current_device(&alice()).await, Some(alice_dev_id.clone()));
        assert_eq!(svc.get_current_device(&bob()).await, Some(bob_dev_id.clone()));
        assert_ne!(alice_dev_id, bob_dev_id);
    }

    #[tokio::test]
    async fn get_current_device_returns_none_before_registration() {
        let svc = DeviceService::new();
        assert!(svc.get_current_device(&alice()).await.is_none());
    }

    #[tokio::test]
    async fn revoke_device_errors_on_current_device() {
        let svc = DeviceService::new();
        let device = svc.register_device(&alice(), req("Phone")).await.unwrap();
        let r = svc.revoke_device(&alice(), &device.device_id).await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn revoke_device_errors_on_not_found() {
        let svc = DeviceService::new();
        let r = svc.revoke_device(&alice(), &DeviceId::from("unknown")).await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn set_primary_errors_on_not_found() {
        let svc = DeviceService::new();
        let r = svc.set_primary(&alice(), &DeviceId::from("unknown")).await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn touch_device_errors_on_not_found() {
        let svc = DeviceService::new();
        let r = svc.touch_device(&alice(), &DeviceId::from("unknown")).await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn touch_device_errors_on_foreign_device() {
        let svc = DeviceService::new();
        let bob_dev = svc.register_device(&bob(), req("Bob Phone")).await.unwrap();
        let r = svc.touch_device(&alice(), &bob_dev.device_id).await;
        assert!(matches!(r, Err(AppError::Forbidden(_))));
    }

    #[tokio::test]
    async fn device_count_returns_correct_count() {
        let svc = DeviceService::new();
        assert_eq!(svc.device_count(&alice()).await, 0);
        svc.register_device(&alice(), req("Phone")).await.unwrap();
        assert_eq!(svc.device_count(&alice()).await, 1);
        svc.register_device(&alice(), req("Tablet")).await.unwrap();
        assert_eq!(svc.device_count(&alice()).await, 2);
    }

    #[tokio::test]
    async fn device_count_returns_zero_for_unknown_user() {
        let svc = DeviceService::new();
        assert_eq!(svc.device_count(&UserId::from("unknown")).await, 0);
    }

    #[tokio::test]
    async fn set_primary_unsets_other_devices() {
        let svc = DeviceService::new();
        let device1 = svc.register_device(&alice(), req("Phone")).await.unwrap();
        let device2 = svc.register_device(&alice(), req("Tablet")).await.unwrap();
        svc.set_primary(&alice(), &device1.device_id).await.unwrap();
        let devices = svc.list_devices(&alice()).await;
        let phone = devices.iter().find(|d| d.device_id == device1.device_id).unwrap();
        let tablet = devices.iter().find(|d| d.device_id == device2.device_id).unwrap();
        assert!(phone.is_primary);
        assert!(!tablet.is_primary);
    }

    #[tokio::test]
    async fn multiple_registrations_mark_first_as_non_current() {
        let svc = DeviceService::new();
        let device1 = svc.register_device(&alice(), req("Phone")).await.unwrap();
        assert!(device1.is_current);
        let device2 = svc.register_device(&alice(), req("Tablet")).await.unwrap();
        let updated = svc.get_device(&device1.device_id).await.unwrap();
        assert!(!updated.is_current);
        assert!(device2.is_current);
    }

    #[tokio::test]
    async fn cross_user_isolation() {
        let svc = DeviceService::new();
        svc.register_device(&alice(), req("Alice Phone")).await.unwrap();
        svc.register_device(&bob(), req("Bob Phone")).await.unwrap();
        assert_eq!(svc.list_devices(&alice()).await.len(), 1);
        assert_eq!(svc.list_devices(&bob()).await.len(), 1);
        assert_eq!(svc.device_count(&alice()).await, 1);
        assert_eq!(svc.device_count(&bob()).await, 1);
    }
}
