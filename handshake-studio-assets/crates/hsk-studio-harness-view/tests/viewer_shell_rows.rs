//! Lane SMB-SHELL-ROW-WRAP-1 (feature `viewer`): the main window's wrapping rows (toolbar tool row,
//! paint-select and vector option rows, Layers panel buttons) wrap their items, so the toolbar keeps
//! one sane height and the Layers panel one width from frame to frame. Before the fix every item
//! sat in a bare `ui.push_id` the row never wrapped (diagnosis smb-lr-healing-2-1, F-TL-1 of
//! SMB-TIMELINE-UI-1: the toolbar about 800 px tall on a 1200 px screen). Headless egui; sizes from
//! egui's own panel state (`PanelState`, the rect a panel used last frame); the first frames are
//! skipped (a wrapping item is sized from its previous frame, F-TL-2), then 20 frames must agree.
//! NOT_PROVEN: a real desktop window (the same `Viewer::ui` draws there).
#![cfg(feature = "viewer")]

mod support_context;

use egui::containers::panel::PanelState;
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::edit::Tool;
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::widget;

/// Frames before measuring: frame 1 sizes measured items, frame 2 allocates them, frame 3 sees the
/// panels at their final sizes.
const WARMUP: usize = 3;
const FRAMES: usize = 20;

/// Per frame (toolbar height, Layers panel width) on a `w` x `h` screen.
fn sizes(v: &mut Viewer, w: f32, h: f32) -> Vec<(f32, f32)> {
    let ctx = egui::Context::default();
    let input = || egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, h))), ..Default::default() };
    (0..WARMUP + FRAMES)
        .map(|_| {
            let _ = ctx.run(input(), |ctx| v.ui(ctx));
            let toolbar = PanelState::load(&ctx, widget::id("toolbar")).expect("toolbar panel state").rect.height();
            let layers = PanelState::load(&ctx, widget::id(widget::LAYER_PANEL)).expect("layers panel state").rect.width();
            (toolbar, layers)
        })
        .collect()
}

/// The sizes after the warm-up are one size; the toolbar is at most `max_toolbar` px tall.
fn check(label: &str, all: &[(f32, f32)], max_toolbar: f32) {
    let trace = all.iter().enumerate().map(|(i, (t, l))| format!("frame {}: toolbar {t:.1} layers {l:.1}", i + 1)).collect::<Vec<_>>().join("\n");
    eprintln!("{label}:\n{trace}");
    let settled = &all[WARMUP..];
    let (t0, l0) = settled[0];
    assert!(settled.iter().all(|(t, l)| (t - t0).abs() < 0.5 && (l - l0).abs() < 0.5), "{label}: toolbar height / Layers width changed across {FRAMES} frames after {WARMUP} warm-up frames:\n{trace}");
    assert!(t0 <= max_toolbar, "{label}: toolbar {t0:.1} px tall (bound {max_toolbar} px):\n{trace}");
}

#[test]
fn toolbar_and_layers_stable_on_default_viewport() {
    // The desktop binary's default viewport (studio-harness.rs: 1400 x 900).
    let mut v = crate::support_context::viewer(Prefer::Cpu);
    let all = sizes(&mut v, 1400.0, 900.0);
    check("default tool, 1400x900", &all, 200.0);
}

#[test]
fn toolbar_and_layers_stable_with_vector_options() {
    // A vector tool adds the fill / stroke / shape option items to the tool row.
    let mut v = crate::support_context::viewer(Prefer::Cpu);
    v.set_tool(Tool::Star);
    let all = sizes(&mut v, 1400.0, 900.0);
    check("star tool, 1400x900", &all, 200.0);
}

#[test]
fn toolbar_and_layers_stable_on_narrow_viewport() {
    // More wrapping: still one size, and the toolbar leaves most of the window to the canvas.
    let mut v = crate::support_context::viewer(Prefer::Cpu);
    v.set_tool(Tool::Star);
    let all = sizes(&mut v, 900.0, 700.0);
    check("star tool, 900x700", &all, 350.0);
}
