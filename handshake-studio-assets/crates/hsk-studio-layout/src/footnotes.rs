//! Footnotes (STU-LAY-046, STU-LAY-150): the host story is reflowed with a footnote band reserved
//! at the bottom of each frame (a full-width text-wrap obstacle, so body lines that would reach
//! into the band move on), the band holds the notes whose references are in that frame, and the
//! reflow repeats until the bands stop changing.
//!
//! Rules (documented; InDesign parity UNVERIFIED, D5):
//! - A reference sits at a host byte; its frame is the frame of the line holding that byte (the
//!   story end belongs to the last line). Notes are numbered in flow order from `start_at` in the
//!   options' style with prefix + number + suffix; the count restarts on a new page, spread or
//!   section (`restart`). References in overset text are not placed or numbered
//!   (`overset_notes`).
//! - Each note is composed into the frame's inset width (reflow core, `compose`); notes stack in
//!   reference order with `space_between` between them; the band's bottom is the frame's bottom
//!   inset and its height is the stacked notes plus `spacer`. With `eos_placement` the band of the
//!   frame where the story ends sits just below its last line instead.
//! - A band may not rise above the lowest reference line of its frame plus `spacer`. A note that
//!   does not fit splits at a line (the rest continues at the top of the next frame's band, before
//!   that frame's own notes, `continued`); with `no_splitting` the whole note moves on. Text that
//!   no frame can take is overset.
//! - Rules: the first rule above a band that starts with a new note, the continuing rule above a
//!   band that starts with a continued note; at `offset` above the band top, from the left inset +
//!   `left_indent`, `width` long.
//! - The paragraph style space before/after of notes is not applied (layout has none).
//!
//! Hostile input: footnotes in footnotes (`footnote_in_footnote`, Folio `check_footnotes`),
//! option bounds (`check_footnote_options`), anchors outside the host story, more than
//! `MAX_FOOTNOTES` notes (refused before anything is composed), and bands that never settle within
//! `max_iterations` (`footnote_layout_unstable`) are typed refusals.
#![deny(clippy::arithmetic_side_effects)]
use crate::folio_input::{Frame, resolve, story_text};
use crate::geometry::Rect;
use crate::numbering::{NumberingError, PageNumber, format_number};
use crate::styles::ParagraphStyling;
use crate::threaded_reflow::{FramePlacement, LayoutProposal, Line, ReflowRequest, compose, reflow_typeset};
use crate::{FrameObstacles, LayoutError, ShapedParagraph, TypesetOptions};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    FootnoteRestart, MAX_FOOTNOTES, StudioColor, StudioFootnote, StudioFootnoteOptions,
    StudioFootnoteRule, StudioTextStory, check_footnote_options, check_footnotes,
};

const EPS: f64 = 1e-9;
/// A note part in a band: note, first line, end line, y offset inside the band, continued.
type NotePart = (usize, usize, usize, f64, bool);
const NOTE_HEIGHT_CAP_PT: f64 = 1.0e6;

/// One footnote with its note story and the note story's shaped paragraphs.
#[derive(Clone, Copy, Debug)]
pub struct NoteInput<'a> {
    pub note: &'a StudioFootnote,
    pub story: &'a StudioTextStory,
    /// Resolved bytes of a stored note story.
    pub resolved_text: Option<&'a str>,
    pub paragraphs: &'a [ShapedParagraph],
}

#[derive(Clone, Copy, Debug)]
pub struct FootnoteRequest<'a> {
    /// The host story's reflow (its obstacles are kept and the bands are added).
    pub reflow: ReflowRequest<'a>,
    pub notes: &'a [NoteInput<'a>],
    pub options: &'a StudioFootnoteOptions,
    /// `number_pages` output (section restart); may be empty when `restart` is not `section`.
    pub pages: &'a [PageNumber],
    /// Reflow passes before `footnote_layout_unstable` (at least 1).
    pub max_iterations: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FootnoteError {
    Layout(LayoutError),
    /// Folio footnote or option rule (`footnote_in_footnote`, `footnote_start_at_range`, ...).
    Notes(&'static str),
    Numbering(NumberingError),
    Unstable,
}
impl FootnoteError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Layout(e) => e.code(),
            Self::Notes(w) => w,
            Self::Numbering(e) => e.code(),
            Self::Unstable => "footnote_layout_unstable",
        }
    }
}
impl std::fmt::Display for FootnoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for FootnoteError {}
impl From<LayoutError> for FootnoteError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}

/// A placed footnote.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedNote {
    pub note_id: String,
    pub number: u32,
    pub label: String,
    /// Thread position of the frame holding the reference.
    pub reference_frame: usize,
    /// Reference position in that frame: x after the glyphs before the anchor, the line baseline.
    pub reference: [f64; 2],
    /// Thread positions of the frames holding the note text (more than one when continued).
    pub text_frames: Vec<usize>,
}

/// A separator rule in frame coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct RuleSegment {
    pub thread_index: usize,
    pub frame_id: String,
    pub spread: usize,
    pub page: usize,
    pub origin_x_pt: f64,
    pub origin_y_pt: f64,
    pub from: [f64; 2],
    pub to: [f64; 2],
    pub weight_pt: f64,
    pub colour: StudioColor,
    pub tint: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FootnoteProposal {
    /// The host story with the bands reserved.
    pub body: LayoutProposal,
    /// Placed notes in number order.
    pub notes: Vec<PlacedNote>,
    /// Per note story (same order as `notes`): its lines in the host frames (frame coordinates).
    pub note_text: Vec<(String, LayoutProposal)>,
    pub rules: Vec<RuleSegment>,
    /// Notes whose reference is overset or whose text found no band.
    pub overset_notes: Vec<String>,
    /// Band height per thread frame.
    pub bands: Vec<f64>,
    pub iterations: usize,
}

/// A note composed at one width: lines relative to its own top.
struct Composed {
    lines: Vec<Line>,
}
fn line_box(l: &Line, paragraphs: &[ShapedParagraph]) -> (f64, f64) {
    let p = &paragraphs[l.paragraph];
    (
        l.baseline_y_pt - p.ascent_pt,
        l.baseline_y_pt + (p.line_height_pt - p.ascent_pt),
    )
}

fn compose_note(
    n: &NoteInput<'_>,
    f: &Frame<'_>,
    req: &FootnoteRequest<'_>,
    opts: &TypesetOptions<'_>,
    cancel: &CancellationToken,
) -> Result<(Composed, Vec<ShapedParagraph>), FootnoteError> {
    let text = story_text(n.story, n.resolved_text)?;
    let styled;
    let (paragraphs, stylings): (&[ShapedParagraph], Vec<Option<ParagraphStyling>>) = match opts.styles {
        Some(src) => {
            let (a, b) = crate::styles::apply(src, n.story, n.paragraphs, opts.alignment)?;
            styled = a;
            (&styled, b.into_iter().map(Some).collect())
        }
        None => (n.paragraphs, vec![None; n.paragraphs.len()]),
    };
    let Some(first) = paragraphs.first() else {
        return Ok((Composed { lines: Vec::new() }, Vec::new()));
    };
    let column = Frame {
        id: f.id,
        layer_index: f.layer_index,
        spread: f.spread,
        page: f.page,
        origin_x_pt: 0.0,
        origin_y_pt: 0.0,
        width_pt: f.width_pt,
        height_pt: NOTE_HEIGHT_CAP_PT,
        inset_top_pt: 0.0,
        inset_left_pt: f.inset_left_pt,
        inset_right_pt: f.inset_right_pt,
        inset_bottom_pt: 0.0,
        first_baseline_pt: first.ascent_pt,
        obstacles: Vec::new(),
    };
    let c = compose(
        std::slice::from_ref(&column),
        text,
        paragraphs,
        &stylings,
        req.reflow.composer,
        req.reflow.space,
        opts,
        req.reflow.limits,
        cancel,
    )?;
    if c.overset.is_some() {
        return Err(LayoutError::BudgetExceeded.into());
    }
    Ok((
        Composed {
            lines: c.lines.into_iter().next().unwrap_or_default(),
        },
        paragraphs.to_vec(),
    ))
}

/// Where a reference is: thread index, line, reference x.
fn locate(body: &LayoutProposal, anchor: u64, story_end: u64, paragraphs: &[ShapedParagraph]) -> Option<(usize, [f64; 2], f64)> {
    let mut last: Option<(usize, &Line)> = None;
    for (fi, f) in body.frames.iter().enumerate() {
        for l in &f.lines {
            let hit = l.source.start <= anchor && anchor < l.source.end;
            if hit {
                let p = paragraphs.get(l.paragraph)?;
                let x = l.x_start_pt
                    + p.glyphs
                        .get(l.glyph_range.clone())?
                        .iter()
                        .take_while(|g| g.cluster.start < anchor)
                        .map(|g| g.advance_pt)
                        .sum::<f64>();
                let bottom = line_box(l, paragraphs).1;
                return Some((fi, [x, l.baseline_y_pt], bottom));
            }
            last = Some((fi, l));
        }
    }
    // The story end belongs to the last line (when the story is not overset).
    match last {
        Some((fi, l)) if anchor == story_end && body.overset.is_none() => {
            let bottom = line_box(l, paragraphs).1;
            Some((fi, [l.x_start_pt + l.width_pt, l.baseline_y_pt], bottom))
        }
        _ => None,
    }
}

/// Lays out the host story with its footnotes (module docs).
pub fn layout_footnotes(
    req: &FootnoteRequest<'_>,
    opts: &TypesetOptions<'_>,
    cancel: &CancellationToken,
) -> Result<FootnoteProposal, FootnoteError> {
    cancel.check().map_err(|_| LayoutError::Canceled)?;
    if req.notes.len() > MAX_FOOTNOTES {
        return Err(FootnoteError::Notes("too_many_footnotes"));
    }
    let records: Vec<StudioFootnote> = req.notes.iter().map(|n| n.note.clone()).collect();
    check_footnotes(&records).map_err(FootnoteError::Notes)?;
    check_footnote_options(req.options).map_err(FootnoteError::Notes)?;
    let host = req.reflow.story;
    let story_end = host.byte_len();
    for n in req.notes {
        if n.note.story_id != host.story_id || n.note.note_story_id != n.story.story_id || n.note.anchor > story_end {
            return Err(LayoutError::InvalidInput.into());
        }
    }
    if req.max_iterations == 0 {
        return Err(LayoutError::InvalidInput.into());
    }
    let input = resolve(req.reflow.document, host, req.reflow.resolved_text, req.reflow.obstacles)?;
    let frames = input.frames;
    let o = req.options;
    let pt = |l: &hsk_studio_folio::Length| l.value;
    let (space_between, spacer) = (pt(&o.space_between), pt(&o.spacer));
    // Notes in anchor order; each composed once per frame width (cached).
    let mut order: Vec<usize> = (0..req.notes.len()).collect();
    order.sort_by_key(|&i| (req.notes[i].note.anchor, i));
    let mut cache: Vec<Vec<(u64, Composed, Vec<ShapedParagraph>)>> = (0..req.notes.len()).map(|_| Vec::new()).collect();
    let mut reserve = vec![0.0f64; frames.len()];
    for iteration in 1..=req.max_iterations {
        cancel.check().map_err(|_| LayoutError::Canceled)?;
        let mut obstacles: Vec<FrameObstacles> = req.reflow.obstacles.to_vec();
        for (f, r) in frames.iter().zip(&reserve) {
            if *r > EPS {
                let bottom = f.height_pt - f.inset_bottom_pt;
                obstacles.push(FrameObstacles {
                    frame_id: f.id.to_owned(),
                    rects: vec![Rect {
                        x0_pt: 0.0,
                        y0_pt: (bottom - r).max(0.0),
                        x1_pt: f.width_pt,
                        y1_pt: f.height_pt,
                    }],
                });
            }
        }
        let body = reflow_typeset(&ReflowRequest { obstacles: &obstacles, ..req.reflow }, opts, cancel)?;
        // References.
        let mut anchored: Vec<Vec<usize>> = vec![Vec::new(); frames.len()];
        let mut floor = vec![f64::NEG_INFINITY; frames.len()];
        let mut refs: Vec<Option<(usize, [f64; 2])>> = vec![None; req.notes.len()];
        for &i in &order {
            if let Some((fi, at, bottom)) = locate(&body, req.notes[i].note.anchor, story_end, req.reflow.paragraphs) {
                anchored[fi].push(i);
                floor[fi] = floor[fi].max(bottom);
                refs[i] = Some((fi, at));
            }
        }
        // Bands.
        let mut carry: Vec<(usize, usize)> = Vec::new();
        let mut new_reserve = vec![0.0f64; frames.len()];
        // Per frame: (note, first line, last line, y offset of the part inside the band, continued).
        let mut parts: Vec<Vec<NotePart>> = vec![Vec::new(); frames.len()];
        let mut used_h = vec![0.0f64; frames.len()];
        for (fi, f) in frames.iter().enumerate() {
            let bottom = f.height_pt - f.inset_bottom_pt;
            let top_limit = if floor[fi].is_finite() { floor[fi] } else { f.inset_top_pt };
            let limit = bottom - top_limit - spacer;
            let width_key = f.width_pt.to_bits() ^ f.inset_left_pt.to_bits().rotate_left(1) ^ f.inset_right_pt.to_bits().rotate_left(2);
            let items: Vec<(usize, usize, bool)> = carry
                .drain(..)
                .map(|(n, from)| (n, from, true))
                .chain(anchored[fi].iter().map(|&n| (n, 0, false)))
                .collect();
            let mut used = 0.0f64;
            for (n, from, continued) in items {
                if !cache[n].iter().any(|(k, ..)| *k == width_key) {
                    let (c, p) = compose_note(&req.notes[n], f, req, opts, cancel)?;
                    cache[n].push((width_key, c, p));
                }
                let Some((_, composed, paragraphs)) = cache[n].iter().find(|(k, ..)| *k == width_key) else {
                    return Err(LayoutError::InvalidInput.into());
                };
                let lines = &composed.lines;
                let gap = if used > EPS { space_between } else { 0.0 };
                let offset = lines.get(from).map_or(0.0, |l| line_box(l, paragraphs).0.max(0.0));
                let mut k = from;
                let mut height = 0.0f64;
                while let Some(l) = lines.get(k) {
                    let b = line_box(l, paragraphs).1 - offset;
                    if used + gap + b > limit + EPS {
                        break;
                    }
                    height = b;
                    k = k.saturating_add(1);
                }
                let all = k >= lines.len();
                if k == from || (!all && o.no_splitting) {
                    carry.push((n, from));
                    continue;
                }
                parts[fi].push((n, from, k, used + gap, continued));
                used += gap + height;
                if !all {
                    carry.push((n, k));
                }
            }
            used_h[fi] = used;
            new_reserve[fi] = if used > EPS { used + spacer } else { 0.0 };
        }
        let stable = new_reserve.iter().zip(&reserve).all(|(a, b)| (a - b).abs() <= 1e-6);
        if !stable {
            reserve = new_reserve;
            continue;
        }
        // Settled: number the placed notes and build the output.
        let placed: Vec<usize> = order.iter().copied().filter(|&i| refs[i].is_some()).collect();
        let mut numbers = vec![0u32; req.notes.len()];
        let mut previous: Option<(usize, usize, Option<usize>)> = None;
        let mut count: u32 = 0;
        for &i in &placed {
            let Some((fi, _)) = refs[i] else { continue };
            let f = &frames[fi];
            let section = req
                .pages
                .iter()
                .find(|p| p.spread == f.spread && p.page == f.page)
                .and_then(|p| p.section);
            let key = (f.spread, f.page, section);
            let restart = match (o.restart, previous) {
                (_, None) => true,
                (FootnoteRestart::DontRestart, _) => false,
                (FootnoteRestart::Page, Some(p)) => (p.0, p.1) != (key.0, key.1),
                (FootnoteRestart::Spread, Some(p)) => p.0 != key.0,
                (FootnoteRestart::Section, Some(p)) => p.2 != key.2,
            };
            count = if restart { o.start_at } else { count.checked_add(1).ok_or(FootnoteError::Numbering(NumberingError::OutOfRange))? };
            numbers[i] = count;
            previous = Some(key);
        }
        let mut notes = Vec::new();
        let mut note_text = Vec::new();
        let mut rules = Vec::new();
        let mut overset_notes: Vec<String> = Vec::new();
        let story_end_frame = body
            .frames
            .iter()
            .rposition(|f| !f.lines.is_empty())
            .filter(|_| body.overset.is_none());
        for &i in &placed {
            let Some((fi, at)) = refs[i] else { continue };
            let n = &req.notes[i];
            let label = format!(
                "{}{}{}",
                o.prefix,
                format_number(numbers[i], o.numbering_style).map_err(FootnoteError::Numbering)?,
                o.suffix
            );
            let mut text_frames = Vec::new();
            let mut fps: Vec<FramePlacement> = Vec::new();
            for (k, f) in frames.iter().enumerate() {
                for &(note, from, to, y_in_band, _) in &parts[k] {
                    if note != i {
                        continue;
                    }
                    let bottom = f.height_pt - f.inset_bottom_pt;
                    let band_top = match story_end_frame {
                        Some(e) if o.eos_placement && e == k => body.frames[k]
                            .lines
                            .last()
                            .map_or(bottom - used_h[k], |l| line_box(l, req.reflow.paragraphs).1 + spacer),
                        _ => bottom - used_h[k],
                    };
                    let width_key = f.width_pt.to_bits() ^ f.inset_left_pt.to_bits().rotate_left(1) ^ f.inset_right_pt.to_bits().rotate_left(2);
                    let Some((_, composed, paragraphs)) = cache[i].iter().find(|(key, ..)| *key == width_key) else { continue };
                    let offset = composed.lines.get(from).map_or(0.0, |l| line_box(l, paragraphs).0.max(0.0));
                    let lines: Vec<Line> = composed.lines[from..to]
                        .iter()
                        .map(|l| {
                            let mut l = l.clone();
                            l.baseline_y_pt += band_top + y_in_band - offset;
                            l
                        })
                        .collect();
                    text_frames.push(k);
                    fps.push(FramePlacement {
                        frame_id: f.id.to_owned(),
                        frame_index: f.layer_index,
                        spread: f.spread,
                        page: f.page,
                        origin_x_pt: f.origin_x_pt,
                        origin_y_pt: f.origin_y_pt,
                        lines,
                    });
                }
            }
            if text_frames.is_empty() {
                overset_notes.push(n.note.note_id.clone());
            }
            notes.push(PlacedNote {
                note_id: n.note.note_id.clone(),
                number: numbers[i],
                label,
                reference_frame: fi,
                reference: at,
                text_frames,
            });
            note_text.push((
                n.story.story_id.clone(),
                LayoutProposal {
                    base_revision: body.base_revision,
                    frames: fps,
                    overset: None,
                },
            ));
        }
        for &i in &order {
            if refs[i].is_none() {
                overset_notes.push(req.notes[i].note.note_id.clone());
            }
        }
        for (k, f) in frames.iter().enumerate() {
            let Some(&(_, _, _, _, continued)) = parts[k].first() else { continue };
            let rule: Option<&StudioFootnoteRule> = if continued { o.continuing_rule.as_ref() } else { o.rule.as_ref() };
            let Some(r) = rule else { continue };
            let bottom = f.height_pt - f.inset_bottom_pt;
            let band_top = match story_end_frame {
                Some(e) if o.eos_placement && e == k => body.frames[k]
                    .lines
                    .last()
                    .map_or(bottom - used_h[k], |l| line_box(l, req.reflow.paragraphs).1 + spacer),
                _ => bottom - used_h[k],
            };
            let y = band_top - r.offset.value;
            let x0 = f.inset_left_pt + r.left_indent.value;
            rules.push(RuleSegment {
                thread_index: k,
                frame_id: f.id.to_owned(),
                spread: f.spread,
                page: f.page,
                origin_x_pt: f.origin_x_pt,
                origin_y_pt: f.origin_y_pt,
                from: [x0, y],
                to: [x0 + r.width.value, y],
                weight_pt: r.weight.value,
                colour: r.colour.clone(),
                tint: r.tint,
            });
        }
        return Ok(FootnoteProposal {
            body,
            notes,
            note_text,
            rules,
            overset_notes,
            bands: reserve,
            iterations: iteration,
        });
    }
    Err(FootnoteError::Unstable)
}
