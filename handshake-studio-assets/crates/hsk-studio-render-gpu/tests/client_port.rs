//! Lane SMB-GPU-CLIENT-PORT-1 (RF-03 phase 0, asset side of MT-36248): the typed GPU client port.
//! The port tests need no adapter (a host-shaped test port over `GrantLedger`, `UnavailablePort`,
//! a canceled admission). The live tests follow `tests/gpu-proof.toml` (V4-03): with
//! `require_gpu = true` no adapter FAILS with a message starting `NOT_PROVEN: no adapter`, never a
//! pass. The transient adapter is asset-local: these results are evidence, never host acceptance
//! (AC-HOST-005). On Windows the transient adapter is Vulkan first; a run that ends on another
//! backend prints a `NOT_PROVEN:` line for the Vulkan row (GUI-DESKTOP-026) and is never counted as
//! a Vulkan proof.
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{FailureCode, Outcome};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BlendSpace, ColourTag, Exactness, ExecuteRequest, Group, Layer,
    NoVector, OutputTarget, PlanNode, PlanNodeId, PlanSources, RenderBackend, RenderPlan,
    SampleFormat, SinkError, SourceKey, StudioBlendMode, TilePlanes, TileSink,
};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_render_gpu::{
    AdmissionRequest, BudgetSource, ByteQuantity, ClientRef, ClientRefusal, DenialCode, DeviceOptions,
    DeviceRef, GpuBackend, GpuClientPort, GpuError, GpuGrant, GpuOperation, GrantIssuer,
    GrantLedger, ModuleRef, PortCounters, PortEvent, Reason, SettleError, SettleOutcome,
    SettleReceipt, Settlement, TransientAdapter, TransientOptions, UnavailablePort,
    port_outcome_of,
};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

const GPU_PROOF_POLICY: &str = include_str!("gpu-proof.toml");
const EXTENT: Rect = Rect { x: 0, y: 0, width: 6, height: 4 };
const RED: [f32; 4] = [0.9, 0.05, 0.1, 1.0];

static DEVICE_LOCK: Mutex<()> = Mutex::new(());
fn device_lock() -> MutexGuard<'static, ()> {
    DEVICE_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

fn require_gpu() -> bool {
    let mut require = None;
    for line in GPU_PROOF_POLICY.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        match line.split_once('=').map(|(k, v)| (k.trim(), v.trim())) {
            Some(("require_gpu", "true")) => require = Some(true),
            Some(("require_gpu", "false")) => require = Some(false),
            Some(("schema", v)) => assert_eq!(v, "\"hsk.studio.render-gpu-proof@1\""),
            other => panic!("tests/gpu-proof.toml: unexpected line {other:?}"),
        }
    }
    require.expect("tests/gpu-proof.toml require_gpu") || std::env::var("HSK_STUDIO_TEST_REQUIRE_GPU").as_deref() == Ok("1")
}

/// `true` with a live device; `false` after a `NOT_PROVEN:` line when there is no adapter and the
/// policy allows it; a panic starting `NOT_PROVEN: no adapter` under `require_gpu = true`; any other
/// failure (pipeline, denial, timeout) is a test failure, never a skip.
fn live(backend: &GpuBackend, what: &str) -> bool {
    if let Some(info) = backend.device_info() {
        eprintln!("GPU_CLIENT_DEVICE: {} ({what})", info.text());
        if cfg!(windows) && info.backend != "vulkan" {
            eprintln!(
                "NOT_PROVEN: vulkan first; the transient adapter opened {} for {what}, which proves no vulkan row",
                info.backend
            );
        }
        return true;
    }
    if let Some(failure) = backend.failure() {
        panic!("{what}: adapter present but the backend failed: {failure} ({failure:?})");
    }
    match backend.unavailable_reason() {
        Some(Reason::NoAdapter) if !require_gpu() => {
            eprintln!("NOT_PROVEN: no adapter; {what} not run");
            false
        }
        Some(Reason::NoAdapter) => {
            panic!("NOT_PROVEN: no adapter; {what} requires a GPU (tests/gpu-proof.toml require_gpu = true)")
        }
        other => panic!("{what}: no live device (unavailable {other:?}, lost {})", backend.is_lost()),
    }
}

/// Red over the extent, a half-transparent blue square at 70 %.
fn plan() -> RenderPlan<NoVector> {
    let r = |x, y, w, h| Rect { x, y, width: w, height: h };
    let layer = |input: u32, opacity: f32| Layer {
        input: PlanNodeId(input),
        mode: StudioBlendMode::Normal,
        opacity,
        fill_opacity: 1.0,
        mask: None,
    };
    RenderPlan {
        revision: 3,
        target: OutputTarget {
            extent: EXTENT,
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::LinearLight },
        },
        root: PlanNodeId(2),
        nodes: vec![
            PlanNode::Solid { rect: EXTENT, rgba: RED },
            PlanNode::Solid { rect: r(2, 1, 2, 2), rgba: [0.1, 0.2, 0.95, 0.5] },
            PlanNode::Group(Group { isolated: true, layers: vec![layer(0, 1.0), layer(1, 0.7)] }),
        ],
    }
}
fn request() -> ExecuteRequest {
    ExecuteRequest { expected_revision: 3, min_exactness: Exactness::Fitted, deadline: None, max_working_bytes: 64 << 20 }
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
/// Straight RGBA f32 of the extent.
struct Image {
    px: Vec<[f32; 4]>,
    calls: usize,
}
impl Image {
    fn new() -> Self {
        Self { px: vec![[f32::NAN; 4]; (EXTENT.width * EXTENT.height) as usize], calls: 0 }
    }
}
impl TileSink for Image {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        for y in 0..r.height as usize {
            for x in 0..r.width as usize {
                let i = y * r.width as usize + x;
                let at = ((r.y - EXTENT.y) as usize + y) * EXTENT.width as usize + (r.x - EXTENT.x) as usize + x;
                self.px[at] = [f(colour, i * 12), f(colour, i * 12 + 4), f(colour, i * 12 + 8), f(alpha, i * 4)];
            }
        }
        self.calls += 1;
        Ok(())
    }
}

/// A host-shaped port without devices (what MT-203's owner implements; the trait is implementable
/// outside render-gpu): `Host` grants from a `GrantLedger`, a budget, host-side rebind.
struct LedgerPort {
    ledger: GrantLedger,
    budget: u64,
}
impl GpuClientPort for LedgerPort {
    fn admit(&self, request: &AdmissionRequest, cancel: &CancellationToken) -> Result<GpuGrant, ClientRefusal> {
        if cancel.check().is_err() {
            return Err(self.ledger.refuse(ClientRefusal::Canceled));
        }
        self.ledger.issue_host(request, self.budget, 41, "vulkan")
    }
    fn authorize(&self, grant: &GpuGrant) -> Result<(), ClientRefusal> {
        self.ledger.authorize(grant)
    }
    fn settle(&self, grant: &GpuGrant, settlement: Settlement) -> Result<SettleReceipt, SettleError> {
        self.ledger.settle(grant, settlement)
    }
    fn rebind(&self, grant: &GpuGrant, _: &CancellationToken) -> Result<GpuGrant, ClientRefusal> {
        self.ledger.rebind_host(grant, self.budget, "vulkan")
    }
    fn counters(&self) -> PortCounters {
        self.ledger.counters()
    }
}
fn attributed() -> AdmissionRequest {
    AdmissionRequest::new(GpuOperation::Readback, ModuleRef(7), ClientRef(0x5eed)).with_requested_bytes(1 << 20)
}

/// AC-HOST-002: the module, client, device and operation of the request travel on the grant (and
/// on the backend's grant), the budget is the issuer's, and a request above it is denied.
#[test]
fn admission_carries_module_client_and_device() {
    let port = LedgerPort { ledger: GrantLedger::new(), budget: 32 << 20 };
    let cancel = CancellationToken::default();
    let grant = port.admit(&attributed(), &cancel).expect("admitted");
    let info = grant.info();
    assert_eq!((info.operation, info.module, info.client), (GpuOperation::Readback, ModuleRef(7), ClientRef(0x5eed)));
    assert_eq!(info.device, DeviceRef::Id(41));
    assert_eq!((info.generation, info.budget_bytes, info.backend), (1, 32 << 20, "vulkan"));
    assert_eq!((info.issuer, info.budget_source), (GrantIssuer::Host, BudgetSource::HostIssued));
    assert!(!grant.carries_device());
    assert_eq!(
        port.admit(&attributed().with_requested_bytes((32 << 20) + 1), &cancel).map(|_| ()),
        Err(ClientRefusal::Denied(DenialCode::BudgetExceeded))
    );
    assert!(port.settle(&grant, Settlement::empty(SettleOutcome::Completed)).is_ok());

    // the backend holds the grant of its request; a grant without a device (no hand-off yet) is
    // the typed Unavailable(no_device), never a CPU fallback
    let port = Arc::new(LedgerPort { ledger: GrantLedger::new(), budget: 32 << 20 });
    let backend = GpuBackend::with_grant(port.clone(), attributed(), DeviceOptions::default());
    let held = backend.grant_info().expect("grant held");
    assert_eq!((held.module, held.client, held.operation), (ModuleRef(7), ClientRef(0x5eed), GpuOperation::Readback));
    assert_eq!(backend.residency_budget_bytes(), held.budget_bytes);
    assert_eq!(backend.unavailable_reason(), Some(Reason::NoDevice));
    let mut sink = Image::new();
    assert_eq!(
        backend.execute(&plan(), &request(), &NoSources, &cancel, &mut sink).map(|_| ()),
        Err(BackendError::Unavailable("no_device"))
    );
    assert_eq!((sink.calls, backend.resident_bytes()), (0, 0));
    let receipt = backend.release().expect("settled once");
    assert_eq!(receipt.grant.module, ModuleRef(7));
    assert_eq!(receipt.settlement.outcome, SettleOutcome::Failed);
    assert_eq!(receipt.settlement.native_driver, ByteQuantity::Unknown);
    let counters = port.counters();
    assert_eq!((counters.admitted, counters.settled, counters.outstanding), (1, 1, 0));
}

/// AC-HOST-003: settle exactly once; a settlement with work in flight changes nothing; a second
/// settle is DoubleRelease; after a rebind the old grant is refused for submission (Lost) and for
/// settlement (LateRelease); a grant of another port is StaleGrant; a revoked grant never submits.
#[test]
fn settle_once_double_and_late_release_are_typed() {
    let port = LedgerPort { ledger: GrantLedger::new(), budget: 8 << 20 };
    let other = LedgerPort { ledger: GrantLedger::new(), budget: 8 << 20 };
    let cancel = CancellationToken::default();
    let measured = |in_flight| Settlement {
        retained: ByteQuantity::Measured(0),
        peak: ByteQuantity::Measured(4096),
        evictions: 0,
        native_driver: ByteQuantity::Unknown,
        in_flight,
        outcome: SettleOutcome::Completed,
    };

    let first = port.admit(&attributed(), &cancel).unwrap();
    assert_eq!(port.settle(&first, measured(1)), Err(SettleError::InFlight));
    assert_eq!(port.authorize(&first), Ok(()), "a refused settlement changes nothing");
    let receipt = port.settle(&first, measured(0)).expect("settles once");
    assert_eq!(receipt.settlement.peak, ByteQuantity::Measured(4096));
    assert_eq!(port.settle(&first, measured(0)), Err(SettleError::DoubleRelease));
    assert_eq!(port.authorize(&first), Err(ClientRefusal::Denied(DenialCode::Revoked)));

    let old = port.admit(&attributed(), &cancel).unwrap();
    let new = port.rebind(&old, &cancel).expect("rebind");
    assert_eq!(new.generation(), old.generation() + 1);
    assert_eq!((new.info().module, new.info().client, new.device()), (ModuleRef(7), ClientRef(0x5eed), old.device()));
    assert_eq!(port.authorize(&old), Err(ClientRefusal::Lost { generation: 1 }));
    assert_eq!(port.settle(&old, measured(0)), Err(SettleError::LateRelease));
    assert_eq!(port.rebind(&old, &cancel).map(|_| ()), Err(ClientRefusal::Lost { generation: 1 }));
    assert_eq!(port.authorize(&new), Ok(()));
    assert_eq!(other.settle(&new, measured(0)), Err(SettleError::StaleGrant));
    assert_eq!(other.authorize(&new), Err(ClientRefusal::Denied(DenialCode::Revoked)));
    assert!(port.settle(&new, measured(0)).is_ok());

    let revoked = port.admit(&attributed(), &cancel).unwrap();
    assert!(port.ledger.revoke(&revoked));
    assert_eq!(port.authorize(&revoked), Err(ClientRefusal::Denied(DenialCode::Revoked)));
    assert!(port.settle(&revoked, Settlement::empty(SettleOutcome::Canceled)).is_ok());

    let c = port.counters();
    assert_eq!(
        (c.admitted, c.settled, c.in_flight, c.double_release, c.late_release, c.rebound, c.outstanding),
        (4, 3, 1, 1, 1, 1, 0),
        "{c:?}"
    );
    assert_eq!(other.counters().stale_grant, 1);
    // every terminal outcome is a closed observe outcome (no text)
    assert_eq!(port_outcome_of(PortEvent::SettleRefused(SettleError::DoubleRelease)), Outcome::Failure(FailureCode::Validation));
    assert_eq!(port_outcome_of(PortEvent::Refused(ClientRefusal::Lost { generation: 1 })), Outcome::Failure(FailureCode::Loss));
    assert_eq!(port_outcome_of(PortEvent::Rebound { generation: 2 }), Outcome::Success);
    assert_eq!(PortEvent::SettleRefused(SettleError::LateRelease).code(), "late_release");
}

/// AC-HOST-003/004: a canceled admission is Canceled before any device work and charges nothing;
/// the no-device port refuses as Unavailable(no_adapter) with no sink call; a dropped backend
/// settles as abandoned and is counted.
#[test]
fn canceled_admission_charges_nothing() {
    let canceled = CancellationToken::default();
    canceled.cancel();
    let transient = TransientAdapter::new(TransientOptions::default());
    assert_eq!(transient.admit(&attributed(), &canceled).map(|_| ()), Err(ClientRefusal::Canceled));
    let c = transient.counters();
    assert_eq!((c.canceled, c.admitted, c.outstanding), (1, 0, 0));
    assert_eq!(port_outcome_of(PortEvent::Refused(ClientRefusal::Canceled)), Outcome::Canceled);
    // the GpuBackend boundary keeps every refusal typed
    assert_eq!(GpuError::from(ClientRefusal::Canceled), GpuError::Backend(BackendError::Canceled));
    assert_eq!(
        GpuError::from(ClientRefusal::Denied(DenialCode::BudgetExceeded)),
        GpuError::Denied(DenialCode::BudgetExceeded)
    );
    assert_eq!(
        BackendError::from(GpuError::Denied(DenialCode::Revoked)),
        BackendError::Unavailable("grant_revoked")
    );

    let none = Arc::new(UnavailablePort::new());
    let backend = GpuBackend::with_grant(none.clone(), attributed(), DeviceOptions::default());
    assert_eq!(backend.unavailable_reason(), Some(Reason::NoAdapter));
    assert_eq!((backend.grant_info(), backend.residency_budget_bytes()), (None, 0));
    let mut sink = Image::new();
    assert_eq!(
        backend.execute(&plan(), &request(), &NoSources, &CancellationToken::default(), &mut sink).map(|_| ()),
        Err(BackendError::Unavailable("no_adapter"))
    );
    assert_eq!((sink.calls, backend.resident_bytes()), (0, 0));
    assert_eq!(backend.last_port_event(), Some(PortEvent::Refused(ClientRefusal::Unavailable(Reason::NoAdapter))));
    assert_eq!(backend.release().map(|_| ()), Err(SettleError::StaleGrant));
    assert_eq!(none.counters().unavailable, 1);

    let port = Arc::new(LedgerPort { ledger: GrantLedger::new(), budget: 1 << 20 });
    drop(GpuBackend::with_grant(port.clone(), AdmissionRequest::asset_local(), DeviceOptions::default()));
    let c = port.counters();
    assert_eq!((c.admitted, c.abandoned, c.settled, c.outstanding), (1, 1, 0, 0));
}

/// Live (transient adapter, Vulkan first): the backend's residency budget is the grant's, a plan
/// executes, and the settlement carries Measured retained and peak equal to the residency counters.
#[test]
fn transient_grant_budget_drives_residency_and_settles_measured() {
    let _guard = device_lock();
    let transient = Arc::new(TransientAdapter::new(TransientOptions {
        residency_budget_bytes: 8 << 20,
        ..TransientOptions::default()
    }));
    let backend = GpuBackend::with_grant(transient.clone(), attributed(), DeviceOptions { chunk_side: 4, ..DeviceOptions::default() });
    if !live(&backend, "transient grant settlement") {
        return;
    }
    let grant = backend.grant_info().expect("grant");
    assert_eq!((grant.issuer, grant.budget_source, grant.budget_bytes), (GrantIssuer::TransientAssetLocal, BudgetSource::StudioDeclared, 8 << 20));
    assert_eq!(grant.backend, backend.device_info().unwrap().backend);
    assert_eq!(backend.residency_budget_bytes(), 8 << 20, "budget comes from the grant");
    let mut sink = Image::new();
    let (receipt, stats) = backend
        .execute_detailed(&plan(), &request(), &NoSources, &CancellationToken::default(), &mut sink)
        .expect("executes on the granted device");
    assert_eq!(receipt.backend.generation, grant.generation);
    assert!(stats.peak_resident_bytes > 0 && stats.peak_resident_bytes <= 8 << 20);
    let px = sink.px[0];
    for (got, want) in px.iter().zip(RED) {
        assert!((got - want).abs() <= 1e-6, "texel {px:?}");
    }
    let retained = backend.resident_bytes();
    let settled = backend.release().expect("settles once");
    assert_eq!(settled.settlement.retained, ByteQuantity::Measured(retained));
    assert_eq!(settled.settlement.peak, ByteQuantity::Measured(stats.peak_resident_bytes));
    assert_eq!((settled.settlement.outcome, settled.settlement.in_flight), (SettleOutcome::Completed, 0));
    assert_eq!(settled.settlement.native_driver, ByteQuantity::Unknown);
    let c = transient.counters();
    assert_eq!((c.admitted, c.settled, c.outstanding), (1, 1, 0));
    eprintln!(
        "GPU_CLIENT_SETTLEMENT: backend {} peak {} retained {} budget {}",
        grant.backend, stats.peak_resident_bytes, retained, grant.budget_bytes
    );
}

/// Live, feature `test-faults`: a destroyed device is `Lost { generation }`; `reacquire` rebinds
/// through the port to generation + 1; the superseded grant is refused for submission and
/// settlement; the successor settles once.
#[cfg(feature = "test-faults")]
#[test]
fn device_loss_rebinds_through_the_port() {
    let _guard = device_lock();
    let transient = Arc::new(TransientAdapter::new(TransientOptions::default()));
    let backend = GpuBackend::with_grant(transient.clone(), attributed(), DeviceOptions::default());
    if !live(&backend, "device-loss rebind") {
        return;
    }
    let cancel = CancellationToken::default();
    let mut sink = Image::new();
    backend.execute(&plan(), &request(), &NoSources, &cancel, &mut sink).expect("live before the loss");
    assert!(backend.simulate_device_loss());
    assert_eq!(
        backend.execute(&plan(), &request(), &NoSources, &cancel, &mut sink).map(|_| ()),
        Err(BackendError::DeviceLost { generation: 1 })
    );
    assert_eq!(backend.reacquire(&cancel), Ok(2));
    assert_eq!(backend.grant_info().map(|g| g.generation), Some(2));
    assert_eq!(backend.last_port_event(), Some(PortEvent::Rebound { generation: 2 }));
    let receipt = backend.execute(&plan(), &request(), &NoSources, &cancel, &mut sink).expect("successor executes");
    assert_eq!(receipt.backend.generation, 2);
    assert!(backend.release().is_ok());
    let c = transient.counters();
    assert_eq!((c.admitted, c.rebound, c.settled, c.outstanding), (2, 1, 1, 0), "{c:?}");

    // the superseded grant itself: refused for submission and settlement
    let old = transient.admit(&attributed(), &cancel).expect("admitted");
    let new = transient.rebind(&old, &cancel).expect("rebound");
    assert_eq!(new.generation(), old.generation() + 1);
    assert_eq!(transient.authorize(&old), Err(ClientRefusal::Lost { generation: old.generation() }));
    assert_eq!(GpuError::from(ClientRefusal::Lost { generation: 1 }), GpuError::DeviceLost { generation: 1 });
    assert_eq!(transient.settle(&old, Settlement::empty(SettleOutcome::Lost)), Err(SettleError::LateRelease));
    assert!(transient.settle(&new, Settlement::empty(SettleOutcome::Completed)).is_ok());
    assert_eq!(transient.settle(&new, Settlement::empty(SettleOutcome::Completed)), Err(SettleError::DoubleRelease));
}

/// Live, feature `shared-device`: `SharedDevice::new` keeps its signature and its backend holds a
/// Host-shaped grant whose residency budget comes from the caller's options; a rebind after a loss
/// is the host's (`DeviceLost`), never a device of render-gpu's own.
#[cfg(feature = "shared-device")]
#[test]
fn shared_device_backend_holds_a_host_grant_budgeted_from_options() {
    use hsk_studio_render_gpu::SharedDevice;
    let _guard = device_lock();
    let backends = if cfg!(windows) { wgpu::Backends::VULKAN | wgpu::Backends::DX12 } else { wgpu::Backends::PRIMARY };
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor { backends, flags: wgpu::InstanceFlags::empty(), ..Default::default() });
    let adapter = match block(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    })) {
        Ok(adapter) => adapter,
        Err(_) if !require_gpu() => {
            eprintln!("NOT_PROVEN: no adapter; shared-device grant not run");
            return;
        }
        Err(_) => panic!("NOT_PROVEN: no adapter; shared-device grant requires a GPU (tests/gpu-proof.toml require_gpu = true)"),
    };
    let (device, queue) = block(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("test host device"),
        required_limits: wgpu::Limits::downlevel_defaults(),
        ..Default::default()
    }))
    .expect("adapter present but no device");
    let options = DeviceOptions { residency_budget_bytes: 24 << 20, ..DeviceOptions::default() };
    let shared = SharedDevice::new(&device, &queue, adapter.get_info(), options).expect("shared device");
    let grant = shared.backend().grant_info().expect("host-shaped grant");
    assert_eq!((grant.issuer, grant.budget_source, grant.budget_bytes), (GrantIssuer::Host, BudgetSource::StudioDeclared, 24 << 20));
    assert_eq!(grant.operation, GpuOperation::Display);
    assert_eq!(shared.backend().residency_budget_bytes(), 24 << 20);
    eprintln!("GPU_CLIENT_SHARED: host device {} grant {:?}", adapter.get_info().backend, grant.id);
    let mut sink = Image::new();
    shared.backend().execute(&plan(), &request(), &NoSources, &CancellationToken::default(), &mut sink).expect("executes on the host device");
    assert_eq!(
        shared.backend().reacquire(&CancellationToken::default()),
        Err(GpuError::DeviceLost { generation: 1 }),
        "loss and recovery are the host's"
    );
    assert_eq!(shared.backend().port_counters().lost, 1);
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
