//! Optional exact-point targeting. Native hosts supply the OS Drop callback point.
use crate::*;
use kumihimo_content::{DropDestination, MountStatus};

#[derive(Clone, Copy, Debug)]
pub struct HostDropPoint { pub point: egui::Pos2, pub epoch: u64 }
/// Resolve the topmost painted content slot. Never use egui's cached pointer here.
pub fn destination(controller: &Controller, report: &FrameReport, point: HostDropPoint) -> Result<DropDestination> {
    if point.epoch == 0 || point.epoch != report.epoch || report.post_generation != controller.snapshot().generation || report.generation != report.post_generation || !point.point.x.is_finite() || !point.point.y.is_finite() {
        return Err(AdapterError::Stale("drop surface/geometry"));
    }
    let slot = report.slots.iter().rev().find(|slot| {
        slot.visible && slot.epoch == point.epoch && to_egui(slot.clip).contains(point.point) && to_egui(slot.bounds).contains(point.point)
    }).ok_or(AdapterError::Unsupported("drop outside content"))?;
    let inspected = controller.inspect();
    let mount = inspected.mounts.values().find(|m| m.active && m.status == MountStatus::Attached && m.scope.target.view() == Some(slot.view) && m.scope.resource == slot.resource).ok_or(AdapterError::Stale("drop mount"))?;
    Ok(DropDestination { scope: mount.scope.clone(), frame: slot.frame, generation: report.generation, epoch: point.epoch })
}
pub fn paint_preview(ctx: &egui::Context, report: &FrameReport, destination: &DropDestination, action: kumihimo_content::DropAction) {
    if let Some(slot) = report.slots.iter().find(|s| s.view == destination.scope.target.view().unwrap_or(ViewId::from_raw(0)) && s.epoch == destination.epoch) {
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("kumihimo-file-drop-preview")));
        painter.rect_stroke(to_egui(slot.clip), 0.0, egui::Stroke::new(3.0, egui::Color32::LIGHT_BLUE), egui::StrokeKind::Inside);
        painter.text(to_egui(slot.clip).left_top() + egui::vec2(8.0, 8.0), egui::Align2::LEFT_TOP, format!("{} · frame {} · view {}", action.label(), slot.frame.raw(), slot.view.raw()), egui::FontId::proportional(15.0), egui::Color32::WHITE);
    }
}
