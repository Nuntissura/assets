//! Effects wave 1 (drop shadow, colour overlay, solid stroke): reference maths, envelope
//! validation and shared admission. Every expected value is derived by hand in the test body.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::adjust_math::AdjustmentOp;
use hsk_studio_pigment::effect_math::{
    ColourOverlayOp, DropShadowOp, EffectBlurKernel, EffectError, EffectKind, EffectOp,
    EffectPlane, InnerShadowOp, LayerParams, StrokeOp, StrokePosition, effect_blur_kernel,
    effect_blur_support, effect_support_px, reference_matte, render_layer_reference,
    render_reference, shadow_offset,
};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_pigment::{Mask, Rect};

const TOL: f32 = 5e-6;

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() <= TOL
}

/// A `w` x `h` plane; `f(x, y)` gives the straight pixel `(rgb, alpha)`.
fn plane(w: u32, h: u32, f: impl Fn(u32, u32) -> ([f32; 3], f32)) -> EffectPlane {
    let (mut rgb, mut alpha) = (Vec::new(), Vec::new());
    for y in 0..h {
        for x in 0..w {
            let (c, a) = f(x, y);
            rgb.push(c);
            alpha.push(a);
        }
    }
    EffectPlane::new(w, h, rgb, alpha).unwrap()
}
fn at(m: &[f32], w: u32, x: u32, y: u32) -> f32 {
    m[(y * w + x) as usize]
}
const BLACK: [f32; 3] = [0.0; 3];
const RED: [f32; 3] = [1.0, 0.0, 0.0];
const BLUE: [f32; 3] = [0.0, 0.0, 1.0];
const NORMAL: StudioBlendMode = StudioBlendMode::Normal;

fn shadow(opacity: f32) -> DropShadowOp {
    DropShadowOp::new(BLACK, NORMAL, opacity)
}

/// Acceptance 1. One opaque pixel, distance 0, spread 0, size 0: the matte is the alpha, it sits
/// under the content (hidden at alpha 1), and at fill 0 it shows at the effect opacity; the layer
/// opacity fades it, knock-out removes it where the layer shape is.
#[test]
fn drop_shadow_equals_matte_and_hides_under_content() {
    let content = plane(1, 1, |_, _| (RED, 1.0));
    let op = EffectOp::DropShadow(shadow(0.5));
    assert_eq!(reference_matte(&content, &op).unwrap(), vec![1.0]);

    // Normal layer, fill 1: shadow (0, 0, 0, 0.5) then opaque red over it -> red, alpha 1.
    let hidden = render_reference(&content, &op).unwrap();
    let p = hidden.pixel(0, 0).unwrap();
    assert_eq!((p.rgb, p.a), (RED, 1.0));

    let backdrop = EffectPlane::transparent(1, 1).unwrap();
    let fill0 = LayerParams {
        mode: NORMAL,
        opacity: 1.0,
        fill_opacity: 0.0,
    };
    // Fill 0: content contributes alpha 0, the shadow stays at the effect opacity 0.5.
    let p = render_layer_reference(&backdrop, &content, fill0, &[op])
        .unwrap()
        .pixel(0, 0)
        .unwrap();
    assert_eq!((p.rgb, p.a), (BLACK, 0.5));
    // Layer opacity 0.5 fades the effect too: 0.5 * 0.5.
    let faded = LayerParams {
        opacity: 0.5,
        ..fill0
    };
    let p = render_layer_reference(&backdrop, &content, faded, &[op])
        .unwrap()
        .pixel(0, 0)
        .unwrap();
    assert!(close(p.a, 0.25));
    // layer_knocks_out: alpha *= 1 - layer alpha = 0, so the fill-0 layer shows nothing.
    let knock = EffectOp::DropShadow(shadow(0.5).with_layer_knocks_out(true));
    assert_eq!(reference_matte(&content, &knock).unwrap(), vec![0.0]);
    let p = render_layer_reference(&backdrop, &content, fill0, &[knock])
        .unwrap()
        .pixel(0, 0)
        .unwrap();
    assert_eq!(p.a, 0.0);
}

/// Acceptance 2. Offset (distance 2, angle 180) moves the matte by (+2, 0); size 2 blurs with
/// Photoshop's tent taps [1, 2, 1] / 4 (cross-lane re-pin by SMB-PS-ORACLE-PARITY-1: the size-2
/// drop shadow of the D5 oracle PS-C12 square_s2_sp0_* reproduces exactly with that kernel; the
/// former Gaussian sigma 1 taps differed by 29 codes); spread 1 grows a hard matte by `size`.
#[test]
fn drop_shadow_offset_blur_taps_and_spread() {
    // dx = round(-2 cos 180) = 2, dy = round(2 sin 180) = 0. Angle 120, distance 4:
    // dx = round(4 * 0.5) = 2, dy = round(4 * 0.8660) = round(3.46) = 3.
    assert_eq!(shadow_offset(180.0, 2.0), Ok((2, 0)));
    assert_eq!(shadow_offset(120.0, 4.0), Ok((2, 3)));
    // Photoshop rounds the half of 5 cos 120 away from zero (D5 PS-C12 *_a120_d5: (3, 4)).
    assert_eq!(shadow_offset(120.0, 5.0), Ok((3, 4)));
    let one = plane(5, 1, |x, _| (RED, if x == 1 { 1.0 } else { 0.0 }));
    let moved = EffectOp::DropShadow(shadow(1.0).with_offset(180.0, 2.0));
    assert_eq!(
        reference_matte(&one, &moved).unwrap(),
        vec![0.0, 0.0, 0.0, 1.0, 0.0]
    );

    // Opaque column x = 4 of a 9 x 9 plane; size 2 -> tent taps 0.25, 0.5, 0.25 (half-width 2:
    // weights 2 - |i| for |i| < 2). Row 4 sees all three vertical samples inside the plane, so the
    // vertical pass keeps 1.
    let column = plane(9, 9, |x, _| (RED, if x == 4 { 1.0 } else { 0.0 }));
    let blur = EffectOp::DropShadow(shadow(1.0).with_size(2.0));
    let m = reference_matte(&column, &blur).unwrap();
    for (x, want) in [
        (4, 0.5),
        (3, 0.25),
        (5, 0.25),
        (2, 0.0),
        (6, 0.0),
        (1, 0.0),
        (7, 0.0),
        (0, 0.0),
        (8, 0.0),
    ] {
        assert!(
            close(at(&m, 9, x, 4), want),
            "x={x}: {} vs {want}",
            at(&m, 9, x, 4)
        );
    }

    // Spread 1, size 2: spread_px 2, blur 0 -> Photoshop's grow by 2 of the pixel (3, 3) (lane
    // SMB-PS-ORACLE-PARITY-2, D5 PS-C12 spread 100): coverage = clamp(2 + 1 - d, 0, 1) with the
    // 5 x 5 chamfer d (GROW_WEIGHTS w1 .99962, w2 1.41366, w5 2.23584): (2,0): d 1.99924 -> 1;
    // (1,1): d 1.41366 -> 1; (2,1): d w5 -> 0.76416; (2,2): d 2 w2 -> 0.17269; (3,0): d 3 w1 ->
    // 0.00114 (the one-pixel fringe beyond the spread).
    let dot = plane(7, 7, |x, y| (RED, if (x, y) == (3, 3) { 1.0 } else { 0.0 }));
    let hard = EffectOp::DropShadow(shadow(1.0).with_size(2.0).with_spread(1.0));
    let m = reference_matte(&dot, &hard).unwrap();
    assert_eq!(at(&m, 7, 3, 3), 1.0);
    assert_eq!(at(&m, 7, 5, 3), 1.0);
    assert_eq!(at(&m, 7, 4, 4), 1.0);
    assert!(close(at(&m, 7, 5, 4), 0.764_164));
    assert!(close(at(&m, 7, 5, 5), 0.172_686_1));
    assert!(close(at(&m, 7, 6, 3), 0.001_138_4));
    // Spread has no effect at size 0 (Adobe community evidence, brief): the matte is the alpha.
    let flat = EffectOp::DropShadow(shadow(1.0).with_spread(1.0));
    assert_eq!(reference_matte(&dot, &flat).unwrap(), dot.alpha().to_vec());
}

/// Acceptance 3. Red, Normal, 50% over blue opaque -> (0.5, 0, 0.5); over transparent it stays
/// transparent (clipped to the layer alpha); at fill 0 it keeps full strength.
#[test]
fn colour_overlay_clips_to_alpha_and_ignores_fill() {
    let content = plane(2, 1, |x, _| if x == 0 { (BLUE, 1.0) } else { (BLACK, 0.0) });
    let op = EffectOp::ColourOverlay(ColourOverlayOp::new(RED, NORMAL, 0.5));
    let out = render_reference(&content, &op).unwrap();
    // a_s = 0.5, a_b = 1: Cr = 0.5 * red + 0.5 * blue.
    let p = out.pixel(0, 0).unwrap();
    assert_eq!((p.rgb, p.a), ([0.5, 0.0, 0.5], 1.0));
    assert_eq!(out.pixel(1, 0).unwrap().a, 0.0);

    // Fill 0: the content vanishes, the overlay remains at its own 50% over the layer shape.
    let fill0 = LayerParams {
        mode: NORMAL,
        opacity: 1.0,
        fill_opacity: 0.0,
    };
    let backdrop = EffectPlane::transparent(2, 1).unwrap();
    let p = render_layer_reference(&backdrop, &content, fill0, &[op])
        .unwrap()
        .pixel(0, 0)
        .unwrap();
    assert_eq!((p.rgb, p.a), (RED, 0.5));
}

/// Acceptance 4 (re-pinned by lane SMB-PS-ORACLE-PARITY-2 to Photoshop's grow, D5 PS-C13). Outside
/// 1 px around a 3 x 3 square at (2..=4, 2..=4) of a 7 x 7 plane: the 12 edge-adjacent pixels are
/// 1, the 4 corner pixels (chamfer distance w2 = 1.41366) are 2 - w2 = 0.58634, the pixels two
/// steps out along an axis keep the fringe 2 - 2 w1 = 0.00076, everything else is 0. Inside gives
/// the inner ring of 8 (and the fringe at the centre); centre size 1 straddles the edge with
/// 1.5 - w1 = 0.50038 on both rings and 0.08634 on the outer corners.
#[test]
fn stroke_outside_inside_and_centre_bands() {
    let sq = plane(7, 7, |x, y| {
        (
            RED,
            if (2..=4).contains(&x) && (2..=4).contains(&y) {
                1.0
            } else {
                0.0
            },
        )
    });
    let stroke =
        |size: f32, position| EffectOp::Stroke(StrokeOp::new(BLUE, NORMAL, 1.0, size, position));

    let out = reference_matte(&sq, &stroke(1.0, StrokePosition::Outside)).unwrap();
    for (x, y) in [
        (1, 2),
        (1, 3),
        (1, 4),
        (5, 3),
        (3, 1),
        (3, 5),
        (2, 5),
        (4, 1),
    ] {
        assert_eq!(at(&out, 7, x, y), 1.0, "edge ({x},{y})");
    }
    for (x, y) in [(1, 1), (5, 1), (1, 5), (5, 5)] {
        assert!(close(at(&out, 7, x, y), 0.586_343), "corner ({x},{y})");
    }
    for (x, y) in [(0, 3), (3, 0), (6, 3)] {
        assert!(close(at(&out, 7, x, y), 0.000_759), "fringe ({x},{y})");
    }
    for (x, y) in [(0, 0), (0, 1), (3, 3), (2, 2)] {
        assert_eq!(at(&out, 7, x, y), 0.0, "far/inside ({x},{y})");
    }
    let sum: f32 = out.iter().sum();
    assert!(close(sum, 12.0 + 4.0 * 0.586_343 + 12.0 * 0.000_759));

    let inn = reference_matte(&sq, &stroke(1.0, StrokePosition::Inside)).unwrap();
    for y in 0..7 {
        for x in 0..7 {
            let ring = (2..=4).contains(&x) && (2..=4).contains(&y) && (x, y) != (3, 3);
            let want = if ring {
                1.0
            } else if (x, y) == (3, 3) {
                0.000_759
            } else {
                0.0
            };
            assert!(close(at(&inn, 7, x, y), want), "inside ({x},{y})");
        }
    }

    // Centre, width 1: halves of 0.5 px -> 1.5 - w1 on the outer ring and on the inner ring.
    let mid = reference_matte(&sq, &stroke(1.0, StrokePosition::Centre)).unwrap();
    assert!(close(at(&mid, 7, 1, 3), 0.500_379_6));
    assert!(close(at(&mid, 7, 2, 3), 0.500_379_6));
    assert!(close(at(&mid, 7, 1, 1), 0.086_343));
    assert_eq!(at(&mid, 7, 3, 3), 0.0);
    assert_eq!(at(&mid, 7, 0, 3), 0.0);
    // Size 0 is an empty matte.
    let none = reference_matte(&sq, &stroke(0.0, StrokePosition::Outside)).unwrap();
    assert!(none.iter().all(|v| *v == 0.0));
}

// ---- envelope validation, limits and budgets -------------------------------------------------

const R: Rect = Rect {
    x: 0,
    y: 0,
    width: 2,
    height: 1,
};
fn layer(input: u32) -> Layer {
    Layer {
        input: PlanNodeId(input),
        mode: NORMAL,
        opacity: 1.0,
        fill_opacity: 1.0,
        mask: None,
    }
}
fn plan(nodes: Vec<PlanNode>, root: u32) -> RenderPlan {
    RenderPlan {
        revision: 3,
        target: OutputTarget {
            extent: R,
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: ColourTag {
                profile_sha256: [7; 32],
                blend_space: BlendSpace::Encoded,
            },
        },
        nodes,
        root: PlanNodeId(root),
    }
}
fn solid(a: f32) -> PlanNode {
    PlanNode::Solid {
        rect: R,
        rgba: [0.2, 0.4, 0.6, a],
    }
}
fn group(isolated: bool, layers: Vec<Layer>) -> PlanNode {
    PlanNode::Group(Group { isolated, layers })
}
fn one_shadow() -> Vec<EffectOp> {
    vec![EffectOp::DropShadow(shadow(0.5))]
}
fn err_of(plan: &RenderPlan, layers: Vec<LayerExtension>) -> PlanError {
    PlanExtensions::from_layers(layers)
        .validate(plan)
        .unwrap_err()
}

struct Legacy;
impl RenderBackend for Legacy {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            name: "legacy",
            version: "0",
            device: DeviceClass::Cpu,
            adapter: String::new(),
            generation: 0,
        }
    }
    fn exactness(&self, _: Operator) -> Exactness {
        Exactness::Exact
    }
    fn execute(
        &self,
        _: &RenderPlan,
        _: &ExecuteRequest,
        _: &dyn PlanSources,
        _: &CancellationToken,
        _: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        Err(BackendError::Unavailable("legacy_execute"))
    }
}
/// Supports every effect kind at class Approximate; refuses the Multiply blend.
struct WithEffects;
impl RenderBackend for WithEffects {
    fn identity(&self) -> BackendIdentity {
        Legacy.identity()
    }
    fn exactness(&self, op: Operator) -> Exactness {
        if op == Operator::Blend(StudioBlendMode::Multiply) {
            Exactness::Unsupported
        } else {
            Exactness::Exact
        }
    }
    fn extension_exactness(&self, op: ExtOperator) -> Exactness {
        match op {
            ExtOperator::Effect(_) => Exactness::Approximate,
            _ => Exactness::Unsupported,
        }
    }
    fn execute(
        &self,
        _: &RenderPlan,
        _: &ExecuteRequest,
        _: &dyn PlanSources,
        _: &CancellationToken,
        _: &mut dyn TileSink,
    ) -> Result<RenderReceipt, BackendError> {
        Err(BackendError::Unavailable("with_effects_execute"))
    }
}
struct NoSources;
impl PlanSources for NoSources {
    fn tile(&self, _: SourceKey) -> Option<TilePlanes<'_>> {
        None
    }
    fn mask(&self, _: SourceKey) -> Option<Mask<'_>> {
        None
    }
}
struct CountingSink(u32);
impl TileSink for CountingSink {
    fn accept(&mut self, _: Rect, _: &[u8], _: &[u8]) -> Result<(), SinkError> {
        self.0 += 1;
        Ok(())
    }
}
fn req(min: Exactness) -> ExecuteRequest {
    ExecuteRequest {
        expected_revision: 3,
        min_exactness: min,
        deadline: None,
        max_working_bytes: 1 << 20,
    }
}

/// Acceptance 5. A backend without effect support refuses `drop_shadow` before any sink call; a
/// backend with support admits at Approximate, refuses below `min_exactness`, and refuses an
/// effect whose blend mode it does not support.
#[test]
fn effect_without_backend_support_is_refused_before_the_sink() {
    let p = plan(vec![solid(1.0), group(true, vec![layer(0)])], 1);
    let ext = PlanExtensions::from_layers(vec![
        LayerExtension::new(PlanNodeId(1), 0).with_effects(one_shadow()),
    ]);
    assert_eq!(
        ext.validate(&p),
        Ok(vec![ExtOperator::Effect(EffectKind::DropShadow)])
    );
    let unsupported = Err(BackendError::Unsupported {
        operator: "drop_shadow",
        exactness: Exactness::Unsupported,
    });
    let cancel = CancellationToken::default();
    assert_eq!(
        admit_extended(&p, &ext, &Legacy, &req(Exactness::Approximate), &cancel).map(|_| ()),
        unsupported
    );
    let mut sink = CountingSink(0);
    let result = Legacy.execute_extended(
        &p,
        &ext,
        &req(Exactness::Approximate),
        &NoSources,
        &cancel,
        &mut sink,
    );
    assert_eq!(result.map(|_| ()), unsupported);
    assert_eq!(sink.0, 0);

    let ledger = admit_extended(
        &p,
        &ext,
        &WithEffects,
        &req(Exactness::Approximate),
        &cancel,
    )
    .unwrap();
    assert_eq!(ledger.lowest, Exactness::Approximate);
    assert!(
        ledger
            .operators
            .iter()
            .any(|o| o.key == "drop_shadow" && o.exactness == Exactness::Approximate)
    );
    assert!(ledger.operators.iter().any(|o| o.key == "normal"));
    assert_eq!(
        admit_extended(&p, &ext, &WithEffects, &req(Exactness::Exact), &cancel).map(|_| ()),
        Err(BackendError::Unsupported {
            operator: "drop_shadow",
            exactness: Exactness::Approximate
        })
    );
    // The effect's own blend mode goes through the backend's v1 class for that mode.
    let multiply =
        PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(1), 0).with_effects(
            vec![EffectOp::DropShadow(DropShadowOp::new(
                BLACK,
                StudioBlendMode::Multiply,
                0.75,
            ))],
        )]);
    assert_eq!(
        admit_extended(
            &p,
            &multiply,
            &WithEffects,
            &req(Exactness::Approximate),
            &cancel
        )
        .map(|_| ()),
        Err(BackendError::Unsupported {
            operator: StudioBlendMode::Multiply.key(),
            exactness: Exactness::Unsupported
        })
    );
    // The digest binds the effect list.
    let plain = PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(1), 0)]);
    assert_ne!(ext.digest_with(&p).unwrap(), plain.digest_with(&p).unwrap());
}

/// Placement rules, parameter and size limits, the per-layer and per-plan counts, the reference
/// work budget (`aggregate_over_budget`-style) and hostile plane dimensions.
#[test]
fn effect_limits_validation_and_budget() {
    // Where effects may not go.
    let adj = plan(
        vec![
            solid(1.0),
            PlanNode::Solid {
                rect: R,
                rgba: [0.0; 4],
            },
            group(true, vec![layer(0), layer(1)]),
        ],
        2,
    );
    assert_eq!(
        err_of(
            &adj,
            vec![
                LayerExtension::new(PlanNodeId(2), 1)
                    .with_adjustment(AdjustmentOp::Invert)
                    .with_effects(one_shadow())
            ]
        ),
        PlanError::EffectOnAdjustment { node: 2 }
    );
    let pass_layer = Layer {
        mode: StudioBlendMode::PassThrough,
        ..layer(1)
    };
    let pt = plan(
        vec![
            solid(1.0),
            group(false, vec![layer(0)]),
            group(true, vec![layer(0), pass_layer]),
        ],
        2,
    );
    assert_eq!(
        err_of(
            &pt,
            vec![LayerExtension::new(PlanNodeId(2), 1).with_effects(one_shadow())]
        ),
        PlanError::EffectPassThrough { node: 2 }
    );
    let clipped = Layer {
        mask: Some(MaskInput::clip_to_base()),
        ..layer(0)
    };
    let clip = plan(vec![solid(1.0), group(true, vec![layer(0), clipped])], 1);
    for index in [0, 1] {
        assert_eq!(
            err_of(
                &clip,
                vec![LayerExtension::new(PlanNodeId(1), index).with_effects(one_shadow())]
            ),
            PlanError::EffectClipRun { node: 1 }
        );
    }

    // Parameters and limits (typed, never clamped).
    let p = plan(vec![solid(1.0), group(true, vec![layer(0)])], 1);
    let with = |op: EffectOp| {
        err_of(
            &p,
            vec![LayerExtension::new(PlanNodeId(1), 0).with_effects(vec![op])],
        )
    };
    let sh = shadow(0.5);
    assert_eq!(
        with(EffectOp::DropShadow(sh.with_size(1000.5))),
        PlanError::EffectLimit { node: 1 }
    );
    assert_eq!(
        with(EffectOp::DropShadow(sh.with_offset(0.0, 30_001.0))),
        PlanError::EffectLimit { node: 1 }
    );
    assert_eq!(
        with(EffectOp::Stroke(StrokeOp::new(
            RED,
            NORMAL,
            1.0,
            f32::NAN,
            StrokePosition::Outside
        ))),
        PlanError::EffectParameter { node: 1 }
    );
    assert_eq!(
        with(EffectOp::DropShadow(shadow(1.5))),
        PlanError::EffectParameter { node: 1 }
    );
    assert_eq!(
        with(EffectOp::DropShadow(sh.with_spread(1.5))),
        PlanError::EffectParameter { node: 1 }
    );
    assert_eq!(
        with(EffectOp::DropShadow(sh.with_offset(181.0, 1.0))),
        PlanError::EffectParameter { node: 1 }
    );
    assert_eq!(
        with(EffectOp::ColourOverlay(ColourOverlayOp::new(
            [f32::NAN, 0.0, 0.0],
            NORMAL,
            0.5
        ))),
        PlanError::EffectParameter { node: 1 }
    );
    assert_eq!(
        with(EffectOp::ColourOverlay(ColourOverlayOp::new(
            RED,
            StudioBlendMode::PassThrough,
            0.5
        ))),
        PlanError::ModeNotApplicable {
            node: 1,
            mode: StudioBlendMode::PassThrough
        }
    );
    // 65 effects on one layer exceed the per-layer budget of 64; 65 layers of 64 exceed the
    // per-plan aggregate of 4096 although each layer is within its own budget.
    assert_eq!(
        err_of(
            &p,
            vec![
                LayerExtension::new(PlanNodeId(1), 0)
                    .with_effects(vec![EffectOp::DropShadow(sh); 65])
            ]
        ),
        PlanError::EffectLimit { node: 1 }
    );
    let wide = plan(vec![solid(1.0), group(true, vec![layer(0); 65])], 1);
    let layers = (0..65)
        .map(|i| {
            LayerExtension::new(PlanNodeId(1), i).with_effects(vec![EffectOp::DropShadow(sh); 64])
        })
        .collect();
    assert_eq!(err_of(&wide, layers), PlanError::EffectLimit { node: 1 });

    // Reference work budget: a 64 x 64 plane with a 1000 px window is refused before any loop.
    let big = plane(64, 64, |_, _| (RED, 1.0));
    let huge = EffectOp::DropShadow(shadow(1.0).with_size(1000.0).with_spread(1.0));
    assert_eq!(reference_matte(&big, &huge), Err(EffectError::OverBudget));
    let huge_stroke = EffectOp::Stroke(StrokeOp::new(
        RED,
        NORMAL,
        1.0,
        1000.0,
        StrokePosition::Outside,
    ));
    assert_eq!(
        reference_matte(&big, &huge_stroke),
        Err(EffectError::OverBudget)
    );
    // Hostile plane dimensions never reach an allocation.
    assert_eq!(
        EffectPlane::new(u32::MAX, u32::MAX, vec![], vec![]),
        Err(EffectError::PlaneShape)
    );
    assert_eq!(
        EffectPlane::new(65, 1, vec![[0.0; 3]; 65], vec![0.0; 65]),
        Err(EffectError::PlaneShape)
    );
    assert_eq!(
        EffectPlane::new(2, 1, vec![[0.0; 3]; 2], vec![0.0; 3]),
        Err(EffectError::PlaneShape)
    );
    assert_eq!(
        EffectPlane::new(1, 1, vec![[2.0, 0.0, 0.0]], vec![0.0]),
        Err(EffectError::OutOfDomain)
    );

    // Margin a backend must read: shadow 3 (offset) + 3 (grow reach of spread_px = .5 * 4 = 2:
    // floor(3 / w1), lane SMB-PS-ORACLE-PARITY-2) + 1 (blur 2 -> tent half-width 2, nonzero taps
    // within 1) = 7; stroke outside 5 -> reach 6; centre 5 -> reach of 2.5 = 3; overlay 0.
    let margin = EffectOp::DropShadow(
        shadow(1.0)
            .with_offset(10.0, 3.0)
            .with_size(4.0)
            .with_spread(0.5),
    );
    assert_eq!(effect_support_px(&margin), Ok(7));
    let stroke = |position| EffectOp::Stroke(StrokeOp::new(RED, NORMAL, 1.0, 5.0, position));
    assert_eq!(effect_support_px(&stroke(StrokePosition::Outside)), Ok(6));
    assert_eq!(effect_support_px(&stroke(StrokePosition::Centre)), Ok(3));
    assert_eq!(
        effect_support_px(&EffectOp::ColourOverlay(ColourOverlayOp::new(
            RED, NORMAL, 1.0
        ))),
        Ok(0)
    );
}

/// Lane SMB-PS-ORACLE-PARITY-1, STUDIO-R36 remediation (`figma_parity_bound_regressed_by_effect_blur_change`):
/// the blur kernel is chosen per shadow. The default stays Photoshop's tent (the D5 oracle suite
/// depends on it); `Gaussian` (Figma-sourced shadows) is the Gaussian of sigma size / 2. Every
/// expected value is derived here by hand.
#[test]
fn gaussian_kernel_is_selected_per_shadow() {
    // Defaults: both shadow ops and every other kind use the tent.
    assert_eq!(EffectBlurKernel::default(), EffectBlurKernel::Tent);
    assert_eq!(shadow(1.0).blur_kernel, EffectBlurKernel::Tent);
    assert_eq!(InnerShadowOp::new(BLACK, NORMAL, 1.0).blur_kernel, EffectBlurKernel::Tent);
    let overlay = EffectOp::ColourOverlay(ColourOverlayOp::new(RED, NORMAL, 1.0));
    assert_eq!(overlay.blur_kernel(), EffectBlurKernel::Tent);
    assert_eq!(EffectBlurKernel::Tent.taps(10.0).unwrap(), effect_blur_kernel(10.0).unwrap());
    assert_eq!(EffectBlurKernel::Tent.support(10.0).unwrap(), effect_blur_support(10.0).unwrap());

    // Gaussian of size 2: sigma 1, support ceil(3 sigma) = 3, taps exp(-i^2 / 2) normalised.
    let g = EffectBlurKernel::Gaussian.taps(2.0).unwrap();
    assert_eq!(g.len(), 7);
    assert_eq!(EffectBlurKernel::Gaussian.support(2.0).unwrap(), 3);
    let raw: Vec<f64> = (-3i32..=3).map(|i| (-f64::from(i * i) / 2.0).exp()).collect();
    let sum: f64 = raw.iter().sum();
    for (t, r) in g.iter().zip(&raw) {
        assert!(close(*t, (r / sum) as f32), "{t} vs {}", r / sum);
    }
    // Size 0 is the identity for both kernels; size 10: sigma 5, support 15 (tent: 9).
    assert_eq!(EffectBlurKernel::Gaussian.taps(0.0).unwrap(), vec![1.0]);
    assert_eq!(EffectBlurKernel::Gaussian.support(10.0).unwrap(), 15);
    // Hostile sizes are the tent's typed refusals.
    for k in [EffectBlurKernel::Tent, EffectBlurKernel::Gaussian] {
        assert_eq!(k.taps(f32::NAN).unwrap_err(), EffectError::OutOfDomain);
        assert_eq!(k.taps(-1.0).unwrap_err(), EffectError::OutOfDomain);
        assert_eq!(k.taps(1_000.5).unwrap_err(), EffectError::SizeOutOfRange);
        assert_eq!(k.support(f32::INFINITY).unwrap_err(), EffectError::OutOfDomain);
    }

    // The support of a Gaussian drop shadow: distance 3 + blur half-width 15.
    let gauss = shadow(1.0)
        .with_offset(180.0, 3.0)
        .with_size(10.0)
        .with_blur_kernel(EffectBlurKernel::Gaussian);
    assert_eq!(effect_support_px(&EffectOp::DropShadow(gauss)).unwrap(), 18);
    let tent = shadow(1.0).with_offset(180.0, 3.0).with_size(10.0);
    assert_eq!(effect_support_px(&EffectOp::DropShadow(tent)).unwrap(), 12);
    let inner = InnerShadowOp::new(BLACK, NORMAL, 1.0)
        .with_size(4.0)
        .with_blur_kernel(EffectBlurKernel::Gaussian);
    assert_eq!(effect_support_px(&EffectOp::InnerShadow(inner)).unwrap(), 6);

    // Matte of one opaque pixel in a 9 x 1 plane, Gaussian size 2, no offset: the separable blur
    // of a delta is the 2-D kernel, so along the row the matte is tap(i) * tap(0).
    let dot = plane(9, 1, |x, _| (RED, if x == 4 { 1.0 } else { 0.0 }));
    let op = EffectOp::DropShadow(shadow(1.0).with_size(2.0).with_blur_kernel(EffectBlurKernel::Gaussian));
    let m = reference_matte(&dot, &op).unwrap();
    for x in 0..9u32 {
        let i = x as i32 - 4;
        let want = if i.abs() <= 3 { g[(i + 3) as usize] * g[3] } else { 0.0 };
        assert!(close(at(&m, 9, x, 0), want), "x {x}: {} vs {want}", at(&m, 9, x, 0));
    }
    // The tent of the same size gives a different matte ([1, 2, 1] / 4 at size 2): centre 1/4.
    let t = reference_matte(&dot, &EffectOp::DropShadow(shadow(1.0).with_size(2.0))).unwrap();
    assert!(close(at(&t, 9, 4, 0), 0.25));
    assert!(!close(at(&m, 9, 4, 0), at(&t, 9, 4, 0)));

    // Serialisation: the tent is not encoded (existing plans keep their bytes), the Gaussian is.
    let tent_json = serde_json_like(&EffectOp::DropShadow(shadow(1.0)));
    assert!(!tent_json.contains("blur_kernel"), "{tent_json}");
    let gauss_json = serde_json_like(&EffectOp::DropShadow(gauss));
    assert!(gauss_json.contains("blur_kernel") && gauss_json.contains("Gaussian"), "{gauss_json}");
}

/// The op's serde encoding as text (pigment serialises plans; no JSON crate needed here).
fn serde_json_like(op: &EffectOp) -> String {
    serde_json::to_string(op).unwrap()
}
