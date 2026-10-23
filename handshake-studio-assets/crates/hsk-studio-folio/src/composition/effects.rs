//! Per-layer effect stack of a composition layer (lane SMB-AE-EFFECT-STACK-1; parity AE-17). Data
//! only: composite lowers the stack into the render and evaluates it.
//!
//! Spec basis (module 14, v02.221):
//! - [STU-FX-001] "A filter or effect applied to any `StudioLayer` is recorded as a
//!   `StudioLiveFilter` node inside that layer's `StudioEffectStack` ..., never as a destructive
//!   pixel/geometry mutation of the layer's source data."
//! - [STU-FX-002] "A `StudioEffectStack` is an ordered list of `StudioLiveFilter` entries. Each entry
//!   carries: `filter_kind` (catalog key ...), typed parameter payload, an optional `StudioMask`
//!   reference ..., a `StudioBlendMode` + opacity for how the entry composes onto the pre-effect
//!   result, an `enabled` flag, and an `render_scope` (`layer` | `below` | `group`)".
//!   [`StudioCompositionLayer::effects`](super::StudioCompositionLayer::effects) is that ordered list
//!   (render order = list order, [STU-CMP-010] step 3 "Evaluate the `StudioEffectStack` in stack
//!   order"); [`StudioCompositionEffect`] carries `filter_kind`, the typed values, `enabled` and the
//!   entry opacity. NOT modelled here (declared gaps, recorded in the lane file): the entry's own
//!   mask reference ([STU-FX-004]), an entry blend mode other than normal ([STU-FX-008]) and
//!   `render_scope` other than `layer`.
//! - [STU-FX-103]/[STU-FX-104]/[STU-FX-106]/[STU-FX-107]: every parameter of a kind has one
//!   [`StudioEffectParamSpec`] row with the four bounds as SEPARATE optional fields and its
//!   `bound_state`; "A value outside `[hard_min, hard_max]` is an ERROR, not a clamp": a static value
//!   or keyframe value outside a declared hard bound is refused `InvalidField` with the target
//!   `{layer}/effects/{effect_id}/{param_key}/effect_param_out_of_range` (Folio's `Code` enum is
//!   matched exhaustively by dependents, so the spec's `EFFECT_PARAM_OUT_OF_RANGE` result is the
//!   target segment). Soft bounds are presentation only and never refused ([STU-FX-105]).
//! - [STU-FX-112] kinds: `scalar`, `angle`, `point_2d` ("stored as fractions of layer size by
//!   default"), `color` (4 channels), `enumeration` ("1-based index into `options`").
//! - [STU-FX-116a] "Enumerated option indices are STABLE": an enumeration value must be one of the
//!   recovered indices of [`StudioEffectParamSpec::options`] (gaps in the source numbering kept).
//! - [STU-FX-131] parameter records of 14.9.4, reproduced row for row for the implemented kinds:
//!   Distort `Twirl` (`ADBE Twirl`), `Spherize` (`ADBE Spherize`), `Offset` (`ADBE Offset`),
//!   `Polar Coordinates` (`ADBE Polar Coordinates`); Generate `Gradient Ramp` (`ADBE Ramp`),
//!   `Circle` (`ADBE Circle`), `Checkerboard` (`ADBE Checkerboard`) (catalogue [STU-FX-130] Distort
//!   and Generate tables). `LAYER`, `GROUP_START`/`GROUP_END`, `BUTTON` and unnamed `CHECKBOX`
//!   rows carry no stored value here (the unnamed rows have no recovered name to key them by).
//!
//! Studio readings, declared (UNVERIFIED, D5 oracle):
//! - Twirl `Angle` is recorded as `ANGLE` with hard and soft ranges `0..0`, precision 0, default
//!   0; a hard range admitting only 0 contradicts the effect's purpose and [STU-FX-112] `angle`
//!   "wraps rather than clamps unless a hard bound is declared", so the row is read as the
//!   capture's no-range encoding: `bound_state = unbounded_in_source`, all four bounds `None`.
//! - Colour parameters carry no `StudioColorProfile` reference ([STU-FX-118] requires one); their
//!   four channels are straight RGBA in the composition plan's colour tag (the gap is recorded).
//!   Defaults are the record's `ARGB #AARRGGBB` values divided by 255.
//! - Percent-unit rows with hard range `0..1` (`Blend With Original`, `Interpolation`) store the
//!   fraction as recorded; rows with hard range `0..100` store percent.
//!
//! Folio hard ceilings (not spec bounds): [`MAX_LAYER_EFFECTS`] entries per layer ([STU-FX-006b]
//! "no fixed per-kind instance limit beyond a document-level performance budget"), effect ids are
//! stack-local keys `[A-Za-z0-9_-]{1,64}`, values finite and within `MAX_STREAM_ABS_VALUE`.
//!
//! Refusals (typed, V4-01/02): `InvalidField` with the record path, `DuplicateIdentity` for a
//! repeated effect id, `BudgetExceeded` for counts; integer arithmetic is denied.
#![deny(clippy::arithmetic_side_effects)]
use super::{MAX_STREAM_ABS_VALUE, StudioInterpolation, StudioKeyedValue, check, validate_keyed};
use crate::values::Ctx;
use crate::{Code, Result, err};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Effect entries per composition layer (a Folio hard ceiling).
pub const MAX_LAYER_EFFECTS: usize = 128;
/// Longest effect id.
pub const MAX_EFFECT_ID_LEN: usize = 64;
/// Target segment of a value outside a declared hard bound ([STU-FX-104]).
pub const EFFECT_PARAM_OUT_OF_RANGE: &str = "effect_param_out_of_range";

/// The implemented composition effect kinds (`filter_kind`, [STU-FX-130] catalogue rows).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioCompositionEffectKind {
    /// Distort `Twirl` (`ADBE Twirl`).
    Twirl,
    /// Distort `Spherize` (`ADBE Spherize`).
    Spherize,
    /// Distort `Offset` (`ADBE Offset`).
    Offset,
    /// Distort `Polar Coordinates` (`ADBE Polar Coordinates`).
    PolarCoordinates,
    /// Generate `Gradient Ramp` (`ADBE Ramp`).
    GradientRamp,
    /// Generate `Circle` (`ADBE Circle`).
    Circle,
    /// Generate `Checkerboard` (`ADBE Checkerboard`).
    Checkerboard,
}

/// [STU-FX-126] category of a kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudioEffectCategory {
    Distort,
    Generate,
}

impl StudioCompositionEffectKind {
    /// Every implemented kind.
    pub const ALL: [Self; 7] =
        [Self::Twirl, Self::Spherize, Self::Offset, Self::PolarCoordinates, Self::GradientRamp, Self::Circle, Self::Checkerboard];
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Twirl => "twirl",
            Self::Spherize => "spherize",
            Self::Offset => "offset",
            Self::PolarCoordinates => "polar_coordinates",
            Self::GradientRamp => "gradient_ramp",
            Self::Circle => "circle",
            Self::Checkerboard => "checkerboard",
        }
    }
    pub const fn category(self) -> StudioEffectCategory {
        match self {
            Self::Twirl | Self::Spherize | Self::Offset | Self::PolarCoordinates => StudioEffectCategory::Distort,
            Self::GradientRamp | Self::Circle | Self::Checkerboard => StudioEffectCategory::Generate,
        }
    }
    /// Source identifier, provenance for round-trip only ([STU-FX-103] `import_key`).
    pub const fn import_key(self) -> &'static str {
        match self {
            Self::Twirl => "ADBE Twirl",
            Self::Spherize => "ADBE Spherize",
            Self::Offset => "ADBE Offset",
            Self::PolarCoordinates => "ADBE Polar Coordinates",
            Self::GradientRamp => "ADBE Ramp",
            Self::Circle => "ADBE Circle",
            Self::Checkerboard => "ADBE Checkerboard",
        }
    }
    /// The stored parameter rows of the kind ([STU-FX-131] tables), in source order.
    pub const fn params(self) -> &'static [StudioEffectParamSpec] {
        match self {
            Self::Twirl => TWIRL,
            Self::Spherize => SPHERIZE,
            Self::Offset => OFFSET,
            Self::PolarCoordinates => POLAR_COORDINATES,
            Self::GradientRamp => GRADIENT_RAMP,
            Self::Circle => CIRCLE,
            Self::Checkerboard => CHECKERBOARD,
        }
    }
    /// The row of `param_key`.
    pub fn param(self, param_key: &str) -> Option<&'static StudioEffectParamSpec> {
        self.params().iter().find(|p| p.param_key == param_key)
    }
}

/// [STU-FX-112] parameter kinds used by the implemented effects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudioParameterKind {
    Scalar,
    Angle,
    Point2d,
    Color,
    Enumeration,
}
impl StudioParameterKind {
    /// Components of a stored value.
    pub const fn components(self) -> usize {
        match self {
            Self::Scalar | Self::Angle | Self::Enumeration => 1,
            Self::Point2d => 2,
            Self::Color => 4,
        }
    }
}

/// [STU-FX-106] provenance of the four bound fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudioBoundState {
    DeclaredBoth,
    DeclaredHardOnly,
    DeclaredSoftOnly,
    UnboundedInSource,
    ObservedOnly,
}

/// [STU-FX-108] unit tokens used by the implemented rows (`--` in the source is `unitless`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudioUnitToken {
    Percent,
    Degrees,
    Unitless,
}

/// One parameter row ([STU-FX-103] fields relevant to storage and validation). The four bounds are
/// separate fields ([STU-FX-107]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StudioEffectParamSpec {
    pub param_key: &'static str,
    /// Row number of the [STU-FX-131] table.
    pub param_index: u16,
    pub display_name: &'static str,
    pub kind: StudioParameterKind,
    pub hard_min: Option<f64>,
    pub hard_max: Option<f64>,
    pub soft_min: Option<f64>,
    pub soft_max: Option<f64>,
    pub bound_state: StudioBoundState,
    /// The default value: fractions of the layer size for `point_2d`, straight RGBA `0..1` for
    /// `color`, the option index for `enumeration`.
    pub default: &'static [f64],
    pub unit: StudioUnitToken,
    pub precision: Option<u8>,
    pub animatable: bool,
    pub interpolable: bool,
    /// `(index, label)` of an enumeration (1-based, source gaps kept).
    pub options: &'static [(u16, &'static str)],
}

impl StudioEffectParamSpec {
    /// Whether `v` lies inside the declared hard bounds (an undeclared bound does not constrain).
    pub fn in_hard_range(&self, v: f64) -> bool {
        self.hard_min.is_none_or(|m| v >= m) && self.hard_max.is_none_or(|m| v <= m)
    }
    /// Whether one stored value is legal for the row (component count, finiteness, hard bounds,
    /// option membership: an exact option index).
    #[allow(clippy::float_cmp)]
    pub fn accepts(&self, value: &[f64]) -> bool {
        if value.len() != self.kind.components() || !value.iter().all(|v| v.is_finite() && v.abs() <= MAX_STREAM_ABS_VALUE) {
            return false;
        }
        match self.kind {
            StudioParameterKind::Enumeration => value.first().is_some_and(|v| self.options.iter().any(|(i, _)| f64::from(*i) == *v)),
            StudioParameterKind::Scalar | StudioParameterKind::Angle => value.iter().all(|v| self.in_hard_range(*v)),
            StudioParameterKind::Point2d | StudioParameterKind::Color => true,
        }
    }
}

const fn scalar(
    param_key: &'static str,
    param_index: u16,
    display_name: &'static str,
    hard: (f64, f64),
    soft: (f64, f64),
    default: &'static [f64],
    unit: StudioUnitToken,
) -> StudioEffectParamSpec {
    StudioEffectParamSpec {
        param_key,
        param_index,
        display_name,
        kind: StudioParameterKind::Scalar,
        hard_min: Some(hard.0),
        hard_max: Some(hard.1),
        soft_min: Some(soft.0),
        soft_max: Some(soft.1),
        bound_state: StudioBoundState::DeclaredBoth,
        default,
        unit,
        precision: Some(1),
        animatable: true,
        interpolable: true,
        options: &[],
    }
}
const fn unbounded(
    param_key: &'static str,
    param_index: u16,
    display_name: &'static str,
    kind: StudioParameterKind,
    default: &'static [f64],
) -> StudioEffectParamSpec {
    StudioEffectParamSpec {
        param_key,
        param_index,
        display_name,
        kind,
        hard_min: None,
        hard_max: None,
        soft_min: None,
        soft_max: None,
        bound_state: StudioBoundState::UnboundedInSource,
        default,
        unit: StudioUnitToken::Unitless,
        precision: None,
        animatable: true,
        interpolable: true,
        options: &[],
    }
}
const fn enumeration(
    param_key: &'static str,
    param_index: u16,
    display_name: &'static str,
    default: &'static [f64],
    options: &'static [(u16, &'static str)],
) -> StudioEffectParamSpec {
    StudioEffectParamSpec {
        interpolable: false,
        options,
        ..unbounded(param_key, param_index, display_name, StudioParameterKind::Enumeration, default)
    }
}

/// `Blending Mode` options of Circle and Checkerboard (verbatim, [STU-FX-131]).
const GENERATOR_BLEND_OPTIONS: &[(u16, &str)] = &[
    (1, "None"),
    (2, "Normal"),
    (3, "Stencil Alpha"),
    (5, "Add"),
    (6, "Multiply"),
    (7, "Screen"),
    (8, "Overlay"),
    (9, "Soft Light"),
    (10, "Hard Light"),
    (12, "Color Dodge"),
    (13, "Color Burn"),
    (15, "Darken"),
    (16, "Lighten"),
    (17, "Difference"),
    (18, "Exclusion"),
    (20, "Hue"),
    (21, "Saturation"),
    (22, "Color"),
    (23, "Luminosity"),
];
const CENTRE: &[f64] = &[0.5, 0.5];
const WHITE: &[f64] = &[1.0, 1.0, 1.0, 1.0];

/// `Twirl` (`ADBE Twirl`): rows 1-3.
const TWIRL: &[StudioEffectParamSpec] = &[
    StudioEffectParamSpec {
        unit: StudioUnitToken::Degrees,
        precision: Some(0),
        ..unbounded("angle", 1, "Angle", StudioParameterKind::Angle, &[0.0])
    },
    scalar("twirl_radius", 2, "Twirl Radius", (0.0, 100.0), (0.0, 100.0), &[30.0], StudioUnitToken::Unitless),
    unbounded("twirl_center", 3, "Twirl Center", StudioParameterKind::Point2d, CENTRE),
];
/// `Spherize` (`ADBE Spherize`): rows 1-2.
const SPHERIZE: &[StudioEffectParamSpec] = &[
    scalar("radius", 1, "Radius", (0.0, 2500.0), (0.0, 250.0), &[0.0], StudioUnitToken::Unitless),
    unbounded("center_of_sphere", 2, "Center of Sphere", StudioParameterKind::Point2d, CENTRE),
];
/// `Offset` (`ADBE Offset`): rows 1-2.
const OFFSET: &[StudioEffectParamSpec] = &[
    unbounded("shift_center_to", 1, "Shift Center To", StudioParameterKind::Point2d, CENTRE),
    scalar("blend_with_original", 2, "Blend With Original", (0.0, 1.0), (0.0, 1.0), &[0.0], StudioUnitToken::Percent),
];
/// `Polar Coordinates` (`ADBE Polar Coordinates`): rows 1-2.
const POLAR_COORDINATES: &[StudioEffectParamSpec] = &[
    scalar("interpolation", 1, "Interpolation", (0.0, 1.0), (0.0, 1.0), &[0.0], StudioUnitToken::Percent),
    enumeration("type_of_conversion", 2, "Type of Conversion", &[2.0], &[(1, "Rect to Polar"), (2, "Polar to Rect")]),
];
/// `Gradient Ramp` (`ADBE Ramp`): rows 1-7 (row 8 is a `BUTTON`).
const GRADIENT_RAMP: &[StudioEffectParamSpec] = &[
    unbounded("start_of_ramp", 1, "Start of Ramp", StudioParameterKind::Point2d, &[0.5, 0.0]),
    unbounded("start_color", 2, "Start Color", StudioParameterKind::Color, &[0.0, 0.0, 0.0, 1.0]),
    unbounded("end_of_ramp", 3, "End of Ramp", StudioParameterKind::Point2d, &[0.5, 1.0]),
    unbounded("end_color", 4, "End Color", StudioParameterKind::Color, WHITE),
    enumeration("ramp_shape", 5, "Ramp Shape", &[1.0], &[(1, "Linear Ramp"), (2, "Radial Ramp")]),
    scalar("ramp_scatter", 6, "Ramp Scatter", (0.0, 512.0), (0.0, 50.0), &[0.0], StudioUnitToken::Unitless),
    scalar("blend_with_original", 7, "Blend With Original", (0.0, 1.0), (0.0, 1.0), &[0.0], StudioUnitToken::Percent),
];
/// `Circle` (`ADBE Circle`): rows 1-4, 6-7, 10-12 (5/8 are the `Feather` group, 9 an unnamed
/// checkbox).
const CIRCLE: &[StudioEffectParamSpec] = &[
    unbounded("center", 1, "Center", StudioParameterKind::Point2d, CENTRE),
    scalar("radius", 2, "Radius", (-10000.0, 10000.0), (0.0, 600.0), &[75.0], StudioUnitToken::Unitless),
    enumeration(
        "edge",
        3,
        "Edge",
        &[1.0],
        &[(1, "None"), (2, "Edge Radius"), (3, "Thickness"), (4, "Thickness * Radius"), (5, "Thickness & Feather * Radius")],
    ),
    scalar("contextual_slider", 4, "Contextual Slider", (0.0, 10000.0), (0.0, 600.0), &[10.0], StudioUnitToken::Unitless),
    scalar("feather_outer_edge", 6, "Feather Outer Edge", (0.0, 10000.0), (0.0, 100.0), &[0.0], StudioUnitToken::Unitless),
    scalar("feather_inner_edge", 7, "Feather Inner Edge", (0.0, 10000.0), (0.0, 100.0), &[0.0], StudioUnitToken::Unitless),
    unbounded("color", 10, "Color", StudioParameterKind::Color, WHITE),
    scalar("opacity", 11, "Opacity", (0.0, 100.0), (0.0, 100.0), &[100.0], StudioUnitToken::Percent),
    enumeration("blending_mode", 12, "Blending Mode", &[1.0], GENERATOR_BLEND_OPTIONS),
];
/// `Checkerboard` (`ADBE Checkerboard`): rows 1-5, 7-8, 10-12 (6/9 are the `Feather` group).
const CHECKERBOARD: &[StudioEffectParamSpec] = &[
    unbounded("anchor", 1, "Anchor", StudioParameterKind::Point2d, CENTRE),
    enumeration("size_from", 2, "Size From", &[2.0], &[(1, "Corner Point"), (2, "Width Slider"), (3, "Width & Height Sliders")]),
    unbounded("corner", 3, "Corner", StudioParameterKind::Point2d, &[0.58, 0.58]),
    scalar("width", 4, "Width", (1.0, 4000.0), (2.0, 200.0), &[16.0], StudioUnitToken::Unitless),
    scalar("height", 5, "Height", (1.0, 4000.0), (2.0, 200.0), &[8.0], StudioUnitToken::Unitless),
    scalar("feather_width", 7, "Feather Width", (0.0, 400.0), (0.0, 20.0), &[0.0], StudioUnitToken::Unitless),
    scalar("feather_height", 8, "Feather Height", (0.0, 400.0), (0.0, 20.0), &[0.0], StudioUnitToken::Unitless),
    unbounded("color", 10, "Color", StudioParameterKind::Color, WHITE),
    scalar("opacity", 11, "Opacity", (0.0, 100.0), (0.0, 100.0), &[100.0], StudioUnitToken::Percent),
    enumeration("blending_mode", 12, "Blending Mode", &[1.0], GENERATOR_BLEND_OPTIONS),
];

fn yes() -> bool {
    true
}
fn is_yes(v: &bool) -> bool {
    *v
}
fn is_no(v: &bool) -> bool {
    !*v
}
fn full_opacity() -> f64 {
    100.0
}
#[allow(clippy::float_cmp, clippy::trivially_copy_pass_by_ref)]
fn is_full_opacity(v: &f64) -> bool {
    *v == 100.0
}

/// One entry of a composition layer's effect stack ([STU-FX-002]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioCompositionEffect {
    /// Stack-local key `[A-Za-z0-9_-]{1,64}`, unique within the layer.
    pub effect_id: String,
    pub filter_kind: StudioCompositionEffectKind,
    /// [STU-FX-002] `enabled`; absent = `true`. A disabled entry is skipped and the stack
    /// continues with the result before it ([STU-FX-003]).
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub enabled: bool,
    /// Solo: when any enabled entry of the stack is soloed, only the enabled soloed entries run
    /// (Studio reading of the effect-solo switch, UNVERIFIED). Absent = `false`.
    #[serde(default, skip_serializing_if = "is_no")]
    pub solo: bool,
    /// [STU-FX-008] entry opacity, percent `0..=100`: the entry's result is mixed with the
    /// pre-entry result by `opacity / 100`. Absent = 100.
    #[serde(default = "full_opacity", skip_serializing_if = "is_full_opacity")]
    pub opacity: f64,
    /// Values keyed by [`StudioEffectParamSpec::param_key`]; an absent key takes the row default.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, StudioKeyedValue>,
}

impl StudioCompositionEffect {
    /// A fresh entry with every value at its default.
    pub fn new(effect_id: impl Into<String>, filter_kind: StudioCompositionEffectKind) -> Self {
        Self { effect_id: effect_id.into(), filter_kind, enabled: true, solo: false, opacity: 100.0, values: BTreeMap::new() }
    }
    /// The static value of `param_key`, else its default (`None` for a key the kind lacks).
    pub fn static_or_default(&self, param_key: &str) -> Option<Vec<f64>> {
        let spec = self.filter_kind.param(param_key)?;
        Some(self.values.get(param_key).map_or_else(|| spec.default.to_vec(), |v| v.static_value.clone()))
    }
}

fn effect_key(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_EFFECT_ID_LEN && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The effect stack of one layer (count, ids, kinds, values against their rows).
pub(super) fn validate_effects(effects: &[StudioCompositionEffect], layer: &str, cx: &mut Ctx) -> Result<()> {
    let t = format!("{layer}/effects");
    if effects.len() > MAX_LAYER_EFFECTS {
        return Err(err(Code::BudgetExceeded, t));
    }
    cx.nodes(effects.len(), &t)?;
    let mut ids = BTreeSet::new();
    for e in effects {
        cx.tick()?;
        check(effect_key(&e.effect_id), &t, "effect_id")?;
        if !ids.insert(e.effect_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, format!("{t}/{}", e.effect_id)));
        }
        let te = format!("{t}/{}", e.effect_id);
        check(e.opacity.is_finite() && (0.0..=100.0).contains(&e.opacity), &te, "opacity")?;
        cx.nodes(e.values.len(), &te)?;
        for (name, v) in &e.values {
            let spec = e.filter_kind.param(name).ok_or_else(|| err(Code::InvalidField, format!("{te}/values")))?;
            let tv = format!("{te}/{name}");
            let dim = spec.kind.components();
            let shape_ok = |x: &[f64]| x.len() == dim && x.iter().all(|c| c.is_finite() && c.abs() <= MAX_STREAM_ABS_VALUE);
            // Shape first (InvalidField on the value), then the hard bound (the out-of-range target).
            validate_keyed(&tv, dim, &shape_ok, &v.static_value, &v.keyframes, cx)?;
            let in_range = std::iter::once(v.static_value.as_slice()).chain(v.keyframes.iter().map(|k| k.value.as_slice())).all(|x| spec.accepts(x));
            if !in_range {
                return Err(err(Code::InvalidField, format!("{tv}/{EFFECT_PARAM_OUT_OF_RANGE}")));
            }
            // A non-interpolable row (an enumeration) holds between keys.
            let held = v.keyframes.iter().all(|k| k.in_interp == StudioInterpolation::Hold && k.out_interp == StudioInterpolation::Hold);
            check(spec.interpolable || held, &tv, "keyframes/interpolation")?;
            check(spec.animatable || v.keyframes.is_empty(), &tv, "keyframes")?;
        }
    }
    Ok(())
}
