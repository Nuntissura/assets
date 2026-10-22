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
//! Composition (lane SMB-LAYOUT-COMPOSER-WIRING): [`StudioCompositionSettings`] carries the
//! composer axes, hyphenation, H&J spacing, single-word justification and auto-leading amount
//! (STU-TYP-135, 146-148, 150-151) per paragraph style (`style_composition`) and as local paragraph
//! overrides (`paragraph_composition`, STU-TYP-201 step 4), beside the styles so the style record
//! and its wire form are unchanged; empty lists are not encoded, so existing documents keep their
//! bytes. Not stored: `composer_name` and the composer registry (STU-TYP-136), kinsoku/mojikumi
//! tables and the leading model (137-139), `balance_ragged_lines` (141), the nine-value
//! justification mode (145), `allow_arbitrary_hyphenation`, `hyphenation_preference`, provider
//! style and exceptions (151-154), `use_auto_leading` as a flag separate from `line_height`.
//!
//! Declared gaps, not invented here: the full character/paragraph attribute groups of 14.7 (OpenType
//! features, decoration, baseline shift, indents, spacing, nested styles;
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

/// STU-TYP-135 `composition_scope`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioCompositionScope {
    /// Each line composed in isolation (Adobe Single-line Composer).
    SingleLine,
    /// All lines of the paragraph weighed together (Adobe Paragraph Composer).
    EveryLine,
}

/// STU-TYP-135 `composition_engine`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioCompositionEngine {
    LatinCjk,
    WorldReady,
    Adornment,
}

/// STU-TYP-146 `single_word_justification`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioSingleWordJustification {
    LeftAlign,
    CenterAlign,
    RightAlign,
    FullyJustified,
}

/// STU-TYP-147 word-spacing hard bounds (percent).
pub const WORD_SPACING_PERCENT: (f64, f64) = (0.0, 1000.0);
/// STU-TYP-147 letter-spacing hard bounds (percent).
pub const LETTER_SPACING_PERCENT: (f64, f64) = (-100.0, 500.0);
/// STU-TYP-147 glyph-scaling hard bounds (percent).
pub const GLYPH_SCALING_PERCENT: (f64, f64) = (50.0, 200.0);
/// STU-TYP-148 `auto_leading_amount` hard bounds (percent).
pub const AUTO_LEADING_PERCENT: (f64, f64) = (0.0, 500.0);
/// STU-TYP-150 `hyphen_weight` hard maximum.
pub const MAX_HYPHEN_WEIGHT: u32 = 100;
/// Paragraph composition overrides per record set (a Folio hard ceiling; the spec declares none).
pub const MAX_PARAGRAPH_COMPOSITION: usize = 65_536;

/// Paragraph composition settings (lane SMB-LAYOUT-COMPOSER-WIRING): the composer axes of
/// STU-TYP-135, the hyphenation record of STU-TYP-150/151, the H&J spacing block of STU-TYP-147,
/// single-word justification (STU-TYP-146) and the auto-leading amount (STU-TYP-148), under the spec
/// field names. Every field is optional: absent means "not set here" and falls through the
/// STU-TYP-201 order (paragraph style `based_on` chain root first, then the local paragraph
/// override); a paragraph with nothing set anywhere composes as before these fields existed.
/// Spacing fields are percent (word and letter spacing of the font's word space, glyph scaling of
/// the glyph width); counts are letters / lines. `hyphenate_ladder_limit` = 0 is the UNLIMITED
/// sentinel, not a prohibition (STU-TYP-150). `hyphenation_zone` is consumed only with
/// `composition_scope = single_line` (STU-TYP-150). Bounds: the STU-TYP-147/148/150 hard bounds
/// (constants above); where the spec states UNKNOWN only finiteness and the physical domain are
/// checked. Within one settings record a stated minimum / desired / maximum triple must be
/// ordered; ordering across inheritance is checked by the consumer after resolution.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioCompositionSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composition_scope: Option<StudioCompositionScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composition_engine: Option<StudioCompositionEngine>,
    /// STU-TYP-150 master switch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyphenation: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyphenate_words_longer_than: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyphenate_after_first: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyphenate_before_last: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyphenate_ladder_limit: Option<u32>,
    /// 0..=100; LOWER = more hyphens (STU-TYP-150 direction).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyphen_weight: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyphenation_zone: Option<Length>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyphenate_capitalized_words: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyphenate_last_word: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyphenate_across_columns: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_spacing_minimum: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_spacing_desired: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_spacing_maximum: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub letter_spacing_minimum: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub letter_spacing_desired: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub letter_spacing_maximum: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glyph_scaling_minimum: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glyph_scaling_desired: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glyph_scaling_maximum: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub single_word_justification: Option<StudioSingleWordJustification>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_leading_amount: Option<f64>,
}
impl StudioCompositionSettings {
    /// True when no field is set.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
    /// `self` with every field `top` sets replaced by `top`'s value (STU-TYP-201: later wins).
    pub fn overlaid(&self, top: &Self) -> Self {
        Self {
            composition_scope: top.composition_scope.or(self.composition_scope),
            composition_engine: top.composition_engine.or(self.composition_engine),
            hyphenation: top.hyphenation.or(self.hyphenation),
            hyphenate_words_longer_than: top.hyphenate_words_longer_than.or(self.hyphenate_words_longer_than),
            hyphenate_after_first: top.hyphenate_after_first.or(self.hyphenate_after_first),
            hyphenate_before_last: top.hyphenate_before_last.or(self.hyphenate_before_last),
            hyphenate_ladder_limit: top.hyphenate_ladder_limit.or(self.hyphenate_ladder_limit),
            hyphen_weight: top.hyphen_weight.or(self.hyphen_weight),
            hyphenation_zone: top.hyphenation_zone.clone().or_else(|| self.hyphenation_zone.clone()),
            hyphenate_capitalized_words: top.hyphenate_capitalized_words.or(self.hyphenate_capitalized_words),
            hyphenate_last_word: top.hyphenate_last_word.or(self.hyphenate_last_word),
            hyphenate_across_columns: top.hyphenate_across_columns.or(self.hyphenate_across_columns),
            word_spacing_minimum: top.word_spacing_minimum.or(self.word_spacing_minimum),
            word_spacing_desired: top.word_spacing_desired.or(self.word_spacing_desired),
            word_spacing_maximum: top.word_spacing_maximum.or(self.word_spacing_maximum),
            letter_spacing_minimum: top.letter_spacing_minimum.or(self.letter_spacing_minimum),
            letter_spacing_desired: top.letter_spacing_desired.or(self.letter_spacing_desired),
            letter_spacing_maximum: top.letter_spacing_maximum.or(self.letter_spacing_maximum),
            glyph_scaling_minimum: top.glyph_scaling_minimum.or(self.glyph_scaling_minimum),
            glyph_scaling_desired: top.glyph_scaling_desired.or(self.glyph_scaling_desired),
            glyph_scaling_maximum: top.glyph_scaling_maximum.or(self.glyph_scaling_maximum),
            single_word_justification: top.single_word_justification.or(self.single_word_justification),
            auto_leading_amount: top.auto_leading_amount.or(self.auto_leading_amount),
        }
    }
    /// The single settings validator: STU-TYP-147 / 148 / 150 hard bounds, finiteness, a
    /// non-negative finite hyphenation zone and ordered triples within this record.
    /// `Code::InvalidField`, target `{target}/{field}`.
    pub fn validate(&self, target: &str) -> Result<()> {
        let in_range = |v: Option<f64>, (lo, hi): (f64, f64)| v.is_none_or(|v| v.is_finite() && (lo..=hi).contains(&v));
        let pair = |x: Option<f64>, y: Option<f64>| match (x, y) {
            (Some(x), Some(y)) => x <= y,
            _ => true,
        };
        let triples = [
            ("word_spacing", WORD_SPACING_PERCENT, [self.word_spacing_minimum, self.word_spacing_desired, self.word_spacing_maximum]),
            ("letter_spacing", LETTER_SPACING_PERCENT, [self.letter_spacing_minimum, self.letter_spacing_desired, self.letter_spacing_maximum]),
            ("glyph_scaling", GLYPH_SCALING_PERCENT, [self.glyph_scaling_minimum, self.glyph_scaling_desired, self.glyph_scaling_maximum]),
        ];
        for (field, bounds, [min, desired, max]) in triples {
            for (suffix, v) in [("minimum", min), ("desired", desired), ("maximum", max)] {
                check(in_range(v, bounds), target, &format!("{field}_{suffix}"))?;
            }
            check(pair(min, desired) && pair(desired, max) && pair(min, max), target, field)?;
        }
        check(in_range(self.auto_leading_amount, AUTO_LEADING_PERCENT), target, "auto_leading_amount")?;
        check(self.hyphen_weight.is_none_or(|w| w <= MAX_HYPHEN_WEIGHT), target, "hyphen_weight")?;
        check(
            self.hyphenation_zone.as_ref().is_none_or(|z| z.value.is_finite() && z.value >= 0.0),
            target,
            "hyphenation_zone",
        )?;
        Ok(())
    }
}

/// The composition settings a paragraph style states (paragraph scope only; one entry per style).
/// Kept beside the styles rather than on `StudioTypeStyle` so existing style records, their wire
/// form and every struct literal that builds one stay unchanged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioStyleComposition {
    /// `STYS` id of a paragraph style of the same set.
    pub style_id: String,
    pub settings: StudioCompositionSettings,
}

/// STU-TYP-201 step 4 local paragraph override of the composition settings, keyed like the
/// layout-flow paragraph records: the text story and the paragraph's start byte.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioParagraphComposition {
    /// `STXT` id of the text story holding the paragraph (a document-local reference).
    pub text_id: String,
    /// Start byte of the paragraph (`StoryParagraph::start`); checked against the story by the
    /// composer, which refuses an override naming no paragraph.
    pub paragraph_start: u64,
    pub settings: StudioCompositionSettings,
}

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
    /// Paragraph-style composition settings (absent = none stated; lane SMB-LAYOUT-COMPOSER-WIRING).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub style_composition: Vec<StudioStyleComposition>,
    /// Local paragraph overrides of the composition settings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paragraph_composition: Vec<StudioParagraphComposition>,
}
impl StudioTypeStyleRecords {
    /// An empty set for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self {
            schema_id: TypeStyleRecordsSchema::V1,
            document_id: document_id.into(),
            styles: Vec::new(),
            style_composition: Vec::new(),
            paragraph_composition: Vec::new(),
        }
    }
    /// The style with this id.
    pub fn style(&self, style_id: &str) -> Option<&StudioTypeStyle> {
        self.styles.iter().find(|s| s.style_id == style_id)
    }
    /// The composition settings the paragraph style `style_id` itself states.
    pub fn style_composition(&self, style_id: &str) -> Option<&StudioCompositionSettings> {
        self.style_composition.iter().find(|c| c.style_id == style_id).map(|c| &c.settings)
    }
    /// The local composition override of the paragraph starting at `paragraph_start` in `text_id`.
    pub fn paragraph_composition(&self, text_id: &str, paragraph_start: u64) -> Option<&StudioCompositionSettings> {
        self.paragraph_composition
            .iter()
            .find(|c| c.text_id == text_id && c.paragraph_start == paragraph_start)
            .map(|c| &c.settings)
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
    fn font(&self, family: &str, style: &str) -> Resolution {
        self.inner.font(family, style)
    }
    fn audio(&self, audio_id: &str) -> Resolution {
        self.inner.audio(audio_id)
    }
    fn lut(&self, lut: &str, content_digest: Option<&super::ContentDigest>) -> Resolution {
        self.inner.lut(lut, content_digest)
    }
    fn placed_link(&self, uri: &str, content_digest: &str) -> Resolution {
        self.inner.placed_link(uri, content_digest)
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
    // Composition settings: per paragraph style (one entry each) and per paragraph override (one
    // per text story and start byte); every settings record within its bounds.
    if r.style_composition.len() > MAX_TYPE_STYLES {
        return Err(err(Code::BudgetExceeded, "style_composition"));
    }
    if r.paragraph_composition.len() > MAX_PARAGRAPH_COMPOSITION {
        return Err(err(Code::BudgetExceeded, "paragraph_composition"));
    }
    cx.nodes(r.style_composition.len(), "style_composition")?;
    cx.nodes(r.paragraph_composition.len(), "paragraph_composition")?;
    let mut composed = BTreeSet::new();
    for c in &r.style_composition {
        cx.tick()?;
        let target = format!("style_composition/{}", c.style_id);
        id(&c.style_id, "STYS", &target)?;
        match scopes.get(c.style_id.as_str()) {
            None => return Err(err(Code::DanglingReference, target)),
            Some(TypeStyleScope::Character) => return Err(err(Code::PayloadMismatch, target)),
            Some(TypeStyleScope::Paragraph) => {}
        }
        if !composed.insert(c.style_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, target));
        }
        c.settings.validate(&target)?;
    }
    let mut overridden = BTreeSet::new();
    for c in &r.paragraph_composition {
        cx.tick()?;
        let target = format!("paragraph_composition/{}/{}", c.text_id, c.paragraph_start);
        id(&c.text_id, "STXT", &target)?;
        if !overridden.insert((c.text_id.as_str(), c.paragraph_start)) {
            return Err(err(Code::DuplicateIdentity, target));
        }
        c.settings.validate(&target)?;
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
