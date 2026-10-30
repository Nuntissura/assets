//! Shaped mask coverage (lane SMB-AE-MASK-FEATHER-1; STU-CMP-025 mask feather, opacity and
//! expansion): `mask_paths::mask_coverage_shaped` with a `MaskShape` per mask. The identity shape is
//! bit-identical to `mask_coverage`; expansion is nib's round-join offset of the path; feather is
//! pigment's separable `coverage_math` feather kernel over the plane; opacity scales the mask
//! before the ordered combination. After Effects parity of every reading is NOT_PROVEN (D5).
use hsk_studio_composite::mask_paths::{
    COMPOSITION_MASK_EXPANSION_GEOMETRY, COMPOSITION_MASK_FEATHER_OUT_OF_RANGE, COMPOSITION_MASK_OPACITY_NOT_FINITE,
    MaskAtTick, MaskShape, MaskShapeRefusal, mask_coverage, mask_coverage_shaped,
};
use hsk_studio_folio::StudioCompositionMaskMode as M;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::transform_math::Affine2;

const W: u32 = 40;
const H: u32 = 32;
const PLANE: Rect = Rect { x: 0, y: 0, width: W, height: H };
const PX: u64 = (W * H) as u64;
/// Quarter-circle handle length of a cubic arc of unit radius.
const KAPPA: f64 = 0.552_284_749_830_793_4;

fn rect_path(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<f64> {
    vec![x0, y0, 0.0, 0.0, 0.0, 0.0, x1, y0, 0.0, 0.0, 0.0, 0.0, x1, y1, 0.0, 0.0, 0.0, 0.0, x0, y1, 0.0, 0.0, 0.0, 0.0]
}
/// The rectangle `(x0, y0)-(x1, y1)` grown by `r` with round corners of radius `r` (cubic arcs).
fn rounded_rect(x0: f64, y0: f64, x1: f64, y1: f64, r: f64) -> Vec<f64> {
    let k = KAPPA * r;
    let (ox0, oy0, ox1, oy1) = (x0 - r, y0 - r, x1 + r, y1 + r);
    vec![
        x0, oy0, -k, 0.0, 0.0, 0.0, //
        x1, oy0, 0.0, 0.0, k, 0.0, //
        ox1, y0, 0.0, -k, 0.0, 0.0, //
        ox1, y1, 0.0, 0.0, 0.0, k, //
        x1, oy1, k, 0.0, 0.0, 0.0, //
        x0, oy1, 0.0, 0.0, -k, 0.0, //
        ox0, y1, 0.0, k, 0.0, 0.0, //
        ox0, y0, 0.0, 0.0, 0.0, -k, //
    ]
}
fn mask(mode: M, inverted: bool, path: Vec<f64>) -> MaskAtTick {
    MaskAtTick { mode, inverted, closed: true, path }
}
fn shaped(masks: &[(MaskAtTick, MaskShape)]) -> Vec<f32> {
    mask_coverage_shaped(masks, &Affine2::IDENTITY, PLANE, 1 << 20).unwrap().expect("a contributing mask")
}
fn at(plane: &[f32], x: u32, y: u32) -> f32 {
    plane[(y * W + x) as usize]
}

#[test]
fn identity_shape_is_bit_identical_to_mask_coverage() {
    let curve = vec![
        6.0, 4.0, -2.0, 1.5, 3.0, -1.0, //
        30.0, 6.5, -2.5, -1.0, 1.0, 4.0, //
        24.25, 27.0, 3.0, 0.5, -6.0, 0.0, //
        5.5, 20.0, 0.0, 3.0, 0.0, -2.0,
    ];
    let sets = vec![
        vec![mask(M::Add, false, curve.clone())],
        vec![mask(M::Add, false, curve.clone()), mask(M::Subtract, true, rect_path(10.5, 8.25, 22.0, 19.5))],
        vec![mask(M::Intersect, false, curve.clone()), mask(M::Difference, false, rect_path(3.0, 3.0, 17.0, 29.0))],
        vec![mask(M::None, false, curve.clone()), mask(M::Lighten, true, curve.clone()), mask(M::Darken, false, rect_path(1.0, 2.0, 35.0, 30.0))],
        vec![MaskAtTick { closed: false, ..mask(M::Add, false, curve.clone()) }],
    ];
    let matrices = [Affine2::IDENTITY, Affine2::from_parts(12.0, 10.0, 0.9, 1.2, 17.5, 2.0, 1.5)];
    for (i, masks) in sets.iter().enumerate() {
        for matrix in &matrices {
            let plain = mask_coverage(masks, matrix, PLANE, PX).unwrap();
            let with: Vec<_> = masks.iter().cloned().map(|m| (m, MaskShape::default())).collect();
            let got = mask_coverage_shaped(&with, matrix, PLANE, PX).unwrap();
            let bits = |v: &Option<Vec<f32>>| v.as_ref().map(|p| p.iter().map(|c| c.to_bits()).collect::<Vec<_>>());
            assert_eq!(bits(&got), bits(&plain), "set {i}");
        }
    }
    assert_eq!(MaskShape::default(), MaskShape { feather_px: [0.0, 0.0], opacity: 1.0, expansion_px: 0.0 });
    // The identity shape keeps mask_coverage's plane bound.
    let one = [(mask(M::Add, false, curve), MaskShape::default())];
    assert_eq!(mask_coverage_shaped(&one, &Affine2::IDENTITY, PLANE, PX - 1).unwrap_err().mask, 0);
}

#[test]
fn expansion_grows_with_round_corners_and_shrinks() {
    let (x0, y0, x1, y1) = (10.0, 8.0, 30.0, 22.0);
    let k = 3.0;
    let grown = shaped(&[(mask(M::Add, false, rect_path(x0, y0, x1, y1)), MaskShape { expansion_px: k, ..MaskShape::default() })]);
    let want = mask_coverage(&[mask(M::Add, false, rounded_rect(x0, y0, x1, y1, k))], &Affine2::IDENTITY, PLANE, PX).unwrap().unwrap();
    let mut corner_worst = 0.0f32;
    for y in 0..H {
        for x in 0..W {
            let (px, py) = (f64::from(x), f64::from(y));
            // The pixel square [px, px + 1] x [py, py + 1] against the grown region.
            let in_cross = (px >= x0 - k && px + 1.0 <= x1 + k && py >= y0 && py + 1.0 <= y1)
                || (px >= x0 && px + 1.0 <= x1 && py >= y0 - k && py + 1.0 <= y1 + k);
            let outside_box = px + 1.0 <= x0 - k || px >= x1 + k || py + 1.0 <= y0 - k || py >= y1 + k;
            let (got, w) = (at(&grown, x, y), at(&want, x, y));
            if in_cross {
                assert_eq!((got, w), (1.0, 1.0), "inside ({x},{y})");
            } else if outside_box {
                assert_eq!((got, w), (0.0, 0.0), "outside ({x},{y})");
            } else {
                corner_worst = corner_worst.max((got - w).abs());
            }
        }
    }
    println!("expansion +{k}: corner max |delta| vs the round-cornered rectangle {corner_worst:e}");
    assert!(corner_worst <= 1.0 / 255.0 + 1e-6, "{corner_worst}");
    // An inset of a convex rectangle keeps sharp corners: exactly the smaller rectangle.
    let shrunk = shaped(&[(mask(M::Add, false, rect_path(x0, y0, x1, y1)), MaskShape { expansion_px: -k, ..MaskShape::default() })]);
    let want = mask_coverage(&[mask(M::Add, false, rect_path(x0 + k, y0 + k, x1 - k, y1 - k))], &Affine2::IDENTITY, PLANE, PX).unwrap().unwrap();
    assert_eq!(shrunk, want);
    // Expansion is in layer space: under a 2x scale the composition rectangle grows by 2k.
    let scaled = mask_coverage_shaped(
        &[(mask(M::Add, false, rect_path(5.0, 4.0, 15.0, 11.0)), MaskShape { expansion_px: -1.0, ..MaskShape::default() })],
        &Affine2 { m: [2.0, 0.0, 0.0, 0.0, 2.0, 0.0] },
        PLANE,
        PX,
    )
    .unwrap()
    .unwrap();
    let want = mask_coverage(&[mask(M::Add, false, rect_path(12.0, 10.0, 28.0, 20.0))], &Affine2::IDENTITY, PLANE, PX).unwrap().unwrap();
    assert_eq!(scaled, want);
}

#[test]
fn collapse_gives_empty_coverage_and_hostile_shapes_are_typed() {
    let r = rect_path(10.0, 8.0, 30.0, 22.0);
    let collapsed = shaped(&[(mask(M::Add, false, r.clone()), MaskShape { expansion_px: -7.5, ..MaskShape::default() })]);
    assert!(collapsed.iter().all(|c| *c == 0.0), "a collapsed add mask covers nothing");
    // Inverted, the collapsed mask covers everything; combined after another mask it adds nothing.
    let inv = shaped(&[(mask(M::Add, true, r.clone()), MaskShape { expansion_px: -100.0, ..MaskShape::default() })]);
    assert!(inv.iter().all(|c| *c == 1.0));
    let refusal = |masks: &[(MaskAtTick, MaskShape)], max: u64| mask_coverage_shaped(masks, &Affine2::IDENTITY, PLANE, max).unwrap_err();
    let plain = (mask(M::Add, false, r.clone()), MaskShape::default());
    let with = |s: MaskShape| [plain.clone(), (mask(M::Subtract, false, r.clone()), s)];
    let feather = |f: [f64; 2]| MaskShape { feather_px: f, ..MaskShape::default() };
    for bad in [[10_000.5, 0.0], [0.0, f64::NAN], [f64::INFINITY, 1.0]] {
        assert_eq!(refusal(&with(feather(bad)), 1 << 30), MaskShapeRefusal { mask: 1, code: COMPOSITION_MASK_FEATHER_OUT_OF_RANGE }, "{bad:?}");
    }
    for bad in [f64::NAN, f64::INFINITY] {
        let s = MaskShape { opacity: bad, ..MaskShape::default() };
        assert_eq!(refusal(&with(s), 1 << 30), MaskShapeRefusal { mask: 1, code: COMPOSITION_MASK_OPACITY_NOT_FINITE });
        let s = MaskShape { expansion_px: bad, ..MaskShape::default() };
        assert_eq!(refusal(&with(s), 1 << 30), MaskShapeRefusal { mask: 1, code: COMPOSITION_MASK_EXPANSION_GEOMETRY });
    }
    // Beyond nib's coordinate limit the offset is refused, typed.
    let s = MaskShape { expansion_px: 2.0e9, ..MaskShape::default() };
    assert_eq!(refusal(&with(s), 1 << 30), MaskShapeRefusal { mask: 1, code: COMPOSITION_MASK_EXPANSION_GEOMETRY });
    // The plane grown by the feather support is charged against max_pixels before allocation:
    // radius 4 -> support 6 on each side.
    let grown = u64::from(W + 12) * u64::from(H + 12);
    assert_eq!(refusal(&with(feather([4.0, 4.0])), grown - 1).mask, 1);
    assert!(mask_coverage_shaped(&with(feather([4.0, 4.0])), &Affine2::IDENTITY, PLANE, grown).is_ok());
    // Sampled overshoot: a negative feather is no feather, opacity is clamped into 0..=1.
    let base = shaped(&[plain.clone()]);
    assert_eq!(shaped(&[(plain.0.clone(), MaskShape { feather_px: [-0.5, -2.0], opacity: 1.7, expansion_px: 0.0 })]), base);
    assert!(shaped(&[(plain.0.clone(), MaskShape { opacity: -0.2, ..MaskShape::default() })]).iter().all(|c| *c == 0.0));
}

#[test]
fn feather_is_a_symmetric_ramp_preserving_the_integral() {
    // One vertical edge at x = 20 (the mask covers x < 20 over the whole plane height).
    let edge = mask(M::Add, false, rect_path(-200.0, -200.0, 20.0, 200.0));
    let radius = 6.0;
    let soft = shaped(&[(edge.clone(), MaskShape { feather_px: [radius, radius], ..MaskShape::default() })]);
    let hard = shaped(&[(edge, MaskShape::default())]);
    let support = hsk_studio_pigment::coverage_math::feather_support(radius as f32).unwrap();
    assert_eq!(support, 9, "sigma = radius / 2, support = ceil(3 sigma)");
    for y in [0, H / 2, H - 1] {
        let row: Vec<f32> = (0..W).map(|x| at(&soft, x, y)).collect();
        // Centred on the edge: the two pixels astride it average 0.5; symmetric about it.
        assert!(((row[19] + row[20]) / 2.0 - 0.5).abs() <= 1e-6, "{row:?}");
        for d in 0..10 {
            assert!((row[19 - d] + row[20 + d] - 1.0).abs() <= 1e-6, "d {d}: {row:?}");
        }
        // A monotone ramp, flat beyond the support.
        assert!(row.windows(2).all(|w| w[1] <= w[0] + 1e-7), "{row:?}");
        assert!(row[..=10].iter().all(|v| (v - 1.0).abs() <= 1e-6) && row[29..].iter().all(|v| v.abs() <= 1e-6));
        assert!(row[12] < 1.0 && row[12] > 0.99 && row[19] > 0.5 && row[19] < 0.6);
        // The feather moves coverage across the edge without changing its total (declared
        // tolerance 1e-4 of a pixel per row: f32 taps and plane).
        let (s, h): (f64, f64) = (row.iter().map(|v| f64::from(*v)).sum(), (0..W).map(|x| f64::from(at(&hard, x, y))).sum());
        assert!((s - h).abs() <= 1e-4, "integral {s} vs {h}");
    }
}

#[test]
fn single_axis_feather_softens_only_edges_across_that_axis() {
    let r = mask(M::Add, false, rect_path(10.0, 8.0, 30.0, 22.0));
    let x_only = shaped(&[(r.clone(), MaskShape { feather_px: [4.0, 0.0], ..MaskShape::default() })]);
    let y_only = shaped(&[(r, MaskShape { feather_px: [0.0, 4.0], ..MaskShape::default() })]);
    let near = |a: f32, b: f32| (a - b).abs() <= 1e-6;
    // x feather: the left and right (vertical) edges ramp, the top and bottom stay sharp.
    assert!(at(&x_only, 9, 15) > 0.0 && at(&x_only, 10, 15) < 1.0 && near(at(&x_only, 9, 15) + at(&x_only, 10, 15), 1.0));
    assert!(at(&x_only, 20, 7) == 0.0 && near(at(&x_only, 20, 8), 1.0) && near(at(&x_only, 20, 21), 1.0) && at(&x_only, 20, 22) == 0.0);
    // y feather: the reverse.
    assert!(at(&y_only, 20, 7) > 0.0 && at(&y_only, 20, 8) < 1.0 && near(at(&y_only, 20, 7) + at(&y_only, 20, 8), 1.0));
    assert!(at(&y_only, 9, 15) == 0.0 && near(at(&y_only, 10, 15), 1.0) && near(at(&y_only, 29, 15), 1.0) && at(&y_only, 30, 15) == 0.0);
}

#[test]
fn opacity_scales_the_mask_and_order_still_matters() {
    let a = mask(M::Add, false, rect_path(4.0, 4.0, 24.0, 20.0));
    let full = shaped(&[(a.clone(), MaskShape::default())]);
    let half = shaped(&[(a.clone(), MaskShape { opacity: 0.5, ..MaskShape::default() })]);
    assert!(full.iter().zip(&half).all(|(f, h)| *h == f * 0.5), "opacity 0.5 halves an add mask");
    assert_eq!(at(&half, 10, 10), 0.5);
    // Inverted then scaled: 0.5 outside the path, 0 inside.
    let inv = shaped(&[(mask(M::Add, true, rect_path(4.0, 4.0, 24.0, 20.0)), MaskShape { opacity: 0.5, ..MaskShape::default() })]);
    assert_eq!((at(&inv, 10, 10), at(&inv, 30, 25)), (0.0, 0.5));
    // add(a) then subtract(b at 0.5) vs subtract(b at 0.5) then add(a): order changes the overlap.
    let b = (mask(M::Subtract, false, rect_path(14.0, 10.0, 34.0, 28.0)), MaskShape { opacity: 0.5, ..MaskShape::default() });
    let ab = shaped(&[(a.clone(), MaskShape::default()), b.clone()]);
    let ba = shaped(&[b, (a, MaskShape::default())]);
    assert_eq!((at(&ab, 20, 15), at(&ba, 20, 15)), (0.5, 1.0), "overlap");
    assert_eq!((at(&ab, 30, 25), at(&ba, 30, 25)), (0.0, 0.5), "b only");
    assert_eq!((at(&ab, 6, 6), at(&ba, 6, 6)), (1.0, 1.0), "a only");
    println!("NOT_PROVEN: After Effects mask feather, opacity and expansion parity (order, feather radius mapping, expansion space): no D5 oracle case for After Effects masks");
    println!("NOT_PROVEN: variable-width mask feather (ADBE Mask Interp) is a declared gap, not implemented");
}
