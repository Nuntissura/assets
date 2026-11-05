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
use super::{Budget, Code, Payload, Result, StudioDocument, Unresolved, canceled, err, id, key, sequence_preset::parse_wire, values::Ctx};
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

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

fn well_formed(token: &str) -> bool {
    !token.is_empty() && token.len() <= MAX_COMPOSITION_TOKEN_BYTES && token.bytes().all(|b| b.is_ascii_graphic())
}

/// Checks a record against the document: its id, every token's form and bound, an upgrade chain that
/// runs forward and ends in the token, and last that this build knows the token, so a well-formed
/// record from a later build is [`Code::UnknownCompositionToken`] and anything else `InvalidField`.
pub fn validate_composition_token_record(r: &StudioCompositionTokenRecord, d: &StudioDocument, b: Budget, token: &CancellationToken) -> Result<()> {
    const AT: &str = "composition_token";
    let mut cx = Ctx::new(b, token, &Unresolved);
    cx.tick()?;
    id(&r.document_id, "SDOC", "document_id")?;
    if r.document_id != d.document_id {
        return Err(err(Code::InvalidField, "document_id"));
    }
    if !well_formed(&r.composition_version) {
        return Err(err(Code::InvalidField, "composition_token/composition_version"));
    }
    if r.upgrades.len() > MAX_COMPOSITION_UPGRADES {
        return Err(err(Code::BudgetExceeded, AT));
    }
    cx.nodes(r.upgrades.len(), AT)?;
    let mut reached: Option<&str> = None;
    for u in &r.upgrades {
        cx.tick()?;
        if !well_formed(&u.from_token) || !well_formed(&u.to_token) || u.from_token == u.to_token {
            return Err(err(Code::InvalidField, "composition_token/upgrades"));
        }
        if reached.is_some_and(|t| t != u.from_token) {
            return Err(err(Code::InvalidField, "composition_token/upgrades"));
        }
        if let (Some(from), Some(to)) = (composition_token_rank(&u.from_token), composition_token_rank(&u.to_token))
            && from >= to
        {
            return Err(err(Code::InvalidField, "composition_token/upgrades"));
        }
        reached = Some(&u.to_token);
        if u.story_ids.len() > MAX_COMPOSITION_UPGRADE_STORIES {
            return Err(err(Code::BudgetExceeded, AT));
        }
        cx.nodes(u.story_ids.len(), AT)?;
        for pair in u.story_ids.windows(2) {
            if pair[0] >= pair[1] {
                return Err(err(Code::InvalidField, "composition_token/story_ids"));
            }
        }
        if !u.story_ids.iter().all(|s| key(s)) {
            return Err(err(Code::InvalidId, "composition_token/story_ids"));
        }
    }
    if reached.is_some_and(|t| t != r.composition_version) {
        return Err(err(Code::InvalidField, "composition_token/upgrades"));
    }
    if composition_token_rank(&r.composition_version).is_none() {
        return Err(err(Code::UnknownCompositionToken, AT));
    }
    Ok(())
}

/// Parses wire JSON (bounded by [`MAX_COMPOSITION_TOKEN_INPUT_BYTES`], `Budget::input_bytes` and depth
/// before any parse; duplicate keys `DuplicateField`, unknown fields, a missing field and wrong types
/// `InvalidJson`) and validates it against the document.
pub fn parse_composition_token_record(input: &[u8], d: &StudioDocument, b: Budget, token: &CancellationToken) -> Result<StudioCompositionTokenRecord> {
    if input.len() > MAX_COMPOSITION_TOKEN_INPUT_BYTES {
        return Err(err(Code::BudgetExceeded, "composition_token"));
    }
    let r: StudioCompositionTokenRecord = parse_wire(input, b, token, "composition_token")?;
    validate_composition_token_record(&r, d, b, token)?;
    Ok(r)
}

#[derive(Deserialize)]
struct TextEntryStories {
    #[serde(default)]
    stories: Vec<TextEntryStory>,
}
#[derive(Deserialize)]
struct TextEntryStory {
    story_id: String,
}

/// The stories a token change affects, sorted and unique: those the document's text layers name and
/// those a host's `text` document entry holds (`stories[].story_id`, the one part of that entry read
/// here; its other fields belong to its producer, which is why they are not refused). Text entry bytes
/// the diff cannot read are the Folio diagnostic, never a shorter list; more stories than one upgrade
/// may name is `BudgetExceeded`. Stories held anywhere else are not listed: no other host composes.
pub fn composition_affected_stories(d: &StudioDocument, text_entry: Option<&[u8]>, b: Budget, token: &CancellationToken) -> Result<Vec<String>> {
    canceled(token)?;
    let mut stories: BTreeSet<String> = BTreeSet::new();
    for layer in &d.layers {
        if let Payload::Text { story_id } = &layer.payload {
            stories.insert(story_id.clone());
        }
    }
    if let Some(bytes) = text_entry {
        let entry: TextEntryStories = parse_wire(bytes, b, token, "composition_token/text")?;
        stories.extend(entry.stories.into_iter().map(|s| s.story_id));
    }
    if stories.len() > MAX_COMPOSITION_UPGRADE_STORIES {
        return Err(err(Code::BudgetExceeded, "composition_token"));
    }
    Ok(stories.into_iter().collect())
}
