//! Lane SMB-AE-MOTION-TOOLS slice 2: per-glyph animated text lowering. Motion evaluates the text
//! animators; composite moves each Type outline, rasterises it with nib and paints it as static
//! content. Glyphs are synthetic 500 x 500 unit squares (Type `GlyphOutline::synthetic`, 1000 upem),
//! so every expected frame, coverage byte and colour is hand-computed.
#![cfg(feature = "motion")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::text::animate::{
    AnimatedGlyph, animated_text_content, glyph_from_outline, text_chars,
};
use hsk_studio_composite::BlendSpace;
use hsk_studio_motion::*;
use hsk_studio_nib::raster::RasterOptions;
use hsk_studio_pigment::render_plan::{ColourTag, MaskFormat, PlanNode, SourceKey};
use hsk_studio_pigment::Rect;
use hsk_studio_type::{GlyphOutline, OutlineCommand};

fn square() -> GlyphOutline {
    GlyphOutline::synthetic(
        1000,
        vec![
            OutlineCommand::MoveTo([0.0, 0.0]),
            OutlineCommand::LineTo([500.0, 0.0]),
            OutlineCommand::LineTo([500.0, 500.0]),
            OutlineCommand::LineTo([0.0, 500.0]),
            OutlineCommand::Close,
        ],
    )
}

/// Two 10 px squares (scale 20 px/em / 1000 upem), pens at (10, 30) and (22, 30), advance 12 px.
fn glyphs() -> Vec<AnimatedGlyph> {
    let s = square();
    vec![
        glyph_from_outline(&s, [10.0, 30.0], 0.02, 12.0, 20.0, 0).unwrap(),
        glyph_from_outline(&s, [22.0, 30.0], 0.02, 12.0, 20.0, 0).unwrap(),
    ]
}

fn index(start: f64, end: f64) -> Selector {
    Selector::Range(RangeSelector { start, end, units: RangeUnits::Index, ..RangeSelector::default() })
}

const SPACE: ColourTag = ColourTag { profile_sha256: [3; 32], blend_space: BlendSpace::Encoded };
const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

#[test]
fn animated_glyphs_lower_to_hand_computed_planes() {
    let g = glyphs();
    // Glyph 0 occupies x 10..20, y 20..30 (font y up -> pixels y down), anchor (16, 30).
    assert_eq!(g[0].anchor_px, [16.0, 30.0]);
    let chars = text_chars(&g);
    assert_eq!(chars.len(), 2);
    // Animator 1 on character 0: rotation 90 and tracking 500 (= 10 px at 20 px/em).
    // Animator 2 on character 1: position (0, -10), opacity 50 %, fill red.
    let animators = [
        Animator {
            properties: AnimatorProperties { rotation: Some(90.0), tracking: Some(500.0), ..AnimatorProperties::default() },
            selectors: vec![index(0.0, 1.0)],
        },
        Animator {
            properties: AnimatorProperties {
                position: Some([0.0, -10.0]),
                opacity: Some(50.0),
                fill: Some([1.0, 0.0, 0.0, 1.0]),
                ..AnimatorProperties::default()
            },
            selectors: vec![index(1.0, 2.0)],
        },
    ];
    let t = evaluate_text_animators(&chars, &animators, [0.0, 0.0, 1.0, 1.0], 0.0).unwrap();
    let content = animated_text_content("SLYR-text", &g, &t, BLUE, SPACE, 7, &RasterOptions::default(), &CancellationToken::default())
        .unwrap()
        .expect("ink");
    // Glyph 0 rotated 90 deg clockwise about (16, 30): local x -6..4, y -10..0 -> x' = -y in 0..10,
    // y' = x in -6..4 => pixels x 16..26, y 24..34. Glyph 1 shifted by the tracking (+10 px) and
    // moved up 10: x 32..42, y 10..20.
    let r0 = Rect { x: 16, y: 24, width: 10, height: 10 };
    let r1 = Rect { x: 32, y: 10, width: 10, height: 10 };
    let m0 = &content.owned_masks[&SourceKey(0)];
    let m1 = &content.owned_masks[&SourceKey(1)];
    assert_eq!((m0.bounds, m1.bounds), (r0, r1));
    assert!(m0.bytes.iter().chain(&m1.bytes).all(|v| *v == 255), "integer-aligned squares cover every pixel fully");
    assert_eq!(m0.sample_format, "coverage_u8");
    assert_eq!(content.rect, Rect { x: 16, y: 10, width: 26, height: 24 });
    assert_eq!(content.plan.nodes[0], PlanNode::Solid { rect: r0, rgba: BLUE });
    assert_eq!(content.plan.nodes[1], PlanNode::Solid { rect: r1, rgba: [1.0, 0.0, 0.0, 1.0] });
    let PlanNode::Group(group) = &content.plan.nodes[2] else { panic!("root group") };
    assert!(group.isolated);
    assert_eq!(group.layers.iter().map(|l| l.opacity).collect::<Vec<_>>(), vec![1.0, 0.5]);
    assert!(group.layers.iter().all(|l| l.mask.as_ref().is_some_and(|m| m.format == MaskFormat::CoverageU8)));

    // Hostile: a NaN transform and a count mismatch are refused before any plane is rasterised.
    let mut bad = t.clone();
    bad[1].rotation = f64::NAN;
    let cancel = CancellationToken::default();
    assert!(animated_text_content("L", &g, &bad, BLUE, SPACE, 7, &RasterOptions::default(), &cancel).is_err());
    assert!(animated_text_content("L", &g, &t[..1], BLUE, SPACE, 7, &RasterOptions::default(), &cancel).is_err());
    // A glyph moved out of the pixel budget is refused, never allocated.
    let mut far = t.clone();
    far[0].scale = [1.0e5, 1.0e5];
    let tight = RasterOptions { max_pixels: 1 << 12, ..RasterOptions::default() };
    assert!(animated_text_content("L", &g, &far, BLUE, SPACE, 7, &tight, &cancel).is_err());
}
