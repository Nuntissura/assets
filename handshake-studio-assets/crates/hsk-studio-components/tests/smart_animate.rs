//! Lane SMB-FIG-SMART-ANIMATE: smart animate layer matching (name path, same-name index, implicit
//! text style with nearest x/y), interpolation at stated progress points against hand-computed
//! values (eased through the transition's cubic bezier), dissolve of unmatched layers and hostile
//! snapshots. Figma parity of the rule is UNVERIFIED (oracle D5).
use hsk_studio_components::{
    ComponentsError, MAX_SMART_LAYERS, SmartFrame, SmartLayer, SmartProps, SmartSide, cubic_bezier_ease, interpolate,
    match_layers, smart_animate_frame,
};
use hsk_studio_folio::easing_preset;

fn props(x: f64, y: f64) -> SmartProps {
    SmartProps { x, y, width: 10.0, height: 10.0, rotation_deg: 0.0, opacity: 1.0, fill: None, corner_radius: 0.0 }
}

fn layer(id: &str, name: &str, parent: Option<&str>, p: SmartProps) -> SmartLayer {
    SmartLayer {
        id: id.into(),
        name: name.into(),
        parent: parent.map(Into::into),
        implicit_text_style: None,
        content: None,
        props: p,
    }
}

fn text(id: &str, parent: Option<&str>, style: &str, content: &str, p: SmartProps) -> SmartLayer {
    SmartLayer {
        implicit_text_style: Some(style.into()),
        content: Some(content.into()),
        ..layer(id, content, parent, p)
    }
}

fn frame(id: &str, layers: Vec<SmartLayer>) -> SmartFrame {
    SmartFrame { frame_id: id.into(), layers }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-12
}

#[test]
fn matching_by_name_path_same_name_index_and_implicit_text_style() {
    let from = frame("A", vec![
        layer("a-card", "card", None, props(0.0, 0.0)),
        layer("a-dot0", "dot", Some("a-card"), props(1.0, 1.0)),
        layer("a-dot1", "dot", Some("a-card"), props(2.0, 2.0)),
        text("a-t0", Some("a-card"), "Inter/16/#111", "Hello", props(10.0, 10.0)),
        text("a-t1", Some("a-card"), "Inter/16/#111", "World", props(10.0, 40.0)),
        layer("a-lone", "lone", None, props(0.0, 0.0)),
        layer("a-deep", "deep", Some("a-card"), props(0.0, 0.0)),
    ]);
    let to = frame("B", vec![
        layer("b-card", "card", None, props(5.0, 5.0)),
        // Same-name siblings pair by their index among same-named siblings, not by id order.
        layer("b-dot0", "dot", Some("b-card"), props(7.0, 7.0)),
        layer("b-dot1", "dot", Some("b-card"), props(8.0, 8.0)),
        // Implicit text: the nearest (x, y) of the same style under the same ancestors wins.
        text("b-t0", Some("b-card"), "Inter/16/#111", "Bye", props(11.0, 39.0)),
        text("b-t1", Some("b-card"), "Inter/16/#222", "Other style", props(10.0, 10.0)),
        // Same name at another depth (direct child of the frame here): no match.
        layer("b-deep", "deep", None, props(0.0, 0.0)),
        layer("b-new", "new", Some("b-card"), props(0.0, 0.0)),
    ]);
    let m = match_layers(&from, &to).unwrap();
    let pairs: Vec<(&str, &str)> = m.pairs.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    assert_eq!(pairs, [("a-card", "b-card"), ("a-dot0", "b-dot0"), ("a-dot1", "b-dot1"), ("a-t1", "b-t0")]);
    assert_eq!(m.from_only, ["a-t0", "a-lone", "a-deep"]);
    assert_eq!(m.to_only, ["b-t1", "b-deep", "b-new"]);
    // A renamed ancestor breaks the match of every descendant (ancestor names are part of the key).
    let mut renamed = to.clone();
    renamed.layers[0].name = "panel".into();
    let m = match_layers(&from, &renamed).unwrap();
    assert!(m.pairs.is_empty());
    assert_eq!(m.to_only.len(), 7);
}

#[test]
fn interpolated_properties_equal_hand_computed_values_at_progress_points() {
    let a = SmartProps {
        x: 24.0,
        y: 100.0,
        width: 70.0,
        height: 70.0,
        rotation_deg: 350.0,
        opacity: 0.2,
        fill: Some([0.2, 0.4, 0.6, 1.0]),
        corner_radius: 4.0,
    };
    let b = SmartProps {
        x: 200.0,
        y: 150.0,
        width: 110.0,
        height: 110.0,
        rotation_deg: 10.0,
        opacity: 1.0,
        fill: Some([1.0, 0.0, 0.0, 0.5]),
        corner_radius: 20.0,
    };
    // e = 0.25: x 24 + 176/4, y 100 + 50/4, size 70 + 40/4, rotation 350 + 20/4 (shortest arc through
    // 360, not 350 - 340/4), opacity 0.2 + 0.8/4, fill channel-wise, radius 4 + 16/4.
    let p = interpolate(&a, &b, 0.25);
    assert!(close(p.x, 68.0) && close(p.y, 112.5) && close(p.width, 80.0) && close(p.height, 80.0));
    assert!(close(p.rotation_deg, 355.0) && close(p.opacity, 0.4) && close(p.corner_radius, 8.0));
    let f = p.fill.unwrap();
    assert!(close(f[0], 0.4) && close(f[1], 0.3) && close(f[2], 0.45) && close(f[3], 0.875), "{f:?}");
    // Ends are exact.
    assert_eq!(interpolate(&a, &b, 0.0), a);
    let end = interpolate(&a, &b, 1.0);
    assert!(close(end.x, 200.0) && close(end.rotation_deg, 370.0) && close(end.opacity, 1.0));
    // Through the transition easing: EASE_IN_AND_OUT (0.42, 0, 0.58, 1) at time 0.5 is 0.5 (symmetric
    // curve), at time 0.25 it is 0.1291619310473198 (independent bisection, 200 steps, Python).
    let curve = easing_preset("EASE_IN_AND_OUT").unwrap();
    let e = cubic_bezier_ease(&curve, 0.5);
    let p = interpolate(&a, &b, e);
    assert!(close(p.x, 112.0) && close(p.y, 125.0) && close(p.width, 90.0) && close(p.rotation_deg, 360.0));
    let e = cubic_bezier_ease(&curve, 0.25);
    assert!((e - 0.1291619310473198).abs() < 1e-9, "{e}");
    let p = interpolate(&a, &b, e);
    assert!((p.x - 46.73249986432828).abs() < 1e-7 && (p.width - 75.16647724189279).abs() < 1e-7);
    // Back easing overshoot: geometry extrapolates, opacity and colour clamp, size never negative.
    let p = interpolate(&a, &b, 1.25);
    assert!(close(p.x, 244.0) && close(p.opacity, 1.0) && p.fill.unwrap()[0] == 1.0);
    let p = interpolate(&a, &b, -2.0);
    assert!(close(p.width, 0.0) && close(p.opacity, 0.0) && close(p.corner_radius, 0.0));
    // A fill that appears fades in through its alpha.
    let p = interpolate(&SmartProps { fill: None, ..a }, &b, 0.25);
    assert_eq!(p.fill, Some([1.0, 0.0, 0.0, 0.125]));
}

#[test]
fn unmatched_layers_dissolve_and_changed_content_cross_dissolves() {
    let from = frame("A", vec![
        layer("a-orb", "orb", None, SmartProps { opacity: 0.8, ..props(24.0, 100.0) }),
        layer("a-old", "old", None, SmartProps { opacity: 0.5, ..props(0.0, 0.0) }),
        layer("a-old-kid", "kid", Some("a-old"), props(1.0, 1.0)),
        text("a-title", None, "S", "Details", props(24.0, 24.0)),
    ]);
    let to = frame("B", vec![
        text("b-title", None, "S", "Animated", props(24.0, 24.0)),
        layer("b-orb", "orb", None, props(200.0, 150.0)),
        layer("b-new", "new", None, SmartProps { opacity: 0.6, ..props(0.0, 0.0) }),
    ]);
    let m = match_layers(&from, &to).unwrap();
    let d = smart_animate_frame(&from, &to, &m, 0.25).unwrap();
    let got: Vec<(&str, SmartSide, f64)> = d.iter().map(|l| (l.id.as_str(), l.side.clone(), l.props.opacity)).collect();
    // Source-only layers first (the outermost fades to 0.5 * 0.75; its child inherits and keeps 1),
    // the title content cross-dissolves in place (0.75 out under 0.25 in), the orb tweens opacity
    // 0.8 -> 1 (0.85 at 0.25) and the new layer dissolves in (0.6 * 0.25).
    let want = [
        ("a-old", SmartSide::FromOnly, 0.375),
        ("a-old-kid", SmartSide::FromOnly, 1.0),
        ("a-title", SmartSide::ContentOut { from: "a-title".into() }, 0.75),
        ("b-title", SmartSide::Matched { from: "a-title".into() }, 0.25),
        ("b-orb", SmartSide::Matched { from: "a-orb".into() }, 0.85),
        ("b-new", SmartSide::ToOnly, 0.15),
    ];
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(want.iter()) {
        assert!(g.0 == w.0 && g.1 == w.1 && close(g.2, w.2), "{g:?} vs {w:?}");
    }
    assert!(close(d[4].props.x, 68.0) && close(d[4].props.y, 112.5));
}

#[test]
fn hostile_snapshots_are_typed_refusals() {
    let ok = frame("B", vec![layer("b", "b", None, props(0.0, 0.0))]);
    let dangling = frame("A", vec![layer("a", "a", Some("ghost"), props(0.0, 0.0))]);
    assert_eq!(match_layers(&dangling, &ok).unwrap_err(), ComponentsError::NoSuchNode("ghost".into()));
    let cycle = frame("A", vec![layer("a", "a", Some("b"), props(0.0, 0.0)), layer("b", "b", Some("a"), props(0.0, 0.0))]);
    assert!(matches!(match_layers(&cycle, &ok).unwrap_err(), ComponentsError::InvalidValue(_)));
    let dup = frame("A", vec![layer("a", "a", None, props(0.0, 0.0)), layer("a", "x", None, props(0.0, 0.0))]);
    assert_eq!(match_layers(&dup, &ok).unwrap_err(), ComponentsError::Duplicate("a".into()));
    let nan = frame("A", vec![layer("a", "a", None, props(f64::NAN, 0.0))]);
    assert!(matches!(match_layers(&ok, &nan).unwrap_err(), ComponentsError::InvalidValue(_)));
    let many = frame("A", (0..=MAX_SMART_LAYERS).map(|i| layer(&format!("l{i}"), "n", None, props(0.0, 0.0))).collect());
    assert_eq!(match_layers(&many, &ok).unwrap_err(), ComponentsError::BudgetExceeded);
    let m = match_layers(&ok, &ok).unwrap();
    assert!(matches!(smart_animate_frame(&ok, &ok, &m, f64::INFINITY).unwrap_err(), ComponentsError::InvalidValue(_)));
}
