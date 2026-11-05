//! Text derived from a story (lane SMB-LAYOUT-ENTRY-TEXT-1, R55 F-R55-5 and F-R55-9; STU-TYP-207).
#![deny(clippy::arithmetic_side_effects)]
use crate::sentinels::{MarkerKind, UnresolvedMarker};

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
