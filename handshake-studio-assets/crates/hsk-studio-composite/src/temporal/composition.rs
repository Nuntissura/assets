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
//! [`CompositionTemporalSource::scene_at`] returns the folio-free [`SceneComposition`] that
//! `composition::lower_scene` / `render_scene` lower and render. The type also implements
//! [`TemporalSource`] for `lower_frame` over a document whose graph mirrors the composition stack.
//!
//! Refused typed (never ignored): records Folio refuses (`Document`), an unknown composition id
//! (`DanglingInput`), a stale revision, layer content other than raster tiles or a null layer
//! (`composition_layer_content`; a null layer is a Folio `group` layer with a `group` payload, since Folio binds no `null` kind), clipped or masked layers (`composition_layer_clipped`,
//! `composition_layer_mask`), a pass-through blend on a content layer, sampled opacity outside
//! `0..=100` (`InvalidState`), precompositions (`composition_precomp`) and track mattes
//! (`composition_track_matte`) until their slices land. Not modelled: 3D layers, motion blur,
//! expressions, frame blending, time remapping (Folio stores none of them).
#![deny(clippy::arithmetic_side_effects)]

use super::motion::sample;
use super::{LayerFrameState, LayerTiming, TemporalError, TemporalSource};
use crate::composition::{SceneComposition, SceneContent, SceneLayer, SceneTile};
use crate::evaluate_static::tile_rects;
use crate::{CompositeError, TileInfoSource};
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{
    Budget, Payload, ProfileBinding, Resolver, Snapshot, StudioComposition,
    StudioCompositionRecords, StudioInterpolation, StudioLayerProperty, StudioLayerTiming,
    StudioPropertyStream, StudioRgba, TileRef, validate_composition_records,
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
pub const COMPOSITION_PRECOMP: &str = "composition_precomp";
pub const COMPOSITION_TRACK_MATTE: &str = "composition_track_matte";
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
}

#[derive(Clone, Debug)]
struct CompLayer {
    layer_id: String,
    timing: StudioLayerTiming,
    parent: Option<String>,
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
    background: Option<[f32; 4]>,
    /// Topmost first, as stored (STU-MOT-009).
    layers: Vec<CompLayer>,
}

/// A stored composition ready to be evaluated at any tick (module docs).
#[derive(Clone, Debug)]
pub struct CompositionTemporalSource {
    root: Comp,
    filter: ResampleFilter,
    tiles: BTreeMap<String, TileRef>,
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
        validate_composition_records(input.records, doc, input.budget, cancel, input.resolver)
            .map_err(|e| TemporalError::Document(format!("{:?}:{}", e.code, e.target)))?;
        let comp = input
            .records
            .composition(input.composition_id)
            .ok_or_else(|| CompositeError::DanglingInput(input.composition_id.to_owned()))?;
        let mut tiles = BTreeMap::new();
        let root = Self::read(comp, input, &mut tiles, cancel)?;
        Ok(Self {
            root,
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
            if l.precomp.is_some() {
                return Err(unsupported(id, COMPOSITION_PRECOMP));
            }
            if l.track_matte.is_some() {
                return Err(unsupported(id, COMPOSITION_TRACK_MATTE));
            }
            let content = match &dl.payload {
                Payload::Group => Content::None,
                Payload::Raster { tiles } if tiles.is_empty() => Content::None,
                Payload::Raster { tiles } => {
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
            if a.mask.as_ref().is_some_and(|m| m.enabled) {
                return Err(unsupported(id, COMPOSITION_LAYER_MASK));
            }
            let mode = StudioBlendMode::try_from(a.blend_mode).map_err(CompositeError::from)?;
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
            background,
            layers,
        })
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
            .checked_mul(self.root.ticks_per_frame)
            .and_then(|t| i64::try_from(t).ok())
            .ok_or_else(|| CompositeError::Overflow.into())
    }

    /// The layer windows of the composition, for `FrameRequest::timings` (`lower_frame`).
    pub fn timings(&self) -> Vec<LayerTiming> {
        self.root
            .layers
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

    fn find(&self, layer_id: &str) -> Option<&CompLayer> {
        self.root.layers.iter().find(|l| l.layer_id == layer_id)
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
        let (mut anchor, origin) = match &l.content {
            Content::Tiles { bounds, .. } => (
                [f64::from(bounds.width) / 2.0, f64::from(bounds.height) / 2.0],
                [bounds.x as f64, bounds.y as f64],
            ),
            Content::None => ([0.0, 0.0], [0.0, 0.0]),
        };
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
        let origin = match &l.content {
            Content::Tiles { bounds, .. } => [bounds.x as f64, bounds.y as f64],
            Content::None => [0.0, 0.0],
        };
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
        let comp = &self.root;
        let mut layers = Vec::new();
        for l in comp.layers.iter().rev() {
            cancel.check().map_err(|_| TemporalError::Composite(CompositeError::Canceled))?;
            let exists = u64::try_from(tick).is_ok_and(|t| l.timing.contains(t));
            if !exists || !l.visible {
                continue;
            }
            let opacity_scale = Self::opacity_scale(l, tick, cancel)?;
            let opacity = l.opacity * opacity_scale;
            layers.push(SceneLayer {
                layer_id: l.layer_id.clone(),
                content: match &l.content {
                    Content::None => SceneContent::None,
                    Content::Tiles { tiles, .. } => SceneContent::Tiles(tiles.clone()),
                },
                mode: l.mode,
                opacity,
                fill_opacity: l.fill_opacity,
                matrix: Self::matrix(comp, l, tick, cancel)?,
            });
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
        let Some(l) = self.find(layer_id) else {
            return Ok(None);
        };
        let Parts {
            anchor,
            position,
            scale,
            rotation: rotation_deg,
        } = Self::transform_parts(l, tick, cancel)?;
        let origin = match &l.content {
            Content::Tiles { bounds, .. } => [bounds.x as f64, bounds.y as f64],
            Content::None => [0.0, 0.0],
        };
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
        let Some(l) = self.find(layer_id) else {
            return Ok(None);
        };
        Self::matrix(&self.root, l, tick, cancel).map(Some)
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
        let origin = match &l.content {
            Content::Tiles { bounds, .. } => [bounds.x as f64, bounds.y as f64],
            Content::None => [0.0, 0.0],
        };
        Ok(Some([origin[0] + anchor[0], origin[1] + anchor[1]]))
    }
}
