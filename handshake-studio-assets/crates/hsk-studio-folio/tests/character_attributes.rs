//! Lane SMB-TYPE-CHARACTER-ATTRIBUTES-1: the character-attribute record (STU-TYP-155..161, 133, 170,
//! 174), the stroke-style registry (STU-TYP-192) and the language records (STU-TYP-209) on the
//! type-style records: every spec field and member is representable and survives the wire, the
//! stated hard bounds and the references are typed refusals, and resolution follows paragraph style,
//! character style, local override (STU-TYP-201).
#![recursion_limit = "256"]
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn stys(n: u32) -> String {
    identity("STYS", n)
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
fn document() -> StudioDocument {
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"pt",
        "colour_mode":3,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[],"graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}

/// Every field of the record set, every enumeration at a non-first member.
fn full_attributes() -> Value {
    json!({
        "size": 11.0, "leading": 14.0, "tracking": 25.0, "kerning_manual": -10.0, "horizontal_scale": 90.0, "vertical_scale": 110.0,
        "baseline_shift": 2.5, "character_rotation": 5.0, "skew": -12.0, "aki_left": 0.25, "aki_right": 0.125, "tsume": 50.0, "stroke_weight": 0.5,
        "kerning_mode": "metrics_roman_only", "capitalization": "all_small_caps", "position": "ot_denominator", "anti_alias_method": "smooth",
        "character_alignment": "icf_bottom", "alternate_glyph_form": "proportional_width", "baseline_direction": "tate_chu_yoko",
        "font_baseline_option": "subscript", "digits_type": "burmese",
        "no_break": true, "faux_bold": true, "faux_italic": false, "fractional_widths": true, "overlap_swash": true, "stretched_alternate": false,
        "connection_forms": true, "kana": true, "italics": false, "rotate_single_byte_characters": true, "cjk_grid_tracking": false,
        "overprint_fill": true, "overprint_stroke": false,
        "opentype_features": {"liga": 0, "salt": 3, "ss01": 1},
        "small_cap_size": 70.0, "superscript_size": 58.3, "subscript_size": 58.3, "superscript_position": 33.3, "subscript_position": 33.3,
        "underline": {"type": "dotted", "weight": {"length_kind": "points", "value": 1.0}, "offset": {"length_kind": "auto"},
            "colour": {"colour_kind": "swatch", "swatch_id": identity("SSWT", 1)}, "tint": 80.0, "overprint": false,
            "gap_colour": identity("SSWT", 2), "gap_tint": 10.0, "gap_overprint": true, "skip_ink": true},
        "strikethrough": {"type": "hairline", "weight": {"length_kind": "percent", "value": 8.0}, "offset": {"length_kind": "pixels", "value": -2.0},
            "colour": {"colour_kind": "auto"}},
        "hyperlink": {"target_kind": "url", "url": "https://example.com/a?b=c"}
    })
}
fn records(attributes: Value) -> Value {
    json!({"schema_id": "hsk.studio.type_style_records@1", "document_id": identity("SDOC", 1),
        "styles": [
            {"schema_id": "hsk.studio.type_style@1", "style_id": stys(1), "scope": "paragraph", "name": "body"},
            {"schema_id": "hsk.studio.type_style@1", "style_id": stys(2), "scope": "character", "name": "base"},
            {"schema_id": "hsk.studio.type_style@1", "style_id": stys(3), "scope": "character", "name": "strong", "based_on": stys(2)}],
        "style_character": [{"style_id": stys(1), "attributes": {"size": 10.0, "tracking": 5.0}}],
        "character_overrides": [{"text_id": identity("STXT", 1), "start": 2, "end": 6, "attributes": attributes}],
        "stroke_styles": [{"name": "hairline", "pattern": {"pattern_kind": "dash", "segments": [3.0, 1.5]}, "end_cap": "round", "end_join": "bevel"}],
        "languages": [{"name": "tr_TR", "primary_language_name": "Turkish", "icu_locale_name": "tr_TR", "hyphenation_vendor": "rule_based",
            "spelling_vendor": "hunspell", "single_quotes": {"open": "\u{2018}", "close": "\u{2019}"}, "double_quotes": {"open": "\u{201c}", "close": "\u{201d}"}}]})
}
fn check(v: &Value) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    let r: StudioTypeStyleRecords = serde_json::from_value(v.clone()).expect("record shape");
    validate_type_style_records(&r, &document(), Budget::default(), &CancellationToken::default(), &Caller)
}
fn refused(v: &Value, code: Code, target_suffix: &str) {
    let d = check(v).expect_err("must be refused");
    assert_eq!(d.code, code, "{}", d.target);
    assert!(d.target.ends_with(target_suffix), "target {} lacks {target_suffix}", d.target);
}
fn with(pointer: &str, value: Value) -> Value {
    let mut v = records(full_attributes());
    *v.pointer_mut(pointer).unwrap_or_else(|| panic!("{pointer}")) = value;
    v
}

#[test]
fn every_field_validates_and_survives_the_wire_byte_identically() {
    let v = records(full_attributes());
    assert_eq!(check(&v).map(|i| i.len()), Ok(0));
    let r: StudioTypeStyleRecords = serde_json::from_value(v).unwrap();
    let bytes = serde_json::to_vec(&r).unwrap();
    let back: StudioTypeStyleRecords = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back, r);
    assert_eq!(serde_json::to_vec(&back).unwrap(), bytes);
    // Empty lists are not written: a set without the new records keeps its old bytes.
    let plain = StudioTypeStyleRecords::new(identity("SDOC", 1));
    let text = serde_json::to_string(&plain).unwrap();
    for key in ["style_character", "character_overrides", "stroke_styles", "languages"] {
        assert!(!text.contains(key), "{text}");
    }
}

/// STU-TYP-221 / `typ.enumeration_complete`: the member lists a model reads are the serde names, and
/// the field list is the record's own.
#[test]
fn enumerations_and_field_list_are_the_record_schema() {
    let schema = serde_json::to_value(type_style_records_schema()).unwrap();
    let props = schema["$defs"]["StudioCharacterAttributes"]["properties"].as_object().expect("attributes properties");
    let mut names: Vec<&str> = props.keys().map(String::as_str).collect();
    names.sort_unstable();
    let mut listed: Vec<&str> = CHARACTER_ATTRIBUTE_FIELDS.to_vec();
    listed.sort_unstable();
    assert_eq!(names, listed);
    for (field, members) in CHARACTER_ENUMERATIONS {
        for m in members {
            let mut a = json!({});
            a[field] = json!(m);
            let parsed: StudioCharacterAttributes = serde_json::from_value(a.clone()).unwrap_or_else(|e| panic!("{field}={m}: {e}"));
            assert_eq!(serde_json::to_value(&parsed).unwrap(), a);
        }
        let mut wrong = json!({});
        wrong[field] = json!("not_a_member");
        assert!(serde_json::from_value::<StudioCharacterAttributes>(wrong).is_err(), "{field} is closed");
    }
    let sizes: Vec<usize> = CHARACTER_ENUMERATIONS.iter().map(|(_, m)| m.len()).collect();
    assert_eq!(sizes, [4, 5, 7, 5, 6, 13, 3, 3, 20], "STU-TYP-156 and STU-TYP-133 member counts");
    // STU-TYP-155 / 158 / 159 rows: UNKNOWN stays None, the stated bounds are the spec's.
    let row = |k: &str| *CHARACTER_PARAMS.iter().find(|r| r.key == k).unwrap();
    assert_eq!((row("tracking").default, row("tracking").precision, row("tracking").hard_min), (Some(0.0), Some(0), None));
    assert_eq!((row("small_cap_size").hard_min, row("small_cap_size").hard_max), (Some(1.0), Some(200.0)));
    assert_eq!((row("stroke_weight").hard_min, row("stroke_weight").hard_max), (Some(0.0), None));
    assert_eq!(row("underline_gap_tint").hard_max, Some(100.0));
}

#[test]
fn stated_bounds_and_domains_are_typed_refusals() {
    let base = "/character_overrides/0/attributes";
    for (field, value) in [
        ("size", json!(0.0)),
        ("leading", json!(-1.0)),
        ("tracking", json!(12.5)),
        ("kerning_manual", json!(0.25)),
        ("horizontal_scale", json!(0.0)),
        ("stroke_weight", json!(-0.1)),
        ("small_cap_size", json!(0.5)),
        ("superscript_size", json!(200.5)),
        ("subscript_position", json!(-500.5)),
    ] {
        refused(&with(&format!("{base}/{field}"), value), Code::InvalidField, field);
    }
    refused(&with(&format!("{base}/underline/tint"), json!(100.5)), Code::InvalidField, "underline/tint");
    refused(&with(&format!("{base}/underline/weight"), json!({"length_kind": "points", "value": -1.0})), Code::InvalidField, "underline/weight");
    refused(&with(&format!("{base}/strikethrough/type"), json!("nowhere")), Code::DanglingReference, "strikethrough/type");
    refused(&with(&format!("{base}/underline/gap_colour"), json!("red")), Code::InvalidId, "underline/gap_colour");
    refused(&with(&format!("{base}/opentype_features"), json!({"li": 1})), Code::InvalidField, "opentype_features");
    let many: serde_json::Map<String, Value> = (0..=MAX_RECORD_FEATURES).map(|i| (format!("x{i:03}"), json!(1))).collect();
    refused(&with(&format!("{base}/opentype_features"), Value::Object(many)), Code::InvalidField, "opentype_features");
    for url in ["javascript:alert(1)", "JavaScript:x", "data:text/html,x", "no scheme", "", "https://a b"] {
        refused(&with(&format!("{base}/hyperlink"), json!({"target_kind": "url", "url": url})), Code::InvalidField, "hyperlink");
    }
    assert_eq!(check(&with(&format!("{base}/hyperlink"), json!({"target_kind": "node", "node_id": identity("SPGS", 1)}))).map(|i| i.len()), Ok(0));
    // `off` and the built-ins always resolve.
    for t in ["off", "solid", "dotted", "wavy"] {
        assert!(check(&with(&format!("{base}/underline/type"), json!(t))).is_ok(), "{t}");
    }
}

#[test]
fn registries_overrides_and_style_entries_are_checked() {
    refused(&with("/stroke_styles/0/name", json!("solid")), Code::InvalidField, "name");
    refused(&with("/stroke_styles/0/pattern", json!({"pattern_kind": "dash", "segments": [3.0]})), Code::InvalidField, "pattern");
    refused(&with("/stroke_styles/0/pattern", json!({"pattern_kind": "stripe", "stripes": [1.0, 1.0]})), Code::InvalidField, "pattern");
    refused(&with("/stroke_styles/0/pattern", json!({"pattern_kind": "dash", "segments": [0.0, 0.0]})), Code::InvalidField, "pattern");
    refused(&with("/languages/0/icu_locale_name", json!("t")), Code::InvalidField, "icu_locale_name");
    refused(&with("/languages/0/icu_locale_name", json!("tr-TR")), Code::InvalidField, "icu_locale_name");
    refused(&with("/languages/0/double_quotes", json!({"open": "", "close": "x"})), Code::InvalidField, "double_quotes");
    refused(&with("/style_character/0/style_id", json!(stys(9))), Code::DanglingReference, &stys(9));
    refused(&with("/character_overrides/0/end", json!(2)), Code::InvalidField, "end");
    refused(&with("/character_overrides/0/attributes", json!({})), Code::InvalidField, "attributes");
    let mut overlap = records(full_attributes());
    overlap["character_overrides"].as_array_mut().unwrap().push(json!({"text_id": identity("STXT", 1), "start": 5, "end": 9, "attributes": {"size": 9.0}}));
    refused(&overlap, Code::InvalidField, "/5");
    let mut other_story = records(full_attributes());
    other_story["character_overrides"].as_array_mut().unwrap().push(json!({"text_id": identity("STXT", 2), "start": 0, "end": 9, "attributes": {"size": 9.0}}));
    assert_eq!(check(&other_story).map(|i| i.len()), Ok(0), "another story's override may share the bytes");
    let mut twice = records(full_attributes());
    let again = twice["languages"][0].clone();
    twice["languages"].as_array_mut().unwrap().push(again);
    refused(&twice, Code::DuplicateIdentity, "languages/tr_TR");
}

/// STU-TYP-201 order and inheritance: paragraph style chain, character style chain (`based_on`
/// root first), then the local override covering the byte; decorations merge field by field and
/// feature entries tag by tag.
#[test]
fn resolution_is_paragraph_then_character_then_override() {
    let mut v = records(json!({"size": 20.0, "opentype_features": {"liga": 0}, "underline": {"tint": 50.0}}));
    v["style_character"] = json!([
        {"style_id": stys(1), "attributes": {"size": 10.0, "tracking": 5.0, "kerning_mode": "none"}},
        {"style_id": stys(2), "attributes": {"tracking": 30.0, "opentype_features": {"liga": 1, "smcp": 1}, "underline": {"type": "solid", "tint": 100.0}}},
        {"style_id": stys(3), "attributes": {"capitalization": "small_caps"}}]);
    assert!(check(&v).is_ok());
    let r: StudioTypeStyleRecords = serde_json::from_value(v).unwrap();
    let story = identity("STXT", 1);
    let outside = r.resolved_character(&story, 0, Some(&stys(1)), Some(&stys(3)));
    assert_eq!((outside.size, outside.tracking, outside.kerning_mode), (Some(10.0), Some(30.0), Some(StudioKerningMode::None)));
    assert_eq!(outside.capitalization, Some(StudioCapitalization::SmallCaps), "inherited through based_on");
    assert_eq!(outside.opentype_features.get("liga"), Some(&1));
    let inside = r.resolved_character(&story, 3, Some(&stys(1)), Some(&stys(3)));
    assert_eq!((inside.size, inside.tracking), (Some(20.0), Some(30.0)));
    assert_eq!((inside.opentype_features.get("liga"), inside.opentype_features.get("smcp")), (Some(&0), Some(&1)));
    let u = inside.underline.expect("merged underline");
    assert_eq!((u.stroke_type.as_deref(), u.tint), (Some("solid"), Some(50.0)));
    assert!(r.resolved_character(&identity("STXT", 2), 3, None, None).is_empty(), "another story has no override");
}

/// A record whose every field is set overlays every field (a field missed by `overlaid` fails here).
#[test]
fn overlay_takes_every_field() {
    let full: StudioCharacterAttributes = serde_json::from_value(full_attributes()).unwrap();
    assert_eq!(StudioCharacterAttributes::default().overlaid(&full), full);
    assert_eq!(full.overlaid(&StudioCharacterAttributes::default()), full);
    let lang: StudioLanguage = serde_json::from_value(records(json!({"size": 1.0}))["languages"][0].clone()).unwrap();
    assert_eq!(lang.language_tag(), "tr-TR");
}
