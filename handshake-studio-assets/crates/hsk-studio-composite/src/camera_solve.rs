//! 3D camera solve (lane SMB-AE-3D slice 4; spec 14.27.4 table, STU-CMP-065: "3D camera solve:
//! analysing footage to recover a camera path and a sparse 3D point cloud, from which a camera
//! layer and target layers can be created ... its parameter set was not recovered", gap
//! STU-CMP-065b). The parameter set below is therefore a Studio definition, not a recovered one.
//!
//! Input: point tracks in composition pixels (point-major, one optional position per frame; the
//! tracker of `hsk_studio_pigment::track_math` produces them) and the camera zoom in pixels with the
//! principal point at the composition centre (calibrated pinhole, the camera model of
//! [`crate::three_d`]). Method (textbook calibrated structure from motion, Hartley & Zisserman,
//! Multiple View Geometry, 2nd ed., ch. 9-12): the key pair is frame 0 and the frame with the
//! largest median track displacement; the essential matrix by the normalised eight-point algorithm
//! with the rank-two, equal-singular-value projection; the four (R, t) decompositions disambiguated
//! by cheirality; linear (DLT) triangulation; every other frame by a linear DLT camera resection
//! projected to a rotation and refined by Gauss-Newton on the reprojection error; points
//! re-triangulated over all solved frames and refined, alternating with pose refinement
//! ([`REFINE_ROUNDS`] passes; a block-coordinate reading of bundle adjustment).
//!
//! Output gauge (Studio reading): frame 0's camera is the composition's default-like camera at
//! `(w/2, h/2, -zoom)` looking along +Z, and the scene is scaled so that the median depth of the
//! points from that camera equals `zoom` (the cloud sits near the plane z = 0, which maps one to
//! one). [`camera_layer`] writes the path as a one-node camera layer (position X/Y and rotation Z
//! streams, Z position and X/Y rotation in `three_d`, unwrapped degrees, linear keys per frame);
//! [`target_layer`] writes one solved point as a 3D null layer.
#![allow(clippy::many_single_char_names)]

use crate::three_d::{Mat3, Vec3, mat_mul, mat_vec, transpose};
use crate::track_apply::TrackTiming;
use hsk_studio_folio::{
    StudioCameraOptions, StudioCompositionKeyframe, StudioCompositionLayer, StudioInterpolation, StudioKeyedValue,
    StudioLayer3d, StudioLayerProperty, StudioLayerTiming, StudioPropertyStream,
};
use std::collections::BTreeMap;

/// Most tracked points one solve accepts.
pub const MAX_SOLVE_POINTS: usize = 4096;
/// Most frames one solve accepts.
pub const MAX_SOLVE_FRAMES: usize = 4096;
/// Alternating point / pose refinement passes after the initial solve.
pub const REFINE_ROUNDS: usize = 6;
/// Fewest points a frame must share with the solved cloud.
pub const MIN_FRAME_POINTS: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraSolveError {
    /// Shape, non-finite value, zoom or limits (checked before any work).
    Parameter,
    /// Fewer than eight points shared by frame 0 and any other frame.
    TooFewPoints,
    /// No frame moves the camera enough against frame 0 (pure rotation or a still camera).
    NoParallax,
    /// A frame shares fewer than [`MIN_FRAME_POINTS`] points with the solved cloud.
    FrameUnderconstrained(usize),
    /// A singular configuration (degenerate essential matrix or resection).
    Degenerate,
    /// A key time outside `u64`.
    TickOverflow,
}
impl CameraSolveError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Parameter => "camera_solve_parameter",
            Self::TooFewPoints => "camera_solve_too_few_points",
            Self::NoParallax => "camera_solve_no_parallax",
            Self::FrameUnderconstrained(_) => "camera_solve_frame_underconstrained",
            Self::Degenerate => "camera_solve_degenerate",
            Self::TickOverflow => "camera_solve_tick_overflow",
        }
    }
}

/// The solve's camera at one frame, in composition space ([`crate::three_d`] conventions).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SolvedCamera {
    pub position: Vec3,
    /// Camera to world (columns: screen right, screen down, forward).
    pub rotation: Mat3,
    /// `rotation = Rz(rz) Ry(ry) Rx(rx)` in degrees, unwrapped along the path.
    pub rx: f64,
    pub ry: f64,
    pub rz: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CameraSolve {
    pub zoom: f64,
    pub cameras: Vec<SolvedCamera>,
    /// One per input track; `None` when the point was seen by fewer than two solved frames.
    pub points: Vec<Option<Vec3>>,
    /// Root-mean-square reprojection error over every observation of a solved point, in pixels.
    pub rms_px: f64,
}

// ---- small dense linear algebra ----

/// Eigen-decomposition of a symmetric matrix by cyclic Jacobi rotations: (eigenvalues, eigenvectors
/// as columns of `v`, row-major `n x n`).
fn jacobi(a: &[f64], n: usize) -> (Vec<f64>, Vec<f64>) {
    let mut a = a.to_vec();
    let mut v = vec![0.0; n * n];
    for i in 0..n {
        v[i * n + i] = 1.0;
    }
    for _ in 0..100 {
        let mut off = 0.0;
        for p in 0..n {
            for q in p + 1..n {
                off += a[p * n + q] * a[p * n + q];
            }
        }
        if off < 1e-300 {
            break;
        }
        for p in 0..n {
            for q in p + 1..n {
                let apq = a[p * n + q];
                if apq.abs() < 1e-300 {
                    continue;
                }
                let theta = (a[q * n + q] - a[p * n + p]) / (2.0 * apq);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let t = if theta == 0.0 { 1.0 } else { t };
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for k in 0..n {
                    let akp = a[k * n + p];
                    let akq = a[k * n + q];
                    a[k * n + p] = c * akp - s * akq;
                    a[k * n + q] = s * akp + c * akq;
                }
                for k in 0..n {
                    let apk = a[p * n + k];
                    let aqk = a[q * n + k];
                    a[p * n + k] = c * apk - s * aqk;
                    a[q * n + k] = s * apk + c * aqk;
                }
                for k in 0..n {
                    let vkp = v[k * n + p];
                    let vkq = v[k * n + q];
                    v[k * n + p] = c * vkp - s * vkq;
                    v[k * n + q] = s * vkp + c * vkq;
                }
            }
        }
    }
    ((0..n).map(|i| a[i * n + i]).collect(), v)
}

/// The unit vector minimising `|A x|` (rows of length `n`): the eigenvector of `A^T A` with the
/// smallest eigenvalue.
fn null_vector(rows: &[Vec<f64>], n: usize) -> Vec<f64> {
    let mut ata = vec![0.0; n * n];
    for r in rows {
        for i in 0..n {
            for j in 0..n {
                ata[i * n + j] += r[i] * r[j];
            }
        }
    }
    let (vals, vecs) = jacobi(&ata, n);
    let k = (0..n).min_by(|a, b| vals[*a].total_cmp(&vals[*b])).unwrap_or(0);
    (0..n).map(|i| vecs[i * n + k]).collect()
}

fn det3(m: &Mat3) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}
fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn dot(a: Vec3, b: Vec3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn col(m: &Mat3, j: usize) -> Vec3 {
    [m[0][j], m[1][j], m[2][j]]
}
fn from_cols(a: Vec3, b: Vec3, c: Vec3) -> Mat3 {
    [[a[0], b[0], c[0]], [a[1], b[1], c[1]], [a[2], b[2], c[2]]]
}

/// SVD of a 3x3 matrix: (U, singular values descending, V) with U, V proper rotations or
/// reflections as computed (`m = U diag(s) V^T`); U's third column is `u0 x u1`.
fn svd3(m: &Mat3) -> Option<(Mat3, Vec3, Mat3)> {
    let mtm = mat_mul(&transpose(m), m);
    let flat: Vec<f64> = mtm.iter().flatten().copied().collect();
    let (vals, vecs) = jacobi(&flat, 3);
    let mut order = [0usize, 1, 2];
    order.sort_by(|a, b| vals[*b].total_cmp(&vals[*a]));
    let v_col = |k: usize| -> Vec3 { [vecs[k], vecs[3 + k], vecs[6 + k]] };
    let (v0, v1) = (v_col(order[0]), v_col(order[1]));
    let v2 = cross(v0, v1);
    let s: Vec3 = [vals[order[0]].max(0.0).sqrt(), vals[order[1]].max(0.0).sqrt(), vals[order[2]].max(0.0).sqrt()];
    if s[1] <= 1e-300 {
        return None;
    }
    let u0 = mat_vec(m, v0).map(|x| x / s[0]);
    let u1 = mat_vec(m, v1).map(|x| x / s[1]);
    let u2 = cross(u0, u1);
    Some((from_cols(u0, u1, u2), s, from_cols(v0, v1, v2)))
}

/// The rotation nearest to `m` (polar factor), with determinant +1.
fn nearest_rotation(m: &Mat3) -> Option<Mat3> {
    let (u, _, v) = svd3(m)?;
    let r = mat_mul(&u, &transpose(&v));
    (det3(&r) > 0.0).then_some(r)
}

/// Rodrigues: the rotation by the vector `w` (axis times angle, radians).
fn exp_so3(w: Vec3) -> Mat3 {
    let th = dot(w, w).sqrt();
    if th < 1e-12 {
        return [[1.0, -w[2], w[1]], [w[2], 1.0, -w[0]], [-w[1], w[0], 1.0]];
    }
    let k = w.map(|x| x / th);
    let (s, c) = th.sin_cos();
    let kx = [[0.0, -k[2], k[1]], [k[2], 0.0, -k[0]], [-k[1], k[0], 0.0]];
    let kx2 = mat_mul(&kx, &kx);
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = f64::from(u8::from(i == j)) + s * kx[i][j] + (1.0 - c) * kx2[i][j];
        }
    }
    r
}

/// Solves the `n x n` system `a x = b` by Gaussian elimination with partial pivoting.
fn solve_linear(mut a: Vec<f64>, mut b: Vec<f64>, n: usize) -> Option<Vec<f64>> {
    for c in 0..n {
        let p = (c..n).max_by(|x, y| a[x * n + c].abs().total_cmp(&a[y * n + c].abs()))?;
        if a[p * n + c].abs() < 1e-300 {
            return None;
        }
        for k in 0..n {
            a.swap(c * n + k, p * n + k);
        }
        b.swap(c, p);
        for r in c + 1..n {
            let f = a[r * n + c] / a[c * n + c];
            for k in c..n {
                a[r * n + k] -= f * a[c * n + k];
            }
            b[r] -= f * b[c];
        }
    }
    let mut x = vec![0.0; n];
    for r in (0..n).rev() {
        let mut s = b[r];
        for k in r + 1..n {
            s -= a[r * n + k] * x[k];
        }
        x[r] = s / a[r * n + r];
    }
    x.iter().all(|v| v.is_finite()).then_some(x)
}

// ---- geometry ----

/// A camera `x = R X + t` in normalised image coordinates.
#[derive(Clone, Copy, Debug)]
struct Pose {
    r: Mat3,
    t: Vec3,
}
impl Pose {
    const IDENTITY: Self = Self { r: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], t: [0.0; 3] };
    fn cam(&self, x: Vec3) -> Vec3 {
        let p = mat_vec(&self.r, x);
        [p[0] + self.t[0], p[1] + self.t[1], p[2] + self.t[2]]
    }
}

/// Linear triangulation from `(pose, normalised observation)` pairs.
fn triangulate(views: &[(Pose, [f64; 2])]) -> Option<Vec3> {
    let mut rows = Vec::with_capacity(views.len() * 2);
    for (p, x) in views {
        let row = |i: usize| [p.r[i][0], p.r[i][1], p.r[i][2], p.t[i]];
        let (r0, r1, r2) = (row(0), row(1), row(2));
        rows.push((0..4).map(|k| x[0] * r2[k] - r0[k]).collect());
        rows.push((0..4).map(|k| x[1] * r2[k] - r1[k]).collect());
    }
    let v = null_vector(&rows, 4);
    (v[3].abs() > 1e-300).then(|| [v[0] / v[3], v[1] / v[3], v[2] / v[3]]).filter(|p| p.iter().all(|c| c.is_finite()))
}

/// Gauss-Newton refinement of one point against its views.
fn refine_point(mut x: Vec3, views: &[(Pose, [f64; 2])]) -> Vec3 {
    for _ in 0..10 {
        let mut jtj = vec![0.0; 9];
        let mut jtr = [0.0; 3];
        for (p, o) in views {
            let c = p.cam(x);
            if c[2] <= 1e-12 {
                continue;
            }
            let r = [c[0] / c[2] - o[0], c[1] / c[2] - o[1]];
            let dp = [[1.0 / c[2], 0.0, -c[0] / (c[2] * c[2])], [0.0, 1.0 / c[2], -c[1] / (c[2] * c[2])]];
            for i in 0..2 {
                let row: Vec<f64> = (0..3).map(|k| dp[i][0] * p.r[0][k] + dp[i][1] * p.r[1][k] + dp[i][2] * p.r[2][k]).collect();
                for a in 0..3 {
                    jtr[a] += row[a] * r[i];
                    for b in 0..3 {
                        jtj[a * 3 + b] += row[a] * row[b];
                    }
                }
            }
        }
        let Some(d) = solve_linear(jtj, jtr.iter().map(|v| -v).collect(), 3) else { break };
        x = [x[0] + d[0], x[1] + d[1], x[2] + d[2]];
        if d.iter().map(|v| v * v).sum::<f64>() < 1e-30 {
            break;
        }
    }
    x
}

/// Gauss-Newton refinement of one pose against `(point, observation)` pairs (left-multiplied
/// rotation update).
fn refine_pose(mut pose: Pose, obs: &[(Vec3, [f64; 2])]) -> Pose {
    for _ in 0..15 {
        let mut jtj = vec![0.0; 36];
        let mut jtr = [0.0; 6];
        for (x, o) in obs {
            let rx = mat_vec(&pose.r, *x);
            let c = [rx[0] + pose.t[0], rx[1] + pose.t[1], rx[2] + pose.t[2]];
            if c[2] <= 1e-12 {
                continue;
            }
            let r = [c[0] / c[2] - o[0], c[1] / c[2] - o[1]];
            let dp = [[1.0 / c[2], 0.0, -c[0] / (c[2] * c[2])], [0.0, 1.0 / c[2], -c[1] / (c[2] * c[2])]];
            // d c / d w = -[R X]x, d c / d t = I.
            let skew = [[0.0, rx[2], -rx[1]], [-rx[2], 0.0, rx[0]], [rx[1], -rx[0], 0.0]];
            for i in 0..2 {
                let mut row = [0.0; 6];
                for k in 0..3 {
                    row[k] = dp[i][0] * skew[0][k] + dp[i][1] * skew[1][k] + dp[i][2] * skew[2][k];
                    row[3 + k] = dp[i][k];
                }
                for a in 0..6 {
                    jtr[a] += row[a] * r[i];
                    for b in 0..6 {
                        jtj[a * 6 + b] += row[a] * row[b];
                    }
                }
            }
        }
        let Some(d) = solve_linear(jtj, jtr.iter().map(|v| -v).collect(), 6) else { break };
        let dr = exp_so3([d[0], d[1], d[2]]);
        pose.r = mat_mul(&dr, &pose.r);
        if let Some(r) = nearest_rotation(&pose.r) {
            pose.r = r;
        }
        pose.t = [pose.t[0] + d[3], pose.t[1] + d[4], pose.t[2] + d[5]];
        if d.iter().map(|v| v * v).sum::<f64>() < 1e-30 {
            break;
        }
    }
    pose
}

/// Linear camera resection (DLT on normalised coordinates, points centred and scaled), projected
/// to a rotation, then refined.
fn resect(obs: &[(Vec3, [f64; 2])]) -> Option<Pose> {
    let n = obs.len() as f64;
    let m = obs.iter().fold([0.0; 3], |a, (x, _)| [a[0] + x[0] / n, a[1] + x[1] / n, a[2] + x[2] / n]);
    let s = (obs.iter().map(|(x, _)| ((x[0] - m[0]).powi(2) + (x[1] - m[1]).powi(2) + (x[2] - m[2]).powi(2)).sqrt()).sum::<f64>() / n).max(1e-300);
    let mut rows = Vec::with_capacity(obs.len() * 2);
    for (x, o) in obs {
        let xh = [(x[0] - m[0]) / s, (x[1] - m[1]) / s, (x[2] - m[2]) / s, 1.0];
        let mut a = vec![0.0; 12];
        let mut b = vec![0.0; 12];
        for k in 0..4 {
            a[k] = xh[k];
            a[8 + k] = -o[0] * xh[k];
            b[4 + k] = xh[k];
            b[8 + k] = -o[1] * xh[k];
        }
        rows.push(a);
        rows.push(b);
    }
    let p = null_vector(&rows, 12);
    // P' acts on normalised points: P = P' T, T = [[I/s, -m/s], [0, 1]].
    let pm: Mat3 = [[p[0] / s, p[1] / s, p[2] / s], [p[4] / s, p[5] / s, p[6] / s], [p[8] / s, p[9] / s, p[10] / s]];
    let p4 = [
        p[3] - (p[0] * m[0] + p[1] * m[1] + p[2] * m[2]) / s,
        p[7] - (p[4] * m[0] + p[5] * m[1] + p[6] * m[2]) / s,
        p[11] - (p[8] * m[0] + p[9] * m[1] + p[10] * m[2]) / s,
    ];
    let (_, sv, _) = svd3(&pm)?;
    let mut scale = (sv[0] + sv[1] + sv[2]) / 3.0;
    if det3(&pm) < 0.0 {
        scale = -scale;
    }
    let r = nearest_rotation(&pm.map(|row| row.map(|v| v / scale)))?;
    let pose = Pose { r, t: p4.map(|v| v / scale) };
    // Cheirality: most points in front.
    let front = obs.iter().filter(|(x, _)| pose.cam(*x)[2] > 0.0).count();
    if front * 2 < obs.len() {
        return None;
    }
    Some(refine_pose(pose, obs))
}

/// `Rz(c) Ry(b) Rx(a)` angles (degrees) of a rotation, `b` in [-90, 90].
pub fn zyx_degrees(r: &Mat3) -> (f64, f64, f64) {
    let b = (-r[2][0]).clamp(-1.0, 1.0).asin();
    let a = r[2][1].atan2(r[2][2]);
    let c = r[1][0].atan2(r[0][0]);
    (a.to_degrees(), b.to_degrees(), c.to_degrees())
}
fn unwrap(prev: f64, v: f64) -> f64 {
    let mut v = v;
    while v - prev > 180.0 {
        v -= 360.0;
    }
    while v - prev < -180.0 {
        v += 360.0;
    }
    v
}

/// Solves the camera path and the point cloud (module docs). `tracks[p][f]` is point `p` at frame
/// `f` in composition pixels; every track has the same frame count.
pub fn solve_camera(tracks: &[Vec<Option<[f64; 2]>>], zoom: f64, width: u32, height: u32) -> Result<CameraSolve, CameraSolveError> {
    let frames = tracks.first().map_or(0, Vec::len);
    if tracks.len() > MAX_SOLVE_POINTS || !(2..=MAX_SOLVE_FRAMES).contains(&frames) || !(zoom.is_finite() && zoom > 0.0) || width == 0 || height == 0 {
        return Err(CameraSolveError::Parameter);
    }
    if tracks.iter().any(|t| t.len() != frames || t.iter().flatten().any(|p| !(p[0].is_finite() && p[1].is_finite()))) {
        return Err(CameraSolveError::Parameter);
    }
    let (cx, cy) = (f64::from(width) / 2.0, f64::from(height) / 2.0);
    let norm = |p: [f64; 2]| [(p[0] - cx) / zoom, (p[1] - cy) / zoom];
    let obs = |p: usize, f: usize| tracks.get(p).and_then(|t| t.get(f)).copied().flatten().map(norm);
    // Key pair: frame 0 and the frame with the largest median displacement (>= 8 common points).
    let mut best: Option<(f64, usize)> = None;
    let mut any_pair = false;
    for f in 1..frames {
        let mut d: Vec<f64> = (0..tracks.len())
            .filter_map(|p| Some((obs(p, 0)?, obs(p, f)?)))
            .map(|(a, b)| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt())
            .collect();
        if d.len() < 8 {
            continue;
        }
        any_pair = true;
        d.sort_by(f64::total_cmp);
        let med = d[d.len() / 2];
        if best.is_none_or(|(m, _)| med > m) {
            best = Some((med, f));
        }
    }
    if !any_pair {
        return Err(CameraSolveError::TooFewPoints);
    }
    let (med, kb) = best.ok_or(CameraSolveError::TooFewPoints)?;
    if med < 1e-3 {
        return Err(CameraSolveError::NoParallax);
    }
    let common: Vec<(usize, [f64; 2], [f64; 2])> = (0..tracks.len()).filter_map(|p| Some((p, obs(p, 0)?, obs(p, kb)?))).collect();
    // Essential matrix, normalised eight-point (x_b^T E x_a = 0).
    let rows: Vec<Vec<f64>> = common
        .iter()
        .map(|(_, a, b)| vec![b[0] * a[0], b[0] * a[1], b[0], b[1] * a[0], b[1] * a[1], b[1], a[0], a[1], 1.0])
        .collect();
    let e = null_vector(&rows, 9);
    let em: Mat3 = [[e[0], e[1], e[2]], [e[3], e[4], e[5]], [e[6], e[7], e[8]]];
    let (mut u, _, mut v) = svd3(&em).ok_or(CameraSolveError::Degenerate)?;
    if det3(&u) < 0.0 {
        u = u.map(|r| r.map(|x| -x));
    }
    if det3(&v) < 0.0 {
        v = v.map(|r| r.map(|x| -x));
    }
    let w: Mat3 = [[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]];
    let vt = transpose(&v);
    let ra = mat_mul(&u, &mat_mul(&w, &vt));
    let rb = mat_mul(&u, &mat_mul(&transpose(&w), &vt));
    let t = col(&u, 2);
    let mut best_pose: Option<(usize, Pose)> = None;
    for r in [ra, rb] {
        for sign in [1.0, -1.0] {
            let pose = Pose { r, t: t.map(|x| x * sign) };
            let front = common
                .iter()
                .filter(|(_, a, b)| triangulate(&[(Pose::IDENTITY, *a), (pose, *b)]).is_some_and(|x| x[2] > 0.0 && pose.cam(x)[2] > 0.0))
                .count();
            if best_pose.is_none_or(|(n, _)| front > n) {
                best_pose = Some((front, pose));
            }
        }
    }
    let (_, pose_b) = best_pose.ok_or(CameraSolveError::Degenerate)?;
    let mut poses: Vec<Option<Pose>> = vec![None; frames];
    poses[0] = Some(Pose::IDENTITY);
    poses[kb] = Some(pose_b);
    let mut points: Vec<Option<Vec3>> = vec![None; tracks.len()];
    for (p, a, b) in &common {
        points[*p] = triangulate(&[(Pose::IDENTITY, *a), (pose_b, *b)]).filter(|x| x[2] > 0.0 && pose_b.cam(*x)[2] > 0.0);
    }
    // Resect every other frame, nearest to the key frames first.
    let mut order: Vec<usize> = (1..frames).filter(|f| *f != kb).collect();
    order.sort_by_key(|f| f.abs_diff(kb).min(*f));
    for f in order {
        let o: Vec<(Vec3, [f64; 2])> = (0..tracks.len()).filter_map(|p| Some((points[p]?, obs(p, f)?))).collect();
        if o.len() < MIN_FRAME_POINTS {
            return Err(CameraSolveError::FrameUnderconstrained(f));
        }
        poses[f] = Some(resect(&o).ok_or(CameraSolveError::Degenerate)?);
    }
    let solved: Vec<Pose> = poses.iter().map(|p| p.ok_or(CameraSolveError::Degenerate)).collect::<Result<_, _>>()?;
    // Alternate: points over every solved frame, then the poses ([`REFINE_ROUNDS`] point passes).
    let views_of = |p: usize, poses: &[Pose]| -> Vec<(Pose, [f64; 2])> { (0..frames).filter_map(|f| Some((poses[f], obs(p, f)?))).collect() };
    let mut poses = solved;
    for round in 0..REFINE_ROUNDS {
        for (p, slot) in points.iter_mut().enumerate() {
            let views = views_of(p, &poses);
            *slot = if views.len() >= 2 { triangulate(&views).map(|x| refine_point(x, &views)) } else { None };
        }
        if round + 1 < REFINE_ROUNDS {
            for (f, pose) in poses.iter_mut().enumerate().skip(1) {
                let o: Vec<(Vec3, [f64; 2])> = (0..tracks.len()).filter_map(|p| Some((points[p]?, obs(p, f)?))).collect();
                if o.len() >= MIN_FRAME_POINTS {
                    *pose = refine_pose(*pose, &o);
                }
            }
        }
    }
    // Gauge: frame 0 at (w/2, h/2, -zoom) looking along +Z; median point depth = zoom.
    let mut depths: Vec<f64> = points.iter().flatten().map(|x| x[2]).filter(|z| *z > 0.0).collect();
    if depths.is_empty() {
        return Err(CameraSolveError::Degenerate);
    }
    depths.sort_by(f64::total_cmp);
    let s = zoom / depths[depths.len() / 2];
    let origin = [cx, cy, -zoom];
    let to_world = |x: Vec3| [origin[0] + s * x[0], origin[1] + s * x[1], origin[2] + s * x[2]];
    let mut cameras = Vec::with_capacity(frames);
    let mut prev = (0.0, 0.0, 0.0);
    for (f, p) in poses.iter().enumerate() {
        let rc = transpose(&p.r);
        let c = mat_vec(&rc, p.t).map(|x| -x);
        let (a, b, cz) = zyx_degrees(&rc);
        let (a, b, cz) = if f == 0 { (a, b, cz) } else { (unwrap(prev.0, a), unwrap(prev.1, b), unwrap(prev.2, cz)) };
        prev = (a, b, cz);
        cameras.push(SolvedCamera { position: to_world(c), rotation: rc, rx: a, ry: b, rz: cz });
    }
    let world_points: Vec<Option<Vec3>> = points.iter().map(|x| x.map(to_world)).collect();
    // RMS reprojection error in pixels.
    let (mut sum, mut n) = (0.0, 0usize);
    for (p, x) in points.iter().enumerate() {
        let Some(x) = x else { continue };
        for (f, pose) in poses.iter().enumerate() {
            let Some(o) = obs(p, f) else { continue };
            let c = pose.cam(*x);
            sum += ((c[0] / c[2] - o[0]).powi(2) + (c[1] / c[2] - o[1]).powi(2)) * zoom * zoom;
            n += 1;
        }
    }
    Ok(CameraSolve { zoom, cameras, points: world_points, rms_px: if n == 0 { 0.0 } else { (sum / n as f64).sqrt() } })
}

fn key(time: u64, value: Vec<f64>) -> StudioCompositionKeyframe {
    StudioCompositionKeyframe {
        time,
        value,
        in_interp: StudioInterpolation::Linear,
        out_interp: StudioInterpolation::Linear,
        in_tangent: Vec::new(),
        out_tangent: Vec::new(),
    }
}
fn tick(timing: TrackTiming, frame: usize) -> Result<u64, CameraSolveError> {
    if timing.ticks_per_frame == 0 {
        return Err(CameraSolveError::Parameter);
    }
    u64::try_from(frame)
        .ok()
        .and_then(|f| f.checked_mul(timing.ticks_per_frame))
        .and_then(|d| d.checked_add(timing.first_tick))
        .ok_or(CameraSolveError::TickOverflow)
}

/// The solved path as a one-node camera layer record: linear keys per frame on position (X/Y),
/// rotation (Z), `three_d.position_z`, `three_d.rotation_x/y`; zoom constant (STU-CMP-040a: a
/// one-node camera is the camera layer without a point of interest).
pub fn camera_layer(solve: &CameraSolve, layer_id: &str, timing: StudioLayerTiming, keys: TrackTiming) -> Result<StudioCompositionLayer, CameraSolveError> {
    let mut xy = Vec::with_capacity(solve.cameras.len());
    let (mut z, mut rx, mut ry, mut rz) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (f, c) in solve.cameras.iter().enumerate() {
        let t = tick(keys, f)?;
        xy.push(key(t, vec![c.position[0], c.position[1]]));
        z.push(key(t, vec![c.position[2]]));
        rx.push(key(t, vec![c.rx]));
        ry.push(key(t, vec![c.ry]));
        rz.push(key(t, vec![c.rz]));
    }
    let first = |k: &[StudioCompositionKeyframe]| k.first().map(|k| k.value.clone()).unwrap_or_default();
    let keyed = |k: Vec<StudioCompositionKeyframe>| StudioKeyedValue { static_value: first(&k), keyframes: k };
    Ok(StudioCompositionLayer {
        layer_id: layer_id.to_owned(),
        timing,
        parent: None,
        precomp: None,
        track_matte: None,
        streams: vec![
            StudioPropertyStream { property: StudioLayerProperty::Position, static_value: first(&xy), keyframes: xy },
            StudioPropertyStream { property: StudioLayerProperty::Rotation, static_value: first(&rz), keyframes: rz },
        ],
        corner_pin: None,
        masks: Vec::new(),
        three_d: Some(StudioLayer3d {
            position_z: Some(keyed(z)),
            rotation_x: Some(keyed(rx)),
            rotation_y: Some(keyed(ry)),
            ..StudioLayer3d::default()
        }),
        camera: Some(StudioCameraOptions {
            zoom: StudioKeyedValue::constant(vec![solve.zoom]),
            point_of_interest: None,
            depth_of_field: false,
            iris_shape: hsk_studio_folio::StudioIrisShape::default(),
            values: BTreeMap::new(),
        }),
        light: None,
        time_remap: None,
        frame_blending: hsk_studio_folio::StudioFrameBlending::Off,
        roto: None,
        effects: Vec::new(),
    })
}

/// One solved point as a 3D null (target) layer record; `None` when the point was not solved.
pub fn target_layer(solve: &CameraSolve, point: usize, layer_id: &str, timing: StudioLayerTiming) -> Option<StudioCompositionLayer> {
    let p = solve.points.get(point).copied().flatten()?;
    Some(StudioCompositionLayer {
        layer_id: layer_id.to_owned(),
        timing,
        parent: None,
        precomp: None,
        track_matte: None,
        streams: vec![StudioPropertyStream { property: StudioLayerProperty::Position, static_value: vec![p[0], p[1]], keyframes: Vec::new() }],
        corner_pin: None,
        masks: Vec::new(),
        three_d: Some(StudioLayer3d { position_z: Some(StudioKeyedValue::constant(vec![p[2]])), ..StudioLayer3d::default() }),
        camera: None,
        light: None,
        time_remap: None,
        frame_blending: hsk_studio_folio::StudioFrameBlending::Off,
        roto: None,
        effects: Vec::new(),
    })
}
