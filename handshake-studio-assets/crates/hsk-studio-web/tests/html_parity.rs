//! Focused parity tests (lane SMB-WEB-PARITY): the crate reads names, values and text the way the HTML Standard's
//! tokenizer and tree builder build them (https://html.spec.whatwg.org/multipage/parsing.html), and refuses typed every
//! input whose tree a browser builds differently from the authored tree, except the documented normalisations N1/N2.
//! Each test pins one disagreement class recorded by the differential lane SMB-WEB-DIFFTEST (D1-D9); the assertions use
//! only API that existed before the fix (refusal codes as strings), so the same file is the red proof on the old crate.

use hsk_studio_accord::CancellationToken;
use hsk_studio_web::{
    ContentModel, Limits, NodeKind, WebDoc, apply, decode_entities, escape_attr, escape_text,
    insert_element, parse, preview_tree, set_attribute, set_text,
};

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
