//! Lane SMB-PS-ORACLE-PARITY-2: the 8-bit working depth (pigment `WorkingDepth::Eight`: the running
//! result a later layer reads is stored in 8-bit levels, Levels runs on whole levels; D5 PS-C28
//! `nomask`, PS-C29 `fill100`, PS-C02 8-bit gamma 1.5) and the pass-through group opacity (pigment
//! `blend_math::pass_through_mix`, D5 PS-C28 `opacity50_nomask`) render bit for bit like pigment's
//! composition of its single owners, for every chunking.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::adjust_math::{
    AdjustmentOp, Levels, LevelsRecord, composite_adjustment,
};
use hsk_studio_pigment::blend_math::{
    PixelSite, Rgba, composite_over, composite_over_fill_at, pass_through_mix,
};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BlendSpace, ColourTag, Exactness, ExecuteRequest, ExtOperator, Group, Layer,
    LayerExtension, NoVector, OutputStage, OutputTarget, PlanExtensions, PlanNode, PlanNodeId,
    PlanSources, RenderBackend, RenderPlan, SampleFormat, SinkError, SourceKey, StudioBlendMode,
    TilePlanes, TileSink, WorkingDepth, quantise_8bit,
};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_render_cpu::Renderer;

const W: u32 = 9;
const H: u32 = 5;
const NORMAL: StudioBlendMode = StudioBlendMode::Normal;
const A: [f32; 4] = [0.3, 0.6, 0.9, 1.0];
const B: [f32; 4] = [0.2, 0.45, 0.75, 1.0];

fn extent() -> Rect {
    Rect { x: 0, y: 0, width: W, height: H }
}
fn tag() -> ColourTag {
    ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded }
}
fn layer(input: u32, mode: StudioBlendMode, opacity: f32) -> Layer {
    Layer { input: PlanNodeId(input), mode, opacity, fill_opacity: 1.0, mask: None }
}
fn plan(nodes: Vec<PlanNode>, root: u32) -> RenderPlan<NoVector> {
    RenderPlan {
        revision: 3,
        target: OutputTarget {
            extent: extent(),
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: tag(),
        },
        nodes,
        root: PlanNodeId(root),
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
struct Image(Vec<[f32; 4]>);
impl TileSink for Image {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        for y in 0..r.height as usize {
            for x in 0..r.width as usize {
                let i = y * r.width as usize + x;
                let at = (r.y as usize + y) * W as usize + r.x as usize + x;
                self.0[at] = [f(colour, i * 12), f(colour, i * 12 + 4), f(colour, i * 12 + 8), f(alpha, i * 4)];
            }
        }
        Ok(())
    }
}
fn render(chunk: u32, plan: &RenderPlan<NoVector>, ext: &PlanExtensions) -> Vec<[f32; 4]> {
    let mut image = Image(vec![[f32::NAN; 4]; (W * H) as usize]);
    let request = ExecuteRequest {
        expected_revision: 3,
        min_exactness: Exactness::Approximate,
        deadline: None,
        max_working_bytes: 1 << 26,
    };
    let renderer = Renderer::new(chunk).unwrap();
    let receipt = renderer
        .execute_extended(plan, ext, &request, &NoSources, &CancellationToken::default(), &mut image)
        .unwrap();
    assert_eq!(receipt.exactness.lowest, Exactness::Approximate);
    image.0
}
fn over(mode: StudioBlendMode, b: [f32; 4], s: [f32; 4], opacity: f32) -> [f32; 4] {
    let out = composite_over(
        mode,
        Rgba::new([b[0], b[1], b[2]], b[3]),
        Rgba::new([s[0], s[1], s[2]], s[3]),
        opacity,
        PixelSite { x: 0, y: 0, seed: 3 },
    )
    .unwrap();
    [out.rgb[0], out.rgb[1], out.rgb[2], out.a.min(1.0)]
}
fn levels() -> AdjustmentOp {
    let r = LevelsRecord {
        input_black: 20.0 / 255.0,
        input_white: 230.0 / 255.0,
        gamma: 1.5,
        output_black: 10.0 / 255.0,
        output_white: 240.0 / 255.0,
    };
    AdjustmentOp::Levels(Levels {
        composite: r,
        red: LevelsRecord::IDENTITY,
        green: LevelsRecord::IDENTITY,
        blue: LevelsRecord::IDENTITY,
    })
}
fn bits(px: &[[f32; 4]]) -> Vec<[u32; 4]> {
    px.iter().map(|p| p.map(f32::to_bits)).collect()
}

/// Root [A Normal, B Multiply 0.9, Levels (in 20/230, gamma 1.5, out 10/240)] at the 8-bit working
/// depth: A and the Multiply result are stored in 8-bit levels (22, 77, 178 for the Multiply), the
/// Levels runs on whole levels and its result (the root's last layer) is delivered unquantised.
#[test]
fn eight_bit_working_depth_stores_layers_and_runs_levels_on_levels() {
    let nodes = vec![
        PlanNode::Solid { rect: extent(), rgba: A },
        PlanNode::Solid { rect: extent(), rgba: B },
        PlanNode::Solid { rect: extent(), rgba: [0.0; 4] },
        PlanNode::Group(Group {
            isolated: true,
            layers: vec![
                layer(0, NORMAL, 1.0),
                layer(1, StudioBlendMode::Multiply, 0.9),
                layer(2, NORMAL, 1.0),
            ],
        }),
    ];
    let p = plan(nodes, 3);
    let entry = LayerExtension::new(PlanNodeId(3), 2).with_adjustment(levels());
    let eight = PlanExtensions::from_layers(vec![entry.clone()])
        .with_output(OutputStage::default().with_working_depth(WorkingDepth::Eight));
    let float = PlanExtensions::from_layers(vec![entry]);
    assert!(eight.validate(&p).unwrap().contains(&ExtOperator::WorkingDepthEight));
    assert!(!float.validate(&p).unwrap().contains(&ExtOperator::WorkingDepthEight));

    let q = |p: [f32; 4]| p.map(quantise_8bit);
    let b1 = q(over(NORMAL, [0.0; 4], A, 1.0));
    let b2 = q(over(StudioBlendMode::Multiply, b1, B, 0.9));
    assert_eq!(b2.map(|v| (v * 255.0).round()), [22.0, 77.0, 178.0, 255.0]);
    let prepared = levels().prepare().unwrap().with_eight_bit(true);
    let out = composite_adjustment(NORMAL, Rgba::new([b2[0], b2[1], b2[2]], b2[3]), &prepared, 1.0)
        .unwrap();
    let want = [out.rgb[0], out.rgb[1], out.rgb[2], out.a];
    // hand values of LevelsRecord::map_value_8bit: 22 -> v8 round(2 * 255 / 210) = 2 -> toe
    // round(255 levels_gamma(2 / 255, 1.5)) = 6 -> round(10 + 6 * 230 / 255) = 15
    assert_eq!((want[0] * 255.0).round(), 15.0);
    let mut first = None;
    for chunk in [16, 4, 3] {
        let got = render(chunk, &p, &eight);
        assert!(got.iter().all(|px| px.map(f32::to_bits) == want.map(f32::to_bits)), "chunk {chunk}");
        assert_eq!(first.get_or_insert_with(|| bits(&got)), &bits(&got));
    }
    // the float depth differs (16.07 levels where the 8-bit document has 15)
    let got = render(16, &p, &float);
    assert!((got[0][0] - want[0]).abs() > 0.5 / 255.0, "{:?} vs {:?}", got[0], want);
}

/// Root [A Normal, pass-through group [B Multiply] at opacity 0.5]: pigment `pass_through_mix` of
/// the result before and after the group (D5 PS-C28 opacity50_nomask).
#[test]
fn pass_through_opacity_is_the_premultiplied_lerp() {
    let nodes = vec![
        PlanNode::Solid { rect: extent(), rgba: A },
        PlanNode::Solid { rect: Rect { x: 2, y: 1, width: 5, height: 3 }, rgba: [0.2, 0.45, 0.75, 0.6] },
        PlanNode::Group(Group { isolated: false, layers: vec![layer(1, StudioBlendMode::Multiply, 1.0)] }),
        PlanNode::Group(Group {
            isolated: true,
            layers: vec![layer(0, NORMAL, 1.0), layer(2, StudioBlendMode::PassThrough, 0.5)],
        }),
    ];
    let p = plan(nodes, 3);
    let before = over(NORMAL, [0.0; 4], A, 1.0);
    let after = over(StudioBlendMode::Multiply, before, [0.2, 0.45, 0.75, 0.6], 1.0);
    let mixed = pass_through_mix(
        Rgba::new([before[0], before[1], before[2]], before[3]),
        Rgba::new([after[0], after[1], after[2]], after[3]),
        0.5,
    );
    let inside = [mixed.rgb[0], mixed.rgb[1], mixed.rgb[2], mixed.a.min(1.0)];
    for chunk in [16, 4, 3] {
        let got = render(chunk, &p, &PlanExtensions::default());
        for y in 0..H {
            for x in 0..W {
                let want = if (2..7).contains(&x) && (1..4).contains(&y) { inside } else { before };
                assert_eq!(
                    got[(y * W + x) as usize].map(f32::to_bits),
                    want.map(f32::to_bits),
                    "chunk {chunk} ({x}, {y})"
                );
            }
        }
    }
}

/// Linear light at fill 0.5 in an 8-bit document: the scaled source 0.5 + (s - 0.5) f is floored to
/// a whole level (pigment `composite_over_fill_at`, D5 PS-B01 d8_o100_f50_LINEARLIGHT); the float
/// document keeps the float source.
#[test]
fn eight_bit_linear_light_fill_floors_the_scaled_source() {
    let backdrop = [0.4, 0.4, 0.4, 1.0];
    let src = [41.0 / 255.0, 0.31, 200.0 / 255.0, 1.0];
    let nodes = vec![
        PlanNode::Solid { rect: extent(), rgba: backdrop },
        PlanNode::Solid { rect: extent(), rgba: src },
        PlanNode::Group(Group {
            isolated: true,
            layers: vec![
                layer(0, NORMAL, 1.0),
                Layer { fill_opacity: 0.5, ..layer(1, StudioBlendMode::LinearLight, 1.0) },
            ],
        }),
    ];
    let p = plan(nodes, 2);
    let site = PixelSite { x: 0, y: 0, seed: 3 };
    let b1 = over(NORMAL, [0.0; 4], backdrop, 1.0).map(quantise_8bit);
    let want = |eight: bool| {
        let out = composite_over_fill_at(
            StudioBlendMode::LinearLight,
            Rgba::new([b1[0], b1[1], b1[2]], b1[3]),
            Rgba::new([src[0], src[1], src[2]], src[3]),
            1.0,
            0.5,
            site,
            eight,
        )
        .unwrap();
        [out.rgb[0], out.rgb[1], out.rgb[2], out.a.min(1.0)]
    };
    let eight = PlanExtensions::default()
        .with_output(OutputStage::default().with_working_depth(WorkingDepth::Eight));
    let got = render(4, &p, &eight);
    assert!(got.iter().all(|px| px.map(f32::to_bits) == want(true).map(f32::to_bits)), "{:?}", got[0]);
    // red: s 41 -> s' = 0.5 + (41 / 255 - 0.5) / 2 = 84.25 levels -> floor 84; the float source
    // differs (no channel's scaled source lies on a whole level)
    assert_ne!(want(true)[0].to_bits(), want(false)[0].to_bits());
}
