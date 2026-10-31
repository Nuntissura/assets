//! Typed Studio settings schema (lane SMB-SETTINGS-SCHEMA-1, RF-09 phase 0, asset side of MT-36249;
//! GUI-DESKTOP-014..019, SET-REC-001..004, SET-SCOPE-001). Additive: no existing option type
//! changes behaviour; a module declares `const` [`SettingEntry`] lists beside its option structs and
//! a host-facing catalog joins them in one [`SettingsSchema`].
//!
//! # Manual (no-context operator or model)
//!
//! - **Id.** [`SettingId`]: `studio.<module>.<feature>.<key>`, exactly four dot-separated segments,
//!   the first `studio`; each segment starts with `a-z`, holds only `a-z`, `0-9` and `-`, and does
//!   not end with `-`; the whole id is at most 128 bytes (the accord id bound). Module and feature
//!   are the existing crate and feature names (GUI-DESKTOP-014), so the id is the tab / subtab
//!   path: module tab, feature subtab, key.
//! - **Namespace.** Every id is the Handshake `preference_id` `<namespace>.<key>` with namespace
//!   [`SETTINGS_NAMESPACE`] (`studio`). SET-SCOPE-001 admits only `data-roots`, `view-defaults`,
//!   `retention` and `feature-toggles`; Studio tool defaults fit none, so the projection marks
//!   [`SETTINGS_NAMESPACE_STATUS`] `spec_delta_pending` (spec delta SD-07 `namespace_gap`). The
//!   schema claims no SET-SCOPE namespace; the spec owner decides.
//! - **Constraint.** [`SettingConstraint`] mirrors Handshake `PreferenceConstraint` Bool,
//!   IntRange, FloatRange (finite bounds) and Enum (the editor-only SyntaxColorMap and ChordMap
//!   have no Studio entry). Bounds are inclusive.
//! - **Build.** [`SettingsSchema::new`] refuses, typed ([`SchemaRefusal`]), before any entry is
//!   admitted: more entries than the declared maximum (checked first), an id outside the grammar,
//!   a module / feature field that differs from the id's segments, a duplicate id, an empty or
//!   control-character label or unit, min > max or a non-finite bound, an empty enum, an empty or
//!   duplicate enum member, and a default whose type or value is outside its own constraint.
//! - **Validate.** [`SettingsSchema::validate`] answers `{id, code}` with the Handshake codes
//!   (`not_bool`, `not_int`, `out_of_range`, `not_float`, `not_finite`, `not_string`,
//!   `unknown_enum_member`) plus `unknown_setting`; nothing is coerced or clamped. As in Handshake,
//!   an integer is a valid float value and a float is never a valid integer value.
//! - **Projection.** [`SettingsSchema::to_preference_json`] is deterministic (object keys sorted,
//!   entries in schema order) and uses the `PreferenceSchemaEntry` field names (`preference_id`,
//!   `namespace`, `scope_kind`, `label`, `constraint`, `redaction_class`, `default_value`) plus the
//!   Studio fields `module`, `feature`, `unit`, `bound_source` and `value_type`.
//! - **Tree.** [`SettingsSchema::tree`]: modules in first-appearance order, each with its features
//!   in first-appearance order, each with its entry ids (the Settings tab / subtab hierarchy).
//! - **Scope.** Typed schema only: no persistence, no registration with a host and no value store
//!   (MT-36249 phase 1: the host tab, PreferenceRecord writes and Handpick search).
#![deny(clippy::arithmetic_side_effects)]
use crate::{Json, object};

/// Handshake namespace of every Studio setting id (see the module notes: not a SET-SCOPE-001 namespace).
pub const SETTINGS_NAMESPACE: &str = "studio";
/// Status of [`SETTINGS_NAMESPACE`] against SET-SCOPE-001 (spec delta SD-07 `namespace_gap`).
pub const SETTINGS_NAMESPACE_STATUS: &str = "spec_delta_pending";
/// Declared maximum entry count of one schema (Studio limit).
pub const MAX_SETTINGS_ENTRIES: usize = 1024;
/// Largest enum domain of one entry (Studio limit).
pub const MAX_ENUM_MEMBERS: usize = 64;
/// Largest id, label, unit or enum member in bytes (the accord id bound).
pub const MAX_SETTING_TEXT_BYTES: usize = 128;

/// A setting id `studio.<module>.<feature>.<key>` (grammar in the module notes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SettingId<'a> {
    id: &'a str,
    module: &'a str,
    feature: &'a str,
    key: &'a str,
}

fn segment_ok(s: &str) -> bool {
    let b = s.as_bytes();
    matches!(b.first(), Some(b'a'..=b'z'))
        && b.last() != Some(&b'-')
        && b.iter().all(|c| matches!(c, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

impl<'a> SettingId<'a> {
    /// Parses an id; `invalid_id` outside the grammar.
    pub fn parse(id: &'a str) -> Result<Self, SettingsError> {
        if id.len() > MAX_SETTING_TEXT_BYTES {
            return Err(SettingsError::InvalidId);
        }
        let mut parts = id.split('.');
        let (Some(ns), Some(module), Some(feature), Some(key), None) =
            (parts.next(), parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(SettingsError::InvalidId);
        };
        if ns != SETTINGS_NAMESPACE || ![module, feature, key].iter().all(|s| segment_ok(s)) {
            return Err(SettingsError::InvalidId);
        }
        Ok(Self { id, module, feature, key })
    }
    pub fn as_str(&self) -> &'a str {
        self.id
    }
    pub fn module(&self) -> &'a str {
        self.module
    }
    pub fn feature(&self) -> &'a str {
        self.feature
    }
    pub fn key(&self) -> &'a str {
        self.key
    }
}

/// Typed constraint (Handshake `PreferenceConstraint` subset; inclusive bounds).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SettingConstraint {
    Bool,
    IntRange { min: i64, max: i64 },
    /// Finite bounds only.
    FloatRange { min: f64, max: f64 },
    Enum(&'static [&'static str]),
}

impl SettingConstraint {
    /// The Handshake `PreferenceValueType` string.
    pub const fn value_type(&self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::IntRange { .. } => "int",
            Self::FloatRange { .. } => "float",
            Self::Enum(_) => "enum",
        }
    }

    fn to_json(self) -> Json {
        match self {
            Self::Bool => object([("kind", Json::String("bool".into()))]),
            Self::IntRange { min, max } => object([
                ("kind", Json::String("int-range".into())),
                ("min", Json::Number(min.to_string())),
                ("max", Json::Number(max.to_string())),
            ]),
            Self::FloatRange { min, max } => object([
                ("kind", Json::String("float-range".into())),
                ("min", Json::Number(min.to_string())),
                ("max", Json::Number(max.to_string())),
            ]),
            Self::Enum(members) => object([
                ("kind", Json::String("enum".into())),
                ("members", Json::Array(members.iter().map(|m| Json::String((*m).into())).collect())),
            ]),
        }
    }
}

/// A setting value (the JSON value kinds the constraints read).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SettingValue<'a> {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(&'a str),
}

impl SettingValue<'_> {
    fn to_json(self) -> Json {
        match self {
            Self::Bool(v) => Json::Bool(v),
            Self::Int(v) => Json::Number(v.to_string()),
            // Schema defaults are finite (refused otherwise), so Display is a JSON number.
            Self::Float(v) => Json::Number(v.to_string()),
            Self::Str(v) => Json::String(v.into()),
        }
    }
}

/// Handshake `PreferenceScopeKind` (resolution surface over workspace over global over default).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingScope {
    Global,
    Workspace,
    Surface,
}

impl SettingScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Workspace => "workspace",
            Self::Surface => "surface",
        }
    }
}

/// Handshake `RedactionClass`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Redaction {
    Public,
    NonPublic,
}

impl Redaction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::NonPublic => "non-public",
        }
    }
}

/// Provenance of an entry's constraint: `Spec` only when every bound or member comes from a spec
/// clause; `StudioDeclared` when any bound is a Studio reading (always inside the engine domain).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundSource {
    Spec,
    StudioDeclared,
}

impl BoundSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Spec => "spec",
            Self::StudioDeclared => "studio_declared",
        }
    }
}

/// One schema entry (Handshake `PreferenceSchemaEntry` plus the Studio module / feature / unit /
/// bound provenance). `const`-constructible so modules declare their lists beside their option types.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SettingEntry {
    /// `studio.<module>.<feature>.<key>`.
    pub id: &'static str,
    /// Must equal the id's module segment.
    pub module: &'static str,
    /// Must equal the id's feature segment.
    pub feature: &'static str,
    pub label: &'static str,
    pub unit: Option<&'static str>,
    pub constraint: SettingConstraint,
    pub default: SettingValue<'static>,
    pub scope: SettingScope,
    pub redaction: Redaction,
    pub bound_source: BoundSource,
}

/// Typed schema refusal and validation codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsError {
    // Build refusals.
    TooManyEntries,
    InvalidId,
    IdMismatch,
    DuplicateId,
    InvalidLabel,
    InvalidUnit,
    InvalidRange,
    EmptyEnum,
    InvalidEnumMember,
    DuplicateEnumMember,
    InvalidDefault,
    // Validation codes (Handshake `validate` plus `unknown_setting`).
    UnknownSetting,
    NotBool,
    NotInt,
    NotFloat,
    NotFinite,
    NotString,
    OutOfRange,
    UnknownEnumMember,
}

impl SettingsError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::TooManyEntries => "too_many_entries",
            Self::InvalidId => "invalid_id",
            Self::IdMismatch => "id_mismatch",
            Self::DuplicateId => "duplicate_id",
            Self::InvalidLabel => "invalid_label",
            Self::InvalidUnit => "invalid_unit",
            Self::InvalidRange => "invalid_range",
            Self::EmptyEnum => "empty_enum",
            Self::InvalidEnumMember => "invalid_enum_member",
            Self::DuplicateEnumMember => "duplicate_enum_member",
            Self::InvalidDefault => "invalid_default",
            Self::UnknownSetting => "unknown_setting",
            Self::NotBool => "not_bool",
            Self::NotInt => "not_int",
            Self::NotFloat => "not_float",
            Self::NotFinite => "not_finite",
            Self::NotString => "not_string",
            Self::OutOfRange => "out_of_range",
            Self::UnknownEnumMember => "unknown_enum_member",
        }
    }
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for SettingsError {}

/// A build refusal: the offending entry's id as declared (`None` for the entry-count limit).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchemaRefusal {
    pub id: Option<&'static str>,
    pub error: SettingsError,
}

/// A validation failure: `{id, code}` (Handshake `PreferenceValidationError` without its text).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingRejection<'a> {
    pub id: &'a str,
    pub error: SettingsError,
}

impl SettingRejection<'_> {
    pub const fn code(&self) -> &'static str {
        self.error.code()
    }
}

/// One feature subtab of the tree: its entry ids in schema order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsFeature {
    pub feature: &'static str,
    pub entries: Vec<&'static str>,
}

/// One module tab of the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsModule {
    pub module: &'static str,
    pub features: Vec<SettingsFeature>,
}

fn text_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_SETTING_TEXT_BYTES && !s.chars().any(char::is_control)
}

fn float_in(v: f64, min: f64, max: f64) -> Result<(), SettingsError> {
    if !v.is_finite() {
        Err(SettingsError::NotFinite)
    } else if !(min..=max).contains(&v) {
        Err(SettingsError::OutOfRange)
    } else {
        Ok(())
    }
}

fn check(constraint: &SettingConstraint, value: &SettingValue<'_>) -> Result<(), SettingsError> {
    match (constraint, value) {
        (SettingConstraint::Bool, SettingValue::Bool(_)) => Ok(()),
        (SettingConstraint::Bool, _) => Err(SettingsError::NotBool),
        (SettingConstraint::IntRange { min, max }, SettingValue::Int(v)) => {
            if (*min..=*max).contains(v) { Ok(()) } else { Err(SettingsError::OutOfRange) }
        }
        (SettingConstraint::IntRange { .. }, _) => Err(SettingsError::NotInt),
        (SettingConstraint::FloatRange { min, max }, SettingValue::Float(v)) => float_in(*v, *min, *max),
        // Handshake reads an integer JSON value as a float (`as_f64`).
        (SettingConstraint::FloatRange { min, max }, SettingValue::Int(v)) => float_in(*v as f64, *min, *max),
        (SettingConstraint::FloatRange { .. }, _) => Err(SettingsError::NotFloat),
        (SettingConstraint::Enum(members), SettingValue::Str(s)) => {
            if members.contains(s) { Ok(()) } else { Err(SettingsError::UnknownEnumMember) }
        }
        (SettingConstraint::Enum(_), _) => Err(SettingsError::NotString),
    }
}

fn check_constraint(constraint: &SettingConstraint) -> Result<(), SettingsError> {
    match constraint {
        SettingConstraint::Bool => Ok(()),
        SettingConstraint::IntRange { min, max } => {
            if min > max { Err(SettingsError::InvalidRange) } else { Ok(()) }
        }
        SettingConstraint::FloatRange { min, max } => {
            if !min.is_finite() || !max.is_finite() || min > max {
                Err(SettingsError::InvalidRange)
            } else {
                Ok(())
            }
        }
        SettingConstraint::Enum(members) => {
            if members.is_empty() {
                return Err(SettingsError::EmptyEnum);
            }
            if members.len() > MAX_ENUM_MEMBERS || !members.iter().all(|m| text_ok(m)) {
                return Err(SettingsError::InvalidEnumMember);
            }
            for (i, m) in members.iter().enumerate() {
                if members.iter().take(i).any(|p| p == m) {
                    return Err(SettingsError::DuplicateEnumMember);
                }
            }
            Ok(())
        }
    }
}

fn check_entry(e: &SettingEntry) -> Result<(), SettingsError> {
    let id = SettingId::parse(e.id)?;
    if id.module() != e.module || id.feature() != e.feature {
        return Err(SettingsError::IdMismatch);
    }
    if !text_ok(e.label) {
        return Err(SettingsError::InvalidLabel);
    }
    if e.unit.is_some_and(|u| !text_ok(u)) {
        return Err(SettingsError::InvalidUnit);
    }
    check_constraint(&e.constraint)?;
    // The default must be exactly the constraint's kind (no integer default for a float entry).
    let kind_ok = matches!(
        (&e.constraint, &e.default),
        (SettingConstraint::Bool, SettingValue::Bool(_))
            | (SettingConstraint::IntRange { .. }, SettingValue::Int(_))
            | (SettingConstraint::FloatRange { .. }, SettingValue::Float(_))
            | (SettingConstraint::Enum(_), SettingValue::Str(_))
    );
    if !kind_ok || check(&e.constraint, &e.default).is_err() {
        return Err(SettingsError::InvalidDefault);
    }
    Ok(())
}

/// A checked, immutable schema (entries in declaration order).
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsSchema {
    entries: Vec<SettingEntry>,
}

impl SettingsSchema {
    /// Builds a schema from entry groups under [`MAX_SETTINGS_ENTRIES`] (refusals in the module notes).
    pub fn new(groups: &[&[SettingEntry]]) -> Result<Self, SchemaRefusal> {
        Self::with_limit(groups, MAX_SETTINGS_ENTRIES)
    }

    /// As [`Self::new`] with a caller limit; a limit above [`MAX_SETTINGS_ENTRIES`] is that maximum.
    pub fn with_limit(groups: &[&[SettingEntry]], max_entries: usize) -> Result<Self, SchemaRefusal> {
        let limit = max_entries.min(MAX_SETTINGS_ENTRIES);
        let too_many = SchemaRefusal { id: None, error: SettingsError::TooManyEntries };
        let mut total = 0usize;
        for g in groups {
            total = total.checked_add(g.len()).ok_or(too_many)?;
        }
        if total > limit {
            return Err(too_many);
        }
        let mut entries: Vec<SettingEntry> = Vec::with_capacity(total);
        for e in groups.iter().flat_map(|g| g.iter()) {
            let refuse = |error| SchemaRefusal { id: Some(e.id), error };
            check_entry(e).map_err(refuse)?;
            if entries.iter().any(|p| p.id == e.id) {
                return Err(refuse(SettingsError::DuplicateId));
            }
            entries.push(*e);
        }
        Ok(Self { entries })
    }

    pub fn entries(&self) -> &[SettingEntry] {
        &self.entries
    }

    pub fn get(&self, id: &str) -> Option<&SettingEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Checks `value` against the entry `id` (codes in the module notes); never coerces.
    pub fn validate<'a>(&self, id: &'a str, value: &SettingValue<'_>) -> Result<(), SettingRejection<'a>> {
        let entry = self.get(id).ok_or(SettingRejection { id, error: SettingsError::UnknownSetting })?;
        check(&entry.constraint, value).map_err(|error| SettingRejection { id, error })
    }

    /// Module tabs and feature subtabs in first-appearance order.
    pub fn tree(&self) -> Vec<SettingsModule> {
        let mut modules: Vec<SettingsModule> = Vec::new();
        for e in &self.entries {
            let at = match modules.iter().position(|m| m.module == e.module) {
                Some(i) => i,
                None => {
                    modules.push(SettingsModule { module: e.module, features: Vec::new() });
                    modules.len().saturating_sub(1)
                }
            };
            let Some(module) = modules.get_mut(at) else { continue };
            match module.features.iter_mut().find(|f| f.feature == e.feature) {
                Some(f) => f.entries.push(e.id),
                None => module.features.push(SettingsFeature { feature: e.feature, entries: vec![e.id] }),
            }
        }
        modules
    }

    /// Deterministic projection (field names in the module notes).
    pub fn to_preference_json(&self) -> String {
        let entries = self
            .entries
            .iter()
            .map(|e| {
                object([
                    ("preference_id", Json::String(e.id.into())),
                    ("namespace", Json::String(SETTINGS_NAMESPACE.into())),
                    ("module", Json::String(e.module.into())),
                    ("feature", Json::String(e.feature.into())),
                    ("scope_kind", Json::String(e.scope.as_str().into())),
                    ("label", Json::String(e.label.into())),
                    ("unit", e.unit.map_or(Json::Null, |u| Json::String(u.into()))),
                    ("value_type", Json::String(e.constraint.value_type().into())),
                    ("constraint", e.constraint.to_json()),
                    ("redaction_class", Json::String(e.redaction.as_str().into())),
                    ("default_value", e.default.to_json()),
                    ("bound_source", Json::String(e.bound_source.as_str().into())),
                ])
            })
            .collect();
        object([
            ("namespace", Json::String(SETTINGS_NAMESPACE.into())),
            ("namespace_status", Json::String(SETTINGS_NAMESPACE_STATUS.into())),
            ("entries", Json::Array(entries)),
        ])
        .encode()
    }
}
