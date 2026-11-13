//! Lane SMB-WEB-CODE-1: [STU-WEB-020], [STU-WEB-028] (format by the formatting rules, idempotent, verbatim contents),
//! [STU-WEB-029] (comment apply/remove by language), [STU-WEB-059] (indent, outdent), [STU-WEB-015] (doctype edit).
use hsk_studio_accord::CancellationToken;
use hsk_studio_web_core::doc_types::doctype;
use hsk_studio_web_core::formatting::{self, AttributeWrap, FormatOptions, IndentChar};
use hsk_studio_web_core::source_dom_patch::TextEdit;
use hsk_studio_web_core::vocabulary::TagRegistry;
use hsk_studio_web_core::{Limits, SourcePatch, WebDoc, apply, parse};

fn doc(src: &str) -> WebDoc {
    parse(src, 1, &Limits::default(), &CancellationToken::default()).unwrap()
}

fn applied(d: &WebDoc, edits: Vec<TextEdit>) -> String {
    let out = apply(d, &SourcePatch::new(d, edits)).unwrap().source;
    parse(&out, 2, &Limits::default(), &CancellationToken::default()).expect("the result parses");
    out
}

fn format(src: &str, opts: FormatOptions) -> String {
    let d = doc(src);
    let edits = formatting::format_edits(&d, &TagRegistry::shipped(), "html", None, opts).unwrap();
    applied(&d, edits)
}

const MESSY: &str = "<!doctype html><html><head><title>T</title><style>p{color:red}\n  </style></head><body><div><p>Hello <b>you</b></p><ul><li>a</li><li>b</li></ul><pre>  keep\n   this</pre><textarea>  raw </textarea></div></body></html>";

#[test]
fn format_places_blocks_by_their_rules_and_keeps_verbatim_contents() {
    let out = format(MESSY, FormatOptions::default());
    let want = "<!doctype html>\n<html>\n<head>\n  <title>T</title>\n  <style>p{color:red}\n  </style>\n</head>\n<body>\n  <div>\n    <p>Hello <b>you</b></p>\n    <ul>\n      <li>a</li>\n      <li>b</li>\n    </ul>\n    <pre>  keep\n   this</pre>\n    <textarea>  raw </textarea>\n  </div>\n</body>\n</html>";
    assert_eq!(out, want);
    // Non-whitespace bytes never change.
    let strip = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    assert_eq!(strip(&out), strip(MESSY));
}

#[test]
fn format_is_idempotent() {
    let once = format(MESSY, FormatOptions::default());
    let d = doc(&once);
    assert!(formatting::format_edits(&d, &TagRegistry::shipped(), "html", None, FormatOptions::default()).unwrap().is_empty(), "already formatted: no edit");
    let tabs = FormatOptions { indent_char: IndentChar::Tab, ..FormatOptions::default() };
    let t = format(MESSY, tabs);
    assert!(t.contains("\n\t\t<ul>\n\t\t\t<li>a</li>"), "{t}");
    assert!(formatting::format_edits(&doc(&t), &TagRegistry::shipped(), "html", None, tabs).unwrap().is_empty());
}

#[test]
fn format_keeps_crlf_blank_lines_and_scopes() {
    let src = "<div>\r\n\r\n\r\n\r\n<p>a</p><p>b</p></div>";
    let out = format(src, FormatOptions::default());
    assert_eq!(out, "<div>\r\n\r\n  <p>a</p>\r\n  <p>b</p>\r\n</div>", "line endings follow the document; at most the rule's max newlines");
    let d = doc("<div><section><p>a</p></section><section><p>b</p></section></div>");
    let second = d.find_by_tag("section")[1];
    let edits = formatting::format_edits(&d, &TagRegistry::shipped(), "html", Some(second), FormatOptions::default()).unwrap();
    assert_eq!(applied(&d, edits), "<div><section><p>a</p></section><section>\n    <p>b</p>\n  </section></div>", "only the selection's contents");
    let pre = doc("<pre><span>x</span></pre>");
    let span = pre.find_by_tag("span")[0];
    assert_eq!(formatting::format_edits(&pre, &TagRegistry::shipped(), "html", Some(span), FormatOptions::default()).unwrap_err().code, "format_inside_verbatim");
}

#[test]
fn long_start_tags_wrap_their_attributes() {
    let opts = FormatOptions { line_length: 40, attribute_wrap: AttributeWrap::WrapOverLineLength, ..FormatOptions::default() };
    let out = format("<div><p id=\"first-paragraph\" class=\"lead wide\" title=\"x\">t</p><p id=a>u</p></div>", opts);
    assert_eq!(out, "<div>\n  <p id=\"first-paragraph\"\n    class=\"lead wide\"\n    title=\"x\">t</p>\n  <p id=a>u</p>\n</div>");
    let d = doc(&out);
    assert!(formatting::format_edits(&d, &TagRegistry::shipped(), "html", None, opts).unwrap().is_empty(), "idempotent with wrapping");
    assert_eq!(FormatOptions { indent_width: 0, ..FormatOptions::default() }.checked().unwrap_err().code, "format_option_invalid");
    assert_eq!(FormatOptions { line_length: 5, ..FormatOptions::default() }.checked().unwrap_err().code, "format_option_invalid");
}

#[test]
fn large_documents_stay_inside_the_edit_limit() {
    let src = format!("<ul>{}</ul>", "<li>x</li>".repeat(3000));
    let d = doc(&src);
    let edits = formatting::format_edits(&d, &TagRegistry::shipped(), "html", None, FormatOptions::default()).unwrap();
    assert!(edits.len() <= d.limits().max_edits, "{}", edits.len());
    let out = applied(&d, edits);
    assert_eq!(out.matches("\n  <li>x</li>").count(), 3000);
}

#[test]
fn comments_follow_the_language_at_the_position() {
    let src = "<style>p { color: red; }</style><script>let a = 1;</script><p>keep <b>me</b></p>";
    let d = doc(src);
    let at = |n: &str| src.find(n).unwrap();
    let range = |n: &str| at(n)..at(n).saturating_add(n.len());
    let (e, scheme) = formatting::comment_edit(&d, range("color: red;")).unwrap();
    assert_eq!((scheme, applied(&d, vec![e]).contains("p { /*color: red;*/ }")), ("css", true));
    let (e, scheme) = formatting::comment_edit(&d, range("let a = 1;")).unwrap();
    assert_eq!((scheme, applied(&d, vec![e]).contains("<script>/*let a = 1;*/</script>")), ("javascript", true));
    let (e, scheme) = formatting::comment_edit(&d, range("<b>me</b>")).unwrap();
    let html = applied(&d, vec![e]);
    assert_eq!((scheme, html.contains("keep <!--<b>me</b>-->")), ("html", true));
    // Remove gives the text back, in every language.
    let d2 = doc(&html);
    let (e, _) = formatting::uncomment_edit(&d2, html.find("<b>").unwrap()).unwrap();
    assert_eq!(applied(&d2, vec![e]), src);
    let css_commented = "<style>p { /* color: red; */ top: 0 }</style>";
    let d3 = doc(css_commented);
    let (e, scheme) = formatting::uncomment_edit(&d3, css_commented.find("color").unwrap()).unwrap();
    assert_eq!((scheme, applied(&d3, vec![e]).as_str()), ("css", "<style>p {  color: red;  top: 0 }</style>"));
    // Refusals: across languages, a nested close, part of a tag, nothing there.
    let code = |r: std::ops::Range<usize>| formatting::comment_edit(&d, r).unwrap_err().code;
    assert_eq!(code(at("color")..at("let")), "range_crosses_languages");
    assert_eq!(code(at("<p>").saturating_add(1)..at("keep")), "range_splits_a_tag");
    let d4 = doc("<p>a -- b</p>");
    assert_eq!(formatting::comment_edit(&d4, 3..9).unwrap_err().code, "comment_would_nest");
    let d5 = doc("<style>p { content: \"*/\" }</style>");
    let s5 = d5.source().to_string();
    assert_eq!(formatting::comment_edit(&d5, s5.find("content").unwrap()..s5.find(" }").unwrap()).unwrap_err().code, "comment_would_nest");
    assert_eq!(formatting::uncomment_edit(&d, at("keep")).unwrap_err().code, "no_comment_here");
}

#[test]
fn indent_and_outdent_shift_lines_and_leave_verbatim_alone() {
    let src = "<div>\n<p>a</p>\n\n  <p>b</p>\n</div>\n<pre>\nx\n</pre>";
    let d = doc(src);
    let p_lines = src.find("<p>a").unwrap()..src.find("</div>").unwrap();
    let indented = applied(&d, formatting::indent_edits(&d, p_lines.clone(), false, "  ").unwrap());
    assert_eq!(indented, "<div>\n  <p>a</p>\n\n    <p>b</p>\n</div>\n<pre>\nx\n</pre>", "blank lines stay blank");
    let d2 = doc(&indented);
    let back = indented.find("<p>a").unwrap()..indented.find("</div>").unwrap();
    let outdented = applied(&d2, formatting::indent_edits(&d2, back, true, "  ").unwrap());
    assert_eq!(outdented, src);
    let x = src.find("x\n").unwrap();
    assert_eq!(formatting::indent_edits(&d, x..x.saturating_add(1), false, "  ").unwrap_err().code, "range_in_verbatim");
    assert_eq!(formatting::indent_edits(&d, 0..3, false, "x").unwrap_err().code, "format_option_invalid");
    let flat = doc("<p>a</p>");
    assert_eq!(formatting::indent_edits(&flat, 0..3, true, "  ").unwrap_err().code, "nothing_to_change");
}

#[test]
fn the_doctype_is_replaced_or_inserted_verbatim() {
    let d = doc("<!DOCTYPE html>\n<p>x</p>");
    let e = formatting::doctype_edit(&d, doctype("html_401_strict").unwrap()).unwrap();
    assert_eq!(applied(&d, vec![e]), "<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.01//EN\" \"http://www.w3.org/TR/html4/strict.dtd\">\n<p>x</p>");
    let bare = doc("<p>x</p>");
    let e = formatting::doctype_edit(&bare, doctype("html_5").unwrap()).unwrap();
    assert_eq!(applied(&bare, vec![e]), "<!doctype html>\n<p>x</p>");
    assert_eq!(formatting::doctype_edit(&bare, doctype("xslt_1").unwrap()).unwrap_err().code, "doctype_needs_xml_parser");
    let five = doc("<!doctype html><p>x</p>");
    assert_eq!(formatting::doctype_edit(&five, doctype("html_5").unwrap()).unwrap_err().code, "nothing_to_change");
}
