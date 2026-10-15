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
//! Precompositions ([`SceneContent::Precomp`], STU-MOT-005 `composition`): the nested scene (already
//! evaluated at the layer's mapped source tick) is RENDERED first by the same backend into a
//! surface of the nested composition's size (content outside the nested rect is clipped, and the
//! nested composition has no background unless its scene carries one), and the surface is then
//! placed like a one-tile raster layer with the layer's matrix, opacity and blend mode
//! (STU-CMP-010: source resolution first, transform after). Recursion is bounded by
//! [`SceneLimits::max_depth`] and every rendered surface counts against
//! [`SceneLimits::max_surfaces`] and [`SceneLimits::max_surface_bytes`] BEFORE it is allocated.
//!
//! Track mattes ([`SceneMatte`], STU-CMP-030): the matte layer is rendered ALONE (its content,
//! transform, opacity and its own matte; never its blend mode) over the composition rect by the
//! same backend; its alpha (alpha matte) or `LUMA_WEIGHTS . rgb * alpha` (luma matte) becomes a
//! `coverage_u16le` plane (`round(v * 65535)`) applied as the matted layer's coverage mask, inverted
//! when the matte is inverted (pigment's mask invert), so it multiplies after transform and opacity
//! and before the blend (STU-CMP-010 step 7). The exactness ledger records `track_matte`
//! Approximate (16-bit quantisation; the luma weights and space are a Studio reading). A matte
//! layer absent at the tick is coverage 0: the matted layer renders nothing, or everything when
//! inverted.
//!
//! Everything a scene carries arrives from a caller and is bounded before use: layer count
//! ([`SceneLimits::max_layers`]), nesting depth, surface count and bytes, non-finite or
//! out-of-range opacities (`InvalidState`), cancellation between layers. Integer arithmetic is
//! checked (workflow V4-01).
#![deny(clippy::arithmetic_side_effects)]

use crate::CompositeError;
use crate::temporal::{TRANSFORM_MULTI_TILE_LAYER, TemporalError};
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::blend_math::{Exactness, ExactnessLedger, StudioBlendMode};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, ColourTag, ExecuteRequest, Group, Layer, LayerExtension, MaskFormat,
    MaskInput, OutputTarget, PlanExtensions, PlanLimits, PlanNode, PlanNodeId, PlanSources, RenderBackend, RenderPlan,
    RenderReceipt, SampleFormat, SinkError, SourceKey, TileInput, TilePlanes, TileSink,
};
use hsk_studio_pigment::adjust_math::AdjustmentOp;
use hsk_studio_pigment::transform_math::{Affine2, LayerTransform, ResampleFilter};
use hsk_studio_pigment::{Mask, Rect};
use std::collections::BTreeMap;

/// Default bound on the layers of one scene (Folio's `MAX_COMPOSITION_LAYERS`).
pub const DEFAULT_MAX_SCENE_LAYERS: usize = 4096;
/// Default bound on the bytes of every surface one render allocates (output frame included).
pub const DEFAULT_MAX_SURFACE_BYTES: u64 = 1 << 32;
/// Default bound on precomposition nesting below the rendered composition.
pub const DEFAULT_MAX_SCENE_DEPTH: usize = 16;
/// Default bound on the surfaces one render allocates (output frame included).
pub const DEFAULT_MAX_SURFACES: usize = 256;
/// Bytes per pixel of a rendered surface: `rgb_f32le` colour (12) + `f32le` alpha (4).
pub const SURFACE_BYTES_PER_PIXEL: u64 = 16;
/// Bytes per pixel of a track-matte coverage plane (`coverage_u16le`).
pub const MATTE_BYTES_PER_PIXEL: u64 = 2;
/// Refusal keys of composition layers composite cannot lower.
pub const ADJUSTMENT_TRANSFORM: &str = "composition_adjustment_transform";
pub const ADJUSTMENT_MATTE: &str = "composition_adjustment_matte";
pub const CLIP_WITHOUT_BASE: &str = "composition_clip_without_base";
pub const CLIP_BASE_ADJUSTMENT: &str = "composition_clip_base_adjustment";
pub const CLIPPED_MATTE: &str = "composition_clipped_matte";
/// Exactness-ledger key of a track matte: the matte coverage is quantised to 16 bits and the
/// luma weights are a Studio reading, so the class is `Approximate`.
pub const TRACK_MATTE_KEY: &str = "track_matte";
/// Luma weights of a luma matte (ITU-R BT.709 on the plan's colour values; After Effects'
/// weights and space are UNVERIFIED, D5 oracle).
pub const LUMA_WEIGHTS: [f32; 3] = [0.2126, 0.7152, 0.0722];

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
    /// A nested composition evaluated at the layer's source tick; its content rect is
    /// `(0, 0, width, height)` of the nested composition.
    Precomp(Box<SceneComposition>),
    /// A static layer subtree (masks, effects, blur, vector and text planes; the document's static
    /// lowering with the layer's opacity and blend left to the composition), rendered into a
    /// surface over `StaticContent::rect` and transformed like a raster layer.
    Static(Box<StaticContent>),
    /// An adjustment layer (STU-MOT-005 `adjustment`): pigment's adjustment over everything below
    /// it in the composition, on a transparent placeholder over the composition rect.
    Adjustment(AdjustmentOp),
    /// Content under a corner pin (lane SMB-AE-KEY-TRACK-3; STU-CMP-065 four-point corner pin):
    /// [`PinnedContent::pin`] maps the inner content's plan coordinates projectively before the
    /// layer matrix, so the layer always resamples through `pin` then `matrix` (pigment
    /// `LayerTransform::projective`). The inner content is tiles, a static subtree or a
    /// precomposition (else `composition_corner_pin_content`).
    Pinned(Box<PinnedContent>),
    /// Content shaded by 3D lights (lane SMB-AE-3D, STU-CMP-045; [`crate::three_d::LitContent`]):
    /// the inner content is rendered over its rect, shaded per texel by
    /// [`crate::three_d::shade`], then placed like a tile (inside a 3D run's pinned layer).
    Lit(Box<crate::three_d::LitContent>),
}

/// The inner content and the pin of [`SceneContent::Pinned`].
#[derive(Clone, Debug, PartialEq)]
pub struct PinnedContent {
    pub content: SceneContent,
    /// Content plan coordinates -> content plan coordinates (the pinned quad).
    pub pin: hsk_studio_pigment::transform_math::Projective2,
}

/// A caller-independent coverage plane a static subtree reads (text, vector and stroke coverage).
#[derive(Clone, Debug, PartialEq)]
pub struct OwnedMask {
    pub bounds: Rect,
    pub sample_format: String,
    pub bytes: Vec<u8>,
    pub stride_bytes: u64,
    pub sha256: [u8; 32],
}

/// A static layer subtree lowered by composite's static path (see [`SceneContent::Static`]).
#[derive(Clone, Debug, PartialEq)]
pub struct StaticContent {
    pub plan: RenderPlan,
    pub extensions: PlanExtensions,
    /// The surface the plan renders: the layer's content extent grown by its effect and blur
    /// support (the plan's target extent).
    pub rect: Rect,
    /// Plan keys -> caller tile ids ([`ScenePixels::tile`]: raster tiles, image paints).
    pub tiles: BTreeMap<SourceKey, String>,
    /// Plan keys -> caller mask ids ([`ScenePixels::mask`]: Folio mask tiles).
    pub masks: BTreeMap<SourceKey, String>,
    /// Plan keys -> owned coverage planes (text, vector fill and stroke coverage).
    pub owned_masks: BTreeMap<SourceKey, OwnedMask>,
    /// Plan keys -> owned tiles (vector gradient and pattern rows).
    pub owned_tiles: BTreeMap<SourceKey, Surface>,
}

struct StaticSources<'a> {
    content: &'a StaticContent,
    pixels: &'a dyn ScenePixels,
}

impl PlanSources for StaticSources<'_> {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        if let Some(s) = self.content.owned_tiles.get(&key) {
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
                space: self.content.plan.target.space,
            });
        }
        self.pixels.tile(self.content.tiles.get(&key)?)
    }
    fn mask(&self, key: SourceKey) -> Option<Mask<'_>> {
        if let Some(m) = self.content.owned_masks.get(&key) {
            return Some(Mask {
                bounds: m.bounds,
                sample_format: &m.sample_format,
                bytes: &m.bytes,
                stride_bytes: m.stride_bytes,
                sha256: m.sha256,
            });
        }
        self.pixels.mask(self.content.masks.get(&key)?)
    }
}

/// Which channel of the rendered matte layer becomes the coverage (STU-CMP-030 `channel`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneMatteMode {
    /// The matte layer's alpha.
    Alpha,
    /// The matte layer's luminance over transparent black: `LUMA_WEIGHTS . rgb * alpha`.
    Luma,
}

/// A track matte (STU-CMP-030): ANOTHER layer, rendered alone with its own transform and opacity
/// over the composition rect, whose alpha or luma becomes this layer's coverage.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneMatte {
    /// The matte layer at this tick; `None` = it does not exist at this tick (coverage 0, which
    /// an inverted matte turns into full coverage).
    pub layer: Option<Box<SceneLayer>>,
    pub mode: SceneMatteMode,
    pub inverted: bool,
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
    /// Track matte applied after transform and opacity, before the blend (STU-CMP-010 step 7).
    pub matte: Option<SceneMatte>,
    /// Clipped to the nearest earlier unclipped content layer of the composition (Folio
    /// `clipped`; pigment's clip-to-base route, Approximate).
    pub clipped: bool,
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
    /// A caller coverage plane of a static subtree ([`StaticContent::masks`]). Default: none.
    fn mask(&self, mask_id: &str) -> Option<Mask<'_>> {
        let _ = mask_id;
        None
    }
}

/// Bounds of one scene lowering / render (hostile-input bounds, V4-02).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SceneLimits {
    pub max_layers: usize,
    pub max_surface_bytes: u64,
    pub max_depth: usize,
    pub max_surfaces: usize,
}

impl Default for SceneLimits {
    fn default() -> Self {
        Self {
            max_layers: DEFAULT_MAX_SCENE_LAYERS,
            max_surface_bytes: DEFAULT_MAX_SURFACE_BYTES,
            max_depth: DEFAULT_MAX_SCENE_DEPTH,
            max_surfaces: DEFAULT_MAX_SURFACES,
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

/// A surface composite rendered and owns (a precomposition), straight alpha.
#[derive(Clone, Debug, PartialEq)]
pub struct Surface {
    pub width: u32,
    pub height: u32,
    pub colour: Vec<u8>,
    pub alpha: Vec<u8>,
    pub colour_sha256: [u8; 32],
    pub alpha_sha256: [u8; 32],
}

impl Surface {
    fn from_rendered(r: RenderedScene) -> Self {
        Self {
            colour_sha256: hsk_studio_prism::profile_hash(&r.colour),
            alpha_sha256: hsk_studio_prism::profile_hash(&r.alpha),
            width: r.width,
            height: r.height,
            colour: r.colour,
            alpha: r.alpha,
        }
    }
}

/// A track-matte coverage plane composite owns (`coverage_u16le`, composition rect).
#[derive(Clone, Debug, PartialEq)]
pub struct MattePlane {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
    pub sha256: [u8; 32],
}

/// The owned source table of a lowered scene: plan keys -> caller tiles or owned surfaces.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneSources {
    pub space: ColourTag,
    pub tiles: BTreeMap<SourceKey, String>,
    pub surfaces: BTreeMap<SourceKey, Surface>,
    pub mattes: BTreeMap<SourceKey, MattePlane>,
}

impl SceneSources {
    fn new(space: ColourTag) -> Self {
        Self {
            space,
            tiles: BTreeMap::new(),
            surfaces: BTreeMap::new(),
            mattes: BTreeMap::new(),
        }
    }

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
        if let Some(s) = self.table.surfaces.get(&key) {
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
                space: self.table.space,
            });
        }
        self.pixels.tile(self.table.tiles.get(&key)?)
    }
    fn mask(&self, key: SourceKey) -> Option<Mask<'_>> {
        let m = self.table.mattes.get(&key)?;
        Some(Mask {
            bounds: Rect {
                x: 0,
                y: 0,
                width: m.width,
                height: m.height,
            },
            sample_format: "coverage_u16le",
            bytes: &m.bytes,
            stride_bytes: u64::from(m.width).checked_mul(2)?,
            sha256: m.sha256,
        })
    }
}

/// A scene lowered to pigment's IR.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneLowering {
    pub plan: RenderPlan,
    /// One `LayerExtension::transform` per resampled layer (empty: the plain plan).
    pub extensions: PlanExtensions,
    pub sources: SceneSources,
    /// The plan's operators plus those of every rendered precomposition.
    pub exactness: ExactnessLedger,
    /// Layers that render, bottom-first.
    pub rendered: Vec<String>,
    /// Layers that render nothing in this frame (opacity 0, singular matrix, no content).
    pub skipped: Vec<String>,
    /// Surfaces rendered while lowering (precompositions, recursively).
    pub surfaces_rendered: usize,
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
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = usize::try_from(
            u64::from(y)
                .checked_mul(u64::from(self.width))?
                .checked_add(u64::from(x))?,
        )
        .ok()?;
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

/// The clip base of the next clipped layer (see `lower_inner`).
#[derive(Clone, Copy)]
enum Base {
    Rendered,
    Skipped,
    Adjustment,
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
        self.next_key = self
            .next_key
            .checked_add(1)
            .ok_or(CompositeError::Overflow)?;
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

/// Running charges of one render (every surface, nested ones included).
#[derive(Default)]
struct Charges {
    bytes: u64,
    surfaces: usize,
}

impl Charges {
    /// Charge one surface before it is allocated.
    fn surface(&mut self, ctx: &SceneContext<'_>, width: u32, height: u32) -> Result<(), TemporalError> {
        self.charge(ctx, surface_bytes(width, height)?)
    }

    /// Charge one allocation of `bytes` (a surface or a matte plane) before it is made.
    fn charge(&mut self, ctx: &SceneContext<'_>, bytes: u64) -> Result<(), TemporalError> {
        let bytes = self.bytes.checked_add(bytes).ok_or(CompositeError::Overflow)?;
        let surfaces = self.surfaces.checked_add(1).ok_or(CompositeError::Overflow)?;
        if bytes > ctx.limits.max_surface_bytes || surfaces > ctx.limits.max_surfaces {
            return Err(CompositeError::BudgetExceeded.into());
        }
        self.bytes = bytes;
        self.surfaces = surfaces;
        Ok(())
    }
}

/// An unlinked vector mask (Folio `StudioVectorMaskParameters.linked == false`, STU-RAS-016; lane
/// SMB-COMPOSITE-VECTOR-MASK-2) of composition layer `layer_id` as an alpha track matte that stays
/// in composition space while the layer moves: the matte layer is a white `Solid` over the mask
/// plane (`crate::vector_mask`: source bounds united with the region, invert / region baked as in
/// the static lowering, no clip) cut by the plane, identity matrix, with the mask feather as its
/// envelope `mask_feather_px`; a pure invert is the matte's invert (full outside the plane). The
/// mask plane is in the layer's content coordinates, which are composition pixels under the
/// identity layer matrix (the unlinked mask stays where the unmoved layer's mask was). An empty
/// plane is an absent matte (coverage 0, or full when inverted). Density below 1 is refused
/// (`vector_mask_unlinked_density`: a matte has no density). `revision` = the plan revision the
/// scene request expects.
///
/// Lane SMB-COMPOSITE-DOCGRAPH-2: the plane is [`crate::vector_mask::unlinked_rect`] (source
/// bounds and region grown by the feather support), so the feather is not cut at the source rect
/// and its taps never clamp onto mask content; [`unlinked_mask_matte_in`] also covers the
/// composition when the mask is full outside its art without pigment's invert (a `reveal_outside`
/// luminance mask), where this form, with no composition rect, cuts at the plane.
pub fn unlinked_mask_matte(
    mask: &crate::vector_mask::VectorMask,
    layer_id: &str,
    space: ColourTag,
    revision: u64,
) -> Result<SceneMatte, CompositeError> {
    unlinked_matte(mask, layer_id, space, revision, None)
}

/// [`unlinked_mask_matte`] in a composition of rect `composition` (`(0, 0, width, height)`): a
/// mask whose value outside its art is 1 and not carried by pigment's invert flag
/// ([`crate::vector_mask::outside_value`]) gets a plane over the composition too, so the matte is
/// full wherever the mask is, as on the document-graph path.
pub fn unlinked_mask_matte_in(
    mask: &crate::vector_mask::VectorMask,
    layer_id: &str,
    space: ColourTag,
    revision: u64,
    composition: Rect,
) -> Result<SceneMatte, CompositeError> {
    unlinked_matte(mask, layer_id, space, revision, Some(composition))
}

fn unlinked_matte(
    mask: &crate::vector_mask::VectorMask,
    layer_id: &str,
    space: ColourTag,
    revision: u64,
    composition: Option<Rect>,
) -> Result<SceneMatte, CompositeError> {
    use crate::vector_mask::{REFUSE_MASK_UNLINKED_DENSITY, outside_value, plane, unlinked_rect};
    let unavailable = |code: &str| CompositeError::Unavailable(format!("{layer_id}:{code}"));
    if mask.density < 1.0 {
        return Err(unavailable(REFUSE_MASK_UNLINKED_DENSITY));
    }
    let mut rect = unlinked_rect(mask).map_err(unavailable)?;
    let content_outside = match &mask.source {
        crate::vector_mask::VectorMaskSource::Content { outside, .. } => *outside,
        crate::vector_mask::VectorMaskSource::Outline(_) => 0.0,
    };
    let pure_invert = mask.invert && mask.region.is_none() && content_outside == 0.0;
    if !pure_invert
        && outside_value(mask) != 0.0
        && let Some(c) = composition
    {
        rect = crate::evaluate_static::union(rect, c)?;
    }
    let Some(rect) = rect else {
        return Ok(SceneMatte { layer: None, mode: SceneMatteMode::Alpha, inverted: pure_invert });
    };
    let (coverage, inverted) = plane(mask, None, rect).map_err(unavailable)?;
    let sha256 = hsk_studio_prism::profile_hash(&coverage);
    let stride_bytes = u64::from(rect.width).checked_mul(2).ok_or(CompositeError::Overflow)?;
    let key = SourceKey(0);
    let nodes = vec![
        PlanNode::Solid { rect, rgba: [1.0, 1.0, 1.0, 1.0] },
        PlanNode::Group(Group {
            isolated: true,
            layers: vec![Layer {
                input: PlanNodeId(0),
                mode: StudioBlendMode::Normal,
                opacity: 1.0,
                fill_opacity: 1.0,
                mask: Some(MaskInput {
                    key,
                    placement: rect,
                    format: MaskFormat::CoverageU16Le,
                    sha256,
                    inverted: false,
                    density: 1.0,
                }),
            }],
        }),
    ];
    let plan = RenderPlan {
        revision,
        target: OutputTarget {
            extent: rect,
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space,
        },
        nodes,
        root: PlanNodeId(1),
    };
    plan.validate(&PlanLimits::default()).map_err(CompositeError::Plan)?;
    let extensions = if mask.feather_px > 0.0 {
        PlanExtensions::from_layers(vec![
            LayerExtension::new(PlanNodeId(1), 0).with_mask_feather(mask.feather_px),
        ])
    } else {
        PlanExtensions::default()
    };
    let mut owned_masks = BTreeMap::new();
    owned_masks.insert(
        key,
        OwnedMask {
            bounds: rect,
            sample_format: "coverage_u16le".into(),
            bytes: coverage,
            stride_bytes,
            sha256,
        },
    );
    let content = StaticContent {
        plan,
        extensions,
        rect,
        tiles: BTreeMap::new(),
        masks: BTreeMap::new(),
        owned_masks,
        owned_tiles: BTreeMap::new(),
    };
    Ok(SceneMatte {
        layer: Some(Box::new(SceneLayer {
            layer_id: format!("{layer_id}/unlinked-vector-mask"),
            content: SceneContent::Static(Box::new(content)),
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            fill_opacity: 1.0,
            matrix: Affine2::IDENTITY,
            matte: None,
            clipped: false,
        })),
        mode: SceneMatteMode::Alpha,
        inverted,
    })
}

/// Lower one scene (module docs). Refuses before any work: more layers than
/// `limits.max_layers` (`BudgetExceeded`), an empty composition rect. Per layer: non-finite or
/// out-of-range opacities and non-finite matrices (`InvalidState`), a resample of a multi-tile
/// layer (`transform_multi_tile_layer`); pigment validates the plan and the envelope. A scene with
/// precompositions renders them through `ctx.backend` while lowering.
pub fn lower_scene(
    scene: &SceneComposition,
    ctx: &SceneContext<'_>,
    cancel: &CancellationToken,
) -> Result<SceneLowering, TemporalError> {
    lower_inner(scene, ctx, cancel, 0, &mut Charges::default())
}

fn lower_inner(
    scene: &SceneComposition,
    ctx: &SceneContext<'_>,
    cancel: &CancellationToken,
    depth: usize,
    charges: &mut Charges,
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
        sources: SceneSources::new(ctx.space),
        next_key: 0,
    };
    let mut exactness = ExactnessLedger::default();
    let mut root_layers: Vec<Layer> = Vec::new();
    // (root layer index, transform) per resampled layer.
    let mut transforms: Vec<(usize, LayerTransform)> = Vec::new();
    // (root layer index, adjustment) per adjustment layer.
    let mut adjustments: Vec<(usize, AdjustmentOp)> = Vec::new();
    // The clip base so far: `None` = no unclipped content layer yet, else whether it was rendered
    // (`Some(true)`), skipped (`Some(false)`: its clipped layers drop with it) or an adjustment.
    let mut base: Option<Base> = None;
    let mut rendered = Vec::new();
    let mut skipped = Vec::new();
    let surfaces_before = charges.surfaces;
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
        // A corner pin (lane SMB-AE-KEY-TRACK-3): the inner content always resamples through the
        // pin then the matrix; a singular matrix still renders nothing.
        let (content, pin) = match &layer.content {
            SceneContent::Pinned(p) => (&p.content, Some(p.pin)),
            other => (other, None),
        };
        if pin.is_some() && !matches!(content, SceneContent::Tiles(_) | SceneContent::Static(_) | SceneContent::Precomp(_) | SceneContent::Lit(_)) {
            return Err(TemporalError::Unsupported {
                layer_id: id.to_owned(),
                feature: crate::track_apply::COMPOSITION_CORNER_PIN_CONTENT,
            });
        }
        let placement = match (pin, placement) {
            (Some(_), Placement::Integer(..)) => Placement::Resample,
            (_, p) => p,
        };
        if layer.clipped {
            if layer.matte.is_some() {
                return Err(TemporalError::Unsupported {
                    layer_id: id.to_owned(),
                    feature: CLIPPED_MATTE,
                });
            }
            match base {
                None => {
                    return Err(TemporalError::Unsupported {
                        layer_id: id.to_owned(),
                        feature: CLIP_WITHOUT_BASE,
                    });
                }
                Some(Base::Adjustment) => {
                    return Err(TemporalError::Unsupported {
                        layer_id: id.to_owned(),
                        feature: CLIP_BASE_ADJUSTMENT,
                    });
                }
                Some(Base::Skipped) => {
                    skipped.push(id.to_owned());
                    continue;
                }
                Some(Base::Rendered) => {}
            }
        }
        let has_content = match content {
            SceneContent::Tiles(t) => !t.is_empty(),
            SceneContent::Precomp(_) | SceneContent::Static(_) | SceneContent::Adjustment(_) | SceneContent::Lit(_) => true,
            SceneContent::None | SceneContent::Pinned(_) => false,
        };
        let matte_empty = layer
            .matte
            .as_ref()
            .is_some_and(|m| m.layer.is_none() && !m.inverted);
        if !has_content
            || layer.opacity == 0.0
            || matte_empty
            || matches!(placement, Placement::Collapsed)
        {
            if !layer.clipped && has_content {
                base = Some(Base::Skipped);
            }
            skipped.push(id.to_owned());
            continue;
        }
        if let SceneContent::Adjustment(op) = &layer.content {
            if layer.matte.is_some() {
                return Err(TemporalError::Unsupported {
                    layer_id: id.to_owned(),
                    feature: ADJUSTMENT_MATTE,
                });
            }
            let input = out.push(PlanNode::Solid {
                rect: scene.rect(),
                rgba: [0.0; 4],
            })?;
            // A transformed adjustment layer acts inside its own bounds only: the composition rect
            // (its layer space) through its matrix, rasterised by nib as the placeholder's
            // coverage over the composition rect (After Effects reading, UNVERIFIED).
            let mask = if layer.matrix.is_identity() {
                None
            } else {
                let plane = adjustment_bounds_plane(scene, id, &layer.matrix, ctx, charges)?;
                let key = out.key()?;
                let input = MaskInput {
                    key,
                    placement: scene.rect(),
                    format: MaskFormat::CoverageU16Le,
                    sha256: plane.sha256,
                    inverted: false,
                    density: 1.0,
                };
                out.sources.mattes.insert(key, plane);
                Some(input)
            };
            adjustments.push((root_layers.len(), op.clone()));
            base = Some(Base::Adjustment);
            #[allow(clippy::cast_possible_truncation)]
            root_layers.push(Layer {
                input,
                mode: layer.mode,
                opacity: layer.opacity as f32,
                fill_opacity: layer.fill_opacity as f32,
                mask,
            });
            rendered.push(id.to_owned());
            continue;
        }
        // (plan source key, tile input without key) of the layer's content.
        let mut inputs: Vec<(SourceKey, Rect, SceneTile)> = Vec::new();
        match content {
            SceneContent::Tiles(tiles) => {
                for t in tiles {
                    let key = out.key()?;
                    out.sources.tiles.insert(key, t.tile_id.clone());
                    inputs.push((key, t.placement, t.clone()));
                }
            }
            SceneContent::Precomp(nested) => {
                if depth >= ctx.limits.max_depth {
                    return Err(CompositeError::BudgetExceeded.into());
                }
                let depth = depth.checked_add(1).ok_or(CompositeError::Overflow)?;
                let r = render_inner(nested, ctx, cancel, depth, charges)?;
                for op in &r.exactness.operators {
                    exactness.record(op.key, op.exactness);
                }
                let surface = Surface::from_rendered(r);
                let key = out.key()?;
                let tile = SceneTile {
                    tile_id: format!("{}/precomp/{}", id, nested.composition_id),
                    placement: nested.rect(),
                    format: SampleFormat::RgbF32Le,
                    alpha: AlphaConvention::Straight,
                    colour_sha256: surface.colour_sha256,
                    alpha_sha256: surface.alpha_sha256,
                };
                out.sources.surfaces.insert(key, surface);
                inputs.push((key, nested.rect(), tile));
            }
            SceneContent::Static(content) => {
                let surface = render_static(id, content, ctx, cancel, charges, &mut exactness)?;
                let key = out.key()?;
                let tile = SceneTile {
                    tile_id: format!("{id}/static"),
                    placement: content.rect,
                    format: SampleFormat::RgbF32Le,
                    alpha: AlphaConvention::Straight,
                    colour_sha256: surface.colour_sha256,
                    alpha_sha256: surface.alpha_sha256,
                };
                out.sources.surfaces.insert(key, surface);
                inputs.push((key, content.rect, tile));
            }
            SceneContent::Lit(lit) => {
                let surface = lit_surface(id, lit, scene.filter, ctx, cancel, depth, charges, &mut exactness)?;
                let key = out.key()?;
                let tile = SceneTile {
                    tile_id: format!("{id}/lit"),
                    placement: lit.rect,
                    format: SampleFormat::RgbF32Le,
                    alpha: AlphaConvention::Straight,
                    colour_sha256: surface.colour_sha256,
                    alpha_sha256: surface.alpha_sha256,
                };
                out.sources.surfaces.insert(key, surface);
                inputs.push((key, lit.rect, tile));
            }
            SceneContent::None | SceneContent::Adjustment(_) | SceneContent::Pinned(_) => {}
        }
        let shift = match placement {
            Placement::Integer(dx, dy) => (dx, dy),
            _ => (0, 0),
        };
        if matches!(placement, Placement::Resample) && inputs.len() != 1 {
            return Err(TemporalError::Unsupported {
                layer_id: id.to_owned(),
                feature: TRANSFORM_MULTI_TILE_LAYER,
            });
        }
        let mut tile_nodes = Vec::with_capacity(inputs.len());
        for (key, placement, t) in inputs {
            tile_nodes.push(out.push(PlanNode::Tile(TileInput {
                key,
                placement: shifted(placement, shift)?,
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
            let transform = match pin {
                None => LayerTransform::new(layer.matrix, scene.filter),
                Some(p) => LayerTransform::projective(
                    p.then(&hsk_studio_pigment::transform_math::Projective2::from_affine(&layer.matrix)),
                    scene.filter,
                ),
            };
            transforms.push((root_layers.len(), transform));
        }
        let mask = match &layer.matte {
            None if layer.clipped => Some(MaskInput::clip_to_base()),
            None => None,
            Some(matte) => {
                let depth = depth.checked_add(1).ok_or(CompositeError::Overflow)?;
                let plane = matte_plane(scene, id, matte, ctx, cancel, depth, charges, &mut exactness)?;
                exactness.record(TRACK_MATTE_KEY, Exactness::Approximate);
                let key = out.key()?;
                let input = MaskInput {
                    key,
                    placement: scene.rect(),
                    format: MaskFormat::CoverageU16Le,
                    sha256: plane.sha256,
                    inverted: matte.inverted,
                    density: 1.0,
                };
                out.sources.mattes.insert(key, plane);
                Some(input)
            }
        };
        // Opacities are validated `0..=1`, so the narrowing keeps them in range.
        #[allow(clippy::cast_possible_truncation)]
        root_layers.push(Layer {
            input,
            mode: layer.mode,
            opacity: layer.opacity as f32,
            fill_opacity: layer.fill_opacity as f32,
            mask,
        });
        if !layer.clipped {
            base = Some(Base::Rendered);
        }
        rendered.push(id.to_owned());
    }
    if root_layers.is_empty() {
        // Nothing renders: a transparent fill keeps the root a valid, non-empty group.
        let id = out.push(PlanNode::Solid {
            rect: scene.rect(),
            rgba: [0.0; 4],
        })?;
        root_layers.push(Layer {
            input: id,
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            fill_opacity: 1.0,
            mask: None,
        });
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
    for op in operators {
        exactness.record(op.key(), op.intrinsic());
    }
    let mut entries = Vec::with_capacity(transforms.len().saturating_add(adjustments.len()));
    for (index, transform) in transforms {
        let li = u32::try_from(index).map_err(|_| CompositeError::Overflow)?;
        entries.push(LayerExtension::new(root, li).with_transform(transform));
    }
    for (index, op) in adjustments {
        let li = u32::try_from(index).map_err(|_| CompositeError::Overflow)?;
        entries.push(LayerExtension::new(root, li).with_adjustment(op));
    }
    entries.sort_by_key(|e| e.layer);
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
        surfaces_rendered: charges.surfaces.saturating_sub(surfaces_before),
    })
}

/// Coverage (`coverage_u16le` over the composition rect) of a transformed adjustment layer's
/// bounds: the composition rect through `matrix`, nib exact area coverage (8-bit, widened exactly).
fn adjustment_bounds_plane(
    scene: &SceneComposition,
    layer_id: &str,
    matrix: &Affine2,
    ctx: &SceneContext<'_>,
    charges: &mut Charges,
) -> Result<MattePlane, TemporalError> {
    use hsk_studio_nib::{FillRule, OwnedPath, PathElement, PathPoint};
    let (w, h) = (f64::from(scene.width), f64::from(scene.height));
    let corner = |x: f64, y: f64| {
        let (a, b) = matrix.apply(x, y);
        PathPoint::new(a, b)
    };
    let quad = OwnedPath {
        elements: vec![
            PathElement::MoveTo(corner(0.0, 0.0)),
            PathElement::LineTo(corner(w, 0.0)),
            PathElement::LineTo(corner(w, h)),
            PathElement::LineTo(corner(0.0, h)),
            PathElement::Close,
        ],
        fill_rule: FillRule::NonZero,
    };
    let px = u64::from(scene.width)
        .checked_mul(u64::from(scene.height))
        .ok_or(CompositeError::Overflow)?;
    charges.charge(ctx, px.checked_mul(MATTE_BYTES_PER_PIXEL).ok_or(CompositeError::Overflow)?)?;
    let coverage = crate::vector_nib::coverage(Some(&quad), scene.rect(), px).map_err(|code| {
        TemporalError::Unsupported {
            layer_id: layer_id.to_owned(),
            feature: code,
        }
    })?;
    let mut bytes = Vec::with_capacity(coverage.len().saturating_mul(2));
    for c in coverage {
        // 255 -> 65535 exactly (x * 257).
        bytes.extend_from_slice(&u16::from(c).saturating_mul(257).to_le_bytes());
    }
    Ok(MattePlane {
        width: scene.width,
        height: scene.height,
        sha256: hsk_studio_prism::profile_hash(&bytes),
        bytes,
    })
}

/// The coverage plane of one track matte of `scene` with fresh charges (the document-graph path,
/// `temporal::composition`).
// Only the document-graph path (feature motion) calls it.
#[cfg_attr(not(feature = "motion"), allow(dead_code))]
pub(crate) fn matte_coverage(
    scene: &SceneComposition,
    layer_id: &str,
    matte: &SceneMatte,
    ctx: &SceneContext<'_>,
    cancel: &CancellationToken,
) -> Result<MattePlane, TemporalError> {
    let mut charges = Charges::default();
    let mut ledger = ExactnessLedger::default();
    matte_plane(scene, layer_id, matte, ctx, cancel, 1, &mut charges, &mut ledger)
}

/// The coverage plane of one track matte (module docs): the matte layer rendered alone over the
/// composition rect, converted to `coverage_u16le`. Absent matte layer = zero coverage (no
/// render). The render and the plane are charged before allocation.
#[allow(clippy::too_many_arguments)]
fn matte_plane(
    scene: &SceneComposition,
    layer_id: &str,
    matte: &SceneMatte,
    ctx: &SceneContext<'_>,
    cancel: &CancellationToken,
    depth: usize,
    charges: &mut Charges,
    exactness: &mut ExactnessLedger,
) -> Result<MattePlane, TemporalError> {
    if depth > ctx.limits.max_depth {
        return Err(CompositeError::BudgetExceeded.into());
    }
    let px = u64::from(scene.width)
        .checked_mul(u64::from(scene.height))
        .ok_or(CompositeError::Overflow)?;
    let plane_bytes = px
        .checked_mul(MATTE_BYTES_PER_PIXEL)
        .ok_or(CompositeError::Overflow)?;
    charges.charge(ctx, plane_bytes)?;
    let len = usize::try_from(plane_bytes).map_err(|_| CompositeError::Overflow)?;
    let mut bytes = vec![0u8; len];
    if let Some(source) = &matte.layer {
        let alone = SceneComposition {
            composition_id: format!("{}/matte/{layer_id}", scene.composition_id),
            width: scene.width,
            height: scene.height,
            background: None,
            filter: scene.filter,
            layers: vec![SceneLayer {
                // The matte contributes its coverage, never its blend mode.
                mode: StudioBlendMode::Normal,
                ..(**source).clone()
            }],
        };
        let r = render_inner(&alone, ctx, cancel, depth, charges)?;
        for op in &r.exactness.operators {
            exactness.record(op.key, op.exactness);
        }
        let read = |plane: &[u8], i: usize| -> Option<f32> {
            let s = plane.get(i..i.checked_add(4)?)?;
            Some(f32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        };
        for (i, out) in bytes.chunks_exact_mut(2).enumerate() {
            let c = i.checked_mul(12).ok_or(CompositeError::Overflow)?;
            let a_at = i.checked_mul(4).ok_or(CompositeError::Overflow)?;
            let alpha = read(&r.alpha, a_at).ok_or(CompositeError::Overflow)?;
            let v = match matte.mode {
                SceneMatteMode::Alpha => alpha,
                SceneMatteMode::Luma => {
                    let mut luma = 0.0f32;
                    for (k, w) in LUMA_WEIGHTS.iter().enumerate() {
                        let at = k
                            .checked_mul(4)
                            .and_then(|o| c.checked_add(o))
                            .ok_or(CompositeError::Overflow)?;
                        luma += w * read(&r.colour, at).ok_or(CompositeError::Overflow)?;
                    }
                    luma * alpha
                }
            };
            if !v.is_finite() {
                return Err(invalid(layer_id, "matte"));
            }
            // `v` is clamped to `0..=1`, so the scaled value fits a u16.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let q = (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
            out.copy_from_slice(&q.to_le_bytes());
        }
    }
    let sha256 = hsk_studio_prism::profile_hash(&bytes);
    Ok(MattePlane {
        width: scene.width,
        height: scene.height,
        bytes,
        sha256,
    })
}

/// Renders a static subtree into an owned surface over its rect (charged before allocation).
fn render_static(
    layer_id: &str,
    content: &StaticContent,
    ctx: &SceneContext<'_>,
    cancel: &CancellationToken,
    charges: &mut Charges,
    exactness: &mut ExactnessLedger,
) -> Result<Surface, TemporalError> {
    if content.plan.target.space != ctx.space || content.plan.target.extent != content.rect {
        return Err(CompositeError::ColourMismatch.into());
    }
    charges.surface(ctx, content.rect.width, content.rect.height)?;
    let mut plan = content.plan.clone();
    plan.revision = ctx.request.expected_revision;
    let (x0, y0) = (content.rect.x, content.rect.y);
    let mut sink = OffsetSink {
        x0,
        y0,
        frame: FrameSink::new(content.rect.width, content.rect.height)?,
    };
    let sources = StaticSources {
        content,
        pixels: ctx.pixels,
    };
    let receipt = ctx
        .backend
        .execute_extended(&plan, &content.extensions, &ctx.request, &sources, cancel, &mut sink)
        .map_err(|e| TemporalError::Render {
            composition_id: format!("{layer_id}/static"),
            code: e.code(),
        })?;
    for op in &receipt.exactness.operators {
        exactness.record(op.key, op.exactness);
    }
    let f = sink.frame;
    Ok(Surface {
        colour_sha256: hsk_studio_prism::profile_hash(&f.colour),
        alpha_sha256: hsk_studio_prism::profile_hash(&f.alpha),
        width: f.width,
        height: f.height,
        colour: f.colour,
        alpha: f.alpha,
    })
}

/// A [`FrameSink`] whose frame starts at `(x0, y0)` of plan space.
struct OffsetSink {
    x0: i64,
    y0: i64,
    frame: FrameSink,
}

impl TileSink for OffsetSink {
    fn accept(&mut self, rect: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let local = Rect {
            x: rect.x.checked_sub(self.x0).ok_or(SinkError::Rejected)?,
            y: rect.y.checked_sub(self.y0).ok_or(SinkError::Rejected)?,
            ..rect
        };
        self.frame.accept(local, colour, alpha)
    }
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
        let px = usize::try_from(
            u64::from(width)
                .checked_mul(u64::from(height))
                .ok_or(CompositeError::Overflow)?,
        )
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
        let (x0, y0) = (
            u32::try_from(rect.x).map_err(|_| bad)?,
            u32::try_from(rect.y).map_err(|_| bad)?,
        );
        let x1 = x0.checked_add(rect.width).ok_or(bad)?;
        let y1 = y0.checked_add(rect.height).ok_or(bad)?;
        if x1 > self.width || y1 > self.height {
            return Err(bad);
        }
        let w = usize::try_from(rect.width).map_err(|_| bad)?;
        let fw = usize::try_from(self.width).map_err(|_| bad)?;
        let (x0, y0) = (
            usize::try_from(x0).map_err(|_| bad)?,
            usize::try_from(y0).map_err(|_| bad)?,
        );
        for row in 0..usize::try_from(rect.height).map_err(|_| bad)? {
            let src = row.checked_mul(w).ok_or(bad)?;
            let dst = y0
                .checked_add(row)
                .and_then(|y| y.checked_mul(fw))
                .and_then(|v| v.checked_add(x0))
                .ok_or(bad)?;
            let span = |i: usize, n: usize| -> Option<std::ops::Range<usize>> {
                let a = i.checked_mul(n)?;
                Some(a..a.checked_add(w.checked_mul(n)?)?)
            };
            let (Some(cs), Some(cd), Some(a_src), Some(a_dst)) =
                (span(src, 12), span(dst, 12), span(src, 4), span(dst, 4))
            else {
                return Err(bad);
            };
            self.colour
                .get_mut(cd)
                .ok_or(bad)?
                .copy_from_slice(colour.get(cs).ok_or(bad)?);
            self.alpha
                .get_mut(a_dst)
                .ok_or(bad)?
                .copy_from_slice(alpha.get(a_src).ok_or(bad)?);
        }
        Ok(())
    }
}

/// Lower and execute one scene with the context's backend into a full-frame surface (straight
/// alpha). Every surface (the frame and each precomposition) is charged against the limits
/// before allocation; a backend refusal is `TemporalError::Render` with the backend's stable
/// code.
pub fn render_scene(
    scene: &SceneComposition,
    ctx: &SceneContext<'_>,
    cancel: &CancellationToken,
) -> Result<RenderedScene, TemporalError> {
    render_inner(scene, ctx, cancel, 0, &mut Charges::default())
}

/// `content` rendered over its plan rect `rect` into a surface (integer placement, no resample).
#[allow(clippy::too_many_arguments)]
fn content_surface(
    id: &str,
    content: &SceneContent,
    rect: Rect,
    filter: ResampleFilter,
    ctx: &SceneContext<'_>,
    cancel: &CancellationToken,
    depth: usize,
    charges: &mut Charges,
    exactness: &mut ExactnessLedger,
) -> Result<Surface, TemporalError> {
    if depth >= ctx.limits.max_depth {
        return Err(CompositeError::BudgetExceeded.into());
    }
    let depth = depth.checked_add(1).ok_or(CompositeError::Overflow)?;
    #[allow(clippy::cast_precision_loss)]
    let matrix = Affine2::translate(-(rect.x as f64), -(rect.y as f64));
    let nested = SceneComposition {
        composition_id: format!("{id}/lit"),
        width: rect.width,
        height: rect.height,
        background: None,
        filter,
        layers: vec![SceneLayer {
            layer_id: id.to_owned(),
            content: content.clone(),
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            fill_opacity: 1.0,
            matrix,
            matte: None,
            clipped: false,
        }],
    };
    let r = render_inner(&nested, ctx, cancel, depth, charges)?;
    for op in &r.exactness.operators {
        exactness.record(op.key, op.exactness);
    }
    Ok(Surface::from_rendered(r))
}

fn f32_at(bytes: &[u8], index: usize) -> f32 {
    index
        .checked_mul(4)
        .and_then(|o| bytes.get(o..o.checked_add(4)?))
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map_or(0.0, f32::from_le_bytes)
}

/// Multiplies a layer-space mask (one coverage per texel, `0..=1`) into a surface's alpha (lane
/// SMB-AE-3D-2; STU-CMP-010 step 2). A coverage plane of another size is refused `InvalidState`.
fn apply_layer_mask(id: &str, surface: &mut Surface, mask: Option<&[f32]>) -> Result<(), TemporalError> {
    let Some(mask) = mask else { return Ok(()) };
    let n = usize::try_from(u64::from(surface.width).checked_mul(u64::from(surface.height)).ok_or(CompositeError::Overflow)?).map_err(|_| CompositeError::Overflow)?;
    if mask.len() != n || surface.alpha.len() != n.checked_mul(4).ok_or(CompositeError::Overflow)? {
        return Err(invalid(id, "layer_mask"));
    }
    for (a, m) in surface.alpha.chunks_exact_mut(4).zip(mask) {
        if !m.is_finite() {
            return Err(invalid(id, "layer_mask"));
        }
        let v = f32::from_le_bytes([a[0], a[1], a[2], a[3]]) * m.clamp(0.0, 1.0);
        a.copy_from_slice(&v.to_le_bytes());
    }
    surface.alpha_sha256 = hsk_studio_prism::profile_hash(&surface.alpha);
    Ok(())
}

/// STU-CMP-050 auxiliary channels of 3D planes (lane SMB-AE-3D-2): every plane's content rendered
/// over its rectangle by the context's backend (layer mask applied), then
/// [`crate::three_d::aux_channels`] per pixel centre. Surfaces are charged before allocation.
#[allow(clippy::too_many_arguments)]
pub fn three_d_aux_channels(
    planes: &[crate::three_d::Plane3d],
    ids: &[u32],
    cam: &crate::three_d::Camera,
    width: u32,
    height: u32,
    filter: ResampleFilter,
    ctx: &SceneContext<'_>,
    cancel: &CancellationToken,
) -> Result<crate::three_d::AuxChannels, TemporalError> {
    let mut charges = Charges::default();
    let mut ledger = ExactnessLedger::default();
    let mut alphas = Vec::with_capacity(planes.len());
    for p in planes {
        cancel.check().map_err(|_| canceled())?;
        let mut s = content_surface(&p.layer_id, &p.content, p.bounds, filter, ctx, cancel, 0, &mut charges, &mut ledger)?;
        apply_layer_mask(&p.layer_id, &mut s, p.layer_mask.as_deref())?;
        let n = usize::try_from(u64::from(s.width).checked_mul(u64::from(s.height)).ok_or(CompositeError::Overflow)?).map_err(|_| CompositeError::Overflow)?;
        alphas.push((0..n).map(|i| f32_at(&s.alpha, i)).collect::<Vec<f32>>());
    }
    crate::three_d::aux_channels(planes, &alphas, ids, cam, width, height)
}

/// The lit plane's surface: its content rendered over `lit.rect`, every texel shaded by
/// [`crate::three_d::shade`] with the casters' rendered alpha (nearest texel).
#[allow(clippy::too_many_arguments)]
fn lit_surface(
    id: &str,
    lit: &crate::three_d::LitContent,
    filter: ResampleFilter,
    ctx: &SceneContext<'_>,
    cancel: &CancellationToken,
    depth: usize,
    charges: &mut Charges,
    exactness: &mut ExactnessLedger,
) -> Result<Surface, TemporalError> {
    let mut surface = content_surface(id, &lit.content, lit.rect, filter, ctx, cancel, depth, charges, exactness)?;
    apply_layer_mask(id, &mut surface, lit.mask.as_deref())?;
    let mut casters = Vec::with_capacity(lit.casters.len());
    for c in &lit.casters {
        let mut s = content_surface(&c.layer_id, &c.content, c.bounds, filter, ctx, cancel, depth, charges, exactness)?;
        apply_layer_mask(&c.layer_id, &mut s, c.mask.as_deref())?;
        casters.push(s);
    }
    let caster_alpha = |i: usize, x: f64, y: f64| -> f32 {
        let (Some(c), Some(s)) = (lit.casters.get(i), casters.get(i)) else { return 0.0 };
        #[allow(clippy::cast_precision_loss)]
        let (ix, iy) = (x.floor() - c.bounds.x as f64, y.floor() - c.bounds.y as f64);
        if !(ix >= 0.0 && iy >= 0.0 && ix < f64::from(s.width) && iy < f64::from(s.height)) {
            return 0.0;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let at = (iy as usize).checked_mul(s.width as usize).and_then(|r| r.checked_add(ix as usize));
        at.map_or(0.0, |at| f32_at(&s.alpha, at))
    };
    let w = usize::try_from(surface.width).map_err(|_| CompositeError::Overflow)?;
    let h = usize::try_from(surface.height).map_err(|_| CompositeError::Overflow)?;
    // No lights (a defocus-only plane): the albedo is kept.
    let shade_rows = if lit.lights.is_empty() { 0 } else { h };
    for row in 0..shade_rows {
        cancel.check().map_err(|_| TemporalError::Composite(CompositeError::Canceled))?;
        for col in 0..w {
            let at = row.checked_mul(w).and_then(|r| r.checked_add(col)).ok_or(CompositeError::Overflow)?;
            let base = at.checked_mul(3).ok_or(CompositeError::Overflow)?;
            let albedo = [f32_at(&surface.colour, base), f32_at(&surface.colour, base.saturating_add(1)), f32_at(&surface.colour, base.saturating_add(2))];
            #[allow(clippy::cast_precision_loss)]
            let (x, y) = (lit.rect.x as f64 + col as f64 + 0.5 - lit.origin[0], lit.rect.y as f64 + row as f64 + 0.5 - lit.origin[1]);
            let shaded = crate::three_d::shade(lit, x, y, albedo, &caster_alpha);
            for (k, v) in shaded.iter().enumerate() {
                let o = base.saturating_add(k).saturating_mul(4);
                if let Some(dst) = surface.colour.get_mut(o..o.saturating_add(4)) {
                    dst.copy_from_slice(&v.to_le_bytes());
                }
            }
        }
    }
    // Depth of field (STU-CMP-040): a per-texel defocus radius from the camera depth.
    if let Some((view, zoom, dof)) = &lit.dof {
        let n = w.checked_mul(h).ok_or(CompositeError::Overflow)?;
        let floats = |b: &[u8], count: usize| -> Vec<f32> { (0..count).map(|i| f32_at(b, i)).collect() };
        let rgb = floats(&surface.colour, n.checked_mul(3).ok_or(CompositeError::Overflow)?);
        let alpha = floats(&surface.alpha, n);
        let mut radii = Vec::with_capacity(n);
        for row in 0..h {
            for col in 0..w {
                #[allow(clippy::cast_precision_loss)]
                let (x, y) = (lit.rect.x as f64 + col as f64 + 0.5, lit.rect.y as f64 + row as f64 + 0.5);
                radii.push(crate::three_d::defocus_radius(view, *zoom, dof, x, y));
            }
        }
        let (rgb, alpha) = crate::three_d::defocus(&rgb, &alpha, w, h, &radii).map_err(|()| TemporalError::Unsupported {
            layer_id: id.to_owned(),
            feature: crate::three_d::COMPOSITION_3D_DOF_BUDGET,
        })?;
        surface.colour = rgb.iter().flat_map(|v| v.to_le_bytes()).collect();
        surface.alpha = alpha.iter().flat_map(|v| v.to_le_bytes()).collect();
        surface.alpha_sha256 = hsk_studio_prism::profile_hash(&surface.alpha);
    }
    surface.colour_sha256 = hsk_studio_prism::profile_hash(&surface.colour);
    Ok(surface)
}

fn render_inner(
    scene: &SceneComposition,
    ctx: &SceneContext<'_>,
    cancel: &CancellationToken,
    depth: usize,
    charges: &mut Charges,
) -> Result<RenderedScene, TemporalError> {
    charges.surface(ctx, scene.width, scene.height)?;
    let lowering = lower_inner(scene, ctx, cancel, depth, charges)?;
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
