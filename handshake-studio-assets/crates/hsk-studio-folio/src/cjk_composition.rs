//! CJK composition records (`hsk.studio.cjk_composition@1`), lane SMB-ID-CJK-KINSOKU-1:
//! STU-TYP-136 (named composer registry), STU-TYP-137 (kinsoku line-break tables and the six
//! paragraph kinsoku attributes) and STU-TYP-138 (mojikumi spacing tables over the seventeen base
//! sets). Like the other record sets it is stored beside the document (`StudioDocument` gains no
//! fields) and every list is `serde(default)` and not encoded when empty, so documents without CJK
//! records keep their bytes. Folio stores and validates only; hsk-studio-layout composes (`cjk`).
//!
//! Names: a kinsoku table name resolves to a table of this set or to a shipped table
//! ([`SHIPPED_KINSOKU_TABLES`], contents in hsk-studio-layout from W3C JLREQ); a mojikumi table name
//! resolves to a table of this set or to one of the seventeen base-set names (the base set with no
//! overrides). A document table may not take a shipped or base-set name. A composer naming a table
//! that resolves nowhere, and a paragraph-style binding naming a composer this set does not hold,
//! is `DanglingReference` (STU-TYP-136: never a silent fallback).
//!
//! Declared readings (the spec gives the fields, not their values; Operator decides spec edits):
//! paragraph kinsoku attributes ride on per-paragraph-style bindings (`bindings`, one per `STYS`
//! paragraph style), not on local paragraph overrides; mojikumi class pairs use the JLREQ character
//! classes cl-01..cl-19, cl-24, cl-26 and cl-27 plus the line-start / line-end positions; aki values
//! are em fractions 0..=[`MAX_AKI_EM`], minimum <= desired <= maximum. The leading model of
//! STU-TYP-139 is not stored here. InDesign parity UNVERIFIED (D5).
//!
//! The records arrive from untrusted wire JSON: integer arithmetic is denied (workflow V4-01) and
//! every list and character set is bounded before it is walked.
#![deny(clippy::arithmetic_side_effects)]
use super::{
    Code, Result, StudioCompositionEngine, StudioCompositionScope, StudioDocument, StudioTypeStyleRecords,
    TypeStyleScope, canceled, err, id,
};
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

schema_token!(CjkCompositionSchema, "hsk.studio.cjk_composition@1");

/// Tables (kinsoku or mojikumi) and named composers per record set (a Folio hard ceiling).
pub const MAX_CJK_TABLES: usize = 256;
/// Characters per kinsoku character-class field (a Folio hard ceiling).
pub const MAX_KINSOKU_CHARS: usize = 4096;
/// Aki overrides per mojikumi table (a Folio hard ceiling).
pub const MAX_AKI_OVERRIDES: usize = 1024;
/// Paragraph-style bindings per record set (a Folio hard ceiling).
pub const MAX_CJK_BINDINGS: usize = 4096;
/// Table and composer name length in bytes (a Folio hard ceiling).
pub const MAX_CJK_NAME_BYTES: usize = 128;
/// Largest aki value in em (a Folio hard ceiling; the spec states none).
pub const MAX_AKI_EM: f64 = 2.0;
/// Kinsoku table names shipped by Studio (W3C JLREQ hard and soft line-breaking rules; contents in
/// hsk-studio-layout `cjk`).
pub const SHIPPED_KINSOKU_TABLES: [&str; 2] = ["hard_kinsoku", "soft_kinsoku"];

/// STU-TYP-137 `kinsoku_type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioKinsokuType {
    PushInFirst,
    PushOutFirst,
    PushOutOnly,
    PrioritizeAdjustment,
}

/// STU-TYP-137 `kinsoku_hang_type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioKinsokuHangType {
    None,
    Regular,
    Force,
}

/// STU-TYP-137 `burasagari_type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioBurasagariType {
    None,
    Standard,
    Forced,
}

/// STU-TYP-137 `kinsoku_order`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioKinsokuOrder {
    PushIn,
    PushOutFirst,
    PushOutOnly,
}

/// STU-TYP-138 mojikumi base set: the seventeen members, all shipped (taken whole from the spec).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioMojikumiBaseSet {
    Nothing,
    LineEndAllOneHalfEm,
    OneEmIndentLineEndUkeOneHalfEm,
    OneOrOneHalfEmIndentLineEndUkeOneHalfEm,
    OneOrOneHalfEmIndentLineEndAllOneEm,
    OneEmIndentLineEndAllOneEm,
    OneEmIndentLineEndAllNoFloat,
    OneEmIndentLineEndUkeNoFloat,
    OneOrOneHalfEmIndentLineEndUkeNoFloat,
    OneEmIndentLineEndAllOneHalfEm,
    LineEndAllOneEm,
    LineEndUkeNoFloat,
    OneOrOneHalfEmIndentLineEndPeriodOneEm,
    OneEmIndentLineEndPeriodOneEm,
    LineEndPeriodOneEm,
    TraditionalChineseDefault,
    SimplifiedChineseDefault,
}
impl StudioMojikumiBaseSet {
    /// Every member, in spec order.
    pub const ALL: [Self; 17] = [
        Self::Nothing,
        Self::LineEndAllOneHalfEm,
        Self::OneEmIndentLineEndUkeOneHalfEm,
        Self::OneOrOneHalfEmIndentLineEndUkeOneHalfEm,
        Self::OneOrOneHalfEmIndentLineEndAllOneEm,
        Self::OneEmIndentLineEndAllOneEm,
        Self::OneEmIndentLineEndAllNoFloat,
        Self::OneEmIndentLineEndUkeNoFloat,
        Self::OneOrOneHalfEmIndentLineEndUkeNoFloat,
        Self::OneEmIndentLineEndAllOneHalfEm,
        Self::LineEndAllOneEm,
        Self::LineEndUkeNoFloat,
        Self::OneOrOneHalfEmIndentLineEndPeriodOneEm,
        Self::OneEmIndentLineEndPeriodOneEm,
        Self::LineEndPeriodOneEm,
        Self::TraditionalChineseDefault,
        Self::SimplifiedChineseDefault,
    ];
    /// The wire (snake_case) name, which is also the name a composer uses to select the base set.
    pub fn name(self) -> &'static str {
        match self {
            Self::Nothing => "nothing",
            Self::LineEndAllOneHalfEm => "line_end_all_one_half_em",
            Self::OneEmIndentLineEndUkeOneHalfEm => "one_em_indent_line_end_uke_one_half_em",
            Self::OneOrOneHalfEmIndentLineEndUkeOneHalfEm => "one_or_one_half_em_indent_line_end_uke_one_half_em",
            Self::OneOrOneHalfEmIndentLineEndAllOneEm => "one_or_one_half_em_indent_line_end_all_one_em",
            Self::OneEmIndentLineEndAllOneEm => "one_em_indent_line_end_all_one_em",
            Self::OneEmIndentLineEndAllNoFloat => "one_em_indent_line_end_all_no_float",
            Self::OneEmIndentLineEndUkeNoFloat => "one_em_indent_line_end_uke_no_float",
            Self::OneOrOneHalfEmIndentLineEndUkeNoFloat => "one_or_one_half_em_indent_line_end_uke_no_float",
            Self::OneEmIndentLineEndAllOneHalfEm => "one_em_indent_line_end_all_one_half_em",
            Self::LineEndAllOneEm => "line_end_all_one_em",
            Self::LineEndUkeNoFloat => "line_end_uke_no_float",
            Self::OneOrOneHalfEmIndentLineEndPeriodOneEm => "one_or_one_half_em_indent_line_end_period_one_em",
            Self::OneEmIndentLineEndPeriodOneEm => "one_em_indent_line_end_period_one_em",
            Self::LineEndPeriodOneEm => "line_end_period_one_em",
            Self::TraditionalChineseDefault => "traditional_chinese_default",
            Self::SimplifiedChineseDefault => "simplified_chinese_default",
        }
    }
    /// The member with this wire name.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }
}

/// Mojikumi character class of an aki override: the W3C JLREQ classes (cl-NN in the comments)
/// this slice composes, plus the line-start and line-end positions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioMojikumiClass {
    /// cl-01
    OpeningBrackets,
    /// cl-02
    ClosingBrackets,
    /// cl-03
    Hyphens,
    /// cl-04
    DividingPunctuation,
    /// cl-05
    MiddleDots,
    /// cl-06
    FullStops,
    /// cl-07
    Commas,
    /// cl-08
    InseparableCharacters,
    /// cl-09
    IterationMarks,
    /// cl-10
    ProlongedSoundMark,
    /// cl-11
    SmallKana,
    /// cl-12
    PrefixedAbbreviations,
    /// cl-13
    PostfixedAbbreviations,
    /// cl-14
    IdeographicSpace,
    /// cl-15
    Hiragana,
    /// cl-16
    Katakana,
    /// cl-17
    MathSymbols,
    /// cl-18
    MathOperators,
    /// cl-19
    Ideographic,
    /// cl-24
    GroupedNumerals,
    /// cl-26
    WesternWordSpace,
    /// cl-27
    WesternCharacters,
    /// Position before the first character of a line (only as `previous`).
    LineStart,
    /// Position after the last character of a line (only as `next`).
    LineEnd,
}

/// STU-TYP-137 kinsoku table: four character sets (each character once, no control characters).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioKinsokuTable {
    pub name: String,
    pub cant_begin_line_chars: String,
    pub cant_end_line_chars: String,
    pub cant_be_separated_chars: String,
    pub hanging_punctuation_chars: String,
}

/// One aki override: the spacing between a `previous` and a `next` class, in em.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioAkiOverride {
    pub previous: StudioMojikumiClass,
    pub next: StudioMojikumiClass,
    pub minimum_em: f64,
    pub desired_em: f64,
    pub maximum_em: f64,
}

/// STU-TYP-138 mojikumi table.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioMojikumiTable {
    pub name: String,
    pub based_on: StudioMojikumiBaseSet,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aki_override_list: Vec<StudioAkiOverride>,
    /// Glyph-CID class lookup instead of codepoint lookup.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub use_cid_mojikumi: bool,
}

/// STU-TYP-136 named composer: both axes and the CJK tables it binds (by name).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioNamedComposer {
    pub composer_name: String,
    pub composition_scope: StudioCompositionScope,
    pub composition_engine: StudioCompositionEngine,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinsoku_table: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mojikumi_table: Option<String>,
}

/// The composer and kinsoku attributes of the paragraphs carrying one paragraph style.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioCjkParagraphBinding {
    /// `STYS` id of a paragraph style.
    pub style_id: String,
    pub composer_name: String,
    pub kinsoku_type: StudioKinsokuType,
    pub kinsoku_hang_type: StudioKinsokuHangType,
    pub bunri_kinshi: bool,
    pub rensuuji: bool,
    pub burasagari_type: StudioBurasagariType,
    pub kinsoku_order: StudioKinsokuOrder,
}

/// The CJK composition records of one document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioCjkCompositionRecords {
    pub schema_id: CjkCompositionSchema,
    /// `SDOC` id of the annotated document.
    pub document_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinsoku_tables: Vec<StudioKinsokuTable>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mojikumi_tables: Vec<StudioMojikumiTable>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub composers: Vec<StudioNamedComposer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bindings: Vec<StudioCjkParagraphBinding>,
}
impl StudioCjkCompositionRecords {
    /// An empty set for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self {
            schema_id: CjkCompositionSchema::V1,
            document_id: document_id.into(),
            kinsoku_tables: Vec::new(),
            mojikumi_tables: Vec::new(),
            composers: Vec::new(),
            bindings: Vec::new(),
        }
    }
    /// The kinsoku table of this set with this name (shipped tables live in hsk-studio-layout).
    pub fn kinsoku_table(&self, name: &str) -> Option<&StudioKinsokuTable> {
        self.kinsoku_tables.iter().find(|t| t.name == name)
    }
    /// The mojikumi table of this set with this name.
    pub fn mojikumi_table(&self, name: &str) -> Option<&StudioMojikumiTable> {
        self.mojikumi_tables.iter().find(|t| t.name == name)
    }
    /// The named composer with this name.
    pub fn composer(&self, name: &str) -> Option<&StudioNamedComposer> {
        self.composers.iter().find(|c| c.composer_name == name)
    }
    /// The binding of the paragraph style `style_id`.
    pub fn binding(&self, style_id: &str) -> Option<&StudioCjkParagraphBinding> {
        self.bindings.iter().find(|b| b.style_id == style_id)
    }
}

/// JSON schema (draft 2020-12) of the CJK composition record set.
pub fn cjk_composition_records_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<StudioCjkCompositionRecords>()
}

fn name_ok(s: &str) -> bool {
    !s.trim().is_empty() && s.len() <= MAX_CJK_NAME_BYTES && !s.chars().any(char::is_control)
}

fn char_set(s: &str, target: &str) -> Result<()> {
    // Bound the byte length first (4 bytes per char at most) so the walk below is bounded too.
    if s.len() > MAX_KINSOKU_CHARS.saturating_mul(4) {
        return Err(err(Code::BudgetExceeded, target));
    }
    let mut seen = BTreeSet::new();
    for c in s.chars() {
        if c.is_control() {
            return Err(err(Code::InvalidField, target));
        }
        if !seen.insert(c) {
            return Err(err(Code::DuplicateField, target));
        }
        if seen.len() > MAX_KINSOKU_CHARS {
            return Err(err(Code::BudgetExceeded, target));
        }
    }
    Ok(())
}

fn aki(o: &StudioAkiOverride, target: &str) -> Result<()> {
    let ok = |v: f64| v.is_finite() && (0.0..=MAX_AKI_EM).contains(&v);
    if !(ok(o.minimum_em) && ok(o.desired_em) && ok(o.maximum_em)) || o.minimum_em > o.desired_em || o.desired_em > o.maximum_em {
        return Err(err(Code::InvalidField, target));
    }
    if o.previous == StudioMojikumiClass::LineEnd || o.next == StudioMojikumiClass::LineStart {
        return Err(err(Code::InvalidField, target));
    }
    if o.previous == StudioMojikumiClass::LineStart && o.next == StudioMojikumiClass::LineEnd {
        return Err(err(Code::InvalidField, target));
    }
    Ok(())
}

/// Validates the set against its document and, when given, its type styles: identity, names
/// (unique per kind, not shipped or base-set names), character sets, aki overrides (ranges, order,
/// line positions, one per class pair), composer table names that resolve (STU-TYP-136), bindings
/// to paragraph styles (unique, the style exists with paragraph scope when `styles` is given) and
/// to composers this set holds. Bounded counts are checked before any list is walked.
pub fn validate_cjk_composition_records(
    r: &StudioCjkCompositionRecords,
    d: &StudioDocument,
    styles: Option<&StudioTypeStyleRecords>,
    token: &CancellationToken,
) -> Result<()> {
    canceled(token)?;
    id(&r.document_id, "SDOC", "document_id")?;
    if r.document_id != d.document_id {
        return Err(err(Code::InvalidField, "document_id"));
    }
    if let Some(s) = styles
        && s.document_id != d.document_id
    {
        return Err(err(Code::InvalidField, "styles/document_id"));
    }
    if r.kinsoku_tables.len() > MAX_CJK_TABLES || r.mojikumi_tables.len() > MAX_CJK_TABLES || r.composers.len() > MAX_CJK_TABLES {
        return Err(err(Code::BudgetExceeded, "cjk_composition"));
    }
    if r.bindings.len() > MAX_CJK_BINDINGS {
        return Err(err(Code::BudgetExceeded, "bindings"));
    }
    let mut kinsoku = BTreeSet::new();
    for t in &r.kinsoku_tables {
        canceled(token)?;
        if !name_ok(&t.name) || SHIPPED_KINSOKU_TABLES.contains(&t.name.as_str()) {
            return Err(err(Code::InvalidField, "kinsoku_tables/name"));
        }
        if !kinsoku.insert(t.name.as_str()) {
            return Err(err(Code::DuplicateIdentity, "kinsoku_tables/name"));
        }
        char_set(&t.cant_begin_line_chars, "kinsoku_tables/cant_begin_line_chars")?;
        char_set(&t.cant_end_line_chars, "kinsoku_tables/cant_end_line_chars")?;
        char_set(&t.cant_be_separated_chars, "kinsoku_tables/cant_be_separated_chars")?;
        char_set(&t.hanging_punctuation_chars, "kinsoku_tables/hanging_punctuation_chars")?;
    }
    let mut mojikumi = BTreeSet::new();
    for t in &r.mojikumi_tables {
        canceled(token)?;
        if !name_ok(&t.name) || StudioMojikumiBaseSet::from_name(&t.name).is_some() {
            return Err(err(Code::InvalidField, "mojikumi_tables/name"));
        }
        if !mojikumi.insert(t.name.as_str()) {
            return Err(err(Code::DuplicateIdentity, "mojikumi_tables/name"));
        }
        if t.aki_override_list.len() > MAX_AKI_OVERRIDES {
            return Err(err(Code::BudgetExceeded, "mojikumi_tables/aki_override_list"));
        }
        let mut pairs = BTreeSet::new();
        for o in &t.aki_override_list {
            aki(o, "mojikumi_tables/aki_override_list")?;
            if !pairs.insert((o.previous, o.next)) {
                return Err(err(Code::DuplicateIdentity, "mojikumi_tables/aki_override_list"));
            }
        }
    }
    let mut composers = BTreeSet::new();
    for c in &r.composers {
        canceled(token)?;
        if !name_ok(&c.composer_name) {
            return Err(err(Code::InvalidField, "composers/composer_name"));
        }
        if !composers.insert(c.composer_name.as_str()) {
            return Err(err(Code::DuplicateIdentity, "composers/composer_name"));
        }
        if let Some(k) = &c.kinsoku_table
            && !(kinsoku.contains(k.as_str()) || SHIPPED_KINSOKU_TABLES.contains(&k.as_str()))
        {
            return Err(err(Code::DanglingReference, "composers/kinsoku_table"));
        }
        if let Some(m) = &c.mojikumi_table
            && !(mojikumi.contains(m.as_str()) || StudioMojikumiBaseSet::from_name(m).is_some())
        {
            return Err(err(Code::DanglingReference, "composers/mojikumi_table"));
        }
    }
    let mut bound = BTreeSet::new();
    for b in &r.bindings {
        canceled(token)?;
        id(&b.style_id, "STYS", "bindings/style_id")?;
        if !bound.insert(b.style_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, "bindings/style_id"));
        }
        if let Some(s) = styles {
            match s.style(&b.style_id).map(|st| st.scope) {
                Some(TypeStyleScope::Paragraph) => {}
                Some(TypeStyleScope::Character) => return Err(err(Code::PayloadMismatch, "bindings/style_id")),
                None => return Err(err(Code::DanglingReference, "bindings/style_id")),
            }
        }
        if !composers.contains(b.composer_name.as_str()) {
            return Err(err(Code::DanglingReference, "bindings/composer_name"));
        }
    }
    Ok(())
}
