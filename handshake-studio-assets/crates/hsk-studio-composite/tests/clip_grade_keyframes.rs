//! Lane SMB-PR-SOURCE-MONITOR slice 3: keyframed grade scalars sampled at the clip tick through
//! motion's keyframe evaluator, faded film (stage 3) and sharpen (the spatial stage after the
//! grade), against hand values. Lumetri parity of the faded-film and sharpen readings UNVERIFIED (D5).
#![cfg(feature = "motion")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::clip_finishing::{
    ClipEffectFrame, ClipGrade, ClipStage, NoLuts, apply_effects, evaluate_effect_stack,
};
use hsk_studio_composite::{SceneContent, SceneLayer, StudioBlendMode};
use hsk_studio_folio::{
    StudioClipEffect, StudioClipEffectParams, StudioClipEffectStack, StudioColorGrade, StudioCompositionKeyframe,
    StudioGradeBasic, StudioGradeKeyedParam, StudioGradeParam, StudioGradeVignette, StudioInterpolation, StudioKeyedValue,
};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::transform_math::Affine2;

const F: u64 = 10_584_000_000;
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];
const CLIP: &str = "SCLP-019abcde-0000-7000-8000-000000000005";

fn keys(k: &[(u64, f64, StudioInterpolation)]) -> StudioKeyedValue {
    StudioKeyedValue {
        static_value: vec![k[0].1],
        keyframes: k
            .iter()
            .map(|&(time, v, i)| StudioCompositionKeyframe { time, value: vec![v], in_interp: i, out_interp: i, in_tangent: vec![], out_tangent: vec![] })
            .collect(),
    }
}
fn stack(g: StudioColorGrade) -> StudioClipEffectStack {
    StudioClipEffectStack {
        clip_id: CLIP.into(),
        effects: vec![StudioClipEffect { effect_id: "grade".into(), enabled: true, params: StudioClipEffectParams::ColorGrade(Box::new(g)) }],
    }
}
fn grade_at(g: &StudioColorGrade, tick: i64) -> std::sync::Arc<ClipGrade> {
    let frames = evaluate_effect_stack(&stack(g.clone()), tick, &NoLuts, LUMA, &CancellationToken::default()).unwrap();
    match &frames[0] {
        ClipEffectFrame::Grade(g) => g.clone(),
        other => panic!("{other:?}"),
    }
}
fn close(got: [f32; 3], want: [f32; 3]) {
    assert!(got.iter().zip(want).all(|(g, w)| (g - w).abs() <= 2e-6), "{got:?} vs {want:?}");
}
fn layer() -> SceneLayer {
    SceneLayer {
        layer_id: "SLYR-019abcde-0000-7000-8000-000000000001".into(),
        content: SceneContent::None,
        mode: StudioBlendMode::Normal,
        opacity: 1.0,
        fill_opacity: 1.0,
        matrix: Affine2::IDENTITY,
        matte: None,
        clipped: false,
    }
}

#[test]
fn keyframed_grade_faded_film_and_sharpen_follow_the_clip_tick() {
    use StudioInterpolation::{Hold, Linear};
    let grey = [0.25f32; 3];
    // Exposure keyed linearly 0 -> 1 stop over 10 frames, saturation held at 0 from frame 4.
    let g = StudioColorGrade {
        basic: StudioGradeBasic { exposure: 3.0, ..StudioGradeBasic::default() },
        keyframes: vec![
            StudioGradeKeyedParam { param: StudioGradeParam::Exposure, value: keys(&[(0, 0.0, Linear), (10 * F, 1.0, Linear)]) },
            StudioGradeKeyedParam { param: StudioGradeParam::Saturation, value: keys(&[(0, 100.0, Hold), (4 * F, 0.0, Hold)]) },
        ],
        ..StudioColorGrade::default()
    };
    // Clip tick 5F: exposure 0.5 stop (the static 3 is not used under keys), saturation still 100.
    let at5 = grade_at(&g, 5 * F as i64);
    let k = 2f32.powf(0.5);
    close(at5.apply(grey, 0.5, 0.5, 1.0, 1.0), [0.25 * k; 3]);
    let colour = [0.5f32, 0.25, 0.125];
    let y = colour[0] * LUMA[0] + colour[1] * LUMA[1] + colour[2] * LUMA[2];
    let k1 = 2f32.powf(0.1);
    // Clip tick F: exposure 0.1 stop, saturation 100 (before the hold key at 4F).
    close(grade_at(&g, F as i64).apply(colour, 0.5, 0.5, 1.0, 1.0), colour.map(|c| c * k1));
    // Past the last key: exposure 1 stop, saturation 0 = the luma of the exposed colour.
    let late = grade_at(&g, 20 * F as i64).apply(colour, 0.5, 0.5, 1.0, 1.0);
    close(late, [2.0 * y; 3]);
    // Before the clip (negative clip tick) the first key holds: exposure 0.
    close(grade_at(&g, -(3 * F as i64)).apply(colour, 0.5, 0.5, 1.0, 1.0), colour);

    // Faded film 100 -> floor 0.1: v + 0.1 (1 - v)^2.
    let faded = StudioColorGrade { faded_film: 100.0, ..StudioColorGrade::default() };
    let fg = grade_at(&faded, 0);
    assert!((fg.faded_floor() - 0.1).abs() < 1e-7);
    close(fg.apply([0.0, 0.5, 1.0], 0.5, 0.5, 1.0, 1.0), [0.1, 0.5 + 0.1 * 0.25, 1.0]);
    // Keyed faded film: 150 at 0 down to 0 at 2F; half way = 75 -> floor 0.075.
    let keyed = StudioColorGrade {
        keyframes: vec![StudioGradeKeyedParam { param: StudioGradeParam::FadedFilm, value: keys(&[(0, 150.0, Linear), (2 * F, 0.0, Linear)]) }],
        ..StudioColorGrade::default()
    };
    assert!((grade_at(&keyed, F as i64).faded_floor() - 0.075).abs() < 1e-7);
    // Keyed vignette amount reaches the prepared grade (vignette gain at the corner falls).
    let vig = StudioColorGrade {
        vignette: Some(StudioGradeVignette { amount: 0.0, midpoint: 50.0, roundness: 0.0, feather: 0.0 }),
        keyframes: vec![StudioGradeKeyedParam { param: StudioGradeParam::VignetteAmount, value: keys(&[(0, 0.0, Linear), (F, -2.0, Linear)]) }],
        ..StudioColorGrade::default()
    };
    let corner = |t: i64| grade_at(&vig, t).apply([0.5; 3], 0.5, 0.5, 100.0, 100.0)[0];
    assert!((corner(0) - 0.5).abs() < 1e-7, "amount 0 at tick 0");
    assert!(corner(F as i64) < 0.5, "amount -2 darkens the corner");

    // Sharpen: the grade is wrapped, then the spatial stage (unsharp mask at +, Gaussian at -).
    let rect = Rect { x: 0, y: 0, width: 8, height: 8 };
    for (s, want) in [(60.0, "unsharp"), (-50.0, "gaussian"), (0.0, "none")] {
        let g = StudioColorGrade { basic: StudioGradeBasic { sharpen: s, ..StudioGradeBasic::default() }, ..StudioColorGrade::default() };
        let frames = evaluate_effect_stack(&stack(g), 0, &NoLuts, LUMA, &CancellationToken::default()).unwrap();
        let out = apply_effects(layer(), rect, &frames).unwrap();
        match (&out.content, want) {
            (SceneContent::Filtered(f), _) => {
                assert!(matches!(&f.content, SceneContent::Graded(g) if matches!(g.stage, ClipStage::Grade(_))));
                let expect = match want {
                    "unsharp" => hsk_studio_composite::clip_finishing::filters(CLIP, hsk_studio_folio::StudioSmartFilterKind::UnsharpMask { amount_pct: 60.0, radius_px: 1.0, threshold_levels: 0 }).unwrap(),
                    "gaussian" => hsk_studio_composite::clip_finishing::filters(CLIP, hsk_studio_folio::StudioSmartFilterKind::GaussianBlur { radius_px: 0.5 }).unwrap(),
                    _ => panic!("sharpen 0 must not add a stage"),
                };
                assert_eq!(format!("{:?}", f.filters), format!("{expect:?}"));
            }
            (SceneContent::Graded(_), "none") => {}
            (other, _) => panic!("{s}: {other:?}"),
        }
    }
}
