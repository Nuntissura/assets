//! Shared helpers of the compressed-audio tests (lane SMB-AUDIO-CODECS).
#![allow(dead_code)]
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_reel::*;
use sha2::{Digest, Sha256};
use std::io::Cursor;

pub fn grant_for(bytes: &[u8]) -> Grant {
    Grant {
        content_id: ContentId::parse("audio-codecs").unwrap(),
        revision: 1,
        expected_len: bytes.len() as u64,
    }
}

pub fn request(codec: &str, start_pts: u64, count: u64, bytes: &[u8]) -> SegmentRequest {
    SegmentRequest {
        codec: codec.into(),
        start_pts: Ticks::new(start_pts),
        count,
        track: None,
        output: OutputMode::Decoded,
        prefer: SourcePreference::Original,
        purpose: Purpose::Preview,
        grant: grant_for(bytes),
        expected_revision: 1,
    }
}

/// Collects decoded f32 chunks.
#[derive(Debug, Default)]
pub struct Collect {
    pub samples: Vec<f32>,
    pub chunks: usize,
    pub first_index: Option<u64>,
}

impl ChunkSink for Collect {
    fn accept(&mut self, chunk: Chunk<'_>) -> Result<(), ReelError> {
        assert_eq!(chunk.kind, ChunkKind::AudioSamples);
        assert_eq!(chunk.bytes.len() % 4, 0);
        self.first_index.get_or_insert(chunk.index);
        self.chunks += 1;
        self.samples.extend(
            chunk
                .bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        );
        Ok(())
    }
}

/// Decode a segment through the standard registry (provider found by magic bytes).
pub fn decode_segment(
    bytes: &[u8],
    req: &SegmentRequest,
    limits: &Limits,
) -> Result<(SegmentReceipt, Collect), ReelError> {
    let registry = Registry::standard().with_audio_codecs()?;
    let mut src = Cursor::new(bytes.to_vec());
    let provider = registry.detect(&mut src)?;
    let mut sink = Collect::default();
    let receipt = provider.decode_segment(
        &mut src,
        req,
        limits,
        &CancellationToken::default(),
        &mut sink,
    )?;
    Ok((receipt, sink))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Deterministic pseudo-random sequence (xorshift), for synthetic signals.
pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Signal-to-noise ratio of `got` against `reference` in dB (over the common length).
pub fn snr_db(reference: &[f32], got: &[f32]) -> f64 {
    let n = reference.len().min(got.len());
    let (mut sig, mut noise) = (0f64, 0f64);
    for i in 0..n {
        let r = f64::from(reference[i]);
        let d = r - f64::from(got[i]);
        sig += r * r;
        noise += d * d;
    }
    if noise == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (sig / noise).log10()
}
