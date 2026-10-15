//! Content-aware scale (lane SMB-PIGMENT-CONTENT-AWARE-2; spec basis STU-RAS-143 "content-aware
//! scale ... MUST have a native on-device implementation", transform catalogue row "Content-aware
//! scale: Scales while protecting flagged content from distortion using a protection
//! mask/channel"). Seam carving written from the paper only (Avidan, Shamir 2007 "Seam Carving for
//! Content-Aware Image Resizing"); no implementation source was read.
//!
//! - Energy `e1` of a pixel = the L1 gradient of the premultiplied RGBA plane: `sum_c |p(x+1) -
//!   p(x-1)| + |p(y+1) - p(y-1)|` (a missing neighbour outside the grid is the pixel itself), plus
//!   `protect x `[`PROTECT_WEIGHT`]` x (max(w, h) + 1)` where `protect` is the protection plane's
//!   coverage in `[0, 1]`: one fully protected pixel costs more than any whole unprotected seam
//!   (`e1 <= 8` per pixel), so a seam crosses protected content only when every seam must.
//! - A vertical seam is the 8-connected top-to-bottom path of least total energy (dynamic
//!   programming `M(x, y) = e(x, y) + min(M(x-1, y-1), M(x, y-1), M(x+1, y-1))`; the last row's
//!   first minimum, then back up preferring `x-1`, `x`, `x+1` in that order on ties).
//! - Narrowing by `k` removes `k` seams one at a time (energy recomputed after each). Widening by
//!   `k` finds the `k` seams a narrowing would remove (on a copy, tracking original columns) and
//!   duplicates them in the original, the new pixel = the premultiplied mean of the seam pixel and
//!   its right neighbour (protection: the larger); widening runs in steps of at most half the
//!   current width (at least one column) so the same seams are not stretched repeatedly.
//! - Width first, then height (rows = seams of the transposed grid). The paper's optimal seam
//!   order (transport map) is not modelled: Studio reading.
//!
//! The output plane keeps the input's top-left corner: `(x, y, new_width, new_height)`. Every
//! pixel is a copy of an input pixel or (widening) a mean of two neighbours; single-threaded,
//! deterministic. Bounds (V4-01/02): sizes are checked, the working bytes
//! ([`SCALE_BYTES_PER_CELL`] per cell of the largest grid plus the output plane) and the work
//! (cell visits of every seam search) are charged before any allocation; typed refusals only.
//! Studio readings, class Approximate; Photoshop's content-aware scale (its amount and
//! protect-skin-tones controls are not modelled) is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use super::coverage_validate;
use crate::retouch::{RGBA_SAMPLE_BYTES, RetouchError, RetouchLimits, RgbaPlane, premul, straight};
use crate::selection::CoveragePlane;
use crate::{Error, Rect};

/// Protection energy per protected pixel, in units of `(max(w, h) + 1)` (the largest `e1` of one
/// pixel is 8).
pub const PROTECT_WEIGHT: f64 = 8.0;
/// Working bytes charged per cell of the largest intermediate grid (pixels, protection, energy,
/// seam table, original-column map, the copy a widening searches).
pub const SCALE_BYTES_PER_CELL: u64 = 96;
/// Cell visits charged per cell of a seam search (energy, table, back-track, copy).
pub const SCALE_WORK_PER_CELL: u64 = 4;

/// What a content-aware scale did (never a silent partial result).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScaleReceipt {
    pub removed_columns: u32,
    pub inserted_columns: u32,
    pub removed_rows: u32,
    pub inserted_rows: u32,
    /// Seam pixels that lay on protected content (coverage > 0); 0 when the protection held.
    pub protected_crossings: u64,
    /// Work charged against [`RetouchLimits::max_work`].
    pub work: u64,
}

/// Result of [`content_aware_scale`].
#[derive(Clone, Debug, PartialEq)]
pub struct ScaleResult {
    pub plane: RgbaPlane,
    pub receipt: ScaleReceipt,
}

#[derive(Clone)]
struct Grid {
    w: usize,
    h: usize,
    px: Vec<[f32; 4]>,
    protect: Vec<f32>,
}

#[inline]
fn at(w: usize, x: usize, y: usize) -> usize {
    // Coordinates inside an allocated grid: the index is below its length.
    y.wrapping_mul(w).wrapping_add(x)
}

impl Grid {
    fn transpose(&self) -> Grid {
        let mut px = Vec::with_capacity(self.px.len());
        let mut protect = Vec::with_capacity(self.protect.len());
        for x in 0..self.w {
            for y in 0..self.h {
                let i = at(self.w, x, y);
                px.push(self.px.get(i).copied().unwrap_or([0.0; 4]));
                protect.push(self.protect.get(i).copied().unwrap_or(0.0));
            }
        }
        Grid { w: self.h, h: self.w, px, protect }
    }

    fn pm(&self, x: usize, y: usize) -> [f64; 4] {
        premul(self.px.get(at(self.w, x, y)).copied().unwrap_or([0.0; 4]))
    }

    fn energy(&self) -> Vec<f64> {
        let (w, h) = (self.w, self.h);
        let weight = PROTECT_WEIGHT * (w.max(h) as f64 + 1.0);
        let mut e = Vec::with_capacity(self.px.len());
        for y in 0..h {
            for x in 0..w {
                let (l, r) = (x.saturating_sub(1), x.saturating_add(1).min(w.saturating_sub(1)));
                let (u, d) = (y.saturating_sub(1), y.saturating_add(1).min(h.saturating_sub(1)));
                let (pl, pr, pu, pd) = (self.pm(l, y), self.pm(r, y), self.pm(x, u), self.pm(x, d));
                let mut v = 0.0f64;
                for c in 0..4 {
                    v += (pr[c] - pl[c]).abs() + (pd[c] - pu[c]).abs();
                }
                let p = f64::from(self.protect.get(at(w, x, y)).copied().unwrap_or(0.0));
                e.push(v + p * weight);
            }
        }
        e
    }

    /// The vertical seam of least energy: one column per row.
    fn seam(&self) -> Vec<usize> {
        let (w, h) = (self.w, self.h);
        let e = self.energy();
        let mut m = e.clone();
        for y in 1..h {
            for x in 0..w {
                let up = y.wrapping_sub(1);
                let mut best = m.get(at(w, x, up)).copied().unwrap_or(f64::INFINITY);
                if x > 0 {
                    best = best.min(m.get(at(w, x.wrapping_sub(1), up)).copied().unwrap_or(f64::INFINITY));
                }
                if x.wrapping_add(1) < w {
                    best = best.min(m.get(at(w, x.wrapping_add(1), up)).copied().unwrap_or(f64::INFINITY));
                }
                if let Some(v) = m.get_mut(at(w, x, y)) {
                    *v += best;
                }
            }
        }
        let mut seam = vec![0usize; h];
        let last = h.saturating_sub(1);
        let mut col = 0usize;
        let mut lowest = f64::INFINITY;
        for x in 0..w {
            let v = m.get(at(w, x, last)).copied().unwrap_or(f64::INFINITY);
            if v < lowest {
                lowest = v;
                col = x;
            }
        }
        if let Some(s) = seam.get_mut(last) {
            *s = col;
        }
        for y in (0..last).rev() {
            let mut next = col;
            let mut lowest = f64::INFINITY;
            for cand in [col.checked_sub(1), Some(col), col.checked_add(1).filter(|c| *c < w)].into_iter().flatten() {
                let v = m.get(at(w, cand, y)).copied().unwrap_or(f64::INFINITY);
                if v < lowest {
                    lowest = v;
                    next = cand;
                }
            }
            col = next;
            if let Some(s) = seam.get_mut(y) {
                *s = col;
            }
        }
        seam
    }

    fn crossings(&self, seam: &[usize]) -> u64 {
        seam.iter()
            .enumerate()
            .filter(|(y, x)| self.protect.get(at(self.w, **x, *y)).copied().unwrap_or(0.0) > 0.0)
            .count() as u64
    }

    /// Remove one pixel per row (`seam[y]`); `cols` (when given) is removed in step.
    fn remove(&mut self, seam: &[usize], cols: Option<&mut Vec<usize>>) {
        let nw = self.w.saturating_sub(1);
        let mut px = Vec::with_capacity(nw.saturating_mul(self.h));
        let mut protect = Vec::with_capacity(nw.saturating_mul(self.h));
        let mut kept_cols = Vec::new();
        for y in 0..self.h {
            let s = seam.get(y).copied().unwrap_or(0);
            for x in 0..self.w {
                if x == s {
                    continue;
                }
                let i = at(self.w, x, y);
                px.push(self.px.get(i).copied().unwrap_or([0.0; 4]));
                protect.push(self.protect.get(i).copied().unwrap_or(0.0));
                if let Some(c) = cols.as_ref() {
                    kept_cols.push(c.get(i).copied().unwrap_or(0));
                }
            }
        }
        if let Some(c) = cols {
            *c = kept_cols;
        }
        self.px = px;
        self.protect = protect;
        self.w = nw;
    }

    /// Insert `k` seams (`k <= w`): the original columns a `k`-seam narrowing would remove are
    /// duplicated (new pixel = premultiplied mean of the pixel and its right neighbour).
    fn widen(&self, k: usize, crossings: &mut u64) -> Grid {
        let (w, h) = (self.w, self.h);
        let mut copy = self.clone();
        let mut cols: Vec<usize> = (0..h).flat_map(|_| 0..w).collect();
        let mut dup = vec![false; self.px.len()];
        for _ in 0..k {
            let seam = copy.seam();
            *crossings = crossings.saturating_add(copy.crossings(&seam));
            for (y, x) in seam.iter().enumerate() {
                let orig = cols.get(at(copy.w, *x, y)).copied().unwrap_or(0);
                if let Some(d) = dup.get_mut(at(w, orig, y)) {
                    *d = true;
                }
            }
            copy.remove(&seam, Some(&mut cols));
        }
        let nw = w.saturating_add(k);
        let mut px = Vec::with_capacity(nw.saturating_mul(h));
        let mut protect = Vec::with_capacity(nw.saturating_mul(h));
        for y in 0..h {
            for x in 0..w {
                let i = at(w, x, y);
                let p = self.px.get(i).copied().unwrap_or([0.0; 4]);
                let g = self.protect.get(i).copied().unwrap_or(0.0);
                px.push(p);
                protect.push(g);
                if dup.get(i).copied().unwrap_or(false) {
                    let j = at(w, x.saturating_add(1).min(w.saturating_sub(1)), y);
                    let (a, b) = (premul(p), premul(self.px.get(j).copied().unwrap_or(p)));
                    px.push(straight([(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0, (a[2] + b[2]) / 2.0, (a[3] + b[3]) / 2.0]));
                    protect.push(g.max(self.protect.get(j).copied().unwrap_or(g)));
                }
            }
        }
        Grid { w: nw, h, px, protect }
    }

    /// Change the width to `target` (>= 1) by seams; returns (removed, inserted).
    fn resize_width(&mut self, target: usize, crossings: &mut u64) -> (usize, usize) {
        let (mut removed, mut inserted) = (0usize, 0usize);
        while self.w > target {
            let seam = self.seam();
            *crossings = crossings.saturating_add(self.crossings(&seam));
            self.remove(&seam, None);
            removed = removed.saturating_add(1);
        }
        while self.w < target {
            let k = target.saturating_sub(self.w).min(self.w.checked_div(2).unwrap_or(0).max(1));
            *self = self.widen(k, crossings);
            inserted = inserted.saturating_add(k);
        }
        (removed, inserted)
    }
}

/// Seam-search work of changing one dimension from `from` to `to` on a grid whose other side is
/// `other` (upper bound: every seam searched on the larger width, plus the widening copies).
fn axis_work(from: u64, to: u64, other: u64) -> Option<u64> {
    let big = from.max(to);
    let seams = from.abs_diff(to);
    // Widening steps (each at least one seam) also rebuild the grid once per step.
    let rebuild = if to > from { seams } else { 0 };
    seams.checked_add(rebuild)?.checked_mul(big)?.checked_mul(other)?.checked_mul(SCALE_WORK_PER_CELL)
}

/// Content-aware scale of `plane` to `new_width x new_height` (module documentation), protecting
/// the pixels where `protect` (document coordinates, any bounds; outside it nothing is protected)
/// has coverage above 0 in proportion to the coverage. Sizes must be at least 1 and the plane must
/// not be empty (`retouch_out_of_range`).
pub fn content_aware_scale(
    plane: &RgbaPlane,
    new_width: u32,
    new_height: u32,
    protect: Option<&CoveragePlane>,
    limits: &RetouchLimits,
) -> Result<ScaleResult, RetouchError> {
    plane.validate()?;
    if let Some(p) = protect {
        coverage_validate(p)?;
    }
    let b = plane.bounds;
    if new_width == 0 || new_height == 0 || b.width == 0 || b.height == 0 {
        return Err(RetouchError::OutOfRange);
    }
    let out = Rect { x: b.x, y: b.y, width: new_width, height: new_height };
    out.end()?;
    let (w0, h0, w1, h1) = (u64::from(b.width), u64::from(b.height), u64::from(new_width), u64::from(new_height));
    // Bytes: the largest intermediate grid at SCALE_BYTES_PER_CELL plus the output plane.
    let cells = w0.max(w1).checked_mul(h0.max(h1)).ok_or(Error::Overflow)?;
    let bytes = cells
        .checked_mul(SCALE_BYTES_PER_CELL)
        .and_then(|v| v.checked_add(out.area().checked_mul(RGBA_SAMPLE_BYTES)?))
        .ok_or(Error::Overflow)?;
    if bytes > limits.max_bytes {
        return Err(Error::BudgetExceeded.into());
    }
    // Work: width pass on the input height, then height pass on the new width.
    let work = axis_work(w0, w1, h0)
        .and_then(|a| a.checked_add(axis_work(h0, h1, w1)?))
        .ok_or(Error::Overflow)?;
    limits.admit_work(work)?;
    let mut receipt = ScaleReceipt { work, ..ScaleReceipt::default() };
    let (w, h) = (b.width as usize, b.height as usize);
    let mut grid = Grid {
        w,
        h,
        px: plane.data.clone(),
        protect: Vec::with_capacity(plane.data.len()),
    };
    let (ex, ey) = b.end()?;
    for y in b.y..ey {
        for x in b.x..ex {
            let c = protect.map_or(0, |p| p.at(x, y));
            grid.protect.push((f64::from(c) / f64::from(u16::MAX)) as f32);
        }
    }
    let mut crossings = 0u64;
    let (rc, ic) = grid.resize_width(new_width as usize, &mut crossings);
    let mut t = grid.transpose();
    let (rr, ir) = t.resize_width(new_height as usize, &mut crossings);
    let grid = t.transpose();
    let conv = |v: usize| u32::try_from(v).map_err(|_| Error::Overflow);
    receipt.removed_columns = conv(rc)?;
    receipt.inserted_columns = conv(ic)?;
    receipt.removed_rows = conv(rr)?;
    receipt.inserted_rows = conv(ir)?;
    receipt.protected_crossings = crossings;
    if grid.px.len() as u64 != out.area() {
        return Err(RetouchError::Shape);
    }
    Ok(ScaleResult { plane: RgbaPlane { bounds: out, data: grid.px }, receipt })
}
