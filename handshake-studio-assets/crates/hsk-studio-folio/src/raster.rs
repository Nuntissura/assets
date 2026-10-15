//! `StudioRasterRecords`: the raster records that the spec places on `StudioLayer`, `StudioMask` and
//! `StudioDocument` (14.4) and that Folio keeps beside the document, like `StudioDesignRecords`,
//! because adding fields to those structs would break every struct-literal constructor in the
//! workspace. One record set annotates one document (`document_id`); entries are keyed by `SLYR`
//! layer id. There is no spec primitive name for the set; members keep their spec names. Decision:
//! work-folder `diagnostics/adjustments-advanced-blending.md`.
//!
//! Members:
//! - `StudioAdjustment` (STU-RAS-151, first set: levels, curves, hue_saturation,
//!   brightness_contrast, exposure, invert, threshold, posterize; second set, lane
//!   SMB-PS-ADJUST-STYLES: vibrance, colour_balance, photo_filter, black_and_white, channel_mixer,
//!   selective_colour, gradient_map, colour_lookup, shadows_highlights, tone_compression,
//!   match_colour, replace_colour) on a layer of kind `adjustment`
//!   whose payload is `PrimitiveReference { schema_id: "hsk.studio.adjustment@1", object_id: <its own
//!   layer id> }` (Accord defines no identity prefix for that schema, so the adjustment is keyed by
//!   its hosting layer; STU-RAS-007: one adjustment per adjustment layer). Such a layer may be a
//!   `source` operation of the composition graph (image output) and an edge source into a group's
//!   `inputs`, so `input_ordinal` gives it its stack position (STU-CMP-092); the document validator
//!   checks the payload names the layer's own id and refuses a layer clipped to an adjustment layer
//!   (`validate_adjustment_clip_bases`). Test: `tests/adjustment_graph_contract.rs`.
//! - `StudioMaskParameters` (STU-RAS-115/124: density percent `0..=100`, feather px `0..=10000`,
//!   invert) for a layer whose `appearance.mask` is present.
//! - `StudioAdvancedBlending` (STU-RAS-041/159: Blend-If eight values per channel, knockout as the
//!   STU-VEC-153 four-member enumeration, per-channel blend enablement, interior/clipped-effects
//!   and transparency-shapes toggles).
//! - `StudioEffectStack` (STU-RAS-040, 156-158: ordered multi-instance effects, common block, kind
//!   blocks, `StudioContour`) and the document-level Global Light pair (STU-RAS-158). The stack also
//!   carries the `blur` kind (layer or backdrop Gaussian blur, STU-FX-002/013/031), which is not a
//!   STU-RAS-157 layer-style block.
//!
//! Folio stores and validates only; the maths is `hsk_studio_pigment::adjust_math` /
//! `coverage_math`, and effects are not rendered yet.
//!
//! Declared gaps, not invented here: Photoshop's knockout depth (shallow/deep) has no spec field
//! (the four-member enumeration is the only one in the spec); levels/curves channels beyond
//! composite/red/green/blue (CMYK, Lab, the fifth reserved curve) are not stored; hue/saturation
//! band storage keeps one band list (the source's parallel arrays and version field stay in the
//! import preservation record); raster gradient and pattern contents are references to
//! `hsk.studio.gradient@1` / `hsk.studio.pattern@1` primitives (STU-RAS-160 raster gradient is a
//! declared gap); bounds the spec marks `unknown` are checked only for finiteness and their
//! physical domain (non-negative sizes); no bound is invented for them.
//!
//! The records arrive from untrusted wire JSON: integer arithmetic is denied here (workflow
//! V4-01); counts are charged through the checked `Ctx` budget.
#![deny(clippy::arithmetic_side_effects)]
use super::{
    BLEND_LAST, BLEND_PASS_THROUGH, BLEND_TOOL_ONLY, Budget, Code, Payload, ResolutionIssue,
    Resolver, Result, StudioDocument, StudioLayer, canceled, err, id, key,
    values::{Ctx, StudioColor, label, percent},
};
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

schema_token!(RasterRecordsSchema, "hsk.studio.raster_records@1");

/// Schema id an adjustment layer's `PrimitiveReference` payload carries.
pub const ADJUSTMENT_SCHEMA: &str = hsk_studio_accord::SCHEMA_STUDIO_ADJUSTMENT;
/// Gradient and pattern primitive schemas an effect may reference.
pub const GRADIENT_SCHEMA: &str = hsk_studio_accord::SCHEMA_STUDIO_GRADIENT;
pub const PATTERN_SCHEMA: &str = hsk_studio_accord::SCHEMA_STUDIO_PATTERN;
/// Curve and contour points per curve (a Folio hard ceiling; the spec declares the count `unknown`).
pub const MAX_CURVE_POINTS: usize = 256;
/// Effect instances per stack (a Folio hard ceiling; STU-RAS-156 requires at least 10 per kind).
pub const MAX_EFFECTS: usize = 1024;
/// Hue/saturation colour bands (a Folio hard ceiling; the spec says band count is data).
pub const MAX_HUE_BANDS: usize = 64;
/// Gradient-map stops (a Folio hard ceiling, equal to pigment's).
pub const MAX_GRADIENT_MAP_STOPS: usize = 256;
/// Colour-lookup cube side (a Folio hard ceiling, equal to pigment's: 65^3 entries).
pub const MAX_LUT_SIDE: u16 = 65;
/// Replace-colour samples (a Folio hard ceiling, equal to pigment's).
pub const MAX_REPLACE_SAMPLES: usize = 64;
/// Channel-mixer rows and weights per row (CMYK has four channels).
pub const MAX_MIXER_CHANNELS: usize = 4;
/// `StudioMask.feather` hard maximum in pixels (STU-RAS-115).
pub const MAX_MASK_FEATHER: f64 = 10_000.0;
/// Blur effect radius maximum in pixels: the D19 (f) engineering effect-size bound (STU-RAS-180 (f),
/// staged), not a Photoshop or Figma limit.
pub const MAX_BLUR_RADIUS: f64 = 1000.0;

/// Channel of a levels record or curve (composite + R + G + B, STU-RAS-151).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum StudioAdjustmentChannel {
    Composite,
    Red,
    Green,
    Blue,
}

/// One levels record (STU-RAS-151/152: points `normalized_0_1`, `gamma` a count).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioLevelsRecord {
    pub channel: StudioAdjustmentChannel,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub input_black: f64,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub input_white: f64,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub output_black: f64,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub output_white: f64,
    /// Soft `0.1..=9.99` (STU-RAS-151 `derived`); hard bound `unknown`: finite and positive only.
    pub gamma: f64,
}

/// Curve control point in `levels_0_255` (STU-RAS-152 exception) with the per-point corner flag.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioCurvePoint {
    #[schemars(range(min = 0.0, max = 255.0))]
    pub input: f64,
    #[schemars(range(min = 0.0, max = 255.0))]
    pub output: f64,
    pub corner: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioCurve {
    pub channel: StudioAdjustmentChannel,
    pub points: Vec<StudioCurvePoint>,
}

/// Hue (degrees), saturation and lightness (percent) shift.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioHslShift {
    pub hue: f64,
    pub saturation: f64,
    pub lightness: f64,
}

/// One hue band: four range edges in degrees (data, not constants) and its shift.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioHueBand {
    pub range: [f64; 4],
    pub shift: StudioHslShift,
}

/// `StudioAdjustment`, first set of STU-RAS-151 (tag `kind`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioAdjustment {
    /// At most one record per channel; an absent channel is the identity.
    Levels {
        records: Vec<StudioLevelsRecord>,
    },
    /// At most one curve per channel; an absent channel is the identity.
    Curves {
        curves: Vec<StudioCurve>,
    },
    HueSaturation {
        master: StudioHslShift,
        colorize: bool,
        colorization: StudioHslShift,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        bands: Vec<StudioHueBand>,
    },
    BrightnessContrast {
        brightness: f64,
        contrast: f64,
        use_legacy: bool,
    },
    Exposure {
        exposure: f64,
        offset: f64,
        gamma: f64,
        use_legacy: bool,
    },
    Invert,
    Threshold {
        /// `levels_0_255`, hard `0..=255` (declared).
        level: f64,
        /// Absent = black (the normative default).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        false_colour: Option<StudioColor>,
        /// Absent = white (the normative default).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        true_colour: Option<StudioColor>,
    },
    /// Level count, hard `2..=255` (derived).
    Posterize {
        levels: u16,
    },
    // Second set (lane SMB-PS-ADJUST-STYLES). Bounds the spec marks `unknown` are checked for
    // finiteness only; the soft ranges are enforced by pigment at lowering (typed refusal).
    /// Percent, soft `-100..=100`.
    Vibrance {
        vibrance: f64,
        saturation: f64,
    },
    /// Per tonal range `[cyan_red, magenta_green, yellow_blue]`, soft `-100..=100`.
    ColourBalance {
        shadows: [f64; 3],
        midtones: [f64; 3],
        highlights: [f64; 3],
        preserve_luminosity: bool,
    },
    /// `density` percent, soft `1..=100`.
    PhotoFilter {
        filter_colour: StudioColor,
        density: f64,
        preserve_luminosity: bool,
    },
    /// Six weights percent (soft `-200..=300`), tint hue degrees `0..=360`, tint saturation percent.
    BlackAndWhite {
        red: f64,
        yellow: f64,
        green: f64,
        cyan: f64,
        blue: f64,
        magenta: f64,
        tint_enabled: bool,
        tint_hue: f64,
        tint_saturation: f64,
    },
    /// One row per output channel (the list length is channel-count dependent, STU-RAS-151); each
    /// row holds one weight per input channel and a constant, percent (soft `-200..=200`).
    ChannelMixer {
        outputs: Vec<StudioMixerRow>,
        monochrome: bool,
    },
    /// Nine CMYK quadruples, percent hard `-100..=100` (derived).
    SelectiveColour {
        method: StudioSelectiveColourMethod,
        reds: [f64; 4],
        yellows: [f64; 4],
        greens: [f64; 4],
        cyans: [f64; 4],
        blues: [f64; 4],
        magentas: [f64; 4],
        whites: [f64; 4],
        neutrals: [f64; 4],
        blacks: [f64; 4],
    },
    /// The gradient's stops by value (STU-VEC-136 stop record: position and opacity percent,
    /// midpoint percent or null), at least two, positions non-decreasing.
    GradientMap {
        stops: Vec<crate::StudioGradientStop>,
        reverse: bool,
        dither: bool,
    },
    /// `lut` is the artifact reference (STU-RAS-151); `table` is its resolved cube carried by value
    /// beside the reference (as a contour carries its points, STU-RAS-158), absent when unresolved.
    ColourLookup {
        lut: String,
        lut_kind: StudioLutKind,
        dither: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        table: Option<StudioLutTable>,
    },
    /// The ten-field shape (STU-RAS-151): amounts and tone widths percent `0..=100`, radii
    /// pixels, colour correction and midtone contrast counts, clips percent.
    ShadowsHighlights {
        shadow_amount: f64,
        shadow_tone_width: f64,
        shadow_radius: f64,
        highlight_amount: f64,
        highlight_tone_width: f64,
        highlight_radius: f64,
        colour_correction: f64,
        midtone_contrast: f64,
        black_clip: f64,
        white_clip: f64,
    },
    /// HDR toning (`tone_compression`): method, exposure, gamma, compression.
    ToneCompression {
        method: StudioToneMethod,
        exposure: f64,
        gamma: f64,
        compression: f64,
    },
    /// Percent controls (hard `0..=100`, luminance and intensity `1..=200`) and the resolved
    /// source selection as statistics.
    MatchColour {
        luminance: f64,
        colour_intensity: f64,
        fade: f64,
        neutralize: bool,
        source: StudioMatchStatistics,
    },
    /// Fuzziness percent hard `0..=200` (derived), sampled colours, HSL shift.
    ReplaceColour {
        fuzziness: f64,
        samples: Vec<StudioColor>,
        shift: StudioHslShift,
    },
}

/// One channel-mixer output row: weights per input channel and the constant, percent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioMixerRow {
    pub weights: Vec<f64>,
    pub constant: f64,
}

/// Selective colour method (STU-RAS-151: `relative = 1 | absolute = 2`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioSelectiveColourMethod {
    Relative,
    Absolute,
}

/// Colour lookup kind (STU-RAS-151 `lut_kind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioLutKind {
    #[serde(rename = "3dlut")]
    Lut3d,
    Abstract,
    DeviceLink,
}

/// A resolved 3D LUT: `side^3` RGB entries `0..=1`, red fastest; side `2..=`[`MAX_LUT_SIDE`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioLutTable {
    pub side: u16,
    pub values: Vec<[f64; 3]>,
}

/// Tone-compression method (the spec names the enum, not its members; members are the four
/// shipped HDR-toning methods).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioToneMethod {
    ExposureGamma,
    HighlightCompression,
    LocalAdaptation,
    EqualizeHistogram,
}

/// Match-colour source selection resolved to per-channel statistics (`0..=1`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioMatchStatistics {
    pub source_mean: [f64; 3],
    pub source_std: [f64; 3],
    pub target_mean: [f64; 3],
    pub target_std: [f64; 3],
}

/// `StudioMask.density` / `.feather` and the invert state (STU-RAS-115, STU-RAS-124).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioMaskParameters {
    /// Percent, hard `0..=100`.
    #[schemars(range(min = 0.0, max = 100.0))]
    pub density: f64,
    /// Pixels, hard `0..=10000`.
    #[schemars(range(min = 0.0, max = 10000.0))]
    pub feather: f64,
    pub inverted: bool,
}

/// Channel of a Blend-If entry; the composite gray entry is one member (STU-RAS-159).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum StudioBlendChannel {
    Gray,
    Red,
    Green,
    Blue,
}

/// One Blend-If entry: eight values in `levels_0_255`, each pair `[start, end]` (STU-RAS-159).
/// Identity: `source_black = destination_black = [0, 0]`, `source_white = destination_white =
/// [255, 255]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioBlendRange {
    pub channel: StudioBlendChannel,
    pub source_black: [f64; 2],
    pub source_white: [f64; 2],
    pub destination_black: [f64; 2],
    pub destination_white: [f64; 2],
}
impl StudioBlendRange {
    /// The identity entry of a channel.
    pub fn identity(channel: StudioBlendChannel) -> Self {
        Self {
            channel,
            source_black: [0.0, 0.0],
            source_white: [255.0, 255.0],
            destination_black: [0.0, 0.0],
            destination_white: [255.0, 255.0],
        }
    }
}

/// STU-VEC-153 knockout state: `unknown = -1`, `disabled = 0`, `enabled = 1`, `inherited = 2`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioKnockout {
    Unknown,
    Disabled,
    Enabled,
    Inherited,
}
impl StudioKnockout {
    /// The captured discriminant (STU-VEC-153).
    pub const fn discriminant(self) -> i8 {
        match self {
            Self::Unknown => -1,
            Self::Disabled => 0,
            Self::Enabled => 1,
            Self::Inherited => 2,
        }
    }
}

/// Per-channel blend enablement (STU-RAS-041).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioChannelBlend {
    pub red: bool,
    pub green: bool,
    pub blue: bool,
}

/// The advanced-blending fields of a layer (STU-RAS-041). Every field is explicit: the spec states
/// no defaults for the toggles, so none are invented.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioAdvancedBlending {
    /// At most one entry per channel; empty = blend everything.
    pub blend_ranges: Vec<StudioBlendRange>,
    pub knockout: StudioKnockout,
    pub channel_blend: StudioChannelBlend,
    pub blend_interior_effects: bool,
    pub blend_clipped_effects: bool,
    pub transparency_shapes_layer: bool,
    /// Knockout depth when `knockout` is enabled (Photoshop shallow | deep, PSD `knko` 1 | 2; lane
    /// SMB-PS-ADJUST-STYLES). The spec has no depth field (declared gap, STU-RAS-041 names
    /// "knockout" only): absent = shallow, not encoded when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knockout_depth: Option<StudioKnockoutDepth>,
}

/// Knockout depth (see [`StudioAdvancedBlending::knockout_depth`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioKnockoutDepth {
    Shallow,
    Deep,
}

/// Contour point (`levels_0_255`, hard `0..=255` declared) with the smoothing-suppressing corner.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioContourPoint {
    pub input: f64,
    pub output: f64,
    pub corner: bool,
}
/// `StudioContour` (STU-RAS-158): name AND resolved point list, carried by value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioContour {
    pub name: String,
    pub points: Vec<StudioContourPoint>,
}

/// Document-level Global Light (STU-RAS-158): degrees.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioGlobalLight {
    #[schemars(range(min = -180.0, max = 180.0))]
    pub angle: f64,
    #[schemars(range(min = 0.0, max = 90.0))]
    pub altitude: f64,
}

/// Reference to a gradient or pattern primitive used by an effect.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioEffectFill {
    pub schema_id: String,
    pub object_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BevelStyle {
    InnerBevel,
    OuterBevel,
    Emboss,
    PillowEmboss,
    StrokeEmboss,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BevelTechnique {
    Smooth,
    ChiselHard,
    ChiselSoft,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BevelDirection {
    Up,
    Down,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StrokePosition {
    Inside,
    Outside,
    Centre,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StrokeFillType {
    Solid,
    Gradient,
    Pattern,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GlowTechnique {
    Softer,
    Precise,
}
/// What a blur effect reads (STU-FX-002 `render_scope`, members `layer` and `below`; `group` is not
/// a blur scope here). `layer` is the uniform Gaussian layer blur (STU-FX-013 "Gaussian Blur",
/// Figma `FOREGROUND_BLUR`); `below` blurs the backdrop under the layer (STU-FX-013 "Background
/// Blur (backdrop)", Figma `BACKGROUND_BLUR`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BlurScope {
    Layer,
    Below,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GlowSource {
    Edge,
    Centre,
}

/// Bevel & Emboss texture sub-block (STU-RAS-157; depth is signed).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioBevelTexture {
    pub pattern: StudioEffectFill,
    pub scale: f64,
    pub depth: f64,
    pub invert: bool,
    pub align_with_layer: bool,
    pub phase_x: f64,
    pub phase_y: f64,
}

/// Kind block of one effect instance (STU-RAS-157), tag `effect_kind`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "effect_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioEffectKind {
    BevelEmboss {
        style: BevelStyle,
        technique: BevelTechnique,
        direction: BevelDirection,
        depth: f64,
        size: f64,
        soften: f64,
        /// Degrees, hard `0..=90` (derived).
        altitude: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gloss_contour: Option<StudioContour>,
        highlight_mode: u16,
        highlight_colour: StudioColor,
        highlight_opacity: f64,
        shadow_mode: u16,
        shadow_colour: StudioColor,
        shadow_opacity: f64,
        anti_alias_gloss: bool,
        use_contour: bool,
        use_texture: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        texture: Option<StudioBevelTexture>,
    },
    Stroke {
        size: f64,
        position: StrokePosition,
        fill_type: StrokeFillType,
        /// Gradient or pattern content when `fill_type` is not `solid` (solid uses `colour`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<StudioEffectFill>,
        overprint: bool,
    },
    InnerShadow {
        distance: f64,
        choke: f64,
        size: f64,
    },
    InnerGlow {
        technique: GlowTechnique,
        source: GlowSource,
        choke: f64,
        size: f64,
        range: f64,
        jitter: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gradient: Option<StudioEffectFill>,
    },
    OuterGlow {
        technique: GlowTechnique,
        spread: f64,
        size: f64,
        range: f64,
        jitter: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gradient: Option<StudioEffectFill>,
    },
    Satin {
        distance: f64,
        size: f64,
        invert: bool,
    },
    ColourOverlay,
    GradientOverlay {
        gradient: StudioEffectFill,
        /// The spec's `type` (members not enumerated by the spec; stored as the source token).
        #[serde(rename = "type")]
        gradient_type: String,
        reverse: bool,
        dither: bool,
        align_with_layer: bool,
        scale: f64,
        offset_x: f64,
        offset_y: f64,
        /// The referenced gradient's stops carried by value beside the reference (as a contour
        /// carries its points, STU-RAS-158), so a renderer needs no gradient resolver; absent =
        /// unresolved (lane SMB-PS-ADJUST-STYLES). Not encoded when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stops: Option<Vec<crate::StudioGradientStop>>,
    },
    PatternOverlay {
        pattern: StudioEffectFill,
        scale: f64,
        phase_x: f64,
        phase_y: f64,
        link_with_layer: bool,
    },
    DropShadow {
        distance: f64,
        spread: f64,
        size: f64,
        layer_knocks_out: bool,
    },
    /// Gaussian blur of the layer or of its backdrop (added by SMB-FOLIO-PAINT-BLUR-STYLE; not a
    /// STU-RAS-157 Photoshop layer-style kind: the blur family is STU-FX-013, and STU-FX-031 dedups
    /// Figma's layer and background blur into it). `radius` is the Gaussian radius in pixels,
    /// `0..=`[`MAX_BLUR_RADIUS`]. The common block's `colour` and `contour` must be absent (a blur has
    /// neither); `angle`, `noise` and `use_global_light` are stored but not read. No renderer
    /// draws it yet: composite lowering refuses the kind with its generic effect refusal until a
    /// composite lane implements it.
    Blur { render_scope: BlurScope, radius: f64 },
}
impl StudioEffectKind {
    pub const fn key(&self) -> &'static str {
        match self {
            Self::BevelEmboss { .. } => "bevel_emboss",
            Self::Stroke { .. } => "stroke",
            Self::InnerShadow { .. } => "inner_shadow",
            Self::InnerGlow { .. } => "inner_glow",
            Self::OuterGlow { .. } => "outer_glow",
            Self::Satin { .. } => "satin",
            Self::ColourOverlay => "colour_overlay",
            Self::GradientOverlay { .. } => "gradient_overlay",
            Self::PatternOverlay { .. } => "pattern_overlay",
            Self::DropShadow { .. } => "drop_shadow",
            Self::Blur { .. } => "blur",
        }
    }
}

/// One effect instance: identity, flags, the STU-RAS-157 common block and its kind block.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioEffect {
    /// Stable instance id, unique within the stack (key grammar).
    pub instance_id: String,
    pub enabled: bool,
    pub visible_in_editor: bool,
    /// `StudioBlendMode` discriminant, layer-applicable.
    pub blend_mode: u16,
    /// Percent `0..=100`.
    pub opacity: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<StudioColor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contour: Option<StudioContour>,
    pub anti_alias: bool,
    /// Percent `0..=100`.
    pub noise: f64,
    /// Degrees `-180..=180`.
    pub angle: f64,
    pub use_global_light: bool,
    pub params: StudioEffectKind,
}

/// `StudioEffectStack` (STU-RAS-040, STU-RAS-156): ordered instances (render order = list order),
/// stack-wide master enable and scale (percent).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioEffectStack {
    pub effect_stack_id: String,
    pub enabled: bool,
    pub scale: f64,
    pub effects: Vec<StudioEffect>,
}

/// The raster records of one layer. Every member is optional; absent is not encoded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioRasterLayerRecord {
    pub layer_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adjustment: Option<StudioAdjustment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<StudioMaskParameters>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advanced_blending: Option<StudioAdvancedBlending>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effects: Option<StudioEffectStack>,
    /// Smart (live) filter stack (lane SMB-PS-SELECT-LIQUIFY-2, module `smart_filter`); absent
    /// is not encoded, so earlier record sets stay byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smart_filters: Option<super::StudioSmartFilterStack>,
}
impl StudioRasterLayerRecord {
    /// An empty entry for `layer_id`.
    pub fn new(layer_id: impl Into<String>) -> Self {
        Self {
            layer_id: layer_id.into(),
            adjustment: None,
            mask: None,
            advanced_blending: None,
            effects: None,
            smart_filters: None,
        }
    }
}

/// The raster records of one document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioRasterRecords {
    pub schema_id: RasterRecordsSchema,
    /// `SDOC` id of the annotated document.
    pub document_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_light: Option<StudioGlobalLight>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<StudioRasterLayerRecord>,
}
impl StudioRasterRecords {
    /// An empty set for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self {
            schema_id: RasterRecordsSchema::V1,
            document_id: document_id.into(),
            global_light: None,
            layers: Vec::new(),
        }
    }
    /// The entry of a layer.
    pub fn layer(&self, layer_id: &str) -> Option<&StudioRasterLayerRecord> {
        self.layers.iter().find(|l| l.layer_id == layer_id)
    }
}

/// JSON schema (draft 2020-12) of the raster record set.
pub fn raster_records_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<StudioRasterRecords>()
}

fn finite(v: f64) -> bool {
    v.is_finite()
}
fn nonneg(v: f64) -> bool {
    v.is_finite() && v >= 0.0
}
fn level(v: f64) -> bool {
    v.is_finite() && (0.0..=255.0).contains(&v)
}
fn check(ok: bool, target: &str, field: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(err(Code::InvalidField, format!("{target}/{field}")))
    }
}
fn blend_mode(mode: u16, target: &str) -> Result<()> {
    if !(1..=BLEND_LAST).contains(&mode) {
        return Err(err(Code::UnknownBlendMode, target));
    }
    if BLEND_TOOL_ONLY.contains(&mode) || mode == BLEND_PASS_THROUGH {
        return Err(err(Code::InapplicableBlendMode, target));
    }
    Ok(())
}
fn hsl(s: &StudioHslShift, saturation_min: f64, target: &str) -> Result<()> {
    // Ranges of the PSD `hue2` record (Adobe Photoshop File Formats Specification).
    check(
        s.hue.is_finite() && (-180.0..=180.0).contains(&s.hue),
        target,
        "hue",
    )?;
    check(
        s.saturation.is_finite() && (saturation_min..=100.0).contains(&s.saturation),
        target,
        "saturation",
    )?;
    check(
        s.lightness.is_finite() && (-100.0..=100.0).contains(&s.lightness),
        target,
        "lightness",
    )
}
fn points(pts: impl Iterator<Item = (f64, f64)> + Clone, target: &str, field: &str) -> Result<()> {
    let n = pts.clone().count();
    check((2..=MAX_CURVE_POINTS).contains(&n), target, field)?;
    check(
        pts.clone().all(|(i, o)| level(i) && level(o)),
        target,
        field,
    )?;
    let xs: Vec<f64> = pts.map(|(i, _)| i).collect();
    check(xs.windows(2).all(|w| w[0] < w[1]), target, field)
}
fn contour(c: &StudioContour, target: &str) -> Result<()> {
    check(label(&c.name), target, "contour/name")?;
    points(
        c.points.iter().map(|p| (p.input, p.output)),
        target,
        "contour/points",
    )
}
fn fill_ref(f: &StudioEffectFill, schema: &str, target: &str) -> Result<()> {
    check(
        f.schema_id == schema && !f.object_id.is_empty(),
        target,
        "fill",
    )
}
fn colour(c: &StudioColor, target: &str, cx: &mut Ctx) -> Result<()> {
    c.validate(target, cx)
}

fn validate_adjustment(a: &StudioAdjustment, target: &str, cx: &mut Ctx) -> Result<()> {
    match a {
        StudioAdjustment::Levels { records } => {
            let mut seen = BTreeSet::new();
            for r in records {
                check(seen.insert(r.channel), target, "levels/channel")?;
                check(
                    [r.input_black, r.input_white, r.output_black, r.output_white]
                        .into_iter()
                        .all(|v| v.is_finite() && (0.0..=1.0).contains(&v))
                        && r.input_black < r.input_white,
                    target,
                    "levels/points",
                )?;
                check(r.gamma.is_finite() && r.gamma > 0.0, target, "levels/gamma")?;
            }
        }
        StudioAdjustment::Curves { curves } => {
            let mut seen = BTreeSet::new();
            for c in curves {
                check(seen.insert(c.channel), target, "curves/channel")?;
                points(
                    c.points.iter().map(|p| (p.input, p.output)),
                    target,
                    "curves/points",
                )?;
            }
        }
        StudioAdjustment::HueSaturation {
            master,
            colorize,
            colorization,
            bands,
        } => {
            hsl(master, -100.0, target)?;
            hsl(colorization, if *colorize { 0.0 } else { -100.0 }, target)?;
            check(bands.len() <= MAX_HUE_BANDS, target, "hue_saturation/bands")?;
            for b in bands {
                check(
                    b.range.iter().all(|v| finite(*v)),
                    target,
                    "hue_saturation/range",
                )?;
                hsl(&b.shift, -100.0, target)?;
            }
        }
        StudioAdjustment::BrightnessContrast {
            brightness,
            contrast,
            ..
        } => {
            // Soft -150..150 (STU-RAS-151 derived); hard unknown: finite only.
            check(
                finite(*brightness) && finite(*contrast),
                target,
                "brightness_contrast",
            )?;
        }
        StudioAdjustment::Exposure {
            exposure,
            offset,
            gamma,
            ..
        } => {
            check(finite(*exposure) && finite(*offset), target, "exposure")?;
            check(gamma.is_finite() && *gamma > 0.0, target, "exposure/gamma")?;
        }
        StudioAdjustment::Invert => {}
        StudioAdjustment::Threshold {
            level: l,
            false_colour,
            true_colour,
        } => {
            check(level(*l), target, "threshold/level")?;
            for c in [false_colour, true_colour].into_iter().flatten() {
                colour(c, target, cx)?;
            }
        }
        StudioAdjustment::Posterize { levels } => {
            check((2..=255).contains(levels), target, "posterize/levels")?;
        }
        StudioAdjustment::Vibrance {
            vibrance,
            saturation,
        } => check(finite(*vibrance) && finite(*saturation), target, "vibrance")?,
        StudioAdjustment::ColourBalance {
            shadows,
            midtones,
            highlights,
            ..
        } => check(
            [shadows, midtones, highlights]
                .into_iter()
                .flatten()
                .all(|v| finite(*v)),
            target,
            "colour_balance",
        )?,
        StudioAdjustment::PhotoFilter {
            filter_colour,
            density,
            ..
        } => {
            colour(filter_colour, target, cx)?;
            check(percent(*density), target, "photo_filter/density")?;
        }
        StudioAdjustment::BlackAndWhite {
            red,
            yellow,
            green,
            cyan,
            blue,
            magenta,
            tint_hue,
            tint_saturation,
            ..
        } => {
            check(
                [red, yellow, green, cyan, blue, magenta]
                    .into_iter()
                    .all(|v| finite(*v)),
                target,
                "black_and_white/weights",
            )?;
            check(
                tint_hue.is_finite() && (0.0..=360.0).contains(tint_hue),
                target,
                "black_and_white/tint_hue",
            )?;
            check(percent(*tint_saturation), target, "black_and_white/tint_saturation")?;
        }
        StudioAdjustment::ChannelMixer { outputs, .. } => {
            check(
                (1..=MAX_MIXER_CHANNELS).contains(&outputs.len())
                    && outputs.iter().all(|r| {
                        (1..=MAX_MIXER_CHANNELS).contains(&r.weights.len())
                            && r.weights.iter().all(|v| finite(*v))
                            && finite(r.constant)
                    }),
                target,
                "channel_mixer/outputs",
            )?;
        }
        StudioAdjustment::SelectiveColour {
            reds,
            yellows,
            greens,
            cyans,
            blues,
            magentas,
            whites,
            neutrals,
            blacks,
            ..
        } => check(
            [
                reds, yellows, greens, cyans, blues, magentas, whites, neutrals, blacks,
            ]
            .into_iter()
            .flatten()
            .all(|v| v.is_finite() && (-100.0..=100.0).contains(v)),
            target,
            "selective_colour",
        )?,
        StudioAdjustment::GradientMap { stops, .. } => {
            check(
                (2..=MAX_GRADIENT_MAP_STOPS).contains(&stops.len()),
                target,
                "gradient_map/stops",
            )?;
            cx.nodes(stops.len(), target)?;
            for s in stops {
                check(
                    percent(s.position)
                        && percent(s.opacity)
                        && s.midpoint
                            .is_none_or(|m| m.is_finite() && (13.0..=87.0).contains(&m)),
                    target,
                    "gradient_map/stop",
                )?;
                colour(&s.colour, target, cx)?;
            }
            check(
                stops.windows(2).all(|w| match w {
                    [a, b] => a.position <= b.position,
                    _ => true,
                }),
                target,
                "gradient_map/order",
            )?;
        }
        StudioAdjustment::ColourLookup { lut, table, .. } => {
            check(!lut.is_empty() && label(lut), target, "colour_lookup/lut")?;
            if let Some(t) = table {
                // Side first: a hostile side is refused before the values are walked.
                check(
                    (2..=MAX_LUT_SIDE).contains(&t.side),
                    target,
                    "colour_lookup/side",
                )?;
                let s = usize::from(t.side);
                let n = s.checked_mul(s).and_then(|v| v.checked_mul(s));
                check(n == Some(t.values.len()), target, "colour_lookup/values")?;
                cx.nodes(t.values.len(), target)?;
                check(
                    t.values
                        .iter()
                        .flatten()
                        .all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
                    target,
                    "colour_lookup/values",
                )?;
            }
        }
        StudioAdjustment::ShadowsHighlights {
            shadow_amount,
            shadow_tone_width,
            shadow_radius,
            highlight_amount,
            highlight_tone_width,
            highlight_radius,
            colour_correction,
            midtone_contrast,
            black_clip,
            white_clip,
        } => {
            check(
                [shadow_amount, highlight_amount, shadow_tone_width, highlight_tone_width]
                    .into_iter()
                    .all(|v| percent(*v)),
                target,
                "shadows_highlights/amounts",
            )?;
            check(
                nonneg(*shadow_radius) && nonneg(*highlight_radius),
                target,
                "shadows_highlights/radius",
            )?;
            check(
                finite(*colour_correction) && finite(*midtone_contrast),
                target,
                "shadows_highlights/counts",
            )?;
            check(
                percent(*black_clip) && percent(*white_clip),
                target,
                "shadows_highlights/clips",
            )?;
        }
        StudioAdjustment::ToneCompression {
            exposure,
            gamma,
            compression,
            ..
        } => check(
            finite(*exposure) && gamma.is_finite() && *gamma > 0.0 && finite(*compression),
            target,
            "tone_compression",
        )?,
        StudioAdjustment::MatchColour {
            luminance,
            colour_intensity,
            fade,
            source,
            ..
        } => {
            check(
                [luminance, colour_intensity]
                    .into_iter()
                    .all(|v| v.is_finite() && (1.0..=200.0).contains(v))
                    && percent(*fade),
                target,
                "match_colour",
            )?;
            check(
                [
                    source.source_mean,
                    source.source_std,
                    source.target_mean,
                    source.target_std,
                ]
                .iter()
                .flatten()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
                target,
                "match_colour/source",
            )?;
        }
        StudioAdjustment::ReplaceColour {
            fuzziness,
            samples,
            shift,
        } => {
            check(
                fuzziness.is_finite() && (0.0..=200.0).contains(fuzziness),
                target,
                "replace_colour/fuzziness",
            )?;
            check(
                (1..=MAX_REPLACE_SAMPLES).contains(&samples.len()),
                target,
                "replace_colour/samples",
            )?;
            for s in samples {
                colour(s, target, cx)?;
            }
            hsl(shift, -100.0, target)?;
        }
    }
    Ok(())
}

fn validate_blending(a: &StudioAdvancedBlending, target: &str) -> Result<()> {
    let mut seen = BTreeSet::new();
    for r in &a.blend_ranges {
        check(seen.insert(r.channel), target, "blend_ranges/channel")?;
        for pair in [
            r.source_black,
            r.source_white,
            r.destination_black,
            r.destination_white,
        ] {
            check(
                level(pair[0]) && level(pair[1]) && pair[0] <= pair[1],
                target,
                "blend_ranges",
            )?;
        }
    }
    Ok(())
}

fn validate_effects(s: &StudioEffectStack, target: &str, cx: &mut Ctx) -> Result<()> {
    id(&s.effect_stack_id, "SEFX", target)?;
    check(s.scale.is_finite() && s.scale > 0.0, target, "scale")?;
    check(s.effects.len() <= MAX_EFFECTS, target, "effects")?;
    cx.nodes(s.effects.len(), target)?;
    let mut ids = BTreeSet::new();
    for e in &s.effects {
        cx.tick()?;
        let t = format!("{target}/{}", e.instance_id);
        check(
            key(&e.instance_id) && ids.insert(e.instance_id.as_str()),
            target,
            "instance_id",
        )?;
        blend_mode(e.blend_mode, &t)?;
        check(percent(e.opacity), &t, "opacity")?;
        check(percent(e.noise), &t, "noise")?;
        check(
            e.angle.is_finite() && (-180.0..=180.0).contains(&e.angle),
            &t,
            "angle",
        )?;
        if let Some(c) = &e.colour {
            colour(c, &t, cx)?;
        }
        if let Some(c) = &e.contour {
            contour(c, &t)?;
        }
        match &e.params {
            StudioEffectKind::BevelEmboss {
                depth,
                size,
                soften,
                altitude,
                gloss_contour,
                highlight_mode,
                highlight_colour,
                highlight_opacity,
                shadow_mode,
                shadow_colour,
                shadow_opacity,
                texture,
                ..
            } => {
                check(
                    finite(*depth) && nonneg(*size) && nonneg(*soften),
                    &t,
                    "bevel",
                )?;
                check(
                    altitude.is_finite() && (0.0..=90.0).contains(altitude),
                    &t,
                    "altitude",
                )?;
                if let Some(c) = gloss_contour {
                    contour(c, &t)?;
                }
                blend_mode(*highlight_mode, &t)?;
                blend_mode(*shadow_mode, &t)?;
                check(
                    percent(*highlight_opacity) && percent(*shadow_opacity),
                    &t,
                    "bevel/opacity",
                )?;
                colour(highlight_colour, &t, cx)?;
                colour(shadow_colour, &t, cx)?;
                if let Some(x) = texture {
                    fill_ref(&x.pattern, PATTERN_SCHEMA, &t)?;
                    check(
                        x.scale.is_finite()
                            && x.scale > 0.0
                            && finite(x.depth)
                            && finite(x.phase_x)
                            && finite(x.phase_y),
                        &t,
                        "texture",
                    )?;
                }
            }
            StudioEffectKind::Stroke {
                size,
                fill_type,
                fill,
                ..
            } => {
                check(nonneg(*size), &t, "size")?;
                match (fill_type, fill) {
                    (StrokeFillType::Solid, None) => {}
                    (StrokeFillType::Gradient, Some(f)) => fill_ref(f, GRADIENT_SCHEMA, &t)?,
                    (StrokeFillType::Pattern, Some(f)) => fill_ref(f, PATTERN_SCHEMA, &t)?,
                    _ => return Err(err(Code::InvalidField, format!("{t}/fill"))),
                }
            }
            StudioEffectKind::InnerShadow {
                distance,
                choke,
                size,
            } => check(
                nonneg(*distance) && nonneg(*choke) && nonneg(*size),
                &t,
                "inner_shadow",
            )?,
            StudioEffectKind::InnerGlow {
                choke,
                size,
                range,
                jitter,
                gradient,
                ..
            } => {
                check(
                    nonneg(*choke) && nonneg(*size) && nonneg(*range) && nonneg(*jitter),
                    &t,
                    "inner_glow",
                )?;
                if let Some(g) = gradient {
                    fill_ref(g, GRADIENT_SCHEMA, &t)?;
                }
            }
            StudioEffectKind::OuterGlow {
                spread,
                size,
                range,
                jitter,
                gradient,
                ..
            } => {
                check(
                    nonneg(*spread) && nonneg(*size) && nonneg(*range) && nonneg(*jitter),
                    &t,
                    "outer_glow",
                )?;
                if let Some(g) = gradient {
                    fill_ref(g, GRADIENT_SCHEMA, &t)?;
                }
            }
            StudioEffectKind::Satin { distance, size, .. } => {
                check(nonneg(*distance) && nonneg(*size), &t, "satin")?
            }
            StudioEffectKind::ColourOverlay => {}
            StudioEffectKind::GradientOverlay {
                gradient,
                gradient_type,
                scale,
                offset_x,
                offset_y,
                stops,
                ..
            } => {
                fill_ref(gradient, GRADIENT_SCHEMA, &t)?;
                check(label(gradient_type), &t, "type")?;
                check(
                    scale.is_finite() && *scale > 0.0 && finite(*offset_x) && finite(*offset_y),
                    &t,
                    "gradient_overlay",
                )?;
                if let Some(stops) = stops {
                    check(
                        (2..=MAX_GRADIENT_MAP_STOPS).contains(&stops.len()),
                        &t,
                        "gradient_overlay/stops",
                    )?;
                    cx.nodes(stops.len(), &t)?;
                    for s in stops {
                        check(
                            percent(s.position)
                                && percent(s.opacity)
                                && s.midpoint
                                    .is_none_or(|m| m.is_finite() && (13.0..=87.0).contains(&m)),
                            &t,
                            "gradient_overlay/stop",
                        )?;
                        colour(&s.colour, &t, cx)?;
                    }
                    check(
                        stops.windows(2).all(|w| match w {
                            [a, b] => a.position <= b.position,
                            _ => true,
                        }),
                        &t,
                        "gradient_overlay/order",
                    )?;
                }
            }
            StudioEffectKind::PatternOverlay {
                pattern,
                scale,
                phase_x,
                phase_y,
                ..
            } => {
                fill_ref(pattern, PATTERN_SCHEMA, &t)?;
                check(
                    scale.is_finite() && *scale > 0.0 && finite(*phase_x) && finite(*phase_y),
                    &t,
                    "pattern_overlay",
                )?;
            }
            StudioEffectKind::DropShadow {
                distance,
                spread,
                size,
                ..
            } => check(
                nonneg(*distance) && nonneg(*spread) && nonneg(*size),
                &t,
                "drop_shadow",
            )?,
            StudioEffectKind::Blur { radius, .. } => {
                check(
                    nonneg(*radius) && *radius <= MAX_BLUR_RADIUS,
                    &t,
                    "blur/radius",
                )?;
                check(
                    e.colour.is_none() && e.contour.is_none(),
                    &t,
                    "blur/colour_contour",
                )?;
            }
        }
    }
    Ok(())
}

/// Clip-base rule for adjustment layers that take part in the composition graph (called by the
/// document validator once the edges are valid). Each group's `inputs` are walked in
/// `input_ordinal` order, the sole stack order (STU-CMP-092), bottom first. The base of a clipped
/// layer is the nearest earlier input that is not itself clipped. A clipped layer whose base is an
/// `adjustment` layer is refused with [`Code::AdjustmentClipBase`], target = the clipped layer.
///
/// Reading of the spec: STU-RAS-015 defines a clipping mask as "a base layer's content/alpha"
/// clipping the layers above it, and STU-RAS-007 says an adjustment layer is applied to the layers
/// below "(or clipped to its parent)", which makes the adjustment layer the clipped layer, not the
/// base. No clause names an adjustment layer as a base, so the arrangement is refused (the pigment
/// render-plan IR refuses it too as `AdjustmentClipBase`); Photoshop behaviour is UNVERIFIED. An
/// adjustment layer that is itself clipped is accepted and is not a base for the layers above it.
/// Documents without adjustment layers in the graph are unaffected (test
/// `adjustment_clip_base_rule`).
pub(crate) fn validate_adjustment_clip_bases(
    d: &StudioDocument,
    token: &CancellationToken,
) -> Result<()> {
    if !d.layers.iter().any(|l| l.kind == "adjustment") {
        return Ok(());
    }
    let layers: BTreeMap<&str, &StudioLayer> =
        d.layers.iter().map(|l| (l.layer_id.as_str(), l)).collect();
    // (group layer, operation key) -> (input_ordinal, source layer) in the order of the edges.
    let mut stacks: BTreeMap<(&str, &str), Vec<(u64, &str)>> = BTreeMap::new();
    for e in &d.graph.edges {
        canceled(token)?;
        stacks
            .entry((&e.target.layer_id, &e.target.operation_key))
            .or_default()
            .push((e.input_ordinal, &e.source.layer_id));
    }
    for stack in stacks.values_mut() {
        canceled(token)?;
        stack.sort_unstable_by_key(|(ordinal, _)| *ordinal);
        let mut base: Option<&StudioLayer> = None;
        for (_, source) in stack.iter() {
            let Some(layer) = layers.get(source) else {
                continue;
            };
            if layer.appearance.as_ref().is_some_and(|a| a.clipped) {
                if base.is_some_and(|b| b.kind == "adjustment") {
                    return Err(err(Code::AdjustmentClipBase, &layer.layer_id));
                }
            } else {
                base = Some(layer);
            }
        }
    }
    Ok(())
}

/// The single raster-record validator: ids, document binding, spec ranges, the adjustment-layer
/// linkage in both directions and the mask-parameter precondition. Unresolved colour profiles are
/// reported as [`ResolutionIssue`]s; everything else is a typed refusal. Effect instances count
/// against `Budget::nodes`.
pub fn validate_raster_records(
    r: &StudioRasterRecords,
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
    if let Some(g) = &r.global_light {
        check(
            g.angle.is_finite()
                && (-180.0..=180.0).contains(&g.angle)
                && g.altitude.is_finite()
                && (0.0..=90.0).contains(&g.altitude),
            "global_light",
            "angle_altitude",
        )?;
    }
    let layers: BTreeMap<&str, &super::StudioLayer> =
        d.layers.iter().map(|l| (l.layer_id.as_str(), l)).collect();
    cx.nodes(r.layers.len(), "layers")?;
    let mut seen = BTreeSet::new();
    for e in &r.layers {
        cx.tick()?;
        let t = e.layer_id.as_str();
        id(t, "SLYR", t)?;
        if !seen.insert(t) {
            return Err(err(Code::DuplicateIdentity, t));
        }
        let layer = layers
            .get(t)
            .ok_or_else(|| err(Code::DanglingReference, t))?;
        let hosts_adjustment = layer.kind == "adjustment"
            && matches!(&layer.payload, Payload::PrimitiveReference { schema_id, object_id }
                if schema_id == ADJUSTMENT_SCHEMA && object_id == t);
        match &e.adjustment {
            Some(a) => {
                if !hosts_adjustment {
                    return Err(err(Code::PayloadMismatch, t));
                }
                validate_adjustment(a, t, &mut cx)?;
            }
            None if hosts_adjustment => return Err(err(Code::DanglingReference, t)),
            None => {}
        }
        if let Some(m) = &e.mask {
            if !layer.appearance.as_ref().is_some_and(|a| a.mask.is_some()) {
                return Err(err(Code::InvalidAppearance, t));
            }
            check(percent(m.density), t, "mask/density")?;
            check(
                m.feather.is_finite() && (0.0..=MAX_MASK_FEATHER).contains(&m.feather),
                t,
                "mask/feather",
            )?;
        }
        if let Some(a) = &e.advanced_blending {
            validate_blending(a, t)?;
        }
        if let Some(s) = &e.effects {
            validate_effects(s, t, &mut cx)?;
        }
        if let Some(s) = &e.smart_filters {
            super::smart_filter::validate_smart_filters(s, layer, &mut cx)?;
        }
    }
    // Every adjustment layer must have its adjustment record.
    for l in &d.layers {
        let hosts = l.kind == "adjustment"
            && matches!(&l.payload, Payload::PrimitiveReference { schema_id, .. } if schema_id == ADJUSTMENT_SCHEMA);
        if hosts
            && r.layer(&l.layer_id)
                .and_then(|e| e.adjustment.as_ref())
                .is_none()
        {
            return Err(err(Code::DanglingReference, &l.layer_id));
        }
    }
    Ok(cx.issues)
}
