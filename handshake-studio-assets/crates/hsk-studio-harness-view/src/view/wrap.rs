//! Items of wrapping rows (`ui.horizontal_wrapped`) under a stable id.
//!
//! A bare `ui.push_id(id, ..)` in a wrapping row opens a child Ui the row never wraps: past the row
//! end the child has zero width, a button's text shrinks to one glyph (text items wrap glyph by
//! glyph, sliders and checkboxes their labels), the row overflows and an auto-sized panel stores
//! that overflow as its size, growing every frame (diagnosis smb-lr-healing-2-1: the develop panel
//! went from 300 to ~1200 px; F-TL-1: the viewer toolbar ~800 px tall on a 1200 px screen). The
//! helpers below let the row allocate each item at its unwrapped size first, so the row wraps it,
//! then draw it there under the same `push_id` id. Outside a wrapping layout they are a plain
//! `ui.push_id`.

/// A text item (button, selectable label, small button) of a wrapping row under the stable id `id`,
/// sized from its label: `text` / `style` are the item's label and text style (`Button` for buttons
/// and selectable labels, `Body` for small buttons).
pub(crate) fn wrap_item<R>(ui: &mut egui::Ui, id: egui::Id, text: &str, style: egui::TextStyle, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    if !ui.layout().main_wrap() {
        return ui.push_id(id, add).inner;
    }
    let galley = egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, style);
    let pad = ui.spacing().button_padding;
    // One point of slack: a label a hair short of its width would wrap inside the allocated cell.
    let size = egui::vec2(galley.size().x + 2.0 * pad.x + 1.0, (galley.size().y + 2.0 * pad.y).max(ui.spacing().interact_size.y));
    ui.allocate_ui(size, |ui| ui.push_id(id, add).inner).inner
}

/// Any item of a wrapping row (slider, checkbox, combo box, colour button) under the stable id `id`,
/// sized from its own previous frame: the item is drawn unwrapped (`TextWrapMode::Extend`) in a
/// cell of the size it used last frame (`interact_size` on its first frame), and the size it used
/// now is kept in egui temp memory for the next frame. The row is exact from the second frame on.
pub(crate) fn wrap_measured<R>(ui: &mut egui::Ui, id: egui::Id, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    if !ui.layout().main_wrap() {
        return ui.push_id(id, add).inner;
    }
    let key = id.with("wrap_measured.size");
    let size = ui.data(|d| d.get_temp::<egui::Vec2>(key)).unwrap_or(ui.spacing().interact_size);
    let r = ui.allocate_ui(size, |ui| {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        ui.push_id(id, add).inner
    });
    let used = r.response.rect.size();
    ui.data_mut(|d| d.insert_temp(key, used));
    r.inner
}
