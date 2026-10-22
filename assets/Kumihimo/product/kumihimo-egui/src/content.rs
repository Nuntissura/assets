//! Host-owned painting in canonical, ancestor-clipped slots.
use crate::*;

pub fn draw(
    ctx: &egui::Context,
    controller: &mut Controller,
    projection: &workspace::Projection,
    surface: SurfaceBinding,
    host: &mut impl DesktopHost,
    report: &mut FrameReport,
) {
    let visible: Vec<_> = projection
        .frames
        .iter()
        .filter(|f| f.child.is_none() && f.clip.width() > 0.0 && f.clip.height() > 0.0)
        .filter_map(|f| {
            controller
                .snapshot()
                .frames
                .get(&f.frame)
                .and_then(|f| f.selected_view)
        })
        .collect();
    if let Err(e) = host.content_visibility(controller, surface, &visible) {
        report.reject(e);
    }
    for frame in &projection.frames {
        if frame.child.is_some() {
            continue;
        }
        let Some(view) = controller
            .snapshot()
            .frames
            .get(&frame.frame)
            .and_then(|f| f.selected_view)
        else {
            continue;
        };
        let Some(record) = controller.snapshot().views.get(&view) else {
            continue;
        };
        if record.closed.is_some() {
            continue;
        }
        let result = (|| -> Result<RenderSlot> {
            let (bounds, _) =
                crate::content_and_navigation_bounds(controller, frame, &host.policy())?;
            let clip = intersect(bounds, frame.clip)?;
            let size = SurfaceBinding { bounds, ..surface }.physical_size()?;
            let focused = controller
                .inspect()
                .mounts
                .values()
                .any(|m| m.scope.target.view() == Some(view) && m.signals.focused);
            Ok(RenderSlot {
                view,
                resource: record.resource,
                frame: frame.frame,
                workspace: frame.workspace,
                canonical_bounds: frame.content,
                bounds,
                clip,
                physical_size: size,
                scale: surface.pixels_per_point,
                epoch: surface.epoch,
                focused,
                visible: clip.width() > 0.0 && clip.height() > 0.0,
            })
        })();
        let slot = match result {
            Ok(s) => s,
            Err(e) => {
                report.reject(e);
                continue;
            }
        };
        if !slot.visible {
            continue;
        }
        let mut ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::new(("kum-content", surface.viewport, frame.frame.raw())),
            egui::UiBuilder::new()
                .layer_id(frame.layer())
                .max_rect(to_egui(slot.bounds)),
        );
        {
            ui.set_min_size(to_egui(slot.bounds).size());
            ui.set_max_size(to_egui(slot.bounds).size());
            ui.set_clip_rect(to_egui(slot.clip));
            match host.paint_content(&mut ui, &slot, controller) {
                Ok(()) => report.painted_views += 1,
                Err(e) => {
                    report.reject(&e);
                    ui.label(format!("Provider unavailable: {e}"));
                }
            }
        }
        report.slots.push(slot);
    }
}

/// A renderer issues this binding from its actual device and current surface epoch.
/// Hosts must reuse the binding, never infer device compatibility from texture IDs.
#[cfg(feature = "capture")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuBinding {
    device: wgpu::Device,
    epoch: u64,
    viewport: egui::ViewportId,
}
#[cfg(feature = "capture")]
impl GpuBinding {
    pub(crate) fn new(device: &wgpu::Device, surface: SurfaceBinding) -> Result<Self> {
        surface.validate()?;
        Ok(Self {
            device: device.clone(),
            epoch: surface.epoch,
            viewport: surface.viewport,
        })
    }
    pub fn validate(&self, renderer: &Self, slot: &RenderSlot, native_child: bool) -> Result<()> {
        if native_child {
            return Err(AdapterError::Unsupported("native child content surfaces"));
        }
        if self.device != renderer.device {
            return Err(AdapterError::Unsupported("incompatible graphics device"));
        }
        if self != renderer || self.epoch != slot.epoch {
            return Err(AdapterError::Stale("GPU handoff epoch/viewport"));
        }
        Ok(())
    }
}
#[cfg(feature = "capture")]
pub fn paint_gpu_texture(
    ui: &egui::Ui,
    slot: &RenderSlot,
    provider: GpuBinding,
    renderer: GpuBinding,
    texture: egui::TextureId,
    native_child: bool,
) -> Result<()> {
    provider.validate(&renderer, slot, native_child)?;
    ui.painter().with_clip_rect(to_egui(slot.clip)).image(
        texture,
        to_egui(slot.bounds),
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
    Ok(())
}
#[cfg(feature = "capture")]
pub fn paint_gpu_callback(
    ui: &egui::Ui,
    slot: &RenderSlot,
    provider: GpuBinding,
    renderer: GpuBinding,
    callback: impl egui_wgpu::CallbackTrait + 'static,
    native_child: bool,
) -> Result<()> {
    provider.validate(&renderer, slot, native_child)?;
    ui.painter()
        .with_clip_rect(to_egui(slot.clip))
        .add(egui_wgpu::Callback::new_paint_callback(
            to_egui(slot.bounds),
            callback,
        ));
    Ok(())
}
