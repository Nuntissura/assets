//! Local adjustments (lane SMB-DEVELOP-CONTROLS slice 3): corrections made of linear-gradient,
//! radial-gradient and brush masks that apply a subset of the Basic controls locally.
//!
//! Spec basis: STU-RAW-121 (correction and mask record: `correction_amount` is a gain, `mask_value`,
//! `mask_inverted`, `mask_blend_mode` observed 0 and 1; linear gradient `zero_x/zero_y/full_x/full_y/
//! flipped`, radial `top/left/bottom/right/angle/midpoint/roundness/feather/flipped`, brush `dabs`
//! as `"d x y"` normalised strokes plus `radius/flow/feather`; "the mask is re-rendered from the dab
//! list at the current resolution"), STU-RAW-122 (the local slider stack is on a normalised +-1
//! scale, NOT the +-100 counts of the global stack), STU-CON-045 (develop owns no folio path).
//!
//! Brush masks are rasterised inside this crate from the dab list. The pigment brush and selection
//! planes are a sibling capability: reusing them here would need a new module edge from develop to
//! the pigment brush surface, which this lane did not add (report to the lead).
//!
//! Choices that are this crate's own and UNVERIFIED against Lightroom (oracle session D5): the
//! `mask_blend_mode` reading (0 add, 1 subtract), `flipped` as inversion, brush `radius` as a
//! fraction of the shorter image side, local exposure as +-4 stops at +-1, gradients linear, the
//! radial `midpoint` (validated and retained, no effect on the output), and the shape of roundness.
use crate::DevelopError;
use crate::adjust::{
    Adjustments, COUNT_LIMIT, PreparedAdjust, WhiteBalanceShift, clarity_base, clarity_offset, dec, enc,
    encoded_luma,
};
use crate::par::{Threads, for_each_band};
use hsk_studio_accord::CancellationToken;

/// Stops of local exposure at a normalised slider of +-1.
pub const LOCAL_EXPOSURE_STOPS: f64 = 4.0;
/// Most dabs one brush mask may carry.
pub const MAX_DABS: usize = 65_536;
/// Largest `correction_amount` gain accepted.
pub const MAX_AMOUNT: f64 = 4.0;
/// Most corrections and masks per correction.
pub const MAX_CORRECTIONS: usize = 64;
pub const MAX_MASKS: usize = 16;
/// Upper bound on rasterised brush pixel visits per mask (dab bounding boxes summed).
pub const BRUSH_VISIT_BUDGET: u64 = 1 << 31;

/// The local slider stack (STU-RAW-122): normalised signed reals, identity 0, refused outside +-1.
/// The effective value is the authored slider times the correction's `amount`, and must stay within
/// +-1 (refused otherwise; nothing is clipped).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LocalStack {
    pub exposure: f64,
    pub contrast: f64,
    pub highlights: f64,
    pub shadows: f64,
    pub whites: f64,
    pub blacks: f64,
    pub clarity: f64,
    pub saturation: f64,
    pub temperature: f64,
    pub tint: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinearGradient {
    pub zero: (f64, f64),
    pub full: (f64, f64),
    pub flipped: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RadialGradient {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    /// Degrees, rotation of the ellipse axes.
    pub angle: f64,
    /// Count 0..100 (spec default 50); validated and retained, does not change the output.
    pub midpoint: f64,
    /// Count -100..100; the ellipse exponent is `2^(1 + roundness/100)` (diamond .. square).
    pub roundness: f64,
    /// Count 0..100; the fraction of the radius over which the mask falls from 1 to 0.
    pub feather: f64,
    pub flipped: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Brush {
    /// Normalised stroke positions (the `x y` of each `"d x y"` record), in stroke order.
    pub dabs: Vec<(f64, f64)>,
    /// Fraction of the shorter image side, in (0, 1].
    pub radius: f64,
    /// 0..1 opacity of one dab.
    pub flow: f64,
    /// 0..1 fraction of the radius over which a dab falls off.
    pub feather: f64,
}

/// Range mask on the pixel's own encoded luma (STU-RAW-121 `luminance_range`): coverage 1 for luma in
/// `lo..=hi` (encoded 0..1), falling smoothly over `feather * 0.25` outside each edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LuminanceRange {
    pub lo: f64,
    pub hi: f64,
    /// 0..1; 0 is a hard edge.
    pub feather: f64,
}

/// Range mask on colour distance (STU-RAW-121 `colour_range`): coverage 1 within `r/2` of the nearest
/// sample in encoded RGB, falling to 0 at `r = 0.02 + 0.6 * breadth` (the refine / breadth control).
#[derive(Clone, Debug, PartialEq)]
pub struct ColourRange {
    /// 1..=16 sampled colours, encoded sRGB components in 0..1.
    pub samples: Vec<[f64; 3]>,
    /// 0..1.
    pub breadth: f64,
}

/// Most colour samples one colour-range mask may carry.
pub const MAX_COLOUR_SAMPLES: usize = 16;

/// Model-assisted mask sources of the STU-RAW-123 closed set (lane SMB-DEVELOP-LENS-PRESETS slice 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelMaskKind {
    Subject,
    Sky,
    Background,
    LandscapeComponent,
    Object,
    Person,
    PersonComponent,
}

impl ModelMaskKind {
    pub const ALL: [Self; 7] =
        [Self::Subject, Self::Sky, Self::Background, Self::LandscapeComponent, Self::Object, Self::Person, Self::PersonComponent];

    pub const fn key(self) -> &'static str {
        match self {
            Self::Subject => "subject",
            Self::Sky => "sky",
            Self::Background => "background",
            Self::LandscapeComponent => "landscape_component",
            Self::Object => "object",
            Self::Person => "person",
            Self::PersonComponent => "person_component",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.key() == key)
    }
}

/// Longest `model_version` text a model mask may carry.
pub const MAX_MODEL_VERSION: usize = 256;

/// A model-assisted mask (subject, sky, people, ...). STU-RAW-014a makes the model an optional
/// adapter that degrades to the manual sources; this build has NO model provider and downloads
/// nothing, so the record is kept (authored, validated, round-tripped) but evaluating it is the typed
/// refusal `model_unavailable:<kind>`. `subcategory` carries an imported vendor subcategory id
/// opaquely (STU-RAW-123); `model_version` records the model that produced a mask (none here).
#[derive(Clone, Debug, PartialEq)]
pub struct ModelMask {
    pub kind: ModelMaskKind,
    pub subcategory: Option<u32>,
    pub model_version: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MaskSource {
    LinearGradient(LinearGradient),
    RadialGradient(RadialGradient),
    Brush(Brush),
    LuminanceRange(LuminanceRange),
    ColourRange(ColourRange),
    /// Model-assisted source; evaluation is `model_unavailable` in this build (see [`ModelMask`]).
    Model(ModelMask),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskBlend {
    Add,
    Subtract,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    pub id: String,
    pub active: bool,
    pub inverted: bool,
    /// 0..1 (`mask_value`).
    pub value: f64,
    pub blend: MaskBlend,
    pub source: MaskSource,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Correction {
    pub id: String,
    pub name: String,
    pub active: bool,
    /// `correction_amount`: a gain on the sliders (0..=4), default 1.
    pub amount: f64,
    pub masks: Vec<Mask>,
    pub stack: LocalStack,
}

impl Correction {
    pub fn new(id: &str, masks: Vec<Mask>, stack: LocalStack) -> Self {
        Self { id: id.to_owned(), name: String::new(), active: true, amount: 1.0, masks, stack }
    }
}

/// Parse one `"d x y"` brush record of STU-RAW-121 into its position.
pub fn parse_dab(record: &str) -> Result<(f64, f64), DevelopError> {
    let mut parts = record.split_whitespace();
    let (Some("d"), Some(x), Some(y), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(DevelopError::InvalidMask);
    };
    let parse = |s: &str| s.parse::<f64>().ok().filter(|v| v.is_finite()).ok_or(DevelopError::InvalidMask);
    Ok((parse(x)?, parse(y)?))
}

fn finite(values: &[f64]) -> Result<(), DevelopError> {
    if values.iter().all(|v| v.is_finite()) { Ok(()) } else { Err(DevelopError::NonFinite) }
}

impl Mask {
    fn validate(&self) -> Result<(), DevelopError> {
        finite(&[self.value])?;
        if !(0.0..=1.0).contains(&self.value) {
            return Err(DevelopError::InvalidMask);
        }
        match &self.source {
            MaskSource::LinearGradient(g) => {
                finite(&[g.zero.0, g.zero.1, g.full.0, g.full.1])?;
                let (dx, dy) = (g.full.0 - g.zero.0, g.full.1 - g.zero.1);
                if g.zero.0.abs().max(g.zero.1.abs()).max(g.full.0.abs()).max(g.full.1.abs()) > 16.0
                    || dx * dx + dy * dy < 1.0e-12
                {
                    return Err(DevelopError::InvalidMask);
                }
            }
            MaskSource::RadialGradient(r) => {
                finite(&[r.left, r.top, r.right, r.bottom, r.angle, r.midpoint, r.roundness, r.feather])?;
                if r.left.abs().max(r.top.abs()).max(r.right.abs()).max(r.bottom.abs()) > 16.0
                    || r.right - r.left < 1.0e-6
                    || r.bottom - r.top < 1.0e-6
                    || !(0.0..=100.0).contains(&r.midpoint)
                    || !(-100.0..=100.0).contains(&r.roundness)
                    || !(0.0..=100.0).contains(&r.feather)
                    || r.angle.abs() > 1.0e6
                {
                    return Err(DevelopError::InvalidMask);
                }
            }
            MaskSource::LuminanceRange(r) => {
                finite(&[r.lo, r.hi, r.feather])?;
                if !(0.0..=1.0).contains(&r.lo)
                    || !(0.0..=1.0).contains(&r.hi)
                    || r.lo >= r.hi
                    || !(0.0..=1.0).contains(&r.feather)
                {
                    return Err(DevelopError::InvalidMask);
                }
            }
            MaskSource::ColourRange(r) => {
                finite(&[r.breadth])?;
                if r.samples.is_empty() || r.samples.len() > MAX_COLOUR_SAMPLES || !(0.0..=1.0).contains(&r.breadth) {
                    return Err(DevelopError::InvalidMask);
                }
                for sample in &r.samples {
                    finite(sample)?;
                    if sample.iter().any(|c| !(0.0..=1.0).contains(c)) {
                        return Err(DevelopError::InvalidMask);
                    }
                }
            }
            MaskSource::Model(m) => {
                if m.model_version.as_ref().is_some_and(|v| v.len() > MAX_MODEL_VERSION) {
                    return Err(DevelopError::InvalidMask);
                }
            }
            MaskSource::Brush(b) => {
                finite(&[b.radius, b.flow, b.feather])?;
                if b.dabs.len() > MAX_DABS
                    || !(f64::MIN_POSITIVE..=1.0).contains(&b.radius)
                    || !(0.0..=1.0).contains(&b.flow)
                    || !(0.0..=1.0).contains(&b.feather)
                {
                    return Err(DevelopError::InvalidMask);
                }
                for (x, y) in &b.dabs {
                    finite(&[*x, *y])?;
                    if x.abs().max(y.abs()) > 16.0 {
                        return Err(DevelopError::InvalidMask);
                    }
                }
            }
        }
        Ok(())
    }
}

impl LocalStack {
    fn values(&self) -> [f64; 10] {
        [
            self.exposure, self.contrast, self.highlights, self.shadows, self.whites, self.blacks, self.clarity,
            self.saturation, self.temperature, self.tint,
        ]
    }
}

impl Correction {
    /// Structural validation (independent of which group is enabled).
    pub(crate) fn validate(&self) -> Result<(), DevelopError> {
        finite(&[self.amount])?;
        if !(0.0..=MAX_AMOUNT).contains(&self.amount) || self.masks.len() > MAX_MASKS {
            return Err(DevelopError::InvalidMask);
        }
        finite(&self.stack.values())?;
        for v in self.stack.values() {
            if v.abs() > 1.0 || (v * self.amount).abs() > 1.0 {
                return Err(DevelopError::InvalidAdjustment);
            }
        }
        self.masks.iter().try_for_each(Mask::validate)
    }
}

// ---------------------------------------------------------------------------------------------

fn smoothstep(t: f64) -> f64 {
    let c = t.clamp(0.0, 1.0);
    (c * c) * (3.0 - 2.0 * c)
}

enum Geometry {
    Linear(LinearGradient),
    Radial(RadialGradient),
    /// Index into the rasterised coverage planes.
    Brush(usize),
    LuminanceRange(LuminanceRange),
    ColourRange(ColourRange),
}

struct PreparedMask {
    geometry: Geometry,
    inverted: bool,
    value: f64,
    blend: MaskBlend,
}

pub(crate) struct PreparedCorrection {
    masks: Vec<PreparedMask>,
    exposure_gain: f64,
    adjust: PreparedAdjust,
    clarity_gain: f64,
}

#[derive(Default)]
pub(crate) struct PreparedLocal {
    corrections: Vec<PreparedCorrection>,
    brush_sources: Vec<Brush>,
}

impl PreparedLocal {
    pub(crate) fn is_empty(&self) -> bool {
        self.corrections.is_empty()
    }

    pub(crate) fn prepare(global: &Adjustments, corrections: &[Correction]) -> Result<Self, DevelopError> {
        if corrections.len() > MAX_CORRECTIONS {
            return Err(DevelopError::InvalidMask);
        }
        let mut local = Self::default();
        for correction in corrections {
            correction.validate()?;
            if !correction.active {
                continue;
            }
            let s = correction.stack;
            let a = correction.amount;
            let count = |v: f64| v * a * COUNT_LIMIT;
            let adjust = Adjustments {
                white_balance: WhiteBalanceShift { temperature: count(s.temperature), tint: count(s.tint) },
                tone: crate::adjust::BasicTone {
                    contrast: count(s.contrast),
                    highlights: count(s.highlights),
                    shadows: count(s.shadows),
                    whites: count(s.whites),
                    blacks: count(s.blacks),
                },
                presence: crate::adjust::Presence { vibrance: 0.0, saturation: count(s.saturation), clarity: 0.0 },
                                working_to_xyz_d50: global.working_to_xyz_d50,
                luma: global.luma,
                ..Adjustments::default()
            };
            let prepared = PreparedAdjust::prepare(
                &adjust,
                crate::adjust::Gates { colour: true, curve: true, ..crate::adjust::Gates::NONE },
            )?;
            let exposure_gain = (s.exposure * a * LOCAL_EXPOSURE_STOPS).exp2();
            if !exposure_gain.is_finite() || exposure_gain <= 0.0 {
                return Err(DevelopError::StageOverflow);
            }
            let mut masks = Vec::new();
            for mask in correction.masks.iter().filter(|m| m.active) {
                let geometry = match &mask.source {
                    MaskSource::LinearGradient(g) => Geometry::Linear(*g),
                    MaskSource::RadialGradient(r) => Geometry::Radial(*r),
                    MaskSource::Brush(b) => {
                        local.brush_sources.push(b.clone());
                        Geometry::Brush(local.brush_sources.len() - 1)
                    }
                    MaskSource::LuminanceRange(r) => Geometry::LuminanceRange(*r),
                    MaskSource::ColourRange(r) => Geometry::ColourRange(r.clone()),
                    MaskSource::Model(m) => return Err(DevelopError::ModelUnavailable(m.kind.key())),
                };
                masks.push(PreparedMask { geometry, inverted: mask.inverted, value: mask.value, blend: mask.blend });
            }
            local.corrections.push(PreparedCorrection {
                masks,
                exposure_gain,
                adjust: prepared,
                clarity_gain: s.clarity * a,
            });
        }
        Ok(local)
    }
}

/// Rasterise a brush into a coverage plane (`width * height` f32 in 0..1).
fn rasterise_brush(brush: &Brush, width: usize, height: usize) -> Result<Vec<f32>, DevelopError> {
    let radius = (brush.radius * width.min(height) as f64).max(0.5);
    let side = (2.0 * radius).ceil() + 2.0;
    if (brush.dabs.len() as f64) * side * side > BRUSH_VISIT_BUDGET as f64 {
        return Err(DevelopError::InvalidMask);
    }
    let mut plane = vec![0.0f32; width * height];
    let inner = 1.0 - brush.feather;
    for (nx, ny) in &brush.dabs {
        let (cx, cy) = (nx * width as f64, ny * height as f64);
        let x0 = ((cx - radius).floor().max(0.0)) as usize;
        let x1 = ((cx + radius).ceil().min(width as f64 - 1.0)).max(-1.0);
        let y0 = ((cy - radius).floor().max(0.0)) as usize;
        let y1 = ((cy + radius).ceil().min(height as f64 - 1.0)).max(-1.0);
        if x1 < 0.0 || y1 < 0.0 {
            continue;
        }
        for y in y0..=(y1 as usize) {
            for x in x0..=(x1 as usize) {
                let d = (((x as f64 + 0.5) - cx).hypot((y as f64 + 0.5) - cy)) / radius;
                if d >= 1.0 {
                    continue;
                }
                let falloff = if d <= inner { 1.0 } else { 1.0 - smoothstep((d - inner) / brush.feather) };
                let alpha = brush.flow * falloff;
                let slot = &mut plane[y * width + x];
                *slot = (1.0 - (1.0 - f64::from(*slot)) * (1.0 - alpha)) as f32;
            }
        }
    }
    Ok(plane)
}

#[allow(clippy::too_many_arguments)]
fn geometry_at(g: &Geometry, planes: &[Vec<f32>], x: usize, y: usize, width: usize, height: usize, pixel: [f64; 3], luma: &[f64; 3]) -> f64 {
    let px = (x as f64 + 0.5) / width as f64;
    let py = (y as f64 + 0.5) / height as f64;
    match g {
        Geometry::Linear(l) => {
            let (dx, dy) = (l.full.0 - l.zero.0, l.full.1 - l.zero.1);
            let t = ((px - l.zero.0) * dx + (py - l.zero.1) * dy) / (dx * dx + dy * dy);
            let v = t.clamp(0.0, 1.0);
            if l.flipped { 1.0 - v } else { v }
        }
        Geometry::Radial(r) => {
            let (cx, cy) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
            let a = (r.right - r.left) / 2.0 * width as f64;
            let b = (r.bottom - r.top) / 2.0 * height as f64;
            let (dx, dy) = ((px - cx) * width as f64, (py - cy) * height as f64);
            let (sin, cos) = r.angle.to_radians().sin_cos();
            let (u, v) = (dx * cos + dy * sin, -dx * sin + dy * cos);
            let n = (1.0 + r.roundness / 100.0).exp2();
            let radius = ((u.abs() / a).powf(n) + (v.abs() / b).powf(n)).powf(1.0 / n);
            let feather = r.feather / 100.0;
            let g = if radius >= 1.0 {
                0.0
            } else if feather == 0.0 {
                1.0
            } else {
                1.0 - smoothstep((radius - (1.0 - feather)) / feather)
            };
            if r.flipped { 1.0 - g } else { g }
        }
        Geometry::Brush(i) => f64::from(planes[*i][y * width + x]),
        Geometry::LuminanceRange(r) => {
            let sum = luma[0] + luma[1] + luma[2];
            let e = pixel.map(enc);
            let ly = ((luma[0] / sum) * e[0] + (luma[1] / sum) * e[1]) + (luma[2] / sum) * e[2];
            if r.feather == 0.0 {
                return if (r.lo..=r.hi).contains(&ly) { 1.0 } else { 0.0 };
            }
            let fw = r.feather * 0.25;
            smoothstep((ly - r.lo + fw) / (2.0 * fw)) * smoothstep((r.hi + fw - ly) / (2.0 * fw))
        }
        Geometry::ColourRange(r) => {
            let e = pixel.map(enc);
            let d = r
                .samples
                .iter()
                .map(|s| ((e[0] - s[0]).powi(2) + (e[1] - s[1]).powi(2) + (e[2] - s[2]).powi(2)).sqrt())
                .fold(f64::INFINITY, f64::min);
            let radius = 0.02 + 0.6 * r.breadth;
            1.0 - smoothstep((d - radius * 0.5) / (radius * 0.5))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn coverage(
    masks: &[PreparedMask],
    planes: &[Vec<f32>],
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    pixel: [f64; 3],
    luma: &[f64; 3],
) -> f64 {
    let mut m = 0.0f64;
    for mask in masks {
        let g = geometry_at(&mask.geometry, planes, x, y, width, height, pixel, luma);
        let v = mask.value * if mask.inverted { 1.0 - g } else { g };
        m = match mask.blend {
            MaskBlend::Add => m + v - m * v,
            MaskBlend::Subtract => m * (1.0 - v),
        };
    }
    m.clamp(0.0, 1.0)
}

/// Apply the prepared corrections to a linear RGB image in place. Rows are independent, so bands of
/// rows run on `threads` workers with identical results for every thread count; the image is
/// unchanged when the pass fails.
pub(crate) fn apply(
    rgb: &mut [f32],
    width: u32,
    height: u32,
    local: &PreparedLocal,
    luma: [f64; 3],
    threads: Threads,
    cancel: &CancellationToken,
) -> Result<(), DevelopError> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || w.checked_mul(h).and_then(|p| p.checked_mul(3)) != Some(rgb.len()) {
        return Err(DevelopError::InvalidDimensions);
    }
    if local.is_empty() {
        return Ok(());
    }
    let mut planes = Vec::with_capacity(local.brush_sources.len());
    for brush in &local.brush_sources {
        cancel.check()?;
        planes.push(rasterise_brush(brush, w, h)?);
    }
    let clarity = if local.corrections.iter().any(|c| c.clarity_gain != 0.0) {
        let plane = encoded_luma(rgb, &luma);
        let base = clarity_base(&plane, w, h);
        Some((plane, base))
    } else {
        None
    };
    let source: &[f32] = &*rgb;
    let mut out = rgb.to_vec();
    let rows_per_band = (65_536 / w).max(1);
    for_each_band(&mut out, rows_per_band * w * 3, threads, cancel, |band, chunk| {
        for (offset, row) in chunk.chunks_exact_mut(w * 3).enumerate() {
            let y = band * rows_per_band + offset;
            for (x, px) in row.chunks_exact_mut(3).enumerate() {
                // Range masks read the image as it was before any correction ran.
                let i = y * w + x;
                let before = [f64::from(source[i * 3]), f64::from(source[i * 3 + 1]), f64::from(source[i * 3 + 2])];
                for correction in &local.corrections {
                    let m = coverage(&correction.masks, &planes, x, y, w, h, before, &luma);
                    if m == 0.0 {
                        continue;
                    }
                    let original = [f64::from(px[0]), f64::from(px[1]), f64::from(px[2])];
                    let mut v = original.map(|c| c * correction.exposure_gain);
                    v = correction.adjust.pre(v)?;
                    v = correction.adjust.colour(v)?;
                    if let Some((plane, base)) = &clarity
                        && correction.clarity_gain != 0.0
                    {
                        let i = y * w + x;
                        let offset = clarity_offset(correction.clarity_gain, plane[i], base[i]);
                        if offset != 0.0 {
                            v = v.map(|c| dec(enc(c) + offset));
                        }
                    }
                    for (slot, (new, old)) in px.iter_mut().zip(v.into_iter().zip(original)) {
                        let blended = old + m * (new - old);
                        let f = blended as f32;
                        if !f.is_finite() {
                            return Err(DevelopError::StageOverflow);
                        }
                        *slot = f;
                    }
                }
            }
        }
        Ok(())
    })?;
    rgb.copy_from_slice(&out);
    Ok(())
}
