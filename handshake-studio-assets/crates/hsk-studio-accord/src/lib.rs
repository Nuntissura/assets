//! Accord host-neutral schema, identity, units and validation transport.
//! This transport is caller-owned and is NOT a full StudioDocument or database schema.
pub const SCHEMA_STUDIO_DOCUMENT: &str = "hsk.studio.document@1";
pub const SCHEMA_STUDIO_LAYER: &str = "hsk.studio.layer@1";
pub const SCHEMA_STUDIO_LAYER_GRAPH: &str = "hsk.studio.layer_graph@1";
pub const SCHEMA_STUDIO_RASTER_TILE: &str = "hsk.studio.raster_tile@1";
pub const SCHEMA_STUDIO_ARTBOARD: &str = "hsk.studio.artboard@1";
pub const SCHEMA_STUDIO_PAGE_SPREAD: &str = "hsk.studio.page_spread@1";
pub const SCHEMA_STUDIO_SELECTION_SET: &str = "hsk.studio.selection_set@1";
pub const SCHEMA_STUDIO_MASK: &str = "hsk.studio.mask@1";
pub const SCHEMA_STUDIO_VECTOR_PATH: &str = "hsk.studio.vector_path@1";
pub const SCHEMA_STUDIO_VECTOR_NETWORK: &str = "hsk.studio.vector_network@1";
pub const SCHEMA_STUDIO_TEXT_STORY: &str = "hsk.studio.text_story@1";
pub const SCHEMA_STUDIO_TYPE_STYLE: &str = "hsk.studio.type_style@1";
pub const SCHEMA_STUDIO_COLOR_PROFILE: &str = "hsk.studio.color_profile@1";
pub const SCHEMA_STUDIO_SWATCH: &str = "hsk.studio.swatch@1";
pub const SCHEMA_STUDIO_GRADIENT: &str = "hsk.studio.gradient@1";
pub const SCHEMA_STUDIO_PATTERN: &str = "hsk.studio.pattern@1";
pub const SCHEMA_STUDIO_EFFECT_STACK: &str = "hsk.studio.effect_stack@1";
pub const SCHEMA_STUDIO_ADJUSTMENT: &str = "hsk.studio.adjustment@1";
pub const SCHEMA_STUDIO_LIVE_FILTER: &str = "hsk.studio.live_filter@1";
pub const SCHEMA_STUDIO_COMPONENT: &str = "hsk.studio.component@1";
pub const SCHEMA_STUDIO_COMPONENT_INSTANCE: &str = "hsk.studio.component_instance@1";
pub const SCHEMA_STUDIO_VARIABLE: &str = "hsk.studio.variable@1";
pub const SCHEMA_STUDIO_VARIABLE_COLLECTION: &str = "hsk.studio.variable_collection@1";
pub const SCHEMA_STUDIO_STYLE_REGISTRY: &str = "hsk.studio.style_registry@1";
pub const SCHEMA_STUDIO_AUTO_LAYOUT: &str = "hsk.studio.auto_layout@1";
pub const SCHEMA_STUDIO_CONSTRAINT: &str = "hsk.studio.constraint@1";
pub const SCHEMA_STUDIO_LAYOUT_GRID: &str = "hsk.studio.layout_grid@1";
pub const SCHEMA_STUDIO_PROTOTYPE_FLOW: &str = "hsk.studio.prototype_flow@1";
pub const SCHEMA_STUDIO_MOTION_TIMELINE: &str = "hsk.studio.motion_timeline@1";
pub const SCHEMA_STUDIO_RAW_DEVELOP: &str = "hsk.studio.raw_develop@1";
pub const SCHEMA_STUDIO_EXPORT_RECIPE: &str = "hsk.studio.export_recipe@1";
pub const SCHEMA_STUDIO_IMPORT_PROFILE: &str = "hsk.studio.import_profile@1";
pub const SCHEMA_STUDIO_HISTORY_ENTRY: &str = "hsk.studio.history_entry@1";
pub const SCHEMA_STUDIO_EDIT_PROPOSAL: &str = "hsk.studio.edit_proposal@1";
pub const SCHEMA_STUDIO_SIMULATION_RECEIPT: &str = "hsk.studio.simulation_receipt@1";
pub const SCHEMA_STUDIO_BUSINESS_EVENT: &str = "hsk.studio.business_event@1";
pub const SCHEMA_STUDIO_EXPRESSION_PROFILE: &str = "hsk.studio.expression_profile@1";
pub const CANONICAL_SCHEMAS: &[&str] = &[
    "hsk.studio.document@1",
    "hsk.studio.layer@1",
    SCHEMA_STUDIO_LAYER_GRAPH,
    SCHEMA_STUDIO_RASTER_TILE,
    "hsk.studio.artboard@1",
    "hsk.studio.page_spread@1",
    "hsk.studio.selection_set@1",
    "hsk.studio.mask@1",
    "hsk.studio.vector_path@1",
    "hsk.studio.vector_network@1",
    "hsk.studio.text_story@1",
    "hsk.studio.type_style@1",
    "hsk.studio.color_profile@1",
    "hsk.studio.swatch@1",
    "hsk.studio.gradient@1",
    "hsk.studio.pattern@1",
    "hsk.studio.effect_stack@1",
    "hsk.studio.adjustment@1",
    "hsk.studio.live_filter@1",
    "hsk.studio.component@1",
    "hsk.studio.component_instance@1",
    "hsk.studio.variable@1",
    "hsk.studio.variable_collection@1",
    "hsk.studio.style_registry@1",
    "hsk.studio.auto_layout@1",
    "hsk.studio.constraint@1",
    "hsk.studio.layout_grid@1",
    "hsk.studio.prototype_flow@1",
    "hsk.studio.motion_timeline@1",
    "hsk.studio.raw_develop@1",
    "hsk.studio.export_recipe@1",
    "hsk.studio.import_profile@1",
    "hsk.studio.history_entry@1",
    "hsk.studio.edit_proposal@1",
    "hsk.studio.simulation_receipt@1",
    "hsk.studio.business_event@1",
    "hsk.studio.expression_profile@1",
];
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub const TICKS_PER_SECOND: u64 = 254_016_000_000;
pub const MAX_INPUT_BYTES: usize = 65_536;
pub const MAX_GEOMETRY_LENGTHS: usize = 64;
pub const BROADCAST_TICKS_PER_FRAME: &[u64] = &[
    10_594_584_000,
    10_584_000_000,
    10_160_640_000,
    8_475_667_200,
    8_467_200_000,
    5_292_000_000,
    5_080_320_000,
    4_237_833_600,
    4_233_600_000,
    20_321_280_000,
    16_934_400_000,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationError {
    Canceled,
    InputLimit,
    DepthLimit,
    ItemLimit,
    StringLimit,
    InvalidJson,
    DuplicateField,
    UnknownField,
    MissingField,
    InvalidType,
    UnsupportedVersion,
    UnknownSchema,
    UnsupportedIdentityMapping,
    InvalidId,
    PrefixMismatch,
    NonFinite,
    OutOfRange,
    MixedUnits,
    MissingResolution,
    TickOverflow,
    UnsupportedFrameRate,
    RevisionMismatch,
    InvalidContext,
}
impl ValidationError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Canceled => "canceled",
            Self::InputLimit => "input_limit",
            Self::DepthLimit => "depth_limit",
            Self::ItemLimit => "item_limit",
            Self::StringLimit => "string_limit",
            Self::InvalidJson => "invalid_json",
            Self::DuplicateField => "duplicate_field",
            Self::UnknownField => "unknown_field",
            Self::MissingField => "missing_field",
            Self::InvalidType => "invalid_type",
            Self::UnsupportedVersion => "unsupported_version",
            Self::UnknownSchema => "unknown_schema",
            Self::UnsupportedIdentityMapping => "unsupported_identity_mapping",
            Self::InvalidId => "invalid_id",
            Self::PrefixMismatch => "prefix_mismatch",
            Self::NonFinite => "nonfinite",
            Self::OutOfRange => "out_of_range",
            Self::MixedUnits => "mixed_units",
            Self::MissingResolution => "missing_resolution",
            Self::TickOverflow => "tick_overflow",
            Self::UnsupportedFrameRate => "unsupported_frame_rate",
            Self::RevisionMismatch => "revision_mismatch",
            Self::InvalidContext => "invalid_context",
        }
    }
}
impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for ValidationError {}
type Result<T> = std::result::Result<T, ValidationError>;

#[derive(Clone, Default, Debug)]
pub struct CancellationToken(Arc<AtomicBool>);
impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn check(&self) -> Result<()> {
        if self.0.load(Ordering::Acquire) {
            Err(ValidationError::Canceled)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaId(&'static str);
impl SchemaId {
    pub fn parse(value: &str) -> Result<Self> {
        CANONICAL_SCHEMAS
            .iter()
            .copied()
            .find(|s| *s == value)
            .map(Self)
            .ok_or(ValidationError::UnknownSchema)
    }
    pub fn as_str(&self) -> &'static str {
        self.0
    }
    pub fn identity_prefix(&self) -> Result<&'static str> {
        match self.0 {
            SCHEMA_STUDIO_DOCUMENT => Ok("SDOC"),
            SCHEMA_STUDIO_LAYER => Ok("SLYR"),
            SCHEMA_STUDIO_ARTBOARD => Ok("SART"),
            SCHEMA_STUDIO_PAGE_SPREAD => Ok("SPGS"),
            SCHEMA_STUDIO_MASK => Ok("SMSK"),
            SCHEMA_STUDIO_VECTOR_PATH => Ok("SVPT"),
            SCHEMA_STUDIO_TEXT_STORY => Ok("STXT"),
            SCHEMA_STUDIO_TYPE_STYLE => Ok("STYS"),
            SCHEMA_STUDIO_COLOR_PROFILE => Ok("SCPF"),
            SCHEMA_STUDIO_SWATCH => Ok("SSWT"),
            SCHEMA_STUDIO_EFFECT_STACK => Ok("SEFX"),
            SCHEMA_STUDIO_COMPONENT => Ok("SCMP"),
            SCHEMA_STUDIO_COMPONENT_INSTANCE => Ok("SCIN"),
            SCHEMA_STUDIO_VARIABLE => Ok("SVAR"),
            SCHEMA_STUDIO_VARIABLE_COLLECTION => Ok("SVCL"),
            SCHEMA_STUDIO_STYLE_REGISTRY => Ok("SSTY"),
            SCHEMA_STUDIO_PROTOTYPE_FLOW => Ok("SPTF"),
            SCHEMA_STUDIO_MOTION_TIMELINE => Ok("SMTL"),
            SCHEMA_STUDIO_EXPORT_RECIPE => Ok("SXPR"),
            SCHEMA_STUDIO_IMPORT_PROFILE => Ok("SIMP"),
            SCHEMA_STUDIO_HISTORY_ENTRY => Ok("SHIS"),
            SCHEMA_STUDIO_EDIT_PROPOSAL => Ok("SEPR"),
            _ => Err(ValidationError::UnsupportedIdentityMapping),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DomainId(String);
impl DomainId {
    /// Validates existing IDs, without generating or changing their spelling.
    pub fn parse(value: &str) -> Result<Self> {
        let (prefix, uuid) = value.split_once('-').ok_or(ValidationError::InvalidId)?;
        if ![
            "SDOC", "SLYR", "SART", "SPGS", "SMSK", "SVPT", "STXT", "STYS", "SCPF", "SSWT", "SEFX",
            "SCMP", "SCIN", "SVAR", "SVCL", "SSTY", "SPTF", "SMTL", "SXPR", "SIMP", "SHIS", "SEPR",
            "STRK", "SCLP",
        ]
        .contains(&prefix)
        {
            return Err(ValidationError::InvalidId);
        }
        let bytes = uuid.as_bytes();
        if bytes.len() != 36 {
            return Err(ValidationError::InvalidId);
        }
        for (i, b) in bytes.iter().enumerate() {
            if [8, 13, 18, 23].contains(&i) {
                if *b != b'-' {
                    return Err(ValidationError::InvalidId);
                }
            } else if !b.is_ascii_hexdigit() {
                return Err(ValidationError::InvalidId);
            }
        }
        if bytes[14] != b'7' || !matches!(bytes[19], b'8' | b'9' | b'a' | b'b' | b'A' | b'B') {
            return Err(ValidationError::InvalidId);
        }
        Ok(Self(value.into()))
    }
    pub fn for_schema(value: &str, schema: &SchemaId) -> Result<Self> {
        let id = Self::parse(value)?;
        if id.prefix() != schema.identity_prefix()? {
            return Err(ValidationError::PrefixMismatch);
        }
        Ok(id)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn prefix(&self) -> &str {
        self.0.split_once('-').expect("validated ID").0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Millimetres,
    Inches,
    Pixels,
    Points,
}
impl Unit {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "mm" => Ok(Self::Millimetres),
            "in" => Ok(Self::Inches),
            "px" => Ok(Self::Pixels),
            "pt" => Ok(Self::Points),
            _ => Err(ValidationError::InvalidType),
        }
    }
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Millimetres => "mm",
            Self::Inches => "in",
            Self::Pixels => "px",
            Self::Points => "pt",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Length {
    value: f64,
    unit: Unit,
}
impl Length {
    pub fn new(value: f64, unit: Unit) -> Result<Self> {
        if !value.is_finite() {
            return Err(ValidationError::NonFinite);
        }
        Ok(Self {
            value: if value == 0.0 { 0.0 } else { value },
            unit,
        })
    }
    pub fn value(self) -> f64 {
        self.value
    }
    pub fn unit(self) -> Unit {
        self.unit
    }
    pub fn nonnegative(self) -> Result<Self> {
        if self.value < 0.0 {
            Err(ValidationError::OutOfRange)
        } else {
            Ok(self)
        }
    }
    /// Convert once at transport decode, using explicit resolution when pixels cross physical units.
    pub fn convert(self, target: Unit, pixels_per_inch: Option<f64>) -> Result<Self> {
        if let Some(ppi) = pixels_per_inch {
            if !ppi.is_finite() {
                return Err(ValidationError::NonFinite);
            }
            if ppi <= 0.0 {
                return Err(ValidationError::OutOfRange);
            }
        }
        if self.unit == target {
            return Ok(self);
        }
        let ppi = if self.unit == Unit::Pixels || target == Unit::Pixels {
            pixels_per_inch.ok_or(ValidationError::MissingResolution)?
        } else {
            1.0
        };
        let inches = match self.unit {
            Unit::Millimetres => self.value / 25.4,
            Unit::Inches => self.value,
            Unit::Pixels => self.value / ppi,
            Unit::Points => self.value / 72.0,
        };
        let value = match target {
            Unit::Millimetres => inches * 25.4,
            Unit::Inches => inches,
            Unit::Pixels => inches * ppi,
            Unit::Points => inches * 72.0,
        };
        Self::new(value, target)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ticks(u64);
impl Ticks {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub const fn value(self) -> u64 {
        self.0
    }
    pub fn from_seconds(seconds: u64) -> Result<Self> {
        seconds
            .checked_mul(TICKS_PER_SECOND)
            .map(Self)
            .ok_or(ValidationError::TickOverflow)
    }
    pub fn checked_add(self, other: Self) -> Result<Self> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(ValidationError::TickOverflow)
    }
    /// Underflow (`other > self`) is `TickOverflow`, like `checked_add` overflow.
    pub fn checked_sub(self, other: Self) -> Result<Self> {
        self.0
            .checked_sub(other.0)
            .map(Self)
            .ok_or(ValidationError::TickOverflow)
    }
    /// Clamps at zero instead of failing.
    pub const fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }
    pub fn checked_frames(frames: u64, rate: FrameRate) -> Result<Self> {
        frames
            .checked_mul(rate.ticks_per_frame)
            .map(Self)
            .ok_or(ValidationError::TickOverflow)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRate {
    ticks_per_frame: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRateNormalization {
    pub source_ticks_per_frame: u64,
    pub canonical_ticks_per_frame: u64,
    pub changed: bool,
}
impl FrameRate {
    pub fn from_import(ticks: u64) -> Result<(Self, FrameRateNormalization)> {
        let canonical = match ticks {
            10_594_594_594 => 10_594_584_000,
            8_475_675_675 => 8_475_667_200,
            value => value,
        };
        if !BROADCAST_TICKS_PER_FRAME.contains(&canonical) {
            return Err(ValidationError::UnsupportedFrameRate);
        }
        Ok((
            Self {
                ticks_per_frame: canonical,
            },
            FrameRateNormalization {
                source_ticks_per_frame: ticks,
                canonical_ticks_per_frame: canonical,
                changed: ticks != canonical,
            },
        ))
    }
    pub fn ticks_per_frame(self) -> u64 {
        self.ticks_per_frame
    }
}

/// Attribution strings are bounded transport identifiers, NOT proof of a host grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActorContext {
    account_id: String,
    principal_id: String,
    owner_account_id: String,
    owner_principal_id: String,
    access_space_id: String,
    session_id: String,
}
impl ActorContext {
    pub fn new(
        account_id: &str,
        principal_id: &str,
        owner_account_id: &str,
        owner_principal_id: &str,
        access_space_id: &str,
        session_id: &str,
    ) -> Result<Self> {
        let ids = [
            account_id,
            principal_id,
            owner_account_id,
            owner_principal_id,
            access_space_id,
            session_id,
        ];
        if ids
            .iter()
            .any(|s| s.is_empty() || s.len() > 128 || s.chars().any(char::is_control))
        {
            return Err(ValidationError::InvalidContext);
        }
        Ok(Self {
            account_id: account_id.into(),
            principal_id: principal_id.into(),
            owner_account_id: owner_account_id.into(),
            owner_principal_id: owner_principal_id.into(),
            access_space_id: access_space_id.into(),
            session_id: session_id.into(),
        })
    }
    pub fn account_id(&self) -> &str {
        &self.account_id
    }
    pub fn principal_id(&self) -> &str {
        &self.principal_id
    }
    pub fn owner_account_id(&self) -> &str {
        &self.owner_account_id
    }
    pub fn owner_principal_id(&self) -> &str {
        &self.owner_principal_id
    }
    pub fn access_space_id(&self) -> &str {
        &self.access_space_id
    }
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedEnvelope {
    schema: SchemaId,
    resource_id: DomainId,
    revision: u64,
    actor: ActorContext,
    document_unit: Unit,
    geometry: Vec<Length>,
    time: Ticks,
    frame_rate: FrameRate,
    normalization: FrameRateNormalization,
}
impl ValidatedEnvelope {
    pub fn schema(&self) -> &SchemaId {
        &self.schema
    }
    pub fn resource_id(&self) -> &DomainId {
        &self.resource_id
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn actor(&self) -> &ActorContext {
        &self.actor
    }
    pub fn document_unit(&self) -> Unit {
        self.document_unit
    }
    pub fn geometry(&self) -> &[Length] {
        &self.geometry
    }
    pub fn time(&self) -> Ticks {
        self.time
    }
    pub fn frame_rate(&self) -> FrameRate {
        self.frame_rate
    }
    pub fn normalization(&self) -> FrameRateNormalization {
        self.normalization
    }
    /// Closed caller-owned transport; canonical schema tag does not imply full document decoding.
    pub fn to_json(&self) -> String {
        let actor = object([
            ("account_id", Json::String(self.actor.account_id.clone())),
            (
                "principal_id",
                Json::String(self.actor.principal_id.clone()),
            ),
            (
                "owner_account_id",
                Json::String(self.actor.owner_account_id.clone()),
            ),
            (
                "owner_principal_id",
                Json::String(self.actor.owner_principal_id.clone()),
            ),
            (
                "access_space_id",
                Json::String(self.actor.access_space_id.clone()),
            ),
            ("session_id", Json::String(self.actor.session_id.clone())),
        ]);
        let geometry = object([
            (
                "document_unit",
                Json::String(self.document_unit.as_str().into()),
            ),
            (
                "lengths",
                Json::Array(
                    self.geometry
                        .iter()
                        .map(|l| {
                            object([
                                ("value", Json::Number(l.value.to_string())),
                                ("unit", Json::String(l.unit.as_str().into())),
                            ])
                        })
                        .collect(),
                ),
            ),
        ]);
        let rate = object([(
            "ticks_per_frame",
            Json::Number(self.frame_rate.ticks_per_frame.to_string()),
        )]);
        object([
            ("envelope_version", Json::Number("1".into())),
            ("schema_id", Json::String(self.schema.as_str().into())),
            (
                "resource_id",
                Json::String(self.resource_id.as_str().into()),
            ),
            ("revision", Json::Number(self.revision.to_string())),
            ("actor", actor),
            ("geometry", geometry),
            ("time_ticks", Json::Number(self.time.0.to_string())),
            ("frame_rate", rate),
        ])
        .encode()
    }
}

#[derive(Debug)]
enum Json {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}
impl Json {
    fn encode(&self) -> String {
        match self {
            Self::Null => "null".into(),
            Self::Bool(v) => v.to_string(),
            Self::Number(s) => s.clone(),
            Self::String(s) => quote(s),
            Self::Array(v) => format!(
                "[{}]",
                v.iter().map(Self::encode).collect::<Vec<_>>().join(",")
            ),
            Self::Object(m) => format!(
                "{{{}}}",
                m.iter()
                    .map(|(k, v)| format!("{}:{}", quote(k), v.encode()))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        }
    }
    fn map(self) -> Result<BTreeMap<String, Json>> {
        if let Self::Object(m) = self {
            Ok(m)
        } else {
            Err(ValidationError::InvalidType)
        }
    }
    fn string(self) -> Result<String> {
        if let Self::String(s) = self {
            Ok(s)
        } else {
            Err(ValidationError::InvalidType)
        }
    }
    fn u64(self) -> Result<u64> {
        if let Self::Number(s) = self {
            if !s.bytes().all(|b| b.is_ascii_digit()) {
                return Err(ValidationError::InvalidType);
            }
            s.parse().map_err(|_| ValidationError::OutOfRange)
        } else {
            Err(ValidationError::InvalidType)
        }
    }
    fn finite(self) -> Result<f64> {
        if let Self::Number(s) = self {
            let n: f64 = s.parse().map_err(|_| ValidationError::InvalidType)?;
            if n.is_finite() {
                Ok(n)
            } else {
                Err(ValidationError::NonFinite)
            }
        } else {
            Err(ValidationError::InvalidType)
        }
    }
}
fn object<const N: usize>(entries: [(&str, Json); N]) -> Json {
    Json::Object(
        entries
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect(),
    )
}
fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
fn field(map: &mut BTreeMap<String, Json>, name: &str) -> Result<Json> {
    map.remove(name).ok_or(ValidationError::MissingField)
}
fn closed(map: &BTreeMap<String, Json>) -> Result<()> {
    if map.is_empty() {
        Ok(())
    } else {
        Err(ValidationError::UnknownField)
    }
}
struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
    items: usize,
    cancel: &'a CancellationToken,
}
impl<'a> Parser<'a> {
    fn skip(&mut self) {
        while self
            .bytes
            .get(self.at)
            .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            self.at += 1
        }
    }
    fn byte(&mut self) -> Result<u8> {
        let b = *self
            .bytes
            .get(self.at)
            .ok_or(ValidationError::InvalidJson)?;
        self.at += 1;
        Ok(b)
    }
    fn expect(&mut self, b: u8) -> Result<()> {
        if self.byte()? == b {
            Ok(())
        } else {
            Err(ValidationError::InvalidJson)
        }
    }
    fn hex4(&mut self) -> Result<u32> {
        let mut v = 0;
        for _ in 0..4 {
            v = v * 16
                + match self.byte()? {
                    b'0'..=b'9' => self.bytes[self.at - 1] as u32 - b'0' as u32,
                    b'a'..=b'f' => self.bytes[self.at - 1] as u32 - b'a' as u32 + 10,
                    b'A'..=b'F' => self.bytes[self.at - 1] as u32 - b'A' as u32 + 10,
                    _ => return Err(ValidationError::InvalidJson),
                }
        }
        Ok(v)
    }
    fn string(&mut self) -> Result<String> {
        self.expect(b'"')?;
        let mut out = String::new();
        let mut start = self.at;
        loop {
            if self.at.is_multiple_of(256) {
                self.cancel.check()?
            }
            let b = self.byte()?;
            if b == b'"' || b == b'\\' {
                out.push_str(
                    std::str::from_utf8(&self.bytes[start..self.at - 1])
                        .map_err(|_| ValidationError::InvalidJson)?,
                );
                if b == b'"' {
                    if out.len() > 1024 {
                        return Err(ValidationError::StringLimit);
                    }
                    return Ok(out);
                }
                let e = self.byte()?;
                let c = match e {
                    b'"' => '"',
                    b'\\' => '\\',
                    b'/' => '/',
                    b'b' => '\u{8}',
                    b'f' => '\u{c}',
                    b'n' => '\n',
                    b'r' => '\r',
                    b't' => '\t',
                    b'u' => {
                        let mut v = self.hex4()?;
                        if (0xd800..=0xdbff).contains(&v) {
                            self.expect(b'\\')?;
                            self.expect(b'u')?;
                            let low = self.hex4()?;
                            if !(0xdc00..=0xdfff).contains(&low) {
                                return Err(ValidationError::InvalidJson);
                            }
                            v = 0x10000 + ((v - 0xd800) << 10) + (low - 0xdc00)
                        }
                        char::from_u32(v).ok_or(ValidationError::InvalidJson)?
                    }
                    _ => return Err(ValidationError::InvalidJson),
                };
                out.push(c);
                start = self.at;
            } else if b < 32 {
                return Err(ValidationError::InvalidJson);
            }
            if out.len() + self.at - start > 1024 {
                return Err(ValidationError::StringLimit);
            }
        }
    }
    fn number(&mut self) -> Result<Json> {
        let start = self.at;
        if self.bytes.get(self.at) == Some(&b'-') {
            self.at += 1
        }
        match self.byte()? {
            b'0' => {}
            b'1'..=b'9' => {
                while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                    self.at += 1
                }
            }
            _ => return Err(ValidationError::InvalidJson),
        }
        if self.bytes.get(self.at) == Some(&b'.') {
            self.at += 1;
            let before = self.at;
            while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                self.at += 1
            }
            if before == self.at {
                return Err(ValidationError::InvalidJson);
            }
        }
        if self
            .bytes
            .get(self.at)
            .is_some_and(|b| *b == b'e' || *b == b'E')
        {
            self.at += 1;
            if self
                .bytes
                .get(self.at)
                .is_some_and(|b| *b == b'+' || *b == b'-')
            {
                self.at += 1
            }
            let before = self.at;
            while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                self.at += 1
            }
            if before == self.at {
                return Err(ValidationError::InvalidJson);
            }
        }
        if self.at - start > 128 {
            return Err(ValidationError::StringLimit);
        }
        Ok(Json::Number(
            std::str::from_utf8(&self.bytes[start..self.at])
                .map_err(|_| ValidationError::InvalidJson)?
                .into(),
        ))
    }
    fn value(&mut self, depth: usize) -> Result<Json> {
        self.cancel.check()?;
        if depth > 8 {
            return Err(ValidationError::DepthLimit);
        }
        self.items += 1;
        if self.items > 512 {
            return Err(ValidationError::ItemLimit);
        }
        self.skip();
        match self.bytes.get(self.at).copied() {
            Some(b'"') => Ok(Json::String(self.string()?)),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(b'[') => {
                self.at += 1;
                let mut v = vec![];
                self.skip();
                if self.bytes.get(self.at) == Some(&b']') {
                    self.at += 1;
                    return Ok(Json::Array(v));
                }
                loop {
                    v.push(self.value(depth + 1)?);
                    self.skip();
                    let b = self.byte()?;
                    if b == b']' {
                        break;
                    }
                    if b != b',' {
                        return Err(ValidationError::InvalidJson);
                    }
                }
                Ok(Json::Array(v))
            }
            Some(b'{') => {
                self.at += 1;
                let mut m = BTreeMap::new();
                self.skip();
                if self.bytes.get(self.at) == Some(&b'}') {
                    self.at += 1;
                    return Ok(Json::Object(m));
                }
                loop {
                    self.skip();
                    let key = self.string()?;
                    self.skip();
                    self.expect(b':')?;
                    let v = self.value(depth + 1)?;
                    if m.insert(key, v).is_some() {
                        return Err(ValidationError::DuplicateField);
                    }
                    self.skip();
                    let b = self.byte()?;
                    if b == b'}' {
                        break;
                    }
                    if b != b',' {
                        return Err(ValidationError::InvalidJson);
                    }
                }
                Ok(Json::Object(m))
            }
            Some(b't' | b'f' | b'n') => {
                let (token, result) = match self.bytes[self.at] {
                    b't' => (b"true".as_slice(), Json::Bool(true)),
                    b'f' => (b"false".as_slice(), Json::Bool(false)),
                    _ => (b"null".as_slice(), Json::Null),
                };
                if self.bytes.get(self.at..self.at + token.len()) != Some(token) {
                    return Err(ValidationError::InvalidJson);
                }
                self.at += token.len();
                Ok(result)
            }
            _ => Err(ValidationError::InvalidJson),
        }
    }
}

/// Decode a closed, versioned capability-validation transport into immutable typed values.
/// `expected_revision` is supplied by the caller's current context, never persisted by Accord.
/// The canonical schema tag identifies the resource; this does not validate its domain payload.
pub fn parse_and_validate(
    input: &str,
    expected_revision: u64,
    cancel: &CancellationToken,
) -> Result<ValidatedEnvelope> {
    cancel.check()?;
    if input.len() > MAX_INPUT_BYTES {
        return Err(ValidationError::InputLimit);
    }
    let mut parser = Parser {
        bytes: input.as_bytes(),
        at: 0,
        items: 0,
        cancel,
    };
    let json = parser.value(0)?;
    parser.skip();
    if parser.at != input.len() {
        return Err(ValidationError::InvalidJson);
    }
    let mut root = json.map()?;
    if field(&mut root, "envelope_version")?.u64()? != 1 {
        return Err(ValidationError::UnsupportedVersion);
    }
    let schema = SchemaId::parse(&field(&mut root, "schema_id")?.string()?)?;
    let resource_id = DomainId::for_schema(&field(&mut root, "resource_id")?.string()?, &schema)?;
    let revision = field(&mut root, "revision")?.u64()?;
    if revision != expected_revision {
        return Err(ValidationError::RevisionMismatch);
    }
    let mut actor = field(&mut root, "actor")?.map()?;
    let context = ActorContext::new(
        &field(&mut actor, "account_id")?.string()?,
        &field(&mut actor, "principal_id")?.string()?,
        &field(&mut actor, "owner_account_id")?.string()?,
        &field(&mut actor, "owner_principal_id")?.string()?,
        &field(&mut actor, "access_space_id")?.string()?,
        &field(&mut actor, "session_id")?.string()?,
    )?;
    closed(&actor)?;
    let mut g = field(&mut root, "geometry")?.map()?;
    let document_unit = Unit::parse(&field(&mut g, "document_unit")?.string()?)?;
    let resolution = g.remove("pixels_per_inch").map(Json::finite).transpose()?;
    if resolution.is_some_and(|p| p <= 0.0) {
        return Err(ValidationError::OutOfRange);
    }
    let Json::Array(raw_lengths) = field(&mut g, "lengths")? else {
        return Err(ValidationError::InvalidType);
    };
    closed(&g)?;
    if raw_lengths.len() > MAX_GEOMETRY_LENGTHS {
        return Err(ValidationError::ItemLimit);
    }
    let mut source_unit = None;
    let mut geometry = vec![];
    for raw in raw_lengths {
        cancel.check()?;
        let mut length = raw.map()?;
        let value = field(&mut length, "value")?.finite()?;
        let unit = Unit::parse(&field(&mut length, "unit")?.string()?)?;
        closed(&length)?;
        if source_unit.is_some_and(|s| s != unit) {
            return Err(ValidationError::MixedUnits);
        }
        source_unit = Some(unit);
        geometry.push(Length::new(value, unit)?.convert(document_unit, resolution)?);
    }
    let time = Ticks::new(field(&mut root, "time_ticks")?.u64()?);
    let mut rate = field(&mut root, "frame_rate")?.map()?;
    let (frame_rate, normalization) =
        FrameRate::from_import(field(&mut rate, "ticks_per_frame")?.u64()?)?;
    closed(&rate)?;
    closed(&root)?;
    cancel.check()?;
    Ok(ValidatedEnvelope {
        schema,
        resource_id,
        revision,
        actor: context,
        document_unit,
        geometry,
        time,
        frame_rate,
        normalization,
    })
}

pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-ACCORD","version":1,"operation":"Validate caller-owned capability envelope; inspect normalized units, revision, identities and ticks","scope":"No complete StudioDocument payload decode, persistence, host authorization, database, GUI or native provider","operator":{"intent":"Check an envelope before handing immutable values to a document consumer","result":"Typed accepted transport plus frame-rate normalization receipt, or bounded typed rejection","recovery":"Correct only rejected input, get current revision/context from authority, retry unless canceled; never overwrite a stale revision"},"model":{"api":"parse_and_validate(input, expected_revision, CancellationToken)","consumer":"accord-consumer --input FILE --expected-revision U64 [--add-ticks U64] [--cancel]","format":"Closed JSON object: envelope_version:1,schema_id canonical hsk.studio.*@1,resource_id matching defined UUIDv7 prefix,revision:u64,actor:{account_id,principal_id,owner_account_id,owner_principal_id,access_space_id,session_id},geometry:{document_unit:mm|in|px|pt,lengths:[{value:finite number,unit:mm|in|px|pt}],pixels_per_inch?:positive finite number},time_ticks:u64,frame_rate:{ticks_per_frame:u64}","identity":"Uses STU-CON-002/004; schemas with no specified prefix return unsupported_identity_mapping; STRK/SCLP are separately validated by DomainId, not reminted","units":"No mixed source units; decode normalizes to document unit; pixel conversion needs explicit resolution; typography callers use pt, raster callers px","time":"254016000000 ticks/s; 11 broadcast tick values plus 10594594594->10594584000 and 8475675675->8475667200 legacy normalization receipt; arbitrary numeric frame picker remains outside this slice","revision":"u64 is local caller revision precondition, not a persisted authority schema","canonical_output":"to_json is deterministic closed transport; normalization receipt separately retains original/canonical ticks; no full document acceptance","limits":{"input_bytes":65536,"json_depth":8,"json_values":512,"string_bytes":1024,"geometry_lengths":64},"unknown_fields":"Reject all unknown fields, including unknown required fields; duplicate fields reject","cancel":"Clone token can be canceled concurrently; parser and validation poll; cancellation returns Canceled without modifying input","undo":"Read-only, no mutation or undo action","argus":{"inspect":"immutable accepted transport and receipt/error","action":"call same validation API with explicit revision and token","state":"schema/resource/actor/revision plus normalized units/ticks","capture":"caller captures bounded output under current granted owner"},"diagnostics":"Caller-owned adapters deliver bounded error and receipt to Flight Recorder, internal diagnostics, Palmistry; actor strings are attribution only and host grant/visibility enforcement remains required before discovery/use"}}"#;
