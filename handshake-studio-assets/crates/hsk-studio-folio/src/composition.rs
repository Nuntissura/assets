//! `StudioComposition` (schema `hsk.studio.composition@1`, STU-MOT-001/004/006/008, STU-CMP-030) and
//! the per-document record set `StudioCompositionRecords` that carries compositions beside the
//! document, like the design, raster, vector and type-style record sets (`StudioDocument` gains no
//! fields). Added by SMB-FOLIO-COMPOSITION: until now Folio held no time-based compositing record, so
//! layer in/out windows and animated transform streams lived only in the composite caller's argument.
//!
//! Stored (data only; nothing is evaluated or rendered here): composition size, frame rate (exact
//! rational fps, tick-exact), duration, background; per layer the timing (`in_point`, `out_point`,
//! `start_time`, rational `stretch`), the parent layer, an optional precomposition source
//! (composition in composition), an optional track matte (alpha | luma, inverted) and the
//! keyframe streams of the five transform properties (anchor, position, scale, rotation, opacity).
//!
//! Time is `u64` ticks at 254016000000 per second (accord `TICKS_PER_SECOND`, STU-VID-012) on the wire,
//! limited to `0..=i64::MAX` so that every consumer can hold it in the `i64` tick of motion's
//! `EvalTick`. A keyframe stream mirrors motion's `Property` input model (static value, strictly
//! increasing keyframe times, per-side interpolation, temporal tangents with `speed` and `influence`)
//! but Folio does not depend on motion: the streams are plain data and an adapter in composite builds
//! the `Property` values from them.
//!
//! Declared gaps, not invented here: the spec states no identity prefix for a composition; Accord
//! registers `SCOM` for `hsk.studio.composition@1` (SMB-ACCORD-COMPOSITION-PREFIX), and
//! `composition_id` is an `SCOM-{uuid_v7}` id. Legacy document-local keys (`[A-Za-z0-9_-]{1,128}`, like
//! `sequence_id`) are still accepted; anything starting `SCOM-` must be a valid id. Layers reference the document's `SLYR` layers, so one
//! document layer belongs to at most one composition (`DuplicateIdentity` otherwise). Not stored: the
//! rest of the STU-MOT-004 settings (resolution divisor, shutter, samples, renderer, nested-rate
//! flags), 3D, expressions, markers, work area, blend/quality/switch attributes (blend mode and
//! visibility stay on `StudioLayer`), the other transform groups and spatial tangents. Time-stretch
//! source mapping uses floor division (`source_tick`); Adobe's rounding is UNVERIFIED (D5 oracle).
//! Quantising keyframe times to frame boundaries (STU-MOT-020) is not enforced.
//!
//! Lane SMB-AE-ROTO-WIRING adds per layer `time_remap` (STU-MOT-045), `frame_blending`
//! (STU-MOT-007a) and the assisted-rotoscoping state `roto` (STU-CMP-034), all serde default and
//! skipped when absent (submodule `roto`).
//!
//! Refusals: out-of-range values `InvalidField`; a tick or stretch product beyond `i64` is
//! `InvalidField` with a `tick_overflow` target segment (no new `Code`: the enum is matched
//! exhaustively by dependents); an unknown layer/composition/parent/matte target `DanglingReference`;
//! a parent loop `ContainmentCycle`; a precomposition loop or matte loop `RenderCycle`.
//!
//! The records arrive from untrusted wire JSON: integer arithmetic is denied here (workflow V4-01);
//! layers, streams and keyframes are charged through the checked `Ctx` node budget.
#![deny(clippy::arithmetic_side_effects)]
use super::{
    Budget, Code, ResolutionIssue, Resolver, Result, StudioDocument, canceled, err, id, key,
    values::{Ctx, StudioRgba, label},
};
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

mod roto;
pub use roto::*;

schema_token!(CompositionSchema, "hsk.studio.composition@1");
schema_token!(CompositionRecordsSchema, "hsk.studio.composition_records@1");

/// Compositions per record set (a Folio hard ceiling; the spec declares none).
pub const MAX_COMPOSITIONS: usize = 64;
/// Layers per composition (a Folio hard ceiling).
pub const MAX_COMPOSITION_LAYERS: usize = 4096;
/// Keyframes per stream (equals motion `MAX_KEYFRAMES`).
pub const MAX_STREAM_KEYFRAMES: usize = 65_536;
/// STU-MOT-004 `width` / `height` bounds in pixels.
pub const MIN_COMPOSITION_SIDE: u32 = 4;
pub const MAX_COMPOSITION_SIDE: u32 = 30_000;
/// STU-MOT-004 `frame_rate` bounds in frames per second.
pub const MIN_FPS: u64 = 1;
pub const MAX_FPS: u64 = 999;
/// Largest tick on the wire: the `i64` tick every consumer holds.
pub const MAX_COMPOSITION_TICK: u64 = i64::MAX as u64;
/// Largest absolute stream value (a Folio hard ceiling; values must also be finite).
pub const MAX_STREAM_ABS_VALUE: f64 = 1.0e12;

/// An exact fraction `numerator / denominator`, always in lowest terms (the wire form is canonical).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioRational {
    pub numerator: i64,
    pub denominator: u64,
}

/// Frames per second as an exact fraction in lowest terms; `1..=999` fps and a whole number of ticks
/// per frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioFrameRate {
    pub numerator: u64,
    pub denominator: u64,
}
impl StudioFrameRate {
    /// Ticks per frame when the rate is valid (exact, non-zero); `None` otherwise.
    pub fn ticks_per_frame(&self) -> Option<u64> {
        if self.numerator == 0 || self.denominator == 0 {
            return None;
        }
        let total = u128::from(TICKS_PER_SECOND).checked_mul(u128::from(self.denominator))?;
        let n = u128::from(self.numerator);
        if total.checked_rem(n)? != 0 {
            return None;
        }
        u64::try_from(total.checked_div(n)?).ok().filter(|t| *t > 0)
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let r = a.checked_rem(b).unwrap_or(0);
        a = b;
        b = r;
    }
    a
}
fn lowest_terms(numerator: u64, denominator: u64) -> bool {
    gcd(numerator, denominator) == 1
}

/// Layer timing (STU-MOT-006 `in_point`, `out_point`, `start_time`, `stretch`), composition ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioLayerTiming {
    /// First tick (inclusive) at which the layer exists.
    pub in_point: u64,
    /// First tick (exclusive end) at which the layer no longer exists; greater than `in_point`.
    pub out_point: u64,
    /// Composition tick at which the layer's source time zero sits (may be negative).
    pub start_time: i64,
    /// Time-stretch factor, `source = (composition - start_time) / stretch`; negative reverses;
    /// never zero.
    pub stretch: StudioRational,
}
impl StudioLayerTiming {
    /// Whether the layer exists at `tick` (`in_point <= tick < out_point`).
    pub fn contains(&self, tick: u64) -> bool {
        self.in_point <= tick && tick < self.out_point
    }
    /// Source-time tick for composition `tick`: `floor((tick - start_time) / stretch)`. `None` when the
    /// stretch is invalid or the result leaves `i64` (tick overflow).
    pub fn source_tick(&self, tick: u64) -> Option<i64> {
        let offset = i128::from(tick).checked_sub(i128::from(self.start_time))?;
        let scaled = offset.checked_mul(i128::from(self.stretch.denominator))?;
        let divisor = i128::from(self.stretch.numerator);
        let mut q = scaled.checked_div(divisor)?;
        let r = scaled.checked_rem(divisor)?;
        if r != 0 && ((r < 0) != (divisor < 0)) {
            q = q.checked_sub(1)?;
        }
        i64::try_from(q).ok()
    }
}

/// Interpolation of one keyframe side (motion `Interpolation`, wire names equal).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioInterpolation {
    Linear,
    Bezier,
    ContinuousBezier,
    AutoBezier,
    Hold,
}
impl StudioInterpolation {
    pub const fn is_bezier(self) -> bool {
        matches!(self, Self::Bezier | Self::ContinuousBezier | Self::AutoBezier)
    }
}

/// Temporal tangent (STU-MOT-033): `speed` in value units per second, `influence` `0..=1` of the
/// segment duration.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTemporalTangent {
    pub speed: f64,
    pub influence: f64,
}

/// One keyframe (STU-MOT-030): value at a composition tick with independent in/out interpolation
/// (the easing) and per-component tangents (one tangent broadcasts to every component).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioCompositionKeyframe {
    pub time: u64,
    pub value: Vec<f64>,
    pub in_interp: StudioInterpolation,
    pub out_interp: StudioInterpolation,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub in_tangent: Vec<StudioTemporalTangent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub out_tangent: Vec<StudioTemporalTangent>,
}

/// The animatable transform properties of a composition layer (STU-MOT-013 group), with the
/// component count and unit the composite adapter expects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioLayerProperty {
    /// 2 components, layer-space pixels.
    Anchor,
    /// 2 components, composition pixels.
    Position,
    /// 2 components, percent (100 = unscaled).
    Scale,
    /// 1 component, degrees (unwrapped).
    Rotation,
    /// 1 component, percent `0..=100`.
    Opacity,
}
impl StudioLayerProperty {
    pub const fn dimension(self) -> usize {
        match self {
            Self::Anchor | Self::Position | Self::Scale => 2,
            Self::Rotation | Self::Opacity => 1,
        }
    }
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Anchor => "anchor",
            Self::Position => "position",
            Self::Scale => "scale",
            Self::Rotation => "rotation",
            Self::Opacity => "opacity",
        }
    }
}

/// One value stream (STU-MOT-020a/b): the static value (kept when animated) and the ordered
/// keyframes; empty `keyframes` is a static property.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioPropertyStream {
    pub property: StudioLayerProperty,
    pub static_value: Vec<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyframes: Vec<StudioCompositionKeyframe>,
}

/// Precomposition source (STU-MOT-005 `composition` kind): the layer's content is another
/// composition of the same record set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioPrecompSource {
    pub composition_id: String,
    /// Pass transforms through to the nested composition (STU-MOT-006 collapse switch).
    pub collapse_transformations: bool,
}

/// Track-matte mode (STU-CMP-030): the matte layer's alpha or luminance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioMatteMode {
    Alpha,
    Luma,
}

/// Track matte reference: another layer of the same composition supplies the matte. Data only;
/// compositing the matte is composite's concern.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTrackMatte {
    pub source_layer_id: String,
    pub mode: StudioMatteMode,
    pub inverted: bool,
}

/// One layer of a composition. `layer_id` is an `SLYR` layer of the document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioCompositionLayer {
    pub layer_id: String,
    pub timing: StudioLayerTiming,
    /// STU-MOT-008 parent (transform hierarchy, a forest): a layer of the same composition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precomp: Option<StudioPrecompSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_matte: Option<StudioTrackMatte>,
    /// At most one stream per property.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub streams: Vec<StudioPropertyStream>,
    /// Corner pin (lane SMB-AE-KEY-TRACK-2; STU-CMP-065 four-point corner-pin solve, the After
    /// Effects Corner Pin effect): the layer-space quad `from` is mapped onto the animated quad
    /// `to` before the layer transform. Absent = none; a record without it serialises byte for
    /// byte as before (serde default, skipped when `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corner_pin: Option<StudioCornerPin>,
    /// Masks (lane SMB-AE-KEY-TRACK-3; STU-CMP-025 mask collection, ordered): animated bezier mask
    /// paths in layer space, written for example by a planar track. Empty = none; a record without
    /// them serialises byte for byte as before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub masks: Vec<StudioCompositionMask>,
    /// 3D (lane SMB-AE-3D; STU-CMP-035): `Some` promotes the layer's transform to three dimensions
    /// and enrols it in the composition's 3D space. Absent = a 2D layer; records without it are
    /// byte-identical to before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub three_d: Option<StudioLayer3d>,
    /// Camera (STU-CMP-040/040a): this layer is a camera; it requires `three_d` (its position and
    /// orientation) and has no content of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<StudioCameraOptions>,
    /// Light (STU-CMP-045): this layer is a light; it requires `three_d` and has no content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light: Option<StudioLightOptions>,
    /// Time remap (lane SMB-AE-ROTO-WIRING; STU-MOT-045): the source time in seconds at each
    /// composition tick (module `roto` docs). Absent = the `start_time` / `stretch` mapping;
    /// records without it are byte-identical to before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_remap: Option<StudioKeyedValue>,
    /// Frame blending (STU-MOT-007a); absent = `off`.
    #[serde(default, skip_serializing_if = "StudioFrameBlending::is_off")]
    pub frame_blending: StudioFrameBlending,
    /// Assisted rotoscoping state (STU-CMP-034 item 1); absent = none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roto: Option<Box<StudioAssistedRoto>>,
}

/// A keyframable value of a fixed component count (static value plus keys; the keyframe rules of a
/// transform stream).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioKeyedValue {
    pub static_value: Vec<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyframes: Vec<StudioCompositionKeyframe>,
}
impl StudioKeyedValue {
    /// A static value.
    pub fn constant(value: Vec<f64>) -> Self {
        Self { static_value: value, keyframes: Vec::new() }
    }
}

/// The 3D transform members of [STU-MOT-013] beyond the 2D group, and the material options of
/// [STU-MOT-014] (STU-CMP-035). Absent streams take the authored default (module docs of the 3D
/// bounds table [`STUDIO_3D_BOUNDS`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioLayer3d {
    /// Z position, composition pixels (positive = away from the default camera). Default 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_z: Option<StudioKeyedValue>,
    /// Anchor Z, layer pixels. Default 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_z: Option<StudioKeyedValue>,
    /// Rotation about X, degrees, unwrapped. Default 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_x: Option<StudioKeyedValue>,
    /// Rotation about Y, degrees, unwrapped. Default 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_y: Option<StudioKeyedValue>,
    /// Orientation (X, Y, Z degrees). Default 0, 0, 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orientation: Option<StudioKeyedValue>,
    #[serde(default, skip_serializing_if = "StudioMaterialOptions::is_default")]
    pub material: StudioMaterialOptions,
}

/// Geometry bevel style ([STU-MOT-014]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioBevelStyle {
    #[default]
    None,
    Angular,
    Concave,
    Convex,
}
/// Geometry bevel direction ([STU-MOT-014]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioBevelDirection {
    #[default]
    Inward,
    Outward,
}

fn yes_flag() -> bool {
    true
}
fn is_true_flag(v: &bool) -> bool {
    *v
}

/// [STU-MOT-014] material and geometry options. Booleans are static; numeric members are
/// keyframable (absent = the authored default of [`STUDIO_3D_BOUNDS`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioMaterialOptions {
    #[serde(default = "yes_flag", skip_serializing_if = "is_true_flag")]
    pub accepts_lights: bool,
    #[serde(default, skip_serializing_if = "is_false_flag")]
    pub accepts_shadows: bool,
    #[serde(default, skip_serializing_if = "is_false_flag")]
    pub casts_shadows: bool,
    #[serde(default = "yes_flag", skip_serializing_if = "is_true_flag")]
    pub appears_in_reflections: bool,
    /// Numeric members by name (`light_transmission`, `ambient`, `diffuse`, `specular_intensity`,
    /// `specular_shininess`, `metal`, `reflection`, `glossiness`, `fresnel`, `index_of_refraction`,
    /// `transparency`, `transparency_rolloff`, `extrusion_depth`, `bevel_depth`, `plane_curvature`,
    /// `plane_subdivision`: one component; `shadow_colour`: three); unknown names are refused.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, StudioKeyedValue>,
    #[serde(default, skip_serializing_if = "is_default_bevel_style")]
    pub bevel_style: StudioBevelStyle,
    #[serde(default, skip_serializing_if = "is_default_bevel_direction")]
    pub bevel_direction: StudioBevelDirection,
}
fn is_default_bevel_style(v: &StudioBevelStyle) -> bool {
    *v == StudioBevelStyle::None
}
fn is_default_bevel_direction(v: &StudioBevelDirection) -> bool {
    *v == StudioBevelDirection::Inward
}
impl Default for StudioMaterialOptions {
    fn default() -> Self {
        Self {
            accepts_lights: true,
            accepts_shadows: false,
            casts_shadows: false,
            appears_in_reflections: true,
            values: BTreeMap::new(),
            bevel_style: StudioBevelStyle::None,
            bevel_direction: StudioBevelDirection::Inward,
        }
    }
}
impl StudioMaterialOptions {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
    /// The value of member `name` at its static value or the authored default.
    pub fn static_or_default(&self, name: &str) -> Option<Vec<f64>> {
        self.values.get(name).map(|v| v.static_value.clone()).or_else(|| bound(name).map(|b| b.default.to_vec()))
    }
}

/// Camera iris shape (STU-CMP-040 `iris_shape`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioIrisShape {
    #[default]
    FastRectangle,
    Triangle,
    Square,
    Pentagon,
    Hexagon,
    Heptagon,
    Octagon,
    Nonagon,
    Decagon,
}

/// STU-CMP-040 / 040a camera options. `zoom` is the focal distance in pixels (angle of view is the
/// same value presented as an angle, [`camera_angle_of_view`]); `point_of_interest` `Some` = a
/// two-node camera aimed at that world point, `None` = one-node. Numeric optical members are
/// keyframable by name in `values` (`focus_distance`, `aperture`, `blur_level`, `focus_area_width`,
/// `split_blur_level`, `iris_rotation`, `iris_roundness`, `iris_aspect_ratio`,
/// `iris_diffraction_fringe`, `highlight_gain`, `highlight_threshold`, `highlight_saturation`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioCameraOptions {
    pub zoom: StudioKeyedValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point_of_interest: Option<StudioKeyedValue>,
    #[serde(default, skip_serializing_if = "is_false_flag")]
    pub depth_of_field: bool,
    #[serde(default, skip_serializing_if = "is_default_iris")]
    pub iris_shape: StudioIrisShape,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, StudioKeyedValue>,
}

fn is_default_iris(v: &StudioIrisShape) -> bool {
    *v == StudioIrisShape::FastRectangle
}
fn is_default_falloff(v: &StudioLightFalloff) -> bool {
    *v == StudioLightFalloff::None
}
/// Angle of view (degrees, along the composition width) of `zoom` pixels: `2 atan(width / (2 zoom))`.
pub fn camera_angle_of_view(zoom: f64, width: f64) -> f64 {
    2.0 * (width / (2.0 * zoom)).atan().to_degrees()
}
/// The zoom of an angle of view (inverse of [`camera_angle_of_view`]).
pub fn camera_zoom_of_angle(degrees: f64, width: f64) -> f64 {
    width / (2.0 * (degrees.to_radians() / 2.0).tan())
}

/// STU-CMP-045 light type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioLightType {
    Parallel,
    Spot,
    Point,
    Ambient,
}
/// STU-CMP-045 falloff model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioLightFalloff {
    #[default]
    None,
    Smooth,
    InverseSquareClamped,
}

/// STU-CMP-045 light options. `colour` is three components in `0..=1` in `colour_profile_id`'s
/// space (absent = the composition's working space); `intensity` (percent) is unbounded both ways
/// (a negative light subtracts). Numeric members by name in `values` (`cone_angle`,
/// `cone_feather`, `falloff_start`, `falloff_distance`, `shadow_darkness`, `shadow_diffusion`,
/// `background_opacity`, `background_blur`). `point_of_interest` aims a parallel or spot light.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioLightOptions {
    pub light_type: StudioLightType,
    pub colour: StudioKeyedValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour_profile_id: Option<String>,
    pub intensity: StudioKeyedValue,
    #[serde(default, skip_serializing_if = "is_default_falloff")]
    pub falloff_type: StudioLightFalloff,
    #[serde(default, skip_serializing_if = "is_false_flag")]
    pub casts_shadows: bool,
    #[serde(default, skip_serializing_if = "is_false_flag")]
    pub background_visible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_atom: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point_of_interest: Option<StudioKeyedValue>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, StudioKeyedValue>,
}

/// One authored bound of a 3D numeric member (STU-CMP-046: the recovered surface carries none;
/// these are Studio's explicit authoring under 14.9.1, not inferred from names or observed values).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Studio3dBound {
    pub name: &'static str,
    pub components: usize,
    pub hard_min: f64,
    pub hard_max: f64,
    pub default: &'static [f64],
    pub unit: &'static str,
}
const UNBOUNDED: f64 = MAX_STREAM_ABS_VALUE;
/// Authored bounds of every named 3D numeric member (material, camera, light).
pub const STUDIO_3D_BOUNDS: &[Studio3dBound] = &[
    Studio3dBound { name: "light_transmission", components: 1, hard_min: 0.0, hard_max: 100.0, default: &[0.0], unit: "percent" },
    Studio3dBound { name: "ambient", components: 1, hard_min: 0.0, hard_max: 1000.0, default: &[100.0], unit: "percent" },
    Studio3dBound { name: "diffuse", components: 1, hard_min: 0.0, hard_max: 1000.0, default: &[50.0], unit: "percent" },
    Studio3dBound { name: "specular_intensity", components: 1, hard_min: 0.0, hard_max: 1000.0, default: &[50.0], unit: "percent" },
    Studio3dBound { name: "specular_shininess", components: 1, hard_min: 0.0, hard_max: 100.0, default: &[5.0], unit: "percent" },
    Studio3dBound { name: "metal", components: 1, hard_min: 0.0, hard_max: 100.0, default: &[100.0], unit: "percent" },
    Studio3dBound { name: "reflection", components: 1, hard_min: 0.0, hard_max: 100.0, default: &[0.0], unit: "percent" },
    Studio3dBound { name: "glossiness", components: 1, hard_min: 0.0, hard_max: 100.0, default: &[100.0], unit: "percent" },
    Studio3dBound { name: "fresnel", components: 1, hard_min: 0.0, hard_max: 100.0, default: &[0.0], unit: "percent" },
    Studio3dBound { name: "index_of_refraction", components: 1, hard_min: 1.0, hard_max: 5.0, default: &[1.0], unit: "ratio" },
    Studio3dBound { name: "transparency", components: 1, hard_min: 0.0, hard_max: 100.0, default: &[0.0], unit: "percent" },
    Studio3dBound { name: "transparency_rolloff", components: 1, hard_min: 0.0, hard_max: 100.0, default: &[0.0], unit: "percent" },
    Studio3dBound { name: "shadow_colour", components: 3, hard_min: 0.0, hard_max: 1.0, default: &[0.0, 0.0, 0.0], unit: "colour" },
    Studio3dBound { name: "extrusion_depth", components: 1, hard_min: 0.0, hard_max: 10000.0, default: &[0.0], unit: "px" },
    Studio3dBound { name: "bevel_depth", components: 1, hard_min: 0.0, hard_max: 10000.0, default: &[0.0], unit: "px" },
    Studio3dBound { name: "plane_curvature", components: 1, hard_min: -100.0, hard_max: 100.0, default: &[0.0], unit: "percent" },
    Studio3dBound { name: "plane_subdivision", components: 1, hard_min: 1.0, hard_max: 256.0, default: &[1.0], unit: "count" },
    Studio3dBound { name: "zoom", components: 1, hard_min: 1.0, hard_max: 1.0e6, default: &[0.0], unit: "px" },
    Studio3dBound { name: "point_of_interest", components: 3, hard_min: -UNBOUNDED, hard_max: UNBOUNDED, default: &[0.0, 0.0, 0.0], unit: "px" },
    Studio3dBound { name: "focus_distance", components: 1, hard_min: 1.0, hard_max: 1.0e6, default: &[1000.0], unit: "px" },
    Studio3dBound { name: "aperture", components: 1, hard_min: 0.0, hard_max: 10000.0, default: &[25.0], unit: "px" },
    Studio3dBound { name: "blur_level", components: 1, hard_min: 0.0, hard_max: 1000.0, default: &[100.0], unit: "percent" },
    Studio3dBound { name: "focus_area_width", components: 1, hard_min: 0.0, hard_max: 1.0e6, default: &[0.0], unit: "px" },
    Studio3dBound { name: "split_blur_level", components: 2, hard_min: 0.0, hard_max: 1000.0, default: &[100.0, 100.0], unit: "percent" },
    Studio3dBound { name: "iris_rotation", components: 1, hard_min: -UNBOUNDED, hard_max: UNBOUNDED, default: &[0.0], unit: "degrees" },
    Studio3dBound { name: "iris_roundness", components: 1, hard_min: 0.0, hard_max: 100.0, default: &[0.0], unit: "percent" },
    Studio3dBound { name: "iris_aspect_ratio", components: 1, hard_min: 0.01, hard_max: 100.0, default: &[1.0], unit: "ratio" },
    Studio3dBound { name: "iris_diffraction_fringe", components: 1, hard_min: 0.0, hard_max: 1000.0, default: &[0.0], unit: "percent" },
    Studio3dBound { name: "highlight_gain", components: 1, hard_min: 0.0, hard_max: 100.0, default: &[0.0], unit: "percent" },
    Studio3dBound { name: "highlight_threshold", components: 1, hard_min: 0.0, hard_max: 100.0, default: &[100.0], unit: "percent" },
    Studio3dBound { name: "highlight_saturation", components: 1, hard_min: 0.0, hard_max: 100.0, default: &[0.0], unit: "percent" },
    Studio3dBound { name: "colour", components: 3, hard_min: 0.0, hard_max: 1.0, default: &[1.0, 1.0, 1.0], unit: "colour" },
    Studio3dBound { name: "intensity", components: 1, hard_min: -UNBOUNDED, hard_max: UNBOUNDED, default: &[100.0], unit: "percent" },
    Studio3dBound { name: "cone_angle", components: 1, hard_min: 0.0, hard_max: 180.0, default: &[90.0], unit: "degrees" },
    Studio3dBound { name: "cone_feather", components: 1, hard_min: 0.0, hard_max: 100.0, default: &[50.0], unit: "percent" },
    Studio3dBound { name: "falloff_start", components: 1, hard_min: 0.0, hard_max: 1.0e6, default: &[0.0], unit: "px" },
    Studio3dBound { name: "falloff_distance", components: 1, hard_min: 0.0, hard_max: 1.0e6, default: &[500.0], unit: "px" },
    Studio3dBound { name: "shadow_darkness", components: 1, hard_min: 0.0, hard_max: 100.0, default: &[100.0], unit: "percent" },
    Studio3dBound { name: "shadow_diffusion", components: 1, hard_min: 0.0, hard_max: 1000.0, default: &[0.0], unit: "px" },
    Studio3dBound { name: "background_opacity", components: 1, hard_min: 0.0, hard_max: 100.0, default: &[100.0], unit: "percent" },
    Studio3dBound { name: "background_blur", components: 1, hard_min: 0.0, hard_max: 1000.0, default: &[0.0], unit: "px" },
];
/// The authored bound of `name`.
pub fn bound(name: &str) -> Option<&'static Studio3dBound> {
    STUDIO_3D_BOUNDS.iter().find(|b| b.name == name)
}
const MATERIAL_NAMES: &[&str] = &[
    "light_transmission", "ambient", "diffuse", "specular_intensity", "specular_shininess", "metal", "reflection",
    "glossiness", "fresnel", "index_of_refraction", "transparency", "transparency_rolloff", "shadow_colour",
    "extrusion_depth", "bevel_depth", "plane_curvature", "plane_subdivision",
];
const CAMERA_NAMES: &[&str] = &[
    "focus_distance", "aperture", "blur_level", "focus_area_width", "split_blur_level", "iris_rotation",
    "iris_roundness", "iris_aspect_ratio", "iris_diffraction_fringe", "highlight_gain", "highlight_threshold",
    "highlight_saturation",
];
const LIGHT_NAMES: &[&str] = &[
    "cone_angle", "cone_feather", "falloff_start", "falloff_distance", "shadow_darkness", "shadow_diffusion",
    "background_opacity", "background_blur",
];

/// Most vertices one mask path may carry.
pub const MAX_MASK_PATH_VERTICES: usize = 1024;
/// Most masks one composition layer may carry.
pub const MAX_LAYER_MASKS: usize = 64;

/// STU-CMP-025a mask mode (seven members; masks combine in order).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioCompositionMaskMode {
    None,
    Add,
    Subtract,
    Intersect,
    Lighten,
    Darken,
    Difference,
}

/// One mask of a composition layer (STU-CMP-025): an animated bezier path, its mode and inversion.
/// The `path` stream's value is the vertex list in layer-space pixels, six numbers per vertex:
/// `x, y, in_dx, in_dy, out_dx, out_dy` (the incoming and outgoing tangent handles relative to the
/// vertex; all four zero for a corner), the same vertex count on every keyframe.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioCompositionMask {
    /// `1..=64` bytes, no control characters; unique on the layer.
    pub mask_id: String,
    pub mode: StudioCompositionMaskMode,
    #[serde(default, skip_serializing_if = "is_false_flag")]
    pub inverted: bool,
    /// Closed path (a mask region); an open path is geometry only (mode `none`).
    pub closed: bool,
    pub static_value: Vec<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyframes: Vec<StudioCompositionKeyframe>,
}
fn is_false_flag(v: &bool) -> bool {
    !*v
}
impl StudioCompositionMask {
    /// Numbers per vertex of the path value.
    pub const VERTEX_COMPONENTS: usize = 6;
}

/// A corner pin of a composition layer: `from` (four layer-space corners, upper left, upper
/// right, lower right, lower left) is warped onto `to`, an 8-component keyframe stream of the same
/// four corners (`x0, y0, x1, y1, x2, y2, x3, y3`, layer-space pixels). The map is the projective
/// transform taking each `from` corner to its `to` corner; both quads must be convex.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioCornerPin {
    pub from: [[f64; 2]; 4],
    pub static_value: Vec<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyframes: Vec<StudioCompositionKeyframe>,
}
impl StudioCornerPin {
    /// Components of the `to` stream.
    pub const DIMENSION: usize = 8;
}

/// Whether four corners form a convex, non-degenerate quad (either winding).
pub fn corner_quad_is_convex(q: &[[f64; 2]; 4]) -> bool {
    let cross = |o: [f64; 2], a: [f64; 2], b: [f64; 2]| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    let c = [cross(q[0], q[1], q[2]), cross(q[1], q[2], q[3]), cross(q[2], q[3], q[0]), cross(q[3], q[0], q[1])];
    q.iter().flatten().all(|v| v.is_finite()) && (c.iter().all(|v| *v > 1e-9) || c.iter().all(|v| *v < -1e-9))
}
impl StudioCompositionLayer {
    /// The stream animating `property`.
    pub fn stream(&self, property: StudioLayerProperty) -> Option<&StudioPropertyStream> {
        self.streams.iter().find(|s| s.property == property)
    }
}

/// Studio-authored members of the STU-VID-018 enumerations (declared gaps STU-VID-082 /
/// STU-CMP-106: "their member sets are authored deliberately"; After Effects / Premiere values
/// UNVERIFIED, D5). `projection_type`: 0 flat, 1 equirectangular.
pub const IMMERSIVE_PROJECTION_FLAT: u32 = 0;
pub const IMMERSIVE_PROJECTION_EQUIRECTANGULAR: u32 = 1;
/// `stereoscopic_type`: 0 monoscopic, 1 over-under, 2 side-by-side.
pub const IMMERSIVE_STEREO_MONOSCOPIC: u32 = 0;
pub const IMMERSIVE_STEREO_OVER_UNDER: u32 = 1;
pub const IMMERSIVE_STEREO_SIDE_BY_SIDE: u32 = 2;
/// `ambisonics_monitoring_type`: 0 none (the only authored member).
pub const IMMERSIVE_AMBISONICS_NONE: u32 = 0;

/// STU-VID-018 immersive settings, all nine fields ("`projection_type` (integer; 0 in every shipped
/// configuration read), `stereoscopic_type`, `stereoscopic_eye`, `captured_horizontal_view`,
/// `captured_vertical_view`, `field_of_horizontal_view` and `field_of_vertical_view` ...,
/// `ambisonics_monitoring_type`, and an `ambisonics_hrir` reference"). Views are degrees:
/// horizontal `(0, 360]`, vertical `(0, 180]`. `stereoscopic_eye`: 0 left, 1 right.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioImmersiveSettings {
    pub projection_type: u32,
    pub stereoscopic_type: u32,
    pub stereoscopic_eye: u32,
    pub captured_horizontal_view: f64,
    pub captured_vertical_view: f64,
    pub field_of_horizontal_view: f64,
    pub field_of_vertical_view: f64,
    pub ambisonics_monitoring_type: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ambisonics_hrir: Option<String>,
}

fn validate_immersive(i: &StudioImmersiveSettings, t: &str, cx: &mut Ctx) -> Result<()> {
    check(matches!(i.projection_type, IMMERSIVE_PROJECTION_FLAT | IMMERSIVE_PROJECTION_EQUIRECTANGULAR), t, "projection_type")?;
    check(i.stereoscopic_type <= IMMERSIVE_STEREO_SIDE_BY_SIDE, t, "stereoscopic_type")?;
    check(i.stereoscopic_eye <= 1, t, "stereoscopic_eye")?;
    let h = |v: f64| v.is_finite() && v > 0.0 && v <= 360.0;
    let v = |x: f64| x.is_finite() && x > 0.0 && x <= 180.0;
    check(h(i.captured_horizontal_view), t, "captured_horizontal_view")?;
    check(v(i.captured_vertical_view), t, "captured_vertical_view")?;
    check(h(i.field_of_horizontal_view), t, "field_of_horizontal_view")?;
    check(v(i.field_of_vertical_view), t, "field_of_vertical_view")?;
    check(i.ambisonics_monitoring_type == IMMERSIVE_AMBISONICS_NONE, t, "ambisonics_monitoring_type")?;
    if let Some(r) = &i.ambisonics_hrir {
        check(label(r), t, "ambisonics_hrir")?;
        cx.text(r, t)?;
    }
    Ok(())
}

/// One `StudioComposition`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioComposition {
    pub schema_id: CompositionSchema,
    /// `SCOM-{uuid_v7}` (or a legacy document-local key); unique in the set.
    pub composition_id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Pixel aspect ratio; positive, lowest terms.
    pub pixel_aspect: StudioRational,
    pub frame_rate: StudioFrameRate,
    /// Composition length in ticks; `1..=i64::MAX`.
    pub duration: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<StudioRgba>,
    /// Immersive frame parameters (lane SMB-AE-3D-2; STU-CMP-055a: "The composition carries the
    /// immersive frame parameters ([STU-VID-018]'s record shape applies to compositions as well
    /// as to sequences)"). Absent = a flat composition; records without it are byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub immersive: Option<StudioImmersiveSettings>,
    /// Stack order, topmost first (STU-MOT-009: index 1 is topmost and composited last).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<StudioCompositionLayer>,
}
impl StudioComposition {
    /// The layer with this document layer id.
    pub fn layer(&self, layer_id: &str) -> Option<&StudioCompositionLayer> {
        self.layers.iter().find(|l| l.layer_id == layer_id)
    }
}

/// The compositions of one document. An empty set is not encoded beyond its header.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct StudioCompositionRecords {
    pub schema_id: CompositionRecordsSchema,
    /// `SDOC` id of the annotated document.
    pub document_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compositions: Vec<StudioComposition>,
}
impl StudioCompositionRecords {
    /// An empty set for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self {
            schema_id: CompositionRecordsSchema::V1,
            document_id: document_id.into(),
            compositions: Vec::new(),
        }
    }
    /// The composition with this id.
    pub fn composition(&self, composition_id: &str) -> Option<&StudioComposition> {
        self.compositions
            .iter()
            .find(|c| c.composition_id == composition_id)
    }
    /// The composition holding `layer_id` and that layer (a document layer is in at most one).
    pub fn layer(&self, layer_id: &str) -> Option<(&StudioComposition, &StudioCompositionLayer)> {
        self.compositions
            .iter()
            .find_map(|c| c.layer(layer_id).map(|l| (c, l)))
    }
}

/// JSON schema (draft 2020-12) of the composition record set.
pub fn composition_records_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<StudioCompositionRecords>()
}

fn check(ok: bool, target: &str, field: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(err(Code::InvalidField, format!("{target}/{field}")))
    }
}
/// A composition id: an Accord `SCOM` identity (`SCOM-{uuid_v7}`), or a legacy document-local key.
/// Anything spelled like an `SCOM` id must be a valid one.
fn composition_key(s: &str) -> bool {
    if s.starts_with("SCOM-") {
        id(s, "SCOM", "composition_id").is_ok()
    } else {
        key(s)
    }
}
fn overflow(target: &str, field: &str) -> super::Diagnostic {
    err(Code::InvalidField, format!("{target}/{field}/tick_overflow"))
}

fn validate_rational(r: &StudioRational, positive: bool, target: &str, field: &str) -> Result<()> {
    let ok = r.numerator != 0
        && r.denominator != 0
        && r.numerator != i64::MIN
        && (!positive || r.numerator > 0)
        && lowest_terms(r.numerator.unsigned_abs(), r.denominator);
    check(ok, target, field)
}

fn validate_stream(
    s: &StudioPropertyStream,
    layer: &str,
    cx: &mut Ctx,
) -> Result<()> {
    cx.tick()?;
    let t = format!("{layer}/streams/{}", s.property.wire());
    let dim = s.property.dimension();
    let value_ok = |v: &[f64]| {
        v.len() == dim
            && v.iter().all(|x| x.is_finite() && x.abs() <= MAX_STREAM_ABS_VALUE)
            && (s.property != StudioLayerProperty::Opacity
                || v.iter().all(|x| (0.0..=100.0).contains(x)))
    };
    validate_keyed(&t, dim, &value_ok, &s.static_value, &s.keyframes, cx)
}

/// A corner pin: convex `from`, an 8-component stream of convex `to` quads (keyframe rules of a
/// transform stream).
fn validate_corner_pin(p: &StudioCornerPin, layer: &str, cx: &mut Ctx) -> Result<()> {
    cx.tick()?;
    let t = format!("{layer}/corner_pin");
    check(
        corner_quad_is_convex(&p.from) && p.from.iter().flatten().all(|x| x.abs() <= MAX_STREAM_ABS_VALUE),
        &t,
        "from",
    )?;
    let value_ok = |v: &[f64]| {
        v.len() == StudioCornerPin::DIMENSION
            && v.iter().all(|x| x.is_finite() && x.abs() <= MAX_STREAM_ABS_VALUE)
            && corner_quad_is_convex(&[[v[0], v[1]], [v[2], v[3]], [v[4], v[5]], [v[6], v[7]]])
    };
    validate_keyed(&t, StudioCornerPin::DIMENSION, &value_ok, &p.static_value, &p.keyframes, cx)
}

/// A mask: id shape, an open path only as geometry (`none`), and the path stream (a fixed vertex
/// count `1..=MAX_MASK_PATH_VERTICES`, six finite bounded numbers per vertex, keyframe rules of a
/// transform stream).
fn validate_mask(m: &StudioCompositionMask, layer: &str, cx: &mut Ctx) -> Result<()> {
    cx.tick()?;
    let id_ok = !m.mask_id.is_empty() && m.mask_id.len() <= 64 && !m.mask_id.chars().any(char::is_control);
    check(id_ok, &format!("{layer}/masks"), "mask_id")?;
    let t = format!("{layer}/masks/{}", m.mask_id);
    check(m.closed || m.mode == StudioCompositionMaskMode::None, &t, "closed")?;
    let len = m.static_value.len();
    let vertices = len.checked_div(StudioCompositionMask::VERTEX_COMPONENTS).unwrap_or(0);
    let shape_ok = len.checked_rem(StudioCompositionMask::VERTEX_COMPONENTS) == Some(0)
        && (1..=MAX_MASK_PATH_VERTICES).contains(&vertices);
    check(shape_ok, &t, "static_value")?;
    let value_ok = |v: &[f64]| v.len() == len && v.iter().all(|x| x.is_finite() && x.abs() <= MAX_STREAM_ABS_VALUE);
    validate_keyed(&t, len, &value_ok, &m.static_value, &m.keyframes, cx)
}

/// A keyed value against its authored bound (component count, hard range, keyframe rules).
fn validate_value(v: &StudioKeyedValue, name: &str, t: &str, cx: &mut Ctx) -> Result<()> {
    let b = bound(name).ok_or_else(|| err(Code::InvalidField, format!("{t}/{name}")))?;
    let t = format!("{t}/{name}");
    let value_ok = |x: &[f64]| x.len() == b.components && x.iter().all(|v| v.is_finite() && *v >= b.hard_min && *v <= b.hard_max);
    validate_keyed(&t, b.components, &value_ok, &v.static_value, &v.keyframes, cx)
}
fn validate_named(values: &BTreeMap<String, StudioKeyedValue>, allowed: &[&str], t: &str, cx: &mut Ctx) -> Result<()> {
    for (name, v) in values {
        check(allowed.contains(&name.as_str()), t, "values")?;
        validate_value(v, name, t, cx)?;
    }
    Ok(())
}
/// 3D transform streams (finite, bounded, one or three components) and the material options.
fn validate_3d(d: &StudioLayer3d, layer: &str, cx: &mut Ctx) -> Result<()> {
    let t = format!("{layer}/three_d");
    for (name, v, n) in [
        ("position_z", &d.position_z, 1usize),
        ("anchor_z", &d.anchor_z, 1),
        ("rotation_x", &d.rotation_x, 1),
        ("rotation_y", &d.rotation_y, 1),
        ("orientation", &d.orientation, 3),
    ] {
        if let Some(v) = v {
            let tn = format!("{t}/{name}");
            let value_ok = |x: &[f64]| x.len() == n && x.iter().all(|v| v.is_finite() && v.abs() <= MAX_STREAM_ABS_VALUE);
            validate_keyed(&tn, n, &value_ok, &v.static_value, &v.keyframes, cx)?;
        }
    }
    validate_named(&d.material.values, MATERIAL_NAMES, &format!("{t}/material"), cx)
}

pub(crate) fn validate_keyed(
    t: &str,
    dim: usize,
    value_ok: &dyn Fn(&[f64]) -> bool,
    static_value: &[f64],
    keyframes: &[StudioCompositionKeyframe],
    cx: &mut Ctx,
) -> Result<()> {
    check(value_ok(static_value), t, "static_value")?;
    if keyframes.len() > MAX_STREAM_KEYFRAMES {
        return Err(err(Code::BudgetExceeded, format!("{t}/keyframes")));
    }
    cx.nodes(keyframes.len(), t)?;
    let mut previous: Option<u64> = None;
    for k in keyframes {
        cx.tick()?;
        if k.time > MAX_COMPOSITION_TICK {
            return Err(overflow(t, "keyframes/time"));
        }
        check(previous.is_none_or(|p| p < k.time), t, "keyframes/time")?;
        previous = Some(k.time);
        check(value_ok(&k.value), t, "keyframes/value")?;
        for (interp, tangents) in [(k.in_interp, &k.in_tangent), (k.out_interp, &k.out_tangent)] {
            let shape = tangents.is_empty() || tangents.len() == 1 || tangents.len() == dim;
            let each = tangents.iter().all(|g| {
                g.speed.is_finite()
                    && g.speed.abs() <= MAX_STREAM_ABS_VALUE
                    && g.influence.is_finite()
                    && (0.0..=1.0).contains(&g.influence)
            });
            check(shape && each, t, "keyframes/tangent")?;
            check(!interp.is_bezier() || !tangents.is_empty(), t, "keyframes/tangent")?;
        }
    }
    Ok(())
}

fn validate_layer(
    l: &StudioCompositionLayer,
    doc_layers: &BTreeSet<&str>,
    cx: &mut Ctx,
) -> Result<()> {
    cx.tick()?;
    let t = l.layer_id.as_str();
    id(t, "SLYR", "layer_id")?;
    if !doc_layers.contains(t) {
        return Err(err(Code::DanglingReference, t));
    }
    let tm = &l.timing;
    if tm.in_point > MAX_COMPOSITION_TICK || tm.out_point > MAX_COMPOSITION_TICK {
        return Err(overflow(t, "timing"));
    }
    check(tm.in_point < tm.out_point, t, "timing")?;
    check(tm.start_time != i64::MIN, t, "timing/start_time")?;
    validate_rational(&tm.stretch, false, t, "timing/stretch")?;
    // The source-time mapping of both window ends must stay inside i64.
    if tm.source_tick(tm.in_point).is_none() || tm.source_tick(tm.out_point).is_none() {
        return Err(overflow(t, "timing/stretch"));
    }
    if let Some(p) = &l.precomp {
        check(composition_key(&p.composition_id), t, "precomp")?;
    }
    if let Some(m) = &l.track_matte {
        id(&m.source_layer_id, "SLYR", &format!("{t}/track_matte"))?;
        check(m.source_layer_id != l.layer_id, t, "track_matte")?;
    }
    if let Some(p) = &l.parent {
        id(p, "SLYR", &format!("{t}/parent"))?;
    }
    cx.nodes(l.streams.len(), t)?;
    let mut seen = BTreeSet::new();
    for s in &l.streams {
        if !seen.insert(s.property) {
            return Err(err(Code::DuplicateIdentity, format!("{t}/streams/{}", s.property.wire())));
        }
        validate_stream(s, t, cx)?;
    }
    if let Some(p) = &l.corner_pin {
        validate_corner_pin(p, t, cx)?;
    }
    if let Some(d) = &l.three_d {
        validate_3d(d, t, cx)?;
    }
    check(!(l.camera.is_some() && l.light.is_some()), t, "camera")?;
    if let Some(c) = &l.camera {
        check(l.three_d.is_some(), t, "camera")?;
        let tc = format!("{t}/camera");
        validate_value(&c.zoom, "zoom", &tc, cx)?;
        if let Some(p) = &c.point_of_interest {
            validate_value(p, "point_of_interest", &tc, cx)?;
        }
        validate_named(&c.values, CAMERA_NAMES, &tc, cx)?;
    }
    if let Some(li) = &l.light {
        check(l.three_d.is_some(), t, "light")?;
        let tl = format!("{t}/light");
        validate_value(&li.colour, "colour", &tl, cx)?;
        validate_value(&li.intensity, "intensity", &tl, cx)?;
        if let Some(p) = &li.point_of_interest {
            validate_value(p, "point_of_interest", &tl, cx)?;
        }
        validate_named(&li.values, LIGHT_NAMES, &tl, cx)?;
    }
    if l.masks.len() > MAX_LAYER_MASKS {
        return Err(err(Code::BudgetExceeded, format!("{t}/masks")));
    }
    cx.nodes(l.masks.len(), t)?;
    let mut ids = BTreeSet::new();
    for m in &l.masks {
        validate_mask(m, t, cx)?;
        if !ids.insert(m.mask_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, format!("{t}/masks/{}", m.mask_id)));
        }
    }
    if let Some(r) = &l.time_remap {
        roto::validate_time_remap(r, t, cx)?;
    }
    if let Some(r) = &l.roto {
        roto::validate_roto(r, t, cx)?;
    }
    Ok(())
}

/// Follows `next` from every key; a repeat on one chain is a cycle (`code`, target the start).
fn reject_cycles(
    next: &BTreeMap<&str, &str>,
    code: Code,
    cx: &Ctx,
    prefix: &str,
) -> Result<()> {
    for start in next.keys() {
        cx.tick()?;
        let mut seen = BTreeSet::new();
        let mut cursor = Some(*start);
        while let Some(node) = cursor {
            if !seen.insert(node) {
                return Err(err(code, format!("{prefix}{start}")));
            }
            cursor = next.get(node).copied();
        }
    }
    Ok(())
}

fn validate_composition(
    c: &StudioComposition,
    doc_layers: &BTreeSet<&str>,
    cx: &mut Ctx,
) -> Result<()> {
    cx.tick()?;
    let t = c.composition_id.as_str();
    check(composition_key(t), "composition_id", t)?;
    check(label(&c.name), t, "name")?;
    cx.text(&c.name, t)?;
    let side = MIN_COMPOSITION_SIDE..=MAX_COMPOSITION_SIDE;
    check(side.contains(&c.width) && side.contains(&c.height), t, "size")?;
    validate_rational(&c.pixel_aspect, true, t, "pixel_aspect")?;
    let fr = &c.frame_rate;
    let fps_ok = fr.denominator != 0
        && lowest_terms(fr.numerator, fr.denominator)
        && fr.numerator >= fr.denominator.saturating_mul(MIN_FPS)
        && fr.numerator <= fr.denominator.saturating_mul(MAX_FPS)
        && fr.ticks_per_frame().is_some();
    check(fps_ok, t, "frame_rate")?;
    if c.duration > MAX_COMPOSITION_TICK {
        return Err(overflow(t, "duration"));
    }
    check(c.duration > 0, t, "duration")?;
    if let Some(b) = &c.background {
        b.validate(&format!("{t}/background"), cx)?;
    }
    if let Some(i) = &c.immersive {
        validate_immersive(i, &format!("{t}/immersive"), cx)?;
    }
    if c.layers.len() > MAX_COMPOSITION_LAYERS {
        return Err(err(Code::BudgetExceeded, format!("{t}/layers")));
    }
    cx.nodes(c.layers.len(), t)?;
    let mut ids = BTreeSet::new();
    for l in &c.layers {
        validate_layer(l, doc_layers, cx)?;
        if !ids.insert(l.layer_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, &l.layer_id));
        }
    }
    let mut parents = BTreeMap::new();
    let mut mattes = BTreeMap::new();
    for l in &c.layers {
        if let Some(p) = &l.parent {
            if !ids.contains(p.as_str()) {
                return Err(err(Code::DanglingReference, format!("{}/parent", l.layer_id)));
            }
            parents.insert(l.layer_id.as_str(), p.as_str());
        }
        if let Some(m) = &l.track_matte {
            if !ids.contains(m.source_layer_id.as_str()) {
                return Err(err(Code::DanglingReference, format!("{}/track_matte", l.layer_id)));
            }
            mattes.insert(l.layer_id.as_str(), m.source_layer_id.as_str());
        }
    }
    reject_cycles(&parents, Code::ContainmentCycle, cx, "")?;
    reject_cycles(&mattes, Code::RenderCycle, cx, "")?;
    Ok(())
}

/// The single composition validator: document binding, ids, size/rate/duration bounds, layer
/// references into the document, per-layer timing and tick-overflow checks, streams, parent and
/// track-matte references inside the composition and their cycles, precomposition references inside
/// the set and their cycles (`Code::RenderCycle`). Layers, streams and keyframes count against
/// `Budget::nodes`, names against `Budget::payload_bytes`.
pub fn validate_composition_records(
    r: &StudioCompositionRecords,
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
    if r.compositions.len() > MAX_COMPOSITIONS {
        return Err(err(Code::BudgetExceeded, "compositions"));
    }
    cx.nodes(r.compositions.len(), "compositions")?;
    let doc_layers: BTreeSet<&str> = d.layers.iter().map(|l| l.layer_id.as_str()).collect();
    let mut comp_ids = BTreeSet::new();
    let mut layer_owner: BTreeMap<&str, &str> = BTreeMap::new();
    for c in &r.compositions {
        validate_composition(c, &doc_layers, &mut cx)?;
        if !comp_ids.insert(c.composition_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, &c.composition_id));
        }
        for l in &c.layers {
            if layer_owner.insert(&l.layer_id, &c.composition_id).is_some() {
                return Err(err(Code::DuplicateIdentity, &l.layer_id));
            }
        }
    }
    // Precomposition references stay in the set; the composition graph is acyclic.
    let mut edges: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for c in &r.compositions {
        for l in &c.layers {
            let Some(p) = &l.precomp else { continue };
            if !comp_ids.contains(p.composition_id.as_str()) {
                return Err(err(Code::DanglingReference, format!("{}/precomp", l.layer_id)));
            }
            edges
                .entry(&c.composition_id)
                .or_default()
                .insert(&p.composition_id);
        }
    }
    for start in edges.keys() {
        cx.tick()?;
        let mut visited = BTreeSet::new();
        let mut stack: Vec<&str> = edges[start].iter().copied().collect();
        while let Some(node) = stack.pop() {
            if node == *start {
                return Err(err(Code::RenderCycle, *start));
            }
            if visited.insert(node) {
                stack.extend(edges.get(node).into_iter().flatten().copied());
            }
        }
    }
    canceled(token)?;
    Ok(cx.issues)
}
