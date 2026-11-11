//! Threaded-frame reflow: one story flows through an ordered chain of text frames (STU-LAY-018,
//! STU-LAY-122). Text that does not fit the last frame is reported as `overset`, never truncated.
//! The result is a revision-bound placement proposal in points; nothing is mutated. The pages, the
//! frames, the thread and the story are read from Folio (`folio_input`).
//!
//! Conventions (documented, not spec-derived): lines are left-aligned and not justified; a line
//! slot is a baseline whose line box `[baseline - ascent, baseline + descent]` lies inside the
//! frame's inset area, and its width is the widest gap left after subtracting every obstacle that
//! overlaps the line box (narrower gaps are not used); a baseline with no gap is skipped; hanging
//! trailing whitespace is part of the line's glyph range but not of `width_pt`; an empty
//! paragraph occupies one empty line; paragraphs have no space before or after unless their Folio
//! composition settings state it (below).
//!
//! Indents and spacing (lane SMB-TYPE-INDENT-SPACING-1, STU-TYP-185; `typeset::ParagraphGeometry`,
//! all 0 by default): every slot is narrowed where it is collected, x by `left_indent` (plus
//! `first_line_indent` on the paragraph's first line), the width by both and `right_indent`; with
//! wrap obstacles the indents measure from the free gap; a slot left without width is skipped like
//! a baseline without a gap (still as the first line); split sub-columns narrow the same way (a
//! sub-column left without width is `invalid_geometry`). `last_line_indent` narrows the last line
//! from the right after breaking, with one bounded re-break when the narrowed last line does not
//! fit (positive values, Latin paragraphs outside split columns, a narrowed width that holds the
//! line's widest box; else the breaks stand and the line is overfull; fit counts what an Adobe
//! composer's spaces may compress; every composer narrows the line that ends the paragraph,
//! whatever its number, and the Adobe paragraph composer re-breaks only its last
//! `PARA_TAIL_LINES + 1` lines); a negative value widens the last line at the set stage only. `space_after` is held and added with the next paragraph's `space_before` as one gap; a
//! paragraph at a column top takes none (a push past the bottom lands at the next column's top);
//! `space_between_same_style` zeroes the gap between paragraphs of one style; span and split
//! paragraphs take the larger of the flow record's and the attribute space; a break sentinel drops
//! the pending space; balancing re-adds the space between lines of one column. Every sum is
//! checked finite (`invalid_geometry`). Zero values execute no operation on a slot. InDesign parity
//! of all of this is UNVERIFIED (D5); the mixed-leading reading of `baseline_of` (earlier lines of
//! a column advance by the current paragraph's line height) is unchanged and UNVERIFIED.
//!
//! Columns (lane SMB-LAYOUT-UI, STU-LAY-017/118): a frame's inset width is split into
//! `text_column_count` equal columns with `text_column_gutter` between them; text fills a column top
//! to bottom, then the next column, then the next frame of the thread. Paragraph overrides from the
//! Folio layout-flow records (STU-LAY-021): a span paragraph starts a band below the deepest line of
//! the current band (that band is balanced first) and runs across the whole inset width; the
//! following text forms a new column band below it (+ space after). A split paragraph is broken at
//! the sub-column width `(column - 2 * outside - (n - 1) * inside) / n` and its lines fill the
//! sub-columns column-major over `ceil(lines / n)` rows of its column; a split block that does not
//! fit its frame moves whole to the next frame. `vertical_balance_columns` balances the band that
//! ends the story: column c takes lines [c*k, (c+1)*k), k = ceil(L / n) (equal column widths keep
//! the breaks). Balancing is skipped for frames with wrap obstacles or bands holding split lines;
//! split paragraphs in frames with obstacles are `unsupported_composition`. InDesign parity of all
//! of this is UNVERIFIED (D5).
//!
//! Vertical justification (lane SMB-ID-PREPRESS, STU-LAY-121, STU-TYP-190; `reflow_flow` only): after
//! composition each column of a frame is moved as a block. The block is the line boxes of the
//! column, from the first line's top (baseline - ascent) to the last line's bottom (baseline +
//! descent); the free space is the frame's inset height minus the block. `center` places the block
//! with equal free space above and below, `bottom` puts the last line's bottom on the bottom inset.
//! `justify` keeps the first line and puts the last line's bottom on the bottom inset: the slack
//! goes to the P gaps between paragraphs, each at most the frame's `vertical_threshold` (Folio
//! layout-flow records; none stored = no limit), and what the limit holds back is shared equally
//! by all line gaps of the column; one line is not moved. Frames with wrap obstacles are not
//! justified (left as composed), and a non-top frame that holds span or split lines is
//! `unsupported_composition`. InDesign parity (block extent, the threshold-overflow rule, wrap
//! behaviour) is UNVERIFIED (D5).
//!
//! Baseline-grid alignment (lane SMB-ID-PREPRESS, STU-LAY-115; `reflow_grid` only): a paragraph
//! marked `all_lines` in the Folio grid records puts every line's baseline on the next baseline
//! grid line at or below where it would fall (the following lines continue from the snapped
//! baseline, so the effective leading rounds up to grid steps); `first_line_only` snaps the first
//! line of the paragraph and lets the rest follow at their own leading. The grid of a
//! frame is its custom baseline frame grid, else the document baseline grid (`grids` module); a
//! frame with neither is not aligned. Span and split paragraphs are not aligned. Balancing does not
//! re-snap moved lines. InDesign parity UNVERIFIED (D5).
//!
//! Stream sentinels (lane SMB-LAYOUT-SENTINELS-1, STU-TYP-207 / SD-29; `sentinels` module): a
//! paragraph ended by a break sentinel (the first character after its content) continues the flow
//! at the break's target (next column, frame, later page, later page of the wanted number parity);
//! no target oversets from the break byte; a column break stops balancing of its frame. A flush
//! space on the last line of a `JustifiedAll` paragraph takes the rest of the slot and the line
//! keeps its natural spacing. InDesign parity UNVERIFIED (D5).
use crate::adobe_composer::{AdobeContext, ParaStats, compress_line, justify_line, last_line_rebreak, paragraph as adobe_paragraph, single_line, single_line_last};
use crate::breaker::{Composer, HyphenRules, LineBreak, Sums, first_fit, first_fit_last, knuth_plass, knuth_plass_last, line_start};
use crate::hyphenate::Hyphenator;
use crate::styles::{Alignment, ParagraphStyling};
use crate::typeset::{Justification, TypesetOptions, justify_metrics, paragraph_params};
use crate::folio_input::{Frame, FrameObstacles, FrameVertical, pt, resolve_flow};
use hsk_studio_folio::{FirstBaseline, StudioAlignToGrid, StudioLayoutGrids, StudioSingleWordJustification};
use crate::paragraph::{FirstBaselineUnavailable, ShapedParagraph, SpaceParams, build_items};
use crate::cjk::{CjkGap, CjkLineSpacing, CjkParagraph};
use crate::sentinels::{BreakOutcome, BreakOverset, BreakResult, FlushOutcome};
use hsk_studio_type::BreakTarget;
use crate::{LayoutError, SourceSpan};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    StudioDocument, StudioLayoutFlowRecords, StudioParagraphFlow, StudioSpanColumn, StudioSpanCount,
    StudioTextStory, VerticalJustification,
};
use std::ops::Range;

const EPS: f64 = 1e-9;

/// Caller-supplied hard bounds; exceeding one refuses the whole reflow (no partial proposal).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_lines: usize,
    pub max_glyphs: usize,
    pub max_paragraphs: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_lines: 100_000,
            max_glyphs: 1_048_576,
            max_paragraphs: 65_536,
        }
    }
}

/// Everything `reflow` reads. The thread is the chain of frame layers of `document` whose text
/// payload names `story`; `paragraphs` are the Type-shaped paragraphs of that story in order, with
/// byte offsets into the story text. The current revision is `document.revision`.
#[derive(Clone, Copy, Debug)]
pub struct ReflowRequest<'a> {
    pub document: &'a StudioDocument,
    pub story: &'a StudioTextStory,
    /// The bytes of a `StoryContent::Stored` story, resolved by the caller (exactly `byte_len`
    /// bytes). Must be `None` for inline content.
    pub resolved_text: Option<&'a str>,
    pub paragraphs: &'a [ShapedParagraph],
    /// Text-wrap obstacles per frame layer (Folio stores none); entries for other frames are ignored.
    pub obstacles: &'a [FrameObstacles],
    pub composer: Composer,
    pub space: SpaceParams,
    pub expected_revision: u64,
    pub limits: Limits,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub paragraph: usize,
    /// From the frame top, in the frame's coordinates.
    pub baseline_y_pt: f64,
    pub x_start_pt: f64,
    /// Natural advance of the glyphs on the line, excluding hanging trailing whitespace.
    pub width_pt: f64,
    pub available_pt: f64,
    pub hang_pt: f64,
    /// The source of the line's glyphs. A line a forced line break makes without a glyph holds no
    /// glyph and spans the bytes from the break before it to the break that ends it
    /// (`paragraph::blank_lines`); any other line without a glyph has an empty span.
    pub source: SourceSpan,
    /// Paragraph-local glyph indices; consecutive lines tile the paragraph's glyphs exactly.
    pub glyph_range: Range<usize>,
    /// The line is wider than its slot (a word that cannot be broken).
    pub overfull: bool,
    /// Knuth-Plass adjustment ratio (negative = shrunk, positive = stretched); `None` for first-fit.
    pub ratio: Option<f64>,
    /// The line ends in a hyphenation break; `width_pt` includes the hyphen advance.
    pub hyphenated: bool,
    /// Set by `reflow_typeset` for justified lines; `None` otherwise (also the last line).
    pub justification: Option<Justification>,
    /// Column of the frame the line sits in (0 for single-column frames and span paragraphs).
    pub column: usize,
    /// The line starts inside its first glyph (`glyph_range.start`): the right part of a ligature
    /// the previous line's hyphenation break split, set from `source.start` with `advance_pt`
    /// instead of the glyph's advance (Type composition v2 ligature components).
    pub start_part: Option<LinePart>,
    /// A hyphenation break changes the material before the hyphen (Type composition v2 data): the
    /// last content glyph set with its `hmtx` advance (kerning toward the next glyph dropped), or,
    /// when `glyph == glyph_range.end`, the left part of a ligature split by the break, set after
    /// the line's glyphs. `width_pt` includes it.
    pub end_part: Option<LinePart>,
}

/// Glyph material a line sets differently at its start or end (see `Line::start_part`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinePart {
    /// Paragraph-local glyph index.
    pub glyph: usize,
    /// The source characters set.
    pub source: SourceSpan,
    pub advance_pt: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FramePlacement {
    /// `layer_id` of the frame layer.
    pub frame_id: String,
    /// Index of the frame layer in `StudioDocument::layers`.
    pub frame_index: usize,
    pub spread: usize,
    pub page: usize,
    /// Frame top-left in spread coordinates.
    pub origin_x_pt: f64,
    pub origin_y_pt: f64,
    pub lines: Vec<Line>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayoutProposal {
    pub base_revision: u64,
    /// One entry per thread frame, in flow order, including frames that received no line.
    pub frames: Vec<FramePlacement>,
    /// First unplaced source byte through the end of the story; `None` when everything fits.
    pub overset: Option<SourceSpan>,
}

/// Position of the flow: frame `pos`, column `col`, line slot `line` of the current band, plus
/// extra vertical space (`extra`, split paragraphs). `band` is the y of the band top in frame
/// coordinates (`None` = frame top, whose first baseline is the frame's first-baseline offset).
/// `first` is that offset once the column's first line fixed it (STU-LAY-120 kinds other than
/// `fixed` take the metric of the column's first line); `None` = not yet fixed.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Cursor {
    pos: usize,
    col: usize,
    line: usize,
    band: Option<f64>,
    extra: f64,
    first: Option<f64>,
}
impl Cursor {
    const fn frame(pos: usize) -> Self {
        Self {
            pos,
            col: 0,
            line: 0,
            band: None,
            extra: 0.0,
            first: None,
        }
    }
}
#[derive(Clone, Copy, Debug)]
struct Slot {
    pos: usize,
    col: usize,
    baseline_y_pt: f64,
    x_start_pt: f64,
    width_pt: f64,
    next: Cursor,
}
/// Column slots stay inside one column; span slots run across the frame's whole inset width.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Column,
    Span,
}

/// Paragraph-level span/split flow (STU-LAY-021), resolved to points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ParaFlow {
    Single,
    /// `count = None` spans all columns.
    Span { count: Option<usize>, before: f64, after: f64 },
    Split { count: usize, inside: f64, outside: f64, before: f64, after: f64 },
}

/// Per placed line: what balancing needs to move it.
#[derive(Clone, Copy)]
struct Meta {
    ascent: f64,
    height: f64,
    single: bool,
    /// First-baseline offset of the line's frame for the line's paragraph (`frame_first`).
    first: Option<f64>,
    /// STU-TYP-185 paragraph space added above this line (its paragraph's first line, in the column
    /// where the space was added); balancing re-adds it except at a column's first line.
    gap: f64,
}

/// Widest gap of `[lo, hi]` left by obstacles that overlap the line box.
fn free_interval(f: &Frame<'_>, lo: f64, hi: f64, top: f64, bottom: f64) -> (f64, f64) {
    let mut blocked: Vec<(f64, f64)> = f
        .obstacles
        .iter()
        .filter(|o| o.y0_pt < bottom - EPS && o.y1_pt > top + EPS)
        .map(|o| (o.x0_pt.max(lo), o.x1_pt.min(hi)))
        .filter(|(a, b)| a < b)
        .collect();
    blocked.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let (mut best, mut at) = ((lo, lo), lo);
    let mut consider = |gap: (f64, f64)| {
        if gap.1 - gap.0 > best.1 - best.0 + EPS {
            best = gap;
        }
    };
    for (a, b) in blocked {
        if a > at {
            consider((at, a));
        }
        at = at.max(b);
    }
    if hi > at {
        consider((at, hi));
    }
    best
}

/// `a + b`, refused as `invalid_geometry` when not finite (sums of finite indents and spaces can
/// overflow; `+inf + -inf` would be a NaN slot).
fn sum(a: f64, b: f64) -> Result<f64, LayoutError> {
    let v = a + b;
    if v.is_finite() { Ok(v) } else { Err(LayoutError::InvalidGeometry) }
}

/// STU-TYP-185: `s` with x moved by `left` and the width narrowed by `left` and `right`; `None`
/// when no width remains (the slot is skipped like a baseline without a gap). Zero indents leave
/// the slot untouched (no float operation, so the bits of an unindented line cannot move).
fn indented(s: Slot, left: f64, right: f64) -> Result<Option<Slot>, LayoutError> {
    if left == 0.0 && right == 0.0 {
        return Ok(Some(s));
    }
    let x = sum(s.x_start_pt, left)?;
    let w = sum(sum(s.width_pt, -left)?, -right)?;
    Ok((w > EPS).then_some(Slot { x_start_pt: x, width_pt: w, ..s }))
}

/// The larger of a flow-record space and the attribute space; the flow value itself (same bits)
/// unless the attribute space is larger.
fn larger(flow: f64, attribute: f64) -> f64 {
    if attribute > flow { attribute } else { flow }
}

/// The narrowest width line `k` of `breaks` is set in (review F3): its natural width
/// (`natural_width`), less, for the Adobe composers, what its inner word spaces may shrink (word
/// spacing desired minus minimum): the set stage closes such a line to its slot (`compress_line`).
fn tightest_width(p: &ShapedParagraph, items: &[crate::paragraph::Item], breaks: &[LineBreak], k: usize, adobe: bool, jp: &crate::typeset::JustifyParams) -> f64 {
    let natural = natural_width(p, items, breaks, k);
    if !adobe {
        return natural;
    }
    let item_start = if k == 0 { 0 } else { line_start(items, breaks[k - 1].item) };
    let b = &breaks[k];
    let content_end = items[item_start..b.item].iter().rev().find(|it| it.kind == crate::paragraph::Kind::Box).map_or(0, |it| it.end);
    let spaces: f64 = items[item_start..b.item]
        .iter()
        .filter(|it| it.kind == crate::paragraph::Kind::Glue && !it.trailing && !it.inf_stretch && it.end <= content_end)
        .map(|it| it.w)
        .sum();
    natural - spaces * (jp.word_spacing.desired - jp.word_spacing.min).max(0.0)
}

/// Natural width of line `k` of `breaks` as the set stage measures it (`compose_flow`): the glyphs
/// up to the line's last box, a line-end hyphen and a split ligature's head change.
fn natural_width(p: &ShapedParagraph, items: &[crate::paragraph::Item], breaks: &[LineBreak], k: usize) -> f64 {
    let n = p.glyphs.len();
    let item_start = if k == 0 { 0 } else { line_start(items, breaks[k - 1].item) };
    let start = if k == 0 { 0 } else { items.get(item_start).map_or(n, |it| it.first) };
    let end = if k + 1 < breaks.len() { items.get(line_start(items, breaks[k].item)).map_or(n, |it| it.first) } else { n };
    let range = start..end.max(start);
    let b = &breaks[k];
    let content_end = items[item_start..b.item]
        .iter()
        .rev()
        .find(|it| it.kind == crate::paragraph::Kind::Box)
        .map_or(range.start, |it| it.end.min(range.end));
    let head = items
        .get(item_start)
        .filter(|it| it.kind == crate::paragraph::Kind::Box && it.first == range.start && !range.is_empty())
        .and_then(|it| it.head);
    p.glyphs[range.start..content_end].iter().map(|g| g.advance_pt).sum::<f64>()
        + 0.0
        + if items[b.item].flagged { items[b.item].w } else { 0.0 }
        + head.map_or(0.0, |h| h.delta)
}

/// Width of the widest box (word or hyphenation fragment) of line `k` of `breaks`.
fn widest_box(items: &[crate::paragraph::Item], breaks: &[LineBreak], k: usize) -> f64 {
    let item_start = if k == 0 { 0 } else { line_start(items, breaks[k - 1].item) };
    items[item_start..breaks[k].item]
        .iter()
        .filter(|it| it.kind == crate::paragraph::Kind::Box)
        .map(|it| it.w)
        .fold(0.0, f64::max)
}

fn baseline_of(cur: &Cursor, first: f64, height: f64, ascent: f64) -> f64 {
    let base = cur.band.map_or(first, |t| t + ascent);
    base + cur.line as f64 * height + cur.extra
}

/// First-baseline offset from the top of frame `f` when paragraph `p` sets the first line of a column
/// (STU-LAY-120): `fixed` (and every caller without frame options) is the frame's explicit offset;
/// the other kinds are the inset top plus the larger of the kind's metric and the minimum offset. A
/// metric the paragraph does not carry is `FirstBaselineUnavailable` (`first_baseline_metric`).
fn frame_first(f: &Frame<'_>, v: Option<&FrameVertical>, p: &ShapedParagraph) -> Result<f64, FirstBaselineUnavailable> {
    match v {
        Some(v) if v.first_kind != FirstBaseline::Fixed => {
            Ok(f.inset_top_pt + p.first_baseline_metric(v.first_kind)?.max(v.first_min_pt))
        }
        _ => Ok(f.first_baseline_pt),
    }
}

/// The next line slot for paragraph `p` at or after `cur`. A frame whose first baseline needs a metric
/// `p` does not carry ends the search (`Ok(None)`) and sets `blocked`: the caller refuses the reflow
/// only when it needs that slot (a frame no line reaches is not refused: tests/frame_autosize.rs
/// missing_face_metric_refuses_typed).
#[allow(clippy::too_many_arguments)]
fn next_slot(
    thread: &[Frame<'_>],
    mut cur: Cursor,
    p: &ShapedParagraph,
    mode: Mode,
    verticals: &[FrameVertical],
    snap: bool,
    blocked: &mut Option<FirstBaselineUnavailable>,
    scans: &mut usize,
    scan_cap: usize,
    cancel: &CancellationToken,
) -> Result<Option<Slot>, LayoutError> {
    let (height, ascent) = (p.line_height_pt, p.ascent_pt);
    let descent = height - ascent;
    while let Some(f) = thread.get(cur.pos) {
        *scans += 1;
        if *scans > scan_cap {
            return Err(LayoutError::BudgetExceeded);
        }
        if (*scans).is_multiple_of(256) {
            cancel.check().map_err(|_| LayoutError::Canceled)?;
        }
        let columns = if mode == Mode::Span { 1 } else { f.column_count.max(1) };
        if cur.col >= columns {
            cur = Cursor::frame(cur.pos + 1);
            continue;
        }
        let first = match (cur.band, cur.first) {
            (Some(_), _) => 0.0,
            (None, Some(first)) => first,
            (None, None) => match frame_first(f, verticals.get(cur.pos), p) {
                Ok(first) => first,
                Err(e) => {
                    *blocked = Some(e);
                    return Ok(None);
                }
            },
        };
        let mut baseline = baseline_of(&cur, first, height, ascent);
        let mut delta = 0.0;
        if snap && let Some((o, step)) = verticals.get(cur.pos).and_then(|v| v.grid) {
            let snapped = o + ((baseline - o) / step - EPS).ceil() * step;
            delta = snapped - baseline;
            baseline = snapped;
        }
        if baseline + descent > f.height_pt - f.inset_bottom_pt + EPS {
            cur = if cur.col + 1 < columns {
                Cursor {
                    col: cur.col + 1,
                    line: 0,
                    extra: 0.0,
                    first: None,
                    ..cur
                }
            } else {
                Cursor::frame(cur.pos + 1)
            };
            continue;
        }
        let (lo, hi) = if mode == Mode::Span { f.inner_x() } else { f.column_x(cur.col) };
        let (x0, x1) = free_interval(f, lo, hi, baseline - ascent, baseline + descent);
        let next = Cursor {
            line: cur.line + 1,
            extra: cur.extra + delta,
            first: if cur.band.is_none() { Some(first) } else { cur.first },
            ..cur
        };
        if x1 - x0 > EPS {
            return Ok(Some(Slot {
                pos: cur.pos,
                col: cur.col,
                baseline_y_pt: baseline,
                x_start_pt: x0,
                width_pt: x1 - x0,
                next,
            }));
        }
        cur = next;
    }
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn break_lines(
    composer: Composer,
    items: &[crate::paragraph::Item],
    sums: &Sums,
    width: &dyn Fn(usize) -> f64,
    rules: &HyphenRules,
    jp: &crate::typeset::JustifyParams,
    justified: bool,
    justify_last: bool,
    cancel: &CancellationToken,
) -> Result<Vec<LineBreak>, LayoutError> {
    match composer {
        Composer::FirstFit => first_fit(items, sums, width, rules, cancel),
        Composer::KnuthPlass(params) => knuth_plass(items, sums, width, &params, rules, cancel),
        Composer::AdobeSingleLine(params) => {
            single_line(items, sums, width, rules, &AdobeContext { params, justify: jp, justified, justify_last }, cancel)
        }
        Composer::AdobeParagraph(params) => {
            adobe_paragraph(items, sums, width, rules, &AdobeContext { params, justify: jp, justified, justify_last }, cancel)
        }
    }
}

fn canceled(cancel: &CancellationToken) -> Result<(), LayoutError> {
    cancel.check().map_err(|_| LayoutError::Canceled)
}

pub fn reflow(
    req: &ReflowRequest<'_>,
    cancel: &CancellationToken,
) -> Result<LayoutProposal, LayoutError> {
    reflow_typeset(req, &TypesetOptions::default(), cancel)
}

/// `reflow` with justification, hyphenation, alignment and Folio type styles (see `typeset`).
/// Multi-column frames are composed column by column (no span/split overrides, no balancing:
/// those come from the Folio layout-flow records, `reflow_flow`).
pub fn reflow_typeset(
    req: &ReflowRequest<'_>,
    opts: &TypesetOptions<'_>,
    cancel: &CancellationToken,
) -> Result<LayoutProposal, LayoutError> {
    let none = StudioLayoutFlowRecords::new(req.document.document_id.clone());
    reflow_flow(req, opts, &none, cancel)
}

/// `reflow_typeset` with the Folio layout-flow records of the document: frame column balancing
/// (`vertical_balance_columns`) and paragraph span/split overrides (STU-LAY-021). A record set of
/// another document, or a paragraph override whose start is no paragraph of the story, is
/// `invalid_input`; a span count N with 1 < N < the frame's column count is
/// `unsupported_composition` (InDesign's partial-span semantics are UNVERIFIED).
pub fn reflow_flow(
    req: &ReflowRequest<'_>,
    opts: &TypesetOptions<'_>,
    flow: &StudioLayoutFlowRecords,
    cancel: &CancellationToken,
) -> Result<LayoutProposal, LayoutError> {
    reflow_inner(req, opts, flow, None, cancel)
}

/// `reflow_flow` that also reports the STU-LAY-119 auto-size of the thread (lane
/// SMB-ID-FRAME-AUTOSIZE-1, `autosize` module): the proposal composed in the auto-sized frame plus
/// one `FrameResize` per auto-sized frame. This is the ONLY entry point that resizes (R44 finding
/// F-R44-6, lane SMB-R44-FOLLOWUPS-1): `reflow`, `reflow_typeset`, `reflow_flow`, `reflow_grid` and
/// `reflow_cjk` compose an auto-sized frame at its authored size, so their proposal matches the
/// document's frame (the same lines as the auto-sized layout only where the size does not change
/// them; tests/frame_autosize.rs `height_only_fits_the_last_line_and_grows_and_shrinks`). The
/// auto-size refusals of the thread (`autosize_threaded_frame`, `autosize_multi_column`) are read
/// from the frames and stay on every entry point.
pub fn reflow_autosize(
    req: &ReflowRequest<'_>,
    opts: &TypesetOptions<'_>,
    flow: &StudioLayoutFlowRecords,
    cancel: &CancellationToken,
) -> Result<crate::autosize::AutosizeProposal, LayoutError> {
    let (layout, _, resizes) = reflow_sized(req, opts, flow, None, &[], true, cancel)?;
    Ok(crate::autosize::AutosizeProposal { layout, resizes })
}

/// `reflow_flow` with the Folio grid records (`hsk.studio.layout_grids@1`): paragraphs marked in
/// `paragraph_alignments` align to the frame's baseline grid (module notes). A record set of
/// another document, or an alignment whose start is no paragraph of the story, is `invalid_input`.
pub fn reflow_grid(
    req: &ReflowRequest<'_>,
    opts: &TypesetOptions<'_>,
    flow: &StudioLayoutFlowRecords,
    grids: &StudioLayoutGrids,
    cancel: &CancellationToken,
) -> Result<LayoutProposal, LayoutError> {
    reflow_inner(req, opts, flow, Some(grids), cancel)
}

fn reflow_inner(
    req: &ReflowRequest<'_>,
    opts: &TypesetOptions<'_>,
    flow: &StudioLayoutFlowRecords,
    grids: Option<&StudioLayoutGrids>,
    cancel: &CancellationToken,
) -> Result<LayoutProposal, LayoutError> {
    reflow_core(req, opts, flow, grids, &[], cancel).map(|r| r.0)
}

/// `reflow_inner` with one optional CJK paragraph setting per shaped paragraph (lane
/// SMB-ID-CJK-KINSOKU-1, `cjk::reflow_cjk`; empty = none): also returns the spacing of every
/// CJK-composed line.
pub(crate) fn reflow_core(
    req: &ReflowRequest<'_>,
    opts: &TypesetOptions<'_>,
    flow: &StudioLayoutFlowRecords,
    grids: Option<&StudioLayoutGrids>,
    cjk: &[Option<CjkParagraph>],
    cancel: &CancellationToken,
) -> Result<(LayoutProposal, Vec<CjkLineSpacing>), LayoutError> {
    // R44 F-R44-6: the plain entry points keep the authored frame size (see `reflow_autosize`).
    reflow_sized(req, opts, flow, grids, cjk, false, cancel).map(|(p, s, _)| (p, s))
}

/// `reflow_core` that also returns the STU-LAY-119 resizes (`autosize` module) when `autosize` is
/// set (only `reflow_autosize`); otherwise, and without an auto-sized frame, the thread is composed
/// once at its authored sizes, exactly as before auto-size existed. Odd/even-page breaks have no
/// page numbers here (`sentinels` module: they overset as `parity_unknown`).
#[allow(clippy::too_many_arguments)]
fn reflow_sized(
    req: &ReflowRequest<'_>,
    opts: &TypesetOptions<'_>,
    flow: &StudioLayoutFlowRecords,
    grids: Option<&StudioLayoutGrids>,
    cjk: &[Option<CjkParagraph>],
    autosize: bool,
    cancel: &CancellationToken,
) -> Result<(LayoutProposal, Vec<CjkLineSpacing>, Vec<crate::autosize::FrameResize>), LayoutError> {
    reflow_sized_facts(req, opts, flow, grids, cjk, autosize, &|_, _| None, cancel).map(|(p, s, r, _)| (p, s, r))
}

/// What the composition did with the stream sentinels (`sentinels` module).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct FlowFacts {
    pub breaks: Vec<BreakOutcome>,
    pub flush: Vec<FlushOutcome>,
}

/// `reflow_flow` with the page number of each (spread, page) for odd/even-page breaks; also returns
/// the break and flush-space outcomes (`sentinels::compose_with_sentinels`).
pub(crate) fn reflow_facts(
    req: &ReflowRequest<'_>,
    opts: &TypesetOptions<'_>,
    flow: &StudioLayoutFlowRecords,
    page_number: &dyn Fn(usize, usize) -> Option<u32>,
    cancel: &CancellationToken,
) -> Result<(LayoutProposal, FlowFacts), LayoutError> {
    reflow_sized_facts(req, opts, flow, None, &[], false, page_number, cancel).map(|(p, _, _, f)| (p, f))
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn reflow_sized_facts(
    req: &ReflowRequest<'_>,
    opts: &TypesetOptions<'_>,
    flow: &StudioLayoutFlowRecords,
    grids: Option<&StudioLayoutGrids>,
    cjk: &[Option<CjkParagraph>],
    autosize: bool,
    page_number: &dyn Fn(usize, usize) -> Option<u32>,
    cancel: &CancellationToken,
) -> Result<(LayoutProposal, Vec<CjkLineSpacing>, Vec<crate::autosize::FrameResize>, FlowFacts), LayoutError> {
    canceled(cancel)?;
    opts.validate()?;
    if req.expected_revision != req.document.revision {
        return Err(LayoutError::StaleRevision {
            expected: req.expected_revision,
            current: req.document.revision,
        });
    }
    if flow.document_id != req.document.document_id {
        return Err(LayoutError::InvalidInput);
    }
    let input = resolve_flow(
        req.document,
        req.story,
        req.resolved_text,
        req.obstacles,
        Some(flow),
    )?;
    let (text, mut thread, mut verticals) = (input.text, input.frames, input.verticals);
    let mut aligns = Vec::new();
    if let Some(g) = grids {
        if g.document_id != req.document.document_id {
            return Err(LayoutError::InvalidInput);
        }
        let story = req.story.story_id.as_str();
        let mine: Vec<_> = g.paragraph_alignments.iter().filter(|a| a.story_id == story).collect();
        if mine.iter().any(|a| !req.paragraphs.iter().any(|p| p.source.start == a.paragraph_start)) {
            return Err(LayoutError::InvalidInput);
        }
        aligns = req
            .paragraphs
            .iter()
            .map(|p| mine.iter().find(|a| a.paragraph_start == p.source.start).map_or(StudioAlignToGrid::None, |a| a.align_to_grid))
            .collect();
        if aligns.iter().any(|a| *a != StudioAlignToGrid::None) {
            for (f, v) in thread.iter().zip(verticals.iter_mut()) {
                v.grid = match crate::grids::frame_grid(req.document, g, f.id) {
                    Ok(grid) => Some(grid),
                    Err(LayoutError::InvalidInput) => None,
                    Err(e) => return Err(e),
                };
            }
        }
    }
    if req
        .paragraphs
        .iter()
        .any(|p| p.source.end > req.story.byte_len())
    {
        return Err(LayoutError::InvalidInput);
    }
    let limits = req.limits;
    if req.paragraphs.len() > limits.max_paragraphs
        || req
            .paragraphs
            .iter()
            .try_fold(0usize, |n, p| n.checked_add(p.glyphs.len()))
            .is_none_or(|n| n > limits.max_glyphs)
    {
        return Err(LayoutError::BudgetExceeded);
    }
    let flows = paragraph_flows(req, flow)?;
    let styled;
    let (paragraphs, stylings): (&[ShapedParagraph], Vec<Option<ParagraphStyling>>) =
        if let Some(src) = opts.styles {
            let (a, b) = crate::styles::apply(src, req.story, text, req.paragraphs, opts.alignment)?;
            styled = a;
            (&styled, b.into_iter().map(Some).collect())
        } else {
            (req.paragraphs, vec![None; req.paragraphs.len()])
        };
    let (anchored, pre) = crate::anchors::flow_effects(flow, req.story, paragraphs)?;
    if pre.iter().zip(&flows).any(|(p, f)| *p > 0.0 && *f != ParaFlow::Single) {
        return Err(LayoutError::UnsupportedComposition);
    }
    let paragraphs: &[ShapedParagraph] = anchored.as_deref().unwrap_or(paragraphs);
    let mut compose = |t: &[Frame<'_>], v: &[FrameVertical]| {
        compose_flow(t, v, &aligns, text, paragraphs, &stylings, &flows, &pre, req.composer, req.space, opts, limits, cjk, page_number, cancel)
    };
    let resize = if autosize { crate::autosize::autosize(&mut thread, &mut verticals, paragraphs, &mut compose, cancel)? } else { None };
    let composed = compose(&thread, &verticals)?;
    let spacing = composed
        .cjk
        .iter()
        .enumerate()
        .flat_map(|(frame, lines)| {
            lines.iter().enumerate().filter_map(move |(line, g)| g.as_ref().map(|gaps| CjkLineSpacing { frame, line, gaps: gaps.clone() }))
        })
        .collect();
    let frames = thread
        .iter()
        .zip(composed.lines)
        .map(|(f, lines)| FramePlacement {
            frame_id: f.id.to_owned(),
            frame_index: f.layer_index,
            spread: f.spread,
            page: f.page,
            origin_x_pt: f.origin_x_pt,
            origin_y_pt: f.origin_y_pt,
            lines,
        })
        .collect();
    Ok((
        LayoutProposal {
            base_revision: req.document.revision,
            frames,
            overset: composed.overset,
        },
        spacing,
        resize.into_iter().collect(),
        composed.facts,
    ))
}

/// One `ParaFlow` per shaped paragraph from the story's overrides (matched by start byte).
fn paragraph_flows(
    req: &ReflowRequest<'_>,
    flow: &StudioLayoutFlowRecords,
) -> Result<Vec<ParaFlow>, LayoutError> {
    let story = req.story.story_id.as_str();
    let mine: Vec<&StudioParagraphFlow> = flow
        .paragraph_flows
        .iter()
        .filter(|p| p.story_id == story)
        .collect();
    if mine
        .iter()
        .any(|m| !req.paragraphs.iter().any(|p| p.source.start == m.paragraph_start))
    {
        return Err(LayoutError::InvalidInput);
    }
    let nonneg = |l: &hsk_studio_folio::Length| {
        let v = pt(l).map_err(|_| LayoutError::InvalidGeometry)?;
        if v < 0.0 { Err(LayoutError::InvalidGeometry) } else { Ok(v) }
    };
    req.paragraphs
        .iter()
        .map(|p| {
            let Some(m) = mine.iter().find(|m| m.paragraph_start == p.source.start) else {
                return Ok(ParaFlow::Single);
            };
            let (before, after) = (nonneg(&m.space_before)?, nonneg(&m.space_after)?);
            Ok(match m.span_column {
                StudioSpanColumn::SingleColumn => ParaFlow::Single,
                StudioSpanColumn::SpanColumns => {
                    if m.span_column_count == 0 && m.span_count.is_none() {
                        return Err(LayoutError::InvalidInput);
                    }
                    ParaFlow::Span {
                        count: match m.span_count {
                            Some(StudioSpanCount::All) => None,
                            None => Some(m.span_column_count as usize),
                        },
                        before,
                        after,
                    }
                }
                StudioSpanColumn::SplitColumns => {
                    if !(2..=hsk_studio_folio::MAX_SPLIT_COLUMNS).contains(&m.split_column_count) {
                        return Err(LayoutError::InvalidInput);
                    }
                    ParaFlow::Split {
                        count: m.split_column_count as usize,
                        inside: nonneg(&m.inside_gutter)?,
                        outside: nonneg(&m.outside_gutter)?,
                        before,
                        after,
                    }
                }
            })
        })
        .collect()
}

/// Lines per frame of `thread` (same order) and the overset span.
pub(crate) struct Composition {
    pub lines: Vec<Vec<Line>>,
    pub overset: Option<SourceSpan>,
    /// Per frame, parallel to `lines`: the aki gaps of CJK-composed lines (`None` = not CJK).
    pub cjk: Vec<Vec<Option<Vec<CjkGap>>>>,
    /// Break and flush-space outcomes (`sentinels` module).
    pub facts: FlowFacts,
    /// Per frame, parallel to `lines`: the width the line's indents take from its slot (left, first
    /// line, right and a positive last-line indent; STU-TYP-185), for auto-size.
    pub indent: Vec<Vec<f64>>,
}

/// The composition core shared by `reflow_typeset`, tables and notes: flows already-styled
/// `paragraphs` (one optional styling each) through the plain-number `thread`. Inputs are checked
/// here (budgets, fonts, scripts, line metrics) so every caller gets the same typed refusals.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compose(
    thread: &[Frame<'_>],
    text: &str,
    paragraphs: &[ShapedParagraph],
    stylings: &[Option<ParagraphStyling>],
    composer: Composer,
    req_space: SpaceParams,
    opts: &TypesetOptions<'_>,
    limits: Limits,
    cancel: &CancellationToken,
) -> Result<Composition, LayoutError> {
    let flows = vec![ParaFlow::Single; paragraphs.len()];
    let pre = vec![0.0; paragraphs.len()];
    compose_flow(thread, &[], &[], text, paragraphs, stylings, &flows, &pre, composer, req_space, opts, limits, &[], &|_, _| None, cancel)
}

/// Moves the lines of one band (all single-column lines, no obstacles in the frame) so that the
/// columns are balanced: with L lines and n columns, column c takes lines [c*k, (c+1)*k), k =
/// ceil(L/n). Column widths are equal, so the line breaks stand. Returns false (nothing moved) when
/// the band cannot be balanced or a moved line would leave the frame.
fn balance_band(f: &Frame<'_>, band: Option<f64>, lines: &mut [Line], meta: &[Meta]) -> bool {
    let n = f.column_count;
    if n < 2 || !f.obstacles.is_empty() || lines.is_empty() || meta.iter().any(|m| !m.single) {
        return false;
    }
    let k = lines.len().div_ceil(n);
    let bottom = f.height_pt - f.inset_bottom_pt + EPS;
    let mut moved = Vec::with_capacity(lines.len());
    // STU-TYP-185 paragraph space of the lines above in the same column (none at a column's top).
    let mut extra = 0.0;
    for (i, (l, m)) in lines.iter().zip(meta).enumerate() {
        let (col, row) = (i / k, i % k);
        if row == 0 {
            extra = 0.0;
        } else if m.gap != 0.0 {
            extra += m.gap;
            if !extra.is_finite() {
                return false;
            }
        }
        let cur = Cursor {
            pos: 0,
            col,
            line: row,
            band,
            extra,
            first: None,
        };
        // The column's first line fixes its first baseline (known: that line was placed there).
        let Some(first) = meta[col * k].first else {
            return false;
        };
        let baseline = baseline_of(&cur, first, m.height, m.ascent);
        if baseline + (m.height - m.ascent) > bottom {
            return false;
        }
        let dx = f.column_x(col).0 - f.column_x(l.column).0;
        moved.push((col, baseline, l.x_start_pt + dx));
    }
    for (l, (col, baseline, x)) in lines.iter_mut().zip(moved) {
        l.column = col;
        l.baseline_y_pt = baseline;
        l.x_start_pt = x;
    }
    true
}

/// Vertical justification of one composed frame (see the module notes): each column's lines move
/// as a block (`center`, `bottom`) or get the slack in their gaps (`justify`).
fn vertical_justify(f: &Frame<'_>, v: FrameVertical, lines: &mut [Line], meta: &[Meta]) -> Result<(), LayoutError> {
    if v.vjust == VerticalJustification::Top || lines.is_empty() {
        return Ok(());
    }
    if meta.iter().any(|m| !m.single) {
        return Err(LayoutError::UnsupportedComposition);
    }
    if !f.obstacles.is_empty() {
        return Ok(());
    }
    let (top, bottom) = (f.inset_top_pt, f.height_pt - f.inset_bottom_pt);
    for col in 0..f.column_count.max(1) {
        let idx: Vec<usize> = (0..lines.len()).filter(|&i| lines[i].column == col).collect();
        let (Some(&first), Some(&last)) = (idx.first(), idx.last()) else { continue };
        let block_top = lines[first].baseline_y_pt - meta[first].ascent;
        let block_bottom = lines[last].baseline_y_pt + (meta[last].height - meta[last].ascent);
        let slack = bottom - block_bottom;
        if slack <= EPS {
            continue;
        }
        match v.vjust {
            VerticalJustification::Top => {}
            VerticalJustification::Center | VerticalJustification::Bottom => {
                let dy = if v.vjust == VerticalJustification::Center {
                    top + ((bottom - top) - (block_bottom - block_top)) / 2.0 - block_top
                } else {
                    slack
                };
                for &i in &idx {
                    lines[i].baseline_y_pt += dy;
                }
            }
            VerticalJustification::Justify => {
                let gaps = idx.len() - 1;
                if gaps == 0 {
                    continue;
                }
                let para_gaps = idx.windows(2).filter(|w| lines[w[0]].paragraph != lines[w[1]].paragraph).count();
                let mut per_para = if para_gaps > 0 { slack / para_gaps as f64 } else { 0.0 };
                if let Some(limit) = v.threshold_pt {
                    per_para = per_para.min(limit);
                }
                let per_line = (slack - per_para * para_gaps as f64) / gaps as f64;
                let mut dy = 0.0;
                for w in idx.windows(2) {
                    dy += per_line;
                    if lines[w[0]].paragraph != lines[w[1]].paragraph {
                        dy += per_para;
                    }
                    lines[w[1]].baseline_y_pt += dy;
                }
            }
        }
    }
    Ok(())
}

fn is_flush(text: &str, c: SourceSpan) -> bool {
    match (usize::try_from(c.start), usize::try_from(c.end)) {
        (Ok(a), Ok(b)) => text.get(a..b) == Some("\u{FDDD}"),
        _ => false,
    }
}

/// The break sentinel that ends paragraph `i`: the last character after its content (its last
/// glyph, else its start) up to the next paragraph's start, when that is a break sentinel. The
/// terminator ends the paragraph's span, so it is the last character there; a marker without
/// glyphs (index marker, unresolved marker) may stand between the content and the terminator
/// (STUDIO-R53, SMB-LAYOUT-SENTINELS-1 remediation; found composing through native Type).
fn paragraph_break(text: &str, paragraphs: &[ShapedParagraph], i: usize) -> Option<(u64, BreakTarget)> {
    let p = paragraphs.get(i)?;
    let content_end = p.glyphs.iter().map(|g| g.cluster.end).fold(p.source.start, u64::max);
    let next = paragraphs.get(i + 1).map_or(text.len() as u64, |n| n.source.start);
    let tail = text.get(usize::try_from(content_end).ok()?..usize::try_from(next).ok()?)?;
    let (offset, c) = tail.char_indices().next_back()?;
    if !hsk_studio_type::is_break_sentinel(c) {
        return None;
    }
    let at = content_end.checked_add(u64::try_from(offset).ok()?)?;
    hsk_studio_type::catalog_member_for_scalar(c)?.break_target.map(|t| (at, t))
}

/// Where the text after a break continues (`sentinels` module), and how many thread frames were
/// passed over; `last` is the paragraph's last slot, `after` the cursor the flow would take.
fn break_target(
    thread: &[Frame<'_>],
    last: Slot,
    after: Cursor,
    target: BreakTarget,
    page_number: &dyn Fn(usize, usize) -> Option<u32>,
) -> Result<(Cursor, usize), BreakOverset> {
    let next_frame = (last.pos + 1 < thread.len()).then_some((Cursor::frame(last.pos + 1), 0));
    let from = &thread[last.pos];
    match target {
        BreakTarget::Column if last.col + 1 < from.column_count.max(1) => Ok((
            Cursor { pos: last.pos, col: last.col + 1, line: 0, band: after.band, extra: 0.0, first: None },
            0,
        )),
        BreakTarget::Column | BreakTarget::Frame => next_frame.ok_or(BreakOverset::NoTarget),
        BreakTarget::Page | BreakTarget::OddPage | BreakTarget::EvenPage => {
            let here = (from.spread, from.page);
            for (q, f) in thread.iter().enumerate().skip(last.pos + 1) {
                if (f.spread, f.page) <= here {
                    continue;
                }
                let fits = match target {
                    BreakTarget::Page => true,
                    _ => {
                        let n = page_number(f.spread, f.page).ok_or(BreakOverset::ParityUnknown)?;
                        (n % 2 == 1) == (target == BreakTarget::OddPage)
                    }
                };
                if fits {
                    return Ok((Cursor::frame(q), q - last.pos - 1));
                }
            }
            Err(BreakOverset::NoTarget)
        }
        // Not break sentinels (`paragraph_break` only returns the five sentinel targets).
        BreakTarget::Line | BreakTarget::Paragraph => Ok((after, 0)),
    }
}

/// `compose` with one `ParaFlow` per paragraph (span and split columns, band balancing).
#[allow(clippy::too_many_arguments)]
fn compose_flow(
    thread: &[Frame<'_>],
    verticals: &[FrameVertical],
    aligns: &[StudioAlignToGrid],
    text: &str,
    paragraphs: &[ShapedParagraph],
    stylings: &[Option<ParagraphStyling>],
    flows: &[ParaFlow],
    pre: &[f64],
    composer: Composer,
    req_space: SpaceParams,
    opts: &TypesetOptions<'_>,
    limits: Limits,
    cjk: &[Option<CjkParagraph>],
    page_number: &dyn Fn(usize, usize) -> Option<u32>,
    cancel: &CancellationToken,
) -> Result<Composition, LayoutError> {
    canceled(cancel)?;
    let mut facts = FlowFacts::default();
    if paragraphs.len() > limits.max_paragraphs
        || stylings.len() != paragraphs.len()
        || flows.len() != paragraphs.len()
        || pre.len() != paragraphs.len()
        || paragraphs
            .iter()
            .try_fold(0usize, |n, p| n.checked_add(p.glyphs.len()))
            .is_none_or(|n| n > limits.max_glyphs)
    {
        return Err(LayoutError::BudgetExceeded);
    }
    let hyphenator = opts.hyphenator.unwrap_or_else(|| Hyphenator::en_us());
    if paragraphs.iter().any(|p| p.font_missing) {
        return Err(LayoutError::MissingFont);
    }
    // A paragraph with a CJK composer binding composes CJK runs (lane SMB-ID-CJK-KINSOKU-1).
    let cjk_of = |i: usize| cjk.get(i).and_then(Option::as_ref);
    if paragraphs.iter().enumerate().any(|(i, p)| p.right_to_left || (!p.latin_only && cjk_of(i).is_none())) {
        return Err(LayoutError::UnsupportedComposition);
    }
    if paragraphs.iter().any(|p| {
        !(p.line_height_pt.is_finite() && p.ascent_pt.is_finite())
            || p.line_height_pt <= 0.0
            || p.ascent_pt < 0.0
            || p.ascent_pt > p.line_height_pt
            || [p.cap_height_pt, p.x_height_pt, p.em_box_pt].into_iter().flatten().any(|v| !(v.is_finite() && v >= 0.0))
    }) {
        return Err(LayoutError::InvalidGeometry);
    }
    let mut frames: Vec<Vec<Line>> = thread.iter().map(|_| Vec::new()).collect();
    let mut metas: Vec<Vec<Meta>> = thread.iter().map(|_| Vec::new()).collect();
    let mut cjk_lines: Vec<Vec<Option<Vec<CjkGap>>>> = thread.iter().map(|_| Vec::new()).collect();
    let mut indents: Vec<Vec<f64>> = thread.iter().map(|_| Vec::new()).collect();
    // Index of the first line of the current band, per frame (bands start after span paragraphs).
    let mut band_start: Vec<usize> = vec![0; thread.len()];
    let mut band_top: Vec<Option<f64>> = vec![None; thread.len()];
    // Frames whose lines a column break placed: never re-balanced (that would undo the break).
    let mut no_balance: Vec<bool> = vec![false; thread.len()];
    let story_end = paragraphs.last().map_or(0, |p| p.source.end);
    let (mut cursor, mut placed, mut overset) = (Cursor::frame(0), 0usize, None);
    let scan_cap_base = limits.max_lines.saturating_add(1);
    // STU-TYP-185: the space after the previous paragraph, added with this one's space before.
    let mut pending_after = 0.0f64;
    for (pi, p) in paragraphs.iter().enumerate() {
        canceled(cancel)?;
        // Composer, H&J limits and hyphenation of this paragraph: the request's unless its Folio style
        // chain or local override states composition settings (`typeset::paragraph_params`).
        let styling = stylings.get(pi).and_then(Option::as_ref);
        let pp = paragraph_params(
            styling.and_then(|s| s.composition.as_ref()),
            styling.map_or(0.0, |s| s.font_size_pt),
            composer,
            req_space,
            opts,
        )?;
        let (composer, jp) = (pp.composer, pp.justify);
        // STU-TYP-185 indents and spacing (module notes): the gap above this paragraph is the
        // pending space after plus this space before, none between paragraphs of one style whose
        // `space_between_same_style` holds.
        let g = pp.geometry;
        let first_left = sum(g.left_indent_pt, g.first_line_indent_pt)?;
        let gap = if styling.is_some_and(|s| s.same_style_no_space) { 0.0 } else { sum(pending_after, g.space_before_pt)? };
        pending_after = g.space_after_pt;
        let hyph = pp.hyphenation.as_ref().map(|h| (hyphenator, h));
        let rules = pp.hyphenation.as_ref().map_or(HyphenRules::NONE, |h| HyphenRules {
            limit: h.consecutive_limit,
            double_demerits: h.double_hyphen_demerits,
        });
        // CJK paragraphs: the explicit paragraph scope overrides the named composer's (STU-TYP-136).
        let cjk_p = cjk_of(pi).map(|c| {
            let mut c = c.clone();
            if let Some(scope) = styling.and_then(|s| s.composition.as_ref()).and_then(|k| k.settings.composition_scope) {
                c.scope = scope;
            }
            c
        });
        let cjk_items = match &cjk_p {
            Some(c) => Some(crate::cjk::build(p, text, c, styling.map_or(0.0, |s| s.font_size_pt), pp.space)?),
            None => None,
        };
        let items = match cjk_items.as_ref() {
            Some(ci) => ci.items.clone(),
            None => build_items(p, text, pp.space, hyph, pp.hyphenate_last_word)?,
        };
        let sums = Sums::new(&items);
        let para_alignment = stylings.get(pi).and_then(Option::as_ref).map_or(opts.alignment, |s| s.alignment);
        let justified = matches!(para_alignment, Alignment::Justified | Alignment::JustifiedAll);
        let adobe = matches!(composer, Composer::AdobeSingleLine(_) | Composer::AdobeParagraph(_));
        let remaining = limits.max_lines.saturating_sub(placed);
        let slot_cap = items.len().min(remaining.saturating_add(1));
        let scan_cap = scan_cap_base.saturating_add(items.len());
        let (mut slots, mut scans, mut blocked) = (Vec::new(), 0usize, None);
        let descent = p.line_height_pt - p.ascent_pt;
        let flow = flows[pi];
        let mut breaks = None;
        let mut after_cursor = None;
        match flow {
            ParaFlow::Single => {
                // The paragraph space; a paragraph at a column top takes none, and a push past the
                // column bottom lands at the next column's top (`next_slot` resets `extra`).
                if cursor.line > 0 && gap != 0.0 {
                    cursor.extra = sum(cursor.extra, gap)?;
                }
                // Above-line anchored objects push the paragraph down (anchors module); a push that
                // falls into the next column or frame is re-applied at that column's top.
                if pre[pi] > 0.0 {
                    let mut c = cursor;
                    c.extra += pre[pi];
                    if let Some(s) =
                        next_slot(thread, c, p, Mode::Column, verticals, false, &mut blocked, &mut scans, scan_cap, cancel)?
                        && (s.pos != cursor.pos || s.col != cursor.col)
                    {
                        c = Cursor {
                            pos: s.pos,
                            col: s.col,
                            line: 0,
                            band: if s.pos == cursor.pos { cursor.band } else { None },
                            extra: pre[pi],
                            first: None,
                        };
                    }
                    cursor = c;
                }
                let mut at = cursor;
                let align = aligns.get(pi).copied().unwrap_or(StudioAlignToGrid::None);
                while slots.len() < slot_cap {
                    let snap = match align {
                        StudioAlignToGrid::None => false,
                        StudioAlignToGrid::AllLines => true,
                        StudioAlignToGrid::FirstLineOnly => slots.is_empty(),
                    };
                    match next_slot(thread, at, p, Mode::Column, verticals, snap, &mut blocked, &mut scans, scan_cap, cancel)? {
                        Some(s) => {
                            at = s.next;
                            let left = if slots.is_empty() { first_left } else { g.left_indent_pt };
                            if let Some(s) = indented(s, left, g.right_indent_pt)? {
                                slots.push(s);
                            }
                        }
                        None => break,
                    }
                }
            }
            ParaFlow::Span { count, before, .. } => {
                // A span paragraph starts a new band below the deepest line of the current band of
                // its frame (that band balanced first, like a straddle head; UNVERIFIED, D5).
                if let Some(f) = thread.get(cursor.pos) {
                    if let Some(n) = count
                        && n > 1
                        && n < f.column_count
                    {
                        return Err(LayoutError::UnsupportedComposition);
                    }
                    let pos = cursor.pos;
                    let start = band_start[pos];
                    if frames[pos].len() > start {
                        if !no_balance[pos] {
                            balance_band(f, cursor.band, &mut frames[pos][start..], &metas[pos][start..]);
                        }
                        let bottom = frames[pos][start..]
                            .iter()
                            .zip(&metas[pos][start..])
                            .map(|(l, m)| l.baseline_y_pt + (m.height - m.ascent))
                            .fold(f64::MIN, f64::max);
                        cursor = Cursor {
                            pos,
                            col: 0,
                            line: 0,
                            band: Some(sum(bottom, larger(before, gap))?),
                            extra: 0.0,
                            first: None,
                        };
                    } else {
                        cursor.col = 0;
                        cursor.first = None;
                    }
                }
                let mut at = cursor;
                while slots.len() < slot_cap {
                    match next_slot(thread, at, p, Mode::Span, verticals, false, &mut blocked, &mut scans, scan_cap, cancel)? {
                        Some(s) => {
                            at = s.next;
                            let left = if slots.is_empty() { first_left } else { g.left_indent_pt };
                            if let Some(s) = indented(s, left, g.right_indent_pt)? {
                                slots.push(s);
                            }
                        }
                        None => break,
                    }
                }
            }
            ParaFlow::Split { count, inside, outside, before, .. } => {
                if cursor.line > 0 {
                    cursor.extra = sum(cursor.extra, larger(before, gap))?;
                }
                let mut start = cursor;
                while let Some(probe) =
                    next_slot(thread, start, p, Mode::Column, verticals, false, &mut blocked, &mut scans, scan_cap, cancel)?
                {
                    let f = &thread[probe.pos];
                    if !f.obstacles.is_empty() {
                        return Err(LayoutError::UnsupportedComposition);
                    }
                    let (c0, c1) = f.column_x(probe.col);
                    let m = count as f64;
                    let sub_w = (c1 - c0 - 2.0 * outside - (m - 1.0) * inside) / m;
                    if sub_w <= EPS {
                        return Err(LayoutError::InvalidGeometry);
                    }
                    // Indents narrow each sub-column line (the first line by the first-line indent too);
                    // a sub-column the indents leave without width is `invalid_geometry`, as above.
                    let narrow = |left: f64| -> Result<f64, LayoutError> {
                        if left == 0.0 && g.right_indent_pt == 0.0 {
                            return Ok(sub_w);
                        }
                        let w = sum(sum(sub_w, -left)?, -g.right_indent_pt)?;
                        if w > EPS { Ok(w) } else { Err(LayoutError::InvalidGeometry) }
                    };
                    let (w_first, w_rest) = (narrow(first_left)?, narrow(g.left_indent_pt)?);
                    let width = |k: usize| if k == 0 { w_first } else { w_rest };
                    let b = match (&cjk_items, &cjk_p) {
                        (Some(ci), Some(c)) => crate::cjk::break_lines(ci, &sums, &width, c, cancel)?,
                        _ => break_lines(composer, &items, &sums, &width, &rules, &jp, justified, para_alignment == Alignment::JustifiedAll, cancel)?,
                    };
                    let rows_needed = b.len().div_ceil(count).max(1);
                    let (mut rows, mut at) = (vec![probe], probe.next);
                    while rows.len() < rows_needed {
                        match next_slot(thread, at, p, Mode::Column, verticals, false, &mut blocked, &mut scans, scan_cap, cancel)? {
                            Some(s) if s.pos == probe.pos => {
                                at = s.next;
                                rows.push(s);
                            }
                            _ => break,
                        }
                    }
                    if rows.len() < rows_needed && probe.pos + 1 < thread.len() {
                        // The split block does not fit this frame: it moves whole to the next one.
                        start = Cursor::frame(probe.pos + 1);
                        continue;
                    }
                    let r = rows.len();
                    for i in 0..(r * count).min(b.len()) {
                        let (sub, row) = (i / r, i % r);
                        let s = rows[row];
                        let x0 = thread[s.pos].column_x(s.col).0 + outside + sub as f64 * (sub_w + inside);
                        let left = if i == 0 { first_left } else { g.left_indent_pt };
                        let x0 = if left == 0.0 { x0 } else { sum(x0, left)? };
                        slots.push(Slot {
                            x_start_pt: x0,
                            width_pt: width(i),
                            ..s
                        });
                    }
                    after_cursor = Some(rows[r - 1].next);
                    breaks = Some(b);
                    break;
                }
            }
        }
        if slots.is_empty() {
            if let Some(e) = blocked {
                return Err(LayoutError::FirstBaselineMetricUnavailable(e));
            }
            overset = Some(SourceSpan {
                start: p.source.start,
                end: story_end,
            });
            break;
        }
        let breaks = match breaks {
            Some(b) => b,
            None => {
                let last_width = slots[slots.len() - 1].width_pt;
                let width = |line: usize| slots.get(line).map_or(last_width, |s| s.width_pt);
                let mut b = match (&cjk_items, &cjk_p) {
                    (Some(ci), Some(c)) => crate::cjk::break_lines(ci, &sums, &width, c, cancel)?,
                    _ => break_lines(composer, &items, &sums, &width, &rules, &jp, justified, para_alignment == Alignment::JustifiedAll, cancel)?,
                };
                // STU-TYP-185 `last_line_indent` (Studio reading, UNVERIFIED, D5): the last line is
                // known only after breaking; when it does not fit its narrowed width the paragraph is
                // broken once more with that width narrowed, and the second result stands. Bounded:
                // one re-break, only for a positive indent, a Latin paragraph and a line that fitted
                // before, and only when the narrowed width holds the line's widest box (word or
                // hyphenation fragment): a narrower width cannot be met by any break, and re-breaking
                // there moved the Adobe paragraph composer's refusal point and cost Knuth-Plass
                // hundreds of times its pass (lead ruling 2026-11-08, lane notes). Otherwise the
                // breaks stand and the set stage marks the line overfull. Every composer narrows
                // the line that ENDS the paragraph, whatever its number (lead ruling 2026-11-11);
                // the Adobe paragraph composer re-breaks only its last `PARA_TAIL_LINES + 1` lines
                // (`adobe_composer::last_line_rebreak`).
                if g.last_line_indent_pt > 0.0
                    && cjk_items.is_none()
                    && let Some(l) = b.len().checked_sub(1)
                    && l < slots.len()
                    && !b[l].overfull
                {
                    let narrowed = sum(slots[l].width_pt, -g.last_line_indent_pt)?;
                    if narrowed > EPS && tightest_width(p, &items, &b, l, adobe, &jp) > narrowed + EPS && widest_box(&items, &b, l) <= narrowed + EPS {
                        let indent = g.last_line_indent_pt;
                        let justify_last = para_alignment == Alignment::JustifiedAll;
                        b = match composer {
                            Composer::FirstFit => first_fit_last(&items, &sums, &width, &rules, cancel, indent)?,
                            Composer::KnuthPlass(params) => knuth_plass_last(&items, &sums, &width, &params, &rules, cancel, indent)?,
                            Composer::AdobeSingleLine(params) => {
                                let ctx = AdobeContext { params, justify: &jp, justified, justify_last };
                                single_line_last(&items, &sums, &width, &rules, &ctx, cancel, indent)?
                            }
                            Composer::AdobeParagraph(params) => {
                                let ctx = AdobeContext { params, justify: &jp, justified, justify_last };
                                last_line_rebreak(&items, &sums, &width, &rules, &ctx, cancel, b, indent, &mut ParaStats::default())?
                            }
                        };
                    }
                }
                b
            }
        };
        // The last line's own indent narrows (or, negative, widens) its slot for the set stage; a
        // narrowed Latin line that still does not fit is overfull.
        let mut last_overfull = false;
        if g.last_line_indent_pt != 0.0
            && let Some(l) = breaks.len().checked_sub(1)
            && l < slots.len()
        {
            let w = sum(slots[l].width_pt, -g.last_line_indent_pt)?;
            slots[l].width_pt = w;
            last_overfull = g.last_line_indent_pt > 0.0 && cjk_items.is_none() && tightest_width(p, &items, &breaks, l, adobe, &jp) > w + EPS;
        }
        // Width the indents take from each line's slot (auto-size).
        let line_indent = |k: usize| -> Result<f64, LayoutError> {
            let mut t = sum(if k == 0 { first_left } else { g.left_indent_pt }, g.right_indent_pt)?;
            if k + 1 == breaks.len() && g.last_line_indent_pt > 0.0 {
                t = sum(t, g.last_line_indent_pt)?;
            }
            Ok(t)
        };
        let n = p.glyphs.len();
        let mut starts = vec![0usize];
        let mut item_starts = vec![0usize];
        for b in &breaks[..breaks.len().saturating_sub(1)] {
            let s = line_start(&items, b.item);
            item_starts.push(s);
            starts.push(items.get(s).map_or(n, |it| it.first));
        }
        starts.push(n);
        // `+ 0.0` turns the empty sum (-0.0) into +0.0 so hang and width never print as "-0".
        let advance = |r: Range<usize>| p.glyphs[r].iter().map(|g| g.advance_pt).sum::<f64>() + 0.0;
        let mut lines_here = Vec::new();
        let mut indent_here = Vec::new();
        let mut flush_here: Vec<Vec<(u64, Option<f64>)>> = Vec::new();
        for (k, b) in breaks.iter().enumerate() {
            let range = starts[k]..starts[k + 1].max(starts[k]);
            let content_end = items[item_starts[k]..b.item]
                .iter()
                .rev()
                .find(|it| it.kind == crate::paragraph::Kind::Box)
                .map_or(range.start, |it| it.end.min(range.end));
            let boundary = |g: usize| {
                p.glyphs
                    .get(g)
                    .map_or(p.source.end, |glyph| glyph.cluster.start)
            };
            // Type composition v2 line-end data at the line's edges: the material a hyphenation break
            // sets before it, and the right part of a ligature the previous break split.
            let end_part = items[b.item].end_part.filter(|_| items[b.item].flagged);
            let head = items
                .get(item_starts[k])
                .filter(|it| it.kind == crate::paragraph::Kind::Box && it.first == range.start && !range.is_empty())
                .and_then(|it| it.head);
            let mut source = if let Some(blank) = items[b.item].blank {
                blank
            } else if range.is_empty() {
                SourceSpan {
                    start: boundary(range.start),
                    end: boundary(range.start),
                }
            } else {
                SourceSpan {
                    start: p.glyphs[range.start].cluster.start,
                    end: p.glyphs[range.end - 1].cluster.end,
                }
            };
            if let Some(h) = head {
                source.start = h.start;
            }
            if let Some(e) = end_part.filter(|e| e.extra) {
                source.end = e.end;
            }
            let start_part = head.map(|h| LinePart {
                glyph: range.start,
                source: SourceSpan {
                    start: h.start,
                    end: p.glyphs[range.start].cluster.end,
                },
                advance_pt: h.advance_pt,
            });
            let end_part = end_part.map(|e| LinePart {
                glyph: e.glyph,
                source: SourceSpan {
                    start: p.glyphs[e.glyph].cluster.start,
                    end: e.end,
                },
                advance_pt: e.advance_pt,
            });
            let extra_glyph = items[b.item].end_part.is_some_and(|e| e.extra && items[b.item].flagged);
            if k >= slots.len() {
                if let Some(e) = blocked {
                    return Err(LayoutError::FirstBaselineMetricUnavailable(e));
                }
                overset = Some(SourceSpan {
                    start: source.start,
                    end: story_end,
                });
                break;
            }
            let slot = &slots[k];
            indent_here.push(line_indent(k)?);
            if let Some(ci) = &cjk_items {
                // CJK line: set through its aki glue (`cjk::finish_line`); x_start is the first
                // glyph's origin, the gaps carry the spacing, no Latin justification.
                let alignment = stylings.get(pi).and_then(Option::as_ref).map_or(opts.alignment, |s| s.alignment);
                let is_last = k + 1 == breaks.len();
                let justify = matches!(alignment, Alignment::Justified | Alignment::JustifiedAll) && (!is_last || alignment == Alignment::JustifiedAll);
                let set = crate::cjk::finish_line(ci, p, item_starts[k], b.item, slot.width_pt, justify);
                let room = (slot.width_pt - set.lead_pt - set.width_pt).max(0.0);
                let shift = match alignment {
                    Alignment::Center => room / 2.0,
                    Alignment::Right => room,
                    _ => 0.0,
                };
                lines_here.push((
                    slot.pos,
                    Line {
                        paragraph: pi,
                        baseline_y_pt: slot.baseline_y_pt,
                        x_start_pt: slot.x_start_pt + shift + set.start_offset_pt,
                        width_pt: set.width_pt,
                        available_pt: slot.width_pt,
                        hang_pt: set.hang_pt,
                        source,
                        glyph_range: range,
                        overfull: b.overfull || set.overfull,
                        ratio: Some(set.ratio),
                        hyphenated: false,
                        justification: None,
                        column: slot.col,
                        start_part: None,
                        end_part: None,
                    },
                    Some(set.gaps),
                ));
                flush_here.push(Vec::new());
                continue;
            }
            let hyphenated = items[b.item].flagged;
            // A flagged penalty's width is the hyphen plus Type's line-end change (`build_items`).
            let width_pt = advance(range.start..content_end)
                + if hyphenated { items[b.item].w } else { 0.0 }
                + head.map_or(0.0, |h| h.delta);
            let alignment = stylings
                .get(pi)
                .and_then(Option::as_ref)
                .map_or(opts.alignment, |s| s.alignment);
            let is_last = k + 1 == breaks.len();
            let overfull = b.overfull || (is_last && last_overfull);
            // Flush spaces of the line (`sentinels` module): on the last line of a fully justified
            // paragraph they take the rest of the slot and the line keeps its natural spacing.
            let flush: Vec<u64> = p.glyphs[range.clone()].iter().filter(|g| is_flush(text, g.cluster)).map(|g| g.cluster.start).collect();
            let absorbs = !flush.is_empty() && !overfull && is_last && alignment == Alignment::JustifiedAll;
            let absorbed = absorbs.then(|| (slot.width_pt - width_pt).max(0.0) / flush.len() as f64);
            flush_here.push(flush.into_iter().map(|at| (at, absorbed)).collect());
            let mut x_start_pt = slot.x_start_pt;
            let mut justification = None;
            if !overfull && !absorbs {
                let spaces: f64 = items[item_starts[k]..b.item]
                    .iter()
                    .filter(|it| {
                        it.kind == crate::paragraph::Kind::Glue
                            && !it.trailing
                            && !it.inf_stretch
                            && it.end <= content_end
                    })
                    .map(|it| it.w)
                    .sum();
                match alignment {
                    Alignment::Left => {}
                    Alignment::Center => x_start_pt += (slot.width_pt - width_pt).max(0.0) / 2.0,
                    Alignment::Right => x_start_pt += (slot.width_pt - width_pt).max(0.0),
                    // STU-TYP-146: a justified line holding one word (no inner word space) set as
                    // the paragraph's single-word justification says; fully justified when unset.
                    Alignment::Justified | Alignment::JustifiedAll
                        if (!is_last || alignment == Alignment::JustifiedAll)
                            && spaces == 0.0
                            && matches!(
                                pp.single_word,
                                Some(StudioSingleWordJustification::LeftAlign | StudioSingleWordJustification::CenterAlign | StudioSingleWordJustification::RightAlign)
                            ) =>
                    {
                        let room = (slot.width_pt - width_pt).max(0.0);
                        match pp.single_word {
                            Some(StudioSingleWordJustification::CenterAlign) => x_start_pt += room / 2.0,
                            Some(StudioSingleWordJustification::RightAlign) => x_start_pt += room,
                            _ => {}
                        }
                    }
                    Alignment::Justified | Alignment::JustifiedAll if !is_last || alignment == Alignment::JustifiedAll => {
                        // Adobe composers: a hyphen is one more glyph gap of the line (oracle ID-02
                        // letter spacing: the hyphen takes its letter gap like the letters).
                        // The left part of a ligature split by the hyphen is one more glyph.
                        let gaps = content_end.saturating_sub(range.start).saturating_sub(1)
                            + usize::from(adobe && hyphenated)
                            + usize::from(extra_glyph);
                        justification = Some(if adobe {
                            justify_line(width_pt, slot.width_pt, spaces, gaps, &jp)
                        } else {
                            justify_metrics(width_pt, slot.width_pt, spaces, gaps, &jp)
                        });
                    }
                    Alignment::Justified | Alignment::JustifiedAll => {}
                }
                // Adobe composers may set a line tighter than its slot (a compressed last line or a
                // ragged line chosen tight): its spaces close to the measure.
                if adobe && justification.is_none() {
                    justification = compress_line(width_pt, slot.width_pt, spaces);
                }
            }
            lines_here.push((
                slot.pos,
                Line {
                    paragraph: pi,
                    baseline_y_pt: slot.baseline_y_pt,
                    x_start_pt,
                    width_pt,
                    available_pt: slot.width_pt,
                    hang_pt: advance(content_end..range.end),
                    source,
                    glyph_range: range,
                    overfull,
                    ratio: b.ratio,
                    hyphenated,
                    justification,
                    column: slot.col,
                    start_part,
                    end_part,
                },
                None,
            ));
        }
        placed = placed
            .checked_add(lines_here.len())
            .filter(|n| *n <= limits.max_lines)
            .ok_or(LayoutError::BudgetExceeded)?;
        let used = lines_here.len();
        let single = flow == ParaFlow::Single;
        for (k, (((pos, line, gaps), flush), indent)) in lines_here.into_iter().zip(flush_here).zip(indent_here).enumerate() {
            let index = frames[pos].len();
            // The paragraph's gap rides on its first line wherever it landed: balancing adds it back
            // when that line no longer opens a column (review F2) and drops it at a column's top.
            let gap = if k == 0 { gap } else { 0.0 };
            facts.flush.extend(flush.into_iter().map(|(story_byte, absorbed_pt)| FlushOutcome { story_byte, frame: pos, line: index, absorbed_pt }));
            frames[pos].push(line);
            indents[pos].push(indent);
            cjk_lines[pos].push(gaps);
            metas[pos].push(Meta {
                ascent: p.ascent_pt,
                height: p.line_height_pt,
                single,
                first: frame_first(&thread[pos], verticals.get(pos), p).ok(),
                gap,
            });
        }
        if overset.is_some() {
            break;
        }
        cursor = match flow {
            ParaFlow::Single => slots[used - 1].next,
            ParaFlow::Span { after, .. } => {
                let last = &slots[used - 1];
                pending_after = 0.0;
                let top = sum(last.baseline_y_pt + descent, larger(after, g.space_after_pt))?;
                band_start[last.pos] = frames[last.pos].len();
                band_top[last.pos] = Some(top);
                Cursor {
                    pos: last.pos,
                    col: 0,
                    line: 0,
                    band: Some(top),
                    extra: 0.0,
                    first: None,
                }
            }
            ParaFlow::Split { after, .. } => {
                let mut c = after_cursor.unwrap_or(slots[used - 1].next);
                c.extra = sum(c.extra, larger(after, g.space_after_pt))?;
                pending_after = 0.0;
                c
            }
        };
        // A break sentinel ending the paragraph moves the flow to its target (`sentinels` module).
        if let Some((at, target)) = paragraph_break(text, paragraphs, pi) {
            let last = slots[used - 1];
            match break_target(thread, last, cursor, target, page_number) {
                Ok((c, skipped_frames)) => {
                    if target == BreakTarget::Column {
                        no_balance[last.pos] = true;
                    }
                    cursor = c;
                    pending_after = 0.0;
                    facts.breaks.push(BreakOutcome {
                        story_byte: at,
                        target,
                        result: BreakResult::Applied { frame: c.pos, column: c.col, skipped_frames },
                    });
                }
                Err(why) => {
                    facts.breaks.push(BreakOutcome { story_byte: at, target, result: BreakResult::Overset(why) });
                    overset = Some(SourceSpan { start: at, end: story_end.max(at.saturating_add(3)) });
                    break;
                }
            }
        }
    }
    // Balance the band that ends the story when its frame asks for it.
    if overset.is_none()
        && let Some(pos) = frames.iter().rposition(|l| !l.is_empty())
        && thread[pos].balance
        && !no_balance[pos]
    {
        let start = band_start[pos];
        balance_band(&thread[pos], band_top[pos], &mut frames[pos][start..], &metas[pos][start..]);
    }
    for ((f, v), (lines, meta)) in thread.iter().zip(verticals).zip(frames.iter_mut().zip(&metas)) {
        vertical_justify(f, *v, lines, meta)?;
    }
    Ok(Composition {
        lines: frames,
        overset,
        cjk: cjk_lines,
        facts,
        indent: indents,
    })
}
