//! Publication/page/threaded-frame composition, overset/preflight, tables and multilingual
//! paragraph algorithms. Slice 1: threaded-frame reflow with overset detection, and a first-fit plus
//! a Knuth-Plass line breaker over Type-shaped Latin left-to-right paragraphs. Pages, text frames,
//! thread links and the story are Folio's (`StudioPageSpread::pages`, `StudioLayer::text_frame`,
//! `StudioTextStory`); layout reads them and keeps no model of its own. Anything else is refused,
//! never mis-laid.
#![forbid(unsafe_code)]

mod breaker;
mod folio_input;
mod geometry;
mod hyphenate;
mod paragraph;
mod threaded_reflow;

pub use breaker::{Composer, KnuthPlassParams};
pub use folio_input::FrameObstacles;
pub use geometry::Rect;
pub use hyphenate::{
    EN_US_LICENCE, HyphenationError, HyphenationParams, Hyphenator, MAX_WORD_BYTES, hyphenated,
};
pub use paragraph::{
    Break, BreakKind, LayoutGlyph, ShapedParagraph, SpaceParams, break_opportunities,
};
pub use threaded_reflow::{FramePlacement, LayoutProposal, Limits, Line, ReflowRequest, reflow};

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

pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-LAYOUT","version":1,"operation":"reflow: one Folio StudioTextStory of Type-shaped Latin left-to-right paragraphs flows through the ordered chain of Folio text frames (StudioLayer.text_frame, previous/next links) on Folio page spreads and returns a revision-bound placement proposal in points with an exact overset span; first-fit and Knuth-Plass line breakers; UAX #14 break opportunities","operator":{"intent":"Place a story into threaded frames and see exactly what does not fit","result":"LayoutProposal {base_revision, frames[{frame_id, spread origin, lines[{baseline, x_start, width, available, hang, source byte span, glyph range, overfull, ratio}]}], overset} or a typed refusal","recovery":"stale_revision: reload and re-run; cyclic_thread: break the loop in the Folio previous_text_frame/next_text_frame links; invalid_geometry: use finite positive Folio Lengths in pt, pages of positive size and a fixed first baseline inside the frame inset; invalid_input: the story needs exactly one frame chain in the document, every link and page must exist, stored content needs resolved_text of byte_len bytes, byte spans must lie on char boundaries of the story text and inside the story, cluster starts must not decrease; missing_font: resolve the font in Type first; unsupported_composition: right-to-left, CJK, Arabic or other non-Latin runs, frame_grid stories, more than one text column, vertical justification other than top, auto-sizing and a first baseline other than fixed are not laid out in this slice; budget_exceeded: raise Limits or shorten the story; canceled: nothing was produced"},"model":{"units":"points (Folio Length unit pt); frame lines in frame coordinates (origin = frame top-left, y down), frame origin in spread coordinates (pages left to right, no gutter)","composers":"FirstFit = greedy single-line composer; KnuthPlass = optimum fit over the paragraph with badness 100*|r|^3, demerits (line_penalty+badness)^2, fitness classes tight/normal/loose/very loose, three passes (tolerance 200, 10000, overfull allowed); relation to the Adobe Paragraph Composer UNVERIFIED","text_flow":"byte offsets of the story text; hanging trailing whitespace belongs to the line glyph range but not to width_pt; no justification, hyphenation, columns, tables, pages or contour wrap in this slice","determinism":"identical input yields an identical proposal; no hashing, ties resolve to the earlier candidate"},"inputs":"ReflowRequest {document: Folio StudioDocument (revision is the current revision), story: Folio StudioTextStory, resolved_text (stored content only), paragraphs: ShapedParagraph[] (from_type adapts a Type ParagraphResult), obstacles: FrameObstacles[] (text wrap is not stored in Folio), composer, space, expected_revision, limits}","limits":{"max_lines":100000,"max_glyphs":1048576,"max_paragraphs":65536},"cancel":"CancellationToken is checked at entry, per paragraph, per 256 line-slot scans and per 256 break candidates","consumer":"layout-consumer [--descriptor] [--advance PT] [--text TEXT] [--frame WxH]... [--composer first-fit|knuth-plass] [--cancel]","argus":{"inspect":"LayoutProposal fields are plain data","action":"the same reflow call with the same token","capture":"caller captures the proposal and Observe frame bytes under a fresh grant"},"diagnostics":"report() emits one Observe terminal outcome; no text, ids or paths are serialized by layout","authority":"Pages, text frames, thread links and stories are Folio contracts read as given (overflow stays layout output and is not written to the document); host grants, history and acceptance remain pending"}"#;
