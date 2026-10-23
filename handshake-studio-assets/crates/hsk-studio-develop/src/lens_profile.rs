//! Caller-supplied lens profiles (lane SMB-DEVELOP-LENS-PRESETS slice 1; STU-RAW-127 lens profile
//! model): a SAMPLED correction model over a (focal length, aperture, focus distance) grid, carried in
//! a documented text format this crate owns (`hsk-lens-profile 1`), interpolated for a shot's values.
//!
//! No profile database is bundled. Adobe `.lcp` files are not read: no published format specification
//! or redistribution terms were found (search 2026-10-15), so the format is NOT claimed; the lensfun
//! database is CC-BY-SA (share-alike) and its code LGPL, neither is used or read. Callers convert
//! their own profiles into this format.
//!
//! # Format (`hsk-lens-profile 1`, ASCII lines, CR LF or LF)
//!
//! ```text
//! hsk-lens-profile 1
//! name <free text to the end of the line>
//! projection rectilinear            # or fisheye (parsed, refused when bound)
//! sample <focal_mm> <aperture_value|-> <distance_m|inf|->
//! perspective <fx> <fy> <cx> <cy> <k1> <k2> <k3> [<p1> <p2> [<scale>]]
//! vignette <a1> <a2> <a3>           # optional
//! ca_green <scale> <k1> <k2> <k3>   # optional, the three lateral CA models of STU-RAW-127
//! ca_red_green <scale> <k1> <k2> <k3>
//! ca_blue_green <scale> <k1> <k2> <k3>
//! residual <mean> <standard_deviation>   # optional, surfaced in the binding
//! ```
//!
//! A line whose first non-blank character is `#` is a comment (no inline comments). Every sample
//! starts with `sample` and needs exactly one `perspective`; each other key at most once per sample.
//! `aperture_value` is the APEX Av (2 log2 N); `inf` (or any distance >= 1e30, the STU-RAW-127
//! infinity sentinel) is infinity and is interpolated as 1/d = 0. Coefficients are f64 (STU-RAW-127:
//! f32 loses shipped profiles); every number must be finite with magnitude <= 1e15.
//!
//! Interpolation (this crate's choice, UNVERIFIED against any vendor): nested brackets, focal length
//! first (linear in mm, clamped at the ends), then aperture value, then 1/distance among the samples
//! at that focal length; each bracket is a linear blend of every model coefficient. An optional
//! sub-model (vignette, CA) blends over the bracketing samples that carry it and is absent only when
//! none does.
//!
//! Hostile input (V4-01/02): byte and sample counts are checked before anything is stored, each line is
//! bounded, every refusal is `invalid_lens_profile`.
#![deny(clippy::arithmetic_side_effects)]

use crate::DevelopError;
use sha2::{Digest, Sha256};

/// Largest accepted profile (bytes). STU-RAW-127 observes up to 61,975 samples in one profile.
pub const MAX_LENS_PROFILE_BYTES: usize = 16 << 20;
/// Most samples one profile may carry.
pub const MAX_LENS_SAMPLES: usize = 65_536;
/// Longest accepted line (bytes).
pub const MAX_LENS_LINE: usize = 1024;
/// Magnitude bound of every number in a profile.
pub const MAX_LENS_COEFFICIENT: f64 = 1.0e15;
/// Distances at or above this are the infinity sentinel.
pub const INFINITY_SENTINEL_M: f64 = 1.0e30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LensProjection {
    Rectilinear,
    Fisheye,
}

/// The perspective (geometric distortion) model of one sample. `focal_*` are in units of the long
/// image side, `centre_*` normalised 0..1 image coordinates (STU-RAW-127 `perspective_model`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PerspectiveModel {
    pub focal_x: f64,
    pub focal_y: f64,
    pub centre_x: f64,
    pub centre_y: f64,
    pub radial: [f64; 3],
    pub tangential: [f64; 2],
    pub scale: f64,
}

/// A lateral chromatic-aberration model: radial magnification of one channel relative to its
/// reference, `scale * (1 + k1 r^2 + k2 r^4 + k3 r^6)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChannelModel {
    pub scale: f64,
    pub radial: [f64; 3],
}

impl ChannelModel {
    pub const IDENTITY: Self = Self { scale: 1.0, radial: [0.0; 3] };
}

/// The correction model of one shot (one profile sample or a blend of several).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LensModel {
    pub perspective: PerspectiveModel,
    /// Falloff `1 + a1 r^2 + a2 r^4 + a3 r^6`; the correction divides by it.
    pub vignette: Option<[f64; 3]>,
    pub ca_green: Option<ChannelModel>,
    pub ca_red_green: Option<ChannelModel>,
    pub ca_blue_green: Option<ChannelModel>,
    /// Residual mean error and standard deviation of the fit (blended like the coefficients).
    pub residual: Option<(f64, f64)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LensSample {
    pub focal_mm: f64,
    pub aperture_value: Option<f64>,
    /// `None` = not given; `Some(f64::INFINITY)` = the infinity sentinel.
    pub distance_m: Option<f64>,
    pub model: LensModel,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LensProfile {
    pub name: String,
    pub projection: LensProjection,
    pub samples: Vec<LensSample>,
    /// SHA-256 of the exact profile bytes (`lens_profile_digest`).
    pub sha256: [u8; 32],
}

/// The profile model resolved for one shot, plus its provenance (STU-RAW-117 `lens_profile_name`,
/// `lens_profile_digest`). This is what a recipe stores, so a re-render needs no profile file.
#[derive(Clone, Debug, PartialEq)]
pub struct LensBinding {
    pub name: String,
    pub sha256: [u8; 32],
    pub focal_mm: f64,
    pub aperture_value: Option<f64>,
    pub distance_m: Option<f64>,
    /// Number of profile samples that contributed to `model`.
    pub samples_used: u32,
    pub model: LensModel,
}

fn bad() -> DevelopError {
    DevelopError::InvalidLensProfile
}

fn number(token: &str) -> Result<f64, DevelopError> {
    let v: f64 = token.parse().map_err(|_| bad())?;
    if !v.is_finite() || v.abs() > MAX_LENS_COEFFICIENT {
        return Err(bad());
    }
    Ok(v)
}

fn numbers<const N: usize>(tokens: &[&str]) -> Result<[f64; N], DevelopError> {
    if tokens.len() != N {
        return Err(bad());
    }
    let mut out = [0.0; N];
    for (slot, t) in out.iter_mut().zip(tokens) {
        *slot = number(t)?;
    }
    Ok(out)
}

fn channel(tokens: &[&str]) -> Result<ChannelModel, DevelopError> {
    let [scale, k1, k2, k3] = numbers::<4>(tokens)?;
    if scale <= 0.0 {
        return Err(bad());
    }
    Ok(ChannelModel { scale, radial: [k1, k2, k3] })
}

struct Building {
    focal_mm: f64,
    aperture_value: Option<f64>,
    distance_m: Option<f64>,
    perspective: Option<PerspectiveModel>,
    vignette: Option<[f64; 3]>,
    ca: [Option<ChannelModel>; 3],
    residual: Option<(f64, f64)>,
}

impl Building {
    fn finish(self) -> Result<LensSample, DevelopError> {
        Ok(LensSample {
            focal_mm: self.focal_mm,
            aperture_value: self.aperture_value,
            distance_m: self.distance_m,
            model: LensModel {
                perspective: self.perspective.ok_or_else(bad)?,
                vignette: self.vignette,
                ca_green: self.ca[0],
                ca_red_green: self.ca[1],
                ca_blue_green: self.ca[2],
                residual: self.residual,
            },
        })
    }
}

fn set_once<T>(slot: &mut Option<T>, value: T) -> Result<(), DevelopError> {
    if slot.is_some() {
        return Err(bad());
    }
    *slot = Some(value);
    Ok(())
}

impl LensProfile {
    /// Parse a `hsk-lens-profile 1` document (see the module documentation).
    pub fn parse(bytes: &[u8]) -> Result<Self, DevelopError> {
        if bytes.len() > MAX_LENS_PROFILE_BYTES {
            return Err(bad());
        }
        let text = std::str::from_utf8(bytes).map_err(|_| bad())?;
        let mut lines = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
        let header = lines.next().ok_or_else(bad)?;
        if header.trim() != "hsk-lens-profile 1" {
            return Err(bad());
        }
        let mut name: Option<String> = None;
        let mut projection: Option<LensProjection> = None;
        let mut samples: Vec<LensSample> = Vec::new();
        let mut current: Option<Building> = None;
        for line in lines {
            if line.len() > MAX_LENS_LINE {
                return Err(bad());
            }
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let mut parts = trimmed.split_ascii_whitespace();
            let key = parts.next().ok_or_else(bad)?;
            let rest: Vec<&str> = parts.collect();
            if rest.len() > 16 {
                return Err(bad());
            }
            match key {
                "name" => {
                    if current.is_some() || rest.is_empty() {
                        return Err(bad());
                    }
                    let value = trimmed.get(4..).ok_or_else(bad)?.trim().to_owned();
                    set_once(&mut name, value)?;
                }
                "projection" => {
                    if current.is_some() {
                        return Err(bad());
                    }
                    let p = match rest.as_slice() {
                        ["rectilinear"] => LensProjection::Rectilinear,
                        ["fisheye"] => LensProjection::Fisheye,
                        _ => return Err(bad()),
                    };
                    set_once(&mut projection, p)?;
                }
                "sample" => {
                    if let Some(done) = current.take() {
                        if samples.len() >= MAX_LENS_SAMPLES {
                            return Err(bad());
                        }
                        samples.push(done.finish()?);
                    }
                    let [focal, aperture, distance] = match rest.as_slice() {
                        [a, b, c] => [*a, *b, *c],
                        _ => return Err(bad()),
                    };
                    let focal_mm = number(focal)?;
                    if focal_mm <= 0.0 {
                        return Err(bad());
                    }
                    let aperture_value = match aperture {
                        "-" => None,
                        a => Some(number(a)?),
                    };
                    let distance_m = match distance {
                        "-" => None,
                        "inf" => Some(f64::INFINITY),
                        d => {
                            let v: f64 = d.parse().map_err(|_| bad())?;
                            if !v.is_finite() || v <= 0.0 {
                                return Err(bad());
                            }
                            Some(if v >= INFINITY_SENTINEL_M { f64::INFINITY } else { v })
                        }
                    };
                    current = Some(Building {
                        focal_mm,
                        aperture_value,
                        distance_m,
                        perspective: None,
                        vignette: None,
                        ca: [None; 3],
                        residual: None,
                    });
                }
                _ => {
                    let sample = current.as_mut().ok_or_else(bad)?;
                    match key {
                        "perspective" => {
                            let mut v = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0];
                            if !(7..=10).contains(&rest.len()) || rest.len() == 8 {
                                return Err(bad());
                            }
                            for (slot, t) in v.iter_mut().zip(&rest) {
                                *slot = number(t)?;
                            }
                            let [fx, fy, cx, cy, k1, k2, k3, p1, p2, scale] = v;
                            if fx <= 0.0 || fy <= 0.0 || scale <= 0.0 || !(0.0..=1.0).contains(&cx) || !(0.0..=1.0).contains(&cy) {
                                return Err(bad());
                            }
                            set_once(
                                &mut sample.perspective,
                                PerspectiveModel { focal_x: fx, focal_y: fy, centre_x: cx, centre_y: cy, radial: [k1, k2, k3], tangential: [p1, p2], scale },
                            )?;
                        }
                        "vignette" => set_once(&mut sample.vignette, numbers::<3>(&rest)?)?,
                        "ca_green" => set_once(&mut sample.ca[0], channel(&rest)?)?,
                        "ca_red_green" => set_once(&mut sample.ca[1], channel(&rest)?)?,
                        "ca_blue_green" => set_once(&mut sample.ca[2], channel(&rest)?)?,
                        "residual" => {
                            let [mean, sd] = numbers::<2>(&rest)?;
                            if mean < 0.0 || sd < 0.0 {
                                return Err(bad());
                            }
                            set_once(&mut sample.residual, (mean, sd))?;
                        }
                        _ => return Err(bad()),
                    }
                }
            }
        }
        if let Some(done) = current.take() {
            if samples.len() >= MAX_LENS_SAMPLES {
                return Err(bad());
            }
            samples.push(done.finish()?);
        }
        if samples.is_empty() {
            return Err(bad());
        }
        // Two samples on the same grid point would make the blend ambiguous.
        for (i, a) in samples.iter().enumerate() {
            if samples.iter().skip(i.saturating_add(1)).any(|b| {
                a.focal_mm == b.focal_mm && a.aperture_value == b.aperture_value && a.distance_m == b.distance_m
            }) {
                return Err(bad());
            }
        }
        Ok(Self {
            name: name.ok_or_else(bad)?,
            projection: projection.unwrap_or(LensProjection::Rectilinear),
            samples,
            sha256: Sha256::digest(bytes).into(),
        })
    }

    /// Resolve the model for a shot (focal length in mm; aperture as APEX Av; focus distance in
    /// metres, `f64::INFINITY` for infinity). A fisheye profile is refused: STU-RAW-127 forbids
    /// evaluating it with the rectilinear model and no fisheye model exists here.
    pub fn bind(&self, focal_mm: f64, aperture_value: Option<f64>, distance_m: Option<f64>) -> Result<LensBinding, DevelopError> {
        if self.projection == LensProjection::Fisheye {
            return Err(DevelopError::UnsupportedContribution("fisheye_lens_model"));
        }
        if !focal_mm.is_finite() || focal_mm <= 0.0 {
            return Err(DevelopError::InvalidAdjustment);
        }
        if aperture_value.is_some_and(|a| !a.is_finite()) || distance_m.is_some_and(|d| d.is_nan() || d <= 0.0) {
            return Err(DevelopError::InvalidAdjustment);
        }
        let mut used = 0u32;
        let model = blend_focal(&self.samples, focal_mm, aperture_value, distance_m, &mut used)?;
        Ok(LensBinding {
            name: self.name.clone(),
            sha256: self.sha256,
            focal_mm,
            aperture_value,
            distance_m,
            samples_used: used,
            model,
        })
    }
}

/// Sorted distinct values and the bracket `(lo, hi, t)` of `x` among them (clamped at the ends).
fn bracket(mut values: Vec<f64>, x: f64) -> (f64, f64, f64) {
    values.sort_by(f64::total_cmp);
    values.dedup();
    let first = values.first().copied().unwrap_or(0.0);
    let last = values.last().copied().unwrap_or(0.0);
    if x <= first {
        return (first, first, 0.0);
    }
    if x >= last {
        return (last, last, 0.0);
    }
    for w in values.windows(2) {
        if let [a, b] = *w
            && x >= a
            && x <= b
        {
            return (a, b, (x - a) / (b - a));
        }
    }
    (last, last, 0.0)
}

fn inv_distance(d: Option<f64>) -> f64 {
    match d {
        None => 0.0,
        Some(v) if v.is_infinite() => 0.0,
        Some(v) => 1.0 / v,
    }
}

fn blend_focal(
    samples: &[LensSample],
    focal: f64,
    aperture: Option<f64>,
    distance: Option<f64>,
    used: &mut u32,
) -> Result<LensModel, DevelopError> {
    let (f0, f1, t) = bracket(samples.iter().map(|s| s.focal_mm).collect(), focal);
    let at = |f: f64, used: &mut u32| -> Result<LensModel, DevelopError> {
        let group: Vec<&LensSample> = samples.iter().filter(|s| s.focal_mm == f).collect();
        blend_aperture(&group, aperture, distance, used)
    };
    let a = at(f0, used)?;
    if f1 == f0 {
        return Ok(a);
    }
    let b = at(f1, used)?;
    Ok(lerp_model(&a, &b, t))
}

fn blend_aperture(
    group: &[&LensSample],
    aperture: Option<f64>,
    distance: Option<f64>,
    used: &mut u32,
) -> Result<LensModel, DevelopError> {
    let given: Vec<f64> = group.iter().filter_map(|s| s.aperture_value).collect();
    let (pick, other, t) = match aperture {
        Some(a) if !given.is_empty() => {
            let (lo, hi, t) = bracket(given, a);
            (Some(lo), Some(hi), t)
        }
        _ => (None, None, 0.0),
    };
    let at = |a: Option<f64>, used: &mut u32| -> Result<LensModel, DevelopError> {
        let sub: Vec<&LensSample> = match a {
            Some(v) => group.iter().copied().filter(|s| s.aperture_value == Some(v)).collect(),
            None => group.to_vec(),
        };
        blend_distance(&sub, distance, used)
    };
    let a = at(pick, used)?;
    if pick == other {
        return Ok(a);
    }
    let b = at(other, used)?;
    Ok(lerp_model(&a, &b, t))
}

fn blend_distance(sub: &[&LensSample], distance: Option<f64>, used: &mut u32) -> Result<LensModel, DevelopError> {
    let first = sub.first().ok_or(DevelopError::InvalidLensProfile)?;
    let keys: Vec<f64> = sub.iter().map(|s| inv_distance(s.distance_m)).collect();
    let (lo, hi, t) = bracket(keys, inv_distance(distance));
    let pick = |k: f64| sub.iter().find(|s| inv_distance(s.distance_m) == k).copied().unwrap_or(first);
    let (a, b) = (pick(lo), pick(hi));
    *used = used.saturating_add(if lo == hi { 1 } else { 2 });
    Ok(if lo == hi { a.model } else { lerp_model(&a.model, &b.model, t) })
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

fn lerp_n<const N: usize>(a: [f64; N], b: [f64; N], t: f64) -> [f64; N] {
    std::array::from_fn(|i| lerp(a[i], b[i], t))
}

fn lerp_opt<T: Copy>(a: Option<T>, b: Option<T>, t: f64, f: impl Fn(T, T, f64) -> T) -> Option<T> {
    match (a, b) {
        (Some(x), Some(y)) => Some(f(x, y, t)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

fn lerp_channel(a: ChannelModel, b: ChannelModel, t: f64) -> ChannelModel {
    ChannelModel { scale: lerp(a.scale, b.scale, t), radial: lerp_n(a.radial, b.radial, t) }
}

fn lerp_model(a: &LensModel, b: &LensModel, t: f64) -> LensModel {
    let (p, q) = (&a.perspective, &b.perspective);
    LensModel {
        perspective: PerspectiveModel {
            focal_x: lerp(p.focal_x, q.focal_x, t),
            focal_y: lerp(p.focal_y, q.focal_y, t),
            centre_x: lerp(p.centre_x, q.centre_x, t),
            centre_y: lerp(p.centre_y, q.centre_y, t),
            radial: lerp_n(p.radial, q.radial, t),
            tangential: lerp_n(p.tangential, q.tangential, t),
            scale: lerp(p.scale, q.scale, t),
        },
        vignette: lerp_opt(a.vignette, b.vignette, t, lerp_n),
        ca_green: lerp_opt(a.ca_green, b.ca_green, t, lerp_channel),
        ca_red_green: lerp_opt(a.ca_red_green, b.ca_red_green, t, lerp_channel),
        ca_blue_green: lerp_opt(a.ca_blue_green, b.ca_blue_green, t, lerp_channel),
        residual: lerp_opt(a.residual, b.residual, t, |x: (f64, f64), y: (f64, f64), t| (lerp(x.0, y.0, t), lerp(x.1, y.1, t))),
    }
}
