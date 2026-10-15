//! Lane SMB-PS-ADJUST-STYLES-2: knockout placements on the CPU backend (pigment `knockout_math`):
//! shallow at the plan root, shallow in a pass-through group (initial backdrop = the running
//! result before the group), and deep (to the transparent document base, clearing every enclosing
//! backdrop, through nested isolated groups and pass-through groups). Hand values derived in the
//! comments; every case is checked for chunk sizes 1, 3, 4 and two workers. Photoshop parity
//! UNVERIFIED (D5).
use hsk_studio_pigment::effect_math::{ColourOverlayOp, EffectOp};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_render_cpu::Renderer;

mod common;
use common::*;

const NORMAL: StudioBlendMode = StudioBlendMode::Normal;
const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
const CLEAR: [f32; 4] = [0.0; 4];

/// Nodes: 0 red everywhere, 1 green everywhere, 2 blue over columns 1..=2 (4 x 1 extent).
fn leaves() -> Vec<PlanNode> {
    let extent = rect(0, 0, 4, 1);
    vec![solid(extent, RED), solid(extent, GREEN), solid(rect(1, 0, 2, 1), BLUE)]
}
fn blue_layer(mode: StudioBlendMode, opacity: f32, fill: f32) -> Layer {
    Layer { fill_opacity: fill, ..layer(2, mode, opacity) }
}
fn pass_through(layers: Vec<Layer>) -> PlanNode {
    PlanNode::Group(Group { isolated: false, layers })
}

/// Rendered row for every chunking / worker count (all must agree bit for bit).
fn row(p: &RenderPlan, e: &PlanExtensions) -> Vec<[f32; 4]> {
    let mut first: Option<Vec<[f32; 4]>> = None;
    for (chunk, threads) in [(4, 1), (1, 1), (3, 1), (1, 2)] {
        let (result, grid) = run_ext(&Renderer::with_threads(chunk, threads).unwrap(), p, e, Exactness::Approximate, 1 << 20, &no_sources());
        result.unwrap_or_else(|err| panic!("chunk {chunk} threads {threads}: {err:?}"));
        let got: Vec<[f32; 4]> = (0..4).map(|x| grid.at(x, 0)).collect();
        match &first {
            None => first = Some(got),
            Some(f) => assert_eq!(f.iter().flatten().map(|v| v.to_bits()).collect::<Vec<_>>(), got.iter().flatten().map(|v| v.to_bits()).collect::<Vec<_>>(), "chunk {chunk} threads {threads}"),
        }
    }
    first.unwrap()
}
fn ko(group: u32, layer: u32, depth: Knockout) -> PlanExtensions {
    envelope(vec![one_layer(group, layer).with_knockout(depth)])
}

#[test]
fn shallow_at_the_root_and_in_a_pass_through_group() {
    // Root-level shallow: root 3 = [red, blue fill 0 knockout]: the blue square punches through to
    // the transparent document base -> red, clear, clear, red.
    let mut nodes = leaves();
    nodes.push(group(vec![layer(0, NORMAL, 1.0), blue_layer(NORMAL, 1.0, 0.0)]));
    let p = plan(rect(0, 0, 4, 1), nodes, 3);
    assert_eq!(row(&p, &ko(3, 1, Knockout::Shallow)), vec![RED, CLEAR, CLEAR, RED]);
    // At fill 1 the square is the blue over transparent, i.e. blue.
    let mut nodes = leaves();
    nodes.push(group(vec![layer(0, NORMAL, 1.0), blue_layer(NORMAL, 1.0, 1.0)]));
    assert_eq!(row(&plan(rect(0, 0, 4, 1), nodes, 3), &ko(3, 1, Knockout::Shallow)), vec![RED, BLUE, BLUE, RED]);

    // Pass-through group 3 = [green, blue multiply knockout] in root 4 = [red, group 3]. The
    // knockout's initial backdrop is the running result before the group (red): S = multiply(red,
    // blue) = black, shape 1 -> black at columns 1..=2. In an ISOLATED group 3 the initial
    // backdrop is transparent: multiply over transparent is the source (blue), then the group
    // composites Normal over red -> blue. The placement decides the colour.
    let build = |isolated: bool| {
        let mut nodes = leaves();
        let inner = vec![layer(1, NORMAL, 1.0), blue_layer(StudioBlendMode::Multiply, 1.0, 1.0)];
        nodes.push(if isolated { group(inner) } else { pass_through(inner) });
        let mode = if isolated { NORMAL } else { StudioBlendMode::PassThrough };
        nodes.push(group(vec![layer(0, NORMAL, 1.0), layer(3, mode, 1.0)]));
        plan(rect(0, 0, 4, 1), nodes, 4)
    };
    let black = [0.0, 0.0, 0.0, 1.0];
    assert_eq!(row(&build(false), &ko(3, 1, Knockout::Shallow)), vec![GREEN, black, black, GREEN]);
    assert_eq!(row(&build(true), &ko(3, 1, Knockout::Shallow)), vec![GREEN, BLUE, BLUE, GREEN]);
    // Fill 0 in the pass-through group: S = red (the snapshot) at opacity 0 -> red shows.
    let mut nodes = leaves();
    nodes.push(pass_through(vec![layer(1, NORMAL, 1.0), blue_layer(NORMAL, 1.0, 0.0)]));
    nodes.push(group(vec![layer(0, NORMAL, 1.0), layer(3, StudioBlendMode::PassThrough, 1.0)]));
    assert_eq!(row(&plan(rect(0, 0, 4, 1), nodes, 4), &ko(3, 1, Knockout::Shallow)), vec![GREEN, RED, RED, GREEN]);
}

#[test]
fn deep_knockout_clears_every_enclosing_backdrop() {
    // Root 4 = [red, isolated group 3 = [green, blue fill 0 DEEP]]: in group 3 the square is
    // knocked to transparent (shape 1), and the root's red under it is cleared too -> green,
    // clear, clear, green. Shallow instead keeps the red: green, red, red, green.
    let build = |opacity: f32, fill: f32| {
        let mut nodes = leaves();
        nodes.push(group(vec![layer(1, NORMAL, 1.0), blue_layer(NORMAL, opacity, fill)]));
        nodes.push(group(vec![layer(0, NORMAL, 1.0), layer(3, NORMAL, 1.0)]));
        plan(rect(0, 0, 4, 1), nodes, 4)
    };
    assert_eq!(row(&build(1.0, 0.0), &ko(3, 1, Knockout::Deep)), vec![GREEN, CLEAR, CLEAR, GREEN]);
    assert_eq!(row(&build(1.0, 0.0), &ko(3, 1, Knockout::Shallow)), vec![GREEN, RED, RED, GREEN]);
    // Opacity 0.5, fill 1: in group 3 lerp(green, blue at alpha 0.5, 0.5) premultiplied = alpha
    // 0.75, colour (0, 0.5, 0.25) / 0.75 = (0, 0.6666667, 0.3333333); K = 0.5 clears the root red
    // to alpha 0.5; the group over it: alpha 0.75 + 0.5 x 0.25 = 0.875, premultiplied colour
    // (0, 0.5, 0.25) + (1, 0, 0) x 0.5 x 0.25 = (0.125, 0.5, 0.25) -> (0.1428571, 0.5714286,
    // 0.2857143).
    let got = row(&build(0.5, 1.0), &ko(3, 1, Knockout::Deep));
    near(got[1], [0.142_857_1, 0.571_428_6, 0.285_714_3, 0.875], 1e-6);
    near(got[0], GREEN, 0.0);

    // Through two isolated levels: root 5 = [red, group 4 = [green, group 3 = [blue fill 0 DEEP]]]:
    // the shape clears group 4's green and then the root's red.
    let mut nodes = leaves();
    nodes.push(group(vec![blue_layer(NORMAL, 1.0, 0.0)]));
    nodes.push(group(vec![layer(1, NORMAL, 1.0), layer(3, NORMAL, 1.0)]));
    nodes.push(group(vec![layer(0, NORMAL, 1.0), layer(4, NORMAL, 1.0)]));
    assert_eq!(row(&plan(rect(0, 0, 4, 1), nodes, 5), &ko(3, 0, Knockout::Deep)), vec![GREEN, CLEAR, CLEAR, GREEN]);

    // Through a pass-through group (it composites into the shared root buffer): root 4 = [red,
    // pass-through 3 = [green, blue fill 0 DEEP]].
    let mut nodes = leaves();
    nodes.push(pass_through(vec![layer(1, NORMAL, 1.0), blue_layer(NORMAL, 1.0, 0.0)]));
    nodes.push(group(vec![layer(0, NORMAL, 1.0), layer(3, StudioBlendMode::PassThrough, 1.0)]));
    assert_eq!(row(&plan(rect(0, 0, 4, 1), nodes, 4), &ko(3, 1, Knockout::Deep)), vec![GREEN, CLEAR, CLEAR, GREEN]);

    // Layers above the deep knockout composite normally over the hole: root 4 = [red, group 3 =
    // [green, blue fill 0 DEEP]], then a half-opaque green on top (root layer 2).
    let mut nodes = leaves();
    nodes.push(group(vec![layer(1, NORMAL, 1.0), blue_layer(NORMAL, 1.0, 0.0)]));
    nodes.push(group(vec![layer(0, NORMAL, 1.0), layer(3, NORMAL, 1.0), layer(1, NORMAL, 0.5)]));
    let got = row(&plan(rect(0, 0, 4, 1), nodes, 4), &ko(3, 1, Knockout::Deep));
    near(got[1], [0.0, 1.0, 0.0, 0.5], 1e-6);
}

/// The placements that read outside the layer's own buffer need a plain ancestry; otherwise the
/// refusal is typed and happens before any sink call. The knockout scratch is charged up front.
#[test]
fn deep_and_pass_through_refusals_and_scratch_charge() {
    let r = Renderer::new(4).unwrap();
    let refused = |p: &RenderPlan, e: &PlanExtensions, budget: u64| {
        let (result, grid) = run_ext(&r, p, e, Exactness::Approximate, budget, &no_sources());
        assert_eq!(grid.calls, 0, "refused before the first sink call");
        result.unwrap_err()
    };
    let unsupported = |key: &'static str| BackendError::Unsupported { operator: key, exactness: Exactness::Unsupported };
    let build = || {
        let mut nodes = leaves();
        nodes.push(group(vec![layer(1, NORMAL, 1.0), blue_layer(NORMAL, 1.0, 0.0)]));
        nodes.push(group(vec![layer(0, NORMAL, 1.0), layer(3, NORMAL, 1.0)]));
        plan(rect(0, 0, 4, 1), nodes, 4)
    };
    // the group layer carrying the deep knockout's group has effects: not a plain ancestry
    let overlay = EffectOp::ColourOverlay(ColourOverlayOp::new([1.0; 3], NORMAL, 0.5));
    let with_fx = envelope(vec![one_layer(3, 1).with_knockout(Knockout::Deep), one_layer(4, 1).with_effects(vec![overlay])]);
    assert_eq!(refused(&build(), &with_fx, 1 << 20), unsupported("knockout_deep"));
    // the group is used twice: not a plain ancestry
    let mut nodes = leaves();
    nodes.push(group(vec![layer(1, NORMAL, 1.0), blue_layer(NORMAL, 1.0, 0.0)]));
    nodes.push(group(vec![layer(0, NORMAL, 1.0), layer(3, NORMAL, 1.0), layer(3, NORMAL, 0.5)]));
    assert_eq!(refused(&plan(rect(0, 0, 4, 1), nodes, 4), &ko(3, 1, Knockout::Deep), 1 << 20), unsupported("knockout_deep"));
    // a shallow knockout in a pass-through group whose group layer is clipped is refused too
    let mut nodes = leaves();
    nodes.push(pass_through(vec![layer(1, NORMAL, 1.0), blue_layer(NORMAL, 1.0, 0.0)]));
    nodes.push(group(vec![layer(0, NORMAL, 1.0), Layer { mask: Some(MaskInput::clip_to_base()), ..layer(3, StudioBlendMode::PassThrough, 1.0) }]));
    let clipped = refused(&plan(rect(0, 0, 4, 1), nodes, 4), &ko(3, 1, Knockout::Shallow), 1 << 20);
    assert!(matches!(clipped, BackendError::Unsupported { .. } | BackendError::InvalidPlan(_)), "{clipped:?}");
    // The deep shape planes (one per isolated level plus one, 4 bytes per chunk pixel) are part of
    // the reserved scratch: a budget that holds the plain plan's scratch but not the planes is
    // BudgetExceeded before any sink call.
    let plain_peak = {
        let (result, _) = run_ext(&r, &build(), &envelope(vec![]), Exactness::Approximate, 1 << 20, &no_sources());
        result.unwrap().peak_working_bytes
    };
    let (result, grid) = run_ext(&r, &build(), &ko(3, 1, Knockout::Deep), Exactness::Approximate, plain_peak, &no_sources());
    assert!(matches!(result, Err(BackendError::BudgetExceeded { .. })), "{result:?}");
    assert_eq!(grid.calls, 0);
}
