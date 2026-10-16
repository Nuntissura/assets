//! Clip finishing at the frame tick (lane SMB-PREMIERE-COLOR-TITLES; Premiere-style clip colour):
//! a clip's Folio `StudioClipFinishingRecords` effect stack applied to the clip's evaluated scene
//! layer. Composite owns the rendering and the stage ORDER; the colour maths stay with their
//! owners: Prism (`grade`: STU-COL-222 wheels, STU-COL-227 key, STU-COL-228 vignette; `CubeLut` /
//! `parse_cube` of lane SMB-DEVELOP-LENS-PRESETS: the STU-COL-158 `.cube` resource, tetrahedral)
//! and Pigment (`adjust_math`: the curve evaluator, RGB curves, vibrance
//! and the HSL conversion). Nothing here re-implements a formula those crates own.
//!
//! [`ClipGrade::prepare`] turns a Folio `StudioColorGrade` into prepared stages (LUTs resolved by
//! content digest through the caller's [`LutSource`], curves prepared once) and
//! [`ClipGrade::apply`] evaluates one straight RGB triple at a pixel of the clip frame, in the
//! NORMATIVE STU-COL-160 order: 1 input LUT, 2 primary correction, 3 creative look LUT mixed by
//! its intensity, 5 curves (RGB, then hue/saturation curves), 6 wheels, 7 secondaries in stack
//! order, 8 vignette (stage 4 "look adjustments" and stage 9 the output transform are not part of a
//! clip grade). [`grade_layer`] wraps a scene layer's content in [`crate::SceneContent::Graded`]:
//! composite renders the content over the clip rect, runs the grade per texel (alpha unchanged,
//! fully transparent texels skipped) and places the result like a raster tile.
//!
//! Primary correction (stage 2), Studio readings of the STU-COL-220 sliders (UNVERIFIED against
//! Lumetri, D5 oracle): `exposure` multiplies by `2^exposure`; `temperature` / `tint` become the
//! STU-COL-222 `Temperature` / `Magenta` scalars at `value / 200` (log2 units, +-100 = +-0.5);
//! `contrast` becomes the STU-COL-222 `Contrast` scalar `value / 150` about pivot 0.5; highlights,
//! shadows, whites and blacks shape a 6-knot tone curve through Pigment's curve evaluator
//! (`(0,0) (0.1, 0.1 + 0.08 b) (0.25, 0.25 + 0.1 s) (0.75, 0.75 + 0.1 h) (0.9, 0.9 + 0.08 w) (1,1)`
//! with each amount `/ 100`, knots clamped to `0..=1`); `saturation` scales chroma about the luma by
//! `value / 100`; `vibrance` is Pigment's vibrance at the same percent. Look intensity mixes
//! `c + (look(c) - c) * intensity / 100` (above 100 extrapolates). Cross-channel curves: the x axis
//! is the HSL hue, luminance or saturation; `hue_vs_hue` rotates the hue by `(y - 0.5) * 360`
//! degrees; the others multiply saturation or lightness by `2 y` (0.5 = unchanged). A secondary
//! applies its own correction (STU-COL-222 maths at its temperature / tint `/ 200` and contrast
//! `/ 150`, then its saturation) mixed by the key weight. Every stage clamps to `0..=1` (the
//! adjustment domain of the plan's blend space; values are never profile-converted, STU-COL-161).
//!
//! Lane SMB-PR-SOURCE-MONITOR: faded film (STU-COL-221 `faded_film`) runs in stage 3 after the look
//! ([`ClipGrade::faded_film`]); `sharpen` (STU-COL-220) is the spatial stage after the per-texel grade
//! ([`ClipGrade::sharpen_filter`], a [`crate::SceneContent::Filtered`] wrap); keyframed grade scalars
//! are sampled at the clip tick by `evaluate_effect_stack` before the grade is prepared.
#![deny(clippy::arithmetic_side_effects)]

use crate::composition::{SceneContent, SceneLayer};
use crate::temporal::TemporalError;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::adjust_colour::Vibrance;
use hsk_studio_pigment::adjust_math::{AdjustmentOp, Curves, PreparedAdjustment, PreparedCurve, hsl_to_rgb, rgb_to_hsl};
use hsk_studio_prism::grade::{
    GradeProgram, GradeSecondary, GradeSpline, KeyRange, SecondaryKey, Vignette, Wheels, WheelBand, apply_wheels, key_weight,
    vignette_gain,
};
use hsk_studio_prism::CubeLut as Lut;
use std::sync::Arc;

/// The Folio record types this module reads (re-exported so renderer tests and hosts can build
/// grades without a direct Folio edge).
pub use hsk_studio_folio::{
    ContentDigest, StudioGradeBasic, StudioGradeKey, StudioGradeSecondary, StudioGradeVignette, StudioGradeWheels,
    StudioKeyRange, StudioSmartFilterKind, StudioWheelBand,
};
pub use hsk_studio_folio::{StudioColorGrade, StudioGradeCurves, StudioLutRef};

/// Refusal codes (`TemporalError::Source { code }`).
pub const CLIP_LUT_UNRESOLVED: &str = "clip_lut_unresolved";
pub const CLIP_LUT_DIGEST: &str = "clip_lut_digest";
pub const CLIP_GRADE_PARAMETER: &str = "clip_grade_parameter";
/// Exactness-ledger key of a clip grade (Approximate: the Lumetri mappings are Studio readings).
pub const CLIP_GRADE_OPERATOR: &str = "clip_colour_grade";

/// Caller resolution of a Folio LUT reference to a parsed Prism LUT (parsed once, with the
/// bounds of [`hsk_studio_prism::parse_cube`], lane SMB-DEVELOP-LENS-PRESETS). The LUT's `sha256` must equal the
/// reference's `sha256` digest (hex), else the grade is refused `clip_lut_digest`.
pub trait LutSource: Sync {
    fn lut(&self, lut: &StudioLutRef) -> Option<Arc<Lut>>;
}

/// No LUTs (a grade that references one is refused `clip_lut_unresolved`).
pub struct NoLuts;
impl LutSource for NoLuts {
    fn lut(&self, _: &StudioLutRef) -> Option<Arc<Lut>> {
        None
    }
}

fn source(clip_id: &str, code: &'static str) -> TemporalError {
    TemporalError::Source {
        layer_id: clip_id.to_owned(),
        code: code.to_owned(),
    }
}

#[allow(clippy::cast_possible_truncation)]
fn f(v: f64) -> f32 {
    v as f32
}

/// Prism's `.cube` evaluation (domain clamp, 1D shaper, tetrahedral 3D) of one f32 triple.
fn lut_apply(l: &Lut, c: [f32; 3]) -> [f32; 3] {
    l.apply(c.map(f64::from)).map(f)
}

fn resolve(r: &StudioLutRef, luts: &dyn LutSource, clip_id: &str) -> Result<Arc<Lut>, TemporalError> {
    let lut = luts.lut(r).ok_or_else(|| source(clip_id, CLIP_LUT_UNRESOLVED))?;
    let hex = hsk_studio_prism::hash_hex(&lut.sha256);
    if !r.content_digest.algorithm.eq_ignore_ascii_case("sha256") || !r.content_digest.digest.eq_ignore_ascii_case(&hex) {
        return Err(source(clip_id, CLIP_LUT_DIGEST));
    }
    Ok(lut)
}

fn curve(points: &[[f64; 2]], clip_id: &str) -> Result<Option<PreparedCurve>, TemporalError> {
    if points.is_empty() {
        return Ok(None);
    }
    let p: Vec<[f32; 2]> = points.iter().map(|p| [f(p[0]), f(p[1])]).collect();
    PreparedCurve::new(&p).map(Some).map_err(|_| source(clip_id, CLIP_GRADE_PARAMETER))
}

fn points(v: &[[f64; 2]]) -> Vec<[f32; 2]> {
    v.iter().map(|p| [f(p[0]), f(p[1])]).collect()
}

/// Prepared cross-channel curves (STU-COL-226).
#[derive(Clone, Debug)]
struct HslCurves {
    hue_vs_hue: Option<PreparedCurve>,
    hue_vs_saturation: Option<PreparedCurve>,
    hue_vs_luminance: Option<PreparedCurve>,
    luminance_vs_saturation: Option<PreparedCurve>,
    saturation_vs_saturation: Option<PreparedCurve>,
}
impl HslCurves {
    fn new(c: &StudioGradeCurves, clip_id: &str) -> Result<Option<Self>, TemporalError> {
        let s = Self {
            hue_vs_hue: curve(&c.hue_vs_hue, clip_id)?,
            hue_vs_saturation: curve(&c.hue_vs_saturation, clip_id)?,
            hue_vs_luminance: curve(&c.hue_vs_luminance, clip_id)?,
            luminance_vs_saturation: curve(&c.luminance_vs_saturation, clip_id)?,
            saturation_vs_saturation: curve(&c.saturation_vs_saturation, clip_id)?,
        };
        let any = s.hue_vs_hue.is_some()
            || s.hue_vs_saturation.is_some()
            || s.hue_vs_luminance.is_some()
            || s.luminance_vs_saturation.is_some()
            || s.saturation_vs_saturation.is_some();
        Ok(any.then_some(s))
    }
    fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        let [h, s, l] = rgb_to_hsl(c);
        let at = |p: &Option<PreparedCurve>, x: f64| p.as_ref().map(|p| f64::from(p.eval(f(x))));
        let mut h2 = h;
        let mut s2 = s;
        let mut l2 = l;
        if let Some(y) = at(&self.hue_vs_hue, h) {
            h2 = (h + (y - 0.5)).rem_euclid(1.0);
        }
        if let Some(y) = at(&self.hue_vs_saturation, h) {
            s2 *= 2.0 * y;
        }
        if let Some(y) = at(&self.hue_vs_luminance, h) {
            l2 *= 2.0 * y;
        }
        if let Some(y) = at(&self.luminance_vs_saturation, l) {
            s2 *= 2.0 * y;
        }
        if let Some(y) = at(&self.saturation_vs_saturation, s) {
            s2 *= 2.0 * y;
        }
        hsl_to_rgb([h2, s2.clamp(0.0, 1.0), l2.clamp(0.0, 1.0)])
    }
}

/// One prepared secondary (key plus correction).
#[derive(Clone, Debug)]
struct Secondary {
    key: SecondaryKey,
    correction: Wheels,
    saturation: f32,
}

/// A prepared clip colour grade (module docs).
#[derive(Clone, Debug)]
pub struct ClipGrade {
    input_lut: Option<Arc<Lut>>,
    exposure_gain: f32,
    primary: Wheels,
    tone: Option<PreparedCurve>,
    saturation: f32,
    vibrance: Option<PreparedAdjustment>,
    look: Option<(Arc<Lut>, f32)>,
    rgb_curves: Option<PreparedAdjustment>,
    hsl_curves: Option<HslCurves>,
    wheels: Wheels,
    secondaries: Vec<Secondary>,
    vignette: Option<Vignette>,
    luma: [f32; 3],
    /// Faded film black lift `0.1 * faded_film / 100` (lane SMB-PR-SOURCE-MONITOR).
    faded: f32,
    /// STU-COL-220 `sharpen` (lane SMB-PR-SOURCE-MONITOR; a spatial stage, see [`ClipGrade::sharpen_filter`]).
    sharpen: f32,
    /// Vibrance fraction and the RGB curve splines (composite, red, green, blue) as data for
    /// [`ClipGrade::program`] (the same splines `rgb_curves` prepares).
    vibrance_fraction: f32,
    rgb_parts: [Option<PreparedCurve>; 4],
}

fn clamp01(c: [f32; 3]) -> [f32; 3] {
    c.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 })
}

fn saturate(c: [f32; 3], k: f32, luma: [f32; 3]) -> [f32; 3] {
    let y = c[0] * luma[0] + c[1] * luma[1] + c[2] * luma[2];
    c.map(|v| y + (v - y) * k)
}

#[allow(clippy::float_cmp)]
fn nonzero(v: f64) -> bool {
    v != 0.0
}

impl ClipGrade {
    /// Prepares `g` (already validated by Folio) for clip `clip_id`; `luma` is the working space's
    /// luminance weight vector (STU-COL-222: explicit, chosen from the working space).
    pub fn prepare(g: &StudioColorGrade, luts: &dyn LutSource, luma: [f32; 3], clip_id: &str) -> Result<Self, TemporalError> {
        let bad = || source(clip_id, CLIP_GRADE_PARAMETER);
        let b = &g.basic;
        let input_lut = g.input_lut.as_ref().map(|r| resolve(r, luts, clip_id)).transpose()?;
        let look = match &g.look {
            Some(r) => Some((resolve(r, luts, clip_id)?, f(g.look_intensity / 100.0))),
            None => None,
        };
        let primary = Wheels {
            temperature: f(b.temperature / 200.0),
            magenta: f(b.tint / 200.0),
            contrast: f(b.contrast / 150.0),
            luma,
            ..Wheels::default()
        };
        primary.validate().map_err(|_| bad())?;
        let tone = if [b.highlights, b.shadows, b.whites, b.blacks].iter().any(|v| nonzero(*v)) {
            let k = |x: f64, y: f64| [f(x), f(y.clamp(0.0, 1.0))];
            let knots = [
                k(0.0, 0.0),
                k(0.1, 0.1 + 0.08 * b.blacks / 100.0),
                k(0.25, 0.25 + 0.1 * b.shadows / 100.0),
                k(0.75, 0.75 + 0.1 * b.highlights / 100.0),
                k(0.9, 0.9 + 0.08 * b.whites / 100.0),
                k(1.0, 1.0),
            ];
            Some(PreparedCurve::new(&knots).map_err(|_| bad())?)
        } else {
            None
        };
        let vibrance = if nonzero(b.vibrance) {
            Some(
                AdjustmentOp::Vibrance(Vibrance { vibrance: f(b.vibrance), saturation: 0.0 })
                    .prepare()
                    .map_err(|_| bad())?,
            )
        } else {
            None
        };
        let c = &g.curves;
        let rgb_curves = if c.composite.is_empty() && c.red.is_empty() && c.green.is_empty() && c.blue.is_empty() {
            None
        } else {
            Some(
                AdjustmentOp::Curves(Curves {
                    composite: points(&c.composite),
                    red: points(&c.red),
                    green: points(&c.green),
                    blue: points(&c.blue),
                })
                .prepare()
                .map_err(|_| bad())?,
            )
        };
        let w = &g.wheels;
        let band = |b: &hsk_studio_folio::StudioWheelBand| WheelBand {
            offset: b.offset.map(f),
            gamma: b.gamma.map(f),
            gain: b.gain.map(f),
        };
        let wheels = Wheels {
            overall: band(&w.overall),
            shadows: band(&w.shadows),
            midtones: band(&w.midtones),
            highlights: band(&w.highlights),
            contrast: f(w.contrast),
            pivot: f(w.pivot),
            input_saturation: f(w.input_saturation),
            post_saturation: [f(w.final_saturation); 3],
            low_mid: f(w.low_mid_split),
            mid_high: f(w.mid_high_split),
            luma,
            ..Wheels::default()
        };
        wheels.validate().map_err(|_| bad())?;
        let mut secondaries = Vec::with_capacity(g.secondaries.len());
        for s in &g.secondaries {
            let range = |r: &hsk_studio_folio::StudioKeyRange| KeyRange { lo: f(r.lo), hi: f(r.hi), soft: f(r.soft) };
            let key = SecondaryKey {
                hue: range(&s.key.hue),
                saturation: range(&s.key.saturation),
                luminance: range(&s.key.luminance),
                invert: s.key.invert,
            };
            key.validate().map_err(|_| bad())?;
            let correction = Wheels {
                temperature: f(s.temperature / 200.0),
                magenta: f(s.tint / 200.0),
                contrast: f(s.contrast / 150.0),
                luma,
                ..Wheels::default()
            };
            correction.validate().map_err(|_| bad())?;
            secondaries.push(Secondary { key, correction, saturation: f(s.saturation / 100.0) });
        }
        let vignette = match &g.vignette {
            Some(v) => {
                let v = Vignette { amount: f(v.amount), midpoint: f(v.midpoint), roundness: f(v.roundness), feather: f(v.feather) };
                v.validate().map_err(|_| bad())?;
                Some(v)
            }
            None => None,
        };
        Ok(Self {
            input_lut,
            exposure_gain: f(b.exposure.exp2()),
            primary,
            tone,
            saturation: f(b.saturation / 100.0),
            vibrance,
            look,
            rgb_curves,
            hsl_curves: HslCurves::new(c, clip_id)?,
            wheels,
            secondaries,
            vignette,
            luma,
            faded: f(0.1 * g.faded_film / 100.0),
            sharpen: f(b.sharpen),
            vibrance_fraction: f(b.vibrance / 100.0),
            rgb_parts: [curve(&c.composite, clip_id)?, curve(&c.red, clip_id)?, curve(&c.green, clip_id)?, curve(&c.blue, clip_id)?],
        })
    }

    /// This grade as a Prism [`GradeProgram`] for a backend grade kernel (lane
    /// SMB-PR-SOURCE-MONITOR): the same prepared stages, identity stages absent, so a kernel runs
    /// exactly the stages [`ClipGrade::apply`] runs.
    pub fn program(&self) -> GradeProgram {
        let spline = |c: &Option<PreparedCurve>| {
            c.as_ref().map(|c| {
                let (xs, ys, m) = c.spline();
                GradeSpline { xs: xs.to_vec(), ys: ys.to_vec(), m: m.to_vec() }
            })
        };
        let any_rgb = self.rgb_curves.is_some();
        let hsl = |pick: fn(&HslCurves) -> &Option<PreparedCurve>| self.hsl_curves.as_ref().and_then(|h| spline(pick(h)));
        GradeProgram {
            input_lut: self.input_lut.clone(),
            exposure_gain: self.exposure_gain,
            primary: (!self.primary.is_identity()).then_some(self.primary),
            tone: spline(&self.tone),
            saturation: self.saturation,
            vibrance: self.vibrance.as_ref().map(|_| self.vibrance_fraction),
            look: self.look.clone(),
            faded_floor: self.faded,
            rgb_curves: if any_rgb { [spline(&self.rgb_parts[0]), spline(&self.rgb_parts[1]), spline(&self.rgb_parts[2]), spline(&self.rgb_parts[3])] } else { [None, None, None, None] },
            hsl_curves: [
                hsl(|h| &h.hue_vs_hue),
                hsl(|h| &h.hue_vs_saturation),
                hsl(|h| &h.hue_vs_luminance),
                hsl(|h| &h.luminance_vs_saturation),
                hsl(|h| &h.saturation_vs_saturation),
            ],
            wheels: (!self.wheels.is_identity()).then_some(self.wheels),
            secondaries: self
                .secondaries
                .iter()
                .map(|s| GradeSecondary { key: s.key, correction: s.correction, saturation: s.saturation })
                .collect(),
            vignette: self.vignette,
            luma: self.luma,
        }
    }

    /// The faded-film lift of one channel (Studio reading, UNVERIFIED against Lumetri, D5): the
    /// black point rises to `floor` with a quadratic toe, `v + floor * (1 - v)^2`; white stays 1.
    pub fn faded_film(floor: f32, v: f32) -> f32 {
        let d = 1.0 - v;
        v + floor * d * d
    }

    /// The faded-film floor of this grade (0 = none).
    pub fn faded_floor(&self) -> f32 {
        self.faded
    }

    /// STU-COL-220 `sharpen` of this grade.
    pub fn sharpen(&self) -> f32 {
        self.sharpen
    }

    /// The spatial sharpen stage that follows the per-texel grade (Studio reading, UNVERIFIED
    /// against Lumetri, D5): `sharpen > 0` is Pigment's unsharp mask at `amount = sharpen` percent,
    /// radius 1 px, threshold 0; `sharpen < 0` softens with Pigment's Gaussian blur of radius
    /// `-sharpen / 100` px; 0 is no stage.
    pub fn sharpen_filter(&self, clip_id: &str) -> Result<Option<hsk_studio_pigment::smart_filter::SmartFilterOp>, TemporalError> {
        let s = self.sharpen;
        if s > 0.0 {
            filters(clip_id, StudioSmartFilterKind::UnsharpMask { amount_pct: s, radius_px: 1.0, threshold_levels: 0 }).map(Some)
        } else if s < 0.0 {
            filters(clip_id, StudioSmartFilterKind::GaussianBlur { radius_px: -s / 100.0 }).map(Some)
        } else {
            Ok(None)
        }
    }

    /// The graded colour of straight RGB `c` at pixel centre `(x, y)` of a `width` x `height` clip
    /// frame (the vignette's coordinates).
    pub fn apply(&self, c: [f32; 3], x: f64, y: f64, width: f64, height: f64) -> [f32; 3] {
        let mut c = clamp01(c);
        if let Some(l) = &self.input_lut {
            c = clamp01(lut_apply(l, c));
        }
        // Stage 2 primary.
        if (self.exposure_gain - 1.0).abs() > 0.0 {
            c = clamp01(c.map(|v| v * self.exposure_gain));
        }
        if !self.primary.is_identity() {
            c = clamp01(apply_wheels(&self.primary, c));
        }
        if let Some(t) = &self.tone {
            c = c.map(|v| t.eval(v));
        }
        if (self.saturation - 1.0).abs() > 0.0 {
            c = clamp01(saturate(c, self.saturation, self.luma));
        }
        if let Some(v) = &self.vibrance {
            c = v.apply(c).map(clamp01).unwrap_or(c);
        }
        // Stage 3 look.
        if let Some((l, k)) = &self.look {
            let o = lut_apply(l, c);
            c = clamp01([0usize, 1, 2].map(|i| c[i] + (o[i] - c[i]) * k));
        }
        if self.faded > 0.0 {
            c = clamp01(c.map(|v| Self::faded_film(self.faded, v)));
        }
        // Stage 5 curves.
        if let Some(rc) = &self.rgb_curves {
            c = rc.apply(c).map(clamp01).unwrap_or(c);
        }
        if let Some(hc) = &self.hsl_curves {
            c = clamp01(hc.apply(c));
        }
        // Stage 6 wheels.
        if !self.wheels.is_identity() {
            c = clamp01(apply_wheels(&self.wheels, c));
        }
        // Stage 7 secondaries.
        for s in &self.secondaries {
            let [h, sat, l] = rgb_to_hsl(c);
            let w = key_weight(&s.key, [f(h * 360.0), f(sat), f(l)]);
            if w <= 0.0 {
                continue;
            }
            let corrected = clamp01(saturate(apply_wheels(&s.correction, c), s.saturation, self.luma));
            c = clamp01([0usize, 1, 2].map(|i| c[i] + (corrected[i] - c[i]) * w));
        }
        // Stage 8 vignette.
        if let Some(v) = &self.vignette {
            let g = vignette_gain(v, x, y, width, height);
            c = clamp01(c.map(|v| v * g));
        }
        c
    }
}

/// A per-texel stage of [`SceneContent::Graded`].
#[derive(Clone, Debug)]
pub enum ClipStage {
    /// The colour grade (alpha unchanged).
    Grade(Arc<ClipGrade>),
    /// The Studio "Crop" effect: fractions `0..=1` of the clip frame removed from each edge; a
    /// texel keeps alpha times the exact area fraction of it inside the kept rectangle (box
    /// coverage; Premiere's edge treatment is UNVERIFIED, D5).
    Crop { left: f64, top: f64, right: f64, bottom: f64 },
}
impl PartialEq for ClipStage {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Grade(a), Self::Grade(b)) => Arc::ptr_eq(a, b),
            (Self::Crop { left, top, right, bottom }, Self::Crop { left: l, top: t, right: r, bottom: b }) => {
                [left, top, right, bottom].iter().zip([l, t, r, b]).all(|(x, y)| x.to_bits() == y.to_bits())
            }
            _ => false,
        }
    }
}
/// Exactness-ledger key of a clip crop (Exact: box coverage in binary64, one rounding to f32).
pub const CLIP_CROP_OPERATOR: &str = "clip_crop";

fn overlap(a0: f64, a1: f64, b0: f64, b1: f64) -> f64 {
    (a1.min(b1) - a0.max(b0)).max(0.0)
}

impl ClipStage {
    /// The exactness key and class of the stage.
    pub fn operator(&self) -> (&'static str, hsk_studio_pigment::blend_math::Exactness) {
        use hsk_studio_pigment::blend_math::Exactness;
        match self {
            Self::Grade(_) => (CLIP_GRADE_OPERATOR, Exactness::Approximate),
            Self::Crop { .. } => (CLIP_CROP_OPERATOR, Exactness::Exact),
        }
    }
    /// The staged `(colour, alpha)` of the texel whose top-left corner is `(x, y)` in a `width` x
    /// `height` clip frame.
    pub fn texel(&self, c: [f32; 3], alpha: f32, x: f64, y: f64, width: f64, height: f64) -> ([f32; 3], f32) {
        match self {
            Self::Grade(g) => (g.apply(c, x + 0.5, y + 0.5, width, height), alpha),
            Self::Crop { left, top, right, bottom } => {
                let (x0, x1) = (left * width, (1.0 - right) * width);
                let (y0, y1) = (top * height, (1.0 - bottom) * height);
                let cover = overlap(x, x + 1.0, x0, x1) * overlap(y, y + 1.0, y0, y1);
                (c, f(f64::from(alpha) * cover.clamp(0.0, 1.0)))
            }
        }
    }
}

/// The content of [`SceneContent::Graded`]: `content` rendered over `rect` (the clip frame in the
/// content's plan coordinates), then `stage` per texel.
#[derive(Clone, Debug, PartialEq)]
pub struct GradedContent {
    pub content: SceneContent,
    pub rect: Rect,
    pub stage: ClipStage,
}

fn wrap(mut layer: SceneLayer, rect: Rect, stage: ClipStage) -> SceneLayer {
    let content = std::mem::replace(&mut layer.content, SceneContent::None);
    layer.content = SceneContent::Graded(Box::new(GradedContent { content, rect, stage }));
    layer
}

/// Wraps `layer`'s content in a grade over the clip frame `rect` (module docs).
pub fn grade_layer(layer: SceneLayer, rect: Rect, grade: Arc<ClipGrade>) -> SceneLayer {
    wrap(layer, rect, ClipStage::Grade(grade))
}

/// One clip effect evaluated at a tick (slice 2: the clip effect stack, STU-VID-020 `effect_stack`
/// / STU-FX-002). Values are on the Folio record scales.
#[derive(Clone, Debug)]
pub enum ClipEffectFrame {
    Grade(Arc<ClipGrade>),
    /// Sequence-frame pixels; scale in percent; rotation in degrees.
    Transform { anchor: [f64; 2], position: [f64; 2], scale: [f64; 2], rotation: f64 },
    /// Percent `0..=100`.
    Opacity(f64),
    /// Percent of the clip frame per edge.
    Crop { left: f64, top: f64, right: f64, bottom: f64 },
    /// Pigment live filters over the clip frame (the Studio "Gaussian Blur" effect lowers to
    /// Pigment's Gaussian blur with `radius_px = blurriness`, a Studio reading, UNVERIFIED).
    Filters(hsk_studio_pigment::smart_filter::SmartFilterOp),
}

/// Applies evaluated clip effects to `layer` in stack order (first entry first). Pixel effects
/// (grade, crop, filters) wrap the content over the clip frame `rect`; the transform and opacity
/// effects compose onto the layer matrix (after the clip's own placement, in sequence-frame
/// pixels: anchor to position, scale and rotation about the anchor) and opacity. Studio reading:
/// geometric effects act after every pixel effect regardless of their stack position (STU-CMP-010
/// order: effects, then transform, then opacity); Premiere's mid-stack Transform is UNVERIFIED.
pub fn apply_effects(mut layer: SceneLayer, rect: Rect, effects: &[ClipEffectFrame]) -> Result<SceneLayer, TemporalError> {
    let id = layer.layer_id.clone();
    let bad = |field: &'static str| TemporalError::InvalidState { layer_id: id.clone(), field };
    for e in effects {
        match e {
            ClipEffectFrame::Grade(g) => {
                layer = wrap(layer, rect, ClipStage::Grade(g.clone()));
                if let Some(op) = g.sharpen_filter(&id)? {
                    let content = std::mem::replace(&mut layer.content, SceneContent::None);
                    layer.content = SceneContent::Filtered(Box::new(crate::composition::FilteredContent { content, rect, filters: op }));
                }
            }
            ClipEffectFrame::Crop { left, top, right, bottom } => {
                let p = [*left, *top, *right, *bottom];
                if !p.iter().all(|v| v.is_finite() && (0.0..=100.0).contains(v)) {
                    return Err(bad("clip_crop"));
                }
                layer = wrap(layer, rect, ClipStage::Crop { left: p[0] / 100.0, top: p[1] / 100.0, right: p[2] / 100.0, bottom: p[3] / 100.0 });
            }
            ClipEffectFrame::Filters(op) => {
                if !op.check() {
                    return Err(bad("clip_filters"));
                }
                let content = std::mem::replace(&mut layer.content, SceneContent::None);
                layer.content = SceneContent::Filtered(Box::new(crate::composition::FilteredContent { content, rect, filters: op.clone() }));
            }
            ClipEffectFrame::Transform { .. } | ClipEffectFrame::Opacity(_) => {}
        }
    }
    for e in effects {
        match e {
            ClipEffectFrame::Transform { anchor, position, scale, rotation } => {
                let v = [anchor[0], anchor[1], position[0], position[1], scale[0], scale[1], *rotation];
                if !v.iter().all(|x| x.is_finite()) {
                    return Err(bad("clip_transform"));
                }
                let t = hsk_studio_pigment::transform_math::Affine2::from_parts(
                    anchor[0],
                    anchor[1],
                    scale[0] / 100.0,
                    scale[1] / 100.0,
                    *rotation,
                    position[0] - anchor[0],
                    position[1] - anchor[1],
                );
                layer.matrix = layer.matrix.then(t);
            }
            ClipEffectFrame::Opacity(o) => {
                if !o.is_finite() || !(0.0..=100.0).contains(o) {
                    return Err(bad("clip_opacity"));
                }
                layer.opacity *= o / 100.0;
            }
            _ => {}
        }
    }
    Ok(layer)
}

/// The clip-local tick of sequence tick `tick` for a clip starting at `timeline_start` (negative
/// before the clip).
pub fn clip_local_tick(tick: u64, timeline_start: u64) -> Option<i64> {
    let d = i128::from(tick).checked_sub(i128::from(timeline_start))?;
    i64::try_from(d).ok()
}

/// Evaluates a Folio clip effect stack at clip-local tick `clip_tick` through motion's keyframe
/// evaluator (the same Folio-to-motion keyframe mapping as composition transform streams);
/// disabled entries are skipped; grades are prepared through `luts`.
#[cfg(feature = "motion")]
pub fn evaluate_effect_stack(
    stack: &hsk_studio_folio::StudioClipEffectStack,
    clip_tick: i64,
    luts: &dyn LutSource,
    luma: [f32; 3],
    cancel: &hsk_studio_accord::CancellationToken,
) -> Result<Vec<ClipEffectFrame>, TemporalError> {
    use hsk_studio_folio::StudioClipEffectParams as P;
    let id = stack.clip_id.as_str();
    let at = |v: &hsk_studio_folio::StudioKeyedValue, n: usize| -> Result<Vec<f64>, TemporalError> {
        let p = crate::temporal::composition::keyed_property(id, v)?;
        let out = p
            .sample(hsk_studio_motion::EvalTick::new(clip_tick), cancel)
            .map_err(|e| TemporalError::Source { layer_id: id.to_owned(), code: e.code().to_owned() })?;
        if out.len() == n { Ok(out) } else { Err(source(id, CLIP_GRADE_PARAMETER)) }
    };
    let pair = |v: Vec<f64>| [v.first().copied().unwrap_or(0.0), v.get(1).copied().unwrap_or(0.0)];
    let one = |v: Vec<f64>| v.first().copied().unwrap_or(0.0);
    let mut out = Vec::with_capacity(stack.effects.len());
    for e in stack.effects.iter().filter(|e| e.enabled) {
        cancel.check().map_err(|_| TemporalError::Composite(crate::CompositeError::Canceled))?;
        out.push(match &e.params {
            P::ColorGrade(g) if g.keyframes.is_empty() => ClipEffectFrame::Grade(Arc::new(ClipGrade::prepare(g, luts, luma, id)?)),
            P::ColorGrade(g) => {
                // Keyframed grade scalars (lane SMB-PR-SOURCE-MONITOR): each stream sampled at the
                // clip tick through motion, written (clamped to its hard bound) over the static field.
                let mut at_tick = (**g).clone();
                for k in &g.keyframes {
                    k.param.set(&mut at_tick, one(at(&k.value, 1)?));
                }
                at_tick.keyframes.clear();
                ClipEffectFrame::Grade(Arc::new(ClipGrade::prepare(&at_tick, luts, luma, id)?))
            }
            P::Transform { anchor, position, scale, rotation } => ClipEffectFrame::Transform {
                anchor: pair(at(anchor, 2)?),
                position: pair(at(position, 2)?),
                scale: pair(at(scale, 2)?),
                rotation: one(at(rotation, 1)?),
            },
            P::Opacity { opacity } => ClipEffectFrame::Opacity(one(at(opacity, 1)?).clamp(0.0, 100.0)),
            P::Crop { left, top, right, bottom } => ClipEffectFrame::Crop {
                left: one(at(left, 1)?).clamp(0.0, 100.0),
                top: one(at(top, 1)?).clamp(0.0, 100.0),
                right: one(at(right, 1)?).clamp(0.0, 100.0),
                bottom: one(at(bottom, 1)?).clamp(0.0, 100.0),
            },
            P::GaussianBlur { blurriness } => {
                let r = one(at(blurriness, 1)?).max(0.0);
                ClipEffectFrame::Filters(filters(id, hsk_studio_folio::StudioSmartFilterKind::GaussianBlur { radius_px: f(r) })?)
            }
            P::PigmentFilter { filter } => ClipEffectFrame::Filters(filters(id, filter.clone())?),
        });
    }
    Ok(out)
}

/// Pigment's envelope form of one Folio live filter (Pigment's own record conversion).
pub fn filters(clip_id: &str, kind: hsk_studio_folio::StudioSmartFilterKind) -> Result<hsk_studio_pigment::smart_filter::SmartFilterOp, TemporalError> {
    let rec = hsk_studio_folio::StudioSmartFilterStack {
        enabled: true,
        stack_mask: None,
        filters: vec![hsk_studio_folio::StudioSmartFilter {
            instance_id: "clip".into(),
            enabled: true,
            opacity: 100.0,
            mask: None,
            params: kind,
        }],
    };
    hsk_studio_pigment::smart_filter::SmartFilterOp::from_record(&rec, |_| None).map_err(|_| source(clip_id, CLIP_EFFECT_FILTER))
}

/// Refusal code of a live filter Pigment cannot express.
pub const CLIP_EFFECT_FILTER: &str = "clip_effect_filter";

// ---------------------------------------------------------------------------------------------
// Slice 4: caption burn-in (STU-VID-037 caption tracks rendered into the frame at export).
// ---------------------------------------------------------------------------------------------

/// Layout of burned-in captions in a `width` x `height` sequence frame (pixels).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaptionLayout {
    pub width: f64,
    pub height: f64,
    /// Distance of the outer baseline (bottom) or the first line's top band (top) from the edge,
    /// and of left / right aligned ink from the side.
    pub margin_px: f64,
    pub line_height_px: f64,
}

/// One caption segment burned in: the caller shapes the segment text (Type owns shaping; italic and
/// bold select the font) into `glyphs`, each line's pen origin at x 0 on baseline y 0 with
/// `AnimatedGlyph::line` = line index; this places every line by its ink extent (Studio reading,
/// Premiere's caption layout UNVERIFIED): bottom placement puts the last baseline `margin_px` above
/// the bottom edge and earlier lines `line_height_px` apart above it; top placement puts line `k`'s
/// baseline at `margin_px + (k + 1) line_height_px`; centre / left / right alignment per line.
/// Glyphs are drawn by the animated-text path (nib coverage, one plane per glyph) in the segment
/// colour (white when unset). Underline is not drawn (gap). `None` when nothing is inked.
#[cfg(feature = "motion")]
#[allow(clippy::too_many_arguments)]
pub fn caption_burn_in(
    layer_id: &str,
    segment: &hsk_studio_folio::StudioCaptionSegment,
    glyphs: &[crate::text::animate::AnimatedGlyph],
    layout: &CaptionLayout,
    space: hsk_studio_pigment::render_plan::ColourTag,
    revision: u64,
    cancel: &hsk_studio_accord::CancellationToken,
) -> Result<Option<SceneLayer>, TemporalError> {
    use hsk_studio_folio::{StudioCaptionAlign as Align, StudioCaptionPlacement as Place};
    let bad = |field: &'static str| TemporalError::InvalidState { layer_id: layer_id.to_owned(), field };
    let l = layout;
    if ![l.width, l.height, l.margin_px, l.line_height_px].iter().all(|v| v.is_finite() && *v >= 0.0) {
        return Err(bad("caption_layout"));
    }
    let lines = glyphs.iter().map(|g| g.line).max().map_or(0u32, |m| m.saturating_add(1));
    // Ink extent per line.
    let mut extent: Vec<Option<(f64, f64)>> = vec![None; usize::try_from(lines).map_err(|_| bad("caption_lines"))?];
    for g in glyphs.iter().filter(|g| !g.is_space) {
        let Some(frame) = crate::text::painted::path_frame(&g.path) else { continue };
        let slot = extent.get_mut(usize::try_from(g.line).map_err(|_| bad("caption_lines"))?).ok_or_else(|| bad("caption_lines"))?;
        let (x0, x1) = (f64::from(frame.x), f64::from(frame.x) + f64::from(frame.width));
        *slot = Some(slot.map_or((x0, x1), |(a, b)| (a.min(x0), b.max(x1))));
    }
    let n = f64::from(lines);
    let mut transforms = Vec::with_capacity(glyphs.len());
    for g in glyphs {
        let k = f64::from(g.line);
        let baseline = match segment.style.placement {
            Place::Bottom => l.height - l.margin_px - (n - 1.0 - k) * l.line_height_px,
            Place::Top => l.margin_px + (k + 1.0) * l.line_height_px,
        };
        let (x0, x1) = extent.get(usize::try_from(g.line).map_err(|_| bad("caption_lines"))?).copied().flatten().unwrap_or((0.0, 0.0));
        let dx = match segment.style.align {
            Align::Center => (l.width - (x1 - x0)) / 2.0 - x0,
            Align::Left => l.margin_px - x0,
            Align::Right => l.width - l.margin_px - x1,
        };
        let mut t = hsk_studio_motion::CharTransform::IDENTITY;
        t.position = [dx, baseline];
        transforms.push(t);
    }
    let fill = segment.style.colour.map_or([1.0f32; 4], |c| c.map(f));
    let content = crate::text::animate::animated_text_content(
        layer_id,
        glyphs,
        &transforms,
        fill,
        space,
        revision,
        &hsk_studio_nib::raster::RasterOptions::default(),
        cancel,
    )
    .map_err(TemporalError::Composite)?;
    Ok(content.map(|c| SceneLayer {
        layer_id: layer_id.to_owned(),
        content: SceneContent::Static(Box::new(c)),
        mode: hsk_studio_pigment::blend_math::StudioBlendMode::Normal,
        opacity: 1.0,
        fill_opacity: 1.0,
        matrix: hsk_studio_pigment::transform_math::Affine2::IDENTITY,
        matte: None,
        clipped: false,
    }))
}

// ---------------------------------------------------------------------------------------------
// Slice 3: titles and graphics (STU-VID-070 motion-graphics templates placed as clips).
// ---------------------------------------------------------------------------------------------

/// STU-VID-070c determinate result code.
pub const TEMPLATE_REQUIREMENT_UNMET: &str = "TEMPLATE_REQUIREMENT_UNMET";
/// Refusal codes of template instantiation.
pub const TEMPLATE_MISMATCH: &str = "template_mismatch";
pub const TEMPLATE_COMPOSITION: &str = "template_composition";
pub const TEMPLATE_TARGET: &str = "template_target";

/// STU-VID-070c: "opening a template whose requirements are unmet produces a determinate
/// `TEMPLATE_REQUIREMENT_UNMET` result naming what is missing -- a missing font is never silently
/// substituted".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateRequirementUnmet {
    pub template_id: String,
    pub missing_fonts: Vec<String>,
    pub missing_effects: Vec<String>,
    pub missing_renderer: Option<String>,
}
impl TemplateRequirementUnmet {
    pub const fn code(&self) -> &'static str {
        TEMPLATE_REQUIREMENT_UNMET
    }
}

/// Checks a template's declared requirements against the host (no substitution, every missing item
/// named).
pub fn check_template_requirements(
    t: &hsk_studio_folio::StudioMotionTemplate,
    font_available: &dyn Fn(&str) -> bool,
    effect_available: &dyn Fn(&str) -> bool,
    renderer_available: &dyn Fn(&str) -> bool,
) -> Result<(), TemplateRequirementUnmet> {
    let missing_fonts: Vec<String> = t.required_fonts.iter().filter(|f| !font_available(f)).cloned().collect();
    let missing_effects: Vec<String> = t.effects_used.iter().filter(|e| !effect_available(e)).cloned().collect();
    let missing_renderer = t.renderer.clone().filter(|r| !renderer_available(r));
    if missing_fonts.is_empty() && missing_effects.is_empty() && missing_renderer.is_none() {
        Ok(())
    } else {
        Err(TemplateRequirementUnmet { template_id: t.template_id.clone(), missing_fonts, missing_effects, missing_renderer })
    }
}

/// A text layer's resolved source text and typography (the caller shapes it; locked fields carry
/// the template's locked values).
#[derive(Clone, Debug, PartialEq)]
pub struct TemplateText {
    pub layer_id: String,
    pub text: String,
    pub font_family: String,
    pub font_size: f64,
    pub faux_bold: bool,
    pub faux_italic: bool,
    pub all_caps: bool,
    pub small_caps: bool,
}

/// A placed template resolved at one clip tick: patched composition and motion records (render
/// them with `temporal::composition::CompositionTemporalSource`), the text the caller shapes and
/// the text fills.
#[derive(Clone, Debug, PartialEq)]
pub struct TemplateFrame {
    pub composition_id: String,
    pub compositions: hsk_studio_folio::StudioCompositionRecords,
    pub motion: hsk_studio_folio::StudioMotionRecords,
    pub text: Vec<TemplateText>,
    pub text_fill: std::collections::BTreeMap<String, [f32; 4]>,
}

#[cfg(feature = "motion")]
fn shape_fill<'a>(group: &'a mut hsk_studio_folio::StudioShapeGroup, path: &[u32]) -> Option<&'a mut [f64; 4]> {
    let (first, rest) = path.split_first()?;
    let item = group.items.get_mut(usize::try_from(*first).ok()?)?;
    match (item, rest.is_empty()) {
        (hsk_studio_folio::StudioShapeItem::Fill { rgba, .. }, true) => Some(rgba),
        (hsk_studio_folio::StudioShapeItem::Group { group }, false) => shape_fill(group, rest),
        _ => None,
    }
}

/// Resolves graphic clip `g` of template `t` at clip-local tick `clip_tick`: unset controls take
/// the template defaults; a source-text control yields a [`TemplateText`]; a colour control writes
/// a shape `fill` item or a text fill; an unchecked checkbox sets the target layer's opacity stream
/// to 0 (Studio reading of "boolean"); a numeric slider (evaluated through motion when keyed) holds
/// one component of the target layer's transform stream at the value (static value and every
/// keyframe), creating an opacity / rotation / scale stream from its STU-MOT-013 default when
/// absent (anchor and position have no stored default and are refused `template_target`). Every
/// target must be a layer of the template's composition.
#[cfg(feature = "motion")]
pub fn instantiate_template(
    t: &hsk_studio_folio::StudioMotionTemplate,
    g: &hsk_studio_folio::StudioGraphicInstance,
    compositions: &hsk_studio_folio::StudioCompositionRecords,
    motion: &hsk_studio_folio::StudioMotionRecords,
    clip_tick: i64,
    cancel: &hsk_studio_accord::CancellationToken,
) -> Result<TemplateFrame, TemporalError> {
    use hsk_studio_folio::{StudioColourTarget as Target, StudioControlKind as K, StudioControlSetting as V, StudioLayerProperty as Prop};
    let id = g.clip_id.as_str();
    if g.template_id != t.template_id {
        return Err(source(id, TEMPLATE_MISMATCH));
    }
    let mut comps = compositions.clone();
    let mut motion = motion.clone();
    let comp = comps
        .compositions
        .iter_mut()
        .find(|c| c.composition_id == t.composition_id)
        .ok_or_else(|| source(id, TEMPLATE_COMPOSITION))?;
    let target_layer = |layer_id: &str, comp: &hsk_studio_folio::StudioComposition| -> Result<usize, TemporalError> {
        comp.layers.iter().position(|l| l.layer_id == layer_id).ok_or_else(|| source(id, TEMPLATE_TARGET))
    };
    let mut frame_text = Vec::new();
    let mut text_fill = std::collections::BTreeMap::new();
    for c in &t.controls {
        cancel.check().map_err(|_| TemporalError::Composite(crate::CompositeError::Canceled))?;
        let set = g.values.iter().find(|v| v.control_id == c.control_id).map(|v| &v.setting);
        match &c.control {
            K::Group { .. } => {}
            K::SourceText { layer_id, default_text, locked_font, locked_size, all_caps, small_caps, bold, italic, .. } => {
                target_layer(layer_id, comp)?;
                let mut out = TemplateText {
                    layer_id: layer_id.clone(),
                    text: default_text.clone(),
                    font_family: locked_font.clone(),
                    font_size: *locked_size,
                    faux_bold: *bold,
                    faux_italic: *italic,
                    all_caps: *all_caps,
                    small_caps: *small_caps,
                };
                if let Some(V::Text { text, font_family, font_size, faux_bold, faux_italic }) = set {
                    // Folio refused any change the template locks (STU-VID-070a hard permission).
                    out.text.clone_from(text);
                    if let Some(fam) = font_family {
                        out.font_family.clone_from(fam);
                    }
                    out.font_size = font_size.unwrap_or(out.font_size);
                    out.faux_bold = faux_bold.unwrap_or(out.faux_bold);
                    out.faux_italic = faux_italic.unwrap_or(out.faux_italic);
                }
                frame_text.push(out);
            }
            K::Colour { target, default } => {
                let rgba = match set {
                    Some(V::Colour { rgba }) => *rgba,
                    _ => *default,
                };
                match target {
                    Target::ShapeFill { layer_id, path } => {
                        target_layer(layer_id, comp)?;
                        let layer = motion
                            .shape_layers
                            .iter_mut()
                            .find(|s| s.layer_id == *layer_id)
                            .ok_or_else(|| source(id, TEMPLATE_TARGET))?;
                        let slot = shape_fill(&mut layer.root, path).ok_or_else(|| source(id, TEMPLATE_TARGET))?;
                        *slot = rgba;
                    }
                    Target::TextFill { layer_id } => {
                        target_layer(layer_id, comp)?;
                        text_fill.insert(layer_id.clone(), rgba.map(f));
                    }
                }
            }
            K::Checkbox { layer_id, default } => {
                let i = target_layer(layer_id, comp)?;
                let checked = match set {
                    Some(V::Checkbox { checked }) => *checked,
                    _ => *default,
                };
                if !checked {
                    let layer = comp.layers.get_mut(i).ok_or_else(|| source(id, TEMPLATE_TARGET))?;
                    layer.streams.retain(|s| s.property != Prop::Opacity);
                    layer.streams.push(hsk_studio_folio::StudioPropertyStream {
                        property: Prop::Opacity,
                        static_value: vec![0.0],
                        keyframes: Vec::new(),
                    });
                }
            }
            K::NumericSlider { layer_id, property, component, default, .. } => {
                let i = target_layer(layer_id, comp)?;
                let v = match set {
                    Some(V::Number { value }) => {
                        let p = crate::temporal::composition::keyed_property(id, value)?;
                        let out = p
                            .sample(hsk_studio_motion::EvalTick::new(clip_tick), cancel)
                            .map_err(|e| TemporalError::Source { layer_id: id.to_owned(), code: e.code().to_owned() })?;
                        out.first().copied().ok_or_else(|| source(id, TEMPLATE_TARGET))?
                    }
                    _ => *default,
                };
                let layer = comp.layers.get_mut(i).ok_or_else(|| source(id, TEMPLATE_TARGET))?;
                if !layer.streams.iter().any(|s| s.property == *property) {
                    let base = match property {
                        Prop::Opacity => vec![100.0],
                        Prop::Rotation => vec![0.0],
                        Prop::Scale => vec![100.0, 100.0],
                        Prop::Anchor | Prop::Position => return Err(source(id, TEMPLATE_TARGET)),
                    };
                    layer.streams.push(hsk_studio_folio::StudioPropertyStream { property: *property, static_value: base, keyframes: Vec::new() });
                }
                let k = usize::from(*component);
                for s in layer.streams.iter_mut().filter(|s| s.property == *property) {
                    let slot = s.static_value.get_mut(k).ok_or_else(|| source(id, TEMPLATE_TARGET))?;
                    *slot = v;
                    for key in &mut s.keyframes {
                        let slot = key.value.get_mut(k).ok_or_else(|| source(id, TEMPLATE_TARGET))?;
                        *slot = v;
                    }
                }
            }
        }
    }
    Ok(TemplateFrame { composition_id: t.composition_id.clone(), compositions: comps, motion, text: frame_text, text_fill })
}
