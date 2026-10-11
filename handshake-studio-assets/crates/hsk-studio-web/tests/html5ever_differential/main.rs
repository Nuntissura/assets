//! Differential check: hsk-studio-web's reading of HTML against html5ever (dev-dependency oracle, servo).
//!
//! Inputs are hand-written adversarial documents plus a small deterministic generator. Each one is parsed by html5ever
//! (fragment parsing with a `div` context, or a whole document) and by `hsk_studio_web::parse`; both results are reduced
//! to one event list (element namespace and name, effective attributes with decoded values, text, comments) and compared.
//! A typed refusal by the crate is not a disagreement (it is counted and printed); anything else that differs fails the
//! test with the minimised input printed. The crate's public model cannot express some comparisons; see the lane notes.
//!
//! Compared: element namespace and name (the crate's `local_name`), effective attributes (first duplicate wins; names as
//! the tokenizer builds them, values as the crate decodes them), decoded text, RCDATA, RAWTEXT and script bodies,
//! comments and CDATA sections (the crate's own character data from `WebDoc::view`), bogus comments (opaque in the crate,
//! preprocessed here), and per `style` element the text a browser's CSS parser reads against the crate's
//! `WebDoc::stylesheet_text` (the text its asset scan and preview interpret; signature `style-sheet-text`), and per CSS
//! carrier (each `style` / SVG presentation attribute, each style sheet) the URLs the crate's `collect_assets` reports
//! against the URLs `css_reference` (a separately written CSS Syntax 3 reading over html5ever's text) finds there
//! (signature `css-urls`; no licensed CSS reference tokenizer is available as a dev-dependency: cssparser is MPL-2.0,
//! which the workspace licence gate refuses). The scripting
//! enabled reading of `noscript` is compared as the preprocessed source slice. Not expressible through the public model: attribute namespaces and
//! prefixes, SVG tag and attribute case adjustment (compared lowercase), the markup reading of `noscript` under scripting
//! (compared as the source slice). Inputs with an authored `html`, `head` or `body` element are compared against a whole
//! document parse; the documented normalisations N1 (implied elements) and N2 (skeleton whitespace) are in `model`.
//! The generator keeps to tags whose tree construction does not restructure well-nested markup (no `p`, tables, forms,
//! nested `a`, headings, `select`); that area has its own hand-written group, whose every input is pinned as refused
//! (with its typed code) or read like a browser by `tree_construction_dispositions_are_pinned`.
//!
//! The disagreements recorded by lane SMB-WEB-DIFFTEST (D1-D9) are fixed or refused typed by lane SMB-WEB-PARITY; every
//! test in this target is expected to pass. `HSK_DIFFTEST_SCALE=<n>` multiplies the generated corpora for soak runs.

mod corpus;
mod css_reference;
mod events;
mod generator;
mod harness;
mod model;
mod oracle;

use events::{Category, Ev, Ns, first_difference};
use generator::Theme;
use harness::Run;
use oracle::{Mode, reference_events};

/// Soak multiplier for the generated corpora (`HSK_DIFFTEST_SCALE=10`: ten times the seeds and ten times the deadline).
/// Default 1; the floors and the committed seeds do not change.
fn soak_scale() -> u64 {
    std::env::var("HSK_DIFFTEST_SCALE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(1, |n| n.clamp(1, 50))
}

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
    match receiver.recv_timeout(Duration::from_secs(45 * soak_scale())) {
        Ok(Ok(())) => {}
        Ok(Err(payload)) => resume_unwind(payload),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("case {name} exceeded its declared deadline (45 s x soak scale)")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("case {name} lost its deadline worker result")
        }
    }
}

fn run_cases(name: &'static str, cases: Vec<(String, String)>, mode: Mode, min_compared: usize) {
    let mut run = Run::new(name);
    for (label, input) in &cases {
        run.feed(label, input, mode);
    }
    run.finish(min_compared);
}

fn open(ns: Ns, name: &str, attrs: &[(&str, &str)]) -> Ev {
    Ev::Open {
        ns,
        name: name.to_owned(),
        attrs: attrs
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
            .collect(),
    }
}

/// The reference sink is the oracle for everything below; these cases pin its behaviour on well-known inputs (tree
/// construction results from the HTML Standard) so a sink bug cannot show up as a product disagreement.
#[test]
fn oracle_sink_self_check() {
    bounded_case("oracle_sink_self_check", || {
        let t = |s: &str, scripting: bool| reference_events(s, Mode::Fragment, scripting);
        // Namespaces and integration points.
        assert_eq!(
            t(
                "<svg><foreignObject><style>&lt;</style></foreignObject></svg>",
                true
            ),
            vec![
                open(Ns::Svg, "svg", &[]),
                open(Ns::Svg, "foreignobject", &[]),
                open(Ns::Html, "style", &[]),
                Ev::Text("&lt;".into()),
                Ev::Close,
                Ev::Close,
                Ev::Close,
            ]
        );
        assert_eq!(
            t("<svg><style>&lt;</style></svg>", true),
            vec![
                open(Ns::Svg, "svg", &[]),
                open(Ns::Svg, "style", &[]),
                Ev::Text("<".into()),
                Ev::Close,
                Ev::Close
            ]
        );
        assert_eq!(
            t(
                "<math><annotation-xml encoding=TEXT/HTML><style>&lt;</style></annotation-xml></math>",
                true
            ),
            vec![
                open(Ns::MathMl, "math", &[]),
                open(Ns::MathMl, "annotation-xml", &[("encoding", "TEXT/HTML")]),
                open(Ns::Html, "style", &[]),
                Ev::Text("&lt;".into()),
                Ev::Close,
                Ev::Close,
                Ev::Close,
            ]
        );
        // Duplicate attributes: the first wins.
        assert_eq!(
            t("<b id=a ID=b id=c>", true),
            vec![open(Ns::Html, "b", &[("id", "a")]), Ev::Close]
        );
        // Character references: a legacy name without a semicolon decodes in text; in an attribute value it stays literal
        // when `=` or an alphanumeric follows (13.2.5.73), and decodes otherwise.
        assert_eq!(
            t("&quot&notit;", true),
            vec![Ev::Text("\"\u{ac}it;".into())]
        );
        assert_eq!(
            t(
                "<b title=\"&quot&not=\" alt='&notit;' lang=\"&not;&not-\">",
                true
            ),
            vec![
                open(
                    Ns::Html,
                    "b",
                    &[
                        ("title", "\"&not="),
                        ("alt", "&notit;"),
                        ("lang", "\u{ac}\u{ac}-")
                    ]
                ),
                Ev::Close
            ]
        );
        // Scripting flag: noscript is raw text with it, a normal tree without it.
        assert_eq!(
            t("<noscript><b>x</b></noscript>", true),
            vec![
                open(Ns::Html, "noscript", &[]),
                Ev::Text("<b>x</b>".into()),
                Ev::Close
            ]
        );
        assert_eq!(
            t("<noscript><b>x</b></noscript>", false),
            vec![
                open(Ns::Html, "noscript", &[]),
                open(Ns::Html, "b", &[]),
                Ev::Text("x".into()),
                Ev::Close,
                Ev::Close
            ]
        );
        // Tree construction the crate refuses rather than guesses: implied end tags and the textarea / pre newline rule.
        assert_eq!(
            t("<p>a<div>b</div>", true),
            vec![
                open(Ns::Html, "p", &[]),
                Ev::Text("a".into()),
                Ev::Close,
                open(Ns::Html, "div", &[]),
                Ev::Text("b".into()),
                Ev::Close
            ]
        );
        assert_eq!(
            t("<textarea>\nx</textarea>", true),
            vec![
                open(Ns::Html, "textarea", &[]),
                Ev::Text("x".into()),
                Ev::Close
            ]
        );
        // Comments end at `--!>`; NUL becomes U+FFFD in attribute values; CRLF is normalised.
        assert_eq!(
            t("<!-- a --!>b", true),
            vec![Ev::Comment(" a ".into()), Ev::Text("b".into())]
        );
        assert_eq!(
            t("<b title=\"a\0b\r\nc\">", true),
            vec![
                open(Ns::Html, "b", &[("title", "a\u{fffd}b\nc")]),
                Ev::Close
            ]
        );
        // Whole-document parsing keeps explicit structure and drops the doctype.
        assert_eq!(
            reference_events(
                "<!DOCTYPE html><html><head></head><body>x</body></html>",
                Mode::Document,
                true
            ),
            vec![
                open(Ns::Html, "html", &[]),
                open(Ns::Html, "head", &[]),
                Ev::Close,
                open(Ns::Html, "body", &[]),
                Ev::Text("x".into()),
                Ev::Close,
                Ev::Close
            ]
        );
    });
}

/// The comparator has teeth: every mutation of a reference event list is reported, with the intended category.
#[test]
fn comparator_self_check() {
    bounded_case("comparator_self_check", || {
        let base = reference_events(
            "<svg><desc><style>a&amp;b</style></desc></svg><textarea>x</textarea><b id=1>t</b><!--c-->",
            Mode::Fragment,
            true,
        );
        assert!(first_difference(&base, &base).is_none());
        let category_of = |mutate: &dyn Fn(&mut Vec<Ev>)| {
            let mut changed = base.clone();
            mutate(&mut changed);
            first_difference(&base, &changed).map(|d| d.category)
        };
        let position =
            |pred: &dyn Fn(&Ev) -> bool| base.iter().position(pred).expect("event present");
        let svg = position(&|e| matches!(e, Ev::Open { name, .. } if name == "svg"));
        let style_text = position(&|e| matches!(e, Ev::Text(t) if t == "a&amp;b"));
        let b = position(&|e| matches!(e, Ev::Open { name, .. } if name == "b"));
        let b_text = position(&|e| matches!(e, Ev::Text(t) if t == "t"));
        let comment = position(&|e| matches!(e, Ev::Comment(_)));
        assert_eq!(
            category_of(&|v| {
                if let Ev::Open { ns, .. } = &mut v[svg] {
                    *ns = Ns::Html;
                }
            }),
            Some(Category::Namespace)
        );
        assert_eq!(
            category_of(&|v| {
                if let Ev::Open { name, .. } = &mut v[b] {
                    name.push('x');
                }
            }),
            Some(Category::ElementName)
        );
        assert_eq!(
            category_of(&|v| {
                if let Ev::Open { attrs, .. } = &mut v[b] {
                    attrs[0].1 = "2".into();
                }
            }),
            Some(Category::Attribute)
        );
        assert_eq!(
            category_of(&|v| v[style_text] = Ev::Text("a&b".into())),
            Some(Category::RawText)
        );
        assert_eq!(
            category_of(&|v| v[b_text] = Ev::Text("u".into())),
            Some(Category::Text)
        );
        assert_eq!(
            category_of(&|v| v[comment] = Ev::Comment("d".into())),
            Some(Category::Comment)
        );
        assert_eq!(
            category_of(&|v| drop(v.remove(b_text))),
            Some(Category::Structure)
        );
        assert_eq!(
            category_of(&|v| v.push(Ev::Close)),
            Some(Category::Structure)
        );
        assert_eq!(
            category_of(&|v| v.truncate(v.len() - 1)),
            Some(Category::Structure)
        );
        // Fine-grained signatures used to keep different defects apart.
        let sub =
            |expected: Ev, actual: Ev| first_difference(&[expected], &[actual]).map(|d| d.sub);
        assert_eq!(
            sub(Ev::Text("a".into()), Ev::Text("a\0".into())),
            Some("nul-not-replaced-or-dropped")
        );
        assert_eq!(
            sub(Ev::Text("a\n".into()), Ev::Text("a\r".into())),
            Some("cr-not-normalised")
        );
        assert_eq!(
            sub(Ev::Text("a".into()), Ev::Text("\na".into())),
            Some("leading-lf-not-dropped")
        );
        assert_eq!(
            sub(
                open(Ns::Html, "b", &[("t", "\u{fffd}")]),
                open(Ns::Html, "b", &[("t", "\0")])
            ),
            Some("value-nul-not-replaced")
        );
    });
}

#[test]
fn namespaces_and_integration_points() {
    bounded_case("namespaces_and_integration_points", || {
        run_cases("namespaces", corpus::namespaces(), Mode::Fragment, 70)
    });
}

#[test]
fn raw_text_rcdata_and_script_data_boundaries() {
    bounded_case("raw_text_rcdata_and_script_data_boundaries", || {
        run_cases("raw_text", corpus::raw_text(), Mode::Fragment, 180)
    });
}

#[test]
fn comments_declarations_and_cdata() {
    bounded_case("comments_declarations_and_cdata", || {
        run_cases(
            "comments",
            corpus::comments_and_declarations(),
            Mode::Fragment,
            60,
        )
    });
}

#[test]
fn character_references_in_text_and_attributes() {
    bounded_case("character_references_in_text_and_attributes", || {
        run_cases(
            "character_references",
            corpus::character_references(),
            Mode::Fragment,
            5000,
        )
    });
}

#[test]
fn tag_and_attribute_tokenization() {
    bounded_case("tag_and_attribute_tokenization", || {
        run_cases(
            "tag_attr_syntax",
            corpus::tag_and_attribute_syntax(),
            Mode::Fragment,
            55,
        )
    });
}

#[test]
fn duplicate_attributes_first_wins() {
    bounded_case("duplicate_attributes_first_wins", || {
        run_cases(
            "duplicate_attributes",
            corpus::duplicate_attributes(),
            Mode::Fragment,
            35,
        )
    });
}

#[test]
fn null_bytes_and_newline_normalisation() {
    bounded_case("null_bytes_and_newline_normalisation", || {
        run_cases(
            "nulls_and_newlines",
            corpus::nulls_and_newlines(),
            Mode::Fragment,
            50,
        )
    });
}

#[test]
fn tree_construction_restructuring_is_refused_or_read_like_a_browser() {
    bounded_case(
        "tree_construction_restructuring_is_refused_or_read_like_a_browser",
        || {
            run_cases(
                "tree_restructuring",
                corpus::tree_construction_restructuring(),
                Mode::Fragment,
                1,
            )
        },
    );
}

#[test]
fn tree_construction_accepted_inputs_read_like_a_browser() {
    bounded_case(
        "tree_construction_accepted_inputs_read_like_a_browser",
        || {
            let cases = corpus::tree_construction_accepted();
            let all = cases.len();
            // Every input is accepted (the floor is the whole group) and must agree after N1.
            run_cases("tree_accepted", cases, Mode::Fragment, all)
        },
    );
}

/// Every input of the tree-construction group with its outcome: refused with the typed code the crate's detection rule
/// gives it (`dom::construction`), or read like a browser after normalisation N1 (`agrees`). A change in either direction
/// fails here, so the D9 disposition list cannot drift silently.
#[test]
fn tree_construction_dispositions_are_pinned() {
    bounded_case("tree_construction_dispositions_are_pinned", || {
        let expected: &[(&str, &str)] = &[
            ("<p><div>x</div></p>", "refused:implied_end_tag"),
            ("<p>a<div>x</div>b</p>", "refused:implied_end_tag"),
            ("<a href=x><a href=y>t</a></a>", "refused:adoption_agency"),
            ("<a><b></a>t</b>", "refused:misnested_end_tag"),
            ("<b><i></b>t</i>", "refused:misnested_end_tag"),
            ("<table><tr><td>x</td></tr></table>", "agrees"),
            (
                "<table><tbody><tr><td>x</td></tr></tbody></table>",
                "agrees",
            ),
            (
                "<table>x<tr><td>y</td></tr></table>",
                "refused:table_content",
            ),
            (
                "<table><svg><style>&lt;</style></svg></table>",
                "refused:table_content",
            ),
            ("<table><div>x</div></table>", "refused:table_content"),
            ("<form><form></form></form>", "refused:ignored_token"),
            (
                "<button><button></button></button>",
                "refused:implied_end_tag",
            ),
            ("<h1><h2>x</h2></h1>", "refused:implied_end_tag"),
            ("<select><option>a</option></select>", "agrees"),
            (
                "<select><option>a</option><option>b</option></select>",
                "agrees",
            ),
            ("<ul><li>a</li><li>b</li></ul>", "agrees"),
            ("<dl><dt>a</dt><dd>b</dd></dl>", "agrees"),
            ("<nobr><nobr>x</nobr></nobr>", "refused:adoption_agency"),
            ("<div><p>x</p></div>", "agrees"),
            ("<p>x</p><p>y</p>", "agrees"),
            ("<li><li></li></li>", "refused:implied_end_tag"),
            ("<ul><li><ul><li>x</li></ul></li></ul>", "agrees"),
            ("<math><mi><p>x</p></mi></math>", "agrees"),
            (
                "<svg><foreignObject><p>a<p>b</p></p></foreignObject></svg>",
                "refused:implied_end_tag",
            ),
            (
                "<svg><foreignObject><table><tr><td>x</td></tr></table></foreignObject></svg>",
                "agrees",
            ),
            ("<div><table></table></div>", "agrees"),
            ("<textarea></textarea><textarea></textarea>", "agrees"),
            ("<frameset></frameset>", "refused:frameset_document"),
            ("<body><b>x</b></body>", "agrees"),
            ("<html><b>x</b></html>", "agrees"),
            ("<head><title>x</title></head>", "agrees"),
            ("<caption>x</caption>", "refused:ignored_token"),
            ("<td>x</td>", "refused:ignored_token"),
            ("<tr><td>x</td></tr>", "refused:ignored_token"),
            ("<option>x</option>", "agrees"),
            ("<optgroup><option>x</option></optgroup>", "agrees"),
            ("<button>x</button>", "agrees"),
            ("<fieldset><legend>x</legend></fieldset>", "agrees"),
            ("<details><summary>x</summary></details>", "agrees"),
            ("<menu><li>x</li></menu>", "agrees"),
            ("<dd>x</dd>", "agrees"),
            ("<h1>x</h1><h2>y</h2>", "agrees"),
            ("<p><h1>x</h1></p>", "refused:implied_end_tag"),
            ("<p><ul><li>x</li></ul></p>", "refused:implied_end_tag"),
            ("<p><svg></svg></p>", "agrees"),
            ("<p><table></table></p>", "refused:implied_end_tag"),
            ("<p><pre>x</pre></p>", "refused:implied_end_tag"),
            ("<p><hr></p>", "refused:implied_end_tag"),
            ("<p><xmp>x</xmp></p>", "refused:implied_end_tag"),
            ("<p><textarea>x</textarea></p>", "agrees"),
            ("<p><button>x</button></p>", "agrees"),
            ("<p><form></form></p>", "refused:implied_end_tag"),
            ("<p><address>x</address></p>", "refused:implied_end_tag"),
            ("<p><center>x</center></p>", "refused:implied_end_tag"),
            ("<p><section>x</section></p>", "refused:implied_end_tag"),
            ("<p><div></div></p>", "refused:implied_end_tag"),
            ("<p><details></details></p>", "refused:implied_end_tag"),
            ("<p><dl></dl></p>", "refused:implied_end_tag"),
            ("<p><ol></ol></p>", "refused:implied_end_tag"),
            (
                "<p><blockquote></blockquote></p>",
                "refused:implied_end_tag",
            ),
            (
                "<table><colgroup></image><col></colgroup></table>",
                "refused:table_content",
            ),
            (
                "<ruby><rb><rtc></rtc></rb></ruby>",
                "refused:implied_end_tag",
            ),
            (
                "<select><option><option></option></option></select>",
                "refused:implied_end_tag",
            ),
            (
                "<option><optgroup></optgroup></option>",
                "refused:implied_end_tag",
            ),
        ];
        let corpus = corpus::tree_construction_restructuring();
        assert_eq!(corpus.len(), expected.len(), "every corpus input is pinned");
        let mut wrong = Vec::new();
        for ((_, input), (pinned, outcome)) in corpus.iter().zip(expected) {
            assert_eq!(input, pinned, "pin order follows the corpus");
            let actual = harness::disposition(input, Mode::Fragment);
            if actual != *outcome {
                wrong.push(format!("{input:?}: pinned {outcome}, now {actual}"));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    });
}

#[test]
fn image_start_tag_is_read_as_img() {
    bounded_case("image_start_tag_is_read_as_img", || {
        run_cases(
            "image_start_tag",
            corpus::image_start_tag(),
            Mode::Fragment,
            6,
        )
    });
}

#[test]
fn leading_newline_after_pre_listing_textarea() {
    bounded_case("leading_newline_after_pre_listing_textarea", || {
        run_cases(
            "leading_newlines",
            corpus::leading_newlines(),
            Mode::Fragment,
            12,
        )
    });
}

fn generated(name: &'static str, theme: Theme, first_seed: u64, count: u64, min_compared: usize) {
    run_cases(
        name,
        generator::cases(theme, first_seed, count * soak_scale()),
        Mode::Fragment,
        min_compared,
    );
}

#[test]
fn generated_mixed() {
    bounded_case("generated_mixed", || {
        generated("generated_mixed", Theme::Mixed, 1, 3000, 2000)
    });
}

#[test]
fn generated_namespaces() {
    bounded_case("generated_namespaces", || {
        generated(
            "generated_namespaces",
            Theme::Namespaces,
            10_001,
            2500,
            1600,
        )
    });
}

#[test]
fn generated_raw_text_boundaries() {
    bounded_case("generated_raw_text_boundaries", || {
        generated("generated_raw_text", Theme::RawText, 20_001, 2500, 1700)
    });
}

#[test]
fn generated_character_references() {
    bounded_case("generated_character_references", || {
        generated("generated_char_refs", Theme::CharRefs, 30_001, 2500, 1800)
    });
}

#[test]
fn generated_duplicate_attributes() {
    bounded_case("generated_duplicate_attributes", || {
        generated("generated_dup_attrs", Theme::DupAttrs, 40_001, 1500, 1000)
    });
}

#[test]
fn generated_malformed_markup_by_mutation() {
    bounded_case("generated_malformed_markup_by_mutation", || {
        generated("generated_fuzz", Theme::Fuzz, 60_001, 4000, 1000)
    });
}

#[test]
fn generated_null_and_cr_noise() {
    bounded_case("generated_null_and_cr_noise", || {
        generated("generated_noise", Theme::Noise, 50_001, 1000, 700)
    });
}

#[test]
fn whole_documents_with_explicit_structure() {
    bounded_case("whole_documents_with_explicit_structure", || {
        run_cases("documents", corpus::documents(), Mode::Document, 12)
    });
}

/// R10 pre-review R10-F1 / R10-P1 (lead decision 2026-10-11T15:25:17Z): every URL a browser loads from CSS, in every
/// carrier, is reported by the gate exactly as the reference CSS reading finds it (signature `css-urls`).
#[test]
fn css_url_payloads_read_like_the_reference() {
    bounded_case("css_url_payloads_read_like_the_reference", || {
        run_cases("css_urls", corpus::css_urls(), Mode::Fragment, 36)
    });
}

#[test]
fn generated_css_urls() {
    bounded_case("generated_css_urls", || {
        generated("generated_css", Theme::Css, 70_001, 3000, 2400)
    });
}
