//! Export parameter dictionary (lane SMB-PR-EXPORT-RECIPE-1; STU-VID-061, 061a, 061b, STU-IO-102..106,
//! STU-FX-104..106): the 453 export parameter identifiers of the spec's facet tables as const rows
//! ([`export_parameters`], data in `export_parameter_rows.rs`), each stored exactly as the table says.
//!
//! Reading rules (STU-VID-061b, STU-IO-103): a `--` cell is [`DeclaredValue::Absent`], never zero, never
//! unbounded-by-decision, never copied from its twin. Soft bounds, default and unit are absent on all 453
//! rows. STU-FX-106 tells `declared_hard_only` to set soft equal to hard; the export clauses (STU-IO-103,
//! STU-VID-061b item 2) forbid that, and they are followed here: soft stays absent. No label and no unit is
//! authored in this table: 44 rows carry a recovered label, the other 409 display their identifier
//! (`label_source: identifier_fallback`, STU-IO-106), and every unit is [`ExportUnit::Undeclared`], which
//! differs from [`ExportUnit::Dimensionless`]. Units a writer consumes are declared in that writer's binding
//! (`export_recipe`), not here.
//!
//! Declared cells that are no usable range are kept as declared and named: [`DEGENERATE_BOUNDS`] (17
//! signed-integer rows whose minimum is not below their maximum, e.g. `ADBEVideoCodec` 0 / -1), the `false`
//! / `false` cells of 14 structure rows, and the text `0,0` of `ADBEVideoAspect`. `ADBEVideoFPS` is typed
//! boolean with bounds 1..254016000000: it is ticks per frame (STU-IO-142).
//!
//! Not in the source table, so not invented here (STU-IO-102 fields): `description`, `group_path`, `step`
//! fields, `enumerated_values`, `is_hidden`. Vendor parity is UNVERIFIED (D5): every cell is the spec's capture.
//! Proof: tests/export_parameters.rs (fixture tests/fixtures/stu-vid-061b-rows.txt, SHA-256 pinned).
#![deny(clippy::arithmetic_side_effects)]

#[path = "export_parameter_rows.rs"]
mod rows;

/// Export parameter type code (STU-IO-104, closed, twelve members).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExportTypeCode {
    GroupHeader,
    SignedInteger,
    FloatingPoint,
    Boolean,
    String,
    EnumeratedInteger,
    Action,
    TabGroup,
    Path,
    Opaque,
    RepeatingGroup,
    Colour,
}

impl ExportTypeCode {
    pub const ALL: [Self; 12] = [
        Self::GroupHeader,
        Self::SignedInteger,
        Self::FloatingPoint,
        Self::Boolean,
        Self::String,
        Self::EnumeratedInteger,
        Self::Action,
        Self::TabGroup,
        Self::Path,
        Self::Opaque,
        Self::RepeatingGroup,
        Self::Colour,
    ];

    /// The STU-IO-104 code, 1..=12.
    pub const fn code(self) -> u8 {
        match self {
            Self::GroupHeader => 1,
            Self::SignedInteger => 2,
            Self::FloatingPoint => 3,
            Self::Boolean => 4,
            Self::String => 5,
            Self::EnumeratedInteger => 6,
            Self::Action => 7,
            Self::TabGroup => 8,
            Self::Path => 9,
            Self::Opaque => 10,
            Self::RepeatingGroup => 11,
            Self::Colour => 12,
        }
    }

    pub const fn token(self) -> &'static str {
        match self {
            Self::GroupHeader => "group_header",
            Self::SignedInteger => "signed_integer",
            Self::FloatingPoint => "floating_point",
            Self::Boolean => "boolean",
            Self::String => "string",
            Self::EnumeratedInteger => "enumerated_integer",
            Self::Action => "action",
            Self::TabGroup => "tab_group",
            Self::Path => "path",
            Self::Opaque => "opaque",
            Self::RepeatingGroup => "repeating_group",
            Self::Colour => "colour",
        }
    }

    /// Codes 1 (group header), 7 (action), 8 (tab group) and 11 (repeating group) store no value; the
    /// dictionary keeps their rows (STU-IO-104: structure, never flattened away).
    pub const fn stores_value(self) -> bool {
        !matches!(self, Self::GroupHeader | Self::Action | Self::TabGroup | Self::RepeatingGroup)
    }
}

/// Export parameter facet (STU-IO-105, closed, fourteen members).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExportFacet {
    VideoCodec,
    VideoFrame,
    RateControl,
    GopStructure,
    Colour,
    Audio,
    Multiplexing,
    Captions,
    Layout,
    Metadata,
    Performance,
    VrImmersive,
    PublishingDestination,
    Other,
}

impl ExportFacet {
    pub const ALL: [Self; 14] = [
        Self::VideoCodec,
        Self::VideoFrame,
        Self::RateControl,
        Self::GopStructure,
        Self::Colour,
        Self::Audio,
        Self::Multiplexing,
        Self::Captions,
        Self::Layout,
        Self::Metadata,
        Self::Performance,
        Self::VrImmersive,
        Self::PublishingDestination,
        Self::Other,
    ];

    pub const fn token(self) -> &'static str {
        match self {
            Self::VideoCodec => "video_codec",
            Self::VideoFrame => "video_frame",
            Self::RateControl => "rate_control",
            Self::GopStructure => "gop_structure",
            Self::Colour => "colour",
            Self::Audio => "audio",
            Self::Multiplexing => "multiplexing",
            Self::Captions => "captions",
            Self::Layout => "layout",
            Self::Metadata => "metadata",
            Self::Performance => "performance",
            Self::VrImmersive => "vr_immersive",
            Self::PublishingDestination => "publishing_destination",
            Self::Other => "other",
        }
    }
}

/// One declared cell of the source table, exactly as written.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DeclaredValue {
    /// `--`: the source declares nothing and Studio declares nothing (STU-VID-061b).
    Absent,
    Integer(i64),
    /// A cell written with a decimal point (`0.19`).
    Decimal(f64),
    /// The cell `false` (declared domain of a structure row, not a number).
    False,
    /// Any other text (`0,0` of `ADBEVideoAspect`).
    Text(&'static str),
}

impl DeclaredValue {
    pub const fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }
}

impl std::fmt::Display for DeclaredValue {
    /// The cell as the spec table writes it (`--` for absent).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Absent => f.write_str("--"),
            Self::Integer(v) => write!(f, "{v}"),
            Self::Decimal(v) => write!(f, "{v}"),
            Self::False => f.write_str("false"),
            Self::Text(t) => f.write_str(t),
        }
    }
}

/// Where a row's label comes from (STU-IO-106).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelSource {
    Recovered,
    IdentifierFallback,
}

impl LabelSource {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Recovered => "recovered",
            Self::IdentifierFallback => "identifier_fallback",
        }
    }
}

/// A parameter's unit (STU-IO-102, STU-IO-160): `Undeclared` (no unit token in the source, nothing authored)
/// is not `Dimensionless` (a declared absence of unit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportUnit {
    Undeclared,
    Dimensionless,
    Declared(&'static str),
}

/// Where the bounds came from (STU-FX-106, five members).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundState {
    DeclaredBoth,
    DeclaredHardOnly,
    DeclaredSoftOnly,
    UnboundedInSource,
    ObservedOnly,
}

impl BoundState {
    pub const ALL: [Self; 5] =
        [Self::DeclaredBoth, Self::DeclaredHardOnly, Self::DeclaredSoftOnly, Self::UnboundedInSource, Self::ObservedOnly];

    pub const fn token(self) -> &'static str {
        match self {
            Self::DeclaredBoth => "declared_both",
            Self::DeclaredHardOnly => "declared_hard_only",
            Self::DeclaredSoftOnly => "declared_soft_only",
            Self::UnboundedInSource => "unbounded_in_source",
            Self::ObservedOnly => "observed_only",
        }
    }
}

/// One export parameter record (STU-IO-102 fields the source table carries, plus provenance).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExportParameter {
    pub identifier: &'static str,
    /// The recovered label; `None` on the 409 unlabelled rows.
    pub label: Option<&'static str>,
    pub label_source: LabelSource,
    pub type_code: ExportTypeCode,
    pub facet: ExportFacet,
    pub hard_min: DeclaredValue,
    pub hard_max: DeclaredValue,
    pub soft_min: DeclaredValue,
    pub soft_max: DeclaredValue,
    pub default: DeclaredValue,
    pub unit: ExportUnit,
    /// Declared decimal places; `None` where the cell is `--`.
    pub precision: Option<u8>,
    pub is_slider: bool,
    /// Shipped presets using the identifier (provenance; no upper bound is asserted).
    pub used_by_preset_count: u32,
    /// Container formats the capture names (provenance, as the table lists them).
    pub used_by_formats: &'static [&'static str],
    pub bound_state: BoundState,
}

impl ExportParameter {
    /// The label an operator or model sees: the recovered label, else the identifier (STU-IO-106).
    pub const fn display_label(&self) -> &'static str {
        match self.label {
            Some(label) => label,
            None => self.identifier,
        }
    }

    /// A declared numeric hard bound usable for a range check (STU-FX-104): integer and decimal cells of a
    /// row not in [`DEGENERATE_BOUNDS`]; absent, `false` and text cells are no bound.
    pub fn usable_hard_bounds(&self) -> (Option<DeclaredValue>, Option<DeclaredValue>) {
        if DEGENERATE_BOUNDS.contains(&self.identifier) {
            return (None, None);
        }
        let usable = |v: DeclaredValue| matches!(v, DeclaredValue::Integer(_) | DeclaredValue::Decimal(_)).then_some(v);
        (usable(self.hard_min), usable(self.hard_max))
    }
}

/// Row constructor for the data file: soft bounds, default and unit are absent on every row of the source
/// (STU-VID-061b items 2-4); the label source and bound state follow from the declared cells.
#[allow(clippy::too_many_arguments)]
const fn row(
    identifier: &'static str,
    label: Option<&'static str>,
    type_code: ExportTypeCode,
    facet: ExportFacet,
    hard_min: DeclaredValue,
    hard_max: DeclaredValue,
    precision: Option<u8>,
    is_slider: bool,
    used_by_preset_count: u32,
    used_by_formats: &'static [&'static str],
) -> ExportParameter {
    let label_source = match label {
        Some(_) => LabelSource::Recovered,
        None => LabelSource::IdentifierFallback,
    };
    let bound_state = if hard_min.is_absent() && hard_max.is_absent() {
        BoundState::UnboundedInSource
    } else {
        BoundState::DeclaredHardOnly
    };
    ExportParameter {
        identifier,
        label,
        label_source,
        type_code,
        facet,
        hard_min,
        hard_max,
        soft_min: DeclaredValue::Absent,
        soft_max: DeclaredValue::Absent,
        default: DeclaredValue::Absent,
        unit: ExportUnit::Undeclared,
        precision,
        is_slider,
        used_by_preset_count,
        used_by_formats,
        bound_state,
    }
}

/// Number of identifiers in the dictionary (STU-VID-061).
pub const EXPORT_PARAMETER_COUNT: usize = 453;

/// Signed-integer rows whose declared minimum is not below their maximum: the cells stay as declared, and
/// they are not range-enforced (lane brief open question, lead-overrulable; 0 / -1 would refuse every value,
/// 0 / 0 is explained by STU-VID-061b only for group headers).
pub const DEGENERATE_BOUNDS: [&str; 17] = [
    "ADBEAudioCodec",
    "ADBEExportColorSpace",
    "ADBEMPEGAudioDeemphasis",
    "ADBEMPEGAudioFormat",
    "ADBEMPEGAudioLayer",
    "ADBEMPEGAudioMode",
    "ADBEMPEGAudioPsychModel",
    "ADBEMPEGMultiplexer",
    "ADBEMPEGMuxStreamCompatibility",
    "ADBEMPEGTVStandard",
    "ADBEMPEGVideoEncoderNameParam",
    "ADBEMPEGVideoEncodingPerformanceParam",
    "ADBEMPEGVideoEncodingPerformanceParamUserSelected",
    "ADBEMPEGVideoHDR10MasterDisplayPrimaries",
    "ADBEVideoCodec",
    "ADBEVideoMPEGProfile",
    "ADBEVideoVRStereoscopic",
];

/// Credential rows outside the `publishing_destination` facet (STU-IO-147, STU-IO-185): refused in a recipe
/// like every `publishing_destination` row.
pub const CREDENTIAL_OUTSIDE_PUBLISHING: [&str; 3] = ["ADBEStockLogin", "ADBEStockLoginSubGroup", "ADBEStockRefreshToken"];

/// Codec-selection integers (STU-IO-108: exporter-local): the signed-integer rows whose recovered label is
/// `Video Codec` or `Audio Codec`. Unlabelled codec-like identifiers are not classified (STU-VID-061a).
pub const CODEC_SELECTION: [&str; 4] = ["ADBEAudioCodec", "ADBEVideoCodec", "AudioCodec", "VideoCodec"];

/// Every row, sorted by identifier bytes.
pub fn export_parameters() -> &'static [ExportParameter] {
    &rows::ROWS
}

/// The row of `identifier` (binary search, O(log n)).
pub fn export_parameter(identifier: &str) -> Option<&'static ExportParameter> {
    let rows = export_parameters();
    rows.binary_search_by(|r| r.identifier.as_bytes().cmp(identifier.as_bytes()))
        .ok()
        .and_then(|i| rows.get(i))
}

/// Label coverage (STU-IO-106, STU-VID-061a): (rows, recovered labels, identifier fallbacks).
pub fn label_coverage() -> (usize, usize, usize) {
    let recovered = export_parameters().iter().filter(|r| r.label_source == LabelSource::Recovered).count();
    (EXPORT_PARAMETER_COUNT, recovered, EXPORT_PARAMETER_COUNT.saturating_sub(recovered))
}
