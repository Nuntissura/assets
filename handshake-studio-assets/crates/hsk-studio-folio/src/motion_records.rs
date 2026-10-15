//! `StudioMotionRecords` (schema `hsk.studio.motion_records@1`, lane SMB-AE-MOTION-RECORDS): the
//! procedural motion-graphics content and render queue of a document's compositions, stored as a
//! record set BESIDE the document and its `StudioCompositionRecords` (like every other Folio record
//! set; `StudioDocument` and `StudioCompositionRecords` gain no fields, so every existing document
//! and composition record serialises byte for byte as before).
//!
//! Stored (data only; nothing is evaluated here; motion evaluates, composite renders):
//! * shape layers (STU-MOT-100..104): per composition layer one root shape group of ordered items
//!   (generators, paints incl. gradient fill/stroke, path operators, repeater, nested groups) and
//!   keyframe streams addressing an item field by index path;
//! * text animators (STU-MOT-090..092): per text layer the animators (properties + range, wiggly or
//!   expression selectors) and their keyframe streams;
//! * composition audio layers: per composition the audio artifacts placed at composition ticks with
//!   a linear gain (the artifact bytes are resolved by the caller; Folio stores the reference);
//! * render queue (STU-MOT-120..122): items of (composition, render settings, one or more output
//!   modules) with the recovered option sets of STU-MOT-121 (`current_settings` is a real member,
//!   STU-MOT-121a) and the STU-MOT-122 output module fields.
//!
//! Numbers are plain data mirrored from the motion / nib / composite models (Folio depends on accord
//! only). Every keyframe stream uses the composition keyframe rules (`StudioKeyedValue`). Bounds:
//! counts below, group depth [`MAX_SHAPE_DEPTH`], every value finite; refusals are typed
//! (`InvalidField`, `BudgetExceeded`, `DanglingReference`, `DuplicateIdentity`), all items charged
//! against the `Budget` node count.
#![deny(clippy::arithmetic_side_effects)]
#![allow(clippy::match_like_matches_macro)]
use super::{Budget, Code, Resolver, Result, StudioCompositionRecords, StudioDocument, err, id, key, values::Ctx};
use crate::composition::{StudioKeyedValue, StudioRational, validate_keyed};
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

schema_token!(MotionRecordsSchema, "hsk.studio.motion_records@1");

/// Deepest shape group nesting.
pub const MAX_SHAPE_DEPTH: usize = 32;
/// Items per shape group; animators per text layer; selectors per animator.
pub const MAX_SHAPE_ITEMS: usize = 1024;
pub const MAX_TEXT_ANIMATORS: usize = 1024;
pub const MAX_TEXT_SELECTORS: usize = 64;
/// Audio layers per composition; render-queue items; output modules per item.
pub const MAX_AUDIO_LAYERS: usize = 256;
pub const MAX_QUEUE_ITEMS: usize = 1024;
pub const MAX_OUTPUT_MODULES: usize = 16;
/// Largest absolute stored number.
pub const MAX_MOTION_ABS_VALUE: f64 = 1.0e9;

/// Fill rule of a shape fill.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioShapeFillRule {
    NonZero,
    EvenOdd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioLineCap {
    Butt,
    Round,
    Square,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioLineJoin {
    Miter,
    Round,
    Bevel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioTrimMode {
    Simultaneously,
    Individually,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioMergeMode {
    Merge,
    Add,
    Subtract,
    Intersect,
    ExcludeIntersections,
}

/// One path command of a shape path (layer-space pixels).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "op")]
pub enum StudioShapePathCommand {
    MoveTo { p: [f64; 2] },
    LineTo { p: [f64; 2] },
    CubicTo { c1: [f64; 2], c2: [f64; 2], p: [f64; 2] },
    Close,
}

/// A gradient stop (offset 0..=1, straight RGBA 0..=1).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioShapeGradientStop {
    pub offset: f64,
    pub rgba: [f64; 4],
}

/// A shape gradient (After Effects gradient fill / stroke): linear from `start` to `end`, or radial
/// centred at `start` with radius `|end - start|`, in layer-space pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioShapeGradient {
    pub radial: bool,
    pub start: [f64; 2],
    pub end: [f64; 2],
    pub stops: Vec<StudioShapeGradientStop>,
}

/// Stroke style shared by the solid and gradient stroke.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioShapeStrokeStyle {
    pub width: f64,
    pub cap: StudioLineCap,
    pub join: StudioLineJoin,
    pub miter_limit: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dashes: Vec<f64>,
    #[serde(default)]
    pub dash_phase: f64,
}

/// A shape group transform (scale and opacity in percent).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioShapeTransform {
    pub anchor: [f64; 2],
    pub position: [f64; 2],
    pub scale: [f64; 2],
    pub rotation: f64,
    pub opacity: f64,
}
impl Default for StudioShapeTransform {
    fn default() -> Self {
        Self { anchor: [0.0; 2], position: [0.0; 2], scale: [100.0; 2], rotation: 0.0, opacity: 100.0 }
    }
}
impl StudioShapeTransform {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// One shape item (STU-MOT-101 families; order is semantic, STU-MOT-102).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum StudioShapeItem {
    Rect { position: [f64; 2], size: [f64; 2], #[serde(default)] roundness: f64, #[serde(default)] reversed: bool },
    Ellipse { position: [f64; 2], size: [f64; 2], #[serde(default)] reversed: bool },
    Polystar {
        star: bool,
        points: f64,
        position: [f64; 2],
        #[serde(default)]
        rotation: f64,
        outer_radius: f64,
        #[serde(default)]
        inner_radius: f64,
        #[serde(default)]
        outer_roundness: f64,
        #[serde(default)]
        inner_roundness: f64,
        #[serde(default)]
        reversed: bool,
    },
    Path { commands: Vec<StudioShapePathCommand>, #[serde(default)] reversed: bool },
    Fill { rgba: [f64; 4], opacity: f64, rule: StudioShapeFillRule },
    Stroke { rgba: [f64; 4], opacity: f64, style: StudioShapeStrokeStyle },
    GradientFill { gradient: StudioShapeGradient, opacity: f64, rule: StudioShapeFillRule },
    GradientStroke { gradient: StudioShapeGradient, opacity: f64, style: StudioShapeStrokeStyle },
    Trim { start: f64, end: f64, #[serde(default)] offset: f64, mode: StudioTrimMode },
    Offset { amount: f64, join: StudioLineJoin, miter_limit: f64, copies: u32 },
    Merge { mode: StudioMergeMode },
    PuckerBloat { amount: f64 },
    RoundCorners { radius: f64 },
    Twist { angle: f64, center: [f64; 2] },
    WigglePaths {
        size: f64,
        detail: u32,
        smooth: bool,
        wiggles_per_second: f64,
        correlation: f64,
        #[serde(default)]
        temporal_phase: f64,
        #[serde(default)]
        spatial_phase: f64,
        seed: u64,
    },
    ZigZag { size: f64, ridges: u32, smooth: bool },
    WiggleTransform {
        anchor: [f64; 2],
        position: [f64; 2],
        scale: [f64; 2],
        rotation: f64,
        wiggles_per_second: f64,
        #[serde(default)]
        temporal_phase: f64,
        seed: u64,
    },
    Repeater {
        copies: f64,
        #[serde(default)]
        offset: f64,
        anchor: [f64; 2],
        position: [f64; 2],
        scale: [f64; 2],
        rotation: f64,
        start_opacity: f64,
        end_opacity: f64,
        above: bool,
    },
    Group { group: StudioShapeGroup },
}

/// An ordered shape group.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioShapeGroup {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<StudioShapeItem>,
    #[serde(default, skip_serializing_if = "StudioShapeTransform::is_default")]
    pub transform: StudioShapeTransform,
}

/// Keyframable shape fields (composite's `ShapeField`; wire names snake case). `group_*` address the
/// group at `path` (empty = root), every other field the item at `path`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioShapeField {
    TrimStart,
    TrimEnd,
    TrimOffset,
    RepeaterCopies,
    RepeaterOffset,
    RepeaterRotation,
    OffsetAmount,
    PuckerAmount,
    RoundRadius,
    TwistAngle,
    ZigZagSize,
    RectSize,
    RectPosition,
    RectRoundness,
    EllipseSize,
    EllipsePosition,
    PolystarOuterRadius,
    PolystarRotation,
    FillOpacity,
    StrokeWidth,
    StrokeOpacity,
    GroupPosition,
    GroupScale,
    GroupRotation,
    GroupOpacity,
}
impl StudioShapeField {
    pub const fn dimension(self) -> usize {
        match self {
            Self::RectSize | Self::RectPosition | Self::EllipseSize | Self::EllipsePosition | Self::GroupPosition | Self::GroupScale => 2,
            _ => 1,
        }
    }
}

/// One keyframe stream of a shape layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioShapeStream {
    pub path: Vec<u32>,
    pub field: StudioShapeField,
    pub value: StudioKeyedValue,
}

/// A shape layer: the composition layer `layer_id` draws `root`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioShapeLayer {
    pub layer_id: String,
    pub root: StudioShapeGroup,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub streams: Vec<StudioShapeStream>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioTextBasedOn {
    Characters,
    CharactersExcludingSpaces,
    Words,
    Lines,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioRangeShape {
    Square,
    RampUp,
    RampDown,
    Triangle,
    Round,
    Smooth,
}

/// One text selector (STU-MOT-091).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum StudioTextSelector {
    Range {
        start: f64,
        end: f64,
        #[serde(default)]
        offset: f64,
        index_units: bool,
        based_on: StudioTextBasedOn,
        shape: StudioRangeShape,
        smoothness: f64,
        #[serde(default)]
        ease_high: f64,
        #[serde(default)]
        ease_low: f64,
        amount: f64,
        #[serde(default)]
        randomize_order: bool,
        #[serde(default)]
        random_seed: u64,
    },
    Wiggly {
        based_on: StudioTextBasedOn,
        max_amount: f64,
        min_amount: f64,
        wiggles_per_second: f64,
        correlation: f64,
        #[serde(default)]
        temporal_phase: f64,
        #[serde(default)]
        spatial_phase: f64,
        #[serde(default)]
        lock_dimensions: bool,
        random_seed: u64,
    },
    /// Expression selector: the amount (percent) per unit is the expression's value with
    /// `textIndex`, `textTotal` and `selectorValue` in scope.
    Expression { based_on: StudioTextBasedOn, source: String },
}

/// The built animator properties (STU-MOT-092 subset); absent = unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTextAnimatorProperties {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<[f64; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTextAnimator {
    pub name: String,
    #[serde(default)]
    pub properties: StudioTextAnimatorProperties,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selectors: Vec<StudioTextSelector>,
}

/// Keyframable text parameters (motion's `AnimatorParam`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioTextParam {
    RangeStart,
    RangeEnd,
    RangeOffset,
    RangeAmount,
    WigglyMax,
    WigglyMin,
    WigglyFrequency,
    Position,
    Scale,
    Rotation,
    Opacity,
    Fill,
    Tracking,
}
impl StudioTextParam {
    pub const fn dimension(self) -> usize {
        match self {
            Self::Position | Self::Scale => 2,
            Self::Fill => 4,
            _ => 1,
        }
    }
    pub const fn on_selector(self) -> bool {
        matches!(self, Self::RangeStart | Self::RangeEnd | Self::RangeOffset | Self::RangeAmount | Self::WigglyMax | Self::WigglyMin | Self::WigglyFrequency)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTextStream {
    pub animator: u32,
    /// Selector index for selector parameters.
    #[serde(default)]
    pub selector: u32,
    pub param: StudioTextParam,
    pub value: StudioKeyedValue,
}

/// The text animators of composition layer `layer_id`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTextAnimators {
    pub layer_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub animators: Vec<StudioTextAnimator>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub streams: Vec<StudioTextStream>,
}

/// One audio layer: artifact `audio_id` (caller-resolved) placed at composition tick `start_tick`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioAudioLayer {
    pub audio_id: String,
    pub start_tick: u64,
    pub gain: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioCompositionAudio {
    pub composition_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<StudioAudioLayer>,
}

macro_rules! settings_enum {
    ($name:ident { $($v:ident),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($v),+ }
    };
}
settings_enum!(StudioRenderQuality { CurrentSettings, Best, Draft, Wireframe });
settings_enum!(StudioDiskCache { CurrentSettings, ReadOnly });
settings_enum!(StudioProxyUse { CurrentSettings, UseAllProxies, UseCompProxiesOnly, UseNoProxies });
settings_enum!(StudioEffectsSwitch { CurrentSettings, AllOn, AllOff });
settings_enum!(StudioOffSwitch { CurrentSettings, AllOff });
/// STU-MOT-121 colour depth (wire names as recovered).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum StudioColorDepth {
    #[serde(rename = "current_settings")]
    CurrentSettings,
    #[serde(rename = "8_bits_per_channel")]
    Bits8,
    #[serde(rename = "16_bits_per_channel")]
    Bits16,
    #[serde(rename = "32_bits_per_channel")]
    Bits32,
}
settings_enum!(StudioLayerSwitch { CurrentSettings, OnForCheckedLayers, OffForAllLayers });
settings_enum!(StudioFieldRender { Off, UpperFieldFirst, LowerFieldFirst });
/// STU-MOT-121 3:2 pulldown phase (wire names as recovered).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum StudioPulldown {
    #[serde(rename = "off")]
    Off,
    #[serde(rename = "WSSWW")]
    Wssww,
    #[serde(rename = "SSWWW")]
    Sswww,
    #[serde(rename = "SWWWS")]
    Swwws,
    #[serde(rename = "WWWSS")]
    Wwwss,
    #[serde(rename = "WWSSW")]
    Wwssw,
}

/// STU-MOT-121 resolution: a member or a custom divisor pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioRenderResolution {
    CurrentSettings,
    Full,
    Half,
    Third,
    Quarter,
    Custom { x: u32, y: u32 },
}

/// STU-MOT-121 time span.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioTimeSpan {
    LengthOfComp,
    WorkAreaOnly,
    Custom { start_tick: u64, end_tick: u64 },
}

/// STU-MOT-121 render settings (the 14 valued controls; `frame_rate` `None` = use the comp's rate).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioRenderSettings {
    pub quality: StudioRenderQuality,
    pub resolution: StudioRenderResolution,
    pub disk_cache: StudioDiskCache,
    pub proxy_use: StudioProxyUse,
    pub effects: StudioEffectsSwitch,
    pub solo_switches: StudioOffSwitch,
    pub guide_layers: StudioOffSwitch,
    pub color_depth: StudioColorDepth,
    pub frame_blending: StudioLayerSwitch,
    pub field_render: StudioFieldRender,
    pub pulldown_3_2: StudioPulldown,
    pub motion_blur: StudioLayerSwitch,
    pub time_span: StudioTimeSpan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<StudioRational>,
}

settings_enum!(StudioOutputChannels { Rgb, Alpha, RgbAlpha });
settings_enum!(StudioOutputColor { Premultiplied, Straight });
settings_enum!(StudioAudioOutput { Auto, On, Off });
settings_enum!(StudioPostRenderAction { None, Import, ImportAndReplaceUsage, SetProxy });
settings_enum!(StudioOutputFormat { MovVc3, MovAv1, Mp4Av1 });

/// STU-MOT-122 resize group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioOutputResize {
    pub width: u32,
    pub height: u32,
    pub lock_aspect: bool,
}

/// STU-MOT-122 crop group (negative values expand the frame).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioOutputCrop {
    pub top: i32,
    pub left: i32,
    pub bottom: i32,
    pub right: i32,
}

/// STU-MOT-122 audio group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioOutputAudio {
    pub output: StudioAudioOutput,
    pub sample_rate: u32,
    pub bit_depth: u16,
    pub float: bool,
    pub channels: u16,
}

/// STU-MOT-122 colour group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioOutputColour {
    pub preserve_rgb: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_color_space: Option<String>,
    pub embed_color_space: bool,
}

/// STU-MOT-122 output module.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioOutputModule {
    pub module_id: String,
    pub format: StudioOutputFormat,
    /// VC-3 profile key (`dnxhr_lb|dnxhr_sq|dnxhr_hq|dnxhr_hqx`) or AV1 quantizer as text.
    pub codec_option: String,
    pub include_project_link: bool,
    pub post_render_action: StudioPostRenderAction,
    pub include_source_metadata: bool,
    pub channels: StudioOutputChannels,
    pub depth: u8,
    pub color: StudioOutputColor,
    pub starting_number: u32,
    pub use_comp_frame_number: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resize: Option<StudioOutputResize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<StudioOutputCrop>,
    pub audio: StudioOutputAudio,
    pub colour: StudioOutputColour,
    /// Relative output path (no drive, no root, no `..`).
    pub output_path: String,
}

/// STU-MOT-120 render queue item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioRenderQueueItem {
    pub item_id: String,
    pub composition_id: String,
    pub settings: StudioRenderSettings,
    pub output_modules: Vec<StudioOutputModule>,
}

/// The motion records of one document (module docs).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioMotionRecords {
    pub schema_id: MotionRecordsSchema,
    pub document_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shape_layers: Vec<StudioShapeLayer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text_animators: Vec<StudioTextAnimators>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub composition_audio: Vec<StudioCompositionAudio>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub render_queue: Vec<StudioRenderQueueItem>,
}

impl StudioMotionRecords {
    /// An empty set for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self {
            schema_id: MotionRecordsSchema::V1,
            document_id: document_id.into(),
            shape_layers: Vec::new(),
            text_animators: Vec::new(),
            composition_audio: Vec::new(),
            render_queue: Vec::new(),
        }
    }
    pub fn shape_layer(&self, layer_id: &str) -> Option<&StudioShapeLayer> {
        self.shape_layers.iter().find(|s| s.layer_id == layer_id)
    }
    pub fn text_animators(&self, layer_id: &str) -> Option<&StudioTextAnimators> {
        self.text_animators.iter().find(|s| s.layer_id == layer_id)
    }
    pub fn composition_audio(&self, composition_id: &str) -> Option<&StudioCompositionAudio> {
        self.composition_audio.iter().find(|s| s.composition_id == composition_id)
    }
}

/// JSON schema of the record set.
pub fn motion_records_schema() -> schemars::Schema {
    schemars::schema_for!(StudioMotionRecords)
}

fn check(ok: bool, t: &str, field: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(err(Code::InvalidField, format!("{t}/{field}"))) }
}

fn num(v: f64) -> bool {
    v.is_finite() && v.abs() <= MAX_MOTION_ABS_VALUE
}

fn nums(v: &[f64]) -> bool {
    v.iter().all(|x| num(*x))
}

fn unit(v: &[f64]) -> bool {
    v.iter().all(|x| x.is_finite() && (0.0..=1.0).contains(x))
}

fn percent(v: f64) -> bool {
    v.is_finite() && (0.0..=100.0).contains(&v)
}

fn gradient(g: &StudioShapeGradient, t: &str) -> Result<()> {
    check(nums(&[g.start[0], g.start[1], g.end[0], g.end[1]]), t, "gradient/geometry")?;
    check(!g.stops.is_empty() && g.stops.len() <= 1024, t, "gradient/stops")?;
    check(g.stops.iter().all(|s| unit(&[s.offset]) && unit(&s.rgba)), t, "gradient/stops")?;
    check(g.stops.windows(2).all(|w| w[0].offset <= w[1].offset), t, "gradient/stops")
}

fn stroke_style(s: &StudioShapeStrokeStyle, t: &str) -> Result<()> {
    check(num(s.width) && s.width >= 0.0, t, "stroke/width")?;
    check(num(s.miter_limit) && (1.0..=500.0).contains(&s.miter_limit), t, "stroke/miter_limit")?;
    check(s.dashes.len() <= 1024 && nums(&s.dashes) && s.dashes.iter().all(|d| *d >= 0.0) && num(s.dash_phase), t, "stroke/dashes")
}

fn validate_item(item: &StudioShapeItem, t: &str, depth: usize, cx: &mut Ctx) -> Result<()> {
    cx.tick()?;
    use StudioShapeItem as I;
    match item {
        I::Rect { position, size, roundness, .. } => check(nums(position) && nums(size) && size.iter().all(|v| *v >= 0.0) && num(*roundness) && *roundness >= 0.0, t, "rect"),
        I::Ellipse { position, size, .. } => check(nums(position) && nums(size) && size.iter().all(|v| *v >= 0.0), t, "ellipse"),
        I::Polystar { points, position, rotation, outer_radius, inner_radius, outer_roundness, inner_roundness, .. } => check(
            num(*points) && (3.0..=1000.0).contains(points) && nums(position) && nums(&[*rotation, *outer_radius, *inner_radius, *outer_roundness, *inner_roundness]) && *outer_radius >= 0.0 && *inner_radius >= 0.0,
            t,
            "polystar",
        ),
        I::Path { commands, .. } => {
            check(commands.len() <= 65_536, t, "path/commands")?;
            cx.nodes(commands.len(), t)?;
            check(matches!(commands.first(), None | Some(StudioShapePathCommand::MoveTo { .. })), t, "path/commands")?;
            check(
                commands.iter().all(|c| match c {
                    StudioShapePathCommand::MoveTo { p } | StudioShapePathCommand::LineTo { p } => nums(p),
                    StudioShapePathCommand::CubicTo { c1, c2, p } => nums(c1) && nums(c2) && nums(p),
                    StudioShapePathCommand::Close => true,
                }),
                t,
                "path/commands",
            )
        }
        I::Fill { rgba, opacity, .. } => check(unit(rgba) && percent(*opacity), t, "fill"),
        I::Stroke { rgba, opacity, style } => {
            check(unit(rgba) && percent(*opacity), t, "stroke")?;
            stroke_style(style, t)
        }
        I::GradientFill { gradient: g, opacity, .. } => {
            check(percent(*opacity), t, "gradient_fill")?;
            gradient(g, t)
        }
        I::GradientStroke { gradient: g, opacity, style } => {
            check(percent(*opacity), t, "gradient_stroke")?;
            gradient(g, t)?;
            stroke_style(style, t)
        }
        I::Trim { start, end, offset, .. } => check(nums(&[*start, *end, *offset]), t, "trim"),
        I::Offset { amount, miter_limit, copies, .. } => check(num(*amount) && num(*miter_limit) && (1..=64).contains(copies), t, "offset"),
        I::Merge { .. } => Ok(()),
        I::PuckerBloat { amount } => check(num(*amount), t, "pucker_bloat"),
        I::RoundCorners { radius } => check(num(*radius) && *radius >= 0.0, t, "round_corners"),
        I::Twist { angle, center } => check(num(*angle) && nums(center), t, "twist"),
        I::WigglePaths { size, detail, wiggles_per_second, correlation, temporal_phase, spatial_phase, .. } => check(
            nums(&[*size, *wiggles_per_second, *temporal_phase, *spatial_phase]) && *detail <= 1024 && percent(*correlation),
            t,
            "wiggle_paths",
        ),
        I::ZigZag { size, ridges, .. } => check(num(*size) && *ridges <= 1024, t, "zig_zag"),
        I::WiggleTransform { anchor, position, scale, rotation, wiggles_per_second, temporal_phase, .. } => check(
            nums(anchor) && nums(position) && nums(scale) && nums(&[*rotation, *wiggles_per_second, *temporal_phase]),
            t,
            "wiggle_transform",
        ),
        I::Repeater { copies, offset, anchor, position, scale, rotation, start_opacity, end_opacity, .. } => check(
            num(*copies) && (0.0..=1024.0).contains(copies) && num(*offset) && offset.abs() <= 1024.0 && nums(anchor) && nums(position) && nums(scale) && scale.iter().all(|s| *s > 0.0) && num(*rotation) && percent(*start_opacity) && percent(*end_opacity),
            t,
            "repeater",
        ),
        I::Group { group } => validate_group(group, t, depth.checked_add(1).ok_or_else(|| err(Code::BudgetExceeded, t))?, cx),
    }
}

fn validate_group(g: &StudioShapeGroup, t: &str, depth: usize, cx: &mut Ctx) -> Result<()> {
    if depth > MAX_SHAPE_DEPTH {
        return Err(err(Code::BudgetExceeded, format!("{t}/depth")));
    }
    if g.items.len() > MAX_SHAPE_ITEMS {
        return Err(err(Code::BudgetExceeded, format!("{t}/items")));
    }
    cx.nodes(g.items.len(), t)?;
    let tr = &g.transform;
    check(nums(&tr.anchor) && nums(&tr.position) && nums(&tr.scale) && num(tr.rotation) && percent(tr.opacity), t, "transform")?;
    for item in &g.items {
        validate_item(item, t, depth, cx)?;
    }
    Ok(())
}

/// Resolves an item index path in a group (every index but the last selects a nested group).
pub fn shape_item_at<'a>(root: &'a StudioShapeGroup, path: &[u32]) -> Option<&'a StudioShapeItem> {
    let (last, parents) = path.split_last()?;
    let mut g = root;
    for i in parents {
        match g.items.get(usize::try_from(*i).ok()?)? {
            StudioShapeItem::Group { group } => g = group,
            _ => return None,
        }
    }
    g.items.get(usize::try_from(*last).ok()?)
}

fn field_fits(item: Option<&StudioShapeItem>, field: StudioShapeField) -> bool {
    use StudioShapeField as F;
    use StudioShapeItem as I;
    match (item, field) {
        (Some(I::Trim { .. }), F::TrimStart | F::TrimEnd | F::TrimOffset) => true,
        (Some(I::Repeater { .. }), F::RepeaterCopies | F::RepeaterOffset | F::RepeaterRotation) => true,
        (Some(I::Offset { .. }), F::OffsetAmount) => true,
        (Some(I::PuckerBloat { .. }), F::PuckerAmount) => true,
        (Some(I::RoundCorners { .. }), F::RoundRadius) => true,
        (Some(I::Twist { .. }), F::TwistAngle) => true,
        (Some(I::ZigZag { .. }), F::ZigZagSize) => true,
        (Some(I::Rect { .. }), F::RectSize | F::RectPosition | F::RectRoundness) => true,
        (Some(I::Ellipse { .. }), F::EllipseSize | F::EllipsePosition) => true,
        (Some(I::Polystar { .. }), F::PolystarOuterRadius | F::PolystarRotation) => true,
        (Some(I::Fill { .. } | I::GradientFill { .. }), F::FillOpacity) => true,
        (Some(I::Stroke { .. } | I::GradientStroke { .. }), F::StrokeWidth | F::StrokeOpacity) => true,
        _ => false,
    }
}

/// The single motion-records validator: document binding, layer and composition references into
/// `compositions`, uniqueness, counts, depth, finite values in domain, stream addressing and the
/// composition keyframe rules (module docs).
pub fn validate_motion_records(
    r: &StudioMotionRecords,
    d: &StudioDocument,
    compositions: &StudioCompositionRecords,
    b: Budget,
    token: &CancellationToken,
    resolver: &dyn Resolver,
) -> Result<()> {
    let mut cx = Ctx::new(b, token, resolver);
    cx.tick()?;
    id(&r.document_id, "SDOC", "document_id")?;
    if r.document_id != d.document_id || compositions.document_id != d.document_id {
        return Err(err(Code::InvalidField, "document_id"));
    }
    let comp_layer = |layer_id: &str| compositions.layer(layer_id).is_some();
    let mut seen = BTreeSet::new();
    cx.nodes(r.shape_layers.len(), "shape_layers")?;
    for s in &r.shape_layers {
        let t = s.layer_id.as_str();
        if !comp_layer(t) {
            return Err(err(Code::DanglingReference, t));
        }
        if !seen.insert(t) {
            return Err(err(Code::DuplicateIdentity, t));
        }
        validate_group(&s.root, t, 0, &mut cx)?;
        cx.nodes(s.streams.len(), t)?;
        for st in &s.streams {
            let item_ok = if matches!(st.field, StudioShapeField::GroupPosition | StudioShapeField::GroupScale | StudioShapeField::GroupRotation | StudioShapeField::GroupOpacity) {
                st.path.is_empty() || matches!(shape_item_at(&s.root, &st.path), Some(StudioShapeItem::Group { .. }))
            } else {
                field_fits(shape_item_at(&s.root, &st.path), st.field)
            };
            if !item_ok {
                return Err(err(Code::DanglingReference, format!("{t}/streams")));
            }
            let dim = st.field.dimension();
            validate_keyed(&format!("{t}/streams"), dim, &|v: &[f64]| v.len() == dim && nums(v), &st.value.static_value, &st.value.keyframes, &mut cx)?;
        }
    }
    let mut text_seen = BTreeSet::new();
    cx.nodes(r.text_animators.len(), "text_animators")?;
    for ta in &r.text_animators {
        let t = ta.layer_id.as_str();
        if !comp_layer(t) {
            return Err(err(Code::DanglingReference, t));
        }
        if !text_seen.insert(t) {
            return Err(err(Code::DuplicateIdentity, t));
        }
        if ta.animators.len() > MAX_TEXT_ANIMATORS {
            return Err(err(Code::BudgetExceeded, format!("{t}/animators")));
        }
        cx.nodes(ta.animators.len(), t)?;
        for a in &ta.animators {
            check(!a.name.is_empty() && a.name.len() <= 256, t, "animators/name")?;
            if a.selectors.len() > MAX_TEXT_SELECTORS {
                return Err(err(Code::BudgetExceeded, format!("{t}/selectors")));
            }
            cx.nodes(a.selectors.len(), t)?;
            let p = &a.properties;
            let mut values = Vec::new();
            values.extend(p.position.iter().flatten());
            values.extend(p.scale.iter().flatten());
            values.extend(p.rotation.iter().chain(p.tracking.iter()));
            check(nums(&values) && p.opacity.is_none_or(percent) && p.fill.is_none_or(|f| unit(&f)), t, "animators/properties")?;
            for s in &a.selectors {
                match s {
                    StudioTextSelector::Range { start, end, offset, smoothness, ease_high, ease_low, amount, .. } => check(
                        nums(&[*start, *end, *offset]) && percent(*smoothness) && (-100.0..=100.0).contains(ease_high) && (-100.0..=100.0).contains(ease_low) && percent(*amount),
                        t,
                        "selectors/range",
                    )?,
                    StudioTextSelector::Wiggly { max_amount, min_amount, wiggles_per_second, correlation, temporal_phase, spatial_phase, .. } => check(
                        percent(*max_amount) && percent(*min_amount) && min_amount <= max_amount && num(*wiggles_per_second) && *wiggles_per_second >= 0.0 && percent(*correlation) && nums(&[*temporal_phase, *spatial_phase]),
                        t,
                        "selectors/wiggly",
                    )?,
                    StudioTextSelector::Expression { source, .. } => check(!source.is_empty() && source.len() <= 65_536, t, "selectors/expression")?,
                }
            }
        }
        cx.nodes(ta.streams.len(), t)?;
        for st in &ta.streams {
            let animator = usize::try_from(st.animator).ok().and_then(|i| ta.animators.get(i));
            let ok = match animator {
                None => false,
                Some(a) if st.param.on_selector() => {
                    let sel = usize::try_from(st.selector).ok().and_then(|i| a.selectors.get(i));
                    match (sel, st.param) {
                        (Some(StudioTextSelector::Range { .. }), StudioTextParam::RangeStart | StudioTextParam::RangeEnd | StudioTextParam::RangeOffset | StudioTextParam::RangeAmount) => true,
                        (Some(StudioTextSelector::Wiggly { .. }), StudioTextParam::WigglyMax | StudioTextParam::WigglyMin | StudioTextParam::WigglyFrequency) => true,
                        _ => false,
                    }
                }
                Some(_) => true,
            };
            if !ok {
                return Err(err(Code::DanglingReference, format!("{t}/streams")));
            }
            let dim = st.param.dimension();
            validate_keyed(&format!("{t}/streams"), dim, &|v: &[f64]| v.len() == dim && nums(v), &st.value.static_value, &st.value.keyframes, &mut cx)?;
        }
    }
    let mut audio_seen = BTreeSet::new();
    cx.nodes(r.composition_audio.len(), "composition_audio")?;
    for a in &r.composition_audio {
        let t = a.composition_id.as_str();
        if compositions.composition(t).is_none() {
            return Err(err(Code::DanglingReference, t));
        }
        if !audio_seen.insert(t) {
            return Err(err(Code::DuplicateIdentity, t));
        }
        if a.layers.len() > MAX_AUDIO_LAYERS {
            return Err(err(Code::BudgetExceeded, format!("{t}/layers")));
        }
        cx.nodes(a.layers.len(), t)?;
        for l in &a.layers {
            check(key(&l.audio_id), t, "layers/audio_id")?;
            check(l.start_tick <= i64::MAX as u64 && num(l.gain) && l.gain >= 0.0, t, "layers")?;
        }
    }
    if r.render_queue.len() > MAX_QUEUE_ITEMS {
        return Err(err(Code::BudgetExceeded, "render_queue"));
    }
    cx.nodes(r.render_queue.len(), "render_queue")?;
    let mut items = BTreeSet::new();
    for q in &r.render_queue {
        let t = q.item_id.as_str();
        check(key(t), t, "item_id")?;
        if !items.insert(t) {
            return Err(err(Code::DuplicateIdentity, t));
        }
        let Some(comp) = compositions.composition(&q.composition_id) else {
            return Err(err(Code::DanglingReference, format!("{t}/composition_id")));
        };
        if let StudioTimeSpan::Custom { start_tick, end_tick } = q.settings.time_span {
            check(start_tick < end_tick && end_tick <= comp.duration, t, "settings/time_span")?;
        }
        if let StudioRenderResolution::Custom { x, y } = q.settings.resolution {
            check((1..=99).contains(&x) && (1..=99).contains(&y), t, "settings/resolution")?;
        }
        if let Some(rate) = q.settings.frame_rate {
            // STU-MOT-121 frame_rate hard_min 0.1, hard_max 999.
            let fps = rate.numerator as f64 / rate.denominator.max(1) as f64;
            check(rate.denominator > 0 && (0.1..=999.0).contains(&fps), t, "settings/frame_rate")?;
        }
        check(!q.output_modules.is_empty() && q.output_modules.len() <= MAX_OUTPUT_MODULES, t, "output_modules")?;
        cx.nodes(q.output_modules.len(), t)?;
        let mut modules = BTreeSet::new();
        for m in &q.output_modules {
            check(key(&m.module_id) && modules.insert(m.module_id.as_str()), t, "output_modules/module_id")?;
            check(m.starting_number <= 9_999_999, t, "output_modules/starting_number")?;
            if let Some(rs) = m.resize {
                check((1..=30_000).contains(&rs.width) && (1..=30_000).contains(&rs.height), t, "output_modules/resize")?;
            }
            if let Some(c) = m.crop {
                check([c.top, c.left, c.bottom, c.right].iter().all(|v| (-30_000..=30_000).contains(v)), t, "output_modules/crop")?;
            }
            let path_ok = !m.output_path.is_empty()
                && m.output_path.len() <= 1024
                && !m.output_path.starts_with(['/', '\\'])
                && !m.output_path.contains(':')
                && m.output_path.split(['/', '\\']).all(|s| !s.is_empty() && s != "..");
            check(path_ok, t, "output_modules/output_path")?;
            check(m.codec_option.len() <= 64 && matches!(m.depth, 8 | 10 | 12 | 16), t, "output_modules/codec")?;
            check((8_000..=192_000).contains(&m.audio.sample_rate) && matches!(m.audio.channels, 1 | 2) && matches!(m.audio.bit_depth, 16 | 24 | 32), t, "output_modules/audio")?;
        }
    }
    Ok(())
}
