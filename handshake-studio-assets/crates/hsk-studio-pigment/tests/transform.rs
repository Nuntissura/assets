//! Layer transform operator (lane SMB-PIGMENT-TRANSFORM slice 1): the binary64 reference sampler
//! (`transform_math`), envelope validation and shared admission. Expected values are derived by hand
//! from the rules in the `transform_math` module documentation.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::render_plan::*;
use hsk_studio_pigment::transform_math::*;
use hsk_studio_pigment::{Mask, Rect};

fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

/// Deterministic texels with distinct bits: colour and alpha in `[0, 1]`, alpha 0 at (0, 0).
fn texel(w: u32) -> impl Fn(u32, u32) -> [f32; 4] {
    move |i, j| {
        let k = (j * w + i) as f32;
        [
            (k * 0.137).fract(),
            (k * 0.291 + 0.05).fract(),
            (k * 0.377 + 0.11).fract(),
            if k == 0.0 { 0.0 } else { (k * 0.173 + 0.2).fract() },
        ]
    }
}

const FILTERS: [ResampleFilter; 3] = [
    ResampleFilter::NearestNeighbour,
    ResampleFilter::Bilinear,
    ResampleFilter::Bicubic,
];

fn bits(v: &[[f32; 4]]) -> Vec<[u32; 4]> {
    v.iter().map(|p| p.map(f32::to_bits)).collect()
}

#[test]
fn identity_and_integer_translation_are_byte_identical() {
    let src = rect(3, -2, 5, 4);
    let read = texel(5);
    let original: Vec<[f32; 4]> = (0..4)
        .flat_map(|j| (0..5).map(move |i| (i, j)))
        .map(|(i, j)| read(i, j))
        .collect();
    for filter in FILTERS {
        for edge in [EdgeMode::Transparent, EdgeMode::Clamp] {
            let identity = LayerTransform::new(Affine2::IDENTITY, filter).with_edge(edge);
            let out = resample_reference(&identity, src, src, &read).unwrap();
            assert_eq!(bits(&out), bits(&original), "identity {filter:?} {edge:?}");
            // Integer translation (+7, -3): the moved region equals the source bits, the pixels
            // around it are transparent zeros (Transparent and Clamp agree on pixel centres).
            let moved = LayerTransform::new(Affine2::translate(7.0, -3.0), filter).with_edge(edge);
            let out = resample_reference(&moved, src, rect(10, -5, 5, 4), &read).unwrap();
            assert_eq!(bits(&out), bits(&original), "translate {filter:?} {edge:?}");
            let ring = resample_reference(&moved, src, rect(9, -6, 1, 6), &read).unwrap();
            assert!(ring.iter().all(|p| *p == [0.0; 4]), "outside {filter:?} {edge:?}");
            // The prepared bounds contain the moved rect.
            let bounds = PreparedTransform::new(&moved, src).unwrap().bounds();
            assert!(bounds.contains(rect(10, -5, 5, 4)).unwrap());
        }
    }
}

#[test]
fn quarter_and_half_turns_are_exact_copies() {
    // 3x2 source at the origin. Clockwise quarter turn about the origin, then translate by the
    // source height: destination pixel (x, y) holds source texel (y, 2 - 1 - x).
    let src = rect(0, 0, 3, 2);
    let read = texel(3);
    let quarter = Affine2::from_parts(0.0, 0.0, 1.0, 1.0, 90.0, 2.0, 0.0);
    assert_eq!(quarter.m, [0.0, -1.0, 2.0, 1.0, 0.0, 0.0]);
    let half = Affine2::from_parts(1.5, 1.0, 1.0, 1.0, 180.0, 0.0, 0.0);
    for filter in FILTERS {
        let out =
            resample_reference(&LayerTransform::new(quarter, filter), src, rect(0, 0, 2, 3), &read)
                .unwrap();
        for y in 0..3u32 {
            for x in 0..2u32 {
                let want = read(y, 1 - x);
                assert_eq!(
                    out[(y * 2 + x) as usize].map(f32::to_bits),
                    want.map(f32::to_bits),
                    "quarter {filter:?} ({x},{y})"
                );
            }
        }
        // Half turn about the centre of the source maps it onto itself, mirrored both ways.
        let out = resample_reference(&LayerTransform::new(half, filter), src, src, &read).unwrap();
        for y in 0..2u32 {
            for x in 0..3u32 {
                assert_eq!(
                    out[(y * 3 + x) as usize].map(f32::to_bits),
                    read(2 - x, 1 - y).map(f32::to_bits),
                    "half {filter:?} ({x},{y})"
                );
            }
        }
    }
}

#[test]
fn known_affine_cases_match_hand_values() {
    // Opaque red at texel 0, transparent (black) at texel 1, then opaque blue: 3x1 source.
    let src = rect(0, 0, 3, 1);
    let px = |i: u32, _j: u32| match i {
        0 => [1.0, 0.0, 0.0, 1.0],
        1 => [0.0, 0.0, 0.0, 0.0],
        _ => [0.0, 0.0, 1.0, 1.0],
    };
    // Half-pixel shift right, bilinear: destination 1 samples u = 0.5 -> weights 1/2, 1/2 between
    // red (alpha 1) and transparent: premultiplied A = 0.5, colour = red (not darkened).
    let shift = LayerTransform::new(Affine2::translate(0.5, 0.0), ResampleFilter::Bilinear);
    let out = resample_reference(&shift, src, rect(0, 0, 4, 1), px).unwrap();
    assert_eq!(out[0], [1.0, 0.0, 0.0, 0.5]); // u = -0.5: half of red, half outside
    assert_eq!(out[1], [1.0, 0.0, 0.0, 0.5]);
    assert_eq!(out[2], [0.0, 0.0, 1.0, 0.5]);
    assert_eq!(out[3], [0.0, 0.0, 1.0, 0.5]);
    // Same shift with Clamp: u = -0.5 is inside the hard edge and reads red twice.
    let clamp = shift.with_edge(EdgeMode::Clamp);
    let out = resample_reference(&clamp, src, rect(0, 0, 4, 1), px).unwrap();
    assert_eq!(out[0], [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(out[3], [0.0; 4]); // u = 2.5: outside [-0.5, 2.5)
    // Nearest, scale 2 about the origin: destination x samples u = (x + 0.5) / 2 - 0.5, so
    // x = 0 -> -0.25 -> texel 0, x = 1 -> 0.25 -> 0, x = 2 -> 0.75 -> 1, x = 3 -> 1.25 -> 1.
    let up = LayerTransform::new(
        Affine2 {
            m: [2.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        },
        ResampleFilter::NearestNeighbour,
    );
    let out = resample_reference(&up, src, rect(0, 0, 6, 1), px).unwrap();
    assert_eq!(out[0], px(0, 0));
    assert_eq!(out[1], px(0, 0));
    assert_eq!(out[2], px(1, 0));
    assert_eq!(out[3], px(1, 0));
    assert_eq!(out[5], px(2, 0));
    // Catmull-Rom weights at t = 0.5 are -1/16, 9/16, 9/16, -1/16 (exact in binary64).
    assert_eq!(catmull_rom(0.5), [-0.0625, 0.5625, 0.5625, -0.0625]);
    assert_eq!(catmull_rom(0.0), [0.0, 1.0, 0.0, 0.0]);
    // Bicubic on an opaque grey ramp 0, 0.25, 0.5, 0.75 (4x1), half-pixel shift: destination 2
    // samples u = 1.5: -1/16*0 + 9/16*0.25 + 9/16*0.5 - 1/16*0.75 = 0.375.
    let ramp = |i: u32, _j: u32| {
        let g = i as f32 * 0.25;
        [g, g, g, 1.0]
    };
    let bicubic = LayerTransform::new(Affine2::translate(0.5, 0.0), ResampleFilter::Bicubic);
    let out = resample_reference(&bicubic, rect(0, 0, 4, 1), rect(2, 0, 1, 1), ramp).unwrap();
    assert_eq!(out[0], [0.375, 0.375, 0.375, 1.0]);
    // Overshoot is clamped: a step 0, 0, 1, 1 (opaque) sampled at u = 1.5 is 0.5.
    let step = |i: u32, _j: u32| if i < 2 { [0.0, 0.0, 0.0, 1.0] } else { [1.0, 1.0, 1.0, 1.0] };
    let out = resample_reference(&bicubic, rect(0, 0, 4, 1), rect(2, 0, 3, 1), step).unwrap();
    assert_eq!(out[0], [0.5, 0.5, 0.5, 1.0]);
    for p in &out {
        assert!(p.iter().all(|c| (0.0..=1.0).contains(c)), "{p:?}");
    }
    // u = 2.5: taps 1..=4 with weights -1/16 9/16 9/16 -1/16, tap 4 outside (transparent):
    // A = 17/16 -> alpha 1; P = 18/16 -> colour 18/17 clamped to 1.
    assert_eq!(out[1], [1.0, 1.0, 1.0, 1.0]);
    // u = 3.5: taps 2..=5, only 2 (-1/16) and 3 (9/16) inside: A = 1/2, P = 1/2 -> white at 1/2.
    assert_eq!(out[2], [1.0, 1.0, 1.0, 0.5]);
}

fn plan_with_tile() -> RenderPlan {
    let r = rect(0, 0, 4, 4);
    RenderPlan {
        revision: 3,
        target: OutputTarget {
            extent: r,
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: ColourTag {
                profile_sha256: [7; 32],
                blend_space: BlendSpace::LinearLight,
            },
        },
        nodes: vec![
            PlanNode::Tile(TileInput {
                key: SourceKey(1),
                placement: rect(0, 0, 2, 2),
                format: SampleFormat::RgbF32Le,
                alpha: AlphaConvention::Straight,
                colour_sha256: [1; 32],
                alpha_sha256: [2; 32],
            }),
            PlanNode::Solid {
                rect: r,
                rgba: [0.1, 0.2, 0.3, 1.0],
            },
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![
                    Layer {
                        input: PlanNodeId(1),
                        mode: StudioBlendMode::Normal,
                        opacity: 1.0,
                        fill_opacity: 1.0,
                        mask: None,
                    },
                    Layer {
                        input: PlanNodeId(0),
                        mode: StudioBlendMode::Normal,
                        opacity: 1.0,
                        fill_opacity: 1.0,
                        mask: None,
                    },
                ],
            }),
            // Unreachable group with a tile layer.
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![Layer {
                    input: PlanNodeId(0),
                    mode: StudioBlendMode::Normal,
                    opacity: 1.0,
                    fill_opacity: 1.0,
                    mask: None,
                }],
            }),
        ],
        root: PlanNodeId(2),
    }
}

fn env(group: u32, layer: u32, t: LayerTransform) -> PlanExtensions {
    PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(group), layer).with_transform(t)])
}

#[test]
fn hostile_matrices_and_combinations_are_refused_typed() {
    let plan = plan_with_tile();
    let bad = |m: [f64; 6]| {
        env(2, 1, LayerTransform::new(Affine2 { m }, ResampleFilter::Bicubic)).validate(&plan)
    };
    let matrix = |error| Err(PlanError::TransformMatrix { node: 2, error });
    assert_eq!(bad([f64::NAN, 0.0, 0.0, 0.0, 1.0, 0.0]), matrix(TransformError::NonFinite));
    assert_eq!(bad([1.0, 0.0, f64::INFINITY, 0.0, 1.0, 0.0]), matrix(TransformError::NonFinite));
    assert_eq!(bad([1.0, 2.0, 0.0, 0.5, 1.0, 0.0]), matrix(TransformError::Singular));
    assert_eq!(bad([0.0; 6]), matrix(TransformError::Singular));
    assert_eq!(bad([1.0e9, 0.0, 0.0, 0.0, 1.0, 0.0]), matrix(TransformError::ScaleOutOfRange));
    assert_eq!(bad([1.0e-6, 0.0, 0.0, 0.0, 1.0, 0.0]), matrix(TransformError::ScaleOutOfRange));
    assert_eq!(
        bad([1.0, 0.0, 1.0e12, 0.0, 1.0, 0.0]),
        matrix(TransformError::TranslationOutOfRange)
    );
    // 1024 x on a 2 px tile is fine; on a 2048 px source it overflows the per-side bound.
    assert!(bad([1024.0, 0.0, 0.0, 0.0, 1.0, 0.0]).is_ok());
    let wide = LayerTransform::new(
        Affine2 {
            m: [1024.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        },
        ResampleFilter::Bilinear,
    );
    assert_eq!(
        PreparedTransform::new(&wide, rect(0, 0, 2048, 1)),
        Err(TransformError::BoundsOutOfRange)
    );
    assert_eq!(
        PlanError::TransformMatrix {
            node: 2,
            error: TransformError::Singular
        }
        .code(),
        "plan_transform_matrix"
    );
    // Target and combinations.
    let ok = LayerTransform::new(Affine2::translate(0.5, 0.25), ResampleFilter::Bilinear);
    assert_eq!(
        env(2, 0, ok).validate(&plan),
        Err(PlanError::TransformTarget { node: 2 })
    );
    let with_fx = PlanExtensions::from_layers(vec![
        LayerExtension::new(PlanNodeId(2), 1)
            .with_transform(ok)
            .with_effects(vec![hsk_studio_pigment::effect_math::EffectOp::ColourOverlay(
                hsk_studio_pigment::effect_math::ColourOverlayOp::new(
                    [1.0, 0.0, 0.0],
                    StudioBlendMode::Normal,
                    1.0,
                ),
            )]),
    ]);
    assert_eq!(
        with_fx.validate(&plan),
        Err(PlanError::TransformCombination { node: 2 })
    );
    assert_eq!(
        env(2, 1, ok.with_mask(true)).validate(&plan),
        Err(PlanError::TransformCombination { node: 2 })
    );
    // Aggregate budget: one more transformed layer than MAX_TRANSFORMS_PER_PLAN.
    let mut big = plan_with_tile();
    let n = MAX_TRANSFORMS_PER_PLAN + 1;
    big.nodes[3] = PlanNode::Group(Group {
        isolated: true,
        layers: (0..n)
            .map(|_| Layer {
                input: PlanNodeId(0),
                mode: StudioBlendMode::Normal,
                opacity: 1.0,
                fill_opacity: 1.0,
                mask: None,
            })
            .collect(),
    });
    let many = PlanExtensions::from_layers(
        (0..n as u32)
            .map(|l| LayerExtension::new(PlanNodeId(3), l).with_transform(ok))
            .collect(),
    );
    assert_eq!(many.validate(&big), Err(PlanError::TransformLimit { node: 3 }));
    // Reference output budget is checked before allocation.
    assert_eq!(
        resample_reference(&ok, rect(0, 0, 2, 2), rect(0, 0, 1 << 14, 1 << 13), texel(2)),
        Err(TransformError::BudgetExceeded)
    );
}

struct Fake(Exactness);
impl RenderBackend for Fake {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            name: "fake",
            version: "0",
            device: DeviceClass::Cpu,
            adapter: String::new(),
            generation: 0,
        }
    }
    fn exactness(&self, _: Operator) -> Exactness {
        Exactness::Exact
    }
    fn extension_exactness(&self, _: ExtOperator) -> Exactness {
        self.0
    }
    fn execute(
        &self,
        _: &RenderPlan,
        _: &ExecuteRequest,
        _: &dyn PlanSources,
        _: &CancellationToken,
        _: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        Err(BackendError::Unavailable("fake"))
    }
}
#[allow(dead_code)]
struct NoSources;
impl PlanSources for NoSources {
    fn tile(&self, _: SourceKey) -> Option<TilePlanes<'_>> {
        None
    }
    fn mask(&self, _: SourceKey) -> Option<Mask<'_>> {
        None
    }
}

#[test]
fn admission_reports_the_filter_operator_and_reachability() {
    let plan = plan_with_tile();
    let cancel = CancellationToken::default();
    let req = |min| ExecuteRequest {
        expected_revision: 3,
        min_exactness: min,
        deadline: None,
        max_working_bytes: 1 << 20,
    };
    let t = LayerTransform::new(Affine2::from_parts(1.0, 1.0, 1.5, 1.5, 30.0, 0.25, 0.0), ResampleFilter::Bicubic);
    let e = env(2, 1, t);
    assert_eq!(e.validate(&plan), Ok(vec![ExtOperator::Transform(ResampleFilter::Bicubic)]));
    // A backend without support is refused under the filter key before any sink call.
    assert_eq!(
        admit_extended(&plan, &e, &Fake(Exactness::Unsupported), &req(Exactness::Approximate), &cancel),
        Err(BackendError::Unsupported {
            operator: "transform_bicubic",
            exactness: Exactness::Unsupported
        })
    );
    // Supported: the ledger records the Approximate class; an Exact floor refuses it.
    let ledger =
        admit_extended(&plan, &e, &Fake(Exactness::Exact), &req(Exactness::Approximate), &cancel)
            .unwrap();
    assert!(ledger
        .operators
        .iter()
        .any(|o| o.key == "transform_bicubic" && o.exactness == Exactness::Approximate));
    assert!(admit_extended(&plan, &e, &Fake(Exactness::Exact), &req(Exactness::Exact), &cancel).is_err());
    // An entry on the unreachable group asks for nothing (structure still checked).
    let unreachable = env(3, 0, t);
    assert_eq!(unreachable.validate(&plan), Ok(vec![]));
    let singular = LayerTransform::new(Affine2 { m: [0.0; 6] }, ResampleFilter::Bilinear);
    assert!(env(3, 0, singular).validate(&plan).is_err());
    // No transform: the envelope digests as before (the field is skipped).
    let plain = PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(2), 1)]);
    let json = serde_json::to_string(&plain).unwrap();
    assert!(!json.contains("transform"), "{json}");
    assert_ne!(plain.digest_with(&plan).unwrap(), e.digest_with(&plan).unwrap());
    // The filter keys and spec values (STU-RAS-141).
    assert_eq!(
        FILTERS.map(|f| (f.key(), f.spec_value())),
        [("nearest_neighbour", 2), ("bilinear", 3), ("bicubic", 4)]
    );
}
