//! Single owner (CX-CAP-001) of the per-pixel maths of `StudioAdjustment` kinds (STU-RAS-151, first
//! set) and of the adjustment-layer compositing rule. Decision: work-folder
//! `diagnostics/adjustments-advanced-blending.md`. render-cpu and render-gpu (WGSL tested against
//! this module) consume it; composite reads the exactness classes; develop's monotone tone curve
//! (STU-CON-044) is a distinct capability and is not owned here.
//!
//! Scope: scalar reference maths on STRAIGHT colours in `[0, 1]`, in the plan's blend space
//! (STU-COL-163: nothing here converts profiles or transfer curves). Parameters use the
//! bit-depth-independent canonical scales of STU-RAS-152 (levels points `normalized_0_1`; curve
//! points arrive as `level / 255`). Non-finite or out-of-range inputs are a typed refusal. Every
//! kind carries an exactness class with the same meaning as `blend_math::Exactness`; Photoshop
//! does not publish its adjustment maths, so all kinds but `invert` are `Approximate` and listed
//! on the D5 oracle list.
use crate::render_plan::SampleRange;
use crate::blend_math::{BlendError, Exactness, Rgba, StudioBlendMode, blend_rgb};
use crate::adjust_colour::ColourOp;
pub use crate::adjust_colour::{
    BlackAndWhite, ChannelMixer, ColourBalance, ColourLookup, GradientMap, GradientMapStop,
    MatchColour, PhotoFilter, ReplaceColour, SelectiveColour, SelectiveColourMethod,
    ShadowsHighlights, ToneCompression, ToneCompressionMethod, Vibrance,
};
use serde::Serialize;
use std::fmt;

/// The adjustment kinds of the first set, with STU-RAS-151 keys.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum AdjustmentKind {
    Levels,
    Curves,
    HueSaturation,
    BrightnessContrast,
    Exposure,
    Invert,
    Threshold,
    Posterize,
    // Second set (lane SMB-PS-ADJUST-STYLES, maths in `adjust_colour`). Appended so the position
    // of every earlier kind in `ALL` (the GPU opcode) is unchanged.
    Vibrance,
    ColourBalance,
    PhotoFilter,
    BlackAndWhite,
    ChannelMixer,
    SelectiveColour,
    GradientMap,
    ColourLookup,
    ShadowsHighlights,
    ToneCompression,
    MatchColour,
    ReplaceColour,
}

/// Static facts of one kind: key, formula-provenance class and reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdjustmentFacts {
    pub kind: AdjustmentKind,
    pub key: &'static str,
    pub exactness: Exactness,
    pub reference: &'static str,
}

impl AdjustmentKind {
    pub const ALL: [Self; 20] = [
        Self::Levels,
        Self::Curves,
        Self::HueSaturation,
        Self::BrightnessContrast,
        Self::Exposure,
        Self::Invert,
        Self::Threshold,
        Self::Posterize,
        Self::Vibrance,
        Self::ColourBalance,
        Self::PhotoFilter,
        Self::BlackAndWhite,
        Self::ChannelMixer,
        Self::SelectiveColour,
        Self::GradientMap,
        Self::ColourLookup,
        Self::ShadowsHighlights,
        Self::ToneCompression,
        Self::MatchColour,
        Self::ReplaceColour,
    ];
    pub const fn facts(self) -> AdjustmentFacts {
        use Exactness::{Approximate, Exact};
        let (key, exactness, reference) = match self {
            Self::Levels => (
                "levels",
                Approximate,
                "GIMP app/operations/gimpoperationlevels.c levels map + psd-tools composite/adjustments.py apply_levels (channel record then composite record); Photoshop order UNVERIFIED (D5 2)",
            ),
            Self::Curves => (
                "curves",
                Approximate,
                "natural cubic spline, flat outside, clamped, channel then composite (psd-tools apply_curves scipy CubicSpline natural; Krita kis_cubic_curve); Photoshop interpolation unpublished (D5 3)",
            ),
            Self::HueSaturation => (
                "hue_saturation",
                Approximate,
                "psd-tools composite/adjustments.py _huesaturation master path (RGB lightness, HSL hue rotate, s/(1.01-S) or s(1+S)) and colorize; empirical, Photoshop maths unpublished (D5 4)",
            ),
            Self::BrightnessContrast => (
                "brightness_contrast",
                Approximate,
                "legacy only: per-channel shift + linear contrast about 0.5 (Photoshop-team statement relayed on W3C public-fx 2012; photocraft PR 2615 measurement); modern curve unpublished -> refused (D5 5)",
            ),
            Self::Exposure => (
                "exposure",
                Approximate,
                "psd-tools apply_exposure: x^g, *2^ev, +offset, clamp, ^(1/gamma), ^(1/g) with g = linearize_gamma; Photoshop linearisation UNVERIFIED (D5 6)",
            ),
            Self::Invert => (
                "invert",
                Exact,
                "1 - x per channel (Adobe help: 8-bit 255 -> 0, 5 -> 250)",
            ),
            Self::Threshold => (
                "threshold",
                Approximate,
                "lum(x) >= level -> white else black (psd-tools apply_threshold, 0.3/0.59/0.11); Photoshop metric UNVERIFIED (D5 7)",
            ),
            Self::Posterize => (
                "posterize",
                Approximate,
                "floor(x n 255/256) / (n-1) per channel (psd-tools apply_posterize, 8-bit LUT correction fitted toward Photoshop); Photoshop mapping UNVERIFIED (D5 8)",
            ),
            Self::Vibrance => (
                "vibrance",
                Approximate,
                "Studio formula: chroma about Lum scaled by 1 + v(1 - (max - min)), then by 1 + saturation (adjust_colour::vibrance); Photoshop vibrance unpublished, UNVERIFIED (D5)",
            ),
            Self::ColourBalance => (
                "colour_balance",
                Approximate,
                "GIMP gimpoperationcolorbalance.c transfer weights on HSL lightness (a 0.25, b 0.333, scale 0.7; behaviour reference, no code copied), optional HSL lightness preservation; Photoshop transfer UNVERIFIED (D5)",
            ),
            Self::PhotoFilter => (
                "photo_filter",
                Approximate,
                "multiply by the filter colour faded by density, optional W3C SetLum to the original Lum (adjust_colour::photo_filter); Photoshop lens model UNVERIFIED (D5)",
            ),
            Self::BlackAndWhite => (
                "black_and_white",
                Approximate,
                "gray = min + (max - mid) w_primary + (mid - min) w_secondary over the six weights, tint = HSL(hue, sat, gray) (adjust_colour::black_and_white); Photoshop mix UNVERIFIED (D5)",
            ),
            Self::ChannelMixer => (
                "channel_mixer",
                Approximate,
                "out_i = (w_r R + w_g G + w_b B + constant) / 100, monochrome = red row; constant as percent of full scale UNVERIFIED (D5)",
            ),
            Self::SelectiveColour => (
                "selective_colour",
                Approximate,
                "nine range memberships (Studio reading) times ink deltas (a + k) ink relative / (a + k) absolute after the Adobe help 50 % + 10 % example; Photoshop maths UNVERIFIED (D5)",
            ),
            Self::GradientMap => (
                "gradient_map",
                Approximate,
                "W3C Lum mapped through linearly interpolated stops, optional reverse; Photoshop luminance metric and smoothness UNVERIFIED (D5)",
            ),
            Self::ColourLookup => (
                "colour_lookup",
                Approximate,
                "trilinear interpolation of a resolved 3D cube (side 2..=65, red fastest); Photoshop interpolation UNVERIFIED (D5)",
            ),
            Self::ShadowsHighlights => (
                "shadows_highlights",
                Approximate,
                "Studio global form only (radius 0; radius > 0 refused): quadratic tone-width weights lift shadows / pull highlights, midtone contrast about 0.5, SetLum, chroma scaled by colour correction; not a Photoshop parity claim, UNVERIFIED (D5)",
            ),
            Self::ToneCompression => (
                "tone_compression",
                Approximate,
                "global methods only: exposure-gamma clamp(x 2^e)^(1/g); highlight compression (y/(1+y))^(1/g) (Reinhard 2002 global); local adaptation and equalize histogram refused; Photoshop HDR toning UNVERIFIED (D5)",
            ),
            Self::MatchColour => (
                "match_colour",
                Approximate,
                "per-channel statistics transfer (Reinhard 2001 in RGB) with neutralize, colour intensity, luminance and fade; Photoshop colour space and statistics UNVERIFIED (D5)",
            ),
            Self::ReplaceColour => (
                "replace_colour",
                Approximate,
                "colour-range membership clamp(1 - 255 d / fuzziness) over the samples, then the Hue/Saturation master shift blended by membership; Photoshop falloff UNVERIFIED (D5)",
            ),
        };
        AdjustmentFacts {
            kind: self,
            key,
            exactness,
            reference,
        }
    }
    pub const fn key(self) -> &'static str {
        self.facts().key
    }
}

/// Typed failure of an adjustment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdjustError {
    /// A colour value was non-finite or outside `[0, 1]`.
    OutOfDomain,
    /// A parameter is non-finite or outside its declared range (names the parameter).
    InvalidParameter(&'static str),
    /// A recognised feature without a usable formula in this slice (names the feature).
    Unsupported(&'static str),
}
impl AdjustError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::OutOfDomain => "adjust_out_of_domain",
            Self::InvalidParameter(_) => "adjust_invalid_parameter",
            Self::Unsupported(_) => "adjust_unsupported",
        }
    }
}
impl fmt::Display for AdjustError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfDomain => f.write_str(self.code()),
            Self::InvalidParameter(p) | Self::Unsupported(p) => write!(f, "{}:{p}", self.code()),
        }
    }
}
impl std::error::Error for AdjustError {}

fn unit(v: f32) -> bool {
    v.is_finite() && (0.0..=1.0).contains(&v)
}
fn param(ok: bool, name: &'static str) -> Result<(), AdjustError> {
    if ok {
        Ok(())
    } else {
        Err(AdjustError::InvalidParameter(name))
    }
}

/// One Levels record (STU-RAS-151 `levels`, all fractions except `gamma`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct LevelsRecord {
    pub input_black: f32,
    pub input_white: f32,
    pub gamma: f32,
    pub output_black: f32,
    pub output_white: f32,
}
impl LevelsRecord {
    pub const IDENTITY: Self = Self {
        input_black: 0.0,
        input_white: 1.0,
        gamma: 1.0,
        output_black: 0.0,
        output_white: 1.0,
    };
    /// Inputs in `[0, 1]` with `input_black < input_white`; outputs in `[0, 1]` (an output black
    /// above output white inverts, as the PSD record permits); gamma in `[0.1, 9.99]` (STU-RAS-151
    /// soft range; the PSD record stores gamma * 100 in 10..=999).
    pub fn validate(&self) -> Result<(), AdjustError> {
        param(
            unit(self.input_black) && unit(self.input_white) && self.input_black < self.input_white,
            "levels.input",
        )?;
        param(
            unit(self.output_black) && unit(self.output_white),
            "levels.output",
        )?;
        param(
            self.gamma.is_finite() && (0.1..=9.99).contains(&self.gamma),
            "levels.gamma",
        )
    }
    /// GIMP `gimp_operation_levels_map`: `v = clamp((x - ib) / (iw - ib))`, `v = gamma curve(v)`
    /// ([`levels_gamma`]: the power `v^(1/gamma)` with Photoshop's toe near black for gamma above
    /// 1), `out = ob + v (ow - ob)`, clamped.
    pub fn map_value(&self, x: f32) -> f32 {
        let mut v =
            ((x - self.input_black) / (self.input_white - self.input_black)).clamp(0.0, 1.0);
        if self.gamma != 1.0 && v > 0.0 {
            v = levels_gamma(v, self.gamma);
        }
        (self.output_black + v * (self.output_white - self.output_black)).clamp(0.0, 1.0)
    }
}

/// Photoshop's Levels gamma curve on the normalised input `v` in `[0, 1]` (lane
/// SMB-PS-ORACLE-PARITY-1, measured on the Photoshop 27.11 D5 oracle PS-C02 / PS-C28). Gamma at
/// most 1 (and above 2) is the power `v^(1/gamma)`. For `1 < gamma <= 2` Photoshop limits the slope
/// near black: below `x0 = s0 / 32` with `s0 = 2^gamma` the curve is the cubic Hermite from `(0, 0)`
/// with slope `s0` to `(x0, x0^(1/gamma))` with the power's slope there (C1 join). Gamma 2: `x0 =
/// 1/8`, `s0 = 4` reproduces the 16-bit ramp within 0.92 steps of the 15-bit scale (the pure power
/// differed by 2116 steps, 17 8-bit codes); gamma 1.5 within 2 8-bit codes (one sample) where the
/// pure power differed by 5. The rule for gamma above 2 (the toe would cover a larger share of the
/// range) and the exact shape between 1 and 1.4 are UNVERIFIED: the oracle has gammas 0.5, 0.8,
/// 1, 1.4, 1.5 and 2 only.
pub fn levels_gamma(v: f32, gamma: f32) -> f32 {
    if gamma == 1.0 || v <= 0.0 {
        return v;
    }
    let e = 1.0 / gamma;
    if gamma > 1.0 && gamma <= 2.0 {
        let s0 = gamma.exp2();
        let x0 = s0 / 32.0;
        if v < x0 {
            let y1 = x0.powf(e);
            let m1 = e * y1; // slope at x0 times x0: e * x0^(e - 1) * x0
            let m0 = s0 * x0;
            let t = v / x0;
            let t2 = t * t;
            let t3 = t2 * t;
            return (t3 - 2.0 * t2 + t) * m0 + (3.0 * t2 - 2.0 * t3) * y1 + (t3 - t2) * m1;
        }
    }
    v.powf(e)
}


/// Levels: a composite record plus one record per channel. Per channel the channel record is
/// applied first, then the composite record (GIMP order; Photoshop order UNVERIFIED, D5 2).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Levels {
    pub composite: LevelsRecord,
    pub red: LevelsRecord,
    pub green: LevelsRecord,
    pub blue: LevelsRecord,
}

/// Maximum control points per curve channel (the PSD curves record and ACV hold 2..=19 points).
pub const MAX_CURVE_POINTS: usize = 19;

/// Curves: one optional point list per channel, `[input, output]` fractions (`level / 255`).
/// Empty = identity for that channel. Per channel the channel curve is applied first, then the
/// composite curve (same order as [`Levels`]; Photoshop order UNVERIFIED, D5 3).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Curves {
    pub composite: Vec<[f32; 2]>,
    pub red: Vec<[f32; 2]>,
    pub green: Vec<[f32; 2]>,
    pub blue: Vec<[f32; 2]>,
}

/// A prepared natural cubic spline (second derivatives zero at both ends), evaluated in binary64.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedCurve {
    xs: Vec<f64>,
    ys: Vec<f64>,
    m: Vec<f64>,
}
impl PreparedCurve {
    /// The evaluator form `(xs, ys, m)` (knots, values, second derivatives) that
    /// [`PreparedCurve::eval`] reads, for kernels that transcribe it (lane SMB-PR-SOURCE-MONITOR,
    /// additive; the render-gpu clip grade kernel).
    pub fn spline(&self) -> (&[f64], &[f64], &[f64]) {
        (&self.xs, &self.ys, &self.m)
    }
    /// 2..=[`MAX_CURVE_POINTS`] points, values in `[0, 1]`, strictly increasing input.
    pub fn new(points: &[[f32; 2]]) -> Result<Self, AdjustError> {
        param(
            (2..=MAX_CURVE_POINTS).contains(&points.len())
                && points.iter().all(|p| unit(p[0]) && unit(p[1]))
                && points.windows(2).all(|w| w[0][0] < w[1][0]),
            "curves.points",
        )?;
        let xs: Vec<f64> = points.iter().map(|p| f64::from(p[0])).collect();
        let ys: Vec<f64> = points.iter().map(|p| f64::from(p[1])).collect();
        let n = xs.len();
        let mut m = vec![0.0f64; n];
        if n > 2 {
            // Tridiagonal system for interior second derivatives (Thomas algorithm).
            let mut c = vec![0.0f64; n];
            let mut d = vec![0.0f64; n];
            for i in 1..n - 1 {
                let h0 = xs[i] - xs[i - 1];
                let h1 = xs[i + 1] - xs[i];
                let a = h0;
                let b = 2.0 * (h0 + h1);
                let cc = h1;
                let r = 6.0 * ((ys[i + 1] - ys[i]) / h1 - (ys[i] - ys[i - 1]) / h0);
                let denom = b - a * c[i - 1];
                c[i] = cc / denom;
                d[i] = (r - a * d[i - 1]) / denom;
            }
            for i in (1..n - 1).rev() {
                m[i] = d[i] - c[i] * m[i + 1];
            }
        }
        Ok(Self { xs, ys, m })
    }
    /// Flat outside the first/last point; output clamped to `[0, 1]`.
    pub fn eval(&self, x: f32) -> f32 {
        let x = f64::from(x);
        let n = self.xs.len();
        if x <= self.xs[0] {
            return self.ys[0] as f32;
        }
        if x >= self.xs[n - 1] {
            return self.ys[n - 1] as f32;
        }
        let i = self.xs.partition_point(|xi| *xi <= x) - 1;
        let h = self.xs[i + 1] - self.xs[i];
        let a = (self.xs[i + 1] - x) / h;
        let b = (x - self.xs[i]) / h;
        let y = a * self.ys[i]
            + b * self.ys[i + 1]
            + ((a * a * a - a) * self.m[i] + (b * b * b - b) * self.m[i + 1]) * h * h / 6.0;
        y.clamp(0.0, 1.0) as f32
    }
}

/// Hue/Saturation, master settings (STU-RAS-151 `hue_saturation`): hue in degrees `[-180, 180]`,
/// saturation and lightness in percent `[-100, 100]`. With `colorize` the three values are the
/// colorization hue/saturation/lightness (saturation `[0, 100]`, PSD `hue2` colorization block).
/// Colour bands are not part of this op (refused upstream until implemented).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct HueSaturation {
    pub hue: f32,
    pub saturation: f32,
    pub lightness: f32,
    pub colorize: bool,
}

/// Brightness/Contrast (STU-RAS-151 `brightness_contrast`): brightness `[-150, 150]`, contrast
/// `[-50, 100]` (modern) or both `[-100, 100]` (legacy, levels units), plus the shipped
/// `use_legacy` switch. Only the legacy transfer is implemented; modern is a typed refusal.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct BrightnessContrast {
    pub brightness: f32,
    pub contrast: f32,
    pub use_legacy: bool,
}

/// Exposure (STU-RAS-151 `exposure`): stops `[-20, 20]`, offset `[-0.5, 0.5]`, gamma `[0.01, 9.99]`.
/// `linearize_gamma` (`[1, 3]`, IR-only, set by the builder from the plan's blend space): `1` when
/// the values are linear light; psd-tools uses `2.2` for encoded RGB documents (a power-law stand-in
/// for the document transfer curve, UNVERIFIED).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Exposure {
    pub exposure: f32,
    pub offset: f32,
    pub gamma: f32,
    pub linearize_gamma: f32,
}

/// One adjustment with its parameters, as carried by the render-plan extension envelope.
/// `#[non_exhaustive]`: further STU-RAS-151 kinds are added without breaking consumers.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum AdjustmentOp {
    Levels(Levels),
    Curves(Curves),
    HueSaturation(HueSaturation),
    BrightnessContrast(BrightnessContrast),
    Exposure(Exposure),
    Invert,
    /// Threshold level as a fraction (`level / 255`, PSD 1..=255).
    Threshold {
        level: f32,
    },
    /// Posterize level count, 2..=255.
    Posterize {
        levels: u16,
    },
    Vibrance(Vibrance),
    ColourBalance(ColourBalance),
    PhotoFilter(PhotoFilter),
    BlackAndWhite(BlackAndWhite),
    ChannelMixer(ChannelMixer),
    SelectiveColour(SelectiveColour),
    GradientMap(GradientMap),
    ColourLookup(ColourLookup),
    ShadowsHighlights(ShadowsHighlights),
    ToneCompression(ToneCompression),
    MatchColour(MatchColour),
    ReplaceColour(ReplaceColour),
}

/// An [`AdjustmentOp`] with prepared state (splines), ready for per-pixel evaluation.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedAdjustment {
    op: AdjustmentOp,
    curves: Option<[Option<PreparedCurve>; 4]>,
}

impl AdjustmentOp {
    pub const fn kind(&self) -> AdjustmentKind {
        match self {
            Self::Levels(_) => AdjustmentKind::Levels,
            Self::Curves(_) => AdjustmentKind::Curves,
            Self::HueSaturation(_) => AdjustmentKind::HueSaturation,
            Self::BrightnessContrast(_) => AdjustmentKind::BrightnessContrast,
            Self::Exposure(_) => AdjustmentKind::Exposure,
            Self::Invert => AdjustmentKind::Invert,
            Self::Threshold { .. } => AdjustmentKind::Threshold,
            Self::Posterize { .. } => AdjustmentKind::Posterize,
            Self::Vibrance(_) => AdjustmentKind::Vibrance,
            Self::ColourBalance(_) => AdjustmentKind::ColourBalance,
            Self::PhotoFilter(_) => AdjustmentKind::PhotoFilter,
            Self::BlackAndWhite(_) => AdjustmentKind::BlackAndWhite,
            Self::ChannelMixer(_) => AdjustmentKind::ChannelMixer,
            Self::SelectiveColour(_) => AdjustmentKind::SelectiveColour,
            Self::GradientMap(_) => AdjustmentKind::GradientMap,
            Self::ColourLookup(_) => AdjustmentKind::ColourLookup,
            Self::ShadowsHighlights(_) => AdjustmentKind::ShadowsHighlights,
            Self::ToneCompression(_) => AdjustmentKind::ToneCompression,
            Self::MatchColour(_) => AdjustmentKind::MatchColour,
            Self::ReplaceColour(_) => AdjustmentKind::ReplaceColour,
        }
    }

    /// The second-set view of this op (`None` for the first set).
    fn colour_op(&self) -> Option<ColourOp<'_>> {
        Some(match self {
            Self::Vibrance(p) => ColourOp::Vibrance(p),
            Self::ColourBalance(p) => ColourOp::ColourBalance(p),
            Self::PhotoFilter(p) => ColourOp::PhotoFilter(p),
            Self::BlackAndWhite(p) => ColourOp::BlackAndWhite(p),
            Self::ChannelMixer(p) => ColourOp::ChannelMixer(p),
            Self::SelectiveColour(p) => ColourOp::SelectiveColour(p),
            Self::GradientMap(p) => ColourOp::GradientMap(p),
            Self::ColourLookup(p) => ColourOp::ColourLookup(p),
            Self::ShadowsHighlights(p) => ColourOp::ShadowsHighlights(p),
            Self::ToneCompression(p) => ColourOp::ToneCompression(p),
            Self::MatchColour(p) => ColourOp::MatchColour(p),
            Self::ReplaceColour(p) => ColourOp::ReplaceColour(p),
            _ => return None,
        })
    }

    /// Validate parameters and prepare per-pixel state. Every refusal is typed.
    pub fn prepare(&self) -> Result<PreparedAdjustment, AdjustError> {
        let mut curves = None;
        match self {
            Self::Levels(l) => {
                for r in [&l.composite, &l.red, &l.green, &l.blue] {
                    r.validate()?;
                }
            }
            Self::Curves(c) => {
                let prep = |p: &Vec<[f32; 2]>| -> Result<Option<PreparedCurve>, AdjustError> {
                    if p.is_empty() {
                        Ok(None)
                    } else {
                        PreparedCurve::new(p).map(Some)
                    }
                };
                curves = Some([
                    prep(&c.composite)?,
                    prep(&c.red)?,
                    prep(&c.green)?,
                    prep(&c.blue)?,
                ]);
            }
            Self::HueSaturation(h) => {
                param(
                    h.hue.is_finite() && (-180.0..=180.0).contains(&h.hue),
                    "hue_saturation.hue",
                )?;
                let smin = if h.colorize { 0.0 } else { -100.0 };
                param(
                    h.saturation.is_finite() && (smin..=100.0).contains(&h.saturation),
                    "hue_saturation.saturation",
                )?;
                param(
                    h.lightness.is_finite() && (-100.0..=100.0).contains(&h.lightness),
                    "hue_saturation.lightness",
                )?;
            }
            Self::BrightnessContrast(b) => {
                let (bmax, cmin, cmax) = if b.use_legacy {
                    (100.0, -100.0, 100.0)
                } else {
                    (150.0, -50.0, 100.0)
                };
                param(
                    b.brightness.is_finite() && (-bmax..=bmax).contains(&b.brightness),
                    "brightness_contrast.brightness",
                )?;
                param(
                    b.contrast.is_finite() && (cmin..=cmax).contains(&b.contrast),
                    "brightness_contrast.contrast",
                )?;
                if !b.use_legacy {
                    return Err(AdjustError::Unsupported("brightness_contrast.modern"));
                }
            }
            Self::Exposure(e) => {
                param(
                    e.exposure.is_finite() && (-20.0..=20.0).contains(&e.exposure),
                    "exposure.exposure",
                )?;
                param(
                    e.offset.is_finite() && (-0.5..=0.5).contains(&e.offset),
                    "exposure.offset",
                )?;
                param(
                    e.gamma.is_finite() && (0.01..=9.99).contains(&e.gamma),
                    "exposure.gamma",
                )?;
                param(
                    e.linearize_gamma.is_finite() && (1.0..=3.0).contains(&e.linearize_gamma),
                    "exposure.linearize_gamma",
                )?;
            }
            Self::Invert => {}
            Self::Threshold { level } => {
                param(unit(*level) && *level >= 1.0 / 255.0, "threshold.level")?;
            }
            Self::Posterize { levels } => {
                param((2..=255).contains(levels), "posterize.levels")?;
            }
            other => {
                if let Some(op) = other.colour_op() {
                    crate::adjust_colour::validate(&op)?;
                }
            }
        }
        Ok(PreparedAdjustment {
            op: self.clone(),
            curves,
        })
    }

    /// Convenience: prepare and apply to one colour.
    pub fn apply(&self, rgb: [f32; 3]) -> Result<[f32; 3], AdjustError> {
        self.prepare()?.apply(rgb)
    }
}

/// RGB (`0..=1`) to HSL with hue, saturation and lightness as fractions `0..=1` (public for the
/// clip colour grade of composite, lane SMB-PREMIERE-COLOR-TITLES; one conversion owner).
pub fn rgb_to_hsl(c: [f32; 3]) -> [f64; 3] {
    let [r, g, b] = c.map(f64::from);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if max == min {
        return [0.0, 0.0, l];
    }
    let d = max - min;
    let s = if l <= 0.5 {
        d / (max + min)
    } else {
        d / (2.0 - max - min)
    };
    let mut h = if max == r {
        (g - b) / d
    } else if max == g {
        2.0 + (b - r) / d
    } else {
        4.0 + (r - g) / d
    } / 6.0;
    if h < 0.0 {
        h += 1.0;
    }
    [h, s, l]
}
/// The inverse of [`rgb_to_hsl`].
pub fn hsl_to_rgb(hsl: [f64; 3]) -> [f32; 3] {
    let [h, s, l] = hsl;
    if s == 0.0 {
        let v = l as f32;
        return [v, v, v];
    }
    let m2 = if l <= 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let m1 = 2.0 * l - m2;
    let ch = |mut t: f64| -> f32 {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        let v = if t < 1.0 / 6.0 {
            m1 + (m2 - m1) * t * 6.0
        } else if t < 0.5 {
            m2
        } else if t < 2.0 / 3.0 {
            m1 + (m2 - m1) * (2.0 / 3.0 - t) * 6.0
        } else {
            m1
        };
        v.clamp(0.0, 1.0) as f32
    };
    [ch(h + 1.0 / 3.0), ch(h), ch(h - 1.0 / 3.0)]
}

/// psd-tools `_apply_lightness`: `L > 0` lifts toward white `x (1 - L) + L`, `L < 0` scales `x (1 + L)`.
fn lightness_shift(x: f64, l: f64) -> f64 {
    let v = if l > 0.0 {
        x * (1.0 - l) + l
    } else if l < 0.0 {
        x * (1.0 + l)
    } else {
        x
    };
    v.clamp(0.0, 1.0)
}

/// Half an 8-bit code: Threshold compares the luminance rounded to 8 bits (D5 oracle PS-C07).
const THRESHOLD_HALF_CODE: f32 = 0.5 / 255.0;

/// Pivot of the legacy contrast: 127/255, measured on the D5 Photoshop oracle (PS-C05: contrast +100
/// steps at input 127, contrast -100 gives 127, brightness +100 with contrast -100 gives 227), as the
/// relayed Photoshop-team statement says (W3C public-fx 2012, msg 0060); 0.5 was used before.
const LEGACY_CONTRAST_PIVOT: f32 = 127.0 / 255.0;

/// Legacy Brightness/Contrast per channel, pivot `p` = [`LEGACY_CONTRAST_PIVOT`]: brightness `b`
/// (fraction) is a shift; contrast `c` (fraction): `c >= 0` -> `(x + b - p) / (1 - c) + p`
/// (brightness before the stretch; `c == 1` is a hard step, white from the pivot up), `c < 0` ->
/// `(x - p)(1 + c) + p + b` (brightness after). The 7 legacy D5 oracle variants (PS-C05) match within
/// one 8-bit code (lane SMB-D5-PS-COMPARE).
fn legacy_brightness_contrast(rgb: [f32; 3], b: f32, c: f32) -> [f32; 3] {
    let p = LEGACY_CONTRAST_PIVOT;
    rgb.map(|x| {
        let v = if c >= 1.0 {
            if x + b >= p { 1.0 } else { 0.0 }
        } else if c >= 0.0 {
            (x + b - p) / (1.0 - c) + p
        } else {
            (x - p) * (1.0 + c) + p + b
        };
        v.clamp(0.0, 1.0)
    })
}

impl PreparedAdjustment {
    pub fn op(&self) -> &AdjustmentOp {
        &self.op
    }

    /// [`PreparedAdjustment::apply`] in `range`. Extended (lane SMB-PIGMENT-EXTENDED-RANGE-2) is
    /// defined for [`adjustment_extended_defined`] only: exposure on linear-light values,
    /// `v = x 2^stops + offset`, then `v^(1/gamma)` where `v > 0`, without the unit clamps (any
    /// finite input; a non-finite result is `OutOfDomain`). Every other kind is
    /// `Unsupported("sample_range_extended")`.
    pub fn apply_in(&self, range: SampleRange, rgb: [f32; 3]) -> Result<[f32; 3], AdjustError> {
        if range == SampleRange::Unit {
            return self.apply(rgb);
        }
        if !adjustment_extended_defined(&self.op) {
            return Err(AdjustError::Unsupported("sample_range_extended"));
        }
        if !rgb.iter().all(|v| v.is_finite()) {
            return Err(AdjustError::OutOfDomain);
        }
        let AdjustmentOp::Exposure(e) = &self.op else {
            return Err(AdjustError::Unsupported("sample_range_extended"));
        };
        let gain = 2f32.powf(e.exposure);
        let out = rgb.map(|x| {
            let mut v = x * gain + e.offset;
            if e.gamma != 1.0 && v > 0.0 {
                v = v.powf(1.0 / e.gamma);
            }
            v
        });
        if out.iter().all(|v| v.is_finite()) {
            Ok(out)
        } else {
            Err(AdjustError::OutOfDomain)
        }
    }

    /// The adjusted colour `f(rgb)`; alpha is not an input (adjustments act on colour only).
    pub fn apply(&self, rgb: [f32; 3]) -> Result<[f32; 3], AdjustError> {
        if !rgb.iter().all(|v| unit(*v)) {
            return Err(AdjustError::OutOfDomain);
        }
        let out = match &self.op {
            AdjustmentOp::Levels(l) => {
                let ch = [&l.red, &l.green, &l.blue];
                [0, 1, 2].map(|i| l.composite.map_value(ch[i].map_value(rgb[i])))
            }
            AdjustmentOp::Curves(_) => {
                let c = self
                    .curves
                    .as_ref()
                    .ok_or(AdjustError::Unsupported("curves.unprepared"))?;
                let eval = |p: &Option<PreparedCurve>, x: f32| p.as_ref().map_or(x, |p| p.eval(x));
                [0, 1, 2].map(|i| eval(&c[0], eval(&c[i + 1], rgb[i])))
            }
            AdjustmentOp::HueSaturation(h) => {
                let hue = (f64::from(h.hue) / 360.0).rem_euclid(1.0);
                let sat = f64::from(h.saturation) / 100.0;
                let light = f64::from(h.lightness) / 100.0;
                if h.colorize {
                    // psd-tools `_huesaturation_colorize`: hue and saturation replaced, lightness
                    // applied to the HSL lightness.
                    let hsl = rgb_to_hsl(rgb);
                    hsl_to_rgb([hue, sat, lightness_shift(hsl[2], light)])
                } else {
                    // psd-tools `_huesaturation` master path: lightness on RGB first, then HSL
                    // hue rotation and saturation `s / (1 - S + 0.01)` (S > 0) or `s (1 + S)`.
                    let lit = rgb.map(|x| lightness_shift(f64::from(x), light) as f32);
                    let mut hsl = rgb_to_hsl(lit);
                    hsl[0] = (hsl[0] + hue).rem_euclid(1.0);
                    hsl[1] = if sat > 0.0 {
                        hsl[1] / (1.0 - sat + 0.01)
                    } else {
                        hsl[1] * (1.0 + sat)
                    }
                    .clamp(0.0, 1.0);
                    hsl_to_rgb(hsl)
                }
            }
            AdjustmentOp::BrightnessContrast(b) => {
                if !b.use_legacy {
                    return Err(AdjustError::Unsupported("brightness_contrast.modern"));
                }
                legacy_brightness_contrast(rgb, b.brightness / 255.0, b.contrast / 100.0)
            }
            AdjustmentOp::Exposure(e) => {
                let gain = 2f32.powf(e.exposure);
                let g = e.linearize_gamma;
                rgb.map(|x| {
                    let lin = if g != 1.0 { x.powf(g) } else { x };
                    let mut v = (lin * gain + e.offset).clamp(0.0, 1.0);
                    if e.gamma != 1.0 && v > 0.0 {
                        v = v.powf(1.0 / e.gamma);
                    }
                    if g != 1.0 && v > 0.0 {
                        v = v.powf(1.0 / g);
                    }
                    v.clamp(0.0, 1.0)
                })
            }
            AdjustmentOp::Invert => rgb.map(|x| 1.0 - x),
            AdjustmentOp::Threshold { level } => {
                // D5 oracle PS-C07: white where round(255 lum) >= level, i.e. lum >= level - half
                // a code (the 17^3 lattice at level 128 and the ramp at 127/128/129 reproduce with 0
                // mismatches); comparing lum >= level lost the exact-equality case in binary32.
                let v = if crate::blend_math::lum(rgb) >= *level - THRESHOLD_HALF_CODE {
                    1.0
                } else {
                    0.0
                };
                [v, v, v]
            }
            AdjustmentOp::Posterize { levels } => {
                // psd-tools `apply_posterize` with its 8-bit LUT correction 255/256: equal-width
                // floor bins, the top bin maps to 1.
                let n = f32::from(*levels);
                rgb.map(|x| ((x * n * (255.0 / 256.0)).floor() / (n - 1.0)).min(1.0))
            }
            other => match other.colour_op() {
                Some(op) => crate::adjust_colour::apply(&op, rgb)?,
                None => return Err(AdjustError::Unsupported("adjustment_kind")),
            },
        };
        Ok(out.map(|v| v.clamp(0.0, 1.0)))
    }
}

/// Is `op` defined on extended-range (scene-referred) values? Only exposure on linear-light values
/// (`linearize_gamma == 1`): Adobe documents the Exposure adjustment as made for HDR / 32-bit
/// images and computed in a linear (gamma 1.0) space, and its formula has no unit white point.
/// Levels, curves, hue/saturation, brightness/contrast, invert (`1 - x`), threshold and posterize
/// are defined on [0, 1] (their ranges, tables, HSL model or `1 - x` refer to the white point) and
/// are undefined above 1 (STU-COL-120, STU-FX-006a); exposure on encoded values needs the
/// `x^2.2` power stand-in, undefined for negative values.
pub fn adjustment_extended_defined(op: &AdjustmentOp) -> bool {
    matches!(op, AdjustmentOp::Exposure(e) if e.linearize_gamma == 1.0)
}

/// Exactness of the adjustment-layer compositing rule itself (alpha preserved; see
/// [`composite_adjustment`]). Studio rule, Photoshop behaviour on a semi-transparent backdrop
/// UNVERIFIED (D5 1).
pub const ADJUSTMENT_LAYER_EXACTNESS: Exactness = Exactness::Approximate;

/// Failure of the adjustment-layer rule: the adjustment or the blend mode refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdjustLayerError {
    Adjust(AdjustError),
    Blend(BlendError),
}
impl From<AdjustError> for AdjustLayerError {
    fn from(e: AdjustError) -> Self {
        Self::Adjust(e)
    }
}
impl From<BlendError> for AdjustLayerError {
    fn from(e: BlendError) -> Self {
        Self::Blend(e)
    }
}

/// Adjustment layer over one backdrop pixel: `C = lerp(Cb, B_mode(Cb, f(Cb)), k)`, alpha unchanged.
/// `coverage` = opacity x fill opacity x mask x Blend-If, in `[0, 1]`. A transparent backdrop stays
/// transparent (nothing to adjust). For an opaque backdrop this equals W3C source-over of the
/// adjusted colour at alpha `k`.
pub fn composite_adjustment(
    mode: StudioBlendMode,
    backdrop: Rgba,
    adjustment: &PreparedAdjustment,
    coverage: f32,
) -> Result<Rgba, AdjustLayerError> {
    composite_adjustment_in(SampleRange::Unit, mode, backdrop, adjustment, coverage)
}

/// [`composite_adjustment`] in `range`: extended uses [`PreparedAdjustment::apply_in`] and
/// `blend_rgb_extended`, and keeps `lerp(Cb, B, k)` unclamped (a non-finite result is
/// `OutOfDomain`).
pub fn composite_adjustment_in(
    range: SampleRange,
    mode: StudioBlendMode,
    backdrop: Rgba,
    adjustment: &PreparedAdjustment,
    coverage: f32,
) -> Result<Rgba, AdjustLayerError> {
    if range == SampleRange::Extended {
        if !unit(coverage) || !unit(backdrop.a) {
            return Err(AdjustError::OutOfDomain.into());
        }
        let adjusted = adjustment.apply_in(range, backdrop.rgb)?;
        let blended = crate::blend_math::blend_rgb_extended(mode, backdrop.rgb, adjusted)?;
        if backdrop.a <= 0.0 {
            return Ok(Rgba::TRANSPARENT);
        }
        let mut rgb = [0.0f32; 3];
        for (i, out) in rgb.iter_mut().enumerate() {
            *out = backdrop.rgb[i] + (blended[i] - backdrop.rgb[i]) * coverage;
        }
        if !rgb.iter().all(|v| v.is_finite()) {
            return Err(AdjustError::OutOfDomain.into());
        }
        return Ok(Rgba { rgb, a: backdrop.a });
    }
    if !unit(coverage) || !unit(backdrop.a) {
        return Err(AdjustError::OutOfDomain.into());
    }
    let adjusted = adjustment.apply(backdrop.rgb)?;
    let blended = blend_rgb(mode, backdrop.rgb, adjusted)?;
    if backdrop.a <= 0.0 {
        return Ok(Rgba::TRANSPARENT);
    }
    let mut rgb = [0.0f32; 3];
    for (i, out) in rgb.iter_mut().enumerate() {
        *out = (backdrop.rgb[i] + (blended[i] - backdrop.rgb[i]) * coverage).clamp(0.0, 1.0);
    }
    Ok(Rgba { rgb, a: backdrop.a })
}
