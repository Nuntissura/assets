//! SMB-LAYOUT-COMPOSER-WIRING: paragraph composition settings on the type-style records
//! (STU-TYP-135, 146-148, 150-151): absent settings keep the wire bytes of existing record sets,
//! set settings survive the wire byte-identically, inheritance overlays field by field
//! (STU-TYP-201), and every spec hard bound is a typed refusal.
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
fn legacy() -> Value {
    json!({"schema_id":"hsk.studio.type_style_records@1","document_id":identity("SDOC",1),
        "styles":[
            {"schema_id":"hsk.studio.type_style@1","style_id":stys(1),"scope":"paragraph","name":"body/base","align":"left"},
            {"schema_id":"hsk.studio.type_style@1","style_id":stys(2),"scope":"paragraph","name":"body/lead","based_on":stys(1)},
            {"schema_id":"hsk.studio.type_style@1","style_id":stys(3),"scope":"character","name":"emphasis"}]})
}
fn full_settings() -> Value {
    json!({"composition_scope":"single_line","composition_engine":"latin_cjk","hyphenation":true,
        "hyphenate_words_longer_than":5,"hyphenate_after_first":2,"hyphenate_before_last":2,"hyphenate_ladder_limit":0,
        "hyphen_weight":5,"hyphenation_zone":{"value":36.0,"unit":"pt"},"hyphenate_capitalized_words":false,
        "hyphenate_last_word":false,"hyphenate_across_columns":true,
        "word_spacing_minimum":80.0,"word_spacing_desired":100.0,"word_spacing_maximum":133.0,
        "letter_spacing_minimum":-5.0,"letter_spacing_desired":0.0,"letter_spacing_maximum":5.0,
        "glyph_scaling_minimum":90.0,"glyph_scaling_desired":100.0,"glyph_scaling_maximum":110.0,
        "single_word_justification":"left_align","auto_leading_amount":120.0})
}
fn with_composition() -> Value {
    let mut v = legacy();
    v["style_composition"] = json!([
        {"style_id":stys(1),"settings":full_settings()},
        {"style_id":stys(2),"settings":{"composition_scope":"every_line","word_spacing_maximum":150.0}}]);
    v["paragraph_composition"] = json!([
        {"text_id":identity("STXT",1),"paragraph_start":0,"settings":{"hyphenation":false}}]);
    v
}
fn check(v: &Value) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    let r: StudioTypeStyleRecords = serde_json::from_value(v.clone()).expect("record shape");
    validate_type_style_records(&r, &document(), Budget::default(), &CancellationToken::default(), &Caller)
}
fn wire(v: &Value) -> Vec<u8> {
    let r: StudioTypeStyleRecords = serde_json::from_value(v.clone()).expect("record shape");
    let bytes = serde_json::to_vec(&r).expect("encode");
    let back: StudioTypeStyleRecords = serde_json::from_slice(&bytes).expect("decode");
    assert_eq!(back, r);
    assert_eq!(serde_json::to_vec(&back).expect("encode again"), bytes);
    bytes
}

/// A record set without composition settings encodes exactly as before the fields existed (no
/// new keys, same bytes as the legacy wire value), and an empty set adds nothing to `new()`.
#[test]
fn absent_composition_keeps_existing_bytes() {
    let v = legacy();
    assert_eq!(check(&v), Ok(vec![]));
    // Same members as the legacy wire value: no new key appears when nothing is set.
    assert_eq!(serde_json::from_slice::<Value>(&wire(&v)).unwrap(), v);
    let empty = StudioTypeStyleRecords::new(identity("SDOC", 1));
    assert_eq!(
        serde_json::to_value(&empty).unwrap(),
        json!({"schema_id":"hsk.studio.type_style_records@1","document_id":identity("SDOC",1)})
    );
    assert!(StudioCompositionSettings::default().is_empty());
    assert_eq!(serde_json::to_value(StudioCompositionSettings::default()).unwrap(), json!({}));
}

/// Every field set survives the wire byte-identically; lookups find the style entry and the
/// paragraph override; `overlaid` takes the top value of every field it sets (STU-TYP-201 order).
#[test]
fn composition_settings_round_trip_and_overlay() {
    let v = with_composition();
    assert_eq!(check(&v), Ok(vec![]));
    let bytes = wire(&v);
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), v);
    let r: StudioTypeStyleRecords = serde_json::from_value(v).unwrap();
    let base = r.style_composition(&stys(1)).expect("style entry");
    let lead = r.style_composition(&stys(2)).expect("style entry");
    assert_eq!(base.composition_scope, Some(StudioCompositionScope::SingleLine));
    assert!(r.style_composition(&stys(3)).is_none());
    let local = r.paragraph_composition(&identity("STXT", 1), 0).expect("override");
    assert!(r.paragraph_composition(&identity("STXT", 1), 1).is_none());
    let resolved = base.overlaid(lead).overlaid(local);
    assert_eq!(resolved.composition_scope, Some(StudioCompositionScope::EveryLine));
    assert_eq!(resolved.word_spacing_maximum, Some(150.0));
    assert_eq!(resolved.word_spacing_minimum, Some(80.0));
    assert_eq!(resolved.hyphenation, Some(false));
    assert_eq!(resolved.hyphenation_zone, Some(Length { value: 36.0, unit: GeometryUnit::Pt }));
    assert_eq!(resolved.single_word_justification, Some(StudioSingleWordJustification::LeftAlign));
    assert_eq!(StudioCompositionSettings::default().overlaid(&resolved), resolved);
    assert_eq!(resolved.overlaid(&StudioCompositionSettings::default()), resolved);
}

/// Hard bounds of STU-TYP-147 / 148 / 150, ordered triples, the zone's physical domain, references
/// and identities: each a typed refusal, never a clamp.
#[test]
fn out_of_range_and_bad_references_are_refused() {
    let bad_fields = [
        ("word_spacing_minimum", json!(-0.5)),
        ("word_spacing_maximum", json!(1000.5)),
        ("letter_spacing_minimum", json!(-100.5)),
        ("letter_spacing_maximum", json!(500.5)),
        ("glyph_scaling_minimum", json!(49.9)),
        ("glyph_scaling_maximum", json!(200.1)),
        ("auto_leading_amount", json!(500.1)),
        ("auto_leading_amount", json!(-1.0)),
        ("hyphen_weight", json!(101)),
        ("hyphenation_zone", json!({"value":-1.0,"unit":"pt"})),
        // Ordered triples: minimum <= desired <= maximum within one record.
        ("word_spacing_desired", json!(70.0)),
        ("glyph_scaling_desired", json!(120.0)),
        ("letter_spacing_maximum", json!(-6.0)),
    ];
    for (field, value) in bad_fields {
        let mut v = with_composition();
        v["style_composition"][0]["settings"][field] = value.clone();
        let e = check(&v).unwrap_err();
        assert_eq!(e.code, Code::InvalidField, "{field} = {value}");
    }
    // A partial triple is ordered against what it states only.
    let mut partial = with_composition();
    partial["style_composition"][1]["settings"]["word_spacing_minimum"] = json!(140.0);
    assert_eq!(check(&partial), Ok(vec![]));
    partial["style_composition"][1]["settings"]["word_spacing_minimum"] = json!(151.0);
    assert_eq!(check(&partial).unwrap_err().code, Code::InvalidField);
    // Negative counts and non-integers are not counts: the wire refuses them.
    for (field, value) in [("hyphenate_after_first", json!(-1)), ("hyphenate_ladder_limit", json!(1.5)), ("hyphenation", json!("yes"))] {
        let mut v = with_composition();
        v["style_composition"][0]["settings"][field] = value;
        assert!(serde_json::from_value::<StudioTypeStyleRecords>(v).is_err(), "{field}");
    }
    // Unknown setting names and enumeration values are refused, never ignored.
    for (field, value) in [("composer", json!("first_fit")), ("composition_scope", json!("paragraph")), ("single_word_justification", json!("justify"))] {
        let mut v = with_composition();
        v["style_composition"][0]["settings"][field] = value;
        assert!(serde_json::from_value::<StudioTypeStyleRecords>(v).is_err(), "{field}");
    }
    let mut dangling = with_composition();
    dangling["style_composition"][0]["style_id"] = json!(stys(9));
    assert_eq!(check(&dangling).unwrap_err().code, Code::DanglingReference);
    let mut character = with_composition();
    character["style_composition"][0]["style_id"] = json!(stys(3));
    assert_eq!(check(&character).unwrap_err().code, Code::PayloadMismatch);
    let mut twice = with_composition();
    twice["style_composition"][1]["style_id"] = json!(stys(1));
    assert_eq!(check(&twice).unwrap_err().code, Code::DuplicateIdentity);
    let mut wrong_text = with_composition();
    wrong_text["paragraph_composition"][0]["text_id"] = json!(stys(1));
    assert_eq!(check(&wrong_text).unwrap_err().code, Code::InvalidId);
    let mut twice_local = with_composition();
    let entry = twice_local["paragraph_composition"][0].clone();
    twice_local["paragraph_composition"].as_array_mut().unwrap().push(entry);
    assert_eq!(check(&twice_local).unwrap_err().code, Code::DuplicateIdentity);
    let mut bad_local = with_composition();
    bad_local["paragraph_composition"][0]["settings"]["glyph_scaling_desired"] = json!(300.0);
    assert_eq!(check(&bad_local).unwrap_err().code, Code::InvalidField);
}
