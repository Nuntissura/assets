//! Lane SMB-ID-TOC-PERSIST-1 item 1: the STU-LAY-148 / STU-LAY-149 contract members persisted in
//! `hsk.studio.longdoc_records@1` (TOC style and entry members, index option members, per-mark
//! page-range scopes, section header sets and the options' header-set reference). The package
//! stores this record set's JSON verbatim, so the byte-identical wire round trip below is the
//! package round trip of every new member; an old-shape record set (written before the members
//! existed) decodes with every new member unknown and re-encodes byte-identically.
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
fn wire_identical<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(v: &T) -> Vec<u8> {
    let first = serde_json::to_vec(v).expect("encode");
    let back: T = serde_json::from_slice(&first).expect("decode");
    assert_eq!(&back, v);
    let again = serde_json::to_vec(&back).expect("encode again");
    assert_eq!(again, first);
    first
}
fn pt(v: f64) -> Value {
    json!({"value": v, "unit": "pt"})
}
fn document() -> StudioDocument {
    let e = json!({"top": pt(36.0), "right": pt(36.0), "bottom": pt(36.0), "left": pt(36.0)});
    let page = |k: &str| json!({"page_id": k, "width": pt(612.0), "height": pt(792.0), "side": "right_hand", "margins": e, "bleed": null});
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"pt",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],
        "page_spreads":[{"schema_id":"hsk.studio.page_spread@1","page_spread_id":identity("SPGS",1),
            "name":"spread 1","parent":null,
            "bounds":{"x":pt(0.0),"y":pt(0.0),"width":pt(1224.0),"height":pt(792.0)},
            "pages":[page("p1"),page("p2")]}],
        "layers":[],
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

/// The old shapes: a TOC style, marks and index options exactly as written before this lane.
fn old_shape() -> Value {
    json!({"schema_id": "hsk.studio.longdoc_records@1", "document_id": identity("SDOC", 1),
        "toc_styles": [{"toc_id": "toc1", "name": "Contents", "title": "Contents", "title_style": identity("STYS", 9),
            "include_overset": true,
            "entries": [{"paragraph_style": identity("STYS", 1), "format_style": identity("STYS", 11), "level": 1, "separator": " "}]}],
        "index_marks": [{"mark_id": "m1", "story_id": identity("STXT", 1), "at": 3, "topics": ["Apple"]}],
        "index_options": {"title": "Index", "level_styles": [identity("STYS", 21)],
            "following_topic_separator": ", ", "between_entries_separator": "; ", "page_range_separator": "-",
            "between_page_numbers_separator": ", ", "before_cross_reference_separator": ". ", "entry_end_separator": ""}
    })
}

/// Every new member set (each scope member once, a pluggable non-Latin header set).
fn full_shape() -> Value {
    let mut v = old_shape();
    let toc = &mut v["toc_styles"][0];
    for (k, b) in [("include_book_documents", false), ("include_hidden", true), ("run_in", true), ("create_bookmarks", true), ("make_anchor", false), ("remove_forced_line_break", false)] {
        toc[k] = json!(b);
    }
    toc["numbered_paragraphs"] = json!("numbered.include_full_paragraph");
    toc["set_story_direction"] = json!("vertical");
    toc["entries"][0]["page_number_style"] = json!(identity("STYS", 41));
    toc["entries"][0]["separator_style"] = json!(identity("STYS", 42));
    let mut default_styled = json!({"toc_id": "toc2", "name": "Default", "title": "", "entries": [], "default_styled": true});
    default_styled["set_story_direction"] = json!("horizontal");
    v["toc_styles"].as_array_mut().unwrap().push(default_styled);
    let scopes = [
        json!({"scope_kind": "current_page"}),
        json!({"scope_kind": "to_next_style_change"}),
        json!({"scope_kind": "to_next_use_of_style", "style": identity("STYS", 5)}),
        json!({"scope_kind": "to_end_of_story"}),
        json!({"scope_kind": "to_end_of_document"}),
        json!({"scope_kind": "to_end_of_section"}),
        json!({"scope_kind": "for_next_paragraphs", "count": 3}),
    ];
    let marks: Vec<Value> = scopes
        .iter()
        .enumerate()
        .map(|(k, s)| json!({"mark_id": format!("s{k}"), "story_id": identity("STXT", 1), "at": k, "topics": ["Scoped"], "page_range_scope": s}))
        .collect();
    v["index_marks"].as_array_mut().unwrap().extend(marks);
    let o = &mut v["index_options"];
    o["replace_existing_index"] = json!(true);
    o["include_book_documents"] = json!(false);
    o["include_hidden_entries"] = json!(false);
    o["page_number_style"] = json!(identity("STYS", 51));
    o["cross_reference_style"] = json!(identity("STYS", 52));
    o["cross_reference_topic_style"] = json!(identity("STYS", 53));
    o["section_header_set"] = json!("Cyrillic set");
    v["section_header_sets"] = json!([
        {"name": "Cyrillic set", "language_id": "ru", "header_type": "header.cyrillic",
            "groups": [{"heading": "Symbols"}, {"heading": "\u{410}", "initials": ["\u{430}"]}, {"heading": "\u{411}", "initials": ["\u{431}"]}]},
        {"name": "Unnamed type", "language_id": "ko", "header_type": "header.captured_member_14", "groups": [{"heading": "*"}]}
    ]);
    v
}

/// Every new member survives the wire (= package) round trip byte-identically, with its kind.
#[test]
fn toc_index_contract_members_round_trip_byte_identical() {
    let v = full_shape();
    let r: StudioLongDocRecords = serde_json::from_value(v.clone()).unwrap();
    let bytes = wire_identical(&r);
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), v, "every member is encoded as written");
    let t = &r.toc_styles[0];
    assert_eq!(
        (t.include_book_documents, t.include_hidden, t.run_in, t.create_bookmarks, t.make_anchor, t.remove_forced_line_break),
        (Some(false), Some(true), Some(true), Some(true), Some(false), Some(false))
    );
    assert_eq!(t.numbered_paragraphs.as_deref(), Some("numbered.include_full_paragraph"));
    assert_eq!(t.set_story_direction.map(TocStoryDirection::code), Some("direction.vertical"));
    assert_eq!((t.entries[0].page_number_style.clone(), t.entries[0].separator_style.clone()), (Some(identity("STYS", 41)), Some(identity("STYS", 42))));
    assert!(r.toc_styles[1].default_styled && r.toc_styles[1].entries.is_empty());
    assert_eq!(r.index_marks[3].page_range_scope, Some(StudioPageRangeScope::ToNextUseOfStyle { style: identity("STYS", 5) }));
    assert_eq!(r.index_marks[7].page_range_scope, Some(StudioPageRangeScope::ForNextParagraphs { count: 3 }));
    let o = r.index_options.as_ref().unwrap();
    assert_eq!((o.replace_existing_index, o.include_book_documents, o.include_hidden_entries), (Some(true), Some(false), Some(false)));
    assert_eq!(o.cross_reference_topic_style, Some(identity("STYS", 53)));
    assert_eq!(r.section_header_set("Cyrillic set").map(|h| h.groups[1].initials.clone()), Some(vec!['\u{430}']));
    assert!(validate(&v).unwrap().is_empty());
    for d in TocStoryDirection::ALL {
        assert_eq!(TocStoryDirection::from_code(d.code()), Some(d));
    }
    assert_eq!(TocStoryDirection::from_code("vertical"), None, "the code carries the namespace");
}

/// An old-shape record set loads with every new member unknown (never a filled-in value) and
/// re-encodes byte-identically, so a package written before the lane opens unchanged.
#[test]
fn old_shape_records_load_with_new_members_unknown() {
    let v = old_shape();
    let r: StudioLongDocRecords = serde_json::from_value(v.clone()).unwrap();
    let bytes = wire_identical(&r);
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), v, "nothing new is encoded");
    let t = &r.toc_styles[0];
    assert_eq!(
        (t.include_book_documents, t.include_hidden, t.run_in, t.create_bookmarks, t.make_anchor, t.remove_forced_line_break),
        (None, None, None, None, None, None)
    );
    assert_eq!((t.numbered_paragraphs.as_deref(), t.set_story_direction, t.default_styled), (None, None, false));
    assert_eq!((t.entries[0].page_number_style.as_deref(), t.entries[0].separator_style.as_deref()), (None, None));
    assert_eq!(r.index_marks[0].page_range_scope, None);
    let o = r.index_options.as_ref().unwrap();
    assert_eq!((o.replace_existing_index, o.include_book_documents, o.include_hidden_entries), (None, None, None));
    assert_eq!((o.page_number_style.as_deref(), o.cross_reference_style.as_deref(), o.cross_reference_topic_style.as_deref()), (None, None, None));
    assert_eq!(o.section_header_set, None);
    assert!(r.section_header_sets.is_empty());
    assert!(validate(&v).unwrap().is_empty());
}

/// Refusals of the new members with their Folio codes; unknown members stay refused.
#[test]
fn toc_index_contract_member_refusals() {
    let with = |f: &dyn Fn(&mut Value)| {
        let mut v = full_shape();
        f(&mut v);
        code_of(&v)
    };
    assert_eq!(with(&|v| v["toc_styles"][0]["numbered_paragraphs"] = json!("Has Space")), (Code::InvalidField, "toc1/toc_numbered_paragraphs".into()));
    assert_eq!(with(&|v| v["toc_styles"][0]["numbered_paragraphs"] = json!("a".repeat(MAX_CAPTURED_TOKEN_BYTES + 1))).1, "toc1/toc_numbered_paragraphs");
    assert_eq!(with(&|v| v["toc_styles"][0]["entries"][0]["separator_style"] = json!(identity("SLYR", 1))), (Code::InvalidId, "toc1/toc_style_ref".into()));
    assert_eq!(with(&|v| v["toc_styles"][0]["entries"][0]["page_number_style"] = json!("STYS")).0, Code::InvalidId);
    assert_eq!(with(&|v| v["toc_styles"][1]["default_styled"] = json!(false)), (Code::InvalidField, "toc2/toc_no_entries".into()));
    assert_eq!(with(&|v| v["index_marks"][7]["page_range_scope"]["count"] = json!(0)), (Code::InvalidField, "index_marks/index_page_range_scope".into()));
    assert_eq!(with(&|v| v["index_marks"][3]["page_range_scope"]["style"] = json!(identity("STXT", 1))).1, "index_marks/index_page_range_scope");
    assert_eq!(with(&|v| v["index_options"]["cross_reference_style"] = json!(identity("STXT", 2))), (Code::InvalidId, "index_options/index_style_ref".into()));
    assert_eq!(with(&|v| v["index_options"]["section_header_set"] = json!("Greek set")), (Code::DanglingReference, "index_options/index_header_set_missing".into()));
    assert_eq!(with(&|v| v["index_options"]["section_header_set"] = json!(" ")).1, "index_options/index_header_set_ref");
    assert_eq!(with(&|v| v["section_header_sets"][1]["name"] = json!("Cyrillic set")), (Code::DuplicateIdentity, "section_header_sets/header_set_duplicate".into()));
    let set = |f: &dyn Fn(&mut Value)| with(&|v| f(&mut v["section_header_sets"][0]));
    assert_eq!(set(&|h| h["name"] = json!("")), (Code::InvalidField, "section_header_sets/header_set_name".into()));
    assert_eq!(set(&|h| h["language_id"] = json!("r u")).1, "section_header_sets/header_set_language_id");
    assert_eq!(set(&|h| h["header_type"] = json!("Header.Cyrillic")).1, "section_header_sets/header_set_type");
    assert_eq!(set(&|h| h["groups"] = json!([])).1, "section_header_sets/header_set_groups");
    assert_eq!(set(&|h| h["groups"][1]["heading"] = json!("\t")).1, "section_header_sets/header_set_heading");
    assert_eq!(set(&|h| h["groups"][1]["initials"] = json!(["\u{410}"])).1, "section_header_sets/header_set_initial", "upper case is refused");
    assert_eq!(set(&|h| h["groups"][2]["initials"] = json!(["\u{430}"])).1, "section_header_sets/header_set_initial_duplicate");
    let many: Vec<String> = (0..=MAX_HEADER_INITIALS as u32).map(|k| char::from_u32(0x4e00 + k).unwrap().to_string()).collect();
    assert_eq!(set(&|h| h["groups"][1]["initials"] = json!(many)), (Code::BudgetExceeded, "section_header_sets/header_set_initials_budget".into()));
    let groups: Vec<Value> = (0..=MAX_HEADER_GROUPS).map(|k| json!({"heading": format!("g{k}")})).collect();
    assert_eq!(set(&|h| h["groups"] = json!(groups)).1, "section_header_sets/header_set_groups");
    // New members keep deny_unknown_fields; a two-character initial is not a char.
    let shapes: [fn(&mut Value); 5] = [
        |v| v["section_header_sets"][0]["surprise"] = json!(1),
        |v| v["index_marks"][7]["page_range_scope"]["style"] = json!(identity("STYS", 5)),
        |v| v["index_marks"][1]["page_range_scope"] = json!({"scope_kind": "to_end_of_book"}),
        |v| v["section_header_sets"][0]["groups"][1]["initials"] = json!(["ab"]),
        |v| v["toc_styles"][0]["set_story_direction"] = json!("direction.vertical"),
    ];
    for f in shapes {
        let mut v = full_shape();
        f(&mut v);
        assert!(serde_json::from_value::<StudioLongDocRecords>(v).is_err());
    }
}
