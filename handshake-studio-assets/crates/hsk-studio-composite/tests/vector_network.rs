//! Lane SMB-COMPOSITE-NETWORK-ADAPTER: the Folio adapter reads `StudioVectorNetwork` records
//! (`VectorAssets::network`) and refuses hostile or unsupported records typed, before conversion.
//! The save/reopen/render proof is harness `tests/viewer_network_adapter.rs`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::vector::{FillRule, OwnedPath, PathElement, PathPoint, REFUSE_NETWORK};
use hsk_studio_composite::*;
use hsk_studio_folio::{StudioDesignRecords, StudioImageSource, StudioVectorNetwork, StudioVectorRecords};
use serde_json::{Value, json};

mod common;
use common::*;

/// The 2x2 grid of a 10 px square (vertices 0..5, divider 1-4), the left face filled.
fn network(region: Value) -> Value {
    json!({"schema_id":"hsk.studio.vector_network@1","path_id":identity("SVPT",1),
        "vertices":[{"x":2.0,"y":3.0},{"x":7.0,"y":3.0},{"x":12.0,"y":3.0},{"x":12.0,"y":13.0},{"x":7.0,"y":13.0},{"x":2.0,"y":13.0}],
        "segments":[{"start":0,"end":1},{"start":1,"end":2},{"start":2,"end":3},{"start":3,"end":4},{"start":4,"end":5},{"start":5,"end":0},{"start":1,"end":4}],
        "regions":[region]})
}
fn left_face() -> Value {
    json!({"winding_rule":"NONZERO","loops":[[0,6,4,5]],"fills":true})
}

struct Net(Value);
impl VectorAssets for Net {
    fn path(&self, path_id: &str) -> Option<OwnedPath> {
        (path_id == identity("SVPT", 1)).then(|| OwnedPath {
            elements: vec![PathElement::MoveTo(PathPoint::new(2.0, 3.0)), PathElement::LineTo(PathPoint::new(12.0, 13.0))],
            fill_rule: FillRule::NonZero,
        })
    }
    fn image(&self, _: &StudioImageSource) -> Option<ImageInfo> {
        None
    }
    fn network(&self, path_id: &str) -> Option<StudioVectorNetwork> {
        (path_id == identity("SVPT", 1)).then(|| serde_json::from_value(self.0.clone()).expect("network shape"))
    }
}

fn design() -> StudioDesignRecords {
    let colour = |r: f64, b: f64| json!({"paint_kind":"solid","colour":{"r":r,"g":0.0,"b":b,"colour_profile_id":null}});
    serde_json::from_value(json!({"schema_id":"hsk.studio.design_records@1","document_id":identity("SDOC",1),
        "nodes":[{"node":{"node_kind":"layer","node_id":identity("SLYR",4)},"style_refs":{"fill_style_id":identity("SSTY",1)}}],
        "paint_styles":[{"schema_id":"hsk.studio.style_registry@1","style_id":identity("SSTY",1),"kind":"PAINT",
        "name":"fills","publishing":{"description":"","description_markdown":"","documentation_links":[],
        "publish_key":"fills","is_remote":false},"rows":[
            {"row_kind":"fill","paint":colour(1.0,0.0),"opacity":100.0,"blend_mode":2,"visible":true},
            {"row_kind":"stroke","paint":colour(0.0,1.0),
             "stroke":{"weight":{"value":2.0,"unit":"px"},"cap":"butt","join":"miter","align":"CENTER","miter_limit":4.0,
             "dash":[],"dash_offset":{"value":0.0,"unit":"px"}},"opacity":100.0,"blend_mode":2,"visible":true}]}]}))
    .expect("design records shape")
}

fn refusal(net: Value) -> Option<&'static str> {
    let mut d = mixed();
    assign_all_profiles(&mut d);
    d["geometry_unit"] = json!("px");
    let snap = open(&d);
    let records: StudioVectorRecords = serde_json::from_value(json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1)})).unwrap();
    let prepared = prepare_vectors(&snap, &design(), &records, &Net(net), BlendSpace::Encoded, &VectorResolution::default(), &CancellationToken::default()).unwrap();
    prepared.refusal(&identity("SLYR", 4))
}

#[test]
fn network_records_prepare_and_hostile_records_are_refused_typed() {
    // A valid network with fill and stroke rows prepares (its open-line path is not the geometry).
    assert_eq!(refusal(network(left_face())), None);
    // A region style the design records lack is unresolved (resolved region styles are painted,
    // lane SMB-VECTOR-STYLES tests/vector_styles.rs).
    let mut styled = left_face();
    styled["fill_style_id"] = json!(identity("SSTY", 2));
    assert_eq!(refusal(network(styled)), Some("vector_fill_style_unresolved"));
    // Out-of-bounds vertex index and a loop that does not close: nib validation, typed.
    let mut bad = network(left_face());
    bad["segments"][0]["end"] = json!(99);
    assert_eq!(refusal(bad), Some(REFUSE_NETWORK));
    assert_eq!(refusal(network(json!({"winding_rule":"NONZERO","loops":[[0,1,2]],"fills":true}))), Some(REFUSE_NETWORK));
    // Over nib's element budget: refused before the nib network is built.
    let mut huge = network(left_face());
    huge["vertices"] = Value::Array(vec![json!({"x":0.0,"y":0.0}); hsk_studio_nib::network::MAX_NETWORK_ELEMENTS + 1]);
    assert_eq!(refusal(huge), Some(REFUSE_NETWORK));
}
