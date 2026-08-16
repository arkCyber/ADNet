//! Remote backend support for backup storage.
//!
//! DO-178C SR-1: Remote backup storage for disaster recovery.

use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, info};

/// Error types for remote operations.
#[derive(Debug, Error)]
pub enum RemoteError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("upload failed: {0}")]
    UploadFailed(String),
    #[error("download failed: {0}")]
    DownloadFailed(String),
    #[error("backend error: {0}")]
    Backend(String),
    #[error("configuration error: {0}")]
    Config(String),
}

/// Remote storage backend trait.
///
/// DO-178C SR-1: Abstract backend for multiple storage providers.
#[async_trait]
pub trait RemoteBackend: Send + Sync {
    /// Upload a file to remote storage.
    async fn upload(&self, local_path: &Path, remote_key: &str) -> Result<String, RemoteError>;

    /// Download a file from remote storage.
    async fn download(&self, remote_key: &str, local_path: &Path) -> Result<(), RemoteError>;

    /// List files in remote storage with a prefix.
    async fn list(&self, prefix: &str) -> Result<Vec<RemoteFileInfo>, RemoteError>;

    /// Delete a file from remote storage.
    async fn delete(&self, remote_key: &str) -> Result<(), RemoteError>;

    /// Check if a file exists in remote storage.
    async fn exists(&self, remote_key: &str) -> Result<bool, RemoteError>;

    /// Get the URL for a file (if public).
    async fn get_url(&self, remote_key: &str) -> Result<Option<String>, RemoteError>;
}

/// Information about a remote file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteFileInfo {
    /// Key/path in remote storage.
    pub key: String,
    /// Size in bytes.
    pub size: u64,
    /// Last modified timestamp (Unix).
    pub modified: i64,
    /// ETag or checksum.
    pub etag: Option<String>,
}

/// S3-compatible storage backend configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct S3Config {
    /// S3 endpoint URL.
    pub endpoint: String,
    /// AWS region.
    pub region: String,
    /// Bucket name.
    pub bucket: String,
    /// Access key ID.
    pub access_key: String,
    /// Secret access key.
    pub secret_key: String,
    /// Use path-style addressing.
    pub path_style: bool,
}

impl Default for S3Config {
    fn default() -> Self {
        Self {
            endpoint: "https://s3.amazonaws.com".to_string(),
            region: "us-east-1".to_string(),
            bucket: String::new(),
            access_key: String::new(),
            secret_key: String::new(),
            path_style: false,
        }
    }
}

/// S3-compatible remote backend.
///
/// DO-178C SR-1: AWS S3 and compatible storage (MinIO, R2, etc.)
pub struct S3Backend {
    config: S3Config,
    client: reqwest::Client,
}

impl S3Backend {
    /// Create a new S3 backend.
    pub fn new(config: S3Config) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    /// Generate the S3 URL for a key.
    fn url(&self, key: &str) -> String {
        if self.config.path_style {
            format!("{}/{}/{}", self.config.endpoint, self.config.bucket, key)
        } else {
            format!("{}/{}/{}", self.config.endpoint, self.config.bucket, key)
        }
    }

    /// Generate authorization headers.
    fn auth_headers(&self) -> reqwest::header::HeaderMap {
        use reqwest::header::{AUTHORIZATION, HeaderValue};

        let mut headers = reqwest::header::HeaderMap::new();
        
        // Note: In production, use AWS SDK or signed requests
        // This is a simplified implementation
        if !self.config.access_key.is_empty() {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_static(""),
            );
        }

        headers
    }
}

#[async_trait]
impl RemoteBackend for S3Backend {
    async fn upload(&self, local_path: &Path, remote_key: &str) -> Result<String, RemoteError> {
        let bytes = std::fs::read(local_path)?;

        let url = self.url(remote_key);
        debug!(url = %url, size = bytes.len(), "Uploading to S3");

        let response = self.client
            .put(&url)
            .headers(self.auth_headers())
            .header("Content-Type", "application/octet-stream")
            .header("Content-Length", bytes.len())
            .body(bytes)
            .send()
            .await?;

        if response.status().is_success() {
            info!(key = %remote_key, "Upload complete");
            Ok(remote_key.to_string())
        } else {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            Err(RemoteError::UploadFailed(format!(
                "S3 returned {}: {}",
                status, body
            )))
        }
    }

    async fn download(&self, remote_key: &str, local_path: &Path) -> Result<(), RemoteError> {
        let url = self.url(remote_key);
        debug!(url = %url, "Downloading from S3");

        let response = self.client
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .await?;

        if response.status().is_success() {
            let bytes = response.bytes().await?;
            std::fs::write(local_path, bytes)?;
            info!(key = %remote_key, path = %local_path.display(), "Download complete");
            Ok(())
        } else {
            Err(RemoteError::DownloadFailed(format!(
                "S3 returned {}",
                response.status()
            )))
        }
    }

    async fn list(&self, prefix: &str) -> Result<Vec<RemoteFileInfo>, RemoteError> {
        // Note: S3 ListObjectsV2 would be implemented here
        // For now, return empty list as a placeholder
        debug!(prefix = %prefix, "Listing S3 objects");
        Ok(Vec::new())
    }

    async fn delete(&self, remote_key: &str) -> Result<(), RemoteError> {
        let url = self.url(remote_key);

        let response = self.client
            .delete(&url)
            .headers(self.auth_headers())
            .send()
            .await?;

        if response.status().is_success() {
            info!(key = %remote_key, "Delete complete");
            Ok(())
        } else {
            Err(RemoteError::Backend(format!(
                "Delete failed: {}",
                response.status()
            )))
        }
    }

    async fn exists(&self, remote_key: &str) -> Result<bool, RemoteError> {
        let url = self.url(remote_key);

        let response = self.client
            .head(&url)
            .headers(self.auth_headers())
            .send()
            .await?;

        Ok(response.status().is_success())
    }

    async fn get_url(&self, remote_key: &str) -> Result<Option<String>, RemoteError> {
        // Generate pre-signed URL in production
        Ok(Some(self.url(remote_key)))
    }
}

/// IPFS remote backend configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpfsConfig {
    /// IPFS API endpoint.
    pub api_endpoint: String,
    /// IPFS gateway endpoint (for public URLs).
    pub gateway_endpoint: String,
    /// Optional API key.
    pub api_key: Option<String>,
}

impl Default for IpfsConfig {
    fn default() -> Self {
        Self {
            api_endpoint: "http://127.0.0.1:5001".to_string(),
            gateway_endpoint: "https://ipfs.io".to_string(),
            api_key: None,
        }
    }
}

/// IPFS remote backend.
///
/// DO-178C SR-1: Decentralized storage via IPFS.
pub struct IpfsBackend {
    config: IpfsConfig,
    client: reqwest::Client,
}

impl IpfsBackend {
    /// Create a new IPFS backend.
    pub fn new(config: IpfsConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl RemoteBackend for IpfsBackend {
    async fn upload(&self, local_path: &Path, _remote_key: &str) -> Result<String, RemoteError> {
        let file = std::fs::read(local_path)?;

        let response = self.client
            .post(&format!("{}/api/v0/add", self.config.api_endpoint))
            .header("Content-Type", "application/octet-stream")
            .body(file)
            .send()
            .await?;

        if response.status().is_success() {
            // Parse response to get CID
            let body: serde_json::Value = response.json().await
                .map_err(|e| RemoteError::Backend(format!("Failed to parse IPFS response: {}", e)))?;

            let cid = body["Hash"]
                .as_str()
                .ok_or_else(|| RemoteError::Backend("No Hash in response".to_string()))?;

            info!(cid = %cid, "Uploaded to IPFS");
            Ok(cid.to_string())
        } else {
            Err(RemoteError::UploadFailed(format!(
                "IPFS returned {}",
                response.status()
            )))
        }
    }

    async fn download(&self, remote_key: &str, local_path: &Path) -> Result<(), RemoteError> {
        let url = format!("{}/ipfs/{}", self.config.gateway_endpoint, remote_key);

        let response = self.client.get(&url).send().await?;

        if response.status().is_success() {
            let bytes = response.bytes().await?;
            std::fs::write(local_path, bytes)?;
            info!(cid = %remote_key, path = %local_path.display(), "Downloaded from IPFS");
            Ok(())
        } else {
            Err(RemoteError::DownloadFailed(format!(
                "IPFS gateway returned {}",
                response.status()
            )))
        }
    }

    async fn list(&self, _prefix: &str) -> Result<Vec<RemoteFileInfo>, RemoteError> {
        // IPFS doesn't have traditional directory listing
        // This would require pin/ls functionality
        Ok(Vec::new())
    }

    async fn delete(&self, _remote_key: &str) -> Result<(), RemoteError> {
        // IPFS is content-addressed, can't delete by key
        // Would need pin rm instead
        Ok(())
    }

    async fn exists(&self, _remote_key: &str) -> Result<bool, RemoteError> {
        // Would need to check if CID is pinned
        Ok(false)
    }

    async fn get_url(&self, remote_key: &str) -> Result<Option<String>, RemoteError> {
        Ok(Some(format!("{}/ipfs/{}", self.config.gateway_endpoint, remote_key)))
    }
}

/// HTTP-based remote backend for simple uploads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpConfig {
    /// Upload endpoint.
    pub upload_url: String,
    /// Download base URL.
    pub download_url: String,
    /// Optional authentication token.
    pub auth_token: Option<String>,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            upload_url: String::new(),
            download_url: String::new(),
            auth_token: None,
        }
    }
}

/// HTTP-based remote backend.
pub struct HttpBackend {
    config: HttpConfig,
    client: reqwest::Client,
}

impl HttpBackend {
    /// Create a new HTTP backend.
    pub fn new(config: HttpConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl RemoteBackend for HttpBackend {
    async fn upload(&self, local_path: &Path, remote_key: &str) -> Result<String, RemoteError> {
        let file = std::fs::read(local_path)?;
        let url = format!("{}/{}", self.config.upload_url.trim_end_matches('/'), remote_key);

        let mut request = self.client.put(&url).body(file);

        if let Some(token) = &self.config.auth_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request.send().await?;

        if response.status().is_success() {
            info!(key = %remote_key, "HTTP upload complete");
            Ok(remote_key.to_string())
        } else {
            Err(RemoteError::UploadFailed(format!(
                "HTTP returned {}",
                response.status()
            )))
        }
    }

    async fn download(&self, remote_key: &str, local_path: &Path) -> Result<(), RemoteError> {
        let url = format!("{}/{}", self.config.download_url.trim_end_matches('/'), remote_key);

        let mut request = self.client.get(&url);

        if let Some(token) = &self.config.auth_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request.send().await?;

        if response.status().is_success() {
            let bytes = response.bytes().await?;
            std::fs::write(local_path, bytes)?;
            info!(key = %remote_key, path = %local_path.display(), "HTTP download complete");
            Ok(())
        } else {
            Err(RemoteError::DownloadFailed(format!(
                "HTTP returned {}",
                response.status()
            )))
        }
    }

    async fn list(&self, _prefix: &str) -> Result<Vec<RemoteFileInfo>, RemoteError> {
        Ok(Vec::new())
    }

    async fn delete(&self, remote_key: &str) -> Result<(), RemoteError> {
        let url = format!("{}/{}", self.config.upload_url.trim_end_matches('/'), remote_key);

        let mut request = self.client.delete(&url);

        if let Some(token) = &self.config.auth_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request.send().await?;

        if response.status().is_success() {
            info!(key = %remote_key, "HTTP delete complete");
            Ok(())
        } else {
            Err(RemoteError::Backend(format!(
                "Delete failed: {}",
                response.status()
            )))
        }
    }

    async fn exists(&self, _remote_key: &str) -> Result<bool, RemoteError> {
        Ok(false)
    }

    async fn get_url(&self, remote_key: &str) -> Result<Option<String>, RemoteError> {
        Ok(Some(format!(
            "{}/{}",
            self.config.download_url.trim_end_matches('/'),
            remote_key
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_s3_config_default() {
        let config = S3Config::default();
        assert_eq!(config.region, "us-east-1");
        assert!(!config.path_style);
    }

    #[test]
    fn test_ipfs_config_default() {
        let config = IpfsConfig::default();
        assert!(config.api_endpoint.contains("5001"));
    }

    #[tokio::test]
    async fn test_http_backend_creation() {
        let config = HttpConfig {
            upload_url: "https://example.com/upload".to_string(),
            download_url: "https://example.com/download".to_string(),
            auth_token: Some("token123".to_string()),
        };

        let backend = HttpBackend::new(config);
        assert!(backend.config.auth_token.is_some());
    }
}
