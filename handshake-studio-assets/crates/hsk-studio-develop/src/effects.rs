//! Effects and calibration (lane SMB-DEVELOP-LENS-PRESETS slice 3; STU-RAW-119 grain and post-crop
//! vignette, STU-RAW-120 calibration, STU-RAW-106 seeded determinism).
//!
//! Grain (whole frame, last stage, on the cropped image; every constant is this crate's choice,
//! Lightroom equivalence UNVERIFIED, D5):
//! * a coarse lattice of cell size `c = 1 + 3 size/100` pixels and a per-pixel fine field, both of
//!   Irwin-Hall(4) noise from a SplitMix64 hash of `(seed, x, y, layer)`; the coarse field is sampled
//!   bilinearly; `field = (1 - r) coarse + r fine` with `r = grain_frequency/100` (roughness);
//! * the field is normalised over the frame to mean 0 and standard deviation 1 exactly (two-pass,
//!   binary64), so the statistics are a function of the parameters only;
//! * each pixel's three channels get the same encoded-domain offset `A n m(b)`,
//!   `A = 0.12 amount/100`, midtone weight `m(b) = 0.25 + 0.75 * 4 b (1 - b)` of the clamped encoded
//!   luma `b` (monochrome grain). Same seed + same frame size = identical grain at every thread
//!   count; the seed is a recipe parameter.
//!
//! Post-crop vignette (on the cropped frame): superellipse distance `d` with half axes blended from
//! the frame's (roundness 0) to a circle of the short side (roundness +100) and exponent
//! `2 + 6 max(0, -roundness/100)` toward a rectangle; `t = smoothstep((d - a) / (b - a))` with
//! `b = 0.5 + midpoint/100`, `a = b - (0.05 + 0.9 feather/100)`; amount `k = amount/100`:
//! `highlight_priority` multiplies linear RGB by `2^(2 k t)` and, when darkening, keeps highlights by
//! `1 - highlight_contrast/100 * smoothstep((Y - 0.5) / 0.5)`; `colour_priority` scales the encoded
//! values by `1 + k t` toward black or white (`e + k t (1 - e)` for k > 0).
//!
//! Calibration (per pixel, before every other stage C step): the hue (+-30 degrees at +-100) and
//! saturation (distance from white x `1 + s/100`) of each working primary's xy chromaticity are moved
//! about the working white, the RGB -> XYZ matrix is rebuilt for the moved primaries with the same
//! white (SMPTE RP 177 construction) and pixels are re-expressed in the original working space; white
//! is a fixed point by construction. `shadow_tint` (positive = magenta) scales green by
//! `2^(-0.25 t w)` and red/blue by `2^(0.125 t w)`, `w = 1 - smoothstep(enc(Y) / 0.5)`.
use crate::DevelopError;
use crate::adjust::{COUNT_LIMIT, count, dec, enc, smoothstep};
use crate::color::{self, Mat3};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grain {
    /// 0..100.
    pub amount: f64,
    /// 0..100, default 25 (spec `derived`).
    pub size: f64,
    /// Roughness 0..100 (spec `grain_frequency`, default unknown; 50 here).
    pub frequency: f64,
    /// STU-RAW-106: the seed makes the render replayable.
    pub seed: u32,
}

impl Default for Grain {
    fn default() -> Self {
        Self { amount: 0.0, size: 25.0, frequency: 50.0, seed: 0 }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VignetteStyle {
    #[default]
    HighlightPriority,
    ColourPriority,
}

impl VignetteStyle {
    pub const fn key(self) -> &'static str {
        match self {
            Self::HighlightPriority => "highlight_priority",
            Self::ColourPriority => "colour_priority",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        [Self::HighlightPriority, Self::ColourPriority].into_iter().find(|s| s.key() == key)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PostCropVignette {
    pub amount: f64,
    pub midpoint: f64,
    pub roundness: f64,
    pub feather: f64,
    pub highlight_contrast: f64,
    pub style: VignetteStyle,
}

impl Default for PostCropVignette {
    fn default() -> Self {
        Self { amount: 0.0, midpoint: 50.0, roundness: 0.0, feather: 50.0, highlight_contrast: 0.0, style: VignetteStyle::HighlightPriority }
    }
}

/// STU-RAW-119 effects group (gated by `enable_effects`; whole frame).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Effects {
    pub grain: Grain,
    pub vignette: PostCropVignette,
    /// Suppresses a creative look's own vignette (recorded; the looks here carry none).
    pub override_look_vignette: bool,
}

/// STU-RAW-120 calibration group (gated by `enable_calibration`), each -100..100.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Calibration {
    pub shadow_tint: f64,
    pub red_hue: f64,
    pub red_saturation: f64,
    pub green_hue: f64,
    pub green_saturation: f64,
    pub blue_hue: f64,
    pub blue_saturation: f64,
}

fn within(v: f64, lo: f64, hi: f64) -> Result<f64, DevelopError> {
    if !v.is_finite() {
        return Err(DevelopError::NonFinite);
    }
    if !(lo..=hi).contains(&v) {
        return Err(DevelopError::InvalidAdjustment);
    }
    Ok(v)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PreparedEffects {
    grain: Option<(f64, f64, f64, u32)>,
    vignette: Option<PostCropVignette>,
}

impl PreparedEffects {
    pub(crate) fn prepare(e: &Effects, active: bool) -> Result<Self, DevelopError> {
        let g = &e.grain;
        let amount = within(g.amount, 0.0, 100.0)?;
        let size = within(g.size, 0.0, 100.0)?;
        let frequency = within(g.frequency, 0.0, 100.0)?;
        let v = &e.vignette;
        count(v.amount)?;
        within(v.midpoint, 0.0, 100.0)?;
        count(v.roundness)?;
        within(v.feather, 0.0, 100.0)?;
        within(v.highlight_contrast, 0.0, 100.0)?;
        if !active {
            return Ok(Self { grain: None, vignette: None });
        }
        Ok(Self {
            grain: (amount > 0.0).then_some((amount / 100.0, size / 100.0, frequency / 100.0, g.seed)),
            vignette: (v.amount != 0.0).then_some(*v),
        })
    }

    pub(crate) fn is_active(&self) -> bool {
        self.grain.is_some() || self.vignette.is_some()
    }

    /// Post-crop vignette, then grain, over the cropped linear frame.
    pub(crate) fn apply(&self, rgb: &mut [f32], width: u32, height: u32, luma: [f64; 3]) -> Result<(), DevelopError> {
        let (w, h) = (width as usize, height as usize);
        let n = w.checked_mul(h).ok_or(DevelopError::Overflow)?;
        if w == 0 || h == 0 || n.checked_mul(3) != Some(rgb.len()) {
            return Err(DevelopError::InvalidDimensions);
        }
        let mut out = rgb.to_vec();
        if let Some(v) = &self.vignette {
            vignette(&mut out, w, h, v, luma);
        }
        if let Some((amount, size, roughness, seed)) = self.grain {
            let field = grain_field(w, h, size, roughness, seed);
            let a = 0.12 * amount;
            for (px, n) in out.chunks_exact_mut(3).zip(&field) {
                let y = (luma[0] * f64::from(px[0]) + luma[1] * f64::from(px[1])) + luma[2] * f64::from(px[2]);
                let b = enc(y).clamp(0.0, 1.0);
                let offset = a * n * (0.25 + 0.75 * (4.0 * b * (1.0 - b)));
                for c in px.iter_mut() {
                    *c = dec(enc(f64::from(*c)) + offset) as f32;
                }
            }
        }
        if out.iter().any(|v| !v.is_finite()) {
            return Err(DevelopError::StageOverflow);
        }
        rgb.copy_from_slice(&out);
        Ok(())
    }
}

fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Irwin-Hall(4) noise with unit variance at lattice point `(x, y)` of `layer`.
fn noise(seed: u32, x: i64, y: i64, layer: u64) -> f64 {
    let key = u64::from(seed) ^ (x as u64).wrapping_mul(0x9e37_79b9) ^ (y as u64).wrapping_mul(0x85eb_ca6b_c2b2_ae35) ^ layer.wrapping_mul(0xc2b2_ae3d_27d4_eb4f);
    let mut h = splitmix(key);
    let mut sum = 0.0;
    for _ in 0..4 {
        h = splitmix(h);
        sum += (h >> 11) as f64 / (1u64 << 53) as f64;
    }
    (sum - 2.0) / (1.0f64 / 3.0).sqrt()
}

/// Zero-mean, unit-deviation grain field of a `w x h` frame (see the module documentation).
pub fn grain_field(w: usize, h: usize, size: f64, roughness: f64, seed: u32) -> Vec<f64> {
    let cell = 1.0 + 3.0 * size;
    let mut field = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let (u, v) = ((x as f64 + 0.5) / cell, (y as f64 + 0.5) / cell);
            let (i, j) = (u.floor(), v.floor());
            let (fx, fy) = (u - i, v - j);
            let (i, j) = (i as i64, j as i64);
            let top = noise(seed, i, j, 1) * (1.0 - fx) + noise(seed, i + 1, j, 1) * fx;
            let bottom = noise(seed, i, j + 1, 1) * (1.0 - fx) + noise(seed, i + 1, j + 1, 1) * fx;
            let coarse = top * (1.0 - fy) + bottom * fy;
            let fine = noise(seed, x as i64, y as i64, 2);
            field.push((1.0 - roughness) * coarse + roughness * fine);
        }
    }
    let n = field.len().max(1) as f64;
    let mean = field.iter().sum::<f64>() / n;
    let var = field.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n;
    let sd = var.sqrt();
    for v in &mut field {
        *v = if sd > 0.0 { (*v - mean) / sd } else { 0.0 };
    }
    field
}

fn vignette(rgb: &mut [f32], w: usize, h: usize, v: &PostCropVignette, luma: [f64; 3]) {
    let k = v.amount / COUNT_LIMIT;
    let round = v.roundness / COUNT_LIMIT;
    let (fw, fh) = (w as f64 / 2.0, h as f64 / 2.0);
    let short = fw.min(fh);
    let blend = round.max(0.0);
    let (ax, ay) = (fw + (short - fw) * blend, fh + (short - fh) * blend);
    let p = 2.0 + 6.0 * (-round).max(0.0);
    let b = 0.5 + v.midpoint / 100.0;
    let a = b - (0.05 + 0.9 * v.feather / 100.0);
    let hc = v.highlight_contrast / 100.0;
    for (i, px) in rgb.chunks_exact_mut(3).enumerate() {
        let (x, y) = ((i % w) as f64 + 0.5 - fw, (i / w) as f64 + 0.5 - fh);
        let d = ((x / ax).abs().powf(p) + (y / ay).abs().powf(p)).powf(1.0 / p);
        let t = smoothstep((d - a) / (b - a));
        if t == 0.0 {
            continue;
        }
        match v.style {
            VignetteStyle::HighlightPriority => {
                let mut gain = (2.0 * k * t).exp2();
                if k < 0.0 && hc > 0.0 {
                    let yl = (luma[0] * f64::from(px[0]) + luma[1] * f64::from(px[1])) + luma[2] * f64::from(px[2]);
                    let keep = hc * smoothstep((yl - 0.5) / 0.5);
                    gain = gain + (1.0 - gain) * keep;
                }
                for c in px.iter_mut() {
                    *c = (f64::from(*c) * gain) as f32;
                }
            }
            VignetteStyle::ColourPriority => {
                for c in px.iter_mut() {
                    let e = enc(f64::from(*c));
                    let e2 = if k < 0.0 { e * (1.0 + k * t) } else { e + k * t * (1.0 - e) };
                    *c = dec(e2) as f32;
                }
            }
        }
    }
}

/// Validated calibration: a working-space matrix and a shadow tint amount.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PreparedCalibration {
    matrix: Option<Mat3>,
    shadow_tint: f64,
    luma: [f64; 3],
}

fn xy(v: [f64; 3]) -> Option<[f64; 2]> {
    let s = v[0] + v[1] + v[2];
    (s.is_finite() && s.abs() > 1e-300).then(|| [v[0] / s, v[1] / s])
}

/// Working -> working matrix for moved primaries (white preserved).
pub(crate) fn calibration_matrix(c: &Calibration, working_to_xyz: &Mat3) -> Result<Option<Mat3>, DevelopError> {
    let hue = [count(c.red_hue)?, count(c.green_hue)?, count(c.blue_hue)?];
    let sat = [count(c.red_saturation)?, count(c.green_saturation)?, count(c.blue_saturation)?];
    if hue.iter().chain(&sat).all(|v| *v == 0.0) {
        return Ok(None);
    }
    let m = working_to_xyz;
    let col = |i: usize| [m[i], m[3 + i], m[6 + i]];
    let white = [m[0] + m[1] + m[2], m[3] + m[4] + m[5], m[6] + m[7] + m[8]];
    let wxy = xy(white).ok_or(DevelopError::InvalidAdjustment)?;
    let mut moved = [[0.0f64; 2]; 3];
    for i in 0..3 {
        let p = xy(col(i)).ok_or(DevelopError::InvalidAdjustment)?;
        let (dx, dy) = (p[0] - wxy[0], p[1] - wxy[1]);
        let (s, co) = (hue[i] / COUNT_LIMIT * 30.0).to_radians().sin_cos();
        let scale = 1.0 + sat[i] / COUNT_LIMIT;
        moved[i] = [wxy[0] + (dx * co - dy * s) * scale, wxy[1] + (dx * s + dy * co) * scale];
    }
    // RP 177: primaries matrix P (columns = xyz of each primary, z = 1 - x - y, scaled to y = 1),
    // then column scales S = P^-1 W so that P S maps (1, 1, 1) to the white.
    let mut p = [0.0f64; 9];
    for (i, q) in moved.iter().enumerate() {
        if q[1].abs() < 1e-12 {
            return Err(DevelopError::InvalidAdjustment);
        }
        p[i] = q[0] / q[1];
        p[3 + i] = 1.0;
        p[6 + i] = (1.0 - q[0] - q[1]) / q[1];
    }
    let pinv = color::invert(&p).map_err(|_| DevelopError::InvalidAdjustment)?;
    let s = color::mul_vec(&pinv, &white);
    let mut moved_to_xyz = p;
    for r in 0..3 {
        for (cidx, sc) in s.iter().enumerate() {
            moved_to_xyz[r * 3 + cidx] *= sc;
        }
    }
    let to_working = color::invert(working_to_xyz).map_err(|_| DevelopError::InvalidAdjustment)?;
    let out = color::mul(&to_working, &moved_to_xyz);
    if out.iter().any(|v| !v.is_finite()) {
        return Err(DevelopError::InvalidAdjustment);
    }
    Ok(Some(out))
}

impl PreparedCalibration {
    pub(crate) fn prepare(c: &Calibration, working_to_xyz: &Mat3, luma: [f64; 3], active: bool) -> Result<Option<Self>, DevelopError> {
        let matrix = calibration_matrix(c, working_to_xyz)?;
        let shadow_tint = count(c.shadow_tint)? / COUNT_LIMIT;
        if !active || (matrix.is_none() && shadow_tint == 0.0) {
            return Ok(None);
        }
        Ok(Some(Self { matrix, shadow_tint, luma }))
    }

    pub(crate) fn apply(&self, v: [f64; 3]) -> [f64; 3] {
        let mut v = match &self.matrix {
            Some(m) => color::mul_vec(m, &v),
            None => v,
        };
        if self.shadow_tint != 0.0 {
            let y = (self.luma[0] * v[0] + self.luma[1] * v[1]) + self.luma[2] * v[2];
            let w = 1.0 - smoothstep(enc(y) / 0.5);
            let t = self.shadow_tint * w;
            let (g, rb) = ((-0.25 * t).exp2(), (0.125 * t).exp2());
            v = [v[0] * rb, v[1] * g, v[2] * rb];
        }
        v
    }
}
