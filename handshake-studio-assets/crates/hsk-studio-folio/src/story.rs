//! Minimal story-content contract for layout: `StudioTextStory` (schema `hsk.studio.text_story@1`, id
//! prefix `STXT`; spec STU-TYP-110, STU-TYP-113, STU-TYP-114, STU-TYP-025, STU-TYP-210, STU-CON-037).
//! A text frame names its story through the text layer payload; this object is what that story id
//! resolves to. It is a standalone primitive, not a `StudioDocument` member, so documents stay
//! byte-compatible. Folio stores and validates only; shaping, composition and style resolution
//! belong to Type and Layout.
//!
//! Stored: the character stream as a content reference (inline UTF-8, or a content-addressed
//! artifact handle with its byte length), the `text_kind` and `story_direction` discriminators, the
//! paragraph partition (half-open original-UTF-8 byte ranges, each with an optional `STYS` style
//! binding and optional `paragraph_direction`) and the character-range partition (each with an
//! optional `STYS` style binding and `applied_language`). Both partitions tile the stream exactly.
//!
//! Declared gaps, not invented here: character and paragraph attribute records and local overrides,
//! `character_direction_override`, the other addressable units, tables, notes and tracked changes
//! (STU-TYP-110/111) are not stored; no language-resource identity prefix is declared, so
//! `applied_language` is an opaque language-resource key (`[A-Za-z0-9_-]{1,128}`, which holds both
//! ICU names such as `en_US` and BCP 47 tags such as `en-US`); `text_sha256` of STU-CON-037 is the
//! `content_digest` of a stored reference and is verified by the caller's resolver, not computed here
//! (Folio depends on Accord alone); there is no read-only extension inspection path for stories yet.
use super::{
    Budget, Code, ContentDigest, Resolution, ResolutionIssue, Resolver, Result, canceled, err, id,
    key, required,
    timeline::{control_free, handle},
};
use hsk_studio_accord::CancellationToken;
use schemars::{JsonSchema, generate::SchemaSettings};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

schema_token!(TextStorySchema, "hsk.studio.text_story@1");

/// STU-TYP-114 `text_kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StoryTextKind {
    Point,
    Area,
    Path,
    FrameGrid,
}

/// STU-TYP-113 `story_direction`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StoryDirection {
    LeftToRight,
    RightToLeft,
    Unknown,
}

/// STU-TYP-113 `paragraph_direction`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StoryParagraphDirection {
    LeftToRight,
    RightToLeft,
}

/// The character stream, by reference. Offsets everywhere are half-open original-UTF-8 byte offsets.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "content_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StoryContent {
    /// The stream is carried in the story itself.
    Inline { text_utf8: String },
    /// The stream is a content-addressed artifact (never a filesystem path), resolved by its owner.
    Stored {
        artifact_manifest_id: String,
        content_digest: ContentDigest,
        byte_len: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StoryParagraph {
    pub start: u64,
    pub end: u64,
    /// `STYS` paragraph-style binding, or explicit null.
    #[serde(deserialize_with = "required")]
    #[schemars(required)]
    pub style_ref: Option<String>,
    /// Required for every paragraph when `story_direction` is `unknown`; otherwise null defaults from
    /// the story direction.
    #[serde(deserialize_with = "required")]
    #[schemars(required)]
    pub paragraph_direction: Option<StoryParagraphDirection>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StoryRange {
    pub start: u64,
    pub end: u64,
    /// `STYS` character-style binding, or explicit null.
    #[serde(deserialize_with = "required")]
    #[schemars(required)]
    pub style_ref: Option<String>,
    /// Language-resource key (STU-TYP-210), or explicit null for no stated language.
    #[serde(deserialize_with = "required")]
    #[schemars(required)]
    pub applied_language: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTextStory {
    pub schema_id: TextStorySchema,
    /// `STXT-{uuid_v7}`.
    pub story_id: String,
    pub text_kind: StoryTextKind,
    pub story_direction: StoryDirection,
    pub content: StoryContent,
    /// Ordered, non-empty, tiling `[0, byte_len)` exactly; empty only for an empty stream.
    pub paragraphs: Vec<StoryParagraph>,
    /// Ordered, non-empty, tiling `[0, byte_len)` exactly; empty only for an empty stream.
    pub character_ranges: Vec<StoryRange>,
}
impl StudioTextStory {
    /// Length of the character stream in bytes.
    pub fn byte_len(&self) -> u64 {
        match &self.content {
            StoryContent::Inline { text_utf8 } => text_utf8.len() as u64,
            StoryContent::Stored { byte_len, .. } => *byte_len,
        }
    }
    /// The stream when it is carried inline.
    pub fn inline_text(&self) -> Option<&str> {
        match &self.content {
            StoryContent::Inline { text_utf8 } => Some(text_utf8),
            StoryContent::Stored { .. } => None,
        }
    }
}

/// JSON schema (draft 2020-12) of the standalone story object.
pub fn text_story_schema() -> schemars::Schema {
    SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<StudioTextStory>()
}

fn charge(total: &mut usize, bytes: usize, limit: usize, target: &str) -> Result<()> {
    *total = total
        .checked_add(bytes)
        .filter(|n| *n <= limit)
        .ok_or_else(|| err(Code::BudgetExceeded, target))?;
    Ok(())
}

/// Checks that `(start, end)` pairs tile `[0, len)` exactly, each non-empty, and (for inline text)
/// start on Unicode scalar boundaries.
fn tiles(
    pairs: impl Iterator<Item = (u64, u64)>,
    len: u64,
    text: Option<&str>,
    token: &CancellationToken,
    target: &str,
) -> Result<()> {
    let mut expected = 0u64;
    for (n, (start, end)) in pairs.enumerate() {
        canceled(token)?;
        let boundary = |offset: u64| {
            text.is_none_or(|t| usize::try_from(offset).is_ok_and(|o| t.is_char_boundary(o)))
        };
        if start != expected || end <= start || end > len || !boundary(start) || !boundary(end) {
            return Err(err(Code::InvalidField, format!("{target}/{n}")));
        }
        expected = end;
    }
    if expected != len {
        return Err(err(Code::InvalidField, target));
    }
    Ok(())
}

/// Validates a standalone story and reports unresolved style or content references. Style bindings
/// resolve through `Resolver::primitive` against `hsk.studio.type_style@1`; stored content resolves
/// through `Resolver::story_content`. Inline text and the range tables are charged against
/// `Budget::payload_bytes`; paragraphs and ranges count against `Budget::nodes`.
pub fn validate_text_story(
    s: &StudioTextStory,
    b: Budget,
    token: &CancellationToken,
    r: &dyn Resolver,
) -> Result<Vec<ResolutionIssue>> {
    canceled(token)?;
    let sid = s.story_id.as_str();
    id(sid, "STXT", "story_id")?;
    if s.paragraphs.len().saturating_add(s.character_ranges.len()) > b.nodes {
        return Err(err(Code::BudgetExceeded, sid));
    }
    let mut payload = sid.len();
    let mut issues = Vec::new();
    match &s.content {
        StoryContent::Inline { text_utf8 } => {
            charge(&mut payload, text_utf8.len(), b.payload_bytes, sid)?;
        }
        StoryContent::Stored {
            artifact_manifest_id,
            content_digest,
            ..
        } => {
            if !handle(artifact_manifest_id)
                || !control_free(&content_digest.algorithm)
                || !control_free(&content_digest.digest)
            {
                return Err(err(Code::InvalidField, format!("{sid}/content")));
            }
            charge(
                &mut payload,
                artifact_manifest_id.len()
                    + content_digest.algorithm.len()
                    + content_digest.digest.len(),
                b.payload_bytes,
                sid,
            )?;
            let disposition = r.story_content(artifact_manifest_id, content_digest);
            if disposition != Resolution::Available {
                issues.push(ResolutionIssue {
                    target: format!("{sid}/content"),
                    disposition,
                });
            }
        }
    }
    let len = s.byte_len();
    let text = s.inline_text();
    tiles(
        s.paragraphs.iter().map(|p| (p.start, p.end)),
        len,
        text,
        token,
        &format!("{sid}/paragraphs"),
    )?;
    tiles(
        s.character_ranges.iter().map(|c| (c.start, c.end)),
        len,
        text,
        token,
        &format!("{sid}/character_ranges"),
    )?;
    let mut styles = BTreeSet::new();
    let mut bind = |style: &Option<String>, target: String| -> Result<()> {
        let Some(style) = style else {
            return Ok(());
        };
        id(style, "STYS", &target)?;
        if styles.insert(style.clone()) {
            let disposition = r.primitive(hsk_studio_accord::SCHEMA_STUDIO_TYPE_STYLE, style);
            if disposition != Resolution::Available {
                issues.push(ResolutionIssue {
                    target: format!("{sid}/style/{style}"),
                    disposition,
                });
            }
        }
        Ok(())
    };
    for (n, p) in s.paragraphs.iter().enumerate() {
        canceled(token)?;
        bind(&p.style_ref, format!("{sid}/paragraphs/{n}/style_ref"))?;
        if s.story_direction == StoryDirection::Unknown && p.paragraph_direction.is_none() {
            return Err(err(
                Code::InvalidField,
                format!("{sid}/paragraphs/{n}/paragraph_direction"),
            ));
        }
        charge(&mut payload, 64, b.payload_bytes, sid)?;
    }
    for (n, c) in s.character_ranges.iter().enumerate() {
        canceled(token)?;
        bind(
            &c.style_ref,
            format!("{sid}/character_ranges/{n}/style_ref"),
        )?;
        if let Some(language) = &c.applied_language {
            if !key(language) {
                return Err(err(
                    Code::InvalidField,
                    format!("{sid}/character_ranges/{n}/applied_language"),
                ));
            }
            charge(&mut payload, language.len(), b.payload_bytes, sid)?;
        }
        charge(&mut payload, 64, b.payload_bytes, sid)?;
    }
    canceled(token)?;
    Ok(issues)
}
