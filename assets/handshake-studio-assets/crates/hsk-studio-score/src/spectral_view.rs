//! Spectral frequency display surface (lane SMB-AU-WAVEFORM-EDITOR; parity rows AU-12 display and
//! AU-20): the [`Spectrogram`] magnitude grid mapped to display rows on a linear or logarithmic
//! frequency axis, in dB, computed per tile of [`SPECTRAL_TILE_FRAMES`] frames and kept in a
//! memory-bounded LRU cache.
//!
//! **Rows.** Row `r` of `R` (row 0 = lowest frequency) covers the frequency interval
//! `[edge(r), edge(r + 1)]` with `edge(t) = t / R * nyquist` (linear) or
//! `edge(t) = min_hz * (nyquist / min_hz)^(t / R)` (logarithmic; row 0 starts at `min_hz`). A row's
//! value is the largest magnitude of the bins whose centre frequency lies in its interval; a row
//! narrower than the bin spacing holding no bin centre reads the magnitude linearly interpolated
//! between the two bins around its centre frequency `edge(r + 0.5)`. Cell value
//! `20 log10(max(m, 10^(floor_db/20)))` (so every cell is at least `floor_db`); `m` is score's
//! calibrated magnitude (a bin-centred sine of amplitude `A` reads `A`, `20 log10 A` dB).
//! [`frequency_at_row`] and [`row_at_frequency`] convert between the two (a selection drawn on the
//! display maps to Hz exactly).
//!
//! **Tiles and cache.** Tile `t` holds frames `t * 64 .. min((t + 1) * 64, frames)` as
//! `frames x rows` values (frame-major), computed from the samples the tile reads only; equal to
//! the same frames of a whole grid (Spectrogram tile property). [`SpectralTileCache`] keeps tiles up
//! to a byte budget (least recently used evicted first), counts hits, misses and evictions, and is
//! invalidated by the owner when the audio changes. Audition display parity (colour map, scale
//! breakpoints) is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]

use crate::error::ScoreError;
use crate::spectrogram::{SpectralWindow, Spectrogram, SpectrogramParams};
use hsk_studio_accord::CancellationToken;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

/// Frames per display tile.
pub const SPECTRAL_TILE_FRAMES: usize = 64;
/// Display row range.
pub const SPECTRAL_ROWS: (usize, usize) = (8, 4096);
/// Floor range in dB.
pub const SPECTRAL_FLOOR_DB: (f64, f64) = (-240.0, -20.0);
/// Default cache budget (bytes of tile values).
pub const DEFAULT_SPECTRAL_CACHE_BYTES: usize = 64 << 20;

/// Frequency axis of the display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FrequencyScale {
    Linear,
    /// Logarithmic from `min_hz` (> 0, below Nyquist) to Nyquist.
    Log { min_hz: f64 },
}

/// Display parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpectralViewParams {
    pub fft_size: usize,
    pub hop: usize,
    pub window: SpectralWindow,
    pub scale: FrequencyScale,
    pub rows: usize,
    pub floor_db: f64,
}

impl SpectralViewParams {
    /// Checks rows, floor and scale against `nyquist`.
    fn check(&self, nyquist: f64) -> Result<(), ScoreError> {
        if !self.floor_db.is_finite() {
            return Err(ScoreError::NotFinite);
        }
        if !(SPECTRAL_ROWS.0..=SPECTRAL_ROWS.1).contains(&self.rows) || !(SPECTRAL_FLOOR_DB.0..=SPECTRAL_FLOOR_DB.1).contains(&self.floor_db) {
            return Err(ScoreError::OutOfRange);
        }
        if let FrequencyScale::Log { min_hz } = self.scale {
            if !min_hz.is_finite() {
                return Err(ScoreError::NotFinite);
            }
            if min_hz <= 0.0 || min_hz >= nyquist {
                return Err(ScoreError::OutOfRange);
            }
        }
        Ok(())
    }
}

/// Frequency (Hz) at fractional row position `t` (`0 ..= rows`) of the axis.
pub fn frequency_at_row(scale: FrequencyScale, t: f64, rows: usize, nyquist: f64) -> f64 {
    let p = t / rows as f64;
    match scale {
        FrequencyScale::Linear => p * nyquist,
        FrequencyScale::Log { min_hz } => min_hz * (nyquist / min_hz).powf(p),
    }
}

/// Fractional row position of `hz` (inverse of [`frequency_at_row`]; below the log axis start it is
/// negative).
pub fn row_at_frequency(scale: FrequencyScale, hz: f64, rows: usize, nyquist: f64) -> f64 {
    let p = match scale {
        FrequencyScale::Linear => hz / nyquist,
        FrequencyScale::Log { min_hz } => (hz / min_hz).ln() / (nyquist / min_hz).ln(),
    };
    p * rows as f64
}

/// Maps one magnitude frame (`bins` values) to display rows (module docs).
pub fn frame_to_rows(magnitudes: &[f32], params: &SpectralViewParams, sample_rate_hz: u32, out: &mut [f32]) {
    let bins = magnitudes.len();
    let nyquist = f64::from(sample_rate_hz) / 2.0;
    let bin_hz = nyquist / bins.saturating_sub(1).max(1) as f64;
    let floor = 10f64.powf(params.floor_db / 20.0);
    let last = bins.saturating_sub(1);
    for (r, cell) in out.iter_mut().enumerate().take(params.rows) {
        let lo = frequency_at_row(params.scale, r as f64, params.rows, nyquist);
        let hi = frequency_at_row(params.scale, r as f64 + 1.0, params.rows, nyquist);
        let first = (lo / bin_hz).ceil().max(0.0) as usize;
        let past = ((hi / bin_hz).floor().max(0.0) as usize).min(last);
        let m = if first <= past && first <= last {
            magnitudes[first..=past].iter().fold(0.0_f64, |m, v| m.max(f64::from(*v)))
        } else {
            let centre = frequency_at_row(params.scale, r as f64 + 0.5, params.rows, nyquist) / bin_hz;
            let b0 = (centre.floor().max(0.0) as usize).min(last);
            let b1 = b0.saturating_add(1).min(last);
            let frac = (centre - b0 as f64).clamp(0.0, 1.0);
            f64::from(magnitudes[b0]) * (1.0 - frac) + f64::from(magnitudes[b1]) * frac
        };
        *cell = (20.0 * m.max(floor).log10()) as f32;
    }
}

/// Cache statistics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpectralCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub bytes: usize,
    pub tiles: usize,
}

/// Display tiles of one channel of `len` samples (module docs).
pub struct SpectralTileCache {
    spectrogram: Spectrogram,
    params: SpectralViewParams,
    len: u64,
    budget: usize,
    tiles: BTreeMap<u64, Arc<Vec<f32>>>,
    order: VecDeque<u64>,
    magnitudes: Vec<f32>,
    stats: SpectralCacheStats,
}

impl SpectralTileCache {
    /// Refusals: as [`Spectrogram::with_window`]; rows outside [`SPECTRAL_ROWS`], floor outside
    /// [`SPECTRAL_FLOOR_DB`], a log start not in `(0, nyquist)` -> `OutOfRange`; NaN/inf -> `NotFinite`;
    /// a budget below one tile -> `CapacityExceeded`.
    pub fn new(sample_rate_hz: u32, params: SpectralViewParams, len: u64, budget_bytes: usize) -> Result<Self, ScoreError> {
        let spectrogram = Spectrogram::with_window(sample_rate_hz, SpectrogramParams { fft_size: params.fft_size, hop: params.hop }, params.window)?;
        params.check(f64::from(sample_rate_hz) / 2.0)?;
        let tile_bytes = SPECTRAL_TILE_FRAMES.checked_mul(params.rows).and_then(|c| c.checked_mul(4)).ok_or(ScoreError::CapacityExceeded)?;
        if budget_bytes < tile_bytes {
            return Err(ScoreError::CapacityExceeded);
        }
        let cells = SPECTRAL_TILE_FRAMES.checked_mul(spectrogram.bins()).ok_or(ScoreError::CapacityExceeded)?;
        Ok(Self { spectrogram, params, len, budget: budget_bytes, tiles: BTreeMap::new(), order: VecDeque::new(), magnitudes: vec![0.0; cells], stats: SpectralCacheStats::default() })
    }
    pub fn params(&self) -> &SpectralViewParams {
        &self.params
    }
    pub fn frames(&self) -> u64 {
        self.spectrogram.frame_count(self.len)
    }
    pub fn tile_count(&self) -> u64 {
        self.frames().div_ceil(SPECTRAL_TILE_FRAMES as u64)
    }
    pub fn hop(&self) -> usize {
        self.params.hop
    }
    pub fn stats(&self) -> SpectralCacheStats {
        self.stats
    }
    /// Drops every tile (the audio changed); `len` is the new length.
    pub fn invalidate(&mut self, len: u64) {
        self.len = len;
        self.tiles.clear();
        self.order.clear();
        self.stats.bytes = 0;
        self.stats.tiles = 0;
    }

    /// Tile `index` (`frames x rows` dB values, frame-major) of `audio` (the whole channel, `len`
    /// samples). Refusals: tile past the end -> `OutOfRange`; audio length differing from `len` ->
    /// `LengthMismatch`; NaN/inf audio -> `NotFinite`; cancel -> `Canceled`.
    pub fn tile(&mut self, index: u64, audio: &[f32], cancel: &CancellationToken) -> Result<Arc<Vec<f32>>, ScoreError> {
        if audio.len() as u64 != self.len {
            return Err(ScoreError::LengthMismatch);
        }
        if index >= self.tile_count() {
            return Err(ScoreError::OutOfRange);
        }
        if let Some(t) = self.tiles.get(&index).cloned() {
            self.stats.hits = self.stats.hits.saturating_add(1);
            self.order.retain(|i| *i != index);
            self.order.push_back(index);
            return Ok(t);
        }
        cancel.check().map_err(|_| ScoreError::Canceled)?;
        let first = index.checked_mul(SPECTRAL_TILE_FRAMES as u64).ok_or(ScoreError::OutOfRange)?;
        let count = (SPECTRAL_TILE_FRAMES as u64).min(self.frames().saturating_sub(first));
        let count_us = usize::try_from(count).map_err(|_| ScoreError::CapacityExceeded)?;
        let bins = self.spectrogram.bins();
        let cells = count_us.checked_mul(bins).ok_or(ScoreError::CapacityExceeded)?;
        let (start, end) = self.spectrogram.tile_input_range(self.len, first, count);
        let (s, e) = (usize::try_from(start).map_err(|_| ScoreError::CapacityExceeded)?, usize::try_from(end).map_err(|_| ScoreError::CapacityExceeded)?);
        self.spectrogram.tile(&audio[s..e], start, self.len, first, count_us, &mut self.magnitudes[..cells])?;
        let rows = self.params.rows;
        let mut values = vec![0.0_f32; count_us.checked_mul(rows).ok_or(ScoreError::CapacityExceeded)?];
        let rate = self.spectrogram.sample_rate_hz();
        for (frame, out) in self.magnitudes[..cells].chunks(bins).zip(values.chunks_mut(rows)) {
            frame_to_rows(frame, &self.params, rate, out);
        }
        let bytes = values.len().saturating_mul(4);
        while self.stats.bytes.saturating_add(bytes) > self.budget {
            let Some(old) = self.order.pop_front() else { break };
            if let Some(t) = self.tiles.remove(&old) {
                self.stats.bytes = self.stats.bytes.saturating_sub(t.len().saturating_mul(4));
                self.stats.evictions = self.stats.evictions.saturating_add(1);
            }
        }
        let tile = Arc::new(values);
        self.tiles.insert(index, tile.clone());
        self.order.push_back(index);
        self.stats.bytes = self.stats.bytes.saturating_add(bytes);
        self.stats.misses = self.stats.misses.saturating_add(1);
        self.stats.tiles = self.tiles.len();
        Ok(tile)
    }
}
