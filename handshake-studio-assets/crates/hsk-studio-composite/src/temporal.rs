//! Time-based compositing, slice 1 (STU-CMP-002 owner: composite consumes keyframe evaluation and
//! never owns it; brief `lane-smb-composite-temporal`). Frame `tick` of a layer stack is the
//! EXISTING static lowering of a disposable frame document plus a per-layer time state:
//!
//! 1. Layer windows: a layer with a [`LayerTiming`] is shown only for `in_tick <= tick < out_tick`.
//! 2. The caller-owned [`TemporalSource`] port returns a [`LayerFrameState`] per layer and tick
//!    (`None` = not animated). `visible == false` hides the layer for this frame (a port never
//!    shows an authored-hidden layer); `opacity_scale` multiplies the authored opacity (STU-CMP-010
//!    step 6).
//! 3. The frame document is re-validated by Folio (`validate_document`) and lowered by the
//!    unchanged `lower` -> `evaluate` -> `lower_to_render_plan`. When no layer changes, the
//!    caller's snapshot is lowered as is, so the frame plan is the static plan.
//! 4. Translation is applied AFTER lowering (STU-CMP-010 step 5 comes after effects): every
//!    `TileInput` of a translated raster layer moves by the translation, and a layer mask moves with
//!    it only when Folio marks it `linked`. The plan is validated again by pigment.
//!
//! 5. Resampling transforms (lane SMB-PIGMENT-TRANSFORM): a fractional translation, a scale other
//!    than 1 or a rotation other than 0 lowers to pigment's layer-transform operator
//!    (`LayerExtension::transform`, carried in [`FrameLowering::extensions`]; execute the frame with
//!    `RenderBackend::execute_extended`) when the source DECLARES the layer's resample filter
//!    ([`TemporalSource::resample_filter`], STU-MOT-007 / STU-RAS-141: never a silent default). The
//!    affine is After Effects' layer model `x' = anchor + translate + R(rotation) S(scale)
//!    (x - anchor)` in plan pixels, `anchor` from [`TemporalSource::layer_anchor`] or, when the
//!    source gives none, the centre of the layer's tile; a linked mask is transformed with its
//!    content (`LayerTransform::transform_mask`), an unlinked one stays. A scale of 0 on an axis hides
//!    the layer for the frame (nothing is rendered). The integer path above is unchanged and stays
//!    byte-identical (no envelope entry).
//!
//! 6. Optional inputs ([`lower_frame_with_inputs`], lane SMB-COMPOSITE-DOCGRAPH-MASKS): the frame
//!    document is evaluated and lowered by the static path WITH the caller's [`RenderInputs`]
//!    (`evaluate_with_inputs` / `lower_with_inputs`: raster records, prepared text, prepared
//!    vectors), so vector clips and vector masks (VECTOR, ALPHA, LUMINANCE, invert, region,
//!    density, feather, reveal_outside) cut their layers on this path as on the static and scene
//!    paths, and vector/text layers draw. A vector clip plane moves with its layer; a vector mask
//!    plane moves with it unless the mask is unlinked (STU-RAS-016), in which case it stays in plan
//!    space and is re-rasterised over the plane rect united with the target extent, so it covers
//!    the moved content (backends refuse a mask that does not cover its layer). The envelope of
//!    the inputs (mask feather, effects, blur) is carried in [`FrameLowering::extensions`] with
//!    the transform entries merged into it; the key tables are [`FrameLowering::tables`].
//!
//! Refusals, typed and never rounded: a resampling transform whose source declares no filter
//! ([`TRANSFORM_REQUIRES_RESAMPLING`], the behaviour before the operator existed); a raster layer
//! lowered to more than one tile ([`TRANSFORM_MULTI_TILE_LAYER`]: resampling tiles one by one
//! would seam at tile edges); a non-zero translation or a transform of a non-raster layer (a group)
//! ([`TRANSLATE_NON_RASTER_LAYER`]: the tiles of its members are not addressed by its id); a matrix
//! pigment refuses (`CompositeError::Plan(PlanError::TransformMatrix)`); a moved layer with a
//! vector clip AND an unlinked vector mask ([`TRANSFORM_UNLINKED_MASK_WITH_CLIP`]: one plane slot
//! cannot both move and stay). Frame documents and plans
//! are disposable and never stored. The motion adapter ([`motion::MotionTemporalSource`], feature `motion`) is the
//! only motion-aware code in composite; the default build keeps the static closure.
#![deny(clippy::arithmetic_side_effects)]

use crate::{
    CompositeError, Limits, LoweringNote, NodeAddr, RenderInputs, RenderLowering, TileInfoSource,
    evaluate, evaluate_with_inputs, lower, lower_to_render_plan, lower_with_inputs, outcome_of,
};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{Budget, Payload, Resolver, Snapshot, validate_document};
use hsk_studio_observe::{FailureCode, Outcome};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::BlendSpace;
use hsk_studio_pigment::render_plan::{
    ExtOperator, LayerExtension, OutputTarget, PlanExtensions, PlanLimits, PlanNode, PlanNodeId,
    SourceKey,
};
use hsk_studio_pigment::transform_math::{Affine2, LayerTransform, ResampleFilter};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[cfg(feature = "motion")]
pub mod composition;
#[cfg(feature = "motion")]
pub mod motion;

/// Refusal key: the frame needs pixel resampling, which pigment's plan IR cannot express.
pub const TRANSFORM_REQUIRES_RESAMPLING: &str = "transform_requires_resampling";
/// Refusal key: a non-zero translation on a layer whose own payload is not raster tiles.
pub const TRANSLATE_NON_RASTER_LAYER: &str = "translate_non_raster_layer";
/// Refusal key: a resampling transform of a raster layer lowered to more than one tile.
pub const TRANSFORM_MULTI_TILE_LAYER: &str = "transform_multi_tile_layer";
/// Refusal key: a moved layer with a vector clip (moves) and an unlinked vector mask (stays), which
/// the lowering combines into one plane.
pub const TRANSFORM_UNLINKED_MASK_WITH_CLIP: &str = "transform_unlinked_vector_mask_with_clip";

/// One layer's state at one tick. [`LayerFrameState::IDENTITY`] changes nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerFrameState {
    /// `false` hides the layer for this frame; `true` keeps the authored visibility.
    pub visible: bool,
    /// Multiplies the authored opacity, `0.0..=1.0`.
    pub opacity_scale: f64,
    /// Offset from the authored placement in plan pixels (x right, y down). Integers take the
    /// exact tile-move path; a fractional value resamples (module docs, item 5).
    pub translate_px: [f64; 2],
    /// Scale factor per axis about the anchor; other than `[1, 1]` resamples, `0` hides.
    pub scale: [f64; 2],
    /// Rotation in degrees about the anchor, clockwise on screen (y down); other than 0 resamples.
    pub rotation_deg: f64,
}

impl LayerFrameState {
    pub const IDENTITY: Self = Self {
        visible: true,
        opacity_scale: 1.0,
        translate_px: [0.0, 0.0],
        scale: [1.0, 1.0],
        rotation_deg: 0.0,
    };
}

impl Default for LayerFrameState {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// Caller-owned per-layer time state (keyframes, expressions, scripted animation).
pub trait TemporalSource {
    /// `None` = the layer is not animated (its static state applies).
    fn layer_state(
        &self,
        layer_id: &str,
        tick: i64,
        cancel: &CancellationToken,
    ) -> Result<Option<LayerFrameState>, TemporalError>;

    /// The layer's declared resample filter (STU-MOT-007 `resample_filter`, STU-RAS-141 method).
    /// `None` (the default) declares none, and a frame that would resample the layer is refused as
    /// [`TRANSFORM_REQUIRES_RESAMPLING`]: the filter is never chosen silently.
    fn resample_filter(&self, layer_id: &str) -> Option<ResampleFilter> {
        let _ = layer_id;
        None
    }

    /// Anchor point of the layer's scale and rotation at `tick`, in plan pixels of the authored
    /// placement. `None` (the default) = the centre of the layer's tile.
    fn layer_anchor(
        &self,
        layer_id: &str,
        tick: i64,
        cancel: &CancellationToken,
    ) -> Result<Option<[f64; 2]>, TemporalError> {
        let _ = (layer_id, tick, cancel);
        Ok(None)
    }

    /// The layer's whole matrix at `tick`: content plan coordinates (the authored tile placement)
    /// -> plan pixels. `Some` expresses any affine (a parent chain, a sheared product of parent
    /// scale and child rotation) and REPLACES the state's `translate_px`, `scale` and
    /// `rotation_deg` and [`TemporalSource::layer_anchor`] (only `visible` and `opacity_scale` are
    /// read); an exact integer translation still takes the tile-move path. `None` (the default)
    /// = the state's fields.
    fn layer_matrix(
        &self,
        layer_id: &str,
        tick: i64,
        cancel: &CancellationToken,
    ) -> Result<Option<Affine2>, TemporalError> {
        let _ = (layer_id, tick, cancel);
        Ok(None)
    }
}

/// No animation: every frame inside the windows is the static plan.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoTemporalSource;

impl TemporalSource for NoTemporalSource {
    fn layer_state(
        &self,
        _: &str,
        _: i64,
        _: &CancellationToken,
    ) -> Result<Option<LayerFrameState>, TemporalError> {
        Ok(None)
    }
}

/// Layer in/out window in accord ticks, half-open `[in_tick, out_tick)` (STU-MOT-020 time base).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerTiming {
    pub layer_id: String,
    pub in_tick: i64,
    pub out_tick: i64,
}

impl LayerTiming {
    pub fn contains(&self, tick: i64) -> bool {
        self.in_tick <= tick && tick < self.out_tick
    }
}

/// Temporal failures. Every variant is typed; nothing is rounded, clamped or dropped silently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemporalError {
    /// The static lowering (or a temporal bound check: `Overflow`, `BudgetExceeded`,
    /// `DanglingInput`, `Canceled`, `StaleRevision`, pigment `Plan`) refused.
    Composite(CompositeError),
    /// A feature the plan IR cannot express (`transform_requires_resampling`,
    /// `translate_non_raster_layer`).
    Unsupported {
        layer_id: String,
        feature: &'static str,
    },
    /// A non-finite or out-of-range state field (`opacity_scale`, `translate_px`, `scale`,
    /// `rotation_deg`).
    InvalidState {
        layer_id: String,
        field: &'static str,
    },
    /// `in_tick >= out_tick`.
    InvalidTiming { layer_id: String },
    /// Two timings for one layer.
    DuplicateTiming { layer_id: String },
    /// Folio refused the frame document (`Code:target`).
    Document(String),
    /// The temporal source failed (its own stable code).
    Source { layer_id: String, code: String },
    /// A render backend refused a composition plan (the backend's stable code).
    Render {
        composition_id: String,
        code: &'static str,
    },
}

impl TemporalError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Composite(e) => e.code(),
            Self::Unsupported { .. } => "temporal_unsupported",
            Self::InvalidState { .. } => "temporal_invalid_state",
            Self::InvalidTiming { .. } => "temporal_invalid_timing",
            Self::DuplicateTiming { .. } => "temporal_duplicate_timing",
            Self::Document(_) => "temporal_frame_document",
            Self::Source { .. } => "temporal_source",
            Self::Render { .. } => "temporal_render",
        }
    }
}

impl From<CompositeError> for TemporalError {
    fn from(e: CompositeError) -> Self {
        Self::Composite(e)
    }
}

impl fmt::Display for TemporalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Composite(e) => e.fmt(f),
            Self::Unsupported { layer_id, feature } => {
                write!(f, "{}:{layer_id}:{feature}", self.code())
            }
            Self::InvalidState { layer_id, field } => {
                write!(f, "{}:{layer_id}:{field}", self.code())
            }
            Self::InvalidTiming { layer_id } | Self::DuplicateTiming { layer_id } => {
                write!(f, "{}:{layer_id}", self.code())
            }
            Self::Document(t) => write!(f, "{}:{t}", self.code()),
            Self::Source { layer_id, code } => write!(f, "{}:{layer_id}:{code}", self.code()),
            Self::Render {
                composition_id,
                code,
            } => write!(f, "{}:{composition_id}:{code}", self.code()),
        }
    }
}

impl std::error::Error for TemporalError {}

/// Observe outcome of a frame lowering; a resampling refusal is `Unsupported`.
pub fn frame_outcome(result: &Result<FrameLowering, TemporalError>) -> Outcome {
    match result {
        Ok(_) => Outcome::Success,
        Err(TemporalError::Composite(e)) => outcome_of::<()>(&Err(e.clone())),
        Err(TemporalError::Unsupported { .. }) => Outcome::Failure(FailureCode::Unsupported),
        Err(_) => Outcome::Failure(FailureCode::Validation),
    }
}

/// Everything [`lower_frame`] needs besides the source and the tick. `budget` and `resolver` are
/// the ones the snapshot was opened with (the frame document is re-validated by Folio).
#[derive(Clone, Copy)]
pub struct FrameRequest<'a> {
    pub snapshot: &'a Snapshot,
    pub expected_revision: u64,
    pub blend_space: BlendSpace,
    pub limits: Limits,
    pub budget: Budget,
    pub resolver: &'a dyn Resolver,
    pub root: &'a NodeAddr,
    pub target: OutputTarget,
    pub info: &'a dyn TileInfoSource,
    /// At most `limits.max_steps` entries; a layer without an entry is shown at every tick.
    pub timings: &'a [LayerTiming],
}

/// One frame: the validated render lowering plus what time changed.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameLowering {
    pub lowering: RenderLowering,
    pub tick: i64,
    /// Animated layers shown in this frame and the state applied to them, in document order.
    pub applied: Vec<(String, LayerFrameState)>,
    /// Layers hidden for this frame by their window or by the source, in document order.
    pub hidden: Vec<String>,
    /// Envelope of the frame: one `LayerExtension::transform` per resampled layer (empty when no
    /// layer resamples, the plain plan), merged into the envelope of the inputs
    /// ([`lower_frame_with_inputs`]). Validated by pigment against `lowering.plan`.
    pub extensions: PlanExtensions,
    /// Key tables of the inputs (empty for [`lower_frame`]); planes already follow their layer's
    /// integer move. Serve them with [`FrameLowering::vector_mask`] / [`FrameLowering::vector_paint`].
    pub tables: FrameTables,
}

/// The key tables of [`lower_frame_with_inputs`] beyond raster tiles and masks (as
/// `crate::InputsLowering`): text and vector coverage planes by layer id, image paint tiles by
/// source id, vector rows, owned clip / vector-mask planes, and the lowering notes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameTables {
    pub text_masks: crate::text::TextMaskTable,
    pub vector_masks: Vec<(SourceKey, String)>,
    pub image_sources: Vec<(SourceKey, String)>,
    pub vector_rows: Vec<(SourceKey, String, usize)>,
    pub clip_planes: Vec<(SourceKey, crate::vector::ClipPlane)>,
    pub notes: Vec<LoweringNote>,
}

impl FrameLowering {
    /// The coverage plane behind `key` when it is a vector, stroke, clip or vector-mask plane.
    pub fn vector_mask<'p>(
        &'p self,
        key: SourceKey,
        vectors: &'p crate::vector::PreparedVectors,
    ) -> Option<hsk_studio_pigment::Mask<'p>> {
        let t = &self.tables;
        crate::lower_render_plan::vector_mask_of(
            &self.lowering.plan,
            &t.vector_masks,
            &t.clip_planes,
            &t.vector_rows,
            key,
            vectors,
        )
    }
    /// The painted vector row tile behind `key`, in the plan's colour tag.
    pub fn vector_paint<'p>(
        &self,
        key: SourceKey,
        vectors: &'p crate::vector::PreparedVectors,
    ) -> Option<hsk_studio_pigment::render_plan::TilePlanes<'p>> {
        crate::lower_render_plan::vector_paint_of(
            &self.tables.vector_rows,
            key,
            vectors,
            self.lowering.plan.target.space,
        )
    }
}

/// How the vector cut (clip or vector mask plane) of a layer follows the layer's movement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VectorLink {
    /// A clip, a linked mask, or a mask whose refusal the lowering reports: moves with the layer.
    Linked,
    /// An unlinked vector mask without a clip: stays in plan space.
    Unlinked,
    /// A clip together with an unlinked mask (one combined plane).
    Split,
}

fn vector_link(vectors: Option<&crate::vector::PreparedVectors>, layer_id: &str) -> Option<VectorLink> {
    let v = vectors?;
    let clip = v.clip(layer_id).is_some();
    match v.mask(layer_id) {
        Some(Ok(m)) if !m.linked && clip => Some(VectorLink::Split),
        Some(Ok(m)) if !m.linked => Some(VectorLink::Unlinked),
        Some(_) => Some(VectorLink::Linked),
        None => clip.then_some(VectorLink::Linked),
    }
}

/// Largest magnitude an `f64` translation may have and still convert to `i64` exactly.
const I64_BOUND: f64 = 9_223_372_036_854_775_808.0; // 2^63

fn integer_px(layer_id: &str, v: f64) -> Result<i64, TemporalError> {
    if !v.is_finite() {
        return Err(TemporalError::InvalidState {
            layer_id: layer_id.to_owned(),
            field: "translate_px",
        });
    }
    if v.fract() != 0.0 {
        return Err(TemporalError::Unsupported {
            layer_id: layer_id.to_owned(),
            feature: TRANSFORM_REQUIRES_RESAMPLING,
        });
    }
    if !(-I64_BOUND..I64_BOUND).contains(&v) {
        return Err(CompositeError::Overflow.into());
    }
    // Exact: `v` is an integer inside the i64 range.
    #[allow(clippy::cast_possible_truncation)]
    Ok(v as i64)
}

/// How one admitted state moves its layer.
enum Movement {
    /// Exact integer tile move (the original path; `(0, 0)` = none).
    Integer(i64, i64),
    /// Resampled through pigment's layer transform with the declared filter.
    Resample(ResampleFilter),
    /// A scale of 0 on an axis: nothing is rendered this frame.
    Collapsed,
}

/// Checks one state and classifies its movement; `filter` is the source's declared filter.
fn admit_state(
    layer_id: &str,
    s: &LayerFrameState,
    filter: Option<ResampleFilter>,
) -> Result<Movement, TemporalError> {
    let invalid = |field| TemporalError::InvalidState {
        layer_id: layer_id.to_owned(),
        field,
    };
    if !s.opacity_scale.is_finite() || !(0.0..=1.0).contains(&s.opacity_scale) {
        return Err(invalid("opacity_scale"));
    }
    if !s.scale.iter().all(|v| v.is_finite()) {
        return Err(invalid("scale"));
    }
    if !s.rotation_deg.is_finite() {
        return Err(invalid("rotation_deg"));
    }
    let integral = s.translate_px.iter().all(|v| v.fract() == 0.0);
    if s.scale == [1.0, 1.0] && s.rotation_deg == 0.0 && integral {
        return Ok(Movement::Integer(
            integer_px(layer_id, s.translate_px[0])?,
            integer_px(layer_id, s.translate_px[1])?,
        ));
    }
    if !s.translate_px.iter().all(|v| v.is_finite()) {
        return Err(invalid("translate_px"));
    }
    let Some(filter) = filter else {
        return Err(TemporalError::Unsupported {
            layer_id: layer_id.to_owned(),
            feature: TRANSFORM_REQUIRES_RESAMPLING,
        });
    };
    if s.scale.contains(&0.0) {
        return Ok(Movement::Collapsed);
    }
    Ok(Movement::Resample(filter))
}

/// Classifies a source-supplied matrix like [`admit_state`] classifies the state's fields.
fn admit_matrix(
    layer_id: &str,
    s: &LayerFrameState,
    m: &Affine2,
    filter: Option<ResampleFilter>,
) -> Result<Movement, TemporalError> {
    let invalid = |field| TemporalError::InvalidState {
        layer_id: layer_id.to_owned(),
        field,
    };
    if !s.opacity_scale.is_finite() || !(0.0..=1.0).contains(&s.opacity_scale) {
        return Err(invalid("opacity_scale"));
    }
    if !m.m.iter().all(|v| v.is_finite()) {
        return Err(invalid("matrix"));
    }
    let [a, b, c, d, e, f] = m.m;
    if a == 1.0 && b == 0.0 && d == 0.0 && e == 1.0 && c.fract() == 0.0 && f.fract() == 0.0 {
        return Ok(Movement::Integer(integer_px(layer_id, c)?, integer_px(layer_id, f)?));
    }
    let Some(filter) = filter else {
        return Err(TemporalError::Unsupported {
            layer_id: layer_id.to_owned(),
            feature: TRANSFORM_REQUIRES_RESAMPLING,
        });
    };
    if a * e - b * d == 0.0 {
        return Ok(Movement::Collapsed);
    }
    Ok(Movement::Resample(filter))
}

/// One layer to resample: its state, explicit anchor (if any), filter and mask linkage.
struct Resampled {
    state: LayerFrameState,
    /// A source-supplied whole matrix ([`TemporalSource::layer_matrix`]).
    matrix: Option<Affine2>,
    anchor: Option<[f64; 2]>,
    filter: ResampleFilter,
    linked_mask: bool,
}

fn shift(r: &mut Rect, (dx, dy): (i64, i64)) -> Result<(), TemporalError> {
    r.x = r.x.checked_add(dx).ok_or(CompositeError::Overflow)?;
    r.y = r.y.checked_add(dy).ok_or(CompositeError::Overflow)?;
    Ok(())
}

/// Lower frame `tick` (accord ticks, `TICKS_PER_SECOND` = 254016000000; frame <-> tick conversion
/// is the caller's, pulse owns it). See the module docs for the order of operations.
///
/// Refuses before any work: `timings` longer than `limits.max_steps` (`BudgetExceeded`), an
/// unknown layer id in `timings` (`DanglingInput`), `in_tick >= out_tick`, a repeated timing, and a
/// stale revision. Per animated layer: non-finite or out-of-range state, resampling transforms, a
/// translation of a non-raster layer, and a translation that leaves `i64` (`Overflow`, checked).
pub fn lower_frame(
    req: &FrameRequest<'_>,
    source: &dyn TemporalSource,
    tick: i64,
    cancel: &CancellationToken,
) -> Result<FrameLowering, TemporalError> {
    lower_frame_with_inputs(req, &RenderInputs::default(), source, tick, cancel)
}

/// [`lower_frame`] with the document's optional inputs (module docs, item 6). With empty inputs it
/// is [`lower_frame`] (the same lowering calls, byte-identical plans). Besides the refusals of
/// [`lower_frame`]: [`TRANSFORM_UNLINKED_MASK_WITH_CLIP`], and every refusal of
/// `evaluate_with_inputs` / `lower_with_inputs` (vector clip and vector mask codes included).
pub fn lower_frame_with_inputs(
    req: &FrameRequest<'_>,
    inputs: &RenderInputs<'_>,
    source: &dyn TemporalSource,
    tick: i64,
    cancel: &CancellationToken,
) -> Result<FrameLowering, TemporalError> {
    let canceled = || TemporalError::Composite(CompositeError::Canceled);
    cancel.check().map_err(|_| canceled())?;
    let doc = req.snapshot.document();
    if doc.revision != req.expected_revision {
        return Err(CompositeError::StaleRevision {
            expected: req.expected_revision,
            actual: doc.revision,
        }
        .into());
    }
    if req.timings.len() > req.limits.max_steps {
        return Err(CompositeError::BudgetExceeded.into());
    }
    let known: BTreeSet<&str> = doc.layers.iter().map(|l| l.layer_id.as_str()).collect();
    let mut windows: BTreeMap<&str, &LayerTiming> = BTreeMap::new();
    for t in req.timings {
        if !known.contains(t.layer_id.as_str()) {
            return Err(CompositeError::DanglingInput(t.layer_id.clone()).into());
        }
        if t.in_tick >= t.out_tick {
            return Err(TemporalError::InvalidTiming {
                layer_id: t.layer_id.clone(),
            });
        }
        if windows.insert(t.layer_id.as_str(), t).is_some() {
            return Err(TemporalError::DuplicateTiming {
                layer_id: t.layer_id.clone(),
            });
        }
    }

    let mut frame = None;
    let mut applied = Vec::new();
    let mut hidden = Vec::new();
    let mut moves: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    let mut linked_masks: BTreeSet<String> = BTreeSet::new();
    // Moved layers whose unlinked vector mask stays in plan space (re-rasterised below).
    let mut unlinked_moved: BTreeSet<String> = BTreeSet::new();
    let mut resampled: BTreeMap<String, Resampled> = BTreeMap::new();
    for (index, layer) in doc.layers.iter().enumerate() {
        cancel.check().map_err(|_| canceled())?;
        let id = layer.layer_id.as_str();
        let in_window = windows.get(id).is_none_or(|t| t.contains(tick));
        let state = if in_window {
            source.layer_state(id, tick, cancel)?
        } else {
            None
        };
        let mut show = in_window;
        let mut opacity_scale = 1.0;
        if let Some(s) = state {
            let matrix = source.layer_matrix(id, tick, cancel)?;
            let movement = match &matrix {
                Some(m) => admit_matrix(id, &s, m, source.resample_filter(id))?,
                None => admit_state(id, &s, source.resample_filter(id))?,
            };
            show = s.visible && !matches!(movement, Movement::Collapsed);
            // The layer's one mask slot: a vector clip / vector mask plane when the inputs carry
            // one (lane SMB-COMPOSITE-DOCGRAPH-MASKS), else the Folio raster mask.
            let vlink = vector_link(inputs.vectors, id);
            let linked = match vlink {
                Some(l) => l != VectorLink::Unlinked,
                None => layer
                    .appearance
                    .as_ref()
                    .and_then(|a| a.mask.as_ref())
                    .is_some_and(|m| m.linked),
            };
            let moves_layer = matches!(movement, Movement::Resample(_))
                || matches!(movement, Movement::Integer(dx, dy) if (dx, dy) != (0, 0));
            if show && moves_layer {
                match vlink {
                    Some(VectorLink::Split) => {
                        return Err(TemporalError::Unsupported {
                            layer_id: id.to_owned(),
                            feature: TRANSFORM_UNLINKED_MASK_WITH_CLIP,
                        });
                    }
                    Some(VectorLink::Unlinked) => {
                        unlinked_moved.insert(id.to_owned());
                    }
                    _ => {}
                }
            }
            if show && let Movement::Resample(filter) = movement {
                if !matches!(layer.payload, Payload::Raster { .. }) {
                    return Err(TemporalError::Unsupported {
                        layer_id: id.to_owned(),
                        feature: TRANSLATE_NON_RASTER_LAYER,
                    });
                }
                let anchor = match matrix {
                    Some(_) => None,
                    None => source.layer_anchor(id, tick, cancel)?,
                };
                if anchor.is_some_and(|a| !a.iter().all(|v| v.is_finite())) {
                    return Err(TemporalError::InvalidState {
                        layer_id: id.to_owned(),
                        field: "anchor_px",
                    });
                }
                resampled.insert(
                    id.to_owned(),
                    Resampled {
                        state: s,
                        matrix,
                        anchor,
                        filter,
                        linked_mask: linked,
                    },
                );
            }
            if show {
                opacity_scale = s.opacity_scale;
                let movement = match movement {
                    Movement::Integer(dx, dy) => (dx, dy),
                    _ => (0, 0),
                };
                if movement != (0, 0) {
                    if !matches!(layer.payload, Payload::Raster { .. }) {
                        return Err(TemporalError::Unsupported {
                            layer_id: id.to_owned(),
                            feature: TRANSLATE_NON_RASTER_LAYER,
                        });
                    }
                    moves.insert(id.to_owned(), movement);
                    if linked {
                        linked_masks.insert(id.to_owned());
                    }
                }
                applied.push((id.to_owned(), s));
            }
        }
        if !show {
            hidden.push(id.to_owned());
        }
        let hide = !show && layer.visible;
        if hide || opacity_scale != 1.0 {
            let d = frame.get_or_insert_with(|| doc.clone());
            let Some(l) = d.layers.get_mut(index) else {
                return Err(CompositeError::Overflow.into());
            };
            if hide {
                l.visible = false;
            }
            if opacity_scale != 1.0 {
                let mut a = l.effective_appearance();
                a.opacity *= opacity_scale;
                l.appearance = Some(a);
            }
        }
    }

    // Unchanged document: the caller's snapshot is the frame snapshot (static plan, no re-parse).
    let owned;
    let snap = match frame {
        None => req.snapshot,
        Some(d) => {
            owned = validate_document(d, req.budget, cancel, req.resolver)
                .map_err(|e| TemporalError::Document(format!("{:?}:{}", e.code, e.target)))?;
            &owned
        }
    };
    let plan = lower(
        snap,
        req.expected_revision,
        req.blend_space,
        &req.limits,
        cancel,
    )?;
    let plain = inputs.records.is_none() && inputs.text.is_none() && inputs.vectors.is_none();
    let (mut lowering, base_extensions, mut tables) = if plain {
        let report = evaluate(&plan, snap, cancel)?;
        (
            lower_to_render_plan(&plan, &report, snap, req.root, req.target, req.info)?,
            PlanExtensions::default(),
            FrameTables::default(),
        )
    } else {
        let report = evaluate_with_inputs(&plan, snap, inputs, cancel)?;
        let low = lower_with_inputs(&plan, &report, snap, inputs, req.root, req.target, req.info)?;
        (
            RenderLowering {
                plan: low.plan,
                sources: low.sources,
                mask_sources: low.mask_sources,
                exactness: low.exactness,
            },
            low.extensions,
            FrameTables {
                text_masks: low.text_masks,
                vector_masks: low.vector_masks,
                image_sources: low.image_sources,
                vector_rows: low.vector_rows,
                clip_planes: low.clip_planes,
                notes: low.notes,
            },
        )
    };
    if !unlinked_moved.is_empty() {
        cover_target(&mut lowering, &mut tables, inputs, &unlinked_moved)?;
    }

    if !moves.is_empty() {
        let tile_layer: BTreeMap<SourceKey, &str> = lowering
            .sources
            .iter()
            .map(|(k, t)| (*k, t.layer_id.as_str()))
            .collect();
        // Raster masks by Folio record, vector clip / vector mask planes by their layer.
        let mask_layer: BTreeMap<SourceKey, &str> = lowering
            .mask_sources
            .iter()
            .filter_map(|(k, tiles)| tiles.first().map(|t| (*k, t.layer_id.as_str())))
            .chain(tables.clip_planes.iter().map(|(k, p)| (*k, p.layer_id.as_str())))
            .collect();
        let mut tile_moves = Vec::new();
        let mut mask_moves = Vec::new();
        for (n, node) in lowering.plan.nodes.iter().enumerate() {
            match node {
                PlanNode::Tile(t) => {
                    if let Some(m) = tile_layer.get(&t.key).and_then(|l| moves.get(*l)) {
                        tile_moves.push((n, *m));
                    }
                }
                PlanNode::Group(g) => {
                    for (li, l) in g.layers.iter().enumerate() {
                        let Some(mask) = l.mask.filter(|m| !m.is_clip_to_base()) else {
                            continue;
                        };
                        if let Some(owner) = mask_layer.get(&mask.key)
                            && linked_masks.contains(*owner)
                            && let Some(m) = moves.get(*owner)
                        {
                            mask_moves.push((n, li, *m));
                        }
                    }
                }
                PlanNode::Solid { .. } | PlanNode::Vector(_) => {}
            }
        }
        for (n, m) in tile_moves {
            if let Some(PlanNode::Tile(t)) = lowering.plan.nodes.get_mut(n) {
                shift(&mut t.placement, m)?;
            }
        }
        for (n, li, m) in mask_moves {
            if let Some(PlanNode::Group(g)) = lowering.plan.nodes.get_mut(n)
                && let Some(mask) = g.layers.get_mut(li).and_then(|l| l.mask.as_mut())
            {
                shift(&mut mask.placement, m)?;
                // An owned plane is served at its plan placement (backends compare them).
                if let Some((_, p)) = tables.clip_planes.iter_mut().find(|(k, _)| *k == mask.key) {
                    p.placement = mask.placement;
                }
            }
        }
        lowering
            .plan
            .validate(&PlanLimits::default())
            .map_err(CompositeError::Plan)?;
    }
    let extensions = if resampled.is_empty() {
        base_extensions
    } else {
        transform_envelope(&mut lowering, &resampled, base_extensions)?
    };
    Ok(FrameLowering {
        lowering,
        tick,
        applied,
        hidden,
        extensions,
        tables,
    })
}

/// Re-rasterises the unlinked vector-mask plane of every layer in `layers` over its rect united
/// with the target extent (the mask stays in plan space while its layer moves, and backends
/// refuse a mask that does not cover the layer's content inside the target). The plane value
/// outside the mask's own rect is the mask's rule (zero, or the content mode's outside value), so
/// the pixels inside the old rect are unchanged.
fn cover_target(
    lowering: &mut RenderLowering,
    tables: &mut FrameTables,
    inputs: &RenderInputs<'_>,
    layers: &BTreeSet<String>,
) -> Result<(), TemporalError> {
    let Some(vectors) = inputs.vectors else {
        return Ok(());
    };
    let extent = lowering.plan.target.extent;
    for (key, p) in &mut tables.clip_planes {
        if !layers.contains(&p.layer_id) {
            continue;
        }
        let Some(Ok(mask)) = vectors.mask(&p.layer_id) else {
            continue;
        };
        let rect = crate::evaluate_static::union(Some(p.placement), extent)?
            .ok_or(CompositeError::Overflow)?;
        if rect == p.placement {
            continue;
        }
        let (coverage, _) = crate::vector_mask::plane(mask, None, rect)
            .map_err(|code| CompositeError::Unavailable(format!("{}:{code}", p.layer_id)))?;
        p.sha256 = hsk_studio_prism::profile_hash(&coverage);
        p.coverage = coverage;
        p.placement = rect;
        for node in &mut lowering.plan.nodes {
            let PlanNode::Group(g) = node else { continue };
            for l in &mut g.layers {
                if let Some(m) = l.mask.as_mut().filter(|m| m.key == *key) {
                    m.placement = rect;
                    m.sha256 = p.sha256;
                }
            }
        }
    }
    Ok(())
}

/// The envelope entries of the resampled layers: each layer's single tile node, every group layer
/// that references it, one `LayerTransform` with the After Effects affine. Records the transform
/// operator's class in the lowering ledger and validates the envelope with pigment. The transform
/// joins an entry `base` already has for the same group layer (mask feather, effects, blur).
fn transform_envelope(
    lowering: &mut RenderLowering,
    resampled: &BTreeMap<String, Resampled>,
    base: PlanExtensions,
) -> Result<PlanExtensions, TemporalError> {
    let owner: BTreeMap<SourceKey, &str> = lowering
        .sources
        .iter()
        .map(|(k, t)| (*k, t.layer_id.as_str()))
        .collect();
    // Tile nodes of each resampled layer, in node order.
    let mut tiles: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (n, node) in lowering.plan.nodes.iter().enumerate() {
        if let PlanNode::Tile(t) = node
            && let Some(layer) = owner.get(&t.key).filter(|l| resampled.contains_key(**l))
        {
            tiles.entry(layer).or_default().push(n);
        }
    }
    let mut extensions = base;
    for (id, nodes) in &tiles {
        let (Some(r), [tile_node]) = (resampled.get(*id), nodes.as_slice()) else {
            return Err(TemporalError::Unsupported {
                layer_id: (*id).to_owned(),
                feature: TRANSFORM_MULTI_TILE_LAYER,
            });
        };
        let Some(PlanNode::Tile(tile)) = lowering.plan.nodes.get(*tile_node) else {
            return Err(CompositeError::Overflow.into());
        };
        let p = tile.placement;
        let [ax, ay] = r.anchor.unwrap_or([
            p.x as f64 + f64::from(p.width) / 2.0,
            p.y as f64 + f64::from(p.height) / 2.0,
        ]);
        let s = &r.state;
        let affine = r.matrix.unwrap_or_else(|| {
            Affine2::from_parts(
                ax,
                ay,
                s.scale[0],
                s.scale[1],
                s.rotation_deg,
                s.translate_px[0],
                s.translate_px[1],
            )
        });
        for (g, node) in lowering.plan.nodes.iter().enumerate() {
            let PlanNode::Group(group) = node else {
                continue;
            };
            for (li, l) in group.layers.iter().enumerate() {
                if l.input.0 as usize != *tile_node {
                    continue;
                }
                let has_mask = l.mask.is_some_and(|m| !m.is_clip_to_base());
                let transform = LayerTransform::new(affine, r.filter)
                    .with_mask(r.linked_mask && has_mask);
                let (Ok(g32), Ok(li32)) = (u32::try_from(g), u32::try_from(li)) else {
                    return Err(CompositeError::Overflow.into());
                };
                match extensions
                    .layers
                    .iter_mut()
                    .find(|e| e.group == PlanNodeId(g32) && e.layer == li32)
                {
                    Some(e) => e.transform = Some(transform),
                    None => extensions.layers.push(
                        LayerExtension::new(PlanNodeId(g32), li32).with_transform(transform),
                    ),
                }
            }
        }
        let op = ExtOperator::Transform(r.filter);
        lowering.exactness.record(op.key(), op.intrinsic());
    }
    extensions
        .validate(&lowering.plan)
        .map_err(CompositeError::Plan)?;
    Ok(extensions)
}
