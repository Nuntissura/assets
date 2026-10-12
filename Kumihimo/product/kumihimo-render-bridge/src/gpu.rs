//! Optional, owning-device GPU resources. Engine handles never enter this module.
use crate::frame::{BridgeLimits, FrameRequest, PixelFrame};
use kumihimo_egui::{content::GpuBinding, AdapterError, Result};
use sha2::{Digest, Sha256};
use std::{
    io::Cursor,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Header bounds are checked before the decoder is allowed to allocate.
pub fn decode_engine_png(
    bytes: &[u8],
    request: FrameRequest,
    expected_hash: &str,
    limits: &BridgeLimits,
) -> Result<PixelFrame> {
    request
        .validate(limits)
        .map_err(|e| AdapterError::Host(e.to_string()))?;
    if bytes.len() > limits.max_png_bytes
        || bytes.len() < 33
        || &bytes[..8] != b"\x89PNG\r\n\x1a\n"
        || bytes[8..12] != 13u32.to_be_bytes()
        || &bytes[12..16] != b"IHDR"
    {
        return Err(AdapterError::Invalid("bounded PNG signature/IHDR"));
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    let raw = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4));
    if width != request.width
        || height != request.height
        || width > limits.max_side
        || height > limits.max_side
        || raw.is_none_or(|n| n > limits.max_rgba_bytes)
        || expected_hash.len() != 64
        || sha256(bytes) != expected_hash
    {
        return Err(AdapterError::Invalid(
            "PNG extent/budget/engine receipt hash",
        ));
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
    let mut decoder_limits = image::Limits::default();
    decoder_limits.max_image_width = Some(limits.max_side);
    decoder_limits.max_image_height = Some(limits.max_side);
    decoder_limits.max_alloc = Some(limits.max_rgba_bytes as u64);
    reader.limits(decoder_limits);
    let image = reader
        .decode()
        .map_err(|e| AdapterError::Host(format!("engine PNG: {e}")))?
        .into_rgba8();
    if image.width() != width || image.height() != height {
        return Err(AdapterError::Invalid("PNG decoded extent"));
    }
    let frame = PixelFrame {
        request,
        row_stride: width
            .checked_mul(4)
            .ok_or(AdapterError::Budget("PNG stride"))?,
        rgba: image.into_raw(),
    };
    frame
        .validate(limits)
        .map_err(|e| AdapterError::Host(e.to_string()))?;
    Ok(frame)
}

pub struct TextureUpload {
    pub binding: GpuBinding,
    pub texture_id: egui::TextureId,
    pub rgba_sha256: String,
    pub sequence: u64,
    _texture: wgpu::Texture,
}
impl TextureUpload {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut egui_wgpu::Renderer,
        binding: GpuBinding,
        frame: &PixelFrame,
        limits: &BridgeLimits,
    ) -> Result<Self> {
        frame
            .validate(limits)
            .map_err(|e| AdapterError::Host(e.to_string()))?;
        if frame.request.width > device.limits().max_texture_dimension_2d
            || frame.request.height > device.limits().max_texture_dimension_2d
        {
            return Err(AdapterError::Budget("engine texture device limits"));
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("actual-servo-pixels"),
            size: wgpu::Extent3d {
                width: frame.request.width,
                height: frame.request.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &frame.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(frame.row_stride),
                rows_per_image: Some(frame.request.height),
            },
            texture.size(),
        );
        let texture_id = renderer.register_native_texture(
            device,
            &texture.create_view(&Default::default()),
            wgpu::FilterMode::Nearest,
        );
        Ok(Self {
            binding,
            texture_id,
            rgba_sha256: sha256(&frame.rgba),
            sequence: frame.request.sequence,
            _texture: texture,
        })
    }
}

struct SceneResources {
    color: wgpu::TextureView,
    depth: wgpu::TextureView,
    camera: wgpu::Buffer,
    scene_pipeline: wgpu::RenderPipeline,
    scene_bind: wgpu::BindGroup,
    quad_pipeline: wgpu::RenderPipeline,
    quad_bind: wgpu::BindGroup,
    _color_texture: wgpu::Texture,
    _depth_texture: wgpu::Texture,
    dirty: AtomicBool,
    renders: AtomicU64,
}
pub struct Scene3d {
    resources: Arc<SceneResources>,
    pub binding: GpuBinding,
    pub size: [u32; 2],
    pub selected: bool,
}
#[derive(Clone)]
pub struct SceneCallback {
    resources: Arc<SceneResources>,
}
impl Scene3d {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        binding: GpuBinding,
        size: [u32; 2],
        selected: bool,
    ) -> Result<Self> {
        if size.contains(&0)
            || size
                .iter()
                .any(|v| *v > 2048 || *v > device.limits().max_texture_dimension_2d)
        {
            return Err(AdapterError::Budget("3D texture size"));
        }
        let make_texture = |label, format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let color_texture = make_texture(
            "bridge-3d-color",
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let depth_texture = make_texture(
            "bridge-3d-depth",
            wgpu::TextureFormat::Depth32Float,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let color = color_texture.create_view(&Default::default());
        let depth = depth_texture.create_view(&Default::default());
        let camera = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("perspective-camera"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(
            &camera,
            0,
            bytemuck::cast_slice(&[
                size[0] as f32 / size[1] as f32,
                if selected { 1.0 } else { 0.0 },
                0.0,
                0.0,
            ]),
        );
        let scene_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene-camera"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let scene_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene-camera"),
            layout: &scene_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera.as_entire_binding(),
            }],
        });
        let quad_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene-sample"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&Default::default());
        let quad_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene-sample"),
            layout: &quad_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&color),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let source = include_str!("scene.wgsl");
        // Separate modules avoid duplicate binding locations used by the two pipelines.
        let split = source
            .find("@group(0) @binding(0) var scene_texture")
            .ok_or(AdapterError::Invalid("scene shader split"))?;
        let scene_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("perspective-scene"),
            source: wgpu::ShaderSource::Wgsl(source[..split].into()),
        });
        let quad_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scene-quad"),
            source: wgpu::ShaderSource::Wgsl(source[split..].into()),
        });
        let pipeline =
            |label, shader: &wgpu::ShaderModule, layout: &wgpu::BindGroupLayout, vs, fs, depth| {
                let pipeline_layout =
                    device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some(label),
                        bind_group_layouts: &[layout],
                        push_constant_ranges: &[],
                    });
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: shader,
                        entry_point: Some(vs),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    primitive: Default::default(),
                    depth_stencil: depth,
                    multisample: Default::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: shader,
                        entry_point: Some(fs),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: wgpu::TextureFormat::Rgba8Unorm,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    multiview: None,
                    cache: None,
                })
            };
        let scene_pipeline = pipeline(
            "true-perspective-depth",
            &scene_shader,
            &scene_layout,
            "scene_vertex",
            "scene_fragment",
            Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
        );
        let quad_pipeline = pipeline(
            "scene-compose",
            &quad_shader,
            &quad_layout,
            "quad_vertex",
            "quad_fragment",
            None,
        );
        Ok(Self {
            resources: Arc::new(SceneResources {
                color,
                depth,
                camera,
                scene_pipeline,
                scene_bind,
                quad_pipeline,
                quad_bind,
                _color_texture: color_texture,
                _depth_texture: depth_texture,
                dirty: AtomicBool::new(true),
                renders: AtomicU64::new(0),
            }),
            binding,
            size,
            selected,
        })
    }
    pub fn set_selected(&mut self, queue: &wgpu::Queue, selected: bool) {
        if self.selected != selected {
            self.selected = selected;
            queue.write_buffer(
                &self.resources.camera,
                0,
                bytemuck::cast_slice(&[
                    self.size[0] as f32 / self.size[1] as f32,
                    if selected { 1.0 } else { 0.0 },
                    0.0,
                    0.0,
                ]),
            );
            self.resources.dirty.store(true, Ordering::Relaxed);
        }
    }
    pub fn callback(&self) -> SceneCallback {
        SceneCallback {
            resources: self.resources.clone(),
        }
    }
    pub fn render_count(&self) -> u64 {
        self.resources.renders.load(Ordering::Relaxed)
    }
    pub fn projected_point(&self, point: [f32; 3]) -> [f32; 2] {
        let aspect = self.size[0] as f32 / self.size[1] as f32;
        [
            (point[0] * 1.8 / aspect / point[2] + 1.0) * 0.5,
            (1.0 - point[1] * 1.8 / point[2]) * 0.5,
        ]
    }
    pub fn pick(&self, normalized: [f32; 2]) -> bool {
        let a = self.projected_point([-0.65, -0.55, 2.0]);
        let b = self.projected_point([0.65, -0.55, 2.0]);
        let c = self.projected_point([0.0, 0.70, 2.0]);
        let cross = |p: [f32; 2], q: [f32; 2], r: [f32; 2]| {
            (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])
        };
        let signs = [
            cross(a, b, normalized),
            cross(b, c, normalized),
            cross(c, a, normalized),
        ];
        signs.iter().all(|v| *v >= 0.0) || signs.iter().all(|v| *v <= 0.0)
    }
}
impl egui_wgpu::CallbackTrait for SceneCallback {
    fn prepare(
        &self,
        _: &wgpu::Device,
        _: &wgpu::Queue,
        _: &egui_wgpu::ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        _: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if self.resources.dirty.swap(false, Ordering::Relaxed) {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("actual-depth-render"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.resources.color,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.035,
                            g: 0.055,
                            b: 0.11,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.resources.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.resources.scene_pipeline);
            pass.set_bind_group(0, &self.resources.scene_bind, &[]);
            pass.draw(0..6, 0..1);
            self.resources.renders.fetch_add(1, Ordering::Relaxed);
        }
        Vec::new()
    }
    fn paint(
        &self,
        _: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        _: &egui_wgpu::CallbackResources,
    ) {
        pass.set_pipeline(&self.resources.quad_pipeline);
        pass.set_bind_group(0, &self.resources.quad_bind, &[]);
        pass.draw(0..3, 0..1);
    }
}
