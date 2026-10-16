//! Spectral frequency display of the waveform editor (lane SMB-AU-WAVEFORM-EDITOR slice 2, parity
//! row AU-12 display; feature `audio-batch`). Score's [`SpectralTileCache`] (STFT with a chosen
//! window, linear or logarithmic frequency rows in dB, tiles of 64 frames in a memory-bounded LRU
//! cache) per channel of the editor's current audio; the caches are invalidated whenever the edit
//! list changes (key: SHA-256 of the stored list) or the parameters change, so undo / redo show the
//! matching spectrum.
//!
//! [`super::Viewer::spectral_image`] renders a channel as an RGBA image of `width` columns (each the
//! per-row maximum over its frames) by `rows` (top row = highest frequency), dB mapped from the floor
//! (black) to 0 dB (white) through a fixed ramp; the panel `spectral.panel` shows it as a texture
//! with window, scale and FFT-size controls. Audition's colour map and default settings are
//! UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]

use hsk_studio_score::{self as score, FrequencyScale, SpectralCacheStats, SpectralTileCache, SpectralViewParams, SpectralWindow};
use serde_json::{Value, json};
use std::sync::Arc;

/// Widget names of the spectral panel.
pub mod widget {
    pub const PANEL: &str = "spectral.panel";
    pub const IMAGE: &str = "spectral.image";
    pub const WINDOW: &str = "spectral.window";
    pub const SCALE: &str = "spectral.scale";
    pub const FFT: &str = "spectral.fft";
}

/// Widest rendered image (columns).
pub const MAX_SPECTRAL_IMAGE_WIDTH: usize = 4096;

/// Default display parameters: Hann 2048 / 512, log rows from 20 Hz, 256 rows, floor -120 dB.
pub fn default_params() -> SpectralViewParams {
    SpectralViewParams { fft_size: 2048, hop: 512, window: SpectralWindow::Hann, scale: FrequencyScale::Log { min_hz: 20.0 }, rows: 256, floor_db: -120.0 }
}

/// The display bound to the waveform editor.
pub struct SpectralDisplayState {
    params: SpectralViewParams,
    caches: Vec<SpectralTileCache>,
    key: Option<[u8; 32]>,
    texture: Option<(egui::TextureHandle, [u8; 32])>,
    budget: usize,
    error: Option<String>,
}

fn window_of(name: &str) -> Option<SpectralWindow> {
    [SpectralWindow::Hann, SpectralWindow::Hamming, SpectralWindow::Blackman, SpectralWindow::BlackmanHarris, SpectralWindow::Rectangular]
        .into_iter()
        .find(|w| w.name() == name)
}

/// Colour of a normalised level `t` in 0..=1 (black, blue, red, yellow, white).
pub fn ramp(t: f32) -> [u8; 4] {
    const STOPS: [[f32; 3]; 5] = [[0.0, 0.0, 0.0], [0.1, 0.1, 0.6], [0.8, 0.1, 0.2], [1.0, 0.85, 0.1], [1.0, 1.0, 1.0]];
    let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
    let x = t * 4.0;
    let i = (x.floor() as usize).min(3);
    let f = x - i as f32;
    let (a, b) = (STOPS[i], STOPS[i.saturating_add(1)]);
    let c = |k: usize| ((a[k] + (b[k] - a[k]) * f) * 255.0).round() as u8;
    [c(0), c(1), c(2), 255]
}

impl super::Viewer {
    /// Opens the spectral display on the waveform editor's audio with `params` (refused when the
    /// waveform editor is closed or the parameters are refused by score).
    pub fn open_spectral_display(&mut self, params: SpectralViewParams) -> Result<(), String> {
        self.waveform_document()?;
        self.waveform_buffer()?;
        self.spectral = Some(SpectralDisplayState { params, caches: Vec::new(), key: None, texture: None, budget: score::DEFAULT_SPECTRAL_CACHE_BYTES, error: None });
        self.spectral_sync()
    }

    /// Changes the display parameters (caches rebuilt).
    pub fn set_spectral_params(&mut self, params: SpectralViewParams) -> Result<(), String> {
        let s = self.spectral.as_mut().ok_or("spectral: not open")?;
        s.params = params;
        s.key = None;
        self.spectral_sync()
    }

    fn spectral_key(&self) -> Result<[u8; 32], String> {
        let s = self.spectral.as_ref().ok_or("spectral: not open")?;
        let mut bytes = self.waveform_document()?.map(|d| d.to_bytes()).unwrap_or_default();
        bytes.extend_from_slice(format!("{:?}", s.params).as_bytes());
        Ok(score::sha256(&bytes))
    }

    /// Rebuilds the caches when the edit list or the parameters changed.
    fn spectral_sync(&mut self) -> Result<(), String> {
        let key = self.spectral_key()?;
        if self.spectral.as_ref().is_some_and(|s| s.key == Some(key)) {
            return Ok(());
        }
        let buffer = self.waveform_buffer()?;
        let s = self.spectral.as_mut().ok_or("spectral: not open")?;
        let built: Result<Vec<SpectralTileCache>, _> = (0..buffer.channel_count())
            .map(|_| SpectralTileCache::new(buffer.sample_rate_hz(), s.params, buffer.frames(), s.budget))
            .collect();
        match built {
            Ok(c) => {
                s.caches = c;
                s.key = Some(key);
                s.texture = None;
                s.error = None;
                Ok(())
            }
            Err(e) => {
                s.caches.clear();
                s.key = None;
                let text = format!("spectral_{}", e.code());
                s.error = Some(text.clone());
                Err(text)
            }
        }
    }

    /// Tile `index` of `channel` (frames x rows dB, frame-major).
    pub fn spectral_tile(&mut self, channel: usize, index: u64) -> Result<Arc<Vec<f32>>, String> {
        self.spectral_sync()?;
        let buffer = self.waveform_buffer()?;
        let audio = buffer.channels().get(channel).ok_or("spectral: no such channel")?;
        let cancel = self.cancel;
        let s = self.spectral.as_mut().ok_or("spectral: not open")?;
        let cache = s.caches.get_mut(channel).ok_or("spectral: no such channel")?;
        cache.tile(index, audio, cancel).map_err(|e| format!("spectral_{}", e.code()))
    }

    /// `width` columns x `rows` of `channel` as dB (row 0 = highest frequency), each column the
    /// per-row maximum over its frames.
    pub fn spectral_columns(&mut self, channel: usize, width: usize) -> Result<(usize, usize, Vec<f32>), String> {
        self.spectral_sync()?;
        let (frames, tiles, rows, floor) = {
            let s = self.spectral.as_ref().ok_or("spectral: not open")?;
            let c = s.caches.get(channel).ok_or("spectral: no such channel")?;
            (c.frames(), c.tile_count(), s.params.rows, s.params.floor_db as f32)
        };
        if width == 0 || width > MAX_SPECTRAL_IMAGE_WIDTH || frames == 0 {
            return Err("spectral_out_of_range".into());
        }
        let width = width.min(usize::try_from(frames).unwrap_or(usize::MAX));
        let cells = width.checked_mul(rows).ok_or("spectral_capacity_exceeded")?;
        let mut out = vec![floor; cells];
        let per_tile = score::SPECTRAL_TILE_FRAMES as u64;
        for t in 0..tiles {
            let tile = self.spectral_tile(channel, t)?;
            for (j, frame) in tile.chunks(rows).enumerate() {
                let k = t.saturating_mul(per_tile).saturating_add(j as u64);
                let col = usize::try_from(k.saturating_mul(width as u64).checked_div(frames).unwrap_or(0)).unwrap_or(0).min(width.saturating_sub(1));
                for (r, v) in frame.iter().enumerate() {
                    let row = rows.saturating_sub(1).saturating_sub(r);
                    if let Some(cell) = out.get_mut(row.saturating_mul(width).saturating_add(col)) {
                        *cell = cell.max(*v);
                    }
                }
            }
        }
        Ok((width, rows, out))
    }

    /// RGBA image of [`spectral_columns`](Self::spectral_columns) through [`ramp`].
    pub fn spectral_image(&mut self, channel: usize, width: usize) -> Result<(usize, usize, Vec<u8>), String> {
        let floor = self.spectral.as_ref().ok_or("spectral: not open")?.params.floor_db as f32;
        let (w, h, db) = self.spectral_columns(channel, width)?;
        let rgba = db.iter().flat_map(|v| ramp((v - floor) / -floor)).collect();
        Ok((w, h, rgba))
    }

    /// Structured projection (`Null` when closed).
    pub fn spectral_json(&self) -> Value {
        let Some(s) = &self.spectral else { return Value::Null };
        let p = s.params;
        let stats: Vec<SpectralCacheStats> = s.caches.iter().map(SpectralTileCache::stats).collect();
        json!({
            "fft_size": p.fft_size, "hop": p.hop, "window": p.window.name(), "rows": p.rows, "floor_db": p.floor_db,
            "scale": match p.scale { FrequencyScale::Linear => json!("linear"), FrequencyScale::Log { min_hz } => json!({"log_min_hz": min_hz}) },
            "frames": s.caches.first().map(SpectralTileCache::frames),
            "tiles": s.caches.first().map(SpectralTileCache::tile_count),
            "cache": stats.iter().map(|c| json!({"hits": c.hits, "misses": c.misses, "evictions": c.evictions, "bytes": c.bytes, "tiles": c.tiles})).collect::<Vec<_>>(),
            "error": s.error,
        })
    }

    /// The spectral panel (while the display is open).
    pub(crate) fn spectral_ui(&mut self, ctx: &egui::Context) {
        if self.spectral.is_none() {
            return;
        }
        let _ = self.spectral_sync();
        let Some(key) = self.spectral.as_ref().and_then(|s| s.key) else { return };
        if self.spectral.as_ref().is_some_and(|s| s.texture.as_ref().is_none_or(|(_, k)| *k != key))
            && let Ok((w, h, rgba)) = self.spectral_image(0, 1024)
        {
            let image = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
            let handle = ctx.load_texture(widget::IMAGE, image, egui::TextureOptions::NEAREST);
            if let Some(s) = self.spectral.as_mut() {
                s.texture = Some((handle, key));
            }
        }
        let Some(s) = self.spectral.as_mut() else { return };
        let mut params = s.params;
        egui::TopBottomPanel::bottom(super::widget::id(widget::PANEL)).resizable(true).min_height(140.0).show(ctx, |ui| {
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt(super::widget::id(widget::WINDOW)).selected_text(params.window.name()).show_ui(ui, |ui| {
                    for name in ["hann", "hamming", "blackman", "blackman_harris", "rectangular"] {
                        if let Some(w) = window_of(name) {
                            ui.selectable_value(&mut params.window, w, name);
                        }
                    }
                });
                let log = matches!(params.scale, FrequencyScale::Log { .. });
                egui::ComboBox::from_id_salt(super::widget::id(widget::SCALE)).selected_text(if log { "log" } else { "linear" }).show_ui(ui, |ui| {
                    ui.selectable_value(&mut params.scale, FrequencyScale::Linear, "linear");
                    ui.selectable_value(&mut params.scale, FrequencyScale::Log { min_hz: 20.0 }, "log");
                });
                egui::ComboBox::from_id_salt(super::widget::id(widget::FFT)).selected_text(params.fft_size.to_string()).show_ui(ui, |ui| {
                    for n in [512usize, 1024, 2048, 4096, 8192] {
                        if ui.selectable_value(&mut params.fft_size, n, n.to_string()).changed() {
                            params.hop = n.checked_div(4).unwrap_or(n);
                        }
                    }
                });
            });
            if let Some((t, _)) = &s.texture {
                let size = egui::vec2(ui.available_width().max(64.0), ui.available_height().max(64.0));
                ui.push_id(super::widget::id(widget::IMAGE), |ui| ui.image((t.id(), size)));
            } else if let Some(e) = &s.error {
                ui.label(e);
            }
        });
        self.drawn.extend([widget::PANEL, widget::WINDOW, widget::SCALE, widget::FFT, widget::IMAGE].map(String::from));
        if self.spectral.as_ref().is_some_and(|s| s.params != params) {
            let _ = self.set_spectral_params(params);
        }
    }
}
