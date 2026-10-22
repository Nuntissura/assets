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

fn map_visits() -> usize {
    MAP_VISITS.with(std::cell::Cell::get)
}

/// Zeroes both counters and disarms [`arm_cancel`].
fn reset_visits() {
    TARGET_VISITS.with(|v| v.set(0));
    MAP_VISITS.with(|v| v.set(0));
    CANCEL_AFTER.with(|armed| *armed.borrow_mut() = None);
}

/// Makes the id-map loop cancel `token` after `map` of its iterations and the target walk after `target` of its iterations
/// (0 = not at all), see `CANCEL_AFTER`.
fn arm_cancel(map: usize, target: usize, token: &CancellationToken) {
    CANCEL_AFTER.with(|armed| *armed.borrow_mut() = Some((map, target, token.clone())));
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

/// R13-F1: cancellation is checked inside the id-map loop and inside the target walk. This test cancels BEFORE the call, so it
/// proves the check at the first iteration of each loop (the map is built with a live token first, so the cancelled call can
/// only stop in the target loop); that the check recurs every 1024 iterations is
/// `animated_loads_cancellation_recurs_every_1024_iterations_in_both_loops`.
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

/// PLR-071 (R14-O2): the cancellation check recurs every 1024 iterations in BOTH loops, not only at the first. The token is
/// cancelled from inside the loop (`arm_cancel`) after the first iteration (and after the 1025th), so the next check, at
/// iteration 1024 (and 2048), is the one that stops it: exactly 1024 (and 2048) iterations ran, nothing was memoised and
/// the map of a cancelled build was not kept. A loop that checked only at its first iteration, or at another stride, ends
/// differently.
#[test]
fn animated_loads_cancellation_recurs_every_1024_iterations_in_both_loops() {
    let doc = doc_of(&shared_id_document(3000, ""));
    // id-map loop (the first call builds the map)
    for (armed_after, stops_at) in [(1, 1024), (1025, 2048)] {
        let cancel = CancellationToken::default();
        let mut c = collector(&doc, &cancel, &Port);
        reset_visits();
        arm_cancel(armed_after, 0, &cancel);
        assert_eq!(
            c.id_loads("x", "href"),
            Err(WebError::Canceled),
            "map loop cancelled after iteration {armed_after}"
        );
        assert_eq!(map_visits(), stops_at, "map iterations that ran");
        assert_eq!(visits(), 0, "the walk was never reached");
        assert!(c.ids.is_none(), "a cancelled build leaves no partial map");
    }
    // target walk (the map is built with the live token first)
    for (armed_after, stops_at) in [(1, 1024), (1025, 2048)] {
        let cancel = CancellationToken::default();
        let mut c = collector(&doc, &cancel, &Port);
        reset_visits();
        assert_eq!(c.id_loads("no-such-id", "href"), Ok(None));
        let built = map_visits();
        assert!(built > 2048, "the map loop itself was not stopped");
        arm_cancel(0, armed_after, &cancel);
        assert_eq!(
            c.id_loads("x", "href"),
            Err(WebError::Canceled),
            "walk cancelled after iteration {armed_after}"
        );
        assert_eq!(visits(), stops_at, "targets walked");
        assert_eq!(map_visits(), built, "the map was not rebuilt");
        let entry = c
            .ids
            .as_ref()
            .and_then(|ids| ids.get("x"))
            .expect("the id is in the map");
        assert!(entry.loads.is_empty(), "a cancelled walk memoises nothing");
    }
}

/// PLR-071 (R14-O2): the operation-count bound of the gate-scale case (`tests/css_url_gate.rs`
/// `r13_f1_many_animations_sharing_ids_finish_within_the_bound`: 20000 animations of one id shared by 20000 elements, within
/// the default limits), in place of a wall-clock bound. Targets walked: 2 attribute names x the sharing elements. The token
/// is armed to cancel one iteration past that bound, so a scan that does more work (the old per-animation walk is 4e8
/// iterations) ends `Canceled` within a stride instead of running for minutes.
#[test]
fn animated_loads_stays_within_the_operation_bound_at_the_gate_scale() {
    let count = 20_000;
    for (extra, targets) in [("", count), ("<script id=\"x\"/>", count + 1)] {
        let src = shared_id_document(count, extra);
        assert!(src.len() < Limits::default().max_bytes);
        let doc = doc_of(&src);
        let bound = 2 * targets;
        let cancel = CancellationToken::default();
        reset_visits();
        arm_cancel(0, bound + 1, &cancel);
        let found = collect_assets(&doc, &Port, &cancel)
            .expect("the scan stays within 2 x sharing elements target evaluations");
        assert_eq!(visits(), bound, "targets walked: 2 names x {targets}");
        assert_eq!(
            found.len(),
            2 * count,
            "each animation reports its href and its to"
        );
        assert!(cancel.check().is_ok(), "the bound was not crossed");
    }
}

/// The first `set` element of `doc`.
fn first_set(doc: &WebDoc) -> NodeId {
    (0..doc.len())
        .find(|&id| doc.local_name(id).as_deref() == Some("set"))
        .expect("the document has a set element")
}

/// PLR-071 (R14-N1): `animated_loads` hands `decide_all` a non-empty set for every shape of target, so the empty-set arm of
/// `decide_all` is not reachable from the collector. Strict (script and resource) where nothing can be found out: no
/// parent, a href that is not a fragment of this document, an id that no element carries, an attribute without a URL row;
/// the element's own contexts where it can.
#[test]
fn animated_loads_returns_a_context_for_every_target_shape() {
    let strict = Loads::of(Load::Script).and(Load::Resource);
    let loads_of = |src: &str, name: &str| {
        let doc = doc_of(src);
        let cancel = CancellationToken::default();
        let animation = first_set(&doc);
        collector(&doc, &cancel, &Port)
            .animated_loads(animation, name)
            .expect("not cancelled")
    };
    // (shape, document, attribute name, expected contexts when they are exactly known)
    let strict_shapes = [
        (
            "href is not a fragment",
            "<svg><set href=\"other.svg#y\" attributeName=\"href\" to=\"#z\"/></svg>",
            "href",
        ),
        (
            "fragment names no element",
            "<svg><set href=\"#nobody\" attributeName=\"href\" to=\"#z\"/></svg>",
            "href",
        ),
        (
            "fragment with an escape names no element",
            "<svg><g id=\"x\"/><set href=\"#%79\" attributeName=\"href\" to=\"#z\"/></svg>",
            "href",
        ),
        (
            "empty href",
            "<svg><set href=\"\" attributeName=\"href\" to=\"#z\"/></svg>",
            "href",
        ),
        (
            "attribute without a URL row on the parent",
            "<svg><rect><set attributeName=\"data-x\" to=\"#z\"/></rect></svg>",
            "data-x",
        ),
    ];
    for (shape, src, name) in strict_shapes {
        assert_eq!(loads_of(src, name), strict, "{shape}");
    }
    // shapes where the target is found or the parent decides: never empty
    let found_shapes = [
        (
            "parent without href",
            "<svg><a><set attributeName=\"href\" to=\"#z\"/></a></svg>",
            "href",
        ),
        (
            "parent is a link",
            "<svg><link rel=\"stylesheet\"><set attributeName=\"href\" to=\"#z\"/></link></svg>",
            "href",
        ),
        (
            "link without keyword",
            "<svg><link><set attributeName=\"href\" to=\"#z\"/></link></svg>",
            "href",
        ),
        (
            "fragment names a g",
            "<svg><g id=\"x\"/><set href=\"#x\" attributeName=\"href\" to=\"#z\"/></svg>",
            "href",
        ),
        (
            "fragment spelled with an escape",
            "<svg><g id=\"x\"/><set href=\"#%78\" attributeName=\"href\" to=\"#z\"/></svg>",
            "href",
        ),
        (
            "fragment names a script",
            "<svg><script id=\"x\"/><set href=\"#x\" attributeName=\"href\" to=\"#z\"/></svg>",
            "href",
        ),
        (
            "set at the root of the document",
            "<set attributeName=\"href\" to=\"#z\"></set>",
            "href",
        ),
        (
            "xlink:href",
            "<svg><a><set attributeName=\"xlink:href\" to=\"#z\"/></a></svg>",
            "xlink:href",
        ),
    ];
    for (shape, src, name) in found_shapes {
        assert!(!loads_of(src, name).is_empty(), "{shape}");
    }
    let script = loads_of(
        "<svg><script id=\"x\"/><set href=\"#x\" attributeName=\"href\" to=\"#z\"/></svg>",
        "href",
    );
    assert!(
        script.contains(Load::Script),
        "a script target is a script load: {script:?}"
    );
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
