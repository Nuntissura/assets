//! Reads the Folio document for reflow. Pages, text frames and threads are Folio's
//! (`StudioPageSpread::pages`, `StudioLayer::text_frame`, `StudioDocument::text_threads`,
//! `StudioDocument::locate_page`) and the story is a `StudioTextStory`; layout keeps no model of its
//! own. This module resolves one story's thread into the plain numbers `reflow` needs and refuses
//! anything Folio can express that this slice does not compose, instead of mis-laying it.
//!
//! The document is NOT assumed to be validated: every number is checked here (finite, unit `pt`,
//! positive sizes, resolvable page) and the thread is re-walked, so a hand-built or cyclic document
//! gets a typed refusal and never loops. Folio's own validation refuses the same inputs earlier.
use crate::LayoutError;
use crate::geometry::Rect;
use hsk_studio_folio::{
    AutoSizing, FirstBaseline, GeometryUnit, Length, Payload, StoryContent, StoryDirection,
    StoryParagraphDirection, StoryTextKind, StudioDocument, StudioLayer, StudioPage,
    StudioTextFrame, StudioTextStory, VerticalJustification,
};
use std::collections::BTreeSet;

/// Text-wrap obstacles of one frame, in frame coordinates (origin = frame top-left, y down). Folio
/// stores no wrap or obstacle data (STU-LAY-126 is not stored), so the caller supplies them.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameObstacles {
    /// `layer_id` of the frame layer the rectangles belong to.
    pub frame_id: String,
    pub rects: Vec<Rect>,
}

/// One frame of the thread, in points. `origin_*` is the frame top-left in spread coordinates.
pub(crate) struct Frame<'a> {
    pub id: &'a str,
    /// Index of the frame's layer in `StudioDocument::layers`.
    pub layer_index: usize,
    pub spread: usize,
    pub page: usize,
    pub origin_x_pt: f64,
    pub origin_y_pt: f64,
    pub width_pt: f64,
    pub height_pt: f64,
    pub inset_left_pt: f64,
    pub inset_right_pt: f64,
    pub inset_bottom_pt: f64,
    /// From the frame top.
    pub first_baseline_pt: f64,
    pub obstacles: Vec<Rect>,
}

pub(crate) struct Input<'a> {
    pub text: &'a str,
    /// In flow order.
    pub frames: Vec<Frame<'a>>,
}

fn pt(l: &Length) -> Result<f64, LayoutError> {
    if l.unit == GeometryUnit::Pt && l.value.is_finite() {
        Ok(l.value)
    } else {
        Err(LayoutError::InvalidGeometry)
    }
}

fn page_ok(p: &StudioPage) -> Result<(), LayoutError> {
    let m = &p.margins;
    if pt(&p.width)? <= 0.0 || pt(&p.height)? <= 0.0 {
        return Err(LayoutError::InvalidGeometry);
    }
    for l in [&m.top, &m.right, &m.bottom, &m.left] {
        if pt(l)? < 0.0 {
            return Err(LayoutError::InvalidGeometry);
        }
    }
    Ok(())
}

/// Story text: inline content, or the caller-resolved bytes of stored content (exact length).
fn story_text<'a>(
    story: &'a StudioTextStory,
    resolved_text: Option<&'a str>,
) -> Result<&'a str, LayoutError> {
    match (&story.content, resolved_text) {
        (StoryContent::Inline { text_utf8 }, None) => Ok(text_utf8.as_str()),
        (StoryContent::Stored { byte_len, .. }, Some(t)) if *byte_len == t.len() as u64 => Ok(t),
        _ => Err(LayoutError::InvalidInput),
    }
}

/// What this slice composes: one column, top-aligned, fixed-size frames, an explicit fixed first
/// baseline, left-to-right Area stories. Everything else is a typed refusal.
fn supported(story: &StudioTextStory, f: &StudioTextFrame) -> Result<(), LayoutError> {
    let rtl = story.story_direction == StoryDirection::RightToLeft
        || story
            .paragraphs
            .iter()
            .any(|p| p.paragraph_direction == Some(StoryParagraphDirection::RightToLeft));
    if rtl
        || f.text_column_count != 1
        || f.vertical_justification != VerticalJustification::Top
        || f.auto_sizing_type != AutoSizing::Off
        || f.first_baseline_offset != FirstBaseline::Fixed
    {
        return Err(LayoutError::UnsupportedComposition);
    }
    Ok(())
}

pub(crate) fn resolve<'a>(
    document: &'a StudioDocument,
    story: &'a StudioTextStory,
    resolved_text: Option<&'a str>,
    obstacles: &[FrameObstacles],
) -> Result<Input<'a>, LayoutError> {
    let text = story_text(story, resolved_text)?;
    match story.text_kind {
        StoryTextKind::Area => {}
        StoryTextKind::FrameGrid => return Err(LayoutError::UnsupportedComposition),
        StoryTextKind::Point | StoryTextKind::Path => return Err(LayoutError::InvalidInput),
    }
    let story_id = story.story_id.as_str();
    let of_story: Vec<(usize, &'a StudioLayer, &'a StudioTextFrame)> = document
        .layers
        .iter()
        .enumerate()
        .filter_map(|(i, l)| match (&l.payload, &l.text_frame) {
            (Payload::Text { story_id: s }, Some(f)) if s.as_str() == story_id => Some((i, l, f)),
            _ => None,
        })
        .collect();
    if of_story.is_empty() {
        return Err(LayoutError::InvalidInput);
    }
    let ids: BTreeSet<&str> = of_story
        .iter()
        .map(|(_, l, _)| l.layer_id.as_str())
        .collect();
    // Folio's chain walk ends silently at a repeated or unresolvable link, so completeness is
    // re-checked here: that is where cycles and stray frames surface.
    let threads = document.text_threads();
    let mut mine = threads.iter().filter(|t| t.story_id() == story_id);
    let thread = match (mine.next(), mine.next()) {
        (Some(t), None) => t,
        (None, _) => {
            // No frame without a predecessor: every frame is preceded by one of the story's own
            // frames only if the links close into a loop.
            let looped = of_story.iter().all(|(_, _, f)| {
                f.previous_text_frame
                    .as_deref()
                    .is_some_and(|p| ids.contains(p))
            });
            return Err(if looped {
                LayoutError::CyclicThread
            } else {
                LayoutError::InvalidInput
            });
        }
        _ => return Err(LayoutError::InvalidInput),
    };
    let mut chain: Vec<(usize, &'a StudioLayer, &'a StudioTextFrame)> =
        Vec::with_capacity(thread.frames().len());
    for id in thread.frames() {
        let found = of_story.iter().find(|(_, l, _)| l.layer_id == *id);
        chain.push(*found.ok_or(LayoutError::InvalidInput)?);
    }
    if chain.len() != of_story.len() {
        return Err(LayoutError::InvalidInput);
    }
    if let Some((_, _, last)) = chain.last()
        && let Some(next) = &last.next_text_frame
    {
        return Err(if ids.contains(next.as_str()) {
            LayoutError::CyclicThread
        } else {
            LayoutError::InvalidInput
        });
    }
    for pair in chain.windows(2) {
        if pair[1].2.previous_text_frame.as_deref() != Some(pair[0].1.layer_id.as_str()) {
            return Err(LayoutError::InvalidInput);
        }
    }

    let mut checked = BTreeSet::new();
    let mut frames = Vec::with_capacity(chain.len());
    for &(layer_index, layer, f) in &chain {
        let loc = document
            .locate_page(&f.page_id)
            .ok_or(LayoutError::InvalidInput)?;
        if checked.insert(loc.spread_index) {
            for p in &loc.spread.pages {
                page_ok(p)?;
            }
        }
        let page_x = loc.spread.pages[..loc.page_index]
            .iter()
            .map(|p| pt(&p.width))
            .sum::<Result<f64, LayoutError>>()?;
        let b = &f.bounds;
        let (x, y, w, h) = (pt(&b.x)?, pt(&b.y)?, pt(&b.width)?, pt(&b.height)?);
        let i = &f.inset_spacing;
        let (top, right, bottom, left) = (pt(&i.top)?, pt(&i.right)?, pt(&i.bottom)?, pt(&i.left)?);
        let baseline = pt(&f.minimum_first_baseline_offset)?;
        if w <= 0.0
            || h <= 0.0
            || top < 0.0
            || right < 0.0
            || bottom < 0.0
            || left < 0.0
            || left + right >= w
            || top + bottom >= h
            || baseline < top
        {
            return Err(LayoutError::InvalidGeometry);
        }
        let rects: Vec<Rect> = obstacles
            .iter()
            .filter(|o| o.frame_id == layer.layer_id)
            .flat_map(|o| o.rects.iter().copied())
            .collect();
        if !rects.iter().all(Rect::valid) {
            return Err(LayoutError::InvalidGeometry);
        }
        frames.push(Frame {
            id: layer.layer_id.as_str(),
            layer_index,
            spread: loc.spread_index,
            page: loc.page_index,
            origin_x_pt: page_x + x,
            origin_y_pt: y,
            width_pt: w,
            height_pt: h,
            inset_left_pt: left,
            inset_right_pt: right,
            inset_bottom_pt: bottom,
            first_baseline_pt: baseline,
            obstacles: rects,
        });
    }
    for (_, _, f) in &chain {
        supported(story, f)?;
    }
    Ok(Input { text, frames })
}
