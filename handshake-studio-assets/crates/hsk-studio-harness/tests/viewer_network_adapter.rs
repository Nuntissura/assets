//! SMB-COMPOSITE-NETWORK-ADAPTER (feature `viewer`): a saved vector-network document, reopened,
//! renders through composite's Folio adapter (`prepare_vectors` reading the `StudioVectorNetwork`
//! record) bit for bit like the viewer's own network drawing, without the harness building it.
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, solid_psd};
use hsk_studio_folio::{StudioDesignRecords, StudioVectorRecords};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::edit::Tool;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::vector::{ShapeKind, VectorTool};
use serde_json::json;

const W: usize = 96;
const H: usize = 64;

fn px(rgba: &[u8], x: usize, y: usize) -> [u8; 3] {
    let i = (y * W + x) * 4;
    [rgba[i], rgba[i + 1], rgba[i + 2]]
}

/// Folio paint for `layer`: a red fill row and a blue 2 px butt/miter stroke row (the viewer tool's
/// stroke), colours 0/1 for the linear-light plan.
fn design(document_id: &str, layer: &str) -> StudioDesignRecords {
    let style = "SSTY-019abcde-0000-7000-8000-000000000001";
    let colour = |r: f64, b: f64| json!({"paint_kind":"solid","colour":{"r":r,"g":0.0,"b":b,"colour_profile_id":null}});
    serde_json::from_value(json!({"schema_id":"hsk.studio.design_records@1","document_id":document_id,
        "nodes":[{"node":{"node_kind":"layer","node_id":layer},"style_refs":{"fill_style_id":style}}],
        "paint_styles":[{"schema_id":"hsk.studio.style_registry@1","style_id":style,"kind":"PAINT",
        "name":"fills","publishing":{"description":"","description_markdown":"","documentation_links":[],
        "publish_key":"fills","is_remote":false},"rows":[
            {"row_kind":"fill","paint":colour(1.0,0.0),"opacity":100.0,"blend_mode":2,"visible":true},
            {"row_kind":"stroke","paint":colour(0.0,1.0),
             "stroke":{"weight":{"value":2.0,"unit":"px"},"cap":"butt","join":"miter","align":"CENTER","miter_limit":4.0,
             "dash":[],"dash_offset":{"value":0.0,"unit":"px"}},"opacity":100.0,"blend_mode":2,"visible":true}]}]}))
    .expect("design records shape")
}

#[test]
fn saved_network_renders_through_the_folio_adapter() {
    let scratch = Scratch::new("network-adapter");
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(W as u32, H as u32, "paper", [255, 255, 255])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    v.set_vector_tool(VectorTool { fill_on: false, fill: [255, 0, 0], stroke_on: true, stroke: [0, 0, 255], stroke_width: 2.0, ..VectorTool::default() });
    let rect = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [50.0, 40.0], false).unwrap();
    v.promote_network(&rect).unwrap();
    v.select(Some(rect.clone()));
    v.set_tool(Tool::Pen);
    v.pen_point(30.0, 10.0, None, 1.0).unwrap();
    v.pen_point(30.0, 40.0, None, 1.0).unwrap();
    v.pen_end();
    v.network_paint_bucket(&rect, [20.0, 25.0], true).unwrap();
    v.render_if_dirty();
    let drawn = v.frame().unwrap().rgba8.clone();
    let out = scratch.0.join("network.handshake");
    v.save(&out, false).unwrap();

    let mut again = Viewer::new(Prefer::Cpu);
    again.open(&out).unwrap();
    let snap = again.run().snapshot().unwrap();
    let document_id = snap.document().document_id.clone();
    let records: StudioVectorRecords = serde_json::from_value(json!({"schema_id":"hsk.studio.vector_records@1","document_id":document_id})).unwrap();
    let folio = again.render_folio_vectors(&design(&document_id, &rect), &records).unwrap().rgba8;
    // Fill only in the filled face, blue strokes on every edge and the divider, white outside.
    assert_eq!(px(&folio, 20, 25), [255, 0, 0]);
    assert_eq!(px(&folio, 20, 11), [255, 0, 0], "pure fill inside the 2 px edge stroke");
    assert_eq!(px(&folio, 40, 25), [255, 255, 255], "unfilled face");
    assert_eq!(px(&folio, 30, 25), [0, 0, 255], "divider stroke");
    assert_eq!(px(&folio, 45, 10), [0, 0, 255], "edge stroke over the unfilled face");
    assert_eq!(px(&folio, 5, 5), [255, 255, 255]);
    // Same pixels as the viewer drew before saving (same nib fill and stroke geometry).
    assert_eq!(folio, drawn);
}
