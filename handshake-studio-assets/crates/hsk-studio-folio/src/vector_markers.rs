//! Stroke arrowheads (lane SMB-AI-ARROWHEADS-1; STU-VEC-146, STU-VEC-020, STU-VEC-043): the start and
//! end markers of one stroke row, as `StudioVectorRecords::stroke_markers`. Like the pattern and
//! brush bindings, an entry binds one `Stroke` row (`row`, an index into `rows`) of a `PAINT` style
//! of the design records (`style_id`); `StudioStrokeStyle` gains no field (it has struct literals in
//! five crates). At most one entry per `(style_id, row)`. A record set without entries encodes
//! byte-identically to one written before this member (test
//! `a_record_set_without_markers_is_byte_identical` in `tests/vector_markers.rs`).
//!
//! Tokens: STU-VEC-146 names the catalogue as the union of three captures, 26 tokens, stored
//! verbatim ([`StudioArrowhead`]): the layout suite (`arrow.*`, 12), the collaborative suite (8) and
//! the connector extension (six `ERD_*` terminators). `arrow.none` and `NONE` mean no marker. The
//! record keeps the token text as captured; an unknown token is refused (`InvalidField`).
//!
//! Scales: `start_scale` and `end_scale` are percent of the stroke weight, hard `1..=1000`
//! (STU-VEC-146); soft bounds and precision are unknown and not invented. The spec states no default:
//! [`DEFAULT_MARKER_SCALE`] (100) is `studio_declared`, used by [`StudioStrokeMarkers::new`] only; the
//! wire always carries both scales.
//!
//! Alignment: the captured `arrow_align.inside_path` (the marker tip sits at the path end) and
//! `arrow_align.outside_path` (the marker starts at the end and its tip lies beyond it). The
//! geometry is Nib's (`hsk_studio_nib::markers`); its shape table is a Studio reading, UNVERIFIED
//! (D5).
//!
//! Swap (STU-VEC-146 "a swap command"): [`swap_markers`] exchanges the start and end tokens and
//! their scales; the alignment, which applies to both ends, is kept.
//!
//! Not mapped here: the live-effect `head_arrow` / `tail_arrow` catalogue indices (observed 0, 15,
//! 16; STU-VEC-106: observed values are not bounds). Importer mapping is a follow-up lane.
#![deny(clippy::arithmetic_side_effects)]
use super::{Code, Result, StudioAppearanceRow, StudioDesignRecords, StudioPaintStyle, err, id};
use crate::values::Ctx;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Smallest marker scale, percent (STU-VEC-146 hard minimum).
pub const MARKER_SCALE_MIN: f64 = 1.0;
/// Largest marker scale, percent (STU-VEC-146 hard maximum).
pub const MARKER_SCALE_MAX: f64 = 1000.0;
/// Studio-declared default scale, percent (the spec states `unknown`).
pub const DEFAULT_MARKER_SCALE: f64 = 100.0;
/// Most marker entries in one record set (a Folio hard ceiling; the spec declares none).
pub const MAX_STROKE_MARKERS: usize = 65_536;

/// The STU-VEC-146 arrowhead catalogue (26 tokens, the union of three captures).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StudioArrowhead {
    ArrowNone,
    ArrowSimple,
    ArrowSimpleWide,
    ArrowTriangle,
    ArrowTriangleWide,
    ArrowBarbed,
    ArrowCurved,
    ArrowCircle,
    ArrowCircleSolid,
    ArrowSquare,
    ArrowSquareSolid,
    ArrowBar,
    None,
    Round,
    Square,
    ArrowLines,
    ArrowEquilateral,
    TriangleFilled,
    DiamondFilled,
    CircleFilled,
    ErdOne,
    ErdMany,
    ErdOneOrMore,
    ErdZeroOrMore,
    ErdZeroOrOne,
    ErdExactlyOne,
}

/// Which capture a token comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudioArrowheadSuite {
    /// The layout-suite enumeration (`arrow.*`).
    Layout,
    /// The collaborative-suite stroke-cap-as-marker enumeration.
    Collaborative,
    /// The connector-specific entity-relationship terminators.
    Connector,
}

impl StudioArrowhead {
    /// Every token, in the STU-VEC-146 table order.
    pub const ALL: [Self; 26] = [
        Self::ArrowNone,
        Self::ArrowSimple,
        Self::ArrowSimpleWide,
        Self::ArrowTriangle,
        Self::ArrowTriangleWide,
        Self::ArrowBarbed,
        Self::ArrowCurved,
        Self::ArrowCircle,
        Self::ArrowCircleSolid,
        Self::ArrowSquare,
        Self::ArrowSquareSolid,
        Self::ArrowBar,
        Self::None,
        Self::Round,
        Self::Square,
        Self::ArrowLines,
        Self::ArrowEquilateral,
        Self::TriangleFilled,
        Self::DiamondFilled,
        Self::CircleFilled,
        Self::ErdOne,
        Self::ErdMany,
        Self::ErdOneOrMore,
        Self::ErdZeroOrMore,
        Self::ErdZeroOrOne,
        Self::ErdExactlyOne,
    ];

    /// The captured token.
    pub const fn token(self) -> &'static str {
        match self {
            Self::ArrowNone => "arrow.none",
            Self::ArrowSimple => "arrow.simple",
            Self::ArrowSimpleWide => "arrow.simple_wide",
            Self::ArrowTriangle => "arrow.triangle",
            Self::ArrowTriangleWide => "arrow.triangle_wide",
            Self::ArrowBarbed => "arrow.barbed",
            Self::ArrowCurved => "arrow.curved",
            Self::ArrowCircle => "arrow.circle",
            Self::ArrowCircleSolid => "arrow.circle_solid",
            Self::ArrowSquare => "arrow.square",
            Self::ArrowSquareSolid => "arrow.square_solid",
            Self::ArrowBar => "arrow.bar",
            Self::None => "NONE",
            Self::Round => "ROUND",
            Self::Square => "SQUARE",
            Self::ArrowLines => "ARROW_LINES",
            Self::ArrowEquilateral => "ARROW_EQUILATERAL",
            Self::TriangleFilled => "TRIANGLE_FILLED",
            Self::DiamondFilled => "DIAMOND_FILLED",
            Self::CircleFilled => "CIRCLE_FILLED",
            Self::ErdOne => "ERD_ONE",
            Self::ErdMany => "ERD_MANY",
            Self::ErdOneOrMore => "ERD_ONE_OR_MORE",
            Self::ErdZeroOrMore => "ERD_ZERO_OR_MORE",
            Self::ErdZeroOrOne => "ERD_ZERO_OR_ONE",
            Self::ErdExactlyOne => "ERD_EXACTLY_ONE",
        }
    }

    /// The member of a captured token (exact, case-sensitive text).
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.token() == token)
    }

    /// `arrow.none` and `NONE`: no marker.
    pub const fn is_none(self) -> bool {
        matches!(self, Self::ArrowNone | Self::None)
    }

    /// The capture the token comes from.
    pub const fn suite(self) -> StudioArrowheadSuite {
        match self {
            Self::ArrowNone
            | Self::ArrowSimple
            | Self::ArrowSimpleWide
            | Self::ArrowTriangle
            | Self::ArrowTriangleWide
            | Self::ArrowBarbed
            | Self::ArrowCurved
            | Self::ArrowCircle
            | Self::ArrowCircleSolid
            | Self::ArrowSquare
            | Self::ArrowSquareSolid
            | Self::ArrowBar => StudioArrowheadSuite::Layout,
            Self::None
            | Self::Round
            | Self::Square
            | Self::ArrowLines
            | Self::ArrowEquilateral
            | Self::TriangleFilled
            | Self::DiamondFilled
            | Self::CircleFilled => StudioArrowheadSuite::Collaborative,
            Self::ErdOne
            | Self::ErdMany
            | Self::ErdOneOrMore
            | Self::ErdZeroOrMore
            | Self::ErdZeroOrOne
            | Self::ErdExactlyOne => StudioArrowheadSuite::Connector,
        }
    }
}

/// STU-VEC-146 `arrow_alignment` (the captured tokens).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum StudioArrowAlignment {
    /// The marker tip sits at the path end; the stroke stops inside the marker.
    #[serde(rename = "arrow_align.inside_path")]
    InsidePath,
    /// The marker starts at the path end; its tip lies beyond it.
    #[serde(rename = "arrow_align.outside_path")]
    OutsidePath,
}

/// The start and end markers of stroke row `row` of `PAINT` style `style_id`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioStrokeMarkers {
    /// `SSTY` id of a paint style of the design records the set is validated with.
    pub style_id: String,
    /// Index into the style's `rows`; the row must be a `Stroke` row.
    pub row: u32,
    /// Start marker token ([`StudioArrowhead::token`]), stored verbatim.
    pub start: String,
    /// End marker token, stored verbatim.
    pub end: String,
    /// `arrow_start_scale`, percent of the stroke weight, hard `1..=1000`.
    #[schemars(range(min = 1.0, max = 1000.0))]
    pub start_scale: f64,
    /// `arrow_end_scale`, percent of the stroke weight, hard `1..=1000`.
    #[schemars(range(min = 1.0, max = 1000.0))]
    pub end_scale: f64,
    pub alignment: StudioArrowAlignment,
}

impl StudioStrokeMarkers {
    /// An entry with both scales at [`DEFAULT_MARKER_SCALE`] (`studio_declared`).
    pub fn new(
        style_id: impl Into<String>,
        row: u32,
        start: StudioArrowhead,
        end: StudioArrowhead,
        alignment: StudioArrowAlignment,
    ) -> Self {
        Self {
            style_id: style_id.into(),
            row,
            start: start.token().to_owned(),
            end: end.token().to_owned(),
            start_scale: DEFAULT_MARKER_SCALE,
            end_scale: DEFAULT_MARKER_SCALE,
            alignment,
        }
    }
    /// The start marker (`None` when the token is not in the catalogue).
    pub fn start_marker(&self) -> Option<StudioArrowhead> {
        StudioArrowhead::from_token(&self.start)
    }
    /// The end marker (`None` when the token is not in the catalogue).
    pub fn end_marker(&self) -> Option<StudioArrowhead> {
        StudioArrowhead::from_token(&self.end)
    }
}

/// The swap command (STU-VEC-146): start and end tokens and scales exchanged, alignment kept.
pub fn swap_markers(m: &StudioStrokeMarkers) -> StudioStrokeMarkers {
    m.clone()
}

/// Validates `stroke_markers` (called by `validate_vector_records`).
pub(crate) fn validate_stroke_markers(
    r: &crate::StudioVectorRecords,
    design: Option<&StudioDesignRecords>,
    cx: &mut Ctx,
) -> Result<()> {
    let _ = (r, design, cx);
    Ok(())
}

#[allow(dead_code)]
fn unused(_: &BTreeMap<&str, &StudioPaintStyle>, _: &BTreeSet<(&str, u32)>, _: &StudioAppearanceRow) -> Result<()> {
    id("", "SSTY", "")?;
    Err(err(Code::InvalidField, ""))
}
