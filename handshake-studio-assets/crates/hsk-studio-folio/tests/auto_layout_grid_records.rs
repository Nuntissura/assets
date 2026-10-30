//! GRID auto-layout records (lane SMB-FIG-GRID-1, STU-DS-145/146/147/150): the `grid` member of
//! `StudioAutoLayout` and the `grid_child` member of `StudioChildLayout`, their wire shape, the
//! byte identity of record sets without them, and the typed refusals of `validate_design_records`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(_) => panic!("case {name} exceeded declared 30-second deadline"),
    }
}
fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn pt(v: f64) -> Value {
    json!({"value": v, "unit": "pt"})
}
fn layer(n: u32, kind: &str, parent: Value) -> Value {
    let payload = match kind {
        "group" => json!({"payload_kind": "group"}),
        _ => json!({"payload_kind": "raster", "tiles": []}),
    };
    json!({"schema_id": "hsk.studio.layer@1", "layer_id": identity("SLYR", n),
        "name": format!("layer {n}"), "parent": parent, "kind": kind, "payload": payload, "visible": true})
}
fn under(n: u32) -> Value {
    json!({"node_kind": "layer", "node_id": identity("SLYR", n)})
}
/// Artboard 1; grid frame 1 with children 2 and 3; horizontal frame 4 with child 5.
fn document() -> StudioDocument {
    let board = json!({"node_kind": "artboard", "node_id": identity("SART", 1)});
    let v = json!({"schema_id": "hsk.studio.document@1", "document_id": identity("SDOC", 1), "revision": 1,
        "geometry_unit": "pt", "colour_mode": 2, "bits_per_channel": 8,
        "working_space_rgb": null, "working_space_cmyk": null, "working_space_gray": null,
        "working_space_spot": null, "blending_space": null,
        "artboards": [{"schema_id": "hsk.studio.artboard@1", "artboard_id": identity("SART", 1),
            "name": "screen", "parent": null,
            "bounds": {"x": pt(0.0), "y": pt(0.0), "width": pt(800.0), "height": pt(600.0)}}],
        "page_spreads": [],
        "layers": [
            layer(1, "group", board.clone()), layer(2, "raster", under(1)), layer(3, "raster", under(1)),
            layer(4, "group", board), layer(5, "raster", under(4))],
        "graph": {"schema_id": "hsk.studio.layer_graph@1", "operations": [], "edges": [], "outputs": []}});
    serde_json::from_value(v).expect("document parses")
}
fn layout(mode: &str) -> Value {
    json!({"schema_id": "hsk.studio.auto_layout@1", "layout_mode": mode, "layout_wrap": "NO_WRAP",
        "primary_axis_sizing_mode": "FIXED", "counter_axis_sizing_mode": "AUTO",
        "primary_axis_align_items": "MIN", "counter_axis_align_items": "MIN",
        "counter_axis_align_content": "AUTO", "item_spacing": 0.0, "counter_axis_spacing": null,
        "padding_top": 20.0, "padding_right": 20.0, "padding_bottom": 20.0, "padding_left": 20.0,
        "strokes_included_in_layout": false, "item_reverse_z_index": false})
}
fn grid() -> Value {
    json!({"grid_row_count": 2, "grid_column_count": 3, "grid_row_gap": 10.0, "grid_column_gap": 10.0,
        "grid_row_sizes": [{"type": "HUG"}, {"type": "HUG"}],
        "grid_column_sizes": [{"type": "FIXED", "value": 100.0}, {"type": "FLEX", "value": 1.0},
            {"type": "FLEX", "value": 2.0}],
        "grid_auto_tracks": "NONE", "grid_items_positioning": "MANUAL"})
}
fn child(grid_child: Option<Value>) -> Value {
    let mut v = json!({"layout_align": "INHERIT", "layout_grow": 0, "layout_positioning": "AUTO",
        "layout_sizing_horizontal": "FIXED", "layout_sizing_vertical": "FIXED"});
    if let Some(g) = grid_child {
        v["grid_child"] = g;
    }
    v
}
fn anchor(row: u32, column: u32, row_span: u32, column_span: u32) -> Value {
    json!({"grid_row_anchor_index": row, "grid_column_anchor_index": column,
        "grid_row_span": row_span, "grid_column_span": column_span})
}
fn node(n: u32) -> Value {
    json!({"node_kind": "layer", "node_id": identity("SLYR", n)})
}
fn records() -> Value {
    let mut grid_layout = layout("GRID");
    grid_layout["grid"] = grid();
    json!({"schema_id": "hsk.studio.design_records@1", "document_id": identity("SDOC", 1),
        "nodes": [
            {"node": node(1), "auto_layout": grid_layout},
            {"node": node(2), "child_layout": child(Some(anchor(0, 1, 2, 2)))},
            {"node": node(3), "child_layout": child(None)},
            {"node": node(4), "auto_layout": layout("HORIZONTAL")},
            {"node": node(5), "child_layout": child(None)}]})
}
fn result_of(r: &StudioDesignRecords) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    validate_design_records(
        r,
        &document(),
        Budget::default(),
        &CancellationToken::default(),
        &Unresolved,
    )
}
fn parse(v: &Value) -> StudioDesignRecords {
    serde_json::from_value(v.clone()).expect("records parse")
}
fn refused_value(v: &Value, code: Code, suffix: &str, label: &str) {
    let d = result_of(&parse(v)).expect_err(label);
    assert_eq!(d.code, code, "{label}: {}", d.target);
    assert!(
        d.target.ends_with(suffix),
        "{label}: target {} lacks {suffix}",
        d.target
    );
}
/// The valid records with each `(pointer, value)` applied, which must be refused.
fn refused(edits: &[(&str, Value)], code: Code, suffix: &str) {
    let mut v = records();
    for (pointer, value) in edits {
        let (parent, last) = pointer.rsplit_once('/').expect("pointer");
        match v.pointer_mut(parent).expect("parent") {
            Value::Array(a) => a[last.parse::<usize>().expect("index")] = value.clone(),
            Value::Object(o) => {
                o.insert(last.to_owned(), value.clone());
            }
            _ => panic!("pointer parent is a scalar"),
        }
    }
    refused_value(&v, code, suffix, edits[0].0);
}

#[test]
fn grid_record_round_trips_through_wire_json() {
    bounded_case("grid_record_round_trips_through_wire_json", || {
        let v = records();
        let r = parse(&v);
        assert_eq!(result_of(&r), Ok(vec![]));
        let g = r.nodes[0].auto_layout.as_ref().unwrap().grid.as_ref().unwrap();
        assert_eq!((g.grid_row_count, g.grid_column_count), (2, 3));
        assert_eq!(g.grid_column_sizes[0], StudioGridTrackSize::fixed(100.0));
        assert_eq!(g.grid_column_sizes[2], StudioGridTrackSize::flex(2.0));
        assert_eq!(g.grid_row_sizes[1], StudioGridTrackSize::hug());
        assert_eq!(g.grid_items_positioning, GridItemsPositioning::Manual);
        assert_eq!(
            r.nodes[1].child_layout.as_ref().unwrap().grid_child,
            Some(StudioGridChild {
                grid_row_anchor_index: 0,
                grid_column_anchor_index: 1,
                grid_row_span: 2,
                grid_column_span: 2,
            })
        );
        // Wire JSON in, the same JSON out; a HUG track has no `value` member on the wire.
        assert_eq!(serde_json::to_value(&r).unwrap(), v);
        let bytes = serde_json::to_vec(&r).unwrap();
        assert_eq!(serde_json::from_slice::<StudioDesignRecords>(&bytes).unwrap(), r);
        // The STU-DS-146 defaults.
        let d = StudioGridLayout::new();
        assert_eq!((d.grid_row_count, d.grid_column_count), (1, 1));
        assert_eq!(d.grid_row_sizes, vec![StudioGridTrackSize::hug()]);
        assert_eq!(d.grid_auto_tracks, GridAutoTracks::None);
        assert_eq!(d.grid_items_positioning, GridItemsPositioning::RowAutoFlow);
        // Unknown members stay refused at decode.
        let mut extra = grid();
        extra["grid_child_index"] = json!(0);
        assert!(serde_json::from_value::<StudioGridLayout>(extra).is_err());
    });
}

#[test]
fn non_grid_records_encode_byte_identically() {
    bounded_case("non_grid_records_encode_byte_identically", || {
        // The bytes a flow container and a flow child encoded to before the grid members existed.
        let before = concat!(
            r#"{"schema_id":"hsk.studio.auto_layout@1","layout_mode":"HORIZONTAL","layout_wrap":"NO_WRAP","#,
            r#""primary_axis_sizing_mode":"FIXED","counter_axis_sizing_mode":"AUTO","#,
            r#""primary_axis_align_items":"MIN","counter_axis_align_items":"MIN","#,
            r#""counter_axis_align_content":"AUTO","item_spacing":0.0,"counter_axis_spacing":null,"#,
            r#""padding_top":20.0,"padding_right":20.0,"padding_bottom":20.0,"padding_left":20.0,"#,
            r#""strokes_included_in_layout":false,"item_reverse_z_index":false}"#
        );
        let a: StudioAutoLayout = serde_json::from_str(before).unwrap();
        assert_eq!(a.grid, None);
        assert_eq!(serde_json::to_string(&a).unwrap(), before);
        let child_before = concat!(
            r#"{"layout_align":"INHERIT","layout_grow":0,"layout_positioning":"AUTO","#,
            r#""layout_sizing_horizontal":"FIXED","layout_sizing_vertical":"FIXED"}"#
        );
        let k: StudioChildLayout = serde_json::from_str(child_before).unwrap();
        assert_eq!(k.grid_child, None);
        assert_eq!(serde_json::to_string(&k).unwrap(), child_before);
        // A whole record set without grid members: decode then encode returns the same bytes.
        let mut v = records();
        v["nodes"] = json!([
            {"node": node(4), "auto_layout": layout("HORIZONTAL")},
            {"node": node(5), "child_layout": child(None)}]);
        let r = parse(&v);
        assert_eq!(result_of(&r), Ok(vec![]));
        assert_eq!(serde_json::to_value(&r).unwrap(), v);
        let bytes = serde_json::to_vec(&r).unwrap();
        let again: StudioDesignRecords = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(serde_json::to_vec(&again).unwrap(), bytes);
        assert!(!String::from_utf8(bytes).unwrap().contains("grid"));
        assert_eq!(StudioAutoLayout::none().grid, None);
    });
}

#[test]
fn grid_container_refusals_are_typed() {
    bounded_case("grid_container_refusals_are_typed", || {
        let g = "/nodes/0/auto_layout/grid";
        // `grid` absent under GRID, present under HORIZONTAL.
        let mut absent = records();
        absent["nodes"][0]["auto_layout"]
            .as_object_mut()
            .unwrap()
            .remove("grid");
        refused_value(&absent, Code::MissingField, "/auto_layout/grid", "absent");
        refused(
            &[("/nodes/3/auto_layout/grid", grid())],
            Code::InvalidField,
            "/auto_layout/grid",
        );
        // Track lists must match the counts.
        refused(
            &[(&format!("{g}/grid_row_count"), json!(3))],
            Code::InvalidField,
            "/grid/grid_row_sizes",
        );
        refused(
            &[(&format!("{g}/grid_column_sizes"), json!([{"type": "HUG"}]))],
            Code::InvalidField,
            "/grid/grid_column_sizes",
        );
        // FLEX without a value, HUG with a value, FIXED below 0.
        refused(
            &[(&format!("{g}/grid_column_sizes/1"), json!({"type": "FLEX"}))],
            Code::InvalidField,
            "/grid/grid_column_sizes/1",
        );
        refused(
            &[(&format!("{g}/grid_row_sizes/0"), json!({"type": "HUG", "value": 4.0}))],
            Code::InvalidField,
            "/grid/grid_row_sizes/0",
        );
        refused(
            &[(&format!("{g}/grid_column_sizes/0"), json!({"type": "FIXED", "value": -1.0}))],
            Code::InvalidField,
            "/grid/grid_column_sizes/0",
        );
        // Gaps: hard_min 0, studio_declared hard_max 100000.
        for (field, v) in [("grid_row_gap", -1.0), ("grid_column_gap", 100001.0)] {
            refused(
                &[(&format!("{g}/{field}"), json!(v))],
                Code::InvalidField,
                &format!("/grid/{field}"),
            );
        }
        // Counts: hard_min 1, studio_declared hard_max 1000.
        refused(
            &[(&format!("{g}/grid_row_count"), json!(0))],
            Code::InvalidField,
            "/grid/grid_row_count",
        );
        refused(
            &[(&format!("{g}/grid_column_count"), json!(1001))],
            Code::InvalidField,
            "/grid/grid_column_count",
        );
        // WRAP stays a flow-only field.
        refused(
            &[("/nodes/0/auto_layout/layout_wrap", json!("WRAP"))],
            Code::InvalidField,
            "/auto_layout/layout_wrap",
        );
        // NaN cannot cross the wire; a caller-built record is refused just the same.
        let mut r = parse(&records());
        r.nodes[0].auto_layout.as_mut().unwrap().grid.as_mut().unwrap().grid_column_sizes[1] =
            StudioGridTrackSize::flex(f64::NAN);
        let d = result_of(&r).unwrap_err();
        assert_eq!(d.code, Code::InvalidField);
        assert!(d.target.ends_with("/grid/grid_column_sizes/1"), "{}", d.target);
    });
}

#[test]
fn grid_child_refusals_are_typed() {
    bounded_case("grid_child_refusals_are_typed", || {
        let gc = "/nodes/1/child_layout/grid_child";
        // A span past the declared count is a validation error (STU-DS-150).
        refused(
            &[(gc, anchor(1, 1, 2, 1))],
            Code::InvalidField,
            "/grid_child/grid_row_span",
        );
        refused(
            &[(gc, anchor(0, 2, 1, 2))],
            Code::InvalidField,
            "/grid_child/grid_column_span",
        );
        refused(
            &[(gc, anchor(0, 0, 0, 1))],
            Code::InvalidField,
            "/grid_child/grid_row_span",
        );
        refused(
            &[(gc, anchor(2, 0, 1, 1))],
            Code::InvalidField,
            "/grid_child/grid_row_anchor_index",
        );
        refused(
            &[(gc, anchor(0, 3, 1, 1))],
            Code::InvalidField,
            "/grid_child/grid_column_anchor_index",
        );
        // u32::MAX span cannot wrap the anchor + span sum.
        refused(
            &[(gc, anchor(1, 0, u32::MAX, 1))],
            Code::InvalidField,
            "/grid_child/grid_row_span",
        );
        // grid_child under a flow parent, and on an ABSOLUTE grid child.
        refused(
            &[("/nodes/4/child_layout", child(Some(anchor(0, 0, 1, 1))))],
            Code::InvalidField,
            "/child_layout/grid_child",
        );
        refused(
            &[("/nodes/1/child_layout/layout_positioning", json!("ABSOLUTE"))],
            Code::InvalidField,
            "/child_layout/grid_child",
        );
    });
}

#[test]
fn hostile_grid_sizes_are_refused_before_entries_are_read() {
    bounded_case("hostile_grid_sizes_are_refused_before_entries_are_read", || {
        // 1000x1000 counts are legal; track lists far longer than the counts are refused on the
        // length comparison, before any entry is examined.
        let mut r = parse(&records());
        let g = r.nodes[0].auto_layout.as_mut().unwrap().grid.as_mut().unwrap();
        g.grid_row_count = 1000;
        g.grid_column_count = 1000;
        g.grid_row_sizes = vec![StudioGridTrackSize::hug(); 1000];
        g.grid_column_sizes = vec![StudioGridTrackSize::fixed(1.0); 1000];
        assert_eq!(result_of(&r), Ok(vec![]));
        let g = r.nodes[0].auto_layout.as_mut().unwrap().grid.as_mut().unwrap();
        g.grid_column_sizes = vec![StudioGridTrackSize::flex(f64::NAN); 1_000_000];
        let d = result_of(&r).unwrap_err();
        assert_eq!(d.code, Code::InvalidField);
        assert!(d.target.ends_with("/grid/grid_column_sizes"), "{}", d.target);
        // u32::MAX counts and spans are refused, never summed.
        let g = r.nodes[0].auto_layout.as_mut().unwrap().grid.as_mut().unwrap();
        g.grid_column_count = u32::MAX;
        assert!(result_of(&r).unwrap_err().target.ends_with("/grid/grid_column_count"));
    });
}
