//! R44 finding F-R44-10 (lane SMB-R44-FOLLOWUPS-1): a marker entry on a stroke whose path holds a
//! closed subpath, or an open subpath the inside-path trim consumes entirely, is reported as a
//! typed per-layer `MarkerNote` on `PreparedVectors`, never a silent plain stroke. Oracles: nib's
//! own `marker_outlines` report on the same path (`closed_subpaths`, `consumed_subpaths`) and the
//! hand-built subpath order.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::vector::{FillRule, OwnedPath, PathElement, PathPoint};
use hsk_studio_composite::*;
use hsk_studio_folio::{StudioDesignRecords, StudioImageSource, StudioVectorRecords};
use hsk_studio_nib::markers::{MarkerAlign, MarkerEnd, MarkerOptions, MarkerShape, MarkerSpec, marker_outlines};
use serde_json::{Value, json};

mod common;
use common::*;

fn at(x: f64, y: f64) -> PathPoint {
    PathPoint::new(x, y)
}

/// Subpath 0: a closed square; subpath 1: the line (2, 8) -> (40, 8); subpath 2 (when `short`): a
/// 2 px line the 6 px inside-path triangle consumes.
fn path(short: bool) -> OwnedPath {
    use PathElement::*;
    let mut elements = vec![MoveTo(at(2.0, 20.0)), LineTo(at(10.0, 20.0)), LineTo(at(10.0, 28.0)), LineTo(at(2.0, 28.0)), Close];
    elements.extend([MoveTo(at(2.0, 8.0)), LineTo(at(40.0, 8.0))]);
    if short {
        elements.extend([MoveTo(at(20.0, 40.0)), LineTo(at(22.0, 40.0))]);
    }
    OwnedPath { elements, fill_rule: FillRule::NonZero }
}

struct Assets(OwnedPath);
impl VectorAssets for Assets {
    fn path(&self, path_id: &str) -> Option<OwnedPath> {
        (path_id == identity("SVPT", 1)).then(|| self.0.clone())
    }
    fn image(&self, _: &StudioImageSource) -> Option<ImageInfo> {
        None
    }
}

/// Layer 4 uses style 1: one 2 px stroke row.
fn design() -> StudioDesignRecords {
    let row = json!({"row_kind":"stroke","paint":{"paint_kind":"solid","colour":{"r":0.0,"g":0.0,"b":1.0,"colour_profile_id":null}},
        "stroke":{"weight":{"value":2.0,"unit":"px"},"cap":"butt","join":"miter","align":"CENTER","miter_limit":4.0,
        "dash":[],"dash_offset":{"value":0.0,"unit":"px"}},"opacity":100.0,"blend_mode":2,"visible":true});
    serde_json::from_value(json!({"schema_id":"hsk.studio.design_records@1","document_id":identity("SDOC",1),
        "nodes":[{"node":{"node_kind":"layer","node_id":identity("SLYR",4)},"style_refs":{"fill_style_id":identity("SSTY",1)}}],
        "paint_styles":[{"schema_id":"hsk.studio.style_registry@1","style_id":identity("SSTY",1),"kind":"PAINT",
        "name":"s1","publishing":{"description":"","description_markdown":"","documentation_links":[],
        "publish_key":"s1","is_remote":false},"rows":[row]}]}))
    .expect("design records shape")
}

fn records(end: &str) -> StudioVectorRecords {
    let mut r = json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1)});
    if end != "none" {
        r["stroke_markers"] = json!([{"style_id":identity("SSTY",1),"row":0,"start":"arrow.none","end":end,
            "start_scale":100.0,"end_scale":100.0,"alignment":"arrow_align.inside_path"}]);
    }
    serde_json::from_value(r).expect("vector records shape")
}

fn prepare(p: OwnedPath, end: &str) -> PreparedVectors {
    let mut d: Value = mixed();
    assign_all_profiles(&mut d);
    d["geometry_unit"] = json!("px");
    let res = VectorResolution { coverage: VectorCoverage::Exact, ..VectorResolution::default() };
    prepare_vectors(&open(&d), &design(), &records(end), &Assets(p), BlendSpace::Encoded, &res, &CancellationToken::default()).unwrap()
}

#[test]
fn closed_and_consumed_subpaths_of_a_marked_row_are_typed_notes() {
    let layer = identity("SLYR", 4);
    // nib's own report on the same path (the oracle): the square is closed, the 2 px line consumed.
    let spec = MarkerSpec { start: None, end: Some(MarkerEnd { shape: MarkerShape::Triangle, scale: 100.0 }), align: MarkerAlign::InsidePath };
    let nib = marker_outlines(&path(true), &spec, 2.0, &MarkerOptions::default()).unwrap();
    assert_eq!((nib.closed_subpaths.as_slice(), nib.consumed_subpaths.as_slice()), ([0usize].as_slice(), [2usize].as_slice()));

    let got = prepare(path(true), "arrow.triangle");
    assert_eq!(got.refusal(&layer), None, "the layer still draws");
    let notes = got.marker_notes(&layer);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!((notes[0].row, notes[0].closed_subpaths.as_slice(), notes[0].consumed_subpaths.as_slice()), (0, [(0usize, 0usize)].as_slice(), [(0usize, 2usize)].as_slice()));
    assert_eq!(notes[0].code(), "marker_closed_subpath");
    // Only a consumed subpath: its own code.
    let mut open_only = path(true);
    open_only.elements.drain(..5);
    let got = prepare(open_only, "arrow.triangle");
    let notes = got.marker_notes(&layer);
    assert_eq!((notes.len(), notes[0].code()), (1, "marker_subpath_consumed"), "{notes:?}");
    assert_eq!(notes[0].consumed_subpaths, [(0, 1)]);
    // Fully marked open path: no note. No marker entry: no note either.
    let mut line = path(false);
    line.elements.drain(..5);
    assert!(prepare(line, "arrow.triangle").all_marker_notes().is_empty());
    assert!(prepare(path(true), "none").all_marker_notes().is_empty());
}
