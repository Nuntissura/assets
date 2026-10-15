//! Publication/page/threaded-frame composition, overset/preflight, tables and multilingual
//! paragraph algorithms. Slice 1: threaded-frame reflow with overset detection, and a first-fit plus
//! a Knuth-Plass line breaker over Type-shaped Latin left-to-right paragraphs. Pages, text frames,
//! thread links and the story are Folio's (`StudioPageSpread::pages`, `StudioLayer::text_frame`,
//! `StudioTextStory`); layout reads them and keeps no model of its own. Anything else is refused,
//! never mis-laid.
#![forbid(unsafe_code)]

mod breaker;
mod fitting;
mod folio_input;
mod geometry;
mod hyphenate;
mod numbering;
mod paragraph;
mod parent_adapter;
mod parents;
mod styles;
mod table;
mod threaded_reflow;
mod toc;
mod typeset;

pub use breaker::{Composer, KnuthPlassParams};
pub use fitting::{Crop, FitError, Fitted, PlacedGraphic, fit_content, place_graphic};
pub use folio_input::FrameObstacles;
pub use geometry::Rect;
pub use hyphenate::{
    EN_US_LICENCE, HyphenationError, HyphenationParams, Hyphenator, MAX_WORD_BYTES, hyphenated,
};
pub use paragraph::{
    Break, BreakKind, LayoutGlyph, ShapedParagraph, SpaceParams, break_opportunities,
};
pub use parent_adapter::{
    LayerAssignment, LayerRects, ParentAdapterError, parent_set_from_records,
    records_from_parent_set, resolve_page_from_records,
};
pub use parents::{
    ItemOverride, ItemSource, PageBinding, ParentItem, ParentItemKind, ParentPage, ParentSet,
    ResolvedItem,
};
pub use styles::{
    Alignment, ParagraphStyling, ResolvedStyle, StyleSource, length_pt, resolve_style,
};
pub use threaded_reflow::{
    FramePlacement, LayoutProposal, Limits, Line, ReflowRequest, reflow, reflow_typeset,
};
pub use table::{
    FlowPosition, PieceKind, PlacedCell, PlacedRow, StrokeSegment, TableError, TableLimits,
    TablePart, TableProposal, TableRequest, layout_table, tint_rgb,
};
pub use numbering::{NumberScope, NumberingError, PageNumber, format_number, last_page_label, number_pages};
pub use toc::{GeneratedStory, TocEntryOut, TocError, TocLimits, TocSource, generate_toc};
pub use typeset::{JustifyParams, Justification, Spacing, TypesetOptions, justify_metrics};

use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{FailureCode, Observation, Observe, Outcome, Receipt, SinkPort};

/// Half-open byte range of the story text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceSpan {
    pub start: u64,
    pub end: u64,
}
impl From<hsk_studio_type::SourceRange> for SourceSpan {
    fn from(r: hsk_studio_type::SourceRange) -> Self {
        Self {
            start: r.start,
            end: r.end,
        }
    }
}

/// Typed refusals. A refusal never carries a partial proposal and never alters the story input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutError {
    StaleRevision { expected: u64, current: u64 },
    CyclicThread,
    InvalidGeometry,
    InvalidInput,
    MissingFont,
    UnsupportedComposition,
    BudgetExceeded,
    Canceled,
}
impl LayoutError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::StaleRevision { .. } => "stale_revision",
            Self::CyclicThread => "cyclic_thread",
            Self::InvalidGeometry => "invalid_geometry",
            Self::InvalidInput => "invalid_input",
            Self::MissingFont => "missing_font",
            Self::UnsupportedComposition => "unsupported_composition",
            Self::BudgetExceeded => "budget_exceeded",
            Self::Canceled => "canceled",
        }
    }
    /// `None` for cancellation (reported as the Canceled outcome).
    pub const fn failure_code(self) -> Option<FailureCode> {
        match self {
            Self::Canceled => None,
            Self::MissingFont => Some(FailureCode::Unavailable),
            Self::UnsupportedComposition => Some(FailureCode::Unsupported),
            _ => Some(FailureCode::Validation),
        }
    }
}
impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for LayoutError {}

/// One Observe terminal outcome per call. Overset is a finding inside a successful proposal.
pub fn report<T>(
    result: &Result<T, LayoutError>,
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

pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-LAYOUT","version":1,"operation":"reflow: one Folio StudioTextStory of Type-shaped Latin left-to-right paragraphs flows through the ordered chain of Folio text frames (StudioLayer.text_frame, previous/next links) on Folio page spreads and returns a revision-bound placement proposal in points with an exact overset span; first-fit and Knuth-Plass line breakers; UAX #14 break opportunities","operator":{"intent":"Place a story into threaded frames and see exactly what does not fit","result":"LayoutProposal {base_revision, frames[{frame_id, spread origin, lines[{baseline, x_start, width, available, hang, source byte span, glyph range, overfull, ratio}]}], overset} or a typed refusal","recovery":"stale_revision: reload and re-run; cyclic_thread: break the loop in the Folio previous_text_frame/next_text_frame links; invalid_geometry: use finite positive Folio Lengths in pt, pages of positive size and a fixed first baseline inside the frame inset; invalid_input: the story needs exactly one frame chain in the document, every link and page must exist, stored content needs resolved_text of byte_len bytes, byte spans must lie on char boundaries of the story text and inside the story, cluster starts must not decrease; missing_font: resolve the font in Type first; unsupported_composition: right-to-left, CJK, Arabic or other non-Latin runs, frame_grid stories, more than one text column, vertical justification other than top, auto-sizing and a first baseline other than fixed are not laid out in this slice; budget_exceeded: raise Limits or shorten the story; canceled: nothing was produced"},"model":{"units":"points (Folio Length unit pt); frame lines in frame coordinates (origin = frame top-left, y down), frame origin in spread coordinates (pages left to right, no gutter)","composers":"FirstFit = greedy single-line composer; KnuthPlass = optimum fit over the paragraph with badness 100*|r|^3, demerits (line_penalty+badness)^2, fitness classes tight/normal/loose/very loose, three passes (tolerance 200, 10000, overfull allowed); relation to the Adobe Paragraph Composer UNVERIFIED","text_flow":"byte offsets of the story text; hanging trailing whitespace belongs to the line glyph range but not to width_pt; no justification, hyphenation, columns, tables, pages or contour wrap in this slice","determinism":"identical input yields an identical proposal; no hashing, ties resolve to the earlier candidate"},"inputs":"ReflowRequest {document: Folio StudioDocument (revision is the current revision), story: Folio StudioTextStory, resolved_text (stored content only), paragraphs: ShapedParagraph[] (from_type adapts a Type ParagraphResult), obstacles: FrameObstacles[] (text wrap is not stored in Folio), composer, space, expected_revision, limits}","limits":{"max_lines":100000,"max_glyphs":1048576,"max_paragraphs":65536},"cancel":"CancellationToken is checked at entry, per paragraph, per 256 line-slot scans and per 256 break candidates","consumer":"layout-consumer [--descriptor] [--advance PT] [--text TEXT] [--frame WxH]... [--composer first-fit|knuth-plass] [--cancel]","argus":{"inspect":"LayoutProposal fields are plain data","action":"the same reflow call with the same token","capture":"caller captures the proposal and Observe frame bytes under a fresh grant"},"diagnostics":"report() emits one Observe terminal outcome; no text, ids or paths are serialized by layout","authority":"Pages, text frames, thread links and stories are Folio contracts read as given (overflow stays layout output and is not written to the document); host grants, history and acceptance remain pending","longdoc":{"lane":"SMB-LAYOUT-LONGDOC","records":"Folio hsk.studio.longdoc_records@1 (StudioLongDocRecords beside the document; validate_longdoc_records)","tables":"layout_table(TableRequest {document, table: Folio StudioTable, host_story (its frame thread carries the table), cell_story + cell_paragraphs (Type-shaped), start: Option<FlowPosition>, composer, space, expected_revision, limits: TableLimits}, TypesetOptions, token) -> TableProposal {parts (frame, rows: header|body|split|footer pieces in frame coordinates), cells (rect, fill, tint), strokes (resolved by edge priority, ties to the lower/right cell, merged, drawing order by stroke_order), text: LayoutProposal of the cell story, overset_row, overset_cells, end: FlowPosition, notes} or TableError {layout codes, grid: table_empty|row_order|span_zero|span_overflow|span_crosses_row_type|cells_overlap|cell_missing, table_too_large (checked before allocation), cell_content_not_on_paragraphs}; headers repeat per frame or per page (skip_first_header), footers follow the last body row of a part (skip_last_footer), rows break between frames, row-span and keep_with_next groups move together, start_row next frame/page/odd/even, allow_split rows split at line boundaries; tests tests/longdoc_tables.rs","parity":"InDesign behaviour UNVERIFIED (D5)","graphics":"fit_content(frame Rect, content_w, content_h, fit: Option<StudioFit>, stored: Option<StudioFit>, anchor: StudioFitAnchor, crop: Crop) -> Fitted {frame, transform [sx,0,0,sy,tx,ty] content pt -> frame, content_box}; place_graphic(document, Folio StudioGraphicFrame) -> PlacedGraphic in spread coordinates; members: content_to_frame, proportionally, fill_proportionally (anchor-positioned), center_content, preserve_sizes, frame_to_content (anchor point of the frame kept), apply_frame_fitting_options (stored rule), cleared = 100 % at the frame origin; FitError content_aware_unavailable | fit_not_legal_here | no_stored_fitting | layout invalid_geometry/invalid_input; tests tests/longdoc_graphics.rs","numbering":"number_pages(document, Folio sections) -> [PageNumber {page_id, spread, page, ordinal, section, number, label = prefix (include_section_prefix) + number in the section style}] after Folio check_sections (prefix <= 5 chars without , or +, unique prefixes, one start per page, first page starts a section, page_number_start 1..=999999 only when not continuing, empty name, page-legal styles); format_number(n, StudioNumberStyle): arabic, roman 1..=3999, letters (repeat form, <= 64), leading zeros 2/3/4 digits, full-width, kanji < 10^9, symbols, asterisks, format_none; abjad/alif-ba-tah/Hebrew/katakana number_style_unsupported, current number_style_not_legal_here; last_page_label(document|section scope); tests tests/longdoc_numbering_toc.rs","toc":"generate_toc(document, Folio StudioTocStyle, [TocSource {story, text, placement}], pages, target_story_id, TocLimits, token) -> GeneratedStory {text, Folio paragraphs with format/title style refs, entries [{level, text, page_label, page_id, story_id, source_start}]} (to_story(id) gives a StudioTextStory to place and compose); page of a paragraph = page of the frame of the line holding its start; overset paragraphs only with include_overset (no label); levels nest, siblings sorted when sort_alphabet; TocError toc_loop (target story is a source, or a format/title style is a mapped source style), Folio check_toc_style codes (toc_no_entries, toc_style_mapped_twice, ...), budget_exceeded (entries / text bytes, checked before each push)"}}"#;
