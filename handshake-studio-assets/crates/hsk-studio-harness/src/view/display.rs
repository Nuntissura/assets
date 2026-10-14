//! Shared-device display (feature `desktop`, lane SMB-RENDER-GPU-SHARED-DEVICE): the viewer's plan
//! renders on eframe's own wgpu device (egui-wgpu `RenderState`, wgpu 27.0.1, the same wgpu as
//! hsk-studio-render-gpu) through `hsk_studio_render_gpu::SharedDevice` straight into a texture
//! registered with egui's renderer. No read-back to the CPU, no second device, no texture upload.
//! The read-back path (render-gpu own device, then render-cpu) stays the fallback in
//! [`super::render::render_to`].
use super::render::GpuDisplay;
use eframe::egui_wgpu::{self, wgpu};
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::render_plan::{ExecuteRequest, NoVector, PlanSources, RenderPlan};
use hsk_studio_render_gpu::{DeviceOptions, SharedDevice, TARGET_FORMAT};

/// The texture the viewer draws, kept across renders and re-created only when the size changes.
struct Target {
    texture: wgpu::Texture,
    id: egui::TextureId,
}

/// [`GpuDisplay`] over an egui-wgpu render state.
pub struct WgpuDisplay {
    state: egui_wgpu::RenderState,
    shared: SharedDevice,
    target: Option<Target>,
}

impl WgpuDisplay {
    /// Builds render-gpu's pipelines on the host's device. A device that cannot hold them is a
    /// typed error text; the viewer then keeps the read-back path.
    pub fn new(state: &egui_wgpu::RenderState) -> Result<Self, String> {
        let shared = SharedDevice::new(&state.device, &state.queue, state.adapter.get_info(), DeviceOptions::default())
            .map_err(|e| format!("render-gpu shared device: {e}"))?;
        Ok(Self { state: state.clone(), shared, target: None })
    }

    /// The display texture of the last frame (`None` before the first frame).
    pub fn texture(&self) -> Option<&wgpu::Texture> {
        self.target.as_ref().map(|t| &t.texture)
    }

    /// Adapter text of the shared device (`backend:kind:name`).
    pub fn adapter(&self) -> String {
        self.shared.backend().device_info().map_or_else(String::new, |i| i.text())
    }
}

impl GpuDisplay for WgpuDisplay {
    fn present(&mut self, plan: &RenderPlan<NoVector>, request: &ExecuteRequest, sources: &dyn PlanSources, cancel: &CancellationToken) -> Result<egui::TextureId, String> {
        let extent = plan.target.extent;
        let fits = self.target.as_ref().is_some_and(|t| t.texture.width() == extent.width && t.texture.height() == extent.height);
        if !fits {
            let texture = self.state.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("hsk-studio-ui.view.texture"),
                size: wgpu::Extent3d { width: extent.width, height: extent.height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: TARGET_FORMAT,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let mut renderer = self.state.renderer.write();
            let id = match self.target.take() {
                Some(old) => {
                    renderer.update_egui_texture_from_wgpu_texture(&self.state.device, &view, wgpu::FilterMode::Nearest, old.id);
                    old.id
                }
                None => renderer.register_native_texture(&self.state.device, &view, wgpu::FilterMode::Nearest),
            };
            self.target = Some(Target { texture, id });
        }
        let target = self.target.as_ref().ok_or("no display texture")?;
        self.shared.render_to_texture(plan, None, request, sources, cancel, &target.texture).map_err(|e| format!("{e} ({e:?})"))?;
        Ok(target.id)
    }
}
