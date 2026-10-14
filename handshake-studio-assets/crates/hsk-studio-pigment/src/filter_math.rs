//! Core pixel filters as plan operators (lane SMB-PIGMENT-RETOUCH slice 4, Photoshop parity gap
//! PS-22; spec basis STU-RAS-037 live filter with a destructive apply form, STU-FX-011 determinism
//! with a seed, STU-FX-013 Gaussian blur core, STU-FX-015 additive noise with seed and colour mode,
//! STU-FX-019 high pass deduped, 14.9 rows Unsharp Mask, High Pass, Median Blur, Add Noise).
//! Single owner (CX-CAP-001) of the filter maths; the plan envelope carries them as
//! [`crate::render_plan::LayerExtension::filters`] and reports [`FilterKind`] operators.
//!
//! Every filter maps a straight-RGBA source plane (row-major over `src_rect`, transparent beyond
//! it) to the straight RGBA of any output window [`filter_window`]; each output pixel depends only
//! on the source, never on the window, so any split of a plane into windows, executed on any
//! number of threads, yields the same bytes ([`filter_plane_chunked`]; test `tests/retouch.rs`
//! `filters::filters_are_chunk_and_thread_count_independent`).
//!
//! Formulas (Studio readings; Photoshop's exact formulas and parameter ranges are UNVERIFIED, D5,
//! so every kind is class Approximate):
//! - `gaussian_blur { radius_px }`: [`crate::blur_math::blur_window`] with the shared Gaussian
//!   (sigma = radius / 2), premultiplied.
//! - `unsharp_mask { amount_pct 1..=500, radius_px 0.1..=1000, threshold_levels 0..=255 }`: with `o`
//!   the source and `b` its Gaussian blur (both straight), per colour channel `diff = o - b`;
//!   when `|diff| * 255 >= threshold` the channel is `clamp(o + amount/100 * diff, 0, 1)`, else
//!   `o`; alpha is `o`'s.
//! - `high_pass { radius_px 0.1..=1000 }`: per colour channel `clamp(o - b + 0.5, 0, 1)`; alpha
//!   kept (flat regions become mid grey).
//! - `median { radius_px 1..=500 }` (integer): per premultiplied channel, the median over the disc
//!   `dx^2 + dy^2 <= r^2` (an odd count, transparent beyond the source), sorted by `total_cmp`,
//!   then un-premultiplied.
//! - `add_noise { amount_pct 0.1..=400, distribution uniform|gaussian, monochromatic, seed }`: per
//!   colour channel `clamp(o + n, 0, 1)` with `A = amount/100 * 0.5`, uniform `n = (2u - 1) A`,
//!   gaussian `n = (u1 + u2 + u3 + u4 - 2) A` (Irwin-Hall: the same standard deviation `A/sqrt 3`
//!   as the uniform, IEEE add/multiply only, no transcendental function); `u` are binary64
//!   uniforms in `[0, 1)` from SplitMix64 of `(seed, x, y, channel, draw)` (monochromatic: one
//!   value for the three channels); alpha kept.
//!
//! Point filters (unsharp, high pass, noise) return `[0, 0, 0, 0]` where the source alpha is 0.
//!
//! Hostile inputs (V4-01/02): parameters are range-checked (never clamped); window bytes and work
//! are computed with checked arithmetic and refused against [`FilterLimits`] before allocation.
#![deny(clippy::arithmetic_side_effects)]
use crate::Rect;
use crate::blend_math::Exactness;
use crate::blur_math::{BlurError, BlurLimits, BlurScope, LayerBlur, blur_window, blur_window_cost, unpremultiply};
use crate::retouch::RgbaPlane;
use serde::Serialize;
use std::fmt;

/// Class of every filter kind (see the module docs).
pub const FILTER_EXACTNESS: Exactness = Exactness::Approximate;
/// Filters one envelope entry may carry.
pub const MAX_FILTERS_PER_LAYER: usize = 16;
/// Bytes of one window sample.
const SAMPLE_BYTES: u64 = 16;

/// Noise distribution (Photoshop Add Noise: uniform | gaussian).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum NoiseDistribution {
    Uniform,
    Gaussian,
}

/// One filter with its parameters.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub enum LayerFilter {
    GaussianBlur { radius_px: f32 },
    UnsharpMask { amount_pct: f32, radius_px: f32, threshold_levels: u8 },
    HighPass { radius_px: f32 },
    Median { radius_px: u32 },
    AddNoise {
        amount_pct: f32,
        distribution: NoiseDistribution,
        monochromatic: bool,
        seed: u64,
    },
}

/// Filter kinds (the unit of plan capability; keys are the operator keys).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum FilterKind {
    GaussianBlur,
    UnsharpMask,
    HighPass,
    Median,
    AddNoise,
}
impl FilterKind {
    pub const fn key(self) -> &'static str {
        match self {
            Self::GaussianBlur => "filter_gaussian_blur",
            Self::UnsharpMask => "filter_unsharp_mask",
            Self::HighPass => "filter_high_pass",
            Self::Median => "filter_median",
            Self::AddNoise => "filter_add_noise",
        }
    }
}

/// Typed failure of the filter maths.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterError {
    /// A parameter is non-finite or outside its range.
    Parameter,
    /// A buffer length disagrees with its rectangle.
    Shape,
    /// A sample outside `[0, 1]` or non-finite.
    OutOfDomain,
    /// Bytes or work above the caller's limits (refused before allocation).
    OverBudget { needed: u64, limit: u64 },
    Overflow,
}
impl FilterError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Parameter => "filter_parameter",
            Self::Shape => "filter_shape",
            Self::OutOfDomain => "filter_out_of_domain",
            Self::OverBudget { .. } => "filter_over_budget",
            Self::Overflow => "filter_overflow",
        }
    }
}
impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for FilterError {}
impl From<BlurError> for FilterError {
    fn from(e: BlurError) -> Self {
        match e {
            BlurError::Radius => Self::Parameter,
            BlurError::Shape => Self::Shape,
            BlurError::OverBudget { needed, limit } => Self::OverBudget { needed, limit },
            BlurError::OutOfDomain => Self::OutOfDomain,
            _ => Self::Overflow,
        }
    }
}

/// Limits of one window: owned bytes at the peak and work (sample visits).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FilterLimits {
    pub max_bytes: u64,
    pub max_work: u64,
}

fn range(v: f32, lo: f32, hi: f32) -> bool {
    v.is_finite() && v >= lo && v <= hi
}

impl LayerFilter {
    pub const fn kind(&self) -> FilterKind {
        match self {
            Self::GaussianBlur { .. } => FilterKind::GaussianBlur,
            Self::UnsharpMask { .. } => FilterKind::UnsharpMask,
            Self::HighPass { .. } => FilterKind::HighPass,
            Self::Median { .. } => FilterKind::Median,
            Self::AddNoise { .. } => FilterKind::AddNoise,
        }
    }
    /// Parameter ranges of the module docs (Studio bounds; Photoshop ranges UNVERIFIED).
    pub fn validate(&self) -> Result<(), FilterError> {
        let ok = match *self {
            Self::GaussianBlur { radius_px } => LayerBlur::new(BlurScope::Layer, radius_px).validate().is_ok(),
            Self::UnsharpMask { amount_pct, radius_px, .. } => {
                range(amount_pct, 1.0, 500.0) && range(radius_px, 0.1, 1000.0)
            }
            Self::HighPass { radius_px } => range(radius_px, 0.1, 1000.0),
            Self::Median { radius_px } => (1..=500).contains(&radius_px),
            Self::AddNoise { amount_pct, .. } => range(amount_pct, 0.1, 400.0),
        };
        if ok { Ok(()) } else { Err(FilterError::Parameter) }
    }
    fn blur_radius(&self) -> Option<f32> {
        match *self {
            Self::GaussianBlur { radius_px } | Self::UnsharpMask { radius_px, .. } | Self::HighPass { radius_px } => {
                Some(radius_px)
            }
            _ => None,
        }
    }
}

fn count(r: Rect) -> Result<usize, FilterError> {
    usize::try_from(u64::from(r.width).checked_mul(u64::from(r.height)).ok_or(FilterError::Overflow)?)
        .map_err(|_| FilterError::Overflow)
}

fn src_at(src: &[[f32; 4]], src_rect: Rect, x: i64, y: i64) -> Result<[f32; 4], FilterError> {
    let (dx, dy) = (x.checked_sub(src_rect.x), y.checked_sub(src_rect.y));
    let (Some(dx), Some(dy)) = (dx, dy) else {
        return Ok([0.0; 4]);
    };
    let (Ok(dx), Ok(dy)) = (u32::try_from(dx), u32::try_from(dy)) else {
        return Ok([0.0; 4]);
    };
    if dx >= src_rect.width || dy >= src_rect.height {
        return Ok([0.0; 4]);
    }
    let at = (dy as usize)
        .checked_mul(src_rect.width as usize)
        .and_then(|r| r.checked_add(dx as usize))
        .ok_or(FilterError::Overflow)?;
    src.get(at).copied().ok_or(FilterError::Shape)
}

fn check(p: [f32; 4]) -> Result<[f32; 4], FilterError> {
    if p.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
        Ok(p)
    } else {
        Err(FilterError::OutOfDomain)
    }
}

/// The disc offsets `dx^2 + dy^2 <= r^2`, row-major.
fn disc(r: u32) -> Result<Vec<(i64, i64)>, FilterError> {
    let ri = i64::from(r);
    let r2 = ri.checked_mul(ri).ok_or(FilterError::Overflow)?;
    let mut out = Vec::new();
    for dy in ri.checked_neg().ok_or(FilterError::Overflow)?..=ri {
        for dx in ri.checked_neg().ok_or(FilterError::Overflow)?..=ri {
            let d2 = dx.checked_mul(dx).and_then(|a| dy.checked_mul(dy).and_then(|b| a.checked_add(b)));
            if d2.ok_or(FilterError::Overflow)? <= r2 {
                out.push((dx, dy));
            }
        }
    }
    Ok(out)
}

/// Number of disc offsets for radius `r` (counted without allocating).
fn disc_len(r: u32) -> Result<u64, FilterError> {
    let ri = i64::from(r);
    let r2 = ri.checked_mul(ri).ok_or(FilterError::Overflow)?;
    let mut n = 0u64;
    for dy in ri.checked_neg().ok_or(FilterError::Overflow)?..=ri {
        let rest = r2.checked_sub(dy.checked_mul(dy).ok_or(FilterError::Overflow)?).ok_or(FilterError::Overflow)?;
        // Largest dx with dx^2 <= rest.
        let mut w = (rest as f64).sqrt() as i64;
        while w.checked_mul(w).is_some_and(|v| v > rest) {
            w = w.checked_sub(1).ok_or(FilterError::Overflow)?;
        }
        while w.checked_add(1).and_then(|v| v.checked_mul(v)).is_some_and(|v| v <= rest) {
            w = w.checked_add(1).ok_or(FilterError::Overflow)?;
        }
        let row = w.checked_mul(2).and_then(|v| v.checked_add(1)).ok_or(FilterError::Overflow)?;
        n = n.checked_add(u64::try_from(row).map_err(|_| FilterError::Overflow)?).ok_or(FilterError::Overflow)?;
    }
    Ok(n)
}

/// Predicted peak bytes and work of [`filter_window`] for `out`.
pub fn filter_window_cost(filter: &LayerFilter, out: Rect) -> Result<(u64, u64), FilterError> {
    filter.validate()?;
    let area = u64::from(out.width).checked_mul(u64::from(out.height)).ok_or(FilterError::Overflow)?;
    let out_bytes = area.checked_mul(SAMPLE_BYTES).ok_or(FilterError::Overflow)?;
    if let Some(r) = filter.blur_radius() {
        let taps = LayerBlur::new(BlurScope::Layer, r).support_px()?;
        let taps = usize::try_from(u64::from(taps).checked_mul(2).and_then(|v| v.checked_add(1)).ok_or(FilterError::Overflow)?)
            .map_err(|_| FilterError::Overflow)?;
        let (bytes, work) = blur_window_cost(out, taps)?;
        return Ok((bytes.checked_add(out_bytes).ok_or(FilterError::Overflow)?, work.checked_add(area).ok_or(FilterError::Overflow)?));
    }
    match *filter {
        LayerFilter::Median { radius_px } => {
            let n = disc_len(radius_px)?;
            let scratch = n.checked_mul(8).and_then(|v| v.checked_mul(4)).ok_or(FilterError::Overflow)?;
            let work = area.checked_mul(n).and_then(|v| v.checked_mul(4)).ok_or(FilterError::Overflow)?;
            Ok((out_bytes.checked_add(scratch).ok_or(FilterError::Overflow)?, work))
        }
        _ => Ok((out_bytes, area.checked_mul(4).ok_or(FilterError::Overflow)?)),
    }
}

fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Uniform binary64 in `[0, 1)` for `(seed, x, y, channel, draw)`.
pub fn noise_uniform(seed: u64, x: i64, y: i64, channel: u8, draw: u8) -> f64 {
    let h = splitmix(seed ^ splitmix((x as u64) ^ splitmix((y as u64) ^ splitmix(u64::from(channel) << 8 | u64::from(draw)))));
    (h >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
}

fn noise_value(dist: NoiseDistribution, a: f64, seed: u64, x: i64, y: i64, channel: u8) -> f64 {
    match dist {
        NoiseDistribution::Uniform => (2.0 * noise_uniform(seed, x, y, channel, 0) - 1.0) * a,
        NoiseDistribution::Gaussian => {
            let s = noise_uniform(seed, x, y, channel, 0)
                + noise_uniform(seed, x, y, channel, 1)
                + noise_uniform(seed, x, y, channel, 2)
                + noise_uniform(seed, x, y, channel, 3);
            (s - 2.0) * a
        }
    }
}

/// The filter of `src` at every pixel of `out` (straight RGBA, row-major over `out`). Bytes and
/// work ([`filter_window_cost`]) are checked against `limits` before any allocation.
pub fn filter_window(
    src: &[[f32; 4]],
    src_rect: Rect,
    out: Rect,
    filter: &LayerFilter,
    limits: FilterLimits,
) -> Result<Vec<[f32; 4]>, FilterError> {
    if src.len() != count(src_rect)? {
        return Err(FilterError::Shape);
    }
    let (bytes, work) = filter_window_cost(filter, out)?;
    if bytes > limits.max_bytes {
        return Err(FilterError::OverBudget { needed: bytes, limit: limits.max_bytes });
    }
    if work > limits.max_work {
        return Err(FilterError::OverBudget { needed: work, limit: limits.max_work });
    }
    out.end().map_err(|_| FilterError::Overflow)?;
    src_rect.end().map_err(|_| FilterError::Overflow)?;
    let n = count(out)?;
    let (ex, ey) = out.end().map_err(|_| FilterError::Overflow)?;
    let blurred = match filter.blur_radius() {
        Some(r) => {
            let taps = LayerBlur::new(BlurScope::Layer, r).taps()?;
            let wide = BlurLimits { max_bytes: limits.max_bytes, max_work: limits.max_work };
            Some(blur_window(src, src_rect, out, &taps, wide)?)
        }
        None => None,
    };
    if let (LayerFilter::GaussianBlur { .. }, Some(b)) = (filter, &blurred) {
        return Ok(b.clone());
    }
    let mut result = Vec::with_capacity(n);
    let disc = match filter {
        LayerFilter::Median { radius_px } => disc(*radius_px)?,
        _ => Vec::new(),
    };
    let mut scratch: [Vec<f32>; 4] = Default::default();
    let mut k = 0usize;
    for y in out.y..ey {
        for x in out.x..ex {
            let o = check(src_at(src, src_rect, x, y)?)?;
            let px = match *filter {
                LayerFilter::GaussianBlur { .. } => o,
                LayerFilter::UnsharpMask { amount_pct, threshold_levels, .. } => {
                    let b = blurred.as_ref().and_then(|v| v.get(k)).copied().ok_or(FilterError::Shape)?;
                    if o[3] == 0.0 {
                        [0.0; 4]
                    } else {
                        let amt = f64::from(amount_pct) / 100.0;
                        let mut p = o;
                        for (c, bc) in p.iter_mut().take(3).zip(b) {
                            let diff = f64::from(*c) - f64::from(bc);
                            if diff.abs() * 255.0 >= f64::from(threshold_levels) {
                                *c = (f64::from(*c) + amt * diff).clamp(0.0, 1.0) as f32;
                            }
                        }
                        p
                    }
                }
                LayerFilter::HighPass { .. } => {
                    let b = blurred.as_ref().and_then(|v| v.get(k)).copied().ok_or(FilterError::Shape)?;
                    if o[3] == 0.0 {
                        [0.0; 4]
                    } else {
                        let mut p = o;
                        for (c, bc) in p.iter_mut().take(3).zip(b) {
                            *c = (f64::from(*c) - f64::from(bc) + 0.5).clamp(0.0, 1.0) as f32;
                        }
                        p
                    }
                }
                LayerFilter::Median { .. } => {
                    for s in scratch.iter_mut() {
                        s.clear();
                    }
                    for &(dx, dy) in &disc {
                        let q = check(src_at(
                            src,
                            src_rect,
                            x.checked_add(dx).ok_or(FilterError::Overflow)?,
                            y.checked_add(dy).ok_or(FilterError::Overflow)?,
                        )?)?;
                        let a = q[3];
                        let pm = [q[0] * a, q[1] * a, q[2] * a, a];
                        for (s, v) in scratch.iter_mut().zip(pm) {
                            s.push(v);
                        }
                    }
                    let mid = disc.len().checked_div(2).ok_or(FilterError::Overflow)?;
                    let mut m = [0.0f32; 4];
                    for (mc, s) in m.iter_mut().zip(scratch.iter_mut()) {
                        s.sort_unstable_by(f32::total_cmp);
                        *mc = s.get(mid).copied().ok_or(FilterError::Shape)?;
                    }
                    unpremultiply(m)
                }
                LayerFilter::AddNoise { amount_pct, distribution, monochromatic, seed } => {
                    if o[3] == 0.0 {
                        [0.0; 4]
                    } else {
                        let a = f64::from(amount_pct) / 100.0 * 0.5;
                        let mut p = o;
                        for (ch, c) in p.iter_mut().take(3).enumerate() {
                            let channel = if monochromatic { 0 } else { u8::try_from(ch).map_err(|_| FilterError::Overflow)? };
                            let n = noise_value(distribution, a, seed, x, y, channel);
                            *c = (f64::from(*c) + n).clamp(0.0, 1.0) as f32;
                        }
                        p
                    }
                }
            };
            result.push(px);
            k = k.checked_add(1).ok_or(FilterError::Overflow)?;
        }
    }
    Ok(result)
}

/// Destructive apply over the whole plane (the "apply to pixels" form of STU-RAS-037).
pub fn filter_plane(plane: &RgbaPlane, filter: &LayerFilter, limits: FilterLimits) -> Result<RgbaPlane, FilterError> {
    let data = filter_window(&plane.data, plane.bounds, plane.bounds, filter, limits)?;
    Ok(RgbaPlane { bounds: plane.bounds, data })
}

/// [`filter_plane`] split into horizontal bands of `band_rows` rows run on `threads` scoped
/// threads (bands assigned round-robin). The bytes equal [`filter_plane`] for every band height
/// and thread count (each window is computed from the whole source). `limits` apply per band.
pub fn filter_plane_chunked(
    plane: &RgbaPlane,
    filter: &LayerFilter,
    limits: FilterLimits,
    band_rows: u32,
    threads: usize,
) -> Result<RgbaPlane, FilterError> {
    if band_rows == 0 || threads == 0 {
        return Err(FilterError::Parameter);
    }
    filter.validate()?;
    let b = plane.bounds;
    let mut bands = Vec::new();
    let mut row = 0u32;
    while row < b.height {
        let h = band_rows.min(b.height.checked_sub(row).ok_or(FilterError::Overflow)?);
        bands.push(Rect {
            x: b.x,
            y: b.y.checked_add(i64::from(row)).ok_or(FilterError::Overflow)?,
            width: b.width,
            height: h,
        });
        row = row.checked_add(h).ok_or(FilterError::Overflow)?;
    }
    let mut results: Vec<Option<Result<Vec<[f32; 4]>, FilterError>>> = vec![None; bands.len()];
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for t in 0..threads {
            let bands = &bands;
            handles.push(scope.spawn(move || {
                let mut mine = Vec::new();
                let mut i = t;
                while let Some(r) = bands.get(i) {
                    mine.push((i, filter_window(&plane.data, plane.bounds, *r, filter, limits)));
                    i = i.saturating_add(threads);
                }
                mine
            }));
        }
        for h in handles {
            if let Ok(mine) = h.join() {
                for (i, r) in mine {
                    if let Some(slot) = results.get_mut(i) {
                        *slot = Some(r);
                    }
                }
            }
        }
    });
    let mut data = Vec::with_capacity(count(b)?);
    for r in results {
        data.extend(r.ok_or(FilterError::Shape)??);
    }
    Ok(RgbaPlane { bounds: b, data })
}

/// Filter surface description (Studio readings, parity NOT_PROVEN).
pub const DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-PIGMENT","surface":"filters","version":1,"kinds":{"filter_gaussian_blur":"blur_math blur_window, sigma = radius/2, radius 0..1000","filter_unsharp_mask":"amount 1..500 %, radius 0.1..1000 px, threshold 0..255 levels; channel = clamp(o + amount/100 (o - blur)) where |o - blur| * 255 >= threshold","filter_high_pass":"radius 0.1..1000 px; clamp(o - blur + 0.5)","filter_median":"integer radius 1..500, disc dx^2+dy^2<=r^2, per premultiplied channel median (total_cmp), transparent beyond the source","filter_add_noise":"amount 0.1..400 %, uniform (2u-1)A or gaussian Irwin-Hall (u1+u2+u3+u4-2)A, A = amount/200, SplitMix64(seed, x, y, channel, draw), monochromatic option"},"determinism":"each output pixel depends only on the source: any window split and thread count give the same bytes (filter_plane_chunked)","plan":"LayerExtension.filters (<= 16, list order, on the layer content); ExtOperator::Filter(kind) class approximate; backends declare through extension_exactness (render-cpu/render-gpu: not yet, admission refuses filter_* keys)","class":"approximate","parity":"NOT_PROVEN (Photoshop formulas and ranges UNVERIFIED, D5)"}"#;
