//! Feature `document`, CPU only (no Vulkan device): the GUI frame on a real layered .handshake. Typed scripted input
//! (never OS input), the Layers panel built with kumihimo-ui, commands as named undo steps, the merged tree file,
//! tree acceptance checks, hostile inputs and a fake pane sink for device loss. SMB-GUI-SPINE-1 items 2 to 8.
#![cfg(feature = "document")]
mod common;

use common::{RED_ONLY, Scratch, fixture};
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::{LayerRow, Viewer};
use hsk_studio_native_host::HostError;
use hsk_studio_native_host::cli::{self, Command, OPERATOR_DEADLINE};
use hsk_studio_native_host::event::{HostEvent, Key, Mods};
use hsk_studio_native_host::gui::{
    self, BODY_ID, DOCK_ID, GROUP_ID, GuiFrame, LIST_ID, Outcome, PaneSink, SCROLL_ID, STATUS_ID, TAB_ID, VIEWER_ID,
    row_id, view,
};
use hsk_studio_native_host::scene::Scene;
use hsk_studio_native_host::script::{self, MAX_STEPS, Step, chord, click};
use hsk_studio_vulkan::Refusal;
use kumihimo_ui::checks;
use kumihimo_ui::geom::{Rect, Scale};
use kumihimo_ui::text::{BoxText, Text};
use kumihimo_ui::tokens::{ColorToken, Theme};
use kumihimo_ui::tree::{Interaction, Paint, Panel};
use std::time::{Duration, Instant};

const CTRL: Mods = Mods { ctrl: true, shift: false, alt: false };
const CTRL_SHIFT: Mods = Mods { ctrl: true, shift: true, alt: false };

fn open(dir: &std::path::Path, w: u32, h: u32, scale: Option<u32>) -> GuiFrame {
    let mut f = GuiFrame::open(&fixture(dir), w, h, scale).unwrap_or_else(|e| panic!("gui open: {e}"));
    f.frame().expect("first frame");
    f
}
fn layer(f: &GuiFrame, name: &str) -> String {
    f.rows().iter().find(|r| r.name == name).unwrap_or_else(|| panic!("layer {name}")).layer_id.clone()
}
fn run(f: &mut GuiFrame, steps: &[Step]) {
    for s in steps {
        f.step(s).unwrap_or_else(|e| panic!("{s:?}: {e}"));
    }
    f.frame().unwrap();
}
fn click_on(f: &mut GuiFrame, author_id: &str) {
    let (x, y) = f.centre(author_id).unwrap_or_else(|| panic!("no node {author_id}"));
    run(f, &click(x, y));
}
/// The viewport's physical pixels (the document and its surround).
fn viewport_px(f: &GuiFrame) -> Vec<u8> {
    let c = f.canvas().unwrap();
    let r = f.scale().snap(f.regions().viewport);
    let mut out = Vec::new();
    for y in r.y0..r.y1 {
        for x in r.x0..r.x1 {
            let p = c.pixel(x as u32, y as u32);
            out.extend([p.r, p.g, p.b, p.a]);
        }
    }
    out
}
fn doc_px(f: &GuiFrame, (dx, dy): (u32, u32)) -> [u8; 4] {
    let img = f.image_rect().unwrap();
    let s = f.scale().get();
    let p = f.canvas().unwrap().pixel(((img.x0 + dx as f32 + 0.5) * s) as u32, ((img.y0 + dy as f32 + 0.5) * s) as u32);
    [p.r, p.g, p.b, p.a]
}

/// Item 2: the fixture is a real layered .handshake: reopened from disk it renders, lists its layers top-first, and the
/// hidden layer stays hidden. An open refusal carries the viewer's own code.
#[test]
fn layered_fixture_reopens_as_handshake_and_lists_layers_top_first() {
    let s = Scratch::new("fixture");
    let path = fixture(&s.0);
    assert_eq!(path.extension().and_then(|e| e.to_str()), Some("handshake"));
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&path).expect("reopen the .handshake");
    v.render_if_dirty();
    assert!(v.render_error().is_none(), "{:?}", v.render_error());
    let rows: Vec<(String, bool)> = v.layers().into_iter().map(|r| (r.name, r.visible)).collect();
    assert_eq!(rows, [("Sun".into(), false), ("Red".into(), true), ("Green".into(), true), ("Blue".into(), true)]);

    let f = open(&s.0, 800, 480, Some(1000));
    assert_eq!(f.rows().iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["Sun", "Red", "Green", "Blue"]);
    assert!(f.revision() > 0);
    assert_eq!(f.image_rect().map(|r| (r.w(), r.h())), Some((common::W as f32, common::H as f32)));
    assert_eq!(doc_px(&f, RED_ONLY), [220, 40, 40, 255], "Red is drawn 1:1 at scale 1");

    // Refusals keep their code: a missing file, and bytes that are no package (never the flat `open`).
    let missing = GuiFrame::open(&s.0.join("absent.handshake"), 800, 480, None).err();
    assert_eq!(missing, Some(HostError::Document("not_found")));
    std::fs::write(s.0.join("junk.handshake"), b"PK\x03\x04 not a package").unwrap();
    match GuiFrame::open(&s.0.join("junk.handshake"), 800, 480, None).err() {
        Some(HostError::Document(code)) => assert!(code != "open" && !code.is_empty(), "{code}"),
        other => panic!("{other:?}"),
    }
}

/// Item 3: typed input. Events stay Copy and Eq; a script maps to the typed queue; over the cap is refused.
#[test]
fn scripted_events_are_typed_and_bounded() {
    let e = HostEvent::Key { key: Key::Char('z'), pressed: true, mods: CTRL };
    let copy = e;
    assert_eq!(e, copy);
    let steps = script::parse("# comment\nclick 10 20\nkey ctrl+shift+z\nwheel -2\nleave\ninvoke studio.tab.layers\nresize 640 360\nscale 1250\n").unwrap();
    let mut want = click(10, 20).to_vec();
    want.extend(chord(Key::Char('z'), CTRL_SHIFT));
    want.push(Step::Event(HostEvent::Wheel { x: 0, y: -2000, pixels: false }));
    want.push(Step::Event(HostEvent::PointerLeft));
    want.push(Step::Invoke("studio.tab.layers".into()));
    want.push(Step::Event(HostEvent::Resized { width: 640, height: 360 }));
    want.push(Step::Event(HostEvent::ScaleFactor { milli: 1250 }));
    assert_eq!(steps, want);
    assert_eq!(script::parse("jump 1 2"), Err(HostError::Usage("script")));
    assert_eq!(script::parse("key hyper+q"), Err(HostError::Usage("script")));
    let long = "leave\n".repeat(MAX_STEPS + 1);
    assert_eq!(script::parse(&long), Err(HostError::Usage("script_too_long")));
    let mut steps = vec![Step::Invoke("studio.tab.layers".into()); MAX_STEPS];
    assert_eq!(script::extend(&mut steps, [Step::Invoke("x.y".into())]), Err(HostError::Usage("script_too_long")));

    // CLI: the gui scene, its flags, and the Operator window's long deadline.
    let Ok(Command::Offscreen(o)) = cli::parse(&["offscreen", "--scene", "gui", "--document", "d.handshake", "--invoke", "studio.tab.layers", "--tree", "t.json", "--scale", "1.25"]) else {
        panic!("gui offscreen parse")
    };
    assert_eq!((o.width, o.height, o.scene.clone()), (1280, 800, Scene::Gui("d.handshake".into())));
    assert_eq!((o.gui.steps.len(), o.gui.tree.is_some(), o.gui.scale_milli), (1, true, Some(1250)));
    for bad in [
        vec!["offscreen", "--scene", "gui"],
        vec!["offscreen", "--invoke", "studio.tab.layers"],
        vec!["offscreen", "--scene", "gui", "--document", "d", "--scale", "9"],
        vec!["offscreen", "--scene", "gui", "--document", "d", "--script", "no-such-script.txt"],
    ] {
        assert!(matches!(cli::parse(&bad), Err(HostError::Usage(_))), "{bad:?}");
    }
    let Ok(Command::Window(w)) = cli::parse(&["window", "--scene", "gui", "--document", "d"]) else { panic!("window") };
    assert_eq!((w.frames, w.deadline), (0, OPERATOR_DEADLINE));
}

/// Item 4: the frame and the Layers panel: regions cover the window, rows carry STU-MDL-100 ids, hidden and selected
/// states, the group header and the status readouts; tree checks at four scales; chrome pixels are tokens.
#[test]
fn layers_panel_regions_rows_states_and_tree_checks() {
    let s = Scratch::new("panel");
    let mut f = open(&s.0, 800, 480, None);
    f.set_device("Test GPU (discrete), vulkan 1.3".into());
    f.frame().unwrap();
    // AC-GEOM-01: the frame's regions partition the window (physical px).
    let r = f.regions();
    let sc = f.scale();
    let l = f.layout().clone();
    let rects: Vec<Rect> = l.regions().iter().map(|(_, q)| *q).collect();
    assert!(checks::partition(l.window, &rects, sc).is_empty(), "{:?}", l.regions());
    assert_eq!((r.dock.w(), r.status.h(), r.header.h()), (300.0, 24.0, 26.0));
    // The spine's three regions are roots of the frame tree, in paint order (SMB-GUI-FRAME-1 adds the other bands).
    let roots: Vec<String> = f.semantic().roots.iter().map(|n| n.author_id.clone()).collect();
    let at = |id: &str| roots.iter().position(|r| r == id).unwrap_or_else(|| panic!("{id} in {roots:?}"));
    assert!(at(VIEWER_ID) < at(DOCK_ID) && at(DOCK_ID) < at(STATUS_ID), "{roots:?}");

    let sun = layer(&f, "Sun");
    let red = layer(&f, "Red");
    assert!(sun.chars().any(|c| c.is_ascii_uppercase()) || sun.starts_with("SLYR"), "{sun}");
    let sun_row = f.node(&row_id(&sun)).expect("Sun row");
    assert_eq!(sun_row.value.as_deref(), Some("hidden"));
    assert_eq!(sun_row.label.as_deref(), Some("Sun"));
    assert!(row_id(&sun).chars().all(|c| !c.is_ascii_uppercase()));
    // Hidden: no eye glyph, name in text.disabled (never colour alone: the value says hidden).
    let vis = f.node(&format!("{}.visibility", row_id(&sun))).unwrap();
    assert_eq!(vis.value.as_deref(), Some("off"));
    let dock = f.panel_of(DOCK_ID).unwrap();
    let tree = dock.tree();
    let vis_id = tree.find(&format!("{}.visibility", row_id(&sun))).unwrap();
    assert!(matches!(tree.get(tree.get(vis_id).children[0]).paint, Paint::TintedIcon { icon, .. } if icon == view::eye(false)), "eye_off, not the eye");
    let row_node = tree.find(&row_id(&sun)).unwrap();
    let disabled_name = tree.get(row_node).children.iter().any(|&c| matches!(&tree.get(c).paint, Paint::Label { color: Some(ColorToken::TextDisabled), text, .. } if text == "Sun"));
    assert!(disabled_name);
    assert_eq!(f.node(&row_id(&red)).unwrap().value.as_deref(), Some("visible"));
    // Group header and tab; the list states its rows; status readouts (AC-BEHAV-07).
    assert_eq!(f.node(GROUP_ID).map(|n| n.role.name()), Some("TabList"));
    assert!(f.node(TAB_ID).unwrap().flags.selected);
    assert_eq!(f.node(LIST_ID).unwrap().value.as_deref(), Some("rows 1-4 of 4"));
    assert!(f.node(SCROLL_ID).is_none(), "content fits: no scrollbar");
    assert!(f.node(BODY_ID).is_some());
    let device = f.node("studio.status.device").unwrap().label.clone().unwrap();
    assert!(device.contains("Test GPU") && device.contains("render-cpu"), "{device}");
    assert!(f.node("studio.status.revision").unwrap().label.clone().unwrap().contains(&f.revision().to_string()));
    assert_eq!(f.node("studio.status.chrome").unwrap().label.as_deref(), Some("cpu painter, Vulkan UI pass pending"));
    // AC-GEOM-04: row pitch 32, row tops on the 4 px grid from the body top.
    let body_y = r.body.y0;
    for row in f.rows().to_vec() {
        let n = f.node(&row_id(&row.layer_id)).unwrap();
        assert_eq!(n.bounds.h(), 32.0);
        assert_eq!((n.bounds.y0 - body_y) % 4.0, 0.0);
    }
    // Selected state through the command.
    f.invoke(&row_id(&red)).unwrap();
    f.frame().unwrap();
    assert!(f.node(&row_id(&red)).unwrap().flags.selected);
    assert_eq!(f.selected(), Some(red.as_str()));

    // AC-COL-01: flat chrome pixels outside the Viewport are theme tokens (text, icons and thumbnails excluded).
    let tokens: Vec<[u8; 4]> = ColorToken::ALL.iter().map(|t| t.resolve(Theme::Dark)).map(|c| [c.r, c.g, c.b, c.a]).collect();
    let mut skip: Vec<Rect> = Vec::new();
    for p in f.panels() {
        for n in &p.tree().nodes {
            // Glyphs, icons, thumbnails and antialiased round shapes (chips, rounded controls, ruler labels) blend.
            let rounded = matches!(n.paint, Paint::Surface { radius, .. } if radius > 0.0);
            if rounded || matches!(n.paint, Paint::Label { .. } | Paint::Value { .. } | Paint::Icon { .. } | Paint::TintedIcon { .. } | Paint::Thumbnail(_) | Paint::Round { .. } | Paint::Ruler { .. }) {
                skip.push(n.bounds);
            }
        }
    }
    skip.push(f.node("studio.colour.proxy").unwrap().bounds);
    let c = f.canvas().unwrap();
    let vp = sc.snap(r.viewport);
    let mut off = 0;
    for y in 0..480u32 {
        for x in 0..800u32 {
            let (lx, ly) = (x as f32 + 0.5, y as f32 + 0.5);
            if ((x as i32) < vp.x1 && (y as i32) < vp.y1) || skip.iter().any(|q| q.contains(kumihimo_ui::geom::Point::new(lx, ly))) {
                continue;
            }
            let p = c.pixel(x, y);
            if !tokens.contains(&[p.r, p.g, p.b, p.a]) {
                off += 1;
            }
        }
    }
    assert_eq!(off, 0, "non-token chrome pixels");

    // AC-GEOM-02, AC-TEXT-01, AC-TEXT-03 and AC-GEOM-03 at the scale matrix, on a window that stays at or above the
    // frame's floor (size::WINDOW_MIN_W x WINDOW_MIN_H logical) at every scale.
    let mut f = open(&s.0, 2048, 1280, None);
    for milli in [1000u32, 1250, 1500, 2000] {
        f.apply(HostEvent::ScaleFactor { milli });
        f.frame().unwrap();
        let sc = Scale::new(milli as f32 / 1000.0).unwrap();
        assert_eq!(f.scale(), sc);
        for p in f.panels() {
            assert!(checks::overlapping_controls(p.tree()).is_empty(), "{milli}: {:?}", checks::overlapping_controls(p.tree()));
            assert!(checks::under_text_floor(p.tree()).is_empty(), "{milli}");
        }
        assert!(f.clipped().is_empty(), "{milli}: {:?}", f.clipped());
        let r = f.regions();
        assert!(checks::edge_gaps(&[r.viewport, r.dock], sc).is_empty(), "{milli}: viewport|dock");
        let t = |q: Rect| Rect::new(q.y0, q.x0, q.y1, q.x1);
        assert!(checks::edge_gaps(&[t(r.header), t(r.body)], sc).is_empty(), "{milli}: header|body");
        let header = f.node(GROUP_ID).unwrap().bounds;
        assert_eq!(sc.snap(header).y1, sc.snap(f.node(BODY_ID).unwrap().bounds).y0, "{milli}");
    }
}

/// Item 5: an eye click is one named undo step (`panel_set visible <layer_id>`, command layer.panel_set); the canvas
/// re-renders inside that layer's rect; undo restores the earlier frame, both redo chords re-apply, a refused redo is
/// a code in the status bar, never fatal.
#[test]
fn eye_click_is_one_named_undo_step_and_undo_redo_restore() {
    let s = Scratch::new("eye");
    let mut f = open(&s.0, 800, 480, Some(1000));
    let red = layer(&f, "Red");
    let rev0 = f.revision();
    let before = viewport_px(&f);
    let doc_before = f.viewer().frame().map(|fr| fr.rgba8.clone()).unwrap();
    click_on(&mut f, &format!("{}.visibility", row_id(&red)));
    assert_eq!(f.last(), Some(&Outcome { command: "layer.set_visible", code: "ok".into() }));
    assert_eq!(f.revision(), rev0 + 1);
    assert_eq!(f.node(&row_id(&red)).unwrap().value.as_deref(), Some("hidden"));
    assert_ne!(doc_px(&f, RED_ONLY), [220, 40, 40, 255], "the canvas re-rendered inside Red's rect");
    let h = f.viewer().run().history();
    assert_eq!(h["undo"].as_array().unwrap().last().unwrap().as_str(), Some(format!("panel_set visible {red}").as_str()));
    assert_eq!(h["undo_steps"].as_array().unwrap().last().unwrap()["command_id"].as_str(), Some("layer.panel_set"));
    assert_eq!(h["undo"].as_array().unwrap().len(), 1, "one undo step");

    run(&mut f, &chord(Key::Char('z'), CTRL));
    assert_eq!(f.last().map(|o| (o.command, o.code.as_str())), Some(("history.undo", "ok")));
    assert_eq!(viewport_px(&f), before, "undo restores the earlier frame");
    assert_eq!(f.viewer().frame().map(|fr| fr.rgba8.clone()).unwrap(), doc_before);
    run(&mut f, &chord(Key::Char('z'), CTRL_SHIFT));
    assert_eq!(f.last().map(|o| (o.command, o.code.as_str())), Some(("history.redo", "ok")));
    assert_eq!(f.node(&row_id(&red)).unwrap().value.as_deref(), Some("hidden"));
    run(&mut f, &chord(Key::Char('z'), CTRL));
    run(&mut f, &chord(Key::Char('y'), CTRL));
    assert_eq!(f.last().map(|o| (o.command, o.code.as_str())), Some(("history.redo", "ok")));
    run(&mut f, &chord(Key::Char('y'), CTRL));
    let last = f.last().cloned().unwrap();
    assert_eq!(last.command, "history.redo");
    assert_ne!(last.code, "ok", "nothing to redo is a refusal");
    let readout = f.node("studio.status.command").unwrap().label.clone().unwrap();
    assert!(readout.contains(&last.code), "{readout}");
    assert!(f.stats().refused >= 1 && f.stats().commands >= 6);
}

/// AC-BEHAV-05 (spine gate test 8): select, deselect, reselect gives a byte-identical tree.
#[test]
fn select_deselect_reselect_tree_is_byte_identical() {
    let s = Scratch::new("reselect");
    let mut f = open(&s.0, 800, 480, Some(1000));
    let green = layer(&f, "Green");
    click_on(&mut f, &row_id(&green));
    assert_eq!(f.selected(), Some(green.as_str()));
    let selected = f.tree_json();
    let list = f.node(LIST_ID).unwrap().bounds;
    run(&mut f, &click(((list.x0 + list.x1) / 2.0) as i32, (list.y1 - 8.0) as i32));
    assert_eq!(f.last().map(|o| o.command), Some("viewer.select"));
    assert_eq!(f.selected(), None, "blank list space deselects");
    assert_ne!(f.tree_json(), selected);
    click_on(&mut f, &row_id(&green));
    assert_eq!(f.tree_json(), selected, "reselect is byte-identical");
    assert!(selected.starts_with(r#"{"schema":"kumihimo.ui.tree@1""#));
}

/// STU-MDL-109: invoke by author_id answers from the same table; an unknown id is a typed refusal.
#[test]
fn invoke_by_author_id_and_unknown_ids_are_refused() {
    let s = Scratch::new("invoke");
    let mut f = open(&s.0, 800, 480, Some(1000));
    let blue = layer(&f, "Blue");
    let rev = f.revision();
    assert_eq!(f.invoke(&row_id(&blue)).unwrap(), Some(Outcome { command: "viewer.select", code: "ok".into() }));
    assert_eq!(f.selected(), Some(blue.as_str()));
    assert_eq!(f.invoke(&format!("{}.visibility", row_id(&blue))).unwrap().map(|o| o.command), Some("layer.set_visible"));
    assert_eq!(f.revision(), rev + 1);
    assert_eq!(f.invoke(LIST_ID).unwrap().map(|o| o.command), Some("viewer.select"));
    assert_eq!(f.selected(), None);
    assert_eq!(f.invoke(TAB_ID), Ok(None), "the active tab is inert");
    assert_eq!(f.invoke("studio.panel.layers.row.no-such-layer"), Err("unknown_layer"));
    assert_eq!(f.invoke("studio.bogus.control"), Err("unknown_author_id"));
    assert_eq!(f.step(&Step::Invoke("studio.bogus.control".into())), Err(HostError::Usage("unknown_author_id")));
    // A pointer outside the client area hits nothing.
    let n = f.stats().commands;
    run(&mut f, &click(-40, -40));
    run(&mut f, &click(5000, 20));
    assert_eq!(f.stats().commands, n);
}

/// Item 6 and hostile (c): idle frames rebuild and upload nothing; 0x0 paints nothing and the restore shows the same
/// frame; a resize and a scale change relayout; a scale outside 0.5..=8 is clamped with a code.
#[test]
fn idle_minimise_resize_and_scale() {
    #[derive(Default)]
    struct Sink(u32);
    impl PaneSink for Sink {
        fn upload(&mut self, rgba8: &[u8]) -> Result<(), Refusal> {
            assert!(!rgba8.is_empty());
            self.0 += 1;
            Ok(())
        }
    }
    let s = Scratch::new("idle");
    let mut f = open(&s.0, 800, 480, None);
    let mut sink = Sink::default();
    assert!(f.upload(&mut sink).unwrap());
    let (builds, paints) = (f.stats().builds, f.stats().paints);
    for _ in 0..10 {
        assert!(!f.frame().unwrap(), "idle frame");
        assert!(!f.upload(&mut sink).unwrap(), "idle frame uploads nothing");
    }
    assert_eq!((f.stats().builds, f.stats().paints, sink.0), (builds, paints, 1));
    assert!(f.stats().idle_frames >= 10);
    let shown = f.rgba().unwrap().to_vec();

    f.apply(HostEvent::Resized { width: 0, height: 0 });
    assert!(!f.frame().unwrap(), "minimised: no layout, no paint");
    assert_eq!(f.stats().paints, paints);
    f.apply(HostEvent::Resized { width: 800, height: 480 });
    f.frame().unwrap();
    assert_eq!(f.rgba().unwrap(), &shown[..], "restore shows the frame from before the minimise");

    f.apply(HostEvent::Resized { width: 1000, height: 600 });
    assert!(f.frame().unwrap());
    assert_eq!(f.extent(), (1000, 600));
    assert_eq!((f.canvas().unwrap().width(), f.canvas().unwrap().height()), (1000, 600));
    // At 1000 x 600 x 1.5 the centre would be under 360, so the dock rule is checked on a frame with room.
    f.apply(HostEvent::Resized { width: 1500, height: 900 });
    f.apply(HostEvent::ScaleFactor { milli: 1500 });
    assert!(f.frame().unwrap());
    assert_eq!(f.scale().snap(f.regions().dock).w(), 450);
    f.apply(HostEvent::ScaleFactor { milli: 100 });
    f.frame().unwrap();
    assert_eq!(f.scale().get(), 0.5);
    assert!(f.notes().contains(&"scale_clamped"));
    f.apply(HostEvent::ScaleFactor { milli: 90_000 });
    f.frame().unwrap();
    assert_eq!(f.scale().get(), 8.0);
}

/// STU-SHL-105: the wheel over the list scrolls it; the passive scrollbar shows when the list overflows; the wheel
/// over the Viewport does not scroll the list.
#[test]
fn wheel_scrolls_the_list_and_the_scrollbar_shows() {
    let s = Scratch::new("wheel");
    // 324 high: the Properties and Colour groups collapse to their headers and the Layers body is 100 px (FIT-H2).
    let mut f = open(&s.0, 800, 324, Some(1000));
    assert_eq!(f.regions().body.h(), 100.0);
    assert_eq!(f.node(LIST_ID).unwrap().value.as_deref(), Some("rows 1-4 of 4"));
    assert!(f.node(SCROLL_ID).is_some(), "100 px body, 4 rows of 32: overflow");
    let (x, y) = f.centre(VIEWER_ID).unwrap();
    run(&mut f, &[Step::Event(HostEvent::PointerMoved { x, y }), Step::Event(HostEvent::Wheel { x: 0, y: -1000, pixels: false })]);
    assert_eq!(f.node(LIST_ID).unwrap().value.as_deref(), Some("rows 1-4 of 4"), "wheel over the Viewport");
    let (x, y) = f.centre(LIST_ID).unwrap();
    run(&mut f, &[Step::Event(HostEvent::PointerMoved { x, y }), Step::Event(HostEvent::Wheel { x: 0, y: -1000, pixels: false })]);
    assert_eq!(f.node(LIST_ID).unwrap().value.as_deref(), Some("rows 2-4 of 4"));
    run(&mut f, &[Step::Event(HostEvent::Wheel { x: 0, y: 3000, pixels: false })]);
    assert_eq!(f.node(LIST_ID).unwrap().value.as_deref(), Some("rows 1-4 of 4"));
    // Review F4: deltas under a row add up. A quarter notch is 0.75 row: no scroll yet; the second makes 1.5 rows.
    let quarter = Step::Event(HostEvent::Wheel { x: 0, y: -250, pixels: false });
    run(&mut f, std::slice::from_ref(&quarter));
    assert_eq!(f.node(LIST_ID).unwrap().value.as_deref(), Some("rows 1-4 of 4"), "0.75 row does not scroll");
    run(&mut f, std::slice::from_ref(&quarter));
    assert_eq!(f.node(LIST_ID).unwrap().value.as_deref(), Some("rows 2-4 of 4"), "1.5 rows scroll one");
    let up16 = Step::Event(HostEvent::Wheel { x: 0, y: 16, pixels: true });
    run(&mut f, std::slice::from_ref(&up16));
    assert_eq!(f.node(LIST_ID).unwrap().value.as_deref(), Some("rows 2-4 of 4"), "16 px up cancels the half row down");
    run(&mut f, std::slice::from_ref(&up16));
    assert_eq!(f.node(LIST_ID).unwrap().value.as_deref(), Some("rows 2-4 of 4"), "half a row: no scroll");
    run(&mut f, std::slice::from_ref(&up16));
    assert_eq!(f.node(LIST_ID).unwrap().value.as_deref(), Some("rows 1-4 of 4"), "two 16 px deltas are one row");
}

/// Review F5: at scale 1.2 a row whose top edge has a physical fraction under 0.49 snaps down (for example 146.4 to
/// 146); a click on that first physical pixel selects that row, and the pixel above selects the row above. The rows
/// sit where the frame puts the Layers body, so the test finds such a row instead of pinning its position.
#[test]
fn boundary_pixel_at_scale_1_2_clicks_the_row_it_shows() {
    let s = Scratch::new("edge");
    // 1200 x 720 at 1.2 is 1000 x 600 logical: the Inspect Dock keeps its groups (at 800 x 480 FIT-W5 collapses it).
    let mut f = open(&s.0, 1200, 720, Some(1200));
    let sc = f.scale();
    let names = ["Sun", "Red", "Green", "Blue"];
    let ids: Vec<String> = names.iter().map(|n| layer(&f, n)).collect();
    let k = (1..ids.len())
        .find(|&i| {
            let v = f.node(&row_id(&ids[i])).unwrap().bounds.y0 * sc.get();
            let frac = v - v.floor();
            frac > 0.01 && frac < 0.49
        })
        .expect("a row edge that snaps down");
    let b = f.node(&row_id(&ids[k])).unwrap().bounds;
    let (x, y) = (sc.edge((b.x0 + b.x1) / 2.0), sc.edge(b.y0));
    assert_eq!(y as f32, (b.y0 * sc.get()).floor(), "the painter's first physical row of {}", names[k]);
    run(&mut f, &click(x, y));
    assert_eq!(f.selected(), Some(ids[k].as_str()));
    run(&mut f, &click(x, y - 1));
    assert_eq!(f.selected(), Some(ids[k - 1].as_str()));
}

/// Hostile (a): 10,000 plain rows build only the rows that fit, shape no row off screen, finish in a stated time.
/// A 4100-layer PSD stops in the PSD importer (`psd_pixel_limit`) and the host shows that exact code; it never reaches
/// Folio's node budget (Budget.nodes 4096), so Folio's own BudgetExceeded refusal is NOT_PROVEN here.
#[test]
fn ten_thousand_rows_build_bounded_and_over_budget_is_refused() {
    let rows: Vec<LayerRow> = (0..10_000)
        .map(|i| LayerRow { layer_id: format!("SLYR-{i:08}"), name: format!("Layer {i}"), kind: "raster".into(), depth: 0, visible: true, opacity: 1.0 })
        .collect();
    let mut text = Text::new(Box::new(BoxText));
    let ui = Interaction::default();
    let mut p = Panel::default();
    let t0 = Instant::now();
    p.update(1, &ui, Rect::xywh(0.0, 0.0, 300.0, 600.0), Scale::ONE, Theme::Dark, &mut text, |b| view::layers_dock(b, &rows, None, 5000, 574.0)).unwrap();
    let took = t0.elapsed();
    assert!(took < Duration::from_secs(2), "stated bound 2 s, took {took:?}");
    let fit = view::rows_fit(574.0);
    assert_eq!(fit, 18);
    assert!(p.tree().len() < 30 * fit + 40, "nodes built {}", p.tree().len());
    assert!(text.stats().shape_misses < 3 * fit as u64 + 10, "{:?}", text.stats());
    let list = p.tree().get(p.tree().find(LIST_ID).unwrap());
    assert_eq!(list.value.as_deref(), Some("rows 5001-5018 of 10000"));
    assert!(p.tree().find(&row_id("SLYR-00005000")).is_some() && p.tree().find(&row_id("SLYR-00004999")).is_none());
    assert!(p.tree().find(SCROLL_ID).is_some());

    let s = Scratch::new("budget");
    let many: Vec<common::Layer> =
        (0..4100).map(|i| common::Layer { name: format!("L{i}"), rect: [0, 0, 1, 1], rgb: [i as u8, 0, 0], hidden: false }).collect();
    let path = s.0.join("many.psd");
    std::fs::write(&path, common::psd(4, 4, &many)).unwrap();
    assert_eq!(GuiFrame::open(&path, 800, 480, None).err(), Some(HostError::Document("psd_pixel_limit")));
}

/// Hostile (b): a 4096-byte name ellipsizes in its row, AC-TEXT-01 stays clean, the accessible label carries the full
/// name (AC-TEXT-02) and shaping grows by a bounded count (F9); a 1 MiB name cuts at 4096 bytes.
#[test]
fn long_layer_names_ellipsize_and_cut() {
    let s = Scratch::new("names");
    let mut f = open(&s.0, 800, 480, Some(1000));
    let red = layer(&f, "Red");
    let long = "ab".repeat(2048);
    let misses = f.text_stats().shape_misses;
    f.edit(|v| {
        v.rename_layer(&red, &long).expect("rename");
    });
    f.frame().unwrap();
    assert_eq!(f.node(&row_id(&red)).unwrap().label.as_deref(), Some(long.as_str()), "AC-TEXT-02");
    assert!(f.clipped().is_empty(), "AC-TEXT-01: {:?}", f.clipped());
    assert!(f.text_stats().shape_misses - misses < 64, "{:?}", f.text_stats());

    let huge = LayerRow { layer_id: "SLYR-1".into(), name: "x".repeat(1 << 20), kind: "raster".into(), depth: 0, visible: true, opacity: 1.0 };
    let mut text = Text::new(Box::new(BoxText));
    let mut p = Panel::default();
    p.update(1, &Interaction::default(), Rect::xywh(0.0, 0.0, 300.0, 400.0), Scale::ONE, Theme::Dark, &mut text, |b| view::layers_dock(b, std::slice::from_ref(&huge), None, 0, 374.0)).unwrap();
    let n = p.tree().get(p.tree().find(&row_id("SLYR-1")).unwrap());
    assert_eq!(n.label.as_ref().map(String::len), Some(4096));
}

/// Hostile (d): a pane sink that loses the device ends the drive with exit 3, no Viewer command after it, no retry.
#[test]
fn device_lost_on_upload_is_exit_3_and_stops_commands() {
    struct Lost {
        calls: u32,
        fail_at: u32,
    }
    impl PaneSink for Lost {
        fn upload(&mut self, _: &[u8]) -> Result<(), Refusal> {
            self.calls += 1;
            if self.calls >= self.fail_at { Err(Refusal::DeviceLost) } else { Ok(()) }
        }
    }
    let s = Scratch::new("lost");
    let mut f = open(&s.0, 800, 480, Some(1000));
    let (red, green) = (layer(&f, "Red"), layer(&f, "Green"));
    let steps = [Step::Invoke(row_id(&red)), Step::Invoke(format!("{}.visibility", row_id(&green))), Step::Invoke(row_id(&green))];
    let mut sink = Lost { calls: 0, fail_at: 2 };
    let err = gui::drive(&mut f, &mut sink, &steps).unwrap_err();
    assert_eq!(err, HostError::Vulkan(Refusal::DeviceLost));
    assert_eq!(err.exit_code(), 3);
    assert_eq!(sink.calls, 2, "no retry");
    assert_eq!(f.stats().commands, 1, "no Viewer command after the loss");
}

/// Item 7: the tree file is one kumihimo.ui.tree@1 document with the revision; the receipt gains a gui member.
#[test]
fn tree_file_and_receipt_gui_member() {
    let s = Scratch::new("tree");
    let mut f = open(&s.0, 800, 480, Some(1000));
    let red = layer(&f, "Red");
    f.invoke(&format!("{}.visibility", row_id(&red))).unwrap();
    f.frame().unwrap();
    let file = f.tree_file();
    let head = format!(r#"{{"schema":"hsk.studio.native_host.tree@1","revision":{},"tree":{{"schema":"kumihimo.ui.tree@1""#, f.revision());
    assert!(file.starts_with(&head), "{}", &file[..file.len().min(200)]);
    assert!(file.contains(&format!(r#""author_id":"{}""#, row_id(&red))));
    let g = f.receipt();
    let num = |k: &str| g.get(k).map(|v| v.render());
    assert_eq!(num("revision"), Some(f.revision().to_string()));
    assert_eq!((num("rows_shown"), num("rows_total")), (Some("4".into()), Some("4".into())));
    assert!(g.get("builds").is_some() && g.get("uploads").is_some() && g.get("paints").is_some());
    assert_eq!(g.get("last_command").map(|v| v.render()), Some(r#"{"command":"layer.set_visible","code":"ok"}"#.into()));
    assert_eq!(g.get("text").map(|v| v.render()), Some(format!(r#""{}""#, f.text_kind())));
}

/// The host never takes focus or synthesises input: no such API in its source (HBR-QUIET-002).
#[test]
fn native_host_source_calls_no_focus_or_input_api() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut stack = vec![src];
    let mut files = 0;
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            files += 1;
            let text = std::fs::read_to_string(&p).unwrap();
            for word in ["focus_window", "set_cursor_grab", "set_cursor_position", "SetForegroundWindow", "SetFocus", "SendInput"] {
                let hit = text.match_indices(word).any(|(i, _)| {
                    let after = text[i + word.len()..].chars().next();
                    !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                });
                assert!(!hit, "{} uses {word}", p.display());
            }
        }
    }
    assert!(files >= 10);
}
