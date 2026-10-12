//! Actual DX12 egui-wgpu readback of the same DesktopAdapter used interactively.
use crate::*;
use std::{
    path::Path,
    time::{Duration, Instant},
};

pub struct CapturedFrame {
    pub image: image::RgbaImage,
    pub report: FrameReport,
    pub nonce: u64,
    pub backend: String,
    pub render_ms: f64,
    pub scale: f32,
    pub viewport: egui::ViewportId,
}
impl CapturedFrame {
    pub fn png(&self, max_bytes: usize) -> Result<Vec<u8>> {
        if (self.image.width() as usize)
            .checked_mul(self.image.height() as usize)
            .and_then(|n| n.checked_mul(4))
            .is_none_or(|n| n > max_bytes)
        {
            return Err(AdapterError::Budget("capture raw bytes"));
        }
        struct Bounded {
            bytes: Vec<u8>,
            limit: usize,
        }
        impl std::io::Write for Bounded {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self
                    .bytes
                    .len()
                    .checked_add(bytes.len())
                    .is_none_or(|n| n > self.limit)
                {
                    return Err(std::io::Error::other("PNG byte limit"));
                }
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        use image::ImageEncoder;
        let mut output = Bounded {
            bytes: vec![],
            limit: max_bytes,
        };
        image::codecs::png::PngEncoder::new(&mut output)
            .write_image(
                self.image.as_raw(),
                self.image.width(),
                self.image.height(),
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| AdapterError::Host(format!("PNG encoding: {e}")))?;
        Ok(output.bytes)
    }
    pub fn metadata(&self) -> serde_json::Value {
        serde_json::json!({"kind":"kumihimo-offscreen-capture", "nonce":self.nonce, "viewport":format!("{:?}",self.viewport), "backend":self.backend, "width":self.image.width(), "height":self.image.height(), "pixels_per_point":self.scale, "render_ms":self.render_ms, "provenance":"actual egui-wgpu DX12 offscreen readback; no native presentation or physical-input claim", "frame":crate::diagnostics::report_json(&self.report)})
    }
    pub fn write(&self, path: &Path, max_bytes: usize) -> Result<()> {
        let png = self.png(max_bytes)?;
        let metadata = crate::diagnostics::encode_bounded(&self.metadata(), max_bytes)?;
        std::fs::write(path.with_extension("png"), png)
            .map_err(|e| AdapterError::Host(e.to_string()))?;
        std::fs::write(path.with_extension("json"), metadata)
            .map_err(|e| AdapterError::Host(e.to_string()))?;
        Ok(())
    }
}
pub struct OffscreenRenderer {
    pub context: egui::Context,
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: egui_wgpu::Renderer,
    backend: String,
    next_nonce: u64,
    max_bytes: usize,
}
impl OffscreenRenderer {
    pub fn gpu_binding(&self, surface: SurfaceBinding) -> Result<crate::content::GpuBinding> {
        crate::content::GpuBinding::new(&self.device, surface)
    }
    pub fn gpu_resources(&mut self) -> (&wgpu::Device, &wgpu::Queue, &mut egui_wgpu::Renderer) {
        (&self.device, &self.queue, &mut self.renderer)
    }
    pub fn new(max_bytes: usize) -> Result<Self> {
        if max_bytes == 0 {
            return Err(AdapterError::Budget("zero capture budget"));
        }
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .map_err(|e| AdapterError::Host(format!("DX12 adapter: {e}")))?;
        let info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("kumihimo-offscreen"),
            ..Default::default()
        }))
        .map_err(|e| AdapterError::Host(format!("DX12 device: {e}")))?;
        let renderer = egui_wgpu::Renderer::new(
            &device,
            wgpu::TextureFormat::Rgba8Unorm,
            egui_wgpu::RendererOptions::PREDICTABLE,
        );
        Ok(Self {
            context: egui::Context::default(),
            device,
            queue,
            renderer,
            backend: format!("{:?}: {} ({:?})", info.backend, info.name, info.device_type),
            next_nonce: 1,
            max_bytes,
        })
    }
    pub fn capture(
        &mut self,
        adapter: &mut DesktopAdapter,
        controller: &mut Controller,
        surface: SurfaceBinding,
        host: &mut impl DesktopHost,
        mut input: egui::RawInput,
    ) -> Result<CapturedFrame> {
        surface.validate()?;
        if surface.bounds.x() != 0.0 || surface.bounds.y() != 0.0 {
            return Err(AdapterError::Invalid("capture surface origin must be zero"));
        }
        let [width, height] = surface.physical_size()?;
        if width == 0 || height == 0 {
            return Err(AdapterError::Invalid("zero capture dimensions"));
        }
        let limit = self.max_bytes.min(host.policy().max_capture_bytes);
        let raw = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or(AdapterError::Budget("capture multiplication"))?;
        let row = width
            .checked_mul(4)
            .ok_or(AdapterError::Budget("capture row"))?;
        let padded = row
            .checked_add(255)
            .ok_or(AdapterError::Budget("capture row padding"))?
            / 256
            * 256;
        let buffer_size = (padded as u64)
            .checked_mul(height as u64)
            .ok_or(AdapterError::Budget("capture buffer"))?;
        if raw > limit || buffer_size > limit as u64 {
            return Err(AdapterError::Budget("capture bytes"));
        }
        if width > self.device.limits().max_texture_dimension_2d
            || height > self.device.limits().max_texture_dimension_2d
        {
            return Err(AdapterError::Budget("device texture extent"));
        }
        let started = Instant::now();
        input.screen_rect = Some(to_egui(surface.bounds));
        input.viewport_id = surface.viewport;
        input
            .viewports
            .entry(surface.viewport)
            .or_default()
            .native_pixels_per_point = Some(surface.pixels_per_point);
        // Host DIP already defines logical coordinates; native scale is applied once.
        self.context.set_zoom_factor(1.0);
        let mut report = None;
        let output = self.context.run(input, |ctx| {
            report = Some(adapter.show(ctx, controller, surface, host));
        });
        let report = report.ok_or(AdapterError::Invalid("egui draw missing"))??;
        if output.pixels_per_point != surface.pixels_per_point {
            return Err(AdapterError::Invalid(
                "offscreen egui scale differs from host surface",
            ));
        }
        let jobs = self
            .context
            .tessellate(output.shapes, output.pixels_per_point);
        for (id, delta) in &output.textures_delta.set {
            self.renderer
                .update_texture(&self.device, &self.queue, *id, delta);
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kumihimo-raster"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kumihimo-readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kumihimo-capture"),
            });
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [width, height],
            pixels_per_point: output.pixels_per_point,
        };
        let mut commands =
            self.renderer
                .update_buffers(&self.device, &self.queue, &mut encoder, &jobs, &screen);
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kumihimo-egui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.06,
                            b: 0.09,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.renderer
                .render(&mut pass.forget_lifetime(), &jobs, &screen);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        commands.push(encoder.finish());
        let submission = self.queue.submit(commands);
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(Duration::from_secs(10)),
            })
            .map_err(|e| AdapterError::Host(format!("readback poll: {e}")))?;
        rx.recv_timeout(Duration::from_secs(1))
            .map_err(|e| AdapterError::Host(format!("readback callback: {e}")))?
            .map_err(|e| AdapterError::Host(format!("readback map: {e}")))?;
        let mapped = buffer.slice(..).get_mapped_range();
        let mut pixels = Vec::with_capacity(raw);
        for chunk in mapped.chunks_exact(padded as usize).take(height as usize) {
            pixels.extend_from_slice(&chunk[..row as usize]);
        }
        drop(mapped);
        buffer.unmap();
        for id in &output.textures_delta.free {
            self.renderer.free_texture(id);
        }
        let image = image::RgbaImage::from_raw(width, height, pixels)
            .ok_or(AdapterError::Invalid("readback image dimensions"))?;
        let nonce = self.next_nonce;
        self.next_nonce = nonce
            .checked_add(1)
            .ok_or(AdapterError::Budget("capture nonce"))?;
        Ok(CapturedFrame {
            image,
            report,
            nonce,
            backend: self.backend.clone(),
            render_ms: started.elapsed().as_secs_f64() * 1000.0,
            scale: output.pixels_per_point,
            viewport: surface.viewport,
        })
    }
}
