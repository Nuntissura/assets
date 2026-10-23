//! Focused parity tests (lane SMB-WEB-PARITY): the crate reads names, values and text the way the HTML Standard's
//! tokenizer and tree builder build them (https://html.spec.whatwg.org/multipage/parsing.html), and refuses typed every
//! input whose tree a browser builds differently from the authored tree, except the documented normalisations N1/N2.
//! Each test pins one disagreement class recorded by the differential lane SMB-WEB-DIFFTEST (D1-D9); the assertions use
//! only API that existed before the fix (refusal codes as strings), so the same file is the red proof on the old crate.

use hsk_studio_accord::CancellationToken;
use hsk_studio_web::responsive::document_media_blocks;
use hsk_studio_web::{
    AssetDecision, AssetPort, ContentModel, Limits, Loads, NodeKind, WebDoc, apply, collect_assets,
    decode_entities, escape_attr, escape_text, insert_element, parse, preview_tree, set_attribute,
    set_text,
};
use std::sync::Mutex;

// Each canonical test case owns its deadline; panic payloads reach the Cargo harness.
fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let outcome = catch_unwind(AssertUnwindSafe(body));
        let _ = sender.send(outcome);
    });
    match receiver.recv_timeout(Duration::from_secs(45)) {
        Ok(Ok(())) => {}
        Ok(Err(payload)) => resume_unwind(payload),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("case {name} exceeded declared 45-second deadline")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("case {name} lost its deadline worker result")
        }
    }
}

fn load(src: &str) -> WebDoc {
    parse(src, 0, &Limits::default(), &CancellationToken::default()).unwrap()
}

/// `ok`, or the stable code of the typed refusal.
fn outcome(src: &str) -> String {
    match parse(src, 0, &Limits::default(), &CancellationToken::default()) {
        Ok(_) => "ok".to_owned(),
        Err(e) => e.code().to_owned(),
    }
}

/// Pre-order tree shape from the view: elements with their attributes, decoded text, other nodes with their source.
fn shape(doc: &WebDoc) -> Vec<String> {
    doc.view()
        .into_iter()
        .skip(1)
        .map(|r| {
            let pad = "  ".repeat(r.depth.saturating_sub(1));
            match r.kind {
                NodeKind::Element => {
                    let attrs: String = r
                        .attrs
                        .iter()
                        .map(|(n, v)| format!(" {n}={:?}", v.clone().unwrap_or_default()))
                        .collect();
                    format!("{pad}<{}{attrs}>", r.tag.unwrap_or_default())
                }
                NodeKind::Text => format!("{pad}{:?}", r.text.unwrap_or_default()),
                other => format!("{pad}{other:?} {:?}", doc.slice(&r.span)),
            }
        })
        .collect()
}

fn count_loss(doc: &WebDoc, code: &str) -> usize {
    doc.loss()
        .entries()
        .iter()
        .filter(|e| e.kind.code() == code)
        .count()
}

fn first(doc: &WebDoc, tag: &str) -> usize {
    *doc.find_by_tag(tag)
        .first()
        .unwrap_or_else(|| panic!("no {tag} element"))
}

/// D1, 13.2.6.4.7 "A start tag whose tag name is image": change the token's tag name to `img` and reprocess (void); the
/// later `</image>` matches no open element and is ignored ("any other end tag"). Foreign `image` stays `image`.
#[test]
fn image_start_tag_is_a_void_img_and_its_end_tag_is_ignored() {
    bounded_case(
        "image_start_tag_is_a_void_img_and_its_end_tag_is_ignored",
        || {
            let src = "<div><IMAGE src=a.png alt=Logo>x</image ></div><svg><image href=b.png></image><g></image></g></svg>";
            let doc = load(src);
            assert_eq!(
                shape(&doc),
                [
                    "<div>",
                    "  <img src=\"a.png\" alt=\"Logo\">",
                    "  \"x\"",
                    "  Unsupported \"</image >\"",
                    "<svg>",
                    "  <image href=\"b.png\">",
                    "  <g>",
                    "    Unsupported \"</image>\"",
                ]
            );
            let img = first(&doc, "img");
            assert_eq!(
                doc.tag_name(img),
                Some("IMAGE"),
                "the span keeps the source spelling"
            );
            assert_eq!(doc.node(img).unwrap().content, ContentModel::Empty);
            assert_eq!(count_loss(&doc, "ignored_end_tag"), 2);
            let preview = preview_tree(&doc, 800.0, &CancellationToken::default()).unwrap();
            let node = preview.nodes.iter().find(|n| n.id == img).unwrap();
            assert_eq!(
                (node.role.as_deref(), node.name.as_deref()),
                (Some("img"), Some("Logo"))
            );
            // Unclosed, as browsers read it; an svg image still needs its end tag; nothing follows `</html>`.
            assert_eq!(outcome("<p><image src=x></p>"), "ok");
            assert_eq!(
                outcome("<svg><image><g></image></g></image></svg>"),
                "misnested_end_tag"
            );
            assert_eq!(
                outcome("<html><head></head><body></body></html></image>"),
                "document_structure"
            );
            // An ignored end tag first ends the "initial" mode, so a DOCTYPE after it is ignored too.
            assert_eq!(outcome("</image><!DOCTYPE html>"), "ignored_token");
            assert_eq!(outcome("<!DOCTYPE html></image><p>x</p>"), "ok");
        },
    );
}

/// D2-D4, 13.2.5.8 tag name, 13.2.5.33 attribute name, 13.2.5.36-.38 attribute value states: U+0000 becomes U+FFFD (names
/// are also ASCII-lowercased); duplicates are found on those names.
#[test]
fn nul_in_names_and_attribute_values_becomes_replacement_character() {
    bounded_case(
        "nul_in_names_and_attribute_values_becomes_replacement_character",
        || {
            let doc = load("<d\0iv A\0B=\"x\0y\" c='\0' d=\0e></D\0IV>");
            let row = &doc.view()[1];
            assert_eq!(row.tag.as_deref(), Some("d\u{fffd}iv"));
            assert_eq!(
                row.attrs,
                [
                    ("a\u{fffd}b".to_owned(), Some("x\u{fffd}y".to_owned())),
                    ("c".to_owned(), Some("\u{fffd}".to_owned())),
                    ("d".to_owned(), Some("\u{fffd}e".to_owned())),
                ]
            );
            // `\0` and U+FFFD are the same attribute name to a browser (the first wins); so are `X` and `x`.
            let doc = load("<b \0=1 \u{fffd}=2 X=3 x=4>t</b>");
            assert_eq!(count_loss(&doc, "duplicate_attribute"), 2);
            let b = first(&doc, "b");
            let found = doc.attribute(b, "\u{fffd}").and_then(|a| doc.attr_value(a));
            assert_eq!(found.as_deref(), Some("1"));
            // Decoy: a reference to U+0000 was already U+FFFD; comments and raw text keep their source bytes (opaque).
            let doc = load("<a title=\"&#0;\">t</a><!--\0--><script>\0</script>");
            assert_eq!(doc.view()[1].attrs[0].1.as_deref(), Some("\u{fffd}"));
            assert!(shape(&doc).iter().any(|r| r == "Comment \"<!--\\0-->\""));
        },
    );
}

/// D5, 13.2.3.5 preprocessing: CR LF and a lone CR in attribute values are LF; a `&#13;` reference is decoded after
/// preprocessing and stays CR.
#[test]
fn newlines_in_attribute_values_are_normalised_before_references() {
    bounded_case(
        "newlines_in_attribute_values_are_normalised_before_references",
        || {
            let doc = load("<a title=\"a\r\nb\rc\nd\r\r\ne\" alt='&#13;&#10;\r' x=&#13;>t</a>");
            let values: Vec<Option<String>> =
                doc.view()[1].attrs.iter().map(|a| a.1.clone()).collect();
            assert_eq!(
                values,
                [
                    Some("a\nb\nc\nd\n\ne".to_owned()),
                    Some("\r\n\n".to_owned()),
                    Some("\r".to_owned())
                ]
            );
            // Escaping writes CR as a reference, so an edited value reads back with its CR.
            assert_eq!(escape_attr("a\rb", '"'), "a&#13;b");
            let a = doc.key_of(first(&doc, "a")).unwrap();
            let patch = set_attribute(&doc, &a, "title", "x\r\ny").unwrap();
            let next = doc
                .apply_patch(&patch, &CancellationToken::default())
                .unwrap();
            let title = next.attribute(first(&next, "a"), "title").unwrap();
            assert_eq!(next.attr_value(title).as_deref(), Some("x\r\ny"));
        },
    );
}

/// D6, 13.2.6.4.7 (a U+0000 character token in HTML content is ignored) and 13.2.6.5 (in foreign content it becomes
/// U+FFFD); integration points follow the HTML rules; RCDATA replaces it (13.2.5.2).
#[test]
fn nul_in_text_is_dropped_in_html_and_replaced_in_foreign_content() {
    bounded_case(
        "nul_in_text_is_dropped_in_html_and_replaced_in_foreign_content",
        || {
            let doc = load(
                "<div>a\0b</div><svg><text>c\0d</text><desc>e\0f</desc></svg>\
                 <math><mi>g\0h</mi><mo><mglyph>i\0j</mglyph></mo></math>\
                 <textarea>k\0l</textarea><p>\0</p><b>&#0;</b>",
            );
            let text = |tag: &str| doc.text_content(first(&doc, tag));
            assert_eq!(text("div"), "ab");
            assert_eq!(text("text"), "c\u{fffd}d");
            assert_eq!(text("desc"), "ef");
            assert_eq!(text("mi"), "gh");
            assert_eq!(text("mglyph"), "i\u{fffd}j");
            assert_eq!(text("textarea"), "k\u{fffd}l");
            assert_eq!(text("p"), "", "a NUL-only text node has no characters");
            assert_eq!(
                text("b"),
                "\u{fffd}",
                "a reference to U+0000 is U+FFFD everywhere"
            );
            assert_eq!(decode_entities("x\0y").text, "xy");
        },
    );
}

/// D7, 13.2.3.5 preprocessing in text, RCDATA and foreign text: CR LF and CR are LF; `&#13;` stays CR.
#[test]
fn newlines_in_text_are_normalised_before_references() {
    bounded_case("newlines_in_text_are_normalised_before_references", || {
        let doc = load(
            "<p>a\r\nb\rc&#13;d\r</p><textarea>x\r\ny\r\r\n</textarea><svg><text>\r\n</text></svg><title>t\r</title>",
        );
        let text = |tag: &str| doc.text_content(first(&doc, tag));
        assert_eq!(text("p"), "a\nb\nc\rd\n");
        assert_eq!(text("textarea"), "x\ny\n\n");
        assert_eq!(text("text"), "\n");
        assert_eq!(text("title"), "t\n");
        assert_eq!(decode_entities("1\r\n2\r3&#13;").text, "1\n2\n3\r");
        assert_eq!(escape_text("a\r\nb"), "a&#13;\nb");
    });
}

/// D8, 13.2.6.4.7 "pre, listing" and "textarea": if the next token is an LF character token it is ignored. That token is
/// the first character after preprocessing (LF, CR, CR LF) or the first code point of a character reference; a U+0000 or
/// a comment first is a different token. Foreign `textarea` has no such rule.
#[test]
fn first_newline_after_pre_listing_textarea_is_dropped() {
    bounded_case(
        "first_newline_after_pre_listing_textarea_is_dropped",
        || {
            let cases: &[(&str, &str, &str)] = &[
                ("<pre>\nA</pre>", "pre", "A"),
                ("<pre>\r\nB</pre>", "pre", "B"),
                ("<pre>\rC</pre>", "pre", "C"),
                ("<pre>\n\nD</pre>", "pre", "\nD"),
                ("<pre>&#10;E</pre>", "pre", "E"),
                ("<pre>&NewLine;F</pre>", "pre", "F"),
                // Spec: a numeric reference without `;` is still a reference (its parse error is not a token).
                ("<pre>&#10G</pre>", "pre", "G"),
                ("<pre>&#13;H</pre>", "pre", "\rH"),
                ("<pre>\0\nI</pre>", "pre", "\nI"),
                ("<pre><!---->\nJ</pre>", "pre", "\nJ"),
                ("<PRE>\nK</PRE>", "pre", "K"),
                ("<listing>\nL</listing>", "listing", "L"),
                ("<textarea>\nM</textarea>", "textarea", "M"),
                ("<textarea>\r\n\r\nN</textarea>", "textarea", "\nN"),
                ("<textarea>&#x0A;O</textarea>", "textarea", "O"),
                (
                    "<svg><foreignObject><pre>\nP</pre></foreignObject></svg>",
                    "pre",
                    "P",
                ),
                ("<svg><textarea>\nQ</textarea></svg>", "textarea", "\nQ"),
                ("<pre>\n</pre>", "pre", ""),
            ];
            for (src, tag, value) in cases {
                let doc = load(src);
                assert_eq!(doc.text_content(first(&doc, tag)), *value, "{src:?}");
            }
            // The view's text row agrees with text_content.
            let doc = load("<pre>\r\nx</pre>");
            assert_eq!(shape(&doc), ["<pre>", "  \"x\""]);
        },
    );
}

/// The relation between spans and decoded values (crate docs, `dom` module): spans are the source bytes; values are what
/// the tokenizer and tree builder build from them; there is no position map from a value back into its span.
#[test]
fn dom_values_are_decoded_and_spans_keep_the_source_bytes() {
    bounded_case(
        "dom_values_are_decoded_and_spans_keep_the_source_bytes",
        || {
            let src = "<pre>\r\nx&amp;\0y\r</pre><p title=\"a\r\n&#13;\">t</p><image src=i>";
            let doc = load(src);
            let pre = first(&doc, "pre");
            let text = doc.node(pre).unwrap().children[0];
            assert_eq!(doc.slice(&doc.node(text).unwrap().span), "\r\nx&amp;\0y\r");
            assert_eq!(doc.text_content(pre), "x&y\n");
            let p = first(&doc, "p");
            let title = doc.attribute(p, "title").unwrap();
            assert_eq!(doc.slice(title.value_span.as_ref().unwrap()), "a\r\n&#13;");
            assert_eq!(doc.attr_value(title).as_deref(), Some("a\n\r"));
            let img = first(&doc, "img");
            assert_eq!(
                doc.slice(doc.node(img).unwrap().name_span.as_ref().unwrap()),
                "image"
            );
            // Byte-exact round trip; children tile their parent.
            let noop = hsk_studio_web::SourcePatch::new(&doc, Vec::new());
            assert_eq!(apply(&doc, &noop).unwrap().source, src);
            let mut cur = 0;
            for &c in &doc.node(doc.root()).unwrap().children {
                let span = &doc.node(c).unwrap().span;
                assert_eq!(span.start, cur);
                cur = span.end;
            }
            assert_eq!(cur, src.len());
        },
    );
}

/// Edits write values that read back unchanged under the newline rules: an LF written right after a `pre`, `listing` or
/// `textarea` start tag gets one extra LF; CR is written as `&#13;`.
#[test]
fn edits_write_values_that_read_back_exactly() {
    bounded_case("edits_write_values_that_read_back_exactly", || {
        let cancel = CancellationToken::default();
        let doc = load("<pre>old</pre><textarea>\nx</textarea><div>d</div>");
        let pre = doc.key_of(first(&doc, "pre")).unwrap();
        let next = doc
            .apply_patch(&set_text(&doc, &pre, "\nnew\r").unwrap(), &cancel)
            .unwrap();
        assert_eq!(
            next.source(),
            "<pre>\n\nnew&#13;</pre><textarea>\nx</textarea><div>d</div>"
        );
        assert_eq!(next.text_content(first(&next, "pre")), "\nnew\r");
        let area = doc.key_of(first(&doc, "textarea")).unwrap();
        let next = doc
            .apply_patch(&set_text(&doc, &area, "\n").unwrap(), &cancel)
            .unwrap();
        assert_eq!(next.text_content(first(&next, "textarea")), "\n");
        let text_node = doc
            .key_of(doc.node(first(&doc, "pre")).unwrap().children[0])
            .unwrap();
        let next = doc
            .apply_patch(&set_text(&doc, &text_node, "\nt").unwrap(), &cancel)
            .unwrap();
        assert_eq!(next.text_content(first(&next, "pre")), "\nt");
        let div = doc.key_of(first(&doc, "div")).unwrap();
        let patch = insert_element(&doc, &div, 0, "textarea", &[], Some("\nv"), &cancel).unwrap();
        let next = doc.apply_patch(&patch, &cancel).unwrap();
        let areas = next.find_by_tag("textarea");
        assert_eq!(next.text_content(areas[1]), "\nv");
        // Not after a pre-like start tag: written as given.
        let next = doc
            .apply_patch(&set_text(&doc, &div, "\nd").unwrap(), &cancel)
            .unwrap();
        assert_eq!(next.text_content(first(&next, "div")), "\nd");
    });
}

/// D9, tree construction (13.2.6.4). (ii) A browser re-parents, reorders or drops an authored node: refused typed, one
/// input per row of the detection rule in `dom::construction`. (i) It only inserts implied elements (N1) or moves
/// skeleton whitespace (N2): accepted, the tree stays the authored tree.
#[test]
fn tree_construction_that_moves_authored_nodes_is_refused_typed() {
    bounded_case(
        "tree_construction_that_moves_authored_nodes_is_refused_typed",
        || {
            let refused: &[(&str, &str)] = &[
                ("<p><div></div></p>", "implied_end_tag"),
                ("<p><span><table></table></span></p>", "implied_end_tag"),
                ("<P><HR></P>", "implied_end_tag"),
                ("<h1><h2></h2></h1>", "implied_end_tag"),
                ("<form><div><form></form></div></form>", "ignored_token"),
                ("<ul><li><b><li></li></b></li></ul>", "implied_end_tag"),
                ("<dl><dd><div><dt></dt></div></dd></dl>", "implied_end_tag"),
                (
                    "<button><span><button></button></span></button>",
                    "implied_end_tag",
                ),
                ("<a><div><a></a></div></a>", "adoption_agency"),
                ("<nobr><b><nobr></nobr></b></nobr>", "adoption_agency"),
                ("<select><select></select></select>", "implied_end_tag"),
                ("<select><input></select>", "implied_end_tag"),
                ("<select><option><hr></option></select>", "implied_end_tag"),
                ("<option><option></option></option>", "implied_end_tag"),
                (
                    "<select><optgroup><optgroup></optgroup></optgroup></select>",
                    "implied_end_tag",
                ),
                ("<ruby><rt><rp></rp></rt></ruby>", "implied_end_tag"),
                // R9 review B: one refusing input for each arm that had none (rb/rtc, option with select in scope,
                // optgroup without select), and for the text arms of the column group and head noscript rows.
                ("<ruby><rb><rtc></rtc></rb></ruby>", "implied_end_tag"),
                (
                    "<select><option><option></option></option></select>",
                    "implied_end_tag",
                ),
                ("<option><optgroup></optgroup></option>", "implied_end_tag"),
                ("<table><colgroup>x</colgroup></table>", "table_content"),
                ("<head><noscript>x</noscript></head>", "document_structure"),
                (
                    "<select><button><selectedcontent></selectedcontent></button></select>",
                    "select_content",
                ),
                ("<table><tr></tr><div></div></table>", "table_content"),
                ("<table><td></td></table>", "table_content"),
                ("<table>x</table>", "table_content"),
                (
                    "<table><tbody><input type=text></tbody></table>",
                    "table_content",
                ),
                (
                    "<table><colgroup><style></style></colgroup></table>",
                    "table_content",
                ),
                ("<table><table></table></table>", "table_content"),
                (
                    "<table><tr><td><tr></tr></td></tr></table>",
                    "table_content",
                ),
                ("<td></td>", "ignored_token"),
                ("<div><body></body></div>", "ignored_token"),
                ("<p></p><!DOCTYPE html>", "ignored_token"),
                ("<keygen>x</keygen>", "void_end_tag"),
                (
                    "<html><head></head><title></title></html>",
                    "document_structure",
                ),
                ("<head><div></div></head>", "document_structure"),
                ("<head> x</head>", "document_structure"),
                ("<html><body></body>x</html>", "document_structure"),
                (
                    "<head><noscript><p></p></noscript></head>",
                    "document_structure",
                ),
            ];
            let wrong: Vec<String> = refused
                .iter()
                .filter(|(src, code)| outcome(src) != *code)
                .map(|(src, code)| format!("{src:?}: expected {code}, got {}", outcome(src)))
                .collect();
            assert!(wrong.is_empty(), "{}", wrong.join("\n"));
            // Accepted: scope boundaries and markers keep the authored tree; N1 implied elements are not created.
            let accepted = [
                "<p><button><div></div></button></p>",
                "<ul><li><ul><li></li></ul></li></ul>",
                "<a><object><a></a></object></a>",
                "<select><option></option><textarea></textarea></select>",
                "<table><input type=HIDDEN><col><tr><td>x</td></tr> <tbody></tbody></table>",
                "<template><td></td></template>x",
            ];
            for src in accepted.iter().take(5) {
                assert_eq!(outcome(src), "ok", "{src:?}");
            }
            // Conservative: template contents follow the "in body" rules here (a browser would accept a td there).
            assert_eq!(outcome(accepted[5]), "ignored_token");
            let doc = load("<table><col><tr><td>x</td></tr></table>");
            assert_eq!(
                shape(&doc),
                ["<table>", "  <col>", "  <tr>", "    <td>", "      \"x\""]
            );
            // N2: skeleton whitespace stays as authored text nodes with their spans.
            let page = "<!DOCTYPE html>\r\n<html>\r\n<head><title>t</title></head>\r\n<body>\r\n<p>x</p>\r\n</body>\r\n</html>\r\n";
            let doc = load(page);
            let tags: Vec<String> = doc.view().into_iter().filter_map(|r| r.tag).collect();
            assert_eq!(tags, ["html", "head", "title", "body", "p"]);
            assert_eq!(
                doc.slice(
                    &doc.node(doc.node(doc.root()).unwrap().children[1])
                        .unwrap()
                        .span
                ),
                "\r\n"
            );
        },
    );
}

// ---- R9 pre-review remediation (validation/R9-prereview-web.json F1-F4, P1-P4; R9 review B advisory) ----

/// Records every reference the asset gate offers; grants `logo.png`, answers `Unavailable` for everything else.
struct Port {
    calls: Mutex<Vec<String>>,
}
impl AssetPort for Port {
    fn resolve(&self, reference: &str, _: Loads) -> AssetDecision {
        self.calls.lock().unwrap().push(reference.to_owned());
        if reference == "logo.png" {
            AssetDecision::Granted { id: "logo".into() }
        } else {
            AssetDecision::Unavailable
        }
    }
}

const EXTERNAL: &str = "Denied(ExternalScheme)";
const PROTOCOL_RELATIVE: &str = "Denied(ProtocolRelative)";

/// (reference, outcome as Debug text) for every reference the gate reports, and the references the port was asked about.
fn gate(src: &str) -> (Vec<(String, String)>, Vec<String>) {
    let doc = load(src);
    let port = Port {
        calls: Mutex::new(Vec::new()),
    };
    let found = collect_assets(&doc, &port, &CancellationToken::default()).unwrap();
    let seen = found
        .iter()
        .map(|a| (a.reference.clone(), format!("{:?}", a.outcome)))
        .collect();
    (seen, port.calls.into_inner().unwrap())
}

fn pair(reference: &str, outcome: &str) -> (String, String) {
    (reference.to_owned(), outcome.to_owned())
}

/// F1 (same class as the R5/R6 gate failures: the crate's text differed from the browser's before CSS tokenizing). The
/// stylesheet of a foreign `style` is its child text content after input-stream preprocessing (13.2.3.5): a CR before
/// `]]>` is an LF, so `\` + CDATA(CR) + LF is an escaped newline followed by a newline that ends the string as a bad
/// string, and the `url()` after it is a real token a browser loads. V4-08: escape (the payload), decoys between the
/// pieces (comment, second CDATA section), CR LF / CR CR LF, upper case and inner whitespace; the same newline spellings in
/// an HTML `style` and a `style` attribute; and the reading where the escaped newline keeps the string open (no URL).
#[test]
fn foreign_style_cdata_is_preprocessed_before_css() {
    bounded_case("foreign_style_cdata_is_preprocessed_before_css", || {
        let loaded: &[(&str, &str)] = &[
            (
                "<svg><style>:root{b:\"x\\<![CDATA[\r]]>\n;background:url(https://evil.example/p.png)}</style></svg>",
                "https://evil.example/p.png",
            ),
            (
                "<svg><style>:root{b:\"x\\<![CDATA[\r]]><!-- c -->\n;background:url(https://evil.example/c.png)}</style></svg>",
                "https://evil.example/c.png",
            ),
            (
                "<svg><style>:root{b:\"x\\<![CDATA[\r]]><![CDATA[\n;background:url(https://evil.example/t.png)}]]></style></svg>",
                "https://evil.example/t.png",
            ),
            (
                "<svg><style>:root{b:\"x\\<![CDATA[\r\n]]>\n;background:url(https://evil.example/n.png)}</style></svg>",
                "https://evil.example/n.png",
            ),
            (
                "<svg><style>:root{b:\"x\\<![CDATA[\r\r\n]]>;background:url(https://evil.example/r.png)}</style></svg>",
                "https://evil.example/r.png",
            ),
            (
                "<SVG><STYLE>:root{b:'x\\<![CDATA[\r]]>\n;background:URL(  https://evil.example/u.png  )}</STYLE></SVG>",
                "https://evil.example/u.png",
            ),
            (
                "<style>:root{b:\"x\\\r\r\n;background:url(https://evil.example/h.png)}</style>",
                "https://evil.example/h.png",
            ),
            (
                "<div style=\"b:'x\\\r\r\n;background:url(https://evil.example/a.png)\"></div>",
                "https://evil.example/a.png",
            ),
        ];
        for (src, url) in loaded {
            let (seen, calls) = gate(src);
            assert_eq!(seen, [pair(url, EXTERNAL)], "{src:?}");
            assert!(calls.is_empty(), "{src:?}");
        }
        // The escaped CR LF (one newline after preprocessing) keeps the string open: a browser loads nothing either.
        let (seen, _) = gate(
            "<svg><style>:root{b:\"x\\<![CDATA[\r\n]]>;background:url(https://evil.example/s.png)}</style></svg>",
        );
        assert!(seen.is_empty(), "{seen:?}");
        // An HTML style body maps reference spans back to the source bytes through the preprocessing.
        let src = "<style>\r\n.a{background:url(https://evil.example/m.png)}</style>";
        let doc = load(src);
        let port = Port {
            calls: Mutex::new(Vec::new()),
        };
        let found = collect_assets(&doc, &port, &CancellationToken::default()).unwrap();
        assert_eq!(found.len(), 1);
        assert!(
            doc.slice(&found[0].span)
                .contains("https://evil.example/m.png")
        );
    });
}

/// The R9 root-cause rule (lead decision 14:14:23Z): every consumer that interprets text reads the text a browser's parser
/// hands on (input-stream preprocessing, the U+0000 rule of the place, CDATA as text), never the raw span bytes; spans only
/// locate. View rows, `text_content`, the preview's style sheets, the media block scan and the asset gate.
#[test]
fn text_consumers_read_the_text_a_browser_builds() {
    bounded_case("text_consumers_read_the_text_a_browser_builds", || {
        let cancel = CancellationToken::default();
        let doc = load(
            "<script>a\r\nb\0c</script><!--d\r\ne\0--><svg><text>x<![CDATA[f\r\ng\0]]>y</text><desc><![CDATA[h\0i]]></desc></svg>",
        );
        let rows = doc.view();
        let text_of = |kind: NodeKind| {
            rows.iter()
                .find(|r| r.kind == kind)
                .and_then(|r| r.text.clone())
        };
        assert_eq!(text_of(NodeKind::Raw).as_deref(), Some("a\nb\u{fffd}c"));
        assert_eq!(text_of(NodeKind::Comment).as_deref(), Some("d\ne\u{fffd}"));
        assert_eq!(doc.text_content(first(&doc, "text")), "xf\ng\u{fffd}y");
        assert_eq!(
            doc.text_content(first(&doc, "desc")),
            "hi",
            "CDATA at an HTML integration point follows the HTML rules: U+0000 dropped"
        );
        // Preview: a CR ends a CSS string like LF (the subset refuses an unterminated string typed instead of reading past
        // it); rule text comes from the preprocessed body, rule spans map back to the source bytes.
        let broken = load("<style>.x{content:\"a\r}.y{display:none}\"}</style><p class=y>t</p>");
        assert_eq!(
            preview_tree(&broken, 800.0, &cancel).unwrap_err().code(),
            "unterminated_css"
        );
        let doc =
            load("<style>\r\n.a{display:none}\r\n.b\0{display:none}\r\n</style><p class=a>t</p>");
        let preview = preview_tree(&doc, 800.0, &cancel).unwrap();
        let rule = &preview.active_rules[0];
        assert_eq!(
            (rule.selector.as_str(), doc.slice(&rule.selector_span)),
            (".a", ".a")
        );
        let p = first(&doc, "p");
        assert!(preview.nodes.iter().any(|n| n.id == p && !n.rendered));
        assert!(
            preview
                .unsupported
                .iter()
                .any(|u| doc.slice(&u.span) == ".b\0"),
            "{:?}",
            preview.unsupported
        );
        // Media blocks: the same text and span map.
        let doc = load("<style>@media (max-width:\r\n600px) { .a{} }\r\n</style>");
        let blocks = document_media_blocks(&doc, &cancel).unwrap();
        assert_eq!(doc.slice(&blocks[0].query_span), "(max-width:\r\n600px)");
        assert_eq!(
            blocks[0].breakpoints.as_ref().unwrap()[0].max_width_px,
            Some(600.0)
        );
        let broken = load("<style>@media (max-width: 600px) { .a{content:\"\r\"} }</style>");
        assert_eq!(
            document_media_blocks(&broken, &cancel).unwrap_err().code(),
            "unterminated_css"
        );
        // Asset gate: U+0000 in an HTML style url() reaches the port as U+FFFD (CSS reads it so).
        let (seen, calls) = gate("<style>.a{background:url(lo\0go.png)}</style>");
        assert_eq!(seen, [pair("lo\u{fffd}go.png", "Unavailable")]);
        assert_eq!(calls, ["lo\u{fffd}go.png"]);
    });
}

/// F2: 13.2.6.4.12 "in column group" has no entry for `</image>`, so "anything else" pops the authored `colgroup` and the
/// following `col` lands in an implied second colgroup: refused typed. Elsewhere the token is ignored.
#[test]
fn ignored_end_tag_in_colgroup_is_refused() {
    bounded_case("ignored_end_tag_in_colgroup_is_refused", || {
        assert_eq!(
            outcome("<table><colgroup></image><col></colgroup></table>"),
            "table_content"
        );
        assert_eq!(
            outcome("<TABLE><COLGROUP></IMAGE ></COLGROUP></TABLE>"),
            "table_content"
        );
        for ok in [
            "<table><colgroup><col></colgroup></image><tr><td>x</td></tr></table>",
            "<table><colgroup><template></image></template></colgroup></table>",
            "<table><colgroup><!--c--><col></colgroup></table>",
            "<head><noscript></image></noscript></head>",
        ] {
            assert_eq!(outcome(ok), "ok", "{ok:?}");
        }
    });
}

/// F3: 13.2.5.7 end tag open state, `>`: a parse error that emits no token, so after `<pre></>` the next token is still the
/// LF and it is dropped; a bogus comment (`</ x>`) and an ignored end tag (`</image>`) are tokens, so the LF after them
/// stays. Writing follows the same rule.
#[test]
fn end_tag_open_without_name_emits_no_token() {
    bounded_case("end_tag_open_without_name_emits_no_token", || {
        let cases: &[(&str, &str, &str)] = &[
            ("<pre></>\nx</pre>", "pre", "x"),
            ("<listing></></>\r\ny</listing>", "listing", "y"),
            ("<pre></>&#10;z</pre>", "pre", "z"),
            ("<PRE></>\r\nw</PRE>", "pre", "w"),
            ("<pre></ x>\nb</pre>", "pre", "\nb"),
            ("<pre></image>\nc</pre>", "pre", "\nc"),
            ("<pre></>a</>\nd</pre>", "pre", "a\nd"),
        ];
        for (src, tag, value) in cases {
            let doc = load(src);
            assert_eq!(doc.text_content(first(&doc, tag)), *value, "{src:?}");
        }
        let cancel = CancellationToken::default();
        let doc = load("<pre></>old</pre>");
        let pre = first(&doc, "pre");
        let text = doc.key_of(doc.node(pre).unwrap().children[1]).unwrap();
        let next = doc
            .apply_patch(&set_text(&doc, &text, "\nnew").unwrap(), &cancel)
            .unwrap();
        assert_eq!(next.source(), "<pre></>\n\nnew</pre>");
        assert_eq!(next.text_content(first(&next, "pre")), "\nnew");
    });
}

/// F4: `insert_element` writes for the destination's rules (13.2.6 dispatcher). Under an `svg` parent a `textarea` is an SVG
/// element (no first-LF rule) and a `style` is foreign (its text is markup, so it is escaped); `foreignObject` is an HTML
/// integration point, so a `textarea` there is HTML and gets the extra LF.
#[test]
fn insert_into_foreign_content_follows_the_foreign_rules() {
    bounded_case(
        "insert_into_foreign_content_follows_the_foreign_rules",
        || {
            let cancel = CancellationToken::default();
            let doc = load("<div></div><svg><g></g><foreignObject></foreignObject></svg>");
            let svg = doc.key_of(first(&doc, "svg")).unwrap();
            let patch =
                insert_element(&doc, &svg, 0, "textarea", &[], Some("\nv"), &cancel).unwrap();
            let next = doc.apply_patch(&patch, &cancel).unwrap();
            let area = first(&next, "textarea");
            assert_eq!(
                next.node(area).unwrap().namespace,
                hsk_studio_web::Namespace::Svg
            );
            assert_eq!(next.text_content(area), "\nv");
            let patch =
                insert_element(&doc, &svg, 0, "style", &[], Some("a&b<c>d\re"), &cancel).unwrap();
            let next = doc.apply_patch(&patch, &cancel).unwrap();
            assert_eq!(next.text_content(first(&next, "style")), "a&b<c>d\re");
            let fo = doc.key_of(first(&doc, "foreignobject")).unwrap();
            let patch =
                insert_element(&doc, &fo, 0, "textarea", &[], Some("\nw"), &cancel).unwrap();
            let next = doc.apply_patch(&patch, &cancel).unwrap();
            let area = first(&next, "textarea");
            assert_eq!(
                next.node(area).unwrap().namespace,
                hsk_studio_web::Namespace::Html
            );
            assert_eq!(next.text_content(area), "\nw");
        },
    );
}

/// P1: N2 is whitespace-only text nodes. The "initial", "before html" and "before head" modes ignore each leading
/// whitespace character token (also one written as a reference) of a run and keep the rest, so such a run's value would
/// differ: refused typed. Whitespace-only runs, runs after the head, and runs whose first token is not whitespace are
/// accepted.
#[test]
fn skeleton_text_with_leading_whitespace_is_refused() {
    bounded_case("skeleton_text_with_leading_whitespace_is_refused", || {
        for src in [
            " x",
            "<html>\n  x<p>y</p></html>",
            "<!DOCTYPE html>\nHello",
            "&#32;x",
            "<html>&#10;x</html>",
        ] {
            assert_eq!(outcome(src), "document_structure", "{src:?}");
        }
        for src in [
            "\n<!DOCTYPE html>\n<html>\n<head></head>\n<body>x</body>\n</html>\n",
            "x y",
            "<p>x</p> y",
            "<html><head></head> x</html>",
            "<html><title>t</title> x</html>",
            "\t<!-- c -->x",
            "\0 x",
        ] {
            assert_eq!(outcome(src), "ok", "{src:?}");
        }
    });
}

/// P3 (lead decision 13:44:24Z): a raw U+0000 in an attribute value reaches the gate as U+FFFD, exactly like `&#0;`, which
/// is what a browser's URL parser receives; the decision is the same for both spellings and nothing becomes less denied.
#[test]
fn raw_nul_in_url_attributes_reads_like_a_reference_to_it() {
    bounded_case(
        "raw_nul_in_url_attributes_reads_like_a_reference_to_it",
        || {
            let cases: &[(&str, &str, &str, usize)] = &[
                ("a\0b.png", "a\u{fffd}b.png", "Unavailable", 1),
                ("\0//e.invalid/x", "\u{fffd}//e.invalid/x", "Unavailable", 1),
                (
                    "//e\0.invalid/x",
                    "//e\u{fffd}.invalid/x",
                    PROTOCOL_RELATIVE,
                    0,
                ),
                (
                    "ht\0tp://e.invalid",
                    "ht\u{fffd}tp://e.invalid",
                    "Unavailable",
                    1,
                ),
                (
                    "java\0script:alert(1)",
                    "java\u{fffd}script:alert(1)",
                    "Unavailable",
                    1,
                ),
                ("http:\0//x", "http:\u{fffd}//x", EXTERNAL, 0),
            ];
            for (raw, reference, decision, port_calls) in cases {
                let (seen, calls) = gate(&format!("<img src=\"{raw}\">"));
                let (by_reference, calls_by_reference) =
                    gate(&format!("<img src=\"{}\">", raw.replace('\0', "&#0;")));
                assert_eq!(seen, [pair(reference, decision)], "{raw:?}");
                assert_eq!(seen, by_reference, "{raw:?}");
                assert_eq!(calls, calls_by_reference, "{raw:?}");
                assert_eq!(calls.len(), *port_calls, "{raw:?}");
            }
        },
    );
}

/// R9 review B (CX-VAL-009 cross-lane note): an HTML `<image>` start tag is an `img` (13.2.6.4.7), so its `srcset`
/// candidates are loaded and must be classified like `img srcset` (the gate missed them at f6e5a49). An SVG `image` has no
/// `srcset`.
#[test]
fn html_image_srcset_is_scanned_like_img() {
    bounded_case("html_image_srcset_is_scanned_like_img", || {
        let (seen, calls) = gate(
            "<image srcset=\"https://e.invalid/x 1x, logo.png 2x\"><IMAGE SRCSET='//e.invalid/y 2x'>\
             <svg><image srcset=\"https://e.invalid/z\"></image></svg>",
        );
        assert_eq!(
            seen,
            [
                pair("https://e.invalid/x", EXTERNAL),
                pair("logo.png", "Granted { id: \"logo\" }"),
                pair("//e.invalid/y", PROTOCOL_RELATIVE),
            ]
        );
        assert_eq!(calls, ["logo.png"]);
    });
}

/// P4: every refusal code is listed once in the DESCRIPTOR's `refused_typed` list.
#[test]
fn descriptor_lists_each_refusal_code_once() {
    bounded_case("descriptor_lists_each_refusal_code_once", || {
        let d = hsk_studio_web::DESCRIPTOR;
        let start = d.find("\"refused_typed\":[").expect("refused_typed list");
        let list = &d[start..start + d[start..].find(']').expect("list end")];
        for code in [
            "foreign_breakout",
            "scripting_dependent",
            "select_content",
            "frameset_document",
            "plaintext_element",
            "implied_end_tag",
            "adoption_agency",
            "table_content",
            "ignored_token",
            "document_structure",
        ] {
            assert_eq!(list.matches(&format!("\"{code}")).count(), 1, "{code}");
        }
    });
}
