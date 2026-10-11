//! hsk-studio-web's reading of a document, reduced to the same event list as the reference.
//!
//! Only what the crate's public model exposes is compared: `WebDoc::view()` rows (kind, `local_name` tag, tokenizer-built
//! attribute names, decoded attribute values, decoded text), `WebDoc::text_content` of RCDATA elements, `Node::namespace`,
//! `Node::content` and spans. Opaque source slices (raw-text bodies, comments, bogus markup, CDATA) are source bytes by
//! design, so they get the input-stream preprocessing a tokenizer applies (newline normalisation, NUL) before they are
//! compared. Decoded values are compared exactly as the crate exposes them.
//!
//! Documented normalisations (crate docs, `dom` module), applied here and nowhere else:
//! - N1 implied elements: the reference's implied `html`, `head`, `body`, `tbody` and `colgroup` elements are removed and
//!   their children spliced into their parent (`events::drop_implied`); the crate's tree has authored elements only.
//! - N2 skeleton whitespace (whole-document comparisons only): leading whitespace of text before `<html>` and between
//!   `<html>` and its first element is dropped (the "initial", "before html" and "before head" modes ignore it), and
//!   whitespace text after the body element (after `</body>` and after `</html>`) is emitted at the end of the body
//!   element ("after body" / "after after body" insert it there; with an implied body that is the end of `html`).

use crate::events::{Ev, Events, Ns};
use hsk_studio_accord::CancellationToken;
use hsk_studio_web::{ContentModel, Limits, Namespace, NodeId, NodeKind, WebDoc, parse};
use std::collections::{HashMap, HashSet};

/// Parse with the default limits; a typed refusal is returned as its stable code.
pub fn parse_model(input: &str) -> Result<WebDoc, &'static str> {
    parse(input, 0, &Limits::default(), &CancellationToken::default()).map_err(|e| e.code())
}

/// Input stream preprocessing (HTML Standard 13.2.3.5: CR and CRLF become LF) plus the NUL replacement the raw-text,
/// script-data and comment tokenizer states perform.
pub fn preprocess(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\0' => out.push('\u{fffd}'),
            c => out.push(c),
        }
    }
    out
}

fn ns_of(ns: Namespace) -> Ns {
    match ns {
        Namespace::Html => Ns::Html,
        Namespace::Svg => Ns::Svg,
        Namespace::MathMl => Ns::MathMl,
    }
}

/// Comment data of a `<!-- ... -->` slice (the crate keeps the whole span; `--!>` and the abrupt `<!-->` / `<!--->` forms
/// end comments too).
fn comment_data(slice: &str) -> &str {
    let inner = slice.strip_prefix("<!--").unwrap_or(slice);
    if inner == ">" || inner == "->" {
        return "";
    }
    inner
        .strip_suffix("-->")
        .or_else(|| inner.strip_suffix("--!>"))
        .unwrap_or(inner)
}

/// What an opaque unsupported span is to a tokenizer: CDATA text under a foreign element (preprocessed; its U+0000
/// character tokens become U+FFFD under the foreign-content rules), otherwise a bogus comment (`<!x>`, `<?x?>`, `</ x>`,
/// `<![CDATA[x]]>` in HTML content); `</>` produces nothing, and an end tag (`</` followed by an ASCII letter) is a token
/// the tree builder ignored (the crate keeps one as an opaque node only then: `</image>`).
fn unsupported_event(slice: &str, parent_ns: Ns) -> Option<Ev> {
    if parent_ns != Ns::Html
        && let Some(inner) = slice
            .strip_prefix("<![CDATA[")
            .and_then(|s| s.strip_suffix("]]>"))
    {
        return Some(Ev::Text(preprocess(inner)));
    }
    if slice == "</>" {
        return None;
    }
    if slice
        .strip_prefix("</")
        .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_alphabetic()))
    {
        return None;
    }
    let body = slice.strip_suffix('>').unwrap_or(slice);
    let data = body
        .strip_prefix("</")
        .or_else(|| body.strip_prefix("<!"))
        .or_else(|| body.strip_prefix('<'))
        .unwrap_or(body);
    Some(Ev::Comment(preprocess(data)))
}

fn html_element(doc: &WebDoc, id: NodeId, name: &str) -> bool {
    doc.node(id)
        .is_some_and(|n| n.kind == NodeKind::Element && n.namespace == Namespace::Html)
        && doc.local_name(id).as_deref() == Some(name)
}

fn is_html_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{c}')
}

/// N2 for a whole-document comparison: text nodes whose leading whitespace the skeleton modes drop, and whitespace text
/// nodes moved to the end of `container` (the body element, or `html` when the body is implied).
struct Skeleton {
    trim_leading: HashSet<NodeId>,
    moved: Vec<NodeId>,
    container: Option<NodeId>,
}

fn skeleton(doc: &WebDoc, text_of: &HashMap<NodeId, String>) -> Skeleton {
    let children = |id: NodeId| doc.node(id).map_or_else(Vec::new, |n| n.children.clone());
    let is_text = |id: NodeId| doc.node(id).is_some_and(|n| n.kind == NodeKind::Text);
    let mut s = Skeleton {
        trim_leading: HashSet::new(),
        moved: Vec::new(),
        container: None,
    };
    let root = children(doc.root());
    // Leading whitespace: root-level text before the first element, then (when that element is an authored html) its
    // text children before its first element, until a non-whitespace character.
    let mut level = root.clone();
    'levels: loop {
        let mut next = None;
        for &id in &level {
            match doc.node(id).map(|n| n.kind) {
                Some(NodeKind::Text) => {
                    s.trim_leading.insert(id);
                    let text = text_of.get(&id).map_or("", String::as_str);
                    if !text.trim_start_matches(is_html_space).is_empty() {
                        break 'levels;
                    }
                }
                Some(NodeKind::Element) => {
                    if html_element(doc, id, "html") {
                        next = Some(children(id));
                    }
                    break;
                }
                _ => {}
            }
        }
        match next {
            Some(inner) => level = inner,
            None => break,
        }
    }
    // Trailing whitespace: text after the body element in its parent, and the root's text after an authored html.
    let html = root
        .iter()
        .copied()
        .find(|&id| html_element(doc, id, "html"));
    let body_parent = html.map_or_else(|| root.clone(), children);
    let body = body_parent
        .iter()
        .copied()
        .find(|&id| html_element(doc, id, "body"));
    s.container = body.or(html);
    if let Some(body) = body {
        let after = body_parent.iter().skip_while(|&&id| id != body).skip(1);
        s.moved.extend(after.copied().filter(|&id| is_text(id)));
    }
    if let Some(html) = html {
        let after = root.iter().skip_while(|&&id| id != html).skip(1);
        s.moved.extend(after.copied().filter(|&id| is_text(id)));
    }
    s
}

/// Events of the model's tree. `scripting` selects the reading to compare against: with scripting enabled a browser reads
/// the content of an HTML `noscript` as raw text, so the crate's markup children are collapsed into the source slice.
/// `document`: the reference parsed a whole document, so N2 applies.
pub fn model_events(doc: &WebDoc, scripting: bool, document: bool) -> Vec<Ev> {
    let rows = doc.view();
    let text_of: HashMap<NodeId, String> = rows
        .iter()
        .filter(|r| r.kind == NodeKind::Text)
        .map(|r| (r.id, r.text.clone().unwrap_or_default()))
        .collect();
    let skel = document.then(|| skeleton(doc, &text_of));
    let mut out = Events::default();
    let mut open: Vec<(usize, NodeId)> = Vec::new();
    let mut skip_deeper_than: Option<usize> = None;
    let close = |out: &mut Events, id: NodeId| {
        if let Some(s) = &skel
            && s.container == Some(id)
        {
            for moved in &s.moved {
                out.push(Ev::Text(text_of.get(moved).cloned().unwrap_or_default()));
            }
        }
        out.push(Ev::Close);
    };
    for row in &rows {
        if row.depth == 0 {
            continue;
        }
        while open.last().is_some_and(|&(d, _)| d >= row.depth) {
            let (_, id) = open.pop().expect("open element");
            close(&mut out, id);
        }
        if let Some(limit) = skip_deeper_than {
            if row.depth > limit {
                continue;
            }
            skip_deeper_than = None;
        }
        let node = doc.node(row.id).expect("view rows name existing nodes");
        match row.kind {
            NodeKind::Element => {
                let name = row.tag.clone().unwrap_or_default();
                let mut seen = HashSet::new();
                let mut attrs = Vec::new();
                for (n, v) in &row.attrs {
                    if seen.insert(n.clone()) {
                        attrs.push((n.clone(), v.clone().unwrap_or_default()));
                    }
                }
                let html = node.namespace == Namespace::Html;
                out.push(Ev::Open {
                    ns: ns_of(node.namespace),
                    name: name.clone(),
                    attrs,
                });
                open.push((row.depth, row.id));
                if scripting && html && name == "noscript" {
                    if let Some(content) = node.content_span() {
                        out.push(Ev::Text(preprocess(doc.slice(&content))));
                    }
                    skip_deeper_than = Some(row.depth);
                }
            }
            NodeKind::Text => {
                let text = row.text.clone().unwrap_or_default();
                match &skel {
                    Some(s) if s.moved.contains(&row.id) => {}
                    Some(s) if s.trim_leading.contains(&row.id) => {
                        out.push(Ev::Text(text.trim_start_matches(is_html_space).to_owned()))
                    }
                    _ => out.push(Ev::Text(text)),
                }
            }
            NodeKind::Raw => {
                let owner = node
                    .parent
                    .and_then(|p| doc.node(p).map(|n| (p, n.content)));
                match owner {
                    // RCDATA: the crate's decoded character data (references, newlines, U+FFFD, textarea's first LF).
                    Some((p, ContentModel::Rcdata)) => out.push(Ev::Text(doc.text_content(p))),
                    _ => out.push(Ev::Text(preprocess(doc.slice(&row.span)))),
                }
            }
            NodeKind::Comment => {
                out.push(Ev::Comment(preprocess(comment_data(doc.slice(&row.span)))))
            }
            NodeKind::Unsupported => {
                let parent_ns = node
                    .parent
                    .and_then(|p| doc.node(p))
                    .map_or(Ns::Html, |p| ns_of(p.namespace));
                if let Some(ev) = unsupported_event(doc.slice(&row.span), parent_ns) {
                    out.push(ev);
                }
            }
            NodeKind::Doctype | NodeKind::Document => {}
        }
    }
    while let Some((_, id)) = open.pop() {
        close(&mut out, id);
    }
    out.0
}

/// True when the crate's tree has an authored HTML `html`, `head` or `body` element: a whole document, whose fragment
/// reading (the context `div` ignores those start tags) is not the crate's reading.
pub fn has_skeleton(doc: &WebDoc) -> bool {
    (0..doc.len()).any(|id| {
        ["html", "head", "body"]
            .iter()
            .any(|n| html_element(doc, id, n))
    })
}

/// Total attributes the crate lists across all elements (duplicates included).
pub fn model_attribute_count(doc: &WebDoc) -> usize {
    doc.view().iter().map(|r| r.attrs.len()).sum()
}
