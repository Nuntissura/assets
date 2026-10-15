//! Lane SMB-HARNESS-PROTOTYPE-PLAY (feature `viewer`): the prototype play mode driven headlessly
//! (no window) over the Operator Figma prototype fixture: the flow starts on its starting point,
//! clicks are hit-tested to the label layers and drive the components player, transitions render
//! as an eased cross-fade, and every screen reached is compared with the Operator PNG of that
//! screen; the viewer draws the play panel's stable widget ids. Lane SMB-FIG-SMART-ANIMATE: the
//! smart-animate transition is a layer tween (the orb's interpolated geometry equals the hand values
//! and the mid frame matches a reference blend), overlays are placed by their frame records.
#![cfg(feature = "viewer")]

use hsk_studio_components::{PlayerEvent, SmartSide, match_layers, smart_animate_frame};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::prototype::{PrototypePlay, Screen, smart_frame, widget};
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
    // Smart animate (350 ms, ease in and out): at 175 ms (eased 0.5) the orb is tweened between its
    // two placements (lane SMB-FIG-SMART-ANIMATE), checked against the reference blend below.
    let (x, y) = label_centre(&play, "Click: smart animate");
    play.click(x, y).unwrap();
    play.advance(175);
    let mid = play.render().unwrap();
    let (t, raw, eased) = play.session().state().transition.unwrap();
    assert_eq!((name_of(&play, &t.to).as_str(), raw, t.match_layers), ("animated-screen", 0.5, true));
    assert!((eased - 0.5).abs() < 1e-9);
    let from = play.frame_raster(&t.from).unwrap();
    let to = play.frame_raster(&t.to).unwrap();
    smart_reference_check(&mid, &from, &to, eased);
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

/// The orb disc (centre x, centre y, radius) at eased progress `e`: from (24, 100) 70 x 70 to
/// (200, 150) 110 x 110 in the detail and animated screens of the fixture.
fn orb(e: f64) -> (f64, f64, f64) {
    let (x, y, size) = (24.0 + 176.0 * e, 100.0 + 50.0 * e, 70.0 + 40.0 * e);
    (x + size / 2.0, y + size / 2.0, size / 2.0)
}

fn px(s: &Screen, x: usize, y: usize) -> [u8; 3] {
    let i = (y * s.width as usize + x) * 3;
    [s.rgb[i], s.rgb[i + 1], s.rgb[i + 2]]
}

fn maxdiff(a: [u8; 3], b: [u8; 3]) -> u8 {
    (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap()
}

/// Reference blend of the smart-animate mid frame: inside the tweened orb disc the orb colour,
/// inside the end discs (outside the tweened one) the frame background, elsewhere the eased
/// cross-fade of the two end screens (the dissolving text layers). Pixels within 1.5 px of a disc
/// edge (anti-aliasing) are skipped. A plain cross-fade would show half-faded orbs at both ends and
/// none in between.
fn smart_reference_check(mid: &Screen, from: &Screen, to: &Screen, e: f64) {
    assert_eq!((mid.width, mid.height), (to.width, to.height));
    let colour = px(to, 255, 205);
    let background = px(to, 340, 100);
    let discs = [orb(0.0), orb(1.0), orb(e)];
    let (mut n, mut sum, mut within, mut orb_n, mut orb_max) = (0usize, 0.0f64, 0usize, 0usize, 0u8);
    for y in 0..mid.height as usize {
        for x in 0..mid.width as usize {
            let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
            let dist: Vec<f64> = discs.iter().map(|(cx, cy, r)| ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt() - r).collect();
            if dist.iter().any(|d| d.abs() <= 1.5) {
                continue;
            }
            let got = px(mid, x, y);
            let want = if dist[2] < 0.0 {
                orb_n += 1;
                orb_max = orb_max.max(maxdiff(got, colour));
                colour
            } else if dist[0] < 0.0 || dist[1] < 0.0 {
                background
            } else {
                let (a, b) = (px(from, x, y), px(to, x, y));
                [0, 1, 2].map(|c| (f64::from(a[c]) * (1.0 - e) + f64::from(b[c]) * e).round() as u8)
            };
            let d = maxdiff(got, want);
            n += 1;
            sum += f64::from(d);
            within += usize::from(d <= 8);
        }
    }
    let (mean, share) = (sum / n as f64, within as f64 / n as f64);
    eprintln!("smart mid frame vs reference blend: n {n} mean {mean:.6} <=8 {share:.6}; orb pixels {orb_n} max {orb_max}");
    eprintln!("smart orb colour {colour:?} background {background:?} at old centre {:?} new centre {:?} mid centre {:?}", px(mid, 59, 135), px(mid, 255, 205), px(mid, 157, 170));
    // Measured 2026-10-15: mean 0.096560/255, 99.7195 % within 8/255 (the differences are where the
    // dissolving title glyphs overlap: two half-opaque layers composite, a cross-fade averages),
    // 5924 orb pixels all exactly the orb colour.
    assert!(mean <= 0.0966 && share >= 0.99719, "reference blend {mean} {share}");
    assert_eq!((orb_n, orb_max), (5924, 0));
    // The orb left its start and has not reached its end; a cross-fade would show neither of these.
    assert_eq!((px(mid, 59, 135), px(mid, 255, 205), px(mid, 157, 170)), (background, background, colour));
    let fade = |x: usize, y: usize| {
        let (a, b) = (px(from, x, y), px(to, x, y));
        [0, 1, 2].map(|c| (f64::from(a[c]) * (1.0 - e) + f64::from(b[c]) * e).round() as u8)
    };
    assert_eq!(fade(157, 170), background);
    assert!(maxdiff(fade(59, 135), background) > 60 && maxdiff(fade(255, 205), background) > 60);
}

#[test]
fn smart_animate_pairs_the_fixture_layers_and_tweens_the_orb() {
    let play = PrototypePlay::open(&fig_fixture("fig-prototype-pages.fig"), None).unwrap();
    let doc = play.document();
    let src = |name: &str| doc.nodes.iter().find(|n| n.name.as_deref() == Some(name)).unwrap().id;
    let (a, b) = (smart_frame(doc, src("detail-screen")).unwrap(), smart_frame(doc, src("animated-screen")).unwrap());
    let m = match_layers(&a, &b).unwrap();
    let name = |f: &hsk_studio_components::SmartFrame, id: &str| f.layers.iter().find(|l| l.id == id).unwrap().name.clone();
    let pairs: Vec<(String, String)> = m.pairs.iter().map(|(x, y)| (name(&a, x), name(&b, y))).collect();
    eprintln!("pairs {pairs:?} from_only {:?} to_only {:?}", m.from_only, m.to_only);
    // The orb pairs by name; the text layers are explicitly named (the fixture stores autoRename
    // false on them, so no text-style matching) with different names: they dissolve.
    assert_eq!(pairs, [("animated-orb".to_owned(), "animated-orb".to_owned())]);
    let names = |f: &hsk_studio_components::SmartFrame, ids: &[String]| ids.iter().map(|i| name(f, i)).collect::<Vec<_>>();
    assert_eq!(names(&a, &m.from_only), ["Details", "Click: smart animate"]);
    assert_eq!(names(&b, &m.to_only), ["Animated", "Click: back to start"]);
    assert!(a.layers.iter().chain(&b.layers).all(|l| l.implicit_text_style.is_none()));
    // At eased 0.5 the orb is at the hand-computed midpoint: x 24 + 176/2, y 100 + 50/2, 90 x 90.
    let draw = smart_animate_frame(&a, &b, &m, 0.5).unwrap();
    let orb = draw.iter().find(|d| matches!(d.side, SmartSide::Matched { .. }) && name(&b, &d.id) == "animated-orb").unwrap();
    assert_eq!((orb.props.x, orb.props.y, orb.props.width, orb.props.height), (112.0, 125.0, 90.0, 90.0));
    assert_eq!(orb.props.opacity, 1.0);
}

#[test]
fn overlays_are_placed_by_their_frame_records_and_close_on_click_outside() {
    use hsk_studio_interop_fig::{Color, FigOutcome, Limits, NodePayload, PathCommand, Vec2, VersionPolicy, decode_document, import_fig};
    let bytes = fig_fixture("fig-prototype-pages.fig");
    let cancel = Default::default();
    let Ok(FigOutcome::Imported(import)) = import_fig(&bytes, &Limits::default(), &VersionPolicy::observed(), None, &cancel) else {
        panic!("imports")
    };
    let mut doc = decode_document(&import, &Limits::default(), &cancel).unwrap();
    // The animated screen becomes a 320 x 480 BOTTOM_CENTER overlay (all its children stay inside)
    // over a half-opaque black background that closes on an outside click; the detail label opens it.
    let id_of = |doc: &hsk_studio_interop_fig::FigDocument, name: &str| doc.nodes.iter().position(|n| n.name.as_deref() == Some(name)).unwrap();
    let (anim, label) = (id_of(&doc, "animated-screen"), id_of(&doc, "Click: smart animate"));
    doc.nodes[anim].size = Some(Vec2 { x: 320.0, y: 480.0 });
    // The frame's stored outline (fill geometry) follows the new size.
    for g in doc.nodes[anim].fill_geometry.iter_mut().filter_map(|g| g.commands.as_mut()) {
        for c in g.iter_mut() {
            if let PathCommand::MoveTo { x, y } | PathCommand::LineTo { x, y } = c {
                (*x, *y) = (*x * 320.0 / 360.0, *y * 480.0 / 520.0);
            }
        }
    }
    let NodePayload::Frame(f) = &mut doc.nodes[anim].payload else { panic!("frame") };
    f.overlay_position_type = Some("BOTTOM_CENTER".into());
    f.overlay_background = Some((Some("SOLID_COLOR".into()), Some(Color { r: 0.0, g: 0.0, b: 0.0, a: 0.5 })));
    f.overlay_background_interaction = Some("CLOSE_ON_CLICK_OUTSIDE".into());
    let a = &mut doc.nodes[label].interactions[0].actions[0];
    a.navigation = Some("OVERLAY".into());
    a.transition = None;
    let mut play = PrototypePlay::open_document(doc, None).unwrap();
    let (x, y) = label_centre(&play, "Click: navigate to details");
    play.click(x, y).unwrap();
    let screen = play.render().unwrap();
    let (x, y) = label_centre(&play, "Click: smart animate");
    play.click(x, y).unwrap();
    let state = play.session().state();
    assert_eq!(state.overlays.len(), 1);
    let overlay = play.frame_raster(&state.overlays[0]).unwrap();
    assert_eq!((overlay.width, overlay.height), (320, 480));
    let shown = play.render().unwrap();
    // BOTTOM_CENTER: top-left ((360 - 320) / 2, 520 - 480) = (20, 40).
    for (x, y) in [(0usize, 0usize), (25, 45), (275, 245), (339, 519), (100, 400)] {
        let i = (y * 360 + x) * 3;
        let inside = (20..340).contains(&x) && (40..520).contains(&y);
        let want: Vec<u8> = if inside {
            let j = ((y - 40) * 320 + (x - 20)) * 3;
            overlay.rgb[j..j + 3].to_vec()
        } else {
            screen.rgb[i..i + 3].iter().map(|v| (f64::from(*v) * 0.5).round() as u8).collect()
        };
        assert_eq!(&shown.rgb[i..i + 3], &want[..], "pixel ({x}, {y}) inside {inside}");
    }
    // An outside click closes it (STU-PRO-124) and reaches nothing else.
    play.take_events();
    assert_eq!(play.click(5.0, 5.0), None);
    assert_eq!(play.take_events(), [PlayerEvent::OverlayClosed(state.overlays[0].clone())]);
    assert!(play.session().state().overlays.is_empty());
    assert_eq!(play.render().unwrap(), screen);
}
