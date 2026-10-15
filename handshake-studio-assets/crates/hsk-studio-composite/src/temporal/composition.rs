//! Folio composition adapter (feature `motion`, lane SMB-COMPOSITE-COMPOSITION-ADAPTER): plays a
//! stored `StudioComposition` (Folio `hsk.studio.composition@1`). [`CompositionTemporalSource`]
//! reads the record set once (validated by Folio's `validate_composition_records`, so parent,
//! precomposition and matte references are acyclic), builds one motion `Property` per keyframe
//! stream (motion owns keyframe evaluation, STU-CMP-002) and evaluates a frame:
//!
//! - Timing windows: a layer exists for `in_point <= tick < out_point` (STU-MOT-006).
//! - Opacity: authored `LayerAppearance` opacity times the `opacity` stream (percent).
//! - Transform (STU-MOT-013, After Effects layer model): layer space has its origin at the top-left
//!   of the layer's content (raster tiles: the union of their placements); content point `x` lands
//!   at `position + R(rotation) S(scale) (x - anchor)` in composition pixels. Without streams the
//!   layer keeps its authored placement: the default anchor is the content centre and the default
//!   position is the content origin plus that anchor (identity).
//! - Anchor stream (STU-MOT-013a, layer space) and parent chain (STU-MOT-008): the content matrix is
//!   the layer's own transform followed by every parent's, so a parent's position, rotation and
//!   scale move the child (any affine, shear included); parenting never inherits opacity, blend
//!   mode or matte. Source time: [`CompositionTemporalSource::source_tick`] maps a composition
//!   tick through `start_time` and the rational `stretch`.
//! - The composition's resampling filter is DECLARED by the source ([`DEFAULT_COMPOSITION_FILTER`]
//!   = bilinear unless [`CompositionTemporalSource::with_filter`] picks another; STU-MOT-007 names
//!   `bilinear | bicubic` and no default; Folio stores no `resample_filter`, so this is the
//!   adapter's stated reading).
//!
//! - Track mattes (STU-CMP-030): a layer's matte names ANOTHER layer of the same composition (any
//!   stack position, one matte may serve several layers); the matte layer is evaluated at the tick
//!   (its window, opacity, transform, parent chain and own matte; its `visible` flag does not
//!   matter) and carried as [`SceneMatte`]; a matte source never renders on its own (STU-CMP-030:
//!   by default the matte layer does not render); outside its window the matte is absent.
//! - Precompositions (STU-MOT-005 `composition`): a layer whose record names a nested composition
//!   (its Folio layer is a content-less `group`) shows that composition evaluated at the layer's
//!   source tick ([`CompositionTemporalSource::source_tick`]: `start_time`, `stretch`, reverse);
//!   outside `[0, duration)` of the nested composition it shows nothing; the nested composition's
//!   background is not drawn (transparent, the After Effects nesting behaviour as Studio reads it,
//!   UNVERIFIED); nesting is bounded by [`MAX_PRECOMP_DEPTH`]. `collapse_transformations` is
//!   refused (`composition_precomp_collapse_transformations`).
//!
//! [`CompositionTemporalSource::scene_at`] returns the folio-free [`SceneComposition`] that
//! `composition::lower_scene` / `render_scene` lower and render. The type also implements
//! [`TemporalSource`] for `lower_frame` over a document whose graph mirrors the composition stack.
//!
//! Refused typed (never ignored): records Folio refuses (`Document`), an unknown composition id
//! (`DanglingInput`), a stale revision, layer content other than raster tiles or a null layer
//! (`composition_layer_content`; a null layer is a Folio `group` layer with a `group` payload,
//! since Folio binds no `null` kind), clipped or masked layers (`composition_layer_clipped`,
//! `composition_layer_mask`), a pass-through blend on a content layer, sampled opacity outside
//! `0..=100` (`InvalidState`), a precomposition layer whose Folio payload is not a content-less
//! group (`composition_precomp_payload`), a precomposition, a matted layer or a matte source on
//! the document-graph path (`composition_precomp_on_document_graph`,
//! `composition_track_matte_on_document_graph`). Not modelled (Folio stores none of them, so
//! nothing is dropped): 3D layers and cameras/lights, motion blur, expressions, frame blending,
//! time remapping, `preserve_source_visibility` of a matte (the spec default, not rendered, is
//! applied).
#![deny(clippy::arithmetic_side_effects)]

use super::motion::sample;
use super::{LayerFrameState, LayerTiming, TemporalError, TemporalSource};
use crate::composition::{
    OwnedMask, SceneComposition, SceneContent, SceneLayer, SceneMatte, SceneMatteMode, SceneTile,
    StaticContent, Surface,
};
use crate::lower_extensions::{REFUSE_ADJUSTMENT_RECORD, adjustment_for, is_adjustment_layer};
use super::{FrameLowering, FrameRequest, lower_frame_with_inputs};
use crate::composition::{SceneContext, render_scene};
use crate::{
    Limits, NodeAddr, RenderInputs, StepStatus, evaluate_with_inputs, lower, lower_with_inputs,
};
use hsk_studio_pigment::render_plan::{
    Group, Layer, LayerExtension, MaskFormat, MaskInput, PlanNode, PlanNodeId, PlanSources, SourceKey, TilePlanes,
};
use std::collections::BTreeSet;
use hsk_studio_pigment::Mask;
use hsk_studio_pigment::adjust_math::AdjustmentOp;
use hsk_studio_pigment::effect_math::window::stack_support_px;
use hsk_studio_pigment::render_plan::{AlphaConvention, ColourTag, OutputTarget, PlanLimits, SampleFormat};
use crate::evaluate_static::tile_rects;
use crate::{CompositeError, TileInfoSource};
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{
    Budget, Payload, ProfileBinding, validate_document, Resolver, Snapshot, StudioComposition,
    StudioCompositionRecords, StudioInterpolation, StudioLayerProperty, StudioLayerTiming,
    StudioMatteMode, StudioPropertyStream, StudioRgba, StudioTrackMatte, TileRef,
    validate_composition_records,
};
use hsk_studio_motion::{Interpolation, Keyframe, MotionError, Property, TemporalTangent};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::{BlendSpace, StudioBlendMode};
use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter};
use std::collections::BTreeMap;

/// The resampling filter a composition declares unless the caller picks another (module docs).
pub const DEFAULT_COMPOSITION_FILTER: ResampleFilter = ResampleFilter::Bilinear;

/// Refusal keys (`TemporalError::Unsupported::feature`).
pub const COMPOSITION_LAYER_CONTENT: &str = "composition_layer_content";
pub const COMPOSITION_LAYER_CLIPPED: &str = "composition_layer_clipped";
pub const COMPOSITION_LAYER_MASK: &str = "composition_layer_mask";
pub const COMPOSITION_LAYER_PASS_THROUGH: &str = "composition_layer_pass_through";
pub const COMPOSITION_PRECOMP_COLLAPSE: &str = "composition_precomp_collapse_transformations";
/// A collapsed precomposition with a blend mode other than normal or with a track matte.
pub const COMPOSITION_COLLAPSE_BLEND: &str = "composition_collapse_blend_mode";
pub const COMPOSITION_COLLAPSE_MATTE: &str = "composition_collapse_matte";
pub const COMPOSITION_PRECOMP_PAYLOAD: &str = "composition_precomp_payload";
pub const COMPOSITION_PRECOMP_ON_DOCUMENT_GRAPH: &str = "composition_precomp_on_document_graph";
/// Precomposition nesting bound of [`CompositionTemporalSource::scene_at`] (Folio refuses cycles;
/// the record set holds at most 64 compositions).
pub const MAX_PRECOMP_DEPTH: usize = 16;
pub const COMPOSITION_TRACK_MATTE_ON_DOCUMENT_GRAPH: &str = "composition_track_matte_on_document_graph";
pub const COMPOSITION_LAYER_NOT_IN_GRAPH: &str = "composition_layer_not_in_graph";
pub const COMPOSITION_SPACE: &str = "composition_space";
pub const COMPOSITION_BACKGROUND_PROFILE: &str = "composition_background_profile";
pub const COMPOSITION_BACKGROUND_LINEAR_LIGHT: &str = "composition_background_linear_light";

/// Everything [`CompositionTemporalSource::new`] reads. `budget` and `resolver` are the ones the
/// snapshot was opened with (Folio validates the records against the document).
#[derive(Clone, Copy)]
pub struct CompositionInput<'a> {
    pub snapshot: &'a Snapshot,
    pub expected_revision: u64,
    pub records: &'a StudioCompositionRecords,
    pub composition_id: &'a str,
    pub budget: Budget,
    pub resolver: &'a dyn Resolver,
    pub blend_space: BlendSpace,
    /// Plane hashes and sample format of each raster tile (as `lower_frame`).
    pub info: &'a dyn TileInfoSource,
    /// Colour tag of the composition's plans (`blend_space` must equal `space.blend_space`);
    /// static subtrees are lowered for it.
    pub space: ColourTag,
    /// Raster records, prepared text and vectors for layers drawn through the static path
    /// (masks, effects, blur, adjustments, vector and text layers).
    pub inputs: RenderInputs<'a>,
}

#[derive(Clone, Debug)]
struct Streams {
    anchor: Option<Property>,
    position: Option<Property>,
    scale: Option<Property>,
    rotation: Option<Property>,
    opacity: Option<Property>,
}

#[derive(Clone, Debug)]
enum Content {
    None,
    Tiles { tiles: Vec<SceneTile>, bounds: Rect },
    /// A static subtree; `bounds` = the content extent (layer space), `content.rect` adds support.
    /// `unlinked`: the layer's unlinked vector mask as a composition-space alpha matte
    /// (`crate::composition::unlinked_mask_matte`, lane SMB-COMPOSITE-VECTOR-MASK-2).
    Static { content: Box<StaticContent>, bounds: Rect, unlinked: Option<Box<SceneMatte>> },
    /// An adjustment layer.
    Adjustment(AdjustmentOp),
    /// A nested composition; `bounds` = `(0, 0, width, height)` of it.
    Precomp {
        composition_id: String,
        /// STU-MOT-006 collapse transformations: the nested layers join this composition.
        collapse: bool,
        bounds: Rect,
    },
}

impl Content {
    fn bounds(&self) -> Option<Rect> {
        match self {
            Self::None | Self::Adjustment(_) => None,
            Self::Tiles { bounds, .. } | Self::Precomp { bounds, .. } | Self::Static { bounds, .. } => {
                Some(*bounds)
            }
        }
    }
    fn origin(&self) -> [f64; 2] {
        self.bounds().map_or([0.0, 0.0], |b| [b.x as f64, b.y as f64])
    }
}

#[derive(Clone, Debug)]
struct CompLayer {
    layer_id: String,
    timing: StudioLayerTiming,
    parent: Option<String>,
    matte: Option<StudioTrackMatte>,
    clipped: bool,
    visible: bool,
    mode: StudioBlendMode,
    opacity: f64,
    fill_opacity: f64,
    streams: Streams,
    content: Content,
}

/// One layer's sampled transform group.
struct Parts {
    anchor: [f64; 2],
    position: [f64; 2],
    scale: [f64; 2],
    rotation: f64,
}

#[derive(Clone, Debug)]
struct Comp {
    composition_id: String,
    width: u32,
    height: u32,
    ticks_per_frame: u64,
    duration: u64,
    background: Option<[f32; 4]>,
    /// Topmost first, as stored (STU-MOT-009).
    layers: Vec<CompLayer>,
}

/// A stored composition ready to be evaluated at any tick (module docs).
#[derive(Clone, Debug)]
pub struct CompositionTemporalSource {
    root: String,
    /// The root and every composition it reaches through precomposition layers.
    comps: BTreeMap<String, Comp>,
    filter: ResampleFilter,
    tiles: BTreeMap<String, TileRef>,
}

/// A layer that needs the static path: a vector, text or live boolean group layer, an enabled mask,
/// a vector clip or vector mask record (lanes SMB-COMPOSITE-CLIP-RECORDS, -VECTOR-MASK),
/// or a raster-records entry (effects, blur, mask parameters, Blend-If, knockout).
fn needs_static(input: &CompositionInput<'_>, dl: &hsk_studio_folio::StudioLayer) -> bool {
    match &dl.payload {
        Payload::Vector { .. } | Payload::Text { .. } => true,
        Payload::Group => input.inputs.vectors.is_some_and(|v| v.knows(&dl.layer_id)),
        _ => {
            dl.appearance
                .as_ref()
                .and_then(|a| a.mask.as_ref())
                .is_some_and(|m| m.enabled)
                || input.inputs.records.is_some_and(|r| r.layer(&dl.layer_id).is_some())
                || input
                    .inputs
                    .vectors
                    .is_some_and(|v| v.clip(&dl.layer_id).is_some() || v.mask(&dl.layer_id).is_some())
        }
    }
}

/// The static subtree of one document layer for a composition (`SceneContent::Static`): the
/// document's own lowering (`lower_with_inputs`, raster records, prepared text and vectors) rooted at
/// the layer's graph operation, with the layer's opacity at 100 % and its blend mode normal (both
/// are applied by the composition after the transform, STU-CMP-010 steps 6 and 8), its fill
/// opacity, masks, effects and blur kept (steps 2-4). The surface is the layer's content extent
/// grown by the effect and blur support of its envelope. `None` = the layer draws nothing.
fn static_content(
    input: &CompositionInput<'_>,
    layer_id: &str,
    cancel: &CancellationToken,
) -> Result<Option<(StaticContent, Rect, Option<SceneMatte>)>, TemporalError> {
    let doc = input.snapshot.document();
    // An unlinked vector mask stays in composition space: it leaves the static subtree and becomes
    // an alpha matte of its plane (lane SMB-COMPOSITE-VECTOR-MASK-2).
    let unlinked = input.inputs.vectors.and_then(|v| match v.mask(layer_id) {
        Some(Ok(m)) if !m.linked => Some(m.clone()),
        _ => None,
    });
    let stripped = match (&unlinked, input.inputs.vectors) {
        (Some(_), Some(v)) => Some(v.without_mask(layer_id)),
        _ => None,
    };
    let inputs = RenderInputs {
        records: input.inputs.records,
        text: input.inputs.text,
        vectors: stripped.as_ref().or(input.inputs.vectors),
    };
    let unlinked = match unlinked {
        Some(m) => Some(
            crate::composition::unlinked_mask_matte(&m, layer_id, input.space, doc.revision)
                .map_err(TemporalError::from)?,
        ),
        None => None,
    };
    let op = doc
        .graph
        .operations
        .iter()
        .find(|o| o.layer_id == layer_id)
        .ok_or_else(|| unsupported(layer_id, COMPOSITION_LAYER_NOT_IN_GRAPH))?;
    let port = op
        .outputs
        .first()
        .ok_or_else(|| unsupported(layer_id, COMPOSITION_LAYER_NOT_IN_GRAPH))?;
    let root = NodeAddr {
        layer_id: layer_id.to_owned(),
        operation_key: op.operation_key.clone(),
        port_key: port.port_key.clone(),
    };
    let mut frame = doc.clone();
    let layer = frame
        .layers
        .iter_mut()
        .find(|l| l.layer_id == layer_id)
        .ok_or_else(|| CompositeError::DanglingInput(layer_id.to_owned()))?;
    layer.visible = true;
    if let Some(a) = layer.appearance.as_mut() {
        a.opacity = 100.0;
        a.blend_mode = hsk_studio_folio::BLEND_NORMAL;
        // Clipping applies in the composition stack (SceneLayer::clipped), not in the subtree.
        a.clipped = false;
    }
    let snap = validate_document(frame, input.budget, cancel, input.resolver)
        .map_err(|e| TemporalError::Document(format!("{:?}:{}", e.code, e.target)))?;
    let plan = lower(&snap, doc.revision, input.space.blend_space, &Limits::default(), cancel)?;
    let report = evaluate_with_inputs(&plan, &snap, &inputs, cancel)?;
    let step = report
        .step(&root)
        .ok_or_else(|| CompositeError::DanglingInput(layer_id.to_owned()))?;
    if let StepStatus::Unavailable(code) = &step.status {
        return Err(CompositeError::Unavailable(format!("{layer_id}:{code}")).into());
    }
    let Some(bounds) = step.extent else {
        return Ok(None);
    };
    let target = OutputTarget {
        extent: bounds,
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: input.space,
    };
    let low = lower_with_inputs(&plan, &report, &snap, &inputs, &root, target, input.info)?;
    // Effect and blur support around the content (the static extent is the content's own).
    let mut margin = 0u32;
    for e in &low.extensions.layers {
        let fx = if e.effects.is_empty() {
            0
        } else {
            stack_support_px(&e.effects).map_err(|_| CompositeError::Overflow)?
        };
        let blur = match &e.blur {
            Some(b) => b.support_px().map_err(|_| CompositeError::Overflow)?,
            None => 0,
        };
        margin = margin
            .checked_add(fx)
            .and_then(|m| m.checked_add(blur))
            .ok_or(CompositeError::Overflow)?;
    }
    let grow = |v: u32| v.checked_add(margin.checked_mul(2)?);
    let rect = Rect {
        x: bounds.x.checked_sub(i64::from(margin)).ok_or(CompositeError::Overflow)?,
        y: bounds.y.checked_sub(i64::from(margin)).ok_or(CompositeError::Overflow)?,
        width: grow(bounds.width).ok_or(CompositeError::Overflow)?,
        height: grow(bounds.height).ok_or(CompositeError::Overflow)?,
    };
    let mut plan = low.plan.clone();
    plan.target.extent = rect;
    plan.validate(&PlanLimits::default()).map_err(CompositeError::Plan)?;
    let mut content = StaticContent {
        plan,
        extensions: low.extensions.clone(),
        rect,
        tiles: BTreeMap::new(),
        masks: BTreeMap::new(),
        owned_masks: BTreeMap::new(),
        owned_tiles: BTreeMap::new(),
    };
    for (key, t) in &low.sources {
        content.tiles.insert(*key, format!("{}/{}", t.layer_id, t.object_key));
    }
    for (key, tiles) in &low.mask_sources {
        let id: Vec<String> = tiles.iter().map(|t| format!("{}/{}", t.layer_id, t.object_key)).collect();
        content.masks.insert(*key, id.join("|"));
    }
    for (key, source) in &low.image_sources {
        content.tiles.insert(*key, source.clone());
    }
    let own = |m: Mask<'_>| OwnedMask {
        bounds: m.bounds,
        sample_format: m.sample_format.to_owned(),
        bytes: m.bytes.to_vec(),
        stride_bytes: m.stride_bytes,
        sha256: m.sha256,
    };
    for (key, layer) in &low.text_masks {
        let m = input
            .inputs
            .text
            .and_then(|t| t.plane(layer))
            .and_then(|p| p.mask())
            .ok_or_else(|| CompositeError::Unavailable(format!("{layer}:text_layer_not_prepared")))?;
        content.owned_masks.insert(*key, own(m));
    }
    if let Some(vectors) = inputs.vectors {
        let keys = low
            .vector_masks
            .iter()
            .map(|(k, _)| *k)
            .chain(low.vector_rows.iter().map(|(k, _, _)| *k))
            // Vector clip and vector mask planes (lanes SMB-COMPOSITE-CLIP-RECORDS, -VECTOR-MASK):
            // the static subtree carries them, so they move with the layer (linked).
            .chain(low.clip_planes.iter().map(|(k, _)| *k));
        for key in keys {
            if let Some(m) = low.vector_mask(key, vectors) {
                content.owned_masks.insert(key, own(m));
            } else if let Some(t) = low.vector_paint(key, vectors, input.space) {
                content.owned_tiles.insert(
                    key,
                    Surface {
                        width: t.width,
                        height: t.height,
                        colour: t.colour.to_vec(),
                        alpha: t.alpha.to_vec(),
                        colour_sha256: t.colour_sha256,
                        alpha_sha256: t.alpha_sha256,
                    },
                );
            }
        }
    }
    Ok(Some((content, bounds, unlinked)))
}

fn unsupported(layer_id: &str, feature: &'static str) -> TemporalError {
    TemporalError::Unsupported {
        layer_id: layer_id.to_owned(),
        feature,
    }
}

fn interpolation(i: StudioInterpolation) -> Interpolation {
    match i {
        StudioInterpolation::Linear => Interpolation::Linear,
        StudioInterpolation::Bezier => Interpolation::Bezier,
        StudioInterpolation::ContinuousBezier => Interpolation::ContinuousBezier,
        StudioInterpolation::AutoBezier => Interpolation::AutoBezier,
        StudioInterpolation::Hold => Interpolation::Hold,
    }
}

/// Motion `Property` of one Folio stream (the mapping SMB-FOLIO-COMPOSITION documents).
fn property(layer_id: &str, s: &StudioPropertyStream) -> Result<Property, TemporalError> {
    let tangents = |t: &[hsk_studio_folio::StudioTemporalTangent]| -> Vec<TemporalTangent> {
        t.iter()
            .map(|t| TemporalTangent {
                speed: t.speed,
                influence: t.influence,
            })
            .collect()
    };
    let keys = s
        .keyframes
        .iter()
        .map(|k| Keyframe {
            time: Ticks::new(k.time),
            value: k.value.clone(),
            in_interp: interpolation(k.in_interp),
            out_interp: interpolation(k.out_interp),
            in_tangent: tangents(&k.in_tangent),
            out_tangent: tangents(&k.out_tangent),
        })
        .collect();
    Property::new(s.property.wire(), s.static_value.clone(), keys).map_err(|e: MotionError| {
        TemporalError::Source {
            layer_id: layer_id.to_owned(),
            code: e.code().to_owned(),
        }
    })
}

/// A background colour in plan units (no conversion: the vector-paint rule).
fn background(
    c: &StudioRgba,
    blending: &ProfileBinding,
    space: BlendSpace,
    composition_id: &str,
) -> Result<[f32; 4], TemporalError> {
    if let ProfileBinding::Assigned(p) = &c.colour_profile_id
        && !matches!(blending, ProfileBinding::Assigned(b) if b == p)
    {
        return Err(unsupported(composition_id, COMPOSITION_BACKGROUND_PROFILE));
    }
    // Folio validates the components `0..=1`.
    #[allow(clippy::cast_possible_truncation)]
    let rgba = [c.r as f32, c.g as f32, c.b as f32, c.a as f32];
    if space == BlendSpace::LinearLight && rgba[..3].iter().any(|v| *v != 0.0 && *v != 1.0) {
        return Err(unsupported(composition_id, COMPOSITION_BACKGROUND_LINEAR_LIGHT));
    }
    Ok(rgba)
}

fn bounds_of(rects: &[Rect]) -> Result<Option<Rect>, TemporalError> {
    let mut acc: Option<(i64, i64, i64, i64)> = None;
    for r in rects {
        let (x1, y1) = r.end().map_err(|_| CompositeError::Overflow)?;
        acc = Some(match acc {
            None => (r.x, r.y, x1, y1),
            Some((a, b, c, d)) => (a.min(r.x), b.min(r.y), c.max(x1), d.max(y1)),
        });
    }
    let Some((x0, y0, x1, y1)) = acc else {
        return Ok(None);
    };
    let w = x1.checked_sub(x0).and_then(|v| u32::try_from(v).ok());
    let h = y1.checked_sub(y0).and_then(|v| u32::try_from(v).ok());
    match (w, h) {
        (Some(width), Some(height)) => Ok(Some(Rect {
            x: x0,
            y: y0,
            width,
            height,
        })),
        _ => Err(CompositeError::Overflow.into()),
    }
}

/// A nested layer of a collapsed precomposition re-expressed in the outer composition: its matrix
/// (and its matte layer's) followed by `outer`, its opacity times `opacity`.
fn collapse_into(mut l: SceneLayer, outer: &Affine2, opacity: f64) -> SceneLayer {
    place_after(&mut l, outer);
    l.opacity *= opacity;
    l
}

/// `outer` after the matrix of `l` and of every matte layer in its chain (mattes keep their own
/// opacity; only their placement follows the collapse).
fn place_after(l: &mut SceneLayer, outer: &Affine2) {
    l.matrix = l.matrix.then(*outer);
    if let Some(src) = l.matte.as_mut().and_then(|m| m.layer.as_mut()) {
        place_after(src, outer);
    }
}

/// `floor((tick - start_time) * denominator / numerator)` in `i128` (see
/// [`CompositionTemporalSource::source_tick`]); `None` for a zero stretch or a result outside `i64`.
fn map_source(t: &StudioLayerTiming, tick: i64) -> Option<i64> {
    let offset = i128::from(tick).checked_sub(i128::from(t.start_time))?;
    let scaled = offset.checked_mul(i128::from(t.stretch.denominator))?;
    let divisor = i128::from(t.stretch.numerator);
    let mut q = scaled.checked_div(divisor)?;
    let r = scaled.checked_rem(divisor)?;
    if r != 0 && ((r < 0) != (divisor < 0)) {
        q = q.checked_sub(1)?;
    }
    i64::try_from(q).ok()
}

impl CompositionTemporalSource {
    /// Read and validate the records (module docs). Every refusal is typed; nothing is dropped.
    pub fn new(input: &CompositionInput<'_>, cancel: &CancellationToken) -> Result<Self, TemporalError> {
        cancel.check().map_err(|_| TemporalError::Composite(CompositeError::Canceled))?;
        let doc = input.snapshot.document();
        if doc.revision != input.expected_revision {
            return Err(CompositeError::StaleRevision {
                expected: input.expected_revision,
                actual: doc.revision,
            }
            .into());
        }
        if input.space.blend_space != input.blend_space {
            return Err(unsupported(input.composition_id, COMPOSITION_SPACE));
        }
        validate_composition_records(input.records, doc, input.budget, cancel, input.resolver)
            .map_err(|e| TemporalError::Document(format!("{:?}:{}", e.code, e.target)))?;
        let mut tiles = BTreeMap::new();
        let mut comps = BTreeMap::new();
        let mut pending = vec![input.composition_id.to_owned()];
        while let Some(id) = pending.pop() {
            if comps.contains_key(&id) {
                continue;
            }
            let comp = input
                .records
                .composition(&id)
                .ok_or_else(|| CompositeError::DanglingInput(id.clone()))?;
            let read = Self::read(comp, input, &mut tiles, cancel)?;
            for l in &read.layers {
                if let Content::Precomp { composition_id, .. } = &l.content {
                    pending.push(composition_id.clone());
                }
            }
            comps.insert(id, read);
        }
        Ok(Self {
            root: input.composition_id.to_owned(),
            comps,
            filter: DEFAULT_COMPOSITION_FILTER,
            tiles,
        })
    }

    fn read(
        comp: &StudioComposition,
        input: &CompositionInput<'_>,
        tiles_out: &mut BTreeMap<String, TileRef>,
        cancel: &CancellationToken,
    ) -> Result<Comp, TemporalError> {
        let doc = input.snapshot.document();
        let ticks_per_frame = comp
            .frame_rate
            .ticks_per_frame()
            .ok_or_else(|| TemporalError::Document(format!("InvalidField:{}/frame_rate", comp.composition_id)))?;
        let background = comp
            .background
            .as_ref()
            .map(|c| background(c, &doc.blending_space, input.blend_space, &comp.composition_id))
            .transpose()?;
        let mut layers = Vec::with_capacity(comp.layers.len());
        for l in &comp.layers {
            cancel.check().map_err(|_| TemporalError::Composite(CompositeError::Canceled))?;
            let id = l.layer_id.as_str();
            let dl = doc
                .layers
                .iter()
                .find(|d| d.layer_id == l.layer_id)
                .ok_or_else(|| CompositeError::DanglingInput(l.layer_id.clone()))?;
            let content = match (&l.precomp, &dl.payload) {
                (Some(p), Payload::Group) => {
                    let nested = input
                        .records
                        .composition(&p.composition_id)
                        .ok_or_else(|| CompositeError::DanglingInput(p.composition_id.clone()))?;
                    Content::Precomp {
                        composition_id: p.composition_id.clone(),
                        collapse: p.collapse_transformations,
                        bounds: Rect {
                            x: 0,
                            y: 0,
                            width: nested.width,
                            height: nested.height,
                        },
                    }
                }
                (Some(_), _) => return Err(unsupported(id, COMPOSITION_PRECOMP_PAYLOAD)),
                // Masks, raster records (effects, blur, mask parameters, Blend-If), vector and text
                // layers and live boolean groups: the static subtree.
                (None, Payload::Raster { .. } | Payload::Vector { .. } | Payload::Text { .. } | Payload::Group)
                    if needs_static(input, dl) =>
                {
                    match static_content(input, id, cancel)? {
                        Some((content, bounds, unlinked)) => Content::Static {
                            content: Box::new(content),
                            bounds,
                            unlinked: unlinked.map(Box::new),
                        },
                        None => Content::None,
                    }
                }
                (None, Payload::PrimitiveReference { .. }) if is_adjustment_layer(dl) => {
                    let records = input
                        .inputs
                        .records
                        .ok_or_else(|| unsupported(id, REFUSE_ADJUSTMENT_RECORD))?;
                    let converted = adjustment_for(records, id, input.blend_space)
                        .map_err(|code| unsupported(id, code))?;
                    Content::Adjustment(converted.op)
                }
                (None, Payload::Group) => Content::None,
                (None, Payload::Raster { tiles }) if tiles.is_empty() => Content::None,
                (None, Payload::Raster { tiles }) => {
                    let rects = tile_rects(tiles)?;
                    let mut scene_tiles = Vec::with_capacity(tiles.len());
                    for (t, rect) in tiles.iter().zip(&rects) {
                        let found = input.info.tile_info(t).ok_or_else(|| {
                            CompositeError::Unavailable(format!("{}/tile/{}:tile_info", t.layer_id, t.object_key))
                        })?;
                        let tile_id = format!("{}/{}", t.layer_id, t.object_key);
                        tiles_out.insert(tile_id.clone(), t.clone());
                        scene_tiles.push(SceneTile {
                            tile_id,
                            placement: *rect,
                            format: found.format,
                            alpha: found.alpha,
                            colour_sha256: found.colour_sha256,
                            alpha_sha256: found.alpha_sha256,
                        });
                    }
                    let bounds = bounds_of(&rects)?.ok_or(CompositeError::Overflow)?;
                    Content::Tiles {
                        tiles: scene_tiles,
                        bounds,
                    }
                }
                _ => return Err(unsupported(id, COMPOSITION_LAYER_CONTENT)),
            };
            let a = dl.effective_appearance();
            // A precomposition layer is a Folio `group`, whose identity appearance is pass-through;
            // its rendered surface composites like any layer, so an UNAUTHORED appearance means
            // normal. An authored pass-through stays refused below.
            let mode = if dl.appearance.is_none() && matches!(content, Content::Precomp { .. }) {
                StudioBlendMode::Normal
            } else {
                StudioBlendMode::try_from(a.blend_mode).map_err(CompositeError::from)?
            };
            if mode == StudioBlendMode::PassThrough && !matches!(content, Content::None) {
                return Err(unsupported(id, COMPOSITION_LAYER_PASS_THROUGH));
            }
            let stream = |p: StudioLayerProperty| -> Result<Option<Property>, TemporalError> {
                l.stream(p).map(|s| property(id, s)).transpose()
            };
            layers.push(CompLayer {
                layer_id: l.layer_id.clone(),
                timing: l.timing,
                parent: l.parent.clone(),
                matte: l.track_matte.clone(),
                clipped: a.clipped,
                visible: dl.visible,
                mode,
                opacity: a.opacity_fraction(),
                fill_opacity: a.fill_opacity_fraction(),
                streams: Streams {
                    anchor: stream(StudioLayerProperty::Anchor)?,
                    position: stream(StudioLayerProperty::Position)?,
                    scale: stream(StudioLayerProperty::Scale)?,
                    rotation: stream(StudioLayerProperty::Rotation)?,
                    opacity: stream(StudioLayerProperty::Opacity)?,
                },
                content,
            });
        }
        Ok(Comp {
            composition_id: comp.composition_id.clone(),
            width: comp.width,
            height: comp.height,
            ticks_per_frame,
            duration: comp.duration,
            background,
            layers,
        })
    }

    fn root(&self) -> Result<&Comp, TemporalError> {
        self.comps
            .get(&self.root)
            .ok_or_else(|| CompositeError::DanglingInput(self.root.clone()).into())
    }

    /// Declare another resampling filter for the composition's transformed layers.
    pub fn with_filter(mut self, filter: ResampleFilter) -> Self {
        self.filter = filter;
        self
    }

    pub fn filter(&self) -> ResampleFilter {
        self.filter
    }

    /// The composition tick of frame `frame` (`frame * ticks_per_frame`, checked).
    pub fn frame_tick(&self, frame: u64) -> Result<i64, TemporalError> {
        frame
            .checked_mul(self.root()?.ticks_per_frame)
            .and_then(|t| i64::try_from(t).ok())
            .ok_or_else(|| CompositeError::Overflow.into())
    }

    /// The layer windows of the composition, for `FrameRequest::timings` (`lower_frame`).
    pub fn timings(&self) -> Vec<LayerTiming> {
        self.comps
            .get(&self.root)
            .map_or(&[][..], |c| &c.layers[..])
            .iter()
            .map(|l| LayerTiming {
                layer_id: l.layer_id.clone(),
                // Folio limits ticks to `0..=i64::MAX`.
                in_tick: i64::try_from(l.timing.in_point).unwrap_or(i64::MAX),
                out_tick: i64::try_from(l.timing.out_point).unwrap_or(i64::MAX),
            })
            .collect()
    }

    /// The authored tile record behind a [`SceneTile::tile_id`] (the caller resolves its pixels).
    pub fn tile_ref(&self, tile_id: &str) -> Option<&TileRef> {
        self.tiles.get(tile_id)
    }

    /// A layer of the root composition.
    fn find(&self, layer_id: &str) -> Option<&CompLayer> {
        self.comps
            .get(&self.root)?
            .layers
            .iter()
            .find(|l| l.layer_id == layer_id)
    }

    /// A root-composition layer for the document-graph path, which cannot render a
    /// precomposition (the graph has no surface for it): refused, never drawn as an empty group.
    fn graph_layer(&self, layer_id: &str) -> Result<Option<&CompLayer>, TemporalError> {
        match self.find(layer_id) {
            Some(l) if matches!(l.content, Content::Precomp { .. }) => {
                Err(unsupported(layer_id, COMPOSITION_PRECOMP_ON_DOCUMENT_GRAPH))
            }
            Some(l)
                if l.matte.is_some()
                    || self.root().is_ok_and(|c| {
                        c.layers.iter().any(|o| {
                            o.matte
                                .as_ref()
                                .is_some_and(|m| m.source_layer_id == layer_id)
                        })
                    }) =>
            {
                Err(unsupported(layer_id, COMPOSITION_TRACK_MATTE_ON_DOCUMENT_GRAPH))
            }
            other => Ok(other),
        }
    }

    /// Sampled opacity stream as a fraction (absent = 1).
    fn opacity_scale(l: &CompLayer, tick: i64, cancel: &CancellationToken) -> Result<f64, TemporalError> {
        let Some(v) = sample(&l.layer_id, l.streams.opacity.as_ref(), 1, tick, cancel)? else {
            return Ok(1.0);
        };
        let pct = v.first().copied().unwrap_or(f64::NAN);
        if !pct.is_finite() || !(0.0..=100.0).contains(&pct) {
            return Err(TemporalError::InvalidState {
                layer_id: l.layer_id.clone(),
                field: "opacity",
            });
        }
        Ok(pct / 100.0)
    }

    /// Anchor (layer space), position (composition pixels), scale factor and rotation (degrees).
    fn transform_parts(
        l: &CompLayer,
        tick: i64,
        cancel: &CancellationToken,
    ) -> Result<Parts, TemporalError> {
        let origin = l.content.origin();
        let mut anchor = l.content.bounds().map_or([0.0, 0.0], |b| {
            [f64::from(b.width) / 2.0, f64::from(b.height) / 2.0]
        });
        if let Some(v) = sample(&l.layer_id, l.streams.anchor.as_ref(), 2, tick, cancel)? {
            anchor = [v[0], v[1]];
        }
        let position = match sample(&l.layer_id, l.streams.position.as_ref(), 2, tick, cancel)? {
            Some(v) => [v[0], v[1]],
            None => [origin[0] + anchor[0], origin[1] + anchor[1]],
        };
        let scale = match sample(&l.layer_id, l.streams.scale.as_ref(), 2, tick, cancel)? {
            Some(v) => [v[0] / 100.0, v[1] / 100.0],
            None => [1.0, 1.0],
        };
        let rotation = match sample(&l.layer_id, l.streams.rotation.as_ref(), 1, tick, cancel)? {
            Some(v) => v[0],
            None => 0.0,
        };
        Ok(Parts {
            anchor,
            position,
            scale,
            rotation,
        })
    }

    /// Layer space -> the parent's layer space (composition pixels without a parent) at `tick`:
    /// `position + R(rotation) S(scale) (x - anchor)`.
    fn local(l: &CompLayer, tick: i64, cancel: &CancellationToken) -> Result<Affine2, TemporalError> {
        let Parts {
            anchor,
            position,
            scale,
            rotation,
        } = Self::transform_parts(l, tick, cancel)?;
        Ok(Affine2::from_parts(
            anchor[0],
            anchor[1],
            scale[0],
            scale[1],
            rotation,
            position[0] - anchor[0],
            position[1] - anchor[1],
        ))
    }

    /// Content plan coordinates -> composition pixels of one layer at `tick`: the layer's own
    /// transform, then every parent's (STU-MOT-008: the child's transform is evaluated in the
    /// parent's space; parenting affects the transform group only). Parents act at every tick,
    /// inside or outside their own window and whether or not they are visible (After Effects
    /// behaviour, UNVERIFIED). Folio refuses parent loops; the walk is bounded by the layer count
    /// anyway (`Cycle`).
    fn matrix(
        comp: &Comp,
        l: &CompLayer,
        tick: i64,
        cancel: &CancellationToken,
    ) -> Result<Affine2, TemporalError> {
        let origin = l.content.origin();
        let mut m = Affine2::translate(-origin[0], -origin[1]).then(Self::local(l, tick, cancel)?);
        let mut next = l.parent.as_deref();
        let mut steps = 0usize;
        while let Some(pid) = next {
            steps = steps.checked_add(1).ok_or(CompositeError::Overflow)?;
            if steps > comp.layers.len() {
                return Err(CompositeError::Cycle(l.layer_id.clone()).into());
            }
            let parent = comp
                .layers
                .iter()
                .find(|p| p.layer_id == pid)
                .ok_or_else(|| CompositeError::DanglingInput(pid.to_owned()))?;
            m = m.then(Self::local(parent, tick, cancel)?);
            next = parent.parent.as_deref();
        }
        Ok(m)
    }

    /// Source time of layer `layer_id` at composition tick `tick` (STU-MOT-006 `start_time` and
    /// rational `stretch`, negative reverses): `floor((tick - start_time) / stretch)`, the same
    /// rule as Folio's `StudioLayerTiming::source_tick` (floor rounding is a Studio reading, After
    /// Effects UNVERIFIED), extended to negative ticks; a result outside `i64` is `Overflow`. A
    /// precomposition layer samples its nested composition at this tick; a still raster layer's
    /// pixels do not depend on it.
    pub fn source_tick(&self, layer_id: &str, tick: i64) -> Result<i64, TemporalError> {
        let l = self
            .find(layer_id)
            .ok_or_else(|| CompositeError::DanglingInput(layer_id.to_owned()))?;
        map_source(&l.timing, tick).ok_or_else(|| CompositeError::Overflow.into())
    }

    /// The composition evaluated at composition tick `tick` (module docs).
    pub fn scene_at(&self, tick: i64, cancel: &CancellationToken) -> Result<SceneComposition, TemporalError> {
        self.scene_of(self.root()?, tick, 0, cancel)
    }

    fn scene_of(
        &self,
        comp: &Comp,
        tick: i64,
        depth: usize,
        cancel: &CancellationToken,
    ) -> Result<SceneComposition, TemporalError> {
        // Matte sources do not render on their own (STU-CMP-030 default).
        let matte_sources: std::collections::BTreeSet<&str> = comp
            .layers
            .iter()
            .filter_map(|l| l.matte.as_ref().map(|m| m.source_layer_id.as_str()))
            .collect();
        let mut layers = Vec::new();
        for l in comp.layers.iter().rev() {
            cancel.check().map_err(|_| TemporalError::Composite(CompositeError::Canceled))?;
            if !l.visible || matte_sources.contains(l.layer_id.as_str()) {
                continue;
            }
            let Some(layer) = self.layer_at(comp, l, tick, depth, 0, cancel)? else {
                continue;
            };
            let collapsed = matches!(l.content, Content::Precomp { collapse: true, .. });
            match (collapsed, layer.content) {
                (true, SceneContent::Precomp(nested)) => {
                    // Collapse transformations: the nested layers render directly in this
                    // composition, each with its own matrix followed by the precomposition
                    // layer's, its opacity times the layer's; no nested surface, no clip to the
                    // nested rect (After Effects behaviour as Studio reads it, UNVERIFIED).
                    if layer.mode != StudioBlendMode::Normal {
                        return Err(unsupported(&layer.layer_id, COMPOSITION_COLLAPSE_BLEND));
                    }
                    if layer.matte.is_some() {
                        return Err(unsupported(&layer.layer_id, COMPOSITION_COLLAPSE_MATTE));
                    }
                    for nl in nested.layers {
                        layers.push(collapse_into(nl, &layer.matrix, layer.opacity));
                    }
                }
                (_, content) => layers.push(SceneLayer { content, ..layer }),
            }
        }
        Ok(SceneComposition {
            composition_id: comp.composition_id.clone(),
            width: comp.width,
            height: comp.height,
            background: comp.background,
            filter: self.filter,
            layers,
        })
    }

    /// One layer at `tick` (`None` outside its window), with its precomposition and matte chain.
    fn layer_at(
        &self,
        comp: &Comp,
        l: &CompLayer,
        tick: i64,
        depth: usize,
        matte_depth: usize,
        cancel: &CancellationToken,
    ) -> Result<Option<SceneLayer>, TemporalError> {
        let exists = u64::try_from(tick).is_ok_and(|t| l.timing.contains(t));
        if !exists {
            return Ok(None);
        }
        let opacity_scale = Self::opacity_scale(l, tick, cancel)?;
        let opacity = l.opacity * opacity_scale;
        let content = match &l.content {
            Content::None => SceneContent::None,
            Content::Tiles { tiles, .. } => SceneContent::Tiles(tiles.clone()),
            Content::Static { content, .. } => SceneContent::Static(content.clone()),
            Content::Adjustment(op) => SceneContent::Adjustment(op.clone()),
            Content::Precomp { composition_id, .. } => {
                let nested = self
                    .comps
                    .get(composition_id)
                    .ok_or_else(|| CompositeError::DanglingInput(composition_id.clone()))?;
                if depth >= MAX_PRECOMP_DEPTH {
                    return Err(CompositeError::BudgetExceeded.into());
                }
                let source = map_source(&l.timing, tick).ok_or(CompositeError::Overflow)?;
                let inside = u64::try_from(source).is_ok_and(|t| t < nested.duration);
                let mut scene = if inside {
                    let depth = depth.checked_add(1).ok_or(CompositeError::Overflow)?;
                    self.scene_of(nested, source, depth, cancel)?
                } else {
                    SceneComposition {
                        composition_id: nested.composition_id.clone(),
                        width: nested.width,
                        height: nested.height,
                        background: None,
                        filter: self.filter,
                        layers: Vec::new(),
                    }
                };
                scene.background = None;
                SceneContent::Precomp(Box::new(scene))
            }
        };
        let unlinked = match &l.content {
            Content::Static { unlinked: Some(u), .. } => Some(u),
            _ => None,
        };
        if unlinked.is_some() && l.matte.is_some() {
            return Err(unsupported(&l.layer_id, crate::vector_mask::REFUSE_MASK_UNLINKED_MATTE));
        }
        let matte = match &l.matte {
            None => unlinked.map(|u| (**u).clone()),
            Some(m) => {
                // Folio refuses matte loops; the chain is bounded by the layer count anyway.
                if matte_depth >= comp.layers.len() {
                    return Err(CompositeError::Cycle(l.layer_id.clone()).into());
                }
                let source = comp
                    .layers
                    .iter()
                    .find(|o| o.layer_id == m.source_layer_id)
                    .ok_or_else(|| CompositeError::DanglingInput(m.source_layer_id.clone()))?;
                let next = matte_depth.checked_add(1).ok_or(CompositeError::Overflow)?;
                let layer = self
                    .layer_at(comp, source, tick, depth, next, cancel)?
                    .map(Box::new);
                Some(SceneMatte {
                    layer,
                    mode: match m.mode {
                        StudioMatteMode::Alpha => SceneMatteMode::Alpha,
                        StudioMatteMode::Luma => SceneMatteMode::Luma,
                    },
                    inverted: m.inverted,
                })
            }
        };
        Ok(Some(SceneLayer {
            layer_id: l.layer_id.clone(),
            content,
            mode: l.mode,
            opacity,
            fill_opacity: l.fill_opacity,
            matrix: Self::matrix(comp, l, tick, cancel)?,
            matte,
            clipped: l.clipped,
        }))
    }
}

impl TemporalSource for CompositionTemporalSource {
    /// The layer's state for `lower_frame` (the document graph supplies the stack): opacity stream
    /// and `translate_px = position - (content origin + anchor)` with [`Self::layer_anchor`] as the
    /// pivot, which is the same matrix as [`CompositionTemporalSource::scene_at`].
    fn layer_state(
        &self,
        layer_id: &str,
        tick: i64,
        cancel: &CancellationToken,
    ) -> Result<Option<LayerFrameState>, TemporalError> {
        let Some(l) = self.graph_layer(layer_id)? else {
            return Ok(None);
        };
        let Parts {
            anchor,
            position,
            scale,
            rotation: rotation_deg,
        } = Self::transform_parts(l, tick, cancel)?;
        let origin = l.content.origin();
        let translate_px = [
            position[0] - (origin[0] + anchor[0]),
            position[1] - (origin[1] + anchor[1]),
        ];
        Ok(Some(LayerFrameState {
            visible: true,
            opacity_scale: Self::opacity_scale(l, tick, cancel)?,
            translate_px,
            scale,
            rotation_deg,
        }))
    }

    fn resample_filter(&self, layer_id: &str) -> Option<ResampleFilter> {
        self.find(layer_id).map(|_| self.filter)
    }

    /// The scene matrix (own transform and parent chain), so `lower_frame` places the layer
    /// exactly as [`CompositionTemporalSource::scene_at`] does.
    fn layer_matrix(
        &self,
        layer_id: &str,
        tick: i64,
        cancel: &CancellationToken,
    ) -> Result<Option<Affine2>, TemporalError> {
        let Some(l) = self.graph_layer(layer_id)? else {
            return Ok(None);
        };
        Self::matrix(self.root()?, l, tick, cancel).map(Some)
    }

    fn layer_anchor(
        &self,
        layer_id: &str,
        tick: i64,
        cancel: &CancellationToken,
    ) -> Result<Option<[f64; 2]>, TemporalError> {
        let Some(l) = self.find(layer_id) else {
            return Ok(None);
        };
        let anchor = Self::transform_parts(l, tick, cancel)?.anchor;
        let origin = l.content.origin();
        Ok(Some([origin[0] + anchor[0], origin[1] + anchor[1]]))
    }
}

// ---- Document-graph path with precompositions and mattes (SMB-COMPOSITE-COMPOSITION-2 slice 4) ----

/// Object key of the synthetic tile that stands for a rendered precomposition in a frame document.
pub const PRECOMP_TILE_KEY: &str = "composite-precomp";
/// Sample-format token of that synthetic tile (resolved only by composite's own wrappers).
pub const PRECOMP_TILE_FORMAT: &str = "composite-surface-rgb_f32le";
pub const COMPOSITION_COLLAPSE_ON_DOCUMENT_GRAPH: &str = "composition_collapse_on_document_graph";
pub const COMPOSITION_MATTE_WITH_MASK: &str = "composition_matte_with_mask";
pub const COMPOSITION_MATTE_TARGET: &str = "composition_matte_target_not_found";
pub const COMPOSITION_PRECOMP_GRAPH: &str = "composition_precomp_graph_operation";

/// A document-graph frame with the surfaces and matte planes composite rendered for it.
#[derive(Clone, Debug, PartialEq)]
pub struct DocumentFrame {
    pub frame: FrameLowering,
    /// Plan keys of the synthetic precomposition tiles -> the rendered nested compositions.
    pub surfaces: BTreeMap<SourceKey, Surface>,
    /// Plan keys of the track-matte masks -> their coverage planes.
    pub mattes: BTreeMap<SourceKey, crate::composition::MattePlane>,
    /// Plan keys of the coverage planes composite owns for this frame (vector clips, vector
    /// masks, vector and stroke coverage, text coverage; lane SMB-COMPOSITE-DOCGRAPH-MASKS), at
    /// their plan placement.
    pub owned_masks: BTreeMap<SourceKey, OwnedMask>,
    /// Plan keys of the painted vector row tiles composite owns for this frame.
    pub owned_tiles: BTreeMap<SourceKey, Surface>,
}

impl DocumentFrame {
    /// `PlanSources` serving the owned surfaces, mattes and planes first, then the caller's
    /// sources (raster tiles and raster masks by Folio record, image paint tiles by the source id
    /// in `frame.tables.image_sources`).
    pub fn sources<'a>(&'a self, caller: &'a dyn PlanSources) -> DocumentSources<'a> {
        DocumentSources { frame: self, caller }
    }
}

/// See [`DocumentFrame::sources`].
pub struct DocumentSources<'a> {
    frame: &'a DocumentFrame,
    caller: &'a dyn PlanSources,
}

impl PlanSources for DocumentSources<'_> {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        if let Some(s) = self.frame.surfaces.get(&key).or_else(|| self.frame.owned_tiles.get(&key)) {
            return Some(TilePlanes {
                width: s.width,
                height: s.height,
                colour: &s.colour,
                colour_stride_bytes: u64::from(s.width).checked_mul(12)?,
                alpha: &s.alpha,
                alpha_stride_bytes: u64::from(s.width).checked_mul(4)?,
                colour_sha256: s.colour_sha256,
                alpha_sha256: s.alpha_sha256,
                format: SampleFormat::RgbF32Le,
                alpha_convention: AlphaConvention::Straight,
                space: self.frame.frame.lowering.plan.target.space,
            });
        }
        self.caller.tile(key)
    }
    fn mask(&self, key: SourceKey) -> Option<Mask<'_>> {
        if let Some(m) = self.frame.mattes.get(&key) {
            return Some(Mask {
                bounds: Rect { x: 0, y: 0, width: m.width, height: m.height },
                sample_format: "coverage_u16le",
                bytes: &m.bytes,
                stride_bytes: u64::from(m.width).checked_mul(2)?,
                sha256: m.sha256,
            });
        }
        if let Some(m) = self.frame.owned_masks.get(&key) {
            return Some(Mask {
                bounds: m.bounds,
                sample_format: &m.sample_format,
                bytes: &m.bytes,
                stride_bytes: m.stride_bytes,
                sha256: m.sha256,
            });
        }
        self.caller.mask(key)
    }
}

/// Resolver of a frame document: the synthetic precomposition tiles are available.
struct FrameResolver<'a> {
    inner: &'a dyn Resolver,
}
impl Resolver for FrameResolver<'_> {
    fn primitive(&self, schema: &str, id: &str) -> hsk_studio_folio::Resolution {
        self.inner.primitive(schema, id)
    }
    fn profile(&self, id: &str) -> hsk_studio_folio::Resolution {
        self.inner.profile(id)
    }
    fn tile(&self, t: &TileRef) -> hsk_studio_folio::Resolution {
        if t.object_key == PRECOMP_TILE_KEY && t.format == PRECOMP_TILE_FORMAT {
            return hsk_studio_folio::Resolution::Available;
        }
        self.inner.tile(t)
    }
    fn format_supported(&self, format: &str) -> bool {
        format == PRECOMP_TILE_FORMAT || self.inner.format_supported(format)
    }
    fn primitive_layer_binding(&self, kind: &str, schema: &str) -> bool {
        self.inner.primitive_layer_binding(kind, schema)
    }
}

/// Tile info of a frame document: the rendered surfaces for the synthetic tiles.
struct FrameInfo<'a> {
    inner: &'a dyn TileInfoSource,
    surfaces: &'a BTreeMap<String, Surface>,
}
impl TileInfoSource for FrameInfo<'_> {
    fn tile_info(&self, t: &TileRef) -> Option<crate::TileInfo> {
        if t.object_key == PRECOMP_TILE_KEY && t.format == PRECOMP_TILE_FORMAT {
            let s = self.surfaces.get(&t.layer_id)?;
            return Some(crate::TileInfo {
                format: SampleFormat::RgbF32Le,
                alpha: AlphaConvention::Straight,
                colour_sha256: s.colour_sha256,
                alpha_sha256: s.alpha_sha256,
            });
        }
        self.inner.tile_info(t)
    }
    fn mask_sha256(&self, tiles: &[TileRef]) -> Option<[u8; 32]> {
        self.inner.mask_sha256(tiles)
    }
}

/// The temporal source of the document-graph path with precompositions and mattes rendered by
/// composite: precomposition and matted layers are allowed, matte sources never render.
struct DocumentGraphSource<'a> {
    inner: &'a CompositionTemporalSource,
}
impl TemporalSource for DocumentGraphSource<'_> {
    fn layer_state(
        &self,
        layer_id: &str,
        tick: i64,
        cancel: &CancellationToken,
    ) -> Result<Option<LayerFrameState>, TemporalError> {
        let Some(l) = self.inner.find(layer_id) else {
            return Ok(None);
        };
        let matte_source = self.inner.root()?.layers.iter().any(|o| {
            o.matte
                .as_ref()
                .is_some_and(|m| m.source_layer_id == layer_id)
        });
        Ok(Some(LayerFrameState {
            visible: !matte_source,
            opacity_scale: CompositionTemporalSource::opacity_scale(l, tick, cancel)?,
            ..LayerFrameState::IDENTITY
        }))
    }
    fn resample_filter(&self, layer_id: &str) -> Option<ResampleFilter> {
        self.inner.find(layer_id).map(|_| self.inner.filter)
    }
    fn layer_matrix(
        &self,
        layer_id: &str,
        tick: i64,
        cancel: &CancellationToken,
    ) -> Result<Option<Affine2>, TemporalError> {
        let Some(l) = self.inner.find(layer_id) else {
            return Ok(None);
        };
        CompositionTemporalSource::matrix(self.inner.root()?, l, tick, cancel).map(Some)
    }
}

impl CompositionTemporalSource {
    /// Frame `tick` over the DOCUMENT graph (`lower_frame`) with this composition's
    /// precompositions and track mattes, which the graph alone cannot express:
    ///
    /// - A precomposition layer (a content-less Folio `group` layer whose graph operation is a
    ///   composite with no inputs, or a source) is rendered by `ctx` from its nested composition at
    ///   its source tick ([`CompositionTemporalSource::scene_at`]) into a surface; the frame document
    ///   gives the layer one synthetic raster tile ([`PRECOMP_TILE_KEY`], format
    ///   [`PRECOMP_TILE_FORMAT`], the nested size at `(0, 0)`) and a source operation with the same
    ///   output port, so the static path places it with the layer's matrix, opacity and appearance.
    ///   Collapsed precompositions are refused here
    ///   ([`COMPOSITION_COLLAPSE_ON_DOCUMENT_GRAPH`]; the scene path expands them).
    /// - A track matte: the matte layer is rendered alone by `ctx` (as the scene path does) into a
    ///   `coverage_u16le` plane over the composition rect, set as the mask of the plan layer that
    ///   carries the matted layer's tiles (plan space; [`COMPOSITION_MATTE_WITH_MASK`] when that
    ///   layer already has a mask, [`COMPOSITION_MATTE_TARGET`] when it lowers to no tile entry).
    ///   Matte sources never render on their own.
    ///
    /// - Vector clips, vector masks, clipped layers, vector and text layers (lane
    ///   SMB-COMPOSITE-DOCGRAPH-MASKS): the frame is lowered with `inputs`
    ///   (`super::lower_frame_with_inputs`), the same static machinery the scene path's static
    ///   subtrees use, so a clip or linked mask moves with its layer and an unlinked mask stays in
    ///   composition (plan) space; the planes are owned by the [`DocumentFrame`].
    ///
    /// The plan is validated again by pigment; execute it with `execute_extended`, the frame's
    /// envelope and [`DocumentFrame::sources`] over the caller's sources. `req.snapshot` and
    /// `inputs` must be the snapshot and inputs the source was read with
    /// ([`CompositionInput::inputs`]).
    pub fn lower_document_frame(
        &self,
        req: &FrameRequest<'_>,
        inputs: &RenderInputs<'_>,
        tick: i64,
        ctx: &SceneContext<'_>,
        cancel: &CancellationToken,
    ) -> Result<DocumentFrame, TemporalError> {
        let root = self.root()?;
        let scene = self.scene_at(tick, cancel)?;
        let doc = req.snapshot.document();
        let mut frame_doc = doc.clone();
        let mut surfaces: BTreeMap<String, Surface> = BTreeMap::new();
        let mut changed = false;
        for l in &root.layers {
            let Content::Precomp { collapse, bounds, .. } = &l.content else {
                continue;
            };
            if *collapse {
                return Err(unsupported(&l.layer_id, COMPOSITION_COLLAPSE_ON_DOCUMENT_GRAPH));
            }
            if let Some(SceneContent::Precomp(nested)) = scene
                .layers
                .iter()
                .find(|s| s.layer_id == l.layer_id)
                .map(|s| &s.content)
            {
                let r = render_scene(nested, ctx, cancel)?;
                surfaces.insert(
                    l.layer_id.clone(),
                    Surface {
                        colour_sha256: hsk_studio_prism::profile_hash(&r.colour),
                        alpha_sha256: hsk_studio_prism::profile_hash(&r.alpha),
                        width: r.width,
                        height: r.height,
                        colour: r.colour,
                        alpha: r.alpha,
                    },
                );
            }
            let profile = match &doc.blending_space {
                ProfileBinding::Assigned(p) => p.clone(),
                ProfileBinding::Unassigned(()) => {
                    return Err(unsupported(&l.layer_id, COMPOSITION_PRECOMP_GRAPH));
                }
            };
            let layer = frame_doc
                .layers
                .iter_mut()
                .find(|d| d.layer_id == l.layer_id)
                .ok_or_else(|| CompositeError::DanglingInput(l.layer_id.clone()))?;
            let digest = surfaces
                .get(&l.layer_id)
                .map_or_else(|| "0".repeat(64), |s| hsk_studio_prism::hash_hex(&s.colour_sha256));
            layer.kind = "raster".to_owned();
            layer.payload = Payload::Raster {
                tiles: vec![TileRef {
                    object_key: PRECOMP_TILE_KEY.to_owned(),
                    layer_id: l.layer_id.clone(),
                    column: 0,
                    row: 0,
                    width: hsk_studio_folio::Length {
                        value: f64::from(bounds.width),
                        unit: hsk_studio_folio::GeometryUnit::Px,
                    },
                    height: hsk_studio_folio::Length {
                        value: f64::from(bounds.height),
                        unit: hsk_studio_folio::GeometryUnit::Px,
                    },
                    format: PRECOMP_TILE_FORMAT.to_owned(),
                    colour_profile_id: profile,
                    schema_id: hsk_studio_folio::TileSchema::V1,
                    content_digest: hsk_studio_folio::ContentDigest {
                        algorithm: "sha256".to_owned(),
                        digest,
                    },
                    artifact_manifest_id: "composite.precomp".to_owned(),
                }],
            };
            let op = frame_doc
                .graph
                .operations
                .iter_mut()
                .find(|o| o.layer_id == l.layer_id)
                .ok_or_else(|| unsupported(&l.layer_id, COMPOSITION_PRECOMP_GRAPH))?;
            let has_inputs = frame_doc
                .graph
                .edges
                .iter()
                .any(|e| e.target.layer_id == l.layer_id);
            if has_inputs || !matches!(op.outputs.as_slice(), [p] if p.port_key == "out") {
                return Err(unsupported(&l.layer_id, COMPOSITION_PRECOMP_GRAPH));
            }
            // The one source port Folio admits for a raster layer (same `out` key, so the edges
            // that read it stay valid).
            op.operation_kind = hsk_studio_folio::OperationKind::Source;
            op.inputs = Vec::new();
            op.outputs = vec![hsk_studio_folio::Port {
                port_key: "out".to_owned(),
                value_type: hsk_studio_folio::ValueType::Image,
                cardinality: hsk_studio_folio::Cardinality::One,
                semantic: hsk_studio_folio::Semantic::Source,
            }];
            changed = true;
        }
        let resolver = FrameResolver {
            inner: req.resolver,
        };
        let owned;
        let snapshot = if changed {
            owned = validate_document(frame_doc, req.budget, cancel, &resolver)
                .map_err(|e| TemporalError::Document(format!("{:?}:{}", e.code, e.target)))?;
            &owned
        } else {
            req.snapshot
        };
        let info = FrameInfo {
            inner: req.info,
            surfaces: &surfaces,
        };
        let frame_req = FrameRequest {
            snapshot,
            resolver: &resolver,
            info: &info,
            ..*req
        };
        let mut frame = lower_frame_with_inputs(
            &frame_req,
            inputs,
            &DocumentGraphSource { inner: self },
            tick,
            cancel,
        )?;
        let (owned_masks, owned_tiles) = own_planes(&frame, inputs)?;
        let mut out_surfaces = BTreeMap::new();
        for (key, t) in &frame.lowering.sources {
            if t.object_key == PRECOMP_TILE_KEY
                && t.format == PRECOMP_TILE_FORMAT
                && let Some(s) = surfaces.get(&t.layer_id)
            {
                out_surfaces.insert(*key, s.clone());
            }
        }
        // Track mattes.
        let mut mattes = BTreeMap::new();
        let mut next_key = next_free_key(&frame.lowering.plan)?;
        for s in &scene.layers {
            let Some(matte) = &s.matte else { continue };
            // A scene matte without a track-matte record is an unlinked vector mask, which this
            // path lowered in place (plan space) with the inputs.
            if root.layers.iter().any(|c| c.layer_id == s.layer_id && c.matte.is_none()) {
                continue;
            }
            let plane = crate::composition::matte_coverage(&scene, &s.layer_id, matte, ctx, cancel)?;
            let owner: BTreeSet<SourceKey> = frame
                .lowering
                .sources
                .iter()
                .filter(|(_, t)| t.layer_id == s.layer_id)
                .map(|(k, _)| *k)
                .collect();
            let entry = matte_entry(&frame.lowering.plan, &owner)
                .ok_or_else(|| unsupported(&s.layer_id, COMPOSITION_MATTE_TARGET))?;
            let key = SourceKey(next_key);
            next_key = next_key.checked_add(1).ok_or(CompositeError::Overflow)?;
            let (mut g, li) = entry;
            let existing = match frame.lowering.plan.nodes.get(g) {
                Some(PlanNode::Group(group)) => group.layers.get(li).and_then(|l| l.mask),
                _ => return Err(CompositeError::Overflow.into()),
            };
            if let Some(mask) = existing {
                // The layer's own coverage mask (a vector clip or mask, a raster mask) moves into
                // an isolated wrapper so the matte can take the slot (lane
                // SMB-COMPOSITE-DOCGRAPH-MASKS), as the scene path cuts the static subtree before
                // the matte. A clip-to-base route or an envelope entry beyond the mask feather
                // (a transform, effects) stays refused.
                g = wrap_masked_entry(&mut frame, g, li, mask)
                    .ok_or_else(|| unsupported(&s.layer_id, COMPOSITION_MATTE_WITH_MASK))?;
            }
            let Some(PlanNode::Group(group)) = frame.lowering.plan.nodes.get_mut(g) else {
                return Err(CompositeError::Overflow.into());
            };
            let Some(layer) = group.layers.get_mut(li) else {
                return Err(CompositeError::Overflow.into());
            };
            layer.mask = Some(MaskInput {
                key,
                placement: Rect { x: 0, y: 0, width: plane.width, height: plane.height },
                format: MaskFormat::CoverageU16Le,
                sha256: plane.sha256,
                inverted: matte.inverted,
                density: 1.0,
            });
            frame
                .lowering
                .exactness
                .record(crate::composition::TRACK_MATTE_KEY, hsk_studio_pigment::blend_math::Exactness::Approximate);
            mattes.insert(key, plane);
        }
        if !mattes.is_empty() {
            let ops = frame
                .lowering
                .plan
                .validate(&PlanLimits::default())
                .map_err(CompositeError::Plan)?;
            for op in ops {
                frame.lowering.exactness.record(op.key(), op.intrinsic());
            }
            if !frame.extensions.is_empty() {
                frame
                    .extensions
                    .validate(&frame.lowering.plan)
                    .map_err(CompositeError::Plan)?;
            }
        }
        Ok(DocumentFrame {
            frame,
            surfaces: out_surfaces,
            mattes,
            owned_masks,
            owned_tiles,
        })
    }
}

/// Owned coverage planes and painted tiles of one document frame (see [`own_planes`]).
type OwnedPlanes = (BTreeMap<SourceKey, OwnedMask>, BTreeMap<SourceKey, Surface>);

/// The coverage planes and painted tiles a frame lowered with `inputs` reads, owned (as the scene
/// path's `static_content` owns them): vector, stroke, clip and vector-mask planes at their plan
/// placement, text coverage by layer.
fn own_planes(
    frame: &FrameLowering,
    inputs: &RenderInputs<'_>,
) -> Result<OwnedPlanes, TemporalError> {
    let own = |m: Mask<'_>| OwnedMask {
        bounds: m.bounds,
        sample_format: m.sample_format.to_owned(),
        bytes: m.bytes.to_vec(),
        stride_bytes: m.stride_bytes,
        sha256: m.sha256,
    };
    let mut masks = BTreeMap::new();
    let mut tiles = BTreeMap::new();
    let t = &frame.tables;
    for (key, layer) in &t.text_masks {
        let m = inputs
            .text
            .and_then(|p| p.plane(layer))
            .and_then(|p| p.mask())
            .ok_or_else(|| CompositeError::Unavailable(format!("{layer}:text_layer_not_prepared")))?;
        masks.insert(*key, own(m));
    }
    if let Some(vectors) = inputs.vectors {
        let keys = t
            .vector_masks
            .iter()
            .map(|(k, _)| *k)
            .chain(t.vector_rows.iter().map(|(k, _, _)| *k))
            .chain(t.clip_planes.iter().map(|(k, _)| *k));
        for key in keys {
            if let Some(m) = frame.vector_mask(key, vectors) {
                masks.insert(key, own(m));
            } else if let Some(p) = frame.vector_paint(key, vectors) {
                tiles.insert(
                    key,
                    Surface {
                        width: p.width,
                        height: p.height,
                        colour: p.colour.to_vec(),
                        alpha: p.alpha.to_vec(),
                        colour_sha256: p.colour_sha256,
                        alpha_sha256: p.alpha_sha256,
                    },
                );
            }
        }
    }
    Ok((masks, tiles))
}

/// Moves the coverage mask of group `g`'s layer `li` into a new isolated wrapper group
/// `Group[Layer{content, Normal, 1, 1, mask}]` inserted before `g` (the plan stays index-ordered:
/// every node id at or after the insertion point, the root and the envelope's group ids shift by
/// one), the layer then reads the wrapper with no mask; a mask feather entry moves to the wrapper's
/// layer. Returns the new index of `g`, or `None` for a clip-to-base route or an envelope entry of
/// that layer with anything besides the mask feather (a transform or effects act on the masked
/// content and cannot be split from it here).
fn wrap_masked_entry(frame: &mut FrameLowering, g: usize, li: usize, mask: MaskInput) -> Option<usize> {
    let g32 = u32::try_from(g).ok()?;
    let li32 = u32::try_from(li).ok()?;
    let entry = frame
        .extensions
        .layers
        .iter()
        .position(|e| e.group == PlanNodeId(g32) && e.layer == li32);
    if mask.is_clip_to_base() {
        return None;
    }
    if let Some(i) = entry {
        let e = frame.extensions.layers.get(i)?;
        let feather_only = LayerExtension::new(e.group, e.layer).with_mask_feather(e.mask_feather_px);
        if *e != feather_only {
            return None;
        }
    }
    let shift = |id: &mut PlanNodeId| -> Option<()> {
        if id.0 >= g32 {
            id.0 = id.0.checked_add(1)?;
        }
        Some(())
    };
    let plan = &mut frame.lowering.plan;
    let content = match plan.nodes.get(g) {
        Some(PlanNode::Group(group)) => group.layers.get(li)?.input,
        _ => return None,
    };
    for node in &mut plan.nodes {
        if let PlanNode::Group(group) = node {
            for l in &mut group.layers {
                shift(&mut l.input)?;
            }
        }
    }
    shift(&mut plan.root)?;
    for e in &mut frame.extensions.layers {
        shift(&mut e.group)?;
    }
    plan.nodes.insert(
        g,
        PlanNode::Group(Group {
            isolated: true,
            layers: vec![Layer {
                input: content,
                mode: StudioBlendMode::Normal,
                opacity: 1.0,
                fill_opacity: 1.0,
                mask: Some(mask),
            }],
        }),
    );
    let parent = g.checked_add(1)?;
    if let Some(PlanNode::Group(group)) = plan.nodes.get_mut(parent)
        && let Some(l) = group.layers.get_mut(li)
    {
        l.input = PlanNodeId(g32);
        l.mask = None;
    }
    if let Some(i) = entry
        && let Some(e) = frame.extensions.layers.get_mut(i)
    {
        e.group = PlanNodeId(g32);
        e.layer = 0;
    }
    Some(parent)
}

/// One past the largest source key the plan uses (tiles and masks).
fn next_free_key(plan: &hsk_studio_pigment::render_plan::RenderPlan) -> Result<u32, TemporalError> {
    let mut max: Option<u32> = None;
    for n in &plan.nodes {
        match n {
            PlanNode::Tile(t) => max = max.max(Some(t.key.0)),
            PlanNode::Group(g) => {
                for l in &g.layers {
                    if let Some(m) = l.mask {
                        max = max.max(Some(m.key.0));
                    }
                }
            }
            _ => {}
        }
    }
    match max {
        None => Ok(0),
        Some(m) => m.checked_add(1).ok_or_else(|| CompositeError::Overflow.into()),
    }
}

/// The (group node, layer index) whose input is the matted layer's content: its single tile node,
/// or an isolated group made only of its tiles.
fn matte_entry(
    plan: &hsk_studio_pigment::render_plan::RenderPlan,
    owner: &BTreeSet<SourceKey>,
) -> Option<(usize, usize)> {
    let is_owned = |id: usize| -> bool {
        match plan.nodes.get(id) {
            Some(PlanNode::Tile(t)) => owner.contains(&t.key),
            Some(PlanNode::Group(g)) => {
                !g.layers.is_empty()
                    && g.layers.iter().all(|l| {
                        matches!(plan.nodes.get(l.input.0 as usize), Some(PlanNode::Tile(t)) if owner.contains(&t.key))
                    })
            }
            _ => false,
        }
    };
    if owner.is_empty() {
        return None;
    }
    for (g, n) in plan.nodes.iter().enumerate() {
        let PlanNode::Group(group) = n else { continue };
        for (li, l) in group.layers.iter().enumerate() {
            if is_owned(l.input.0 as usize) {
                return Some((g, li));
            }
        }
    }
    None
}
