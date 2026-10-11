//! The carrier inventory is complete in the sense the table can check: every row has a scanning path the dispatch
//! reaches and a fixture that exercises every element and name of the row through `collect_assets`.

use super::*;
use crate::{AssetDecision, AssetPort, Limits, collect_assets, parse};
use hsk_studio_accord::CancellationToken;

const EVIL: &str = "https://evil.example/f";
const EXTERNAL: &str = "Denied(ExternalScheme)";
const INLINE_DOCUMENT: &str = "Denied(InlineDocument)";

struct Port;
impl AssetPort for Port {
    fn resolve(&self, _: &str) -> AssetDecision {
        AssetDecision::Unavailable
    }
}

/// What a fixture document must produce.
enum Expect {
    /// `reference` is reported with `outcome` (Debug text).
    Reported(String, &'static str),
    /// No reference containing `EVIL` is reported.
    NothingEvil,
    /// The document or the scan is refused with this code.
    Refused(&'static str),
}

struct Case {
    doc: String,
    expect: Expect,
}

fn evil(doc: String) -> Case {
    Case {
        doc,
        expect: Expect::Reported(EVIL.to_owned(), EXTERNAL),
    }
}

fn attr(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;")
}

const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];

/// `name="value"` on an HTML element `el` placed where the tree builder keeps it.
fn html_element(el: &str, name: &str, value: &str) -> String {
    let a = format!("{name}=\"{}\"", attr(value));
    match el {
        "body" => format!("<html><head></head><body {a}></body></html>"),
        "html" => format!("<html {a}><head></head><body></body></html>"),
        "head" => format!("<html><head {a}></head><body></body></html>"),
        "table" => format!("<table {a}></table>"),
        "thead" | "tbody" | "tfoot" => format!("<table><{el} {a}></{el}></table>"),
        "tr" => format!("<table><tbody><tr {a}></tr></tbody></table>"),
        "td" | "th" => format!("<table><tbody><tr><{el} {a}></{el}></tr></tbody></table>"),
        "source" if name == "srcset" => {
            format!("<picture><source {a}><img src=\"x.png\"></picture>")
        }
        "source" | "track" => format!("<video><{el} {a}></video>"),
        "input" if name == "src" => format!("<input type=\"image\" {a}>"),
        // HTML `<image>` is an `img` (covered by that name); the row's own name is the SVG element.
        "image" => format!("<svg><image {a}/></svg>"),
        el if VOID.contains(&el) => format!("<{el} {a}>"),
        el => format!("<{el} {a}></{el}>"),
    }
}

/// `name="value"` on a foreign element `el` (`math`-rooted for MathML names, else inside `svg`).
fn svg_element(el: &str, name: &str, value: &str) -> String {
    let a = format!("{name}=\"{}\"", attr(value));
    match el {
        "feimage" => format!("<svg><filter><feImage {a}/></filter></svg>"),
        "mi" => format!("<math><mi {a}></mi></math>"),
        el => format!("<svg><{el} {a}/></svg>"),
    }
}

fn elements_or<'a>(row: &'a Carrier, any: &'a [&'a str]) -> &'a [&'a str] {
    if row.elements.is_empty() {
        any
    } else {
        row.elements
    }
}

/// The documents exercising `row` (every element and name of it); `None` for an unknown fixture name.
fn fixture(row: &Carrier) -> Option<Vec<Case>> {
    let mut out = Vec::new();
    match row.fixture {
        "html_url" => {
            for el in row.elements {
                for n in row.names {
                    if *el == "frame" {
                        // A frame exists only in a frameset document, which the crate refuses typed.
                        out.push(Case {
                            doc: format!("<frameset><frame {n}=\"{EVIL}\"></frameset>"),
                            expect: Expect::Refused("frameset_document"),
                        });
                    } else {
                        out.push(evil(html_element(el, n, EVIL)));
                    }
                }
            }
        }
        "html_url_list" => {
            for el in row.elements {
                for n in row.names {
                    out.push(evil(html_element(el, n, &format!("a.png\t{EVIL}"))));
                }
            }
        }
        "html_srcset" => {
            for el in row.elements {
                for n in row.names {
                    out.push(evil(html_element(el, n, &format!("a.png 1x, {EVIL} 2x"))));
                }
            }
        }
        "html_srcdoc" => out.push(Case {
            doc: html_element("iframe", "srcdoc", &format!("<img src={EVIL}>")),
            expect: Expect::Reported(format!("<img src={EVIL}>"), INLINE_DOCUMENT),
        }),
        "meta_refresh" => {
            out.push(evil(format!(
                "<meta http-equiv=\"Refresh\" content=\"0; URL='{EVIL}'\">"
            )));
            // Without http-equiv=refresh the content attribute is text.
            out.push(Case {
                doc: format!("<meta name=\"x\" content=\"0; url={EVIL}\">"),
                expect: Expect::NothingEvil,
            });
        }
        "no_load_attribute" => {
            for n in row.names {
                out.push(Case {
                    doc: format!("<div {n}=\"{EVIL}\"></div>"),
                    expect: Expect::NothingEvil,
                });
            }
        }
        "frameset_refused" => out.push(Case {
            doc: format!("<frameset><frame src=\"{EVIL}\"></frameset>"),
            expect: Expect::Refused("frameset_document"),
        }),
        "base_href" => {
            out.push(evil(format!("<base href=\"{EVIL}\">")));
            out.push(Case {
                doc: format!("<base href=\"{EVIL}\"><img src=\"a.png\">"),
                expect: Expect::Reported("a.png".to_owned(), "Denied(BaseHref)"),
            });
        }
        "xml_base" => {
            for n in row.names {
                out.push(evil(svg_element("g", n, EVIL)));
                out.push(Case {
                    doc: format!("<svg {n}=\"{EVIL}\"><image href=\"a.png\"/></svg>"),
                    expect: Expect::Reported("a.png".to_owned(), "Denied(BaseHref)"),
                });
            }
        }
        "svg_url" => {
            for el in elements_or(row, &["lineargradient", "textpath", "mpath", "x-any", "mi"]) {
                for n in row.names {
                    out.push(evil(svg_element(el, n, EVIL)));
                }
            }
        }
        "css_attribute" => {
            for n in row.names {
                let value = format!("url({EVIL})");
                if *n == "style" {
                    out.push(evil(html_element("div", n, &format!("background:{value}"))));
                    out.push(evil(svg_element("g", n, &format!("fill:{value}"))));
                } else {
                    out.push(evil(svg_element("rect", n, &value)));
                }
            }
        }
        "animation" => {
            for el in row.elements {
                for n in row.names {
                    let css = if *n == "values" {
                        format!("none /*;url({EVIL})")
                    } else {
                        format!("url({EVIL})")
                    };
                    let url = if *n == "values" {
                        format!("a.png;{EVIL}")
                    } else {
                        EVIL.to_owned()
                    };
                    out.push(evil(format!(
                        "<svg><rect><{el} attributeName=\"fill\" {n}=\"{}\"/></rect></svg>",
                        attr(&css)
                    )));
                    out.push(evil(format!(
                        "<svg><image><{el} attributeName=\"href\" {n}=\"{}\"/></image></svg>",
                        attr(&url)
                    )));
                    // A target that is neither a URL attribute nor a CSS property of the inventory loads nothing.
                    out.push(Case {
                        doc: format!(
                            "<svg><rect><{el} attributeName=\"width\" {n}=\"{}\"/></rect></svg>",
                            attr(&url)
                        ),
                        expect: Expect::NothingEvil,
                    });
                }
            }
        }
        "style_element" => {
            out.push(evil(format!("<style>.a{{background:url({EVIL})}}</style>")));
            out.push(evil(format!(
                "<svg><style>.a{{background:url({EVIL})}}</style></svg>"
            )));
        }
        "speculation_rules" => {
            let rules = format!("{{\"prefetch\":[{{\"source\":\"list\",\"urls\":[\"{EVIL}\"]}}]}}");
            for ty in ["speculationrules", " SpeculationRules\n"] {
                out.push(Case {
                    doc: format!("<script type=\"{ty}\">{rules}</script>"),
                    expect: Expect::Reported(rules.clone(), INLINE_DOCUMENT),
                });
            }
            out.push(Case {
                doc: format!("<script type=\"application/json\">{rules}</script>"),
                expect: Expect::NothingEvil,
            });
        }
        "css_property" => {
            for n in row.names {
                out.push(evil(html_element("div", "style", &format!("{n}:url({EVIL})"))));
                out.push(evil(format!(
                    "<svg><rect><set attributeName=\"{n}\" to=\"url({EVIL})\"/></rect></svg>"
                )));
                out.push(evil(format!(
                    "<svg><rect><animate attributeName=\"{}\" values=\"none /*;url({EVIL})\"/></rect></svg>",
                    n.to_ascii_uppercase()
                )));
            }
        }
        "css_at_rule" => {
            for at in row.elements {
                for n in row.names {
                    let sheet = match (*at, *n) {
                        ("@import", _) => format!("@import \"{EVIL}\";@import url({EVIL});"),
                        (at, n) => format!("{at} x{{{n}:url({EVIL})}}"),
                    };
                    out.push(evil(format!("<style>{sheet}</style>")));
                }
            }
        }
        "css_namespace" => out.push(Case {
            doc: format!(
                "<style>@namespace url({EVIL});@namespace svg \"{EVIL}\";@NAMESPACE x url(\"{EVIL}\");\
                 @n\\61mespace/**/y/**/url({EVIL})</style>"
            ),
            expect: Expect::NothingEvil,
        }),
        "css_function" => {
            for f in row.names {
                out.push(evil(html_element(
                    "div",
                    "style",
                    &format!("background:{f}('{EVIL}')"),
                )));
            }
        }
        "data_css" => {
            for n in row.names {
                let inner = format!("data:{n},@import '{EVIL}';");
                out.push(evil(format!("<style>@import url(\"{inner}\");</style>")));
                out.push(evil(html_element("link", "href", &inner)));
                out.push(evil(html_element("img", "src", &inner)));
                out.push(Case {
                    doc: html_element("link", "href", &inner),
                    expect: Expect::Reported(inner.clone(), "Inline"),
                });
            }
        }
        "data_document" => {
            for n in row.names {
                let url = format!("data:{n},<img src={EVIL}>");
                out.push(Case {
                    doc: html_element("iframe", "src", &url),
                    expect: Expect::Reported(url.clone(), INLINE_DOCUMENT),
                });
                out.push(Case {
                    doc: html_element("img", "src", &url),
                    expect: Expect::Reported(url.clone(), "Inline"),
                });
            }
        }
        "data_inert" => {
            for n in row.names {
                let url = format!("data:{n},x");
                for el in ["iframe", "img", "object"] {
                    let name = if el == "object" { "data" } else { "src" };
                    out.push(Case {
                        doc: html_element(el, name, &url),
                        expect: Expect::Reported(url.clone(), "Inline"),
                    });
                }
            }
        }
        _ => return None,
    }
    Some(out)
}

const FIXTURES: &[&str] = &[
    "html_url",
    "html_url_list",
    "html_srcset",
    "html_srcdoc",
    "meta_refresh",
    "no_load_attribute",
    "frameset_refused",
    "base_href",
    "xml_base",
    "svg_url",
    "css_attribute",
    "animation",
    "style_element",
    "speculation_rules",
    "css_property",
    "css_at_rule",
    "css_namespace",
    "css_function",
    "data_css",
    "data_document",
    "data_inert",
];

/// The dispatch reaches the row's path (a later row can be shadowed by an earlier one with the same element and name).
fn dispatch_reaches(row: &Carrier) -> Result<(), String> {
    match row.class {
        Class::HtmlAttribute | Class::SvgAttribute => {
            let els = elements_or(row, &["x-any"]);
            for el in els {
                for n in row.names {
                    let got = attribute_path(el, n);
                    if got != Some((row.path, row.load)) {
                        return Err(format!("attribute_path({el:?}, {n:?}) = {got:?}"));
                    }
                }
            }
            if row.path == Path::Css {
                for n in row.names {
                    if animation_target(n) != Some(Path::Css) {
                        return Err(format!("{n:?} is no animation CSS target"));
                    }
                }
            }
        }
        Class::SvgAnimation => {
            for el in row.elements {
                for n in row.names {
                    if animation_path(el, n) != Some(row.path) {
                        return Err(format!("animation_path({el:?}, {n:?})"));
                    }
                }
            }
        }
        Class::CssProperty => {
            for n in row.names {
                if !css_target(n) || animation_target(n) != Some(Path::Css) {
                    return Err(format!("{n:?} is no CSS animation target"));
                }
            }
        }
        Class::CssFunction => {
            for n in row.names {
                if !URL_FUNCTIONS.contains(n) {
                    return Err(format!("{n:?} is not a URL function"));
                }
            }
        }
        // Element text, at-rules and data: payloads are fixed code paths in `assets`, `cssurl` and `datauri`; their
        // fixtures prove them.
        Class::ElementText | Class::CssAtRule | Class::DataPayload => {}
    }
    Ok(())
}

fn run(case: &Case) -> Result<(), String> {
    let found = parse(
        &case.doc,
        0,
        &Limits::default(),
        &CancellationToken::default(),
    )
    .map_err(|e| e.code().to_owned())
    .and_then(|doc| {
        collect_assets(&doc, &Port, &CancellationToken::default()).map_err(|e| e.code().to_owned())
    });
    match (&case.expect, found) {
        (Expect::Refused(code), Err(got)) if got == *code => Ok(()),
        (expect, Err(got)) if !matches!(expect, Expect::Refused(_)) => {
            Err(format!("refused {got}"))
        }
        (Expect::Refused(code), result) => Err(format!("expected refusal {code}, got {result:?}")),
        (Expect::Reported(reference, outcome), Ok(found)) => {
            if found
                .iter()
                .any(|a| a.reference == *reference && format!("{:?}", a.outcome) == *outcome)
            {
                Ok(())
            } else {
                Err(format!("{reference:?} not reported {outcome}: {found:?}"))
            }
        }
        (Expect::NothingEvil, Ok(found)) => {
            if found.iter().any(|a| a.reference.contains(EVIL)) {
                Err(format!("reported {found:?}"))
            } else {
                Ok(())
            }
        }
        (_, Err(got)) => Err(format!("refused {got}")),
    }
}

/// Every row: a fixture exists, the dispatch reaches the row's path, and every case of the fixture holds.
#[test]
fn every_row_has_a_path_and_a_fixture() {
    let mut failures = Vec::new();
    let mut cases = 0usize;
    for (i, row) in CARRIERS.iter().enumerate() {
        let label = format!("row {i} {:?} {:?} {:?}", row.class, row.elements, row.names);
        assert!(!row.source.is_empty(), "{label}: no source");
        if let Err(e) = dispatch_reaches(row) {
            failures.push(format!("{label}: {e}"));
        }
        let Some(list) = fixture(row) else {
            failures.push(format!("{label}: no fixture {:?}", row.fixture));
            continue;
        };
        if list.is_empty() {
            failures.push(format!("{label}: fixture {:?} has no case", row.fixture));
        }
        for case in &list {
            cases += 1;
            if let Err(e) = run(case) {
                failures.push(format!("{label}: {:?}: {e}", case.doc));
            }
        }
    }
    // Every fixture is used by a row.
    for f in FIXTURES {
        if !CARRIERS.iter().any(|r| r.fixture == *f) {
            failures.push(format!("fixture {f:?} unused"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} failure(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(cases > 400, "{cases} cases");
}

/// The inventory's sources: every class of carrier the lead decision names has rows (HTML attribute index, SVG attribute
/// index and presentation attributes, SVG animation, CSS properties incl. shorthands such as `marker`, at-rules, URL
/// functions, `data:` payloads).
#[test]
fn inventory_covers_every_source() {
    for class in [
        Class::HtmlAttribute,
        Class::SvgAttribute,
        Class::SvgAnimation,
        Class::ElementText,
        Class::CssProperty,
        Class::CssAtRule,
        Class::CssFunction,
        Class::DataPayload,
    ] {
        assert!(CARRIERS.iter().any(|r| r.class == class), "{class:?}");
    }
    for name in [
        "marker",
        "mask",
        "background",
        "list-style",
        "border-image",
        "-webkit-mask",
    ] {
        assert!(css_target(name), "{name}");
    }
    for at in ["@import", "@font-face", "@namespace"] {
        assert!(CARRIERS.iter().any(|r| r.elements.contains(&at)), "{at}");
    }
    // Names are lower-case (the dispatch compares lower-cased names).
    for row in CARRIERS {
        for n in row.names.iter().chain(row.elements) {
            assert_eq!(*n, n.to_ascii_lowercase(), "{n}");
        }
    }
}
