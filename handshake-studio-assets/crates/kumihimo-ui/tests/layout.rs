//! B1/B2 snapping and the stack layout at the designer's sizes, with the tree-method acceptance checks
//! (AC-GEOM-02, AC-GEOM-03, AC-GEOM-04, AC-TEXT-01, AC-TEXT-03).
use kumihimo_ui::checks::{clipped_labels, edge_gaps, overlapping_controls, under_text_floor};
use kumihimo_ui::display::{DisplayList, Prim};
use kumihimo_ui::gallery::{self, Scene};
use kumihimo_ui::geom::{Rect, Scale};
use kumihimo_ui::paint::{Canvas, NoResources, paint};
use kumihimo_ui::text::{BoxText, Text};
use kumihimo_ui::tokens::{ColorToken, Theme};
use kumihimo_ui::tree::{Interaction, Tree};

const SCALES: [f32; 9] = [1.0, 1.1, 1.25, 1.375, 1.5, 1.75, 2.0, 2.25, 3.0];

fn text() -> Text {
    Text::new(Box::new(BoxText))
}
fn tree_of(scene: Scene) -> Tree {
    let (_, panel, _) = gallery::render(scene, Scale::ONE, Theme::Dark, &Interaction::default(), &mut text());
    panel.tree().clone()
}
fn bounds(t: &Tree, id: &str) -> Rect {
    t.get(t.find(id).unwrap_or_else(|| panic!("node {id}"))).bounds
}

/// AC-GEOM-03: adjacent panes share one physical edge at every scale; docked panes split a 1920 frame at the
/// designer's widths plus awkward fractional ones.
#[test]
fn adjacent_panes_share_physical_edges_at_fractional_scales() {
    let widths = [44.0, 260.0, 1.0, 913.37, 6.0, 300.0, 0.5, 395.13];
    let mut x = 0.0f32;
    let rects: Vec<Rect> = widths
        .iter()
        .map(|w| {
            let r = Rect::new(x, 0.0, x + w, 100.0);
            x += w;
            r
        })
        .collect();
    for s in SCALES {
        let scale = Scale::new(s).unwrap();
        assert!(edge_gaps(&rects, scale).is_empty(), "gap at scale {s}");
        // Painted proof: two token fills side by side leave no background pixel and no overlap blend.
        let mut list = DisplayList::default();
        for (i, r) in rects.iter().enumerate() {
            let t = if i % 2 == 0 { ColorToken::SurfacePanel } else { ColorToken::SurfaceStrip };
            list.push(Prim::Rect { rect: *r, color: t.resolve(Theme::Dark) });
        }
        let w = scale.edge(x) as u32;
        let mut c = Canvas::new(w, scale.edge(100.0) as u32, ColorToken::FocusRing.resolve(Theme::Dark)).unwrap();
        paint(&mut c, &list, scale, &mut text(), &mut NoResources);
        let ring = ColorToken::FocusRing.resolve(Theme::Dark);
        for px in 0..w {
            assert_ne!(c.pixel(px, 0), ring, "unpainted column {px} at scale {s}");
        }
    }
    assert_eq!(Scale::new(1.25).unwrap().hairline(), 1);
    assert_eq!(Scale::new(2.5).unwrap().hairline(), 2);
    assert_eq!(Scale::new(1.25).unwrap().edge_px(2), 3);
    assert!(Scale::new(0.0).is_none() && Scale::new(f32::NAN).is_none() && Scale::new(9.0).is_none());
}

/// AC-GEOM-04: control heights and pitches equal components.json at scale 1.
#[test]
fn widgets_lay_out_at_the_designers_sizes() {
    let t = tree_of(Scene::Toolbar);
    let bar = bounds(&t, "studio.options.gallery");
    assert_eq!((bar.w(), bar.h()), (300.0, 34.0));
    for id in ["studio.options.gallery.move", "studio.options.gallery.brush"] {
        let b = bounds(&t, id);
        assert_eq!((b.w(), b.h()), (24.0, 24.0), "{id}");
        assert_eq!(b.y0, 5.0, "{id} centred in the 34 px bar");
    }
    let reset = bounds(&t, "studio.options.gallery.reset");
    assert_eq!(reset.h(), 24.0);
    assert!(reset.w() >= 56.0, "button.min_w: {}", reset.w());
    assert_eq!(reset.x1, 292.0, "flexible space pushes Reset to the right padding");
    assert_eq!(bounds(&t, "studio.options.gallery.move").x0, 8.0);

    let t = tree_of(Scene::TabRow);
    assert_eq!(bounds(&t, "studio.group.layers").h(), 26.0);
    let (a, b) = (bounds(&t, "studio.tab.layers"), bounds(&t, "studio.tab.channels"));
    assert_eq!((a.y0, a.h()), (0.0, 26.0));
    assert_eq!(a.x1, b.x0, "tabs abut");

    let t = tree_of(Scene::Slider);
    let row = bounds(&t, "studio.panel.layers.opacity");
    assert_eq!(row.h(), 44.0);
    let field = bounds(&t, "studio.panel.layers.opacity.field");
    assert_eq!((field.w(), field.h()), (72.0, 24.0));
    assert_eq!(field.x1, 300.0 - 12.0, "field ends at the panel's right padding");
    let rail = bounds(&t, "studio.panel.layers.opacity.rail");
    assert_eq!((rail.x0, rail.x1), (12.0, 288.0));

    let t = tree_of(Scene::LayerRow);
    let r1 = bounds(&t, "studio.panel.layers.row.lyr-1");
    let r2 = bounds(&t, "studio.panel.layers.row.lyr-2");
    assert_eq!((r1.y0, r1.h(), r2.y0, r2.h()), (0.0, 32.0, 32.0, 32.0));
    assert!(r1.on_grid(4.0) && r2.on_grid(4.0), "row tops on the 4 px grid");
    let eye = bounds(&t, "studio.panel.layers.row.lyr-1.visibility");
    assert_eq!((eye.w(), eye.h()), (22.0, 22.0));
    let th1 = bounds(&t, "studio.panel.layers.row.lyr-1.thumbnail");
    let th2 = bounds(&t, "studio.panel.layers.row.lyr-2.thumbnail");
    assert_eq!((th1.w(), th1.h()), (24.0, 24.0));
    assert_eq!(th2.x0 - th1.x0, 16.0, "one level of indent is 16 px");
}

/// AC-GEOM-02, AC-TEXT-01, AC-TEXT-03 on every gallery scene.
#[test]
fn gallery_has_no_overlap_no_clipped_label_and_respects_the_text_floor() {
    for scene in Scene::ALL {
        let t = tree_of(scene);
        assert!(overlapping_controls(&t).is_empty(), "{}: {:?}", scene.name(), overlapping_controls(&t));
        assert!(clipped_labels(&t, &mut text()).is_empty(), "{}: {:?}", scene.name(), clipped_labels(&t, &mut text()));
        assert!(under_text_floor(&t).is_empty());
    }
}

/// The checks catch what they claim to: a crowded toolbar overlaps and clips.
#[test]
fn checks_report_overlap_and_clipping() {
    use kumihimo_ui::tree::{Action, Align, Axis, Builder, LayoutSpec, Node, Paint, Role, Size};
    use kumihimo_ui::tokens::TextToken;
    let ui = Interaction::default();
    let mut b = Builder::new(Node::new("", Role::Group).layout(LayoutSpec { axis: Axis::Column, w: Size::Fill, h: Size::Fill, ..LayoutSpec::default() }), &ui);
    b.leaf(Node::new("studio.test.a", Role::Button).actions(&[Action::Click]).size(Size::Fixed(40.0), Size::Fixed(24.0)));
    let mut t = b.finish().unwrap();
    // Two controls forced into the same place, and a label narrower than its text.
    let mut over = Node::new("studio.test.b", Role::Button).actions(&[Action::Click]);
    over.parent = Some(t.root());
    over.bounds = Rect::xywh(10.0, 0.0, 40.0, 24.0);
    let mut lab = Node::deco().paint(Paint::Label { text: "Extremely long label".into(), token: TextToken::Body, color: None, align: Align::Start, ellipsis: false });
    lab.bounds = Rect::xywh(0.0, 30.0, 20.0, 16.0);
    t.nodes.push(over);
    t.nodes.push(lab);
    t.nodes[0].children.extend([kumihimo_ui::tree::NodeId(2), kumihimo_ui::tree::NodeId(3)]);
    t.nodes[1].bounds = Rect::xywh(0.0, 0.0, 40.0, 24.0);
    assert_eq!(overlapping_controls(&t), vec![("studio.test.a".to_owned(), "studio.test.b".to_owned())]);
    let clipped = clipped_labels(&t, &mut text());
    assert_eq!(clipped.len(), 1);
    assert!(clipped[0].overflow_px > 50.0);
}
