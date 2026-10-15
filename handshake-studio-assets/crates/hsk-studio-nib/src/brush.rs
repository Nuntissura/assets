//! Art, scatter and pattern brushes along a path (lane SMB-ILLUSTRATOR-TOOLS-2; STU-VEC-026,
//! STU-VEC-150, STU-VEC-152, STU-VEC-162, STU-VEC-163). Nib owns the geometry: each function maps
//! brush art (paths in the art's own frame) along every subpath of a spine and returns, per art
//! path, the united output (one `OwnedPath` per input art path, so a caller paints each with that
//! art path's paint).
//!
//! Frame: the art's union bounds give the length axis (x, left to right = along the path) and
//! the width axis (y, centred on the bounds' middle; +y maps to the spine's right-hand normal on
//! screen, y down). A spine point at arc length `s` with unit tangent `t` maps art point
//! `(s, d)` to `P(s) + d (-t_y, t_x)`; beyond the ends the end tangent extends the spine. Art is
//! flattened (0.05 px chords) and subdivided to 1 px before mapping (class Approximate).
//!
//! * [`art_brush`]: the art spans the whole subpath. Scale `Proportional` scales the width by the
//!   same factor as the length (times `width` percent); `Stretch` keeps the art's own width
//!   (times `width` percent); `StretchBetweenGuides` keeps the art outside the guides at its own
//!   length at each end and stretches the middle. `flip_along` / `flip_across` mirror the art.
//! * [`scatter_brush`]: copies of the art (centred on its bounds) every `spacing` percent of the
//!   scaled art width along the path, first copy half a step in; each copy scaled by `size`
//!   percent, rotated by `rotation` degrees (relative to the page or to the path tangent) and
//!   moved across the path by `offset` percent of the scaled art height. A parameter with
//!   `random` takes a seeded uniform value between its min and max per copy (STU-VEC-150
//!   `dynamics.random`); otherwise its min. At most [`MAX_SCATTER_COPIES`] copies.
//! * [`pattern_brush`]: the spine subpath is cut at anchors turning more than
//!   [`CORNER_TURN_DEG`]; the start tile opens the first run, the end tile closes the last one, an
//!   outer (turning left on screen) or inner corner tile is centred on each corner and takes half
//!   its length from each neighbouring run; each run takes `n = round(run / tile)` side tiles
//!   (at least one): `StretchToFit` stretches them to fill the run, `AddSpaceToFit` keeps their
//!   length and spaces the remainder evenly; `ApproximatePath` is laid out like `StretchToFit`
//!   (declared gap). Tiles are scaled by `scale` percent along and across.
//!
//! Angles use nib's polynomial `shape::sin_cos_deg` (no platform trigonometry). Studio rules throughout; Illustrator brush parity UNVERIFIED (D5). Calligraphic and bristle
//! brushes are not drawn here.
#![deny(clippy::arithmetic_side_effects)]
use crate::arrange::{Bounds, unit_random, union_bounds};
use crate::boolean::{BooleanOp, BooleanOptions, boolean};
use crate::flatten::{FlattenOptions, flatten};
use crate::warp::map_paths;
use crate::shape::sin_cos_deg;
use crate::{Error, OwnedPath, PathElement, PathPoint};

/// Most scatter copies (and pattern tiles) one brush call places.
pub const MAX_SCATTER_COPIES: usize = 4096;
/// A pattern-brush corner: an anchor where the path turns more than this.
pub const CORNER_TURN_DEG: f64 = 10.0;
/// Chord tolerance of art and spines.
const TOLERANCE: f64 = 0.05;
/// Largest mapped piece.
const PIECE: f64 = 1.0;

/// An arc-length parameterised spine subpath.
struct Spine {
    points: Vec<PathPoint>,
    lengths: Vec<f64>,
    /// Indices of corner anchors with their turn side (+1 = left on screen, -1 = right).
    corners: Vec<(usize, f64)>,
}

impl Spine {
    fn total(&self) -> f64 {
        self.lengths.last().copied().unwrap_or(0.0)
    }
    /// Point and unit tangent at arc length `s` (extended past the ends).
    fn at(&self, s: f64) -> (PathPoint, PathPoint) {
        let n = self.points.len();
        let i = self.lengths.partition_point(|l| *l < s).clamp(1, n.saturating_sub(1).max(1));
        let (a, b) = (self.points[i.saturating_sub(1)], self.points[i.min(n.saturating_sub(1))]);
        let (l0, l1) = (self.lengths[i.saturating_sub(1)], self.lengths[i.min(n.saturating_sub(1))]);
        let len = l1 - l0;
        let t = if len > 0.0 { PathPoint::new((b.x - a.x) / len, (b.y - a.y) / len) } else { PathPoint::new(1.0, 0.0) };
        let f = s - l0;
        (PathPoint::new(a.x + t.x * f, a.y + t.y * f), t)
    }
}

fn spines(path: &OwnedPath) -> Result<Vec<Spine>, Error> {
    let lines = flatten(path, &FlattenOptions { tolerance: TOLERANCE, ..FlattenOptions::default() })?;
    let mut out = Vec::new();
    for l in lines {
        let mut points = l.points.clone();
        let mut anchors = l.anchors.clone();
        if l.closed && let Some(f) = points.first().copied() {
            points.push(f);
            anchors.push(true);
        }
        if points.len() < 2 {
            continue;
        }
        let mut lengths = vec![0.0];
        for w in points.windows(2) {
            let last = lengths.last().copied().unwrap_or(0.0);
            lengths.push(last + (sq(w[1].x - w[0].x) + sq(w[1].y - w[0].y)).sqrt());
        }
        let mut corners = Vec::new();
        for i in 1..points.len().saturating_sub(1) {
            if !anchors.get(i).copied().unwrap_or(false) {
                continue;
            }
            let (a, b, c) = (points[i.saturating_sub(1)], points[i], points[i.saturating_add(1)]);
            let (ux, uy, vx, vy) = (b.x - a.x, b.y - a.y, c.x - b.x, c.y - b.y);
            // Screen y down: a left turn on screen has a negative cross product. Turn above the
            // threshold: cos(turn) below cos(threshold) (no platform trigonometry).
            let (lu, lv) = ((ux * ux + uy * uy).sqrt(), (vx * vx + vy * vy).sqrt());
            if lu > 0.0 && lv > 0.0 && (ux * vx + uy * vy) / (lu * lv) < sin_cos_deg(CORNER_TURN_DEG).1 {
                let left = ux * vy - uy * vx < 0.0;
                corners.push((i, if left { 1.0 } else { -1.0 }));
            }
        }
        out.push(Spine { points, lengths, corners });
    }
    Ok(out)
}

/// Maps art in [`Bounds`] frame `b` so that art x in `[x0, x1]` covers spine arc length
/// `[s0, s1]` and art y offset from the middle is scaled by `across`.
#[allow(clippy::too_many_arguments)]
fn lay(art: &[OwnedPath], b: &Bounds, spine: &Spine, x0: f64, x1: f64, s0: f64, s1: f64, across: f64, flip_along: bool, flip_across: bool) -> Result<Vec<OwnedPath>, Error> {
    let mid = b.centre()[1];
    let span = (x1 - x0).max(f64::MIN_POSITIVE);
    let f = |q: PathPoint| {
        let x = if flip_along { b.max[0] - (q.x - b.min[0]) } else { q.x };
        let d = (q.y - mid) * across * if flip_across { -1.0 } else { 1.0 };
        let s = s0 + (x - x0) / span * (s1 - s0);
        let (p, t) = spine.at(s);
        PathPoint::new(p.x - t.y * d, p.y + t.x * d)
    };
    map_paths(art, TOLERANCE, PIECE, &f)
}

/// Unites `parts[k]` into `acc[k]`.
fn accumulate(acc: &mut [Vec<OwnedPath>], parts: Vec<OwnedPath>) {
    for (a, p) in acc.iter_mut().zip(parts) {
        a.push(p);
    }
}

fn unite(acc: Vec<Vec<OwnedPath>>) -> Result<Vec<OwnedPath>, Error> {
    acc.into_iter()
        .map(|parts| {
            let drawn: Vec<OwnedPath> = parts.into_iter().filter(|p| !p.elements.is_empty()).collect();
            match drawn.len() {
                0 => Ok(OwnedPath::new(crate::FillRule::NonZero)),
                1 => Ok(drawn.into_iter().next().unwrap_or_else(|| OwnedPath::new(crate::FillRule::NonZero))),
                _ => {
                    // Copies keep their own fill rule; overlapping copies are united.
                    let mut out = OwnedPath::new(crate::FillRule::NonZero);
                    match boolean(&drawn, BooleanOp::Union, &BooleanOptions::default()) {
                        Ok(g) => out = g.path,
                        Err(Error::EmptyPathfinderResult) => {}
                        Err(e) => return Err(e),
                    }
                    Ok(out)
                }
            }
        })
        .collect()
}

fn art_frame(art: &[OwnedPath]) -> Result<Bounds, Error> {
    let b = union_bounds(art).ok_or(Error::InvalidGeometry)?;
    if b.width() <= 0.0 {
        return Err(Error::InvalidGeometry);
    }
    Ok(b)
}

/// Art-brush scale mode (STU-VEC-152 `art_scale_mode`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ArtScale {
    Proportional,
    Stretch,
    /// Guides in art x coordinates.
    StretchBetweenGuides { start: f64, end: f64 },
}

/// Art brush (STU-VEC-152).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArtBrush {
    /// Percent (soft 1..1000).
    pub width: f64,
    pub scale: ArtScale,
    pub flip_along: bool,
    pub flip_across: bool,
}

/// The art stretched along every subpath of `spine` (module docs).
pub fn art_brush(spine: &OwnedPath, art: &[OwnedPath], brush: &ArtBrush) -> Result<Vec<OwnedPath>, Error> {
    if !(brush.width.is_finite() && brush.width > 0.0 && brush.width <= 100_000.0) {
        return Err(Error::InvalidRequest);
    }
    let b = art_frame(art)?;
    let w = brush.width / 100.0;
    let mut acc = vec![Vec::new(); art.len()];
    for sp in spines(spine)? {
        let l = sp.total();
        if l <= 0.0 {
            continue;
        }
        match brush.scale {
            ArtScale::Proportional => accumulate(&mut acc, lay(art, &b, &sp, b.min[0], b.max[0], 0.0, l, l / b.width() * w, brush.flip_along, brush.flip_across)?),
            ArtScale::Stretch => accumulate(&mut acc, lay(art, &b, &sp, b.min[0], b.max[0], 0.0, l, w, brush.flip_along, brush.flip_across)?),
            ArtScale::StretchBetweenGuides { start, end } => {
                if !(start.is_finite() && end.is_finite() && b.min[0] <= start && start < end && end <= b.max[0]) {
                    return Err(Error::InvalidRequest);
                }
                let (head, tail) = (start - b.min[0], b.max[0] - end);
                let k = if head + tail > l { l / (head + tail) } else { 1.0 };
                let (h, t) = (head * k, tail * k);
                // One continuous piecewise map: [min, start] -> [0, h], [start, end] -> [h, l - t], [end, max] -> [l - t, l].
                let mid = b.centre()[1];
                let (fa, fc) = (brush.flip_along, brush.flip_across);
                let f = |q: PathPoint| {
                    let x = if fa { b.max[0] - (q.x - b.min[0]) } else { q.x };
                    let s = if x <= start {
                        (x - b.min[0]) * k
                    } else if x >= end {
                        l - t + (x - end) * k
                    } else {
                        h + (x - start) / (end - start) * (l - t - h)
                    };
                    let d = (q.y - mid) * w * if fc { -1.0 } else { 1.0 };
                    let (p, tg) = sp.at(s);
                    PathPoint::new(p.x - tg.y * d, p.y + tg.x * d)
                };
                accumulate(&mut acc, map_paths(art, TOLERANCE, PIECE, &f)?);
            }
        }
    }
    unite(acc)
}

/// A brush parameter (STU-VEC-150): fixed (`min`) or random between `min` and `max`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dyn {
    pub min: f64,
    pub max: f64,
    pub random: bool,
}

impl Dyn {
    pub const fn fixed(v: f64) -> Self {
        Self { min: v, max: v, random: false }
    }
    fn value(&self, seed: u64, stream: u64, i: u64) -> f64 {
        if self.random { self.min + (self.max - self.min) * unit_random(seed, stream, i) } else { self.min }
    }
    fn within(&self, lo: f64, hi: f64) -> bool {
        [self.min, self.max].iter().all(|v| v.is_finite() && (lo..=hi).contains(v))
    }
}

/// `scatter_rotation_relative_to`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RotationRelative {
    Page,
    Path,
}

/// Scatter brush (STU-VEC-152 hard ranges: size and spacing 1..10000 %, offset -10000..10000 %,
/// rotation -360..360 deg).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScatterBrush {
    pub size: Dyn,
    pub spacing: Dyn,
    pub offset: Dyn,
    pub rotation: Dyn,
    pub relative: RotationRelative,
    pub seed: u64,
}

/// Copies of the art along every subpath of `spine` (module docs).
pub fn scatter_brush(spine: &OwnedPath, art: &[OwnedPath], brush: &ScatterBrush) -> Result<Vec<OwnedPath>, Error> {
    if !(brush.size.within(1.0, 10_000.0) && brush.spacing.within(1.0, 10_000.0) && brush.offset.within(-10_000.0, 10_000.0) && brush.rotation.within(-360.0, 360.0)) {
        return Err(Error::InvalidRequest);
    }
    let b = art_frame(art)?;
    let c = b.centre();
    let mut acc = vec![Vec::new(); art.len()];
    let mut copies = 0_usize;
    for sp in spines(spine)? {
        let l = sp.total();
        let mut s = 0.0;
        let mut first = true;
        while s <= l {
            let i = copies as u64;
            let k = brush.size.value(brush.seed, 1, i) / 100.0;
            let step = b.width() * k * brush.spacing.value(brush.seed, 2, i) / 100.0;
            if first {
                s = step / 2.0;
                first = false;
                if s > l {
                    break;
                }
            }
            copies = copies.checked_add(1).filter(|n| *n <= MAX_SCATTER_COPIES).ok_or(Error::BudgetExceeded)?;
            let (p, t) = sp.at(s);
            let off = brush.offset.value(brush.seed, 3, i) / 100.0 * b.height() * k;
            let (rs, rc) = sin_cos_deg(brush.rotation.value(brush.seed, 4, i));
            // Rotation relative to the path composes with the unit tangent (cos, sin) directly.
            let (sn, cs) = match brush.relative {
                RotationRelative::Page => (rs, rc),
                RotationRelative::Path => (t.y * rc + t.x * rs, t.x * rc - t.y * rs),
            };
            let at = PathPoint::new(p.x - t.y * off, p.y + t.x * off);
            let f = |q: PathPoint| {
                let (x, y) = ((q.x - c[0]) * k, (q.y - c[1]) * k);
                PathPoint::new(at.x + x * cs - y * sn, at.y + x * sn + y * cs)
            };
            accumulate(&mut acc, map_paths(art, TOLERANCE, PIECE, &f)?);
            if step <= 0.0 {
                break;
            }
            s += step;
        }
    }
    unite(acc)
}

/// Pattern-brush tile slots (STU-VEC-152: five named references).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileSlot {
    Side,
    OuterCorner,
    InnerCorner,
    Start,
    End,
}

/// One art path of a tile.
#[derive(Clone, Debug, PartialEq)]
pub struct TileArt {
    pub slot: TileSlot,
    pub path: OwnedPath,
}

/// `fit_mode` (captured 1, 2, 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatternFit {
    StretchToFit,
    AddSpaceToFit,
    ApproximatePath,
}

/// Pattern brush (STU-VEC-152).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PatternBrush {
    /// Percent.
    pub scale: f64,
    pub fit: PatternFit,
    pub flip_along: bool,
    pub flip_across: bool,
}

/// Tiles along every subpath of `spine` (module docs); one output per input tile art path.
pub fn pattern_brush(spine: &OwnedPath, tiles: &[TileArt], brush: &PatternBrush) -> Result<Vec<OwnedPath>, Error> {
    if !(brush.scale.is_finite() && brush.scale > 0.0 && brush.scale <= 100_000.0) {
        return Err(Error::InvalidRequest);
    }
    let k = brush.scale / 100.0;
    // Per slot: the art indices and the slot frame.
    let frame = |slot: TileSlot| -> Result<Option<(Vec<usize>, Bounds)>, Error> {
        let idx: Vec<usize> = tiles.iter().enumerate().filter(|(_, t)| t.slot == slot).map(|(i, _)| i).collect();
        if idx.is_empty() {
            return Ok(None);
        }
        let paths: Vec<OwnedPath> = idx.iter().map(|i| tiles[*i].path.clone()).collect();
        Ok(Some((idx, art_frame(&paths)?)))
    };
    let side = frame(TileSlot::Side)?.ok_or(Error::InvalidRequest)?;
    let (start, end, outer, inner) = (frame(TileSlot::Start)?, frame(TileSlot::End)?, frame(TileSlot::OuterCorner)?, frame(TileSlot::InnerCorner)?);
    let mut acc = vec![Vec::new(); tiles.len()];
    let mut placed = 0_usize;
    let mut place = |slot: &(Vec<usize>, Bounds), sp: &Spine, s0: f64, s1: f64, acc: &mut Vec<Vec<OwnedPath>>| -> Result<(), Error> {
        placed = placed.checked_add(1).filter(|n| *n <= MAX_SCATTER_COPIES).ok_or(Error::BudgetExceeded)?;
        let paths: Vec<OwnedPath> = slot.0.iter().map(|i| tiles[*i].path.clone()).collect();
        let out = lay(&paths, &slot.1, sp, slot.1.min[0], slot.1.max[0], s0, s1, k, brush.flip_along, brush.flip_across)?;
        for (i, p) in slot.0.iter().zip(out) {
            if let Some(a) = acc.get_mut(*i) {
                a.push(p);
            }
        }
        Ok(())
    };
    for sp in spines(spine)? {
        let l = sp.total();
        // Cuts: (arc length of the cut, half length taken by a corner tile, corner slot).
        let mut bounds_s = vec![0.0];
        let mut corner_tiles = Vec::new();
        for (i, turn) in &sp.corners {
            let s = sp.lengths[*i];
            let slot = if *turn > 0.0 { outer.as_ref() } else { inner.as_ref() };
            let half = slot.map_or(0.0, |c| c.1.width() * k / 2.0);
            corner_tiles.push((s, half, slot));
            bounds_s.push(s);
        }
        bounds_s.push(l);
        // Runs between cuts, trimmed by corner halves and the start / end tiles.
        let runs = bounds_s.len().saturating_sub(1);
        for r in 0..runs {
            let mut a = bounds_s[r];
            let mut z = bounds_s[r.saturating_add(1)];
            if r > 0 {
                a += corner_tiles[r.saturating_sub(1)].1;
            }
            if r.saturating_add(1) < runs {
                z -= corner_tiles[r].1;
            }
            if r == 0 && let Some(st) = &start {
                let len = st.1.width() * k;
                place(st, &sp, a, a + len, &mut acc)?;
                a += len;
            }
            if r.saturating_add(1) == runs && let Some(en) = &end {
                let len = en.1.width() * k;
                place(en, &sp, z - len, z, &mut acc)?;
                z -= len;
            }
            let run = z - a;
            let tile = side.1.width() * k;
            if run <= 0.0 || tile <= 0.0 {
                continue;
            }
            match brush.fit {
                PatternFit::StretchToFit | PatternFit::ApproximatePath => {
                    let n = (run / tile).round().max(1.0);
                    if n > MAX_SCATTER_COPIES as f64 {
                        return Err(Error::BudgetExceeded);
                    }
                    let step = run / n;
                    let mut s = a;
                    for _ in 0..(n as usize) {
                        place(&side, &sp, s, s + step, &mut acc)?;
                        s += step;
                    }
                }
                PatternFit::AddSpaceToFit => {
                    let n = (run / tile).floor().max(1.0);
                    if n > MAX_SCATTER_COPIES as f64 {
                        return Err(Error::BudgetExceeded);
                    }
                    let gap = ((run - n * tile) / n).max(0.0);
                    let mut s = a + gap / 2.0;
                    for _ in 0..(n as usize) {
                        place(&side, &sp, s, s + tile, &mut acc)?;
                        s += tile + gap;
                    }
                }
            }
        }
        for (s, half, slot) in corner_tiles {
            if let Some(c) = slot {
                place(c, &sp, s - half, s + half, &mut acc)?;
            }
        }
    }
    unite(acc)
}

/// Closed rectangle `x0, y0, x1, y1` (art helper).
pub fn tile_rect(x0: f64, y0: f64, x1: f64, y1: f64) -> OwnedPath {
    let mut o = OwnedPath::new(crate::FillRule::NonZero);
    let p = PathPoint::new;
    o.elements = vec![PathElement::MoveTo(p(x0, y0)), PathElement::LineTo(p(x1, y0)), PathElement::LineTo(p(x1, y1)), PathElement::LineTo(p(x0, y1)), PathElement::Close];
    o
}

fn sq(v: f64) -> f64 {
    v * v
}
