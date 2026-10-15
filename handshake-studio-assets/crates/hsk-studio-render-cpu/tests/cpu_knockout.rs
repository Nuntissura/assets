//! Lane SMB-PS-ADJUST-STYLES slice 3: shallow knockout on the CPU backend (pigment
//! `knockout_math::knockout_over`), hand values end to end, chunk independence, and the typed
//! refusals of the placements pigment defines no rule for. Photoshop parity UNVERIFIED (D5).
use hsk_studio_pigment::effect_math::{ColourOverlayOp, EffectOp};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_render_cpu::Renderer;

mod common;
use common::*;

const NORMAL: StudioBlendMode = StudioBlendMode::Normal;

/// Red backdrop (root layer 0); isolated group 3 = green everywhere, then a blue square over
/// columns 1..=2 (group layer 1, the knockout layer) with `opacity` and `fill`.
fn plan_with(opacity: f32, fill: f32, isolated: bool) -> RenderPlan {
    let extent = rect(0, 0, 4, 1);
    let inner = vec![
        layer(1, NORMAL, 1.0),
        Layer { fill_opacity: fill, ..layer(2, NORMAL, opacity) },
    ];
    let group3 = if isolated { group(inner) } else { PlanNode::Group(Group { isolated: false, layers: inner }) };
    let mode3 = if isolated { NORMAL } else { StudioBlendMode::PassThrough };
    plan(
        extent,
        vec![
            solid(extent, [1.0, 0.0, 0.0, 1.0]),
            solid(extent, [0.0, 1.0, 0.0, 1.0]),
            solid(rect(1, 0, 2, 1), [0.0, 0.0, 1.0, 1.0]),
            group3,
            group(vec![layer(0, NORMAL, 1.0), layer(3, mode3, 1.0)]),
        ],
        4,
    )
}
fn ko(group: u32, layer: u32, depth: Knockout) -> PlanExtensions {
    envelope(vec![one_layer(group, layer).with_knockout(depth)])
}

#[test]
fn shallow_knockout_renders_and_misplaced_knockout_is_refused() {
    let r = Renderer::new(1).unwrap();
    let px = |p: &RenderPlan, e: &PlanExtensions, chunk: u32| {
        let (result, grid) = run_ext(&Renderer::with_threads(chunk, 1).unwrap(), p, e, Exactness::Approximate, 1 << 20, &no_sources());
        result.unwrap();
        (0..4).map(|x| grid.at(x, 0)).collect::<Vec<_>>()
    };
    // Fill 0, opacity 1: the square punches through the group's green to the group's initial
    // (transparent) backdrop, so the red backdrop of the root shows there.
    for chunk in [4, 1, 3] {
        let got = px(&plan_with(1.0, 0.0, true), &ko(3, 1, Knockout::Shallow), chunk);
        assert_eq!(got, vec![[0.0, 1.0, 0.0, 1.0], [1.0, 0.0, 0.0, 1.0], [1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]], "chunk {chunk}");
    }
    // Opacity 0.5, fill 1: inside the group (0, 0.6666667, 0.3333333) at alpha 0.75 (pigment
    // tests/knockout.rs), over opaque red: 0.75 x that + 0.25 x red = (0.25, 0.5, 0.25), alpha 1.
    let got = px(&plan_with(0.5, 1.0, true), &ko(3, 1, Knockout::Shallow), 4);
    near(got[1], [0.25, 0.5, 0.25, 1.0], 1e-6);
    near(got[0], [0.0, 1.0, 0.0, 1.0], 1e-6);
    // Without the knockout the blue covers the green at half strength instead.
    let plain = px(&plan_with(0.5, 1.0, true), &envelope(vec![]), 4);
    near(plain[1], [0.0, 0.5, 0.5, 1.0], 1e-6);

    // Typed refusals before any sink call: deep (no Studio rule), shallow on the root group,
    // shallow inside a pass-through group, shallow together with effects.
    let refused = |p: &RenderPlan, e: &PlanExtensions| {
        let (result, grid) = run_ext(&r, p, e, Exactness::Approximate, 1 << 20, &no_sources());
        assert_eq!(grid.calls, 0, "refused before the first sink call");
        result.unwrap_err()
    };
    let unsupported = |key: &'static str| BackendError::Unsupported { operator: key, exactness: Exactness::Unsupported };
    assert_eq!(refused(&plan_with(1.0, 0.0, true), &ko(3, 1, Knockout::Deep)), unsupported("knockout_deep"));
    assert_eq!(refused(&plan_with(1.0, 0.0, true), &ko(4, 1, Knockout::Shallow)), unsupported("knockout_shallow"));
    assert_eq!(refused(&plan_with(1.0, 0.0, false), &ko(3, 1, Knockout::Shallow)), unsupported("knockout_shallow"));
    let with_fx = envelope(vec![one_layer(3, 1).with_knockout(Knockout::Shallow).with_effects(vec![EffectOp::ColourOverlay(ColourOverlayOp::new([1.0; 3], NORMAL, 1.0))])]);
    assert_eq!(refused(&plan_with(1.0, 0.0, true), &with_fx), unsupported("knockout_shallow"));
    let backend: &dyn RenderBackend = &r;
    assert_eq!(backend.extension_exactness(ExtOperator::Knockout(Knockout::Shallow)), Exactness::Approximate);
}
