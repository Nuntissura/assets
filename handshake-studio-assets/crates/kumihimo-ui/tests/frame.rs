//! SMB-GUI-FRAME-1 items 2 and 3: the frame roles, node seams, ruler, round and tinted paints, the AC-GEOM-01
//! partition check, the fit helpers, the middle ellipsis and the frame widgets at components.json sizes.
use kumihimo_ui::checks::{self, PartitionFault};
use kumihimo_ui::display::{IconId, Prim, Side};
use kumihimo_ui::fit::{fit_labels, fit_tabs};
use kumihimo_ui::gallery;
use kumihimo_ui::geom::{Rect, Scale};
use kumihimo_ui::icons;
use kumihimo_ui::paint::{Canvas, NoResources, paint};
use kumihimo_ui::text::{BoxText, MAX_LABEL_BYTES, Text};
use kumihimo_ui::tokens::{ColorToken, TextToken, Theme, size};
use kumihimo_ui::tree::{Axis, Builder, Interaction, LayoutSpec, MAX_RULER_TICKS, Node, Paint, Panel, Role, Size, Tree};
use kumihimo_ui::widgets::{self, GroupHeader, MenuTitle, frame as fw};
use std::time::{Duration, Instant};

const NIY: Option<&str> = Some("not_implemented_yet");

fn text() -> Text {
    Text::new(Box::new(BoxText))
}
fn build(w: f32, h: f32, t: &mut Text, f: impl FnOnce(&mut Builder<'_>)) -> Panel {
    let mut p = Panel::default();
    p.update(1, &Interaction::default(), Rect::xywh(0.0, 0.0, w, h), Scale::ONE, Theme::Dark, t, f).expect("build");
    p
}
fn node<'a>(p: &'a Panel, id: &str) -> &'a Node {
    let t = p.tree();
    t.get(t.find(id).unwrap_or_else(|| panic!("no node {id}")))
}
fn seam_color() -> kumihimo_ui::geom::Rgba8 {
    ColorToken::EdgeSeam.resolve(Theme::Dark)
}

#[test]
fn frame_roles_carry_accesskit_names() {
    let names: Vec<&str> =
        [Role::MenuBar, Role::MenuItem, Role::TabPanel, Role::Status, Role::ComboBox, Role::SearchInput, Role::Canvas, Role::ScrollBar, Role::Region].iter().map(|r| r.name()).collect();
    assert_eq!(names, ["MenuBar", "MenuItem", "TabPanel", "Status", "ComboBox", "SearchInput", "Canvas", "ScrollBar", "Region"]);
}

/// Item 2b: a seam is a 1 logical px edge.seam Prim::Edge drawn after the node's children, hairline at any scale.
#[test]
fn seams_draw_hairline_edges_after_children() {
    let mut t = text();
    let p = build(100.0, 40.0, &mut t, |b| {
        b.begin(Node::new("studio.band.a", Role::Group).seam(Side::Bottom).seam(Side::Right).paint(Paint::Fill(ColorToken::SurfaceFrame)).size(Size::Fill, Size::Fill));
        b.leaf(Node::deco().paint(Paint::Fill(ColorToken::SurfacePanel)).size(Size::Fill, Size::Fill));
        b.end();
    });
    let items = &p.display_list().items;
    let edges: Vec<(usize, Side)> =
        items.iter().enumerate().filter_map(|(i, it)| match it { Prim::Edge { side, px: 1, color, .. } if *color == seam_color() => Some((i, *side)), _ => None }).collect();
    assert_eq!(edges.iter().map(|e| e.1).collect::<Vec<_>>(), [Side::Right, Side::Bottom], "{items:?}");
    let child_fill = items.iter().position(|it| matches!(it, Prim::Rect { color, .. } if *color == ColorToken::SurfacePanel.resolve(Theme::Dark))).unwrap();
    assert!(edges.iter().all(|e| e.0 > child_fill), "seams draw over the children");
    for s in [1.0f32, 1.25, 1.5, 2.0] {
        let sc = Scale::new(s).unwrap();
        let mut c = Canvas::new(sc.edge(100.0) as u32, sc.edge(40.0) as u32, ColorToken::SurfaceStrip.resolve(Theme::Dark)).unwrap();
        paint(&mut c, p.display_list(), sc, &mut t, &mut NoResources);
        let y = c.height() - 1;
        let rows = (0..c.height()).rev().take_while(|&yy| c.pixel(c.width() / 2, yy) == seam_color()).count();
        assert_eq!(rows as i32, sc.hairline(), "scale {s}: bottom seam thickness");
        assert_eq!(c.pixel(c.width() / 2, y), seam_color());
    }
}

/// Item 2c: the ruler draws ticks in text.disabled (minor 5, major 10 long), labels at the major ticks in document px,
/// dropped near the far end rather than clipped, and at most MAX_RULER_TICKS ticks.
#[test]
fn ruler_ticks_labels_drop_and_cap() {
    let mut t = text();
    let p = build(380.0, 20.0, &mut t, |b| widgets::ruler(b, "studio.viewer.ruler.x", "Horizontal ruler", Axis::Row, 60.0, true));
    let r = node(&p, "studio.viewer.ruler.x");
    assert!(matches!(r.paint, Paint::Ruler { axis: Axis::Row, step: 20.0, major: 5, labels: true, .. }), "{:?}", r.paint);
    let tick = ColorToken::TextDisabled.resolve(Theme::Dark);
    let ticks: Vec<Rect> = p.display_list().items.iter().filter_map(|it| match it { Prim::Edge { owner, color, .. } if *color == tick => Some(*owner), _ => None }).collect();
    // Ticks at 0, 20, ..., 360 (document 0 at x 60, so ticks also run left of the document origin).
    assert_eq!(ticks.len(), 19, "{ticks:?}");
    let major: Vec<f32> = ticks.iter().filter(|q| q.h() == 10.0).map(|q| q.x0).collect();
    assert_eq!(major, [60.0, 160.0, 260.0, 360.0]);
    assert!(ticks.iter().all(|q| q.h() == 5.0 || q.h() == 10.0));
    assert!(ticks.iter().all(|q| q.y1 == 20.0), "ticks stand on the far edge");
    let labels: Vec<f32> = p.display_list().items.iter().filter_map(|it| match it { Prim::Glyphs { origin, .. } => Some(origin.x), _ => None }).collect();
    // Major ticks at 60 (0), 160 (100), 260 (200), 360 (300): the last label would run past 380 and is dropped.
    assert_eq!(labels.len(), 3, "{labels:?}");
    assert!(checks::clipped_labels(p.tree(), &mut t).is_empty());
    // The vertical ruler draws ticks and no labels.
    let v = build(20.0, 300.0, &mut t, |b| widgets::ruler(b, "studio.viewer.ruler.y", "Vertical ruler", Axis::Column, 0.0, false));
    assert!(!v.display_list().items.iter().any(|it| matches!(it, Prim::Glyphs { .. })));
    assert_eq!(v.display_list().items.iter().filter(|it| matches!(it, Prim::Edge { color, .. } if *color == tick)).count(), 15);
    // A 16384 px ruler at step 20 would need 820 ticks; a hostile 4 px step is capped.
    let big = build(16_384.0, 20.0, &mut t, |b| {
        b.leaf(Node::new("studio.viewer.ruler.x", Role::Group).paint(Paint::Ruler { axis: Axis::Row, origin: 0.0, step: 1.0e-3, major: 5, labels: false }).size(Size::Fill, Size::Fill));
    });
    let n = big.display_list().items.iter().filter(|it| matches!(it, Prim::Edge { color, .. } if *color == tick)).count();
    assert!(n > 0 && n <= MAX_RULER_TICKS, "{n}");
}

/// Round chips and tinted icons emit rounded fills and fixed-token icons (a fixed token stays when disabled: the module
/// icon keeps accent.base, the variant marker text.tertiary).
#[test]
fn round_and_tinted_icon_paints() {
    let mut t = text();
    let p = build(100.0, 40.0, &mut t, |b| {
        b.leaf(Node::deco().paint(Paint::Round { fill: Some(ColorToken::PresenceOperator), radius: 9.0, border: Some(ColorToken::EdgeSeam) }).size(Size::Fixed(18.0), Size::Fixed(18.0)));
        b.leaf(Node::deco().paint(Paint::TintedIcon { icon: IconId(7), size: 16.0, color: ColorToken::AccentBase }).size(Size::Fixed(16.0), Size::Fixed(16.0)));
        b.begin(Node::deco().flags(|f| f.disabled = true).size(Size::Fixed(16.0), Size::Fixed(16.0)));
        b.leaf(Node::deco().paint(Paint::TintedIcon { icon: IconId(7), size: 16.0, color: ColorToken::AccentBase }).size(Size::Fixed(16.0), Size::Fixed(16.0)));
        b.end();
    });
    let items = &p.display_list().items;
    assert!(items.iter().any(|it| matches!(it, Prim::RRect { radius, fill, border: Some(_), .. } if *radius == 9.0 && *fill == ColorToken::PresenceOperator.resolve(Theme::Dark))));
    let icons: Vec<_> = items.iter().filter_map(|it| match it { Prim::Icon { form, hot, .. } => Some((*form, *hot)), _ => None }).collect();
    let acc = ColorToken::AccentBase.resolve(Theme::Dark);
    assert_eq!(icons, [(acc, acc), (acc, acc)]);
}

/// Item 2d: AC-GEOM-01 partition: exact cover, disjoint, inside; faults are typed.
#[test]
fn partition_check_finds_gaps_overlaps_and_escapes() {
    let w = Rect::new(0.0, 0.0, 1706.667, 960.0);
    let left = Rect::new(0.0, 0.0, 500.333, 960.0);
    let right = Rect::new(500.333, 0.0, 1706.667, 960.0);
    for s in [1.0f32, 1.25, 1.5, 2.0] {
        assert!(checks::partition(w, &[left, right], Scale::new(s).unwrap()).is_empty(), "scale {s}");
    }
    let one = Scale::ONE;
    let empty = Rect::new(500.333, 0.0, 500.333, 960.0);
    assert!(checks::partition(w, &[left, empty, right], one).is_empty(), "an empty member is allowed");
    let short = Rect::new(510.0, 0.0, 1706.667, 960.0);
    assert!(matches!(checks::partition(w, &[left, short], one).as_slice(), [PartitionFault::Uncovered(px)] if *px == 10 * 960), "{:?}", checks::partition(w, &[left, short], one));
    let wide = Rect::new(0.0, 0.0, 520.0, 960.0);
    assert!(checks::partition(w, &[wide, right], one).contains(&PartitionFault::Overlap(0, 1)));
    let out = Rect::new(500.333, 0.0, 1800.0, 960.0);
    assert!(checks::partition(w, &[left, out], one).contains(&PartitionFault::Outside(1)));
}

/// Item 2e: fit_labels takes the first padding step where every label fits, else the last step and an overflow
/// chip; a label over the shaping bound never fits; nineteen 4096-byte labels all overflow in bounded time.
#[test]
fn fit_labels_padding_steps_overflow_and_bounds() {
    let mut t = text();
    let labels = ["FILE", "EDIT", "VIEW", "DOCUMENT"];
    // Widths as layout sizes a Hug label: measured, then ceiled.
    let w: Vec<f32> = labels.iter().map(|l| t.measure(l, TextToken::MenuTitle, 0).ceil()).collect();
    let total = |pad: f32| w.iter().map(|x| x + 2.0 * pad).sum::<f32>();
    let f = fit_labels(&mut t, &labels, TextToken::MenuTitle, 0, &[8.0, 5.0], total(8.0) + 0.5, 20.0);
    assert_eq!((f.pad_step, f.shown, f.overflow()), (0, 4, false));
    let f = fit_labels(&mut t, &labels, TextToken::MenuTitle, 0, &[8.0, 5.0], total(8.0) - 1.0, 20.0);
    assert_eq!((f.pad_step, f.pad, f.shown), (1, 5.0, 4));
    assert!((f.used - total(5.0)).abs() < 0.01);
    let room = w[0] + w[1] + 20.0 + 20.0 + 1.0;
    let f = fit_labels(&mut t, &labels, TextToken::MenuTitle, 0, &[8.0, 5.0], room, 20.0);
    assert_eq!((f.pad_step, f.shown, f.overflow()), (1, 2, true), "{f:?}");
    assert!(f.used <= room);
    let huge = "W".repeat(MAX_LABEL_BYTES + 1);
    let f = fit_labels(&mut t, &[huge.as_str()], TextToken::Body, 0, &[0.0], 1.0e9, 0.0);
    assert_eq!(f.shown, 0, "over the bound never fits");
    let long = "M".repeat(MAX_LABEL_BYTES);
    let many: Vec<&str> = vec![long.as_str(); 19];
    let t0 = Instant::now();
    let f = fit_labels(&mut t, &many, TextToken::MenuTitle, 600, &[8.0, 5.0], 1920.0, 20.0);
    assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
    assert_eq!((f.shown, f.widths.len()), (0, 19));
}

/// docking.tab_overflow: the active tab is always shown and takes the slot of the last visible tab.
#[test]
fn fit_tabs_keeps_the_active_tab_in_the_last_visible_slot() {
    let mut t = text();
    let labels = ["Properties", "Adjustments", "Effect Controls", "Masks"];
    let all = fit_tabs(&mut t, &labels, 0, TextToken::Tab, 0, 600, 8.0, 10_000.0, 30.0);
    assert_eq!((all.shown, all.hidden), (vec![0, 1, 2, 3], vec![]));
    fn w(t: &mut Text, s: &str, wt: u16) -> f32 {
        t.measure(s, TextToken::Tab, wt).ceil() + 16.0
    }
    let room = w(&mut t, "Properties", 600) + w(&mut t, "Adjustments", 0) + 30.0 + 1.0;
    let f = fit_tabs(&mut t, &labels, 0, TextToken::Tab, 0, 600, 8.0, room, 30.0);
    assert_eq!((f.shown, f.hidden), (vec![0, 1], vec![2, 3]));
    let room = w(&mut t, "Properties", 0) + w(&mut t, "Masks", 600) + 30.0 + 1.0;
    let f = fit_tabs(&mut t, &labels, 3, TextToken::Tab, 0, 600, 8.0, room, 30.0);
    assert_eq!((f.shown, f.hidden), (vec![0, 3], vec![1, 2]), "Masks takes the last visible slot");
    let f = fit_tabs(&mut t, &labels, 2, TextToken::Tab, 0, 600, 8.0, 1.0, 30.0);
    assert_eq!(f.shown, vec![2], "the active tab is shown even with no room");
}

/// FIT-T1 middle ellipsis: whole text when it fits; head and tail kept on char boundaries; bounded on 1 MiB.
#[test]
fn ellipsize_middle_keeps_head_and_tail_bounded() {
    let mut t = text();
    let name = "harbour-dusk-retouch-final.handshake";
    let full = t.measure(name, TextToken::Tab, 0);
    assert_eq!(t.ellipsize_middle(name, TextToken::Tab, 0, full), name);
    let cut = t.ellipsize_middle(name, TextToken::Tab, 0, full / 2.0);
    let (head, tail) = cut.split_once('\u{2026}').expect("an ellipsis");
    assert!(name.starts_with(head) && name.ends_with(tail) && !head.is_empty() && !tail.is_empty(), "{cut}");
    assert!(head.chars().count() >= tail.chars().count());
    assert!(t.measure(&cut, TextToken::Tab, 0) <= full / 2.0);
    let multi = "\u{00e9}\u{4e2d}\u{1f600}".repeat(40);
    let cut = t.ellipsize_middle(&multi, TextToken::Tab, 0, 80.0);
    assert!(cut.contains('\u{2026}') && t.measure(&cut, TextToken::Tab, 0) <= 80.0, "{cut}");
    let misses = t.stats().shape_misses;
    let huge = "ab".repeat(1 << 19);
    let t0 = Instant::now();
    let cut = t.ellipsize_middle(&huge, TextToken::Tab, 0, 200.0);
    assert!(t0.elapsed() < Duration::from_secs(2));
    assert!(cut.len() <= MAX_LABEL_BYTES && cut.contains('\u{2026}'));
    assert!(t.stats().shape_misses - misses < 64, "{:?}", t.stats());
    assert_eq!(t.ellipsize_middle("abc", TextToken::Tab, 0, 0.0), "\u{2026}");
}

fn ids(tree: &Tree) -> Vec<String> {
    tree.preorder().into_iter().map(|i| tree.get(i).author_id.clone()).filter(|s| !s.is_empty()).collect()
}

/// Item 3: frame widgets at components.json sizes, roles and reasons.
#[test]
fn frame_widgets_sizes_roles_and_reasons() {
    let mut t = text();
    let close = icons::ui("close").unwrap();
    let p = build(1920.0, 400.0, &mut t, |b| {
        b.begin(Node::deco().layout(LayoutSpec { axis: Axis::Column, w: Size::Fill, h: Size::Fill, cross: kumihimo_ui::tree::Align::Start, ..LayoutSpec::default() }));
        b.begin(Node::deco().layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fixed(28.0), ..LayoutSpec::default() }));
        let titles = [
            MenuTitle { id: "menu.file", text: "FILE", full: "FILE", shown: true },
            MenuTitle { id: "menu.document", text: "DOC", full: "DOCUMENT", shown: true },
            MenuTitle { id: "menu.help", text: "HELP", full: "HELP", shown: false },
        ];
        b.begin(Node::new("shell.menu-bar", Role::MenuBar).layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fill, ..LayoutSpec::default() }));
        widgets::menu_title_row(b, &titles, 8.0, Some(("shell.menu-bar.more", "not_implemented_yet hidden=menu.help", icons::ui("chev_down").unwrap())), NIY);
        widgets::presence_chip(b, "studio.presence.operator", "Operator", "O", ColorToken::PresenceOperator, NIY);
        b.end();
        b.end();
        b.begin(Node::deco().layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fixed(32.0), ..LayoutSpec::default() }));
        widgets::document_tab(b, "studio.tab.document", "layered.handshake", "layered.handshake", 220.0, true, true, ("studio.tab.document.close", close), NIY);
        widgets::search_field(b, "studio.search", "Search", "Search tools, commands, help", "Ctrl K", icons::ui("search").unwrap(), NIY);
        widgets::dropdown_face(b, "studio.layout-preset.dropdown", "Layout preset", "Photo", icons::ui("chev_down").unwrap(), false, None, NIY);
        b.end();
        b.begin(Node::deco().layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fixed(34.0), ..LayoutSpec::default() }));
        widgets::context_chip(b, "studio.options.identity", "Paint Brush", icons::tool("paint").unwrap(), NIY);
        widgets::dropdown_face(b, "studio.options.preset", "Tool preset", "Default", icons::ui("chev_down").unwrap(), true, None, NIY);
        b.end();
        b.begin(Node::deco().layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fixed(64.0), cross: kumihimo_ui::tree::Align::Start, ..LayoutSpec::default() }));
        widgets::tool_slot(b, "studio.toolgroup.paint", "Paint Brush", "paint_brush", icons::tool("paint").unwrap(), 32.0, true, Some(icons::VARIANT_MARKER), None);
        widgets::tool_slot(b, "studio.toolgroup.range", "Range Selection", "range_selection", icons::tool("range").unwrap(), 28.0, false, None, None);
        widgets::icon_rail_button(b, "studio.rail.left.brushes", "Brushes", icons::ui("brushes").unwrap(), NIY);
        widgets::rail_caption_button(b, "studio.rail.bottom.jobs", "Jobs", icons::ui("jobs").unwrap(), NIY);
        b.end();
        let tabs = [("studio.tab.properties", "Properties"), ("studio.tab.adjustments", "Adjustments"), ("studio.tab.masks", "Masks")];
        b.begin(Node::deco().layout(LayoutSpec { axis: Axis::Column, w: Size::Fixed(300.0), h: Size::Fixed(26.0), ..LayoutSpec::default() }));
        widgets::group_header(
            b,
            &GroupHeader {
                id: "studio.group.properties",
                tabs: &tabs,
                shown: &[0, 1],
                active: 0,
                focused: true,
                collapsed: false,
                disclosure: ("studio.group.properties.disclosure", icons::ui("chev_down").unwrap()),
                more: Some(("studio.group.properties.more", "not_implemented_yet hidden=studio.tab.masks", icons::ui("chev_down").unwrap())),
                menu: ("studio.group.properties.menu", icons::ui("menu").unwrap()),
                reason: NIY,
            },
        );
        b.end();
        b.begin(Node::deco().layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fixed(24.0), ..LayoutSpec::default() }));
        widgets::status_readouts(b, &[("studio.status.zoom", "100%"), ("studio.status.size", "160 x 120 px"), ("studio.status.device", "Test GPU (discrete), vulkan 1.3")], Some(60.0));
        b.end();
        b.end();
    });
    let n = |id: &str| node(&p, id);
    // Menu titles: MenuBar of MenuItems, all three in the tree, the hidden one with no box and no drawn label.
    assert_eq!(n("shell.menu-bar").role, Role::MenuBar);
    let titles: Vec<String> = ids(p.tree()).into_iter().filter(|i| i.starts_with("menu.")).collect();
    assert_eq!(titles, ["menu.file", "menu.document", "menu.help"]);
    assert_eq!(n("menu.document").label.as_deref(), Some("DOCUMENT"), "abbreviated title keeps the full name");
    assert_eq!(n("menu.file").role, Role::MenuItem);
    assert_eq!(n("menu.file").bounds.h(), 28.0);
    assert_eq!(n("menu.file").bounds.w(), (t.measure("FILE", TextToken::MenuTitle, 0) + 16.0).ceil());
    assert!(n("menu.help").bounds.is_empty());
    assert_eq!(n("menu.help").value.as_deref(), Some("not_implemented_yet"));
    // D-F1: unbuilt controls keep the normal look and carry the reason as data.
    assert!(!n("menu.file").flags.disabled && !n("shell.menu-bar.more").flags.disabled && n("menu.file").value.as_deref() == NIY);
    assert!(!n("studio.tab.document").flags.disabled && n("studio.tab.document").flags.selected, "D-F2: the open document's tab is active");
    assert_eq!(n("shell.menu-bar.more").value.as_deref(), Some("not_implemented_yet hidden=menu.help"));
    assert_eq!((n("studio.presence.operator").bounds.w(), n("studio.presence.operator").bounds.h()), (fw::PRESENCE_D, fw::PRESENCE_D));
    assert_eq!(n("studio.presence.operator").label.as_deref(), Some("Operator"));
    assert!(!n("studio.presence.operator").actions.is_empty() && n("studio.presence.operator").value.as_deref() == NIY, "C-F1: it answers");
    // Document tab 220 x 32 with the close 20; the search field 240 x 24 (SearchInput); the preset face is a ComboBox.
    let tab = n("studio.tab.document");
    assert_eq!((tab.role, tab.bounds.w(), tab.bounds.h()), (Role::Tab, 220.0, 32.0));
    assert!(tab.seams.has(Side::Right));
    assert_eq!(n("studio.tab.document.close").bounds.w(), fw::DOC_TAB_CLOSE);
    let s = n("studio.search");
    assert_eq!((s.role, s.bounds.w(), s.bounds.h()), (Role::SearchInput, fw::SEARCH_W, size::FIELD_H));
    assert_eq!(n("studio.layout-preset.dropdown").role, Role::ComboBox);
    assert_eq!(n("studio.options.identity").label.as_deref(), Some("Paint Brush"));
    // Tool slots 36 x 32 and 36 x 28; the active one toggled; the variant marker only with variants.
    let a = n("studio.toolgroup.paint");
    assert_eq!((a.bounds.w(), a.bounds.h(), a.flags.toggled, a.value.as_deref()), (36.0, 32.0, true, Some("paint_brush")));
    assert_eq!(n("studio.toolgroup.range").bounds.h(), 28.0);
    let marker = |id: &str| {
        let tr = p.tree();
        tr.preorder().into_iter().filter(|&i| tr.is_ancestor(tr.find(id).unwrap(), i)).any(|i| matches!(tr.get(i).paint, Paint::TintedIcon { icon, .. } if icon == icons::VARIANT_MARKER))
    };
    assert!(marker("studio.toolgroup.paint") && !marker("studio.toolgroup.range"));
    assert_eq!((n("studio.rail.left.brushes").bounds.w(), n("studio.rail.left.brushes").bounds.h()), (32.0, 32.0));
    assert_eq!(n("studio.rail.bottom.jobs").bounds.h(), fw::RAIL_CAPTION_H);
    // Group header 26: disclosure 20 first, panel menu 20 last, +N between the tabs and the spacer.
    let g = n("studio.group.properties");
    assert_eq!((g.role, g.bounds.h()), (Role::TabList, 26.0));
    assert!(g.seams.has(Side::Bottom));
    let d = n("studio.group.properties.disclosure").bounds;
    let m = n("studio.group.properties.menu").bounds;
    assert_eq!((d.x0, d.w(), m.x1, m.w()), (0.0, 20.0, 300.0, 20.0));
    assert!(n("studio.tab.properties").flags.selected && !n("studio.tab.properties").flags.disabled, "tabs are live");
    assert!(p.tree().find("studio.tab.masks").is_some(), "hidden tabs stay in the tree");
    assert!(n("studio.tab.masks").bounds.is_empty());
    let more = n("studio.group.properties.more").bounds;
    assert!(more.x0 >= n("studio.tab.adjustments").bounds.x1 && more.x1 <= m.x0);
    // Status readouts: separators between, the last fixed and ellipsized with the full label.
    let dev = n("studio.status.device");
    assert_eq!((dev.bounds.w(), dev.label.as_deref()), (60.0, Some("Test GPU (discrete), vulkan 1.3")));
    assert!(matches!(dev.paint, Paint::Label { ellipsis: true, .. }));
    let seps = p.tree().nodes.iter().filter(|q| matches!(q.paint, Paint::Fill(ColorToken::EdgeSubtle)) && q.bounds.w() == 1.0 && q.bounds.h() == fw::STATUS_SEPARATOR_H).count();
    assert_eq!(seps, 2);
    // Tree checks: no overlapping controls, nothing clipped, nothing under the text floor.
    assert!(checks::overlapping_controls(p.tree()).is_empty(), "{:?}", checks::overlapping_controls(p.tree()));
    assert!(checks::clipped_labels(p.tree(), &mut t).is_empty(), "{:?}", checks::clipped_labels(p.tree(), &mut t));
    assert!(checks::under_text_floor(p.tree()).is_empty());
}

/// Hostile (d): 4096-byte and 1 MiB tool names: cut at MAX_LABEL_BYTES, clipped_labels reports the overflow in a
/// narrow bar instead of passing silently, nothing panics.
#[test]
fn hostile_tool_names_are_cut_and_reported() {
    let mut t = text();
    for len in [MAX_LABEL_BYTES, 1 << 20] {
        let name = "x".repeat(len);
        let p = build(300.0, 34.0, &mut t, |b| {
            b.begin(Node::new("studio.options", Role::Toolbar).layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fill, ..LayoutSpec::default() }));
            widgets::context_chip(b, "studio.options.identity", &name, icons::tool("paint").unwrap(), None);
            widgets::tool_slot(b, "studio.toolgroup.paint", &name, "paint_brush", icons::tool("paint").unwrap(), 32.0, false, None, None);
            b.end();
        });
        assert!(node(&p, "studio.options.identity").label.as_ref().unwrap().len() <= MAX_LABEL_BYTES);
        assert!(node(&p, "studio.toolgroup.paint").label.as_ref().unwrap().len() <= MAX_LABEL_BYTES);
        let clipped = checks::clipped_labels(p.tree(), &mut t);
        assert!(clipped.iter().any(|c| c.owner == "studio.options.identity" && c.overflow_px > 0.0), "{len}: {clipped:?}");
    }
}

/// The designer's contact sheet: every frame widget in every state, painted with the designer's icons and nothing
/// missing; Scene::ALL (the goldens) is unchanged.
#[test]
fn contact_sheet_renders_every_widget_with_the_icon_set() {
    let mut t = text();
    let (c, p, st) = gallery::render_contact_sheet(Scale::new(4.0).unwrap(), Theme::Dark, &mut t);
    assert_eq!((c.width(), c.height()), ((gallery::CONTACT_SHEET.0 * 4.0) as u32, (gallery::CONTACT_SHEET.1 * 4.0) as u32));
    assert_eq!((st.missing_resources, st.glyph_refusals), (0, 0), "{st:?}");
    let all = ids(p.tree());
    for want in ["menu.", "studio.tab.", "studio.search", "studio.toolgroup.", "studio.rail.", "studio.group.", "studio.status.", "studio.viewer.ruler."] {
        assert!(all.iter().any(|i| i.starts_with(want)), "{want} in {all:?}");
    }
    assert!(p.tree().nodes.iter().any(|n| n.value.as_deref() == Some("not_implemented_yet")) && p.tree().nodes.iter().any(|n| n.flags.toggled), "states shown");
    assert_eq!(gallery::Scene::ALL.len(), 4);
}

/// D-F6: the vertical ruler labels its major ticks with rotated caption labels (reading bottom to top, 2 px from the
/// outer edge, top at tick + 1, dropped near the end): icons of the ruler-label range whose masks Text rasterises.
#[test]
fn vertical_ruler_draws_rotated_labels() {
    use kumihimo_ui::text::{ruler_label_icon, ruler_label_value};
    let mut t = text();
    let v = build(20.0, 300.0, &mut t, |b| widgets::ruler(b, "studio.viewer.ruler.y", "Vertical ruler", Axis::Column, 40.0, true));
    let labels: Vec<(Rect, i64)> =
        v.display_list().items.iter().filter_map(|it| match it { Prim::Icon { rect, icon, .. } => ruler_label_value(*icon).map(|val| (*rect, val)), _ => None }).collect();
    // Majors at 40 (0), 140 (100) and 240 (200): each label runs down from tick + 1 and fits before 300.
    assert_eq!(labels.iter().map(|l| l.1).collect::<Vec<_>>(), [0, 100, 200], "{labels:?}");
    assert!(labels.iter().all(|(r, _)| r.x0 == 2.0), "2 px from the outer edge");
    assert_eq!(labels[1].0.y0, 141.0, "top at tick + 1");
    assert_eq!(ruler_label_value(ruler_label_icon(-300)), Some(-300));
    for s in [1.0f32, 1.5, 2.0] {
        let m = t.rotated_label("100", TextToken::Caption, Scale::new(s).unwrap()).expect("mask");
        assert!(m.height > m.width, "rotated: taller than wide at {s}: {}x{}", m.width, m.height);
        assert!(m.rg.iter().any(|p| p[0] > 0), "ink");
    }
    assert!(t.rotated_label(&"9".repeat(5000), TextToken::Caption, Scale::ONE).is_none(), "over the label bound");
}

/// D-F10: the frame's layer row draws its visibility icon in a fixed token (text.tertiary); the gallery's layer_row
/// output is unchanged (goldens).
#[test]
fn tinted_layer_row_eye() {
    let mut t = text();
    let p = build(300.0, 32.0, &mut t, |b| {
        widgets::layer_row_tinted(b, "studio.panel.layers.row.a", "A", ColorToken::TagNone, false, false, 0, None, icons::ui("eye_off").unwrap(), &[], Some(ColorToken::TextTertiary));
    });
    let tr = p.tree();
    let vis = tr.get(tr.find("studio.panel.layers.row.a.visibility").unwrap());
    assert!(matches!(tr.get(vis.children[0]).paint, Paint::TintedIcon { color: ColorToken::TextTertiary, .. }));
}
