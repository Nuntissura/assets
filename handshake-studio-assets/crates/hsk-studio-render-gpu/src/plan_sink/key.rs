//! Key pipelines on the GPU backend (lane SMB-AE-KEY-TRACK-3): the envelope entry
//! `LayerExtension::keys` on a TILE layer. The stages run as `key.wgsl` compute passes (a binary32
//! transcription of pigment `keying`, binary64) over the layer's tile inside the plan extent, the
//! result (colour despilled, alpha times the matte) is read back and the plan is rewritten so the
//! layer reads that keyed tile (a fresh source key, its own hashes) without the key entry; the
//! normal GPU path then composites it (mask, Blend-If, mode, opacity after the key, as render-cpu).
//! Receipt digest and exactness ledger are the original plan's.
//!
//! Supported stage kinds ([`gpu_key_supported`]): chroma, luma, colour range, colour key,
//! colour-difference, linear colour, extract, levels, choke/spread (radius <= [`MORPH_MAX`]),
//! feather (taps <= [`FEATHER_MAX_TAPS`]), edge-aware smoothing (radius <= [`SMOOTH_MAX`]),
//! despill, unmult. Inner/outer key, key cleaner and wire removal are declared Unsupported (shared
//! admission refuses them under their keys); a keyed layer whose input is not a tile, or a tile
//! another layer also reads, is the typed refusal [`KEY_GPU_INPUT`]; a radius above the GPU bound
//! is [`KEY_GPU_RADIUS`]; all before any device work. Measured agreement with pigment: see
//! `tests/gpu_keying.rs` (tolerance stated there).
#![deny(clippy::arithmetic_side_effects)]
use super::{overflow, tile_pixel, unsupported, verify_tile, DEFAULT_POLL_LIMIT};
use crate::device::GpuContext;
use crate::error::GpuError;
use crate::pipeline::{ErrorScopes, Stage, guarded};
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::coverage_math::feather_kernel;
use hsk_studio_pigment::keying::{
    ChromaSpace, ExtractChannel, KeyKind, KeyStage, LinearMatch, LumaKeyType, MatteCombine, RangeSpace, SpillLimit,
    SpillScreen,
};
use hsk_studio_pigment::render_plan::{
    BackendError, ExecuteRequest, NoVector, PlanExtensions, PlanNode, PlanSources, RenderPlan, SourceKey, TileInput,
    TilePlanes,
};
use std::collections::BTreeMap;

/// A keyed layer whose input is not a tile node, or whose tile node another layer also reads.
pub(super) const KEY_GPU_INPUT: &str = "key_gpu_input";
/// A choke/spread, smoothing radius or feather above the GPU bounds.
pub(super) const KEY_GPU_RADIUS: &str = "key_gpu_radius";
pub(super) const MORPH_MAX: u32 = 32;
pub(super) const SMOOTH_MAX: u32 = 16;
pub(super) const FEATHER_MAX_TAPS: usize = 129;
const LABEL: &str = "hsk-studio-render-gpu key";
const PARAMS_BYTES: u64 = 96;

/// Whether this backend runs stage kind `kind` (see the module docs).
pub(crate) fn gpu_key_supported(kind: KeyKind) -> bool {
    matches!(
        kind,
        KeyKind::Chroma
            | KeyKind::Luma
            | KeyKind::ColourRange
            | KeyKind::ColourKey
            | KeyKind::ColourDifference
            | KeyKind::LinearColour
            | KeyKind::Extract
            | KeyKind::Levels
            | KeyKind::ChokeSpread
            | KeyKind::Feather
            | KeyKind::EdgeSmooth
            | KeyKind::Despill
            | KeyKind::Unmult
    )
}

/// Does the envelope key any layer?
pub(super) fn has_keys(extensions: Option<&PlanExtensions>) -> bool {
    extensions.is_some_and(|e| e.layers.iter().any(|l| !l.keys.is_empty()))
}

/// One keyed layer: its tile node and stages.
struct Job {
    node: usize,
    stages: Vec<KeyStage>,
}

/// Nodes the root reaches (inputs are earlier nodes, so one backward pass suffices).
fn reachable(plan: &RenderPlan<NoVector>) -> Vec<bool> {
    let mut r = vec![false; plan.nodes.len()];
    if let Some(x) = r.get_mut(plan.root.0 as usize) {
        *x = true;
    }
    for i in (0..plan.nodes.len()).rev() {
        if !r.get(i).copied().unwrap_or(false) {
            continue;
        }
        if let Some(PlanNode::Group(g)) = plan.nodes.get(i) {
            for l in &g.layers {
                if let Some(x) = r.get_mut(l.input.0 as usize) {
                    *x = true;
                }
            }
        }
    }
    r
}

/// Host-side refusals before any device work: tile input read by this layer only, radii in bounds.
fn jobs(plan: &RenderPlan<NoVector>, ext: &PlanExtensions) -> Result<Vec<Job>, GpuError> {
    let reach = reachable(plan);
    let mut readers: BTreeMap<usize, usize> = BTreeMap::new();
    for (i, n) in plan.nodes.iter().enumerate() {
        if let PlanNode::Group(g) = n
            && reach.get(i).copied().unwrap_or(false)
        {
            for l in &g.layers {
                let e = readers.entry(l.input.0 as usize).or_insert(0);
                *e = e.saturating_add(1);
            }
        }
    }
    let mut out = Vec::new();
    for e in ext.layers.iter().filter(|e| !e.keys.is_empty()) {
        if !reach.get(e.group.0 as usize).copied().unwrap_or(false) {
            continue;
        }
        let Some(PlanNode::Group(g)) = plan.nodes.get(e.group.0 as usize) else {
            return Err(unsupported("plan_shape"));
        };
        let layer = g.layers.get(e.layer as usize).ok_or_else(|| unsupported("plan_shape"))?;
        let node = layer.input.0 as usize;
        if !matches!(plan.nodes.get(node), Some(PlanNode::Tile(_))) || readers.get(&node).copied().unwrap_or(0) != 1 {
            return Err(unsupported(KEY_GPU_INPUT));
        }
        for s in &e.keys {
            let ok = match s {
                KeyStage::ChokeSpread { radius_px } => radius_px.unsigned_abs() <= MORPH_MAX,
                KeyStage::EdgeSmooth { radius_px, .. } => *radius_px <= SMOOTH_MAX,
                KeyStage::Feather { radius_px } => feather_kernel(*radius_px).is_ok_and(|t| t.len() <= FEATHER_MAX_TAPS),
                _ => gpu_key_supported(s.kind()),
            };
            if !ok {
                return Err(unsupported(if gpu_key_supported(s.kind()) { KEY_GPU_RADIUS } else { s.key() }));
            }
        }
        out.push(Job { node, stages: e.keys.clone() });
    }
    Ok(out)
}

/// Refusals that need no device (run before the device state is consulted).
pub(super) fn check(plan: &RenderPlan<NoVector>, ext: &PlanExtensions) -> Result<(), GpuError> {
    jobs(plan, ext).map(|_| ())
}

/// One keyed tile, owned.
struct OwnedTile {
    key: SourceKey,
    width: u32,
    height: u32,
    colour: Vec<u8>,
    alpha: Vec<u8>,
    colour_sha256: [u8; 32],
    alpha_sha256: [u8; 32],
}

/// The rewritten plan: keyed tiles under fresh keys, the envelope without key entries.
pub(super) struct Keyed {
    pub(super) plan: RenderPlan<NoVector>,
    pub(super) ext: PlanExtensions,
    tiles: Vec<OwnedTile>,
}

/// The caller's sources plus the keyed tiles.
pub(super) struct KeyedSources<'s> {
    inner: &'s dyn PlanSources,
    keyed: &'s Keyed,
}
impl Keyed {
    pub(super) fn sources<'s>(&'s self, inner: &'s dyn PlanSources) -> KeyedSources<'s> {
        KeyedSources { inner, keyed: self }
    }
}
impl PlanSources for KeyedSources<'_> {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        if let Some(t) = self.keyed.tiles.iter().find(|t| t.key == key) {
            let template = self.keyed.plan.target.space;
            return Some(TilePlanes {
                width: t.width,
                height: t.height,
                colour: &t.colour,
                colour_stride_bytes: u64::from(t.width).saturating_mul(12),
                alpha: &t.alpha,
                alpha_stride_bytes: u64::from(t.width).saturating_mul(4),
                colour_sha256: t.colour_sha256,
                alpha_sha256: t.alpha_sha256,
                format: hsk_studio_pigment::render_plan::SampleFormat::RgbF32Le,
                alpha_convention: hsk_studio_pigment::render_plan::AlphaConvention::Straight,
                space: template,
            });
        }
        self.inner.tile(key)
    }
    fn mask(&self, key: SourceKey) -> Option<hsk_studio_pigment::Mask<'_>> {
        self.inner.mask(key)
    }
}

/// Keys every keyed tile on the device and rewrites the plan (module docs).
pub(super) fn run(
    ctx: &GpuContext,
    plan: &RenderPlan<NoVector>,
    ext: &PlanExtensions,
    sources: &dyn PlanSources,
    request: &ExecuteRequest,
    cancel: &CancellationToken,
) -> Result<Keyed, GpuError> {
    let jobs = jobs(plan, ext)?;
    let mut next_key = plan
        .nodes
        .iter()
        .filter_map(|n| match n {
            PlanNode::Tile(t) => Some(t.key.0),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let pipes = Pipes::new(ctx)?;
    let mut out_plan = plan.clone();
    let mut tiles = Vec::with_capacity(jobs.len());
    for job in &jobs {
        cancel.check().map_err(|_| GpuError::Backend(BackendError::Canceled))?;
        let Some(PlanNode::Tile(t)) = plan.nodes.get(job.node) else {
            return Err(unsupported(KEY_GPU_INPUT));
        };
        let planes = sources.tile(t.key).ok_or(GpuError::Backend(BackendError::UnresolvedSource(t.key)))?;
        verify_tile(t, &planes, plan.target.space, false)?;
        let Some(rect) = t.placement.intersect(plan.target.extent).map_err(|_| overflow())? else {
            continue;
        };
        let (dx, dy) = (
            u32::try_from(rect.x.checked_sub(t.placement.x).ok_or_else(overflow)?).map_err(|_| overflow())?,
            u32::try_from(rect.y.checked_sub(t.placement.y).ok_or_else(overflow)?).map_err(|_| overflow())?,
        );
        let n = usize::try_from(u64::from(rect.width).checked_mul(u64::from(rect.height)).ok_or_else(overflow)?)
            .map_err(|_| overflow())?;
        let budget = u64::try_from(n).map_err(|_| overflow())?.checked_mul(64).ok_or_else(overflow)?;
        if budget > request.max_working_bytes {
            return Err(GpuError::Backend(BackendError::BudgetExceeded { requested: budget, limit: request.max_working_bytes }));
        }
        let mut plane = Vec::with_capacity(n);
        for y in 0..rect.height {
            for x in 0..rect.width {
                plane.push(tile_pixel(&planes, x.saturating_add(dx), y.saturating_add(dy)));
            }
        }
        let (colour, matte) = pipes.key_plane(ctx, &plane, rect, &job.stages, request)?;
        let (mut cbytes, mut abytes) = (Vec::with_capacity(n.saturating_mul(12)), Vec::with_capacity(n.saturating_mul(4)));
        for (p, m) in colour.iter().zip(&matte) {
            for c in &p[..3] {
                cbytes.extend_from_slice(&c.to_le_bytes());
            }
            let a = (f64::from(p[3]) * f64::from(*m)) as f32;
            abytes.extend_from_slice(&a.to_le_bytes());
        }
        next_key = next_key.checked_add(1).ok_or_else(overflow)?;
        let key = SourceKey(next_key);
        let owned = OwnedTile {
            key,
            width: rect.width,
            height: rect.height,
            colour_sha256: hsk_studio_prism::profile_hash(&cbytes),
            alpha_sha256: hsk_studio_prism::profile_hash(&abytes),
            colour: cbytes,
            alpha: abytes,
        };
        if let Some(PlanNode::Tile(slot)) = out_plan.nodes.get_mut(job.node) {
            *slot = TileInput {
                key,
                placement: rect,
                format: t.format,
                alpha: t.alpha,
                colour_sha256: owned.colour_sha256,
                alpha_sha256: owned.alpha_sha256,
            };
        }
        tiles.push(owned);
    }
    let mut out_ext = ext.clone();
    for e in &mut out_ext.layers {
        e.keys.clear();
    }
    Ok(Keyed { plan: out_plan, ext: out_ext, tiles })
}

/// The compute pipelines of `key.wgsl` with their shared bind layout.
struct Pipes {
    layout: wgpu::BindGroupLayout,
    point: wgpu::ComputePipeline,
    morph: wgpu::ComputePipeline,
    blur: wgpu::ComputePipeline,
    smooth: wgpu::ComputePipeline,
}

fn fault(e: crate::pipeline::PipelineFault) -> GpuError {
    e.error()
}

impl Pipes {
    fn new(ctx: &GpuContext) -> Result<Self, GpuError> {
        let device = &ctx.device;
        let module = guarded(device, LABEL, Stage::Module, || {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(LABEL),
                source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!("../key.wgsl"))),
            })
        })
        .map_err(fault)?;
        let entry = |binding: u32, ty: wgpu::BufferBindingType| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let layout = guarded(device, LABEL, Stage::Pipeline, || {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(LABEL),
                entries: &[
                    entry(0, wgpu::BufferBindingType::Uniform),
                    entry(1, wgpu::BufferBindingType::Storage { read_only: false }),
                    entry(2, wgpu::BufferBindingType::Storage { read_only: true }),
                    entry(3, wgpu::BufferBindingType::Storage { read_only: false }),
                    entry(4, wgpu::BufferBindingType::Storage { read_only: true }),
                ],
            })
        })
        .map_err(fault)?;
        let pl = guarded(device, LABEL, Stage::Pipeline, || {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(LABEL),
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            })
        })
        .map_err(fault)?;
        let make = |entry_point: &'static str| {
            guarded(device, LABEL, Stage::Pipeline, || {
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(LABEL),
                    layout: Some(&pl),
                    module: &module,
                    entry_point: Some(entry_point),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    cache: None,
                })
            })
            .map_err(fault)
        };
        Ok(Self { point: make("key_point")?, morph: make("key_morph")?, blur: make("key_blur")?, smooth: make("key_smooth")?, layout })
    }

    /// Runs `stages` over `plane` (straight RGBA of `rect`): the keyed colour and the matte.
    fn key_plane(
        &self,
        ctx: &GpuContext,
        plane: &[[f32; 4]],
        rect: Rect,
        stages: &[KeyStage],
        request: &ExecuteRequest,
    ) -> Result<(Vec<[f32; 4]>, Vec<f32>), GpuError> {
        let device = &ctx.device;
        let n = u64::try_from(plane.len()).map_err(|_| overflow())?;
        let colour_bytes = n.checked_mul(16).ok_or_else(overflow)?;
        let matte_bytes = n.checked_mul(4).ok_or_else(overflow)?;
        if colour_bytes > ctx.max_buffer_bytes {
            return Err(GpuError::Backend(BackendError::BudgetExceeded { requested: colour_bytes, limit: ctx.max_buffer_bytes }));
        }
        let usage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST;
        let buffer = |label: &'static str, size: u64, usage: wgpu::BufferUsages| {
            device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: size.max(16), usage, mapped_at_creation: false })
        };
        let scopes = ErrorScopes::push(device);
        let colour = buffer("key colour", colour_bytes, usage);
        let mattes = [buffer("key matte a", matte_bytes, usage), buffer("key matte b", matte_bytes, usage)];
        let params = buffer("key params", PARAMS_BYTES, wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST);
        let aux = buffer("key aux", (FEATHER_MAX_TAPS as u64).saturating_mul(4), wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST);
        let mut flat = Vec::with_capacity(plane.len().saturating_mul(16));
        for p in plane {
            for c in p {
                flat.extend_from_slice(&c.to_le_bytes());
            }
        }
        ctx.queue.write_buffer(&colour, 0, &flat);
        let ones: Vec<u8> = std::iter::repeat_n(1.0f32.to_le_bytes(), plane.len()).flatten().collect();
        ctx.queue.write_buffer(&mattes[0], 0, &ones);
        let binds: Vec<wgpu::BindGroup> = (0..2)
            .map(|k: usize| {
                let (src, dst) = if k == 0 { (&mattes[0], &mattes[1]) } else { (&mattes[1], &mattes[0]) };
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(LABEL),
                    layout: &self.layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: colour.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: src.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 3, resource: dst.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 4, resource: aux.as_entire_binding() },
                    ],
                })
            })
            .collect();
        let groups = (rect.width.div_ceil(8), rect.height.div_ceil(8));
        // `current` = the bind group whose matte_in holds the current matte.
        let mut current = 0usize;
        let mut dispatch = |pipe: &wgpu::ComputePipeline, p: [u8; PARAMS_BYTES as usize], aux_data: &[f32]| {
            ctx.queue.write_buffer(&params, 0, &p);
            if !aux_data.is_empty() {
                let bytes: Vec<u8> = aux_data.iter().flat_map(|v| v.to_le_bytes()).collect();
                ctx.queue.write_buffer(&aux, 0, &bytes);
            }
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(LABEL) });
            {
                let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some(LABEL), timestamp_writes: None });
                pass.set_pipeline(pipe);
                pass.set_bind_group(0, binds.get(current), &[]);
                pass.dispatch_workgroups(groups.0, groups.1, 1);
            }
            ctx.queue.submit([enc.finish()]);
            current = 1usize.saturating_sub(current);
        };
        for s in stages {
            let (op, combine) = point_op(s);
            match s {
                KeyStage::ChokeSpread { radius_px } => {
                    dispatch(&self.morph, pack(rect, 0, 0, [radius_px.unsigned_abs() as i32, i32::from(*radius_px > 0), 0, 0], [[0.0; 4]; 4]), &[]);
                }
                KeyStage::Feather { radius_px } => {
                    let taps = feather_kernel(*radius_px).map_err(|_| unsupported(KEY_GPU_RADIUS))?;
                    let support = i32::try_from(taps.len().checked_div(2).unwrap_or(0)).map_err(|_| overflow())?;
                    dispatch(&self.blur, pack(rect, 0, 0, [support, 0, 0, 0], [[0.0; 4]; 4]), &taps);
                    dispatch(&self.blur, pack(rect, 0, 0, [support, 1, 0, 0], [[0.0; 4]; 4]), &taps);
                }
                KeyStage::EdgeSmooth { radius_px, colour_sigma } => {
                    let inv = (1.0 / (f64::from(*colour_sigma) * f64::from(*colour_sigma))) as f32;
                    dispatch(&self.smooth, pack(rect, 0, 0, [*radius_px as i32, 0, 0, 0], [[0.0; 4], [inv, 0.0, 0.0, 0.0], [0.0; 4], [0.0; 4]]), &[]);
                }
                _ => {
                    let (ints, f, aux_data) = point_params(s)?;
                    dispatch(&self.point, pack(rect, op, combine, ints, f), &aux_data);
                }
            }
        }
        // Read back the colour and the current matte.
        let matte_now = if current == 0 { &mattes[0] } else { &mattes[1] };
        let staging = buffer("key staging", colour_bytes.checked_add(matte_bytes).ok_or_else(overflow)?, wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(LABEL) });
        enc.copy_buffer_to_buffer(&colour, 0, &staging, 0, colour_bytes);
        enc.copy_buffer_to_buffer(matte_now, 0, &staging, colour_bytes, matte_bytes);
        ctx.queue.submit([enc.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        staging.slice(..).map_async(wgpu::MapMode::Read, move |o| {
            let _ = tx.send(o);
        });
        let wait = request.deadline.map_or(DEFAULT_POLL_LIMIT, |d| {
            d.saturating_duration_since(std::time::Instant::now()).max(std::time::Duration::from_millis(1))
        });
        let polled = device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(wait) });
        ctx.check_alive()?;
        if polled.is_err() || !matches!(rx.try_recv(), Ok(Ok(()))) {
            let captured = scopes.finish();
            return Err(GpuError::Gpu(if captured.is_some() { "key_submit_fault" } else { "key_readback_failed" }));
        }
        let (colour_out, matte_out) = {
            let data = staging.slice(..).get_mapped_range();
            let f = |at: usize| f32::from_le_bytes([data[at], data[at.saturating_add(1)], data[at.saturating_add(2)], data[at.saturating_add(3)]]);
            let cb = usize::try_from(colour_bytes).map_err(|_| overflow())?;
            let colour_out: Vec<[f32; 4]> = (0..plane.len())
                .map(|i| {
                    let b = i.saturating_mul(16);
                    [f(b), f(b.saturating_add(4)), f(b.saturating_add(8)), f(b.saturating_add(12))]
                })
                .collect();
            let matte_out: Vec<f32> = (0..plane.len()).map(|i| f(cb.saturating_add(i.saturating_mul(4)))).collect();
            (colour_out, matte_out)
        };
        staging.unmap();
        if scopes.finish().is_some() {
            return Err(GpuError::Gpu("key_submit_fault"));
        }
        Ok((colour_out, matte_out))
    }
}

/// Op code and combine code of a point stage (see `key.wgsl`).
fn point_op(s: &KeyStage) -> (u32, u32) {
    let combine = |c: &MatteCombine| match c {
        MatteCombine::Replace => 0,
        MatteCombine::Multiply => 1,
        MatteCombine::Min => 2,
        MatteCombine::Max => 3,
    };
    match s {
        KeyStage::Chroma(_, c) => (0, combine(c)),
        KeyStage::Luma(_, c) => (1, combine(c)),
        KeyStage::ColourRange(_, c) => (2, combine(c)),
        KeyStage::ColourKey(_, c) => (3, combine(c)),
        KeyStage::ColourDifference(_, c) => (4, combine(c)),
        KeyStage::LinearColour(_, c) => (5, combine(c)),
        KeyStage::Extract(_, c) => (6, combine(c)),
        KeyStage::Levels { .. } => (7, 0),
        KeyStage::Despill(_) => (8, 0),
        _ => (9, 0),
    }
}

type PointParams = ([i32; 4], [[f32; 4]; 4], Vec<f32>);

fn rgb3(k: [f32; 3]) -> [f64; 3] {
    [f64::from(k[0]), f64::from(k[1]), f64::from(k[2])]
}
fn ycbcr(c: [f64; 3]) -> [f64; 3] {
    let y = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    [y, (c[2] - y) / 1.8556, (c[0] - y) / 1.5748]
}
fn lab(c: [f64; 3]) -> [f64; 3] {
    let x = (0.4124 * c[0] + 0.3576 * c[1] + 0.1805 * c[2]) / 0.95047;
    let y = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    let z = (0.0193 * c[0] + 0.1192 * c[1] + 0.9505 * c[2]) / 1.08883;
    let e = 216.0 / 24389.0;
    let k = 24389.0 / 27.0;
    let f = |t: f64| if t > e { hsk_studio_pigment::keying::det_cbrt(t) } else { (k * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [(116.0 * fy - 16.0) / 100.0, 5.0 * (fx - fy), 2.0 * (fy - fz)]
}
fn space_of(space: RangeSpace, c: [f64; 3]) -> [f64; 3] {
    match space {
        RangeSpace::Rgb => c,
        RangeSpace::YCbCr => ycbcr(c),
        RangeSpace::Lab => lab(c),
    }
}
fn v4(a: f64, b: f64, c: f64, d: f64) -> [f32; 4] {
    [a as f32, b as f32, c as f32, d as f32]
}
fn hue(c: [f64; 3]) -> f64 {
    let mx = c[0].max(c[1]).max(c[2]);
    let mn = c[0].min(c[1]).min(c[2]);
    let d = mx - mn;
    if d <= 0.0 {
        return -1.0;
    }
    let h = if mx == c[0] {
        ((c[1] - c[2]) / d).rem_euclid(6.0)
    } else if mx == c[1] {
        (c[2] - c[0]) / d + 2.0
    } else {
        (c[0] - c[1]) / d + 4.0
    };
    h * 60.0
}

/// Integer and float parameters (and aux data) of a point stage; key colours are converted on
/// the host in binary64.
fn point_params(s: &KeyStage) -> Result<PointParams, GpuError> {
    let z = [0.0f32; 4];
    Ok(match s {
        KeyStage::Chroma(k, _) => {
            let key = rgb3(k.key);
            let (space, kv) = match k.space {
                ChromaSpace::YCbCr => (0, ycbcr(key)),
                ChromaSpace::Lab => (1, lab(key)),
            };
            ([0, space, 0, 0], [v4(kv[0], kv[1], kv[2], f64::from(k.luma_weight)), [k.tolerance, k.softness, 0.0, 0.0], z, z], vec![])
        }
        KeyStage::Luma(k, _) => {
            let kind = match k.kind {
                LumaKeyType::KeyOutDarker => 0,
                LumaKeyType::KeyOutBrighter => 1,
                LumaKeyType::KeyOutSimilar => 2,
                LumaKeyType::KeyOutDissimilar => 3,
            };
            ([0, kind, 0, 0], [z, [k.threshold, k.tolerance, k.softness, 0.0], z, z], vec![])
        }
        KeyStage::ColourRange(k, _) => {
            let space = match k.space {
                RangeSpace::Rgb => 0,
                RangeSpace::YCbCr => 1,
                RangeSpace::Lab => 2,
            };
            let bx = |samples: &[[f32; 3]]| -> Option<([f64; 3], [f64; 3])> {
                let mut it = samples.iter().map(|s| space_of(k.space, rgb3(*s)));
                let first = it.next()?;
                Some(it.fold((first, first), |(lo, hi), v| {
                    ([lo[0].min(v[0]), lo[1].min(v[1]), lo[2].min(v[2])], [hi[0].max(v[0]), hi[1].max(v[1]), hi[2].max(v[2])])
                }))
            };
            let add = bx(&k.add).ok_or_else(|| unsupported("key_parameter"))?;
            let sub = bx(&k.subtract);
            let mut aux: Vec<f32> = add.0.iter().chain(add.1.iter()).map(|v| *v as f32).collect();
            if let Some(s) = sub {
                aux.extend(s.0.iter().chain(s.1.iter()).map(|v| *v as f32));
            } else {
                aux.extend([0.0f32; 6]);
            }
            ([0, space, i32::from(sub.is_some()), 0], [z, [k.fuzziness, 0.0, 0.0, 0.0], z, z], aux)
        }
        KeyStage::ColourKey(k, _) => ([0; 4], [[k.key[0], k.key[1], k.key[2], 0.0], [k.tolerance, 0.0, 0.0, 0.0], z, z], vec![]),
        KeyStage::ColourDifference(k, _) => {
            let screen = if k.screen == SpillScreen::Blue { 2 } else { 1 };
            ([0, screen, 0, 0], [z, [k.a_black, k.a_white, k.b_black, k.b_white], z, z], vec![])
        }
        KeyStage::LinearColour(k, _) => {
            let key = rgb3(k.key);
            let matching = match k.matching {
                LinearMatch::Rgb => 0,
                LinearMatch::Hue => 1,
                LinearMatch::Chroma => 2,
            };
            let c = ycbcr(key);
            (
                [0, matching, i32::from(k.keep), 0],
                [v4(key[0], key[1], key[2], hue(key)), [k.tolerance, k.softness, 0.0, 0.0], v4(c[1], c[2], 0.0, 0.0), z],
                vec![],
            )
        }
        KeyStage::Extract(k, _) => {
            let channel = match k.channel {
                ExtractChannel::Luminance => 0,
                ExtractChannel::Red => 1,
                ExtractChannel::Green => 2,
                ExtractChannel::Blue => 3,
                ExtractChannel::Alpha => 4,
            };
            ([0, channel, i32::from(k.invert), 0], [z, [k.black, k.white, k.black_softness, k.white_softness], z, z], vec![])
        }
        KeyStage::Levels { black, white } => ([0; 4], [z, [*black, *white, 0.0, 0.0], z, z], vec![]),
        KeyStage::Despill(d) => {
            let screen = if d.screen == SpillScreen::Blue { 2 } else { 1 };
            let limit = match d.limit {
                SpillLimit::Average => 0,
                SpillLimit::Max => 1,
                SpillLimit::Min => 2,
            };
            ([0, screen, limit, i32::from(d.preserve_luminance)], [z, [d.amount, 0.0, 0.0, 0.0], z, z], vec![])
        }
        KeyStage::Unmult => ([0; 4], [z; 4], vec![]),
        other => return Err(unsupported(other.key())),
    })
}

/// The 96-byte parameter block of `key.wgsl` (`Params`).
fn pack(rect: Rect, op: u32, combine: u32, ints: [i32; 4], f: [[f32; 4]; 4]) -> [u8; PARAMS_BYTES as usize] {
    let mut out = [0u8; PARAMS_BYTES as usize];
    let mut words: Vec<[u8; 4]> = vec![rect.width.to_le_bytes(), rect.height.to_le_bytes(), op.to_le_bytes(), combine.to_le_bytes()];
    words.extend(ints.iter().map(|v| v.to_le_bytes()));
    words.extend(f.iter().flatten().map(|v| v.to_le_bytes()));
    for (chunk, w) in out.chunks_exact_mut(4).zip(words) {
        chunk.copy_from_slice(&w);
    }
    out
}
