//! Lane SMB-DEVELOP-CONTROLS-2, whole-frame detail stages (gated by `enable_detail`): texture,
//! dehaze, sharpening and luminance / colour noise reduction. Spec basis: STU-RAW-111 (`texture`,
//! `dehaze`, soft range -100..100, sparse default) and STU-RAW-114 (`sharpen_amount` 0..150,
//! `sharpen_radius` 0.5..3.0, `sharpen_detail` 0..100, `sharpen_edge_masking` 0..100,
//! `luminance`/`colour_noise_reduction` 0..100).
//!
//! DIVERGENCE FROM THE SPEC DEFAULTS (named, not hidden): the spec derives `sharpen_amount` 40,
//! `sharpen_radius` 1.0, `sharpen_detail` 25 and `colour_noise_reduction` 25 as defaults. Here every
//! default is the identity (amount 0, colour NR 0) so that switching `enable_detail` on never
//! changes an image by itself; [`Sharpen::spec_defaults`] and [`NoiseReduction::spec_defaults`] give
//! the spec values for a preset. All maths are this crate's own choices and UNVERIFIED against
//! Lightroom (oracle session D5).
//!
//! Methods (all scalar binary64, serial, deterministic, working on the sRGB-encoded value `enc(c)`
//! added back to every channel, so colour ratios stay close):
//! * texture: fine-detail layer `L - G(L)` with a Gaussian of sigma `clamp(0.002 * short side, 1, 8)`
//!   pixels, offset `0.6 * amount/100 * detail` (negative amounts smooth);
//! * dehaze: dark channel prior (He, Sun, Tang 2009): per-pixel channel minimum, minimum filter of
//!   radius `max(2, round(0.008 * short side))`, atmospheric light = the brightest pixel among the
//!   top 0.1 % of the dark channel (floored at 0.05 per channel), transmission
//!   `1 - 0.95 * amount/100 * darkchannel(I / A)` smoothed by a 3-pass box blur of the same radius
//!   (NOT the soft-matting or guided-filter refinement of the paper), floor 0.1, radiance
//!   `(I - A) / t + A`. Negative amounts add haze with a uniform transmission `1 - 0.5 * |amount|/100`;
//! * sharpening: unsharp mask on encoded luma, `G` Gaussian of sigma = radius; the detail slider
//!   limits halos by compressing large differences `d / (1 + (1 - detail/100) * 8 |d|)`; edge masking
//!   weights by the smoothstep of the blurred-luma gradient magnitude over `masking/100 * 0.08`;
//! * noise reduction: a separable (horizontal then vertical) bilateral filter guided by encoded luma.
//!   Luminance: sigma_s `0.8 + 1.7 l`, sigma_r `0.01 + 0.06 l`, filtered luma replaces luma. Colour:
//!   the three channel-minus-luma planes are filtered with sigma_s `1 + 4 c`, sigma_r `0.05 + 0.1 c`
//!   and added back to the (filtered) luma. The separable form is an approximation of the full
//!   bilateral filter.
use crate::DevelopError;
use crate::adjust::{box_blur_3, dec, enc, smoothstep};

pub const TEXTURE_GAIN: f64 = 0.6;
pub const SHARPEN_GAIN: f64 = 2.0;
pub const DEHAZE_OMEGA: f64 = 0.95;
pub const DEHAZE_MIN_TRANSMISSION: f64 = 0.1;
pub const DEHAZE_MIN_ATMOSPHERE: f64 = 0.05;
pub const DEHAZE_ADDED_HAZE: f64 = 0.5;
pub const SHARPEN_HALO_COMPRESSION: f64 = 8.0;
pub const SHARPEN_MASK_THRESHOLD: f64 = 0.08;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sharpen {
    /// 0..150 (spec default 40; identity default 0).
    pub amount: f64,
    /// 0.5..3.0 pixels (spec default 1.0).
    pub radius: f64,
    /// 0..100 (spec default 25).
    pub detail: f64,
    /// 0..100 (spec default 0).
    pub masking: f64,
}

impl Default for Sharpen {
    fn default() -> Self {
        Self { amount: 0.0, radius: 1.0, detail: 25.0, masking: 0.0 }
    }
}

impl Sharpen {
    pub const fn spec_defaults() -> Self {
        Self { amount: 40.0, radius: 1.0, detail: 25.0, masking: 0.0 }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NoiseReduction {
    /// 0..100.
    pub luminance: f64,
    /// 0..100 (spec default 25; identity default 0).
    pub colour: f64,
}

impl NoiseReduction {
    pub const fn spec_defaults() -> Self {
        Self { luminance: 0.0, colour: 25.0 }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Detail {
    /// -100..100.
    pub texture: f64,
    /// -100..100.
    pub dehaze: f64,
    pub sharpen: Sharpen,
    pub noise: NoiseReduction,
}

fn bounded(value: f64, lo: f64, hi: f64) -> Result<f64, DevelopError> {
    if !value.is_finite() {
        return Err(DevelopError::NonFinite);
    }
    if !(lo..=hi).contains(&value) {
        return Err(DevelopError::InvalidAdjustment);
    }
    Ok(value)
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct PreparedDetail {
    pub(crate) texture: f64,
    pub(crate) dehaze: f64,
    pub(crate) sharpen: Option<Sharpen>,
    pub(crate) noise_luminance: f64,
    pub(crate) noise_colour: f64,
}

impl PreparedDetail {
    /// Validates every field (even while `enable_detail` is off) and keeps the active ones.
    pub(crate) fn prepare(detail: &Detail, on: bool) -> Result<Self, DevelopError> {
        let texture = bounded(detail.texture, -100.0, 100.0)? / 100.0;
        let dehaze = bounded(detail.dehaze, -100.0, 100.0)? / 100.0;
        let s = &detail.sharpen;
        bounded(s.amount, 0.0, 150.0)?;
        bounded(s.radius, 0.5, 3.0)?;
        bounded(s.detail, 0.0, 100.0)?;
        bounded(s.masking, 0.0, 100.0)?;
        let luminance = bounded(detail.noise.luminance, 0.0, 100.0)? / 100.0;
        let colour = bounded(detail.noise.colour, 0.0, 100.0)? / 100.0;
        if !on {
            return Ok(Self::default());
        }
        Ok(Self {
            texture,
            dehaze,
            sharpen: (s.amount != 0.0).then_some(*s),
            noise_luminance: luminance,
            noise_colour: colour,
        })
    }

    /// Name of the first active whole-frame detail stage, if any.
    pub(crate) fn first_active(&self) -> Option<&'static str> {
        if self.noise_luminance != 0.0 || self.noise_colour != 0.0 {
            Some("noise_reduction_whole_frame")
        } else if self.dehaze != 0.0 {
            Some("dehaze_whole_frame")
        } else if self.texture != 0.0 {
            Some("texture_whole_frame")
        } else if self.sharpen.is_some() {
            Some("sharpening_whole_frame")
        } else {
            None
        }
    }

    /// Run the active stages in order: noise reduction, dehaze, texture, (clarity is the caller's),
    /// sharpening. `clarity_between` runs after texture and before sharpening.
    pub(crate) fn apply(
        &self,
        rgb: &mut [f32],
        width: u32,
        height: u32,
        luma: [f64; 3],
        clarity_between: &mut dyn FnMut(&mut [f32]) -> Result<(), DevelopError>,
    ) -> Result<(), DevelopError> {
        if self.noise_luminance != 0.0 || self.noise_colour != 0.0 {
            reduce_noise(rgb, width, height, &NoiseReduction { luminance: self.noise_luminance * 100.0, colour: self.noise_colour * 100.0 }, luma)?;
        }
        if self.dehaze != 0.0 {
            dehaze(rgb, width, height, self.dehaze * 100.0)?;
        }
        if self.texture != 0.0 {
            texture(rgb, width, height, self.texture * 100.0, luma)?;
        }
        clarity_between(rgb)?;
        if let Some(s) = &self.sharpen {
            sharpen(rgb, width, height, s, luma)?;
        }
        Ok(())
    }
}

/// Dimension, length and finiteness checks shared by every stage; returns `(w, h)`.
fn frame(rgb: &[f32], width: u32, height: u32, luma: Option<&[f64; 3]>) -> Result<(usize, usize), DevelopError> {
    let (w, h) = (width as usize, height as usize);
    let pixels = w.checked_mul(h).ok_or(DevelopError::Overflow)?;
    if w == 0 || h == 0 || pixels.checked_mul(3) != Some(rgb.len()) {
        return Err(DevelopError::InvalidDimensions);
    }
    if luma.is_some_and(|l| l.iter().any(|v| !v.is_finite() || *v < 0.0)) {
        return Err(DevelopError::InvalidAdjustment);
    }
    if rgb.iter().any(|v| !v.is_finite()) {
        return Err(DevelopError::NonFinite);
    }
    Ok((w, h))
}

fn encoded_luma(rgb: &[f32], luma: &[f64; 3]) -> Vec<f32> {
    let sum = luma[0] + luma[1] + luma[2];
    let k = [luma[0] / sum, luma[1] / sum, luma[2] / sum];
    rgb.chunks_exact(3)
        .map(|p| {
            let e = [enc(f64::from(p[0])), enc(f64::from(p[1])), enc(f64::from(p[2]))];
            ((k[0] * e[0] + k[1] * e[1]) + k[2] * e[2]) as f32
        })
        .collect()
}

/// Separable Gaussian blur with edge replication (kernel truncated at 3 sigma).
pub(crate) fn gaussian(plane: &[f32], w: usize, h: usize, sigma: f64) -> Vec<f32> {
    let radius = (3.0 * sigma).ceil().max(1.0) as usize;
    let mut kernel: Vec<f64> = (0..=2 * radius)
        .map(|i| {
            let d = i as f64 - radius as f64;
            (-(d * d) / (2.0 * sigma * sigma)).exp()
        })
        .collect();
    let total: f64 = kernel.iter().sum();
    for k in &mut kernel {
        *k /= total;
    }
    let mut tmp = vec![0.0f32; plane.len()];
    for (src, dst) in plane.chunks_exact(w).zip(tmp.chunks_exact_mut(w)) {
        for (x, out) in dst.iter_mut().enumerate() {
            let mut acc = 0.0f64;
            for (i, k) in kernel.iter().enumerate() {
                let xi = (x as isize + i as isize - radius as isize).clamp(0, w as isize - 1) as usize;
                acc += k * f64::from(src[xi]);
            }
            *out = acc as f32;
        }
    }
    let mut out = vec![0.0f32; plane.len()];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0f64;
            for (i, k) in kernel.iter().enumerate() {
                let yi = (y as isize + i as isize - radius as isize).clamp(0, h as isize - 1) as usize;
                acc += k * f64::from(tmp[yi * w + x]);
            }
            out[y * w + x] = acc as f32;
        }
    }
    out
}

fn add_offset(rgb: &mut [f32], offsets: impl Fn(usize) -> f64) -> Result<(), DevelopError> {
    let mut out = rgb.to_vec();
    for (i, px) in out.chunks_exact_mut(3).enumerate() {
        let offset = offsets(i);
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

/// Texture: fine-detail contrast, `amount` in -100..100.
pub fn texture(rgb: &mut [f32], width: u32, height: u32, amount: f64, luma: [f64; 3]) -> Result<(), DevelopError> {
    let gain = bounded(amount, -100.0, 100.0)? / 100.0;
    let (w, h) = frame(rgb, width, height, Some(&luma))?;
    if gain == 0.0 {
        return Ok(());
    }
    let plane = encoded_luma(rgb, &luma);
    let sigma = (w.min(h) as f64 * 0.002).clamp(1.0, 8.0);
    let base = gaussian(&plane, w, h, sigma);
    add_offset(rgb, |i| (gain * TEXTURE_GAIN) * (f64::from(plane[i]) - f64::from(base[i])))
}

/// Sharpening (unsharp mask with halo limit and edge masking).
pub fn sharpen(rgb: &mut [f32], width: u32, height: u32, s: &Sharpen, luma: [f64; 3]) -> Result<(), DevelopError> {
    let amount = bounded(s.amount, 0.0, 150.0)? / 100.0;
    bounded(s.radius, 0.5, 3.0)?;
    let detail = bounded(s.detail, 0.0, 100.0)? / 100.0;
    let masking = bounded(s.masking, 0.0, 100.0)? / 100.0;
    let (w, h) = frame(rgb, width, height, Some(&luma))?;
    if amount == 0.0 {
        return Ok(());
    }
    let plane = encoded_luma(rgb, &luma);
    let base = gaussian(&plane, w, h, s.radius);
    let threshold = masking * SHARPEN_MASK_THRESHOLD;
    let weight = |i: usize| -> f64 {
        if threshold == 0.0 {
            return 1.0;
        }
        let (x, y) = (i % w, i / w);
        let at = |xx: usize, yy: usize| f64::from(base[yy * w + xx]);
        let gx = at((x + 1).min(w - 1), y) - at(x.saturating_sub(1), y);
        let gy = at(x, (y + 1).min(h - 1)) - at(x, y.saturating_sub(1));
        smoothstep(gx.hypot(gy) / 2.0 / threshold)
    };
    let compress = (1.0 - detail) * SHARPEN_HALO_COMPRESSION;
    add_offset(rgb, |i| {
        let d = f64::from(plane[i]) - f64::from(base[i]);
        let limited = d / (1.0 + compress * d.abs());
        (amount * SHARPEN_GAIN) * limited * weight(i)
    })
}

fn min_filter(plane: &[f32], w: usize, h: usize, radius: usize) -> Vec<f32> {
    let mut tmp = vec![0.0f32; plane.len()];
    for (src, dst) in plane.chunks_exact(w).zip(tmp.chunks_exact_mut(w)) {
        for (x, out) in dst.iter_mut().enumerate() {
            let lo = x.saturating_sub(radius);
            let hi = (x + radius).min(w - 1);
            *out = src[lo..=hi].iter().copied().fold(f32::INFINITY, f32::min);
        }
    }
    let mut out = vec![0.0f32; plane.len()];
    for y in 0..h {
        let (lo, hi) = (y.saturating_sub(radius), (y + radius).min(h - 1));
        for x in 0..w {
            out[y * w + x] = (lo..=hi).map(|yy| tmp[yy * w + x]).fold(f32::INFINITY, f32::min);
        }
    }
    out
}

/// Dehaze (dark channel prior, see the module documentation), `amount` in -100..100.
pub fn dehaze(rgb: &mut [f32], width: u32, height: u32, amount: f64) -> Result<(), DevelopError> {
    let strength = bounded(amount, -100.0, 100.0)? / 100.0;
    let (w, h) = frame(rgb, width, height, None)?;
    if strength == 0.0 {
        return Ok(());
    }
    let encoded: Vec<f32> = rgb.iter().map(|c| enc(f64::from(*c)) as f32).collect();
    let radius = ((w.min(h) as f64 * 0.008).round() as usize).max(2);
    let channel_min: Vec<f32> = encoded.chunks_exact(3).map(|p| p[0].min(p[1]).min(p[2]).clamp(0.0, 1.0)).collect();
    let dark = min_filter(&channel_min, w, h, radius);
    // Atmospheric light: brightest pixel among the top 0.1 % of the dark channel.
    let mut order: Vec<usize> = (0..dark.len()).collect();
    let top = (dark.len() / 1000).max(1);
    let split = dark.len() - top;
    order.select_nth_unstable_by(split, |a, b| dark[*a].total_cmp(&dark[*b]).then(a.cmp(b)));
    let brightest = order[split..]
        .iter()
        .copied()
        .max_by(|a, b| {
            let sum = |i: usize| encoded[i * 3] + encoded[i * 3 + 1] + encoded[i * 3 + 2];
            sum(*a).total_cmp(&sum(*b)).then(b.cmp(a))
        })
        .ok_or(DevelopError::InvalidDimensions)?;
    let atmosphere: [f64; 3] = std::array::from_fn(|c| f64::from(encoded[brightest * 3 + c]).clamp(DEHAZE_MIN_ATMOSPHERE, 1.0));
    let transmission: Vec<f32> = if strength > 0.0 {
        let normalised: Vec<f32> = encoded
            .chunks_exact(3)
            .map(|p| {
                (0..3).map(|c| (f64::from(p[c]).clamp(0.0, 1.0) / atmosphere[c]) as f32).fold(f32::INFINITY, f32::min)
            })
            .collect();
        let dark_n = min_filter(&normalised, w, h, radius);
        let mut t: Vec<f32> = dark_n.iter().map(|d| (1.0 - DEHAZE_OMEGA * strength * f64::from(*d)) as f32).collect();
        box_blur_3(&mut t, w, h, radius);
        t
    } else {
        vec![(1.0 - DEHAZE_ADDED_HAZE * -strength) as f32; w * h]
    };
    let mut out = rgb.to_vec();
    for ((px, e), t) in out.chunks_exact_mut(3).zip(encoded.chunks_exact(3)).zip(&transmission) {
        for c in 0..3 {
            let (ev, a) = (f64::from(e[c]), atmosphere[c]);
            let j = if strength > 0.0 {
                (ev - a) / f64::from(*t).max(DEHAZE_MIN_TRANSMISSION) + a
            } else {
                ev * f64::from(*t) + a * (1.0 - f64::from(*t))
            };
            let v = dec(j) as f32;
            if !v.is_finite() {
                return Err(DevelopError::StageOverflow);
            }
            px[c] = v;
        }
    }
    rgb.copy_from_slice(&out);
    Ok(())
}

/// Separable bilateral filter of `plane`, range weights from `guide` (same size).
fn bilateral(plane: &[f32], guide: &[f32], w: usize, h: usize, sigma_s: f64, sigma_r: f64) -> Vec<f32> {
    let radius = (2.0 * sigma_s).ceil().max(1.0) as usize;
    let spatial: Vec<f64> = (0..=2 * radius)
        .map(|i| {
            let d = i as f64 - radius as f64;
            (-(d * d) / (2.0 * sigma_s * sigma_s)).exp()
        })
        .collect();
    let inv = 1.0 / (2.0 * sigma_r * sigma_r);
    let pass = |src: &[f32], guide: &[f32], horizontal: bool| -> Vec<f32> {
        let mut out = vec![0.0f32; src.len()];
        for y in 0..h {
            for x in 0..w {
                let centre = f64::from(guide[y * w + x]);
                let (mut acc, mut norm) = (0.0f64, 0.0f64);
                for (i, s) in spatial.iter().enumerate() {
                    let o = i as isize - radius as isize;
                    let (xx, yy) = if horizontal {
                        ((x as isize + o).clamp(0, w as isize - 1) as usize, y)
                    } else {
                        (x, (y as isize + o).clamp(0, h as isize - 1) as usize)
                    };
                    let g = f64::from(guide[yy * w + xx]) - centre;
                    let weight = s * (-(g * g) * inv).exp();
                    acc += weight * f64::from(src[yy * w + xx]);
                    norm += weight;
                }
                out[y * w + x] = (acc / norm) as f32;
            }
        }
        out
    };
    let horizontal = pass(plane, guide, true);
    pass(&horizontal, guide, false)
}

/// Luminance and colour noise reduction (separable bilateral approximation).
pub fn reduce_noise(rgb: &mut [f32], width: u32, height: u32, nr: &NoiseReduction, luma: [f64; 3]) -> Result<(), DevelopError> {
    let l = bounded(nr.luminance, 0.0, 100.0)? / 100.0;
    let c = bounded(nr.colour, 0.0, 100.0)? / 100.0;
    let (w, h) = frame(rgb, width, height, Some(&luma))?;
    if l == 0.0 && c == 0.0 {
        return Ok(());
    }
    let encoded: Vec<f32> = rgb.iter().map(|v| enc(f64::from(*v)) as f32).collect();
    let plane = encoded_luma(rgb, &luma);
    let filtered = if l != 0.0 { bilateral(&plane, &plane, w, h, 0.8 + 1.7 * l, 0.01 + 0.06 * l) } else { plane.clone() };
    let chroma: [Vec<f32>; 3] = std::array::from_fn(|ch| {
        let diff: Vec<f32> = encoded.chunks_exact(3).zip(&plane).map(|(p, y)| p[ch] - y).collect();
        if c != 0.0 { bilateral(&diff, &filtered, w, h, 1.0 + 4.0 * c, 0.05 + 0.1 * c) } else { diff }
    });
    let mut out = rgb.to_vec();
    for (i, px) in out.chunks_exact_mut(3).enumerate() {
        for (ch, slot) in px.iter_mut().enumerate() {
            let v = dec(f64::from(filtered[i]) + f64::from(chroma[ch][i])) as f32;
            if !v.is_finite() {
                return Err(DevelopError::StageOverflow);
            }
            *slot = v;
        }
    }
    rgb.copy_from_slice(&out);
    Ok(())
}
