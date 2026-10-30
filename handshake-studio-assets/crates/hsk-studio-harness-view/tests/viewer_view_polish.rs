//! Lane SMB-VIEW-POLISH-1 (feature `viewer`): window gaps left by SMB-AI-AREA-TYPE-3 and F-R41-3.
//! 1. The Properties panel's widest row, measured from egui's own paint list (every text shape the
//!    panel drew, its width and whether egui may wrap it), at 900 x 700 with a document open.
//! 2. Text tool clicks: on an open vector path = path text, inside a closed one = area text,
//!    elsewhere = point text (real pointer events after the first frame drew the canvas).
//! 3. TYPE > Convert > Create Outlines with Ctrl+Shift+O (consumed before Ctrl+O).
//! 4. Path edits that change a live text's placed / overset counts are echoed on the status line;
//!    the Path Operations summary shows the live-text state.
//! Oracles: egui's paint list and panel state, the vector entry's bindings, layer kinds,
//! composite's reports, the status log, undo / redo. Illustrator behaviour UNVERIFIED (D5).
#![cfg(feature = "viewer")]

#[path = "../../hsk-studio-harness/tests/common/mod.rs"]
mod common;

use common::{Scratch, solid_psd};
use egui::containers::panel::PanelState;
use egui::epaint::Shape;
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::edit::Tool;
use hsk_studio_harness_view::view::hosted::HostedPanel;
use hsk_studio_harness_view::view::info;
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::shell::{CREATE_OUTLINES_SHORTCUT, Panel, ShellCommand, menu_entries};
use hsk_studio_harness_view::view::vector::{LiveTextKind, ShapeKind, VectorTool};
use hsk_studio_harness_view::view::widget;
use hsk_studio_nib::anchor_path::Constraint;

// ---- 1. F-R41-3: the Properties panel's rows -----------------------------------------------------

/// One text shape: its painted rect, whether its galley may wrap, the text.
type Row = (egui::Rect, bool, String);

fn texts(shape: &Shape, out: &mut Vec<Row>) {
    match shape {
        Shape::Vec(v) => v.iter().for_each(|s| texts(s, out)),
        Shape::Text(t) => {
            out.push((t.visual_bounding_rect(), t.galley.job.wrap.max_width.is_finite(), t.galley.text().to_owned()));
        }
        _ => {}
    }
}

/// The text rows the Properties panel drew in the last of `frames` frames at `w` x `h` (rows
/// starting inside the panel's rect), widest right edge first, and the panel's rect.
fn properties_rows(v: &mut Viewer, w: f32, h: f32, frames: usize) -> (egui::Rect, Vec<Row>) {
    let ctx = egui::Context::default();
    let input = || egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, h))), ..Default::default() };
    let mut out = ctx.run(input(), |ctx| v.ui(ctx));
    for _ in 1..frames {
        out = ctx.run(input(), |ctx| v.ui(ctx));
    }
    let panel = PanelState::load(&ctx, widget::id(widget::PROPERTIES_PANEL)).expect("properties panel state").rect;
    let mut rows = Vec::new();
    for c in out.shapes.iter().filter(|c| c.clip_rect.intersects(panel) && panel.expand(0.5).contains_rect(c.clip_rect.intersect(panel))) {
        texts(&c.shape, &mut rows);
    }
    rows.retain(|r| panel.expand(0.5).contains(r.0.min));
    rows.sort_by(|a, b| b.0.max.x.total_cmp(&a.0.max.x));
    (panel, rows)
}

fn psd_viewer(scratch: &Scratch) -> Viewer {
    let path = scratch.0.join("solid.psd");
    std::fs::write(&path, solid_psd(64, 48, "paper", [120, 130, 140])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&path).unwrap_or_else(|e| panic!("{e}"));
    v
}

/// F-R41-3: the scene of `viewer_side_panels::info_panel_width_is_stable` at 900 x 700 (a
/// document open, Info shown with samplers and a selection, no layer selected). Prints every text
/// row of the Properties panel with its width (`MEASURED` lines). Measured at fc10ccbd (before the
/// fix): Properties 445 px wide, its widest rows the import receipt's loss grid cells
/// `document/layer_extras` | `transformed` | `merged_image_oracle_unknown` (x 481.0..892.2, none
/// wrapping). Now: Properties keeps its default width and every row ends inside it.
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
    for (r, wraps, text) in &rows {
        println!("MEASURED row left {:.1} width {:.1} right {:.1} wraps {wraps} text {:?}", r.min.x, r.width(), r.max.x, text.chars().take(90).collect::<String>());
    }
    // Default 300 px (view/mod.rs) plus at most egui's rounding and frame; before the fix 445 px.
    assert!(panel.width() <= 310.0, "Properties opened at {:.1} px", panel.width());
    for (r, _, text) in &rows {
        assert!(r.max.x <= panel.max.x + 0.5, "{text:?} ends at {:.1}, past the panel's right edge {:.1}", r.max.x, panel.max.x);
    }
    // The loss entries are still drawn, one wrapping row each.
    let loss: Vec<&Row> = rows.iter().filter(|r| r.2.contains(" / ") && r.2 != "path / kind / code").collect();
    assert!(loss.len() == 3 && loss.iter().all(|r| r.1), "{loss:?}");
    assert!(rows.iter().any(|r| r.2.contains("merged_image_oracle_unknown")), "{rows:?}");
}

// ---- 2. Text tool gestures on paths --------------------------------------------------------------

const W: usize = 96;
const H: usize = 64;
const SCREEN: (f32, f32) = (1400.0, 900.0);

fn frame(v: &mut Viewer, ctx: &egui::Context, events: Vec<egui::Event>, modifiers: egui::Modifiers) {
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SCREEN.0, SCREEN.1))), events, modifiers, ..Default::default() };
    let _ = ctx.run(input, |ctx| v.ui(ctx));
}

/// White paper, unfilled and unstroked shapes, 12 pt text.
fn paper(label: &str) -> (Scratch, Viewer) {
    let scratch = Scratch::new(&format!("view-polish-{label}"));
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(W as u32, H as u32, "paper", [255, 255, 255])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    v.set_vector_tool(VectorTool { fill_on: false, stroke_on: false, stroke: [255, 255, 255], ..VectorTool::default() });
    v.text_size_pt = 12.0;
    (scratch, v)
}

/// The screen point of document point `(x, y)` from the canvas mapping of the last frame.
fn screen_of(v: &Viewer, x: f64, y: f64) -> egui::Pos2 {
    let view = v.state_json()["info"]["view"].clone();
    assert!(view.is_object(), "no canvas mapping: {}", v.state_json()["info"]);
    let f = |k: &str, i: usize| view[k][i].as_f64().unwrap();
    let zoom = view["zoom"].as_f64().unwrap();
    egui::pos2((f("shown_min", 0) + (x - f("origin", 0)) * zoom) as f32, (f("shown_min", 1) + (y - f("origin", 1)) * zoom) as f32)
}

/// A primary click on the canvas at document point `(x, y)`, aimed after a frame drew the canvas.
fn click(v: &mut Viewer, ctx: &egui::Context, x: f64, y: f64) {
    frame(v, ctx, vec![], egui::Modifiers::NONE);
    let p = screen_of(v, x, y);
    let button = |pressed| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    frame(v, ctx, vec![egui::Event::PointerMoved(p)], egui::Modifiers::NONE);
    frame(v, ctx, vec![button(true)], egui::Modifiers::NONE);
    frame(v, ctx, vec![button(false)], egui::Modifiers::NONE);
    frame(v, ctx, vec![], egui::Modifiers::NONE);
}

fn status(v: &Viewer) -> Vec<String> {
    v.state_json()["status"].as_array().map(|a| a.iter().filter_map(|l| l.as_str().map(String::from)).collect()).unwrap_or_default()
}

fn kinds(v: &Viewer) -> Vec<String> {
    v.layers().into_iter().map(|r| r.kind).collect()
}

/// (path-text layers, area-text containers) of the vector entry.
fn bindings(v: &Viewer) -> (Vec<String>, Vec<String>) {
    let e = v.vector_entry().unwrap();
    (e.records.path_texts.iter().map(|p| p.path_layer_id.clone()).collect(), e.records.area_texts.iter().map(|a| a.container_layer_id.clone()).collect())
}

/// Text tool clicks in the real viewer (headless egui, real pointer events): inside a closed path
/// = area text in it, on an open path = path text on it, elsewhere = point text; each one undo
/// step. Studio reading; Illustrator behaviour UNVERIFIED (D5).
#[test]
fn text_tool_clicks_bind_area_and_path_text_or_place_point_text() {
    let (_s, mut v) = paper("gestures");
    let rect = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [80.0, 50.0], false).unwrap();
    let line = v.draw_shape(ShapeKind::Line, [8.0, 58.0], [88.0, 58.0], false).unwrap();
    v.set_tool(Tool::Text);
    let ctx = egui::Context::default();
    let layers0 = kinds(&v);

    // Inside the closed rectangle: area text in it, no new layer.
    click(&mut v, &ctx, 40.0, 30.0);
    assert_eq!(bindings(&v), (vec![], vec![rect.clone()]), "{:?}", status(&v));
    assert_eq!(kinds(&v), layers0);
    let reports = v.vector_text_reports().unwrap();
    assert_eq!((reports.len(), reports[0].report.layer_id.as_str()), (1, rect.as_str()));
    assert!(reports[0].report.outcome.as_ref().is_ok_and(|d| d.placed > 0), "{:?}", reports[0].report);
    assert!(status(&v).iter().any(|l| l.contains("glyphs placed")), "{:?}", status(&v));
    // One undo step removes it; redo brings it back.
    v.undo().unwrap();
    assert_eq!(bindings(&v), (vec![], vec![]));
    v.redo().unwrap();
    assert_eq!(bindings(&v), (vec![], vec![rect.clone()]));

    // On the open line: path text on it, the line selected.
    click(&mut v, &ctx, 48.0, 58.0);
    assert_eq!(bindings(&v), (vec![line.clone()], vec![rect.clone()]), "{:?}", status(&v));
    assert_eq!(kinds(&v), layers0);
    assert_eq!(v.state_json()["selected"], line.as_str());
    v.undo().unwrap();
    assert_eq!(bindings(&v), (vec![], vec![rect.clone()]));

    // Elsewhere (outside the rectangle, off the line): a point-text layer, no binding.
    click(&mut v, &ctx, 88.0, 5.0);
    assert_eq!(bindings(&v), (vec![], vec![rect.clone()]));
    assert_eq!(kinds(&v).iter().filter(|k| *k == "text").count(), 1, "{:?}", kinds(&v));
    v.undo().unwrap();
    assert_eq!(kinds(&v), layers0);

    // Model surface: a non-finite point binds nothing.
    assert!(v.text_click([f64::NAN, 5.0], 1.0, "Text").is_err());
    assert_eq!(bindings(&v), (vec![], vec![rect]));
}

// ---- 3. TYPE > Convert > Create Outlines -----------------------------------------------------------

fn ctrl_shift() -> egui::Modifiers {
    egui::Modifiers { ctrl: true, shift: true, command: true, ..Default::default() }
}

/// The menu leaf and its key run the Path Operations command; Ctrl+O keeps opening the dialog.
#[test]
fn create_outlines_is_a_type_menu_leaf_with_ctrl_shift_o() {
    // The binding is unique among every menu entry, panel toggle and hosted-panel toggle, and no
    // plain or Shift-only O binding exists.
    let entries = menu_entries();
    let leaf = entries.iter().find(|e| e.command == ShellCommand::CreateOutlines).expect("Create Outlines entry");
    assert_eq!((leaf.menu, leaf.label, leaf.shortcut), ("Type", "Create Outlines", Some(CREATE_OUTLINES_SHORTCUT)));
    let mut keys: Vec<&str> = entries.iter().filter(|e| !matches!(e.command, ShellCommand::Panel(_))).filter_map(|e| e.shortcut).collect();
    keys.extend(Panel::ALL.iter().map(|p| p.shortcut_text()));
    assert_eq!(keys.iter().filter(|k| k.eq_ignore_ascii_case(CREATE_OUTLINES_SHORTCUT)).count(), 1, "{keys:?}");
    assert!(!keys.iter().any(|k| *k == "O" || *k == "Shift+O"), "{keys:?}");
    let n = keys.len();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), n, "a shortcut bound twice");
    let bar = Viewer::new(Prefer::Cpu).menu_bar();
    let titles: Vec<&str> = bar.titles.iter().map(|t| t.label.as_str()).collect();
    assert_eq!(titles, ["File", "Edit", "View", "Type", "Color", "Workspace", "Help"], "[STU-SHL-011] TYPE between VIEW and COLOR");

    // Refused with the menu's reason: nothing open, then no layer selected.
    let mut v = Viewer::new(Prefer::Cpu);
    assert!(v.shell_command(ShellCommand::CreateOutlines).unwrap_err().contains("nothing open"));
    let (_s, mut v) = paper("outlines-key");
    let text = v.place_text(8.0, 40.0, "Outline me").unwrap();
    v.select(None);
    assert!(v.shell_command(ShellCommand::CreateOutlines).unwrap_err().contains("select a text layer"));

    // Ctrl+Shift+O (a real key event): the point text becomes outlines; no file dialog opens.
    v.select(Some(text.clone()));
    let ctx = egui::Context::default();
    frame(&mut v, &ctx, vec![], egui::Modifiers::NONE);
    let key = |m| egui::Event::Key { key: egui::Key::O, physical_key: None, pressed: true, repeat: false, modifiers: m };
    frame(&mut v, &ctx, vec![key(ctrl_shift())], ctrl_shift());
    assert!(v.state_json()["shell"]["dialog"].is_null(), "Ctrl+O took Ctrl+Shift+O: {}", v.state_json()["shell"]["dialog"]);
    assert!(!v.layers().iter().any(|r| r.layer_id == text), "{:?}", status(&v));
    assert!(status(&v).iter().any(|l| l.starts_with("Path Operations: outlines in ")), "{:?}", status(&v));
    // One undo step restores the text layer; the menu leaf does the same as the key.
    v.undo().unwrap();
    assert!(v.layers().iter().any(|r| r.layer_id == text && r.kind == "text"));
    v.select(Some(text.clone()));
    v.shell_command(ShellCommand::CreateOutlines).unwrap();
    assert!(!v.layers().iter().any(|r| r.layer_id == text));
    // Ctrl+O still opens the file dialog.
    frame(&mut v, &ctx, vec![key(egui::Modifiers::COMMAND)], egui::Modifiers::COMMAND);
    assert!(v.state_json()["shell"]["dialog"].is_object(), "{}", v.state_json()["shell"]["dialog"]);
}

// ---- 4. Overset echo after path edits, Path Operations summary --------------------------------------

/// A path edit that changes the placed / overset counts of the bound area text is echoed on the
/// status line at the next render; the Path Operations summary shows the live-text state.
#[test]
fn path_edits_echo_live_text_counts_and_the_summary_shows_them() {
    let (_s, mut v) = paper("echo");
    let rect = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [40.0, 30.0], false).unwrap();
    v.select(Some(rect.clone()));
    assert!(v.hosted_summary(HostedPanel::PathOperations).ends_with(": live shape"), "{}", v.hosted_summary(HostedPanel::PathOperations));
    v.bind_live_text(&rect, "Overset area text flows on and on", LiveTextKind::Area { inset_pt: 0.0 }).unwrap();
    v.render_if_dirty();
    let counts = |v: &Viewer| v.vector_text_reports().unwrap()[0].report.outcome.as_ref().map(|d| (d.placed, d.overset_count)).unwrap();
    let (placed0, overset0) = counts(&v);
    assert!(overset0 > 0, "the small frame should overset: {placed0} placed");
    let summary = v.hosted_summary(HostedPanel::PathOperations);
    assert!(summary.contains(&format!("{placed0} glyphs placed, {overset0} overset")), "{summary}");
    let before = status(&v).len();
    // Drag the bottom-right corner out: the frame grows, more glyphs are placed.
    v.direct_drag([40.0, 30.0], [90.0, 60.0], Constraint::Free, false, 2.0).unwrap();
    v.render_if_dirty();
    let (placed1, overset1) = counts(&v);
    assert!(placed1 > placed0 && overset1 < overset0, "{placed0}/{overset0} -> {placed1}/{overset1}");
    let new: Vec<String> = status(&v).split_off(before);
    let want = if overset1 > 0 { format!("{placed1} glyphs placed, {overset1} overset") } else { format!("{placed1} glyphs placed") };
    assert!(new.iter().any(|l| l.starts_with("live text STXT-") && l.ends_with(&want)), "{want} not echoed: {new:?}");
    assert!(v.hosted_summary(HostedPanel::PathOperations).ends_with(&want), "{}", v.hosted_summary(HostedPanel::PathOperations));
    // A render without a count change says nothing more.
    let before = status(&v).len();
    v.set_visible(&rect, false).unwrap();
    v.render_if_dirty();
    v.set_visible(&rect, true).unwrap();
    v.render_if_dirty();
    assert!(!status(&v).split_off(before).iter().any(|l| l.starts_with("live text")), "{:?}", status(&v));
}
