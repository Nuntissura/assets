//! Typed execution faults (lane SMB-RENDER-GPU-SUBMIT-FAULT): an error raised while a chunk's
//! commands are encoded, submitted or read back is a typed `GpuError::Gpu` code with its detail
//! (`GpuBackend::submit_fault`: operation label, wgpu's object label, class, redacted message),
//! never the generic `device_fault`; a panic inside the execution is `submit_panic` and latches the
//! device lost; `SharedDevice::new` returns the `PipelineFault` detail; machine paths are reduced
//! to file names by `redact_paths`.
//!
//! The injected faults (feature `test-faults`) are real wgpu failures on the live device: an
//! out-of-bounds read-back copy (validation error when the encoder is finished and submitted) and a
//! destroyed staging buffer (failed mapping). Live-adapter proofs follow `tests/gpu-proof.toml`
//! (V4-03): without an adapter they FAIL with `NOT_PROVEN: no adapter` while `require_gpu = true`.
#![cfg(feature = "test-faults")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BlendSpace, ColourTag, Exactness, ExecuteRequest, Group, Layer, NoVector,
    OutputTarget, PlanNode, PlanNodeId, PlanSources, RenderPlan, SampleFormat, SinkError,
    SourceKey, StudioBlendMode, TilePlanes, TileSink,
};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_render_gpu::pipeline::InjectedFault;
use hsk_studio_render_gpu::{
    DeviceOptions, FAULT_MESSAGE_MAX_BYTES, GpuBackend, GpuError, GpuStats, Reason, redact_paths,
};
use hsk_studio_pigment::render_plan::RenderReceipt;
use std::sync::{Mutex, MutexGuard, PoisonError};

static DEVICE_LOCK: Mutex<()> = Mutex::new(());
fn device_lock() -> MutexGuard<'static, ()> {
    DEVICE_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

fn require_gpu() -> bool {
    include_str!("gpu-proof.toml").lines().any(|l| l.trim() == "require_gpu = true")
        || std::env::var("HSK_STUDIO_TEST_REQUIRE_GPU").as_deref() == Ok("1")
}

/// A live backend, or `None` after a `NOT_PROVEN:` line when the policy allows no adapter.
fn live_backend(what: &str) -> Option<GpuBackend> {
    let backend = GpuBackend::new(DeviceOptions { chunk_side: 8, ..DeviceOptions::default() });
    match backend.unavailable_reason() {
        None => {
            let info = backend.device_info().unwrap_or_else(|| panic!("{what}: no live device ({:?})", backend.failure()));
            eprintln!("ADAPTER: {}", info.text());
            Some(backend)
        }
        Some(Reason::NoAdapter) if require_gpu() => {
            panic!("NOT_PROVEN: no adapter; {what} requires a GPU (tests/gpu-proof.toml)")
        }
        Some(Reason::NoAdapter) => {
            eprintln!("NOT_PROVEN: no adapter; {what} not run");
            None
        }
        Some(other) => panic!("{what}: no device ({other:?})"),
    }
}

const RGBA: [f32; 4] = [0.25, 0.5, 0.75, 0.5];
fn extent() -> Rect {
    Rect { x: 0, y: 0, width: 12, height: 10 }
}
fn plan() -> RenderPlan<NoVector> {
    RenderPlan {
        revision: 5,
        target: OutputTarget {
            extent: extent(),
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::LinearLight },
        },
        root: PlanNodeId(1),
        nodes: vec![
            PlanNode::Solid { rect: extent(), rgba: RGBA },
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
struct NoSources;
impl PlanSources for NoSources {
    fn tile(&self, _: SourceKey) -> Option<TilePlanes<'_>> {
        None
    }
    fn mask(&self, _: SourceKey) -> Option<Mask<'_>> {
        None
    }
}
/// Counts chunks and checks every texel against `RGBA`.
#[derive(Default)]
struct Check {
    accepted: usize,
    worst: f32,
}
impl TileSink for Check {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        for i in 0..(r.width * r.height) as usize {
            let px = [f(colour, i * 12), f(colour, i * 12 + 4), f(colour, i * 12 + 8), f(alpha, i * 4)];
            for (got, want) in px.iter().zip(RGBA) {
                self.worst = self.worst.max((got - want).abs());
            }
        }
        self.accepted += 1;
        Ok(())
    }
}
fn execute(backend: &GpuBackend, sink: &mut Check) -> Result<(RenderReceipt, GpuStats), GpuError> {
    let request = ExecuteRequest {
        expected_revision: 5,
        min_exactness: Exactness::Fitted,
        deadline: None,
        max_working_bytes: 1 << 30,
    };
    backend.execute_detailed(&plan(), &request, &NoSources, &CancellationToken::default(), sink)
}
/// A clean execution on `backend`: every chunk, every texel within 1e-6, no fault detail.
fn assert_clean_execution(backend: &GpuBackend, generation: u64) {
    let mut sink = Check::default();
    let result = execute(backend, &mut sink);
    assert!(result.is_ok(), "clean execution: {:?} {:?}", result.err(), backend.submit_fault());
    assert!(sink.accepted > 1 && sink.worst <= 1e-6, "chunks {} worst {}", sink.accepted, sink.worst);
    assert_eq!(backend.submit_fault(), None);
    assert_eq!(backend.device_info().map(|i| i.generation), Some(generation));
}

/// An out-of-bounds read-back copy raises a wgpu validation error at encoder finish / submit: the
/// execution is `Gpu("submit_fault")` (not `device_fault`), with the operation label, the label wgpu
/// names, class validation and the backend message; no chunk reaches the sink; the next execution
/// on the same device and generation renders correctly.
#[test]
fn submit_error_is_typed_and_the_device_stays_usable() {
    let _guard = device_lock();
    let Some(backend) = live_backend("submit-fault typing") else { return };
    let generation = backend.generation();
    assert!(backend.inject_submit_fault(InjectedFault::ReadbackCopyOutOfBounds));
    let mut sink = Check::default();
    assert_eq!(execute(&backend, &mut sink).map(|_| ()), Err(GpuError::Gpu("submit_fault")));
    assert_eq!(sink.accepted, 0);
    let fault = backend.submit_fault().expect("detail");
    eprintln!("SUBMIT_FAULT: {fault} (object {})", fault.object);
    assert_eq!((fault.code, fault.label, fault.class), ("submit_fault", "hsk-studio-render-gpu readback", "validation"), "{fault}");
    // wgpu reports an encoder error without a label: the object falls back to the operation label
    assert_eq!(fault.object, "hsk-studio-render-gpu readback", "{fault}");
    assert!(fault.message.contains("overrun"), "{fault}");
    assert!(fault.message.len() <= FAULT_MESSAGE_MAX_BYTES && !fault.message.is_empty());
    assert!(!backend.is_lost());
    assert_clean_execution(&backend, generation);
}

/// A destroyed staging buffer fails the mapping: `Gpu("readback_map_failed")` with the read-back
/// label and the scoped wgpu error as its class and message; the device stays usable.
#[test]
fn readback_failure_is_typed_with_its_cause() {
    let _guard = device_lock();
    let Some(backend) = live_backend("read-back fault typing") else { return };
    let generation = backend.generation();
    assert!(backend.inject_submit_fault(InjectedFault::StagingDestroyed));
    let mut sink = Check::default();
    assert_eq!(execute(&backend, &mut sink).map(|_| ()), Err(GpuError::Gpu("readback_map_failed")));
    assert_eq!(sink.accepted, 0);
    let fault = backend.submit_fault().expect("detail");
    eprintln!("READBACK_FAULT: {fault} (object {})", fault.object);
    assert_eq!((fault.code, fault.label), ("readback_map_failed", "hsk-studio-render-gpu readback"), "{fault}");
    assert!(matches!(fault.class, "validation" | "map"), "{fault}");
    if fault.class == "validation" {
        assert_eq!(fault.object, "hsk-studio-render-gpu staging", "{fault}");
    }
    assert!(!backend.is_lost());
    assert_clean_execution(&backend, generation);
}

/// A panic inside the execution does not escape: `Gpu("submit_panic")` with its text, the backend
/// latches the device lost (state unknown after a panic) and `reacquire` recovers with generation
/// + 1, after which a plan renders correctly.
#[test]
fn panic_in_execution_is_typed_and_latches_the_device_lost() {
    let _guard = device_lock();
    let Some(backend) = live_backend("execution panic typing") else { return };
    let generation = backend.generation();
    assert!(backend.inject_submit_fault(InjectedFault::Panic));
    let mut sink = Check::default();
    assert_eq!(execute(&backend, &mut sink).map(|_| ()), Err(GpuError::Gpu("submit_panic")));
    let fault = backend.submit_fault().expect("detail");
    assert_eq!((fault.code, fault.label, fault.class), ("submit_panic", "hsk-studio-render-gpu execute", "panic"));
    assert!(fault.message.contains("injected execution panic"), "{fault}");
    assert!(backend.is_lost());
    assert_eq!(
        execute(&backend, &mut sink).map(|_| ()),
        Err(GpuError::DeviceLost { generation })
    );
    assert_eq!(backend.reacquire(&CancellationToken::default()), Ok(generation + 1));
    assert_clean_execution(&backend, generation + 1);
}

/// `SharedDevice::new` on a host device whose limits cannot hold the pipelines (two storage
/// buffers per stage; render-gpu's layouts need up to four) returns the `PipelineFault` detail:
/// code, failing object label, class and message, not only a code.
#[cfg(feature = "shared-device")]
#[test]
fn shared_device_new_returns_the_fault_detail() {
    use hsk_studio_render_gpu::SharedDevice;
    let _guard = device_lock();
    let what = "shared-device fault detail";
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: if cfg!(windows) { wgpu::Backends::DX12 } else { wgpu::Backends::PRIMARY },
        flags: wgpu::InstanceFlags::empty(),
        ..Default::default()
    });
    let adapter = match block(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) {
        Ok(adapter) => adapter,
        Err(_) if !require_gpu() => {
            eprintln!("NOT_PROVEN: no adapter; {what} not run");
            return;
        }
        Err(_) => panic!("NOT_PROVEN: no adapter; {what} requires a GPU (tests/gpu-proof.toml)"),
    };
    let limits = wgpu::Limits { max_storage_buffers_per_shader_stage: 2, ..wgpu::Limits::downlevel_defaults() };
    let (device, queue) = block(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("test host device (2 storage buffers)"),
        required_limits: limits,
        ..Default::default()
    }))
    .expect("adapter present but no device");
    let fault = match SharedDevice::new(&device, &queue, adapter.get_info(), DeviceOptions::default()) {
        Ok(_) => panic!("{what}: the pipelines were built on a 2-storage-buffer device"),
        Err(fault) => fault,
    };
    eprintln!("SHARED_FAULT: {fault} (object {})", fault.object);
    assert_eq!((fault.code, fault.class), ("pipeline_create", "validation"), "{fault}");
    assert_eq!(fault.label, "hsk-studio-render-gpu layout", "{fault}");
    assert_eq!(fault.error(), GpuError::Pipeline("pipeline_create"));
    assert!(!fault.message.is_empty());
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

/// `redact_paths` (no adapter needed) keeps only file names: the FXC form (drive path with spaces
/// and a label with a space), forward-slash drive, UNC and Unix paths, several paths in one text;
/// prose with slashes, URLs and path-free text are unchanged; the redaction runs before the message
/// enters fault data (`PipelineFault::new`).
#[test]
fn redact_paths_keeps_only_file_names() {
    let fxc = "FXC D3DCompile error (0x80004005): D:\\Projects\\LLM projects\\H W\\crates\\hsk-studio-render-gpu\\hsk-studio-render-gpu composite(49,13-31): error X3500: array reference";
    assert_eq!(
        redact_paths(fxc),
        "FXC D3DCompile error (0x80004005): hsk-studio-render-gpu composite(49,13-31): error X3500: array reference"
    );
    assert_eq!(redact_paths("at C:/Users/a b/k.wgsl:3:4"), "at k.wgsl:3:4");
    assert_eq!(redact_paths("share \\\\host\\share\\dir\\f.hlsl\nnext"), "share f.hlsl\nnext");
    assert_eq!(redact_paths("open '/home/u/x/z.wgsl' failed"), "open 'z.wgsl' failed");
    assert_eq!(redact_paths("(/tmp/a.txt) and (/var/b.txt)"), "(a.txt) and (b.txt)");
    for unchanged in [
        "read and/or write 4 / 2 bytes",
        "see https://example.org/a/b",
        "In Device::create_compute_pipeline, label = 'hsk-studio-render-gpu probe'",
        "",
    ] {
        assert_eq!(redact_paths(unchanged), unchanged);
    }
    let fault = hsk_studio_render_gpu::PipelineFault::new(
        "submit_fault",
        "op",
        "validation",
        "In Queue::submit, label = 'obj'\n  file D:\\secret dir\\x.bin broke",
    );
    assert_eq!(fault.message, "In Queue::submit, label = 'obj'\n  file x.bin broke");
    assert_eq!(fault.object, "obj");
    assert_eq!(fault.error(), GpuError::Gpu("submit_fault"));
}
