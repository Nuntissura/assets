//! Runs one input through both parsers, minimises disagreements and reports them with the input printed.

use crate::css_reference::{UrlKind, urls};
use crate::events::{Category, Ev, drop_implied, first_difference};
use crate::model::{has_skeleton, model_attribute_count, model_events, parse_model};
use crate::oracle::{Mode, reference_events};
use hsk_studio_accord::CancellationToken;
use hsk_studio_web::{
    AssetDecision, AssetOutcome, AssetPort, AssetSource, LossKind, Namespace, NodeKind, WebDoc,
    collect_assets,
};
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;

/// One disagreement on one input.
#[derive(Clone, Debug)]
pub struct Disagreement {
    pub category: Category,
    pub sub: &'static str,
    pub scripting: bool,
    pub detail: String,
}

pub enum Outcome {
    /// The crate refused the document with this stable error code; nothing to compare.
    Refused(&'static str),
    /// Both parsers were compared; empty = they agree.
    Compared(Vec<Disagreement>),
}

/// Oracle correction O1 (an html5ever 0.40.1 deviation from the HTML Standard, not a crate normalisation): html5ever's
/// tree builder takes the "ignore the next LF" flag of `pre`, `listing` and `textarea` on every token it is handed,
/// parse errors included (`tree_builder/mod.rs:481` runs `self.ignore_lf.take()` before the `ParseError` arm), so the
/// missing-end-tag-name parse error of an `</>` right after `<pre>` makes it keep the LF. The Standard emits no token for
/// `</>` (13.2.5.7, U+003E: a parse error, then the data state) and 13.2.6.4.7 ignores the next LF token. The reference is
/// therefore given the input without each `</>` the crate reads as such (no token) that directly follows a `>` or starts
/// the input: there the tokenizer is in the data state with nothing pending, so removing it changes no token. Every other
/// `</>` (after `<` or `&`, where removal would change tokens) is left in place, and so is one inside an HTML `noscript`
/// for the scripting-enabled reading (there the content is RAWTEXT and `</>` is text).
fn oracle_input(doc: &WebDoc, input: &str, scripting: bool) -> String {
    let b = input.as_bytes();
    let in_noscript = |mut id: usize| {
        while let Some(p) = doc.node(id).and_then(|n| n.parent) {
            if doc.node(p).is_some_and(|n| n.namespace == Namespace::Html)
                && doc.local_name(p).as_deref() == Some("noscript")
            {
                return true;
            }
            id = p;
        }
        false
    };
    let mut cut: Vec<std::ops::Range<usize>> = (0..doc.len())
        .filter(|&id| {
            doc.node(id)
                .is_some_and(|n| n.kind == NodeKind::Unsupported && doc.slice(&n.span) == "</>")
                && !(scripting && in_noscript(id))
        })
        .filter_map(|id| doc.node(id).map(|n| n.span.clone()))
        .collect();
    cut.sort_by_key(|s| s.start);
    let mut out = String::with_capacity(input.len());
    let mut from = 0;
    for span in cut {
        let after_markup = span.start == 0 || b[span.start - 1] == b'>';
        if after_markup && span.start >= from {
            out.push_str(&input[from..span.start]);
            from = span.end;
        }
    }
    out.push_str(&input[from..]);
    out
}

/// Compare the crate's reading of `input` with html5ever's, once per scripting-flag reading. The crate reads whole
/// documents; an input with an authored `html`, `head` or `body` element is compared against a whole-document parse
/// (the fragment parser's `div` context ignores those start tags). Normalisations N1 (always) and N2 (whole documents)
/// are documented in `model`; the oracle correction O1 in `oracle_input`.
pub fn check(input: &str, mode: Mode) -> Outcome {
    let doc = match parse_model(input) {
        Ok(doc) => doc,
        Err(code) => return Outcome::Refused(code),
    };
    let mode = if mode == Mode::Fragment && has_skeleton(&doc) {
        Mode::Document
    } else {
        mode
    };
    let mut found = Vec::new();
    for scripting in [true, false] {
        let actual = model_events(&doc, scripting, mode == Mode::Document);
        let reference_input = oracle_input(&doc, input, scripting);
        let expected = drop_implied(
            &reference_events(&reference_input, mode, scripting),
            &actual,
        );
        let difference = first_difference(&expected, &actual);
        if !scripting && difference.is_none() {
            // Gate-level check: the style text the crate's asset scan and preview read (`WebDoc::stylesheet_text`) must be
            // the child text content a browser's DOM hands to CSS, for every `style` element (HTML and foreign).
            let reference = style_texts(&expected);
            let crate_texts: Vec<String> = (0..doc.len())
                .filter(|&id| doc.local_name(id).as_deref() == Some("style"))
                .map(|id| doc.stylesheet_text(id).unwrap_or_default())
                .collect();
            if reference != crate_texts {
                found.push(Disagreement {
                    category: Category::RawText,
                    sub: "style-sheet-text",
                    scripting,
                    detail: format!(
                        "      html5ever style text content {reference:?}\n      hsk-web stylesheet_text      {crate_texts:?}\n"
                    ),
                });
            } else if let Some(detail) = css_urls_differ(&doc, &expected) {
                // Gate-level CSS check: the URLs the crate's asset gate reports from every CSS carrier equal the URLs the
                // reference CSS reading (`css_reference`) finds in the text html5ever hands to CSS for that carrier.
                found.push(Disagreement {
                    category: Category::Attribute,
                    sub: "css-urls",
                    scripting,
                    detail,
                });
            }
        }
        if let Some(d) = difference {
            found.push(Disagreement {
                category: d.category,
                sub: d.sub,
                scripting,
                detail: d.detail,
            });
        }
        if !scripting {
            // Duplicate attributes: html5ever's tokenizer drops every later duplicate; the crate must list each one in
            // its loss report so no consumer can mistake it for an effective attribute.
            let reference_attrs: usize = expected
                .iter()
                .map(|e| match e {
                    Ev::Open { attrs, .. } => attrs.len(),
                    _ => 0,
                })
                .sum();
            let dropped = model_attribute_count(&doc).saturating_sub(reference_attrs);
            let reported = doc.loss().count(LossKind::DuplicateAttribute);
            if dropped != reported {
                found.push(Disagreement {
                    category: Category::Attribute,
                    sub: "duplicate-attribute-loss-count",
                    scripting,
                    detail: format!(
                        "      html5ever dropped {dropped} duplicate attribute(s); hsk-web loss report lists {reported} DuplicateAttribute entr(ies)\n"
                    ),
                });
            }
        }
    }
    Outcome::Compared(found)
}

/// Attributes the gate reads as CSS on any element (`style` and the SVG presentation attributes it scans).
const CSS_ATTRIBUTES: &[&str] = &[
    "style",
    "fill",
    "stroke",
    "clip-path",
    "cursor",
    "filter",
    "mask",
    "marker-start",
    "marker-mid",
    "marker-end",
];

struct GrantAll;
impl AssetPort for GrantAll {
    fn resolve(&self, reference: &str) -> AssetDecision {
        AssetDecision::Granted {
            id: reference.to_owned(),
        }
    }
}

/// (computed, value) of each URL, sorted: the order inside one carrier does not matter for the gate.
type CarrierUrls = Vec<(bool, String)>;

/// `None` when, for every CSS carrier (each CSS attribute in source order, then the style sheet of a `style` element), the
/// crate's `collect_assets` reports the same URLs as the reference reading of html5ever's text for that carrier, and never
/// grants a URL the reference reads as bad or computed. Elements with a duplicate CSS attribute are not compared (the gate
/// scans every duplicate; a browser keeps the first).
fn css_urls_differ(doc: &WebDoc, expected: &[Ev]) -> Option<String> {
    let assets = match collect_assets(doc, &GrantAll, &CancellationToken::default()) {
        Ok(a) => a,
        Err(e) => {
            return Some(format!(
                "      hsk-web collect_assets refused: {}\n",
                e.code()
            ));
        }
    };
    let mut crate_lists: Vec<CarrierUrls> = Vec::new();
    let mut granted: Vec<String> = Vec::new();
    for row in doc.view().iter().filter(|r| r.kind == NodeKind::Element) {
        let node = doc.node(row.id)?;
        let mut names = HashSet::new();
        let mut sources: Vec<AssetSource> = Vec::new();
        for attr in &node.attrs {
            let name = doc.attr_name(attr).to_ascii_lowercase();
            if CSS_ATTRIBUTES.contains(&name.as_str()) {
                if !names.insert(name.clone()) {
                    return None;
                }
                sources.push(if name == "style" {
                    AssetSource::InlineStyle
                } else {
                    AssetSource::Attribute(name)
                });
            }
        }
        if doc.local_name(row.id).as_deref() == Some("style") {
            sources.push(AssetSource::StyleBody);
        }
        for source in sources {
            let mut list: CarrierUrls = assets
                .iter()
                .filter(|a| a.node == row.id && a.source == source)
                .map(|a| {
                    if matches!(a.outcome, AssetOutcome::Granted { .. }) {
                        granted.push(a.reference.clone());
                    }
                    // Compared as text so this file also builds on a crate without the reason (red proof at 62d8983).
                    let computed = format!("{:?}", a.outcome) == "Denied(ComputedUrl)";
                    (computed, a.reference.clone())
                })
                .collect();
            list.sort();
            crate_lists.push(list);
        }
    }
    let texts = style_texts(expected);
    let mut sheets = texts.iter();
    let mut labels: Vec<String> = Vec::new();
    let mut reference_lists: Vec<CarrierUrls> = Vec::new();
    // How many times the reference reads each value as a loadable URL (bad and computed readings excluded).
    let mut loadable: BTreeMap<String, usize> = BTreeMap::new();
    for ev in expected {
        let Ev::Open { name, attrs, .. } = ev else {
            continue;
        };
        let mut carriers: Vec<(String, &str)> = attrs
            .iter()
            .filter(|(n, _)| CSS_ATTRIBUTES.contains(&n.as_str()))
            .map(|(n, v)| (n.clone(), v.as_str()))
            .collect();
        if name == "style" {
            carriers.push(("sheet".to_owned(), sheets.next().map_or("", String::as_str)));
        }
        for (label, text) in carriers {
            let found = urls(text);
            for u in found.iter().filter(|u| u.kind == UrlKind::Url) {
                *loadable.entry(u.value.clone()).or_insert(0) += 1;
            }
            let mut list: CarrierUrls = found
                .into_iter()
                .map(|u| (u.kind == UrlKind::Computed, u.value))
                .collect();
            list.sort();
            labels.push(label);
            reference_lists.push(list);
        }
    }
    if crate_lists != reference_lists {
        return Some(format!(
            "      carriers           {labels:?}\n      reference css urls {reference_lists:?}\n      hsk-web css urls   {crate_lists:?}\n"
        ));
    }
    // Bad urls, bad strings and computed URLs are never granted (the port is not asked about them): no value is granted
    // more often than the reference reads it as a loadable URL.
    let mut granted_count: BTreeMap<&String, usize> = BTreeMap::new();
    for v in &granted {
        *granted_count.entry(v).or_insert(0) += 1;
    }
    let wrongly_granted: Vec<&String> = granted_count
        .iter()
        .filter(|(v, n)| **n > loadable.get(v.as_str()).copied().unwrap_or(0))
        .map(|(v, _)| *v)
        .collect();
    if !wrongly_granted.is_empty() {
        return Some(format!(
            "      reference reads these as bad or computed, hsk-web granted them: {wrongly_granted:?}\n"
        ));
    }
    None
}

/// The child text content of every `style` element (any namespace) in document order: the text nodes directly inside it,
/// concatenated (what a browser's DOM hands to its CSS parser; nested elements and comments add nothing).
fn style_texts(events: &[Ev]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut open: Vec<Option<usize>> = Vec::new();
    for ev in events {
        match ev {
            Ev::Open { name, .. } => {
                if name == "style" {
                    out.push(String::new());
                    open.push(Some(out.len() - 1));
                } else {
                    open.push(None);
                }
            }
            Ev::Close => {
                open.pop();
            }
            Ev::Text(t) => {
                if let Some(Some(i)) = open.last() {
                    out[*i].push_str(t);
                }
            }
            Ev::Comment(_) => {}
        }
    }
    out
}

/// One input's outcome as a short string: `refused:<code>`, `agrees`, or `disagrees:<CATEGORY>/<signature>`.
pub fn disposition(input: &str, mode: Mode) -> String {
    match check(input, mode) {
        Outcome::Refused(code) => format!("refused:{code}"),
        Outcome::Compared(found) => match found.first() {
            None => "agrees".to_owned(),
            Some(d) => format!("disagrees:{}/{}", d.category, d.sub),
        },
    }
}

fn find(input: &str, mode: Mode, key: (Category, &'static str, bool)) -> Option<Disagreement> {
    match check(input, mode) {
        Outcome::Refused(_) => None,
        Outcome::Compared(found) => found
            .into_iter()
            .find(|d| (d.category, d.sub, d.scripting) == key),
    }
}

/// Delta-debugging over characters: remove chunks while the same kind of disagreement persists.
pub fn minimize(input: &str, mode: Mode, key: (Category, &'static str, bool)) -> String {
    let mut cur: Vec<char> = input.chars().collect();
    let mut budget = 6000usize;
    let mut chunk = cur.len().div_ceil(2).max(1);
    loop {
        let mut i = 0;
        while i < cur.len() && budget > 0 {
            let end = (i + chunk).min(cur.len());
            let candidate: String = cur[..i].iter().chain(cur[end..].iter()).collect();
            budget -= 1;
            if find(&candidate, mode, key).is_some() {
                cur.drain(i..end);
            } else {
                i += chunk;
            }
        }
        if chunk == 1 || budget == 0 {
            break;
        }
        chunk = chunk.div_ceil(2);
    }
    cur.into_iter().collect()
}

struct Class {
    category: Category,
    sub: &'static str,
    mode: Mode,
    scripting: Vec<bool>,
    minimal: String,
    detail: String,
    count: usize,
    first_label: String,
}

/// Accumulates the outcome of one test's inputs.
pub struct Run {
    name: &'static str,
    compared: usize,
    refused: BTreeMap<&'static str, usize>,
    classes: BTreeMap<(Category, &'static str, String), Class>,
    unminimised: usize,
    disagreeing_inputs: usize,
    /// (label, input, first category and signature) of the first disagreeing inputs, printed when there are few.
    failing: Vec<(String, String, String)>,
}

const MAX_MINIMISATIONS: usize = 80;

impl Run {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            compared: 0,
            refused: BTreeMap::new(),
            classes: BTreeMap::new(),
            unminimised: 0,
            disagreeing_inputs: 0,
            failing: Vec::new(),
        }
    }

    pub fn feed(&mut self, label: &str, input: &str, mode: Mode) {
        match check(input, mode) {
            Outcome::Refused(code) => *self.refused.entry(code).or_insert(0) += 1,
            Outcome::Compared(found) => {
                self.compared += 1;
                if found.is_empty() {
                    return;
                }
                self.disagreeing_inputs += 1;
                if self.failing.len() < 80 {
                    let first = &found[0];
                    self.failing.push((
                        label.to_owned(),
                        input.to_owned(),
                        format!("{}/{}", first.category, first.sub),
                    ));
                }
                for d in found {
                    let key = (d.category, d.sub, d.scripting);
                    let minimal = if self.classes.len() < MAX_MINIMISATIONS {
                        minimize(input, mode, key)
                    } else {
                        self.unminimised += 1;
                        input.to_owned()
                    };
                    let detail = find(&minimal, mode, key).map_or(d.detail.clone(), |x| x.detail);
                    self.classes
                        .entry((d.category, d.sub, minimal.clone()))
                        .and_modify(|c| {
                            c.count += 1;
                            if !c.scripting.contains(&d.scripting) {
                                c.scripting.push(d.scripting);
                            }
                        })
                        .or_insert(Class {
                            category: d.category,
                            sub: d.sub,
                            mode,
                            scripting: vec![d.scripting],
                            minimal,
                            detail,
                            count: 1,
                            first_label: label.to_owned(),
                        });
                }
            }
        }
    }

    /// Print the summary (kept by the test runner for passing tests too) and fail on any disagreement, or when fewer than
    /// `min_compared` inputs were comparable (a refusal-only corpus proves nothing).
    pub fn finish(self, min_compared: usize) {
        let mut summary = format!(
            "difftest[{}]: compared={} refused={} {:?} disagreeing_inputs={} classes={}",
            self.name,
            self.compared,
            self.refused.values().sum::<usize>(),
            self.refused,
            self.disagreeing_inputs,
            self.classes.len()
        );
        println!("{summary}");
        assert!(
            self.compared >= min_compared,
            "NOT_PROVEN: only {} inputs were comparable (floor {min_compared}); the crate refused the rest: {:?}",
            self.compared,
            self.refused
        );
        if self.classes.is_empty() {
            return;
        }
        let mut classes: Vec<&Class> = self.classes.values().collect();
        classes.sort_by(|a, b| (a.category, a.sub, b.count).cmp(&(b.category, b.sub, a.count)));
        summary.push('\n');
        // The three shortest distinct inputs per category and signature, with the total count of inputs and variants.
        let mut shown = std::collections::BTreeSet::new();
        for c in classes.iter() {
            if !shown.insert((c.category, c.sub)) {
                continue;
            }
            let mut others: Vec<&Class> = classes
                .iter()
                .copied()
                .filter(|o| (o.category, o.sub) == (c.category, c.sub))
                .collect();
            let inputs: usize = others.iter().map(|o| o.count).sum();
            let variants = others.len();
            others.sort_by_key(|o| o.minimal.len());
            for (rank, best) in others.iter().take(3).enumerate() {
                let head = if rank == 0 {
                    format!("inputs={inputs} variants={variants} ")
                } else {
                    String::new()
                };
                let _ = writeln!(
                    summary,
                    "  [{}/{}] mode={:?} scripting={:?} {head}first={}\n      input: {:?}\n{}",
                    best.category,
                    best.sub,
                    best.mode,
                    best.scripting,
                    best.first_label,
                    best.minimal,
                    best.detail
                );
            }
        }
        if self.disagreeing_inputs <= 80 {
            let _ = writeln!(summary, "  disagreeing inputs ({}):", self.failing.len());
            for (label, input, what) in &self.failing {
                let _ = writeln!(summary, "      {label}: {input:?} -> {what}");
            }
        }
        if self.unminimised > 0 {
            let _ = writeln!(
                summary,
                "  ({} disagreement(s) not minimised)",
                self.unminimised
            );
        }
        panic!("{summary}");
    }
}
