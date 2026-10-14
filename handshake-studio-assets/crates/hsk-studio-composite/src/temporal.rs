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
//! Honest limit of pigment's render-plan IR (it has no transform or resampling operator): only an
//! integer pixel translation is representable. A fractional translation, a scale other than 1 or a
//! rotation other than 0 is refused as [`TRANSFORM_REQUIRES_RESAMPLING`], never rounded. A non-zero
//! translation of a non-raster layer (a group) is refused as [`TRANSLATE_NON_RASTER_LAYER`]: the
//! tiles of its members are not addressed by its id. Frame documents and plans are disposable and
//! never stored. The motion adapter ([`motion::MotionTemporalSource`], feature `motion`) is the
//! only motion-aware code in composite; the default build keeps the static closure.
#![deny(clippy::arithmetic_side_effects)]

use crate::{
    CompositeError, Limits, NodeAddr, RenderLowering, TileInfoSource, evaluate, lower,
    lower_to_render_plan, outcome_of,
};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{Budget, Payload, Resolver, Snapshot, validate_document};
use hsk_studio_observe::{FailureCode, Outcome};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::BlendSpace;
use hsk_studio_pigment::render_plan::{OutputTarget, PlanLimits, PlanNode, SourceKey};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[cfg(feature = "motion")]
pub mod motion;

/// Refusal key: the frame needs pixel resampling, which pigment's plan IR cannot express.
pub const TRANSFORM_REQUIRES_RESAMPLING: &str = "transform_requires_resampling";
/// Refusal key: a non-zero translation on a layer whose own payload is not raster tiles.
pub const TRANSLATE_NON_RASTER_LAYER: &str = "translate_non_raster_layer";

/// One layer's state at one tick. [`LayerFrameState::IDENTITY`] changes nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerFrameState {
    /// `false` hides the layer for this frame; `true` keeps the authored visibility.
    pub visible: bool,
    /// Multiplies the authored opacity, `0.0..=1.0`.
    pub opacity_scale: f64,
    /// Offset from the authored placement in plan pixels (x right, y down); integers only.
    pub translate_px: [f64; 2],
    /// Scale factor per axis; anything other than `[1, 1]` is refused (resampling).
    pub scale: [f64; 2],
    /// Rotation in degrees; anything other than 0 is refused (resampling).
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

/// Checks one state and returns its integer translation.
fn admit_state(layer_id: &str, s: &LayerFrameState) -> Result<(i64, i64), TemporalError> {
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
    if s.scale != [1.0, 1.0] || s.rotation_deg != 0.0 {
        return Err(TemporalError::Unsupported {
            layer_id: layer_id.to_owned(),
            feature: TRANSFORM_REQUIRES_RESAMPLING,
        });
    }
    Ok((
        integer_px(layer_id, s.translate_px[0])?,
        integer_px(layer_id, s.translate_px[1])?,
    ))
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
            let movement = admit_state(id, &s)?;
            show = s.visible;
            if show {
                opacity_scale = s.opacity_scale;
                if movement != (0, 0) {
                    if !matches!(layer.payload, Payload::Raster { .. }) {
                        return Err(TemporalError::Unsupported {
                            layer_id: id.to_owned(),
                            feature: TRANSLATE_NON_RASTER_LAYER,
                        });
                    }
                    moves.insert(id.to_owned(), movement);
                    if layer
                        .appearance
                        .as_ref()
                        .and_then(|a| a.mask.as_ref())
                        .is_some_and(|m| m.linked)
                    {
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
    let report = evaluate(&plan, snap, cancel)?;
    let mut lowering =
        lower_to_render_plan(&plan, &report, snap, req.root, req.target, req.info)?;

    if !moves.is_empty() {
        let tile_layer: BTreeMap<SourceKey, &str> = lowering
            .sources
            .iter()
            .map(|(k, t)| (*k, t.layer_id.as_str()))
            .collect();
        let mask_layer: BTreeMap<SourceKey, &str> = lowering
            .mask_sources
            .iter()
            .filter_map(|(k, tiles)| tiles.first().map(|t| (*k, t.layer_id.as_str())))
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
            }
        }
        lowering
            .plan
            .validate(&PlanLimits::default())
            .map_err(CompositeError::Plan)?;
    }
    Ok(FrameLowering {
        lowering,
        tick,
        applied,
        hidden,
    })
}
