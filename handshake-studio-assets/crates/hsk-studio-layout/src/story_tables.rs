//! A story with anchored tables (STU-LAY-035: a table is flowed inside a `StudioTextStory`): the
//! host text before each table is composed, the table continues from where that text ends, and the
//! host text after it resumes below the table (lane SMB-LAYOUT-LONGDOC-2).
//!
//! Rules (documented; InDesign parity UNVERIFIED, D5):
//! - A table's `anchor` must be the start of a shaped host paragraph or the story end
//!   (`table_anchor_not_on_paragraph` otherwise); tables at one anchor follow each other in the
//!   given order.
//! - Text resumes at the table's end position: in that frame its first baseline is the larger of
//!   the frame's fixed first baseline and the end position plus the paragraph's ascent; when that
//!   line does not fit, the text continues in the next frame of the thread as usual.
//! - Overset text, or a table that runs out of frames, makes everything after it overset (the
//!   story overset span runs to the story end; later tables are not laid out).
#![deny(clippy::arithmetic_side_effects)]
use crate::folio_input::{Frame, resolve};
use crate::styles::ParagraphStyling;
use crate::table::{FlowPosition, TableError, TableLimits, TableProposal, TableRequest, layout_table};
use crate::threaded_reflow::{FramePlacement, LayoutProposal, Line, ReflowRequest, compose};
use crate::{LayoutError, ShapedParagraph, SourceSpan, TypesetOptions};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{StudioTable, StudioTextStory};

/// One table anchored in the host story, with its cell story.
#[derive(Clone, Copy, Debug)]
pub struct AnchoredTable<'a> {
    pub table: &'a StudioTable,
    pub cell_story: Option<&'a StudioTextStory>,
    pub cell_resolved_text: Option<&'a str>,
    pub cell_paragraphs: &'a [ShapedParagraph],
}

/// The host text placement and the table proposals (in anchor order).
#[derive(Clone, Debug, PartialEq)]
pub struct StoryWithTables {
    pub text: LayoutProposal,
    pub tables: Vec<TableProposal>,
}

/// `compose` from a position: the start frame's first baseline moves down to the position.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compose_from(
    thread: &[Frame<'_>],
    start: Option<FlowPosition>,
    text: &str,
    paragraphs: &[ShapedParagraph],
    stylings: &[Option<ParagraphStyling>],
    req: &ReflowRequest<'_>,
    opts: &TypesetOptions<'_>,
    cancel: &CancellationToken,
) -> Result<(Vec<Vec<Line>>, Option<SourceSpan>), LayoutError> {
    let mut out: Vec<Vec<Line>> = thread.iter().map(|_| Vec::new()).collect();
    let Some(first) = paragraphs.first() else {
        return Ok((out, None));
    };
    let k = start.map_or(0, |s| s.frame);
    let mut sub: Vec<Frame<'_>> = thread.get(k..).ok_or(LayoutError::InvalidInput)?.to_vec();
    if let (Some(s), Some(f)) = (start, sub.first_mut()) {
        if !s.y_pt.is_finite() {
            return Err(LayoutError::InvalidInput);
        }
        f.first_baseline_pt = f.first_baseline_pt.max(s.y_pt + first.ascent_pt);
    }
    if sub.is_empty() {
        return Ok((out, Some(SourceSpan { start: first.source.start, end: first.source.start })));
    }
    let c = compose(&sub, text, paragraphs, stylings, req.composer, req.space, opts, req.limits, cancel)?;
    for (i, lines) in c.lines.into_iter().enumerate() {
        if let Some(slot) = k.checked_add(i).and_then(|j| out.get_mut(j)) {
            *slot = lines;
        }
    }
    Ok((out, c.overset))
}

/// Where composed text ends: the frame of the last line and its bottom.
fn end_of(lines: &[Vec<Line>], paragraphs: &[ShapedParagraph], offset: usize) -> Option<FlowPosition> {
    let (fi, l) = lines.iter().enumerate().rev().find_map(|(i, f)| f.last().map(|l| (i, l)))?;
    let p = paragraphs.get(l.paragraph.checked_sub(offset)?)?;
    Some(FlowPosition { frame: fi, y_pt: l.baseline_y_pt + (p.line_height_pt - p.ascent_pt) })
}

/// Reflows the host story of `req` with its anchored tables (module docs).
pub fn reflow_with_tables(
    req: &ReflowRequest<'_>,
    tables: &[AnchoredTable<'_>],
    table_limits: TableLimits,
    opts: &TypesetOptions<'_>,
    cancel: &CancellationToken,
) -> Result<StoryWithTables, TableError> {
    cancel.check().map_err(|_| LayoutError::Canceled)?;
    opts.validate()?;
    if req.expected_revision != req.document.revision {
        return Err(LayoutError::StaleRevision { expected: req.expected_revision, current: req.document.revision }.into());
    }
    let input = resolve(req.document, req.story, req.resolved_text, req.obstacles)?;
    let (text, thread) = (input.text, input.frames);
    let story_end = req.story.byte_len();
    if req.paragraphs.iter().any(|p| p.source.end > story_end) {
        return Err(LayoutError::InvalidInput.into());
    }
    let styled;
    let (paragraphs, stylings): (&[ShapedParagraph], Vec<Option<ParagraphStyling>>) = match opts.styles {
        Some(src) => {
            let (a, b) = crate::styles::apply(src, req.story, text, req.paragraphs, opts.alignment)?;
            styled = a;
            (&styled, b.into_iter().map(Some).collect())
        }
        None => (req.paragraphs, vec![None; req.paragraphs.len()]),
    };
    // Each table's paragraph index (its anchor is that paragraph's start, or the story end).
    let mut order: Vec<(usize, usize)> = Vec::with_capacity(tables.len());
    for (ti, t) in tables.iter().enumerate() {
        if t.table.story_id != req.story.story_id {
            return Err(LayoutError::InvalidInput.into());
        }
        let at = t.table.anchor;
        let idx = if at == story_end {
            paragraphs.len()
        } else {
            paragraphs.iter().position(|p| p.source.start == at).ok_or(TableError::Anchor)?
        };
        order.push((idx, ti));
    }
    order.sort();
    let mut lines: Vec<Vec<Line>> = thread.iter().map(|_| Vec::new()).collect();
    let mut placed_tables = Vec::new();
    let mut pos: Option<FlowPosition> = None;
    let mut overset: Option<SourceSpan> = None;
    let mut prev = 0usize;
    let segment = |from: usize, to: usize, pos: Option<FlowPosition>, lines: &mut Vec<Vec<Line>>| -> Result<(Option<FlowPosition>, Option<SourceSpan>), TableError> {
        let (Some(ps), Some(st)) = (paragraphs.get(from..to), stylings.get(from..to)) else {
            return Err(LayoutError::InvalidInput.into());
        };
        let (mut seg, over) = compose_from(&thread, pos, text, ps, st, req, opts, cancel)?;
        for f in &mut seg {
            for l in f.iter_mut() {
                l.paragraph = l.paragraph.checked_add(from).ok_or(LayoutError::BudgetExceeded)?;
            }
        }
        let end = end_of(&seg, paragraphs, 0).or(pos);
        for (dst, src) in lines.iter_mut().zip(seg) {
            dst.extend(src);
        }
        Ok((end, over.map(|o| SourceSpan { start: o.start, end: story_end })))
    };
    for (idx, ti) in order {
        let (end, over) = segment(prev, idx, pos, &mut lines)?;
        pos = end;
        prev = idx;
        if over.is_some() {
            overset = over;
            break;
        }
        let t = &tables[ti];
        let proposal = layout_table(
            &TableRequest {
                document: req.document,
                table: t.table,
                host_story: req.story,
                host_resolved_text: req.resolved_text,
                cell_story: t.cell_story,
                cell_resolved_text: t.cell_resolved_text,
                cell_paragraphs: t.cell_paragraphs,
                start: pos,
                composer: req.composer,
                space: req.space,
                expected_revision: req.expected_revision,
                limits: table_limits,
            },
            opts,
            cancel,
        )?;
        let table_end = proposal.end;
        let ran_out = proposal.overset_row.is_some();
        placed_tables.push(proposal);
        if ran_out || table_end.is_none() {
            overset = Some(SourceSpan { start: t.table.anchor, end: story_end });
            break;
        }
        pos = table_end;
    }
    if overset.is_none() {
        let (_, over) = segment(prev, paragraphs.len(), pos, &mut lines)?;
        overset = over;
    }
    let frames = thread
        .iter()
        .zip(lines)
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
    Ok(StoryWithTables {
        text: LayoutProposal { base_revision: req.document.revision, frames, overset },
        tables: placed_tables,
    })
}
