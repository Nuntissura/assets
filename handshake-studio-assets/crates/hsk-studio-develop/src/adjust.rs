//! Develop adjustment controls (lane SMB-DEVELOP-CONTROLS): the Basic panel (white balance shift,
//! contrast, highlights, shadows, whites, blacks, vibrance, saturation, clarity), the 8-band colour
//! mixer and the parametric tone-curve regions. Spec basis: STU-RAW-110 (incremental temperature and
//! tint, soft range -100..100, a relative offset), STU-RAW-111 (basic tone and presence, soft range
//! -100..100, identity 0), STU-RAW-112 (parametric curve: four region amounts and three split points),
//! STU-RAW-115 (eight fixed hue bands, hue/saturation/luminance each, identity 0).
//!
//! Every control is a versioned recipe parameter ([`ADJUST_VERSION`]) with a deterministic binary64
//! scalar implementation (no FMA, no SIMD, no unsafe). All numeric mappings below are this crate's
//! own documented choices: Lightroom-matching values are UNVERIFIED (oracle session D5), nothing here
//! claims parity.
//!
//! Domains and policies (each documented because nothing is silently clipped or repaired):
//! * values arrive as scene-linear working RGB (f64); tone controls, vibrance, saturation, clarity
//!   and the mixer work on the signed sRGB-encoded value `enc(c)` (sign-symmetric, continuous and
//!   differentiable past 1, so HDR values keep their order); the result is decoded back with `dec`;
//! * the tone controls build a monotone cubic Hermite curve (PCHIP) per recipe and apply it to each
//!   channel; its input below 0 continues with slope 1 and above 1 with the last segment's slope;
//! * a pixel with any negative channel skips vibrance, saturation and the mixer (their hue is
//!   undefined there); tone controls still apply to it;
//! * a slider outside its soft range, a non-finite number or an out-of-order split refuses with
//!   `invalid_adjustment` / `non_finite` (the spec leaves hard bounds `unknown`; refusing is the
//!   conservative reading, not a claim about Lightroom).
use crate::DevelopError;
use crate::color::{self, D50_XYZ, Mat3, SRGB_TO_XYZ_D50};

/// Version of the adjustment maths. Bump when any mapping below changes.
pub const ADJUST_VERSION: (u32, u32) = (1, 3);
/// Oldest recipe version still read: 1.0 recipes lack the 1.1 families and 1.0/1.1 recipes the 1.2
/// families (optics, geometry), which are identity.
pub const ADJUST_MIN_VERSION: (u32, u32) = (1, 0);

/// Soft range of every count-scale slider (STU-RAW-110/111/112/115).
pub const COUNT_LIMIT: f64 = 100.0;

/// Rec.709 / sRGB luma weights. The recipe has no knowledge of the working profile's primaries, so
/// the caller sets [`Adjustments::luma`] when the working space is not sRGB-primaried.
pub const REC709_LUMA: [f64; 3] = [0.212_672_9, 0.715_152_2, 0.072_175];

/// Colour-mixer band names in spec order (STU-RAW-115) and the hue (degrees) each band is centred on.
/// The centres are this crate's choice (UNVERIFIED against Lightroom).
pub const BAND_NAMES: [&str; 8] = ["red", "orange", "yellow", "green", "aqua", "blue", "purple", "magenta"];
pub const BAND_CENTRES: [f64; 8] = [0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 270.0, 300.0];
/// Maximum hue rotation of a band at a slider of +-100.
pub const BAND_HUE_DEGREES: f64 = 30.0;

/// Incremental (non-raw form) white balance: a relative offset, never kelvin (STU-RAW-110).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WhiteBalanceShift {
    pub temperature: f64,
    pub tint: f64,
}

/// Current-generation tone sliders (STU-RAW-111). Exposure stays `DevelopRecipe::exposure_stops`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BasicTone {
    pub contrast: f64,
    pub highlights: f64,
    pub shadows: f64,
    pub whites: f64,
    pub blacks: f64,
}

/// Presence sliders (STU-RAW-111). Clarity needs a whole frame (local contrast), see [`clarity`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Presence {
    pub vibrance: f64,
    pub saturation: f64,
    pub clarity: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HslBand {
    pub hue: f64,
    pub saturation: f64,
    pub luminance: f64,
}

/// Eight fixed bands in [`BAND_NAMES`] order.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ColourMixer {
    pub bands: [HslBand; 8],
}

/// Parametric tone curve (STU-RAW-112): region amounts and the three split points in `0..100`.
/// The shadow split default 25 is `derived` in the spec; 50 and 75 for the other two are this
/// crate's choice (UNVERIFIED).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParametricCurve {
    pub highlights: f64,
    pub lights: f64,
    pub darks: f64,
    pub shadows: f64,
    pub shadow_split: f64,
    pub midtone_split: f64,
    pub highlight_split: f64,
}

impl Default for ParametricCurve {
    fn default() -> Self {
        Self {
            highlights: 0.0,
            lights: 0.0,
            darks: 0.0,
            shadows: 0.0,
            shadow_split: 25.0,
            midtone_split: 50.0,
            highlight_split: 75.0,
        }
    }
}

/// All global adjustment parameters of a recipe. Retained as authored even while their group is
/// disabled; see [`crate::recipe::Enables`] for the gating of each family.
#[derive(Clone, Debug, PartialEq)]
pub struct Adjustments {
    pub version: (u32, u32),
    pub white_balance: WhiteBalanceShift,
    pub tone: BasicTone,
    pub presence: Presence,
    pub mixer: ColourMixer,
    pub parametric: ParametricCurve,
    /// Working-space RGB -> XYZ D50 (colorants), used by the white-balance shift. Default sRGB.
    pub working_to_xyz_d50: Mat3,
    pub luma: [f64; 3],
    /// Local corrections (gradient and brush masks); applied after the global stages by
    /// `develop_to_image` when `enable_mask_group_based_corrections` is set. Whole-frame only.
    pub local: Vec<crate::local::Correction>,
    /// Absolute (raw form) white balance request that produced the recipe's resolved neutral and
    /// matrix; must be `Some` exactly when `white_balance_mode` is `Temperature` (STU-RAW-110).
    pub absolute_white_balance: Option<AbsoluteWhiteBalance>,
    pub point_colours: Vec<crate::colour::PointColour>,
    pub grayscale: crate::colour::GrayscaleMix,
    pub grading: crate::colour::ColourGrading,
    pub detail: crate::detail::Detail,
    /// STU-RAW-117 lens corrections (gated by `enable_lens_corrections`; whole frame).
    pub optics: crate::optics::Optics,
    /// STU-RAW-118 geometry (gated by `enable_transform`; whole frame).
    pub geometry: crate::geometry::Geometry,
    /// STU-RAW-119 grain and post-crop vignette (gated by `enable_effects`; whole frame, last).
    pub effects: crate::effects::Effects,
    /// STU-RAW-120 calibration (gated by `enable_calibration`; per pixel, first in stage C).
    pub calibration: crate::effects::Calibration,
}

/// Raw-form white balance (STU-RAW-110): kelvin 2000..25000 (the Kim Planckian approximation ends at
/// 25000; the spec's soft maximum 50000 is NOT reached, refused) and tint -150..150.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AbsoluteWhiteBalance {
    pub kelvin: f64,
    pub tint: f64,
}

/// uv distance of the illuminant from the Planckian locus per tint count (UNVERIFIED scale).
pub const TINT_UV_PER_COUNT: f64 = 1.0 / 3000.0;

/// Illuminant chromaticity for a kelvin / tint request: the Planckian point, displaced along the
/// locus normal toward green for tint > 0 (a greener illuminant is corrected with magenta).
pub fn white_xy(kelvin: f64, tint: f64) -> Result<[f64; 2], DevelopError> {
    if !kelvin.is_finite() || !tint.is_finite() {
        return Err(DevelopError::NonFinite);
    }
    if !(2000.0..=25000.0).contains(&kelvin) || tint.abs() > 150.0 {
        return Err(DevelopError::InvalidAdjustment);
    }
    let mired = 1.0e6 / kelvin;
    let at = |m: f64| planck_xy(1.0e6 / m).map(xy_to_uv);
    let uv = at(mired)?;
    let (a, b) = (at((mired - 5.0).max(40.0))?, at(mired + 5.0)?);
    let (tu, tv) = (b[0] - a[0], b[1] - a[1]);
    let len = tu.hypot(tv);
    if !len.is_finite() || len == 0.0 {
        return Err(DevelopError::ColorNoConvergence);
    }
    let (mut nu, mut nv) = (-tv / len, tu / len);
    if nv < 0.0 {
        nu = -nu;
        nv = -nv;
    }
    Ok(uv_to_xy([uv[0] + tint * TINT_UV_PER_COUNT * nu, uv[1] + tint * TINT_UV_PER_COUNT * nv]))
}

/// Which adjustment groups are enabled (the STU-RAW-105 booleans this module reads).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Gates {
    pub colour: bool,
    pub curve: bool,
    pub detail: bool,
    pub mask: bool,
    pub grayscale: bool,
    pub lens: bool,
    pub transform: bool,
    pub effects: bool,
    pub calibration: bool,
}

impl Gates {
    pub(crate) const NONE: Self =
        Self { colour: false, curve: false, detail: false, mask: false, grayscale: false, lens: false, transform: false, effects: false, calibration: false };
    pub(crate) const ALL_PIXEL: Self =
        Self { colour: true, curve: true, detail: true, mask: false, grayscale: true, lens: false, transform: false, effects: false, calibration: true };

    pub(crate) fn from_enables(e: &crate::recipe::Enables) -> Self {
        Self {
            colour: e.enable_colour_adjustments,
            curve: e.enable_tone_curve,
            detail: e.enable_detail,
            mask: e.enable_mask_group_based_corrections,
            grayscale: e.enable_grayscale_mix,
            lens: e.enable_lens_corrections,
            transform: e.enable_transform,
            effects: e.enable_effects,
            calibration: e.enable_calibration,
        }
    }
}

impl Default for Adjustments {
    fn default() -> Self {
        Self {
            version: ADJUST_VERSION,
            white_balance: WhiteBalanceShift::default(),
            tone: BasicTone::default(),
            presence: Presence::default(),
            mixer: ColourMixer::default(),
            parametric: ParametricCurve::default(),
            working_to_xyz_d50: SRGB_TO_XYZ_D50,
            luma: REC709_LUMA,
            local: Vec::new(),
            absolute_white_balance: None,
            point_colours: Vec::new(),
            grayscale: crate::colour::GrayscaleMix::default(),
            grading: crate::colour::ColourGrading::default(),
            detail: crate::detail::Detail::default(),
            optics: crate::optics::Optics::default(),
            geometry: crate::geometry::Geometry::default(),
            effects: crate::effects::Effects::default(),
            calibration: crate::effects::Calibration::default(),
        }
    }
}

pub(crate) fn count(value: f64) -> Result<f64, DevelopError> {
    if !value.is_finite() {
        return Err(DevelopError::NonFinite);
    }
    if value.abs() > COUNT_LIMIT {
        return Err(DevelopError::InvalidAdjustment);
    }
    Ok(value)
}

// ---------------------------------------------------------------------------------------------
// Transfer: signed sRGB OETF / EOTF (extended; odd-symmetric, C1 at the joins, defined for all f64).

pub(crate) fn enc(c: f64) -> f64 {
    let a = c.abs();
    let e = if a <= 0.003_130_8 { 12.92 * a } else { 1.055 * a.powf(1.0 / 2.4) - 0.055 };
    if c < 0.0 { -e } else { e }
}

pub(crate) fn dec(e: f64) -> f64 {
    let a = e.abs();
    let c = if a <= 0.040_45 { a / 12.92 } else { ((a + 0.055) / 1.055).powf(2.4) };
    if e < 0.0 { -c } else { c }
}

// ---------------------------------------------------------------------------------------------
// Monotone cubic Hermite (Fritsch-Carlson limited slopes), the same slope rule as `curves.rs`.

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Pchip {
    xs: Vec<f64>,
    ys: Vec<f64>,
    m: Vec<f64>,
}

impl Pchip {
    /// `xs` strictly ascending, `ys` nondecreasing, all finite, at least two points.
    pub(crate) fn new(xs: Vec<f64>, ys: Vec<f64>) -> Result<Self, DevelopError> {
        let n = xs.len();
        if n < 2 || ys.len() != n {
            return Err(DevelopError::InvalidAdjustment);
        }
        if xs.iter().chain(ys.iter()).any(|v| !v.is_finite())
            || xs.windows(2).any(|w| w[1] <= w[0])
            || ys.windows(2).any(|w| w[1] < w[0])
        {
            return Err(DevelopError::InvalidAdjustment);
        }
        let d: Vec<f64> = (0..n - 1).map(|i| (ys[i + 1] - ys[i]) / (xs[i + 1] - xs[i])).collect();
        let mut m = vec![0.0f64; n];
        m[0] = d[0];
        m[n - 1] = d[n - 2];
        for i in 1..n - 1 {
            m[i] = if d[i - 1] * d[i] <= 0.0 { 0.0 } else { (d[i - 1] + d[i]) / 2.0 };
        }
        for i in 0..n - 1 {
            if d[i] == 0.0 {
                m[i] = 0.0;
                m[i + 1] = 0.0;
            } else {
                let a = m[i] / d[i];
                let b = m[i + 1] / d[i];
                let s = a * a + b * b;
                if s > 9.0 {
                    let q = 3.0 / s.sqrt();
                    m[i] = (q * a) * d[i];
                    m[i + 1] = (q * b) * d[i];
                }
            }
        }
        if m.iter().any(|v| !v.is_finite()) {
            return Err(DevelopError::InvalidAdjustment);
        }
        Ok(Self { xs, ys, m })
    }

    pub(crate) fn eval(&self, x: f64) -> f64 {
        let n = self.xs.len();
        if x <= self.xs[0] {
            return self.ys[0] + (x - self.xs[0]);
        }
        if x >= self.xs[n - 1] {
            return self.ys[n - 1] + (x - self.xs[n - 1]) * self.m[n - 1];
        }
        let upper = self.xs.iter().map(|k| usize::from(*k < x)).sum::<usize>();
        let i = upper.saturating_sub(1).min(n - 2);
        let h = self.xs[i + 1] - self.xs[i];
        let t = (x - self.xs[i]) / h;
        let t2 = t * t;
        let t3 = (t * t) * t;
        let a = (2.0 * t3 - 3.0 * t2 + 1.0) * self.ys[i];
        let b = ((t3 - 2.0 * t2 + t) * h) * self.m[i];
        let c = (-2.0 * t3 + 3.0 * t2) * self.ys[i + 1];
        let d = ((t3 - t2) * h) * self.m[i + 1];
        ((a + b) + c) + d
    }
}

fn cumulative_max(ys: &mut [f64]) {
    for i in 1..ys.len() {
        if ys[i] < ys[i - 1] {
            ys[i] = ys[i - 1];
        }
    }
}

const TONE_XS: [f64; 7] = [0.0, 0.125, 0.25, 0.5, 0.75, 0.875, 1.0];
// Region weights per knot and full-scale amount (encoded units) for a slider of +-100.
const W_BLACKS: [f64; 7] = [1.0, 0.5, 0.15, 0.0, 0.0, 0.0, 0.0];
const W_SHADOWS: [f64; 7] = [0.0, 0.5, 1.0, 0.4, 0.0, 0.0, 0.0];
const W_HIGHLIGHTS: [f64; 7] = [0.0, 0.0, 0.0, 0.4, 1.0, 0.5, 0.0];
const W_WHITES: [f64; 7] = [0.0, 0.0, 0.0, 0.0, 0.15, 0.5, 1.0];
const A_BLACKS: f64 = 0.12;
const A_SHADOWS: f64 = 0.20;
const A_HIGHLIGHTS: f64 = 0.20;
const A_WHITES: f64 = 0.12;
/// Contrast: `x - K*(c/100)*x(1-x)(1-2x)`; slope at the ends `1-K`, at the middle `1+K/2` (K = 0.6).
const K_CONTRAST: f64 = 0.6;

fn basic_tone_curve(tone: &BasicTone) -> Result<Option<Pchip>, DevelopError> {
    let c = count(tone.contrast)? / COUNT_LIMIT;
    let h = count(tone.highlights)? / COUNT_LIMIT;
    let s = count(tone.shadows)? / COUNT_LIMIT;
    let w = count(tone.whites)? / COUNT_LIMIT;
    let b = count(tone.blacks)? / COUNT_LIMIT;
    if c == 0.0 && h == 0.0 && s == 0.0 && w == 0.0 && b == 0.0 {
        return Ok(None);
    }
    let mut ys = Vec::with_capacity(TONE_XS.len());
    for (i, x) in TONE_XS.iter().enumerate() {
        let g = (x * (1.0 - x)) * (1.0 - 2.0 * x);
        let y = ((((x - (K_CONTRAST * c) * g) + (b * A_BLACKS) * W_BLACKS[i])
            + (s * A_SHADOWS) * W_SHADOWS[i])
            + (h * A_HIGHLIGHTS) * W_HIGHLIGHTS[i])
            + (w * A_WHITES) * W_WHITES[i];
        ys.push(y);
    }
    cumulative_max(&mut ys);
    Pchip::new(TONE_XS.to_vec(), ys).map(Some)
}

/// Parametric regions: the curve moves the knot at each region's centre by `amount * 0.15`.
const A_REGION: f64 = 0.15;

fn parametric_curve(p: &ParametricCurve) -> Result<Option<Pchip>, DevelopError> {
    let hi = count(p.highlights)? / COUNT_LIMIT;
    let li = count(p.lights)? / COUNT_LIMIT;
    let da = count(p.darks)? / COUNT_LIMIT;
    let sh = count(p.shadows)? / COUNT_LIMIT;
    for split in [p.shadow_split, p.midtone_split, p.highlight_split] {
        if !split.is_finite() {
            return Err(DevelopError::NonFinite);
        }
    }
    let (s1, s2, s3) = (p.shadow_split / 100.0, p.midtone_split / 100.0, p.highlight_split / 100.0);
    // Splits strictly inside (0,1) with at least 5 points between neighbours so the knots keep order.
    if !(0.05..=0.95).contains(&s1) || s2 < s1 + 0.05 || s3 < s2 + 0.05 || s3 > 0.95 {
        return Err(DevelopError::InvalidAdjustment);
    }
    if hi == 0.0 && li == 0.0 && da == 0.0 && sh == 0.0 {
        return Ok(None);
    }
    let xs = vec![0.0, s1 / 2.0, (s1 + s2) / 2.0, (s2 + s3) / 2.0, (s3 + 1.0) / 2.0, 1.0];
    let amounts = [0.0, sh, da, li, hi, 0.0];
    let mut ys: Vec<f64> = xs.iter().zip(amounts).map(|(x, a)| x + a * A_REGION).collect();
    cumulative_max(&mut ys);
    Pchip::new(xs, ys).map(Some)
}

// ---------------------------------------------------------------------------------------------
// White balance shift (Bradford adaptation in the working space).

/// Planckian locus chromaticity, Kim et al. (2002) cubic splines, valid 1667..25000 K.
pub fn planck_xy(kelvin: f64) -> Result<[f64; 2], DevelopError> {
    if !kelvin.is_finite() || !(1667.0..=25000.0).contains(&kelvin) {
        return Err(DevelopError::InvalidAdjustment);
    }
    let t = kelvin;
    let (t2, t3) = (t * t, t * t * t);
    let x = if t <= 4000.0 {
        -0.266_123_9e9 / t3 - 0.234_358_9e6 / t2 + 0.877_695_6e3 / t + 0.179_910
    } else {
        -3.025_846_9e9 / t3 + 2.107_037_9e6 / t2 + 0.222_634_7e3 / t + 0.240_390
    };
    let (x2, x3) = (x * x, x * x * x);
    let y = if t <= 2222.0 {
        -1.106_381_4 * x3 - 1.348_110_20 * x2 + 2.185_558_32 * x - 0.202_196_83
    } else if t <= 4000.0 {
        -0.954_947_6 * x3 - 1.374_185_93 * x2 + 2.091_370_15 * x - 0.167_488_67
    } else {
        3.081_758_0 * x3 - 5.873_386_70 * x2 + 3.751_129_97 * x - 0.370_014_83
    };
    Ok([x, y])
}

fn xy_to_uv(xy: [f64; 2]) -> [f64; 2] {
    let d = -2.0 * xy[0] + 12.0 * xy[1] + 3.0;
    [4.0 * xy[0] / d, 6.0 * xy[1] / d]
}

fn uv_to_xy(uv: [f64; 2]) -> [f64; 2] {
    let d = 2.0 * uv[0] - 8.0 * uv[1] + 4.0;
    [3.0 * uv[0] / d, 2.0 * uv[1] / d]
}

/// Mired shift of the assumed scene illuminant at a temperature slider of +-100.
pub const WB_MIRED_SPAN: f64 = 100.0;
/// uv distance of the assumed illuminant from the D50 baseline at a tint slider of +-100.
pub const WB_TINT_UV_SPAN: f64 = 0.02;
const D50_XY: [f64; 2] = [0.345_67, 0.358_50];
const D50_CCT: f64 = 5003.0;

/// Working-space matrix for an incremental temperature/tint offset. Temperature `t > 0` assumes a
/// bluer scene illuminant (so the picture warms); `tint > 0` assumes a greener illuminant (so the
/// picture gains magenta). The assumed illuminant is the D50 white displaced along the Planckian
/// locus by `-t/100 * 100` mired and along its normal by `tint/100 * 0.02` (CIE 1960 uv), then
/// Bradford-adapted to D50 and expressed in the working space. Zero offsets give `None`.
fn white_balance_matrix(wb: &WhiteBalanceShift, working_to_xyz: &Mat3) -> Result<Option<Mat3>, DevelopError> {
    let t = count(wb.temperature)? / COUNT_LIMIT;
    let tint = count(wb.tint)? / COUNT_LIMIT;
    if working_to_xyz.iter().any(|v| !v.is_finite()) {
        return Err(DevelopError::NonFinite);
    }
    if t == 0.0 && tint == 0.0 {
        return Ok(None);
    }
    let m0 = 1.0e6 / D50_CCT;
    let at = |mired: f64| planck_xy(1.0e6 / mired).map(xy_to_uv);
    let base = at(m0)?;
    let shifted = at(m0 - t * WB_MIRED_SPAN)?;
    // Unit normal of the locus at the baseline, oriented toward +v (green).
    let (a, b) = (at(m0 - 5.0)?, at(m0 + 5.0)?);
    let (tu, tv) = (b[0] - a[0], b[1] - a[1]);
    let len = tu.hypot(tv);
    if !len.is_finite() || len == 0.0 {
        return Err(DevelopError::ColorNoConvergence);
    }
    let (mut nu, mut nv) = (-tv / len, tu / len);
    if nv < 0.0 {
        nu = -nu;
        nv = -nv;
    }
    let d50_uv = xy_to_uv(D50_XY);
    let src_uv = [
        d50_uv[0] + (shifted[0] - base[0]) + tint * WB_TINT_UV_SPAN * nu,
        d50_uv[1] + (shifted[1] - base[1]) + tint * WB_TINT_UV_SPAN * nv,
    ];
    let src_white = color::xy_to_xyz(uv_to_xy(src_uv));
    let adapt = color::bradford(&src_white, &D50_XYZ)?;
    let xyz_to_working = color::invert(working_to_xyz)?;
    let matrix = color::mul(&xyz_to_working, &color::mul(&adapt, working_to_xyz));
    if matrix.iter().any(|v| !v.is_finite()) {
        return Err(DevelopError::NonFinite);
    }
    Ok(Some(matrix))
}

// ---------------------------------------------------------------------------------------------
// Colour stage: vibrance, saturation, mixer (HSV on the encoded value).

pub(crate) fn rgb_to_hsv(e: [f64; 3]) -> (f64, f64, f64) {
    let mx = e[0].max(e[1]).max(e[2]);
    let mn = e[0].min(e[1]).min(e[2]);
    let d = mx - mn;
    if d <= 0.0 || mx <= 0.0 {
        return (0.0, 0.0, mx);
    }
    let h = if mx == e[0] {
        ((e[1] - e[2]) / d).rem_euclid(6.0)
    } else if mx == e[1] {
        (e[2] - e[0]) / d + 2.0
    } else {
        (e[0] - e[1]) / d + 4.0
    };
    (h * 60.0, d / mx, mx)
}

pub(crate) fn hsv_to_rgb(h: f64, s: f64, v: f64) -> [f64; 3] {
    let h6 = h.rem_euclid(360.0) / 60.0;
    let sector = h6.floor();
    let f = h6 - sector;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match sector as u32 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

pub(crate) fn smoothstep(t: f64) -> f64 {
    let c = t.clamp(0.0, 1.0);
    (c * c) * (3.0 - 2.0 * c)
}

/// Band weights for a hue: the two neighbouring centres share the hue by a smoothstep, so the
/// weights always sum to 1 and a hue at a centre belongs to that band alone.
pub(crate) fn band_weights(hue: f64) -> [f64; 8] {
    let h = hue.rem_euclid(360.0);
    let mut w = [0.0; 8];
    let mut i = 7;
    for (k, centre) in BAND_CENTRES.iter().enumerate() {
        if *centre <= h {
            i = k;
        }
    }
    let lo = BAND_CENTRES[i];
    let (j, hi) = if i == 7 { (0, 360.0) } else { (i + 1, BAND_CENTRES[i + 1]) };
    let t = smoothstep((h - lo) / (hi - lo));
    w[i] = 1.0 - t;
    w[j] += t;
    w
}

/// Skin-hue protection of vibrance: weight 0.5 at 25 degrees (orange), 1 beyond +-25 degrees.
fn skin_weight(hue: f64) -> f64 {
    let d = (hue - 25.0).abs().min(25.0);
    1.0 - 0.5 * (1.0 - d / 25.0)
}

struct ColourStage {
    vibrance: f64,
    saturation: f64,
    mixer: Option<[HslBand; 8]>,
    /// Second and third luma weights divided by the weight sum; the luma of `e` is then
    /// `e0 + k1 (e1 - e0) + k2 (e2 - e0)`, which is exactly `e0` for a grey (greys are fixed points).
    luma_diff: [f64; 2],
    extras: crate::colour::ColourExtras,
}

impl ColourStage {
    fn apply(&self, v: [f64; 3]) -> [f64; 3] {
        if v.iter().any(|c| *c < 0.0) {
            return v;
        }
        let mut e = [enc(v[0]), enc(v[1]), enc(v[2])];
        let (h, s, _) = rgb_to_hsv(e);
        let yl = (e[0] + self.luma_diff[0] * (e[1] - e[0])) + self.luma_diff[1] * (e[2] - e[0]);
        let factor = (1.0 + self.vibrance * (1.0 - s) * skin_weight(h)) * (1.0 + self.saturation);
        if factor != 1.0 {
            for c in &mut e {
                *c = yl + (*c - yl) * factor;
            }
        }
        if let Some(bands) = &self.mixer
            && e.iter().all(|c| *c >= 0.0)
        {
            let (h, s, mx2) = rgb_to_hsv(e);
            let w = band_weights(h);
            let (mut dh, mut ds, mut dl) = (0.0, 0.0, 0.0);
            for (wi, b) in w.iter().zip(bands) {
                dh += wi * b.hue;
                ds += wi * b.saturation;
                dl += wi * b.luminance;
            }
            let s2 = (s * (1.0 + ds)).clamp(0.0, 1.0);
            let v2 = mx2 * (1.0 + 0.5 * dl * s2);
            e = hsv_to_rgb(h + dh * BAND_HUE_DEGREES, s2, v2);
        }
        if self.extras.is_active() {
            self.extras.apply(&mut e);
        }
        [dec(e[0]), dec(e[1]), dec(e[2])]
    }
}

// ---------------------------------------------------------------------------------------------

/// Validated, derived adjustment numbers used per pixel.
pub(crate) struct PreparedAdjust {
    wb: Option<Mat3>,
    tone: Option<Pchip>,
    regions: Option<Pchip>,
    colour: Option<ColourStage>,
    /// Clarity gain (slider / 100); 0 when off or its group (`enable_detail`) is disabled.
    pub(crate) clarity: f64,
    pub(crate) luma: [f64; 3],
    pub(crate) local: crate::local::PreparedLocal,
    pub(crate) detail: crate::detail::PreparedDetail,
    pub(crate) optics: crate::optics::PreparedOptics,
    pub(crate) geometry: Option<crate::geometry::PreparedGeometry>,
    pub(crate) effects: crate::effects::PreparedEffects,
    calibration: Option<crate::effects::PreparedCalibration>,
}

impl PreparedAdjust {
    /// Validates every parameter of every family (even a disabled one) and derives the active parts.
    pub(crate) fn prepare(
        adjust: &Adjustments,
        gates: Gates,
    ) -> Result<Self, DevelopError> {
        let Gates { colour: colour_on, curve: curve_on, detail: detail_on, mask: mask_on, grayscale: gray_on, lens: lens_on, transform: transform_on, effects: effects_on, calibration: calibration_on } = gates;
        if adjust.version < ADJUST_MIN_VERSION || adjust.version > ADJUST_VERSION {
            return Err(DevelopError::InvalidAdjustment);
        }
        if let Some(a) = &adjust.absolute_white_balance {
            white_xy(a.kelvin, a.tint)?;
        }
        let luma = adjust.luma;
        if luma.iter().any(|v| !v.is_finite() || *v < 0.0) || ((luma[0] + luma[1]) + luma[2] - 1.0).abs() > 1.0e-3 {
            return Err(DevelopError::InvalidAdjustment);
        }
        let wb = white_balance_matrix(&adjust.white_balance, &adjust.working_to_xyz_d50)?;
        let tone = basic_tone_curve(&adjust.tone)?;
        let regions = parametric_curve(&adjust.parametric)?;
        let vibrance = count(adjust.presence.vibrance)? / COUNT_LIMIT;
        let saturation = count(adjust.presence.saturation)? / COUNT_LIMIT;
        let clarity = count(adjust.presence.clarity)? / COUNT_LIMIT;
        let mut bands = [HslBand::default(); 8];
        let mut mixer_active = false;
        for (slot, band) in bands.iter_mut().zip(&adjust.mixer.bands) {
            *slot = HslBand {
                hue: count(band.hue)? / COUNT_LIMIT,
                saturation: count(band.saturation)? / COUNT_LIMIT,
                luminance: count(band.luminance)? / COUNT_LIMIT,
            };
            mixer_active |= *slot != HslBand::default();
        }
        let local = if mask_on {
            crate::local::PreparedLocal::prepare(adjust, &adjust.local)?
        } else {
            adjust.local.iter().try_for_each(crate::local::Correction::validate)?;
            crate::local::PreparedLocal::default()
        };
        let extras = crate::colour::ColourExtras::prepare(
            &adjust.point_colours,
            &adjust.grayscale,
            &adjust.grading,
            luma,
            colour_on,
            gray_on,
        )?;
        let detail = crate::detail::PreparedDetail::prepare(&adjust.detail, detail_on)?;
        let optics = crate::optics::PreparedOptics::prepare(&adjust.optics, lens_on)?;
        let geometry = crate::geometry::PreparedGeometry::prepare(&adjust.geometry, transform_on)?;
        let effects = crate::effects::PreparedEffects::prepare(&adjust.effects, effects_on)?;
        let calibration = crate::effects::PreparedCalibration::prepare(&adjust.calibration, &adjust.working_to_xyz_d50, luma, calibration_on)?;
        let colour = (extras.is_active() || (colour_on && (vibrance != 0.0 || saturation != 0.0 || mixer_active))).then_some(ColourStage {
            vibrance: if colour_on { vibrance } else { 0.0 },
            saturation: if colour_on { saturation } else { 0.0 },
            mixer: (colour_on && mixer_active).then_some(bands),
            luma_diff: [luma[1] / (luma[0] + luma[1] + luma[2]), luma[2] / (luma[0] + luma[1] + luma[2])],
            extras,
        });
        Ok(Self {
            wb,
            tone,
            regions: if curve_on { regions } else { None },
            colour,
            clarity: if detail_on { clarity } else { 0.0 },
            luma,
            local,
            detail,
            optics,
            geometry,
            effects,
            calibration,
        })
    }

    /// True when the per-pixel stages (everything but clarity and local corrections) do anything.
    pub(crate) fn pixel_active(&self) -> bool {
        self.wb.is_some() || self.tone.is_some() || self.regions.is_some() || self.colour.is_some() || self.calibration.is_some()
    }

    /// White balance shift, basic tone, parametric regions (before the point curves).
    pub(crate) fn pre(&self, v: [f64; 3]) -> Result<[f64; 3], DevelopError> {
        let mut v = match &self.calibration {
            Some(c) => c.apply(v),
            None => v,
        };
        if let Some(m) = &self.wb {
            v = color::mul_vec(m, &v);
        }
        for curve in [&self.tone, &self.regions].into_iter().flatten() {
            for c in &mut v {
                *c = dec(curve.eval(enc(*c)));
            }
        }
        if v.iter().any(|c| !c.is_finite()) {
            return Err(DevelopError::StageOverflow);
        }
        Ok(v)
    }

    /// Vibrance, saturation and the colour mixer (after the point curves).
    pub(crate) fn colour(&self, v: [f64; 3]) -> Result<[f64; 3], DevelopError> {
        let out = match &self.colour {
            Some(stage) => stage.apply(v),
            None => v,
        };
        if out.iter().any(|c| !c.is_finite()) {
            return Err(DevelopError::StageOverflow);
        }
        Ok(out)
    }
}

/// Scalar reference response of every per-pixel family at once (all groups treated as enabled:
/// white balance shift, basic tone, parametric regions, then vibrance/saturation/mixer) on one
/// scene-linear working RGB value. This is the function the develop stage C runs per pixel (minus
/// the point curves between the two halves), exposed for curve previews, tools and tests. Local
/// corrections need a whole frame and are not part of it.
pub fn apply_pixel(adjust: &Adjustments, rgb: [f64; 3]) -> Result<[f64; 3], DevelopError> {
    if rgb.iter().any(|v| !v.is_finite()) {
        return Err(DevelopError::NonFinite);
    }
    let prepared = PreparedAdjust::prepare(adjust, Gates::ALL_PIXEL)?;
    prepared.colour(prepared.pre(rgb)?)
}

// ---------------------------------------------------------------------------------------------
// Clarity: whole-frame local contrast.

/// Blur radius of the clarity base layer as a fraction of the short image side (3 box passes).
pub const CLARITY_RADIUS_FRACTION: f64 = 0.02;
/// Detail gain at clarity +-100 (encoded units per unit detail).
pub const CLARITY_GAIN: f64 = 0.8;

/// One 1-D running-sum box blur of `data` (length n, stride 1) with edge replication.
fn box_blur_line(src: &[f32], dst: &mut [f32], radius: usize) {
    let n = src.len();
    let window = (2 * radius + 1) as f64;
    let at = |i: isize| -> f64 { f64::from(src[i.clamp(0, n as isize - 1) as usize]) };
    let r = radius as isize;
    let mut sum = 0.0f64;
    for k in -r..=r {
        sum += at(k);
    }
    for (i, slot) in dst.iter_mut().enumerate() {
        *slot = (sum / window) as f32;
        let i = i as isize;
        sum += at(i + r + 1) - at(i - r);
    }
}

pub(crate) fn box_blur_3(plane: &mut [f32], width: usize, height: usize, radius: usize) {
    let mut tmp = vec![0.0f32; plane.len()];
    for _ in 0..3 {
        for (src, dst) in plane.chunks_exact(width).zip(tmp.chunks_exact_mut(width)) {
            box_blur_line(src, dst, radius);
        }
        // Vertical pass: running sum per column, row by row.
        let window = (2 * radius + 1) as f64;
        let r = radius as isize;
        let mut sums = vec![0.0f64; width];
        for k in -r..=r {
            let row = k.clamp(0, height as isize - 1) as usize;
            for (s, v) in sums.iter_mut().zip(&tmp[row * width..(row + 1) * width]) {
                *s += f64::from(*v);
            }
        }
        for y in 0..height as isize {
            let out_row = &mut plane[y as usize * width..(y as usize + 1) * width];
            for (o, s) in out_row.iter_mut().zip(&sums) {
                *o = (s / window) as f32;
            }
            let add = (y + r + 1).clamp(0, height as isize - 1) as usize;
            let sub = (y - r).clamp(0, height as isize - 1) as usize;
            for x in 0..width {
                sums[x] += f64::from(tmp[add * width + x]) - f64::from(tmp[sub * width + x]);
            }
        }
    }
}

/// Encoded luma plane of a linear RGB image.
pub(crate) fn encoded_luma(rgb: &[f32], luma: &[f64; 3]) -> Vec<f32> {
    rgb.chunks_exact(3)
        .map(|p| {
            let y = (luma[0] * f64::from(p[0]) + luma[1] * f64::from(p[1])) + luma[2] * f64::from(p[2]);
            enc(y) as f32
        })
        .collect()
}

/// Blurred encoded luma used as the clarity / local-clarity base layer.
pub(crate) fn clarity_base(plane: &[f32], width: usize, height: usize) -> Vec<f32> {
    let short = width.min(height) as f64;
    let radius = ((short * CLARITY_RADIUS_FRACTION).round() as usize).max(1);
    let mut base = plane.to_vec();
    box_blur_3(&mut base, width, height, radius);
    base
}

/// Encoded-domain offset added to every channel of a pixel for a clarity gain `g` (slider/100):
/// `g * 0.8 * (luma - base) * 4 b (1 - b)` with `b` the clamped base layer (midtone weighting).
#[inline]
pub(crate) fn clarity_offset(gain: f64, luma_encoded: f32, base: f32) -> f64 {
    let b = f64::from(base).clamp(0.0, 1.0);
    let weight = (4.0 * b) * (1.0 - b);
    ((gain * CLARITY_GAIN) * (f64::from(luma_encoded) - f64::from(base))) * weight
}

/// Whole-frame clarity (local contrast) over a linear RGB image of `width * height` pixels.
/// Deterministic and serial; the image is only mutated when the whole pass succeeded.
pub fn clarity(rgb: &mut [f32], width: u32, height: u32, amount: f64, luma: [f64; 3]) -> Result<(), DevelopError> {
    let gain = count(amount)? / COUNT_LIMIT;
    let (w, h) = (width as usize, height as usize);
    let pixels = w.checked_mul(h).ok_or(DevelopError::Overflow)?;
    if w == 0 || h == 0 || pixels.checked_mul(3) != Some(rgb.len()) {
        return Err(DevelopError::InvalidDimensions);
    }
    if luma.iter().any(|v| !v.is_finite() || *v < 0.0) {
        return Err(DevelopError::InvalidAdjustment);
    }
    if rgb.iter().any(|v| !v.is_finite()) {
        return Err(DevelopError::NonFinite);
    }
    if gain == 0.0 {
        return Ok(());
    }
    let plane = encoded_luma(rgb, &luma);
    let base = clarity_base(&plane, w, h);
    let mut out = rgb.to_vec();
    for ((px, l), b) in out.chunks_exact_mut(3).zip(&plane).zip(&base) {
        let offset = clarity_offset(gain, *l, *b);
        if offset == 0.0 {
            continue;
        }
        for c in px {
            let v = dec(enc(f64::from(*c)) + offset) as f32;
            if !v.is_finite() {
                return Err(DevelopError::StageOverflow);
            }
            *c = v;
        }
    }
    rgb.copy_from_slice(&out);
    Ok(())
}
