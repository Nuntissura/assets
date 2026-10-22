//! Geometry stage (lane SMB-DEVELOP-LENS-PRESETS; STU-RAW-118 geometry parameters, STU-RAW-131 crop
//! angle and flips): one whole-frame resample that composes the lens map ([`crate::optics`]), the
//! automatic perspective correction ([`crate::upright`]), the manual perspective sliders, the crop
//! straighten angle and flips, so every output pixel is interpolated once.
//!
//! Coordinates: the transform matrices are row-major homogeneous 3x3 maps in NORMALISED frame
//! coordinates (origin at the frame centre, unit = half the long side, y down), mapping the
//! lens-corrected frame to the output frame. Composition and inversion use Pigment's
//! [`Projective2`] (lane SMB-AE-KEY-TRACK-2) and the pixel map is validated by Pigment's
//! [`PreparedTransform`] (finite, bounded scale, positive homogeneous weight over the frame). Sampling
//! is Catmull-Rom (Pigment's [`catmull_rom`] weights, 4x4 taps clamped to the frame edge) per colour
//! channel, so lateral CA moves each channel separately; a centre outside the captured frame by more
//! than half a pixel is 0 (black) and counted in [`GeometryReport::outside_pixels`].
//!
//! Manual sliders (forward map, applied to the lens-corrected frame in this order: keystone, rotate,
//! aspect, scale, offset; every mapping UNVERIFIED against Lightroom, D5):
//! * vertical v: homogeneous row `(0, 0.4 v/100, 1)` (positive widens the top);
//! * horizontal h: row `(0.4 h/100, 0, 1)` (positive widens the left);
//! * rotate r degrees (-10..10, positive clockwise on screen); aspect a: x scale `2^(a/200)`, y scale
//!   `2^(-a/200)`; scale s (50..150): uniform `s/100`; offset x/y (-100..100): `0.5 * o/100` units.
//!
//! Crop angle (-45..45 degrees, positive turns the picture clockwise) and the flips apply after the
//! perspective; `constrain_to_warp` zooms about the centre by the smallest factor (bisection, 40
//! steps, 32 border samples per edge) for which every border sample of the output reads inside the
//! captured frame.
use crate::DevelopError;
use crate::adjust::{COUNT_LIMIT, count};
use crate::optics::{LensMap, PreparedOptics};
use crate::par::{Threads, band_rows, for_each_band};
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::transform_math::{LayerTransform, PreparedTransform, Projective2, ResampleFilter, catmull_rom};

/// STU-RAW-118 `auto_perspective_mode` closed set. The index is the slot of the stored candidate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UprightMode {
    #[default]
    Off,
    Auto,
    Level,
    Vertical,
    Full,
    Guided,
}

impl UprightMode {
    pub const ALL: [Self; 6] = [Self::Off, Self::Auto, Self::Level, Self::Vertical, Self::Full, Self::Guided];

    pub const fn index(self) -> usize {
        match self {
            Self::Off => 0,
            Self::Auto => 1,
            Self::Level => 2,
            Self::Vertical => 3,
            Self::Full => 4,
            Self::Guided => 5,
        }
    }

    pub const fn key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Auto => "auto",
            Self::Level => "level",
            Self::Vertical => "vertical",
            Self::Full => "full",
            Self::Guided => "guided",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.key() == key)
    }
}

/// Identity matrix (row-major 3x3).
pub const IDENTITY_3X3: [f64; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
/// Most guided-mode reference lines (STU-RAW-118 `auto_perspective_guide_count` hard max).
pub const MAX_GUIDES: usize = 4;

/// STU-RAW-118 geometry group.
#[derive(Clone, Debug, PartialEq)]
pub struct Geometry {
    pub perspective_vertical: f64,
    pub perspective_horizontal: f64,
    /// Degrees, -10..10.
    pub perspective_rotate: f64,
    /// 50..150, default 100.
    pub perspective_scale: f64,
    pub perspective_aspect: f64,
    pub perspective_offset_x: f64,
    pub perspective_offset_y: f64,
    pub upright_mode: UprightMode,
    /// Normalised 0..1 image coordinates of the analysis centre (default 0.5, 0.5).
    pub upright_centre: (f64, f64),
    /// 35 mm-equivalent focal length the analysis assumes (default 35, spec fallback).
    pub upright_focal_length_35mm: f64,
    /// Guided mode: up to four segments in normalised 0..1 image coordinates.
    pub upright_guides: Vec<[[f64; 2]; 2]>,
    /// The six stored candidate transforms (off, auto, level, vertical, full, guided), normalised
    /// coordinates; `None` = analyse the frame at develop time (deterministic).
    pub upright_transforms: Option<[[f64; 9]; 6]>,
}

impl Default for Geometry {
    fn default() -> Self {
        Self {
            perspective_vertical: 0.0,
            perspective_horizontal: 0.0,
            perspective_rotate: 0.0,
            perspective_scale: 100.0,
            perspective_aspect: 0.0,
            perspective_offset_x: 0.0,
            perspective_offset_y: 0.0,
            upright_mode: UprightMode::Off,
            upright_centre: (0.5, 0.5),
            upright_focal_length_35mm: 35.0,
            upright_guides: Vec::new(),
            upright_transforms: None,
        }
    }
}

fn finite(v: f64) -> Result<f64, DevelopError> {
    if v.is_finite() { Ok(v) } else { Err(DevelopError::NonFinite) }
}

fn within(v: f64, lo: f64, hi: f64) -> Result<f64, DevelopError> {
    let v = finite(v)?;
    if (lo..=hi).contains(&v) { Ok(v) } else { Err(DevelopError::InvalidAdjustment) }
}

fn mat(m: [f64; 9]) -> Projective2 {
    Projective2 { m }
}

/// Rotation by `degrees` (positive clockwise on screen, y down).
pub(crate) fn rotation(degrees: f64) -> Projective2 {
    let (s, c) = degrees.to_radians().sin_cos();
    mat([c, -s, 0.0, s, c, 0.0, 0.0, 0.0, 1.0])
}

/// Validated geometry parameters (the manual part composed into one matrix).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreparedGeometry {
    pub(crate) manual: Projective2,
    pub(crate) mode: UprightMode,
    pub(crate) stored: Option<[[f64; 9]; 6]>,
    pub(crate) geometry: Geometry,
}

impl PreparedGeometry {
    pub(crate) fn prepare(g: &Geometry, active: bool) -> Result<Option<Self>, DevelopError> {
        let v = count(g.perspective_vertical)? / COUNT_LIMIT;
        let h = count(g.perspective_horizontal)? / COUNT_LIMIT;
        let r = within(g.perspective_rotate, -10.0, 10.0)?;
        let s = within(g.perspective_scale, 50.0, 150.0)? / 100.0;
        let a = count(g.perspective_aspect)? / COUNT_LIMIT;
        let ox = count(g.perspective_offset_x)? / COUNT_LIMIT;
        let oy = count(g.perspective_offset_y)? / COUNT_LIMIT;
        within(g.upright_centre.0, 0.0, 1.0)?;
        within(g.upright_centre.1, 0.0, 1.0)?;
        if finite(g.upright_focal_length_35mm)? <= 0.0 || g.upright_focal_length_35mm > 10_000.0 {
            return Err(DevelopError::InvalidAdjustment);
        }
        if g.upright_guides.len() > MAX_GUIDES {
            return Err(DevelopError::InvalidAdjustment);
        }
        for seg in &g.upright_guides {
            for p in seg {
                within(p[0], 0.0, 1.0)?;
                within(p[1], 0.0, 1.0)?;
            }
            if seg[0] == seg[1] {
                return Err(DevelopError::InvalidAdjustment);
            }
        }
        if let Some(stored) = &g.upright_transforms {
            for m in stored {
                if m.iter().any(|v| !v.is_finite()) {
                    return Err(DevelopError::NonFinite);
                }
                mat(*m).inverse().map_err(|_| DevelopError::InvalidTransform)?;
            }
        }
        if !active {
            return Ok(None);
        }
        let keystone = mat([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.4 * h, 0.4 * v, 1.0]);
        let aspect = mat([(a / 2.0).exp2(), 0.0, 0.0, 0.0, (-a / 2.0).exp2(), 0.0, 0.0, 0.0, 1.0]);
        let scale = mat([s, 0.0, 0.0, 0.0, s, 0.0, 0.0, 0.0, 1.0]);
        let offset = mat([1.0, 0.0, 0.5 * ox, 0.0, 1.0, 0.5 * oy, 0.0, 0.0, 1.0]);
        let manual = keystone.then(&rotation(r)).then(&aspect).then(&scale).then(&offset);
        let identity_manual = manual.m == IDENTITY_3X3;
        if identity_manual && g.upright_mode == UprightMode::Off {
            return Ok(None);
        }
        Ok(Some(Self { manual, mode: g.upright_mode, stored: g.upright_transforms, geometry: g.clone() }))
    }
}

/// What the geometry stage did (carried in [`crate::DevelopReceipt::geometry`]).
#[derive(Clone, Debug, PartialEq)]
pub struct GeometryReport {
    /// Output pixels whose green sample fell outside the captured frame (written as 0).
    pub outside_pixels: u64,
    /// Zoom applied by `constrain_to_warp` (1 when off).
    pub constrain_zoom: f64,
    /// Forward transform (lens-corrected frame -> output) in normalised coordinates.
    pub transform: [f64; 9],
    /// Automatic perspective mode applied and the six candidates used.
    pub upright_mode: UprightMode,
    pub upright_transforms: Option<[[f64; 9]; 6]>,
    pub lens_profile: Option<(String, [u8; 32])>,
}

/// Normalised <-> pixel conversion of a frame.
#[derive(Clone, Copy)]
pub(crate) struct Frame {
    pub(crate) width: f64,
    pub(crate) height: f64,
    pub(crate) unit: f64,
}

impl Frame {
    pub(crate) fn new(width: u32, height: u32) -> Self {
        let (w, h) = (f64::from(width), f64::from(height));
        Self { width: w, height: h, unit: w.max(h) / 2.0 }
    }
    pub(crate) fn to_norm(self, x: f64, y: f64) -> (f64, f64) {
        ((x - self.width / 2.0) / self.unit, (y - self.height / 2.0) / self.unit)
    }
    pub(crate) fn to_pixel(self, u: f64, v: f64) -> (f64, f64) {
        (u * self.unit + self.width / 2.0, v * self.unit + self.height / 2.0)
    }
    /// Pixel -> normalised as a matrix.
    pub(crate) fn norm_matrix(self) -> Projective2 {
        let k = 1.0 / self.unit;
        mat([k, 0.0, -self.width / 2.0 * k, 0.0, k, -self.height / 2.0 * k, 0.0, 0.0, 1.0])
    }
}

/// The complete map of one frame: output pixel -> lens-corrected pixel -> captured RGB positions.
pub(crate) struct WarpPlan<'a> {
    pub(crate) frame: Frame,
    /// Output normalised -> lens-corrected normalised (inverse of the forward transform), `None` = identity.
    pub(crate) inverse: Option<Projective2>,
    pub(crate) lens: Option<LensMap<'a>>,
}

impl WarpPlan<'_> {
    /// Captured sample positions and gain of output pixel centre `(x, y)`; `None` beyond the horizon.
    fn sample(&self, x: f64, y: f64) -> Result<Option<Sampled>, DevelopError> {
        let (mut px, mut py) = (x, y);
        if let Some(inv) = &self.inverse {
            let (u, v) = self.frame.to_norm(x, y);
            match inv.apply(u, v) {
                Some((a, b)) => (px, py) = self.frame.to_pixel(a, b),
                None => return Ok(None),
            }
        }
        match &self.lens {
            Some(lens) => {
                let s = lens.sample(px, py)?;
                Ok(Some((s.points, s.gain)))
            }
            None => Ok(Some(([(px, py); 3], 1.0))),
        }
    }

    fn inside(&self, p: (f64, f64)) -> bool {
        p.0 >= 0.0 && p.1 >= 0.0 && p.0 < self.frame.width && p.1 < self.frame.height
    }
}

/// Captured red, green and blue sample positions and the gain of one output pixel.
type Sampled = ([(f64, f64); 3], f64);

/// Forward map (lens-corrected normalised -> output normalised) for the crop angle and flips.
pub(crate) fn crop_map(angle: f64, flip_h: bool, flip_v: bool) -> Projective2 {
    let flip = mat([if flip_h { -1.0 } else { 1.0 }, 0.0, 0.0, 0.0, if flip_v { -1.0 } else { 1.0 }, 0.0, 0.0, 0.0, 1.0]);
    rotation(angle).then(&flip)
}

/// Validate a forward normalised map over the frame through Pigment's projective admission.
pub(crate) fn admit(forward: &Projective2, frame: Frame) -> Result<Projective2, DevelopError> {
    let n = frame.norm_matrix();
    let pixel = n.then(forward).then(&n.inverse().map_err(|_| DevelopError::InvalidTransform)?);
    let src = Rect { x: 0, y: 0, width: frame.width as u32, height: frame.height as u32 };
    PreparedTransform::new(&LayerTransform::projective(pixel, ResampleFilter::Bicubic), src)
        .map_err(|_| DevelopError::InvalidTransform)?;
    forward.inverse().map_err(|_| DevelopError::InvalidTransform)
}

/// Smallest zoom (>= 1) about the centre for which the output border reads inside the frame.
pub(crate) fn constrain_zoom(plan: &WarpPlan<'_>) -> Result<f64, DevelopError> {
    let f = plan.frame;
    let border = |zoom: f64| -> Result<bool, DevelopError> {
        for k in 0..=32 {
            let t = f64::from(k) / 32.0;
            for (x, y) in [(t * f.width, 0.0), (t * f.width, f.height), (0.0, t * f.height), (f.width, t * f.height)] {
                let (cx, cy) = (f.width / 2.0 + (x - f.width / 2.0) / zoom, f.height / 2.0 + (y - f.height / 2.0) / zoom);
                let inset = (cx.clamp(0.5, f.width - 0.5), cy.clamp(0.5, f.height - 0.5));
                match plan.sample(inset.0, inset.1)? {
                    Some((points, _)) if points.iter().all(|p| plan.inside(*p)) => {}
                    _ => return Ok(false),
                }
            }
        }
        Ok(true)
    };
    if border(1.0)? {
        return Ok(1.0);
    }
    let (mut lo, mut hi) = (1.0f64, 16.0f64);
    if !border(hi)? {
        return Err(DevelopError::InvalidTransform);
    }
    for _ in 0..40 {
        let mid = (lo + hi) / 2.0;
        if border(mid)? { hi = mid } else { lo = mid }
    }
    Ok(hi)
}

/// Catmull-Rom sample of channel `c` of an interleaved RGB frame at continuous pixel position `p`.
fn sample_channel(rgb: &[f32], width: usize, height: usize, c: usize, p: (f64, f64)) -> f64 {
    let (u, v) = (p.0 - 0.5, p.1 - 0.5);
    let (x0, y0) = (u.floor(), v.floor());
    let (wx, wy) = (catmull_rom(u - x0), catmull_rom(v - y0));
    let (mw, mh) = (width as i64 - 1, height as i64 - 1);
    let mut sum = 0.0f64;
    for (j, wyj) in wy.iter().enumerate() {
        let row = (y0 as i64 + j as i64 - 1).clamp(0, mh) as usize;
        for (i, wxi) in wx.iter().enumerate() {
            let col = (x0 as i64 + i as i64 - 1).clamp(0, mw) as usize;
            sum += (wxi * wyj) * f64::from(rgb[(row * width + col) * 3 + c]);
        }
    }
    sum
}

/// Resample the whole frame through `plan` (output = same size as the input). Returns the image and
/// the number of output pixels whose green sample fell outside.
pub(crate) fn warp(
    rgb: &[f32],
    width: u32,
    height: u32,
    plan: &WarpPlan<'_>,
    threads: Threads,
    cancel: &CancellationToken,
) -> Result<(Vec<f32>, u64), DevelopError> {
    let (w, h) = (width as usize, height as usize);
    let n = w.checked_mul(h).ok_or(DevelopError::Overflow)?;
    if w == 0 || h == 0 || n.checked_mul(3) != Some(rgb.len()) {
        return Err(DevelopError::InvalidDimensions);
    }
    let mut out = vec![0.0f32; n * 3];
    let rows = band_rows(w);
    let outside = std::sync::atomic::AtomicU64::new(0);
    for_each_band(&mut out, rows * w * 3, threads, cancel, |band_index, band| {
        let first = band_index * rows;
        let mut missed = 0u64;
        for (k, px) in band.chunks_exact_mut(3).enumerate() {
            let (x, y) = (k % w, first + k / w);
            let Some((points, gain)) = plan.sample(x as f64 + 0.5, y as f64 + 0.5)? else {
                missed += 1;
                continue;
            };
            let edge = |p: (f64, f64)| p.0 >= 0.0 && p.1 >= 0.0 && p.0 <= w as f64 && p.1 <= h as f64;
            if !edge(points[1]) {
                missed += 1;
            }
            for (c, slot) in px.iter_mut().enumerate() {
                if !edge(points[c]) {
                    continue;
                }
                let v = (sample_channel(rgb, w, h, c, points[c]) * gain) as f32;
                if !v.is_finite() {
                    return Err(DevelopError::StageOverflow);
                }
                *slot = v;
            }
        }
        outside.fetch_add(missed, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    })?;
    Ok((out, outside.into_inner()))
}

/// Run the optics + perspective + crop-angle warp over a full linear frame (develop's whole-frame
/// geometry stage), then defringe. Returns the warped frame and its report.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    rgb: &[f32],
    width: u32,
    height: u32,
    optics: &PreparedOptics,
    geometry: Option<&PreparedGeometry>,
    crop: &crate::recipe::NormalizedCrop,
    luma: [f64; 3],
    threads: Threads,
    cancel: &CancellationToken,
) -> Result<(Vec<f32>, GeometryReport), DevelopError> {
    let frame = Frame::new(width, height);
    let lens = optics.warps().then(|| LensMap::new(optics, width, height));
    // Upright candidates: stored, else analysed on the lens-corrected frame.
    let (upright, candidates) = match geometry {
        Some(g) if g.mode != UprightMode::Off => {
            let candidates = match g.stored {
                Some(stored) => stored,
                None => {
                    let base = match &lens {
                        Some(_) => {
                            let plan = WarpPlan { frame, inverse: None, lens: lens.as_ref().map(|_| LensMap::new(optics, width, height)) };
                            warp(rgb, width, height, &plan, threads, cancel)?.0
                        }
                        None => rgb.to_vec(),
                    };
                    crate::upright::analyse(&base, width, height, &g.geometry, luma)?.transforms
                }
            };
            (mat(candidates[g.mode.index()]), Some(candidates))
        }
        _ => (Projective2::IDENTITY, None),
    };
    let manual = geometry.map_or(Projective2::IDENTITY, |g| g.manual);
    let forward = upright.then(&manual).then(&crop_map(crop.angle, crop.flip_horizontal, crop.flip_vertical));
    let is_identity = forward.m == IDENTITY_3X3;
    let inverse = if is_identity { None } else { Some(admit(&forward, frame)?) };
    let mut plan = WarpPlan { frame, inverse, lens };
    let mut zoom = 1.0;
    if crop.constrain_to_warp {
        zoom = constrain_zoom(&plan)?;
        if zoom != 1.0 {
            let z = mat([1.0 / zoom, 0.0, 0.0, 0.0, 1.0 / zoom, 0.0, 0.0, 0.0, 1.0]);
            let inv = plan.inverse.unwrap_or(Projective2::IDENTITY);
            plan.inverse = Some(z.then(&inv));
        }
    }
    let (mut out, outside) = if plan.inverse.is_none() && plan.lens.is_none() {
        (rgb.to_vec(), 0)
    } else {
        warp(rgb, width, height, &plan, threads, cancel)?
    };
    if let Some(d) = &optics.defringe {
        crate::optics::defringe(&mut out, width as usize, height as usize, d, luma)?;
    }
    Ok((
        out,
        GeometryReport {
            outside_pixels: outside,
            constrain_zoom: zoom,
            transform: forward.m,
            upright_mode: geometry.map_or(UprightMode::Off, |g| g.mode),
            upright_transforms: candidates,
            lens_profile: optics.model_identity(),
        },
    ))
}

/// The geometry stage on its own, over an already developed linear RGB frame (`width x height`,
/// 3 x f32 per pixel): lens corrections when `enables.enable_lens_corrections`, perspective when
/// `enables.enable_transform`, then the crop angle, flips and `constrain_to_warp` of `crop`. The
/// result is the full frame (not cropped). Every parameter is validated as in a recipe.
#[allow(clippy::too_many_arguments)]
pub fn apply_geometry(
    rgb: &[f32],
    width: u32,
    height: u32,
    adjust: &crate::adjust::Adjustments,
    enables: &crate::recipe::Enables,
    crop: &crate::recipe::NormalizedCrop,
    threads: Threads,
    cancel: &CancellationToken,
) -> Result<(Vec<f32>, GeometryReport), DevelopError> {
    crop.source_bounds(width, height)?;
    if rgb.iter().any(|v| !v.is_finite()) {
        return Err(DevelopError::NonFinite);
    }
    let optics = PreparedOptics::prepare(&adjust.optics, enables.enable_lens_corrections)?;
    let geometry = PreparedGeometry::prepare(&adjust.geometry, enables.enable_transform)?;
    apply(rgb, width, height, &optics, geometry.as_ref(), crop, adjust.luma, threads, cancel)
}
