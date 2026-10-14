//! Composition scenes (lane SMB-COMPOSITE-COMPOSITION-ADAPTER, After Effects-style "play a stored
//! composition"). A [`SceneComposition`] is ONE evaluated frame of a composition: its layers
//! bottom-first (STU-MOT-009), each with its content, blend mode, opacities and one matrix that maps
//! the content's plan coordinates to composition pixels (anchor, scale, rotation, position and the
//! parent chain already applied, STU-CMP-010 step 5). Scenes are folio-free and motion-free: the
//! Folio + motion adapter (`temporal::composition::CompositionTemporalSource`, feature `motion`)
//! evaluates `StudioCompositionRecords` at a tick into a scene, and renderer tests build scenes
//! directly (decision B1).
//!
//! [`lower_scene`] lowers a scene to pigment's render plan: one isolated root group over the
//! composition rect `(0, 0, width, height)` (transparent backdrop), an optional background `Solid`
//! at the bottom, then one `Layer` per scene layer. A content matrix that is an exact integer
//! translation moves the tiles (exact, no envelope); any other invertible matrix lowers to
//! pigment's layer-transform operator with the scene's DECLARED resample filter (never a silent
//! default; a resample of a layer lowered to several tiles is refused
//! `transform_multi_tile_layer`, as in `temporal`); a singular matrix (a scale of 0) renders
//! nothing. [`render_scene`] lowers and executes the plan through a caller-chosen
//! `RenderBackend` into one full-frame surface.
//!
//! Everything a scene carries arrives from a caller and is bounded before use: layer count
//! ([`SceneLimits::max_layers`]), surface bytes ([`SceneLimits::max_surface_bytes`], charged
//! before allocation), non-finite or out-of-range opacities (`InvalidState`), cancellation between
//! layers. Integer arithmetic is checked (workflow V4-01).
#![deny(clippy::arithmetic_side_effects)]

use crate::CompositeError;
use crate::temporal::{TRANSFORM_MULTI_TILE_LAYER, TemporalError};
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::blend_math::{ExactnessLedger, StudioBlendMode};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, ColourTag, ExecuteRequest, Group, Layer, LayerExtension,
    OutputTarget, PlanExtensions, PlanLimits, PlanNode, PlanNodeId, PlanSources, RenderBackend,
    RenderPlan, RenderReceipt, SampleFormat, SinkError, SourceKey, TileInput, TilePlanes, TileSink,
};
use hsk_studio_pigment::transform_math::{Affine2, LayerTransform, ResampleFilter};
use hsk_studio_pigment::{Mask, Rect};
use std::collections::BTreeMap;

/// Default bound on the layers of one scene (Folio's `MAX_COMPOSITION_LAYERS`).
pub const DEFAULT_MAX_SCENE_LAYERS: usize = 4096;
/// Default bound on the bytes of every surface one render allocates (output frame included).
pub const DEFAULT_MAX_SURFACE_BYTES: u64 = 1 << 32;
/// Bytes per pixel of a rendered surface: `rgb_f32le` colour (12) + `f32le` alpha (4).
pub const SURFACE_BYTES_PER_PIXEL: u64 = 16;

/// One caller tile of a layer's content, placed in the content's plan coordinates. `tile_id` is
/// the caller's handle; [`ScenePixels::tile`] resolves it at execution.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneTile {
    pub tile_id: String,
    pub placement: Rect,
    pub format: SampleFormat,
    pub alpha: AlphaConvention,
    pub colour_sha256: [u8; 32],
    pub alpha_sha256: [u8; 32],
}

/// What a scene layer draws.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum SceneContent {
    /// Nothing (a null layer: a parent or expression target, STU-MOT-005).
    None,
    /// Raster tiles in the content's plan coordinates.
    Tiles(Vec<SceneTile>),
}

/// One evaluated layer of a frame.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneLayer {
    pub layer_id: String,
    pub content: SceneContent,
    pub mode: StudioBlendMode,
    /// Authored opacity times the animated opacity, `0.0..=1.0` (STU-CMP-010 step 6).
    pub opacity: f64,
    /// Authored fill opacity, `0.0..=1.0`.
    pub fill_opacity: f64,
    /// Content plan coordinates -> composition pixels.
    pub matrix: Affine2,
}

/// One evaluated frame of a composition.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneComposition {
    pub composition_id: String,
    pub width: u32,
    pub height: u32,
    /// Straight RGBA in plan units, drawn under every layer over the composition rect.
    pub background: Option<[f32; 4]>,
    /// The resample filter the composition declares for its transformed layers (STU-MOT-007).
    pub filter: ResampleFilter,
    /// Bottom-first (index 0 composited first).
    pub layers: Vec<SceneLayer>,
}

impl SceneComposition {
    /// The composition rect in plan pixels.
    pub fn rect(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
        }
    }
}

/// Caller resolution of [`SceneTile::tile_id`] to immutable planes (already in the plan's tag).
pub trait ScenePixels: Sync {
    fn tile(&self, tile_id: &str) -> Option<TilePlanes<'_>>;
}

/// Bounds of one scene lowering / render (hostile-input bounds, V4-02).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SceneLimits {
    pub max_layers: usize,
    pub max_surface_bytes: u64,
}

impl Default for SceneLimits {
    fn default() -> Self {
        Self {
            max_layers: DEFAULT_MAX_SCENE_LAYERS,
            max_surface_bytes: DEFAULT_MAX_SURFACE_BYTES,
        }
    }
}

/// Everything [`lower_scene`] / [`render_scene`] needs besides the scene.
#[derive(Clone, Copy)]
pub struct SceneContext<'a> {
    pub backend: &'a dyn RenderBackend,
    pub pixels: &'a dyn ScenePixels,
    /// `expected_revision` becomes every plan's revision.
    pub request: ExecuteRequest,
    /// Colour tag of every source and of the output (never converted).
    pub space: ColourTag,
    pub limits: SceneLimits,
}

/// The owned source table of a lowered scene: plan keys -> caller tiles.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SceneSources {
    pub tiles: BTreeMap<SourceKey, String>,
}

impl SceneSources {
    /// A `PlanSources` over these keys and the caller's pixels.
    pub fn bind<'a>(&'a self, pixels: &'a dyn ScenePixels) -> BoundSources<'a> {
        BoundSources {
            table: self,
            pixels,
        }
    }
}

/// [`SceneSources`] bound to the caller's pixels.
pub struct BoundSources<'a> {
    table: &'a SceneSources,
    pixels: &'a dyn ScenePixels,
}

impl PlanSources for BoundSources<'_> {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        self.pixels.tile(self.table.tiles.get(&key)?)
    }
    fn mask(&self, _key: SourceKey) -> Option<Mask<'_>> {
        None
    }
}

/// A scene lowered to pigment's IR.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneLowering {
    pub plan: RenderPlan,
    /// One `LayerExtension::transform` per resampled layer (empty: the plain plan).
    pub extensions: PlanExtensions,
    pub sources: SceneSources,
    pub exactness: ExactnessLedger,
    /// Layers that render, bottom-first.
    pub rendered: Vec<String>,
    /// Layers that render nothing in this frame (opacity 0, singular matrix, no content).
    pub skipped: Vec<String>,
}

/// One rendered frame: the composition rect, straight alpha, tightly packed planes.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderedScene {
    pub width: u32,
    pub height: u32,
    /// `rgb_f32le`, `width * height * 12` bytes.
    pub colour: Vec<u8>,
    /// `f32le` straight alpha, `width * height * 4` bytes.
    pub alpha: Vec<u8>,
    pub receipt: RenderReceipt,
    pub exactness: ExactnessLedger,
}

impl RenderedScene {
    /// Straight RGBA of pixel `(x, y)` of the frame.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[f32; 4]> {
        let i = usize::try_from(u64::from(y).checked_mul(u64::from(self.width))?.checked_add(u64::from(x))?).ok()?;
        let f = |bytes: &[u8], at: usize| -> Option<f32> {
            let s = bytes.get(at..at.checked_add(4)?)?;
            Some(f32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        };
        let c = i.checked_mul(12)?;
        Some([
            f(&self.colour, c)?,
            f(&self.colour, c.checked_add(4)?)?,
            f(&self.colour, c.checked_add(8)?)?,
            f(&self.alpha, i.checked_mul(4)?)?,
        ])
    }

    /// SHA-256 over the colour plane then the alpha plane (the frame digest of the tests).
    pub fn digest(&self) -> [u8; 32] {
        let mut bytes = Vec::with_capacity(self.colour.len().saturating_add(self.alpha.len()));
        bytes.extend_from_slice(&self.colour);
        bytes.extend_from_slice(&self.alpha);
        hsk_studio_prism::profile_hash(&bytes)
    }
}

fn canceled() -> TemporalError {
    TemporalError::Composite(CompositeError::Canceled)
}

fn invalid(layer_id: &str, field: &'static str) -> TemporalError {
    TemporalError::InvalidState {
        layer_id: layer_id.to_owned(),
        field,
    }
}

/// How a content matrix moves the content.
enum Placement {
    /// Exact integer translation (tiles move, no resampling).
    Integer(i64, i64),
    /// Any other invertible matrix: resampled with the declared filter.
    Resample,
    /// Singular (a scale of 0 on an axis): nothing is rendered.
    Collapsed,
}

/// Largest magnitude an `f64` may have and still convert to `i64` exactly.
const I64_BOUND: f64 = 9_223_372_036_854_775_808.0; // 2^63

fn classify(layer_id: &str, m: &Affine2) -> Result<Placement, TemporalError> {
    if !m.m.iter().all(|v| v.is_finite()) {
        return Err(invalid(layer_id, "matrix"));
    }
    let [a, b, c, d, e, f] = m.m;
    if a == 1.0 && b == 0.0 && d == 0.0 && e == 1.0 && c.fract() == 0.0 && f.fract() == 0.0 {
        if !(-I64_BOUND..I64_BOUND).contains(&c) || !(-I64_BOUND..I64_BOUND).contains(&f) {
            return Err(CompositeError::Overflow.into());
        }
        // Exact: integers inside the i64 range.
        #[allow(clippy::cast_possible_truncation)]
        return Ok(Placement::Integer(c as i64, f as i64));
    }
    let det = a * e - b * d;
    if det == 0.0 {
        return Ok(Placement::Collapsed);
    }
    Ok(Placement::Resample)
}

struct Builder {
    nodes: Vec<PlanNode>,
    sources: SceneSources,
    next_key: u32,
}

impl Builder {
    fn push(&mut self, node: PlanNode) -> Result<PlanNodeId, TemporalError> {
        let id = u32::try_from(self.nodes.len()).map_err(|_| CompositeError::Overflow)?;
        self.nodes.push(node);
        Ok(PlanNodeId(id))
    }
    fn key(&mut self) -> Result<SourceKey, TemporalError> {
        let k = SourceKey(self.next_key);
        self.next_key = self.next_key.checked_add(1).ok_or(CompositeError::Overflow)?;
        Ok(k)
    }
}

fn shifted(r: Rect, (dx, dy): (i64, i64)) -> Result<Rect, TemporalError> {
    Ok(Rect {
        x: r.x.checked_add(dx).ok_or(CompositeError::Overflow)?,
        y: r.y.checked_add(dy).ok_or(CompositeError::Overflow)?,
        ..r
    })
}

fn surface_bytes(width: u32, height: u32) -> Result<u64, TemporalError> {
    u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|p| p.checked_mul(SURFACE_BYTES_PER_PIXEL))
        .ok_or_else(|| CompositeError::Overflow.into())
}

/// Lower one scene (module docs). Refuses before any work: more layers than
/// `limits.max_layers` (`BudgetExceeded`), an empty composition rect. Per layer: non-finite or
/// out-of-range opacities and non-finite matrices (`InvalidState`), a resample of a multi-tile
/// layer (`transform_multi_tile_layer`); pigment validates the plan and the envelope.
pub fn lower_scene(
    scene: &SceneComposition,
    ctx: &SceneContext<'_>,
    cancel: &CancellationToken,
) -> Result<SceneLowering, TemporalError> {
    cancel.check().map_err(|_| canceled())?;
    if scene.layers.len() > ctx.limits.max_layers {
        return Err(CompositeError::BudgetExceeded.into());
    }
    if scene.width == 0 || scene.height == 0 {
        return Err(invalid(&scene.composition_id, "composition_size"));
    }
    let mut out = Builder {
        nodes: Vec::new(),
        sources: SceneSources::default(),
        next_key: 0,
    };
    let mut root_layers: Vec<Layer> = Vec::new();
    // (root layer index, transform) per resampled layer.
    let mut transforms: Vec<(usize, LayerTransform)> = Vec::new();
    let mut rendered = Vec::new();
    let mut skipped = Vec::new();
    if let Some(rgba) = scene.background {
        if !rgba.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
            return Err(invalid(&scene.composition_id, "background"));
        }
        let id = out.push(PlanNode::Solid {
            rect: scene.rect(),
            rgba,
        })?;
        root_layers.push(Layer {
            input: id,
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            fill_opacity: 1.0,
            mask: None,
        });
    }
    for layer in &scene.layers {
        cancel.check().map_err(|_| canceled())?;
        let id = layer.layer_id.as_str();
        for (field, v) in [("opacity", layer.opacity), ("fill_opacity", layer.fill_opacity)] {
            if !v.is_finite() || !(0.0..=1.0).contains(&v) {
                return Err(invalid(id, field));
            }
        }
        let placement = classify(id, &layer.matrix)?;
        let tiles = match &layer.content {
            SceneContent::Tiles(t) if !t.is_empty() => t,
            _ => {
                skipped.push(id.to_owned());
                continue;
            }
        };
        if layer.opacity == 0.0 || matches!(placement, Placement::Collapsed) {
            skipped.push(id.to_owned());
            continue;
        }
        let shift = match placement {
            Placement::Integer(dx, dy) => (dx, dy),
            _ => (0, 0),
        };
        if matches!(placement, Placement::Resample) && tiles.len() != 1 {
            return Err(TemporalError::Unsupported {
                layer_id: id.to_owned(),
                feature: TRANSFORM_MULTI_TILE_LAYER,
            });
        }
        let mut tile_nodes = Vec::with_capacity(tiles.len());
        for t in tiles {
            let key = out.key()?;
            out.sources.tiles.insert(key, t.tile_id.clone());
            tile_nodes.push(out.push(PlanNode::Tile(TileInput {
                key,
                placement: shifted(t.placement, shift)?,
                format: t.format,
                alpha: t.alpha,
                colour_sha256: t.colour_sha256,
                alpha_sha256: t.alpha_sha256,
            }))?);
        }
        let input = match tile_nodes.as_slice() {
            [only] => *only,
            many => {
                let layers = many
                    .iter()
                    .map(|n| Layer {
                        input: *n,
                        mode: StudioBlendMode::Normal,
                        opacity: 1.0,
                        fill_opacity: 1.0,
                        mask: None,
                    })
                    .collect();
                out.push(PlanNode::Group(Group {
                    isolated: true,
                    layers,
                }))?
            }
        };
        if matches!(placement, Placement::Resample) {
            transforms.push((
                root_layers.len(),
                LayerTransform::new(layer.matrix, scene.filter),
            ));
        }
        // Opacities are validated `0..=1`, so the narrowing keeps them in range.
        #[allow(clippy::cast_possible_truncation)]
        root_layers.push(Layer {
            input,
            mode: layer.mode,
            opacity: layer.opacity as f32,
            fill_opacity: layer.fill_opacity as f32,
            mask: None,
        });
        rendered.push(id.to_owned());
    }
    let root = out.push(PlanNode::Group(Group {
        isolated: true,
        layers: root_layers,
    }))?;
    let plan = RenderPlan {
        revision: ctx.request.expected_revision,
        target: OutputTarget {
            extent: scene.rect(),
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: ctx.space,
        },
        nodes: out.nodes,
        root,
    };
    let operators = plan
        .validate(&PlanLimits::default())
        .map_err(CompositeError::Plan)?;
    let mut exactness = ExactnessLedger::default();
    for op in operators {
        exactness.record(op.key(), op.intrinsic());
    }
    let mut entries = Vec::with_capacity(transforms.len());
    for (index, transform) in transforms {
        let li = u32::try_from(index).map_err(|_| CompositeError::Overflow)?;
        entries.push(LayerExtension::new(root, li).with_transform(transform));
    }
    let extensions = PlanExtensions::from_layers(entries);
    if !extensions.is_empty() {
        let ops = extensions.validate(&plan).map_err(CompositeError::Plan)?;
        for op in ops {
            exactness.record(op.key(), op.intrinsic());
        }
    }
    Ok(SceneLowering {
        plan,
        extensions,
        sources: out.sources,
        exactness,
        rendered,
        skipped,
    })
}

/// Full-frame sink: chunks are copied into the composition-sized planes.
struct FrameSink {
    width: u32,
    height: u32,
    colour: Vec<u8>,
    alpha: Vec<u8>,
}

impl FrameSink {
    fn new(width: u32, height: u32) -> Result<Self, TemporalError> {
        let px = usize::try_from(u64::from(width).checked_mul(u64::from(height)).ok_or(CompositeError::Overflow)?)
            .map_err(|_| CompositeError::Overflow)?;
        Ok(Self {
            width,
            height,
            colour: vec![0; px.checked_mul(12).ok_or(CompositeError::Overflow)?],
            alpha: vec![0; px.checked_mul(4).ok_or(CompositeError::Overflow)?],
        })
    }
}

impl TileSink for FrameSink {
    fn accept(&mut self, rect: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let bad = SinkError::Rejected;
        let (x0, y0) = (u32::try_from(rect.x).map_err(|_| bad)?, u32::try_from(rect.y).map_err(|_| bad)?);
        let x1 = x0.checked_add(rect.width).ok_or(bad)?;
        let y1 = y0.checked_add(rect.height).ok_or(bad)?;
        if x1 > self.width || y1 > self.height {
            return Err(bad);
        }
        let w = usize::try_from(rect.width).map_err(|_| bad)?;
        let fw = usize::try_from(self.width).map_err(|_| bad)?;
        for row in 0..usize::try_from(rect.height).map_err(|_| bad)? {
            let src = row.checked_mul(w).ok_or(bad)?;
            let dst = usize::try_from(y0)
                .ok()
                .and_then(|y| y.checked_add(row))
                .and_then(|y| y.checked_mul(fw))
                .and_then(|v| v.checked_add(usize::try_from(x0).ok()?))
                .ok_or(bad)?;
            let span = |i: usize, n: usize| -> Option<std::ops::Range<usize>> {
                let a = i.checked_mul(n)?;
                Some(a..a.checked_add(w.checked_mul(n)?)?)
            };
            let (Some(cs), Some(cd), Some(as_), Some(ad)) = (span(src, 12), span(dst, 12), span(src, 4), span(dst, 4)) else {
                return Err(bad);
            };
            self.colour
                .get_mut(cd)
                .ok_or(bad)?
                .copy_from_slice(colour.get(cs).ok_or(bad)?);
            self.alpha
                .get_mut(ad)
                .ok_or(bad)?
                .copy_from_slice(alpha.get(as_).ok_or(bad)?);
        }
        Ok(())
    }
}

/// Lower and execute one scene with the context's backend into a full-frame surface (straight
/// alpha). The frame's bytes are charged against `limits.max_surface_bytes` before allocation;
/// a backend refusal is `TemporalError::Render` with the backend's stable code.
pub fn render_scene(
    scene: &SceneComposition,
    ctx: &SceneContext<'_>,
    cancel: &CancellationToken,
) -> Result<RenderedScene, TemporalError> {
    if surface_bytes(scene.width, scene.height)? > ctx.limits.max_surface_bytes {
        return Err(CompositeError::BudgetExceeded.into());
    }
    let lowering = lower_scene(scene, ctx, cancel)?;
    let mut sink = FrameSink::new(scene.width, scene.height)?;
    let sources = lowering.sources.bind(ctx.pixels);
    let receipt = ctx
        .backend
        .execute_extended(
            &lowering.plan,
            &lowering.extensions,
            &ctx.request,
            &sources,
            cancel,
            &mut sink,
        )
        .map_err(|e| TemporalError::Render {
            composition_id: scene.composition_id.clone(),
            code: e.code(),
        })?;
    let mut exactness = lowering.exactness;
    for op in &receipt.exactness.operators {
        exactness.record(op.key, op.exactness);
    }
    Ok(RenderedScene {
        width: sink.width,
        height: sink.height,
        colour: sink.colour,
        alpha: sink.alpha,
        receipt,
        exactness,
    })
}
