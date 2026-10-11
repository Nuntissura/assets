//! Runs one input through both parsers, minimises disagreements and reports them with the input printed.

use crate::events::{Category, Ev, first_difference};
use crate::model::{model_attribute_count, model_events, parse_model};
use crate::oracle::{Mode, reference_events};
use hsk_studio_web::LossKind;
use std::collections::BTreeMap;
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

/// Compare the crate's reading of `input` with html5ever's, once per scripting-flag reading.
pub fn check(input: &str, mode: Mode) -> Outcome {
    let doc = match parse_model(input) {
        Ok(doc) => doc,
        Err(code) => return Outcome::Refused(code),
    };
    let mut found = Vec::new();
    for scripting in [true, false] {
        let expected = reference_events(input, mode, scripting);
        let actual = model_events(&doc, scripting);
        if let Some(d) = first_difference(&expected, &actual) {
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
