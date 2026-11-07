//! Retained tree, idle quiet, hit test, focus order, press/release and the semantic tree (`gui.tree`).
use kumihimo_ui::gallery::{self, Scene};
use kumihimo_ui::geom::{Point, Rect, Scale};
use kumihimo_ui::semantic::{fnv64, semantic, stable_id, valid_author_id};
use kumihimo_ui::text::{BoxText, Text};
use kumihimo_ui::tokens::{ColorToken, Theme};
use kumihimo_ui::tree::{Action, BuildError, Builder, Interaction, Node, Panel, Role};

fn text() -> Text {
    Text::new(Box::new(BoxText))
}
fn panel(scene: Scene, ui: &Interaction) -> Panel {
    gallery::render(scene, Scale::ONE, Theme::Dark, ui, &mut text()).1
}
fn centre(p: &Panel, id: &str) -> Point {
    let b = p.tree().get(p.tree().find(id).unwrap()).bounds;
    Point::new((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0)
}

/// No render when idle: the same inputs rebuild nothing; a state or interaction change rebuilds once.
#[test]
fn panel_rebuilds_only_when_inputs_change() {
    let mut t = text();
    let mut ui = Interaction::default();
    let mut p = Panel::default();
    let vp = Rect::xywh(0.0, 0.0, 300.0, 34.0);
    let mut calls = 0;
    let step = |p: &mut Panel, rev: u64, ui: &Interaction, t: &mut Text, calls: &mut u32| {
        p.update(rev, ui, vp, Scale::ONE, Theme::Dark, t, |b| {
            *calls += 1;
            Scene::Toolbar.build(b)
        })
        .unwrap()
    };
    assert!(step(&mut p, 1, &ui, &mut t, &mut calls));
    for _ in 0..10 {
        assert!(!step(&mut p, 1, &ui, &mut t, &mut calls), "idle frame rebuilt");
    }
    assert_eq!((p.builds(), calls), (1, 1));
    assert!(step(&mut p, 2, &ui, &mut t, &mut calls), "state revision rebuilds");
    let tree = p.tree().clone();
    assert!(ui.pointer_move(&tree, centre(&p, "studio.options.gallery.reset")));
    assert!(step(&mut p, 2, &ui, &mut t, &mut calls), "hover rebuilds");
    assert!(p.tree().get(p.tree().find("studio.options.gallery.reset").unwrap()).flags.hovered);
    assert!(!ui.pointer_move(&tree, centre(&p, "studio.options.gallery.reset")), "same hover is no change");
    assert!(!step(&mut p, 2, &ui, &mut t, &mut calls));
    assert_eq!(p.builds(), 3);
}

#[test]
fn hit_test_press_release_and_hover_ownership() {
    let ui0 = Interaction::default();
    let p = panel(Scene::Toolbar, &ui0);
    let tree = p.tree();
    let id_at = |pt: Point| kumihimo_ui::hit::hit_test(tree, pt).map(|i| tree.get(i).author_id.clone());
    assert_eq!(id_at(centre(&p, "studio.options.gallery.brush")).as_deref(), Some("studio.options.gallery.brush"));
    // The label inside a button hits the button.
    assert_eq!(id_at(centre(&p, "studio.options.gallery.reset")).as_deref(), Some("studio.options.gallery.reset"));
    // Empty bar: the toolbar itself takes no action.
    assert_eq!(id_at(Point::new(150.0, 2.0)), None);
    assert_eq!(id_at(Point::new(-5.0, 10.0)), None);

    let mut ui = Interaction::default();
    let reset = centre(&p, "studio.options.gallery.reset");
    assert!(ui.pointer_down(tree, reset));
    assert_eq!(ui.pressed.as_deref(), Some("studio.options.gallery.reset"));
    assert!(!ui.pointer_move(tree, centre(&p, "studio.options.gallery.move")), "hover frozen while a widget owns the pointer");
    assert_eq!(ui.pointer_up(tree, reset).as_deref(), Some("studio.options.gallery.reset"), "release inside activates");
    ui.pointer_down(tree, reset);
    assert_eq!(ui.pointer_up(tree, Point::new(150.0, 2.0)), None, "release outside does not");
}

#[test]
fn focus_order_is_reading_order_and_the_ring_shows_on_keyboard_focus() {
    let mut ui = Interaction::default();
    let p = panel(Scene::Toolbar, &ui);
    let order: Vec<String> = (0..5).map(|_| ui.focus_step(p.tree(), false).unwrap()).collect();
    assert_eq!(
        order,
        ["studio.options.gallery.move", "studio.options.gallery.brush", "studio.options.gallery.snap", "studio.options.gallery.reset", "studio.options.gallery.move"]
    );
    assert_eq!(ui.focus_step(p.tree(), true).as_deref(), Some("studio.options.gallery.reset"));
    assert!(ui.focus_visible);
    // AC-FOCUS-01: focus.ring pixels on the focused control's edge, unclipped.
    let (c, p2, _) = gallery::render(Scene::Toolbar, Scale::ONE, Theme::Dark, &ui, &mut text());
    let ring = ColorToken::FocusRing.resolve(Theme::Dark);
    let b = p2.tree().get(p2.tree().find("studio.options.gallery.reset").unwrap()).bounds;
    let ring_px = (0..c.height()).flat_map(|y| (0..c.width()).map(move |x| (x, y))).filter(|&(x, y)| c.pixel(x, y) == ring).count();
    assert!(ring_px > 40, "focus ring visible: {ring_px} px");
    assert_eq!(c.pixel((b.x0 + b.w() / 2.0) as u32, (b.y0 - 2.0) as u32), ring, "ring sits outside the control");
    // Pointer press moves focus without the ring.
    let tree = p2.tree().clone();
    ui.pointer_down(&tree, centre(&p2, "studio.options.gallery.move"));
    assert!(!ui.focus_visible);
    // Disabled controls are not focus stops and do not take a press.
    let ui2 = Interaction::default();
    let mut b = Builder::new(Node::new("", Role::Group), &ui2);
    kumihimo_ui::widgets::button(&mut b, "studio.test.group.on", "On", true);
    kumihimo_ui::widgets::button(&mut b, "studio.test.group.off", "Off", false);
    let mut t = b.finish().unwrap();
    let root = t.root();
    kumihimo_ui::layout::LayoutPort::layout(&mut kumihimo_ui::layout::StackLayout, &mut t, root, Rect::xywh(0.0, 0.0, 300.0, 40.0), &mut text());
    let order = kumihimo_ui::hit::focus_order(&t);
    assert_eq!(order.len(), 1);
}

#[test]
fn semantic_tree_carries_ids_values_and_explicit_truncation() {
    let p = panel(Scene::Slider, &Interaction::default());
    let s = semantic(p.tree(), 1000);
    assert!(!s.truncated);
    let field = s.find("studio.panel.layers.opacity.field").unwrap();
    assert_eq!(field.id, "hs-studio-panel-layers-opacity-field");
    assert_eq!(field.node_id, fnv64("studio.panel.layers.opacity.field"));
    assert_eq!(field.role.name(), "SpinButton");
    assert_eq!(field.value.as_deref(), Some("72"));
    let n = field.numeric.unwrap();
    assert_eq!((n.value, n.min, n.max), (72.0, 0.0, 100.0));
    assert!(field.actions.contains(&"set_value"));
    assert_eq!((field.bounds.w(), field.bounds.h()), (72.0, 24.0));
    let json = s.to_json();
    assert!(json.starts_with("{\"schema\":\"kumihimo.ui.tree@1\""));
    assert!(json.contains("\"author_id\":\"studio.panel.layers.opacity.rail\""));
    assert!(json.contains("\"numeric\":{\"value\":72,\"min\":0,\"max\":100,\"step\":1}"));
    // Decorative nodes fold away: the slider group's children are the field and the rail.
    let group = s.find("studio.panel.layers.opacity").unwrap();
    assert_eq!(group.children.iter().map(|c| c.author_id.as_str()).collect::<Vec<_>>(), ["studio.panel.layers.opacity.field", "studio.panel.layers.opacity.rail"]);
    let cut = semantic(p.tree(), 2);
    assert!(cut.truncated && cut.nodes == 2, "STU-MDL-108: truncation is explicit");
    assert!(cut.to_json().contains("\"truncated\":true"));
    // A tab's selection and the stable id are in the tree.
    let tabs = semantic(panel(Scene::TabRow, &Interaction::default()).tree(), 100);
    assert!(tabs.find("studio.tab.layers").unwrap().selected);
    assert!(!tabs.find("studio.tab.paths").unwrap().selected);
    assert_eq!(stable_id("studio.tab.layers"), "hs-studio-tab-layers");
}

#[test]
fn author_ids_follow_the_grammar_and_are_unique() {
    for ok in ["studio.tab.layers", "studio.panel.layers.row.lyr-1", "studio.panel.preset-library#2", "studio.options.pen.width"] {
        assert!(valid_author_id(ok), "{ok}");
    }
    for bad in ["", "studio", "Studio.tab.layers", "studio.tab..x", "studio.tab.right 3", "studio.tab.x#1", "studio.tab.x#01", "studio.tab.-x", "studio.tab.x#"] {
        assert!(!valid_author_id(bad), "{bad}");
    }
    let ui = Interaction::default();
    let mut b = Builder::new(Node::new("", Role::Group), &ui);
    b.leaf(Node::new("studio.test.a", Role::Button).actions(&[Action::Click]));
    b.leaf(Node::new("studio.test.a", Role::Button).actions(&[Action::Click]));
    assert_eq!(b.finish(), Err(BuildError::DuplicateId("studio.test.a".into())));
    let mut b = Builder::new(Node::new("", Role::Group), &ui);
    b.leaf(Node::new("studio.Test", Role::Button));
    assert_eq!(b.finish(), Err(BuildError::InvalidId("studio.Test".into())));
    let mut b = Builder::new(Node::new("", Role::Group), &ui);
    b.end();
    assert_eq!(b.finish(), Err(BuildError::Unbalanced));
}

/// F4, F5: stable_id collisions and '#+2' are refused.
#[test]
fn stable_id_collisions_and_signed_instances_are_refused() {
    assert!(!valid_author_id("studio.tab.x#+2"));
    assert!(!valid_author_id("studio.tab.x#2x"));
    assert!(valid_author_id("studio.tab.a-b") && valid_author_id("studio.tab-a.b"));
    assert_eq!(stable_id("studio.tab.a-b"), stable_id("studio.tab-a.b"), "the spec's derivation meets here");
    let ui = Interaction::default();
    let mut b = Builder::new(Node::new("", Role::Group), &ui);
    b.leaf(Node::new("studio.tab.a-b", Role::Tab));
    b.leaf(Node::new("studio.tab-a.b", Role::Tab));
    assert_eq!(b.finish(), Err(BuildError::DuplicateStableId("studio.tab-a.b".into())));
}

/// F6: a new text system (fonts) or an explicit invalidate rebuilds; F7: one window Interaction, two panels, a hover
/// rebuilds only the panels it enters or leaves.
#[test]
fn text_epoch_invalidate_and_per_panel_interaction_slices() {
    let vp = Rect::xywh(0.0, 0.0, 300.0, 34.0);
    let mut ui = Interaction::default();
    let (mut a, mut b) = (Panel::default(), Panel::default());
    let mut t = text();
    let up = |p: &mut Panel, scene: Scene, ui: &Interaction, t: &mut Text| p.update(1, ui, vp, Scale::ONE, Theme::Dark, t, |bd| scene.build(bd)).unwrap();
    assert!(up(&mut a, Scene::Toolbar, &ui, &mut t) && up(&mut b, Scene::TabRow, &ui, &mut t));
    // Hover inside A: A rebuilds, B does not.
    let ta = a.tree().clone();
    assert!(ui.pointer_move(&ta, centre(&a, "studio.options.gallery.move")));
    assert!(up(&mut a, Scene::Toolbar, &ui, &mut t));
    assert!(!up(&mut b, Scene::TabRow, &ui, &mut t), "B's slice did not change");
    assert!(!up(&mut a, Scene::Toolbar, &ui, &mut t), "idle after the hover");
    // The pointer moves into B: both slices change once.
    let tb = b.tree().clone();
    assert!(ui.pointer_move(&tb, centre(&b, "studio.tab.paths")));
    assert!(up(&mut a, Scene::Toolbar, &ui, &mut t), "A lost its hover");
    assert!(up(&mut b, Scene::TabRow, &ui, &mut t), "B gained it");
    assert!(!up(&mut a, Scene::Toolbar, &ui, &mut t) && !up(&mut b, Scene::TabRow, &ui, &mut t));
    // F6: a new Text (other fonts) re-shapes with the same state revision; invalidate forces a rebuild.
    let mut t2 = text();
    assert_ne!(t.epoch(), t2.epoch());
    assert!(up(&mut a, Scene::Toolbar, &ui, &mut t2));
    assert!(!up(&mut a, Scene::Toolbar, &ui, &mut t2));
    a.invalidate();
    assert!(up(&mut a, Scene::Toolbar, &ui, &mut t2));
}

/// F8: Tab skips controls that are zero-size or clipped out by an ancestor.
#[test]
fn focus_skips_hidden_controls() {
    use kumihimo_ui::tree::{Axis, LayoutSpec, Size};
    let ui = Interaction::default();
    let mut b = Builder::new(Node::new("", Role::Group).layout(LayoutSpec { axis: Axis::Column, w: Size::Fill, h: Size::Fill, ..LayoutSpec::default() }), &ui);
    b.leaf(Node::new("studio.test.list.seen", Role::Button).actions(&[Action::Click]).size(Size::Fixed(40.0), Size::Fixed(24.0)));
    b.leaf(Node::new("studio.test.list.zero", Role::Button).actions(&[Action::Click]).size(Size::Fixed(0.0), Size::Fixed(24.0)));
    b.begin(Node::new("studio.test.list.view", Role::Group).flags(|f| f.clip = true).layout(LayoutSpec { axis: Axis::Column, w: Size::Fixed(100.0), h: Size::Fixed(24.0), ..LayoutSpec::default() }));
    b.leaf(Node::new("studio.test.list.row-1", Role::Button).actions(&[Action::Click]).size(Size::Fixed(100.0), Size::Fixed(24.0)));
    b.leaf(Node::new("studio.test.list.row-2", Role::Button).actions(&[Action::Click]).size(Size::Fixed(100.0), Size::Fixed(24.0)));
    b.end();
    let mut tree = b.finish().unwrap();
    let root = tree.root();
    kumihimo_ui::layout::LayoutPort::layout(&mut kumihimo_ui::layout::StackLayout, &mut tree, root, Rect::xywh(0.0, 0.0, 300.0, 300.0), &mut text());
    let order: Vec<String> = kumihimo_ui::hit::focus_order(&tree).into_iter().map(|i| tree.get(i).author_id.clone()).collect();
    assert_eq!(order, ["studio.test.list.seen", "studio.test.list.row-1"], "zero-size and scrolled-out rows are not stops");
}
