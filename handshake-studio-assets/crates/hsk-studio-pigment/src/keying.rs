//! Keying and matte refinement (lane SMB-AE-KEY-TRACK slice 1, After Effects parity gap AE-18).
//!
//! Spec basis (module 14 section 14.27.2): STU-CMP-032 (Studio ships a keying FAMILY: chroma key,
//! colour range key, luma key, difference matte, spill suppressor, matte choker / simple choker),
//! STU-CMP-032a (each is a live filter in the `keying` or `matte` category; parameter records are
//! declared gap STU-FX-146, so the parameters below are Studio's authored reading), STU-CMP-033 (a
//! key is a MATTE PIPELINE: extract, refine, despill, each stage an ordinary, reorderable stack
//! entry; no single opaque "key" operation). Single owner (CX-CAP-001) of the keying pixel maths;
//! the Gaussian feather reuses the shared kernel [`crate::coverage_math::feather_kernel`].
//!
//! Model: a pipeline ([`run_key_pipeline`]) carries a straight-RGBA colour plane and a matte (one
//! `f32` per pixel, `1` = keep, `0` = keyed out), initially all `1`. Each [`KeyStage`] is one
//! stack entry: an extractor computes a matte and combines it into the current one
//! ([`MatteCombine`]), a refinement edits the matte, despill edits the colour. The result alpha is
//! `source alpha * matte`. Every formula below is evaluated in binary64 with IEEE `+ - * /` and
//! `sqrt` only (the Lab cube root is [`det_cbrt`], Newton steps from a bit-pattern seed), in a
//! fixed pixel order, then rounded once to binary32: two runs on any host give the same bits
//! (test `tests/keying.rs` `pipeline_is_bit_deterministic_and_gpu_refuses_typed`). The one
//! exception is the feather, whose shared Gaussian taps come from `f64::exp` (platform libm; the
//! taps are rounded to binary32 before use).
//!
//! Formulas (Studio readings; After Effects' exact formulas are UNVERIFIED, D5, so every stage is
//! class Approximate, [`KEY_EXACTNESS`]):
//! - ramp(d, t, s): `0` for `d <= t`, `1` for `s == 0` otherwise, else `min((d - t) / s, 1)`.
//! - chroma key: distance from the key colour in YCbCr (BT.709 luma weights, `Cb = (B - Y) /
//!   1.8556`, `Cr = (R - Y) / 1.5748`, distance `sqrt(dCb^2 + dCr^2 + (w dY)^2)`) or CIE 1976
//!   L*a*b* (samples read as linear light with Rec.709 primaries and the D65 white, XYZ by the
//!   IEC 61966-2-1 matrix, components divided by 100, distance `sqrt((w dL)^2 + da^2 + db^2)`);
//!   `w` = `luma_weight`; matte = ramp(distance, tolerance, softness).
//! - luma key: `Y` = BT.709 weights of the sample; darker: ramp(Y - threshold, 0, softness);
//!   brighter: ramp(threshold - Y, 0, softness); similar: ramp(|Y - threshold|, tolerance,
//!   softness); dissimilar: `1 -` similar.
//! - difference matte: both planes premultiplied (optionally Gaussian-blurred with the shared
//!   kernel, edges clamped), `d = max(|dR|, |dG|, |dB|, |dA|)`, matte = ramp(d, tolerance,
//!   softness).
//! - colour range key: the axis-aligned box of the `add` samples in the chosen space; the Chebyshev
//!   distance `d` outside the box (0 inside); matte = ramp(d, 0, fuzziness); a pixel within the
//!   `subtract` box (same rule) is kept: `matte = max(matte, 1 - ramp(d_sub, 0, fuzziness))`.
//! - despill: the screen channel `S` is limited to `L` = average | max | min of the other two:
//!   `S' = S - amount * max(S - L, 0)`; with `preserve_luminance` the lost luma `W_S (S - S')` is
//!   added to all three channels (clamped to 1). A neutral grey is unchanged.
//! - levels: `clamp((m - black) / (white - black), 0, 1)`.
//! - choke / spread: grey-level erosion (min, choke, negative radius) or dilation (max, spread,
//!   positive radius) over the disc `dx^2 + dy^2 <= r^2`, samples beyond the plane not read.
//! - feather: separable shared Gaussian, edges clamped to the border.
//! - edge-aware smoothing: a cross (joint) bilateral filter of the matte guided by the colour,
//!   window `(2r + 1)^2` clipped to the plane, weight `1 / (1 + |dC|^2 / sigma^2)` (Cauchy range
//!   kernel; `dC` the straight RGB difference to the centre pixel): flat-colour regions are
//!   smoothed, edges where the colour changes stay sharp.
//!
//! GPU: no render backend executes keying stages yet. [`gpu_admission`] returns the typed refusal
//! [`KeyError::GpuUnsupported`] (key [`KEY_GPU_REFUSAL`]) for every stage, so a GPU path refuses
//! instead of approximating.
//!
//! Hostile inputs (V4-01/02): parameters are range-checked (never clamped), planes are checked
//! (length, finite, `[0, 1]`) and the byte and work cost of the whole pipeline is computed with
//! checked arithmetic and refused against [`KeyLimits`] before any allocation.
#![deny(clippy::arithmetic_side_effects)]
use crate::Rect;
use crate::blend_math::Exactness;
use crate::coverage_math::feather_kernel;
use crate::retouch::RgbaPlane;
use serde::Serialize;
use std::fmt;

/// Class of every keying stage (see the module docs).
pub const KEY_EXACTNESS: Exactness = Exactness::Approximate;
/// Typed refusal key a GPU backend reports for a keying stage.
pub const KEY_GPU_REFUSAL: &str = "keying_gpu_unsupported";
/// Stages one pipeline may carry.
pub const MAX_KEY_STAGES: usize = 32;
/// Largest choke / spread radius.
pub const MAX_KEY_MORPH_RADIUS_PX: u32 = 256;
/// Largest feather radius.
pub const MAX_KEY_FEATHER_PX: f32 = 250.0;
/// Largest edge-aware smoothing radius.
pub const MAX_KEY_SMOOTH_RADIUS_PX: u32 = 64;
/// Largest difference-matte pre-blur radius.
pub const MAX_KEY_BLUR_PX: f32 = 100.0;
/// Samples one colour-range set may carry.
pub const MAX_RANGE_SAMPLES: usize = 256;

/// Byte and work budget of one pipeline run (work = sample visits).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyLimits {
    pub max_bytes: u64,
    pub max_work: u64,
}
impl KeyLimits {
    /// 1 GiB, 4e9 sample visits.
    pub const DEFAULT: Self = Self {
        max_bytes: 1 << 30,
        max_work: 4_000_000_000,
    };
}

/// Typed failure of the keying maths.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyError {
    /// A parameter is non-finite or outside its range.
    Parameter,
    /// A buffer length disagrees with its rectangle, or a clean plate's rectangle differs.
    Shape,
    /// A sample outside `[0, 1]` or non-finite.
    OutOfDomain,
    /// A difference matte without a clean plate.
    MissingCleanPlate,
    /// Bytes or work above the caller's limits (refused before allocation).
    OverBudget { needed: u64, limit: u64 },
    Overflow,
    /// No GPU backend executes this stage.
    GpuUnsupported { stage: &'static str },
}
impl KeyError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Parameter => "key_parameter",
            Self::Shape => "key_shape",
            Self::OutOfDomain => "key_out_of_domain",
            Self::MissingCleanPlate => "key_missing_clean_plate",
            Self::OverBudget { .. } => "key_over_budget",
            Self::Overflow => "key_overflow",
            Self::GpuUnsupported { .. } => KEY_GPU_REFUSAL,
        }
    }
}
impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for KeyError {}

/// Colour space of a chroma key distance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum ChromaSpace {
    YCbCr,
    Lab,
}
/// Colour space of a colour range key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum RangeSpace {
    Rgb,
    YCbCr,
    Lab,
}

/// Chroma key: matte from the distance to one screen colour.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ChromaKey {
    /// Screen colour, straight RGB in `[0, 1]`.
    pub key: [f32; 3],
    pub space: ChromaSpace,
    /// Distance at or below which the matte is 0, `0..=2`.
    pub tolerance: f32,
    /// Width of the ramp to 1 above the tolerance, `0..=2`.
    pub softness: f32,
    /// Weight of the luma (YCbCr) or lightness (Lab) difference, `0..=1`.
    pub luma_weight: f32,
}

/// Which luminances a luma key removes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum LumaKeyType {
    KeyOutDarker,
    KeyOutBrighter,
    KeyOutSimilar,
    KeyOutDissimilar,
}
/// Luma key: matte from luminance.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct LumaKey {
    pub kind: LumaKeyType,
    /// `0..=1`.
    pub threshold: f32,
    /// `0..=1` (similar / dissimilar only).
    pub tolerance: f32,
    /// `0..=1`.
    pub softness: f32,
}

/// Difference matte against a clean plate.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct DifferenceMatte {
    /// `0..=1`.
    pub tolerance: f32,
    /// `0..=1`.
    pub softness: f32,
    /// Gaussian pre-blur of both planes, `0..=MAX_KEY_BLUR_PX`.
    pub blur_px: f32,
}

/// Colour range key: matte from sampled colour boxes.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ColourRangeKey {
    pub space: RangeSpace,
    /// Samples of the removed range, `1..=MAX_RANGE_SAMPLES`, straight RGB in `[0, 1]`.
    pub add: Vec<[f32; 3]>,
    /// Samples kept back out of the range, `0..=MAX_RANGE_SAMPLES`.
    pub subtract: Vec<[f32; 3]>,
    /// Width of the ramp outside the boxes, `0..=2`.
    pub fuzziness: f32,
}

/// The spill (screen) channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum SpillScreen {
    Green,
    Blue,
}
/// The limit the screen channel is reduced to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum SpillLimit {
    Average,
    Max,
    Min,
}
/// Spill suppression.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Despill {
    pub screen: SpillScreen,
    pub limit: SpillLimit,
    /// `0..=1`.
    pub amount: f32,
    pub preserve_luminance: bool,
}

/// How an extracted matte joins the current matte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum MatteCombine {
    Replace,
    Multiply,
    Min,
    Max,
}

/// One stack entry of a key pipeline (STU-CMP-033).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum KeyStage {
    Chroma(ChromaKey, MatteCombine),
    Luma(LumaKey, MatteCombine),
    Difference(DifferenceMatte, MatteCombine),
    ColourRange(ColourRangeKey, MatteCombine),
    /// Matte black / white point, `0 <= black < white <= 1`.
    Levels { black: f32, white: f32 },
    /// Choke (negative) or spread (positive) radius in pixels.
    ChokeSpread { radius_px: i32 },
    /// Feather radius in pixels.
    Feather { radius_px: f32 },
    /// Cross-bilateral smoothing radius and colour sigma (`0 < sigma <= 2`).
    EdgeSmooth { radius_px: u32, colour_sigma: f32 },
    Despill(Despill),
}
impl KeyStage {
    /// Operator key of the stage (the unit of capability).
    pub const fn key(&self) -> &'static str {
        match self {
            Self::Chroma(..) => "key_chroma",
            Self::Luma(..) => "key_luma",
            Self::Difference(..) => "key_difference",
            Self::ColourRange(..) => "key_colour_range",
            Self::Levels { .. } => "matte_levels",
            Self::ChokeSpread { .. } => "matte_choke_spread",
            Self::Feather { .. } => "matte_feather",
            Self::EdgeSmooth { .. } => "matte_edge_smooth",
            Self::Despill(_) => "key_despill",
        }
    }
    /// Range checks of the stage's parameters.
    pub fn validate(&self) -> Result<(), KeyError> {
        let unit = |v: f32| v.is_finite() && (0.0..=1.0).contains(&v);
        let two = |v: f32| v.is_finite() && (0.0..=2.0).contains(&v);
        let ok = match self {
            Self::Chroma(k, _) => {
                k.key.iter().all(|c| unit(*c)) && two(k.tolerance) && two(k.softness) && unit(k.luma_weight)
            }
            Self::Luma(k, _) => unit(k.threshold) && unit(k.tolerance) && unit(k.softness),
            Self::Difference(k, _) => {
                unit(k.tolerance)
                    && unit(k.softness)
                    && k.blur_px.is_finite()
                    && (0.0..=MAX_KEY_BLUR_PX).contains(&k.blur_px)
            }
            Self::ColourRange(k, _) => {
                (1..=MAX_RANGE_SAMPLES).contains(&k.add.len())
                    && k.subtract.len() <= MAX_RANGE_SAMPLES
                    && k.add.iter().chain(k.subtract.iter()).all(|c| c.iter().all(|v| unit(*v)))
                    && two(k.fuzziness)
            }
            Self::Levels { black, white } => unit(*black) && unit(*white) && black < white,
            Self::ChokeSpread { radius_px } => radius_px.unsigned_abs() <= MAX_KEY_MORPH_RADIUS_PX,
            Self::Feather { radius_px } => radius_px.is_finite() && (0.0..=MAX_KEY_FEATHER_PX).contains(radius_px),
            Self::EdgeSmooth { radius_px, colour_sigma } => {
                *radius_px <= MAX_KEY_SMOOTH_RADIUS_PX && colour_sigma.is_finite() && *colour_sigma > 0.0 && *colour_sigma <= 2.0
            }
            Self::Despill(d) => unit(d.amount),
        };
        if ok { Ok(()) } else { Err(KeyError::Parameter) }
    }
}

/// A matte: one value in `[0, 1]` per pixel of `bounds`, row-major.
#[derive(Clone, Debug, PartialEq)]
pub struct Matte {
    pub bounds: Rect,
    pub data: Vec<f32>,
}

/// Output of a pipeline: the colour (despilled, alpha multiplied by the matte) and the matte.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyResult {
    pub colour: RgbaPlane,
    pub matte: Matte,
}

/// The typed GPU refusal for a stage (no GPU backend executes keying).
pub fn gpu_admission(stage: &KeyStage) -> Result<(), KeyError> {
    Err(KeyError::GpuUnsupported { stage: stage.key() })
}

// ---- colour spaces ----

const W_R: f64 = 0.2126;
const W_G: f64 = 0.7152;
const W_B: f64 = 0.0722;

fn rgb(p: [f32; 4]) -> [f64; 3] {
    [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])]
}
fn rgb3(p: [f32; 3]) -> [f64; 3] {
    [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])]
}
fn luma(c: [f64; 3]) -> f64 {
    W_R * c[0] + W_G * c[1] + W_B * c[2]
}
fn ycbcr(c: [f64; 3]) -> [f64; 3] {
    let y = luma(c);
    [y, (c[2] - y) / 1.8556, (c[0] - y) / 1.5748]
}

/// Deterministic cube root (IEEE `+ - * /` only): a bit-pattern seed refined by Newton steps;
/// relative error below 1e-15 on `[1e-300, 1e300]` (zero and negatives return 0).
pub fn det_cbrt(x: f64) -> f64 {
    if x.is_nan() || x <= 0.0 {
        return 0.0;
    }
    // Seed: divide the biased exponent by three (classic bit hack), within a few percent.
    let bits = x.to_bits();
    let seed = f64::from_bits((bits / 3).wrapping_add(0x2A9F_7893_782D_A1CE));
    let mut y = seed;
    for _ in 0..8 {
        y = y - (y * y * y - x) / (3.0 * y * y);
    }
    y
}

fn lab(c: [f64; 3]) -> [f64; 3] {
    let x = (0.4124 * c[0] + 0.3576 * c[1] + 0.1805 * c[2]) / 0.95047;
    let y = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    let z = (0.0193 * c[0] + 0.1192 * c[1] + 0.9505 * c[2]) / 1.08883;
    let e = 216.0 / 24389.0;
    let k = 24389.0 / 27.0;
    let f = |t: f64| if t > e { det_cbrt(t) } else { (k * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [(116.0 * fy - 16.0) / 100.0, 5.0 * (fx - fy), 2.0 * (fy - fz)]
}

fn ramp(d: f64, t: f64, s: f64) -> f64 {
    if d <= t {
        0.0
    } else if s <= 0.0 {
        1.0
    } else {
        ((d - t) / s).min(1.0)
    }
}

fn to_space(space: RangeSpace, c: [f64; 3]) -> [f64; 3] {
    match space {
        RangeSpace::Rgb => c,
        RangeSpace::YCbCr => ycbcr(c),
        RangeSpace::Lab => lab(c),
    }
}

// ---- admission ----

fn check_plane(p: &RgbaPlane) -> Result<usize, KeyError> {
    p.bounds.end().map_err(|_| KeyError::Overflow)?;
    let n = u64::from(p.bounds.width)
        .checked_mul(u64::from(p.bounds.height))
        .ok_or(KeyError::Overflow)?;
    let n = usize::try_from(n).map_err(|_| KeyError::Overflow)?;
    if p.data.len() != n {
        return Err(KeyError::Shape);
    }
    if p.data.iter().any(|s| s.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))) {
        return Err(KeyError::OutOfDomain);
    }
    Ok(n)
}

/// Predicted peak bytes and work of [`run_key_pipeline`] over a plane of `n` pixels.
pub fn key_pipeline_cost(n: u64, stages: &[KeyStage]) -> Result<(u64, u64), KeyError> {
    // Colour copy (16 B) + matte (4 B) + scratch matte (4 B) + extraction matte (4 B).
    let mut bytes = n.checked_mul(28).ok_or(KeyError::Overflow)?;
    let mut work: u64 = 0;
    for stage in stages {
        let (b, w) = match stage {
            KeyStage::Difference(d, _) => {
                let taps = u64::try_from(feather_kernel(d.blur_px).map_err(|_| KeyError::Parameter)?.len())
                    .map_err(|_| KeyError::Overflow)?;
                // Two premultiplied planes and one scratch plane, two passes each.
                (n.checked_mul(48).ok_or(KeyError::Overflow)?, n.checked_mul(taps.checked_mul(4).ok_or(KeyError::Overflow)?).ok_or(KeyError::Overflow)?)
            }
            KeyStage::ChokeSpread { radius_px } => {
                let side = u64::from(radius_px.unsigned_abs()).checked_mul(2).and_then(|v| v.checked_add(1)).ok_or(KeyError::Overflow)?;
                (0, n.checked_mul(side.checked_mul(side).ok_or(KeyError::Overflow)?).ok_or(KeyError::Overflow)?)
            }
            KeyStage::Feather { radius_px } => {
                let taps = u64::try_from(feather_kernel(*radius_px).map_err(|_| KeyError::Parameter)?.len())
                    .map_err(|_| KeyError::Overflow)?;
                (0, n.checked_mul(taps.checked_mul(2).ok_or(KeyError::Overflow)?).ok_or(KeyError::Overflow)?)
            }
            KeyStage::EdgeSmooth { radius_px, .. } => {
                let side = u64::from(*radius_px).checked_mul(2).and_then(|v| v.checked_add(1)).ok_or(KeyError::Overflow)?;
                (0, n.checked_mul(side.checked_mul(side).ok_or(KeyError::Overflow)?).ok_or(KeyError::Overflow)?)
            }
            KeyStage::ColourRange(k, _) => {
                let samples = u64::try_from(k.add.len().saturating_add(k.subtract.len())).map_err(|_| KeyError::Overflow)?;
                (0, n.checked_add(samples).ok_or(KeyError::Overflow)?)
            }
            _ => (0, n),
        };
        bytes = bytes.max(n.checked_mul(28).ok_or(KeyError::Overflow)?.checked_add(b).ok_or(KeyError::Overflow)?);
        work = work.checked_add(w).ok_or(KeyError::Overflow)?;
    }
    Ok((bytes, work))
}

fn admit(n: usize, stages: &[KeyStage], limits: &KeyLimits) -> Result<(), KeyError> {
    let n = u64::try_from(n).map_err(|_| KeyError::Overflow)?;
    let (bytes, work) = key_pipeline_cost(n, stages)?;
    if bytes > limits.max_bytes {
        return Err(KeyError::OverBudget { needed: bytes, limit: limits.max_bytes });
    }
    if work > limits.max_work {
        return Err(KeyError::OverBudget { needed: work, limit: limits.max_work });
    }
    Ok(())
}

// ---- pipeline ----

/// Runs `stages` in order over `src` (straight RGBA in `[0, 1]`); `clean_plate` (same bounds) is
/// required by a difference matte. Everything is validated and the cost admitted before any
/// allocation.
pub fn run_key_pipeline(
    src: &RgbaPlane,
    stages: &[KeyStage],
    clean_plate: Option<&RgbaPlane>,
    limits: &KeyLimits,
) -> Result<KeyResult, KeyError> {
    if stages.len() > MAX_KEY_STAGES {
        return Err(KeyError::Parameter);
    }
    for s in stages {
        s.validate()?;
    }
    let n = check_plane(src)?;
    let needs_plate = stages.iter().any(|s| matches!(s, KeyStage::Difference(..)));
    if needs_plate {
        let plate = clean_plate.ok_or(KeyError::MissingCleanPlate)?;
        if plate.bounds != src.bounds {
            return Err(KeyError::Shape);
        }
        check_plane(plate)?;
    }
    admit(n, stages, limits)?;
    let w = usize::try_from(src.bounds.width).map_err(|_| KeyError::Overflow)?;
    let h = usize::try_from(src.bounds.height).map_err(|_| KeyError::Overflow)?;
    let mut colour = src.data.clone();
    let mut matte = vec![1.0f32; n];
    for stage in stages {
        match stage {
            KeyStage::Chroma(k, c) => {
                let e = chroma_values(&colour, k);
                combine(&mut matte, &e, *c);
            }
            KeyStage::Luma(k, c) => {
                let e = luma_values(&colour, k);
                combine(&mut matte, &e, *c);
            }
            KeyStage::Difference(k, c) => {
                let plate = clean_plate.ok_or(KeyError::MissingCleanPlate)?;
                let e = difference_values(&colour, &plate.data, w, h, k)?;
                combine(&mut matte, &e, *c);
            }
            KeyStage::ColourRange(k, c) => {
                let e = range_values(&colour, k);
                combine(&mut matte, &e, *c);
            }
            KeyStage::Levels { black, white } => {
                let (b, wp) = (f64::from(*black), f64::from(*white));
                for m in &mut matte {
                    *m = ((f64::from(*m) - b) / (wp - b)).clamp(0.0, 1.0) as f32;
                }
            }
            KeyStage::ChokeSpread { radius_px } => {
                matte = morph(&matte, w, h, *radius_px);
            }
            KeyStage::Feather { radius_px } => {
                let taps = feather_kernel(*radius_px).map_err(|_| KeyError::Parameter)?;
                matte = separable(&matte, w, h, &taps);
            }
            KeyStage::EdgeSmooth { radius_px, colour_sigma } => {
                matte = edge_smooth(&matte, &colour, w, h, *radius_px, *colour_sigma);
            }
            KeyStage::Despill(d) => despill(&mut colour, d),
        }
    }
    for (p, m) in colour.iter_mut().zip(&matte) {
        p[3] = (f64::from(p[3]) * f64::from(*m)) as f32;
    }
    Ok(KeyResult {
        colour: RgbaPlane {
            bounds: src.bounds,
            data: colour,
        },
        matte: Matte {
            bounds: src.bounds,
            data: matte,
        },
    })
}

fn combine(matte: &mut [f32], e: &[f32], c: MatteCombine) {
    for (m, v) in matte.iter_mut().zip(e) {
        *m = match c {
            MatteCombine::Replace => *v,
            MatteCombine::Multiply => (f64::from(*m) * f64::from(*v)) as f32,
            MatteCombine::Min => m.min(*v),
            MatteCombine::Max => m.max(*v),
        };
    }
}

fn chroma_values(colour: &[[f32; 4]], k: &ChromaKey) -> Vec<f32> {
    let key = rgb3(k.key);
    let lw = f64::from(k.luma_weight);
    let (t, s) = (f64::from(k.tolerance), f64::from(k.softness));
    let kref = match k.space {
        ChromaSpace::YCbCr => ycbcr(key),
        ChromaSpace::Lab => lab(key),
    };
    colour
        .iter()
        .map(|p| {
            let c = rgb(*p);
            let d = match k.space {
                ChromaSpace::YCbCr => {
                    let v = ycbcr(c);
                    let (dy, db, dr) = (lw * (v[0] - kref[0]), v[1] - kref[1], v[2] - kref[2]);
                    (db * db + dr * dr + dy * dy).sqrt()
                }
                ChromaSpace::Lab => {
                    let v = lab(c);
                    let (dl, da, db) = (lw * (v[0] - kref[0]), v[1] - kref[1], v[2] - kref[2]);
                    (dl * dl + da * da + db * db).sqrt()
                }
            };
            ramp(d, t, s) as f32
        })
        .collect()
}

fn luma_values(colour: &[[f32; 4]], k: &LumaKey) -> Vec<f32> {
    let (th, tol, s) = (f64::from(k.threshold), f64::from(k.tolerance), f64::from(k.softness));
    colour
        .iter()
        .map(|p| {
            let y = luma(rgb(*p));
            let m = match k.kind {
                LumaKeyType::KeyOutDarker => ramp(y - th, 0.0, s),
                LumaKeyType::KeyOutBrighter => ramp(th - y, 0.0, s),
                LumaKeyType::KeyOutSimilar => ramp((y - th).abs(), tol, s),
                LumaKeyType::KeyOutDissimilar => 1.0 - ramp((y - th).abs(), tol, s),
            };
            m as f32
        })
        .collect()
}

fn premultiplied(data: &[[f32; 4]]) -> Vec<[f32; 4]> {
    data.iter()
        .map(|p| {
            let a = f64::from(p[3]);
            [
                (f64::from(p[0]) * a) as f32,
                (f64::from(p[1]) * a) as f32,
                (f64::from(p[2]) * a) as f32,
                p[3],
            ]
        })
        .collect()
}

fn blur_rgba(data: &[[f32; 4]], w: usize, h: usize, taps: &[f32]) -> Vec<[f32; 4]> {
    let mut out = data.to_vec();
    for ch in 0..4 {
        let plane: Vec<f32> = data.iter().map(|p| p[ch]).collect();
        let b = separable(&plane, w, h, taps);
        for (o, v) in out.iter_mut().zip(b) {
            o[ch] = v;
        }
    }
    out
}

fn difference_values(
    colour: &[[f32; 4]],
    plate: &[[f32; 4]],
    w: usize,
    h: usize,
    k: &DifferenceMatte,
) -> Result<Vec<f32>, KeyError> {
    let mut a = premultiplied(colour);
    let mut b = premultiplied(plate);
    if k.blur_px > 0.0 {
        let taps = feather_kernel(k.blur_px).map_err(|_| KeyError::Parameter)?;
        a = blur_rgba(&a, w, h, &taps);
        b = blur_rgba(&b, w, h, &taps);
    }
    let (t, s) = (f64::from(k.tolerance), f64::from(k.softness));
    Ok(a.iter()
        .zip(&b)
        .map(|(p, q)| {
            let d = (0..4)
                .map(|c| (f64::from(p[c]) - f64::from(q[c])).abs())
                .fold(0.0f64, f64::max);
            ramp(d, t, s) as f32
        })
        .collect())
}

fn sample_box(space: RangeSpace, samples: &[[f32; 3]]) -> Option<([f64; 3], [f64; 3])> {
    let mut it = samples.iter().map(|s| to_space(space, rgb3(*s)));
    let first = it.next()?;
    let (mut lo, mut hi) = (first, first);
    for v in it {
        for c in 0..3 {
            lo[c] = lo[c].min(v[c]);
            hi[c] = hi[c].max(v[c]);
        }
    }
    Some((lo, hi))
}

fn box_distance(v: [f64; 3], b: ([f64; 3], [f64; 3])) -> f64 {
    (0..3)
        .map(|c| (b.0[c] - v[c]).max(v[c] - b.1[c]).max(0.0))
        .fold(0.0f64, f64::max)
}

fn range_values(colour: &[[f32; 4]], k: &ColourRangeKey) -> Vec<f32> {
    let fuzz = f64::from(k.fuzziness);
    let add = sample_box(k.space, &k.add);
    let sub = sample_box(k.space, &k.subtract);
    colour
        .iter()
        .map(|p| {
            let v = to_space(k.space, rgb(*p));
            let mut m = add.map_or(1.0, |b| ramp(box_distance(v, b), 0.0, fuzz));
            if let Some(b) = sub {
                m = m.max(1.0 - ramp(box_distance(v, b), 0.0, fuzz));
            }
            m as f32
        })
        .collect()
}

fn despill(colour: &mut [[f32; 4]], d: &Despill) {
    let (s, o1, o2, ws) = match d.screen {
        SpillScreen::Green => (1usize, 0usize, 2usize, W_G),
        SpillScreen::Blue => (2, 0, 1, W_B),
    };
    let amount = f64::from(d.amount);
    for p in colour.iter_mut() {
        if p[3] <= 0.0 {
            continue;
        }
        let (a, b) = (f64::from(p[o1]), f64::from(p[o2]));
        let limit = match d.limit {
            SpillLimit::Average => (a + b) / 2.0,
            SpillLimit::Max => a.max(b),
            SpillLimit::Min => a.min(b),
        };
        let sv = f64::from(p[s]);
        let excess = amount * (sv - limit).max(0.0);
        if excess <= 0.0 {
            continue;
        }
        p[s] = (sv - excess) as f32;
        if d.preserve_luminance {
            let lost = ws * excess;
            for v in p.iter_mut().take(3) {
                *v = (f64::from(*v) + lost).min(1.0) as f32;
            }
        }
    }
}

/// Index of `(x, y)` in a `w`-wide plane; callers keep `x < w`, `y < h` with `w * h` admitted.
fn idx(x: usize, y: usize, w: usize) -> usize {
    y.saturating_mul(w).saturating_add(x)
}

fn morph(m: &[f32], w: usize, h: usize, radius: i32) -> Vec<f32> {
    if radius == 0 {
        return m.to_vec();
    }
    let r = usize::try_from(radius.unsigned_abs()).unwrap_or(0);
    let r2 = r.saturating_mul(r);
    let dilate = radius > 0;
    let mut out = vec![0.0f32; m.len()];
    for y in 0..h {
        for x in 0..w {
            let mut acc = if dilate { 0.0f32 } else { 1.0f32 };
            for yy in y.saturating_sub(r)..=y.saturating_add(r).min(h.saturating_sub(1)) {
                let dy = yy.abs_diff(y);
                for xx in x.saturating_sub(r)..=x.saturating_add(r).min(w.saturating_sub(1)) {
                    let dx = xx.abs_diff(x);
                    if dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy)) > r2 {
                        continue;
                    }
                    let v = m[idx(xx, yy, w)];
                    acc = if dilate { acc.max(v) } else { acc.min(v) };
                }
            }
            out[idx(x, y, w)] = acc;
        }
    }
    out
}

/// Separable convolution with `taps` (odd length), edges clamped to the border.
fn separable(m: &[f32], w: usize, h: usize, taps: &[f32]) -> Vec<f32> {
    if taps.len() <= 1 {
        return m.to_vec();
    }
    let support = taps.len() / 2;
    let clampi = |v: usize, lo_off: usize, n: usize| -> usize {
        // v + (t - support) clamped to [0, n - 1] where t = lo_off
        let up = v.saturating_add(lo_off);
        if up < support { 0 } else { (up.saturating_sub(support)).min(n.saturating_sub(1)) }
    };
    let mut tmp = vec![0.0f32; m.len()];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0f64;
            for (t, k) in taps.iter().enumerate() {
                acc += f64::from(*k) * f64::from(m[idx(clampi(x, t, w), y, w)]);
            }
            tmp[idx(x, y, w)] = acc as f32;
        }
    }
    let mut out = vec![0.0f32; m.len()];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0f64;
            for (t, k) in taps.iter().enumerate() {
                acc += f64::from(*k) * f64::from(tmp[idx(x, clampi(y, t, h), w)]);
            }
            out[idx(x, y, w)] = (acc.clamp(0.0, 1.0)) as f32;
        }
    }
    out
}

fn edge_smooth(m: &[f32], colour: &[[f32; 4]], w: usize, h: usize, r: u32, sigma: f32) -> Vec<f32> {
    if r == 0 {
        return m.to_vec();
    }
    let r = usize::try_from(r).unwrap_or(0);
    let inv_s2 = 1.0 / (f64::from(sigma) * f64::from(sigma));
    let mut out = vec![0.0f32; m.len()];
    for y in 0..h {
        for x in 0..w {
            let c0 = rgb(colour[idx(x, y, w)]);
            let (mut num, mut den) = (0.0f64, 0.0f64);
            for yy in y.saturating_sub(r)..=y.saturating_add(r).min(h.saturating_sub(1)) {
                for xx in x.saturating_sub(r)..=x.saturating_add(r).min(w.saturating_sub(1)) {
                    let i = idx(xx, yy, w);
                    let c = rgb(colour[i]);
                    let (d0, d1, d2c) = (c[0] - c0[0], c[1] - c0[1], c[2] - c0[2]);
                    let d2 = d0 * d0 + d1 * d1 + d2c * d2c;
                    let wt = 1.0 / (1.0 + d2 * inv_s2);
                    num += wt * f64::from(m[i]);
                    den += wt;
                }
            }
            out[idx(x, y, w)] = (num / den).clamp(0.0, 1.0) as f32;
        }
    }
    out
}
