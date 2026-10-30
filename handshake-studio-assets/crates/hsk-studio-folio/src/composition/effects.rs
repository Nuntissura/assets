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
//!   entry opacity; lane SMB-AE-EFFECT-FAMILIES-2 adds the entry blend mode ([STU-FX-008]) and the
//!   entry mask reference ([STU-FX-004]). NOT modelled here (declared gap, recorded in the lane
//!   file): `render_scope` other than `layer`.
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
//! - Colour parameters: four channels, straight RGBA encoded in the profile the parameter's
//!   [STU-FX-118] `StudioColorProfile` reference names
//!   ([`StudioCompositionEffect::colour_profiles`], lane SMB-R43-FOLLOWUPS-1). Defaults are the
//!   record's `ARGB #AARRGGBB` values divided by 255.
//! - Percent-unit rows with hard range `0..1` (`Blend With Original`, `Interpolation`) store the
//!   fraction as recorded; rows with hard range `0..100` store percent.
//!
//! Lane SMB-AE-EFFECT-FAMILIES-2 (parity AE-17, blur / glow / colour families and entry
//! semantics) adds the [STU-FX-131] rows of Blur & Sharpen `Gaussian Blur` (`ADBE Gaussian Blur
//! 2`), `Fast Box Blur` (`ADBE Box Blur2`), `Directional Blur` (`ADBE Motion Blur`), `Radial Blur`
//! (`ADBE Radial Blur`); Stylize `Glow` (`ADBE Glo2`), `Find Edges` (`ADBE Find Edges`); Color
//! Correction `Color Balance` (`ADBE Color Balance 2`), `Exposure` (`ADBE Exposure2`),
//! `Hue/Saturation` (`ADBE HUE SATURATION`), `Levels` (`ADBE Easy Levels2`), `Tint` (`ADBE Tint`),
//! `Tritone` (`ADBE Tritone`); Channel `Invert` (`ADBE Invert`), and the entry fields
//! [`StudioCompositionEffect::blend_mode`] ([STU-FX-008]) and [`StudioCompositionEffect::mask_id`]
//! ([STU-FX-004]). Its declared readings (UNVERIFIED, D5 oracle):
//! - An unnamed `CHECKBOX` row of these kinds is stored as a [STU-FX-112] `boolean` under the key
//!   of the After Effects control it is read as (the record names none): Gaussian / Fast Box Blur
//!   `repeat_edge_pixels`, Find Edges `invert`, Color Balance `preserve_luminosity`, Exposure
//!   `bypass_linear_light_conversion`, Hue/Saturation `colorize`. Booleans are `0` / `1`, hold-only.
//! - Directional Blur `Direction` records the hard pair `0..-1.5e-05` (max below min): read as the
//!   capture's no-range encoding like Twirl `Angle`, so the hard bounds are `None` and the soft pair
//!   `0..7936` survives (`declared_soft_only`). Hue/Saturation `Master Hue` / `Colorize Hue`
//!   (`0..0` everywhere) are read as Twirl `Angle`.
//! - `ARBITRARY_DATA` rows (Hue/Saturation `Channel Range`, Levels `Histogram`) carry no declared
//!   schema id ([STU-FX-112] `arbitrary_data` needs one) and are not stored; Curves
//!   (`ADBE CurvesCustom`), whose only value row is such a blob, is therefore not a kind here.
//! - Brightness & Contrast and Camera Lens Blur have no [STU-FX-131] record ([STU-FX-146] gap) and
//!   are not kinds here.
//!
//! Lane SMB-R43-FOLLOWUPS-1 (round finding F-R43-4, [STU-FX-118] "carries a required
//! StudioColorProfile reference"): [`StudioCompositionEffect::colour_profiles`] maps the
//! `param_key` of a `color` row to its profile reference, in the `StudioColor` shape
//! ([`StudioEffectColourProfile::colour_profile_id`]: an `SCPF` id, or explicit null for the
//! document's RGB working space). Additive: a record without the member keeps its bytes (test
//! `tests/composition_effect_records.rs`
//! `colour_parameters_carry_a_profile_reference_additively`). Folio validates the references it
//! finds (a key that is not a `color` row of the kind is `InvalidField` at
//! `{layer}/effects/{effect_id}/colour_profiles`; the id is checked and resolved like every
//! profile binding) and accepts a colour parameter without one: the renderer refuses that
//! parameter typed when the effect is applied, not at load.
//!
//! Folio hard ceilings (not spec bounds): [`MAX_LAYER_EFFECTS`] entries per layer ([STU-FX-006b]
//! "no fixed per-kind instance limit beyond a document-level performance budget"), effect ids are
//! stack-local keys `[A-Za-z0-9_-]{1,64}`, values finite and within `MAX_STREAM_ABS_VALUE`.
//!
//! Refusals (typed, V4-01/02): `InvalidField` with the record path, `DuplicateIdentity` for a
//! repeated effect id, `BudgetExceeded` for counts; integer arithmetic is denied.
#![deny(clippy::arithmetic_side_effects)]
use super::{MAX_STREAM_ABS_VALUE, StudioInterpolation, StudioKeyedValue, check, validate_keyed};
use crate::appearance::{BLEND_LAST, BLEND_NORMAL, BLEND_PASS_THROUGH, BLEND_TOOL_ONLY};
use crate::values::Ctx;
use crate::{Code, ProfileBinding, Result, err};
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
    /// Blur & Sharpen `Gaussian Blur` (`ADBE Gaussian Blur 2`).
    GaussianBlur,
    /// Blur & Sharpen `Fast Box Blur` (`ADBE Box Blur2`).
    FastBoxBlur,
    /// Blur & Sharpen `Directional Blur` (`ADBE Motion Blur`).
    DirectionalBlur,
    /// Blur & Sharpen `Radial Blur` (`ADBE Radial Blur`).
    RadialBlur,
    /// Stylize `Glow` (`ADBE Glo2`).
    Glow,
    /// Stylize `Find Edges` (`ADBE Find Edges`).
    FindEdges,
    /// Color Correction `Color Balance` (`ADBE Color Balance 2`).
    ColorBalance,
    /// Color Correction `Exposure` (`ADBE Exposure2`).
    Exposure,
    /// Color Correction `Hue/Saturation` (`ADBE HUE SATURATION`).
    HueSaturation,
    /// Color Correction `Levels` (`ADBE Easy Levels2`).
    Levels,
    /// Color Correction `Tint` (`ADBE Tint`).
    Tint,
    /// Color Correction `Tritone` (`ADBE Tritone`).
    Tritone,
    /// Channel `Invert` (`ADBE Invert`).
    Invert,
}

/// [STU-FX-126] category of a kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudioEffectCategory {
    Distort,
    Generate,
    BlurSharpen,
    Stylize,
    ColorCorrection,
    Channel,
}

impl StudioCompositionEffectKind {
    /// Every implemented kind.
    pub const ALL: [Self; 20] = [
        Self::Twirl,
        Self::Spherize,
        Self::Offset,
        Self::PolarCoordinates,
        Self::GradientRamp,
        Self::Circle,
        Self::Checkerboard,
        Self::GaussianBlur,
        Self::FastBoxBlur,
        Self::DirectionalBlur,
        Self::RadialBlur,
        Self::Glow,
        Self::FindEdges,
        Self::ColorBalance,
        Self::Exposure,
        Self::HueSaturation,
        Self::Levels,
        Self::Tint,
        Self::Tritone,
        Self::Invert,
    ];
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Twirl => "twirl",
            Self::Spherize => "spherize",
            Self::Offset => "offset",
            Self::PolarCoordinates => "polar_coordinates",
            Self::GradientRamp => "gradient_ramp",
            Self::Circle => "circle",
            Self::Checkerboard => "checkerboard",
            Self::GaussianBlur => "gaussian_blur",
            Self::FastBoxBlur => "fast_box_blur",
            Self::DirectionalBlur => "directional_blur",
            Self::RadialBlur => "radial_blur",
            Self::Glow => "glow",
            Self::FindEdges => "find_edges",
            Self::ColorBalance => "color_balance",
            Self::Exposure => "exposure",
            Self::HueSaturation => "hue_saturation",
            Self::Levels => "levels",
            Self::Tint => "tint",
            Self::Tritone => "tritone",
            Self::Invert => "invert",
        }
    }
    pub const fn category(self) -> StudioEffectCategory {
        match self {
            Self::Twirl | Self::Spherize | Self::Offset | Self::PolarCoordinates => StudioEffectCategory::Distort,
            Self::GradientRamp | Self::Circle | Self::Checkerboard => StudioEffectCategory::Generate,
            Self::GaussianBlur | Self::FastBoxBlur | Self::DirectionalBlur | Self::RadialBlur => StudioEffectCategory::BlurSharpen,
            Self::Glow | Self::FindEdges => StudioEffectCategory::Stylize,
            Self::ColorBalance | Self::Exposure | Self::HueSaturation | Self::Levels | Self::Tint | Self::Tritone => {
                StudioEffectCategory::ColorCorrection
            }
            Self::Invert => StudioEffectCategory::Channel,
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
            Self::GaussianBlur => "ADBE Gaussian Blur 2",
            Self::FastBoxBlur => "ADBE Box Blur2",
            Self::DirectionalBlur => "ADBE Motion Blur",
            Self::RadialBlur => "ADBE Radial Blur",
            Self::Glow => "ADBE Glo2",
            Self::FindEdges => "ADBE Find Edges",
            Self::ColorBalance => "ADBE Color Balance 2",
            Self::Exposure => "ADBE Exposure2",
            Self::HueSaturation => "ADBE HUE SATURATION",
            Self::Levels => "ADBE Easy Levels2",
            Self::Tint => "ADBE Tint",
            Self::Tritone => "ADBE Tritone",
            Self::Invert => "ADBE Invert",
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
            Self::GaussianBlur => GAUSSIAN_BLUR,
            Self::FastBoxBlur => FAST_BOX_BLUR,
            Self::DirectionalBlur => DIRECTIONAL_BLUR,
            Self::RadialBlur => RADIAL_BLUR,
            Self::Glow => GLOW,
            Self::FindEdges => FIND_EDGES,
            Self::ColorBalance => COLOR_BALANCE,
            Self::Exposure => EXPOSURE,
            Self::HueSaturation => HUE_SATURATION,
            Self::Levels => LEVELS,
            Self::Tint => TINT,
            Self::Tritone => TRITONE,
            Self::Invert => INVERT,
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
    /// `boolean` (a `CHECKBOX` row): stored as `0` or `1`, hold-only.
    Boolean,
}
impl StudioParameterKind {
    /// Components of a stored value.
    pub const fn components(self) -> usize {
        match self {
            Self::Scalar | Self::Angle | Self::Enumeration | Self::Boolean => 1,
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
    /// `pixel` (Levels input and output rows).
    Pixels,
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
            StudioParameterKind::Boolean => value.first().is_some_and(|v| *v == 0.0 || *v == 1.0),
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

/// A `scalar` row with its own precision.
#[allow(clippy::too_many_arguments)]
const fn scalar_p(
    param_key: &'static str,
    param_index: u16,
    display_name: &'static str,
    hard: (f64, f64),
    soft: (f64, f64),
    default: &'static [f64],
    unit: StudioUnitToken,
    precision: Option<u8>,
) -> StudioEffectParamSpec {
    StudioEffectParamSpec { precision, ..scalar(param_key, param_index, display_name, hard, soft, default, unit) }
}
/// A row whose source flags carry `CANNOT_TIME_VARY` ([STU-FX-131a]: `animatable = false`).
const fn fixed(row: StudioEffectParamSpec) -> StudioEffectParamSpec {
    StudioEffectParamSpec { animatable: false, ..row }
}
/// An unnamed `CHECKBOX` row stored under the key it is read as (module docs).
const fn boolean(param_key: &'static str, param_index: u16, display_name: &'static str, default: &'static [f64]) -> StudioEffectParamSpec {
    StudioEffectParamSpec { interpolable: false, ..unbounded(param_key, param_index, display_name, StudioParameterKind::Boolean, default) }
}
/// An `ANGLE` row recorded `0..0` everywhere (module docs: the no-range encoding).
const fn free_angle(param_key: &'static str, param_index: u16, display_name: &'static str) -> StudioEffectParamSpec {
    StudioEffectParamSpec {
        unit: StudioUnitToken::Degrees,
        precision: Some(0),
        ..unbounded(param_key, param_index, display_name, StudioParameterKind::Angle, &[0.0])
    }
}
const ON: &[f64] = &[1.0];
const OFF: &[f64] = &[0.0];
/// `Blur Dimensions` / `Glow Dimensions` options (verbatim).
const DIMENSION_OPTIONS: &[(u16, &str)] = &[(1, "Horizontal and Vertical"), (2, "Horizontal"), (3, "Vertical")];
/// `Channel:` options of Levels (verbatim).
const LEVELS_CHANNEL_OPTIONS: &[(u16, &str)] = &[(1, "RGB"), (2, "Red"), (3, "Green"), (4, "Blue"), (5, "Alpha")];
/// `Clip To Output Black` / `White` options (verbatim).
const CLIP_OPTIONS: &[(u16, &str)] = &[(1, "On"), (2, "Off"), (3, "Off for 32 bpc Color")];

/// `Gaussian Blur` (`ADBE Gaussian Blur 2`): rows 1-3 (row 3 the unnamed checkbox).
const GAUSSIAN_BLUR: &[StudioEffectParamSpec] = &[
    scalar("blurriness", 1, "Blurriness", (0.0, 30000.0), (0.0, 50.0), &[0.0], StudioUnitToken::Unitless),
    enumeration("blur_dimensions", 2, "Blur Dimensions", &[1.0], DIMENSION_OPTIONS),
    boolean("repeat_edge_pixels", 3, "Repeat Edge Pixels (unnamed checkbox, Studio reading)", ON),
];
/// `Fast Box Blur` (`ADBE Box Blur2`): rows 1-4 (row 4 the unnamed checkbox).
const FAST_BOX_BLUR: &[StudioEffectParamSpec] = &[
    scalar("blur_radius", 1, "Blur Radius", (0.0, 30000.0), (0.0, 127.0), &[0.0], StudioUnitToken::Unitless),
    scalar_p("iterations", 2, "Iterations", (1.0, 50.0), (1.0, 5.0), &[3.0], StudioUnitToken::Unitless, None),
    enumeration("blur_dimensions", 3, "Blur Dimensions", &[1.0], DIMENSION_OPTIONS),
    boolean("repeat_edge_pixels", 4, "Repeat Edge Pixels (unnamed checkbox, Studio reading)", ON),
];
/// `Directional Blur` (`ADBE Motion Blur`): rows 1-2 (module docs on `Direction`).
const DIRECTIONAL_BLUR: &[StudioEffectParamSpec] = &[
    StudioEffectParamSpec {
        soft_min: Some(0.0),
        soft_max: Some(7936.0),
        bound_state: StudioBoundState::DeclaredSoftOnly,
        unit: StudioUnitToken::Degrees,
        ..unbounded("direction", 1, "Direction", StudioParameterKind::Angle, &[0.0])
    },
    scalar("blur_length", 2, "Blur Length", (0.0, 1000.0), (0.0, 20.0), &[0.0], StudioUnitToken::Unitless),
];
/// `Radial Blur` (`ADBE Radial Blur`): rows 1-4 and 6 (row 5 is `NO_DATA`).
const RADIAL_BLUR: &[StudioEffectParamSpec] = &[
    scalar("amount", 1, "Amount", (0.0, 1000.0), (0.0, 100.0), &[10.0], StudioUnitToken::Unitless),
    unbounded("center", 2, "Center", StudioParameterKind::Point2d, CENTRE),
    enumeration("type", 3, "Type", &[1.0], &[(1, "Spin"), (2, "Zoom")]),
    enumeration("antialiasing_best_quality", 4, "Antialiasing (Best Quality)", &[1.0], &[(1, "Low"), (2, "High")]),
    scalar_p("random_seed", 6, "Random Seed", (0.0, 10000.0), (0.0, 1000.0), &[0.0], StudioUnitToken::Unitless, None),
];
/// `Glow` (`ADBE Glo2`): rows 1-14. `Glow Threshold` is the 8-bit level `0..255` the source shows
/// as a percent (default 153 = 60 %, Studio reading).
const GLOW: &[StudioEffectParamSpec] = &[
    enumeration("glow_based_on", 1, "Glow Based On", &[2.0], &[(1, "Alpha Channel"), (2, "Color Channels")]),
    scalar("glow_threshold", 2, "Glow Threshold", (0.0, 255.0), (0.0, 255.0), &[153.0], StudioUnitToken::Percent),
    scalar("glow_radius", 3, "Glow Radius", (0.0, 1000.0), (0.0, 100.0), &[10.0], StudioUnitToken::Unitless),
    scalar("glow_intensity", 4, "Glow Intensity", (0.0, 255.0), (0.0, 4.0), &[1.0], StudioUnitToken::Unitless),
    enumeration("composite_original", 5, "Composite Original", &[2.0], &[(1, "On Top"), (2, "Behind"), (3, "None")]),
    enumeration(
        "glow_operation",
        6,
        "Glow Operation",
        &[3.0],
        &[
            (1, "None"),
            (2, "Normal"),
            (3, "Add"),
            (4, "Multiply"),
            (5, "Dissolve"),
            (6, "Screen"),
            (7, "Overlay"),
            (8, "Soft Light"),
            (9, "Hard Light"),
            (10, "Darken"),
            (11, "Lighten"),
            (12, "Difference"),
            (13, "Hue"),
            (14, "Saturation"),
            (15, "Color"),
            (16, "Luminosity"),
            (17, "Color Dodge"),
            (18, "Color Burn"),
            (19, "Exclusion"),
            (20, "Stencil Alpha"),
            (21, "Stencil Luma"),
            (22, "Silhouette Alpha"),
            (23, "Silhouette Luma"),
            (24, "Luminescent Premultiply"),
            (25, "Alpha Add"),
        ],
    ),
    enumeration("glow_colors", 7, "Glow Colors", &[1.0], &[(1, "Original Colors"), (2, "A & B Colors"), (3, "Arbitrary Map")]),
    enumeration(
        "color_looping",
        8,
        "Color Looping",
        &[3.0],
        &[(1, "Sawtooth A>B"), (2, "Sawtooth B>A"), (3, "Triangle A>B>A"), (4, "Triangle B>A>B")],
    ),
    scalar("color_loops", 9, "Color Loops", (1.0, 127.0), (1.0, 10.0), &[1.0], StudioUnitToken::Unitless),
    StudioEffectParamSpec {
        kind: StudioParameterKind::Angle,
        precision: None,
        ..scalar("color_phase", 10, "Color Phase", (0.003906, 0.496094), (0.003906, 0.039062), &[0.003906], StudioUnitToken::Degrees)
    },
    scalar_p("a_b_midpoint", 11, "A & B Midpoint", (0.009995, 0.990005), (0.009995, 0.990005), &[0.5], StudioUnitToken::Percent, Some(0)),
    unbounded("color_a", 12, "Color A", StudioParameterKind::Color, WHITE),
    unbounded("color_b", 13, "Color B", StudioParameterKind::Color, &[0.0, 0.0, 0.0, 1.0]),
    enumeration("glow_dimensions", 14, "Glow Dimensions", &[1.0], DIMENSION_OPTIONS),
];
/// `Find Edges` (`ADBE Find Edges`): rows 1-2 (row 1 the unnamed checkbox).
const FIND_EDGES: &[StudioEffectParamSpec] = &[
    boolean("invert", 1, "Invert (unnamed checkbox, Studio reading)", OFF),
    scalar_p("blend_with_original", 2, "Blend With Original", (0.0, 1.0), (0.0, 1.0), &[0.0], StudioUnitToken::Percent, Some(0)),
];
const fn balance(param_key: &'static str, param_index: u16, display_name: &'static str) -> StudioEffectParamSpec {
    scalar(param_key, param_index, display_name, (-100.0, 100.0), (-100.0, 100.0), &[0.0], StudioUnitToken::Unitless)
}
/// `Color Balance` (`ADBE Color Balance 2`): rows 1-10 (row 10 the unnamed checkbox).
const COLOR_BALANCE: &[StudioEffectParamSpec] = &[
    balance("shadow_red_balance", 1, "Shadow Red Balance"),
    balance("shadow_green_balance", 2, "Shadow Green Balance"),
    balance("shadow_blue_balance", 3, "Shadow Blue Balance"),
    balance("midtone_red_balance", 4, "Midtone Red Balance"),
    balance("midtone_green_balance", 5, "Midtone Green Balance"),
    balance("midtone_blue_balance", 6, "Midtone Blue Balance"),
    balance("highlight_red_balance", 7, "Highlight Red Balance"),
    balance("highlight_green_balance", 8, "Highlight Green Balance"),
    balance("highlight_blue_balance", 9, "Highlight Blue Balance"),
    boolean("preserve_luminosity", 10, "Preserve Luminosity (unnamed checkbox, Studio reading)", OFF),
];
const fn exposure_stops(param_key: &'static str, param_index: u16, display_name: &'static str) -> StudioEffectParamSpec {
    scalar_p(param_key, param_index, display_name, (-100.0, 100.0), (-4.0, 4.0), &[0.0], StudioUnitToken::Unitless, Some(2))
}
const fn exposure_offset(param_key: &'static str, param_index: u16, display_name: &'static str) -> StudioEffectParamSpec {
    scalar_p(param_key, param_index, display_name, (-2.0, 2.0), (-0.5, 0.5), &[0.0], StudioUnitToken::Unitless, Some(4))
}
const fn exposure_gamma(param_key: &'static str, param_index: u16, display_name: &'static str) -> StudioEffectParamSpec {
    scalar_p(param_key, param_index, display_name, (0.1, 10.0), (0.1, 10.0), &[1.0], StudioUnitToken::Unitless, Some(2))
}
/// `Exposure` (`ADBE Exposure2`): rows 1, 3-5, 8-10, 13-15, 18-20, 22 (the groups carry no value;
/// row 22 the unnamed checkbox).
const EXPOSURE: &[StudioEffectParamSpec] = &[
    enumeration("channels", 1, "Channels:", &[1.0], &[(1, "Master"), (2, "Individual Channels")]),
    exposure_stops("exposure", 3, "Exposure"),
    exposure_offset("offset", 4, "Offset"),
    exposure_gamma("gamma_correction", 5, "Gamma Correction"),
    exposure_stops("red_exposure", 8, "Red Exposure"),
    exposure_offset("red_offset", 9, "Red Offset"),
    exposure_gamma("red_gamma_correction", 10, "Red Gamma Correction"),
    exposure_stops("green_exposure", 13, "Green Exposure"),
    exposure_offset("green_offset", 14, "Green Offset"),
    exposure_gamma("green_gamma_correction", 15, "Green Gamma Correction"),
    exposure_stops("blue_exposure", 18, "Blue Exposure"),
    exposure_offset("blue_offset", 19, "Blue Offset"),
    exposure_gamma("blue_gamma_correction", 20, "Blue Gamma Correction"),
    boolean("bypass_linear_light_conversion", 22, "Bypass Linear Light Conversion (unnamed checkbox, Studio reading)", OFF),
];
/// `Hue/Saturation` (`ADBE HUE SATURATION`): rows 2, 4-10 (row 3 `Channel Range` is
/// `ARBITRARY_DATA`, module docs; row 7 the unnamed checkbox).
const HUE_SATURATION: &[StudioEffectParamSpec] = &[
    fixed(enumeration(
        "channel_control",
        2,
        "Channel Control",
        &[1.0],
        &[(1, "Master"), (2, "Reds"), (3, "Yellows"), (4, "Greens"), (5, "Cyans"), (6, "Blues"), (7, "Magentas")],
    )),
    fixed(free_angle("master_hue", 4, "Master Hue")),
    fixed(scalar_p("master_saturation", 5, "Master Saturation", (-100.0, 100.0), (-100.0, 100.0), &[0.0], StudioUnitToken::Unitless, Some(0))),
    fixed(scalar_p("master_lightness", 6, "Master Lightness", (-100.0, 100.0), (-100.0, 100.0), &[0.0], StudioUnitToken::Unitless, Some(0))),
    fixed(boolean("colorize", 7, "Colorize (unnamed checkbox, Studio reading)", OFF)),
    free_angle("colorize_hue", 8, "Colorize Hue"),
    scalar_p("colorize_saturation", 9, "Colorize Saturation", (0.0, 100.0), (0.0, 100.0), &[25.0], StudioUnitToken::Unitless, Some(0)),
    scalar_p("colorize_lightness", 10, "Colorize Lightness", (-100.0, 100.0), (-100.0, 100.0), &[0.0], StudioUnitToken::Unitless, Some(0)),
];
const fn level(param_key: &'static str, param_index: u16, display_name: &'static str, default: &'static [f64]) -> StudioEffectParamSpec {
    fixed(scalar(param_key, param_index, display_name, (-10000.0, 10000.0), (0.0, 1.0), default, StudioUnitToken::Pixels))
}
/// `Levels` (`ADBE Easy Levels2`): rows 1, 3-9 (row 2 `Histogram` is `ARBITRARY_DATA`).
const LEVELS: &[StudioEffectParamSpec] = &[
    fixed(enumeration("channel", 1, "Channel:", &[1.0], LEVELS_CHANNEL_OPTIONS)),
    level("input_black", 3, "Input Black", &[0.0]),
    level("input_white", 4, "Input White", &[1.0]),
    fixed(scalar_p("gamma", 5, "Gamma", (0.0, 5.0), (0.0, 5.0), &[1.0], StudioUnitToken::Unitless, Some(2))),
    level("output_black", 6, "Output Black", &[0.0]),
    level("output_white", 7, "Output White", &[1.0]),
    enumeration("clip_to_output_black", 8, "Clip To Output Black", &[3.0], CLIP_OPTIONS),
    enumeration("clip_to_output_white", 9, "Clip To Output White", &[3.0], CLIP_OPTIONS),
];
/// `Tint` (`ADBE Tint`): rows 1-3 (row 4 is a `BUTTON`).
const TINT: &[StudioEffectParamSpec] = &[
    unbounded("map_black_to", 1, "Map Black To", StudioParameterKind::Color, &[0.0, 0.0, 0.0, 0.0]),
    unbounded("map_white_to", 2, "Map White To", StudioParameterKind::Color, &[1.0, 1.0, 1.0, 0.0]),
    scalar("amount_to_tint", 3, "Amount to Tint", (0.0, 100.0), (0.0, 100.0), &[100.0], StudioUnitToken::Percent),
];
/// `Tritone` (`ADBE Tritone`): rows 1-4.
const TRITONE: &[StudioEffectParamSpec] = &[
    unbounded("highlights", 1, "Highlights", StudioParameterKind::Color, &[1.0, 1.0, 1.0, 0.0]),
    unbounded("midtones", 2, "Midtones", StudioParameterKind::Color, &[127.0 / 255.0, 100.0 / 255.0, 70.0 / 255.0, 0.0]),
    unbounded("shadows", 3, "Shadows", StudioParameterKind::Color, &[0.0, 0.0, 0.0, 0.0]),
    scalar("blend_with_original", 4, "Blend With Original", (0.0, 100.0), (0.0, 100.0), &[0.0], StudioUnitToken::Percent),
];
/// `Invert` (`ADBE Invert`): rows 1-2 (option gaps 5, 10, 15 kept).
const INVERT: &[StudioEffectParamSpec] = &[
    enumeration(
        "channel",
        1,
        "Channel",
        &[1.0],
        &[
            (1, "RGB"),
            (2, "Red"),
            (3, "Green"),
            (4, "Blue"),
            (6, "HLS"),
            (7, "Hue"),
            (8, "Lightness"),
            (9, "Saturation"),
            (11, "YIQ"),
            (12, "Luminance"),
            (13, "In Phase Chrominance"),
            (14, "Quadrature Chrominance"),
            (16, "Alpha"),
        ],
    ),
    scalar_p("blend_with_original", 2, "Blend With Original", (0.0, 100.0), (0.0, 100.0), &[0.0], StudioUnitToken::Percent, Some(0)),
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
fn normal_mode() -> u16 {
    BLEND_NORMAL
}
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_normal_mode(v: &u16) -> bool {
    *v == BLEND_NORMAL
}

/// The [STU-FX-118] `StudioColorProfile` reference of one colour parameter (module docs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioEffectColourProfile {
    /// An `SCPF` id, or explicit null for the document's RGB working space (as `StudioColor`).
    #[serde(deserialize_with = "crate::required")]
    pub colour_profile_id: ProfileBinding,
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
    /// [STU-FX-008] entry blend mode: how the entry's result composes onto the pre-entry result,
    /// as the canonical `StudioBlendMode` discriminant of `LayerAppearance::blend_mode`
    /// (`1..=35`; pass-through and the tool-only modes are refused). Absent = normal (2).
    #[serde(default = "normal_mode", skip_serializing_if = "is_normal_mode")]
    pub blend_mode: u16,
    /// [STU-FX-004] entry effect mask: the `mask_id` of one of the layer's masks (After Effects'
    /// effect mask reference); the entry applies only where that mask covers. Absent = everywhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_id: Option<String>,
    /// Values keyed by [`StudioEffectParamSpec::param_key`]; an absent key takes the row default.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, StudioKeyedValue>,
    /// [STU-FX-118] profile reference per colour parameter, keyed by the `param_key` of a `color`
    /// row of the kind (module docs). Absent = none; not encoded when empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub colour_profiles: BTreeMap<String, StudioEffectColourProfile>,
}

impl StudioCompositionEffect {
    /// A fresh entry with every value at its default.
    pub fn new(effect_id: impl Into<String>, filter_kind: StudioCompositionEffectKind) -> Self {
        Self {
            effect_id: effect_id.into(),
            filter_kind,
            enabled: true,
            solo: false,
            opacity: 100.0,
            blend_mode: BLEND_NORMAL,
            mask_id: None,
            values: BTreeMap::new(),
            colour_profiles: BTreeMap::new(),
        }
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

/// The effect stack of one layer (count, ids, kinds, entry blend mode and mask reference against
/// the layer's `mask_ids`, values against their rows).
pub(super) fn validate_effects(effects: &[StudioCompositionEffect], mask_ids: &BTreeSet<&str>, layer: &str, cx: &mut Ctx) -> Result<()> {
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
        let mode_ok = (1..=BLEND_LAST).contains(&e.blend_mode) && e.blend_mode != BLEND_PASS_THROUGH && !BLEND_TOOL_ONLY.contains(&e.blend_mode);
        check(mode_ok, &te, "blend_mode")?;
        check(e.mask_id.as_deref().is_none_or(|m| mask_ids.contains(m)), &te, "mask_id")?;
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
        cx.nodes(e.colour_profiles.len(), &te)?;
        for (name, p) in &e.colour_profiles {
            let colour = e.filter_kind.param(name).is_some_and(|s| s.kind == StudioParameterKind::Color);
            check(colour, &te, "colour_profiles")?;
            cx.profile(&p.colour_profile_id, &format!("{te}/colour_profiles/{name}"))?;
        }
    }
    Ok(())
}
