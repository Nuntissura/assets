//! Lane SMB-PS-INFO-HISTOGRAM-1 (feature `viewer`): the right side panels keep one width from frame
//! to frame. The Layers panel narrowed every frame once its wrapped row used less than the panel
//! (SMB-SHELL-ROW-WRAP-1) and the develop rows widened it (diagnosis smb-lr-healing-2-1); this
//! checks the right Properties / Develop panel and the Info panel the same way: headless egui, sizes
//! from egui's own panel state (`PanelState`, the rect a panel used last frame), 3 warm-up frames,
//! then 20 frames must agree, at the desktop default viewport (1400 x 900) and a constrained one
//! (900 x 700). Each panel also stays at least as wide as its default (it never narrows to its
//! narrowest row), Info included at 900 x 700 with a document open (F-R41-3, lane
//! SMB-VIEW-POLISH-1: the import receipt's loss grid, measured by
//! `viewer_view_polish::properties_rows_measured_at_900x700`, opened Properties at 445 px and left
//! Info clamped below its default; its entries now wrap and Properties keeps its default width).
//! NOT_PROVEN: a real desktop window (the same `Viewer::ui` draws there).
#![cfg(feature = "viewer")]

#[path = "../../hsk-studio-harness/tests/common/mod.rs"]
mod common;

use common::{Scratch, solid_psd, synthetic_dng};
use egui::containers::panel::PanelState;
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::info;
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::shell::{Panel, ShellCommand};
use hsk_studio_harness_view::view::widget;

const WARMUP: usize = 3;
const FRAMES: usize = 20;
/// Default widths (view/mod.rs, view/info.rs) less egui's panel frame margins.
const PROPERTIES_MIN: f32 = 280.0;
const INFO_MIN: f32 = 240.0;

/// Per frame, the width of each panel in `ids` on a `w` x `h` screen.
fn widths(v: &mut Viewer, ids: &[&str], w: f32, h: f32) -> Vec<Vec<f32>> {
    let ctx = egui::Context::default();
    let input = || egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, h))), ..Default::default() };
    (0..WARMUP + FRAMES)
        .map(|_| {
            let _ = ctx.run(input(), |ctx| v.ui(ctx));
            ids.iter().map(|id| PanelState::load(&ctx, widget::id(id)).unwrap_or_else(|| panic!("{id} panel state")).rect.width()).collect()
        })
        .collect()
}

/// After the warm-up every panel keeps one width, at least `mins[i]` (pass 0 where the window is too
/// narrow for every panel's default: egui then clamps the inner panel to the space left, a fixed
/// width, not a creep).
fn check(label: &str, ids: &[&str], mins: &[f32], all: &[Vec<f32>]) {
    let trace = all.iter().enumerate().map(|(i, ws)| format!("frame {}: {}", i + 1, ids.iter().zip(ws).map(|(id, w)| format!("{id} {w:.1}")).collect::<Vec<_>>().join(" "))).collect::<Vec<_>>().join("\n");
    eprintln!("{label}:\n{trace}");
    let settled = &all[WARMUP..];
    for (k, id) in ids.iter().enumerate() {
        let w0 = settled[0][k];
        assert!(settled.iter().all(|ws| (ws[k] - w0).abs() < 0.5), "{label}: {id} width changed across {FRAMES} frames after {WARMUP} warm-up frames:\n{trace}");
        assert!(w0 >= mins[k], "{label}: {id} narrowed to {w0:.1} px (default about {} px):\n{trace}", mins[k]);
    }
}

fn psd_viewer(scratch: &Scratch) -> Viewer {
    let path = scratch.0.join("solid.psd");
    std::fs::write(&path, solid_psd(64, 48, "paper", [120, 130, 140])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&path).unwrap_or_else(|e| panic!("{e}"));
    v
}

#[test]
fn properties_panel_width_is_stable() {
    // Nothing open: the panel holds only short lines ("Select a layer.").
    for (w, h) in [(1400.0, 900.0), (900.0, 700.0)] {
        let mut v = Viewer::new(Prefer::Cpu);
        let all = widths(&mut v, &[widget::PROPERTIES_PANEL], w, h);
        check(&format!("properties, nothing open, {w}x{h}"), &[widget::PROPERTIES_PANEL], &[PROPERTIES_MIN], &all);
    }
    // A layer selected: the controls-descriptor panel and the import receipt.
    let scratch = Scratch::new("side-properties");
    for (w, h) in [(1400.0, 900.0), (900.0, 700.0)] {
        let mut v = psd_viewer(&scratch);
        let layer = v.layers()[0].layer_id.clone();
        v.select(Some(layer));
        let all = widths(&mut v, &[widget::PROPERTIES_PANEL], w, h);
        check(&format!("properties, layer selected, {w}x{h}"), &[widget::PROPERTIES_PANEL], &[PROPERTIES_MIN], &all);
    }
}

#[test]
fn develop_panel_width_is_stable() {
    // A camera raw open: develop and properties share the right panel.
    let scratch = Scratch::new("side-develop");
    let dng = scratch.0.join("raw.dng");
    std::fs::write(&dng, synthetic_dng(64, 48)).unwrap();
    for (w, h) in [(1400.0, 900.0), (900.0, 700.0)] {
        let mut v = Viewer::new(Prefer::Cpu);
        v.open(&dng).unwrap_or_else(|e| panic!("{e}"));
        assert!(v.develop_available());
        let all = widths(&mut v, &[widget::PROPERTIES_PANEL], w, h);
        check(&format!("develop + properties, {w}x{h}"), &[widget::PROPERTIES_PANEL], &[PROPERTIES_MIN], &all);
        // Develop alone (Properties hidden).
        v.shell_command(ShellCommand::Panel(Panel::Properties)).unwrap();
        let all = widths(&mut v, &[widget::PROPERTIES_PANEL], w, h);
        check(&format!("develop alone, {w}x{h}"), &[widget::PROPERTIES_PANEL], &[PROPERTIES_MIN], &all);
    }
}

#[test]
fn info_panel_width_is_stable() {
    let scratch = Scratch::new("side-info");
    for (w, h) in [(1400.0, 900.0), (900.0, 700.0)] {
        // Nothing open, then a document with samplers and a selection (the longest rows).
        let mut v = Viewer::new(Prefer::Cpu);
        v.shell_command(ShellCommand::Panel(Panel::Info)).unwrap();
        let ids = [widget::PROPERTIES_PANEL, info::widget::PANEL];
        let all = widths(&mut v, &ids, w, h);
        check(&format!("info + properties, nothing open, {w}x{h}"), &ids, &[PROPERTIES_MIN, INFO_MIN], &all);
        let mut v = psd_viewer(&scratch);
        v.shell_command(ShellCommand::Panel(Panel::Info)).unwrap();
        for k in 0..4 {
            v.info_command(info::InfoCommand::AddSampler { x: f64::from(k * 10), y: 5.0 }).unwrap();
        }
        v.render_if_dirty();
        v.select_rect(2.0, 3.0, 20.0, 10.0).unwrap();
        let _ = v.info_eyedropper(5.0, 5.0).unwrap();
        // At 900 px too: Layers (240) + Properties (300) + Info (260) defaults fit the window.
        let all = widths(&mut v, &ids, w, h);
        check(&format!("info + properties, samplers and selection, {w}x{h}"), &ids, &[PROPERTIES_MIN, INFO_MIN], &all);
    }
}
