//! Running headers and footers, text variables and section markers (STU-LAY-015, STU-LAY-016,
//! STU-LAY-048; lane SMB-LAYOUT-LONGDOC-2): each Folio `StudioRunningText` resolves, per frame it is
//! shown in, to the text of that frame's page; [`compose_overlay`] composes any text into a frame
//! it does not own (the frame geometry only), ready for the PDF overlays.
//!
//! Rules (documented; InDesign parity UNVERIFIED, D5):
//! - Page number: the page's label (`number_pages`). Section marker: the marker of the page's
//!   section (empty without one).
//! - Match paragraph / character style: the paragraphs (character ranges) of the sources carrying
//!   the style, each on the page where its first byte is placed; first or last on the page in
//!   source order; with none on the page, the last one on an earlier page; with none at all, empty.
//!   Delete end punctuation trims trailing `. , ; : ! ?`; change case upper / lower / title (first
//!   letter of each word upper) / sentence (first letter upper, rest unchanged).
//! - Last page number: the last page of the document or of the page's section, in the variable's
//!   style (`current` = the section's own style, without prefix). Chapter number: `before`, the
//!   number, `after`. File name: the caller's path, without folders unless `include_path`, without the
//!   extension unless `include_extension`. Dates: the caller's already formatted strings (layout
//!   has no clock; the stored format is the caller's). Cross-reference and live-caption variables
//!   are typed `variable_unsupported`.
#![deny(clippy::arithmetic_side_effects)]
use crate::folio_input::{single_frame, story_text};
use crate::numbering::{NumberingError, PageNumber, format_number};
use crate::styles::ParagraphStyling;
use crate::threaded_reflow::{FramePlacement, LayoutProposal, Limits, compose};
use crate::toc::{TocSource, clean, page_of};
use crate::{Composer, LayoutError, ShapedParagraph, SpaceParams, TypesetOptions};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    ChangeCase, RunningUse, StudioDocument, StudioNumberStyle, StudioRunningPart, StudioRunningText,
    StudioSection, StudioTextStory, StudioTextVariable, StudioVariableKind, VariableScope, check_running,
};
use std::collections::BTreeMap;

/// What only the caller knows.
#[derive(Clone, Copy, Debug, Default)]
pub struct VariableContext<'a> {
    pub file_path: Option<&'a str>,
    pub output_date: Option<&'a str>,
    pub creation_date: Option<&'a str>,
    pub modification_date: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunningError {
    /// Folio rule (`running_variable_missing`, `variable_duplicate`, ...).
    Rules(&'static str),
    Numbering(NumberingError),
    /// `variable_unsupported`, `file_name_unavailable`, `date_unavailable`.
    Unresolved(&'static str),
    Layout(LayoutError),
}
impl RunningError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Rules(w) | Self::Unresolved(w) => w,
            Self::Numbering(e) => e.code(),
            Self::Layout(e) => e.code(),
        }
    }
}
impl std::fmt::Display for RunningError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for RunningError {}

/// One resolved running text in one frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunningInstance {
    pub running_id: String,
    pub frame_id: String,
    pub page_id: String,
    pub text: String,
}

fn change_case(s: &str, case: ChangeCase) -> String {
    match case {
        ChangeCase::None => s.to_owned(),
        ChangeCase::Uppercase => s.to_uppercase(),
        ChangeCase::Lowercase => s.to_lowercase(),
        ChangeCase::Titlecase => {
            let mut out = String::with_capacity(s.len());
            let mut start = true;
            for c in s.chars() {
                if start && c.is_alphabetic() {
                    out.extend(c.to_uppercase());
                    start = false;
                } else {
                    out.push(c);
                    start = c.is_whitespace();
                }
            }
            out
        }
        ChangeCase::Sentencecase => {
            let mut chars = s.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        }
    }
}

/// (page ordinal, text) of every source occurrence of a style, in source order.
fn occurrences(sources: &[TocSource<'_>], by_page: &BTreeMap<(usize, usize), &PageNumber>, style: &str, paragraph: bool) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for s in sources {
        let spans: Vec<(u64, u64)> = if paragraph {
            s.story.paragraphs.iter().filter(|p| p.style_ref.as_deref() == Some(style)).map(|p| (p.start, p.end)).collect()
        } else {
            s.story.character_ranges.iter().filter(|r| r.style_ref.as_deref() == Some(style)).map(|r| (r.start, r.end)).collect()
        };
        for (a, b) in spans {
            let Some(page) = page_of(s.placement, a, by_page) else { continue };
            let text = usize::try_from(a).ok().zip(usize::try_from(b).ok()).and_then(|(a, b)| s.text.get(a..b)).map(clean).unwrap_or_default();
            out.push((page.ordinal, text));
        }
    }
    out
}

/// Resolves every running text for every frame showing it (module docs).
#[allow(clippy::too_many_arguments)]
pub fn resolve_running_texts(
    document: &StudioDocument,
    variables: &[StudioTextVariable],
    running: &[StudioRunningText],
    sections: &[StudioSection],
    pages: &[PageNumber],
    sources: &[TocSource<'_>],
    ctx: &VariableContext<'_>,
) -> Result<Vec<RunningInstance>, RunningError> {
    check_running(variables, running).map_err(RunningError::Rules)?;
    for s in sources {
        if s.placement.is_some_and(|p| p.base_revision != document.revision) {
            return Err(RunningError::Layout(LayoutError::StaleRevision { expected: s.placement.map_or(0, |p| p.base_revision), current: document.revision }));
        }
    }
    let by_page: BTreeMap<(usize, usize), &PageNumber> = pages.iter().map(|p| ((p.spread, p.page), p)).collect();
    let by_id: BTreeMap<&str, &StudioTextVariable> = variables.iter().map(|v| (v.variable_id.as_str(), v)).collect();
    let mut out = Vec::new();
    for r in running {
        for frame_id in &r.frame_ids {
            let layer = document.layers.iter().find(|l| l.layer_id == *frame_id).ok_or(RunningError::Layout(LayoutError::InvalidInput))?;
            let f = layer.text_frame.as_ref().ok_or(RunningError::Layout(LayoutError::InvalidInput))?;
            let page = pages.iter().find(|p| p.page_id == f.page_id).ok_or(RunningError::Layout(LayoutError::InvalidInput))?;
            let section = page.section.and_then(|k| sections.get(k));
            let mut text = String::new();
            for part in &r.parts {
                match part {
                    StudioRunningPart::Text { text: t } => text.push_str(t),
                    StudioRunningPart::PageNumber => text.push_str(&page.label),
                    StudioRunningPart::SectionMarker => text.push_str(section.and_then(|s| s.marker.as_deref()).unwrap_or("")),
                    StudioRunningPart::Variable { variable_id } => {
                        let v = by_id.get(variable_id.as_str()).ok_or(RunningError::Rules("running_variable_missing"))?;
                        text.push_str(&resolve_variable(v, page, pages, sections, sources, &by_page, ctx)?);
                    }
                }
                if text.len() > 65_536 {
                    return Err(RunningError::Layout(LayoutError::BudgetExceeded));
                }
            }
            out.push(RunningInstance { running_id: r.running_id.clone(), frame_id: frame_id.clone(), page_id: page.page_id.clone(), text });
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn resolve_variable(
    v: &StudioTextVariable,
    page: &PageNumber,
    pages: &[PageNumber],
    sections: &[StudioSection],
    sources: &[TocSource<'_>],
    by_page: &BTreeMap<(usize, usize), &PageNumber>,
    ctx: &VariableContext<'_>,
) -> Result<String, RunningError> {
    let fmt = |n: u32, s: StudioNumberStyle| format_number(n, s).map_err(RunningError::Numbering);
    Ok(match &v.kind {
        StudioVariableKind::CustomText { text } => text.clone(),
        StudioVariableKind::FileName { include_path, include_extension } => {
            let path = ctx.file_path.ok_or(RunningError::Unresolved("file_name_unavailable"))?;
            let name_at = path.rfind(['/', '\\']).map_or(0, |i| i.saturating_add(1));
            let (dir, mut name) = path.split_at(name_at);
            if !include_extension && let Some(dot) = name.rfind('.').filter(|d| *d > 0) {
                name = &name[..dot];
            }
            if *include_path { format!("{dir}{name}") } else { name.to_owned() }
        }
        StudioVariableKind::LastPageNumber { scope, style } => {
            let last = match scope {
                VariableScope::Document => pages.last(),
                VariableScope::Section => pages.iter().rev().find(|p| p.section == page.section),
            }
            .ok_or(RunningError::Layout(LayoutError::InvalidInput))?;
            let style = match style {
                StudioNumberStyle::Current => last.section.and_then(|k| sections.get(k)).map_or(StudioNumberStyle::Arabic, |s| s.page_number_style),
                s => *s,
            };
            fmt(last.number, style)?
        }
        StudioVariableKind::ChapterNumber { number, before, after, style } => format!("{before}{}{after}", fmt(*number, *style)?),
        StudioVariableKind::OutputDate { .. } => ctx.output_date.ok_or(RunningError::Unresolved("date_unavailable"))?.to_owned(),
        StudioVariableKind::CreationDate { .. } => ctx.creation_date.ok_or(RunningError::Unresolved("date_unavailable"))?.to_owned(),
        StudioVariableKind::ModificationDate { .. } => ctx.modification_date.ok_or(RunningError::Unresolved("date_unavailable"))?.to_owned(),
        StudioVariableKind::MatchParagraphStyle { style, use_on_page, delete_end_punctuation, change_case: case }
        | StudioVariableKind::MatchCharacterStyle { style, use_on_page, delete_end_punctuation, change_case: case } => {
            let paragraph = matches!(v.kind, StudioVariableKind::MatchParagraphStyle { .. });
            let occ = occurrences(sources, by_page, style, paragraph);
            let here: Vec<&(usize, String)> = occ.iter().filter(|o| o.0 == page.ordinal).collect();
            let picked = match use_on_page {
                RunningUse::FirstOnPage => here.first().copied(),
                RunningUse::LastOnPage => here.last().copied(),
            }
            .or_else(|| occ.iter().filter(|o| o.0 < page.ordinal).max_by_key(|o| o.0));
            let mut t = picked.map(|o| o.1.clone()).unwrap_or_default();
            if *delete_end_punctuation {
                t = t.trim_end_matches(['.', ',', ';', ':', '!', '?']).to_owned();
            }
            change_case(&t, *case)
        }
        StudioVariableKind::XrefPageNumber | StudioVariableKind::XrefChapterNumber | StudioVariableKind::LiveCaption => {
            return Err(RunningError::Unresolved("variable_unsupported"));
        }
    })
}

/// Composes `paragraphs` of `story` into the frame layer `frame_id` (its geometry only, from the
/// frame's first baseline), as a placement whose one frame names that layer.
#[allow(clippy::too_many_arguments)]
pub fn compose_overlay(
    document: &StudioDocument,
    frame_id: &str,
    story: &StudioTextStory,
    resolved_text: Option<&str>,
    paragraphs: &[ShapedParagraph],
    composer: Composer,
    opts: &TypesetOptions<'_>,
    limits: Limits,
    cancel: &CancellationToken,
) -> Result<LayoutProposal, LayoutError> {
    opts.validate()?;
    let text = story_text(story, resolved_text)?;
    let (frame, _) = single_frame(document, frame_id)?;
    let styled;
    let (paragraphs, stylings): (&[ShapedParagraph], Vec<Option<ParagraphStyling>>) = match opts.styles {
        Some(src) => {
            let (a, b) = crate::styles::apply(src, story, text, paragraphs, opts.alignment)?;
            styled = a;
            (&styled, b.into_iter().map(Some).collect())
        }
        None => (paragraphs, vec![None; paragraphs.len()]),
    };
    let c = compose(std::slice::from_ref(&frame), text, paragraphs, &stylings, composer, SpaceParams::default(), opts, limits, cancel)?;
    Ok(LayoutProposal {
        base_revision: document.revision,
        frames: vec![FramePlacement {
            frame_id: frame.id.to_owned(),
            frame_index: frame.layer_index,
            spread: frame.spread,
            page: frame.page,
            origin_x_pt: frame.origin_x_pt,
            origin_y_pt: frame.origin_y_pt,
            lines: c.lines.into_iter().next().unwrap_or_default(),
        }],
        overset: c.overset,
    })
}
