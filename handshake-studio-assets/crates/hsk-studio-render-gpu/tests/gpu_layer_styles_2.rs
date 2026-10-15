//! GPU passes of the layer styles (lane SMB-PS-ADJUST-STYLES-2): satin, bevel & emboss (smooth,
//! chisel hard, chisel soft, soften, stroke emboss; every style and both parts) and gradient
//! overlay. Each case renders a soft-edged blob with its effect stack over an opaque backdrop and
//! is compared with pigment's `render_layer_reference` (the owner of every formula) on a plane
//! holding the layer plus its margin, within a tolerance declared before the run (binary32 passes
//! against pigment's binary64 blur accumulation and shading). Every case also asserts that the
//! stack changes the bare layer. GPU-execution proofs follow `tests/gpu-proof.toml` (V4-03).
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::effect_math::{
    BevelEmbossOp, BevelPart, BevelShape, BevelStroke, BevelStyle, BevelTechnique, EffectOp, EffectPlane,
    GradientOverlayOp, GradientStopOp, GradientStyle, LayerParams, SatinOp, StrokeOp, StrokePosition,
    render_layer_reference,
};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BlendSpace, ColourTag, Exactness, ExecuteRequest, ExtOperator, Group, Layer,
    LayerExtension, NoVector, OutputTarget, PlanExtensions, PlanNode, PlanNodeId, PlanSources, RenderBackend,
    RenderPlan, SampleFormat, SinkError, SourceKey, StudioBlendMode, TileInput, TilePlanes, TileSink,
};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_render_gpu::{DeviceOptions, GpuBackend, Reason};
use std::sync::OnceLock;

const BACKDROP: [f32; 4] = [0.25, 0.45, 0.7, 1.0];
const TOLERANCE: f32 = 2e-5;

fn tag() -> ColourTag {
    ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::LinearLight }
}

/// Soft-edged disc with a hole (one-pixel alpha ramp at the rim), colour varying across the tile.
fn blob(w: u32, h: u32, x: u32, y: u32) -> [f32; 4] {
    let (cx, cy) = ((w as f32 - 1.0) / 2.0, (h as f32 - 1.0) / 2.0);
    let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
    let mut a = (w.min(h) as f32 / 2.0 - 0.5 - d + 0.5).clamp(0.0, 1.0);
    if d < 2.2 {
        a = 0.0;
    }
    [x as f32 / w as f32, y as f32 / h as f32, 0.55, a]
}

struct Tile {
    at: Rect,
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl Tile {
    fn new(at: Rect) -> Self {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in 0..at.height {
            for x in 0..at.width {
                let p = blob(at.width, at.height, x, y);
                for c in &p[..3] {
                    colour.extend_from_slice(&c.to_le_bytes());
                }
                alpha.extend_from_slice(&p[3].to_le_bytes());
            }
        }
        Self { at, colour, alpha }
    }
}
impl PlanSources for Tile {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        (key.0 == 1).then(|| TilePlanes {
            width: self.at.width,
            height: self.at.height,
            colour: &self.colour,
            colour_stride_bytes: u64::from(self.at.width) * 12,
            alpha: &self.alpha,
            alpha_stride_bytes: u64::from(self.at.width) * 4,
            colour_sha256: hsk_studio_prism::profile_hash(&self.colour),
            alpha_sha256: hsk_studio_prism::profile_hash(&self.alpha),
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: tag(),
        })
    }
    fn mask(&self, _key: SourceKey) -> Option<Mask<'_>> {
        None
    }
}

struct Image {
    extent: Rect,
    px: Vec<[f32; 4]>,
    calls: usize,
}
impl TileSink for Image {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        for y in 0..r.height as usize {
            for x in 0..r.width as usize {
                let i = y * r.width as usize + x;
                let at = (r.y - self.extent.y) as usize * self.extent.width as usize + y * self.extent.width as usize + (r.x - self.extent.x) as usize + x;
                self.px[at] = [f(colour, i * 12), f(colour, i * 12 + 4), f(colour, i * 12 + 8), f(alpha, i * 4)];
            }
        }
        self.calls += 1;
        Ok(())
    }
}

fn plan(extent: Rect, tile: &Tile, mode: StudioBlendMode, opacity: f32, fill: f32) -> RenderPlan<NoVector> {
    RenderPlan {
        revision: 9,
        target: OutputTarget { extent, format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, space: tag() },
        nodes: vec![
            PlanNode::Solid { rect: extent, rgba: BACKDROP },
            PlanNode::Tile(TileInput {
                key: SourceKey(1),
                placement: tile.at,
                format: SampleFormat::RgbF32Le,
                alpha: AlphaConvention::Straight,
                colour_sha256: hsk_studio_prism::profile_hash(&tile.colour),
                alpha_sha256: hsk_studio_prism::profile_hash(&tile.alpha),
            }),
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![
                    Layer { input: PlanNodeId(0), mode: StudioBlendMode::Normal, opacity: 1.0, fill_opacity: 1.0, mask: None },
                    Layer { input: PlanNodeId(1), mode, opacity, fill_opacity: fill, mask: None },
                ],
            }),
        ],
        root: PlanNodeId(2),
    }
}

fn backend() -> &'static GpuBackend {
    static SHARED: OnceLock<GpuBackend> = OnceLock::new();
    SHARED.get_or_init(|| GpuBackend::new(DeviceOptions { chunk_side: 24, ..DeviceOptions::default() }))
}

/// `true` when a device is live; without an adapter FAIL (`NOT_PROVEN: no adapter`) under the
/// committed strict policy.
fn gate(what: &str) -> bool {
    let b = backend();
    if b.device_info().is_some() {
        return true;
    }
    if let Some(failure) = b.failure() {
        panic!("{what}: adapter present but the backend failed: {failure:?} ({:?})", b.pipeline_fault());
    }
    let strict = include_str!("gpu-proof.toml").lines().any(|l| l.trim() == "require_gpu = true")
        || std::env::var("HSK_STUDIO_TEST_REQUIRE_GPU").as_deref() == Ok("1");
    match b.unavailable_reason() {
        Some(Reason::NoAdapter) if strict => panic!("NOT_PROVEN: no adapter; {what} requires a GPU (tests/gpu-proof.toml)"),
        Some(Reason::NoAdapter) => {
            eprintln!("NOT_PROVEN: no adapter; {what} not run");
            false
        }
        other => panic!("{what}: no live device ({other:?})"),
    }
}

fn execute(extent: Rect, tile: &Tile, ops: &[EffectOp], layer: (StudioBlendMode, f32, f32)) -> (Result<(), BackendError>, Image) {
    let p = plan(extent, tile, layer.0, layer.1, layer.2);
    let e = PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(2), 1).with_effects(ops.to_vec())]);
    let mut image = Image { extent, px: vec![[f32::NAN; 4]; (extent.width * extent.height) as usize], calls: 0 };
    let request = ExecuteRequest { expected_revision: 9, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 30 };
    let r = backend().execute_extended(&p, &e, &request, tile, &CancellationToken::default(), &mut image).map(|_| ());
    (r, image)
}

/// Pigment's reference on the extent plane (backdrop everywhere, the blob at `tile.at`).
fn reference(extent: Rect, tile: &Tile, ops: &[EffectOp], layer: (StudioBlendMode, f32, f32)) -> EffectPlane {
    let n = (extent.width * extent.height) as usize;
    let back = EffectPlane::new(extent.width, extent.height, vec![[BACKDROP[0], BACKDROP[1], BACKDROP[2]]; n], vec![1.0; n]).unwrap();
    let (mut rgb, mut alpha) = (vec![[0.0f32; 3]; n], vec![0.0f32; n]);
    for y in 0..tile.at.height {
        for x in 0..tile.at.width {
            let p = blob(tile.at.width, tile.at.height, x, y);
            let i = ((tile.at.y - extent.y) as u32 + y) * extent.width + (tile.at.x - extent.x) as u32 + x;
            rgb[i as usize] = [p[0], p[1], p[2]];
            alpha[i as usize] = p[3];
        }
    }
    let content = EffectPlane::new(extent.width, extent.height, rgb, alpha).unwrap();
    render_layer_reference(&back, &content, LayerParams { mode: layer.0, opacity: layer.1, fill_opacity: layer.2 }, ops).unwrap()
}

fn error(image: &Image, want: &EffectPlane) -> f32 {
    let mut worst = 0.0f32;
    for y in 0..want.height() {
        for x in 0..want.width() {
            let g = image.px[(y * want.width() + x) as usize];
            let w = want.pixel(x, y).unwrap();
            worst = worst.max((g[3] - w.a).abs());
            for c in 0..3 {
                worst = worst.max((g[c] * g[3] - w.rgb[c] * w.a).abs());
            }
        }
    }
    worst
}

fn bevel(part: BevelPart, shape: BevelShape) -> EffectOp {
    EffectOp::BevelEmboss(BevelEmbossOp::new(part, if part == BevelPart::Highlight { [1.0, 0.95, 0.8] } else { [0.1, 0.0, 0.2] }, if part == BevelPart::Highlight { StudioBlendMode::Screen } else { StudioBlendMode::Multiply }, 0.8, shape))
}

fn both(shape: BevelShape) -> Vec<EffectOp> {
    vec![bevel(BevelPart::Shadow, shape), bevel(BevelPart::Highlight, shape)]
}

#[test]
fn gpu_styles_match_pigment_reference_within_tolerance() {
    if !gate("gpu_styles_match_pigment_reference_within_tolerance") {
        return;
    }
    let extent = Rect { x: 0, y: 0, width: 50, height: 44 };
    let tile = Tile::new(Rect { x: 14, y: 12, width: 22, height: 20 });
    let normal = (StudioBlendMode::Normal, 0.9, 0.7);
    let base = BevelShape { size_px: 3.0, depth: 220.0, angle_deg: 120.0, altitude_deg: 30.0, ..BevelShape::DEFAULT };
    let mut cases: Vec<(String, Vec<EffectOp>)> = vec![
        ("satin".into(), vec![EffectOp::Satin(SatinOp::new([0.1, 0.1, 0.3], StudioBlendMode::Multiply, 0.7, 19.0, 4.0, 3.0, false))]),
        ("satin inverted".into(), vec![EffectOp::Satin(SatinOp::new([0.9, 0.8, 0.2], StudioBlendMode::Normal, 0.6, -140.0, 3.0, 2.0, true))]),
    ];
    for style in [BevelStyle::InnerBevel, BevelStyle::OuterBevel, BevelStyle::Emboss, BevelStyle::PillowEmboss] {
        for technique in [BevelTechnique::Smooth, BevelTechnique::ChiselHard, BevelTechnique::ChiselSoft] {
            cases.push((format!("bevel {style:?} {technique:?}"), both(BevelShape { style, technique, up: style != BevelStyle::OuterBevel, ..base })));
        }
    }
    cases.push(("bevel soften".into(), both(BevelShape { soften_px: 2.0, ..base })));
    cases.push(("bevel chisel soften".into(), both(BevelShape { technique: BevelTechnique::ChiselHard, soften_px: 1.5, ..base })));
    for position in [StrokePosition::Outside, StrokePosition::Inside, StrokePosition::Centre] {
        let stroke = StrokeOp::new([0.9, 0.1, 0.1], StudioBlendMode::Normal, 1.0, 3.0, position);
        let mut ops = vec![EffectOp::Stroke(stroke)];
        ops.extend(both(BevelShape { style: BevelStyle::StrokeEmboss, stroke: Some(BevelStroke { size_px: 3.0, position }), ..base }));
        cases.push((format!("stroke emboss {position:?}"), ops));
    }
    for (name, ops) in &cases {
        let (r, image) = execute(extent, &tile, ops, normal);
        r.unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let want = reference(extent, &tile, ops, normal);
        let bare = reference(extent, &tile, &[], normal);
        let changed = (0..want.height()).flat_map(|y| (0..want.width()).map(move |x| (x, y))).filter(|(x, y)| {
            let (a, b) = (want.pixel(*x, *y).unwrap(), bare.pixel(*x, *y).unwrap());
            (a.a - b.a).abs() > 1e-3 || a.rgb.iter().zip(&b.rgb).any(|(p, q)| (p - q).abs() > 1e-3)
        }).count();
        let err = error(&image, &want);
        println!("GPU_STYLES_2 {name:<34} max err {err:e} (tolerance {TOLERANCE:e}), {changed} pixels changed by the stack");
        assert!(changed > 0, "{name}: the stack changes nothing");
        assert!(err <= TOLERANCE, "{name}: {err:e}");
    }
    // Gradient overlay: the layer covers the extent (the reference's gradient geometry is its plane,
    // the windowed evaluator's and the GPU's the layer bounds: they agree when both are the extent).
    let full = Tile::new(extent);
    let stops = [
        GradientStopOp { position: 0.1, colour: [0.9, 0.1, 0.2], opacity: 1.0 },
        GradientStopOp { position: 0.6, colour: [0.1, 0.8, 0.3], opacity: 0.4 },
        GradientStopOp { position: 1.0, colour: [0.2, 0.3, 1.0], opacity: 0.9 },
    ];
    for style in [GradientStyle::Linear, GradientStyle::Radial, GradientStyle::Angle, GradientStyle::Reflected, GradientStyle::Diamond] {
        let g = GradientOverlayOp::new(StudioBlendMode::Screen, 0.85, style, 30.0, &stops).unwrap().with_scale(0.7).with_offset([0.1, -0.2]);
        let ops = [EffectOp::GradientOverlay(g)];
        let (r, image) = execute(extent, &full, &ops, normal);
        r.unwrap();
        let err = error(&image, &reference(extent, &full, &ops, normal));
        println!("GPU_STYLES_2 gradient {style:?} max err {err:e} (tolerance {TOLERANCE:e})");
        assert!(err <= TOLERANCE, "gradient {style:?}: {err:e}");
    }
    for kind in [hsk_studio_pigment::effect_math::EffectKind::Satin, hsk_studio_pigment::effect_math::EffectKind::BevelEmboss, hsk_studio_pigment::effect_math::EffectKind::GradientOverlay] {
        assert_eq!(backend().extension_exactness(ExtOperator::Effect(kind)), Exactness::Approximate);
    }
}

/// Hostile size: a chisel bevel whose distance window exceeds the per-pass cost even at the
/// smallest frame is a typed refusal (`effect_cost`) before any sink call; a stroke emboss without
/// a stroke draws nothing (the result equals the bare layer).
#[test]
fn gpu_chisel_cost_refusal_and_empty_stroke_emboss() {
    if !gate("gpu_chisel_cost_refusal_and_empty_stroke_emboss") {
        return;
    }
    let extent = Rect { x: 0, y: 0, width: 30, height: 30 };
    let tile = Tile::new(Rect { x: 5, y: 5, width: 20, height: 20 });
    let huge = both(BevelShape { technique: BevelTechnique::ChiselHard, size_px: 900.0, ..BevelShape::DEFAULT });
    let (r, image) = execute(extent, &tile, &huge, (StudioBlendMode::Normal, 1.0, 1.0));
    assert!(matches!(r, Err(BackendError::Unsupported { operator: "effect_cost", .. })), "{r:?}");
    assert_eq!(image.calls, 0);
    let empty = both(BevelShape { style: BevelStyle::StrokeEmboss, stroke: None, ..BevelShape::DEFAULT });
    let (r, with) = execute(extent, &tile, &empty, (StudioBlendMode::Normal, 1.0, 1.0));
    r.unwrap();
    let (r, bare) = execute(extent, &tile, &[], (StudioBlendMode::Normal, 1.0, 1.0));
    r.unwrap();
    assert_eq!(with.px, bare.px);
}
