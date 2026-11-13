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
//! Indents and spacing (lane SMB-TYPE-INDENT-SPACING-1, STU-TYP-185) ride on the same settings:
//! `left_indent`, `right_indent`, `first_line_indent`, `last_line_indent`, `space_before`,
//! `space_after` (points, finite, either sign, no bound: [`INDENT_SPACING_PARAMS`] holds the spec's
//! seven-field rows with every UNKNOWN as `None`) and `space_between_same_style`.
//!
//! Character attributes (lane SMB-TYPE-CHARACTER-ATTRIBUTES-1, STU-TYP-155..161, 133, 170, 174,
//! 192, 209): [`StudioCharacterAttributes`] (numeric block, enumerations, booleans, the OpenType
//! feature map, case-synthesis parameters, underline and strikethrough records, hyperlink,
//! `digits_type`) per style (`style_character`, either scope) and as local overrides over story
//! byte spans (`character_overrides`, non-overlapping per story), resolved paragraph style chain,
//! then character style chain, then the override ([`StudioTypeStyleRecords::resolved_character`]);
//! the stored stroke-style registry members (`stroke_styles`, built-ins `solid`, `dotted`, `wavy`)
//! and the language records (`languages`, keyed by the name a range's `applied_language` holds).
//! Folio stores and validates; rendering belongs to Type and the hosts.
//!
//! Declared gaps, not invented here: the paragraph attribute groups of 14.7 (keep options, rules,
//! tabs, lists, nested styles;
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
    Budget, Code, GeometryUnit, Length, Resolution, ResolutionIssue, Resolver, Result, StudioDocument,
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
/// field names, and the STU-TYP-185 indent and spacing block (lane SMB-TYPE-INDENT-SPACING-1, lengths
/// in points). Every field is optional: absent means "not set here" and falls through the
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
    /// STU-TYP-185 indents and spacing (lane SMB-TYPE-INDENT-SPACING-1): lengths in points
    /// (STU-TYP-108 stores points), finite, either sign; the spec states no bound
    /// ([`INDENT_SPACING_PARAMS`], STU-TYP-106: nothing clamped). Unset = 0 (the engine default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left_indent: Option<Length>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right_indent: Option<Length>,
    /// Added to `left_indent` on the paragraph's first line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_line_indent: Option<Length>,
    /// A distinct fourth indent of the paragraph's last line from the right; never folded into
    /// `right_indent` (STU-TYP-185).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_line_indent: Option<Length>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_before: Option<Length>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_after: Option<Length>,
    /// STU-TYP-185: true suppresses `space_before` / `space_after` between adjacent paragraphs
    /// sharing a paragraph style.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_between_same_style: Option<bool>,
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
            left_indent: top.left_indent.clone().or_else(|| self.left_indent.clone()),
            right_indent: top.right_indent.clone().or_else(|| self.right_indent.clone()),
            first_line_indent: top.first_line_indent.clone().or_else(|| self.first_line_indent.clone()),
            last_line_indent: top.last_line_indent.clone().or_else(|| self.last_line_indent.clone()),
            space_before: top.space_before.clone().or_else(|| self.space_before.clone()),
            space_after: top.space_after.clone().or_else(|| self.space_after.clone()),
            space_between_same_style: top.space_between_same_style.or(self.space_between_same_style),
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
        // STU-TYP-185 / 108: points, finite, either sign; no bound (STU-TYP-106).
        for (field, v) in [
            ("left_indent", &self.left_indent),
            ("right_indent", &self.right_indent),
            ("first_line_indent", &self.first_line_indent),
            ("last_line_indent", &self.last_line_indent),
            ("space_before", &self.space_before),
            ("space_after", &self.space_after),
        ] {
            check(v.as_ref().is_none_or(|l| l.unit == GeometryUnit::Pt && l.value.is_finite()), target, field)?;
        }
        Ok(())
    }
}

/// One STU-TYP-105 seven-field parameter record held as data (Studio-minted name, lane
/// SMB-TYPE-INDENT-SPACING-1, for the parameter-contract work of MT-30972). `None` is the spec's
/// literal `UNKNOWN`: that side is not clamped (STU-TYP-106). Hard and soft bounds are separate
/// fields and never stand in for each other.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TypographyParamRow {
    /// The spec field name, as stored on [`StudioCompositionSettings`].
    pub key: &'static str,
    pub hard_min: Option<f64>,
    pub hard_max: Option<f64>,
    pub soft_min: Option<f64>,
    pub soft_max: Option<f64>,
    pub default: Option<f64>,
    /// STU-TYP-105 unit token.
    pub unit: &'static str,
    /// Decimal places carried and round-tripped.
    pub precision: Option<u32>,
}

const fn unknown_bounds_points(key: &'static str) -> TypographyParamRow {
    TypographyParamRow { key, hard_min: None, hard_max: None, soft_min: None, soft_max: None, default: Some(0.0), unit: "points", precision: None }
}

/// STU-TYP-185 parameter table, row for row: every bound and the precision UNKNOWN, default 0,
/// unit points.
pub const INDENT_SPACING_PARAMS: [TypographyParamRow; 6] = [
    unknown_bounds_points("left_indent"),
    unknown_bounds_points("right_indent"),
    unknown_bounds_points("first_line_indent"),
    unknown_bounds_points("last_line_indent"),
    unknown_bounds_points("space_before"),
    unknown_bounds_points("space_after"),
];

// ---------------------------------------------------------------------------------------------
// Character attributes (lane SMB-TYPE-CHARACTER-ATTRIBUTES-1; STU-TYP-155..161, 133, 170, 174,
// 192, 209). Stored beside the styles, like the composition settings: per style
// (`style_character`) and as local overrides over story byte spans (`character_overrides`), plus
// the stroke-style registry (`stroke_styles`) and the language records (`languages`).
// ---------------------------------------------------------------------------------------------

/// Local character overrides per record set (a Folio hard ceiling; the spec declares none).
pub const MAX_CHARACTER_OVERRIDES: usize = 65_536;
/// OpenType feature entries per character-attribute record (a Folio hard ceiling).
pub const MAX_RECORD_FEATURES: usize = 256;
/// Stored stroke styles per record set (a Folio hard ceiling).
pub const MAX_STROKE_STYLES: usize = 1024;
/// Language records per record set (a Folio hard ceiling).
pub const MAX_LANGUAGES: usize = 1024;
/// Dash or stripe entries of one stroke style (a Folio hard ceiling).
pub const MAX_STROKE_PATTERN: usize = 32;
/// Bytes of a hyperlink URL (a Folio hard ceiling).
pub const MAX_URL_BYTES: usize = 2048;
/// Bytes of one quote mark of a language's quote pair (a Folio hard ceiling).
pub const MAX_QUOTE_BYTES: usize = 16;
/// Bytes of an ICU locale name (a Folio hard ceiling).
pub const MAX_ICU_LOCALE_BYTES: usize = 64;
/// The stroke-style registry's built-in members (STU-TYP-192: "Solid is a member"; `dotted` and
/// `wavy` are the STU-TYP-160 `decoration_style` members, which select a registry style). A stored
/// style cannot take these names.
pub const BUILTIN_STROKE_STYLES: [&str; 3] = ["solid", "dotted", "wavy"];
/// The decoration `type` that draws nothing (STU-TYP-156 `underline_type`: `off`, plus the registry).
pub const DECORATION_OFF: &str = "off";

/// STU-TYP-156 `kerning_mode`. `metrics_roman_only` is its own member, never folded into `metrics`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioKerningMode {
    None,
    Metrics,
    Optical,
    MetricsRomanOnly,
}
/// STU-TYP-156 `capitalization` (a render transform, STU-TYP-158: stored characters never change).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioCapitalization {
    Normal,
    SmallCaps,
    AllCaps,
    AllSmallCaps,
    LowerCase,
}
/// STU-TYP-156 `position`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioCharacterPosition {
    Normal,
    Superscript,
    Subscript,
    OtSuperscript,
    OtSubscript,
    OtNumerator,
    OtDenominator,
}
/// STU-TYP-156 `anti_alias_method` (five members; a four-member source maps `strong` to `strong`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioAntiAliasMethod {
    None,
    Sharp,
    Crisp,
    Strong,
    Smooth,
}
/// STU-TYP-156 `character_alignment`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioCharacterAlignment {
    Baseline,
    EmTop,
    EmCenter,
    EmBottom,
    IcfTop,
    IcfBottom,
}
/// STU-TYP-156 `alternate_glyph_form`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioAlternateGlyphForm {
    None,
    Traditional,
    Expert,
    Jis78,
    Jis83,
    Jis90,
    Jis04,
    Nlc,
    HalfWidth,
    ThirdWidth,
    QuarterWidth,
    FullWidth,
    ProportionalWidth,
}
/// STU-TYP-156 `baseline_direction`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioBaselineDirection {
    Standard,
    VerticalRotated,
    TateChuYoko,
}
/// STU-TYP-156 `font_baseline_option`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioFontBaselineOption {
    Normal,
    Superscript,
    Subscript,
}
/// STU-TYP-133 `digits_type`: the digit glyph set chosen at shaping time; never rewrites the
/// stored characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioDigitsType {
    Default,
    Arabic,
    Hindi,
    Farsi,
    Native,
    FullFarsi,
    Thai,
    Lao,
    Devanagari,
    Bengali,
    Gurmukhi,
    Gujarati,
    Oriya,
    Tamil,
    Telugu,
    Kannada,
    Malayalam,
    Tibetan,
    Khmer,
    Burmese,
}

/// Every character-attribute enumeration with its complete member list (STU-TYP-156, 133;
/// STU-TYP-221: a model discovers the members without trial and error). The decoration types are
/// `off` plus the stroke-style registry (STU-TYP-192), not a closed list. Pinned to the serde
/// names by `tests/character_attributes.rs`.
pub const CHARACTER_ENUMERATIONS: [(&str, &[&str]); 9] = [
    ("kerning_mode", &["none", "metrics", "optical", "metrics_roman_only"]),
    ("capitalization", &["normal", "small_caps", "all_caps", "all_small_caps", "lower_case"]),
    ("position", &["normal", "superscript", "subscript", "ot_superscript", "ot_subscript", "ot_numerator", "ot_denominator"]),
    ("anti_alias_method", &["none", "sharp", "crisp", "strong", "smooth"]),
    ("character_alignment", &["baseline", "em_top", "em_center", "em_bottom", "icf_top", "icf_bottom"]),
    (
        "alternate_glyph_form",
        &["none", "traditional", "expert", "jis78", "jis83", "jis90", "jis04", "nlc", "half_width", "third_width", "quarter_width", "full_width", "proportional_width"],
    ),
    ("baseline_direction", &["standard", "vertical_rotated", "tate_chu_yoko"]),
    ("font_baseline_option", &["normal", "superscript", "subscript"]),
    (
        "digits_type",
        &[
            "default", "arabic", "hindi", "farsi", "native", "full_farsi", "thai", "lao", "devanagari", "bengali", "gurmukhi", "gujarati", "oriya", "tamil", "telugu",
            "kannada", "malayalam", "tibetan", "khmer", "burmese",
        ],
    ),
];

/// A decoration weight or offset (STU-TYP-159 points; STU-TYP-160 `auto`, `percent`, `pixels`,
/// mapped onto the one decoration record: `auto` resolves from font metrics at composition time).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "length_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioDecorationLength {
    Auto,
    Points { value: f64 },
    Percent { value: f64 },
    Pixels { value: f64 },
}
impl StudioDecorationLength {
    /// The stated number; `None` for `auto`.
    pub fn value(&self) -> Option<f64> {
        match self {
            Self::Auto => None,
            Self::Points { value } | Self::Percent { value } | Self::Pixels { value } => Some(*value),
        }
    }
}

/// A decoration colour: a `StudioSwatch` reference (STU-TYP-159, id prefix `SSWT`) or `auto`, the
/// run's fill colour (STU-TYP-160).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "colour_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioDecorationColour {
    Auto,
    Swatch { swatch_id: String },
}

/// STU-TYP-159 decoration record: underline and strikethrough are two independent records of this
/// shape; every field is stored separately and gap fields round-trip even when inert. STU-TYP-160
/// maps onto it without a second primitive: `decoration_style` is the `type` (a registry style:
/// `solid`, `dotted`, `wavy`), `decoration_thickness` the `weight`, `decoration_offset` the
/// `offset`, `decoration_colour` the `colour`; `skip_ink` is a first-class Studio field.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioDecoration {
    /// `off` or a stroke-style registry name (STU-TYP-156 `underline_type` / `strikethrough_type`).
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub stroke_type: Option<String>,
    /// Hard minimum 0 (any stated form).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<StudioDecorationLength>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<StudioDecorationLength>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<StudioDecorationColour>,
    /// Percent, 0..=100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tint: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overprint: Option<bool>,
    /// `SSWT` swatch id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap_colour: Option<String>,
    /// Percent, 0..=100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap_tint: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap_overprint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_ink: Option<bool>,
}
impl StudioDecoration {
    /// `self` with every field `top` states replaced by `top`'s.
    pub fn overlaid(&self, top: &Self) -> Self {
        Self {
            stroke_type: top.stroke_type.clone().or_else(|| self.stroke_type.clone()),
            weight: top.weight.clone().or_else(|| self.weight.clone()),
            offset: top.offset.clone().or_else(|| self.offset.clone()),
            colour: top.colour.clone().or_else(|| self.colour.clone()),
            tint: top.tint.or(self.tint),
            overprint: top.overprint.or(self.overprint),
            gap_colour: top.gap_colour.clone().or_else(|| self.gap_colour.clone()),
            gap_tint: top.gap_tint.or(self.gap_tint),
            gap_overprint: top.gap_overprint.or(self.gap_overprint),
            skip_ink: top.skip_ink.or(self.skip_ink),
        }
    }
    /// True when a stated `type` other than `off` draws a line.
    pub fn draws(&self) -> bool {
        self.stroke_type.as_deref().is_some_and(|t| t != DECORATION_OFF)
    }
}

/// STU-TYP-161 per-range hyperlink target: a URL or an in-document node (artboard, page spread or
/// anchor id). A character attribute, so it survives range splitting and merging.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "target_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioHyperlink {
    Url { url: String },
    Node { node_id: String },
}

/// STU-TYP-155 character-attribute record (plus 156, 157, 158, 159-161, 133 and the STU-TYP-170
/// feature map). Every field is optional: absent = not set here, so a record is also a patch and a
/// local override (STU-TYP-110). Units are the spec table's: `size`, `leading`, `baseline_shift`
/// and `stroke_weight` points; `tracking` and `kerning_manual` em/1000 with precision 0 (whole
/// numbers); scales, `tsume` and the synthesis sizes and positions percent; rotation and skew
/// degrees; aki em. Bounds: only the stated ones ([`CHARACTER_PARAMS`]); where the spec says UNKNOWN
/// only finiteness and the physical domain are checked (size, leading and scales positive).
/// STU-TYP-174: the legacy OpenType booleans (`ligatures` ... `proportional_metrics`) are entries of
/// `opentype_features` (Type's `LegacyBoolean` mapping), never a second storage; the STU-TYP-157
/// booleans stored here are the ones with no single registered tag.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioCharacterAttributes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leading: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kerning_manual: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub horizontal_scale: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertical_scale: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_shift: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_rotation: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skew: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aki_left: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aki_right: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tsume: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke_weight: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kerning_mode: Option<StudioKerningMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capitalization: Option<StudioCapitalization>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<StudioCharacterPosition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anti_alias_method: Option<StudioAntiAliasMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_alignment: Option<StudioCharacterAlignment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternate_glyph_form: Option<StudioAlternateGlyphForm>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_direction: Option<StudioBaselineDirection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_baseline_option: Option<StudioFontBaselineOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digits_type: Option<StudioDigitsType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_break: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub faux_bold: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub faux_italic: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fractional_widths: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlap_swash: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stretched_alternate: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_forms: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kana: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italics: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotate_single_byte_characters: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cjk_grid_tracking: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overprint_fill: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overprint_stroke: Option<bool>,
    /// STU-TYP-170 feature map: four-character tag -> value (0/1 for a binary feature, an alternate
    /// index otherwise). Any well-formed tag is accepted; there is no closed list.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub opentype_features: BTreeMap<String, u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub small_cap_size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superscript_size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscript_size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superscript_position: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscript_position: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underline: Option<StudioDecoration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strikethrough: Option<StudioDecoration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyperlink: Option<StudioHyperlink>,
}

/// The fields of [`StudioCharacterAttributes`] in declaration order: the names a model sets one at
/// a time or clears (STU-TYP-112). Pinned to the record's schema by `tests/character_attributes.rs`.
pub const CHARACTER_ATTRIBUTE_FIELDS: [&str; 44] = [
    "size", "leading", "tracking", "kerning_manual", "horizontal_scale", "vertical_scale", "baseline_shift", "character_rotation", "skew", "aki_left", "aki_right",
    "tsume", "stroke_weight", "kerning_mode", "capitalization", "position", "anti_alias_method", "character_alignment", "alternate_glyph_form",
    "baseline_direction", "font_baseline_option", "digits_type", "no_break", "faux_bold", "faux_italic", "fractional_widths", "overlap_swash",
    "stretched_alternate", "connection_forms", "kana", "italics", "rotate_single_byte_characters", "cjk_grid_tracking", "overprint_fill", "overprint_stroke",
    "opentype_features", "small_cap_size", "superscript_size", "subscript_size", "superscript_position", "subscript_position", "underline", "strikethrough",
    "hyperlink",
];

/// STU-TYP-155 / 158 / 159 parameter rows as data (STU-TYP-221: the seven-field record per numeric
/// parameter; `None` = the spec's UNKNOWN, never clamped). Decoration rows name the
/// `underline` / `strikethrough` record fields.
pub const CHARACTER_PARAMS: [TypographyParamRow; 26] = [
    param("size", None, None, None, "points", None),
    param("leading", None, None, None, "points", None),
    param("tracking", None, None, Some(0.0), "em/1000", Some(0)),
    param("kerning_manual", None, None, Some(0.0), "em/1000", Some(0)),
    param("horizontal_scale", None, None, Some(100.0), "percent", None),
    param("vertical_scale", None, None, Some(100.0), "percent", None),
    param("baseline_shift", None, None, Some(0.0), "points", None),
    param("character_rotation", None, None, Some(0.0), "degrees", None),
    param("skew", None, None, Some(0.0), "degrees", None),
    param("aki_left", None, None, None, "em", None),
    param("aki_right", None, None, None, "em", None),
    param("tsume", None, None, None, "percent", None),
    param("stroke_weight", Some(0.0), None, None, "points", None),
    param("small_cap_size", Some(1.0), Some(200.0), None, "percent", None),
    param("superscript_size", Some(0.0), Some(200.0), None, "percent", None),
    param("subscript_size", Some(0.0), Some(200.0), None, "percent", None),
    param("superscript_position", Some(-500.0), Some(500.0), None, "percent", None),
    param("subscript_position", Some(-500.0), Some(500.0), None, "percent", None),
    param("underline_weight", Some(0.0), None, None, "points", None),
    param("underline_offset", None, None, None, "points", None),
    param("underline_tint", Some(0.0), Some(100.0), None, "percent", None),
    param("underline_gap_tint", Some(0.0), Some(100.0), None, "percent", None),
    param("strikethrough_weight", Some(0.0), None, None, "points", None),
    param("strikethrough_offset", None, None, None, "points", None),
    param("strikethrough_tint", Some(0.0), Some(100.0), None, "percent", None),
    param("strikethrough_gap_tint", Some(0.0), Some(100.0), None, "percent", None),
];

const fn param(key: &'static str, hard_min: Option<f64>, hard_max: Option<f64>, default: Option<f64>, unit: &'static str, precision: Option<u32>) -> TypographyParamRow {
    TypographyParamRow { key, hard_min, hard_max, soft_min: None, soft_max: None, default, unit, precision }
}

/// True for a well-formed OpenType tag: four ASCII bytes 0x20..=0x7E, no leading space.
pub fn opentype_tag_ok(tag: &str) -> bool {
    let b = tag.as_bytes();
    b.len() == 4 && b.iter().all(|c| (0x20..=0x7E).contains(c)) && b.first() != Some(&b' ')
}

/// The hyperlink URL rule: 1..=[`MAX_URL_BYTES`] bytes, no whitespace or control character, an
/// RFC 3986 scheme, and not a script or inline-data scheme (`javascript`, `vbscript`, `data`).
pub fn hyperlink_url_ok(url: &str) -> bool {
    if url.is_empty() || url.len() > MAX_URL_BYTES || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    let well_formed = chars.next().is_some_and(|c| c.is_ascii_alphabetic()) && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    well_formed && !["javascript", "vbscript", "data"].iter().any(|s| scheme.eq_ignore_ascii_case(s))
}

impl StudioCharacterAttributes {
    /// True when no field is set.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
    /// `self` with every field `top` sets replaced by `top`'s (decorations field by field, feature
    /// entries tag by tag): the STU-TYP-201 later-wins order.
    pub fn overlaid(&self, top: &Self) -> Self {
        let mut out = self.clone();
        macro_rules! take {
            ($($f:ident),* $(,)?) => {
                $(if top.$f.is_some() {
                    out.$f = top.$f.clone();
                })*
            };
        }
        take!(
            size, leading, tracking, kerning_manual, horizontal_scale, vertical_scale, baseline_shift, character_rotation, skew, aki_left, aki_right, tsume,
            stroke_weight, kerning_mode, capitalization, position, anti_alias_method, character_alignment, alternate_glyph_form, baseline_direction,
            font_baseline_option, digits_type, no_break, faux_bold, faux_italic, fractional_widths, overlap_swash, stretched_alternate, connection_forms, kana,
            italics, rotate_single_byte_characters, cjk_grid_tracking, overprint_fill, overprint_stroke, small_cap_size, superscript_size, subscript_size,
            superscript_position, subscript_position, hyperlink,
        );
        for (slot, top) in [(&mut out.underline, &top.underline), (&mut out.strikethrough, &top.strikethrough)] {
            if let Some(t) = top {
                *slot = Some(slot.as_ref().map_or_else(|| t.clone(), |s| s.overlaid(t)));
            }
        }
        for (tag, value) in &top.opentype_features {
            out.opentype_features.insert(tag.clone(), *value);
        }
        out
    }
    /// The record validator: stated bounds ([`CHARACTER_PARAMS`]), finiteness, physical domains,
    /// whole numbers where the precision is 0, feature tags, decoration references (`off`, a
    /// built-in or a stored stroke style of `strokes`), swatch ids and hyperlink targets.
    /// `Code::InvalidField` (`Code::DanglingReference` for an unknown stroke style), target
    /// `{target}/{field}`.
    pub fn validate(&self, target: &str, strokes: &[StudioStrokeStyleEntry]) -> Result<()> {
        let finite = |v: Option<f64>| v.is_none_or(f64::is_finite);
        let positive = |v: Option<f64>| v.is_none_or(|v| v.is_finite() && v > 0.0);
        let whole = |v: Option<f64>| v.is_none_or(|v| v.is_finite() && v.fract() == 0.0);
        let within = |v: Option<f64>, lo: f64, hi: f64| v.is_none_or(|v| v.is_finite() && (lo..=hi).contains(&v));
        for (field, ok) in [
            ("size", positive(self.size)),
            ("leading", positive(self.leading)),
            ("tracking", whole(self.tracking)),
            ("kerning_manual", whole(self.kerning_manual)),
            ("horizontal_scale", positive(self.horizontal_scale)),
            ("vertical_scale", positive(self.vertical_scale)),
            ("baseline_shift", finite(self.baseline_shift)),
            ("character_rotation", finite(self.character_rotation)),
            ("skew", finite(self.skew)),
            ("aki_left", finite(self.aki_left)),
            ("aki_right", finite(self.aki_right)),
            ("tsume", finite(self.tsume)),
            ("stroke_weight", self.stroke_weight.is_none_or(|v| v.is_finite() && v >= 0.0)),
            ("small_cap_size", within(self.small_cap_size, 1.0, 200.0)),
            ("superscript_size", within(self.superscript_size, 0.0, 200.0)),
            ("subscript_size", within(self.subscript_size, 0.0, 200.0)),
            ("superscript_position", within(self.superscript_position, -500.0, 500.0)),
            ("subscript_position", within(self.subscript_position, -500.0, 500.0)),
        ] {
            check(ok, target, field)?;
        }
        check(self.opentype_features.len() <= MAX_RECORD_FEATURES, target, "opentype_features")?;
        for tag in self.opentype_features.keys() {
            check(opentype_tag_ok(tag), target, "opentype_features")?;
        }
        for (field, d) in [("underline", &self.underline), ("strikethrough", &self.strikethrough)] {
            let Some(d) = d else { continue };
            let t = format!("{target}/{field}");
            if let Some(name) = &d.stroke_type
                && name != DECORATION_OFF
                && !BUILTIN_STROKE_STYLES.contains(&name.as_str())
                && !strokes.iter().any(|s| s.name == *name)
            {
                return Err(err(Code::DanglingReference, format!("{t}/type")));
            }
            check(d.weight.as_ref().is_none_or(|w| w.value().is_none_or(|v| v.is_finite() && v >= 0.0)), &t, "weight")?;
            check(d.offset.as_ref().is_none_or(|o| o.value().is_none_or(f64::is_finite)), &t, "offset")?;
            check(within(d.tint, 0.0, 100.0), &t, "tint")?;
            check(within(d.gap_tint, 0.0, 100.0), &t, "gap_tint")?;
            if let Some(StudioDecorationColour::Swatch { swatch_id }) = &d.colour {
                id(swatch_id, "SSWT", &format!("{t}/colour"))?;
            }
            if let Some(g) = &d.gap_colour {
                id(g, "SSWT", &format!("{t}/gap_colour"))?;
            }
        }
        match &self.hyperlink {
            Some(StudioHyperlink::Url { url }) => check(hyperlink_url_ok(url), target, "hyperlink")?,
            Some(StudioHyperlink::Node { node_id }) => check(label(node_id), target, "hyperlink")?,
            None => {}
        }
        Ok(())
    }
}

/// STU-TYP-192 end cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioStrokeEndCap {
    Butt,
    Round,
    Projecting,
}
/// STU-TYP-192 end join.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioStrokeEndJoin {
    Miter,
    Round,
    Bevel,
}
/// STU-TYP-192 pattern: solid, a dash pattern (on/off lengths in points, an even count) or a stripe
/// pattern (relative widths alternating line/gap, an odd count so it starts and ends with a line),
/// or the dotted and wavy forms of the STU-TYP-160 decoration styles.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "pattern_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioStrokePattern {
    Solid,
    Dash { segments: Vec<f64> },
    Stripe { stripes: Vec<f64> },
    Dotted,
    Wavy,
}
/// One member of the stroke-style registry (STU-TYP-192): underline, strikethrough, paragraph
/// rules and borders all name styles here; there is no per-decoration stroke vocabulary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioStrokeStyleEntry {
    /// Unique in the registry; not `off` and not a built-in name ([`BUILTIN_STROKE_STYLES`]).
    pub name: String,
    pub pattern: StudioStrokePattern,
    pub end_cap: StudioStrokeEndCap,
    pub end_join: StudioStrokeEndJoin,
}
impl StudioStrokeStyleEntry {
    /// Name, pattern counts and lengths (finite, non-negative, a positive total). `InvalidField`.
    pub fn validate(&self) -> Result<()> {
        let t = format!("stroke_styles/{}", self.name);
        check(label(&self.name) && self.name != DECORATION_OFF && !BUILTIN_STROKE_STYLES.contains(&self.name.as_str()), &t, "name")?;
        let lengths = |v: &[f64], even: bool| {
            !v.is_empty()
                && v.len() <= MAX_STROKE_PATTERN
                && (v.len() & 1 == 0) == even
                && v.iter().all(|x| x.is_finite() && *x >= 0.0)
                && v.iter().any(|x| *x > 0.0)
        };
        match &self.pattern {
            StudioStrokePattern::Dash { segments } => check(lengths(segments, true), &t, "pattern"),
            StudioStrokePattern::Stripe { stripes } => check(lengths(stripes, false), &t, "pattern"),
            StudioStrokePattern::Solid | StudioStrokePattern::Dotted | StudioStrokePattern::Wavy => Ok(()),
        }
    }
}

/// A quote pair (open, close) of a language (STU-TYP-209).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioQuotePair {
    pub open: String,
    pub close: String,
}

/// STU-TYP-209 language record, a document resource keyed by `name` (the key a character range's
/// `applied_language` names, STU-TYP-210). The vendors are open-registry strings, not enumerations;
/// the quote pairs drive typographer-quote substitution and are language properties.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioLanguage {
    pub name: String,
    pub primary_language_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sublanguage_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub untranslated_name: Option<String>,
    /// ICU locale name (`en_US`, `de_DE`, `zh_Hans_CN`): letters, then `_`-separated
    /// alphanumeric subtags.
    pub icu_locale_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyphenation_vendor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spelling_vendor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub single_quotes: Option<StudioQuotePair>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub double_quotes: Option<StudioQuotePair>,
}
impl StudioLanguage {
    /// The BCP 47 form of `icu_locale_name` (`en_US` -> `en-US`): Type's shaping language tag.
    pub fn language_tag(&self) -> String {
        self.icu_locale_name.replace('_', "-")
    }
    /// Names (labels), the locale syntax and the quote marks (1..=[`MAX_QUOTE_BYTES`] bytes, no
    /// control character). `InvalidField`, target `languages/{name}/{field}`.
    pub fn validate(&self) -> Result<()> {
        let t = format!("languages/{}", self.name);
        check(label(&self.name), &t, "name")?;
        check(label(&self.primary_language_name), &t, "primary_language_name")?;
        for (field, v) in [
            ("sublanguage_name", &self.sublanguage_name),
            ("untranslated_name", &self.untranslated_name),
            ("hyphenation_vendor", &self.hyphenation_vendor),
            ("spelling_vendor", &self.spelling_vendor),
        ] {
            check(v.as_deref().is_none_or(label), &t, field)?;
        }
        let locale = &self.icu_locale_name;
        let mut parts = locale.split('_');
        let ok = locale.len() <= MAX_ICU_LOCALE_BYTES
            && parts.next().is_some_and(|p| (2..=8).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphabetic()))
            && parts.all(|p| (1..=8).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric()));
        check(ok, &t, "icu_locale_name")?;
        let mark = |m: &str| !m.is_empty() && m.len() <= MAX_QUOTE_BYTES && !m.chars().any(char::is_control);
        for (field, q) in [("single_quotes", &self.single_quotes), ("double_quotes", &self.double_quotes)] {
            check(q.as_ref().is_none_or(|q| mark(&q.open) && mark(&q.close)), &t, field)?;
        }
        Ok(())
    }
}

/// The character attributes a style states (either scope: a paragraph style's apply to every
/// character of its paragraphs, a character style's on top; one entry per style).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioStyleCharacter {
    /// `STYS` id of a style of the same set.
    pub style_id: String,
    pub attributes: StudioCharacterAttributes,
}

/// A local character override (STU-TYP-110 "plus local overrides") over the story bytes
/// `[start, end)` of `text_id`. Overrides of one story never overlap; text edits move them like the
/// story's ranges (hsk-studio-layout `shift_paragraph_records`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioCharacterOverride {
    /// `STXT` id of the text story (a document-local reference).
    pub text_id: String,
    pub start: u64,
    pub end: u64,
    /// Not empty.
    pub attributes: StudioCharacterAttributes,
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
    /// Character attributes per style (lane SMB-TYPE-CHARACTER-ATTRIBUTES-1).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub style_character: Vec<StudioStyleCharacter>,
    /// Local character overrides over story byte spans, ordered by story and start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub character_overrides: Vec<StudioCharacterOverride>,
    /// The stored members of the stroke-style registry (STU-TYP-192; the built-ins are implicit).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stroke_styles: Vec<StudioStrokeStyleEntry>,
    /// Language records (STU-TYP-209).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub languages: Vec<StudioLanguage>,
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
            style_character: Vec::new(),
            character_overrides: Vec::new(),
            stroke_styles: Vec::new(),
            languages: Vec::new(),
        }
    }
    /// The character attributes the style `style_id` itself states.
    pub fn style_character(&self, style_id: &str) -> Option<&StudioCharacterAttributes> {
        self.style_character.iter().find(|c| c.style_id == style_id).map(|c| &c.attributes)
    }
    /// The language record named `name` (a range's `applied_language`).
    pub fn language(&self, name: &str) -> Option<&StudioLanguage> {
        self.languages.iter().find(|l| l.name == name)
    }
    /// The character attributes a style states along its `based_on` chain, root first (the chain
    /// is bounded by the set size; a cycle, which validation refuses, stops the walk).
    pub fn chain_character(&self, style_id: &str) -> StudioCharacterAttributes {
        let mut chain = Vec::new();
        let mut at = Some(style_id);
        while let Some(id) = at {
            if chain.len() > self.styles.len() || chain.contains(&id) {
                break;
            }
            chain.push(id);
            at = self.style(id).and_then(|s| s.based_on.as_deref());
        }
        chain.iter().rev().fold(StudioCharacterAttributes::default(), |acc, id| self.style_character(id).map_or(acc.clone(), |c| acc.overlaid(c)))
    }
    /// The character attributes in force at a byte of `text_id` (STU-TYP-201 order): the
    /// paragraph style's chain, then the character style's chain, then the local override covering
    /// `byte`.
    pub fn resolved_character(&self, text_id: &str, byte: u64, paragraph_style: Option<&str>, character_style: Option<&str>) -> StudioCharacterAttributes {
        let mut out = paragraph_style.map(|s| self.chain_character(s)).unwrap_or_default();
        if let Some(s) = character_style {
            out = out.overlaid(&self.chain_character(s));
        }
        match self.character_overrides.iter().find(|o| o.text_id == text_id && o.start <= byte && byte < o.end) {
            Some(o) => out.overlaid(&o.attributes),
            None => out,
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

/// Deepest `based_on` chain a resolution walks (red skeleton).
pub const MAX_STYLE_CHAIN: usize = 64;

/// One export's or read's character-attribute resolution (red skeleton: no legacy fold, no bound).
pub struct CharacterResolver<'a> {
    records: &'a StudioTypeStyleRecords,
}
impl<'a> CharacterResolver<'a> {
    pub fn new(records: &'a StudioTypeStyleRecords, step_budget: u64) -> Self {
        let _ = step_budget;
        Self { records }
    }
    pub fn at(&mut self, text_id: &str, byte: u64, paragraph_style: Option<&str>, character_style: Option<&str>) -> Result<StudioCharacterAttributes> {
        Ok(self.records.resolved_character(text_id, byte, paragraph_style, character_style))
    }
    pub fn steps(&self) -> u64 {
        0
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
    // Character attributes, the stroke-style registry and the language records (lane
    // SMB-TYPE-CHARACTER-ATTRIBUTES-1): counts first, then names, references and every record.
    for (field, n, max) in [
        ("style_character", r.style_character.len(), MAX_TYPE_STYLES),
        ("character_overrides", r.character_overrides.len(), MAX_CHARACTER_OVERRIDES),
        ("stroke_styles", r.stroke_styles.len(), MAX_STROKE_STYLES),
        ("languages", r.languages.len(), MAX_LANGUAGES),
    ] {
        if n > max {
            return Err(err(Code::BudgetExceeded, field));
        }
        cx.nodes(n, field)?;
    }
    let mut names = BTreeSet::new();
    for s in &r.stroke_styles {
        cx.tick()?;
        s.validate()?;
        cx.text(&s.name, "stroke_styles")?;
        if !names.insert(s.name.as_str()) {
            return Err(err(Code::DuplicateIdentity, format!("stroke_styles/{}", s.name)));
        }
    }
    let mut names = BTreeSet::new();
    for l in &r.languages {
        cx.tick()?;
        l.validate()?;
        cx.text(&l.name, "languages")?;
        if !names.insert(l.name.as_str()) {
            return Err(err(Code::DuplicateIdentity, format!("languages/{}", l.name)));
        }
    }
    let mut stated = BTreeSet::new();
    for c in &r.style_character {
        cx.tick()?;
        let target = format!("style_character/{}", c.style_id);
        id(&c.style_id, "STYS", &target)?;
        if !scopes.contains_key(c.style_id.as_str()) {
            return Err(err(Code::DanglingReference, target));
        }
        if !stated.insert(c.style_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, target));
        }
        c.attributes.validate(&target, &r.stroke_styles)?;
    }
    let mut spans: Vec<(&str, u64, u64)> = Vec::with_capacity(r.character_overrides.len());
    for o in &r.character_overrides {
        cx.tick()?;
        let target = format!("character_overrides/{}/{}", o.text_id, o.start);
        id(&o.text_id, "STXT", &target)?;
        check(o.start < o.end, &target, "end")?;
        check(!o.attributes.is_empty(), &target, "attributes")?;
        o.attributes.validate(&target, &r.stroke_styles)?;
        spans.push((o.text_id.as_str(), o.start, o.end));
    }
    spans.sort_unstable();
    for w in spans.windows(2) {
        if let [a, b] = w
            && a.0 == b.0
            && b.1 < a.2
        {
            return Err(err(Code::InvalidField, format!("character_overrides/{}/{}", b.0, b.1)));
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
