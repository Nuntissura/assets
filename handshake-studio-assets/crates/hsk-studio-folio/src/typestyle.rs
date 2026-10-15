//! `StudioTypeStyle` (schema `hsk.studio.type_style@1`, id prefix `STYS`, both already registered in
//! Accord) and the per-document record set `StudioTypeStyleRecords` that carries them beside the
//! document, like the design, raster and vector record sets (`StudioDocument` gains no fields).
//! Added by SMB-FOLIO-PAINT-BLUR-STYLE: text stories already bind paragraphs and character ranges to
//! `STYS` ids (`StoryParagraph::style_ref`, `StoryRange::style_ref`); until now no Folio record
//! held the style itself, so the host had to resolve every id.
//!
//! Members (STU-TYP-025, STU-TYP-200..203): the scope (`character` | `paragraph`, STU-TYP-200), a
//! slash-path name, an optional `based_on` parent of the same scope (a cycle is a validation error,
//! STU-TYP-201), an optional `next_style` (paragraph scope only, STU-TYP-203), and the attributes the
//! importers carry today: font family and style names (font files are resolved by Type, never
//! here), size, RGBA fill, line height, letter spacing and paragraph alignment. Every attribute is
//! optional; absent means "not set by this style" (it falls through the STU-TYP-201 order).
//!
//! [`TypeStyleResolver`] answers `hsk.studio.type_style@1` lookups from a record set and defers
//! everything else to the caller's resolver, so `validate_text_story` reports only the style ids
//! that the set does not hold; [`check_story_style_scopes`] refuses a story that binds a paragraph
//! to a character style or a range to a paragraph style.
//!
//! Declared gaps, not invented here: the full character/paragraph attribute groups of 14.7 (OpenType
//! features, decoration, baseline shift, indents, spacing, hyphenation, composer, nested styles;
//! GREP styles are carried since SMB-LAYOUT-UI as `grep_styles`), override reporting (STU-TYP-202) and variable-bound fields (STU-TYP-204) are not stored;
//! the `StudioStyleRegistry` storage and publishing fields (14.6/14.10) stay with the design records
//! (`PAINT` entries only). Bounds the spec does not state are checked for finiteness and their
//! physical domain only (size and line-height length positive, percent line height positive).
//! Rendering of the attributes belongs to Type and composite; nothing is rendered from this set yet.
//!
//! The records arrive from untrusted wire JSON: integer arithmetic is denied here (workflow
//! V4-01); counts are charged through the checked `Ctx` budget.
#![deny(clippy::arithmetic_side_effects)]
use super::{
    Budget, Code, Length, Resolution, ResolutionIssue, Resolver, Result, StudioDocument,
    StudioTextStory, TileRef, canceled, err, id,
    values::{Ctx, StudioRgba, label, slash_path},
};
use hsk_studio_accord::{CancellationToken, SCHEMA_STUDIO_TYPE_STYLE};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

schema_token!(TypeStyleSchema, "hsk.studio.type_style@1");
schema_token!(TypeStyleRecordsSchema, "hsk.studio.type_style_records@1");

/// Type styles per record set (a Folio hard ceiling; the spec declares none).
pub const MAX_TYPE_STYLES: usize = 4096;
/// Font family and style name length in bytes (a Folio hard ceiling).
pub const MAX_FONT_NAME_BYTES: usize = 256;

/// STU-TYP-200 style scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TypeStyleScope {
    Character,
    Paragraph,
}

/// Line height: automatic (font metrics), a percent of the font size, or an absolute length.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "line_height_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioLineHeight {
    Auto,
    Percent { value: f64 },
    Length { length: Length },
}

/// Letter spacing (tracking): a percent of the font size or an absolute length; either sign.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "spacing_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioLetterSpacing {
    Percent { value: f64 },
    Length { length: Length },
}

/// Horizontal paragraph alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioTextAlign {
    Left,
    Center,
    Right,
    Justified,
}

/// One `StudioTypeStyle`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTypeStyle {
    pub schema_id: TypeStyleSchema,
    /// `STYS-{uuid_v7}`; unique in the set.
    pub style_id: String,
    pub scope: TypeStyleScope,
    /// Slash-path name.
    pub name: String,
    /// Parent style of the same scope in the same set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub based_on: Option<String>,
    /// Paragraph scope only: the style applied to the following paragraph (same set, paragraph).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_style: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_style: Option<String>,
    /// Positive length in any unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<Length>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<StudioRgba>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_height: Option<StudioLineHeight>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub letter_spacing: Option<StudioLetterSpacing>,
    /// Paragraph scope only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<StudioTextAlign>,
    /// STU-LAY-038 pattern (GREP) styles, paragraph scope only, stored ON the style (lane
    /// SMB-LAYOUT-UI): each rule applies a character style to every regular-expression match inside
    /// paragraphs carrying this style. Inherited through `based_on` (parent rules first); the
    /// pattern syntax is the linear-time engine's (hsk-studio-layout `grep`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grep_styles: Vec<StudioGrepStyle>,
}

/// One pattern style rule.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioGrepStyle {
    /// Regular expression, 1..=`MAX_GREP_PATTERN_BYTES` bytes.
    pub pattern: String,
    /// `STYS` id of a character style of the same set.
    pub character_style: String,
}

/// Pattern bytes of one GREP rule (a Folio hard ceiling).
pub const MAX_GREP_PATTERN_BYTES: usize = 1024;
/// GREP rules per paragraph style (a Folio hard ceiling).
pub const MAX_GREP_STYLES: usize = 64;

/// The type styles of one document. An empty set is not encoded beyond its header.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct StudioTypeStyleRecords {
    pub schema_id: TypeStyleRecordsSchema,
    /// `SDOC` id of the annotated document.
    pub document_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub styles: Vec<StudioTypeStyle>,
}
impl StudioTypeStyleRecords {
    /// An empty set for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self {
            schema_id: TypeStyleRecordsSchema::V1,
            document_id: document_id.into(),
            styles: Vec::new(),
        }
    }
    /// The style with this id.
    pub fn style(&self, style_id: &str) -> Option<&StudioTypeStyle> {
        self.styles.iter().find(|s| s.style_id == style_id)
    }
}

/// JSON schema (draft 2020-12) of the type-style record set.
pub fn type_style_records_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<StudioTypeStyleRecords>()
}

/// Answers `hsk.studio.type_style@1` from `styles` (Available when the set holds the id) and defers
/// every other lookup, and type-style ids the set does not hold, to `inner`.
pub struct TypeStyleResolver<'a> {
    pub styles: &'a StudioTypeStyleRecords,
    pub inner: &'a dyn Resolver,
}
impl Resolver for TypeStyleResolver<'_> {
    fn primitive(&self, schema_id: &str, object_id: &str) -> Resolution {
        if schema_id == SCHEMA_STUDIO_TYPE_STYLE && self.styles.style(object_id).is_some() {
            Resolution::Available
        } else {
            self.inner.primitive(schema_id, object_id)
        }
    }
    fn profile(&self, profile_id: &str) -> Resolution {
        self.inner.profile(profile_id)
    }
    fn tile(&self, tile: &TileRef) -> Resolution {
        self.inner.tile(tile)
    }
    fn format_supported(&self, format: &str) -> bool {
        self.inner.format_supported(format)
    }
    fn primitive_layer_binding(&self, kind: &str, schema_id: &str) -> bool {
        self.inner.primitive_layer_binding(kind, schema_id)
    }
    fn media(&self, id: &str, digest: &super::ContentDigest) -> Resolution {
        self.inner.media(id, digest)
    }
    fn story_content(&self, id: &str, digest: &super::ContentDigest) -> Resolution {
        self.inner.story_content(id, digest)
    }
}

fn check(ok: bool, target: &str, field: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(err(Code::InvalidField, format!("{target}/{field}")))
    }
}
fn positive_length(l: &Length) -> bool {
    l.value.is_finite() && l.value > 0.0
}
fn font_name(s: &str) -> bool {
    label(s) && s.len() <= MAX_FONT_NAME_BYTES
}

fn validate_style(s: &StudioTypeStyle, cx: &mut Ctx) -> Result<()> {
    cx.tick()?;
    let t = s.style_id.as_str();
    id(t, "STYS", "style_id")?;
    check(slash_path(&s.name), t, "name")?;
    cx.text(&s.name, t)?;
    for name in [&s.font_family, &s.font_style].into_iter().flatten() {
        check(font_name(name), t, "font")?;
        cx.text(name, t)?;
    }
    if let Some(size) = &s.font_size {
        check(positive_length(size), t, "font_size")?;
    }
    if let Some(fill) = &s.fill {
        fill.validate(t, cx)?;
    }
    match &s.line_height {
        Some(StudioLineHeight::Percent { value }) => {
            check(value.is_finite() && *value > 0.0, t, "line_height")?
        }
        Some(StudioLineHeight::Length { length }) => {
            check(positive_length(length), t, "line_height")?
        }
        Some(StudioLineHeight::Auto) | None => {}
    }
    match &s.letter_spacing {
        Some(StudioLetterSpacing::Percent { value }) => {
            check(value.is_finite(), t, "letter_spacing")?
        }
        Some(StudioLetterSpacing::Length { length }) => {
            check(length.value.is_finite(), t, "letter_spacing")?
        }
        None => {}
    }
    if s.scope == TypeStyleScope::Character {
        check(s.align.is_none(), t, "align")?;
        check(s.next_style.is_none(), t, "next_style")?;
        check(s.grep_styles.is_empty(), t, "grep_styles")?;
    }
    check(s.grep_styles.len() <= MAX_GREP_STYLES, t, "grep_styles")?;
    for g in &s.grep_styles {
        check(
            !g.pattern.is_empty() && g.pattern.len() <= MAX_GREP_PATTERN_BYTES,
            t,
            "grep_styles/pattern",
        )?;
        id(&g.character_style, "STYS", &format!("{t}/grep_styles/character_style"))?;
    }
    Ok(())
}

/// The single type-style validator: ids, document binding, names and attribute domains, colour
/// profiles of fills (unresolved ones reported as [`ResolutionIssue`]s), `based_on` / `next_style`
/// references inside the set with matching scope, and `based_on` cycles (`Code::RenderCycle`,
/// STU-TYP-201: a cycle is a validation error). Styles count against `Budget::nodes`, names
/// against `Budget::payload_bytes`.
pub fn validate_type_style_records(
    r: &StudioTypeStyleRecords,
    d: &StudioDocument,
    b: Budget,
    token: &CancellationToken,
    resolver: &dyn Resolver,
) -> Result<Vec<ResolutionIssue>> {
    let mut cx = Ctx::new(b, token, resolver);
    cx.tick()?;
    id(&r.document_id, "SDOC", "document_id")?;
    if r.document_id != d.document_id {
        return Err(err(Code::InvalidField, "document_id"));
    }
    if r.styles.len() > MAX_TYPE_STYLES {
        return Err(err(Code::BudgetExceeded, "styles"));
    }
    cx.nodes(r.styles.len(), "styles")?;
    let mut scopes: BTreeMap<&str, TypeStyleScope> = BTreeMap::new();
    for s in &r.styles {
        validate_style(s, &mut cx)?;
        if scopes.insert(&s.style_id, s.scope).is_some() {
            return Err(err(Code::DuplicateIdentity, &s.style_id));
        }
    }
    for s in &r.styles {
        cx.tick()?;
        let t = s.style_id.as_str();
        for g in &s.grep_styles {
            let target = format!("{t}/grep_styles/character_style");
            match scopes.get(g.character_style.as_str()) {
                None => return Err(err(Code::DanglingReference, target)),
                Some(TypeStyleScope::Paragraph) => return Err(err(Code::PayloadMismatch, target)),
                Some(_) => {}
            }
        }
        for (field, reference, scope) in [
            ("based_on", &s.based_on, s.scope),
            ("next_style", &s.next_style, TypeStyleScope::Paragraph),
        ] {
            let Some(reference) = reference else {
                continue;
            };
            let target = format!("{t}/{field}");
            id(reference, "STYS", &target)?;
            match scopes.get(reference.as_str()) {
                None => return Err(err(Code::DanglingReference, target)),
                Some(found) if *found != scope => {
                    return Err(err(Code::PayloadMismatch, target));
                }
                Some(_) => {}
            }
        }
    }
    // `based_on` is a forest: follow each chain, bounded by the set size.
    let parent: BTreeMap<&str, &str> = r
        .styles
        .iter()
        .filter_map(|s| s.based_on.as_deref().map(|p| (s.style_id.as_str(), p)))
        .collect();
    for start in parent.keys() {
        cx.tick()?;
        let mut seen = BTreeSet::new();
        let mut cursor = Some(*start);
        while let Some(node) = cursor {
            if !seen.insert(node) {
                return Err(err(Code::RenderCycle, *start));
            }
            cursor = parent.get(node).copied();
        }
    }
    canceled(token)?;
    Ok(cx.issues)
}

/// Refuses a story whose paragraph binds a `character` style or whose range binds a `paragraph`
/// style held by `styles` (`Code::PayloadMismatch`, target the binding). Ids the set does not hold
/// are left to `validate_text_story` and the caller's resolver.
pub fn check_story_style_scopes(
    story: &StudioTextStory,
    styles: &StudioTypeStyleRecords,
) -> Result<()> {
    let sid = story.story_id.as_str();
    let paragraphs = story
        .paragraphs
        .iter()
        .enumerate()
        .map(|(n, p)| (n, &p.style_ref, TypeStyleScope::Paragraph, "paragraphs"));
    let ranges = story
        .character_ranges
        .iter()
        .enumerate()
        .map(|(n, c)| (n, &c.style_ref, TypeStyleScope::Character, "character_ranges"));
    for (n, reference, scope, list) in paragraphs.chain(ranges) {
        let Some(style) = reference.as_deref().and_then(|r| styles.style(r)) else {
            continue;
        };
        if style.scope != scope {
            return Err(err(
                Code::PayloadMismatch,
                format!("{sid}/{list}/{n}/style_ref"),
            ));
        }
    }
    Ok(())
}
