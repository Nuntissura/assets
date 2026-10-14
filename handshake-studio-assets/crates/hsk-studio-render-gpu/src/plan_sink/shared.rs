//! Shared-device execution (feature `shared-device`, lane SMB-RENDER-GPU-SHARED-DEVICE): render a
//! pigment plan on a CALLER-OWNED wgpu device and queue (for example egui-wgpu's, wgpu 27.0.1 is
//! the one wgpu in the workspace) straight into a caller-owned texture, with no read-back to the
//! host and no upload from it.
//!
//! Same admission, preparation, budgets, chunking and compute passes as [`GpuBackend`] (the
//! backend inside is an ordinary `GpuBackend` over the caller's device): only the chunk output
//! differs. Instead of the staging copy + map + `TileSink`, one compute pass per chunk converts
//! the finished straight-alpha linear `RGBA f32` chunk into display bytes and stores them into the
//! target texture at the chunk's position relative to the plan extent origin.
//!
//! Display encoding (the egui convention: textures hold gamma-space, premultiplied 8-bit values):
//! `a8 = round(a * 255)`, `c8 = round(srgb_encode(c) * 255)` (IEC 61966-2-1, clamped to `0..=1`,
//! NaN as 0), stored `round(c8 * a8 / 255) / 255` and `a8 / 255` into an [`TARGET_FORMAT`]
//! (`Rgba8Unorm`) storage texture. Rounding is `floor(x + 0.5)`, Rust `f32::round` for the
//! non-negative values here; `pow` may differ from the CPU in the last ulp, so parity with the
//! CPU encoding is +-1 per channel (test tests/shared_device.rs
//! shared_device_renders_into_the_callers_texture_without_readback).
//!
//! Contract: the texture must be created on the same device, `Rgba8Unorm`, 2D, one sample,
//! usage `STORAGE_BINDING`, and at least the plan extent in both dimensions; anything else is
//! refused as `Backend(Sink(Rejected))` before any device work. The commands are submitted on the
//! caller's queue, so a later submission on that queue (egui's frame) sees the finished texture.
//! Device loss is the host's to detect: no device-lost or uncaptured-error callback is installed
//! on a shared device (see `GpuContext::from_external`).
#![deny(clippy::arithmetic_side_effects)]
use super::{GpuBackend, GpuStats, State, overflow};
use crate::device::{DeviceOptions, GpuContext};
use crate::error::GpuError;
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{
    BackendError, ExecuteRequest, NoVector, PlanExtensions, PlanSources, RenderPlan,
    RenderReceipt, SinkError, TileSink,
};

/// The only texture format the shared output writes (storage-writable in core WebGPU).
pub const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Machine-readable facts of this module (the crate DESCRIPTOR is unchanged).
pub const DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-RENDER_GPU.shared","lane":"SMB-RENDER-GPU-SHARED-DEVICE","feature":"shared-device (default off: the default build keeps no wgpu type in a public signature, STU-ARC-002)","api":"SharedDevice::new(&wgpu::Device, &wgpu::Queue, wgpu::AdapterInfo, DeviceOptions) -> Result<SharedDevice, GpuError>; render_to_texture(plan, Option<&PlanExtensions>, request, sources, cancel, &wgpu::Texture) -> Result<(RenderReceipt, GpuStats), GpuError>; backend() -> &GpuBackend (the same device through the TileSink path)","output":"Rgba8Unorm storage texture, gamma-space premultiplied 8-bit (egui texture convention), written at the chunk position relative to the plan extent origin; no read-back (GpuStats.bytes_read_back stays 0)","refused":"texture not Rgba8Unorm, not 2D, multisampled, without STORAGE_BINDING or smaller than the plan extent: Backend(Sink(Rejected)) before any device work","loss":"no callbacks installed on the caller's device; the host detects loss","proven_by":"tests/shared_device.rs shared_device_renders_into_the_callers_texture_without_readback (live adapter, tests/gpu-proof.toml policy), shared_target_refusals_happen_before_device_work"}"#;

const PARAMS_BYTES: u64 = 32;
const WORKGROUP: u32 = 8;

const SHADER: &str = r"
struct Params {
    dst: vec2<u32>,
    size: vec2<u32>,
    frame_width: u32,
    margin: u32,
    pad0: u32,
    pad1: u32,
};
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> level: array<vec4<f32>>;
@group(0) @binding(2) var target_tex: texture_storage_2d<rgba8unorm, write>;

fn unit(v: f32) -> f32 {
    // NaN compares unequal to itself: map it to 0 like the CPU encoder.
    return select(0.0, clamp(v, 0.0, 1.0), v == v);
}
fn byte(v: f32) -> f32 {
    return floor(unit(v) * 255.0 + 0.5);
}
fn encode(v: f32) -> f32 {
    let c = unit(v);
    if c <= 0.0031308 {
        return c * 12.92;
    }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.size.x || id.y >= params.size.y {
        return;
    }
    let p = level[(id.y + params.margin) * params.frame_width + id.x + params.margin];
    let a8 = byte(p.a);
    let c8 = vec3<f32>(byte(encode(p.r)), byte(encode(p.g)), byte(encode(p.b)));
    let pre = floor(c8 * a8 / 255.0 + vec3<f32>(0.5));
    textureStore(target_tex, vec2<u32>(params.dst.x + id.x, params.dst.y + id.y), vec4<f32>(pre / 255.0, a8 / 255.0));
}
";

/// The display-encoding pass, built once per shared device.
struct OutputPipeline {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
}

impl OutputPipeline {
    fn new(device: &wgpu::Device) -> Result<Self, GpuError> {
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hsk-studio-render-gpu shared output"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(SHADER)),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("hsk-studio-render-gpu shared output"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: TARGET_FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("hsk-studio-render-gpu shared output"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("hsk-studio-render-gpu shared output"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hsk-studio-render-gpu shared output params"),
            size: PARAMS_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        match crate::device::block_on_bounded(device.pop_error_scope(), None) {
            Some(None) => Ok(Self {
                pipeline,
                layout,
                params,
            }),
            Some(Some(_)) => Err(GpuError::Pipeline("shader_validation")),
            None => Err(GpuError::Pipeline("shader_validation_timeout")),
        }
    }
}

/// One execution's texture output (built by [`SharedDevice::render_to_texture`], used by `run`).
pub(in crate::plan_sink) struct TextureOut<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    pipeline: &'a OutputPipeline,
    view: wgpu::TextureView,
    origin_x: i64,
    origin_y: i64,
}

impl TextureOut<'_> {
    /// Encode the central `rect` of the finished frame in `level` (frame width `frame_width`,
    /// effect margin `margin`) into the target texture and submit on the caller's queue.
    pub(in crate::plan_sink) fn write_chunk(
        &self,
        level: &wgpu::Buffer,
        rect: Rect,
        frame_width: u32,
        margin: u32,
    ) -> Result<(), GpuError> {
        let dst_x = u32::try_from(rect.x.checked_sub(self.origin_x).ok_or_else(overflow)?)
            .map_err(|_| overflow())?;
        let dst_y = u32::try_from(rect.y.checked_sub(self.origin_y).ok_or_else(overflow)?)
            .map_err(|_| overflow())?;
        let mut params = Vec::with_capacity(32);
        for word in [dst_x, dst_y, rect.width, rect.height, frame_width, margin, 0, 0] {
            params.extend_from_slice(&word.to_le_bytes());
        }
        self.queue.write_buffer(&self.pipeline.params, 0, &params);
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hsk-studio-render-gpu shared output"),
            layout: &self.pipeline.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.pipeline.params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: level.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&self.view),
                },
            ],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hsk-studio-render-gpu shared output"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("hsk-studio-render-gpu shared output"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(
                rect.width.div_ceil(WORKGROUP),
                rect.height.div_ceil(WORKGROUP),
                1,
            );
        }
        self.queue.submit(Some(encoder.finish()));
        Ok(())
    }
}

/// The sink of a texture execution: never called (chunks go to the texture).
struct NoSink;
impl TileSink for NoSink {
    fn accept(&mut self, _: Rect, _: &[u8], _: &[u8]) -> Result<(), SinkError> {
        Err(SinkError::Unavailable)
    }
}

/// A render-gpu backend on a caller-owned device, with the texture output pass.
pub struct SharedDevice {
    backend: GpuBackend,
    output: OutputPipeline,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl SharedDevice {
    /// Wrap the caller's device and queue (clones of wgpu's reference-counted handles). Builds the
    /// render-gpu compute pipelines and the output pass on that device; a failure is the typed
    /// `GpuError::Pipeline`.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        adapter: wgpu::AdapterInfo,
        options: DeviceOptions,
    ) -> Result<Self, GpuError> {
        let ctx = GpuContext::from_external(device.clone(), queue.clone(), adapter, &options)?;
        let generation = ctx.generation();
        let output = OutputPipeline::new(device)?;
        Ok(Self {
            backend: GpuBackend::with_state(options, State::Ready(Box::new(ctx)), generation),
            output,
            device: device.clone(),
            queue: queue.clone(),
        })
    }

    /// The backend over the shared device (read-back path through a `TileSink`, state queries).
    pub fn backend(&self) -> &GpuBackend {
        &self.backend
    }

    /// Render `plan` (with its envelope when `extensions` is `Some`) into `target`. Same order and
    /// typed failures as [`GpuBackend::execute_detailed`]; the target is checked first.
    pub fn render_to_texture(
        &self,
        plan: &RenderPlan<NoVector>,
        extensions: Option<&PlanExtensions>,
        request: &ExecuteRequest,
        sources: &dyn PlanSources,
        cancel: &CancellationToken,
        target: &wgpu::Texture,
    ) -> Result<(RenderReceipt, GpuStats), GpuError> {
        let extent = plan.target.extent;
        let rejected = GpuError::Backend(BackendError::Sink(SinkError::Rejected));
        if target.format() != TARGET_FORMAT
            || target.dimension() != wgpu::TextureDimension::D2
            || target.sample_count() != 1
            || !target.usage().contains(wgpu::TextureUsages::STORAGE_BINDING)
            || target.width() < extent.width
            || target.height() < extent.height
        {
            return Err(rejected);
        }
        let out = TextureOut {
            device: &self.device,
            queue: &self.queue,
            pipeline: &self.output,
            view: target.create_view(&wgpu::TextureViewDescriptor::default()),
            origin_x: extent.x,
            origin_y: extent.y,
        };
        self.backend.execute_inner(
            plan,
            extensions,
            request,
            sources,
            cancel,
            &mut NoSink,
            Some(&out),
        )
    }
}
