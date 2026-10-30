//! Recorded automation records (lane SMB-PS-ACTIONS-1): action sets, actions and steps
//! ([STU-AUT-102], [STU-AUT-103], [STU-AUT-104]), the closed dialog-option, value-type and unit
//! vocabularies ([STU-AUT-105], [STU-AUT-106], [STU-AUT-107]), the strict portable JSON artifact
//! ([STU-AUT-114]), the automation validators ([STU-AUT-181]..[STU-AUT-185]) and the command
//! corpus of the harness [`crate::Run`] ([STU-AUT-147] subset, [`RUN_COMMANDS`]).
//!
//! A recorded step names a corpus `command_id` and its resolved typed parameters; playback
//! ([`crate::Run::play_action`]) calls the same public `Run` method the original edit used
//! ([STU-AUT-109], [STU-RAS-169]: no parallel vocabulary, no translation layer).
//!
//! The codec reads and writes `serde_json::Value` (harness-core's only approved serialization
//! dependency; no serde derive). Decoding is strict: an unknown, duplicate or missing field, a value
//! outside its closed domain, and an artifact over its byte or count budget are typed refusals
//! ([`DecodeError`]); tests `tests/action_records.rs`.
//!
//! Naming: Folio's public `StudioAction` (prototype reactions) is a different record; these records
//! are [`ActionSet`], [`ActionRecord`] and [`ActionStep`].
//!
//! Gaps (NOT_PROVEN, declared in the tests): no `schemars` input schema per command (schemars is
//! not an approved harness-core dependency), no `SACT` identity prefix in Accord (an `action_id` is
//! validated as text here), the [STU-AUT-111] step kinds, event-bound actions ([STU-AUT-112]),
//! conditional predicates ([STU-AUT-113]), Folio persistence of action sets and a GUI actions panel.
#![deny(clippy::arithmetic_side_effects)]

use crate::harness::{Place, PlacementWrite};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Largest portable artifact (action set or batch job) the decoder reads, checked before parsing.
pub const MAX_ARTIFACT_BYTES: usize = 16_777_216;
/// Most actions in one set.
pub const MAX_ACTIONS: usize = 4096;
/// Most steps in one action (the shipped maximum is 26, [STU-AUT-108]).
pub const MAX_STEPS_PER_ACTION: usize = 4096;
/// Most steps across all actions of one set (aggregate budget; the shipped library has 1,543).
pub const MAX_STEPS_PER_SET: usize = 16_384;
/// Most parameters of one step.
pub const MAX_PARAMETERS: usize = 1024;
/// Deepest `list` nesting of a recorded value.
pub const MAX_LIST_DEPTH: usize = 8;
/// Most items of one `list` value.
pub const MAX_LIST_ITEMS: usize = 65_536;
/// Longest text field (bytes).
pub const MAX_TEXT_BYTES: usize = 65_536;

// ----- closed vocabularies ---------------------------------------------------------------------

/// [STU-AUT-105] dialog options: closed, four members, wire value 0..=3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogOption {
    /// 0: never show a dialog, even in interactive playback.
    Silent,
    /// 1: show the dialog only when the step's `with_dialog` flag is set.
    ShowIfConfigured,
    /// 2: show in interactive playback, suppress in batch (headless) playback.
    ShowInInteractivePlayback,
    /// 3: always show, including inside a batch job (a headless job refuses to start).
    AlwaysShow,
}

impl DialogOption {
    pub const ALL: [DialogOption; 4] = [Self::Silent, Self::ShowIfConfigured, Self::ShowInInteractivePlayback, Self::AlwaysShow];

    pub const fn value(self) -> u8 {
        match self {
            Self::Silent => 0,
            Self::ShowIfConfigured => 1,
            Self::ShowInInteractivePlayback => 2,
            Self::AlwaysShow => 3,
        }
    }

    pub const fn token(self) -> &'static str {
        match self {
            Self::Silent => "silent",
            Self::ShowIfConfigured => "show_if_configured",
            Self::ShowInInteractivePlayback => "show_in_interactive_playback",
            Self::AlwaysShow => "always_show",
        }
    }

    pub fn from_value(v: u64) -> Option<Self> {
        Self::ALL.into_iter().find(|d| u64::from(d.value()) == v)
    }
}

/// [STU-AUT-107] unit tokens of a `measured` value: closed, six members.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitToken {
    Percent,
    Pixels,
    Relative,
    AngleDegrees,
    Points,
    ResolutionDpi,
}

impl UnitToken {
    pub const ALL: [UnitToken; 6] = [Self::Percent, Self::Pixels, Self::Relative, Self::AngleDegrees, Self::Points, Self::ResolutionDpi];

    pub const fn token(self) -> &'static str {
        match self {
            Self::Percent => "percent",
            Self::Pixels => "pixels",
            Self::Relative => "relative",
            Self::AngleDegrees => "angle_degrees",
            Self::Points => "points",
            Self::ResolutionDpi => "resolution_dpi",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|u| u.token() == s)
    }
}

/// [STU-AUT-106] recorded value types. The clause heading says "seven members" while its table
/// has eight rows; all eight rows are implemented and `string` and `list` stay distinct
/// (spec-delta request in the lane notes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueType {
    Reference,
    Class,
    Enumerated,
    Measured,
    Number,
    Boolean,
    String,
    List,
}

impl ValueType {
    pub const ALL: [ValueType; 8] = [Self::Reference, Self::Class, Self::Enumerated, Self::Measured, Self::Number, Self::Boolean, Self::String, Self::List];

    pub const fn token(self) -> &'static str {
        match self {
            Self::Reference => "reference",
            Self::Class => "class",
            Self::Enumerated => "enumerated",
            Self::Measured => "measured",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::String => "string",
            Self::List => "list",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.token() == s)
    }
}

/// One recorded parameter value ([STU-AUT-106]). `measured` and `number` are different types.
#[derive(Clone, Debug, PartialEq)]
pub enum RecordedValue {
    /// A document object by stable prefixed id ([STU-AUT-008]; validated by Accord `DomainId`).
    Reference(String),
    /// A type token.
    Class(String),
    /// A two-part `family.member` token.
    Enumerated(String),
    /// A number plus a unit token. `unit: None` is only ever the decoded form of a record that
    /// lacks its unit; the validator rejects it ([STU-AUT-185]).
    Measured { value: f64, unit: Option<UnitToken> },
    /// A bare number with no declared unit.
    Number(f64),
    Boolean(bool),
    String(String),
    /// An ordered array of any of the above.
    List(Vec<RecordedValue>),
}

impl RecordedValue {
    pub fn value_type(&self) -> ValueType {
        match self {
            Self::Reference(_) => ValueType::Reference,
            Self::Class(_) => ValueType::Class,
            Self::Enumerated(_) => ValueType::Enumerated,
            Self::Measured { .. } => ValueType::Measured,
            Self::Number(_) => ValueType::Number,
            Self::Boolean(_) => ValueType::Boolean,
            Self::String(_) => ValueType::String,
            Self::List(_) => ValueType::List,
        }
    }

    pub fn to_json(&self) -> Value {
        let t = self.value_type().token();
        match self {
            Self::Reference(s) | Self::Class(s) | Self::Enumerated(s) | Self::String(s) => json!({"type": t, "value": s}),
            Self::Measured { value, unit: Some(u) } => json!({"type": t, "value": value, "unit": u.token()}),
            Self::Measured { value, unit: None } => json!({"type": t, "value": value}),
            Self::Number(n) => json!({"type": t, "value": n}),
            Self::Boolean(b) => json!({"type": t, "value": b}),
            Self::List(items) => json!({"type": t, "value": items.iter().map(Self::to_json).collect::<Vec<_>>()}),
        }
    }

    fn from_json(v: &Value, path: &str, depth: usize) -> Result<Self, DecodeError> {
        let map = v.as_object().ok_or_else(|| DecodeError::new("field_type", path))?;
        let ty = match map.get("type") {
            None => return Err(DecodeError::new("field_missing", format!("{path}/type"))),
            Some(t) => t.as_str().ok_or_else(|| DecodeError::new("field_type", format!("{path}/type")))?,
        };
        let ty = ValueType::parse(ty).ok_or_else(|| DecodeError::new("value_type_unknown", format!("{path}/type")))?;
        let allowed: &[&str] = if ty == ValueType::Measured { &["type", "value", "unit"] } else { &["type", "value"] };
        let o = Obj::new(v, path, allowed)?;
        Ok(match ty {
            ValueType::Reference => {
                let s = o.text("value")?;
                hsk_studio_accord::DomainId::parse(&s).map_err(|_| DecodeError::new("reference_malformed", format!("{path}/value")))?;
                Self::Reference(s)
            }
            ValueType::Class => Self::Class(o.text("value")?),
            ValueType::Enumerated => {
                let s = o.text("value")?;
                if !enumerated_token_ok(&s) {
                    return Err(DecodeError::new("enumerated_token_malformed", format!("{path}/value")));
                }
                Self::Enumerated(s)
            }
            ValueType::Measured => {
                let value = o.number("value")?;
                let unit = match map.get("unit") {
                    None => None,
                    Some(u) => {
                        let s = u.as_str().ok_or_else(|| DecodeError::new("field_type", format!("{path}/unit")))?;
                        Some(UnitToken::parse(s).ok_or_else(|| DecodeError::new("unit_token_unknown", format!("{path}/unit")))?)
                    }
                };
                Self::Measured { value, unit }
            }
            ValueType::Number => Self::Number(o.number("value")?),
            ValueType::Boolean => Self::Boolean(o.boolean("value")?),
            ValueType::String => Self::String(o.text("value")?),
            ValueType::List => {
                if depth >= MAX_LIST_DEPTH {
                    return Err(DecodeError::new("list_too_deep", path));
                }
                let items = o.array("value", MAX_LIST_ITEMS)?;
                let mut out = Vec::with_capacity(items.len());
                for (i, item) in items.iter().enumerate() {
                    out.push(Self::from_json(item, &format!("{path}/value/{i}"), depth.saturating_add(1))?);
                }
                Self::List(out)
            }
        })
    }

    /// Every `measured` value without a unit, by its position path below this value.
    fn missing_units(&self, at: &str, out: &mut Vec<String>) {
        match self {
            Self::Measured { unit: None, .. } => out.push(at.to_string()),
            Self::List(items) => {
                for (i, item) in items.iter().enumerate() {
                    item.missing_units(&format!("{at}[{i}]"), out);
                }
            }
            _ => {}
        }
    }
}

/// `family.member`: exactly one dot, both parts non-empty lowercase ASCII letters, digits or `_`.
fn enumerated_token_ok(s: &str) -> bool {
    let part = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    match s.split_once('.') {
        Some((family, member)) => part(family) && part(member),
        None => false,
    }
}

/// `SACT-{uuid_v7}` checked as text (Accord has no `SACT` identity prefix: declared gap).
pub fn action_id_ok(id: &str) -> bool {
    let Some(uuid) = id.strip_prefix("SACT-") else {
        return false;
    };
    let b = uuid.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| if [8, 13, 18, 23].contains(&i) { *c == b'-' } else { c.is_ascii_hexdigit() })
        && b.get(14) == Some(&b'7')
        && matches!(b.get(19), Some(b'8' | b'9' | b'a' | b'b' | b'A' | b'B'))
}

// ----- records ---------------------------------------------------------------------------------

/// [STU-AUT-104] one recorded step (ten fields). A step whose `command_id` resolves to no corpus
/// entry is kept and reported unresolvable ([STU-AUT-183], [STU-AUT-114]); it round-trips unchanged.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionStep {
    pub index: u64,
    /// A disabled step is skipped on playback but kept in the record.
    pub enabled: bool,
    pub expanded: bool,
    pub with_dialog: bool,
    pub dialog_options: DialogOption,
    pub command_id: String,
    pub display_name: String,
    pub has_parameters: bool,
    pub parameter_class: Option<String>,
    pub parameters: Option<BTreeMap<String, RecordedValue>>,
}

impl ActionStep {
    const FIELDS: [&'static str; 10] =
        ["index", "enabled", "expanded", "with_dialog", "dialog_options", "command_id", "display_name", "has_parameters", "parameter_class", "parameters"];

    /// The corpus entry this step names (`None`: unresolvable).
    pub fn command(&self) -> Option<&'static CommandEntry> {
        command(&self.command_id)
    }

    pub fn to_json(&self) -> Value {
        let parameters = self.parameters.as_ref().map_or(Value::Null, |p| {
            Value::Object(p.iter().map(|(k, v)| (k.clone(), v.to_json())).collect::<Map<String, Value>>())
        });
        json!({
            "index": self.index, "enabled": self.enabled, "expanded": self.expanded, "with_dialog": self.with_dialog,
            "dialog_options": self.dialog_options.value(), "command_id": self.command_id, "display_name": self.display_name,
            "has_parameters": self.has_parameters, "parameter_class": self.parameter_class, "parameters": parameters,
        })
    }

    pub(crate) fn from_json(v: &Value, path: &str) -> Result<Self, DecodeError> {
        let o = Obj::new(v, path, &Self::FIELDS)?;
        let dialog = o.uint("dialog_options")?;
        let dialog_options = DialogOption::from_value(dialog).ok_or_else(|| DecodeError::new("dialog_options_out_of_domain", format!("{path}/dialog_options")))?;
        let parameters = match o.get("parameters")? {
            Value::Null => None,
            Value::Object(map) => {
                if map.len() > MAX_PARAMETERS {
                    return Err(DecodeError::new("too_many_parameters", format!("{path}/parameters")));
                }
                let mut out = BTreeMap::new();
                for (k, pv) in map {
                    if k.len() > MAX_TEXT_BYTES {
                        return Err(DecodeError::new("text_too_long", format!("{path}/parameters")));
                    }
                    out.insert(k.clone(), RecordedValue::from_json(pv, &format!("{path}/parameters/{k}"), 0)?);
                }
                Some(out)
            }
            _ => return Err(DecodeError::new("field_type", format!("{path}/parameters"))),
        };
        Ok(Self {
            index: o.uint("index")?,
            enabled: o.boolean("enabled")?,
            expanded: o.boolean("expanded")?,
            with_dialog: o.boolean("with_dialog")?,
            dialog_options,
            command_id: o.text("command_id")?,
            display_name: o.text("display_name")?,
            has_parameters: o.boolean("has_parameters")?,
            parameter_class: o.opt_text("parameter_class")?,
            parameters,
        })
    }
}

/// [STU-AUT-103] one action (ten fields): `function_key_index` 0..=12 (0 = none, 1..=12 = F1..F12),
/// `color_index` 0..=7 (0 = none); `declared_step_count` must equal `steps.len()` ([STU-AUT-181]).
#[derive(Clone, Debug, PartialEq)]
pub struct ActionRecord {
    /// `SACT-{uuid_v7}`, supplied by the caller ([`action_id_ok`]).
    pub action_id: String,
    pub index: u64,
    pub name: String,
    pub function_key_index: u8,
    pub shift_modifier: bool,
    pub command_modifier: bool,
    pub color_index: u8,
    pub expanded: bool,
    pub declared_step_count: u64,
    pub steps: Vec<ActionStep>,
}

impl ActionRecord {
    const FIELDS: [&'static str; 10] =
        ["action_id", "index", "name", "function_key_index", "shift_modifier", "command_modifier", "color_index", "expanded", "declared_step_count", "steps"];

    pub fn to_json(&self) -> Value {
        json!({
            "action_id": self.action_id, "index": self.index, "name": self.name,
            "function_key_index": self.function_key_index, "shift_modifier": self.shift_modifier,
            "command_modifier": self.command_modifier, "color_index": self.color_index, "expanded": self.expanded,
            "declared_step_count": self.declared_step_count,
            "steps": self.steps.iter().map(ActionStep::to_json).collect::<Vec<_>>(),
        })
    }

    pub(crate) fn from_json(v: &Value, path: &str, steps_budget: &mut usize) -> Result<Self, DecodeError> {
        let o = Obj::new(v, path, &Self::FIELDS)?;
        let action_id = o.text("action_id")?;
        if !action_id_ok(&action_id) {
            return Err(DecodeError::new("action_id_malformed", format!("{path}/action_id")));
        }
        let fk = o.uint("function_key_index")?;
        let function_key_index = u8::try_from(fk).ok().filter(|k| *k <= 12).ok_or_else(|| DecodeError::new("function_key_index_out_of_domain", format!("{path}/function_key_index")))?;
        let color = o.uint("color_index")?;
        let color_index = u8::try_from(color).ok().filter(|c| *c <= 7).ok_or_else(|| DecodeError::new("color_index_out_of_domain", format!("{path}/color_index")))?;
        let raw_steps = o.array("steps", MAX_STEPS_PER_ACTION).map_err(|e| if e.code == "list_too_long" { DecodeError::new("too_many_steps", e.path) } else { e })?;
        *steps_budget = steps_budget.checked_sub(raw_steps.len()).ok_or_else(|| DecodeError::new("steps_over_budget", format!("{path}/steps")))?;
        let mut steps = Vec::with_capacity(raw_steps.len());
        for (i, s) in raw_steps.iter().enumerate() {
            steps.push(ActionStep::from_json(s, &format!("{path}/steps/{i}"))?);
        }
        Ok(Self {
            action_id,
            index: o.uint("index")?,
            name: o.text("name")?,
            function_key_index,
            shift_modifier: o.boolean("shift_modifier")?,
            command_modifier: o.boolean("command_modifier")?,
            color_index,
            expanded: o.boolean("expanded")?,
            declared_step_count: o.uint("declared_step_count")?,
            steps,
        })
    }

    /// The `(function key, shift, command)` binding, `None` without a function key.
    pub fn binding(&self) -> Option<(u8, bool, bool)> {
        (self.function_key_index != 0).then_some((self.function_key_index, self.shift_modifier, self.command_modifier))
    }
}

/// [STU-AUT-102] action set: `{action_set_id, name, actions[]}`, actions ordered. The unit of
/// import and export ([`ActionSet::export`], [`ActionSet::import`]).
#[derive(Clone, Debug, PartialEq)]
pub struct ActionSet {
    pub action_set_id: String,
    pub name: String,
    pub actions: Vec<ActionRecord>,
}

impl ActionSet {
    const FIELDS: [&'static str; 3] = ["action_set_id", "name", "actions"];

    pub fn to_json(&self) -> Value {
        json!({"action_set_id": self.action_set_id, "name": self.name, "actions": self.actions.iter().map(ActionRecord::to_json).collect::<Vec<_>>()})
    }

    /// The portable artifact: compact JSON of [`ActionSet::to_json`]. Export of an imported set is
    /// byte-identical to the artifact it came from when that artifact was itself an export (test
    /// `export_then_import_is_byte_identical`).
    pub fn export(&self) -> Vec<u8> {
        serde_json::to_vec(&self.to_json()).unwrap_or_default()
    }

    /// Strict decode only (no validators).
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let v = parse_strict(bytes)?;
        Self::from_json(&v)
    }

    pub(crate) fn from_json(v: &Value) -> Result<Self, DecodeError> {
        let o = Obj::new(v, "", &Self::FIELDS)?;
        let action_set_id = o.text("action_set_id")?;
        if action_set_id.is_empty() {
            return Err(DecodeError::new("field_type", "/action_set_id"));
        }
        let raw = o.array("actions", MAX_ACTIONS).map_err(|e| if e.code == "list_too_long" { DecodeError::new("too_many_actions", e.path) } else { e })?;
        let mut budget = MAX_STEPS_PER_SET;
        let mut actions = Vec::with_capacity(raw.len());
        for (i, a) in raw.iter().enumerate() {
            actions.push(ActionRecord::from_json(a, &format!("/actions/{i}"), &mut budget)?);
        }
        Ok(Self { action_set_id, name: o.text("name")?, actions })
    }

    /// Import ([STU-AUT-114]): strict decode, then the validators. The load fails on
    /// `action_declared_step_count_mismatch` ([STU-AUT-181]) and `action_measured_value_missing_unit`
    /// ([STU-AUT-185]). A step with an unresolvable command is kept, listed in
    /// [`ImportedSet::unresolvable`] and reported ([STU-AUT-183]); collisions ([STU-AUT-182]) and
    /// unknown parameter keys ([STU-AUT-184], warning) are reported (Studio reading: a collision is
    /// a registration error among enabled sets, so it does not fail the load of one set).
    pub fn import(bytes: &[u8]) -> Result<ImportedSet, ImportError> {
        let set = Self::decode(bytes).map_err(ImportError::Decode)?;
        let diagnostics = validate_set(&set);
        if diagnostics.iter().any(|d| LOAD_FAILING.contains(&d.code)) {
            return Err(ImportError::Rejected(diagnostics));
        }
        let unresolvable = set
            .actions
            .iter()
            .flat_map(|a| a.steps.iter().filter(|s| s.command().is_none()).map(|s| (a.action_id.clone(), s.index, s.command_id.clone())))
            .collect();
        Ok(ImportedSet { set, diagnostics, unresolvable })
    }
}

/// Diagnostics that fail an import.
const LOAD_FAILING: [&str; 2] = [CODE_STEP_COUNT_MISMATCH, CODE_MEASURED_MISSING_UNIT];

/// A set that imported, with every diagnostic and the steps marked unresolvable
/// `(action_id, step index, command_id)`.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedSet {
    pub set: ActionSet,
    pub diagnostics: Vec<Diagnostic>,
    pub unresolvable: Vec<(String, u64, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ImportError {
    Decode(DecodeError),
    Rejected(Vec<Diagnostic>),
}

impl ImportError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Decode(e) => e.code,
            Self::Rejected(d) => d.iter().find(|d| LOAD_FAILING.contains(&d.code)).map_or("action_set_rejected", |d| d.code),
        }
    }
}

// ----- strict decoding -------------------------------------------------------------------------

/// A typed decode refusal: the stable `code` and the JSON path of the offending field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError {
    pub code: &'static str,
    pub path: String,
}

impl DecodeError {
    pub(crate) fn new(code: &'static str, path: impl Into<String>) -> Self {
        Self { code, path: path.into() }
    }
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at {}", self.code, if self.path.is_empty() { "/" } else { &self.path })
    }
}

impl std::error::Error for DecodeError {}

/// Parses an artifact: byte budget first, then JSON, then a duplicate-key scan (a `Value` keeps
/// only the last of duplicate keys, so duplicates are found on the raw bytes).
pub(crate) fn parse_strict(bytes: &[u8]) -> Result<Value, DecodeError> {
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(DecodeError::new("artifact_too_large", ""));
    }
    let v: Value = serde_json::from_slice(bytes).map_err(|_| DecodeError::new("artifact_not_json", ""))?;
    scan_duplicate_keys(bytes)?;
    Ok(v)
}

/// Walks syntactically valid JSON and refuses an object that names a key twice (keys compared
/// after unescaping). Runs only after `serde_json` accepted the bytes.
fn scan_duplicate_keys(bytes: &[u8]) -> Result<(), DecodeError> {
    let mut stack: Vec<Option<BTreeSet<String>>> = Vec::new();
    let mut expect_key = false;
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        match b {
            b'{' => {
                stack.push(Some(BTreeSet::new()));
                expect_key = true;
            }
            b'[' => {
                stack.push(None);
                expect_key = false;
            }
            b'}' | b']' => {
                stack.pop();
                expect_key = false;
            }
            b',' => expect_key = matches!(stack.last(), Some(Some(_))),
            b':' => expect_key = false,
            b'"' => {
                let start = i;
                let mut j = i.saturating_add(1);
                loop {
                    match bytes.get(j) {
                        Some(b'\\') => j = j.saturating_add(2),
                        Some(b'"') | None => break,
                        Some(_) => j = j.saturating_add(1),
                    }
                }
                if expect_key {
                    let raw = bytes.get(start..=j).ok_or_else(|| DecodeError::new("artifact_not_json", ""))?;
                    let key: String = serde_json::from_slice(raw).map_err(|_| DecodeError::new("artifact_not_json", ""))?;
                    if let Some(Some(keys)) = stack.last_mut()
                        && !keys.insert(key.clone())
                    {
                        return Err(DecodeError::new("field_duplicate", format!("{key} (byte {start})")));
                    }
                }
                i = j;
            }
            _ => {}
        }
        i = i.saturating_add(1);
    }
    Ok(())
}

/// A JSON object whose keys were checked against an allowed list.
pub(crate) struct Obj<'a> {
    map: &'a Map<String, Value>,
    path: String,
}

impl<'a> Obj<'a> {
    pub(crate) fn new(v: &'a Value, path: &str, allowed: &[&str]) -> Result<Self, DecodeError> {
        let map = v.as_object().ok_or_else(|| DecodeError::new("field_type", path))?;
        if let Some(k) = map.keys().find(|k| !allowed.contains(&k.as_str())) {
            return Err(DecodeError::new("field_unknown", format!("{path}/{k}")));
        }
        Ok(Self { map, path: path.to_string() })
    }

    fn at(&self, k: &str) -> String {
        format!("{}/{k}", self.path)
    }

    pub(crate) fn get(&self, k: &str) -> Result<&'a Value, DecodeError> {
        self.map.get(k).ok_or_else(|| DecodeError::new("field_missing", self.at(k)))
    }

    pub(crate) fn text(&self, k: &str) -> Result<String, DecodeError> {
        let s = self.get(k)?.as_str().ok_or_else(|| DecodeError::new("field_type", self.at(k)))?;
        if s.len() > MAX_TEXT_BYTES {
            return Err(DecodeError::new("text_too_long", self.at(k)));
        }
        Ok(s.to_string())
    }

    pub(crate) fn opt_text(&self, k: &str) -> Result<Option<String>, DecodeError> {
        match self.get(k)? {
            Value::Null => Ok(None),
            _ => self.text(k).map(Some),
        }
    }

    pub(crate) fn boolean(&self, k: &str) -> Result<bool, DecodeError> {
        self.get(k)?.as_bool().ok_or_else(|| DecodeError::new("field_type", self.at(k)))
    }

    pub(crate) fn uint(&self, k: &str) -> Result<u64, DecodeError> {
        self.get(k)?.as_u64().ok_or_else(|| DecodeError::new("field_type", self.at(k)))
    }

    pub(crate) fn number(&self, k: &str) -> Result<f64, DecodeError> {
        self.get(k)?.as_f64().ok_or_else(|| DecodeError::new("field_type", self.at(k)))
    }

    pub(crate) fn array(&self, k: &str, max: usize) -> Result<&'a Vec<Value>, DecodeError> {
        let a = self.get(k)?.as_array().ok_or_else(|| DecodeError::new("field_type", self.at(k)))?;
        if a.len() > max {
            return Err(DecodeError::new("list_too_long", self.at(k)));
        }
        Ok(a)
    }
}

// ----- validators ------------------------------------------------------------------------------

pub const CODE_STEP_COUNT_MISMATCH: &str = "action_declared_step_count_mismatch";
pub const CODE_SHORTCUT_COLLISION: &str = "action_shortcut_collision";
pub const CODE_COMMAND_UNRESOLVABLE: &str = "action_step_command_unresolvable";
pub const CODE_PARAMETER_KEY_UNKNOWN: &str = "action_step_parameter_key_unknown";
pub const CODE_MEASURED_MISSING_UNIT: &str = "action_measured_value_missing_unit";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

/// One validator finding: the clause code, its clause, severity and the fields the clause says the
/// diagnostic must name.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub clause: &'static str,
    pub severity: Severity,
    pub fields: Value,
}

impl Diagnostic {
    pub fn to_json(&self) -> Value {
        json!({"code": self.code, "clause": self.clause, "severity": self.severity.token(), "fields": self.fields})
    }
}

/// [STU-AUT-181], [STU-AUT-183], [STU-AUT-184], [STU-AUT-185] over one action.
pub fn validate_action(action: &ActionRecord) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let length = u64::try_from(action.steps.len()).unwrap_or(u64::MAX);
    if action.declared_step_count != length {
        out.push(Diagnostic {
            code: CODE_STEP_COUNT_MISMATCH,
            clause: "STU-AUT-181",
            severity: Severity::Error,
            fields: json!({"action_id": action.action_id, "declared_step_count": action.declared_step_count, "steps_length": length}),
        });
    }
    validate_steps(&action.action_id, &action.steps, &mut out);
    out
}

/// The step checks of [`validate_action`] for an owner id (an action, or a batch job's inline
/// command sequence).
pub(crate) fn validate_steps(owner: &str, steps: &[ActionStep], out: &mut Vec<Diagnostic>) {
    for step in steps {
        let entry = step.command();
        if entry.is_none() {
            out.push(Diagnostic {
                code: CODE_COMMAND_UNRESOLVABLE,
                clause: "STU-AUT-183",
                severity: Severity::Error,
                fields: json!({"action_id": owner, "step_index": step.index, "command_id": step.command_id}),
            });
        }
        for (key, value) in step.parameters.iter().flatten() {
            if let Some(entry) = entry
                && !entry.parameters.iter().any(|p| p.key == key)
            {
                out.push(Diagnostic {
                    code: CODE_PARAMETER_KEY_UNKNOWN,
                    clause: "STU-AUT-184",
                    severity: Severity::Warning,
                    fields: json!({"action_id": owner, "step_index": step.index, "key": key, "command_id": step.command_id}),
                });
            }
            let mut missing = Vec::new();
            value.missing_units(key, &mut missing);
            for key in missing {
                out.push(Diagnostic {
                    code: CODE_MEASURED_MISSING_UNIT,
                    clause: "STU-AUT-185",
                    severity: Severity::Error,
                    fields: json!({"action_id": owner, "step_index": step.index, "key": key}),
                });
            }
        }
    }
}

/// Every action's checks plus binding collisions inside the set.
pub fn validate_set(set: &ActionSet) -> Vec<Diagnostic> {
    let mut out: Vec<Diagnostic> = set.actions.iter().flat_map(validate_action).collect();
    out.extend(shortcut_collisions(&[set]));
    out
}

/// [STU-AUT-182]: two actions of the given (enabled) sets claiming the same function key and
/// modifier combination; each colliding pair is named with the binding.
pub fn shortcut_collisions(enabled_sets: &[&ActionSet]) -> Vec<Diagnostic> {
    let mut seen: Vec<((u8, bool, bool), &str, &str)> = Vec::new();
    let mut out = Vec::new();
    for set in enabled_sets {
        for action in &set.actions {
            let Some(binding) = action.binding() else { continue };
            for (b, set_id, action_id) in &seen {
                if *b == binding {
                    out.push(Diagnostic {
                        code: CODE_SHORTCUT_COLLISION,
                        clause: "STU-AUT-182",
                        severity: Severity::Error,
                        fields: json!({
                            "actions": [{"action_set_id": set_id, "action_id": action_id}, {"action_set_id": set.action_set_id, "action_id": action.action_id}],
                            "binding": {"function_key_index": binding.0, "shift_modifier": binding.1, "command_modifier": binding.2},
                        }),
                    });
                }
            }
            seen.push((binding, &set.action_set_id, &action.action_id));
        }
    }
    out
}

// ----- command corpus --------------------------------------------------------------------------

/// One declared parameter of a corpus command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParamSpec {
    pub key: &'static str,
    pub value_type: ValueType,
    /// The unit a `measured` parameter is recorded in.
    pub unit: Option<UnitToken>,
}

/// One [STU-AUT-147] corpus entry (subset: no `input_schema`/`output_schema`, schemars is not an
/// approved harness-core dependency; no capability, enabler, receipt shape, argus targets or
/// manual anchor). `dry_runnable` is false for every entry: `Run` has no dry-run mode (gap).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandEntry {
    pub command_id: &'static str,
    /// The public `Run` method the command runs through.
    pub run_method: &'static str,
    pub display_name: &'static str,
    pub recordable: bool,
    pub mutates_authority: bool,
    pub dry_runnable: bool,
    pub parameters: &'static [ParamSpec],
}

pub const CMD_RENAME: &str = "layer.rename";
pub const CMD_SET_VISIBLE: &str = "layer.set_visible";
pub const CMD_SET_OPACITY: &str = "layer.set_opacity";
pub const CMD_SET_BLEND_MODE: &str = "layer.set_blend_mode";
pub const CMD_PANEL_SET: &str = "layer.panel_set";
pub const CMD_RESTRUCTURE: &str = "layer.restructure";

const fn p(key: &'static str, value_type: ValueType) -> ParamSpec {
    ParamSpec { key, value_type, unit: None }
}

const LAYER: ParamSpec = p("layer", ValueType::Reference);
const NAME: ParamSpec = p("name", ValueType::String);
const VISIBLE: ParamSpec = p("visible", ValueType::Boolean);
const OPACITY: ParamSpec = ParamSpec { key: "opacity", value_type: ValueType::Measured, unit: Some(UnitToken::Percent) };
const BLEND_MODE: ParamSpec = p("blend_mode", ValueType::Number);
const CONTROL: ParamSpec = p("control", ValueType::String);
const VALUE: ParamSpec = p("value", ValueType::String);
const OP: ParamSpec = p("op", ValueType::String);
const WRITES: ParamSpec = p("writes", ValueType::List);

const fn entry(command_id: &'static str, run_method: &'static str, display_name: &'static str, recordable: bool, mutates_authority: bool, parameters: &'static [ParamSpec]) -> CommandEntry {
    CommandEntry { command_id, run_method, display_name, recordable, mutates_authority, dry_runnable: false, parameters }
}

/// The automatable surface of [`crate::Run`]: every public operation, recordable or not. Recording
/// captures exactly the `recordable` ones; any other one run during a recording is reported as
/// `not_recordable` ([STU-AUT-109]: silent omission is forbidden).
pub const RUN_COMMANDS: &[CommandEntry] = &[
    entry(CMD_RENAME, "rename", "Rename layer", true, true, &[LAYER, NAME]),
    entry(CMD_SET_VISIBLE, "set_visible", "Show or hide layer", true, true, &[LAYER, VISIBLE]),
    entry(CMD_SET_OPACITY, "set_opacity", "Set layer opacity", true, true, &[LAYER, OPACITY]),
    // The blend mode is the StudioBlendMode discriminant: no member-name source exists in
    // harness-core's dependencies (names live in Pigment), so it records as a bare `number`.
    entry(CMD_SET_BLEND_MODE, "set_blend_mode", "Set layer blend mode", true, true, &[LAYER, BLEND_MODE]),
    entry(CMD_PANEL_SET, "panel_set", "Set panel control", true, true, &[LAYER, CONTROL, VALUE]),
    // Placement-only writes; a restructure that creates a layer carries a record payload and is
    // reported not_recordable at record time.
    entry(CMD_RESTRUCTURE, "restructure", "Restructure layers", true, true, &[OP, WRITES]),
    entry("layer.restructure_and_entry", "restructure_and_entry", "Restructure layers with document entry", false, true, &[]),
    entry("layer.set_tiles", "set_tiles", "Replace raster tiles", false, true, &[]),
    entry("layer.set_tiles_and_entry", "set_tiles_and_entry", "Replace raster tiles with document entry", false, true, &[]),
    entry("layer.set_vector_paths", "set_vector_paths", "Replace vector paths", false, true, &[]),
    entry("layer.set_vector_paths_and_entry", "set_vector_paths_and_entry", "Replace vector paths with document entry", false, true, &[]),
    entry("document.set_document_entry", "set_document_entry", "Set document entry", false, true, &[]),
    entry("document.stage_tile", "stage_tile", "Stage tile body", false, false, &[]),
    entry("document.submit_sequence", "submit_sequence", "Replace sequence", false, true, &[]),
    entry("document.submit_sequence_and_entry", "submit_sequence_and_entry", "Replace sequence with document entry", false, true, &[]),
    entry("document.submit_sequence_edits", "submit_sequence_edits", "Replace, insert or remove sequences", false, true, &[]),
    entry("document.open", "open_path / open_bytes", "Open document", false, false, &[]),
    entry("document.import", "open_import / open_import_detailed", "Open and convert to native", false, false, &[]),
    entry("document.reopen", "reopen_bytes / reopen_path", "Reopen saved document", false, false, &[]),
    entry("document.save", "save_to", "Save document", false, false, &[]),
    entry("document.checkpoint", "checkpoint", "Checkpoint document", false, false, &[]),
    entry("document.verify", "verify_live / verify_reopened", "Verify document", false, false, &[]),
    entry("document.summary", "summary", "Document summary", false, false, &[]),
    entry("layer.panel", "panel", "Layer panel", false, false, &[LAYER]),
    entry("history.undo", "undo", "Undo", false, true, &[]),
    entry("history.redo", "redo", "Redo", false, true, &[]),
    entry("action.play", "play_action", "Play action", false, true, &[]),
];

/// The corpus entry of `command_id`.
pub fn command(command_id: &str) -> Option<&'static CommandEntry> {
    RUN_COMMANDS.iter().find(|c| c.command_id == command_id)
}

/// The corpus as JSON (manual and CLI).
pub fn corpus_json() -> Value {
    json!(RUN_COMMANDS
        .iter()
        .map(|c| json!({
            "command_id": c.command_id, "run_method": c.run_method, "display_name": c.display_name,
            "recordable": c.recordable, "mutates_authority": c.mutates_authority, "dry_runnable": c.dry_runnable,
            "parameters": c.parameters.iter().map(|p| json!({"key": p.key, "value_type": p.value_type.token(), "unit": p.unit.map(UnitToken::token)})).collect::<Vec<_>>(),
        }))
        .collect::<Vec<_>>())
}

// ----- recording and playback support ----------------------------------------------------------

/// How [`crate::Run::play_action`] treats dialogs ([STU-AUT-105]). The engine has no dialog host:
/// in `Interactive` mode the steps that would show a dialog are listed in the play record
/// (`dialogs_not_shown`, GUI gap); in `Headless` mode an enabled `always_show` step refuses before
/// any step runs and `show_in_interactive_playback` is suppressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayMode {
    Interactive,
    Headless,
}

/// An operation that ran during a recording and was not captured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotRecordable {
    pub command_id: &'static str,
    pub reason: &'static str,
}

/// What a recording captured and what it did not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordingReport {
    pub action_id: String,
    pub steps: usize,
    /// Typed entries for every operation that ran and is `recordable: false`.
    pub not_recordable: Vec<NotRecordable>,
    /// Recordable operations that were refused (not recorded; Studio reading, UNVERIFIED (D5)).
    pub refused: Vec<(String, String)>,
}

impl RecordingReport {
    pub fn to_json(&self) -> Value {
        json!({
            "action_id": self.action_id, "steps": self.steps,
            "not_recordable": self.not_recordable.iter().map(|n| json!({"command_id": n.command_id, "reason": n.reason})).collect::<Vec<_>>(),
            "refused": self.refused.iter().map(|(c, e)| json!({"command_id": c, "error": e})).collect::<Vec<_>>(),
        })
    }
}

pub(crate) struct Recording {
    pub(crate) action_id: String,
    pub(crate) name: String,
    pub(crate) steps: Vec<ActionStep>,
    pub(crate) not_recordable: Vec<NotRecordable>,
    pub(crate) refused: Vec<(String, String)>,
}

impl Recording {
    pub(crate) fn new(action_id: &str, name: &str) -> Self {
        Self { action_id: action_id.to_string(), name: name.to_string(), steps: Vec::new(), not_recordable: Vec::new(), refused: Vec::new() }
    }

    pub(crate) fn push_step(&mut self, command_id: &'static str, parameters: Vec<(&'static str, RecordedValue)>) {
        let display_name = command(command_id).map_or(command_id, |c| c.display_name);
        self.steps.push(ActionStep {
            index: u64::try_from(self.steps.len()).unwrap_or(u64::MAX),
            enabled: true,
            expanded: false,
            with_dialog: false,
            dialog_options: DialogOption::Silent,
            command_id: command_id.to_string(),
            display_name: display_name.to_string(),
            has_parameters: !parameters.is_empty(),
            parameter_class: None,
            parameters: Some(parameters.into_iter().map(|(k, v)| (k.to_string(), v)).collect()),
        });
    }

    pub(crate) fn finish(self) -> (ActionRecord, RecordingReport) {
        let report = RecordingReport { action_id: self.action_id.clone(), steps: self.steps.len(), not_recordable: self.not_recordable, refused: self.refused };
        let action = ActionRecord {
            action_id: self.action_id,
            index: 0,
            name: self.name,
            function_key_index: 0,
            shift_modifier: false,
            command_modifier: false,
            color_index: 0,
            expanded: false,
            declared_step_count: u64::try_from(self.steps.len()).unwrap_or(u64::MAX),
            steps: self.steps,
        };
        (action, report)
    }
}

/// The recorded form of one placement write: `[reference layer, enumerated place.<mode>,
/// reference anchor?]`; a delete is `place.delete`.
pub(crate) fn placement_value(w: &PlacementWrite) -> RecordedValue {
    let (mode, anchor) = match &w.to {
        None => ("delete", None),
        Some(Place::Inside(g)) => ("inside", Some(g)),
        Some(Place::AtBeginning(c)) => ("at_beginning", c.as_ref()),
        Some(Place::AtEnd(c)) => ("at_end", c.as_ref()),
        Some(Place::Before(a)) => ("before", Some(a)),
        Some(Place::After(a)) => ("after", Some(a)),
    };
    let mut items = vec![RecordedValue::Reference(w.layer_id.clone()), RecordedValue::Enumerated(format!("place.{mode}"))];
    if let Some(a) = anchor {
        items.push(RecordedValue::Reference(a.clone()));
    }
    RecordedValue::List(items)
}

fn placement_of(v: &RecordedValue) -> Option<PlacementWrite> {
    let RecordedValue::List(items) = v else { return None };
    let (layer, mode, anchor) = match items.as_slice() {
        [RecordedValue::Reference(l), RecordedValue::Enumerated(m)] => (l, m.as_str(), None),
        [RecordedValue::Reference(l), RecordedValue::Enumerated(m), RecordedValue::Reference(a)] => (l, m.as_str(), Some(a.clone())),
        _ => return None,
    };
    let to = match (mode, anchor) {
        ("place.delete", None) => None,
        ("place.inside", Some(a)) => Some(Place::Inside(a)),
        ("place.at_beginning", a) => Some(Place::AtBeginning(a)),
        ("place.at_end", a) => Some(Place::AtEnd(a)),
        ("place.before", Some(a)) => Some(Place::Before(a)),
        ("place.after", Some(a)) => Some(Place::After(a)),
        _ => return None,
    };
    Some(PlacementWrite { layer_id: layer.clone(), to, create: None })
}

/// A playable step: the `Run` call and its arguments.
pub(crate) enum Call {
    Rename { layer: String, name: String },
    SetVisible { layer: String, visible: bool },
    SetOpacity { layer: String, percent: f64 },
    SetBlendMode { layer: String, mode: u16 },
    PanelSet { layer: String, control: String, value: String },
    Restructure { op: String, writes: Vec<PlacementWrite> },
}

impl Call {
    /// Layer ids the call addresses (checked to resolve before the step runs).
    pub(crate) fn layers(&self) -> Vec<&str> {
        match self {
            Self::Rename { layer, .. } | Self::SetVisible { layer, .. } | Self::SetOpacity { layer, .. } | Self::SetBlendMode { layer, .. } | Self::PanelSet { layer, .. } => vec![layer.as_str()],
            Self::Restructure { writes, .. } => writes.iter().map(|w| w.layer_id.as_str()).collect(),
        }
    }
}

/// Why a step cannot be played: a typed code and the parameter key when one is at fault.
pub(crate) struct StepFault {
    pub(crate) code: &'static str,
    pub(crate) key: Option<&'static str>,
}

/// The call a step makes, from its command id and typed parameters.
pub(crate) fn call_of(step: &ActionStep) -> Result<Call, StepFault> {
    let entry = step.command().ok_or(StepFault { code: CODE_COMMAND_UNRESOLVABLE, key: None })?;
    if !entry.recordable {
        return Err(StepFault { code: "action_step_not_playable", key: None });
    }
    let empty = BTreeMap::new();
    let params = step.parameters.as_ref().unwrap_or(&empty);
    let bad = |key: &'static str| StepFault { code: "action_step_parameter_invalid", key: Some(key) };
    let reference = |key: &'static str| match params.get(key) {
        Some(RecordedValue::Reference(s)) => Ok(s.clone()),
        _ => Err(bad(key)),
    };
    let text = |key: &'static str| match params.get(key) {
        Some(RecordedValue::String(s)) => Ok(s.clone()),
        _ => Err(bad(key)),
    };
    Ok(match entry.command_id {
        CMD_RENAME => Call::Rename { layer: reference("layer")?, name: text("name")? },
        CMD_SET_VISIBLE => match params.get("visible") {
            Some(RecordedValue::Boolean(b)) => Call::SetVisible { layer: reference("layer")?, visible: *b },
            _ => return Err(bad("visible")),
        },
        CMD_SET_OPACITY => match params.get("opacity") {
            Some(RecordedValue::Measured { value, unit: Some(UnitToken::Percent) }) => Call::SetOpacity { layer: reference("layer")?, percent: *value },
            _ => return Err(bad("opacity")),
        },
        CMD_SET_BLEND_MODE => match params.get("blend_mode") {
            Some(RecordedValue::Number(n)) if n.fract() == 0.0 && (1.0..=f64::from(u16::MAX)).contains(n) => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let mode = *n as u16;
                Call::SetBlendMode { layer: reference("layer")?, mode }
            }
            _ => return Err(bad("blend_mode")),
        },
        CMD_PANEL_SET => Call::PanelSet { layer: reference("layer")?, control: text("control")?, value: text("value")? },
        CMD_RESTRUCTURE => {
            let writes = match params.get("writes") {
                Some(RecordedValue::List(items)) if !items.is_empty() => items.iter().map(placement_of).collect::<Option<Vec<_>>>().ok_or(bad("writes"))?,
                _ => return Err(bad("writes")),
            };
            Call::Restructure { op: text("op")?, writes }
        }
        _ => return Err(StepFault { code: "action_step_not_playable", key: None }),
    })
}
