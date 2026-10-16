//! Effects wave 2 (inner shadow, outer glow, inner glow; colour source only): reference maths,
//! typed refusals and admission. Every expected value is derived by hand in the test body.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::effect_math::{
    DropShadowOp, EffectError, EffectFeature, EffectKind, EffectOp, EffectOrder, EffectPlane,
    GlowFill, GlowSource, GlowTechnique, InnerGlowOp, InnerShadowOp, LayerParams, OuterGlowOp,
    effect_support_px, reference_matte, render_layer_reference, render_reference,
};
use hsk_studio_pigment::render_plan::*;

const TOL: f32 = 5e-6;
const RED: [f32; 3] = [1.0, 0.0, 0.0];
const BLACK: [f32; 3] = [0.0; 3];
const WHITE: [f32; 3] = [1.0; 3];
const NORMAL: StudioBlendMode = StudioBlendMode::Normal;

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() <= TOL
}
fn plane(w: u32, h: u32, f: impl Fn(u32, u32) -> f32) -> EffectPlane {
    let (mut rgb, mut alpha) = (Vec::new(), Vec::new());
    for y in 0..h {
        for x in 0..w {
            rgb.push(RED);
            alpha.push(f(x, y));
        }
    }
    EffectPlane::new(w, h, rgb, alpha).unwrap()
}
fn refused<T>(feature: EffectFeature) -> Result<T, EffectError> {
    Err(EffectError::Unsupported(feature))
}
fn at(m: &[f32], w: u32, x: u32, y: u32) -> f32 {
    m[(y * w + x) as usize]
}

/// Inner shadow = the shadow of the inverted layer alpha (beyond the plane counts as transparent,
/// i.e. 1), shifted by the drop-shadow offset, clipped to the layer alpha; choke is the hard share
/// of the size (choke 1 = a hard inner stroke of width `size`).
#[test]
fn inner_shadow_is_the_inverted_matte_clipped_to_the_alpha() {
    // 5 x 5 opaque, angle 180 distance 1 -> (dx, dy) = (1, 0): out(x, y) = inv(x - 1, y); only x = 0
    // reads from beyond the plane (value 1), everything else reads inv = 0.
    let full = plane(5, 5, |_, _| 1.0);
    let op = EffectOp::InnerShadow(InnerShadowOp::new(BLACK, NORMAL, 0.5).with_offset(180.0, 1.0));
    let m = reference_matte(&full, &op).unwrap();
    for y in 0..5 {
        for x in 0..5 {
            assert_eq!(at(&m, 5, x, y), if x == 0 { 1.0 } else { 0.0 }, "({x},{y})");
        }
    }
    // Over the content: black 50 % over opaque red at (0, 2) -> (0.5, 0, 0), alpha 1.
    let p = render_reference(&full, &op).unwrap().pixel(0, 2).unwrap();
    assert_eq!((p.rgb, p.a), ([0.5, 0.0, 0.0], 1.0));
    assert_eq!(
        render_reference(&full, &op)
            .unwrap()
            .pixel(1, 2)
            .unwrap()
            .rgb,
        RED
    );

    // Clipped to the alpha: alpha [0, 1, 1, 1, 0] -> inv [1, 0, 0, 0, 1] -> shifted [1, 1, 0, 0, 0]
    // -> times alpha [0, 1, 0, 0, 0]: nothing is drawn outside the layer shape.
    let strip = plane(5, 1, |x, _| if (1..=3).contains(&x) { 1.0 } else { 0.0 });
    assert_eq!(
        reference_matte(&strip, &op).unwrap(),
        vec![0.0, 1.0, 0.0, 0.0, 0.0]
    );

    // Choke 1, size 2, distance 0: grown by 2 hard, blur 0 -> the 2-pixel ring of the plane is 1
    // (ramp(2, d) = 1 for d <= 1.5), the centre pixel (distance 2.5 to the outside) is 0.
    let ring = EffectOp::InnerShadow(
        InnerShadowOp::new(BLACK, NORMAL, 1.0)
            .with_choke(1.0)
            .with_size(2.0),
    );
    let m = reference_matte(&full, &ring).unwrap();
    assert_eq!(m.iter().sum::<f32>(), 24.0);
    assert_eq!(at(&m, 5, 1, 2), 1.0);
    assert_eq!(at(&m, 5, 2, 1), 1.0);
    assert_eq!(at(&m, 5, 2, 2), 0.0);
    // Choke 0 with the same size is a pure blur: sigma 1, row 2 of a 5 x 5 plane is too close to
    // the border for a closed form, so only the monotone fall-off from the edge is asserted.
    let soft = EffectOp::InnerShadow(InnerShadowOp::new(BLACK, NORMAL, 1.0).with_size(2.0));
    let m = reference_matte(&full, &soft).unwrap();
    assert!(at(&m, 5, 0, 2) > at(&m, 5, 1, 2) && at(&m, 5, 1, 2) > at(&m, 5, 2, 2));
}

/// Inner glow from the edge on a 7 x 7 opaque plane, size 2 (sigma 1, taps reach 3): at row 3 the
/// inverted matte blurred horizontally gives, with taps t = exp(-i^2/2) / 2.505950 for i = 0..3
/// (0.399050, 0.242036, 0.054006, 0.004433): x = 0 sums the three taps beyond the plane, 0.300475;
/// x = 1 two taps, 0.058439; x = 2 one tap, 0.004433; x = 3 none, 0. The vertical pass keeps them
/// (rows 0..6 are all inside). Centre source is 1 minus that. Outer glow equals the drop-shadow
/// chain without offset.
#[test]
fn glow_edge_centre_and_outer_equal_the_shadow_chain() {
    let full = plane(7, 7, |_, _| 1.0);
    let edge = EffectOp::InnerGlow(InnerGlowOp::new(WHITE, NORMAL, 1.0).with_size(2.0));
    let centre = EffectOp::InnerGlow(
        InnerGlowOp::new(WHITE, NORMAL, 1.0)
            .with_size(2.0)
            .with_source(GlowSource::Centre),
    );
    let e = reference_matte(&full, &edge).unwrap();
    let c = reference_matte(&full, &centre).unwrap();
    for (x, want) in [
        (0, 0.300_474_86),
        (1, 0.058_438_63),
        (2, 0.004_433_05),
        (3, 0.0),
    ] {
        assert!(close(at(&e, 7, x, 3), want), "edge x={x}");
        assert!(close(at(&e, 7, 6 - x, 3), want), "edge mirrored x={x}");
        assert!(close(at(&c, 7, x, 3), 1.0 - want), "centre x={x}");
    }

    // Outer glow: column x = 4 of a 9 x 9 plane, size 2: centre tap 0.399050, neighbours 0.242036.
    let column = plane(9, 9, |x, _| if x == 4 { 1.0 } else { 0.0 });
    let outer = EffectOp::OuterGlow(OuterGlowOp::new(WHITE, NORMAL, 1.0).with_size(2.0));
    let shadow = EffectOp::DropShadow(DropShadowOp::new(WHITE, NORMAL, 1.0).with_size(2.0));
    let g = reference_matte(&column, &outer).unwrap();
    assert_eq!(g, reference_matte(&column, &shadow).unwrap());
    assert!(close(at(&g, 9, 4, 4), 0.399_050_28));
    assert!(close(at(&g, 9, 3, 4), 0.242_036_23));
    // Spread 1, size 1 on a 3 x 1 strip with only the middle pixel: hard growth by 1 -> all three.
    let dot = plane(3, 1, |x, _| if x == 1 { 1.0 } else { 0.0 });
    let hard = EffectOp::OuterGlow(
        OuterGlowOp::new(WHITE, NORMAL, 0.5)
            .with_size(1.0)
            .with_spread(1.0),
    );
    assert_eq!(reference_matte(&dot, &hard).unwrap(), vec![1.0, 1.0, 1.0]);

    // Placement and composition: the outer glow sits BEHIND the content (visible through fill 0 and
    // outside the shape), the inner glow OVER it but clipped to the alpha (nothing outside).
    assert_eq!(hard.kind(), EffectKind::OuterGlow);
    assert_eq!(hard.placement(), EffectOrder::BehindContent);
    assert_eq!(edge.placement(), EffectOrder::OverContent);
    assert_eq!(
        EffectOp::InnerShadow(InnerShadowOp::new(BLACK, NORMAL, 1.0)).placement(),
        EffectOrder::OverContent
    );
    let fill0 = LayerParams {
        mode: NORMAL,
        opacity: 1.0,
        fill_opacity: 0.0,
    };
    let backdrop = EffectPlane::transparent(3, 1).unwrap();
    let out = render_layer_reference(&backdrop, &dot, fill0, &[hard]).unwrap();
    for x in 0..3 {
        let p = out.pixel(x, 0).unwrap();
        assert_eq!((p.rgb, p.a), (WHITE, 0.5), "outer glow x={x}");
    }
    // Inner glow, choke 1 size 1: inverted [1, 0, 1] grown by 1 -> [1, 1, 1], times alpha [0, 1, 0].
    let inner = EffectOp::InnerGlow(
        InnerGlowOp::new(WHITE, NORMAL, 0.5)
            .with_size(1.0)
            .with_choke(1.0),
    );
    assert_eq!(reference_matte(&dot, &inner).unwrap(), vec![0.0, 1.0, 0.0]);
    let out = render_layer_reference(&backdrop, &dot, fill0, &[inner]).unwrap();
    assert_eq!(out.pixel(0, 0).unwrap().a, 0.0);
    assert_eq!(out.pixel(1, 0).unwrap().a, 0.5);
    assert_eq!(out.pixel(2, 0).unwrap().a, 0.0);
}

// ---- refusals, limits, margins and admission --------------------------------------------------

const R: Rect = Rect {
    x: 0,
    y: 0,
    width: 2,
    height: 1,
};
fn plan() -> RenderPlan {
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
        nodes: vec![
            PlanNode::Solid {
                rect: R,
                rgba: [0.2, 0.4, 0.6, 1.0],
            },
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![Layer {
                    input: PlanNodeId(0),
                    mode: NORMAL,
                    opacity: 1.0,
                    fill_opacity: 1.0,
                    mask: None,
                }],
            }),
        ],
        root: PlanNodeId(1),
    }
}
fn envelope(op: EffectOp) -> PlanExtensions {
    PlanExtensions::from_layers(vec![
        LayerExtension::new(PlanNodeId(1), 0).with_effects(vec![op]),
    ])
}
struct Backend {
    effects: bool,
}
impl RenderBackend for Backend {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            name: "wave2",
            version: "0",
            device: DeviceClass::Cpu,
            adapter: String::new(),
            generation: 0,
        }
    }
    fn exactness(&self, _: Operator) -> Exactness {
        Exactness::Exact
    }
    fn extension_exactness(&self, op: ExtOperator) -> Exactness {
        match op {
            ExtOperator::Effect(_) if self.effects => Exactness::Approximate,
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
        Err(BackendError::Unavailable("wave2_execute"))
    }
}
fn req() -> ExecuteRequest {
    ExecuteRequest {
        expected_revision: 3,
        min_exactness: Exactness::Approximate,
        deadline: None,
        max_working_bytes: 1 << 20,
    }
}

/// Gradient fill, Precise technique, range below 100 % and jitter above 0 are typed refusals (never
/// a flat colour); domain errors win over refusals; the envelope reports them as the feature key at
/// admission; the new kinds are refused by a backend without effect support; budgets and margins.
#[test]
fn glow_refusals_envelope_admission_and_budget() {
    let outer = OuterGlowOp::new(WHITE, NORMAL, 0.5).with_size(2.0);
    let inner = InnerGlowOp::new(WHITE, NORMAL, 0.5).with_size(2.0);
    assert_eq!(EffectOp::OuterGlow(outer).validate(), Ok(()));
    assert_eq!(EffectOp::InnerGlow(inner).validate(), Ok(()));
    assert_eq!(
        EffectOp::OuterGlow(outer.with_fill(GlowFill::Gradient)).validate(),
        refused(EffectFeature::GlowGradient)
    );
    assert_eq!(
        EffectOp::InnerGlow(inner.with_technique(GlowTechnique::Precise)).validate(),
        refused(EffectFeature::GlowTechniquePrecise)
    );
    assert_eq!(
        EffectOp::OuterGlow(outer.with_range(0.5)).validate(),
        refused(EffectFeature::GlowRange)
    );
    assert_eq!(
        EffectOp::InnerGlow(inner.with_jitter(0.2)).validate(),
        refused(EffectFeature::GlowJitter)
    );
    // Domain first: range 0 or above 1, jitter above 1 are out of domain, not "unsupported".
    for bad in [
        EffectOp::OuterGlow(outer.with_range(0.0)),
        EffectOp::OuterGlow(outer.with_range(1.5).with_fill(GlowFill::Gradient)),
        EffectOp::InnerGlow(inner.with_jitter(2.0)),
        EffectOp::InnerGlow(inner.with_choke(1.5)),
        EffectOp::OuterGlow(outer.with_spread(-0.1)),
    ] {
        assert_eq!(bad.validate(), Err(EffectError::OutOfDomain), "{bad:?}");
    }
    // The reference refuses the same way (before any work).
    let full = plane(3, 3, |_, _| 1.0);
    assert_eq!(
        reference_matte(
            &full,
            &EffectOp::OuterGlow(outer.with_fill(GlowFill::Gradient))
        ),
        refused(EffectFeature::GlowGradient)
    );

    // Envelope: typed plan error, then the feature key at admission; the new kinds are refused by
    // key on a backend without effect support and admitted at Approximate with it.
    let p = plan();
    let gradient = envelope(EffectOp::OuterGlow(outer.with_fill(GlowFill::Gradient)));
    assert_eq!(
        gradient.validate(&p),
        Err(PlanError::EffectUnsupported {
            node: 1,
            feature: EffectFeature::GlowGradient
        })
    );
    let cancel = CancellationToken::default();
    let with = Backend { effects: true };
    assert_eq!(
        admit_extended(&p, &gradient, &with, &req(), &cancel).map(|_| ()),
        Err(BackendError::Unsupported {
            operator: "glow_gradient",
            exactness: Exactness::Unsupported
        })
    );
    let without = Backend { effects: false };
    for (op, key) in [
        (
            EffectOp::InnerShadow(InnerShadowOp::new(BLACK, NORMAL, 0.5)),
            "inner_shadow",
        ),
        (EffectOp::OuterGlow(outer), "outer_glow"),
        (EffectOp::InnerGlow(inner), "inner_glow"),
    ] {
        let ext = envelope(op);
        assert_eq!(ext.validate(&p), Ok(vec![ExtOperator::Effect(op.kind())]));
        assert_eq!(
            admit_extended(&p, &ext, &without, &req(), &cancel).map(|_| ()),
            Err(BackendError::Unsupported {
                operator: key,
                exactness: Exactness::Unsupported
            })
        );
        let ledger = admit_extended(&p, &ext, &with, &req(), &cancel).unwrap();
        assert!(
            ledger
                .operators
                .iter()
                .any(|o| o.key == key && o.exactness == Exactness::Approximate)
        );
    }
    // Limits are typed plan errors; the shadow distance limit applies to the inner shadow too.
    let limit = envelope(EffectOp::InnerShadow(
        InnerShadowOp::new(BLACK, NORMAL, 1.0).with_offset(0.0, 30_001.0),
    ));
    assert_eq!(limit.validate(&p), Err(PlanError::EffectLimit { node: 1 }));
    let angle = envelope(EffectOp::InnerShadow(
        InnerShadowOp::new(BLACK, NORMAL, 1.0).with_offset(-181.0, 1.0),
    ));
    assert_eq!(
        angle.validate(&p),
        Err(PlanError::EffectParameter { node: 1 })
    );

    // Work budget: a 64 x 64 plane with a 1000 px choke window is refused before any loop.
    let big = plane(64, 64, |_, _| 1.0);
    let huge = EffectOp::InnerShadow(
        InnerShadowOp::new(BLACK, NORMAL, 1.0)
            .with_size(1000.0)
            .with_choke(1.0),
    );
    assert_eq!(reference_matte(&big, &huge), Err(EffectError::OverBudget));
    let huge_glow = EffectOp::OuterGlow(
        OuterGlowOp::new(WHITE, NORMAL, 1.0)
            .with_size(1000.0)
            .with_spread(1.0),
    );
    assert_eq!(
        reference_matte(&big, &huge_glow),
        Err(EffectError::OverBudget)
    );

    // Margins: inner shadow 3 (offset) + 2 (choke .5 * 4) + 3 (blur 2 -> sigma 1 -> ceil 3) = 8;
    // outer glow size 4, spread 0: blur 4 -> sigma 2 -> ceil 6 = 6; inner glow size 4, choke .5:
    // 2 + 3 = 5.
    let shadow = EffectOp::InnerShadow(
        InnerShadowOp::new(BLACK, NORMAL, 1.0)
            .with_offset(10.0, 3.0)
            .with_size(4.0)
            .with_choke(0.5),
    );
    assert_eq!(effect_support_px(&shadow), Ok(8));
    assert_eq!(
        effect_support_px(&EffectOp::OuterGlow(
            OuterGlowOp::new(WHITE, NORMAL, 1.0).with_size(4.0)
        )),
        Ok(6)
    );
    assert_eq!(
        effect_support_px(&EffectOp::InnerGlow(
            InnerGlowOp::new(WHITE, NORMAL, 1.0)
                .with_size(4.0)
                .with_choke(0.5)
        )),
        Ok(5)
    );
    assert_eq!(
        EffectKind::ALL.map(EffectKind::key),
        [
            "drop_shadow",
            "colour_overlay",
            "stroke",
            "inner_shadow",
            "outer_glow",
            "inner_glow",
            // lane SMB-PS-ADJUST-STYLES (effect_math::style)
            "satin",
            "bevel_emboss",
            "gradient_overlay",
            // lane SMB-PS-ADJUST-STYLES-2
            "pattern_overlay"
        ]
    );
}
