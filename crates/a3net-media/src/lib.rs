//! `a3net-media` — aerospace-grade short-video media pipeline.
//!
//! DO-178C DAL-B. Every clip is decomposed into a deterministic DAG
//! of segments; every segment is BLAKE3-addressed; every manifest
//! is length-prefixed for decoder-bounded IO.
//!
//! Pipeline:
//! ```text
//! raw bytes
//!    │
//!    ▼  ingest
//! EncodedMedia { codec, dimensions, duration, samples }
//!    │
//!    ▼  transcode → variant ladder
//! Vec<Variant>     (e.g. 480p, 720p, 1080p)
//!    │
//!    ▼  segment
//! Vec<Segment>     (fixed duration; GOP-aligned)
//!    │
//!    ▼  audio
//! Vec<AudioChunk>  (PCM frame windows + energy fingerprint)
//!    │
//!    ▼  DAG
//! MediaDag { root_cid, variants, audio, manifest_cid }
//! ```
//!
//! Safety: every public function returns a `Result<_, MediaError>`
//! and never `unwrap()`s. Decoder-facing payloads carry an explicit
//! length prefix (`u32` little-endian) so a truncated stream raises
//! `MediaError::TruncatedFrame` rather than overrunning a buffer.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod audio;
pub mod codec;
pub mod config;
pub mod dag;
pub mod error;
pub mod ffmpeg;
pub mod ffmpeg_locator;
pub mod ffmpeg_probe;
pub mod ingest;
pub mod integrity;
pub mod manifest;
pub mod persist;
pub mod segment;
pub mod transcode;
pub mod verify;

#[cfg(feature = "aerospace")]
pub mod aerospace;

// Re-exports for ergonomic top-level usage.
pub use codec::{AudioCodec, MediaKind, SampleFormat, VideoCodec};
pub use config::{MediaConfig, MediaConfigError, SegmenterConfig, VariantLadder, VariantSpec};
pub use dag::{MediaDag, MediaDagBuilder, VariantNode, VideoSegmentNode, AudioSegmentNode};
pub use error::{MediaError, MediaResult};
pub use ingest::{IngestReport, MediaIngester};
pub use integrity::{MediaDigest, SegmentDigest, media_root_hash, segment_hash};
pub use manifest::{MediaManifest, MediaManifestV1, VariantManifest, AudioManifest, SegmentRef};
pub use segment::{Segment, SegmentKind, Segmenter};
pub use transcode::{TranscodeError, TranscodeInput, TranscodeOutput, Transcoder};
pub use verify::{VerifyReport, VerifyStatus, verify_dag, verify_manifest};
pub use ffmpeg::{FFmpegConfig, FFmpegTranscoder, ProgressCallback, transcode_synthetic};
pub use ffmpeg_locator::{DEFAULT_FFMPEG_BIN, DEFAULT_FFPROBE_BIN, FFmpegLocator};
pub use ffmpeg_probe::MediaProbe;
pub use persist::{AliasMap, MediaStore, MediaStoreReport};
