//! The WGSL compute pipeline (private: no wgpu type leaves the crate). One pipeline composites
//! one layer (solid fill or an uploaded source region) into a chunk destination buffer.
use crate::error::GpuError;

pub(crate) const WORKGROUP_SIDE: u32 = 8;
/// Size of the uniform block in `kernel.wgsl` (`Params`).
pub(crate) const PARAMS_BYTES: u64 = 64;
/// Bytes per destination/source texel: RGBA f32.
pub(crate) const TEXEL_BYTES: u64 = 16;

/// Layer-mode codes understood by the shader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShaderMode {
    Normal = 0,
    Multiply = 1,
    Screen = 2,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct LayerParams {
    pub chunk_w: u32,
    pub chunk_h: u32,
    pub region: [u32; 4],
    pub mode: ShaderMode,
    /// `Some` = solid fill with this straight RGBA; `None` = uploaded source region.
    pub solid: Option<[f32; 4]>,
    pub opacity: f32,
}
impl LayerParams {
    pub(crate) fn to_bytes(self) -> [u8; PARAMS_BYTES as usize] {
        let mut out = [0u8; PARAMS_BYTES as usize];
        let words: [u32; 8] = [
            self.chunk_w,
            self.chunk_h,
            self.region[0],
            self.region[1],
            self.region[2],
            self.region[3],
            self.mode as u32,
            u32::from(self.solid.is_none()),
        ];
        for (i, w) in words.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&w.to_ne_bytes());
        }
        out[32..36].copy_from_slice(&self.opacity.to_ne_bytes());
        // bytes 36..48 stay zero (padding), the solid colour starts at the 16-byte aligned 48.
        for (i, c) in self.solid.unwrap_or([0.0; 4]).iter().enumerate() {
            out[48 + i * 4..52 + i * 4].copy_from_slice(&c.to_ne_bytes());
        }
        out
    }
}

pub(crate) struct Kernel {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

impl Kernel {
    /// Build the pipeline; shader/pipeline validation failures surface as typed errors, never as
    /// an uncaptured-error panic.
    pub(crate) fn new(device: &wgpu::Device) -> Result<Self, GpuError> {
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hsk-studio-render-gpu composite"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
                "kernel.wgsl"
            ))),
        });
        let entry = |binding: u32, ty: wgpu::BufferBindingType| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("hsk-studio-render-gpu layout"),
            entries: &[
                entry(0, wgpu::BufferBindingType::Uniform),
                entry(1, wgpu::BufferBindingType::Storage { read_only: true }),
                entry(2, wgpu::BufferBindingType::Storage { read_only: false }),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("hsk-studio-render-gpu pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("hsk-studio-render-gpu composite"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("composite"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        match crate::device::block_on_bounded(device.pop_error_scope(), None) {
            Some(None) => Ok(Self { pipeline, layout }),
            Some(Some(_)) => Err(GpuError::Gpu("shader_validation")),
            None => Err(GpuError::Gpu("shader_validation_timeout")),
        }
    }

    pub(crate) fn bind(
        &self,
        device: &wgpu::Device,
        params: &wgpu::Buffer,
        src: &wgpu::Buffer,
        dst: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hsk-studio-render-gpu bind"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: src.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: dst.as_entire_binding(),
                },
            ],
        })
    }

    /// Record one dispatch covering `region` width x height texels.
    pub(crate) fn dispatch(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bind: &wgpu::BindGroup,
        width: u32,
        height: u32,
    ) {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("hsk-studio-render-gpu composite"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.dispatch_workgroups(
            width.div_ceil(WORKGROUP_SIDE),
            height.div_ceil(WORKGROUP_SIDE),
            1,
        );
    }
}
