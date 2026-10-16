//! Lane SMB-PREMIERE-COLOR-TITLES slice 2: a Folio clip effect stack evaluated at clip-local ticks
//! through motion's keyframe evaluator (hand values), applied to a scene layer in stack order.
#![cfg(feature = "motion")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::clip_finishing::{
    ClipEffectFrame, ClipStage, NoLuts, apply_effects, clip_local_tick, evaluate_effect_stack,
};
use hsk_studio_composite::{SceneContent, SceneLayer, StudioBlendMode};
use hsk_studio_folio::{
    StudioClipEffect, StudioClipEffectParams, StudioClipEffectStack, StudioCompositionKeyframe,
    StudioInterpolation, StudioKeyedValue, StudioSmartFilterKind,
};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::transform_math::Affine2;

const F: u64 = 10_584_000_000;

fn key(time: u64, v: f64, interp: StudioInterpolation) -> StudioCompositionKeyframe {
    StudioCompositionKeyframe { time, value: vec![v], in_interp: interp, out_interp: interp, in_tangent: vec![], out_tangent: vec![] }
}
fn c(v: &[f64]) -> StudioKeyedValue {
    StudioKeyedValue::constant(v.to_vec())
}
fn entry(id: &str, enabled: bool, params: StudioClipEffectParams) -> StudioClipEffect {
    StudioClipEffect { effect_id: id.into(), enabled, params }
}

#[test]
fn clip_effect_stack_evaluates_keyframes_and_applies_in_order() {
    let cancel = CancellationToken::default();
    let stack = StudioClipEffectStack {
        clip_id: "SCLP-019abcde-0000-7000-8000-000000000005".into(),
        effects: vec![
            entry("grade", true, StudioClipEffectParams::ColorGrade(Box::default())),
            entry("crop", true, StudioClipEffectParams::Crop { left: c(&[25.0]), top: c(&[0.0]), right: c(&[12.5]), bottom: c(&[0.0]) }),
            entry("off", false, StudioClipEffectParams::Opacity { opacity: c(&[0.0]) }),
            entry(
                "fade",
                true,
                StudioClipEffectParams::Opacity {
                    opacity: StudioKeyedValue {
                        static_value: vec![100.0],
                        keyframes: vec![key(0, 0.0, StudioInterpolation::Linear), key(10 * F, 100.0, StudioInterpolation::Linear)],
                    },
                },
            ),
            entry(
                "move",
                true,
                StudioClipEffectParams::Transform {
                    anchor: c(&[2.0, 1.0]),
                    position: StudioKeyedValue {
                        static_value: vec![2.0, 1.0],
                        keyframes: vec![
                            StudioCompositionKeyframe { time: 0, value: vec![2.0, 1.0], in_interp: StudioInterpolation::Hold, out_interp: StudioInterpolation::Hold, in_tangent: vec![], out_tangent: vec![] },
                            StudioCompositionKeyframe { time: 4 * F, value: vec![6.0, 2.0], in_interp: StudioInterpolation::Hold, out_interp: StudioInterpolation::Hold, in_tangent: vec![], out_tangent: vec![] },
                        ],
                    },
                    scale: c(&[100.0, 100.0]),
                    rotation: c(&[0.0]),
                },
            ),
            entry("blur", true, StudioClipEffectParams::GaussianBlur { blurriness: StudioKeyedValue {
                static_value: vec![0.0],
                keyframes: vec![key(0, 0.0, StudioInterpolation::Linear), key(2 * F, 4.0, StudioInterpolation::Linear)],
            } }),
            entry("noise", true, StudioClipEffectParams::PigmentFilter { filter: StudioSmartFilterKind::Median { radius_px: 1 } }),
        ],
    };
    // Sequence tick 15F for a clip starting at 10F = clip tick 5F: fade 50, position held at the
    // 4F key (6, 2), blur past its last key (4 px).
    let tick = clip_local_tick(15 * F, 10 * F).unwrap();
    assert_eq!(tick, 5 * F as i64);
    assert_eq!(clip_local_tick(0, 10 * F), Some(-(10 * F as i64)));
    let frames = evaluate_effect_stack(&stack, tick, &NoLuts, [0.2126, 0.7152, 0.0722], &cancel).unwrap();
    assert_eq!(frames.len(), 6, "the disabled entry is skipped");
    assert!(matches!(frames[0], ClipEffectFrame::Grade(_)));
    assert!(matches!(frames[1], ClipEffectFrame::Crop { left, right, .. } if left == 25.0 && right == 12.5));
    assert!(matches!(frames[2], ClipEffectFrame::Opacity(o) if (o - 50.0).abs() < 1e-9));
    match &frames[3] {
        ClipEffectFrame::Transform { anchor, position, scale, rotation } => {
            assert_eq!((*anchor, *position, *scale, *rotation), ([2.0, 1.0], [6.0, 2.0], [100.0, 100.0], 0.0));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(&frames[4], ClipEffectFrame::Filters(op) if op.entries.len() == 1 && op.check()));
    // Before the position key at 4F the hold keeps (2, 1); at clip tick F the fade is 10 %.
    let early = evaluate_effect_stack(&stack, F as i64, &NoLuts, [0.2126, 0.7152, 0.0722], &cancel).unwrap();
    assert!(matches!(early[2], ClipEffectFrame::Opacity(o) if (o - 10.0).abs() < 1e-9));
    assert!(matches!(&early[3], ClipEffectFrame::Transform { position, .. } if *position == [2.0, 1.0]));

    // Applied: pixel effects wrap the content innermost-first (grade, crop, then the filters);
    // the transform moves anchor (2, 1) to (6, 2) = translate (4, 1); opacity halves.
    let layer = SceneLayer {
        layer_id: "clip".into(),
        content: SceneContent::None,
        mode: StudioBlendMode::Normal,
        opacity: 1.0,
        fill_opacity: 1.0,
        matrix: Affine2::IDENTITY,
        matte: None,
        clipped: false,
    };
    let rect = Rect { x: 0, y: 0, width: 4, height: 2 };
    let out = apply_effects(layer, rect, &frames).unwrap();
    assert_eq!(out.matrix, Affine2::translate(4.0, 1.0));
    assert!((out.opacity - 0.5).abs() < 1e-12);
    let SceneContent::Filtered(f2) = &out.content else { panic!("outer filter") };
    let SceneContent::Filtered(f1) = &f2.content else { panic!("blur") };
    let SceneContent::Graded(crop) = &f1.content else { panic!("crop") };
    assert!(matches!(crop.stage, ClipStage::Crop { left, right, .. } if left == 0.25 && right == 0.125));
    let SceneContent::Graded(grade) = &crop.content else { panic!("grade") };
    assert!(matches!(grade.stage, ClipStage::Grade(_)));

    // Crop coverage per texel (box coverage): left 1 px removed, right half a pixel.
    let s = ClipStage::Crop { left: 0.25, top: 0.0, right: 0.125, bottom: 0.0 };
    assert_eq!(s.texel([0.5; 3], 1.0, 0.0, 0.0, 4.0, 2.0).1, 0.0);
    assert_eq!(s.texel([0.5; 3], 1.0, 1.0, 0.0, 4.0, 2.0).1, 1.0);
    assert_eq!(s.texel([0.5; 3], 0.8, 3.0, 1.0, 4.0, 2.0).1, 0.4);

    // Hostile: non-finite or out-of-range evaluated values are refused, not clamped.
    let layer = SceneLayer { content: SceneContent::None, ..out.clone() };
    assert!(apply_effects(layer.clone(), rect, &[ClipEffectFrame::Opacity(f64::NAN)]).is_err());
    assert!(apply_effects(layer.clone(), rect, &[ClipEffectFrame::Crop { left: 101.0, top: 0.0, right: 0.0, bottom: 0.0 }]).is_err());
    assert!(apply_effects(layer, rect, &[ClipEffectFrame::Transform { anchor: [0.0; 2], position: [f64::INFINITY, 0.0], scale: [100.0; 2], rotation: 0.0 }]).is_err());
}
