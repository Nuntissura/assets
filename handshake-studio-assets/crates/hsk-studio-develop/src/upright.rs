//! Automatic perspective correction (lane SMB-DEVELOP-LENS-PRESETS slice 2; STU-RAW-118
//! `auto_perspective_*`): line detection, vanishing points and camera-rotation rectification. The
//! analysis computes all six candidates the spec stores (off, auto, level, vertical, full, guided) so a
//! mode switch needs no re-analysis.
//!
//! Method (published techniques, this crate's own implementation; Lightroom's Upright results are
//! UNVERIFIED, D5):
//! 1. Line detection: encoded luma, box-downsampled so the long side is at most 512; Sobel gradients;
//!    pixels with magnitude above `max(0.03, 0.2 * max)` vote in a (theta, rho) Hough accumulator
//!    (Duda & Hart 1972) of 0.5-degree by 1-pixel bins, each pixel only within +-3 degrees of its
//!    gradient orientation (O'Gorman & Clowes 1976); peaks with at least `max(10, 0.15 * short side)`
//!    votes are taken greedily (at most 32) with a +-2 degree / +-6 pixel suppression window.
//! 2. Lines within 30 degrees of vertical (horizontal) estimate the vertical (horizontal) vanishing
//!    point as the least-squares null vector of the vote-weighted line matrix (smallest eigenvector,
//!    cyclic Jacobi), so parallel lines give a point at infinity.
//! 3. Camera model: focal length `f = focal_35mm / 43.2666 * diagonal` (35 mm equivalent on the
//!    diagonal; spec fallback 35 mm), principal point = `auto_perspective_centre`. A vanishing
//!    point's direction `K^-1 v` is rotated onto the camera axis; the correction is `K R K^-1`
//!    (a pure camera rotation, so no shear is invented).
//! 4. Candidates: level = roll only (vote-weighted median deviation of the near-vertical and
//!    near-horizontal lines within 15 degrees); vertical = the smallest rotation taking the vertical
//!    direction to the image y axis; full = the rotation taking the vertical direction to y and the
//!    horizontal one (orthogonalised) to x; auto = full when both exist and the rotation is at most
//!    20 degrees, else vertical under the same limit, else level (this crate's balancing rule);
//!    guided = the same from the caller's guide segments (two or more near-vertical guides give the
//!    vertical point, two or more near-horizontal the horizontal one, a single guide levels).
//!    A candidate with no evidence is the identity.
//! 5. Every candidate keeps the principal point fixed and has unit area scale there (Jacobian
//!    determinant 1), so the picture neither drifts nor zooms.
use crate::DevelopError;
use crate::adjust::enc;
use crate::geometry::{Frame, Geometry, IDENTITY_3X3};
use hsk_studio_pigment::transform_math::Projective2;

/// One detected line in normalised frame coordinates: `cos(theta) x + sin(theta) y = rho`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DetectedLine {
    pub theta: f64,
    pub rho: f64,
    pub votes: u32,
}

/// Result of an upright analysis.
#[derive(Clone, Debug, PartialEq)]
pub struct UprightAnalysis {
    /// Candidates in [`crate::UprightMode::index`] order (off, auto, level, vertical, full, guided),
    /// row-major 3x3 in normalised frame coordinates.
    pub transforms: [[f64; 9]; 6],
    pub lines: Vec<DetectedLine>,
    /// Homogeneous vanishing points (normalised coordinates), when found.
    pub vertical_vanishing_point: Option<[f64; 3]>,
    pub horizontal_vanishing_point: Option<[f64; 3]>,
}

const MAX_SIDE: usize = 512;
const THETA_BINS: usize = 360;
const MAX_LINES: usize = 32;
const NEAR: f64 = 0.5; // sin(30 degrees)
const AUTO_LIMIT_DEGREES: f64 = 20.0;

type V3 = [f64; 3];
type M3 = [[f64; 3]; 3];
const I3: M3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

fn dot(a: V3, b: V3) -> f64 {
    (a[0] * b[0] + a[1] * b[1]) + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn norm(a: V3) -> Option<V3> {
    let l = dot(a, a).sqrt();
    (l.is_finite() && l > 1e-300).then(|| [a[0] / l, a[1] / l, a[2] / l])
}
fn mm(a: &M3, b: &M3) -> M3 {
    std::array::from_fn(|r| std::array::from_fn(|c| (a[r][0] * b[0][c] + a[r][1] * b[1][c]) + a[r][2] * b[2][c]))
}
fn mv(a: &M3, v: V3) -> V3 {
    std::array::from_fn(|r| (a[r][0] * v[0] + a[r][1] * v[1]) + a[r][2] * v[2])
}
fn flat(m: &M3) -> [f64; 9] {
    [m[0][0], m[0][1], m[0][2], m[1][0], m[1][1], m[1][2], m[2][0], m[2][1], m[2][2]]
}

/// Rodrigues rotation taking unit `a` onto unit `b`.
fn rotate_onto(a: V3, b: V3) -> M3 {
    let c = dot(a, b).clamp(-1.0, 1.0);
    let Some(k) = norm(cross(a, b)) else {
        return I3;
    };
    let s = (1.0 - c * c).max(0.0).sqrt();
    let kx: M3 = [[0.0, -k[2], k[1]], [k[2], 0.0, -k[0]], [-k[1], k[0], 0.0]];
    let kk = mm(&kx, &kx);
    std::array::from_fn(|r| std::array::from_fn(|col| I3[r][col] + s * kx[r][col] + (1.0 - c) * kk[r][col]))
}

fn rotation_angle(r: &M3) -> f64 {
    (((r[0][0] + r[1][1] + r[2][2]) - 1.0) / 2.0).clamp(-1.0, 1.0).acos().to_degrees()
}

/// Smallest-eigenvalue eigenvector of a symmetric 3x3 matrix (cyclic Jacobi).
fn smallest_eigenvector(mut a: M3) -> Option<V3> {
    let mut v = I3;
    for _ in 0..64 {
        let off = a[0][1].abs() + a[0][2].abs() + a[1][2].abs();
        let scale = a[0][0].abs() + a[1][1].abs() + a[2][2].abs();
        if off <= 1e-15 * scale || off < 1e-300 {
            break;
        }
        for (p, q) in [(0usize, 1usize), (0, 2), (1, 2)] {
            if a[p][q].abs() < 1e-300 {
                continue;
            }
            let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
            let t = if theta == 0.0 { 1.0 } else { theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt()) };
            let c = 1.0 / (t * t + 1.0).sqrt();
            let s = t * c;
            let mut j = I3;
            j[p][p] = c;
            j[q][q] = c;
            j[p][q] = s;
            j[q][p] = -s;
            let jt: M3 = std::array::from_fn(|r| std::array::from_fn(|col| j[col][r]));
            a = mm(&jt, &mm(&a, &j));
            v = mm(&v, &j);
        }
    }
    let k = (0..3).min_by(|x, y| a[*x][*x].total_cmp(&a[*y][*y]))?;
    let e = [v[0][k], v[1][k], v[2][k]];
    e.iter().all(|x| x.is_finite()).then_some(e)
}

/// Least-squares vanishing point of weighted homogeneous lines (each `(a, b)` unit).
fn vanishing_point(lines: &[(V3, f64)]) -> Option<V3> {
    if lines.len() < 2 {
        return None;
    }
    let mut m: M3 = [[0.0; 3]; 3];
    for (l, w) in lines {
        for (r, row) in m.iter_mut().enumerate() {
            for (c, cell) in row.iter_mut().enumerate() {
                *cell += w * l[r] * l[c];
            }
        }
    }
    smallest_eigenvector(m)
}

fn weighted_median(mut items: Vec<(f64, f64)>) -> Option<f64> {
    if items.is_empty() {
        return None;
    }
    items.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total: f64 = items.iter().map(|i| i.1).sum();
    let mut acc = 0.0;
    for (v, w) in &items {
        acc += w;
        if acc >= total / 2.0 {
            return Some(*v);
        }
    }
    items.last().map(|i| i.0)
}

/// Signed deviation (degrees) of a line from its nearest axis; positive = the line is turned
/// clockwise on screen (y down) from that axis.
fn deviation(l: V3) -> f64 {
    // Line direction (-b, a); a near-vertical line is oriented down (+y), a near-horizontal +x.
    let (mut dx, mut dy) = (-l[1], l[0]);
    if dy.abs() >= dx.abs() {
        if dy < 0.0 {
            (dx, dy) = (-dx, -dy);
        }
        // A clockwise turn (on screen) of a downward vector moves it toward -x.
        (-dx).atan2(dy).to_degrees()
    } else {
        if dx < 0.0 {
            (dx, dy) = (-dx, -dy);
        }
        dy.atan2(dx).to_degrees()
    }
}

struct Camera {
    k: M3,
    k_inv: M3,
    principal: (f64, f64),
}

impl Camera {
    fn new(frame: &Frame, g: &Geometry) -> Self {
        let diag = frame.width.hypot(frame.height) / frame.unit;
        let f = g.upright_focal_length_35mm / 43.266_615 * diag;
        let (px, py) = ((g.upright_centre.0 - 0.5) * frame.width / frame.unit, (g.upright_centre.1 - 0.5) * frame.height / frame.unit);
        Self {
            k: [[f, 0.0, px], [0.0, f, py], [0.0, 0.0, 1.0]],
            k_inv: [[1.0 / f, 0.0, -px / f], [0.0, 1.0 / f, -py / f], [0.0, 0.0, 1.0]],
            principal: (px, py),
        }
    }

    fn direction(&self, vp: V3, target: V3) -> Option<V3> {
        let d = norm(mv(&self.k_inv, vp))?;
        Some(if dot(d, target) < 0.0 { [-d[0], -d[1], -d[2]] } else { d })
    }

    /// `K R K^-1`, normalised to keep the principal point fixed with unit area scale there.
    fn homography(&self, r: &M3) -> [f64; 9] {
        let h = Projective2 { m: flat(&mm(&self.k, &mm(r, &self.k_inv))) };
        let (px, py) = self.principal;
        let Some((qx, qy)) = h.apply(px, py) else { return IDENTITY_3X3 };
        let e = 1e-6;
        let at = |x: f64, y: f64| h.apply(x, y).unwrap_or((f64::NAN, f64::NAN));
        let (a, b) = (at(px + e, py), at(px - e, py));
        let (c, d) = (at(px, py + e), at(px, py - e));
        let det = ((a.0 - b.0) * (c.1 - d.1) - (a.1 - b.1) * (c.0 - d.0)) / (4.0 * e * e);
        if !det.is_finite() || det <= 0.0 {
            return IDENTITY_3X3;
        }
        let s = 1.0 / det.sqrt();
        let fix = Projective2 { m: [s, 0.0, px - s * qx, 0.0, s, py - s * qy, 0.0, 0.0, 1.0] };
        let out = h.then(&fix).m;
        if out.iter().all(|v| v.is_finite()) { out } else { IDENTITY_3X3 }
    }

    /// Camera roll that turns the picture by `degrees` (positive clockwise on screen).
    fn roll(&self, degrees: f64) -> M3 {
        let (s, c) = degrees.to_radians().sin_cos();
        [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]]
    }
}

struct Evidence {
    vertical: Vec<(V3, f64)>,
    horizontal: Vec<(V3, f64)>,
}

impl Evidence {
    fn from_lines(lines: impl Iterator<Item = (V3, f64)>) -> Self {
        let (mut vertical, mut horizontal) = (Vec::new(), Vec::new());
        for (l, w) in lines {
            if l[1].abs() <= NEAR {
                vertical.push((l, w));
            } else if l[0].abs() <= NEAR {
                horizontal.push((l, w));
            }
        }
        Self { vertical, horizontal }
    }

    fn level(&self) -> Option<f64> {
        let items = self.vertical.iter().chain(&self.horizontal).map(|(l, w)| (deviation(*l), *w)).filter(|(d, _)| d.abs() <= 15.0).collect();
        weighted_median(items)
    }

    /// (level, vertical, full) rotations; `None` where the evidence is missing.
    fn rotations(&self, cam: &Camera, single_guide_levels: bool) -> (Option<M3>, Option<M3>, Option<M3>) {
        let level = if single_guide_levels || self.vertical.len() + self.horizontal.len() > 0 {
            self.level().map(|d| cam.roll(-d))
        } else {
            None
        };
        let dv = vanishing_point(&self.vertical).and_then(|v| cam.direction(v, [0.0, 1.0, 0.0]));
        let dh = vanishing_point(&self.horizontal).and_then(|v| cam.direction(v, [1.0, 0.0, 0.0]));
        let vertical = dv.map(|d| rotate_onto(d, [0.0, 1.0, 0.0]));
        let full = match (dv, dh) {
            (Some(y), Some(h)) => {
                let k = dot(h, y);
                norm([h[0] - k * y[0], h[1] - k * y[1], h[2] - k * y[2]]).map(|x| {
                    let z = cross(x, y);
                    // Rows x, y, z take those directions onto the axes; keep the view direction forward.
                    if z[2] < 0.0 { [[-x[0], -x[1], -x[2]], y, [-z[0], -z[1], -z[2]]] } else { [x, y, z] }
                })
            }
            _ => None,
        };
        (level, vertical, full)
    }
}

fn line_from_points(a: (f64, f64), b: (f64, f64)) -> Option<V3> {
    let l = cross([a.0, a.1, 1.0], [b.0, b.1, 1.0]);
    let n = l[0].hypot(l[1]);
    (n.is_finite() && n > 1e-12).then(|| [l[0] / n, l[1] / n, l[2] / n])
}

/// Detect lines in a linear RGB frame (`width x height`, 3 x f32).
fn detect(rgb: &[f32], width: usize, height: usize, luma: [f64; 3], frame: &Frame) -> Vec<DetectedLine> {
    let k = width.max(height).div_ceil(MAX_SIDE).max(1);
    let (dw, dh) = (width / k, height / k);
    if dw < 8 || dh < 8 {
        return Vec::new();
    }
    let mut plane = vec![0.0f64; dw * dh];
    for (i, slot) in plane.iter_mut().enumerate() {
        let (x, y) = (i % dw, i / dw);
        let mut sum = 0.0;
        for yy in y * k..(y + 1) * k {
            for xx in x * k..(x + 1) * k {
                let p = &rgb[(yy * width + xx) * 3..(yy * width + xx) * 3 + 3];
                sum += enc((luma[0] * f64::from(p[0]) + luma[1] * f64::from(p[1])) + luma[2] * f64::from(p[2]));
            }
        }
        *slot = sum / (k * k) as f64;
    }
    let mut grads = Vec::new();
    let mut max_mag = 0.0f64;
    for y in 1..dh - 1 {
        for x in 1..dw - 1 {
            let at = |dx: usize, dy: usize| plane[(y + dy - 1) * dw + (x + dx - 1)];
            let gx = (at(2, 0) + 2.0 * at(2, 1) + at(2, 2)) - (at(0, 0) + 2.0 * at(0, 1) + at(0, 2));
            let gy = (at(0, 2) + 2.0 * at(1, 2) + at(2, 2)) - (at(0, 0) + 2.0 * at(1, 0) + at(2, 0));
            let m = gx.hypot(gy) / 8.0;
            max_mag = max_mag.max(m);
            grads.push((x, y, gx, gy, m));
        }
    }
    let threshold = (0.2 * max_mag).max(0.03);
    let half_diag = ((dw * dw + dh * dh) as f64).sqrt() / 2.0;
    let rho_off = half_diag.ceil();
    let rho_bins = 2 * rho_off as usize + 1;
    let mut acc = vec![0u32; THETA_BINS * rho_bins];
    let (cx, cy) = (dw as f64 / 2.0, dh as f64 / 2.0);
    let step = std::f64::consts::PI / THETA_BINS as f64;
    for &(x, y, gx, gy, m) in &grads {
        if m < threshold {
            continue;
        }
        let phi = gy.atan2(gx).rem_euclid(std::f64::consts::PI);
        let centre = (phi / step).round() as i64;
        let (px, py) = (x as f64 + 0.5 - cx, y as f64 + 0.5 - cy);
        for d in -6i64..=6 {
            let raw = centre + d;
            let t = raw.rem_euclid(THETA_BINS as i64) as usize;
            let theta = t as f64 * step;
            let rho = px * theta.cos() + py * theta.sin();
            let r = (rho + rho_off).round();
            if r >= 0.0 && (r as usize) < rho_bins {
                acc[t * rho_bins + r as usize] += 1;
            }
        }
    }
    let min_votes = (0.15 * dw.min(dh) as f64).max(10.0) as u32;
    let mut lines = Vec::new();
    while lines.len() < MAX_LINES {
        let (best, votes) = acc.iter().enumerate().fold((0usize, 0u32), |b, (i, v)| if *v > b.1 { (i, *v) } else { b });
        if votes < min_votes {
            break;
        }
        let (t, r) = (best / rho_bins, best % rho_bins);
        // Sub-bin refinement: vote-weighted mean over the 3x3 neighbourhood (no wrap needed inside).
        let (mut st, mut sr, mut sw) = (0.0, 0.0, 0.0);
        for dt in -1i64..=1 {
            for dr in -1i64..=1 {
                let (tt, rr) = (t as i64 + dt, r as i64 + dr);
                if tt >= 0 && (tt as usize) < THETA_BINS && rr >= 0 && (rr as usize) < rho_bins {
                    let w = f64::from(acc[tt as usize * rho_bins + rr as usize]);
                    st += w * tt as f64;
                    sr += w * rr as f64;
                    sw += w;
                }
            }
        }
        let theta = st / sw * step;
        let rho = sr / sw - rho_off;
        lines.push(DetectedLine { theta, rho: rho * k as f64 / frame.unit, votes });
        for dt in -4i64..=4 {
            let tt = t as i64 + dt;
            let (tt, flip) = if tt < 0 {
                (tt + THETA_BINS as i64, true)
            } else if tt >= THETA_BINS as i64 {
                (tt - THETA_BINS as i64, true)
            } else {
                (tt, false)
            };
            let rc = if flip { rho_bins as i64 - 1 - r as i64 } else { r as i64 };
            for dr in -6i64..=6 {
                let rr = rc + dr;
                if rr >= 0 && (rr as usize) < rho_bins {
                    acc[tt as usize * rho_bins + rr as usize] = 0;
                }
            }
        }
    }
    lines
}

/// Analyse a lens-corrected linear frame and compute the six candidate transforms.
pub fn analyse(rgb: &[f32], width: u32, height: u32, geometry: &Geometry, luma: [f64; 3]) -> Result<UprightAnalysis, DevelopError> {
    let (w, h) = (width as usize, height as usize);
    let n = w.checked_mul(h).ok_or(DevelopError::Overflow)?;
    if w == 0 || h == 0 || n.checked_mul(3) != Some(rgb.len()) {
        return Err(DevelopError::InvalidDimensions);
    }
    if rgb.iter().any(|v| !v.is_finite()) {
        return Err(DevelopError::NonFinite);
    }
    crate::geometry::PreparedGeometry::prepare(geometry, false)?;
    let frame = Frame::new(width, height);
    let cam = Camera::new(&frame, geometry);
    let lines = detect(rgb, w, h, luma, &frame);
    let evidence = Evidence::from_lines(lines.iter().map(|l| ([l.theta.cos(), l.theta.sin(), -l.rho], f64::from(l.votes))));
    let (level, vertical, full) = evidence.rotations(&cam, false);
    let to = |r: Option<M3>| r.map_or(IDENTITY_3X3, |r| cam.homography(&r));
    let auto = match (full, vertical) {
        (Some(f), _) if rotation_angle(&f) <= AUTO_LIMIT_DEGREES => Some(f),
        (_, Some(v)) if rotation_angle(&v) <= AUTO_LIMIT_DEGREES => Some(v),
        _ => level,
    };
    let guided = {
        let to_norm = |p: [f64; 2]| frame.to_norm(p[0] * frame.width, p[1] * frame.height);
        let guides = Evidence::from_lines(
            geometry.upright_guides.iter().filter_map(|s| line_from_points(to_norm(s[0]), to_norm(s[1]))).map(|l| (l, 1.0)),
        );
        let (gl, gv, gf) = guides.rotations(&cam, true);
        gf.or(gv).or(gl)
    };
    Ok(UprightAnalysis {
        transforms: [IDENTITY_3X3, to(auto), to(level), to(vertical), to(full), to(guided)],
        vertical_vanishing_point: vanishing_point(&evidence.vertical),
        horizontal_vanishing_point: vanishing_point(&evidence.horizontal),
        lines,
    })
}
