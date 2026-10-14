//! Typed shader-module and pipeline creation failures (lane SMB-RENDER-GPU-PIPELINE-FAULT).
//!
//! wgpu 27 classes a backend shader-compiler rejection (DX12 FXC `D3DCompile`) as an INTERNAL
//! error, which a validation-only error scope does not capture: it used to reach the uncaptured
//! handler (fault latch, `device_fault` at the first dispatch) or wgpu's default handler (panic on
//! a host-owned device). These proofs run on a live adapter and follow `tests/gpu-proof.toml`
//! (V4-03): without an adapter they FAIL with `NOT_PROVEN: no adapter` while `require_gpu = true`;
//! on an adapter whose backend is not DX12 the FXC-specific assertions print a `NOT_PROVEN:` line
//! (the backend compiler differs) while the typed-refusal assertions still run.
#![cfg(feature = "test-faults")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BlendSpace, ColourTag, Exactness, ExecuteRequest, Group, Layer, NoVector,
    OutputTarget, PlanNode, PlanNodeId, PlanSources, RenderPlan, SampleFormat, SinkError,
    SourceKey, StudioBlendMode, TilePlanes, TileSink,
};
use hsk_studio_render_gpu::{
    DeviceOptions, FAULT_MESSAGE_MAX_BYTES, GpuBackend, GpuError, PipelineFault, Reason,
};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Accepted by naga (valid WGSL), rejected by FXC: a data-dependent write to a vector component
/// inside a loop whose trip count is data-dependent (FXC cannot unroll it and cannot address a
/// vector component dynamically as an l-value). Bindings fit the composite layout (1 read, 2 write).
const FXC_REJECTED_WGSL: &str = "\
@group(0) @binding(1) var<storage, read> src: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> dst: array<vec4<f32>>;

@compute @workgroup_size(8, 8, 1)
fn composite(@builtin(global_invocation_id) id: vec3<u32>) {
    var c = vec4<f32>(0.0);
    let n = u32(src[id.x].w);
    for (var k = 0u; k < n; k = k + 1u) {
        let i = u32(src[id.x + k].x) % 4u;
        c[i] = c[(i + 1u) % 4u] + src[id.x + k].y;
    }
    dst[id.x] = c;
}
";

/// Rejected by naga (unknown identifier): the module itself fails, `shader_validation`.
const NAGA_REJECTED_WGSL: &str =
    "@compute @workgroup_size(8, 8, 1)\nfn composite() {\n    let x: f32 = no_such_symbol;\n}\n";

/// A valid kernel for the same probe route (auto layout).
const VALID_WGSL: &str = "\
@group(0) @binding(0) var<storage, read_write> dst: array<vec4<f32>>;

@compute @workgroup_size(8, 8, 1)
fn composite(@builtin(global_invocation_id) id: vec3<u32>) {
    let c = dst[id.x];
    dst[id.x] = vec4<f32>(c.x, c.y, c.z, 1.0);
}
";

static DEVICE_LOCK: Mutex<()> = Mutex::new(());
fn device_lock() -> MutexGuard<'static, ()> {
    DEVICE_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

fn strict() -> bool {
    include_str!("gpu-proof.toml").lines().any(|l| l.trim() == "require_gpu = true")
        || std::env::var("HSK_STUDIO_TEST_REQUIRE_GPU").as_deref() == Ok("1")
}

/// `false` (after a `NOT_PROVEN:` line) when there is no adapter and the policy allows it; panics
/// with `NOT_PROVEN: no adapter` under `require_gpu = true`.
fn adapter_present(b: &GpuBackend, what: &str) -> bool {
    match b.unavailable_reason() {
        None => true,
        Some(Reason::NoAdapter) if strict() => {
            panic!("NOT_PROVEN: no adapter; {what} requires a GPU (tests/gpu-proof.toml)")
        }
        Some(Reason::NoAdapter) => {
            eprintln!("NOT_PROVEN: no adapter; {what} not run");
            false
        }
        Some(other) => panic!("{what}: no device ({other:?})"),
    }
}

fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect { x, y, width, height }
}
fn solid_plan(extent: Rect, rgba: [f32; 4]) -> RenderPlan<NoVector> {
    RenderPlan {
        revision: 3,
        target: OutputTarget {
            extent,
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::LinearLight },
        },
        root: PlanNodeId(1),
        nodes: vec![
            PlanNode::Solid { rect: extent, rgba },
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![Layer {
                    input: PlanNodeId(0),
                    mode: StudioBlendMode::Normal,
                    opacity: 1.0,
                    fill_opacity: 1.0,
                    mask: None,
                }],
            }),
        ],
    }
}
fn request() -> ExecuteRequest {
    ExecuteRequest {
        expected_revision: 3,
        min_exactness: Exactness::Fitted,
        deadline: None,
        max_working_bytes: 1 << 30,
    }
}
struct NoSources;
impl PlanSources for NoSources {
    fn tile(&self, _: SourceKey) -> Option<TilePlanes<'_>> {
        None
    }
    fn mask(&self, _: SourceKey) -> Option<Mask<'_>> {
        None
    }
}
/// Collects straight RGBA texels.
struct Image {
    extent: Rect,
    px: Vec<[f32; 4]>,
    accepted: usize,
}
impl Image {
    fn new(extent: Rect) -> Self {
        Self { extent, px: vec![[f32::NAN; 4]; (extent.width * extent.height) as usize], accepted: 0 }
    }
}
impl TileSink for Image {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        for y in 0..r.height as usize {
            for x in 0..r.width as usize {
                let i = y * r.width as usize + x;
                let at = ((r.y - self.extent.y) as usize + y) * self.extent.width as usize
                    + (r.x - self.extent.x) as usize
                    + x;
                self.px[at] = [f(colour, i * 12), f(colour, i * 12 + 4), f(colour, i * 12 + 8), f(alpha, i * 4)];
            }
        }
        self.accepted += 1;
        Ok(())
    }
}

/// FXC-specific expectations of a fault, or a `NOT_PROVEN:` line on another backend.
fn check_fxc_fault(fault: &PipelineFault, backend: &str, label: &str) {
    assert_eq!(fault.code, "pipeline_create", "{fault}");
    assert_eq!(fault.label, label, "{fault}");
    if backend == "dx12" {
        assert_eq!(fault.class, "internal", "{fault}");
        assert!(fault.message.contains("D3DCompile"), "backend message missing: {fault}");
    } else {
        eprintln!("NOT_PROVEN: backend {backend} is not dx12; the FXC rejection class was not exercised ({fault})");
    }
}

/// A backend whose shader FXC rejects is the typed pipeline failure at construction, with the
/// failing pipeline's label and FXC's message, before any dispatch: never `device_fault`, never a
/// panic, never `Unavailable`; no sink call.
#[test]
fn fxc_rejected_kernel_is_a_typed_refusal_before_any_dispatch() {
    let _guard = device_lock();
    let broken = GpuBackend::with_shader_source(DeviceOptions::default(), FXC_REJECTED_WGSL);
    if !adapter_present(&broken, "FXC-rejected kernel typing") {
        return;
    }
    let fault = broken.pipeline_fault().expect("pipeline fault detail");
    eprintln!("PIPELINE_FAULT: {fault}");
    assert_eq!(broken.failure(), Some(GpuError::Pipeline("pipeline_create")));
    // The backend name comes from a live device: build a sound backend for it.
    let backend = GpuBackend::new(DeviceOptions::default())
        .device_info()
        .expect("a sound backend on the same adapter")
        .backend;
    check_fxc_fault(&fault, backend, "hsk-studio-render-gpu composite");
    assert!(fault.message.len() <= FAULT_MESSAGE_MAX_BYTES);
    let extent = rect(0, 0, 4, 4);
    let mut sink = Image::new(extent);
    let detailed = broken.execute_detailed(
        &solid_plan(extent, [0.2, 0.4, 0.6, 1.0]),
        &request(),
        &NoSources,
        &CancellationToken::default(),
        &mut sink,
    );
    assert_eq!(detailed.map(|_| ()), Err(GpuError::Pipeline("pipeline_create")));
    assert_eq!(sink.accepted, 0);
    // a re-acquire rebuilds the same shader: the same typed failure and detail
    assert_eq!(
        broken.reacquire(&CancellationToken::default()),
        Err(GpuError::Pipeline("pipeline_create"))
    );
    assert_eq!(broken.pipeline_fault(), Some(fault));
}

/// On one live device: a naga-rejected and an FXC-rejected probe kernel are typed refusals, the
/// uncaptured-error latch stays clear, a valid probe kernel is then built, and the backend's own
/// pipelines still render a plan with the expected pixels on the same device (same generation).
#[test]
fn probe_failure_leaves_the_device_usable() {
    let _guard = device_lock();
    let backend = GpuBackend::new(DeviceOptions { chunk_side: 8, ..DeviceOptions::default() });
    if !adapter_present(&backend, "pipeline-fault device reuse") {
        return;
    }
    let info = backend.device_info().expect("live device");
    eprintln!("ADAPTER: {}", info.text());

    assert_eq!(
        backend.probe_pipeline(NAGA_REJECTED_WGSL, "composite"),
        Err(GpuError::Pipeline("shader_validation"))
    );
    let naga = backend.pipeline_fault().expect("detail");
    assert_eq!((naga.label, naga.class), ("hsk-studio-render-gpu probe", "validation"), "{naga}");
    assert!(naga.message.contains("no_such_symbol"), "{naga}");

    // hostile size: a 20000-byte identifier in the message is cut at FAULT_MESSAGE_MAX_BYTES
    let long = format!(
        "@compute @workgroup_size(1)\nfn composite() {{\n    let x: f32 = {};\n}}\n",
        "q".repeat(20_000)
    );
    assert_eq!(backend.probe_pipeline(&long, "composite"), Err(GpuError::Pipeline("shader_validation")));
    let cut = backend.pipeline_fault().expect("detail");
    assert!(cut.message.len() <= FAULT_MESSAGE_MAX_BYTES, "{}", cut.message.len());

    assert_eq!(
        backend.probe_pipeline(FXC_REJECTED_WGSL, "composite"),
        Err(GpuError::Pipeline("pipeline_create"))
    );
    let fxc = backend.pipeline_fault().expect("detail");
    eprintln!("PROBE_FAULT: {fxc}");
    check_fxc_fault(&fxc, info.backend, "hsk-studio-render-gpu probe");

    assert_eq!(backend.probe_pipeline(VALID_WGSL, "composite"), Ok(()));

    let extent = rect(0, 0, 12, 10);
    let rgba = [0.25, 0.5, 0.75, 0.5];
    let mut sink = Image::new(extent);
    let receipt = backend.execute_detailed(
        &solid_plan(extent, rgba),
        &request(),
        &NoSources,
        &CancellationToken::default(),
        &mut sink,
    );
    assert!(receipt.is_ok(), "execute after the refusals: {:?}", receipt.err());
    assert!(sink.accepted > 1, "chunks {}", sink.accepted);
    for px in &sink.px {
        for (got, want) in px.iter().zip(rgba) {
            assert!((got - want).abs() <= 1e-6, "pixel {px:?} vs {rgba:?}");
        }
    }
    assert!(!backend.is_lost());
    assert_eq!(backend.device_info().map(|i| i.generation), Some(info.generation));
}

/// V4-04 + STU-ARC-002 (source scan, no adapter needed): the probe hook is compiled only with the
/// `test-faults` feature, and no public item of the new module names a wgpu type.
#[test]
fn probe_hook_is_gated_and_the_fault_is_plain_data() {
    let plan_sink = include_str!("../src/plan_sink.rs");
    let lines: Vec<&str> = plan_sink.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.trim_start().starts_with("pub fn probe_pipeline"))
        .expect("probe_pipeline");
    assert!(
        lines[at.saturating_sub(3)..at].iter().any(|l| l.trim() == "#[cfg(feature = \"test-faults\")]"),
        "probe_pipeline without the test-faults gate"
    );
    for line in include_str!("../src/pipeline.rs").lines() {
        let t = line.trim_start();
        if t.starts_with("pub ") {
            assert!(!t.contains("wgpu"), "public item mentions wgpu: {t}");
        }
    }
    let fault = PipelineFault {
        code: "pipeline_create",
        label: "l",
        class: "internal",
        message: String::from("m"),
    };
    assert_eq!(fault.error(), GpuError::Pipeline("pipeline_create"));
    assert_eq!(fault.to_string(), "pipeline_create(l, internal): m");
}
