//! Lane SMB-FIG-SMART-ANIMATE-2: spring easing from the analytic damped oscillator, directional
//! transition placement, gradient fill tweening and the opt-in Studio text-by-content matching rule,
//! each against hand-computed values. Figma parity UNVERIFIED (oracle D5).
use hsk_studio_components::{
    ActiveTransition, MAX_TEXT_PAIR_EVALUATIONS, SmartFrame, SmartGradient, SmartLayer, SmartMatchOptions, SmartProps,
    SmartSide, directional_frame, gradients_tween, interpolate, match_layers, match_layers_with, smart_animate_frame,
    spring_progress, spring_settle_ms, text_similarity, transition_progress,
};
use hsk_studio_folio::{CubicBezier, SpringEasing, TransitionDirection as D, TransitionType as T};

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

fn spring(mass: f64, stiffness: f64, damping: f64, initial_velocity: f64) -> SpringEasing {
    SpringEasing { mass, stiffness, damping, initial_velocity }
}

#[test]
fn spring_progress_equals_the_damped_oscillator_solution() {
    // Critically damped (c = 2 sqrt(k m) = 20): x = (1 + 10 t) e^(-10 t); at t = 0.1, 1 - 2/e.
    let crit = spring(1.0, 100.0, 20.0, 0.0);
    assert!(close(spring_progress(&crit, 0.1).unwrap(), 1.0 - 2.0 * (-1.0f64).exp(), 1e-12));
    assert_eq!(spring_progress(&crit, 0.0), Some(0.0));
    // Under-damped (zeta 0.5, w0 10, wd sqrt 75): e^(-5 t)(cos(wd t) + 5/wd sin(wd t)); values from an
    // independent Python evaluation of the same closed form; it overshoots past 1 at 0.25 s.
    let under = spring(1.0, 100.0, 10.0, 0.0);
    let wd = 75.0f64.sqrt();
    let hand = 1.0 - (-0.5f64).exp() * ((wd * 0.1).cos() + 5.0 / wd * (wd * 0.1).sin());
    assert!(close(spring_progress(&under, 0.1).unwrap(), hand, 1e-12));
    assert!(close(hand, 0.34029984660829826, 1e-12));
    assert!(close(spring_progress(&under, 0.25).unwrap(), 1.0233595799066924, 1e-12));
    // Over-damped (zeta 2.5): r = -10(2.5 -/+ sqrt 5.25), C1 = -r2 / (r1 - r2).
    let over = spring(1.0, 100.0, 50.0, 0.0);
    let s = 5.25f64.sqrt();
    let (r1, r2) = (-10.0 * (2.5 - s), -10.0 * (2.5 + s));
    let c1 = -r2 / (r1 - r2);
    let hand = 1.0 - (c1 * (r1 * 0.1).exp() + (1.0 - c1) * (r2 * 0.1).exp());
    assert!(close(spring_progress(&over, 0.1).unwrap(), hand, 1e-12) && close(hand, 0.15178386178471592, 1e-12));
    // Initial velocity toward the target.
    assert!(close(spring_progress(&spring(2.0, 200.0, 8.0, 3.0), 0.15).unwrap(), 1.0004523163321422, 1e-12));
    // Settling (|x| <= 1e-3 from then on) and refusals.
    assert_eq!(spring_settle_ms(&under, 1e-3), Some(1271));
    assert_eq!(spring_settle_ms(&spring(1.0, 100.0, 0.0, 0.0), 1e-3), None, "undamped never settles");
    assert_eq!(spring_progress(&spring(0.0, 100.0, 10.0, 0.0), 0.1), None);
    assert_eq!(spring_progress(&spring(1.0, f64::NAN, 10.0, 0.0), 0.1), None);
    // The player's eased progress uses the spring in elapsed seconds, and 1 once the duration ends.
    let t = ActiveTransition {
        from: "a".into(),
        to: "b".into(),
        kind: "MoveIn".into(),
        start_ms: 0,
        duration_ms: 800,
        easing: CubicBezier { x1: 0.0, y1: 0.0, x2: 1.0, y2: 1.0 },
        match_layers: false,
        transition_type: T::MoveIn,
        direction: Some(D::Left),
        spring: Some(under.clone()),
    };
    assert!(close(transition_progress(&t, 0.125, 100.0), 0.34029984660829826, 1e-12));
    assert_eq!(transition_progress(&t, 1.0, 800.0), 1.0);
}

#[test]
fn directional_placement_equals_hand_values() {
    let (w, h, e) = (360.0, 520.0, 0.25);
    let f = |k, d| directional_frame(k, d, e, w, h).unwrap();
    let eq = |a: (f64, f64), b: (f64, f64)| close(a.0, b.0, 1e-9) && close(a.1, b.1, 1e-9);
    // MOVE_IN from the left: the destination is 3/4 of a width left of rest, the origin stays.
    let m = f(T::MoveIn, D::Left);
    assert!(eq(m.to_offset, (-270.0, 0.0)) && eq(m.from_offset, (0.0, 0.0)) && m.to_on_top);
    // PUSH from the right: both move left together.
    let m = f(T::Push, D::Right);
    assert!(eq(m.to_offset, (270.0, 0.0)) && eq(m.from_offset, (-90.0, 0.0)));
    // SLIDE_IN from the bottom: the origin moves up by 0.3 of its travel.
    let m = f(T::SlideIn, D::Bottom);
    assert!(eq(m.to_offset, (0.0, 390.0)) && eq(m.from_offset, (0.0, -39.0)) && m.to_on_top);
    // MOVE_OUT to the top: the origin leaves upward over a fixed destination.
    let m = f(T::MoveOut, D::Top);
    assert!(eq(m.from_offset, (0.0, -130.0)) && eq(m.to_offset, (0.0, 0.0)) && !m.to_on_top);
    // SLIDE_OUT to the left: the destination comes from 0.3 of the travel on the right.
    let m = f(T::SlideOut, D::Left);
    assert!(eq(m.from_offset, (-90.0, 0.0)) && eq(m.to_offset, (81.0, 0.0)) && !m.to_on_top);
    // Ends: at e = 1 the destination is at rest; non-directional types have no placement.
    assert!(eq(directional_frame(T::Push, D::Left, 1.0, w, h).unwrap().to_offset, (0.0, 0.0)));
    assert!(directional_frame(T::SmartAnimate, D::Left, e, w, h).is_none());
}

fn grad(kind: &str, stops: &[(f64, [f64; 4])], transform: [f64; 6]) -> SmartGradient {
    SmartGradient { kind: kind.into(), stops: stops.to_vec(), transform }
}

fn base() -> SmartProps {
    SmartProps { x: 0.0, y: 0.0, width: 10.0, height: 10.0, rotation_deg: 0.0, opacity: 1.0, fill: None, corner_radius: 0.0, gradient: None }
}

fn eq4(a: [f64; 4], b: [f64; 4]) -> bool {
    (0..4).all(|i| close(a[i], b[i], 1e-12))
}

#[test]
fn gradient_stops_and_colours_tween() {
    const ID: [f64; 6] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
    // Same stop count: stop by stop. e = 0.25: positions 0 -> 0.2 = 0.05, 1 -> 0.8 = 0.95.
    let a = SmartProps { gradient: Some(grad("GRADIENT_LINEAR", &[(0.0, [1.0, 0.0, 0.0, 1.0]), (1.0, [0.0, 0.0, 1.0, 1.0])], ID)), ..base() };
    let b = SmartProps {
        gradient: Some(grad("GRADIENT_LINEAR", &[(0.2, [0.0, 1.0, 0.0, 1.0]), (0.8, [1.0, 1.0, 1.0, 0.0])], [2.0, 0.0, 10.0, 0.0, 2.0, 20.0])),
        ..base()
    };
    let g = interpolate(&a, &b, 0.25).gradient.unwrap();
    assert!(close(g.stops[0].0, 0.05, 1e-12) && close(g.stops[1].0, 0.95, 1e-12));
    assert!(eq4(g.stops[0].1, [0.75, 0.25, 0.0, 1.0]) && eq4(g.stops[1].1, [0.25, 0.25, 1.0, 0.75]), "{g:?}");
    assert_eq!(g.transform, [1.25, 0.0, 2.5, 0.0, 1.25, 5.0]);
    // Different counts: resampled at the union {0, 0.5, 1}; black->white sampled at 0.5 is grey.
    let a = SmartProps { gradient: Some(grad("GRADIENT_LINEAR", &[(0.0, [0.0, 0.0, 0.0, 1.0]), (1.0, [1.0, 1.0, 1.0, 1.0])], ID)), ..base() };
    let rgb = [(0.0, [1.0, 0.0, 0.0, 1.0]), (0.5, [0.0, 1.0, 0.0, 1.0]), (1.0, [0.0, 0.0, 1.0, 1.0])];
    let b = SmartProps { gradient: Some(grad("GRADIENT_LINEAR", &rgb, ID)), ..base() };
    let g = interpolate(&a, &b, 0.5).gradient.unwrap();
    let pos: Vec<f64> = g.stops.iter().map(|s| s.0).collect();
    assert_eq!(pos, [0.0, 0.5, 1.0]);
    assert!(eq4(g.stops[0].1, [0.5, 0.0, 0.0, 1.0]) && eq4(g.stops[1].1, [0.25, 0.75, 0.25, 1.0]) && eq4(g.stops[2].1, [0.5, 0.5, 1.0, 1.0]));
    // Solid white against the gradient: a uniform white gradient on the gradient's stops.
    let solid = SmartProps { fill: Some([1.0, 1.0, 1.0, 1.0]), ..base() };
    let p = interpolate(&solid, &b, 0.5);
    let g = p.gradient.unwrap();
    assert!(p.fill.is_none() && g.stops.len() == 3 && eq4(g.stops[0].1, [1.0, 0.5, 0.5, 1.0]) && eq4(g.stops[1].1, [0.5, 1.0, 0.5, 1.0]));
    // Linear vs radial does not tween: the frame cross-dissolves the two fills.
    let radial = SmartProps { gradient: Some(grad("GRADIENT_RADIAL", &rgb, ID)), ..base() };
    assert!(!gradients_tween(&a, &radial));
    let layer = |id: &str, p: SmartProps| SmartLayer {
        id: id.into(), name: "fill".into(), parent: None, implicit_text_style: None, content: None, is_text: false, props: p,
    };
    let (fa, fb) = (SmartFrame { frame_id: "A".into(), layers: vec![layer("a", a.clone())] }, SmartFrame { frame_id: "B".into(), layers: vec![layer("b", radial)] });
    let d = smart_animate_frame(&fa, &fb, &match_layers(&fa, &fb).unwrap(), 0.5).unwrap();
    assert_eq!(d[0].side, SmartSide::ContentOut { from: "a".into() });
    assert_eq!((d[0].props.gradient.as_ref().unwrap().kind.as_str(), d[0].props.opacity), ("GRADIENT_LINEAR", 0.5));
    assert_eq!((d[1].props.gradient.as_ref().unwrap().kind.as_str(), d[1].props.opacity), ("GRADIENT_RADIAL", 0.5));
}

fn text(id: &str, parent: Option<&str>, content: &str, x: f64) -> SmartLayer {
    SmartLayer {
        id: id.into(),
        name: content.into(),
        parent: parent.map(Into::into),
        implicit_text_style: None,
        content: Some(content.into()),
        is_text: true,
        props: SmartProps { x, ..base() },
    }
}

fn group(id: &str, name: &str) -> SmartLayer {
    SmartLayer { id: id.into(), name: name.into(), parent: None, implicit_text_style: None, content: None, is_text: false, props: base() }
}

#[test]
fn text_by_content_rule_is_opt_in_and_typed() {
    // Levenshtein similarity: kitten -> sitting is 3 edits over 7.
    assert!(close(text_similarity("kitten", "sitting"), 1.0 - 3.0 / 7.0, 1e-12));
    assert_eq!((text_similarity("", ""), text_similarity("abc", "")), (1.0, 0.0));
    let from = SmartFrame {
        frame_id: "A".into(),
        layers: vec![
            group("a-card", "card"),
            text("a-price", Some("a-card"), "Price: 10", 0.0),
            text("a-total", Some("a-card"), "Total", 0.0),
            text("a-loose", None, "Price: 10", 0.0),
        ],
    };
    let to = SmartFrame {
        frame_id: "B".into(),
        layers: vec![group("b-card", "card"), text("b-total", Some("b-card"), "Totals", 0.0), text("b-price", Some("b-card"), "Price: 12", 0.0)],
    };
    // Default (spec rule): explicitly named texts with different names never pair.
    let m = match_layers(&from, &to).unwrap();
    assert_eq!(m.pairs, [("a-card".to_owned(), "b-card".to_owned())]);
    assert!(m.text_similarity_pairs.is_empty());
    // On: same ancestor path, best similarity first (8/9 then 5/6); the loose text has another path.
    let on = SmartMatchOptions { text_content_min_similarity: Some(0.0) };
    let m = match_layers_with(&from, &to, &on).unwrap();
    let pairs: Vec<(&str, &str)> = m.pairs.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    assert_eq!(pairs, [("a-card", "b-card"), ("a-total", "b-total"), ("a-price", "b-price")]);
    assert_eq!(m.text_similarity_pairs.len(), 2);
    assert!(close(m.text_similarity_pairs[0].2, 1.0 - 1.0 / 9.0, 1e-12) && close(m.text_similarity_pairs[1].2, 1.0 - 1.0 / 6.0, 1e-12));
    assert_eq!(m.from_only, ["a-loose"]);
    // A threshold above 5/6 keeps only the price pair.
    let m = match_layers_with(&from, &to, &SmartMatchOptions { text_content_min_similarity: Some(0.85) }).unwrap();
    assert_eq!(m.text_similarity_pairs.len(), 1);
    assert_eq!(m.to_only, ["b-total"]);
    // Bounds: an out-of-range threshold is refused; 65 x 65 candidates score at most the budget.
    assert!(match_layers_with(&from, &to, &SmartMatchOptions { text_content_min_similarity: Some(1.5) }).is_err());
    let many = |p: &str| SmartFrame { frame_id: p.into(), layers: (0..65).map(|i| text(&format!("{p}{i}"), None, &format!("t{p}{i}"), 0.0)).collect() };
    let m = match_layers_with(&many("x"), &many("y"), &on).unwrap();
    assert_eq!(m.text_pairs_not_scored, 65 * 65 - MAX_TEXT_PAIR_EVALUATIONS);
}
