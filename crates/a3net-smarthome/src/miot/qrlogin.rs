//! QR code-based login flow for Xiaomi accounts

use crate::error::{Result, SmartHomeError};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tracing::debug;

const SID: &str = "xiaomiio";
const MSG_URL: &str = "https://account.xiaomi.com/pass/serviceLogin?sid=xiaomiio&_json=true";
const QR_URL: &str = "https://account.xiaomi.com/longPolling/loginUrl";
const DEFAULT_UA: &str = "Mozilla/5.0 (Linux; Android 6.0; Nexus 5 Build/MRA58N) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Mobile Safari/537.36";
const QR_TIMEOUT: Duration = Duration::from_secs(120);

/// QR login session
#[derive(Debug)]
pub struct QRLoginSession {
    /// The URL to encode as a QR code
    pub login_url: String,
    /// Session identifier
    pub session_id: String,
    lp: String,
    device_id: String,
    client: Client,
    started: Instant,
}

/// Credentials returned after successful QR scan
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QRLoginCredentials {
    pub user_id: String,
    pub ssecurity: String,
    pub device_id: String,
    pub service_token: String,
    #[serde(rename = "cUserId", default)]
    pub c_user_id: String,
}

/// Initiate a QR login flow. Returns a session containing the login URL to render as QR.
pub async fn begin_qr_login() -> Result<QRLoginSession> {
    let client = Client::builder()
        .cookie_store(true)
        .user_agent(DEFAULT_UA)
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| SmartHomeError::Network(e.to_string()))?;

    let device_id = random_string(16);

    // Step 1: Fetch session index
    let index_text = client
        .get(MSG_URL)
        .header("Cookie", format!("deviceId={}; sdkVersion=3.4.1", device_id))
        .send()
        .await?
        .text()
        .await?;

    // Xiaomi prepends ")]}'" (11 chars) to prevent XSSI
    let json_text = strip_xssi(&index_text)?;
    let index_data: serde_json::Value = serde_json::from_str(json_text)?;

    let qs = index_data["qs"].as_str().ok_or_else(|| SmartHomeError::Protocol("missing qs".into()))?;
    let sign = index_data["_sign"].as_str().ok_or_else(|| SmartHomeError::Protocol("missing _sign".into()))?;
    let callback = index_data["callback"].as_str().ok_or_else(|| SmartHomeError::Protocol("missing callback".into()))?;
    let location = index_data["location"].as_str().ok_or_else(|| SmartHomeError::Protocol("missing location".into()))?;

    let service_param = url::Url::parse(location)
        .map_err(|e| SmartHomeError::Protocol(format!("bad location URL: {}", e)))?
        .query_pairs()
        .find(|(k, _)| k == "serviceParam")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default();

    // Step 2: Get QR login URL
    let dc = chrono::Utc::now().timestamp_millis().to_string();
    let qr_params = [
        ("_qrsize", "240"),
        ("qs", qs),
        ("bizDeviceType", ""),
        ("callback", callback),
        ("_json", "true"),
        ("theme", ""),
        ("sid", SID),
        ("needTheme", "false"),
        ("showActiveX", "false"),
        ("serviceParam", &service_param),
        ("_local", "zh_CN"),
        ("_sign", sign),
        ("_dc", &dc),
    ];

    let qr_text = client
        .get(QR_URL)
        .header("Referer", MSG_URL)
        .query(&qr_params)
        .send()
        .await?
        .text()
        .await?;

    let qr_json_text = strip_xssi(&qr_text)?;
    let qr_data: serde_json::Value = serde_json::from_str(qr_json_text)?;

    if qr_data["code"].as_i64().unwrap_or(-1) != 0 {
        return Err(SmartHomeError::Auth(format!(
            "QR URL fetch failed: {}",
            qr_data["desc"].as_str().unwrap_or("unknown")
        )));
    }

    let login_url = qr_data["loginUrl"].as_str()
        .ok_or_else(|| SmartHomeError::Protocol("missing loginUrl".into()))?
        .to_string();

    let lp = qr_data["lp"].as_str()
        .ok_or_else(|| SmartHomeError::Protocol("missing lp".into()))?
        .to_string();

    Ok(QRLoginSession {
        login_url: login_url.clone(),
        session_id: random_string(16),
        lp,
        device_id,
        client,
        started: Instant::now(),
    })
}

/// Poll the session until the user scans the QR code or the overall
/// session timeout is reached. Uses the remaining session time for each
/// individual long-poll request rather than a fixed timeout, avoiding
/// double-counting. When the response is `Pending` the caller should
/// immediately call this function again.
///
/// ```ignore
/// loop {
///     match poll_qr_login(&session).await {
///         Ok(creds) => break creds,
///         Err(Pending) => continue,
///         Err(e) => return Err(e),
///     }
/// }
/// ```
pub async fn poll_qr_login(
    session: &QRLoginSession,
) -> std::result::Result<QRLoginCredentials, PollQrError> {
    let remaining = QR_TIMEOUT.saturating_sub(session.started.elapsed());

    let lp_text = session
        .client
        .get(&session.lp)
        .header("Connection", "keep-alive")
        .timeout(remaining)
        .send()
        .await
        .map_err(|e| {
            if remaining.is_zero() {
                PollQrError::Timeout
            } else {
                PollQrError::Network(e.to_string())
            }
        })?
        .text()
        .await
        .map_err(|e| PollQrError::Network(e.to_string()))?;

    let lp_json_text = strip_xssi(&lp_text)?;
    let lp_data: serde_json::Value = serde_json::from_str(lp_json_text)
        .map_err(|e| SmartHomeError::Protocol(format!("parse poll response: {e}")))?;

    let code = lp_data["code"].as_i64().unwrap_or(-1);

    if code == 0 {
        // Scan complete — extract credentials.
        let redirect_url = lp_data["location"].as_str()
            .ok_or_else(|| SmartHomeError::Protocol("missing redirect location".into()))?;

        let resp = session
            .client
            .get(redirect_url)
            .header(
                "Cookie",
                format!("deviceId={}; sdkVersion=3.4.1", session.device_id),
            )
            .send()
            .await
            .map_err(|e| SmartHomeError::Network(e.to_string()))?;

        let service_token = resp
            .cookies()
            .find(|c| c.name() == "serviceToken")
            .map(|c| c.value().to_string())
            .unwrap_or_default();

        let c_user_id = resp
            .cookies()
            .find(|c| c.name() == "cUserId")
            .map(|c| c.value().to_string())
            .unwrap_or_default();

        let user_id = match &lp_data["userId"] {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            other => other.to_string(),
        };

        let ssecurity = lp_data["ssecurity"].as_str()
            .ok_or_else(|| SmartHomeError::Protocol("missing ssecurity".into()))?
            .to_string();

        debug!("QR login successful for user {}", user_id);

        Ok(QRLoginCredentials {
            user_id,
            ssecurity,
            device_id: session.device_id.clone(),
            service_token,
            c_user_id,
        })
    } else if code == -1 || code == -2 {
        Err(PollQrError::Expired(lp_data["desc"].as_str().unwrap_or("expired").into()))
    } else {
        Err(PollQrError::Pending(lp_data["desc"].as_str().unwrap_or("pending").into()))
    }
}

/// Error returned by [`poll_qr_login`].
#[derive(Debug)]
pub enum PollQrError {
    /// The QR code session expired (user cancelled or timed out).
    Expired(String),
    /// The long-poll request timed out waiting for the user to scan.
    Timeout,
    /// Network-level error.
    Network(String),
    /// User has not scanned yet; retry [`poll_qr_login`].
    Pending(String),
    /// Other protocol error.
    Protocol(SmartHomeError),
}

impl From<SmartHomeError> for PollQrError {
    fn from(e: SmartHomeError) -> Self {
        Self::Protocol(e)
    }
}

impl std::fmt::Display for PollQrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Expired(s) => write!(f, "QR session expired: {s}"),
            Self::Timeout => write!(f, "QR poll timed out waiting for scan"),
            Self::Network(s) => write!(f, "network error: {s}"),
            Self::Pending(s) => write!(f, "waiting for scan: {s}"),
            Self::Protocol(e) => write!(f, "protocol error: {e}"),
        }
    }
}

impl std::error::Error for PollQrError {}

/// Strip Xiaomi XSSI prefix (&&&START&&& or ")]}'\n") from response body
fn strip_xssi(text: &str) -> Result<&str> {
    // Xiaomi uses a 11-byte prefix "&&&START&&&"
    if text.len() < 11 {
        return Err(SmartHomeError::Protocol("response too short".into()));
    }
    Ok(&text[11..])
}

fn random_string(n: usize) -> String {
    use rand::Rng;
    const CHARS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut rng = rand::thread_rng();
    (0..n).map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char).collect()
}
