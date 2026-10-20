//! Single owner (CX-CAP-001) of the coverage modifiers that scale a layer's source alpha before
//! source-over: mask density / invert (STU-RAS-115/124), the mask feather kernel, and the Blend-If
//! split-slider weight (STU-RAS-159). Decision: work-folder
//! `diagnostics/adjustments-advanced-blending.md`. Backends (render-cpu, render-gpu) consume these
//! functions and do not define a second formula.
//!
//! Scope: scalar reference maths on values already in the plan's blend space (STU-COL-163). All
//! inputs are fractions in `[0, 1]`; non-finite or out-of-range inputs are a typed refusal, not
//! clamped in (`tests/adjust_coverage.rs` `mask_density_invert_feather_and_blend_if`). Every operator here is class `Approximate`: the formulas are documented Studio
//! readings of Adobe help text, not Adobe-published maths (oracle list D5, cases 9-10).
use crate::blend_math::{Exactness, lum};
use serde::Serialize;
use std::fmt;

/// Typed domain failure of a coverage operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoverageError {
    /// A coverage, density, colour or range value was non-finite or outside `[0, 1]`.
    OutOfDomain,
    /// Feather radius non-finite, negative or above [`MAX_FEATHER_PX`].
    FeatherOutOfRange,
    /// A Blend-If pair is not ordered (`start > end`).
    RangeOrder,
}
impl CoverageError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::OutOfDomain => "coverage_out_of_domain",
            Self::FeatherOutOfRange => "coverage_feather_out_of_range",
            Self::RangeOrder => "coverage_range_order",
        }
    }
}
impl fmt::Display for CoverageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for CoverageError {}

fn unit(v: f32) -> bool {
    v.is_finite() && (0.0..=1.0).contains(&v)
}

/// Exactness of every operator in this module (see module doc).
pub const COVERAGE_EXACTNESS: Exactness = Exactness::Approximate;

/// Effective mask coverage after invert and density (STU-RAS-115 `StudioMask.density`, the
/// STU-RAS-124 invert command): `m' = inverted ? 1 - m : m`, then `1 - density * (1 - m')`.
///
/// Reading of the Adobe Photoshop help (Properties panel, "Density": at 100 % the mask is opaque,
/// lowering it makes the hidden areas progressively visible): density 0 reveals everything and
/// density 1 is the plain mask; linear in between. Class Approximate (D5 case 10).
pub fn mask_coverage(coverage: f32, inverted: bool, density: f32) -> Result<f32, CoverageError> {
    if !unit(coverage) || !unit(density) {
        return Err(CoverageError::OutOfDomain);
    }
    let m = if inverted { 1.0 - coverage } else { coverage };
    Ok((1.0 - density * (1.0 - m)).clamp(0.0, 1.0))
}

/// Hard maximum of `StudioMask.feather` in pixels (STU-RAS-115 hard range `0..=10000`).
pub const MAX_FEATHER_PX: f32 = 10_000.0;

/// Gaussian standard deviation for a feather radius in pixels. Studio rule: `sigma = radius / 2`,
/// so the kernel support `ceil(3 sigma)` reaches 1.5 radii. Photoshop's radius-to-sigma relation
/// is UNVERIFIED (D5 case 10); class Approximate. `0` means no feather.
pub fn feather_sigma(radius_px: f32) -> Result<f32, CoverageError> {
    if !radius_px.is_finite() || !(0.0..=MAX_FEATHER_PX).contains(&radius_px) {
        return Err(CoverageError::FeatherOutOfRange);
    }
    Ok(radius_px / 2.0)
}

/// Half-width in pixels of the feather kernel (`ceil(3 sigma)`); the margin a backend must read
/// around a feathered mask.
pub fn feather_support(radius_px: f32) -> Result<u32, CoverageError> {
    let sigma = feather_sigma(radius_px)?;
    Ok((3.0 * sigma).ceil() as u32)
}

/// Normalised 1-D Gaussian taps `[-support ..= support]` for a separable feather (sum = 1). A
/// zero radius returns the identity kernel `[1.0]`. Backends blur the coverage plane with this
/// kernel horizontally then vertically (edge samples clamp to the plane border) BEFORE density.
pub fn feather_kernel(radius_px: f32) -> Result<Vec<f32>, CoverageError> {
    let sigma = feather_sigma(radius_px)?;
    if sigma <= 0.0 {
        return Ok(vec![1.0]);
    }
    let support = feather_support(radius_px)? as i64;
    let two_s2 = 2.0 * f64::from(sigma) * f64::from(sigma);
    let taps: Vec<f64> = (-support..=support)
        .map(|i| (-((i * i) as f64) / two_s2).exp())
        .collect();
    let sum: f64 = taps.iter().sum();
    Ok(taps.iter().map(|t| (t / sum) as f32).collect())
}

/// Channel an entry of a Blend-If range set refers to (STU-RAS-159: the composite gray entry is
/// one member of the per-channel set).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum BlendChannel {
    Gray,
    Red,
    Green,
    Blue,
}
impl BlendChannel {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Gray => "gray",
            Self::Red => "red",
            Self::Green => "green",
            Self::Blue => "blue",
        }
    }
    /// The channel value of a straight RGB colour. Gray uses [`lum`] (W3C 0.3/0.59/0.11, the one
    /// luminance owner); Blend-If reads [`blend_if_value`], whose gray uses Photoshop's 15-bit weights
    /// (D5 PS-C09, [`BLEND_IF_GRAY_WEIGHTS`]).
    pub fn value(self, rgb: [f32; 3]) -> f32 {
        match self {
            Self::Gray => lum(rgb),
            Self::Red => rgb[0],
            Self::Green => rgb[1],
            Self::Blue => rgb[2],
        }
    }
}

/// One split-slider range `[black_start, black_end, white_start, white_end]` as fractions
/// (`level / 255`). Identity: `[0, 0, 1, 1]`.
pub type SplitRange = [f32; 4];
/// The identity range (blend everything).
pub const IDENTITY_RANGE: SplitRange = [0.0, 0.0, 1.0, 1.0];

/// Validate one split range: values in `[0, 1]`, each pair ordered (`start <= end`).
pub fn check_range(r: SplitRange) -> Result<(), CoverageError> {
    if !r.iter().all(|v| unit(*v)) {
        return Err(CoverageError::OutOfDomain);
    }
    if r[0] > r[1] || r[2] > r[3] {
        return Err(CoverageError::RangeOrder);
    }
    Ok(())
}

/// Weight of value `v` under a split range: `ramp_up(v; bs, be) * ramp_down(v; ws, we)` with
/// Photoshop's inclusive integer ramps on the 0..255 slider scale (lane SMB-PS-ORACLE-PARITY-1,
/// measured on the D5 oracle PS-C09): in levels (`x * 255`), `up = clamp((v - bs + 1) / (be - bs +
/// 1))` and `down = clamp((we + 1 - v) / (we - ws + 1))`. A split 50/100 reaches 1/51 at level 50
/// and 1 at level 100; a collapsed pair (`bs == be`) is a hard cut between levels `bs - 1` and
/// `bs` (`v == bs` blended). The this-red, underlying-gray and underlying-red variants reproduce
/// within 1 code (the previous continuous ramp `(v - bs) / (be - bs)` differed by up to 6), the hard
/// 100/100/200/200 variant exactly. Values between two levels (16-bit documents) follow the same
/// line, UNVERIFIED.
pub fn range_weight(v: f32, r: SplitRange) -> Result<f32, CoverageError> {
    if !unit(v) {
        return Err(CoverageError::OutOfDomain);
    }
    check_range(r)?;
    let [bs, be, ws, we] = r.map(|x| x * 255.0);
    let v = v * 255.0;
    let up = ((v - bs + 1.0) / (be - bs + 1.0)).clamp(0.0, 1.0);
    let down = ((we + 1.0 - v) / (we - ws + 1.0)).clamp(0.0, 1.0);
    Ok(up * down)
}

/// One Blend-If entry: a channel with a range on this layer's colour (`source`, "This Layer") and
/// on the backdrop colour (`destination`, "Underlying Layer").
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct BlendRangeEntry {
    pub channel: BlendChannel,
    pub source: SplitRange,
    pub destination: SplitRange,
}
impl BlendRangeEntry {
    /// An entry that changes nothing.
    pub const fn identity(channel: BlendChannel) -> Self {
        Self {
            channel,
            source: IDENTITY_RANGE,
            destination: IDENTITY_RANGE,
        }
    }
}

/// Blend-If weight for one pixel over an opaque backdrop: [`blend_if_weight_over`] with backdrop
/// alpha 1.
pub fn blend_if_weight(
    entries: &[BlendRangeEntry],
    source_rgb: [f32; 3],
    backdrop_rgb: [f32; 3],
) -> Result<f32, CoverageError> {
    blend_if_weight_over(entries, source_rgb, backdrop_rgb, 1.0)
}

/// Photoshop's Blend-If gray weights in 15-bit fixed point (lane SMB-PS-ORACLE-PARITY-2):
/// `0.30 / 0.59 / 0.11` as `9830 / 19334 / 3604` out of 32768 (the 15-bit roundings `9830.4 ->
/// 9830`, `3604.48 -> 3604`, green taking the remainder so the sum is 32768). Measured on the D5
/// oracle PS-C09 `this_gray_50_100__200_240`: on the exact halves of the 0.3/0.59/0.11 sum the
/// red-only and magenta bands round down (red 185 -> 55, magenta 150 -> 61) while the cyan and
/// yellow bands round up (cyan 75 -> 53, yellow 250 -> 223), which these weights reproduce (every
/// sample within one 8-bit code, 176 samples up to 5 codes with the W3C weights); the hard and the
/// Underlying gray variants stay exact.
pub const BLEND_IF_GRAY_WEIGHTS: [f32; 3] = [9830.0, 19334.0, 3604.0];

/// The channel value a Blend-If range reads: [`BlendChannel::value`] for red, green and blue; the
/// gray is [`BLEND_IF_GRAY_WEIGHTS`] on the 0..255 scale rounded to a whole level
/// (`round((9830 R + 19334 G + 3604 B) / 32768) / 255`; D5 PS-C09: the hard 100/100/200/200 gray
/// variant is exact only with the rounded gray, lane SMB-PS-ORACLE-PARITY-1). Evaluated in binary32
/// in this order by every backend.
pub fn blend_if_value(channel: BlendChannel, rgb: [f32; 3]) -> f32 {
    if channel == BlendChannel::Gray {
        let [wr, wg, wb] = BLEND_IF_GRAY_WEIGHTS;
        let level = (wr * rgb[0] + wg * rgb[1] + wb * rgb[2]) * (255.0 / 32768.0);
        (level.round() / 255.0).clamp(0.0, 1.0)
    } else {
        channel.value(rgb)
    }
}

/// Blend-If weight for one pixel: the product over entries of the source-range weight of the
/// layer colour and the destination-range weight of the backdrop colour. It multiplies the
/// layer's source alpha before source-over. An empty set is weight 1. `backdrop_alpha` (lane
/// SMB-PS-ORACLE-PARITY-1, D5 PS-C23): an Underlying range hides a layer only where something lies
/// below it, so each destination weight `w` counts as `1 - ab (1 - w)` (a layer alone in an isolated
/// group stays visible, as in Photoshop; the opaque backdrop keeps `w`); the rule between the two
/// end points is UNVERIFIED.
pub fn blend_if_weight_over(
    entries: &[BlendRangeEntry],
    source_rgb: [f32; 3],
    backdrop_rgb: [f32; 3],
    backdrop_alpha: f32,
) -> Result<f32, CoverageError> {
    if !unit(backdrop_alpha) {
        return Err(CoverageError::OutOfDomain);
    }
    if !source_rgb
        .iter()
        .chain(backdrop_rgb.iter())
        .all(|v| unit(*v))
    {
        return Err(CoverageError::OutOfDomain);
    }
    let mut w = 1.0f32;
    for e in entries {
        w *= range_weight(blend_if_value(e.channel, source_rgb), e.source)?;
        let under = range_weight(blend_if_value(e.channel, backdrop_rgb), e.destination)?;
        w *= if backdrop_alpha >= 1.0 {
            under
        } else {
            1.0 - backdrop_alpha * (1.0 - under)
        };
    }
    Ok(w)
}
