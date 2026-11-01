//! OpenType feature surface (spec 14.7.11 and STU-TYP-017).
//!
//! - STU-TYP-170: [`FeatureMap`] is the only feature storage of a character range: an ordered map
//!   from a well-formed four-byte tag to [`FeatureValue`]. [`registry`] is the 245-tag table of
//!   STU-TYP-170 (v02.228, SD-30): the 243 tags registered in OpenType 1.9.1 (`cv01`-`cv99` and
//!   `ss01`-`ss20` counted member by member; `hngl` and `opbd` deprecated) plus `abrv` and `just`,
//!   each with its friendly name, [`FeatureStatus`] and whether the 229-tag observed design-tool
//!   union carries it (`RegistryEntry::observed`; the union omits 16 registered tags). Any other
//!   well-formed tag is still accepted, stored and applied (no closed list); a deprecated tag is
//!   stored and applied like any other and is not offered for new authoring.
//! - STU-TYP-171: [`FeatureGroup`] and [`project_groups`] are presentation over the map.
//! - STU-TYP-017: [`StudioFeatureGroup`] is the 13-member normative feature surface enumeration.
//! - STU-TYP-172: [`FeatureMap::to_features`] yields `declared: false` entries, so a tag the active
//!   font lacks is shaped as `FeatureState::UnavailableInert` (stored, reported, never dropped); the
//!   shaper never rewrites the character stream (proof `tests/opentype_features.rs`
//!   `mt_30957_inert_feature_round_trips_and_text_bytes_unchanged`).
//! - STU-TYP-173: [`FontFeatures`] from `TextEngine::font_features` (bounded GSUB/GPOS reader below).
//! - STU-TYP-174/175: [`LegacyBoolean`], [`FigureStyle`] and the stylistic-set bitfield read and
//!   write the map; there is no second storage.
//!
//! Boolean-to-tag readings (STU-TYP-174) use the tag names of the OpenType Layout Tag Registry
//! (Microsoft, OpenType 1.9.1 "Registered features"): `liga` Standard Ligatures, `dlig`
//! Discretionary Ligatures, `calt` Contextual Alternates, `swsh` Swash, `titl` Titling, `ordn`
//! Ordinals, `frac` Fractions, `zero` Slashed Zero, `salt` Stylistic Alternates, `jalt`
//! Justification Alternates, `palt` Proportional Alternate Widths (for `proportional_metrics`).
//! These are Studio readings; the source applications' own mapping is UNVERIFIED (D5).
//! `overlap_swash` and `stretched_alternate` have no registered tag and `hv_kana` names two
//! (`hkna`, `vkna`): all three refuse with `legacy_projection_unmapped`, never a guess.
//!
//! Diagnostics: terminal outcomes go through Observe as fixed codes only ([`report_feature_outcome`]);
//! tags, text and font names never reach Observe.
#![deny(clippy::arithmetic_side_effects)]
use crate::feature_table::ROWS;
use crate::provider::{Context, storage::ChargedVec, ttf};
use crate::{Error, Feature, FontResource, Limits, Ports, SourceRange};
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe as observe;
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Typed refusal of the feature surface. `Type` carries the crate's fixed codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureError {
    Type(Error),
    /// Not four ASCII bytes (the shaping well-formedness rule of `validate.rs`).
    MalformedTag,
    NegativeAlternateIndex,
    AlternateIndexOverflow,
    /// The same tag twice with different values in one range's features.
    ConflictingFeatureValue,
    /// `FeatureMap::from_features` takes the features of one range.
    MixedRanges,
    FeatureMapFull,
    TagNotInGroup,
    /// STU-TYP-174 boolean without a registered tag (`overlap_swash`, `stretched_alternate`,
    /// `hv_kana`).
    LegacyProjectionUnmapped,
    /// The stored figure tags are not one of the five STU-TYP-175 values.
    FigureStyleUnrepresentable,
    /// Bitfield bits beyond `ss20`: the import refuses and leaves the map unchanged.
    StylisticSetBitsLost,
    LayoutVersionUnsupported,
    LayoutTableTruncated,
    ScriptListTruncated,
    FeatureListTruncated,
    LookupListTruncated,
    FeatureIndexOutOfRange,
    LookupIndexOutOfRange,
    /// Feature or lookup count over `Limits::features` / `Limits::lookups`, per table or summed
    /// over GSUB and GPOS; checked before any allocation.
    LayoutOverLimit,
    /// A font FeatureRecord tag that is not four ASCII bytes.
    FontTagMalformed,
}
impl FeatureError {
    /// Stable snake-case code; carries no tags, text, font names or bytes.
    pub fn code(&self) -> &'static str {
        match self {
            FeatureError::Type(_) => "type_error",
            FeatureError::MalformedTag => "malformed_feature_tag",
            FeatureError::NegativeAlternateIndex => "negative_alternate_index",
            FeatureError::AlternateIndexOverflow => "alternate_index_overflow",
            FeatureError::ConflictingFeatureValue => "conflicting_feature_value",
            FeatureError::MixedRanges => "mixed_feature_ranges",
            FeatureError::FeatureMapFull => "feature_map_full",
            FeatureError::TagNotInGroup => "tag_not_in_group",
            FeatureError::LegacyProjectionUnmapped => "legacy_projection_unmapped",
            FeatureError::FigureStyleUnrepresentable => "figure_style_unrepresentable",
            FeatureError::StylisticSetBitsLost => "stylistic_set_bits_lost",
            FeatureError::LayoutVersionUnsupported => "layout_version_unsupported",
            FeatureError::LayoutTableTruncated => "layout_table_truncated",
            FeatureError::ScriptListTruncated => "script_list_truncated",
            FeatureError::FeatureListTruncated => "feature_list_truncated",
            FeatureError::LookupListTruncated => "lookup_list_truncated",
            FeatureError::FeatureIndexOutOfRange => "feature_index_out_of_range",
            FeatureError::LookupIndexOutOfRange => "lookup_index_out_of_range",
            FeatureError::LayoutOverLimit => "layout_over_limit",
            FeatureError::FontTagMalformed => "font_tag_malformed",
        }
    }
}
impl From<Error> for FeatureError {
    fn from(error: Error) -> Self {
        FeatureError::Type(error)
    }
}

/// Observe failure class of a refusal (closed Observe vocabulary).
pub fn feature_failure_code(error: &FeatureError) -> observe::FailureCode {
    match error {
        FeatureError::Type(
            Error::MissingFont | Error::UnavailableFont | Error::RevokedFont | Error::UnavailableFace,
        ) => observe::FailureCode::Unavailable,
        FeatureError::Type(Error::UnsupportedFont | Error::UnsupportedProfile)
        | FeatureError::LegacyProjectionUnmapped
        | FeatureError::LayoutVersionUnsupported => observe::FailureCode::Unsupported,
        FeatureError::StylisticSetBitsLost => observe::FailureCode::Loss,
        _ => observe::FailureCode::Validation,
    }
}

/// Emits one terminal Observe frame for a finished feature query or projection: `Success`, or
/// `Failure(feature_failure_code)`. Only the correlation, revision and the fixed outcome are sent
/// (no private project text). A cancelled token makes Observe record `Canceled`.
pub fn report_feature_outcome(
    observer: &mut observe::Observe,
    sink: &mut dyn observe::SinkPort,
    correlation_id: u64,
    revision: u64,
    outcome: Result<(), &FeatureError>,
    cancel: &CancellationToken,
) -> Result<observe::Receipt, observe::Error> {
    let event = observe::Observation {
        correlation_id,
        revision,
        outcome: match outcome {
            Ok(()) => observe::Outcome::Success,
            Err(error) => observe::Outcome::Failure(feature_failure_code(error)),
        },
        progress: None,
        private_project_text: None,
    };
    observer.emit(event, cancel, sink)
}

const fn well_formed(bytes: [u8; 4]) -> bool {
    bytes[0].is_ascii() && bytes[1].is_ascii() && bytes[2].is_ascii() && bytes[3].is_ascii()
}

/// A well-formed OpenType feature tag: four ASCII bytes, the same rule shaping applies to
/// `Feature::tag` (`validate.rs` `tag`). Registry membership is not required.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FeatureTag([u8; 4]);
impl FeatureTag {
    pub const fn new(bytes: [u8; 4]) -> Result<Self, FeatureError> {
        if well_formed(bytes) {
            Ok(Self(bytes))
        } else {
            Err(FeatureError::MalformedTag)
        }
    }
    pub fn parse(tag: &str) -> Result<Self, FeatureError> {
        let bytes: [u8; 4] = tag
            .as_bytes()
            .try_into()
            .map_err(|_| FeatureError::MalformedTag)?;
        Self::new(bytes)
    }
    pub const fn bytes(self) -> [u8; 4] {
        self.0
    }
    pub fn as_str(&self) -> &str {
        // Four ASCII bytes are always valid UTF-8.
        std::str::from_utf8(&self.0).unwrap_or("")
    }
}
impl std::fmt::Display for FeatureTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
const fn lit(bytes: &[u8; 4]) -> FeatureTag {
    FeatureTag(*bytes)
}
const DIGITS: &[u8; 10] = b"0123456789";
/// `ss01`..`ss20`, `cv01`..`cv99`.
fn numbered(prefix: [u8; 2], n: u8) -> Option<FeatureTag> {
    let tens = DIGITS.get(usize::from(n.checked_div(10)?))?;
    let ones = DIGITS.get(usize::from(n.checked_rem(10)?))?;
    Some(FeatureTag([prefix[0], prefix[1], *tens, *ones]))
}

/// STU-TYP-170 value: a boolean for a binary feature, an unsigned alternate index otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureValue {
    Binary(bool),
    Alternate(u32),
}
impl FeatureValue {
    /// The shaper value: off 0, on 1, alternate n.
    pub const fn shaping_value(self) -> u32 {
        match self {
            FeatureValue::Binary(false) => 0,
            FeatureValue::Binary(true) => 1,
            FeatureValue::Alternate(n) => n,
        }
    }
    /// Canonical form of a shaper value: 0 and 1 are binary, larger values alternates
    /// (`Alternate(1)` and `Binary(true)` shape identically and canonicalise to `Binary(true)`).
    pub const fn from_shaping_value(value: u32) -> Self {
        match value {
            0 => FeatureValue::Binary(false),
            1 => FeatureValue::Binary(true),
            n => FeatureValue::Alternate(n),
        }
    }
    /// Import boundary for signed alternate indices: negative or over `u32::MAX` refuses.
    pub fn alternate_from_signed(value: i64) -> Result<Self, FeatureError> {
        if value < 0 {
            return Err(FeatureError::NegativeAlternateIndex);
        }
        u32::try_from(value)
            .map(FeatureValue::Alternate)
            .map_err(|_| FeatureError::AlternateIndexOverflow)
    }
    pub const fn is_on(self) -> bool {
        self.shaping_value() != 0
    }
}

/// Hard bound on stored entries (hostile documents); not a closed tag list.
pub const FEATURE_MAP_MAX_ENTRIES: usize = 1024;

/// STU-TYP-170 tag map of one character range, ordered by tag bytes. The only feature storage:
/// group selections, legacy booleans, figure style and stylistic sets all read and write it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeatureMap {
    entries: BTreeMap<FeatureTag, FeatureValue>,
}
impl FeatureMap {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn get(&self, tag: FeatureTag) -> Option<FeatureValue> {
        self.entries.get(&tag).copied()
    }
    pub fn iter(&self) -> impl Iterator<Item = (FeatureTag, FeatureValue)> + '_ {
        self.entries.iter().map(|(tag, value)| (*tag, *value))
    }
    /// Stores `value` under `tag` (registered or not); returns the previous value.
    pub fn set(
        &mut self,
        tag: FeatureTag,
        value: FeatureValue,
    ) -> Result<Option<FeatureValue>, FeatureError> {
        if !self.entries.contains_key(&tag) && self.entries.len() >= FEATURE_MAP_MAX_ENTRIES {
            return Err(FeatureError::FeatureMapFull);
        }
        Ok(self.entries.insert(tag, value))
    }
    /// `set` from a tag string; a malformed tag leaves the map unchanged.
    pub fn set_str(
        &mut self,
        tag: &str,
        value: FeatureValue,
    ) -> Result<Option<FeatureValue>, FeatureError> {
        let tag = FeatureTag::parse(tag)?;
        self.set(tag, value)
    }
    pub fn remove(&mut self, tag: FeatureTag) -> Option<FeatureValue> {
        self.entries.remove(&tag)
    }
    fn is_on(&self, tag: FeatureTag) -> bool {
        self.get(tag).is_some_and(FeatureValue::is_on)
    }
    /// Shaping input for `range`, in tag order. `declared` is false: a tag the active font lacks
    /// shapes as `UnavailableInert` and stays stored (STU-TYP-172).
    pub fn to_features(&self, range: SourceRange) -> Vec<Feature> {
        self.entries
            .iter()
            .map(|(tag, value)| Feature {
                tag: tag.bytes(),
                value: value.shaping_value(),
                range,
                declared: false,
            })
            .collect()
    }
    /// Map of one range's shaping features (values canonicalised by
    /// [`FeatureValue::from_shaping_value`]). All features must share one range.
    pub fn from_features(features: &[Feature]) -> Result<Self, FeatureError> {
        let mut map = FeatureMap::new();
        let Some(first) = features.first() else {
            return Ok(map);
        };
        for feature in features {
            if feature.range != first.range {
                return Err(FeatureError::MixedRanges);
            }
            let tag = FeatureTag::new(feature.tag)?;
            let value = FeatureValue::from_shaping_value(feature.value);
            match map.get(tag) {
                Some(existing) if existing != value => {
                    return Err(FeatureError::ConflictingFeatureValue);
                }
                _ => {
                    map.set(tag, value)?;
                }
            }
        }
        Ok(map)
    }
}

/// STU-TYP-171 group (presentation; the tag map is authority).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FeatureGroup {
    Ligatures,
    Alternates,
    StylisticSets,
    CharacterVariants,
    FigureStyle,
    FigureWidth,
    Fractions,
    Ordinals,
    VerticalPosition,
    Zero,
    CapsForms,
    PositionalForms,
    ArabicJustification,
    Indic,
    CjkWidthAndForm,
    CjkVariants,
    VerticalWriting,
    Kana,
    KerningAndMarks,
    Normalisation,
}
const FIXED_GROUP_TAGS: [(FeatureGroup, &[FeatureTag]); 18] = [
    (
        FeatureGroup::Ligatures,
        &[lit(b"liga"), lit(b"dlig"), lit(b"clig"), lit(b"rlig"), lit(b"hlig")],
    ),
    (
        FeatureGroup::Alternates,
        &[
            lit(b"salt"),
            lit(b"calt"),
            lit(b"titl"),
            lit(b"swsh"),
            lit(b"cswh"),
            lit(b"ornm"),
            lit(b"aalt"),
        ],
    ),
    (FeatureGroup::FigureStyle, &[lit(b"lnum"), lit(b"onum")]),
    (FeatureGroup::FigureWidth, &[lit(b"pnum"), lit(b"tnum")]),
    (FeatureGroup::Fractions, &[lit(b"frac"), lit(b"afrc")]),
    (FeatureGroup::Ordinals, &[lit(b"ordn")]),
    (
        FeatureGroup::VerticalPosition,
        &[lit(b"sups"), lit(b"subs"), lit(b"numr"), lit(b"dnom")],
    ),
    (FeatureGroup::Zero, &[lit(b"zero")]),
    (
        FeatureGroup::CapsForms,
        &[
            lit(b"smcp"),
            lit(b"c2sc"),
            lit(b"pcap"),
            lit(b"c2pc"),
            lit(b"cpsp"),
            lit(b"case"),
        ],
    ),
    (
        FeatureGroup::PositionalForms,
        &[lit(b"init"), lit(b"medi"), lit(b"fina"), lit(b"isol"), lit(b"curs")],
    ),
    (
        FeatureGroup::ArabicJustification,
        &[lit(b"jalt"), lit(b"mset"), lit(b"cfar")],
    ),
    (
        FeatureGroup::Indic,
        &[
            lit(b"akhn"),
            lit(b"blwf"),
            lit(b"blws"),
            lit(b"blwm"),
            lit(b"abvf"),
            lit(b"abvs"),
            lit(b"abvm"),
            lit(b"cjct"),
            lit(b"half"),
            lit(b"haln"),
            lit(b"nukt"),
            lit(b"pref"),
            lit(b"pres"),
            lit(b"pstf"),
            lit(b"psts"),
            lit(b"rkrf"),
            lit(b"rphf"),
            lit(b"vatu"),
        ],
    ),
    (
        FeatureGroup::CjkWidthAndForm,
        &[
            lit(b"halt"),
            lit(b"hwid"),
            lit(b"fwid"),
            lit(b"pwid"),
            lit(b"twid"),
            lit(b"qwid"),
            lit(b"palt"),
            lit(b"vhal"),
            lit(b"vpal"),
        ],
    ),
    (
        FeatureGroup::CjkVariants,
        &[
            lit(b"jp78"),
            lit(b"jp83"),
            lit(b"jp90"),
            lit(b"jp04"),
            lit(b"nlck"),
            lit(b"trad"),
            lit(b"smpl"),
            lit(b"expt"),
            lit(b"hojo"),
        ],
    ),
    (
        FeatureGroup::VerticalWriting,
        &[
            lit(b"vert"),
            lit(b"vrt2"),
            lit(b"vkna"),
            lit(b"vkrn"),
            lit(b"valt"),
            lit(b"vrtr"),
        ],
    ),
    (FeatureGroup::Kana, &[lit(b"hkna"), lit(b"pkna"), lit(b"ruby")]),
    (
        FeatureGroup::KerningAndMarks,
        &[lit(b"kern"), lit(b"mark"), lit(b"mkmk"), lit(b"abvm"), lit(b"blwm")],
    ),
    (
        FeatureGroup::Normalisation,
        &[lit(b"ccmp"), lit(b"locl"), lit(b"rvrn"), lit(b"rclt")],
    ),
];
impl FeatureGroup {
    /// The 20 groups in the order of the STU-TYP-171 table.
    pub const ALL: [FeatureGroup; 20] = [
        FeatureGroup::Ligatures,
        FeatureGroup::Alternates,
        FeatureGroup::StylisticSets,
        FeatureGroup::CharacterVariants,
        FeatureGroup::FigureStyle,
        FeatureGroup::FigureWidth,
        FeatureGroup::Fractions,
        FeatureGroup::Ordinals,
        FeatureGroup::VerticalPosition,
        FeatureGroup::Zero,
        FeatureGroup::CapsForms,
        FeatureGroup::PositionalForms,
        FeatureGroup::ArabicJustification,
        FeatureGroup::Indic,
        FeatureGroup::CjkWidthAndForm,
        FeatureGroup::CjkVariants,
        FeatureGroup::VerticalWriting,
        FeatureGroup::Kana,
        FeatureGroup::KerningAndMarks,
        FeatureGroup::Normalisation,
    ];
    /// The group name exactly as the STU-TYP-171 table writes it.
    pub const fn label(self) -> &'static str {
        match self {
            FeatureGroup::Ligatures => "Ligatures",
            FeatureGroup::Alternates => "Alternates",
            FeatureGroup::StylisticSets => "Stylistic sets",
            FeatureGroup::CharacterVariants => "Character variants",
            FeatureGroup::FigureStyle => "Figure style",
            FeatureGroup::FigureWidth => "Figure width",
            FeatureGroup::Fractions => "Fractions",
            FeatureGroup::Ordinals => "Ordinals",
            FeatureGroup::VerticalPosition => "Vertical position",
            FeatureGroup::Zero => "Zero",
            FeatureGroup::CapsForms => "Caps forms",
            FeatureGroup::PositionalForms => "Positional forms",
            FeatureGroup::ArabicJustification => "Arabic justification",
            FeatureGroup::Indic => "Indic",
            FeatureGroup::CjkWidthAndForm => "CJK width and form",
            FeatureGroup::CjkVariants => "CJK variants",
            FeatureGroup::VerticalWriting => "Vertical writing",
            FeatureGroup::Kana => "Kana",
            FeatureGroup::KerningAndMarks => "Kerning and marks",
            FeatureGroup::Normalisation => "Normalisation",
        }
    }
    /// Member tags in table order.
    pub fn tags(self) -> Vec<FeatureTag> {
        match self {
            FeatureGroup::StylisticSets => (1..=20).filter_map(|n| numbered(*b"ss", n)).collect(),
            FeatureGroup::CharacterVariants => {
                (1..=99).filter_map(|n| numbered(*b"cv", n)).collect()
            }
            _ => FIXED_GROUP_TAGS
                .iter()
                .find(|(group, _)| *group == self)
                .map(|(_, tags)| tags.to_vec())
                .unwrap_or_default(),
        }
    }
    pub fn contains(self, tag: FeatureTag) -> bool {
        self.tags().contains(&tag)
    }
    /// Stores a selection of a member tag in the map; a non-member refuses unchanged.
    pub fn select(
        self,
        map: &mut FeatureMap,
        tag: FeatureTag,
        value: FeatureValue,
    ) -> Result<Option<FeatureValue>, FeatureError> {
        if !self.contains(tag) {
            return Err(FeatureError::TagNotInGroup);
        }
        map.set(tag, value)
    }
    /// The stored entries of this group's tags, in table order (a projection of the map).
    pub fn selection(self, map: &FeatureMap) -> Vec<(FeatureTag, FeatureValue)> {
        self.tags()
            .into_iter()
            .filter_map(|tag| map.get(tag).map(|value| (tag, value)))
            .collect()
    }
}

/// STU-TYP-170 status of a registry tag (v02.228, SD-30).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FeatureStatus {
    /// Registered in OpenType 1.9.1 and not deprecated.
    Registered,
    /// Registered but deprecated (`hngl`, `opbd`): stored and applied like any tag, not offered
    /// for new authoring.
    Deprecated,
    /// Carried by the 229-tag observed union but absent from the OpenType registry (`abrv`,
    /// `just`).
    ObservedUnregistered,
}
impl FeatureStatus {
    /// The status word as the STU-TYP-170 table writes it.
    pub const fn label(self) -> &'static str {
        match self {
            FeatureStatus::Registered => "registered",
            FeatureStatus::Deprecated => "deprecated",
            FeatureStatus::ObservedUnregistered => "observed_unregistered",
        }
    }
}

/// One STU-TYP-170 registry row with its STU-TYP-171 group memberships (`abvm` and `blwm` have two;
/// a tag outside the group table, such as `sinf`, has none).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegistryEntry {
    pub tag: FeatureTag,
    /// Friendly name as the table writes it (`(not registered)` for `abrv` and `just`).
    pub name: &'static str,
    pub status: FeatureStatus,
    /// True when the 229-tag observed union carries the tag.
    pub observed: bool,
    groups: [Option<FeatureGroup>; 2],
}
impl RegistryEntry {
    pub fn groups(&self) -> impl Iterator<Item = FeatureGroup> + '_ {
        self.groups.iter().flatten().copied()
    }
    /// False for a deprecated tag: it is stored and applied like any other and is not offered for
    /// new authoring.
    pub const fn offered_for_new_authoring(&self) -> bool {
        !matches!(self.status, FeatureStatus::Deprecated)
    }
}
/// The STU-TYP-170 registry: the 245 rows of the table, sorted by tag bytes. Unregistered
/// well-formed tags remain valid map entries.
pub fn registry() -> &'static [RegistryEntry] {
    static REGISTRY: OnceLock<Vec<RegistryEntry>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut entries: Vec<RegistryEntry> = ROWS
            .iter()
            .map(|(bytes, name, status, observed)| RegistryEntry {
                tag: FeatureTag(*bytes),
                name: *name,
                status: *status,
                observed: *observed,
                groups: [None, None],
            })
            .collect();
        entries.sort_by_key(|entry| entry.tag);
        for group in FeatureGroup::ALL {
            for tag in group.tags() {
                if let Ok(index) = entries.binary_search_by_key(&tag, |entry| entry.tag)
                    && let Some(entry) = entries.get_mut(index)
                {
                    let slot = usize::from(entry.groups[0].is_some());
                    entry.groups[slot] = Some(group);
                }
            }
        }
        entries
    })
}
pub fn registry_entry(tag: FeatureTag) -> Option<&'static RegistryEntry> {
    let entries = registry();
    entries
        .binary_search_by_key(&tag, |entry| entry.tag)
        .ok()
        .and_then(|index| entries.get(index))
}

/// STU-TYP-017 normative feature surface enumeration (13 members, deduped across the suites).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StudioFeatureGroup {
    Ligatures,
    Alternates,
    StylisticSets,
    FigureStyle,
    FigureWidth,
    Fractions,
    Ordinals,
    SuperscriptSubscript,
    NumeratorDenominator,
    SlashedZero,
    CapsForms,
    PositionalForms,
    KerningMarks,
}
impl StudioFeatureGroup {
    pub const ALL: [StudioFeatureGroup; 13] = [
        StudioFeatureGroup::Ligatures,
        StudioFeatureGroup::Alternates,
        StudioFeatureGroup::StylisticSets,
        StudioFeatureGroup::FigureStyle,
        StudioFeatureGroup::FigureWidth,
        StudioFeatureGroup::Fractions,
        StudioFeatureGroup::Ordinals,
        StudioFeatureGroup::SuperscriptSubscript,
        StudioFeatureGroup::NumeratorDenominator,
        StudioFeatureGroup::SlashedZero,
        StudioFeatureGroup::CapsForms,
        StudioFeatureGroup::PositionalForms,
        StudioFeatureGroup::KerningMarks,
    ];
    /// Member name exactly as the STU-TYP-017 table writes it.
    pub const fn label(self) -> &'static str {
        match self {
            StudioFeatureGroup::Ligatures => "Ligatures",
            StudioFeatureGroup::Alternates => "Alternates",
            StudioFeatureGroup::StylisticSets => "Stylistic sets",
            StudioFeatureGroup::FigureStyle => "Figure style",
            StudioFeatureGroup::FigureWidth => "Figure width",
            StudioFeatureGroup::Fractions => "Fractions",
            StudioFeatureGroup::Ordinals => "Ordinals",
            StudioFeatureGroup::SuperscriptSubscript => "Superscript / subscript",
            StudioFeatureGroup::NumeratorDenominator => "Numerator / denominator",
            StudioFeatureGroup::SlashedZero => "Slashed zero",
            StudioFeatureGroup::CapsForms => "Caps forms",
            StudioFeatureGroup::PositionalForms => "Positional forms",
            StudioFeatureGroup::KerningMarks => "Kerning / marks",
        }
    }
    /// "Members / behavior" exactly as the STU-TYP-017 table writes it (tooltip text).
    pub const fn meaning(self) -> &'static str {
        match self {
            StudioFeatureGroup::Ligatures => "Standard, discretionary, and contextual ligatures",
            StudioFeatureGroup::Alternates => {
                "Stylistic alternates, contextual alternates, titling alternates, swash, ornaments"
            }
            StudioFeatureGroup::StylisticSets => "Named/numbered stylistic set toggles",
            StudioFeatureGroup::FigureStyle => "Lining vs oldstyle numerals",
            StudioFeatureGroup::FigureWidth => "Proportional vs tabular numerals",
            StudioFeatureGroup::Fractions => "Diagonal fractions and stacked fractions",
            StudioFeatureGroup::Ordinals => "Ordinal indicators",
            StudioFeatureGroup::SuperscriptSubscript => "True OpenType figure/letter positions",
            StudioFeatureGroup::NumeratorDenominator => "Fraction building blocks",
            StudioFeatureGroup::SlashedZero => "Disambiguated zero",
            StudioFeatureGroup::CapsForms => {
                "Small caps, all-small-caps, petite caps, capital spacing/forms"
            }
            StudioFeatureGroup::PositionalForms => {
                "Initial/medial/final/isolated forms for connected scripts"
            }
            StudioFeatureGroup::KerningMarks => "OpenType kerning, mark positioning",
        }
    }
    /// The member's OpenType tags exactly as the STU-TYP-017 table lists them.
    pub fn tags(self) -> Vec<FeatureTag> {
        let fixed: &[FeatureTag] = match self {
            StudioFeatureGroup::StylisticSets => {
                return (1..=20).filter_map(|n| numbered(*b"ss", n)).collect();
            }
            StudioFeatureGroup::Ligatures => {
                &[lit(b"liga"), lit(b"dlig"), lit(b"clig"), lit(b"rlig")]
            }
            StudioFeatureGroup::Alternates => &[
                lit(b"salt"),
                lit(b"calt"),
                lit(b"titl"),
                lit(b"swsh"),
                lit(b"ornm"),
            ],
            StudioFeatureGroup::FigureStyle => &[lit(b"lnum"), lit(b"onum")],
            StudioFeatureGroup::FigureWidth => &[lit(b"pnum"), lit(b"tnum")],
            StudioFeatureGroup::Fractions => &[lit(b"frac"), lit(b"afrc")],
            StudioFeatureGroup::Ordinals => &[lit(b"ordn")],
            StudioFeatureGroup::SuperscriptSubscript => &[lit(b"sups"), lit(b"subs")],
            StudioFeatureGroup::NumeratorDenominator => &[lit(b"numr"), lit(b"dnom")],
            StudioFeatureGroup::SlashedZero => &[lit(b"zero")],
            StudioFeatureGroup::CapsForms => &[
                lit(b"smcp"),
                lit(b"c2sc"),
                lit(b"pcap"),
                lit(b"cpsp"),
                lit(b"case"),
            ],
            StudioFeatureGroup::PositionalForms => {
                &[lit(b"init"), lit(b"medi"), lit(b"fina"), lit(b"isol")]
            }
            StudioFeatureGroup::KerningMarks => &[lit(b"kern"), lit(b"mark"), lit(b"mkmk")],
        };
        fixed.to_vec()
    }
    pub fn contains(self, tag: FeatureTag) -> bool {
        self.tags().contains(&tag)
    }
    /// Decodes a selection of a member tag into the map; a non-member refuses unchanged.
    pub fn select(
        self,
        map: &mut FeatureMap,
        tag: FeatureTag,
        value: FeatureValue,
    ) -> Result<Option<FeatureValue>, FeatureError> {
        if !self.contains(tag) {
            return Err(FeatureError::TagNotInGroup);
        }
        map.set(tag, value)
    }
    pub fn selection(self, map: &FeatureMap) -> Vec<(FeatureTag, FeatureValue)> {
        self.tags()
            .into_iter()
            .filter_map(|tag| map.get(tag).map(|value| (tag, value)))
            .collect()
    }
}

/// What the active font says about a tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureAvailability {
    /// No font was queried.
    Unknown,
    /// The font declares the tag; `alternates` is the largest GSUB AlternateSet of its type-3
    /// lookups (0 when it has none).
    Declared { alternates: u32 },
    NotDeclared,
}
/// One font-declared feature tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FontFeature {
    pub tag: FeatureTag,
    pub alternates: u32,
    pub gsub: bool,
    pub gpos: bool,
}
/// STU-TYP-173 result for one font resource: the sorted, deduplicated GSUB and GPOS feature tags
/// reachable from any script and language system (required feature included).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FontFeatures {
    features: Vec<FontFeature>,
}
impl FontFeatures {
    pub fn features(&self) -> &[FontFeature] {
        &self.features
    }
    pub fn tags(&self) -> impl Iterator<Item = FeatureTag> + '_ {
        self.features.iter().map(|feature| feature.tag)
    }
    fn find(&self, tag: FeatureTag) -> Option<&FontFeature> {
        self.features
            .binary_search_by_key(&tag, |feature| feature.tag)
            .ok()
            .and_then(|index| self.features.get(index))
    }
    /// STU-TYP-173 per-font predicate: true only when the font declares `tag`; registry
    /// membership never counts as font support.
    pub fn check_opentype_feature(&self, tag: FeatureTag) -> bool {
        self.find(tag).is_some()
    }
    pub fn alternate_count(&self, tag: FeatureTag) -> Option<u32> {
        self.find(tag).map(|feature| feature.alternates)
    }
    pub fn availability(&self, tag: FeatureTag) -> FeatureAvailability {
        match self.find(tag) {
            Some(feature) => FeatureAvailability::Declared {
                alternates: feature.alternates,
            },
            None => FeatureAvailability::NotDeclared,
        }
    }
}

/// One tag in the grouped projection: its stored value and the font's availability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupedFeature {
    pub tag: FeatureTag,
    pub stored: Option<FeatureValue>,
    pub availability: FeatureAvailability,
}
impl GroupedFeature {
    /// STU-TYP-017: offered only when the active font declares it.
    pub fn offered(&self) -> bool {
        matches!(self.availability, FeatureAvailability::Declared { .. })
    }
    /// STU-TYP-172: stored but not declared by the active font.
    pub fn inert(&self) -> bool {
        self.stored.is_some() && self.availability == FeatureAvailability::NotDeclared
    }
}
/// STU-TYP-171 presentation over the map (and optionally a font's availability). Building it
/// never changes the map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeatureGroupView {
    pub groups: Vec<(FeatureGroup, Vec<GroupedFeature>)>,
    /// Stored or font-declared tags outside every STU-TYP-171 group (a registered tag such as
    /// `sinf` or an unregistered custom tag); they stay addressable by tag.
    pub ungrouped: Vec<GroupedFeature>,
}
pub fn project_groups(map: &FeatureMap, font: Option<&FontFeatures>) -> FeatureGroupView {
    let entry = |tag: FeatureTag| GroupedFeature {
        tag,
        stored: map.get(tag),
        availability: font.map_or(FeatureAvailability::Unknown, |font| font.availability(tag)),
    };
    let groups = FeatureGroup::ALL
        .iter()
        .map(|group| (*group, group.tags().into_iter().map(entry).collect()))
        .collect();
    let mut ungrouped: Vec<FeatureTag> = map
        .iter()
        .map(|(tag, _)| tag)
        .chain(font.into_iter().flat_map(FontFeatures::tags))
        .filter(|tag| registry_entry(*tag).is_none_or(|entry| entry.groups().next().is_none()))
        .collect();
    ungrouped.sort();
    ungrouped.dedup();
    FeatureGroupView {
        groups,
        ungrouped: ungrouped.into_iter().map(entry).collect(),
    }
}

/// STU-TYP-174 legacy boolean projections.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LegacyBoolean {
    Ligatures,
    DiscretionaryLigature,
    ContextualAlternate,
    Swash,
    Titling,
    Ordinal,
    Fraction,
    SlashedZero,
    StylisticAlternate,
    OverlapSwash,
    StretchedAlternate,
    JustificationAlternate,
    HvKana,
    ProportionalMetrics,
}
impl LegacyBoolean {
    /// The 14 booleans in STU-TYP-174 order.
    pub const ALL: [LegacyBoolean; 14] = [
        LegacyBoolean::Ligatures,
        LegacyBoolean::DiscretionaryLigature,
        LegacyBoolean::ContextualAlternate,
        LegacyBoolean::Swash,
        LegacyBoolean::Titling,
        LegacyBoolean::Ordinal,
        LegacyBoolean::Fraction,
        LegacyBoolean::SlashedZero,
        LegacyBoolean::StylisticAlternate,
        LegacyBoolean::OverlapSwash,
        LegacyBoolean::StretchedAlternate,
        LegacyBoolean::JustificationAlternate,
        LegacyBoolean::HvKana,
        LegacyBoolean::ProportionalMetrics,
    ];
    /// The boolean's name as STU-TYP-174 writes it.
    pub const fn name(self) -> &'static str {
        match self {
            LegacyBoolean::Ligatures => "ligatures",
            LegacyBoolean::DiscretionaryLigature => "discretionary_ligature",
            LegacyBoolean::ContextualAlternate => "contextual_alternate",
            LegacyBoolean::Swash => "swash",
            LegacyBoolean::Titling => "titling",
            LegacyBoolean::Ordinal => "ordinal",
            LegacyBoolean::Fraction => "fraction",
            LegacyBoolean::SlashedZero => "slashed_zero",
            LegacyBoolean::StylisticAlternate => "stylistic_alternate",
            LegacyBoolean::OverlapSwash => "overlap_swash",
            LegacyBoolean::StretchedAlternate => "stretched_alternate",
            LegacyBoolean::JustificationAlternate => "justification_alternate",
            LegacyBoolean::HvKana => "hv_kana",
            LegacyBoolean::ProportionalMetrics => "proportional_metrics",
        }
    }
    /// The documented boolean-to-tag mapping (module docs); unmapped booleans refuse.
    pub const fn tag(self) -> Result<FeatureTag, FeatureError> {
        Ok(match self {
            LegacyBoolean::Ligatures => lit(b"liga"),
            LegacyBoolean::DiscretionaryLigature => lit(b"dlig"),
            LegacyBoolean::ContextualAlternate => lit(b"calt"),
            LegacyBoolean::Swash => lit(b"swsh"),
            LegacyBoolean::Titling => lit(b"titl"),
            LegacyBoolean::Ordinal => lit(b"ordn"),
            LegacyBoolean::Fraction => lit(b"frac"),
            LegacyBoolean::SlashedZero => lit(b"zero"),
            LegacyBoolean::StylisticAlternate => lit(b"salt"),
            LegacyBoolean::JustificationAlternate => lit(b"jalt"),
            LegacyBoolean::ProportionalMetrics => lit(b"palt"),
            LegacyBoolean::OverlapSwash
            | LegacyBoolean::StretchedAlternate
            | LegacyBoolean::HvKana => return Err(FeatureError::LegacyProjectionUnmapped),
        })
    }
}

/// STU-TYP-175 figure style, a projection onto the `lnum`/`onum` x `pnum`/`tnum` pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FigureStyle {
    Default,
    TabularLining,
    ProportionalOldstyle,
    ProportionalLining,
    TabularOldstyle,
}
const LNUM: FeatureTag = lit(b"lnum");
const ONUM: FeatureTag = lit(b"onum");
const PNUM: FeatureTag = lit(b"pnum");
const TNUM: FeatureTag = lit(b"tnum");
impl FigureStyle {
    pub const ALL: [FigureStyle; 5] = [
        FigureStyle::Default,
        FigureStyle::TabularLining,
        FigureStyle::ProportionalOldstyle,
        FigureStyle::ProportionalLining,
        FigureStyle::TabularOldstyle,
    ];
    pub const fn name(self) -> &'static str {
        match self {
            FigureStyle::Default => "default",
            FigureStyle::TabularLining => "tabular_lining",
            FigureStyle::ProportionalOldstyle => "proportional_oldstyle",
            FigureStyle::ProportionalLining => "proportional_lining",
            FigureStyle::TabularOldstyle => "tabular_oldstyle",
        }
    }
    /// The (figure style tag, figure width tag) pair; `None` for `Default`.
    pub const fn tags(self) -> Option<(FeatureTag, FeatureTag)> {
        match self {
            FigureStyle::Default => None,
            FigureStyle::TabularLining => Some((LNUM, TNUM)),
            FigureStyle::ProportionalOldstyle => Some((ONUM, PNUM)),
            FigureStyle::ProportionalLining => Some((LNUM, PNUM)),
            FigureStyle::TabularOldstyle => Some((ONUM, TNUM)),
        }
    }
}

/// Bit i of the source bitfield is `ss(i+1)` (Studio reading, bit order UNVERIFIED).
pub const STYLISTIC_SET_BITS: u32 = 20;

impl FeatureMap {
    /// A legacy boolean reads its tag's entry (`true` when stored on).
    pub fn legacy_boolean(&self, boolean: LegacyBoolean) -> Result<bool, FeatureError> {
        Ok(self.is_on(boolean.tag()?))
    }
    /// Writes the boolean as an explicit `Binary` entry of its tag (an explicit `false` is kept,
    /// e.g. `liga` off, which shapers apply by default).
    pub fn set_legacy_boolean(
        &mut self,
        boolean: LegacyBoolean,
        on: bool,
    ) -> Result<(), FeatureError> {
        self.set(boolean.tag()?, FeatureValue::Binary(on)).map(|_| ())
    }
    pub fn figure_style(&self) -> Result<FigureStyle, FeatureError> {
        let state = (
            self.is_on(LNUM),
            self.is_on(ONUM),
            self.is_on(PNUM),
            self.is_on(TNUM),
        );
        Ok(match state {
            (false, false, false, false) => FigureStyle::Default,
            (true, false, false, true) => FigureStyle::TabularLining,
            (false, true, true, false) => FigureStyle::ProportionalOldstyle,
            (true, false, true, false) => FigureStyle::ProportionalLining,
            (false, true, false, true) => FigureStyle::TabularOldstyle,
            _ => return Err(FeatureError::FigureStyleUnrepresentable),
        })
    }
    /// Replaces the four figure entries with exactly the style's pair (none for `Default`).
    /// All-or-nothing: a full map refuses unchanged.
    pub fn set_figure_style(&mut self, style: FigureStyle) -> Result<(), FeatureError> {
        let mut next = self.clone();
        for tag in [LNUM, ONUM, PNUM, TNUM] {
            next.remove(tag);
        }
        if let Some((figure, width)) = style.tags() {
            next.set(figure, FeatureValue::Binary(true))?;
            next.set(width, FeatureValue::Binary(true))?;
        }
        *self = next;
        Ok(())
    }
    /// Import boundary (STU-TYP-175): the bitfield is the complete set state, so set bits store
    /// `ssNN` on and clear bits remove `ssNN`. Bits beyond `ss20` refuse unchanged.
    pub fn import_stylistic_sets(&mut self, bits: u64) -> Result<(), FeatureError> {
        if bits.checked_shr(STYLISTIC_SET_BITS).unwrap_or(0) != 0 {
            return Err(FeatureError::StylisticSetBitsLost);
        }
        let mut next = self.clone();
        for index in 0..STYLISTIC_SET_BITS {
            let tag = stylistic_set_tag(index)?;
            if bits.checked_shr(index).unwrap_or(0) & 1 == 1 {
                next.set(tag, FeatureValue::Binary(true))?;
            } else {
                next.remove(tag);
            }
        }
        *self = next;
        Ok(())
    }
    /// Export boundary: bit i set when `ss(i+1)` is stored on.
    pub fn export_stylistic_sets(&self) -> u32 {
        let mut bits = 0u32;
        for index in 0..STYLISTIC_SET_BITS {
            if stylistic_set_tag(index).is_ok_and(|tag| self.is_on(tag)) {
                bits |= 1u32.checked_shl(index).unwrap_or(0);
            }
        }
        bits
    }
}
fn stylistic_set_tag(index: u32) -> Result<FeatureTag, FeatureError> {
    u8::try_from(index)
        .ok()
        .and_then(|n| n.checked_add(1))
        .and_then(|n| numbered(*b"ss", n))
        .ok_or(FeatureError::StylisticSetBitsLost)
}

/// Inputs of `TextEngine::font_features`: one caller-granted font resource, loaded only through
/// the resolver with revision, lease and content-hash checks (no file reads).
#[derive(Clone, Copy)]
pub struct FeatureQuery<'a> {
    pub font: FontResource<'a>,
    pub limits: Limits,
    pub cancellation: &'a CancellationToken,
    pub cancel_epoch: u64,
}

pub(crate) fn query<'a>(
    query: FeatureQuery<'a>,
    ports: &Ports<'a>,
) -> Result<FontFeatures, FeatureError> {
    let ctx = Context::new(
        query.cancellation,
        ports.context,
        query.cancel_epoch,
        ports.admission,
        query.limits.work_units,
        query.limits.requested_owned_bytes,
        query.limits.recursion,
    )?;
    // Face construction meters GSUB/GPOS lookups and glyphs against the shape limits.
    ctx.set_shape_limits(query.limits.glyphs, query.limits.lookups)?;
    let font = crate::fonts::load(query.font, ports, query.limits, &ctx)?;
    let raw = ttf::RawFace::parse(font.read.bytes, font.resource.face_index)
        .map_err(|_| Error::CorruptFont)?;
    let gsub = raw.table_bounded(ttf::Tag::from_bytes(b"GSUB"), &ctx)?;
    let gpos = raw.table_bounded(ttf::Tag::from_bytes(b"GPOS"), &ctx)?;
    let mut budget = Aggregate::default();
    let mut features: Vec<FontFeature> = Vec::new();
    for (table, is_gsub) in [(gsub, true), (gpos, false)] {
        let Some(table) = table else { continue };
        let declared = declared_features(table, is_gsub, query.limits, &mut budget, &ctx)?;
        features
            .try_reserve(declared.len())
            .map_err(|_| Error::Allocation)?;
        for (tag, alternates) in declared {
            features.push(FontFeature {
                tag,
                alternates,
                gsub: is_gsub,
                gpos: !is_gsub,
            });
        }
    }
    features.sort_by_key(|feature| feature.tag);
    let mut merged: Vec<FontFeature> = Vec::with_capacity(features.len());
    for feature in features {
        ctx.step()?;
        match merged.last_mut() {
            Some(last) if last.tag == feature.tag => {
                last.alternates = last.alternates.max(feature.alternates);
                last.gsub |= feature.gsub;
                last.gpos |= feature.gpos;
            }
            _ => merged.push(feature),
        }
    }
    ports.resolver.verify_lifetime(&font.resource, &font.read)?;
    ctx.poll()?;
    Ok(FontFeatures { features: merged })
}

/// Feature and lookup counts summed over GSUB and GPOS.
#[derive(Default)]
struct Aggregate {
    features: u64,
    lookups: u64,
}

fn at(base: usize, offset: usize, error: FeatureError) -> Result<usize, FeatureError> {
    base.checked_add(offset).ok_or(error)
}
fn u16_at(data: &[u8], offset: usize, error: FeatureError) -> Result<u16, FeatureError> {
    let end = at(offset, 2, error)?;
    let bytes = data.get(offset..end).ok_or(error)?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}
fn u32_at(data: &[u8], offset: usize, error: FeatureError) -> Result<u32, FeatureError> {
    let end = at(offset, 4, error)?;
    let bytes = data.get(offset..end).ok_or(error)?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}
/// `start + head + count * width <= data.len()`, checked before reading or allocating.
fn array_fits(
    data: &[u8],
    start: usize,
    head: usize,
    count: u16,
    width: usize,
    error: FeatureError,
) -> Result<(), FeatureError> {
    let bytes = usize::from(count).checked_mul(width).ok_or(error)?;
    let end = at(at(start, head, error)?, bytes, error)?;
    if end > data.len() {
        return Err(error);
    }
    Ok(())
}

/// Reached (tag, alternate count) pairs of one GSUB/GPOS table, in FeatureList order.
fn declared_features<'a>(
    data: &[u8],
    is_gsub: bool,
    limits: Limits,
    budget: &mut Aggregate,
    ctx: &Context<'a>,
) -> Result<Vec<(FeatureTag, u32)>, FeatureError> {
    use FeatureError as E;
    ctx.step()?;
    if u16_at(data, 0, E::LayoutTableTruncated)? != 1 {
        return Err(E::LayoutVersionUnsupported);
    }
    let script_list = usize::from(u16_at(data, 4, E::LayoutTableTruncated)?);
    let feature_list = usize::from(u16_at(data, 6, E::LayoutTableTruncated)?);
    let lookup_list = usize::from(u16_at(data, 8, E::LayoutTableTruncated)?);
    // Counts, the GSUB+GPOS aggregate and each record array's fit in the table are all checked
    // before any allocation (V4-01/02).
    let feature_count = if feature_list == 0 {
        0
    } else {
        u16_at(data, feature_list, E::FeatureListTruncated)?
    };
    let features_total = budget
        .features
        .checked_add(u64::from(feature_count))
        .ok_or(E::LayoutOverLimit)?;
    if features_total > limits.features {
        return Err(E::LayoutOverLimit);
    }
    if feature_count > 0 {
        array_fits(data, feature_list, 2, feature_count, 6, E::FeatureListTruncated)?;
    }
    let lookup_count = if lookup_list == 0 {
        0
    } else {
        u16_at(data, lookup_list, E::LookupListTruncated)?
    };
    let lookups_total = budget
        .lookups
        .checked_add(u64::from(lookup_count))
        .ok_or(E::LayoutOverLimit)?;
    if lookups_total > limits.lookups {
        return Err(E::LayoutOverLimit);
    }
    if lookup_count > 0 {
        array_fits(data, lookup_list, 2, lookup_count, 2, E::LookupListTruncated)?;
    }
    budget.features = features_total;
    budget.lookups = lookups_total;
    let mut reached: ChargedVec<'a, bool> = ChargedVec::new();
    reached.resize_copy(usize::from(feature_count), false, ctx)?;
    if script_list != 0 {
        mark_reached(data, script_list, feature_count, &mut reached, ctx)?;
    }
    let mut out: Vec<(FeatureTag, u32)> = Vec::new();
    out.try_reserve(reached.iter().filter(|r| **r).count())
        .map_err(|_| Error::Allocation)?;
    for index in 0..feature_count {
        ctx.step()?;
        if !reached.get(usize::from(index)).copied().unwrap_or(false) {
            continue;
        }
        let record = at(
            at(feature_list, 2, E::FeatureListTruncated)?,
            usize::from(index)
                .checked_mul(6)
                .ok_or(E::FeatureListTruncated)?,
            E::FeatureListTruncated,
        )?;
        let tag_end = at(record, 4, E::FeatureListTruncated)?;
        let tag_bytes: [u8; 4] = data
            .get(record..tag_end)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(E::FeatureListTruncated)?;
        let tag = FeatureTag::new(tag_bytes).map_err(|_| E::FontTagMalformed)?;
        let table = at(
            feature_list,
            usize::from(u16_at(data, tag_end, E::FeatureListTruncated)?),
            E::FeatureListTruncated,
        )?;
        let indices = u16_at(data, at(table, 2, E::FeatureListTruncated)?, E::FeatureListTruncated)?;
        array_fits(data, table, 4, indices, 2, E::FeatureListTruncated)?;
        let mut alternates = 0u32;
        for slot in 0..indices {
            ctx.step()?;
            let offset = at(
                table,
                usize::from(slot)
                    .checked_mul(2)
                    .and_then(|o| o.checked_add(4))
                    .ok_or(E::FeatureListTruncated)?,
                E::FeatureListTruncated,
            )?;
            let lookup = u16_at(data, offset, E::FeatureListTruncated)?;
            if lookup >= lookup_count {
                return Err(E::LookupIndexOutOfRange);
            }
            if is_gsub {
                alternates = alternates.max(lookup_alternates(data, lookup_list, lookup, ctx)?);
            }
        }
        out.push((tag, alternates));
    }
    Ok(out)
}

fn mark_reached(
    data: &[u8],
    script_list: usize,
    feature_count: u16,
    reached: &mut [bool],
    ctx: &Context<'_>,
) -> Result<(), FeatureError> {
    use FeatureError as E;
    let scripts = u16_at(data, script_list, E::ScriptListTruncated)?;
    array_fits(data, script_list, 2, scripts, 6, E::ScriptListTruncated)?;
    for index in 0..scripts {
        ctx.step()?;
        let record = record_at(script_list, index, E::ScriptListTruncated)?;
        let script = at(
            script_list,
            usize::from(u16_at(data, at(record, 4, E::ScriptListTruncated)?, E::ScriptListTruncated)?),
            E::ScriptListTruncated,
        )?;
        let default = usize::from(u16_at(data, script, E::ScriptListTruncated)?);
        let languages = u16_at(data, at(script, 2, E::ScriptListTruncated)?, E::ScriptListTruncated)?;
        array_fits(data, script, 4, languages, 6, E::ScriptListTruncated)?;
        if default != 0 {
            mark_language(data, at(script, default, E::ScriptListTruncated)?, feature_count, reached, ctx)?;
        }
        for language in 0..languages {
            ctx.step()?;
            let lang_record = at(
                at(script, 4, E::ScriptListTruncated)?,
                usize::from(language)
                    .checked_mul(6)
                    .ok_or(E::ScriptListTruncated)?,
                E::ScriptListTruncated,
            )?;
            let offset = usize::from(u16_at(
                data,
                at(lang_record, 4, E::ScriptListTruncated)?,
                E::ScriptListTruncated,
            )?);
            mark_language(data, at(script, offset, E::ScriptListTruncated)?, feature_count, reached, ctx)?;
        }
    }
    Ok(())
}
/// Record `index` of a tag+offset16 record array starting after a u16 count at `list`.
fn record_at(list: usize, index: u16, error: FeatureError) -> Result<usize, FeatureError> {
    at(
        at(list, 2, error)?,
        usize::from(index).checked_mul(6).ok_or(error)?,
        error,
    )
}
fn mark_language(
    data: &[u8],
    lang_sys: usize,
    feature_count: u16,
    reached: &mut [bool],
    ctx: &Context<'_>,
) -> Result<(), FeatureError> {
    use FeatureError as E;
    let required = u16_at(data, at(lang_sys, 2, E::ScriptListTruncated)?, E::ScriptListTruncated)?;
    let count = u16_at(data, at(lang_sys, 4, E::ScriptListTruncated)?, E::ScriptListTruncated)?;
    array_fits(data, lang_sys, 6, count, 2, E::ScriptListTruncated)?;
    let mut mark = |index: u16| -> Result<(), FeatureError> {
        if index >= feature_count {
            return Err(E::FeatureIndexOutOfRange);
        }
        if let Some(slot) = reached.get_mut(usize::from(index)) {
            *slot = true;
        }
        Ok(())
    };
    if required != 0xFFFF {
        mark(required)?;
    }
    for slot in 0..count {
        ctx.step()?;
        let offset = at(
            lang_sys,
            usize::from(slot)
                .checked_mul(2)
                .and_then(|o| o.checked_add(6))
                .ok_or(E::ScriptListTruncated)?,
            E::ScriptListTruncated,
        )?;
        mark(u16_at(data, offset, E::ScriptListTruncated)?)?;
    }
    Ok(())
}
/// Largest AlternateSet over the lookup's type-3 subtables (type-7 extensions resolved); 0 for
/// other lookup types.
fn lookup_alternates(
    data: &[u8],
    lookup_list: usize,
    lookup: u16,
    ctx: &Context<'_>,
) -> Result<u32, FeatureError> {
    use FeatureError as E;
    let slot = at(
        lookup_list,
        usize::from(lookup)
            .checked_mul(2)
            .and_then(|o| o.checked_add(2))
            .ok_or(E::LookupListTruncated)?,
        E::LookupListTruncated,
    )?;
    let table = at(
        lookup_list,
        usize::from(u16_at(data, slot, E::LookupListTruncated)?),
        E::LookupListTruncated,
    )?;
    let kind = u16_at(data, table, E::LookupListTruncated)?;
    if kind != 3 && kind != 7 {
        return Ok(0);
    }
    let subtables = u16_at(data, at(table, 4, E::LookupListTruncated)?, E::LookupListTruncated)?;
    array_fits(data, table, 6, subtables, 2, E::LookupListTruncated)?;
    let mut best = 0u32;
    for index in 0..subtables {
        ctx.step()?;
        let offset = at(
            table,
            usize::from(index)
                .checked_mul(2)
                .and_then(|o| o.checked_add(6))
                .ok_or(E::LookupListTruncated)?,
            E::LookupListTruncated,
        )?;
        let mut subtable = at(
            table,
            usize::from(u16_at(data, offset, E::LookupListTruncated)?),
            E::LookupListTruncated,
        )?;
        if kind == 7 {
            if u16_at(data, at(subtable, 2, E::LookupListTruncated)?, E::LookupListTruncated)? != 3 {
                continue;
            }
            let extension = u32_at(data, at(subtable, 4, E::LookupListTruncated)?, E::LookupListTruncated)?;
            subtable = at(
                subtable,
                usize::try_from(extension).map_err(|_| E::LookupListTruncated)?,
                E::LookupListTruncated,
            )?;
        }
        if u16_at(data, subtable, E::LookupListTruncated)? != 1 {
            continue;
        }
        let sets = u16_at(data, at(subtable, 4, E::LookupListTruncated)?, E::LookupListTruncated)?;
        array_fits(data, subtable, 6, sets, 2, E::LookupListTruncated)?;
        for set in 0..sets {
            ctx.step()?;
            let offset = at(
                subtable,
                usize::from(set)
                    .checked_mul(2)
                    .and_then(|o| o.checked_add(6))
                    .ok_or(E::LookupListTruncated)?,
                E::LookupListTruncated,
            )?;
            let alternate_set = at(
                subtable,
                usize::from(u16_at(data, offset, E::LookupListTruncated)?),
                E::LookupListTruncated,
            )?;
            let glyphs = u16_at(data, alternate_set, E::LookupListTruncated)?;
            array_fits(data, alternate_set, 2, glyphs, 2, E::LookupListTruncated)?;
            best = best.max(u32::from(glyphs));
        }
    }
    Ok(best)
}

