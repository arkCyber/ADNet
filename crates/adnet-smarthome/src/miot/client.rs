//! MIoT cloud API client

use crate::error::{Result, SmartHomeError};
use super::crypto::MiotCrypto;
use super::types::*;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, info};

const MIOT_API_HOST: &str = "https://api.io.mi.com";
const USER_AGENT: &str = "APP/com.xiaomi.mihome APPV/6.0.103 iosPassportSDK/3.9.0 iOS/14.4 miHSTS";

/// Credentials for authenticating with the MIoT cloud
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiotAuth {
    pub user_id: String,
    pub service_token: String,
    pub device_id: String,
    pub ssecurity: String,
}

/// Alias for the QR login result struct so callers can convert directly
pub type QRCodeLoginResult = super::qrlogin::QRLoginCredentials;

impl From<QRCodeLoginResult> for MiotAuth {
    fn from(r: QRCodeLoginResult) -> Self {
        Self {
            user_id: r.user_id,
            service_token: r.service_token,
            device_id: r.device_id,
            ssecurity: r.ssecurity,
        }
    }
}

/// Generic MIoT API response wrapper
#[derive(Debug, Deserialize)]
struct ApiResponse {
    pub code: i32,
    pub message: Option<String>,
    pub result: Option<serde_json::Value>,
}

/// Authenticated MIoT cloud API client
pub struct MiotClient {
    auth: Arc<RwLock<MiotAuth>>,
    http: Client,
    crypto: MiotCrypto,
    api_host: String,
}

impl MiotClient {
    pub fn new(auth: MiotAuth) -> Result<Self> {
        Self::with_host(auth, MIOT_API_HOST.to_string())
    }

    /// Build a client pointed at a custom API host. Used by tests to
    /// point the client at a local mock server instead of Xiaomi's
    /// production cloud.
    pub fn with_host(auth: MiotAuth, api_host: String) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| SmartHomeError::Network(e.to_string()))?;

        Ok(Self {
            auth: Arc::new(RwLock::new(auth)),
            http,
            crypto: MiotCrypto::new(),
            api_host,
        })
    }

    /// List all devices in the user's account
    pub async fn get_device_list(&self) -> Result<Vec<MiotDevice>> {
        let val = self.post("/app/v2/home/device_list", serde_json::json!({})).await?;
        let list = val.get("list").cloned()
            .ok_or_else(|| SmartHomeError::Protocol("device_list: missing 'list' key".into()))?;
        serde_json::from_value(list)
            .map_err(|e| SmartHomeError::Protocol(format!("parse device list: {}", e)))
    }

    /// Get named properties from a device (MIoT property get)
    pub async fn get_device_properties(
        &self,
        device_id: &str,
        props: Vec<Property>,
    ) -> Result<Vec<PropertyValue>> {
        let body = serde_json::json!({
            "did": device_id,
            "params": props,
        });
        let val = self.post("/app/v2/properties/get", body).await?;
        serde_json::from_value(val)
            .map_err(|e| SmartHomeError::Protocol(format!("parse property values: {}", e)))
    }

    /// Set properties on a device
    pub async fn set_device_properties(
        &self,
        device_id: &str,
        props: Vec<PropertyValue>,
    ) -> Result<()> {
        let body = serde_json::json!({
            "did": device_id,
            "params": props,
        });
        self.post("/app/v2/properties/set", body).await?;
        Ok(())
    }

    /// Invoke a MIoT action
    pub async fn invoke_action(
        &self,
        device_id: &str,
        action: Action,
    ) -> Result<serde_json::Value> {
        let body = serde_json::json!({
            "did": device_id,
            "action": action,
        });
        self.post("/app/v2/action/invoke", body).await
    }

    /// Legacy RPC call (get_prop / set_prop / etc.)
    pub async fn rpc(
        &self,
        device_id: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let body = serde_json::json!({
            "did": device_id,
            "method": method,
            "params": params,
        });
        self.post("/app/home/rpc", body).await
    }

    /// Get device spec from the MIoT spec catalogue
    pub async fn get_device_spec(&self, model: &str) -> Result<MiotDeviceSpec> {
        let body = serde_json::json!({ "model": model });
        let val = self.post("/app/v2/spec/device", body).await?;
        serde_json::from_value(val)
            .map_err(|e| SmartHomeError::Protocol(format!("parse device spec: {}", e)))
    }

    /// Update stored credentials (e.g. after token refresh)
    pub async fn update_auth(&self, auth: MiotAuth) {
        let mut guard = self.auth.write().await;
        info!("MIoT auth updated for user {}", auth.user_id);
        *guard = auth;
    }

    // ── private helper ──────────────────────────────────────────────────────

    async fn post(&self, path: &str, data: serde_json::Value) -> Result<serde_json::Value> {
        let auth = self.auth.read().await;
        let data_str = serde_json::to_string(&data)?;

        let nonce = self.crypto.generate_nonce();
        let signed_nonce = self.crypto.generate_signed_nonce(&auth.ssecurity, &nonce)?;
        let signature = self.crypto.generate_signature(path, &signed_nonce, &nonce, &data_str)?;

        let cookie = format!(
            "serviceToken={}; userId={}; PassportDeviceId={}",
            auth.service_token, auth.user_id, auth.device_id
        );

        debug!("POST {} ({} bytes)", path, data_str.len());

        let resp = self.http
            .post(format!("{}{}", self.api_host, path))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Cookie", cookie)
            .header("x-xiaomi-protocal-flag-cli", "PROTOCAL-HTTP2")
            .form(&[("_nonce", &nonce), ("data", &data_str), ("signature", &signature)])
            .send()
            .await
            .map_err(|e| SmartHomeError::Network(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(SmartHomeError::Network(format!("HTTP {}", resp.status())));
        }

        let api_resp: ApiResponse = resp.json().await
            .map_err(|e| SmartHomeError::Protocol(format!("parse API response: {}", e)))?;

        if api_resp.code != 0 {
            return Err(SmartHomeError::DeviceControl(format!(
                "MIoT error code={} msg={}",
                api_resp.code,
                api_resp.message.unwrap_or_default()
            )));
        }

        Ok(api_resp.result.unwrap_or(serde_json::Value::Null))
    }
}
