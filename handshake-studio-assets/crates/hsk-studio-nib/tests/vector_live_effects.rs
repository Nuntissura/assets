//! Vector live effects (lane SMB-AI-DISTORT-EFFECTS-1): STU-VEC-154/155/158 typed records and the
//! Distort & Transform operators of STU-FX-028. Test names carry the MT recipe names:
//! MT-32980 vector_free_distort_frame, MT-32981 vector_pucker_bloat_live, MT-32982
//! vector_roughen_live, MT-32983 vector_step_repeat_live, MT-32984 vector_tweak_live, MT-32985
//! vector_twist_live, MT-32986 vector_zig_zag_live, MT-30559 shape_mosaic_gap_contract.
//! Oracles are computed here independently of the module (own homography solve, own rotation,
//! own normal displacement); laws are the declared Studio readings v1 (Illustrator parity
//! UNVERIFIED, D5).
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_nib::arrange::unit_random;
use hsk_studio_nib::live_effect::*;
use hsk_studio_nib::{FillRule, OwnedPath, PathElement as E, PathPoint};
use hsk_studio_observe::{Budget as ObserveBudget, DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort};

fn p(x: f64, y: f64) -> PathPoint {
    PathPoint::new(x, y)
}

fn path(elements: Vec<E>) -> OwnedPath {
    let mut o = OwnedPath::new(FillRule::NonZero);
    o.elements = elements;
    o
}

fn rect(x0: f64, y0: f64, w: f64, h: f64) -> OwnedPath {
    path(vec![E::MoveTo(p(x0, y0)), E::LineTo(p(x0 + w, y0)), E::LineTo(p(x0 + w, y0 + h)), E::LineTo(p(x0, y0 + h)), E::Close])
}

fn line(a: PathPoint, b: PathPoint) -> OwnedPath {
    path(vec![E::MoveTo(a), E::LineTo(b)])
}

fn go(sources: &[OwnedPath], record: &EffectRecord) -> Vec<OwnedPath> {
    evaluate(sources, record, &CancellationToken::default()).expect("evaluate")
}

fn err(sources: &[OwnedPath], record: &EffectRecord) -> LiveEffectError {
    evaluate(sources, record, &CancellationToken::default()).expect_err("refused")
}

/// End points of every element (MoveTo/LineTo/CubicTo end), in order.
fn anchors(o: &OwnedPath) -> Vec<PathPoint> {
    o.elements
        .iter()
        .filter_map(|e| match *e {
            E::MoveTo(q) | E::LineTo(q) | E::QuadTo(_, q) | E::CubicTo(_, _, q) => Some(q),
            E::Close => None,
        })
        .collect()
}

fn near(a: PathPoint, b: PathPoint, tol: f64) -> bool {
    (a.x - b.x).abs() <= tol && (a.y - b.y).abs() <= tol
}

fn signed_area(o: &OwnedPath) -> f64 {
    let a = anchors(o);
    let mut s = 0.0;
    for i in 0..a.len() {
        let (u, v) = (a[i], a[(i + 1) % a.len()]);
        s += u.x * v.y - v.x * u.y;
    }
    s / 2.0
}

fn bez(c: [PathPoint; 4], t: f64) -> PathPoint {
    let u = 1.0 - t;
    let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
    p(c.iter().zip(w).map(|(q, k)| q.x * k).sum(), c.iter().zip(w).map(|(q, k)| q.y * k).sum())
}

fn record(kind: EffectKind, sets: &[(&str, ParamValue)]) -> EffectRecord {
    let mut r = EffectRecord::new(kind);
    for (k, v) in sets {
        r.set(k, v.clone()).unwrap_or_else(|e| panic!("{k}: {e}"));
    }
    r
}

fn real(v: f64) -> ParamValue {
    ParamValue::Real(v)
}
fn tok(v: &str) -> ParamValue {
    ParamValue::String(v.to_owned())
}

#[test]
fn live_effect_typed_parameter_map() {
    // STU-VEC-154: stable ids, version stamp, typed map.
    for kind in EffectKind::ALL {
        assert!(kind.effect_id().starts_with(EFFECT_ID_PREFIX));
        assert_eq!(EffectKind::from_effect_id(kind.effect_id()), Some(kind));
        let r = EffectRecord::new(kind);
        assert_eq!((r.version(), r.effect_id()), (RECORD_VERSION, kind.effect_id()));
    }
    assert_eq!(EffectKind::Roughen.effect_id(), "hsk.studio.vector_effect.roughen");
    // STU-VEC-155 roughen rows, cell for cell; absolute hard and slider ranges kept apart.
    let ParamKind::Numeric(abs) = EffectKind::Roughen.param("roughen_size_absolute").unwrap().kind else { panic!() };
    assert_eq!(
        (abs.ty, abs.hard_min, abs.hard_max, abs.soft_min, abs.soft_max, abs.default, abs.unit, abs.precision),
        (ParamType::Real, Some(0.0), Some(7200.0), Some(0.0), Some(100.0), Some(1.0), Unit::DocumentUnit, Some(2))
    );
    let ParamKind::Numeric(rel) = EffectKind::Roughen.param("roughen_size_relative").unwrap().kind else { panic!() };
    assert_eq!((rel.hard_min, rel.hard_max, rel.soft_min, rel.soft_max, rel.unit), (None, None, Some(0.0), Some(100.0), Unit::Percent));
    let ParamKind::Numeric(det) = EffectKind::Roughen.param("roughen_detail").unwrap().kind else { panic!() };
    assert_eq!((det.hard_min, det.hard_max, det.default, det.unit.token()), (None, None, Some(1.0), "per_in"));
    let ParamKind::Enum(mode) = EffectKind::Roughen.param("roughen_mode").unwrap().kind else { panic!() };
    assert_eq!(mode.default, Some("mode.relative"));
    assert!(mode.default_is_studio_decision);
    assert_eq!(mode.members[1], EnumMember { id: "mode.absolute", captured: Some(Captured::Bool(true)) });
    assert_eq!(EffectKind::Roughen.param("seed").unwrap().provenance, Provenance::StudioDeclared);
    assert_eq!(EffectKind::Roughen.param("roughen_points").unwrap().provenance, Provenance::Recovered("STU-VEC-155"));
    // Fresh record: known defaults only; unknown defaults absent.
    let mut r = EffectRecord::new(EffectKind::Roughen);
    assert_eq!(r.get("roughen_mode"), Some(&tok("mode.relative")));
    assert_eq!(r.get("roughen_size_absolute"), Some(&real(1.0)));
    assert_eq!(r.get("roughen_points"), None);
    // Hard bounds refuse, soft bounds never clamp, unknown bounds accept.
    assert!(r.set_real("roughen_size_absolute", 7200.0).is_ok());
    assert_eq!(r.set_real("roughen_size_absolute", 7200.5), Err(LiveEffectError::OutOfHardRange));
    assert_eq!(r.set_real("roughen_size_absolute", -0.01), Err(LiveEffectError::OutOfHardRange));
    r.set_real("roughen_size_absolute", 500.0).unwrap();
    assert_eq!(r.get("roughen_size_absolute"), Some(&real(500.0)));
    r.set_real("roughen_size_relative", 1.0e6).unwrap();
    r.set_real("roughen_detail", -3.0).unwrap();
    // Typed refusals.
    assert_eq!(r.set("roughen_detail", ParamValue::Int(3)), Err(LiveEffectError::WrongParameterType));
    assert_eq!(r.set_real("roughen_amount", 1.0), Err(LiveEffectError::UnknownParameter));
    assert_eq!(r.set_real("roughen_detail", f64::NAN), Err(LiveEffectError::NonFinite));
    assert_eq!(r.set_real("roughen_detail", f64::INFINITY), Err(LiveEffectError::NonFinite));
    assert_eq!(r.set_token("roughen_points", "points.wavy"), Err(LiveEffectError::UnknownEnumMember));
    assert_eq!(r.set_triple("2", "Float", "roughen_detail"), Err(LiveEffectError::UntypedWrite));
    assert_eq!(r.set_triple("2", "", "roughen_detail"), Err(LiveEffectError::UntypedWrite));
    assert_eq!(r.set_triple("two", "Real", "roughen_detail"), Err(LiveEffectError::MalformedValue));
    assert_eq!(r.set_triple("2", "Int", "roughen_detail"), Err(LiveEffectError::WrongParameterType));
    r.set_triple("2.5", "Real", "roughen_detail").unwrap();
    r.set_triple("points.corner", "String", "roughen_points").unwrap();
    assert_eq!(r.get("roughen_detail"), Some(&real(2.5)));
    // Ordered map; decode round trip; version and id refusals.
    let keys: Vec<&str> = r.entries().map(|(k, _)| k).collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(keys, sorted);
    let entries: Vec<(&str, ParamValue)> = r.entries().map(|(k, v)| (k, v.clone())).collect();
    assert_eq!(EffectRecord::from_parts(r.effect_id(), 1, &entries).unwrap(), r);
    assert_eq!(EffectRecord::from_parts(r.effect_id(), 2, &entries), Err(LiveEffectError::UnsupportedVersion));
    assert_eq!(EffectRecord::from_parts("hsk.studio.vector_effect.scribble", 1, &[]), Err(LiveEffectError::UnknownEffect));
    // Tweak: three booleans default true, amounts default 10 with unknown hard bounds, no mode default.
    let t = EffectRecord::new(EffectKind::Tweak);
    for k in ["modify_anchor_points", "modify_in_control_points", "modify_out_control_points"] {
        assert_eq!(t.get(k), Some(&ParamValue::Bool(true)), "{k}");
    }
    assert_eq!((t.get("tweak_horizontal"), t.get("tweak_mode")), (Some(&real(10.0)), None));
    let ParamKind::Numeric(th) = EffectKind::Tweak.param("tweak_horizontal").unwrap().kind else { panic!() };
    assert_eq!((th.hard_min, th.hard_max, th.soft_max, th.unit), (None, None, Some(100.0), Unit::Context("tweak_mode")));
    // Zig zag ridges are counts; everything else unknown.
    let mut z = EffectRecord::new(EffectKind::ZigZag);
    assert_eq!(z.entries().count(), 0);
    assert_eq!(z.set_real("zigzag_ridges_per_segment", 2.5), Err(LiveEffectError::NotACount));
    assert_eq!(z.set_real("zigzag_ridges_per_segment", -1.0), Err(LiveEffectError::NotACount));
    z.set_real("zigzag_size", -1.0e8).unwrap();
    // Free distort: 16 typed corner reals, all unknown; quad-to-quad writer.
    assert_eq!(EffectKind::FreeDistort.params().len(), 16);
    for spec in EffectKind::FreeDistort.params() {
        let ParamKind::Numeric(n) = spec.kind else { panic!() };
        assert_eq!((n.hard_min, n.hard_max, n.soft_min, n.soft_max, n.default, n.precision), (None, None, None, None, None, None));
    }
    let mut f = EffectRecord::new(EffectKind::FreeDistort);
    f.set_free_distort_frame([[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]], [[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]]).unwrap();
    assert_eq!((f.get("src2_h"), f.get("dst2_v"), f.entries().count()), (Some(&real(1.0)), Some(&real(2.0)), 16));
    assert_eq!(f.set_free_distort_frame([[f64::NAN, 0.0]; 4], [[0.0, 0.0]; 4]), Err(LiveEffectError::NonFinite));
    assert_eq!(f.get("src0_h"), Some(&real(0.0)), "failed frame write leaves the record unchanged");
    // Step-repeat (lane SMB-AI-LIVE-EFFECTS-1): STU-VEC-157 recovers the shared transform parameter
    // set, cell for cell; only the seed (STU-FX-011) stays studio_declared.
    let stu157 = Provenance::Recovered("STU-VEC-157");
    assert_eq!(EffectKind::StepRepeat.provenance(), stu157);
    let row = |key: &str| {
        let spec = EffectKind::StepRepeat.param(key).unwrap_or_else(|| panic!("{key}"));
        assert_eq!(spec.provenance, stu157, "{key}");
        let ParamKind::Numeric(n) = spec.kind else { panic!("{key}") };
        n
    };
    for key in ["scale_horizontal", "scale_vertical"] {
        let n = row(key);
        assert_eq!(
            (n.ty, n.hard_min, n.hard_max, n.soft_min, n.soft_max, n.unit, n.precision),
            (ParamType::Real, Some(-4000.0), Some(4000.0), Some(0.0), Some(200.0), Unit::Percent, Some(2)),
            "{key}"
        );
        // Lead ruling Q2 2026-11-08: the spec cell reads 1.0; Studio keeps the identity 100 percent.
        assert_eq!((n.default, n.default_is_studio_decision), (Some(100.0), true), "{key}");
    }
    for key in ["move_horizontal", "move_vertical"] {
        let n = row(key);
        assert_eq!(
            (n.ty, n.hard_min, n.hard_max, n.soft_min, n.soft_max, n.unit, n.precision),
            (ParamType::Real, Some(-4000.0), Some(4000.0), Some(-100.0), Some(100.0), Unit::DocumentUnit, Some(4)),
            "{key}"
        );
        assert_eq!((n.default, n.default_is_studio_decision), (Some(0.0), true), "{key}");
    }
    let rotate = row("rotate");
    assert_eq!(
        (rotate.hard_min, rotate.hard_max, rotate.soft_min, rotate.soft_max, rotate.default, rotate.unit, rotate.precision, rotate.default_is_studio_decision),
        (None, None, None, None, Some(0.0), Unit::Deg, None, false)
    );
    let copies = row("copies");
    assert_eq!(
        (copies.ty, copies.hard_min, copies.hard_max, copies.soft_min, copies.soft_max, copies.default, copies.unit, copies.precision),
        (ParamType::Int, None, None, None, None, Some(0.0), Unit::Count, Some(0))
    );
    for key in ["reflect_x", "reflect_y", "randomize"] {
        let spec = EffectKind::StepRepeat.param(key).unwrap_or_else(|| panic!("{key}"));
        assert_eq!((spec.kind, spec.provenance), (ParamKind::Bool { default: Some(false) }, stu157), "{key}");
    }
    assert_eq!(EffectKind::StepRepeat.param("random"), None, "the spec key is `randomize`");
    assert_eq!(EffectKind::StepRepeat.param("reference_point").map(|s| s.provenance), Some(stu157));
    assert_eq!(EffectKind::StepRepeat.param("seed").map(|s| s.provenance), Some(Provenance::StudioDeclared));
    let mut s = EffectRecord::new(EffectKind::StepRepeat);
    assert_eq!((s.get("scale_horizontal"), s.get("move_vertical"), s.get("randomize")), (Some(&real(100.0)), Some(&real(0.0)), Some(&ParamValue::Bool(false))));
    // The edit range is wider than the slider and refuses past it; never clamped.
    s.set_real("scale_horizontal", -4000.0).unwrap();
    s.set_real("move_vertical", 250.0).unwrap();
    assert_eq!(s.set_real("scale_vertical", 4000.5), Err(LiveEffectError::OutOfHardRange));
    assert_eq!(s.set_real("move_horizontal", -4000.01), Err(LiveEffectError::OutOfHardRange));
    assert_eq!(s.set_bool("random", true), Err(LiveEffectError::UnknownParameter));
    // An unset unknown-default parameter refuses evaluation, never a guessed default.
    assert_eq!(err(&[rect(0.0, 0.0, 10.0, 10.0)], &EffectRecord::new(EffectKind::PuckerBloat)), LiveEffectError::ParameterUnset);
    for kind in [EffectKind::Roughen, EffectKind::ZigZag, EffectKind::Twist, EffectKind::Tweak, EffectKind::FreeDistort] {
        assert_eq!(err(&[rect(0.0, 0.0, 10.0, 10.0)], &EffectRecord::new(kind)), LiveEffectError::ParameterUnset, "{kind:?}");
    }
}

#[test]
fn shape_mosaic_gap_contract() {
    // STU-VEC-158 convert to shape: typed tokens with captured values; sizing law not recovered.
    let c = EffectRecord::new(EffectKind::ConvertToShape);
    assert_eq!((c.get("sizing_mode"), c.get("target_shape")), (Some(&tok("sizing.absolute")), None));
    let ParamKind::Enum(shape) = EffectKind::ConvertToShape.param("target_shape").unwrap().kind else { panic!() };
    let ids: Vec<(&str, Option<Captured>)> = shape.members.iter().map(|m| (m.id, m.captured)).collect();
    assert_eq!(
        ids,
        [
            ("shape.rectangle", Some(Captured::Token("Rectangle"))),
            ("shape.rounded_rectangle", Some(Captured::Token("RoundedRectangle"))),
            ("shape.ellipse", Some(Captured::Token("Ellipse"))),
        ]
    );
    let mut c = c;
    for shape in ["shape.rectangle", "shape.rounded_rectangle", "shape.ellipse"] {
        for sizing in ["sizing.absolute", "sizing.relative"] {
            c.set_token("target_shape", shape).unwrap();
            c.set_token("sizing_mode", sizing).unwrap();
            let entries: Vec<(&str, ParamValue)> = c.entries().map(|(k, v)| (k, v.clone())).collect();
            assert_eq!(EffectRecord::from_parts(c.effect_id(), 1, &entries).unwrap(), c);
            let refused = evaluate(&[rect(0.0, 0.0, 10.0, 10.0)], &c, &CancellationToken::default());
            assert_eq!(refused, Err(LiveEffectError::ConvertToShapeSizingLawNotRecovered));
            assert_eq!(outcome(&refused), Outcome::Failure(FailureCode::Unsupported));
        }
    }
    for gap in ["width", "height", "extra_width", "extra_height"] {
        assert_eq!(c.set_real(gap, 10.0), Err(LiveEffectError::ParameterNotRecovered), "{gap}");
    }
    assert_eq!(LiveEffectError::ConvertToShapeSizingLawNotRecovered.code(), "convert_to_shape_sizing_law_not_recovered");
    // Object mosaic: members and independent flags round trip; counts/spacing/resize not recovered.
    let mut m = EffectRecord::new(EffectKind::ObjectMosaic);
    assert_eq!(m.entries().count(), 0, "no recovered defaults");
    m.set_token("constrain_axis", "axis.height").unwrap();
    m.set_token("tile_colour_mode", "mosaic.gray").unwrap();
    m.set_bool("resize_using_percentages", true).unwrap();
    m.set_bool("delete_source_raster", false).unwrap();
    assert_eq!(m.set_token("tile_colour_mode", "mosaic.grey"), Err(LiveEffectError::UnknownEnumMember));
    assert_eq!(m.set("delete_source_raster", ParamValue::Int(1)), Err(LiveEffectError::WrongParameterType));
    for gap in ["tile_counts", "tile_spacing", "resize_numerics"] {
        assert_eq!(m.set_int(gap, 4), Err(LiveEffectError::ParameterNotRecovered), "{gap}");
    }
    assert_eq!(err(&[rect(0.0, 0.0, 10.0, 10.0)], &m), LiveEffectError::ObjectMosaicGridLawNotRecovered);
}

fn roughen_line(size: f64, detail: f64, seed: i64, points: &str) -> EffectRecord {
    record(
        EffectKind::Roughen,
        &[
            ("roughen_mode", tok("mode.absolute")),
            ("roughen_size_absolute", real(size)),
            ("roughen_detail", real(detail)),
            ("roughen_points", tok(points)),
            ("seed", ParamValue::Int(seed)),
        ],
    )
}

#[test]
fn vector_roughen_live() {
    let src = [line(p(0.0, 0.0), p(144.0, 0.0))];
    let before = src.clone();
    // 2 per inch over 2 inches: 4 pieces, 5 points; normal of +x is +y.
    let out = go(&src, &roughen_line(3.0, 2.0, 7, "points.corner"));
    assert_eq!(src, before, "source never mutated");
    let a = anchors(&out[0]);
    assert_eq!(a.len(), 5);
    assert!(matches!(out[0].elements[..], [E::MoveTo(_), E::LineTo(_), E::LineTo(_), E::LineTo(_), E::LineTo(_)]));
    assert_eq!((a[0], a[4]), (p(0.0, 0.0), p(144.0, 0.0)), "open end anchors stay");
    for (k, q) in a.iter().enumerate().take(4).skip(1) {
        let sign = if k % 2 == 0 { 1.0 } else { -1.0 };
        let d = 3.0 * sign * unit_random(7, 0, k as u64);
        assert!(q.x == 36.0 * k as f64 && (q.y - d).abs() <= 1e-12, "point {k}: {q:?}");
        assert!(d.abs() <= 3.0);
    }
    assert!(a[1].y <= 0.0 && a[2].y >= 0.0 && a[3].y <= 0.0, "alternating peaks and valleys");
    // Seeded: same seed identical, another seed differs.
    assert_eq!(go(&src, &roughen_line(3.0, 2.0, 7, "points.corner")), out);
    assert_ne!(go(&src, &roughen_line(3.0, 2.0, 8, "points.corner")), out);
    // Density per inch: 10 per inch over one inch = 10 pieces.
    assert_eq!(anchors(&go(&[line(p(0.0, 0.0), p(72.0, 0.0))], &roughen_line(3.0, 10.0, 7, "points.corner"))[0]).len(), 11);
    // Detail at or below zero adds nothing: only the fixed ends remain.
    assert_eq!(go(&src, &roughen_line(3.0, 0.0, 7, "points.corner")), src.to_vec());
    assert_eq!(go(&src, &roughen_line(3.0, -5.0, 7, "points.corner")), src.to_vec());
    // Smooth: Catmull-Rom cubics through the same points.
    let smooth = go(&src, &roughen_line(3.0, 2.0, 7, "points.smooth"));
    assert!(smooth[0].elements[1..].iter().all(|e| matches!(e, E::CubicTo(..))));
    assert_eq!(anchors(&smooth[0]), a);
    // Relative size: percent of the bounds diagonal (144 here).
    let mut rel = roughen_line(3.0, 2.0, 7, "points.corner");
    rel.set_token("roughen_mode", "mode.relative").unwrap();
    rel.set_real("roughen_size_relative", 10.0).unwrap();
    let r = anchors(&go(&src, &rel)[0]);
    assert!((r[2].y - 10.0 / 100.0 * 144.0 * unit_random(7, 0, 2)).abs() <= 1e-12);
    // Closed subpaths move every point, the first anchor included.
    let sq = [rect(0.0, 0.0, 72.0, 72.0)];
    let rs = go(&sq, &roughen_line(3.0, 1.0, 7, "points.corner"));
    assert_ne!(anchors(&rs[0])[0], p(0.0, 0.0));
    assert!(matches!(rs[0].elements.last(), Some(E::Close)));
    // Re-evaluation starts from the edited source, never from the previous result.
    let edited = [line(p(0.0, 0.0), p(216.0, 0.0))];
    let rec = roughen_line(3.0, 2.0, 7, "points.corner");
    let fresh = go(&edited, &rec);
    assert_eq!(anchors(&fresh[0]).len(), 7);
    assert_ne!(go(&go(&edited, &rec), &rec), fresh);
    // Absolute size accepts its full hard range (7200) beyond the slider.
    assert!(evaluate(&src, &roughen_line(7200.0, 2.0, 7, "points.corner"), &CancellationToken::default()).is_ok());
    // Negative paths: unset point type, non-finite source, cancel.
    assert_eq!(err(&src, &EffectRecord::new(EffectKind::Roughen)), LiveEffectError::ParameterUnset);
    assert_eq!(err(&[line(p(0.0, 0.0), p(f64::NAN, 0.0))], &rec), LiveEffectError::InvalidGeometry);
    let cancel = CancellationToken::default();
    cancel.cancel();
    assert_eq!(evaluate(&src, &rec, &cancel), Err(LiveEffectError::Canceled));
}

fn zigzag(size: f64, ridges: f64, mode: &str, points: &str) -> EffectRecord {
    record(
        EffectKind::ZigZag,
        &[("zigzag_mode", tok(mode)), ("zigzag_size", real(size)), ("zigzag_ridges_per_segment", real(ridges)), ("zigzag_points", tok(points))],
    )
}

#[test]
fn vector_zig_zag_live() {
    let src = [line(p(0.0, 0.0), p(100.0, 0.0))];
    let out = go(&src, &zigzag(5.0, 3.0, "mode.absolute", "points.corner"));
    let want = [p(0.0, 0.0), p(25.0, 5.0), p(50.0, -5.0), p(75.0, 5.0), p(100.0, 0.0)];
    let got = anchors(&out[0]);
    assert_eq!(got.len(), want.len());
    assert!(got.iter().zip(want).all(|(g, w)| near(*g, w, 1e-12)), "{got:?}");
    // Uniform: six ridges, seven equal pieces, every interior point at the full size.
    let six = anchors(&go(&src, &zigzag(5.0, 6.0, "mode.absolute", "points.corner"))[0]);
    assert_eq!(six.len(), 8);
    for (k, q) in six.iter().enumerate().take(7).skip(1) {
        assert!((q.x - 100.0 * k as f64 / 7.0).abs() < 1e-9 && (q.y.abs() - 5.0).abs() < 1e-12, "{k}: {q:?}");
    }
    // Smooth vs corner: same points, cubic vs straight joins.
    let smooth = go(&src, &zigzag(5.0, 3.0, "mode.absolute", "points.smooth"));
    assert_eq!(anchors(&smooth[0]), anchors(&out[0]));
    assert!(smooth[0].elements[1..].iter().all(|e| matches!(e, E::CubicTo(..))));
    assert!(out[0].elements[1..].iter().all(|e| matches!(e, E::LineTo(..))));
    // Relative: percent of the bounds diagonal.
    let rel = anchors(&go(&src, &zigzag(10.0, 3.0, "mode.relative", "points.corner"))[0]);
    assert!((rel[1].y - 10.0).abs() < 1e-12);
    // Zero ridges keep the segment; source edits reevaluate without cascading.
    assert_eq!(anchors(&go(&src, &zigzag(5.0, 0.0, "mode.absolute", "points.corner"))[0]), vec![p(0.0, 0.0), p(100.0, 0.0)]);
    let rec = zigzag(5.0, 3.0, "mode.absolute", "points.corner");
    let edited = [line(p(0.0, 0.0), p(200.0, 0.0))];
    assert!(near(anchors(&go(&edited, &rec)[0])[1], p(50.0, 5.0), 1e-12));
    assert_ne!(go(&go(&edited, &rec), &rec), go(&edited, &rec));
    // Negative paths.
    assert_eq!(err(&src, &EffectRecord::new(EffectKind::ZigZag)), LiveEffectError::ParameterUnset);
    let mut no_points = zigzag(5.0, 3.0, "mode.absolute", "points.corner");
    assert_eq!(no_points.set_token("zigzag_points", "points.wavy"), Err(LiveEffectError::UnknownEnumMember));
    no_points = EffectRecord::new(EffectKind::ZigZag);
    no_points.set_token("zigzag_mode", "mode.absolute").unwrap();
    no_points.set_real("zigzag_size", 5.0).unwrap();
    no_points.set_real("zigzag_ridges_per_segment", 3.0).unwrap();
    assert_eq!(err(&src, &no_points), LiveEffectError::ParameterUnset);
    assert_eq!(err(&src, &zigzag(5.0, 2000.0, "mode.absolute", "points.corner")), LiveEffectError::OutputOverBudget);
    assert_eq!(err(&src, &zigzag(2.0e9, 3.0, "mode.absolute", "points.corner")), LiveEffectError::BeyondEngineRange);
}

#[test]
fn vector_pucker_bloat_live() {
    let src = [rect(0.0, 0.0, 100.0, 100.0)];
    let factor = |f: f64| record(EffectKind::PuckerBloat, &[("pucker_bloat_factor", real(f))]);
    // Independent expectation of the top edge (0,0)-(100,0), vertex centroid (50,50).
    let edge = |o: &OwnedPath| -> [PathPoint; 4] {
        let E::MoveTo(a) = o.elements[0] else { panic!() };
        let E::CubicTo(b, c, d) = o.elements[1] else { panic!("{:?}", o.elements) };
        [a, b, c, d]
    };
    let pucker = go(&src, &factor(-50.0));
    let e = edge(&pucker[0]);
    assert_eq!(e, [p(-25.0, -25.0), p(25.0, 25.0), p(75.0, 25.0), p(125.0, -25.0)]);
    assert_eq!(bez(e, 0.5), p(50.0, 12.5), "pucker: anchors outward, the edge pinched toward the centre");
    let bloat = go(&src, &factor(50.0));
    let e = edge(&bloat[0]);
    assert_eq!(e, [p(25.0, 25.0), p(-25.0, -25.0), p(125.0, -25.0), p(75.0, 25.0)]);
    assert_eq!(bez(e, 0.5), p(50.0, -12.5), "bloat: anchors inward, the edge bulging outward");
    let dist = |q: PathPoint| ((q.x - 50.0).powi(2) + (q.y - 50.0).powi(2)).sqrt();
    let corner = dist(p(0.0, 0.0));
    assert!(dist(anchors(&pucker[0])[0]) > corner && dist(anchors(&bloat[0])[0]) < corner);
    // Zero keeps every anchor; the source never changes; edits reevaluate from source.
    assert_eq!(anchors(&go(&src, &factor(0.0))[0])[..4], anchors(&src[0])[..4]);
    assert_eq!(src[0], rect(0.0, 0.0, 100.0, 100.0));
    let edited = [rect(0.0, 0.0, 200.0, 100.0)];
    assert_eq!(anchors(&go(&edited, &factor(-50.0))[0])[0], p(-50.0, -25.0));
    assert_ne!(go(&go(&edited, &factor(-50.0)), &factor(-50.0)), go(&edited, &factor(-50.0)));
    // Negative paths.
    assert_eq!(err(&src, &EffectRecord::new(EffectKind::PuckerBloat)), LiveEffectError::ParameterUnset);
    let mut r = EffectRecord::new(EffectKind::PuckerBloat);
    assert_eq!(r.set_real("pucker_bloat_factor", f64::NAN), Err(LiveEffectError::NonFinite));
    assert_eq!(err(&[line(p(0.0, 0.0), p(f64::INFINITY, 1.0))], &factor(10.0)), LiveEffectError::InvalidGeometry);
}

#[test]
fn vector_twist_live() {
    // Two crossing open lines: bounds centre (0,0), every anchor at R = 100.
    let src = [line(p(-100.0, 0.0), p(100.0, 0.0)), line(p(0.0, -100.0), p(0.0, 100.0))];
    let rec = |a: f64| record(EffectKind::Twist, &[("twist_angle", real(a))]);
    let out = go(&src, &rec(90.0));
    let oracle = |q: PathPoint, angle: f64| {
        let d = (q.x * q.x + q.y * q.y).sqrt();
        let th = (angle * (1.0 - d / 100.0).max(0.0)).to_radians();
        p(q.x * th.cos() - q.y * th.sin(), q.x * th.sin() + q.y * th.cos())
    };
    for (o, s) in out.iter().zip(&src) {
        let [E::MoveTo(a), E::LineTo(b)] = s.elements[..] else { panic!() };
        let [E::MoveTo(m), E::CubicTo(c1, c2, e)] = o.elements[..] else { panic!("{:?}", o.elements) };
        let third = |t: f64| p(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
        for (got, want) in [(m, a), (c1, third(1.0 / 3.0)), (c2, third(2.0 / 3.0)), (e, b)] {
            assert!(near(got, oracle(want, 90.0), 1e-9), "{got:?} vs {:?}", oracle(want, 90.0));
        }
        // The outermost anchors stay; the inner control points turn by two thirds of the angle.
        assert!(near(m, a, 1e-12) && near(e, b, 1e-12));
        let turned = |q: PathPoint, r: PathPoint| (q.x * r.y - q.y * r.x).atan2(q.x * r.x + q.y * r.y).to_degrees();
        assert!((turned(third(1.0 / 3.0), c1) - 60.0).abs() < 1e-9, "more rotation at the centre than at the edge");
    }
    // Not an ordinary rotation: the anchors did not turn while the inner points did.
    assert_ne!(out, go(&src, &rec(0.0)));
    assert_eq!(go(&src, &rec(90.0)), out, "deterministic");
    // Re-evaluation from an edited source.
    let edited = [line(p(-50.0, 0.0), p(150.0, 0.0)), src[1].clone()];
    assert_ne!(go(&go(&edited, &rec(90.0)), &rec(90.0)), go(&edited, &rec(90.0)));
    // Negative paths.
    assert_eq!(err(&src, &EffectRecord::new(EffectKind::Twist)), LiveEffectError::ParameterUnset);
    assert_eq!(err(&[line(p(5.0, 5.0), p(5.0, 5.0))], &rec(45.0)), LiveEffectError::DegenerateExtent);
    let mut bad = EffectRecord::new(EffectKind::Twist);
    assert_eq!(bad.set_real("twist_angle", f64::NEG_INFINITY), Err(LiveEffectError::NonFinite));
}

fn tweak_rec(mode: &str, h: f64, v: f64, flags: [bool; 3], seed: i64) -> EffectRecord {
    record(
        EffectKind::Tweak,
        &[
            ("tweak_mode", tok(mode)),
            ("tweak_horizontal", real(h)),
            ("tweak_vertical", real(v)),
            ("modify_anchor_points", ParamValue::Bool(flags[0])),
            ("modify_in_control_points", ParamValue::Bool(flags[1])),
            ("modify_out_control_points", ParamValue::Bool(flags[2])),
            ("seed", ParamValue::Int(seed)),
        ],
    )
}

#[test]
fn vector_tweak_live() {
    let src = [rect(0.0, 0.0, 100.0, 100.0)];
    let r = |stream: u64, k: u64| 2.0 * unit_random(3, stream, k) - 1.0;
    let off = |c: u64, k: u64| p(10.0 * r(c, k), 20.0 * r(c + 1, k));
    let out = go(&src, &tweak_rec("mode.absolute", 10.0, 20.0, [true; 3], 3));
    assert!(matches!(out[0].elements[..], [E::MoveTo(_), E::CubicTo(..), E::CubicTo(..), E::CubicTo(..), E::CubicTo(..), E::Close]));
    let E::MoveTo(m) = out[0].elements[0] else { panic!() };
    let E::CubicTo(c1, c2, e) = out[0].elements[1] else { panic!() };
    let (a0, a1) = (off(0, 0), off(0, 1));
    assert_eq!(m, p(0.0 + a0.x, 0.0 + a0.y));
    assert_eq!(e, p(100.0 + a1.x, 0.0 + a1.y));
    let (o0, i1) = (off(4, 0), off(2, 1));
    assert_eq!(c1, p((0.0 + a0.x) + o0.x, (0.0 + a0.y) + o0.y), "out handle follows its anchor plus its own offset");
    assert_eq!(c2, p((100.0 + a1.x) + i1.x, (0.0 + a1.y) + i1.y), "in handle follows its anchor plus its own offset");
    assert!(a0.x.abs() <= 10.0 && a0.y.abs() <= 20.0);
    // The closing segment ends on the moved first anchor.
    let E::CubicTo(_, _, last) = out[0].elements[4] else { panic!() };
    assert_eq!(last, m);
    // Anchors only: straight segments stay straight.
    let anchors_only = go(&src, &tweak_rec("mode.absolute", 10.0, 20.0, [true, false, false], 3));
    assert!(matches!(anchors_only[0].elements[..], [E::MoveTo(_), E::LineTo(_), E::LineTo(_), E::LineTo(_), E::Close]));
    assert_eq!(anchors(&anchors_only[0])[1], e);
    // Handles only: anchors stay, segments curve.
    let handles = go(&src, &tweak_rec("mode.absolute", 10.0, 20.0, [false, true, true], 3));
    assert_eq!(anchors(&handles[0])[..4], anchors(&src[0])[..4]);
    assert!(handles[0].elements.iter().any(|e| matches!(e, E::CubicTo(..))));
    // All switches off: identity.
    assert_eq!(go(&src, &tweak_rec("mode.absolute", 10.0, 20.0, [false; 3], 3)), src.to_vec());
    // Relative: horizontal percent of the width, vertical of the height (200 x 50 here).
    let wide = [rect(0.0, 0.0, 200.0, 50.0)];
    let rel = go(&wide, &tweak_rec("mode.relative", 10.0, 20.0, [true, false, false], 3));
    let E::MoveTo(q) = rel[0].elements[0] else { panic!() };
    assert_eq!(q, p(0.0 + 10.0 / 100.0 * 200.0 * r(0, 0), 0.0 + 20.0 / 100.0 * 50.0 * r(1, 0)));
    // Seeded replay; another seed differs; re-evaluation without accumulation.
    let rec = tweak_rec("mode.absolute", 10.0, 20.0, [true; 3], 3);
    assert_eq!(go(&src, &rec), out);
    assert_ne!(go(&src, &tweak_rec("mode.absolute", 10.0, 20.0, [true; 3], 4)), out);
    assert_ne!(go(&go(&src, &rec), &rec), out);
    // Negative paths: unset mode (no recovered default), unknown mode, non-finite amount.
    let mut no_mode = EffectRecord::new(EffectKind::Tweak);
    assert_eq!(err(&src, &no_mode), LiveEffectError::ParameterUnset);
    assert_eq!(no_mode.set_token("tweak_mode", "mode.inward"), Err(LiveEffectError::UnknownEnumMember));
    assert_eq!(no_mode.set_real("tweak_vertical", f64::NAN), Err(LiveEffectError::NonFinite));
    no_mode.set_real("tweak_vertical", 5000.0).unwrap();
    assert_eq!(no_mode.get("tweak_vertical"), Some(&real(5000.0)), "unknown hard bounds are not mirrored from the slider");
}

/// Independent homography: 8x8 Gaussian elimination with `h33 = 1`.
fn oracle_h(src: [[f64; 2]; 4], dst: [[f64; 2]; 4]) -> [f64; 9] {
    let mut m = [[0.0f64; 9]; 8];
    for i in 0..4 {
        let ([x, y], [u, v]) = (src[i], dst[i]);
        m[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
        m[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
    }
    for c in 0..8 {
        let piv = (c..8).max_by(|a, b| m[*a][c].abs().total_cmp(&m[*b][c].abs())).unwrap();
        m.swap(c, piv);
        let pivot = m[c];
        for (r, row) in m.iter_mut().enumerate() {
            if r != c {
                let f = row[c] / pivot[c];
                for (cell, pv) in row.iter_mut().zip(pivot).skip(c) {
                    *cell -= f * pv;
                }
            }
        }
    }
    let mut h = [0.0; 9];
    for i in 0..8 {
        h[i] = m[i][8] / m[i][i];
    }
    h[8] = 1.0;
    h
}

fn apply_h(h: &[f64; 9], q: PathPoint) -> (PathPoint, f64) {
    let w = h[6] * q.x + h[7] * q.y + h[8];
    (p((h[0] * q.x + h[1] * q.y + h[2]) / w, (h[3] * q.x + h[4] * q.y + h[5]) / w), w)
}

fn frame(src: [[f64; 2]; 4], dst: [[f64; 2]; 4]) -> EffectRecord {
    let mut r = EffectRecord::new(EffectKind::FreeDistort);
    r.set_free_distort_frame(src, dst).unwrap();
    r
}

#[test]
fn vector_free_distort_frame() {
    let sq = [[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]];
    let curve = path(vec![E::MoveTo(p(10.0, 50.0)), E::CubicTo(p(30.0, 0.0), p(70.0, 100.0), p(90.0, 50.0))]);
    let src = [rect(0.0, 0.0, 100.0, 100.0), line(p(0.0, 50.0), p(100.0, 50.0)), curve.clone()];
    // Unchanged frame: identity, byte for byte.
    assert_eq!(go(&src, &frame(sq, sq)), src.to_vec());
    // Each corner dragged on its own: the source square lands on the new frame exactly.
    let pushed = [[-20.0, -10.0], [120.0, -10.0], [120.0, 110.0], [-20.0, 110.0]];
    for i in 0..4 {
        let mut dst = sq;
        dst[i] = pushed[i];
        let out = go(&src[..1], &frame(sq, dst));
        let a = anchors(&out[0]);
        for k in 0..4 {
            assert!(near(a[k], p(dst[k][0], dst[k][1]), 1e-9), "corner {i}: {:?} vs {:?}", a[k], dst[k]);
        }
    }
    // A non-rectangular perspective frame against the independent solve.
    let dst = [[0.0, 0.0], [100.0, 0.0], [70.0, 60.0], [30.0, 60.0]];
    let h = oracle_h(sq, dst);
    let out = go(&src, &frame(sq, dst));
    for (o, s) in out.iter().zip(&src).take(2) {
        assert!(o.elements.iter().all(|e| !matches!(e, E::CubicTo(..))), "lines map to lines");
        for (got, want) in anchors(o).iter().zip(anchors(s)) {
            assert!(near(*got, apply_h(&h, want).0, 1e-9), "{got:?}");
        }
    }
    // The cubic: subdivided pieces stay within the declared tolerance of the exact image.
    let image: Vec<PathPoint> = (0..=4096).map(|i| apply_h(&h, bez([p(10.0, 50.0), p(30.0, 0.0), p(70.0, 100.0), p(90.0, 50.0)], f64::from(i) / 4096.0)).0).collect();
    let to_image = |q: PathPoint| {
        image
            .windows(2)
            .map(|w| {
                let (a, b) = (w[0], w[1]);
                let (dx, dy) = (b.x - a.x, b.y - a.y);
                let t = (((q.x - a.x) * dx + (q.y - a.y) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
                ((q.x - a.x - t * dx).powi(2) + (q.y - a.y - t * dy).powi(2)).sqrt()
            })
            .fold(f64::INFINITY, f64::min)
    };
    let mut prev = None;
    let mut pieces = 0;
    for e in &out[2].elements {
        match *e {
            E::MoveTo(q) => {
                assert!(near(q, apply_h(&h, p(10.0, 50.0)).0, 1e-9));
                prev = Some(q);
            }
            E::CubicTo(b, c, d) => {
                let a = prev.unwrap();
                for t in [0.25, 0.5, 0.75] {
                    assert!(to_image(bez([a, b, c, d], t)) <= FREE_DISTORT_TOLERANCE_PT + 1e-3, "piece {pieces} t {t}");
                }
                prev = Some(d);
                pieces += 1;
            }
            _ => panic!("{e:?}"),
        }
    }
    assert!(pieces >= 1);
    assert!(near(prev.unwrap(), apply_h(&h, p(90.0, 50.0)).0, 1e-9));
    // Re-evaluation from the edited source, never accumulating.
    let rec = frame(sq, dst);
    let edited = [rect(10.0, 10.0, 50.0, 50.0)];
    let fresh = go(&edited, &rec);
    for (got, want) in anchors(&fresh[0]).iter().zip(anchors(&edited[0])) {
        assert!(near(*got, apply_h(&h, want).0, 1e-9));
    }
    assert_ne!(go(&fresh, &rec), fresh);
    // Refusals before any output.
    let refuse = |dst: [[f64; 2]; 4]| err(&src, &frame(sq, dst));
    assert_eq!(refuse([[0.0, 0.0], [0.0, 0.0], [100.0, 100.0], [0.0, 100.0]]), LiveEffectError::DegenerateQuad);
    assert_eq!(refuse([[0.0, 0.0], [50.0, 0.0], [100.0, 0.0], [0.0, 100.0]]), LiveEffectError::DegenerateQuad);
    assert_eq!(refuse([[0.0, 0.0], [100.0, 0.0], [0.0, 100.0], [100.0, 100.0]]), LiveEffectError::SelfCrossingQuad);
    assert_eq!(refuse([[0.0, 0.0], [100.0, 0.0], [40.0, 40.0], [0.0, 100.0]]), LiveEffectError::QuadNotConvex);
    assert_eq!(err(&src, &frame([[0.0, 0.0], [100.0, 0.0], [0.0, 100.0], [100.0, 100.0]], dst)), LiveEffectError::SelfCrossingQuad);
    assert_eq!(err(&src, &EffectRecord::new(EffectKind::FreeDistort)), LiveEffectError::ParameterUnset);
    // A point on or beyond the vanishing line of the map is refused, never folded through infinity.
    let far = [-10.0, -50.0, -70.0, -100.0, -1000.0, -1.0e5, 150.0, 1000.0, 1.0e5]
        .into_iter()
        .map(|y| p(50.0, y))
        .find(|q| apply_h(&h, *q).1 <= 0.0)
        .expect("a point past the horizon");
    assert_eq!(err(&[line(p(50.0, 50.0), far)], &rec), LiveEffectError::PointBeyondHorizon);
}

fn step(sets: &[(&str, ParamValue)]) -> EffectRecord {
    record(EffectKind::StepRepeat, sets)
}

#[test]
fn vector_step_repeat_live() {
    let src = [rect(0.0, 0.0, 10.0, 10.0)];
    // Move: copy k is offset by k steps; the source comes first.
    let out = go(&src, &step(&[("copies", ParamValue::Int(3)), ("move_horizontal", real(20.0))]));
    assert_eq!(out.len(), 4);
    assert_eq!(out[0], src[0]);
    for (k, copy) in out.iter().enumerate().skip(1) {
        let want: Vec<PathPoint> = anchors(&src[0]).iter().map(|q| p(q.x + 20.0 * k as f64, q.y)).collect();
        assert_eq!(anchors(copy), want, "copy {k}");
        assert!(matches!(copy.elements.last(), Some(E::Close)));
    }
    // Rotate 90 about the centre: clockwise on the y-down page; four copies come back home.
    let rot = go(&src, &step(&[("copies", ParamValue::Int(4)), ("rotate", real(90.0))]));
    assert_eq!(anchors(&rot[1])[0], p(10.0, 0.0));
    for (a, b) in anchors(&rot[4]).iter().zip(anchors(&src[0])) {
        assert!(near(*a, b, 1e-12));
    }
    // Composition: copy 2 = T(T(q)) with an independent affine.
    let t = |q: PathPoint| {
        // rotate 30 about the bottom-right reference (10, 10), then move (5, -2).
        let (s, c) = 30.0f64.to_radians().sin_cos();
        let (x, y) = (q.x - 10.0, q.y - 10.0);
        p(10.0 + c * x - s * y + 5.0, 10.0 + s * x + c * y - 2.0)
    };
    let comp = go(
        &src,
        &step(&[
            ("copies", ParamValue::Int(2)),
            ("rotate", real(30.0)),
            ("move_horizontal", real(5.0)),
            ("move_vertical", real(-2.0)),
            ("reference_point", tok("ref.bottom_right")),
        ]),
    );
    for (got, q) in anchors(&comp[2]).iter().zip(anchors(&src[0])) {
        assert!(near(*got, t(t(q)), 1e-9), "{got:?} vs {:?}", t(t(q)));
    }
    // Reflect: mirrored across the vertical through the reference point, orientation reversed.
    let refl = go(&src, &step(&[("copies", ParamValue::Int(1)), ("reflect_x", ParamValue::Bool(true))]));
    assert_eq!(anchors(&refl[1])[0], p(10.0, 0.0));
    assert!(signed_area(&refl[1]) * signed_area(&src[0]) < 0.0);
    // Scale: geometric sequence about the centre.
    let scaled = go(&src, &step(&[("copies", ParamValue::Int(2)), ("scale_horizontal", real(50.0)), ("scale_vertical", real(50.0))]));
    assert_eq!(anchors(&scaled[2])[0], p(3.75, 3.75));
    // Random: each amount drawn between identity and the value, seeded.
    let rnd = step(&[("copies", ParamValue::Int(2)), ("move_horizontal", real(20.0)), ("randomize", ParamValue::Bool(true)), ("seed", ParamValue::Int(9))]);
    let r1 = go(&src, &rnd);
    assert_eq!(anchors(&r1[1])[0], p(20.0 * unit_random(9, 4, 1), 0.0));
    assert_eq!(go(&src, &rnd), r1);
    // Zero copies: the sources; edits reevaluate every copy from the source.
    assert_eq!(go(&src, &EffectRecord::new(EffectKind::StepRepeat)), src.to_vec());
    let edited = [rect(0.0, 0.0, 30.0, 10.0)];
    let moved = go(&edited, &step(&[("copies", ParamValue::Int(2)), ("move_horizontal", real(20.0))]));
    assert_eq!(anchors(&moved[2])[1], p(70.0, 0.0));
    // Negative paths: counts, malformed integers, budget before allocation.
    let mut r = EffectRecord::new(EffectKind::StepRepeat);
    assert_eq!(r.set_int("copies", -1), Err(LiveEffectError::NotACount));
    assert_eq!(r.set_triple("1.5", "Int", "copies"), Err(LiveEffectError::MalformedValue));
    assert_eq!(r.set_real("copies", 2.0), Err(LiveEffectError::WrongParameterType));
    assert_eq!(r.set_token("reference_point", "ref.middle"), Err(LiveEffectError::UnknownEnumMember));
    r.set_int("copies", 100_000).unwrap();
    assert_eq!(err(&src, &r), LiveEffectError::OutputOverBudget);
    // Within the STU-VEC-157 edit range, compounding copies still leave the engine range: refused.
    let huge = step(&[("copies", ParamValue::Int(3)), ("scale_horizontal", real(4000.0))]);
    assert_eq!(err(&[rect(0.0, 0.0, 1.0e6, 1.0e6)], &huge), LiveEffectError::BeyondEngineRange);
}

#[test]
fn live_effect_dims_u32_max() {
    let src = [line(p(0.0, 0.0), p(144.0, 0.0))];
    assert_eq!(err(&src, &zigzag(1.0, f64::from(u32::MAX), "mode.absolute", "points.corner")), LiveEffectError::OutputOverBudget);
    assert_eq!(err(&src, &roughen_line(1.0, 1.0e12, 1, "points.corner")), LiveEffectError::OutputOverBudget);
    assert_eq!(err(&src, &roughen_line(1.0, f64::MAX, 1, "points.corner")), LiveEffectError::OutputOverBudget);
    for copies in [i64::from(u32::MAX), i64::MAX] {
        assert_eq!(err(&src, &step(&[("copies", ParamValue::Int(copies))])), LiveEffectError::OutputOverBudget, "{copies}");
    }
    // Even with no geometry, an enormous copy count over a source list is refused, not looped.
    assert_eq!(err(&[OwnedPath::new(FillRule::NonZero)], &step(&[("copies", ParamValue::Int(i64::MAX))])), LiveEffectError::OutputOverBudget);
}

#[test]
fn live_effect_aggregate_over_budget() {
    // Five sources each within the per-path budget, together over it: refused before work.
    let long = |n: usize| {
        let mut e = vec![E::MoveTo(p(0.0, 0.0))];
        e.extend((1..n).map(|i| E::LineTo(p(i as f64, (i % 2) as f64))));
        path(e)
    };
    let many: Vec<OwnedPath> = (0..5).map(|_| long(60_000)).collect();
    let pb = record(EffectKind::PuckerBloat, &[("pucker_bloat_factor", real(10.0))]);
    assert_eq!(err(&many, &pb), LiveEffectError::OutputOverBudget);
    assert!(evaluate(&many[..1], &pb, &CancellationToken::default()).is_ok());
    // Roughen: each line alone fits, the running aggregate of both does not.
    let rec = roughen_line(1.0, 150.0, 1, "points.corner");
    let one = [line(p(0.0, 0.0), p(72_000.0, 0.0))];
    assert_eq!(anchors(&go(&one, &rec)[0]).len(), 150_001);
    let two = [one[0].clone(), line(p(0.0, 10.0), p(72_000.0, 10.0))];
    assert_eq!(err(&two, &rec), LiveEffectError::OutputOverBudget);
}

struct Collector(Vec<(DeliveryClass, Vec<u8>)>);

impl SinkPort for Collector {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push((class, bytes.to_vec()));
        Ok(())
    }
}

#[test]
fn live_effect_outcomes_through_observe() {
    let src = [rect(0.0, 0.0, 10.0, 10.0)];
    let cancel = CancellationToken::default();
    let stop = CancellationToken::default();
    stop.cancel();
    let pb = record(EffectKind::PuckerBloat, &[("pucker_bloat_factor", real(10.0))]);
    let cases = [
        (outcome(&evaluate(&src, &pb, &cancel)), Outcome::Success),
        (outcome(&evaluate(&src, &EffectRecord::new(EffectKind::PuckerBloat), &cancel)), Outcome::Failure(FailureCode::Validation)),
        (outcome(&evaluate(&src, &pb, &stop)), Outcome::Canceled),
        (outcome(&evaluate(&src, &EffectRecord::new(EffectKind::ConvertToShape), &cancel)), Outcome::Failure(FailureCode::Unsupported)),
        (outcome(&evaluate(&src, &step(&[("copies", ParamValue::Int(i64::MAX))]), &cancel)), Outcome::Failure(FailureCode::Unavailable)),
    ];
    let mut sink = Collector(Vec::new());
    for (n, (got, want)) in cases.into_iter().enumerate() {
        assert_eq!(got, want, "case {n}");
        let actor = ActorContext::new("a", "p", "o", "op", "s", "sess").unwrap();
        let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
        let mut observe = Observe::new(42, 7, resource, actor, ObserveBudget::new(0, 0).unwrap());
        let receipt = report(got, 42, 7, &mut observe, &cancel, &mut sink).unwrap();
        assert_eq!(receipt.outcome, want, "case {n}");
    }
    assert_eq!(sink.0.len(), 5);
    for (class, bytes) in &sink.0 {
        assert_eq!(*class, DeliveryClass::Terminal);
        assert!(bytes.starts_with(b"HSKO"));
        for text in [&b"pucker"[..], b"roughen", b"vector_effect", b"convert", b"copies"] {
            assert!(!bytes.windows(text.len()).any(|w| w == text), "no record text on the wire");
        }
    }
    // Every refusal has a distinct stable code.
    let codes = [
        LiveEffectError::UnknownEffect, LiveEffectError::UnsupportedVersion, LiveEffectError::UnknownParameter,
        LiveEffectError::ParameterNotRecovered, LiveEffectError::UntypedWrite, LiveEffectError::MalformedValue,
        LiveEffectError::WrongParameterType, LiveEffectError::NonFinite, LiveEffectError::OutOfHardRange,
        LiveEffectError::NotACount, LiveEffectError::UnknownEnumMember, LiveEffectError::ParameterUnset,
        LiveEffectError::InvalidGeometry, LiveEffectError::DegenerateExtent, LiveEffectError::DegenerateQuad,
        LiveEffectError::SelfCrossingQuad, LiveEffectError::QuadNotConvex, LiveEffectError::PointBeyondHorizon,
        LiveEffectError::BeyondEngineRange, LiveEffectError::OutputOverBudget, LiveEffectError::Canceled,
        LiveEffectError::ConvertToShapeSizingLawNotRecovered, LiveEffectError::ObjectMosaicGridLawNotRecovered,
    ]
    .map(LiveEffectError::code);
    let mut unique = codes.to_vec();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), codes.len());
}

#[test]
fn live_effect_descriptor_entry() {
    let d = hsk_studio_nib::descriptor();
    assert!(d.contains("\"vector_live_effects\":{\"lane\":\"SMB-AI-DISTORT-EFFECTS-1\""));
    for needle in ["STU-VEC-155", "STU-VEC-158", "convert_to_shape_sizing_law_not_recovered", "tests/vector_live_effects.rs", "UNVERIFIED", "gpu none"] {
        assert!(d.contains(needle), "{needle}");
    }
    // Lane SMB-AI-LIVE-EFFECTS-1: the STU-VEC-157 rows, the `randomize` key and the Studio reading
    // of the scale and move defaults beside the spec cell.
    for needle in ["STU-VEC-157", "randomize", "spec cell 1.0", "MT-30558 stays open"] {
        assert!(d.contains(needle), "{needle}");
    }
    assert!(!d.contains("random false"), "the old key is gone from the descriptor");
}

#[test]
fn live_effects_keep_the_fill_rule() {
    // Lane SMB-AI-LIVE-EFFECTS-1 (V4-06 class: a law that rebuilds the path must keep its fill rule):
    // every evaluating effect over an even-odd source returns even-odd paths.
    let mut sq = rect(0.0, 0.0, 40.0, 20.0);
    sq.fill_rule = FillRule::EvenOdd;
    let src = [sq];
    let records = [
        roughen_line(2.0, 10.0, 1, "points.corner"),
        zigzag(2.0, 3.0, "mode.absolute", "points.smooth"),
        record(EffectKind::PuckerBloat, &[("pucker_bloat_factor", real(-20.0))]),
        record(EffectKind::Twist, &[("twist_angle", real(30.0))]),
        tweak_rec("mode.absolute", 2.0, 2.0, [true; 3], 4),
        frame([[0.0, 0.0], [40.0, 0.0], [40.0, 20.0], [0.0, 20.0]], [[2.0, 1.0], [42.0, 0.0], [38.0, 22.0], [0.0, 19.0]]),
        step(&[("copies", ParamValue::Int(2)), ("move_horizontal", real(50.0))]),
    ];
    for r in &records {
        let out = go(&src, r);
        assert!(!out.is_empty(), "{:?}", r.kind());
        assert!(out.iter().all(|o| o.fill_rule == FillRule::EvenOdd), "{:?}", r.kind());
    }
}
