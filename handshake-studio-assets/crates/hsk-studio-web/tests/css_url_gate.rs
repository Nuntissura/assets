//! The asset gate sees every URL a browser loads from CSS (lane SMB-WEB-PARITY, rem-web-r10; lead decision
//! 2026-10-11T15:25:17Z after R10 pre-review R10-F1 / R10-P1). CSS text is tokenized per CSS Syntax Level 3 (W3C CRD
//! 2026-10-01, https://www.w3.org/TR/css-syntax-3/ 4.3) before any URL is classified; URL strings of `url()`, `src()`
//! (CSS Values 4 4.5), `image-set()`, `-webkit-image-set()`, `image()` (CSS Images 4 2.4, 2.5, Appendix A) and `@import`
//! are reported; a URL computed at style time inside those functions is denied.
//!
//! Every payload runs through each carrier a browser hands to CSS: a `style` attribute, an HTML `style` body, an SVG
//! `style` body (character references decoded) and an SVG presentation attribute. V4-08: each group has escape,
//! comment/decoy and case/whitespace inputs. The assertions use only API that existed before the fix (outcomes compared
//! as `Debug` text), so this file is also the red proof on the crate at 62d8983.

use hsk_studio_accord::CancellationToken;
use hsk_studio_web::{AssetDecision, AssetPort, Limits, collect_assets, parse};
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

/// Answers every relative reference `Unavailable` and records it.
struct Port {
    calls: Mutex<Vec<String>>,
}
impl AssetPort for Port {
    fn resolve(&self, reference: &str) -> AssetDecision {
        self.calls.lock().unwrap().push(reference.to_owned());
        AssetDecision::Unavailable
    }
}

const EXTERNAL: &str = "Denied(ExternalScheme)";
const COMPUTED: &str = "Denied(ComputedUrl)";
const NOT_GRANTED: &str = "Denied(NotGranted)";
const EVIL: &str = "https://evil.example/p.png";

/// (reference, outcome as `Debug` text) for every reference, and the references the port was asked about; `Err(code)`
/// when the scan is refused typed.
type Seen = (Vec<(String, String)>, Vec<String>);

fn scan(src: &str) -> Result<Seen, String> {
    let doc = parse(src, 0, &Limits::default(), &CancellationToken::default())
        .unwrap_or_else(|e| panic!("{src:?} parses: {}", e.code()));
    let port = Port {
        calls: Mutex::new(Vec::new()),
    };
    let found = collect_assets(&doc, &port, &CancellationToken::default())
        .map_err(|e| e.code().to_owned())?;
    let seen = found
        .iter()
        .map(|a| (a.reference.clone(), format!("{:?}", a.outcome)))
        .collect();
    Ok((seen, port.calls.into_inner().unwrap()))
}

fn attr_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;")
}

fn foreign_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;")
}

/// The declaration list `decls` in a `style` attribute, an HTML `style` rule, an SVG `style` rule; the value `value` in
/// SVG presentation attributes.
fn carriers(decls: &str, value: &str) -> Vec<String> {
    vec![
        format!("<div style=\"{}\"></div>", attr_text(decls)),
        format!("<style>.x{{{decls}}}</style>"),
        format!("<svg><style>.x{{{}}}</style></svg>", foreign_text(decls)),
        format!("<svg><rect fill=\"{}\"/></svg>", attr_text(value)),
        format!("<svg><g mask=\"{}\"></g></svg>", attr_text(value)),
    ]
}

/// `url` is reported with `outcome` in every carrier of `decoy` followed by `real` (and the port never hears of it).
fn reports_after(decoy: &str, real: &str, url: &str, outcome: &str) {
    let decls = format!("a:{decoy};background:{real}");
    let value = format!("{decoy} {real}");
    for src in carriers(&decls, &value) {
        let (seen, calls) = scan(&src).unwrap();
        assert!(
            seen.contains(&(url.to_owned(), outcome.to_owned())),
            "{src:?}: {url} not reported as {outcome}; seen {seen:?}"
        );
        assert!(
            !calls.iter().any(|c| c == url),
            "{src:?}: port asked about {url}"
        );
    }
}

/// Nothing is reported in any carrier (a browser loads nothing either).
fn reports_nothing(decls: &str, value: &str) {
    for src in carriers(decls, value) {
        let (seen, _) = scan(&src).unwrap();
        assert!(seen.is_empty(), "{src:?}: {seen:?}");
    }
}

/// R10-F1 (CSS Syntax 3 4.3.13 consume a number): a `.` or an exponent joins a number only before a digit, so `1.url(`
/// is a number, a delim and a url token; the `/*` inside it is url text (4.3.6), not a comment that swallows the rest.
#[test]
fn r10_f1_number_and_dot_cannot_hide_a_later_url() {
    bounded_case("r10_f1_number_and_dot_cannot_hide_a_later_url", || {
        let real = format!("url({EVIL})");
        for decoy in [
            "1.url(/*)",
            "+1.url(/*)",
            "-1.url(/*)",
            ".5.url(/*)",
            "+.5.url(/*)",
            "1.5e-1.url(/*)",
            "1E+2.url(/*)",
            "@1.url(/*)",
            // escape: `\75` is `u` after the delim
            "1.\\75rl(/*)",
            "1.\\75 rl(/*)",
            "1.u\\72l(/*)",
            // case and whitespace inside the url token
            "1.URL(  /*  )",
            "1.UrL(\t/*)",
            // decoy: an unterminated string inside a quoted url( ends at the newline as a bad string
            "1.url(\"/*\n",
        ] {
            reports_after(decoy, &real, EVIL, EXTERNAL);
        }
        // A unit right after the number is a dimension token (`1url`) and `(` opens a block whose comment runs to the end:
        // a browser reads no url token there and loads nothing, so nothing is reported.
        for decoy in ["1url(/*)", "1\\75rl(/*)", ".5URL(/*)"] {
            reports_nothing(
                &format!("a:{decoy};background:{real}"),
                &format!("{decoy} {real}"),
            );
        }
    });
}

/// CSS Syntax 3 4.3.1: `<!--` is a CDO token and `-->` a CDC token (not the start of an `--url` identifier), so the
/// `url(` after them is a url token (same class as R10-F1: a token read differently from the browser hid a later URL).
#[test]
fn cdo_and_cdc_are_tokens_not_identifiers() {
    bounded_case("cdo_and_cdc_are_tokens_not_identifiers", || {
        let real = format!("url({EVIL})");
        for decoy in [
            "<!--url(/*)-->",
            "<!--URL( /* )-->",
            "<!--\\75rl(/*)",
            "<!--u\\rl(/*)",
            "-->url(/*)",
            "<!---->url(/*)",
        ] {
            reports_after(decoy, &real, EVIL, EXTERNAL);
        }
        // `<!-` is no CDO: `-u` starts the identifier `-url`, a function, and the comment after it hides the rest.
        reports_nothing(
            &format!("a:<!-url(/*);b:{real}"),
            &format!("<!-url(/*) {real}"),
        );
    });
}

/// R10-P1: strings that a browser loads as URLs (CSS Values 4 4.5 `url( <string> )`, `src( <string> )`; CSS Images 4 2.4
/// image-set strings, 2.5 image() strings, Appendix A `-webkit-image-set()`) and `@import` strings.
#[test]
fn url_strings_of_image_set_src_image_and_import_are_scanned() {
    bounded_case(
        "url_strings_of_image_set_src_image_and_import_are_scanned",
        || {
            for real in [
                format!("image-set(\"{EVIL}\" 1x)"),
                format!("-webkit-image-set('{EVIL}' 1x, 'b.png' 2x)"),
                format!("image('{EVIL}', red)"),
                format!("src(\"{EVIL}\")"),
                format!("cross-fade('{EVIL}' 50%, red)"),
                format!("image(image-set('{EVIL}' 1x))"),
                // escape: escaped function name and escaped scheme letters
                "\\69mage-set(\"h\\74tps://evil.example/p.png\" 1x)".to_owned(),
                "image-set('\\68 ttps://evil.example/p.png' 1x)".to_owned(),
                // case and whitespace
                format!("IMAGE-SET(  \"{EVIL}\"  1X  )"),
                format!("-WebKit-Image-Set(\n'{EVIL}'\t1x)"),
                format!("SRC( '{EVIL}' )"),
                // decoys around it: a comment, a type() string and a gradient
                format!(
                    "image-set(/* 'x.png' */ linear-gradient(red, blue) 1x, \"{EVIL}\" type(\"image/png\") 2x)"
                ),
            ] {
                reports_after("1.url(/*)", &real, EVIL, EXTERNAL);
                reports_after("x", &real, EVIL, EXTERNAL);
            }
            // @import strings in a style sheet: after whitespace or a comment, any case, an escaped name.
            for import in [
                format!("@import \"{EVIL}\";"),
                format!("@IMPORT/**/'{EVIL}';"),
                format!("@\\69mport\n\"{EVIL}\" screen;"),
                format!("@import url(\"{EVIL}\") layer(x);"),
                format!("<!--@import '{EVIL}';-->"),
            ] {
                for src in [
                    format!("<style>{import}</style>"),
                    format!("<svg><style>{}</style></svg>", foreign_text(&import)),
                ] {
                    let (seen, calls) = scan(&src).unwrap();
                    assert_eq!(seen, [(EVIL.to_owned(), EXTERNAL.to_owned())], "{src:?}");
                    assert!(calls.is_empty(), "{src:?}");
                }
            }
            // Strings that are not URLs stay unreported: content text, a font name, a type() MIME type, a format().
            let (seen, calls) = scan(
                "<style>.a{content:\"https://e.invalid/c\";font-family:'x.png'}\
                 @font-face{src:url(f.woff) format('woff'),local('https://e.invalid/l')}\
                 .b{background:image-set('b.png' type('https://e.invalid/t'))}</style>",
            )
            .unwrap();
            assert_eq!(
                seen,
                [
                    ("f.woff".to_owned(), "Unavailable".to_owned()),
                    ("b.png".to_owned(), "Unavailable".to_owned())
                ]
            );
            assert_eq!(calls, ["f.woff", "b.png"]);
        },
    );
}

/// A URL a browser computes at style time inside a URL function (`src(var(--u))`, CSS Values 4 4.5; arbitrary
/// substitution functions, CSS Values 5) is not in the source: the gate denies it (`ComputedUrl`) instead of skipping it.
#[test]
fn computed_urls_inside_url_functions_are_denied() {
    bounded_case("computed_urls_inside_url_functions_are_denied", || {
        for (real, function) in [
            ("image-set(var(--u) 1x)", "var(--u)"),
            ("src(var(--u))", "var(--u)"),
            ("image(attr(data-u))", "attr(data-u)"),
            ("-webkit-image-set(env(x) 1x)", "env(x)"),
            ("image-set(--f('a.png') 1x)", "--f('a.png')"),
            ("url('a.png' if(x))", "if(x)"),
            // escape and case
            ("image-set(v\\61r(--u) 1x)", "v\\61r(--u)"),
            ("SRC(VAR(--u))", "VAR(--u)"),
            // a comment and a gradient before it
            (
                "image-set(/* x */ linear-gradient(red, blue) 1x, var(--u) 2x)",
                "var(--u)",
            ),
        ] {
            reports_after("1.url(/*)", real, function, COMPUTED);
            reports_after("x", real, function, COMPUTED);
        }
        // var() outside a URL function, and value functions inside one, compute no URL.
        reports_nothing(
            "a:var(--x);b:image-set(linear-gradient(red, blue) 1x, rgb(0 0 0) calc(1x + 1x))",
            "var(--x) image-set(linear-gradient(red, blue) 1x)",
        );
    });
}

/// url tokens after the hostile prefixes: escapes inside the URL are decoded, `/*` and quotes are url text or end it as a
/// bad url (whitespace, CR from `&#13;`, FF, a quote, `(`), and the remnants end at the first unescaped `)` so the next
/// URL is read (CSS Syntax 3 4.3.6, 4.3.15, 3.3 preprocessing).
#[test]
fn url_tokens_escapes_comments_and_bad_urls_follow_the_tokenizer() {
    bounded_case(
        "url_tokens_escapes_comments_and_bad_urls_follow_the_tokenizer",
        || {
            for prefix in ["", "1.url(/*) ", "<!--url(/*) ", "x\u{a0}url(/*) "] {
                let real = format!("url({EVIL})");
                // escapes and comments inside a url token
                reports_after(prefix, "url(ht\\74ps://evil.example/p.png)", EVIL, EXTERNAL);
                reports_after(
                    prefix,
                    "url(  https://evil.example/p.png/* )",
                    "https://evil.example/p.png/*",
                    EXTERNAL,
                );
                reports_after(
                    prefix,
                    "URL(\\68\nttps://evil.example/p.png)",
                    EVIL,
                    EXTERNAL,
                );
                // bad urls: reported denied, never offered to the port, and the next URL is read
                for bad in [
                    "url(a b)",
                    "url(a\u{c}b)",
                    "url(a\rb)",
                    "url(a\r\nb)",
                    "url(a'b)",
                    "url(a\"b)",
                    "url(a(b)",
                    "url(a b\\) c)",
                ] {
                    let decoy = format!("{prefix}{bad}");
                    let decls = format!("a:{decoy};background:{real}");
                    let value = format!("{decoy} {real}");
                    for src in carriers(&decls, &value) {
                        let (seen, calls) = scan(&src).unwrap();
                        assert!(
                            seen.contains(&("a".to_owned(), NOT_GRANTED.to_owned()))
                                && seen.contains(&(EVIL.to_owned(), EXTERNAL.to_owned())),
                            "{src:?}: {seen:?}"
                        );
                        assert!(!calls.iter().any(|c| c == "a"), "{src:?}: {calls:?}");
                    }
                }
            }
        },
    );
}

/// CSS Syntax 3 4.2 "non-ASCII ident code point" names ranges; the engines treat every code point >= U+0080 as a name
/// code point. The gate scans both readings, so a URL either one loads is reported.
#[test]
fn both_non_ascii_ident_readings_are_scanned() {
    bounded_case("both_non_ascii_ident_readings_are_scanned", || {
        let real = format!("url({EVIL})");
        for decoy in [
            "x\u{a0}url(/*)",
            "x\u{80}url(/*)",
            "x\u{200b}URL( /* )",
            "#\u{a0}url(/*)",
            "x\u{a0}\\75rl(/*)",
        ] {
            reports_after(decoy, &real, EVIL, EXTERNAL);
        }
    });
}

/// Gecko's reading of bad-url remnants after whitespace (Servo cssparser 0.38 `consume_url_end` consumes the code point
/// after the whitespace before `consume_bad_url`): in Firefox `url(x \)` ends at that `)`, so a later url() loads, while
/// CSS Syntax 3 4.3.6 (and Blink/WebKit) read `\)` as an escape inside the remnants. The gate scans both readings.
#[test]
fn gecko_bad_url_reading_is_scanned() {
    bounded_case("gecko_bad_url_reading_is_scanned", || {
        let real = format!("url({EVIL})");
        for decoy in [
            "url(x \\)",
            "url(x  \\)",
            "URL(x\n\\)",
            "url(x\t\\)",
            "u\\72l(x \\)",
            "1.url(x \\)",
        ] {
            reports_after(decoy, &real, EVIL, EXTERNAL);
        }
    });
}

/// Blocks nested deeper than the scan tracks refuse the whole scan typed instead of reading on without structure.
#[test]
fn css_nesting_beyond_the_limit_is_refused_typed() {
    bounded_case("css_nesting_beyond_the_limit_is_refused_typed", || {
        let deep = "(".repeat(1100);
        for src in [
            format!("<div style=\"a:{deep}url({EVIL})\"></div>"),
            format!("<style>.a{{b:{deep}}}</style>"),
            format!("<svg><rect fill=\"{}\"/></svg>", "[".repeat(1100)),
        ] {
            assert_eq!(scan(&src).err().as_deref(), Some("limit_depth"), "{src:?}");
        }
        let (seen, _) = scan(&format!(
            "<div style=\"a:{}url({EVIL})\"></div>",
            "(".repeat(1000)
        ))
        .unwrap();
        assert_eq!(seen, [(EVIL.to_owned(), EXTERNAL.to_owned())]);
    });
}

// ----- R11 (lead decision 2026-10-11T16:55:12Z): SVG animation values, every CSS property as a target, data: payloads --

/// Answers every relative reference `Unavailable` and records it; scans with `limits`.
fn scan_with(src: &str, limits: &Limits) -> Result<Seen, String> {
    let doc = parse(src, 0, limits, &CancellationToken::default())
        .unwrap_or_else(|e| panic!("{src:?} parses: {}", e.code()));
    let port = Port {
        calls: Mutex::new(Vec::new()),
    };
    let found = collect_assets(&doc, &port, &CancellationToken::default())
        .map_err(|e| e.code().to_owned())?;
    let seen = found
        .iter()
        .map(|a| (a.reference.clone(), format!("{:?}", a.outcome)))
        .collect();
    Ok((seen, port.calls.into_inner().unwrap()))
}

/// `EVIL` is reported denied and never offered to the port.
fn assert_evil(src: &str) {
    let (seen, calls) = scan(src).unwrap();
    assert!(
        seen.contains(&(EVIL.to_owned(), EXTERNAL.to_owned())),
        "{src:?}: {EVIL} not reported; seen {seen:?}"
    );
    assert!(!calls.iter().any(|c| c == EVIL), "{src:?}: port asked");
}

fn animation(element: &str, target: &str, attr: &str, value: &str) -> String {
    format!(
        "<svg><path d=\"M0 0L9 9\"><{element} attributeName=\"{}\" dur=\"1s\" {attr}=\"{}\"/></path></svg>",
        attr_text(target),
        attr_text(value)
    )
}

/// R11-F1: browsers split a SMIL `values` list on every `;` before each piece is parsed as a CSS value (Blink
/// `SVGAnimationElement::ParseValues`, Gecko `SMILParserUtils::ParseValues`), so a comment, string or bad url opened in one
/// piece ends at the `;` and the next piece's URL loads. `to`/`from`/`by` are one value.
#[test]
fn r11_f1_animation_values_are_split_like_the_engines() {
    bounded_case("r11_f1_animation_values_are_split_like_the_engines", || {
        for element in ["animate", "animateMotion", "animateTransform", "set"] {
            for target in [
                "fill",
                "mask",
                "stroke",
                "filter",
                "clip-path",
                "marker-start",
                "cursor",
                "FILL",
                " mask ",
            ] {
                for value in [
                    // comment decoy (the pre-review payload)
                    format!("none /*;url({EVIL})"),
                    // bad url and unterminated string decoys
                    format!("url(a b;url({EVIL})"),
                    format!("none \"x;url({EVIL})"),
                    format!("url('a;url({EVIL})"),
                    // escaped function name in the later piece
                    format!("none /*;\\75 rl({EVIL})"),
                    format!("none /*;u\\72l({EVIL})"),
                    // case and whitespace around the pieces
                    format!("none /* ;\n URL(  {EVIL}  ) ;"),
                    format!("none/*;\turl({EVIL})"),
                ] {
                    assert_evil(&animation(element, target, "values", &value));
                }
            }
        }
        // The browser's piece is `url(a`: "a" is reported, not "a;b".
        let (seen, calls) = scan(&animation("animate", "mask", "values", "url(a;b)")).unwrap();
        assert_eq!(seen, [("a".to_owned(), "Unavailable".to_owned())]);
        assert_eq!(calls, ["a"]);
        // A relative URL in a later piece reaches the port.
        let (_, calls) = scan(&animation(
            "animate",
            "mask",
            "values",
            "none /*;url(private/m.png)",
        ))
        .unwrap();
        assert_eq!(calls, ["private/m.png"]);
        // to/from/by are one CSS value: the comment runs to the end of it, as in the engines.
        for attr in ["to", "from", "by"] {
            let (seen, _) = scan(&animation(
                "animate",
                "mask",
                attr,
                &format!("none /*;url({EVIL})"),
            ))
            .unwrap();
            assert!(seen.is_empty(), "{attr}: {seen:?}");
            assert_evil(&animation("animate", "mask", attr, &format!("url({EVIL})")));
        }
    });
}

/// R11-F2: an animation of any CSS property in the carrier inventory reads its values as CSS (SVG Animations L2 2.6 matches
/// CSS properties first; Gecko animates the `marker` shorthand), including shorthands and image properties.
#[test]
fn r11_f2_every_css_property_is_an_animation_target() {
    bounded_case("r11_f2_every_css_property_is_an_animation_target", || {
        for target in [
            "marker",
            "MARKER",
            " marker ",
            "mask-image",
            "-webkit-mask",
            "background-image",
            "list-style",
            "border-image",
            "content",
            "offset-path",
            "style",
        ] {
            for (attr, value) in [
                ("to", format!("url({EVIL})")),
                ("from", format!("URL( '{EVIL}' )")),
                ("by", format!("\\75 rl({EVIL})")),
                ("values", format!("none /*;url({EVIL})")),
            ] {
                for element in ["set", "animate"] {
                    assert_evil(&animation(element, target, attr, &value));
                }
            }
        }
        // The pre-review payload: a relative marker document reaches the port, an external one is denied.
        let (seen, calls) = scan(&animation(
            "set",
            "marker",
            "to",
            "url(private/markers.svg#m)",
        ))
        .unwrap();
        assert_eq!(
            seen,
            [("private/markers.svg#m".to_owned(), "Unavailable".to_owned())]
        );
        assert_eq!(calls, ["private/markers.svg#m"]);
        assert_evil(&animation("set", "marker", "to", &format!("url({EVIL})")));
        // A target that is neither a URL attribute nor a CSS property of the inventory reads nothing.
        let (seen, _) = scan(&animation("set", "width", "to", &format!("url({EVIL})"))).unwrap();
        assert!(seen.is_empty(), "{seen:?}");
    });
}

fn b64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().fold(0u32, |n, b| (n << 8) | u32::from(*b)) << (8 * (3 - chunk.len()));
        for i in 0..=chunk.len() {
            out.push(char::from(A[((n >> (18 - 6 * i)) & 63) as usize]));
        }
        for _ in chunk.len()..3 {
            out.push('=');
        }
    }
    out
}

/// R11-P1 (not accepted by the lead): a `data:` style sheet's own references load, so its payload is decoded (percent,
/// base64, charset) and scanned, recursively; a relative reference inside resolves against the `data:` URL and loads
/// nothing (denied, never offered to the port).
#[test]
fn data_css_payloads_are_scanned_recursively() {
    bounded_case("data_css_payloads_are_scanned_recursively", || {
        let import = format!("@import '{EVIL}';");
        let utf16le: Vec<u8> = import.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut utf16be_bom = vec![0xfe, 0xff];
        utf16be_bom.extend(import.encode_utf16().flat_map(u16::to_be_bytes));
        for src in [
            format!("<style>@import url(\"data:text/css,@import%20'{EVIL}';\");</style>"),
            format!(
                "<link rel=\"stylesheet\" href=\"data:text/css,{}\">",
                attr_text(&import)
            ),
            format!(
                "<style>@import \"data:text/css;base64,{}\";</style>",
                b64(import.as_bytes())
            ),
            // escapes: CSS escape in the scheme, percent-escaped @import, character references in the attribute
            format!("<style>@import \"d\\61ta:text/css,%40import '{EVIL}';\";</style>"),
            format!("<link href=\"da&#10;ta:text/css,@im&#9;port '{EVIL}';\">"),
            // comment and string decoys inside the payload
            format!(
                "<style>@import 'data:text/css,/*x*/a{{b: \"/*\"}}@import \"{EVIL}\";';</style>"
            ),
            // case and whitespace
            format!("<link href=\" DATA:TEXT/CSS ;Charset=UTF-8 ,@IMPORT URL( {EVIL} );\">"),
            // a style sheet carrier reads plain text and untyped payloads as CSS (quirks mode)
            format!("<style>@import \"data:text/plain,{import}\";</style>"),
            format!("<style>@import \"data:,{import}\";</style>"),
            // text/css is scanned in every carrier: image, CSS value, presentation attribute
            format!("<img src=\"data:text/css,a{{b:url({EVIL})}}\">"),
            format!(
                "<div style=\"background:url(&quot;data:text/css,{}&quot;)\"></div>",
                attr_text(&import)
            ),
            format!(
                "<svg><rect fill=\"url(&quot;data:text/css,{}&quot;)\"/></svg>",
                attr_text(&import)
            ),
            // nested data: payloads
            format!(
                "<style>@import 'data:text/css,@import \"data:text/css,@import url({EVIL});\";';</style>"
            ),
            // UTF-16 by charset parameter and by BOM
            format!(
                "<style>@import 'data:text/css;charset=utf-16le;base64,{}';</style>",
                b64(&utf16le)
            ),
            format!(
                "<style>@import 'data:text/css;base64,{}';</style>",
                b64(&utf16be_bom)
            ),
        ] {
            assert_evil(&src);
        }
        // A relative reference inside a data: payload is denied and never reaches the port.
        let (seen, calls) =
            scan("<style>@import 'data:text/css,@import \"x.css\";';</style>").unwrap();
        assert!(
            seen.contains(&("x.css".to_owned(), NOT_GRANTED.to_owned())),
            "{seen:?}"
        );
        assert!(calls.is_empty(), "{calls:?}");
        // A payload in an encoding the crate does not decode is denied instead of being read wrongly.
        let url = format!("data:text/css;charset=shift_jis,{import}");
        let (seen, _) = scan(&format!("<style>@import \"{url}\";</style>")).unwrap();
        assert_eq!(seen, [(url, "Denied(InlineDocument)".to_owned())]);
        // A failed base64 payload loads nothing in a browser; the gate denies it.
        let (seen, _) = scan("<style>@import 'data:text/css;base64,QG=';</style>").unwrap();
        assert_eq!(
            seen,
            [(
                "data:text/css;base64,QG=".to_owned(),
                "Denied(InlineDocument)".to_owned()
            )]
        );
    });
}

/// R11-P1: a `data:` URL a browser loads as a document, SVG resource document, script or text track (HTML, XML incl. SVG,
/// scripts, unknown or sniffed types) cannot be scanned here and is denied; an image carrier decodes it as an image (SVG
/// in secure mode) and keeps it inline.
#[test]
fn data_documents_are_denied_outside_image_carriers() {
    bounded_case("data_documents_are_denied_outside_image_carriers", || {
        let payloads = [
            format!("data:text/html,<img src={EVIL}>"),
            format!(
                "DATA:Text/HTML;BASE64,{}",
                b64(format!("<img src={EVIL}>").as_bytes())
            ),
            format!("data:application/xhtml+xml,<img src='{EVIL}'/>"),
            format!("data:image/svg+xml,<svg><image href='{EVIL}'/></svg>"),
            "data:unknown/unknown,<html>".to_owned(),
            "data:text/javascript,fetch(1)".to_owned(),
        ];
        for url in &payloads {
            let a = attr_text(url);
            for src in [
                format!("<iframe src=\"{a}\"></iframe>"),
                format!("<object data=\"{a}\"></object>"),
                format!("<embed src=\"{a}\">"),
                format!("<a href=\"{a}\"></a>"),
                format!("<svg><use href=\"{a}\"/></svg>"),
                format!("<link href=\"{a}\">"),
                format!("<div style=\"mask:url(&quot;{a}&quot;)\"></div>"),
                format!("<video src=\"{a}\"></video>"),
            ] {
                let (seen, _) = scan(&src).unwrap();
                assert!(
                    seen.contains(&(url.clone(), "Denied(InlineDocument)".to_owned())),
                    "{src:?}: {seen:?}"
                );
            }
            for src in [
                format!("<img src=\"{a}\">"),
                format!("<svg><image href=\"{a}\"/></svg>"),
                format!("<video poster=\"{a}\"></video>"),
            ] {
                let (seen, _) = scan(&src).unwrap();
                assert_eq!(seen, [(url.clone(), "Inline".to_owned())], "{src:?}");
            }
        }
        // Scripts execute any type Fetch does not block for scripts; inert types stay inline in documents.
        let (seen, _) = scan("<script src=\"data:text/plain,fetch(1)\"></script>").unwrap();
        assert_eq!(seen[0].1, "Denied(InlineDocument)");
        let (seen, _) = scan(
            "<iframe src=\"data:image/png;base64,AAAA\"></iframe><iframe src=\"data:,hi\"></iframe>",
        )
        .unwrap();
        assert!(seen.iter().all(|s| s.1 == "Inline"), "{seen:?}");
    });
}

/// V4-01/02: nested `data:` payloads are bounded in depth (`MAX_DATA_NESTING` = 8) and in the aggregate encoded size of every
/// scanned payload at every depth (`Limits::max_bytes`), both checked before decoding and refused typed.
#[test]
fn data_payloads_beyond_the_bounds_are_refused_typed() {
    bounded_case("data_payloads_beyond_the_bounds_are_refused_typed", || {
        let nested = |levels: usize| {
            let mut css = format!("@import '{EVIL}';");
            for _ in 0..levels {
                css = format!("@import 'data:text/css;base64,{}';", b64(css.as_bytes()));
            }
            format!("<style>{css}</style>")
        };
        assert_evil(&nested(8));
        assert_eq!(scan(&nested(9)).err().as_deref(), Some("limit_depth"));
        assert_eq!(scan(&nested(30)).err().as_deref(), Some("limit_depth"));
        // Aggregate: three nested payloads (about 1000, 760 and 560 encoded bytes) under a 2048-byte budget.
        let mut css = format!("@import '{EVIL}';/*{}*/", "x".repeat(400));
        for _ in 0..3 {
            css = format!("@import 'data:text/css;base64,{}';", b64(css.as_bytes()));
        }
        let src = format!("<style>{css}</style>");
        let small = Limits {
            max_bytes: 2048,
            ..Limits::default()
        };
        assert!(src.len() <= small.max_bytes, "{}", src.len());
        assert_eq!(
            scan_with(&src, &small).err().as_deref(),
            Some("limit_bytes")
        );
        let (seen, _) = scan_with(&src, &Limits::default()).unwrap();
        assert!(seen.contains(&(EVIL.to_owned(), EXTERNAL.to_owned())));
    });
}
