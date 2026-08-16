//! Model manifest - the core metadata structure

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::types::{ModelStatus, ModelType, Quantization};

/// The core model manifest containing all metadata about an AI model
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelManifest {
    /// Unique identifier (UUID v4)
    pub id: String,
    /// Human-readable model name (e.g., "Cyberpunk_LoRA", "Llama3-8B")
    pub name: String,
    /// Semantic version string (e.g., "1.0.0", "2.1.0-beta")
    pub version: String,
    /// Model type classification
    pub model_type: ModelType,
    /// File size in bytes
    pub size_bytes: u64,
    /// BLAKE3 content hash (64 hex characters)
    pub content_hash: String,
    /// Iroh blob ticket for P2P download
    pub iroh_ticket: String,
    /// Author/creator name
    pub author: String,
    /// Human-readable description
    pub description: String,
    /// Searchable tags
    pub tags: Vec<String>,
    /// Model architecture (e.g., "llama3", "stable-diffusion-xl")
    pub architecture: String,
    /// Quantization format (if applicable)
    pub quantization: Quantization,
    /// SPDX license identifier
    pub license: String,
    /// Source URL (optional, e.g., HuggingFace link)
    pub source_url: Option<String>,
    /// Model status
    pub status: ModelStatus,
    /// Download count
    pub download_count: u64,
    /// Timestamp when the model was added
    pub created_at: DateTime<Utc>,
    /// Timestamp when the model was last updated
    pub updated_at: DateTime<Utc>,
}

impl Eq for ModelManifest {}

impl ModelManifest {
    /// Create a new model manifest
    pub fn new(
        name: String,
        version: String,
        model_type: ModelType,
        size_bytes: u64,
        content_hash: String,
        iroh_ticket: String,
        author: String,
        description: String,
        tags: Vec<String>,
        architecture: String,
        quantization: Quantization,
        license: String,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            version,
            model_type,
            size_bytes,
            content_hash,
            iroh_ticket,
            author,
            description,
            tags,
            architecture,
            quantization,
            license,
            source_url: None,
            status: ModelStatus::Available,
            download_count: 0,
            created_at: now,
            updated_at: now,
        }
    }

    /// Get the display name for file size
    pub fn size_display(&self) -> String {
        format_size(self.size_bytes)
    }

    /// Check if model matches search query
    pub fn matches_query(&self, query: &str) -> bool {
        let query_lower = query.to_lowercase();
        self.name.to_lowercase().contains(&query_lower)
            || self.description.to_lowercase().contains(&query_lower)
            || self.author.to_lowercase().contains(&query_lower)
            || self.tags.iter().any(|t| t.to_lowercase().contains(&query_lower))
            || self.architecture.to_lowercase().contains(&query_lower)
    }

    /// Check if model has a specific tag
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t.eq_ignore_ascii_case(tag))
    }

    /// Check if model matches architecture
    pub fn matches_architecture(&self, arch: &str) -> bool {
        self.architecture.to_lowercase() == arch.to_lowercase()
    }

    /// Increment download count
    pub fn increment_downloads(&mut self) {
        self.download_count = self.download_count.saturating_add(1);
        self.updated_at = Utc::now();
    }

    /// Update status
    pub fn set_status(&mut self, status: ModelStatus) {
        self.status = status;
        self.updated_at = Utc::now();
    }

    /// Set the source URL
    pub fn with_source_url(mut self, url: impl Into<String>) -> Self {
        self.source_url = Some(url.into());
        self
    }

    /// Validate the manifest.
    ///
    /// Ensures:
    /// - name / author / description / iroh_ticket / architecture are non-empty
    /// - `size_bytes > 0`
    /// - `content_hash` is 64 hex characters (BLAKE3)
    /// - `iroh_ticket` starts with `iroh://` (any scheme recognized by Iroh)
    /// - tag names contain no whitespace and are ≤ 64 chars
    pub fn validate(&self) -> Result<(), super::error::ModelCatalogError> {
        use super::error::ModelCatalogError;

        if self.name.trim().is_empty() {
            return Err(ModelCatalogError::ValidationError(
                "Model name cannot be empty".to_string(),
            ));
        }
        if self.name.len() > 256 {
            return Err(ModelCatalogError::ValidationError(
                "Model name too long (> 256 chars)".to_string(),
            ));
        }
        if self.version.trim().is_empty() {
            return Err(ModelCatalogError::ValidationError(
                "Model version cannot be empty".to_string(),
            ));
        }
        if self.author.trim().is_empty() {
            return Err(ModelCatalogError::ValidationError(
                "Author cannot be empty".to_string(),
            ));
        }
        if self.content_hash.len() != 64 {
            return Err(ModelCatalogError::ValidationError(
                "Content hash must be 64 hex characters".to_string(),
            ));
        }
        if !self.content_hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(ModelCatalogError::ValidationError(
                "Content hash must be hex characters".to_string(),
            ));
        }
        if self.iroh_ticket.trim().is_empty() {
            return Err(ModelCatalogError::ValidationError(
                "Iroh ticket cannot be empty".to_string(),
            ));
        }
        if !self.iroh_ticket.starts_with("iroh://") {
            return Err(ModelCatalogError::ValidationError(format!(
                "Iroh ticket must start with `iroh://` (got: {})",
                &self.iroh_ticket[..self.iroh_ticket.len().min(32)]
            )));
        }
        if self.size_bytes == 0 {
            return Err(ModelCatalogError::ValidationError(
                "Size must be greater than 0".to_string(),
            ));
        }
        if self.architecture.trim().is_empty() {
            return Err(ModelCatalogError::ValidationError(
                "Architecture cannot be empty".to_string(),
            ));
        }
        for tag in &self.tags {
            if tag.is_empty() {
                return Err(ModelCatalogError::ValidationError(
                    "Tags cannot contain empty values".to_string(),
                ));
            }
            if tag.len() > 64 {
                return Err(ModelCatalogError::ValidationError(format!(
                    "Tag too long: {}",
                    tag
                )));
            }
            if tag.chars().any(|c| c.is_whitespace()) {
                return Err(ModelCatalogError::ValidationError(format!(
                    "Tag cannot contain whitespace: {}",
                    tag
                )));
            }
        }
        Ok(())
    }
}

/// Format bytes into human-readable string
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;

    if bytes >= TB {
        format!("{:.2} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Compute the BLAKE3 hex digest of `data`.
pub fn compute_blake3_hash(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest() -> ModelManifest {
        ModelManifest::new(
            "TestModel".to_string(),
            "1.0.0".to_string(),
            ModelType::Llm,
            1024 * 1024 * 100,
            "a".repeat(64),
            "iroh://blob/abc".to_string(),
            "Test Author".to_string(),
            "A test model".to_string(),
            vec!["test".to_string(), "llm".to_string()],
            "llama3".to_string(),
            Quantization::Q4("K_M".to_string()),
            "MIT".to_string(),
        )
    }

    #[test]
    fn test_model_manifest_creation() {
        let manifest = sample_manifest();
        assert!(!manifest.id.is_empty());
        assert_eq!(manifest.name, "TestModel");
        assert_eq!(manifest.size_display(), "100.00 MB");
    }

    #[test]
    fn test_size_format() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.00 KB");
        assert_eq!(format_size(1024 * 1024), "1.00 MB");
        assert_eq!(format_size(1024u64.pow(3)), "1.00 GB");
        assert_eq!(format_size(1024u64.pow(4)), "1.00 TB");
    }

    #[test]
    fn test_matches_query() {
        let manifest = sample_manifest();
        assert!(manifest.matches_query("test"));
        assert!(manifest.matches_query("TestModel"));
        assert!(manifest.matches_query("author"));
        assert!(manifest.matches_query("llama3"));
        // `architecture = "llama3"` contains the substring "llama",
        // so a partial search must also succeed.
        assert!(manifest.matches_query("llama"));
        assert!(!manifest.matches_query("gpt"));
    }

    #[test]
    fn test_has_tag() {
        let manifest = sample_manifest();
        assert!(manifest.has_tag("test"));
        assert!(manifest.has_tag("LLM"));
        assert!(!manifest.has_tag("nonexistent"));
    }

    #[test]
    fn test_validate_ok() {
        assert!(sample_manifest().validate().is_ok());
    }

    #[test]
    fn test_validate_empty_name() {
        let mut m = sample_manifest();
        m.name = "".into();
        assert!(m.validate().is_err());
    }

    #[test]
    fn test_validate_short_hash() {
        let mut m = sample_manifest();
        m.content_hash = "short".into();
        assert!(m.validate().is_err());
    }

    #[test]
    fn test_validate_invalid_hash_chars() {
        let mut m = sample_manifest();
        m.content_hash = "z".repeat(64);
        assert!(m.validate().is_err());
    }

    #[test]
    fn test_validate_zero_size() {
        let mut m = sample_manifest();
        m.size_bytes = 0;
        assert!(m.validate().is_err());
    }

    #[test]
    fn test_validate_bad_ticket() {
        let mut m = sample_manifest();
        m.iroh_ticket = "http://something".into();
        assert!(m.validate().is_err());
    }

    #[test]
    fn test_validate_bad_tags() {
        let mut m = sample_manifest();
        m.tags = vec!["ok".into(), "has space".into()];
        assert!(m.validate().is_err());
    }

    #[test]
    fn test_increment_downloads() {
        let mut m = sample_manifest();
        let before = m.updated_at;
        std::thread::sleep(std::time::Duration::from_millis(10));
        m.increment_downloads();
        assert_eq!(m.download_count, 1);
        assert!(m.updated_at >= before);
    }

    #[test]
    fn test_blake3_hash() {
        let h1 = compute_blake3_hash(b"hello");
        let h2 = compute_blake3_hash(b"hello");
        let h3 = compute_blake3_hash(b"world");
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn test_with_source_url() {
        let m = sample_manifest().with_source_url("https://example.com/model");
        assert_eq!(m.source_url.as_deref(), Some("https://example.com/model"));
    }

    #[test]
    fn test_matches_query_is_case_insensitive() {
        let m = sample_manifest();
        assert!(m.matches_query("TESTMODEL"));
        assert!(m.matches_query("Test AUTHOR"));
    }

    #[test]
    fn test_saturating_increment() {
        let mut m = sample_manifest();
        m.download_count = u64::MAX;
        m.increment_downloads();
        assert_eq!(m.download_count, u64::MAX);
    }

    #[test]
    fn test_validate_rejects_oversize_name() {
        let mut m = sample_manifest();
        m.name = "x".repeat(300);
        assert!(m.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_tag_with_whitespace() {
        let mut m = sample_manifest();
        m.tags = vec!["has space".to_string()];
        assert!(m.validate().is_err());
    }
}
