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
use crate::{
    Limits, NodeAddr, RenderInputs, StepStatus, evaluate_with_inputs, lower, lower_with_inputs,
};
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
    Static { content: Box<StaticContent>, bounds: Rect },
    /// An adjustment layer.
    Adjustment(AdjustmentOp),
    /// A nested composition; `bounds` = `(0, 0, width, height)` of it.
    Precomp { composition_id: String, bounds: Rect },
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
) -> Result<Option<(StaticContent, Rect)>, TemporalError> {
    let doc = input.snapshot.document();
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
    }
    let snap = validate_document(frame, input.budget, cancel, input.resolver)
        .map_err(|e| TemporalError::Document(format!("{:?}:{}", e.code, e.target)))?;
    let plan = lower(&snap, doc.revision, input.space.blend_space, &Limits::default(), cancel)?;
    let report = evaluate_with_inputs(&plan, &snap, &input.inputs, cancel)?;
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
    let low = lower_with_inputs(&plan, &report, &snap, &input.inputs, &root, target, input.info)?;
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
    if let Some(vectors) = input.inputs.vectors {
        let keys = low
            .vector_masks
            .iter()
            .map(|(k, _)| *k)
            .chain(low.vector_rows.iter().map(|(k, _, _)| *k));
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
    Ok(Some((content, bounds)))
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
                (Some(p), _) if p.collapse_transformations => {
                    return Err(unsupported(id, COMPOSITION_PRECOMP_COLLAPSE));
                }
                (Some(p), Payload::Group) => {
                    let nested = input
                        .records
                        .composition(&p.composition_id)
                        .ok_or_else(|| CompositeError::DanglingInput(p.composition_id.clone()))?;
                    Content::Precomp {
                        composition_id: p.composition_id.clone(),
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
                        Some((content, bounds)) => Content::Static {
                            content: Box::new(content),
                            bounds,
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
            if a.clipped {
                return Err(unsupported(id, COMPOSITION_LAYER_CLIPPED));
            }
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
            if let Some(layer) = self.layer_at(comp, l, tick, depth, 0, cancel)? {
                layers.push(layer);
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
        let matte = match &l.matte {
            None => None,
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
