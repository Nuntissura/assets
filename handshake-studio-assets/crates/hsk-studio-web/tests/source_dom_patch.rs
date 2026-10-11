use hsk_studio_accord::CancellationToken;
use hsk_studio_web::responsive::{Breakpoint, document_media_blocks, matches_any, parse_media_query};
use hsk_studio_web::{
    Applied, EditRefusal, Limits, LimitKind, LossKind, MalformedKind, NodeKey, NodeKind, SourcePatch, UnsupportedKind, WebDoc, WebError,
    apply, decode_entities, escape_attr, escape_text, insert_element, parse, remove_attribute, remove_node, set_attribute, set_text,
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
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("case {name} exceeded declared 45-second deadline"),
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("case {name} lost its deadline worker result"),
    }
}

// Synthetic document: CRLF, tabs, mixed-case tags, all quoting styles, RCDATA title, raw script,
// foreign svg, comment containing markup, and an opaque CDATA token.
const DOC: &str = "<!DOCTYPE html>\r\n<HTML lang=en>\r\n\t<Head><title>T &amp; Co</title></Head>\r\n\t<BODY class='a b' data-x = \"1\">\r\n\t\t<!-- keep <b>this</b> -->\r\n\t\t<img src=pic.png alt=\"\" disabled>\r\n\t\t<p id=\"p1\">Hello &amp; <em>world</em></p>\r\n\t\t<script>if (a < b && c) { document.write(\"</p>\"); }</script>\r\n\t\t<svg viewBox=\"0 0 1 1\"><path d=\"M0 0\"/></svg>\r\n\t\t<![CDATA[ opaque ]]>\r\n\t</BODY>\r\n</HTML>\r\n";

fn load(src: &str) -> WebDoc {
    parse(src, 0, &Limits::default(), &CancellationToken::default()).unwrap()
}

fn key(doc: &WebDoc, tag: &str) -> NodeKey {
    doc.key_of(doc.find_by_tag(tag)[0]).unwrap()
}

/// Children tile their parent's content exactly, so concatenating spans reproduces the source bytes.
fn assert_tiles(doc: &WebDoc) {
    for id in 0..doc.len() {
        let n = doc.node(id).unwrap();
        let (start, end) = match n.kind {
            NodeKind::Document => (0, doc.source().len()),
            NodeKind::Element => match n.content_span() {
                Some(c) => {
                    assert_eq!(n.span.start, n.open_span.as_ref().unwrap().start);
                    assert_eq!(n.span.end, n.close_span.as_ref().unwrap().end);
                    (c.start, c.end)
                }
                None => {
                    assert!(n.children.is_empty());
                    continue;
                }
            },
            _ => {
                assert!(n.children.is_empty());
                continue;
            }
        };
        let mut cur = start;
        for &c in &n.children {
            let span = &doc.node(c).unwrap().span;
            assert_eq!(span.start, cur, "gap before child {c} of node {id}");
            cur = span.end;
        }
        assert_eq!(cur, end, "children of node {id} do not reach its content end");
    }
}

/// Byte-diff oracle: only the edited span differs, everything before and after is identical.
fn assert_only_span_changed(old: &WebDoc, patch: &SourcePatch) -> Applied {
    let applied = apply(old, patch).unwrap();
    assert_eq!(patch.edits.len(), 1);
    let (e, new) = (&patch.edits[0], &applied.source);
    assert_eq!(&old.source()[..e.span.start], &new[..e.span.start]);
    assert_eq!(&old.source()[e.span.end..], &new[applied.changed[0].end..]);
    assert_eq!(&new[applied.changed[0].clone()], e.replacement);
    assert_eq!(applied.revision, old.revision() + 1);
    applied
}

fn expect_source(old: &WebDoc, patch: &SourcePatch, from: &str, to: &str) {
    let applied = assert_only_span_changed(old, patch);
    assert_eq!(applied.source, DOC.replacen(from, to, 1), "{from} -> {to}");
    // The patched text re-projects cleanly at the new revision.
    let next = old.apply_patch(patch, &CancellationToken::default()).unwrap();
    assert_eq!(next.source(), applied.source);
    assert_eq!(next.revision(), 1);
    assert_tiles(&next);
}

#[test]
fn round_trip_is_byte_exact_and_spans_tile() {
    bounded_case("round_trip_is_byte_exact_and_spans_tile", || {
        let doc = load(DOC);
        assert_tiles(&doc);
        let noop = SourcePatch::new(&doc, Vec::new());
        let applied = apply(&doc, &noop).unwrap();
        assert_eq!((applied.source.as_str(), applied.revision), (DOC, 0));
        assert_eq!(doc.sha256_hex(), hsk_studio_web::sha256_hex(DOC.as_bytes()));
        // Original case, quoting and the opaque token survive in the projection.
        assert_eq!(doc.tag_name(doc.find_by_tag("body")[0]), Some("BODY"));
        assert_eq!(doc.loss().count(LossKind::Cdata), 1);
        assert_eq!(doc.loss().count(LossKind::ForeignContent), 1);
        let rows = doc.view();
        assert_eq!(rows.len(), doc.len());
        assert!(rows.iter().any(|r| r.kind == NodeKind::Unsupported && r.text.as_deref() == Some("<![CDATA[ opaque ]]>")));
        // Keys are stable paths that resolve back to the same node.
        for id in 0..doc.len() {
            assert_eq!(doc.resolve(&doc.key_of(id).unwrap()), Some(id));
        }
        // The script body stays one opaque span with its `</p>` text untouched.
        let script = doc.find_by_tag("script")[0];
        let raw = doc.node(doc.node(script).unwrap().children[0]).unwrap();
        assert_eq!(raw.kind, NodeKind::Raw);
        assert_eq!(doc.slice(&raw.span), "if (a < b && c) { document.write(\"</p>\"); }");
        // Decoded views: RCDATA title, attribute values, mixed text.
        assert_eq!(doc.text_content(doc.find_by_tag("title")[0]), "T & Co");
        assert_eq!(doc.text_content(doc.find_by_html_id("p1").unwrap()), "Hello & world");
    });
}

#[test]
fn minimal_diff_patches_rewrite_only_affected_spans() {
    bounded_case("minimal_diff_patches_rewrite_only_affected_spans", || {
        let doc = load(DOC);
        let (html, body, img, em) = (key(&doc, "html"), key(&doc, "body"), key(&doc, "img"), key(&doc, "em"));
        expect_source(&doc, &set_attribute(&doc, &body, "class", "a b c").unwrap(), "class='a b'", "class='a b c'");
        expect_source(&doc, &set_attribute(&doc, &body, "data-x", "2").unwrap(), "data-x = \"1\"", "data-x = \"2\"");
        expect_source(&doc, &set_attribute(&doc, &html, "LANG", "en-GB").unwrap(), "lang=en>", "lang=en-GB>");
        expect_source(&doc, &set_attribute(&doc, &html, "lang", "en GB").unwrap(), "lang=en>", "lang=\"en GB\">");
        expect_source(&doc, &set_attribute(&doc, &img, "disabled", "x").unwrap(), "disabled>", "disabled=\"x\">");
        expect_source(
            &doc,
            &set_attribute(&doc, &img, "title", "a\"b&c").unwrap(),
            "alt=\"\" disabled>",
            "alt=\"\" disabled title=\"a&quot;b&amp;c\">",
        );
        expect_source(&doc, &remove_attribute(&doc, &img, "alt").unwrap(), " alt=\"\"", "");
        expect_source(&doc, &set_text(&doc, &em, "a<b").unwrap(), "<em>world</em>", "<em>a&lt;b</em>");
        expect_source(&doc, &set_text(&doc, &key(&doc, "title"), "A & B").unwrap(), "T &amp; Co", "A &amp; B");
        expect_source(&doc, &set_text(&doc, &key(&doc, "script"), "x();").unwrap(), "if (a < b && c) { document.write(\"</p>\"); }", "x();");
        expect_source(&doc, &remove_node(&doc, &doc.key_of(doc.node(doc.find_by_tag("body")[0]).unwrap().children[1]).unwrap()).unwrap(), "<!-- keep <b>this</b> -->", "");
        // Setting the current value is a no-op patch: same bytes, same revision.
        let same = set_attribute(&doc, &body, "class", "a b").unwrap();
        assert!(same.is_noop());
        assert_eq!(apply(&doc, &same).unwrap().revision, 0);
        // Insert: a void and a text element at the end of body, each exactly one inserted span.
        let cancel = CancellationToken::default();
        let body_id = doc.find_by_tag("body")[0];
        let end = doc.node(body_id).unwrap().children.len();
        let ins = insert_element(&doc, &body, end, "br", &[("class", "x\"y")], None, &cancel).unwrap();
        let applied = assert_only_span_changed(&doc, &ins);
        assert!(applied.source.contains("<br class=\"x&quot;y\">\t</BODY>") || applied.source.contains("<br class=\"x&quot;y\"></BODY>"));
        let li = insert_element(&doc, &body, 0, "li", &[], Some("a&b"), &cancel).unwrap();
        assert_eq!(li.edits[0].replacement, "<li>a&amp;b</li>");
        assert_only_span_changed(&doc, &li);
        // Refusals: markup children, opaque token, document root, unsafe raw text, bad names, bad fragments.
        let p = doc.key_of(doc.find_by_html_id("p1").unwrap()).unwrap();
        assert_eq!(set_text(&doc, &p, "x"), Err(WebError::NotEditable { reason: EditRefusal::HasMarkupChildren }));
        let cdata = doc.key_of(doc.node(body_id).unwrap().children.iter().copied().find(|&c| doc.node(c).unwrap().kind == NodeKind::Unsupported).unwrap()).unwrap();
        assert_eq!(set_text(&doc, &cdata, "x"), Err(WebError::NotEditable { reason: EditRefusal::Opaque }));
        assert_eq!(remove_node(&doc, &cdata), Err(WebError::NotEditable { reason: EditRefusal::Opaque }));
        assert_eq!(remove_node(&doc, &NodeKey::new("/")), Err(WebError::NotEditable { reason: EditRefusal::DocumentRoot }));
        assert_eq!(set_text(&doc, &key(&doc, "script"), "a</SCRIPT>"), Err(WebError::NotEditable { reason: EditRefusal::UnsafeRawText }));
        assert_eq!(set_attribute(&doc, &body, "a b", "x"), Err(WebError::InvalidName));
        assert_eq!(set_attribute(&doc, &NodeKey::new("/99:div"), "a", "x"), Err(WebError::NoSuchNode));
        assert_eq!(remove_attribute(&doc, &body, "nope"), Err(WebError::NoSuchAttribute));
        assert!(matches!(
            insert_element(&doc, &img, 0, "b", &[], None, &cancel),
            Err(WebError::NotEditable { reason: EditRefusal::Void })
        ));
        assert!(matches!(
            hsk_studio_web::insert_fragment(&doc, &body, 0, "<b>x</i>", &cancel),
            Err(WebError::Malformed { kind: MalformedKind::InvalidFragment, .. })
        ));
        assert!(matches!(
            hsk_studio_web::insert_fragment(&doc, &body, 0, "<![CDATA[x]]>", &cancel),
            Err(WebError::Unsupported { kind: UnsupportedKind::FragmentConstruct, .. })
        ));
        // Revision binding: stale revision, same-number conflict, overlapping edits, out-of-range span.
        let patch = set_attribute(&doc, &body, "class", "z").unwrap();
        let next = doc.apply_patch(&patch, &cancel).unwrap();
        assert_eq!(apply(&next, &patch), Err(WebError::StaleRevision { expected: 1, found: 0 }));
        let other = parse("<p>x</p>", 0, &Limits::default(), &cancel).unwrap();
        assert!(matches!(apply(&other, &patch), Err(WebError::Conflict { .. })));
        let twice = patch.clone().compose(set_attribute(&doc, &body, "class", "y").unwrap()).unwrap();
        assert!(matches!(apply(&doc, &twice), Err(WebError::OverlappingEdits { .. })));
        let both = patch.clone().compose(set_attribute(&doc, &html, "lang", "de").unwrap()).unwrap();
        assert_eq!(apply(&doc, &both).unwrap().changed.len(), 2);
        let mut bad = patch.clone();
        bad.edits[0].span = 0..DOC.len() + 1;
        assert!(matches!(apply(&doc, &bad), Err(WebError::InvalidSpan { .. })));
    });
}

#[test]
fn hostile_input_is_bounded_and_typed() {
    bounded_case("hostile_input_is_bounded_and_typed", || {
        let cancel = CancellationToken::default();
        let try_parse = |src: &str, limits: &Limits| parse(src, 0, limits, &cancel).expect_err("must be refused");
        let limits = Limits { max_depth: 64, max_nodes: 1000, max_bytes: 4096, max_attributes: 16, ..Limits::default() };
        let deep = format!("{}{}", "<div>".repeat(200), "</div>".repeat(200));
        assert!(matches!(try_parse(&deep, &Limits { max_bytes: 1 << 20, ..limits }), WebError::LimitExceeded { limit: LimitKind::Depth, .. }));
        let ok_deep = format!("{}{}", "<div>".repeat(64), "</div>".repeat(64));
        assert!(parse(&ok_deep, 0, &limits, &cancel).is_ok());
        let wide = "<i></i>".repeat(1500);
        assert!(matches!(try_parse(&wide, &Limits { max_bytes: 1 << 20, ..limits }), WebError::LimitExceeded { limit: LimitKind::Nodes, .. }));
        let big = "x".repeat(5000);
        assert_eq!(try_parse(&big, &limits), WebError::LimitExceeded { limit: LimitKind::Bytes, span: 4096..5000 });
        let many = format!("<a {}>", "x ".repeat(40));
        assert!(matches!(try_parse(&many, &limits), WebError::LimitExceeded { limit: LimitKind::Attributes, .. }));
        // Structure that cannot be determined without guessing is refused with an exact span.
        assert_eq!(try_parse("<b><i></b></i>", &limits), WebError::Malformed { kind: MalformedKind::MisnestedEndTag, span: 6..10 });
        assert_eq!(try_parse("a</div>", &limits), WebError::Malformed { kind: MalformedKind::StrayEndTag, span: 1..7 });
        assert_eq!(try_parse("<br></br>", &limits), WebError::Malformed { kind: MalformedKind::VoidEndTag, span: 4..9 });
        assert_eq!(try_parse("<ul><li>a<li>b</ul>", &limits), WebError::Unsupported { kind: UnsupportedKind::ImpliedEndTag, span: 14..19 });
        assert_eq!(try_parse("<p>never closed", &limits), WebError::Unsupported { kind: UnsupportedKind::ImpliedEndTag, span: 0..3 });
        assert_eq!(try_parse("<div>x", &limits), WebError::Malformed { kind: MalformedKind::UnclosedElement, span: 0..5 });
        assert_eq!(try_parse("<script>alert(1)", &limits), WebError::Malformed { kind: MalformedKind::UnterminatedRawText, span: 0..8 });
        assert_eq!(try_parse("<p>x<!-- no end", &limits), WebError::Malformed { kind: MalformedKind::UnterminatedComment, span: 4..15 });
        assert_eq!(try_parse("<a href=\"x", &limits), WebError::Malformed { kind: MalformedKind::UnterminatedValue, span: 8..10 });
        assert_eq!(try_parse("<a", &limits), WebError::Malformed { kind: MalformedKind::UnterminatedTag, span: 0..2 });
        assert!(matches!(try_parse("<plaintext>x", &limits), WebError::Unsupported { kind: UnsupportedKind::PlaintextElement, .. }));
        // Cancellation, before and between chunks.
        let stop = CancellationToken::default();
        stop.cancel();
        assert_eq!(parse("<p></p>", 0, &limits, &stop).err(), Some(WebError::Canceled));
        // Opaque and multi-byte input never panics and keeps exact spans.
        let odd = parse("<p>日本<![CDATA[é]]><a ü=ö title=\"ß\">é</a>< 3 <</p>", 0, &limits, &cancel).unwrap();
        assert_eq!(odd.loss().count(LossKind::Cdata), 1);
        assert_tiles(&odd);
        // Large flat documents parse in linear time; script text with `<` stays opaque.
        let flat = "x<b>y</b>".repeat(20_000);
        let flat_doc = parse(&flat, 0, &Limits::default(), &cancel).unwrap();
        assert_eq!(flat_doc.len(), 1 + 20_000 * 3);
        assert_tiles(&flat_doc);
        let lt = parse(&"< ".repeat(500_000), 0, &Limits::default(), &cancel).unwrap();
        assert_eq!(lt.len(), 2);
        // Scripts never execute and nothing is fetched: this crate has no I/O; `<script src>` is just an attribute.
        let s = load("<script src=\"http://example.invalid/x.js\"></script>");
        let id = s.find_by_tag("script")[0];
        assert_eq!(s.attribute(id, "src").and_then(|a| s.attr_value(a)).as_deref(), Some("http://example.invalid/x.js"));
    });
}

#[test]
fn entities_decode_the_common_set_and_report_the_rest() {
    bounded_case("entities_decode_the_common_set_and_report_the_rest", || {
        let input = "a &amp; b &lt;&gt; &quot;q&quot; &#65;&#x42;&#X43; &nbsp;&copy; &bogus; &amp &#0; &#xD800; &#1114112; AT&T; x";
        let decoded = decode_entities(input);
        assert_eq!(
            decoded.text,
            "a & b <> \"q\" ABC \u{a0}\u{a9} &bogus; &amp \u{fffd} \u{fffd} \u{fffd} AT&T; x"
        );
        let spans: Vec<&str> = decoded.unresolved.iter().map(|r| &input[r.clone()]).collect();
        assert_eq!(spans, ["&bogus;", "&T;"]);
        assert!(decoded.text.len() <= input.len());
        // Escaping is the inverse of decoding for the characters it escapes.
        for s in ["a&b<c>d&amp;e", "\"quoted\" 'single' &lt;", "plain"] {
            assert_eq!(decode_entities(&escape_text(s)).text, s);
            assert_eq!(decode_entities(&escape_attr(s, '"')).text, s);
            assert_eq!(decode_entities(&escape_attr(s, '\'')).text, s);
        }
        assert!(!escape_attr("\"'", '"').contains('"'));
        // Through the DOM: attribute values and text content are decoded, the source stays as written.
        let doc = load("<a title=\"x &amp; y &#169; &#x1F600;\">1 &lt; 2 <b>&amp;</b></a>");
        let a = doc.find_by_tag("a")[0];
        assert_eq!(doc.attribute(a, "title").and_then(|t| doc.attr_value(t)).as_deref(), Some("x & y \u{a9} \u{1f600}"));
        assert_eq!(doc.text_content(a), "1 < 2 &");
        assert!(doc.source().contains("&amp;"));
    });
}

#[test]
fn breakpoints_parse_match_and_refuse_typed() {
    bounded_case("breakpoints_parse_match_and_refuse_typed", || {
        let bp = |min, max| Breakpoint { min_width_px: min, max_width_px: max };
        assert_eq!(parse_media_query("(max-width: 600px)").unwrap(), [bp(None, Some(600.0))]);
        assert_eq!(parse_media_query("screen and (min-width: 600px) and (max-width: 900px)").unwrap(), [bp(Some(600.0), Some(900.0))]);
        assert_eq!(
            parse_media_query("only screen and (MIN-WIDTH:48.5px), (max-width:0)").unwrap(),
            [bp(Some(48.5), None), bp(None, Some(0.0))]
        );
        assert_eq!(parse_media_query("(min-width: 100px) and (min-width: 300px)").unwrap(), [bp(Some(300.0), None)]);
        assert_eq!(parse_media_query("").unwrap(), [bp(None, None)]);
        assert_eq!(parse_media_query("all").unwrap(), [bp(None, None)]);
        let list = parse_media_query("(max-width: 600px), (min-width: 1200px)").unwrap();
        assert!([500.0, 600.0, 1200.0, 2000.0].iter().all(|&w| matches_any(&list, w)));
        assert!([600.5, 800.0, 1199.9].iter().all(|&w| !matches_any(&list, w)));
        // Typed refusals, never guessed.
        let kind = |q: &str| match parse_media_query(q) {
            Err(WebError::Unsupported { kind, .. }) => format!("unsupported:{}", kind.code()),
            Err(WebError::Malformed { kind, .. }) => format!("malformed:{}", kind.code()),
            other => panic!("{q}: {other:?}"),
        };
        assert_eq!(kind("print"), "unsupported:media_type");
        assert_eq!(kind("(orientation: landscape)"), "unsupported:media_feature");
        assert_eq!(kind("(prefers-color-scheme: dark)"), "unsupported:media_feature");
        assert_eq!(kind("(min-width: 40em)"), "unsupported:media_unit");
        assert_eq!(kind("(width >= 600px)"), "unsupported:media_syntax");
        assert_eq!(kind("not screen"), "unsupported:media_syntax");
        assert_eq!(kind("(min-width: 600px) or (max-width: 10px)"), "unsupported:media_syntax");
        assert_eq!(kind("(min-width: 600px"), "malformed:invalid_media_query");
        assert_eq!(kind("(min-width: -5px)"), "malformed:invalid_media_query");
        assert_eq!(kind("screen and"), "malformed:invalid_media_query");
        assert_eq!(kind("(min-width: 1px),"), "malformed:invalid_media_query");
        assert_eq!(kind("(min-width: 600)"), "malformed:invalid_media_query");

        // Scan `<style>` bodies: comments and strings skipped, nesting depth reported, per-query errors typed.
        let html = "<style>@media (max-width: 600px) { .nav { display:none } } /* @media (min-width:1px) {} */ \
                    a::before { content: \"}{ @media\" } @media print { a{} } @media screen { @media (min-width: 10em) { b{} } }</style>";
        let doc = load(html);
        let blocks = document_media_blocks(&doc, &CancellationToken::default()).unwrap();
        assert_eq!(blocks.len(), 4);
        assert_eq!(doc.slice(&blocks[0].query_span), "(max-width: 600px)");
        assert_eq!(doc.slice(&blocks[0].body_span), " .nav { display:none } ");
        assert_eq!(blocks[0].breakpoints.as_ref().unwrap(), &[bp(None, Some(600.0))]);
        assert!(matches!(blocks[1].breakpoints, Err(WebError::Unsupported { kind: UnsupportedKind::MediaType, .. })));
        assert_eq!(doc.slice(&blocks[1].query_span), "print");
        assert_eq!((blocks[2].depth, blocks[3].depth), (0, 1));
        let nested_err = blocks[3].breakpoints.as_ref().unwrap_err();
        assert!(matches!(nested_err, WebError::Unsupported { kind: UnsupportedKind::MediaUnit, .. }));
        assert!(blocks[3].query_span.contains(&nested_err.span().unwrap().start), "error span is a document offset");
        let broken = load("<style>@media (max-width: 600px) { a{}</style>");
        assert!(matches!(
            document_media_blocks(&broken, &CancellationToken::default()),
            Err(WebError::Malformed { kind: MalformedKind::UnterminatedCss, .. })
        ));
    });
}
