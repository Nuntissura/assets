//! Neutral interchange proposal types. A proposal is data, not authority: mapping it into a Folio
//! sequence and committing it belong to the caller (STU-ARC-011).
use hsk_studio_accord::{FrameRate, Ticks};
use hsk_studio_folio::Speed;

/// STU-IO-186 per-entity outcome vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    /// Mapped to an editable Folio value without change.
    Editable,
    /// Preserved only as opaque original data.
    Opaque,
    /// Mapped exactly into a different representation (for example a speed ratio).
    Translated,
    /// Mapped with a named approximation.
    Approximate,
    /// Present in the source, not representable; dropped explicitly.
    Unsupported,
    /// Referenced but not available (offline media, missing decoder).
    Missing,
}

impl Disposition {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Editable => "editable",
            Self::Opaque => "opaque",
            Self::Translated => "translated",
            Self::Approximate => "approximate",
            Self::Unsupported => "unsupported",
            Self::Missing => "missing",
        }
    }
}

/// One typed loss or approximation. Nothing the importer drops or bends is silent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LossNote {
    /// `event 007`, `edl`, `native:project_items`.
    pub entity: String,
    pub property: &'static str,
    pub disposition: Disposition,
    pub code: &'static str,
}

impl LossNote {
    pub(crate) fn new(
        entity: impl Into<String>,
        property: &'static str,
        disposition: Disposition,
        code: &'static str,
    ) -> Self {
        Self {
            entity: entity.into(),
            property,
            disposition,
            code,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceFormat {
    Cmx3600Edl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LaneKind {
    Video,
    Audio,
}

/// A track lane: kind plus zero-based index within the kind (`A2` = audio index 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Lane {
    pub kind: LaneKind,
    pub index: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipProposal {
    /// Source event number (repeats for the two clips of a `B` event).
    pub event: u32,
    pub reel: String,
    pub clip_name: Option<String>,
    pub lane: Lane,
    /// Source window in the SOURCE time base (absolute timecode * ticks per frame).
    pub source_in: Ticks,
    pub source_out: Ticks,
    /// Position on the timeline, relative to the request record origin.
    pub record_in: Ticks,
    pub record_out: Ticks,
    /// Folio's stored speed (reduced; negative numerator = reverse).
    pub speed: Speed,
    pub disposition: Disposition,
    /// Events that produced a linked video+audio pair share this key.
    pub link: Option<u32>,
}

impl ClipProposal {
    /// Equality of everything the CMX3600 export can reproduce (event numbers and links excluded).
    pub fn same_mapping(&self, other: &ClipProposal) -> bool {
        self.reel == other.reel
            && self.clip_name == other.clip_name
            && self.lane == other.lane
            && self.source_in == other.source_in
            && self.source_out == other.source_out
            && self.record_in == other.record_in
            && self.record_out == other.record_out
            && self.speed == other.speed
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineProposal {
    pub source_format: SourceFormat,
    pub title: Option<String>,
    pub frame_rate: FrameRate,
    pub drop_frame: bool,
    /// Frames subtracted from every record timecode (for example 01:00:00:00 = 90000 at 25 fps).
    pub record_origin_frames: u64,
    pub clips: Vec<ClipProposal>,
    pub losses: Vec<LossNote>,
    /// Exact input length in bytes.
    pub source_len: u64,
    /// Comment lines that carry no mapped meaning (counted, not interpreted).
    pub ignored_comment_lines: u32,
}

impl TimelineProposal {
    pub fn losses_with(&self, disposition: Disposition) -> impl Iterator<Item = &LossNote> {
        self.losses
            .iter()
            .filter(move |l| l.disposition == disposition)
    }
}
