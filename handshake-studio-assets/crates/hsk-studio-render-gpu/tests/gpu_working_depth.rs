//! Lane SMB-PS-ORACLE-PARITY-2 on the GPU backend: the 8-bit working depth (`Exec::store_depth`
//! after every layer but the root's last, the 8-bit Levels body of kernel.wgsl) and the pass-through
//! group opacity (`composite_mix` with a uniform weight) against pigment's composition of its single
//! owners (`render_plan::quantise_8bit`, `LevelsRecord::map_value_8bit`,
//! `blend_math::pass_through_mix`), which render-cpu reproduces bit for bit (render-cpu
//! tests/cpu_working_depth.rs). GPU-execution proofs follow `tests/gpu-proof.toml` (V4-03); the
//! declaration test is hardware-independent.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::adjust_math::{
    AdjustmentOp, Levels, LevelsRecord, composite_adjustment,
};
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over, pass_through_mix};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BlendSpace, ColourTag, Exactness, ExecuteRequest, ExtOperator, Group, Layer,
    LayerExtension, NoVector, Operator, OutputStage, OutputTarget, PlanExtensions, PlanNode,
    PlanNodeId, PlanSources, RenderBackend, RenderPlan, SampleFormat, SinkError, SourceKey,
    StudioBlendMode, TilePlanes, TileSink, WorkingDepth, quantise_8bit,
};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_render_gpu::{DeviceOptions, GpuBackend, Reason};
use std::sync::OnceLock;

/// Alpha and premultiplied colour: the composites run in binary32 with fused multiply-add where
/// pigment rounds each step (measured values are printed).
const TOLERANCE: f32 = 1e-5;
const W: u32 = 9;
const H: u32 = 5;
const NORMAL: StudioBlendMode = StudioBlendMode::Normal;
const A: [f32; 4] = [0.3, 0.6, 0.9, 1.0];
const B: [f32; 4] = [0.2, 0.45, 0.75, 1.0];

fn extent() -> Rect {
    Rect { x: 0, y: 0, width: W, height: H }
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
            space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded },
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
fn backend() -> &'static GpuBackend {
    static SHARED: OnceLock<GpuBackend> = OnceLock::new();
    SHARED.get_or_init(|| GpuBackend::new(DeviceOptions { chunk_side: 8, ..DeviceOptions::default() }))
}
fn gate(what: &str) -> bool {
    let b = backend();
    if b.device_info().is_some() {
        return true;
    }
    if let Some(failure) = b.failure() {
        panic!("{what}: adapter present but the backend failed: {failure:?}");
    }
    let strict = include_str!("gpu-proof.toml").lines().any(|l| l.trim() == "require_gpu = true")
        || std::env::var("HSK_STUDIO_TEST_REQUIRE_GPU").as_deref() == Ok("1");
    match b.unavailable_reason() {
        Some(Reason::NoAdapter) if strict => {
            panic!("NOT_PROVEN: no adapter; {what} requires a GPU (tests/gpu-proof.toml)")
        }
        Some(Reason::NoAdapter) => {
            eprintln!("NOT_PROVEN: no adapter; {what} not run");
            false
        }
        other => panic!("{what}: no live device ({other:?})"),
    }
}
fn render(p: &RenderPlan<NoVector>, ext: &PlanExtensions) -> Vec<[f32; 4]> {
    let mut image = Image(vec![[f32::NAN; 4]; (W * H) as usize]);
    let request = ExecuteRequest {
        expected_revision: 3,
        min_exactness: Exactness::Approximate,
        deadline: None,
        max_working_bytes: 1 << 26,
    };
    let receipt = backend()
        .execute_extended(p, ext, &request, &NoSources, &CancellationToken::default(), &mut image)
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
fn deviation(got: &[[f32; 4]], want: impl Fn(u32, u32) -> [f32; 4]) -> f32 {
    let pre = |p: [f32; 4]| [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3]];
    let mut dev = 0.0f32;
    for y in 0..H {
        for x in 0..W {
            let (g, w) = (pre(got[(y * W + x) as usize]), pre(want(x, y)));
            for c in 0..4 {
                dev = dev.max((g[c] - w[c]).abs());
            }
        }
    }
    dev
}

#[test]
fn working_depth_and_pass_through_opacity_are_declared() {
    assert_eq!(
        backend().extension_exactness(ExtOperator::WorkingDepthEight),
        Exactness::Approximate
    );
    assert_eq!(backend().exactness(Operator::PassThroughOpacity), Exactness::Approximate);
}

/// Root [A Normal, B Multiply 0.9, Levels (in 20/230, gamma 1.5, out 10/240)] at the 8-bit working
/// depth: the Multiply result is stored as levels 22, 77, 178 (no rounding tie in the staged
/// Levels), the Levels runs on whole levels; the float depth differs by more than half a code.
#[test]
fn gpu_eight_bit_working_depth_matches_pigment() {
    if !gate("gpu 8-bit working depth parity") {
        return;
    }
    let r = LevelsRecord {
        input_black: 20.0 / 255.0,
        input_white: 230.0 / 255.0,
        gamma: 1.5,
        output_black: 10.0 / 255.0,
        output_white: 240.0 / 255.0,
    };
    let levels = AdjustmentOp::Levels(Levels {
        composite: r,
        red: LevelsRecord::IDENTITY,
        green: LevelsRecord::IDENTITY,
        blue: LevelsRecord::IDENTITY,
    });
    let p = plan(
        vec![
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
        ],
        3,
    );
    let entry = LayerExtension::new(PlanNodeId(3), 2).with_adjustment(levels.clone());
    let eight = PlanExtensions::from_layers(vec![entry])
        .with_output(OutputStage::default().with_working_depth(WorkingDepth::Eight));
    let q = |p: [f32; 4]| p.map(quantise_8bit);
    let b1 = q(over(NORMAL, [0.0; 4], A, 1.0));
    let b2 = q(over(StudioBlendMode::Multiply, b1, B, 0.9));
    let prepared = levels.prepare().unwrap().with_eight_bit(true);
    let out = composite_adjustment(NORMAL, Rgba::new([b2[0], b2[1], b2[2]], b2[3]), &prepared, 1.0)
        .unwrap();
    let want = [out.rgb[0], out.rgb[1], out.rgb[2], out.a];
    let got = render(&p, &eight);
    let dev = deviation(&got, |_, _| want);
    println!("8-bit working depth: max |delta| = {dev:e} (tolerance {TOLERANCE:e})");
    assert!(dev <= TOLERANCE, "deviation {dev:e}: {:?} vs {want:?}", got[0]);
    // the float depth renders something else
    let float = render(&p, &PlanExtensions::from_layers(vec![LayerExtension::new(PlanNodeId(3), 2).with_adjustment(levels)]));
    assert!(deviation(&float, |_, _| want) > 0.5 / 255.0);
}

/// Root [A Normal, pass-through group [B' Multiply] at opacity 0.5]: pigment `pass_through_mix`.
#[test]
fn gpu_pass_through_opacity_matches_pigment() {
    if !gate("gpu pass-through opacity parity") {
        return;
    }
    let inner = Rect { x: 2, y: 1, width: 5, height: 3 };
    let b = [0.2, 0.45, 0.75, 0.6];
    let p = plan(
        vec![
            PlanNode::Solid { rect: extent(), rgba: A },
            PlanNode::Solid { rect: inner, rgba: b },
            PlanNode::Group(Group { isolated: false, layers: vec![layer(1, StudioBlendMode::Multiply, 1.0)] }),
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![layer(0, NORMAL, 1.0), layer(2, StudioBlendMode::PassThrough, 0.5)],
            }),
        ],
        3,
    );
    let before = over(NORMAL, [0.0; 4], A, 1.0);
    let after = over(StudioBlendMode::Multiply, before, b, 1.0);
    let mixed = pass_through_mix(
        Rgba::new([before[0], before[1], before[2]], before[3]),
        Rgba::new([after[0], after[1], after[2]], after[3]),
        0.5,
    );
    let inside = [mixed.rgb[0], mixed.rgb[1], mixed.rgb[2], mixed.a.min(1.0)];
    let got = render(&p, &PlanExtensions::default());
    let dev = deviation(&got, |x, y| {
        if (2..7).contains(&x) && (1..4).contains(&y) { inside } else { before }
    });
    println!("pass-through opacity: max |delta| = {dev:e} (tolerance {TOLERANCE:e})");
    assert!(dev <= TOLERANCE, "deviation {dev:e}");
}
