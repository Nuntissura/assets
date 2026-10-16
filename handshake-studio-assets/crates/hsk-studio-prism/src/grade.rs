//! Clip colour-grade maths (lane SMB-PREMIERE-COLOR-TITLES; Premiere "Lumetri"-style clip colour,
//! spec 14.8.7): the per-pixel functions Prism owns for the stages of STU-COL-160 that no other
//! engine already implements. Composite orders the stages and supplies the stages owned elsewhere
//! (Pigment's tone/RGB curve evaluator, vibrance and HSL conversion; the `.cube` resource is
//! [`crate::CubeLut`], lane SMB-DEVELOP-LENS-PRESETS); nothing here reads pixels positions except [`vignette_gain`].
//!
//! - [`Wheels`] / [`apply_wheels`]: STU-COL-222 "Colour wheels: the lift/gamma/gain model
//!   (NORMATIVE MATHEMATICS)": `TempGain.r = pow(2, gain.r + (Temperature + Magenta * 0.5))`,
//!   `TempGain.g = pow(2, gain.g + (-Magenta))`, `TempGain.b = pow(2, gain.b + (-Temperature +
//!   Magenta * 0.5))`, `Contrast' = pow(2, Contrast)`, `offset'.c = TempGain.c * (offset.c + Pivot -
//!   Contrast' * Pivot)`, `gain'.c = TempGain.c * Contrast'`, `gamma'.c = pow(3, gamma_band.c +
//!   gamma.c)`; "Band results compose by adding the band offset to the overall offset"; luminance
//!   weights are an explicit 3-vector chosen from the working space (never hard-coded Rec.601).
//!   The clause states the coefficients, not how they are applied or how the bands are weighted;
//!   Studio readings (UNVERIFIED, D5 oracle): `v = gain' * c + offset'`, then `out = max(v, 0) ^
//!   (1 / gamma')` (a positive gamma wheel brightens midtones), input saturation about the luma
//!   before, `PostSat` per channel about the luma after; band values (offset, gain and gamma per
//!   channel) are blended with luma weights `w_shadow = clamp(1 - Y / (2 lowMid))^2`,
//!   `w_highlight = clamp(1 - (1 - Y) / (2 (1 - midHigh)))^2`, `w_mid = 1 - w_shadow - w_highlight`
//!   (renormalised when negative); at the declared defaults `lowMid = midHigh = 0.5` this is the
//!   quadratic Bernstein basis `(1 - Y)^2, 2 Y (1 - Y), Y^2`.
//! - [`SecondaryKey`] / [`key_weight`]: STU-COL-227 "The KEY carries ... per-axis hue, saturation
//!   and luminance ranges with soft edges". Weight = product over the three axes of a trapezoid:
//!   1 inside `[lo, hi]`, falling linearly to 0 over `soft` outside (hue is circular, degrees). The
//!   set/add/remove-colour samplings are UI operations that produce these ranges.
//! - [`Vignette`] / [`vignette_gain`]: STU-COL-228 `amount` (-5..5), `midpoint` (0..100),
//!   `roundness` (-100..100), `feather` (0..100). Studio reading (UNVERIFIED): normalised offsets
//!   `u, v` from the frame centre in half-extents; roundness `r >= 0` blends the half-extents
//!   toward the shorter side (a circle at 100), `r < 0` raises the superellipse exponent from 2 to
//!   8 (more rectangular); `d = (|u|^p + |v|^p)^(1/p)`; `t = smoothstep(m (1 - f), m (1 + f), d)`
//!   with `m = midpoint / 100`, `f = feather / 100` (a hard edge when `f = 0`), and the gain is
//!   `2^(amount * t)` (negative darkens the edges).
//!
//! Every parameter is validated against the spec's hard bounds ([`GradeError`]); the functions
//! never allocate and never read beyond their arguments.

/// A typed refusal naming the parameter (`grade.<field>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GradeError(pub &'static str);
impl std::fmt::Display for GradeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid grade parameter {}", self.0)
    }
}
impl std::error::Error for GradeError {}

fn check(ok: bool, name: &'static str) -> Result<(), GradeError> {
    if ok { Ok(()) } else { Err(GradeError(name)) }
}
fn finite3(v: [f32; 3]) -> bool {
    v.iter().all(|x| x.is_finite())
}

/// Rec.709 luma weights (the default for an sRGB / Rec.709 working space).
pub const LUMA_REC709: [f32; 3] = [0.2126, 0.7152, 0.0722];

fn luma(c: [f32; 3], w: [f32; 3]) -> f32 {
    c[0] * w[0] + c[1] * w[1] + c[2] * w[2]
}

/// One wheel band (STU-COL-222 `offset`, `gamma`, `gain` per channel; all default 0).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WheelBand {
    pub offset: [f32; 3],
    pub gamma: [f32; 3],
    pub gain: [f32; 3],
}
impl WheelBand {
    fn valid(&self) -> bool {
        finite3(self.offset)
            && finite3(self.gamma)
            && finite3(self.gain)
            && self.offset.iter().chain(&self.gamma).chain(&self.gain).all(|v| v.abs() <= 16.0)
    }
    fn is_identity(&self) -> bool {
        *self == Self::default()
    }
}

/// The STU-COL-222 wheel set: overall band plus shadow / midtone / highlight bands and the scalars.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wheels {
    pub overall: WheelBand,
    pub shadows: WheelBand,
    pub midtones: WheelBand,
    pub highlights: WheelBand,
    /// `Temperature`, log2 units.
    pub temperature: f32,
    /// `Magenta`, log2 units.
    pub magenta: f32,
    /// `Contrast` (hard min -1), log2 units.
    pub contrast: f32,
    /// `Pivot`, default 0.5.
    pub pivot: f32,
    /// `Saturation` (input), default 1, hard max 2, clamp-at-min 0.
    pub input_saturation: f32,
    /// `PostSat` per channel, default (1, 1, 1).
    pub post_saturation: [f32; 3],
    /// `lowMid` / `midHigh` band splits, default 0.5.
    pub low_mid: f32,
    pub mid_high: f32,
    /// Explicit luminance weights (from the working space).
    pub luma: [f32; 3],
}
impl Default for Wheels {
    fn default() -> Self {
        Self {
            overall: WheelBand::default(),
            shadows: WheelBand::default(),
            midtones: WheelBand::default(),
            highlights: WheelBand::default(),
            temperature: 0.0,
            magenta: 0.0,
            contrast: 0.0,
            pivot: 0.5,
            input_saturation: 1.0,
            post_saturation: [1.0; 3],
            low_mid: 0.5,
            mid_high: 0.5,
            luma: LUMA_REC709,
        }
    }
}

impl Wheels {
    /// Bounds: STU-COL-222 hard bounds (contrast >= -1, saturations <= 2), finite everything,
    /// splits in `(0, 1)`, Studio caps |band value| <= 16 and |temperature|, |magenta| <= 8.
    pub fn validate(&self) -> Result<(), GradeError> {
        check(
            self.overall.valid() && self.shadows.valid() && self.midtones.valid() && self.highlights.valid(),
            "wheel.band",
        )?;
        check(self.temperature.is_finite() && self.temperature.abs() <= 8.0, "wheel.temperature")?;
        check(self.magenta.is_finite() && self.magenta.abs() <= 8.0, "wheel.magenta")?;
        check(self.contrast.is_finite() && (-1.0..=8.0).contains(&self.contrast), "wheel.contrast")?;
        check(self.pivot.is_finite() && (0.0..=1.0).contains(&self.pivot), "wheel.pivot")?;
        check(self.input_saturation.is_finite() && self.input_saturation <= 2.0, "wheel.input_saturation")?;
        check(
            finite3(self.post_saturation) && self.post_saturation.iter().all(|v| *v <= 2.0),
            "wheel.final_saturation",
        )?;
        check(
            self.low_mid.is_finite() && self.low_mid > 0.0 && self.low_mid < 1.0,
            "wheel.low_mid_split",
        )?;
        check(
            self.mid_high.is_finite() && self.mid_high > 0.0 && self.mid_high < 1.0,
            "wheel.mid_high_split",
        )?;
        check(finite3(self.luma) && self.luma.iter().all(|v| *v >= 0.0), "wheel.luma")
    }

    /// True when the wheels change nothing.
    pub fn is_identity(&self) -> bool {
        #[allow(clippy::float_cmp)]
        let scalars = self.temperature == 0.0
            && self.magenta == 0.0
            && self.contrast == 0.0
            && self.input_saturation == 1.0
            && self.post_saturation == [1.0; 3];
        scalars
            && self.overall.is_identity()
            && self.shadows.is_identity()
            && self.midtones.is_identity()
            && self.highlights.is_identity()
    }
}

/// Band weights `(shadow, midtone, highlight)` of luma `y` (module docs; a partition of unity).
pub fn band_weights(y: f32, low_mid: f32, mid_high: f32) -> [f32; 3] {
    let y = y.clamp(0.0, 1.0);
    let ws = (1.0 - y / (2.0 * low_mid)).clamp(0.0, 1.0).powi(2);
    let wh = (1.0 - (1.0 - y) / (2.0 * (1.0 - mid_high))).clamp(0.0, 1.0).powi(2);
    let wm = 1.0 - ws - wh;
    if wm >= 0.0 {
        [ws, wm, wh]
    } else {
        let s = ws + wh;
        [ws / s, 0.0, wh / s]
    }
}

fn saturate(c: [f32; 3], y: f32, k: [f32; 3]) -> [f32; 3] {
    [0usize, 1, 2].map(|i| y + (c[i] - y) * k[i])
}

/// Applies the STU-COL-222 wheels to one straight RGB triple (module docs). Output is not clamped.
pub fn apply_wheels(w: &Wheels, c: [f32; 3]) -> [f32; 3] {
    let y0 = luma(c, w.luma);
    let c = saturate(c, y0, [w.input_saturation.max(0.0); 3]);
    let [ws, wm, wh] = band_weights(luma(c, w.luma), w.low_mid, w.mid_high);
    let band = |f: fn(&WheelBand) -> [f32; 3]| -> [f32; 3] {
        let (o, s, m, h) = (f(&w.overall), f(&w.shadows), f(&w.midtones), f(&w.highlights));
        [0usize, 1, 2].map(|i| o[i] + ws * s[i] + wm * m[i] + wh * h[i])
    };
    let offset = band(|b| b.offset);
    let gain = band(|b| b.gain);
    let gamma = band(|b| b.gamma);
    let temp = [
        w.temperature + w.magenta * 0.5,
        -w.magenta,
        -w.temperature + w.magenta * 0.5,
    ];
    let contrast = w.contrast.exp2();
    let out = [0usize, 1, 2].map(|i| {
        let temp_gain = (gain[i] + temp[i]).exp2();
        let offset_p = temp_gain * (offset[i] + w.pivot - contrast * w.pivot);
        let gain_p = temp_gain * contrast;
        let gamma_p = 3.0f32.powf(gamma[i]);
        (gain_p * c[i] + offset_p).max(0.0).powf(1.0 / gamma_p)
    });
    saturate(out, luma(out, w.luma), w.post_saturation.map(|v| v.max(0.0)))
}

/// A secondary key range on one axis: full weight in `[lo, hi]`, linear soft edges of `soft`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeyRange {
    pub lo: f32,
    pub hi: f32,
    pub soft: f32,
}
impl KeyRange {
    /// The whole axis (weight 1 everywhere).
    pub const ALL: KeyRange = KeyRange { lo: 0.0, hi: 1.0, soft: 0.0 };
}

/// STU-COL-227 key: hue range in degrees (`lo > hi` wraps through 0), saturation and luminance
/// ranges as fractions `0..=1` (HSL).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SecondaryKey {
    pub hue: KeyRange,
    pub saturation: KeyRange,
    pub luminance: KeyRange,
    pub invert: bool,
}
impl SecondaryKey {
    pub fn validate(&self) -> Result<(), GradeError> {
        let h = self.hue;
        check(
            [h.lo, h.hi, h.soft].iter().all(|v| v.is_finite())
                && (0.0..=360.0).contains(&h.lo)
                && (0.0..=360.0).contains(&h.hi)
                && (0.0..=180.0).contains(&h.soft),
            "secondary.hue",
        )?;
        for (r, name) in [(self.saturation, "secondary.saturation_range"), (self.luminance, "secondary.luminance_range")] {
            check(
                [r.lo, r.hi, r.soft].iter().all(|v| v.is_finite())
                    && (0.0..=1.0).contains(&r.lo)
                    && (0.0..=1.0).contains(&r.hi)
                    && r.lo <= r.hi
                    && (0.0..=1.0).contains(&r.soft),
                name,
            )?;
        }
        Ok(())
    }
}

fn ramp(distance: f32, soft: f32) -> f32 {
    if distance <= 0.0 {
        1.0
    } else if soft <= 0.0 {
        0.0
    } else {
        (1.0 - distance / soft).clamp(0.0, 1.0)
    }
}

/// Key weight `0..=1` of an HSL triple (hue degrees, saturation and lightness fractions).
pub fn key_weight(key: &SecondaryKey, hsl: [f32; 3]) -> f32 {
    let [h, s, l] = hsl;
    let hue = {
        let r = key.hue;
        // Arc length from the hue to the [lo, hi] arc (lo > hi wraps through 0).
        let inside = if r.lo <= r.hi { h >= r.lo && h <= r.hi } else { h >= r.lo || h <= r.hi };
        if inside {
            1.0
        } else {
            let arc = |a: f32, b: f32| {
                let d = (a - b).rem_euclid(360.0);
                d.min(360.0 - d)
            };
            ramp(arc(h, r.lo).min(arc(h, r.hi)), r.soft)
        }
    };
    let linear = |r: KeyRange, v: f32| ramp((r.lo - v).max(v - r.hi), r.soft);
    let w = hue * linear(key.saturation, s) * linear(key.luminance, l);
    if key.invert { 1.0 - w } else { w }
}

/// STU-COL-228 vignette parameters on the spec scales.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vignette {
    pub amount: f32,
    pub midpoint: f32,
    pub roundness: f32,
    pub feather: f32,
}
impl Vignette {
    /// STU-COL-228 hard bounds.
    pub fn validate(&self) -> Result<(), GradeError> {
        check(self.amount.is_finite() && (-5.0..=5.0).contains(&self.amount), "vignette.amount")?;
        check(self.midpoint.is_finite() && (0.0..=100.0).contains(&self.midpoint), "vignette.midpoint")?;
        check(self.roundness.is_finite() && (-100.0..=100.0).contains(&self.roundness), "vignette.roundness")?;
        check(self.feather.is_finite() && (0.0..=100.0).contains(&self.feather), "vignette.feather")
    }
}

/// Multiplicative vignette gain at pixel centre `(x, y)` of a `width` x `height` frame (module docs).
pub fn vignette_gain(v: &Vignette, x: f64, y: f64, width: f64, height: f64) -> f32 {
    if width <= 0.0 || height <= 0.0 {
        return 1.0;
    }
    let r = f64::from(v.roundness) / 100.0;
    let short = width.min(height);
    let (hw, hh) = if r >= 0.0 {
        (0.5 * (width + (short - width) * r), 0.5 * (height + (short - height) * r))
    } else {
        (0.5 * width, 0.5 * height)
    };
    let p = if r >= 0.0 { 2.0 } else { 2.0 + 6.0 * -r };
    let u = ((x - 0.5 * width) / hw).abs();
    let w = ((y - 0.5 * height) / hh).abs();
    let d = (u.powf(p) + w.powf(p)).powf(1.0 / p);
    let m = f64::from(v.midpoint) / 100.0;
    let f = f64::from(v.feather) / 100.0;
    let (e0, e1) = (m * (1.0 - f), m * (1.0 + f));
    let t = if e1 <= e0 {
        if d >= e0 { 1.0 } else { 0.0 }
    } else {
        let s = ((d - e0) / (e1 - e0)).clamp(0.0, 1.0);
        s * s * (3.0 - 2.0 * s)
    };
    narrow((f64::from(v.amount) * t).exp2())
}

#[allow(clippy::cast_possible_truncation)]
fn narrow(v: f64) -> f32 {
    v as f32
}
