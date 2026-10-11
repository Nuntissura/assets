//! Focused proofs for the GPU backend (MT-35546 cases). Everything except the GPU-execution test
//! (`gpu_blend_parity_vs_pigment_blend_math`) is hardware-independent and must pass on a machine
//! without an adapter. The GPU-execution test asserts the typed `Unavailable` and prints
//! `NOT_PROVEN` when no adapter exists: it never claims a GPU proof it did not run. Device
//! creation is serialised by `DEVICE_LOCK` (one device at a time in this process).
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BlendSpace, ColourTag, Exactness, ExecuteRequest, Group, Layer,
    MaskFormat, MaskInput, NoVector, OutputTarget, PlanError, PlanNode, PlanNodeId, PlanSources,
    RenderBackend, RenderPlan, SampleFormat, SinkError, SourceKey, StudioBlendMode, TileInput,
    TilePlanes, TileSink,
};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_render_gpu::{
    BackendChoice, DeviceInfo, DeviceOptions, GpuBackend, GpuContext, GpuError, Reason, Residency,
    ResidentKey,
};
use std::sync::{Mutex, MutexGuard, PoisonError};

static DEVICE_LOCK: Mutex<()> = Mutex::new(());
fn device_lock() -> MutexGuard<'static, ()> {
    DEVICE_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}
fn tag() -> ColourTag {
    ColourTag {
        profile_sha256: [7; 32],
        blend_space: BlendSpace::LinearLight,
    }
}
fn target(extent: Rect) -> OutputTarget {
    OutputTarget {
        extent,
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: tag(),
    }
}
fn layer(input: u32, mode: StudioBlendMode, opacity: f32, fill: f32) -> Layer {
    Layer {
        input: PlanNodeId(input),
        mode,
        opacity,
        fill_opacity: fill,
        mask: None,
    }
}
fn plan_of(
    revision: u64,
    extent: Rect,
    mut nodes: Vec<PlanNode<NoVector>>,
    layers: Vec<Layer>,
) -> RenderPlan<NoVector> {
    nodes.push(PlanNode::Group(Group {
        isolated: true,
        layers,
    }));
    RenderPlan {
        revision,
        target: target(extent),
        root: PlanNodeId(nodes.len() as u32 - 1),
        nodes,
    }
}
fn request(revision: u64, min: Exactness) -> ExecuteRequest {
    ExecuteRequest {
        expected_revision: revision,
        min_exactness: min,
        deadline: None,
        max_working_bytes: 1 << 30,
    }
}
fn solid(r: Rect, rgba: [f32; 4]) -> PlanNode<NoVector> {
    PlanNode::Solid { rect: r, rgba }
}

struct OwnedTile {
    width: u32,
    height: u32,
    colour: Vec<u8>,
    alpha: Vec<u8>,
    colour_sha: [u8; 32],
    alpha_sha: [u8; 32],
}
impl OwnedTile {
    /// Deterministic pseudo-random straight RGBA tile with exact 0 and 1 samples mixed in.
    fn random(width: u32, height: u32, seed: u32) -> Self {
        let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(12_345);
        let mut next = move || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            match (state >> 24) % 11 {
                0 => 0.0_f32,
                1 => 1.0_f32,
                _ => (state >> 8) as f32 / 16_777_216.0,
            }
        };
        let mut colour = Vec::new();
        let mut alpha = Vec::new();
        for _ in 0..width * height {
            for _ in 0..3 {
                colour.extend_from_slice(&next().to_le_bytes());
            }
            alpha.extend_from_slice(&next().to_le_bytes());
        }
        Self {
            width,
            height,
            colour_sha: hsk_studio_prism::profile_hash(&colour),
            alpha_sha: hsk_studio_prism::profile_hash(&alpha),
            colour,
            alpha,
        }
    }
    fn input(&self, key: u32, placement: Rect) -> PlanNode<NoVector> {
        PlanNode::Tile(TileInput {
            key: SourceKey(key),
            placement,
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: self.colour_sha,
            alpha_sha256: self.alpha_sha,
        })
    }
    fn pixel(&self, x: u32, y: u32) -> [f32; 4] {
        let c = (y * self.width + x) as usize * 12;
        let a = (y * self.width + x) as usize * 4;
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        [
            f(&self.colour, c),
            f(&self.colour, c + 4),
            f(&self.colour, c + 8),
            f(&self.alpha, a),
        ]
    }
}
#[derive(Default)]
struct Sources(Vec<(u32, OwnedTile)>);
impl PlanSources for Sources {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        let (_, t) = self.0.iter().find(|(k, _)| *k == key.0)?;
        Some(TilePlanes {
            width: t.width,
            height: t.height,
            colour: &t.colour,
            colour_stride_bytes: u64::from(t.width) * 12,
            alpha: &t.alpha,
            alpha_stride_bytes: u64::from(t.width) * 4,
            colour_sha256: t.colour_sha,
            alpha_sha256: t.alpha_sha,
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: tag(),
        })
    }
    fn mask(&self, _key: SourceKey) -> Option<Mask<'_>> {
        None
    }
}

/// Collects chunks into one image; optionally rejects the Nth chunk.
struct Image {
    extent: Rect,
    px: Vec<[f32; 4]>,
    accepted: usize,
    reject_at: Option<usize>,
}
impl Image {
    fn new(extent: Rect) -> Self {
        Self {
            extent,
            px: vec![[f32::NAN; 4]; extent.width as usize * extent.height as usize],
            accepted: 0,
            reject_at: None,
        }
    }
}
impl TileSink for Image {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        if self.reject_at == Some(self.accepted) {
            return Err(SinkError::Rejected);
        }
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        for y in 0..r.height as usize {
            for x in 0..r.width as usize {
                let i = y * r.width as usize + x;
                let at = (r.y - self.extent.y) as usize * self.extent.width as usize
                    + (r.x - self.extent.x) as usize
                    + y * self.extent.width as usize
                    + x;
                self.px[at] = [
                    f(colour, i * 12),
                    f(colour, i * 12 + 4),
                    f(colour, i * 12 + 8),
                    f(alpha, i * 4),
                ];
            }
        }
        self.accepted += 1;
        Ok(())
    }
}

/// `HSK_STUDIO_TEST_GPU_BACKEND=none|vulkan|dx12|metal|auto` selects the backend of the GPU test:
/// `none` reproduces a machine without an adapter, the others qualify the proof per backend.
fn backend_from_env() -> BackendChoice {
    match std::env::var("HSK_STUDIO_TEST_GPU_BACKEND").as_deref() {
        Ok("none") => BackendChoice::None,
        Ok("vulkan") => BackendChoice::Vulkan,
        Ok("dx12") => BackendChoice::Dx12,
        Ok("metal") => BackendChoice::Metal,
        _ => BackendChoice::Auto,
    }
}
fn no_adapter() -> DeviceOptions {
    DeviceOptions {
        backend: BackendChoice::None,
        ..DeviceOptions::default()
    }
}
fn run(
    backend: &GpuBackend,
    plan: &RenderPlan<NoVector>,
    req: &ExecuteRequest,
    sources: &dyn PlanSources,
    sink: &mut Image,
) -> Result<hsk_studio_pigment::render_plan::RenderReceipt, BackendError> {
    backend.execute(plan, req, sources, &CancellationToken::default(), sink)
}

#[test]
fn admission_refusals_run_without_adapter() {
    let backend = GpuBackend::new(no_adapter());
    let extent = rect(0, 0, 4, 4);
    let sources = Sources::default();
    let mut sink = Image::new(extent);
    let base = |mode| {
        plan_of(
            5,
            extent,
            vec![solid(extent, [0.5, 0.5, 0.5, 1.0])],
            vec![layer(0, mode, 1.0, 1.0)],
        )
    };

    // stale revision is refused before the (missing) device is even considered
    let stale = run(
        &backend,
        &base(StudioBlendMode::Normal),
        &request(6, Exactness::Exact),
        &sources,
        &mut sink,
    );
    assert_eq!(
        stale,
        Err(BackendError::StaleRevision {
            expected: 6,
            found: 5
        })
    );
    // a mode the slice-1 shader does not implement is refused at admission, naming the operator
    let overlay = run(
        &backend,
        &base(StudioBlendMode::Overlay),
        &request(5, Exactness::Approximate),
        &sources,
        &mut sink,
    );
    assert!(matches!(
        overlay,
        Err(BackendError::Unsupported { operator, .. }) if operator == StudioBlendMode::Overlay.key()
    ));
    // GPU blend operators are class Fitted: an export floor of Exact refuses them (by design)
    let exact_floor = run(
        &backend,
        &base(StudioBlendMode::Multiply),
        &request(5, Exactness::Exact),
        &sources,
        &mut sink,
    );
    assert_eq!(
        exact_floor,
        Err(BackendError::Unsupported {
            operator: StudioBlendMode::Multiply.key(),
            exactness: Exactness::Fitted
        })
    );
    // structural validation is shared: a forward reference never reaches the device
    let forward = plan_of(
        5,
        extent,
        vec![solid(extent, [0.0, 0.0, 0.0, 1.0])],
        vec![layer(1, StudioBlendMode::Normal, 1.0, 1.0)],
    );
    assert!(matches!(
        run(
            &backend,
            &forward,
            &request(5, Exactness::Exact),
            &sources,
            &mut sink
        ),
        Err(BackendError::InvalidPlan(
            PlanError::ForwardReference { .. }
        ))
    ));
    // masks are not implemented: Unsupported, not silently ignored
    let mut masked = base(StudioBlendMode::Normal);
    if let PlanNode::Group(g) = masked.nodes.last_mut().expect("group") {
        g.layers[0].mask = Some(MaskInput {
            key: SourceKey(9),
            placement: extent,
            format: MaskFormat::CoverageU8,
            sha256: [0; 32],
            inverted: false,
            density: 1.0,
        });
    }
    assert!(matches!(
        run(
            &backend,
            &masked,
            &request(5, Exactness::Exact),
            &sources,
            &mut sink
        ),
        Err(BackendError::Unsupported { .. })
    ));
    // canceled token wins over everything
    let token = CancellationToken::default();
    token.cancel();
    assert_eq!(
        backend.execute(
            &base(StudioBlendMode::Normal),
            &request(5, Exactness::Exact),
            &sources,
            &token,
            &mut sink
        ),
        Err(BackendError::Canceled)
    );

    // source verification also runs without a device: unresolved and hash-mismatching tiles
    let tile = OwnedTile::random(4, 4, 1);
    let tile_plan = plan_of(
        5,
        extent,
        vec![tile.input(1, extent)],
        vec![layer(0, StudioBlendMode::Normal, 1.0, 1.0)],
    );
    assert_eq!(
        run(
            &backend,
            &tile_plan,
            &request(5, Exactness::Fitted),
            &sources,
            &mut sink
        ),
        Err(BackendError::UnresolvedSource(SourceKey(1)))
    );
    let mut bad = OwnedTile::random(4, 4, 1);
    bad.colour[0] ^= 0x01; // bytes no longer match the declared hash
    let bad_sources = Sources(vec![(1, bad)]);
    assert_eq!(
        run(
            &backend,
            &tile_plan,
            &request(5, Exactness::Fitted),
            &bad_sources,
            &mut sink
        ),
        Err(BackendError::SourceMismatch(SourceKey(1)))
    );

    // nothing above touched the sink or the residency ledger
    assert_eq!(sink.accepted, 0);
    assert_eq!(backend.resident_bytes(), 0);

    // a fully valid plan on a backend with no adapter is the typed Unavailable result
    let ok = run(
        &backend,
        &base(StudioBlendMode::Normal),
        &request(5, Exactness::Fitted),
        &sources,
        &mut sink,
    );
    assert_eq!(ok, Err(BackendError::Unavailable("no_adapter")));
    assert_eq!(sink.accepted, 0);
    assert_eq!(backend.unavailable_reason(), Some(Reason::NoAdapter));
    // capability query: what the slice implements, nothing else
    use hsk_studio_pigment::render_plan::Operator;
    assert_eq!(backend.exactness(Operator::Solid), Exactness::Exact);
    assert_eq!(
        backend.exactness(Operator::Blend(StudioBlendMode::Screen)),
        Exactness::Fitted
    );
    assert_eq!(
        backend.exactness(Operator::Blend(StudioBlendMode::SoftLight)),
        Exactness::Unsupported
    );
    assert_eq!(
        backend.exactness(Operator::IsolatedGroup),
        Exactness::Unsupported
    );
}

#[test]
fn device_loss_and_unavailable_are_explicit() {
    let token = CancellationToken::default();
    match GpuContext::acquire(&no_adapter(), &token) {
        Err(GpuError::Unavailable(Reason::NoAdapter)) => {}
        Err(other) => panic!("expected Unavailable(NoAdapter), got {other}"),
        Ok(_) => panic!("BackendChoice::None must not acquire a device"),
    }

    // Injected loss (the state the real callback latches): every execute reports it, stale
    // generations are never silently replaced, and a failed re-acquire keeps the loss.
    let extent = rect(0, 0, 2, 2);
    let plan = plan_of(
        1,
        extent,
        vec![solid(extent, [0.1, 0.2, 0.3, 1.0])],
        vec![layer(0, StudioBlendMode::Normal, 1.0, 1.0)],
    );
    let sources = Sources::default();
    let mut sink = Image::new(extent);
    let lost = GpuBackend::with_injected_loss(no_adapter(), 7);
    for _ in 0..2 {
        assert_eq!(
            run(
                &lost,
                &plan,
                &request(1, Exactness::Fitted),
                &sources,
                &mut sink
            ),
            Err(BackendError::DeviceLost { generation: 7 })
        );
    }
    assert!(lost.is_lost());
    assert_eq!(lost.generation(), 7);
    assert_eq!(lost.identity().generation, 7);
    assert_eq!(
        lost.reacquire(&token),
        Err(GpuError::Unavailable(Reason::NoAdapter))
    );
    assert_eq!(
        run(
            &lost,
            &plan,
            &request(1, Exactness::Fitted),
            &sources,
            &mut sink
        ),
        Err(BackendError::DeviceLost { generation: 7 })
    );
    assert_eq!((sink.accepted, lost.resident_bytes()), (0, 0));
}

#[test]
fn residency_lru_budget_accounting() {
    let tile = |source: u32| ResidentKey::Tile {
        source,
        bounds: rect(0, 0, 4, 4),
    };
    let mut res = Residency::new(100);
    let a = res.reserve(tile(1), 40).expect("a");
    let b = res.reserve(tile(2), 40).expect("b");
    assert_eq!(
        (res.used(), res.peak_used(), res.pinned_bytes()),
        (80, 80, 80)
    );

    // everything pinned: nothing may be evicted, the refusal is typed and leaves state intact
    assert_eq!(
        res.reserve(tile(3), 30),
        Err(GpuError::BudgetExceeded {
            requested: 30,
            budget: 100
        })
    );
    assert_eq!((res.used(), res.evictions(), res.len()), (80, 0, 2));
    // a single entry above the budget is refused outright
    assert_eq!(
        res.reserve(tile(9), 101),
        Err(GpuError::BudgetExceeded {
            requested: 101,
            budget: 100
        })
    );

    // unpin both, touch A: B is now the least recently used and goes first
    assert!(res.unpin(a.id) && res.unpin(b.id));
    assert!(res.touch(&tile(1)));
    let c = res.reserve(tile(3), 40).expect("c evicts b");
    assert_eq!(c.evicted, vec![tile(2)]);
    assert!(!c.hit);
    assert_eq!((res.used(), res.evictions()), (80, 1));
    assert!(res.contains(&tile(1)) && !res.contains(&tile(2)));

    // a resident key is a hit: touched and pinned, never charged twice; a size change is refused
    let again = res.reserve(tile(1), 40).expect("hit");
    assert!(again.hit && again.evicted.is_empty());
    assert_eq!(res.used(), 80);
    assert!(res.reserve(tile(1), 41).is_err());

    // release-on-exit: everything reserved after a mark returns to the baseline, counters stay
    let baseline = res.used();
    let mark = res.mark();
    res.reserve(ResidentKey::Scratch("dst"), 10)
        .expect("scratch");
    res.reserve(ResidentKey::Scratch("staging"), 10)
        .expect("scratch");
    assert_eq!(res.used(), baseline + 20);
    assert_eq!(res.peak_used(), 100);
    assert_eq!(res.release_since(mark), 20);
    assert_eq!(res.used(), baseline);
    assert!(res.release(a.id));
    assert_eq!(res.used(), baseline - 40);
    assert!(!res.release(a.id), "double release is refused");
    res.reset_peak();
    assert_eq!(res.peak_used(), res.used());
    res.clear();
    assert_eq!((res.used(), res.len()), (0, 0));
    assert!(res.evictions() >= 1);
}

#[test]
fn no_wgpu_types_in_public_api() {
    // STU-ARC-002: wgpu is a private leaf. Every `pub` declaration line in the crate sources and
    // the descriptor must be free of wgpu paths (private/`pub(crate)` items may use them).
    let sources = [
        ("lib.rs", include_str!("../src/lib.rs")),
        ("device.rs", include_str!("../src/device.rs")),
        ("error.rs", include_str!("../src/error.rs")),
        ("plan_sink.rs", include_str!("../src/plan_sink.rs")),
        ("report.rs", include_str!("../src/report.rs")),
        ("residency.rs", include_str!("../src/residency.rs")),
    ];
    for (name, text) in sources {
        for (n, line) in text.lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with("pub ") && !t.starts_with("pub const DESCRIPTOR") {
                assert!(
                    !t.contains("wgpu"),
                    "{name}:{}: public item mentions wgpu: {t}",
                    n + 1
                );
            }
        }
    }
    assert!(!hsk_studio_render_gpu::DESCRIPTOR.contains("wgpu::"));
    // GpuContext fields holding wgpu objects are crate-private
    let device_rs = include_str!("../src/device.rs");
    assert!(device_rs.contains("pub(crate) device: wgpu::Device"));
    assert!(!device_rs.contains("pub device: wgpu"));
    // DeviceInfo is plain data usable without any GPU crate
    let _info: Option<DeviceInfo> = None;
}

type SourceFn = Box<dyn Fn(i64, i64) -> Option<[f32; 4]>>;
type RefLayer = (StudioBlendMode, f32, SourceFn);

/// Per-layer reference: pigment `composite_over` with the executor's skip rules.
fn reference(layers: &[RefLayer], x: i64, y: i64) -> [f32; 4] {
    let mut dst = [0.0_f32; 4];
    for (mode, opacity, source) in layers {
        let Some(s) = source(x, y) else { continue };
        if *opacity <= 0.0 || s[3] <= 0.0 {
            continue;
        }
        let out = composite_over(
            *mode,
            Rgba::new([dst[0], dst[1], dst[2]], dst[3]),
            Rgba::new([s[0], s[1], s[2]], s[3]),
            *opacity,
            PixelSite::default(),
        )
        .expect("reference blend");
        dst = [out.rgb[0], out.rgb[1], out.rgb[2], out.a.min(1.0)];
    }
    dst
}

#[test]
fn gpu_blend_parity_vs_pigment_blend_math() {
    const TOL_PER_LAYER: f32 = 1.0e-6;
    let _guard = device_lock();
    let options = DeviceOptions {
        chunk_side: 16,
        backend: backend_from_env(),
        ..DeviceOptions::default()
    };
    let backend = GpuBackend::new(options);
    let sources0 = Sources::default();
    let tiny = rect(0, 0, 1, 1);
    let tiny_plan = plan_of(
        1,
        tiny,
        vec![solid(tiny, [0.2, 0.4, 0.6, 1.0])],
        vec![layer(0, StudioBlendMode::Normal, 1.0, 1.0)],
    );
    let Some(info) = backend.device_info() else {
        // Typed, explicit, and not a pass for the GPU claim: the lane records NOT_PROVEN.
        let reason = backend.unavailable_reason();
        eprintln!("NOT_PROVEN: no adapter ({reason:?}); GPU execution parity not run");
        assert!(matches!(
            reason,
            Some(Reason::NoAdapter | Reason::NoDevice | Reason::Timeout)
        ));
        let mut sink = Image::new(tiny);
        let refused = run(
            &backend,
            &tiny_plan,
            &request(1, Exactness::Fitted),
            &sources0,
            &mut sink,
        );
        assert!(matches!(refused, Err(BackendError::Unavailable(_))));
        assert_eq!(sink.accepted, 0);
        return;
    };
    eprintln!(
        "GPU_BACKEND: {} generation {}",
        info.text(),
        info.generation
    );

    // (a) hand-computed oracle: backdrop (0.2,0.4,0.6,1.0), red source at opacity 0.5.
    for (mode, expect) in [
        (StudioBlendMode::Normal, [0.6_f32, 0.2, 0.3, 1.0]),
        (StudioBlendMode::Multiply, [0.2, 0.2, 0.3, 1.0]),
        (StudioBlendMode::Screen, [0.6, 0.4, 0.6, 1.0]),
    ] {
        let plan = plan_of(
            1,
            tiny,
            vec![
                solid(tiny, [0.2, 0.4, 0.6, 1.0]),
                solid(tiny, [1.0, 0.0, 0.0, 1.0]),
            ],
            vec![
                layer(0, StudioBlendMode::Normal, 1.0, 1.0),
                layer(1, mode, 0.5, 1.0),
            ],
        );
        let mut sink = Image::new(tiny);
        let receipt = run(
            &backend,
            &plan,
            &request(1, Exactness::Fitted),
            &sources0,
            &mut sink,
        )
        .expect("hand oracle executes on the device");
        let got = sink.px[0];
        let err = got
            .iter()
            .zip(expect)
            .map(|(g, e)| (g - e).abs())
            .fold(0.0_f32, f32::max);
        eprintln!(
            "HAND_ORACLE {}: got {got:?} want {expect:?} max_err {err:e}",
            mode.key()
        );
        assert!(
            err <= 2.0 * TOL_PER_LAYER,
            "{} hand oracle error {err:e}",
            mode.key()
        );
        assert_eq!(receipt.exactness.lowest, Exactness::Fitted);
        assert_eq!(
            receipt.backend.device,
            hsk_studio_pigment::render_plan::DeviceClass::Gpu
        );
    }

    // (b) randomised parity against pigment blend_math across chunk boundaries and a non-zero
    // origin: 37x29 at (3,-2) with 16-texel chunks = 3 x 2 chunks, five layers, fill opacity,
    // tiles that overlap chunk edges and exact-0/1 samples.
    let extent = rect(3, -2, 37, 29);
    let t0 = OwnedTile::random(30, 25, 11);
    let t1 = OwnedTile::random(20, 20, 23);
    let p0 = rect(8, 0, 30, 25);
    let p1 = rect(18, 3, 20, 20);
    let s1 = rect(3, -2, 20, 20);
    let s2 = rect(0, -10, 100, 100);
    let plan = plan_of(
        9,
        extent,
        vec![
            t0.input(1, p0),
            solid(s1, [0.7, 0.1, 0.3, 0.6]),
            t1.input(2, p1),
            solid(s2, [0.25, 0.9, 0.5, 0.5]),
        ],
        vec![
            layer(0, StudioBlendMode::Normal, 0.9, 1.0),
            layer(1, StudioBlendMode::Multiply, 0.8, 1.0),
            layer(2, StudioBlendMode::Screen, 0.7, 0.5),
            layer(3, StudioBlendMode::Normal, 0.25, 1.0),
            layer(2, StudioBlendMode::Multiply, 1.0, 1.0),
        ],
    );
    let sources = Sources(vec![(1, t0), (2, t1)]);
    let mut sink = Image::new(extent);
    let receipt = run(
        &backend,
        &plan,
        &request(9, Exactness::Approximate),
        &sources,
        &mut sink,
    )
    .expect("randomised plan executes on the device");
    let tile_fn = |idx: u32, p: Rect| -> SourceFn {
        let src = sources
            .0
            .iter()
            .find(|(k, _)| *k == idx)
            .map(|(_, t)| OwnedTile {
                width: t.width,
                height: t.height,
                colour: t.colour.clone(),
                alpha: t.alpha.clone(),
                colour_sha: t.colour_sha,
                alpha_sha: t.alpha_sha,
            });
        let src = src.expect("tile");
        Box::new(move |x, y| {
            let (lx, ly) = (x - p.x, y - p.y);
            (lx >= 0 && ly >= 0 && lx < i64::from(p.width) && ly < i64::from(p.height))
                .then(|| src.pixel(lx as u32, ly as u32))
        })
    };
    let solid_fn = |r: Rect, c: [f32; 4]| -> SourceFn {
        Box::new(move |x, y| {
            (x >= r.x && y >= r.y && x < r.x + i64::from(r.width) && y < r.y + i64::from(r.height))
                .then_some(c)
        })
    };
    let layers: Vec<RefLayer> = vec![
        (StudioBlendMode::Normal, 0.9, tile_fn(1, p0)),
        (
            StudioBlendMode::Multiply,
            0.8,
            solid_fn(s1, [0.7, 0.1, 0.3, 0.6]),
        ),
        (StudioBlendMode::Screen, 0.7 * 0.5, tile_fn(2, p1)),
        (
            StudioBlendMode::Normal,
            0.25,
            solid_fn(s2, [0.25, 0.9, 0.5, 0.5]),
        ),
        (StudioBlendMode::Multiply, 1.0, tile_fn(2, p1)),
    ];
    let mut max_err = 0.0_f32;
    let mut worst = (0, 0);
    for y in 0..extent.height as i64 {
        for x in 0..extent.width as i64 {
            let want = reference(&layers, extent.x + x, extent.y + y);
            let got = sink.px[y as usize * extent.width as usize + x as usize];
            for c in 0..4 {
                let e = (got[c] - want[c]).abs();
                assert!(e.is_finite(), "non-finite output at ({x},{y})");
                if e > max_err {
                    max_err = e;
                    worst = (x, y);
                }
            }
        }
    }
    let tolerance = TOL_PER_LAYER * layers.len() as f32;
    eprintln!(
        "PARITY max_err {max_err:e} at {worst:?} (tolerance {tolerance:e}, {} layers)",
        layers.len()
    );
    assert!(
        max_err <= tolerance,
        "GPU vs pigment blend_math max error {max_err:e} > {tolerance:e}"
    );

    // receipt and accounting facts for the randomised run
    assert_eq!(receipt.chunks, 6);
    assert_eq!(receipt.region, extent);
    assert_eq!(receipt.plan_revision, 9);
    assert_eq!(receipt.exactness.lowest, Exactness::Approximate); // fill_opacity with Screen
    let stats = backend.last_stats().expect("stats");
    assert_eq!((stats.chunk_width, stats.chunk_height), (16, 16));
    assert_eq!(stats.bytes_read_back, 37 * 29 * 16);
    assert!(stats.bytes_uploaded > 0 && stats.dispatches >= 5);
    assert_eq!(receipt.peak_working_bytes, 3 * 16 * 16 * 16 + 64);
    assert_eq!(backend.resident_bytes(), 0);

    // (c) sink rejection mid-read-back and an impossible budget both release residency and
    // leave the backend usable; neither produces a receipt.
    let mut reject = Image::new(extent);
    reject.reject_at = Some(2);
    assert_eq!(
        run(
            &backend,
            &plan,
            &request(9, Exactness::Approximate),
            &sources,
            &mut reject
        ),
        Err(BackendError::Sink(SinkError::Rejected))
    );
    assert_eq!((reject.accepted, backend.resident_bytes()), (2, 0));
    let mut starved = request(9, Exactness::Approximate);
    starved.max_working_bytes = 1000;
    let mut none = Image::new(extent);
    assert!(matches!(
        run(&backend, &plan, &starved, &sources, &mut none),
        Err(BackendError::BudgetExceeded { limit: 1000, .. })
    ));
    assert_eq!((none.accepted, backend.resident_bytes()), (0, 0));
    let mut again = Image::new(extent);
    run(
        &backend,
        &plan,
        &request(9, Exactness::Approximate),
        &sources,
        &mut again,
    )
    .expect("backend still usable after refusals");
    assert_eq!(again.px, sink.px);

    // (d) real device loss on this same device: destroy it through wgpu, observe
    // DeviceLost{generation}, then an explicit re-acquire bumps the generation and the backend
    // executes again (the hardware-free half of this contract is in
    // `device_loss_and_unavailable_are_explicit`).
    let first = info.generation;
    assert!(backend.simulate_device_loss());
    let mut lost_sink = Image::new(tiny);
    assert_eq!(
        run(
            &backend,
            &tiny_plan,
            &request(1, Exactness::Fitted),
            &sources0,
            &mut lost_sink
        ),
        Err(BackendError::DeviceLost { generation: first })
    );
    assert!(backend.device_info().is_none());
    let second = backend
        .reacquire(&CancellationToken::default())
        .expect("re-acquire after loss");
    assert!(second > first);
    let receipt = run(
        &backend,
        &tiny_plan,
        &request(1, Exactness::Fitted),
        &sources0,
        &mut lost_sink,
    )
    .expect("execute on the successor device");
    assert_eq!(receipt.backend.generation, second);
    assert_eq!(backend.resident_bytes(), 0);
}
