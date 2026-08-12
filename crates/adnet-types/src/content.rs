//! BLAKE3 content addressing (iroh-blobs parity).

use serde::{Deserialize, Serialize};

use crate::error::{AdnetError, Result};

/// Lowercase hex-encoded BLAKE3 hash (32-byte digest -> 64 chars).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(transparent)]
pub struct ContentHash(String);

impl ContentHash {
    pub const HEX_LEN: usize = 64;

    /// Hash raw bytes using BLAKE3.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(blake3::hash(bytes).to_hex().to_string())
    }

    /// Hash a streaming reader; convenience wrapper.
    pub fn from_reader<R: std::io::Read>(mut reader: R) -> std::io::Result<Self> {
        let mut hasher = blake3::Hasher::new();
        let mut buf = [0u8; 16 * 1024];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(Self(hasher.finalize().to_hex().to_string()))
    }

    /// Parse an existing hex hash string.
    pub fn from_hex(s: &str) -> Result<Self> {
        if s.len() != Self::HEX_LEN || !s.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(AdnetError::InvalidContentHash(s.to_string()));
        }
        Ok(Self(s.to_ascii_lowercase()))
    }

    fn hex_nibble(b: u8) -> u8 {
        match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => 0,
        }
    }

    pub fn as_hex(&self) -> &str {
        &self.0
    }

    /// Decode the hex representation into a 32-byte vector. Callers
    /// that need a borrow should use [`Self::as_bytes_array`] which
    /// copies into a stack buffer.
    pub fn as_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32);
        let bytes = self.0.as_bytes();
        for chunk in bytes.chunks_exact(2) {
            let hi = Self::hex_nibble(chunk[0]);
            let lo = Self::hex_nibble(chunk[1]);
            out.push((hi << 4) | lo);
        }
        out
    }

    /// First 8 chars — handy for filenames.
    pub fn short(&self) -> &str {
        &self.0[..8]
    }
}

impl std::fmt::Display for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Classification of the content announced in a room.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CdnContentKind {
    Article,
    AiModel,
    VideoModel,
    Dataset,
    GenericFile,
}

impl CdnContentKind {
    pub fn from_str_loose(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "article" | "longread" => Self::Article,
            "ai_model" | "aimodel" | "llm" => Self::AiModel,
            "video_model" | "videomodel" => Self::VideoModel,
            "dataset" => Self::Dataset,
            "generic_file" | "file" => Self::GenericFile,
            _ => return None,
        })
    }

    /// Map a chat-attachment MIME-ish `file_type` string (e.g. `"image"`,
    /// `"video"`, `"model"`) to the closest [`CdnContentKind`].
    ///
    /// Mirrors `attachment_kind()` in `Exodus@src-backup/.../p2p_cdn/commands.rs`.
    pub fn from_attachment_file_type(file_type: &str) -> Self {
        match file_type.to_ascii_lowercase().as_str() {
            "image" => Self::GenericFile,
            "video" => Self::VideoModel,
            "audio" => Self::GenericFile,
            "model" | "ai_model" | "aimodel" => Self::AiModel,
            _ => Self::GenericFile,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Article => "article",
            Self::AiModel => "ai_model",
            Self::VideoModel => "video_model",
            Self::Dataset => "dataset",
            Self::GenericFile => "generic_file",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_blake3() {
        let h = ContentHash::from_bytes(b"adnet");
        assert_eq!(h.as_hex().len(), ContentHash::HEX_LEN);
        // BLAKE3 of "adnet"
        assert_eq!(
            h.as_hex(),
            "06f2d53302776b2f936007c0d902d1b389f8f9b1c576627c21a260e03d4d0f18"
        );
    }

    #[test]
    fn content_hash_short() {
        let h = ContentHash::from_bytes(b"abc");
        assert_eq!(h.short().len(), 8);
    }

    #[test]
    fn attachment_kind_mapping() {
        assert_eq!(
            CdnContentKind::from_attachment_file_type("image"),
            CdnContentKind::GenericFile
        );
        assert_eq!(
            CdnContentKind::from_attachment_file_type("video"),
            CdnContentKind::VideoModel
        );
        assert_eq!(
            CdnContentKind::from_attachment_file_type("MODEL"),
            CdnContentKind::AiModel
        );
        assert_eq!(
            CdnContentKind::from_attachment_file_type("weird"),
            CdnContentKind::GenericFile
        );
    }

    #[test]
    fn invalid_content_hash_rejected() {
        assert!(ContentHash::from_hex("bad").is_err());
        assert!(ContentHash::from_hex(&"a".repeat(ContentHash::HEX_LEN)).is_ok());
    }

    #[test]
    fn content_kind_loose_parse() {
        assert_eq!(
            CdnContentKind::from_str_loose("llm"),
            Some(CdnContentKind::AiModel)
        );
        assert_eq!(
            CdnContentKind::from_str_loose("VIDEO_MODEL"),
            Some(CdnContentKind::VideoModel)
        );
        assert_eq!(CdnContentKind::from_str_loose("wat"), None);
    }
}
