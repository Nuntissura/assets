//! Area type and point type (lane SMB-AI-AREA-TYPE-1; STU-TYP-002, STU-TYP-114, STU-TYP-116,
//! STU-VEC-038). Type composes a story inside a closed container; the container geometry is the
//! vector domain's (Nib) and arrives as plain [`BaselineCommand`]s in document units (y down), as
//! for text on a path, so Type keeps no geometry dependency.
//!
//! Input: shaped glyphs in logical order ([`ComposeGlyph`]: advance and break class), grouped
//! into runs that carry the line metrics ([`ComposeRun`]). A `space` glyph is a break opportunity
//! after it and hangs at the end of a soft-wrapped line (it does not count toward the line width);
//! a `paragraph` glyph ends its paragraph (forced break, its advance is ignored).
//!
//! Container: every subpath must be closed (STU-TYP-002 "closed container"); curves are flattened
//! with `flatness_pt` (Studio rule: subdivisions `n = ceil(sqrt(6 m / (8 tol)))`, `m` the largest
//! second difference of the control polygon, which bounds the chord error by `tol`); inside is the
//! non-zero winding rule over all subpaths (holes by opposite winding).
//!
//! Lines (declared readings, Illustrator parity UNVERIFIED, D5): the first baseline sits
//! `first_baseline` below the inset top of the container's bounding box (`ascent`, `cap_height`,
//! `x_height`, `leading` of the first glyph's run, or `fixed` = `first_baseline_min_pt`), never
//! less than `first_baseline_min_pt`; each later baseline is the previous one plus the leading of
//! its first glyph's run. A line's band is `[baseline - ascent, baseline + descent]` of its first
//! glyph's run (its top clamped to the inset top); the band is intersected with the container on
//! five evenly spaced scanlines spanning the band extended by the inset, the intervals inside on
//! every scanline are kept, each is shrunk by the inset on both sides, and the WIDEST is used (ties:
//! leftmost; the band rule for concave shapes is a spec gap, STU-TYP-002). Greedy fill: whole words
//! while they fit; a word that does not fit an empty line skips the band (the baseline moves down
//! one leading) when the band is narrower than the container's inset bounding box, otherwise it is
//! broken between glyphs (a glyph wider than the box stops the layout: overset). Every glyph that
//! does not fit is OVERSET and reported ([`AreaTextLayout::overset_from`]); nothing is dropped.
//! `align` places each line's ink width (trailing spaces excluded) left, centred or right inside its
//! interval. A placed glyph's transform maps glyph space (x along the advance, y UP, points) to
//! document space: `[1, 0, 0, -1, x, baseline]`.
//!
//! Auto-size (STU-TYP-116, rectangular containers only: an axis-aligned rectangle of four line
//! segments; any other container takes only `off`, the rest are refused
//! [`AreaTextError::AutoSizeNeedsRectangle`]; the top-left corner stays fixed, a declared reading):
//! `off` keeps the geometry (excess is overset); `height_only` sets the height to the content
//! (last line's band bottom plus the inset); `width_only` sets the width to the widest paragraph
//! set on one line plus both insets (no wrap); `height_and_width` does both;
//! `height_and_width_proportional` scales width and height by the smallest common factor at which
//! nothing is overset (bisection, 48 steps, the factor reported). The resized rectangle is
//! [`AreaTextLayout::container`]; the host commits it.
//!
//! Point type ([`layout_point_text`]): one line per paragraph, first baseline at the origin, the
//! same leading rule; `align` places each line left of, centred on or right of the origin.
//! [`point_to_area`] / [`area_to_point`] (STU-TYP-114) move the story value through unchanged (it
//! is generic and never inspected: every character, attribute, binding and override is kept by
//! construction) and change only the geometry: point -> area gets the rectangle of the point
//! text's line boxes (inset 0, `ascent` first baseline, same alignment), so the area layout places
//! every glyph where the point layout did (test `point_area_conversion_keeps_every_attribute`);
//! area -> point puts the origin at the first line's anchor and keeps overset text (no character is
//! added or removed, so soft wraps do not become hard returns: a declared reading of "MUST preserve
//! every character").
//!
//! Bounds (V4-01/02): commands, flattened vertices, glyphs, runs and bands are checked against
//! [`AreaTextLimits`] before or while allocating; non-finite input is refused.
#![deny(clippy::arithmetic_side_effects)]
use crate::BaselineCommand;

/// Break class of one glyph.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GlyphBreak {
    #[default]
    None,
    /// A space: break opportunity after it; hangs at the end of a soft-wrapped line.
    Space,
    /// Ends its paragraph (forced line break; advance ignored).
    Paragraph,
}

/// One shaped glyph in logical order.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ComposeGlyph {
    pub advance_pt: f64,
    pub brk: GlyphBreak,
}

/// One run of glyphs and its line metrics (points, positive distances).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ComposeRun {
    pub glyph_count: usize,
    pub ascent_pt: f64,
    pub descent_pt: f64,
    /// Baseline-to-baseline distance (must be positive).
    pub leading_pt: f64,
    pub cap_height_pt: f64,
    pub x_height_pt: f64,
}

/// First-baseline mode (declared subset; Illustrator's legacy and em-box modes are not modelled).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FirstBaseline {
    #[default]
    Ascent,
    CapHeight,
    XHeight,
    Leading,
    /// Exactly `first_baseline_min_pt`.
    Fixed,
}

/// STU-TYP-116 auto-size members.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AutoSize {
    #[default]
    Off,
    HeightOnly,
    WidthOnly,
    HeightAndWidth,
    HeightAndWidthProportional,
}

/// Horizontal placement of a line in its interval (point type: around the origin).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// Area-type options.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AreaTextOptions {
    /// Inset from the container on every side, points (`>= 0`).
    pub inset_pt: f64,
    pub first_baseline: FirstBaseline,
    pub first_baseline_min_pt: f64,
    pub auto_size: AutoSize,
    pub align: TextAlign,
    /// Curve flattening tolerance, points (`> 0`).
    pub flatness_pt: f64,
}
impl Default for AreaTextOptions {
    fn default() -> Self {
        Self { inset_pt: 0.0, first_baseline: FirstBaseline::Ascent, first_baseline_min_pt: 0.0, auto_size: AutoSize::Off, align: TextAlign::Left, flatness_pt: 0.01 }
    }
}

/// Caller bounds, checked before allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AreaTextLimits {
    pub max_commands: usize,
    /// Flattened container vertices (aggregate).
    pub max_vertices: usize,
    pub max_glyphs: usize,
    pub max_runs: usize,
    /// Bands visited (placed lines plus skipped bands).
    pub max_bands: usize,
}
impl Default for AreaTextLimits {
    fn default() -> Self {
        Self { max_commands: 1 << 16, max_vertices: 1 << 20, max_glyphs: 1 << 20, max_runs: 1 << 16, max_bands: 1 << 18 }
    }
}

/// Typed refusals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AreaTextError {
    /// Non-finite or out-of-range input (the field is named).
    InvalidInput(&'static str),
    /// Over an [`AreaTextLimits`] bound (the bound is named).
    OverLimit(&'static str),
    /// The container encloses no area.
    DegenerateContainer,
    /// A container subpath is not closed.
    OpenContainer,
    /// An auto-size member other than `off` on a non-rectangular container.
    AutoSizeNeedsRectangle,
    /// `height_and_width_proportional` found no factor up to 1e6 at which the story fits.
    AutoSizeUnfit,
}
impl AreaTextError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "area_text_invalid_input",
            Self::OverLimit(_) => "area_text_over_limit",
            Self::DegenerateContainer => "area_text_degenerate_container",
            Self::OpenContainer => "area_text_open_container",
            Self::AutoSizeNeedsRectangle => "area_text_auto_size_needs_rectangle",
            Self::AutoSizeUnfit => "area_text_auto_size_unfit",
        }
    }
}

/// One placed glyph of point or area text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedTextGlyph {
    pub index: usize,
    pub run: usize,
    pub line: usize,
    /// Glyph space (x along the advance, y up, points) to document space.
    pub transform: [f64; 6],
}

/// One composed line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ComposedLine {
    pub baseline_y: f64,
    /// The interval the line was set in (after the inset); point type: the line box.
    pub left: f64,
    pub right: f64,
    /// Pen x of the first glyph.
    pub x: f64,
    /// Ink width (trailing spaces and the paragraph glyph excluded).
    pub width_pt: f64,
    pub first_glyph: usize,
    pub glyph_count: usize,
}

/// Composed area text.
#[derive(Clone, Debug, PartialEq)]
pub struct AreaTextLayout {
    pub glyphs: Vec<PlacedTextGlyph>,
    pub lines: Vec<ComposedLine>,
    /// First glyph that did not fit; `None` when everything fits.
    pub overset_from: Option<usize>,
    pub overset_count: usize,
    /// Bounding box `[x0, y0, x1, y1]` of the container the text was set in (after auto-size).
    pub container: [f64; 4],
    /// Auto-size changed the container (the host commits [`Self::container`]).
    pub resized: bool,
    /// `height_and_width_proportional`: the factor applied; otherwise 1.
    pub scale: f64,
}

/// Composed point text.
#[derive(Clone, Debug, PartialEq)]
pub struct PointTextLayout {
    pub glyphs: Vec<PlacedTextGlyph>,
    pub lines: Vec<ComposedLine>,
    /// Union of the line boxes `[x0, y0, x1, y1]`; `None` without glyphs.
    pub bounds: Option<[f64; 4]>,
}

/// Point text: a story value at an origin (STU-TYP-114 `point`).
#[derive(Clone, Debug, PartialEq)]
pub struct PointText<T> {
    pub story: T,
    pub origin: [f64; 2],
    pub align: TextAlign,
}

/// Area text: a story value in a closed container (STU-TYP-114 `area`).
#[derive(Clone, Debug, PartialEq)]
pub struct AreaText<T> {
    pub story: T,
    pub container: Vec<BaselineCommand>,
    pub options: AreaTextOptions,
}

const EPS: f64 = 1e-9;
/// Scanline nudge inside the band (half-open edge rule).
const NUDGE: f64 = 1e-7;
const SCANLINES: usize = 5;
const MAX_SUBDIV: usize = 1024;

#[derive(Clone, Debug)]
enum Container {
    Rect { x0: f64, y0: f64, x1: f64, y1: f64 },
    Poly { edges: Vec<([f64; 2], [f64; 2])>, bbox: [f64; 4] },
}
impl Container {
    fn bbox(&self) -> [f64; 4] {
        match self {
            Container::Rect { x0, y0, x1, y1 } => [*x0, *y0, *x1, *y1],
            Container::Poly { bbox, .. } => *bbox,
        }
    }
    /// Inside intervals of the scanline `y`, sorted, disjoint.
    fn scan(&self, y: f64) -> Vec<(f64, f64)> {
        match self {
            Container::Rect { x0, y0, x1, y1 } => {
                if y >= *y0 && y < *y1 {
                    vec![(*x0, *x1)]
                } else {
                    Vec::new()
                }
            }
            Container::Poly { edges, .. } => {
                let mut xs: Vec<(f64, i32)> = Vec::new();
                for (p, q) in edges {
                    let dir = if p[1] <= y && y < q[1] {
                        1
                    } else if q[1] <= y && y < p[1] {
                        -1
                    } else {
                        continue;
                    };
                    let t = (y - p[1]) / (q[1] - p[1]);
                    xs.push((p[0] + (q[0] - p[0]) * t, dir));
                }
                xs.sort_by(|a, b| a.0.total_cmp(&b.0));
                let mut out: Vec<(f64, f64)> = Vec::new();
                let mut wind = 0_i32;
                let mut start = 0.0;
                for (x, d) in xs {
                    let before = wind;
                    wind = wind.saturating_add(d);
                    if before == 0 && wind != 0 {
                        start = x;
                    } else if before != 0 && wind == 0 && x > start {
                        match out.last_mut() {
                            Some(last) if (last.1 - start).abs() <= EPS => last.1 = x,
                            _ => out.push((start, x)),
                        }
                    }
                }
                out
            }
        }
    }
}

fn intersect(a: &[(f64, f64)], b: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let (mut i, mut j) = (0_usize, 0_usize);
    let mut out = Vec::new();
    while i < a.len() && j < b.len() {
        let lo = a[i].0.max(b[j].0);
        let hi = a[i].1.min(b[j].1);
        if hi > lo {
            out.push((lo, hi));
        }
        if a[i].1 < b[j].1 {
            i = i.saturating_add(1);
        } else {
            j = j.saturating_add(1);
        }
    }
    out
}

fn finite2(p: [f64; 2]) -> bool {
    p[0].is_finite() && p[1].is_finite()
}

/// Flattens `path` into a container (module docs).
fn container(path: &[BaselineCommand], flatness: f64, limits: &AreaTextLimits) -> Result<Container, AreaTextError> {
    if path.len() > limits.max_commands {
        return Err(AreaTextError::OverLimit("max_commands"));
    }
    if !(flatness.is_finite() && flatness > 0.0) {
        return Err(AreaTextError::InvalidInput("flatness_pt"));
    }
    let mut subpaths: Vec<Subpath> = Vec::new(); // (points, closed, all lines)
    let mut vertices = 0_usize;
    let mut push = |pts: &mut Vec<[f64; 2]>, p: [f64; 2]| -> Result<(), AreaTextError> {
        vertices = vertices.checked_add(1).ok_or(AreaTextError::OverLimit("max_vertices"))?;
        if vertices > limits.max_vertices {
            return Err(AreaTextError::OverLimit("max_vertices"));
        }
        pts.push(p);
        Ok(())
    };
    for c in path {
        let need = |p: [f64; 2]| if finite2(p) { Ok(p) } else { Err(AreaTextError::InvalidInput("container")) };
        match *c {
            BaselineCommand::MoveTo(p) => {
                let p = need(p)?;
                let mut pts = Vec::new();
                push(&mut pts, p)?;
                subpaths.push((pts, false, true));
            }
            BaselineCommand::Close => {
                let s = subpaths.last_mut().ok_or(AreaTextError::InvalidInput("container_without_moveto"))?;
                s.1 = true;
            }
            BaselineCommand::LineTo(p) => {
                let p = need(p)?;
                let s = open_subpath(&mut subpaths)?;
                push(&mut s.0, p)?;
            }
            BaselineCommand::QuadTo(q, p) => {
                let (q, p) = (need(q)?, need(p)?);
                let s = open_subpath(&mut subpaths)?;
                let cur = *s.0.last().ok_or(AreaTextError::InvalidInput("container"))?;
                let c1 = [cur[0] + (q[0] - cur[0]) * 2.0 / 3.0, cur[1] + (q[1] - cur[1]) * 2.0 / 3.0];
                let c2 = [p[0] + (q[0] - p[0]) * 2.0 / 3.0, p[1] + (q[1] - p[1]) * 2.0 / 3.0];
                flatten(s, [cur, c1, c2, p], flatness, &mut push)?;
            }
            BaselineCommand::CubicTo(a, b, p) => {
                let (a, b, p) = (need(a)?, need(b)?, need(p)?);
                let s = open_subpath(&mut subpaths)?;
                let cur = *s.0.last().ok_or(AreaTextError::InvalidInput("container"))?;
                flatten(s, [cur, a, b, p], flatness, &mut push)?;
            }
        }
    }
    let mut edges = Vec::new();
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for (pts, closed, _) in &subpaths {
        if pts.len() < 2 {
            continue;
        }
        let (first, last) = (pts[0], pts[pts.len().saturating_sub(1)]);
        if !closed && (first[0] - last[0]).hypot(first[1] - last[1]) > EPS {
            return Err(AreaTextError::OpenContainer);
        }
        for w in pts.windows(2) {
            edges.push((w[0], w[1]));
        }
        edges.push((last, first));
        for p in pts {
            lo = [lo[0].min(p[0]), lo[1].min(p[1])];
            hi = [hi[0].max(p[0]), hi[1].max(p[1])];
        }
    }
    // Signed area (non-zero rule needs some enclosed area).
    let area: f64 = edges.iter().map(|(p, q)| p[0] * q[1] - q[0] * p[1]).sum::<f64>() / 2.0;
    // NaN (overflowing coordinates) counts as degenerate.
    let thin = |v: f64| v.is_nan() || v <= EPS;
    if edges.is_empty() || thin(area.abs()) || thin(hi[0] - lo[0]) || thin(hi[1] - lo[1]) {
        return Err(AreaTextError::DegenerateContainer);
    }
    // Axis-aligned rectangle of four line segments.
    if let [(pts, _, true)] = subpaths.as_slice() {
        let mut corners: Vec<[f64; 2]> = pts.clone();
        if corners.len() == 5 && (corners[0][0] - corners[4][0]).abs() <= EPS && (corners[0][1] - corners[4][1]).abs() <= EPS {
            corners.pop();
        }
        if corners.len() == 4 {
            let on = |p: &[f64; 2]| ((p[0] - lo[0]).abs() <= EPS || (p[0] - hi[0]).abs() <= EPS) && ((p[1] - lo[1]).abs() <= EPS || (p[1] - hi[1]).abs() <= EPS);
            let axis = (0..4).all(|k| {
                let (a, b) = (corners[k], corners[k.saturating_add(1) % 4]);
                (a[0] - b[0]).abs() <= EPS || (a[1] - b[1]).abs() <= EPS
            });
            if axis && corners.iter().all(on) {
                return Ok(Container::Rect { x0: lo[0], y0: lo[1], x1: hi[0], y1: hi[1] });
            }
        }
    }
    Ok(Container::Poly { edges, bbox: [lo[0], lo[1], hi[0], hi[1]] })
}

type Subpath = (Vec<[f64; 2]>, bool, bool);

/// The current subpath, which must still be open.
fn open_subpath(subpaths: &mut [Subpath]) -> Result<&mut Subpath, AreaTextError> {
    let s = subpaths.last_mut().ok_or(AreaTextError::InvalidInput("container_without_moveto"))?;
    if s.1 {
        return Err(AreaTextError::InvalidInput("container_segment_after_close"));
    }
    Ok(s)
}

/// Flattens one cubic onto `s` (module docs: subdivision count bounds the chord error).
fn flatten(s: &mut Subpath, ctrl: [[f64; 2]; 4], flatness: f64, push: &mut impl FnMut(&mut Vec<[f64; 2]>, [f64; 2]) -> Result<(), AreaTextError>) -> Result<(), AreaTextError> {
    s.2 = false;
    let d2 = |a: [f64; 2], b: [f64; 2], c: [f64; 2]| (a[0] - 2.0 * b[0] + c[0]).hypot(a[1] - 2.0 * b[1] + c[1]);
    let m = d2(ctrl[0], ctrl[1], ctrl[2]).max(d2(ctrl[1], ctrl[2], ctrl[3]));
    let n_f = (6.0 * m / (8.0 * flatness)).sqrt().ceil();
    if !n_f.is_finite() {
        return Err(AreaTextError::InvalidInput("container"));
    }
    let n = (n_f.max(1.0) as usize).min(MAX_SUBDIV);
    for k in 1..=n {
        let t = k as f64 / n as f64;
        let u = 1.0 - t;
        let (b0, b1, b2, b3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
        let p = [
            b0 * ctrl[0][0] + b1 * ctrl[1][0] + b2 * ctrl[2][0] + b3 * ctrl[3][0],
            b0 * ctrl[0][1] + b1 * ctrl[1][1] + b2 * ctrl[2][1] + b3 * ctrl[3][1],
        ];
        push(&mut s.0, p)?;
    }
    Ok(())
}

/// Checks runs and glyphs; returns the run of every glyph.
fn check_input(runs: &[ComposeRun], glyphs: &[ComposeGlyph], max_glyphs: usize, max_runs: usize) -> Result<Vec<usize>, AreaTextError> {
    if glyphs.len() > max_glyphs {
        return Err(AreaTextError::OverLimit("max_glyphs"));
    }
    if runs.len() > max_runs {
        return Err(AreaTextError::OverLimit("max_runs"));
    }
    let counted = runs.iter().try_fold(0_usize, |acc, r| acc.checked_add(r.glyph_count)).ok_or(AreaTextError::OverLimit("max_glyphs"))?;
    if counted != glyphs.len() {
        return Err(AreaTextError::InvalidInput("run_glyph_counts"));
    }
    for r in runs {
        let v = [r.ascent_pt, r.descent_pt, r.leading_pt, r.cap_height_pt, r.x_height_pt];
        if v.iter().any(|x| !(x.is_finite() && *x >= 0.0)) || r.leading_pt <= 0.0 {
            return Err(AreaTextError::InvalidInput("run_metrics"));
        }
    }
    if glyphs.iter().any(|g| !(g.advance_pt.is_finite() && g.advance_pt >= 0.0)) {
        return Err(AreaTextError::InvalidInput("advance"));
    }
    Ok(runs.iter().enumerate().flat_map(|(i, r)| std::iter::repeat_n(i, r.glyph_count)).collect())
}

fn width_of(g: &ComposeGlyph) -> f64 {
    if g.brk == GlyphBreak::Paragraph { 0.0 } else { g.advance_pt }
}

/// The word starting at `i`: (end exclusive, ink width, trailing space width, ends paragraph).
fn word(glyphs: &[ComposeGlyph], i: usize) -> (usize, f64, f64, bool) {
    let mut j = i;
    let mut ink = 0.0;
    while j < glyphs.len() && glyphs[j].brk == GlyphBreak::None {
        ink += glyphs[j].advance_pt;
        j = j.saturating_add(1);
    }
    let mut trailing = 0.0;
    while j < glyphs.len() && glyphs[j].brk == GlyphBreak::Space {
        trailing += glyphs[j].advance_pt;
        j = j.saturating_add(1);
    }
    let mut para = false;
    if j < glyphs.len() && glyphs[j].brk == GlyphBreak::Paragraph {
        para = true;
        j = j.saturating_add(1);
    }
    (j, ink, trailing, para)
}

struct Composed {
    glyphs: Vec<PlacedTextGlyph>,
    lines: Vec<ComposedLine>,
    overset_from: Option<usize>,
    /// Bottom of the last line's band.
    bottom: Option<f64>,
}

/// Places the glyphs `[first_glyph, first_glyph + glyph_count)` of `line` from its pen `x`.
fn place_line(out: &mut Composed, glyphs: &[ComposeGlyph], run_of: &[usize], line: ComposedLine) {
    let index = out.lines.len();
    let mut pen = line.x;
    let to = line.first_glyph.saturating_add(line.glyph_count);
    for (k, g) in glyphs.iter().enumerate().take(to).skip(line.first_glyph) {
        out.glyphs.push(PlacedTextGlyph { index: k, run: run_of[k], line: index, transform: [1.0, 0.0, 0.0, -1.0, pen, line.baseline_y] });
        pen += width_of(g);
    }
    out.lines.push(line);
}

fn align_x(align: TextAlign, left: f64, avail: f64, width: f64) -> f64 {
    match align {
        TextAlign::Left => left,
        TextAlign::Center => left + (avail - width) / 2.0,
        TextAlign::Right => left + (avail - width),
    }
}

fn first_offset(o: &AreaTextOptions, r: &ComposeRun) -> f64 {
    let v = match o.first_baseline {
        FirstBaseline::Ascent => r.ascent_pt,
        FirstBaseline::CapHeight => r.cap_height_pt,
        FirstBaseline::XHeight => r.x_height_pt,
        FirstBaseline::Leading => r.leading_pt,
        FirstBaseline::Fixed => o.first_baseline_min_pt,
    };
    v.max(o.first_baseline_min_pt)
}

/// Greedy composition into `c` (module docs). `bottomless`: a rectangle without a bottom edge.
fn compose(c: &Container, bottomless: bool, runs: &[ComposeRun], glyphs: &[ComposeGlyph], run_of: &[usize], o: &AreaTextOptions, limits: &AreaTextLimits) -> Result<Composed, AreaTextError> {
    let bb = c.bbox();
    let inset = o.inset_pt;
    let top = bb[1] + inset;
    let bottom = if bottomless { f64::INFINITY } else { bb[3] - inset };
    let full = bb[2] - bb[0] - 2.0 * inset;
    let mut out = Composed { glyphs: Vec::with_capacity(glyphs.len()), lines: Vec::new(), overset_from: None, bottom: None };
    let mut i = 0_usize;
    let mut baseline: Option<f64> = None;
    let mut bands = 0_usize;
    while i < glyphs.len() {
        bands = bands.saturating_add(1);
        if bands > limits.max_bands {
            return Err(AreaTextError::OverLimit("max_bands"));
        }
        let r = &runs[run_of[i]];
        let b = match baseline {
            None => top + first_offset(o, r),
            Some(prev) => prev + r.leading_pt,
        };
        baseline = Some(b);
        let band_top = (b - r.ascent_pt).max(top);
        let band_bottom = b + r.descent_pt;
        if band_bottom > bottom + EPS || !band_top.is_finite() {
            break;
        }
        // Intervals inside on every scanline of the band extended by the inset.
        let (s0, s1) = (band_top - inset, band_bottom + inset);
        let mut kept: Option<Vec<(f64, f64)>> = None;
        let rect_band = bottomless && matches!(c, Container::Rect { .. });
        for k in 0..SCANLINES {
            let f = k as f64 / (SCANLINES.saturating_sub(1)) as f64;
            let y = (s0 + NUDGE) + (s1 - s0 - 2.0 * NUDGE).max(0.0) * f;
            let iv = if rect_band { vec![(bb[0], bb[2])] } else { c.scan(y) };
            kept = Some(match kept {
                None => iv,
                Some(prev) => intersect(&prev, &iv),
            });
        }
        let widest = kept
            .unwrap_or_default()
            .into_iter()
            .map(|(a, z)| (a + inset, z - inset))
            .filter(|(a, z)| z >= a)
            .fold(None::<(f64, f64)>, |best, iv| match best {
                Some(bst) if bst.1 - bst.0 >= iv.1 - iv.0 => Some(bst),
                _ => Some(iv),
            });
        let Some((left, right)) = widest else {
            continue;
        };
        let avail = right - left;
        // Fill the line.
        let from = i;
        let mut width = 0.0_f64;
        let mut pending = 0.0_f64;
        let mut end = i;
        while end < glyphs.len() {
            let (next, ink, trailing, para) = word(glyphs, end);
            if end == from || width + pending + ink <= avail + EPS {
                if end == from && ink > avail + EPS {
                    // An empty line that cannot hold the word.
                    if avail < full - EPS {
                        break; // narrowed band: skip it
                    }
                    // Full-width band: break between glyphs.
                    let mut k = end;
                    let mut w = 0.0;
                    while k < glyphs.len() && glyphs[k].brk == GlyphBreak::None && w + glyphs[k].advance_pt <= avail + EPS {
                        w += glyphs[k].advance_pt;
                        k = k.saturating_add(1);
                    }
                    if k == end {
                        out.overset_from = Some(i);
                        return Ok(out);
                    }
                    width = w;
                    end = k;
                    break;
                }
                width += pending + ink;
                pending = trailing;
                end = next;
                if para {
                    break;
                }
            } else {
                break;
            }
        }
        if end == from {
            continue; // skipped band
        }
        let x = align_x(o.align, left, avail, width);
        place_line(&mut out, glyphs, run_of, ComposedLine { baseline_y: b, left, right, x, width_pt: width, first_glyph: from, glyph_count: end.saturating_sub(from) });
        out.bottom = Some(band_bottom);
        i = end;
    }
    if i < glyphs.len() {
        out.overset_from = Some(i);
    }
    Ok(out)
}

fn check_options(o: &AreaTextOptions) -> Result<(), AreaTextError> {
    if !(o.inset_pt.is_finite() && o.inset_pt >= 0.0) {
        return Err(AreaTextError::InvalidInput("inset_pt"));
    }
    if !(o.first_baseline_min_pt.is_finite() && o.first_baseline_min_pt >= 0.0) {
        return Err(AreaTextError::InvalidInput("first_baseline_min_pt"));
    }
    Ok(())
}

/// Widest paragraph set on one line (ink width).
fn widest_paragraph(glyphs: &[ComposeGlyph]) -> f64 {
    let (mut best, mut i) = (0.0_f64, 0_usize);
    while i < glyphs.len() {
        let (mut width, mut pending) = (0.0, 0.0);
        loop {
            let (next, ink, trailing, para) = word(glyphs, i);
            width += pending + ink;
            pending = trailing;
            i = next;
            if para || i >= glyphs.len() {
                break;
            }
        }
        best = best.max(width);
    }
    best
}

fn finish(c: Composed, glyph_total: usize, container: [f64; 4], resized: bool, scale: f64) -> AreaTextLayout {
    let overset_count = c.overset_from.map_or(0, |f| glyph_total.saturating_sub(f));
    AreaTextLayout { glyphs: c.glyphs, lines: c.lines, overset_from: c.overset_from, overset_count, container, resized, scale }
}

/// Composes `glyphs` (logical order, grouped by `runs`) inside `container` (module docs).
pub fn layout_area_text(
    container_path: &[BaselineCommand],
    runs: &[ComposeRun],
    glyphs: &[ComposeGlyph],
    options: &AreaTextOptions,
    limits: &AreaTextLimits,
) -> Result<AreaTextLayout, AreaTextError> {
    let run_of = check_input(runs, glyphs, limits.max_glyphs, limits.max_runs)?;
    check_options(options)?;
    let c = container(container_path, options.flatness_pt, limits)?;
    let bb = c.bbox();
    let o = options;
    if o.auto_size == AutoSize::Off {
        let out = compose(&c, false, runs, glyphs, &run_of, o, limits)?;
        return Ok(finish(out, glyphs.len(), bb, false, 1.0));
    }
    let Container::Rect { x0, y0, x1, y1 } = c else {
        return Err(AreaTextError::AutoSizeNeedsRectangle);
    };
    // `off` returned above.
    let rect = |w: f64, h: f64| Container::Rect { x0, y0, x1: x0 + w, y1: y0 + h };
    let (w, h) = (x1 - x0, y1 - y0);
    let content_width = widest_paragraph(glyphs) + 2.0 * o.inset_pt;
    match o.auto_size {
        AutoSize::Off | AutoSize::WidthOnly => {
            let r = rect(content_width.max(EPS), h);
            let out = compose(&r, false, runs, glyphs, &run_of, o, limits)?;
            Ok(finish(out, glyphs.len(), r.bbox(), true, 1.0))
        }
        AutoSize::HeightOnly | AutoSize::HeightAndWidth => {
            let width = if o.auto_size == AutoSize::HeightOnly { w } else { content_width.max(EPS) };
            let tall = rect(width, f64::INFINITY);
            let out = compose(&tall, true, runs, glyphs, &run_of, o, limits)?;
            let height = out.bottom.map_or(2.0 * o.inset_pt, |b| b + o.inset_pt - y0).max(0.0);
            let r = [x0, y0, x0 + width, y0 + height];
            Ok(finish(out, glyphs.len(), r, true, 1.0))
        }
        AutoSize::HeightAndWidthProportional => {
            let fits = |s: f64| -> Result<Option<Composed>, AreaTextError> {
                let out = compose(&rect(w * s, h * s), false, runs, glyphs, &run_of, o, limits)?;
                Ok(out.overset_from.is_none().then_some(out))
            };
            let mut hi = 1.0_f64;
            while fits(hi)?.is_none() {
                hi *= 2.0;
                if hi > 1.0e6 {
                    return Err(AreaTextError::AutoSizeUnfit);
                }
            }
            let mut lo = 0.0_f64;
            for _ in 0..48 {
                let mid = (lo + hi) / 2.0;
                if mid <= 0.0 {
                    break;
                }
                if fits(mid)?.is_some() {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            let out = fits(hi)?.ok_or(AreaTextError::AutoSizeUnfit)?;
            Ok(finish(out, glyphs.len(), [x0, y0, x0 + w * hi, y0 + h * hi], true, hi))
        }
    }
}

/// Point type: one line per paragraph from `origin` (module docs).
pub fn layout_point_text(origin: [f64; 2], runs: &[ComposeRun], glyphs: &[ComposeGlyph], align: TextAlign, limits: &AreaTextLimits) -> Result<PointTextLayout, AreaTextError> {
    let run_of = check_input(runs, glyphs, limits.max_glyphs, limits.max_runs)?;
    if !finite2(origin) {
        return Err(AreaTextError::InvalidInput("origin"));
    }
    let mut out = Composed { glyphs: Vec::with_capacity(glyphs.len()), lines: Vec::new(), overset_from: None, bottom: None };
    let mut bounds: Option<[f64; 4]> = None;
    let (mut i, mut baseline) = (0_usize, None::<f64>);
    while i < glyphs.len() {
        let r = &runs[run_of[i]];
        let b = baseline.map_or(origin[1], |p| p + r.leading_pt);
        baseline = Some(b);
        let from = i;
        let (mut width, mut pending) = (0.0, 0.0);
        loop {
            let (next, ink, trailing, para) = word(glyphs, i);
            width += pending + ink;
            pending = trailing;
            i = next;
            if para || i >= glyphs.len() {
                break;
            }
        }
        let x = match align {
            TextAlign::Left => origin[0],
            TextAlign::Center => origin[0] - width / 2.0,
            TextAlign::Right => origin[0] - width,
        };
        place_line(&mut out, glyphs, &run_of, ComposedLine { baseline_y: b, left: x, right: x + width, x, width_pt: width, first_glyph: from, glyph_count: i.saturating_sub(from) });
        let line_box = [x, b - r.ascent_pt, x + width, b + r.descent_pt];
        bounds = Some(match bounds {
            None => line_box,
            Some(u) => [u[0].min(line_box[0]), u[1].min(line_box[1]), u[2].max(line_box[2]), u[3].max(line_box[3])],
        });
    }
    Ok(PointTextLayout { glyphs: out.glyphs, lines: out.lines, bounds })
}

/// Point -> area (STU-TYP-114; module docs): the story moves through untouched.
pub fn point_to_area<T>(text: PointText<T>, layout: &PointTextLayout) -> Result<AreaText<T>, AreaTextError> {
    let [x0, y0, x1, y1] = layout.bounds.ok_or(AreaTextError::InvalidInput("empty_point_text"))?;
    // A zero-width line set (only spaces) still needs an area: one point wide.
    let x1 = if x1 - x0 > EPS { x1 } else { x0 + 1.0 };
    let container = vec![
        BaselineCommand::MoveTo([x0, y0]),
        BaselineCommand::LineTo([x1, y0]),
        BaselineCommand::LineTo([x1, y1]),
        BaselineCommand::LineTo([x0, y1]),
        BaselineCommand::Close,
    ];
    let options = AreaTextOptions { align: text.align, ..AreaTextOptions::default() };
    Ok(AreaText { story: text.story, container, options })
}

/// Area -> point (STU-TYP-114; module docs): origin at the first line's anchor.
pub fn area_to_point<T>(text: AreaText<T>, layout: &AreaTextLayout) -> PointText<T> {
    let align = text.options.align;
    let origin = match layout.lines.first() {
        Some(l) => {
            let x = match align {
                TextAlign::Left => l.x,
                TextAlign::Center => l.x + l.width_pt / 2.0,
                TextAlign::Right => l.x + l.width_pt,
            };
            [x, l.baseline_y]
        }
        None => [layout.container[0] + text.options.inset_pt, layout.container[1] + text.options.inset_pt],
    };
    PointText { story: text.story, origin, align }
}
