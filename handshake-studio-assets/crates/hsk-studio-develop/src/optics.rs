//! Lens corrections (lane SMB-DEVELOP-LENS-PRESETS slice 1; STU-RAW-117 optics parameter contract,
//! STU-RAW-127 lens profile model): profile distortion, profile vignetting and the three lateral
//! chromatic-aberration models, manual distortion and vignetting, and purple/green defringe.
//!
//! Studio lens model v1 (this crate's own documented convention; equivalence with any vendor model,
//! including the DNG `WarpRectilinear` / `FixVignetteRadial` opcodes it resembles, is UNVERIFIED):
//! the model maps a CORRECTED output position to the CAPTURED position it is sampled from, so no
//! inversion is needed. For output pixel centre `(x, y)` of a `w x h` frame, `L = max(w, h)`,
//! `C = (cx w, cy h)`:
//! * `u = s (x - Cx) / (fx L)`, `v = s (y - Cy) / (fy L)`, `r^2 = u^2 + v^2` (`s` = the model scale);
//! * `R = 1 + k1 r^2 + k2 r^4 + k3 r^6`; `ud = u R + 2 p1 u v + p2 (r^2 + 2 u^2)`,
//!   `vd = v R + p1 (r^2 + 2 v^2) + 2 p2 u v`;
//! * green (and the reference of red / blue) samples at `C + (ud fx L, vd fy L) * G(rd)` with
//!   `G = ca_green` (identity when absent) and `rd^2 = ud^2 + vd^2`; red and blue sample at the
//!   green displacement times `ca_red_green` / `ca_blue_green` of the green radius (only while
//!   `auto_lateral_chromatic_aberration` is on);
//! * `lens_profile_distortion_scale` d (0..200) blends the displacement: `x + d/100 (P(x) - x)`;
//! * vignetting: falloff `V = 1 + a1 rg^2 + a2 rg^4 + a3 rg^6` at the green captured radius, gain
//!   `V^(-v/100)` for `lens_profile_vignetting_scale` v; `V <= 0` anywhere in the frame refuses
//!   `invalid_lens_profile`;
//! * manual distortion m (-100..100): the output position is first scaled about the frame centre by
//!   `1 - 0.15 (m/100) q^2`, `q` = distance / half diagonal (positive corrects barrel); manual
//!   vignette a (-100..100): gain `2^(a/100 q^2)` (positive brightens the corners). Both UNVERIFIED.
//!
//! Defringe (whole frame, after the warp, on linear working RGB): a pixel's chroma is pulled toward
//! its luma by `amount/20 * edge * hue_window`, `edge = smoothstep(g / 0.08)` of the largest encoded
//! luma gradient within 2 pixels, hue windows in HSV degrees `200 + 1.6 count` (purple, defaults
//! 30/70 -> 248..312) and `40 + 1.6 count` (green, 40/60 -> 104..136), 10-degree soft edges. The
//! mapping from Lightroom's 0..100 hue counts to degrees is UNVERIFIED (D5).
use crate::DevelopError;
use crate::adjust::{COUNT_LIMIT, count, enc, rgb_to_hsv, smoothstep};
use crate::lens_profile::{ChannelModel, LensBinding};

/// STU-RAW-117: defringe amounts 0..20, hue windows 0..100 with the captured non-zero defaults.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Defringe {
    pub purple_amount: f64,
    pub purple_hue_low: f64,
    pub purple_hue_high: f64,
    pub green_amount: f64,
    pub green_hue_low: f64,
    pub green_hue_high: f64,
}

impl Default for Defringe {
    fn default() -> Self {
        Self { purple_amount: 0.0, purple_hue_low: 30.0, purple_hue_high: 70.0, green_amount: 0.0, green_hue_low: 40.0, green_hue_high: 60.0 }
    }
}

/// STU-RAW-117 optics group. Profile correction and lateral CA removal default ON (spec `derived`);
/// with no bound profile they do nothing, so the default is the identity.
#[derive(Clone, Debug, PartialEq)]
pub struct Optics {
    pub lens_profile_enable: bool,
    /// The profile model resolved for this shot (name, digest, grid values, model).
    pub profile: Option<LensBinding>,
    /// 0..200, default 100.
    pub lens_profile_distortion_scale: f64,
    /// 0..200, default 100.
    pub lens_profile_vignetting_scale: f64,
    pub auto_lateral_chromatic_aberration: bool,
    /// -100..100.
    pub manual_distortion_amount: f64,
    /// -100..100.
    pub manual_vignette_amount: f64,
    pub defringe: Defringe,
}

impl Default for Optics {
    fn default() -> Self {
        Self {
            lens_profile_enable: true,
            profile: None,
            lens_profile_distortion_scale: 100.0,
            lens_profile_vignetting_scale: 100.0,
            auto_lateral_chromatic_aberration: true,
            manual_distortion_amount: 0.0,
            manual_vignette_amount: 0.0,
            defringe: Defringe::default(),
        }
    }
}

const MANUAL_DISTORTION_K: f64 = 0.15;

fn scale_0_200(v: f64) -> Result<f64, DevelopError> {
    if !v.is_finite() {
        return Err(DevelopError::NonFinite);
    }
    if !(0.0..=200.0).contains(&v) {
        return Err(DevelopError::InvalidAdjustment);
    }
    Ok(v / 100.0)
}

fn range(v: f64, lo: f64, hi: f64) -> Result<f64, DevelopError> {
    if !v.is_finite() {
        return Err(DevelopError::NonFinite);
    }
    if !(lo..=hi).contains(&v) {
        return Err(DevelopError::InvalidAdjustment);
    }
    Ok(v)
}

/// Validated optics numbers; `None` parts are inactive.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreparedOptics {
    pub(crate) model: Option<PreparedModel>,
    pub(crate) manual_k: f64,
    pub(crate) manual_vignette: f64,
    pub(crate) defringe: Option<PreparedDefringe>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreparedModel {
    binding: LensBinding,
    distortion: f64,
    vignetting: f64,
    ca: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PreparedDefringe {
    purple: (f64, f64, f64),
    green: (f64, f64, f64),
}

fn check_channel(c: &Option<ChannelModel>) -> Result<(), DevelopError> {
    if let Some(m) = c
        && (!m.scale.is_finite() || m.scale <= 0.0 || m.radial.iter().any(|v| !v.is_finite()))
    {
        return Err(DevelopError::InvalidLensProfile);
    }
    Ok(())
}

impl PreparedOptics {
    /// Validates every field (even while the group is off); `active` = `enable_lens_corrections`.
    pub(crate) fn prepare(o: &Optics, active: bool) -> Result<Self, DevelopError> {
        let distortion = scale_0_200(o.lens_profile_distortion_scale)?;
        let vignetting = scale_0_200(o.lens_profile_vignetting_scale)?;
        let manual_d = count(o.manual_distortion_amount)? / COUNT_LIMIT;
        let manual_v = count(o.manual_vignette_amount)? / COUNT_LIMIT;
        let d = &o.defringe;
        let pa = range(d.purple_amount, 0.0, 20.0)?;
        let ga = range(d.green_amount, 0.0, 20.0)?;
        let (pl, ph) = (range(d.purple_hue_low, 0.0, 100.0)?, range(d.purple_hue_high, 0.0, 100.0)?);
        let (gl, gh) = (range(d.green_hue_low, 0.0, 100.0)?, range(d.green_hue_high, 0.0, 100.0)?);
        if pl >= ph || gl >= gh {
            return Err(DevelopError::InvalidAdjustment);
        }
        if let Some(b) = &o.profile {
            let p = &b.model.perspective;
            let all = [p.focal_x, p.focal_y, p.centre_x, p.centre_y, p.scale]
                .into_iter()
                .chain(p.radial)
                .chain(p.tangential)
                .chain(b.model.vignette.into_iter().flatten());
            if all.into_iter().any(|v| !v.is_finite())
                || p.focal_x <= 0.0
                || p.focal_y <= 0.0
                || p.scale <= 0.0
                || !(0.0..=1.0).contains(&p.centre_x)
                || !(0.0..=1.0).contains(&p.centre_y)
            {
                return Err(DevelopError::InvalidLensProfile);
            }
            check_channel(&b.model.ca_green)?;
            check_channel(&b.model.ca_red_green)?;
            check_channel(&b.model.ca_blue_green)?;
        }
        if !active {
            return Ok(Self { model: None, manual_k: 0.0, manual_vignette: 0.0, defringe: None });
        }
        let model = match (&o.profile, o.lens_profile_enable) {
            (Some(binding), true) => Some(PreparedModel {
                binding: binding.clone(),
                distortion,
                vignetting,
                ca: o.auto_lateral_chromatic_aberration,
            }),
            _ => None,
        };
        let defringe = (pa > 0.0 || ga > 0.0).then_some(PreparedDefringe {
            purple: (pa / 20.0, 200.0 + 1.6 * pl, 200.0 + 1.6 * ph),
            green: (ga / 20.0, 40.0 + 1.6 * gl, 40.0 + 1.6 * gh),
        });
        Ok(Self { model, manual_k: -MANUAL_DISTORTION_K * manual_d, manual_vignette: manual_v, defringe })
    }

    /// True when the warp (any geometric or gain change) is needed.
    pub(crate) fn warps(&self) -> bool {
        self.model.is_some() || self.manual_k != 0.0 || self.manual_vignette != 0.0
    }

    pub(crate) fn is_active(&self) -> bool {
        self.warps() || self.defringe.is_some()
    }
}

/// Per-frame evaluation of the optics map for a `width x height` captured frame.
pub(crate) struct LensMap<'a> {
    optics: &'a PreparedOptics,
    centre: (f64, f64),
    half_diagonal: f64,
    long: f64,
    size: (f64, f64),
}

/// Captured sample positions (red, green, blue) and the gain of one output position.
pub(crate) struct LensSample3 {
    pub(crate) points: [(f64, f64); 3],
    pub(crate) gain: f64,
}

fn radial_poly(k: &[f64; 3], r2: f64) -> f64 {
    let r4 = r2 * r2;
    ((1.0 + k[0] * r2) + k[1] * r4) + k[2] * (r4 * r2)
}

fn channel_factor(c: &Option<ChannelModel>, r2: f64) -> f64 {
    match c {
        Some(m) => m.scale * radial_poly(&m.radial, r2),
        None => 1.0,
    }
}

impl<'a> LensMap<'a> {
    pub(crate) fn new(optics: &'a PreparedOptics, width: u32, height: u32) -> Self {
        let (w, h) = (f64::from(width), f64::from(height));
        Self { optics, centre: (w / 2.0, h / 2.0), half_diagonal: w.hypot(h) / 2.0, long: w.max(h), size: (w, h) }
    }

    /// Where output position `(x, y)` (continuous pixel coordinates) samples the captured frame.
    pub(crate) fn sample(&self, x: f64, y: f64) -> Result<LensSample3, DevelopError> {
        let o = self.optics;
        let (dx, dy) = (x - self.centre.0, y - self.centre.1);
        let q2 = (dx * dx + dy * dy) / (self.half_diagonal * self.half_diagonal);
        let m = 1.0 + o.manual_k * q2;
        let (x, y) = (self.centre.0 + dx * m, self.centre.1 + dy * m);
        let mut gain = if o.manual_vignette != 0.0 { (o.manual_vignette * q2).exp2() } else { 1.0 };
        let Some(pm) = &o.model else {
            return Ok(LensSample3 { points: [(x, y); 3], gain });
        };
        let model = &pm.binding.model;
        let p = &model.perspective;
        let (cx, cy) = (p.centre_x * self.size.0, p.centre_y * self.size.1);
        let (sx, sy) = (p.focal_x * self.long, p.focal_y * self.long);
        let u = p.scale * (x - cx) / sx;
        let v = p.scale * (y - cy) / sy;
        let r2 = u * u + v * v;
        let radial = radial_poly(&p.radial, r2);
        let [p1, p2] = p.tangential;
        let ud = (u * radial + 2.0 * p1 * u * v) + p2 * (r2 + 2.0 * u * u);
        let vd = (v * radial + p1 * (r2 + 2.0 * v * v)) + 2.0 * p2 * u * v;
        let g = channel_factor(&model.ca_green, ud * ud + vd * vd);
        let (gu, gv) = (ud * g, vd * g);
        let rg2 = gu * gu + gv * gv;
        let at = |fu: f64, fv: f64| -> (f64, f64) {
            let (px, py) = (cx + fu * sx, cy + fv * sy);
            (x + pm.distortion * (px - x), y + pm.distortion * (py - y))
        };
        let green = at(gu, gv);
        let (red, blue) = if pm.ca {
            let fr = channel_factor(&model.ca_red_green, rg2);
            let fb = channel_factor(&model.ca_blue_green, rg2);
            (at(gu * fr, gv * fr), at(gu * fb, gv * fb))
        } else {
            (green, green)
        };
        if let Some(a) = &model.vignette {
            let falloff = radial_poly(a, rg2);
            if !falloff.is_finite() || falloff <= 0.0 {
                return Err(DevelopError::InvalidLensProfile);
            }
            if pm.vignetting != 0.0 {
                gain *= falloff.powf(-pm.vignetting);
            }
        }
        let points = [red, green, blue];
        if !gain.is_finite() || points.iter().any(|(a, b)| !a.is_finite() || !b.is_finite()) {
            return Err(DevelopError::InvalidLensProfile);
        }
        Ok(LensSample3 { points, gain })
    }
}

fn hue_window(h: f64, lo: f64, hi: f64) -> f64 {
    const SOFT: f64 = 10.0;
    let d = |a: f64, b: f64| {
        let x = (a - b).rem_euclid(360.0);
        x.min(360.0 - x)
    };
    let mid = (lo + hi) / 2.0;
    let half = (hi - lo) / 2.0;
    let dist = d(h, mid) - half;
    if dist <= 0.0 { 1.0 } else { 1.0 - smoothstep(dist / SOFT) }
}

/// Whole-frame defringe over linear RGB (see the module documentation). Serial and deterministic;
/// the image is changed only when the pass succeeds.
pub(crate) fn defringe(rgb: &mut [f32], width: usize, height: usize, d: &PreparedDefringe, luma: [f64; 3]) -> Result<(), DevelopError> {
    let n = width.checked_mul(height).ok_or(DevelopError::Overflow)?;
    if n.checked_mul(3) != Some(rgb.len()) || width < 3 || height < 3 {
        return Err(DevelopError::InvalidDimensions);
    }
    let plane = crate::adjust::encoded_luma(rgb, &luma);
    let mut grad = vec![0.0f32; n];
    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let at = |dx: usize, dy: usize| f64::from(plane[(y + dy - 1) * width + (x + dx - 1)]);
            let gx = (at(2, 0) + 2.0 * at(2, 1) + at(2, 2)) - (at(0, 0) + 2.0 * at(0, 1) + at(0, 2));
            let gy = (at(0, 2) + 2.0 * at(1, 2) + at(2, 2)) - (at(0, 0) + 2.0 * at(1, 0) + at(2, 0));
            grad[y * width + x] = (gx.hypot(gy) / 8.0) as f32;
        }
    }
    let mut out = rgb.to_vec();
    for y in 0..height {
        for x in 0..width {
            let px = &mut out[(y * width + x) * 3..(y * width + x) * 3 + 3];
            let v = [f64::from(px[0]), f64::from(px[1]), f64::from(px[2])];
            if v.iter().any(|c| *c < 0.0) {
                continue;
            }
            let mut g = 0.0f32;
            for yy in y.saturating_sub(2)..(y + 3).min(height) {
                for xx in x.saturating_sub(2)..(x + 3).min(width) {
                    g = g.max(grad[yy * width + xx]);
                }
            }
            let edge = smoothstep(f64::from(g) / 0.08);
            if edge == 0.0 {
                continue;
            }
            let e = [enc(v[0]), enc(v[1]), enc(v[2])];
            let (h, s, _) = rgb_to_hsv(e);
            if s == 0.0 {
                continue;
            }
            let pull = (d.purple.0 * hue_window(h, d.purple.1, d.purple.2) + d.green.0 * hue_window(h, d.green.1, d.green.2)).min(1.0) * edge;
            if pull == 0.0 {
                continue;
            }
            let yl = (luma[0] * v[0] + luma[1] * v[1]) + luma[2] * v[2];
            for (slot, c) in px.iter_mut().zip(v) {
                let nv = (yl + (c - yl) * (1.0 - pull)) as f32;
                if !nv.is_finite() {
                    return Err(DevelopError::StageOverflow);
                }
                *slot = nv;
            }
        }
    }
    rgb.copy_from_slice(&out);
    Ok(())
}

impl PreparedOptics {
    /// Name and digest of the bound profile when its model runs.
    pub(crate) fn model_identity(&self) -> Option<(String, [u8; 32])> {
        self.model.as_ref().map(|m| (m.binding.name.clone(), m.binding.sha256))
    }
}
