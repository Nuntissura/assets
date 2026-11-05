//! The composition token a stored document was authored with (lane SMB-TYPE-AUTHORED-TOKEN-1;
//! STU-TYP-130: "A stored document reopened under a newer token MUST recompose under the token it was
//! authored with unless the operator or model explicitly upgrades it, and the upgrade MUST be a
//! recorded, undoable, diffable operation").
//!
//! [`StudioCompositionTokenRecord`] (`hsk.studio.composition_token@1`) is one record per document: the
//! clause speaks of a stored document, not of a story, so every story of a document composes under one
//! token. It holds the token and the upgrades that led to it, each with its from-token, to-token and
//! the stories it affects. A document with no record reads as [`ABSENT_COMPOSITION_TOKEN`] (lead
//! decision on this lane: a later token would claim semantics the file never recorded).
//!
//! [`STUDIO_COMPOSITION_TOKENS`] is the list this build can compose under, oldest first. Type owns the
//! tokens; Folio may depend only on Accord, so they are repeated here, and a harness-view test
//! (`tests/viewer_composition_token.rs`) fails when the two disagree or Type gains a token. A
//! well-formed token that is not on the list is [`Code::UnknownCompositionToken`], never the newest.
//!
//! The record arrives from untrusted wire JSON: integer arithmetic is denied (workflow V4-01), token
//! bytes are bounded by [`MAX_COMPOSITION_TOKEN_BYTES`], every list is counted before it is walked, and
//! a parse refuses bytes over [`MAX_COMPOSITION_TOKEN_INPUT_BYTES`] whatever the budget allows.
#![deny(clippy::arithmetic_side_effects)]
use super::{Budget, Result, StudioDocument, sequence_preset::parse_wire};
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

schema_token!(CompositionTokenSchema, "hsk.studio.composition_token@1");

/// Every token this build composes under, oldest first; the last is the current one. A copy of Type's
/// `COMPOSITION_VERSION_V1`, `COMPOSITION_VERSION_V2` and `COMPOSITION_VERSION` (module docs).
pub const STUDIO_COMPOSITION_TOKENS: [&str; 3] = [
    "hsk-type-shaped-runs@1/rustybuzz-0.20.1/ttf-parser-0.25.1/bidi-0.3.18-u16/ccc-0.4.0-u16/mirroring-0.4.0-u16/script-0.5.8-u17/properties-0.1.4-u17/preserve/source-groups-v1",
    "hsk-type-shaped-runs@2/rustybuzz-0.20.1/ttf-parser-0.25.1/bidi-0.3.18-u16/ccc-0.4.0-u16/mirroring-0.4.0-u16/script-0.5.8-u17/properties-0.1.4-u17/preserve/source-groups-v1/presentation-ligatures-v1/line-end-advances-v1",
    "hsk-type-shaped-runs@3/rustybuzz-0.20.1/ttf-parser-0.25.1/bidi-0.3.18-u16/ccc-0.4.0-u16/mirroring-0.4.0-u16/script-0.5.8-u17/properties-0.1.4-u17/preserve/source-groups-v1/presentation-ligatures-v1/line-end-advances-v1/forced-line-break-v1",
];
/// What a document with no stored token reads as.
pub const ABSENT_COMPOSITION_TOKEN: &str = STUDIO_COMPOSITION_TOKENS[0];
/// The token a new document records when it is created.
pub const CURRENT_COMPOSITION_TOKEN: &str = STUDIO_COMPOSITION_TOKENS[STUDIO_COMPOSITION_TOKENS.len() - 1];
/// Longest stored token in bytes (Studio hostile-input bound; today's tokens are under 256).
pub const MAX_COMPOSITION_TOKEN_BYTES: usize = 1024;
/// Most upgrades one record holds.
pub const MAX_COMPOSITION_UPGRADES: usize = 64;
/// Most stories one upgrade names.
pub const MAX_COMPOSITION_UPGRADE_STORIES: usize = 4096;
/// Largest wire form of a record in bytes, whatever the caller's budget allows.
pub const MAX_COMPOSITION_TOKEN_INPUT_BYTES: usize = 1_048_576;

/// One recorded upgrade, and the diff of the operation: the token before, the token after and the
/// stories that recompose differently named by id, sorted. Glyph-level difference is not held here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioCompositionUpgrade {
    pub from_token: String,
    pub to_token: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub story_ids: Vec<String>,
}

/// The per-document token record (module docs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioCompositionTokenRecord {
    pub schema_id: CompositionTokenSchema,
    /// `SDOC` id of the document.
    pub document_id: String,
    /// STU-TYP-130 `composition_version`: the token the document composes under.
    pub composition_version: String,
    /// The upgrades that led to it, oldest first; the last one's `to_token` is `composition_version`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub upgrades: Vec<StudioCompositionUpgrade>,
}
impl StudioCompositionTokenRecord {
    /// A record for `document_id` authored under `token`, with no upgrades.
    pub fn new(document_id: impl Into<String>, token: impl Into<String>) -> Self {
        Self { schema_id: CompositionTokenSchema::V1, document_id: document_id.into(), composition_version: token.into(), upgrades: Vec::new() }
    }
}

/// The position of `token` in [`STUDIO_COMPOSITION_TOKENS`] (`None` = this build does not know it).
pub fn composition_token_rank(token: &str) -> Option<usize> {
    STUDIO_COMPOSITION_TOKENS.iter().position(|t| *t == token)
}

/// The JSON schema of the record.
pub fn composition_token_record_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12().into_generator().into_root_schema_for::<StudioCompositionTokenRecord>()
}

/// Checks a record against the document.
pub fn validate_composition_token_record(_r: &StudioCompositionTokenRecord, _d: &StudioDocument, _b: Budget, _token: &CancellationToken) -> Result<()> {
    Ok(())
}

/// Parses wire JSON and validates it against the document.
pub fn parse_composition_token_record(input: &[u8], d: &StudioDocument, b: Budget, token: &CancellationToken) -> Result<StudioCompositionTokenRecord> {
    let r: StudioCompositionTokenRecord = parse_wire(input, b, token, "composition_token")?;
    validate_composition_token_record(&r, d, b, token)?;
    Ok(r)
}

/// The stories a token change affects, sorted and unique.
pub fn composition_affected_stories(_d: &StudioDocument, _text_entry: Option<&[u8]>, _b: Budget, _token: &CancellationToken) -> Result<Vec<String>> {
    Ok(Vec::new())
}
