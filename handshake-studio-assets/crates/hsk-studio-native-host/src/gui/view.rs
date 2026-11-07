//! View functions of the frame's panels (pure: inputs in, nodes out). RED skeleton.
use hsk_studio_harness_view::view::LayerRow;
use kumihimo_ui::tree::Builder;

/// Rows built for a list `list_h` logical px tall.
pub fn rows_fit(list_h: f32) -> usize {
    let _ = list_h;
    0
}

/// The right dock: group header with the Layers tab, then the Layers body (virtualised rows, passive scrollbar).
pub fn layers_dock(b: &mut Builder<'_>, rows: &[LayerRow], selected: Option<&str>, first: usize, body_h: f32) {
    let _ = (b, rows, selected, first, body_h);
}
