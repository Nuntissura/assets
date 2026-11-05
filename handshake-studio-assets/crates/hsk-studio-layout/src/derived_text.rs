//! Text derived from a story (lane SMB-LAYOUT-ENTRY-TEXT-1, R55 F-R55-5 and F-R55-9; Master Spec
//! v02.229 STU-TYP-207): TOC entries and their generated story, running text, cross-reference text,
//! endnote text and captured index topics. The one rule of every site that copies story text into generated or derived text:
//! the result holds no stream sentinel and ends where the paragraph ends. The stored stream is not
//! touched (a marker sentinel carries no text; composition shows its value without rewriting it).
//!
//! The members of the STU-TYP-207 catalogs, by what derived text does with them (`derive_text`):
//! - No value at the source, left out: index marker U+FDD7, footnote reference U+FDD4, endnote
//!   reference U+FDD5, anchored-object marker U+FFFC.
//! - A value at the source, shown as that value: current, next and previous page number U+FDD0..U+FDD2
//!   (the label of the page holding the marker's own line, of the next, of the previous document page),
//!   section marker U+FDD3 (the `marker` of that page's section). The value is composition's own rule,
//!   `sentinels::marker_value`; `derived_text_native_type::derived_values_equal_the_values_composition_resolves`
//!   holds the two locations of a marker equal through native Type.
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
//! `capture_index_mark` has no placement, pages or records, so any gap refuses the capture
//! (`index_invalid_topic_entry`), since a topic is stored text with nowhere to carry a gap.
//!
//! A heading that holds only markers: `generate_toc` and `generate_table_of_contents` give it no entry,
//! as an empty heading has none ([`DerivedText::only_markers`]: it held a marker and leaves no text but
//! white space); a gap it holds is reported on the result. A marker with a value is text, so a heading
//! whose only marker has one keeps its entry. A section marker at those calls has no readable value, so
//! alone it is this case, with its gap. White space with no marker stays an entry, as before.
//!
//! Studio readings; InDesign's handling of markers in a TOC entry UNVERIFIED (D5).
//!
//! Hostile input: only a marker with a value or a gap allocates beyond the input's own size, at most
//! `MAX_RESOLVED_MARKER_BYTES` bytes each, and one generator call takes at most `MAX_STORY_MARKERS` of
//! them ([`DerivedBudget`]); over it the call refuses with its own budget error
//! (`v4_02_the_budget_is_the_aggregate_of_one_call`).
#![deny(clippy::arithmetic_side_effects)]
use crate::numbering::PageNumber;
use crate::sentinels::{FLUSH_SPACE, MAX_RESOLVED_MARKER_BYTES, MAX_STORY_MARKERS, MarkerKind, MarkerPlace, UnresolvedMarker, ends_paragraph, marker_value};
use crate::toc::{TocSource, blank_line_of, line_of};
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
    /// The span held a marker of any group, shown or not.
    held_marker: bool,
}
impl DerivedText {
    /// It held a marker and leaves no text but white space (module docs).
    pub(crate) fn only_markers(&self) -> bool {
        self.held_marker && self.text.trim().is_empty()
    }
}

/// The value of a page-number or section marker at story byte `at` (module docs): where the byte is
/// placed in `source` (`toc::line_of`, or the blank line of a marker-only paragraph), then the one rule, `sentinels::marker_value`. A call with no
/// sections cannot read a section marker that is placed.
fn value_at<'a>(
    kind: MarkerKind,
    at: u64,
    source: Option<&TocSource<'_>>,
    pages: Option<&DerivedPages<'a>>,
) -> Result<&'a str, DerivedGapReason> {
    let placed = source.zip(pages).and_then(|(source, d)| {
        let (frame, _) = line_of(source, at).or_else(|| blank_line_of(source, at))?;
        let page: &'a PageNumber = d.by_page.get(&(frame.spread, frame.page)).copied()?;
        Some((MarkerPlace::Page { page, pages: d.pages }, d.sections))
    });
    let (place, sections) = match placed {
        Some((place, Some(sections))) => (place, sections),
        Some((_, None)) if kind == MarkerKind::SectionMarker => return Err(DerivedGapReason::SectionsUnavailable),
        Some((place, None)) => (place, &[][..]),
        None => (MarkerPlace::NotPlaced, &[][..]),
    };
    marker_value(kind, place, sections, None, MAX_RESOLVED_MARKER_BYTES).map_err(DerivedGapReason::Marker)
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
    let mut out = DerivedText { text: String::with_capacity(body.len()), gaps: Vec::new(), held_marker: false };
    for (i, c) in body.char_indices() {
        let is_space = (c == '\u{2028}' && rules.forced_break_to_space) || (c == '\t' && rules.tab_to_space) || c == FLUSH_SPACE || ends_paragraph(c);
        if is_space {
            out.text.push(' ');
            continue;
        }
        if c == ANCHORED_OBJECT {
            out.held_marker = true;
            continue;
        }
        if !ty::is_sentinel_block(c) {
            out.text.push(c);
            continue;
        }
        out.held_marker = true;
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
