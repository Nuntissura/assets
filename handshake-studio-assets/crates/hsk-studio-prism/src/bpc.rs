//! Black point compensation owned by Prism (STU-COL-136: an independent boolean), after the ICC
//! final-approved text of ISO 18619 (https://archive.color.org/files/BlackPointCompensation.pdf) with
//! the lcms2 `cmsDetectDestinationBlackPoint` curve fit as cross-reference; no code was copied.
//!
//! Exact rule implemented here:
//! 1. Intents Relative, Perceptual and Saturation only, the same intent on both sides. Absolute
//!    colorimetric ignores the flag (receipt rule `absolute_ignores_bpc`).
//! 2. Only L* of each black point is used; both are neutral `(L, 0, 0)` and at most L* 50.
//! 3. Source black: for a CMYK profile with a perceptual B2A table, the device colour that
//!    `Lab(0,0,0)` maps to (perceptual) evaluated through the source A2B at the request intent;
//!    otherwise the darkest device vertex (Gray {0,1}; RGB {000,111}; CMYK {0000,1111,0001,1110}).
//! 4. Destination black: a matrix/Gray profile uses its darkest vertex. A table-based profile uses
//!    the ISO round-trip ramp: 256 Lab points `(0,ka,kb)..(100,0,0)` through `Lab -> device (request
//!    intent) -> Lab (relative)`, made monotonic; equal ends give 0; for Relative a straight
//!    mid-range (`|in-out| < 4` wherever `in > MinL + 0.2 (MaxL-MinL)`, the lcms2 reading of the
//!    garbled ISO operator) keeps the initial black; otherwise the least-squares quadratic root of
//!    the shadow samples (needs at least 3 samples, ISO text; lcms2 needs 4) clamped to 0..50.
//! 5. `Y(L)` is the CIE inverse (`((L+16)/116)^3`, `L * 27/24389` for L <= 8). With
//!    `scale = (1 - Y_dst) / (1 - Y_src)` the PCS map is `XYZ' = scale * XYZ + (1 - scale) * D50`,
//!    so white is fixed and the source black lands on the destination black.
//! 6. moxcms's own black-point scaling (RGB v4, perceptual or saturation, only where a table is
//!    involved) never runs: every leg touches the version-2.1 companion. When moxcms would have
//!    applied it, the receipt says `engine_builtin_suppressed`, and Prism applies exactly one
//!    compensation, or none when the flag is off.
use crate::wide::{BpcReport, DeviceSpace, PcsMap, ProfileKind, WideError, WideProfileDescriptor, leg};
use crate::Intent;
use hsk_studio_accord::CancellationToken;
use moxcms::{ColorProfile, TransformF32Executor};
use std::sync::Arc;

const D50: [f64; 3] = [0.9642, 1.0, 0.8249];
const RAMP: usize = 256;

pub(crate) fn y_of_l(l: f64) -> f64 {
    if l > 8.0 {
        ((l + 16.0) / 116.0).powi(3)
    } else {
        l * 27.0 / 24389.0
    }
}
pub(crate) fn l_of_y(y: f64) -> f64 {
    if y > 216.0 / 24389.0 {
        116.0 * y.cbrt() - 16.0
    } else {
        24389.0 / 27.0 * y
    }
}
fn lab_to_xyz(l: f64, a: f64, b: f64) -> [f64; 3] {
    let fy = (l + 16.0) / 116.0;
    let (fx, fz) = (fy + a / 500.0, fy - b / 200.0);
    let inv = |t: f64| {
        if t > 6.0 / 29.0 {
            t * t * t
        } else {
            3.0 * (6.0f64 / 29.0).powi(2) * (t - 4.0 / 29.0)
        }
    };
    [D50[0] * inv(fx), D50[1] * inv(fy), D50[2] * inv(fz)]
}
fn xyz_to_lab(xyz: [f64; 3]) -> [f64; 3] {
    let f = |t: f64| {
        if t > 216.0 / 24389.0 {
            t.cbrt()
        } else {
            (24389.0 / 27.0 * t + 16.0) / 116.0
        }
    };
    let (fx, fy, fz) = (f(xyz[0] / D50[0]), f(xyz[1] / D50[1]), f(xyz[2] / D50[2]));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

struct Probe {
    exec: Arc<TransformF32Executor>,
    in_channels: usize,
    out_channels: usize,
}
impl Probe {
    fn new(exec: Arc<TransformF32Executor>, in_channels: usize, out_channels: usize) -> Self {
        Self {
            exec,
            in_channels,
            out_channels,
        }
    }
    /// Evaluate whole pixels (`input.len()` is a multiple of the input channel count).
    fn eval(&self, input: &[f32]) -> Result<Vec<f32>, WideError> {
        let mut out = vec![0.0f32; self.out_channels * (input.len() / self.in_channels)];
        self.exec
            .transform(input, &mut out)
            .map_err(|_| WideError::EngineFailure)?;
        if out.iter().any(|v| !v.is_finite()) {
            return Err(WideError::NonfiniteOutput);
        }
        Ok(out)
    }
}

pub(crate) struct BpcInputs<'a> {
    pub companion: &'a ColorProfile,
    pub source: (&'a ColorProfile, &'a WideProfileDescriptor),
    pub destination: (&'a ColorProfile, &'a WideProfileDescriptor),
    pub intent: Intent,
    pub to_pcs: &'a Arc<TransformF32Executor>,
    pub from_pcs: &'a Arc<TransformF32Executor>,
}

fn vertices(space: DeviceSpace) -> Vec<Vec<f32>> {
    match space {
        DeviceSpace::Gray => vec![vec![0.0], vec![1.0]],
        DeviceSpace::Rgb => vec![vec![0.0; 3], vec![1.0; 3]],
        DeviceSpace::Cmyk => vec![
            vec![0.0; 4],
            vec![1.0; 4],
            vec![0.0, 0.0, 0.0, 1.0],
            vec![1.0, 1.0, 1.0, 0.0],
        ],
    }
}
/// Lowest L* among the darkest-candidate device vertices.
fn darkest_l(to_xyz: &Probe, space: DeviceSpace) -> Result<f64, WideError> {
    let mut best = f64::INFINITY;
    for v in vertices(space) {
        let xyz = to_xyz.eval(&v)?;
        best = best.min(l_of_y(f64::from(xyz[1])));
    }
    Ok(best)
}

fn quadratic_root(points: &[(f64, f64)]) -> f64 {
    let n = points.len() as f64;
    let (mut sx, mut sx2, mut sx3, mut sx4) = (0.0, 0.0, 0.0, 0.0);
    let (mut sy, mut sxy, mut sx2y) = (0.0, 0.0, 0.0);
    for &(x, y) in points {
        sx += x;
        sx2 += x * x;
        sx3 += x * x * x;
        sx4 += x * x * x * x;
        sy += y;
        sxy += x * y;
        sx2y += x * x * y;
    }
    // Normal equations for y = a x^2 + b x + c, solved by Cramer's rule.
    let det = |m: [[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let m = [[sx4, sx3, sx2], [sx3, sx2, sx], [sx2, sx, n]];
    let d = det(m);
    if d.abs() < 1e-12 {
        return 0.0;
    }
    let rhs = [sx2y, sxy, sy];
    let col = |i: usize| {
        let mut t = m;
        for (r, row) in t.iter_mut().enumerate() {
            row[i] = rhs[r];
        }
        det(t) / d
    };
    let (a, b, c) = (col(0), col(1), col(2));
    if a.abs() < 1e-10 {
        return if b == 0.0 { 0.0 } else { (-c / b).clamp(0.0, 50.0) };
    }
    let disc = b * b - 4.0 * a * c;
    if disc <= 0.0 {
        0.0
    } else {
        ((-b + disc.sqrt()) / (2.0 * a)).clamp(0.0, 50.0)
    }
}

pub(crate) fn compute(
    c: &BpcInputs<'_>,
    cancel: &CancellationToken,
) -> Result<(PcsMap, BpcReport), WideError> {
    let ((sp, sd), (dp, dd)) = (c.source, c.destination);
    let canceled = || cancel.check().map_err(|_| WideError::Canceled);
    let src_to_xyz = Probe::new(c.to_pcs.clone(), sd.device.channels(), 3);
    // Source black.
    let source_l = if sd.device == DeviceSpace::Cmyk && sd.pcs_to_device.perceptual {
        let b2a0 = Probe::new(
            leg(c.companion, DeviceSpace::Rgb, sp, DeviceSpace::Cmyk, Intent::Perceptual)?,
            3,
            4,
        );
        let local = b2a0.eval(&[0.0, 0.0, 0.0])?;
        let xyz = src_to_xyz.eval(&local)?;
        l_of_y(f64::from(xyz[1]))
    } else {
        darkest_l(&src_to_xyz, sd.device)?
    }
    .min(50.0);
    canceled()?;
    // Destination black.
    let table_based = matches!(dd.kind, ProfileKind::LegacyLut | ProfileKind::MultiDimensional)
        && (dd.pcs_to_device.colorimetric || dd.pcs_to_device.perceptual || dd.pcs_to_device.saturation);
    let dst_out = dd.device.channels();
    let dst_to_xyz_intent = Probe::new(
        leg(dp, dd.device, c.companion, DeviceSpace::Rgb, c.intent)?,
        dst_out,
        3,
    );
    let dest_l = if !table_based {
        darkest_l(&dst_to_xyz_intent, dd.device)?.min(50.0)
    } else {
        if !dd.device_to_pcs.colorimetric {
            return Err(WideError::MissingIntentTable);
        }
        let relative = c.intent == Intent::RelativeColorimetric;
        let dst_to_xyz_rel = Probe::new(
            leg(dp, dd.device, c.companion, DeviceSpace::Rgb, Intent::RelativeColorimetric)?,
            dst_out,
            3,
        );
        let mut initial = [0.0f64; 3];
        if relative {
            let local: Vec<f32> = if dd.device == DeviceSpace::Cmyk && dd.pcs_to_device.perceptual {
                Probe::new(
                    leg(c.companion, DeviceSpace::Rgb, dp, dd.device, Intent::Perceptual)?,
                    3,
                    dst_out,
                )
                .eval(&[0.0, 0.0, 0.0])?
            } else {
                // Darkest vertex by L* at the request intent.
                let mut best: Option<(f64, Vec<f32>)> = None;
                for v in vertices(dd.device) {
                    let l = l_of_y(f64::from(dst_to_xyz_intent.eval(&v)?[1]));
                    if best.as_ref().is_none_or(|(b, _)| l < *b) {
                        best = Some((l, v));
                    }
                }
                best.map(|(_, v)| v).unwrap_or_default()
            };
            let xyz = dst_to_xyz_rel.eval(&local)?;
            initial = xyz_to_lab([f64::from(xyz[0]), f64::from(xyz[1]), f64::from(xyz[2])]);
            if dd.device == DeviceSpace::Cmyk {
                initial = [initial[0].min(50.0), 0.0, 0.0];
            }
        }
        let (ka, kb) = (initial[1].clamp(-50.0, 50.0), initial[2].clamp(-50.0, 50.0));
        let mut in_l = [0.0f64; RAMP];
        let mut xyz_in = Vec::with_capacity(RAMP * 3);
        for (i, slot) in in_l.iter_mut().enumerate() {
            let t = i as f64 / (RAMP - 1) as f64;
            *slot = t * 100.0;
            for v in lab_to_xyz(*slot, ka * (1.0 - t), kb * (1.0 - t)) {
                xyz_in.push(v as f32);
            }
        }
        let mut device = vec![0.0f32; RAMP * dst_out];
        c.from_pcs
            .transform(&xyz_in, &mut device)
            .map_err(|_| WideError::EngineFailure)?;
        canceled()?;
        let mut back = vec![0.0f32; RAMP * 3];
        dst_to_xyz_rel
            .exec
            .transform(&device, &mut back)
            .map_err(|_| WideError::EngineFailure)?;
        if back.iter().any(|v| !v.is_finite()) {
            return Err(WideError::NonfiniteOutput);
        }
        let mut out_l: Vec<f64> = back.chunks_exact(3).map(|p| l_of_y(f64::from(p[1]))).collect();
        for i in (0..RAMP - 1).rev() {
            out_l[i] = out_l[i].min(out_l[i + 1]);
        }
        let (min_l, max_l) = (out_l[0], out_l[RAMP - 1]);
        if min_l >= max_l {
            0.0
        } else if relative
            && (0..RAMP).all(|i| in_l[i] <= min_l + 0.2 * (max_l - min_l) || (in_l[i] - out_l[i]).abs() < 4.0)
        {
            initial[0]
        } else {
            let (lo, hi) = if relative { (0.1, 0.5) } else { (0.03, 0.25) };
            let points: Vec<(f64, f64)> = (0..RAMP)
                .map(|i| (in_l[i], (out_l[i] - min_l) / (max_l - min_l)))
                .filter(|(_, y)| *y >= lo && *y < hi)
                .collect();
            if points.len() < 3 { 0.0 } else { quadratic_root(&points) }
        }
    };
    canceled()?;
    let (ys, yd) = (y_of_l(source_l), y_of_l(dest_l));
    let denom = 1.0 - ys;
    let scale = if denom.abs() < 1e-12 { 1.0 } else { (1.0 - yd) / denom };
    let equal = (scale - 1.0).abs() < 1e-12;
    let report = BpcReport {
        requested: true,
        applied: !equal,
        rule: if equal { "iso18619_equal_black_points" } else { "iso18619" },
        source_black_lstar: Some(source_l),
        destination_black_lstar: Some(dest_l),
        scale: Some(scale),
        engine_builtin_suppressed: false,
    };
    let map = if equal {
        PcsMap::Identity
    } else {
        PcsMap::Affine {
            scale,
            offset: [(1.0 - scale) * D50[0], (1.0 - scale) * D50[1], (1.0 - scale) * D50[2]],
        }
    };
    Ok((map, report))
}
