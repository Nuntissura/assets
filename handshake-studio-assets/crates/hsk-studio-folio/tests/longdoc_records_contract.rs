//! SMB-LAYOUT-LONGDOC: `hsk.studio.longdoc_records@1`. Byte-identical wire round trip (absent and
//! empty members are not encoded) and the refusal codes of `validate_longdoc_records`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
struct Caller;
impl Resolver for Caller {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Available
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        true
    }
}
fn wire_identical<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(v: &T) -> T {
    let first = serde_json::to_vec(v).expect("encode");
    let back: T = serde_json::from_slice(&first).expect("decode");
    assert_eq!(&back, v);
    assert_eq!(serde_json::to_vec(&back).expect("encode again"), first);
    back
}
fn pt(v: f64) -> Value {
    json!({"value": v, "unit": "pt"})
}
fn page(key: &str) -> Value {
    let e = json!({"top": pt(36.0), "right": pt(36.0), "bottom": pt(36.0), "left": pt(36.0)});
    json!({"page_id": key, "width": pt(612.0), "height": pt(792.0), "side": "right_hand", "margins": e, "bleed": null})
}
fn document() -> StudioDocument {
    let layers: Vec<Value> = (1..=2)
        .map(|n| {
            json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR", n),"name":format!("l{n}"),
                "parent":null,"kind":"raster","payload":{"payload_kind":"raster","tiles":[]},"visible":true})
        })
        .collect();
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"pt",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],
        "page_spreads":[{"schema_id":"hsk.studio.page_spread@1","page_spread_id":identity("SPGS",1),
            "name":"spread 1","parent":null,
            "bounds":{"x":pt(0.0),"y":pt(0.0),"width":pt(1224.0),"height":pt(792.0)},
            "pages":[page("p1"),page("p2"),page("p3")]}],
        "layers":layers,
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}
fn validate(v: &Value) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    let r: StudioLongDocRecords = serde_json::from_value(v.clone()).expect("record shape");
    validate_longdoc_records(&r, &document(), Budget::default(), &CancellationToken::default(), &Caller)
}
fn code_of(v: &Value) -> (Code, String) {
    let d = validate(v).expect_err("refused");
    (d.code, d.target)
}
fn cell(row: u32, column: u32) -> Value {
    let e = json!({"top": pt(2.0), "right": pt(2.0), "bottom": pt(2.0), "left": pt(2.0)});
    json!({"row": row, "column": column, "insets": e, "vertical_justification": "top"})
}
fn table() -> Value {
    let row = |t: &str| json!({"row_type": t, "minimum_height": pt(10.0), "maximum_height": pt(100.0)});
    let red = json!({"r": 1.0, "g": 0.0, "b": 0.0, "colour_profile_id": null});
    let mut c00 = cell(0, 0);
    c00["column_span"] = json!(2);
    c00["content"] = json!({"start": 0, "end": 4});
    c00["fill"] = red.clone();
    c00["fill_tint"] = json!(40.0);
    c00["strokes"] = json!({"bottom": {"weight": pt(1.0), "colour": red, "tint": 100.0}});
    c00["stroke_priorities"] = json!({"bottom": 3});
    let mut body = row("body");
    body["keep_with_next_row"] = json!(true);
    body["start_row"] = json!("next_odd_page");
    body["allow_split"] = json!(true);
    json!({"table_id": "t1", "story_id": identity("STXT", 1), "anchor": 5, "cell_story_id": identity("STXT", 2),
        "column_widths": [pt(50.0), pt(60.0)], "rows": [row("header"), body, row("footer")],
        "cells": [c00, cell(1, 0), cell(1, 1), cell(2, 0), cell(2, 1)],
        "break_headers": "once_per_page", "break_footers": "once_per_text_frame",
        "skip_first_header": true, "stroke_order": "column_on_top"})
}
fn records(tables: Vec<Value>) -> Value {
    json!({"schema_id": "hsk.studio.longdoc_records@1", "document_id": identity("SDOC", 1), "tables": tables})
}

/// Slice 1 (tables): wire round trip, empty set encoding, and the table refusals.
#[test]
fn longdoc_tables_round_trip_and_refusals() {
    let empty = StudioLongDocRecords::new(identity("SDOC", 1));
    let bytes = serde_json::to_string(&wire_identical(&empty)).unwrap();
    assert_eq!(bytes, format!(r#"{{"schema_id":"hsk.studio.longdoc_records@1","document_id":"{}"}}"#, identity("SDOC", 1)));
    let v = records(vec![table()]);
    let r: StudioLongDocRecords = serde_json::from_value(v.clone()).unwrap();
    let back = wire_identical(&r);
    assert_eq!(serde_json::to_value(&back).unwrap()["tables"][0]["cells"][1], cell(1, 0), "defaults are not encoded");
    assert_eq!(back.tables[0].cells[0].column_span, 2);
    assert_eq!(back.tables[0].rows[1].start_row, TableStartRow::NextOddPage);
    assert!(validate(&v).unwrap().is_empty());

    let mut over = table();
    over["cells"][4]["row_span"] = json!(u32::MAX);
    assert_eq!(code_of(&records(vec![over])), (Code::InvalidField, "t1/span_overflow".into()));
    let mut overlap = table();
    overlap["cells"][1]["column_span"] = json!(2);
    assert_eq!(code_of(&records(vec![overlap])), (Code::InvalidField, "t1/cells_overlap".into()));
    let mut order = table();
    order["rows"][0]["row_type"] = json!("footer");
    assert_eq!(code_of(&records(vec![order])), (Code::InvalidField, "t1/row_order".into()));
    let mut tint = table();
    tint["cells"][0]["fill_tint"] = json!(100.5);
    assert_eq!(code_of(&records(vec![tint])).0, Code::InvalidField);
    let mut cyc = table();
    cyc["cell_story_id"] = json!(identity("STXT", 1));
    assert_eq!(code_of(&records(vec![cyc])).0, Code::RenderCycle);
    let mut huge = table();
    huge["column_widths"] = json!(vec![pt(1.0); MAX_TABLE_COLUMNS + 1]);
    assert_eq!(code_of(&records(vec![huge])).0, Code::BudgetExceeded);
    assert_eq!(code_of(&records(vec![table(), table()])), (Code::DuplicateIdentity, "t1".into()));
    let mut unknown = table();
    unknown["cells"][0]["surprise"] = json!(1);
    assert!(serde_json::from_value::<StudioLongDocRecords>(records(vec![unknown])).is_err());
}

fn graphic(layer: u32) -> Value {
    json!({"layer_id": identity("SLYR", layer), "page_id": "p2",
        "bounds": {"x": pt(10.0), "y": pt(20.0), "width": pt(80.0), "height": pt(40.0)},
        "content_width": pt(200.0), "content_height": pt(100.0)})
}
fn graphics(list: Vec<Value>) -> Value {
    json!({"schema_id": "hsk.studio.longdoc_records@1", "document_id": identity("SDOC", 1), "graphic_frames": list})
}

/// Slice 2 (placed graphics): a graphic frame with every member round-trips byte-identically, a
/// minimal one encodes only its required members (centre anchor, rectangle and no crop are not
/// written), and the frame refusals.
#[test]
fn longdoc_graphic_frames_round_trip_and_refusals() {
    let mut full = graphic(1);
    full["shape"] = json!({"shape_kind": "path", "path_id": identity("SVPT", 1)});
    full["fit"] = json!("fill_proportionally");
    full["frame_fitting"] = json!("proportionally");
    full["anchor"] = json!("bottom_right");
    full["crop"] = json!({"top": pt(1.0), "right": pt(-2.0), "bottom": pt(3.0), "left": pt(4.0)});
    let v = graphics(vec![full, graphic(2)]);
    let r: StudioLongDocRecords = serde_json::from_value(v.clone()).unwrap();
    let back = wire_identical(&r);
    assert_eq!(serde_json::to_value(&back).unwrap()["graphic_frames"][1], graphic(2));
    assert_eq!(back.graphic_frames[0].anchor, StudioFitAnchor::BottomRight);
    assert_eq!(back.graphic_frame(&identity("SLYR", 2)).unwrap().shape, StudioFrameShape::Rectangle);
    assert!(validate(&v).unwrap().is_empty());

    let mut missing = graphic(9);
    missing["page_id"] = json!("p1");
    assert_eq!(code_of(&graphics(vec![missing])).0, Code::DanglingReference);
    let mut page = graphic(1);
    page["page_id"] = json!("p9");
    assert_eq!(code_of(&graphics(vec![page])).0, Code::DanglingReference);
    let mut empty = graphic(1);
    empty["content_width"] = pt(0.0);
    assert_eq!(code_of(&graphics(vec![empty])).0, Code::InvalidField);
    let mut eaten = graphic(1);
    eaten["crop"] = json!({"top": pt(0.0), "right": pt(100.0), "bottom": pt(0.0), "left": pt(100.0)});
    assert_eq!(code_of(&graphics(vec![eaten])).0, Code::InvalidField);
    let mut merge_only = graphic(1);
    merge_only["fit"] = json!("honour_existing_style");
    assert_eq!(code_of(&graphics(vec![merge_only])).0, Code::InvalidField);
    let mut bad_path = graphic(1);
    bad_path["shape"] = json!({"shape_kind": "path", "path_id": identity("SLYR", 1)});
    assert_eq!(code_of(&graphics(vec![bad_path])).0, Code::InvalidId);
    assert_eq!(code_of(&graphics(vec![graphic(1), graphic(1)])).0, Code::DuplicateIdentity);
}

fn sec(id: &str, start: &str) -> Value {
    json!({"section_id": id, "name": format!("Section {id}"), "page_start": start, "page_number_style": "arabic"})
}
fn toc_style() -> Value {
    json!({"toc_id": "toc1", "name": "Contents", "title": "Contents", "title_style": identity("STYS", 9),
        "entries": [
            {"paragraph_style": identity("STYS", 1), "format_style": identity("STYS", 11), "level": 1, "separator": " "},
            {"paragraph_style": identity("STYS", 2), "level": 2, "separator": "\t", "page_number_position": "before", "sort_alphabet": true}
        ]})
}
fn numbering(sections: Vec<Value>, tocs: Vec<Value>) -> Value {
    json!({"schema_id": "hsk.studio.longdoc_records@1", "document_id": identity("SDOC", 1), "sections": sections, "toc_styles": tocs})
}

/// Slice 3 (sections, numbering, TOC): round trip (continue_numbering true and the `after`
/// position are defaults and not encoded) and the STU-LAY-108 / STU-LAY-148 refusals with codes.
#[test]
fn longdoc_sections_and_toc_styles_round_trip_and_refusals() {
    let mut b = sec("b", "p2");
    b["continue_numbering"] = json!(false);
    b["page_number_start"] = json!(5);
    b["page_number_style"] = json!("lower_roman");
    b["include_section_prefix"] = json!(true);
    b["section_prefix"] = json!("App-");
    b["marker"] = json!("Appendix");
    let v = numbering(vec![sec("a", "p1"), b], vec![toc_style()]);
    let r: StudioLongDocRecords = serde_json::from_value(v.clone()).unwrap();
    let back = wire_identical(&r);
    let wire = serde_json::to_value(&back).unwrap();
    assert_eq!(wire["sections"][0], sec("a", "p1"));
    assert_eq!(wire["toc_styles"][0], toc_style());
    assert!(back.sections[0].continue_numbering);
    assert_eq!(back.toc_styles[0].entries[0].page_number_position, TocPageNumberPosition::After);
    assert!(validate(&v).unwrap().is_empty());

    let with = |f: &dyn Fn(&mut Value)| {
        let mut v = numbering(vec![sec("a", "p1"), sec("b", "p2")], vec![toc_style()]);
        f(&mut v);
        code_of(&v)
    };
    assert_eq!(with(&|v| v["sections"][1]["section_prefix"] = json!("ABCDEF")), (Code::InvalidField, "sections/section_prefix_too_long".into()));
    assert_eq!(
        with(&|v| {
            v["sections"][0]["section_prefix"] = json!("X");
            v["sections"][1]["section_prefix"] = json!("X");
        }),
        (Code::DuplicateIdentity, "sections/section_prefix_duplicate".into())
    );
    assert_eq!(with(&|v| v["sections"][1]["page_start"] = json!("p9")), (Code::DanglingReference, "sections/section_page_missing".into()));
    assert_eq!(with(&|v| v["sections"][1]["page_number_start"] = json!(3)), (Code::InvalidField, "sections/page_number_start_while_continuing".into()));
    assert_eq!(with(&|v| v["sections"][0]["page_start"] = json!("p3")), (Code::InvalidField, "sections/first_page_without_section".into()));
    assert_eq!(with(&|v| v["toc_styles"][0]["entries"][1]["paragraph_style"] = json!(identity("STYS", 1))), (Code::DuplicateIdentity, "toc1/toc_style_mapped_twice".into()));
    assert_eq!(with(&|v| v["toc_styles"][0]["entries"] = json!([])), (Code::InvalidField, "toc1/toc_no_entries".into()));
    assert_eq!(with(&|v| v["toc_styles"][0]["title_style"] = json!(identity("SLYR", 1))).0, Code::InvalidId);
    assert_eq!(with(&|v| v["toc_styles"][0]["entries"][0]["level"] = json!(0)).0, Code::InvalidField);
}

fn notes_and_index() -> Value {
    let black = json!({"r": 0.0, "g": 0.0, "b": 0.0, "colour_profile_id": null});
    json!({"schema_id": "hsk.studio.longdoc_records@1", "document_id": identity("SDOC", 1),
        "footnotes": [
            {"note_id": "n1", "story_id": identity("STXT", 1), "anchor": 5, "note_story_id": identity("STXT", 2)},
            {"note_id": "n2", "story_id": identity("STXT", 1), "anchor": 9, "note_story_id": identity("STXT", 3)}
        ],
        "footnote_options": {"numbering_style": "symbols", "start_at": 1, "restart": "page", "suffix": ")",
            "space_between": pt(2.0), "spacer": pt(3.0),
            "rule": {"weight": pt(0.5), "colour": black, "tint": 100.0, "left_indent": pt(0.0), "width": pt(72.0), "offset": pt(1.0)},
            "no_splitting": true},
        "index_marks": [
            {"mark_id": "m1", "story_id": identity("STXT", 1), "at": 3, "topics": ["Apple", "Green"], "sort_by": ["apple", "green"]},
            {"mark_id": "m2", "story_id": identity("STXT", 1), "at": 7, "topics": ["Pear"],
                "xref": {"kind": "custom_after", "target": ["Apple"], "custom_text": "compare"}}
        ],
        "index_options": {"title": "Index", "index_format": "run_in", "include_section_headings": true,
            "level_styles": [identity("STYS", 1), null],
            "following_topic_separator": ", ", "between_entries_separator": "; ", "page_range_separator": "-",
            "between_page_numbers_separator": ", ", "before_cross_reference_separator": ". ", "entry_end_separator": ""}
    })
}

/// Slice 4 (footnotes, index): round trip (prefix, eos/no-splitting defaults, sort_by and xref
/// absence are not encoded) and the footnote / index refusals with their Folio codes.
#[test]
fn longdoc_footnotes_and_index_round_trip_and_refusals() {
    let v = notes_and_index();
    let r: StudioLongDocRecords = serde_json::from_value(v.clone()).unwrap();
    let back = wire_identical(&r);
    let wire = serde_json::to_value(&back).unwrap();
    assert!(wire["footnote_options"].get("prefix").is_none() && wire["footnote_options"].get("eos_placement").is_none());
    assert_eq!(wire["footnote_options"]["restart"], json!("page"));
    assert_eq!(back.footnote_options.as_ref().unwrap().numbering_style, StudioNumberStyle::Symbols);
    assert_eq!(back.index_marks[1].xref.as_ref().unwrap().kind, IndexXrefKind::CustomAfter);
    assert!(validate(&v).unwrap().is_empty());

    let with = |f: &dyn Fn(&mut Value)| {
        let mut v = notes_and_index();
        f(&mut v);
        code_of(&v)
    };
    assert_eq!(with(&|v| v["footnotes"][1]["note_story_id"] = json!(identity("STXT", 1))), (Code::RenderCycle, "footnotes/footnote_in_footnote".into()));
    assert_eq!(with(&|v| v["footnotes"][1]["story_id"] = json!(identity("STXT", 2))), (Code::RenderCycle, "footnotes/footnote_in_footnote".into()));
    assert_eq!(with(&|v| v["footnotes"][1]["note_story_id"] = json!(identity("STXT", 2))), (Code::DuplicateIdentity, "footnotes/note_story_shared".into()));
    assert_eq!(with(&|v| v["footnote_options"]["start_at"] = json!(401)), (Code::InvalidField, "footnote_options/footnote_start_at_range".into()));
    assert_eq!(with(&|v| v["footnote_options"]["numbering_style"] = json!("triple_leading_zeros")).1, "footnote_options/number_style_not_legal_for_footnotes");
    assert_eq!(with(&|v| v["footnote_options"]["rule"]["weight"] = pt(1000.5)).1, "footnote_options/footnote_rule");
    assert_eq!(with(&|v| v["footnote_options"]["prefix"] = json!("x".repeat(101))).1, "footnote_options/footnote_affix_length");
    assert_eq!(with(&|v| v["index_marks"][0]["topics"] = json!(["a", "b", "c", "d", "e"])).1, "index_marks/index_topic_levels");
    assert_eq!(with(&|v| v["index_marks"][0]["sort_by"] = json!(["a"])).1, "index_marks/index_sort_by_levels");
    assert_eq!(with(&|v| v["index_marks"][1]["xref"]["kind"] = json!("see")).1, "index_marks/index_xref_custom_text");
    assert_eq!(with(&|v| v["index_marks"][1]["mark_id"] = json!("m1")), (Code::DuplicateIdentity, "index_marks/mark_duplicate".into()));
    assert_eq!(
        with(&|v| {
            v["index_options"]["include_section_headings"] = json!(false);
            v["index_options"]["include_empty_index_sections"] = json!(true);
        }),
        (Code::InvalidField, "index_options/index_empty_sections_without_headings".into())
    );
    assert_eq!(with(&|v| v["index_options"]["level_styles"] = json!([null, null, null, null, null])).1, "index_options/index_level_styles");
}
