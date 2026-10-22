//! Shared vector-mask fixture of lane SMB-COMPOSITE-VECTOR-MASK (folio-free). Included with
//! `#[path]` by render-cpu / render-gpu `tests/vector_mask.rs` (decision B1): the nib layer of
//! `vector_scenes.rs` masked four ways, lowered by `vector_layer_render_plan`, every pixel compared
//! with [`oracle_masked`].
//!
//! Mask source (rendered alone, crate::vector_mask): a group (opacity 0.9) of `A` (yellow square,
//! normal 60 %) and `B` (blue triangle, multiply). Cases: VECTOR (the diamond of
//! `vector_scenes::clip_outline`), VECTOR inverted (pigment's mask invert), ALPHA of the group, and
//! LUMINANCE of the group inverted inside a fractional region (baked on the host).
//!
//! The oracle is the unmasked layer oracle (`vector_scenes::oracle`) cut by the lowered mask plane
//! (`coverage_u16le / 65535`, inverted by the plan flag, zero outside the plane); the mask values
//! themselves are proven against nib and pigment by composite `tests/vector_mask.rs`.
#![allow(dead_code)]
#[path = "vector_scenes.rs"]
pub mod base;

use hsk_studio_composite::vector::{FillRule, OwnedPath, PathElement, PathPoint};
use hsk_studio_composite::{
    FillPaint, FillSpec, InputsLowering, MaskContentMode, MaskNode, OwnedMask, PendingMaskSource,
    PreparedVectors, StaticContent, Surface, VectorCoverage, VectorGeometry, VectorLayerSpec, VectorMask,
    VectorMaskSource, VectorPlane, VectorResolution, mask_content, prepare_vector_layers,
};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::coverage_math::{feather_kernel, mask_coverage};
use std::collections::BTreeMap;
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, StudioBlendMode, composite_over};
use hsk_studio_pigment::render_plan::PlanNode;

pub const MASK_A: &str = "SLYR-019abcde-0000-7000-8000-00000000000b";
pub const MASK_B: &str = "SLYR-019abcde-0000-7000-8000-00000000000c";
pub const CASES: [&str; 4] = ["vector", "vector_inverted", "alpha", "luminance_inverted_region"];

fn poly(points: &[(f64, f64)]) -> OwnedPath {
    let mut elements = vec![PathElement::MoveTo(PathPoint::new(points[0].0, points[0].1))];
    elements.extend(points[1..].iter().map(|p| PathElement::LineTo(PathPoint::new(p.0, p.1))));
    elements.push(PathElement::Close);
    OwnedPath { elements, fill_rule: FillRule::NonZero }
}
fn solid(id: &str, path: OwnedPath, rgba: [f32; 4], mode: StudioBlendMode) -> VectorLayerSpec {
    VectorLayerSpec {
        layer_id: id.to_owned(),
        geometry: VectorGeometry::Paths(vec![path]),
        fills: vec![FillSpec { paint: FillPaint::Solid(rgba), mode, opacity: 1.0 }],
    }
}

/// The nib layer of `vector_scenes` (optionally moved by `(dx, dy)`) and the two mask-source layers.
pub fn prepared_moved(dx: f64, dy: f64) -> PreparedVectors {
    let mut layer = base::spec();
    if let VectorGeometry::Paths(paths) = &mut layer.geometry {
        for p in paths {
            for e in &mut p.elements {
                let mv = |q: &mut PathPoint| *q = PathPoint::new(q.x + dx, q.y + dy);
                match e {
                    PathElement::MoveTo(q) | PathElement::LineTo(q) => mv(q),
                    PathElement::QuadTo(c, q) => {
                        mv(c);
                        mv(q);
                    }
                    PathElement::CubicTo(c1, c2, q) => {
                        mv(c1);
                        mv(c2);
                        mv(q);
                    }
                    PathElement::Close => {}
                }
            }
        }
    }
    let a = solid(MASK_A, poly(&[(6.0, 3.5), (22.5, 3.5), (22.5, 18.0), (6.0, 18.0)]), [0.95, 0.85, 0.1, 1.0], StudioBlendMode::Normal);
    let b = solid(MASK_B, poly(&[(12.0, 8.0), (31.0, 12.5), (15.5, 23.0)]), [0.1, 0.3, 0.9, 0.8], StudioBlendMode::Normal);
    let res = VectorResolution { coverage: VectorCoverage::Exact, ..VectorResolution::default() };
    prepare_vector_layers(&[layer, a, b], &res, &CancellationToken::default()).unwrap()
}
pub fn prepared() -> PreparedVectors {
    prepared_moved(0.0, 0.0)
}

/// The mask source group: opacity 0.9, `A` normal 60 %, `B` multiply.
pub fn source() -> MaskNode {
    MaskNode::Group {
        mode: StudioBlendMode::Normal,
        opacity: 0.9,
        children: vec![
            MaskNode::Leaf { layer_id: MASK_A.into(), mode: StudioBlendMode::Normal, opacity: 0.6 },
            MaskNode::Leaf { layer_id: MASK_B.into(), mode: StudioBlendMode::Multiply, opacity: 1.0 },
        ],
    }
}

/// The mask of case `name` (one of [`CASES`]) over `vectors`.
pub fn mask(name: &str, vectors: &PreparedVectors) -> VectorMask {
    let content = |mode| mask_content(vectors, &source(), mode, &mut (1 << 24)).unwrap();
    match name {
        "vector" => VectorMask::of(VectorMaskSource::Outline(Some(base::clip_outline()))),
        "vector_inverted" => VectorMask { invert: true, ..VectorMask::of(VectorMaskSource::Outline(Some(base::clip_outline()))) },
        "alpha" => VectorMask::of(content(MaskContentMode::Alpha)),
        "luminance_inverted_region" => VectorMask {
            invert: true,
            region: Some([3.25, 2.0, 26.5, 20.75]),
            ..VectorMask::of(content(MaskContentMode::Luminance))
        },
        other => panic!("case {other}"),
    }
}

/// `prepared()` with `base::LAYER` masked by case `name`.
pub fn masked(name: &str) -> PreparedVectors {
    let v = prepared();
    let m = mask(name, &v);
    v.with_mask(base::LAYER, m)
}

/// Stored value of a `coverage_u16le` plane at `(x, y)`, clamped into the plane when `clamp`
/// (feather taps), else zero outside.
pub fn stored(r: Rect, bytes: &[u8], x: i64, y: i64, clamp: bool) -> f32 {
    let (x, y) = if clamp {
        (x.clamp(r.x, r.x + i64::from(r.width) - 1), y.clamp(r.y, r.y + i64::from(r.height) - 1))
    } else {
        (x, y)
    };
    let inside = x >= r.x && y >= r.y && x < r.x + i64::from(r.width) && y < r.y + i64::from(r.height);
    if !inside {
        return 0.0;
    }
    let i = (((y - r.y) * i64::from(r.width) + (x - r.x)) * 2) as usize;
    (f64::from(u16::from_le_bytes([bytes[i], bytes[i + 1]])) / 65535.0) as f32
}

/// Effective coverage at `(x, y)` of a coverage_u16le plane under pigment's rule: feathered first
/// (`coverage_math::feather_kernel`, horizontal then vertical, taps clamped to the plane, binary64
/// sums, clamped into [0, 1]), then `coverage_math::mask_coverage` (invert, density).
pub fn effective(r: Rect, bytes: &[u8], feather_px: f32, inverted: bool, density: f32, x: i64, y: i64) -> f32 {
    let raw = if feather_px > 0.0 {
        let taps = feather_kernel(feather_px).unwrap();
        let s = (taps.len() / 2) as i64;
        let row = |yy: i64| -> f32 {
            let mut sum = 0.0f64;
            for (t, w) in taps.iter().enumerate() {
                sum += f64::from(*w) * f64::from(stored(r, bytes, x - s + t as i64, yy, true));
            }
            sum as f32
        };
        let mut sum = 0.0f64;
        for (t, w) in taps.iter().enumerate() {
            sum += f64::from(*w) * f64::from(row(y - s + t as i64));
        }
        (sum as f32).clamp(0.0, 1.0)
    } else {
        stored(r, bytes, x, y, false)
    };
    mask_coverage(raw, inverted, density).unwrap()
}

/// The lowered mask's effective value at `(x, y)` (the wrapper layer of `vector_layer_render_plan`:
/// its plane, invert and density flags and its envelope feather).
pub fn mask_at(low: &InputsLowering, x: i64, y: i64) -> f32 {
    let PlanNode::Group(root) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!("wrapped root") };
    let m = root.layers[0].mask.expect("mask on the wrapper");
    let [(_, plane)] = low.clip_planes.as_slice() else { panic!("one plane") };
    assert!(plane.wide);
    let feather = low
        .extensions
        .layers
        .iter()
        .find(|e| e.group == low.plan.root && e.layer == 0)
        .map_or(0.0, |e| e.mask_feather_px);
    effective(plane.placement, &plane.coverage, feather, m.inverted, m.density, x, y)
}

/// A static subtree (`SceneContent::Static`) of a lowering that owns every plane it reads.
pub fn static_content(low: &InputsLowering, vectors: &PreparedVectors) -> StaticContent {
    let own = |m: hsk_studio_pigment::Mask<'_>| OwnedMask {
        bounds: m.bounds,
        sample_format: m.sample_format.to_owned(),
        bytes: m.bytes.to_vec(),
        stride_bytes: m.stride_bytes,
        sha256: m.sha256,
    };
    let mut owned_masks = BTreeMap::new();
    let mut owned_tiles = BTreeMap::new();
    let keys = low.vector_masks.iter().map(|(k, _)| *k)
        .chain(low.vector_rows.iter().map(|(k, _, _)| *k))
        .chain(low.clip_planes.iter().map(|(k, _)| *k));
    for key in keys {
        if let Some(m) = low.vector_mask(key, vectors) {
            owned_masks.insert(key, own(m));
        } else if let Some(t) = low.vector_paint(key, vectors, base::space()) {
            owned_tiles.insert(key, Surface {
                width: t.width,
                height: t.height,
                colour: t.colour.to_vec(),
                alpha: t.alpha.to_vec(),
                colour_sha256: t.colour_sha256,
                alpha_sha256: t.alpha_sha256,
            });
        }
    }
    StaticContent {
        plan: low.plan.clone(),
        extensions: low.extensions.clone(),
        rect: low.plan.target.extent,
        tiles: BTreeMap::new(),
        masks: BTreeMap::new(),
        owned_masks,
        owned_tiles,
    }
}

/// A raster source tile for the caller-rendered case (lane SMB-COMPOSITE-VECTOR-MASK-2): 20x16 at
/// (6, 4), a colour ramp with alpha rising left to right and a transparent hole.
pub const SOURCE_RECT: Rect = Rect { x: 6, y: 4, width: 20, height: 16 };
pub fn source_pixels() -> Vec<[f32; 4]> {
    let mut px = Vec::new();
    for y in 0..SOURCE_RECT.height {
        for x in 0..SOURCE_RECT.width {
            let a = if (8..12).contains(&x) && (6..10).contains(&y) { 0.0 } else { (x as f32 + 1.0) / 20.0 };
            px.push([x as f32 / 19.0, y as f32 / 15.0, 0.5, a]);
        }
    }
    px
}
/// `prepared()` with `base::LAYER` waiting for a caller-rendered ALPHA source (`mode`), feathered.
pub fn pending(mode: MaskContentMode) -> PreparedVectors {
    prepared().with_pending_mask(PendingMaskSource {
        mask_id: "m-raster".into(),
        source_layer_id: "SLYR-raster-source".into(),
        mode,
        layers: vec![base::LAYER.into()],
        template: VectorMask { feather_px: 1.5, ..VectorMask::of(VectorMaskSource::Content { rect: None, values: Vec::new(), outside: mode.outside() }) },
    })
}

/// [`base::oracle`] of `plane` cut by the lowered mask of `low`.
pub fn oracle_masked(plane: &VectorPlane, low: &InputsLowering) -> Vec<[f32; 4]> {
    base::oracle(plane)
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
            let (x, y) = ((i % base::W as usize) as i64, (i / base::W as usize) as i64);
            let o = composite_over(StudioBlendMode::Normal, Rgba::TRANSPARENT, Rgba::new([p[0], p[1], p[2]], p[3]),
                mask_at(low, x, y), PixelSite { x, y, seed: 0 }).unwrap();
            [o.rgb[0], o.rgb[1], o.rgb[2], o.a]
        })
        .collect()
}

// ---- Lane SMB-COMPOSITE-VECTOR-MASK-2 cases (shared by render-cpu / render-gpu `tests/vector_mask_2.rs`) ----
use hsk_studio_composite::{
    SceneComposition, SceneContent, SceneContext, SceneLayer, SceneLimits, ScenePixels, render_scene,
    unlinked_mask_matte, vector_layer_render_plan,
};
use hsk_studio_pigment::Mask;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, Exactness, ExecuteRequest, Group, Layer, MaskFormat, MaskInput, OutputTarget,
    PlanNodeId, PlanSources, RenderBackend, RenderPlan, SampleFormat, SinkError, SourceKey, TileInput,
    TilePlanes, TileSink,
};
use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter};
use base as scenes;

struct Frame(usize, Vec<[f32; 4]>);
impl TileSink for Frame {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], i: usize| f32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        for row in 0..r.height as usize {
            for col in 0..r.width as usize {
                let i = row * r.width as usize + col;
                self.1[(r.y as usize + row) * self.0 + r.x as usize + col] =
                    [f(colour, i * 12), f(colour, i * 12 + 4), f(colour, i * 12 + 8), f(alpha, i * 4)];
            }
        }
        Ok(())
    }
}
fn target(extent: Rect) -> OutputTarget {
    OutputTarget { extent, format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, space: scenes::space() }
}
fn full() -> Rect {
    Rect { x: 0, y: 0, width: scenes::W, height: scenes::H }
}
fn request() -> ExecuteRequest {
    ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 }
}
fn render(b: &dyn RenderBackend, plan: &RenderPlan, ext: &hsk_studio_pigment::render_plan::PlanExtensions, sources: &dyn PlanSources, w: u32, h: u32) -> Result<Vec<[f32; 4]>, String> {
    let mut frame = Frame(w as usize, vec![[0.0; 4]; (w * h) as usize]);
    b.execute_extended(plan, ext, &request(), sources, &CancellationToken::default(), &mut frame)
        .map_err(|e| format!("{e:?}"))?;
    Ok(frame.1)
}
fn layer_frame(b: &dyn RenderBackend, low: &InputsLowering, vectors: &PreparedVectors) -> Vec<[f32; 4]> {
    render(b, &low.plan, &low.extensions, &scenes::Sources { low, vectors }, scenes::W, scenes::H).expect("renders")
}
fn over(c: [f32; 4], k: f32, x: i64, y: i64) -> [f32; 4] {
    let o = composite_over(StudioBlendMode::Normal, Rgba::TRANSPARENT, Rgba::new([c[0], c[1], c[2]], c[3]), k, PixelSite { x, y, seed: 0 }).unwrap();
    [o.rgb[0], o.rgb[1], o.rgb[2], o.a]
}

/// Raster source tile planes (straight `rgb_f32le` + `f32le`).
struct SourceTile(Vec<u8>, Vec<u8>);
impl SourceTile {
    fn new() -> Self {
        let (mut c, mut a) = (Vec::new(), Vec::new());
        for p in source_pixels() {
            for v in &p[..3] {
                c.extend_from_slice(&v.to_le_bytes());
            }
            a.extend_from_slice(&p[3].to_le_bytes());
        }
        Self(c, a)
    }
    fn plan(&self) -> RenderPlan {
        RenderPlan {
            revision: 7,
            target: target(SOURCE_RECT),
            nodes: vec![PlanNode::Tile(TileInput {
                key: SourceKey(0),
                placement: SOURCE_RECT,
                format: SampleFormat::RgbF32Le,
                alpha: AlphaConvention::Straight,
                colour_sha256: hsk_studio_prism::profile_hash(&self.0),
                alpha_sha256: hsk_studio_prism::profile_hash(&self.1),
            })],
            root: PlanNodeId(0),
        }
    }
}
impl PlanSources for SourceTile {
    fn tile(&self, _: SourceKey) -> Option<TilePlanes<'_>> {
        let w = SOURCE_RECT.width;
        Some(TilePlanes {
            width: w,
            height: SOURCE_RECT.height,
            colour: &self.0,
            colour_stride_bytes: u64::from(w) * 12,
            alpha: &self.1,
            alpha_stride_bytes: u64::from(w) * 4,
            colour_sha256: hsk_studio_prism::profile_hash(&self.0),
            alpha_sha256: hsk_studio_prism::profile_hash(&self.1),
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: scenes::space(),
        })
    }
    fn mask(&self, _: SourceKey) -> Option<Mask<'_>> {
        None
    }
}

/// The masked layer against the shared oracle; returns the max |delta|.
fn check(b: &dyn RenderBackend, vectors: &PreparedVectors) -> f32 {
    let plane = vectors.plane(scenes::LAYER).unwrap();
    let low = vector_layer_render_plan(vectors, scenes::LAYER, 7, target(full())).unwrap();
    let got = layer_frame(b, &low, vectors);
    scenes::max_delta(&got, &oracle_masked(plane, &low))
}

/// Every lane-2 case on backend `b` against the shared oracle: `(case, max |delta|)`. Structural
/// facts (refusal before the pigment rewrite, the resolved plane, the unlinked mask cutting the
/// moved content) are asserted here; the caller asserts its tolerance.
pub fn lane2_cases(b: &dyn RenderBackend) -> Vec<(&'static str, f32)> {
    let mut out = Vec::new();
    // (1) Density 40 % + feather 2 px on the VECTOR diamond; Illustrator reveal_outside luminance.
    let v = prepared();
    let dense = VectorMask { density: 0.4, feather_px: 2.0, ..mask("vector", &v) };
    let vectors = v.with_mask(scenes::LAYER, dense);
    let low = vector_layer_render_plan(&vectors, scenes::LAYER, 7, target(full())).unwrap();
    assert_eq!(low.extensions.layers[0].mask_feather_px, 2.0);
    out.push(("density_feather", check(b, &vectors)));
    let v = prepared();
    let revealed = hsk_studio_composite::mask_content(&v, &source(), MaskContentMode::LuminanceRevealed, &mut (1 << 24)).unwrap();
    let vectors = v.with_mask(scenes::LAYER, VectorMask::of(revealed));
    out.push(("reveal_outside", check(b, &vectors)));
    let low = vector_layer_render_plan(&vectors, scenes::LAYER, 7, target(full())).unwrap();
    let [(_, rp)] = low.clip_planes.as_slice() else { panic!("one plane") };
    let corner = (rp.placement.x, rp.placement.y);
    assert!(corner.0 < 6 && corner.1 < 3, "the plane corner lies outside the mask art: {corner:?}");
    assert_eq!(mask_at(&low, corner.0, corner.1), 1.0, "white outside the mask art");

    // (2) A caller-rendered raster ALPHA source (feathered 1.5 px): render, resolve, render.
    let tile = SourceTile::new();
    let source = render(b, &tile.plan(), &Default::default(), &tile, SOURCE_RECT.width + 6, SOURCE_RECT.height + 4).unwrap();
    let w = (SOURCE_RECT.width + 6) as usize;
    let frame: Vec<[f32; 4]> = (0..SOURCE_RECT.height as usize)
        .flat_map(|y| (0..SOURCE_RECT.width as usize).map(move |x| (x, y)))
        .map(|(x, y)| source[(y + 4) * w + x + 6])
        .collect();
    let mut vectors = pending(MaskContentMode::Alpha);
    assert!(vector_layer_render_plan(&vectors, scenes::LAYER, 7, target(full())).unwrap_err().to_string().contains("vector_mask_source_not_rendered"));
    vectors.resolve_mask_source("m-raster", Some(SOURCE_RECT), &frame).unwrap();
    let low = vector_layer_render_plan(&vectors, scenes::LAYER, 7, target(full())).unwrap();
    // The mask is the tile's alpha (before feather): a pixel of the hole and of the ramp.
    let plane_at = |x, y| { let [(_, p)] = low.clip_planes.as_slice() else { panic!() }; stored(p.placement, &p.coverage, x, y, false) };
    assert_eq!(plane_at(6 + 9, 4 + 7), 0.0);
    assert!((plane_at(6 + 3, 4 + 1) - 0.2).abs() < 1e-4);
    out.push(("raster_source", check(b, &vectors)));

    // (3) Pigment's pass-through isolation: refused before, the isolated reading after.
    let diamond = vector_layer_render_plan(&masked("vector"), scenes::LAYER, 7, target(full())).unwrap();
    let [(_, plane)] = diamond.clip_planes.as_slice() else { panic!() };
    let (pr, pbytes) = (plane.placement, plane.coverage.clone());
    struct PlaneSource(Rect, Vec<u8>);
    impl PlanSources for PlaneSource {
        fn tile(&self, _: SourceKey) -> Option<TilePlanes<'_>> {
            None
        }
        fn mask(&self, _: SourceKey) -> Option<Mask<'_>> {
            Some(Mask { bounds: self.0, sample_format: "coverage_u16le", bytes: &self.1, stride_bytes: u64::from(self.0.width) * 2, sha256: hsk_studio_prism::profile_hash(&self.1) })
        }
    }
    let src = PlaneSource(pr, pbytes.clone());
    let (back, fill) = ([0.1f32, 0.6, 0.3, 1.0], [0.9f32, 0.2, 0.4, 0.8]);
    let layer = |input, mode, mask| Layer { input: PlanNodeId(input), mode, opacity: 1.0, fill_opacity: 1.0, mask };
    let mut plan = RenderPlan {
        revision: 7,
        target: target(full()),
        nodes: vec![
            PlanNode::Solid { rect: full(), rgba: back },
            PlanNode::Solid { rect: pr, rgba: fill },
            PlanNode::Group(Group { isolated: false, layers: vec![layer(1, StudioBlendMode::Multiply, None)] }),
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![
                    layer(0, StudioBlendMode::Normal, None),
                    layer(2, StudioBlendMode::PassThrough, Some(MaskInput {
                        key: SourceKey(0), placement: pr, format: MaskFormat::CoverageU16Le,
                        sha256: hsk_studio_prism::profile_hash(&pbytes), inverted: false, density: 1.0,
                    })),
                ],
            }),
        ],
        root: PlanNodeId(3),
    };
    assert!(plan.has_pass_through_mask());
    let refused = render(b, &plan, &Default::default(), &src, scenes::W, scenes::H).unwrap_err();
    assert!(refused.contains("mask_on_pass_through"), "{refused}");
    assert!(plan.isolate_masked_pass_through(PlanNodeId(3), 0).is_err(), "a Normal layer is not rewritten");
    plan.isolate_masked_pass_through(PlanNodeId(3), 1).unwrap();
    assert!(!plan.has_pass_through_mask());
    let got = render(b, &plan, &Default::default(), &src, scenes::W, scenes::H).unwrap();
    let mut want = Vec::new();
    for y in 0..scenes::H as i64 {
        for x in 0..scenes::W as i64 {
            let site = PixelSite { x, y, seed: 0 };
            let inside = x >= pr.x && y >= pr.y && x < pr.x + i64::from(pr.width) && y < pr.y + i64::from(pr.height);
            let b = Rgba::new([back[0], back[1], back[2]], back[3]);
            let out = if inside {
                let inner = composite_over(StudioBlendMode::Multiply, Rgba::TRANSPARENT, Rgba::new([fill[0], fill[1], fill[2]], fill[3]), 1.0, site).unwrap();
                composite_over(StudioBlendMode::Normal, b, inner, stored(pr, &pbytes, x, y, false), site).unwrap()
            } else {
                b
            };
            want.push([out.rgb[0], out.rgb[1], out.rgb[2], out.a]);
        }
    }
    out.push(("pass_through_isolated", scenes::max_delta(&got, &want)));

    // (4) Unlinked in a composition: the layer moves by (5, 3), its mask stays in place.
    let v = prepared();
    let mask = VectorMask { linked: false, ..mask("alpha", &v) };
    let unmasked = vector_layer_render_plan(&v, scenes::LAYER, 7, target(full())).unwrap();
    let content = static_content(&unmasked, &v);
    let matte = unlinked_mask_matte(&mask, scenes::LAYER, scenes::space(), 7).unwrap();
    let SceneContent::Static(mc) = &matte.layer.as_ref().unwrap().content else { panic!() };
    let mp = mc.owned_masks.values().next().unwrap().clone();
    let (dx, dy) = (5i64, 3i64);
    let (sw, sh) = (scenes::W + 8, scenes::H + 6);
    let scene = SceneComposition {
        composition_id: "unlinked".into(),
        width: sw,
        height: sh,
        background: None,
        filter: ResampleFilter::Bilinear,
        layers: vec![SceneLayer {
            layer_id: scenes::LAYER.into(),
            content: SceneContent::Static(Box::new(content)),
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            fill_opacity: 1.0,
            matrix: Affine2 { m: [1.0, 0.0, dx as f64, 0.0, 1.0, dy as f64] },
            matte: Some(matte),
            clipped: false,
        }],
    };
    struct NoPixels;
    impl ScenePixels for NoPixels {
        fn tile(&self, _: &str) -> Option<TilePlanes<'_>> {
            None
        }
    }
    let ctx = SceneContext { backend: b, pixels: &NoPixels, request: request(), space: scenes::space(), limits: SceneLimits::default() };
    let rendered = render_scene(&scene, &ctx, &CancellationToken::default()).expect("scene renders");
    let still = layer_frame(b, &unmasked, &v);
    let f = |b: &[u8], i: usize| f32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let mut worst = 0.0f32;
    let mut cut = false;
    for y in 0..i64::from(sh) {
        for x in 0..i64::from(sw) {
            let i = (y * i64::from(sw) + x) as usize;
            let got = [f(&rendered.colour, i * 12), f(&rendered.colour, i * 12 + 4), f(&rendered.colour, i * 12 + 8), f(&rendered.alpha, i * 4)];
            let (sx, sy) = (x - dx, y - dy);
            let content = if sx >= 0 && sy >= 0 && sx < i64::from(scenes::W) && sy < i64::from(scenes::H) {
                still[(sy * i64::from(scenes::W) + sx) as usize]
            } else {
                [0.0; 4]
            };
            // The mask at the UNMOVED position (composition space).
            let k = stored(mp.bounds, &mp.bytes, x, y, false);
            cut |= content[3] > 0.0 && k < 0.5;
            worst = worst.max(scenes::max_delta(&[got], &[over(content, k, x, y)]));
        }
    }
    assert!(cut, "the unlinked mask cuts the moved content");
    out.push(("unlinked_composition", worst));
    out
}
