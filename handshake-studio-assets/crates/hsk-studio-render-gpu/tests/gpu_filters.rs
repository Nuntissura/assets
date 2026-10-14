//! Layer filters on the GPU backend (lane SMB-RENDER-FILTERS slice 2): the `filter.wgsl` and
//! `blur.wgsl` passes (binary32) against the chain of pigment's single owner
//! (`filter_math::filter_window`, binary64, applied stage by stage to the whole layer) composited
//! with `blend_math::composite_over`, which render-cpu reproduces bit for bit (render-cpu
//! tests/cpu_filters.rs). GPU-execution proofs follow `tests/gpu-proof.toml` (V4-03): without an
//! adapter they FAIL with `NOT_PROVEN: no adapter` while `require_gpu = true`. The admission test is
//! hardware-independent.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::blur_math::{BlurScope, LayerBlur};
use hsk_studio_pigment::filter_math::*;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BlendSpace, ColourTag, Exactness, ExecuteRequest, ExtOperator,
    Group, Layer, LayerExtension, MaskFormat, MaskInput, NoVector, OutputTarget, PlanError,
    PlanExtensions, PlanNode, PlanNodeId, PlanSources, RenderBackend, RenderPlan, RenderReceipt,
    SampleFormat, SinkError, SourceKey, StudioBlendMode, TileInput, TilePlanes, TileSink,
};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_render_gpu::{DeviceOptions, GpuBackend, Reason};
use std::sync::OnceLock;

/// Tolerance on alpha and premultiplied colour: binary32 against binary64 (blur sums, the 24-bit
/// noise uniform, unsharp amplification up to 5x); measured values are printed by the tests.
const TOLERANCE: f32 = 2e-5;
const NORMAL: StudioBlendMode = StudioBlendMode::Normal;
const WIDE: FilterLimits = FilterLimits { max_bytes: 1 << 30, max_work: 1 << 40 };
const W: u32 = 20;
const H: u32 = 16;

fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect { x, y, width, height }
}
fn tag() -> ColourTag {
    ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::LinearLight }
}
/// The render-cpu cpu_filters fixture tile (9x7 at (4, 3)).
fn tile_px(x: u32, y: u32) -> [f32; 4] {
    let hole = (x * 3 + y * 5).is_multiple_of(7);
    [
        (x as f32 / 8.0).min(1.0),
        (y as f32 / 6.0).min(1.0),
        ((x + y) % 3) as f32 / 2.0,
        if hole { 0.0 } else { ((x + y) % 4 + 1) as f32 / 4.0 },
    ]
}
fn mask_byte(x: i64, y: i64) -> u8 {
    ((x * 17 + y * 29) % 256) as u8
}
struct Tile {
    colour: Vec<u8>,
    alpha: Vec<u8>,
    mask: Vec<u8>,
}
impl Tile {
    fn new() -> Self {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in 0..7 {
            for x in 0..9 {
                let p = tile_px(x, y);
                for c in &p[..3] {
                    colour.extend_from_slice(&c.to_le_bytes());
                }
                alpha.extend_from_slice(&p[3].to_le_bytes());
            }
        }
        let mut mask = Vec::new();
        for y in 0..H as i64 {
            for x in 0..W as i64 {
                mask.push(mask_byte(x, y));
            }
        }
        Self { colour, alpha, mask }
    }
}
impl PlanSources for Tile {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        (key.0 == 1).then(|| TilePlanes {
            width: 9,
            height: 7,
            colour: &self.colour,
            colour_stride_bytes: 9 * 12,
            alpha: &self.alpha,
            alpha_stride_bytes: 9 * 4,
            colour_sha256: hsk_studio_prism::profile_hash(&self.colour),
            alpha_sha256: hsk_studio_prism::profile_hash(&self.alpha),
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: tag(),
        })
    }
    fn mask(&self, key: SourceKey) -> Option<Mask<'_>> {
        (key.0 == 9).then(|| Mask {
            bounds: rect(0, 0, W, H),
            sample_format: MaskFormat::CoverageU8.key(),
            bytes: &self.mask,
            stride_bytes: u64::from(W),
            sha256: hsk_studio_prism::profile_hash(&self.mask),
        })
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
                let at = (r.y as usize + y) * W as usize + r.x as usize + x;
                self.px[at] = [f(colour, i * 12), f(colour, i * 12 + 4), f(colour, i * 12 + 8), f(alpha, i * 4)];
            }
        }
        self.calls += 1;
        Ok(())
    }
}
fn layer(input: u32, mode: StudioBlendMode, opacity: f32) -> Layer {
    Layer { input: PlanNodeId(input), mode, opacity, fill_opacity: 1.0, mask: None }
}
/// The render-cpu cpu_filters plan: root [backdrop, tile (multiply 0.9, optional mask), glass].
fn filter_plan(tile: &Tile, masked: bool) -> RenderPlan<NoVector> {
    let extent = rect(0, 0, W, H);
    let mut tile_layer = layer(1, StudioBlendMode::Multiply, 0.9);
    if masked {
        tile_layer.mask = Some(MaskInput {
            key: SourceKey(9),
            placement: extent,
            format: MaskFormat::CoverageU8,
            sha256: hsk_studio_prism::profile_hash(&tile.mask),
            inverted: false,
            density: 1.0,
        });
    }
    RenderPlan {
        revision: 7,
        target: OutputTarget { extent, format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, space: tag() },
        nodes: vec![
            PlanNode::Solid { rect: extent, rgba: [0.2, 0.5, 0.8, 1.0] },
            PlanNode::Tile(TileInput {
                key: SourceKey(1),
                placement: rect(4, 3, 9, 7),
                format: SampleFormat::RgbF32Le,
                alpha: AlphaConvention::Straight,
                colour_sha256: hsk_studio_prism::profile_hash(&tile.colour),
                alpha_sha256: hsk_studio_prism::profile_hash(&tile.alpha),
            }),
            PlanNode::Solid { rect: rect(6, 5, 8, 6), rgba: [1.0, 1.0, 1.0, 0.5] },
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![layer(0, NORMAL, 1.0), tile_layer, layer(2, NORMAL, 0.8)],
            }),
        ],
        root: PlanNodeId(3),
    }
}

fn over(mode: StudioBlendMode, b: [f32; 4], s: [f32; 4], opacity: f32, x: i64, y: i64) -> [f32; 4] {
    if opacity <= 0.0 || s[3] <= 0.0 {
        return b;
    }
    let out = composite_over(
        mode,
        Rgba::new([b[0], b[1], b[2]], b[3]),
        Rgba::new([s[0], s[1], s[2]], s[3]),
        opacity,
        PixelSite { x, y, seed: 7 },
    )
    .unwrap();
    [out.rgb[0], out.rgb[1], out.rgb[2], out.a.min(1.0)]
}

/// The filter chain of the whole tile, stage by stage, over a window far larger than the extent.
fn chain(filters: &[LayerFilter]) -> (Vec<[f32; 4]>, Rect) {
    let big = rect(-40, -40, 100, 96);
    let mut data = Vec::new();
    for y in 0..7 {
        for x in 0..9 {
            data.push(tile_px(x, y));
        }
    }
    let mut at = rect(4, 3, 9, 7);
    for f in filters {
        data = filter_window(&data, at, big, f, WIDE).unwrap();
        at = big;
    }
    (data, at)
}

/// Pigment's composition (what render-cpu renders bit for bit); `masked` applies `mask_byte`.
fn expected(filters: &[LayerFilter], masked: bool) -> Vec<[f32; 4]> {
    let (data, at) = chain(filters);
    let mut want = Vec::new();
    for y in 0..H as i64 {
        for x in 0..W as i64 {
            let b = over(NORMAL, [0.0; 4], [0.2, 0.5, 0.8, 1.0], 1.0, x, y);
            let inside = x >= at.x && y >= at.y && x < at.x + at.width as i64 && y < at.y + at.height as i64;
            let s = if inside { data[((y - at.y) * at.width as i64 + (x - at.x)) as usize] } else { [0.0; 4] };
            let cov = if masked { (f64::from(mask_byte(x, y)) / 255.0) as f32 } else { 1.0 };
            let b = over(StudioBlendMode::Multiply, b, s, 0.9 * cov, x, y);
            let glass = if (6..14).contains(&x) && (5..11).contains(&y) { [1.0, 1.0, 1.0, 0.5] } else { [0.0; 4] };
            want.push(over(NORMAL, b, glass, 0.8, x, y));
        }
    }
    want
}

fn backend() -> &'static GpuBackend {
    static SHARED: OnceLock<GpuBackend> = OnceLock::new();
    SHARED.get_or_init(|| GpuBackend::new(DeviceOptions { chunk_side: 8, ..DeviceOptions::default() }))
}
fn backend_wide() -> &'static GpuBackend {
    static SHARED: OnceLock<GpuBackend> = OnceLock::new();
    SHARED.get_or_init(|| GpuBackend::new(DeviceOptions { chunk_side: 64, ..DeviceOptions::default() }))
}
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
        Some(Reason::NoAdapter) if strict => {
            panic!("NOT_PROVEN: no adapter; {what} requires a GPU (tests/gpu-proof.toml)")
        }
        Some(Reason::NoAdapter) => {
            eprintln!("NOT_PROVEN: no adapter; {what} not run");
            false
        }
        other => panic!("{what}: no live device ({other:?})"),
    }
}
fn request(min: Exactness) -> ExecuteRequest {
    ExecuteRequest { expected_revision: 7, min_exactness: min, deadline: None, max_working_bytes: 1 << 30 }
}
fn execute_on(
    b: &GpuBackend,
    plan: &RenderPlan<NoVector>,
    ext: &PlanExtensions,
    tile: &Tile,
    min: Exactness,
) -> (Result<RenderReceipt, BackendError>, Image) {
    let mut image = Image { px: vec![[f32::NAN; 4]; (W * H) as usize], calls: 0 };
    let result = b.execute_extended(plan, ext, &request(min), tile, &CancellationToken::default(), &mut image);
    (result, image)
}
fn execute(plan: &RenderPlan<NoVector>, ext: &PlanExtensions, tile: &Tile, min: Exactness) -> (Result<RenderReceipt, BackendError>, Image) {
    execute_on(backend(), plan, ext, tile, min)
}
fn entry(filters: Vec<LayerFilter>) -> LayerExtension {
    LayerExtension::new(PlanNodeId(3), 1).with_filters(filters)
}
/// Largest |delta| of alpha and of premultiplied colour.
fn deviation(got: &[[f32; 4]], want: &[[f32; 4]]) -> f32 {
    let pre = |p: [f32; 4]| [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3]];
    got.iter()
        .zip(want)
        .flat_map(|(g, w)| {
            let (g, w) = (pre(*g), pre(*w));
            (0..4).map(move |c| (g[c] - w[c]).abs())
        })
        .fold(0.0, f32::max)
}

/// Every kind (the GPU median radius is at most 7) and a chain.
fn each_kind() -> Vec<(&'static str, Vec<LayerFilter>)> {
    vec![
        ("gaussian", vec![LayerFilter::GaussianBlur { radius_px: 2.5 }]),
        ("unsharp", vec![LayerFilter::UnsharpMask { amount_pct: 150.0, radius_px: 1.5, threshold_levels: 3 }]),
        ("high_pass", vec![LayerFilter::HighPass { radius_px: 2.0 }]),
        ("median_r2", vec![LayerFilter::Median { radius_px: 2 }]),
        ("median_r7", vec![LayerFilter::Median { radius_px: 7 }]),
        (
            "noise_uniform",
            vec![LayerFilter::AddNoise { amount_pct: 25.0, distribution: NoiseDistribution::Uniform, monochromatic: false, seed: 77 }],
        ),
        (
            "noise_gaussian_mono",
            vec![LayerFilter::AddNoise { amount_pct: 40.0, distribution: NoiseDistribution::Gaussian, monochromatic: true, seed: 9 }],
        ),
        (
            "noise_big_seed",
            vec![LayerFilter::AddNoise { amount_pct: 400.0, distribution: NoiseDistribution::Gaussian, monochromatic: false, seed: 0xDEAD_BEEF_F00D_CAFE }],
        ),
        (
            "chain",
            vec![
                LayerFilter::Median { radius_px: 1 },
                LayerFilter::GaussianBlur { radius_px: 1.5 },
                LayerFilter::UnsharpMask { amount_pct: 200.0, radius_px: 1.0, threshold_levels: 0 },
                LayerFilter::AddNoise { amount_pct: 10.0, distribution: NoiseDistribution::Gaussian, monochromatic: false, seed: 5 },
            ],
        ),
    ]
}

#[test]
fn gpu_filters_match_pigment_within_tolerance() {
    if !gate("gpu filter parity") {
        return;
    }
    let tile = Tile::new();
    let mut worst = 0.0_f32;
    for masked in [false, true] {
        let plan = filter_plan(&tile, masked);
        for (label, filters) in each_kind() {
            let ext = PlanExtensions::from_layers(vec![entry(filters.clone())]);
            let want = expected(&filters, masked);
            // Two chunkings: the frame margin makes the result independent of the chunk side.
            for (name, b) in [("chunk 8", backend()), ("chunk 64", backend_wide())] {
                let (result, image) = execute_on(b, &plan, &ext, &tile, Exactness::Approximate);
                let receipt = result.unwrap_or_else(|e| panic!("{label} masked {masked} {name}: {e:?}"));
                assert_eq!(receipt.exactness.lowest, Exactness::Approximate, "{label}");
                let dev = deviation(&image.px, &want);
                println!("{label} masked {masked} {name}: max |delta| alpha/premultiplied colour = {dev:e} (tolerance {TOLERANCE:e})");
                assert!(dev <= TOLERANCE, "{label} masked {masked} {name}: deviation {dev:e}");
                worst = worst.max(dev);
            }
            // The filter really acts: the unfiltered plan differs well beyond the tolerance.
            let (_, plain) = execute(&plan, &PlanExtensions::default(), &tile, Exactness::Approximate);
            assert!(deviation(&plain.px, &want) > 1e-3, "{label}: filter had no effect");
        }
    }
    println!("worst max |delta| over all cases = {worst:e}");
}

#[test]
fn gpu_noise_follows_the_seed() {
    if !gate("gpu noise seed") {
        return;
    }
    let tile = Tile::new();
    let plan = filter_plan(&tile, false);
    let noise = |seed: u64| LayerFilter::AddNoise { amount_pct: 60.0, distribution: NoiseDistribution::Uniform, monochromatic: false, seed };
    let run = |seed: u64| execute(&plan, &PlanExtensions::from_layers(vec![entry(vec![noise(seed)])]), &tile, Exactness::Approximate);
    let (a, one) = run(1);
    let (b, two) = run(2);
    let (c, again) = run(1);
    a.unwrap();
    b.unwrap();
    c.unwrap();
    // Same seed, same bytes; another seed, another pattern (the hash is the reference's, so the
    // pattern is the reference's: tests above).
    assert_eq!(one.px.iter().map(|p| p.map(f32::to_bits)).collect::<Vec<_>>(), again.px.iter().map(|p| p.map(f32::to_bits)).collect::<Vec<_>>());
    assert!(deviation(&one.px, &two.px) > 1e-3);
}

#[test]
fn gpu_filter_admission_and_refusals_are_typed() {
    let tile = Tile::new();
    let plan = filter_plan(&tile, false);
    // Declared with pigment's class for every kind.
    for kind in [FilterKind::GaussianBlur, FilterKind::UnsharpMask, FilterKind::HighPass, FilterKind::Median, FilterKind::AddNoise] {
        assert_eq!(backend().extension_exactness(ExtOperator::Filter(kind)), Exactness::Approximate, "{kind:?}");
    }
    // Below the class Approximate each kind is refused under its key before any device work.
    for (key, f) in [
        ("filter_gaussian_blur", LayerFilter::GaussianBlur { radius_px: 2.0 }),
        ("filter_unsharp_mask", LayerFilter::UnsharpMask { amount_pct: 100.0, radius_px: 1.0, threshold_levels: 0 }),
        ("filter_high_pass", LayerFilter::HighPass { radius_px: 1.0 }),
        ("filter_median", LayerFilter::Median { radius_px: 1 }),
        ("filter_add_noise", LayerFilter::AddNoise { amount_pct: 5.0, distribution: NoiseDistribution::Uniform, monochromatic: false, seed: 0 }),
    ] {
        let ext = PlanExtensions::from_layers(vec![entry(vec![f])]);
        let (result, image) = execute(&plan, &ext, &tile, Exactness::Fitted);
        assert!(matches!(result, Err(BackendError::Unsupported { operator, .. }) if operator == key), "{key}: {result:?}");
        assert_eq!(image.calls, 0);
    }
    // Out-of-range parameters: pigment's admission, no sink call.
    for bad in [
        LayerFilter::Median { radius_px: 0 },
        LayerFilter::Median { radius_px: 501 },
        LayerFilter::UnsharpMask { amount_pct: f32::NAN, radius_px: 1.0, threshold_levels: 0 },
        LayerFilter::HighPass { radius_px: -1.0 },
        LayerFilter::AddNoise { amount_pct: 401.0, distribution: NoiseDistribution::Uniform, monochromatic: false, seed: 0 },
    ] {
        let ext = PlanExtensions::from_layers(vec![entry(vec![bad])]);
        let (result, image) = execute(&plan, &ext, &tile, Exactness::Approximate);
        assert_eq!(result.unwrap_err(), BackendError::InvalidPlan(PlanError::FilterParameter { node: 3 }), "{bad:?}");
        assert_eq!(image.calls, 0);
    }
    // Filters with a blur on one layer: pigment's combination rule.
    let both = PlanExtensions::from_layers(vec![entry(vec![LayerFilter::HighPass { radius_px: 1.0 }]).with_blur(LayerBlur::new(BlurScope::Layer, 2.0))]);
    let (result, image) = execute(&plan, &both, &tile, Exactness::Approximate);
    assert_eq!(result.unwrap_err(), BackendError::InvalidPlan(PlanError::FilterCombination { node: 3 }));
    assert_eq!(image.calls, 0);
    // The GPU median is bounded (radius 7): a larger admitted radius is the typed refusal, decided
    // before any device work (without an adapter the device state is checked first, so only the
    // typed refusal or the no-adapter state may be returned and never a sink call).
    let wide = PlanExtensions::from_layers(vec![entry(vec![LayerFilter::Median { radius_px: 8 }])]);
    let (result, image) = execute(&plan, &wide, &tile, Exactness::Approximate);
    assert_eq!(image.calls, 0);
    match result {
        Err(BackendError::Unsupported { operator: "filter_median_radius", .. }) => {}
        Err(BackendError::Unavailable(_)) if backend().device_info().is_none() => {}
        other => panic!("{other:?}"),
    }
}
