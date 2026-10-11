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
use crate::blend_math::{BlendError, Exactness, Rgba, StudioBlendMode, blend_rgb};
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
    pub const ALL: [Self; 8] = [
        Self::Levels,
        Self::Curves,
        Self::HueSaturation,
        Self::BrightnessContrast,
        Self::Exposure,
        Self::Invert,
        Self::Threshold,
        Self::Posterize,
    ];
    pub const fn facts(self) -> AdjustmentFacts {
        use Exactness::{Approximate, Exact};
        let (key, exactness, reference) = match self {
            Self::Levels => (
                "levels",
                Approximate,
                "GIMP levels map (input stretch, clamp, ^(1/gamma), output lerp); Photoshop record order UNVERIFIED (D5 2)",
            ),
            Self::Curves => (
                "curves",
                Approximate,
                "natural cubic spline through the points, flat outside, clamped; Photoshop interpolation unpublished (D5 3)",
            ),
            Self::HueSaturation => (
                "hue_saturation",
                Approximate,
                "HSL hue rotation, saturation and lightness scaling; Photoshop maths unpublished (D5 4)",
            ),
            Self::BrightnessContrast => (
                "brightness_contrast",
                Approximate,
                "documented stand-in formulas; Photoshop modern and legacy curves unpublished (D5 5)",
            ),
            Self::Exposure => (
                "exposure",
                Approximate,
                "(x * 2^ev + offset)^(1/gamma) on the given numbers; Photoshop linearisation UNVERIFIED (D5 6)",
            ),
            Self::Invert => (
                "invert",
                Exact,
                "1 - x per channel (Adobe help: 8-bit 255 -> 0, 5 -> 250)",
            ),
            Self::Threshold => (
                "threshold",
                Approximate,
                "lum(x) >= level -> white else black; Photoshop brightness metric UNVERIFIED (D5 7)",
            ),
            Self::Posterize => (
                "posterize",
                Approximate,
                "round(x (n-1)) / (n-1) per channel (GIMP); Photoshop mapping UNVERIFIED (D5 8)",
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
        param(unit(self.output_black) && unit(self.output_white), "levels.output")?;
        param(
            self.gamma.is_finite() && (0.1..=9.99).contains(&self.gamma),
            "levels.gamma",
        )
    }
    /// GIMP `gimp_operation_levels_map`: `v = clamp((x - ib) / (iw - ib))`, `v = v^(1/gamma)`,
    /// `out = ob + v (ow - ob)`, clamped.
    pub fn map_value(&self, x: f32) -> f32 {
        let mut v = ((x - self.input_black) / (self.input_white - self.input_black)).clamp(0.0, 1.0);
        if self.gamma != 1.0 && v > 0.0 {
            v = v.powf(1.0 / self.gamma);
        }
        (self.output_black + v * (self.output_white - self.output_black)).clamp(0.0, 1.0)
    }
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

/// Hue/Saturation, master settings only (STU-RAS-151 `hue_saturation`): hue in degrees
/// `[-180, 180]`, saturation and lightness in percent `[-100, 100]`. Colour bands and colorize
/// are recognised but refused in this slice.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct HueSaturation {
    pub hue: f32,
    pub saturation: f32,
    pub lightness: f32,
    pub colorize: bool,
}

/// Brightness/Contrast (STU-RAS-151 `brightness_contrast`): brightness `[-150, 150]`, contrast
/// `[-50, 100]` (modern) or both `[-100, 100]` (legacy), plus the shipped `use_legacy` switch.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct BrightnessContrast {
    pub brightness: f32,
    pub contrast: f32,
    pub use_legacy: bool,
}

/// Exposure (STU-RAS-151 `exposure`): stops `[-20, 20]`, offset `[-0.5, 0.5]`, gamma `[0.01, 9.99]`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Exposure {
    pub exposure: f32,
    pub offset: f32,
    pub gamma: f32,
}

/// One adjustment with its parameters, as carried by the render-plan extension envelope.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum AdjustmentOp {
    Levels(Levels),
    Curves(Curves),
    HueSaturation(HueSaturation),
    BrightnessContrast(BrightnessContrast),
    Exposure(Exposure),
    Invert,
    /// Threshold level as a fraction (`level / 255`, PSD 1..=255).
    Threshold { level: f32 },
    /// Posterize level count, 2..=255.
    Posterize { levels: u16 },
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
        }
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
                param(h.hue.is_finite() && (-180.0..=180.0).contains(&h.hue), "hue_saturation.hue")?;
                param(
                    h.saturation.is_finite() && (-100.0..=100.0).contains(&h.saturation),
                    "hue_saturation.saturation",
                )?;
                param(
                    h.lightness.is_finite() && (-100.0..=100.0).contains(&h.lightness),
                    "hue_saturation.lightness",
                )?;
                if h.colorize {
                    return Err(AdjustError::Unsupported("hue_saturation.colorize"));
                }
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
                return Err(AdjustError::Unsupported("brightness_contrast"));
            }
            Self::Exposure(e) => {
                param(
                    e.exposure.is_finite() && (-20.0..=20.0).contains(&e.exposure),
                    "exposure.exposure",
                )?;
                param(e.offset.is_finite() && (-0.5..=0.5).contains(&e.offset), "exposure.offset")?;
                param(e.gamma.is_finite() && (0.01..=9.99).contains(&e.gamma), "exposure.gamma")?;
            }
            Self::Invert => {}
            Self::Threshold { level } => {
                param(unit(*level) && *level >= 1.0 / 255.0, "threshold.level")?;
            }
            Self::Posterize { levels } => {
                param((2..=255).contains(levels), "posterize.levels")?;
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

fn rgb_to_hsl(c: [f32; 3]) -> [f64; 3] {
    let [r, g, b] = c.map(f64::from);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if max == min {
        return [0.0, 0.0, l];
    }
    let d = max - min;
    let s = if l <= 0.5 { d / (max + min) } else { d / (2.0 - max - min) };
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
fn hsl_to_rgb(hsl: [f64; 3]) -> [f32; 3] {
    let [h, s, l] = hsl;
    if s == 0.0 {
        let v = l as f32;
        return [v, v, v];
    }
    let m2 = if l <= 0.5 { l * (1.0 + s) } else { l + s - l * s };
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

impl PreparedAdjustment {
    pub fn op(&self) -> &AdjustmentOp {
        &self.op
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
                let c = self.curves.as_ref().ok_or(AdjustError::Unsupported("curves.unprepared"))?;
                let eval = |p: &Option<PreparedCurve>, x: f32| p.as_ref().map_or(x, |p| p.eval(x));
                [0, 1, 2].map(|i| eval(&c[0], eval(&c[i + 1], rgb[i])))
            }
            AdjustmentOp::HueSaturation(h) => {
                let mut hsl = rgb_to_hsl(rgb);
                hsl[0] = (hsl[0] + f64::from(h.hue) / 360.0).rem_euclid(1.0);
                let s = f64::from(h.saturation) / 100.0;
                hsl[1] = if s < 0.0 { hsl[1] * (1.0 + s) } else { hsl[1] + (1.0 - hsl[1]) * s };
                hsl[1] = hsl[1].clamp(0.0, 1.0);
                let l = f64::from(h.lightness) / 100.0;
                hsl[2] = if l < 0.0 { hsl[2] * (1.0 + l) } else { hsl[2] + (1.0 - hsl[2]) * l };
                hsl_to_rgb(hsl)
            }
            AdjustmentOp::BrightnessContrast(_) => {
                return Err(AdjustError::Unsupported("brightness_contrast"));
            }
            AdjustmentOp::Exposure(e) => {
                let gain = 2f32.powf(e.exposure);
                rgb.map(|x| {
                    let v = (x * gain + e.offset).max(0.0);
                    let v = if e.gamma != 1.0 && v > 0.0 { v.powf(1.0 / e.gamma) } else { v };
                    v.clamp(0.0, 1.0)
                })
            }
            AdjustmentOp::Invert => rgb.map(|x| 1.0 - x),
            AdjustmentOp::Threshold { level } => {
                let v = if crate::blend_math::lum(rgb) >= *level { 1.0 } else { 0.0 };
                [v, v, v]
            }
            AdjustmentOp::Posterize { levels } => {
                let n = f32::from(*levels - 1);
                rgb.map(|x| (x * n).round() / n)
            }
        };
        Ok(out.map(|v| v.clamp(0.0, 1.0)))
    }
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
