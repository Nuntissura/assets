//! Shared fixture of lane SMB-COMPOSITE-DOCGRAPH-MASKS (folio-free, motion-free). Included with
//! `#[path]` by composite `tests/composition_docgraph_masks.rs` (which proves that the Folio
//! records lower, on the document-graph path, to exactly [`document_frame`] and render
//! pixel-identical to the scene path) and by render-cpu / render-gpu
//! `tests/composition_docgraph_masks.rs` (decision B1), which execute [`document_frame`] and
//! compare every pixel with [`oracle`].
//!
//! Composition 32x24, no background; the tiles of `composition_scenes::Pixels`, each authored at
//! (0, 0) in the document and moved by its composition position (anchor (0, 0)), bottom first:
//! - layer 11 `tf` 20x14: no cut.
//! - layer 10 `te` 10x8: VECTOR mask of the diamond (source 30), inverted, density 60 %,
//!   feather 1.5 px, linked.
//! - layer 12 `tg` 8x8: Folio `clipped`, clipped to layer 10 (a masked base).
//! - layer 2 `ta` 8x6: vector clip of the triangle (source 31, NONZERO), linked.
//! - layer 3 `tb` 6x6: ALPHA mask of source 32 (a 70 % fill), UNLINKED: the layer moves, the mask
//!   stays where the document put it.
//! - layer 7 `tc` 6x4: LUMINANCE mask of source 33 with `reveal_outside` (white outside the art),
//!   linked.
//!
//! The mask planes come from composite's folio-free static machinery: each one is the plane
//! `vector_layer_render_plan` gives a stand-in vector rectangle with the layer's tile extent (or,
//! for the unlinked mask, the frame extent) carrying the same clip / mask. The oracle composites
//! the tiles with pigment's `composite_over`, the masks with pigment's feather kernel and
//! `mask_coverage`, and the clip run with the clip-to-base rule (`composition_scenes::oracle`).
#![allow(dead_code)]
#[path = "composition_scenes.rs"]
pub mod scenes;

use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::vector::{FillRule, OwnedPath, PathElement, PathPoint};
use hsk_studio_composite::{
    FillPaint, FillSpec, MaskContentMode, MaskNode, OwnedMask, PreparedVectors, RenderedScene, SceneComposition,
    SceneContent, SceneContext, SceneLayer, SceneLimits, StaticContent, VectorCoverage, VectorGeometry,
    VectorLayerSpec, VectorMask, VectorMaskSource, VectorResolution, mask_content, prepare_vector_layers,
    render_scene, unlinked_mask_matte, vector_layer_render_plan,
};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, StudioBlendMode, composite_over};
use hsk_studio_pigment::coverage_math::{feather_kernel, mask_coverage};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BackendIdentity, DeviceClass, Exactness, ExecuteRequest, Group, Layer,
    LayerExtension, MaskInput, NoVector, Operator, OutputTarget, PlanExtensions, PlanNode, PlanNodeId, PlanSources,
    RenderBackend, RenderPlan, RenderReceipt, SampleFormat, SinkError, SourceKey, TileInput, TilePlanes, TileSink,
};
use hsk_studio_pigment::Mask;
use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter};
use scenes::Pixels;
use std::collections::BTreeMap;

pub const VECTOR_SOURCE: u8 = 30;
pub const CLIP_SOURCE: u8 = 31;
pub const ALPHA_SOURCE: u8 = 32;
pub const LUMA_SOURCE: u8 = 33;
pub const VECTOR_MASKED: u8 = 10;
pub const CLIPPED_BY_VECTOR: u8 = 2;
pub const UNLINKED_MASKED: u8 = 3;
pub const LUMA_MASKED: u8 = 7;
const STAND_IN: &str = "stand-in";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cut {
    /// VECTOR mask, inverted, density 60 %, feather 1.5 px, linked.
    Vector,
    /// Vector clip (moves with the layer).
    Clip,
    /// ALPHA mask, unlinked.
    AlphaUnlinked,
    /// LUMINANCE mask, reveal_outside, linked.
    LuminanceRevealed,
}
/// One raster layer of the composition (bottom first in [`LAYERS`]).
pub struct LayerSpec {
    pub n: u8,
    pub key: &'static str,
    pub w: u32,
    pub h: u32,
    pub clipped: bool,
    /// Composition position (anchor (0, 0)).
    pub position: [i64; 2],
    /// Plan translation of the layer's content (`position - content origin`).
    pub translate: [i64; 2],
    pub cut: Option<Cut>,
}
pub const LAYERS: [LayerSpec; 6] = [
    LayerSpec { n: 11, key: "tf", w: 20, h: 14, clipped: false, position: [1, 2], translate: [1, 2], cut: None },
    LayerSpec { n: 10, key: "te", w: 10, h: 8, clipped: false, position: [12, 3], translate: [12, 3], cut: Some(Cut::Vector) },
    LayerSpec { n: 12, key: "tg", w: 8, h: 8, clipped: true, position: [14, 5], translate: [14, 5], cut: None },
    LayerSpec { n: 2, key: "ta", w: 8, h: 6, clipped: false, position: [3, 12], translate: [3, 12], cut: Some(Cut::Clip) },
    LayerSpec { n: 3, key: "tb", w: 6, h: 6, clipped: false, position: [22, 14], translate: [22, 14], cut: Some(Cut::AlphaUnlinked) },
    LayerSpec { n: 7, key: "tc", w: 6, h: 4, clipped: false, position: [24, 2], translate: [24, 2], cut: Some(Cut::LuminanceRevealed) },
];
/// Hidden vector sources and their fill (straight RGB, fill opacity).
pub const SOURCES: [(u8, [f32; 4]); 4] = [
    (VECTOR_SOURCE, [0.4, 0.4, 0.4, 1.0]),
    (CLIP_SOURCE, [0.5, 0.5, 0.5, 1.0]),
    (ALPHA_SOURCE, [0.2, 0.6, 0.9, 0.7]),
    (LUMA_SOURCE, [0.9, 0.3, 0.2, 1.0]),
];

fn poly(points: &[(f64, f64)]) -> OwnedPath {
    let mut elements = vec![PathElement::MoveTo(PathPoint::new(points[0].0, points[0].1))];
    elements.extend(points[1..].iter().map(|p| PathElement::LineTo(PathPoint::new(p.0, p.1))));
    elements.push(PathElement::Close);
    OwnedPath { elements, fill_rule: FillRule::NonZero }
}
/// The geometry of source layer `n` (document pixels).
pub fn source_path(n: u8) -> OwnedPath {
    match n {
        VECTOR_SOURCE => poly(&[(5.0, 0.5), (10.5, 4.0), (5.0, 7.5), (-0.5, 4.0)]),
        CLIP_SOURCE => poly(&[(0.5, 0.5), (8.5, 1.25), (2.25, 6.5)]),
        ALPHA_SOURCE => poly(&[(20.5, 12.5), (29.0, 13.0), (28.0, 21.25), (21.0, 20.0)]),
        LUMA_SOURCE => poly(&[(1.0, 0.5), (6.0, 2.0), (2.0, 4.5)]),
        other => panic!("source {other}"),
    }
}
pub fn layer_id(n: u8) -> String {
    scenes::identity("SLYR", n)
}
pub fn extent() -> Rect {
    Rect { x: 0, y: 0, width: scenes::W, height: scenes::H }
}
pub fn spec(n: u8) -> &'static LayerSpec {
    LAYERS.iter().find(|l| l.n == n).expect("fixture layer")
}
/// The layer's tile rect after its move.
pub fn moved_rect(l: &LayerSpec) -> Rect {
    Rect { x: l.translate[0], y: l.translate[1], width: l.w, height: l.h }
}
/// The tile of layer `l` in the fixture pixels.
pub fn tile_of<'p>(pixels: &'p Pixels, l: &LayerSpec) -> &'p scenes::TilePix {
    pixels.get(&scenes::tile_id(l.n, l.key)).expect("fixture tile")
}

/// The sources prepared folio-free (the fills of [`SOURCES`]) plus a stand-in rectangle `rect`.
fn prepared(rect: Rect) -> PreparedVectors {
    let fill = |rgba: [f32; 4]| FillSpec { paint: FillPaint::Solid([rgba[0], rgba[1], rgba[2], 1.0]), mode: StudioBlendMode::Normal, opacity: rgba[3] };
    let mut specs: Vec<VectorLayerSpec> = SOURCES
        .iter()
        .map(|(n, c)| VectorLayerSpec { layer_id: layer_id(*n), geometry: VectorGeometry::Paths(vec![source_path(*n)]), fills: vec![fill(*c)] })
        .collect();
    let (x0, y0) = (rect.x as f64, rect.y as f64);
    let (x1, y1) = (x0 + f64::from(rect.width), y0 + f64::from(rect.height));
    specs.push(VectorLayerSpec {
        layer_id: STAND_IN.into(),
        geometry: VectorGeometry::Paths(vec![poly(&[(x0, y0), (x1, y0), (x1, y1), (x0, y1)])]),
        fills: vec![fill([1.0, 1.0, 1.0, 1.0])],
    });
    let res = VectorResolution { coverage: VectorCoverage::Exact, ..VectorResolution::default() };
    prepare_vector_layers(&specs, &res, &CancellationToken::default()).unwrap()
}
/// The vector mask of `cut` over `v` (Folio's reading of the fixture records).
pub fn vector_mask(cut: Cut, v: &PreparedVectors) -> Option<VectorMask> {
    let content = |n: u8, mode| {
        let node = MaskNode::Leaf { layer_id: layer_id(n), mode: StudioBlendMode::Normal, opacity: 1.0 };
        mask_content(v, &node, mode, &mut (1 << 24)).unwrap()
    };
    match cut {
        Cut::Vector => Some(VectorMask {
            invert: true,
            density: 0.6,
            feather_px: 1.5,
            ..VectorMask::of(VectorMaskSource::Outline(Some(source_path(VECTOR_SOURCE))))
        }),
        Cut::Clip => None,
        Cut::AlphaUnlinked => Some(VectorMask { linked: false, ..VectorMask::of(content(ALPHA_SOURCE, MaskContentMode::Alpha)) }),
        Cut::LuminanceRevealed => Some(VectorMask::of(content(LUMA_SOURCE, MaskContentMode::LuminanceRevealed))),
    }
}

/// One cut layer's plane as the static machinery lowers it: the plane `vector_layer_render_plan`
/// gives the stand-in rectangle `rect` with the same clip or mask, the wrapper's mask flags and
/// the envelope feather.
pub struct CutPlane {
    pub plane: OwnedMask,
    pub input: MaskInput,
    pub feather_px: f32,
}
pub fn cut_plane(cut: Cut, rect: Rect) -> CutPlane {
    let v = prepared(rect);
    let v = match vector_mask(cut, &v) {
        Some(m) => v.with_mask(STAND_IN, m),
        None => {
            let mut tri = source_path(CLIP_SOURCE);
            tri.fill_rule = FillRule::NonZero;
            v.with_clip(STAND_IN, Some(tri))
        }
    };
    let target = OutputTarget { extent: extent(), format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, space: scenes::space() };
    let low = vector_layer_render_plan(&v, STAND_IN, 7, target).unwrap();
    let PlanNode::Group(root) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!("wrapped stand-in") };
    let input = root.layers[0].mask.expect("cut on the wrapper");
    let [(_, p)] = low.clip_planes.as_slice() else { panic!("one plane") };
    let feather_px = low.extensions.layers.iter().find(|e| e.group == low.plan.root && e.layer == 0).map_or(0.0, |e| e.mask_feather_px);
    let m = p.mask();
    CutPlane {
        plane: OwnedMask { bounds: m.bounds, sample_format: m.sample_format.into(), bytes: m.bytes.to_vec(), stride_bytes: m.stride_bytes, sha256: m.sha256 },
        input,
        feather_px,
    }
}
/// The plane of layer `l` in plan space: a linked cut over the tile extent, moved with the layer;
/// the unlinked mask over the frame extent, in place.
pub fn layer_plane(l: &LayerSpec) -> Option<CutPlane> {
    let cut = l.cut?;
    let tile = Rect { x: 0, y: 0, width: l.w, height: l.h };
    if cut == Cut::AlphaUnlinked {
        return Some(cut_plane(cut, extent()));
    }
    let mut c = cut_plane(cut, tile);
    c.plane.bounds.x += l.translate[0];
    c.plane.bounds.y += l.translate[1];
    c.input.placement = c.plane.bounds;
    Some(c)
}

// ---- Document frame (what composite lowers the Folio records to) ----

/// A document-graph frame without Folio types: plan, envelope, plan keys -> fixture tile ids, and
/// the planes composite owns.
#[derive(Clone, Debug, PartialEq)]
pub struct DocFrame {
    pub plan: RenderPlan,
    pub extensions: PlanExtensions,
    pub tiles: BTreeMap<SourceKey, String>,
    pub owned_masks: BTreeMap<SourceKey, OwnedMask>,
}
/// The sources of a [`DocFrame`]: fixture tiles by id, owned planes by key.
pub struct FrameSources<'a>(pub &'a DocFrame, pub &'a Pixels);
impl PlanSources for FrameSources<'_> {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        self.1.get(self.0.tiles.get(&key)?).map(scenes::TilePix::planes)
    }
    fn mask(&self, key: SourceKey) -> Option<Mask<'_>> {
        let m = self.0.owned_masks.get(&key)?;
        Some(Mask { bounds: m.bounds, sample_format: &m.sample_format, bytes: &m.bytes, stride_bytes: m.stride_bytes, sha256: m.sha256 })
    }
}

/// The document-graph frame of the fixture, in the static lowering's layout: one tile node per
/// layer in layer-id order (key = node), then the root group (isolated) with the layers bottom
/// first, each cut layer's plane keyed after the tiles in group order, the clipped layer on
/// pigment's clip-to-base route, and the feather in the envelope.
pub fn document_frame(pixels: &Pixels) -> DocFrame {
    let mut by_id: Vec<&LayerSpec> = LAYERS.iter().collect();
    by_id.sort_by_key(|l| layer_id(l.n));
    let mut nodes = Vec::new();
    let mut tiles = BTreeMap::new();
    for (i, l) in by_id.iter().enumerate() {
        let t = tile_of(pixels, l);
        let key = SourceKey(i as u32);
        nodes.push(PlanNode::Tile(TileInput {
            key,
            placement: moved_rect(l),
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: t.colour_sha256,
            alpha_sha256: t.alpha_sha256,
        }));
        tiles.insert(key, t.id.clone());
    }
    let root = PlanNodeId(nodes.len() as u32);
    let mut next = nodes.len() as u32;
    let mut layers = Vec::new();
    let mut owned_masks = BTreeMap::new();
    let mut entries = Vec::new();
    for (li, l) in LAYERS.iter().enumerate() {
        let node = by_id.iter().position(|o| o.n == l.n).unwrap() as u32;
        let mask = if l.clipped {
            Some(MaskInput::clip_to_base())
        } else if let Some(c) = layer_plane(l) {
            let key = SourceKey(next);
            next += 1;
            owned_masks.insert(key, c.plane);
            if c.feather_px > 0.0 {
                entries.push(LayerExtension::new(root, li as u32).with_mask_feather(c.feather_px));
            }
            Some(MaskInput { key, ..c.input })
        } else {
            None
        };
        layers.push(Layer { input: PlanNodeId(node), mode: StudioBlendMode::Normal, opacity: 1.0, fill_opacity: 1.0, mask });
    }
    nodes.push(PlanNode::Group(Group { isolated: true, layers }));
    DocFrame {
        plan: RenderPlan { revision: 7, target: target(), nodes, root },
        extensions: PlanExtensions::from_layers(entries),
        tiles,
        owned_masks,
    }
}
/// The scene path of the same composition (what `CompositionTemporalSource::scene_at` returns
/// for the Folio records): plain layers as tiles, cut layers as static subtrees (the tile and its
/// unmoved plane, `SceneContent::Static`), the clipped layer on the clip-to-base route, the
/// unlinked mask as a composition-space alpha matte (`unlinked_mask_matte`, identity matrix).
pub fn scene(pixels: &Pixels) -> SceneComposition {
    let mut layers = Vec::new();
    for l in &LAYERS {
        let t = tile_of(pixels, l);
        let rect = Rect { x: 0, y: 0, width: l.w, height: l.h };
        let mut matte = None;
        let content = match l.cut {
            None => SceneContent::Tiles(vec![t.scene_tile(0, 0)]),
            Some(cut) => {
                let tile = PlanNode::Tile(TileInput {
                    key: SourceKey(0),
                    placement: rect,
                    format: SampleFormat::RgbF32Le,
                    alpha: AlphaConvention::Straight,
                    colour_sha256: t.colour_sha256,
                    alpha_sha256: t.alpha_sha256,
                });
                let mut owned_masks = BTreeMap::new();
                let mut entries = Vec::new();
                let (nodes, root) = if cut == Cut::AlphaUnlinked {
                    let mask = vector_mask(cut, &prepared(rect)).unwrap();
                    matte = Some(unlinked_mask_matte(&mask, &layer_id(l.n), scenes::space(), 7).unwrap());
                    (vec![tile], PlanNodeId(0))
                } else {
                    let c = cut_plane(cut, rect);
                    owned_masks.insert(SourceKey(1), c.plane);
                    if c.feather_px > 0.0 {
                        entries.push(LayerExtension::new(PlanNodeId(1), 0).with_mask_feather(c.feather_px));
                    }
                    let wrap = PlanNode::Group(Group {
                        isolated: true,
                        layers: vec![Layer {
                            input: PlanNodeId(0),
                            mode: StudioBlendMode::Normal,
                            opacity: 1.0,
                            fill_opacity: 1.0,
                            mask: Some(MaskInput { key: SourceKey(1), ..c.input }),
                        }],
                    });
                    (vec![tile, wrap], PlanNodeId(1))
                };
                let mut tiles = BTreeMap::new();
                tiles.insert(SourceKey(0), t.id.clone());
                SceneContent::Static(Box::new(StaticContent {
                    plan: RenderPlan {
                        revision: 7,
                        target: OutputTarget { extent: rect, ..target() },
                        nodes,
                        root,
                    },
                    extensions: PlanExtensions::from_layers(entries),
                    rect,
                    tiles,
                    masks: BTreeMap::new(),
                    owned_masks,
                    owned_tiles: BTreeMap::new(),
                }))
            }
        };
        layers.push(SceneLayer {
            layer_id: layer_id(l.n),
            content,
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            fill_opacity: 1.0,
            matrix: Affine2 { m: [1.0, 0.0, l.translate[0] as f64, 0.0, 1.0, l.translate[1] as f64] },
            matte,
            clipped: l.clipped,
        });
    }
    SceneComposition {
        composition_id: scenes::identity("SCOM", 9),
        width: scenes::W,
        height: scenes::H,
        background: None,
        filter: ResampleFilter::Bilinear,
        layers,
    }
}
pub fn target() -> OutputTarget {
    OutputTarget { extent: extent(), format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, space: scenes::space() }
}

// ---- Rendering helpers ----

pub fn request() -> ExecuteRequest {
    ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 }
}
pub fn context<'a>(backend: &'a dyn RenderBackend, pixels: &'a Pixels) -> SceneContext<'a> {
    SceneContext { backend, pixels, request: request(), space: scenes::space(), limits: SceneLimits::default() }
}
struct Grid(Vec<[f32; 4]>);
fn f32_at(b: &[u8], i: usize) -> f32 {
    f32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}
impl TileSink for Grid {
    fn accept(&mut self, r: Rect, c: &[u8], a: &[u8]) -> Result<(), SinkError> {
        for row in 0..r.height as usize {
            for col in 0..r.width as usize {
                let i = row * r.width as usize + col;
                let at = (r.y as usize + row) * scenes::W as usize + r.x as usize + col;
                self.0[at] = [f32_at(c, i * 12), f32_at(c, i * 12 + 4), f32_at(c, i * 12 + 8), f32_at(a, i * 4)];
            }
        }
        Ok(())
    }
}
/// The frame executed by `backend`, row-major straight RGBA over [`extent`].
pub fn render_frame(backend: &dyn RenderBackend, frame: &DocFrame, pixels: &Pixels) -> Result<Vec<[f32; 4]>, BackendError> {
    let mut grid = Grid(vec![[0.0; 4]; (scenes::W * scenes::H) as usize]);
    backend.execute_extended(&frame.plan, &frame.extensions, &request(), &FrameSources(frame, pixels), &CancellationToken::default(), &mut grid)?;
    Ok(grid.0)
}
/// `render_scene` of `scene` by `backend`, row-major straight RGBA.
pub fn render_scene_px(backend: &dyn RenderBackend, scene: &SceneComposition, pixels: &Pixels) -> Vec<[f32; 4]> {
    let r: RenderedScene = render_scene(scene, &context(backend, pixels), &CancellationToken::default()).expect("scene renders");
    (0..r.height).flat_map(|y| (0..r.width).map(move |x| (x, y))).map(|(x, y)| r.pixel(x, y).unwrap()).collect()
}
/// Largest |delta| of alpha and premultiplied colour.
pub fn max_delta(got: &[[f32; 4]], want: &[[f32; 4]]) -> f32 {
    let mut worst = 0.0f32;
    for (g, w) in got.iter().zip(want) {
        worst = worst.max((g[3] - w[3]).abs());
        for c in 0..3 {
            worst = worst.max((g[c] * g[3] - w[c] * w[3]).abs());
        }
    }
    worst
}
pub fn digest(px: &[[f32; 4]]) -> String {
    let (mut colour, mut alpha) = (Vec::new(), Vec::new());
    for p in px {
        for c in &p[..3] {
            colour.extend_from_slice(&c.to_le_bytes());
        }
        alpha.extend_from_slice(&p[3].to_le_bytes());
    }
    colour.extend_from_slice(&alpha);
    scenes::hex(&hsk_studio_prism::profile_hash(&colour))
}

// ---- Mask maths shared by the reference executor and the oracle ----

fn stored(m: &OwnedMask, x: i64, y: i64, clamp: bool) -> f32 {
    let r = m.bounds;
    let (x, y) = if clamp {
        (x.clamp(r.x, r.x + i64::from(r.width) - 1), y.clamp(r.y, r.y + i64::from(r.height) - 1))
    } else {
        (x, y)
    };
    if x < r.x || y < r.y || x >= r.x + i64::from(r.width) || y >= r.y + i64::from(r.height) {
        return 0.0;
    }
    let row = (y - r.y) as usize * m.stride_bytes as usize;
    if m.sample_format == "coverage_u8" {
        (f64::from(m.bytes[row + (x - r.x) as usize]) / 255.0) as f32
    } else {
        let i = row + (x - r.x) as usize * 2;
        (f64::from(u16::from_le_bytes([m.bytes[i], m.bytes[i + 1]])) / 65535.0) as f32
    }
}
/// Pigment's mask rule at `(x, y)`: feathered first (`feather_kernel`, horizontal then vertical,
/// taps clamped to the plane, binary64 sums, clamped into [0, 1]), then `mask_coverage`.
pub fn effective(m: &OwnedMask, feather_px: f32, inverted: bool, density: f32, x: i64, y: i64) -> f32 {
    let raw = if feather_px > 0.0 {
        let taps = feather_kernel(feather_px).unwrap();
        let s = (taps.len() / 2) as i64;
        let row = |yy: i64| -> f32 {
            let mut sum = 0.0f64;
            for (t, w) in taps.iter().enumerate() {
                sum += f64::from(*w) * f64::from(stored(m, x - s + t as i64, yy, true));
            }
            sum as f32
        };
        let mut sum = 0.0f64;
        for (t, w) in taps.iter().enumerate() {
            sum += f64::from(*w) * f64::from(row(y - s + t as i64));
        }
        (sum as f32).clamp(0.0, 1.0)
    } else {
        stored(m, x, y, false)
    };
    mask_coverage(raw, inverted, density).unwrap()
}
/// One layer of a group for the run rule: content, mode, opacity x fill x coverage, clipped.
struct Entry {
    src: Rgba,
    mode: StudioBlendMode,
    k: f32,
    clipped: bool,
}
/// Composites bottom-first entries over transparent: unclipped entries with `composite_over`; a
/// clip run [base, c1..cn] (pigment's clip-to-base rule, `composition_scenes::oracle`): G = the
/// base content with its own coverage, each clipped entry onto G with its alpha times the base
/// alpha and G's alpha held, then G onto the backdrop with the base's mode and opacity.
fn composite_entries(entries: &[(Entry, f32)], site: PixelSite) -> Rgba {
    let mut acc = Rgba::TRANSPARENT;
    let mut i = 0;
    while i < entries.len() {
        let (e, cov) = &entries[i];
        let mut j = i + 1;
        while entries.get(j).is_some_and(|(c, _)| c.clipped) {
            j += 1;
        }
        if j == i + 1 {
            acc = composite_over(e.mode, acc, e.src, e.k * cov, site).unwrap();
        } else {
            let mut g = Rgba::new(e.src.rgb, e.src.a * cov);
            let base_alpha = g.a;
            for (c, ccov) in &entries[i + 1..j] {
                let s = Rgba::new(c.src.rgb, c.src.a * base_alpha);
                g = composite_over(c.mode, g, s, c.k * ccov, site).unwrap();
                g.a = base_alpha;
            }
            acc = composite_over(e.mode, acc, g, e.k, site).unwrap();
        }
        i = j;
    }
    acc
}

/// Reference executor (plain plans plus envelope mask feather): per pixel, groups composite their
/// layers bottom-up over transparent with [`composite_entries`]; a coverage mask is [`effective`].
pub struct Ref;
fn texel(p: &TilePlanes<'_>, cx: i64, cy: i64) -> Rgba {
    let c = cy as usize * p.colour_stride_bytes as usize + cx as usize * 12;
    let a = cy as usize * p.alpha_stride_bytes as usize + cx as usize * 4;
    Rgba::new([f32_at(p.colour, c), f32_at(p.colour, c + 4), f32_at(p.colour, c + 8)], f32_at(p.alpha, a))
}
fn eval(plan: &RenderPlan, ext: &PlanExtensions, id: usize, src: &dyn PlanSources, x: i64, y: i64) -> Rgba {
    match &plan.nodes[id] {
        PlanNode::Tile(t) => {
            let (cx, cy) = (x - t.placement.x, y - t.placement.y);
            if cx < 0 || cy < 0 || cx >= i64::from(t.placement.width) || cy >= i64::from(t.placement.height) {
                return Rgba::TRANSPARENT;
            }
            texel(&src.tile(t.key).expect("tile served"), cx, cy)
        }
        PlanNode::Solid { rect, rgba } => {
            let inside = x >= rect.x && y >= rect.y && x < rect.x + i64::from(rect.width) && y < rect.y + i64::from(rect.height);
            if inside { Rgba::new([rgba[0], rgba[1], rgba[2]], rgba[3]) } else { Rgba::TRANSPARENT }
        }
        PlanNode::Group(g) => {
            let mut entries = Vec::new();
            for (li, l) in g.layers.iter().enumerate() {
                let s = eval(plan, ext, l.input.0 as usize, src, x, y);
                let mut cov = 1.0;
                let mut clipped = false;
                if let Some(m) = l.mask {
                    if m.is_clip_to_base() {
                        clipped = true;
                    } else {
                        let served = src.mask(m.key).expect("mask served");
                        assert_eq!(served.bounds, m.placement, "plane served at its plan placement");
                        let owned = OwnedMask { bounds: served.bounds, sample_format: served.sample_format.into(), bytes: served.bytes.to_vec(),
                            stride_bytes: served.stride_bytes, sha256: served.sha256 };
                        let feather = ext.layers.iter().find(|e| e.group.0 as usize == id && e.layer as usize == li).map_or(0.0, |e| e.mask_feather_px);
                        cov = effective(&owned, feather, m.inverted, m.density, x, y);
                    }
                }
                entries.push((Entry { src: s, mode: l.mode, k: l.opacity * l.fill_opacity, clipped }, cov));
            }
            composite_entries(&entries, PixelSite { x, y, seed: 0 })
        }
        PlanNode::Vector(v) => match *v {},
    }
}
impl RenderBackend for Ref {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity { name: "docgraph-ref", version: "0", device: DeviceClass::Cpu, adapter: String::new(), generation: 0 }
    }
    fn exactness(&self, _: Operator) -> Exactness {
        Exactness::Exact
    }
    fn supports_clip_to_base(&self) -> bool {
        true
    }
    fn execute_extended(&self, plan: &RenderPlan<NoVector>, ext: &PlanExtensions, _: &ExecuteRequest, src: &dyn PlanSources,
        _: &CancellationToken, sink: &mut dyn TileSink) -> Result<RenderReceipt, BackendError> {
        for e in &ext.layers {
            assert!(e.transform.is_none() && e.effects.is_empty() && e.blur.is_none() && e.adjustment.is_none(),
                "the reference executor takes mask feather only");
        }
        let e = plan.target.extent;
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in e.y..e.y + i64::from(e.height) {
            for x in e.x..e.x + i64::from(e.width) {
                let p = eval(plan, ext, plan.root.0 as usize, src, x, y);
                for v in p.rgb {
                    colour.extend_from_slice(&v.to_le_bytes());
                }
                alpha.extend_from_slice(&p.a.to_le_bytes());
            }
        }
        sink.accept(e, &colour, &alpha).map_err(BackendError::Sink)?;
        Ok(RenderReceipt {
            plan_revision: plan.revision,
            plan_digest: plan.digest().unwrap(),
            backend: self.identity(),
            region: e,
            target: plan.target,
            exactness: Default::default(),
            chunks: 1,
            nodes_executed: plan.nodes.len() as u64,
            peak_working_bytes: 0,
        })
    }
    fn execute(&self, plan: &RenderPlan<NoVector>, r: &ExecuteRequest, src: &dyn PlanSources, c: &CancellationToken,
        sink: &mut dyn TileSink) -> Result<RenderReceipt, BackendError> {
        self.execute_extended(plan, &PlanExtensions::default(), r, src, c, sink)
    }
}

/// The oracle: independent of the plan, per pixel every layer of [`LAYERS`] bottom first, its
/// tile texel at the moved position, its plane's [`effective`] value (linked planes moved with the
/// layer, the unlinked one in place), the clip run rule for layer 12.
pub fn oracle(pixels: &Pixels) -> Vec<[f32; 4]> {
    let planes: Vec<Option<CutPlane>> = LAYERS.iter().map(layer_plane).collect();
    let mut out = Vec::new();
    for y in 0..i64::from(scenes::H) {
        for x in 0..i64::from(scenes::W) {
            let mut entries = Vec::new();
            for (l, p) in LAYERS.iter().zip(&planes) {
                let (cx, cy) = (x - l.translate[0], y - l.translate[1]);
                let src = if cx >= 0 && cy >= 0 && cx < i64::from(l.w) && cy < i64::from(l.h) {
                    let t = tile_of(pixels, l).at(cx as u32, cy as u32);
                    Rgba::new([t[0], t[1], t[2]], t[3])
                } else {
                    Rgba::TRANSPARENT
                };
                let cov = p.as_ref().map_or(1.0, |c| effective(&c.plane, c.feather_px, c.input.inverted, c.input.density, x, y));
                entries.push((Entry { src, mode: StudioBlendMode::Normal, k: 1.0, clipped: l.clipped }, cov));
            }
            let o = composite_entries(&entries, PixelSite { x, y, seed: 0 });
            out.push([o.rgb[0], o.rgb[1], o.rgb[2], o.a]);
        }
    }
    out
}
