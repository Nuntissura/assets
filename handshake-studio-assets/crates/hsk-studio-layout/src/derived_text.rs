//! Text derived from a story (lane SMB-LAYOUT-ENTRY-TEXT-1, R55 F-R55-5 and F-R55-9; Master Spec
//! v02.229 STU-TYP-207): TOC entries and their generated story, running text, cross-reference text and
//! endnote text. The one rule of every site that copies story text into generated or derived text:
//! the result holds no stream sentinel and ends where the paragraph ends. The stored stream is not
//! touched (a marker sentinel carries no text; composition shows its value without rewriting it).
//!
//! The members of the STU-TYP-207 catalogs, by what derived text does with them (`derive_text`):
//! - No value at the source, left out: index marker U+FDD7, footnote reference U+FDD4, endnote
//!   reference U+FDD5, anchored-object marker U+FFFC.
//! - A value at the source, shown as that value: current, next and previous page number U+FDD0..U+FDD2
//!   (the label of the page holding the marker's own line, of the next, of the previous document page),
//!   section marker U+FDD3 (the `marker` of that page's section). The same rules as
//!   `sentinels::resolve`; `derived_text_native_type::derived_values_equal_the_values_composition_resolves`
//!   holds the two equal through native Type.
//! - A value this call cannot read, a typed [`DerivedMarkerGap`], no text: cross-reference marker
//!   U+FDD6 (its value is the display text of a record no derived-text call is given,
//!   `UnresolvedMarker::RecordMissing`), a section marker in a call without sections
//!   ([`DerivedGapReason::SectionsUnavailable`]: TOC and cross-references), a marker with no placed line
//!   (`NotPlaced`), no neighbouring page or section value (the composition states), a value over
//!   `MAX_RESOLVED_MARKER_BYTES` or not inline (`TextOverBudget`, `TextNotInline`).
//! - Not a marker: every terminator of `ends_paragraph` (the twelve, break sentinels U+FDD8..U+FDDC
//!   included) is trimmed at the end and becomes a space inside (a character-style range or a whole note
//!   body crossing paragraphs), so the generated story's paragraphs are its partition. Flush space
//!   U+FDDD becomes a space, as a tab does in an entry. A reserved code point U+FDDE..U+FDEF has no
//!   member: left out with a typed gap.
//!
//! Studio readings; InDesign's handling of markers in a TOC entry UNVERIFIED (D5).
//!
//! Hostile input: only a marker with a value or a gap allocates beyond the input's own size, at most
//! `MAX_RESOLVED_MARKER_BYTES` bytes each, and one generator call takes at most `MAX_STORY_MARKERS` of
//! them ([`DerivedBudget`]); over it the call refuses with its own budget error
//! (`v4_02_the_budget_is_the_aggregate_of_one_call`).
#![deny(clippy::arithmetic_side_effects)]
use crate::numbering::PageNumber;
use crate::sentinels::{FLUSH_SPACE, MAX_RESOLVED_MARKER_BYTES, MAX_STORY_MARKERS, MarkerKind, UnresolvedMarker, ends_paragraph};
use crate::toc::{TocSource, line_of};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::StudioSection;
use hsk_studio_type as ty;
use std::collections::BTreeMap;

/// The anchored-object marker of the catalog (a real character, not a sentinel).
const ANCHORED_OBJECT: char = '\u{FFFC}';

/// Why a marker in derived text shows no value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DerivedGapReason {
    /// The explicit state the marker has at its source byte.
    Marker(UnresolvedMarker),
    /// A section marker in a call that is given no Folio sections.
    SectionsUnavailable,
    /// A reserved sentinel-block code point (U+FDDE..U+FDEF): no catalog member, no meaning.
    ReservedCodePoint,
}
impl DerivedGapReason {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Marker(u) => u.code(),
            Self::SectionsUnavailable => "derived_sections_unavailable",
            Self::ReservedCodePoint => "derived_reserved_code_point",
        }
    }
}

/// One marker whose value derived text does not show: an explicit outcome, never a guessed value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DerivedMarkerGap {
    /// The sentinel's byte in its source story.
    pub story_byte: u64,
    /// `None` for a reserved code point.
    pub kind: Option<MarkerKind>,
    pub reason: DerivedGapReason,
}

/// What the site keeps of two characters that are not terminators.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DerivedRules {
    pub forced_break_to_space: bool,
    pub tab_to_space: bool,
}
/// TOC entries, running text and endnote text: U+2028 and tabs become spaces.
pub(crate) const ENTRY_RULES: DerivedRules = DerivedRules { forced_break_to_space: true, tab_to_space: true };
/// Cross-reference text keeps them.
pub(crate) const KEEP_RULES: DerivedRules = DerivedRules { forced_break_to_space: false, tab_to_space: false };

/// Where the source markers resolve: the source story's placement, the document's pages and, when the
/// call has them, its sections.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DerivedPages<'a> {
    pub pages: &'a [PageNumber],
    pub by_page: &'a BTreeMap<(usize, usize), &'a PageNumber>,
    pub sections: Option<&'a [StudioSection]>,
}

/// Markers with a value or a gap one generator call may still take (module docs).
#[derive(Clone, Copy, Debug)]
pub(crate) struct DerivedBudget {
    left: usize,
}
impl DerivedBudget {
    pub(crate) const fn new() -> Self {
        Self { left: MAX_STORY_MARKERS }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DerivedRefusal {
    BudgetExceeded,
    Canceled,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DerivedText {
    pub text: String,
    pub gaps: Vec<DerivedMarkerGap>,
}

/// The document page with 1-based ordinal `ordinal`.
fn page_with_ordinal(pages: &[PageNumber], ordinal: usize) -> Option<&PageNumber> {
    pages.get(pages.partition_point(|p| p.ordinal < ordinal)).filter(|p| p.ordinal == ordinal)
}

/// The value of a page-number or section marker at story byte `at` (module docs).
fn value_at<'a>(
    kind: MarkerKind,
    at: u64,
    source: Option<&TocSource<'_>>,
    pages: Option<&DerivedPages<'a>>,
) -> Result<&'a str, DerivedGapReason> {
    use UnresolvedMarker as U;
    let unresolved = |u: U| DerivedGapReason::Marker(u);
    let (Some(source), Some(d)) = (source, pages) else {
        return Err(unresolved(U::NotPlaced));
    };
    let (frame, _) = line_of(source, at).ok_or(unresolved(U::NotPlaced))?;
    let page: &'a PageNumber = d.by_page.get(&(frame.spread, frame.page)).copied().ok_or(unresolved(U::NotPlaced))?;
    let value = match kind {
        MarkerKind::CurrentPageNumber => page.label.as_str(),
        MarkerKind::NextPageNumber => {
            let next = page.ordinal.checked_add(1).and_then(|o| page_with_ordinal(d.pages, o));
            next.ok_or(unresolved(U::NoNextPage))?.label.as_str()
        }
        MarkerKind::PreviousPageNumber => {
            let previous = page.ordinal.checked_sub(1).and_then(|o| page_with_ordinal(d.pages, o));
            previous.ok_or(unresolved(U::NoPreviousPage))?.label.as_str()
        }
        MarkerKind::SectionMarker => {
            let sections = d.sections.ok_or(DerivedGapReason::SectionsUnavailable)?;
            let section = page.section.ok_or(unresolved(U::NoSection))?;
            sections.get(section).and_then(|s| s.marker.as_deref()).ok_or(unresolved(U::NoSectionMarker))?
        }
        MarkerKind::CrossReference => return Err(unresolved(U::RecordMissing)),
        MarkerKind::Index | MarkerKind::FootnoteReference | MarkerKind::EndnoteReference => return Err(unresolved(U::RuleUnstated)),
    };
    if value.len() > MAX_RESOLVED_MARKER_BYTES {
        return Err(unresolved(U::TextOverBudget));
    }
    if value.chars().any(|c| ends_paragraph(c) || ty::is_sentinel_block(c) || c == ANCHORED_OBJECT) {
        return Err(unresolved(U::TextNotInline));
    }
    Ok(value)
}

/// The derived text of `raw`, the story bytes starting at `start` of `source` (module docs). `source`
/// `None` = a story with no placement at this call (a note body).
pub(crate) fn derive_text<'a>(
    raw: &str,
    start: u64,
    source: Option<&TocSource<'_>>,
    pages: Option<&DerivedPages<'a>>,
    rules: DerivedRules,
    budget: &mut DerivedBudget,
    cancel: Option<&CancellationToken>,
) -> Result<DerivedText, DerivedRefusal> {
    let body = raw.trim_end_matches(ends_paragraph);
    let mut out = DerivedText { text: String::with_capacity(body.len()), gaps: Vec::new() };
    for (i, c) in body.char_indices() {
        let is_space = (c == '\u{2028}' && rules.forced_break_to_space) || (c == '\t' && rules.tab_to_space) || c == FLUSH_SPACE || ends_paragraph(c);
        if is_space {
            out.text.push(' ');
            continue;
        }
        if c == ANCHORED_OBJECT {
            continue;
        }
        if !ty::is_sentinel_block(c) {
            out.text.push(c);
            continue;
        }
        let kind = MarkerKind::from_sentinel(c);
        if matches!(kind, Some(MarkerKind::Index | MarkerKind::FootnoteReference | MarkerKind::EndnoteReference)) {
            continue;
        }
        budget.left = budget.left.checked_sub(1).ok_or(DerivedRefusal::BudgetExceeded)?;
        if budget.left.is_multiple_of(256) && cancel.is_some_and(|t| t.check().is_err()) {
            return Err(DerivedRefusal::Canceled);
        }
        let story_byte = start.saturating_add(i as u64);
        let value = match kind {
            Some(k) => value_at(k, story_byte, source, pages),
            None => Err(DerivedGapReason::ReservedCodePoint),
        };
        match value {
            Ok(v) => out.text.push_str(v),
            Err(reason) => out.gaps.push(DerivedMarkerGap { story_byte, kind, reason }),
        }
    }
    Ok(out)
}
