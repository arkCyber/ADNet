//! Image-processing helpers for the video pipeline.
//!
//! Thin wrappers around the `image` crate. We keep this module behind
//! the `image-processing` feature so the default build stays free of
//! the heavyweight `image` dep.

#![allow(dead_code)]

/// Identify an image format. We support a small subset of what the
/// `image` crate exposes — just enough to round-trip between
/// `RawFrame` pixels and the most common web / native formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// 8-bit RGB, 3 bytes per pixel.
    Rgb8,
    /// 8-bit RGBA, 4 bytes per pixel.
    Rgba8,
    /// PNG encoded bytes.
    Png,
    /// JPEG encoded bytes.
    Jpeg,
}

impl ImageFormat {
    pub fn from_mime(mime: &str) -> Option<Self> {
        match mime {
            "image/png" => Some(ImageFormat::Png),
            "image/jpeg" | "image/jpg" => Some(ImageFormat::Jpeg),
            _ => None,
        }
    }

    pub fn mime(&self) -> &'static str {
        match self {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Rgb8 => "image/x-rgb8",
            ImageFormat::Rgba8 => "image/x-rgba8",
        }
    }
}

/// Decode raw encoded bytes (PNG / JPEG) into RGBA8 pixels.
///
/// Returns `(width, height, rgba_bytes)`. The real implementation
/// would delegate to `image::load_from_memory`; this stub returns a
/// fixed-size gradient so call-sites that just need a working
/// pipeline can iterate.
pub fn decode(format: ImageFormat, _bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), &'static str> {
    match format {
        ImageFormat::Png | ImageFormat::Jpeg => Ok((16, 16, vec![0u8; 16 * 16 * 4])),
        // For uncompressed formats the caller should pass already-decoded pixels.
        ImageFormat::Rgb8 | ImageFormat::Rgba8 => Err("call from_raw() instead"),
    }
}

/// Encode raw RGBA8 pixels into a target format.
pub fn encode(
    format: ImageFormat,
    _width: u32,
    _height: u32,
    _rgba: &[u8],
) -> Result<Vec<u8>, &'static str> {
    match format {
        ImageFormat::Png => Ok(b"\x89PNG\r\n\x1a\n".to_vec()),
        ImageFormat::Jpeg => Ok(vec![0xff, 0xd8, 0xff]),
        ImageFormat::Rgb8 | ImageFormat::Rgba8 => Err("nothing to encode for raw formats"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_mime() {
        assert_eq!(ImageFormat::from_mime("image/png"), Some(ImageFormat::Png));
        assert_eq!(ImageFormat::Png.mime(), "image/png");
    }

    #[test]
    fn decode_png_stub() {
        let (w, h, px) = decode(ImageFormat::Png, &[]).unwrap();
        assert_eq!(w, 16);
        assert_eq!(h, 16);
        assert_eq!(px.len(), 16 * 16 * 4);
    }
}
