//! SMB-COMPOSITE-NEW-KINDS slice 1: Folio `blur` effect instances (`render_scope` layer | below)
//! lower to pigment's `LayerExtension.blur` (layer blur / backdrop blur); the refusals are typed and
//! a document without a blur lowers byte-identically. Expected entries are derived by hand from the
//! fixture (node ids as in tests/render_plan_lowering.rs `tiled`).
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::*;
use hsk_studio_folio::{StudioRasterRecords, TileRef};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blur_math::{BlurScope, LayerBlur};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, ColourTag, ExtOperator, LayerExtension, OutputTarget, PlanNodeId, SampleFormat,
};
use serde_json::{Value, json};

mod common;
use common::*;

fn target() -> OutputTarget {
    OutputTarget {
        extent: Rect { x: 0, y: 0, width: 128, height: 128 },
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded },
    }
}
struct Info;
impl TileInfoSource for Info {
    fn tile_info(&self, tile: &TileRef) -> Option<TileInfo> {
        Some(TileInfo {
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: [tile.object_key.len() as u8; 32],
            alpha_sha256: [3; 32],
        })
    }
}
/// Group 1 (isolated, normal unless `pass_through`) = raster 2 (two tiles) then raster 3.
fn doc(pass_through: bool) -> Value {
    let mut v = mixed();
    assign_all_profiles(&mut v);
    v["layers"][0]["appearance"] = json!({"opacity":100.0,"fill_opacity":100.0,
        "blend_mode": if pass_through { 1 } else { 2 },"clipped":false,
        "isolation": if pass_through { "pass_through" } else { "isolated" }});
    v["layers"][1]["payload"] =
        json!({"payload_kind":"raster","tiles":[tile(2,"t00",0,0,64,64),tile(2,"t10",1,0,64,32)]});
    v["layers"][2]["payload"] = json!({"payload_kind":"raster","tiles":[tile(3,"t01",0,1,64,64)]});
    v
}
fn blur(id: &str, scope: &str, radius: f64) -> Value {
    json!({"instance_id":id,"enabled":true,"visible_in_editor":true,"blend_mode":2,"opacity":100.0,
        "anti_alias":false,"noise":0.0,"angle":0.0,"use_global_light":false,
        "params":{"effect_kind":"blur","render_scope":scope,"radius":radius}})
}
fn stack(effects: Vec<Value>) -> Value {
    json!({"effect_stack_id":"SEFX-019abcde-0000-7000-8000-000000000001","enabled":true,"scale":100.0,
        "effects":effects})
}
fn records(layers: Value) -> StudioRasterRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.raster_records@1",
        "document_id":identity("SDOC",1),"layers":layers}))
    .expect("records shape")
}
fn flow(v: &Value, recs: &StudioRasterRecords) -> (EvalReport, Result<ExtendedLowering, CompositeError>) {
    let snap = open(v);
    let token = CancellationToken::default();
    let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
    let report = evaluate_extended(&plan, &snap, recs, &token).unwrap();
    let low = lower_to_render_plan_extended(&plan, &report, &snap, recs, &addr(1, "mix", "out"), target(), &Info);
    (report, low)
}

#[test]
fn folio_blurs_lower_to_pigment_layer_and_backdrop_blur() {
    bounded_case("folio_blurs_lower_to_pigment_layer_and_backdrop_blur", || {
        let recs = records(json!([
            {"layer_id":identity("SLYR",2),"effects":stack(vec![blur("b1","layer",4.0)])},
            {"layer_id":identity("SLYR",3),"effects":stack(vec![blur("b2","below",2.5)])}]));
        let (report, low) = flow(&doc(false), &recs);
        // The fixture's unrelated vector and text outputs stay unavailable; the rendered root is ready.
        assert_eq!(report.step(&addr(1, "mix", "out")).unwrap().status, StepStatus::Ready);
        let low = low.unwrap();
        // Nodes: tiles 0, 1 + tile group 2 (raster 2), tile 3 (raster 3), root group 4.
        assert_eq!(
            low.extensions.layers,
            vec![
                LayerExtension::new(PlanNodeId(4), 0).with_blur(LayerBlur::new(BlurScope::Layer, 4.0)),
                LayerExtension::new(PlanNodeId(4), 1).with_blur(LayerBlur::new(BlurScope::Backdrop, 2.5)),
            ]
        );
        assert_eq!(
            low.extensions.validate(&low.plan),
            Ok(vec![ExtOperator::Blur(BlurScope::Layer), ExtOperator::Blur(BlurScope::Backdrop)])
        );
        for ledger in [&report.receipt.exactness, &low.exactness] {
            for key in ["blur_layer", "blur_backdrop"] {
                assert_eq!(
                    ledger.operators.iter().find(|o| o.key == key).map(|o| o.exactness),
                    Some(Exactness::Approximate),
                    "{key}"
                );
            }
        }
        // Without the records' blurs the plan is the plain one (static path unchanged).
        let (_, plain) = flow(&doc(false), &records(json!([])));
        let plain = plain.unwrap();
        assert_eq!(plain.plan, low.plan);
        assert!(plain.extensions.is_empty());
    });
}

#[test]
fn blur_refusals_are_typed_and_never_dropped() {
    bounded_case("blur_refusals_are_typed_and_never_dropped", || {
        let refused = |v: &Value, layers: Value, code: &str| {
            let recs = records(layers);
            let (report, low) = flow(v, &recs);
            let statuses = format!("{:?}", report.steps.iter().map(|s| &s.status).collect::<Vec<_>>());
            assert!(statuses.contains(code), "evaluation {statuses} lacks {code}");
            let low = low.unwrap_err().to_string();
            assert!(low.contains(code), "lowering {low} lacks {code}");
        };
        let shadow = json!({"instance_id":"ds","enabled":true,"visible_in_editor":true,"blend_mode":2,
            "opacity":50.0,"colour":{"r":0.0,"g":0.0,"b":0.0,"colour_profile_id":null},"anti_alias":false,
            "noise":0.0,"angle":120.0,"use_global_light":false,
            "params":{"effect_kind":"drop_shadow","distance":2.0,"spread":0.0,"size":2.0,"layer_knocks_out":false}});
        let l2 = identity("SLYR", 2);
        // A blur with another live effect on one layer: pigment's BlurCombination code.
        refused(&doc(false), json!([{"layer_id":l2,"effects":stack(vec![blur("b","layer",2.0), shadow])}]),
            "plan_blur_combination");
        // Two blurs on one layer.
        refused(&doc(false), json!([{"layer_id":l2,"effects":stack(vec![blur("a","layer",2.0), blur("b","below",2.0)])}]),
            "blur_multiple");
        // A blur below 100 percent or not normal.
        let mut faint = blur("b", "layer", 2.0);
        faint["opacity"] = json!(60.0);
        refused(&doc(false), json!([{"layer_id":l2,"effects":stack(vec![faint])}]), "blur_parameter");
        // A backdrop blur in a pass-through group (no own result to be the backdrop).
        refused(&doc(true), json!([{"layer_id":l2,"effects":stack(vec![blur("b","below",2.0)])}]),
            "plan_blur_combination");
        // A disabled blur instance renders nothing and is not refused.
        let mut off = blur("b", "layer", 2.0);
        off["enabled"] = json!(false);
        let recs = records(json!([{"layer_id":l2,"effects":stack(vec![off])}]));
        let (report, low) = flow(&doc(false), &recs);
        assert_eq!(report.step(&addr(1, "mix", "out")).unwrap().status, StepStatus::Ready);
        assert!(low.unwrap().extensions.is_empty());
    });
}
