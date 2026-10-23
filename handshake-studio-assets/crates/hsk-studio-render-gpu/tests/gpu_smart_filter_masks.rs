//! Smart-filter masks on the GPU backend (lane SMB-PS-ADJUST-STYLES-3; STU-RAS-037 per-filter
//! masking, STU-FX-004): per-entry masks and the stack mask of `LayerExtension::smart_filters`
//! (coverage_u8 inverted, coverage_u16le), and enabled masks over a placed-asset instance through
//! the composition path (composite resamples the document-space masks into the source surface).
//! Oracle: pigment's `SmartFilterStack::render` with the same coverage planes, composited by pigment
//! (what render-cpu produces bit for bit); tolerance 2e-5 (binary32 filters and mix; the mix weight
//! is pigment's binary64 product rounded once). GPU-execution proofs follow `tests/gpu-proof.toml`
//! (V4-03). Photoshop parity UNVERIFIED (D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{
    FilteredContent, FilteredMask, SceneComposition, SceneContent, SceneContext, SceneLayer, SceneLimits, ScenePixels,
    render_scene,
};
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::filter_math::LayerFilter;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BlendSpace, ColourTag, Exactness, ExecuteRequest, Group, Layer, LayerExtension,
    MaskFormat, MaskInput, NoVector, OutputTarget, PlanExtensions, PlanNode, PlanNodeId, PlanSources, RenderBackend,
    RenderPlan, SampleFormat, SinkError, SourceKey, StudioBlendMode, TileInput, TilePlanes, TileSink,
};
use hsk_studio_pigment::retouch::RgbaPlane;
use hsk_studio_pigment::selection::CoveragePlane;
use hsk_studio_pigment::smart_filter::{SmartFilterEntryOp, SmartFilterOp, SmartFilterStack, StackEntry, StackFilter, StackLimits};
use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_render_gpu::{DeviceOptions, GpuBackend, Reason};
use std::sync::OnceLock;

const TOLERANCE: f32 = 2e-5;
const EXTENT: Rect = Rect { x: 0, y: 0, width: 36, height: 30 };
const TILE: Rect = Rect { x: 5, y: 4, width: 26, height: 21 };
/// Entry mask (coverage_u8, inverted) over part of the tile and beyond it.
const MA: Rect = Rect { x: 9, y: 2, width: 20, height: 14 };
/// Stack mask (coverage_u16le) over the extent.
const MB: Rect = Rect { x: 0, y: 0, width: 36, height: 30 };
const BACKDROP: [f32; 4] = [0.9, 0.7, 0.5, 1.0];
const OPACITY: f32 = 0.8;

fn tag() -> ColourTag {
    ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::LinearLight }
}
fn tile_px(x: u32, y: u32) -> [f32; 4] {
    if (x / 4 + y / 3).is_multiple_of(2) {
        [0.9, 0.2 + 0.02 * (x % 5) as f32, 0.1, 1.0]
    } else {
        [0.1, 0.45, 0.85, if (x + y).is_multiple_of(7) { 0.0 } else { 0.6 }]
    }
}
fn ma(x: u32, y: u32) -> u8 {
    ((x * 37 + y * 11) % 256) as u8
}
fn mb(x: u32, y: u32) -> u16 {
    (x * 1700 + y * 900) as u16
}

struct Sources {
    colour: Vec<u8>,
    alpha: Vec<u8>,
    a: Vec<u8>,
    b: Vec<u8>,
}
impl Sources {
    fn new() -> Self {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in 0..TILE.height {
            for x in 0..TILE.width {
                let p = tile_px(x, y);
                for c in &p[..3] {
                    colour.extend_from_slice(&c.to_le_bytes());
                }
                alpha.extend_from_slice(&p[3].to_le_bytes());
            }
        }
        let a = (0..MA.height).flat_map(|y| (0..MA.width).map(move |x| ma(x, y))).collect();
        let b = (0..MB.height).flat_map(|y| (0..MB.width).flat_map(move |x| mb(x, y).to_le_bytes())).collect();
        Self { colour, alpha, a, b }
    }
    fn input(&self, key: u32) -> MaskInput {
        match key {
            3 => MaskInput { key: SourceKey(3), placement: MA, format: MaskFormat::CoverageU8, sha256: hsk_studio_prism::profile_hash(&self.a), inverted: true, density: 1.0 },
            _ => MaskInput { key: SourceKey(4), placement: MB, format: MaskFormat::CoverageU16Le, sha256: hsk_studio_prism::profile_hash(&self.b), inverted: false, density: 1.0 },
        }
    }
}
impl PlanSources for Sources {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        (key.0 == 1).then(|| TilePlanes {
            width: TILE.width,
            height: TILE.height,
            colour: &self.colour,
            colour_stride_bytes: u64::from(TILE.width) * 12,
            alpha: &self.alpha,
            alpha_stride_bytes: u64::from(TILE.width) * 4,
            colour_sha256: hsk_studio_prism::profile_hash(&self.colour),
            alpha_sha256: hsk_studio_prism::profile_hash(&self.alpha),
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: tag(),
        })
    }
    fn mask(&self, key: SourceKey) -> Option<Mask<'_>> {
        match key.0 {
            3 => Some(Mask { bounds: MA, sample_format: "coverage_u8", bytes: &self.a, stride_bytes: u64::from(MA.width), sha256: hsk_studio_prism::profile_hash(&self.a) }),
            4 => Some(Mask { bounds: MB, sample_format: "coverage_u16le", bytes: &self.b, stride_bytes: u64::from(MB.width) * 2, sha256: hsk_studio_prism::profile_hash(&self.b) }),
            _ => None,
        }
    }
}

struct Image {
    width: u32,
    px: Vec<[f32; 4]>,
    calls: usize,
}
impl TileSink for Image {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        for y in 0..r.height as usize {
            for x in 0..r.width as usize {
                let i = y * r.width as usize + x;
                let at = (r.y as usize + y) * self.width as usize + r.x as usize + x;
                self.px[at] = [f(colour, i * 12), f(colour, i * 12 + 4), f(colour, i * 12 + 8), f(alpha, i * 4)];
            }
        }
        self.calls += 1;
        Ok(())
    }
}

fn backend() -> &'static GpuBackend {
    static SHARED: OnceLock<GpuBackend> = OnceLock::new();
    SHARED.get_or_init(|| GpuBackend::new(DeviceOptions { chunk_side: 16, ..DeviceOptions::default() }))
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

fn entry(filter: LayerFilter, opacity: f32, mask: Option<MaskInput>) -> SmartFilterEntryOp {
    SmartFilterEntryOp { filter: StackFilter::Filter(filter), enabled: true, opacity, mask }
}
/// Blur with the inverted u8 entry mask, median at 0.7, unsharp at 0.5; the u16 stack mask.
fn op(s: &Sources) -> SmartFilterOp {
    SmartFilterOp {
        entries: vec![
            entry(LayerFilter::GaussianBlur { radius_px: 2.0 }, 1.0, Some(s.input(3))),
            entry(LayerFilter::Median { radius_px: 1 }, 0.7, None),
            entry(LayerFilter::UnsharpMask { amount_pct: 120.0, radius_px: 1.0, threshold_levels: 0 }, 0.5, None),
        ],
        stack_mask: Some(s.input(4)),
    }
}
fn plan(s: &Sources) -> RenderPlan<NoVector> {
    RenderPlan {
        revision: 5,
        target: OutputTarget { extent: EXTENT, format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, space: tag() },
        nodes: vec![
            PlanNode::Solid { rect: EXTENT, rgba: BACKDROP },
            PlanNode::Tile(TileInput {
                key: SourceKey(1),
                placement: TILE,
                format: SampleFormat::RgbF32Le,
                alpha: AlphaConvention::Straight,
                colour_sha256: hsk_studio_prism::profile_hash(&s.colour),
                alpha_sha256: hsk_studio_prism::profile_hash(&s.alpha),
            }),
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![
                    Layer { input: PlanNodeId(0), mode: StudioBlendMode::Normal, opacity: 1.0, fill_opacity: 1.0, mask: None },
                    Layer { input: PlanNodeId(1), mode: StudioBlendMode::Normal, opacity: OPACITY, fill_opacity: 1.0, mask: None },
                ],
            }),
        ],
        root: PlanNodeId(2),
    }
}

/// Pigment's stack of the tile plane with the masks as pigment coverage, composited by pigment.
fn reference(s: &Sources) -> Vec<[f32; 4]> {
    let data: Vec<[f32; 4]> = (0..TILE.height).flat_map(|y| (0..TILE.width).map(move |x| tile_px(x, y))).collect();
    let cov_a = CoveragePlane { bounds: MA, data: s.a.iter().map(|v| u16::MAX - u16::from(*v) * 257).collect() };
    let cov_b = CoveragePlane { bounds: MB, data: s.b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect() };
    let o = op(s);
    let stack = SmartFilterStack {
        entries: o
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| StackEntry { filter: e.filter.clone(), enabled: e.enabled, opacity: e.opacity, mask: (i == 0).then(|| cov_a.clone()) })
            .collect(),
        stack_mask: Some(cov_b),
    };
    let out = stack.render(&RgbaPlane::new(TILE, data).unwrap(), &StackLimits::default()).unwrap();
    let mut want = Vec::new();
    for y in 0..i64::from(EXTENT.height) {
        for x in 0..i64::from(EXTENT.width) {
            let inside = x >= TILE.x && y >= TILE.y && x < TILE.x + i64::from(TILE.width) && y < TILE.y + i64::from(TILE.height);
            let p = if inside { out.data[((y - TILE.y) * i64::from(TILE.width) + (x - TILE.x)) as usize] } else { [0.0; 4] };
            let b = BACKDROP;
            want.push(if p[3] <= 0.0 {
                b
            } else {
                let o = composite_over(StudioBlendMode::Normal, Rgba::new([b[0], b[1], b[2]], b[3]), Rgba::new([p[0], p[1], p[2]], p[3]), OPACITY, PixelSite { x, y, seed: 7 }).unwrap();
                [o.rgb[0], o.rgb[1], o.rgb[2], o.a.min(1.0)]
            });
        }
    }
    want
}

fn worst(got: &[[f32; 4]], want: &[[f32; 4]]) -> f32 {
    got.iter().zip(want).map(|(g, w)| (0..4).map(|k| (g[k] - w[k]).abs()).fold(0.0f32, f32::max)).fold(0.0, f32::max)
}

#[test]
fn gpu_masked_smart_filter_stack_matches_pigment_within_tolerance() {
    if !gate("gpu_masked_smart_filter_stack_matches_pigment_within_tolerance") {
        return;
    }
    let s = Sources::new();
    let e = PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(2), 1).with_smart_filters(op(&s))]);
    let request = ExecuteRequest { expected_revision: 5, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 30 };
    let run = |e: &PlanExtensions| {
        let mut image = Image { width: EXTENT.width, px: vec![[f32::NAN; 4]; (EXTENT.width * EXTENT.height) as usize], calls: 0 };
        let r = backend().execute_extended(&plan(&s), e, &request, &s, &CancellationToken::default(), &mut image).map(|_| ());
        (r, image)
    };
    let (r, image) = run(&e);
    r.unwrap();
    let err = worst(&image.px, &reference(&s));
    println!("gpu masked smart filter stack: max |gpu - pigment| = {err:e} (tolerance {TOLERANCE:e})");
    assert!(err <= TOLERANCE, "{err:e}");
    // the masks matter: the unmasked stack renders differently
    let mut plain = op(&s);
    plain.entries[0].mask = None;
    plain.stack_mask = None;
    let (r, unmasked) = run(&PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(2), 1).with_smart_filters(plain)]));
    r.unwrap();
    assert!(worst(&unmasked.px, &image.px) > 1e-3);
    // an unresolved mask source, a declared hash that differs, a density below 1: typed, before
    // any sink call
    let refuse = |o: SmartFilterOp, want: BackendError| {
        let (r, image) = run(&PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(2), 1).with_smart_filters(o)]));
        let label = format!("{want:?}");
        assert_eq!(r, Err(want), "{label}");
        assert_eq!(image.calls, 0);
    };
    let mut unresolved = op(&s);
    unresolved.entries[1].mask = Some(MaskInput { key: SourceKey(9), ..s.input(3) });
    refuse(unresolved, BackendError::UnresolvedSource(SourceKey(9)));
    let mut forged = op(&s);
    forged.stack_mask = Some(MaskInput { sha256: [5; 32], ..s.input(4) });
    refuse(forged, BackendError::SourceMismatch(SourceKey(4)));
}

// ---- placed instance through the composition path ------------------------------------------

/// Document masks `L/a` (coverage_u8 over (4, 1) 4 x 2) and `L/b` (coverage_u16le over the
/// document) as in render-cpu tests/cpu_placed_filter_masks.rs.
struct Pixels {
    a: Vec<u8>,
    b: Vec<u8>,
}
const PA: Rect = Rect { x: 4, y: 1, width: 4, height: 2 };
impl Pixels {
    fn new() -> Self {
        let a = vec![255, 128, 0, 64, 32, 255, 200, 10];
        let b = (0..4u32).flat_map(|y| (0..12u32).flat_map(move |x| ((x * 5000 + y * 700) as u16).to_le_bytes())).collect();
        Self { a, b }
    }
    fn a_at(&self, x: i64, y: i64) -> u16 {
        if x < PA.x || y < PA.y || x >= PA.x + 4 || y >= PA.y + 2 {
            return 0;
        }
        u16::from(self.a[((y - PA.y) * 4 + (x - PA.x)) as usize]) * 257
    }
    fn b_at(&self, x: i64, y: i64) -> u16 {
        if !(0..12).contains(&x) || !(0..4).contains(&y) {
            return 0;
        }
        let at = ((y * 12 + x) * 2) as usize;
        u16::from_le_bytes([self.b[at], self.b[at + 1]])
    }
}
impl ScenePixels for Pixels {
    fn tile(&self, _: &str) -> Option<TilePlanes<'_>> {
        None
    }
    fn mask(&self, id: &str) -> Option<Mask<'_>> {
        match id {
            "L/a" => Some(Mask { bounds: PA, sample_format: "coverage_u8", bytes: &self.a, stride_bytes: 4, sha256: hsk_studio_prism::profile_hash(&self.a) }),
            "L/b" => Some(Mask {
                bounds: Rect { x: 0, y: 0, width: 12, height: 4 },
                sample_format: "coverage_u16le",
                bytes: &self.b,
                stride_bytes: 24,
                sha256: hsk_studio_prism::profile_hash(&self.b),
            }),
            _ => None,
        }
    }
}
fn layer(id: &str, content: SceneContent, matrix: Affine2) -> SceneLayer {
    SceneLayer { layer_id: id.into(), content, mode: StudioBlendMode::Normal, opacity: 1.0, fill_opacity: 1.0, matrix, matte: None, clipped: false }
}
fn scene(id: &str, width: u32, height: u32, background: Option<[f32; 4]>, layers: Vec<SceneLayer>) -> SceneComposition {
    SceneComposition { composition_id: id.into(), width, height, background, filter: ResampleFilter::NearestNeighbour, layers }
}
fn source() -> SceneComposition {
    let red = scene("red", 2, 2, Some([0.9, 0.1, 0.1, 1.0]), vec![]);
    scene("src1", 6, 4, Some([0.2, 0.4, 0.6, 1.0]), vec![layer("square", SceneContent::Precomp(Box::new(red)), Affine2::translate(2.0, 1.0))])
}
const SRC: Rect = Rect { x: 0, y: 0, width: 6, height: 4 };

#[test]
fn gpu_masked_placed_instance_matches_pigment_stack() {
    if !gate("gpu_masked_placed_instance_matches_pigment_stack") {
        return;
    }
    let pixels = Pixels::new();
    let at = Affine2::translate(3.0, 0.0);
    let placeholder = |key: u32| MaskInput { key: SourceKey(key), placement: SRC, format: MaskFormat::CoverageU16Le, sha256: [0; 32], inverted: false, density: 1.0 };
    let o = SmartFilterOp {
        entries: vec![entry(LayerFilter::GaussianBlur { radius_px: 1.5 }, 1.0, Some(placeholder(1))), entry(LayerFilter::Median { radius_px: 1 }, 0.5, None)],
        stack_mask: Some(placeholder(2)),
    };
    let masks = vec![
        FilteredMask { key: SourceKey(1), mask_id: "L/a".into(), to_document: at },
        FilteredMask { key: SourceKey(2), mask_id: "L/b".into(), to_document: at },
    ];
    let filtered = SceneContent::Filtered(Box::new(FilteredContent { content: SceneContent::Precomp(Box::new(source())), rect: SRC, filters: o.clone(), masks }));
    let ctx = SceneContext {
        backend: backend(),
        pixels: &pixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 },
        space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded },
        limits: SceneLimits::default(),
    };
    let render = |s: &SceneComposition| -> Vec<[f32; 4]> {
        let out = render_scene(s, &ctx, &CancellationToken::default()).expect("renders");
        (0..s.height).flat_map(|y| (0..s.width).map(move |x| (x, y))).map(|(x, y)| out.pixel(x, y).unwrap()).collect()
    };
    let got = render(&scene("doc", 12, 4, None, vec![layer("SLYR-1", filtered, at)]));
    // oracle: the source alone (this backend), the masks resampled at (u + 3.5, v + 0.5), pigment's stack
    let resample = |f: &dyn Fn(i64, i64) -> u16| CoveragePlane { bounds: SRC, data: (0..4i64).flat_map(|v| (0..6i64).map(move |u| (u, v))).map(|(u, v)| f(u + 3, v)).collect() };
    let (ca, cb) = (resample(&|x, y| pixels.a_at(x, y)), resample(&|x, y| pixels.b_at(x, y)));
    let stack = SmartFilterStack {
        entries: o.entries.iter().enumerate().map(|(i, e)| StackEntry { filter: e.filter.clone(), enabled: true, opacity: e.opacity, mask: (i == 0).then(|| ca.clone()) }).collect(),
        stack_mask: Some(cb),
    };
    let out = stack.render(&RgbaPlane::new(SRC, render(&source())).unwrap(), &StackLimits::default()).unwrap();
    let over = |p: [f32; 4], x: i64, y: i64| -> [f32; 4] {
        if p[3] <= 0.0 {
            return [0.0; 4];
        }
        let o = composite_over(StudioBlendMode::Normal, Rgba::TRANSPARENT, Rgba::new([p[0], p[1], p[2]], p[3]), 1.0, PixelSite { x, y, seed: 7 }).unwrap();
        [o.rgb[0], o.rgb[1], o.rgb[2], o.a]
    };
    let want: Vec<[f32; 4]> = (0..4i64)
        .flat_map(|y| (0..12i64).map(move |x| (x, y)))
        .map(|(x, y)| if (3..9).contains(&x) { over(over(out.data[(y * 6 + x - 3) as usize], x - 3, y), x, y) } else { [0.0; 4] })
        .collect();
    let err = worst(&got, &want);
    println!("gpu masked placed instance: max |gpu - pigment stack| = {err:e} (tolerance {TOLERANCE:e})");
    assert!(err <= TOLERANCE, "{err:e}");
}
