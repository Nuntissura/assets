//! Layer transform resampling (lane SMB-PIGMENT-TRANSFORM): the affine 2x3 operator, its validation
//! and the scalar binary64 REFERENCE sampler every backend must match (render-cpu calls it, so its
//! bytes are this module's; render-gpu transcribes it to WGSL and is tolerance-tested against it).
//!
//! Spec basis: STU-RAS-141 (explicit resampling method; `nearest_neighbour = 2`, `bilinear = 3`,
//! `bicubic = 4` are implemented, the other members are not representable here), STU-MOT-007
//! (`resample_filter` of a transformed layer, `bilinear | bicubic`). The spec does not name the cubic:
//! Studio's `bicubic` is the Catmull-Rom spline (Keys 1981 cubic convolution with `a = -0.5`), which
//! interpolates (a sample on a pixel centre returns that pixel). Photoshop's and After Effects' exact
//! kernels are UNVERIFIED, so every filter is class Approximate ([`TRANSFORM_EXACTNESS`]).
//!
//! Geometry. [`Affine2`] maps SOURCE plan coordinates to DESTINATION plan coordinates (continuous,
//! pixel `(x, y)` covers `[x, x+1) x [y, y+1)`): `x' = m[0] x + m[1] y + m[2]`,
//! `y' = m[3] x + m[4] y + m[5]`. The source is a tile placed at `src` (its plan rect). Destination
//! pixel `(x, y)` is sampled at its centre: `(sx, sy) = inverse(x + 0.5, y + 0.5)`, then in
//! continuous texel-index space `u = sx - src.x - 0.5`, `v = sy - src.y - 0.5` (integer `u` = a texel
//! centre).
//!
//! Sampling rules (one definition for colour and coverage):
//! 1. Exact centre: when `u` and `v` are both integers inside the source, the texel is returned
//!    unchanged (all three filters interpolate). An identity transform, an integer translation and a
//!    rotation by a multiple of 90 degrees about a texel-grid point are therefore byte-identical
//!    copies (tests `tests/transform.rs`).
//! 2. Nearest: texel `(floor(u + 0.5), floor(v + 0.5))`; ties go to the higher index.
//! 3. Bilinear: taps `floor(u) + {0, 1}` with weights `1 - t, t`; bicubic: taps `floor(u) + {-1..=2}`
//!    with the Catmull-Rom weights of `t = u - floor(u)`. 2-D weights are products, taps are summed in
//!    row-major order (v outer, u inner) in binary64.
//! 4. Colour is resampled premultiplied: `A = sum w*alpha`, `P = sum w*alpha*C`; `A <= 0` gives a
//!    transparent texel `[0, 0, 0, 0]`; otherwise alpha = `min(A, 1)` and colour =
//!    `clamp(P / A, 0, 1)` per channel (the bicubic lobes can overshoot; samples outside `[0, 1]`
//!    have no operator class in the plan IR). Coverage is `clamp(sum w*m, 0, 1)`. One rounding to
//!    binary32 at the end.
//! 5. Edge modes: [`EdgeMode::Transparent`] reads a tap outside the source as transparent (zero
//!    coverage), so edges are filtered; [`EdgeMode::Clamp`] reads the nearest edge texel for a tap
//!    outside, and a destination centre whose `(u, v)` lies outside `[-0.5, w - 0.5) x
//!    [-0.5, h - 0.5)` is transparent (hard edge).
//!
//! Projective transforms (lane SMB-AE-KEY-TRACK-2; corner pin and planar tracks, STU-CMP-065):
//! [`LayerTransform::perspective`] `Some(p)` makes the map `p` applied after `affine` (a
//! [`Projective2`], homogeneous row-major 3x3). Destination pixel centres map back through the
//! inverse 3x3; a centre whose homogeneous weight is not positive lies beyond the plane's horizon
//! and is transparent. Rules 1-5 apply unchanged to the resulting `(u, v)`, so the bytes stay the
//! reference's and a backend that takes its sample sites from [`PreparedTransform::sample_site`]
//! (render-cpu, render-gpu) runs perspective without its own geometry. Validation of a projective
//! map: finite coefficients, a positive homogeneous weight above [`MIN_PERSPECTIVE_WEIGHT`] at the
//! four corners of the source footprint (so on all of it: the weight is affine in the source),
//! the local linear part (Jacobian) and its inverse at those corners and the centre bounded by
//! [`MAX_TRANSFORM_LINEAR`], and mapped corners inside [`MAX_TRANSFORM_TRANSLATE`]; the bounds are
//! the mapped footprint corners (a line maps to a line where the weight is positive).
//!
//! Hostile inputs (V4-01/02): every coefficient must be finite, the linear part and its inverse are
//! bounded by [`MAX_TRANSFORM_LINEAR`] (so scale lies in `[1/1024, 1024]` per axis, no degenerate
//! shear), the translation by [`MAX_TRANSFORM_TRANSLATE`]; a singular matrix is refused; the
//! destination bounds are computed in binary64 and refused above [`MAX_TRANSFORM_BOUNDS_SIDE`] per
//! side before anything is allocated; the reference resampler checks its output area against
//! [`MAX_REFERENCE_PIXELS`] before allocating. Integer arithmetic is checked.
#![deny(clippy::arithmetic_side_effects)]
use crate::Rect;
use crate::blend_math::Exactness;
use serde::Serialize;

/// Class of every resampling filter (Studio rules; Photoshop / After Effects parity UNVERIFIED).
pub const TRANSFORM_EXACTNESS: Exactness = Exactness::Approximate;
/// Bound on every coefficient of the linear part and of its inverse (scale `1/1024 ..= 1024`).
pub const MAX_TRANSFORM_LINEAR: f64 = 1024.0;
/// Bound on the translation coefficients (plan pixels).
pub const MAX_TRANSFORM_TRANSLATE: f64 = 16_777_216.0;
/// Bound on each side of a transformed layer's destination bounds (plan pixels).
pub const MAX_TRANSFORM_BOUNDS_SIDE: u32 = 1 << 20;
/// Number of transformed layers one plan envelope may carry.
pub const MAX_TRANSFORMS_PER_PLAN: usize = 4096;
/// Aggregate destination-bounds area (pixels) over the transformed layers of one envelope.
pub const MAX_TRANSFORM_AREA_PER_PLAN: u64 = 1 << 36;
/// Output pixels [`resample_reference`] allocates at most.
pub const MAX_REFERENCE_PIXELS: u64 = 1 << 26;
/// Smallest homogeneous weight a projective map may give a source footprint corner, relative to
/// the largest absolute coefficient of its last row.
pub const MIN_PERSPECTIVE_WEIGHT: f64 = 1e-6;

/// A projective map, homogeneous row-major 3x3: `x' = (m0 x + m1 y + m2) / w`,
/// `y' = (m3 x + m4 y + m5) / w`, `w = m6 x + m7 y + m8`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Projective2 {
    pub m: [f64; 9],
}
impl Projective2 {
    pub const IDENTITY: Self = Self {
        m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
    };
    pub fn from_affine(a: &Affine2) -> Self {
        let [p, q, r, s, t, u] = a.m;
        Self { m: [p, q, r, s, t, u, 0.0, 0.0, 1.0] }
    }
    /// `self` followed by `after`.
    pub fn then(&self, after: &Self) -> Self {
        let (a, b) = (&after.m, &self.m);
        let at = |r: usize, c: usize| -> f64 {
            let row = match r {
                0 => [a[0], a[1], a[2]],
                1 => [a[3], a[4], a[5]],
                _ => [a[6], a[7], a[8]],
            };
            let col = match c {
                0 => [b[0], b[3], b[6]],
                1 => [b[1], b[4], b[7]],
                _ => [b[2], b[5], b[8]],
            };
            row[0] * col[0] + row[1] * col[1] + row[2] * col[2]
        };
        Self { m: [at(0, 0), at(0, 1), at(0, 2), at(1, 0), at(1, 1), at(1, 2), at(2, 0), at(2, 1), at(2, 2)] }
    }
    /// The homogeneous image `(X, Y, W)` of `(x, y)`.
    pub fn apply_h(&self, x: f64, y: f64) -> (f64, f64, f64) {
        let m = &self.m;
        (m[0] * x + m[1] * y + m[2], m[3] * x + m[4] * y + m[5], m[6] * x + m[7] * y + m[8])
    }
    /// The image of `(x, y)`, `None` where the weight is not positive or a value is not finite.
    pub fn apply(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        let (px, py, w) = self.apply_h(x, y);
        let (ox, oy) = (px / w, py / w);
        (w > 0.0 && ox.is_finite() && oy.is_finite()).then_some((ox, oy))
    }
    /// Inverse by the adjugate over the determinant (a source point of positive weight maps back
    /// with positive weight).
    pub fn inverse(&self) -> Result<Self, TransformError> {
        let [a, b, c, d, e, f, g, h, i] = self.m;
        if !self.m.iter().all(|v| v.is_finite()) {
            return Err(TransformError::NonFinite);
        }
        let adj = [
            e * i - f * h,
            c * h - b * i,
            b * f - c * e,
            f * g - d * i,
            a * i - c * g,
            c * d - a * f,
            d * h - e * g,
            b * g - a * h,
            a * e - b * d,
        ];
        let det = a * adj[0] + b * adj[3] + c * adj[6];
        if !det.is_finite() || det == 0.0 {
            return Err(TransformError::Singular);
        }
        // adj / det; the sign of det is kept so a positive weight stays positive.
        let m = adj.map(|v| v / det);
        if !m.iter().all(|v| v.is_finite()) {
            return Err(TransformError::NonFinite);
        }
        Ok(Self { m })
    }
    /// The map sending the quad `src` to the quad `dst` (corner `k` to corner `k`): the exact
    /// 4-point homography (normalised DLT, `track_math::Homography::fit`). `Singular` for
    /// collinear or coincident corners.
    pub fn from_quads(src: [[f64; 2]; 4], dst: [[f64; 2]; 4]) -> Result<Self, TransformError> {
        let h = crate::track_math::Homography::fit(&src, &dst).map_err(|e| match e {
            crate::track_math::TrackError::Parameter => TransformError::NonFinite,
            _ => TransformError::Singular,
        })?;
        Ok(Self { m: h.m })
    }
    /// Local linear part (Jacobian) of the map at `(x, y)`, `None` off the positive half-plane.
    fn jacobian(&self, x: f64, y: f64) -> Option<[f64; 4]> {
        let (px, py, w) = self.apply_h(x, y);
        if w <= 0.0 {
            return None;
        }
        let (ox, oy) = (px / w, py / w);
        let m = &self.m;
        Some([(m[0] - ox * m[6]) / w, (m[1] - ox * m[7]) / w, (m[3] - oy * m[6]) / w, (m[4] - oy * m[7]) / w])
    }
}

/// Resampling method (STU-RAS-141 members 2, 3, 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum ResampleFilter {
    NearestNeighbour,
    Bilinear,
    /// Catmull-Rom (Keys cubic convolution, `a = -0.5`).
    Bicubic,
}
impl ResampleFilter {
    pub const fn key(self) -> &'static str {
        match self {
            Self::NearestNeighbour => "nearest_neighbour",
            Self::Bilinear => "bilinear",
            Self::Bicubic => "bicubic",
        }
    }
    /// The STU-RAS-141 enumeration value.
    pub const fn spec_value(self) -> u8 {
        match self {
            Self::NearestNeighbour => 2,
            Self::Bilinear => 3,
            Self::Bicubic => 4,
        }
    }
    /// How far (texels) beyond the source a destination centre can still receive a tap under
    /// [`EdgeMode::Transparent`]: the footprint is the source rect grown by this amount.
    pub const fn footprint_growth(self) -> f64 {
        match self {
            Self::NearestNeighbour => 0.0,
            Self::Bilinear => 0.5,
            Self::Bicubic => 1.5,
        }
    }
}

/// What a tap outside the source reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum EdgeMode {
    /// Transparent / zero coverage outside (filtered edges).
    Transparent,
    /// Nearest edge texel; the output is limited to the source rect (hard edge).
    Clamp,
}
impl EdgeMode {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Transparent => "transparent",
            Self::Clamp => "clamp",
        }
    }
}

/// Affine map source plan coordinates -> destination plan coordinates, row-major 2x3:
/// `x' = m[0] x + m[1] y + m[2]`, `y' = m[3] x + m[4] y + m[5]`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Affine2 {
    pub m: [f64; 6],
}
impl Affine2 {
    pub const IDENTITY: Self = Self {
        m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
    };
    pub const fn translate(tx: f64, ty: f64) -> Self {
        Self {
            m: [1.0, 0.0, tx, 0.0, 1.0, ty],
        }
    }
    /// `self` followed by `after` (apply `self` first, then `after`).
    pub fn then(self, after: Self) -> Self {
        let [a, b, c, d, e, f] = self.m;
        let [p, q, r, s, t, u] = after.m;
        Self {
            m: [
                p * a + q * d,
                p * b + q * e,
                p * c + q * f + r,
                s * a + t * d,
                s * b + t * e,
                s * c + t * f + u,
            ],
        }
    }
    /// Scale by `(sx, sy)`, rotate by `degrees` (clockwise on screen: y points down), translate by
    /// `(tx, ty)`, all about the anchor `(ax, ay)` (source coordinates). Multiples of 90 degrees use
    /// exact sine and cosine so a quarter turn stays an exact copy.
    pub fn from_parts(ax: f64, ay: f64, sx: f64, sy: f64, degrees: f64, tx: f64, ty: f64) -> Self {
        let (sin, cos) = exact_sin_cos(degrees);
        Self::translate(-ax, -ay)
            .then(Self {
                m: [sx, 0.0, 0.0, 0.0, sy, 0.0],
            })
            .then(Self {
                m: [cos, -sin, 0.0, sin, cos, 0.0],
            })
            .then(Self::translate(ax + tx, ay + ty))
    }
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let [a, b, c, d, e, f] = self.m;
        (a * x + b * y + c, d * x + e * y + f)
    }
    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }
    /// Validate (finite, bounded, invertible) and return the inverse.
    pub fn inverse(&self) -> Result<Self, TransformError> {
        if !self.m.iter().all(|v| v.is_finite()) {
            return Err(TransformError::NonFinite);
        }
        let [a, b, c, d, e, f] = self.m;
        if [a, b, d, e].iter().any(|v| v.abs() > MAX_TRANSFORM_LINEAR) {
            return Err(TransformError::ScaleOutOfRange);
        }
        if c.abs() > MAX_TRANSFORM_TRANSLATE || f.abs() > MAX_TRANSFORM_TRANSLATE {
            return Err(TransformError::TranslationOutOfRange);
        }
        let det = a * e - b * d;
        if !det.is_finite() || det == 0.0 {
            return Err(TransformError::Singular);
        }
        let (ia, ib, id, ie) = (e / det, -b / det, -d / det, a / det);
        if ![ia, ib, id, ie]
            .iter()
            .all(|v| v.is_finite() && v.abs() <= MAX_TRANSFORM_LINEAR)
        {
            return Err(TransformError::ScaleOutOfRange);
        }
        let ic = -(ia * c + ib * f);
        let i_f = -(id * c + ie * f);
        if !(ic.is_finite() && i_f.is_finite()) {
            return Err(TransformError::NonFinite);
        }
        Ok(Self {
            m: [ia, ib, ic, id, ie, i_f],
        })
    }
}

/// Sine and cosine of `degrees`, exact for multiples of 90.
pub fn exact_sin_cos(degrees: f64) -> (f64, f64) {
    let r = degrees.rem_euclid(360.0);
    if r == 0.0 {
        (0.0, 1.0)
    } else if r == 90.0 {
        (1.0, 0.0)
    } else if r == 180.0 {
        (0.0, -1.0)
    } else if r == 270.0 {
        (-1.0, 0.0)
    } else {
        r.to_radians().sin_cos()
    }
}

/// A layer transform as carried by the plan envelope.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct LayerTransform {
    pub affine: Affine2,
    pub filter: ResampleFilter,
    pub edge: EdgeMode,
    /// The layer's coverage mask is transformed with its content (a linked mask); otherwise it
    /// stays in plan space.
    pub transform_mask: bool,
    /// A projective map applied after `affine` (corner pin, planar track); `None` = affine only
    /// (and absent from the digest).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub perspective: Option<Projective2>,
}
impl LayerTransform {
    pub const fn new(affine: Affine2, filter: ResampleFilter) -> Self {
        Self {
            affine,
            filter,
            edge: EdgeMode::Transparent,
            transform_mask: false,
            perspective: None,
        }
    }
    /// A projective transform (`affine` identity).
    pub const fn projective(p: Projective2, filter: ResampleFilter) -> Self {
        Self {
            affine: Affine2::IDENTITY,
            filter,
            edge: EdgeMode::Transparent,
            transform_mask: false,
            perspective: Some(p),
        }
    }
    pub const fn with_perspective(mut self, p: Projective2) -> Self {
        self.perspective = Some(p);
        self
    }
    /// The whole source -> destination map as a projective matrix.
    pub fn map(&self) -> Projective2 {
        let a = Projective2::from_affine(&self.affine);
        match &self.perspective {
            None => a,
            Some(p) => a.then(p),
        }
    }
    pub const fn with_edge(mut self, edge: EdgeMode) -> Self {
        self.edge = edge;
        self
    }
    pub const fn with_mask(mut self, transform_mask: bool) -> Self {
        self.transform_mask = transform_mask;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransformError {
    NonFinite,
    Singular,
    ScaleOutOfRange,
    TranslationOutOfRange,
    /// Destination bounds above [`MAX_TRANSFORM_BOUNDS_SIDE`] or outside the i64 plan space.
    BoundsOutOfRange,
    /// Output area above [`MAX_REFERENCE_PIXELS`] (reference resampler only).
    BudgetExceeded,
    EmptySource,
}
impl TransformError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::NonFinite => "transform_nonfinite",
            Self::Singular => "transform_singular",
            Self::ScaleOutOfRange => "transform_scale_out_of_range",
            Self::TranslationOutOfRange => "transform_translation_out_of_range",
            Self::BoundsOutOfRange => "transform_bounds_out_of_range",
            Self::BudgetExceeded => "transform_budget_exceeded",
            Self::EmptySource => "transform_empty_source",
        }
    }
}

/// A validated transform bound to its source rect: the inverse map and the destination bounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedTransform {
    inverse: Affine2,
    /// The inverse 3x3 of a projective transform (`None` for an affine one).
    projective_inverse: Option<Projective2>,
    filter: ResampleFilter,
    edge: EdgeMode,
    src: Rect,
    bounds: Rect,
}
impl PreparedTransform {
    pub fn new(transform: &LayerTransform, src: Rect) -> Result<Self, TransformError> {
        if src.width == 0 || src.height == 0 {
            return Err(TransformError::EmptySource);
        }
        let (x1, y1) = src.end().map_err(|_| TransformError::BoundsOutOfRange)?;
        let g = match transform.edge {
            EdgeMode::Transparent => transform.filter.footprint_growth(),
            EdgeMode::Clamp => 0.0,
        };
        let (fx0, fy0, fx1, fy1) = (src.x as f64 - g, src.y as f64 - g, x1 as f64 + g, y1 as f64 + g);
        let corners = [(fx0, fy0), (fx1, fy0), (fx0, fy1), (fx1, fy1)];
        let (inverse, projective_inverse) = match transform.perspective {
            None => (transform.affine.inverse()?, None),
            Some(p) => {
                if !p.m.iter().all(|v| v.is_finite()) {
                    return Err(TransformError::NonFinite);
                }
                transform.affine.inverse()?;
                let map = transform.map();
                let scale = map.m[6].abs().max(map.m[7].abs()).max(map.m[8].abs());
                let centre = ((fx0 + fx1) / 2.0, (fy0 + fy1) / 2.0);
                for (x, y) in corners.iter().copied().chain([centre]) {
                    let (_, _, w) = map.apply_h(x, y);
                    if !(w.is_finite() && w > MIN_PERSPECTIVE_WEIGHT * scale) {
                        return Err(TransformError::Singular);
                    }
                    let j = map.jacobian(x, y).ok_or(TransformError::Singular)?;
                    let det = j[0] * j[3] - j[1] * j[2];
                    if !det.is_finite() || det == 0.0 {
                        return Err(TransformError::Singular);
                    }
                    let inv = [j[3] / det, -j[1] / det, -j[2] / det, j[0] / det];
                    if !j.iter().chain(inv.iter()).all(|v| v.is_finite() && v.abs() <= MAX_TRANSFORM_LINEAR) {
                        return Err(TransformError::ScaleOutOfRange);
                    }
                    let (dx, dy) = map.apply(x, y).ok_or(TransformError::Singular)?;
                    if dx.abs() > MAX_TRANSFORM_TRANSLATE || dy.abs() > MAX_TRANSFORM_TRANSLATE {
                        return Err(TransformError::TranslationOutOfRange);
                    }
                }
                (Affine2::IDENTITY, Some(map.inverse()?))
            }
        };
        let map = transform.map();
        let (mut lo_x, mut lo_y, mut hi_x, mut hi_y) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for (x, y) in corners {
            let (dx, dy) = match transform.perspective {
                None => transform.affine.apply(x, y),
                Some(_) => map.apply(x, y).ok_or(TransformError::Singular)?,
            };
            lo_x = lo_x.min(dx);
            lo_y = lo_y.min(dy);
            hi_x = hi_x.max(dx);
            hi_y = hi_y.max(dy);
        }
        // One pixel of slack on every side absorbs the rounding of the forward map; pixels in the
        // slack sample as transparent when they fall outside the footprint.
        let to_i = |v: f64| -> Result<i64, TransformError> {
            if v.is_finite() && v.abs() < 9.0e15 {
                Ok(v as i64)
            } else {
                Err(TransformError::BoundsOutOfRange)
            }
        };
        let x0 = to_i(lo_x.floor() - 1.0)?;
        let y0 = to_i(lo_y.floor() - 1.0)?;
        let xe = to_i(hi_x.ceil() + 1.0)?;
        let ye = to_i(hi_y.ceil() + 1.0)?;
        let side = |a: i64, b: i64| -> Result<u32, TransformError> {
            let w = b.checked_sub(a).ok_or(TransformError::BoundsOutOfRange)?;
            match u32::try_from(w) {
                Ok(w) if w > 0 && w <= MAX_TRANSFORM_BOUNDS_SIDE => Ok(w),
                _ => Err(TransformError::BoundsOutOfRange),
            }
        };
        let bounds = Rect {
            x: x0,
            y: y0,
            width: side(x0, xe)?,
            height: side(y0, ye)?,
        };
        bounds.end().map_err(|_| TransformError::BoundsOutOfRange)?;
        Ok(Self {
            inverse,
            projective_inverse,
            filter: transform.filter,
            edge: transform.edge,
            src,
            bounds,
        })
    }
    /// Destination plan rect outside of which every sample is transparent / zero coverage.
    pub fn bounds(&self) -> Rect {
        self.bounds
    }
    pub fn source(&self) -> Rect {
        self.src
    }
    /// The affine inverse; the identity for a projective transform (see
    /// [`PreparedTransform::projective_inverse`]).
    pub fn inverse(&self) -> Affine2 {
        self.inverse
    }
    /// The inverse 3x3 of a projective transform.
    pub fn projective_inverse(&self) -> Option<Projective2> {
        self.projective_inverse
    }
    pub fn filter(&self) -> ResampleFilter {
        self.filter
    }
    pub fn edge(&self) -> EdgeMode {
        self.edge
    }

    /// Continuous texel-index coordinates `(u, v)` of destination pixel `(x, y)`'s centre.
    /// For a projective transform a centre beyond the horizon (homogeneous weight not positive) is
    /// `(NaN, NaN)`, which samples as transparent.
    pub fn sample_position(&self, x: i64, y: i64) -> (f64, f64) {
        let (sx, sy) = match &self.projective_inverse {
            None => self.inverse.apply(x as f64 + 0.5, y as f64 + 0.5),
            Some(p) => p.apply(x as f64 + 0.5, y as f64 + 0.5).unwrap_or((f64::NAN, f64::NAN)),
        };
        (sx - self.src.x as f64 - 0.5, sy - self.src.y as f64 - 0.5)
    }

    /// The taps and weights of destination pixel `(x, y)`, or `None` when it is transparent.
    fn taps(&self, x: i64, y: i64) -> Taps {
        let (u, v) = self.sample_position(x, y);
        let (w, h) = (f64::from(self.src.width), f64::from(self.src.height));
        if !(u.is_finite() && v.is_finite()) {
            return Taps::None;
        }
        // Rule 1: exact centre.
        if u.fract() == 0.0 && v.fract() == 0.0 && u >= 0.0 && v >= 0.0 && u < w && v < h {
            return Taps::Exact(u as u32, v as u32);
        }
        if self.edge == EdgeMode::Clamp && !(u >= -0.5 && u < w - 0.5 && v >= -0.5 && v < h - 0.5) {
            return Taps::None;
        }
        match self.filter {
            ResampleFilter::NearestNeighbour => {
                let (i, j) = ((u + 0.5).floor(), (v + 0.5).floor());
                if i >= 0.0 && j >= 0.0 && i < w && j < h {
                    Taps::Exact(i as u32, j as u32)
                } else {
                    Taps::None
                }
            }
            ResampleFilter::Bilinear | ResampleFilter::Bicubic => {
                let (x0, y0) = (u.floor(), v.floor());
                let (tx, ty) = (u - x0, v - y0);
                let (wx, wy, first, n) = if self.filter == ResampleFilter::Bilinear {
                    ([1.0 - tx, tx, 0.0, 0.0], [1.0 - ty, ty, 0.0, 0.0], 0.0, 2)
                } else {
                    (catmull_rom(tx), catmull_rom(ty), -1.0, 4)
                };
                // Every tap outside the source (Transparent): nothing to read. This also keeps the
                // i64 casts below small (a cast saturates, the tap loop adds with saturation).
                let (fx, fy, span) = (x0 + first, y0 + first, n as f64);
                if fx + span <= 0.0 || fy + span <= 0.0 || fx >= w || fy >= h {
                    return Taps::None;
                }
                Taps::Filtered {
                    x0: fx as i64,
                    y0: fy as i64,
                    wx,
                    wy,
                    n,
                    tx,
                    ty,
                }
            }
        }
    }

    /// The sampling decision of destination pixel `(x, y)` (see [`SampleSite`]).
    pub fn sample_site(&self, x: i64, y: i64) -> SampleSite {
        match self.taps(x, y) {
            Taps::None => SampleSite::Transparent,
            Taps::Exact(column, row) => SampleSite::Texel { column, row },
            Taps::Filtered { x0, y0, tx, ty, .. } => SampleSite::Filtered {
                first_column: x0,
                first_row: y0,
                tx,
                ty,
            },
        }
    }

    fn tap_index(&self, i: i64, len: u32) -> Option<u32> {
        match self.edge {
            EdgeMode::Transparent => u32::try_from(i).ok().filter(|v| *v < len),
            EdgeMode::Clamp => {
                let max = i64::from(len.saturating_sub(1));
                u32::try_from(i.clamp(0, max)).ok()
            }
        }
    }

    /// Straight RGBA of destination pixel `(x, y)` (rules 1-5); `texel(column, row)` reads the
    /// source in straight RGBA, every component finite and alpha in `[0, 1]`.
    pub fn sample_rgba<E>(
        &self,
        x: i64,
        y: i64,
        texel: impl Fn(u32, u32) -> Result<[f32; 4], E>,
    ) -> Result<[f32; 4], E> {
        self.sample_rgba_in(crate::render_plan::SampleRange::Unit, x, y, texel)
    }

    /// [`PreparedTransform::sample_rgba`] in `range` (lane SMB-PIGMENT-EXTENDED-RANGE-2): the
    /// extended range keeps the filtered colour `P / A` unclamped (bilinear is convex; the
    /// Catmull-Rom overshoot is the kernel's own, no unit white point).
    pub fn sample_rgba_in<E>(
        &self,
        range: crate::render_plan::SampleRange,
        x: i64,
        y: i64,
        texel: impl Fn(u32, u32) -> Result<[f32; 4], E>,
    ) -> Result<[f32; 4], E> {
        let colour = |v: f64| {
            if range == crate::render_plan::SampleRange::Extended {
                v as f32
            } else {
                v.clamp(0.0, 1.0) as f32
            }
        };
        match self.taps(x, y) {
            Taps::None => Ok([0.0; 4]),
            Taps::Exact(i, j) => texel(i, j),
            Taps::Filtered {
                x0, y0, wx, wy, n, ..
            } => {
                let (mut a, mut p) = (0.0f64, [0.0f64; 3]);
                for (dj, wyj) in wy.iter().take(n).enumerate() {
                    let Some(j) = self.tap_index(y0.saturating_add(dj as i64), self.src.height)
                    else {
                        continue;
                    };
                    for (di, wxi) in wx.iter().take(n).enumerate() {
                        let Some(i) = self.tap_index(x0.saturating_add(di as i64), self.src.width)
                        else {
                            continue;
                        };
                        let t = texel(i, j)?;
                        let w = wxi * wyj;
                        let wa = w * f64::from(t[3]);
                        a += wa;
                        for (pc, c) in p.iter_mut().zip(t) {
                            *pc += wa * f64::from(c);
                        }
                    }
                }
                if a <= 0.0 {
                    return Ok([0.0; 4]);
                }
                Ok([colour(p[0] / a), colour(p[1] / a), colour(p[2] / a), a.min(1.0) as f32])
            }
        }
    }

    /// Coverage of destination pixel `(x, y)` from a coverage plane over the source rect (stored
    /// values, before invert and density).
    pub fn sample_coverage<E>(
        &self,
        x: i64,
        y: i64,
        value: impl Fn(u32, u32) -> Result<f32, E>,
    ) -> Result<f32, E> {
        match self.taps(x, y) {
            Taps::None => Ok(0.0),
            Taps::Exact(i, j) => value(i, j),
            Taps::Filtered {
                x0, y0, wx, wy, n, ..
            } => {
                let mut sum = 0.0f64;
                for (dj, wyj) in wy.iter().take(n).enumerate() {
                    let Some(j) = self.tap_index(y0.saturating_add(dj as i64), self.src.height)
                    else {
                        continue;
                    };
                    for (di, wxi) in wx.iter().take(n).enumerate() {
                        let Some(i) = self.tap_index(x0.saturating_add(di as i64), self.src.width)
                        else {
                            continue;
                        };
                        sum += wxi * wyj * f64::from(value(i, j)?);
                    }
                }
                Ok(sum.clamp(0.0, 1.0) as f32)
            }
        }
    }
}

enum Taps {
    None,
    Exact(u32, u32),
    Filtered {
        x0: i64,
        y0: i64,
        wx: [f64; 4],
        wy: [f64; 4],
        n: usize,
        tx: f64,
        ty: f64,
    },
}

/// Where destination pixel `(x, y)` samples the source ([`PreparedTransform::sample_site`]): the
/// geometry half of the reference, exposed so a backend that evaluates the filter itself (render-gpu,
/// binary32) reads the same decisions (rules 1, 2 and 5) and binary64 positions as this module.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SampleSite {
    /// Transparent / zero coverage.
    Transparent,
    /// Exactly this texel (rule 1, or nearest).
    Texel { column: u32, row: u32 },
    /// Bilinear (2 taps per axis) or bicubic (4) from `first_column`, `first_row`; `tx`, `ty` in
    /// `[0, 1)` are the fractions the weights are taken at ([`catmull_rom`] for bicubic, `1 - t, t`
    /// for bilinear). Taps outside the source follow the edge mode.
    Filtered {
        first_column: i64,
        first_row: i64,
        tx: f64,
        ty: f64,
    },
}

/// Catmull-Rom weights of taps `-1, 0, 1, 2` at fraction `t` in `[0, 1)`.
pub fn catmull_rom(t: f64) -> [f64; 4] {
    let t2 = t * t;
    let t3 = t2 * t;
    [
        (-t3 + 2.0 * t2 - t) * 0.5,
        (3.0 * t3 - 5.0 * t2 + 2.0) * 0.5,
        (-3.0 * t3 + 4.0 * t2 + t) * 0.5,
        (t3 - t2) * 0.5,
    ]
}

/// Reference resample: the straight RGBA of every pixel of `region` (row-major), source read with
/// `texel`. The output area is checked against [`MAX_REFERENCE_PIXELS`] before allocation.
pub fn resample_reference(
    transform: &LayerTransform,
    src: Rect,
    region: Rect,
    texel: impl Fn(u32, u32) -> [f32; 4],
) -> Result<Vec<[f32; 4]>, TransformError> {
    let prepared = PreparedTransform::new(transform, src)?;
    let area = u64::from(region.width)
        .checked_mul(u64::from(region.height))
        .ok_or(TransformError::BudgetExceeded)?;
    if area > MAX_REFERENCE_PIXELS {
        return Err(TransformError::BudgetExceeded);
    }
    region.end().map_err(|_| TransformError::BoundsOutOfRange)?;
    let mut out = Vec::with_capacity(usize::try_from(area).map_err(|_| TransformError::BudgetExceeded)?);
    for dy in 0..i64::from(region.height) {
        for dx in 0..i64::from(region.width) {
            let (x, y) = (region.x.saturating_add(dx), region.y.saturating_add(dy));
            let px = prepared.sample_rgba(x, y, |i, j| Ok::<_, TransformError>(texel(i, j)))?;
            out.push(px);
        }
    }
    Ok(out)
}
