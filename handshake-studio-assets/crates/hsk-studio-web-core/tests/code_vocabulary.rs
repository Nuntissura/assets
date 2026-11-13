//! Lane SMB-WEB-CODE-1: [STU-WEB-017] to [STU-WEB-027] (tag libraries, definitions, formatting rules, attribute
//! vocabulary, cross-tag groups, case, inspector fallback, registration).
use hsk_studio_web_core::vocabulary::{
    AllowedValue, AttributeDefinition, AttributeGroup, FORMATTING_KEYS, FormattingRules, HasEndTag, InspectorBinding, TagDefinition, TagLibrary, TagRegistry, TagType, ValueType,
};

fn library(id: &str, applies: &[&str]) -> TagLibrary {
    TagLibrary { library_id: id.into(), display_name: id.into(), applies_to_document_types: applies.iter().map(|s| (*s).to_string()).collect(), namespace_prefix: None, declared_tag_count: 1, tag_chooser_id: None }
}

fn tag(name: &str, lib: &str, attrs: Vec<AttributeDefinition>, groups: &[&str]) -> TagDefinition {
    TagDefinition {
        tag_name: name.into(),
        library_id: lib.into(),
        has_end_tag: HasEndTag::Yes,
        tag_type: Some(TagType::Nonempty),
        formatting_rules: FormattingRules::INLINE,
        property_inspector_id: None,
        attributes: attrs,
        groups: groups.iter().map(|s| (*s).to_string()).collect(),
    }
}

#[test]
fn the_ten_libraries_and_their_deltas_are_reported() {
    let r = TagRegistry::shipped();
    let ids: Vec<&str> = r.libraries().iter().map(|l| l.library_id.as_str()).collect();
    assert_eq!(ids, ["html", "cfml", "aspnet", "jsp", "jrun", "asp", "php", "templates", "xslt", "svg"]);
    assert_eq!(r.libraries().iter().map(|l| l.declared_tag_count).sum::<u32>(), 466, "466 index-declared references");
    let html = &r.libraries()[0];
    for t in ["html", "php", "library_item", "xslt_fragment", "template_html", "template_php", "template_aspnet_vb"] {
        assert!(html.applies_to_document_types.iter().any(|x| x == t), "html applies to {t}");
    }
    assert_eq!(r.libraries()[2].namespace_prefix.as_deref(), Some("<asp:"));
    // Every library whose loaded count differs from its declared count is a load diagnostic, never silent.
    let deltas: Vec<&str> = r.load_diagnostics().iter().filter(|d| d.code == "library_definition_delta").map(|d| d.library_id.as_str()).collect();
    assert_eq!(deltas.len(), 10, "{deltas:?}");
    let html_delta = r.load_diagnostics().iter().find(|d| d.library_id == "html").unwrap();
    assert_eq!(html_delta.declared, Some(121));
    assert_eq!(html_delta.loaded, Some(u32::try_from(r.tags().len()).unwrap()));
    assert!(r.tags().len() >= 100, "the HTML element index: {}", r.tags().len());
    // Resolution follows applies_to: svg resolves the svg library only.
    assert_eq!(r.libraries_for("svg").iter().map(|l| l.library_id.as_str()).collect::<Vec<_>>(), ["svg"]);
    assert_eq!(r.libraries_for("css").len(), 0);
}

#[test]
fn definitions_carry_shape_rules_and_merged_attributes() {
    let r = TagRegistry::shipped();
    let img = r.tag("html", "IMG").expect("tag names are case-insensitive in HTML types");
    assert_eq!((img.has_end_tag, img.tag_type), (HasEndTag::No, Some(TagType::Empty)));
    let pre = r.tag("html", "pre").unwrap();
    assert!(!pre.formatting_rules.formatcontents, "pre is verbatim");
    for t in ["script", "style", "textarea"] {
        assert!(!r.tag("html", t).unwrap().formatting_rules.formatcontents, "{t} is verbatim");
    }
    let attrs = r.attributes_of(img);
    assert!(attrs.iter().any(|a| a.name == "src" && a.value_type == ValueType::RelativePath));
    assert!(attrs.iter().any(|a| a.name == "width" && a.value_type == ValueType::WidthSrc && a.value_type.derived()));
    assert!(attrs.iter().any(|a| a.name == "class" && a.value_type == ValueType::CssClass), "global group merged");
    let input_type = r.attribute(r.tag("html", "input").unwrap(), "TYPE").unwrap();
    assert_eq!(input_type.allowed_values.first().map(|v| v.value.as_str()), Some("hidden"), "declared order, not sorted");
    assert!(input_type.allows("Checkbox") && !input_type.allows("slider"));
    assert_eq!(r.inspector(img), InspectorBinding::GenericAttributeTable);
    assert_eq!(r.inspector(img).token(), "generic_attribute_table");
    assert!(r.tag("svg", "div").is_none(), "svg does not resolve the html library");
    assert!(r.tag("html", "blink").is_none());
}

#[test]
fn formatting_rules_are_a_closed_set() {
    assert_eq!(FORMATTING_KEYS.len(), 11);
    let r = FormattingRules::from_pairs(&[("nlbeforetag", "1"), ("indentcontents", "yes"), ("formatcontents", "no"), ("preserveattrcase", "yes")]).unwrap();
    assert!(r.nlbeforetag == 1 && r.indentcontents && !r.formatcontents && r.preserveattrcase);
    assert_eq!(r.pairs().len(), 11);
    assert_eq!(FormattingRules::INLINE.pairs().len(), 10, "preserveattrcase only when present");
    assert_eq!(FormattingRules::from_pairs(&[("wrap", "1")]).unwrap_err().code, "formatting_key_unknown");
    assert_eq!(FormattingRules::from_pairs(&[("nlbeforetag", "2")]).unwrap_err().code, "formatting_value_invalid");
    assert_eq!(FormattingRules::from_pairs(&[("nlbeforetag_max", "3")]).unwrap_err().code, "formatting_value_invalid");
    assert_eq!(FormattingRules::from_pairs(&[("preserveattrcase", "no")]).unwrap_err().code, "formatting_value_invalid");
}

#[test]
fn value_types_are_twenty_normalised_to_lower_case() {
    assert_eq!(ValueType::ALL.len(), 20);
    assert_eq!(ValueType::parse("Width:Src"), Some(ValueType::WidthSrc));
    assert_eq!(ValueType::parse("CSSCLASS"), Some(ValueType::CssClass));
    assert_eq!(ValueType::parse("number"), None);
    assert_eq!(AttributeDefinition::new("dir", ValueType::Enumerated, &[]).unwrap_err().code, "allowed_values_missing");
    assert_eq!(AttributeDefinition::new("title", ValueType::Text, &["a"]).unwrap_err().code, "allowed_values_without_enumerated");
    assert_eq!(AttributeDefinition::new("1bad", ValueType::Text, &[]).unwrap_err().code, "vocabulary_name_invalid");
}

#[test]
fn registration_extends_without_editing_and_refuses_typed() {
    let mut r = TagRegistry::shipped();
    let shipped_html = r.libraries()[0].clone();
    let group = AttributeGroup { group_id: "kit_global".into(), attributes: vec![AttributeDefinition::new("theme", ValueType::Text, &[]).unwrap()] };
    let own_theme = AttributeDefinition { name: "theme".into(), value_type: ValueType::Enumerated, case_sensitive: false, allowed_values: vec![AllowedValue { value: "dark".into(), caption: Some("Dark".into()) }] };
    let diags = r.register(library("kit", &["html", "php"]), vec![tag("kit-card", "kit", vec![own_theme], &["kit_global"])], vec![group]).unwrap();
    assert!(diags.iter().any(|d| d.code == "cross_tag_attribute_collision" && d.subject.as_deref() == Some("kit-card/kit_global/theme")), "{diags:?}");
    assert!(r.is_registered("kit") && !r.is_registered("html"));
    assert_eq!(r.libraries()[0], shipped_html, "a shipped library is never edited");
    let card = r.tag("php", "kit-card").unwrap();
    let theme = r.attribute(card, "theme").unwrap();
    assert_eq!(theme.value_type, ValueType::Enumerated, "the tag's own definition wins the collision");
    assert_eq!(r.attributes_of(card).iter().filter(|a| a.name == "theme").count(), 1, "merged, not duplicated");
    // Typed refusals, nothing registered.
    let before = r.libraries().len();
    assert_eq!(r.register(library("kit", &["html"]), vec![], vec![]).unwrap_err().code, "library_id_taken");
    assert_eq!(r.register(library("kit2", &["cobol"]), vec![], vec![]).unwrap_err().code, "document_type_unknown");
    assert_eq!(r.register(library("kit3", &["html"]), vec![tag("x-a", "other", vec![], &[])], vec![]).unwrap_err().code, "tag_library_mismatch");
    assert_eq!(r.register(library("kit4", &["html"]), vec![tag("x-b", "kit4", vec![], &["nope"])], vec![]).unwrap_err().code, "group_unknown");
    assert_eq!(r.register(library("Kit 5", &["html"]), vec![], vec![]).unwrap_err().code, "vocabulary_id_invalid");
    assert_eq!(r.register(library("kit6", &["html"]), vec![tag("<script>", "kit6", vec![], &[])], vec![]).unwrap_err().code, "vocabulary_name_invalid");
    let many: Vec<TagDefinition> = (0..9000).map(|i| tag(&format!("t{i}"), "kit7", vec![], &[])).collect();
    assert_eq!(r.register(library("kit7", &["html"]), many, vec![]).unwrap_err().code, "vocabulary_too_large");
    assert_eq!(r.libraries().len(), before);
}

#[test]
fn xml_family_types_match_tags_exactly() {
    let mut r = TagRegistry::shipped();
    r.register(library("vec", &["svg"]), vec![tag("linearGradient", "vec", vec![], &[])], vec![]).unwrap();
    assert!(r.tag("svg", "linearGradient").is_some());
    assert!(r.tag("svg", "lineargradient").is_none(), "XML-family types are case-sensitive");
}
