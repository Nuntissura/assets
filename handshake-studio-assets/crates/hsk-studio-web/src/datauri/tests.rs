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
    // CSS is scanned everywhere.
    for load in [
        Load::Image,
        Load::Document,
        Load::Stylesheet,
        Load::Script,
        Load::Resource,
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
