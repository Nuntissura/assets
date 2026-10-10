use crate::provider::storage::ChargedVec;
use crate::{Axis, Direction, Feature, FontResource, Request, SourceRange, SubstitutionReason};
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glyph {
    pub glyph_id: u32,
    pub cluster: SourceRange,
    pub x_advance_pt: f64,
    pub y_advance_pt: f64,
    pub x_offset_pt: f64,
    pub y_offset_pt: f64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureState {
    Applied,
    UnavailableInert,
}
#[derive(Clone, Copy, Debug)]
pub struct FeatureReceipt {
    pub feature: Feature,
    pub state: FeatureState,
}
#[derive(Clone, Copy, Debug)]
pub struct AxisReceipt {
    pub axis: Axis,
    pub defaulted: bool,
}
pub struct Run<'a> {
    pub(crate) source: SourceRange,
    pub(crate) bidi_level: u8,
    pub(crate) direction: Direction,
    pub(crate) script: [u8; 4],
    pub(crate) language: &'a str,
    pub(crate) font_size_pt: f64,
    pub(crate) requested_identity: &'a str,
    pub(crate) resolved: FontResource<'a>,
    pub(crate) candidate_index: u32,
    pub(crate) substitution: Option<SubstitutionReason>,
    pub(crate) provider_scale: u32,
    pub(crate) units_per_em: u32,
    pub(crate) axes: ChargedVec<'a, AxisReceipt>,
    pub(crate) features: ChargedVec<'a, FeatureReceipt>,
    pub(crate) glyphs: ChargedVec<'a, Glyph>,
}
impl<'a> Run<'a> {
    pub fn source(&self) -> SourceRange {
        self.source
    }
    pub fn bidi_level(&self) -> u8 {
        self.bidi_level
    }
    pub fn direction(&self) -> Direction {
        self.direction
    }
    pub fn script(&self) -> [u8; 4] {
        self.script
    }
    pub fn language(&self) -> &str {
        self.language
    }
    pub fn font_size_pt(&self) -> f64 {
        self.font_size_pt
    }
    pub fn requested_identity(&self) -> &str {
        self.requested_identity
    }
    pub fn resolved(&self) -> FontResource<'a> {
        self.resolved
    }
    pub fn candidate_index(&self) -> u32 {
        self.candidate_index
    }
    pub fn substitution(&self) -> Option<SubstitutionReason> {
        self.substitution
    }
    pub fn provider_scale(&self) -> u32 {
        self.provider_scale
    }
    pub fn units_per_em(&self) -> u32 {
        self.units_per_em
    }
    pub fn axes(&self) -> &[AxisReceipt] {
        self.axes.as_slice()
    }
    pub fn features(&self) -> &[FeatureReceipt] {
        self.features.as_slice()
    }
    pub fn glyphs(&self) -> &[Glyph] {
        self.glyphs.as_slice()
    }
}
pub struct ParagraphResult<'a> {
    pub(crate) source: SourceRange,
    pub(crate) direction: Direction,
    pub(crate) runs: ChargedVec<'a, Run<'a>>,
}
impl<'a> ParagraphResult<'a> {
    pub fn source(&self) -> SourceRange {
        self.source
    }
    pub fn direction(&self) -> Direction {
        self.direction
    }
    pub fn runs(&self) -> &[Run<'a>] {
        self.runs.as_slice()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoverageKind {
    ShapedCluster,
    ParagraphSeparator,
    NonrenderingControl,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlyphReference {
    pub paragraph: u32,
    pub run: u32,
    pub start: u32,
    pub end: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Coverage {
    pub source: SourceRange,
    pub kind: CoverageKind,
    pub glyphs: Option<GlyphReference>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextDisposition {
    Shaped,
    EmptyInput,
    ControlOnly,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub input_bytes: u64,
    pub scalars: u64,
    pub fonts: u64,
    pub runs: u64,
    pub glyphs: u64,
    pub fallback_attempts: u64,
    pub work_units: u64,
    pub current_requested_bytes: u64,
    pub operation_peak_requested_bytes: u64,
    pub retained_generations: u64,
}
/// Sole ownership of admitted result storage; no owned Clone or mutable allocation escape.
pub struct Prepared<'a> {
    pub(crate) request: Request<'a>,
    pub(crate) paragraphs: ChargedVec<'a, ParagraphResult<'a>>,
    pub(crate) coverage: ChargedVec<'a, Coverage>,
    pub(crate) serialized: ChargedVec<'a, u8>,
    pub(crate) disposition: TextDisposition,
    pub(crate) counts: Counts,
    pub(crate) _generation: crate::Generation<'a>,
}
#[derive(Clone, Copy, Debug)]
pub struct Rejected {
    pub error: crate::Error,
    pub source: Option<SourceRange>,
    pub counts: Counts,
    pub cancel_epoch: u64,
}
impl<'a> Prepared<'a> {
    pub fn request(&self) -> &Request<'a> {
        &self.request
    }
    pub fn paragraphs(&self) -> &[ParagraphResult<'a>] {
        self.paragraphs.as_slice()
    }
    pub fn coverage(&self) -> &[Coverage] {
        self.coverage.as_slice()
    }
    pub fn serialized(&self) -> &[u8] {
        self.serialized.as_slice()
    }
    pub fn disposition(&self) -> TextDisposition {
        self.disposition
    }
    pub fn counts(&self) -> Counts {
        self.counts
    }
}
