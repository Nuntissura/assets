//! Unit tests of the asset gate's internals: the animated-target walk (R13-F1: linear work and cancellation inside its loops),
//! the document-path comparison behind `AssetPort::document_url` (R13-P2), blank `values` pieces (R13-P1) and the
//! URL-only refresh reading (R13-P3). The scans through the public API are in `tests/css_url_gate.rs`.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "test code sizes and indexes its own fixed inputs; the module lint is for the untrusted-text decoder"
)]

use super::*;
use crate::{Limits, parse};

struct Port;
impl AssetPort for Port {
    fn resolve(&self, _: &str, _: Loads) -> AssetDecision {
        AssetDecision::Unavailable
    }
}

fn doc_of(src: &str) -> WebDoc {
    parse(src, 0, &Limits::default(), &CancellationToken::default()).expect("parses")
}

fn collector<'a>(
    doc: &'a WebDoc,
    cancel: &'a CancellationToken,
    port: &'a dyn AssetPort,
) -> Collector<'a> {
    Collector {
        doc,
        port,
        cancel,
        base_blocks: false,
        document: None,
        limit: doc.limits().max_nodes,
        data_budget: doc.limits().max_bytes,
        out: Vec::new(),
        ids: None,
    }
}

fn visits() -> usize {
    TARGET_VISITS.with(std::cell::Cell::get)
}

fn reset_visits() {
    TARGET_VISITS.with(|v| v.set(0));
}

/// `count` elements `<g id="x"/>` followed by `count` animations of `href`/`xlink:href` naming them in four spellings.
fn shared_id_document(count: usize, extra_target: &str) -> String {
    let sets: String = (0..count)
        .map(|i| {
            let (name, href) = match i % 4 {
                0 => ("href", "#x"),
                1 => ("href", "#%78"),
                2 => ("xlink:href", "#x"),
                _ => ("xlink:href", " #%78"),
            };
            format!("<set href=\"{href}\" attributeName=\"{name}\" to=\"#y\"/>")
        })
        .collect();
    format!(
        "<svg>{extra_target}{}{sets}</svg>",
        "<g id=\"x\"/>".repeat(count)
    )
}

/// R13-F1: the elements sharing an id are walked once per animated attribute name (two names here), however many
/// animations name them and however they spell the fragment (`#x`, `#%78`, leading space): the work is linear in the
/// document, not the product of animations and shared ids (at e409030 this count is 2000 x 2000). The memoised contexts are
/// the union over all sharing elements: one `script` among the `g` elements makes every animated `href` a script load.
#[test]
fn animated_loads_walks_each_shared_id_once_per_attribute_name() {
    let count = 2000;
    let doc = doc_of(&shared_id_document(count, ""));
    reset_visits();
    let found = collect_assets(&doc, &Port, &CancellationToken::default()).expect("scans");
    assert_eq!(visits(), 2 * count, "targets walked: 2 names x {count} g");
    // every animation reports its own `href` (same document, resource context) and its `to="#y"`
    assert_eq!(found.len(), 2 * count);
    assert!(
        found
            .iter()
            .all(|a| a.outcome == AssetOutcome::SameDocument),
        "g targets are resource documents: no self reference"
    );

    let doc = doc_of(&shared_id_document(count, "<script id=\"x\"/>"));
    reset_visits();
    let found = collect_assets(&doc, &Port, &CancellationToken::default()).expect("scans");
    assert_eq!(visits(), 2 * (count + 1));
    let to: Vec<&AssetRef> = found.iter().filter(|a| a.reference == "#y").collect();
    assert_eq!(to.len(), count);
    assert!(
        to.iter()
            .all(|a| a.outcome == AssetOutcome::Denied(DenyReason::SelfReference)),
        "the script among the targets makes the animated href a script load"
    );
}

/// R13-F1: cancellation is checked inside the id-map loop and inside the target walk, every 1024 elements. The map is built
/// with a live token first, so the cancelled call can only stop in the target loop.
#[test]
fn animated_loads_checks_cancellation_inside_its_loops() {
    let doc = doc_of(&shared_id_document(3000, ""));
    // map loop
    let stopped = CancellationToken::default();
    stopped.cancel();
    let mut c = collector(&doc, &stopped, &Port);
    assert_eq!(c.id_loads("x", "href"), Err(WebError::Canceled));
    assert!(c.ids.is_none(), "a cancelled build leaves no partial map");
    // target loop
    let cancel = CancellationToken::default();
    let mut c = collector(&doc, &cancel, &Port);
    assert_eq!(c.id_loads("no-such-id", "href"), Ok(None));
    assert!(c.ids.is_some());
    cancel.cancel();
    reset_visits();
    assert_eq!(c.id_loads("x", "href"), Err(WebError::Canceled));
    assert_eq!(visits(), 0, "stopped before the first target");
    // the memo is only written by a completed walk
    let live = CancellationToken::default();
    let mut c = collector(&doc, &live, &Port);
    let first = c.id_loads("x", "href");
    assert!(matches!(first, Ok(Some(_))));
    reset_visits();
    assert_eq!(c.id_loads("x", "href"), first);
    assert_eq!(visits(), 0, "the second call is the memo");
}

/// R13-P2: which relative spellings resolve to the document, with the document at an absolute URL, at `file:` and as a
/// project path (V4-08: escape `%69`, decoy `?`/fragment/`..`, case and whitespace).
#[test]
fn document_url_comparison_resolves_relative_spellings() {
    for document in [
        "https://project.invalid/site/index.html",
        "https://project.invalid/site/index.html?v=1#top",
        "HTTPS://project.invalid:8080/site/index.html",
        "file:///site/index.html",
        "/site/index.html",
        "site/index.html",
        "site\\index.html",
        "  site/index.html ",
    ] {
        let d = DocumentUrl::of(document);
        for same in [
            "index.html",
            "./index.html",
            "INDEX.HTML",
            "Index.Html",
            "%69ndex.html",
            "index%2ehtml",
            "index.html?x",
            "index.html#frag",
            "index.html?x#frag",
            "a/../index.html",
            "./a/./../index.html",
            "../site/index.html",
            "/site/index.html",
            "/site//index.html",
            "/./site/../site/index.html",
            "?",
            "?x",
            "?x#y",
            "\tindex.html",
            "in\tdex.html",
            " index.html ",
        ] {
            // `%2e` is refused as ambiguous before the comparison is reached; the comparison itself reads it as a dot
            assert!(d.names(same), "{document:?} names {same:?}");
        }
        for other in [
            "other.html",
            "index.htm",
            "index.html.css",
            "index.htmlx",
            "xindex.html",
            "site/index.html",
            "../index.html",
            "a/index.html",
            "../../index.html",
            "/index.html",
            "/other/site/index.html",
            ".",
            "./",
            "..",
            "../",
            "dir/",
            "/",
        ] {
            assert!(!d.names(other), "{document:?} does not name {other:?}");
        }
    }
    // a directory-form document is named by its directory spellings
    let d = DocumentUrl::of("https://project.invalid/site/");
    for same in [".", "./", "../site/", "../site", "/site/", "?q"] {
        assert!(d.names(same), "{same:?}");
    }
    assert!(!d.names("index.html"));
    // a document at the root: `..` stops at the root
    let d = DocumentUrl::of("https://project.invalid/index.html");
    for same in [
        "index.html",
        "../index.html",
        "../../../index.html",
        "/index.html",
    ] {
        assert!(d.names(same), "{same:?}");
    }
    // scheme and authority are not part of the path, a colon later in a project path is
    assert_eq!(document_path("https://h.invalid"), "");
    assert_eq!(document_path("https://h.invalid/a/b?q#f"), "/a/b");
    assert_eq!(document_path("file:///a/b.html"), "/a/b.html");
    assert_eq!(document_path("a/b:c.html"), "a/b:c.html");
    assert_eq!(document_path("/a/b.html"), "/a/b.html");
}

/// R13-P1: a values piece with no character at all is dropped, a piece of nothing but HTML whitespace is kept as written
/// (with its offset), every other piece is stripped (R11-F1 split on every `;`).
#[test]
fn value_pieces_keep_blank_pieces_as_written() {
    let text = "a;\u{c};;b; ; \u{c}c\t;";
    assert_eq!(
        value_pieces(text),
        [(0, "a"), (2, "\u{c}"), (5, "b"), (7, " "), (11, "c"),]
    );
    for (at, piece) in value_pieces(text) {
        assert_eq!(text.get(at..at + piece.len()), Some(piece));
    }
    assert!(value_pieces("").is_empty());
    assert!(value_pieces(";;;").is_empty());
    assert_eq!(value_pieces("\u{c}"), [(0, "\u{c}")]);
}

/// R13-P3: the URL range of a refresh `content` that has no delay number (the HTML steps read nothing from it), and the
/// contents the legacy reading must not turn into a URL.
#[test]
fn legacy_refresh_reads_url_only_content() {
    let url = |s: &str| legacy_refresh_url(s).map(|r| s[r].to_owned());
    for (content, expected) in [
        ("https://evil.example/", "https://evil.example/"),
        ("page2.html", "page2.html"),
        ("; page2.html", "page2.html"),
        (", page2.html", "page2.html"),
        ("  ;\tpage2.html", "page2.html"),
        ("url=page2.html", "page2.html"),
        ("URL = page2.html", "page2.html"),
        ("; url='page2.html'", "page2.html"),
        ("\"page2.html\"", "page2.html"),
        ("'page2.html' tail", "page2.html"),
        ("uploads/page2.html", "uploads/page2.html"),
        ("ur", "ur"),
    ] {
        assert_eq!(url(content).as_deref(), Some(expected), "{content:?}");
    }
    // blank, separator only, or a delay: no URL for the legacy reading
    for content in [
        "",
        " ",
        "\t\n\u{c}",
        ";",
        " ; ",
        ",",
        "5",
        "5;",
        "5; url=x",
        "0.5;x",
        ".5;x",
        "5x",
        "1e3;x",
    ] {
        assert_eq!(url(content), None, "{content:?}");
    }
    // the HTML steps keep their reading
    assert_eq!(
        refresh_url("5; url=x.html")
            .map(|r| "5; url=x.html"[r].to_owned())
            .as_deref(),
        Some("x.html")
    );
    assert_eq!(refresh_url("x.html"), None);
}
