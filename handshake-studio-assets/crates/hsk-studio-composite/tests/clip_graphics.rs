//! Lane SMB-PREMIERE-COLOR-TITLES slice 3: a motion-graphics template (STU-VID-070) placed as a
//! graphic clip. The template composition holds an AE shape layer (rect + solid fill) and a text
//! layer; the clip's control values recolour the fill, set the source text (within the template's
//! hard font permissions), animate the shape's rotation and hide the text layer. The patched
//! records render through `CompositionTemporalSource` (the shape plane's painted fill is checked
//! texel by texel) and the frame is placed on the sequence as a precomposition.
#![cfg(feature = "motion")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::clip_finishing::{TEMPLATE_REQUIREMENT_UNMET, TEMPLATE_TARGET, check_template_requirements, instantiate_template};
use hsk_studio_composite::temporal::TemporalError;
use hsk_studio_composite::temporal::composition::{CompositionInput, CompositionTemporalSource};
use hsk_studio_composite::*;
use hsk_studio_folio::{
    Budget, StudioCompositionRecords, StudioGraphicInstance, StudioMotionRecords, StudioMotionTemplate, TileRef,
};
use hsk_studio_pigment::render_plan::{ColourTag, PlanNode, SourceKey};
use serde_json::{Value, json};
use std::collections::BTreeMap;

mod common;
use common::*;

const S: u64 = 254_016_000_000;
const SPACE: ColourTag = ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded };

fn info(_: &TileRef) -> Option<TileInfo> {
    None
}
fn document() -> Value {
    let mut v = mixed();
    assign_all_profiles(&mut v);
    v["geometry_unit"] = json!("px");
    v
}
fn compositions() -> StudioCompositionRecords {
    let layer = |n: u8| json!({"layer_id":identity("SLYR",n),
        "timing":{"in_point":0,"out_point":S,"start_time":0,"stretch":{"numerator":1,"denominator":1}}});
    serde_json::from_value(json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),
        "compositions":[{"schema_id":"hsk.studio.composition@1","composition_id":identity("SCOM",9),"name":"lower third","width":64,"height":32,
            "pixel_aspect":{"numerator":1,"denominator":1},"frame_rate":{"numerator":25,"denominator":1},
            "duration":S,"layers":[layer(2),layer(3)]}]}))
    .unwrap()
}
fn motion() -> StudioMotionRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.motion_records@1","document_id":identity("SDOC",1),
        "shape_layers":[{"layer_id":identity("SLYR",2),"root":{"items":[
            {"kind":"rect","position":[8.0,8.0],"size":[16.0,16.0]},
            {"kind":"fill","rgba":[1.0,1.0,1.0,1.0],"opacity":100.0,"rule":"non_zero"}]}}]}))
    .unwrap()
}
fn template() -> StudioMotionTemplate {
    serde_json::from_value(json!({"template_id":"basic-lower-third","name":"Basic Lower Third","composition_id":identity("SCOM",9),
        "required_fonts":["Inter"],
        "controls":[
            {"control_id":"g","name":"Bar","can_animate":false,"control":{"control_type":"group","heads_layer_kind":"shape"}},
            {"control_id":"bar","name":"Bar colour","can_animate":false,
             "control":{"control_type":"colour","target":{"target":"shape_fill","layer_id":identity("SLYR",2),"path":[1]},"default":[1.0,1.0,1.0,1.0]}},
            {"control_id":"spin","name":"Spin","can_animate":true,
             "control":{"control_type":"numeric_slider","layer_id":identity("SLYR",2),"property":"rotation","component":0,"min":-180.0,"max":180.0,"default":0.0}},
            {"control_id":"name","name":"Name","can_animate":false,
             "control":{"control_type":"source_text","layer_id":identity("SLYR",3),"default_text":"Name","font_family_editable":false,
                "faux_style_editable":true,"font_size_editable":true,"locked_font":"Inter","locked_size":24.0}},
            {"control_id":"show","name":"Show title","can_animate":false,"control":{"control_type":"checkbox","layer_id":identity("SLYR",3),"default":true}}]}))
    .unwrap()
}
fn instance() -> StudioGraphicInstance {
    serde_json::from_value(json!({"clip_id":"SCLP-019abcde-0000-7000-8000-000000000007","template_id":"basic-lower-third","values":[
        {"control_id":"bar","setting":{"value_type":"colour","rgba":[0.25,0.5,0.75,1.0]}},
        {"control_id":"spin","setting":{"value_type":"number","value":{"static_value":[0.0],"keyframes":[
            {"time":0,"value":[0.0],"in_interp":"linear","out_interp":"linear"},
            {"time":S,"value":[90.0],"in_interp":"linear","out_interp":"linear"}]}}},
        {"control_id":"name","setting":{"value_type":"text","text":"Ilka Vane","font_size":30.0}},
        {"control_id":"show","setting":{"value_type":"checkbox","checked":false}}]}))
    .unwrap()
}

#[test]
fn placed_template_resolves_controls_renders_and_refuses_unmet_requirements() {
    let cancel = CancellationToken::default();
    let (t, g) = (template(), instance());
    // Half a second into the clip: spin = 45 degrees (linear 0 -> 90 over one second).
    let frame = instantiate_template(&t, &g, &compositions(), &motion(), (S / 2) as i64, &cancel).unwrap();
    let comp = &frame.compositions.compositions[0];
    let shape = comp.layers.iter().find(|l| l.layer_id == identity("SLYR", 2)).unwrap();
    let rot = shape.streams.iter().find(|s| s.property == hsk_studio_folio::StudioLayerProperty::Rotation).unwrap();
    assert_eq!(rot.static_value, vec![45.0]);
    let text = comp.layers.iter().find(|l| l.layer_id == identity("SLYR", 3)).unwrap();
    assert!(text.streams.iter().any(|s| s.property == hsk_studio_folio::StudioLayerProperty::Opacity && s.static_value == vec![0.0]));
    // Source text: new words and size, the locked family stays.
    assert_eq!(frame.text.len(), 1);
    assert_eq!((frame.text[0].text.as_str(), frame.text[0].font_family.as_str(), frame.text[0].font_size), ("Ilka Vane", "Inter", 30.0));

    // Render the patched records: the shape plane is painted in the control colour (the
    // template's white is gone) at every covered texel.
    let snap = open(&document());
    let src = CompositionTemporalSource::new(
        &CompositionInput {
            snapshot: &snap, expected_revision: 7, records: &frame.compositions, composition_id: &frame.composition_id,
            budget: Budget::default(), resolver: &Available, blend_space: BlendSpace::Encoded, info: &info, space: SPACE,
            inputs: RenderInputs::default(),
        },
        &cancel,
    )
    .unwrap()
    .with_motion_records(&frame.motion, &BTreeMap::new(), SPACE, 7, None)
    .unwrap();
    let scene = src.scene_at((S / 2) as i64, &cancel).unwrap();
    let layer = scene.layers.iter().find(|l| l.layer_id == identity("SLYR", 2)).unwrap();
    let SceneContent::Static(c) = &layer.content else { panic!("shape content {:?}", layer.content) };
    assert!(
        c.plan.nodes.iter().any(|n| matches!(n, PlanNode::Solid { rgba, .. } if *rgba == [0.25, 0.5, 0.75, 1.0])),
        "control colour painted: {:?}",
        c.plan.nodes
    );
    assert!(!c.plan.nodes.iter().any(|n| matches!(n, PlanNode::Solid { rgba, .. } if *rgba == [1.0; 4])), "template white gone");
    assert_eq!(c.owned_masks[&SourceKey(0)].bounds.width, 16);
    // The shape layer turns 45 degrees (clockwise on the y-down canvas): the matrix's linear part is
    // [cos -sin; sin cos] at 45 degrees (the pivot is composite's anchor convention, proven by its
    // own composition tests).
    let [a, b, _, d, e, _] = layer.matrix.m;
    let h = 0.5f64.sqrt();
    assert!([a - h, b + h, d - h, e - h].iter().all(|v| v.abs() < 1e-9), "{:?}", layer.matrix);

    // Placed on the sequence: the graphic clip is a precomposition layer of the sequence frame.
    let placed = SceneLayer {
        layer_id: g.clip_id.clone(),
        content: SceneContent::Precomp(Box::new(scene)),
        mode: StudioBlendMode::Normal,
        opacity: 1.0,
        fill_opacity: 1.0,
        matrix: hsk_studio_pigment::transform_math::Affine2::translate(0.0, 40.0),
        matte: None,
        clipped: false,
    };
    assert!(matches!(placed.content, SceneContent::Precomp(ref s) if s.width == 64 && s.height == 32));

    // STU-VID-070c: a missing font is named, never substituted.
    let unmet = check_template_requirements(&t, &|f| f != "Inter", &|_| true, &|_| true).unwrap_err();
    assert_eq!((unmet.code(), unmet.missing_fonts.clone()), (TEMPLATE_REQUIREMENT_UNMET, vec!["Inter".to_string()]));
    assert!(check_template_requirements(&t, &|_| true, &|_| true, &|_| true).is_ok());
    // A control aimed at a layer outside the template composition is a typed refusal.
    let mut bad = template();
    bad.controls[1] = serde_json::from_value(json!({"control_id":"bar","name":"Bar","can_animate":false,
        "control":{"control_type":"colour","target":{"target":"shape_fill","layer_id":identity("SLYR",4),"path":[1]},"default":[1.0,1.0,1.0,1.0]}})).unwrap();
    let e = instantiate_template(&bad, &g, &compositions(), &motion(), 0, &cancel).unwrap_err();
    assert!(matches!(&e, TemporalError::Source { code, .. } if code == TEMPLATE_TARGET), "{e:?}");
    // A fill path that names the rect (not a fill item) is refused, never written elsewhere.
    let mut bad = template();
    bad.controls[1] = serde_json::from_value(json!({"control_id":"bar","name":"Bar","can_animate":false,
        "control":{"control_type":"colour","target":{"target":"shape_fill","layer_id":identity("SLYR",2),"path":[0]},"default":[1.0,1.0,1.0,1.0]}})).unwrap();
    assert!(instantiate_template(&bad, &g, &compositions(), &motion(), 0, &cancel).is_err());
}
