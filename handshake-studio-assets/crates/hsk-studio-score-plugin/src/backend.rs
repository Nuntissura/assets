//! Worker-side backend port and the pure-Rust REFERENCE backend.
//!
//! A backend is the code that sits behind the framed protocol inside the worker process. The reference
//! backend is a gain-and-delay processor: it exercises protocol, isolation, timeout and reclaim, and does not
//! load or run the approved plugin file (it only verifies its SHA-256). The native CLAP backend
//! (feature `clap`, `clap_backend.rs`) is the only code that loads foreign plugin code.
#![deny(clippy::arithmetic_side_effects)]
use crate::hashing;
#[cfg(feature = "clap")]
use crate::staging::StageError;
use crate::types::{Arch, Completeness, PluginFormat, SUPPORTED_CLAP_ABI, validate_sample_rate};
use crate::wire::{
    ConfigureSpec, E_ABI, E_BUDGET, E_CONFIG, E_HASH, E_NOT_CONFIGURED, E_STALE, E_UNAVAILABLE,
    PluginSpec,
};
use hsk_studio_score::{MAX_BLOCK_FRAMES, MAX_CHANNELS};

/// Identity the reference backend reports in HELLO and receipts.
pub const REFERENCE_IDENTITY: &str = "reference-gain-delay@1";
pub(crate) const MAX_PLUGIN_FILE_BYTES: u64 = 512 << 20;

/// A typed refusal: worker error code and a detail number (for example the current state version).
pub(crate) type Refusal = (u8, u64);

pub(crate) struct ProbeInfo {
    pub plugin_id: String,
    pub name: String,
    pub format: PluginFormat,
    pub abi: u32,
    pub inputs: u16,
    pub outputs: u16,
}

pub(crate) struct Configured {
    pub latency: u32,
    pub version: u64,
}

pub(crate) struct Processed {
    pub version: u64,
    pub completeness: Completeness,
    pub latency: u32,
    pub samples: Vec<f32>,
}

pub(crate) struct Captured {
    pub version: u64,
    pub completeness: Completeness,
    pub bytes: Vec<u8>,
}

/// What sits behind the protocol in the worker. All methods run on the worker's single thread.
pub(crate) trait Backend {
    fn identity(&self) -> &'static str;
    /// CLAP ABI major version this backend speaks (0 = none).
    fn clap_abi(&self) -> u32;
    fn probe(&mut self, spec: &PluginSpec) -> Result<ProbeInfo, Refusal>;
    fn configure(&mut self, spec: &PluginSpec, cfg: &ConfigureSpec) -> Result<Configured, Refusal>;
    /// `samples` is planar (channel after channel); returns planar output of the same length.
    fn process(
        &mut self,
        expected_version: u64,
        start_sample: u64,
        frames: usize,
        channels: usize,
        samples: &[f32],
    ) -> Result<Processed, Refusal>;
    fn set_parameter(&mut self, param: u32, value: f64) -> Result<u64, Refusal>;
    fn capture_state(&mut self, max_bytes: u32) -> Result<Captured, Refusal>;
}

/// Format, architecture and ABI gate; nothing is read.
pub(crate) fn check_supported(spec: &PluginSpec) -> Result<(), Refusal> {
    let supported = PluginFormat::from_wire(spec.format) == Some(PluginFormat::Clap)
        && spec.abi == SUPPORTED_CLAP_ABI
        && Arch::host().map(Arch::wire) == Some(spec.arch);
    if supported { Ok(()) } else { Err((E_ABI, 0)) }
}

/// The gate plus the content hash check of the REFERENCE backend, which only verifies the file and
/// never loads or executes it (so no staging copy is needed there). The file is streamed over one fixed
/// buffer (`hashing.rs`), not read whole.
pub(crate) fn verify_file(spec: &PluginSpec) -> Result<(), Refusal> {
    check_supported(spec)?;
    let mut file = std::fs::File::open(&spec.path).map_err(|_| (E_UNAVAILABLE, 0))?;
    let meta = file.metadata().map_err(|_| (E_UNAVAILABLE, 0))?;
    if !meta.is_file() || meta.len() > MAX_PLUGIN_FILE_BYTES {
        return Err((E_UNAVAILABLE, 0));
    }
    let (digest, _) = hashing::stream_sha256(&mut file, None, MAX_PLUGIN_FILE_BYTES)
        .map_err(|_| (E_UNAVAILABLE, 0))?;
    if digest == spec.sha {
        Ok(())
    } else {
        Err((E_HASH, 0))
    }
}

/// Worker refusal for a failed staging copy.
#[cfg(feature = "clap")]
pub(crate) fn refusal_of_stage(error: StageError) -> Refusal {
    match error {
        StageError::Unavailable => (E_UNAVAILABLE, 0),
        StageError::Budget => (E_BUDGET, 0),
        StageError::Hash => (E_HASH, 0),
    }
}

/// Parameter 0 of the reference backend: gain in -16..16.
fn gain_ok(value: f64) -> bool {
    value.is_finite() && (-16.0..=16.0).contains(&value)
}

/// The configured reference processor: `out[n] = in[n - latency] * gain`, history carried across blocks.
struct Active {
    channels: usize,
    max_frames: usize,
    gain: f64,
    version: u64,
    delay: Vec<Vec<f32>>,
}

impl Active {
    /// `frames` is non-zero and `samples` holds `channels * frames` samples (the caller checked both).
    fn process(&mut self, frames: usize, samples: &[f32]) -> Vec<f32> {
        let gain = self.gain as f32;
        let latency = self.delay.first().map_or(0, Vec::len);
        let mut out = vec![0.0_f32; samples.len()];
        for ((history, input), dst) in self
            .delay
            .iter_mut()
            .zip(samples.chunks_exact(frames))
            .zip(out.chunks_exact_mut(frames))
        {
            for (i, slot) in dst.iter_mut().enumerate() {
                let source = match i.checked_sub(latency) {
                    None => history.get(i).copied(),
                    Some(j) => input.get(j).copied(),
                };
                *slot = source.unwrap_or(0.0) * gain;
            }
            if latency > 0 {
                match frames.checked_sub(latency) {
                    Some(start) => {
                        if let Some(tail) = input.get(start..) {
                            history.copy_from_slice(tail);
                        }
                    }
                    None => {
                        history.rotate_left(frames);
                        let from = latency.saturating_sub(frames);
                        if let Some(room) = history.get_mut(from..) {
                            room.copy_from_slice(input);
                        }
                    }
                }
            }
        }
        out
    }
}

/// Pure-Rust gain-and-delay backend (parameter 0 = gain, state = gain as f64).
pub(crate) struct Reference {
    latency: u32,
    completeness: Completeness,
    active: Option<Active>,
}

impl Reference {
    pub(crate) fn new(latency: u32, completeness: Completeness) -> Self {
        Self {
            latency,
            completeness,
            active: None,
        }
    }
}

impl Backend for Reference {
    fn identity(&self) -> &'static str {
        REFERENCE_IDENTITY
    }

    fn clap_abi(&self) -> u32 {
        SUPPORTED_CLAP_ABI
    }

    fn probe(&mut self, spec: &PluginSpec) -> Result<ProbeInfo, Refusal> {
        verify_file(spec)?;
        Ok(ProbeInfo {
            plugin_id: "hsk.reference.gain-delay".to_owned(),
            name: "Reference gain and delay (plugin file verified, not loaded)".to_owned(),
            format: PluginFormat::Clap,
            abi: SUPPORTED_CLAP_ABI,
            inputs: 2,
            outputs: 2,
        })
    }

    fn configure(&mut self, spec: &PluginSpec, cfg: &ConfigureSpec) -> Result<Configured, Refusal> {
        verify_file(spec)?;
        let channels = usize::from(cfg.channels);
        let max_frames = usize::try_from(cfg.max_frames).unwrap_or(usize::MAX);
        if channels == 0
            || channels > MAX_CHANNELS
            || max_frames == 0
            || max_frames > MAX_BLOCK_FRAMES
        {
            return Err((E_CONFIG, 0));
        }
        validate_sample_rate(cfg.sample_rate_hz).map_err(|_| (E_CONFIG, 0))?;
        let mut gain = 1.0;
        for (param, value) in &cfg.params {
            if *param != 0 || !gain_ok(*value) {
                return Err((E_CONFIG, u64::from(*param)));
            }
            gain = *value;
        }
        let latency = usize::try_from(self.latency).unwrap_or(0);
        self.active = Some(Active {
            channels,
            max_frames,
            gain,
            version: 1,
            delay: vec![vec![0.0; latency]; channels],
        });
        Ok(Configured {
            latency: self.latency,
            version: 1,
        })
    }

    fn process(
        &mut self,
        expected_version: u64,
        _start_sample: u64,
        frames: usize,
        channels: usize,
        samples: &[f32],
    ) -> Result<Processed, Refusal> {
        let (completeness, latency) = (self.completeness, self.latency);
        let active = self.active.as_mut().ok_or((E_NOT_CONFIGURED, 0))?;
        if expected_version != active.version {
            return Err((E_STALE, active.version));
        }
        let expected_samples = channels.checked_mul(frames);
        if frames == 0 || expected_samples != Some(samples.len()) {
            return Err((E_CONFIG, 0));
        }
        if channels != active.channels || frames > active.max_frames {
            return Err((E_BUDGET, u64::try_from(frames).unwrap_or(u64::MAX)));
        }
        Ok(Processed {
            version: active.version,
            completeness,
            latency,
            samples: active.process(frames, samples),
        })
    }

    fn set_parameter(&mut self, param: u32, value: f64) -> Result<u64, Refusal> {
        let active = self.active.as_mut().ok_or((E_NOT_CONFIGURED, 0))?;
        if param != 0 || !gain_ok(value) {
            return Err((E_CONFIG, u64::from(param)));
        }
        active.gain = value;
        active.version = active.version.checked_add(1).ok_or((E_CONFIG, 0))?;
        Ok(active.version)
    }

    fn capture_state(&mut self, max_bytes: u32) -> Result<Captured, Refusal> {
        let completeness = self.completeness;
        let active = self.active.as_ref().ok_or((E_NOT_CONFIGURED, 0))?;
        let bytes = match completeness {
            Completeness::Complete => active.gain.to_le_bytes().to_vec(),
            Completeness::Partial => (active.gain as f32).to_le_bytes().to_vec(),
            Completeness::Unavailable => Vec::new(),
        };
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if len > u64::from(max_bytes) {
            return Err((E_BUDGET, len));
        }
        Ok(Captured {
            version: active.version,
            completeness,
            bytes,
        })
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)] // test code
mod tests {
    use super::*;
    use hsk_studio_score::sha256;

    #[test]
    fn reference_verify_streams_a_large_file_in_bounded_memory() {
        let len = 48_usize << 20;
        let bytes: Vec<u8> = (0..len)
            .map(|i| u8::try_from((i.wrapping_mul(97) ^ (i >> 11)) % 251).unwrap_or(0))
            .collect();
        let path = std::env::temp_dir().join(format!(
            "hsk-score-plugin-backend-{}.bin",
            std::process::id()
        ));
        std::fs::write(&path, &bytes).unwrap();
        let spec = |sha: [u8; 32]| PluginSpec {
            format: PluginFormat::Clap.wire(),
            arch: Arch::host().map_or(0, Arch::wire),
            abi: SUPPORTED_CLAP_ABI,
            sha,
            path: path.to_str().unwrap().to_owned(),
        };
        let honest = spec(sha256(&bytes));
        let wrong = spec(sha256(b"other"));
        drop(bytes);
        let (ok, peak) = crate::hashing::meter::peak_of(|| verify_file(&honest));
        let (bad, bad_peak) = crate::hashing::meter::peak_of(|| verify_file(&wrong));
        let _ = std::fs::remove_file(&path);
        assert_eq!(ok, Ok(()));
        assert_eq!(bad, Err((E_HASH, 0)));
        assert!(peak < 4 << 20, "peak {peak} bytes for a {len} byte file");
        assert!(bad_peak < 4 << 20, "peak {bad_peak} on the mismatch path");
    }
}
