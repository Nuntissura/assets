//! Tables of contents (STU-LAY-044, STU-LAY-148): a Folio `StudioTocStyle` generates a new story
//! from the paragraphs of the source stories whose paragraph style is mapped, with the label of the
//! page where each paragraph starts (layout placements + `number_pages`). The caller places and
//! composes the generated story like any other (`GeneratedStory::to_story`); multiple TOCs are
//! generated one style at a time (update-all = every style again).
//!
//! Rules (documented; InDesign parity UNVERIFIED, D5):
//! - Sources are read in the given story order, paragraphs in story order. A paragraph's page is the
//!   page of the frame holding the first line whose source span contains (or starts at) the
//!   paragraph start. An unplaced (overset) paragraph is left out unless the style's
//!   `include_overset` is set; it is then listed without a page label.
//! - Entry text = the paragraph text without its trailing break; forced line breaks (U+2028) and tabs
//!   become spaces. Position `after`: text + separator + label; `before`: label + separator + text;
//!   `none`: text.
//! - Levels nest (an entry's children are the following deeper entries); siblings are sorted
//!   alphabetically (lower-cased, stable) when every sibling's level has `sort_alphabet`.
//! - Loops are refused before anything is generated (`toc_loop`): the target story is one of the
//!   sources, or a format or title style is itself a mapped source style (regenerating would feed the
//!   TOC into itself). The style rules are Folio's `check_toc_style` (`toc_no_entries`,
//!   `toc_style_mapped_twice`, ...). Entries and text bytes are bounded (`TocLimits`), checked before
//!   each push.
#![deny(clippy::arithmetic_side_effects)]
use crate::numbering::PageNumber;
use crate::threaded_reflow::LayoutProposal;
use crate::LayoutError;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    StoryContent, StoryDirection, StoryParagraph, StoryRange, StoryTextKind, StudioDocument,
    StudioTextStory, StudioTocStyle, TextStorySchema, TocPageNumberPosition, check_toc_style,
};
use std::collections::BTreeMap;

/// One story the TOC reads.
#[derive(Clone, Copy, Debug)]
pub struct TocSource<'a> {
    pub story: &'a StudioTextStory,
    /// The whole story text (inline content or the caller-resolved stored bytes).
    pub text: &'a str,
    /// Its layout placement; `None` = nothing of it is placed.
    pub placement: Option<&'a LayoutProposal>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TocLimits {
    pub max_entries: usize,
    pub max_text_bytes: usize,
}
impl Default for TocLimits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_text_bytes: 16 << 20,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TocError {
    /// A Folio TOC-style rule (`toc_no_entries`, `toc_style_mapped_twice`, ...).
    Style(&'static str),
    Loop,
    BudgetExceeded,
    Layout(LayoutError),
}
impl TocError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Style(w) => w,
            Self::Loop => "toc_loop",
            Self::BudgetExceeded => "budget_exceeded",
            Self::Layout(e) => e.code(),
        }
    }
}
impl std::fmt::Display for TocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for TocError {}

/// One generated entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TocEntryOut {
    pub level: u32,
    pub text: String,
    pub page_label: Option<String>,
    pub page_id: Option<String>,
    pub story_id: String,
    pub source_start: u64,
}

/// Generated text with Folio paragraphs (style references set) and the entries it holds.
#[derive(Clone, Debug, PartialEq)]
pub struct GeneratedStory {
    pub text: String,
    pub paragraphs: Vec<StoryParagraph>,
    pub entries: Vec<TocEntryOut>,
}
impl GeneratedStory {
    /// A Folio story (inline content, left to right, one character range) carrying this text.
    pub fn to_story(&self, story_id: &str) -> StudioTextStory {
        let end = self.text.len() as u64;
        StudioTextStory {
            schema_id: TextStorySchema::V1,
            story_id: story_id.to_owned(),
            text_kind: StoryTextKind::Area,
            story_direction: StoryDirection::LeftToRight,
            content: StoryContent::Inline {
                text_utf8: self.text.clone(),
            },
            paragraphs: self.paragraphs.clone(),
            character_ranges: if end == 0 {
                Vec::new()
            } else {
                vec![StoryRange {
                    start: 0,
                    end,
                    style_ref: None,
                    applied_language: None,
                }]
            },
        }
    }
}

/// Appends paragraphs to a generated story within a byte budget.
pub(crate) struct Writer {
    pub text: String,
    pub paragraphs: Vec<StoryParagraph>,
    max: usize,
}
impl Writer {
    pub(crate) fn new(max: usize) -> Self {
        Self {
            text: String::new(),
            paragraphs: Vec::new(),
            max,
        }
    }
    /// One paragraph; a newline ends the previous one.
    pub(crate) fn push(&mut self, text: &str, style: Option<&str>) -> Result<(), TocError> {
        let sep = usize::from(!self.text.is_empty());
        let need = self
            .text
            .len()
            .checked_add(sep)
            .and_then(|n| n.checked_add(text.len()))
            .filter(|n| *n <= self.max)
            .ok_or(TocError::BudgetExceeded)?;
        if sep == 1 {
            self.text.push('\n');
            if let Some(last) = self.paragraphs.last_mut() {
                last.end = self.text.len() as u64;
            }
        }
        let start = self.text.len() as u64;
        self.text.push_str(text);
        debug_assert_eq!(self.text.len(), need);
        self.paragraphs.push(StoryParagraph {
            start,
            end: self.text.len() as u64,
            style_ref: style.map(str::to_owned),
            paragraph_direction: None,
        });
        Ok(())
    }
}

/// Paragraph text without its trailing break; U+2028 and tabs become spaces.
pub(crate) fn clean(text: &str) -> String {
    text.trim_end_matches(['\n', '\r', '\u{2029}'])
        .chars()
        .map(|c| if c == '\u{2028}' || c == '\t' { ' ' } else { c })
        .collect()
}

/// The page label (and id) where story byte `at` is placed, if it is.
pub(crate) fn page_of<'p>(
    placement: Option<&LayoutProposal>,
    at: u64,
    pages: &'p BTreeMap<(usize, usize), &'p PageNumber>,
) -> Option<&'p PageNumber> {
    let placement = placement?;
    placement.frames.iter().find_map(|f| {
        f.lines
            .iter()
            .any(|l| l.source.start == at || (l.source.start <= at && at < l.source.end))
            .then(|| pages.get(&(f.spread, f.page)).copied())
            .flatten()
    })
}

struct Node {
    entry: TocEntryOut,
    sort: bool,
    style_index: usize,
    children: Vec<Node>,
}

fn sort_nodes(nodes: &mut [Node]) {
    if !nodes.is_empty() && nodes.iter().all(|n| n.sort) {
        nodes.sort_by_cached_key(|n| n.entry.text.to_lowercase());
    }
    for n in nodes.iter_mut() {
        sort_nodes(&mut n.children);
    }
}
fn flatten(nodes: Vec<Node>, out: &mut Vec<(TocEntryOut, usize)>) {
    for n in nodes {
        out.push((n.entry, n.style_index));
        flatten(n.children, out);
    }
}

/// Generates one TOC (module docs). `pages` comes from `number_pages`; `target_story_id` is the
/// story the TOC will be written into.
pub fn generate_toc(
    document: &StudioDocument,
    style: &StudioTocStyle,
    sources: &[TocSource<'_>],
    pages: &[PageNumber],
    target_story_id: &str,
    limits: TocLimits,
    cancel: &CancellationToken,
) -> Result<GeneratedStory, TocError> {
    cancel.check().map_err(|_| TocError::Layout(LayoutError::Canceled))?;
    check_toc_style(style).map_err(TocError::Style)?;
    let mapped: BTreeMap<&str, usize> = style
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.paragraph_style.as_str(), i))
        .collect();
    let feeds_back = style
        .entries
        .iter()
        .filter_map(|e| e.format_style.as_deref())
        .chain(style.title_style.as_deref())
        .any(|s| mapped.contains_key(s));
    if feeds_back || sources.iter().any(|s| s.story.story_id == target_story_id) {
        return Err(TocError::Loop);
    }
    for s in sources {
        if s.placement.is_some_and(|p| p.base_revision != document.revision) {
            return Err(TocError::Layout(LayoutError::StaleRevision {
                expected: s.placement.map_or(0, |p| p.base_revision),
                current: document.revision,
            }));
        }
        if s.text.len() as u64 != s.story.byte_len() {
            return Err(TocError::Layout(LayoutError::InvalidInput));
        }
    }
    let by_page: BTreeMap<(usize, usize), &PageNumber> =
        pages.iter().map(|p| ((p.spread, p.page), p)).collect();
    // Flat list first (bounded), then the level tree.
    let mut flat: Vec<(TocEntryOut, bool, usize)> = Vec::new();
    for s in sources {
        for (k, p) in s.story.paragraphs.iter().enumerate() {
            if k.is_multiple_of(256) {
                cancel.check().map_err(|_| TocError::Layout(LayoutError::Canceled))?;
            }
            let Some(&ei) = p.style_ref.as_deref().and_then(|r| mapped.get(r)) else {
                continue;
            };
            let e = &style.entries[ei];
            let page = page_of(s.placement, p.start, &by_page);
            if page.is_none() && !style.include_overset {
                continue;
            }
            if flat.len() >= limits.max_entries {
                return Err(TocError::BudgetExceeded);
            }
            let raw = usize::try_from(p.start)
                .ok()
                .zip(usize::try_from(p.end).ok())
                .and_then(|(a, b)| s.text.get(a..b))
                .ok_or(TocError::Layout(LayoutError::InvalidInput))?;
            flat.push((
                TocEntryOut {
                    level: e.level,
                    text: clean(raw),
                    page_label: page.map(|x| x.label.clone()),
                    page_id: page.map(|x| x.page_id.clone()),
                    story_id: s.story.story_id.clone(),
                    source_start: p.start,
                },
                e.sort_alphabet,
                ei,
            ));
        }
    }
    // Level tree: an entry's children are the following deeper entries.
    let mut roots: Vec<Node> = Vec::new();
    let mut stack: Vec<Node> = Vec::new();
    for (entry, sort, style_index) in flat {
        let node = Node {
            entry,
            sort,
            style_index,
            children: Vec::new(),
        };
        while stack
            .last()
            .is_some_and(|top| top.entry.level >= node.entry.level)
        {
            let done = stack.pop().expect("non-empty");
            match stack.last_mut() {
                Some(parent) => parent.children.push(done),
                None => roots.push(done),
            }
        }
        stack.push(node);
    }
    while let Some(done) = stack.pop() {
        match stack.last_mut() {
            Some(parent) => parent.children.push(done),
            None => roots.push(done),
        }
    }
    sort_nodes(&mut roots);
    let mut entries = Vec::new();
    flatten(roots, &mut entries);
    let mut w = Writer::new(limits.max_text_bytes);
    if !style.title.is_empty() {
        w.push(&style.title, style.title_style.as_deref())?;
    }
    for (k, (e, ei)) in entries.iter().enumerate() {
        if k.is_multiple_of(256) {
            cancel.check().map_err(|_| TocError::Layout(LayoutError::Canceled))?;
        }
        // The TOC entry that mapped its source paragraph style decides the format.
        let te = &style.entries[*ei];
        let (sep, fmt) = (te.separator.as_str(), te.format_style.as_deref());
        let line = match (&e.page_label, te.page_number_position) {
            (Some(l), TocPageNumberPosition::After) => format!("{}{sep}{l}", e.text),
            (Some(l), TocPageNumberPosition::Before) => format!("{l}{sep}{}", e.text),
            _ => e.text.clone(),
        };
        w.push(&line, fmt)?;
    }
    Ok(GeneratedStory {
        text: w.text,
        paragraphs: w.paragraphs,
        entries: entries.into_iter().map(|(e, _)| e).collect(),
    })
}
