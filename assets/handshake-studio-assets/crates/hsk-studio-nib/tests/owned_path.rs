use hsk_studio_accord::{Length, Unit};
use hsk_studio_nib::*;
use kurbo::PathEl;
use sha2::{Digest, Sha256};

fn p(x: f64, y: f64) -> PathPoint {
    PathPoint::new(x, y)
}
fn px(x: f64, y: f64) -> Point {
    Point {
        x: Length::new(x, Unit::Pixels).unwrap(),
        y: Length::new(y, Unit::Pixels).unwrap(),
    }
}
fn anchor(position: Point, incoming: Point, outgoing: Point) -> Anchor {
    Anchor {
        position,
        incoming,
        outgoing,
        handle_mirroring: Mirroring::None,
    }
}
fn path(fill_rule: FillRule, elements: Vec<PathElement>) -> OwnedPath {
    OwnedPath {
        elements,
        fill_rule,
    }
}
fn triangle() -> OwnedPath {
    path(
        FillRule::NonZero,
        vec![
            PathElement::MoveTo(p(0.0, 0.0)),
            PathElement::LineTo(p(4.0, 0.0)),
            PathElement::LineTo(p(2.0, 3.0)),
            PathElement::Close,
        ],
    )
}
/// Every kind of element, a negative zero, an EvenOdd rule.
fn mixed() -> OwnedPath {
    path(
        FillRule::EvenOdd,
        vec![
            PathElement::MoveTo(p(0.0, 0.0)),
            PathElement::LineTo(p(4.0, 0.0)),
            PathElement::QuadTo(p(5.0, 1.0), p(4.0, 2.0)),
            PathElement::CubicTo(p(3.5, 3.0), p(1.0, 3.0), p(0.0, 2.0)),
            PathElement::LineTo(p(-0.0, 1.5)),
            PathElement::Close,
        ],
    )
}
const MIXED_JSON: &str = r#"{"fill_rule":"evenodd","elements":[["M",0,0],["L",4,0],["Q",5,1,4,2],["C",3.5,3,1,3,0,2],["L",0,1.5],["Z"]]}"#;

#[test]
fn owned_path_validation_rules() {
    let limits = PathLimits::default();
    triangle().validate(&limits).unwrap();
    OwnedPath::new(FillRule::NonZero).validate(&limits).unwrap();
    assert!(triangle().is_closed());
    assert!(!OwnedPath::new(FillRule::NonZero).is_closed());

    let bad = |elements: Vec<PathElement>, limits: &PathLimits| {
        path(FillRule::NonZero, elements)
            .validate(limits)
            .unwrap_err()
    };
    let m = PathElement::MoveTo(p(0.0, 0.0));
    let l = PathElement::LineTo(p(1.0, 1.0));
    // Non-finite and out-of-range coordinates.
    for v in [f64::NAN, f64::INFINITY, -f64::INFINITY, 2.0e9] {
        assert_eq!(
            bad(vec![m, PathElement::LineTo(p(v, 0.0))], &limits),
            Error::InvalidGeometry,
            "{v}"
        );
    }
    assert_eq!(
        bad(
            vec![m, PathElement::QuadTo(p(0.0, 0.0), p(0.0, f64::NAN))],
            &limits
        ),
        Error::InvalidGeometry
    );
    // Structure: a path starts with MoveTo; Close needs drawn segments; no drawing after Close.
    assert_eq!(bad(vec![l], &limits), Error::InvalidGeometry);
    assert_eq!(
        bad(vec![PathElement::Close], &limits),
        Error::InvalidGeometry
    );
    assert_eq!(
        bad(vec![m, PathElement::Close], &limits),
        Error::InvalidGeometry
    );
    assert_eq!(
        bad(vec![m, l, PathElement::Close, l], &limits),
        Error::InvalidGeometry
    );
    // Budget.
    let tight = PathLimits {
        max_elements: 3,
        ..limits
    };
    assert_eq!(
        bad(vec![m, l, l, PathElement::Close], &tight),
        Error::BudgetExceeded
    );
    // Closed-ness rule: every subpath must end in Close.
    let strict = PathLimits {
        require_closed: true,
        ..limits
    };
    triangle().validate(&strict).unwrap();
    assert_eq!(bad(vec![m, l, l], &strict), Error::InvalidGeometry);
    let two = vec![m, l, l, PathElement::Close, m, l, l];
    path(FillRule::NonZero, two.clone())
        .validate(&limits)
        .unwrap();
    assert_eq!(bad(two.clone(), &strict), Error::InvalidGeometry);
    assert!(!path(FillRule::NonZero, two).is_closed());
}

#[test]
fn canonical_json_round_trip_and_strictness() {
    let limits = PathLimits::default();
    let json = mixed().to_canonical_json();
    assert_eq!(json, MIXED_JSON, "canonical text, -0 written as 0");
    assert_eq!(
        mixed().content_sha256().to_vec(),
        Sha256::digest(MIXED_JSON.as_bytes()).to_vec()
    );
    assert_eq!(
        OwnedPath::from_canonical_json(MIXED_JSON, &limits).unwrap(),
        mixed()
    );
    // Whitespace and field order do not matter to the strict reader.
    let spaced = r#" { "elements" : [ [ "M" , 0 , 0 ] , [ "L" , 4 , 0 ] , [ "Q" , 5 , 1 , 4 , 2 ] , [ "C" , 3.5 , 3 , 1 , 3 , 0 , 2 ] , [ "L" , 0 , 1.5 ] , [ "Z" ] ] , "fill_rule" : "evenodd" } "#;
    assert_eq!(
        OwnedPath::from_canonical_json(spaced, &limits).unwrap(),
        mixed()
    );
    // Shortest round-trip decimals keep awkward values bit-exact.
    let awkward = path(
        FillRule::NonZero,
        vec![
            PathElement::MoveTo(p(0.1, 1.0e-7)),
            PathElement::LineTo(p(123456789.123, -0.30000000000000004)),
            PathElement::LineTo(p(1.0 / 3.0, 7.0e8)),
            PathElement::Close,
        ],
    );
    let back = OwnedPath::from_canonical_json(&awkward.to_canonical_json(), &limits).unwrap();
    assert_eq!(back, awkward);
    assert_eq!(
        OwnedPath::new(FillRule::EvenOdd).to_canonical_json(),
        r#"{"fill_rule":"evenodd","elements":[]}"#
    );

    let reject =
        |text: &str, limits: &PathLimits| match OwnedPath::from_canonical_json(text, limits) {
            Ok(path) => panic!("accepted {text:?} as {path:?}"),
            Err(error) => error,
        };
    let ok_tail = r#""elements":[["M",0,0],["L",1,1],["Z"]]"#;
    for text in [
        format!(r#"{{"fill_rule":"nonzero",{ok_tail},"extra":1}}"#),
        format!(r#"{{"fill_rule":"nonzero","fill_rule":"evenodd",{ok_tail}}}"#),
        format!(r#"{{"fill_rule":"winding",{ok_tail}}}"#),
        // A backslash escape is never canonical.
        format!(r#"{{"fill_rule":"nonzero",{ok_tail}}}"#).replace("nonzero", "non\\zero"),
        r#"{"fill_rule":"nonzero","elements":[["X",1,2]]}"#.into(),
        r#"{"fill_rule":"nonzero","elements":[["Q",1,2,3]]}"#.into(),
        r#"{"fill_rule":"nonzero","elements":[["M",NaN,2]]}"#.into(),
        format!(r#"{{"fill_rule":"nonzero",{ok_tail}}} trailing"#),
        format!(r#"{{"fill_rule":"nonzero",{ok_tail}"#),
    ] {
        assert_eq!(reject(&text, &limits), Error::InvalidEncoding, "{text}");
    }
    assert_eq!(
        reject(&format!("{{{ok_tail}}}"), &limits),
        Error::MissingField
    );
    assert_eq!(
        reject(
            r#"{"fill_rule":"nonzero","elements":[["M",1e999,0]]}"#,
            &limits
        ),
        Error::InvalidGeometry
    );
    // Decoding validates structure and budgets before the value is returned.
    assert_eq!(
        reject(r#"{"fill_rule":"nonzero","elements":[["L",1,1]]}"#, &limits),
        Error::InvalidGeometry
    );
    let tight = PathLimits {
        max_elements: 2,
        ..limits
    };
    assert_eq!(reject(MIXED_JSON, &tight), Error::BudgetExceeded);
    assert_eq!(
        reject(&" ".repeat(tight.max_input_bytes() + 1), &tight),
        Error::BudgetExceeded
    );
}

#[test]
fn kurbo_and_anchor_conversion() {
    let limits = PathLimits::default();
    // kurbo is lossless both ways, one element each.
    let k = mixed().to_kurbo().unwrap();
    assert_eq!(
        k.elements(),
        &[
            PathEl::MoveTo((0.0, 0.0).into()),
            PathEl::LineTo((4.0, 0.0).into()),
            PathEl::QuadTo((5.0, 1.0).into(), (4.0, 2.0).into()),
            PathEl::CurveTo((3.5, 3.0).into(), (1.0, 3.0).into(), (0.0, 2.0).into()),
            PathEl::LineTo((0.0, 1.5).into()),
            PathEl::ClosePath,
        ]
    );
    assert_eq!(
        OwnedPath::from_kurbo(&k, FillRule::EvenOdd, &limits).unwrap(),
        mixed()
    );
    // An invalid path never reaches kurbo (which would debug-assert on a missing MoveTo).
    let orphan = path(FillRule::NonZero, vec![PathElement::LineTo(p(1.0, 1.0))]);
    assert_eq!(orphan.to_kurbo().unwrap_err(), Error::InvalidGeometry);

    // Anchors: handles are anchor-relative; zero handles are straight; closing is implicit.
    let z = px(0.0, 0.0);
    let rect = [
        anchor(px(0.0, 0.0), z, z),
        anchor(px(4.0, 0.0), z, z),
        anchor(px(4.0, 3.0), z, z),
        anchor(px(0.0, 3.0), z, z),
    ];
    let closed =
        OwnedPath::from_anchors(&rect, true, Winding::NonZero, Unit::Pixels, None, &limits)
            .unwrap();
    assert_eq!(
        closed.elements,
        vec![
            PathElement::MoveTo(p(0.0, 0.0)),
            PathElement::LineTo(p(4.0, 0.0)),
            PathElement::LineTo(p(4.0, 3.0)),
            PathElement::LineTo(p(0.0, 3.0)),
            PathElement::Close,
        ]
    );
    assert!(closed.is_closed());
    let open = OwnedPath::from_anchors(&rect, false, Winding::EvenOdd, Unit::Pixels, None, &limits)
        .unwrap();
    assert_eq!(open.fill_rule, FillRule::EvenOdd);
    assert_eq!(open.elements.len(), 4);
    assert!(!open.is_closed());
    // Curve A -> B: control points are A + A.outgoing and B + B.incoming.
    let curve = [
        anchor(px(0.0, 0.0), z, px(1.0, 2.0)),
        anchor(px(4.0, 0.0), px(-1.0, 2.0), z),
    ];
    assert_eq!(
        OwnedPath::from_anchors(&curve, false, Winding::NonZero, Unit::Pixels, None, &limits)
            .unwrap()
            .elements,
        vec![
            PathElement::MoveTo(p(0.0, 0.0)),
            PathElement::CubicTo(p(1.0, 2.0), p(3.0, 2.0), p(4.0, 0.0)),
        ]
    );
    // Closed with a curved first segment and a straight closing segment.
    assert_eq!(
        OwnedPath::from_anchors(&curve, true, Winding::NonZero, Unit::Pixels, None, &limits)
            .unwrap()
            .elements,
        vec![
            PathElement::MoveTo(p(0.0, 0.0)),
            PathElement::CubicTo(p(1.0, 2.0), p(3.0, 2.0), p(4.0, 0.0)),
            PathElement::Close,
        ]
    );
    // Units: 36 pt = 0.5 in = 72 px at 144 ppi; pixels<->points needs a resolution.
    let pts = [
        anchor(Point::pt(36.0, 72.0).unwrap(), z, z),
        anchor(Point::pt(72.0, 0.0).unwrap(), z, z),
    ];
    assert_eq!(
        OwnedPath::from_anchors(
            &pts,
            false,
            Winding::NonZero,
            Unit::Pixels,
            Some(144.0),
            &limits
        )
        .unwrap()
        .elements,
        vec![
            PathElement::MoveTo(p(72.0, 144.0)),
            PathElement::LineTo(p(144.0, 0.0)),
        ]
    );
    for (anchors, winding, ppi) in [
        (&pts[..], Winding::NonZero, None),
        (&rect[..], Winding::None, None),
        (&rect[..1], Winding::NonZero, None),
    ] {
        assert_eq!(
            OwnedPath::from_anchors(anchors, false, winding, Unit::Pixels, ppi, &limits)
                .unwrap_err(),
            Error::InvalidGeometry
        );
    }
}

/// Deterministic xorshift64* so the round-trip property test needs no external crate.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Any finite `f64` within `COORDINATE_LIMIT`, subnormals and both signs included: random
    /// mantissa bits under a random binary exponent.
    fn coordinate(&mut self) -> f64 {
        loop {
            let bits = self.next();
            let exponent = (self.next() % (1023 + 30)) << 52;
            let value = f64::from_bits((bits & (1 << 63)) | exponent | (bits & ((1 << 52) - 1)));
            if value.abs() <= COORDINATE_LIMIT {
                return value;
            }
        }
    }
    fn point(&mut self) -> PathPoint {
        PathPoint::new(self.coordinate(), self.coordinate())
    }
}

#[test]
fn canonical_json_round_trips_every_valid_path_within_budget() {
    // R3 finding: f64 Display never uses an exponent, so [MoveTo(5e-324, 5e-324)] serialized to
    // ~700 bytes against a 416 byte budget and the reader refused its own output.
    let tiny = path(
        FillRule::NonZero,
        vec![PathElement::MoveTo(p(5e-324, -5e-324))],
    );
    let exact = PathLimits {
        max_elements: 1,
        ..PathLimits::default()
    };
    tiny.validate(&exact).unwrap();
    let json = tiny.to_canonical_json();
    assert!(
        json.len() <= exact.max_input_bytes(),
        "{} bytes",
        json.len()
    );
    assert_eq!(
        json,
        r#"{"fill_rule":"nonzero","elements":[["M",5e-324,-5e-324]]}"#
    );
    assert_eq!(OwnedPath::from_canonical_json(&json, &exact).unwrap(), tiny);

    // Worst-case number width: 17 digits, negative, three-digit negative exponent, in every slot
    // of a full-budget cubic path.
    let wide = -1.234_567_890_123_456_7e-300;
    let cubic = PathElement::CubicTo(p(wide, wide), p(wide, wide), p(wide, wide));
    let full = path(
        FillRule::EvenOdd,
        [
            PathElement::MoveTo(p(wide, wide)),
            cubic,
            cubic,
            cubic,
            cubic,
        ]
        .to_vec(),
    );
    let limits = PathLimits {
        max_elements: 5,
        ..PathLimits::default()
    };
    let json = full.to_canonical_json();
    assert!(
        json.len() <= limits.max_input_bytes(),
        "{} bytes",
        json.len()
    );
    assert_eq!(
        OwnedPath::from_canonical_json(&json, &limits).unwrap(),
        full
    );

    // Short values keep their plain decimal text (hashes of ordinary paths are unchanged).
    let plain = path(
        FillRule::NonZero,
        vec![PathElement::MoveTo(p(1.0e-7, 123_456_789.123))],
    );
    assert_eq!(
        plain.to_canonical_json(),
        r#"{"fill_rule":"nonzero","elements":[["M",0.0000001,123456789.123]]}"#
    );

    // Property: random valid paths of every element kind (subnormals through 1e9, both signs)
    // round-trip bit-exactly within the budget of exactly their own element count.
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for case in 0_u32..400 {
        let mut elements = Vec::new();
        for _ in 0..=rng.next() % 4 {
            elements.push(PathElement::MoveTo(rng.point()));
            for _ in 0..=rng.next() % 3 {
                elements.push(match rng.next() % 3 {
                    0 => PathElement::LineTo(rng.point()),
                    1 => PathElement::QuadTo(rng.point(), rng.point()),
                    _ => PathElement::CubicTo(rng.point(), rng.point(), rng.point()),
                });
            }
            if rng.next().is_multiple_of(2) {
                elements.push(PathElement::Close);
            }
        }
        let rule = if case.is_multiple_of(2) {
            FillRule::NonZero
        } else {
            FillRule::EvenOdd
        };
        let candidate = path(rule, elements);
        let limits = PathLimits {
            max_elements: candidate.elements.len(),
            ..PathLimits::default()
        };
        candidate.validate(&limits).unwrap();
        let json = candidate.to_canonical_json();
        assert!(
            json.len() <= limits.max_input_bytes(),
            "case {case}: {} bytes > {}",
            json.len(),
            limits.max_input_bytes()
        );
        let back = OwnedPath::from_canonical_json(&json, &limits)
            .unwrap_or_else(|error| panic!("case {case}: {error:?} for {json}"));
        assert_eq!(back, candidate, "case {case}");
        assert_eq!(
            back.to_canonical_json(),
            json,
            "case {case}: canonical text"
        );
        assert_eq!(back.content_sha256(), candidate.content_sha256());
    }
}

#[test]
fn is_closed_agrees_with_require_closed_validation() {
    // R3 finding: [MoveTo] and [M, L, Z, M] reported closed while validate(require_closed)
    // refused them. One definition: closed == non-empty and the strict state machine accepts.
    let (m, l, z) = (
        PathElement::MoveTo(p(0.0, 0.0)),
        PathElement::LineTo(p(1.0, 1.0)),
        PathElement::Close,
    );
    for lone in [vec![m], vec![m, l, z, m], vec![m, z], vec![m, l], vec![]] {
        assert!(
            !path(FillRule::NonZero, lone.clone()).is_closed(),
            "{lone:?}"
        );
    }
    assert!(path(FillRule::NonZero, vec![m, l, z]).is_closed());
    assert!(path(FillRule::NonZero, vec![m, l, z, m, l, z]).is_closed());

    // Exhaustive over every verb sequence up to length 7: is_closed() equals the strict verdict.
    let strict = PathLimits {
        require_closed: true,
        ..PathLimits::default()
    };
    let verbs = [
        m,
        l,
        PathElement::QuadTo(p(1.0, 0.0), p(2.0, 0.0)),
        PathElement::CubicTo(p(1.0, 0.0), p(2.0, 0.0), p(3.0, 0.0)),
        z,
    ];
    let mut checked = 0_usize;
    for len in 0..=7_u32 {
        for code in 0..(verbs.len() as u32).pow(len) {
            let elements: Vec<_> = (0..len)
                .map(|i| verbs[(code / (verbs.len() as u32).pow(i)) as usize % verbs.len()])
                .collect();
            let candidate = path(FillRule::NonZero, elements);
            assert_eq!(
                candidate.is_closed(),
                !candidate.elements.is_empty() && candidate.validate(&strict).is_ok(),
                "{:?}",
                candidate.elements
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 97_656);
}
