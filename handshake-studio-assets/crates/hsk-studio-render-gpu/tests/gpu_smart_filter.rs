//! Smart-filter stacks on the GPU backend (lanes SMB-PS-SELECT-LIQUIFY-2, SMB-PS-SMART-FILTER-
//! WIRING). Filter-only stacks without masks run as the existing filter passes (`filter.wgsl`,
//! `blur.wgsl`) plus a premultiplied mix pass per entry, compared with pigment's
//! `SmartFilterStack::render` composited by pigment's `composite_over` (what render-cpu produces
//! bit for bit) within the filter tolerance 2e-5 (binary32; the noise differs from the reference
//! by at most 2^-24 of its amplitude). GPU-execution proofs follow `tests/gpu-proof.toml` (V4-03):
//! without an adapter they FAIL with `NOT_PROVEN: no adapter` while `require_gpu = true`. Liquify
//! entries and masks are typed refusals at prepare, before any pass or sink call.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::filter_math::{LayerFilter, NoiseDistribution};
use hsk_studio_pigment::liquify::{LiquifyLimits, LiquifyMesh};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BlendSpace, ColourTag, Exactness, ExecuteRequest, ExtOperator, Group, Layer,
    LayerExtension, MaskFormat, MaskInput, NoVector, OutputTarget, PlanExtensions, PlanNode, PlanNodeId, PlanSources,
    RenderBackend, RenderPlan, SampleFormat, SinkError, SourceKey, StudioBlendMode, TileInput, TilePlanes, TileSink,
};
use hsk_studio_pigment::retouch::RgbaPlane;
use hsk_studio_pigment::smart_filter::{SmartFilterEntryOp, SmartFilterOp, SmartFilterStack, StackEntry, StackFilter, StackLimits};
use hsk_studio_pigment::transform_math::ResampleFilter;
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_render_gpu::{DeviceOptions, GpuBackend, Reason};
use std::sync::OnceLock;

const TOLERANCE: f32 = 2e-5;
const EXTENT: Rect = Rect { x: 0, y: 0, width: 40, height: 33 };
const TILE: Rect = Rect { x: 6, y: 5, width: 28, height: 22 };
const BACKDROP: [f32; 4] = [0.9, 0.7, 0.5, 1.0];
const OPACITY: f32 = 0.85;

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

struct Tile {
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl Tile {
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
        Self { colour, alpha }
    }
}
impl PlanSources for Tile {
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
    fn mask(&self, _key: SourceKey) -> Option<Mask<'_>> {
        None
    }
}

struct Image {
    px: Vec<[f32; 4]>,
    calls: usize,
}
impl TileSink for Image {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        for y in 0..r.height as usize {
            for x in 0..r.width as usize {
                let i = y * r.width as usize + x;
                let at = (r.y as usize + y) * EXTENT.width as usize + r.x as usize + x;
                self.px[at] = [f(colour, i * 12), f(colour, i * 12 + 4), f(colour, i * 12 + 8), f(alpha, i * 4)];
            }
        }
        self.calls += 1;
        Ok(())
    }
}

fn plan(tile: &Tile) -> RenderPlan<NoVector> {
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
                colour_sha256: hsk_studio_prism::profile_hash(&tile.colour),
                alpha_sha256: hsk_studio_prism::profile_hash(&tile.alpha),
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

fn entry(filter: LayerFilter, enabled: bool, opacity: f32) -> SmartFilterEntryOp {
    SmartFilterEntryOp { filter: StackFilter::Filter(filter), enabled, opacity, mask: None }
}

/// Blur -> noise at 0.6 -> disabled high pass -> median -> unsharp at 0.35.
fn op() -> SmartFilterOp {
    SmartFilterOp {
        entries: vec![
            entry(LayerFilter::GaussianBlur { radius_px: 2.5 }, true, 1.0),
            entry(LayerFilter::AddNoise { amount_pct: 12.0, distribution: NoiseDistribution::Gaussian, monochromatic: false, seed: 9 }, true, 0.6),
            entry(LayerFilter::HighPass { radius_px: 3.0 }, false, 1.0),
            entry(LayerFilter::Median { radius_px: 1 }, true, 1.0),
            entry(LayerFilter::UnsharpMask { amount_pct: 150.0, radius_px: 1.5, threshold_levels: 2 }, true, 0.35),
        ],
        stack_mask: None,
    }
}

fn ext(op: SmartFilterOp) -> PlanExtensions {
    PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(2), 1).with_smart_filters(op)])
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

fn execute(p: &RenderPlan<NoVector>, e: &PlanExtensions, tile: &Tile) -> (Result<(), BackendError>, Image) {
    let mut image = Image { px: vec![[f32::NAN; 4]; (EXTENT.width * EXTENT.height) as usize], calls: 0 };
    let request = ExecuteRequest { expected_revision: 5, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 30 };
    let r = backend().execute_extended(p, e, &request, tile, &CancellationToken::default(), &mut image).map(|_| ());
    (r, image)
}

/// Pigment's stack of the tile plane, composited by pigment.
fn reference() -> Vec<[f32; 4]> {
    let data: Vec<[f32; 4]> = (0..TILE.height).flat_map(|y| (0..TILE.width).map(move |x| tile_px(x, y))).collect();
    let o = op();
    let stack = SmartFilterStack {
        entries: o.entries.iter().map(|e| StackEntry { filter: e.filter.clone(), enabled: e.enabled, opacity: e.opacity, mask: None }).collect(),
        stack_mask: None,
    };
    let out = stack.render(&RgbaPlane::new(TILE, data).unwrap(), &StackLimits::default()).unwrap();
    let mut want = Vec::new();
    for y in 0..i64::from(EXTENT.height) {
        for x in 0..i64::from(EXTENT.width) {
            let inside = x >= TILE.x && y >= TILE.y && x < TILE.x + i64::from(TILE.width) && y < TILE.y + i64::from(TILE.height);
            let s = if inside { out.data[((y - TILE.y) * i64::from(TILE.width) + (x - TILE.x)) as usize] } else { [0.0; 4] };
            let b = BACKDROP;
            want.push(if s[3] <= 0.0 {
                b
            } else {
                let o = composite_over(StudioBlendMode::Normal, Rgba::new([b[0], b[1], b[2]], b[3]), Rgba::new([s[0], s[1], s[2]], s[3]), OPACITY, PixelSite { x, y, seed: 7 }).unwrap();
                [o.rgb[0], o.rgb[1], o.rgb[2], o.a.min(1.0)]
            });
        }
    }
    want
}

#[test]
fn gpu_smart_filter_stack_matches_pigment_within_tolerance() {
    if !gate("gpu_smart_filter_stack_matches_pigment_within_tolerance") {
        return;
    }
    let tile = Tile::new();
    assert_eq!(backend().extension_exactness(ExtOperator::SmartFilter), Exactness::Approximate);
    let (r, image) = execute(&plan(&tile), &ext(op()), &tile);
    r.unwrap();
    let want = reference();
    let mut worst = 0.0f32;
    for (g, w) in image.px.iter().zip(&want) {
        for k in 0..4 {
            worst = worst.max((g[k] - w[k]).abs());
        }
    }
    println!("gpu smart filter stack: max |gpu - pigment| = {worst:e} (tolerance {TOLERANCE:e})");
    assert!(worst <= TOLERANCE, "{worst}");
    let (r, plain) = execute(&plan(&tile), &PlanExtensions::default(), &tile);
    r.unwrap();
    assert!(plain.px.iter().zip(&image.px).any(|(a, b)| a != b), "the stack changed nothing");
}

#[test]
fn gpu_smart_filter_refusals_are_typed_before_any_sink_call() {
    if !gate("gpu_smart_filter_refusals_are_typed_before_any_sink_call") {
        return;
    }
    let tile = Tile::new();
    let p = plan(&tile);
    let refuse = |o: SmartFilterOp, operator: &'static str| {
        let (r, image) = execute(&p, &ext(o), &tile);
        assert_eq!(r, Err(BackendError::Unsupported { operator, exactness: Exactness::Unsupported }), "{operator}");
        assert_eq!(image.calls, 0);
    };
    let mut liq = op();
    liq.entries.push(SmartFilterEntryOp {
        filter: StackFilter::Liquify { mesh: LiquifyMesh::new(TILE, 4, &LiquifyLimits::default()).unwrap(), resample: ResampleFilter::Bilinear },
        enabled: true,
        opacity: 1.0,
        mask: None,
    });
    refuse(liq, "smart_filter_liquify");
    let mask = MaskInput { key: SourceKey(9), placement: TILE, format: MaskFormat::CoverageU8, sha256: [0; 32], inverted: false, density: 1.0 };
    let mut masked = op();
    masked.entries[0].mask = Some(mask);
    refuse(masked, "smart_filter_mask");
    let mut stack_masked = op();
    stack_masked.stack_mask = Some(mask);
    refuse(stack_masked, "smart_filter_mask");
    let mut median = op();
    median.entries[3] = entry(LayerFilter::Median { radius_px: 9 }, true, 1.0);
    refuse(median, "filter_median_radius");
}
