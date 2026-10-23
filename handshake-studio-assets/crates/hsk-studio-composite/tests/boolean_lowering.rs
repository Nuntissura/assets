//! SMB-COMPOSITE-NEW-KINDS slice 3: live boolean groups (Folio `StudioBooleanGroup`, STU-VEC-013)
//! are drawn as one shape: the operands are combined per sample (UNION any, INTERSECT all, SUBTRACT
//! the first and none of the others, EXCLUDE an odd number), or the cached outline is used when
//! present, and the group lowers as a leaf filled with its own paint style. Expected coverages are
//! derived by hand from pixel-aligned rectangles (every sample of a pixel agrees).
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::*;
use hsk_studio_composite::vector::{FillRule, OwnedPath, PathElement, PathPoint};
use hsk_studio_folio::{StudioDesignRecords, StudioImageSource, StudioVectorRecords, TileRef};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, ColourTag, MaskFormat, OutputTarget, PlanNode, SampleFormat,
};
use serde_json::{Value, json};

mod common;
use common::*;

fn target() -> OutputTarget {
    OutputTarget {
        extent: Rect { x: 0, y: 0, width: 32, height: 32 },
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded },
    }
}
fn no_tiles(_: &TileRef) -> Option<TileInfo> {
    None
}
fn rect_path(x0: f64, y0: f64, x1: f64, y1: f64) -> OwnedPath {
    let p = PathPoint::new;
    OwnedPath {
        elements: vec![
            PathElement::MoveTo(p(x0, y0)),
            PathElement::LineTo(p(x1, y0)),
            PathElement::LineTo(p(x1, y1)),
            PathElement::LineTo(p(x0, y1)),
            PathElement::Close,
        ],
        fill_rule: FillRule::NonZero,
    }
}
/// SVPT 1: (2, 3)-(12, 13); SVPT 2: (8, 3)-(18, 13); SVPT 3: (2, 3)-(4, 5); SVPT 9: (1, 1)-(4, 4).
struct Assets;
impl VectorAssets for Assets {
    fn path(&self, path_id: &str) -> Option<OwnedPath> {
        [(1, (2.0, 3.0, 12.0, 13.0)), (2, (8.0, 3.0, 18.0, 13.0)), (3, (2.0, 3.0, 4.0, 5.0)), (9, (1.0, 1.0, 4.0, 4.0))]
            .into_iter()
            .find(|(n, _)| identity("SVPT", *n) == path_id)
            .map(|(_, (a, b, c, d))| rect_path(a, b, c, d))
    }
    fn image(&self, _: &StudioImageSource) -> Option<ImageInfo> {
        None
    }
}
fn source_vector(n: u8) -> Value {
    source(n, "vector")
}
/// Group 1 (boolean) with vector children 2 (SVPT 1) and 3 (SVPT 2); group 5 (boolean) with
/// children group 1 and vector 6 (SVPT 3); raster 4 is a stray child of group 5 used as a bad
/// operand. Group 1 and group 5 are graph outputs (their composite steps have no inputs: Folio
/// types vector sources `vector`, so they cannot feed a group's image inputs).
fn doc() -> Value {
    let child = |n: u8, kind: &str, payload: Value, parent: u8| {
        let mut l = layer(n, kind, payload);
        l["parent"] = json!({"node_kind":"layer","node_id":identity("SLYR",parent)});
        l
    };
    let mut v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[
            layer(5, "group", json!({"payload_kind":"group"})),
            child(1, "group", json!({"payload_kind":"group"}), 5),
            child(2, "vector", json!({"payload_kind":"vector","path_ids":[identity("SVPT",1)]}), 1),
            child(3, "vector", json!({"payload_kind":"vector","path_ids":[identity("SVPT",2)]}), 1),
            child(4, "raster", json!({"payload_kind":"raster","tiles":[]}), 5),
            child(6, "vector", json!({"payload_kind":"vector","path_ids":[identity("SVPT",3)]}), 5)],
        "graph":{"schema_id":"hsk.studio.layer_graph@1",
            "operations":[source_vector(2), source_vector(3), source_vector(6), source(4, "image"), composite(1), composite(5)],
            "edges":[],"outputs":[address(1,"mix","out"), address(5,"mix","out")]}
    });
    assign_all_profiles(&mut v);
    v
}
fn design() -> StudioDesignRecords {
    let node = |n: u8| json!({"node":{"node_kind":"layer","node_id":identity("SLYR",n)},
        "style_refs":{"fill_style_id":identity("SSTY",1)}});
    serde_json::from_value(json!({"schema_id":"hsk.studio.design_records@1","document_id":identity("SDOC",1),
        "nodes":[node(1), node(5)],
        "paint_styles":[{"schema_id":"hsk.studio.style_registry@1","style_id":identity("SSTY",1),"kind":"PAINT",
            "name":"fill","publishing":{"description":"","description_markdown":"","documentation_links":[],
            "publish_key":"fill","is_remote":false},
            "rows":[{"row_kind":"fill","paint":{"paint_kind":"solid","colour":{"r":0.2,"g":0.4,"b":0.6,"colour_profile_id":null}},
                "opacity":100.0,"blend_mode":2,"visible":true}]}]}))
    .expect("design shape")
}
fn records(op: &str, outline: bool, outer: Value) -> StudioVectorRecords {
    let mut inner = json!({"layer_id":identity("SLYR",1),"operation":op,"operands":[identity("SLYR",2), identity("SLYR",3)]});
    if outline {
        inner["fill_geometry_path_ids"] = json!([identity("SVPT",9)]);
    }
    serde_json::from_value(json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1),
        "boolean_groups":[inner, outer]}))
    .expect("vector records shape")
}
fn outer(operands: Vec<String>) -> Value {
    json!({"layer_id":identity("SLYR",5),"operation":"UNION","operands":operands})
}
fn prepare(r: &StudioVectorRecords) -> (hsk_studio_folio::Snapshot, PreparedVectors) {
    let snap = open(&doc());
    let p = prepare_vectors(&snap, &design(), r, &Assets, BlendSpace::Encoded, &VectorResolution::default(),
        &CancellationToken::default()).unwrap();
    (snap, p)
}
/// (plane rect, number of fully covered pixels; every other pixel is 0).
fn shape_of(v: &PreparedVectors, layer: u8) -> (Rect, usize) {
    let plane = v.plane(&identity("SLYR", layer)).expect("plane");
    assert!(plane.coverage.iter().all(|c| *c == 0 || *c == 255), "pixel-aligned shapes only");
    (plane.placement.unwrap(), plane.coverage.iter().filter(|c| **c == 255).count())
}

#[test]
fn boolean_operations_cached_outline_and_nesting_follow_the_hand_counts() {
    bounded_case("boolean_operations_cached_outline_and_nesting_follow_the_hand_counts", || {
        let ops6 = || outer(vec![identity("SLYR", 1), identity("SLYR", 6)]);
        // Operands (2, 3)-(12, 13) and (8, 3)-(18, 13): 10 rows; 16 / 4 / 6 / 12 covered columns.
        let both = Rect { x: 2, y: 3, width: 16, height: 10 };
        for (op, covered) in [("UNION", 160), ("INTERSECT", 40), ("SUBTRACT", 60), ("EXCLUDE", 120)] {
            let (_, v) = prepare(&records(op, false, ops6()));
            assert_eq!(shape_of(&v, 1), (both, covered), "{op}");
        }
        // The cached outline (1, 1)-(4, 4) replaces the operands.
        let (_, v) = prepare(&records("INTERSECT", true, ops6()));
        assert_eq!(shape_of(&v, 1), (Rect { x: 1, y: 1, width: 3, height: 3 }, 9));
        // Nesting: group 5 = UNION(group 1 = SUBTRACT, vector 6 (2, 3)-(4, 5)), inside the subtract.
        let (snap, v) = prepare(&records("SUBTRACT", false, ops6()));
        assert_eq!(shape_of(&v, 5), (both, 60));
        // Group 1 lowers as a leaf: content group [Layer{rows, mask coverage}] over its plane,
        // its vector operands are not part of the render.
        let token = CancellationToken::default();
        let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
        let inputs = RenderInputs { vectors: Some(&v), ..RenderInputs::default() };
        let report = evaluate_with_inputs(&plan, &snap, &inputs, &token).unwrap();
        assert_eq!(report.step(&addr(1, "mix", "out")).unwrap().extent, Some(both));
        assert_eq!(report.step(&addr(1, "mix", "out")).unwrap().status, StepStatus::Ready);
        let low = lower_with_inputs(&plan, &report, &snap, &inputs, &addr(1, "mix", "out"), target(), &no_tiles).unwrap();
        assert_eq!(low.plan.nodes.len(), 3, "solid, rows group, content group");
        assert_eq!(low.plan.nodes[0], PlanNode::Solid { rect: both, rgba: [0.2, 0.4, 0.6, 1.0] });
        let PlanNode::Group(content) = &low.plan.nodes[2] else { panic!("content group") };
        let mask = content.layers[0].mask.unwrap();
        assert_eq!((mask.format, mask.placement), (MaskFormat::CoverageU8, both));
        assert_eq!(low.vector_masks, vec![(mask.key, identity("SLYR", 1))]);
        assert!(low.sources.is_empty() && low.image_sources.is_empty());
    });
}

#[test]
fn boolean_refusals_are_typed() {
    bounded_case("boolean_refusals_are_typed", || {
        // A raster operand has no shape.
        let (snap, v) = prepare(&records("UNION", false, outer(vec![identity("SLYR", 1), identity("SLYR", 4)])));
        assert_eq!(v.refusal(&identity("SLYR", 5)), Some("boolean_operand_kind"));
        let token = CancellationToken::default();
        let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
        let inputs = RenderInputs { vectors: Some(&v), ..RenderInputs::default() };
        let report = evaluate_with_inputs(&plan, &snap, &inputs, &token).unwrap();
        assert_eq!(report.step(&addr(5, "mix", "out")).unwrap().status, StepStatus::Unavailable("boolean_operand_kind".into()));
        let err = lower_with_inputs(&plan, &report, &snap, &inputs, &addr(5, "mix", "out"), target(), &no_tiles).unwrap_err();
        assert!(err.to_string().contains("boolean_operand_kind"), "{err}");
        // An operand path the caller cannot resolve.
        let mut r = records("UNION", false, outer(vec![identity("SLYR", 1), identity("SLYR", 6)]));
        r.boolean_groups[0].fill_geometry_path_ids = vec![identity("SVPT", 8)];
        let (_, v) = prepare(&r);
        assert_eq!(v.refusal(&identity("SLYR", 1)), Some("vector_path_unresolved"));
        // Without prepared vectors a boolean group is an ordinary group (its vector operands are
        // not inputs), so nothing is drawn from the boolean record: the static path is unchanged.
        let plain = evaluate(&plan, &snap, &token).unwrap();
        assert_eq!(plain.step(&addr(1, "mix", "out")).unwrap().extent, None);
    });
}
