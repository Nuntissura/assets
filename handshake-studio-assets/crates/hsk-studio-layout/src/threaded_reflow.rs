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
use crate::breaker::{Composer, HyphenRules, Sums, first_fit, knuth_plass, line_start};
use crate::hyphenate::Hyphenator;
use crate::styles::{Alignment, ParagraphStyling};
use crate::typeset::{Justification, TypesetOptions, justify_metrics};
use crate::folio_input::{Frame, FrameObstacles, resolve};
use crate::paragraph::{ShapedParagraph, SpaceParams, build_items};
use crate::{LayoutError, SourceSpan};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{StudioDocument, StudioTextStory};
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

#[derive(Clone, Copy)]
struct Cursor {
    pos: usize,
    line: usize,
}
struct Slot {
    pos: usize,
    baseline_y_pt: f64,
    x_start_pt: f64,
    width_pt: f64,
    next: Cursor,
}

/// Widest gap of the frame's inset width left by obstacles that overlap the line box.
fn free_interval(f: &Frame<'_>, top: f64, bottom: f64) -> (f64, f64) {
    let (lo, hi) = (f.inset_left_pt, f.width_pt - f.inset_right_pt);
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

#[allow(clippy::too_many_arguments)]
fn next_slot(
    thread: &[Frame<'_>],
    mut cur: Cursor,
    height: f64,
    ascent: f64,
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
        let baseline = f.first_baseline_pt + cur.line as f64 * height;
        if baseline + descent > f.height_pt - f.inset_bottom_pt + EPS {
            cur = Cursor {
                pos: cur.pos + 1,
                line: 0,
            };
            continue;
        }
        let (x0, x1) = free_interval(f, baseline - ascent, baseline + descent);
        let next = Cursor {
            pos: cur.pos,
            line: cur.line + 1,
        };
        if x1 - x0 > EPS {
            return Ok(Some(Slot {
                pos: cur.pos,
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
pub fn reflow_typeset(
    req: &ReflowRequest<'_>,
    opts: &TypesetOptions<'_>,
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
    let input = resolve(req.document, req.story, req.resolved_text, req.obstacles)?;
    let (text, thread) = (input.text, input.frames);
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
    let styled;
    let (paragraphs, stylings): (&[ShapedParagraph], Vec<Option<ParagraphStyling>>) =
        if let Some(src) = opts.styles {
            let (a, b) = crate::styles::apply(src, req.story, req.paragraphs, opts.alignment)?;
            styled = a;
            (&styled, b.into_iter().map(Some).collect())
        } else {
            (req.paragraphs, vec![None; req.paragraphs.len()])
        };
    let jp = opts.justify.unwrap_or_default();
    let space = if opts.justify.is_some() { jp.space_params() } else { req.space };
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
    let mut frames: Vec<FramePlacement> = thread
        .iter()
        .map(|f| FramePlacement {
            frame_id: f.id.to_owned(),
            frame_index: f.layer_index,
            spread: f.spread,
            page: f.page,
            origin_x_pt: f.origin_x_pt,
            origin_y_pt: f.origin_y_pt,
            lines: Vec::new(),
        })
        .collect();
    let story_end = paragraphs.last().map_or(0, |p| p.source.end);
    let (mut cursor, mut placed, mut overset) = (Cursor { pos: 0, line: 0 }, 0usize, None);
    for (pi, p) in paragraphs.iter().enumerate() {
        canceled(cancel)?;
        let items = build_items(p, text, space, hyph)?;
        let sums = Sums::new(&items);
        let remaining = limits.max_lines.saturating_sub(placed);
        let slot_cap = items.len().min(remaining.saturating_add(1));
        let scan_cap = limits.max_lines.saturating_add(items.len()).saturating_add(1);
        let (mut slots, mut at, mut scans) = (Vec::new(), cursor, 0usize);
        while slots.len() < slot_cap {
            match next_slot(
                &thread,
                at,
                p.line_height_pt,
                p.ascent_pt,
                &mut scans,
                scan_cap,
                cancel,
            )? {
                Some(s) => {
                    at = s.next;
                    slots.push(s);
                }
                None => break,
            }
        }
        if slots.is_empty() {
            overset = Some(SourceSpan {
                start: p.source.start,
                end: story_end,
            });
            break;
        }
        let last_width = slots[slots.len() - 1].width_pt;
        let width = |line: usize| slots.get(line).map_or(last_width, |s| s.width_pt);
        let breaks = match req.composer {
            Composer::FirstFit => first_fit(&items, &sums, &width, &rules, cancel)?,
            Composer::KnuthPlass(params) => knuth_plass(&items, &sums, &width, &params, &rules, cancel)?,
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
            let alignment = stylings[pi].as_ref().map_or(opts.alignment, |s| s.alignment);
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
                },
            ));
        }
        placed = placed
            .checked_add(lines_here.len())
            .filter(|n| *n <= limits.max_lines)
            .ok_or(LayoutError::BudgetExceeded)?;
        let used = lines_here.len();
        for (pos, line) in lines_here {
            frames[pos].lines.push(line);
        }
        if overset.is_some() {
            break;
        }
        cursor = slots[used - 1].next;
    }
    Ok(LayoutProposal {
        base_revision: req.document.revision,
        frames,
        overset,
    })
}
