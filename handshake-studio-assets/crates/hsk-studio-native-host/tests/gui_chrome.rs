//! Feature `document`, CPU only: the static raster frame of SMB-GUI-FRAME-1 items 6 to 9 on a real document. One
//! panel per region, the 19 menu titles, the rail, the docks, the rulers and the status bar; every control answers
//! (a rail pick, a group tab, or `not_implemented_yet` from a disabled control); studio.dock.state against fit_table;
//! the tree checks at the scale matrix; chrome pixels are tokens; hostile names, scales and storms.
#![cfg(feature = "document")]
mod common;

use common::{Scratch, fixture};
use hsk_studio_native_host::event::HostEvent;
use hsk_studio_native_host::gui::{GuiFrame, Outcome, VIEWER_ID, preset, row_id};
use hsk_studio_native_host::script::{MAX_STEPS, Step, click};
use kumihimo_ui::checks;
use kumihimo_ui::geom::{Point, Rect, Scale};
use kumihimo_ui::icons;
use kumihimo_ui::semantic::SemanticNode;
use kumihimo_ui::text::MAX_LABEL_BYTES;
use kumihimo_ui::tokens::{ColorToken, Theme};
use kumihimo_ui::tree::{Paint, Role};

const NIY: &str = "not_implemented_yet";

fn open(dir: &std::path::Path, w: u32, h: u32, scale: u32) -> GuiFrame {
    let mut f = GuiFrame::open(&fixture(dir), w, h, Some(scale)).unwrap_or_else(|e| panic!("gui open: {e}"));
    f.frame().expect("first frame");
    f
}
fn run(f: &mut GuiFrame, steps: &[Step]) {
    for s in steps {
        f.step(s).unwrap_or_else(|e| panic!("{s:?}: {e}"));
    }
    f.frame().unwrap();
}
fn click_on(f: &mut GuiFrame, id: &str) {
    let (x, y) = f.centre(id).unwrap_or_else(|| panic!("no node {id}"));
    run(f, &click(x, y));
}
fn walk<'a>(ns: &'a [SemanticNode], out: &mut Vec<&'a SemanticNode>) {
    for n in ns {
        out.push(n);
        walk(&n.children, out);
    }
}
fn all_nodes(f: &GuiFrame) -> Vec<SemanticNode> {
    let s = f.semantic();
    let mut v = Vec::new();
    walk(&s.roots, &mut v);
    v.into_iter().cloned().collect()
}
fn layer(f: &GuiFrame, name: &str) -> String {
    f.rows().iter().find(|r| r.name == name).unwrap().layer_id.clone()
}
/// The numbers of the JSON array after `"key":[` in `s`.
fn nums(s: &str, key: &str) -> Vec<f32> {
    let at = s.find(&format!("\"{key}\":[")).unwrap_or_else(|| panic!("{key} in {s}")) + key.len() + 4;
    let end = at + s[at..].find(']').unwrap();
    s[at..end].split(',').filter(|v| !v.is_empty()).map(|v| v.parse().unwrap()).collect()
}

/// Item 6: one panel per region in paint order, the semantic roots, ids unique across panels, the Viewport id of
/// STU-SHL-066, rulers, both icon rails, three groups, the status bar.
#[test]
fn frame_regions_panels_and_ids() {
    let s = Scratch::new("chrome-ids");
    let f = open(&s.0, 1920, 1080, 1000);
    assert_eq!(VIEWER_ID, "studio.viewer.canvas");
    let roots: Vec<String> = f.semantic().roots.iter().map(|n| n.author_id.clone()).collect();
    assert_eq!(
        roots,
        [
            "shell.menu-bar",
            "studio.tab-strip",
            "studio.options",
            "studio.panel.tools",
            "studio.dock.left",
            "studio.viewer.canvas",
            "studio.dock.right",
            "studio.dock.bottom",
            "studio.status",
            "studio.dock.state",
            "studio.tool.active"
        ]
    );
    let nodes = all_nodes(&f);
    let mut ids: Vec<&str> = nodes.iter().map(|n| n.author_id.as_str()).collect();
    let n = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), n, "author_ids unique across panels");
    let node = |id: &str| f.node(id).unwrap_or_else(|| panic!("no {id}"));
    assert_eq!(node(VIEWER_ID).role, Role::Canvas);
    assert!(node("studio.viewer.ruler.x").bounds.h() == 20.0 && node("studio.viewer.ruler.y").bounds.w() == 20.0);
    // The menu bar: the module switch, 19 titles in STU-SHL-011 order, the window title, the presence chip.
    let titles: Vec<String> = nodes.iter().filter(|n| n.role == Role::MenuItem).map(|n| n.label.clone().unwrap()).collect();
    assert_eq!(titles, preset::MENU);
    assert!(node("shell.window-title").label.as_deref().unwrap().ends_with(": Studio"));
    assert_eq!(node("studio.presence.operator").label.as_deref(), Some("Operator"));
    // Tool Rail: 19 slots in presets.raster.rail order, kebab ids (STU-MDL-103), the foot.
    let slots: Vec<String> = nodes.iter().filter(|n| n.author_id.starts_with("studio.toolgroup.")).map(|n| n.author_id.clone()).collect();
    let want: Vec<String> = preset::RAIL.iter().map(|g| format!("studio.toolgroup.{}", g.group.replace('_', "-"))).collect();
    assert_eq!(slots, want);
    assert!(slots.contains(&"studio.toolgroup.smart-select".to_owned()));
    for id in ["studio.tool.family-browse", "studio.colour.proxy", "studio.rail.left.expand", "studio.rail.bottom.expand", "studio.tab.document", "studio.search", "studio.layout-preset.dropdown"] {
        node(id);
    }
    for p in preset::LEFT {
        node(&format!("studio.rail.left.{}", p.id));
    }
    for p in preset::BOTTOM {
        node(&format!("studio.rail.bottom.{}", p.id));
    }
    // Inspect Dock: three groups, tabs in STU-SHL-085 order, the first tab active, empty states naming the reason.
    for g in preset::RIGHT {
        let gid = format!("studio.group.{}", g.tabs[0].0);
        assert_eq!(node(&gid).role, Role::TabList, "{gid}");
        assert!(node(&format!("studio.tab.{}", g.tabs[0].0)).flags.selected);
    }
    let empty = node("studio.panel.properties.empty");
    assert_eq!(empty.value.as_deref(), Some("Not built in this host yet (not_implemented_yet)"));
    assert_eq!(f.tool_active(), "select", "the select slot at open");
    assert_eq!(node("studio.tool.active").value.as_deref(), Some("select"));
    assert_eq!(node("studio.options.identity").label.as_deref(), Some("Select"));
    // Hidden layer: eye_off from the icon set, name in text.disabled, value hidden (AC-COL-06).
    let sun = row_id(&layer(&f, "Sun"));
    let dock = f.panel_of(&sun).unwrap();
    let t = dock.tree();
    let vis = t.find(&format!("{sun}.visibility")).unwrap();
    assert!(matches!(t.get(t.get(vis).children[0]).paint, Paint::Icon { icon, .. } if Some(icon) == icons::ui("eye_off")));
}

/// Every interactive author_id maps to one command table entry; every unbuilt control answers not_implemented_yet in
/// the status bar and the receipt, by pointer and by invoke, with no Viewer call, no undo step and no focus move.
#[test]
fn every_control_answers_and_static_ones_say_not_implemented_yet() {
    let s = Scratch::new("chrome-static");
    let mut f = open(&s.0, 1920, 1080, 1000);
    let ops = f.viewer().run().ops().len();
    let rev = f.revision();
    let interactive: Vec<(String, bool)> = all_nodes(&f).iter().filter(|n| !n.actions.is_empty()).map(|n| (n.author_id.clone(), n.disabled)).collect();
    assert!(interactive.len() > 60, "{}", interactive.len());
    let mut statics = 0u64;
    for (id, disabled) in &interactive {
        let before = f.node(id).and_then(|n| n.value.clone());
        let got = f.invoke(id).unwrap_or_else(|e| panic!("{id}: {e}"));
        if *disabled {
            statics += 1;
            assert_eq!(got, Some(Outcome { command: "frame.static", code: NIY.into() }), "{id}");
            assert!(before.as_deref().is_some_and(|v| v.starts_with(NIY)), "{id}: the value states the reason first");
        }
    }
    assert!(statics > 40, "{statics}");
    assert_eq!(f.statics(), statics);
    // Undo the layer commands the sweep made, so only the static ones are left to compare.
    let viewer_ops = f.viewer().run().ops().len();
    assert!(viewer_ops >= ops);
    let mut g = open(&s.0, 1920, 1080, 1000);
    let ops0 = g.viewer().run().ops().len();
    for id in ["menu.file", "menu.operator", "shell.module-switch", "studio.tab.document.close", "studio.search", "studio.colour.proxy", "studio.rail.left.brushes", "studio.group.layers.menu"] {
        click_on(&mut g, id);
        assert_eq!(g.last(), Some(&Outcome { command: "frame.static", code: NIY.into() }), "{id}");
        assert!(!g.node(id).unwrap().flags.focused, "{id}: no focus move");
    }
    assert_eq!(g.viewer().run().ops().len(), ops0, "static controls never reach the Viewer");
    assert_eq!(g.revision(), rev);
    let readout = g.node("studio.status.command").unwrap().label.clone().unwrap();
    assert!(readout.contains(NIY), "{readout}");
    let receipt = g.receipt().render();
    assert!(receipt.contains(r#""static_outcomes":8"#), "{receipt}");
    assert!(receipt.contains(r#""last_command":{"command":"frame.static","code":"not_implemented_yet"}"#), "{receipt}");
    assert_eq!(g.invoke("studio.bogus.control"), Err("unknown_author_id"));
}

/// Item 7: a rail pick by click and by invoke give the same studio.tool.active; pick, pick another, pick back is a
/// byte-identical tree (AC-BEHAV-05); no undo step, no Viewer call; a group tab switches its group.
#[test]
fn rail_pick_and_group_tab_behaviour() {
    let s = Scratch::new("chrome-pick");
    let mut f = open(&s.0, 1920, 1080, 1000);
    let undo = f.viewer().run().history()["undo"].as_array().unwrap().len();
    let ops = f.viewer().run().ops().len();
    click_on(&mut f, "studio.toolgroup.paint");
    assert_eq!(f.last(), Some(&Outcome { command: "frame.tool_pick", code: "ok".into() }));
    assert_eq!(f.tool_active(), "paint_brush");
    assert_eq!(f.node("studio.options.identity").unwrap().label.as_deref(), Some("Paint Brush"));
    assert!(f.node("studio.toolgroup.paint").unwrap().flags.toggled && !f.node("studio.toolgroup.select").unwrap().flags.toggled);
    let picked = f.tree_json();
    let mut g = open(&s.0, 1920, 1080, 1000);
    g.invoke("studio.toolgroup.paint").unwrap();
    g.frame().unwrap();
    assert_eq!(g.tool_active(), "paint_brush", "click and invoke take the same path");
    click_on(&mut f, "studio.toolgroup.crop");
    assert_eq!(f.tool_active(), "crop");
    assert_ne!(f.tree_json(), picked);
    click_on(&mut f, "studio.toolgroup.paint");
    assert_eq!(f.tree_json(), picked, "pick, pick another, pick back is byte-identical");
    // Group tabs: Masks (behind +N at 300 wide, so by invoke) becomes the Properties group's active tab and is shown;
    // the Layers group keeps Layers.
    f.invoke("studio.tab.masks").unwrap();
    f.frame().unwrap();
    assert_eq!(f.last(), Some(&Outcome { command: "frame.tab_select", code: "ok".into() }));
    assert!(f.node("studio.tab.masks").unwrap().flags.selected && !f.node("studio.tab.properties").unwrap().flags.selected);
    assert!(!f.node("studio.tab.masks").unwrap().bounds.is_empty(), "the active tab is always shown");
    assert!(f.node("studio.panel.masks.empty").is_some());
    assert_eq!(f.active_tab("studio.group.properties"), Some("masks"));
    assert!(f.dock_state().contains(r#""active":"masks""#), "{}", f.dock_state());
    assert_eq!(f.invoke("studio.tab.layers"), Ok(None), "the active tab is inert");
    assert_eq!(f.viewer().run().history()["undo"].as_array().unwrap().len(), undo, "frame picks are never undo steps");
    assert_eq!(f.viewer().run().ops().len(), ops);
    // The spine's Layers behaviour on the new layout: the eye is one named undo step.
    let red = layer(&f, "Red");
    click_on(&mut f, &format!("{}.visibility", row_id(&red)));
    assert_eq!(f.last(), Some(&Outcome { command: "layer.set_visible", code: "ok".into() }));
}

/// AC-GEOM-06: studio.dock.state carries fit_table's keys; at the four matrix windows its sizes equal the raster rows
/// within 1 px.
#[test]
fn dock_state_equals_fit_table_at_the_matrix_windows() {
    let s = Scratch::new("chrome-dock");
    for ((w, h), milli, centre, groups, pitch, overflow, applied) in [
        ((1280, 720), 1000, [904.0, 574.0], [172.0, 138.0, 264.0], 28, 2, r#"["FIT-R1","FIT-R2"]"#),
        ((1920, 1080), 1000, [1544.0, 934.0], [280.0, 224.0, 430.0], 32, 0, "[]"),
        ((2560, 1440), 1500, [1331.0, 814.0], [244.0, 195.0, 375.0], 32, 0, "[]"),
        ((3840, 2160), 2000, [1544.0, 934.0], [280.0, 224.0, 430.0], 32, 0, "[]"),
    ] {
        let f = open(&s.0, w, h, milli);
        let d = f.dock_state();
        assert_eq!(f.node("studio.dock.state").unwrap().value.as_deref(), Some(d.as_str()));
        let keys = ["window", "scale", "logical", "left", "left_state", "right", "right_state", "meter", "bottom", "bottom_state", "centre", "right_group_heights", "rail_pitch", "rail_overflow_slots", "applied", "groups", "rail_panels"];
        let mut at = 0;
        for k in keys {
            let i = d[at..].find(&format!("\"{k}\":")).unwrap_or_else(|| panic!("{k} after {at} in {d}")) + at;
            at = i;
        }
        assert_eq!(nums(&d, "window"), [w as f32, h as f32]);
        let c = nums(&d, "centre");
        assert!((c[0] - centre[0]).abs() <= 1.0 && (c[1] - centre[1]).abs() <= 1.0, "{w}x{h}: {d}");
        assert_eq!(nums(&d, "right_group_heights"), groups, "{w}x{h}");
        assert!(d.contains(&format!(r#""rail_pitch":{pitch},"rail_overflow_slots":{overflow},"applied":{applied}"#)), "{d}");
        assert!(d.contains(r#""left":32,"left_state":"rail","right":300,"right_state":"expanded","meter":0,"bottom":28,"bottom_state":"rail""#), "{d}");
    }
}

/// AC-GEOM-02, -03, -04, AC-TEXT-01, -03 on every panel at the scale matrix; AC-REACH-04: the 19 titles in order at
/// every window, also under the FIT-M1 chevron; AC-TEXT-01: menu titles, tabs and Context Bar labels never ellipsize.
#[test]
fn tree_checks_and_menu_titles_at_the_matrix() {
    let s = Scratch::new("chrome-checks");
    let mut f = open(&s.0, 1920, 1080, 1000);
    for milli in [1000u32, 1250, 1500, 2000] {
        f.apply(HostEvent::ScaleFactor { milli });
        f.frame().unwrap();
        let sc = Scale::new(milli as f32 / 1000.0).unwrap();
        for p in f.panels() {
            assert!(checks::overlapping_controls(p.tree()).is_empty(), "{milli}: {:?}", checks::overlapping_controls(p.tree()));
            assert!(checks::under_text_floor(p.tree()).is_empty(), "{milli}");
        }
        assert!(f.clipped().is_empty(), "{milli}: {:?}", f.clipped());
        let l = f.layout().clone();
        let rects: Vec<Rect> = l.regions().iter().map(|(_, q)| *q).collect();
        assert!(checks::partition(l.window, &rects, sc).is_empty(), "{milli}");
        let t = |q: Rect| Rect::new(q.y0, q.x0, q.y1, q.x1);
        assert!(checks::edge_gaps(&[l.tool_rail, l.left, l.centre, l.right], sc).is_empty(), "{milli}");
        assert!(checks::edge_gaps(&[t(l.menu_bar), t(l.tab_strip), t(l.context_bar), t(l.centre), t(l.bottom), t(l.status)], sc).is_empty(), "{milli}");
        let titles: Vec<String> = all_nodes(&f).iter().filter(|n| n.role == Role::MenuItem).map(|n| n.label.clone().unwrap()).collect();
        assert_eq!(titles, preset::MENU, "{milli}: all 19 in order");
        for p in f.panels() {
            for n in &p.tree().nodes {
                if let Paint::Label { ellipsis: true, .. } = &n.paint {
                    let owner = p.tree().preorder().into_iter().map(|i| p.tree().get(i)).find(|m| m.children.iter().any(|c| std::ptr::eq(p.tree().get(*c), n)));
                    let oid = owner.map(|m| m.author_id.as_str()).unwrap_or("");
                    assert!(!oid.starts_with("menu.") && !oid.starts_with("studio.tab.") && !oid.starts_with("studio.options."), "{milli}: {oid} ellipsizes");
                }
            }
        }
    }
    // AC-GEOM-04 at scale 1: component heights and pitches; layer rows on the 4 px grid from the body top.
    f.apply(HostEvent::ScaleFactor { milli: 1000 });
    f.frame().unwrap();
    let h = |id: &str| f.node(id).unwrap().bounds.h();
    assert_eq!((h("menu.file"), h("studio.tab.document"), h("studio.toolgroup.select"), h("studio.group.layers"), h("studio.search")), (28.0, 32.0, 32.0, 26.0, 24.0));
    let body = f.regions().body.y0;
    for r in f.rows().to_vec() {
        let b = f.node(&row_id(&r.layer_id)).unwrap().bounds;
        assert_eq!((b.h(), (b.y0 - body) % 4.0), (32.0, 0.0));
    }
    // FIT-M1 at narrow widths: padding 5, abbreviations, then the chevron; all 19 nodes stay.
    for (w, h) in [(1280, 720), (1024, 640), (640, 480)] {
        let g = open(&s.0, w, h, 1000);
        let nodes = all_nodes(&g);
        let titles: Vec<String> = nodes.iter().filter(|n| n.role == Role::MenuItem).map(|n| n.label.clone().unwrap()).collect();
        assert_eq!(titles, preset::MENU, "{w}x{h}");
        let mut gg = g;
        assert!(gg.clipped().is_empty(), "{w}x{h}: {:?}", gg.clipped());
    }
    let narrow = open(&s.0, 640, 480, 1000);
    let more = narrow.node("shell.menu-bar.more").expect("chevron at 640");
    assert!(more.value.as_deref().unwrap().starts_with("not_implemented_yet hidden="));
}

/// AC-COL-01 (flat chrome pixels are tokens outside the document, glyphs, icons, thumbnails, rounded chips and the
/// proxy) and AC-COL-02 (chromatic pixels only on the active slot, the active tab edges, the selected row, the module
/// icon and the presence chip).
#[test]
fn chrome_pixels_are_tokens_and_colour_is_rare() {
    let s = Scratch::new("chrome-col");
    let mut f = open(&s.0, 1920, 1080, 1000);
    let red = layer(&f, "Red");
    f.invoke(&row_id(&red)).unwrap();
    f.frame().unwrap();
    let tokens: Vec<[u8; 4]> = ColorToken::ALL.iter().map(|t| t.resolve(Theme::Dark)).map(|c| [c.r, c.g, c.b, c.a]).collect();
    let mut skip: Vec<Rect> = vec![f.image_rect().unwrap()];
    let mut allowed: Vec<Rect> = Vec::new();
    for p in f.panels() {
        for n in &p.tree().nodes {
            let rounded = matches!(n.paint, Paint::Surface { radius, .. } if radius > 0.0);
            if rounded || matches!(n.paint, Paint::Label { .. } | Paint::Value { .. } | Paint::Icon { .. } | Paint::TintedIcon { .. } | Paint::Thumbnail(_) | Paint::Round { .. } | Paint::Ruler { .. }) {
                skip.push(n.bounds);
            }
            let chromatic_owner = n.author_id == "shell.module-switch"
                || n.author_id == "studio.presence.operator"
                || (n.flags.toggled && n.author_id.starts_with("studio.toolgroup."))
                || (n.flags.selected && (n.author_id.starts_with("studio.tab.") || n.author_id.starts_with("studio.panel.layers.row.")));
            if chromatic_owner {
                allowed.push(n.bounds);
            }
        }
    }
    skip.push(f.node("studio.colour.proxy").unwrap().bounds);
    allowed.push(f.node("studio.colour.proxy").unwrap().bounds);
    let c = f.canvas().unwrap();
    let inside = |q: &[Rect], x: u32, y: u32| q.iter().any(|r| r.contains(Point::new(x as f32 + 0.5, y as f32 + 0.5)));
    let (mut off, mut chroma) = (0, 0);
    for y in 0..1080u32 {
        for x in 0..1920u32 {
            let p = c.pixel(x, y);
            if !inside(&skip, x, y) && !tokens.contains(&[p.r, p.g, p.b, p.a]) {
                off += 1;
            }
            let spread = p.r.max(p.g).max(p.b) - p.r.min(p.g).min(p.b);
            if spread > 12 && !inside(&allowed, x, y) && !inside(&[f.image_rect().unwrap()], x, y) {
                chroma += 1;
            }
        }
    }
    assert_eq!(off, 0, "non-token chrome pixels");
    assert_eq!(chroma, 0, "chromatic pixels outside the allowed places");
}

/// Hostile (b), (c), (d), (e): extreme scales, a 4096-byte and a lossy file name, a 10 kB device name, tiny windows,
/// and a script storm at the step cap.
#[test]
fn hostile_scales_names_tiny_windows_and_storms() {
    let s = Scratch::new("chrome-hostile");
    let mut f = open(&s.0, 1920, 1080, 1000);
    for milli in [0u32, 499, 8001, u32::MAX] {
        let mut g = GuiFrame::open(&fixture(&s.0), 1920, 1080, Some(milli)).unwrap();
        g.frame().unwrap();
        assert!(g.notes().contains(&"scale_clamped"), "{milli}");
    }
    let mut g = GuiFrame::open(&fixture(&s.0), 1920, 1080, Some(8000)).unwrap();
    g.frame().unwrap();
    assert!(g.notes().contains(&"below_min_size"), "logical 240 x 135");
    assert!(g.layout().below_min);
    g.apply(HostEvent::Resized { width: 20_000, height: 10 });
    assert!(g.notes().contains(&"extent_refused"));
    // Long and lossy names middle-ellipsize in the title and the tab; the labels carry the full name (AC-TEXT-02).
    for name in ["n".repeat(MAX_LABEL_BYTES), String::from_utf8_lossy(b"harbour-\xff\xfe-dusk.handshake").into_owned()] {
        f.set_file_name(&name);
        f.frame().unwrap();
        let title = f.node("shell.window-title").unwrap();
        assert!(title.label.as_deref().unwrap().starts_with(&name[..name.len().min(64)]));
        let tab = f.node("studio.tab.document").unwrap();
        assert_eq!(tab.label.as_deref(), Some(name.as_str()));
        assert!(f.clipped().is_empty(), "{:?}", f.clipped());
    }
    // A 10 kB device name: cut at 256 bytes, ellipsized in the bar, the cut text in the label.
    f.set_device("GPU ".repeat(2500));
    f.frame().unwrap();
    let dev = f.node("studio.status.device").unwrap().label.clone().unwrap();
    assert!(dev.len() <= 256, "{}", dev.len());
    assert!(f.clipped().is_empty());
    // Tiny windows lay out with clamped rects and say so.
    for (w, h) in [(1, 1), (44, 118), (1023, 639)] {
        let mut t = open(&s.0, w, h, 1000);
        assert!(t.notes().contains(&"below_min_size"), "{w}x{h}");
        let l = t.layout().clone();
        let rects: Vec<Rect> = l.regions().iter().map(|(_, q)| *q).collect();
        assert!(checks::partition(l.window, &rects, l.scale).is_empty(), "{w}x{h}");
        assert!(t.frame().is_ok());
    }
    // A storm at the step cap: rail picks and tab switches rebuild only the panels they change, no undo step.
    let mut g = open(&s.0, 1920, 1080, 1000);
    let undo = g.viewer().run().history()["undo"].as_array().unwrap().len();
    let ids = ["studio.toolgroup.paint", "studio.tab.adjustments", "studio.toolgroup.crop", "studio.tab.properties"];
    let builds0 = g.stats().builds;
    for i in 0..MAX_STEPS {
        g.step(&Step::Invoke(ids[i % ids.len()].into())).unwrap();
        g.frame().unwrap();
    }
    let per_step = (g.stats().builds - builds0) as f64 / MAX_STEPS as f64;
    assert!(per_step <= 4.0, "{per_step} rebuilds per step");
    assert_eq!(g.viewer().run().history()["undo"].as_array().unwrap().len(), undo);
}

/// Item 8: the receipt's frame member: preset, fit rules, tool, active tabs, static outcomes, summed paint stats
/// (no missing resource, no refused glyph); AC-LOOK-04: idle frames rebuild and upload nothing.
#[test]
fn receipt_frame_member_and_idle_frames() {
    let s = Scratch::new("chrome-receipt");
    let mut f = open(&s.0, 1280, 720, 1000);
    let g = f.receipt();
    let fr = g.get("frame").expect("frame member").render();
    for want in [r#""preset":"raster""#, r#""applied":["FIT-R1","FIT-R2"]"#, r#""tool_active":"select""#, r#""static_outcomes":0"#, r#""glyph_refusals":0"#, r#""missing_resources":0"#] {
        assert!(fr.contains(want), "{want} in {fr}");
    }
    assert!(fr.contains(r#""active_tabs":["properties","colour","layers"]"#), "{fr}");
    let builds = f.stats().builds;
    for _ in 0..10 {
        assert!(!f.frame().unwrap());
    }
    assert_eq!(f.stats().builds, builds);
}

/// AC-COL-01 source half: the frame code under src/gui types no colour (tokens only; the proxy's fill chip is the
/// document's brush colour, read at run time).
#[test]
fn gui_source_has_no_colour_literal() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/gui");
    let mut files = 0;
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        let src = std::fs::read_to_string(&p).unwrap();
        files += 1;
        for (i, line) in src.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            let lit = code.match_indices("Rgba8::new(").any(|(at, m)| code[at + m.len()..].trim_start().starts_with(|c: char| c.is_ascii_digit()));
            assert!(!lit, "{}:{}: colour literal", p.display(), i + 1);
            let hex = code.match_indices('#').any(|(at, _)| {
                let tail: String = code[at + 1..].chars().take_while(|c| c.is_ascii_hexdigit()).collect();
                tail.len() == 6 || tail.len() == 8
            });
            assert!(!hex, "{}:{}: hex colour literal", p.display(), i + 1);
        }
    }
    assert!(files >= 5);
}
