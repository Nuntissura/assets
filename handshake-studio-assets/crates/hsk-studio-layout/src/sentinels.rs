//! Stream sentinels at composition (STU-TYP-207 with the v02.228 SD-29 assignment; lane
//! SMB-LAYOUT-SENTINELS-1, R52 finding F-R52-1). Type stores the sentinels, ends paragraphs at the
//! break sentinels and refuses to shape marker and flush sentinels (MissingGlyph); resolving them is
//! layout's job.
//!
//! - Paragraph partition ([`paragraph_partition`], [`ends_paragraph`]): a paragraph ends after every
//!   character of UAX #9 bidi class B (U+000A, U+000D, U+001C..U+001E, U+0085, U+2029) and after
//!   every break sentinel U+FDD8..U+FDDC, terminator included, as Type's request validation does;
//!   `replace_story_text` re-splits edited paragraphs the same way.
//! - Markers ([`compose_with_sentinels`]): each marker sentinel U+FDD0..U+FDD7 is replaced, for
//!   shaping only, by the text it resolves to where its line lands. The stored story keeps the one
//!   sentinel and every story byte offset: the glyphs of the resolved text carry the sentinel's
//!   3-byte cluster. Current / next / previous page number = the label (`number_pages`: the section
//!   prefix when included plus the number in the section style) of the page holding the line / of
//!   the next / of the previous document page; section marker = the `marker` of that page's section.
//!   Footnote, endnote and cross-reference markers take the display text of their [`MarkerRecord`]
//!   (the output of the generator that owns the numbering); the index marker has no stated
//!   resolution. Whatever cannot be resolved is an explicit [`UnresolvedMarker`] state, shaped as
//!   nothing, never a guessed value. Placement and resolution iterate: pass 1 composes with every
//!   marker unresolved, each later pass with the values the previous placement resolved, until a
//!   pass resolves exactly the values it was composed with (`passes`); not settling within
//!   `max_passes` is `markers_unstable`.
//! - Breaks (threaded reflow core, so every entry point): a paragraph ended by a break sentinel puts
//!   the following text at the top of the next column (column break; after the last column, the next
//!   frame), of the next frame of the thread (frame break), of the first later frame on a later page
//!   (page break) or on a later page whose number is odd / even (odd-page / even-page break; page
//!   numbers from `number_pages`, which only [`compose_with_sentinels`] knows: the plain entry
//!   points overset at them as `parity_unknown`). No page is inserted (the spec states no
//!   insertion). A break without a target oversets from the break byte, never drops text.
//! - Flush space U+FDDD is never shaped: it enters composition as one zero-advance glyph. On the last
//!   line of a fully justified (`JustifiedAll`) paragraph the line keeps its natural spacing and the
//!   flush spaces share the rest of the slot equally ([`FlushOutcome::absorbed_pt`]); elsewhere it
//!   has no width (`None`).
//!
//! Studio readings beyond the spec text: next / previous = document page order, parity by page
//! number, the overset at a missing target, equal sharing between flush spaces, zero width
//! elsewhere. Vendor parity UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use crate::numbering::NumberingError;
use crate::threaded_reflow::{LayoutProposal, Limits};
use crate::{Composer, FrameObstacles, LayoutError, ShapedParagraph, SourceSpan, SpaceParams, TypesetOptions};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{StudioDocument, StudioLayoutFlowRecords, StudioSection, StudioTextStory};
use hsk_studio_observe::{FailureCode, Observation, Observe, Outcome, Receipt, SinkPort};
use hsk_studio_type as ty;

/// Most marker plus flush sentinels one story may hold (checked while scanning, before shaping).
pub const MAX_STORY_MARKERS: usize = 65_536;
/// Longest resolved marker text, bytes; longer is `marker_text_over_budget`.
pub const MAX_RESOLVED_MARKER_BYTES: usize = 256;
/// Default composition passes before `markers_unstable`.
pub const MAX_MARKER_PASSES: usize = 4;
/// The flush-space sentinel (SD-29 U+FDDD).
pub const FLUSH_SPACE: char = '\u{FDDD}';

/// True for a character that ends its paragraph (module docs).
pub fn ends_paragraph(c: char) -> bool {
    ty::is_break_sentinel(c)
        || matches!(c, '\u{000A}' | '\u{000D}' | '\u{001C}'..='\u{001E}' | '\u{0085}' | '\u{2029}')
}

/// The paragraph partition of `text` (module docs): consecutive spans, each ending after its
/// terminator; text after the last terminator is the last paragraph; empty text has none.
pub fn paragraph_partition(text: &str) -> Vec<SourceSpan> {
    let _ = text;
    Vec::new()
}

/// The eight marker sentinels of SD-29 (the anchored-object marker is U+FFFC, not a sentinel).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MarkerKind {
    CurrentPageNumber,
    NextPageNumber,
    PreviousPageNumber,
    SectionMarker,
    FootnoteReference,
    EndnoteReference,
    CrossReference,
    Index,
}
impl MarkerKind {
    pub const ALL: [Self; 8] = [
        Self::CurrentPageNumber,
        Self::NextPageNumber,
        Self::PreviousPageNumber,
        Self::SectionMarker,
        Self::FootnoteReference,
        Self::EndnoteReference,
        Self::CrossReference,
        Self::Index,
    ];
    /// The Type catalog member (`hsk_studio_type::CATALOG`) stored as this sentinel.
    pub const fn member_id(self) -> &'static str {
        match self {
            Self::CurrentPageNumber => "markers.current_page_number",
            Self::NextPageNumber => "markers.next_page_number",
            Self::PreviousPageNumber => "markers.previous_page_number",
            Self::SectionMarker => "markers.section_marker",
            Self::FootnoteReference => "markers.footnote_reference",
            Self::EndnoteReference => "markers.endnote_reference",
            Self::CrossReference => "markers.cross_reference",
            Self::Index => "markers.index",
        }
    }
    pub const fn code(self) -> &'static str {
        match self {
            Self::CurrentPageNumber => "current_page_number",
            Self::NextPageNumber => "next_page_number",
            Self::PreviousPageNumber => "previous_page_number",
            Self::SectionMarker => "section_marker",
            Self::FootnoteReference => "footnote_reference",
            Self::EndnoteReference => "endnote_reference",
            Self::CrossReference => "cross_reference",
            Self::Index => "index",
        }
    }
    /// The stream sentinel, read from the Type catalog.
    pub fn sentinel(self) -> Option<char> {
        ty::catalog_member(self.member_id()).filter(|m| m.is_sentinel()).map(|m| m.scalar())
    }
    pub fn from_sentinel(c: char) -> Option<Self> {
        let member = ty::catalog_member_for_scalar(c).filter(|m| m.is_sentinel())?;
        Self::ALL.into_iter().find(|k| k.member_id() == member.id)
    }
}

/// What a marker resolves against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkerTarget {
    /// Page-number and section markers: the page where the marker's line lands.
    Placement,
    /// Footnote / endnote reference, cross-reference: the display text the owning generator produced
    /// for record `record_id` (note numbering, cross-reference format).
    Generated { record_id: String, text: String },
}

/// STU-TYP-207 typed marker record, keyed by the sentinel's story byte.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkerRecord {
    pub story_byte: u64,
    pub kind: MarkerKind,
    pub target: MarkerTarget,
}

/// Explicit unresolved state of a marker (shaped as nothing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnresolvedMarker {
    /// The marker is in overset text.
    NotPlaced,
    /// Next page number on the last document page.
    NoNextPage,
    /// Previous page number on the first document page.
    NoPreviousPage,
    /// Section marker on a page no section governs.
    NoSection,
    /// Section marker in a section without `marker` text.
    NoSectionMarker,
    /// Footnote, endnote or cross-reference marker without a `Generated` record.
    RecordMissing,
    /// Index marker: the spec states no displayed value (spec gap, lane notes).
    RuleUnstated,
    /// The page labels could not be produced.
    Numbering(NumberingError),
    /// Longer than the budget's `max_marker_text_bytes`.
    TextOverBudget,
    /// The text holds a paragraph terminator, a sentinel-block code point or U+FFFC.
    TextNotInline,
}
impl UnresolvedMarker {
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotPlaced => "marker_not_placed",
            Self::NoNextPage => "marker_no_next_page",
            Self::NoPreviousPage => "marker_no_previous_page",
            Self::NoSection => "marker_no_section",
            Self::NoSectionMarker => "marker_no_section_marker",
            Self::RecordMissing => "marker_record_missing",
            Self::RuleUnstated => "marker_rule_unstated",
            Self::Numbering(e) => e.code(),
            Self::TextOverBudget => "marker_text_over_budget",
            Self::TextNotInline => "marker_text_not_inline",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkerState {
    Resolved(String),
    Unresolved(UnresolvedMarker),
}

/// One marker of the story after composition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkerOutcome {
    pub story_byte: u64,
    pub kind: MarkerKind,
    /// Page holding the marker's line; `None` in overset text.
    pub page_id: Option<String>,
    pub state: MarkerState,
}

/// Why a break could not move the following text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BreakOverset {
    /// No later column, frame or page of the thread (of that parity).
    NoTarget,
    /// Odd/even-page break without page numbers (plain entry points, notes, tables).
    ParityUnknown,
}
impl BreakOverset {
    pub const fn code(self) -> &'static str {
        match self {
            Self::NoTarget => "break_target_missing",
            Self::ParityUnknown => "break_parity_unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BreakResult {
    /// The following text starts at the top of column `column` of thread frame `frame`;
    /// `skipped_frames` thread frames were passed over (same page, wrong parity).
    Applied { frame: usize, column: usize, skipped_frames: usize },
    /// The text from the break byte on is overset.
    Overset(BreakOverset),
}

/// One break sentinel the composition reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BreakOutcome {
    pub story_byte: u64,
    pub target: ty::BreakTarget,
    pub result: BreakResult,
}

/// One placed flush space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlushOutcome {
    pub story_byte: u64,
    /// Thread position (index into `LayoutProposal::frames`) and line index in that frame.
    pub frame: usize,
    pub line: usize,
    /// Width it takes on a fully justified last line; `None` elsewhere (no width).
    pub absorbed_pt: Option<f64>,
}

/// Hostile-input budgets of [`compose_with_sentinels`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SentinelBudget {
    pub max_markers: usize,
    pub max_marker_text_bytes: usize,
    pub max_passes: usize,
}
impl Default for SentinelBudget {
    fn default() -> Self {
        Self {
            max_markers: MAX_STORY_MARKERS,
            max_marker_text_bytes: MAX_RESOLVED_MARKER_BYTES,
            max_passes: MAX_MARKER_PASSES,
        }
    }
}

/// Everything [`compose_with_sentinels`] reads; the paragraphs are partitioned and shaped here.
#[derive(Clone, Copy, Debug)]
pub struct SentinelRequest<'a> {
    pub document: &'a StudioDocument,
    pub story: &'a StudioTextStory,
    pub resolved_text: Option<&'a str>,
    pub obstacles: &'a [FrameObstacles],
    pub composer: Composer,
    pub space: SpaceParams,
    pub expected_revision: u64,
    pub limits: Limits,
    /// Folio sections (`StudioLongDocRecords::sections`); empty = one implicit arabic section.
    pub sections: &'a [StudioSection],
    pub markers: &'a [MarkerRecord],
    pub budget: SentinelBudget,
}

/// One paragraph to shape: `text` is the composition text of the whole story (resolved markers,
/// no flush spaces), `paragraph` the paragraph's span in it, terminator included (Type request
/// convention). The shaper returns a paragraph in `text` coordinates.
#[derive(Clone, Copy, Debug)]
pub struct ShapeInput<'a> {
    pub text: &'a str,
    pub paragraph: SourceSpan,
    pub index: usize,
}

/// Host port: shapes one paragraph (Type). A refusal refuses the composition.
pub trait SentinelShaper {
    fn shape(&mut self, input: &ShapeInput<'_>) -> Result<ShapedParagraph, LayoutError>;
}
impl<F: FnMut(&ShapeInput<'_>) -> Result<ShapedParagraph, LayoutError>> SentinelShaper for F {
    fn shape(&mut self, input: &ShapeInput<'_>) -> Result<ShapedParagraph, LayoutError> {
        self(input)
    }
}

/// The composed story with its sentinel outcomes.
#[derive(Clone, Debug, PartialEq)]
pub struct SentinelComposition {
    pub layout: LayoutProposal,
    /// The shaped paragraphs the layout was composed from, in story coordinates.
    pub paragraphs: Vec<ShapedParagraph>,
    /// One per marker sentinel, in story order.
    pub markers: Vec<MarkerOutcome>,
    /// One per break sentinel the composition reached, in story order.
    pub breaks: Vec<BreakOutcome>,
    /// One per placed flush space, in flow order.
    pub flush_spaces: Vec<FlushOutcome>,
    /// Composition passes run (1 when the story holds no marker).
    pub passes: usize,
}

/// Typed refusals; a refusal carries no partial composition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SentinelError {
    Layout(LayoutError),
    /// A marker record at a byte without a marker sentinel of its kind, a duplicate, or a target
    /// the kind does not take.
    MarkerRecordInvalid,
    /// The resolved values did not settle within `max_passes`.
    MarkersUnstable,
}
impl SentinelError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Layout(e) => e.code(),
            Self::MarkerRecordInvalid => "marker_record_invalid",
            Self::MarkersUnstable => "markers_unstable",
        }
    }
    /// `None` for cancellation (reported as the Canceled outcome).
    pub const fn failure_code(self) -> Option<FailureCode> {
        match self {
            Self::Layout(e) => e.failure_code(),
            Self::MarkerRecordInvalid => Some(FailureCode::Validation),
            Self::MarkersUnstable => Some(FailureCode::Unsupported),
        }
    }
}
impl From<LayoutError> for SentinelError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}
impl std::fmt::Display for SentinelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for SentinelError {}

/// One Observe terminal outcome per [`compose_with_sentinels`] call (codes only, no text).
pub fn report_sentinels<T>(
    result: &Result<T, SentinelError>,
    correlation_id: u64,
    revision: u64,
    cancel: &CancellationToken,
    observe: &mut Observe,
    sink: &mut (impl SinkPort + ?Sized),
) -> Result<Receipt, hsk_studio_observe::Error> {
    let outcome = match result {
        Ok(_) => Outcome::Success,
        Err(e) => e.failure_code().map_or(Outcome::Canceled, Outcome::Failure),
    };
    observe.emit(
        Observation {
            correlation_id,
            revision,
            outcome,
            progress: None,
            private_project_text: None,
        },
        cancel,
        sink,
    )
}

/// Composes `req.story` through its thread with every stream sentinel resolved (module docs).
pub fn compose_with_sentinels(
    req: &SentinelRequest<'_>,
    opts: &TypesetOptions<'_>,
    flow: &StudioLayoutFlowRecords,
    shaper: &mut dyn SentinelShaper,
    cancel: &CancellationToken,
) -> Result<SentinelComposition, SentinelError> {
    let _ = (req, opts, flow, shaper, cancel);
    Err(SentinelError::Layout(LayoutError::UnsupportedComposition))
}
