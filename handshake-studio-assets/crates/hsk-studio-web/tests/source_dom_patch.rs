use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{Budget, DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort};
use hsk_studio_web::responsive::{Breakpoint, document_media_blocks, matches_any, parse_media_query};
use hsk_studio_web::{
    Applied, AssetDecision, AssetOutcome, AssetPort, AssetSource, DenyReason, EditRefusal, HiddenReason, LimitKind, Limits, LossKind,
    MalformedKind, NodeKey, NodeKind, SourcePatch, UnsupportedKind, UnsupportedRuleReason, WebDoc, WebError, apply, collect_assets,
    decode_entities, escape_attr, escape_text, insert_element, outcome_of, parse, preview_tree, refresh, remove_attribute, remove_node,
    report, set_attribute, set_text,
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
            "a & b <> \"q\" ABC \u{a0}\u{a9} &bogus; & \u{fffd} \u{fffd} \u{fffd} AT&T; x"
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

#[test]
fn code_edit_refresh_same_revision() {
    bounded_case("code_edit_refresh_same_revision", || {
        let cancel = CancellationToken::default();
        let doc = load(DOC);
        let (body, p) = (key(&doc, "body"), doc.key_of(doc.find_by_html_id("p1").unwrap()).unwrap());
        // A visual insert, then the same bytes arriving as a code edit: refresh == parse(apply(..)) at R+1.
        let patch = insert_element(&doc, &body, 0, "nav", &[("id", "top")], Some("Menu"), &cancel).unwrap();
        let applied = apply(&doc, &patch).unwrap();
        let reparsed = doc.apply_patch(&patch, &cancel).unwrap();
        let (fresh, map) = refresh(&doc, &applied.source, 1, &cancel).unwrap();
        assert_eq!((fresh.revision(), fresh.sha256_hex()), (reparsed.revision(), reparsed.sha256_hex()));
        assert_eq!(fresh.view(), reparsed.view());
        // Surviving nodes keep identity through shifted positions; only the new subtree is reported.
        let moved = map.map_key(&doc, &fresh, &p).unwrap();
        assert_ne!(moved, p, "the insert shifted the key");
        assert_eq!(fresh.resolve(&moved), fresh.find_by_html_id("p1"));
        assert_eq!(map.kept(), doc.len());
        assert_eq!((map.removed_roots().len(), map.removed_nodes()), (0, 0));
        assert_eq!((map.added_roots().len(), map.added_nodes()), (1, 2));
        assert_eq!(fresh.tag_name(map.added_roots()[0]), Some("nav"));
        let script = doc.find_by_tag("script")[0];
        let raw = doc.node(script).unwrap().children[0];
        assert_eq!(fresh.slice(&fresh.node(map.new_id(raw).unwrap()).unwrap().span), doc.slice(&doc.node(raw).unwrap().span));
        // A removal reports the removed subtree and keeps every other node.
        let comment = doc.key_of(doc.node(doc.find_by_tag("body")[0]).unwrap().children[1]).unwrap();
        let gone = apply(&doc, &remove_node(&doc, &comment).unwrap()).unwrap();
        let (after, map) = refresh(&doc, &gone.source, 1, &cancel).unwrap();
        // The comment and the whitespace text after it merge into one text node when the comment is gone.
        assert!(map.removed_keys(&doc).contains(&comment));
        assert_eq!((map.removed_nodes(), map.added_nodes(), map.kept()), (2, 0, doc.len() - 2));
        assert_eq!(after.len(), doc.len() - 2);
        // Elements with an HTML id keep identity when reordered inside one parent.
        let old = load("<div><p id=a>1</p><p id=b>2</p><p id=c>3</p></div>");
        let (new, map) = refresh(&old, "<div><p id=c>3</p><p id=b>2</p><p id=a>1</p></div>", 1, &cancel).unwrap();
        for id in ["a", "b", "c"] {
            let (o, n) = (old.find_by_html_id(id).unwrap(), new.find_by_html_id(id).unwrap());
            assert_eq!(map.new_id(o), Some(n), "id {id}");
            assert_eq!(map.old_id(n), Some(o));
        }
        assert_eq!((map.added_nodes(), map.removed_nodes()), (0, 0));
        // The next revision is the only revision a refresh accepts; cancellation and bad source are typed.
        assert_eq!(refresh(&doc, &gone.source, 0, &cancel).err(), Some(WebError::StaleRevision { expected: 1, found: 0 }));
        assert_eq!(refresh(&doc, &gone.source, 2, &cancel).err(), Some(WebError::StaleRevision { expected: 1, found: 2 }));
        assert!(matches!(refresh(&doc, "<b><i></b></i>", 1, &cancel), Err(WebError::Malformed { kind: MalformedKind::MisnestedEndTag, .. })));
        let stop = CancellationToken::default();
        stop.cancel();
        assert_eq!(refresh(&doc, &gone.source, 1, &stop).err(), Some(WebError::Canceled));
    });
}

struct Grants {
    calls: Mutex<Vec<String>>,
}
impl AssetPort for Grants {
    fn resolve(&self, reference: &str) -> AssetDecision {
        self.calls.lock().unwrap().push(reference.to_owned());
        match reference {
            "logo.png" => AssetDecision::Granted { id: "asset-logo".into() },
            "theme.css" => AssetDecision::Granted { id: "asset-theme".into() },
            "base.css" => AssetDecision::Granted { id: "asset-base".into() },
            "sub/e.png" => AssetDecision::Granted { id: "asset-sub".into() },
            "missing.png" => AssetDecision::Unavailable,
            _ => AssetDecision::Denied,
        }
    }
}

#[test]
fn external_asset_denial_cancel_stale() {
    bounded_case("external_asset_denial_cancel_stale", || {
        let cancel = CancellationToken::default();
        let html = "<img src=\"logo.png\"><img src=\"missing.png\"><img src=\"http://e.invalid/a.png\"><img src=\"//cdn.invalid/b.png\">\
            <a href=\"file:///c:/x\"></a><a href=\"java&#10;script:alert(1)\"></a><a href=\"JavaScript:1\"></a>\
            <img src=\"data:image/png;base64,AAAA\"><a href=\"#top\"></a><a href=\"mailto:x@y.invalid\"></a>\
            <div style=\"background:url('bg.png'), url(//cdn.invalid/c.png)\"></div><link href=\"theme.css\" rel=stylesheet>\
            <style>@import \"base.css\"; .a{background:url(http://e.invalid/d.png)} /* url(nope.png) */ .b{background:url( sub/e.png )}</style>";
        let doc = load(html);
        let port = Grants { calls: Mutex::new(Vec::new()) };
        let found = collect_assets(&doc, &port, &cancel).unwrap();
        let seen: Vec<(&str, AssetOutcome)> = found.iter().map(|a| (a.reference.as_str(), a.outcome.clone())).collect();
        let granted = |id: &str| AssetOutcome::Granted { id: id.into() };
        let denied = AssetOutcome::Denied;
        assert_eq!(
            seen,
            [
                ("logo.png", granted("asset-logo")),
                ("missing.png", AssetOutcome::Unavailable),
                ("http://e.invalid/a.png", denied(DenyReason::ExternalScheme)),
                ("//cdn.invalid/b.png", denied(DenyReason::ProtocolRelative)),
                ("file:///c:/x", denied(DenyReason::FileScheme)),
                ("java\nscript:alert(1)", denied(DenyReason::ScriptScheme)),
                ("JavaScript:1", denied(DenyReason::ScriptScheme)),
                ("data:image/png;base64,AAAA", AssetOutcome::Inline),
                ("#top", AssetOutcome::SameDocument),
                ("mailto:x@y.invalid", denied(DenyReason::ExternalScheme)),
                ("bg.png", denied(DenyReason::NotGranted)),
                ("//cdn.invalid/c.png", denied(DenyReason::ProtocolRelative)),
                ("theme.css", granted("asset-theme")),
                ("base.css", granted("asset-base")),
                ("http://e.invalid/d.png", denied(DenyReason::ExternalScheme)),
                ("sub/e.png", granted("asset-sub")),
            ]
        );
        // The port only ever saw relative references, once each, in document order.
        assert_eq!(*port.calls.lock().unwrap(), ["logo.png", "missing.png", "bg.png", "theme.css", "base.css", "sub/e.png"]);
        // Spans point at the reference bytes, also inside style attributes and style bodies.
        let sources: Vec<&AssetSource> = found.iter().map(|a| &a.source).collect();
        assert_eq!(sources[0], &AssetSource::Attribute("src".into()));
        assert_eq!((sources[10], sources[13]), (&AssetSource::InlineStyle, &AssetSource::StyleBody));
        for a in found.iter().filter(|a| a.reference != "java\nscript:alert(1)") {
            assert_eq!(doc.slice(&a.span), a.reference, "{a:?}");
        }
        // Cancellation stops the walk before the port is consulted; stale revisions and bad widths are typed.
        let stop = CancellationToken::default();
        stop.cancel();
        let quiet = Grants { calls: Mutex::new(Vec::new()) };
        assert_eq!(collect_assets(&doc, &quiet, &stop).err(), Some(WebError::Canceled));
        assert!(quiet.calls.lock().unwrap().is_empty());
        let patch = set_attribute(&doc, &doc.key_of(doc.find_by_tag("img")[0]).unwrap(), "src", "other.png").unwrap();
        let next = doc.apply_patch(&patch, &cancel).unwrap();
        assert_eq!(apply(&next, &patch), Err(WebError::StaleRevision { expected: 1, found: 0 }));
        assert_eq!(preview_tree(&doc, -1.0, &cancel).err(), Some(WebError::InvalidViewport));
        assert_eq!(preview_tree(&doc, f64::NAN, &cancel).err(), Some(WebError::InvalidViewport));
    });
}

const PAGE: &str = "<!DOCTYPE html><html><head><style>\n\
    .nav { display: block }\n\
    @media (max-width: 600px) { .nav { display: none } #menu { display: none !important } }\n\
    @media (min-width: 601px) { .wide { display: none } }\n\
    a:hover { color: red } @font-face { font-family: x } div > p { margin: 0 }\n\
    @media print { .x { display: none } }\n\
    </style></head><body>\n\
    <header><nav class=\"nav\" aria-label=\"Main\"><a href=\"/a\">Home</a><a href=\"/b\" tabindex=\"2\">Docs</a></nav></header>\n\
    <main><h1>Title <em>x</em></h1><img src=\"a.png\" alt=\"Logo\"><img src=\"b.png\"><img src=\"c.png\" alt=\"\">\n\
    <button id=\"menu\">Menu</button><button tabindex=\"-1\">Skip</button><button disabled>Off</button>\n\
    <input type=\"text\" id=\"q\"><label for=\"q\">Search</label>\n\
    <a href=\"/c\" class=\"wide\">Wide only</a><div tabindex=\"1\">Focus first</div>\n\
    <div aria-hidden=\"true\"><button>Hidden btn</button></div></main>\n\
    <footer>f</footer></body></html>";

#[test]
fn breakpoint_keyboard_a11y_preview() {
    bounded_case("breakpoint_keyboard_a11y_preview", || {
        let cancel = CancellationToken::default();
        let doc = parse(PAGE, 7, &Limits::default(), &cancel).unwrap();
        let order = |p: &hsk_studio_web::PreviewTree| -> Vec<String> {
            p.tab_order.iter().map(|&id| doc.text_content(id).trim().to_owned()).collect()
        };
        let by_tag = |p: &hsk_studio_web::PreviewTree, tag: &str| p.nodes.iter().find(|n| n.tag == tag).cloned().unwrap();

        // Narrow: the media rules hide the nav and the menu button; the wide-only link is still there.
        let narrow = preview_tree(&doc, 500.0, &cancel).unwrap();
        assert_eq!((narrow.source_revision, narrow.viewport_width_px), (7, 500.0));
        let nav = by_tag(&narrow, "nav");
        assert_eq!((nav.rendered, nav.hidden), (false, Some(HiddenReason::StyleRule)));
        assert!(narrow.nodes.iter().all(|n| n.tag != "a" || doc.text_content(n.id) == "Wide only"), "hidden nav subtree omitted");
        assert_eq!(order(&narrow), ["Focus first", "", "Wide only", "Hidden btn"]);
        let rules: Vec<_> = narrow.active_rules.iter().map(|r| (r.selector.as_str(), r.media.len(), r.matched_count)).collect();
        assert_eq!(rules, [(".nav", 0, 1), (".nav", 1, 1), ("#menu", 1, 1)]);
        assert_eq!(doc.slice(&narrow.active_rules[1].media[0]), "(max-width: 600px)");

        // Wide: the max-width rules drop out, the min-width rule hides the wide-only link; tabindex orders first.
        let wide = preview_tree(&doc, 800.0, &cancel).unwrap();
        assert_eq!(order(&wide), ["Focus first", "Docs", "Home", "Menu", "", "Hidden btn"]);
        assert_eq!(wide.active_rules.iter().map(|r| r.selector.as_str()).collect::<Vec<_>>(), [".nav", ".wide"]);
        let link = wide.nodes.iter().find(|n| n.hidden.is_some() && n.tag == "a").unwrap();
        assert_eq!(doc.text_content(link.id), "Wide only");

        // Roles, accessible names, heading level, unnamed report (wide).
        let facts = |tag: &str| {
            let n = by_tag(&wide, tag);
            (n.role.unwrap_or_default(), n.name.unwrap_or_default(), n.heading_level)
        };
        assert_eq!(facts("header"), ("banner".into(), String::new(), None));
        assert_eq!(facts("nav"), ("navigation".into(), "Main".into(), None));
        assert_eq!(facts("main").0, "main");
        assert_eq!(facts("footer").0, "contentinfo");
        assert_eq!(facts("h1"), ("heading".into(), "Title x".into(), Some(1)));
        assert_eq!(facts("input"), ("textbox".into(), "Search".into(), None));
        let imgs: Vec<_> = wide.nodes.iter().filter(|n| n.tag == "img").map(|n| (n.role.clone().unwrap(), n.name.clone())).collect();
        assert_eq!(
            imgs,
            [("img".to_owned(), Some("Logo".to_owned())), ("img".to_owned(), None), ("presentation".to_owned(), None)]
        );
        assert_eq!(wide.unnamed.len(), 1);
        assert_eq!(doc.attribute(wide.unnamed[0], "src").and_then(|a| doc.attr_value(a)).as_deref(), Some("b.png"));
        let hidden_btn = wide.nodes.iter().find(|n| n.a11y_hidden && n.tag == "button").unwrap();
        assert!(hidden_btn.focusable && hidden_btn.name.as_deref() == Some("Hidden btn"));
        let disabled = wide.nodes.iter().find(|n| doc.text_content(n.id) == "Off").unwrap();
        assert!(!disabled.focusable);

        // Everything outside the subset is reported, in both widths, never guessed.
        let reasons: Vec<String> = wide
            .unsupported
            .iter()
            .map(|u| match &u.reason {
                UnsupportedRuleReason::Rule(r) => r.code().to_owned(),
                UnsupportedRuleReason::MediaQuery(WebError::Unsupported { kind, .. }) => format!("media:{}", kind.code()),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(reasons, ["at_rule", "selector", "selector", "media:media_type"]);
        assert_eq!(preview_tree(&doc, 500.0, &cancel).unwrap().unsupported, narrow.unsupported);

        // Inline style wins over a non-important rule; `hidden` hides unless a rule sets a display.
        let page = load("<style>.k{display:block}.i{display:none !important}</style><p class=k style=\"display:none\">a</p><p hidden>b</p><p hidden class=k>c</p><p class=i style=\"display:block\">d</p><input type=hidden><p>e</p>");
        let tree = preview_tree(&page, 100.0, &cancel).unwrap();
        let seen: Vec<_> = tree.nodes.iter().map(|n| (page.text_content(n.id), n.hidden)).collect();
        assert_eq!(
            seen,
            [
                ("a".into(), Some(HiddenReason::InlineStyle)),
                ("b".into(), Some(HiddenReason::HiddenAttribute)),
                ("c".into(), None),
                ("d".into(), Some(HiddenReason::StyleRule)),
                (String::new(), Some(HiddenReason::InputHidden)),
                ("e".into(), None),
            ]
        );
    });
}

struct Frames(Vec<Vec<u8>>);
impl SinkPort for Frames {
    fn try_send(&mut self, _class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}

fn observer(revision: u64) -> Observe {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
    Observe::new(5, revision, resource, actor, Budget::new(1, 4096).unwrap())
}

#[test]
fn observe_report_maps_outcomes() {
    bounded_case("observe_report_maps_outcomes", || {
        let fresh = CancellationToken::default();
        let delivered = |result: Result<(), WebError>, token: &CancellationToken| {
            let mut sink = Frames(Vec::new());
            let receipt = report(&mut observer(3), 5, 3, &result, token, &mut sink).unwrap();
            assert_eq!(sink.0.len(), 1, "exactly one terminal frame");
            assert_eq!((receipt.correlation_id, receipt.revision), (5, 3));
            receipt.outcome
        };
        assert_eq!(delivered(Ok(()), &fresh), Outcome::Success);
        let unsupported = WebError::Unsupported { kind: UnsupportedKind::ImpliedEndTag, span: 0..1 };
        assert_eq!(delivered(Err(unsupported.clone()), &fresh), Outcome::Failure(FailureCode::Unsupported));
        assert_eq!(delivered(Err(WebError::StaleRevision { expected: 2, found: 1 }), &fresh), Outcome::Failure(FailureCode::Validation));
        assert_eq!(delivered(Err(WebError::Conflict { span: 0..0 }), &fresh), Outcome::Failure(FailureCode::Validation));
        assert_eq!(delivered(Err(WebError::Canceled), &fresh), Outcome::Canceled);
        // A cancelled token always reports Canceled, even for a result that finished.
        let stop = CancellationToken::default();
        stop.cancel();
        assert_eq!(delivered(Ok(()), &stop), Outcome::Canceled);
        // Denied assets inside an Ok result are still a Success; the list stays with the caller.
        assert_eq!(outcome_of(&Ok::<_, WebError>(vec![AssetOutcome::Denied(DenyReason::ExternalScheme)])), Outcome::Success);
        // The wrong revision is refused by observe itself, not silently re-labelled.
        let mut sink = Frames(Vec::new());
        assert!(report(&mut observer(3), 5, 4, &Ok::<(), WebError>(()), &fresh, &mut sink).is_err());
        assert!(sink.0.is_empty());
    });
}

// ---- R5 group B remediation: hostile URL spellings must reach the same classification as the plain ones ----

struct Spy {
    calls: Mutex<Vec<String>>,
}
impl AssetPort for Spy {
    fn resolve(&self, reference: &str) -> AssetDecision {
        self.calls.lock().unwrap().push(reference.to_owned());
        if reference.starts_with("logo.png") { AssetDecision::Granted { id: "asset-logo".into() } } else { AssetDecision::Denied }
    }
}

const GRANTED: &str = "Granted { id: \"asset-logo\" }";
const EXTERNAL: &str = "Denied(ExternalScheme)";
const PROTOCOL_RELATIVE: &str = "Denied(ProtocolRelative)";
const SCRIPT: &str = "Denied(ScriptScheme)";
const NOT_GRANTED: &str = "Denied(NotGranted)";

type Seen = Vec<(String, String)>;

/// Every reported (reference, outcome as Debug text) in report order, and every reference the port was asked about.
fn scan(html: &str) -> (Seen, Vec<String>) {
    let doc = load(html);
    let spy = Spy { calls: Mutex::new(Vec::new()) };
    let found = collect_assets(&doc, &spy, &CancellationToken::default()).unwrap();
    let seen = found.iter().map(|a| (a.reference.clone(), format!("{:?}", a.outcome))).collect();
    (seen, spy.calls.into_inner().unwrap())
}

fn pairs(list: &[(&str, &str)]) -> Seen {
    list.iter().map(|(r, o)| ((*r).to_owned(), (*o).to_owned())).collect()
}

#[test]
fn css_escapes_cannot_hide_external_urls() {
    bounded_case("css_escapes_cannot_hide_external_urls", || {
        // Escaped function name, escaped scheme letters, mixed case, escaped line break inside a string, hex escapes with
        // their optional trailing space, an escaped tab, and an escaped quoted-url function: a CSS engine reads all of them
        // as the plain URL, so the port must never see any of them.
        let html = r#"<div style="background:ur\6c(//e.invalid/s.png)"></div><style>
.a{background:u\72l(https://e.invalid/a.png)}
.b{background:url(ht\74p://e.invalid/b.png)}
.c{background:url(HT\54P://e.invalid/c.png)}
.d{background:url("ht\
tp://e.invalid/d.png")}
.e{background:url(\2f\2f e.invalid/e.png)}
.f{background:url(java\73 cript:void)}
.g{background:url(ht\9 tp://e.invalid/g.png)}
.h{background:\75rl("//e.invalid/h.png")}
.i{background:url(logo.png)}
</style>"#;
        let (seen, calls) = scan(html);
        assert_eq!(
            seen,
            pairs(&[
                ("//e.invalid/s.png", PROTOCOL_RELATIVE),
                ("https://e.invalid/a.png", EXTERNAL),
                ("http://e.invalid/b.png", EXTERNAL),
                ("HTTP://e.invalid/c.png", EXTERNAL),
                ("http://e.invalid/d.png", EXTERNAL),
                ("//e.invalid/e.png", PROTOCOL_RELATIVE),
                ("javascript:void", SCRIPT),
                ("ht\ttp://e.invalid/g.png", EXTERNAL),
                ("//e.invalid/h.png", PROTOCOL_RELATIVE),
                ("logo.png", GRANTED),
            ])
        );
        assert_eq!(calls, ["logo.png"]);
    });
}

#[test]
fn css_decoys_case_and_whitespace_follow_the_tokenizer() {
    bounded_case("css_decoys_case_and_whitespace_follow_the_tokenizer", || {
        // Decoys that look like url( but are not url tokens (comment, string, `url/**/(`, `url (`, `xurl(`, `1url(`)
        // are not reported; real tokens survive case changes, inner whitespace and comments around @import; a bad-url
        // token (whitespace then more text) is reported but never offered to the port.
        let html = "<style>\n\
            /* url(http://decoy.invalid/c1) */\n\
            .k{content:\"url(http://decoy.invalid/s1)\"}\n\
            .l{background:url/**/(http://decoy.invalid/l)}\n\
            .m{background:url (http://decoy.invalid/m)}\n\
            .n{background:xurl(http://decoy.invalid/n)}\n\
            .o{background:1url(http://decoy.invalid/o)}\n\
            .p{background:UrL(\n   HtTp://e.invalid/p.png\t)}\n\
            .q{background:url(   \"//e.invalid/q.png\"   )}\n\
            .r{background:URL( logo.png )}\n\
            @IMPORT \"//e.invalid/r.css\";\n\
            @import/**/'https://e.invalid/s.css';\n\
            @import url(  //e.invalid/t.css  );\n\
            .s{background:url(logo.png x)}\n\
            </style>";
        let (seen, calls) = scan(html);
        assert_eq!(
            seen,
            pairs(&[
                ("HtTp://e.invalid/p.png", EXTERNAL),
                ("//e.invalid/q.png", PROTOCOL_RELATIVE),
                ("logo.png", GRANTED),
                ("//e.invalid/r.css", PROTOCOL_RELATIVE),
                ("https://e.invalid/s.css", EXTERNAL),
                ("//e.invalid/t.css", PROTOCOL_RELATIVE),
                ("logo.png", NOT_GRANTED),
            ])
        );
        assert_eq!(calls, ["logo.png"], "only the well-formed relative url token reaches the port");
    });
}

#[test]
fn character_references_cannot_hide_schemes() {
    bounded_case("character_references_cannot_hide_schemes", || {
        // Named references outside the old common set, numeric references without a semicolon or with many leading zeros,
        // and a style attribute whose parentheses are references: a browser decodes all of them before URL parsing.
        let html = "<a href=\"http&colon;//e.invalid/1\"></a>\
            <img src=\"java&#115cript:alert(1)\">\
            <img src=\"&#x0000000068;ttp://e.invalid/3\">\
            <img src=\"&#104ttp://e.invalid/4\">\
            <img src=\"/&sol;e.invalid/5\">\
            <img src=\"ht&Tab;tp://e.invalid/6\">\
            <img src=\"&notarealname;logo.png\">\
            <img src=\"logo.png?x=1&y=2&copy=3\">\
            <div style=\"background:url&lpar;//e.invalid/7&rpar;\"></div>";
        let (seen, calls) = scan(html);
        assert_eq!(
            seen,
            pairs(&[
                ("http://e.invalid/1", EXTERNAL),
                ("javascript:alert(1)", SCRIPT),
                ("http://e.invalid/3", EXTERNAL),
                ("http://e.invalid/4", EXTERNAL),
                ("//e.invalid/5", PROTOCOL_RELATIVE),
                ("ht\ttp://e.invalid/6", EXTERNAL),
                ("&notarealname;logo.png", NOT_GRANTED),
                ("logo.png?x=1&y=2&copy=3", GRANTED),
                ("//e.invalid/7", PROTOCOL_RELATIVE),
            ])
        );
        assert_eq!(
            calls,
            ["&notarealname;logo.png", "logo.png?x=1&y=2&copy=3"],
            "unknown names and legacy names before an alphanumeric or `=` stay literal in attributes, as in a browser"
        );
    });
}

#[test]
fn base_href_re_targets_every_relative_reference() {
    bounded_case("base_href_re_targets_every_relative_reference", || {
        const BASE: &str = "Denied(BaseHref)";
        // External base: reported as denied, and no relative reference (any element, any position) is offered to the port.
        let (seen, calls) = scan(
            "<head><base href=\"https://evil.invalid/\"></head><body><img src=\"logo.png\"><a href=\"page.html\">x</a>\
             <img src=\"http://e.invalid/x.png\"><div style=\"background:url(bg.png)\"></div></body>",
        );
        assert_eq!(
            seen,
            pairs(&[
                ("https://evil.invalid/", EXTERNAL),
                ("logo.png", BASE),
                ("page.html", BASE),
                ("http://e.invalid/x.png", EXTERNAL),
                ("bg.png", BASE),
            ])
        );
        assert!(calls.is_empty());
        // A relative base still moves every relative reference, also when it appears after them; a valueless one too.
        let (seen, calls) = scan("<img src=\"logo.png\"><base href=\"sub/\">");
        assert_eq!(seen, pairs(&[("logo.png", BASE), ("sub/", BASE)]));
        assert!(calls.is_empty());
        let (seen, calls) = scan("<base href><img src=\"logo.png\">");
        assert_eq!(seen, pairs(&[("logo.png", BASE)]));
        assert!(calls.is_empty());
        // Without an href the base changes nothing.
        let (seen, calls) = scan("<img src=\"logo.png\"><base target=\"_blank\">");
        assert_eq!(seen, pairs(&[("logo.png", GRANTED)]));
        assert_eq!(calls, ["logo.png"]);
    });
}

#[test]
fn more_url_attributes_and_foreign_content_are_scanned() {
    bounded_case("more_url_attributes_and_foreign_content_are_scanned", || {
        let html = r#"<!-- <img src="http://decoy.invalid/comment"> -->
<img alt="http://decoy.invalid/alt" title="url(http://decoy.invalid/title)" srcset="logo.png 1x, http://e.invalid/a.png 2x, //e.invalid/b.png 480w">
<picture><source srcset="https://e.invalid/c.png"></picture>
<video poster="//e.invalid/d.png" src="logo.png"></video>
<form action="http://e.invalid/post"><button formaction="//e.invalid/f">go</button></form>
<table background="http://e.invalid/g.gif"></table>
<a href="x" ping="http://e.invalid/p logo.png">p</a>
<object data="http://e.invalid/o"></object>
<link rel=preload as=image imagesrcset="https://e.invalid/h.png 1x">
<iframe srcdoc="&lt;img src=//e.invalid/z&gt;"></iframe>
<meta http-equiv="&#82efresh" content="5; URL = 'http://e.invalid/r'">
<svg width="1" height="1"><image href="http://e.invalid/s1.png"/><use xlink:href="//e.invalid/s2.svg#a"/><a XLINK:HREF="https://e.invalid/s3"><rect fill="url(http://e.invalid/s4#p)" stroke="url(#local)" filter="u\72l(//e.invalid/s5)"/></a><style>@import url(//e.invalid/s6.css); .a{fill:url(logo.png)}</style><style><![CDATA[.b{fill:url(http://e.invalid/s7)}]]></style></svg>"#;
        let (seen, calls) = scan(html);
        assert_eq!(
            seen,
            pairs(&[
                ("logo.png", GRANTED),
                ("http://e.invalid/a.png", EXTERNAL),
                ("//e.invalid/b.png", PROTOCOL_RELATIVE),
                ("https://e.invalid/c.png", EXTERNAL),
                ("//e.invalid/d.png", PROTOCOL_RELATIVE),
                ("logo.png", GRANTED),
                ("http://e.invalid/post", EXTERNAL),
                ("//e.invalid/f", PROTOCOL_RELATIVE),
                ("http://e.invalid/g.gif", EXTERNAL),
                ("x", NOT_GRANTED),
                ("http://e.invalid/p", EXTERNAL),
                ("logo.png", GRANTED),
                ("http://e.invalid/o", EXTERNAL),
                ("https://e.invalid/h.png", EXTERNAL),
                ("<img src=//e.invalid/z>", "Denied(InlineDocument)"),
                ("http://e.invalid/r", EXTERNAL),
                ("http://e.invalid/s1.png", EXTERNAL),
                ("//e.invalid/s2.svg#a", PROTOCOL_RELATIVE),
                ("https://e.invalid/s3", EXTERNAL),
                ("http://e.invalid/s4#p", EXTERNAL),
                ("#local", "SameDocument"),
                ("//e.invalid/s5", PROTOCOL_RELATIVE),
                ("//e.invalid/s6.css", PROTOCOL_RELATIVE),
                ("logo.png", GRANTED),
                ("http://e.invalid/s7", EXTERNAL),
            ])
        );
        assert_eq!(calls, ["logo.png", "logo.png", "x", "logo.png", "logo.png"]);
    });
}

#[test]
fn ambiguous_relative_paths_never_reach_the_port() {
    bounded_case("ambiguous_relative_paths_never_reach_the_port", || {
        // Backslash separators and percent-encoded dots/slashes mean different paths to a browser and to a file-system
        // port; they are refused. A plain `..` is the port's containment decision (the document location is unknown here).
        let html = "<img src=\"..\\..\\secret.png\">\
            <img src=\"a/%2e%2e/%2E%2E/b.png\">\
            <img src=\"a%2f..%2fb.png\">\
            <img src=\"a%5Cb.png\">\
            <img src=\"../ok/logo.png\">\
            <img src=\"logo.png\">\
            <img src=\"logo.png?next=..%2f..%2fx\">\
            <img src=\"sub/%20name.png\">";
        let (seen, calls) = scan(html);
        let ambiguous = "Denied(AmbiguousPath)";
        assert_eq!(
            seen,
            pairs(&[
                ("..\\..\\secret.png", ambiguous),
                ("a/%2e%2e/%2E%2E/b.png", ambiguous),
                ("a%2f..%2fb.png", ambiguous),
                ("a%5Cb.png", ambiguous),
                ("../ok/logo.png", NOT_GRANTED),
                ("logo.png", GRANTED),
                ("logo.png?next=..%2f..%2fx", GRANTED),
                ("sub/%20name.png", NOT_GRANTED),
            ])
        );
        assert_eq!(calls, ["../ok/logo.png", "logo.png", "logo.png?next=..%2f..%2fx", "sub/%20name.png"]);
    });
}

// ---- R6 group B remediation: the crate reads the bytes the way an HTML parser does, or refuses the document ----
// Each test pins the browser reading (WHATWG HTML 13.2: character reference states, tokenizer states, tree-construction
// dispatcher). Expectations use only the public API that existed before the fix, so the same file proves red on bd8533f.

/// `scan`, but a parse refusal comes back as its error code.
fn scan_or_refuse(html: &str) -> Result<(Seen, Vec<String>), String> {
    let doc = parse(html, 0, &Limits::default(), &CancellationToken::default()).map_err(|e| e.code().to_owned())?;
    let spy = Spy { calls: Mutex::new(Vec::new()) };
    let found = collect_assets(&doc, &spy, &CancellationToken::default()).unwrap();
    let seen = found.iter().map(|a| (a.reference.clone(), format!("{:?}", a.outcome))).collect();
    Ok((seen, spy.calls.into_inner().unwrap()))
}

fn refused(html: &str) -> String {
    match scan_or_refuse(html) {
        Err(code) => code,
        Ok(found) => panic!("expected a typed refusal, got {found:?}"),
    }
}

#[test]
fn legacy_named_reference_without_semicolon_in_attribute() {
    bounded_case("legacy_named_reference_without_semicolon_in_attribute", || {
        // R6 finding 1 payload: `&quot` (no `;`) followed by a space decodes to `"` in an attribute value, which closes the
        // CSS string, so the browser fetches the url(). Same with the upper-case legacy name and a newline after it.
        let html = "<p style='font-family:\"a&quot ;background-image:url(https://evil.example/b.png)'>x</p>\
            <p style='a:\"&QUOT ;b:url(//e.invalid/u)'>y</p>\
            <p style='a:\"&quot\nb:url(//e.invalid/n)'>z</p>\
            <img src=\"logo.png&quot\">";
        let (seen, calls) = scan(html);
        assert_eq!(
            seen,
            pairs(&[
                ("https://evil.example/b.png", EXTERNAL),
                ("//e.invalid/u", PROTOCOL_RELATIVE),
                ("//e.invalid/n", PROTOCOL_RELATIVE),
                ("logo.png\"", GRANTED),
            ])
        );
        assert_eq!(calls, ["logo.png\""]);
    });
}

#[test]
fn legacy_reference_attribute_exceptions_and_longest_match() {
    bounded_case("legacy_reference_attribute_exceptions_and_longest_match", || {
        // In an attribute a legacy name followed by `=` or an alphanumeric stays literal (`&quot=`, `&quotx`, `&notify`,
        // `&ampx`), so those strings close normally and the url() is fetched. The longest table name wins (`&not/` decodes,
        // `&amp;` decodes). `url(&quot//e.invalid/w&quot)` is a quoted protocol-relative url for a browser: the port must
        // never see it (before the fix it was offered to the port as a relative path).
        let html = "<p style='a:\"&quot=\";b:url(//e.invalid/eq)'>1</p>\
            <p style='a:\"&quotx\";b:url(//e.invalid/ax)'>2</p>\
            <img src=\"logo.png?a=1&notify=2&ampx=3&amp;y\">\
            <img src=\"logo.png&not/x\">\
            <div style='background:url(&quot//e.invalid/w&quot)'>3</div>";
        let (seen, calls) = scan(html);
        assert_eq!(
            seen,
            pairs(&[
                ("//e.invalid/eq", PROTOCOL_RELATIVE),
                ("//e.invalid/ax", PROTOCOL_RELATIVE),
                ("logo.png?a=1&notify=2&ampx=3&y", GRANTED),
                ("logo.png\u{ac}/x", GRANTED),
                ("//e.invalid/w", PROTOCOL_RELATIVE),
            ])
        );
        assert_eq!(calls, ["logo.png?a=1&notify=2&ampx=3&y", "logo.png\u{ac}/x"]);
    });
}

#[test]
fn legacy_references_in_foreign_style_text_follow_the_data_state() {
    bounded_case("legacy_references_in_foreign_style_text_follow_the_data_state", || {
        // Text (not attribute) context: `&quot` decodes before any character, `=` included. Decoy: `"&quot=x"` closes the
        // string early, the rest becomes an unterminated string, so that url() is not fetched and not reported. Numeric
        // references in 0x80-0x9F map through windows-1252 (`&#x80;` is the euro sign, not a control character).
        let html = "<svg><style>a{x:\"&quot }b{background:url(//e.invalid/t)}</style>\
            <style>a{x:\"&quot=x\"}b{background:url(//e.invalid/decoy)}</style>\
            <style>.d{background:url(logo.png&#x80;)}</style></svg>";
        let (seen, calls) = scan(html);
        assert_eq!(seen, pairs(&[("//e.invalid/t", PROTOCOL_RELATIVE), ("logo.png\u{20ac}", GRANTED)]));
        assert_eq!(calls, ["logo.png\u{20ac}"]);
    });
}

#[test]
fn svg_html_integration_points_read_style_as_raw_text() {
    bounded_case("svg_html_integration_points_read_style_as_raw_text", || {
        // R6 finding 2 payload, then desc and title (the other SVG HTML integration points), an HTML element inside
        // foreignObject, and an svg nested inside it again (foreign: decoded). Decoy: plain svg style is foreign text, so
        // `"&quot;"` decodes to three quotes and hides the url() from the browser.
        let html = "<svg><foreignObject><style>a{x:&quot;}b{background:url(https://evil.example/x.png)}</style></foreignObject></svg>\
            <svg><desc><style>a{x:\"&quot;\"}b{background:url(//e.invalid/d)}</style></desc></svg>\
            <svg><TITLE><style>a{x:\"&quot;\"}b{background:url(//e.invalid/t)}</style></TITLE></svg>\
            <svg><foreignobject><section><style>a{x:\"&quot;\"}b{background:url(//e.invalid/f)}</style></section></foreignobject></svg>\
            <svg><foreignObject><svg><style>a{x:\"&quot }b{background:url(//e.invalid/f2)}</style></svg></foreignObject></svg>\
            <svg><style>a{x:\"&quot;\"}b{background:url(//e.invalid/decoy)}</style></svg>";
        let (seen, calls) = scan(html);
        assert_eq!(
            seen,
            pairs(&[
                ("https://evil.example/x.png", EXTERNAL),
                ("//e.invalid/d", PROTOCOL_RELATIVE),
                ("//e.invalid/t", PROTOCOL_RELATIVE),
                ("//e.invalid/f", PROTOCOL_RELATIVE),
                ("//e.invalid/f2", PROTOCOL_RELATIVE),
            ])
        );
        assert!(calls.is_empty());
    });
}

#[test]
fn mathml_integration_points_and_nested_namespaces() {
    bounded_case("mathml_integration_points_and_nested_namespaces", || {
        // MathML text integration points (mi, mtext) and annotation-xml with an HTML encoding (any case, also spelled with
        // a character reference) read style as RAWTEXT. annotation-xml without it, mglyph under mi, and math inside svg
        // (an SVG element named math, so its mi is no integration point) stay foreign: their style text is decoded.
        let raw = |u: &str| format!("<style>a{{x:\"&quot;\"}}b{{background:url({u})}}</style>");
        let decoded = |u: &str| format!("<style>a{{x:\"&quot }}b{{background:url({u})}}</style>");
        let html = [
            format!("<math><mi>{}</mi></math>", raw("//e.invalid/mi")),
            format!("<math><mtext>{}</mtext></math>", raw("//e.invalid/mt")),
            format!("<math><annotation-xml encoding=\"TEXT/HTML\">{}</annotation-xml></math>", raw("//e.invalid/ax")),
            format!("<math><annotation-xml encoding=\"text&sol;html\">{}</annotation-xml></math>", raw("//e.invalid/ax2")),
            format!("<math><annotation-xml>{}</annotation-xml></math>", decoded("//e.invalid/an")),
            format!("<math><mi><mglyph>{}</mglyph></mi></math>", decoded("//e.invalid/mg")),
            format!("<svg><math><mi>{}</mi></math></svg>", decoded("//e.invalid/sm")),
        ]
        .concat();
        let (seen, calls) = scan(&html);
        assert_eq!(
            seen,
            pairs(&[
                ("//e.invalid/mi", PROTOCOL_RELATIVE),
                ("//e.invalid/mt", PROTOCOL_RELATIVE),
                ("//e.invalid/ax", PROTOCOL_RELATIVE),
                ("//e.invalid/ax2", PROTOCOL_RELATIVE),
                ("//e.invalid/an", PROTOCOL_RELATIVE),
                ("//e.invalid/mg", PROTOCOL_RELATIVE),
                ("//e.invalid/sm", PROTOCOL_RELATIVE),
            ])
        );
        assert!(calls.is_empty());
    });
}

#[test]
fn foreign_breakout_and_cdata_follow_the_tree_builder() {
    bounded_case("foreign_breakout_and_cdata_follow_the_tree_builder", || {
        // An HTML start tag from the breakout list pops svg/math in a browser (the later style is then HTML RAWTEXT); the
        // crate never re-nests, it refuses. `font` breaks out only with color/face/size.
        let hidden = "<svg><p>x</p><style>a{x:\"&quot;\"}b{background:url(//e.invalid/b)}</style></svg>";
        assert_eq!(refused(hidden), "foreign_breakout");
        assert_eq!(refused("<svg><font color=red>x</font></svg>"), "foreign_breakout");
        assert_eq!(refused("<math><mrow><DIV></DIV></mrow></math>"), "foreign_breakout");
        assert!(scan_or_refuse("<svg><font>x</font><source>t</source></svg>").is_ok());
        // `<![CDATA[` is a CDATA section only under a foreign current node (foreignObject itself counts); in HTML content
        // it is a bogus comment that ends at the first `>`.
        let html = "<div><![CDATA[ x > <img src=\"//e.invalid/c\"> ]]></div>\
            <svg><![CDATA[ x > <img src=\"//e.invalid/no1\"> ]]></svg>\
            <svg><foreignObject><![CDATA[ > <img src=\"//e.invalid/no2\"> ]]></foreignObject></svg>\
            <svg><foreignObject><div><![CDATA[ > <img src=\"//e.invalid/c4\"> ]]></div></foreignObject></svg>";
        let (seen, calls) = scan(html);
        assert_eq!(seen, pairs(&[("//e.invalid/c", PROTOCOL_RELATIVE), ("//e.invalid/c4", PROTOCOL_RELATIVE)]));
        assert!(calls.is_empty());
    });
}

#[test]
fn comment_script_and_noscript_states_follow_the_tokenizer() {
    bounded_case("comment_script_and_noscript_states_follow_the_tokenizer", || {
        // `--!>` ends a comment; `--!->` does not. In script data, `<!--<script>` makes the next `</script>` part of the
        // script, so the element ends inside the alt value and the image after it is real. With scripting enabled
        // `noscript` is RAWTEXT: when that reading ends before the markup reading, the document is refused.
        let html = "<p><!-- a --!><img src=\"//e.invalid/c1\"><!-- --></p>\
            <!----!><img src=\"//e.invalid/c2\">\
            <!-- --!-><img src=\"//e.invalid/no\"> -->\
            <script><!--<script></script><img alt=\"</script><img src=//e.invalid/s>\">\
            <noscript><img src=\"logo.png\"></noscript>";
        let (seen, calls) = scan(html);
        assert_eq!(
            seen,
            pairs(&[
                ("//e.invalid/c1", PROTOCOL_RELATIVE),
                ("//e.invalid/c2", PROTOCOL_RELATIVE),
                ("//e.invalid/s", PROTOCOL_RELATIVE),
                ("logo.png", GRANTED),
            ])
        );
        assert_eq!(calls, ["logo.png"]);
        assert_eq!(refused("<noscript><img alt=\"</noscript><img src=//e.invalid/n>\"></noscript>"), "scripting_dependent");
        assert_eq!(refused("<noscript><style></noscript><img src=//e.invalid/n2></style></noscript>"), "scripting_dependent");
    });
}

#[test]
fn select_and_frameset_contexts_are_refused() {
    bounded_case("select_and_frameset_contexts_are_refused", || {
        // Older "in select" parsers ignore a style start tag (its content becomes markup), current ones read RAWTEXT: two
        // readings, refused. script and textarea read the same in both and stay accepted. A frameset document ignores
        // every start tag except frame/frameset/noframes, so a frame inside "style" text is real there: refused.
        assert_eq!(refused("<select><style></select><img src=//e.invalid/sel></style></select>"), "select_content");
        assert_eq!(refused("<select><svg><foreignObject><style></style></foreignObject></svg></select>"), "select_content");
        assert!(scan_or_refuse("<select><option>a</option><script>x</script><textarea>t</textarea></select>").is_ok());
        assert_eq!(refused("<frameset><style><frame src=//e.invalid/fr></style></frameset>"), "frameset_document");
    });
}

#[test]
fn svg_animation_of_href_is_scanned() {
    bounded_case("svg_animation_of_href_is_scanned", || {
        // R6 PLAUSIBLE: set/animate write to/from/by/values into the attribute named by attributeName; an animated href
        // (or src, or a url()-bearing presentation attribute) loads those values. Other targets are not URLs.
        let html = "<svg><image href=\"logo.png\">\
            <set attributeName=\"href\" to=\"https://e.invalid/a.png\"/>\
            <animate attributeName=\"xlink:href\" values=\"logo.png; //e.invalid/b.png ;\" dur=\"1s\"/>\
            <animate attributeName=\"fill\" from=\"url(#g)\" to=\"url(http://e.invalid/f)\"/>\
            <set attributeName=\"src\" to=\"//e.invalid/src\"/>\
            <animate attributeName=\"width\" to=\"//e.invalid/not-a-url\"/>\
            </image></svg>";
        let (seen, calls) = scan(html);
        assert_eq!(
            seen,
            pairs(&[
                ("logo.png", GRANTED),
                ("https://e.invalid/a.png", EXTERNAL),
                ("logo.png", GRANTED),
                ("//e.invalid/b.png", PROTOCOL_RELATIVE),
                ("#g", "SameDocument"),
                ("http://e.invalid/f", EXTERNAL),
                ("//e.invalid/src", PROTOCOL_RELATIVE),
            ])
        );
        assert_eq!(calls, ["logo.png", "logo.png"]);
    });
}
