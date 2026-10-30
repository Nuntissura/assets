//! Lane SMB-AE-EFFECT-FAMILIES-2 (parity AE-17): blur, glow and colour families in the per-layer
//! effect stack and the entry blend mode / effect mask (STU-FX-004, STU-FX-008, STU-FX-131).
//! Goldens are hand values on small fixtures (an impulse, a flat field, the 16x12 stepped-alpha
//! source of `tests/support/effect_scenes.rs`); Folio records lower through
//! `CompositionTemporalSource` and the lowered `SceneEffect`s are compared with the kernels'
//! inputs. After Effects' own maths is UNVERIFIED (D5): every expectation is the declared Studio
//! reading (`composition::effect_stack` and `temporal::composition::effects` module docs).
#![cfg(feature = "motion")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::composition::{
    ChannelAdjustment, ColourEffect, EffectError, EffectedContent, EntryComposite, GeneratorComposite, GlowColours, GlowEffect, GlowLooping,
    GlowOriginal, SceneEffect, SceneEffectEntry, apply_effects, box_taps,
};
use hsk_studio_composite::temporal::composition::{
    COMPOSITION_EFFECT_KERNEL, COMPOSITION_EFFECT_OPTION, CompositionInput, CompositionTemporalSource,
};
use hsk_studio_composite::*;
use hsk_studio_folio::{Budget, StudioCompositionRecords, TileRef};
use hsk_studio_pigment::adjust_math::{
    AdjustmentOp, ColourBalance, Exposure, GradientMap, GradientMapStop, HueSaturation, Levels, LevelsRecord,
};
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_pigment::blur_math::{BlurScope, LayerBlur};
use hsk_studio_pigment::render_plan::{AlphaConvention, SampleFormat};
use serde_json::{Value, json};

mod common;
use common::{Available, bounded_case, layer, open, tile};
#[path = "support/effect_scenes.rs"]
mod scenes;
use scenes::{H, Pixels, W, entry, identity, premultiplied_source, straight};

fn run_on(w: u32, h: u32, mut buf: Vec<[f32; 4]>, effects: &[SceneEffectEntry]) -> Vec<[f32; 4]> {
    apply_effects(w, h, &mut buf, effects, &CancellationToken::default()).expect("stack runs");
    buf
}
fn run(effects: &[SceneEffectEntry]) -> Vec<[f32; 4]> {
    run_on(W, H, premultiplied_source(), effects)
}
fn gaussian(radius: f32) -> Vec<f32> {
    LayerBlur::new(BlurScope::Layer, radius).taps().expect("pigment taps")
}
/// A `w x h` transparent buffer with one opaque white texel at `(cx, cy)` (premultiplied).
fn impulse(w: u32, h: u32, cx: u32, cy: u32) -> Vec<[f32; 4]> {
    (0..w * h).map(|i| if i == cy * w + cx { [1.0; 4] } else { [0.0; 4] }).collect()
}
fn flat(w: u32, h: u32, straight: [f32; 4]) -> Vec<[f32; 4]> {
    let a = straight[3];
    vec![[straight[0] * a, straight[1] * a, straight[2] * a, a]; (w * h) as usize]
}
fn close(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}
fn colour_entry(ops: Vec<ChannelAdjustment>, blend: f64) -> SceneEffectEntry {
    entry("c", SceneEffect::Colour(Box::new(ColourEffect { ops, blend })))
}
fn all(op: AdjustmentOp) -> ChannelAdjustment {
    ChannelAdjustment { op, channels: [true; 3] }
}
fn glow(threshold: f64, original: GlowOriginal, mode: StudioBlendMode, colours: Option<GlowColours>) -> SceneEffectEntry {
    entry(
        "g",
        SceneEffect::Glow(Box::new(GlowEffect {
            based_on_alpha: false,
            threshold,
            taps: gaussian(2.0),
            intensity: 1.0,
            original,
            mode,
            colours,
        })),
    )
}

// ---- Folio records -> lowered stack (as tests/composition_effects.rs) ----

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
fn constant(v: Value) -> Value {
    json!({ "static_value": v })
}
/// The layer's lowered effect stack at `frame` for records `extra`.
fn lowered(extra: Value, frame: u64) -> Result<EffectedContent, TemporalError> {
    let pixels = Pixels::fixture();
    let snap = open(&document());
    let records: StudioCompositionRecords = serde_json::from_value(records(extra)).expect("records shape");
    let input = CompositionInput {
        snapshot: &snap,
        expected_revision: 7,
        records: &records,
        composition_id: &identity("SCOM", 1),
        budget: Budget::default(),
        resolver: &Available,
        blend_space: BlendSpace::Encoded,
        info: &Info(&pixels),
        space: scenes::space(),
        inputs: RenderInputs::default(),
    };
    let cancel = CancellationToken::default();
    let scene = CompositionTemporalSource::new(&input, &cancel)?.scene_at(f(frame) as i64, &cancel)?;
    match &scene.layers[0].content {
        SceneContent::Effected(e) => Ok((**e).clone()),
        other => panic!("not effected: {other:?}"),
    }
}
fn one(e: Value) -> Value {
    json!({ "effects": [e] })
}
fn lowered_one(e: Value) -> SceneEffect {
    lowered(one(e), 0).expect("lowers").effects[0].effect.clone()
}
/// The refusal of records `extra`: an adapter feature or a Folio document target.
fn refusal(extra: Value) -> String {
    match lowered(extra, 0) {
        Err(TemporalError::Unsupported { feature, .. }) => feature.to_owned(),
        Err(TemporalError::Document(t)) => t,
        other => panic!("not refused: {other:?}"),
    }
}

#[test]
fn blur_family_runs_pigment_kernels_with_edges_and_dimensions() {
    bounded_case("blur_family_runs_pigment_kernels_with_edges_and_dimensions", || {
        // Gaussian on an impulse: the texel (4 + dx, 4 + dy) is taps[dx] taps[dy] (pigment's taps).
        let taps = gaussian(2.0);
        assert_eq!(taps.len(), 7);
        let out = run_on(9, 9, impulse(9, 9, 4, 4), &[entry("b", SceneEffect::Blur { taps: taps.clone(), repeat_edges: false })]);
        let mut sum = 0.0;
        for y in 0..9u32 {
            for x in 0..9u32 {
                let (dx, dy) = (x as i64 - 4, y as i64 - 4);
                let want = if dx.abs() <= 3 && dy.abs() <= 3 { taps[(dx + 3) as usize] * taps[(dy + 3) as usize] } else { 0.0 };
                let p = out[(y * 9 + x) as usize];
                assert!(close(p[3], want, 1e-6), "({x}, {y}) {} vs {want}", p[3]);
                assert!(p[..3].iter().all(|c| close(*c, p[3], 1e-6)), "white stays white: {p:?}");
                sum += p[3];
            }
        }
        assert!(close(sum, 1.0, 1e-5), "{sum}");
        // Repeat Edge Pixels: a flat field stays flat; without it the corner sees transparency
        // beyond the rect: alpha = (centre tap + one side)^2.
        let field = [0.25, 0.5, 0.75, 1.0];
        let rep = run_on(9, 7, flat(9, 7, field), &[entry("b", SceneEffect::Blur { taps: taps.clone(), repeat_edges: true })]);
        assert!(rep.iter().all(|p| p.iter().zip(field).all(|(a, b)| close(*a, b, 1e-6))), "{:?}", rep[0]);
        let cut = run_on(9, 7, flat(9, 7, field), &[entry("b", SceneEffect::Blur { taps: taps.clone(), repeat_edges: false })]);
        let half: f32 = taps[3..].iter().sum();
        assert!(close(cut[0][3], half * half, 1e-6), "{} vs {}", cut[0][3], half * half);
        // Fast Box Blur taps: the iterated box, normalised.
        let b = box_taps(1, 2).unwrap();
        let want = [1.0, 2.0, 3.0, 2.0, 1.0].map(|v: f32| v / 9.0);
        assert!(b.len() == 5 && b.iter().zip(want).all(|(a, w)| close(*a, w, 1e-7)), "{b:?}");
        assert_eq!(box_taps(2, 1).unwrap().len(), 5);
        assert!(box_taps(2, 1).unwrap().iter().all(|t| close(*t, 0.2, 1e-7)));
        assert_eq!(box_taps(0, 3), Some(vec![1.0]));
        assert_eq!(box_taps(1000, 50), None, "more than MAX_EFFECT_TAPS");
        let boxed = run_on(9, 9, impulse(9, 9, 4, 4), &[entry("b", SceneEffect::Blur { taps: box_taps(1, 1).unwrap(), repeat_edges: false })]);
        assert!(close(boxed[3 * 9 + 5][3], 1.0 / 9.0, 1e-6) && boxed[2 * 9 + 4][3] == 0.0);
        // Folio rows lower onto those kernels: Gaussian Blurriness = pigment radius, default repeat on.
        let g = lowered_one(json!({"effect_id":"g","filter_kind":"gaussian_blur","values":{"blurriness":constant(json!([2.0]))}}));
        assert_eq!(g, SceneEffect::Blur { taps: taps.clone(), repeat_edges: true });
        let g = lowered_one(json!({"effect_id":"g","filter_kind":"gaussian_blur","values":{
            "blurriness":constant(json!([2.0])),"repeat_edge_pixels":constant(json!([0.0]))}}));
        assert_eq!(g, SceneEffect::Blur { taps, repeat_edges: false });
        // Fast Box: radius 1.4 rounds to 1, iterations 2.5 to 3.
        let fb = lowered_one(json!({"effect_id":"f","filter_kind":"fast_box_blur","values":{
            "blur_radius":constant(json!([1.4])),"iterations":constant(json!([2.5]))}}));
        assert_eq!(fb, SceneEffect::Blur { taps: box_taps(1, 3).unwrap(), repeat_edges: true });
        // No one-axis or directional / radial kernel in pigment: refused typed, never approximated.
        let k = |e: Value| refusal(one(e));
        assert_eq!(k(json!({"effect_id":"g","filter_kind":"gaussian_blur","values":{"blur_dimensions":constant(json!([2.0]))}})), COMPOSITION_EFFECT_KERNEL);
        assert_eq!(k(json!({"effect_id":"f","filter_kind":"fast_box_blur","values":{"blur_dimensions":constant(json!([3.0]))}})), COMPOSITION_EFFECT_KERNEL);
        assert_eq!(k(json!({"effect_id":"g","filter_kind":"gaussian_blur","values":{"blurriness":constant(json!([1500.0]))}})), COMPOSITION_EFFECT_KERNEL);
        assert_eq!(k(json!({"effect_id":"d","filter_kind":"directional_blur","values":{"blur_length":constant(json!([5.0]))}})), COMPOSITION_EFFECT_KERNEL);
        assert_eq!(k(json!({"effect_id":"r","filter_kind":"radial_blur"})), COMPOSITION_EFFECT_KERNEL);
        assert_eq!(k(json!({"effect_id":"e","filter_kind":"find_edges"})), COMPOSITION_EFFECT_KERNEL);
        // A Folio hard bound still refuses first (STU-FX-104).
        assert!(k(json!({"effect_id":"g","filter_kind":"gaussian_blur","values":{"blurriness":constant(json!([30001.0]))}}))
            .contains("effect_param_out_of_range"));
    });
}

#[test]
fn blur_radius_zero_is_identity() {
    bounded_case("blur_radius_zero_is_identity", || {
        let src = premultiplied_source();
        for repeat_edges in [false, true] {
            assert_eq!(run(&[entry("b", SceneEffect::Blur { taps: vec![1.0], repeat_edges })]), src, "bit for bit");
        }
        assert_eq!(gaussian(0.0), vec![1.0]);
        let identity = SceneEffect::Blur { taps: vec![1.0], repeat_edges: true };
        assert_eq!(lowered_one(json!({"effect_id":"g","filter_kind":"gaussian_blur"})), identity);
        assert_eq!(lowered_one(json!({"effect_id":"f","filter_kind":"fast_box_blur"})), identity);
        let none = SceneEffect::Blur { taps: vec![1.0], repeat_edges: false };
        assert_eq!(lowered_one(json!({"effect_id":"d","filter_kind":"directional_blur","values":{"direction":constant(json!([30.0]))}})), none);
        assert_eq!(lowered_one(json!({"effect_id":"r","filter_kind":"radial_blur","values":{"amount":constant(json!([0.0]))}})), none);
        assert_eq!(lowered_one(json!({"effect_id":"e","filter_kind":"find_edges","values":{"blend_with_original":constant(json!([1.0]))}})), none);
        // Hostile kernels are refused before any buffer: even taps, negative or non-finite taps.
        let mut b = premultiplied_source();
        let cancel = CancellationToken::default();
        for taps in [vec![0.5, 0.5], vec![-1.0, 3.0, -1.0], vec![f32::NAN], vec![0.0]] {
            assert_eq!(
                apply_effects(W, H, &mut b, &[entry("b", SceneEffect::Blur { taps, repeat_edges: false })], &cancel),
                Err(EffectError::Parameter(0))
            );
        }
    });
}

#[test]
fn animated_effect_parameters_evaluate_per_frame() {
    bounded_case("animated_effect_parameters_evaluate_per_frame", || {
        // Blurriness 0 -> 8 over frames 0..2 (STU-FX-011b): the taps follow the sampled radius.
        let blur = json!({"effect_id":"g","filter_kind":"gaussian_blur","values":{"blurriness":{"static_value":[0.0],"keyframes":[
            {"time":0,"value":[0.0],"in_interp":"linear","out_interp":"linear"},
            {"time":f(2),"value":[8.0],"in_interp":"linear","out_interp":"linear"}]}}});
        let taps_at = |frame: u64| match lowered(one(blur.clone()), frame).unwrap().effects[0].effect.clone() {
            SceneEffect::Blur { taps, repeat_edges: true } => taps,
            other => panic!("{other:?}"),
        };
        assert_eq!(taps_at(0), vec![1.0]);
        for (frame, radius) in [(1, 4.0), (2, 8.0)] {
            let (got, want) = (taps_at(frame), gaussian(radius));
            assert!(got.len() == want.len() && got.iter().zip(&want).all(|(a, b)| close(*a, *b, 1e-6)), "frame {frame}");
        }
        // A colour parameter animates the same way: Tint amount 0 -> 100.
        let tint = json!({"effect_id":"t","filter_kind":"tint","values":{"amount_to_tint":{"static_value":[0.0],"keyframes":[
            {"time":0,"value":[0.0],"in_interp":"linear","out_interp":"linear"},
            {"time":f(2),"value":[100.0],"in_interp":"linear","out_interp":"linear"}]}}});
        let blend_at = |frame: u64| match lowered(one(tint.clone()), frame).unwrap().effects[0].effect.clone() {
            SceneEffect::Colour(c) => c.blend,
            other => panic!("{other:?}"),
        };
        assert!(close(blend_at(0) as f32, 1.0, 1e-9) && close(blend_at(1) as f32, 0.5, 1e-6) && close(blend_at(2) as f32, 0.0, 1e-9));
    });
}

#[test]
fn glow_builds_from_threshold_blur_and_blend() {
    bounded_case("glow_builds_from_threshold_blur_and_blend", || {
        let taps = gaussian(2.0);
        let at = |buf: &[[f32; 4]], x: u32, y: u32| buf[(y * 9 + x) as usize];
        let t = taps[4] * taps[3];
        // Impulse on transparency, behind, normal: the glow is the blurred bright pass.
        let out = straight(&run_on(9, 9, impulse(9, 9, 4, 4), &[glow(0.5, GlowOriginal::Behind, StudioBlendMode::Normal, None)]));
        let p = at(&out, 5, 4);
        assert!(close(p[3], t, 1e-6) && p[..3].iter().all(|c| close(*c, 1.0, 1e-5)), "{p:?} vs {t}");
        assert!(at(&out, 4, 4).iter().all(|c| close(*c, 1.0, 1e-6)));
        // Opaque grey 0.25 (below the threshold) with a white centre, Add (the default operation):
        // co = t + (1 - t) 0.25 over an opaque result.
        let mut grey = flat(9, 9, [0.25, 0.25, 0.25, 1.0]);
        grey[4 * 9 + 4] = [1.0; 4];
        let out = straight(&run_on(9, 9, grey.clone(), &[glow(0.6, GlowOriginal::Behind, StudioBlendMode::LinearDodgeAdd, None)]));
        let want = t + (1.0 - t) * 0.25;
        let p = at(&out, 5, 4);
        assert!(p[..3].iter().all(|c| close(*c, want, 1e-5)) && close(p[3], 1.0, 1e-6), "{p:?} vs {want}");
        // Threshold 100 % (255) passes nothing: the entry is the identity bit for bit; so is intensity 0.
        assert_eq!(run_on(9, 9, grey.clone(), &[glow(1.0, GlowOriginal::Behind, StudioBlendMode::LinearDodgeAdd, None)]), grey);
        let mut dark = glow(0.6, GlowOriginal::Behind, StudioBlendMode::LinearDodgeAdd, None);
        if let SceneEffect::Glow(g) = &mut dark.effect {
            g.intensity = 0.0;
        }
        assert_eq!(run_on(9, 9, grey.clone(), &[dark]), grey);
        // On Top over an opaque layer: the original covers its glow.
        let top = straight(&run_on(9, 9, grey.clone(), &[glow(0.6, GlowOriginal::OnTop, StudioBlendMode::LinearDodgeAdd, None)]));
        assert!(top.iter().zip(straight(&grey)).all(|(a, b)| a.iter().zip(b).all(|(x, y)| close(*x, y, 1e-6))));
        // None: the glow alone; texels without glow are transparent.
        let alone = straight(&run_on(9, 9, grey.clone(), &[glow(0.6, GlowOriginal::None, StudioBlendMode::LinearDodgeAdd, None)]));
        assert_eq!(at(&alone, 0, 0), [0.0; 4]);
        assert!(close(at(&alone, 5, 4)[3], t, 1e-6));
        // A & B colours, sawtooth A>B, one loop, midpoint 0.5: colour = lerp(A, B, glow alpha).
        let ab = GlowColours { a: [1.0, 0.0, 0.0, 1.0], b: [0.0, 0.0, 1.0, 1.0], looping: GlowLooping::SawtoothAb, loops: 1.0, midpoint: 0.5 };
        let out = straight(&run_on(9, 9, impulse(9, 9, 4, 4), &[glow(0.5, GlowOriginal::Behind, StudioBlendMode::Normal, Some(ab.clone()))]));
        let p = at(&out, 5, 4);
        assert!(close(p[0], 1.0 - t, 1e-5) && close(p[1], 0.0, 1e-6) && close(p[2], t, 1e-5) && close(p[3], t, 1e-6), "{p:?}");
        let tri = GlowColours { looping: GlowLooping::TriangleAba, ..ab };
        assert!(close(tri.position(0.5) as f32, 1.0, 1e-12) && close(tri.position(1.0) as f32, 0.0, 1e-12));
        // Folio Glow rows: threshold level / 255, radius = blur radius, Add = linear dodge.
        let g = lowered_one(json!({"effect_id":"g","filter_kind":"glow","values":{"glow_radius":constant(json!([2.0]))}}));
        assert_eq!(
            g,
            SceneEffect::Glow(Box::new(GlowEffect {
                based_on_alpha: false,
                threshold: 153.0 / 255.0,
                taps,
                intensity: 1.0,
                original: GlowOriginal::Behind,
                mode: StudioBlendMode::LinearDodgeAdd,
                colours: None,
            }))
        );
        let k = |values: Value| refusal(one(json!({"effect_id":"g","filter_kind":"glow","values":values})));
        assert_eq!(k(json!({"glow_colors":constant(json!([3.0]))})), COMPOSITION_EFFECT_OPTION);
        assert_eq!(k(json!({"glow_operation":constant(json!([5.0]))})), COMPOSITION_EFFECT_OPTION);
        assert_eq!(k(json!({"glow_operation":constant(json!([1.0]))})), COMPOSITION_EFFECT_OPTION);
        assert_eq!(k(json!({"glow_colors":constant(json!([2.0])),"color_phase":constant(json!([0.1]))})), COMPOSITION_EFFECT_OPTION);
        assert_eq!(k(json!({"glow_dimensions":constant(json!([2.0]))})), COMPOSITION_EFFECT_KERNEL);
    });
}

#[test]
fn colour_family_maps_onto_pigment_adjustments() {
    bounded_case("colour_family_maps_onto_pigment_adjustments", || {
        let src = straight(&premultiplied_source());
        let check = |effects: &[SceneEffectEntry], want: &dyn Fn([f32; 4]) -> [f32; 3], name: &str| {
            let out = straight(&run(effects));
            for (i, (o, s)) in out.iter().zip(&src).enumerate() {
                if s[3] <= 0.0 {
                    assert_eq!(*o, [0.0; 4], "{name} {i}: transparent texels stay");
                    continue;
                }
                let w = want(*s);
                assert!(close(o[3], s[3], 1e-6), "{name} {i}: alpha kept");
                assert!(o[..3].iter().zip(w).all(|(a, b)| close(*a, b, 1e-5)), "{name} {i}: {o:?} vs {w:?}");
            }
        };
        check(&[colour_entry(vec![all(AdjustmentOp::Invert)], 0.0)], &|s| [1.0 - s[0], 1.0 - s[1], 1.0 - s[2]], "invert");
        check(
            &[colour_entry(vec![ChannelAdjustment { op: AdjustmentOp::Invert, channels: [true, false, false] }], 0.0)],
            &|s| [1.0 - s[0], s[1], s[2]],
            "invert red",
        );
        check(&[colour_entry(vec![all(AdjustmentOp::Invert)], 0.25)], &|s| [0.75 - 0.5 * s[0], 0.75 - 0.5 * s[1], 0.75 - 0.5 * s[2]], "invert blended");
        let levels = LevelsRecord { input_black: 0.25, input_white: 0.75, gamma: 1.0, output_black: 0.0, output_white: 1.0 };
        let id = LevelsRecord::IDENTITY;
        check(
            &[colour_entry(vec![all(AdjustmentOp::Levels(Levels { composite: levels, red: id, green: id, blue: id }))], 0.0)],
            &|s| [0usize, 1, 2].map(|k| ((s[k] - 0.25) / 0.5).clamp(0.0, 1.0)),
            "levels",
        );
        let exposure = |e: f32| AdjustmentOp::Exposure(Exposure { exposure: e, offset: 0.0, gamma: 1.0, linearize_gamma: 1.0 });
        check(&[colour_entry(vec![all(exposure(1.0))], 0.0)], &|s| [0usize, 1, 2].map(|k| (2.0 * s[k]).min(1.0)), "exposure +1");
        let ramp = |a: [f32; 3], b: [f32; 3]| {
            AdjustmentOp::GradientMap(GradientMap {
                stops: vec![GradientMapStop { position: 0.0, colour: a }, GradientMapStop { position: 1.0, colour: b }],
                reverse: false,
            })
        };
        check(
            &[colour_entry(vec![all(ramp([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]))], 0.0)],
            &|s| {
                let l = 0.3 * s[0] + 0.59 * s[1] + 0.11 * s[2];
                [1.0 - l, 0.0, l]
            },
            "tint",
        );
        // Identity parameters are the identity.
        let same = |s: [f32; 4]| [s[0], s[1], s[2]];
        let hs = AdjustmentOp::HueSaturation(HueSaturation { hue: 0.0, saturation: 0.0, lightness: 0.0, colorize: false });
        let cb = AdjustmentOp::ColourBalance(ColourBalance {
            shadows: [0.0; 3],
            midtones: [0.0; 3],
            highlights: [0.0; 3],
            preserve_luminosity: false,
        });
        let ident_levels = AdjustmentOp::Levels(Levels { composite: id, red: id, green: id, blue: id });
        for (name, e) in [
            ("levels", colour_entry(vec![all(ident_levels)], 0.0)),
            ("hue_saturation", colour_entry(vec![all(hs)], 0.0)),
            ("exposure", colour_entry(vec![all(exposure(0.0))], 0.0)),
            ("color_balance", colour_entry(vec![all(cb)], 0.0)),
            ("tint amount 0", colour_entry(vec![all(ramp([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]))], 1.0)),
            ("invert blend 100", colour_entry(vec![all(AdjustmentOp::Invert)], 1.0)),
        ] {
            check(&[e], &same, name);
        }
        // Folio rows: defaults and their pigment ops.
        let c = |e: Value| match lowered_one(e) {
            SceneEffect::Colour(c) => *c,
            other => panic!("{other:?}"),
        };
        let ex = c(json!({"effect_id":"x","filter_kind":"exposure","values":{"exposure":constant(json!([1.5]))}}));
        assert_eq!(ex.ops, vec![all(AdjustmentOp::Exposure(Exposure { exposure: 1.5, offset: 0.0, gamma: 1.0, linearize_gamma: 2.2 }))]);
        let ex = c(json!({"effect_id":"x","filter_kind":"exposure","values":{"bypass_linear_light_conversion":constant(json!([1.0])),
            "channels":constant(json!([2.0])),"green_exposure":constant(json!([-1.0]))}}));
        let lin = |e: f32| AdjustmentOp::Exposure(Exposure { exposure: e, offset: 0.0, gamma: 1.0, linearize_gamma: 1.0 });
        assert_eq!(ex.ops, vec![
            ChannelAdjustment { op: lin(0.0), channels: [true, false, false] },
            ChannelAdjustment { op: lin(-1.0), channels: [false, true, false] },
            ChannelAdjustment { op: lin(0.0), channels: [false, false, true] },
        ]);
        let hs = c(json!({"effect_id":"h","filter_kind":"hue_saturation","values":{"master_hue":constant(json!([270.0])),
            "master_saturation":constant(json!([-50.0]))}}));
        assert_eq!(hs.ops, vec![all(AdjustmentOp::HueSaturation(HueSaturation { hue: -90.0, saturation: -50.0, lightness: 0.0, colorize: false }))]);
        let hs = c(json!({"effect_id":"h","filter_kind":"hue_saturation","values":{"colorize":constant(json!([1.0]))}}));
        assert_eq!(hs.ops, vec![all(AdjustmentOp::HueSaturation(HueSaturation { hue: 0.0, saturation: 25.0, lightness: 0.0, colorize: true }))]);
        let tint = c(json!({"effect_id":"t","filter_kind":"tint"}));
        assert_eq!((tint.ops, tint.blend), (vec![all(ramp([0.0; 3], [1.0; 3]))], 0.0));
        let tri = c(json!({"effect_id":"t","filter_kind":"tritone","values":{"blend_with_original":constant(json!([25.0]))}}));
        assert_eq!(tri.blend, 0.25);
        let AdjustmentOp::GradientMap(map) = &tri.ops[0].op else { panic!() };
        assert_eq!(map.stops.iter().map(|s| s.position).collect::<Vec<_>>(), vec![0.0, 0.5, 1.0]);
        let inv = c(json!({"effect_id":"i","filter_kind":"invert","values":{"channel":constant(json!([3.0]))}}));
        assert_eq!(inv.ops, vec![ChannelAdjustment { op: AdjustmentOp::Invert, channels: [false, true, false] }]);
        let lv = c(json!({"effect_id":"l","filter_kind":"levels","values":{"input_black":constant(json!([0.25])),"input_white":constant(json!([0.75]))}}));
        assert_eq!(lv.ops, vec![all(AdjustmentOp::Levels(Levels { composite: levels, red: id, green: id, blue: id }))]);
        let bal = c(json!({"effect_id":"b","filter_kind":"color_balance","values":{"midtone_red_balance":constant(json!([40.0]))}}));
        let AdjustmentOp::ColourBalance(cb) = &bal.ops[0].op else { panic!() };
        assert_eq!((cb.midtones, cb.preserve_luminosity), ([40.0, 0.0, 0.0], false));
        // Outside pigment's domain or without a formula: refused typed.
        let k = |e: Value| refusal(one(e));
        assert_eq!(k(json!({"effect_id":"i","filter_kind":"invert","values":{"channel":constant(json!([16.0]))}})), COMPOSITION_EFFECT_OPTION);
        assert_eq!(k(json!({"effect_id":"i","filter_kind":"invert","values":{"channel":constant(json!([6.0]))}})), COMPOSITION_EFFECT_OPTION);
        assert_eq!(
            k(json!({"effect_id":"l","filter_kind":"levels","values":{"input_black":constant(json!([0.8])),"input_white":constant(json!([0.2]))}})),
            COMPOSITION_EFFECT_OPTION
        );
        assert_eq!(k(json!({"effect_id":"x","filter_kind":"exposure","values":{"exposure":constant(json!([30.0]))}})), COMPOSITION_EFFECT_OPTION);
        assert!(k(json!({"effect_id":"x","filter_kind":"exposure","values":{"exposure":constant(json!([101.0]))}})).contains("effect_param_out_of_range"));
    });
}

#[test]
fn entry_blend_modes_and_masks_compose_like_layers() {
    bounded_case("entry_blend_modes_and_masks_compose_like_layers", || {
        println!("NOT_PROVEN: After Effects parity of the blur, glow and colour families and of entry blend modes and effect masks (no D5 oracle case for After Effects effects)");
        println!("NOT_PROVEN: GPU path of the composition effect stack (the stack runs on the host after the backend renders the layer; no render-gpu proof)");
        let fill = SceneEffect::Circle {
            centre: [8.0, 6.0],
            radius_px: 100.0,
            feather_px: 0.0,
            colour: [0.5, 1.0, 1.0, 1.0],
            opacity: 1.0,
            composite: GeneratorComposite::Replace,
        };
        let wrap = |mode: StudioBlendMode, mask: Option<Vec<f32>>| {
            entry("c", SceneEffect::Composite(Box::new(EntryComposite { effect: fill.clone(), mode, mask })))
        };
        let src = straight(&premultiplied_source());
        // Multiply (STU-FX-008): Cs' = (1 - ab) Cs + ab Cs Cb, alpha of the entry's result (1).
        let out = straight(&run(&[wrap(StudioBlendMode::Multiply, None)]));
        for (o, s) in out.iter().zip(&src) {
            let cs = [0.5, 1.0, 1.0];
            let want = [0usize, 1, 2].map(|k| (1.0 - s[3]) * cs[k] + s[3] * cs[k] * s[k]);
            assert!(close(o[3], 1.0, 1e-6) && o[..3].iter().zip(want).all(|(a, b)| close(*a, b, 1e-5)), "{o:?} vs {want:?} over {s:?}");
        }
        // Normal with a full mask equals the unwrapped entry bit for bit.
        let plain = run(&[entry("c", fill.clone())]);
        assert_eq!(run(&[wrap(StudioBlendMode::Normal, Some(vec![1.0; (W * H) as usize]))]), plain);
        // STU-FX-004: the entry applies only where its mask covers.
        let left: Vec<f32> = (0..W * H).map(|i| if i % W < 8 { 1.0 } else { 0.0 }).collect();
        let masked = run(&[wrap(StudioBlendMode::Normal, Some(left))]);
        let source = premultiplied_source();
        for i in 0..(W * H) as usize {
            assert_eq!(masked[i], if (i as u32) % W < 8 { plain[i] } else { source[i] }, "texel {i}");
        }
        // Hostile: a mask of the wrong size, a value outside 0..=1, dissolve, a nested wrapper.
        let mut b = premultiplied_source();
        let cancel = CancellationToken::default();
        for bad in [
            wrap(StudioBlendMode::Normal, Some(vec![1.0; 3])),
            wrap(StudioBlendMode::Normal, Some(vec![1.5; (W * H) as usize])),
            wrap(StudioBlendMode::Dissolve, None),
            entry("n", SceneEffect::Composite(Box::new(EntryComposite { effect: wrap(StudioBlendMode::Screen, None).effect, mode: StudioBlendMode::Normal, mask: None }))),
        ] {
            assert_eq!(apply_effects(W, H, &mut b, &[bad], &cancel), Err(EffectError::Parameter(0)));
        }
        // Folio entry fields lower to the wrapper: the named mask alone (mode none on the layer, so
        // the layer itself is not masked) and the canonical blend discriminant (5 = multiply).
        let mask = json!([{"mask_id":"m","mode":"none","closed":true,"static_value":[
            0.0,0.0,0.0,0.0,0.0,0.0, 8.0,0.0,0.0,0.0,0.0,0.0, 8.0,12.0,0.0,0.0,0.0,0.0, 0.0,12.0,0.0,0.0,0.0,0.0]}]);
        let inv = json!({"effect_id":"i","filter_kind":"invert","blend_mode":5,"mask_id":"m"});
        let e = lowered(json!({"masks":mask, "effects":[inv]}), 0).expect("lowers");
        assert!(e.mask.is_none(), "a mode-none mask does not mask the layer");
        let SceneEffect::Composite(c) = &e.effects[0].effect else { panic!("{:?}", e.effects[0].effect) };
        assert_eq!(c.mode, StudioBlendMode::Multiply);
        assert_eq!(c.effect, SceneEffect::Colour(Box::new(ColourEffect { ops: vec![all(AdjustmentOp::Invert)], blend: 0.0 })));
        let cov = c.mask.as_ref().expect("entry mask");
        for (i, v) in cov.iter().enumerate() {
            assert!(close(*v, if (i as u32) % W < 8 { 1.0 } else { 0.0 }, 1e-6), "coverage {i}: {v}");
        }
        // An entry at the defaults is not wrapped (SMB-AE-EFFECT-STACK-1 bytes unchanged).
        assert!(matches!(lowered_one(json!({"effect_id":"i","filter_kind":"invert"})), SceneEffect::Colour(_)));
        // Folio refuses a tool-only or pass-through mode and an unknown mask; the adapter refuses dissolve.
        let doc = |e: Value, field: &str| {
            let t = refusal(json!({"masks":mask, "effects":[e]}));
            assert!(t.contains(&format!("/effects/i/{field}")), "{t}");
        };
        doc(json!({"effect_id":"i","filter_kind":"invert","blend_mode":24}), "blend_mode");
        doc(json!({"effect_id":"i","filter_kind":"invert","blend_mode":1}), "blend_mode");
        doc(json!({"effect_id":"i","filter_kind":"invert","mask_id":"x"}), "mask_id");
        assert_eq!(refusal(one(json!({"effect_id":"i","filter_kind":"invert","blend_mode":3}))), COMPOSITION_EFFECT_OPTION);
    });
}
