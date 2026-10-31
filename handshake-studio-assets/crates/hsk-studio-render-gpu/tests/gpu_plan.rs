//! Focused proofs for the GPU backend (MT-35546 cases plus the R3 regressions). The admission,
//! latch and residency tests are hardware-independent and must pass on a machine without an
//! adapter. Every GPU-execution proof goes through `gpu_gate`, which follows the checked-in
//! `tests/gpu-proof.toml` (V4-03): with `require_gpu = true` (committed) "no adapter" FAILS the
//! proof with a message starting `NOT_PROVEN: no adapter`; with `false` the proof prints that line
//! and passes, and the round counts it (exit 73, never green). An adapter whose device, shader or
//! pipeline failed, a timeout or a lost device FAILS the test in both modes.
//! `HSK_STUDIO_TEST_REQUIRE_GPU=1` only strengthens the file. Device creation is serialised by
//! `DEVICE_LOCK`; the parity tests share one lazily created backend.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::adjust_math::{
    AdjustmentKind, AdjustmentOp, BrightnessContrast, Curves, Exposure, HueSaturation, Levels,
    LevelsRecord, composite_adjustment,
};
use hsk_studio_pigment::blend_math::{
    Applicability, BlendContext, Family, MODES, PixelSite, Rgba, StackNode, blend_rgb,
    composite_over, evaluate_stack,
};
use hsk_studio_pigment::coverage_math::{
    BlendChannel, BlendRangeEntry, IDENTITY_RANGE, blend_if_weight_over, feather_kernel, mask_coverage,
};
use hsk_studio_pigment::effect_math::{
    ColourOverlayOp, DropShadowOp, EffectBlurKernel, EffectKind, EffectOp, EffectPlane, GlowFill,
    GlowSource, InnerGlowOp, InnerShadowOp, LayerParams as RefLayerParams, OuterGlowOp, StrokeOp,
    StrokePosition, render_layer_reference,
};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BlendSpace, ColourTag, Exactness, ExecuteRequest, ExtOperator,
    Group, Knockout, Layer, LayerExtension, MaskFormat, MaskInput, NoVector, OutputTarget,
    PlanError, PlanExtensions, PlanNode, PlanNodeId, PlanSources, RenderBackend, RenderPlan,
    SampleFormat, SinkError, SourceKey, StudioBlendMode, TileInput, TilePlanes, TileSink,
};
use hsk_studio_pigment::{Mask, Rect};
#[cfg(feature = "test-faults")]
use hsk_studio_render_gpu::GpuClientPort;
use hsk_studio_render_gpu::{
    AdmissionRequest, DeviceInfo, DeviceOptions, GpuBackend, GpuError, Reason, Residency,
    ResidentKey, UnavailablePort, gpu_adjust_supported, gpu_blend_qualifies, gpu_blend_supported,
};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
#[cfg(feature = "test-faults")]
use std::time::Instant;

/// The modes the backend must refuse, pinned HERE independently of `gpu_blend_qualifies` so the
/// predicate cannot silently widen or shrink: structural (pass_through is a group), stochastic
/// (dissolve: the plan carries no seed), tool-only and unspecified modes. Every other pigment mode
/// is generated and parity-tested; a refused mode is never replaced by Normal.
const REFUSED_MODES: [StudioBlendMode; 9] = [
    StudioBlendMode::PassThrough,
    StudioBlendMode::Dissolve,
    StudioBlendMode::Behind,
    StudioBlendMode::Clear,
    StudioBlendMode::Average,
    StudioBlendMode::Negation,
    StudioBlendMode::Reflect,
    StudioBlendMode::Glow,
    StudioBlendMode::Erase,
];

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

#[derive(Clone)]
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
    /// A tile whose pixel (x, y) is `pixel(x, y)` (straight RGBA).
    fn from_pixels(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [f32; 4]) -> Self {
        let mut colour = Vec::new();
        let mut alpha = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let p = pixel(x, y);
                for c in &p[..3] {
                    colour.extend_from_slice(&c.to_le_bytes());
                }
                alpha.extend_from_slice(&p[3].to_le_bytes());
            }
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
/// A coverage plane (`coverage_u8` or `coverage_u16le`) with deterministic pseudo-random samples
/// including exact 0 and full-scale values.
#[derive(Clone)]
struct OwnedMask {
    bounds: Rect,
    format: MaskFormat,
    bytes: Vec<u8>,
    sha: [u8; 32],
}
impl OwnedMask {
    fn random(bounds: Rect, format: MaskFormat, seed: u32) -> Self {
        let wide = format == MaskFormat::CoverageU16Le;
        let mut state = seed.wrapping_mul(2_246_822_519).wrapping_add(3_266_489_917);
        let mut bytes = Vec::new();
        for _ in 0..bounds.width * bounds.height {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let v = match (state >> 24) % 9 {
                0 => 0_u16,
                1 => {
                    if wide {
                        u16::MAX
                    } else {
                        255
                    }
                }
                _ => {
                    let r = (state >> 8) as u16;
                    if wide { r } else { r & 0xff }
                }
            };
            if wide {
                bytes.extend_from_slice(&v.to_le_bytes());
            } else {
                bytes.push(v as u8);
            }
        }
        Self {
            bounds,
            format,
            sha: hsk_studio_prism::profile_hash(&bytes),
            bytes,
        }
    }
    /// A `coverage_u8` plane with the given samples (row-major, one byte per pixel).
    fn from_u8(bounds: Rect, bytes: Vec<u8>) -> Self {
        Self {
            bounds,
            format: MaskFormat::CoverageU8,
            sha: hsk_studio_prism::profile_hash(&bytes),
            bytes,
        }
    }
    fn input(&self, key: u32, inverted: bool) -> MaskInput {
        MaskInput {
            key: SourceKey(key),
            placement: self.bounds,
            format: self.format,
            sha256: self.sha,
            inverted,
            density: 1.0,
        }
    }
    fn stride(&self) -> u64 {
        u64::from(self.bounds.width)
            * if self.format == MaskFormat::CoverageU16Le {
                2
            } else {
                1
            }
    }
    /// Independent conversion (f64 division, one f32 rounding), inversion in f32.
    fn coverage(&self, x: i64, y: i64, inverted: bool) -> f32 {
        let (lx, ly) = ((x - self.bounds.x) as usize, (y - self.bounds.y) as usize);
        let c = if self.format == MaskFormat::CoverageU16Le {
            let at = ly * self.stride() as usize + lx * 2;
            (f64::from(u16::from_le_bytes([self.bytes[at], self.bytes[at + 1]])) / 65535.0) as f32
        } else {
            (f64::from(self.bytes[ly * self.stride() as usize + lx]) / 255.0) as f32
        };
        if inverted { 1.0 - c } else { c }
    }
}

#[derive(Default)]
struct Sources {
    tiles: Vec<(u32, OwnedTile)>,
    masks: Vec<(u32, OwnedMask)>,
}
impl Sources {
    fn with_tiles(tiles: Vec<(u32, OwnedTile)>) -> Self {
        Self {
            tiles,
            masks: Vec::new(),
        }
    }
}
impl PlanSources for Sources {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        let (_, t) = self.tiles.iter().find(|(k, _)| *k == key.0)?;
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
    fn mask(&self, key: SourceKey) -> Option<Mask<'_>> {
        let (_, m) = self.masks.iter().find(|(k, _)| *k == key.0)?;
        Some(Mask {
            bounds: m.bounds,
            sample_format: m.format.key(),
            bytes: &m.bytes,
            stride_bytes: m.stride(),
            sha256: m.sha,
        })
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

/// `HSK_STUDIO_TEST_GPU_BACKEND` as given (`auto` when unset).
fn backend_key_from_env() -> String {
    std::env::var("HSK_STUDIO_TEST_GPU_BACKEND").unwrap_or_else(|_| "auto".to_owned())
}
/// The backend of a GPU test (lane SMB-GPU-CLIENT-PORT-1): `HSK_STUDIO_TEST_GPU_BACKEND` = `none`
/// is the wgpu-free `UnavailablePort` (a machine without an adapter); unset or `auto` is the
/// transient adapter's platform order (Vulkan first on Windows); `vulkan|dx12|metal` qualify the
/// proof per backend through the test-only feature `asset-local-backends`. Without that feature
/// only `vulkan` is accepted, and only when the transient default actually opened Vulkan: a
/// request is never silently proven on another backend (GUI-DESKTOP-026).
fn env_backend(options: DeviceOptions) -> GpuBackend {
    match backend_key_from_env().as_str() {
        "none" => GpuBackend::with_grant(
            Arc::new(UnavailablePort::new()),
            AdmissionRequest::asset_local(),
            options,
        ),
        "auto" => GpuBackend::new(options),
        other => selected_backend(options, other),
    }
}
#[cfg(feature = "asset-local-backends")]
fn selected_backend(options: DeviceOptions, key: &str) -> GpuBackend {
    use hsk_studio_render_gpu::BackendChoice;
    let backend = match key {
        "vulkan" => BackendChoice::Vulkan,
        "dx12" => BackendChoice::Dx12,
        "metal" => BackendChoice::Metal,
        other => panic!("HSK_STUDIO_TEST_GPU_BACKEND={other:?} is not none|auto|vulkan|dx12|metal"),
    };
    GpuBackend::new(DeviceOptions { backend, ..options })
}
#[cfg(not(feature = "asset-local-backends"))]
fn selected_backend(options: DeviceOptions, key: &str) -> GpuBackend {
    let backend = GpuBackend::new(options);
    let opened = backend.device_info().map(|info| info.backend);
    match (key, opened) {
        ("vulkan", Some("vulkan") | None) => backend,
        (other, opened) => panic!(
            "HSK_STUDIO_TEST_GPU_BACKEND={other:?} needs the test-only feature asset-local-backends \
             (the transient default opened {opened:?})"
        ),
    }
}
/// One backend for the GPU-execution tests (acquiring a device takes seconds). Created on first
/// use while the caller holds `DEVICE_LOCK`.
fn shared_backend() -> &'static GpuBackend {
    static SHARED: OnceLock<GpuBackend> = OnceLock::new();
    SHARED.get_or_init(|| {
        env_backend(DeviceOptions {
            chunk_side: 16,
            ..DeviceOptions::default()
        })
    })
}
/// `DeviceOptions` carrying the env selection where the test-only feature exists (the
/// fault-injection constructors take options, not a port).
#[cfg(feature = "test-faults")]
fn env_options() -> DeviceOptions {
    #[cfg(feature = "asset-local-backends")]
    {
        use hsk_studio_render_gpu::BackendChoice;
        let backend = match backend_key_from_env().as_str() {
            "none" => BackendChoice::None,
            "vulkan" => BackendChoice::Vulkan,
            "dx12" => BackendChoice::Dx12,
            "metal" => BackendChoice::Metal,
            _ => BackendChoice::Auto,
        };
        DeviceOptions { backend, ..DeviceOptions::default() }
    }
    #[cfg(not(feature = "asset-local-backends"))]
    {
        DeviceOptions::default()
    }
}
/// The wgpu-free no-device backend (the old `BackendChoice::None`).
fn no_adapter() -> GpuBackend {
    GpuBackend::with_grant(
        Arc::new(UnavailablePort::new()),
        AdmissionRequest::asset_local(),
        DeviceOptions::default(),
    )
}
/// The checked-in GPU proof policy (`tests/gpu-proof.toml`), compiled in so no test reads a path and
/// the round (which unsets every HSK_* variable) sees the committed value.
const GPU_PROOF_POLICY: &str = include_str!("gpu-proof.toml");

/// `require_gpu` of a policy text; a missing schema, a missing key or any other value is an error,
/// never a silent default.
fn parse_proof_policy(text: &str) -> Result<bool, String> {
    let (mut schema, mut require) = (None, None);
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("not a `key = value` line: {line:?}"));
        };
        match (key.trim(), value.trim()) {
            ("schema", v) => schema = Some(v.trim_matches('"').to_owned()),
            ("require_gpu", "true") => require = Some(true),
            ("require_gpu", "false") => require = Some(false),
            ("require_gpu", other) => {
                return Err(format!("require_gpu must be true|false: {other:?}"));
            }
            (other, _) => return Err(format!("unknown key {other:?}")),
        }
    }
    if schema.as_deref() != Some("hsk.studio.render-gpu-proof@1") {
        return Err(format!(
            "schema must be hsk.studio.render-gpu-proof@1, got {schema:?}"
        ));
    }
    require.ok_or_else(|| "require_gpu is missing".to_owned())
}
/// Strict mode: the committed policy, or `HSK_STUDIO_TEST_REQUIRE_GPU=1` (which only strengthens).
fn require_gpu() -> bool {
    parse_proof_policy(GPU_PROOF_POLICY)
        .unwrap_or_else(|e| panic!("tests/gpu-proof.toml is invalid: {e}"))
        || std::env::var("HSK_STUDIO_TEST_REQUIRE_GPU").as_deref() == Ok("1")
}
/// The line the round counts (`^[ \t]*NOT_PROVEN:`): a proof that did not run for lack of an adapter.
fn not_proven_line(what: &str) -> String {
    format!("NOT_PROVEN: no adapter; {what} not run")
}
/// `gpu_gate` with the strictness made explicit. `Ok(Some)`: a live device. `Ok(None)`: the one
/// NOT_PROVEN case (no adapter at all, or the forced `none` backend) while `strict` is false; the
/// `NOT_PROVEN:` line is printed. Strict mode, an adapter whose device, shader or pipeline failed, a
/// timeout and a lost device are `Err`: the calling test FAILS, never skips.
fn gate(backend: &GpuBackend, what: &str, strict: bool) -> Result<Option<DeviceInfo>, String> {
    gate_with(backend, what, strict, &mut |line| eprintln!("{line}"))
}
/// `gate` with the NOT_PROVEN line handed to `emit` (the regression probe captures it instead of
/// printing, so the probe itself is not counted as a NOT_PROVEN by the round).
fn gate_with(
    backend: &GpuBackend,
    what: &str,
    strict: bool,
    emit: &mut dyn FnMut(String),
) -> Result<Option<DeviceInfo>, String> {
    if let Some(info) = backend.device_info() {
        return Ok(Some(info));
    }
    if let Some(failure) = backend.failure() {
        return Err(format!(
            "{what}: adapter present but the backend failed: {failure} ({failure:?})"
        ));
    }
    match backend.unavailable_reason() {
        Some(Reason::NoAdapter) if !strict => {
            emit(not_proven_line(what));
            Ok(None)
        }
        Some(Reason::NoAdapter) => Err(format!(
            "NOT_PROVEN: no adapter; {what} requires a GPU (tests/gpu-proof.toml require_gpu = true)"
        )),
        other => Err(format!(
            "{what}: no live device (unavailable {other:?}, lost {})",
            backend.is_lost()
        )),
    }
}
/// Gate of every GPU-execution proof, with the committed strictness.
fn gpu_gate(backend: &GpuBackend, what: &str) -> Result<Option<DeviceInfo>, String> {
    gate(backend, what, require_gpu())
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

/// R5 finding `no-adapter-early-return-green` (regression, hardware-independent): without an
/// adapter a GPU proof is never a green pass when the checked-in policy requires the GPU, and the
/// permitted NOT_PROVEN route prints the line the round counts.
#[test]
fn no_adapter_is_not_a_green_gpu_proof_when_the_gpu_is_required() {
    const SCHEMA: &str = "schema = \"hsk.studio.render-gpu-proof@1\"\n";
    // the committed policy is well-formed; malformed policies are errors, never a default
    let committed = parse_proof_policy(GPU_PROOF_POLICY).expect("tests/gpu-proof.toml is valid");
    assert_eq!(
        parse_proof_policy(&format!("{SCHEMA}require_gpu = true\n")),
        Ok(true)
    );
    assert_eq!(
        parse_proof_policy(&format!("{SCHEMA}require_gpu = false\n")),
        Ok(false)
    );
    for bad in [
        "".to_owned(),
        "require_gpu = true\n".to_owned(),
        SCHEMA.to_owned(),
        format!("{SCHEMA}require_gpu = maybe\n"),
        format!("{SCHEMA}require_gpu = true\nextra = 1\n"),
        format!("{SCHEMA}require_gpu\n"),
        "schema = \"other\"\nrequire_gpu = true\n".to_owned(),
    ] {
        assert!(parse_proof_policy(&bad).is_err(), "accepted {bad:?}");
    }

    // strict gate on a machine without an adapter (forced `none` backend): a FAILURE whose message
    // starts with the NOT_PROVEN marker
    let backend = no_adapter();
    let strict = gate(&backend, "regression probe", true);
    assert!(
        matches!(&strict, Err(m) if m.starts_with("NOT_PROVEN: no adapter")),
        "strict gate must fail on no adapter, got {:?}",
        strict.as_ref().map(|d| d.is_some())
    );
    // opt-out route: not an error, and the line it prints is the one the round counts
    // (captured, not printed: the probe must not add a NOT_PROVEN line to the round output)
    let mut emitted = Vec::new();
    assert!(matches!(
        gate_with(&backend, "regression probe", false, &mut |l| emitted.push(l)),
        Ok(None)
    ));
    assert_eq!(emitted, vec![not_proven_line("regression probe")]);
    let line = emitted.remove(0);
    assert!(line.starts_with("NOT_PROVEN: no adapter;"), "{line}");
    assert!(line.contains("regression probe"));
    // the gate every GPU proof uses follows the committed policy (env can only add strictness)
    let env_strict = std::env::var("HSK_STUDIO_TEST_REQUIRE_GPU").as_deref() == Ok("1");
    assert_eq!(require_gpu(), committed || env_strict);
    assert_eq!(
        gpu_gate(&backend, "regression probe").is_err(),
        require_gpu(),
        "gpu_gate must fail on no adapter exactly when the GPU is required"
    );
}

#[test]
fn admission_refusals_run_without_adapter() {
    let backend = no_adapter();
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
    // modes the shader does not implement (stochastic dissolve, unspecified) are refused at
    // admission, naming the operator, never replaced by Normal
    for mode in REFUSED_MODES
        .iter()
        .copied()
        .filter(|m| m.facts().applicability == Applicability::Layer)
        .chain([StudioBlendMode::Average])
    {
        let refused = run(
            &backend,
            &base(mode),
            &request(5, Exactness::Approximate),
            &sources,
            &mut sink,
        );
        assert!(
            matches!(
                refused,
                Err(BackendError::Unsupported { operator, .. }) if operator == mode.key()
            ),
            "{mode:?}: {refused:?}"
        );
    }
    // the supported list is derived from pigment's facts: no drift between the two predicates
    // (a qualifying mode without a WGSL body would fail here until it gets one)
    for facts in &MODES {
        assert_eq!(
            gpu_blend_supported(facts.mode),
            gpu_blend_qualifies(facts.mode),
            "{} qualifies by pigment facts but has no WGSL formula (or vice versa)",
            facts.key
        );
        // ...and the list itself is pinned independently of the predicate: exactly the modes in
        // REFUSED_MODES are refused, every other pigment mode is generated
        assert_eq!(
            gpu_blend_supported(facts.mode),
            !REFUSED_MODES.contains(&facts.mode),
            "{}: supported list differs from the declared refusals",
            facts.key
        );
    }
    assert!(gpu_blend_supported(StudioBlendMode::Normal));
    assert!(gpu_blend_supported(StudioBlendMode::Overlay));
    assert!(gpu_blend_supported(StudioBlendMode::LinearBurn));
    assert!(gpu_blend_supported(StudioBlendMode::Hue));
    assert!(!gpu_blend_supported(StudioBlendMode::PassThrough));
    // pigment's class travels: an Approximate mode is refused by a Fitted floor (class Approximate,
    // not the backend's Fitted), admitted by an Approximate floor; an Exact non-separable mode is
    // Fitted on the GPU, refused by an Exact floor
    for (mode, floor, class) in [
        (
            StudioBlendMode::LinearBurn,
            Exactness::Fitted,
            Exactness::Approximate,
        ),
        (
            StudioBlendMode::HardMix,
            Exactness::Exact,
            Exactness::Approximate,
        ),
        (StudioBlendMode::Hue, Exactness::Exact, Exactness::Fitted),
        (
            StudioBlendMode::Luminosity,
            Exactness::Exact,
            Exactness::Fitted,
        ),
    ] {
        assert_eq!(
            run(
                &backend,
                &base(mode),
                &request(5, floor),
                &sources,
                &mut sink
            ),
            Err(BackendError::Unsupported {
                operator: mode.key(),
                exactness: class
            }),
            "{mode:?} at floor {floor:?}"
        );
    }
    for (mode, floor) in [
        (StudioBlendMode::LinearBurn, Exactness::Approximate),
        (StudioBlendMode::Divide, Exactness::Approximate),
        (StudioBlendMode::DarkerColour, Exactness::Approximate),
        (StudioBlendMode::Hue, Exactness::Fitted),
        (StudioBlendMode::Saturation, Exactness::Approximate),
    ] {
        assert_eq!(
            run(
                &backend,
                &base(mode),
                &request(5, floor),
                &sources,
                &mut sink
            ),
            Err(BackendError::Unavailable("no_adapter")),
            "{mode:?} at floor {floor:?} is admitted (then the missing device)"
        );
    }
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
    // masks are implemented, so an unresolved mask source is the typed refusal (not "unsupported")
    let mask_input = |format, density| MaskInput {
        key: SourceKey(9),
        placement: extent,
        format,
        sha256: [0; 32],
        inverted: false,
        density,
    };
    let with_mask = |mask: MaskInput| {
        let mut plan = base(StudioBlendMode::Normal);
        if let PlanNode::Group(g) = plan.nodes.last_mut().expect("group") {
            g.layers[0].mask = Some(mask);
        }
        plan
    };
    assert_eq!(
        run(
            &backend,
            &with_mask(mask_input(MaskFormat::CoverageU8, 1.0)),
            &request(5, Exactness::Fitted),
            &sources,
            &mut sink
        ),
        Err(BackendError::UnresolvedSource(SourceKey(9)))
    );
    // mask density other than 1 has no verified semantics: refused, never approximated
    assert!(matches!(
        run(
            &backend,
            &with_mask(mask_input(MaskFormat::CoverageU16Le, 0.5)),
            &request(5, Exactness::Fitted),
            &sources,
            &mut sink
        ),
        Err(BackendError::Unsupported {
            operator: "mask_density",
            ..
        })
    ));
    // group shapes this backend does not implement stay refused at admission
    let group_plan = |child_isolated: bool, mode: StudioBlendMode, opacity: f32| {
        let mut nodes = vec![solid(extent, [0.5, 0.5, 0.5, 1.0])];
        nodes.push(PlanNode::Group(Group {
            isolated: child_isolated,
            layers: vec![layer(0, StudioBlendMode::Normal, 1.0, 1.0)],
        }));
        plan_of(5, extent, nodes, vec![layer(1, mode, opacity, 1.0)])
    };
    let approx = request(5, Exactness::Approximate);
    assert!(matches!(
        run(
            &backend,
            &group_plan(false, StudioBlendMode::Multiply, 1.0),
            &approx,
            &sources,
            &mut sink
        ),
        Err(BackendError::Unsupported {
            operator: "group_non_isolated_blended",
            ..
        })
    ));
    // an isolated group and a pass-through group are accepted by admission: the only remaining
    // refusal for these valid plans is the missing device
    for plan in [
        group_plan(true, StudioBlendMode::Multiply, 0.5),
        group_plan(false, StudioBlendMode::PassThrough, 1.0),
    ] {
        assert_eq!(
            run(
                &backend,
                &plan,
                &request(5, Exactness::Fitted),
                &sources,
                &mut sink
            ),
            Err(BackendError::Unavailable("no_adapter"))
        );
    }
    // pass-through opacity (lane SMB-PS-ORACLE-PARITY-2, pigment `pass_through_mix`, class
    // Approximate): admitted at an Approximate floor (then Unavailable here)
    assert_eq!(
        run(
            &backend,
            &group_plan(false, StudioBlendMode::PassThrough, 0.5),
            &approx,
            &sources,
            &mut sink
        ),
        Err(BackendError::Unavailable("no_adapter"))
    );
    // clip-to-base is implemented: a valid clipped plan is admitted (then Unavailable here)...
    let clip_plan = |clipped_first: bool| {
        let mut layers = vec![
            layer(0, StudioBlendMode::Normal, 1.0, 1.0),
            layer(0, StudioBlendMode::Multiply, 0.5, 1.0),
        ];
        layers[usize::from(!clipped_first)].mask = Some(MaskInput::clip_to_base());
        plan_of(5, extent, vec![solid(extent, [0.5, 0.5, 0.5, 1.0])], layers)
    };
    assert_eq!(
        run(&backend, &clip_plan(false), &approx, &sources, &mut sink),
        Err(BackendError::Unavailable("no_adapter"))
    );
    // ...at class Approximate (pigment's rule is unverified), so a Fitted floor refuses it...
    assert!(matches!(
        run(
            &backend,
            &clip_plan(false),
            &request(5, Exactness::Fitted),
            &sources,
            &mut sink
        ),
        Err(BackendError::Unsupported {
            operator: "clip_to_base",
            exactness: Exactness::Approximate
        })
    ));
    // ...and a clipped layer with no base is pigment's structural refusal
    assert!(matches!(
        run(&backend, &clip_plan(true), &approx, &sources, &mut sink),
        Err(BackendError::InvalidPlan(PlanError::ClipWithoutBase { .. }))
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
    let bad_sources = Sources::with_tiles(vec![(1, bad)]);
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
        Exactness::Fitted
    );
    // backend class only (pigment's intrinsic class is applied by admit): Fitted for every
    // generated mode, Unsupported for the refused ones
    for facts in &MODES {
        let want = if REFUSED_MODES.contains(&facts.mode) {
            Exactness::Unsupported
        } else {
            Exactness::Fitted
        };
        assert_eq!(
            backend.exactness(Operator::Blend(facts.mode)),
            want,
            "{}",
            facts.key
        );
    }
    assert_eq!(backend.exactness(Operator::IsolatedGroup), Exactness::Exact);
    assert_eq!(
        backend.exactness(Operator::PassThroughGroup),
        Exactness::Exact
    );
    assert_eq!(
        backend.exactness(Operator::Mask(MaskFormat::CoverageU8)),
        Exactness::Exact
    );
    assert_eq!(
        backend.exactness(Operator::Mask(MaskFormat::BaseAlpha)),
        Exactness::Approximate
    );
    assert!(backend.supports_clip_to_base());
    assert_eq!(
        backend.exactness(Operator::NonIsolatedBlendedGroup),
        Exactness::Unsupported
    );
    // lane SMB-PS-ORACLE-PARITY-2: the pass-through mix (pigment `pass_through_mix`)
    assert_eq!(
        backend.exactness(Operator::PassThroughOpacity),
        Exactness::Approximate
    );
}

/// R3 finding 1 (regression): a leaf root has no layer, so the plan lists no blend operator, but
/// the executor composites it through the blend kernel (Normal, opacity 1, over transparent). The
/// class must be that kernel's (Fitted, the crate's GPU blend policy): an Exact floor is refused at
/// admission, before any source lookup or device work, and the receipt carries `normal: fitted`.
#[test]
fn leaf_root_reports_the_blend_kernel_class() {
    let backend = no_adapter();
    let extent = rect(0, 0, 3, 2);
    let none = Sources::default();
    let mut sink = Image::new(extent);
    let leaf = |node: PlanNode<NoVector>| RenderPlan {
        revision: 2,
        target: target(extent),
        nodes: vec![node],
        root: PlanNodeId(0),
    };
    let solid_root = leaf(solid(extent, [0.3, 0.5, 0.7, 0.25]));
    let tile_root = leaf(OwnedTile::random(3, 2, 5).input(1, extent));
    let refused = Err(BackendError::Unsupported {
        operator: StudioBlendMode::Normal.key(),
        exactness: Exactness::Fitted,
    });
    for plan in [&solid_root, &tile_root] {
        assert_eq!(
            run(
                &backend,
                plan,
                &request(2, Exactness::Exact),
                &none,
                &mut sink
            ),
            refused
        );
    }
    // a Fitted floor admits it; the only refusal left on this backend is the missing device
    assert_eq!(
        run(
            &backend,
            &solid_root,
            &request(2, Exactness::Fitted),
            &none,
            &mut sink
        ),
        Err(BackendError::Unavailable("no_adapter"))
    );
    assert_eq!((sink.accepted, backend.resident_bytes()), (0, 0));

    // live device: the receipt names the kernel class, and the leaf texels come back
    let _guard = device_lock();
    let shared = shared_backend();
    if gpu_gate(shared, "leaf-root receipt class")
        .unwrap_or_else(|e| panic!("{e}"))
        .is_none()
    {
        return;
    }
    let receipt = run(
        shared,
        &solid_root,
        &request(2, Exactness::Fitted),
        &none,
        &mut sink,
    )
    .expect("leaf root executes on the device");
    assert_eq!(receipt.exactness.lowest, Exactness::Fitted);
    assert!(
        receipt
            .exactness
            .operators
            .iter()
            .any(|o| o.key == StudioBlendMode::Normal.key() && o.exactness == Exactness::Fitted)
    );
    for got in &sink.px {
        let err = got
            .iter()
            .zip([0.3_f32, 0.5, 0.7, 0.25])
            .map(|(g, e)| (g - e).abs())
            .fold(0.0_f32, f32::max);
        assert!(err <= 1.0e-6, "leaf texel {got:?}");
    }
    eprintln!(
        "LEAF_ROOT: receipt lowest {:?}, operators {:?}",
        receipt.exactness.lowest, receipt.exactness.operators
    );
}

/// Unit test of the loss LATCH and the unavailable path (no hardware, no real loss): a backend
/// whose state is injected as lost. The real destroy-and-detect proof is
/// `device_destroy_is_detected_by_the_lost_callback`.
#[cfg(feature = "test-faults")]
#[test]
fn injected_loss_latch_and_unavailable_are_explicit() {
    let token = CancellationToken::default();
    // the no-device port never admits (lane SMB-GPU-CLIENT-PORT-1: the old BackendChoice::None)
    let none = UnavailablePort::new();
    match none.admit(&AdmissionRequest::asset_local(), &token) {
        Err(refusal) => assert_eq!(GpuError::from(refusal), GpuError::Unavailable(Reason::NoAdapter)),
        Ok(_) => panic!("UnavailablePort must not admit a device"),
    }
    assert_eq!(
        no_adapter().reacquire(&token),
        Err(GpuError::Unavailable(Reason::NoAdapter))
    );

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
    let lost = GpuBackend::with_injected_loss(DeviceOptions::default(), 7);
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
    // a refused rebind (here canceled, hardware-independent) keeps the loss: stale generations
    // stay refused
    let canceled = CancellationToken::default();
    canceled.cancel();
    assert_eq!(
        lost.reacquire(&canceled),
        Err(GpuError::Backend(BackendError::Canceled))
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
    assert!(lost.is_lost());
    assert_eq!(lost.generation(), 7);
    assert_eq!((sink.accepted, lost.resident_bytes()), (0, 0));
}

/// R3 finding 3 (regression): `wgpu::Device::destroy` on a live device must be detected by the
/// backend ITSELF, through wgpu's device-lost callback (delivered by the poll the backend runs
/// before every execute and state query). `simulate_device_loss` only destroys and never sets the
/// latch, so removing the callback wiring in device.rs makes this test fail. A driver-initiated
/// loss (TDR, adapter removal) is a different trigger and stays NOT_PROVEN.
#[cfg(feature = "test-faults")]
#[test]
fn device_destroy_is_detected_by_the_lost_callback() {
    let _guard = device_lock();
    // Own backend: the shared GPU-test backend must stay alive.
    let live = env_backend(DeviceOptions::default());
    let Some(info) = gpu_gate(&live, "device-loss detection").unwrap_or_else(|e| panic!("{e}"))
    else {
        return;
    };
    let token = CancellationToken::default();
    let extent = rect(0, 0, 2, 2);
    let plan = plan_of(
        1,
        extent,
        vec![solid(extent, [0.1, 0.2, 0.3, 1.0])],
        vec![layer(0, StudioBlendMode::Normal, 1.0, 1.0)],
    );
    let sources = Sources::default();
    let mut sink = Image::new(extent);
    let first = info.generation;
    assert_eq!(first, 1);
    run(
        &live,
        &plan,
        &request(1, Exactness::Fitted),
        &sources,
        &mut sink,
    )
    .expect("the live device executes before the loss");
    assert!(!live.is_lost());

    // destroy only; the very next execute must find the loss on its own (unlatched by the test)
    let started = Instant::now();
    assert!(live.simulate_device_loss());
    let destroyed = Instant::now();
    let destroy_took = destroyed - started;
    assert_eq!(
        run(
            &live,
            &plan,
            &request(1, Exactness::Fitted),
            &sources,
            &mut sink
        ),
        Err(BackendError::DeviceLost { generation: first })
    );
    eprintln!(
        "DEVICE_LOSS: {} destroy {destroy_took:?}, detected by the wgpu lost callback on the next \
         execute in {:?}",
        info.text(),
        destroyed.elapsed()
    );
    assert!(live.is_lost());
    assert!(live.device_info().is_none());
    assert!(!live.simulate_device_loss(), "no live device is left");
    let second = live.reacquire(&token).expect("re-acquire after loss");
    assert!(second > first);
    let receipt = run(
        &live,
        &plan,
        &request(1, Exactness::Fitted),
        &sources,
        &mut sink,
    )
    .expect("execute on the successor device");
    assert_eq!(receipt.backend.generation, second);
    assert_eq!(live.resident_bytes(), 0);
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

/// R3 finding 4 (regression): byte accounting near `u64::MAX` never overflows. `used + bytes`
/// beyond the budget evicts (or is the typed `BudgetExceeded`), it never wraps or panics.
#[test]
fn residency_accounting_never_overflows() {
    let tile = |source: u32| ResidentKey::Tile {
        source,
        bounds: rect(0, 0, 4, 4),
    };
    let half = 1_u64 << 63;
    let exceeded = |requested| GpuError::BudgetExceeded {
        requested,
        budget: u64::MAX,
    };
    let mut res = Residency::new(u64::MAX);
    let a = res.reserve(tile(1), half).expect("first half");
    // pinned: the second half cannot fit next to it (u64::MAX - 2^63 < 2^63)
    assert_eq!(res.reserve(tile(2), half), Err(exceeded(half)));
    assert_eq!((res.used(), res.evictions()), (half, 0));
    // unpinned: used + bytes = 2^64 would overflow; the LRU entry is evicted instead
    assert!(res.unpin(a.id));
    let b = res.reserve(tile(2), half).expect("evicts the first half");
    assert_eq!(b.evicted, vec![tile(1)]);
    assert_eq!(
        (res.used(), res.peak_used(), res.evictions()),
        (half, half, 1)
    );
    // exactly u64::MAX fits; one more byte is the typed refusal, state intact
    res.reserve(ResidentKey::Scratch("rest"), half - 1)
        .expect("fills to u64::MAX");
    assert_eq!(res.used(), u64::MAX);
    assert_eq!(
        res.reserve(ResidentKey::Scratch("one"), 1),
        Err(exceeded(1))
    );
    assert_eq!((res.used(), res.len()), (u64::MAX, 2));
    // releasing everything returns to zero
    assert!(res.release(b.id));
    assert_eq!(res.used(), half - 1);
}

/// R3 finding 2 (regression): a shader/pipeline validation failure on a machine WITH an adapter
/// is the typed `GpuError::Pipeline`, distinct from `Unavailable`, and the GPU-proof gate fails on
/// it instead of skipping (it used to become `Unavailable(NoDevice)`, so every GPU test passed via
/// its NOT_PROVEN branch).
#[cfg(feature = "test-faults")]
#[test]
fn pipeline_failure_is_typed_and_fails_the_gpu_gate() {
    const BROKEN_WGSL: &str = "@compute @workgroup_size(8, 8, 1)\nfn composite() {\n    let x: f32 = no_such_symbol;\n}\n";
    let _guard = device_lock();
    let broken = GpuBackend::with_shader_source(env_options(), BROKEN_WGSL);
    if broken.unavailable_reason() == Some(Reason::NoAdapter) {
        // No adapter at all: acquisition stops before any shader exists. The gate decides
        // (NOT_PROVEN here, a failure while tests/gpu-proof.toml says require_gpu = true).
        let gate = gpu_gate(&broken, "pipeline-failure typing").unwrap_or_else(|e| panic!("{e}"));
        assert!(gate.is_none());
        return;
    }
    assert_eq!(
        broken.failure(),
        Some(GpuError::Pipeline("shader_validation")),
        "unavailable {:?}",
        broken.unavailable_reason()
    );
    assert_eq!(broken.unavailable_reason(), None);
    assert!(broken.device_info().is_none() && !broken.is_lost());
    // the GPU-proof gate refuses to skip: a failure, never NOT_PROVEN
    let gate = gpu_gate(&broken, "pipeline-failure probe");
    assert!(gate.is_err(), "gate must fail, got {gate:?}");
    eprintln!(
        "PIPELINE_FAILURE: backend {} with an adapter: {:?}; gate refuses to skip",
        backend_key_from_env(),
        broken.failure()
    );
    // every execute reports the typed failure; at the port its code is never no_adapter/no_device
    let extent = rect(0, 0, 2, 2);
    let plan = plan_of(
        1,
        extent,
        vec![solid(extent, [0.1, 0.2, 0.3, 1.0])],
        vec![layer(0, StudioBlendMode::Normal, 1.0, 1.0)],
    );
    let none = Sources::default();
    let mut sink = Image::new(extent);
    let detailed = broken.execute_detailed(
        &plan,
        &request(1, Exactness::Fitted),
        &none,
        &CancellationToken::default(),
        &mut sink,
    );
    assert_eq!(
        detailed.map(|_| ()),
        Err(GpuError::Pipeline("shader_validation"))
    );
    assert_eq!(
        run(
            &broken,
            &plan,
            &request(1, Exactness::Fitted),
            &none,
            &mut sink
        ),
        Err(BackendError::Unavailable("shader_validation"))
    );
    // re-acquire builds the same shader and stays the typed failure
    assert_eq!(
        broken.reacquire(&CancellationToken::default()),
        Err(GpuError::Pipeline("shader_validation"))
    );
    assert_eq!(
        broken.failure(),
        Some(GpuError::Pipeline("shader_validation"))
    );
    assert_eq!(sink.accepted, 0);
}

#[test]
fn no_wgpu_types_in_public_api() {
    // STU-ARC-002: wgpu is a private leaf. Every `pub` declaration line in the crate sources and
    // the descriptor must be free of wgpu paths (private/`pub(crate)` items may use them).
    let sources = [
        ("lib.rs", include_str!("../src/lib.rs")),
        ("client.rs", include_str!("../src/client.rs")),
        ("client/transient.rs", include_str!("../src/client/transient.rs")),
        ("client/external.rs", include_str!("../src/client/external.rs")),
        ("device.rs", include_str!("../src/device.rs")),
        ("ext.rs", include_str!("../src/ext.rs")),
        ("kernel.rs", include_str!("../src/kernel.rs")),
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
    let backend = shared_backend();
    let sources0 = Sources::default();
    let tiny = rect(0, 0, 1, 1);
    let tiny_plan = plan_of(
        1,
        tiny,
        vec![solid(tiny, [0.2, 0.4, 0.6, 1.0])],
        vec![layer(0, StudioBlendMode::Normal, 1.0, 1.0)],
    );
    let Some(info) = gpu_gate(backend, "GPU execution parity").unwrap_or_else(|e| panic!("{e}"))
    else {
        // No adapter at all: typed, explicit, and not a pass for the GPU claim (NOT_PROVEN).
        let mut sink = Image::new(tiny);
        let refused = run(
            backend,
            &tiny_plan,
            &request(1, Exactness::Fitted),
            &sources0,
            &mut sink,
        );
        assert_eq!(refused, Err(BackendError::Unavailable("no_adapter")));
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
            backend,
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
    let sources = Sources::with_tiles(vec![(1, t0), (2, t1)]);
    let mut sink = Image::new(extent);
    let receipt = run(
        backend,
        &plan,
        &request(9, Exactness::Approximate),
        &sources,
        &mut sink,
    )
    .expect("randomised plan executes on the device");
    let tile_fn = |idx: u32, p: Rect| -> SourceFn {
        let src = sources
            .tiles
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
    // dst + staging + upload chunk buffers, 80-byte params (the fill split of lane
    // SMB-PS-ORACLE-PARITY-1), minimal 4-byte mask binding
    assert_eq!(receipt.peak_working_bytes, 3 * 16 * 16 * 16 + 80 + 4);
    assert_eq!(backend.resident_bytes(), 0);

    // (c) sink rejection mid-read-back and an impossible budget both release residency and
    // leave the backend usable; neither produces a receipt.
    let mut reject = Image::new(extent);
    reject.reject_at = Some(2);
    assert_eq!(
        run(
            backend,
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
        run(backend, &plan, &starved, &sources, &mut none),
        Err(BackendError::BudgetExceeded { limit: 1000, .. })
    ));
    assert_eq!((none.accepted, backend.resident_bytes()), (0, 0));
    let mut again = Image::new(extent);
    run(
        backend,
        &plan,
        &request(9, Exactness::Approximate),
        &sources,
        &mut again,
    )
    .expect("backend still usable after refusals");
    assert_eq!(again.px, sink.px);
}

// ---- slice 2: independent CPU reference of the plan semantics (pigment blend_math only) -------

fn inside(r: Rect, x: i64, y: i64) -> bool {
    x >= r.x && y >= r.y && x < r.x + i64::from(r.width) && y < r.y + i64::from(r.height)
}
/// Source-over with blend into `dst` using the executor's skip rules.
fn over(dst: &mut [f32; 4], mode: StudioBlendMode, s: [f32; 4], opacity: f32) {
    if opacity <= 0.0 || s[3] <= 0.0 {
        return;
    }
    let out = composite_over(
        mode,
        Rgba::new([dst[0], dst[1], dst[2]], dst[3]),
        Rgba::new([s[0], s[1], s[2]], s[3]),
        opacity,
        PixelSite::default(),
    )
    .expect("reference blend");
    *dst = [out.rgb[0], out.rgb[1], out.rgb[2], out.a.min(1.0)];
}

/// Blurred stored coverage of `plane` at (x, y): the naive double sum over pigment's feather taps
/// with replicate clamping at the plane border (independent of the backend's two-pass blur).
fn blurred_raw(plane: &OwnedMask, radius_px: f32, x: i64, y: i64) -> f32 {
    let taps = feather_kernel(radius_px).expect("feather taps");
    let half = (taps.len() as i64 - 1) / 2;
    let b = plane.bounds;
    let mut acc = 0.0_f64;
    for (j, tj) in taps.iter().enumerate() {
        let yy = (y + j as i64 - half).clamp(b.y, b.y + i64::from(b.height) - 1);
        for (i, ti) in taps.iter().enumerate() {
            let xx = (x + i as i64 - half).clamp(b.x, b.x + i64::from(b.width) - 1);
            acc += f64::from(*tj) * f64::from(*ti) * f64::from(plane.coverage(xx, yy, false));
        }
    }
    acc as f32
}

/// Per-pixel evaluation of a plan: layers bottom-first, masks, opacity * fill, isolated groups
/// (own transparent result), pass-through groups (onto the running backdrop) and pigment's
/// documented clip-to-base rule (`MaskFormat::BaseAlpha`). With an extension envelope it also
/// evaluates, from pigment's scalar functions only, adjustment layers
/// (`composite_adjustment`), Blend-If (`blend_if_weight`), mask density / invert (`mask_coverage`)
/// and mask feather (naive blur over `feather_kernel`).
struct Reference<'a> {
    plan: &'a RenderPlan<NoVector>,
    sources: &'a Sources,
    ext: Option<&'a PlanExtensions>,
}
impl Reference<'_> {
    fn layer_ext(&self, group: usize, layer: usize) -> Option<&LayerExtension> {
        self.ext.and_then(|e| {
            e.extension(
                PlanNodeId(u32::try_from(group).ok()?),
                u32::try_from(layer).ok()?,
            )
        })
    }
    /// Effective coverage of a layer mask: plain (inversion only) outside the envelope; with it,
    /// stored coverage -> optional feather -> `mask_coverage(invert, density)`.
    fn mask_with(&self, m: &MaskInput, lx: Option<&LayerExtension>, x: i64, y: i64) -> f32 {
        let (_, plane) = self
            .sources
            .masks
            .iter()
            .find(|(k, _)| *k == m.key.0)
            .expect("mask plane");
        if self.ext.is_none() {
            return plane.coverage(x, y, m.inverted);
        }
        let feather = lx.map_or(0.0, |e| e.mask_feather_px);
        let raw = if feather > 0.0 {
            blurred_raw(plane, feather, x, y)
        } else {
            plane.coverage(x, y, false)
        };
        mask_coverage(raw, m.inverted, m.density).expect("mask coverage")
    }
    /// Blend-If weight of a layer at (source colour, backdrop colour and alpha); 1 without entries
    /// (pigment `blend_if_weight_over`: an Underlying range weakens over a translucent backdrop,
    /// lane SMB-PS-ORACLE-PARITY-1).
    fn weight(lx: Option<&LayerExtension>, source: [f32; 3], backdrop: [f32; 3], backdrop_a: f32) -> f32 {
        lx.map_or(1.0, |e| {
            blend_if_weight_over(&e.blend_if, source, backdrop, backdrop_a).expect("blend-if weight")
        })
    }
    fn leaf(&self, idx: usize, x: i64, y: i64) -> Option<[f32; 4]> {
        match &self.plan.nodes[idx] {
            PlanNode::Solid { rect, rgba } => inside(*rect, x, y).then_some(*rgba),
            PlanNode::Tile(t) => {
                if !inside(t.placement, x, y) {
                    return None;
                }
                let (_, tile) = self.sources.tiles.iter().find(|(k, _)| *k == t.key.0)?;
                Some(tile.pixel((x - t.placement.x) as u32, (y - t.placement.y) as u32))
            }
            _ => None,
        }
    }
    /// A layer's source texel: a leaf pixel, or the result of an isolated child group.
    fn source(&self, l: &Layer, x: i64, y: i64) -> Option<[f32; 4]> {
        let idx = l.input.0 as usize;
        match &self.plan.nodes[idx] {
            PlanNode::Group(_) => {
                let mut sub = [0.0; 4];
                self.group(idx, x, y, &mut sub);
                Some(sub)
            }
            _ => self.leaf(idx, x, y),
        }
    }
    fn group(&self, idx: usize, x: i64, y: i64, dst: &mut [f32; 4]) {
        let PlanNode::Group(g) = &self.plan.nodes[idx] else {
            panic!("not a group");
        };
        let clipped = |l: &Layer| l.mask.as_ref().is_some_and(MaskInput::is_clip_to_base);
        let mut i = 0;
        while i < g.layers.len() {
            let mut j = i + 1;
            while j < g.layers.len() && clipped(&g.layers[j]) {
                j += 1;
            }
            if j == i + 1 {
                self.layer(idx, i, &g.layers[i], x, y, dst);
            } else {
                self.run(idx, i, &g.layers[i..j], x, y, dst);
            }
            i = j;
        }
    }
    /// An adjustment layer (envelope) over `backdrop`: `f(Cb)` blended back at
    /// `k = opacity * fill * mask * blend_if`, alpha unchanged; only inside its placeholder rect.
    #[allow(clippy::too_many_arguments)]
    fn adjust(
        &self,
        l: &Layer,
        lx: &LayerExtension,
        op: &AdjustmentOp,
        mask: f32,
        x: i64,
        y: i64,
        backdrop: [f32; 4],
    ) -> [f32; 4] {
        if self.leaf(l.input.0 as usize, x, y).is_none() {
            return backdrop;
        }
        let prepared = op.prepare().expect("prepared adjustment");
        let cb = [backdrop[0], backdrop[1], backdrop[2]];
        let adjusted = if backdrop[3] > 0.0 {
            prepared.apply(cb).expect("adjusted colour")
        } else {
            cb
        };
        let w = Self::weight(Some(lx), adjusted, cb, backdrop[3]);
        let k = l.opacity * l.fill_opacity * mask * w;
        let out = composite_adjustment(l.mode, Rgba::new(cb, backdrop[3]), &prepared, k)
            .expect("reference adjustment");
        [out.rgb[0], out.rgb[1], out.rgb[2], out.a]
    }
    fn layer(&self, gi: usize, li: usize, l: &Layer, x: i64, y: i64, dst: &mut [f32; 4]) {
        let lx = self.layer_ext(gi, li);
        if let (Some(e), Some(op)) = (lx, lx.and_then(|e| e.adjustment.as_ref())) {
            let m = l.mask.as_ref().map_or(1.0, |m| self.mask_with(m, lx, x, y));
            *dst = self.adjust(l, e, op, m, x, y, *dst);
            return;
        }
        if let PlanNode::Group(c) = &self.plan.nodes[l.input.0 as usize]
            && !c.isolated
        {
            self.group(l.input.0 as usize, x, y, dst);
            return;
        }
        let Some(s) = self.source(l, x, y) else {
            return;
        };
        let m = l.mask.as_ref().map_or(1.0, |m| self.mask_with(m, lx, x, y));
        let w = Self::weight(lx, [s[0], s[1], s[2]], [dst[0], dst[1], dst[2]], dst[3]);
        over(dst, l.mode, s, l.opacity * l.fill_opacity * m * w);
    }
    /// `run = [base, clipped...]`: G = base content with the base's mask; each clipped layer is
    /// composited onto G with its source alpha times the base alpha and G's alpha held at the base
    /// alpha; G goes onto `dst` with the BASE layer's mode, opacity and fill.
    fn run(&self, gi: usize, first: usize, run: &[Layer], x: i64, y: i64, dst: &mut [f32; 4]) {
        let base = &run[0];
        let base_ext = self.layer_ext(gi, first);
        let mut g = [0.0_f32; 4];
        if let Some(s) = self.source(base, x, y) {
            let m = base
                .mask
                .as_ref()
                .map_or(1.0, |m| self.mask_with(m, base_ext, x, y));
            let a = s[3] * m;
            if a > 0.0 {
                g = [s[0], s[1], s[2], a];
            }
        }
        let a_base = g[3];
        for (n, c) in run.iter().enumerate().skip(1) {
            let cx = self.layer_ext(gi, first + n);
            if let (Some(e), Some(op)) = (cx, cx.and_then(|e| e.adjustment.as_ref())) {
                // a clipped adjustment acts on the run unit G (mask ignored, like every clipped layer)
                if c.opacity * c.fill_opacity > 0.0 {
                    g = self.adjust(c, e, op, 1.0, x, y, g);
                }
                continue;
            }
            let Some(s) = self.source(c, x, y) else {
                continue;
            };
            let w = Self::weight(cx, [s[0], s[1], s[2]], [g[0], g[1], g[2]], g[3]);
            let opacity = c.opacity * c.fill_opacity * w;
            let clipped_alpha = s[3] * a_base;
            if opacity <= 0.0 || clipped_alpha <= 0.0 {
                continue;
            }
            let out = composite_over(
                c.mode,
                Rgba::new([g[0], g[1], g[2]], g[3]),
                Rgba::new([s[0], s[1], s[2]], clipped_alpha),
                opacity,
                PixelSite::default(),
            )
            .expect("reference clip blend");
            g = [out.rgb[0], out.rgb[1], out.rgb[2], a_base];
        }
        let w = Self::weight(base_ext, [g[0], g[1], g[2]], [dst[0], dst[1], dst[2]], dst[3]);
        over(dst, base.mode, g, base.opacity * base.fill_opacity * w);
    }
}

fn layer_records(plan: &RenderPlan<NoVector>) -> usize {
    plan.nodes
        .iter()
        .map(|n| match n {
            PlanNode::Group(g) => g.layers.len(),
            _ => 0,
        })
        .sum()
}

/// Execute `plan` on the device and return (max abs error vs the reference, worst pixel, receipt).
fn gpu_vs_reference(
    backend: &GpuBackend,
    plan: &RenderPlan<NoVector>,
    sources: &Sources,
    floor: Exactness,
) -> (
    f32,
    (i64, i64),
    hsk_studio_pigment::render_plan::RenderReceipt,
) {
    gpu_vs_reference_with(backend, plan, None, sources, floor)
}

/// [`gpu_vs_reference`] through the envelope path (`execute_extended`) when `ext` is given.
fn gpu_vs_reference_with(
    backend: &GpuBackend,
    plan: &RenderPlan<NoVector>,
    ext: Option<&PlanExtensions>,
    sources: &Sources,
    floor: Exactness,
) -> (
    f32,
    (i64, i64),
    hsk_studio_pigment::render_plan::RenderReceipt,
) {
    let (err, at, receipt, _) = gpu_vs_reference_ties(backend, plan, ext, sources, floor, &|_, _| false);
    (err, at, receipt)
}

/// `gpu_vs_reference_with` where `tie(x, y)` marks a threshold boundary pixel (lead disposition
/// 2026-10-31, lane SMB-GPU-CLIENT-PORT-1): its CPU luminance is within 2^-20 of the threshold cut,
/// where binary32 (GPU) and pigment's evaluation may legitimately land on opposite sides of an
/// exact tie. Such a pixel must still be a threshold output (every colour channel exactly 0 or
/// exactly 1, alpha equal to the reference), is excluded from the error and is counted; every
/// other pixel is compared exactly as before. Returns the boundary count last.
fn gpu_vs_reference_ties(
    backend: &GpuBackend,
    plan: &RenderPlan<NoVector>,
    ext: Option<&PlanExtensions>,
    sources: &Sources,
    floor: Exactness,
    tie: &dyn Fn(i64, i64) -> bool,
) -> (
    f32,
    (i64, i64),
    hsk_studio_pigment::render_plan::RenderReceipt,
    usize,
) {
    let extent = plan.target.extent;
    let mut sink = Image::new(extent);
    let req = request(plan.revision, floor);
    let receipt = match ext {
        None => run(backend, plan, &req, sources, &mut sink),
        Some(e) => backend.execute_extended(
            plan,
            e,
            &req,
            sources,
            &CancellationToken::default(),
            &mut sink,
        ),
    }
    .expect("plan executes on the device");
    let reference = Reference { plan, sources, ext };
    let (mut max_err, mut worst) = (0.0_f32, (0, 0));
    let mut worst_values = ([0.0_f32; 4], [0.0_f32; 4]);
    let mut ties = 0_usize;
    for y in 0..i64::from(extent.height) {
        for x in 0..i64::from(extent.width) {
            let mut want = [0.0_f32; 4];
            reference.group(plan.root.0 as usize, extent.x + x, extent.y + y, &mut want);
            let got = sink.px[(y * i64::from(extent.width) + x) as usize];
            if tie(x, y) {
                assert!(
                    got[..3].iter().all(|&v| v == 0.0 || v == 1.0) && got[3] == want[3],
                    "boundary pixel ({x},{y}) is not a threshold output: got {got:?} want {want:?}"
                );
                ties += 1;
                continue;
            }
            for c in 0..4 {
                let e = (got[c] - want[c]).abs();
                assert!(e.is_finite(), "non-finite output at ({x},{y})");
                if e > max_err {
                    max_err = e;
                    worst = (x, y);
                    worst_values = (got, want);
                }
            }
        }
    }
    if max_err > 1.0e-5 {
        eprintln!(
            "WORST_PIXEL {worst:?}: got {:?} want {:?}",
            worst_values.0, worst_values.1
        );
    }
    (max_err, worst, receipt, ties)
}

/// Threshold boundary window around the cut (lead disposition 2026-10-31): 2^-20.
const THRESHOLD_TIE_WINDOW: f64 = 1.0 / 1_048_576.0;
/// The CPU cut of pigment's threshold for `level` (adjust_math `level - THRESHOLD_HALF_CODE`).
fn threshold_cut(level: f32) -> f64 {
    f64::from(level - 0.5 / 255.0)
}
/// Whether straight `rgb` is a threshold boundary pixel for `level`: pigment's luminance within
/// 2^-20 of the cut.
fn threshold_tie(rgb: [f32; 3], level: f32) -> bool {
    (f64::from(hsk_studio_pigment::blend_math::lum(rgb)) - threshold_cut(level)).abs() <= THRESHOLD_TIE_WINDOW
}

#[test]
fn gpu_modes_masks_groups_clip_parity() {
    const TOL_PER_LAYER: f32 = 1.0e-6;
    let _guard = device_lock();
    let backend = shared_backend();
    let Some(info) = gpu_gate(backend, "slice-2 GPU parity").unwrap_or_else(|e| panic!("{e}"))
    else {
        let extent = rect(0, 0, 2, 2);
        let plan = plan_of(
            1,
            extent,
            vec![solid(extent, [0.5, 0.5, 0.5, 1.0])],
            vec![
                layer(0, StudioBlendMode::Normal, 1.0, 1.0),
                Layer {
                    mask: Some(MaskInput::clip_to_base()),
                    ..layer(0, StudioBlendMode::Multiply, 0.5, 1.0)
                },
            ],
        );
        let mut sink = Image::new(extent);
        assert!(matches!(
            run(
                backend,
                &plan,
                &request(1, Exactness::Approximate),
                &Sources::default(),
                &mut sink
            ),
            Err(BackendError::Unavailable("no_adapter"))
        ));
        assert_eq!(sink.accepted, 0);
        return;
    };
    eprintln!("GPU_BACKEND: {} (slice 2)", info.text());

    // (A) every mode the supported list (derived from pigment's facts) contains, against
    // blend_math over random tiles with exact 0/1 samples and varying backdrop alpha.
    let extent = rect(1, 2, 21, 13);
    let backdrop = OwnedTile::random(21, 13, 7);
    let (mut tested, mut worst_mode) = (0, 0.0_f32);
    for facts in &MODES {
        let mode = facts.mode;
        if !gpu_blend_supported(mode) {
            continue;
        }
        let source = OwnedTile::random(21, 13, 100 + u32::from(mode.discriminant()));
        let plan = plan_of(
            3,
            extent,
            vec![backdrop.input(1, extent), source.input(2, extent)],
            vec![
                layer(0, StudioBlendMode::Normal, 1.0, 1.0),
                layer(1, mode, 0.85, 1.0),
            ],
        );
        let sources = Sources::with_tiles(vec![(1, backdrop.clone()), (2, source)]);
        // pigment's class travels (the backend only lowers it): Fitted for an Exact mode,
        // Approximate for a community-formula mode
        let class = facts.exactness.lowest_of(Exactness::Fitted);
        let (err, at, receipt) = gpu_vs_reference(backend, &plan, &sources, class);
        eprintln!(
            "MODE {:<17} class {} max_err {err:e} at {at:?}",
            facts.key,
            class.key()
        );
        if err > 1.0e-5 {
            let (px, py) = (at.0 as u32, at.1 as u32);
            eprintln!(
                "  inputs at {at:?}: backdrop {:?} source {:?}",
                backdrop.pixel(px, py),
                sources.tiles[1].1.pixel(px, py)
            );
        }
        assert!(
            err <= 2.0 * TOL_PER_LAYER,
            "{} error {err:e} > {:e}",
            facts.key,
            2.0 * TOL_PER_LAYER
        );
        assert_eq!(receipt.exactness.lowest, class, "{}", facts.key);
        assert!(
            receipt
                .exactness
                .operators
                .iter()
                .any(|o| o.key == facts.key && o.exactness == class),
            "{}: ledger does not carry class {}",
            facts.key,
            class.key()
        );
        worst_mode = worst_mode.max(err);
        tested += 1;
    }
    assert_eq!(
        tested,
        MODES.len() - REFUSED_MODES.len(),
        "every non-refused pigment mode must have been executed"
    );
    eprintln!("MODES tested {tested}, worst max_err {worst_mode:e}");

    // (B) coverage masks: u8 plane, u16 plane inverted, fill opacity, 2 x 2 chunks.
    let extent = rect(0, 0, 19, 17);
    let (t0, t1) = (OwnedTile::random(19, 17, 31), OwnedTile::random(10, 9, 32));
    let m1 = OwnedMask::random(rect(0, 0, 19, 17), MaskFormat::CoverageU8, 5);
    let m2 = OwnedMask::random(rect(3, 2, 16, 15), MaskFormat::CoverageU16Le, 6);
    let plan = plan_of(
        4,
        extent,
        vec![
            t0.input(1, extent),
            solid(rect(2, 3, 14, 12), [0.35, 0.8, 0.15, 0.9]),
            t1.input(2, rect(5, 4, 10, 9)),
        ],
        vec![
            layer(0, StudioBlendMode::Normal, 1.0, 1.0),
            Layer {
                mask: Some(m1.input(11, false)),
                ..layer(1, StudioBlendMode::Multiply, 0.9, 0.8)
            },
            Layer {
                mask: Some(m2.input(12, true)),
                ..layer(2, StudioBlendMode::Screen, 1.0, 1.0)
            },
        ],
    );
    let sources = Sources {
        tiles: vec![(1, t0), (2, t1)],
        masks: vec![(11, m1), (12, m2)],
    };
    let (err, at, receipt) = gpu_vs_reference(backend, &plan, &sources, Exactness::Approximate);
    let tolerance = TOL_PER_LAYER * layer_records(&plan) as f32;
    eprintln!("MASKS max_err {err:e} at {at:?} (tolerance {tolerance:e})");
    assert!(err <= tolerance, "mask parity {err:e} > {tolerance:e}");
    assert_eq!(receipt.chunks, 4);
    assert_eq!(backend.resident_bytes(), 0);

    // (C) isolated groups (one nested two deep), a pass-through group, a masked group layer.
    let extent = rect(0, 0, 30, 22);
    let t_a = OwnedTile::random(30, 22, 41);
    let t_b = OwnedTile::random(12, 12, 42);
    let t_c = OwnedTile::random(14, 10, 43);
    let m3 = OwnedMask::random(rect(0, 0, 30, 22), MaskFormat::CoverageU8, 7);
    let plan = plan_of(
        5,
        extent,
        vec![
            t_a.input(1, extent),                            // 0
            solid(rect(4, 2, 20, 16), [0.7, 0.2, 0.4, 0.8]), // 1
            t_b.input(2, rect(10, 5, 12, 12)),               // 2
            solid(rect(8, 6, 12, 12), [0.3, 0.9, 0.6, 0.7]), // 3
            t_c.input(3, rect(2, 8, 14, 10)),                // 4
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![
                    layer(2, StudioBlendMode::Normal, 1.0, 1.0),
                    layer(3, StudioBlendMode::Difference, 0.8, 1.0),
                ],
            }), // 5: G2
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![
                    layer(1, StudioBlendMode::Multiply, 0.9, 1.0),
                    layer(5, StudioBlendMode::Overlay, 0.6, 1.0),
                    layer(4, StudioBlendMode::Screen, 0.7, 1.0),
                ],
            }), // 6: G1 containing G2
            PlanNode::Group(Group {
                isolated: false,
                layers: vec![
                    layer(3, StudioBlendMode::Normal, 0.5, 1.0),
                    layer(2, StudioBlendMode::HardLight, 0.8, 1.0),
                ],
            }), // 7: pass-through P
            solid(rect(0, 0, 30, 22), [0.5, 0.4, 0.9, 0.6]), // 8
        ],
        vec![
            layer(0, StudioBlendMode::Normal, 1.0, 1.0),
            Layer {
                mask: Some(m3.input(13, false)),
                ..layer(6, StudioBlendMode::SoftLight, 0.7, 1.0)
            },
            layer(7, StudioBlendMode::PassThrough, 1.0, 1.0),
            layer(8, StudioBlendMode::Exclusion, 0.3, 1.0),
        ],
    );
    let sources = Sources {
        tiles: vec![(1, t_a), (2, t_b), (3, t_c)],
        masks: vec![(13, m3)],
    };
    let (err, at, receipt) = gpu_vs_reference(backend, &plan, &sources, Exactness::Fitted);
    let tolerance = TOL_PER_LAYER * layer_records(&plan) as f32;
    eprintln!("GROUPS max_err {err:e} at {at:?} (tolerance {tolerance:e})");
    assert!(err <= tolerance, "group parity {err:e} > {tolerance:e}");
    assert_eq!(receipt.exactness.lowest, Exactness::Fitted);
    let stats = backend.last_stats().expect("stats");
    assert_eq!(stats.levels, 3);
    assert_eq!(receipt.chunks, 4);
    // 3 destination levels + staging + upload + mask + params, 16 x 16 chunk buffers
    assert_eq!(
        receipt.peak_working_bytes,
        4 * 16 * 16 * 16 + 16 * 16 * 16 + 16 * 16 * 4 + 80
    );
    assert_eq!(backend.resident_bytes(), 0);

    // (D) clip-to-base: masked base, three clipped layers (solid beyond the base, tile with fill
    // opacity, isolated group), then an unclipped layer ends the run.
    let extent = rect(0, 0, 26, 20);
    let clip = Some(MaskInput::clip_to_base());
    let t_a = OwnedTile::random(26, 20, 51);
    let t_d = OwnedTile::random(20, 16, 52);
    let t_e = OwnedTile::random(18, 14, 53);
    let t_f = OwnedTile::random(10, 10, 54);
    let m4 = OwnedMask::random(rect(2, 1, 22, 18), MaskFormat::CoverageU16Le, 8);
    let plan = plan_of(
        6,
        extent,
        vec![
            t_a.input(1, extent),                            // 0
            t_d.input(2, rect(3, 2, 20, 16)),                // 1: base
            solid(rect(0, 0, 26, 20), [0.6, 0.3, 0.8, 0.9]), // 2: clipped, beyond the base
            t_e.input(3, rect(6, 4, 18, 14)),                // 3: clipped
            t_f.input(4, rect(8, 6, 10, 10)),                // 4
            solid(rect(9, 7, 12, 8), [0.2, 0.7, 0.5, 0.8]),  // 5
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![
                    layer(4, StudioBlendMode::Normal, 1.0, 1.0),
                    layer(5, StudioBlendMode::Difference, 0.7, 1.0),
                ],
            }), // 6: clipped isolated group
            solid(rect(0, 0, 26, 20), [0.4, 0.5, 0.1, 0.7]), // 7
        ],
        vec![
            layer(0, StudioBlendMode::Normal, 1.0, 1.0),
            Layer {
                mask: Some(m4.input(14, false)),
                ..layer(1, StudioBlendMode::SoftLight, 0.8, 0.9)
            },
            Layer {
                mask: clip,
                ..layer(2, StudioBlendMode::Multiply, 0.7, 1.0)
            },
            Layer {
                mask: clip,
                ..layer(3, StudioBlendMode::Screen, 0.9, 0.5)
            },
            Layer {
                mask: clip,
                ..layer(6, StudioBlendMode::Overlay, 0.6, 1.0)
            },
            layer(7, StudioBlendMode::Normal, 0.2, 1.0),
        ],
    );
    let sources = Sources {
        tiles: vec![(1, t_a), (2, t_d), (3, t_e), (4, t_f)],
        masks: vec![(14, m4)],
    };
    let (err, at, receipt) = gpu_vs_reference(backend, &plan, &sources, Exactness::Approximate);
    let tolerance = TOL_PER_LAYER * layer_records(&plan) as f32;
    eprintln!("CLIP max_err {err:e} at {at:?} (tolerance {tolerance:e})");
    assert!(err <= tolerance, "clip parity {err:e} > {tolerance:e}");
    assert_eq!(receipt.exactness.lowest, Exactness::Approximate);
    assert!(
        receipt
            .exactness
            .operators
            .iter()
            .any(|o| o.key == "clip_to_base")
    );
    assert_eq!(backend.last_stats().expect("stats").levels, 3);
    assert_eq!(backend.resident_bytes(), 0);

    // (E) hand-derived two-layer clip on a 2 x 1 target. Backdrop (0.5,0.5,0.5,1). Base (0.8,0.4,
    // 0.2) alpha 0.5 covers pixel 0 only; the clipped layer (0.2,0.6,1.0,1.0) Multiply at opacity
    // 0.5 covers both pixels. Pixel 0: source alpha 1.0 * base alpha 0.5 = 0.5, a_s = 0.25 over G
    // (a_b = 0.5): a_o = 0.625, B = (0.16,0.24,0.2), Cs' = 0.5*Cs + 0.5*B = (0.18,0.42,0.6),
    // Co = 0.25*Cs' + 0.75*0.5*Cb = (0.345,0.255,0.225), G = Co / 0.625 = (0.552,0.408,0.36) with
    // alpha held at 0.5. G over the backdrop (Normal, opacity 1): 0.5*G + 0.5*0.5 =
    // (0.526,0.454,0.43), alpha 1. Pixel 1 has no base: clipped content never extends beyond it,
    // so the backdrop is untouched.
    let extent = rect(0, 0, 2, 1);
    let plan = plan_of(
        7,
        extent,
        vec![
            solid(extent, [0.5, 0.5, 0.5, 1.0]),
            solid(rect(0, 0, 1, 1), [0.8, 0.4, 0.2, 0.5]),
            solid(extent, [0.2, 0.6, 1.0, 1.0]),
        ],
        vec![
            layer(0, StudioBlendMode::Normal, 1.0, 1.0),
            layer(1, StudioBlendMode::Normal, 1.0, 1.0),
            Layer {
                mask: clip,
                ..layer(2, StudioBlendMode::Multiply, 0.5, 1.0)
            },
        ],
    );
    let none = Sources::default();
    let mut sink = Image::new(extent);
    let receipt = run(
        backend,
        &plan,
        &request(7, Exactness::Approximate),
        &none,
        &mut sink,
    )
    .expect("hand-derived clip plan executes");
    let expect = [[0.526_f32, 0.454, 0.43, 1.0], [0.5, 0.5, 0.5, 1.0]];
    for (got, want) in sink.px.iter().zip(expect) {
        let err = got
            .iter()
            .zip(want)
            .map(|(g, e)| (g - e).abs())
            .fold(0.0_f32, f32::max);
        eprintln!("CLIP_HAND got {got:?} want {want:?} max_err {err:e}");
        assert!(err <= 2.0 * TOL_PER_LAYER, "clip hand oracle {err:e}");
    }
    // the CPU reference agrees with the hand derivation too (so it is a usable oracle above)
    let reference = Reference {
        plan: &plan,
        sources: &none,
        ext: None,
    };
    for (x, want) in expect.iter().enumerate() {
        let mut got = [0.0_f32; 4];
        reference.group(plan.root.0 as usize, x as i64, 0, &mut got);
        let err = got
            .iter()
            .zip(want)
            .map(|(g, e)| (g - e).abs())
            .fold(0.0_f32, f32::max);
        assert!(err <= 2.0 * TOL_PER_LAYER, "reference vs hand {err:e}");
    }
    assert_eq!(receipt.exactness.lowest, Exactness::Approximate);
    assert_eq!(backend.last_stats().expect("stats").levels, 2);
    assert_eq!(backend.resident_bytes(), 0);
}

// ---- slice 3: non-separable family and Approximate separable modes ------------------------------

/// The modes slice 3 added, taken from pigment's facts (not listed here): the non-separable family
/// (class Exact in pigment), the separable modes pigment marks Approximate and the whole-pixel
/// darker/lighter colour (Approximate).
fn slice3_modes() -> Vec<StudioBlendMode> {
    MODES
        .iter()
        .filter(|f| {
            f.applicability == Applicability::Layer
                && (f.family == Family::NonSeparable
                    || f.family == Family::WholePixel
                    || (f.family == Family::Separable && f.exactness == Exactness::Approximate))
        })
        .map(|f| f.mode)
        .collect()
}

fn max_abs(a: [f32; 3], b: [f32; 3]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

#[test]
fn slice3_hand_oracles_and_lattice_parity() {
    const TOL_PER_LAYER: f32 = 1.0e-6;
    // Hand-derived (W3C Compositing 1 s10.2 and the documented community functions), opaque
    // backdrop and source, opacity 1: the composite equals B(Cb, Cs). Derivations:
    //  Lum(C) = 0.3R + 0.59G + 0.11B. Cb = (0.2,0.4,0.6): Lum 0.362, Sat 0.4. Cs = (0.8,0.4,0.2):
    //  Lum 0.498, Sat 0.6.
    //  luminosity = SetLum(Cb, 0.498): d = 0.136 -> (0.336,0.536,0.736).
    //  colour = SetLum(Cs, 0.362): d = -0.136 -> (0.664,0.264,0.064).
    //  hue = SetLum(SetSat(Cs, 0.4), 0.362): SetSat -> (0.4, 0.2*0.4/0.6, 0) = (0.4,0.13333,0), Lum
    //   0.198667, d = 0.163333 -> (0.563333,0.296667,0.163333).
    //  saturation = SetLum(SetSat(Cb, 0.6), 0.362): SetSat -> (0, 0.2*0.6/0.4, 0.6) = (0,0.3,0.6),
    //   Lum 0.243, d = 0.119 -> (0.119,0.419,0.719).
    //  colour, Cb gray 0.9, Cs red: SetLum((1,0,0), 0.9) = (1.6,0.6,0.6), Lum 0.9, x = 1.6 > 1:
    //   C = 0.9 + (C-0.9)*0.1/0.7 -> (1.0, 0.857143, 0.857143).
    //  colour, Cb gray 0.1, Cs red: SetLum -> (0.8,-0.2,-0.2), Lum 0.1, n = -0.2 < 0:
    //   C = 0.1 + (C-0.1)*0.1/0.3 -> (0.333333, 0, 0).
    //  Approximate separable on Cb = (0.2,0.4,0.6): linear_burn max(b+s-1,0), linear_dodge_add
    //   min(b+s,1), subtract max(b-s,0), divide min(b/s,1), linear_light clamp(b+2s-1),
    //   vivid_light (s<0.5: 1-(1-b)/2s; else b/(2(1-s))), pin_light (s<0.5: min(b,2s); else
    //   max(b,2s-1)), hard_mix (b+s>=1 ? 1 : 0).
    type Hand = (StudioBlendMode, [f32; 3], [f32; 3], [f32; 3]);
    let cb = [0.2_f32, 0.4, 0.6];
    let cs = [0.8_f32, 0.4, 0.2];
    let hand: [Hand; 18] = [
        (
            StudioBlendMode::Hue,
            cb,
            cs,
            [0.563_333, 0.296_667, 0.163_333],
        ),
        (StudioBlendMode::Saturation, cb, cs, [0.119, 0.419, 0.719]),
        (StudioBlendMode::Colour, cb, cs, [0.664, 0.264, 0.064]),
        (StudioBlendMode::Luminosity, cb, cs, [0.336, 0.536, 0.736]),
        (
            StudioBlendMode::Colour,
            [0.9; 3],
            [1.0, 0.0, 0.0],
            [1.0, 0.857_143, 0.857_143],
        ),
        (
            StudioBlendMode::Colour,
            [0.1; 3],
            [1.0, 0.0, 0.0],
            [0.333_333, 0.0, 0.0],
        ),
        (
            StudioBlendMode::LinearBurn,
            cb,
            [0.9, 0.8, 0.7],
            [0.1, 0.2, 0.3],
        ),
        (
            StudioBlendMode::LinearDodgeAdd,
            cb,
            [0.5, 0.3, 0.1],
            [0.7, 0.7, 0.7],
        ),
        (
            StudioBlendMode::Subtract,
            cb,
            [0.5, 0.3, 0.1],
            [0.0, 0.1, 0.5],
        ),
        (
            StudioBlendMode::Divide,
            cb,
            [0.5, 0.3, 0.1],
            [0.4, 1.0, 1.0],
        ),
        (
            StudioBlendMode::LinearLight,
            cb,
            [0.6, 0.4, 0.3],
            [0.4, 0.2, 0.2],
        ),
        (
            StudioBlendMode::VividLight,
            cb,
            [0.6, 0.4, 0.3],
            [0.25, 0.25, 0.333_333],
        ),
        (
            StudioBlendMode::PinLight,
            cb,
            [0.7, 0.1, 0.2],
            [0.4, 0.2, 0.4],
        ),
        (
            StudioBlendMode::HardMix,
            cb,
            [0.9, 0.3, 0.45],
            [1.0, 0.0, 1.0],
        ),
        // whole pixel: channel sums Cb 1.2, Cs 0.9 / 1.4 (darker takes the source only when its
        // sum is smaller, lighter only when larger; otherwise the backdrop stays)
        (
            StudioBlendMode::DarkerColour,
            cb,
            [0.1, 0.3, 0.5],
            [0.1, 0.3, 0.5],
        ),
        (
            StudioBlendMode::DarkerColour,
            cb,
            [0.8, 0.4, 0.2],
            [0.2, 0.4, 0.6],
        ),
        (
            StudioBlendMode::LighterColour,
            cb,
            [0.8, 0.4, 0.2],
            [0.8, 0.4, 0.2],
        ),
        (
            StudioBlendMode::LighterColour,
            cb,
            [0.1, 0.3, 0.5],
            [0.2, 0.4, 0.6],
        ),
    ];
    let modes = slice3_modes();
    assert_eq!(
        modes.len(),
        14,
        "slice 3 = hue, saturation, colour, luminosity + 8 Approximate separable + darker/lighter colour: {modes:?}"
    );
    for (mode, b, s, expect) in hand {
        assert!(modes.contains(&mode), "{mode:?} is a slice-3 mode");
        // the hand table agrees with pigment itself (so a wrong hand value cannot hide a GPU bug)
        let cpu = blend_rgb(mode, b, s).expect("pigment blend");
        assert!(
            max_abs(cpu, expect) <= 2.0 * TOL_PER_LAYER,
            "{mode:?}: hand {expect:?} vs pigment {cpu:?}"
        );
    }

    let _guard = device_lock();
    let backend = shared_backend();
    let Some(info) = gpu_gate(backend, "slice-3 GPU parity").unwrap_or_else(|e| panic!("{e}"))
    else {
        // NOT_PROVEN (no adapter): the hand table above was checked against pigment only
        let tiny = rect(0, 0, 1, 1);
        let plan = plan_of(
            1,
            tiny,
            vec![solid(tiny, [0.2, 0.4, 0.6, 1.0])],
            vec![layer(0, StudioBlendMode::Hue, 1.0, 1.0)],
        );
        let mut sink = Image::new(tiny);
        assert_eq!(
            run(
                backend,
                &plan,
                &request(1, Exactness::Fitted),
                &Sources::default(),
                &mut sink
            ),
            Err(BackendError::Unavailable("no_adapter"))
        );
        assert_eq!(sink.accepted, 0);
        return;
    };
    eprintln!("GPU_BACKEND: {} (slice 3)", info.text());

    // (a) the hand table on the device
    let tiny = rect(0, 0, 1, 1);
    let mut worst_hand = 0.0_f32;
    for (mode, b, s, expect) in hand {
        let class = mode.facts().exactness.lowest_of(Exactness::Fitted);
        let plan = plan_of(
            1,
            tiny,
            vec![
                solid(tiny, [b[0], b[1], b[2], 1.0]),
                solid(tiny, [s[0], s[1], s[2], 1.0]),
            ],
            vec![
                layer(0, StudioBlendMode::Normal, 1.0, 1.0),
                layer(1, mode, 1.0, 1.0),
            ],
        );
        let mut sink = Image::new(tiny);
        let receipt = run(
            backend,
            &plan,
            &request(1, class),
            &Sources::default(),
            &mut sink,
        )
        .expect("hand plan executes on the device");
        let got = sink.px[0];
        let err = max_abs([got[0], got[1], got[2]], expect);
        eprintln!(
            "HAND {:<17} got {got:?} want {expect:?} max_err {err:e}",
            mode.key()
        );
        assert!(err <= 2.0 * TOL_PER_LAYER, "{mode:?} hand error {err:e}");
        assert_eq!(got[3], 1.0);
        assert_eq!(receipt.exactness.lowest, class, "{mode:?}");
        worst_hand = worst_hand.max(err);
    }

    // (b) exhaustive lattice: every (backdrop, source) pair of 7^3 colours with channel values
    // {0, 0.1, 0.25, 0.5, 0.75, 0.9, 1} (equal channels, ties between channels, exact 0 / 1 and
    // exact sums of 1: the places where sorts, clips and thresholds change branch). Both layers are
    // opaque and the backdrop tile enters bit-exact, so every comparison sees identical inputs
    // on CPU and GPU. Reference = pigment `composite_over`. Tolerance 2e-6 (two composited
    // layers at 1e-6 each; the Normal layer is exact). The non-separable family is ill-conditioned
    // next to its clip boundaries (colour over a white backdrop: Lum is 1 +- 1 ulp and the clip
    // scale (1 - l) / (x - l) divides by a near-zero difference): pigment's own f32 result differs
    // from the same formula in f64 by up to 1.6e-6 at such inputs (independent f32/f64 emulation),
    // so GPU-vs-pigment differences of about 1e-6 there are input conditioning, not a formula
    // difference; the hand and random-tile cases stay at 5e-7 or less.
    let levels = [0.0_f32, 0.1, 0.25, 0.5, 0.75, 0.9, 1.0];
    let mut colours = Vec::new();
    for r in levels {
        for g in levels {
            for b in levels {
                colours.push([r, g, b]);
            }
        }
    }
    let n = colours.len() as u32;
    let extent = rect(0, 0, n, n);
    let backdrop = OwnedTile::from_pixels(n, n, |_, y| {
        let c = colours[y as usize];
        [c[0], c[1], c[2], 1.0]
    });
    let source = OwnedTile::from_pixels(n, n, |x, _| {
        let c = colours[x as usize];
        [c[0], c[1], c[2], 1.0]
    });
    let sources = Sources::with_tiles(vec![(1, backdrop), (2, source)]);
    let (mut worst_nonsep, mut worst_approx) = (0.0_f32, 0.0_f32);
    for mode in &modes {
        let facts = mode.facts();
        let class = facts.exactness.lowest_of(Exactness::Fitted);
        let plan = plan_of(
            11,
            extent,
            vec![
                sources.tiles[0].1.input(1, extent),
                sources.tiles[1].1.input(2, extent),
            ],
            vec![
                layer(0, StudioBlendMode::Normal, 1.0, 1.0),
                layer(1, *mode, 1.0, 1.0),
            ],
        );
        let (err, at, receipt) = gpu_vs_reference(backend, &plan, &sources, class);
        eprintln!(
            "LATTICE {:<17} {} px class {} max_err {err:e} at {at:?}",
            facts.key,
            u64::from(n) * u64::from(n),
            class.key()
        );
        assert!(
            err <= 2.0 * TOL_PER_LAYER,
            "{} lattice error {err:e} at {at:?} > {:e}",
            facts.key,
            2.0 * TOL_PER_LAYER
        );
        assert_eq!(receipt.exactness.lowest, class, "{}", facts.key);
        assert!(
            receipt
                .exactness
                .operators
                .iter()
                .any(|o| o.key == facts.key && o.exactness == class),
            "{}: ledger does not carry class {}",
            facts.key,
            class.key()
        );
        if facts.family == Family::NonSeparable {
            worst_nonsep = worst_nonsep.max(err);
        } else {
            worst_approx = worst_approx.max(err);
        }
    }
    eprintln!(
        "SLICE3 max_err: hand {worst_hand:e}, lattice non-separable {worst_nonsep:e}, lattice approximate (separable + whole-pixel) {worst_approx:e} (tolerance {:e})",
        2.0 * TOL_PER_LAYER
    );
    assert_eq!(backend.resident_bytes(), 0);
}

#[test]
fn slice3_modes_in_groups_masks_clip_parity() {
    const TOL_PER_LAYER: f32 = 1.0e-6;
    let _guard = device_lock();
    let backend = shared_backend();
    let Some(info) = gpu_gate(backend, "slice-3 group parity").unwrap_or_else(|e| panic!("{e}"))
    else {
        return;
    };
    eprintln!("GPU_BACKEND: {} (slice 3 groups)", info.text());
    let clip = Some(MaskInput::clip_to_base());
    let extent = rect(0, 0, 24, 18);
    let t_a = OwnedTile::random(24, 18, 61);
    let t_b = OwnedTile::random(12, 10, 62);
    let m = OwnedMask::random(rect(0, 0, 24, 18), MaskFormat::CoverageU8, 9);
    let plan = plan_of(
        8,
        extent,
        vec![
            t_a.input(1, extent),                            // 0
            solid(rect(4, 2, 14, 12), [0.7, 0.2, 0.4, 0.8]), // 1
            t_b.input(2, rect(6, 4, 12, 10)),                // 2
            solid(extent, [0.4, 0.5, 0.1, 0.7]),             // 3
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![
                    layer(2, StudioBlendMode::Normal, 1.0, 1.0),
                    layer(1, StudioBlendMode::VividLight, 0.8, 1.0),
                ],
            }), // 4: isolated group with an Approximate member
        ],
        vec![
            layer(0, StudioBlendMode::Normal, 1.0, 1.0),
            Layer {
                mask: Some(m.input(21, false)),
                ..layer(4, StudioBlendMode::Hue, 0.7, 1.0)
            },
            layer(1, StudioBlendMode::Saturation, 0.6, 0.9),
            layer(2, StudioBlendMode::Divide, 0.9, 1.0), // base of the clipping run
            Layer {
                mask: clip,
                ..layer(3, StudioBlendMode::Luminosity, 0.8, 1.0)
            },
            Layer {
                mask: clip,
                ..layer(1, StudioBlendMode::PinLight, 0.7, 1.0)
            },
        ],
    );
    let sources = Sources {
        tiles: vec![(1, t_a), (2, t_b)],
        masks: vec![(21, m)],
    };
    let (err, at, receipt) = gpu_vs_reference(backend, &plan, &sources, Exactness::Approximate);
    let tolerance = TOL_PER_LAYER * layer_records(&plan) as f32;
    eprintln!("SLICE3_GROUPS max_err {err:e} at {at:?} (tolerance {tolerance:e})");
    assert!(
        err <= tolerance,
        "slice-3 group parity {err:e} > {tolerance:e}"
    );
    assert_eq!(receipt.exactness.lowest, Exactness::Approximate);
    for key in ["vivid_light", "divide", "pin_light"] {
        assert!(
            receipt
                .exactness
                .operators
                .iter()
                .any(|o| o.key == key && o.exactness == Exactness::Approximate),
            "{key} must travel as Approximate"
        );
    }
    for key in ["hue", "saturation", "luminosity"] {
        assert!(
            receipt
                .exactness
                .operators
                .iter()
                .any(|o| o.key == key && o.exactness == Exactness::Fitted),
            "{key} must be Fitted"
        );
    }
    assert_eq!(backend.resident_bytes(), 0);
}

// ---- slice 4: plan-extension envelope (mask density / invert / feather, Blend-If, adjustments) ---
//
// Every GPU result is compared with `Reference`, which evaluates the envelope from pigment's scalar
// functions only (`mask_coverage`, `feather_kernel`, `blend_if_weight`, `composite_adjustment`),
// never with render-cpu. Tolerances are declared per operator before the run and the observed
// maximum is printed (`EXT_*` lines).

/// Effect kinds without GPU passes (typed refusal). Lane SMB-PS-ADJUST-STYLES-2 gave satin, bevel & emboss and gradient overlay GPU passes
/// (parity in tests/gpu_layer_styles_2.rs): none is refused any more.
const GPU_REFUSED_EFFECTS: [EffectKind; 0] = [];

/// Adjustment kinds the shader must refuse, pinned independently of `gpu_adjust_supported`. Lane
/// SMB-PS-ADJUST-STYLES-2 gave every kind a WGSL body (legacy brightness / contrast and the second
/// STU-RAS-151 set; parity in tests/gpu_adjust_styles_2.rs): none is refused by kind any more
/// (modern brightness / contrast stays pigment's typed refusal at admission).
const REFUSED_ADJUSTMENTS: [AdjustmentKind; 0] = [];

fn run_ext(
    backend: &GpuBackend,
    plan: &RenderPlan<NoVector>,
    ext: &PlanExtensions,
    req: &ExecuteRequest,
    sources: &dyn PlanSources,
    sink: &mut Image,
) -> Result<hsk_studio_pigment::render_plan::RenderReceipt, BackendError> {
    backend.execute_extended(plan, ext, req, sources, &CancellationToken::default(), sink)
}

fn has_operator(
    receipt: &hsk_studio_pigment::render_plan::RenderReceipt,
    key: &str,
    class: Exactness,
) -> bool {
    receipt
        .exactness
        .operators
        .iter()
        .any(|o| o.key == key && o.exactness == class)
}

/// 17 coverage levels used by the mask grid: 0, 16, ..., 240, 255.
fn grid_levels() -> [u8; 17] {
    let mut levels = [0_u8; 17];
    for (i, v) in levels.iter_mut().enumerate() {
        *v = if i == 16 { 255 } else { (i * 16) as u8 };
    }
    levels
}

#[test]
fn ext_mask_density_invert_feather_parity() {
    const TOL: f32 = 2.0e-6;
    let _guard = device_lock();
    let backend = shared_backend();
    let Some(info) = gpu_gate(backend, "envelope mask parity").unwrap_or_else(|e| panic!("{e}"))
    else {
        return;
    };
    eprintln!("GPU_BACKEND: {} (envelope masks)", info.text());

    // (a) 17 x 17 grid: coverage varies along x, the backdrop along y; effective coverage must equal
    // pigment mask_coverage(c, inverted, density) for density 0 / 0.5 / 1 with and without invert.
    let extent = rect(0, 0, 17, 17);
    let levels = grid_levels();
    let mask = OwnedMask::from_u8(extent, (0..17 * 17).map(|i| levels[i % 17]).collect());
    let backdrop = OwnedTile::from_pixels(17, 17, |_, y| {
        let g = y as f32 / 16.0;
        [g, 1.0 - g, 0.5, 1.0]
    });
    let red = [1.0_f32, 0.2, 0.4, 1.0];
    let sources = Sources {
        tiles: vec![(1, backdrop)],
        masks: vec![(7, mask)],
    };
    let mut worst = 0.0_f32;
    for density in [0.0_f32, 0.5, 1.0] {
        for inverted in [false, true] {
            let mut masked = sources.masks[0].1.input(7, inverted);
            masked.density = density;
            let plan = plan_of(
                21,
                extent,
                vec![sources.tiles[0].1.input(1, extent), solid(extent, red)],
                vec![
                    layer(0, StudioBlendMode::Normal, 1.0, 1.0),
                    Layer {
                        mask: Some(masked),
                        ..layer(1, StudioBlendMode::Normal, 1.0, 1.0)
                    },
                ],
            );
            let empty = PlanExtensions::default();
            let (err, at, receipt) = gpu_vs_reference_with(
                backend,
                &plan,
                Some(&empty),
                &sources,
                Exactness::Approximate,
            );
            eprintln!("EXT_MASK density {density} inverted {inverted}: max_err {err:e} at {at:?}");
            assert!(
                err <= TOL,
                "mask grid d={density} inv={inverted}: {err:e} > {TOL:e}"
            );
            assert_eq!(
                has_operator(&receipt, "mask_density", Exactness::Approximate),
                density < 1.0,
                "mask_density is in the ledger exactly when density < 1"
            );
            worst = worst.max(err);
            if density == 1.0 && !inverted {
                // an empty envelope without density < 1 is the v1 plan, byte for byte
                let mut v1 = Image::new(extent);
                let v1_receipt = run(
                    backend,
                    &plan,
                    &request(21, Exactness::Approximate),
                    &sources,
                    &mut v1,
                )
                .expect("v1 path");
                let mut ext = Image::new(extent);
                let ext_receipt = run_ext(
                    backend,
                    &plan,
                    &empty,
                    &request(21, Exactness::Approximate),
                    &sources,
                    &mut ext,
                )
                .expect("empty envelope");
                assert_eq!(v1.px, ext.px);
                assert_eq!(v1_receipt.plan_digest, ext_receipt.plan_digest);
                assert_eq!(v1_receipt.exactness, ext_receipt.exactness);
                assert_eq!(
                    v1_receipt.peak_working_bytes,
                    ext_receipt.peak_working_bytes
                );
            }
        }
    }
    eprintln!("EXT_MASK grid worst {worst:e} (tolerance {TOL:e})");

    // (b) feather (host blur with pigment's taps) with density and invert: 24 x 18 plane with a hard
    // vertical edge, radius 2 (sigma 1) and radius 10 (support 15 > the plane height: border clamp).
    let fe = rect(0, 0, 24, 18);
    let edge = OwnedMask::from_u8(
        fe,
        (0..24 * 18)
            .map(|i| if i % 24 < 12 { 255_u8 } else { 0 })
            .collect(),
    );
    let noise = OwnedMask::random(fe, MaskFormat::CoverageU8, 5);
    let back = OwnedTile::random(24, 18, 77);
    let fsources = Sources {
        tiles: vec![(1, back)],
        masks: vec![(7, edge), (8, noise)],
    };
    for (key, radius, density, inverted) in [
        (7, 2.0_f32, 1.0_f32, false),
        (7, 10.0, 0.6, true),
        (8, 3.0, 0.8, false),
    ] {
        let mut masked = fsources
            .masks
            .iter()
            .find(|(k, _)| *k == key)
            .expect("mask")
            .1
            .input(key, inverted);
        masked.density = density;
        let plan = plan_of(
            22,
            fe,
            vec![
                fsources.tiles[0].1.input(1, fe),
                solid(fe, [0.9, 0.1, 0.3, 1.0]),
            ],
            vec![
                layer(0, StudioBlendMode::Normal, 1.0, 1.0),
                Layer {
                    mask: Some(masked),
                    ..layer(1, StudioBlendMode::Normal, 1.0, 1.0)
                },
            ],
        );
        let ext = PlanExtensions::from_layers(vec![
            LayerExtension::new(plan.root, 1).with_mask_feather(radius),
        ]);
        let (err, at, receipt) = gpu_vs_reference_with(
            backend,
            &plan,
            Some(&ext),
            &fsources,
            Exactness::Approximate,
        );
        eprintln!(
            "EXT_FEATHER mask {key} radius {radius} density {density} inverted {inverted}: max_err {err:e} at {at:?}"
        );
        assert!(err <= 5.0e-6, "feather parity {err:e}");
        assert!(has_operator(
            &receipt,
            "mask_feather",
            Exactness::Approximate
        ));
        assert_eq!(backend.resident_bytes(), 0);
    }
}

#[test]
fn ext_blend_if_parity() {
    const TOL: f32 = 5.0e-6;
    let _guard = device_lock();
    let backend = shared_backend();
    let Some(info) =
        gpu_gate(backend, "envelope Blend-If parity").unwrap_or_else(|e| panic!("{e}"))
    else {
        return;
    };
    eprintln!("GPU_BACKEND: {} (envelope Blend-If)", info.text());
    let entry = |channel, source, destination| BlendRangeEntry {
        channel,
        source,
        destination,
    };

    // (a) flat plan: gray split ranges on both sides, a red range on the backdrop, an identity
    // green entry that must be dropped; random tiles with exact 0 / 1 samples.
    let extent = rect(0, 0, 64, 8);
    let back = OwnedTile::random(64, 8, 91);
    let top = OwnedTile::random(64, 8, 92);
    let sources = Sources::with_tiles(vec![(1, back), (2, top)]);
    let plan = plan_of(
        31,
        extent,
        vec![
            sources.tiles[0].1.input(1, extent),
            sources.tiles[1].1.input(2, extent),
        ],
        vec![
            layer(0, StudioBlendMode::Normal, 1.0, 1.0),
            layer(1, StudioBlendMode::Multiply, 0.8, 1.0),
        ],
    );
    let entries = vec![
        entry(
            BlendChannel::Gray,
            [0.2, 0.4, 0.6, 0.8],
            [0.0, 0.0, 0.75, 0.9],
        ),
        entry(BlendChannel::Red, IDENTITY_RANGE, [0.25, 0.5, 1.0, 1.0]),
        entry(BlendChannel::Green, IDENTITY_RANGE, IDENTITY_RANGE),
    ];
    let ext = PlanExtensions::from_layers(vec![
        LayerExtension::new(plan.root, 1).with_blend_if(entries),
    ]);
    let (err, at, receipt) =
        gpu_vs_reference_with(backend, &plan, Some(&ext), &sources, Exactness::Approximate);
    eprintln!("EXT_BLEND_IF flat: max_err {err:e} at {at:?} (tolerance {TOL:e})");
    assert!(err <= TOL, "Blend-If flat parity {err:e}");
    assert!(has_operator(&receipt, "blend_if", Exactness::Approximate));
    // the weight really changes the result (the test is not vacuous): without the envelope the
    // plain plan differs from the Blend-If result somewhere
    let mut plain = Image::new(extent);
    run(
        backend,
        &plan,
        &request(31, Exactness::Approximate),
        &sources,
        &mut plain,
    )
    .expect("plain plan");
    let mut weighted = Image::new(extent);
    run_ext(
        backend,
        &plan,
        &ext,
        &request(31, Exactness::Approximate),
        &sources,
        &mut weighted,
    )
    .expect("weighted plan");
    let differing = plain
        .px
        .iter()
        .zip(&weighted.px)
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        differing > 32,
        "Blend-If must change the output, changed {differing} texels"
    );

    // (b) Blend-If on an unclipped layer after a clipping run, a layer sourced from an isolated
    // group, and a layer inside that group. The clip base and the clipped layer of the run carry
    // none: pigment defines no Blend-If on either, admission refuses it (checked below, and in
    // fx_admission_and_refusals_run_without_adapter).
    let ge = rect(0, 0, 26, 20);
    let t_a = OwnedTile::random(26, 20, 101);
    let t_b = OwnedTile::random(26, 20, 102);
    let t_c = OwnedTile::random(18, 14, 103);
    let clip = Some(MaskInput::clip_to_base());
    let gsources = Sources::with_tiles(vec![(1, t_a), (2, t_b), (3, t_c)]);
    let plan = plan_of(
        32,
        ge,
        vec![
            gsources.tiles[0].1.input(1, ge),                 // 0
            gsources.tiles[1].1.input(2, ge),                 // 1
            solid(ge, [0.6, 0.3, 0.8, 0.9]),                  // 2
            gsources.tiles[2].1.input(3, rect(4, 3, 18, 14)), // 3
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![
                    layer(1, StudioBlendMode::Normal, 1.0, 1.0),
                    layer(2, StudioBlendMode::Screen, 0.7, 1.0),
                ],
            }), // 4
        ],
        vec![
            layer(0, StudioBlendMode::Normal, 1.0, 1.0),
            layer(3, StudioBlendMode::Multiply, 0.9, 1.0), // clip base
            Layer {
                mask: clip,
                ..layer(2, StudioBlendMode::Overlay, 0.8, 1.0)
            },
            layer(4, StudioBlendMode::Normal, 0.6, 1.0), // isolated group as the source
            layer(2, StudioBlendMode::Overlay, 0.8, 1.0), // unclipped, the former clipped layer's source
        ],
    );
    let root = plan.root;
    let blue = entry(BlendChannel::Blue, IDENTITY_RANGE, [0.0, 0.0, 0.6, 0.95]);
    let ext = PlanExtensions::from_layers(vec![
        LayerExtension::new(root, 4).with_blend_if(vec![blue]),
        LayerExtension::new(root, 3).with_blend_if(vec![entry(
            BlendChannel::Green,
            [0.2, 0.5, 1.0, 1.0],
            [0.0, 0.0, 0.8, 1.0],
        )]),
        LayerExtension::new(PlanNodeId(4), 1).with_blend_if(vec![entry(
            BlendChannel::Red,
            [0.0, 0.0, 0.5, 0.7],
            IDENTITY_RANGE,
        )]),
    ]);
    let (err, at, _) = gpu_vs_reference_with(
        backend,
        &plan,
        Some(&ext),
        &gsources,
        Exactness::Approximate,
    );
    eprintln!("EXT_BLEND_IF group/clip: max_err {err:e} at {at:?} (tolerance {TOL:e})");
    assert!(
        err <= TOL * layer_records(&plan) as f32,
        "Blend-If group/clip parity {err:e}"
    );
    assert_eq!(backend.resident_bytes(), 0);

    // (c) Refusal: the same ranges on the clip base (root layer 1) and on the clipped layer (root
    // layer 2) are refused by shared admission with pigment's keys, before any upload or sink call.
    // Non-vacuity: the unclipped layer 4 above carried the identical entry in (b) and rendered.
    for (layer_index, key) in [(1, "blend_if_clip_base"), (2, "blend_if_clipped_layer")] {
        let refused = PlanExtensions::from_layers(vec![
            LayerExtension::new(root, layer_index).with_blend_if(vec![blue]),
        ]);
        let mut sink = Image::new(ge);
        assert_eq!(
            run_ext(
                backend,
                &plan,
                &refused,
                &request(32, Exactness::Approximate),
                &gsources,
                &mut sink
            ),
            Err(BackendError::Unsupported {
                operator: key,
                exactness: Exactness::Unsupported
            }),
            "layer {layer_index}"
        );
        assert_eq!(sink.accepted, 0, "layer {layer_index}: sink untouched");
        assert_eq!(backend.resident_bytes(), 0);
    }
}

/// One adjustment case on the 17^3 colour lattice.
struct AdjustCase {
    name: &'static str,
    op: AdjustmentOp,
    mode: StudioBlendMode,
    opacity: f32,
    tolerance: f32,
}

/// Lead disposition 2026-10-31 (lane SMB-GPU-CLIENT-PORT-1), the threshold tie rule from both
/// sides: a pixel whose luminance ties the cut exactly may be 0 or 1 (binary32 vs pigment), while
/// pixels 1e-4 below and above the cut match pigment exactly on every backend (0 and 1).
#[test]
fn threshold_tie_at_the_cut_is_either_side_and_off_tie_pixels_match() {
    let _guard = device_lock();
    let backend = shared_backend();
    let Some(info) = gpu_gate(backend, "threshold tie rule").unwrap_or_else(|e| panic!("{e}")) else {
        return;
    };
    let level = 128.0_f32 / 255.0;
    let cut = threshold_cut(level) as f32;
    let (below, above) = (cut - 1.0e-4, cut + 1.0e-4);
    // (7/16, 10/16, 0): luminance 0.5 = the cut in real arithmetic; greys: luminance = value
    let pixels = [[7.0 / 16.0, 10.0 / 16.0, 0.0], [below; 3], [above; 3]];
    assert!(threshold_tie(pixels[0], level), "the tie pixel is a boundary pixel");
    assert!(!threshold_tie(pixels[1], level) && !threshold_tie(pixels[2], level));
    let extent = rect(0, 0, 3, 1);
    let tile = OwnedTile::from_pixels(3, 1, |x, _| {
        let [r, g, b] = pixels[x as usize];
        [r, g, b, 1.0]
    });
    let sources = Sources::with_tiles(vec![(1, tile)]);
    let plan = plan_of(
        43,
        extent,
        vec![sources.tiles[0].1.input(1, extent), solid(extent, [0.0; 4])],
        vec![
            layer(0, StudioBlendMode::Normal, 1.0, 1.0),
            layer(1, StudioBlendMode::Normal, 1.0, 1.0),
        ],
    );
    let ext = PlanExtensions::from_layers(vec![
        LayerExtension::new(plan.root, 1).with_adjustment(AdjustmentOp::Threshold { level }),
    ]);
    // pigment's own answer off the tie: below -> black, above -> white
    let reference = Reference { plan: &plan, sources: &sources, ext: Some(&ext) };
    for (x, want) in [(1_i64, 0.0_f32), (2, 1.0)] {
        let mut px = [0.0_f32; 4];
        reference.group(plan.root.0 as usize, x, 0, &mut px);
        assert_eq!(px, [want, want, want, 1.0], "pigment at x {x}");
    }
    let (err, at, _, ties) = gpu_vs_reference_ties(
        backend,
        &plan,
        Some(&ext),
        &sources,
        Exactness::Approximate,
        &|x, _| x == 0,
    );
    eprintln!(
        "THRESHOLD_TIE backend {}: off-tie max_err {err:e} at {at:?}, {ties} boundary pixel(s) accepted as 0 or 1",
        info.backend
    );
    assert_eq!((err, ties), (0.0, 1), "off-tie pixels match pigment exactly");
}

#[test]
fn ext_adjustment_layer_parity() {
    let _guard = device_lock();
    let backend = shared_backend();
    let Some(info) =
        gpu_gate(backend, "envelope adjustment parity").unwrap_or_else(|e| panic!("{e}"))
    else {
        return;
    };
    eprintln!("GPU_BACKEND: {} (envelope adjustments)", info.text());
    // lattice: x = i * 17 + j -> (r, g) = (i / 16, j / 16), y -> b = y / 16; opaque backdrop
    let extent = rect(0, 0, 289, 17);
    let lattice = OwnedTile::from_pixels(289, 17, |x, y| {
        [
            (x / 17) as f32 / 16.0,
            (x % 17) as f32 / 16.0,
            y as f32 / 16.0,
            1.0,
        ]
    });
    let rec = |ib, iw, gamma, ob, ow| LevelsRecord {
        input_black: ib,
        input_white: iw,
        gamma,
        output_black: ob,
        output_white: ow,
    };
    let cases = vec![
        AdjustCase {
            name: "invert",
            op: AdjustmentOp::Invert,
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            tolerance: 2.0e-6,
        },
        AdjustCase {
            name: "invert multiply 0.6",
            op: AdjustmentOp::Invert,
            mode: StudioBlendMode::Multiply,
            opacity: 0.6,
            tolerance: 2.0e-6,
        },
        AdjustCase {
            name: "levels",
            op: AdjustmentOp::Levels(Levels {
                composite: rec(0.1, 0.9, 2.0, 0.05, 0.95),
                red: rec(0.1, 0.8, 1.5, 0.0, 1.0),
                green: LevelsRecord::IDENTITY,
                blue: LevelsRecord::IDENTITY,
            }),
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            tolerance: 5.0e-6,
        },
        AdjustCase {
            name: "posterize 4",
            op: AdjustmentOp::Posterize { levels: 4 },
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            tolerance: 2.0e-6,
        },
        AdjustCase {
            // level 128 / 255: the cut is level - 0.5 / 255 = 0.5 exactly, which lattice
            // luminances tie (e.g. (7/16, 10/16, 0)); those boundary pixels may be 0 or 1 on either
            // backend (lead disposition 2026-10-31: Vulkan 1.0 at (129, 0), DX12 0); every other
            // lattice pixel is compared as before
            name: "threshold 128",
            op: AdjustmentOp::Threshold {
                level: 128.0 / 255.0,
            },
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            tolerance: 2.0e-6,
        },
        AdjustCase {
            name: "exposure +1 linear",
            op: AdjustmentOp::Exposure(Exposure {
                exposure: 1.0,
                offset: 0.0,
                gamma: 1.0,
                linearize_gamma: 1.0,
            }),
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            tolerance: 2.0e-6,
        },
        AdjustCase {
            name: "exposure +1 offset 0.1 gamma 1.5 encoded 2.2",
            op: AdjustmentOp::Exposure(Exposure {
                exposure: 1.0,
                offset: 0.1,
                gamma: 1.5,
                linearize_gamma: 2.2,
            }),
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            tolerance: 5.0e-6,
        },
        AdjustCase {
            name: "curves composite + red",
            op: AdjustmentOp::Curves(Curves {
                composite: vec![
                    [0.0, 0.0],
                    [0.25, 0.15],
                    [0.5, 0.5],
                    [0.75, 0.85],
                    [1.0, 1.0],
                ],
                red: vec![[0.0, 0.1], [0.5, 0.8], [1.0, 0.9]],
                green: vec![],
                blue: vec![],
            }),
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            tolerance: 5.0e-6,
        },
        AdjustCase {
            name: "hue/saturation master",
            op: AdjustmentOp::HueSaturation(HueSaturation {
                hue: 60.0,
                saturation: 30.0,
                lightness: -10.0,
                colorize: false,
            }),
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            tolerance: 5.0e-6,
        },
        AdjustCase {
            name: "hue/saturation colorize",
            op: AdjustmentOp::HueSaturation(HueSaturation {
                hue: 120.0,
                saturation: 50.0,
                lightness: 10.0,
                colorize: true,
            }),
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            tolerance: 5.0e-6,
        },
    ];
    let mut worst_overall = 0.0_f32;
    for case in &cases {
        let kind = case.op.kind();
        let sources = Sources::with_tiles(vec![(1, lattice.clone())]);
        let plan = plan_of(
            41,
            extent,
            vec![
                sources.tiles[0].1.input(1, extent),
                solid(extent, [0.0; 4]), // the adjustment placeholder
            ],
            vec![
                layer(0, StudioBlendMode::Normal, 1.0, 1.0),
                layer(1, case.mode, case.opacity, 1.0),
            ],
        );
        let ext = PlanExtensions::from_layers(vec![
            LayerExtension::new(plan.root, 1).with_adjustment(case.op.clone()),
        ]);
        let tie = |x: i64, y: i64| match case.op {
            AdjustmentOp::Threshold { level } => threshold_tie(
                [(x / 17) as f32 / 16.0, (x % 17) as f32 / 16.0, y as f32 / 16.0],
                level,
            ),
            _ => false,
        };
        let (err, at, receipt, ties) =
            gpu_vs_reference_ties(backend, &plan, Some(&ext), &sources, Exactness::Approximate, &tie);
        eprintln!(
            "EXT_ADJUST {:<46} max_err {err:e} at {at:?} (tolerance {:e})",
            case.name, case.tolerance
        );
        if ties > 0 {
            eprintln!(
                "NOT_PROVEN: threshold tie at cut, {ties} boundary pixel(s), backend {}",
                info.backend
            );
        }
        assert!(
            err <= case.tolerance,
            "{} parity {err:e} > {:e}",
            case.name,
            case.tolerance
        );
        // pigment's intrinsic class is kept (adjustment_layer: approximate); the kind carries its
        // own class lowered to the backend's Fitted
        assert!(has_operator(
            &receipt,
            "adjustment_layer",
            Exactness::Approximate
        ));
        let class = kind.facts().exactness.lowest_of(Exactness::Fitted);
        assert!(
            has_operator(&receipt, kind.key(), class),
            "{}: {:?}",
            case.name,
            receipt.exactness
        );
        assert_eq!(receipt.exactness.lowest, Exactness::Approximate);
        worst_overall = worst_overall.max(err);
    }
    eprintln!("EXT_ADJUST worst {worst_overall:e}");

    // semi-transparent backdrop (random tile): alpha must be unchanged by the adjustment, and a
    // transparent texel stays transparent; with a mask, Blend-If on the adjusted colour and an
    // opacity, k = opacity * mask * weight
    let ra = rect(0, 0, 40, 30);
    let back = OwnedTile::random(40, 30, 55);
    let mask = OwnedMask::random(ra, MaskFormat::CoverageU8, 12);
    let sources = Sources {
        tiles: vec![(1, back)],
        masks: vec![(9, mask)],
    };
    let plan = plan_of(
        42,
        ra,
        vec![sources.tiles[0].1.input(1, ra), solid(ra, [0.0; 4])],
        vec![
            layer(0, StudioBlendMode::Normal, 1.0, 1.0),
            Layer {
                mask: Some(sources.masks[0].1.input(9, false)),
                ..layer(1, StudioBlendMode::Normal, 0.7, 1.0)
            },
        ],
    );
    let ext = PlanExtensions::from_layers(vec![
        LayerExtension::new(plan.root, 1)
            .with_adjustment(AdjustmentOp::Invert)
            .with_blend_if(vec![BlendRangeEntry {
                channel: BlendChannel::Gray,
                source: [0.1, 0.3, 0.8, 0.95],
                destination: IDENTITY_RANGE,
            }]),
    ]);
    let (err, at, _) =
        gpu_vs_reference_with(backend, &plan, Some(&ext), &sources, Exactness::Approximate);
    eprintln!("EXT_ADJUST translucent backdrop + mask + Blend-If: max_err {err:e} at {at:?}");
    assert!(err <= 5.0e-6, "translucent adjustment parity {err:e}");
    // alpha is untouched: compare against the plain backdrop alpha
    let mut sink = Image::new(ra);
    run_ext(
        backend,
        &plan,
        &ext,
        &request(42, Exactness::Approximate),
        &sources,
        &mut sink,
    )
    .expect("translucent plan");
    let mut base_only = Image::new(ra);
    let base_plan = plan_of(
        43,
        ra,
        vec![sources.tiles[0].1.input(1, ra)],
        vec![layer(0, StudioBlendMode::Normal, 1.0, 1.0)],
    );
    run(
        backend,
        &base_plan,
        &request(43, Exactness::Fitted),
        &sources,
        &mut base_only,
    )
    .expect("backdrop only");
    for (adjusted, plain) in sink.px.iter().zip(&base_only.px) {
        assert_eq!(
            adjusted[3], plain[3],
            "an adjustment layer never changes alpha"
        );
    }

    // an adjustment layer inside a clipping run acts on the run unit only: the clipped invert must
    // not change the backdrop outside the base
    let ce = rect(0, 0, 26, 20);
    let tile = OwnedTile::random(26, 20, 66);
    let base_tile = OwnedTile::random(14, 10, 67);
    let csources = Sources::with_tiles(vec![(1, tile), (2, base_tile)]);
    let cplan = plan_of(
        44,
        ce,
        vec![
            csources.tiles[0].1.input(1, ce),
            csources.tiles[1].1.input(2, rect(6, 5, 14, 10)),
            solid(ce, [0.0; 4]),
        ],
        vec![
            layer(0, StudioBlendMode::Normal, 1.0, 1.0),
            layer(1, StudioBlendMode::Normal, 1.0, 1.0), // clip base
            Layer {
                mask: Some(MaskInput::clip_to_base()),
                ..layer(2, StudioBlendMode::Normal, 0.8, 1.0)
            },
        ],
    );
    let cext = PlanExtensions::from_layers(vec![
        LayerExtension::new(cplan.root, 2).with_adjustment(AdjustmentOp::Posterize { levels: 3 }),
    ]);
    let (err, at, _) = gpu_vs_reference_with(
        backend,
        &cplan,
        Some(&cext),
        &csources,
        Exactness::Approximate,
    );
    eprintln!("EXT_ADJUST clipped adjustment: max_err {err:e} at {at:?}");
    assert!(err <= 5.0e-6, "clipped adjustment parity {err:e}");
    assert_eq!(backend.resident_bytes(), 0);
}

/// Hardware-independent: capability table, shared admission and the refusals of the envelope all
/// happen before any device work (no adapter is needed to prove them).
#[test]
fn ext_refusals_and_capabilities_run_without_adapter() {
    let backend = no_adapter();
    let extent = rect(0, 0, 8, 8);
    let back = OwnedTile::random(8, 8, 3);
    let mask = OwnedMask::from_u8(extent, vec![200; 64]);
    let sources = Sources {
        tiles: vec![(1, back)],
        masks: vec![(4, mask)],
    };
    let approx = request(51, Exactness::Approximate);
    let plan_with = |top: Layer| {
        plan_of(
            51,
            extent,
            vec![
                sources.tiles[0].1.input(1, extent),
                solid(extent, [0.4, 0.5, 0.6, 1.0]),
                solid(extent, [0.0; 4]),
            ],
            vec![layer(0, StudioBlendMode::Normal, 1.0, 1.0), top],
        )
    };
    let mut sink = Image::new(extent);

    // capability table: every adjustment kind either has a body or is in the pinned refusal list
    for kind in AdjustmentKind::ALL {
        assert_eq!(
            gpu_adjust_supported(kind),
            !REFUSED_ADJUSTMENTS.contains(&kind),
            "{}: supported list differs from the declared refusals",
            kind.key()
        );
        // a supported operator reports the lower class of pigment's and Fitted (never a better
        // one than pigment's, CX-CAP-001)
        let want = if REFUSED_ADJUSTMENTS.contains(&kind) {
            Exactness::Unsupported
        } else {
            ExtOperator::Adjustment(kind)
                .intrinsic()
                .lowest_of(Exactness::Fitted)
        };
        assert_eq!(
            backend.extension_exactness(ExtOperator::Adjustment(kind)),
            want
        );
    }
    for op in [
        ExtOperator::AdjustmentLayer,
        ExtOperator::BlendIf,
        ExtOperator::MaskDensity,
        ExtOperator::MaskFeather,
    ] {
        assert_eq!(
            backend.extension_exactness(op),
            op.intrinsic().lowest_of(Exactness::Fitted),
            "{}",
            op.key()
        );
    }
    // knockout runs on this backend since lane SMB-PS-ADJUST-STYLES-3 (tests/gpu_knockout.rs)
    for depth in [Knockout::Shallow, Knockout::Deep] {
        assert_eq!(
            backend.extension_exactness(ExtOperator::Knockout(depth)),
            Exactness::Approximate
        );
    }
    // Blend-If on a clip base or a clipped layer has no definition: Unsupported in pigment and here
    for (op, key) in [
        (ExtOperator::BlendIfClipBase, "blend_if_clip_base"),
        (ExtOperator::BlendIfClippedLayer, "blend_if_clipped_layer"),
    ] {
        assert_eq!(op.key(), key);
        assert_eq!(op.intrinsic(), Exactness::Unsupported);
        assert_eq!(backend.extension_exactness(op), Exactness::Unsupported);
    }
    // effects are implemented (fx_* tests); their class is pigment's (Approximate). The layer
    // styles of lane SMB-PS-ADJUST-STYLES (satin, bevel & emboss, gradient overlay) have GPU
    // passes since lane SMB-PS-ADJUST-STYLES-2 (GPU_REFUSED_EFFECTS is empty).
    for kind in EffectKind::ALL {
        let want = if GPU_REFUSED_EFFECTS.contains(&kind) {
            Exactness::Unsupported
        } else {
            Exactness::Approximate
        };
        assert_eq!(
            backend.extension_exactness(ExtOperator::Effect(kind)),
            want,
            "{}",
            kind.key()
        );
    }

    // mask density: refused by v1 execute, admitted by the envelope path (then only the missing
    // device remains)
    let mut masked = sources.masks[0].1.input(4, false);
    masked.density = 0.5;
    let density_plan = plan_with(Layer {
        mask: Some(masked),
        ..layer(1, StudioBlendMode::Normal, 1.0, 1.0)
    });
    assert!(matches!(
        run(&backend, &density_plan, &approx, &sources, &mut sink),
        Err(BackendError::Unsupported {
            operator: "mask_density",
            ..
        })
    ));
    assert_eq!(
        run_ext(
            &backend,
            &density_plan,
            &PlanExtensions::default(),
            &approx,
            &sources,
            &mut sink
        ),
        Err(BackendError::Unavailable("no_adapter"))
    );

    // adjustment layer: class Approximate (pigment's rule), refused by a Fitted floor
    let adj_plan = plan_with(layer(2, StudioBlendMode::Normal, 1.0, 1.0));
    let adj_ext = |op| {
        PlanExtensions::from_layers(vec![
            LayerExtension::new(adj_plan.root, 1).with_adjustment(op),
        ])
    };
    assert_eq!(
        run_ext(
            &backend,
            &adj_plan,
            &adj_ext(AdjustmentOp::Invert),
            &request(51, Exactness::Fitted),
            &sources,
            &mut sink
        ),
        Err(BackendError::Unsupported {
            operator: "adjustment_layer",
            exactness: Exactness::Approximate
        })
    );
    assert_eq!(
        run_ext(
            &backend,
            &adj_plan,
            &adj_ext(AdjustmentOp::Invert),
            &approx,
            &sources,
            &mut sink
        ),
        Err(BackendError::Unavailable("no_adapter"))
    );
    // brightness / contrast: modern is pigment's typed refusal (legacy has a GPU body since lane
    // SMB-PS-ADJUST-STYLES-2 and reaches the device: no_adapter here)
    {
        let use_legacy = false;
        let refused = run_ext(
            &backend,
            &adj_plan,
            &adj_ext(AdjustmentOp::BrightnessContrast(BrightnessContrast {
                brightness: 10.0,
                contrast: 10.0,
                use_legacy,
            })),
            &approx,
            &sources,
            &mut sink,
        );
        assert!(
            matches!(
                refused,
                Err(BackendError::Unsupported {
                    operator: "brightness_contrast",
                    ..
                })
            ),
            "legacy={use_legacy}: {refused:?}"
        );
        let legacy = run_ext(
            &backend,
            &adj_plan,
            &adj_ext(AdjustmentOp::BrightnessContrast(BrightnessContrast {
                brightness: 10.0,
                contrast: 10.0,
                use_legacy: true,
            })),
            &approx,
            &sources,
            &mut sink,
        );
        assert_eq!(legacy, Err(BackendError::Unavailable("no_adapter")));
    }
    // knockout at the root is admitted since lane SMB-PS-ADJUST-STYLES-3: without an adapter the
    // run ends at no_adapter, after admission (tests/gpu_knockout.rs renders it)
    for depth in [Knockout::Shallow, Knockout::Deep] {
        let knockout = PlanExtensions::from_layers(vec![
            LayerExtension::new(adj_plan.root, 1).with_knockout(depth),
        ]);
        assert_eq!(
            run_ext(&backend, &adj_plan, &knockout, &approx, &sources, &mut sink),
            Err(BackendError::Unavailable("no_adapter")),
            "{depth:?}"
        );
    }
    // an effect on an adjustment layer is pigment's structural refusal (never rendered without it)
    let effect = EffectOp::ColourOverlay(ColourOverlayOp::new(
        [1.0, 0.0, 0.0],
        StudioBlendMode::Normal,
        0.5,
    ));
    let effects = PlanExtensions::from_layers(vec![
        LayerExtension::new(adj_plan.root, 1)
            .with_adjustment(AdjustmentOp::Invert)
            .with_effects(vec![effect]),
    ]);
    assert!(matches!(
        run_ext(&backend, &adj_plan, &effects, &approx, &sources, &mut sink),
        Err(BackendError::InvalidPlan(
            PlanError::EffectOnAdjustment { .. }
        ))
    ));
    // feather without a mask is pigment's structural refusal
    let feather = PlanExtensions::from_layers(vec![
        LayerExtension::new(adj_plan.root, 0).with_mask_feather(2.0),
    ]);
    assert!(matches!(
        run_ext(&backend, &adj_plan, &feather, &approx, &sources, &mut sink),
        Err(BackendError::InvalidPlan(
            PlanError::FeatherWithoutMask { .. }
        ))
    ));
    // aggregate_over_budget (V4-02): the feather scratch is charged to max_working_bytes and a
    // blur whose cost exceeds the host bound is refused, both before any device work
    let masked_plan = plan_with(Layer {
        mask: Some(sources.masks[0].1.input(4, false)),
        ..layer(1, StudioBlendMode::Normal, 1.0, 1.0)
    });
    let feathered = PlanExtensions::from_layers(vec![
        LayerExtension::new(masked_plan.root, 1).with_mask_feather(10.0),
    ]);
    let mut starved = request(51, Exactness::Approximate);
    starved.max_working_bytes = 100;
    assert!(matches!(
        run_ext(
            &backend,
            &masked_plan,
            &feathered,
            &starved,
            &sources,
            &mut sink
        ),
        Err(BackendError::BudgetExceeded { limit: 100, .. })
    ));
    let big = rect(0, 0, 400, 300);
    let big_mask = OwnedMask::from_u8(big, vec![128; 400 * 300]);
    let big_sources = Sources {
        tiles: vec![],
        masks: vec![(4, big_mask)],
    };
    let big_plan = plan_of(
        52,
        big,
        vec![solid(big, [0.1, 0.2, 0.3, 1.0])],
        vec![Layer {
            mask: Some(big_sources.masks[0].1.input(4, false)),
            ..layer(0, StudioBlendMode::Normal, 1.0, 1.0)
        }],
    );
    let huge = PlanExtensions::from_layers(vec![
        LayerExtension::new(big_plan.root, 0).with_mask_feather(5000.0),
    ]);
    let mut big_sink = Image::new(big);
    assert!(matches!(
        run_ext(
            &backend,
            &big_plan,
            &huge,
            &request(52, Exactness::Approximate),
            &big_sources,
            &mut big_sink
        ),
        Err(BackendError::Unsupported {
            operator: "mask_feather_cost",
            ..
        })
    ));
    // nothing above touched the sink or the residency ledger
    assert_eq!((sink.accepted, big_sink.accepted), (0, 0));
    assert_eq!(backend.resident_bytes(), 0);
}

// ---- agreement with pigment's evaluate_stack (Blend-If on an adjustment layer and on a layer) -----

/// Per-pixel reference of one pixel of the first layer (a Normal tile over transparent) through
/// pigment's `evaluate_stack`, the backdrop the second layer sees.
fn stack_backdrop(tile: &OwnedTile, x: u32, y: u32, ctx: &BlendContext) -> Rgba {
    let p = tile.pixel(x, y);
    evaluate_stack(
        &[StackNode::Leaf {
            source: Rgba::new([p[0], p[1], p[2]], p[3]),
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
        }],
        Rgba::TRANSPARENT,
        ctx,
    )
    .expect("backdrop stack")
    .0
}

fn rgba_err(got: [f32; 4], want: Rgba) -> f32 {
    let mut e = (got[3] - want.a).abs();
    for (g, w) in got.iter().zip(want.rgb.iter()) {
        e = e.max((g - w).abs());
    }
    e
}

/// Agreement check (lead decision: pigment's `evaluate_stack` / `adjust_math` / `coverage_math` own
/// the Blend-If semantics, the GPU must match them). Case A: Blend-If on an adjustment layer. The
/// layer's content is `f(backdrop)` (pigment: "Content = f(backdrop)"), so its "This Layer" colour
/// is the adjusted colour and the weight is `blend_if_weight(entries, f(Cb), Cb)`; the reference
/// is `evaluate_stack` with an `Adjustment` node at `opacity * fill * weight`. Case B: Blend-If on
/// a plain layer: `evaluate_stack` with a `Leaf` at `opacity * fill * weight(source, Cb)`.
#[test]
fn ext_blend_if_agrees_with_pigment_evaluate_stack() {
    const TOL: f32 = 5.0e-6;
    let _guard = device_lock();
    let backend = shared_backend();
    let Some(info) = gpu_gate(backend, "Blend-If agreement with pigment evaluate_stack")
        .unwrap_or_else(|e| panic!("{e}"))
    else {
        return;
    };
    eprintln!("GPU_BACKEND: {} (Blend-If vs evaluate_stack)", info.text());
    let ctx = BlendContext {
        space: BlendSpace::LinearLight,
        site: PixelSite::default(),
    };
    let entry = |channel, source, destination| BlendRangeEntry {
        channel,
        source,
        destination,
    };
    let extent = rect(0, 0, 48, 6);
    let back = OwnedTile::random(48, 6, 201);
    let top = OwnedTile::random(48, 6, 202);
    let sources = Sources::with_tiles(vec![(1, back), (2, top)]);
    let entries_sets = [
        vec![
            entry(
                BlendChannel::Gray,
                [0.2, 0.4, 0.6, 0.8],
                [0.0, 0.0, 0.75, 0.9],
            ),
            entry(BlendChannel::Red, IDENTITY_RANGE, [0.25, 0.5, 1.0, 1.0]),
        ],
        vec![entry(
            BlendChannel::Blue,
            [0.1, 0.3, 0.7, 0.95],
            [0.3, 0.5, 0.5, 0.7],
        )],
    ];

    // Case A: adjustment layers.
    let adjustments = [
        (
            AdjustmentOp::Invert,
            StudioBlendMode::Normal,
            0.8_f32,
            1.0_f32,
        ),
        (
            AdjustmentOp::Levels(Levels {
                composite: LevelsRecord {
                    input_black: 0.1,
                    input_white: 0.9,
                    gamma: 2.0,
                    output_black: 0.05,
                    output_white: 0.95,
                },
                red: LevelsRecord::IDENTITY,
                green: LevelsRecord::IDENTITY,
                blue: LevelsRecord::IDENTITY,
            }),
            StudioBlendMode::Multiply,
            0.7,
            0.9,
        ),
        (
            AdjustmentOp::HueSaturation(HueSaturation {
                hue: 60.0,
                saturation: 30.0,
                lightness: -10.0,
                colorize: false,
            }),
            StudioBlendMode::Normal,
            1.0,
            1.0,
        ),
    ];
    let mut worst = 0.0_f32;
    let mut partial_weights = 0_usize;
    for (n, (op, mode, opacity, fill)) in adjustments.iter().enumerate() {
        for (set_index, entries) in entries_sets.iter().enumerate() {
            let rev = 61 + (n * 2 + set_index) as u64;
            let plan = plan_of(
                rev,
                extent,
                vec![
                    sources.tiles[0].1.input(1, extent),
                    solid(extent, [0.0; 4]), // the adjustment placeholder
                ],
                vec![
                    layer(0, StudioBlendMode::Normal, 1.0, 1.0),
                    layer(1, *mode, *opacity, *fill),
                ],
            );
            let ext = PlanExtensions::from_layers(vec![
                LayerExtension::new(plan.root, 1)
                    .with_adjustment(op.clone())
                    .with_blend_if(entries.clone()),
            ]);
            let mut sink = Image::new(extent);
            let receipt = run_ext(
                backend,
                &plan,
                &ext,
                &request(rev, Exactness::Approximate),
                &sources,
                &mut sink,
            )
            .expect("adjustment with Blend-If");
            assert!(has_operator(&receipt, "blend_if", Exactness::Approximate));
            let prepared = op.prepare().expect("prepared adjustment");
            let mut case_worst = 0.0_f32;
            for y in 0..extent.height {
                for x in 0..extent.width {
                    let cb = stack_backdrop(&sources.tiles[0].1, x, y, &ctx);
                    let adjusted = if cb.a > 0.0 {
                        prepared.apply(cb.rgb).expect("adjusted colour")
                    } else {
                        cb.rgb
                    };
                    let w = blend_if_weight_over(entries, adjusted, cb.rgb, cb.a).expect("weight");
                    if cb.a > 0.0 && w > 0.0 && w < 1.0 {
                        partial_weights += 1;
                    }
                    let k = opacity * fill * w;
                    let p = sources.tiles[0].1.pixel(x, y);
                    let want = evaluate_stack(
                        &[
                            StackNode::Leaf {
                                source: Rgba::new([p[0], p[1], p[2]], p[3]),
                                mode: StudioBlendMode::Normal,
                                opacity: 1.0,
                            },
                            StackNode::Adjustment {
                                op: op.clone(),
                                mode: *mode,
                                opacity: k,
                            },
                        ],
                        Rgba::TRANSPARENT,
                        &ctx,
                    )
                    .expect("evaluate_stack adjustment")
                    .0;
                    let got = sink.px[(y * extent.width + x) as usize];
                    case_worst = case_worst.max(rgba_err(got, want));
                }
            }
            eprintln!(
                "EXT_BLEND_IF_STACK adjustment {} set {set_index}: max_err {case_worst:e} (tolerance {TOL:e})",
                op.kind().key()
            );
            assert!(
                case_worst <= TOL,
                "adjustment {} set {set_index}: {case_worst:e}",
                op.kind().key()
            );
            worst = worst.max(case_worst);
        }
    }
    assert!(
        partial_weights > 20,
        "the Blend-If weights must be partial somewhere ({partial_weights} texels)"
    );

    // Case B: a plain layer with Blend-If against an evaluate_stack Leaf.
    for (set_index, entries) in entries_sets.iter().enumerate() {
        let rev = 80 + set_index as u64;
        let plan = plan_of(
            rev,
            extent,
            vec![
                sources.tiles[0].1.input(1, extent),
                sources.tiles[1].1.input(2, extent),
            ],
            vec![
                layer(0, StudioBlendMode::Normal, 1.0, 1.0),
                layer(1, StudioBlendMode::Multiply, 0.8, 0.9),
            ],
        );
        let ext = PlanExtensions::from_layers(vec![
            LayerExtension::new(plan.root, 1).with_blend_if(entries.clone()),
        ]);
        let mut sink = Image::new(extent);
        run_ext(
            backend,
            &plan,
            &ext,
            &request(rev, Exactness::Approximate),
            &sources,
            &mut sink,
        )
        .expect("layer with Blend-If");
        let mut case_worst = 0.0_f32;
        for y in 0..extent.height {
            for x in 0..extent.width {
                let cb = stack_backdrop(&sources.tiles[0].1, x, y, &ctx);
                let s = sources.tiles[1].1.pixel(x, y);
                let w = blend_if_weight_over(entries, [s[0], s[1], s[2]], cb.rgb, cb.a).expect("weight");
                let want = evaluate_stack(
                    &[
                        StackNode::Leaf {
                            source: Rgba::new(cb.rgb, cb.a),
                            mode: StudioBlendMode::Normal,
                            opacity: 1.0,
                        },
                        StackNode::Leaf {
                            source: Rgba::new([s[0], s[1], s[2]], s[3]),
                            mode: StudioBlendMode::Multiply,
                            opacity: 0.8 * 0.9 * w,
                        },
                    ],
                    Rgba::TRANSPARENT,
                    &ctx,
                )
                .expect("evaluate_stack leaf")
                .0;
                let got = sink.px[(y * extent.width + x) as usize];
                case_worst = case_worst.max(rgba_err(got, want));
            }
        }
        eprintln!(
            "EXT_BLEND_IF_STACK layer set {set_index}: max_err {case_worst:e} (tolerance {TOL:e})"
        );
        assert!(case_worst <= TOL, "layer set {set_index}: {case_worst:e}");
        worst = worst.max(case_worst);
    }
    eprintln!("EXT_BLEND_IF_STACK worst {worst:e} (tolerance {TOL:e})");
    assert_eq!(backend.resident_bytes(), 0);
}

// ---- layer effects (pigment effect_math), parity against render_layer_reference --------------

/// A soft-edged disc with a hole: alpha ramps over one pixel around 0.5 at the rim, so stroke and
/// inner-effect decisions (alpha >= 0.5) see fractional values; colour varies across the tile.
fn blob(width: u32, height: u32) -> OwnedTile {
    OwnedTile::from_pixels(width, height, |x, y| {
        let cx = (width as f32 - 1.0) / 2.0;
        let cy = (height as f32 - 1.0) / 2.0;
        let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
        let outer = width.min(height) as f32 / 2.0 - 0.5;
        let mut a = (outer - d + 0.5).clamp(0.0, 1.0);
        if d < 1.8 {
            a = 0.0; // a hole, so inner effects see a second edge
        }
        [x as f32 / width as f32, y as f32 / height as f32, 0.55, a]
    })
}

/// A translucent backdrop (alpha > 0 everywhere, so the straight colour is meaningful).
fn translucent_backdrop(width: u32, height: u32) -> OwnedTile {
    OwnedTile::from_pixels(width, height, |x, y| {
        [
            0.9 - x as f32 / (2.0 * width as f32),
            0.2 + y as f32 / (2.0 * height as f32),
            0.35,
            0.45 + 0.5 * ((x + y) % 5) as f32 / 4.0,
        ]
    })
}

/// A reference plane of `extent` holding `tile` at `at` (optionally times a plain mask coverage)
/// over transparent.
fn effect_plane_of(
    extent: Rect,
    tile: &OwnedTile,
    at: Rect,
    mask: Option<&OwnedMask>,
) -> EffectPlane {
    let (w, h) = (extent.width, extent.height);
    let mut rgb = vec![[0.0_f32; 3]; (w * h) as usize];
    let mut alpha = vec![0.0_f32; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let (gx, gy) = (i64::from(x) + extent.x, i64::from(y) + extent.y);
            if !inside(at, gx, gy) {
                continue;
            }
            let p = tile.pixel((gx - at.x) as u32, (gy - at.y) as u32);
            let m = mask.map_or(1.0, |m| m.coverage(gx, gy, false));
            let i = (y * w + x) as usize;
            rgb[i] = [p[0], p[1], p[2]];
            alpha[i] = p[3] * m;
        }
    }
    EffectPlane::new(w, h, rgb, alpha).expect("reference plane")
}

/// Largest difference between a rendered image and a reference plane: alpha and premultiplied
/// colour (a straight colour is not meaningful where the alpha is tiny).
fn plane_error(image: &Image, plane: &EffectPlane) -> (f32, (u32, u32)) {
    let (w, h) = (plane.width(), plane.height());
    let (mut worst, mut at) = (0.0_f32, (0, 0));
    for y in 0..h {
        for x in 0..w {
            let got = image.px[(y * w + x) as usize];
            let want = plane.pixel(x, y).expect("plane pixel");
            let mut e = (got[3] - want.a).abs();
            for c in 0..3 {
                e = e.max((got[c] * got[3] - want.rgb[c] * want.a).abs());
            }
            assert!(e.is_finite(), "non-finite output at ({x},{y})");
            if e > worst {
                worst = e;
                at = (x, y);
            }
        }
    }
    (worst, at)
}

/// Pixels where two reference planes differ by more than 1e-3 in alpha or premultiplied colour: an
/// effect case that changes nothing would pass any parity check, so each case asserts it changes
/// the bare layer (or, for an empty matte, nothing).
fn planes_differ(a: &EffectPlane, b: &EffectPlane) -> usize {
    let mut n = 0;
    for y in 0..a.height() {
        for x in 0..a.width() {
            let (p, q) = (a.pixel(x, y).expect("pixel"), b.pixel(x, y).expect("pixel"));
            let mut e = (p.a - q.a).abs();
            for (u, v) in p.rgb.iter().zip(q.rgb.iter()) {
                e = e.max((u * p.a - v * q.a).abs());
            }
            if e > 1.0e-3 {
                n += 1;
            }
        }
    }
    n
}

/// One effect parity case on the device: `layer` over `backdrop`, rendered through the envelope and
/// compared with pigment's `render_layer_reference` on a plane that holds the layer plus its margin.
#[allow(clippy::too_many_arguments)]
fn fx_vs_reference(
    backend: &GpuBackend,
    rev: u64,
    extent: Rect,
    backdrop: &OwnedTile,
    content: &OwnedTile,
    at: Rect,
    mask: Option<OwnedMask>,
    params: (StudioBlendMode, f32, f32),
    ops: &[EffectOp],
) -> (
    f32,
    (u32, u32),
    hsk_studio_pigment::render_plan::RenderReceipt,
    usize,
) {
    let (mode, opacity, fill) = params;
    let back_plane = effect_plane_of(extent, backdrop, extent, None);
    let content_plane = effect_plane_of(extent, content, at, mask.as_ref());
    let bare = render_layer_reference(
        &back_plane,
        &content_plane,
        RefLayerParams {
            mode,
            opacity,
            fill_opacity: fill,
        },
        &[],
    )
    .expect("reference without effects");
    let want = render_layer_reference(
        &back_plane,
        &content_plane,
        RefLayerParams {
            mode,
            opacity,
            fill_opacity: fill,
        },
        ops,
    )
    .expect("reference render");
    let mut top = layer(1, mode, opacity, fill);
    let mut sources = Sources::with_tiles(vec![(1, backdrop.clone()), (2, content.clone())]);
    if let Some(m) = mask {
        top.mask = Some(m.input(7, false));
        sources.masks.push((7, m));
    }
    let plan = plan_of(
        rev,
        extent,
        vec![backdrop.input(1, extent), content.input(2, at)],
        vec![layer(0, StudioBlendMode::Normal, 1.0, 1.0), top],
    );
    let ext = PlanExtensions::from_layers(vec![
        LayerExtension::new(plan.root, 1).with_effects(ops.to_vec()),
    ]);
    let mut sink = Image::new(extent);
    let receipt = run_ext(
        backend,
        &plan,
        &ext,
        &request(rev, Exactness::Approximate),
        &sources,
        &mut sink,
    )
    .expect("effects execute on the device");
    let (err, worst) = plane_error(&sink, &want);
    (err, worst, receipt, planes_differ(&want, &bare))
}

fn black() -> [f32; 3] {
    [0.0, 0.0, 0.0]
}

/// A named effect stack with the layer's (mode, opacity, fill).
type FxCase = (&'static str, Vec<EffectOp>, (StudioBlendMode, f32, f32));

#[test]
fn fx_wave1_parity_vs_pigment_render_layer_reference() {
    const TOL: f32 = 1.0e-5;
    let _guard = device_lock();
    let backend = shared_backend();
    let Some(info) = gpu_gate(backend, "effects wave 1 parity").unwrap_or_else(|e| panic!("{e}"))
    else {
        return;
    };
    eprintln!("GPU_BACKEND: {} (effects wave 1)", info.text());
    // The plane holds the layer plus a margin of 14 px (every case reads at most 13).
    let extent = rect(0, 0, 48, 44);
    let at = rect(15, 14, 18, 16);
    let content = blob(18, 16);
    let backdrop = translucent_backdrop(48, 44);
    let normal = StudioBlendMode::Normal;
    let cases: Vec<FxCase> = vec![
        (
            "drop shadow size 6 spread 0.3 angle 120 distance 4",
            vec![EffectOp::DropShadow(
                DropShadowOp::new(black(), normal, 0.75)
                    .with_offset(120.0, 4.0)
                    .with_spread(0.3)
                    .with_size(6.0),
            )],
            (normal, 1.0, 1.0),
        ),
        (
            "drop shadow knockout, fill 0.4, hard offset",
            vec![EffectOp::DropShadow(
                DropShadowOp::new([0.1, 0.0, 0.3], normal, 0.9)
                    .with_offset(180.0, 3.0)
                    .with_layer_knocks_out(true),
            )],
            (normal, 0.9, 0.4),
        ),
        (
            "drop shadow pure blur 10 multiply",
            vec![EffectOp::DropShadow(
                DropShadowOp::new([0.3, 0.1, 0.0], StudioBlendMode::Multiply, 0.8).with_size(10.0),
            )],
            (normal, 1.0, 1.0),
        ),
        // Lane SMB-PS-ORACLE-PARITY-1 (STUDIO-R36 remediation): a shadow selecting the Gaussian
        // kernel (Figma-sourced) runs the feather taps on the GPU like pigment's reference.
        (
            "gaussian drop shadow size 6 distance 3 and gaussian inner shadow size 4",
            vec![
                EffectOp::DropShadow(
                    DropShadowOp::new(black(), normal, 0.6)
                        .with_offset(180.0, 3.0)
                        .with_size(6.0)
                        .with_blur_kernel(EffectBlurKernel::Gaussian),
                ),
                EffectOp::InnerShadow(
                    InnerShadowOp::new([0.2, 0.0, 0.4], normal, 0.7)
                        .with_offset(90.0, 2.0)
                        .with_size(4.0)
                        .with_blur_kernel(EffectBlurKernel::Gaussian),
                ),
            ],
            (normal, 1.0, 1.0),
        ),
        (
            "colour overlay 50% at fill 0 (full strength)",
            vec![EffectOp::ColourOverlay(ColourOverlayOp::new(
                [1.0, 0.0, 0.0],
                normal,
                0.5,
            ))],
            (normal, 1.0, 0.0),
        ),
        (
            "stroke outside 3 / inside 3 / centre 5",
            vec![
                EffectOp::Stroke(StrokeOp::new(
                    [1.0, 1.0, 0.0],
                    normal,
                    0.8,
                    3.0,
                    StrokePosition::Outside,
                )),
                EffectOp::Stroke(StrokeOp::new(
                    [0.0, 0.5, 1.0],
                    normal,
                    0.8,
                    3.0,
                    StrokePosition::Inside,
                )),
                EffectOp::Stroke(StrokeOp::new(
                    [0.0, 1.0, 0.0],
                    normal,
                    0.6,
                    5.0,
                    StrokePosition::Centre,
                )),
            ],
            (normal, 1.0, 1.0),
        ),
        (
            "stroke size 0 is an empty matte",
            vec![EffectOp::Stroke(StrokeOp::new(
                [1.0, 1.0, 0.0],
                normal,
                0.8,
                0.0,
                StrokePosition::Centre,
            ))],
            (normal, 1.0, 1.0),
        ),
        (
            "layer opacity 0 draws nothing (effects fade with it)",
            vec![EffectOp::DropShadow(
                DropShadowOp::new(black(), normal, 0.75)
                    .with_offset(120.0, 4.0)
                    .with_size(6.0),
            )],
            (normal, 0.0, 1.0),
        ),
    ];
    let mut worst = 0.0_f32;
    for (n, (name, ops, params)) in cases.iter().enumerate() {
        let (err, at_px, receipt, changed) = fx_vs_reference(
            backend,
            100 + n as u64,
            extent,
            &backdrop,
            &content,
            at,
            None,
            *params,
            ops,
        );
        eprintln!(
            "FX_WAVE1 {name}: max_err {err:e} at {at_px:?}, {changed} texels differ from the bare layer (tolerance {TOL:e})"
        );
        assert!(err <= TOL, "{name}: {err:e} > {TOL:e}");
        if name.contains("empty matte") || name.contains("draws nothing") {
            assert_eq!(changed, 0, "{name}: draws nothing");
        } else {
            assert!(changed > 10, "{name}: the effect changed {changed} texels");
        }
        for op in ops {
            // pigment's declared class travels; the backend never reports a better one
            assert!(
                has_operator(&receipt, op.kind().key(), Exactness::Approximate),
                "{name}: {:?}",
                receipt.exactness
            );
        }
        worst = worst.max(err);
    }
    eprintln!("FX_WAVE1 worst {worst:e} (tolerance {TOL:e})");
    assert_eq!(backend.resident_bytes(), 0);
}

#[test]
fn fx_wave2_parity_vs_pigment_render_layer_reference() {
    const TOL: f32 = 1.0e-5;
    let _guard = device_lock();
    let backend = shared_backend();
    let Some(info) = gpu_gate(backend, "effects wave 2 parity").unwrap_or_else(|e| panic!("{e}"))
    else {
        return;
    };
    eprintln!("GPU_BACKEND: {} (effects wave 2)", info.text());
    let extent = rect(0, 0, 48, 44);
    let at = rect(15, 14, 18, 16);
    let content = blob(18, 16);
    let backdrop = translucent_backdrop(48, 44);
    let normal = StudioBlendMode::Normal;
    let cases: Vec<(&str, EffectOp)> = vec![
        (
            "inner shadow size 6 choke 0.4 angle -45 distance 3",
            EffectOp::InnerShadow(
                InnerShadowOp::new(black(), normal, 0.75)
                    .with_offset(-45.0, 3.0)
                    .with_choke(0.4)
                    .with_size(6.0),
            ),
        ),
        (
            "inner shadow hard (size 0)",
            EffectOp::InnerShadow(
                InnerShadowOp::new([0.2, 0.0, 0.4], StudioBlendMode::Multiply, 0.9)
                    .with_offset(90.0, 2.0),
            ),
        ),
        (
            "outer glow size 8 spread 0.25",
            EffectOp::OuterGlow(
                OuterGlowOp::new([1.0, 0.9, 0.3], StudioBlendMode::Screen, 0.8)
                    .with_spread(0.25)
                    .with_size(8.0),
            ),
        ),
        (
            "inner glow edge size 5 choke 0.2",
            EffectOp::InnerGlow(
                InnerGlowOp::new([1.0, 1.0, 0.8], normal, 0.7)
                    .with_choke(0.2)
                    .with_size(5.0),
            ),
        ),
        (
            "inner glow centre size 6",
            EffectOp::InnerGlow(
                InnerGlowOp::new([0.6, 1.0, 0.8], normal, 0.7)
                    .with_size(6.0)
                    .with_source(GlowSource::Centre),
            ),
        ),
    ];
    let mut worst = 0.0_f32;
    for (n, (name, op)) in cases.iter().enumerate() {
        let (err, at_px, receipt, changed) = fx_vs_reference(
            backend,
            120 + n as u64,
            extent,
            &backdrop,
            &content,
            at,
            None,
            (normal, 1.0, 1.0),
            std::slice::from_ref(op),
        );
        eprintln!(
            "FX_WAVE2 {name}: max_err {err:e} at {at_px:?}, {changed} texels differ from the bare layer (tolerance {TOL:e})"
        );
        assert!(err <= TOL, "{name}: {err:e} > {TOL:e}");
        assert!(changed > 10, "{name}: the effect changed {changed} texels");
        assert!(has_operator(
            &receipt,
            op.kind().key(),
            Exactness::Approximate
        ));
        worst = worst.max(err);
    }
    eprintln!("FX_WAVE2 worst {worst:e} (tolerance {TOL:e})");
    assert_eq!(backend.resident_bytes(), 0);
}

/// Composition rules of a layer with effects: all six kinds in one stack (behind effects before
/// the content, the rest over it, list order), layer opacity versus fill, a layer mask on the
/// effect layer, an isolated group as the effect layer's content, and a nested layer with effects
/// inside that group (the frame margin is the sum along the nesting). Each case equals pigment's
/// `render_layer_reference` on a plane that holds the layer plus its margins.
#[test]
fn fx_stack_mask_group_and_nesting_parity() {
    const TOL: f32 = 1.0e-5;
    let _guard = device_lock();
    let backend = shared_backend();
    let Some(info) =
        gpu_gate(backend, "effects composition parity").unwrap_or_else(|e| panic!("{e}"))
    else {
        return;
    };
    eprintln!("GPU_BACKEND: {} (effects composition)", info.text());
    let extent = rect(0, 0, 48, 44);
    let at = rect(15, 14, 18, 16);
    let content = blob(18, 16);
    let backdrop = translucent_backdrop(48, 44);
    let normal = StudioBlendMode::Normal;
    let stack = vec![
        EffectOp::DropShadow(
            DropShadowOp::new(black(), StudioBlendMode::Multiply, 0.8)
                .with_offset(135.0, 3.0)
                .with_spread(0.2)
                .with_size(5.0),
        ),
        EffectOp::OuterGlow(
            OuterGlowOp::new([1.0, 0.8, 0.2], StudioBlendMode::Screen, 0.6).with_size(4.0),
        ),
        EffectOp::ColourOverlay(ColourOverlayOp::new(
            [0.2, 0.4, 1.0],
            StudioBlendMode::SoftLight,
            0.5,
        )),
        EffectOp::Stroke(StrokeOp::new(
            [1.0, 1.0, 1.0],
            normal,
            0.9,
            2.0,
            StrokePosition::Outside,
        )),
        EffectOp::InnerShadow(
            InnerShadowOp::new(black(), normal, 0.5)
                .with_offset(45.0, 2.0)
                .with_choke(0.1)
                .with_size(3.0),
        ),
        EffectOp::InnerGlow(InnerGlowOp::new([1.0, 0.5, 0.5], normal, 0.5).with_size(3.0)),
    ];
    let mut worst = 0.0_f32;

    // (a) the whole stack, layer opacity 0.8 and fill 0.5 (effects fade with the opacity only)
    let (err, at_px, receipt, changed) = fx_vs_reference(
        backend,
        140,
        extent,
        &backdrop,
        &content,
        at,
        None,
        (normal, 0.8, 0.5),
        &stack,
    );
    eprintln!(
        "FX_STACK six kinds opacity 0.8 fill 0.5: max_err {err:e} at {at_px:?}, {changed} texels differ from the bare layer"
    );
    assert!(err <= TOL, "stack: {err:e}");
    assert!(changed > 200, "the stack changed {changed} texels");
    for op in &stack {
        assert!(has_operator(
            &receipt,
            op.kind().key(),
            Exactness::Approximate
        ));
    }
    worst = worst.max(err);

    // (b) a coverage mask on the effect layer multiplies the content alpha the mattes derive from
    let mask = OwnedMask::from_u8(
        extent,
        (0..48 * 44)
            .map(|i| {
                let (x, y) = (i % 48, i / 48);
                if (x + y) % 7 < 2 { 90_u8 } else { 255 }
            })
            .collect(),
    );
    let (err, at_px, _, changed) = fx_vs_reference(
        backend,
        141,
        extent,
        &backdrop,
        &content,
        at,
        Some(mask),
        (normal, 1.0, 1.0),
        &stack[..4],
    );
    eprintln!(
        "FX_STACK masked layer: max_err {err:e} at {at_px:?}, {changed} texels differ from the bare layer"
    );
    assert!(err <= TOL, "masked stack: {err:e}");
    assert!(changed > 100, "the masked stack changed {changed} texels");
    worst = worst.max(err);

    // (c) the effect layer's content is an isolated group of two tiles (Normal, then Multiply),
    // and (d) the same group holds a layer with its own effects: outer margin + inner margin.
    let tile_a = blob(18, 16);
    let tile_b = OwnedTile::from_pixels(12, 10, |x, y| {
        [0.2 + x as f32 / 20.0, 0.7, 0.3 + y as f32 / 20.0, 0.8]
    });
    let at_b = rect(18, 17, 12, 10);
    let outer = vec![
        EffectOp::DropShadow(
            DropShadowOp::new(black(), normal, 0.7)
                .with_offset(90.0, 2.0)
                .with_size(4.0),
        ),
        EffectOp::Stroke(StrokeOp::new(
            [1.0, 0.0, 0.0],
            normal,
            0.8,
            2.0,
            StrokePosition::Outside,
        )),
    ];
    let inner = vec![EffectOp::OuterGlow(
        OuterGlowOp::new([0.0, 1.0, 0.5], normal, 0.9).with_size(4.0),
    )];
    let sources = Sources::with_tiles(vec![
        (1, backdrop.clone()),
        (2, tile_a.clone()),
        (3, tile_b.clone()),
    ]);
    // reference group result: tile A, then tile B Multiply 0.9 over it, each over transparent
    let ctx_site = PixelSite::default();
    let group_plane = |with_inner: bool| -> EffectPlane {
        let mut rgb = vec![[0.0_f32; 3]; 48 * 44];
        let mut alpha = vec![0.0_f32; 48 * 44];
        for y in 0..44_i64 {
            for x in 0..48_i64 {
                let mut px = Rgba::TRANSPARENT;
                if inside(at, x, y) {
                    let p = tile_a.pixel((x - at.x) as u32, (y - at.y) as u32);
                    px = composite_over(
                        StudioBlendMode::Normal,
                        px,
                        Rgba::new([p[0], p[1], p[2]], p[3]),
                        1.0,
                        ctx_site,
                    )
                    .expect("tile A");
                }
                if inside(at_b, x, y) {
                    let p = tile_b.pixel((x - at_b.x) as u32, (y - at_b.y) as u32);
                    px = composite_over(
                        StudioBlendMode::Multiply,
                        px,
                        Rgba::new([p[0], p[1], p[2]], p[3]),
                        0.9,
                        ctx_site,
                    )
                    .expect("tile B");
                }
                let i = (y * 48 + x) as usize;
                rgb[i] = px.rgb;
                alpha[i] = px.a.min(1.0);
            }
        }
        let plane = EffectPlane::new(48, 44, rgb, alpha).expect("group plane");
        if !with_inner {
            return plane;
        }
        // the group is [layer with the inner effects over transparent]; its result is that layer
        render_layer_reference(
            &EffectPlane::transparent(48, 44).expect("transparent"),
            &plane,
            RefLayerParams {
                mode: normal,
                opacity: 1.0,
                fill_opacity: 1.0,
            },
            &inner,
        )
        .expect("inner effects")
    };
    let back_plane = effect_plane_of(extent, &backdrop, extent, None);
    for (rev, nested) in [(142_u64, false), (143, true)] {
        let want = render_layer_reference(
            &back_plane,
            &group_plane(nested),
            RefLayerParams {
                mode: normal,
                opacity: 1.0,
                fill_opacity: 1.0,
            },
            &outer,
        )
        .expect("reference render");
        // nodes: 0 backdrop, 1 tile A, 2 tile B, 3 group (A, B multiply 0.9), 4 group holding
        // layer 3 with the inner effects (nested case) / the plain group (flat case)
        let mut nodes = vec![
            backdrop.input(1, extent),
            tile_a.input(2, at),
            tile_b.input(3, at_b),
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![
                    layer(1, normal, 1.0, 1.0),
                    layer(2, StudioBlendMode::Multiply, 0.9, 1.0),
                ],
            }),
        ];
        let (content_node, mut ext_layers) = if nested {
            nodes.push(PlanNode::Group(Group {
                isolated: true,
                layers: vec![layer(3, normal, 1.0, 1.0)],
            }));
            (
                4_u32,
                vec![LayerExtension::new(PlanNodeId(4), 0).with_effects(inner.clone())],
            )
        } else {
            (3_u32, Vec::new())
        };
        let plan = plan_of(
            rev,
            extent,
            nodes,
            vec![
                layer(0, normal, 1.0, 1.0),
                layer(content_node, normal, 1.0, 1.0),
            ],
        );
        ext_layers.push(LayerExtension::new(plan.root, 1).with_effects(outer.clone()));
        let ext = PlanExtensions::from_layers(ext_layers);
        let mut sink = Image::new(extent);
        run_ext(
            backend,
            &plan,
            &ext,
            &request(rev, Exactness::Approximate),
            &sources,
            &mut sink,
        )
        .expect("group content with effects");
        let (err, at_px) = plane_error(&sink, &want);
        let bare = render_layer_reference(
            &back_plane,
            &group_plane(nested),
            RefLayerParams {
                mode: normal,
                opacity: 1.0,
                fill_opacity: 1.0,
            },
            &[],
        )
        .expect("bare group layer");
        let changed = planes_differ(&want, &bare);
        eprintln!(
            "FX_STACK group content (nested inner effects: {nested}): max_err {err:e} at {at_px:?}, {changed} texels differ from the bare layer"
        );
        assert!(err <= TOL, "group content nested={nested}: {err:e}");
        assert!(changed > 100, "group content changed {changed} texels");
        worst = worst.max(err);
    }
    eprintln!("FX_STACK worst {worst:e} (tolerance {TOL:e})");
    assert_eq!(backend.resident_bytes(), 0);

    // (e) a starved working-byte limit is refused by the frame sizing before any dispatch
    let plan = plan_of(
        150,
        extent,
        vec![backdrop.input(1, extent), content.input(2, at)],
        vec![layer(0, normal, 1.0, 1.0), layer(1, normal, 1.0, 1.0)],
    );
    let ext = PlanExtensions::from_layers(vec![
        LayerExtension::new(plan.root, 1).with_effects(stack.clone()),
    ]);
    let fx_sources = Sources::with_tiles(vec![(1, backdrop.clone()), (2, content.clone())]);
    let mut starved = request(150, Exactness::Approximate);
    starved.max_working_bytes = 20_000;
    let mut sink = Image::new(extent);
    let refused = run_ext(backend, &plan, &ext, &starved, &fx_sources, &mut sink);
    assert!(
        matches!(
            refused,
            Err(BackendError::BudgetExceeded { limit: 20_000, .. })
        ),
        "{refused:?}"
    );
    assert_eq!(sink.accepted, 0);
    assert_eq!(backend.resident_bytes(), 0);
    // hostile size: a 30000 px shadow distance (pigment's largest) needs a frame margin no budget
    // holds; it is a typed refusal before any dispatch, not an overflow or a wrapped size
    let far =
        PlanExtensions::from_layers(vec![LayerExtension::new(plan.root, 1).with_effects(vec![
            EffectOp::DropShadow(
                DropShadowOp::new(black(), normal, 0.5).with_offset(0.0, 30_000.0),
            ),
        ])]);
    let refused = run_ext(
        backend,
        &plan,
        &far,
        &request(150, Exactness::Approximate),
        &fx_sources,
        &mut sink,
    );
    assert!(
        matches!(refused, Err(BackendError::BudgetExceeded { .. })),
        "{refused:?}"
    );
    assert_eq!(sink.accepted, 0);
    assert_eq!(backend.resident_bytes(), 0);
    // an effect whose window costs more than one pass may (grow 1000 px: a 2001 x 2001 window) is
    // refused as Unsupported{effect_cost} before any dispatch
    let heavy = PlanExtensions::from_layers(vec![LayerExtension::new(plan.root, 1).with_effects(
        vec![EffectOp::OuterGlow(
            OuterGlowOp::new([1.0, 0.0, 0.0], normal, 0.5)
                .with_spread(1.0)
                .with_size(1000.0),
        )],
    )]);
    let refused = run_ext(
        backend,
        &plan,
        &heavy,
        &request(150, Exactness::Approximate),
        &fx_sources,
        &mut sink,
    );
    assert!(
        matches!(
            refused,
            Err(BackendError::Unsupported {
                operator: "effect_cost",
                ..
            })
        ),
        "{refused:?}"
    );
    assert_eq!(sink.accepted, 0);
    assert_eq!(backend.resident_bytes(), 0);
}

/// Hardware-independent: effect capability classes, shared admission and the backend-side
/// refusals happen before any device work.
#[test]
fn fx_admission_and_refusals_run_without_adapter() {
    let backend = no_adapter();
    let extent = rect(0, 0, 8, 8);
    let back = OwnedTile::random(8, 8, 3);
    let sources = Sources::with_tiles(vec![(1, back)]);
    let approx = request(71, Exactness::Approximate);
    let plan = plan_of(
        71,
        extent,
        vec![
            sources.tiles[0].1.input(1, extent),
            solid(extent, [0.4, 0.5, 0.6, 1.0]),
        ],
        vec![
            layer(0, StudioBlendMode::Normal, 1.0, 1.0),
            layer(1, StudioBlendMode::Normal, 1.0, 1.0),
        ],
    );
    let mut sink = Image::new(extent);
    let overlay = EffectOp::ColourOverlay(ColourOverlayOp::new(
        [1.0, 0.0, 0.0],
        StudioBlendMode::Normal,
        0.5,
    ));

    // capability: the six kinds of waves 1 and 2 are supported and report pigment's class, never
    // a better one (GPU_REFUSED_EFFECTS: none since lane SMB-PS-ADJUST-STYLES-2)
    for kind in EffectKind::ALL {
        let op = ExtOperator::Effect(kind);
        if GPU_REFUSED_EFFECTS.contains(&kind) {
            assert_eq!(backend.extension_exactness(op), Exactness::Unsupported, "{}", kind.key());
            continue;
        }
        assert_eq!(
            backend.extension_exactness(op),
            Exactness::Approximate,
            "{}",
            kind.key()
        );
        assert!(
            backend.extension_exactness(op).rank() >= op.intrinsic().rank(),
            "{}: reported class is better than pigment's",
            kind.key()
        );
    }
    // every supported envelope operator reports at least pigment's class
    for kind in AdjustmentKind::ALL {
        let op = ExtOperator::Adjustment(kind);
        let got = backend.extension_exactness(op);
        if got != Exactness::Unsupported {
            assert!(got.rank() >= op.intrinsic().rank(), "{}", kind.key());
        }
    }

    let with_effects = |ops: Vec<EffectOp>| {
        PlanExtensions::from_layers(vec![LayerExtension::new(plan.root, 1).with_effects(ops)])
    };
    // class Approximate: a Fitted floor refuses the effect, the Approximate floor reaches the device
    assert_eq!(
        run_ext(
            &backend,
            &plan,
            &with_effects(vec![overlay]),
            &request(71, Exactness::Fitted),
            &sources,
            &mut sink
        ),
        Err(BackendError::Unsupported {
            operator: "colour_overlay",
            exactness: Exactness::Approximate
        })
    );
    assert_eq!(
        run_ext(
            &backend,
            &plan,
            &with_effects(vec![overlay]),
            &approx,
            &sources,
            &mut sink
        ),
        Err(BackendError::Unavailable("no_adapter"))
    );
    // an effect blend mode the backend has no body for is refused as that mode
    let dissolve = EffectOp::ColourOverlay(ColourOverlayOp::new(
        [1.0, 0.0, 0.0],
        StudioBlendMode::Dissolve,
        0.5,
    ));
    assert!(matches!(
        run_ext(
            &backend,
            &plan,
            &with_effects(vec![dissolve]),
            &approx,
            &sources,
            &mut sink
        ),
        Err(BackendError::Unsupported {
            operator: "dissolve",
            ..
        })
    ));
    // pigment's own typed refusals are kept: a gradient glow is never drawn as a flat colour
    let gradient = EffectOp::OuterGlow(
        OuterGlowOp::new([1.0, 0.0, 0.0], StudioBlendMode::Normal, 0.5)
            .with_fill(GlowFill::Gradient),
    );
    assert!(matches!(
        run_ext(
            &backend,
            &plan,
            &with_effects(vec![gradient]),
            &approx,
            &sources,
            &mut sink
        ),
        Err(BackendError::Unsupported {
            operator: "glow_gradient",
            ..
        })
    ));
    // effects together with Blend-If on one layer are refused (render-cpu refuses them too)
    let both = PlanExtensions::from_layers(vec![
        LayerExtension::new(plan.root, 1)
            .with_effects(vec![overlay])
            .with_blend_if(vec![BlendRangeEntry {
                channel: BlendChannel::Gray,
                source: [0.2, 0.4, 0.6, 0.8],
                destination: IDENTITY_RANGE,
            }]),
    ]);
    assert!(matches!(
        run_ext(&backend, &plan, &both, &approx, &sources, &mut sink),
        Err(BackendError::Unsupported {
            operator: "effects_with_blend_if",
            ..
        })
    ));
    // aggregate_over_budget (V4-02): the blur kernels are charged to max_working_bytes while the
    // effects compile, before any device work
    let blurred = EffectOp::DropShadow(
        DropShadowOp::new(black(), StudioBlendMode::Normal, 0.5).with_size(50.0),
    );
    let mut starved = request(71, Exactness::Approximate);
    starved.max_working_bytes = 100;
    assert!(matches!(
        run_ext(
            &backend,
            &plan,
            &with_effects(vec![blurred]),
            &starved,
            &sources,
            &mut sink
        ),
        Err(BackendError::BudgetExceeded { limit: 100, .. })
    ));

    // Blend-If on the base of a clipping run and on a clipped layer is refused by shared admission
    // (no public source defines either; pigment's keys), before any adapter use; the same entry on
    // an unclipped layer, and identity ranges on either role, are admitted and only the missing
    // device remains
    let clip_plan = plan_of(
        72,
        extent,
        vec![
            solid(extent, [0.4, 0.5, 0.6, 1.0]),
            solid(rect(1, 1, 5, 5), [0.9, 0.2, 0.1, 1.0]),
            solid(extent, [0.1, 0.8, 0.3, 0.9]),
        ],
        vec![
            layer(0, StudioBlendMode::Normal, 1.0, 1.0),
            layer(1, StudioBlendMode::Normal, 1.0, 1.0), // clip base
            Layer {
                mask: Some(MaskInput::clip_to_base()),
                ..layer(2, StudioBlendMode::Multiply, 0.8, 1.0)
            },
        ],
    );
    let entry = vec![BlendRangeEntry {
        channel: BlendChannel::Gray,
        source: [0.1, 0.3, 0.9, 1.0],
        destination: IDENTITY_RANGE,
    }];
    let on_base = PlanExtensions::from_layers(vec![
        LayerExtension::new(clip_plan.root, 1).with_blend_if(entry.clone()),
    ]);
    let on_clipped = PlanExtensions::from_layers(vec![
        LayerExtension::new(clip_plan.root, 2).with_blend_if(entry.clone()),
    ]);
    let on_unclipped = PlanExtensions::from_layers(vec![
        LayerExtension::new(clip_plan.root, 0).with_blend_if(entry),
    ]);
    let identity_on_roles = PlanExtensions::from_layers(vec![
        LayerExtension::new(clip_plan.root, 1)
            .with_blend_if(vec![BlendRangeEntry::identity(BlendChannel::Gray)]),
        LayerExtension::new(clip_plan.root, 2)
            .with_blend_if(vec![BlendRangeEntry::identity(BlendChannel::Red)]),
    ]);
    let approx72 = request(72, Exactness::Approximate);
    for (ext, key) in [
        (&on_base, "blend_if_clip_base"),
        (&on_clipped, "blend_if_clipped_layer"),
    ] {
        assert_eq!(
            run_ext(&backend, &clip_plan, ext, &approx72, &sources, &mut sink),
            Err(BackendError::Unsupported {
                operator: key,
                exactness: Exactness::Unsupported
            }),
            "{key}"
        );
    }
    for (ext, what) in [
        (&on_unclipped, "unclipped layer"),
        (&identity_on_roles, "identity ranges on both roles"),
    ] {
        assert_eq!(
            run_ext(&backend, &clip_plan, ext, &approx72, &sources, &mut sink),
            Err(BackendError::Unavailable("no_adapter")),
            "{what}: admitted, only the missing device remains"
        );
    }
    // nothing above touched the sink or the residency ledger
    assert_eq!(sink.accepted, 0);
    assert_eq!(backend.resident_bytes(), 0);
}

/// V4-04: the fault-injection hooks (`with_shader_source`, `with_injected_loss`,
/// `simulate_device_loss` on the backend and on the context) are compiled only with the non-default
/// `test-faults` feature; the three tests that use them are gated the same way.
#[test]
fn fault_hooks_exist_only_under_the_test_faults_feature() {
    let sources = [
        ("plan_sink.rs", include_str!("../src/plan_sink.rs")),
        ("device.rs", include_str!("../src/device.rs")),
    ];
    let hooks = [
        "pub fn with_shader_source",
        "pub fn with_injected_loss",
        "pub fn simulate_device_loss",
    ];
    let mut found = 0;
    for (name, text) in sources {
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if hooks.iter().any(|h| line.trim_start().starts_with(h)) {
                found += 1;
                let gated = lines[i.saturating_sub(3)..i]
                    .iter()
                    .any(|l| l.trim() == "#[cfg(feature = \"test-faults\")]");
                assert!(
                    gated,
                    "{name}:{}: fault-injection hook without the test-faults feature gate: {line}",
                    i + 1
                );
            }
        }
    }
    assert_eq!(
        found, 4,
        "with_shader_source, with_injected_loss and two simulate_device_loss"
    );
    let manifest = include_str!("../Cargo.toml");
    assert!(
        manifest.contains("default = []"),
        "test-faults must not be a default feature"
    );
    assert!(manifest.contains("test-faults = []"));
}
