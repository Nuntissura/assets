use crate::{AdmissionPort, EpochPort, Error, Reservation};
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceRange {
    pub start: u64,
    pub end: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoryDirection {
    LeftToRight,
    RightToLeft,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    LeftToRight,
    RightToLeft,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CharacterOverride {
    Default,
    LeftToRight,
    RightToLeft,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectionProvenance {
    Explicit,
    StoryDefault,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Normalization {
    Preserve,
    Unsupported,
}
#[derive(Clone, Copy, Debug)]
pub struct ParagraphInput {
    pub range: SourceRange,
    pub direction: Direction,
    pub provenance: DirectionProvenance,
}
#[derive(Clone, Copy, Debug)]
pub struct ReadAddress<'a> {
    pub owner: &'a DomainId,
    pub property: &'a str,
    pub expected_revision: u64,
    pub fingerprint: [u8; 32],
}
#[derive(Clone, Copy, Debug)]
pub struct ResultTarget<'a> {
    pub address: ReadAddress<'a>,
    pub preimage: &'a [u8],
}
#[derive(Clone, Copy, Debug)]
pub struct Axis {
    pub tag: [u8; 4],
    pub value: f32,
}
#[derive(Clone, Copy, Debug)]
pub struct Feature {
    pub tag: [u8; 4],
    pub value: u32,
    pub range: SourceRange,
    pub declared: bool,
}
/// Explicit immutable font provenance; identity strings are caller-owned, never parsed identities.
#[derive(Clone, Copy, Debug)]
pub struct FontResource<'a> {
    pub identity: &'a str,
    pub location: &'a str,
    pub content_hash: [u8; 32],
    pub face_index: u32,
    pub resource_revision: u64,
    pub grant_revision: u64,
}
#[derive(Clone, Copy, Debug)]
pub struct Substitution<'a> {
    pub requested_identity: &'a str,
    pub resolved_identity: &'a str,
    pub reason: SubstitutionReason,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubstitutionReason {
    MissingRequested,
    UnavailableRequested,
    CoverageFallback,
}
#[derive(Clone, Copy, Debug)]
pub struct Style<'a> {
    pub read_index: u32,
    pub range: SourceRange,
    pub font_size_pt: f64,
    pub requested_identity: &'a str,
    pub candidates: &'a [FontResource<'a>],
    pub substitution: Option<Substitution<'a>>,
    pub script: [u8; 4],
    pub language: &'a str,
    pub character_override: CharacterOverride,
    pub features: &'a [Feature],
    pub axes: &'a [Axis],
}
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub input_bytes: u64,
    pub input_scalars: u64,
    pub styles: u64,
    pub features: u64,
    pub axes: u64,
    pub font_resources: u64,
    pub font_bytes: u64,
    pub parser_tables: u64,
    pub lookups: u64,
    pub runs: u64,
    pub glyphs: u64,
    pub fallback_attempts: u64,
    pub recursion: u64,
    pub work_units: u64,
    pub requested_owned_bytes: u64,
    pub retained_generations: u64,
}
#[derive(Clone, Copy)]
pub struct Request<'a> {
    pub document_id: &'a DomainId,
    pub document_revision: u64,
    pub story_id: &'a DomainId,
    pub layer_id: &'a DomainId,
    pub text_utf8: &'a str,
    pub text_sha256: [u8; 32],
    pub text_read_index: u32,
    pub reads: &'a [ReadAddress<'a>],
    pub result_target: ResultTarget<'a>,
    pub composition_version: &'a str,
    pub normalization: Normalization,
    pub story_direction: StoryDirection,
    pub paragraphs: &'a [ParagraphInput],
    pub styles: &'a [Style<'a>],
    pub resolver_revision: u64,
    pub cancel_epoch: u64,
    pub cancellation: &'a CancellationToken,
    pub actor: &'a ActorContext,
    pub correlation_id: &'a str,
    pub observation_correlation: u64,
    pub limits: Limits,
}
/// A resource borrower carries its exact caller-owned reservation through every parser borrower.
pub struct FontRead<'a> {
    pub bytes: &'a [u8],
    pub resource_revision: u64,
    pub grant_revision: u64,
    pub lease: Reservation<'a>,
}
pub trait FontResolver: Send + Sync {
    fn revision(&self) -> Result<u64, Error>;
    fn resolve(&self, resource: &FontResource<'_>) -> Result<FontRead<'_>, Error>;
    fn verify_lifetime(
        &self,
        resource: &FontResource<'_>,
        read: &FontRead<'_>,
    ) -> Result<(), Error>;
}
#[derive(Clone, Copy, Debug)]
pub struct InputCharge {
    pub lifetime_id: u64,
    pub requested_bytes: u64,
}
pub trait InputLifetime: Send + Sync {
    fn verify(&self, request: &Request<'_>) -> Result<InputCharge, Error>;
}
pub struct CurrentRead<'a> {
    pub revision: u64,
    pub fingerprint: [u8; 32],
    pub bytes: &'a [u8],
}
/// Serializing membership/readset/result-slot checks is the injected caller's responsibility.
pub trait SourceContext: EpochPort {
    fn member(&self, request: &Request<'_>) -> Result<(), Error>;
    fn read(&self, address: ReadAddress<'_>) -> Result<CurrentRead<'_>, Error>;
    fn resolved_style(&self, address: ReadAddress<'_>) -> Result<Style<'_>, Error>;
}
pub struct Ports<'a> {
    pub context: &'a dyn SourceContext,
    pub resolver: &'a dyn FontResolver,
    pub input_lifetime: &'a dyn InputLifetime,
    pub admission: &'a dyn AdmissionPort,
}
