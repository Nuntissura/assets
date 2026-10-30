//! Translation-only bracket alignment by median threshold bitmaps (MTB), implemented from the
//! paper (no code read): G. Ward, "Fast, Robust Image Registration for Compositing High Dynamic
//! Range Photographs from Hand-Held Exposures", Journal of Graphics Tools 8(2), 2003, 17-30.
//!
//! Per frame: grey `(54 R + 183 G + 19 B) / 256` (the paper's weights), a pyramid of 2x2 box
//! halvings, and per level a threshold bitmap (`grey > median`) and an exclusion bitmap (samples
//! near the median are ignored). From the coarsest level down, the nine offsets within one sample
//! of twice the coarser offset are scored by counting the overlap samples where both exclusion
//! bitmaps keep the sample and the threshold bitmaps disagree; the smallest count wins (ties keep
//! the earlier candidate, the centre first). The median threshold is invariant to exposure, so
//! frames of different exposure align directly.
//!
//! Studio readings (declared): every frame aligns directly to the reference frame (the paper
//! aligns neighbours); the inputs are linear, so the exclusion band is relative,
//! `|grey - median| <= median / 16` ([`MTB_EXCLUSION_FRACTION`]; the paper's band is +-4 of 255 on
//! gamma-encoded grey); the search is bounded by [`AlignOptions::max_shift`] (candidates whose
//! full-resolution offset would exceed it are not scored) and by the image (the coarsest level keeps
//! at least [`ALIGN_MIN_SIZE`] samples per side, so the overlap never empties). Medians are exact
//! (selection on a copy), not the paper's histogram.
use super::{HdrError, check_cancel};
use crate::retouch::RgbaPlane;
use hsk_studio_accord::CancellationToken;

/// Largest accepted [`AlignOptions::max_shift`].
pub const ALIGN_MAX_SHIFT: u32 = 64;
/// Smallest frame side alignment accepts; also the smallest side of the coarsest pyramid level.
pub const ALIGN_MIN_SIZE: u32 = 8;
/// Relative half-width of the exclusion band around the median.
pub const MTB_EXCLUSION_FRACTION: f32 = 1.0 / 16.0;

/// Alignment options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AlignOptions {
    /// Largest accepted offset per axis in pixels, `1..=ALIGN_MAX_SHIFT`.
    pub max_shift: u32,
}
impl Default for AlignOptions {
    fn default() -> Self {
        Self { max_shift: 16 }
    }
}

/// Offsets scored at every level, the centre first.
const CANDIDATES: [(i64, i64); 9] = [(0, 0), (-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, -1), (-1, 1), (1, 1)];

struct Grey {
    width: usize,
    height: usize,
    v: Vec<f32>,
}

fn grey(frame: &RgbaPlane) -> Grey {
    let v = frame
        .data
        .iter()
        .map(|p| ((54.0 * f64::from(p[0]) + 183.0 * f64::from(p[1]) + 19.0 * f64::from(p[2])) / 256.0) as f32)
        .collect();
    Grey {
        width: frame.bounds.width as usize,
        height: frame.bounds.height as usize,
        v,
    }
}

fn shrink(g: &Grey) -> Grey {
    let (width, height) = (g.width / 2, g.height / 2);
    let mut v = Vec::with_capacity(width * height);
    for y in 0..height {
        let r0 = 2 * y * g.width;
        let r1 = r0 + g.width;
        for x in 0..width {
            let c = 2 * x;
            let sum = f64::from(g.v[r0 + c]) + f64::from(g.v[r0 + c + 1]) + f64::from(g.v[r1 + c]) + f64::from(g.v[r1 + c + 1]);
            v.push((sum * 0.25) as f32);
        }
    }
    Grey { width, height, v }
}

struct Bitmaps {
    width: usize,
    height: usize,
    above: Vec<bool>,
    keep: Vec<bool>,
}

fn bitmaps(g: &Grey) -> Bitmaps {
    let mut sorted = g.v.clone();
    let mid = sorted.len() / 2;
    let (_, median, _) = sorted.select_nth_unstable_by(mid, f32::total_cmp);
    let median = *median;
    let band = median.abs() * MTB_EXCLUSION_FRACTION;
    Bitmaps {
        width: g.width,
        height: g.height,
        above: g.v.iter().map(|&v| v > median).collect(),
        keep: g.v.iter().map(|&v| (v - median).abs() > band).collect(),
    }
}

/// Disagreeing kept samples of `b` shifted by `(dx, dy)` (`b'(x, y) = b(x - dx, y - dy)`) against `a`.
fn mismatch(a: &Bitmaps, b: &Bitmaps, dx: i64, dy: i64) -> u64 {
    let (w, h) = (a.width as i64, a.height as i64);
    let mut err = 0u64;
    for y in 0..h {
        let sy = y - dy;
        if !(0..h).contains(&sy) {
            continue;
        }
        for x in 0..w {
            let sx = x - dx;
            if !(0..w).contains(&sx) {
                continue;
            }
            let i = (y * w + x) as usize;
            let j = (sy * w + sx) as usize;
            if a.keep[i] && b.keep[j] && a.above[i] != b.above[j] {
                err += 1;
            }
        }
    }
    err
}

/// Pyramid depth for `max_shift` on a `width` x `height` frame (module docs).
fn levels(max_shift: u32, width: u32, height: u32) -> u32 {
    let mut l = 0u32;
    while (1u64 << (l + 1)) - 1 < u64::from(max_shift) {
        l += 1;
    }
    while l > 0 && (width.min(height) >> l) < ALIGN_MIN_SIZE {
        l -= 1;
    }
    l
}

/// Validate options against the frame size.
pub(crate) fn check_align(options: &AlignOptions, width: u32, height: u32) -> Result<(), HdrError> {
    if options.max_shift == 0 || options.max_shift > ALIGN_MAX_SHIFT || width.min(height) < ALIGN_MIN_SIZE {
        return Err(HdrError::Parameter);
    }
    Ok(())
}

/// Conservative work (sample visits) of aligning one frame of `area` samples.
pub(crate) const ALIGN_WORK_PER_SAMPLE: u64 = 24;
/// Conservative bytes per sample alive while aligning (reference and frame pyramids and bitmaps).
pub(crate) const ALIGN_BYTES_PER_SAMPLE: u64 = 24;

/// The reference frame's pyramid of bitmaps, built once per merge.
pub(crate) struct ReferencePyramid {
    levels: Vec<Bitmaps>,
    max_shift: i64,
}

pub(crate) fn reference_pyramid(reference: &RgbaPlane, options: &AlignOptions) -> ReferencePyramid {
    let depth = levels(options.max_shift, reference.bounds.width, reference.bounds.height);
    ReferencePyramid {
        levels: pyramid(reference, depth),
        max_shift: i64::from(options.max_shift),
    }
}

fn pyramid(frame: &RgbaPlane, depth: u32) -> Vec<Bitmaps> {
    let mut g = grey(frame);
    let mut out = Vec::with_capacity(depth as usize + 1);
    out.push(bitmaps(&g));
    for _ in 0..depth {
        g = shrink(&g);
        out.push(bitmaps(&g));
    }
    out
}

/// Offset `(dx, dy)` that aligns `frame` to the reference: the aligned frame is
/// `frame(x - dx, y - dy)`.
pub(crate) fn align_to(reference: &ReferencePyramid, frame: &RgbaPlane, cancel: &CancellationToken) -> Result<(i64, i64), HdrError> {
    let depth = reference.levels.len() - 1;
    let frames = pyramid(frame, depth as u32);
    let mut cur = (0i64, 0i64);
    for l in (0..=depth).rev() {
        check_cancel(cancel)?;
        let (a, b) = (&reference.levels[l], &frames[l]);
        let base = if l == depth { (0, 0) } else { (cur.0 * 2, cur.1 * 2) };
        let mut best: Option<((i64, i64), u64)> = None;
        for (cx, cy) in CANDIDATES {
            let s = (base.0 + cx, base.1 + cy);
            if (s.0.abs() << l) > reference.max_shift || (s.1.abs() << l) > reference.max_shift {
                continue;
            }
            let e = mismatch(a, b, s.0, s.1);
            if best.is_none_or(|(_, be)| e < be) {
                best = Some((s, e));
            }
        }
        // The base candidate is always within the bound (it was at the coarser level).
        cur = best.map_or(base, |(s, _)| s);
    }
    Ok(cur)
}
