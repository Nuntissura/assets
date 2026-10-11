//! `data:` URL reading, one group per algorithm (Fetch data: URL processor, URL Standard preprocessing and serialization,
//! MIME Sniffing parse, Infra forgiving-base64, Encoding labels/BOM, CSS Syntax 3 3.2), each with escape, decoy and
//! case/whitespace inputs (V4-08), and the policy by load context.

use super::*;

fn header(reference: &str) -> Option<(String, Option<String>, Vec<u8>)> {
    let url = parse(reference)?;
    let body = url.body()?;
    Some((url.essence, url.charset, body))
}

fn essence(reference: &str) -> String {
    parse(reference).map(|u| u.essence).unwrap_or_default()
}

#[test]
fn scheme_fragment_and_comma_follow_the_url_parser() {
    // Leading/trailing C0 controls and spaces, tabs and newlines anywhere, any case.
    assert_eq!(
        header(" \u{1}DaTa:text/css,a{}\u{0} ").map(|h| h.2),
        Some(b"a{}".to_vec())
    );
    assert_eq!(
        header("da\nta:te\txt/c\rss,@im\nport 'x'").map(|h| (h.0, h.2)),
        Some(("text/css".to_owned(), b"@import 'x'".to_vec()))
    );
    // The fragment ends the body (and a `#` before the comma leaves no comma: failure).
    assert_eq!(
        header("data:text/css,a{b:url(x#y)}").map(|h| h.2),
        Some(b"a{b:url(x".to_vec())
    );
    assert_eq!(header("data:text/css#,x"), None);
    assert_eq!(header("data:text/css"), None);
    // Not data: (a decoy prefix or a different scheme).
    assert_eq!(header("xdata:text/css,a"), None);
    assert_eq!(header("dat:text/css,a"), None);
    // The query is part of what the processor reads: a `?` in the MIME part makes it unparsable.
    assert_eq!(essence("data:text/css?x,a"), "text/plain");
    assert_eq!(
        header("data:text/css,a?b").map(|h| h.2),
        Some(b"a?b".to_vec())
    );
}

#[test]
fn percent_decoding_and_base64() {
    // Percent escapes in the body (any case), a broken escape stays, a newline inside an escape is removed first.
    assert_eq!(
        header("data:text/css,%40import%20'x'%2").map(|h| h.2),
        Some(b"@import 'x'%2".to_vec())
    );
    assert_eq!(
        header("data:text/css,%4\n0%6f").map(|h| h.2),
        Some(b"@o".to_vec())
    );
    // base64 suffix in any case with spaces before it; whitespace inside the body; padding rules.
    for r in [
        "data:text/css;base64,QGltcG9ydA==",
        "data:text/css; BASE64 ,QGltcG9ydA==",
        "data:text/css;Base64,QGlt cG9y\ndA",
        "data:text/css;base64,QGltcG9ydA%3D%3D",
    ] {
        assert_eq!(header(r).map(|h| h.2), Some(b"@import".to_vec()), "{r:?}");
    }
    // Failures: `=` in the middle, length 1 mod 4, non-alphabet (incl. a percent-decoded byte >= 0x80).
    for r in [
        "data:text/css;base64,QG=ltcG9y",
        "data:text/css;base64,QGltc",
        "data:text/css;base64,QGlt*G9y",
        "data:text/css;base64,%C3%A9AA",
    ] {
        assert_eq!(header(r), None, "{r:?}");
    }
    // Decoys: `base64` that is not the `;base64` suffix.
    assert_eq!(
        header("data:text/cssbase64,QQ==").map(|h| (h.0, h.2)),
        Some(("text/cssbase64".to_owned(), b"QQ==".to_vec()))
    );
    assert_eq!(
        header("data:text/css;x=base64,QQ==").map(|h| h.2),
        Some(b"QQ==".to_vec())
    );
}

#[test]
fn mime_types_parse_like_mime_sniffing() {
    // Case, whitespace, parameters, quoted strings with escapes, first charset wins, invalid parameters skipped.
    let charset = |r: &str| parse(r).and_then(|u| u.charset);
    assert_eq!(essence("data:TEXT/CSS ;charset=x,a"), "text/css");
    assert_eq!(essence("data: text/css ,a"), "text/css");
    assert_eq!(essence("data:;charset=utf-16le,a"), "text/plain");
    assert_eq!(
        charset("data:;charset=utf-16le,a").as_deref(),
        Some("utf-16le")
    );
    assert_eq!(
        charset("data:text/css;charset=\"utf\\-16\\\"le\";x,a").as_deref(),
        Some("utf-16\"le")
    );
    assert_eq!(
        charset("data:text/css;charset=;charset=utf-8;charset=latin1,a").as_deref(),
        Some("utf-8")
    );
    assert_eq!(charset("data:text/css;CHARSET = utf-8,a"), None);
    // Unparsable: no slash, empty subtype, non-token code points (a raw non-ASCII code point is percent-encoded by the
    // URL serializer first, which keeps the type parsable as an unknown type).
    assert_eq!(essence("data:text,a"), "text/plain");
    assert_eq!(charset("data:text,a").as_deref(), Some("US-ASCII"));
    assert_eq!(essence("data:text/,a"), "text/plain");
    assert_eq!(essence("data:te xt/css,a"), "text/plain");
    assert_eq!(essence("data:text/c\u{e9}ss,a"), "text/c%c3%a9ss");
    assert_eq!(essence("data:,a"), "text/plain");
}

#[test]
fn css_payloads_decode_per_bom_charset_and_at_charset() {
    let text = |b: &[u8], c: Option<&str>| css_text(b, c);
    assert_eq!(text(b"a{}", None).as_deref(), Some("a{}"));
    // A BOM beats every label.
    assert_eq!(
        text(b"\xff\xfe@\0i\0", Some("windows-1252")).as_deref(),
        Some("@i")
    );
    assert_eq!(text(b"\xfe\xff\0@\0i", None).as_deref(), Some("@i"));
    assert_eq!(
        text(b"\xef\xbb\xbf@i", Some("utf-16le")).as_deref(),
        Some("@i")
    );
    // The charset parameter, any label case and surrounding whitespace.
    assert_eq!(text(b"@\0i\0", Some(" UTF-16LE ")).as_deref(), Some("@i"));
    assert_eq!(text(b"\0@\0i", Some("unicodeFFFE")).as_deref(), Some("@i"));
    assert_eq!(
        text(b"\x80\x9f\xe9", Some("us-ascii")).as_deref(),
        Some("\u{20ac}\u{178}\u{e9}")
    );
    assert_eq!(
        text(b"abc", Some("iso-2022-kr")).as_deref(),
        Some("\u{fffd}")
    );
    // Labels this crate does not decode (or does not know) refuse the payload.
    assert_eq!(text(b"a", Some("shift_jis")), None);
    assert_eq!(text(b"a", Some("x-unknown")), None);
    // @charset: exact bytes, UTF-16 read as UTF-8, within 1024 bytes; decoys with a capital or an extra space ignored.
    assert_eq!(
        text(b"@charset \"utf-16le\";a", None).as_deref(),
        Some("@charset \"utf-16le\";a")
    );
    assert_eq!(
        text(b"@charset \"latin1\";\xe9", None).as_deref(),
        Some("@charset \"latin1\";\u{e9}")
    );
    assert_eq!(text(b"@charset \"gbk\";a", None), None);
    assert_eq!(
        text(b"@CHARSET \"gbk\";a", None).as_deref(),
        Some("@CHARSET \"gbk\";a")
    );
    assert_eq!(
        text(b"@charset  \"gbk\";a", None).as_deref(),
        Some("@charset  \"gbk\";a")
    );
    let far = format!("@charset \"{}gbk\";", " ".repeat(1020));
    assert!(text(far.as_bytes(), None).is_some());
    // UTF-16 errors: a lone byte and a lone surrogate at the end are one error together.
    assert_eq!(text(b"a", Some("utf-16le")).as_deref(), Some("\u{fffd}"));
    assert_eq!(
        text(b"@\0\x00\xd8x", Some("utf-16le")).as_deref(),
        Some("@\u{fffd}")
    );
}

#[test]
fn decisions_by_type_and_load_context() {
    let d = |load: Load, r: &str| decide(load, &parse(r).expect("data URL"));
    // CSS is scanned where it is a style sheet or nothing reads the payload (not in a script or a track: see
    // `load_context_decides_before_the_type`).
    for load in [
        Load::Image,
        Load::Font,
        Load::Document,
        Load::Stylesheet,
        Load::Resource,
        Load::Media,
        Load::Beacon,
        Load::None,
        Load::Base,
    ] {
        assert_eq!(d(load, "data:Text/CSS;charset=utf-8,a"), Decision::ScanCss);
    }
    // Images decode anything as an image; beacons and identifiers load nothing.
    for r in [
        "data:text/html,<img src=x>",
        "data:image/svg+xml,<svg/>",
        "data:unknown/unknown,x",
    ] {
        assert_eq!(d(Load::Image, r), Decision::Inline);
        assert_eq!(d(Load::Beacon, r), Decision::Inline);
        assert_eq!(d(Load::None, r), Decision::Inline);
    }
    // Documents, resources, media: only inert types stay inline.
    for load in [Load::Document, Load::Resource, Load::Media] {
        for r in [
            "data:text/html,x",
            "data:application/xhtml+xml,x",
            "data:image/svg+xml,x",
            "data:IMAGE/SVG+XML;base64,PHN2Zy8+",
            "data:text/xml,x",
            "data:application/rss+xml,x",
            "data:unknown/unknown,x",
            "data:application/unknown,x",
            "data:*/*,x",
            "data:text/javascript,x",
            "data:application/json,x",
            "data:application/vnd.apple.mpegurl,x",
            "data:audio/x-mpegurl,x",
            "data:text/vtt,x",
            "data:application/pdf,x",
            "data:multipart/x-mixed-replace,x",
        ] {
            assert_eq!(d(load, r), Decision::Deny, "{load:?} {r:?}");
        }
        for r in [
            "data:image/png;base64,AAAA",
            "data:font/woff2,x",
            "data:application/x-font-woff,x",
            "data:audio/ogg,x",
            "data:video/mp4,x",
            "data:,x",
            "data:text/plain,x",
            "data:application/octet-stream,x",
            "data:te xt/html,x",
        ] {
            assert_eq!(d(load, r), Decision::Inline, "{load:?} {r:?}");
        }
    }
    // Style sheets read inert types as CSS (quirks mode) and deny the rest.
    assert_eq!(
        d(Load::Stylesheet, "data:text/plain,@import 'x'"),
        Decision::ScanCss
    );
    assert_eq!(d(Load::Stylesheet, "data:,@import 'x'"), Decision::ScanCss);
    assert_eq!(d(Load::Stylesheet, "data:image/svg+xml,x"), Decision::Deny);
    // Scripts execute everything except the types Fetch blocks for scripts.
    assert_eq!(d(Load::Script, "data:text/plain,fetch(1)"), Decision::Deny);
    assert_eq!(
        d(Load::Script, "data:application/octet-stream,x"),
        Decision::Deny
    );
    assert_eq!(d(Load::Script, "data:image/png,x"), Decision::Inline);
    assert_eq!(d(Load::Script, "data:text/csv,x"), Decision::Inline);
    // Tracks: only images, audio and video (no text type can become WebVTT there).
    assert_eq!(d(Load::Track, "data:text/plain,WEBVTT"), Decision::Deny);
    assert_eq!(d(Load::Track, "data:audio/ogg,x"), Decision::Inline);
}

/// R12-F1/P1: the load context is decided first, the type only within it. The expected decision of every (load context,
/// type) pair is written out here, from Fetch 2.10 (script-like destinations block `audio/*`, `image/*`, `video/*`,
/// `text/csv` only), the HTML Standard (a script is executed whatever its type; a track is WebVTT; a manifest is parsed
/// whatever its type; the quirks-mode style sheet reads any same-origin type as CSS) and the module docs.
#[test]
fn load_context_decides_before_the_type() {
    use Decision::{Deny, Inline, ScanCss};
    let types = [
        "text/css",
        "text/plain",
        "text/html",
        "image/png",
        "image/svg+xml",
        "text/vtt",
        "text/csv",
        "application/octet-stream",
    ];
    // (load context, decision per type in the order of `types`)
    let table: [(Load, [Decision; 8]); 13] = [
        (
            Load::Script,
            [Deny, Deny, Deny, Inline, Inline, Deny, Inline, Deny],
        ),
        (
            Load::Track,
            [Deny, Deny, Deny, Inline, Deny, Deny, Deny, Deny],
        ),
        (
            Load::Manifest,
            [Deny, Deny, Deny, Deny, Deny, Deny, Deny, Deny],
        ),
        (
            Load::Image,
            [
                ScanCss, Inline, Inline, Inline, Inline, Inline, Inline, Inline,
            ],
        ),
        (
            Load::Font,
            [
                ScanCss, Inline, Inline, Inline, Inline, Inline, Inline, Inline,
            ],
        ),
        (
            Load::Beacon,
            [
                ScanCss, Inline, Inline, Inline, Inline, Inline, Inline, Inline,
            ],
        ),
        (
            Load::None,
            [
                ScanCss, Inline, Inline, Inline, Inline, Inline, Inline, Inline,
            ],
        ),
        (
            Load::Base,
            [
                ScanCss, Inline, Inline, Inline, Inline, Inline, Inline, Inline,
            ],
        ),
        (
            Load::Stylesheet,
            [ScanCss, ScanCss, Deny, ScanCss, Deny, Deny, Deny, ScanCss],
        ),
        (
            Load::Document,
            [ScanCss, Inline, Deny, Inline, Deny, Deny, Deny, Inline],
        ),
        (
            Load::Resource,
            [ScanCss, Inline, Deny, Inline, Deny, Deny, Deny, Inline],
        ),
        (
            Load::Media,
            [ScanCss, Inline, Deny, Inline, Deny, Deny, Deny, Inline],
        ),
        (
            Load::Inline,
            [ScanCss, Inline, Deny, Inline, Deny, Deny, Deny, Inline],
        ),
    ];
    assert_eq!(table.len(), Load::ALL.len(), "every load context has a row");
    for (load, expected) in table {
        for (ty, want) in types.iter().zip(expected) {
            // The type spelled with case, parameters and whitespace decides the same.
            for spelled in [
                format!("data:{ty},x"),
                format!("DATA:{};Charset=utf-8,x", ty.to_ascii_uppercase()),
                format!(" \tdata: {ty} ;x=y,x"),
            ] {
                assert_eq!(
                    decide(load, &parse(&spelled).expect("data URL")),
                    want,
                    "{load:?} {spelled:?}"
                );
            }
        }
    }
    // Several contexts: the strictest decision wins (Inline < ScanCss < Deny).
    let d = |loads: Loads, r: &str| decide_all(loads, &parse(r).expect("data URL"));
    let icon_and_sheet = Loads::of(Load::Image).and(Load::Stylesheet);
    assert_eq!(d(icon_and_sheet, "data:image/png,x"), ScanCss);
    assert_eq!(d(icon_and_sheet, "data:image/svg+xml,x"), Deny);
    let script_and_sheet = Loads::of(Load::Script).and(Load::Stylesheet);
    assert_eq!(d(script_and_sheet, "data:text/css,x"), Deny);
    assert_eq!(d(script_and_sheet, "data:image/png,x"), ScanCss);
    assert_eq!(
        d(Loads::of(Load::Image).and(Load::Font), "data:text/css,x"),
        ScanCss
    );
    assert_eq!(d(Loads::of(Load::Image), "data:text/html,x"), Inline);
    // No context at all is read as a script, the strictest single reading.
    assert_eq!(d(Loads::EMPTY, "data:text/css,x"), Deny);
    assert_eq!(d(Loads::EMPTY, "data:image/png,x"), Inline);
}

/// V4-02: a MIME part longer than `MAX_HEADER_BYTES` is not copied or parsed; it is an unknown type (never CSS, never
/// inert), so only image, beacon and identifier carriers keep it inline. Tabs and newlines do not count (the URL parser
/// removes them), multi-byte code points count by their UTF-8 length.
#[test]
fn oversized_headers_are_an_unknown_type() {
    let at = |n: usize, pad: &str| format!("data:text/css;x={},@import 'y'", pad.repeat(n));
    let fits = at(MAX_HEADER_BYTES - "text/css;x=".len(), "a");
    let over = at(MAX_HEADER_BYTES - "text/css;x=".len() + 1, "a");
    assert_eq!(essence(&fits), "text/css");
    assert_eq!(essence(&over), "");
    // Tabs inside do not count; a two-byte code point counts twice.
    assert_eq!(
        essence(&format!(
            "data:text/css;x={}\t\t,a",
            "a".repeat(MAX_HEADER_BYTES - 11)
        )),
        "text/css"
    );
    assert_eq!(
        essence(&format!("data:text/css;x={},a", "\u{e9}".repeat(2048))),
        ""
    );
    for load in [
        Load::Stylesheet,
        Load::Document,
        Load::Resource,
        Load::Script,
    ] {
        assert_eq!(
            decide(load, &parse(&over).unwrap()),
            Decision::Deny,
            "{load:?}"
        );
    }
    for load in [Load::Image, Load::Beacon, Load::None] {
        assert_eq!(
            decide(load, &parse(&over).unwrap()),
            Decision::Inline,
            "{load:?}"
        );
    }
}
