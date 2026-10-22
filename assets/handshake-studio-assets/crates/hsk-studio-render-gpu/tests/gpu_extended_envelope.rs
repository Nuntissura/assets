//! Envelope operations of an extended-range plan on render-gpu (lane SMB-RENDER-GPU-EXTENDED-
//! ENVELOPE): linear exposure, gaussian blur, unsharp mask, median, layer blur, backdrop blur,
//! transform and a drop shadow against pigment's unclamped `*_in(SampleRange::Extended, ..)`
//! functions (the maths render-cpu runs bit for bit, render-cpu tests/cpu_extended_envelope.rs),
//! within TOLERANCE; overflow on the shared-texture path is the typed refusal render-cpu returns.
//! GPU-execution proofs follow `tests/gpu-proof.toml` (V4-03): no adapter FAILS with
//! `NOT_PROVEN: no adapter` while `require_gpu = true`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::adjust_math::{AdjustmentOp, Exposure, composite_adjustment_in};
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over_extended};
use hsk_studio_pigment::blur_math::{BlurLimits, BlurScope, LayerBlur, backdrop_mix_in, blur_window_in};
use hsk_studio_pigment::effect_math::{DropShadowOp, EffectOp};
use hsk_studio_pigment::filter_math::{FilterLimits, LayerFilter, filter_window_in};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BlendSpace, ColourTag, Exactness, ExecuteRequest, Group, Layer,
    LayerExtension, NoVector, OutputStage, OutputTarget, PlanExtensions, PlanNode, PlanNodeId,
    PlanSources, RenderPlan, SAMPLE_RANGE_OPERATOR, SampleFormat, SampleRange, SinkError,
    SourceKey, StudioBlendMode, TileInput, TilePlanes, TileSink,
};
use hsk_studio_pigment::transform_math::{Affine2, LayerTransform, PreparedTransform, ResampleFilter};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_prism::profile_hash;
use hsk_studio_render_gpu::{DeviceOptions, GpuBackend, GpuError, Reason};

/// Bound on |GPU - pigment| / max(1, |pigment|) per channel: binary32 WGSL against pigment's
/// binary64 blur and filter sums and Rust `powf` (GPU `pow`); measured values are printed.
const TOLERANCE: f32 = 2e-5;
const W: u32 = 12;
const H: u32 = 8;
const EXTENT: Rect = Rect { x: 0, y: 0, width: W, height: H };
const WIDE_F: FilterLimits = FilterLimits { max_bytes: 1 << 30, max_work: 1 << 40 };
const WIDE_B: BlurLimits = BlurLimits { max_bytes: 1 << 30, max_work: 1 << 40 };

fn scene(x: u32, y: u32) -> [f32; 4] {
    let (x, y) = (x as f32, y as f32);
    [0.5 + 0.5 * x, 3.0 - 0.25 * y, 0.25 + 0.1 * x, 1.0]
}
fn tag() -> ColourTag {
    ColourTag { profile_sha256: profile_hash(b"working-profile"), blend_space: BlendSpace::LinearLight }
}
struct Tile {
    w: u32,
    h: u32,
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl Tile {
    fn new(w: u32, h: u32, f: impl Fn(u32, u32) -> [f32; 4]) -> Self {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in 0..h {
            for x in 0..w {
                let p = f(x, y);
                for v in &p[..3] {
                    colour.extend_from_slice(&v.to_le_bytes());
                }
                alpha.extend_from_slice(&p[3].to_le_bytes());
            }
        }
        Self { w, h, colour, alpha }
    }
    fn planes(&self) -> TilePlanes<'_> {
        TilePlanes {
            width: self.w,
            height: self.h,
            colour: &self.colour,
            colour_stride_bytes: u64::from(self.w) * 12,
            alpha: &self.alpha,
            alpha_stride_bytes: u64::from(self.w) * 4,
            colour_sha256: profile_hash(&self.colour),
            alpha_sha256: profile_hash(&self.alpha),
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: tag(),
        }
    }
    fn input(&self, key: u32, x: i64, y: i64) -> TileInput {
        TileInput {
            key: SourceKey(key),
            placement: Rect { x, y, width: self.w, height: self.h },
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: profile_hash(&self.colour),
            alpha_sha256: profile_hash(&self.alpha),
        }
    }
    fn at(&self, x: u32, y: u32) -> [f32; 4] {
        let c = ((y * self.w + x) * 12) as usize;
        let a = ((y * self.w + x) * 4) as usize;
        let f = |b: &[u8], i: usize| f32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        [f(&self.colour, c), f(&self.colour, c + 4), f(&self.colour, c + 8), f(&self.alpha, a)]
    }
}
struct Sources<'a>(Vec<(u32, TilePlanes<'a>)>);
impl PlanSources for Sources<'_> {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        self.0.iter().find(|(k, _)| *k == key.0).map(|(_, t)| *t)
    }
    fn mask(&self, _: SourceKey) -> Option<Mask<'_>> {
        None
    }
}
struct Grid {
    px: Vec<[f32; 4]>,
}
impl TileSink for Grid {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], i: usize| f32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        for row in 0..r.height as usize {
            for col in 0..r.width as usize {
                let s = row * r.width as usize + col;
                let i = (r.y as usize + row) * W as usize + r.x as usize + col;
                self.px[i] = [f(colour, s * 12), f(colour, s * 12 + 4), f(colour, s * 12 + 8), f(alpha, s * 4)];
            }
        }
        Ok(())
    }
}
fn layer(input: u32, mode: StudioBlendMode) -> Layer {
    Layer { input: PlanNodeId(input), mode, opacity: 1.0, fill_opacity: 1.0, mask: None }
}
fn plan(nodes: Vec<PlanNode<NoVector>>) -> RenderPlan<NoVector> {
    let root = PlanNodeId(nodes.len() as u32 - 1);
    RenderPlan {
        revision: 2,
        target: OutputTarget { extent: EXTENT, format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, space: tag() },
        nodes,
        root,
    }
}
fn group(layers: Vec<Layer>) -> PlanNode<NoVector> {
    PlanNode::Group(Group { isolated: true, layers })
}
fn ext(entry: LayerExtension) -> PlanExtensions {
    PlanExtensions::from_layers(vec![entry]).with_output(OutputStage::new(SampleRange::Extended, None))
}
fn request() -> ExecuteRequest {
    ExecuteRequest { expected_revision: 2, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 30 }
}
fn over(b: [f32; 4], s: [f32; 4], x: i64, y: i64) -> [f32; 4] {
    let o = composite_over_extended(StudioBlendMode::Normal, Rgba::new([b[0], b[1], b[2]], b[3]), Rgba::new([s[0], s[1], s[2]], s[3]), 1.0, PixelSite { x, y, seed: 0 }).unwrap();
    [o.rgb[0], o.rgb[1], o.rgb[2], o.a]
}
/// One parity case: name, plan, envelope, pigment reference per pixel.
type Case = (&'static str, RenderPlan<NoVector>, PlanExtensions, Box<dyn Fn(i64, i64) -> [f32; 4]>);
fn strict() -> bool {
    include_str!("gpu-proof.toml").lines().any(|l| l.trim() == "require_gpu = true")
        || std::env::var("HSK_STUDIO_TEST_REQUIRE_GPU").as_deref() == Ok("1")
}
/// The live backend, or `None` after printing NOT_PROVEN when the policy allows it.
fn live(chunk_side: u32, what: &str) -> Option<GpuBackend> {
    let gpu = GpuBackend::new(DeviceOptions { chunk_side, ..DeviceOptions::default() });
    if gpu.device_info().is_some() {
        return Some(gpu);
    }
    if let Some(failure) = gpu.failure() {
        panic!("adapter present but the backend failed: {failure:?}");
    }
    match gpu.unavailable_reason() {
        Some(Reason::NoAdapter) if strict() => panic!("NOT_PROVEN: no adapter; {what} requires a GPU (tests/gpu-proof.toml)"),
        Some(Reason::NoAdapter) => {
            eprintln!("NOT_PROVEN: no adapter; {what} not run");
            None
        }
        other => panic!("no live device ({other:?})"),
    }
}

/// Each operation on a live adapter at chunk sides 8 and 64, against pigment's extended maths:
/// exposure (+1 stop, offset 0.1, gamma 2) = composite_adjustment_in; gaussian blur r 2, unsharp
/// mask 150 % r 1.5, median r 1 = filter_window_in over the whole layer; layer blur r 2 =
/// blur_window_in; backdrop blur r 2 under a 0.6-alpha block = backdrop_mix_in of blur_window_in
/// then the block composited; bicubic 1.5x transform = sample_rgba_in; all composited by
/// composite_over_extended. Drop shadow (multiply 0.5, 2 px) on a 4.0 block over 3.0: block 4.0,
/// shadow-only pixels 1.5 (hand values). Values above 1 are present in every reference.
#[test]
fn gpu_runs_every_extended_envelope_operation_within_tolerance_of_pigment() {
    let s = Tile::new(W, H, scene);
    let whole: Vec<[f32; 4]> = (0..H).flat_map(|y| (0..W).map(move |x| scene(x, y))).collect();

    let single = |e: LayerExtension| (plan(vec![PlanNode::Tile(s.input(1, 0, 0)), group(vec![layer(0, StudioBlendMode::Normal)])]), ext(e));
    let at = |v: &[[f32; 4]], x: i64, y: i64| v[(y as usize) * W as usize + x as usize];

    let mut cases: Vec<Case> = Vec::new();
    // Exposure adjustment layer.
    let exposure = AdjustmentOp::Exposure(Exposure { exposure: 1.0, offset: 0.1, gamma: 2.0, linearize_gamma: 1.0 });
    let prepared = exposure.prepare().unwrap();
    cases.push((
        "exposure",
        plan(vec![PlanNode::Tile(s.input(1, 0, 0)), PlanNode::Solid { rect: EXTENT, rgba: [0.0; 4] }, group(vec![layer(0, StudioBlendMode::Normal), layer(1, StudioBlendMode::Normal)])]),
        ext(LayerExtension::new(PlanNodeId(2), 1).with_adjustment(exposure)),
        Box::new(move |x, y| {
            let p = scene(x as u32, y as u32);
            let o = composite_adjustment_in(SampleRange::Extended, StudioBlendMode::Normal, Rgba::new([p[0], p[1], p[2]], 1.0), &prepared, 1.0).unwrap();
            [o.rgb[0], o.rgb[1], o.rgb[2], o.a]
        }),
    ));
    for (name, filter) in [
        ("gaussian_blur", LayerFilter::GaussianBlur { radius_px: 2.0 }),
        ("unsharp_mask", LayerFilter::UnsharpMask { amount_pct: 150.0, radius_px: 1.5, threshold_levels: 0 }),
        ("median", LayerFilter::Median { radius_px: 1 }),
    ] {
        let out = filter_window_in(SampleRange::Extended, &whole, EXTENT, EXTENT, &filter, WIDE_F).unwrap();
        let (p, e) = single(LayerExtension::new(PlanNodeId(1), 0).with_filters(vec![filter]));
        cases.push((name, p, e, Box::new(move |x, y| over([0.0; 4], at(&out, x, y), x, y))));
    }
    let taps = LayerBlur::new(BlurScope::Layer, 2.0).taps().unwrap();
    let blurred = blur_window_in(SampleRange::Extended, &whole, EXTENT, EXTENT, &taps, WIDE_B).unwrap();
    {
        let b = blurred.clone();
        let (p, e) = single(LayerExtension::new(PlanNodeId(1), 0).with_blur(LayerBlur::new(BlurScope::Layer, 2.0)));
        cases.push(("layer_blur", p, e, Box::new(move |x, y| over([0.0; 4], at(&b, x, y), x, y))));
    }
    // Backdrop blur under a 6x4 block (alpha 0.6) at (3, 2).
    let block = Tile::new(6, 4, |_, _| [0.5, 4.0, 1.0, 0.6]);
    {
        let b = blurred.clone();
        cases.push((
            "backdrop_blur",
            plan(vec![PlanNode::Tile(s.input(1, 0, 0)), PlanNode::Tile(block.input(2, 3, 2)), group(vec![layer(0, StudioBlendMode::Normal), layer(1, StudioBlendMode::Normal)])]),
            ext(LayerExtension::new(PlanNodeId(2), 1).with_blur(LayerBlur::new(BlurScope::Backdrop, 2.0))),
            Box::new(move |x, y| {
                let base = scene(x as u32, y as u32);
                if !((3..9).contains(&x) && (2..6).contains(&y)) {
                    return base;
                }
                let mixed = backdrop_mix_in(SampleRange::Extended, base, at(&b, x, y), 0.6).unwrap();
                over(mixed, [0.5, 4.0, 1.0, 0.6], x, y)
            }),
        ));
    }
    // Bicubic transform of the 6x4 scene corner, scaled 1.5x and moved by (1.25, 0.75).
    let corner = Tile::new(6, 4, scene);
    let t = LayerTransform::new(Affine2 { m: [1.5, 0.0, 1.25, 0.0, 1.5, 0.75] }, ResampleFilter::Bicubic);
    let prep = PreparedTransform::new(&t, Rect { x: 0, y: 0, width: 6, height: 4 }).unwrap();
    {
        let tile = Tile::new(6, 4, scene);
        cases.push((
            "transform",
            plan(vec![PlanNode::Tile(corner.input(3, 0, 0)), group(vec![layer(0, StudioBlendMode::Normal)])]),
            ext(LayerExtension::new(PlanNodeId(1), 0).with_transform(t)),
            Box::new(move |x, y| {
                let p = prep.sample_rgba_in::<()>(SampleRange::Extended, x, y, |i, j| Ok(tile.at(i, j))).unwrap();
                over([0.0; 4], p, x, y)
            }),
        ));
    }
    let src_all = Sources(vec![(1, s.planes()), (2, block.planes()), (3, corner.planes())]);
    let cancel = CancellationToken::default();
    let mut proven = 0;
    for chunk_side in [8u32, 64] {
        let Some(gpu) = live(chunk_side, "gpu extended envelope parity") else { return };
        for (name, p, e, want) in &cases {
            let mut grid = Grid { px: vec![[f32::NAN; 4]; (W * H) as usize] };
            let (receipt, _) = gpu
                .execute_extended_detailed(p, e, &request(), &src_all, &cancel, &mut grid)
                .unwrap_or_else(|err| panic!("{name} chunk {chunk_side}: {err:?}"));
            assert!(receipt.exactness.operators.iter().any(|o| o.key.starts_with("sample_range_extended_")), "{name}");
            let (mut max, mut above) = (0f32, 0);
            for y in 0..H as i64 {
                for x in 0..W as i64 {
                    let (got, w) = (grid.px[(y * W as i64 + x) as usize], want(x, y));
                    above += usize::from(w[..3].iter().any(|v| *v > 1.0));
                    for (g, r) in got.iter().zip(w) {
                        max = max.max((g - r).abs() / r.abs().max(1.0));
                    }
                }
            }
            println!("chunk {chunk_side} {name}: max relative |delta| vs pigment = {max:e} (tolerance {TOLERANCE:e})");
            assert!(max <= TOLERANCE, "{name} chunk {chunk_side}: {max:e}");
            assert!(above > 0, "{name}");
        }
        // Drop shadow hand values.
        let four = Tile::new(2, 2, |_, _| [4.0, 4.0, 4.0, 1.0]);
        let fx_plan = plan(vec![PlanNode::Solid { rect: EXTENT, rgba: [3.0, 3.0, 3.0, 1.0] }, PlanNode::Tile(four.input(4, 4, 3)), group(vec![layer(0, StudioBlendMode::Normal), layer(1, StudioBlendMode::Normal)])]);
        let mut shadow = DropShadowOp::new([0.5, 0.5, 0.5], StudioBlendMode::Multiply, 1.0);
        shadow.distance_px = 2.0;
        let fx_ext = ext(LayerExtension::new(PlanNodeId(2), 1).with_effects(vec![EffectOp::DropShadow(shadow)]));
        let mut grid = Grid { px: vec![[f32::NAN; 4]; (W * H) as usize] };
        gpu.execute_extended_detailed(&fx_plan, &fx_ext, &request(), &Sources(vec![(4, four.planes())]), &cancel, &mut grid).unwrap();
        let near = |a: [f32; 4], b: [f32; 4]| a.iter().zip(b).all(|(x, y)| (x - y).abs() <= TOLERANCE * y.abs().max(1.0));
        assert!(near(grid.px[(3 * W + 4) as usize], [4.0, 4.0, 4.0, 1.0]));
        assert!(near(grid.px[0], [3.0, 3.0, 3.0, 1.0]));
        assert!(grid.px.iter().any(|p| near(*p, [1.5, 1.5, 1.5, 1.0])), "no shadow-only pixel at 3.0 x 0.5");
        proven += 1;
    }
    assert_eq!(proven, 2);

}

/// Overflow is the typed refusal render-cpu returns on both GPU outputs: an unsharp mask of 500 %
/// across a 3e38 / 0 edge on the read-back path, and 3e38 + 3e38 under linear dodge on the
/// shared-texture path (the pass flags the texel instead of displaying it; one 4-byte flag read).
/// An extended plan without overflow renders into the texture with 4 bytes read back, a unit plan
/// with none.
#[test]
fn gpu_overflow_in_envelope_operations_and_on_the_shared_texture_is_typed() {
    let refused = GpuError::Backend(BackendError::Unsupported { operator: SAMPLE_RANGE_OPERATOR, exactness: Exactness::Unsupported });
    let Some(gpu) = live(16, "gpu envelope overflow") else { return };
    let edge = Tile::new(W, H, |x, _| if x < 6 { [3.0e38, 1.0, 1.0, 1.0] } else { [0.0, 1.0, 1.0, 1.0] });
    let p = plan(vec![PlanNode::Tile(edge.input(1, 0, 0)), group(vec![layer(0, StudioBlendMode::Normal)])]);
    let e = ext(LayerExtension::new(PlanNodeId(1), 0).with_filters(vec![LayerFilter::UnsharpMask { amount_pct: 500.0, radius_px: 1.0, threshold_levels: 0 }]));
    let mut grid = Grid { px: vec![[0.0; 4]; (W * H) as usize] };
    assert_eq!(gpu.execute_extended_detailed(&p, &e, &request(), &Sources(vec![(1, edge.planes())]), &CancellationToken::default(), &mut grid).unwrap_err(), refused);
    shared_texture_overflow(refused);
}

#[cfg(feature = "shared-device")]
fn shared_texture_overflow(refused: GpuError) {
    use hsk_studio_render_gpu::{SharedDevice, TARGET_FORMAT};
    let backends = if cfg!(windows) { wgpu::Backends::DX12 } else { wgpu::Backends::PRIMARY };
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor { backends, flags: wgpu::InstanceFlags::empty(), ..Default::default() });
    let adapter = block(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .expect("an adapter (the read-back half found one)");
    let (device, queue) = block(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("test host device"),
        required_limits: wgpu::Limits::downlevel_defaults(),
        ..Default::default()
    }))
    .expect("a device");
    let shared = SharedDevice::new(&device, &queue, adapter.get_info(), DeviceOptions { chunk_side: 8, ..DeviceOptions::default() }).unwrap();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test target"),
        size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET_FORMAT,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let cancel = CancellationToken::default();
    let solids = |a: f32, b: f32, mode: StudioBlendMode| {
        plan(vec![
            PlanNode::Solid { rect: EXTENT, rgba: [a, 1.0, 1.0, 1.0] },
            PlanNode::Solid { rect: EXTENT, rgba: [b, 0.5, 0.5, 1.0] },
            group(vec![layer(0, StudioBlendMode::Normal), layer(1, mode)]),
        ])
    };
    let stage = PlanExtensions::default().with_output(OutputStage::new(SampleRange::Extended, None));
    assert_eq!(shared.render_to_texture(&solids(3.0e38, 3.0e38, StudioBlendMode::LinearDodgeAdd), Some(&stage), &request(), &Sources(vec![]), &cancel, &texture).unwrap_err(), refused);
    let (_, stats) = shared.render_to_texture(&solids(2.0, 3.0, StudioBlendMode::LinearDodgeAdd), Some(&stage), &request(), &Sources(vec![]), &cancel, &texture).unwrap();
    assert_eq!(stats.bytes_read_back, 4);
    let (_, stats) = shared.render_to_texture(&solids(0.25, 0.5, StudioBlendMode::Multiply), None, &request(), &Sources(vec![]), &cancel, &texture).unwrap();
    assert_eq!(stats.bytes_read_back, 0);
    // The overflow flag is cleared per execution: a later clean extended frame succeeds.
    shared.render_to_texture(&solids(2.0, 3.0, StudioBlendMode::LinearDodgeAdd), Some(&stage), &request(), &Sources(vec![]), &cancel, &texture).unwrap();
}
#[cfg(not(feature = "shared-device"))]
fn shared_texture_overflow(_: GpuError) {
    println!("NOT_PROVEN: shared-texture overflow needs --features shared-device");
}

#[cfg(feature = "shared-device")]
fn block<F: std::future::Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    let start = std::time::Instant::now();
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        assert!(start.elapsed().as_secs() < 30, "wgpu request timed out");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
