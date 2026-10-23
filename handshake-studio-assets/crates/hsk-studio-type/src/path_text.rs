//! Text on a path (lane SMB-AI-TYPE-PATH-RECOLOR; STU-TYP-004, STU-TYP-120, STU-TYP-121,
//! STU-VEC-038, STU-LAY-023). Type owns the layout of a story along a path; the path geometry is
//! the vector domain's (Nib) and arrives here as plain [`BaselineCommand`]s in document units
//! (y down), so Type keeps no geometry dependency.
//!
//! Model: glyphs (shaped advances in visual order, grouped into runs that carry the run metrics
//! and the per-run `baseline_offset`) are laid along the arc length of ONE open or closed subpath
//! between the flow-extent handles `start_t` and `end_t`. A handle value is segment-relative
//! (STU-TYP-120): integer part = segment index, fraction = the curve parameter inside that
//! segment; it maps to arc length through the segment's own parameterisation. On a closed path an
//! `end_t` below `start_t` wraps through the closing point; `start_t == end_t` on a closed path is
//! the whole loop. Arc length: lines exactly, cubics by 8-point Gauss-Legendre over 32 sub-spans
//! per segment, the inverse by table search plus Newton steps (error below 1e-9 pt on the tests).
//!
//! Placement: glyph `i` occupies `[s_i, s_i + w_i]` of the available arc; `align` places the
//! sequence at the start, centre or end of the extent (`flow_direction = right_to_left` swaps
//! start and end). A glyph that ends past the end handle is OVERSET: it and every later glyph are
//! not placed and are reported ([`PathTextLayout::overset_from`]); nothing is silently dropped.
//! Each placed glyph gets an affine `[a b c d e f]` mapping glyph space (x along the advance from
//! the glyph origin, y UP from the baseline, points) to document space:
//! `doc = (a x + c y + e, b x + d y + f)`. With unit tangent `T` and up-normal `N = (T.y, -T.x)`
//! at the glyph's arc midpoint `P`, and the baseline offset `d` (below):
//! * `rainbow` (default): x-axis `T`, y-axis `N`, origin `P + d N - (w/2) T` (rotated to the tangent);
//! * `skew`: x-axis `T`, y-axis vertical (`(0, -1)`, or `(0, 1)` when `N` points down), origin
//!   `P + d Y - (w/2) T` (vertical edges stay vertical);
//! * `ribbon` (3D ribbon): x-axis `(T.x, 0)`, y-axis `N`, origin `P + d N - (w/2) X` (horizontal
//!   edges stay horizontal, vertical edges follow the normal);
//! * `stair_step`: x-axis `(1, 0)`, y-axis `(0, -1)`, origin at the glyph's START point plus `d Y`
//!   (no rotation);
//! * `gravity`: y-axis the unit vector from the centre of the path's bounding box to `P`
//!   (oriented to `N`'s side; `N` when `P` is the centre), x-axis `(-Y.y, Y.x)`, origin
//!   `P + d Y - (w/2) X`.
//!
//! Baseline offset `d = p - a + b`: `p` is where on the stroke the text sits
//! (`path_type_alignment`: `top_path` = `+stroke/2`, `center_path` = 0, `bottom_path` =
//! `-stroke/2`), `a` the glyph-space height of the glyph part that meets the path
//! (`text_type_alignment`: `baseline` 0, `ascender` = ascent, `descender` = -descent, `center` =
//! (ascent - descent)/2, the em and ICF box edges as given by the run), `b` the run's
//! `baseline_offset`. `flipped` lays the text along the reversed path (other side, reading the
//! other way; the Illustrator Flip and the Affinity reverse-text-path geometry); `undefined` is a
//! mixed-selection state and is refused for layout. `spacing` (points, STU-TYP-120 curve
//! tightening) shortens the gap after a glyph by `spacing * theta / pi`, `theta` the tangent turn
//! (radians) across the glyph, never below zero (Studio rule).
//!
//! Bounds (V4-01/02): commands, segments, glyphs and runs are checked against [`PathTextLimits`]
//! before allocation; non-finite input is refused; a path with zero length is
//! [`PathTextError::DegeneratePath`]. Illustrator parity of the five effects, the spacing rule and
//! the handle semantics is UNVERIFIED (D5; lane SMB-D5-AI-ID-AE-COMPARE).
#![deny(clippy::arithmetic_side_effects)]
use crate::{GlyphOutline, OutlineCommand};

/// One command of a baseline path in document units (y down).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BaselineCommand {
    MoveTo([f64; 2]),
    LineTo([f64; 2]),
    QuadTo([f64; 2], [f64; 2]),
    CubicTo([f64; 2], [f64; 2], [f64; 2]),
    Close,
}

/// STU-TYP-121 `path_effect`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PathEffect {
    #[default]
    Rainbow,
    Skew,
    Ribbon,
    StairStep,
    Gravity,
}

/// STU-TYP-121 `path_type_alignment`: which part of the stroke the text sits on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PathTypeAlignment {
    TopPath,
    BottomPath,
    #[default]
    CenterPath,
}

/// STU-TYP-121 `text_type_alignment`: which part of the glyph meets the path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextTypeAlignment {
    Ascender,
    Descender,
    Center,
    #[default]
    Baseline,
    AboveRightEmBox,
    BelowLeftEmBox,
    AboveRightIcfBox,
    BelowLeftIcfBox,
}

/// STU-TYP-120 `flipped`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PathFlip {
    #[default]
    NotFlipped,
    Flipped,
    /// Mixed-selection state: refused for layout.
    Undefined,
}

/// STU-TYP-004 flow direction: which handle `align` start refers to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PathFlowDirection {
    #[default]
    LeftToRight,
    RightToLeft,
}

/// Placement of the glyph sequence inside the flow extent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PathTextAlign {
    #[default]
    Start,
    Center,
    End,
}

/// Path-text options (STU-TYP-120/121).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PathTextOptions {
    /// Segment-relative start handle; `None` = path start.
    pub start_t: Option<f64>,
    /// Segment-relative end handle; `None` = path end.
    pub end_t: Option<f64>,
    /// Curve-tightening compensation, points.
    pub spacing_pt: f64,
    pub flipped: PathFlip,
    pub effect: PathEffect,
    pub path_alignment: PathTypeAlignment,
    pub text_alignment: TextTypeAlignment,
    pub flow_direction: PathFlowDirection,
    pub align: PathTextAlign,
    /// Stroke width of the path (for `path_type_alignment`), points.
    pub stroke_width_pt: f64,
}
impl Default for PathTextOptions {
    fn default() -> Self {
        Self {
            start_t: None,
            end_t: None,
            spacing_pt: 0.0,
            flipped: PathFlip::NotFlipped,
            effect: PathEffect::Rainbow,
            path_alignment: PathTypeAlignment::CenterPath,
            text_alignment: TextTypeAlignment::Baseline,
            flow_direction: PathFlowDirection::LeftToRight,
            align: PathTextAlign::Start,
            stroke_width_pt: 0.0,
        }
    }
}

/// One run of glyphs: its glyph count, metrics (glyph space, y up, points) and baseline offset.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PathTextRun {
    pub glyph_count: usize,
    pub baseline_offset_pt: f64,
    /// Ascent above the baseline (positive).
    pub ascent_pt: f64,
    /// Descent below the baseline (positive).
    pub descent_pt: f64,
    pub em_top_pt: f64,
    pub em_bottom_pt: f64,
    pub icf_top_pt: f64,
    pub icf_bottom_pt: f64,
}

/// Caller bounds, checked before allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathTextLimits {
    pub max_commands: usize,
    pub max_glyphs: usize,
    pub max_runs: usize,
}
impl Default for PathTextLimits {
    fn default() -> Self {
        Self { max_commands: 1 << 16, max_glyphs: 1 << 20, max_runs: 1 << 16 }
    }
}

/// Typed refusals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathTextError {
    /// Non-finite or out-of-range input (the field is named).
    InvalidInput(&'static str),
    /// The path has no length (or no segment).
    DegeneratePath,
    /// More than one subpath: text sits on one path.
    MultipleSubpaths,
    /// `flipped = undefined` (mixed state) cannot be laid out.
    UndefinedFlip,
    /// Over a [`PathTextLimits`] bound (the bound is named).
    OverLimit(&'static str),
}
impl PathTextError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "path_text_invalid_input",
            Self::DegeneratePath => "path_text_degenerate_path",
            Self::MultipleSubpaths => "path_text_multiple_subpaths",
            Self::UndefinedFlip => "path_text_undefined_flip",
            Self::OverLimit(_) => "path_text_over_limit",
        }
    }
}

/// One placed glyph.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedPathGlyph {
    /// Index into the caller's glyph sequence.
    pub index: usize,
    /// Run of the glyph.
    pub run: usize,
    /// Glyph space (x along the advance, y up, points) to document space.
    pub transform: [f64; 6],
    /// Arc position of the glyph start and end along the (possibly reversed) path, points.
    pub arc_start_pt: f64,
    pub arc_end_pt: f64,
}

/// The laid-out path text.
#[derive(Clone, Debug, PartialEq)]
pub struct PathTextLayout {
    pub glyphs: Vec<PlacedPathGlyph>,
    pub path_length_pt: f64,
    /// Arc length between the handles.
    pub available_pt: f64,
    /// Arc length the whole sequence needs (spacing applied).
    pub text_length_pt: f64,
    /// First glyph that did not fit (overset); `None` when everything fits.
    pub overset_from: Option<usize>,
    pub overset_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Seg {
    Line([f64; 2], [f64; 2]),
    Cubic([[f64; 2]; 4]),
}

/// Sub-spans per cubic of the arc-length table.
const SUB: usize = 32;
const GL_X: [f64; 8] = [
    -0.960_289_856_497_536_3,
    -0.796_666_477_413_626_7,
    -0.525_532_409_916_329,
    -0.183_434_642_495_649_8,
    0.183_434_642_495_649_8,
    0.525_532_409_916_329,
    0.796_666_477_413_626_7,
    0.960_289_856_497_536_3,
];
const GL_W: [f64; 8] = [
    0.101_228_536_290_376_26,
    0.222_381_034_453_374_47,
    0.313_706_645_877_887_3,
    0.362_683_783_378_362,
    0.362_683_783_378_362,
    0.313_706_645_877_887_3,
    0.222_381_034_453_374_47,
    0.101_228_536_290_376_26,
];

fn add(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] + b[0], a[1] + b[1]]
}
fn sub(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] - b[0], a[1] - b[1]]
}
fn mul(a: [f64; 2], k: f64) -> [f64; 2] {
    [a[0] * k, a[1] * k]
}
fn norm(a: [f64; 2]) -> f64 {
    a[0].hypot(a[1])
}

impl Seg {
    fn point(&self, t: f64) -> [f64; 2] {
        match *self {
            Seg::Line(a, b) => add(a, mul(sub(b, a), t)),
            Seg::Cubic([p0, p1, p2, p3]) => {
                let u = 1.0 - t;
                let (b0, b1, b2, b3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                [
                    b0 * p0[0] + b1 * p1[0] + b2 * p2[0] + b3 * p3[0],
                    b0 * p0[1] + b1 * p1[1] + b2 * p2[1] + b3 * p3[1],
                ]
            }
        }
    }
    fn deriv(&self, t: f64) -> [f64; 2] {
        match *self {
            Seg::Line(a, b) => sub(b, a),
            Seg::Cubic([p0, p1, p2, p3]) => {
                let u = 1.0 - t;
                let (d0, d1, d2) = (sub(p1, p0), sub(p2, p1), sub(p3, p2));
                mul(add(add(mul(d0, u * u), mul(d1, 2.0 * u * t)), mul(d2, t * t)), 3.0)
            }
        }
    }
    /// Unit tangent at `t` (finite-difference and chord fallbacks at cusps); `None` for a point.
    fn tangent(&self, t: f64) -> Option<[f64; 2]> {
        let candidates = [self.deriv(t), self.deriv((t + 1e-6).min(1.0)), self.deriv((t - 1e-6).max(0.0)), sub(self.end(), self.start())];
        candidates.into_iter().find_map(|d| {
            let n = norm(d);
            (n > 1e-12).then(|| mul(d, 1.0 / n))
        })
    }
    fn start(&self) -> [f64; 2] {
        match *self {
            Seg::Line(a, _) => a,
            Seg::Cubic(p) => p[0],
        }
    }
    fn end(&self) -> [f64; 2] {
        match *self {
            Seg::Line(_, b) => b,
            Seg::Cubic(p) => p[3],
        }
    }
    fn reversed(&self) -> Seg {
        match *self {
            Seg::Line(a, b) => Seg::Line(b, a),
            Seg::Cubic([p0, p1, p2, p3]) => Seg::Cubic([p3, p2, p1, p0]),
        }
    }
    /// Arc length over `[t0, t1]` (8-point Gauss-Legendre for cubics).
    fn length(&self, t0: f64, t1: f64) -> f64 {
        match *self {
            Seg::Line(a, b) => norm(sub(b, a)) * (t1 - t0),
            Seg::Cubic(_) => {
                let (h, m) = ((t1 - t0) / 2.0, (t1 + t0) / 2.0);
                GL_X.iter().zip(GL_W).map(|(x, w)| w * norm(self.deriv(m + h * x))).sum::<f64>() * h
            }
        }
    }
}

/// Arc-length parameterised path.
struct Arc {
    segs: Vec<Seg>,
    /// Per segment: cumulative arc length at its start.
    cum: Vec<f64>,
    /// Per cubic segment: cumulative length at each of the SUB + 1 table knots (empty for lines).
    tables: Vec<Vec<f64>>,
    length: f64,
    closed: bool,
    bbox_centre: [f64; 2],
}

impl Arc {
    fn new(segs: Vec<Seg>, closed: bool) -> Result<Self, PathTextError> {
        let mut cum = Vec::with_capacity(segs.len());
        let mut tables = Vec::with_capacity(segs.len());
        let mut total = 0.0;
        let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for s in &segs {
            cum.push(total);
            let pts: Vec<[f64; 2]> = match *s {
                Seg::Line(a, b) => vec![a, b],
                Seg::Cubic(p) => p.to_vec(),
            };
            for p in pts {
                lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                hi = [hi[0].max(p[0]), hi[1].max(p[1])];
            }
            match s {
                Seg::Line(..) => {
                    tables.push(Vec::new());
                    total += s.length(0.0, 1.0);
                }
                Seg::Cubic(_) => {
                    let mut t = Vec::with_capacity(SUB.saturating_add(1));
                    let mut acc = 0.0;
                    t.push(0.0);
                    for k in 0..SUB {
                        let (a, b) = (k as f64 / SUB as f64, (k as f64 + 1.0) / SUB as f64);
                        acc += s.length(a, b);
                        t.push(acc);
                    }
                    total += acc;
                    tables.push(t);
                }
            }
        }
        if !(total.is_finite() && total > 1e-9) {
            return Err(PathTextError::DegeneratePath);
        }
        let bbox_centre = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
        Ok(Self { segs, cum, tables, length: total, closed, bbox_centre })
    }

    /// Arc length from the segment start to parameter `t` of segment `i`.
    fn partial(&self, i: usize, t: f64) -> f64 {
        let s = &self.segs[i];
        match s {
            Seg::Line(..) => s.length(0.0, t),
            Seg::Cubic(_) => {
                let k = ((t * SUB as f64).floor() as usize).min(SUB.saturating_sub(1));
                let knot = k as f64 / SUB as f64;
                self.tables[i][k] + s.length(knot, t)
            }
        }
    }

    /// Segment-relative handle -> arc length.
    fn handle(&self, h: f64) -> Result<f64, PathTextError> {
        let n = self.segs.len() as f64;
        if !(h.is_finite() && (0.0..=n).contains(&h)) {
            return Err(PathTextError::InvalidInput("handle_t"));
        }
        if h >= n {
            return Ok(self.length);
        }
        let i = (h.floor() as usize).min(self.segs.len().saturating_sub(1));
        Ok(self.cum[i] + self.partial(i, h - i as f64))
    }

    /// (segment, t) at arc position `s` (clamped to the path; wraps on a closed path).
    fn locate(&self, s: f64) -> (usize, f64) {
        let mut s = s;
        if self.closed {
            s = s.rem_euclid(self.length);
        }
        let s = s.clamp(0.0, self.length);
        // Last segment whose start is <= s and that has length (zero-length segments are skipped).
        let mut i = self.cum.partition_point(|c| *c <= s).saturating_sub(1);
        let seg_len = |j: usize| {
            let end = self.cum.get(j.saturating_add(1)).copied().unwrap_or(self.length);
            end - self.cum[j]
        };
        while i > 0 && seg_len(i) <= 1e-12 {
            i = i.saturating_sub(1);
        }
        let len = seg_len(i);
        if len <= 1e-12 {
            return (i, 0.0);
        }
        let target = s - self.cum[i];
        let seg = &self.segs[i];
        let t = match seg {
            Seg::Line(..) => (target / len).clamp(0.0, 1.0),
            Seg::Cubic(_) => {
                let tab = &self.tables[i];
                let k = tab.partition_point(|c| *c <= target).saturating_sub(1).min(SUB.saturating_sub(1));
                let (c0, c1) = (tab[k], tab[k.saturating_add(1)]);
                let frac = if c1 > c0 { (target - c0) / (c1 - c0) } else { 0.0 };
                let mut t = (k as f64 + frac) / SUB as f64;
                for _ in 0..4 {
                    let d = norm(seg.deriv(t));
                    if d <= 1e-12 {
                        break;
                    }
                    t = (t - (self.partial(i, t) - target) / d).clamp(0.0, 1.0);
                }
                t
            }
        };
        (i, t)
    }

    fn point(&self, s: f64) -> [f64; 2] {
        let (i, t) = self.locate(s);
        self.segs[i].point(t)
    }

    fn tangent(&self, s: f64) -> [f64; 2] {
        let (i, t) = self.locate(s);
        // A zero-length span resolves to the nearest segment that has a direction.
        self.segs[i]
            .tangent(t)
            .or_else(|| self.segs.iter().skip(i).chain(self.segs.iter().take(i).rev()).find_map(|g| g.tangent(0.5)))
            .unwrap_or([1.0, 0.0])
    }
}

fn finite2(p: [f64; 2]) -> bool {
    p[0].is_finite() && p[1].is_finite()
}

/// The single subpath of `path` as segments (quads raised to cubics) and whether it is closed.
fn segments(path: &[BaselineCommand], limits: &PathTextLimits) -> Result<(Vec<Seg>, bool), PathTextError> {
    if path.len() > limits.max_commands {
        return Err(PathTextError::OverLimit("max_commands"));
    }
    let mut segs = Vec::with_capacity(path.len());
    let (mut start, mut cur): (Option<[f64; 2]>, [f64; 2]) = (None, [0.0; 2]);
    let mut closed = false;
    for c in path {
        if closed && !matches!(c, BaselineCommand::Close) {
            return Err(PathTextError::MultipleSubpaths);
        }
        let need = |p: [f64; 2]| if finite2(p) { Ok(p) } else { Err(PathTextError::InvalidInput("path")) };
        match *c {
            BaselineCommand::MoveTo(p) => {
                if start.is_some() {
                    return Err(PathTextError::MultipleSubpaths);
                }
                cur = need(p)?;
                start = Some(cur);
            }
            BaselineCommand::LineTo(p) => {
                let p = need(p)?;
                start.ok_or(PathTextError::InvalidInput("path_without_moveto"))?;
                segs.push(Seg::Line(cur, p));
                cur = p;
            }
            BaselineCommand::QuadTo(q, p) => {
                let (q, p) = (need(q)?, need(p)?);
                start.ok_or(PathTextError::InvalidInput("path_without_moveto"))?;
                let c1 = add(cur, mul(sub(q, cur), 2.0 / 3.0));
                let c2 = add(p, mul(sub(q, p), 2.0 / 3.0));
                segs.push(Seg::Cubic([cur, c1, c2, p]));
                cur = p;
            }
            BaselineCommand::CubicTo(a, b, p) => {
                let (a, b, p) = (need(a)?, need(b)?, need(p)?);
                start.ok_or(PathTextError::InvalidInput("path_without_moveto"))?;
                segs.push(Seg::Cubic([cur, a, b, p]));
                cur = p;
            }
            BaselineCommand::Close => {
                let s = start.ok_or(PathTextError::InvalidInput("path_without_moveto"))?;
                if !closed {
                    if norm(sub(s, cur)) > 1e-12 {
                        segs.push(Seg::Line(cur, s));
                    }
                    cur = s;
                    closed = true;
                }
            }
        }
    }
    if segs.is_empty() {
        return Err(PathTextError::DegeneratePath);
    }
    Ok((segs, closed))
}

fn affine(x: [f64; 2], y: [f64; 2], o: [f64; 2]) -> [f64; 6] {
    [x[0], x[1], y[0], y[1], o[0], o[1]]
}

/// Lays `advances` (points, visual order, grouped by `runs`) along `path` (module docs).
pub fn layout_path_text(
    path: &[BaselineCommand],
    runs: &[PathTextRun],
    advances: &[f64],
    options: &PathTextOptions,
    limits: &PathTextLimits,
) -> Result<PathTextLayout, PathTextError> {
    if advances.len() > limits.max_glyphs {
        return Err(PathTextError::OverLimit("max_glyphs"));
    }
    if runs.len() > limits.max_runs {
        return Err(PathTextError::OverLimit("max_runs"));
    }
    let counted = runs.iter().try_fold(0_usize, |acc, r| acc.checked_add(r.glyph_count)).ok_or(PathTextError::OverLimit("max_glyphs"))?;
    if counted != advances.len() {
        return Err(PathTextError::InvalidInput("run_glyph_counts"));
    }
    if advances.iter().any(|a| !(a.is_finite() && *a >= 0.0)) {
        return Err(PathTextError::InvalidInput("advance"));
    }
    for r in runs {
        let v = [r.baseline_offset_pt, r.ascent_pt, r.descent_pt, r.em_top_pt, r.em_bottom_pt, r.icf_top_pt, r.icf_bottom_pt];
        if v.iter().any(|x| !x.is_finite()) {
            return Err(PathTextError::InvalidInput("run_metrics"));
        }
    }
    let o = options;
    if !(o.spacing_pt.is_finite() && o.stroke_width_pt.is_finite() && o.stroke_width_pt >= 0.0) {
        return Err(PathTextError::InvalidInput("options"));
    }
    if o.flipped == PathFlip::Undefined {
        return Err(PathTextError::UndefinedFlip);
    }
    let (mut segs, closed) = segments(path, limits)?;
    let n = segs.len();
    // Handles are stated on the path as drawn; a flip lays the text along the reversed path.
    let probe = Arc::new(segs.clone(), closed)?;
    let mut s0 = probe.handle(o.start_t.unwrap_or(0.0))?;
    let mut s1 = probe.handle(o.end_t.unwrap_or(n as f64))?;
    let length = probe.length;
    drop(probe);
    if o.flipped == PathFlip::Flipped {
        segs.reverse();
        for s in &mut segs {
            *s = s.reversed();
        }
        (s0, s1) = (length - s1, length - s0);
    }
    let arc = Arc::new(segs, closed)?;
    let available = if s1 > s0 {
        s1 - s0
    } else if closed {
        if (s1 - s0).abs() <= 1e-12 { arc.length } else { arc.length - s0 + s1 }
    } else if (s1 - s0).abs() <= 1e-12 {
        0.0
    } else {
        return Err(PathTextError::InvalidInput("end_t_before_start_t"));
    };

    // Pass 1: relative positions from the start of the sequence (spacing applied).
    let run_of: Vec<usize> = runs.iter().enumerate().flat_map(|(i, r)| std::iter::repeat_n(i, r.glyph_count)).collect();
    let mut rel = Vec::with_capacity(advances.len());
    let mut s = 0.0_f64;
    for (i, w) in advances.iter().enumerate() {
        rel.push(s);
        let mut gap = *w;
        if o.spacing_pt != 0.0 && i.saturating_add(1) < advances.len() {
            let (ta, tb) = (arc.tangent(s0 + s), arc.tangent(s0 + s + w));
            let theta = (ta[0] * tb[1] - ta[1] * tb[0]).atan2(ta[0] * tb[0] + ta[1] * tb[1]).abs();
            gap = (w - o.spacing_pt * theta / std::f64::consts::PI).max(0.0);
        }
        s += gap;
    }
    let total = match (advances.last(), rel.last()) {
        (Some(w), Some(r)) => r + w,
        _ => 0.0,
    };
    let lead = match (o.align, o.flow_direction) {
        (PathTextAlign::Start, PathFlowDirection::LeftToRight) | (PathTextAlign::End, PathFlowDirection::RightToLeft) => 0.0,
        (PathTextAlign::Center, _) => ((available - total) / 2.0).max(0.0),
        _ => (available - total).max(0.0),
    };

    let stroke = match o.path_alignment {
        PathTypeAlignment::TopPath => o.stroke_width_pt / 2.0,
        PathTypeAlignment::CenterPath => 0.0,
        PathTypeAlignment::BottomPath => -o.stroke_width_pt / 2.0,
    };
    let mut glyphs = Vec::with_capacity(advances.len());
    let mut overset_from = None;
    for (i, w) in advances.iter().enumerate() {
        let start = lead + rel[i];
        if start + w > available + 1e-9 {
            overset_from = Some(i);
            break;
        }
        let r = &runs[run_of[i]];
        let meet = match o.text_alignment {
            TextTypeAlignment::Baseline => 0.0,
            TextTypeAlignment::Ascender => r.ascent_pt,
            TextTypeAlignment::Descender => -r.descent_pt,
            TextTypeAlignment::Center => (r.ascent_pt - r.descent_pt) / 2.0,
            TextTypeAlignment::AboveRightEmBox => r.em_top_pt,
            TextTypeAlignment::BelowLeftEmBox => r.em_bottom_pt,
            TextTypeAlignment::AboveRightIcfBox => r.icf_top_pt,
            TextTypeAlignment::BelowLeftIcfBox => r.icf_bottom_pt,
        };
        let d = stroke - meet + r.baseline_offset_pt;
        let (a0, a1) = (s0 + start, s0 + start + w);
        let mid = s0 + start + w / 2.0;
        let p = arc.point(mid);
        let t = arc.tangent(mid);
        let nrm = [t[1], -t[0]];
        let half = w / 2.0;
        let transform = match o.effect {
            PathEffect::Rainbow => affine(t, nrm, sub(add(p, mul(nrm, d)), mul(t, half))),
            PathEffect::Skew => {
                let y = if nrm[1] <= 0.0 { [0.0, -1.0] } else { [0.0, 1.0] };
                affine(t, y, sub(add(p, mul(y, d)), mul(t, half)))
            }
            PathEffect::Ribbon => {
                let x = [t[0], 0.0];
                affine(x, nrm, sub(add(p, mul(nrm, d)), mul(x, half)))
            }
            PathEffect::StairStep => {
                let y = [0.0, -1.0];
                affine([1.0, 0.0], y, add(arc.point(a0), mul(y, d)))
            }
            PathEffect::Gravity => {
                let v = sub(p, arc.bbox_centre);
                let len = norm(v);
                let mut y = if len > 1e-9 { mul(v, 1.0 / len) } else { nrm };
                if y[0] * nrm[0] + y[1] * nrm[1] < 0.0 {
                    y = mul(y, -1.0);
                }
                let x = [-y[1], y[0]];
                affine(x, y, sub(add(p, mul(y, d)), mul(x, half)))
            }
        };
        if !transform.iter().all(|v| v.is_finite()) {
            return Err(PathTextError::InvalidInput("geometry"));
        }
        let wrap = |v: f64| if closed { v.rem_euclid(arc.length) } else { v };
        glyphs.push(PlacedPathGlyph { index: i, run: run_of[i], transform, arc_start_pt: wrap(a0), arc_end_pt: wrap(a1) });
    }
    let overset_count = overset_from.map_or(0, |f| advances.len().saturating_sub(f));
    Ok(PathTextLayout { glyphs, path_length_pt: arc.length, available_pt: available, text_length_pt: total, overset_from, overset_count })
}

/// A glyph outline placed on the path: font units scaled by `font_size_pt / units_per_em`, then
/// the glyph's transform (document units; text-to-outlines, STU-VEC-038).
pub fn place_outline(glyph: &PlacedPathGlyph, outline: &GlyphOutline, font_size_pt: f64) -> Result<Vec<BaselineCommand>, PathTextError> {
    let upem = f64::from(outline.units_per_em());
    if !(font_size_pt.is_finite() && font_size_pt > 0.0 && upem > 0.0) {
        return Err(PathTextError::InvalidInput("font_size"));
    }
    let k = font_size_pt / upem;
    let m = glyph.transform;
    let map = |p: [f32; 2]| {
        let (x, y) = (f64::from(p[0]) * k, f64::from(p[1]) * k);
        [m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]]
    };
    Ok(outline
        .commands()
        .iter()
        .map(|c| match *c {
            OutlineCommand::MoveTo(p) => BaselineCommand::MoveTo(map(p)),
            OutlineCommand::LineTo(p) => BaselineCommand::LineTo(map(p)),
            OutlineCommand::QuadTo(a, p) => BaselineCommand::QuadTo(map(a), map(p)),
            OutlineCommand::CurveTo(a, b, p) => BaselineCommand::CubicTo(map(a), map(b), map(p)),
            OutlineCommand::Close => BaselineCommand::Close,
        })
        .collect())
}
