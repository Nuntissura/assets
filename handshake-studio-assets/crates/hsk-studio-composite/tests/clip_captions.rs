//! Lane SMB-PREMIERE-COLOR-TITLES slice 4: caption burn-in. The active segment of a caption track at
//! a tick is selected (Folio `segments_at`), its caller-shaped glyphs (synthetic 10 px squares here)
//! are laid out at the bottom or top of the frame by ink extent and painted in the segment colour;
//! every plane rectangle and colour is hand-computed.
#![cfg(feature = "motion")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::clip_finishing::{CaptionLayout, caption_burn_in};
use hsk_studio_composite::text::animate::glyph_from_outline;
use hsk_studio_composite::*;
use hsk_studio_folio::{StudioCaptionPlacement, StudioCaptionSegment, StudioCaptionStyle, StudioCaptionTrack};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{ColourTag, PlanNode, SourceKey};
use hsk_studio_type::{GlyphOutline, OutlineCommand};

const MS: u64 = 254_016_000;
const SPACE: ColourTag = ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded };

fn square() -> GlyphOutline {
    GlyphOutline::synthetic(1000, vec![
        OutlineCommand::MoveTo([0.0, 0.0]),
        OutlineCommand::LineTo([500.0, 0.0]),
        OutlineCommand::LineTo([500.0, 500.0]),
        OutlineCommand::LineTo([0.0, 500.0]),
        OutlineCommand::Close,
    ])
}

/// "AB" on line 0 and "C" on line 1: 10 px squares at pens x 0 and 12 (line 0) and 0 (line 1),
/// baseline y 0 (ink y -10..0).
fn glyphs() -> Vec<hsk_studio_composite::text::animate::AnimatedGlyph> {
    let s = square();
    vec![
        glyph_from_outline(&s, [0.0, 0.0], 0.02, 12.0, 20.0, 0).unwrap(),
        glyph_from_outline(&s, [12.0, 0.0], 0.02, 12.0, 20.0, 0).unwrap(),
        glyph_from_outline(&s, [0.0, 0.0], 0.02, 12.0, 20.0, 1).unwrap(),
    ]
}

#[test]
fn active_caption_burns_in_at_the_hand_computed_place_and_colour() {
    let cancel = CancellationToken::default();
    let track = StudioCaptionTrack {
        track_id: "STRK-019abcde-0000-7000-8000-000000000003".into(),
        language: "en".into(),
        active: true,
        constraints: Default::default(),
        segments: vec![
            StudioCaptionSegment { start: 0, end: 1000 * MS, text: "AB\nC".into(), style: StudioCaptionStyle { colour: Some([1.0, 0.5, 0.0, 1.0]), ..Default::default() } },
            StudioCaptionSegment { start: 2000 * MS, end: 3000 * MS, text: "AB\nC".into(), style: StudioCaptionStyle { placement: StudioCaptionPlacement::Top, ..Default::default() } },
        ],
    };
    assert_eq!(track.segments_at(500 * MS).count(), 1);
    assert_eq!(track.segments_at(1500 * MS).count(), 0, "between segments nothing burns in");
    assert_eq!(track.segments_at(1000 * MS).count(), 0, "end is exclusive");
    let layout = CaptionLayout { width: 64.0, height: 32.0, margin_px: 2.0, line_height_px: 12.0 };

    // Bottom, centred: line 1 baseline 30, line 0 baseline 18. Line 0 ink x 0..22 -> dx 21; line 1
    // ink 0..10 -> dx 27.
    let seg = track.segments_at(500 * MS).next().unwrap();
    let layer = caption_burn_in("captions", seg, &glyphs(), &layout, SPACE, 7, &cancel).unwrap().unwrap();
    let SceneContent::Static(c) = &layer.content else { panic!("{:?}", layer.content) };
    let rects: Vec<Rect> = (0..3).map(|k| c.owned_masks[&SourceKey(k)].bounds).collect();
    assert_eq!(rects, vec![
        Rect { x: 21, y: 8, width: 10, height: 10 },
        Rect { x: 33, y: 8, width: 10, height: 10 },
        Rect { x: 27, y: 20, width: 10, height: 10 },
    ]);
    assert!(matches!(c.plan.nodes[0], PlanNode::Solid { rgba, .. } if rgba == [1.0, 0.5, 0.0, 1.0]));

    // Top placement, white: line 0 baseline 2 + 12 = 14, line 1 baseline 26.
    let seg = track.segments_at(2500 * MS).next().unwrap();
    let layer = caption_burn_in("captions", seg, &glyphs(), &layout, SPACE, 7, &cancel).unwrap().unwrap();
    let SceneContent::Static(c) = &layer.content else { panic!() };
    assert_eq!(c.owned_masks[&SourceKey(0)].bounds, Rect { x: 21, y: 4, width: 10, height: 10 });
    assert_eq!(c.owned_masks[&SourceKey(2)].bounds, Rect { x: 27, y: 16, width: 10, height: 10 });
    assert!(matches!(c.plan.nodes[2], PlanNode::Solid { rgba, .. } if rgba == [1.0; 4]));

    // Hostile layout values are refused; an empty glyph list burns nothing in.
    let bad = CaptionLayout { line_height_px: f64::NAN, ..layout };
    assert!(caption_burn_in("captions", seg, &glyphs(), &bad, SPACE, 7, &cancel).is_err());
    assert!(caption_burn_in("captions", seg, &[], &layout, SPACE, 7, &cancel).unwrap().is_none());
}
