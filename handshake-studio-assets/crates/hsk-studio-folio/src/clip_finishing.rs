//! Clip finishing records (lane SMB-PREMIERE-COLOR-TITLES): the per-document record set
//! `StudioClipFinishingRecords` (schema `hsk.studio.clip_finishing@1`) that carries what a clip of a
//! `StudioSequence` shows beyond its source window, beside the document like the composition record
//! set (`StudioDocument` and `StudioClip` gain no fields, so every existing document and clip
//! literal is unchanged).
//!
//! Spec basis: STU-VID-020 `effect_stack` "`StudioEffectStack` ([STU-FX-002]). Includes the clip's
//! intrinsic transform and opacity, which are ordinary keyframable properties"; STU-FX-002 "A
//! `StudioEffectStack` is an ordered list of `StudioLiveFilter` entries. Each entry carries:
//! `filter_kind` ..., typed parameter payload, ... an `enabled` flag"; STU-COL-160 (grade stage
//! order), STU-COL-220 (basic tone and white balance), STU-COL-221 (creative look and intensity),
//! STU-COL-222 (wheels), STU-COL-225/226 (tone and cross-channel curves), STU-COL-227 (secondary
//! key), STU-COL-228 (vignette), STU-COL-158 (a LUT is a content-addressed resource).
//!
//! Stored (data only; evaluation is Prism's and Pigment's, rendering composite's): per clip an
//! ordered effect stack; entry kind `color_grade` (the Studio "Color Grade" effect, import key
//! `ADBE Lumetri`) with its stage blocks. Studio readings, declared: the `filter_kind` keys are
//! snake_case forms of the Studio effect names (no canonical key is verified in STU-FX-126); a LUT
//! is referenced by artifact manifest id plus content digest (STU-COL-158 `content_hash`), not by
//! the STU-COL-221 registry slot index (the registry is not stored yet); curve points are on the
//! float `0..=1` domain (STU-COL-225 "an explicit domain declaration so a float pipeline can use
//! 0.0-1.0"); the cross-channel curves map the x axis (hue / 360, saturation or luminance) to
//! `0..=1` with 0.5 = no change. Not stored: `hdr_specular`, `tint_balance`, the kelvin
//! white-balance pair, the secondary refinement block (denoise, blur) and secondary sharpen
//! (UNVERIFIED Lumetri behaviour; gaps in the lane notes).
//!
//! Lane SMB-PR-SOURCE-MONITOR: `basic.sharpen` (STU-COL-220 row `sharpen`, hard -100..=100) and
//! `faded_film` (STU-COL-221 row `faded_film`, hard 0..=150, soft max 100), both omitted when 0 so
//! older records stay byte-identical; and `keyframes`: grade scalars animated as
//! [`StudioGradeKeyedParam`] streams in CLIP-LOCAL ticks (the keyframe rules of every other clip
//! effect entry), each param at most once, every key within the param's hard bound, vignette params
//! only on a grade with a vignette. The static field is the value where the stream has no keys.
//!
//! Further entry kinds (slice 2): `transform`, `opacity`, `crop`, `gaussian_blur` (keyed values in
//! clip-local ticks, the keyframe rules of a composition transform stream) and `pigment_filter`
//! (Pigment's live filter kinds, `liquify` refused). Slice 3: `templates` (STU-VID-070
//! `StudioMotionTemplate`, exposed controls of the five STU-VID-070a types with `can_animate` and
//! the HARD source-text font permissions, declared requirements) and `graphics` (a template placed
//! as a clip whose `source_ref` is the template primitive, with its control values). Slice 4:
//! `caption_tracks` (STU-VID-037: segments with text, timing and styling per caption track, a
//! language, at most one active track per sequence, constraints whose violations are display
//! warnings).
//!
//! Refusals: out-of-range values `InvalidField` (spec hard bounds); an unknown clip
//! `DanglingReference`; duplicate clip stacks or effect ids `DuplicateIdentity`; counts beyond the
//! hard ceilings `BudgetExceeded`. The records arrive from untrusted wire JSON: integer arithmetic is
//! denied (workflow V4-01), every list is counted before it is walked and charged to the node budget.
#![deny(clippy::arithmetic_side_effects)]
use super::composition::validate_keyed;
use super::{
    Budget, ClipSource, Code, ContentDigest, MAX_COMPOSITION_TICK, MAX_STREAM_ABS_VALUE, Result, StudioDocument, StudioKeyedValue,
    StudioLayerProperty, StudioSmartFilterKind, err, id, key, values::Ctx,
};
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

schema_token!(ClipFinishingSchema, "hsk.studio.clip_finishing@1");

/// Clips with an effect stack in one record set.
pub const MAX_CLIP_EFFECT_STACKS: usize = 262_144;
/// Entries of one clip effect stack.
pub const MAX_CLIP_EFFECTS: usize = 64;
/// Points of one grade curve (Pigment's curve evaluator holds 2..=19).
pub const MAX_GRADE_CURVE_POINTS: usize = 19;
/// Secondary (keyed) corrections of one grade.
pub const MAX_GRADE_SECONDARIES: usize = 8;
/// Studio cap on the magnitude of one wheel offset / gamma / gain component (the spec declares no
/// hard bound, STU-COL-222).
pub const MAX_WHEEL_COMPONENT: f64 = 16.0;

/// A LUT resource reference (STU-COL-158): caller-owned manifest handle plus content digest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioLutRef {
    pub artifact_manifest_id: String,
    pub content_digest: ContentDigest,
}

/// STU-COL-220 primary block (spec names and scales; identity is 0 except `saturation` 100).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct StudioGradeBasic {
    pub temperature: f64,
    pub tint: f64,
    /// Stops.
    pub exposure: f64,
    pub contrast: f64,
    pub highlights: f64,
    pub shadows: f64,
    pub whites: f64,
    pub blacks: f64,
    /// Percent, 100 = unchanged.
    pub saturation: f64,
    pub vibrance: f64,
    /// STU-COL-220 `sharpen`, hard -100..=100, 0 = none (lane SMB-PR-SOURCE-MONITOR).
    #[serde(skip_serializing_if = "is_default")]
    pub sharpen: f64,
}
impl Default for StudioGradeBasic {
    fn default() -> Self {
        Self {
            temperature: 0.0,
            tint: 0.0,
            exposure: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            saturation: 100.0,
            vibrance: 0.0,
            sharpen: 0.0,
        }
    }
}

/// STU-COL-225/226 curves: point lists `[x, y]` on `0..=1`; empty = identity.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct StudioGradeCurves {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub composite: Vec<[f64; 2]>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub red: Vec<[f64; 2]>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub green: Vec<[f64; 2]>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blue: Vec<[f64; 2]>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hue_vs_hue: Vec<[f64; 2]>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hue_vs_saturation: Vec<[f64; 2]>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hue_vs_luminance: Vec<[f64; 2]>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub luminance_vs_saturation: Vec<[f64; 2]>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub saturation_vs_saturation: Vec<[f64; 2]>,
}

/// One wheel band (STU-COL-222 `offset`, `gamma`, `gain` per channel).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct StudioWheelBand {
    pub offset: [f64; 3],
    pub gamma: [f64; 3],
    pub gain: [f64; 3],
}

/// STU-COL-222 wheels: overall plus shadow / midtone / highlight bands and the scalars.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct StudioGradeWheels {
    pub overall: StudioWheelBand,
    pub shadows: StudioWheelBand,
    pub midtones: StudioWheelBand,
    pub highlights: StudioWheelBand,
    pub contrast: f64,
    pub pivot: f64,
    pub input_saturation: f64,
    pub final_saturation: f64,
    pub low_mid_split: f64,
    pub mid_high_split: f64,
}
impl Default for StudioGradeWheels {
    fn default() -> Self {
        Self {
            overall: StudioWheelBand::default(),
            shadows: StudioWheelBand::default(),
            midtones: StudioWheelBand::default(),
            highlights: StudioWheelBand::default(),
            contrast: 0.0,
            pivot: 0.5,
            input_saturation: 1.0,
            final_saturation: 1.0,
            low_mid_split: 0.5,
            mid_high_split: 0.5,
        }
    }
}

/// One key axis range with a soft edge (hue in degrees `0..=360`, `lo > hi` wraps; saturation and
/// luminance as fractions `0..=1`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioKeyRange {
    pub lo: f64,
    pub hi: f64,
    pub soft: f64,
}

/// STU-COL-227 key (HSL ranges with soft edges; `invert` keys the complement).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioGradeKey {
    pub hue: StudioKeyRange,
    pub saturation: StudioKeyRange,
    pub luminance: StudioKeyRange,
    #[serde(default)]
    pub invert: bool,
}

/// STU-COL-227 secondary correction: key plus its own correction block (the secondary
/// temperature / tint are DIFFERENT parameters from the primary ones: hard -300..=300).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioGradeSecondary {
    pub key: StudioGradeKey,
    #[serde(default)]
    pub temperature: f64,
    #[serde(default)]
    pub tint: f64,
    #[serde(default)]
    pub contrast: f64,
    /// Percent, 100 = unchanged.
    #[serde(default = "hundred")]
    pub saturation: f64,
}
fn hundred() -> f64 {
    100.0
}

/// STU-COL-228 vignette.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioGradeVignette {
    pub amount: f64,
    pub midpoint: f64,
    pub roundness: f64,
    pub feather: f64,
}

/// The Studio "Color Grade" effect payload (stage blocks in STU-COL-160 order).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioColorGrade {
    /// Stage 1 input / technical LUT.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_lut: Option<StudioLutRef>,
    /// Stage 2 primary correction.
    #[serde(default)]
    pub basic: StudioGradeBasic,
    /// Stage 3 creative look LUT and its intensity (percent `0..=200`, default 100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub look: Option<StudioLutRef>,
    #[serde(default = "hundred")]
    pub look_intensity: f64,
    /// Stage 5 curves.
    #[serde(default)]
    pub curves: StudioGradeCurves,
    /// Stage 6 wheels.
    #[serde(default)]
    pub wheels: StudioGradeWheels,
    /// Stage 7 secondaries, in stack order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secondaries: Vec<StudioGradeSecondary>,
    /// Stage 8 vignette.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vignette: Option<StudioGradeVignette>,
    /// STU-COL-221 `faded_film`, hard 0..=150 (soft max 100), 0 = none (lane SMB-PR-SOURCE-MONITOR).
    #[serde(default, skip_serializing_if = "is_default")]
    pub faded_film: f64,
    /// Keyframed grade scalars (lane SMB-PR-SOURCE-MONITOR; module docs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyframes: Vec<StudioGradeKeyedParam>,
}

/// A grade scalar that can be keyframed (lane SMB-PR-SOURCE-MONITOR); the key names its field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioGradeParam {
    Temperature,
    Tint,
    Exposure,
    Contrast,
    Highlights,
    Shadows,
    Whites,
    Blacks,
    Saturation,
    Vibrance,
    Sharpen,
    LookIntensity,
    FadedFilm,
    WheelsContrast,
    WheelsPivot,
    InputSaturation,
    FinalSaturation,
    VignetteAmount,
    VignetteMidpoint,
    VignetteRoundness,
    VignetteFeather,
}
/// Most keyed params of one grade (one per [`StudioGradeParam`]).
pub const MAX_GRADE_KEYED_PARAMS: usize = 21;
impl StudioGradeParam {
    /// The param's hard bound (the record's validation bound).
    pub fn bounds(self) -> (f64, f64) {
        use StudioGradeParam as P;
        match self {
            P::Temperature | P::Tint | P::Contrast | P::Highlights | P::Shadows | P::Whites | P::Blacks => (-150.0, 150.0),
            P::Exposure => (-7.0, 7.0),
            P::Saturation => (0.0, 300.0),
            P::Vibrance | P::Sharpen => (-100.0, 100.0),
            P::LookIntensity => (0.0, 200.0),
            P::FadedFilm => (0.0, 150.0),
            P::WheelsContrast => (-1.0, 8.0),
            P::WheelsPivot => (0.0, 1.0),
            P::InputSaturation | P::FinalSaturation => (0.0, 2.0),
            P::VignetteAmount => (-5.0, 5.0),
            P::VignetteMidpoint | P::VignetteFeather => (0.0, 100.0),
            P::VignetteRoundness => (-100.0, 100.0),
        }
    }
    /// Writes `v`, clamped into [`StudioGradeParam::bounds`] (a sampled bezier may overshoot its
    /// keys), into its field of `g`; a non-finite `v` or a vignette param on a grade without a
    /// vignette changes nothing (validation refuses that record).
    pub fn set(self, g: &mut StudioColorGrade, v: f64) {
        use StudioGradeParam as P;
        if !v.is_finite() {
            return;
        }
        let (lo, hi) = self.bounds();
        let v = v.clamp(lo, hi);
        let b = &mut g.basic;
        match self {
            P::Temperature => b.temperature = v,
            P::Tint => b.tint = v,
            P::Exposure => b.exposure = v,
            P::Contrast => b.contrast = v,
            P::Highlights => b.highlights = v,
            P::Shadows => b.shadows = v,
            P::Whites => b.whites = v,
            P::Blacks => b.blacks = v,
            P::Saturation => b.saturation = v,
            P::Vibrance => b.vibrance = v,
            P::Sharpen => b.sharpen = v,
            P::LookIntensity => g.look_intensity = v,
            P::FadedFilm => g.faded_film = v,
            P::WheelsContrast => g.wheels.contrast = v,
            P::WheelsPivot => g.wheels.pivot = v,
            P::InputSaturation => g.wheels.input_saturation = v,
            P::FinalSaturation => g.wheels.final_saturation = v,
            P::VignetteAmount => {
                if let Some(vg) = &mut g.vignette {
                    vg.amount = v;
                }
            }
            P::VignetteMidpoint => {
                if let Some(vg) = &mut g.vignette {
                    vg.midpoint = v;
                }
            }
            P::VignetteRoundness => {
                if let Some(vg) = &mut g.vignette {
                    vg.roundness = v;
                }
            }
            P::VignetteFeather => {
                if let Some(vg) = &mut g.vignette {
                    vg.feather = v;
                }
            }
        }
    }
    fn is_vignette(self) -> bool {
        matches!(self, Self::VignetteAmount | Self::VignetteMidpoint | Self::VignetteRoundness | Self::VignetteFeather)
    }
}

/// One keyframed grade scalar: a one-dimensional keyed stream in clip-local ticks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioGradeKeyedParam {
    pub param: StudioGradeParam,
    pub value: StudioKeyedValue,
}
impl Default for StudioColorGrade {
    fn default() -> Self {
        Self {
            input_lut: None,
            basic: StudioGradeBasic::default(),
            look: None,
            look_intensity: 100.0,
            curves: StudioGradeCurves::default(),
            wheels: StudioGradeWheels::default(),
            secondaries: Vec::new(),
            vignette: None,
            faded_film: 0.0,
            keyframes: Vec::new(),
        }
    }
}

/// Typed payload of one clip effect, tag `filter_kind`. Keyframe times of every keyed value are
/// CLIP-LOCAL ticks (ticks since the clip's `timeline_start`; Studio reading: STU-VID-022a scopes
/// a clip's keyframe timeline to the clip).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "filter_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioClipEffectParams {
    /// The Studio "Color Grade" effect (Premiere Lumetri Color).
    ColorGrade(Box<StudioColorGrade>),
    /// The clip's intrinsic / Studio "Transform" effect in sequence-frame pixels: `anchor` (2),
    /// `position` (2), `scale` (2, percent, 100 = unscaled), `rotation` (1, degrees, clockwise).
    Transform {
        anchor: StudioKeyedValue,
        position: StudioKeyedValue,
        scale: StudioKeyedValue,
        rotation: StudioKeyedValue,
    },
    /// The clip's intrinsic opacity, percent `0..=100`.
    Opacity { opacity: StudioKeyedValue },
    /// The Studio "Crop" effect: percent of the clip frame removed from each edge, `0..=100`.
    Crop {
        left: StudioKeyedValue,
        top: StudioKeyedValue,
        right: StudioKeyedValue,
        bottom: StudioKeyedValue,
    },
    /// The Studio "Gaussian Blur" effect: `blurriness` in pixels `0..=`[`MAX_CLIP_BLUR_PX`].
    GaussianBlur { blurriness: StudioKeyedValue },
    /// One of Pigment's live filters (the smart-filter kinds; `liquify` is layer-bound and refused).
    PigmentFilter { filter: StudioSmartFilterKind },
}

/// Largest Gaussian blurriness a clip effect stores (pixels; Studio bound, the catalogue row
/// declares no bound).
pub const MAX_CLIP_BLUR_PX: f64 = 1000.0;

/// One entry of a clip effect stack (STU-FX-002).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioClipEffect {
    /// Unique within the record set (key grammar).
    pub effect_id: String,
    pub enabled: bool,
    pub params: StudioClipEffectParams,
}

/// The ordered effect stack of one clip (first entry applied first).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioClipEffectStack {
    /// `SCLP` id of a clip of the document.
    pub clip_id: String,
    pub effects: Vec<StudioClipEffect>,
}

/// Wire schema id of a template placed as a clip (`ClipSource::Primitive.schema_id`).
pub const MOTION_TEMPLATE_SCHEMA: &str = "hsk.studio.motion_template@1";
/// Templates in one record set.
pub const MAX_TEMPLATES: usize = 1024;
/// Exposed controls of one template (the recovered shipped set has 461 over 77 templates).
pub const MAX_TEMPLATE_CONTROLS: usize = 512;
/// Required fonts / effects of one template.
pub const MAX_TEMPLATE_REQUIREMENTS: usize = 64;
/// Longest source-text value or name, in bytes.
pub const MAX_TEMPLATE_TEXT_BYTES: usize = 4096;

/// Where a colour control writes (Studio readings of STU-VID-070a `colour`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "target", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioColourTarget {
    /// A solid `fill` item of a shape layer, addressed by its item path in the shape tree.
    ShapeFill { layer_id: String, path: Vec<u32> },
    /// The fill of a text layer (handed to the caller's glyph preparation).
    TextFill { layer_id: String },
}

/// STU-VID-070a exposed-control types (`control_type` tag).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "control_type", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioControlKind {
    /// Localised text bound to a text layer's source text with HARD font-editing permissions.
    SourceText {
        layer_id: String,
        default_text: String,
        font_family_editable: bool,
        faux_style_editable: bool,
        font_size_editable: bool,
        locked_font: String,
        locked_size: f64,
        #[serde(default)]
        all_caps: bool,
        #[serde(default)]
        small_caps: bool,
        #[serde(default)]
        bold: bool,
        #[serde(default)]
        italic: bool,
    },
    /// A section header; structural, no value.
    Group { heads_layer_kind: String },
    /// Linear RGBA `0..=1`.
    Colour { target: StudioColourTarget, default: [f64; 4] },
    /// Boolean (Studio reading: the layer renders when checked, opacity 0 otherwise).
    Checkbox { layer_id: String, default: bool },
    /// Numeric with SOFT bounds `min`/`max` (`declared_soft_only`); the property's own hard
    /// bounds govern acceptance. Writes one component of a composition layer transform stream.
    NumericSlider {
        layer_id: String,
        property: StudioLayerProperty,
        component: u8,
        min: f64,
        max: f64,
        default: f64,
    },
}

/// One exposed control (STU-VID-070, STU-VID-070b `can_animate`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioExposedControl {
    pub control_id: String,
    pub name: String,
    pub can_animate: bool,
    pub control: StudioControlKind,
}

/// `StudioMotionTemplate` (`hsk.studio.motion_template@1`, STU-VID-070): a composition of the
/// document's composition records plus its ordered exposed controls and its declared requirements
/// (STU-VID-070c fonts, effects, renderer).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioMotionTemplate {
    pub template_id: String,
    pub name: String,
    pub composition_id: String,
    pub controls: Vec<StudioExposedControl>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_fonts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects_used: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<String>,
}

/// One control value of a placed template.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "value_type", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioControlSetting {
    /// Source text plus the font changes the template permits (refused when not permitted).
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        font_family: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        font_size: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        faux_bold: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        faux_italic: Option<bool>,
    },
    Colour { rgba: [f64; 4] },
    Checkbox { checked: bool },
    /// Keyframes (clip-local ticks) only when the control `can_animate`.
    Number { value: StudioKeyedValue },
}

/// A control value keyed by control id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioControlValue {
    pub control_id: String,
    pub setting: StudioControlSetting,
}

/// A graphic clip: a template placed on a track (the clip's `source_ref` is
/// `Primitive { schema_id: "hsk.studio.motion_template@1", object_id: template_id }`), with the
/// values the person set; unset controls take the template defaults.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioGraphicInstance {
    pub clip_id: String,
    pub template_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<StudioControlValue>,
}

fn label_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_TEMPLATE_TEXT_BYTES && !s.chars().any(char::is_control)
}
fn rgba_ok(c: &[f64; 4]) -> bool {
    c.iter().all(|v| within(*v, 0.0, 1.0))
}
/// The hard acceptance range of a layer transform property (opacity percent `0..=100`).
fn property_range(p: StudioLayerProperty) -> (f64, f64) {
    match p {
        StudioLayerProperty::Opacity => (0.0, 100.0),
        _ => (-MAX_STREAM_ABS_VALUE, MAX_STREAM_ABS_VALUE),
    }
}

fn template(tpl: &StudioMotionTemplate, cx: &mut Ctx) -> Result<()> {
    cx.tick()?;
    let t = tpl.template_id.as_str();
    check(key(t), "templates", "template_id")?;
    check(key(&tpl.composition_id), t, "composition_id")?;
    check(label_ok(&tpl.name), t, "name")?;
    cx.text(&tpl.name, t)?;
    if tpl.controls.len() > MAX_TEMPLATE_CONTROLS
        || tpl.required_fonts.len() > MAX_TEMPLATE_REQUIREMENTS
        || tpl.effects_used.len() > MAX_TEMPLATE_REQUIREMENTS
    {
        return Err(err(Code::BudgetExceeded, format!("{t}/controls")));
    }
    cx.nodes(tpl.controls.len(), t)?;
    for s in tpl.required_fonts.iter().chain(&tpl.effects_used).chain(&tpl.renderer) {
        check(label_ok(s), t, "requirements")?;
        cx.text(s, t)?;
    }
    let mut ids = BTreeSet::new();
    for c in &tpl.controls {
        cx.tick()?;
        check(key(&c.control_id) && ids.insert(c.control_id.as_str()), t, "controls/control_id")?;
        check(label_ok(&c.name), t, "controls/name")?;
        cx.text(&c.name, t)?;
        let ct = format!("{t}/controls/{}", c.control_id);
        match &c.control {
            StudioControlKind::SourceText { layer_id, default_text, locked_font, locked_size, .. } => {
                id(layer_id, "SLYR", "controls/layer_id")?;
                check(default_text.len() <= MAX_TEMPLATE_TEXT_BYTES, &ct, "default_text")?;
                cx.text(default_text, &ct)?;
                check(label_ok(locked_font), &ct, "locked_font")?;
                check(within(*locked_size, 0.0, 10_000.0) && *locked_size > 0.0, &ct, "locked_size")?;
                check(!c.can_animate, &ct, "can_animate")?;
            }
            StudioControlKind::Group { heads_layer_kind } => {
                check(label_ok(heads_layer_kind), &ct, "heads_layer_kind")?;
                check(!c.can_animate, &ct, "can_animate")?;
            }
            StudioControlKind::Colour { target, default } => {
                match target {
                    StudioColourTarget::ShapeFill { layer_id, path } => {
                        id(layer_id, "SLYR", "controls/target")?;
                        check(path.len() <= 64, &ct, "target/path")?;
                    }
                    StudioColourTarget::TextFill { layer_id } => id(layer_id, "SLYR", "controls/target")?,
                }
                check(rgba_ok(default), &ct, "default")?;
            }
            StudioControlKind::Checkbox { layer_id, .. } => id(layer_id, "SLYR", "controls/layer_id")?,
            StudioControlKind::NumericSlider { layer_id, property, component, min, max, default } => {
                id(layer_id, "SLYR", "controls/layer_id")?;
                check(usize::from(*component) < property.dimension(), &ct, "component")?;
                let (lo, hi) = property_range(*property);
                check(
                    within(*min, lo, hi) && within(*max, lo, hi) && min < max && within(*default, *min, *max),
                    &ct,
                    "bounds",
                )?;
            }
        }
    }
    Ok(())
}

fn graphic(g: &StudioGraphicInstance, d: &StudioDocument, templates: &[StudioMotionTemplate], cx: &mut Ctx) -> Result<()> {
    cx.tick()?;
    let t = g.clip_id.as_str();
    id(t, "SCLP", "graphics/clip_id")?;
    let clip = d
        .sequences
        .iter()
        .flat_map(|s| s.tracks.iter())
        .flat_map(|tr| tr.clips.iter())
        .find(|c| c.clip_id == t)
        .ok_or_else(|| err(Code::DanglingReference, t))?;
    let placed = matches!(&clip.source_ref, ClipSource::Primitive { schema_id, object_id }
        if schema_id == MOTION_TEMPLATE_SCHEMA && *object_id == g.template_id);
    check(placed, t, "source_ref")?;
    let tpl = templates
        .iter()
        .find(|x| x.template_id == g.template_id)
        .ok_or_else(|| err(Code::DanglingReference, format!("{t}/template_id")))?;
    if g.values.len() > MAX_TEMPLATE_CONTROLS {
        return Err(err(Code::BudgetExceeded, format!("{t}/values")));
    }
    cx.nodes(g.values.len(), t)?;
    let mut seen = BTreeSet::new();
    for v in &g.values {
        cx.tick()?;
        check(seen.insert(v.control_id.as_str()), t, "values/control_id")?;
        let c = tpl
            .controls
            .iter()
            .find(|c| c.control_id == v.control_id)
            .ok_or_else(|| err(Code::DanglingReference, format!("{t}/values/{}", v.control_id)))?;
        let vt = format!("{t}/values/{}", v.control_id);
        match (&c.control, &v.setting) {
            (
                StudioControlKind::SourceText { font_family_editable, faux_style_editable, font_size_editable, .. },
                StudioControlSetting::Text { text, font_family, font_size, faux_bold, faux_italic },
            ) => {
                check(text.len() <= MAX_TEMPLATE_TEXT_BYTES, &vt, "text")?;
                cx.text(text, &vt)?;
                // STU-VID-070a: the permissions are HARD, never a UI hint.
                check(font_family.is_none() || *font_family_editable, &vt, "font_family_locked")?;
                check(font_size.is_none() || *font_size_editable, &vt, "font_size_locked")?;
                check((faux_bold.is_none() && faux_italic.is_none()) || *faux_style_editable, &vt, "faux_style_locked")?;
                if let Some(f) = font_family {
                    check(label_ok(f), &vt, "font_family")?;
                }
                check(font_size.is_none_or(|s| within(s, 0.0, 10_000.0) && s > 0.0), &vt, "font_size")?;
            }
            (StudioControlKind::Colour { .. }, StudioControlSetting::Colour { rgba }) => check(rgba_ok(rgba), &vt, "rgba")?,
            (StudioControlKind::Checkbox { .. }, StudioControlSetting::Checkbox { .. }) => {}
            (StudioControlKind::NumericSlider { property, .. }, StudioControlSetting::Number { value }) => {
                // STU-VID-070b: keyframes only on a control that can animate.
                check(value.keyframes.is_empty() || c.can_animate, &vt, "can_animate")?;
                let (lo, hi) = property_range(*property);
                keyed(value, 1, lo, hi, &vt, "value", cx)?;
            }
            _ => return Err(err(Code::InvalidField, format!("{vt}/value_type"))),
        }
    }
    Ok(())
}

/// Caption segments of one caption track (a caption-bomb ceiling, checked before any segment).
pub const MAX_CAPTION_SEGMENTS: usize = 100_000;
/// Longest caption text in bytes (lines separated by `\n`).
pub const MAX_CAPTION_TEXT_BYTES: usize = 4096;
/// Caption tracks in one record set.
pub const MAX_CAPTION_TRACKS: usize = 256;

/// Vertical placement of a caption (SRT `{\an8}` / WebVTT `line` read as top or bottom).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioCaptionPlacement {
    #[default]
    Bottom,
    Top,
}
/// Horizontal alignment of a caption.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioCaptionAlign {
    Left,
    #[default]
    Center,
    Right,
}
fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

/// Segment styling (STU-VID-037 "styling"; the per-format styling model is the declared gap
/// STU-VID-085, so this is the Studio subset both SRT and WebVTT can carry: whole-segment bold,
/// italic, underline, an optional colour, placement and alignment).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct StudioCaptionStyle {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub bold: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub italic: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub underline: bool,
    /// Straight RGBA `0..=1`; `None` = the track's default (white).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colour: Option<[f64; 4]>,
    #[serde(skip_serializing_if = "is_default")]
    pub placement: StudioCaptionPlacement,
    #[serde(skip_serializing_if = "is_default")]
    pub align: StudioCaptionAlign,
}

/// One caption segment: `[start, end)` in SEQUENCE ticks, text (lines separated by `\n`), style.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioCaptionSegment {
    pub start: u64,
    pub end: u64,
    pub text: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub style: StudioCaptionStyle,
}

/// The track's declared caption constraints (STU-VID-037: segments that violate them carry a
/// validation-warning display state; they are warnings, not refusals).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct StudioCaptionConstraints {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_line_chars: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_lines: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_duration: Option<u64>,
}

/// A caption track's records (STU-VID-037): the document track (kind `caption`) they belong to, its
/// language, whether it is the active display track, constraints and segments ordered by start
/// (segments may overlap: two speakers can caption at once).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioCaptionTrack {
    pub track_id: String,
    /// Language tag (BCP 47 letters, digits and `-`, 1..=35 bytes).
    pub language: String,
    pub active: bool,
    #[serde(default, skip_serializing_if = "is_default")]
    pub constraints: StudioCaptionConstraints,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<StudioCaptionSegment>,
}

/// Why a segment carries the validation-warning display state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptionWarning {
    LineTooLong,
    TooManyLines,
    TooShort,
}

impl StudioCaptionTrack {
    /// Segments showing at sequence tick `tick` (in start order).
    pub fn segments_at(&self, tick: u64) -> impl Iterator<Item = &StudioCaptionSegment> {
        let end = self.segments.partition_point(|s| s.start <= tick);
        self.segments.get(..end).unwrap_or(&[]).iter().filter(move |s| tick < s.end)
    }
    /// Segment indices that violate the track constraints, with the reason (display state).
    pub fn warnings(&self) -> Vec<(usize, CaptionWarning)> {
        let c = &self.constraints;
        let mut out = Vec::new();
        for (i, s) in self.segments.iter().enumerate() {
            let lines = s.text.split('\n');
            let n = |m: u32| usize::try_from(m).unwrap_or(usize::MAX);
            if c.max_line_chars.is_some_and(|m| lines.clone().any(|l| l.chars().count() > n(m))) {
                out.push((i, CaptionWarning::LineTooLong));
            }
            if c.max_lines.is_some_and(|m| lines.count() > n(m)) {
                out.push((i, CaptionWarning::TooManyLines));
            }
            if c.min_duration.is_some_and(|m| s.end.saturating_sub(s.start) < m) {
                out.push((i, CaptionWarning::TooShort));
            }
        }
        out
    }
}

fn caption_track(c: &StudioCaptionTrack, d: &StudioDocument, cx: &mut Ctx) -> Result<Option<String>> {
    cx.tick()?;
    let t = c.track_id.as_str();
    id(t, "STRK", "caption_tracks/track_id")?;
    let (seq, track) = d
        .sequences
        .iter()
        .find_map(|s| s.tracks.iter().find(|tr| tr.track_id == t).map(|tr| (s, tr)))
        .ok_or_else(|| err(Code::DanglingReference, t))?;
    check(track.kind == super::TrackKind::Caption, t, "kind")?;
    check(
        !c.language.is_empty() && c.language.len() <= 35 && c.language.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        t,
        "language",
    )?;
    if c.segments.len() > MAX_CAPTION_SEGMENTS {
        return Err(err(Code::BudgetExceeded, format!("{t}/segments")));
    }
    cx.nodes(c.segments.len(), t)?;
    let mut previous = 0u64;
    for s in &c.segments {
        cx.tick()?;
        check(s.start < s.end && s.start >= previous, t, "segments/time")?;
        previous = s.start;
        check(s.end <= MAX_COMPOSITION_TICK, t, "segments/time")?;
        check(
            s.text.len() <= MAX_CAPTION_TEXT_BYTES && !s.text.chars().any(|ch| ch.is_control() && ch != '\n'),
            t,
            "segments/text",
        )?;
        cx.text(&s.text, t)?;
        check(s.style.colour.is_none_or(|c| rgba_ok(&c)), t, "segments/style/colour")?;
    }
    Ok(c.active.then(|| seq.sequence_id.clone()))
}

/// The per-document clip finishing record set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioClipFinishingRecords {
    pub schema_id: ClipFinishingSchema,
    /// `SDOC` id of the annotated document.
    pub document_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effect_stacks: Vec<StudioClipEffectStack>,
    /// Motion-graphics templates (STU-VID-070) available to the document's graphic clips.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub templates: Vec<StudioMotionTemplate>,
    /// Graphic clips: a template placed as a clip with its control values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub graphics: Vec<StudioGraphicInstance>,
    /// Caption track records (STU-VID-037), one per document caption track that holds segments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caption_tracks: Vec<StudioCaptionTrack>,
}
impl StudioClipFinishingRecords {
    /// An empty set for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self {
            schema_id: ClipFinishingSchema::V1,
            document_id: document_id.into(),
            effect_stacks: Vec::new(),
            templates: Vec::new(),
            graphics: Vec::new(),
            caption_tracks: Vec::new(),
        }
    }
    /// The caption records of document track `track_id`.
    pub fn caption_track(&self, track_id: &str) -> Option<&StudioCaptionTrack> {
        self.caption_tracks.iter().find(|c| c.track_id == track_id)
    }
    /// The template `template_id`.
    pub fn template(&self, template_id: &str) -> Option<&StudioMotionTemplate> {
        self.templates.iter().find(|t| t.template_id == template_id)
    }
    /// The graphic instance of clip `clip_id`.
    pub fn graphic(&self, clip_id: &str) -> Option<&StudioGraphicInstance> {
        self.graphics.iter().find(|g| g.clip_id == clip_id)
    }
    /// The effect stack of `clip_id`.
    pub fn stack(&self, clip_id: &str) -> Option<&StudioClipEffectStack> {
        self.effect_stacks.iter().find(|s| s.clip_id == clip_id)
    }
}

/// JSON schema (draft 2020-12) of the clip finishing record set.
pub fn clip_finishing_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<StudioClipFinishingRecords>()
}

fn check(ok: bool, target: &str, field: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(err(Code::InvalidField, format!("{target}/{field}")))
    }
}
fn within(v: f64, lo: f64, hi: f64) -> bool {
    v.is_finite() && v >= lo && v <= hi
}

fn lut_ref(r: &StudioLutRef, t: &str, field: &str, cx: &mut Ctx) -> Result<()> {
    cx.text(&r.artifact_manifest_id, t)?;
    cx.text(&r.content_digest.digest, t)?;
    check(
        !r.artifact_manifest_id.is_empty()
            && r.artifact_manifest_id.len() <= super::MAX_HANDLE_BYTES
            && !r.artifact_manifest_id.chars().any(char::is_control)
            && !r.content_digest.algorithm.is_empty()
            && r.content_digest.algorithm.len() <= 32
            && !r.content_digest.digest.is_empty()
            && r.content_digest.digest.len() <= super::MAX_HANDLE_BYTES
            && r.content_digest.digest.bytes().all(|b| b.is_ascii_alphanumeric()),
        t,
        field,
    )
}

fn curve(points: &[[f64; 2]], t: &str, field: &str, cx: &mut Ctx) -> Result<()> {
    if points.len() > MAX_GRADE_CURVE_POINTS {
        return Err(err(Code::BudgetExceeded, format!("{t}/{field}")));
    }
    cx.nodes(points.len(), t)?;
    check(
        points.is_empty()
            || (points.len() >= 2
                && points.iter().all(|p| within(p[0], 0.0, 1.0) && within(p[1], 0.0, 1.0))
                && points.windows(2).all(|w| w[0][0] < w[1][0])),
        t,
        field,
    )
}

fn band(b: &StudioWheelBand, t: &str, field: &str) -> Result<()> {
    check(
        b.offset
            .iter()
            .chain(&b.gamma)
            .chain(&b.gain)
            .all(|v| within(*v, -MAX_WHEEL_COMPONENT, MAX_WHEEL_COMPONENT)),
        t,
        field,
    )
}

fn key_range(r: &StudioKeyRange, hue: bool, t: &str, field: &str) -> Result<()> {
    let ok = if hue {
        within(r.lo, 0.0, 360.0) && within(r.hi, 0.0, 360.0) && within(r.soft, 0.0, 180.0)
    } else {
        within(r.lo, 0.0, 1.0) && within(r.hi, 0.0, 1.0) && r.lo <= r.hi && within(r.soft, 0.0, 1.0)
    };
    check(ok, t, field)
}

fn color_grade(g: &StudioColorGrade, t: &str, cx: &mut Ctx) -> Result<()> {
    if let Some(r) = &g.input_lut {
        lut_ref(r, t, "input_lut", cx)?;
    }
    if let Some(r) = &g.look {
        lut_ref(r, t, "look", cx)?;
    }
    let b = &g.basic;
    for (v, name) in [
        (b.temperature, "basic/temperature"),
        (b.tint, "basic/tint"),
        (b.contrast, "basic/contrast"),
        (b.highlights, "basic/highlights"),
        (b.shadows, "basic/shadows"),
        (b.whites, "basic/whites"),
        (b.blacks, "basic/blacks"),
    ] {
        check(within(v, -150.0, 150.0), t, name)?;
    }
    check(within(b.exposure, -7.0, 7.0), t, "basic/exposure")?;
    check(within(b.saturation, 0.0, 300.0), t, "basic/saturation")?;
    check(within(b.vibrance, -100.0, 100.0), t, "basic/vibrance")?;
    check(within(b.sharpen, -100.0, 100.0), t, "basic/sharpen")?;
    check(within(g.faded_film, 0.0, 150.0), t, "faded_film")?;
    check(within(g.look_intensity, 0.0, 200.0), t, "look_intensity")?;
    if g.keyframes.len() > MAX_GRADE_KEYED_PARAMS {
        return Err(err(Code::BudgetExceeded, format!("{t}/keyframes")));
    }
    cx.nodes(g.keyframes.len(), t)?;
    let mut seen = BTreeSet::new();
    for k in &g.keyframes {
        cx.tick()?;
        if !seen.insert(k.param) {
            return Err(err(Code::DuplicateIdentity, format!("{t}/keyframes")));
        }
        check(!k.param.is_vignette() || g.vignette.is_some(), t, "keyframes/vignette")?;
        let (lo, hi) = k.param.bounds();
        keyed(&k.value, 1, lo, hi, t, "keyframes/value", cx)?;
    }
    let c = &g.curves;
    for (points, name) in [
        (&c.composite, "curves/composite"),
        (&c.red, "curves/red"),
        (&c.green, "curves/green"),
        (&c.blue, "curves/blue"),
        (&c.hue_vs_hue, "curves/hue_vs_hue"),
        (&c.hue_vs_saturation, "curves/hue_vs_saturation"),
        (&c.hue_vs_luminance, "curves/hue_vs_luminance"),
        (&c.luminance_vs_saturation, "curves/luminance_vs_saturation"),
        (&c.saturation_vs_saturation, "curves/saturation_vs_saturation"),
    ] {
        curve(points, t, name, cx)?;
    }
    let w = &g.wheels;
    band(&w.overall, t, "wheels/overall")?;
    band(&w.shadows, t, "wheels/shadows")?;
    band(&w.midtones, t, "wheels/midtones")?;
    band(&w.highlights, t, "wheels/highlights")?;
    check(within(w.contrast, -1.0, 8.0), t, "wheels/contrast")?;
    check(within(w.pivot, 0.0, 1.0), t, "wheels/pivot")?;
    check(within(w.input_saturation, 0.0, 2.0), t, "wheels/input_saturation")?;
    check(within(w.final_saturation, 0.0, 2.0), t, "wheels/final_saturation")?;
    check(w.low_mid_split.is_finite() && w.low_mid_split > 0.0 && w.low_mid_split < 1.0, t, "wheels/low_mid_split")?;
    check(w.mid_high_split.is_finite() && w.mid_high_split > 0.0 && w.mid_high_split < 1.0, t, "wheels/mid_high_split")?;
    if g.secondaries.len() > MAX_GRADE_SECONDARIES {
        return Err(err(Code::BudgetExceeded, format!("{t}/secondaries")));
    }
    cx.nodes(g.secondaries.len(), t)?;
    for s in &g.secondaries {
        cx.tick()?;
        key_range(&s.key.hue, true, t, "secondaries/key/hue")?;
        key_range(&s.key.saturation, false, t, "secondaries/key/saturation")?;
        key_range(&s.key.luminance, false, t, "secondaries/key/luminance")?;
        check(within(s.temperature, -300.0, 300.0), t, "secondaries/temperature")?;
        check(within(s.tint, -300.0, 300.0), t, "secondaries/tint")?;
        check(within(s.contrast, -150.0, 150.0), t, "secondaries/contrast")?;
        check(within(s.saturation, 0.0, 300.0), t, "secondaries/saturation")?;
    }
    if let Some(v) = &g.vignette {
        check(within(v.amount, -5.0, 5.0), t, "vignette/amount")?;
        check(within(v.midpoint, 0.0, 100.0), t, "vignette/midpoint")?;
        check(within(v.roundness, -100.0, 100.0), t, "vignette/roundness")?;
        check(within(v.feather, 0.0, 100.0), t, "vignette/feather")?;
    }
    Ok(())
}

fn keyed(v: &StudioKeyedValue, dim: usize, lo: f64, hi: f64, t: &str, field: &str, cx: &mut Ctx) -> Result<()> {
    let t = format!("{t}/{field}");
    let ok = |x: &[f64]| x.len() == dim && x.iter().all(|v| within(*v, lo, hi));
    validate_keyed(&t, dim, &ok, &v.static_value, &v.keyframes, cx)
}

fn effect(p: &StudioClipEffectParams, t: &str, cx: &mut Ctx) -> Result<()> {
    const BIG: f64 = MAX_STREAM_ABS_VALUE;
    match p {
        StudioClipEffectParams::ColorGrade(g) => color_grade(g, t, cx),
        StudioClipEffectParams::Transform { anchor, position, scale, rotation } => {
            keyed(anchor, 2, -BIG, BIG, t, "anchor", cx)?;
            keyed(position, 2, -BIG, BIG, t, "position", cx)?;
            keyed(scale, 2, -BIG, BIG, t, "scale", cx)?;
            keyed(rotation, 1, -BIG, BIG, t, "rotation", cx)
        }
        StudioClipEffectParams::Opacity { opacity } => keyed(opacity, 1, 0.0, 100.0, t, "opacity", cx),
        StudioClipEffectParams::Crop { left, top, right, bottom } => {
            for (v, name) in [(left, "left"), (top, "top"), (right, "right"), (bottom, "bottom")] {
                keyed(v, 1, 0.0, 100.0, t, name, cx)?;
            }
            Ok(())
        }
        StudioClipEffectParams::GaussianBlur { blurriness } => keyed(blurriness, 1, 0.0, MAX_CLIP_BLUR_PX, t, "blurriness", cx),
        StudioClipEffectParams::PigmentFilter { filter } => {
            let finite = |v: f32| v.is_finite() && v >= 0.0 && f64::from(v) <= MAX_CLIP_BLUR_PX;
            let ok = match filter {
                StudioSmartFilterKind::GaussianBlur { radius_px } | StudioSmartFilterKind::HighPass { radius_px } => finite(*radius_px),
                StudioSmartFilterKind::UnsharpMask { amount_pct, radius_px, .. } => finite(*amount_pct) && finite(*radius_px),
                StudioSmartFilterKind::Median { radius_px } => f64::from(*radius_px) <= MAX_CLIP_BLUR_PX,
                StudioSmartFilterKind::AddNoise { amount_pct, .. } => finite(*amount_pct),
                StudioSmartFilterKind::Liquify { .. } => false,
            };
            check(ok, t, "filter")
        }
    }
}

/// Parses wire JSON (bounded by `Budget::input_bytes` and depth before any parse; duplicate keys
/// refused) and validates it against the document.
pub fn parse_clip_finishing_records(
    input: &[u8],
    d: &StudioDocument,
    b: Budget,
    token: &CancellationToken,
) -> Result<StudioClipFinishingRecords> {
    super::preflight(input, b, token)?;
    let value = serde_json::from_slice::<super::UniqueValue>(input)
        .map_err(|e| {
            err(
                if e.to_string().contains("duplicate_field") { Code::DuplicateField } else { Code::InvalidJson },
                "clip_finishing",
            )
        })?
        .0;
    let r: StudioClipFinishingRecords =
        serde_json::from_value(value).map_err(|_| err(Code::InvalidJson, "clip_finishing"))?;
    validate_clip_finishing_records(&r, d, b, token)?;
    Ok(r)
}

/// Validates the record set against the document (module docs).
pub fn validate_clip_finishing_records(
    r: &StudioClipFinishingRecords,
    d: &StudioDocument,
    b: Budget,
    token: &CancellationToken,
) -> Result<()> {
    let mut cx = Ctx::new(b, token, &super::Unresolved);
    cx.tick()?;
    id(&r.document_id, "SDOC", "document_id")?;
    if r.document_id != d.document_id {
        return Err(err(Code::InvalidField, "document_id"));
    }
    if r.effect_stacks.len() > MAX_CLIP_EFFECT_STACKS {
        return Err(err(Code::BudgetExceeded, "effect_stacks"));
    }
    cx.nodes(r.effect_stacks.len(), "effect_stacks")?;
    let clips: BTreeSet<&str> = d
        .sequences
        .iter()
        .flat_map(|s| s.tracks.iter())
        .flat_map(|t| t.clips.iter())
        .map(|c| c.clip_id.as_str())
        .collect();
    let mut stacks = BTreeSet::new();
    let mut effect_ids = BTreeSet::new();
    for s in &r.effect_stacks {
        cx.tick()?;
        let t = s.clip_id.as_str();
        id(t, "SCLP", "effect_stacks/clip_id")?;
        if !clips.contains(t) {
            return Err(err(Code::DanglingReference, t));
        }
        if !stacks.insert(t) {
            return Err(err(Code::DuplicateIdentity, t));
        }
        if s.effects.len() > MAX_CLIP_EFFECTS {
            return Err(err(Code::BudgetExceeded, format!("{t}/effects")));
        }
        cx.nodes(s.effects.len(), t)?;
        for e in &s.effects {
            cx.tick()?;
            check(key(&e.effect_id), t, "effects/effect_id")?;
            if !effect_ids.insert(e.effect_id.as_str()) {
                return Err(err(Code::DuplicateIdentity, &e.effect_id));
            }
            let et = format!("{t}/effects/{}", e.effect_id);
            effect(&e.params, &et, &mut cx)?;
        }
    }
    if r.templates.len() > MAX_TEMPLATES || r.graphics.len() > MAX_CLIP_EFFECT_STACKS {
        return Err(err(Code::BudgetExceeded, "templates"));
    }
    cx.nodes(r.templates.len(), "templates")?;
    cx.nodes(r.graphics.len(), "graphics")?;
    let mut template_ids = BTreeSet::new();
    for tpl in &r.templates {
        template(tpl, &mut cx)?;
        if !template_ids.insert(tpl.template_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, &tpl.template_id));
        }
    }
    let mut graphic_clips = BTreeSet::new();
    for g in &r.graphics {
        graphic(g, d, &r.templates, &mut cx)?;
        if !graphic_clips.insert(g.clip_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, &g.clip_id));
        }
    }
    if r.caption_tracks.len() > MAX_CAPTION_TRACKS {
        return Err(err(Code::BudgetExceeded, "caption_tracks"));
    }
    cx.nodes(r.caption_tracks.len(), "caption_tracks")?;
    let mut caption_ids = BTreeSet::new();
    let mut active_in = BTreeSet::new();
    for c in &r.caption_tracks {
        let active = caption_track(c, d, &mut cx)?;
        if !caption_ids.insert(c.track_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, &c.track_id));
        }
        // STU-VID-037: "one is active at a time for display" (per sequence).
        if let Some(seq) = active {
            check(active_in.insert(seq), &c.track_id, "active")?;
        }
    }
    Ok(())
}
