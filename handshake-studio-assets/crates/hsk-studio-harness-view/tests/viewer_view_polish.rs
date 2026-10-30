//! Lane SMB-VIEW-POLISH-1 (feature `viewer`): window gaps left by SMB-AI-AREA-TYPE-3 and F-R41-3.
//! 1. The Properties panel's widest row, measured from egui's own paint list (every text shape the
//!    panel drew, its width and whether egui may wrap it), at 900 x 700 with a document open.
#![cfg(feature = "viewer")]

#[path = "../../hsk-studio-harness/tests/common/mod.rs"]
mod common;

use common::{Scratch, solid_psd};
use egui::containers::panel::PanelState;
use egui::epaint::Shape;
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::info;
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::shell::{Panel, ShellCommand};
use hsk_studio_harness_view::view::widget;

/// One text shape: left edge, width, whether its galley may wrap, the text.
type Row = (f32, f32, bool, String);

fn texts(shape: &Shape, out: &mut Vec<Row>) {
    match shape {
        Shape::Vec(v) => v.iter().for_each(|s| texts(s, out)),
        Shape::Text(t) => {
            let r = t.visual_bounding_rect();
            out.push((r.min.x, r.width(), t.galley.job.wrap.max_width.is_finite(), t.galley.text().to_owned()));
        }
        _ => {}
    }
}

/// The text rows the Properties panel drew in the last of `frames` frames at `w` x `h`, widest
/// right edge first, and the panel's rect.
fn properties_rows(v: &mut Viewer, w: f32, h: f32, frames: usize) -> (egui::Rect, Vec<Row>) {
    let ctx = egui::Context::default();
    let input = || egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, h))), ..Default::default() };
    let mut out = ctx.run(input(), |ctx| v.ui(ctx));
    for _ in 1..frames {
        out = ctx.run(input(), |ctx| v.ui(ctx));
    }
    let panel = PanelState::load(&ctx, widget::id(widget::PROPERTIES_PANEL)).expect("properties panel state").rect;
    let mut rows = Vec::new();
    for c in out.shapes.iter().filter(|c| panel.contains_rect(c.clip_rect.intersect(panel)) && c.clip_rect.intersects(panel)) {
        texts(&c.shape, &mut rows);
    }
    rows.retain(|r| r.0 >= panel.min.x - 0.5 && r.0 < panel.max.x);
    rows.sort_by(|a, b| (b.0 + b.1).total_cmp(&(a.0 + a.1)));
    (panel, rows)
}

fn psd_viewer(scratch: &Scratch) -> Viewer {
    let path = scratch.0.join("solid.psd");
    std::fs::write(&path, solid_psd(64, 48, "paper", [120, 130, 140])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&path).unwrap_or_else(|e| panic!("{e}"));
    v
}

/// F-R41-3 measurement: the scene of `viewer_side_panels::info_panel_width_is_stable` at 900 x 700
/// (a document open, Info shown with samplers and a selection, no layer selected). Prints every
/// text row of the Properties panel with its width; the widest non-wrapping row is the one that
/// sets the panel's width.
#[test]
fn properties_rows_measured_at_900x700() {
    let scratch = Scratch::new("view-polish-rows");
    let mut v = psd_viewer(&scratch);
    v.shell_command(ShellCommand::Panel(Panel::Info)).unwrap();
    for k in 0..4 {
        v.info_command(info::InfoCommand::AddSampler { x: f64::from(k * 10), y: 5.0 }).unwrap();
    }
    v.render_if_dirty();
    v.select_rect(2.0, 3.0, 20.0, 10.0).unwrap();
    let _ = v.info_eyedropper(5.0, 5.0).unwrap();
    let (panel, rows) = properties_rows(&mut v, 900.0, 700.0, 6);
    println!("MEASURED properties panel {:.1} px wide (x {:.1}..{:.1})", panel.width(), panel.min.x, panel.max.x);
    for (left, width, wraps, text) in &rows {
        println!("MEASURED row left {left:.1} width {width:.1} right {:.1} wraps {wraps} text {:?}", left + width, text.chars().take(90).collect::<String>());
    }
    assert!(!rows.is_empty());
}
