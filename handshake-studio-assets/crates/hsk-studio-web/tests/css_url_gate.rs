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
        // A bad string (or bad url) spelling a data: URL loads nothing: denied, its truncated payload never read.
        let (seen, _) = scan(&format!(
            "<style>.a{{b:url(\"data:text/css,@import '{EVIL}';\n}}</style>"
        ))
        .unwrap();
        assert_eq!(
            seen,
            [(
                format!("data:text/css,@import '{EVIL}';"),
                NOT_GRANTED.to_owned()
            )]
        );
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

// ---------------------------------------------------------------------------------------------------------------------
// R12 (rem-web-r12): the carrier's LOAD CONTEXT decides first, then the URL and the `data:` type (lead decision
// 2026-10-11T18:32:40Z after the R12 pre-review). Outcomes are compared as `Debug` text, so this part of the file is also
// the red proof on the crate at 5cd8c9e.
// ---------------------------------------------------------------------------------------------------------------------

const INLINE_DOCUMENT: &str = "Denied(InlineDocument)";
const SELF_REFERENCE: &str = "Denied(SelfReference)";
const BASE_HREF: &str = "Denied(BaseHref)";
const SAME_DOCUMENT: &str = "SameDocument";
const INLINE: &str = "Inline";
const UNAVAILABLE: &str = "Unavailable";

/// `reference` is reported with exactly `outcome` in `src`, and the port was not asked about it.
fn assert_reports(src: &str, reference: &str, outcome: &str) {
    let (seen, calls) = scan(src).unwrap_or_else(|e| panic!("{src:?}: refused {e}"));
    assert!(
        seen.contains(&(reference.to_owned(), outcome.to_owned())),
        "{src:?}: {reference:?} not reported as {outcome}; seen {seen:?}"
    );
    assert!(
        !calls.iter().any(|c| c == reference),
        "{src:?}: port asked about {reference:?}"
    );
}

/// `src` reports exactly the one reference `(reference, outcome)`, and the port heard of nothing.
fn assert_only(src: &str, reference: &str, outcome: &str) {
    let (seen, calls) = scan(src).unwrap_or_else(|e| panic!("{src:?}: refused {e}"));
    assert_eq!(
        seen,
        [(reference.to_owned(), outcome.to_owned())],
        "{src:?}"
    );
    assert!(calls.is_empty(), "{src:?}: port asked {calls:?}");
}

/// R12-F1: `<script src="data:text/css,...">` executes the payload as a classic script (HTML "fetch a classic script" does
/// no MIME type check; Fetch 2.10 blocks only audio/*, image/*, video/* and text/csv for script-like destinations), so a
/// CSS-typed payload in a script carrier is not a style sheet to scan and not `Inline`: it is denied. The same holds for an
/// SVG `script`, `modulepreload`, `preload as=script|worker` and a `link` with several keywords one of which is a script.
#[test]
fn r12_f1_data_css_in_a_script_carrier_is_a_script_load() {
    bounded_case(
        "r12_f1_data_css_in_a_script_carrier_is_a_script_load",
        || {
            let js = format!("fetch('{EVIL}')");
            let payloads = [
                format!("data:text/css,{js}"),
                format!("data:text/css;charset=utf-8,{js}"),
                // case and whitespace
                format!("DATA:Text/CSS;Charset=UTF-8,{js}"),
                format!(" \t\ndata:text/css,{js}"),
                format!("da\nta:text/c\tss,{js}"),
                // base64 and percent escapes in the body
                format!("data:text/css;base64,{}", b64(js.as_bytes())),
                format!("data:text/css,fetch%28%27{EVIL}%27%29"),
                // decoy: also valid CSS (a rule, a comment) and valid JavaScript
                format!("data:text/css,a{{b:url(x)}}//{js}"),
                format!("data:text/css,/*{js}*/{js}"),
                // other types a script executes
                format!("data:text/plain,{js}"),
                format!("data:application/octet-stream,{js}"),
                format!("data:,{js}"),
            ];
            for url in &payloads {
                let a = attr_text(url);
                for src in [
                    format!("<script src=\"{a}\"></script>"),
                    format!("<SCRIPT SRC=\"{a}\"></SCRIPT>"),
                    format!("<svg><script href=\"{a}\"/></svg>"),
                    format!("<svg><script xlink:href=\"{a}\"></script></svg>"),
                    format!("<link rel=modulepreload href=\"{a}\">"),
                    format!("<link rel=\"preload\" as=\"script\" href=\"{a}\">"),
                    format!("<link rel=\"PRELOAD\" as=\" Worker \" href=\"{a}\">"),
                    // several keywords: every one applies, the strictest decision wins
                    format!("<link rel=\"icon\tpreload\" as=\"script\" href=\"{a}\">"),
                    format!("<link rel=\"stylesheet modulepreload\" href=\"{a}\">"),
                ] {
                    assert_only(&src, url, INLINE_DOCUMENT);
                }
            }
            // Character references in the scheme and the type.
            let url = format!("data:text/css,{js}");
            for (written, decoded) in [
                (format!("&#100;ata:text/css,{js}"), url.clone()),
                (format!("&#x64;&#97;ta:text/&#99;ss,{js}"), url.clone()),
                (
                    format!("d&Tab;ata:text/css,{js}"),
                    format!("d\tata:text/css,{js}"),
                ),
            ] {
                assert_only(
                    &format!("<script src=\"{written}\"></script>"),
                    &decoded,
                    INLINE_DOCUMENT,
                );
                assert_only(
                    &format!("<svg><script href=\"{written}\"/></svg>"),
                    &decoded,
                    INLINE_DOCUMENT,
                );
            }
            // Contrast: the types Fetch blocks for scripts stay inline there, and text/css is still scanned in an image,
            // a style sheet and a navigation.
            for ty in [
                "image/png",
                "image/svg+xml",
                "audio/ogg",
                "video/mp4",
                "text/csv",
            ] {
                let url = format!("data:{ty},x");
                assert_only(&format!("<script src=\"{url}\"></script>"), &url, INLINE);
                assert_only(
                    &format!("<svg><script href=\"{url}\"/></svg>"),
                    &url,
                    INLINE,
                );
            }
            let css = format!("data:text/css,a{{b:url({EVIL})}}");
            for src in [
                format!("<img src=\"{css}\">"),
                format!("<link rel=stylesheet href=\"{css}\">"),
                format!("<iframe src=\"{css}\"></iframe>"),
                format!("<link rel=\"preload\" as=\"style\" href=\"{css}\">"),
            ] {
                let (seen, _) = scan(&src).unwrap();
                assert_eq!(
                    seen,
                    [
                        (css.clone(), INLINE.to_owned()),
                        (EVIL.to_owned(), EXTERNAL.to_owned())
                    ],
                    "{src:?}"
                );
            }
        },
    );
}

/// R12-F2: a reference to the document itself is harmless in a navigation, an image or an SVG reference, but a browser that
/// loads it as a style sheet (quirks mode assumes `text/css` for a same-origin resource), a script (no MIME type check), a
/// text track or a manifest reads the document's own text as that format and loads whatever URLs it spells. Such a
/// reference is `Denied(SelfReference)` whatever the spelling (`#`, `#x`, empty, whitespace after the URL parser's
/// stripping); everything else stays `SameDocument`.
#[test]
fn r12_f2_self_references_from_style_sheet_and_script_carriers_are_denied() {
    bounded_case(
        "r12_f2_self_references_from_style_sheet_and_script_carriers_are_denied",
        || {
            // (as written in the attribute, as the browser's URL parser receives it)
            let spellings: [(&str, &str); 13] = [
                ("#", "#"),
                ("#x", "#x"),
                ("# ", "# "),
                (" #", " #"),
                ("\t#", "\t#"),
                ("&#9;#", "\t#"),
                ("&#12;#", "\u{c}#"),
                ("&#1;#x", "\u{1}#x"),
                ("&#x20;&#x20;", "  "),
                (" ", " "),
                ("&#32;", " "),
                ("&Tab;&NewLine;", "\t\n"),
                ("", ""),
            ];
            for (written, decoded) in spellings {
                for src in [
                    format!("<link rel=stylesheet href=\"{written}\">"),
                    format!("<link rel=\"Stylesheet\" href=\"{written}\">"),
                    format!("<link rel=\"alternate stylesheet\" href=\"{written}\">"),
                    format!("<link rel=\" icon\tSTYLESHEET \" href=\"{written}\">"),
                    format!("<link rel=preload as=style href=\"{written}\">"),
                    format!("<link rel=preload as=script href=\"{written}\">"),
                    format!("<link rel=modulepreload href=\"{written}\">"),
                    format!("<link rel=manifest href=\"{written}\">"),
                    format!("<script src=\"{written}\"></script>"),
                    format!("<SCRIPT SRC=\"{written}\"></SCRIPT>"),
                    format!("<svg><script href=\"{written}\"/></svg>"),
                    format!("<svg><script xlink:href=\"{written}\"></script></svg>"),
                    format!("<video><track src=\"{written}\"></video>"),
                ] {
                    assert_only(&src, decoded, SELF_REFERENCE);
                }
            }
            // @import: strings, url(), quoted url(), CSS escapes, comments, case.
            for (css, value) in [
                ("@import \"#\";", "#"),
                ("@import '#x';", "#x"),
                ("@import url(#);", "#"),
                ("@import url( \"#\" );", "#"),
                ("@import url('#x');", "#x"),
                ("@IMPORT/**/URL(#);", "#"),
                ("@im\\70ort \"\\23\";", "#"),
                ("@import \"\\23 x\";", "#x"),
                ("@import \" \";", " "),
                ("@import \"\t\";", "\t"),
                ("@import url(\" \");", " "),
                ("@import url(\"\\9 \");", "\t"),
            ] {
                for src in [
                    format!("<style>{css}</style>"),
                    format!("<svg><style>{css}</style></svg>"),
                    format!("<style>a{{b:c}}{css}</style>"),
                ] {
                    assert_only(&src, value, SELF_REFERENCE);
                }
            }
            // Contrast: navigation, image, SVG reference and CSS value carriers keep SameDocument (and the port is
            // never asked).
            for (src, value) in [
                ("<a href=\"#\"></a>", "#"),
                ("<a href=\"#x\"></a>", "#x"),
                ("<a href=\"&#32;\"></a>", " "),
                ("<iframe src=\"#\"></iframe>", "#"),
                ("<iframe src=\"&#9;\"></iframe>", "\t"),
                ("<form action=\"#\"></form>", "#"),
                ("<img src=\"#\">", "#"),
                ("<img src=\" \">", " "),
                ("<svg><use href=\"#x\"/></svg>", "#x"),
                ("<svg><use xlink:href=\"&#32;#x\"/></svg>", " #x"),
                ("<link rel=icon href=\"#\">", "#"),
                ("<link rel=\"canonical\" href=\"#\">", "#"),
                ("<link href=\"#\">", "#"),
                ("<link rel=preload as=image href=\"#\">", "#"),
                ("<link rel=preload as=font href=\"#\">", "#"),
                ("<div style=\"background:url(#a)\"></div>", "#a"),
                ("<div style=\"mask:url(#m)\"></div>", "#m"),
                ("<svg><rect fill=\"url(#g)\"/></svg>", "#g"),
                ("<style>.a{background:url(#b)}</style>", "#b"),
                ("<meta http-equiv=refresh content=\"5;url=#\">", "#"),
            ] {
                assert_only(src, value, SAME_DOCUMENT);
            }
            // The empty string fetches nothing in a navigation or image carrier: nothing is reported.
            for src in [
                "<img src=\"\">",
                "<a href=\"\"></a>",
                "<iframe src=\"\"></iframe>",
            ] {
                let (seen, _) = scan(src).unwrap();
                assert!(seen.is_empty(), "{src:?}: {seen:?}");
            }
            // Anything else relative in those carriers still reaches the port (not a self-reference).
            let (_, calls) =
                scan("<link rel=stylesheet href=\"?x\"><script src=\"./\"></script>").unwrap();
            assert_eq!(calls, ["?x", "./"]);
        },
    );
}

/// R12-F2 for SMIL: an animation writing a self-reference into a script's `href` is a script load of the document; into a
/// `use` it is a same-document reference (SVG frame animation). The target is the element named by the animation's `href`
/// (every element with that id, also percent-decoded) or its parent; a target the gate cannot find is the strictest reading.
/// R12-P1: a `data:` URL set into a script's `href` is a script load.
#[test]
fn r12_f2_animated_urls_take_the_load_context_of_the_animated_element() {
    bounded_case(
        "r12_f2_animated_urls_take_the_load_context_of_the_animated_element",
        || {
            // parent = script
            for (attr, to) in [("href", "#"), ("xlink:href", "#x"), ("HREF", " ")] {
                assert_reports(
                    &format!(
                        "<svg><script><set attributeName=\"{attr}\" to=\"{to}\"/></script></svg>"
                    ),
                    to,
                    SELF_REFERENCE,
                );
            }
            // parent = use: frame animation stays a same-document reference
            assert_reports(
                "<svg><use href=\"#a\"><animate attributeName=\"href\" values=\"#a;#b; #c\" dur=\"1s\"/></use></svg>",
                "#b",
                SAME_DOCUMENT,
            );
            assert_reports(
                "<svg><use href=\"#a\"><set attributeName=\" xlink:href \" to=\"#z\"/></use></svg>",
                "#z",
                SAME_DOCUMENT,
            );
            // target by href: a script (also by a percent-encoded fragment)
            for (id, href) in [
                ("s", "#s"),
                ("s t", "#s%20t"),
                ("s%20t", "#s%20t"),
                ("s", "#%73"),
            ] {
                assert_reports(
                    &format!(
                        "<svg><script id=\"{id}\"></script><use id=\"u\"/><set href=\"{href}\" attributeName=\"href\" to=\"#q\"/></svg>"
                    ),
                    "#q",
                    SELF_REFERENCE,
                );
            }
            // target by href: a use
            assert_reports(
                "<svg><script id=\"s\"></script><use id=\"u\"/><set href=\"#u\" attributeName=\"href\" to=\"#q\"/></svg>",
                "#q",
                SAME_DOCUMENT,
            );
            // a target that cannot be found, or an external one, is the strictest reading: script and resource together
            for href in ["#missing", "other.svg#s", "https://evil.example/x.svg#s"] {
                assert_reports(
                    &format!(
                        "<svg><use id=\"u\"/><set href=\"{href}\" attributeName=\"href\" to=\"#q\"/></svg>"
                    ),
                    "#q",
                    SELF_REFERENCE,
                );
            }
            // R12-P1: a data: URL set into a script is a script load, into a use or an image a resource or image
            let url = "data:text/plain,fetch(1)";
            for (parent, expected) in [
                ("script", INLINE_DOCUMENT),
                ("use", INLINE),
                ("image", INLINE),
            ] {
                assert_reports(
                    &format!(
                        "<svg><{parent}><set attributeName=\"href\" to=\"{url}\"/></{parent}></svg>"
                    ),
                    url,
                    expected,
                );
            }
            assert_reports(
                "<svg><script id=\"s\"></script><set href=\"#s\" attributeName=\"xlink:href\" to=\"DATA:text/css,fetch(1)\"/></svg>",
                "DATA:text/css,fetch(1)",
                INLINE_DOCUMENT,
            );
            assert_reports(
                "<svg><use id=\"u\"/><set href=\"#u\" attributeName=\"xlink:href\" to=\"DATA:text/css,a{b:url(x)}\"/></svg>",
                "DATA:text/css,a{b:url(x)}",
                INLINE,
            );
        },
    );
}

/// R12-F3: URL values are trimmed as the URL parser does (leading and trailing C0 controls and spaces, tabs and newlines
/// removed), never as Unicode White_Space: U+00A0, U+3000, U+2028 are ordinary characters, so the value is a relative
/// reference the browser requests. The gate must not skip it.
#[test]
fn r12_f3_unicode_whitespace_is_a_relative_reference_not_nothing() {
    bounded_case(
        "r12_f3_unicode_whitespace_is_a_relative_reference_not_nothing",
        || {
            for (written, decoded) in [
                ("&nbsp;", "\u{a0}"),
                ("&#x3000;", "\u{3000}"),
                ("&#x2028;", "\u{2028}"),
                ("&#x2003;", "\u{2003}"),
                ("&ensp;&emsp;", "\u{2002}\u{2003}"),
                ("&#32;&nbsp;&#32;", " \u{a0} "),
                ("&nbsp;#", "\u{a0}#"),
            ] {
                for src in [
                    format!("<img src=\"{written}\">"),
                    format!("<a href=\"{written}\"></a>"),
                    format!("<iframe src=\"{written}\"></iframe>"),
                    format!("<link rel=stylesheet href=\"{written}\">"),
                    format!("<script src=\"{written}\"></script>"),
                    format!("<svg><script href=\"{written}\"/></svg>"),
                    format!("<svg><image href=\"{written}\"/></svg>"),
                    format!("<meta http-equiv=refresh content=\"0;url={written}\">"),
                    format!("<meta http-equiv=refresh content=\"0; URL = '{written}'\">"),
                    format!(
                        "<svg><image><set attributeName=\"href\" to=\"{written}\"/></image></svg>"
                    ),
                    format!(
                        "<svg><image><animate attributeName=\"href\" values=\"{written}\"/></image></svg>"
                    ),
                ] {
                    let (seen, calls) = scan(&src).unwrap();
                    let kept = decoded.trim_matches(|c: char| c <= ' ');
                    assert!(
                        seen.iter().any(|(r, o)| {
                            r.trim_matches(|c: char| c <= ' ') == kept && o == UNAVAILABLE
                        }),
                        "{src:?}: {decoded:?} not offered to the port; seen {seen:?}"
                    );
                    assert!(!calls.is_empty(), "{src:?}");
                }
            }
            // A base href that is only Unicode white space is a base (a relative URL): every relative reference is denied.
            let (seen, _) = scan("<base href=\"&nbsp;\"><img src=\"a.png\">").unwrap();
            assert!(
                seen.contains(&("\u{a0}".to_owned(), BASE_HREF.to_owned())),
                "{seen:?}"
            );
            assert!(
                seen.contains(&("a.png".to_owned(), BASE_HREF.to_owned())),
                "{seen:?}"
            );
            // ASCII whitespace, tabs, newlines and C0 controls are the URL parser's: the document itself.
            for written in ["&#32;", "&#9;&#10;&#13;", "&#1;&#31;"] {
                let (seen, calls) = scan(&format!("<img src=\"{written}\">")).unwrap();
                assert_eq!(seen.len(), 1, "{seen:?}");
                assert_eq!(seen[0].1, SAME_DOCUMENT);
                assert!(calls.is_empty());
            }
        },
    );
}

/// R12-P1: a text track is read as WebVTT whatever the type says, so `data:text/css` there is not a style sheet: the CSS
/// reading comments out the WebVTT `STYLE` block whose `::cue` rule loads a URL. Only images, audio and video stay inline.
#[test]
fn r12_p1_data_text_in_a_track_is_denied() {
    bounded_case("r12_p1_data_text_in_a_track_is_denied", || {
        let vtt = format!("WEBVTT /*\n\nSTYLE\n::cue{{background:url({EVIL})}}");
        let encoded = vtt.replace('\n', "%0A").replace(' ', "%20");
        for url in [
            format!("data:text/css,{encoded}"),
            format!("DATA:TEXT/CSS;charset=utf-8,{encoded}"),
            format!("data:text/css;base64,{}", b64(vtt.as_bytes())),
            format!(" \tdata:text/plain,{encoded}"),
            format!("data:text/vtt,{encoded}"),
            format!("data:,{encoded}"),
            format!("data:image/svg+xml,{encoded}"),
        ] {
            let a = attr_text(&url);
            for src in [
                format!("<video><track src=\"{a}\"></video>"),
                format!("<audio><TRACK SRC=\"{a}\"></audio>"),
                format!("<link rel=preload as=track href=\"{a}\">"),
            ] {
                assert_only(&src, &url, INLINE_DOCUMENT);
            }
        }
        for ty in ["image/png", "audio/ogg", "video/mp4"] {
            let url = format!("data:{ty},x");
            assert_only(
                &format!("<video><track src=\"{url}\"></video>"),
                &url,
                INLINE,
            );
        }
    });
}

/// R12-P2: carriers only some engines (or legacy plugins) load. Each is reported and denied or read as the conservative
/// kind of URL: MathML `mglyph src` (an image; MathML 3 3.2.1.2), `object codebase` (a base, as `base href`), `param value`
/// for the names a plugin loads (denied; relative references never reach the port), `meta http-equiv=link` (denied).
#[test]
fn r12_p2_engine_only_carriers_are_reported_or_denied() {
    bounded_case("r12_p2_engine_only_carriers_are_reported_or_denied", || {
        // mglyph src: an image URL (external denied, relative to the port, data: inline).
        for tag in ["mglyph", "MGLYPH", "mGlyph"] {
            for attr in ["src", "SRC"] {
                assert_reports(
                    &format!("<math><mi><{tag} {attr}=\"{EVIL}\"/></mi></math>"),
                    EVIL,
                    EXTERNAL,
                );
                assert_reports(
                    &format!(
                        "<math><mi><{tag} {attr}=\"&#104;ttps://evil.example/p.png\"/></mi></math>"
                    ),
                    EVIL,
                    EXTERNAL,
                );
            }
        }
        let (seen, calls) = scan("<math><mi><mglyph src=\"g.png\"/></mi></math>").unwrap();
        assert_eq!(seen, [("g.png".to_owned(), UNAVAILABLE.to_owned())]);
        assert_eq!(calls, ["g.png"]);
        assert_only(
            "<math><mi><mglyph src=\"data:text/html,<b>\"/></mi></math>",
            "data:text/html,<b>",
            INLINE,
        );
        // object codebase: a base like base href (the object's own relative data and every other relative reference are
        // denied); the base itself is reported with its typed reason.
        for attr in ["codebase", "CODEBASE", "CodeBase"] {
            let (seen, calls) = scan(&format!(
                "<object {attr}=\"x/\" data=\"a.png\"></object><img src=\"b.png\">"
            ))
            .unwrap();
            assert_eq!(
                seen,
                [
                    ("x/".to_owned(), BASE_HREF.to_owned()),
                    ("a.png".to_owned(), BASE_HREF.to_owned()),
                    ("b.png".to_owned(), BASE_HREF.to_owned()),
                ],
                "{attr}"
            );
            assert!(calls.is_empty(), "{calls:?}");
        }
        assert_reports(
            &format!("<object codebase=\" {EVIL} \"></object>"),
            &format!(" {EVIL} "),
            EXTERNAL,
        );
        assert_reports(
            &format!(
                "<object codebase=\"&#x68;ttps://evil.example/p.png\" data=\"a.png\"></object>"
            ),
            EVIL,
            EXTERNAL,
        );
        // param value: a plugin URL for these names only, denied.
        for name in [
            "src",
            "movie",
            "code",
            "url",
            "data",
            "filename",
            " Movie ",
            "SRC",
            "&#109;ovie",
        ] {
            assert_reports(
                &format!("<object><param name=\"{name}\" value=\"{EVIL}\"></object>"),
                EVIL,
                EXTERNAL,
            );
            assert_reports(
                &format!("<object><param value=\"a.swf\" name=\"{name}\"></object>"),
                "a.swf",
                NOT_GRANTED,
            );
            assert_reports(
                &format!(
                    "<object><param name=\"{name}\" value=\"DATA:text/css,a{{b:url({EVIL})}}\"></object>"
                ),
                &format!("DATA:text/css,a{{b:url({EVIL})}}"),
                INLINE_DOCUMENT,
            );
            assert_reports(
                &format!("<object><param name=\"{name}\" value=\"//evil.example/x\"></object>"),
                "//evil.example/x",
                "Denied(ProtocolRelative)",
            );
        }
        let (seen, calls) = scan(
            "<object><param name=\"movie\" value=\"a.swf\"><param name=\"quality\" value=\"high\"><param name=\"flashvars\" value=\"u=https://evil.example/x\"><param name=\"movie\" value=\"\"><param value=\"b.swf\"></object>",
        )
        .unwrap();
        assert_eq!(seen, [("a.swf".to_owned(), NOT_GRANTED.to_owned())]);
        assert!(calls.is_empty(), "{calls:?}");
        // meta http-equiv=link: a Link header value (engine-only, legacy): denied unread; refresh is untouched.
        for equiv in ["link", "LINK", " Link ", "li&#110;k"] {
            let content = format!("<{EVIL}>; rel=stylesheet");
            let src = format!("<meta http-equiv=\"{equiv}\" content=\"{}\">", content);
            assert_reports(&src, &content, INLINE_DOCUMENT);
        }
        // Not a pragma directive without http-equiv: plain text.
        let (seen, _) = scan(&format!(
            "<meta name=\"link\" content=\"<{EVIL}>; rel=stylesheet\">"
        ))
        .unwrap();
        assert!(seen.is_empty(), "{seen:?}");
    });
}

/// R12-P3: a `link` is loaded by its `rel` keywords and `as`, not always as a style sheet: an icon or a preloaded font or
/// image is not read as CSS (a binary payload of a few hundred KB would otherwise overflow the CSS nesting bound and refuse
/// the whole document), a style sheet keyword still is, and several keywords apply together (strictest wins).
#[test]
fn r12_p3_link_load_context_follows_rel_and_as() {
    bounded_case("r12_p3_link_load_context_follows_rel_and_as", || {
        // Not read as CSS: the payload (valid CSS text under another type) is neither scanned nor bounded by the CSS limits.
        let css_text = format!("a{{b:url({EVIL})}}");
        let deep = "(".repeat(3000);
        let inert = [
            ("image/png", css_text.clone()),
            ("image/png", deep.clone()),
            ("font/woff2", css_text.clone()),
            ("text/plain", css_text.clone()),
        ];
        for (ty, body) in &inert {
            let url = format!("data:{ty};base64,{}", b64(body.as_bytes()));
            for src in [
                format!("<link rel=icon href=\"{url}\">"),
                format!("<link rel=\"SHORTCUT ICON\" href=\"{url}\">"),
                format!("<link rel=\"apple-touch-icon\" href=\"{url}\">"),
                format!("<link rel=mask-icon href=\"{url}\">"),
                format!("<link rel=preload as=font href=\"{url}\">"),
                format!("<link rel=preload as=image href=\"{url}\">"),
                format!("<link rel=preload as=\"FONT\" href=\"{url}\">"),
                format!("<link rel=preconnect href=\"{url}\">"),
            ] {
                assert_only(&src, &url, INLINE);
            }
        }
        // A style sheet keyword still reads it as CSS: found, or refused typed beyond the nesting bound.
        let url = format!("data:image/png;base64,{}", b64(css_text.as_bytes()));
        for src in [
            format!("<link rel=stylesheet href=\"{url}\">"),
            format!("<link rel=\"icon stylesheet\" href=\"{url}\">"),
            format!("<link rel=\"alternate STYLESHEET\" href=\"{url}\">"),
            format!("<link rel=preload as=style href=\"{url}\">"),
        ] {
            let (seen, _) = scan(&src).unwrap();
            assert!(
                seen.contains(&(EVIL.to_owned(), EXTERNAL.to_owned())),
                "{src:?}: {seen:?}"
            );
        }
        let deep_url = format!("data:image/png;base64,{}", b64(deep.as_bytes()));
        for src in [
            format!("<link rel=stylesheet href=\"{deep_url}\">"),
            format!("<link rel=\"icon stylesheet\" href=\"{deep_url}\">"),
        ] {
            assert_eq!(scan(&src).err().as_deref(), Some("limit_depth"), "{src:?}");
        }
        // Types a document, an SVG image and a manifest are: image carriers keep them inline, style sheet and manifest
        // carriers deny them.
        let svg = "data:image/svg+xml,<svg/>";
        assert_only(&format!("<link rel=icon href=\"{svg}\">"), svg, INLINE);
        assert_only(
            &format!("<link rel=preload as=image href=\"{svg}\">"),
            svg,
            INLINE,
        );
        for src in [
            format!("<link rel=stylesheet href=\"{svg}\">"),
            format!("<link rel=\"icon stylesheet\" href=\"{svg}\">"),
            format!("<link rel=manifest href=\"{svg}\">"),
            format!("<link rel=search href=\"{svg}\">"),
            format!("<link rel=prefetch href=\"{svg}\">"),
            format!("<link rel=preload as=fetch href=\"{svg}\">"),
            format!("<link rel=preload href=\"{svg}\">"),
            format!("<link rel=\"nofollow\" href=\"{svg}\">"),
            format!("<link href=\"{svg}\">"),
        ] {
            assert_only(&src, svg, INLINE_DOCUMENT);
        }
        // A manifest is parsed whatever its type: every data: payload is denied there.
        for ty in ["application/json", "text/plain", "image/png", "text/css"] {
            let url = format!("data:{ty},x");
            for rel in [
                "manifest",
                "MANIFEST",
                " search ",
                "payment-method-manifest",
            ] {
                assert_only(
                    &format!("<link rel=\"{rel}\" href=\"{url}\">"),
                    &url,
                    INLINE_DOCUMENT,
                );
            }
        }
    });
}
