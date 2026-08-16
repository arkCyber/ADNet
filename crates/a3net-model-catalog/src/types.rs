//! Core types for the model catalog

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Model type classification
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelType {
    /// Large Language Model (e.g., Llama, Mistral, GPT)
    Llm,
    /// LoRA adapter (Low-Rank Adaptation)
    Lora,
    /// Text-to-Image model (e.g., Stable Diffusion)
    TextToImage,
    /// Image-to-Image model
    ImageToImage,
    /// ControlNet model
    ControlNet,
    /// VAE (Variational Autoencoder)
    Vae,
    /// Text-to-Video model
    TextToVideo,
    /// Image-to-Video model
    ImageToVideo,
    /// Embedding model
    Embedding,
    /// Speech-to-Text (Whisper, etc.)
    SpeechToText,
    /// Text-to-Speech
    TextToSpeech,
    /// Vision model
    Vision,
    /// Multilingual model
    Multilingual,
    /// Other/unknown type
    Other(String),
}

impl ModelType {
    /// Convert to display string
    pub fn display_name(&self) -> String {
        match self {
            ModelType::Llm => "LLM".to_string(),
            ModelType::Lora => "LoRA".to_string(),
            ModelType::TextToImage => "Text-to-Image".to_string(),
            ModelType::ImageToImage => "Image-to-Image".to_string(),
            ModelType::ControlNet => "ControlNet".to_string(),
            ModelType::Vae => "VAE".to_string(),
            ModelType::TextToVideo => "Text-to-Video".to_string(),
            ModelType::ImageToVideo => "Image-to-Video".to_string(),
            ModelType::Embedding => "Embedding".to_string(),
            ModelType::SpeechToText => "Speech-to-Text".to_string(),
            ModelType::TextToSpeech => "Text-to-Speech".to_string(),
            ModelType::Vision => "Vision".to_string(),
            ModelType::Multilingual => "Multilingual".to_string(),
            ModelType::Other(s) => s.clone(),
        }
    }

    /// Parse from string (case-insensitive)
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "llm" | "language_model" | "text" => ModelType::Llm,
            "lora" | "adapter" => ModelType::Lora,
            "text_to_image" | "t2i" | "txt2img" | "stable_diffusion" | "sdxl" => ModelType::TextToImage,
            "image_to_image" | "i2i" | "img2img" => ModelType::ImageToImage,
            "controlnet" | "control_net" => ModelType::ControlNet,
            "vae" | "autoencoder" => ModelType::Vae,
            "text_to_video" | "t2v" | "txt2vid" => ModelType::TextToVideo,
            "image_to_video" | "i2v" | "img2vid" => ModelType::ImageToVideo,
            "embedding" | "embeddings" => ModelType::Embedding,
            "speech_to_text" | "stt" | "whisper" => ModelType::SpeechToText,
            "text_to_speech" | "tts" => ModelType::TextToSpeech,
            "vision" | "visual" => ModelType::Vision,
            "multilingual" | "multi" => ModelType::Multilingual,
            other => ModelType::Other(other.to_string()),
        }
    }
}

impl std::fmt::Display for ModelType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

/// Quantization format
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Quantization {
    /// No quantization (full precision)
    None,
    /// 4-bit integer (Q4_0, Q4_K_M, etc.)
    Q4(String),
    /// 8-bit integer (Q8_0, Q8_K, etc.)
    Q8(String),
    /// 16-bit float (FP16, BF16)
    FP16,
    /// GPTQ format
    GPTQ(String),
    /// AWQ format
    AWQ(String),
    /// GGUF format
    GGUF(String),
    /// Other quantization
    Other(String),
}

impl Quantization {
    /// Check if this is a quantized model
    pub fn is_quantized(&self) -> bool {
        !matches!(self, Quantization::None | Quantization::FP16)
    }

    /// Get display name
    pub fn display_name(&self) -> String {
        match self {
            Quantization::None => "Full Precision".to_string(),
            Quantization::FP16 => "FP16".to_string(),
            Quantization::Q4(s) => format!("Q4{}", if s.is_empty() { "" } else { "_" }).to_string() + s,
            Quantization::Q8(s) => format!("Q8{}", if s.is_empty() { "" } else { "_" }).to_string() + s,
            Quantization::GPTQ(s) => format!("GPTQ{}", if s.is_empty() { "" } else { " " }).to_string() + s,
            Quantization::AWQ(s) => format!("AWQ{}", if s.is_empty() { "" } else { " " }).to_string() + s,
            Quantization::GGUF(s) => format!("GGUF{}", if s.is_empty() { "" } else { " " }).to_string() + s,
            Quantization::Other(s) => s.clone(),
        }
    }
}

impl Default for Quantization {
    fn default() -> Self {
        Quantization::None
    }
}

/// Model status in the catalog
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelStatus {
    /// Model is available for download
    Available,
    /// Model is being downloaded
    Downloading,
    /// Model is being uploaded by provider
    Uploading,
    /// Model is temporarily unavailable
    Unavailable,
    /// Model has been removed
    Removed,
}

impl Default for ModelStatus {
    fn default() -> Self {
        ModelStatus::Available
    }
}

/// Search filter for models
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelFilter {
    /// Filter by model type
    pub model_type: Option<ModelType>,
    /// Filter by tags (AND logic)
    pub tags: Option<Vec<String>>,
    /// Filter by architecture (e.g., "llama3", "sdxl")
    pub architecture: Option<String>,
    /// Filter by minimum size (bytes)
    pub min_size: Option<u64>,
    /// Filter by maximum size (bytes)
    pub max_size: Option<u64>,
    /// Filter by author
    pub author: Option<String>,
    /// Filter by quantization
    pub quantization: Option<Quantization>,
    /// Full-text search query
    pub query: Option<String>,
    /// Sort by field
    pub sort_by: Option<SortField>,
    /// Sort direction
    pub sort_desc: Option<bool>,
    /// Pagination offset
    pub offset: Option<u64>,
    /// Pagination limit
    pub limit: Option<u64>,
}

/// Sort field options
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortField {
    Name,
    CreatedAt,
    UpdatedAt,
    Size,
    Downloads,
}

impl Default for SortField {
    fn default() -> Self {
        SortField::CreatedAt
    }
}

/// Paginated result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaginatedModels<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub offset: u64,
    pub limit: u64,
}

impl<T> PaginatedModels<T> {
    pub fn new(items: Vec<T>, total: u64, offset: u64, limit: u64) -> Self {
        Self { items, total, offset, limit }
    }

    pub fn has_more(&self) -> bool {
        (self.offset as usize + self.items.len()) < self.total as usize
    }
}

/// Download progress tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub model_id: String,
    pub status: DownloadStatus,
    pub bytes_downloaded: u64,
    pub total_bytes: u64,
    pub speed_bps: u64,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl DownloadProgress {
    pub fn percent(&self) -> f64 {
        if self.total_bytes == 0 {
            0.0
        } else {
            (self.bytes_downloaded as f64 / self.total_bytes as f64) * 100.0
        }
    }
}

/// Download status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DownloadStatus {
    Pending,
    Connecting,
    Downloading,
    Verifying,
    Completed,
    Failed(String),
    Cancelled,
}

impl DownloadProgress {
    pub fn new(model_id: String, total_bytes: u64) -> Self {
        let now = Utc::now();
        Self {
            model_id,
            status: DownloadStatus::Pending,
            bytes_downloaded: 0,
            total_bytes,
            speed_bps: 0,
            started_at: now,
            updated_at: now,
        }
    }

    pub fn update(&mut self, bytes: u64, speed: u64) {
        self.bytes_downloaded = bytes;
        self.speed_bps = speed;
        self.status = DownloadStatus::Downloading;
        self.updated_at = Utc::now();
    }

    pub fn complete(&mut self) {
        self.bytes_downloaded = self.total_bytes;
        self.status = DownloadStatus::Completed;
        self.updated_at = Utc::now();
    }

    pub fn fail(&mut self, error: String) {
        self.status = DownloadStatus::Failed(error);
        self.updated_at = Utc::now();
    }
}

/// Statistics for the catalog
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CatalogStats {
    pub total_models: u64,
    pub total_size_bytes: u64,
    pub models_by_type: std::collections::HashMap<String, u64>,
    pub recent_models: u64,
    pub active_downloads: u64,
}
