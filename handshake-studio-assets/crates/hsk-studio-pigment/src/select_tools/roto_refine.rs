//! Roto refine edge (lane SMB-AE-ROTO-TIMEREMAP slice 3; parity AE-18 "Refine Edge" / refine
//! matte of Roto Brush): soft alpha by matting inside a trimap band around a roto matte, colour
//! decontamination, and temporal chatter reduction across a sequence.
//!
//! Spec basis (module 14): the refine-edge control set of STU-RAS-121 -- `radius`, `smart_radius`,
//! `smooth`, `feather`, `contrast`, `shift_edge`, `decontaminate_colours` ("boolean; gates the
//! amount control"), `decontamination_amount` ("percent ... all four bounds and the default
//! undeclared") -- applied per frame through the one Pigment refine engine
//! ([`super::refine::refine_with_marks`]: guided filter of He, Sun and Tang 2010 or the closed-form
//! matting of Levin, Lischinski and Weiss 2008), not a copy; STU-CMP-033 (a key / matte is a
//! pipeline with a visible refine stage); STU-CMP-034 / STU-CMP-100 (assisted rotoscoping gap,
//! see [`super::roto`] for the conflict record).
//!
//! Studio readings (After Effects Refine Edge / Reduce Chatter behaviour UNVERIFIED, D5; class
//! Approximate):
//! - trimap band: [`RotoBand::Radius`] = the refine engine's band (exact distance to the matte
//!   boundary minus 0.5 below the radius); [`RotoBand::Trimap`] = a painted unknown region (the
//!   engine's radius is set to 0 so the painted band is the only unknown region).
//! - decontamination: the engine's rule (band colours move `amount * min(1, 4 (1 - alpha))` toward
//!   the `alpha^4`-weighted local foreground colour); outside the refined coverage the frame's own
//!   straight colour is returned.
//! - reduce chatter (`0..=100` percent): `a' = (1 - k) a + k mean(neighbour alphas)`, `k = 2/3 *
//!   chatter / 100` (100 % = equal thirds over three frames), each neighbour alpha warped into the
//!   frame along the optical flow ([`crate::frame_blend`]); the end frames use their one
//!   neighbour. Computed from the per-frame refined alphas (no cascade).
#![deny(clippy::arithmetic_side_effects)]
use super::refine::{RefineOptions, RefineOutput, TrimapMarks, refine_with_marks};
use super::roto::{RotoLimits, blend_err, check_frame};
use super::{ToolError, ToolLimits, index_in};
use crate::frame_blend::{FlowParams, bilinear, flow_admit, flow_gray, luma};
use crate::retouch::RgbaPlane;
use crate::selection::{ColourView, CoveragePlane, SelectionContext};
use crate::Error;
use hsk_studio_accord::CancellationToken;

/// The unknown region of the trimap.
#[derive(Clone, Copy, Debug)]
pub enum RotoBand<'a> {
    /// Pixels nearer the matte boundary than this many pixels (`>= 0`).
    Radius(f64),
    /// A painted unknown region (coverage `>= 0.5`).
    Trimap(&'a CoveragePlane),
}

/// Sequence refinement options.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RotoRefineOptions {
    /// Engine controls (STU-RAS-121); `radius` is replaced by `band_px`.
    pub refine: RefineOptions,
    /// Trimap band half-width per frame (`>= 0`, finite).
    pub band_px: f64,
    /// Reduce chatter, percent `0..=100`.
    pub reduce_chatter: f64,
    pub flow: FlowParams,
}
impl Default for RotoRefineOptions {
    fn default() -> Self {
        Self {
            refine: RefineOptions::default(),
            band_px: 3.0,
            reduce_chatter: 0.0,
            flow: FlowParams::DEFAULT,
        }
    }
}

/// One refined frame over the frame bounds.
#[derive(Clone, Debug, PartialEq)]
pub struct RotoRefined {
    pub index: usize,
    /// Soft matte, row-major over the frame bounds.
    pub coverage: CoveragePlane,
    /// Decontaminated straight colours over the frame bounds (with `decontaminate_colours`).
    pub colours: Option<Vec<[f32; 3]>>,
    pub band_pixels: u64,
}

/// Frame bytes as the selection engine's colour view (rgb f32le + alpha f32le).
fn view_bytes(frame: &RgbaPlane, limits: &ToolLimits) -> Result<(Vec<u8>, Vec<u8>), Error> {
    let n = limits.admit(frame.bounds.area(), 16)?;
    let mut colour = Vec::with_capacity(n.checked_mul(12).ok_or(Error::Overflow)?);
    let mut alpha = Vec::with_capacity(n.checked_mul(4).ok_or(Error::Overflow)?);
    for p in &frame.data {
        for c in &p[..3] {
            colour.extend_from_slice(&c.to_le_bytes());
        }
        alpha.extend_from_slice(&p[3].to_le_bytes());
    }
    Ok((colour, alpha))
}

/// Refine one frame's roto matte inside `band` (module docs).
pub fn refine_roto_frame(
    frame: &RgbaPlane,
    matte: &CoveragePlane,
    band: &RotoBand<'_>,
    refine: &RefineOptions,
    limits: &ToolLimits,
) -> Result<RefineOutput, ToolError> {
    check_frame(frame)?;
    let mut opts = *refine;
    let marks = match band {
        RotoBand::Radius(r) => {
            if !r.is_finite() {
                return Err(Error::Nonfinite.into());
            }
            opts.radius = *r;
            TrimapMarks::default()
        }
        RotoBand::Trimap(p) => {
            opts.radius = 0.0;
            TrimapMarks {
                unknown: Some(*p),
                ..TrimapMarks::default()
            }
        }
    };
    let (colour, alpha) = view_bytes(frame, limits)?;
    let view = ColourView {
        bounds: frame.bounds,
        colour: &colour,
        alpha: &alpha,
    };
    let ctx = SelectionContext::new(frame.bounds);
    refine_with_marks(&[view], 0, matte, &marks, &opts, &ctx, limits)
}

/// Refine every frame of a roto sequence, then reduce chatter (module docs). `mattes[i]` belongs
/// to `frames[i]`. `reduce_chatter` is range-checked `0..=100` here (the predecessor contract,
/// pinned by `tests/roto_refine.rs`); [`refine_roto_stage`] is the STU-CMP-034 v02.217 entry whose
/// chatter bounds are `unknown` and therefore not checked.
pub fn refine_roto_sequence(
    frames: &[RgbaPlane],
    mattes: &[CoveragePlane],
    opts: &RotoRefineOptions,
    limits: &RotoLimits,
    cancel: &CancellationToken,
) -> Result<Vec<RotoRefined>, ToolError> {
    if !opts.reduce_chatter.is_finite() || !opts.band_px.is_finite() {
        return Err(Error::Nonfinite.into());
    }
    if !(0.0..=100.0).contains(&opts.reduce_chatter) || opts.band_px < 0.0 {
        return Err(ToolError::OutOfRange);
    }
    let bands: Vec<Option<RotoBand<'_>>> = frames.iter().map(|_| Some(RotoBand::Radius(opts.band_px))).collect();
    refine_frames(frames, mattes, &bands, &opts.refine, opts.reduce_chatter, &opts.flow, limits, cancel)
}

/// The refine stage of assisted rotoscoping (lane SMB-AE-ROTO-WIRING; STU-CMP-034 item 5): per
/// frame the one STU-RAS-121 engine inside `bands[i]` (`None` = the frame is passed through
/// unrefined: its coverage is the input matte, no colours), then chatter reduction at
/// `reduce_chatter` percent (`k = 2/3 * percent / 100`, module docs). The v02.217 table declares
/// the chatter bounds `unknown` ([STU-RAS-106]): any finite value is used as given, never clamped
/// or range-checked (a value outside `0..=100` extrapolates the blend; the result alpha is
/// limited to `[0, 1]` when written as coverage). Bytes and flow work are admitted before work.
#[allow(clippy::too_many_arguments)]
pub fn refine_roto_stage(
    frames: &[RgbaPlane],
    mattes: &[CoveragePlane],
    bands: &[Option<RotoBand<'_>>],
    refine: &RefineOptions,
    reduce_chatter: f64,
    flow: &FlowParams,
    limits: &RotoLimits,
    cancel: &CancellationToken,
) -> Result<Vec<RotoRefined>, ToolError> {
    if !reduce_chatter.is_finite() {
        return Err(Error::Nonfinite.into());
    }
    refine_frames(frames, mattes, bands, refine, reduce_chatter, flow, limits, cancel)
}

#[allow(clippy::too_many_arguments)]
fn refine_frames(
    frames: &[RgbaPlane],
    mattes: &[CoveragePlane],
    bands: &[Option<RotoBand<'_>>],
    refine: &RefineOptions,
    reduce_chatter: f64,
    flow: &FlowParams,
    limits: &RotoLimits,
    cancel: &CancellationToken,
) -> Result<Vec<RotoRefined>, ToolError> {
    let first = frames.first().ok_or(Error::InvalidInput)?;
    if mattes.len() != frames.len() || bands.len() != frames.len() {
        return Err(Error::InvalidInput.into());
    }
    if frames.len() > limits.max_frames {
        return Err(Error::BudgetExceeded.into());
    }
    let fb = first.bounds;
    let count = u64::try_from(frames.len()).map_err(|_| Error::Overflow)?;
    // Output alpha (2 bytes) + f64 alpha (8) + colours (12) per pixel per frame, before any work.
    let out = fb.area().checked_mul(22).and_then(|b| b.checked_mul(count)).ok_or(Error::Overflow)?;
    if out > limits.max_output_bytes {
        return Err(Error::BudgetExceeded.into());
    }
    let chatter = reduce_chatter != 0.0 && frames.len() > 1;
    if chatter {
        // Two flow estimates per frame.
        flow_admit(fb.area(), flow, count.saturating_mul(2), &limits.blend).map_err(blend_err)?;
    }
    for f in frames {
        if f.bounds != fb {
            return Err(Error::InvalidInput.into());
        }
        check_frame(f)?;
    }
    let (w, h) = (fb.width as usize, fb.height as usize);
    let n = w.checked_mul(h).ok_or(Error::Overflow)?;
    let mut alphas: Vec<Vec<f64>> = Vec::with_capacity(frames.len());
    let mut colours: Vec<Option<Vec<[f32; 3]>>> = Vec::with_capacity(frames.len());
    let mut bands_px = Vec::with_capacity(frames.len());
    for ((frame, matte), band) in frames.iter().zip(mattes).zip(bands) {
        cancel.check().map_err(|_| Error::Canceled)?;
        let pixel = |i: usize| {
            (
                fb.x.saturating_add(i.checked_rem(w).unwrap_or(0) as i64),
                fb.y.saturating_add(i.checked_div(w).unwrap_or(0) as i64),
            )
        };
        let Some(band) = band else {
            alphas.push((0..n).map(|i| { let (x, y) = pixel(i); f64::from(matte.at(x, y)) / 65535.0 }).collect());
            colours.push(None);
            bands_px.push(0);
            continue;
        };
        let r = refine_roto_frame(frame, matte, band, refine, &limits.tool)?;
        let mut a = Vec::with_capacity(n);
        let mut c = r.colours.as_ref().map(|_| Vec::with_capacity(n));
        for i in 0..n {
            let (x, y) = pixel(i);
            a.push(f64::from(r.coverage.at(x, y)) / 65535.0);
            if let (Some(out), Some(src)) = (c.as_mut(), r.colours.as_ref()) {
                let own = frame.data.get(i).copied().unwrap_or([0.0; 4]);
                let px = index_in(r.coverage.bounds, x, y)
                    .and_then(|j| src.get(j).copied())
                    .unwrap_or([own[0], own[1], own[2]]);
                out.push(px);
            }
        }
        alphas.push(a);
        colours.push(c);
        bands_px.push(r.band_pixels);
    }
    let finals: Vec<Vec<f64>> = if chatter {
        let k = 2.0 / 3.0 * reduce_chatter / 100.0;
        let lumas: Vec<_> = frames.iter().map(|f| luma(f, w, h)).collect();
        let mut out = Vec::with_capacity(frames.len());
        for t in 0..frames.len() {
            cancel.check().map_err(|_| Error::Canceled)?;
            let mut sum = vec![0.0f64; n];
            let mut used = 0u32;
            for nb in [t.checked_sub(1), t.checked_add(1).filter(|v| *v < frames.len())].into_iter().flatten() {
                let (Some(lt), Some(ln), Some(an)) = (lumas.get(t), lumas.get(nb), alphas.get(nb)) else {
                    continue;
                };
                let fl = flow_gray(lt, ln, flow, cancel).map_err(blend_err)?;
                for (i, s) in sum.iter_mut().enumerate() {
                    let (x, y) = (i.checked_rem(w).unwrap_or(0), i.checked_div(w).unwrap_or(0));
                    let [u, v] = fl.at(x, y);
                    *s += bilinear(w, h, x as f64 + u, y as f64 + v, |j| an.get(j).copied().unwrap_or(0.0));
                }
                used = used.saturating_add(1);
            }
            let own = alphas.get(t).ok_or(Error::InvalidInput)?;
            out.push(
                own.iter()
                    .zip(&sum)
                    .map(|(a, s)| if used > 0 { (1.0 - k) * a + k * s / f64::from(used) } else { *a })
                    .collect(),
            );
        }
        out
    } else {
        alphas
    };
    Ok(finals
        .into_iter()
        .zip(colours)
        .zip(bands_px)
        .enumerate()
        .map(|(index, ((a, colours), band_pixels))| RotoRefined {
            index,
            coverage: CoveragePlane {
                bounds: fb,
                data: a.iter().map(|v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16).collect(),
            },
            colours,
            band_pixels,
        })
        .collect())
}
