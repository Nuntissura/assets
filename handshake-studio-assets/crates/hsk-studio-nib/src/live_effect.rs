//! Vector live effects: the Distort & Transform family (lane SMB-AI-DISTORT-EFFECTS-1; STU-FX-027,
//! STU-FX-028, STU-VEC-154, STU-VEC-155, STU-VEC-158, STU-VEC-100, STU-VEC-101, STU-FX-011).
//!
//! # Typed records (STU-VEC-154)
//! An [`EffectRecord`] carries a stable effect id (`hsk.studio.vector_effect.<snake>`), a version
//! stamp ([`RECORD_VERSION`]) and an ordered map from stable parameter keys to typed values drawn
//! from exactly `{Int, Real, Bool, String}` ([`ParamValue`]). A write with an unknown key, the wrong
//! type, a missing type token ([`EffectRecord::set_triple`]), a non-finite real, a value outside a
//! KNOWN hard bound, a non-member enum token or a non-count for a `count` unit is refused with a
//! typed [`LiveEffectError`]; nothing is clamped.
//!
//! The parameter rows of [`EffectKind::params`] are copied cell for cell from STU-VEC-155
//! (roughen, zig zag, pucker and bloat, twist, tweak, free distort) and STU-VEC-158 (convert to
//! shape, object mosaic): seven independent numeric fields, `unknown` kept as `None` (never
//! substituted by the soft range, STU-VEC-100). A parameter whose default is `unknown` is absent
//! from a fresh record and an evaluation refuses `parameter_unset` until a caller sets it: Studio
//! never invents a default. `roughen_mode` defaults to `mode.relative`, the Studio decision the spec
//! demands (STU-VEC-155); the roughen and tweak `seed` rows (STU-FX-011) and every Transform
//! (step-repeat) row are `StudioDeclared` (no recovered record exists for them).
//!
//! # Laws (declared Studio readings v1; Illustrator parity UNVERIFIED, D5)
//! The spec recovers parameters, not formulas (lead decision G1). Each law below is the Studio
//! reading for record version 1; a record stamped with another version is refused
//! (`unsupported_version`), so an unknown law never evaluates. Geometry and absolute sizes are in
//! `pt`, the canonical internal unit; `document_unit` values are resolved to `pt` at the API decode
//! boundary (STU-VEC-101) before they reach these operators. `per_in` uses 72 pt per inch.
//! Relative sizes are percent of the union bounds of the source paths (exact geometric bounds):
//! roughen and zig zag of the bounds diagonal, tweak horizontal of the width and vertical of the
//! height.
//! * Roughen: each segment of arc length `L` is cut into `max(1, ceil(detail * L / 72))` equal
//!   arc-length pieces; every sample point (original anchors included) moves along the left normal
//!   `(-t_y, t_x)` by `size * s_k * u_k`, `s_k = +1, -1, ...` alternating (peaks and valleys),
//!   `u_k` in `[0, 1)` from `arrange::unit_random(seed, subpath stream, k)`; the two end anchors of an
//!   open subpath stay. `points.corner` joins the points with lines, `points.smooth` with
//!   Catmull-Rom cubics. A density at or below zero adds no points; a negative size flips the
//!   alternation.
//! * Zig zag: `shape_ops::zig_zag` (each segment cut into `ridges + 1` equal-length pieces, interior
//!   points alternate `+size, -size` along the normal; corner = lines, smooth = Catmull-Rom); the
//!   ridge count is a non-negative integer (`count` unit).
//! * Pucker and bloat: `shape_ops::pucker_bloat` (anchors move toward each subpath's vertex
//!   centroid by `factor / 100`, Bezier control points away from it): negative factors pucker
//!   (anchors outward, segments pinched), positive factors bloat.
//! * Twist: straight segments are first raised to cubics with handles at one and two thirds; every
//!   anchor and control point rotates about the centre of the source bounds by
//!   `angle * max(0, 1 - d / R)`, `d` its distance to the centre, `R` the largest anchor distance:
//!   the centre turns by the full angle, the outermost anchor stays. Positive angles turn clockwise
//!   on the y-down page. `R = 0` is refused (`degenerate_extent`).
//! * Tweak: anchor `k` of a subpath moves by `(H r, V r')`, `r, r'` uniform in `[-1, 1)` (seeded
//!   streams); its incoming and outgoing control points follow their anchor and, when their own
//!   switch is on, add their own seeded displacement. A straight segment stays straight unless a
//!   handle switch is on. All three switches off is the identity.
//! * Free distort: the projective map (homography) of the four source corners `src0..src3` onto
//!   `dst0..dst3` (unit square construction, P. Heckbert 1989). Both quads must be non-degenerate,
//!   non-self-crossing and convex (a projective map of a convex quad onto a concave one does not
//!   exist); a point mapped on or beyond the vanishing line is refused. Lines map exactly by their
//!   end points; a cubic is split in halves until the mapped control-point cubic is within
//!   [`FREE_DISTORT_TOLERANCE_PT`] of the exact image at `t = 1/4, 1/2, 3/4`, or depth
//!   [`FREE_DISTORT_MAX_DEPTH`]. `dst == src` returns the sources unchanged.
//! * Transform (step-repeat): `T = translate(move) . about(reference, rotate . scale . reflect)`
//!   (reference point of the source bounds, `arrange::ReferencePoint`); copy `k` (`1..=copies`) gets
//!   `T_k = step_k . T_(k-1)`, `step_k = T` unless `random`, which draws each amount uniformly
//!   between the identity value and the set value (seeded). Output: the sources, then every
//!   source per copy, in order.
//! * Convert to shape and object mosaic: typed records only; evaluation refuses
//!   `convert_to_shape_sizing_law_not_recovered` / `object_mosaic_grid_law_not_recovered`
//!   (STU-VEC-158 declared gaps; MT-30559 / MT-33001 step 1 forbid guessing the law).
//!
//! # Re-evaluation and outcomes
//! Every operator is a pure function of (source paths, record): it borrows the sources, never
//! mutates them, and a re-evaluation after a source edit starts from the edited source (STU-FX-027);
//! the same record (and seed) gives byte-identical output (STU-FX-011). A canceled token refuses
//! `canceled` and returns nothing. Terminal outcomes reach observe as codes only through
//! [`outcome`] and [`report`] (no geometry, names or parameter text).
//!
//! # Engine limits are not parameter bounds
//! [`MAX_OP_ELEMENTS`](crate::shape_ops::MAX_OP_ELEMENTS) bounds the source and output element count
//! of one evaluation, [`MAX_POINTS_PER_SEGMENT`](crate::shape_ops::MAX_POINTS_PER_SEGMENT) the zig zag
//! ridges per segment and [`COORDINATE_LIMIT`](crate::COORDINATE_LIMIT) every value and result coordinate. They are resource
//! limits of this engine (refused as `output_over_budget` / `beyond_engine_range` before
//! allocation), not parameter limits: STU-VEC-100 forbids inventing parameter bounds, and the
//! descriptors keep every unrecovered bound `None`.
#![deny(clippy::arithmetic_side_effects)]
use std::collections::BTreeMap;

use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{
    Error as ObserveError, FailureCode, Observation, Observe, Outcome, Receipt, SinkPort,
};

use crate::owned_path::{OwnedPath, PathLimits, PathPoint};
use crate::path_contract::Error;
use crate::shape_ops::MAX_OP_ELEMENTS;

mod laws;

/// Version stamp of every record this module writes; it names the law set (module docs).
pub const RECORD_VERSION: u32 = 1;
/// Prefix of every effect id.
pub const EFFECT_ID_PREFIX: &str = "hsk.studio.vector_effect.";
/// `per_in` conversion: points per inch.
pub const PT_PER_INCH: f64 = 72.0;
/// Free distort: largest distance between a mapped cubic and the exact projective image, at the
/// three probe parameters, before subdivision stops (unless the depth cap is reached).
pub const FREE_DISTORT_TOLERANCE_PT: f64 = 0.01;
/// Free distort: deepest halving of one source cubic (at most 1024 pieces).
pub const FREE_DISTORT_MAX_DEPTH: u32 = 10;

// ---------------------------------------------------------------------------------------------
// Refusals

/// Typed refusal; [`LiveEffectError::code`] is the stable wire code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveEffectError {
    UnknownEffect,
    UnsupportedVersion,
    UnknownParameter,
    ParameterNotRecovered,
    UntypedWrite,
    MalformedValue,
    WrongParameterType,
    NonFinite,
    OutOfHardRange,
    NotACount,
    UnknownEnumMember,
    ParameterUnset,
    InvalidGeometry,
    DegenerateExtent,
    DegenerateQuad,
    SelfCrossingQuad,
    QuadNotConvex,
    PointBeyondHorizon,
    BeyondEngineRange,
    OutputOverBudget,
    Canceled,
    ConvertToShapeSizingLawNotRecovered,
    ObjectMosaicGridLawNotRecovered,
}

impl LiveEffectError {
    /// Stable code.
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnknownEffect => "unknown_effect",
            Self::UnsupportedVersion => "unsupported_version",
            Self::UnknownParameter => "unknown_parameter",
            Self::ParameterNotRecovered => "parameter_not_recovered",
            Self::UntypedWrite => "untyped_write",
            Self::MalformedValue => "malformed_value",
            Self::WrongParameterType => "wrong_parameter_type",
            Self::NonFinite => "non_finite",
            Self::OutOfHardRange => "out_of_hard_range",
            Self::NotACount => "not_a_count",
            Self::UnknownEnumMember => "unknown_enum_member",
            Self::ParameterUnset => "parameter_unset",
            Self::InvalidGeometry => "invalid_geometry",
            Self::DegenerateExtent => "degenerate_extent",
            Self::DegenerateQuad => "degenerate_quad",
            Self::SelfCrossingQuad => "self_crossing_quad",
            Self::QuadNotConvex => "quad_not_convex",
            Self::PointBeyondHorizon => "point_beyond_horizon",
            Self::BeyondEngineRange => "beyond_engine_range",
            Self::OutputOverBudget => "output_over_budget",
            Self::Canceled => "canceled",
            Self::ConvertToShapeSizingLawNotRecovered => "convert_to_shape_sizing_law_not_recovered",
            Self::ObjectMosaicGridLawNotRecovered => "object_mosaic_grid_law_not_recovered",
        }
    }
}

impl std::fmt::Display for LiveEffectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for LiveEffectError {}

type Result<T> = std::result::Result<T, LiveEffectError>;

// ---------------------------------------------------------------------------------------------
// Parameter descriptors (STU-VEC-100, STU-VEC-101, STU-VEC-104)

/// The stored type vocabulary (STU-VEC-154).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamType {
    Int,
    Real,
    Bool,
    String,
}

impl ParamType {
    /// The stored type token.
    pub const fn token(self) -> &'static str {
        match self {
            Self::Int => "Int",
            Self::Real => "Real",
            Self::Bool => "Bool",
            Self::String => "String",
        }
    }
    fn from_token(token: &str) -> Option<Self> {
        [Self::Int, Self::Real, Self::Bool, Self::String].into_iter().find(|t| t.token() == token)
    }
}

/// A typed parameter value.
#[derive(Clone, Debug, PartialEq)]
pub enum ParamValue {
    Int(i64),
    Real(f64),
    Bool(bool),
    String(String),
}

impl ParamValue {
    pub const fn param_type(&self) -> ParamType {
        match self {
            Self::Int(_) => ParamType::Int,
            Self::Real(_) => ParamType::Real,
            Self::Bool(_) => ParamType::Bool,
            Self::String(_) => ParamType::String,
        }
    }
}

/// Unit token (STU-VEC-101); `Context` names the sibling mode control that selects the unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Pt,
    DocumentUnit,
    Percent,
    Deg,
    Count,
    PerIn,
    Context(&'static str),
}

impl Unit {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Pt => "pt",
            Self::DocumentUnit => "document_unit",
            Self::Percent => "percent",
            Self::Deg => "deg",
            Self::Count => "count",
            Self::PerIn => "per_in",
            Self::Context(_) => "context",
        }
    }
}

/// The seven independent numeric fields; `None` = `unknown`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NumericSpec {
    pub ty: ParamType,
    pub hard_min: Option<f64>,
    pub hard_max: Option<f64>,
    pub soft_min: Option<f64>,
    pub soft_max: Option<f64>,
    pub default: Option<f64>,
    pub unit: Unit,
    pub precision: Option<u8>,
}

impl NumericSpec {
    fn admit(&self, v: f64) -> Result<()> {
        if !v.is_finite() {
            return Err(LiveEffectError::NonFinite);
        }
        if self.hard_min.is_some_and(|m| v < m) || self.hard_max.is_some_and(|m| v > m) {
            return Err(LiveEffectError::OutOfHardRange);
        }
        if self.unit == Unit::Count && (v < 0.0 || v.fract() != 0.0) {
            return Err(LiveEffectError::NotACount);
        }
        Ok(())
    }
}

/// A captured enumeration value (STU-VEC-104), when the capture recorded one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Captured {
    Bool(bool),
    Token(&'static str),
}

/// An enumeration member: stable Studio id plus the captured value (`None` = not recorded).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnumMember {
    pub id: &'static str,
    pub captured: Option<Captured>,
}

/// An enumerated parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnumSpec {
    pub members: &'static [EnumMember],
    pub default: Option<&'static str>,
    /// True when the default is a Studio decision rather than a captured value.
    pub default_is_studio_decision: bool,
}

/// What kind of value a key holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamKind {
    Numeric(NumericSpec),
    Enum(EnumSpec),
    Bool { default: Option<bool> },
    /// Explicit integer seed (STU-FX-011); the bits are used as `u64`.
    Seed { default: i64 },
}

/// Where a row comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provenance {
    /// Copied from the named spec clause.
    Recovered(&'static str),
    /// No recovered record: a Studio declaration.
    StudioDeclared,
}

/// One parameter row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamSpec {
    pub key: &'static str,
    pub kind: ParamKind,
    pub provenance: Provenance,
}

impl ParamSpec {
    pub const fn param_type(&self) -> ParamType {
        match self.kind {
            ParamKind::Numeric(n) => n.ty,
            ParamKind::Enum(_) => ParamType::String,
            ParamKind::Bool { .. } => ParamType::Bool,
            ParamKind::Seed { .. } => ParamType::Int,
        }
    }
    fn default_value(&self) -> Option<ParamValue> {
        match self.kind {
            ParamKind::Numeric(n) => n.default.map(|d| match n.ty {
                ParamType::Int => ParamValue::Int(d as i64),
                _ => ParamValue::Real(d),
            }),
            ParamKind::Enum(e) => e.default.map(|d| ParamValue::String(d.to_owned())),
            ParamKind::Bool { default } => default.map(ParamValue::Bool),
            ParamKind::Seed { default } => Some(ParamValue::Int(default)),
        }
    }
}

const STU155: Provenance = Provenance::Recovered("STU-VEC-155");
const STU158: Provenance = Provenance::Recovered("STU-VEC-158");
const DECLARED: Provenance = Provenance::StudioDeclared;

#[allow(clippy::too_many_arguments)]
const fn numeric(
    key: &'static str,
    ty: ParamType,
    bounds: [Option<f64>; 4],
    default: Option<f64>,
    unit: Unit,
    precision: Option<u8>,
    provenance: Provenance,
) -> ParamSpec {
    ParamSpec {
        key,
        kind: ParamKind::Numeric(NumericSpec {
            ty,
            hard_min: bounds[0],
            hard_max: bounds[1],
            soft_min: bounds[2],
            soft_max: bounds[3],
            default,
            unit,
            precision,
        }),
        provenance,
    }
}

const fn real(key: &'static str, bounds: [Option<f64>; 4], default: Option<f64>, unit: Unit, precision: Option<u8>, provenance: Provenance) -> ParamSpec {
    numeric(key, ParamType::Real, bounds, default, unit, precision, provenance)
}

const fn enumeration(key: &'static str, members: &'static [EnumMember], default: Option<&'static str>, studio_decision: bool, provenance: Provenance) -> ParamSpec {
    ParamSpec { key, kind: ParamKind::Enum(EnumSpec { members, default, default_is_studio_decision: studio_decision }), provenance }
}

const fn flag(key: &'static str, default: Option<bool>, provenance: Provenance) -> ParamSpec {
    ParamSpec { key, kind: ParamKind::Bool { default }, provenance }
}

const SEED: ParamSpec = ParamSpec { key: "seed", kind: ParamKind::Seed { default: 0 }, provenance: DECLARED };
const UNKNOWN4: [Option<f64>; 4] = [None, None, None, None];

const fn member(id: &'static str) -> EnumMember {
    EnumMember { id, captured: None }
}

const MODE_CAPTURED: &[EnumMember] = &[
    EnumMember { id: "mode.relative", captured: Some(Captured::Bool(false)) },
    EnumMember { id: "mode.absolute", captured: Some(Captured::Bool(true)) },
];
const MODE: &[EnumMember] = &[member("mode.relative"), member("mode.absolute")];
const POINTS: &[EnumMember] = &[member("points.smooth"), member("points.corner")];

const ROUGHEN: &[ParamSpec] = &[
    real("roughen_size_absolute", [Some(0.0), Some(7200.0), Some(0.0), Some(100.0)], Some(1.0), Unit::DocumentUnit, Some(2), STU155),
    real("roughen_size_relative", [None, None, Some(0.0), Some(100.0)], Some(1.0), Unit::Percent, Some(2), STU155),
    real("roughen_detail", [None, None, Some(0.0), Some(100.0)], Some(1.0), Unit::PerIn, Some(2), STU155),
    enumeration("roughen_mode", MODE_CAPTURED, Some("mode.relative"), true, STU155),
    enumeration("roughen_points", POINTS, None, false, STU155),
    SEED,
];

const ZIG_ZAG: &[ParamSpec] = &[
    real("zigzag_size", UNKNOWN4, None, Unit::Context("zigzag_mode"), Some(2), STU155),
    real("zigzag_ridges_per_segment", UNKNOWN4, None, Unit::Count, Some(2), STU155),
    enumeration("zigzag_mode", MODE, None, false, STU155),
    enumeration("zigzag_points", POINTS, None, false, STU155),
];

const PUCKER_BLOAT: &[ParamSpec] = &[real("pucker_bloat_factor", UNKNOWN4, None, Unit::Percent, Some(2), STU155)];

const TWIST: &[ParamSpec] = &[real("twist_angle", UNKNOWN4, None, Unit::Deg, None, STU155)];

const TWEAK: &[ParamSpec] = &[
    real("tweak_horizontal", [None, None, Some(0.0), Some(100.0)], Some(10.0), Unit::Context("tweak_mode"), Some(2), STU155),
    real("tweak_vertical", [None, None, Some(0.0), Some(100.0)], Some(10.0), Unit::Context("tweak_mode"), Some(2), STU155),
    enumeration("tweak_mode", MODE_CAPTURED, None, false, STU155),
    flag("modify_anchor_points", Some(true), STU155),
    flag("modify_in_control_points", Some(true), STU155),
    flag("modify_out_control_points", Some(true), STU155),
    SEED,
];

const fn corner(key: &'static str) -> ParamSpec {
    real(key, UNKNOWN4, None, Unit::DocumentUnit, None, STU155)
}

/// Free distort corner keys, `[corner][h, v]` for the source then the destination quad.
pub const FREE_DISTORT_KEYS: [[[&str; 2]; 4]; 2] = [
    [["src0_h", "src0_v"], ["src1_h", "src1_v"], ["src2_h", "src2_v"], ["src3_h", "src3_v"]],
    [["dst0_h", "dst0_v"], ["dst1_h", "dst1_v"], ["dst2_h", "dst2_v"], ["dst3_h", "dst3_v"]],
];

const FREE_DISTORT: &[ParamSpec] = &[
    corner("src0_h"), corner("src0_v"), corner("src1_h"), corner("src1_v"),
    corner("src2_h"), corner("src2_v"), corner("src3_h"), corner("src3_v"),
    corner("dst0_h"), corner("dst0_v"), corner("dst1_h"), corner("dst1_v"),
    corner("dst2_h"), corner("dst2_v"), corner("dst3_h"), corner("dst3_v"),
];

const REFERENCE: &[EnumMember] = &[
    member("ref.top_left"), member("ref.top"), member("ref.top_right"),
    member("ref.left"), member("ref.centre"), member("ref.right"),
    member("ref.bottom_left"), member("ref.bottom"), member("ref.bottom_right"),
];

const STEP_REPEAT: &[ParamSpec] = &[
    real("move_horizontal", UNKNOWN4, Some(0.0), Unit::DocumentUnit, None, DECLARED),
    real("move_vertical", UNKNOWN4, Some(0.0), Unit::DocumentUnit, None, DECLARED),
    real("scale_horizontal", UNKNOWN4, Some(100.0), Unit::Percent, None, DECLARED),
    real("scale_vertical", UNKNOWN4, Some(100.0), Unit::Percent, None, DECLARED),
    real("rotate", UNKNOWN4, Some(0.0), Unit::Deg, None, DECLARED),
    flag("reflect_x", Some(false), DECLARED),
    flag("reflect_y", Some(false), DECLARED),
    enumeration("reference_point", REFERENCE, Some("ref.centre"), true, DECLARED),
    numeric("copies", ParamType::Int, UNKNOWN4, Some(0.0), Unit::Count, Some(0), DECLARED),
    flag("random", Some(false), DECLARED),
    SEED,
];

const CONVERT_TO_SHAPE: &[ParamSpec] = &[
    enumeration(
        "target_shape",
        &[
            EnumMember { id: "shape.rectangle", captured: Some(Captured::Token("Rectangle")) },
            EnumMember { id: "shape.rounded_rectangle", captured: Some(Captured::Token("RoundedRectangle")) },
            EnumMember { id: "shape.ellipse", captured: Some(Captured::Token("Ellipse")) },
        ],
        None,
        false,
        STU158,
    ),
    enumeration(
        "sizing_mode",
        &[
            EnumMember { id: "sizing.absolute", captured: Some(Captured::Token("Absolute")) },
            EnumMember { id: "sizing.relative", captured: Some(Captured::Token("Relative")) },
        ],
        Some("sizing.absolute"),
        false,
        STU158,
    ),
];

const OBJECT_MOSAIC: &[ParamSpec] = &[
    enumeration("constrain_axis", &[member("axis.width"), member("axis.height")], None, false, STU158),
    enumeration("tile_colour_mode", &[member("mosaic.colour"), member("mosaic.gray")], None, false, STU158),
    flag("resize_using_percentages", None, STU158),
    flag("delete_source_raster", None, STU158),
];

/// The live-effect families this module records (STU-VEC-154 Distort, Convert to shape, Create
/// object mosaic, and the STU-FX-028 Transform (step-repeat) row).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EffectKind {
    Roughen,
    ZigZag,
    PuckerBloat,
    Twist,
    Tweak,
    FreeDistort,
    StepRepeat,
    ConvertToShape,
    ObjectMosaic,
}

impl EffectKind {
    pub const ALL: [Self; 9] = [
        Self::Roughen,
        Self::ZigZag,
        Self::PuckerBloat,
        Self::Twist,
        Self::Tweak,
        Self::FreeDistort,
        Self::StepRepeat,
        Self::ConvertToShape,
        Self::ObjectMosaic,
    ];

    /// Stable effect id.
    pub const fn effect_id(self) -> &'static str {
        match self {
            Self::Roughen => "hsk.studio.vector_effect.roughen",
            Self::ZigZag => "hsk.studio.vector_effect.zig_zag",
            Self::PuckerBloat => "hsk.studio.vector_effect.pucker_bloat",
            Self::Twist => "hsk.studio.vector_effect.twist",
            Self::Tweak => "hsk.studio.vector_effect.tweak",
            Self::FreeDistort => "hsk.studio.vector_effect.free_distort",
            Self::StepRepeat => "hsk.studio.vector_effect.step_repeat",
            Self::ConvertToShape => "hsk.studio.vector_effect.convert_to_shape",
            Self::ObjectMosaic => "hsk.studio.vector_effect.object_mosaic",
        }
    }

    pub fn from_effect_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.effect_id() == id)
    }

    /// The record's parameter rows, in stored order.
    pub const fn params(self) -> &'static [ParamSpec] {
        match self {
            Self::Roughen => ROUGHEN,
            Self::ZigZag => ZIG_ZAG,
            Self::PuckerBloat => PUCKER_BLOAT,
            Self::Twist => TWIST,
            Self::Tweak => TWEAK,
            Self::FreeDistort => FREE_DISTORT,
            Self::StepRepeat => STEP_REPEAT,
            Self::ConvertToShape => CONVERT_TO_SHAPE,
            Self::ObjectMosaic => OBJECT_MOSAIC,
        }
    }

    pub fn param(self, key: &str) -> Option<&'static ParamSpec> {
        self.params().iter().find(|p| p.key == key)
    }

    /// Where the record comes from.
    pub const fn provenance(self) -> Provenance {
        match self {
            Self::StepRepeat => Provenance::StudioDeclared,
            Self::ConvertToShape | Self::ObjectMosaic => STU158,
            _ => STU155,
        }
    }

    /// Parameters the capture names but did not recover (STU-VEC-158 declared gaps); a write is
    /// refused `parameter_not_recovered`.
    pub const fn not_recovered(self) -> &'static [&'static str] {
        match self {
            Self::ConvertToShape => &["width", "height", "extra_width", "extra_height"],
            Self::ObjectMosaic => &["tile_counts", "tile_spacing", "resize_numerics"],
            _ => &[],
        }
    }

    /// False when evaluation is refused because the law is not recovered.
    pub const fn evaluates(self) -> bool {
        !matches!(self, Self::ConvertToShape | Self::ObjectMosaic)
    }
}

// ---------------------------------------------------------------------------------------------
// Records

/// A typed live-effect record: effect id, version stamp and ordered typed parameter map.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectRecord {
    kind: EffectKind,
    version: u32,
    values: BTreeMap<&'static str, ParamValue>,
}

impl EffectRecord {
    /// A fresh record holding only the KNOWN defaults (unknown defaults stay unset).
    pub fn new(kind: EffectKind) -> Self {
        let values = kind.params().iter().filter_map(|p| p.default_value().map(|v| (p.key, v))).collect();
        Self { kind, version: RECORD_VERSION, values }
    }

    /// Decodes a stored record: known effect id, supported version, every entry through [`Self::set`].
    pub fn from_parts(effect_id: &str, version: u32, entries: &[(&str, ParamValue)]) -> Result<Self> {
        let kind = EffectKind::from_effect_id(effect_id).ok_or(LiveEffectError::UnknownEffect)?;
        if version != RECORD_VERSION {
            return Err(LiveEffectError::UnsupportedVersion);
        }
        let mut record = Self::new(kind);
        for (key, value) in entries {
            record.set(key, value.clone())?;
        }
        Ok(record)
    }

    pub const fn kind(&self) -> EffectKind {
        self.kind
    }
    pub const fn effect_id(&self) -> &'static str {
        self.kind.effect_id()
    }
    pub const fn version(&self) -> u32 {
        self.version
    }
    pub fn get(&self, key: &str) -> Option<&ParamValue> {
        self.values.get(key)
    }
    /// Entries in stable key order.
    pub fn entries(&self) -> impl Iterator<Item = (&'static str, &ParamValue)> {
        self.values.iter().map(|(k, v)| (*k, v))
    }

    /// Typed write (module docs): refused, never clamped.
    pub fn set(&mut self, key: &str, value: ParamValue) -> Result<()> {
        let spec = self.kind.param(key).ok_or(if self.kind.not_recovered().contains(&key) {
            LiveEffectError::ParameterNotRecovered
        } else {
            LiveEffectError::UnknownParameter
        })?;
        if value.param_type() != spec.param_type() {
            return Err(LiveEffectError::WrongParameterType);
        }
        match (&spec.kind, &value) {
            (ParamKind::Numeric(n), ParamValue::Real(v)) => n.admit(*v)?,
            (ParamKind::Numeric(n), ParamValue::Int(v)) => n.admit(*v as f64)?,
            (ParamKind::Enum(e), ParamValue::String(s)) => {
                if !e.members.iter().any(|m| m.id == s) {
                    return Err(LiveEffectError::UnknownEnumMember);
                }
            }
            (ParamKind::Bool { .. }, ParamValue::Bool(_)) | (ParamKind::Seed { .. }, ParamValue::Int(_)) => {}
            _ => return Err(LiveEffectError::WrongParameterType),
        }
        self.values.insert(spec.key, value);
        Ok(())
    }

    /// A stored `(value, type, key)` triple (STU-VEC-154): the type token must be one of `Int`,
    /// `Real`, `Bool`, `String` (else `untyped_write`); `Bool` text is `true` / `false`.
    pub fn set_triple(&mut self, value: &str, type_token: &str, key: &str) -> Result<()> {
        let ty = ParamType::from_token(type_token).ok_or(LiveEffectError::UntypedWrite)?;
        let typed = match ty {
            ParamType::Int => ParamValue::Int(value.parse().map_err(|_| LiveEffectError::MalformedValue)?),
            ParamType::Real => ParamValue::Real(value.parse().map_err(|_| LiveEffectError::MalformedValue)?),
            ParamType::Bool => match value {
                "true" => ParamValue::Bool(true),
                "false" => ParamValue::Bool(false),
                _ => return Err(LiveEffectError::MalformedValue),
            },
            ParamType::String => ParamValue::String(value.to_owned()),
        };
        self.set(key, typed)
    }

    pub fn set_real(&mut self, key: &str, v: f64) -> Result<()> {
        self.set(key, ParamValue::Real(v))
    }
    pub fn set_int(&mut self, key: &str, v: i64) -> Result<()> {
        self.set(key, ParamValue::Int(v))
    }
    pub fn set_bool(&mut self, key: &str, v: bool) -> Result<()> {
        self.set(key, ParamValue::Bool(v))
    }
    pub fn set_token(&mut self, key: &str, v: &str) -> Result<()> {
        self.set(key, ParamValue::String(v.to_owned()))
    }

    /// Free distort: writes the typed quad-to-quad correspondence (`[x, y]` per corner).
    pub fn set_free_distort_frame(&mut self, src: [[f64; 2]; 4], dst: [[f64; 2]; 4]) -> Result<()> {
        if self.kind != EffectKind::FreeDistort {
            return Err(LiveEffectError::UnknownParameter);
        }
        let mut next = self.clone();
        for (quad, keys) in [src, dst].iter().zip(FREE_DISTORT_KEYS) {
            for (point, [kh, kv]) in quad.iter().zip(keys) {
                next.set_real(kh, point[0])?;
                next.set_real(kv, point[1])?;
            }
        }
        *self = next;
        Ok(())
    }

    fn value(&self, key: &str) -> Result<&ParamValue> {
        self.values.get(key).ok_or(LiveEffectError::ParameterUnset)
    }
    fn real(&self, key: &str) -> Result<f64> {
        match self.value(key)? {
            ParamValue::Real(v) => Ok(*v),
            _ => Err(LiveEffectError::WrongParameterType),
        }
    }
    fn int(&self, key: &str) -> Result<i64> {
        match self.value(key)? {
            ParamValue::Int(v) => Ok(*v),
            _ => Err(LiveEffectError::WrongParameterType),
        }
    }
    fn flag(&self, key: &str) -> Result<bool> {
        match self.value(key)? {
            ParamValue::Bool(v) => Ok(*v),
            _ => Err(LiveEffectError::WrongParameterType),
        }
    }
    fn token(&self, key: &str) -> Result<&str> {
        match self.value(key)? {
            ParamValue::String(v) => Ok(v),
            _ => Err(LiveEffectError::WrongParameterType),
        }
    }
    fn seed(&self) -> Result<u64> {
        Ok(u64::from_ne_bytes(self.int("seed")?.to_ne_bytes()))
    }
    fn quad(&self, which: usize) -> Result<[PathPoint; 4]> {
        let keys = FREE_DISTORT_KEYS.get(which).ok_or(LiveEffectError::UnknownParameter)?;
        let mut q = [PathPoint::new(0.0, 0.0); 4];
        for (slot, [kh, kv]) in q.iter_mut().zip(keys) {
            *slot = PathPoint::new(self.real(kh)?, self.real(kv)?);
        }
        Ok(q)
    }
}

// ---------------------------------------------------------------------------------------------
// Evaluation

/// Evaluates `record` over `sources` (module docs). Pure: the sources are borrowed and never
/// mutated; one result per source (step-repeat: the sources, then each copy of every source).
pub fn evaluate(sources: &[OwnedPath], record: &EffectRecord, cancel: &CancellationToken) -> Result<Vec<OwnedPath>> {
    check_cancel(cancel)?;
    match record.kind {
        EffectKind::ConvertToShape => return Err(LiveEffectError::ConvertToShapeSizingLawNotRecovered),
        EffectKind::ObjectMosaic => return Err(LiveEffectError::ObjectMosaicGridLawNotRecovered),
        _ => {}
    }
    if record.version != RECORD_VERSION {
        return Err(LiveEffectError::UnsupportedVersion);
    }
    let mut total = 0_usize;
    for s in sources {
        s.validate(&PathLimits { max_elements: MAX_OP_ELEMENTS, require_closed: false }).map_err(|e| match e {
            Error::BudgetExceeded => LiveEffectError::OutputOverBudget,
            _ => LiveEffectError::InvalidGeometry,
        })?;
        total = total.checked_add(s.elements.len()).ok_or(LiveEffectError::OutputOverBudget)?;
        if total > MAX_OP_ELEMENTS {
            return Err(LiveEffectError::OutputOverBudget);
        }
    }
    match record.kind {
        EffectKind::Roughen => laws::roughen(sources, record, cancel),
        EffectKind::ZigZag => laws::zig_zag_live(sources, record, cancel),
        EffectKind::PuckerBloat => laws::pucker_bloat_live(sources, record, cancel),
        EffectKind::Twist => laws::twist_live(sources, record, cancel),
        EffectKind::Tweak => laws::tweak(sources, record, cancel),
        EffectKind::FreeDistort => laws::free_distort(sources, record, cancel),
        EffectKind::StepRepeat => laws::step_repeat(sources, record, cancel),
        EffectKind::ConvertToShape => Err(LiveEffectError::ConvertToShapeSizingLawNotRecovered),
        EffectKind::ObjectMosaic => Err(LiveEffectError::ObjectMosaicGridLawNotRecovered),
    }
}

fn check_cancel(cancel: &CancellationToken) -> Result<()> {
    cancel.check().map_err(|_| LiveEffectError::Canceled)
}

// ---------------------------------------------------------------------------------------------
// Outcomes (codes only)

/// The observe outcome of an evaluation (deliver with [`report`]).
pub fn outcome<T>(result: &std::result::Result<T, LiveEffectError>) -> Outcome {
    match result {
        Ok(_) => Outcome::Success,
        Err(LiveEffectError::Canceled) => Outcome::Canceled,
        Err(LiveEffectError::ConvertToShapeSizingLawNotRecovered | LiveEffectError::ObjectMosaicGridLawNotRecovered) => {
            Outcome::Failure(FailureCode::Unsupported)
        }
        Err(LiveEffectError::OutputOverBudget | LiveEffectError::BeyondEngineRange) => Outcome::Failure(FailureCode::Unavailable),
        Err(_) => Outcome::Failure(FailureCode::Validation),
    }
}

/// Delivers one terminal outcome through the caller-owned sink (no project text is sent).
pub fn report(
    outcome: Outcome,
    correlation_id: u64,
    revision: u64,
    observe: &mut Observe,
    cancel: &CancellationToken,
    sink: &mut (impl SinkPort + ?Sized),
) -> std::result::Result<Receipt, ObserveError> {
    observe.emit(
        Observation { correlation_id, revision, outcome, progress: None, private_project_text: None },
        cancel,
        sink,
    )
}
