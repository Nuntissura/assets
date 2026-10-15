//! Lane SMB-HARNESS-PROTOTYPE-PLAY (feature `viewer`): the prototype play mode driven headlessly
//! (no window) over the Operator Figma prototype fixture: the flow starts on its starting point,
//! clicks are hit-tested to the label layers and drive the components player, transitions render
//! as an eased cross-fade, and every screen reached is compared with the Operator PNG of that
//! screen; the viewer draws the play panel's stable widget ids.
#![cfg(feature = "viewer")]

use hsk_studio_components::PlayerEvent;
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::prototype::{PrototypePlay, Screen, widget};
use hsk_studio_harness::view::render::Prefer;

fn fig_fixture(name: &str) -> Vec<u8> {
    // The Operator fixtures live with the importer (repository-relative, never machine-local).
    let path = format!("{}/../hsk-studio-interop-fig/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {path}: {e}"))
}

fn decode_png(bytes: &[u8]) -> (usize, usize, Vec<u8>) {
    let img = hsk_studio_interop_raster::decode(bytes, &hsk_studio_interop_raster::Limits::default(), &Default::default()).unwrap();
    let hsk_studio_interop_raster::Samples::U8(s) = img.samples else { panic!("8-bit") };
    let ch = img.colour.channels();
    let rgb = s.chunks(ch).flat_map(|p| [p[0], p[1], p[2]]).collect();
    (img.width as usize, img.height as usize, rgb)
}

/// (mean, share within 8/255) of a screen vs a PNG of the same size.
fn compare(s: &Screen, png: &(usize, usize, Vec<u8>)) -> (f64, f64) {
    assert_eq!((s.width as usize, s.height as usize), (png.0, png.1));
    let d: Vec<f64> = s
        .rgb
        .chunks(3)
        .zip(png.2.chunks(3))
        .map(|(a, b)| (0..3).map(|c| (f64::from(a[c]) - f64::from(b[c])).abs()).fold(0.0, f64::max))
        .collect();
    let n = d.len() as f64;
    (d.iter().sum::<f64>() / n, d.iter().filter(|v| **v <= 8.0).count() as f64 / n)
}

fn label_centre(play: &PrototypePlay, name: &str) -> (f64, f64) {
    let d = play.proposal().snapshot.document();
    let screen = play.session().state().screen;
    let text = d.layers.iter().find(|l| l.name == name && l.kind == "text").expect("label");
    let layout = play.proposal().text_layouts.iter().find(|t| t.layer_id == text.layer_id).unwrap();
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for g in layout.glyphs.iter().filter_map(|g| g.path.as_ref()) {
        for e in &g.elements {
            if let hsk_studio_nib::PathElement::MoveTo(p) | hsk_studio_nib::PathElement::LineTo(p) = e {
                (x0, y0, x1, y1) = (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y));
            }
        }
    }
    // Frame-local: minus the screen frame's origin (its background bounds).
    let bg = play.proposal().frame_backgrounds.iter().find(|b| b.frame.node_id == screen).unwrap();
    let path = play.proposal().paths.iter().find(|p| p.layer_id == bg.layer_id).unwrap();
    let (mut fx, mut fy) = (f64::MAX, f64::MAX);
    for e in &path.geometry.elements {
        if let hsk_studio_nib::PathElement::MoveTo(p) | hsk_studio_nib::PathElement::LineTo(p) = e {
            (fx, fy) = (fx.min(p.x), fy.min(p.y));
        }
    }
    ((x0 + x1) / 2.0 - fx, (y0 + y1) / 2.0 - fy)
}

fn name_of(play: &PrototypePlay, id: &str) -> String {
    play.proposal().snapshot.document().layers.iter().find(|l| l.layer_id == id).unwrap().name.clone()
}

#[test]
fn play_mode_steps_the_prototype_fixture_against_the_operator_pngs() {
    let mut play = PrototypePlay::open(&fig_fixture("fig-prototype-pages.fig"), None).unwrap();
    let screen_png = |n: &str| decode_png(&fig_fixture(&format!("fig-prototype-pages-{n}.png")));
    // The flow starts on its stored starting point.
    assert_eq!(name_of(&play, &play.session().state().screen), "start-screen");
    let (mean, within8) = compare(&play.render().unwrap(), &screen_png("start"));
    eprintln!("start: mean {mean:.6} <=8 {within8:.6}");
    assert!(mean <= 0.378 && within8 >= 0.992259, "start");
    // Hover over the label: hit-tested to the text layer (no hover reaction on it: no change).
    let (x, y) = label_centre(&play, "Click: navigate to details");
    play.hover(x, y);
    assert_eq!(play.state_json()["hovered"].as_str().map(|id| name_of(&play, id)).as_deref(), Some("Click: navigate to details"));
    assert_eq!(name_of(&play, &play.session().state().screen), "start-screen");
    // Click it: navigate to the detail screen.
    let hit = play.click(x, y).unwrap();
    assert_eq!(name_of(&play, &hit), "Click: navigate to details");
    assert_eq!(name_of(&play, &play.session().state().screen), "detail-screen");
    let (mean, within8) = compare(&play.render().unwrap(), &screen_png("detail"));
    eprintln!("detail: mean {mean:.6} <=8 {within8:.6}");
    assert!(mean <= 0.340 && within8 >= 0.99283, "detail");
    // Smart animate (350 ms, ease in and out): the screen at 175 ms is the eased cross-fade.
    let (x, y) = label_centre(&play, "Click: smart animate");
    play.click(x, y).unwrap();
    play.advance(175);
    let mid = play.render().unwrap();
    let (t, raw, eased) = play.session().state().transition.unwrap();
    assert_eq!((name_of(&play, &t.to).as_str(), raw), ("animated-screen", 0.5));
    let from = play.frame_raster(&t.from).unwrap();
    let to = play.frame_raster(&t.to).unwrap();
    for i in (0..mid.rgb.len()).step_by(997) {
        let want = (f64::from(from.rgb[i]) * (1.0 - eased) + f64::from(to.rgb[i]) * eased).round();
        assert_eq!(f64::from(mid.rgb[i]), want, "cross-fade at byte {i}");
    }
    play.advance(175);
    assert!(play.take_events().contains(&PlayerEvent::TransitionEnded { to: t.to.clone() }));
    let (mean, within8) = compare(&play.render().unwrap(), &screen_png("animated"));
    eprintln!("animated: mean {mean:.6} <=8 {within8:.6}");
    assert!(mean <= 0.349 && within8 >= 0.99197, "animated");
    // Back to the start screen; a click on empty frame area hits the background (no reaction).
    let (x, y) = label_centre(&play, "Click: back to start");
    play.click(x, y).unwrap();
    assert_eq!(name_of(&play, &play.session().state().screen), "start-screen");
    play.take_events();
    play.click(300.0, 300.0);
    assert_eq!(play.take_events(), [PlayerEvent::Diagnostic("no_reaction_for_trigger")]);
    // Restart returns to the flow start with an empty history.
    play.restart();
    assert_eq!(play.session().state().history_depth, 0);
}

#[test]
fn viewer_draws_the_play_panel() {
    let mut v = Viewer::new(Prefer::Cpu);
    v.open_prototype(&fig_fixture("fig-prototype-pages.fig")).unwrap();
    let ctx = egui::Context::default();
    let _ = ctx.run(egui::RawInput::default(), |ctx| v.ui(ctx));
    let drawn = v.drawn();
    for id in [widget::PANEL, widget::RESTART, widget::SCREEN] {
        assert!(drawn.iter().any(|d| d == id), "{id} drawn: {drawn:?}");
    }
    assert!(v.prototype().is_some());
    // A file without a flow starting point and no start node is a typed refusal.
    let err = PrototypePlay::open(&fig_fixture("fig-basic-shapes.fig"), None).err().unwrap();
    assert_eq!(err.code, "no_flow_starting_point");
}
