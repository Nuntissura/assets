//! Reading-neutral event list both parsers are reduced to, and the first-difference comparison.

use std::fmt;

/// Element namespace as an HTML parser assigns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Ns {
    Html,
    Svg,
    MathMl,
}

impl fmt::Display for Ns {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Ns::Html => "html",
            Ns::Svg => "svg",
            Ns::MathMl => "math",
        })
    }
}

/// One step of a pre-order walk. Attribute names are ASCII-lowercased, valueless attributes carry an empty value, and
/// duplicates are already resolved (first occurrence wins, as in an HTML tokenizer).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ev {
    Open {
        ns: Ns,
        name: String,
        attrs: Vec<(String, String)>,
    },
    Close,
    Text(String),
    Comment(String),
}

impl fmt::Display for Ev {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ev::Open { ns, name, attrs } => {
                write!(f, "<{ns}:{}", name.escape_debug())?;
                for (n, v) in attrs {
                    write!(f, " {}={v:?}", n.escape_debug())?;
                }
                f.write_str(">")
            }
            Ev::Close => f.write_str("</>"),
            Ev::Text(t) => write!(f, "text {t:?}"),
            Ev::Comment(c) => write!(f, "comment {c:?}"),
        }
    }
}

/// Event list builder: adjacent text merges (a DOM has no adjacent text nodes) and empty text vanishes.
#[derive(Default, Debug)]
pub struct Events(pub Vec<Ev>);

impl Events {
    pub fn push(&mut self, ev: Ev) {
        match ev {
            Ev::Text(t) => {
                if t.is_empty() {
                    return;
                }
                if let Some(Ev::Text(prev)) = self.0.last_mut() {
                    prev.push_str(&t);
                } else {
                    self.0.push(Ev::Text(t));
                }
            }
            other => self.0.push(other),
        }
    }
}

/// Elements a tree builder inserts without a start tag that the crate documents as normalisation N1 (crate docs, `dom`
/// module): `html`, `head`, `body` (13.2.6.4.2, .3, .6), `tbody` around a `tr` and `colgroup` around a `col` directly in a
/// `table` (13.2.6.4.9).
const IMPLIED: &[&str] = &["html", "head", "body", "tbody", "colgroup"];

/// N1: remove the reference's implied elements and splice their children into their parent. Per name, only as many
/// attribute-less HTML elements as the reference has more than the model are removed, each one where the model's event
/// at the same position is something else; text that becomes adjacent merges as in a DOM. An authored element is never
/// removed (the counts match), so a re-parented authored element still shows as a difference.
pub fn drop_implied(expected: &[Ev], actual: &[Ev]) -> Vec<Ev> {
    let count = |evs: &[Ev], n: &str| {
        evs.iter()
            .filter(|e| matches!(e, Ev::Open { ns: Ns::Html, name, .. } if name == n))
            .count()
    };
    let mut budget: Vec<(&str, usize)> = IMPLIED
        .iter()
        .map(|n| (*n, count(expected, n).saturating_sub(count(actual, n))))
        .collect();
    let mut out = Events::default();
    let mut skipped: Vec<bool> = Vec::new();
    for ev in expected {
        match ev {
            Ev::Open {
                ns: Ns::Html,
                name,
                attrs,
            } if attrs.is_empty() => {
                let aligned = actual.get(out.0.len()) == Some(ev);
                let slot = budget
                    .iter_mut()
                    .find(|(n, left)| *n == name.as_str() && *left > 0);
                match slot {
                    Some((_, left)) if !aligned => {
                        *left -= 1;
                        skipped.push(true);
                    }
                    _ => {
                        skipped.push(false);
                        out.push(ev.clone());
                    }
                }
            }
            Ev::Open { .. } => {
                skipped.push(false);
                out.push(ev.clone());
            }
            Ev::Close => {
                if skipped.pop() != Some(true) {
                    out.push(Ev::Close);
                }
            }
            other => out.push(other.clone()),
        }
    }
    out.0
}

/// What differs first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    /// Same position, different element namespace.
    Namespace,
    /// Same position, different element name.
    ElementName,
    /// Same element, different effective attributes (names or decoded values).
    Attribute,
    /// Text of a raw-text, RCDATA, script or `noscript` element differs (where the body starts or ends, or its content).
    RawText,
    /// Text of any other element differs (decoded character references, NUL, newlines).
    Text,
    Comment,
    /// Different kind of node at the same position (tree shape).
    Structure,
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Category::Namespace => "NAMESPACE",
            Category::ElementName => "ELEMENT-NAME",
            Category::Attribute => "ATTRIBUTE",
            Category::RawText => "RAW-TEXT",
            Category::Text => "TEXT",
            Category::Comment => "COMMENT",
            Category::Structure => "STRUCTURE",
        })
    }
}

const RAW_OWNERS: &[&str] = &[
    "script",
    "style",
    "xmp",
    "iframe",
    "noembed",
    "noframes",
    "textarea",
    "title",
    "noscript",
    "plaintext",
];

/// One difference: its category, a finer signature (what kind of difference) and a few events of context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Difference {
    pub category: Category,
    pub sub: &'static str,
    pub detail: String,
}

/// Why two strings differ, as far as the known newline and NUL rules explain it.
fn text_sub(expected: &str, actual: &str) -> &'static str {
    if actual.contains('\0') && !expected.contains('\0') {
        "nul-not-replaced-or-dropped"
    } else if actual.contains('\r') && !expected.contains('\r') {
        "cr-not-normalised"
    } else if actual.strip_prefix('\n') == Some(expected) {
        "leading-lf-not-dropped"
    } else {
        "other"
    }
}

fn attribute_sub(expected: &[(String, String)], actual: &[(String, String)]) -> &'static str {
    for (e, a) in expected.iter().zip(actual.iter()) {
        if e.0 != a.0 {
            return if a.0.contains('\0') {
                "name-nul-not-replaced"
            } else {
                "name"
            };
        }
        if e.1 != a.1 {
            return match text_sub(&e.1, &a.1) {
                "nul-not-replaced-or-dropped" => "value-nul-not-replaced",
                "cr-not-normalised" => "value-cr-not-normalised",
                _ => "value",
            };
        }
    }
    "count"
}

fn only_nul(t: &str) -> bool {
    !t.is_empty() && t.chars().all(|c| c == '\0')
}

/// First difference between the reference events and the model events, with a few events of context each side.
pub fn first_difference(expected: &[Ev], actual: &[Ev]) -> Option<Difference> {
    // Open raw-text owners (HTML namespace only) so text differences can be classified.
    let mut open: Vec<(Ns, String)> = Vec::new();
    let n = expected.len().min(actual.len());
    for i in 0..n {
        let (e, a) = (&expected[i], &actual[i]);
        if e != a {
            let (category, sub) = match (e, a) {
                (
                    Ev::Open {
                        ns: ne,
                        name: e_name,
                        attrs: e_attrs,
                    },
                    Ev::Open {
                        ns: na,
                        name: a_name,
                        attrs: a_attrs,
                    },
                ) => {
                    if ne != na {
                        (Category::Namespace, "namespace")
                    } else if e_name != a_name {
                        (
                            Category::ElementName,
                            if a_name.contains('\0') {
                                "nul-not-replaced"
                            } else {
                                "name"
                            },
                        )
                    } else {
                        (Category::Attribute, attribute_sub(e_attrs, a_attrs))
                    }
                }
                (Ev::Text(et), Ev::Text(at)) => {
                    let sub = text_sub(et, at);
                    if open.last().is_some_and(|(ns, name)| {
                        *ns == Ns::Html && RAW_OWNERS.contains(&name.as_str())
                    }) {
                        (Category::RawText, sub)
                    } else {
                        (Category::Text, sub)
                    }
                }
                (Ev::Comment(_), Ev::Comment(_)) => (Category::Comment, "other"),
                (_, Ev::Text(t)) if only_nul(t) => {
                    (Category::Structure, "nul-only-text-not-dropped")
                }
                _ => (Category::Structure, "other"),
            };
            return Some(Difference {
                category,
                sub,
                detail: window(expected, actual, i),
            });
        }
        match e {
            Ev::Open { ns, name, .. } => open.push((*ns, name.clone())),
            Ev::Close => {
                open.pop();
            }
            _ => {}
        }
    }
    if expected.len() != actual.len() {
        let sub = match actual.get(n) {
            Some(Ev::Text(t)) if only_nul(t) => "nul-only-text-not-dropped",
            _ => "other",
        };
        return Some(Difference {
            category: Category::Structure,
            sub,
            detail: window(expected, actual, n),
        });
    }
    None
}

fn window(expected: &[Ev], actual: &[Ev], at: usize) -> String {
    let from = at.saturating_sub(2);
    let render = |label: &str, evs: &[Ev]| {
        let mut s = String::new();
        for (k, ev) in evs.iter().enumerate().skip(from).take(5) {
            let mark = if k == at { ">>" } else { "  " };
            s.push_str(&format!("      {label} {mark} [{k}] {ev}\n"));
        }
        if at >= evs.len() {
            s.push_str(&format!("      {label} >> [{at}] <end of events>\n"));
        }
        s
    };
    format!(
        "{}{}",
        render("html5ever", expected),
        render("hsk-web  ", actual)
    )
}
