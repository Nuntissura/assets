//! Shared helpers of the audio import and batch tests (lane SMB-AUDIO-IMPORT-2): reel fixture paths and
//! a WAV writer that builds files from known samples (the tests' oracle is the sample values written).
#![allow(dead_code)]
use std::path::PathBuf;

pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../hsk-studio-reel/tests/fixtures/audio")
        .join(name)
}

/// Sample encoding of a test WAV.
#[derive(Clone, Copy)]
pub enum WavFormat {
    Int16,
    Int24,
    Float32,
}

/// A RIFF/WAVE file of `interleaved` samples (values in [-1, 1); integers are `round(v * 2^(bits-1))`).
/// `mask`: WAVE_FORMAT_EXTENSIBLE with that channel mask; `None`: a plain fmt chunk.
pub fn wav(channels: u16, rate: u32, format: WavFormat, mask: Option<u32>, interleaved: &[f32]) -> Vec<u8> {
    let (tag, bits): (u16, u16) = match format {
        WavFormat::Int16 => (1, 16),
        WavFormat::Int24 => (1, 24),
        WavFormat::Float32 => (3, 32),
    };
    let mut data = Vec::new();
    for v in interleaved {
        match format {
            WavFormat::Int16 => data.extend_from_slice(&((v * 32768.0).round() as i16).to_le_bytes()),
            WavFormat::Int24 => data.extend_from_slice(&((v * 8_388_608.0).round() as i32).to_le_bytes()[..3]),
            WavFormat::Float32 => data.extend_from_slice(&v.to_le_bytes()),
        }
    }
    let block = channels * (bits / 8);
    let mut fmt = Vec::new();
    fmt.extend_from_slice(&(if mask.is_some() { 0xFFFEu16 } else { tag }).to_le_bytes());
    fmt.extend_from_slice(&channels.to_le_bytes());
    fmt.extend_from_slice(&rate.to_le_bytes());
    fmt.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
    fmt.extend_from_slice(&block.to_le_bytes());
    fmt.extend_from_slice(&bits.to_le_bytes());
    if let Some(mask) = mask {
        fmt.extend_from_slice(&22u16.to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        fmt.extend_from_slice(&mask.to_le_bytes());
        // Sub-format GUID: tag + the standard KSDATAFORMAT suffix.
        fmt.extend_from_slice(&tag.to_le_bytes());
        fmt.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71]);
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((4 + 8 + fmt.len() + 8 + data.len()) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    // An unknown chunk before fmt is skipped.
    out.extend_from_slice(b"junk");
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&[1, 2, 3, 0]);
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
    out.extend_from_slice(&fmt);
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    out
}

/// Deterministic test signal of `frames` x `channels`: channel c is a distinct sawtooth, |v| <= 0.25.
pub fn signal(frames: usize, channels: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        for c in 0..channels {
            let period = 37 + 11 * c;
            let phase = (i % period) as f32 / period as f32;
            // Multiples of 2^-15 so every format holds them exactly.
            out.push(((phase - 0.5) * 0.5 * 32768.0).round() / 32768.0);
        }
    }
    out
}

/// A scratch directory for one test under the cargo target tree.
pub fn scratch(test: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("audio-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
