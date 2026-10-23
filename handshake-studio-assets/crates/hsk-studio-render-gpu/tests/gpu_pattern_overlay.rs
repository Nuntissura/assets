//! Pattern overlay on the GPU backend (lane SMB-PS-ADJUST-STYLES-2): the pattern pixels are a plan
//! tile source verified and decoded at prepare; the per-pixel colours and pattern alphas come from
//! pigment's `PatternOverlayOp::sample_at` (the owner) and composite through the existing kernel.
//! Compared with pigment's `render_layer_reference_with` within a declared tolerance; an unresolved
//! pattern is a typed refusal before any sink call. GPU-execution proofs follow
//! `tests/gpu-proof.toml` (V4-03).
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::effect_math::{
    EffectKind, EffectOp, EffectPlane, LayerParams, PatternOverlayOp, PatternPixels, PatternRef,
    render_layer_reference_with,
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
    pattern_colour: Vec<u8>,
    pattern_alpha: Vec<u8>,
}
/// The 4 x 3 pattern.
fn pattern_px(x: u32, y: u32) -> [f32; 4] {
    [x as f32 / 4.0, y as f32 / 3.0, 0.5, if (x + y).is_multiple_of(3) { 0.5 } else { 1.0 }]
}
struct Px(Vec<[f32; 4]>);
impl PatternPixels for Px {
    fn pattern(&self, key: SourceKey) -> Option<&[[f32; 4]]> {
        (key == SourceKey(9)).then_some(self.0.as_slice())
    }
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
        let (mut pattern_colour, mut pattern_alpha) = (Vec::new(), Vec::new());
        for y in 0..3 {
            for x in 0..4 {
                let p = pattern_px(x, y);
                for c in &p[..3] {
                    pattern_colour.extend_from_slice(&c.to_le_bytes());
                }
                pattern_alpha.extend_from_slice(&p[3].to_le_bytes());
            }
        }
        Self { at, colour, alpha, pattern_colour, pattern_alpha }
    }
    fn pattern_ref(&self) -> PatternRef {
        PatternRef {
            key: SourceKey(9),
            width: 4,
            height: 3,
            colour_sha256: hsk_studio_prism::profile_hash(&self.pattern_colour),
            alpha_sha256: hsk_studio_prism::profile_hash(&self.pattern_alpha),
        }
    }
}
impl PlanSources for Tile {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        if key.0 == 9 {
            return Some(TilePlanes {
                width: 4,
                height: 3,
                colour: &self.pattern_colour,
                colour_stride_bytes: 48,
                alpha: &self.pattern_alpha,
                alpha_stride_bytes: 16,
                colour_sha256: hsk_studio_prism::profile_hash(&self.pattern_colour),
                alpha_sha256: hsk_studio_prism::profile_hash(&self.pattern_alpha),
                format: SampleFormat::RgbF32Le,
                alpha_convention: AlphaConvention::Straight,
                space: tag(),
            });
        }
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
        panic!("{what}: adapter present but the backend failed: {failure:?}");
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
    let pixels: Vec<[f32; 4]> = (0..12).map(|i| pattern_px(i % 4, i / 4)).collect();
    render_layer_reference_with(&back, &content, LayerParams { mode: layer.0, opacity: layer.1, fill_opacity: layer.2 }, ops, &Px(pixels))
        .unwrap()
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


#[test]
fn gpu_pattern_overlay_matches_pigment_reference_within_tolerance() {
    if !gate("gpu_pattern_overlay_matches_pigment_reference_within_tolerance") {
        return;
    }
    // the layer covers the extent: the reference plane and the layer bounds coincide
    let extent = Rect { x: 0, y: 0, width: 30, height: 22 };
    let tile = Tile::new(extent);
    let r = tile.pattern_ref();
    let cases = [
        ("pattern screen scaled", vec![EffectOp::PatternOverlay(PatternOverlayOp::new(StudioBlendMode::Screen, 0.7, r).with_scale(1.5).with_phase([1.0, -2.0]))]),
        ("pattern normal unlinked", vec![EffectOp::PatternOverlay(PatternOverlayOp::new(StudioBlendMode::Normal, 0.5, r).with_link(false))]),
    ];
    for (name, ops) in &cases {
        eprintln!("GPU_PATTERN start {name}");
        let (res, image) = execute(extent, &tile, ops, (StudioBlendMode::Multiply, 0.9, 1.0));
        res.unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let err = error(&image, &reference(extent, &tile, ops, (StudioBlendMode::Multiply, 0.9, 1.0)));
        println!("GPU_PATTERN {name:<26} max err {err:e} (tolerance {TOLERANCE:e})");
        assert!(err <= TOLERANCE, "{name}: {err:e}");
    }
    assert_eq!(backend().extension_exactness(ExtOperator::Effect(EffectKind::PatternOverlay)), Exactness::Approximate);
    // an unresolved pattern (key 10) is a typed refusal before any sink call
    let missing = vec![EffectOp::PatternOverlay(PatternOverlayOp::new(StudioBlendMode::Normal, 1.0, PatternRef { key: SourceKey(10), ..r }))];
    eprintln!("GPU_PATTERN start unresolved");
    let (res, image) = execute(extent, &tile, &missing, (StudioBlendMode::Normal, 1.0, 1.0));
    assert!(matches!(res, Err(BackendError::UnresolvedSource(SourceKey(10)))), "{res:?}");
    assert_eq!(image.calls, 0);
}
