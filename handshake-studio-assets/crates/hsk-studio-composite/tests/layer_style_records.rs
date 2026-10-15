//! Lane SMB-PS-ADJUST-STYLES slice 2b: Folio satin, bevel & emboss and gradient overlay records
//! lowered to pigment ops (`lower_extensions::effect_ops`), with the typed refusals. Pixel proofs:
//! render-cpu `tests/cpu_layer_styles.rs`; formulas: pigment `tests/layer_styles.rs`.
use hsk_studio_composite::BlendSpace;
use hsk_studio_composite::lower_extensions::{
    REFUSE_BEVEL_TEXTURE, REFUSE_EFFECT_GRADIENT_UNALIGNED, REFUSE_EFFECT_GRADIENT_UNRESOLVED,
    REFUSE_EFFECTS, effect_ops,
};
use hsk_studio_folio::{ProfileBinding, StudioEffectStack, StudioGlobalLight};
use hsk_studio_pigment::effect_math::{BevelPart, BevelStyle, EffectOp, GradientStyle};
use serde_json::{Value, json};

fn colour(r: f64, g: f64, b: f64) -> Value {
    json!({"r":r,"g":g,"b":b,"colour_profile_id":null})
}
fn effect(id: &str, opacity: f64, global: bool, params: Value) -> Value {
    json!({"instance_id":id,"enabled":true,"visible_in_editor":true,"blend_mode":2,"opacity":opacity,
        "colour":colour(0.1,0.2,0.3),"anti_alias":false,"noise":0.0,"angle":45.0,"use_global_light":global,
        "params":params})
}
fn stack(effects: Vec<Value>) -> StudioEffectStack {
    serde_json::from_value(json!({"effect_stack_id":"SEFX-019abcde-0000-7000-8000-000000000001","enabled":true,
        "scale":100.0,"effects":effects}))
    .expect("stack shape")
}
fn bevel(technique: &str, texture: bool) -> Value {
    json!({"effect_kind":"bevel_emboss","style":"outer_bevel","technique":technique,"direction":"down",
        "depth":250.0,"size":6.0,"soften":0.0,"altitude":10.0,
        "highlight_mode":10,"highlight_colour":colour(1.0,1.0,0.9),"highlight_opacity":75.0,
        "shadow_mode":5,"shadow_colour":colour(0.0,0.0,0.1),"shadow_opacity":50.0,
        "anti_alias_gloss":false,"use_contour":false,"use_texture":texture})
}
fn gradient(aligned: bool, stops: Option<Value>) -> Value {
    let mut v = json!({"effect_kind":"gradient_overlay",
        "gradient":{"schema_id":"hsk.studio.gradient@1","object_id":"g1"},"type":"Rflc","reverse":true,
        "dither":false,"align_with_layer":aligned,"scale":80.0,"offset_x":10.0,"offset_y":-5.0});
    if let Some(s) = stops {
        v["stops"] = s;
    }
    v
}
fn ops(effects: Vec<Value>) -> Result<Vec<EffectOp>, &'static str> {
    let light = StudioGlobalLight { angle: 120.0, altitude: 30.0 };
    effect_ops(&stack(effects), Some(&light), &ProfileBinding::Unassigned(()), BlendSpace::Encoded)
}

#[test]
fn styles_lower_to_pigment_ops_with_typed_refusals() {
    let stops = json!([
        {"position":0.0,"midpoint":null,"opacity":100.0,"colour":colour(1.0,0.0,0.0)},
        {"position":60.0,"midpoint":50.0,"opacity":40.0,"colour":colour(0.0,0.0,1.0)}]);
    let lowered = ops(vec![
        effect("sa", 50.0, false, json!({"effect_kind":"satin","distance":11.0,"size":14.0,"invert":true})),
        effect("be", 80.0, true, bevel("smooth", false)),
        effect("go", 90.0, false, gradient(true, Some(stops))),
    ])
    .unwrap();
    // Satin: local angle 45, distance, size, invert; colour and opacity of the common block.
    let EffectOp::Satin(s) = lowered[0] else { panic!("{:?}", lowered[0]) };
    assert_eq!((s.angle_deg, s.distance_px, s.size_px, s.invert, s.opacity, s.colour), (45.0, 11.0, 14.0, true, 0.5, [0.1, 0.2, 0.3]));
    // Bevel: two ops, shadow then highlight; Global Light angle 120 and altitude 30 replace the
    // local 45 / 10; opacity = part opacity x common opacity (0.5 x 0.8 = 0.4, 0.75 x 0.8 = 0.6);
    // modes 5 (multiply) and 10 (screen) as Studio discriminants.
    let (EffectOp::BevelEmboss(sh), EffectOp::BevelEmboss(hi)) = (lowered[1], lowered[2]) else { panic!("bevel") };
    assert_eq!((sh.part, hi.part), (BevelPart::Shadow, BevelPart::Highlight));
    assert_eq!((sh.shape.angle_deg, sh.shape.altitude_deg, sh.shape.style, sh.shape.up), (120.0, 30.0, BevelStyle::OuterBevel, false));
    assert_eq!((sh.shape.depth, sh.shape.size_px), (250.0, 6.0));
    assert!((sh.opacity - 0.4).abs() < 1e-6 && (hi.opacity - 0.6).abs() < 1e-6);
    assert_eq!((sh.colour, hi.colour), ([0.0, 0.0, 0.1], [1.0, 1.0, 0.9]));
    assert_eq!((sh.mode.discriminant(), hi.mode.discriminant()), (5, 10));
    // Gradient overlay: PSD token Rflc -> reflected; percent -> fractions; stops resolved.
    let EffectOp::GradientOverlay(g) = lowered[3] else { panic!("{:?}", lowered[3]) };
    assert_eq!((g.style, g.reverse, g.scale, g.offset, g.angle_deg), (GradientStyle::Reflected, true, 0.8, [0.1, -0.05], 45.0));
    let used = g.used_stops();
    assert_eq!((used.len(), used[1].position, used[1].opacity, used[1].colour), (2, 0.6, 0.4, [0.0, 0.0, 1.0]));

    // Chisel renders since lane SMB-PS-ADJUST-STYLES-2. Refusals: texture, unresolved and
    // canvas-aligned gradients, pattern overlay (no pattern pixels in the plan). A disabled
    // instance is not refused.
    let chisel = ops(vec![effect("be", 100.0, false, bevel("chisel_hard", false))]).unwrap();
    assert!(matches!(chisel[0], EffectOp::BevelEmboss(b) if b.shape.technique == hsk_studio_pigment::effect_math::BevelTechnique::ChiselHard));
    assert_eq!(ops(vec![effect("be", 100.0, false, bevel("smooth", true))]), Err(REFUSE_BEVEL_TEXTURE));
    assert_eq!(ops(vec![effect("go", 100.0, false, gradient(true, None))]), Err(REFUSE_EFFECT_GRADIENT_UNRESOLVED));
    let two = json!([{"position":0.0,"midpoint":null,"opacity":100.0,"colour":colour(0.0,0.0,0.0)},
        {"position":100.0,"midpoint":null,"opacity":100.0,"colour":colour(1.0,1.0,1.0)}]);
    assert_eq!(ops(vec![effect("go", 100.0, false, gradient(false, Some(two)))]), Err(REFUSE_EFFECT_GRADIENT_UNALIGNED));
    let pattern = json!({"effect_kind":"pattern_overlay","pattern":{"schema_id":"hsk.studio.pattern@1","object_id":"p"},
        "scale":100.0,"phase_x":0.0,"phase_y":0.0,"link_with_layer":true});
    assert_eq!(ops(vec![effect("po", 100.0, false, pattern.clone())]), Err(REFUSE_EFFECTS));
    let mut off = effect("po", 100.0, false, pattern);
    off["enabled"] = json!(false);
    assert_eq!(ops(vec![off]), Ok(vec![]));
}
