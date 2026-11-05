//! Stream sentinels at composition (STU-TYP-207 with the v02.228 SD-29 assignment; lane
//! SMB-LAYOUT-SENTINELS-1, R52 finding F-R52-1). Type stores the sentinels, ends paragraphs at the
//! break sentinels and refuses to shape marker and flush sentinels (MissingGlyph); resolving them is
//! layout's job.
//!
//! - Paragraph partition ([`paragraph_partition`], [`ends_paragraph`]): a paragraph ends after every
//!   character of UAX #9 bidi class B (U+000A, U+000D, U+001C..U+001E, U+0085, U+2029) and after
//!   every break sentinel U+FDD8..U+FDDC, terminator included, as Type's request validation does
//!   ([`ends_paragraph`] is Type's public rule, lane SMB-SENTINELS-NATIVE-1; no second copy);
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
use crate::folio_input::story_text;
use crate::numbering::{NumberingError, PageNumber, number_pages};
use crate::textflow::MAX_STORY_BYTES;
use crate::threaded_reflow::{LayoutProposal, Limits, ReflowRequest, reflow_facts};
use crate::{
    Break, BreakKind, Composer, FrameObstacles, LayoutError, LayoutGlyph, ShapedParagraph, SourceSpan, SpaceParams,
    TypesetOptions, break_opportunities,
};
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

/// True for a character that ends its paragraph: Type's own rule, `hsk_studio_type::ends_paragraph`
/// (module docs), so layout's partition is the one Type's request validation requires.
pub use hsk_studio_type::ends_paragraph;

/// The paragraph partition of `text` (module docs): consecutive spans, each ending after its
/// terminator; text after the last terminator is the last paragraph; empty text has none.
pub fn paragraph_partition(text: &str) -> Vec<SourceSpan> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, c) in text.char_indices() {
        if ends_paragraph(c) {
            let end = i.saturating_add(c.len_utf8());
            out.push(SourceSpan { start: start as u64, end: end as u64 });
            start = end;
        }
    }
    if start < text.len() {
        out.push(SourceSpan { start: start as u64, end: text.len() as u64 });
    }
    out
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
    cancel.check().map_err(|_| LayoutError::Canceled)?;
    if req.expected_revision != req.document.revision {
        return Err(LayoutError::StaleRevision { expected: req.expected_revision, current: req.document.revision }.into());
    }
    let budget = req.budget;
    if budget.max_passes == 0 {
        return Err(LayoutError::InvalidInput.into());
    }
    let text = story_text(req.story, req.resolved_text)?;
    if text.len() > MAX_STORY_BYTES {
        return Err(LayoutError::BudgetExceeded.into());
    }
    // Marker and flush sentinels in story order, counted before anything is allocated per marker.
    let (mut markers, mut flush): (Vec<(u64, MarkerKind)>, Vec<u64>) = (Vec::new(), Vec::new());
    for (i, c) in text.char_indices().filter(|(_, c)| ty::is_sentinel_block(*c)) {
        let kind = MarkerKind::from_sentinel(c);
        if kind.is_none() && c != FLUSH_SPACE {
            continue;
        }
        if markers.len().saturating_add(flush.len()) >= budget.max_markers {
            return Err(LayoutError::BudgetExceeded.into());
        }
        match kind {
            Some(k) => markers.push((i as u64, k)),
            None => flush.push(i as u64),
        }
    }
    let records = check_records(req.markers, &markers)?;
    let partition = paragraph_partition(text);
    if partition.len() > req.limits.max_paragraphs {
        return Err(LayoutError::BudgetExceeded.into());
    }
    let pages = number_pages(req.document, req.sections);
    let page_ids: Vec<&str> =
        req.document.page_spreads.iter().flat_map(|s| s.pages.iter().map(|p| p.page_id.as_str())).collect();
    let mut spread_base = Vec::with_capacity(req.document.page_spreads.len());
    let mut base = 0usize;
    for s in &req.document.page_spreads {
        spread_base.push(base);
        base = base.saturating_add(s.pages.len());
    }
    let ordinal = |spread: usize, page: usize| spread_base.get(spread).and_then(|b| b.checked_add(page));
    let parity = |spread: usize, page: usize| -> Option<u32> {
        let p = pages.as_ref().ok()?.get(ordinal(spread, page)?)?;
        (p.spread == spread && p.page == page).then_some(p.number)
    };
    let mut states = vec![MarkerState::Unresolved(UnresolvedMarker::NotPlaced); markers.len()];
    for pass in 1..=budget.max_passes {
        cancel.check().map_err(|_| LayoutError::Canceled)?;
        let mut events: Vec<(u64, &str)> = markers
            .iter()
            .zip(&states)
            .map(|((at, _), s)| (*at, if let MarkerState::Resolved(t) = s { t.as_str() } else { "" }))
            .chain(flush.iter().map(|f| (*f, "")))
            .collect();
        events.sort_unstable_by_key(|e| e.0);
        let shaped = shape_story(text, &partition, &events, &flush, shaper, cancel)?;
        let reflow = ReflowRequest {
            document: req.document,
            story: req.story,
            resolved_text: req.resolved_text,
            paragraphs: &shaped,
            obstacles: req.obstacles,
            composer: req.composer,
            space: req.space,
            expected_revision: req.expected_revision,
            limits: req.limits,
        };
        let (layout, facts) = reflow_facts(&reflow, opts, flow, &parity, cancel)?;
        let placed = place_markers(text, &layout, &partition, &markers, &ordinal);
        let next: Vec<MarkerState> = markers
            .iter()
            .zip(&placed)
            .zip(&records)
            .map(|(((_, kind), at), record)| resolve(*kind, *at, &pages, req.sections, *record, budget.max_marker_text_bytes))
            .collect();
        if next == states {
            let markers = markers
                .iter()
                .zip(placed)
                .zip(next)
                .map(|(((story_byte, kind), at), state)| MarkerOutcome {
                    story_byte: *story_byte,
                    kind: *kind,
                    page_id: at.and_then(|o| page_ids.get(o)).map(|s| (*s).to_owned()),
                    state,
                })
                .collect();
            return Ok(SentinelComposition {
                layout,
                paragraphs: shaped,
                markers,
                breaks: facts.breaks,
                flush_spaces: facts.flush,
                passes: pass,
            });
        }
        states = next;
    }
    Err(SentinelError::MarkersUnstable)
}

/// One record per marker (by index), checked: the byte holds a marker of the record's kind, at most
/// one record per marker, and the target the kind takes (`Generated` exactly for note and
/// cross-reference markers).
fn check_records<'r>(
    records: &'r [MarkerRecord],
    markers: &[(u64, MarkerKind)],
) -> Result<Vec<Option<&'r MarkerRecord>>, SentinelError> {
    let mut out: Vec<Option<&MarkerRecord>> = vec![None; markers.len()];
    for r in records {
        let i = markers.binary_search_by_key(&r.story_byte, |m| m.0).map_err(|_| SentinelError::MarkerRecordInvalid)?;
        let generated = matches!(r.target, MarkerTarget::Generated { .. });
        let takes_generated =
            matches!(r.kind, MarkerKind::FootnoteReference | MarkerKind::EndnoteReference | MarkerKind::CrossReference);
        let slot = out.get_mut(i).ok_or(SentinelError::MarkerRecordInvalid)?;
        if markers.get(i).map(|m| m.1) != Some(r.kind) || generated != takes_generated || slot.is_some() {
            return Err(SentinelError::MarkerRecordInvalid);
        }
        *slot = Some(r);
    }
    Ok(out)
}

/// Where a marker's value is read from.
#[derive(Clone, Copy, Debug)]
pub(crate) enum MarkerPlace<'a> {
    /// In overset text, or on no page.
    NotPlaced,
    /// Placed, but the page labels could not be produced.
    NoPages(NumberingError),
    /// On `page` of the document's `pages` (`PageNumber::ordinal` finds its neighbours).
    Page { page: &'a PageNumber, pages: &'a [PageNumber] },
}

/// The document page with 1-based ordinal `ordinal`.
fn page_with_ordinal(pages: &[PageNumber], ordinal: usize) -> Option<&PageNumber> {
    pages.get(pages.partition_point(|p| p.ordinal < ordinal)).filter(|p| p.ordinal == ordinal)
}

/// The one rule of a marker's value, composition's ([`resolve`]) and derived text's
/// (`derived_text`), module docs: `Ok` is the text to show, `Err` the explicit state, never a guessed
/// value. A marker that is not placed has no value whatever its kind.
pub(crate) fn marker_value<'a>(
    kind: MarkerKind,
    place: MarkerPlace<'a>,
    sections: &'a [StudioSection],
    record: Option<&'a MarkerRecord>,
    max_bytes: usize,
) -> Result<&'a str, UnresolvedMarker> {
    use UnresolvedMarker as U;
    let here = match place {
        MarkerPlace::NotPlaced => return Err(U::NotPlaced),
        MarkerPlace::NoPages(e) => Err(U::Numbering(e)),
        MarkerPlace::Page { page, pages } => Ok((page, pages)),
    };
    let text = match kind {
        MarkerKind::Index => return Err(U::RuleUnstated),
        MarkerKind::FootnoteReference | MarkerKind::EndnoteReference | MarkerKind::CrossReference => {
            match record.map(|r| &r.target) {
                Some(MarkerTarget::Generated { text, .. }) => text.as_str(),
                _ => return Err(U::RecordMissing),
            }
        }
        MarkerKind::CurrentPageNumber => here?.0.label.as_str(),
        MarkerKind::NextPageNumber => {
            let (page, pages) = here?;
            let next = page.ordinal.checked_add(1).and_then(|o| page_with_ordinal(pages, o));
            next.ok_or(U::NoNextPage)?.label.as_str()
        }
        MarkerKind::PreviousPageNumber => {
            let (page, pages) = here?;
            let previous = page.ordinal.checked_sub(1).and_then(|o| page_with_ordinal(pages, o));
            previous.ok_or(U::NoPreviousPage)?.label.as_str()
        }
        MarkerKind::SectionMarker => {
            let section = here?.0.section.ok_or(U::NoSection)?;
            sections.get(section).and_then(|s| s.marker.as_deref()).ok_or(U::NoSectionMarker)?
        }
    };
    if text.len() > max_bytes {
        return Err(U::TextOverBudget);
    }
    if text.chars().any(|c| ends_paragraph(c) || ty::is_sentinel_block(c) || c == '\u{FFFC}') {
        return Err(U::TextNotInline);
    }
    Ok(text)
}

/// The state of one marker placed on document page `at` (index into `pages`, which holds every page
/// of the document, so `at` is always in it): [`marker_value`].
fn resolve(
    kind: MarkerKind,
    at: Option<usize>,
    pages: &Result<Vec<PageNumber>, NumberingError>,
    sections: &[StudioSection],
    record: Option<&MarkerRecord>,
    max_bytes: usize,
) -> MarkerState {
    let place = match (at, pages) {
        (None, _) => MarkerPlace::NotPlaced,
        (Some(_), Err(e)) => MarkerPlace::NoPages(*e),
        (Some(i), Ok(pages)) => pages.get(i).map_or(MarkerPlace::NotPlaced, |page| MarkerPlace::Page { page, pages }),
    };
    match marker_value(kind, place, sections, record, max_bytes) {
        Ok(t) => MarkerState::Resolved(t.to_owned()),
        Err(u) => MarkerState::Unresolved(u),
    }
}

/// UAX #14 opportunities (story bytes) of the last paragraph a [`line_holding`] lookup needed.
pub(crate) type BreakCache = Option<(SourceSpan, Vec<Break>)>;

/// The line holding story byte `at`: an index into `lines`, the placed lines of `paragraph` in
/// flow order (`source` reads a line's source span); `None` when `at` is in the overset text, the
/// paragraph has no placed line, or `at` belongs to the overset line that follows. A byte inside a
/// line's span belongs to it, a byte before the paragraph's first glyph to its first line. A byte
/// shaped as nothing that lies after a line's last glyph and before the next line's first glyph
/// (placed or overset) sits where the line broke: it belongs to the next line when the break
/// opportunity taken there (UAX #14 over the stored paragraph text; the first mandatory one in that
/// gap, else the first allowed one) is at or before it, so it is glued to the next word, else to the
/// line before (R54 F-R54-6; Studio reading, InDesign placement UNVERIFIED, D5). Without a next
/// line it stays with the line before (the paragraph's trailing glyph-less bytes and terminator).
/// A blank line (`paragraph::blank_lines`) spans its bytes, so a byte on it is inside its span, and
/// its end is a mandatory break, so a byte right after it goes to the next line by the same rule.
/// The one rule of every byte-to-line lookup of layout: markers here, note references
/// (`footnotes::locate`), TOC source paragraphs and index marks (`toc::line_of`), cross-reference
/// destinations (`xref`; lane SMB-LAYOUT-MARKER-LOOKUPS-1, V4-06).
pub(crate) fn line_holding<T>(
    text: &str,
    paragraph: SourceSpan,
    lines: &[T],
    source: impl Fn(&T) -> SourceSpan,
    overset: Option<SourceSpan>,
    at: u64,
    cache: &mut BreakCache,
) -> Option<usize> {
    if overset.is_some_and(|o| at >= o.start) {
        return None;
    }
    let k = lines.partition_point(|l| source(l).start <= at);
    let i = k.saturating_sub(1);
    let line = source(lines.get(i)?);
    if k == 0 || at < line.end {
        return Some(i);
    }
    // The next line of the paragraph: placed, else the overset line starting in it.
    let next = match lines.get(k) {
        Some(n) => Some((source(n).start, Some(k))),
        None => overset.filter(|o| o.start > at && o.start < paragraph.end).map(|o| (o.start, None)),
    };
    let Some((next_start, next_line)) = next else {
        return Some(i);
    };
    if cache.as_ref().is_none_or(|(p, _)| *p != paragraph) {
        let (s, e) = (usize::try_from(paragraph.start).ok()?, usize::try_from(paragraph.end).ok()?);
        let found = break_opportunities(text.get(s..e)?)
            .into_iter()
            .filter_map(|b| Some(Break { byte: b.byte.checked_add(s)?, kind: b.kind }))
            .collect();
        *cache = Some((paragraph, found));
    }
    let gap = cache.as_ref().map_or(&[][..], |(_, o)| o.as_slice());
    let in_gap = |b: &&Break| (line.end..=next_start).contains(&(b.byte as u64));
    let taken = gap.iter().filter(in_gap).find(|b| b.kind == BreakKind::Mandatory).or_else(|| gap.iter().find(in_gap));
    if taken.is_some_and(|b| b.byte as u64 <= at) { next_line } else { Some(i) }
}

/// The document page (ordinal index) of each marker: the page of the line holding it
/// ([`line_holding`] over the lines of its paragraph); `None` in overset text.
fn place_markers(
    text: &str,
    layout: &LayoutProposal,
    partition: &[SourceSpan],
    markers: &[(u64, MarkerKind)],
    ordinal: &dyn Fn(usize, usize) -> Option<usize>,
) -> Vec<Option<usize>> {
    // Lines in flow order (paragraph index and source start are non-decreasing in it).
    let lines: Vec<(usize, SourceSpan, usize, usize)> = layout
        .frames
        .iter()
        .flat_map(|f| f.lines.iter().map(move |l| (l.paragraph, l.source, f.spread, f.page)))
        .collect();
    let mut cache: BreakCache = None;
    markers
        .iter()
        .map(|&(at, _)| {
            if layout.overset.is_some_and(|o| at >= o.start) {
                return None;
            }
            let pi = partition.partition_point(|p| p.end <= at);
            let lo = lines.partition_point(|l| l.0 < pi);
            let hi = lines.partition_point(|l| l.0 <= pi);
            let mine = lines.get(lo..hi)?;
            let paragraph = *partition.get(pi)?;
            let line = mine.get(line_holding(text, paragraph, mine, |l| l.1, layout.overset, at, &mut cache)?)?;
            ordinal(line.2, line.3)
        })
        .collect()
}

/// A 3-byte sentinel at story byte `story` became `len` bytes of composition text at `comp`.
#[derive(Clone, Copy, Debug)]
struct Sub {
    story: u64,
    comp: u64,
    len: u64,
}
const SENTINEL_LEN: u64 = 3;
fn sub_story_end(s: &Sub) -> u64 {
    s.story.saturating_add(SENTINEL_LEN)
}
fn sub_comp_end(s: &Sub) -> u64 {
    s.comp.saturating_add(s.len)
}
/// Story byte `y` (never inside a sentinel) in composition coordinates.
fn to_comp(subs: &[Sub], y: u64) -> Option<u64> {
    let k = subs.partition_point(|s| sub_story_end(s) <= y);
    let (s, c) = k.checked_sub(1).and_then(|i| subs.get(i)).map_or((0, 0), |p| (sub_story_end(p), sub_comp_end(p)));
    c.checked_add(y.checked_sub(s)?)
}
/// Composition byte `x` in story coordinates: inside a substituted marker text it is the sentinel's
/// start (`end` false) or end (`end` true); at an empty substitution a start lies after the
/// sentinel and an end before it.
fn to_story(subs: &[Sub], x: u64, end: bool) -> Option<u64> {
    let before = |s: &Sub| {
        let e = sub_comp_end(s);
        e < x || (e == x && (s.len > 0 || !end))
    };
    let k = subs.partition_point(before);
    if let Some(sub) = subs.get(k)
        && sub.comp < x
        && x < sub_comp_end(sub)
    {
        return Some(if end { sub_story_end(sub) } else { sub.story });
    }
    let (s, c) = k.checked_sub(1).and_then(|i| subs.get(i)).map_or((0, 0), |p| (sub_story_end(p), sub_comp_end(p)));
    s.checked_add(x.checked_sub(c)?)
}

/// Builds the composition text (each event's sentinel replaced by its text), shapes every paragraph
/// through the host port and maps the result back to story coordinates, with one zero-advance glyph
/// per flush space.
fn shape_story(
    text: &str,
    partition: &[SourceSpan],
    events: &[(u64, &str)],
    flush: &[u64],
    shaper: &mut dyn SentinelShaper,
    cancel: &CancellationToken,
) -> Result<Vec<ShapedParagraph>, LayoutError> {
    let bad = || LayoutError::InvalidInput;
    let mut comp = String::with_capacity(text.len());
    let mut subs = Vec::with_capacity(events.len());
    let mut last = 0usize;
    for &(at, repl) in events {
        let at_u = usize::try_from(at).map_err(|_| bad())?;
        comp.push_str(text.get(last..at_u).ok_or_else(bad)?);
        subs.push(Sub { story: at, comp: comp.len() as u64, len: repl.len() as u64 });
        comp.push_str(repl);
        last = at_u.checked_add(3).ok_or(LayoutError::BudgetExceeded)?;
        if comp.len() > MAX_STORY_BYTES {
            return Err(LayoutError::BudgetExceeded);
        }
    }
    comp.push_str(text.get(last..).ok_or_else(bad)?);
    if comp.len() > MAX_STORY_BYTES {
        return Err(LayoutError::BudgetExceeded);
    }
    let comp_len = comp.len() as u64;
    let mut out = Vec::with_capacity(partition.len());
    for (index, p) in partition.iter().enumerate() {
        if index.is_multiple_of(64) {
            cancel.check().map_err(|_| LayoutError::Canceled)?;
        }
        let paragraph = SourceSpan { start: to_comp(&subs, p.start).ok_or_else(bad)?, end: to_comp(&subs, p.end).ok_or_else(bad)? };
        let mut shaped = shaper.shape(&ShapeInput { text: &comp, paragraph, index })?;
        let map = |s: SourceSpan| -> Result<SourceSpan, LayoutError> {
            if s.start > s.end || s.end > comp_len {
                return Err(bad());
            }
            let m = SourceSpan { start: to_story(&subs, s.start, false).ok_or_else(bad)?, end: to_story(&subs, s.end, true).ok_or_else(bad)? };
            if m.start > m.end { Err(bad()) } else { Ok(m) }
        };
        shaped.source = *p;
        for g in &mut shaped.glyphs {
            g.cluster = map(g.cluster)?;
        }
        for l in &mut shaped.ligature_parts {
            l.source = map(l.source)?;
        }
        let (lo, hi) = (flush.partition_point(|f| *f < p.start), flush.partition_point(|f| *f < p.end));
        for &f in flush.get(lo..hi).unwrap_or(&[]) {
            let at = shaped.glyphs.partition_point(|g| g.cluster.start <= f);
            let cluster = SourceSpan { start: f, end: f.checked_add(SENTINEL_LEN).ok_or_else(bad)? };
            shaped.glyphs.insert(at, LayoutGlyph { cluster, advance_pt: 0.0 });
            for a in &mut shaped.line_end_advances {
                if a.glyph >= at {
                    a.glyph = a.glyph.checked_add(1).ok_or_else(bad)?;
                }
            }
            for l in &mut shaped.ligature_parts {
                if l.glyph >= at {
                    l.glyph = l.glyph.checked_add(1).ok_or_else(bad)?;
                }
            }
        }
        out.push(shaped);
    }
    Ok(out)
}
