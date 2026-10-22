//! The plan's output stage (lane SMB-PIGMENT-EXTENDED-RANGE): the sample range the compositor
//! runs in and the display transform after it. It travels in the additive envelope
//! ([`super::PlanExtensions::output`], part of [`super::PlanExtensions::digest_with`]) because
//! `OutputTarget` is a v1 type built by struct literal in composite, render-cpu, render-gpu,
//! harness and interop crates; the default stage (unit range, no display transform) is absent from
//! the digest, so every existing plan and envelope keeps its digest and its bytes.
//!
//! Spec basis: STU-COL-120 (32-bit is "floating point, linear-light, unbounded"; a reduced tool
//! set "MUST be reported per operation"), STU-COL-161 (stage 4 "Blending and compositing" in the
//! blending space, then stage 6a "Display transform: Working space -> display").
use serde::Serialize;

/// Range of the colour samples the compositor admits and produces.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize)]
pub enum SampleRange {
    /// `[0, 1]`: tiles and solids outside it are refused (`sample_range_outside_unit`) and every
    /// composite clamps to it. The default; unchanged.
    #[default]
    Unit,
    /// Any finite colour channel (alpha stays `[0, 1]`); blend modes run through
    /// `blend_math::composite_over_extended` without the unit clamp; modes that are not
    /// `blend_math::extended_defined` are refused at admission.
    Extended,
}

/// Stage-6a display transform applied to the composite before it reaches the sink.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum DisplayTransform {
    /// Clamp every output colour channel into `[0, 1]` (alpha untouched).
    Clip,
    /// A Prism view transform (lane SMB-PRISM-VIEW-TRANSFORM, STU-COL-313): every composite
    /// colour goes through `hsk_studio_prism::ViewTransformId::apply` (scene-referred working
    /// space, linear sRGB -> display-referred linear: `[0, 1]` for the SDR views, units of
    /// 100 cd/m2 for the HDR views), alpha untouched. Serialised as the view's key.
    View(#[serde(serialize_with = "serialize_view")] hsk_studio_prism::ViewTransformId),
    /// A Prism view transform from an explicit scene input (lane SMB-PRISM-VIEW-TRANSFORM-2,
    /// STU-COL-311/312: ACEScg or ACES2065-1 composite data, or linear sRGB stated explicitly).
    /// Serialised as `"<view key>@<input key>"`.
    ViewFrom(#[serde(serialize_with = "serialize_view_from")] hsk_studio_prism::ViewTransform),
}
impl DisplayTransform {
    /// The view of a `View` / `ViewFrom` stage (`View(id)` reads linear sRGB); `None` for `Clip`.
    pub fn view(&self) -> Option<hsk_studio_prism::ViewTransform> {
        match self {
            Self::Clip => None,
            Self::View(id) => Some(hsk_studio_prism::ViewTransform::new(
                *id,
                hsk_studio_prism::SceneInput::LinearSrgb,
            )),
            Self::ViewFrom(view) => Some(*view),
        }
    }
}
fn serialize_view_from<S: serde::Serializer>(
    view: &hsk_studio_prism::ViewTransform,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_str(&format_args!("{}@{}", view.view.key(), view.input.key()))
}
fn serialize_view<S: serde::Serializer>(
    view: &hsk_studio_prism::ViewTransformId,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(view.key())
}

/// The sample depth the compositor stores between layers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize)]
pub enum WorkingDepth {
    /// binary32 throughout (the default; unchanged, absent from the digest).
    #[default]
    Float,
    /// Photoshop's 8-bit document (lane SMB-PS-ORACLE-PARITY-2, measured on the Photoshop 27.11 D5
    /// oracle): the running result of every group is stored in 8-bit levels after each layer (except
    /// the root's last layer: the delivered result keeps binary32, the sink owns the output depth)
    /// ([`quantise_8bit`] on each straight channel and on alpha; PS-C28 `nomask` and PS-C29 `fill100`:
    /// the Multiply result is stored before the Levels layer reads it, within one code where the float
    /// composite differed by 3), and Levels runs on whole levels
    /// ([`crate::adjust_math::LevelsRecord::map_value_8bit`]; PS-C02 8-bit gamma 1.5 variants).
    /// Key [`WORKING_DEPTH_EIGHT_OPERATOR`], class Approximate, declared by each backend
    /// (`ExtOperator::WorkingDepthEight`).
    Eight,
}
impl WorkingDepth {
    pub fn is_float(&self) -> bool {
        *self == Self::Float
    }
}
/// Operator key of an 8-bit working depth (`ExtOperator::WorkingDepthEight`).
pub const WORKING_DEPTH_EIGHT_OPERATOR: &str = "working_depth_8bit";

/// One sample stored in 8-bit levels: `floor(255 v + 0.5) / 255` (half up, every backend the same
/// way; [`WorkingDepth::Eight`]).
pub fn quantise_8bit(v: f32) -> f32 {
    (v * 255.0 + 0.5).floor() / 255.0
}

/// The output stage of a plan: sample range plus optional display transform, and the working depth
/// (lane SMB-PS-ORACLE-PARITY-2; [`WorkingDepth::Float`] is not serialised, so every existing
/// digest is unchanged).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize)]
pub struct OutputStage {
    pub sample_range: SampleRange,
    pub display: Option<DisplayTransform>,
    #[serde(skip_serializing_if = "WorkingDepth::is_float")]
    pub working_depth: WorkingDepth,
}
impl OutputStage {
    pub const fn new(sample_range: SampleRange, display: Option<DisplayTransform>) -> Self {
        Self {
            sample_range,
            display,
            working_depth: WorkingDepth::Float,
        }
    }
    /// This stage with the working depth `depth`.
    pub const fn with_working_depth(mut self, depth: WorkingDepth) -> Self {
        self.working_depth = depth;
        self
    }
    /// The 8-bit working depth ([`WorkingDepth::Eight`]).
    pub fn is_eight_bit(&self) -> bool {
        self.working_depth == WorkingDepth::Eight
    }
    /// Unit range, no display transform and the float working depth (absent from the digest).
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
    pub fn is_extended(&self) -> bool {
        self.sample_range == SampleRange::Extended
    }
}

/// Operator key of an extended-range plan (`ExtOperator::ExtendedRange`).
pub const EXTENDED_RANGE_OPERATOR: &str = "sample_range_extended";
/// Prefix of the per-mode refusal keys (`blend_math::extended_mode_key`): a mode undefined above 1
/// is refused as `sample_range_extended_mode_<mode key>` (lane SMB-PIGMENT-EXTENDED-RANGE-2); this
/// bare key itself is no longer reported.
pub const EXTENDED_RANGE_MODE_OPERATOR: &str = "sample_range_extended_mode";
/// Retired key of `ExtOperator::ExtendedRangeCombination` (envelope entries are now reported per
/// operation as [`ExtendedOp`]; never reported).
pub const EXTENDED_RANGE_COMBINATION_OPERATOR: &str = "sample_range_extended_combination";
/// Operator key of the stage-6a clip (`ExtOperator::DisplayClip`).
pub const DISPLAY_CLIP_OPERATOR: &str = "display_clip";
/// Operator key of a stage-6a view transform (`ExtOperator::DisplayView`).
pub const DISPLAY_VIEW_OPERATOR: &str = "display_view_transform";
/// The clauses every extended-range refusal cites ([`ExtendedOp::clause`]): STU-COL-120 (32-bit is
/// unbounded; reduced availability MUST be reported per operation) and STU-FX-006a (an effect or
/// filter undefined outside the unit range declares its supported depths, never degrades
/// silently). Blend modes also cite STU-RAS-155 (b).
pub const EXTENDED_RANGE_CLAUSE: &str = "STU-COL-120; STU-FX-006a";

/// One envelope operation of a layer entry in an extended-range plan (lane
/// SMB-PIGMENT-EXTENDED-RANGE-2), reported as `ExtOperator::ExtendedEnvelope`. DEFINED operations
/// ([`ExtendedOp::defined`]) have no reference to the unit white point and class Approximate (each
/// backend declares them); the others are intrinsically Unsupported and refused for every backend
/// under their own key, citing [`EXTENDED_RANGE_CLAUSE`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExtendedOp {
    /// An adjustment layer of this kind (exposure on linear-light values is defined).
    Adjustment(crate::adjust_math::AdjustmentKind),
    /// Exposure with `linearize_gamma != 1` (encoded values): undefined.
    ExposureEncoded,
    /// Non-identity Blend-If: its ranges are levels of the unit scale; undefined.
    BlendIf,
    /// Layer effects (mattes are alpha maths; their modes are checked as blend modes): defined.
    Effects,
    /// A layer filter of this kind (gaussian blur, unsharp mask, median defined).
    Filter(crate::filter_math::FilterKind),
    /// Content-aware fill without colour adaptation: defined.
    ContentAwareFill,
    /// Content-aware fill with colour adaptation (unit-scale gain / bias bounds, heal): undefined.
    ContentAwareColourAdaptation,
    /// Layer or backdrop blur: defined (convex).
    Blur,
    /// Layer transform: defined (unclamped resampling).
    Transform,
}
impl ExtendedOp {
    pub const fn defined(self) -> bool {
        use crate::adjust_math::AdjustmentKind;
        match self {
            Self::Adjustment(k) => matches!(k, AdjustmentKind::Exposure),
            Self::Filter(k) => crate::filter_math::filter_extended_defined(k),
            Self::Effects | Self::ContentAwareFill | Self::Blur | Self::Transform => true,
            Self::ExposureEncoded | Self::BlendIf | Self::ContentAwareColourAdaptation => false,
        }
    }
    /// The spec clauses a refusal of this operation cites.
    pub const fn clause(self) -> &'static str {
        EXTENDED_RANGE_CLAUSE
    }
    pub const fn key(self) -> &'static str {
        use crate::adjust_math::AdjustmentKind as A;
        use crate::filter_math::FilterKind as F;
        match self {
            Self::Adjustment(A::Levels) => "sample_range_extended_levels",
            Self::Adjustment(A::Curves) => "sample_range_extended_curves",
            Self::Adjustment(A::HueSaturation) => "sample_range_extended_hue_saturation",
            Self::Adjustment(A::BrightnessContrast) => "sample_range_extended_brightness_contrast",
            Self::Adjustment(A::Exposure) => "sample_range_extended_exposure",
            Self::Adjustment(A::Invert) => "sample_range_extended_invert",
            Self::Adjustment(A::Threshold) => "sample_range_extended_threshold",
            Self::Adjustment(A::Posterize) => "sample_range_extended_posterize",
            Self::Adjustment(A::Vibrance) => "sample_range_extended_vibrance",
            Self::Adjustment(A::ColourBalance) => "sample_range_extended_colour_balance",
            Self::Adjustment(A::PhotoFilter) => "sample_range_extended_photo_filter",
            Self::Adjustment(A::BlackAndWhite) => "sample_range_extended_black_and_white",
            Self::Adjustment(A::ChannelMixer) => "sample_range_extended_channel_mixer",
            Self::Adjustment(A::SelectiveColour) => "sample_range_extended_selective_colour",
            Self::Adjustment(A::GradientMap) => "sample_range_extended_gradient_map",
            Self::Adjustment(A::ColourLookup) => "sample_range_extended_colour_lookup",
            Self::Adjustment(A::ShadowsHighlights) => "sample_range_extended_shadows_highlights",
            Self::Adjustment(A::ToneCompression) => "sample_range_extended_tone_compression",
            Self::Adjustment(A::MatchColour) => "sample_range_extended_match_colour",
            Self::Adjustment(A::ReplaceColour) => "sample_range_extended_replace_colour",
            Self::ExposureEncoded => "sample_range_extended_exposure_encoded",
            Self::BlendIf => "sample_range_extended_blend_if",
            Self::Effects => "sample_range_extended_effects",
            Self::Filter(F::GaussianBlur) => "sample_range_extended_filter_gaussian_blur",
            Self::Filter(F::UnsharpMask) => "sample_range_extended_filter_unsharp_mask",
            Self::Filter(F::HighPass) => "sample_range_extended_filter_high_pass",
            Self::Filter(F::Median) => "sample_range_extended_filter_median",
            Self::Filter(F::AddNoise) => "sample_range_extended_filter_add_noise",
            Self::ContentAwareFill => "sample_range_extended_content_aware_fill",
            Self::ContentAwareColourAdaptation => {
                "sample_range_extended_content_aware_colour_adaptation"
            }
            Self::Blur => "sample_range_extended_blur",
            Self::Transform => "sample_range_extended_transform",
        }
    }
}
