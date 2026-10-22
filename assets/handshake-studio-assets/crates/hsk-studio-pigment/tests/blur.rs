//! Layer blur and backdrop blur maths (`blur_math`, lane SMB-COMPOSITE-NEW-KINDS slice 1) and the
//! envelope rules of `LayerExtension::blur`. Expected values are derived by hand from the shared
//! Gaussian (`coverage_math::feather_kernel`, sigma = radius / 2).
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blur_math::*;
use hsk_studio_pigment::coverage_math::{feather_kernel, feather_support};
use hsk_studio_pigment::effect_math::{ColourOverlayOp, EffectOp};
use hsk_studio_pigment::render_plan::*;

const WIDE: BlurLimits = BlurLimits { max_bytes: 1 << 30, max_work: 1 << 40 };

fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect { x, y, width, height }
}

#[test]
fn layer_blur_uses_the_shared_gaussian_and_conserves_premultiplied_mass() {
    for r in [0.0_f32, 1.0, 2.5, 7.0] {
        let b = LayerBlur::new(BlurScope::Layer, r);
        assert_eq!(b.taps().unwrap(), feather_kernel(r).unwrap(), "radius {r}");
        assert_eq!(b.support_px().unwrap(), feather_support(r).unwrap());
    }
    // radius 2 -> sigma 1 -> support 3, 7 taps.
    let b = LayerBlur::new(BlurScope::Layer, 2.0);
    let taps = b.taps().unwrap();
    assert_eq!(taps.len(), 7);
    let k = 3_i64;
    // One opaque orange pixel at (10, 20) in a 1x1 plane; out window covers its full support.
    let src = vec![[1.0, 0.5, 0.0, 1.0]];
    let out = rect(10 - k, 20 - k, 7, 7);
    let got = blur_window(&src, rect(10, 20, 1, 1), out, &taps, WIDE).unwrap();
    let mut mass = 0.0_f64;
    for j in 0..7 {
        for i in 0..7 {
            let p = got[j * 7 + i];
            // Hand value: alpha = tap_i * tap_j (each pass rounds once to binary32).
            let want = (f64::from(taps[j]) * f64::from((f64::from(taps[i]) * 1.0) as f32)) as f32;
            assert_eq!(p[3], want, "alpha at ({i},{j})");
            // Colour of a single-colour source is preserved where inked (premultiplied blur).
            assert!((p[0] - 1.0).abs() < 1e-6 && (p[1] - 0.5).abs() < 1e-6 && p[2] == 0.0, "{p:?}");
            mass += f64::from(p[3]);
        }
    }
    assert!((mass - 1.0).abs() < 1e-5, "alpha mass {mass}");
    // Beyond the support nothing arrives; radius 0 is the identity.
    let far = blur_window(&src, rect(10, 20, 1, 1), rect(14, 20, 1, 1), &taps, WIDE).unwrap();
    assert_eq!(far, vec![[0.0; 4]]);
    let id = LayerBlur::new(BlurScope::Layer, 0.0).taps().unwrap();
    let plane = vec![[0.25, 0.5, 0.75, 0.5], [0.0; 4], [1.0, 1.0, 1.0, 1.0], [0.1, 0.2, 0.3, 0.4]];
    assert_eq!(blur_window(&plane, rect(0, 0, 2, 2), rect(0, 0, 2, 2), &id, WIDE).unwrap()[3], [0.1, 0.2, 0.3, 0.4]);
    // The window is chunk-independent: one 4x4 window equals its four 2x2 quarters.
    let whole = blur_window(&plane, rect(0, 0, 2, 2), rect(-1, -1, 4, 4), &taps, WIDE).unwrap();
    for (qx, qy) in [(-1, -1), (1, -1), (-1, 1), (1, 1)] {
        let q = blur_window(&plane, rect(0, 0, 2, 2), rect(qx, qy, 2, 2), &taps, WIDE).unwrap();
        for j in 0..2 {
            for i in 0..2 {
                let (wx, wy) = ((qx + 1) as usize + i, (qy + 1) as usize + j);
                assert_eq!(q[j * 2 + i], whole[wy * 4 + wx]);
            }
        }
    }
}

#[test]
fn hostile_radii_sizes_and_budgets_are_refused_before_allocation() {
    for r in [f32::NAN, f32::INFINITY, -1.0, MAX_BLUR_RADIUS_PX + 0.5] {
        assert_eq!(LayerBlur::new(BlurScope::Backdrop, r).taps(), Err(BlurError::Radius), "{r}");
    }
    assert_eq!(LayerBlur::new(BlurScope::Layer, MAX_BLUR_RADIUS_PX).taps().unwrap().len(), 3001);
    let taps = LayerBlur::new(BlurScope::Layer, 4.0).taps().unwrap();
    // dims_u32_max: a u32::MAX square window overflows or exceeds any budget, never allocates.
    let huge = rect(0, 0, u32::MAX, u32::MAX);
    assert!(matches!(
        blur_window(&[], rect(0, 0, 0, 0), huge, &taps, WIDE),
        Err(BlurError::OverBudget { .. } | BlurError::Overflow)
    ));
    // aggregate_over_budget: the exact predicted bytes pass, one byte less is refused.
    let out = rect(0, 0, 16, 9);
    let (bytes, work) = blur_window_cost(out, taps.len()).unwrap();
    assert_eq!(bytes, (16 * (9 + 2 * 6) + 16 * 9) * BLUR_SAMPLE_BYTES);
    assert_eq!(work, (16 * (9 + 2 * 6) + 16 * 9) * 13);
    let src = vec![[0.5; 4]; 4];
    let exact = BlurLimits { max_bytes: bytes, max_work: work };
    assert!(blur_window(&src, rect(0, 0, 2, 2), out, &taps, exact).is_ok());
    assert_eq!(
        blur_window(&src, rect(0, 0, 2, 2), out, &taps, BlurLimits { max_bytes: bytes - 1, ..exact }),
        Err(BlurError::OverBudget { needed: bytes, limit: bytes - 1 })
    );
    assert_eq!(
        blur_window(&src, rect(0, 0, 2, 2), out, &taps, BlurLimits { max_work: work - 1, ..exact }),
        Err(BlurError::OverBudget { needed: work, limit: work - 1 })
    );
    // Out-of-domain samples and a length/rect mismatch are typed.
    assert_eq!(blur_window(&[[1.5, 0.0, 0.0, 1.0]], rect(0, 0, 1, 1), out, &taps, WIDE), Err(BlurError::OutOfDomain));
    assert_eq!(blur_window(&src, rect(0, 0, 3, 2), out, &taps, WIDE), Err(BlurError::Shape));
}

#[test]
fn backdrop_mix_is_a_premultiplied_lerp() {
    let b = [1.0, 0.0, 0.0, 1.0];
    let l = [0.0, 0.0, 1.0, 0.5];
    assert_eq!(backdrop_mix(b, l, 0.0).unwrap(), b);
    assert_eq!(backdrop_mix(b, l, 1.0).unwrap(), l);
    // k = 0.5: P = (0.5, 0, 0.25), A = 0.75 -> straight (2/3, 0, 1/3).
    let m = backdrop_mix(b, l, 0.5).unwrap();
    assert_eq!(m[3], 0.75);
    assert!((m[0] - 2.0 / 3.0).abs() < 1e-7 && m[1] == 0.0 && (m[2] - 1.0 / 3.0).abs() < 1e-7);
    assert_eq!(backdrop_mix([0.0; 4], [0.0; 4], 0.7).unwrap(), [0.0; 4]);
    assert_eq!(backdrop_mix(b, l, 1.5), Err(BlurError::OutOfDomain));
    assert_eq!(backdrop_mix([f32::NAN, 0.0, 0.0, 1.0], l, 0.5), Err(BlurError::OutOfDomain));
}

fn plan(isolated: bool) -> RenderPlan {
    let r = rect(0, 0, 4, 4);
    let layer = |input: u32, mode: StudioBlendMode| Layer {
        input: PlanNodeId(input),
        mode,
        opacity: 1.0,
        fill_opacity: 1.0,
        mask: None,
    };
    RenderPlan {
        revision: 1,
        target: OutputTarget {
            extent: r,
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded },
        },
        nodes: vec![
            PlanNode::Solid { rect: r, rgba: [0.2, 0.4, 0.6, 1.0] },
            PlanNode::Solid { rect: rect(1, 1, 2, 2), rgba: [1.0, 1.0, 1.0, 0.5] },
            PlanNode::Group(Group {
                isolated,
                layers: vec![layer(0, StudioBlendMode::Normal), layer(1, StudioBlendMode::Normal)],
            }),
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![layer(
                    2,
                    if isolated { StudioBlendMode::Normal } else { StudioBlendMode::PassThrough },
                )],
            }),
        ],
        root: PlanNodeId(3),
    }
}

#[test]
fn blur_envelope_entries_validate_report_and_refuse_typed() {
    let blur = |scope| LayerExtension::new(PlanNodeId(2), 1).with_blur(LayerBlur::new(scope, 3.0));
    let ok = PlanExtensions::from_layers(vec![blur(BlurScope::Layer)]);
    assert_eq!(ok.validate(&plan(true)), Ok(vec![ExtOperator::Blur(BlurScope::Layer)]));
    let back = PlanExtensions::from_layers(vec![blur(BlurScope::Backdrop)]);
    assert_eq!(back.validate(&plan(true)), Ok(vec![ExtOperator::Blur(BlurScope::Backdrop)]));
    assert_eq!(ExtOperator::Blur(BlurScope::Layer).key(), "blur_layer");
    assert_eq!(ExtOperator::Blur(BlurScope::Backdrop).key(), "blur_backdrop");
    assert_eq!(ExtOperator::Blur(BlurScope::Layer).intrinsic(), Exactness::Approximate);
    // A backdrop blur in a pass-through group has no group result as its backdrop.
    assert_eq!(back.validate(&plan(false)), Err(PlanError::BlurCombination { node: 2 }));
    // Blur with effects on the same layer.
    let with_fx = PlanExtensions::from_layers(vec![blur(BlurScope::Layer).with_effects(vec![
        EffectOp::ColourOverlay(ColourOverlayOp::new([1.0, 0.0, 0.0], StudioBlendMode::Normal, 1.0)),
    ])]);
    assert_eq!(with_fx.validate(&plan(true)), Err(PlanError::BlurCombination { node: 2 }));
    assert_eq!(PlanError::BlurCombination { node: 2 }.code(), "plan_blur_combination");
    // Hostile radii.
    let bad = |r| PlanExtensions::from_layers(vec![
        LayerExtension::new(PlanNodeId(2), 1).with_blur(LayerBlur::new(BlurScope::Layer, r)),
    ]);
    assert_eq!(bad(f32::NAN).validate(&plan(true)), Err(PlanError::NonFinite { node: 2 }));
    assert_eq!(bad(1000.5).validate(&plan(true)), Err(PlanError::OutOfRange { node: 2 }));
    // An entry without blur serialises without the member (digests of existing plans unchanged).
    let plain = serde_json::to_string(&LayerExtension::new(PlanNodeId(2), 1)).unwrap();
    assert!(!plain.contains("blur"), "{plain}");
    assert!(serde_json::to_string(&blur(BlurScope::Layer)).unwrap().contains("\"blur\""));
}
