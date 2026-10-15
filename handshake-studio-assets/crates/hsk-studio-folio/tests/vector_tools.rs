//! Lane SMB-ILLUSTRATOR-TOOLS-2: swatches and swatch bindings, brushes and brush strokes,
//! envelopes in `StudioVectorRecords` (STU-COL-170..174, STU-VEC-150/152/162/163, STU-VEC-168/169).
//! Absent members encode byte-identically; a full set round-trips; hostile records are refused
//! with the code of the rule named next to them.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn layer(n: u32, kind: &str, payload: Value) -> Value {
    json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",n),"name":format!("l{n}"),
        "parent":null,"kind":kind,"payload":payload,"visible":true})
}
/// Layers 1, 2, 3 and 5 vector, 4 raster.
fn document() -> StudioDocument {
    let v = json!({"payload_kind":"vector","path_ids":[identity("SVPT",1)]});
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[layer(1,"vector",v.clone()),layer(2,"vector",v.clone()),layer(3,"vector",v.clone()),layer(4,"raster",json!({"payload_kind":"raster","tiles":[]})),layer(5,"vector",v)],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}
fn colour() -> Value {
    json!({"r": 0.5, "g": 0.25, "b": 1.0, "colour_profile_id": null})
}
/// Style 1: a fill row and a stroke row.
fn design() -> StudioDesignRecords {
    let fill = json!({"row_kind":"fill","paint":{"paint_kind":"solid","colour":colour()},"opacity":100.0,"blend_mode":2,"visible":true});
    let stroke = json!({"row_kind":"stroke","paint":{"paint_kind":"solid","colour":colour()},
        "stroke":{"weight":{"value":2.0,"unit":"px"},"cap":"butt","join":"miter","align":"CENTER","miter_limit":4.0,"dash":[],"dash_offset":{"value":0.0,"unit":"px"}},
        "opacity":100.0,"blend_mode":2,"visible":true});
    serde_json::from_value(json!({"schema_id":"hsk.studio.design_records@1","document_id":identity("SDOC",1),
        "paint_styles":[{"schema_id":"hsk.studio.style_registry@1","style_id":identity("SSTY",1),"kind":"PAINT","name":"s1",
        "publishing":{"description":"","description_markdown":"","documentation_links":[],"publish_key":"s1","is_remote":false},"rows":[fill, stroke]}]}))
    .expect("design shape")
}
fn param(v: f64) -> Value {
    json!({"value": v})
}
fn art(n: u32) -> Value {
    json!({"path_id": identity("SVPT", n), "colour": colour()})
}
fn full() -> Value {
    json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1),
        "swatches":[
            {"schema_id":"hsk.studio.swatch@1","swatch_id":identity("SSWT",1),"name":"Brand CMYK","swatch_kind":"global","color_model":"CMYK","components":[0.1,0.9,0.0,0.05],"profile_ref":identity("SCPF",1)},
            {"schema_id":"hsk.studio.swatch@1","swatch_id":identity("SSWT",2),"name":"Lab red","swatch_kind":"process","color_model":"LAB","components":[54.0,80.0,67.0]},
            {"schema_id":"hsk.studio.swatch@1","swatch_id":identity("SSWT",3),"name":"Spot 1","swatch_kind":"spot","spot":{"ink_name":"Ink 1","measured_lab":[60.0,-20.0,30.0],"alternate_space":"CMYK","alternate_color_value":[0.5,0.0,1.0,0.0]}},
            {"schema_id":"hsk.studio.swatch@1","swatch_id":identity("SSWT",4),"name":"Spot 1 40%","swatch_kind":"tint","base_swatch_ref":identity("SSWT",3),"tint_value":40.0},
            {"schema_id":"hsk.studio.swatch@1","swatch_id":identity("SSWT",5),"name":"Fade","swatch_kind":"gradient","gradient":{"geometry":{"gradient_kind":"linear","x1":0.0,"y1":0.0,"x2":1.0,"y2":0.0},"transform":[1.0,0.0,0.0,1.0,0.0,0.0],"spread":"pad","interpolation":"classic","dither":false,
                "stops":[{"position":0.0,"midpoint":null,"opacity":100.0,"colour":colour()},{"position":100.0,"midpoint":null,"opacity":100.0,"colour":colour()}]}}],
        "swatch_paints":[{"style_id":identity("SSTY",1),"row":0,"swatch_id":identity("SSWT",3),"tint":50.0}],
        "brushes":[
            {"brush_id":"leaf","name":"Leaf","colorization":"none","kind":{"brush_kind":"art","art_width":param(100.0),"art_scale_mode":"stretch_between_guides","art_guide_start":2.0,"art_guide_end":8.0,"flip_along":false,"flip_across":true,"art":[art(2),art(3)]}},
            {"brush_id":"dots","name":"Dots","colorization":"tints","kind":{"brush_kind":"scatter","scatter_size":{"value":50.0,"dynamics":"random","max":100.0},"scatter_spacing":param(100.0),"scatter_offset":param(0.0),"scatter_rotation":param(0.0),"scatter_rotation_relative_to":"path","seed":7,"art":[art(4)]}},
            {"brush_id":"rope","name":"Rope","colorization":"none","kind":{"brush_kind":"pattern","pattern_brush_scale":param(100.0),"fit_mode":"add_space_to_fit","flip_along":false,"flip_across":false,"show_auto_generated_corner_tiles":false,"tiles":{"side":[art(5)],"outer_corner":[art(6)],"end":[art(7)]}}},
            {"brush_id":"nib","name":"Nib","colorization":"none","kind":{"brush_kind":"calligraphic","nib_angle":param(30.0),"nib_roundness":param(60.0),"nib_size":param(4.0)}}],
        "brush_strokes":[{"style_id":identity("SSTY",1),"row":1,"brush_id":"rope","scale_override":50.0}],
        "envelopes":[
            {"layer_id":identity("SLYR",1),"envelope":{"envelope_kind":"warp","warp_style":"flag","warp_axis":"vertical","warp_bend":0.42,"warp_distortion_horizontal":-0.29,"warp_distortion_vertical":0.27},"envelope_fidelity":50.0},
            {"layer_id":identity("SLYR",2),"envelope":{"envelope_kind":"mesh","rows":1,"columns":2,"points":[[0.0,0.0],[5.0,0.0],[10.0,0.0],[0.0,10.0],[5.0,12.0],[10.0,10.0]]},"envelope_fidelity":100.0,"distort_appearance":true},
            {"layer_id":identity("SLYR",3),"envelope":{"envelope_kind":"top_object","source_layer_id":identity("SLYR",5)},"envelope_fidelity":0.0}]})
}

fn check(v: &Value) -> Result<(), Code> {
    let r: StudioVectorRecords = serde_json::from_value(v.clone()).map_err(|_| Code::InvalidField)?;
    validate_vector_records(&r, &document(), Some(&design()), Budget::default(), &CancellationToken::default()).map_err(|d| d.code)
}

#[test]
fn tool_records_round_trip_byte_identically_and_refuse_hostile_input() {
    // Absent members: the bytes of a record set written before them.
    let before = format!(r#"{{"schema_id":"hsk.studio.vector_records@1","document_id":"{}"}}"#, identity("SDOC", 1));
    let r: StudioVectorRecords = serde_json::from_str(&before).unwrap();
    assert_eq!(serde_json::to_string(&r).unwrap(), before);
    // The full set validates and round-trips value for value.
    let f = full();
    assert_eq!(check(&f), Ok(()));
    let r: StudioVectorRecords = serde_json::from_value(f.clone()).unwrap();
    assert_eq!(serde_json::to_value(&r).unwrap(), f);
    assert_eq!(r.swatch_paint(&identity("SSTY", 1), 0).map(|p| p.swatch_id.clone()), Some(identity("SSWT", 3)));
    assert_eq!(r.brush("rope").map(|b| b.art().len()), Some(3));
    assert!(r.envelope(&identity("SLYR", 3)).is_some());
    // Hostile edits, one rule each.
    let bad = |path: &[&str], v: Value| {
        let mut x = full();
        let mut at = &mut x;
        for k in &path[..path.len() - 1] {
            at = match k.parse::<usize>() {
                Ok(i) => &mut at[i],
                Err(_) => &mut at[*k],
            };
        }
        let last = path[path.len() - 1];
        match last.parse::<usize>() {
            Ok(i) => at[i] = v,
            Err(_) => at[last] = v,
        }
        check(&x)
    };
    assert_eq!(bad(&["swatches", "0", "components"], json!([0.1, 0.9, 0.0])), Err(Code::InvalidField), "CMYK takes four components");
    assert_eq!(bad(&["swatches", "1", "components"], json!([120.0, 0.0, 0.0])), Err(Code::InvalidField), "L* above 100");
    assert_eq!(bad(&["swatches", "2", "spot", "alternate_color_value"], json!([0.5])), Err(Code::InvalidField), "alternate value of its space");
    assert_eq!(bad(&["swatches", "3", "base_swatch_ref"], json!(identity("SSWT", 5))), Err(Code::InvalidField), "a tint tracks a colour swatch");
    assert_eq!(bad(&["swatches", "3", "base_swatch_ref"], json!(identity("SSWT", 9))), Err(Code::DanglingReference));
    assert_eq!(bad(&["swatches", "1", "swatch_id"], json!(identity("SSWT", 1))), Err(Code::DuplicateIdentity));
    assert_eq!(bad(&["swatch_paints", "0", "row"], json!(5)), Err(Code::DanglingReference));
    assert_eq!(bad(&["brushes", "1", "kind", "scatter_size"], json!({"value": 0.0})), Err(Code::InvalidField), "scatter size 1..10000");
    assert_eq!(bad(&["brushes", "1", "kind", "scatter_size"], json!({"value": 50.0, "dynamics": "random"})), Err(Code::InvalidField), "non-fixed dynamics keep a max");
    assert_eq!(bad(&["brushes", "2", "kind", "tiles"], json!({"start": [art(5)]})), Err(Code::InvalidField), "a pattern brush needs a side tile");
    assert_eq!(bad(&["brush_strokes", "0", "row"], json!(0)), Err(Code::InvalidField), "brush strokes bind stroke rows");
    assert_eq!(bad(&["brush_strokes", "0", "brush_id"], json!("nope")), Err(Code::DanglingReference));
    assert_eq!(bad(&["envelopes", "1", "envelope", "points"], json!([[0.0, 0.0]])), Err(Code::InvalidField), "(rows + 1)(columns + 1) points");
    assert_eq!(bad(&["envelopes", "0", "envelope_fidelity"], json!(101.0)), Err(Code::InvalidField));
    assert_eq!(bad(&["envelopes", "0", "envelope", "warp_bend"], json!(1.5)), Err(Code::InvalidField), "stored as a ratio");
    assert_eq!(bad(&["envelopes", "2", "envelope", "source_layer_id"], json!(identity("SLYR", 1))), Err(Code::InvalidField), "a top object is not wrapped");
    assert_eq!(bad(&["envelopes", "0", "layer_id"], json!(identity("SLYR", 4))), Err(Code::PayloadMismatch), "vector layers only");
    let mut many = full();
    many["swatches"] = Value::Array(vec![json!({}); TOOLS_MAX_ENTRIES + 1]);
    assert!(check(&many).is_err(), "over the entry budget");
}
