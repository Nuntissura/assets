//! Lane SMB-COMPOSITE-VECTOR-IN-GROUP slice 1: a vector source whose `out` port is image-typed
//! (Folio SMB-FOLIO-VECTOR-IMAGE-PORT) feeds a group's image inputs and lowers exactly like the
//! standalone vector plan (`vector_layer_render_plan`, renderer-proven) inside that group, with the
//! layer's authored appearance on the group entry. Without prepared vectors it is the typed
//! refusal `vector_to_image_conversion`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::*;
use hsk_studio_folio::TileRef;
use hsk_studio_nib::{FillRule, OwnedPath, PathElement, PathPoint};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{AlphaConvention, ColourTag, OutputTarget, PlanNode, SampleFormat};
use serde_json::{Value, json};

mod common;
use common::*;

fn target() -> OutputTarget {
    OutputTarget {
        extent: Rect { x: 0, y: 0, width: 64, height: 64 },
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded },
    }
}
fn info(tile: &TileRef) -> Option<TileInfo> {
    Some(TileInfo {
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        colour_sha256: [tile.object_key.len() as u8; 32],
        alpha_sha256: [9; 32],
    })
}
fn triangle() -> OwnedPath {
    let p = PathPoint::new;
    OwnedPath {
        elements: vec![
            PathElement::MoveTo(p(4.0, 4.0)),
            PathElement::LineTo(p(28.5, 6.25)),
            PathElement::LineTo(p(10.0, 30.0)),
            PathElement::Close,
        ],
        fill_rule: FillRule::NonZero,
    }
}
/// Group 1 composites raster 2 (ordinal 0) then vector 3 (ordinal 1, multiply 50 %), whose source
/// declares its `out` port `image`.
fn doc(port: &str) -> Value {
    let mut vector = layer(3, "vector", json!({"payload_kind":"vector","path_ids":[identity("SVPT",1)]}));
    vector["appearance"] = json!({"opacity":50.0,"fill_opacity":100.0,"blend_mode":
        hsk_studio_folio::BLEND_NORMAL + 3,"clipped":false,"isolation":"isolated"});
    let mut v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[layer(1, "group", json!({"payload_kind":"group"})), layer(2, "raster", json!({"payload_kind":"raster","tiles":[tile(2, "t00", 0, 0, 32, 32)]})), vector],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[source(2, "image"), composite(1), source(3, port)],
            "edges":[edge(address(2, "source", "out"), address(1, "mix", "inputs"), 0),
                     edge(address(3, "source", "out"), address(1, "mix", "inputs"), 1)],
            "outputs":[address(1, "mix", "out")]}
    });
    assign_all_profiles(&mut v);
    v
}
fn vectors() -> PreparedVectors {
    let spec = VectorLayerSpec {
        layer_id: identity("SLYR", 3),
        geometry: VectorGeometry::Paths(vec![triangle()]),
        fills: vec![FillSpec { paint: FillPaint::Solid([0.2, 0.6, 0.9, 1.0]), mode: StudioBlendMode::Normal, opacity: 1.0 }],
    };
    prepare_vector_layers(&[spec], &VectorResolution::default(), &CancellationToken::default()).unwrap()
}

#[test]
fn image_typed_vector_composites_inside_a_group_over_a_raster() {
    bounded_case("image_typed_vector_composites_inside_a_group_over_a_raster", || {
        let token = CancellationToken::default();
        let snap = open(&doc("image"));
        let plan = plan_of(&snap);
        let root = addr(1, "mix", "out");

        // Without prepared vectors: typed refusal, never dropped.
        let plain = evaluate(&plan, &snap, &token).unwrap();
        assert_eq!(
            plain.step(&addr(3, "source", "out")).unwrap().status,
            StepStatus::Unavailable("vector_to_image_conversion".into())
        );
        assert!(lower_to_render_plan(&plan, &plain, &snap, &root, target(), &info).is_err());

        let prepared = vectors();
        let plane = prepared.plane(&identity("SLYR", 3)).unwrap().clone();
        let inputs = RenderInputs { vectors: Some(&prepared), ..RenderInputs::default() };
        let report = evaluate_with_inputs(&plan, &snap, &inputs, &token).unwrap();
        report.require_ready().unwrap();
        assert_eq!(report.step(&addr(3, "source", "out")).unwrap().extent, plane.placement);
        let low = lower_with_inputs(&plan, &report, &snap, &inputs, &root, target(), &info).unwrap();
        let nodes = &low.plan.nodes;
        let PlanNode::Group(top) = &nodes[low.plan.root.0 as usize] else { panic!("root group") };
        assert_eq!(top.layers.len(), 2, "raster bottom, vector top");
        assert!(matches!(nodes[top.layers[0].input.0 as usize], PlanNode::Tile(_)));
        let entry = &top.layers[1];
        assert_eq!((entry.mode, entry.opacity), (StudioBlendMode::Multiply, 0.5));

        // The vector content subtree equals the standalone vector plan's root subtree.
        let alone = vector_layer_render_plan(&prepared, &identity("SLYR", 3), 7, target()).unwrap();
        let shape = |ns: &[PlanNode], id: usize| -> String {
            // Node structure without source keys (the raster tile took key 0 in the document plan).
            fn walk(ns: &[PlanNode], id: usize, out: &mut String) {
                match &ns[id] {
                    PlanNode::Group(g) => {
                        out.push_str(&format!("G{}[", g.isolated));
                        for l in &g.layers {
                            out.push_str(&format!("{:?}/{}/{}/{:?}:", l.mode, l.opacity, l.fill_opacity,
                                l.mask.map(|m| (m.placement, m.format, m.sha256, m.inverted, m.density))));
                            walk(ns, l.input.0 as usize, out);
                        }
                        out.push(']');
                    }
                    PlanNode::Solid { rect, rgba } => out.push_str(&format!("S{rect:?}{rgba:?}")),
                    other => out.push_str(&format!("{other:?}")),
                }
            }
            let mut s = String::new();
            walk(ns, id, &mut s);
            s
        };
        // The standalone plan's root IS the vector content group.
        let alone_content = alone.plan.root.0 as usize;
        assert_eq!(shape(nodes, entry.input.0 as usize), shape(&alone.plan.nodes, alone_content));
        // The coverage plane is served for the group's mask key.
        let PlanNode::Group(content) = &nodes[entry.input.0 as usize] else { panic!() };
        let key = content.layers[0].mask.unwrap().key;
        assert_eq!(low.vector_mask(key, &prepared).unwrap().sha256, plane.sha256);
        assert_eq!(low.sources.len(), 1);
    });
}
