//! SMB-ID-LIQUID-LAYOUT-1: `hsk.studio.layout_rules@1` (STU-LAY-011/019/111). Byte-identical wire
//! round trip, the seven liquid members, alternate-layout name rules, separate linked-story and
//! linked-page-item option objects and every refusal code next to the rule it comes from.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn slyr(n: u32) -> String {
    identity("SLYR", n)
}
fn stxt(n: u32) -> String {
    identity("STXT", n)
}
fn pt(v: f64) -> Value {
    json!({"value": v, "unit": "pt"})
}
fn page(key: &str) -> Value {
    let e = json!({"top": pt(36.0), "right": pt(36.0), "bottom": pt(36.0), "left": pt(36.0)});
    json!({"page_id": key, "width": pt(612.0), "height": pt(792.0), "side": "right_hand",
        "margins": e, "bleed": null})
}
fn text_layer(n: u32, story: u32, page: &str) -> Value {
    let zero = json!({"top": pt(0.0), "right": pt(0.0), "bottom": pt(0.0), "left": pt(0.0)});
    json!({"schema_id":"hsk.studio.layer@1","layer_id":slyr(n),"name":format!("t{n}"),"parent":null,
        "kind":"text","payload":{"payload_kind":"text","story_id":stxt(story)},"visible":true,
        "text_frame":{"page_id":page,"bounds":{"x":pt(36.0),"y":pt(36.0),"width":pt(200.0),"height":pt(100.0)},
            "inset_spacing":zero,"text_column_count":1,"text_column_gutter":pt(0.0),
            "first_baseline_offset":"fixed","minimum_first_baseline_offset":pt(10.0),
            "vertical_justification":"top","auto_sizing_type":"off",
            "previous_text_frame":null,"next_text_frame":null}})
}
fn raster_layer(n: u32) -> Value {
    json!({"schema_id":"hsk.studio.layer@1","layer_id":slyr(n),"name":format!("r{n}"),
        "parent":null,"kind":"raster","payload":{"payload_kind":"raster","tiles":[]},"visible":true})
}
fn document_value() -> Value {
    let layers = vec![
        text_layer(1, 1, "p1"),
        text_layer(2, 2, "p3"),
        text_layer(3, 3, "p4"),
        raster_layer(4),
        raster_layer(5),
    ];
    json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"pt",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],
        "page_spreads":[
            {"schema_id":"hsk.studio.page_spread@1","page_spread_id":identity("SPGS",1),"name":"s1","parent":null,
             "bounds":{"x":pt(0.0),"y":pt(0.0),"width":pt(1224.0),"height":pt(792.0)},"pages":[page("p1"),page("p2")]},
            {"schema_id":"hsk.studio.page_spread@1","page_spread_id":identity("SPGS",2),"name":"s2","parent":null,
             "bounds":{"x":pt(0.0),"y":pt(900.0),"width":pt(1224.0),"height":pt(792.0)},"pages":[page("p3"),page("p4")]}
        ],
        "layers":layers,
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    })
}
fn document() -> StudioDocument {
    serde_json::from_value(document_value()).expect("document shape")
}
fn parents() -> StudioParentPageRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.parent_page_records@1","document_id":identity("SDOC",1),
        "parents":[{"schema_id":"hsk.studio.parent_page@1","parent_id":"A","name":"A"},
                   {"schema_id":"hsk.studio.parent_page@1","parent_id":"B","name":"B","based_on":"A"}],
        "page_bindings":[{"page_id":"p1","applied_parent":"B"},{"page_id":"p2"}]}))
    .expect("parents shape")
}
fn story(n: u32, text: &str) -> StudioTextStory {
    serde_json::from_value(json!({"schema_id":"hsk.studio.text_story@1","story_id":stxt(n),"text_kind":"area",
        "story_direction":"left_to_right","content":{"content_kind":"inline","text_utf8":text},
        "paragraphs":[{"start":0,"end":text.len(),"style_ref":null,"paragraph_direction":null}],
        "character_ranges":[{"start":0,"end":text.len(),"style_ref":null,"applied_language":null}]}))
    .expect("story shape")
}
fn base() -> Value {
    json!({"schema_id":"hsk.studio.layout_rules@1","document_id":identity("SDOC",1)})
}
fn check_with(v: &Value, p: Option<&StudioParentPageRecords>) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    let r: StudioLayoutRuleRecords = serde_json::from_value(v.clone()).expect("record shape");
    validate_layout_rule_records(&r, &document(), p, Budget::default(), &CancellationToken::default(), &Unresolved)
}
fn check(v: &Value) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    check_with(v, Some(&parents()))
}
fn code(v: &Value) -> Code {
    check(v).expect_err("refused").code
}
fn with(key: &str, member: Value) -> Value {
    let mut v = base();
    v[key] = member;
    v
}
fn round_trip(v: &Value) -> StudioLayoutRuleRecords {
    let r: StudioLayoutRuleRecords = serde_json::from_value(v.clone()).expect("decode");
    let first = serde_json::to_vec(&r).unwrap();
    let back: StudioLayoutRuleRecords = serde_json::from_slice(&first).unwrap();
    assert_eq!(back, r);
    assert_eq!(serde_json::to_vec(&back).unwrap(), first);
    r
}

/// STU-LAY-111: the seven members with their Studio ids round-trip; six are storable page rules,
/// `preserve_existing` is refused as a stored rule and acts as the skip directive of an apply
/// range; `use_parent` resolves through the applied parent and its `based_on` chain.
#[test]
fn liquid_rules_validate_seven_members() {
    let wire = ["off", "scale", "recenter", "object_based", "guide_based", "use_parent", "preserve_existing"];
    assert_eq!(StudioLiquidRule::ALL.len(), 7);
    for (rule, w) in StudioLiquidRule::ALL.iter().zip(wire) {
        assert_eq!(serde_json::to_value(rule).unwrap(), json!(w));
        assert_eq!(rule.studio_id(), format!("liquid.{w}"));
    }
    for w in &wire[..6] {
        let v = with("page_rules", json!([{"page_id":"p1","liquid_rule":w}]));
        round_trip(&v);
        assert!(check(&v).is_ok(), "{w}");
    }
    let v = with("page_rules", json!([{"page_id":"p1","liquid_rule":"preserve_existing"}]));
    assert_eq!(code(&v), Code::InvalidField);
    assert_eq!(code(&with("page_rules", json!([{"page_id":"p9","liquid_rule":"scale"}]))), Code::DanglingReference);
    assert_eq!(
        code(&with("page_rules", json!([{"page_id":"p1","liquid_rule":"scale"},{"page_id":"p1","liquid_rule":"off"}]))),
        Code::DuplicateIdentity
    );
    assert_eq!(code(&with("parent_rules", json!([{"parent_id":"Z","liquid_rule":"scale"}]))), Code::DanglingReference);
    let pr = with("parent_rules", json!([{"parent_id":"A","liquid_rule":"scale"}]));
    assert_eq!(check_with(&pr, None).expect_err("no parent set").code, Code::DanglingReference);
    assert_eq!(code(&with("parent_rules", json!([{"parent_id":"A","liquid_rule":"preserve_existing"}]))), Code::InvalidField);

    // use_parent: p1 applies B (no rule, use_parent) based on A (recenter).
    let mut r = StudioLayoutRuleRecords::new(identity("SDOC", 1));
    r.page_rules.push(StudioPageLiquidRule { page_id: "p1".into(), liquid_rule: StudioLiquidRule::UseParent });
    r.page_rules.push(StudioPageLiquidRule { page_id: "p2".into(), liquid_rule: StudioLiquidRule::UseParent });
    r.parent_rules.push(StudioParentLiquidRule { parent_id: "B".into(), liquid_rule: StudioLiquidRule::UseParent });
    r.parent_rules.push(StudioParentLiquidRule { parent_id: "A".into(), liquid_rule: StudioLiquidRule::Recenter });
    let p = parents();
    assert!(validate_layout_rule_records(&r, &document(), Some(&p), Budget::default(), &CancellationToken::default(), &Unresolved).is_ok());
    assert_eq!(resolve_liquid_rule(&r, Some(&p), "p1"), StudioLiquidRule::Recenter);
    assert_eq!(resolve_liquid_rule(&r, Some(&p), "p2"), StudioLiquidRule::Off, "no applied parent");
    assert_eq!(resolve_liquid_rule(&r, None, "p1"), StudioLiquidRule::Off);
    assert_eq!(resolve_liquid_rule(&r, Some(&p), "p3"), StudioLiquidRule::Off, "no stored rule");

    // Apply range: preserve_existing skips p1, the others take the new rule.
    let skipped = r.apply_page_rules(&[
        ("p1".into(), StudioLiquidRule::PreserveExisting),
        ("p2".into(), StudioLiquidRule::GuideBased),
        ("p3".into(), StudioLiquidRule::Scale),
    ]);
    assert_eq!(skipped, 1);
    assert_eq!(r.page_rule("p1"), Some(StudioLiquidRule::UseParent));
    assert_eq!(r.page_rule("p2"), Some(StudioLiquidRule::GuideBased));
    assert_eq!(r.page_rule("p3"), Some(StudioLiquidRule::Scale));

    // Object pins and constraints: over-constrained axes refused, unknown layers dangling.
    let ok = json!([{"layer_id":slyr(1),"pin_left":true,"pin_top":true,"pin_right":true,"pin_bottom":false,
        "width":"relative","height":"fixed"}]);
    round_trip(&with("object_constraints", ok.clone()));
    assert!(check(&with("object_constraints", ok)).is_ok());
    let over_w = json!([{"layer_id":slyr(1),"pin_left":true,"pin_top":false,"pin_right":true,"pin_bottom":false,
        "width":"fixed","height":"fixed"}]);
    assert_eq!(code(&with("object_constraints", over_w)), Code::InvalidField);
    let over_h = json!([{"layer_id":slyr(1),"pin_left":false,"pin_top":true,"pin_right":false,"pin_bottom":true,
        "width":"fixed","height":"fixed"}]);
    assert_eq!(code(&with("object_constraints", over_h)), Code::InvalidField);
    let dangling = json!([{"layer_id":slyr(99),"pin_left":false,"pin_top":false,"pin_right":false,"pin_bottom":false,
        "width":"fixed","height":"fixed"}]);
    assert_eq!(code(&with("object_constraints", dangling)), Code::DanglingReference);
}

/// STU-LAY-011: an alternate layout is a section carrying `alternate_layout` and
/// `alternate_layout_length`; the name is not blank, unique and does not end with `*` or `:`; the
/// page count is validated against the document page order and sections do not overlap.
#[test]
fn alternate_layout_names_follow_the_captured_rules() {
    let alt = |id: &str, name: &str, len: u32, start: &str| {
        json!({"section_id":id,"alternate_layout":name,"alternate_layout_length":len,"page_start":start})
    };
    let good = with("alternate_layouts", json!([alt("h", "iPad H", 2, "p3"), alt("v", "iPad V", 1, "p2")]));
    let r = round_trip(&good);
    assert!(check(&good).is_ok());
    assert_eq!(r.alternate_layout("h").unwrap().alternate_layout_length, 2);
    for (name, rule) in [("", "alternate_layout_blank"), ("   ", "alternate_layout_blank"), ("Phone*", "alternate_layout_suffix"), ("Phone:", "alternate_layout_suffix")] {
        assert_eq!(check_alternate_layout_name(name), Err(rule));
        let d = check(&with("alternate_layouts", json!([alt("h", name, 1, "p3")]))).expect_err("refused");
        assert_eq!(d.code, Code::InvalidField);
        assert!(d.target.ends_with(rule), "{}", d.target);
    }
    assert_eq!(check_alternate_layout_name("Phone *x"), Ok(()));
    assert_eq!(check_alternate_layout_name("tab\tname"), Err("alternate_layout_label"));
    assert_eq!(code(&with("alternate_layouts", json!([alt("h", "Same", 1, "p3"), alt("v", "Same", 1, "p1")]))), Code::DuplicateIdentity);
    assert_eq!(code(&with("alternate_layouts", json!([alt("h", "A", 1, "p3"), alt("h", "B", 1, "p1")]))), Code::DuplicateIdentity);
    assert_eq!(code(&with("alternate_layouts", json!([alt("h", "A", 3, "p3")]))), Code::InvalidField, "past the last page");
    assert_eq!(code(&with("alternate_layouts", json!([alt("h", "A", 0, "p3")]))), Code::InvalidField, "zero pages");
    assert_eq!(code(&with("alternate_layouts", json!([alt("h", "A", u32::MAX, "p3")]))), Code::InvalidField, "hostile length");
    assert_eq!(code(&with("alternate_layouts", json!([alt("h", "A", 1, "p9")]))), Code::DanglingReference);
    let overlap = with("alternate_layouts", json!([alt("h", "A", 2, "p2"), alt("v", "B", 1, "p3")]));
    let d = check(&overlap).expect_err("overlap");
    assert_eq!((d.code, d.target.ends_with("overlap")), (Code::InvalidField, true));
    // Hostile list size: refused before any record is examined.
    let many: Vec<Value> = (0..=MAX_LAYOUT_RULE_RECORDS).map(|_| json!({"page_id":"p1","liquid_rule":"off"})).collect();
    assert_eq!(code(&with("page_rules", Value::Array(many))), Code::BudgetExceeded);
    let tight = Budget { nodes: 1, ..Budget::default() };
    let r: StudioLayoutRuleRecords = serde_json::from_value(good).unwrap();
    let d = validate_layout_rule_records(&r, &document(), None, tight, &CancellationToken::default(), &Unresolved).expect_err("budget");
    assert_eq!(d.code, Code::BudgetExceeded);
}

/// STU-LAY-019 / STU-TYP-125: the linked-story option object (update policy, remove forced line
/// breaks) and the linked-page-item option object (update policy) are separate types on separate
/// lists; link state is derived from the parent fingerprint; links must resolve and must not loop.
#[test]
fn linked_story_and_page_item_options_are_separate() {
    let parent = story(1, "Parent text");
    let fp = linked_story_fingerprint(&parent);
    assert_eq!(fp.len(), 16);
    let mut renamed = parent.clone();
    renamed.story_id = stxt(7);
    assert_eq!(linked_story_fingerprint(&renamed), fp, "the id is not content");
    let links = json!([{"child_story_id":stxt(2),"parent_story_id":stxt(1),
        "options":{"update_policy":"warn_on_change","remove_forced_line_breaks":true},"synced_parent_fingerprint":fp}]);
    let doc = document();
    let item_fp = linked_page_item_fingerprint(&doc.layers[3]);
    let items = json!([{"child_layer_id":slyr(5),"parent_layer_id":slyr(4),
        "options":{"update_policy":"auto_update"},"synced_parent_fingerprint":item_fp}]);
    let mut v = with("linked_stories", links);
    v["linked_page_items"] = items;
    let r = round_trip(&v);
    assert!(check(&v).is_ok());
    let link = r.linked_story(&stxt(2)).unwrap();
    assert_eq!(link.options, StudioLinkedStoryOptions { update_policy: StudioLinkUpdatePolicy::WarnOnChange, remove_forced_line_breaks: true });
    assert_eq!(r.linked_page_items[0].options, StudioLinkedPageItemOptions { update_policy: StudioLinkUpdatePolicy::AutoUpdate });
    assert_eq!(link.state(&parent), StudioLinkState::UpToDate);
    assert_eq!(link.state(&story(1, "Parent text, edited")), StudioLinkState::OutOfDate);
    assert_eq!(r.linked_page_items[0].state(&doc.layers[3]), StudioLinkState::UpToDate);
    let mut hidden = doc.layers[3].clone();
    hidden.visible = false;
    assert_eq!(r.linked_page_items[0].state(&hidden), StudioLinkState::OutOfDate);
    // The page-item options do not accept story-only members, and vice versa.
    let wrong = json!([{"child_layer_id":slyr(5),"parent_layer_id":slyr(4),
        "options":{"update_policy":"auto_update","remove_forced_line_breaks":true},"synced_parent_fingerprint":item_fp}]);
    assert!(serde_json::from_value::<StudioLayoutRuleRecords>(with("linked_page_items", wrong)).is_err());
    let missing = json!([{"child_story_id":stxt(2),"parent_story_id":stxt(1),
        "options":{"update_policy":"auto_update"},"synced_parent_fingerprint":fp}]);
    assert!(serde_json::from_value::<StudioLayoutRuleRecords>(with("linked_stories", missing)).is_err());

    let link = |c: u32, p: u32, f: &str| {
        json!({"child_story_id":stxt(c),"parent_story_id":stxt(p),
            "options":{"update_policy":"auto_update","remove_forced_line_breaks":false},"synced_parent_fingerprint":f})
    };
    assert_eq!(code(&with("linked_stories", json!([link(9, 1, &fp)]))), Code::DanglingReference, "child not shown");
    assert_eq!(code(&with("linked_stories", json!([link(1, 1, &fp)]))), Code::RenderCycle);
    assert_eq!(code(&with("linked_stories", json!([link(2, 1, &fp), link(2, 3, &fp)]))), Code::DuplicateIdentity);
    assert_eq!(code(&with("linked_stories", json!([link(2, 1, &fp), link(1, 2, &fp)]))), Code::RenderCycle);
    assert_eq!(code(&with("linked_stories", json!([link(2, 1, "XYZ")]))), Code::InvalidField);
    assert_eq!(code(&with("linked_stories", json!([link(2, 1, "0123456789ABCDEF")]))), Code::InvalidField, "uppercase");
    assert!(check(&with("linked_stories", json!([link(2, 1, &fp), link(3, 2, &fp)]))).is_ok(), "chains are allowed");
    let item = |c: u32, p: u32| {
        json!({"child_layer_id":slyr(c),"parent_layer_id":slyr(p),"options":{"update_policy":"warn_on_change"},
            "synced_parent_fingerprint":item_fp})
    };
    assert_eq!(code(&with("linked_page_items", json!([item(5, 99)]))), Code::DanglingReference);
    assert_eq!(code(&with("linked_page_items", json!([item(5, 5)]))), Code::RenderCycle);
    assert_eq!(code(&with("linked_page_items", json!([item(5, 4), item(4, 5)]))), Code::RenderCycle);
}

/// Absent layout rules: a document re-serializes byte-identically (no field was added to the
/// document, page, spread or layer structs) and an empty record set encodes only its header.
#[test]
fn absent_layout_rules_keep_documents_byte_identical() {
    let v = document_value();
    let original = serde_json::to_vec(&v).unwrap();
    let d: StudioDocument = serde_json::from_slice(&original).unwrap();
    let again = serde_json::to_vec(&serde_json::to_value(&d).unwrap()).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&again).unwrap(), v);
    let empty = StudioLayoutRuleRecords::new(identity("SDOC", 1));
    assert_eq!(serde_json::to_value(&empty).unwrap(), base());
    assert!(check(&base()).is_ok());
    let mut other = base();
    other["document_id"] = json!(identity("SDOC", 2));
    assert_eq!(code(&other), Code::InvalidField);
    let schema = serde_json::to_value(layout_rule_records_schema()).unwrap();
    assert_eq!(schema["title"], json!("StudioLayoutRuleRecords"));
    let mut unknown = base();
    unknown["layout_rule"] = json!("scale");
    assert!(serde_json::from_value::<StudioLayoutRuleRecords>(unknown).is_err(), "deny_unknown_fields");
}
