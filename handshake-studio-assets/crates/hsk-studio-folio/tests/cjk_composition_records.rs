//! SMB-ID-CJK-KINSOKU-1: `hsk.studio.cjk_composition@1` (STU-TYP-136/137/138). Both spec
//! enumerations taken whole (the four kinsoku_type members with the other kinsoku attributes and
//! the seventeen mojikumi base sets), byte-identical round trips, name resolution refusals and
//! hostile bounds.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn pt(v: f64) -> Value {
    json!({"value": v, "unit": "pt"})
}
fn document_value() -> Value {
    json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"pt",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],
        "page_spreads":[{"schema_id":"hsk.studio.page_spread@1","page_spread_id":identity("SPGS",1),"name":"s1","parent":null,
            "bounds":{"x":pt(0.0),"y":pt(0.0),"width":pt(612.0),"height":pt(792.0)},"pages":[]}],
        "layers":[],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    })
}
fn document() -> StudioDocument {
    serde_json::from_value(document_value()).expect("document shape")
}
fn styles() -> StudioTypeStyleRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.type_style_records@1","document_id":identity("SDOC",1),
        "styles":[
            {"schema_id":"hsk.studio.type_style@1","style_id":identity("STYS",1),"scope":"paragraph","name":"Body"},
            {"schema_id":"hsk.studio.type_style@1","style_id":identity("STYS",2),"scope":"character","name":"Em"}
        ]}))
    .expect("styles shape")
}
fn base() -> Value {
    json!({"schema_id":"hsk.studio.cjk_composition@1","document_id":identity("SDOC",1)})
}
fn full() -> Value {
    let mut v = base();
    v["kinsoku_tables"] = json!([{"name":"house","cant_begin_line_chars":"、。」ー","cant_end_line_chars":"「（",
        "cant_be_separated_chars":"…‥","hanging_punctuation_chars":"、。"}]);
    v["mojikumi_tables"] = json!([{"name":"tight","based_on":"line_end_all_one_half_em",
        "aki_override_list":[{"previous":"closing_brackets","next":"ideographic","minimum_em":0.0,"desired_em":0.25,"maximum_em":0.5},
                             {"previous":"line_start","next":"opening_brackets","minimum_em":0.0,"desired_em":0.0,"maximum_em":0.0}],
        "use_cid_mojikumi":false}]);
    v["composers"] = json!([
        {"composer_name":"Japanese Paragraph","composition_scope":"every_line","composition_engine":"latin_cjk",
         "kinsoku_table":"house","mojikumi_table":"tight"},
        {"composer_name":"Japanese Single","composition_scope":"single_line","composition_engine":"latin_cjk",
         "kinsoku_table":"hard_kinsoku","mojikumi_table":"one_em_indent_line_end_all_one_em"}
    ]);
    v["bindings"] = json!([{"style_id":identity("STYS",1),"composer_name":"Japanese Paragraph","kinsoku_type":"push_in_first",
        "kinsoku_hang_type":"regular","bunri_kinshi":true,"rensuuji":true,"burasagari_type":"standard","kinsoku_order":"push_in"}]);
    v
}
fn decode(v: &Value) -> StudioCjkCompositionRecords {
    serde_json::from_value(v.clone()).expect("record shape")
}
fn check(v: &Value) -> Result<(), Diagnostic> {
    validate_cjk_composition_records(&decode(v), &document(), Some(&styles()), &CancellationToken::default())
}
fn code(v: &Value) -> Code {
    check(v).expect_err("refused").code
}
fn round_trip(v: &Value) -> StudioCjkCompositionRecords {
    let r = decode(v);
    let first = serde_json::to_vec(&r).unwrap();
    let back: StudioCjkCompositionRecords = serde_json::from_slice(&first).unwrap();
    assert_eq!(back, r);
    assert_eq!(serde_json::to_vec(&back).unwrap(), first);
    r
}

/// STU-TYP-137 and STU-TYP-138: every member of both enumerations (and of the other kinsoku
/// attribute enumerations) round-trips under its spec wire name and validates.
#[test]
fn cjk_records_validate_both_enumerations_whole() {
    let base_sets = [
        "nothing", "line_end_all_one_half_em", "one_em_indent_line_end_uke_one_half_em",
        "one_or_one_half_em_indent_line_end_uke_one_half_em", "one_or_one_half_em_indent_line_end_all_one_em",
        "one_em_indent_line_end_all_one_em", "one_em_indent_line_end_all_no_float", "one_em_indent_line_end_uke_no_float",
        "one_or_one_half_em_indent_line_end_uke_no_float", "one_em_indent_line_end_all_one_half_em", "line_end_all_one_em",
        "line_end_uke_no_float", "one_or_one_half_em_indent_line_end_period_one_em", "one_em_indent_line_end_period_one_em",
        "line_end_period_one_em", "traditional_chinese_default", "simplified_chinese_default",
    ];
    assert_eq!(StudioMojikumiBaseSet::ALL.len(), 17);
    for (set, w) in StudioMojikumiBaseSet::ALL.iter().zip(base_sets) {
        assert_eq!(serde_json::to_value(set).unwrap(), json!(w));
        assert_eq!(set.name(), w);
        assert_eq!(StudioMojikumiBaseSet::from_name(w), Some(*set));
        let mut v = full();
        v["mojikumi_tables"][0]["based_on"] = json!(w);
        round_trip(&v);
        assert!(check(&v).is_ok(), "{w}");
        // A composer may name the base set directly.
        let mut v = full();
        v["composers"][1]["mojikumi_table"] = json!(w);
        assert!(check(&v).is_ok(), "composer -> {w}");
    }
    for k in ["push_in_first", "push_out_first", "push_out_only", "prioritize_adjustment"] {
        let mut v = full();
        v["bindings"][0]["kinsoku_type"] = json!(k);
        round_trip(&v);
        assert!(check(&v).is_ok(), "{k}");
    }
    for (field, members) in [
        ("kinsoku_hang_type", &["none", "regular", "force"][..]),
        ("burasagari_type", &["none", "standard", "forced"][..]),
        ("kinsoku_order", &["push_in", "push_out_first", "push_out_only"][..]),
    ] {
        for m in members {
            let mut v = full();
            v["bindings"][0][field] = json!(m);
            round_trip(&v);
            assert!(check(&v).is_ok(), "{field}={m}");
        }
    }
    let mut v = full();
    v["bindings"][0]["kinsoku_type"] = json!("push_sideways");
    assert!(serde_json::from_value::<StudioCjkCompositionRecords>(v).is_err(), "closed enumeration");
    let mut v = full();
    v["mojikumi_tables"][0]["based_on"] = json!("korean_default");
    assert!(serde_json::from_value::<StudioCjkCompositionRecords>(v).is_err(), "seventeen members only");
    // Six combinations of the two axes are representable on a composer (STU-TYP-135).
    for scope in ["single_line", "every_line"] {
        for engine in ["latin_cjk", "world_ready", "adornment"] {
            let mut v = full();
            v["composers"][0]["composition_scope"] = json!(scope);
            v["composers"][0]["composition_engine"] = json!(engine);
            round_trip(&v);
            assert!(check(&v).is_ok(), "{scope}/{engine}");
        }
    }
}

/// STU-TYP-136: an unresolvable composer or table name is a validation error, never a fallback.
#[test]
fn unresolvable_composer_or_table_name_is_refused() {
    assert!(check(&full()).is_ok());
    let mut v = full();
    v["composers"][0]["kinsoku_table"] = json!("missing");
    assert_eq!(code(&v), Code::DanglingReference);
    let mut v = full();
    v["composers"][0]["mojikumi_table"] = json!("missing");
    assert_eq!(code(&v), Code::DanglingReference);
    let mut v = full();
    v["bindings"][0]["composer_name"] = json!("Korean Paragraph");
    assert_eq!(code(&v), Code::DanglingReference);
    let mut v = full();
    v["bindings"][0]["style_id"] = json!(identity("STYS", 9));
    assert_eq!(code(&v), Code::DanglingReference, "style not in the type-style records");
    let mut v = full();
    v["bindings"][0]["style_id"] = json!(identity("STYS", 2));
    assert_eq!(code(&v), Code::PayloadMismatch, "a character style takes no paragraph binding");
    // Without type-style records only the id grammar is checked.
    let mut v = full();
    v["bindings"][0]["style_id"] = json!(identity("STYS", 9));
    assert!(validate_cjk_composition_records(&decode(&v), &document(), None, &CancellationToken::default()).is_ok());
    let mut v = full();
    v["bindings"][0]["style_id"] = json!(identity("SLYR", 1));
    assert_eq!(code(&v), Code::InvalidId);
    // Names: unique per kind, shipped and base-set names reserved, no blank or control names.
    let mut v = full();
    v["kinsoku_tables"][0]["name"] = json!("hard_kinsoku");
    assert_eq!(code(&v), Code::InvalidField);
    let mut v = full();
    v["mojikumi_tables"][0]["name"] = json!("nothing");
    assert_eq!(code(&v), Code::InvalidField);
    let mut v = full();
    v["composers"][1]["composer_name"] = json!("Japanese Paragraph");
    assert_eq!(code(&v), Code::DuplicateIdentity);
    let mut v = full();
    v["composers"][1]["composer_name"] = json!("  ");
    assert_eq!(code(&v), Code::InvalidField);
    let mut v = full();
    v["bindings"] = json!([v["bindings"][0].clone(), v["bindings"][0].clone()]);
    assert_eq!(code(&v), Code::DuplicateIdentity);
    // Character sets: each character once, no controls.
    let mut v = full();
    v["kinsoku_tables"][0]["cant_begin_line_chars"] = json!("。。");
    assert_eq!(code(&v), Code::DuplicateField);
    let mut v = full();
    v["kinsoku_tables"][0]["hanging_punctuation_chars"] = json!("、\u{7}");
    assert_eq!(code(&v), Code::InvalidField);
    // Aki overrides: ordered, in range, line positions on the right side, one per pair.
    for bad in [
        json!({"previous":"commas","next":"ideographic","minimum_em":0.5,"desired_em":0.25,"maximum_em":0.5}),
        json!({"previous":"commas","next":"ideographic","minimum_em":0.0,"desired_em":0.25,"maximum_em":2.5}),
        json!({"previous":"commas","next":"ideographic","minimum_em":-0.1,"desired_em":0.0,"maximum_em":0.5}),
        json!({"previous":"line_end","next":"ideographic","minimum_em":0.0,"desired_em":0.0,"maximum_em":0.0}),
        json!({"previous":"commas","next":"line_start","minimum_em":0.0,"desired_em":0.0,"maximum_em":0.0}),
        json!({"previous":"line_start","next":"line_end","minimum_em":0.0,"desired_em":0.0,"maximum_em":0.0}),
    ] {
        let mut v = full();
        v["mojikumi_tables"][0]["aki_override_list"] = json!([bad]);
        assert_eq!(code(&v), Code::InvalidField, "{bad}");
    }
    let mut v = full();
    let o = v["mojikumi_tables"][0]["aki_override_list"][0].clone();
    v["mojikumi_tables"][0]["aki_override_list"] = json!([o.clone(), o]);
    assert_eq!(code(&v), Code::DuplicateIdentity);
    let mut v = full();
    v["document_id"] = json!(identity("SDOC", 2));
    assert_eq!(code(&v), Code::InvalidField);
}

/// Absent records change nothing: the document has no CJK member, an empty set encodes as its
/// header only, and every list is skipped on the wire when empty.
#[test]
fn absent_cjk_records_keep_documents_byte_identical() {
    let d = document();
    let bytes = serde_json::to_vec(&d).unwrap();
    let back: StudioDocument = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(serde_json::to_vec(&back).unwrap(), bytes);
    assert!(!String::from_utf8(bytes).unwrap().contains("kinsoku"));
    let empty = StudioCjkCompositionRecords::new(identity("SDOC", 1));
    assert_eq!(serde_json::to_value(&empty).unwrap(), base());
    assert_eq!(round_trip(&base()), empty);
    assert!(validate_cjk_composition_records(&empty, &d, None, &CancellationToken::default()).is_ok());
    // use_cid_mojikumi false and an empty override list are not encoded.
    let mut v = full();
    v["mojikumi_tables"][0]["aki_override_list"] = json!([]);
    let r = decode(&v);
    let wire = serde_json::to_value(&r).unwrap();
    assert_eq!(wire["mojikumi_tables"][0], json!({"name":"tight","based_on":"line_end_all_one_half_em"}));
    let mut unknown = base();
    unknown["highlight_kinsoku"] = json!(true);
    assert!(serde_json::from_value::<StudioCjkCompositionRecords>(unknown).is_err(), "highlight_kinsoku is a view preference");
    // The schema is generated and names the record set.
    let schema = serde_json::to_value(cjk_composition_records_schema()).unwrap();
    assert!(schema.to_string().contains("hsk.studio.cjk_composition@1"));
}

/// Hostile sizes: over-long character sets, lists and override lists are refused as
/// `BudgetExceeded` before they are walked.
#[test]
fn hostile_cjk_record_sizes_are_bounded() {
    let mut v = full();
    let many: String = (0x4E00u32..0x4E00 + MAX_KINSOKU_CHARS as u32 + 1).filter_map(char::from_u32).collect();
    v["kinsoku_tables"][0]["cant_begin_line_chars"] = json!(many);
    assert_eq!(code(&v), Code::BudgetExceeded);
    let mut v = full();
    v["kinsoku_tables"][0]["cant_end_line_chars"] = json!("x".repeat(MAX_KINSOKU_CHARS * 4 + 1));
    assert_eq!(code(&v), Code::BudgetExceeded);
    let mut v = full();
    let o = json!({"previous":"commas","next":"ideographic","minimum_em":0.0,"desired_em":0.0,"maximum_em":0.0});
    v["mojikumi_tables"][0]["aki_override_list"] = Value::Array(vec![o; MAX_AKI_OVERRIDES + 1]);
    assert_eq!(code(&v), Code::BudgetExceeded);
    let mut v = full();
    let c = v["composers"][0].clone();
    v["composers"] = Value::Array(vec![c; MAX_CJK_TABLES + 1]);
    assert_eq!(code(&v), Code::BudgetExceeded);
    let mut v = full();
    let b = v["bindings"][0].clone();
    v["bindings"] = Value::Array(vec![b; MAX_CJK_BINDINGS + 1]);
    assert_eq!(code(&v), Code::BudgetExceeded);
    let mut v = full();
    v["mojikumi_tables"][0]["aki_override_list"][0]["desired_em"] = json!(f64::MAX);
    assert_eq!(code(&v), Code::InvalidField);
}
