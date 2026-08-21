//! Mailbox client — `reqwest`-based wrapper around the mailbox HTTP
//! API. Mirrors `a3net-relay::RelayClient` style: a thin facade that
//! builds URLs and POSTs/GETs the right endpoints, surfacing failures
//! as [`MailboxError::Remote`] / [`MailboxError::Transport`].
//!
//! Each async method handles a full happy-path round-trip including
//! signature preparation (the user supplies the 65-byte signature; the
//! client only base64-encodes it). The caller is responsible for
//! holding the wallet that produces the signature.

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::auth::{canonical_ack, canonical_pull};
use crate::config::MailboxConfig;
use crate::error::{MailboxError, MailboxResult};
use crate::storage::{StoredEnvelope, Watermark};

/// Mailbox HTTP client.
#[derive(Debug, Clone)]
pub struct MailboxClient {
    cfg: MailboxConfig,
    http: reqwest::Client,
}

impl MailboxClient {
    /// Build a client from a [`MailboxConfig`]. Errors when the
    /// config has no `base_url` set (the client is then "disabled").
    pub fn new(cfg: MailboxConfig) -> MailboxResult<Self> {
        if cfg.base_url.is_none() {
            return Err(MailboxError::Config(
                "MailboxClient::new requires base_url in MailboxConfig".into(),
            ));
        }
        let http = reqwest::Client::builder()
            .timeout(cfg.upstream_timeout)
            .build()
            .map_err(MailboxError::from_reqwest)?;
        Ok(Self { cfg, http })
    }

    /// Build a client with a custom `reqwest::Client` (useful for
    /// tests that need to bypass the default timeout).
    pub fn with_http(cfg: MailboxConfig, http: reqwest::Client) -> MailboxResult<Self> {
        if cfg.base_url.is_none() {
            return Err(MailboxError::Config(
                "MailboxClient::with_http requires base_url in MailboxConfig".into(),
            ));
        }
        Ok(Self { cfg, http })
    }

    fn base_url(&self) -> &str {
        self.cfg
            .base_url
            .as_deref()
            .expect("base_url validated in new()")
    }

    /// Construct the canonical `enqueue` URL for a recipient.
    pub fn enqueue_url(&self, recipient_id: &str) -> MailboxResult<url::Url> {
        let base = self.base_url().trim_end_matches('/');
        let url = format!("{base}/v1/inbox/{recipient_id}");
        url::Url::parse(&url)
            .map_err(|e| MailboxError::Config(format!("invalid enqueue URL: {e}")))
    }

    /// Construct the canonical `pull` URL for a recipient.
    pub fn pull_url(&self, recipient_id: &str) -> MailboxResult<url::Url> {
        let base = self.base_url().trim_end_matches('/');
        let url = format!("{base}/v1/inbox/{recipient_id}");
        url::Url::parse(&url)
            .map_err(|e| MailboxError::Config(format!("invalid pull URL: {e}")))
    }

    /// Construct the canonical `ack` URL for a recipient.
    pub fn ack_url(&self, recipient_id: &str) -> MailboxResult<url::Url> {
        let base = self.base_url().trim_end_matches('/');
        let url = format!("{base}/v1/inbox/{recipient_id}/ack");
        url::Url::parse(&url)
            .map_err(|e| MailboxError::Config(format!("invalid ack URL: {e}")))
    }

    /// Return the underlying `reqwest::Client` (used by tests).
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Return the configuration snapshot.
    pub fn config(&self) -> &MailboxConfig {
        &self.cfg
    }

    /// Enqueue an envelope on behalf of `sender_id`.
    ///
    /// `sender_signature` is the 65-byte EIP-191 signature over the
    /// canonical enqueue message; the caller obtains it from
    /// `wallet.sign_personal(&digest_of(canonical_enqueue(...)))`.
    pub async fn enqueue(
        &self,
        recipient_id: &str,
        sender_id: &str,
        msg_id: &str,
        ciphertext: &[u8],
        sender_signature: &[u8],
        ttl_secs: Option<u64>,
    ) -> MailboxResult<EnqueueResponse> {
        let url = self.enqueue_url(recipient_id)?;
        let body = EnqueueRequest {
            sender_id: sender_id.to_string(),
            msg_id: msg_id.to_string(),
            ciphertext_b64: base64::engine::general_purpose::STANDARD.encode(ciphertext),
            sender_signature_b64: base64::engine::general_purpose::STANDARD
                .encode(sender_signature),
            ttl_secs,
            timestamp: Some(chrono::Utc::now().timestamp()),
        };
        let resp = self
            .http
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(MailboxError::from_reqwest)?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(MailboxError::from_reqwest)?;
        if !status.is_success() {
            return Err(MailboxError::Remote {
                status: status.as_u16(),
                body: String::from_utf8_lossy(&bytes).into_owned(),
            });
        }
        serde_json::from_slice(&bytes).map_err(|e| MailboxError::Internal(format!(
            "decode enqueue response: {e}; body={}",
            String::from_utf8_lossy(&bytes)
        )))
    }

    /// Pull envelopes for `recipient_id`. `recipient_signature` is
    /// the 65-byte EIP-191 signature over the canonical pull message.
    pub async fn pull(
        &self,
        recipient_id: &str,
        recipient_signature: &[u8],
        since: Watermark,
        limit: Option<usize>,
    ) -> MailboxResult<PullResponse> {
        let url = self.pull_url(recipient_id)?;
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(recipient_signature);
        let mut req = self
            .http
            .get(url)
            .query(&[("signature", sig_b64.as_str()), ("since", &since.to_string())]);
        if let Some(l) = limit {
            req = req.query(&[("limit", &l.to_string())]);
        }
        let resp = req.send().await.map_err(MailboxError::from_reqwest)?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(MailboxError::from_reqwest)?;
        if !status.is_success() {
            return Err(MailboxError::Remote {
                status: status.as_u16(),
                body: String::from_utf8_lossy(&bytes).into_owned(),
            });
        }
        serde_json::from_slice(&bytes).map_err(|e| MailboxError::Internal(format!(
            "decode pull response: {e}; body={}",
            String::from_utf8_lossy(&bytes)
        )))
    }

    /// Pull all envelopes for `recipient_id` by automatically following
    /// `has_more` pagination.
    ///
    /// Calls `pull` in a loop, starting at `since` and fetching up to
    /// `page_size` messages per request, until the server returns
    /// `has_more = false`. Returns all messages merged into one `Vec`.
    ///
    /// This is useful for clients that want a complete inbox snapshot
    /// without manually managing watermarks.
    ///
    /// If the server returns an error at any page, that error is returned
    /// immediately (partial results are discarded).
    pub async fn pull_all(
        &self,
        recipient_id: &str,
        recipient_signature: &[u8],
        since: Watermark,
        page_size: usize,
    ) -> MailboxResult<Vec<StoredEnvelope>> {
        let mut all = Vec::new();
        let mut cursor = since;

        loop {
            let resp = self
                .pull(recipient_id, recipient_signature, cursor, Some(page_size))
                .await?;
            all.extend(resp.messages);
            if !resp.has_more {
                break;
            }
            cursor = resp.next_watermark;
        }

        Ok(all)
    }

    /// Acknowledge receipt of `msg_ids` for `recipient_id`.
    /// `recipient_signature` is the 65-byte EIP-191 signature over
    /// the canonical ack message.
    pub async fn ack(
        &self,
        recipient_id: &str,
        recipient_signature: &[u8],
        msg_ids: &[String],
    ) -> MailboxResult<AckResponse> {
        let url = self.ack_url(recipient_id)?;
        let body = AckRequest {
            recipient_id: recipient_id.to_string(),
            msg_ids: msg_ids.to_vec(),
            signature_b64: base64::engine::general_purpose::STANDARD
                .encode(recipient_signature),
        };
        let resp = self
            .http
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(MailboxError::from_reqwest)?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(MailboxError::from_reqwest)?;
        if !status.is_success() {
            return Err(MailboxError::Remote {
                status: status.as_u16(),
                body: String::from_utf8_lossy(&bytes).into_owned(),
            });
        }
        serde_json::from_slice(&bytes).map_err(|e| MailboxError::Internal(format!(
            "decode ack response: {e}; body={}",
            String::from_utf8_lossy(&bytes)
        )))
    }
}

/// Build the canonical bytes that the caller signs with their wallet
/// for the `pull` route.
pub fn canonical_pull_bytes(recipient_id: &str) -> Vec<u8> {
    canonical_pull(recipient_id)
}

/// Build the canonical bytes that the caller signs with their wallet
/// for the `ack` route.
pub fn canonical_ack_bytes(recipient_id: &str, msg_ids: &[String]) -> Vec<u8> {
    canonical_ack(recipient_id, msg_ids)
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// Request body for `POST /v1/inbox/{recipient_id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EnqueueRequest {
    pub sender_id: String,
    pub msg_id: String,
    pub ciphertext_b64: String,
    pub sender_signature_b64: String,
    pub ttl_secs: Option<u64>,
    pub timestamp: Option<i64>,
}

/// Response body for `POST /v1/inbox/{recipient_id}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EnqueueResponse {
    pub msg_id: String,
    pub sequence: u64,
    pub queued_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub duplicate: bool,
}

/// Response body for `GET /v1/inbox/{recipient_id}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PullResponse {
    pub messages: Vec<StoredEnvelope>,
    pub next_watermark: Watermark,
    pub has_more: bool,
}

/// Request body for `POST /v1/inbox/{recipient_id}/ack`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AckRequest {
    pub recipient_id: String,
    pub msg_ids: Vec<String>,
    pub signature_b64: String,
}

/// Response body for `POST /v1/inbox/{recipient_id}/ack`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AckResponse {
    pub acked: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MailboxConfig;

    fn cfg() -> MailboxConfig {
        MailboxConfig {
            base_url: Some("http://localhost:18791".to_string()),
            ..MailboxConfig::default()
        }
    }

    #[test]
    fn client_requires_base_url() {
        let c = MailboxConfig {
            base_url: None,
            ..MailboxConfig::default()
        };
        let r = MailboxClient::new(c);
        assert!(matches!(r, Err(MailboxError::Config(_))));
    }

    #[test]
    fn urls_strip_trailing_slash() {
        let c = MailboxConfig {
            base_url: Some("http://localhost:18791/".to_string()),
            ..MailboxConfig::default()
        };
        let cli = MailboxClient::new(c).unwrap();
        assert_eq!(
            cli.enqueue_url("0x0000000000000000000000000000000000000001")
                .unwrap()
                .as_str(),
            "http://localhost:18791/v1/inbox/0x0000000000000000000000000000000000000001"
        );
    }

    #[test]
    fn ack_url_has_ack_suffix() {
        let cli = MailboxClient::new(cfg()).unwrap();
        let u = cli
            .ack_url("0x0000000000000000000000000000000000000001")
            .unwrap();
        assert!(u.as_str().ends_with("/v1/inbox/0x0000000000000000000000000000000000000001/ack"));
    }

    #[test]
    fn canonical_pull_bytes_are_stable() {
        let a = canonical_pull_bytes("0x0000000000000000000000000000000000000001");
        let b = canonical_pull_bytes("0x0000000000000000000000000000000000000001");
        assert_eq!(a, b);
    }

    #[test]
    fn canonical_ack_bytes_are_stable() {
        let ids = vec!["a".repeat(64), "b".repeat(64)];
        let a = canonical_ack_bytes("0x0000000000000000000000000000000000000001", &ids);
        let b = canonical_ack_bytes("0x0000000000000000000000000000000000000001", &ids);
        assert_eq!(a, b);
    }
}
