//! Lane SMB-PS-ADJUST-STYLES-2: deep-knockout helpers of `knockout_math` (hand values of the
//! documented formulas: clear = premultiplied lerp toward transparent, union = `k + s (1 - k)`) and
//! the admission of the knockout placements by `PlanExtensions::validate` (root, pass-through,
//! deep; a plain ancestry for the placements that read outside the layer's own buffer).
//! Photoshop parity UNVERIFIED (D5).
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::{BlendError, Rgba};
use hsk_studio_pigment::effect_math::{ColourOverlayOp, EffectOp};
use hsk_studio_pigment::knockout_math::{knockout_clear, knockout_union};
use hsk_studio_pigment::render_plan::*;

const R: Rect = Rect { x: 0, y: 0, width: 2, height: 1 };

#[test]
fn clear_and_union_hand_values() {
    // clear: alpha 0.8 by k 0.25 -> 0.8 - 0.2 = 0.6, the straight colour kept; k 1 -> transparent
    let c = knockout_clear(Rgba::new([0.2, 0.4, 0.6], 0.8), 0.25).unwrap();
    assert_eq!(c.rgb, [0.2, 0.4, 0.6]);
    assert!((c.a - 0.6).abs() < 1e-7);
    assert_eq!(knockout_clear(Rgba::new([0.2, 0.4, 0.6], 0.8), 1.0).unwrap(), Rgba::TRANSPARENT);
    assert_eq!(knockout_clear(Rgba::new([0.2, 0.4, 0.6], 0.8), 0.0).unwrap(), Rgba::new([0.2, 0.4, 0.6], 0.8));
    // union: 0.5 and 0.5 -> 0.75; with 0 -> the other; with 1 -> 1
    assert_eq!(knockout_union(0.5, 0.5).unwrap(), 0.75);
    assert_eq!(knockout_union(0.0, 0.3).unwrap(), 0.3);
    assert_eq!(knockout_union(1.0, 0.3).unwrap(), 1.0);
    // hostile shapes are domain errors, never clamped in
    for bad in [-0.1, 1.5, f32::NAN, f32::INFINITY] {
        assert_eq!(knockout_clear(Rgba::TRANSPARENT, bad), Err(BlendError::OutOfDomain));
        assert_eq!(knockout_union(bad, 0.0), Err(BlendError::OutOfDomain));
        assert_eq!(knockout_union(0.0, bad), Err(BlendError::OutOfDomain));
    }
}

fn layer(input: u32, mode: StudioBlendMode) -> Layer {
    Layer { input: PlanNodeId(input), mode, opacity: 1.0, fill_opacity: 1.0, mask: None }
}

/// 0 solid, 1 solid; 2 = group(isolated?) [0, 1]; 3 = root isolated [0, layer(2, mode)] plus
/// optional extra uses of node 2.
fn plan(inner_isolated: bool, uses: usize) -> RenderPlan {
    let mode = if inner_isolated { StudioBlendMode::Normal } else { StudioBlendMode::PassThrough };
    let mut root = vec![layer(0, StudioBlendMode::Normal)];
    for _ in 0..uses {
        root.push(layer(2, mode));
    }
    RenderPlan {
        revision: 1,
        target: OutputTarget {
            extent: R,
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded },
        },
        nodes: vec![
            PlanNode::Solid { rect: R, rgba: [0.2, 0.4, 0.6, 1.0] },
            PlanNode::Solid { rect: R, rgba: [0.9, 0.1, 0.1, 1.0] },
            PlanNode::Group(Group { isolated: inner_isolated, layers: vec![layer(0, StudioBlendMode::Normal), layer(1, StudioBlendMode::Normal)] }),
            PlanNode::Group(Group { isolated: true, layers: root }),
        ],
        root: PlanNodeId(3),
    }
}

#[test]
fn placements_are_admitted_with_a_plain_ancestry() {
    let ko = |group: u32, depth: Knockout| LayerExtension::new(PlanNodeId(group), 1).with_knockout(depth);
    let ok = |p: &RenderPlan, entries: Vec<LayerExtension>, depth: Knockout| {
        assert_eq!(PlanExtensions::from_layers(entries).validate(p), Ok(vec![ExtOperator::Knockout(depth)]));
    };
    let refused = |p: &RenderPlan, entries: Vec<LayerExtension>, node: u32, depth: Knockout| {
        assert_eq!(PlanExtensions::from_layers(entries).validate(p), Err(PlanError::KnockoutUnsupported { node, depth }));
    };
    // root level (node 3): shallow and deep
    ok(&plan(true, 1), vec![ko(3, Knockout::Shallow)], Knockout::Shallow);
    ok(&plan(true, 1), vec![ko(3, Knockout::Deep)], Knockout::Deep);
    // isolated inner group: shallow needs no ancestry; deep needs it and has it
    ok(&plan(true, 1), vec![ko(2, Knockout::Shallow)], Knockout::Shallow);
    ok(&plan(true, 1), vec![ko(2, Knockout::Deep)], Knockout::Deep);
    // pass-through inner group: shallow and deep, plain ancestry
    ok(&plan(false, 1), vec![ko(2, Knockout::Shallow)], Knockout::Shallow);
    ok(&plan(false, 1), vec![ko(2, Knockout::Deep)], Knockout::Deep);
    // the inner group used twice: deep and pass-through shallow are refused, isolated shallow not
    refused(&plan(true, 2), vec![ko(2, Knockout::Deep)], 2, Knockout::Deep);
    refused(&plan(false, 2), vec![ko(2, Knockout::Shallow)], 2, Knockout::Shallow);
    ok(&plan(true, 2), vec![ko(2, Knockout::Shallow)], Knockout::Shallow);
    // the group layer of the inner group carries an operator (effects): refused
    let overlay = EffectOp::ColourOverlay(ColourOverlayOp::new([1.0; 3], StudioBlendMode::Normal, 1.0));
    refused(&plan(true, 1), vec![ko(2, Knockout::Deep), LayerExtension::new(PlanNodeId(3), 1).with_effects(vec![overlay])], 2, Knockout::Deep);
    // a knockout on a pass-through layer is refused at any depth
    let mut p = plan(true, 1);
    if let PlanNode::Group(g) = &mut p.nodes[3] {
        g.layers[1].mode = StudioBlendMode::PassThrough;
        if let PlanNode::Group(inner) = &mut p.nodes[2] {
            inner.isolated = false;
        }
    }
    refused(&p, vec![ko(3, Knockout::Deep)], 3, Knockout::Deep);
}
