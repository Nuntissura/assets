//! GPU `RenderBackend` for the shared pigment render plan: admission (`admit`, before any device
//! work), source verification, then bounded `chunk x chunk` execution of one WGSL compute pass per
//! layer, read back into the caller's `TileSink`.
//!
//! Slice 1 scope: a flat plan (root group, or a single leaf) of `Solid` and `Tile` layers with
//! `Normal`/`Multiply`/`Screen`, opacity and fill opacity, straight alpha, `rgb_f32le` in the
//! plan's colour tag. Everything else is refused by `admit` as `Unsupported` (typed, before any
//! upload). There is no CPU fallback: no adapter is `Unavailable`, a lost device is `DeviceLost`.
//!
//! Numerics: the shader reproduces `hsk_studio_pigment::blend_math::composite_over` in f32. GPU
//! fused multiply-add and division may differ from the CPU reference in the last ulps, so the
//! backend reports class `Fitted` (tolerance-equal, not bit-exact) for the blend operators.
use crate::device::{DeviceInfo, DeviceOptions, GpuContext};
use crate::error::{GpuError, Reason};
use crate::kernel::{LayerParams, PARAMS_BYTES, ShaderMode, TEXEL_BYTES};
use crate::residency::{Residency, ResidentKey};
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BackendIdentity, ColourTag, DeviceClass, Exactness,
    ExecuteRequest, NoVector, Operator, PlanNode, PlanSources, RenderBackend, RenderPlan,
    RenderReceipt, SampleFormat, SourceKey, StudioBlendMode, TileInput, TilePlanes, TileSink,
    admit,
};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

pub const RENDERER_NAME: &str = "hsk-studio-render-gpu";
pub const RENDERER_VERSION: &str = "0.1";
/// Smallest chunk (texels) the executor will shrink to before reporting `BudgetExceeded`.
const MIN_CHUNK_TEXELS: u64 = 64;
const DEFAULT_POLL_LIMIT: Duration = Duration::from_secs(60);

/// True for the blend modes the slice-1 shader implements.
pub fn gpu_blend_supported(mode: StudioBlendMode) -> bool {
    shader_mode(mode).is_some()
}
fn shader_mode(mode: StudioBlendMode) -> Option<ShaderMode> {
    match mode {
        StudioBlendMode::Normal => Some(ShaderMode::Normal),
        StudioBlendMode::Multiply => Some(ShaderMode::Multiply),
        StudioBlendMode::Screen => Some(ShaderMode::Screen),
        _ => None,
    }
}

/// Counters of one successful execution (device-side detail the shared receipt has no field for).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuStats {
    pub bytes_uploaded: u64,
    pub bytes_read_back: u64,
    /// Highest simultaneous byte count charged to residency during the call.
    pub peak_resident_bytes: u64,
    pub evictions: u64,
    pub dispatches: u64,
    pub chunk_width: u32,
    pub chunk_height: u32,
}

// ---- prepared (verified) plan -----------------------------------------------------------------

enum Source<'a> {
    Solid([f32; 4]),
    Tile(TilePlanes<'a>),
}
struct PLayer<'a> {
    source: Source<'a>,
    bounds: Rect,
    mode: ShaderMode,
    /// `opacity * fill_opacity` (fill folded in as a product, class Approximate, as render-cpu).
    opacity: f32,
}
struct Prepared<'a> {
    layers: Vec<PLayer<'a>>,
    nodes_executed: u64,
}

fn unsupported(operator: &'static str) -> GpuError {
    GpuError::Backend(BackendError::Unsupported {
        operator,
        exactness: Exactness::Unsupported,
    })
}
fn mismatch(key: SourceKey) -> GpuError {
    GpuError::Backend(BackendError::SourceMismatch(key))
}
fn overflow() -> GpuError {
    GpuError::Backend(BackendError::Overflow)
}
fn f32_at(bytes: &[u8], at: usize) -> f32 {
    f32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}
fn tile_pixel(p: &TilePlanes<'_>, x: u32, y: u32) -> [f32; 4] {
    let c = y as usize * p.colour_stride_bytes as usize + x as usize * 12;
    let a = y as usize * p.alpha_stride_bytes as usize + x as usize * 4;
    [
        f32_at(p.colour, c),
        f32_at(p.colour, c + 4),
        f32_at(p.colour, c + 8),
        f32_at(p.alpha, a),
    ]
}
fn plane_ok(bytes: &[u8], stride: u64, width: u32, height: u32, bpp: u64) -> bool {
    let row = u64::from(width) * bpp;
    stride >= row
        && stride.is_multiple_of(4)
        && stride
            .checked_mul(u64::from(height))
            .is_some_and(|size| u64::try_from(bytes.len()).ok() == Some(size))
}

/// Shape, plane hashes and values of a resolved tile against the plan's declaration.
fn verify_tile(
    input: &TileInput,
    planes: &TilePlanes<'_>,
    plan_space: ColourTag,
) -> Result<(), GpuError> {
    let key = input.key;
    if input.alpha != AlphaConvention::Straight {
        return Err(unsupported("alpha_premultiplied"));
    }
    if !planes.matches(input, plan_space)
        || !plane_ok(
            planes.colour,
            planes.colour_stride_bytes,
            planes.width,
            planes.height,
            12,
        )
        || !plane_ok(
            planes.alpha,
            planes.alpha_stride_bytes,
            planes.width,
            planes.height,
            4,
        )
    {
        return Err(mismatch(key));
    }
    if hsk_studio_prism::profile_hash(planes.colour) != planes.colour_sha256
        || hsk_studio_prism::profile_hash(planes.alpha) != planes.alpha_sha256
    {
        return Err(mismatch(key));
    }
    let mut out_of_unit = false;
    for y in 0..planes.height {
        for x in 0..planes.width {
            let [r, g, b, a] = tile_pixel(planes, x, y);
            if ![r, g, b, a].iter().all(|v| v.is_finite()) || !(0.0..=1.0).contains(&a) {
                return Err(mismatch(key));
            }
            out_of_unit |= ![r, g, b].iter().all(|v| (0.0..=1.0).contains(v));
        }
    }
    if out_of_unit {
        return Err(unsupported("sample_range_outside_unit"));
    }
    Ok(())
}

fn leaf<'a>(
    plan: &RenderPlan<NoVector>,
    index: usize,
    sources: &'a dyn PlanSources,
) -> Result<(Source<'a>, Rect), GpuError> {
    match &plan.nodes[index] {
        PlanNode::Tile(t) => {
            let planes = sources
                .tile(t.key)
                .ok_or(GpuError::Backend(BackendError::UnresolvedSource(t.key)))?;
            verify_tile(t, &planes, plan.target.space)?;
            t.placement.end().map_err(|_| overflow())?;
            Ok((Source::Tile(planes), t.placement))
        }
        PlanNode::Solid { rect, rgba } => {
            if !rgba
                .iter()
                .all(|c| c.is_finite() && (0.0..=1.0).contains(c))
            {
                return Err(unsupported("sample_range_outside_unit"));
            }
            rect.end().map_err(|_| overflow())?;
            Ok((Source::Solid(*rgba), *rect))
        }
        PlanNode::Vector(never) => match *never {},
        PlanNode::Group(_) => Err(unsupported("group_nested")),
    }
}

fn prepare<'a>(
    plan: &RenderPlan<NoVector>,
    sources: &'a dyn PlanSources,
) -> Result<Prepared<'a>, GpuError> {
    let target = plan.target;
    if target.alpha != AlphaConvention::Straight {
        return Err(unsupported("alpha_premultiplied"));
    }
    if target.format != SampleFormat::RgbF32Le {
        return Err(unsupported("sample_format"));
    }
    let root = plan.root.0 as usize;
    let mut layers = Vec::new();
    let mut inputs: Vec<usize> = Vec::new();
    match &plan.nodes[root] {
        PlanNode::Group(group) => {
            for l in &group.layers {
                let mode = shader_mode(l.mode).ok_or_else(|| unsupported("blend_mode"))?;
                if l.mask.is_some() {
                    return Err(unsupported("mask"));
                }
                let index = l.input.0 as usize;
                let (source, bounds) = leaf(plan, index, sources)?;
                if !inputs.contains(&index) {
                    inputs.push(index);
                }
                layers.push(PLayer {
                    source,
                    bounds,
                    mode,
                    opacity: l.opacity * l.fill_opacity,
                });
            }
        }
        _ => {
            // A leaf root is one Normal, opacity-1 layer over transparent black.
            let (source, bounds) = leaf(plan, root, sources)?;
            layers.push(PLayer {
                source,
                bounds,
                mode: ShaderMode::Normal,
                opacity: 1.0,
            });
        }
    }
    Ok(Prepared {
        layers,
        nodes_executed: 1 + inputs.len() as u64,
    })
}

// ---- backend ------------------------------------------------------------------------------------

enum State {
    Ready(Box<GpuContext>),
    Lost { generation: u64, adapter: String },
    Unavailable(Reason),
}
struct Inner {
    state: State,
    residency: Residency,
    /// Generation of the most recent device (0 = none yet); the next acquire is `last + 1`.
    last_generation: u64,
    stats: Option<GpuStats>,
}

/// GPU backend over the pigment `RenderBackend` port. Device use is serialised by one mutex.
pub struct GpuBackend {
    options: DeviceOptions,
    inner: Mutex<Inner>,
}

impl GpuBackend {
    /// Try to acquire a headless device now. Never fails: when there is none the backend exists in
    /// the `Unavailable` state and every `execute` reports it (after admission) as a typed error.
    pub fn new(options: DeviceOptions) -> Self {
        let acquired = GpuContext::acquire(&options, &CancellationToken::default());
        let (state, last_generation) = match acquired {
            Ok(ctx) => {
                let generation = ctx.generation();
                (State::Ready(Box::new(ctx)), generation)
            }
            Err(GpuError::Unavailable(reason)) => (State::Unavailable(reason), 0),
            Err(_) => (State::Unavailable(Reason::NoDevice), 0),
        };
        Self::with_state(options, state, last_generation)
    }

    /// Fault-injection constructor: a backend whose device of `generation` was already lost (the
    /// state the real device-lost callback latches). Lets supervisors and tests exercise
    /// `DeviceLost` and `reacquire` without hardware.
    #[doc(hidden)]
    pub fn with_injected_loss(options: DeviceOptions, generation: u64) -> Self {
        let state = State::Lost {
            generation,
            adapter: String::from("injected"),
        };
        Self::with_state(options, state, generation)
    }

    fn with_state(options: DeviceOptions, state: State, last_generation: u64) -> Self {
        Self {
            options,
            inner: Mutex::new(Inner {
                state,
                residency: Residency::new(options.residency_budget_bytes),
                last_generation,
                stats: None,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn options(&self) -> DeviceOptions {
        self.options
    }
    /// Description of the live device, `None` when unavailable or lost.
    pub fn device_info(&self) -> Option<DeviceInfo> {
        match &self.lock().state {
            State::Ready(ctx) if !ctx.is_lost() => Some(ctx.info().clone()),
            _ => None,
        }
    }
    /// Why no device is available (`None` while a device is live or after a loss).
    pub fn unavailable_reason(&self) -> Option<Reason> {
        match &self.lock().state {
            State::Unavailable(reason) => Some(*reason),
            _ => None,
        }
    }
    /// Generation of the most recent device; 0 before any device existed.
    pub fn generation(&self) -> u64 {
        self.lock().last_generation
    }
    pub fn is_lost(&self) -> bool {
        match &self.lock().state {
            State::Ready(ctx) => ctx.is_lost(),
            State::Lost { .. } => true,
            State::Unavailable(_) => false,
        }
    }
    /// Counters of the most recent successful execution.
    pub fn last_stats(&self) -> Option<GpuStats> {
        self.lock().stats
    }
    /// Bytes currently charged to residency (0 whenever no execution runs).
    pub fn resident_bytes(&self) -> u64 {
        self.lock().residency.used()
    }
    /// Destroy the live device through the real wgpu path and latch the loss; the next `execute`
    /// returns `DeviceLost{generation}`. Returns `false` when there was no live device.
    pub fn simulate_device_loss(&self) -> bool {
        match &self.lock().state {
            State::Ready(ctx) if !ctx.is_lost() => {
                ctx.simulate_device_loss();
                true
            }
            _ => false,
        }
    }
    /// Explicit recovery: acquire a successor device (generation + 1) and drop all residency.
    /// Never called implicitly by `execute`. On failure the previous state is kept (a lost device
    /// stays lost, so stale generations are still refused).
    pub fn reacquire(&self, cancel: &CancellationToken) -> Result<u64, GpuError> {
        let mut inner = self.lock();
        let ctx = GpuContext::acquire_next(&self.options, cancel, inner.last_generation)?;
        let generation = ctx.generation();
        inner.state = State::Ready(Box::new(ctx));
        inner.last_generation = generation;
        inner.residency.clear();
        Ok(generation)
    }

    /// Execute with the detailed internal error and the device-side counters. Order: admission
    /// (cancel, stale revision, plan validation, per-operator class), source verification, device
    /// state, then chunks. A bad plan or source therefore never reaches the device, and a receipt
    /// exists only after the last chunk was accepted by the sink.
    pub fn execute_detailed(
        &self,
        plan: &RenderPlan<NoVector>,
        request: &ExecuteRequest,
        sources: &dyn PlanSources,
        cancel: &CancellationToken,
        sink: &mut dyn TileSink,
    ) -> Result<(RenderReceipt, GpuStats), GpuError> {
        let ledger = admit::<NoVector>(plan, self, request, cancel)?;
        let prepared = prepare(plan, sources)?;
        let digest = plan.digest().map_err(|_| GpuError::Gpu("plan_digest"))?;

        let mut guard = self.lock();
        let inner = &mut *guard;
        if let State::Ready(ctx) = &inner.state
            && ctx.is_lost()
        {
            let generation = ctx.generation();
            inner.state = State::Lost {
                generation,
                adapter: ctx.info().text(),
            };
        }
        let ctx = match &inner.state {
            State::Ready(ctx) => ctx,
            State::Lost { generation, .. } => {
                return Err(GpuError::DeviceLost {
                    generation: *generation,
                });
            }
            State::Unavailable(reason) => return Err(GpuError::Unavailable(*reason)),
        };

        let residency = &mut inner.residency;
        let mark = residency.mark();
        residency.reset_peak();
        let evictions_before = residency.evictions();
        let mut stats = GpuStats::default();
        let result = run(
            ctx,
            residency,
            &self.options,
            &prepared,
            plan,
            request,
            cancel,
            sink,
            &mut stats,
        );
        stats.peak_resident_bytes = residency.peak_used();
        stats.evictions = residency.evictions() - evictions_before;
        residency.release_since(mark);
        let chunks = match result {
            Ok(chunks) => chunks,
            // A lost device is the root cause of any follow-up GPU failure.
            Err(GpuError::Gpu(_)) if ctx.is_lost() => {
                return Err(GpuError::DeviceLost {
                    generation: ctx.generation(),
                });
            }
            Err(error) => return Err(error),
        };
        let receipt = RenderReceipt {
            plan_revision: plan.revision,
            plan_digest: digest,
            backend: identity_of(&inner.state),
            region: plan.target.extent,
            target: plan.target,
            exactness: ledger,
            chunks,
            nodes_executed: prepared.nodes_executed,
            peak_working_bytes: stats.peak_resident_bytes,
        };
        inner.stats = Some(stats);
        Ok((receipt, stats))
    }
}

fn identity_of(state: &State) -> BackendIdentity {
    let (adapter, generation) = match state {
        State::Ready(ctx) => (ctx.info().text(), ctx.generation()),
        State::Lost {
            generation,
            adapter,
        } => (adapter.clone(), *generation),
        State::Unavailable(_) => (String::new(), 0),
    };
    BackendIdentity {
        name: RENDERER_NAME,
        version: RENDERER_VERSION,
        device: DeviceClass::Gpu,
        adapter,
        generation,
    }
}

impl RenderBackend<NoVector> for GpuBackend {
    fn identity(&self) -> BackendIdentity {
        identity_of(&self.lock().state)
    }
    fn exactness(&self, operator: Operator) -> Exactness {
        match operator {
            // Pure data movement: f32 bits survive upload and read-back unchanged.
            Operator::Tile | Operator::Solid => Exactness::Exact,
            // Same published formula as pigment; f32 GPU arithmetic is tolerance-equal, not
            // bit-exact (fused multiply-add, division rounding).
            Operator::Blend(mode) if gpu_blend_supported(mode) => Exactness::Fitted,
            Operator::FillOpacity => Exactness::Approximate,
            _ => Exactness::Unsupported,
        }
    }
    fn execute(
        &self,
        plan: &RenderPlan<NoVector>,
        request: &ExecuteRequest,
        sources: &dyn PlanSources,
        cancel: &CancellationToken,
        sink: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        self.execute_detailed(plan, request, sources, cancel, sink)
            .map(|(receipt, _)| receipt)
            .map_err(BackendError::from)
    }
}

// ---- device execution ---------------------------------------------------------------------------

/// Create device objects with validation and out-of-memory errors captured as typed results.
fn scoped<T>(
    device: &wgpu::Device,
    requested: u64,
    budget: u64,
    create: impl FnOnce() -> T,
) -> Result<T, GpuError> {
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let value = create();
    let oom = crate::device::block_on_bounded(device.pop_error_scope(), None);
    let validation = crate::device::block_on_bounded(device.pop_error_scope(), None);
    match (oom, validation) {
        (Some(None), Some(None)) => Ok(value),
        (Some(Some(_)), _) => Err(GpuError::BudgetExceeded { requested, budget }),
        _ => Err(GpuError::Gpu("device_validation")),
    }
}

fn texel_bytes(width: u32, height: u32) -> u64 {
    u64::from(width) * u64::from(height) * TEXEL_BYTES
}

#[allow(clippy::too_many_arguments)]
fn run(
    ctx: &GpuContext,
    residency: &mut Residency,
    options: &DeviceOptions,
    prepared: &Prepared<'_>,
    plan: &RenderPlan<NoVector>,
    request: &ExecuteRequest,
    cancel: &CancellationToken,
    sink: &mut dyn TileSink,
    stats: &mut GpuStats,
) -> Result<u64, GpuError> {
    let canceled = || GpuError::Backend(BackendError::Canceled);
    let deadline = || GpuError::Backend(BackendError::DeadlineExceeded);
    ctx.check_alive()?;
    let _ = ctx.take_fault();

    // ---- bounded sizing: the chunk shrinks until every device buffer fits both budgets ----
    let extent = plan.target.extent;
    let (extent_x1, extent_y1) = extent.end().map_err(|_| overflow())?;
    let has_tiles = prepared
        .layers
        .iter()
        .any(|l| matches!(l.source, Source::Tile(_)));
    let per_texel = TEXEL_BYTES * if has_tiles { 3 } else { 2 };
    let fixed = PARAMS_BYTES + if has_tiles { 0 } else { TEXEL_BYTES };
    let limit = request.max_working_bytes.min(residency.budget_bytes());
    let by_budget = limit.saturating_sub(fixed) / per_texel;
    let by_buffer = ctx.max_buffer_bytes / TEXEL_BYTES;
    let max_texels = by_budget.min(by_buffer);
    if max_texels < MIN_CHUNK_TEXELS {
        return Err(GpuError::BudgetExceeded {
            requested: fixed + per_texel * MIN_CHUNK_TEXELS,
            budget: limit,
        });
    }
    let side = u32::try_from(max_texels.isqrt())
        .unwrap_or(u32::MAX)
        .min(options.chunk_side)
        .max(1);
    let chunk_w = side.min(extent.width);
    let chunk_h = side.min(extent.height);
    let chunk_bytes = texel_bytes(chunk_w, chunk_h);
    let src_bytes = if has_tiles { chunk_bytes } else { TEXEL_BYTES };
    stats.chunk_width = chunk_w;
    stats.chunk_height = chunk_h;

    // ---- charge residency first, then create the buffers ----
    for (name, bytes) in [
        ("dst", chunk_bytes),
        ("staging", chunk_bytes),
        ("src", src_bytes),
        ("params", PARAMS_BYTES),
    ] {
        residency.reserve(ResidentKey::Scratch(name), bytes)?;
    }
    let total = 2 * chunk_bytes + src_bytes + PARAMS_BYTES;
    let device = &ctx.device;
    let queue = &ctx.queue;
    let (dst, staging, src, params, bind) = scoped(device, total, limit, || {
        let buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let dst = buffer(
            "hsk-studio-render-gpu dst",
            chunk_bytes,
            wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
        );
        let staging = buffer(
            "hsk-studio-render-gpu staging",
            chunk_bytes,
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        );
        let src = buffer(
            "hsk-studio-render-gpu src",
            src_bytes,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let params = buffer(
            "hsk-studio-render-gpu params",
            PARAMS_BYTES,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let bind = ctx.kernel.bind(device, &params, &src, &dst);
        (dst, staging, src, params, bind)
    })?;

    let mut upload: Vec<u8> = Vec::new();
    let mut colour: Vec<u8> = Vec::new();
    let mut alpha: Vec<u8> = Vec::new();
    let mut chunks = 0_u64;
    let side_w = i64::from(chunk_w);
    let side_h = i64::from(chunk_h);
    let mut cy = extent.y;
    while cy < extent_y1 {
        let mut cx = extent.x;
        while cx < extent_x1 {
            cancel.check().map_err(|_| canceled())?;
            if request.deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(deadline());
            }
            ctx.check_alive()?;
            let rect = Rect {
                x: cx,
                y: cy,
                width: side_w.min(extent_x1 - cx) as u32,
                height: side_h.min(extent_y1 - cy) as u32,
            };
            let count = texel_bytes(rect.width, rect.height);

            // Transparent backdrop for this chunk.
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hsk-studio-render-gpu clear"),
            });
            encoder.clear_buffer(&dst, 0, Some(count));
            queue.submit(Some(encoder.finish()));

            for layer in &prepared.layers {
                cancel.check().map_err(|_| canceled())?;
                if layer.opacity <= 0.0 {
                    continue;
                }
                let solid = match &layer.source {
                    Source::Solid(rgba) if rgba[3] <= 0.0 => continue,
                    Source::Solid(rgba) => Some(*rgba),
                    Source::Tile(_) => None,
                };
                let Some(region) = layer.bounds.intersect(rect).map_err(|_| overflow())? else {
                    continue;
                };
                if let Source::Tile(planes) = &layer.source {
                    upload.clear();
                    for y in 0..region.height {
                        for x in 0..region.width {
                            let px = (region.x + i64::from(x) - layer.bounds.x) as u32;
                            let py = (region.y + i64::from(y) - layer.bounds.y) as u32;
                            for v in tile_pixel(planes, px, py) {
                                upload.extend_from_slice(&v.to_ne_bytes());
                            }
                        }
                    }
                    queue.write_buffer(&src, 0, &upload);
                    stats.bytes_uploaded += upload.len() as u64;
                }
                let layer_params = LayerParams {
                    chunk_w: rect.width,
                    chunk_h: rect.height,
                    region: [
                        (region.x - rect.x) as u32,
                        (region.y - rect.y) as u32,
                        region.width,
                        region.height,
                    ],
                    mode: layer.mode,
                    solid,
                    opacity: layer.opacity,
                };
                queue.write_buffer(&params, 0, &layer_params.to_bytes());
                let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("hsk-studio-render-gpu layer"),
                });
                ctx.kernel
                    .dispatch(&mut encoder, &bind, region.width, region.height);
                queue.submit(Some(encoder.finish()));
                stats.dispatches += 1;
            }

            // Read the finished chunk back.
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hsk-studio-render-gpu readback"),
            });
            encoder.copy_buffer_to_buffer(&dst, 0, &staging, 0, count);
            queue.submit(Some(encoder.finish()));
            let (tx, rx) = std::sync::mpsc::channel();
            staging
                .slice(0..count)
                .map_async(wgpu::MapMode::Read, move |outcome| {
                    let _ = tx.send(outcome);
                });
            let wait = request.deadline.map_or(DEFAULT_POLL_LIMIT, |d| {
                d.saturating_duration_since(Instant::now())
                    .max(Duration::from_millis(1))
            });
            let polled = device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(wait),
            });
            ctx.check_alive()?;
            if polled.is_err() {
                return Err(if request.deadline.is_some_and(|d| Instant::now() >= d) {
                    deadline()
                } else {
                    GpuError::Gpu("device_timeout")
                });
            }
            if !matches!(rx.try_recv(), Ok(Ok(()))) {
                return Err(GpuError::Gpu("readback_map_failed"));
            }
            colour.clear();
            alpha.clear();
            {
                let view = staging.slice(0..count).get_mapped_range();
                for texel in view.chunks_exact(TEXEL_BYTES as usize) {
                    let v = |i: usize| {
                        f32::from_ne_bytes([texel[i], texel[i + 1], texel[i + 2], texel[i + 3]])
                    };
                    for i in [0, 4, 8] {
                        colour.extend_from_slice(&v(i).to_le_bytes());
                    }
                    alpha.extend_from_slice(&v(12).to_le_bytes());
                }
            }
            staging.unmap();
            stats.bytes_read_back += count;
            if ctx.take_fault() {
                return Err(GpuError::Gpu("device_fault"));
            }
            sink.accept(rect, &colour, &alpha)
                .map_err(|e| GpuError::Backend(BackendError::Sink(e)))?;
            chunks += 1;
            cx += side_w;
        }
        cy += side_h;
    }
    Ok(chunks)
}
