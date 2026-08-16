//! VP8/VP9 codec support via the `vpx` crate.
//!
//! This module is a stub. The `vpx = "0.3"` crate depends on a
//! nightly-only feature (`core::c_str::cstr_to_str`) and therefore
//! cannot be compiled on stable Rust. We keep the feature gated so the
//! rest of the workspace can build, and leave a clear extension point
//! for downstream nightly consumers.
//!
//! To enable real VP8/VP9 encoding/decoding on nightly:
//! 1. Add `dep:vpx` back to the `vpx = []` feature in `Cargo.toml`.
//! 2. Replace this stub with a real implementation that wraps the
//!    `vpx` encoder / decoder APIs.

#![allow(dead_code)]

/// Placeholder VP8/VP9 codec identifier. The real enum would come
/// from the `vpx` crate; we hard-code the values here so call-sites
/// that take an `Option<VpxCodec>` keep compiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VpxCodec {
    /// VP8 — most widely deployed WebRTC video codec.
    Vp8,
    /// VP9 — successor to VP8, ~30 % better compression.
    Vp9,
}

impl VpxCodec {
    pub fn name(&self) -> &'static str {
        match self {
            VpxCodec::Vp8 => "vp8",
            VpxCodec::Vp9 => "vp9",
        }
    }
}

/// A configuration block for the (future) VP8/VP9 encoder.
#[derive(Debug, Clone)]
pub struct VpxEncoderConfig {
    pub codec: VpxCodec,
    pub width: u32,
    pub height: u32,
    pub bitrate_kbps: u32,
    pub framerate: u32,
}

impl Default for VpxEncoderConfig {
    fn default() -> Self {
        Self {
            codec: VpxCodec::Vp8,
            width: 1280,
            height: 720,
            bitrate_kbps: 1500,
            framerate: 30,
        }
    }
}

/// Stub encoder. The real implementation will wrap `vpx::Encoder`.
pub struct VpxEncoder {
    cfg: VpxEncoderConfig,
}

impl VpxEncoder {
    pub fn new(cfg: VpxEncoderConfig) -> Result<Self, &'static str> {
        Ok(Self { cfg })
    }
    pub fn config(&self) -> &VpxEncoderConfig {
        &self.cfg
    }
}

/// Stub decoder.
pub struct VpxDecoder;

impl VpxDecoder {
    pub fn new() -> Result<Self, &'static str> {
        Ok(Self)
    }
}

/// Compile-time gate: this feature is currently a stub on stable.
pub const VPX_AVAILABLE: bool = false;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_names() {
        assert_eq!(VpxCodec::Vp8.name(), "vp8");
        assert_eq!(VpxCodec::Vp9.name(), "vp9");
    }

    #[test]
    fn default_config() {
        let cfg = VpxEncoderConfig::default();
        assert_eq!(cfg.codec, VpxCodec::Vp8);
        assert_eq!(cfg.width, 1280);
    }

    #[test]
    fn encoder_construct() {
        let enc = VpxEncoder::new(VpxEncoderConfig::default()).unwrap();
        assert_eq!(enc.config().width, 1280);
    }

    #[test]
    fn decoder_construct() {
        let _dec = VpxDecoder::new().unwrap();
    }
}
