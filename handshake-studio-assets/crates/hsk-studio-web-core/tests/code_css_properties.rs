//! Lane SMB-WEB-CODE-1: [STU-WEB-060] to [STU-WEB-076] (CSS property surface, contracts, media-query records and the
//! four-level rule edit), with hostile style-sheet text (V4-08: escapes, comment decoys, case and whitespace).
use hsk_studio_accord::CancellationToken;
use hsk_studio_web_core::css_properties::{
    self as css, ANIMATABLE_PROPERTIES, BoundSource, CURATED, Category, ControlType, DeclarationChange, ItemKind, LENGTH_UNITS, OPACITY_CONTRACT, RESOLUTION_UNITS, TIMING_FUNCTIONS, TRANSITION_PSEUDO_CLASSES,
    TRANSITION_VENDOR_PREFIXES, compose_number, numeric_contract, parse_media_query_list, property, rule_edit, scan_css, shipped_prefix_rules,
};
use hsk_studio_web_core::{Limits, SourcePatch, WebDoc, apply, parse};

fn doc(src: &str) -> WebDoc {
    parse(src, 1, &Limits::default(), &CancellationToken::default()).unwrap()
}

fn style_of(d: &WebDoc) -> usize {
    css::style_elements(d)[0]
}

fn set(p: &str, v: &str) -> DeclarationChange {
    DeclarationChange { property: p.into(), value: Some(v.into()) }
}

fn edited(d: &WebDoc, media: Option<&str>, selector: &str, changes: &[DeclarationChange]) -> String {
    let outcome = rule_edit(d, style_of(d), media, selector, changes, &shipped_prefix_rules()).unwrap();
    let out = apply(d, &SourcePatch::new(d, vec![outcome.edit])).unwrap().source;
    parse(&out, 2, &Limits::default(), &CancellationToken::default()).expect("the edited document still parses");
    out
}

#[test]
fn categories_controls_and_units_are_the_closed_sets() {
    assert_eq!(Category::ALL.map(Category::id), ["layout", "text", "border", "background", "more"]);
    assert_eq!(Category::More.slots(), None);
    assert_eq!(ControlType::ALL.len(), 14);
    assert_eq!(LENGTH_UNITS, ["px", "pt", "pc", "in", "cm", "mm", "%", "em", "rem", "ex", "ch", "vw", "vh", "vmin", "vmax"]);
    assert_eq!(RESOLUTION_UNITS, ["dpi", "dpcm", "dppx"]);
    let count = |c: Category| CURATED.iter().filter(|e| e.category == c).count();
    assert_eq!((count(Category::Layout), count(Category::Text), count(Category::Border), count(Category::Background)), (22, 19, 4, 9));
    assert_eq!(property("display").unwrap().option_count(), 19);
    assert_eq!(property("font-weight").unwrap().option_count(), 13);
    assert_eq!(property("list-style-type").unwrap().option_count(), 21);
    assert_eq!(property("width").unwrap().option_list().first().copied().flatten(), Some("auto"));
    assert_eq!(property("font-size").unwrap().units().len(), 11, "the eleven non-viewport units");
    assert_eq!(property("text-indent").unwrap().units(), ["px", "pt", "pc", "in", "cm", "mm", "%"]);
    assert!(property("border").unwrap().option_list().contains(&None), "separators are dividers, not values");
    assert_eq!(ANIMATABLE_PROPERTIES.len(), 49);
    assert_eq!((TRANSITION_PSEUDO_CLASSES.len(), TIMING_FUNCTIONS.len(), TRANSITION_VENDOR_PREFIXES.len()), (8, 6, 2));
    assert_eq!(css::GRADIENT_SAVED_SWATCHES_MAX, 5);
    let svg = css::SVG_WEB_EXPORT_DEFAULTS;
    assert_eq!((svg.trim_to_art_bounds, svg.responsive, svg.decimal_precision), (true, true, 1));
    assert_eq!(css::SVG_DECIMAL_PRECISION_CONTRACT.hard_max.source, BoundSource::StudioDeclared);
    assert!(css::is_recognised("BORDER-TOP-WIDTH") && css::is_recognised("-webkit-transition") && !css::is_recognised("grid-template"));
}

#[test]
fn numeric_contracts_label_every_bound_and_keep_the_unit_apart() {
    assert_eq!((OPACITY_CONTRACT.hard_min.value, OPACITY_CONTRACT.hard_max.value, OPACITY_CONTRACT.precision, OPACITY_CONTRACT.fine_step), (0.0, 1.0, 2, 0.001));
    let width = numeric_contract(property("width").unwrap()).unwrap();
    assert_eq!((width.hard_min.value, width.hard_min.source), (0.0, BoundSource::Declared), "no negatives on width");
    assert_eq!(width.hard_max.source, BoundSource::StudioDeclared);
    let margin = numeric_contract(property("margin").unwrap()).unwrap();
    assert_eq!((margin.hard_min.value, margin.hard_min.source, margin.soft_min.value), (-100_000.0, BoundSource::StudioDeclared, -1000.0));
    assert_eq!(compose_number(property("width").unwrap(), 12.34567, Some("px")).unwrap(), "12.346px");
    assert_eq!(compose_number(property("margin").unwrap(), -5.0, Some("rem")).unwrap(), "-5rem");
    assert_eq!(compose_number(property("opacity").unwrap(), 0.456, None).unwrap(), "0.46");
    assert_eq!(compose_number(property("width").unwrap(), 0.0, None).unwrap(), "0");
    let code = |p: &str, n: f64, u: Option<&str>| compose_number(property(p).unwrap(), n, u).unwrap_err().code;
    assert_eq!(code("width", -1.0, Some("px")), "value_out_of_range");
    assert_eq!(code("opacity", 1.5, None), "value_out_of_range");
    assert_eq!(code("opacity", 0.5, Some("%")), "unit_not_offered");
    assert_eq!(code("text-indent", 2.0, Some("vw")), "unit_not_offered");
    assert_eq!(code("width", 5.0, None), "unit_missing");
    assert_eq!(code("width", f64::NAN, Some("px")), "value_not_finite");
    assert_eq!(code("display", 1.0, None), "value_needs_text");
}

#[test]
fn media_queries_become_records_and_the_closed_sets_refuse() {
    let r = parse_media_query_list("screen and (min-width: 600px) and (orientation: landscape)").unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].media_type, Some("screen"));
    assert_eq!((r[0].conditions[0].feature, r[0].conditions[0].operator, r[0].conditions[0].value.as_str(), r[0].conditions[0].unit), ("min-width", ":", "600", Some("px")));
    assert_eq!(r[0].conditions[1].value, "landscape");
    let two = parse_media_query_list("print, (max-width: 40em)").unwrap();
    assert_eq!((two[0].media_type, two[1].media_type, two[1].conditions[0].unit), (Some("print"), None, Some("em")));
    assert_eq!(parse_media_query_list("all and (min-resolution: 2dppx)").unwrap()[0].media_type, None, "all restricts nothing");
    assert_eq!(parse_media_query_list("(aspect-ratio: 16/9)").unwrap()[0].conditions[0].value, "16/9");
    assert_eq!(parse_media_query_list("(width >= 600px)").unwrap()[0].conditions[0].operator, ">=");
    let code = |q: &str| parse_media_query_list(q).unwrap_err().code;
    assert_eq!(code("not screen"), "media_query_negation_unsupported");
    assert_eq!(code("speech"), "media_type_unknown");
    assert_eq!(code("(prefers-color-scheme: dark)"), "media_feature_unknown");
    assert_eq!(code("(min-width: 10furlongs)"), "media_unit_unknown");
    assert_eq!(code("(min-width >= 10px)"), "media_syntax_unsupported");
    assert_eq!(code("(aspect-ratio: 0/9)"), "media_value_invalid");
    assert_eq!(code("(min-width: 600px) or (max-width: 9px)"), "media_syntax_unsupported");
}

#[test]
fn hostile_media_text_is_tokenised_not_substring_matched() {
    // Escape: `scr\65 en` is `screen` after escape decoding.
    assert_eq!(parse_media_query_list("scr\\65 en and (min-width:1px)").unwrap()[0].media_type, Some("screen"));
    // Comment decoy: the commented `print` is no media type.
    assert_eq!(parse_media_query_list("/* print, */ screen").unwrap().len(), 1);
    // Case and whitespace.
    let r = parse_media_query_list("  SCREEN   AND(MIN-WIDTH:600PX)").err();
    assert_eq!(r.map(|e| e.code), Some("media_syntax_unsupported"), "`and(` is no `and` token");
    assert_eq!(parse_media_query_list("  SCREEN \n AND  ( MIN-WIDTH : 600PX )").unwrap()[0].conditions[0].unit, Some("px"));
}

#[test]
fn style_sheets_scan_by_tokens() {
    // A brace in a string, a comment holding a rule, an escaped brace in a selector.
    let text = "a[title=\"}{\"] { color: red }\n/* .x { color: blue } */\n.a\\{b { margin: 0 }\n@MEDIA screen { p { top: 0 } }\n@import url(x.css);";
    let items = scan_css(text, 0..text.len()).unwrap();
    let kinds: Vec<ItemKind> = items.iter().map(|i| i.kind).collect();
    assert_eq!(kinds, [ItemKind::Rule, ItemKind::Rule, ItemKind::Media, ItemKind::Statement]);
    assert_eq!(&text[items[1].prelude.clone()], ".a\\{b");
    assert_eq!(items[2].children.len(), 1);
    assert_eq!(css::media_query_text(text, &items[2]).trim(), "screen");
    let code = |t: &str| scan_css(t, 0..t.len()).unwrap_err().code;
    assert_eq!(code("p { color: red"), "css_unterminated_block");
    assert_eq!(code("p { content: \"x\n\" }"), "css_unterminated_string");
    assert_eq!(code("/* open"), "css_unterminated_comment");
    assert_eq!(code("} p {}"), "css_unbalanced_brace");
    let deep = format!("@media a {{{}", "@media b {".repeat(40));
    assert!(scan_css(&deep, 0..deep.len()).is_err(), "nesting is bounded");
}

#[test]
fn one_call_creates_a_rule_in_a_named_media_query_of_a_named_sheet() {
    let src = "<!doctype html>\n<html><head>\n  <style id=\"main\">\n    p { color: red; }\n  </style>\n</head><body><p>x</p></body></html>\n";
    let d = doc(src);
    let out = edited(&d, Some("screen and (min-width: 600px)"), ".card", &[set("padding", "4px"), set("transition", "opacity 1s")]);
    assert!(out.contains("    p { color: red; }\n"), "untouched bytes stay: {out}");
    assert!(out.contains("@media screen and (min-width: 600px) {\n      .card {\n        padding: 4px;\n        -webkit-transition: opacity 1s;\n        -o-transition: opacity 1s;\n        transition: opacity 1s;\n      }\n    }\n  </style>"), "{out}");
    // The same media query (as records: case and spacing do not count) is found again: a second rule goes into it.
    let d2 = doc(&out);
    let out2 = edited(&d2, Some("SCREEN  and (min-width:600px)"), "h1", &[set("color", "blue")]);
    assert_eq!(out2.matches("@media").count(), 1, "{out2}");
    assert!(out2.contains("      h1 {\n        color: blue;\n      }\n    }"), "{out2}");
}

#[test]
fn an_existing_rule_is_changed_in_place_and_removal_takes_the_emitted_prefixes() {
    let src = "<style>\n  .a {\n    color: red;\n    margin: 0;\n  }\n</style><p class=a>x</p>";
    let d = doc(src);
    let out = edited(&d, None, ".a", &[set("color", "blue"), set("transition", "top 2s")]);
    assert!(out.starts_with("<style>\n  .a {\n    color: blue;\n    margin: 0;\n    -webkit-transition: top 2s;\n    -o-transition: top 2s;\n    transition: top 2s;\n  }\n</style>"), "{out}");
    let d2 = doc(&out);
    let outcome = rule_edit(&d2, style_of(&d2), None, ".a", &[DeclarationChange { property: "transition".into(), value: None }], &shipped_prefix_rules()).unwrap();
    assert_eq!(outcome.removed, ["-webkit-transition", "-o-transition", "transition"]);
    let out2 = apply(&d2, &SourcePatch::new(&d2, vec![outcome.edit])).unwrap().source;
    assert_eq!(out2, "<style>\n  .a {\n    color: blue;\n    margin: 0;\n  }\n</style><p class=a>x</p>", "removing the base removes its prefixes in the same edit");
    // A one-line rule stays one line.
    let d3 = doc("<style>p{color:red}</style>");
    assert_eq!(edited(&d3, None, "p", &[set("margin", "0")]), "<style>p{color:red; margin: 0;}</style>");
}

#[test]
fn rule_edits_refuse_text_that_would_escape_the_sheet() {
    let d = doc("<style>p { color: red }</style><div></div>");
    let code = |media: Option<&str>, sel: &str, p: &str, v: Option<&str>| rule_edit(&d, style_of(&d), media, sel, &[DeclarationChange { property: p.into(), value: v.map(str::to_string) }], &[]).unwrap_err().code;
    assert_eq!(code(None, "p", "color", Some("red</style><script>x()</script>")), "css_text_unsafe");
    assert_eq!(code(None, "p", "color", Some("red; background: url(x)")), "css_value_invalid");
    assert_eq!(code(None, "p { } q", "color", Some("red")), "css_selector_invalid");
    assert_eq!(code(None, "p", "co lor", Some("red")), "css_property_invalid");
    assert_eq!(code(None, "p", "color", Some("red /* x */")), "css_text_unsafe");
    assert_eq!(code(None, "p", "margin", None), "css_property_absent");
    assert_eq!(code(None, "h1", "margin", None), "css_rule_absent");
    assert_eq!(code(Some("speech"), "p", "margin", Some("0")), "media_type_unknown");
    assert_eq!(code(None, "p", "color", Some("red")), "nothing_to_change");
    let div = (0..d.len()).find(|&i| d.local_name(i).as_deref() == Some("div")).unwrap();
    assert_eq!(rule_edit(&d, div, None, "p", &[set("top", "0")], &[]).unwrap_err().code, "not_a_style_element");
}

#[test]
fn crlf_documents_get_crlf_rules() {
    let d = doc("<style>\r\n  p { top: 0 }\r\n</style>");
    let out = edited(&d, None, "h2", &[set("left", "0")]);
    assert_eq!(out, "<style>\r\n  p { top: 0 }\r\n  h2 {\r\n    left: 0;\r\n  }\r\n</style>");
}

#[test]
fn the_code_navigator_lists_the_rules_matching_an_element() {
    let d = doc("<style>p { top: 0 } .x { left: 0 } #y p { right: 0 } @media print { p.x { color: red } }</style><p class=x id=z>t</p>");
    let p = (0..d.len()).find(|&i| d.local_name(i).as_deref() == Some("p")).unwrap();
    let (rules, unsupported) = css::affecting_rules(&d, p).unwrap();
    let selectors: Vec<(&str, Option<&str>, u32)> = rules.iter().map(|r| (r.selector.as_str(), r.media.as_deref(), r.specificity)).collect();
    assert_eq!(selectors, [("p", None, 1), (".x", None, 10), ("p.x", Some("print"), 11)]);
    assert_eq!(unsupported, 1, "the descendant selector is listed as unsupported, not guessed");
}

#[test]
fn media_records_come_from_the_document() {
    let d = doc("<style>@media screen and (max-width: 30em) { .a {top:0} .b {top:1px} } @media (hover: hover) { p {} }</style>");
    let blocks = css::document_media_records(&d);
    assert_eq!(blocks.len(), 2);
    let r = blocks[0].records.as_ref().unwrap();
    assert_eq!((r[0].media_type, r[0].rules.clone()), (Some("screen"), vec![".a".to_string(), ".b".to_string()]));
    assert_eq!(blocks[1].records.as_ref().unwrap_err().code, "media_feature_unknown");
}
