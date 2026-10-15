//! Endnotes and sidenotes (STU-LAY-046, STU-LAY-150; lane SMB-LAYOUT-LONGDOC-2).
//!
//! Endnotes (documented; InDesign parity UNVERIFIED, D5): references are read in source order,
//! then anchor order; numbers run from `start_at` and restart per story with `story_restart`. The
//! generated story holds the title (when not empty) and one paragraph per note: prefix + number +
//! suffix + separator text + the note text (without its trailing break). Document scope makes one
//! group; story scope one group per host story (each with the title). Reference positions come from
//! the host placements (as footnote references); references in overset text have none.
//!
//! Sidenotes (the spec names them; their option contract is a declared SPEC GAP, so the Studio
//! options are provisional): each note is composed in a column of `width` beside its frame, `gap`
//! from the frame edge, outside or inside the margin (outside = right on right-hand and single-sided
//! pages, left on left-hand pages); its top is the reference line's top or the frame's top inset;
//! notes on the same side of a frame stack with `space_between`. Numbers restart like footnotes.
#![deny(clippy::arithmetic_side_effects)]
use crate::folio_input::{Frame, single_frame, story_text};
use crate::footnotes::{FootnoteError, locate};
use crate::geometry::Rect;
use crate::numbering::{NumberingError, PageNumber, format_number};
use crate::threaded_reflow::{FramePlacement, LayoutProposal, Limits, Line, compose};
use crate::toc::{GeneratedStory, Writer, clean};
use crate::{Composer, LayoutError, ShapedParagraph, SpaceParams, TypesetOptions};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    EndnoteRestart, EndnoteScope, FootnoteRestart, PageSide, SidenoteAnchor, SidenoteSide, StudioDocument, StudioEndnote,
    StudioEndnoteOptions, StudioSidenoteOptions, StudioTextStory, check_endnotes, check_sidenote_options,
};

/// A host story, its placement and the paragraphs it was composed from.
#[derive(Clone, Copy, Debug)]
pub struct NoteHost<'a> {
    pub story: &'a StudioTextStory,
    pub placement: Option<&'a LayoutProposal>,
    pub paragraphs: &'a [ShapedParagraph],
}

/// One endnote or sidenote with its note story.
#[derive(Clone, Copy, Debug)]
pub struct NoteBody<'a> {
    pub note: &'a StudioEndnote,
    pub story: &'a StudioTextStory,
    pub resolved_text: Option<&'a str>,
    pub paragraphs: &'a [ShapedParagraph],
}

/// A numbered reference.
#[derive(Clone, Debug, PartialEq)]
pub struct NoteReference {
    pub note_id: String,
    pub number: u32,
    pub label: String,
    /// Host frame id and the reference position in it (x after the anchor's preceding glyphs,
    /// baseline); `None` when the reference is overset.
    pub position: Option<(String, [f64; 2])>,
}

/// One generated endnote story: `story_id` of its host for story scope, `None` for document scope.
#[derive(Clone, Debug, PartialEq)]
pub struct EndnoteGroup {
    pub story_id: Option<String>,
    pub story: GeneratedStory,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GeneratedEndnotes {
    pub groups: Vec<EndnoteGroup>,
    pub references: Vec<NoteReference>,
}

fn reference(host: &NoteHost<'_>, anchor: u64) -> Option<(String, [f64; 2])> {
    let placement = host.placement?;
    let (fi, at, _) = locate(placement, anchor, host.story.byte_len(), host.paragraphs)?;
    Some((placement.frames.get(fi)?.frame_id.clone(), at))
}

/// Generates the endnotes of `hosts` (module docs).
pub fn generate_endnotes(
    hosts: &[NoteHost<'_>],
    notes: &[NoteBody<'_>],
    options: &StudioEndnoteOptions,
    max_text_bytes: usize,
) -> Result<GeneratedEndnotes, FootnoteError> {
    let records: Vec<StudioEndnote> = notes.iter().map(|n| n.note.clone()).collect();
    check_endnotes(&records, Some(options)).map_err(FootnoteError::Notes)?;
    let mut order: Vec<(usize, u64, usize)> = Vec::with_capacity(notes.len());
    for (i, n) in notes.iter().enumerate() {
        let h = hosts.iter().position(|h| h.story.story_id == n.note.story_id).ok_or(LayoutError::InvalidInput)?;
        if n.note.anchor > hosts[h].story.byte_len() || n.note.note_story_id != n.story.story_id {
            return Err(LayoutError::InvalidInput.into());
        }
        order.push((h, n.note.anchor, i));
    }
    order.sort();
    let mut references = Vec::new();
    let mut groups: Vec<EndnoteGroup> = Vec::new();
    let mut writer: Option<(Option<String>, Writer)> = None;
    let mut count = 0u32;
    let mut last_host: Option<usize> = None;
    let new_writer = |story_id: Option<String>| -> Result<(Option<String>, Writer), FootnoteError> {
        let mut w = Writer::new(max_text_bytes);
        if !options.title.is_empty() {
            w.push(&options.title, options.title_style.as_deref()).map_err(|_| FootnoteError::Layout(LayoutError::BudgetExceeded))?;
        }
        Ok((story_id, w))
    };
    for (h, anchor, i) in order {
        let n = &notes[i];
        let new_story = last_host != Some(h);
        count = if last_host.is_none() || (new_story && options.restart == EndnoteRestart::StoryRestart) {
            options.start_at
        } else {
            count.checked_add(1).ok_or(FootnoteError::Numbering(NumberingError::OutOfRange))?
        };
        if options.scope == EndnoteScope::Story && new_story {
            if let Some((id, w)) = writer.take() {
                groups.push(EndnoteGroup { story_id: id, story: GeneratedStory { text: w.text, paragraphs: w.paragraphs, entries: Vec::new() } });
            }
            writer = Some(new_writer(Some(hosts[h].story.story_id.clone()))?);
        } else if writer.is_none() {
            writer = Some(new_writer(None)?);
        }
        last_host = Some(h);
        let label = format!("{}{}{}", options.prefix, format_number(count, options.numbering_style).map_err(FootnoteError::Numbering)?, options.suffix);
        let body = story_text(n.story, n.resolved_text)?;
        let line = format!("{label}{}{}", options.separator_text, clean(body));
        if let Some((_, w)) = writer.as_mut() {
            w.push(&line, options.text_style.as_deref()).map_err(|_| FootnoteError::Layout(LayoutError::BudgetExceeded))?;
        }
        references.push(NoteReference {
            note_id: n.note.note_id.clone(),
            number: count,
            label,
            position: reference(&hosts[h], anchor),
        });
    }
    if let Some((id, w)) = writer.take() {
        groups.push(EndnoteGroup { story_id: id, story: GeneratedStory { text: w.text, paragraphs: w.paragraphs, entries: Vec::new() } });
    }
    Ok(GeneratedEndnotes { groups, references })
}

/// One placed sidenote.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedSidenote {
    pub reference: NoteReference,
    /// Host frame id; `rect` is in that frame's coordinates (it lies outside the frame).
    pub frame_id: String,
    pub rect: Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SidenoteProposal {
    pub notes: Vec<PlacedSidenote>,
    /// Per note (same order): its note story id and lines in the host frame's coordinates.
    pub text: Vec<(String, LayoutProposal)>,
    /// Notes whose reference is overset (not placed).
    pub overset: Vec<String>,
}

/// Places sidenotes beside their reference frames (module docs).
#[allow(clippy::too_many_arguments)]
pub fn place_sidenotes(
    document: &StudioDocument,
    host: &NoteHost<'_>,
    notes: &[NoteBody<'_>],
    options: &StudioSidenoteOptions,
    pages: &[PageNumber],
    composer: Composer,
    opts: &TypesetOptions<'_>,
    limits: Limits,
    cancel: &CancellationToken,
) -> Result<SidenoteProposal, FootnoteError> {
    let records: Vec<StudioEndnote> = notes.iter().map(|n| n.note.clone()).collect();
    check_endnotes(&records, None).map_err(FootnoteError::Notes)?;
    check_sidenote_options(options).map_err(FootnoteError::Notes)?;
    let placement = host.placement.ok_or(LayoutError::InvalidInput)?;
    if placement.base_revision != document.revision {
        return Err(LayoutError::StaleRevision { expected: placement.base_revision, current: document.revision }.into());
    }
    let mut order: Vec<usize> = (0..notes.len()).collect();
    for n in notes {
        if n.note.story_id != host.story.story_id || n.note.anchor > host.story.byte_len() || n.note.note_story_id != n.story.story_id {
            return Err(LayoutError::InvalidInput.into());
        }
    }
    order.sort_by_key(|&i| (notes[i].note.anchor, i));
    let (width, gap, between) = (options.width.value, options.gap.value, options.space_between.value);
    let mut out = SidenoteProposal { notes: Vec::new(), text: Vec::new(), overset: Vec::new() };
    // Bottom of the last sidenote per (frame, right side).
    let mut stacks: Vec<(String, bool, f64)> = Vec::new();
    let mut count = 0u32;
    let mut previous: Option<(usize, usize, Option<usize>)> = None;
    for i in order {
        cancel.check().map_err(|_| LayoutError::Canceled)?;
        let n = &notes[i];
        let Some((fi, at, _)) = locate(placement, n.note.anchor, host.story.byte_len(), host.paragraphs) else {
            out.overset.push(n.note.note_id.clone());
            continue;
        };
        let fp = &placement.frames[fi];
        let (frame, side): (Frame<'_>, PageSide) = single_frame(document, &fp.frame_id)?;
        let section = pages.iter().find(|p| p.spread == fp.spread && p.page == fp.page).and_then(|p| p.section);
        let key = (fp.spread, fp.page, section);
        let restart = match (options.restart, previous) {
            (_, None) => true,
            (FootnoteRestart::DontRestart, _) => false,
            (FootnoteRestart::Page, Some(p)) => (p.0, p.1) != (key.0, key.1),
            (FootnoteRestart::Spread, Some(p)) => p.0 != key.0,
            (FootnoteRestart::Section, Some(p)) => p.2 != key.2,
        };
        count = if restart { options.start_at } else { count.checked_add(1).ok_or(FootnoteError::Numbering(NumberingError::OutOfRange))? };
        previous = Some(key);
        let label = format_number(count, options.numbering_style).map_err(FootnoteError::Numbering)?;
        let outside_right = side != PageSide::LeftHand;
        let right = match options.side {
            SidenoteSide::Outside => outside_right,
            SidenoteSide::Inside => !outside_right,
        };
        let x0 = if right { frame.width_pt + gap } else { -gap - width };
        let line_top = fp
            .lines
            .iter()
            .find(|l| (l.baseline_y_pt - at[1]).abs() < 1e-9)
            .and_then(|l| host.paragraphs.get(l.paragraph))
            .map_or(at[1], |p| at[1] - p.ascent_pt);
        let mut top = match options.anchor {
            SidenoteAnchor::ReferenceLine => line_top,
            SidenoteAnchor::FrameTop => frame.inset_top_pt,
        };
        if let Some(s) = stacks.iter().find(|s| s.0 == fp.frame_id && s.1 == right) {
            top = top.max(s.2 + between);
        }
        // Compose the note in its column.
        let text = story_text(n.story, n.resolved_text)?;
        let Some(first) = n.paragraphs.first() else {
            out.overset.push(n.note.note_id.clone());
            continue;
        };
        let column = Frame {
            id: frame.id,
            layer_index: frame.layer_index,
            spread: frame.spread,
            page: frame.page,
            origin_x_pt: 0.0,
            origin_y_pt: 0.0,
            width_pt: width,
            height_pt: 1.0e6,
            inset_top_pt: 0.0,
            inset_left_pt: 0.0,
            inset_right_pt: 0.0,
            inset_bottom_pt: 0.0,
            first_baseline_pt: first.ascent_pt,
            obstacles: Vec::new(),
        };
        let stylings = vec![None; n.paragraphs.len()];
        let c = compose(std::slice::from_ref(&column), text, n.paragraphs, &stylings, composer, SpaceParams::default(), opts, limits, cancel)?;
        let lines: Vec<Line> = c
            .lines
            .into_iter()
            .next()
            .unwrap_or_default()
            .into_iter()
            .map(|mut l| {
                l.baseline_y_pt += top;
                l.x_start_pt += x0;
                l
            })
            .collect();
        let bottom = lines
            .last()
            .and_then(|l| n.paragraphs.get(l.paragraph).map(|p| l.baseline_y_pt + (p.line_height_pt - p.ascent_pt)))
            .unwrap_or(top);
        match stacks.iter_mut().find(|s| s.0 == fp.frame_id && s.1 == right) {
            Some(s) => s.2 = bottom,
            None => stacks.push((fp.frame_id.clone(), right, bottom)),
        }
        out.notes.push(PlacedSidenote {
            reference: NoteReference { note_id: n.note.note_id.clone(), number: count, label, position: Some((fp.frame_id.clone(), at)) },
            frame_id: fp.frame_id.clone(),
            rect: Rect { x0_pt: x0, y0_pt: top, x1_pt: x0 + width, y1_pt: bottom },
        });
        out.text.push((
            n.story.story_id.clone(),
            LayoutProposal {
                base_revision: document.revision,
                frames: vec![FramePlacement {
                    frame_id: fp.frame_id.clone(),
                    frame_index: fp.frame_index,
                    spread: fp.spread,
                    page: fp.page,
                    origin_x_pt: fp.origin_x_pt,
                    origin_y_pt: fp.origin_y_pt,
                    lines,
                }],
                overset: c.overset,
            },
        ));
    }
    Ok(out)
}
