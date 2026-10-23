//! Selection engine proofs (lane SMB-PIGMENT-SELECTION). Hand-computed values.
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_pigment::coverage_math::{CoverageError, feather_kernel};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_pigment::selection::*;
use hsk_studio_pigment::{Error, Mask, Rect};

const CANVAS: Rect = Rect {
    x: 0,
    y: 0,
    width: 40,
    height: 40,
};

fn ctx() -> SelectionContext {
    SelectionContext::new(CANVAS)
}
fn rf(x: f64, y: f64, width: f64, height: f64) -> RectF {
    RectF { x, y, width, height }
}

#[test]
fn shapes_hand_values_and_fill_rules() {
    // Rect 0.5..2.5 x 0..2: edge pixel columns see 2 of 4 sample columns = 8/16 -> 32768.
    let p = rect(rf(0.5, 0.0, 2.0, 2.0), &ctx()).unwrap();
    assert_eq!(p.bounds, Rect { x: 0, y: 0, width: 3, height: 2 });
    for y in 0..2 {
        assert_eq!([p.at(0, y), p.at(1, y), p.at(2, y)], [32768, 65535, 32768]);
    }
    // AA off: centre sample only; 0.5 centre of pixel 0 is on the edge (x >= 0.5 inside).
    let mut c = ctx();
    c.anti_alias = false;
    let p = rect(rf(0.5, 0.0, 2.0, 2.0), &c).unwrap();
    assert!(p.data.iter().all(|v| *v == 0 || *v == 65535));
    assert_eq!([p.at(0, 0), p.at(1, 0), p.at(2, 0)], [65535, 65535, 0]);
    // Ellipse 40x20 area within 1 % of pi*a*b = pi*20*10.
    let e = ellipse(rf(0.0, 10.0, 40.0, 20.0), &ctx()).unwrap();
    let exact = std::f64::consts::PI * 20.0 * 10.0;
    assert!((e.area_sum() - exact).abs() / exact < 0.01, "{}", e.area_sum());
    // Closed square polygon == rect.
    let sq = polygon(&[[3.25, 4.0], [9.0, 4.0], [9.0, 8.5], [3.25, 8.5]], FillRule::EvenOdd, &ctx()).unwrap();
    assert_eq!(sq, rect(rf(3.25, 4.0, 5.75, 4.5), &ctx()).unwrap());
    // Pentagram: the centre pentagon is wound twice -> excluded by even-odd, included by non-zero.
    let star: Vec<[f64; 2]> = (0..5)
        .map(|k| {
            let a = std::f64::consts::FRAC_PI_2 + f64::from(k) * 4.0 * std::f64::consts::PI / 5.0;
            [20.0 + 18.0 * a.cos(), 20.0 - 18.0 * a.sin()]
        })
        .collect();
    let eo = polygon(&star, FillRule::EvenOdd, &ctx()).unwrap();
    let nz = polygon(&star, FillRule::NonZero, &ctx()).unwrap();
    assert_eq!(eo.at(20, 20), 0);
    assert_eq!(nz.at(20, 20), 65535);
    assert_eq!(eo.at(20, 4), nz.at(20, 4));
    assert!(eo.at(20, 4) > 0);
}

#[test]
fn combine_modes_on_known_shapes() {
    for (v, m) in [(0u8, None), (1, Some(CombineMode::Replace)), (4, Some(CombineMode::Intersect)), (5, None)] {
        assert_eq!(CombineMode::from_u8(v), m);
    }
    assert_eq!(CombineMode::Diminish as u8, 3);
    assert_eq!(CombineMode::Extend as u8, 2);
    let lim = SelectionLimits::default();
    // A = 10x10 at (0,0) = 100 px; B = 10x10 at (5,5) = 100 px; overlap 5x5 = 25 px.
    let a = rect(rf(0.0, 0.0, 10.0, 10.0), &ctx()).unwrap();
    let b = rect(rf(5.0, 5.0, 10.0, 10.0), &ctx()).unwrap();
    let count = |p: &CoveragePlane| p.data.iter().filter(|v| **v == 65535).count();
    assert_eq!(count(&combine(Some(&a), &b, CombineMode::Replace, &lim).unwrap()), 100);
    assert_eq!(count(&combine(Some(&a), &b, CombineMode::Extend, &lim).unwrap()), 175);
    assert_eq!(count(&combine(Some(&a), &b, CombineMode::Diminish, &lim).unwrap()), 75);
    assert_eq!(count(&combine(Some(&a), &b, CombineMode::Intersect, &lim).unwrap()), 25);
    // No base: replace/extend keep new; diminish/intersect are empty.
    assert_eq!(combine(None, &b, CombineMode::Extend, &lim).unwrap(), b);
    assert_eq!(count(&combine(None, &b, CombineMode::Diminish, &lim).unwrap()), 0);
    // Partial coverage: a=0.5 (32768) b=0.5: extend 0.75, diminish 0.25, intersect 0.25.
    let h = CoveragePlane::filled(Rect { x: 0, y: 0, width: 1, height: 1 }, 32768, &lim).unwrap();
    let at = |m| combine(Some(&h), &h, m, &lim).unwrap().data[0];
    assert!((i32::from(at(CombineMode::Extend)) - 49152).abs() <= 1);
    assert!((i32::from(at(CombineMode::Diminish)) - 16384).abs() <= 1);
    assert!((i32::from(at(CombineMode::Intersect)) - 16384).abs() <= 1);
}

#[test]
fn feather_matches_kernel_and_colour_range_selects() {
    // Hard 6x6 rect in the middle; reference = separable kernel applied in the test.
    let hard = rect(rf(10.0, 10.0, 6.0, 6.0), &ctx()).unwrap();
    let f = feather(&hard, 4.0, &ctx()).unwrap();
    let k = feather_kernel(4.0).unwrap();
    let s = (k.len() / 2) as i64;
    let h = |x: i64, y: i64| -> f64 {
        k.iter().enumerate().map(|(i, t)| f64::from(*t) * f64::from(hard.at(x + i as i64 - s, y))).sum()
    };
    for (x, y) in [(12, 12), (9, 12), (7, 13), (12, 8), (16, 16), (8, 8)] {
        let r: f64 = k.iter().enumerate().map(|(i, t)| f64::from(*t) * h(x, y + i as i64 - s)).sum();
        assert!((f64::from(f.at(x, y)) - r).abs() <= 1.0, "({x},{y}) {} vs {r}", f.at(x, y));
    }
    assert!(f.bounds.width > hard.bounds.width);
    assert_eq!(f.at(0, 0), 0);
    // Colour range on a 4x1 synthetic image: red, red-ish, green, transparent red.
    let px: [[f32; 3]; 4] = [[1.0, 0.0, 0.0], [0.9, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]];
    let colour: Vec<u8> = px.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
    let alpha: Vec<u8> = [1.0f32, 1.0, 1.0, 0.0].iter().flat_map(|v| v.to_le_bytes()).collect();
    let view = ColourView { bounds: Rect { x: 0, y: 0, width: 4, height: 1 }, colour: &colour, alpha: &alpha };
    let p = colour_range(&view, [1.0, 0.0, 0.0], 0.2, &ctx()).unwrap();
    assert_eq!(p.at(0, 0), 65535);
    assert!((i32::from(p.at(1, 0)) - 32768).abs() <= 1); // d = 0.1, f = 0.2
    assert_eq!(p.at(2, 0), 0);
    assert_eq!(p.at(3, 0), 0); // alpha 0
    assert_eq!(invert(&p, &ctx()).unwrap().at(0, 0), 0);
    assert_eq!(invert(&p, &ctx()).unwrap().at(2, 0), 65535);
}

#[test]
fn selection_feeds_the_existing_mask_path() {
    let sel = polygon(&[[1.0, 1.0], [7.0, 2.0], [5.0, 7.0]], FillRule::EvenOdd, &ctx()).unwrap();
    for (fmt, key) in [(MaskFormat::CoverageU8, "coverage_u8"), (MaskFormat::CoverageU16Le, "coverage_u16le")] {
        let bytes = sel.to_mask_bytes(fmt).unwrap();
        let sha = hsk_studio_prism::profile_hash(&bytes);
        let size = if key == "coverage_u8" { 1 } else { 2 };
        let stride = u64::from(sel.bounds.width) * size;
        let m = Mask { bounds: sel.bounds, sample_format: key, bytes: &bytes, stride_bytes: stride, sha256: sha };
        m.validate(sel.bounds).unwrap();
    }
    assert_eq!(sel.to_mask_bytes(MaskFormat::BaseAlpha), Err(Error::Unsupported));
    let bytes = sel.to_mask_bytes(MaskFormat::CoverageU16Le).unwrap();
    let extent = Rect { x: 0, y: 0, width: 8, height: 8 };
    let tag = ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded };
    let layer = |input, mask| Layer { input: PlanNodeId(input), mode: StudioBlendMode::Normal, opacity: 1.0, fill_opacity: 1.0, mask };
    let plan: RenderPlan = RenderPlan {
        revision: 1,
        target: OutputTarget { extent, format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, space: tag },
        nodes: vec![
            PlanNode::Solid { rect: extent, rgba: [1.0, 0.0, 0.0, 1.0] },
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![layer(0, Some(MaskInput {
                    key: SourceKey(1),
                    placement: sel.bounds,
                    format: MaskFormat::CoverageU16Le,
                    sha256: hsk_studio_prism::profile_hash(&bytes),
                    inverted: false,
                    density: 1.0,
                }))],
            }),
        ],
        root: PlanNodeId(1),
    };
    let ops = plan.validate(&PlanLimits::default()).unwrap();
    assert!(ops.contains(&Operator::Mask(MaskFormat::CoverageU16Le)));
}

#[test]
fn hostile_sizes_and_values() {
    // dims_u32_max: a u32::MAX x u32::MAX canvas refuses before any allocation.
    let big = Rect { x: 0, y: 0, width: u32::MAX, height: u32::MAX };
    let c = SelectionContext::new(big);
    assert_eq!(rect(rf(0.0, 0.0, 1.0e9, 1.0e9), &c).unwrap_err(), SelectionError::Pigment(Error::BudgetExceeded));
    assert!(matches!(invert(&CoveragePlane { bounds: CANVAS, data: vec![] }, &c).unwrap_err(), SelectionError::Pigment(Error::BudgetExceeded | Error::Overflow)));
    assert!(CoveragePlane::filled(big, 0, &SelectionLimits::default()).is_err());
    // aggregate_over_budget: 10^7 points refuse at the point cap; the sum of planes is bounded too.
    let pts = vec![[1.0f64, 1.0]; 10_000_000];
    assert_eq!(polygon(&pts, FillRule::EvenOdd, &ctx()).unwrap_err(), SelectionError::Pigment(Error::BudgetExceeded));
    let tight = SelectionLimits { max_bytes: 100, ..SelectionLimits::default() };
    let a = CoveragePlane::filled(Rect { x: 0, y: 0, width: 5, height: 5 }, 1, &tight).unwrap();
    let b = CoveragePlane::filled(Rect { x: 100, y: 0, width: 5, height: 5 }, 1, &tight).unwrap();
    assert_eq!(combine(Some(&a), &b, CombineMode::Extend, &tight).unwrap_err(), SelectionError::Pigment(Error::BudgetExceeded));
    // Non-finite and out-of-range inputs.
    assert_eq!(rect(rf(f64::NAN, 0.0, 1.0, 1.0), &ctx()).unwrap_err(), SelectionError::Pigment(Error::Nonfinite));
    assert_eq!(polygon(&[[0.0, 0.0], [f64::INFINITY, 1.0], [1.0, 1.0]], FillRule::EvenOdd, &ctx()).unwrap_err(), SelectionError::Pigment(Error::Nonfinite));
    let view = ColourView { bounds: Rect { x: 0, y: 0, width: 0, height: 0 }, colour: &[], alpha: &[] };
    assert_eq!(colour_range(&view, [0.0; 3], f32::NAN, &ctx()).unwrap_err(), SelectionError::Pigment(Error::Nonfinite));
    let p = rect(rf(1.0, 1.0, 2.0, 2.0), &ctx()).unwrap();
    assert_eq!(feather(&p, 10_001.0, &ctx()).unwrap_err(), SelectionError::Coverage(CoverageError::FeatherOutOfRange));
}
