//! Lower an evaluated composite plan into pigment's shared render-plan IR (decision record
//! `diagnostics/render-plan-ir.md`). Composite owns graph semantics and authored ids; the IR is
//! plain, folio-free data that render-cpu / render-gpu execute. The key -> record tables stay on
//! the composite side (`SourceTable`, `MaskTable`), because the IR addresses sources by content
//! hash and `SourceKey` only.
//!
//! Mapping: a raster layer becomes one `Tile` node (one tile) or an isolated group of `Tile`
//! nodes (several tiles, Normal, disjoint grid cells); a composite step becomes a `Group` whose
//! layers follow the authored `input_ordinal` order, bottom-first. Each referencing `Layer`
//! carries the referenced layer's authored appearance (STU-RAS-006): `StudioBlendMode`, opacity
//! and fill opacity as fractions, and an enabled mask as a `MaskInput` over the union of its
//! coverage tiles (density 1, not inverted: Folio does not store either yet). A group follows
//! its mode: pass_through = non-isolated, any other mode = isolated.
//!
//! Clipping (Folio `clipped`): a clipped layer clips to the nearest unclipped layer below it in
//! the same group and lowers to pigment's clip-to-base route (`MaskInput::clip_to_base`, Approximate:
//! the Photoshop rule is UNVERIFIED). A hidden or empty base drops its clipped layers with it, in
//! evaluation too (they neither block the parent nor add extent). Never silently
//! ignored: a clipped layer with no base, with its own mask (one mask slot) or referencing a
//! pass-through group, a pass-through group serving as the clip base (`REFUSE_CLIP_BASE_PASS_THROUGH`,
//! pigment's `PlanError::ClipBasePassThrough` code, Unsupported in the evaluation ledger, consistent
//! with the backends; Photoshop's semantics for it
//! are UNVERIFIED pending the oracle), and an enabled mask without coverage, are `Unavailable`. A
//! hidden or empty pass-through base drops its clipped layers instead. A disabled mask is
//! an authored "no mask" and is omitted. Hidden and empty layers are not emitted.
//!
//! The requested root is rendered regardless of its own visibility flag. Its own appearance is
//! honoured by wrapping it in one synthetic isolated group when it is not neutral over a
//! transparent backdrop (opacity or fill below 1, or an enabled mask); the blend mode itself is
//! irrelevant over a transparent backdrop (W3C `ab = 0` gives `Cs' = Cs`).
//!
//! Extended lowering ([`lower_to_render_plan_extended`], decision record
//! `diagnostics/adjustments-advanced-blending.md`): the same plan plus a pigment `PlanExtensions`
//! envelope built from Folio's `StudioRasterRecords`: adjustment layers (a transparent `Solid`
//! placeholder in the group plus `LayerExtension.adjustment`), mask density / invert / feather,
//! Blend-If, knockout and effects waves 1 and 2 (drop shadow, colour overlay, solid stroke, inner
//! shadow, outer glow, inner glow with a colour source -> `LayerExtension.effects`). The envelope is built with pigment's constructors only; composite
//! adds no formula and no class. A feature the envelope cannot carry is `Unavailable` (effect
//! kinds pigment does not render, effects on adjustment / pass-through / clipped / clip-base
//! layers, non-linear contour, noise, gradient or pattern stroke fill, unresolved Global Light,
//! effect colours it would have to convert, hue bands, threshold colours, channel enablement, and
//! Blend Clipped Layers as Group switched off on a clip base that renders a clipped layer:
//! `blend_clipped_layers_as_group_off_not_modelled`, because pigment's clip rule is the switched-on
//! reading); knockout, modern brightness/contrast and Blend-If on a clip base or a clipped layer
//! (`blend_if_clip_base`, `blend_if_clipped_layer`: no public definition) are carried and refused
//! by pigment's `admit_extended`, and the ledger lists the operators as Unsupported.
use crate::{
    CompositeError,
    evaluate_static::{EvalReport, StepStatus, tile_rects, union},
    lower_extensions::{
        AdjustmentOp, EffectContext, LayerFeatures, LoweringNote, adjustment_for,
        is_adjustment_layer,
    },
    lower_plan::{LoweredPlan, NodeAddr, SourceValue, Step, StepAppearance},
};
use hsk_studio_folio::{Parent, Payload, Snapshot, StudioLayer, StudioRasterRecords, TileRef};
use hsk_studio_pigment::blend_math::{Exactness, ExactnessLedger, StudioBlendMode};
use hsk_studio_pigment::coverage_math::BlendRangeEntry;
use hsk_studio_pigment::effect_math::EffectOp;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, ExtOperator, Group, Knockout, Layer, LayerExtension, MaskFormat, MaskInput,
    Operator, OutputTarget, PlanError, PlanExtensions, PlanLimits, PlanNode, PlanNodeId,
    RenderPlan, SampleFormat, SourceKey, TileInput,
};
use hsk_studio_pigment::Rect;
use std::collections::{BTreeMap, BTreeSet};

/// What the caller knows about one resolved tile that Folio's authored `TileRef` cannot state:
/// the plane hashes and the declared sample/alpha convention of the resolved bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileInfo {
    pub format: SampleFormat,
    pub alpha: AlphaConvention,
    pub colour_sha256: [u8; 32],
    pub alpha_sha256: [u8; 32],
}

/// Caller-owned lookup (host artifact store / resolved tiles). `None` = not resolvable, which
/// makes the lowering `Unavailable` instead of inventing hashes.
pub trait TileInfoSource {
    fn tile_info(&self, tile: &TileRef) -> Option<TileInfo>;
    /// SHA-256 of the single coverage plane the caller resolves from ALL of one mask's tiles
    /// (stitched over the union of their placements). Default: unresolvable.
    fn mask_sha256(&self, _tiles: &[TileRef]) -> Option<[u8; 32]> {
        None
    }
}

impl<F: Fn(&TileRef) -> Option<TileInfo>> TileInfoSource for F {
    fn tile_info(&self, tile: &TileRef) -> Option<TileInfo> {
        self(tile)
    }
}

/// `SourceKey` -> authored tile record, in key order.
pub type SourceTable = Vec<(SourceKey, TileRef)>;
/// `SourceKey` -> the authored coverage tiles that make up one mask plane.
pub type MaskTable = Vec<(SourceKey, Vec<TileRef>)>;

#[derive(Clone, Debug, PartialEq)]
pub struct RenderLowering {
    pub plan: RenderPlan,
    pub sources: SourceTable,
    pub mask_sources: MaskTable,
    /// Intrinsic class of every operator the validated plan uses (formula provenance only; a
    /// backend can lower it further at admission).
    pub exactness: ExactnessLedger,
}

/// Result of [`lower_to_render_plan_extended`]: the plan next to its envelope. Execute it with
/// `RenderBackend::execute_extended`; a backend that has not opted in to an envelope operator
/// refuses it at `admit_extended`.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtendedLowering {
    pub plan: RenderPlan,
    pub extensions: PlanExtensions,
    pub sources: SourceTable,
    pub mask_sources: MaskTable,
    /// Intrinsic class of every v1 and envelope operator the plan uses, plus the classes of the
    /// recorded approximations (`notes`). An envelope operator pigment refuses (knockout, modern
    /// brightness/contrast) is recorded `Unsupported`.
    pub exactness: ExactnessLedger,
    /// The lowering report: every approximation the conversion made (curve corner flags dropped,
    /// exposure linearisation exponent), never silent.
    pub notes: Vec<LoweringNote>,
}

struct Emitter<'a> {
    nodes: Vec<PlanNode>,
    sources: SourceTable,
    mask_sources: MaskTable,
    text_masks: crate::text::TextMaskTable,
    /// `SourceKey` -> vector layer whose coverage plane is that key's mask.
    vector_masks: Vec<(SourceKey, String)>,
    /// `SourceKey` -> image paint source id (`VectorPlane` image rows) the caller serves as a tile.
    image_sources: Vec<(SourceKey, String)>,
    /// `SourceKey` -> (vector layer, row): a stroke row's coverage mask or a painted row's tile.
    vector_rows: Vec<(SourceKey, String, usize)>,
    /// Prepared vectors whose clips (`PreparedVectors::clip`) cut the layers they name.
    clips: Option<&'a crate::vector::PreparedVectors>,
    /// `SourceKey` -> rasterised clip plane (owned).
    clip_planes: Vec<(SourceKey, crate::vector::ClipPlane)>,
    next_key: u32,
    info: &'a dyn TileInfoSource,
}

/// Everything [`lower_inner`] produces.
pub(crate) struct Lowered {
    pub lowering: RenderLowering,
    pub extensions: PlanExtensions,
    pub notes: Vec<LoweringNote>,
    pub text_masks: crate::text::TextMaskTable,
    pub vector_masks: Vec<(SourceKey, String)>,
    pub image_sources: Vec<(SourceKey, String)>,
    pub vector_rows: Vec<(SourceKey, String, usize)>,
    pub clip_planes: Vec<(SourceKey, crate::vector::ClipPlane)>,
}

impl Emitter<'_> {
    /// The clip mask of `layer_id` (lane SMB-COMPOSITE-CLIP-RECORDS): its clip outline rasterised by
    /// nib over the layer's extent united with the clip bounds (a plane that covers everything the
    /// layer draws, so no backend reads outside it). `None` without a clip.
    fn clip_mask(&mut self, layer_id: &str, extent: Option<Rect>) -> Result<Option<MaskInput>, CompositeError> {
        let Some(clip) = self.clips.and_then(|v| v.clip(layer_id)) else {
            return Ok(None);
        };
        let outline = clip.map_err(|code| CompositeError::Unavailable(format!("{layer_id}:{code}")))?;
        let mut rect = extent;
        if let Some([x0, y0, x1, y1]) = outline.and_then(crate::vector_nib::path_bounds) {
            let (fx, fy, gx, gy) = (x0.floor(), y0.floor(), x1.ceil(), y1.ceil());
            let ok = |v: f64| v.is_finite() && v.abs() < 9.0e15;
            if !(ok(fx) && ok(fy) && ok(gx) && ok(gy)) {
                return Err(CompositeError::Overflow);
            }
            let (w, h) = (gx - fx, gy - fy);
            if w > f64::from(u32::MAX) || h > f64::from(u32::MAX) {
                return Err(CompositeError::Overflow);
            }
            // Finite, integral and in range by the checks above.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let bounds = Rect { x: fx as i64, y: fy as i64, width: w as u32, height: h as u32 };
            if bounds.width > 0 && bounds.height > 0 {
                rect = crate::evaluate_static::union(rect, bounds)?;
            }
        }
        let Some(rect) = rect else {
            return Ok(None);
        };
        let coverage = crate::vector_nib::coverage(outline, rect, crate::vector::MAX_CLIP_PIXELS)
            .map_err(|code| CompositeError::Unavailable(format!("{layer_id}:{code}")))?;
        let sha256 = hsk_studio_prism::profile_hash(&coverage);
        let key = self.key()?;
        self.clip_planes.push((
            key,
            crate::vector::ClipPlane { layer_id: layer_id.to_owned(), placement: rect, coverage, sha256 },
        ));
        Ok(Some(MaskInput {
            key,
            placement: rect,
            format: MaskFormat::CoverageU8,
            sha256,
            inverted: false,
            density: 1.0,
        }))
    }
    fn push(&mut self, node: PlanNode) -> Result<PlanNodeId, CompositeError> {
        let id = u32::try_from(self.nodes.len()).map_err(|_| CompositeError::Overflow)?;
        self.nodes.push(node);
        Ok(PlanNodeId(id))
    }
    /// A text layer's content: an isolated group with one Normal layer, a Solid over the plane
    /// masked by the plane's coverage. An inkless layer emits nothing (like an empty raster).
    fn text(
        &mut self,
        plane: &crate::text::TextPlane,
    ) -> Result<Option<PlanNodeId>, CompositeError> {
        let Some(rect) = plane.placement else {
            return Ok(None);
        };
        let key = self.key()?;
        self.text_masks.push((key, plane.layer_id.clone()));
        let solid = self.push(PlanNode::Solid {
            rect,
            rgba: plane.fill_rgba,
        })?;
        let group = self.push(PlanNode::Group(Group {
            isolated: true,
            layers: vec![Layer {
                input: solid,
                mode: StudioBlendMode::Normal,
                opacity: 1.0,
                fill_opacity: 1.0,
                mask: Some(MaskInput {
                    key,
                    placement: rect,
                    format: MaskFormat::CoverageU8,
                    sha256: plane.sha256,
                    inverted: false,
                    density: 1.0,
                }),
            }],
        }))?;
        Ok(Some(group))
    }
    /// A vector layer's content (crate::vector): `Group[Layer { rows, Normal, mask: coverage }]`,
    /// `rows` = one layer per fill row with the row's mode and opacity (a `Solid` over the plane,
    /// or the image `Tile` placed by a layer transform; several copies sit in their own group). The
    /// transform entries are appended to `entries`. An empty shape or no fills emits nothing.
    fn vector(
        &mut self,
        plane: &crate::vector::VectorPlane,
        entries: &mut Vec<LayerExtension>,
    ) -> Result<Option<PlanNodeId>, CompositeError> {
        use crate::vector::{IMAGE_FILTER, PreparedPaint};
        use hsk_studio_pigment::transform_math::{EdgeMode, LayerTransform};
        let Some(rect) = plane.placement else {
            return Ok(None);
        };
        if plane.fills.is_empty() {
            return Ok(None);
        }
        let transform = |a| LayerTransform::new(a, IMAGE_FILTER).with_edge(EdgeMode::Clamp);
        let mut rows = Vec::with_capacity(plane.fills.len());
        let mut row_transforms: Vec<(u32, LayerTransform)> = Vec::new();
        // Stroke rows: (paint node, row index), composited above the fills with their own mask.
        let mut stroke_layers: Vec<(PlanNodeId, usize)> = Vec::new();
        for (index, fill) in plane.fills.iter().enumerate() {
            let stroke = plane.is_stroke(index);
            let input = match &fill.paint {
                PreparedPaint::Solid(rgba) => self.push(PlanNode::Solid { rect, rgba: *rgba })?,
                PreparedPaint::Painted(t) => {
                    let key = self.key()?;
                    self.vector_rows.push((key, plane.layer_id.clone(), index));
                    self.push(PlanNode::Tile(TileInput {
                        key,
                        placement: t.placement,
                        format: SampleFormat::RgbF32Le,
                        alpha: AlphaConvention::Straight,
                        colour_sha256: t.colour_sha256,
                        alpha_sha256: t.alpha_sha256,
                    }))?
                }
                PreparedPaint::Image {
                    source_id,
                    info,
                    placements,
                } => {
                    if stroke {
                        // Refused at preparation; defence in depth.
                        return Err(CompositeError::Unavailable(format!(
                            "{}:{}",
                            plane.layer_id,
                            crate::vector::REFUSE_STROKE_PAINT
                        )));
                    }
                    let key = self.key()?;
                    self.image_sources.push((key, source_id.clone()));
                    let tile = self.push(PlanNode::Tile(TileInput {
                        key,
                        placement: Rect {
                            x: 0,
                            y: 0,
                            width: info.width,
                            height: info.height,
                        },
                        format: info.format,
                        alpha: info.alpha,
                        colour_sha256: info.colour_sha256,
                        alpha_sha256: info.alpha_sha256,
                    }))?;
                    match placements.as_slice() {
                        [one] => {
                            let index =
                                u32::try_from(rows.len()).map_err(|_| CompositeError::Overflow)?;
                            row_transforms.push((index, transform(*one)));
                            tile
                        }
                        many => {
                            let copies = self.push(PlanNode::Group(Group {
                                isolated: true,
                                layers: many
                                    .iter()
                                    .map(|_| Layer {
                                        input: tile,
                                        mode: StudioBlendMode::Normal,
                                        opacity: 1.0,
                                        fill_opacity: 1.0,
                                        mask: None,
                                    })
                                    .collect(),
                            }))?;
                            for (i, a) in many.iter().enumerate() {
                                let i = u32::try_from(i).map_err(|_| CompositeError::Overflow)?;
                                entries.push(
                                    LayerExtension::new(copies, i).with_transform(transform(*a)),
                                );
                            }
                            copies
                        }
                    }
                }
            };
            if stroke {
                stroke_layers.push((input, index));
                continue;
            }
            rows.push(Layer {
                input,
                mode: fill.mode,
                opacity: fill.opacity,
                fill_opacity: 1.0,
                mask: None,
            });
        }
        let mut content_layers = Vec::with_capacity(stroke_layers.len().saturating_add(1));
        if !rows.is_empty() {
            let rows_group = self.push(PlanNode::Group(Group {
                isolated: true,
                layers: rows,
            }))?;
            for (i, t) in row_transforms {
                entries.push(LayerExtension::new(rows_group, i).with_transform(t));
            }
            let key = self.key()?;
            self.vector_masks.push((key, plane.layer_id.clone()));
            content_layers.push(Layer {
                input: rows_group,
                mode: StudioBlendMode::Normal,
                opacity: 1.0,
                fill_opacity: 1.0,
                mask: Some(MaskInput {
                    key,
                    placement: rect,
                    format: MaskFormat::CoverageU8,
                    sha256: plane.sha256,
                    inverted: false,
                    density: 1.0,
                }),
            });
        }
        for (input, index) in stroke_layers {
            let (Some(fill), Some(stroke)) = (
                plane.fills.get(index),
                plane.strokes.iter().find(|s| s.row == index),
            ) else {
                return Err(CompositeError::Overflow);
            };
            let key = self.key()?;
            self.vector_rows.push((key, plane.layer_id.clone(), index));
            content_layers.push(Layer {
                input,
                mode: fill.mode,
                opacity: fill.opacity,
                fill_opacity: 1.0,
                mask: Some(MaskInput {
                    key,
                    placement: rect,
                    format: MaskFormat::CoverageU8,
                    sha256: stroke.sha256,
                    inverted: false,
                    density: 1.0,
                }),
            });
        }
        let content = self.push(PlanNode::Group(Group {
            isolated: true,
            layers: content_layers,
        }))?;
        Ok(Some(content))
    }
    fn key(&mut self) -> Result<SourceKey, CompositeError> {
        let key = SourceKey(self.next_key);
        self.next_key = self.next_key.checked_add(1).ok_or(CompositeError::Overflow)?;
        Ok(key)
    }
    /// The referencing layer: the referenced layer's authored appearance plus its mask. A clipped
    /// layer carries pigment's clip-to-base route in its single mask slot, so a clipped layer
    /// with its own mask is refused (evaluation already marks it unavailable). `mask_params`:
    /// the layer's density / invert (extended lowering; `None` = density 1, not inverted).
    fn layer(
        &mut self,
        input: PlanNodeId,
        appearance: &StepAppearance,
        layer: &StudioLayer,
        mask_params: Option<crate::lower_extensions::MaskParams>,
        clip: Option<MaskInput>,
    ) -> Result<Layer, CompositeError> {
        if clip.is_some() && (appearance.clipped || appearance.mask) {
            return Err(CompositeError::Unavailable(format!(
                "{}:{}",
                layer.layer_id,
                crate::vector::REFUSE_CLIP_WITH_MASK
            )));
        }
        let mask = match (appearance.clipped, appearance.mask) {
            (false, false) if clip.is_some() => clip,
            (true, true) => {
                return Err(CompositeError::Unavailable(format!(
                    "{}:clipped_with_mask_unsupported",
                    layer.layer_id
                )));
            }
            (true, false) => Some(MaskInput::clip_to_base()),
            (false, true) => Some(self.mask(layer, mask_params)?),
            (false, false) => None,
        };
        Ok(Layer {
            input,
            mode: appearance.mode,
            opacity: appearance.opacity,
            fill_opacity: appearance.fill_opacity,
            mask,
        })
    }
    fn mask(
        &mut self,
        layer: &StudioLayer,
        params: Option<crate::lower_extensions::MaskParams>,
    ) -> Result<MaskInput, CompositeError> {
        let tiles = layer
            .appearance
            .as_ref()
            .and_then(|a| a.mask.as_ref())
            .map(|m| m.tiles.as_slice())
            .filter(|t| !t.is_empty())
            .ok_or_else(|| {
                CompositeError::Unavailable(format!("{}:mask_empty", layer.layer_id))
            })?;
        let format = mask_format(tiles)?;
        let mut placement = None;
        for rect in tile_rects(tiles)? {
            placement = union(placement, rect)?;
        }
        let placement = placement.ok_or(CompositeError::Plan(PlanError::Empty))?;
        let sha256 = self.info.mask_sha256(tiles).ok_or_else(|| {
            CompositeError::Unavailable(format!("{}/mask:mask_hash", layer.layer_id))
        })?;
        let key = self.key()?;
        self.mask_sources.push((key, tiles.to_vec()));
        Ok(MaskInput {
            key,
            placement,
            format,
            sha256,
            inverted: params.is_some_and(|p| p.inverted),
            density: params.map_or(1.0, |p| p.density),
        })
    }
}

fn mask_format(tiles: &[TileRef]) -> Result<MaskFormat, CompositeError> {
    let format = |t: &TileRef| match t.format.as_str() {
        "coverage_u8" => Some(MaskFormat::CoverageU8),
        "coverage_u16le" => Some(MaskFormat::CoverageU16Le),
        _ => None,
    };
    let first = tiles.first().and_then(format);
    match first {
        Some(f) if tiles.iter().all(|t| format(t) == Some(f)) => Ok(f),
        _ => Err(CompositeError::UnsupportedDescriptor("mask_format".into())),
    }
}

/// One layer's envelope entry before its group node id is known.
struct EntryParts {
    layer: u32,
    adjustment: Option<AdjustmentOp>,
    blend_if: Vec<BlendRangeEntry>,
    feather_px: f32,
    knockout: Knockout,
    effects: Vec<EffectOp>,
    blur: Option<hsk_studio_pigment::blur_math::LayerBlur>,
}

impl EntryParts {
    fn new(
        layer_index: usize,
        features: &LayerFeatures,
        coverage_mask: bool,
        adjustment: Option<AdjustmentOp>,
    ) -> Result<Self, CompositeError> {
        Ok(Self {
            layer: u32::try_from(layer_index).map_err(|_| CompositeError::Overflow)?,
            adjustment,
            blend_if: features.blend_if.clone(),
            // Feather blurs a coverage plane: it needs an enabled, unclipped mask on the layer.
            feather_px: features
                .mask
                .filter(|_| coverage_mask)
                .map_or(0.0, |m| m.feather_px),
            knockout: features.knockout,
            effects: features.effect_ops().to_vec(),
            blur: features.blur,
        })
    }
    fn is_empty(&self) -> bool {
        self.adjustment.is_none()
            && self.blend_if.is_empty()
            && self.feather_px <= 0.0
            && self.knockout == Knockout::None
            && self.effects.is_empty()
            && self.blur.is_none()
    }
    fn into_extension(self, group: PlanNodeId) -> LayerExtension {
        let mut e = LayerExtension::new(group, self.layer);
        if let Some(op) = self.adjustment {
            e = e.with_adjustment(op);
        }
        if !self.blend_if.is_empty() {
            e = e.with_blend_if(self.blend_if);
        }
        if self.feather_px > 0.0 {
            e = e.with_mask_feather(self.feather_px);
        }
        if self.knockout != Knockout::None {
            e = e.with_knockout(self.knockout);
        }
        if !self.effects.is_empty() {
            e = e.with_effects(self.effects);
        }
        if let Some(b) = self.blur {
            e = e.with_blur(b);
        }
        e
    }
}

/// What a group input contributes at its position.
enum Slot {
    /// Hidden, empty or (adjustment in an empty isolated group) nothing to act on.
    Empty,
    Node(PlanNodeId),
    /// An adjustment layer; the rectangle bounds where it applies.
    Adjustment(Rect),
}

fn record_ext(ledger: &mut ExactnessLedger, op: ExtOperator) {
    ledger.record(op.key(), op.intrinsic());
}

/// Fold the envelope into the ledger and validate it, first entry by entry and then as a whole.
/// Knockout and an adjustment feature without a formula (modern brightness/contrast) are carried,
/// not lowering errors: pigment's `admit_extended` refuses them with their operator name
/// (`tests/render_plan_lowering.rs` `knockout_and_effect_stack_are_refused`), and the ledger says
/// `Unsupported`. Every other envelope defect is a typed lowering error: an entry's own defect from
/// its probe, and a defect only the whole envelope has (more than `MAX_EFFECTS_PER_PLAN` effects
/// over all layers, a repeated `(group, layer)`) from the whole-envelope validate, as
/// `CompositeError::Plan` with pigment's own `PlanError`: the lowering and `admit_extended` run the
/// same validation of the plan and of the whole envelope, so the `InvalidPlan` refusals of
/// admission are lowering errors here (test
/// `plan_wide_effect_limit_is_refused_at_lowering_like_pigment_admission`, which compares the
/// error with admission's for an envelope one effect over the plan limit).
fn record_envelope(
    render: &RenderPlan,
    extensions: &PlanExtensions,
    ledger: &mut ExactnessLedger,
) -> Result<(), CompositeError> {
    // Entries whose adjustment feature pigment refuses (carried above as an `Unsupported` entry).
    let mut carried_adjustment: BTreeSet<usize> = BTreeSet::new();
    for (index, e) in extensions.layers.iter().enumerate() {
        let mut probe = e.clone();
        if probe.knockout != Knockout::None {
            record_ext(ledger, ExtOperator::Knockout(probe.knockout));
            probe.knockout = Knockout::None;
        }
        match PlanExtensions::from_layers(vec![probe]).validate(render) {
            Ok(ops) => {
                for op in ops {
                    record_ext(ledger, op);
                }
            }
            Err(PlanError::AdjustmentUnsupported { kind, .. }) => {
                carried_adjustment.insert(index);
                record_ext(ledger, ExtOperator::AdjustmentLayer);
                record_ext(ledger, ExtOperator::Adjustment(kind));
                ledger.record(kind.key(), Exactness::Unsupported);
            }
            Err(other) => return Err(CompositeError::Plan(other)),
        }
    }
    if render.has_mask_density() {
        record_ext(ledger, ExtOperator::MaskDensity);
    }
    // Rules that need the whole envelope, which the entry-by-entry probes above cannot see: the
    // effect count over all layers (`PlanError::EffectLimit`), a repeated `(group, layer)`, and
    // nested effects (the content of a layer holds another layer with effects). Pigment's validate
    // of the full envelope decides all of them, and admission runs the same validate. The features
    // carried above are taken out of this probe, so they neither end it early nor hide a defect of
    // a later entry: knockout is cleared, and a refused adjustment is dropped from its entry (such
    // an entry holds no effects, `LayerFeatures::refusal`). Any other error is a lowering error.
    let whole = PlanExtensions::from_layers(
        extensions
            .layers
            .iter()
            .enumerate()
            .map(|(index, e)| {
                let mut e = e.clone();
                e.knockout = Knockout::None;
                if carried_adjustment.contains(&index) {
                    e.adjustment = None;
                }
                e
            })
            .collect(),
    );
    match whole.validate(render) {
        Ok(ops) => {
            if ops.contains(&ExtOperator::EffectsNested) {
                record_ext(ledger, ExtOperator::EffectsNested);
            }
        }
        Err(other) => return Err(CompositeError::Plan(other)),
    }
    // An effect composites with its own blend mode; pigment's admission asks the backend for that
    // mode's v1 class, so the ledger lists it under the mode's pigment key.
    for mode in extensions.effect_modes() {
        let op = Operator::Blend(mode);
        ledger.record(op.key(), op.intrinsic());
    }
    Ok(())
}

/// Lower `root` of an evaluated plan. The result has passed `RenderPlan::validate`.
///
/// Fails `Unavailable` when the blending profile is unbound, when any step reachable from `root`
/// is unavailable (including clipped layers and empty masks), when a reachable layer has an
/// unresolved document issue, or when `info` cannot describe a reachable tile or mask;
/// `ColourMismatch` when the target's declared blend space differs from the plan's;
/// `StaleRevision` when plan, report and snapshot revisions differ.
pub fn lower_to_render_plan(
    plan: &LoweredPlan,
    report: &EvalReport,
    snap: &Snapshot,
    root: &NodeAddr,
    target: OutputTarget,
    info: &dyn TileInfoSource,
) -> Result<RenderLowering, CompositeError> {
    Ok(lower_inner(plan, report, snap, None, root, target, info, None, None)?.lowering)
}

/// [`lower_to_render_plan`] plus the pigment extension envelope built from `records` (the
/// document's `StudioRasterRecords`, `records.document_id` = the snapshot's document). Use
/// `evaluate_extended` for the report: it refuses the same cases. Layout of the result:
///
/// - Adjustment layer: a `Layer` in its group whose input is a transparent `Solid` placeholder
///   (`rgba [0; 4]`) over the group's content extent (an isolated group) or the target extent (a
///   pass-through group, whose backdrop is the parent's), with `LayerExtension.adjustment`. In an
///   isolated group with no content to act on it is dropped (it changes nothing there).
/// - Mask parameters: `density / 100` and `inverted` on the `MaskInput`, `feather` in
///   `LayerExtension.mask_feather_px` (an enabled coverage mask only).
/// - Blend-If: `level / 255` ranges `[black_start, black_end, white_start, white_end]`, source
///   then destination, per gray/red/green/blue entry; identity entries are dropped.
/// - Knockout: `Enabled` (and an `Inherited` that resolves to enabled) is `Knockout::Shallow`.
/// - Effects: the enabled instances of an enabled stack at 100 percent scale, in list order, as
///   pigment `EffectOp`s (percent / 100; a drop shadow resolves Global Light into its angle;
///   spread and choke are read as a percent of the size and noted). Drop shadow, colour overlay,
///   solid stroke, inner shadow, outer glow and inner glow (colour source) only.
///
/// Fails `Unavailable` for an enabled effect of any other kind (`layer_effects_not_rendered`), a
/// gradient glow, a glow range other than 100 percent or the Precise technique (pigment's
/// `glow_gradient` / `glow_range` / `glow_technique_precise`), a
/// stack scale other than 100, a contour other than linear, noise above zero, a gradient or
/// pattern or overprint stroke, a drop shadow that follows an absent Global Light, an effect
/// colour in another profile (or mid-tone in a linear-light plan), effects on an adjustment,
/// pass-through or clipped layer or on a clip base, hue bands, threshold colours, exposure
/// `use_legacy`, channel enablement other than
/// all on, a clipped layer whose base is an adjustment layer, and a visible adjustment layer that
/// the composition graph does not place in its (rendered) parent group: an adjustment layer is a
/// graph source whose `input_ordinal` is its stack position (Folio ENG-6, STU-CMP-092), so one that
/// is contained in the group but has no graph operation has no authored position, and the spec
/// forbids inferring one from storage order or containment (`adjustment_not_in_composition_graph`).
///
/// Fails `CompositeError::Plan(PlanError::EffectLimit)` (code `plan_effect_limit`) when the rendered
/// layers together carry more than pigment's `MAX_EFFECTS_PER_PLAN` effects: each layer is within
/// `MAX_EFFECTS_PER_LAYER` (`effect_limit`, `Unavailable`), but the count is a rule of the whole
/// plan, so it depends on the requested root and is decided here, not by `evaluate_extended` (which
/// reports every step of the document). Any other defect that only the whole envelope has is
/// returned the same way, as pigment's own `PlanError`.
pub fn lower_to_render_plan_extended(
    plan: &LoweredPlan,
    report: &EvalReport,
    snap: &Snapshot,
    records: &StudioRasterRecords,
    root: &NodeAddr,
    target: OutputTarget,
    info: &dyn TileInfoSource,
) -> Result<ExtendedLowering, CompositeError> {
    if records.document_id != snap.document().document_id {
        return Err(CompositeError::UnsupportedDescriptor(
            "raster_records_document_mismatch".into(),
        ));
    }
    let lowered = lower_inner(plan, report, snap, Some(records), root, target, info, None, None)?;
    Ok(ExtendedLowering {
        plan: lowered.lowering.plan,
        extensions: lowered.extensions,
        sources: lowered.lowering.sources,
        mask_sources: lowered.lowering.mask_sources,
        exactness: lowered.lowering.exactness,
        notes: lowered.notes,
    })
}

/// Folio-free plan for one prepared text layer rendered as its own neutral root: the same emitter
/// and node layout as a text root lowered from a document ([`lower_inner`]), so the plans are equal
/// (test `tests/text_lowering.rs` `text_layer_lowers_with_pinned_digest`).
pub(crate) fn text_root_plan(
    plane: &crate::text::TextPlane,
    revision: u64,
    target: OutputTarget,
) -> Result<(RenderLowering, crate::text::TextMaskTable), CompositeError> {
    let none = |_: &TileRef| None;
    let mut out = Emitter {
        nodes: Vec::new(),
        sources: Vec::new(),
        mask_sources: Vec::new(),
        text_masks: Vec::new(),
        vector_masks: Vec::new(),
        image_sources: Vec::new(),
        vector_rows: Vec::new(),
        clips: None,
        clip_planes: Vec::new(),
        next_key: 0,
        info: &none,
    };
    let root = out.text(plane)?.ok_or_else(|| {
        CompositeError::Unavailable(format!("{}:text_layer_empty", plane.layer_id))
    })?;
    let render = RenderPlan {
        revision,
        target,
        nodes: out.nodes,
        root,
    };
    let operators = render
        .validate(&PlanLimits::default())
        .map_err(CompositeError::Plan)?;
    let mut exactness = ExactnessLedger::default();
    for op in operators {
        exactness.record(op.key(), op.intrinsic());
    }
    Ok((
        RenderLowering {
            plan: render,
            sources: out.sources,
            mask_sources: out.mask_sources,
            exactness,
        },
        out.text_masks,
    ))
}

/// `text`: prepared text planes (crate::text); `None` keeps every text layer unsupported.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_inner(
    plan: &LoweredPlan,
    report: &EvalReport,
    snap: &Snapshot,
    records: Option<&StudioRasterRecords>,
    root: &NodeAddr,
    target: OutputTarget,
    info: &dyn TileInfoSource,
    text: Option<&crate::text::PreparedText>,
    vectors: Option<&crate::vector::PreparedVectors>,
) -> Result<Lowered, CompositeError> {
    let doc = snap.document();
    for revision in [plan.revision, report.receipt.revision] {
        if revision != doc.revision {
            return Err(CompositeError::StaleRevision {
                expected: revision,
                actual: doc.revision,
            });
        }
    }
    if !plan.blend_profile_bound {
        return Err(CompositeError::Unavailable(
            "blending_space:unassigned_profile".into(),
        ));
    }
    if target.space.blend_space != plan.blend_space {
        return Err(CompositeError::ColourMismatch);
    }
    let index: BTreeMap<&NodeAddr, usize> = plan
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.addr(), i))
        .collect();
    if index.len() != plan.steps.len() || report.steps.len() != plan.steps.len() {
        return Err(CompositeError::UnsupportedDescriptor(
            "plan_report_mismatch".into(),
        ));
    }
    let root_index = *index
        .get(root)
        .ok_or_else(|| CompositeError::DanglingInput(root.text()))?;

    if plan.steps[root_index].appearance().clipped {
        // A root has no siblings, hence no clip base.
        return Err(CompositeError::Unavailable(format!(
            "{}:clip_without_base",
            root.text()
        )));
    }
    let layers: BTreeMap<&str, &StudioLayer> = doc
        .layers
        .iter()
        .map(|l| (l.layer_id.as_str(), l))
        .collect();
    if records.is_some() && layers.get(root.layer_id.as_str()).is_some_and(|l| is_adjustment_layer(l)) {
        // An adjustment acts on a backdrop; a root has none.
        return Err(CompositeError::Unavailable(format!(
            "{}:adjustment_layer_as_root",
            root.layer_id
        )));
    }

    // Phase 1: steps reachable from the root through visible inputs. A clipped input clips to the
    // nearest unclipped input below it; with a hidden base it is not part of the render.
    let mut reachable = BTreeSet::new();
    let mut stack = vec![root_index];
    while let Some(i) = stack.pop() {
        if !reachable.insert(i) {
            continue;
        }
        // A live boolean group is drawn as one shape: its inputs are not part of the render.
        if vectors.is_some_and(|v| v.knows(&plan.steps[i].addr().layer_id)) {
            continue;
        }
        if let Step::Composite { inputs, .. } = &plan.steps[i] {
            // Is the nearest unclipped input below a live base? (`None`: no unclipped input yet.)
            // A hidden or empty base takes its clipped layers with it, so they are not part of
            // the render and their own status is not checked (the evaluation rule).
            let mut base_live: Option<bool> = None;
            for input in inputs {
                let child = *index
                    .get(input)
                    .ok_or_else(|| CompositeError::DanglingInput(input.text()))?;
                if child >= i {
                    return Err(CompositeError::Cycle(input.text()));
                }
                let step = &plan.steps[child];
                if step.appearance().clipped {
                    match base_live {
                        None => {
                            return Err(CompositeError::Unavailable(format!(
                                "{}:clip_without_base",
                                input.text()
                            )));
                        }
                        Some(false) => continue,
                        Some(true) => {}
                    }
                } else {
                    base_live = Some(step.visible() && !report.steps[child].is_ready_and_empty());
                }
                if step.visible() {
                    stack.push(child);
                }
            }
        }
    }
    for i in &reachable {
        let eval = &report.steps[*i];
        if eval.addr != *plan.steps[*i].addr() {
            return Err(CompositeError::UnsupportedDescriptor(
                "plan_report_mismatch".into(),
            ));
        }
        if let StepStatus::Unavailable(reason) = &eval.status {
            return Err(CompositeError::Unavailable(format!(
                "{}:{reason}",
                eval.addr.text()
            )));
        }
        for u in &report.unavailable {
            if u.target.starts_with(&eval.addr.layer_id) {
                return Err(CompositeError::Unavailable(format!(
                    "{}:{}",
                    u.target, u.reason
                )));
            }
        }
    }
    if records.is_some() {
        // A visible adjustment layer contained in a rendered group but absent from the group's
        // composition inputs would vanish from the render: refuse it (no stack position exists).
        let in_plan: BTreeSet<&str> = plan.steps.iter().map(|s| s.addr().layer_id.as_str()).collect();
        let groups: BTreeSet<&str> = reachable
            .iter()
            .filter(|i| matches!(plan.steps[**i], Step::Composite { .. }))
            .map(|i| plan.steps[*i].addr().layer_id.as_str())
            .collect();
        // The document root (`LoweredPlan::with_document_root`) contains the top-level layers.
        let document_root = groups.contains(crate::lower_plan::DOCUMENT_ROOT_LAYER);
        for l in &doc.layers {
            let contained = match &l.parent {
                Parent::Node(p) => groups.contains(p.node_id.as_str()),
                Parent::Root(()) => document_root,
            };
            if l.visible && contained && is_adjustment_layer(l) && !in_plan.contains(l.layer_id.as_str())
            {
                return Err(CompositeError::Unavailable(format!(
                    "{}:adjustment_not_in_composition_graph",
                    l.layer_id
                )));
            }
        }
    }

    // Phase 2: emit nodes in step order (every reference points to an earlier node).
    let mut out = Emitter {
        nodes: Vec::new(),
        sources: Vec::new(),
        mask_sources: Vec::new(),
        text_masks: Vec::new(),
        vector_masks: Vec::new(),
        image_sources: Vec::new(),
        vector_rows: Vec::new(),
        clips: vectors,
        clip_planes: Vec::new(),
        next_key: 0,
        info,
    };
    let mut node_of: BTreeMap<usize, PlanNodeId> = BTreeMap::new();
    let effect_cx = records.map(|r| EffectContext {
        global_light: r.global_light.as_ref(),
        blending: &doc.blending_space,
        space: plan.blend_space,
    });
    let root_features = records
        .zip(effect_cx.as_ref())
        .map(|(r, cx)| LayerFeatures::of(&layers, r, cx, &root.layer_id));
    if let Some(reason) = root_features
        .as_ref()
        .and_then(|f| f.refusal(plan.steps[root_index].appearance(), false))
    {
        return Err(CompositeError::Unavailable(format!(
            "{}:{reason}",
            root.layer_id
        )));
    }
    // Blend-If or knockout on the root change its own source alpha even over a transparent
    // backdrop, so such a root is wrapped (the wrapper layer carries the entry).
    let root_neutral = plan.steps[root_index]
        .appearance()
        .is_neutral_over_transparent()
        && root_features.as_ref().is_none_or(LayerFeatures::is_neutral)
        // A clipped root (vector clip record) is cut by its clip: wrapped like a masked root.
        && vectors.is_none_or(|v| v.clip(&root.layer_id).is_none());
    let mut entries: Vec<LayerExtension> = Vec::new();
    let mut notes: Vec<LoweringNote> = Vec::new();
    for (i, step) in plan.steps.iter().enumerate() {
        if !reachable.contains(&i) {
            continue;
        }
        match step {
            Step::Source { addr, value, .. } => {
                if records.is_some() && layers.get(addr.layer_id.as_str()).is_some_and(|l| is_adjustment_layer(l)) {
                    // An adjustment layer has no node of its own: its placeholder is emitted in
                    // the group that composites it (it needs that group's extent).
                    continue;
                }
                // A text source, text-typed or image-typed (Folio G1), draws its prepared plane.
                let text_payload = matches!(
                    layers.get(addr.layer_id.as_str()).map(|l| &l.payload),
                    Some(Payload::Text { .. })
                );
                let draws_text = *value == SourceValue::Text
                    || (*value == SourceValue::Image && text_payload);
                // A vector source draws its prepared plane, vector-typed or image-typed (Folio
                // SMB-FOLIO-VECTOR-IMAGE-PORT: a rasterisable vector as a group input).
                let vector_payload = matches!(
                    layers.get(addr.layer_id.as_str()).map(|l| &l.payload),
                    Some(Payload::Vector { .. })
                );
                let draws_vector = *value == SourceValue::Vector
                    || (*value == SourceValue::Image && vector_payload);
                if let (true, Some(prepared)) = (draws_vector, vectors) {
                    let plane = prepared.plane(&addr.layer_id).ok_or_else(|| {
                        CompositeError::Unavailable(format!(
                            "{}:vector_layer_not_prepared",
                            addr.text()
                        ))
                    })?;
                    if let Some(id) = out.vector(plane, &mut entries)? {
                        node_of.insert(i, id);
                    }
                    continue;
                }
                if let (true, Some(prepared)) = (draws_text, text) {
                    let plane = prepared.plane(&addr.layer_id).ok_or_else(|| {
                        CompositeError::Unavailable(format!(
                            "{}:text_layer_not_prepared",
                            addr.text()
                        ))
                    })?;
                    if let Some(id) = out.text(plane)? {
                        node_of.insert(i, id);
                    }
                    continue;
                }
                let (SourceValue::Image, Some(Payload::Raster { tiles: tile_records })) = (
                    value,
                    layers.get(addr.layer_id.as_str()).map(|l| &l.payload),
                ) else {
                    return Err(CompositeError::Unavailable(format!(
                        "{}:unsupported_payload",
                        addr.text()
                    )));
                };
                let rects = tile_rects(tile_records)?;
                let mut tile_nodes = Vec::with_capacity(tile_records.len());
                for (record, rect) in tile_records.iter().zip(rects) {
                    let found = out.info.tile_info(record).ok_or_else(|| {
                        CompositeError::Unavailable(format!(
                            "{}/tile/{}:tile_info",
                            record.layer_id, record.object_key
                        ))
                    })?;
                    let key = out.key()?;
                    out.sources.push((key, record.clone()));
                    let id = out.push(PlanNode::Tile(TileInput {
                        key,
                        placement: rect,
                        format: found.format,
                        alpha: found.alpha,
                        colour_sha256: found.colour_sha256,
                        alpha_sha256: found.alpha_sha256,
                    }))?;
                    tile_nodes.push(id);
                }
                match tile_nodes.as_slice() {
                    [] => {}
                    [only] => {
                        node_of.insert(i, *only);
                    }
                    many => {
                        let mut group_layers = Vec::with_capacity(many.len());
                        for id in many {
                            group_layers.push(Layer {
                                input: *id,
                                mode: StudioBlendMode::Normal,
                                opacity: 1.0,
                                fill_opacity: 1.0,
                                mask: None,
                            });
                        }
                        let id = out.push(PlanNode::Group(Group {
                            isolated: true,
                            layers: group_layers,
                        }))?;
                        node_of.insert(i, id);
                    }
                }
            }
            Step::Composite { addr, .. }
                if vectors.is_some_and(|v| v.knows(&addr.layer_id)) =>
            {
                let plane = vectors
                    .and_then(|v| v.plane(&addr.layer_id))
                    .ok_or_else(|| {
                        CompositeError::Unavailable(format!(
                            "{}:vector_layer_not_prepared",
                            addr.text()
                        ))
                    })?;
                if let Some(id) = out.vector(plane, &mut entries)? {
                    node_of.insert(i, id);
                }
            }
            Step::Composite { inputs, appearance, .. } => {
                let is_root = i == root_index;
                // A neutral root renders over a transparent backdrop, where isolated and
                // pass-through are equivalent; every other group follows its mode.
                let group_isolated = (is_root && root_neutral)
                    || appearance.mode != StudioBlendMode::PassThrough;
                // Where an adjustment layer of this group acts: an isolated group starts from a
                // transparent backdrop, so only where its own content is (`None`: nowhere); a
                // pass-through group sees the parent's backdrop, i.e. the whole target.
                let adjustment_area: Option<Rect> = if group_isolated {
                    report.steps[i].extent
                } else {
                    Some(target.extent)
                };
                let mut group_layers = Vec::with_capacity(inputs.len());
                let mut parts: Vec<EntryParts> = Vec::new();
                // Was the nearest unclipped input below emitted? (`None`: no unclipped input yet.)
                let mut base_emitted: Option<bool> = None;
                // Is that base a pass-through group (no isolated content to take a base alpha from)?
                let mut base_pass_through = false;
                // Is that base a (visible) adjustment layer (no base alpha to clip to)?
                let mut base_adjustment = false;
                // Is that base an emitted layer with effects (pigment refuses it as a base)?
                let mut base_effects = false;
                // Is that base an emitted layer with Blend Clipped Layers as Group switched off
                // (`clbl = 0`)? Pigment's clip rule is the switched-on reading.
                let mut base_group_blend_off = false;
                for input in inputs {
                    let child = index[input];
                    let child_visible = plan.steps[child].visible();
                    let child_layer = layers.get(input.layer_id.as_str()).copied();
                    let adjustment = records.is_some() && child_layer.is_some_and(is_adjustment_layer);
                    let slot = if !child_visible {
                        Slot::Empty
                    } else if adjustment {
                        adjustment_area.map_or(Slot::Empty, Slot::Adjustment)
                    } else {
                        node_of.get(&child).map_or(Slot::Empty, |id| Slot::Node(*id))
                    };
                    let present = !matches!(slot, Slot::Empty);
                    let features = if present {
                        records
                            .zip(effect_cx.as_ref())
                            .zip(child_layer)
                            .map(|((r, cx), l)| LayerFeatures::of(&layers, r, cx, &l.layer_id))
                    } else {
                        None
                    };
                    if plan.steps[child].appearance().clipped {
                        // Emitted clipped layer on a pass-through base: backends refuse it, and
                        // Photoshop's semantics are UNVERIFIED (oracle pending), so never guess.
                        if base_pass_through && present {
                            return Err(CompositeError::Unavailable(format!(
                                "{}:{}",
                                input.text(),
                                crate::lower_extensions::REFUSE_CLIP_BASE_PASS_THROUGH
                            )));
                        }
                        // Pigment: an adjustment layer cannot be a clip base.
                        if base_adjustment && present {
                            return Err(CompositeError::Unavailable(format!(
                                "{}:clip_base_adjustment",
                                input.text()
                            )));
                        }
                        // Pigment: effects on the base of a clipping run are refused.
                        if base_effects && present {
                            return Err(CompositeError::Unavailable(format!(
                                "{}:{}",
                                input.text(),
                                crate::lower_extensions::REFUSE_EFFECT_ON_CLIP_BASE
                            )));
                        }
                        // The switched-off reading of Blend Clipped Layers as Group is not
                        // modelled: refused with a typed code, never rendered as if it were on.
                        if base_group_blend_off && present {
                            return Err(CompositeError::Unavailable(format!(
                                "{}:{}",
                                input.text(),
                                crate::lower_extensions::REFUSE_CLIP_GROUP_BLEND_OFF
                            )));
                        }
                        match base_emitted {
                            None => {
                                return Err(CompositeError::Unavailable(format!(
                                    "{}:clip_without_base",
                                    input.text()
                                )));
                            }
                            // Nothing to clip to: the clipped layer contributes nothing.
                            Some(false) => continue,
                            Some(true) => {}
                        }
                    } else {
                        base_emitted = Some(present);
                        base_pass_through = present
                            && plan.steps[child].appearance().mode == StudioBlendMode::PassThrough;
                        base_adjustment = adjustment && child_visible;
                        base_effects = features
                            .as_ref()
                            .is_some_and(|f| f.has_effects() || f.has_blur());
                        base_group_blend_off = features
                            .as_ref()
                            .is_some_and(|f| f.clipped_group_blend_off);
                    }
                    let input_id = match slot {
                        Slot::Empty => continue,
                        Slot::Node(id) => id,
                        Slot::Adjustment(rect) => out.push(PlanNode::Solid {
                            rect,
                            rgba: [0.0; 4],
                        })?,
                    };
                    let layer = child_layer
                        .ok_or_else(|| CompositeError::DanglingInput(input.text()))?;
                    let child_appearance = plan.steps[child].appearance();
                    // A backdrop blur needs the group's own result as its backdrop (pigment's
                    // `BlurCombination` for a non-isolated group).
                    if !group_isolated
                        && features.as_ref().is_some_and(LayerFeatures::has_backdrop_blur)
                    {
                        return Err(CompositeError::Unavailable(format!(
                            "{}:{}",
                            layer.layer_id,
                            crate::lower_extensions::REFUSE_BLUR_COMBINATION
                        )));
                    }
                    if let Some(reason) = features
                        .as_ref()
                        .and_then(|f| f.refusal(child_appearance, adjustment))
                    {
                        return Err(CompositeError::Unavailable(format!(
                            "{}:{reason}",
                            layer.layer_id
                        )));
                    }
                    if let Some(f) = &features {
                        for (key, exactness, detail) in &f.notes {
                            notes.push(LoweringNote {
                                layer_id: layer.layer_id.clone(),
                                key,
                                exactness: *exactness,
                                detail: detail.clone(),
                            });
                        }
                    }
                    let adjustment_op = match (adjustment, records) {
                        (true, Some(r)) => {
                            let converted = adjustment_for(r, &layer.layer_id, plan.blend_space)
                                .map_err(|code| {
                                    CompositeError::Unavailable(format!(
                                        "{}:{code}",
                                        layer.layer_id
                                    ))
                                })?;
                            for (key, exactness, detail) in converted.notes {
                                notes.push(LoweringNote {
                                    layer_id: layer.layer_id.clone(),
                                    key,
                                    exactness,
                                    detail,
                                });
                            }
                            Some(converted.op)
                        }
                        _ => None,
                    };
                    let clip = out.clip_mask(&layer.layer_id, report.steps[child].extent)?;
                    let emitted_layer = out.layer(
                        input_id,
                        child_appearance,
                        layer,
                        features.as_ref().and_then(|f| f.mask),
                        clip,
                    )?;
                    if let Some(f) = &features {
                        let coverage_mask = child_appearance.mask && !child_appearance.clipped;
                        let p =
                            EntryParts::new(group_layers.len(), f, coverage_mask, adjustment_op)?;
                        if !p.is_empty() {
                            parts.push(p);
                        }
                    }
                    group_layers.push(emitted_layer);
                }
                if group_layers.is_empty() && !is_root {
                    continue;
                }
                let group = Group {
                    isolated: group_isolated,
                    layers: group_layers,
                };
                let id = out.push(PlanNode::Group(group))?;
                entries.extend(parts.into_iter().map(|p| p.into_extension(id)));
                node_of.insert(i, id);
            }
        }
    }
    let mut root_node = *node_of
        .get(&root_index)
        .ok_or(CompositeError::Plan(PlanError::Empty))?;
    if !root_neutral {
        // The root's own opacity / fill / mask apply: one synthetic isolated wrapper layer.
        let root_layer = layers
            .get(root.layer_id.as_str())
            .ok_or_else(|| CompositeError::DanglingInput(root.text()))?;
        let root_appearance = plan.steps[root_index].appearance();
        let clip = out.clip_mask(&root_layer.layer_id, report.steps[root_index].extent)?;
        let layer = out.layer(
            root_node,
            root_appearance,
            root_layer,
            root_features.as_ref().and_then(|f| f.mask),
            clip,
        )?;
        root_node = out.push(PlanNode::Group(Group {
            isolated: true,
            layers: vec![layer],
        }))?;
        if let Some(f) = &root_features {
            let p = EntryParts::new(0, f, root_appearance.mask, None)?;
            if !p.is_empty() {
                entries.push(p.into_extension(root_node));
            }
        }
    }
    let render = RenderPlan {
        revision: plan.revision,
        target,
        nodes: out.nodes,
        root: root_node,
    };
    let operators = render
        .validate(&PlanLimits::default())
        .map_err(CompositeError::Plan)?;
    let mut exactness = ExactnessLedger::default();
    for op in operators {
        exactness.record(op.key(), op.intrinsic());
    }
    let extensions = PlanExtensions::from_layers(entries);
    if records.is_some() || !extensions.is_empty() {
        record_envelope(&render, &extensions, &mut exactness)?;
        for n in &notes {
            exactness.record(n.key, n.exactness);
        }
    }
    Ok(Lowered {
        lowering: RenderLowering {
            plan: render,
            sources: out.sources,
            mask_sources: out.mask_sources,
            exactness,
        },
        extensions,
        notes,
        text_masks: out.text_masks,
        vector_masks: out.vector_masks,
        image_sources: out.image_sources,
        vector_rows: out.vector_rows,
        clip_planes: out.clip_planes,
    })
}

/// The optional inputs of a render beyond the snapshot: raster records (envelope features), prepared
/// text and prepared vector layers.
#[derive(Clone, Copy, Default)]
pub struct RenderInputs<'a> {
    pub records: Option<&'a StudioRasterRecords>,
    pub text: Option<&'a crate::text::PreparedText>,
    pub vectors: Option<&'a crate::vector::PreparedVectors>,
}

/// The lowering of [`lower_with_inputs`]: the plan, its envelope and every key table the caller's
/// `PlanSources` serves from (raster tiles and masks by Folio record, text and vector coverage
/// planes by layer id, image paint tiles by source id).
#[derive(Clone, Debug, PartialEq)]
pub struct InputsLowering {
    pub plan: RenderPlan,
    pub extensions: PlanExtensions,
    pub sources: SourceTable,
    pub mask_sources: MaskTable,
    pub text_masks: crate::text::TextMaskTable,
    pub vector_masks: Vec<(SourceKey, String)>,
    pub image_sources: Vec<(SourceKey, String)>,
    /// `SourceKey` -> (vector layer, row): a stroke row's coverage mask or a painted row's tile.
    pub vector_rows: Vec<(SourceKey, String, usize)>,
    /// `SourceKey` -> the rasterised clip plane of a clipped layer (owned).
    pub clip_planes: Vec<(SourceKey, crate::vector::ClipPlane)>,
    pub exactness: ExactnessLedger,
    pub notes: Vec<LoweringNote>,
}
impl InputsLowering {
    /// The coverage mask behind `key` when it is a vector mask.
    pub fn vector_mask<'p>(
        &'p self,
        key: SourceKey,
        vectors: &'p crate::vector::PreparedVectors,
    ) -> Option<hsk_studio_pigment::Mask<'p>> {
        if let Some((_, layer)) = self.vector_masks.iter().find(|(k, _)| *k == key) {
            return vectors.plane(layer)?.mask();
        }
        if let Some((_, plane)) = self.clip_planes.iter().find(|(k, _)| *k == key) {
            return Some(plane.mask());
        }
        let (_, layer, row) = self.vector_rows.iter().find(|(k, _, _)| *k == key)?;
        vectors.plane(layer)?.stroke_mask(*row)
    }
    /// The painted (gradient or pattern) row tile behind `key`, in the plan's colour tag `space`.
    pub fn vector_paint<'p>(
        &self,
        key: SourceKey,
        vectors: &'p crate::vector::PreparedVectors,
        space: hsk_studio_pigment::render_plan::ColourTag,
    ) -> Option<hsk_studio_pigment::render_plan::TilePlanes<'p>> {
        let (_, layer, row) = self.vector_rows.iter().find(|(k, _, _)| *k == key)?;
        let plane = vectors.plane(layer)?;
        let crate::vector::PreparedPaint::Painted(t) = &plane.fills.get(*row)?.paint else {
            return None;
        };
        Some(hsk_studio_pigment::render_plan::TilePlanes {
            width: t.placement.width,
            height: t.placement.height,
            colour: &t.colour,
            colour_stride_bytes: u64::from(t.placement.width).checked_mul(12)?,
            alpha: &t.alpha,
            alpha_stride_bytes: u64::from(t.placement.width).checked_mul(4)?,
            colour_sha256: t.colour_sha256,
            alpha_sha256: t.alpha_sha256,
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space,
        })
    }
    /// The image paint source id behind `key`.
    pub fn image_source(&self, key: SourceKey) -> Option<&str> {
        self.image_sources
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, s)| s.as_str())
    }
    fn of(lowered: Lowered) -> Self {
        Self {
            plan: lowered.lowering.plan,
            extensions: lowered.extensions,
            sources: lowered.lowering.sources,
            mask_sources: lowered.lowering.mask_sources,
            text_masks: lowered.text_masks,
            vector_masks: lowered.vector_masks,
            image_sources: lowered.image_sources,
            vector_rows: lowered.vector_rows,
            clip_planes: lowered.clip_planes,
            exactness: lowered.lowering.exactness,
            notes: lowered.notes,
        }
    }
}

/// [`crate::evaluate`] with every optional input ([`RenderInputs`]): raster records as
/// [`crate::evaluate_extended`], prepared text as [`crate::evaluate_with_text`], prepared vector
/// layers and boolean groups (crate::vector) Ready with their plane as extent.
pub fn evaluate_with_inputs(
    plan: &LoweredPlan,
    snap: &Snapshot,
    inputs: &RenderInputs<'_>,
    cancel: &hsk_studio_accord::CancellationToken,
) -> Result<EvalReport, CompositeError> {
    if inputs
        .records
        .is_some_and(|r| r.document_id != snap.document().document_id)
    {
        return Err(CompositeError::UnsupportedDescriptor(
            "raster_records_document_mismatch".into(),
        ));
    }
    crate::evaluate_static::evaluate_with(
        plan,
        snap,
        inputs.records,
        inputs.text,
        inputs.vectors,
        cancel,
    )
}

/// [`lower_to_render_plan`] with every optional input ([`RenderInputs`]); evaluate with
/// [`evaluate_with_inputs`] and the same inputs.
pub fn lower_with_inputs(
    plan: &LoweredPlan,
    report: &EvalReport,
    snap: &Snapshot,
    inputs: &RenderInputs<'_>,
    root: &NodeAddr,
    target: OutputTarget,
    info: &dyn TileInfoSource,
) -> Result<InputsLowering, CompositeError> {
    if inputs
        .records
        .is_some_and(|r| r.document_id != snap.document().document_id)
    {
        return Err(CompositeError::UnsupportedDescriptor(
            "raster_records_document_mismatch".into(),
        ));
    }
    lower_inner(
        plan,
        report,
        snap,
        inputs.records,
        root,
        target,
        info,
        inputs.text,
        inputs.vectors,
    )
    .map(InputsLowering::of)
}

/// Folio-free plan of one prepared vector layer as a neutral root: the node layout a document
/// lowering emits for it (renderer tests execute this; renderers may not depend on Folio).
/// `Unavailable <layer>:vector_layer_not_prepared | vector_layer_empty`.
pub fn vector_layer_render_plan(
    vectors: &crate::vector::PreparedVectors,
    layer_id: &str,
    revision: u64,
    target: OutputTarget,
) -> Result<InputsLowering, CompositeError> {
    let plane = vectors.plane(layer_id).ok_or_else(|| {
        CompositeError::Unavailable(format!("{layer_id}:vector_layer_not_prepared"))
    })?;
    let none = |_: &TileRef| None;
    let mut out = Emitter {
        nodes: Vec::new(),
        sources: Vec::new(),
        mask_sources: Vec::new(),
        text_masks: Vec::new(),
        vector_masks: Vec::new(),
        image_sources: Vec::new(),
        vector_rows: Vec::new(),
        clips: Some(vectors),
        clip_planes: Vec::new(),
        next_key: 0,
        info: &none,
    };
    let mut entries = Vec::new();
    let mut root = out.vector(plane, &mut entries)?.ok_or_else(|| {
        CompositeError::Unavailable(format!("{layer_id}:vector_layer_empty"))
    })?;
    // A clipped layer (PreparedVectors::clip): one isolated wrapper layer carries the clip mask.
    if let Some(mask) = out.clip_mask(layer_id, plane.placement)? {
        root = out.push(PlanNode::Group(Group {
            isolated: true,
            layers: vec![Layer {
                input: root,
                mode: StudioBlendMode::Normal,
                opacity: 1.0,
                fill_opacity: 1.0,
                mask: Some(mask),
            }],
        }))?;
    }
    let render = RenderPlan {
        revision,
        target,
        nodes: out.nodes,
        root,
    };
    let operators = render
        .validate(&PlanLimits::default())
        .map_err(CompositeError::Plan)?;
    let mut exactness = ExactnessLedger::default();
    for op in operators {
        exactness.record(op.key(), op.intrinsic());
    }
    let extensions = PlanExtensions::from_layers(entries);
    if !extensions.is_empty() {
        record_envelope(&render, &extensions, &mut exactness)?;
    }
    Ok(InputsLowering {
        plan: render,
        extensions,
        sources: out.sources,
        mask_sources: out.mask_sources,
        text_masks: out.text_masks,
        vector_masks: out.vector_masks,
        image_sources: out.image_sources,
        vector_rows: out.vector_rows,
        clip_planes: out.clip_planes,
        exactness,
        notes: Vec::new(),
    })
}
