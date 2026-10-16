//! Anti-aliased auxiliary channels (lane SMB-AE-3D-3; STU-CMP-050 "Auxiliary channel extract",
//! "ID matte", and the coverage the "Cryptomatte" row's "anti-aliased, overlapping-aware mattes"
//! need).
//!
//! [`pixel_coverage`] is the ranking pixel filter of the planar renderer: every pixel is sampled on
//! an `n x n` stratified grid (sample centres `((i + 0.5) / n, (j + 0.5) / n)` inside the pixel, box
//! filter); per sample the camera ray is tested against every plane exactly as
//! [`super::aux_channels`] does (inverse plane homography, nearest content texel, alpha x opacity),
//! and the hits are walked FRONT TO BACK with non-refractive transparency: hit `k` weighs
//! `a_k prod_{j < k} (1 - a_j)` (`a = clamp(alpha x opacity, 0, 1)`; equal depth: the upper plane
//! first). The weights of one plane are summed over the pixel's samples and divided by `n^2`
//! (empty samples count, so an edge pixel's coverages sum to less than 1); each plane's camera
//! depth and camera-space normal are averaged with the same weights. Entries are ranked by
//! coverage (ties: nearer, then lower plane index).
//!
//! [`aux_channels_aa`] resolves the filter per pixel where each channel is meaningful (Studio
//! reading; STU-CMP-050 names the channels, not their resolve; After Effects UNVERIFIED, D5):
//! entries with the same object id are merged first; `objectId` is the id with the largest merged
//! coverage (EXACT, never blended), `idCoverage` that id's coverage, `coverage` the sum of all
//! coverages (the alpha of the hits), `Z` that id's coverage-weighted mean depth (a depth averaged
//! across objects is not a depth; `+inf` where nothing is hit), `N.X N.Y N.Z` the coverage-weighted
//! mean of every hit's normal, renormalised (0 where nothing is hit). With `n = 1` and opaque
//! content the object ids and depths equal [`super::aux_channels`].
//!
//! Hostile input is refused before any per-pixel work: a non-finite plane transform or camera
//! (`InvalidState`, field `three_d_transform`), a NaN or infinite alpha (`aux_channels`),
//! `n` outside `1..=`[`MAX_AUX_SAMPLES`] ([`super::AUX_MATTE_PARAMETER`]), more than
//! [`super::MAX_AUX_WORK`] pixel x plane x sample tests (`BudgetExceeded`).
#![deny(clippy::arithmetic_side_effects)]

use super::{AuxChannels, Camera, NEAR_DEPTH, Plane3d, Vec3, mat_vec, normalize, transpose};
use crate::CompositeError;
use crate::temporal::TemporalError;
use hsk_studio_pigment::transform_math::{Affine2, Projective2};

/// Most samples per pixel axis.
pub const MAX_AUX_SAMPLES: u32 = 16;
/// The channels of [`AuxChannelsAa`]: [`super::AUX_CHANNEL_NAMES`] plus `idCoverage`.
pub const AUX_AA_CHANNEL_NAMES: [&str; 7] = ["Z", "N.X", "N.Y", "N.Z", "objectId", "coverage", "idCoverage"];

/// One plane's share of one pixel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoverageEntry {
    /// Index into the planes passed to [`pixel_coverage`].
    pub plane: u32,
    /// Weight sum over the pixel's samples divided by `n^2`.
    pub coverage: f32,
    /// Weighted mean camera depth.
    pub depth: f32,
    /// Weighted mean camera-space normal (unit, facing the camera).
    pub normal: [f32; 3],
}

/// The ranking pixel filter of one frame (module docs), pixels row-major.
#[derive(Clone, Debug, PartialEq)]
pub struct PixelCoverage {
    pub width: u32,
    pub height: u32,
    /// Samples per pixel axis.
    pub samples: u32,
    /// `entries[offsets[p]..offsets[p + 1]]` are pixel `p`'s entries, ranked.
    pub offsets: Vec<usize>,
    pub entries: Vec<CoverageEntry>,
}
impl PixelCoverage {
    /// Pixel `p`'s ranked entries (empty outside the frame).
    pub fn pixel(&self, p: usize) -> &[CoverageEntry] {
        match (self.offsets.get(p), p.checked_add(1).and_then(|q| self.offsets.get(q))) {
            (Some(a), Some(b)) => self.entries.get(*a..*b).unwrap_or(&[]),
            _ => &[],
        }
    }
    pub fn pixels(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }
}

fn invalid(id: &str, field: &'static str) -> TemporalError {
    TemporalError::InvalidState { layer_id: id.to_owned(), field }
}

fn finite3(v: &Vec3) -> bool {
    v.iter().all(|x| x.is_finite())
}

/// Per plane: inverse homography (screen -> content plan), the content rectangle, the camera-space
/// unit normal; `None` for a plane wholly behind the camera.
struct Prep {
    inverse: Projective2,
    x0: f64,
    y0: f64,
    w: usize,
    h: usize,
    normal: Vec3,
}

/// The ranking pixel filter (module docs). `alphas[i]` is plane `i`'s rendered content alpha over
/// its `bounds` (row-major, layer mask applied).
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::too_many_lines)]
pub fn pixel_coverage(planes: &[Plane3d], alphas: &[Vec<f32>], cam: &Camera, width: u32, height: u32, samples: u32) -> Result<PixelCoverage, TemporalError> {
    let first = planes.first().map_or("", |p| p.layer_id.as_str());
    if alphas.len() != planes.len() {
        return Err(invalid(first, "aux_channels"));
    }
    if samples == 0 || samples > MAX_AUX_SAMPLES {
        return Err(TemporalError::Unsupported { layer_id: first.to_owned(), feature: super::AUX_MATTE_PARAMETER });
    }
    if !(finite3(&cam.position) && cam.rotation.iter().all(finite3) && cam.zoom.is_finite() && cam.centre.iter().all(|v| v.is_finite())) {
        return Err(invalid("camera", "three_d_transform"));
    }
    let px = u64::from(width).checked_mul(u64::from(height)).ok_or(CompositeError::Overflow)?;
    let n2 = u64::from(samples).checked_mul(u64::from(samples)).ok_or(CompositeError::Overflow)?;
    let work = px
        .checked_mul(u64::try_from(planes.len()).map_err(|_| CompositeError::Overflow)?)
        .and_then(|v| v.checked_mul(n2))
        .ok_or(CompositeError::Overflow)?;
    if work > super::MAX_AUX_WORK {
        return Err(CompositeError::BudgetExceeded.into());
    }
    let rc_t = transpose(&cam.rotation);
    let mut preps: Vec<Option<Prep>> = Vec::with_capacity(planes.len());
    for (p, a) in planes.iter().zip(alphas) {
        let t = &p.transform;
        let parent_ok = t.parent.m.iter().all(finite3) && finite3(&t.parent.t);
        if !(finite3(&t.anchor) && finite3(&t.position) && t.scale.iter().all(|v| v.is_finite()) && t.rotation.iter().all(finite3) && parent_ok && p.opacity.is_finite()) {
            return Err(invalid(&p.layer_id, "three_d_transform"));
        }
        let (w, h) = (usize::try_from(p.bounds.width).map_err(|_| CompositeError::Overflow)?, usize::try_from(p.bounds.height).map_err(|_| CompositeError::Overflow)?);
        if a.len() != w.checked_mul(h).ok_or(CompositeError::Overflow)? || a.iter().any(|v| !v.is_finite()) {
            return Err(invalid(&p.layer_id, "aux_channels"));
        }
        let to_layer = Projective2::from_affine(&Affine2::translate(-p.origin[0], -p.origin[1]));
        let pin = to_layer.then(&super::plane_homography(t, cam));
        let (x0, y0) = (p.bounds.x as f64, p.bounds.y as f64);
        let (x1, y1) = (x0 + f64::from(p.bounds.width), y0 + f64::from(p.bounds.height));
        let front = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)].iter().filter(|(x, y)| pin.apply_h(*x, *y).2 > NEAR_DEPTH).count();
        if front != 0 && front != 4 {
            return Err(TemporalError::Unsupported { layer_id: p.layer_id.clone(), feature: super::COMPOSITION_3D_CROSSES_CAMERA });
        }
        if front == 0 {
            preps.push(None);
            continue;
        }
        let inverse = pin.inverse().map_err(|_| TemporalError::Unsupported { layer_id: p.layer_id.clone(), feature: super::COMPOSITION_3D_CROSSES_CAMERA })?;
        let normal = normalize(mat_vec(&rc_t, t.normal())).unwrap_or([0.0, 0.0, -1.0]);
        preps.push(Some(Prep { inverse, x0, y0, w, h, normal }));
    }
    let n = usize::try_from(px).map_err(|_| CompositeError::Overflow)?;
    let mut offsets = Vec::with_capacity(n.checked_add(1).ok_or(CompositeError::Overflow)?);
    offsets.push(0usize);
    let mut entries: Vec<CoverageEntry> = Vec::new();
    // Per plane accumulators of one pixel: weight, weighted depth, weighted normal.
    let mut acc: Vec<(f64, f64, [f64; 3])> = vec![(0.0, 0.0, [0.0; 3]); planes.len()];
    let mut hits: Vec<(f64, usize, f64)> = Vec::with_capacity(planes.len());
    let inv_n = 1.0 / f64::from(samples);
    let total = f64::from(samples) * f64::from(samples);
    for y in 0..height {
        for x in 0..width {
            acc.fill((0.0, 0.0, [0.0; 3]));
            for sj in 0..samples {
                for si in 0..samples {
                    let sx = f64::from(x) + (f64::from(si) + 0.5) * inv_n;
                    let sy = f64::from(y) + (f64::from(sj) + 0.5) * inv_n;
                    hits.clear();
                    for (i, pr) in preps.iter().enumerate() {
                        let Some(pr) = pr else { continue };
                        let (lx, ly, w) = pr.inverse.apply_h(sx, sy);
                        if !(w.is_finite() && w > 0.0) {
                            continue;
                        }
                        let depth = 1.0 / w;
                        let (u, v) = (lx / w - pr.x0, ly / w - pr.y0);
                        if !(depth > NEAR_DEPTH && u >= 0.0 && v >= 0.0) {
                            continue;
                        }
                        #[allow(clippy::cast_sign_loss)]
                        let (iu, iv) = (u.floor() as usize, v.floor() as usize);
                        if iu >= pr.w || iv >= pr.h {
                            continue;
                        }
                        let alpha = iv.checked_mul(pr.w).and_then(|r| r.checked_add(iu)).and_then(|k| alphas.get(i)?.get(k)).copied().unwrap_or(0.0);
                        let opacity = planes.get(i).map_or(0.0, |p| p.opacity);
                        let a = (f64::from(alpha) * opacity).clamp(0.0, 1.0);
                        if a > 0.0 {
                            hits.push((depth, i, a));
                        }
                    }
                    // Front to back; equal depth: the upper (later) plane first.
                    hits.sort_by(|a, b| a.0.total_cmp(&b.0).then(b.1.cmp(&a.1)));
                    let mut transmit = 1.0f64;
                    for (depth, i, a) in &hits {
                        let wgt = a * transmit;
                        transmit *= 1.0 - a;
                        let (Some(slot), Some(Some(pr))) = (acc.get_mut(*i), preps.get(*i)) else { continue };
                        // The normal faces the camera at the sample point.
                        let q = [depth * (sx - cam.centre[0]) / cam.zoom, depth * (sy - cam.centre[1]) / cam.zoom, *depth];
                        let s = if super::dot(pr.normal, q) > 0.0 { -1.0 } else { 1.0 };
                        slot.0 += wgt;
                        slot.1 += wgt * depth;
                        for k in 0..3 {
                            slot.2[k] += wgt * s * pr.normal[k];
                        }
                        if transmit <= 0.0 {
                            break;
                        }
                    }
                }
            }
            let start = entries.len();
            for (i, (wsum, dsum, nsum)) in acc.iter().enumerate() {
                if *wsum <= 0.0 {
                    continue;
                }
                let normal = normalize(*nsum).unwrap_or([0.0; 3]);
                entries.push(CoverageEntry {
                    plane: u32::try_from(i).map_err(|_| CompositeError::Overflow)?,
                    coverage: (wsum / total) as f32,
                    depth: (dsum / wsum) as f32,
                    normal: [normal[0] as f32, normal[1] as f32, normal[2] as f32],
                });
            }
            if let Some(slice) = entries.get_mut(start..) {
                slice.sort_by(|a, b| b.coverage.total_cmp(&a.coverage).then(a.depth.total_cmp(&b.depth)).then(a.plane.cmp(&b.plane)));
            }
            offsets.push(entries.len());
        }
    }
    Ok(PixelCoverage { width, height, samples, offsets, entries })
}

/// [`AuxChannels`] resolved from the ranking filter (module docs) plus `idCoverage`.
#[derive(Clone, Debug, PartialEq)]
pub struct AuxChannelsAa {
    pub channels: AuxChannels,
    /// The coverage of the pixel's `objectId`.
    pub id_coverage: Vec<f32>,
    pub samples: u32,
}
impl AuxChannelsAa {
    /// `(name, plane)` for every channel of [`AUX_AA_CHANNEL_NAMES`].
    pub fn channels(&self) -> Vec<(&'static str, &[f32])> {
        let mut v = self.channels.channels();
        v.push((AUX_AA_CHANNEL_NAMES[6], &self.id_coverage[..]));
        v
    }
    /// Every channel as one OpenEXR file.
    pub fn to_exr(&self) -> Result<Vec<u8>, &'static str> {
        super::exr::encode_exr_f32(self.channels.width, self.channels.height, &self.channels())
    }
}

/// The anti-aliased auxiliary channels of `planes` (module docs); `ids[i]` is plane `i`'s object id
/// (non-zero; planes may share an id, their coverages then merge).
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
pub fn aux_channels_aa(planes: &[Plane3d], alphas: &[Vec<f32>], ids: &[u32], cam: &Camera, width: u32, height: u32, samples: u32) -> Result<AuxChannelsAa, TemporalError> {
    if ids.len() != planes.len() || ids.contains(&0) {
        return Err(invalid(planes.first().map_or("", |p| p.layer_id.as_str()), "aux_channels"));
    }
    let cov = pixel_coverage(planes, alphas, cam, width, height, samples)?;
    let n = cov.pixels();
    let mut out = AuxChannels {
        width,
        height,
        depth: vec![f32::INFINITY; n],
        normal: [vec![0.0; n], vec![0.0; n], vec![0.0; n]],
        object_id: vec![0.0; n],
        coverage: vec![0.0; n],
        ids: {
            let mut v: Vec<(u32, String)> = ids.iter().copied().zip(planes.iter().map(|p| p.layer_id.clone())).collect();
            v.sort();
            v.dedup_by(|a, b| a.0 == b.0);
            v
        },
    };
    let mut id_coverage = vec![0.0f32; n];
    // Per id of one pixel: (id, coverage, coverage-weighted depth).
    let mut merged: Vec<(u32, f64, f64)> = Vec::new();
    for p in 0..n {
        let e = cov.pixel(p);
        if e.is_empty() {
            continue;
        }
        merged.clear();
        let (mut total, mut nsum) = (0.0f64, [0.0f64; 3]);
        for c in e {
            let id = ids.get(usize::try_from(c.plane).map_err(|_| CompositeError::Overflow)?).copied().unwrap_or(0);
            let (w, d) = (f64::from(c.coverage), f64::from(c.depth));
            total += w;
            for (s, v) in nsum.iter_mut().zip(c.normal) {
                *s += w * f64::from(v);
            }
            match merged.iter_mut().find(|m| m.0 == id) {
                Some(m) => {
                    m.1 += w;
                    m.2 += w * d;
                }
                None => merged.push((id, w, w * d)),
            }
        }
        // Largest coverage; ties: nearer, then the lower id.
        merged.sort_by(|a, b| b.1.total_cmp(&a.1).then((a.2 / a.1).total_cmp(&(b.2 / b.1))).then(a.0.cmp(&b.0)));
        let Some(&(id, w, wd)) = merged.first() else { continue };
        let normal = normalize(nsum).unwrap_or([0.0; 3]);
        let (Some(z), Some(o), Some(c), Some(ic)) = (out.depth.get_mut(p), out.object_id.get_mut(p), out.coverage.get_mut(p), id_coverage.get_mut(p)) else { continue };
        *z = (wd / w) as f32;
        *o = id as f32;
        *c = total.min(1.0) as f32;
        *ic = w as f32;
        for (plane, v) in out.normal.iter_mut().zip(normal) {
            if let Some(slot) = plane.get_mut(p) {
                *slot = v as f32;
            }
        }
    }
    Ok(AuxChannelsAa { channels: out, id_coverage, samples })
}

/// Anti-aliased STU-CMP-050 ID matte: the pixel's `idCoverage` where its `objectId` is one of
/// `wanted`, else 0 (the ranked coverages of every object, overlapping objects included, are what
/// Cryptomatte carries, `super::cryptomatte`).
#[allow(clippy::cast_precision_loss)]
pub fn id_matte_aa(aux: &AuxChannelsAa, wanted: &[u32]) -> Vec<f32> {
    let wanted: Vec<f32> = wanted.iter().map(|w| *w as f32).collect();
    aux.channels.object_id.iter().zip(&aux.id_coverage).map(|(v, c)| if *v != 0.0 && wanted.contains(v) { *c } else { 0.0 }).collect()
}
