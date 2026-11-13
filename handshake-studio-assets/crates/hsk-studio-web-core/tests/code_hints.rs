//! Lane SMB-WEB-CODE-1: [STU-WEB-045] to [STU-WEB-051] (schemes, region ownership, code hints) and [STU-WEB-059]
//! (balance braces, parent tag, fold ranges).
use hsk_studio_accord::CancellationToken;
use hsk_studio_web_core::hints::{self, BlockDelimiter, ColouringScheme, SchemeRegistry};
use hsk_studio_web_core::vocabulary::TagRegistry;
use hsk_studio_web_core::doc_types::Work;
use hsk_studio_web_core::{Limits, WebDoc, parse};

fn work_regions(d: &WebDoc, type_id: &str) -> Vec<hints::Region> {
    let cancel = CancellationToken::default();
    hints::regions(d, &SchemeRegistry::shipped(), type_id, &mut Work::new(&cancel)).unwrap()
}

fn balance(d: &WebDoc, r: std::ops::Range<usize>) -> Result<std::ops::Range<usize>, hsk_studio_web_core::doc_types::CodeError> {
    let cancel = CancellationToken::default();
    hints::balance(d, &SchemeRegistry::shipped(), "html", r, &mut Work::new(&cancel))
}

fn doc(src: &str) -> WebDoc {
    parse(src, 1, &Limits::default(), &CancellationToken::default()).unwrap()
}

fn at(src: &str, needle: &str) -> usize {
    src.find(needle).unwrap_or_else(|| panic!("{needle} in {src}"))
}

const PAGE: &str = "<!doctype html><html><head><style>p { color: red; } .a { margin: 0 }</style><script>if (a) { b(\"}\"); }</script><script type=\"application/ld+json\">{\"a\": 1}</script></head><body><!-- note --><input type=\"text\" class=\"x\"><p id=\"q\">Hi <b>there</b></p></body></html>";

#[test]
fn regions_come_from_the_tokenizer_spans_and_name_their_owner() {
    let d = doc(PAGE);
    let regions = work_regions(&d, "html");
    let owners: Vec<(&str, &str)> = regions.iter().map(|r| (r.scheme_id.as_str(), &PAGE[r.range.clone()])).filter(|(s, _)| *s != "html").collect();
    assert_eq!(owners, [("css", "p { color: red; } .a { margin: 0 }"), ("javascript", "if (a) { b(\"}\"); }"), ("text", "{\"a\": 1}"), ("html_comment", "<!-- note -->")]);
    let css = regions.iter().find(|r| r.scheme_id == "css").unwrap();
    assert_eq!((css.decision, css.open_delimiter.as_deref(), css.close_delimiter.as_deref(), css.enclosing.as_deref()), ("embedded_block", Some("<style"), Some("</style>"), Some("html")));
    // The regions cover the document without overlap.
    assert_eq!(regions.first().unwrap().range.start, 0);
    assert_eq!(regions.last().unwrap().range.end, PAGE.len());
    assert!(regions.windows(2).all(|w| w[0].range.end == w[1].range.start));
    assert_eq!(hints::region_at(&regions, at(PAGE, "color")).unwrap().scheme_id, "css");
    assert_eq!(hints::region_at(&regions, at(PAGE, "Hi")).unwrap().scheme_id, "html");
}

#[test]
fn ambiguous_boundaries_are_refused_at_registration() {
    let mut r = SchemeRegistry::shipped();
    assert_eq!(r.schemes().iter().find(|s| s.scheme_id == "html").unwrap().priority, hints::BAND_DOCUMENT);
    let scheme = |id: &str, blocks: Vec<BlockDelimiter>| ColouringScheme {
        scheme_id: id.into(),
        display_name: id.into(),
        applies_to_document_types: vec!["php".into()],
        priority: hints::BAND_DOCUMENT,
        ignore_case: true,
        ignore_tags: false,
        keyword_lists: Vec::new(),
        delimiters: Vec::new(),
        embedded_block_delimiters: blocks,
        tag_specs: Vec::new(),
        sample_text: String::new(),
    };
    let block = |o: &str, c: &str, s: &str| BlockDelimiter { open: o.into(), close: c.into(), scheme_id: s.into() };
    assert_eq!(r.register(scheme("php_page", vec![block("<?php", "?>", "javascript"), block("<?PHP", "?>", "css")])).unwrap_err().code, "delimiter_ambiguous");
    assert_eq!(r.register(scheme("php_page", vec![block("<%", "%>", "cobol")])).unwrap_err().code, "delimiter_scheme_unknown");
    assert_eq!(r.register(scheme("html", vec![])).unwrap_err().code, "scheme_id_taken");
    r.register(scheme("php_page", vec![block("<?php", "?>", "javascript")])).unwrap();
    assert!(r.scheme("php_page").is_some());
}

#[test]
fn hint_groups_are_built_from_the_vocabulary_and_flag_missing_descriptions() {
    let groups = hints::hint_groups(&TagRegistry::shipped(), "html");
    let ids: Vec<&str> = groups.iter().map(|g| g.group_id).collect();
    assert_eq!(ids, ["html_tags", "html_attributes", "html_attribute_values", "css_properties", "css_values"]);
    assert!(groups.iter().all(|g| !g.described()), "no descriptions yet: every group is flagged");
    let values = groups.iter().find(|g| g.group_id == "css_values").unwrap();
    let display = values.hints.iter().find(|h| h.pattern == "display:").unwrap();
    assert_eq!((display.display_restriction, display.items.len(), display.items[0].value.as_str()), ("css", 19, "inherit"));
    assert!(values.hints.iter().find(|h| h.pattern == "margin:").unwrap().allow_multiple_values);
}

#[test]
fn the_hint_at_an_offset_follows_the_language_there() {
    let d = doc(PAGE);
    let v = TagRegistry::shipped();
    let schemes = SchemeRegistry::shipped();
    let cancel = CancellationToken::default();
    let hint = |offset: usize| hints::hints_at(&d, &v, &schemes, "html", offset, &mut Work::new(&cancel)).unwrap();
    // In a CSS value after `color: r`.
    let h = hint(at(PAGE, "red").saturating_add(1)).unwrap();
    assert_eq!((h.group_id, h.prefix.as_str()), ("css_values", "r"));
    assert!(h.items.is_empty(), "color has no keyword options; nothing matches `r`");
    let h = hint(at(PAGE, "margin").saturating_add(3)).unwrap();
    assert_eq!((h.group_id, h.prefix.as_str()), ("css_properties", "mar"));
    assert!(h.items.iter().any(|i| i.value == "margin") && h.items.iter().all(|i| i.value.starts_with("mar")));
    // In a start tag: the name, an attribute name, an enumerated value.
    let h = hint(at(PAGE, "nput").saturating_add(2)).unwrap();
    assert_eq!((h.group_id, h.prefix.as_str()), ("html_tags", "inp"));
    let h = hint(at(PAGE, "\"text\"").saturating_add(3)).unwrap();
    assert_eq!((h.group_id, h.prefix.as_str()), ("html_attribute_values", "te"));
    assert_eq!(h.items.iter().map(|i| i.value.as_str()).collect::<Vec<_>>(), ["text", "tel"]);
    let h = hint(at(PAGE, "class=\"x\"").saturating_add(2)).unwrap();
    assert_eq!((h.group_id, h.prefix.as_str()), ("html_attributes", "cl"));
    assert_eq!(h.items.first().map(|i| i.value.as_str()), Some("class"));
    assert!(hint(at(PAGE, "Hi")).is_none(), "plain text: no hint");
    assert!(hint(at(PAGE, "b(")).is_none(), "script: no hint group here");
    assert_eq!(hints::hints_at(&d, &v, &schemes, "html", PAGE.len().saturating_add(1), &mut Work::new(&cancel)).unwrap_err().code, "offset_invalid");
}

#[test]
fn navigation_balances_brackets_by_language_and_walks_tags() {
    let d = doc(PAGE);
    // In the script, `}` inside the string is no bracket.
    let inner = balance(&d, at(PAGE, "b(")..at(PAGE, "b(")).unwrap();
    assert_eq!(&PAGE[inner], "{ b(\"}\"); }");
    let css = balance(&d, at(PAGE, "color")..at(PAGE, "color")).unwrap();
    assert_eq!(&PAGE[css], "{ color: red; }");
    // In markup: the enclosing element, then outward.
    let b = at(PAGE, "there");
    let first = balance(&d, b..b).unwrap();
    assert_eq!(&PAGE[first.clone()], "<b>there</b>");
    let second = balance(&d, first).unwrap();
    assert_eq!(&PAGE[second], "<p id=\"q\">Hi <b>there</b></p>");
    let q = d.find_by_html_id("q").unwrap();
    let parent = hints::parent_tag(&d, q).unwrap();
    assert_eq!(d.local_name(parent).as_deref(), Some("body"));
    assert_eq!(&PAGE[hints::full_tag_range(&d, q).unwrap()], "<p id=\"q\">Hi <b>there</b></p>");
    assert_eq!(hints::parent_tag(&d, d.find_by_tag("html")[0]).unwrap_err().code, "no_parent_tag");
    assert_eq!(hints::selection_range(&d, 5..5).unwrap_err().code, "range_invalid");
    assert_eq!(balance(&d, 0..PAGE.len()).unwrap_err().code, "nothing_encloses");
}

/// Review F8: regions follow the document type; the offset before `</style>` is the style's; registered delimiters
/// own their nodes and cross-scheme duplicates are refused; the JavaScript MIME essences all run as script.
#[test]
fn regions_follow_the_type_and_the_registered_schemes() {
    let css = doc("p{color:red}");
    let r = work_regions(&css, "css");
    assert_eq!((r.len(), r[0].scheme_id.as_str(), r[0].range.clone()), (1, "css", 0..12));
    assert_eq!(work_regions(&doc("let a = 1;"), "javascript")[0].scheme_id, "javascript");
    assert_eq!(work_regions(&doc("<a>b</a>"), "xml")[0].scheme_id, "text", "no language applies: plain text");
    let page = "<style>p{col</style>";
    let r = work_regions(&doc(page), "html");
    assert_eq!(hints::region_at(&r, page.find("</style>").unwrap()).unwrap().scheme_id, "css");
    for ty in ["text/x-javascript", "application/x-ecmascript", "text/livescript", "text/javascript1.5", "module", "TEXT/JAVASCRIPT; charset=utf-8"] {
        let src = format!("<script type=\"{ty}\">a()</script>");
        let r = work_regions(&doc(&src), "html");
        assert_eq!(r.iter().find(|x| x.decision == "embedded_block").unwrap().scheme_id, "javascript", "{ty}");
    }
    let mut schemes = SchemeRegistry::shipped();
    let scheme = |id: &str, open: &str| ColouringScheme {
        scheme_id: id.into(),
        display_name: id.into(),
        applies_to_document_types: vec!["php".into()],
        priority: hints::BAND_DOCUMENT,
        ignore_case: true,
        ignore_tags: false,
        keyword_lists: Vec::new(),
        delimiters: Vec::new(),
        embedded_block_delimiters: vec![BlockDelimiter { open: open.into(), close: "?>".into(), scheme_id: "javascript".into() }],
        tag_specs: Vec::new(),
        sample_text: String::new(),
    };
    schemes.register(scheme("php_a", "<?php")).unwrap();
    assert_eq!(schemes.register(scheme("php_b", "<?PHP")).unwrap_err().code, "delimiter_ambiguous", "two schemes for one type on one opener");
    let src = "<p>x</p><?php echo 1; ?><p>y</p>";
    let cancel = CancellationToken::default();
    let r = hints::regions(&doc(src), &schemes, "php", &mut Work::new(&cancel)).unwrap();
    let owned = r.iter().find(|x| x.decision == "embedded_block").expect("the registered delimiter owns the node");
    assert_eq!((owned.scheme_id.as_str(), &src[owned.range.clone()]), ("javascript", " echo 1; "));
}

/// Review F1: the lexer knows line comments, strings, template literals with expressions and regular expressions.
#[test]
fn the_lexer_reads_script_tokens() {
    use hints::TokenKind::{BlockComment, LineComment, Regex, String as Str, Template};
    let js = "a = 1 / 2; // don't\nb = /x[/]y/g.test(s) ? `t ${ {k: '}'}.k } u` : \"q\"; /* c */";
    let cancel = CancellationToken::default();
    let tokens = hints::lex(js, 0..js.len(), true, &mut Work::new(&cancel)).unwrap();
    let kinds: Vec<(hints::TokenKind, &str)> = tokens.iter().map(|t| (t.kind, &js[t.range.clone()])).collect();
    assert_eq!(kinds, [(LineComment, "// don't"), (Regex, "/x[/]y/g"), (Template, "`t ${ {k: '}'}.k } u`"), (Str, "\"q\""), (BlockComment, "/* c */")]);
    let css = "a { content: \"/*\" } /* x */ .b\\{ {}";
    let tokens = hints::lex(css, 0..css.len(), false, &mut Work::new(&cancel)).unwrap();
    assert_eq!(tokens.iter().map(|t| &css[t.range.clone()]).collect::<Vec<_>>(), ["\"/*\"", "/* x */"]);
    assert!(hints::token_around(&tokens, css.find("x */").unwrap()).is_some());
    assert!(hints::token_around(&tokens, css.find("/* x").unwrap()).is_none(), "a token's start is outside it");
    // Unmatched closers cost O(1) each: a million of them balance in linear time.
    let hostile = format!("<script>{}{}</script>", "(".repeat(1 << 20), "}".repeat(1 << 20));
    let d = doc(&hostile);
    let start = std::time::Instant::now();
    let at = 8 + (1 << 19);
    assert_eq!(balance(&d, at..at).map(|r| r.len()), Ok(hostile.len()), "only the script element encloses it");
    assert!(start.elapsed().as_secs() < 20, "linear: {:?}", start.elapsed());
}
