//! Audio — energy fingerprint + silence detection.
//!
//! The audio module computes a small per-frame energy vector
//! that downstream modules (recommendation, mood tagging,
//! duplicate detection) can compare. It is intentionally
//! decoder-free: it operates on raw PCM bytes that the
//! transcode pipeline has already produced.

use crate::codec::SampleFormat;
use crate::error::{MediaError, MediaResult};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioFrame {
    /// Time offset of this frame in ms.
    pub t_ms: u64,
    /// RMS energy in [0, 1].
    pub rms: f32,
    /// Peak absolute amplitude in [0, 1].
    pub peak: f32,
    /// True if the frame is "silent" (RMS < threshold).
    pub silent: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioEnergy {
    pub sample_rate: u32,
    pub channels: u8,
    pub format: SampleFormat,
    pub frames: Vec<AudioFrame>,
    /// Average RMS across all frames. Cached for ranking.
    pub avg_rms: f32,
    /// Fraction of frames that are silent. Cached for filtering.
    pub silence_ratio: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AudioConfig {
    /// Frame size in ms. 20 ms is the canonical voice frame.
    pub frame_ms: u32,
    /// RMS threshold below which a frame is declared "silent".
    pub silence_rms: f32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self { frame_ms: 20, silence_rms: 0.01 }
    }
}

pub fn analyze(
    samples: &[u8],
    format: SampleFormat,
    channels: u8,
    sample_rate: u32,
    cfg: AudioConfig,
) -> MediaResult<AudioEnergy> {
    if channels == 0 {
        return Err(MediaError::InvalidConfig("audio channels must be > 0".into()));
    }
    if sample_rate == 0 {
        return Err(MediaError::InvalidConfig("audio sample rate must be > 0".into()));
    }
    let bpf = format.bytes_per_sample() as usize;
    let frame_bytes = (sample_rate as u64 * cfg.frame_ms as u64 / 1_000) as usize
        * channels as usize * bpf;
    if frame_bytes == 0 {
        return Err(MediaError::InvalidConfig(
            "audio frame size collapsed to zero bytes".into(),
        ));
    }
    let mut frames = Vec::new();
    let mut total_rms = 0f64;
    let mut n_silent = 0u64;
    let mut idx = 0usize;
    let mut t_ms = 0u64;
    while idx + frame_bytes <= samples.len() {
        let (rms, peak) = match format {
            SampleFormat::S16 => block_stats_s16(&samples[idx..idx + frame_bytes], channels as usize),
            SampleFormat::S24 => block_stats_s24(&samples[idx..idx + frame_bytes], channels as usize),
            SampleFormat::F32 => block_stats_f32(&samples[idx..idx + frame_bytes], channels as usize),
        };
        let silent = rms < cfg.silence_rms;
        if silent {
            n_silent += 1;
        }
        total_rms += rms as f64;
        frames.push(AudioFrame { t_ms, rms, peak, silent });
        idx += frame_bytes;
        t_ms += cfg.frame_ms as u64;
    }
    let n = frames.len() as f64;
    let avg_rms = if n == 0.0 { 0.0 } else { (total_rms / n) as f32 };
    let silence_ratio = if n == 0.0 { 0.0 } else { (n_silent as f64 / n) as f32 };
    Ok(AudioEnergy {
        sample_rate,
        channels,
        format,
        frames,
        avg_rms,
        silence_ratio,
    })
}

fn block_stats_s16(buf: &[u8], _channels: usize) -> (f32, f32) {
    let mut sum_sq = 0f64;
    let mut peak: i64 = 0;
    let mut n = 0u64;
    for chunk in buf.chunks_exact(2) {
        let s = i16::from_le_bytes([chunk[0], chunk[1]]) as i64;
        sum_sq += (s as f64) * (s as f64);
        if s.abs() > peak { peak = s.abs(); }
        n += 1;
    }
    if n == 0 { return (0.0, 0.0); }
    let rms = (sum_sq / n as f64).sqrt() / i16::MAX as f64;
    let peak = peak as f64 / i16::MAX as f64;
    (rms as f32, peak as f32)
}

fn block_stats_s24(buf: &[u8], _channels: usize) -> (f32, f32) {
    let mut sum_sq = 0f64;
    let mut peak: i64 = 0;
    let mut n = 0u64;
    for chunk in buf.chunks_exact(3) {
        let s = (i32::from_le_bytes([chunk[0], chunk[1], chunk[2], 0]) >> 8) as i64;
        sum_sq += (s as f64) * (s as f64);
        if s.abs() > peak { peak = s.abs(); }
        n += 1;
    }
    if n == 0 { return (0.0, 0.0); }
    let rms = (sum_sq / n as f64).sqrt() / (1i32 << 23) as f64;
    let peak = peak as f64 / (1i32 << 23) as f64;
    (rms as f32, peak as f32)
}

fn block_stats_f32(buf: &[u8], _channels: usize) -> (f32, f32) {
    let mut sum_sq = 0f64;
    let mut peak = 0f32;
    let mut n = 0u64;
    for chunk in buf.chunks_exact(4) {
        let s = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        sum_sq += (s as f64) * (s as f64);
        if s.abs() > peak { peak = s.abs(); }
        n += 1;
    }
    if n == 0 { return (0.0, 0.0); }
    let rms = (sum_sq / n as f64).sqrt();
    (rms as f32, peak)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_detected_for_zero_signal() {
        let samples = vec![0u8; 48_000 * 2 * 2 * 1]; // 1 s of silence @ 48 kHz stereo S16
        let e = analyze(
            &samples,
            SampleFormat::S16,
            2,
            48_000,
            AudioConfig::default(),
        ).unwrap();
        assert_eq!(e.silence_ratio, 1.0);
        assert_eq!(e.avg_rms, 0.0);
    }

    #[test]
    fn full_scale_signal_detected_as_loud() {
        let mut samples = vec![0u8; 48_000 * 2 * 2 * 1];
        for chunk in samples.chunks_exact_mut(2) {
            chunk[0] = 0xFF;
            chunk[1] = 0x7F; // i16 max
        }
        let e = analyze(
            &samples,
            SampleFormat::S16,
            2,
            48_000,
            AudioConfig::default(),
        ).unwrap();
        assert!(e.avg_rms > 0.9);
        assert_eq!(e.silence_ratio, 0.0);
    }

    #[test]
    fn invalid_config_rejected() {
        let err = analyze(&[], SampleFormat::S16, 0, 48_000, AudioConfig::default());
        assert!(matches!(err, Err(MediaError::InvalidConfig(_))));
    }

    #[test]
    fn f32_path_works() {
        let mut samples = vec![0u8; 48_000 * 2 * 4 / 50]; // 20 ms
        for chunk in samples.chunks_exact_mut(4) {
            chunk.copy_from_slice(&0.5f32.to_le_bytes());
        }
        let e = analyze(
            &samples,
            SampleFormat::F32,
            2,
            48_000,
            AudioConfig::default(),
        ).unwrap();
        assert!(e.frames.len() >= 1);
        assert!(e.avg_rms > 0.4);
    }
}
