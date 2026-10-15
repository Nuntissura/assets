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
    /// space -> display-referred linear `[0, 1]`), alpha untouched. Serialised as the view's key.
    View(#[serde(serialize_with = "serialize_view")] hsk_studio_prism::ViewTransformId),
}
fn serialize_view<S: serde::Serializer>(
    view: &hsk_studio_prism::ViewTransformId,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(view.key())
}

/// The output stage of a plan: sample range plus optional display transform.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize)]
pub struct OutputStage {
    pub sample_range: SampleRange,
    pub display: Option<DisplayTransform>,
}
impl OutputStage {
    pub const fn new(sample_range: SampleRange, display: Option<DisplayTransform>) -> Self {
        Self {
            sample_range,
            display,
        }
    }
    /// Unit range and no display transform (absent from the digest).
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
    pub fn is_extended(&self) -> bool {
        self.sample_range == SampleRange::Extended
    }
}

/// Operator key of an extended-range plan (`ExtOperator::ExtendedRange`).
pub const EXTENDED_RANGE_OPERATOR: &str = "sample_range_extended";
/// Operator key of a layer whose mode is undefined above 1 in an extended-range plan.
pub const EXTENDED_RANGE_MODE_OPERATOR: &str = "sample_range_extended_mode";
/// Operator key of an envelope layer entry (adjustment, Blend-If, effects, feather, transform,
/// blur, filters, content-aware fill) in an extended-range plan: their maths are unit-domain.
pub const EXTENDED_RANGE_COMBINATION_OPERATOR: &str = "sample_range_extended_combination";
/// Operator key of the stage-6a clip (`ExtOperator::DisplayClip`).
pub const DISPLAY_CLIP_OPERATOR: &str = "display_clip";
/// Operator key of a stage-6a view transform (`ExtOperator::DisplayView`).
pub const DISPLAY_VIEW_OPERATOR: &str = "display_view_transform";
