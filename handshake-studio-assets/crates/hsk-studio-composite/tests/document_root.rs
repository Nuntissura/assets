//! Document root (`LoweredPlan::with_document_root`): one root for every graph output, so a viewer
//! needs no display-only Folio group. The expected plan is the one composite lowers for the same
//! document with an authored isolated normal group over the outputs (the wrapper the harness viewer
//! builds today), so the proof does not depend on the code under test describing itself.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{
    BlendSpace, CompositeError, DOCUMENT_ROOT_LAYER, Limits, TileInfo, TileInfoSource, evaluate,
    lower, lower_to_render_plan,
};
use hsk_studio_folio::TileRef;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, ColourTag, OutputTarget, PlanNode, SampleFormat,
};
use serde_json::{Value, json};

mod common;
use common::*;

fn target() -> OutputTarget {
    OutputTarget {
        extent: Rect { x: 0, y: 0, width: 256, height: 256 },
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::LinearLight },
    }
}
struct Info;
impl TileInfoSource for Info {
    fn tile_info(&self, tile: &TileRef) -> Option<TileInfo> {
        let h = |salt: u8| [tile.object_key.bytes().fold(salt, |a, b| a.wrapping_mul(31).wrapping_add(b)); 32];
        Some(TileInfo {
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: h(1),
            alpha_sha256: h(2),
        })
    }
}

/// Group 1 (rasters 2, 3) and rasters 4 (multiply, 50 %) and 5 (hidden) are the graph outputs.
fn document() -> Value {
    let mut v = mixed();
    assign_all_profiles(&mut v);
    v["layers"][1]["payload"] = json!({"payload_kind":"raster","tiles":[tile(2,"t00",0,0,64,64)]});
    v["layers"][2]["payload"] = json!({"payload_kind":"raster","tiles":[tile(3,"t01",0,1,64,64)]});
    v["layers"][3] = layer(4, "raster", json!({"payload_kind":"raster","tiles":[tile(4,"t11",1,1,64,64)]}));
    v["layers"][3]["appearance"] = json!({"opacity":50.0,"fill_opacity":100.0,"blend_mode":
        hsk_studio_folio::BLEND_NORMAL + 3,"clipped":false,"isolation":"isolated"});
    v["layers"][4] = layer(5, "raster", json!({"payload_kind":"raster","tiles":[tile(5,"t20",2,0,64,64)]}));
    v["layers"][4]["visible"] = json!(false);
    v["graph"]["operations"] =
        json!([source(2, "image"), source(3, "image"), source(4, "image"), source(5, "image"), composite(1)]);
    v
}

/// The same document with an authored root group 9 (isolated, normal, 100 %) over the outputs.
fn wrapped(mut v: Value) -> Value {
    let outputs = v["graph"]["outputs"].as_array().cloned().unwrap();
    for l in v["layers"].as_array_mut().unwrap() {
        if l["parent"].is_null() {
            l["parent"] = json!({"node_kind":"layer","node_id":identity("SLYR",9)});
        }
    }
    let mut root = layer(9, "group", json!({"payload_kind":"group"}));
    root["appearance"] = json!({"opacity":100.0,"fill_opacity":100.0,"blend_mode":
        hsk_studio_folio::BLEND_NORMAL,"clipped":false,"isolation":"isolated"});
    v["layers"].as_array_mut().unwrap().insert(0, root);
    v["graph"]["operations"].as_array_mut().unwrap().push(composite(9));
    for (n, o) in outputs.into_iter().enumerate() {
        v["graph"]["edges"].as_array_mut().unwrap().push(edge(o, address(9, "mix", "inputs"), n as u64));
    }
    v["graph"]["outputs"] = json!([address(9, "mix", "out")]);
    v
}

fn plan_for(v: &Value) -> (hsk_studio_folio::Snapshot, hsk_studio_composite::LoweredPlan) {
    let snap = open(v);
    let plan = lower(&snap, 7, BlendSpace::LinearLight, &Limits::default(), &CancellationToken::default())
        .unwrap();
    (snap, plan)
}

#[test]
fn document_root_equals_an_authored_root_group_over_the_outputs() {
    bounded_case("document_root_equals_an_authored_root_group_over_the_outputs", || {
        let token = CancellationToken::default();
        let (snap, plan) = plan_for(&document());
        let plan = plan.with_document_root().unwrap();
        let root = plan.document_root();
        assert_eq!(root.layer_id, DOCUMENT_ROOT_LAYER);
        assert_eq!(plan.outputs, std::slice::from_ref(&root));
        let report = evaluate(&plan, &snap, &token).unwrap();
        report.require_ready().unwrap();
        let got = lower_to_render_plan(&plan, &report, &snap, &root, target(), &Info).unwrap();

        let (wsnap, wplan) = plan_for(&wrapped(document()));
        let wreport = evaluate(&wplan, &wsnap, &token).unwrap();
        let want =
            lower_to_render_plan(&wplan, &wreport, &wsnap, &addr(9, "mix", "out"), target(), &Info).unwrap();
        assert_eq!(got, want, "document root lowers exactly like an authored root group");
        // Hand check: root group = [group 1, raster 4 (multiply 50 %)], the hidden output 5 absent.
        let PlanNode::Group(top) = &got.plan.nodes[got.plan.root.0 as usize] else { panic!("root is a group") };
        assert!(top.isolated);
        assert_eq!(top.layers.len(), 2);
        assert_eq!(top.layers[1].opacity, 0.5);
        let keys: Vec<&str> = got.sources.iter().map(|(_, t)| t.object_key.as_str()).collect();
        assert_eq!(keys, ["t00", "t01", "t11"]);
        assert_eq!(report.step(&root).unwrap().extent, Some(Rect { x: 0, y: 0, width: 128, height: 128 }));
    });
}

#[test]
fn document_root_refusals_are_typed_and_other_roots_are_unchanged() {
    bounded_case("document_root_refusals_are_typed_and_other_roots_are_unchanged", || {
        let token = CancellationToken::default();
        let (snap, plain) = plan_for(&document());
        let with_root = plain.clone().with_document_root().unwrap();
        // Static path: lowering an authored root is byte-identical with or without the extra step.
        for (plan, label) in [(&plain, "plain"), (&with_root, "with document root")] {
            let report = evaluate(plan, &snap, &token).unwrap();
            let low = lower_to_render_plan(plan, &report, &snap, &addr(1, "mix", "out"), target(), &Info)
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            let reference = {
                let r = evaluate(&plain, &snap, &token).unwrap();
                lower_to_render_plan(&plain, &r, &snap, &addr(1, "mix", "out"), target(), &Info).unwrap()
            };
            assert_eq!(low, reference, "{label}");
            assert_eq!(low.plan.digest().unwrap(), reference.plan.digest().unwrap());
        }
        assert_eq!(
            with_root.clone().with_document_root().unwrap_err(),
            CompositeError::UnsupportedDescriptor("document_root_present".into())
        );
        let mut empty = plain.clone();
        empty.outputs.clear();
        assert_eq!(
            empty.with_document_root().unwrap_err().to_string(),
            "composite_unavailable:composite.document_root/document/out:graph_outputs_empty"
        );
        let mut duplicate = plain.clone();
        duplicate.outputs.push(duplicate.outputs[0].clone());
        assert!(matches!(duplicate.with_document_root(),
            Err(CompositeError::Unavailable(r)) if r.contains("graph_output_duplicate")));
        let mut dangling = plain.clone();
        dangling.outputs.push(addr(42, "mix", "out"));
        assert!(matches!(dangling.with_document_root(), Err(CompositeError::DanglingInput(_))));
        // An unavailable output (a vector source without a route) blocks the document root, typed.
        let (vsnap, vplan) = plan_for(&{
            let mut v = mixed();
            assign_all_profiles(&mut v);
            v
        });
        let vplan = vplan.with_document_root().unwrap();
        let vreport = evaluate(&vplan, &vsnap, &token).unwrap();
        let err = lower_to_render_plan(&vplan, &vreport, &vsnap, &vplan.document_root(), target(), &Info)
            .unwrap_err();
        assert!(matches!(err, CompositeError::Unavailable(_)), "{err}");
    });
}
