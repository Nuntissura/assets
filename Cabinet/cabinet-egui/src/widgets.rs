use crate::Palette;
use cabinet::interaction::{parse_hex, rgb_hex};
use cabinet::*;
use egui::{Color32, Rect, Response, Sense, Stroke, Ui, Vec2};

pub(super) fn presentation_label(entry: &Entry) -> String {
    if entry.kind != EntryKind::Folder {
        return entry.label.clone();
    }
    match &entry.children {
        ChildrenState::Loaded => entry.label.clone(),
        ChildrenState::Unloaded => format!("{} · Not loaded", entry.label),
        ChildrenState::Loading => format!("{} · Loading…", entry.label),
        ChildrenState::Failed(reason) => format!("{} · Failed: {}", entry.label, reason),
    }
}
pub(super) fn sort_label(s: Sort) -> &'static str {
    match s {
        Sort::Manual => "Manual order",
        Sort::NameAscending => "Name A–Z",
        Sort::NameDescending => "Name Z–A",
        Sort::CreatedDescending => "Newest created",
        Sort::CreatedAscending => "Oldest created",
        Sort::ModifiedDescending => "Recently modified",
        Sort::ModifiedAscending => "Oldest modified",
        Sort::FileSizeAscending => "Size smallest first",
        Sort::FileSizeDescending => "Size largest first",
        Sort::FileTypeAscending => "File type A–Z",
        Sort::FileTypeDescending => "File type Z–A",
    }
}
pub(super) fn sort_options() -> [Sort; 11] {
    [
        Sort::Manual,
        Sort::NameAscending,
        Sort::NameDescending,
        Sort::CreatedDescending,
        Sort::CreatedAscending,
        Sort::ModifiedDescending,
        Sort::ModifiedAscending,
        Sort::FileSizeAscending,
        Sort::FileSizeDescending,
        Sort::FileTypeAscending,
        Sort::FileTypeDescending,
    ]
}
/// Hue/saturation disk with a brightness slider; changes remain a draft until Apply.
pub(super) fn color_wheel(ui: &mut Ui, rgb: &mut [u8; 3], hex: &mut String) -> (Rect, Rect) {
    let mut hsv = egui::ecolor::Hsva::from_srgb(*rgb);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(130.0), Sense::click_and_drag());
    let radius = 60.0;
    let center = rect.center();
    let mut mesh = egui::epaint::Mesh::default();
    let n = 72;
    mesh.colored_vertex(
        center,
        Color32::from(egui::ecolor::Hsva::new(0.0, 0.0, hsv.v, 1.0)),
    );
    for i in 0..=n {
        let h = i as f32 / n as f32;
        let a = h * std::f32::consts::TAU;
        mesh.colored_vertex(
            center + Vec2::new(a.cos(), a.sin()) * radius,
            Color32::from(egui::ecolor::Hsva::new(h, 1.0, hsv.v, 1.0)),
        );
    }
    for i in 0..n {
        mesh.add_triangle(0, i + 1, i + 2);
    }
    ui.painter().add(egui::Shape::mesh(mesh));
    let mut changed = false;
    if (response.clicked() || response.dragged()) && response.interact_pointer_pos().is_some() {
        let delta = response.interact_pointer_pos().unwrap() - center;
        hsv.h = (delta.y.atan2(delta.x) / std::f32::consts::TAU).rem_euclid(1.0);
        hsv.s = (delta.length() / radius).clamp(0.0, 1.0);
        changed = true;
    }
    let angle = hsv.h * std::f32::consts::TAU;
    ui.painter().circle_stroke(
        center + Vec2::new(angle.cos(), angle.sin()) * radius * hsv.s,
        4.0,
        Stroke::new(2.0_f32, Color32::WHITE),
    );
    changed |= ui
        .add(egui::Slider::new(&mut hsv.v, 0.0..=1.0).text("Brightness"))
        .changed();
    if changed {
        *rgb = hsv.to_srgb();
        *hex = rgb_hex(*rgb);
    }
    let hex_response = ui.add(egui::TextEdit::singleline(hex).hint_text("#RRGGBB"));
    if hex_response.changed() {
        if let Some(color) = parse_hex(hex) {
            *rgb = color;
        }
    }
    if parse_hex(hex).is_none() {
        ui.colored_label(ui.visuals().error_fg_color, "Enter six hexadecimal digits");
    }
    (rect, hex_response.rect)
}
pub(super) fn folder_plus(ui: &mut Ui, p: Palette) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::click());
    let painter = ui.painter();
    let color = if response.hovered() { p.accent } else { p.text };
    let r = rect.shrink(6.0);
    painter.rect_stroke(
        r,
        2.0,
        Stroke::new(1.5_f32, color),
        egui::StrokeKind::Inside,
    );
    painter.line_segment(
        [r.left_top(), r.left_top() + Vec2::new(6.0, -3.0)],
        Stroke::new(2.0_f32, color),
    );
    let c = r.center();
    painter.line_segment(
        [c - Vec2::new(3.0, 0.0), c + Vec2::new(3.0, 0.0)],
        Stroke::new(1.5_f32, color),
    );
    painter.line_segment(
        [c - Vec2::new(0.0, 3.0), c + Vec2::new(0.0, 3.0)],
        Stroke::new(1.5_f32, color),
    );
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "New folder"));
    response.on_hover_text("New folder")
}
