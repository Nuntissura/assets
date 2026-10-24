//! Gamut warning (STU-COL-244, STU-COL-138) and the in-gamut substitute (STU-COL-245).
//!
//! The warning marks every document pixel the proof device cannot reproduce and is queryable as structured
//! state: the per-pixel mask, the out-of-gamut area fraction and a bounded list of bounding regions
//! (8-connected components, at most `MAX_GAMUT_REGIONS`, raster order of their first pixel), with the
//! configurable warning colour and `gamut_warning.opacity` (0..=100 percent, default 100, integer: the
//! STU-COL-244 parameter table row). How a pixel is judged (Studio readings, listed as lane spec deltas):
//! - matrix/Gray proof devices: the clipping report of STU-COL-138, i.e. the linear device values of the
//!   document -> proof device stage outside 0..1 by more than `GAMUT_LINEAR_TOLERANCE` (the same test and
//!   tolerance as `ClipReport`, tests/soft_proof.rs `gamut_mask_marks_exactly_the_clipped_pixels`);
//! - table (CMYK) proof devices, whose clamped output cannot reveal clipping: the round trip document -> PCS
//!   -> proof device -> PCS, out of gamut when CIE76 delta E exceeds `GAMUT_ROUND_TRIP_DELTA_E76` (STU-COL-244
//!   declares no tolerance; 5 is the LittleCMS 2 `cmsgmt.c` gamut-check threshold, used as a cross-reference,
//!   no code copied; Photoshop's threshold UNVERIFIED) (`table_destination_gamut_check_uses_round_trip`);
//! - perceptual and saturation proof intents map with the profile's own tables and never clip (STU-COL-138),
//!   so the check runs under relative colorimetric; the colorimetric intents check themselves.
//! The default warning colour is mid grey (0.5, 0.5, 0.5) in display RGB (STU-COL-244 declares no default;
//! Photoshop's default UNVERIFIED).
//!
//! The substitute is an explicit call, never applied by `run` or the warning: the document colour rendered by
//! the proof device under the ACTIVE `proof_intent` (device values clamped to the device range), converted
//! back to the document space (relative colorimetric, no black point compensation, clamped to 0..1), with the
//! CIE76 delta E between the two document colours (`substitute_is_in_gamut_and_reports_delta`).
#![deny(clippy::arithmetic_side_effects)] // V4-01: hostile sizes, explicit checked/saturating/wrapping math
use super::ProofError;
use super::chain::{Managed, Route, SoftProof, check_pixels};
use crate::Intent;
use crate::icc_eval::{D50, xyz_to_lab};
use crate::wide::{CHUNK_PIXELS, ClipMethod, WideError, canceled};
use hsk_studio_accord::CancellationToken;

pub const GAMUT_WARNING_OPACITY_MIN: u32 = 0;
pub const GAMUT_WARNING_OPACITY_MAX: u32 = 100;
pub const GAMUT_WARNING_OPACITY_DEFAULT: u32 = 100;
pub const DEFAULT_GAMUT_WARNING_COLOUR: [f32; 3] = [0.5, 0.5, 0.5];
/// Linear device tolerance of matrix/Gray proof devices (the `ClipReport` tolerance).
pub const GAMUT_LINEAR_TOLERANCE: f64 = 1e-3;
/// CIE76 round-trip threshold of table proof devices.
pub const GAMUT_ROUND_TRIP_DELTA_E76: f64 = 5.0;
/// Most bounding regions listed; `GamutWarning::region_count` keeps counting past it.
pub const MAX_GAMUT_REGIONS: usize = 256;

/// Warning colour (display RGB, 0..1) and opacity (integer percent, 0..=100).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GamutWarningStyle {
    colour: [f32; 3],
    opacity_percent: u32,
}
impl GamutWarningStyle {
    pub fn new(colour: [f32; 3], opacity_percent: u32) -> Result<Self, ProofError> {
        if colour.iter().any(|c| !c.is_finite() || !(0.0..=1.0).contains(c)) {
            return Err(ProofError::WarningColourRange);
        }
        if !(GAMUT_WARNING_OPACITY_MIN..=GAMUT_WARNING_OPACITY_MAX).contains(&opacity_percent) {
            return Err(ProofError::OpacityRange);
        }
        Ok(Self { colour, opacity_percent })
    }
    pub const fn colour(&self) -> [f32; 3] {
        self.colour
    }
    pub const fn opacity_percent(&self) -> u32 {
        self.opacity_percent
    }
}
impl Default for GamutWarningStyle {
    fn default() -> Self {
        Self {
            colour: DEFAULT_GAMUT_WARNING_COLOUR,
            opacity_percent: GAMUT_WARNING_OPACITY_DEFAULT,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GamutMethod {
    /// Matrix/Gray proof device: linear device values outside 0..1.
    LinearDevice,
    /// Table proof device: PCS round trip above the delta E threshold.
    RoundTrip,
    /// Legacy uncompensated RGB: no proof device, nothing is out of gamut.
    NotApplicable,
}
impl GamutMethod {
    pub const fn code(self) -> &'static str {
        match self {
            Self::LinearDevice => "linear_device",
            Self::RoundTrip => "round_trip",
            Self::NotApplicable => "not_applicable",
        }
    }
}

/// Inclusive pixel bounding box of one 8-connected out-of-gamut component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GamutRegion {
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
    pub pixels: u64,
}

/// Structured gamut-warning state (STU-COL-244; readable as numbers by a headless model, acc.gamut_coverage_readable).
#[derive(Clone, Debug, PartialEq)]
pub struct GamutWarning {
    pub method: GamutMethod,
    pub gamut_intent: Option<Intent>,
    /// The round-trip threshold in force (`None` for the linear method).
    pub threshold_delta_e76: Option<f64>,
    pub mask: Vec<bool>,
    pub out_of_gamut_pixels: u64,
    pub pixel_count: u64,
    /// `out_of_gamut_pixels / pixel_count`.
    pub area_fraction: f64,
    /// Bounding box of every marked pixel.
    pub bounds: Option<GamutRegion>,
    /// At most `MAX_GAMUT_REGIONS` components.
    pub regions: Vec<GamutRegion>,
    /// All components, listed or not.
    pub region_count: u64,
    pub regions_truncated: bool,
    /// Largest round-trip delta E over the image (round-trip method only).
    pub max_delta_e76: Option<f64>,
    pub width: usize,
    pub height: usize,
    pub style: GamutWarningStyle,
}
impl GamutWarning {
    /// The overlay on a display RGB preview of the same image: `(1 - a) * pixel + a * colour` on every marked
    /// pixel, `a = opacity / 100`; other pixels unchanged.
    pub fn apply_overlay(&self, display_rgb: &[f32]) -> Result<Vec<f32>, ProofError> {
        if Some(display_rgb.len()) != self.mask.len().checked_mul(3) {
            return Err(WideError::ChannelCount.into());
        }
        let a = self.style.opacity_percent as f32 / 100.0;
        let colour = self.style.colour;
        let mut out = display_rgb.to_vec();
        for (px, marked) in out.chunks_exact_mut(3).zip(&self.mask) {
            if *marked {
                for (v, c) in px.iter_mut().zip(colour) {
                    *v = (1.0 - a) * *v + a * c;
                }
            }
        }
        Ok(out)
    }
}

/// Result of the explicit in-gamut substitute call.
#[derive(Clone, Debug, PartialEq)]
pub struct GamutSubstitute {
    pub original: Vec<f32>,
    /// Document device values; equal to `original` when it was already in gamut.
    pub substitute: Vec<f32>,
    pub out_of_gamut: bool,
    /// The substitute passes the same gamut check.
    pub substitute_in_gamut: bool,
    /// CIE76 delta E (D50 PCS) between the original and the substitute.
    pub delta_e76: f64,
    /// The active intent the substitute was rendered under (`proof_intent`).
    pub intent: Intent,
}

fn delta_e76(a: [f64; 3], b: [f64; 3]) -> f64 {
    let la = xyz_to_lab([a[0] * D50[0], a[1] * D50[1], a[2] * D50[2]]);
    let lb = xyz_to_lab([b[0] * D50[0], b[1] * D50[1], b[2] * D50[2]]);
    let d = [la[0] - lb[0], la[1] - lb[1], la[2] - lb[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

impl Managed {
    fn method(&self) -> GamutMethod {
        match self.gamut.clip_method() {
            ClipMethod::Unavailable => GamutMethod::RoundTrip,
            _ => GamutMethod::LinearDevice,
        }
    }
    /// Whether one document pixel is outside the proof gamut, and its round-trip delta E (table devices).
    fn judge(&self, px: &[f32]) -> (bool, Option<f64>) {
        let channels = self.proof_space.channels();
        let mut device = [0.0f32; 4];
        match &self.gamut.source {
            Some(source) if self.method() == GamutMethod::RoundTrip => {
                let n = source.eval(px);
                let (k, o) = self.gamut.map.affine();
                let target = [k[0] * n[0] + o[0], k[1] * n[1] + o[1], k[2] * n[2] + o[2]];
                self.gamut.dest.eval(target, &mut device[..channels]);
                let back = self.proof_to_pcs.eval(&device[..channels]);
                let de = delta_e76(target, back);
                (de.is_nan() || de > GAMUT_ROUND_TRIP_DELTA_E76, Some(de))
            }
            _ => match self.gamut.pixel(px, false, &mut device[..channels]) {
                Some(lin) => (
                    lin.iter()
                        .take(channels)
                        .any(|v| !(-GAMUT_LINEAR_TOLERANCE..=1.0 + GAMUT_LINEAR_TOLERANCE).contains(v)),
                    None,
                ),
                None => (false, None),
            },
        }
    }
}

/// 8-connected components of `mask` (`width` x `height`): bounding regions in raster order of their first pixel,
/// at most `MAX_GAMUT_REGIONS` listed; returns (listed, total).
fn regions(mask: &[bool], width: usize, height: usize) -> (Vec<GamutRegion>, u64) {
    let mut seen = vec![false; mask.len()];
    let mut stack: Vec<usize> = Vec::new();
    let mut listed = Vec::new();
    let mut total = 0u64;
    for (start, marked) in mask.iter().enumerate() {
        if !*marked || seen[start] {
            continue;
        }
        total = total.saturating_add(1);
        seen[start] = true;
        stack.push(start);
        let (sx, sy) = (start.checked_rem(width).unwrap_or(0), start.checked_div(width).unwrap_or(0));
        let mut region = GamutRegion { x0: sx, y0: sy, x1: sx, y1: sy, pixels: 0 };
        while let Some(i) = stack.pop() {
            let (x, y) = (i.checked_rem(width).unwrap_or(0), i.checked_div(width).unwrap_or(0));
            region.pixels = region.pixels.saturating_add(1);
            region.x0 = region.x0.min(x);
            region.x1 = region.x1.max(x);
            region.y0 = region.y0.min(y);
            region.y1 = region.y1.max(y);
            for ny in [y.checked_sub(1), Some(y), y.checked_add(1)].into_iter().flatten() {
                if ny >= height {
                    continue;
                }
                for nx in [x.checked_sub(1), Some(x), x.checked_add(1)].into_iter().flatten() {
                    if nx >= width {
                        continue;
                    }
                    let j = ny.saturating_mul(width).saturating_add(nx);
                    if j < mask.len() && mask[j] && !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
        }
        if listed.len() < MAX_GAMUT_REGIONS {
            listed.push(region);
        }
    }
    (listed, total)
}

impl SoftProof {
    /// Gamut warning of `pixels` (document device values, `width` pixels per row) against the proof device.
    pub fn gamut_warning(
        &self,
        pixels: &[f32],
        width: usize,
        style: GamutWarningStyle,
        cancel: &CancellationToken,
    ) -> Result<GamutWarning, ProofError> {
        canceled(cancel)?;
        let channels = self.document_space.channels();
        let (count, height) = check_pixels(pixels, channels, width)?;
        let mut mask = vec![false; count];
        let mut max_de: Option<f64> = None;
        let (method, gamut_intent) = match &self.route {
            Route::Managed(m) => (m.method(), Some(m.gamut_intent)),
            Route::Unmanaged => (GamutMethod::NotApplicable, None),
        };
        if let Route::Managed(m) = &self.route {
            let chunk_in = CHUNK_PIXELS.saturating_mul(channels);
            for (px_chunk, mask_chunk) in pixels.chunks(chunk_in).zip(mask.chunks_mut(CHUNK_PIXELS)) {
                canceled(cancel)?;
                for (px, marked) in px_chunk.chunks_exact(channels).zip(mask_chunk.iter_mut()) {
                    let (out, de) = m.judge(px);
                    *marked = out;
                    if let Some(de) = de {
                        max_de = Some(max_de.map_or(de, |v: f64| v.max(de)));
                    }
                }
            }
        }
        canceled(cancel)?;
        let out_of_gamut = mask.iter().filter(|m| **m).count() as u64;
        let (regions, region_count) = regions(&mask, width, height);
        let bounds = regions.iter().copied().reduce(|a, b| GamutRegion {
            x0: a.x0.min(b.x0),
            y0: a.y0.min(b.y0),
            x1: a.x1.max(b.x1),
            y1: a.y1.max(b.y1),
            pixels: a.pixels.saturating_add(b.pixels),
        });
        // Regions past the cap still widen the overall bounds.
        let bounds = bounds.map(|b| {
            let mut b = b;
            if region_count > regions.len() as u64 {
                for (i, _) in mask.iter().enumerate().filter(|(_, m)| **m) {
                    let (x, y) = (i.checked_rem(width).unwrap_or(0), i.checked_div(width).unwrap_or(0));
                    b.x0 = b.x0.min(x);
                    b.x1 = b.x1.max(x);
                    b.y0 = b.y0.min(y);
                    b.y1 = b.y1.max(y);
                }
            }
            b.pixels = out_of_gamut;
            b
        });
        Ok(GamutWarning {
            method,
            gamut_intent,
            threshold_delta_e76: (method == GamutMethod::RoundTrip).then_some(GAMUT_ROUND_TRIP_DELTA_E76),
            out_of_gamut_pixels: out_of_gamut,
            pixel_count: count as u64,
            area_fraction: out_of_gamut as f64 / count as f64,
            bounds,
            regions_truncated: region_count > regions.len() as u64,
            regions,
            region_count,
            max_delta_e76: max_de,
            width,
            height,
            style,
            mask,
        })
    }

    /// The explicit in-gamut substitute of one document colour (STU-COL-245); never applied automatically.
    pub fn in_gamut_substitute(&self, colour: &[f32], cancel: &CancellationToken) -> Result<GamutSubstitute, ProofError> {
        canceled(cancel)?;
        let channels = self.document_space.channels();
        let (count, _) = check_pixels(colour, channels, 1)?;
        if count != 1 {
            return Err(WideError::ChannelCount.into());
        }
        let unchanged = |intent: Intent| GamutSubstitute {
            original: colour.to_vec(),
            substitute: colour.to_vec(),
            out_of_gamut: false,
            substitute_in_gamut: true,
            delta_e76: 0.0,
            intent,
        };
        let m = match &self.route {
            Route::Managed(m) => m,
            Route::Unmanaged => return Ok(unchanged(self.condition.proof_intent)),
        };
        if !m.judge(colour).0 {
            return Ok(unchanged(self.condition.proof_intent));
        }
        let back = m.back.as_ref().map_err(|e| ProofError::Wide(*e))?;
        let pch = m.proof_space.channels();
        let mut device = [0.0f32; 4];
        if let Some(lin) = m.to_proof.pixel(colour, false, &mut device[..pch]) {
            let mut clamped = lin;
            for v in clamped.iter_mut().take(pch) {
                *v = v.clamp(0.0, 1.0);
            }
            m.to_proof.dest.encode_linear(clamped, &mut device[..pch]);
        }
        let mut substitute = [0.0f32; 4];
        back.pixel(&device[..pch], false, &mut substitute[..channels]);
        for v in substitute.iter_mut().take(channels) {
            *v = v.clamp(0.0, 1.0);
        }
        let substitute = substitute[..channels].to_vec();
        if substitute.iter().any(|v| !v.is_finite()) {
            return Err(WideError::NonfiniteOutput.into());
        }
        canceled(cancel)?;
        Ok(GamutSubstitute {
            original: colour.to_vec(),
            substitute_in_gamut: !m.judge(&substitute).0,
            delta_e76: delta_e76(m.doc_to_pcs.eval(colour), m.doc_to_pcs.eval(&substitute)),
            substitute,
            out_of_gamut: true,
            intent: self.condition.proof_intent,
        })
    }
}
