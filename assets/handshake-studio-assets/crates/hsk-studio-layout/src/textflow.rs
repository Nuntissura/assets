//! Story text edits and conditional text (lane SMB-LAYOUT-UI).
//!
//! `replace_story_text` is the one text-edit primitive (story editor, cross-reference updates): it
//! replaces a byte range of an inline story and remaps the paragraph and character partitions
//! (positions before the edit stay, positions after it shift, the range collapses to its start, so
//! inserted text joins the range that starts at the edit). Paragraphs touched by the edit are
//! merged and re-split after every `\n` (typing a newline makes a paragraph, deleting one merges
//! two; the first touched paragraph's style and direction win). `shift_flow_records` moves every
//! story byte the Folio layout-flow records hold (anchors, text anchors, cross-reference ranges,
//! conditional ranges, paragraph overrides) the same way.
//!
//! Conditional text (STU-LAY-049/153): visibility resolves BEFORE composition. A byte is hidden when
//! at least one conditional range covers it and none of the covering ranges carries a visible
//! condition (text with any visible condition shows; InDesign parity UNVERIFIED, D5).
//! `conditional_view` returns the visible story (inline content, partitions remapped, empty
//! partitions dropped) and the byte map back to the original story.
#![deny(clippy::arithmetic_side_effects)]
use crate::LayoutError;
use crate::folio_input::story_text;
use hsk_studio_folio::{
    StoryContent, StoryParagraph, StoryRange, StudioLayoutFlowRecords, StudioTextStory,
    StudioXrefDestination,
};

/// Largest story the edit primitives accept, bytes (checked before any allocation by length).
pub const MAX_STORY_BYTES: usize = 64 * 1024 * 1024;

fn usize_of(v: u64) -> Result<usize, LayoutError> {
    usize::try_from(v).map_err(|_| LayoutError::InvalidInput)
}

/// Maps an old position through replacing `[start, end)` by `new_len` bytes.
fn map_pos(pos: u64, start: u64, end: u64, new_len: u64) -> Result<u64, LayoutError> {
    if pos < start {
        Ok(pos)
    } else if pos >= end {
        pos.checked_sub(end)
            .and_then(|d| d.checked_add(start))
            .and_then(|v| v.checked_add(new_len))
            .ok_or(LayoutError::BudgetExceeded)
    } else {
        Ok(start)
    }
}

/// The edited story (inline content) and its new text. `[start, end)` must lie on char boundaries
/// of the story text; `resolved_text` as for `reflow` (stored content).
pub fn replace_story_text(
    story: &StudioTextStory,
    resolved_text: Option<&str>,
    start: u64,
    end: u64,
    new_text: &str,
) -> Result<StudioTextStory, LayoutError> {
    let text = story_text(story, resolved_text)?;
    let (s, e) = (usize_of(start)?, usize_of(end)?);
    if s > e || e > text.len() || !text.is_char_boundary(s) || !text.is_char_boundary(e) {
        return Err(LayoutError::InvalidInput);
    }
    let new_size = text
        .len()
        .checked_sub(e.checked_sub(s).ok_or(LayoutError::InvalidInput)?)
        .and_then(|n| n.checked_add(new_text.len()))
        .ok_or(LayoutError::BudgetExceeded)?;
    if new_size > MAX_STORY_BYTES {
        return Err(LayoutError::BudgetExceeded);
    }
    let mut out = String::with_capacity(new_size);
    out.push_str(&text[..s]);
    out.push_str(new_text);
    out.push_str(&text[e..]);
    let n = new_text.len() as u64;
    let edit_end = start.checked_add(n).ok_or(LayoutError::BudgetExceeded)?;
    // Paragraphs: map, then merge the ones the edit touches and re-split them at newlines.
    let mut mapped = Vec::with_capacity(story.paragraphs.len());
    for p in &story.paragraphs {
        let (a, b) = (map_pos(p.start, start, end, n)?, map_pos(p.end, start, end, n)?);
        mapped.push(StoryParagraph { start: a, end: b, ..p.clone() });
    }
    // A zero-length story (or all paragraphs collapsed) still needs one paragraph.
    let touched: Vec<usize> = mapped
        .iter()
        .enumerate()
        .filter(|(_, p)| p.start <= edit_end && p.end >= start)
        .map(|(i, _)| i)
        .collect();
    let mut paragraphs = Vec::with_capacity(mapped.len().saturating_add(4));
    let (first, last) = (touched.first().copied(), touched.last().copied());
    for (i, p) in mapped.iter().enumerate() {
        match (first, last) {
            (Some(f), Some(l)) if i == f => {
                let region_end = mapped[l].end;
                let template = p.clone();
                let mut at = p.start;
                let bytes = out.as_bytes();
                let mut k = usize_of(p.start)?;
                let stop = usize_of(region_end)?;
                while k < stop {
                    if bytes[k] == b'\n' {
                        let cut = (k as u64).checked_add(1).ok_or(LayoutError::BudgetExceeded)?;
                        if cut < region_end {
                            paragraphs.push(StoryParagraph { start: at, end: cut, ..template.clone() });
                            at = cut;
                        }
                    }
                    k = k.checked_add(1).ok_or(LayoutError::BudgetExceeded)?;
                }
                paragraphs.push(StoryParagraph { start: at, end: region_end, ..template });
            }
            (Some(f), Some(l)) if i > f && i <= l => {}
            _ => paragraphs.push(p.clone()),
        }
    }
    paragraphs.retain(|p| p.start < p.end || out.is_empty());
    if paragraphs.is_empty() {
        paragraphs.push(StoryParagraph {
            start: 0,
            end: out.len() as u64,
            style_ref: story.paragraphs.first().and_then(|p| p.style_ref.clone()),
            paragraph_direction: None,
        });
    }
    let mut character_ranges = Vec::with_capacity(story.character_ranges.len());
    for r in &story.character_ranges {
        let (a, b) = (map_pos(r.start, start, end, n)?, map_pos(r.end, start, end, n)?);
        if a < b {
            character_ranges.push(StoryRange { start: a, end: b, ..r.clone() });
        }
    }
    if character_ranges.is_empty() && !out.is_empty() {
        let template = story.character_ranges.first().cloned();
        character_ranges.push(StoryRange {
            start: 0,
            end: out.len() as u64,
            style_ref: template.as_ref().and_then(|r| r.style_ref.clone()),
            applied_language: template.and_then(|r| r.applied_language),
        });
    }
    Ok(StudioTextStory {
        content: StoryContent::Inline { text_utf8: out },
        paragraphs,
        character_ranges,
        ..story.clone()
    })
}

/// Moves every byte of `story_id` that the flow records hold through the edit `[start, end)` ->
/// `new_len` bytes (see `replace_story_text`). Conditional ranges that collapse are dropped.
pub fn shift_flow_records(
    records: &mut StudioLayoutFlowRecords,
    story_id: &str,
    start: u64,
    end: u64,
    new_len: u64,
) -> Result<(), LayoutError> {
    if start > end {
        return Err(LayoutError::InvalidInput);
    }
    let m = |p: u64| map_pos(p, start, end, new_len);
    for a in records.anchored_objects.iter_mut().filter(|a| a.story_id == story_id) {
        a.anchor_byte = m(a.anchor_byte)?;
    }
    for a in records.text_anchors.iter_mut().filter(|a| a.story_id == story_id) {
        a.byte = m(a.byte)?;
    }
    for x in &mut records.cross_references {
        if x.source_story_id == story_id {
            // The reference text itself is replaced when the edit is the reference range.
            let whole = x.source_start == start && x.source_end == end;
            x.source_start = m(x.source_start)?;
            x.source_end = if whole {
                start.checked_add(new_len).ok_or(LayoutError::BudgetExceeded)?
            } else {
                m(x.source_end)?
            };
        }
        if let StudioXrefDestination::Paragraph { story_id: s, paragraph_start } = &mut x.destination
            && s == story_id
        {
            *paragraph_start = m(*paragraph_start)?;
        }
    }
    for g in records.conditional_ranges.iter_mut().filter(|g| g.story_id == story_id) {
        g.start = m(g.start)?;
        g.end = m(g.end)?;
    }
    records.conditional_ranges.retain(|g| g.start < g.end);
    for p in records.paragraph_flows.iter_mut().filter(|p| p.story_id == story_id) {
        p.paragraph_start = m(p.paragraph_start)?;
    }
    Ok(())
}

/// The visible part of a story under the current condition visibility.
#[derive(Clone, Debug, PartialEq)]
pub struct ConditionalView {
    /// Inline story holding only visible text (same id, partitions remapped).
    pub story: StudioTextStory,
    /// Kept segments: (original start, original end, view start), ascending.
    pub kept: Vec<(u64, u64, u64)>,
    /// Hidden original ranges, ascending, disjoint.
    pub hidden: Vec<(u64, u64)>,
}
impl ConditionalView {
    /// View byte of an original byte; `None` when it is hidden. The story end maps to the view end.
    pub fn to_view(&self, byte: u64) -> Option<u64> {
        for &(a, b, v) in &self.kept {
            if byte >= a && byte < b {
                return byte.checked_sub(a).and_then(|d| v.checked_add(d));
            }
        }
        let end = self.kept.last().map_or(0, |k| k.1);
        if self.hidden.iter().all(|h| h.1 <= byte) && byte >= end {
            let view_end = self.kept.last().map_or(0, |&(a, b, v)| v.saturating_add(b.saturating_sub(a)));
            return Some(view_end);
        }
        None
    }
    /// The flow records with this story's bytes moved into the view: anchored objects, text
    /// anchors and paragraph overrides in hidden text are dropped (hidden anchors hide their
    /// objects), cross-references whose source range is hidden are dropped, this story's
    /// conditional ranges are removed (already resolved).
    pub fn remap_records(&self, records: &StudioLayoutFlowRecords) -> StudioLayoutFlowRecords {
        let id = self.story.story_id.as_str();
        let mut r = records.clone();
        r.anchored_objects.retain_mut(|a| {
            a.story_id != id || self.to_view(a.anchor_byte).map(|v| a.anchor_byte = v).is_some()
        });
        r.text_anchors
            .retain_mut(|a| a.story_id != id || self.to_view(a.byte).map(|v| a.byte = v).is_some());
        r.paragraph_flows.retain_mut(|p| {
            p.story_id != id
                || self.to_view(p.paragraph_start).map(|v| p.paragraph_start = v).is_some()
        });
        r.cross_references.retain_mut(|x| {
            if let StudioXrefDestination::Paragraph { story_id, paragraph_start } = &mut x.destination
                && story_id == id
                && let Some(v) = self.to_view(*paragraph_start)
            {
                *paragraph_start = v;
            }
            if x.source_story_id != id {
                return true;
            }
            match (self.to_view(x.source_start), self.to_view(x.source_end)) {
                (Some(a), Some(b)) => {
                    x.source_start = a;
                    x.source_end = b;
                    true
                }
                _ => false,
            }
        });
        r.conditional_ranges.retain(|g| g.story_id != id);
        r
    }
    /// Original byte of a view byte.
    pub fn to_original(&self, byte: u64) -> Option<u64> {
        for &(a, b, v) in &self.kept {
            let len = b.checked_sub(a)?;
            if byte >= v && byte < v.checked_add(len)? {
                return byte.checked_sub(v).and_then(|d| a.checked_add(d));
            }
        }
        self.kept.last().map(|k| k.1)
    }
}

/// Resolves conditional visibility of `story` (module docs). Ranges must lie on char boundaries of
/// the story text (else `invalid_input`); conditions are looked up in `records`.
pub fn conditional_view(
    story: &StudioTextStory,
    resolved_text: Option<&str>,
    records: &StudioLayoutFlowRecords,
) -> Result<ConditionalView, LayoutError> {
    let text = story_text(story, resolved_text)?;
    let len = text.len() as u64;
    // Sweep events: (pos, visible_delta, cover_delta) as signed pairs.
    let mut events: Vec<(u64, i64, i64)> = Vec::new();
    for g in records.conditional_ranges.iter().filter(|g| g.story_id == story.story_id) {
        let (s, e) = (usize_of(g.start)?, usize_of(g.end)?);
        if g.start >= g.end || g.end > len || !text.is_char_boundary(s) || !text.is_char_boundary(e) {
            return Err(LayoutError::InvalidInput);
        }
        let mut visible = false;
        for c in &g.conditions {
            visible |= records.condition(c).ok_or(LayoutError::InvalidInput)?.visible;
        }
        let v = i64::from(visible);
        events.push((g.start, v, 1));
        events.push((g.end, v.wrapping_neg(), -1));
    }
    events.sort_by_key(|e| e.0);
    let mut hidden: Vec<(u64, u64)> = Vec::new();
    let (mut vis, mut cover, mut i) = (0i64, 0i64, 0usize);
    let mut open: Option<u64> = None;
    while i < events.len() {
        let pos = events[i].0;
        while i < events.len() && events[i].0 == pos {
            vis = vis.saturating_add(events[i].1);
            cover = cover.saturating_add(events[i].2);
            i = i.saturating_add(1);
        }
        let hide = cover > 0 && vis == 0;
        match (open, hide) {
            (None, true) => open = Some(pos),
            (Some(a), false) => {
                hidden.push((a, pos));
                open = None;
            }
            _ => {}
        }
    }
    let mut kept = Vec::new();
    let (mut at, mut view) = (0u64, 0u64);
    for &(a, b) in &hidden {
        if a > at {
            kept.push((at, a, view));
            view = view.checked_add(a.saturating_sub(at)).ok_or(LayoutError::BudgetExceeded)?;
        }
        at = b;
    }
    if len > at {
        kept.push((at, len, view));
    }
    let mut out = String::new();
    for &(a, b, _) in &kept {
        out.push_str(&text[usize_of(a)?..usize_of(b)?]);
    }
    let map = |p: u64| -> u64 {
        let mut acc = 0u64;
        for &(a, b, _) in &kept {
            if p >= b {
                acc = acc.saturating_add(b.saturating_sub(a));
            } else if p > a {
                acc = acc.saturating_add(p.saturating_sub(a));
            }
        }
        acc
    };
    let paragraphs: Vec<StoryParagraph> = story
        .paragraphs
        .iter()
        .map(|p| StoryParagraph { start: map(p.start), end: map(p.end), ..p.clone() })
        .filter(|p| p.start < p.end)
        .collect();
    let character_ranges: Vec<StoryRange> = story
        .character_ranges
        .iter()
        .map(|r| StoryRange { start: map(r.start), end: map(r.end), ..r.clone() })
        .filter(|r| r.start < r.end)
        .collect();
    Ok(ConditionalView {
        story: StudioTextStory {
            content: StoryContent::Inline { text_utf8: out },
            paragraphs,
            character_ranges,
            ..story.clone()
        },
        kept,
        hidden,
    })
}
