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
//! paragraph occupies one empty line; paragraphs have no space before or after.
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
use crate::breaker::{Composer, HyphenRules, Sums, first_fit, knuth_plass, line_start};
use crate::hyphenate::Hyphenator;
use crate::styles::{Alignment, ParagraphStyling};
use crate::typeset::{Justification, TypesetOptions, justify_metrics};
use crate::folio_input::{Frame, FrameObstacles, FrameVertical, pt, resolve_flow};
use crate::paragraph::{ShapedParagraph, SpaceParams, build_items};
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
/// coordinates (`None` = frame top, whose first baseline is the frame's fixed offset).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Cursor {
    pos: usize,
    col: usize,
    line: usize,
    band: Option<f64>,
    extra: f64,
}
impl Cursor {
    const fn frame(pos: usize) -> Self {
        Self {
            pos,
            col: 0,
            line: 0,
            band: None,
            extra: 0.0,
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

fn baseline_of(f: &Frame<'_>, cur: &Cursor, height: f64, ascent: f64) -> f64 {
    let base = cur.band.map_or(f.first_baseline_pt, |t| t + ascent);
    base + cur.line as f64 * height + cur.extra
}

#[allow(clippy::too_many_arguments)]
fn next_slot(
    thread: &[Frame<'_>],
    mut cur: Cursor,
    height: f64,
    ascent: f64,
    mode: Mode,
    scans: &mut usize,
    scan_cap: usize,
    cancel: &CancellationToken,
) -> Result<Option<Slot>, LayoutError> {
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
        let baseline = baseline_of(f, &cur, height, ascent);
        if baseline + descent > f.height_pt - f.inset_bottom_pt + EPS {
            cur = if cur.col + 1 < columns {
                Cursor {
                    col: cur.col + 1,
                    line: 0,
                    extra: 0.0,
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
    let (text, thread, verticals) = (input.text, input.frames, input.verticals);
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
            let (a, b) = crate::styles::apply(src, req.story, req.paragraphs, opts.alignment)?;
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
    let composed = compose_flow(
        &thread,
        &verticals,
        text,
        paragraphs,
        &stylings,
        &flows,
        &pre,
        req.composer,
        req.space,
        opts,
        limits,
        cancel,
    )?;
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
    Ok(LayoutProposal {
        base_revision: req.document.revision,
        frames,
        overset: composed.overset,
    })
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
    compose_flow(thread, &[], text, paragraphs, stylings, &flows, &pre, composer, req_space, opts, limits, cancel)
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
    for (i, (l, m)) in lines.iter().zip(meta).enumerate() {
        let (col, row) = (i / k, i % k);
        let cur = Cursor {
            pos: 0,
            col,
            line: row,
            band,
            extra: 0.0,
        };
        let baseline = baseline_of(f, &cur, m.height, m.ascent);
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

/// `compose` with one `ParaFlow` per paragraph (span and split columns, band balancing).
#[allow(clippy::too_many_arguments)]
fn compose_flow(
    thread: &[Frame<'_>],
    verticals: &[FrameVertical],
    text: &str,
    paragraphs: &[ShapedParagraph],
    stylings: &[Option<ParagraphStyling>],
    flows: &[ParaFlow],
    pre: &[f64],
    composer: Composer,
    req_space: SpaceParams,
    opts: &TypesetOptions<'_>,
    limits: Limits,
    cancel: &CancellationToken,
) -> Result<Composition, LayoutError> {
    canceled(cancel)?;
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
    let jp = opts.justify.unwrap_or_default();
    let space = if opts.justify.is_some() { jp.space_params() } else { req_space };
    let hyphenator = opts.hyphenator.unwrap_or_else(|| Hyphenator::en_us());
    let hyph = opts.hyphenation.as_ref().map(|h| (hyphenator, h));
    let rules = opts.hyphenation.as_ref().map_or(HyphenRules::NONE, |h| HyphenRules {
        limit: h.consecutive_limit,
        double_demerits: h.double_hyphen_demerits,
    });
    let hyphen_w = opts.hyphenation.as_ref().map_or(0.0, |h| h.hyphen_advance_pt);
    if paragraphs.iter().any(|p| p.font_missing) {
        return Err(LayoutError::MissingFont);
    }
    if paragraphs.iter().any(|p| p.right_to_left || !p.latin_only) {
        return Err(LayoutError::UnsupportedComposition);
    }
    if paragraphs.iter().any(|p| {
        !(p.line_height_pt.is_finite() && p.ascent_pt.is_finite())
            || p.line_height_pt <= 0.0
            || p.ascent_pt < 0.0
            || p.ascent_pt > p.line_height_pt
    }) {
        return Err(LayoutError::InvalidGeometry);
    }
    let mut frames: Vec<Vec<Line>> = thread.iter().map(|_| Vec::new()).collect();
    let mut metas: Vec<Vec<Meta>> = thread.iter().map(|_| Vec::new()).collect();
    // Index of the first line of the current band, per frame (bands start after span paragraphs).
    let mut band_start: Vec<usize> = vec![0; thread.len()];
    let mut band_top: Vec<Option<f64>> = vec![None; thread.len()];
    let story_end = paragraphs.last().map_or(0, |p| p.source.end);
    let (mut cursor, mut placed, mut overset) = (Cursor::frame(0), 0usize, None);
    let scan_cap_base = limits.max_lines.saturating_add(1);
    for (pi, p) in paragraphs.iter().enumerate() {
        canceled(cancel)?;
        let items = build_items(p, text, space, hyph)?;
        let sums = Sums::new(&items);
        let remaining = limits.max_lines.saturating_sub(placed);
        let slot_cap = items.len().min(remaining.saturating_add(1));
        let scan_cap = scan_cap_base.saturating_add(items.len());
        let (mut slots, mut scans) = (Vec::new(), 0usize);
        let descent = p.line_height_pt - p.ascent_pt;
        let flow = flows[pi];
        let mut breaks = None;
        let mut after_cursor = None;
        match flow {
            ParaFlow::Single => {
                // Above-line anchored objects push the paragraph down (anchors module); a push that
                // falls into the next column or frame is re-applied at that column's top.
                if pre[pi] > 0.0 {
                    let mut c = cursor;
                    c.extra += pre[pi];
                    if let Some(s) =
                        next_slot(thread, c, p.line_height_pt, p.ascent_pt, Mode::Column, &mut scans, scan_cap, cancel)?
                        && (s.pos != cursor.pos || s.col != cursor.col)
                    {
                        c = Cursor {
                            pos: s.pos,
                            col: s.col,
                            line: 0,
                            band: if s.pos == cursor.pos { cursor.band } else { None },
                            extra: pre[pi],
                        };
                    }
                    cursor = c;
                }
                let mut at = cursor;
                while slots.len() < slot_cap {
                    match next_slot(thread, at, p.line_height_pt, p.ascent_pt, Mode::Column, &mut scans, scan_cap, cancel)? {
                        Some(s) => {
                            at = s.next;
                            slots.push(s);
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
                        balance_band(f, cursor.band, &mut frames[pos][start..], &metas[pos][start..]);
                        let bottom = frames[pos][start..]
                            .iter()
                            .zip(&metas[pos][start..])
                            .map(|(l, m)| l.baseline_y_pt + (m.height - m.ascent))
                            .fold(f64::MIN, f64::max);
                        cursor = Cursor {
                            pos,
                            col: 0,
                            line: 0,
                            band: Some(bottom + before),
                            extra: 0.0,
                        };
                    } else {
                        cursor.col = 0;
                    }
                }
                let mut at = cursor;
                while slots.len() < slot_cap {
                    match next_slot(thread, at, p.line_height_pt, p.ascent_pt, Mode::Span, &mut scans, scan_cap, cancel)? {
                        Some(s) => {
                            at = s.next;
                            slots.push(s);
                        }
                        None => break,
                    }
                }
            }
            ParaFlow::Split { count, inside, outside, before, .. } => {
                if cursor.line > 0 {
                    cursor.extra += before;
                }
                let mut start = cursor;
                while let Some(probe) =
                    next_slot(thread, start, p.line_height_pt, p.ascent_pt, Mode::Column, &mut scans, scan_cap, cancel)?
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
                    let width = |_: usize| sub_w;
                    let b = match composer {
                        Composer::FirstFit => first_fit(&items, &sums, &width, &rules, cancel)?,
                        Composer::KnuthPlass(params) => knuth_plass(&items, &sums, &width, &params, &rules, cancel)?,
                    };
                    let rows_needed = b.len().div_ceil(count).max(1);
                    let (mut rows, mut at) = (vec![probe], probe.next);
                    while rows.len() < rows_needed {
                        match next_slot(thread, at, p.line_height_pt, p.ascent_pt, Mode::Column, &mut scans, scan_cap, cancel)? {
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
                        slots.push(Slot {
                            x_start_pt: x0,
                            width_pt: sub_w,
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
                match composer {
                    Composer::FirstFit => first_fit(&items, &sums, &width, &rules, cancel)?,
                    Composer::KnuthPlass(params) => knuth_plass(&items, &sums, &width, &params, &rules, cancel)?,
                }
            }
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
            let source = if range.is_empty() {
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
            if k >= slots.len() {
                overset = Some(SourceSpan {
                    start: source.start,
                    end: story_end,
                });
                break;
            }
            let slot = &slots[k];
            let hyphenated = items[b.item].flagged;
            let width_pt = advance(range.start..content_end) + if hyphenated { hyphen_w } else { 0.0 };
            let alignment = stylings
                .get(pi)
                .and_then(Option::as_ref)
                .map_or(opts.alignment, |s| s.alignment);
            let is_last = k + 1 == breaks.len();
            let mut x_start_pt = slot.x_start_pt;
            let mut justification = None;
            if !b.overfull {
                match alignment {
                    Alignment::Left => {}
                    Alignment::Center => x_start_pt += (slot.width_pt - width_pt).max(0.0) / 2.0,
                    Alignment::Right => x_start_pt += (slot.width_pt - width_pt).max(0.0),
                    Alignment::Justified if !is_last => {
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
                        let gaps = content_end.saturating_sub(range.start).saturating_sub(1);
                        justification = Some(justify_metrics(width_pt, slot.width_pt, spaces, gaps, &jp));
                    }
                    Alignment::Justified => {}
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
                    overfull: b.overfull,
                    ratio: b.ratio,
                    hyphenated,
                    justification,
                    column: slot.col,
                },
            ));
        }
        placed = placed
            .checked_add(lines_here.len())
            .filter(|n| *n <= limits.max_lines)
            .ok_or(LayoutError::BudgetExceeded)?;
        let used = lines_here.len();
        let single = flow == ParaFlow::Single;
        for (pos, line) in lines_here {
            frames[pos].push(line);
            metas[pos].push(Meta {
                ascent: p.ascent_pt,
                height: p.line_height_pt,
                single,
            });
        }
        if overset.is_some() {
            break;
        }
        cursor = match flow {
            ParaFlow::Single => slots[used - 1].next,
            ParaFlow::Span { after, .. } => {
                let last = &slots[used - 1];
                let top = last.baseline_y_pt + descent + after;
                band_start[last.pos] = frames[last.pos].len();
                band_top[last.pos] = Some(top);
                Cursor {
                    pos: last.pos,
                    col: 0,
                    line: 0,
                    band: Some(top),
                    extra: 0.0,
                }
            }
            ParaFlow::Split { after, .. } => {
                let mut c = after_cursor.unwrap_or(slots[used - 1].next);
                c.extra += after;
                c
            }
        };
    }
    // Balance the band that ends the story when its frame asks for it.
    if overset.is_none()
        && let Some(pos) = frames.iter().rposition(|l| !l.is_empty())
        && thread[pos].balance
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
    })
}
