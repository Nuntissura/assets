//! Lane SMB-AE-MOTION-TOOLS slice 3: shape layers (STU-MOT-100..104). Groups of generators, paints
//! and nib path operators evaluated in authored order and lowered to Solid + coverage_u8 static
//! content. Every frame and coverage count is hand-computed on pixel-aligned geometry (coverage
//! bytes 0 or 255). After Effects parity UNVERIFIED (D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::BlendSpace;
use hsk_studio_composite::vector_nib::shape_layer::*;
use hsk_studio_nib::raster::RasterOptions;
use hsk_studio_nib::shape_ops::{RepeaterTransform, TrimMode};
use hsk_studio_nib::stroke::{LineCap, LineJoin};
use hsk_studio_nib::FillRule;
use hsk_studio_pigment::render_plan::{ColourTag, PlanNode, SourceKey};
use hsk_studio_pigment::Rect;

const SPACE: ColourTag = ColourTag { profile_sha256: [5; 32], blend_space: BlendSpace::Encoded };
const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

fn rect() -> ShapeItem {
    // Centre (50, 25), 100 x 50: x 0..100, y 0..50, path starting at (0, 0) clockwise.
    ShapeItem::Rect { position: [50.0, 25.0], size: [100.0, 50.0], roundness: 0.0, reversed: false }
}

fn group(items: Vec<ShapeItem>) -> ShapeGroup {
    ShapeGroup { items, transform: GroupTransform::IDENTITY }
}

fn stroke() -> ShapeItem {
    ShapeItem::Stroke(ShapeStroke { rgba: BLUE, opacity: 100.0, width: 10.0, cap: LineCap::Butt, join: LineJoin::Miter, miter_limit: 4.0, dash: None })
}

fn trim(end: f64) -> ShapeItem {
    ShapeItem::Trim { start: 0.0, end, offset: 0.0, mode: TrimMode::Simultaneously }
}

fn content(root: &ShapeGroup) -> Result<Option<hsk_studio_composite::StaticContent>, hsk_studio_composite::CompositeError> {
    shape_layer_content("SLYR-shape", root, 0.0, SPACE, 3, &RasterOptions::default(), &CancellationToken::default())
}

/// The trimmed half (0,0)->(100,0)->(100,50) stroked 10 px, butt caps, miter join:
/// [0,105] x [-5,5] U [95,105] x [5,50] = 1050 + 450 = 1500 px, frame (0, -5, 105, 55).
fn assert_half_stroke(c: &hsk_studio_composite::StaticContent) {
    let m = &c.owned_masks[&SourceKey(0)];
    assert_eq!(m.bounds, Rect { x: 0, y: -5, width: 105, height: 55 });
    assert_eq!(m.bytes.iter().filter(|v| **v == 255).count(), 1500);
    assert_eq!(m.bytes.iter().filter(|v| **v == 0).count(), 105 * 55 - 1500);
    assert_eq!(c.plan.nodes[0], PlanNode::Solid { rect: m.bounds, rgba: BLUE });
}

#[test]
fn repeater_fills_and_trimmed_strokes_follow_authored_order() {
    // Rect, fill, repeater x3 (+120 px, opacity 100 -> 50, copies above): back to front copy 0, 1, 2.
    let t = RepeaterTransform { anchor: [0.0; 2], position: [120.0, 0.0], scale: [100.0; 2], rotation: 0.0, start_opacity: 100.0, end_opacity: 50.0 };
    let fill = ShapeItem::Fill(ShapeFill { rgba: RED, opacity: 100.0, rule: FillRule::NonZero });
    let rep = |copies: f64| ShapeItem::Repeater { copies, offset: 0.0, transform: t, above: true };
    let c = content(&group(vec![rect(), fill.clone(), rep(3.0)])).unwrap().expect("ink");
    for (i, x) in [0i64, 120, 240].into_iter().enumerate() {
        let m = &c.owned_masks[&SourceKey(i as u32)];
        assert_eq!(m.bounds, Rect { x, y: 0, width: 100, height: 50 });
        assert!(m.bytes.iter().all(|v| *v == 255));
    }
    let PlanNode::Group(g) = &c.plan.nodes[3] else { panic!("root group") };
    assert_eq!(g.layers.iter().map(|l| l.opacity).collect::<Vec<_>>(), vec![1.0, 0.75, 0.5]);
    assert_eq!(c.rect, Rect { x: 0, y: 0, width: 340, height: 50 });

    // Trim after the stroke and trim before it both trim the stroked geometry (module rule).
    let after = content(&group(vec![rect(), stroke(), trim(50.0)])).unwrap().expect("ink");
    assert_half_stroke(&after);
    let before = content(&group(vec![rect(), trim(50.0), stroke()])).unwrap().expect("ink");
    assert_eq!(after.owned_masks, before.owned_masks);
    // A paint above every path draws nothing; a nested group moves by its transform.
    assert!(content(&group(vec![stroke(), rect()])).unwrap().is_none());
    let moved = ShapeGroup { items: vec![rect(), fill.clone()], transform: GroupTransform { position: [10.0, 20.0], ..GroupTransform::IDENTITY } };
    let nested = content(&group(vec![ShapeItem::Group(moved)])).unwrap().expect("ink");
    assert_eq!(nested.owned_masks[&SourceKey(0)].bounds, Rect { x: 10, y: 20, width: 100, height: 50 });

    // Hostile: repeater copies over the nib bound, a repeater whose copies overflow the path budget,
    // a NaN paint opacity and a group nesting bomb are refused before any plane is allocated.
    assert!(content(&group(vec![rect(), fill.clone(), rep(1025.0)])).is_err());
    let five = vec![rect(), rect(), rect(), rect(), rect(), fill.clone(), rep(1024.0)];
    let err = content(&group(five)).unwrap_err();
    assert_eq!(err, hsk_studio_composite::CompositeError::Unavailable("SLYR-shape:shape_repeater_budget".into()));
    let nan = ShapeItem::Fill(ShapeFill { rgba: RED, opacity: f64::NAN, rule: FillRule::NonZero });
    assert!(content(&group(vec![rect(), nan])).is_err());
    let mut bomb = group(vec![rect(), fill]);
    for _ in 0..40 {
        bomb = group(vec![ShapeItem::Group(bomb)]);
    }
    assert_eq!(content(&bomb).unwrap_err(), hsk_studio_composite::CompositeError::Unavailable("SLYR-shape:shape_depth".into()));
}

#[cfg(feature = "motion")]
#[test]
fn keyframed_trim_end_draws_the_stroke_on() {
    use hsk_studio_accord::Ticks;
    use hsk_studio_composite::vector_nib::shape_layer::track::*;
    use hsk_studio_motion::{EvalTick, Keyframe, Property};
    const S: u64 = 254_016_000_000;
    let end = Property::new("trim_end", vec![0.0], vec![Keyframe::new(Ticks::new(0), vec![0.0]), Keyframe::new(Ticks::new(S), vec![100.0])]).unwrap();
    let track = ShapeLayerTrack { base: group(vec![rect(), stroke(), trim(0.0)]), streams: vec![ShapeStream { path: vec![2], field: ShapeField::TrimEnd, property: end }] };
    let cancel = CancellationToken::default();
    // 0 s: end 0 % => nothing drawn; 0.5 s: end 50 % => the hand-computed half stroke.
    assert!(content(&track.at(EvalTick::new(0), &cancel).unwrap()).unwrap().is_none());
    assert_half_stroke(&content(&track.at(EvalTick::new((S / 2) as i64), &cancel).unwrap()).unwrap().expect("ink"));
    let wrong = ShapeLayerTrack { base: track.base.clone(), streams: vec![ShapeStream { path: vec![0], field: ShapeField::TrimEnd, property: Property::new("x", vec![0.0], vec![]).unwrap() }] };
    assert!(wrong.at(EvalTick::new(0), &cancel).is_err(), "a trim field on a rect item is refused");
}
