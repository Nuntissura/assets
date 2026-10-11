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
use hsk_studio_pigment::blend_math::{
    Applicability, Family, MODES, PixelSite, Rgba, blend_rgb, composite_over,
};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BlendSpace, ColourTag, Exactness, ExecuteRequest, Group, Layer,
    MaskFormat, MaskInput, NoVector, OutputTarget, PlanError, PlanNode, PlanNodeId, PlanSources,
    RenderBackend, RenderPlan, SampleFormat, SinkError, SourceKey, StudioBlendMode, TileInput,
    TilePlanes, TileSink,
};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_render_gpu::{
    BackendChoice, DeviceInfo, DeviceOptions, GpuBackend, GpuContext, GpuError, Reason, Residency,
    ResidentKey, gpu_blend_qualifies, gpu_blend_supported,
};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
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
/// One backend for the GPU-execution tests (acquiring a device takes seconds). Created on first
/// use while the caller holds `DEVICE_LOCK`.
fn shared_backend() -> &'static GpuBackend {
    static SHARED: OnceLock<GpuBackend> = OnceLock::new();
    SHARED.get_or_init(|| {
        GpuBackend::new(DeviceOptions {
            chunk_side: 16,
            backend: backend_from_env(),
            ..DeviceOptions::default()
        })
    })
}
fn no_adapter() -> DeviceOptions {
    DeviceOptions {
        backend: BackendChoice::None,
        ..DeviceOptions::default()
    }
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
            eprintln!("{}", not_proven_line(what));
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
    let backend = GpuBackend::new(no_adapter());
    let strict = gate(&backend, "regression probe", true);
    assert!(
        matches!(&strict, Err(m) if m.starts_with("NOT_PROVEN: no adapter")),
        "strict gate must fail on no adapter, got {:?}",
        strict.as_ref().map(|d| d.is_some())
    );
    // opt-out route: not an error, and the line it prints is the one the round counts
    assert!(matches!(
        gate(&backend, "regression probe", false),
        Ok(None)
    ));
    let line = not_proven_line("regression probe");
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
    assert!(matches!(
        run(
            &backend,
            &group_plan(false, StudioBlendMode::PassThrough, 0.5),
            &approx,
            &sources,
            &mut sink
        ),
        Err(BackendError::Unsupported {
            operator: "group_pass_through_opacity",
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
    assert_eq!(
        backend.exactness(Operator::PassThroughOpacity),
        Exactness::Unsupported
    );
}

/// R3 finding 1 (regression): a leaf root has no layer, so the plan lists no blend operator, but
/// the executor composites it through the blend kernel (Normal, opacity 1, over transparent). The
/// class must be that kernel's (Fitted, the crate's GPU blend policy): an Exact floor is refused at
/// admission, before any source lookup or device work, and the receipt carries `normal: fitted`.
#[test]
fn leaf_root_reports_the_blend_kernel_class() {
    let backend = GpuBackend::new(no_adapter());
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
#[test]
fn injected_loss_latch_and_unavailable_are_explicit() {
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

/// R3 finding 3 (regression): `wgpu::Device::destroy` on a live device must be detected by the
/// backend ITSELF, through wgpu's device-lost callback (delivered by the poll the backend runs
/// before every execute and state query). `simulate_device_loss` only destroys and never sets the
/// latch, so removing the callback wiring in device.rs makes this test fail. A driver-initiated
/// loss (TDR, adapter removal) is a different trigger and stays NOT_PROVEN.
#[test]
fn device_destroy_is_detected_by_the_lost_callback() {
    let _guard = device_lock();
    // Own backend: the shared GPU-test backend must stay alive.
    let live = GpuBackend::new(DeviceOptions {
        backend: backend_from_env(),
        ..DeviceOptions::default()
    });
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
#[test]
fn pipeline_failure_is_typed_and_fails_the_gpu_gate() {
    const BROKEN_WGSL: &str = "@compute @workgroup_size(8, 8, 1)\nfn composite() {\n    let x: f32 = no_such_symbol;\n}\n";
    let _guard = device_lock();
    let broken = GpuBackend::with_shader_source(
        DeviceOptions {
            backend: backend_from_env(),
            ..DeviceOptions::default()
        },
        BROKEN_WGSL,
    );
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
        backend_from_env().key(),
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
        ("device.rs", include_str!("../src/device.rs")),
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
    // dst + staging + upload chunk buffers, 64-byte params, minimal 4-byte mask binding
    assert_eq!(receipt.peak_working_bytes, 3 * 16 * 16 * 16 + 64 + 4);
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

/// Per-pixel evaluation of a plan: layers bottom-first, masks, opacity * fill, isolated groups
/// (own transparent result), pass-through groups (onto the running backdrop) and pigment's
/// documented clip-to-base rule (`MaskFormat::BaseAlpha`).
struct Reference<'a> {
    plan: &'a RenderPlan<NoVector>,
    sources: &'a Sources,
}
impl Reference<'_> {
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
    fn mask(&self, m: &MaskInput, x: i64, y: i64) -> f32 {
        let (_, plane) = self
            .sources
            .masks
            .iter()
            .find(|(k, _)| *k == m.key.0)
            .expect("mask plane");
        plane.coverage(x, y, m.inverted)
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
                self.layer(&g.layers[i], x, y, dst);
            } else {
                self.run(&g.layers[i..j], x, y, dst);
            }
            i = j;
        }
    }
    fn layer(&self, l: &Layer, x: i64, y: i64, dst: &mut [f32; 4]) {
        if let PlanNode::Group(c) = &self.plan.nodes[l.input.0 as usize]
            && !c.isolated
        {
            self.group(l.input.0 as usize, x, y, dst);
            return;
        }
        let Some(s) = self.source(l, x, y) else {
            return;
        };
        let m = l.mask.as_ref().map_or(1.0, |m| self.mask(m, x, y));
        over(dst, l.mode, s, l.opacity * l.fill_opacity * m);
    }
    /// `run = [base, clipped...]`: G = base content with the base's mask; each clipped layer is
    /// composited onto G with its source alpha times the base alpha and G's alpha held at the base
    /// alpha; G goes onto `dst` with the BASE layer's mode, opacity and fill.
    fn run(&self, run: &[Layer], x: i64, y: i64, dst: &mut [f32; 4]) {
        let base = &run[0];
        let mut g = [0.0_f32; 4];
        if let Some(s) = self.source(base, x, y) {
            let m = base.mask.as_ref().map_or(1.0, |m| self.mask(m, x, y));
            let a = s[3] * m;
            if a > 0.0 {
                g = [s[0], s[1], s[2], a];
            }
        }
        let a_base = g[3];
        for c in &run[1..] {
            let Some(s) = self.source(c, x, y) else {
                continue;
            };
            let opacity = c.opacity * c.fill_opacity;
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
        over(dst, base.mode, g, base.opacity * base.fill_opacity);
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
    let extent = plan.target.extent;
    let mut sink = Image::new(extent);
    let receipt = run(
        backend,
        plan,
        &request(plan.revision, floor),
        sources,
        &mut sink,
    )
    .expect("plan executes on the device");
    let reference = Reference { plan, sources };
    let (mut max_err, mut worst) = (0.0_f32, (0, 0));
    let mut worst_values = ([0.0_f32; 4], [0.0_f32; 4]);
    for y in 0..i64::from(extent.height) {
        for x in 0..i64::from(extent.width) {
            let mut want = [0.0_f32; 4];
            reference.group(plan.root.0 as usize, extent.x + x, extent.y + y, &mut want);
            let got = sink.px[(y * i64::from(extent.width) + x) as usize];
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
    (max_err, worst, receipt)
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
        4 * 16 * 16 * 16 + 16 * 16 * 16 + 16 * 16 * 4 + 64
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
