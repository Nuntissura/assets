//! hsk-studio-web's reading of a document, reduced to the same event list as the reference.
//!
//! Only what the crate's public model exposes is compared: `WebDoc::view()` rows (kind, tag, decoded attributes, decoded
//! text), `Node::namespace`, `Node::content` and spans. Opaque source slices (raw-text and RCDATA bodies, comments,
//! bogus markup, CDATA) are source bytes by design, so they get the input-stream preprocessing a tokenizer applies
//! (newline normalisation, NUL) before they are compared; RCDATA bodies are then decoded with the public
//! `decode_entities`. Decoded values (attribute values, text nodes) are compared exactly as the crate exposes them.

use crate::events::{Ev, Events, Ns};
use hsk_studio_accord::CancellationToken;
use hsk_studio_web::{ContentModel, Limits, Namespace, NodeKind, WebDoc, decode_entities, parse};
use std::collections::HashSet;

/// Parse with the default limits; a typed refusal is returned as its stable code.
pub fn parse_model(input: &str) -> Result<WebDoc, &'static str> {
    parse(input, 0, &Limits::default(), &CancellationToken::default()).map_err(|e| e.code())
}

/// Input stream preprocessing (HTML Standard 13.2.3.5: CR and CRLF become LF) plus the NUL replacement the raw-text,
/// RCDATA, script-data and comment tokenizer states perform.
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

/// What an opaque unsupported span is to a tokenizer: CDATA text under a foreign element, otherwise a bogus comment
/// (`<!x>`, `<?x?>`, `</ x>`, `<![CDATA[x]]>` in HTML content); `</>` produces nothing.
fn unsupported_event(slice: &str, parent_ns: Ns) -> Option<Ev> {
    if parent_ns != Ns::Html
        && let Some(inner) = slice
            .strip_prefix("<![CDATA[")
            .and_then(|s| s.strip_suffix("]]>"))
    {
        return Some(Ev::Text(inner.to_owned()));
    }
    if slice == "</>" {
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

/// Events of the model's tree. `scripting` selects the reading to compare against: with scripting enabled a browser reads
/// the content of an HTML `noscript` as raw text, so the crate's markup children are collapsed into the source slice.
pub fn model_events(doc: &WebDoc, scripting: bool) -> Vec<Ev> {
    let mut out = Events::default();
    let mut open: Vec<usize> = Vec::new();
    let mut skip_deeper_than: Option<usize> = None;
    for row in doc.view() {
        if row.depth == 0 {
            continue;
        }
        while open.last().is_some_and(|&d| d >= row.depth) {
            open.pop();
            out.push(Ev::Close);
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
                let name = row.tag.as_deref().unwrap_or_default().to_ascii_lowercase();
                let mut seen = HashSet::new();
                let mut attrs = Vec::new();
                for (n, v) in &row.attrs {
                    let n = n.to_ascii_lowercase();
                    if seen.insert(n.clone()) {
                        attrs.push((n, v.clone().unwrap_or_default()));
                    }
                }
                let html = node.namespace == Namespace::Html;
                out.push(Ev::Open {
                    ns: ns_of(node.namespace),
                    name: name.clone(),
                    attrs,
                });
                open.push(row.depth);
                if scripting && html && name == "noscript" {
                    if let Some(content) = node.content_span() {
                        out.push(Ev::Text(preprocess(doc.slice(&content))));
                    }
                    skip_deeper_than = Some(row.depth);
                }
            }
            NodeKind::Text => out.push(Ev::Text(row.text.clone().unwrap_or_default())),
            NodeKind::Raw => {
                let owner = node.parent.and_then(|p| doc.node(p).map(|n| n.content));
                let slice = preprocess(doc.slice(&row.span));
                if owner == Some(ContentModel::Rcdata) {
                    out.push(Ev::Text(decode_entities(&slice).text));
                } else {
                    out.push(Ev::Text(slice));
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
    while open.pop().is_some() {
        out.push(Ev::Close);
    }
    out.0
}

/// Total attributes the crate lists across all elements (duplicates included).
pub fn model_attribute_count(doc: &WebDoc) -> usize {
    doc.view().iter().map(|r| r.attrs.len()).sum()
}
