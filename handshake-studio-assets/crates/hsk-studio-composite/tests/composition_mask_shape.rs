//! Lane SMB-AE-MASK-FEATHER-1: Folio mask feather, opacity and expansion streams evaluated by
//! `CompositionTemporalSource` (feature `motion`). Each stream is sampled per tick with the same
//! keyframe conversion as the path and handed to `mask_paths::mask_coverage_shaped`, for the
//! layer's composition-space matte and for the coverage folded into an effect stack's content
//! (STU-CMP-010 step 2). The CPU pixel proof is render-cpu `tests/composition_mask_shape.rs`.
#![cfg(feature = "motion")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::mask_paths::{
    COMPOSITION_MASK_FEATHER_OUT_OF_RANGE, MaskAtTick, MaskShape, StudioCompositionMaskMode, mask_coverage_shaped,
};
use hsk_studio_composite::temporal::composition::{CompositionInput, CompositionTemporalSource};
use hsk_studio_composite::*;
use hsk_studio_folio::{Budget, StudioCompositionRecords, TileRef};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{AlphaConvention, SampleFormat};
use hsk_studio_pigment::transform_math::Affine2;
use serde_json::{Value, json};

mod common;
use common::{Available, bounded_case, layer, open, tile};
#[path = "support/effect_scenes.rs"]
mod scenes;
use scenes::{H, Pixels, W, identity};

struct Info<'a>(&'a Pixels);
impl TileInfoSource for Info<'_> {
    fn tile_info(&self, t: &TileRef) -> Option<TileInfo> {
        (format!("{}/{}", t.layer_id, t.object_key) == scenes::tile_id()).then_some(TileInfo {
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: self.0.colour_sha256,
            alpha_sha256: self.0.alpha_sha256,
        })
    }
}

/// 24 fps.
const TICKS_PER_FRAME: u64 = 10_584_000_000;
fn f(frame: u64) -> u64 {
    frame * TICKS_PER_FRAME
}
/// Document: raster layer 2 (the 16x12 tile of `effect_scenes`).
fn document() -> Value {
    let mut v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"px",
        "colour_mode":3,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,
        "working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[layer(2, "raster", json!({"payload_kind":"raster","tiles":[tile(2,"src",0,0,W,H)]}))],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    });
    common::assign_all_profiles(&mut v);
    v
}
/// One 16x12 composition (4 frames) holding layer 2 with `extra` merged into its record.
fn records(extra: Value) -> Value {
    let mut l = json!({"layer_id":identity("SLYR",2),
        "timing":{"in_point":0,"out_point":f(4),"start_time":0,"stretch":{"numerator":1,"denominator":1}}});
    for (k, v) in extra.as_object().cloned().unwrap_or_default() {
        l[k] = v;
    }
    json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),"compositions":[
        {"schema_id":"hsk.studio.composition@1","composition_id":identity("SCOM",1),"name":"main","width":W,"height":H,
         "pixel_aspect":{"numerator":1,"denominator":1},"frame_rate":{"numerator":24,"denominator":1},"duration":f(4),
         "layers":[l]}]})
}
fn source(rec: &Value, pixels: &Pixels) -> Result<CompositionTemporalSource, TemporalError> {
    let snap = open(&document());
    let records: StudioCompositionRecords = serde_json::from_value(rec.clone()).expect("records shape");
    let input = CompositionInput {
        snapshot: &snap,
        expected_revision: 7,
        records: &records,
        composition_id: &identity("SCOM", 1),
        budget: Budget::default(),
        resolver: &Available,
        blend_space: BlendSpace::Encoded,
        info: &Info(pixels),
        space: scenes::space(),
        inputs: RenderInputs::default(),
    };
    CompositionTemporalSource::new(&input, &CancellationToken::default())
}
const RECT: [f64; 4] = [3.0, 2.0, 12.0, 9.0];
fn rect_path() -> Vec<f64> {
    let [x0, y0, x1, y1] = RECT;
    vec![x0, y0, 0.0, 0.0, 0.0, 0.0, x1, y0, 0.0, 0.0, 0.0, 0.0, x1, y1, 0.0, 0.0, 0.0, 0.0, x0, y1, 0.0, 0.0, 0.0, 0.0]
}
/// An `add` mask over [`RECT`] with `streams` (feather / opacity / expansion members) merged in.
fn mask(streams: Value) -> Value {
    let mut m = json!({"mask_id":"soft","mode":"add","closed":true,"static_value":rect_path()});
    for (k, v) in streams.as_object().cloned().unwrap_or_default() {
        m[k] = v;
    }
    m
}
fn ramp(v0: Value, v1: Value) -> Value {
    json!({"static_value":v0.clone(),"keyframes":[
        {"time":0,"value":v0,"in_interp":"linear","out_interp":"linear"},
        {"time":f(2),"value":v1,"in_interp":"linear","out_interp":"linear"}]})
}
fn constant(v: Value) -> Value {
    json!({ "static_value": v })
}
/// The layer's scene at `frame` with `extra` merged into its record.
fn scene(extra: Value, frame: u64, pixels: &Pixels) -> Result<SceneComposition, TemporalError> {
    source(&records(extra), pixels)?.scene_at(f(frame) as i64, &CancellationToken::default())
}
/// The composition-space matte of a layer carrying one mask with `streams`, at `frame`.
fn matte(streams: Value, frame: u64, pixels: &Pixels) -> SceneMatte {
    scene(json!({"masks":[mask(streams)]}), frame, pixels).expect("scene").layers[0].matte.clone().expect("mask matte")
}

#[test]
fn animated_feather_matches_the_static_evaluation_at_each_tick() {
    bounded_case("animated_feather_matches_the_static_evaluation_at_each_tick", || {
        let pixels = Pixels::fixture();
        let animated = json!({"feather": ramp(json!([0.0, 0.0]), json!([8.0, 8.0]))});
        let at: Vec<SceneMatte> = (0..3).map(|k| matte(animated.clone(), k, &pixels)).collect();
        for (k, v) in [(0u64, 0.0), (1, 4.0), (2, 8.0)] {
            assert_eq!(at[k as usize], matte(json!({"feather": constant(json!([v, v]))}), k, &pixels), "frame {k}: feather {v}");
        }
        assert_ne!(at[0], at[2], "feather 0 and 8 px give different mattes");
        assert_ne!(at[1], at[2]);
        // Feather 0 is the identity: the same matte as a mask without the stream.
        assert_eq!(at[0], matte(json!({}), 0, &pixels));
        // Past the last key the value holds.
        assert_eq!(matte(animated, 3, &pixels), matte(json!({"feather": constant(json!([8.0, 8.0]))}), 3, &pixels));
    });
}

#[test]
fn animated_opacity_and_expansion_match_the_static_evaluation_at_each_tick() {
    bounded_case("animated_opacity_and_expansion_match_the_static_evaluation_at_each_tick", || {
        let pixels = Pixels::fixture();
        let opacity = json!({"opacity": ramp(json!([0.25]), json!([0.75]))});
        let expansion = json!({"expansion": ramp(json!([-2.0]), json!([4.0]))});
        for (k, o, e) in [(0u64, 0.25, -2.0), (1, 0.5, 1.0), (2, 0.75, 4.0)] {
            assert_eq!(matte(opacity.clone(), k, &pixels), matte(json!({"opacity": constant(json!([o]))}), k, &pixels), "frame {k}: opacity {o}");
            assert_eq!(matte(expansion.clone(), k, &pixels), matte(json!({"expansion": constant(json!([e]))}), k, &pixels), "frame {k}: expansion {e}");
        }
        assert_ne!(matte(opacity.clone(), 0, &pixels), matte(opacity, 2, &pixels));
        assert_ne!(matte(expansion.clone(), 0, &pixels), matte(expansion, 2, &pixels));
        // All three together, keyed, still equal their static evaluation.
        let all = json!({"feather": ramp(json!([0.0, 2.0]), json!([6.0, 0.0])), "opacity": ramp(json!([0.25]), json!([0.75])),
            "expansion": ramp(json!([-2.0]), json!([4.0]))});
        let fixed = json!({"feather": constant(json!([3.0, 1.0])), "opacity": constant(json!([0.5])), "expansion": constant(json!([1.0]))});
        assert_eq!(matte(all, 1, &pixels), matte(fixed, 1, &pixels));
    });
}

#[test]
fn an_effect_stack_folds_the_shaped_mask_and_refusals_name_the_mask() {
    bounded_case("an_effect_stack_folds_the_shaped_mask_and_refusals_name_the_mask", || {
        let pixels = Pixels::fixture();
        let offset = json!({"effect_id":"o","filter_kind":"offset","values":{"shift_center_to":constant(json!([0.6875,0.75]))}});
        let streams = json!({"feather": ramp(json!([0.0, 0.0]), json!([4.0, 2.0])), "opacity": constant(json!([0.5])),
            "expansion": constant(json!([1.0]))});
        let s = scene(json!({"masks":[mask(streams)], "effects":[offset]}), 1, &pixels).expect("scene");
        assert!(s.layers[0].matte.is_none(), "masks are folded into the effected content, not a matte");
        let SceneContent::Effected(e) = &s.layers[0].content else { panic!("not effected: {:?}", s.layers[0].content) };
        // The content rect is the tile at (0, 0): layer space = plane pixels.
        let rect = Rect { x: 0, y: 0, width: W, height: H };
        let shape = MaskShape { feather_px: [2.0, 1.0], opacity: 0.5, expansion_px: 1.0 };
        let at = MaskAtTick { mode: StudioCompositionMaskMode::Add, inverted: false, closed: true, path: rect_path() };
        let want = mask_coverage_shaped(&[(at.clone(), shape)], &Affine2::IDENTITY, rect, 1 << 20).unwrap();
        assert_eq!(e.mask, want, "the folded coverage is the shaped coverage at frame 1");
        let unshaped = mask_coverage_shaped(&[(at, MaskShape::default())], &Affine2::IDENTITY, rect, 1 << 20).unwrap();
        assert_ne!(e.mask, unshaped);
        let peak = e.mask.as_ref().unwrap().iter().fold(0.0f32, |a, b| a.max(*b));
        assert!(peak <= 0.5 && peak > 0.49, "opacity 0.5 bounds the folded mask: {peak}");
        // A sampled feather beyond pigment's support limit is refused typed, naming the mask.
        let wide = json!({"masks":[mask(json!({"feather": constant(json!([20000.0, 0.0]))}))]});
        match scene(wide, 0, &pixels) {
            Err(TemporalError::Unsupported { layer_id, feature }) => {
                assert_eq!((layer_id.as_str(), feature), (format!("{}/masks/soft", identity("SLYR", 2)).as_str(), COMPOSITION_MASK_FEATHER_OUT_OF_RANGE));
            }
            other => panic!("{other:?}"),
        }
    });
}
