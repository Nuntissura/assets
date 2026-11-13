//! Lane SMB-WEB-CODE-1: [STU-WEB-020], [STU-WEB-028] (format by the formatting rules, idempotent, verbatim contents),
//! [STU-WEB-029] (comment apply/remove by language), [STU-WEB-059] (indent, outdent), [STU-WEB-015] (doctype edit).
use hsk_studio_accord::CancellationToken;
use hsk_studio_web_core::doc_types::doctype;
use hsk_studio_web_core::formatting::{self, AttributeWrap, FormatOptions, IndentChar};
use hsk_studio_web_core::source_dom_patch::TextEdit;
use hsk_studio_web_core::vocabulary::TagRegistry;
use hsk_studio_web_core::doc_types::Work;
use hsk_studio_web_core::hints::SchemeRegistry;
use hsk_studio_web_core::{Limits, SourcePatch, WebDoc, apply, parse};

fn fmt(d: &WebDoc, scope: Option<usize>, opts: FormatOptions) -> Result<Vec<TextEdit>, &'static str> {
    let cancel = CancellationToken::default();
    formatting::format_edits(d, &TagRegistry::shipped(), "html", scope, opts, &mut Work::new(&cancel)).map_err(|e| e.code)
}

fn comment(d: &WebDoc, r: std::ops::Range<usize>) -> Result<(TextEdit, &'static str), &'static str> {
    let cancel = CancellationToken::default();
    formatting::comment_edit(d, &SchemeRegistry::shipped(), "html", r, &mut Work::new(&cancel)).map_err(|e| e.code)
}

fn uncomment(d: &WebDoc, at: usize) -> Result<(TextEdit, &'static str), &'static str> {
    let cancel = CancellationToken::default();
    formatting::uncomment_edit(d, &SchemeRegistry::shipped(), "html", at, &mut Work::new(&cancel)).map_err(|e| e.code)
}

fn indent(d: &WebDoc, r: Option<std::ops::Range<usize>>, out: bool, unit: &str) -> Result<Vec<TextEdit>, &'static str> {
    let cancel = CancellationToken::default();
    formatting::indent_edits(d, r, out, unit, &mut Work::new(&cancel)).map_err(|e| e.code)
}

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
    let edits = fmt(&d, None, opts).unwrap();
    applied(&d, edits)
}

const MESSY: &str = "<!doctype html><html><head><title>T</title><style>p{color:red}\n  </style></head><body><div><p>Hello <b>you</b></p><ul><li>a</li><li>b</li></ul><pre>  keep\n   this</pre><textarea>  raw </textarea></div></body></html>";

#[test]
fn format_places_blocks_by_their_rules_and_keeps_verbatim_contents() {
    let out = format(MESSY, FormatOptions::default());
    let want = "<!doctype html>\n<html>\n<head>\n  <title>T</title>\n  <style>p{color:red}\n  </style>\n</head>\n<body>\n  <div>\n    <p>Hello <b>you</b></p>\n    <ul>\n      <li>a</li>\n      <li>b</li>\n    </ul>\n    <pre>  keep\n   this</pre><textarea>  raw </textarea>\n  </div>\n</body>\n</html>";
    assert_eq!(out, want);
    // Non-whitespace bytes never change.
    let strip = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    assert_eq!(strip(&out), strip(MESSY));
}

#[test]
fn format_is_idempotent() {
    let once = format(MESSY, FormatOptions::default());
    let d = doc(&once);
    assert!(fmt(&d, None, FormatOptions::default()).unwrap().is_empty(), "already formatted: no edit");
    let tabs = FormatOptions { indent_char: IndentChar::Tab, ..FormatOptions::default() };
    let t = format(MESSY, tabs);
    assert!(t.contains("\n\t\t<ul>\n\t\t\t<li>a</li>"), "{t}");
    assert!(fmt(&doc(&t), None, tabs).unwrap().is_empty());
}

#[test]
fn format_keeps_crlf_blank_lines_and_scopes() {
    let src = "<div>\r\n\r\n\r\n\r\n<p>a</p><p>b</p></div>";
    let out = format(src, FormatOptions::default());
    assert_eq!(out, "<div>\r\n\r\n  <p>a</p>\r\n  <p>b</p>\r\n</div>", "line endings follow the document; at most the rule's max newlines");
    let d = doc("<div><section><p>a</p></section><section><p>b</p></section></div>");
    let second = d.find_by_tag("section")[1];
    let edits = fmt(&d, Some(second), FormatOptions::default()).unwrap();
    assert_eq!(applied(&d, edits), "<div><section><p>a</p></section><section>\n    <p>b</p>\n  </section></div>", "only the selection's contents");
    let pre = doc("<pre><span>x</span></pre>");
    let span = pre.find_by_tag("span")[0];
    assert_eq!(fmt(&pre, Some(span), FormatOptions::default()).unwrap_err(), "format_inside_verbatim");
}

#[test]
fn long_start_tags_wrap_their_attributes() {
    let opts = FormatOptions { line_length: 40, attribute_wrap: AttributeWrap::WrapOverLineLength, ..FormatOptions::default() };
    let out = format("<div><p id=\"first-paragraph\" class=\"lead wide\" title=\"x\">t</p><p id=a>u</p></div>", opts);
    assert_eq!(out, "<div>\n  <p id=\"first-paragraph\"\n    class=\"lead wide\"\n    title=\"x\">t</p>\n  <p id=a>u</p>\n</div>");
    let d = doc(&out);
    assert!(fmt(&d, None, opts).unwrap().is_empty(), "idempotent with wrapping");
    assert_eq!(FormatOptions { indent_width: 0, ..FormatOptions::default() }.checked().unwrap_err().code, "format_option_invalid");
    assert_eq!(FormatOptions { line_length: 5, ..FormatOptions::default() }.checked().unwrap_err().code, "format_option_invalid");
}

#[test]
fn large_documents_stay_inside_the_edit_limit() {
    let src = format!("<ul>{}</ul>", "<li>x</li>".repeat(3000));
    let d = doc(&src);
    let edits = fmt(&d, None, FormatOptions::default()).unwrap();
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
    let (e, scheme) = comment(&d, range("color: red;")).unwrap();
    assert_eq!((scheme, applied(&d, vec![e]).contains("p { /*color: red;*/ }")), ("css", true));
    let (e, scheme) = comment(&d, range("let a = 1;")).unwrap();
    assert_eq!((scheme, applied(&d, vec![e]).contains("<script>/*let a = 1;*/</script>")), ("javascript", true));
    let (e, scheme) = comment(&d, range("<b>me</b>")).unwrap();
    let html = applied(&d, vec![e]);
    assert_eq!((scheme, html.contains("keep <!--<b>me</b>-->")), ("html", true));
    // Remove gives the text back, in every language.
    let d2 = doc(&html);
    let (e, _) = uncomment(&d2, html.find("<b>").unwrap()).unwrap();
    assert_eq!(applied(&d2, vec![e]), src);
    let css_commented = "<style>p { /* color: red; */ top: 0 }</style>";
    let d3 = doc(css_commented);
    let (e, scheme) = uncomment(&d3, css_commented.find("color").unwrap()).unwrap();
    assert_eq!((scheme, applied(&d3, vec![e]).as_str()), ("css", "<style>p {  color: red;  top: 0 }</style>"));
    // Refusals: across languages, a nested close, part of a tag, nothing there.
    let code = |r: std::ops::Range<usize>| comment(&d, r).unwrap_err();
    assert_eq!(code(at("color")..at("let")), "range_crosses_languages");
    assert_eq!(code(at("<p>").saturating_add(1)..at("keep")), "range_splits_a_tag");
    let d4 = doc("<p>a -- b</p>");
    assert_eq!(comment(&d4, 3..9).unwrap_err(), "comment_would_nest");
    let d5 = doc("<style>p { content: \"*/\" }</style>");
    let s5 = d5.source().to_string();
    assert_eq!(comment(&d5, s5.find("content").unwrap()..s5.find(" }").unwrap()).unwrap_err(), "comment_would_nest");
    assert_eq!(uncomment(&d, at("keep")).unwrap_err(), "no_comment_here");
}

#[test]
fn indent_and_outdent_shift_lines_and_leave_verbatim_alone() {
    let src = "<div>\n<p>a</p>\n\n  <p>b</p>\n</div>\n<pre>\nx\n</pre>";
    let d = doc(src);
    let p_lines = src.find("<p>a").unwrap()..src.find("</div>").unwrap();
    let indented = applied(&d, indent(&d, Some(p_lines.clone()), false, "  ").unwrap());
    assert_eq!(indented, "<div>\n  <p>a</p>\n\n    <p>b</p>\n</div>\n<pre>\nx\n</pre>", "blank lines stay blank");
    let d2 = doc(&indented);
    let back = indented.find("<p>a").unwrap()..indented.find("</div>").unwrap();
    let outdented = applied(&d2, indent(&d2, Some(back), true, "  ").unwrap());
    assert_eq!(outdented, src);
    let x = src.find("x\n").unwrap();
    assert_eq!(indent(&d, Some(x..x.saturating_add(1)), false, "  ").unwrap_err(), "range_in_verbatim");
    assert_eq!(indent(&d, Some(0..3), false, "x").unwrap_err(), "format_option_invalid");
    let flat = doc("<p>a</p>");
    assert_eq!(indent(&flat, Some(0..3), true, "  ").unwrap_err(), "nothing_to_change");
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

/// Review F1: comment edits change only their selection (the review's examples, at the engine).
#[test]
fn comment_edits_change_only_the_selection() {
    let nested = doc("<!-- a <!-- b --><p>keep</p><!-- c -->");
    assert_eq!(uncomment(&nested, 5).unwrap_err(), "comment_inner_has_markers");
    let css = "<style>/* a /* b */ p{top:0}</style>";
    assert_eq!(uncomment(&doc(css), css.find(" a").unwrap()).unwrap_err(), "comment_inner_has_markers");
    let js = "<script>// don't\nvar t = '/* x */';</script>";
    assert_eq!(uncomment(&doc(js), js.find('x').unwrap()).unwrap_err(), "no_comment_here");
    let line = "<script>// don't\nf();</script>";
    assert_eq!(uncomment(&doc(line), 10).unwrap_err(), "comment_inner_has_markers", "unwrapping would open a string");
    let swallow = "<script>/* a // b */ c();</script>";
    assert_eq!(uncomment(&doc(swallow), 10).unwrap_err(), "comment_inner_has_markers", "a line comment would swallow c()");
    let ok = "<script>// note\nf();</script>";
    let d = doc(ok);
    let (e, _) = uncomment(&d, 10).unwrap();
    assert_eq!(applied(&d, vec![e]), "<script> note\nf();</script>");
    let inside = "<style>/* a b c */ p{top:0}</style>";
    let b = inside.find(" b ").unwrap() + 1;
    assert_eq!(comment(&doc(inside), b..b + 1).unwrap_err(), "range_splits_a_token");
    let string = "<script>var s = 'abc'; t = `x ${y} z`;</script>";
    let at = string.find("bc").unwrap();
    assert_eq!(comment(&doc(string), at..at + 1).unwrap_err(), "range_splits_a_token");
    let y = string.find('y').unwrap();
    assert_eq!(comment(&doc(string), y..y + 1).unwrap_err(), "range_splits_a_token", "inside a template literal");
    let title = "<title>Hello</title><textarea>t</textarea><xmp>x</xmp>";
    let d = doc(title);
    assert_eq!(comment(&d, 7..12).unwrap_err(), "range_in_rcdata");
    let t = title.find(">t<").unwrap() + 1;
    assert_eq!(comment(&d, t..t + 1).unwrap_err(), "range_in_rcdata");
    let x = title.find(">x<").unwrap() + 1;
    assert_eq!(comment(&d, x..x + 1).unwrap_err(), "range_in_raw_text");
}

/// Review F3: the review's examples format with no new whitespace next to inline-level or replaced content.
#[test]
fn format_is_rendering_neutral_on_the_review_examples() {
    for page in ["<p>a<canvas id=c></canvas>b</p>", "<p>Price:<select><option>1</option></select></p>", "<span><video src=v></video></span>"] {
        assert!(fmt(&doc(page), None, FormatOptions::default()).unwrap().is_empty(), "{page}");
    }
    let pic = "<div><img src=a><picture><source srcset=x><img src=b></picture></div>";
    assert_eq!(format(pic, FormatOptions::default()), "<div>\n  <img src=a><picture><source srcset=x><img src=b></picture>\n</div>");
    let listing = "<div><listing>a\n  b</listing></div>";
    assert_eq!(format(listing, FormatOptions::default()), "<div>\n  <listing>a\n  b</listing>\n</div>");
}

/// Review F3: a line start inside an attribute value is protected; the whole document skips protected lines.
#[test]
fn indent_protects_attribute_values_and_verbatim_lines() {
    let src = "<div title=\"a\nb\">\n<p>x</p>\n</div>\n<pre>\nq\n</pre>";
    let d = doc(src);
    let b = src.find("b\"").unwrap();
    assert_eq!(indent(&d, Some(b..b + 1), false, "  ").unwrap_err(), "range_in_attribute_value");
    let out = applied(&d, indent(&d, None, false, "  ").unwrap());
    assert_eq!(out, "  <div title=\"a\nb\">\n  <p>x</p>\n  </div>\n  <pre>\nq\n</pre>", "value and pre lines untouched");
    let d2 = doc(&out);
    assert_eq!(applied(&d2, indent(&d2, None, true, "  ").unwrap()), src);
}

/// Review F10: the doctype goes after an XML declaration (a leading BOM: the host's open keeps it out of the source,
/// tests/web_code.rs in the harness).
#[test]
fn the_doctype_follows_a_bom_and_an_xml_declaration() {
    let xml = doc("<?xml version=\"1.0\"?>\n<p>x</p>");
    let e = formatting::doctype_edit(&xml, doctype("xhtml_1_strict").unwrap()).unwrap();
    assert!(applied(&xml, vec![e]).starts_with("<?xml version=\"1.0\"?>\n<!DOCTYPE html PUBLIC"));
}

/// A small deterministic generator of well-formed pages (review F14: properties over a corpus, not one fixture).
struct Gen(u64);

impl Gen {
    fn next(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }

    fn inline(&mut self, depth: u32) -> String {
        match self.next(7) {
            0 => "x".into(),
            1 => "y z".into(),
            2 => "<img src=a>".into(),
            3 => "<canvas></canvas>".into(),
            4 => "<select><option>1</option><option>2</option></select>".into(),
            5 if depth > 0 => format!("<b>{}</b>", self.inline(depth - 1)),
            _ => "<!-- c -->".into(),
        }
    }

    fn block(&mut self, depth: u32) -> String {
        let kids = self.next(4);
        let mut out = String::new();
        for _ in 0..kids {
            out.push_str(&match self.next(6) {
                0 if depth > 0 => format!("<div>{}</div>", self.block(depth - 1)),
                1 if depth > 0 => format!("<section>{}</section>", self.block(depth - 1)),
                2 => format!("<p>{}{}</p>", self.inline(depth), self.inline(depth)),
                3 => format!("<ul><li>{}</li><li>{}</li></ul>", self.inline(depth), self.inline(depth)),
                4 => "<pre> a\n  b</pre>".into(),
                _ => self.inline(depth),
            });
        }
        out
    }
}

/// The name of the tag that ends at `p` (`</x>` or `<x ...>`) and of the tag that starts at `p`.
fn tags_around(src: &str, p: usize) -> (Option<String>, Option<String>) {
    let before = src[..p].ends_with('>').then(|| src[..p].rfind('<')).flatten().map(|i| src[i + 1..p - 1].split([' ', '>']).next().unwrap_or("").to_string());
    let after = src[p..].starts_with('<').then(|| src[p + 1..].split([' ', '>']).next().unwrap_or("").to_string());
    (before, after)
}

/// Review F14: format, comment and indent over a generated corpus: formatting keeps every non-whitespace byte, writes
/// whitespace into an empty gap only between block-level tags or at a block container's edge, and is idempotent; a
/// comment applied and removed, and an indent then outdent, give back the bytes.
#[test]
fn format_comment_and_indent_round_trip_over_a_generated_corpus() {
    const CONTAINERS: [&str; 6] = ["div", "section", "ul", "html", "head", "body"];
    const BLOCKS: [&str; 12] = ["div", "section", "ul", "li", "p", "pre", "option", "html", "head", "body", "title", "!doctype"];
    let mut g = Gen(0x9e37_79b9_7f4a_7c15);
    let mut checked = 0;
    for _ in 0..200 {
        let body = g.block(3);
        let src = format!("<!doctype html><html><head><title>t</title></head><body>{body}</body></html>");
        let d = doc(&src);
        let edits = fmt(&d, None, FormatOptions::default()).unwrap();
        for e in edits.iter().filter(|e| e.span.is_empty()) {
            let (before, after) = tags_around(&src, e.span.start);
            let (before, after) = (before.unwrap_or_default(), after.unwrap_or_default());
            let edge = CONTAINERS.contains(&before.as_str()) || after.strip_prefix('/').is_some_and(|t| CONTAINERS.contains(&t));
            let both = BLOCKS.contains(&before.trim_start_matches('/')) && BLOCKS.contains(&after.trim_start_matches('/'));
            assert!(edge || both, "whitespace inserted between <{before}> and <{after}> in {src}");
        }
        let out = applied(&d, edits);
        let strip = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        assert_eq!(strip(&out), strip(&src));
        let again = doc(&out);
        assert!(fmt(&again, None, FormatOptions::default()).unwrap().is_empty(), "idempotent: {out}");
        // Every element of the body commented and uncommented gives the page back.
        for id in 0..again.len() {
            let Some(n) = again.node(id) else { continue };
            if n.kind != hsk_studio_web_core::NodeKind::Element || n.parent.and_then(|p| again.local_name(p)).as_deref() != Some("body") {
                continue;
            }
            let Ok((e, _)) = comment(&again, n.span.clone()) else { continue };
            let commented = applied(&again, vec![e]);
            let cd = doc(&commented);
            let (u, _) = uncomment(&cd, n.span.start + 4).unwrap();
            assert_eq!(applied(&cd, vec![u]), out);
            checked += 1;
        }
        let ind = applied(&again, indent(&again, None, false, "  ").unwrap());
        let id = doc(&ind);
        assert_eq!(applied(&id, indent(&id, None, true, "  ").unwrap()), out);
    }
    assert!(checked > 100, "the corpus exercised comments: {checked}");
}
