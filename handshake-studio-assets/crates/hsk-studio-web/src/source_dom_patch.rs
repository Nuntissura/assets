//! Visual-edit operations as source patches (STU-WEB-003/004): every structural edit is a list of text
//! edits that rewrite only the affected spans, so untouched bytes (line endings, indentation, quoting,
//! tag case, comments, opaque constructs) stay identical. Patches are bound to one source revision and
//! its SHA-256; applying one to any other revision is a typed `StaleRevision` or `Conflict`.

#![deny(clippy::arithmetic_side_effects)]

use crate::dom::{
    ContentModel, NodeId, NodeKey, NodeKind, Quote, RAWTEXT, VOID, WebDoc, find_raw_text_close,
    in_set, parse, sha256_hex,
};
use crate::entity::{escape_attr, escape_text};
use crate::error::{EditRefusal, LimitKind, LossKind, MalformedKind, UnsupportedKind, WebError};
use hsk_studio_accord::CancellationToken;
use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub span: Range<usize>,
    pub replacement: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcePatch {
    pub base_revision: u64,
    pub base_sha256_hex: String,
    pub edits: Vec<TextEdit>,
}
impl SourcePatch {
    pub fn new(doc: &WebDoc, edits: Vec<TextEdit>) -> Self {
        Self {
            base_revision: doc.revision(),
            base_sha256_hex: doc.sha256_hex().to_owned(),
            edits,
        }
    }

    /// A patch whose edits change nothing (setting a value to what it already is).
    pub fn is_noop(&self) -> bool {
        self.edits.is_empty()
    }

    /// Combine two patches made against the same revision into one; overlap is rejected by `apply`.
    pub fn compose(mut self, other: SourcePatch) -> Result<SourcePatch, WebError> {
        if self.base_revision != other.base_revision {
            return Err(WebError::StaleRevision {
                expected: self.base_revision,
                found: other.base_revision,
            });
        }
        if self.base_sha256_hex != other.base_sha256_hex {
            return Err(WebError::Conflict {
                span: other.edits.first().map_or(0..0, |e| e.span.clone()),
            });
        }
        self.edits.extend(other.edits);
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub source: String,
    pub revision: u64,
    pub sha256_hex: String,
    /// Spans in the NEW source holding replacement text, in source order.
    pub changed: Vec<Range<usize>>,
}

/// Apply a patch to the document it was made against. A patch with no edits keeps the revision.
pub fn apply(doc: &WebDoc, patch: &SourcePatch) -> Result<Applied, WebError> {
    if patch.base_revision != doc.revision() {
        return Err(WebError::StaleRevision {
            expected: doc.revision(),
            found: patch.base_revision,
        });
    }
    if patch.base_sha256_hex != doc.sha256_hex() {
        return Err(WebError::Conflict {
            span: patch.edits.first().map_or(0..0, |e| e.span.clone()),
        });
    }
    let src = doc.source();
    if patch.edits.is_empty() {
        return Ok(Applied {
            source: src.to_owned(),
            revision: doc.revision(),
            sha256_hex: doc.sha256_hex().to_owned(),
            changed: Vec::new(),
        });
    }
    if patch.edits.len() > doc.limits().max_edits {
        return Err(WebError::LimitExceeded {
            limit: LimitKind::Edits,
            span: 0..0,
        });
    }
    let mut order: Vec<&TextEdit> = patch.edits.iter().collect();
    order.sort_by_key(|e| (e.span.start, e.span.end));
    for e in &order {
        if e.span.start > e.span.end
            || e.span.end > src.len()
            || !src.is_char_boundary(e.span.start)
            || !src.is_char_boundary(e.span.end)
        {
            return Err(WebError::InvalidSpan {
                span: e.span.clone(),
            });
        }
    }
    for pair in order.windows(2) {
        if pair[0].span.end > pair[1].span.start {
            return Err(WebError::OverlappingEdits {
                first: pair[0].span.clone(),
                second: pair[1].span.clone(),
            });
        }
    }
    let revision = doc
        .revision()
        .checked_add(1)
        .ok_or(WebError::RevisionOverflow)?;
    let mut out = String::with_capacity(src.len());
    let mut changed = Vec::with_capacity(order.len());
    let mut cursor = 0;
    for e in &order {
        out.push_str(&src[cursor..e.span.start]);
        let start = out.len();
        out.push_str(&e.replacement);
        changed.push(start..out.len());
        cursor = e.span.end;
        if out.len() > doc.limits().max_bytes {
            return Err(WebError::LimitExceeded {
                limit: LimitKind::Bytes,
                span: e.span.clone(),
            });
        }
    }
    out.push_str(&src[cursor..]);
    if out.len() > doc.limits().max_bytes {
        return Err(WebError::LimitExceeded {
            limit: LimitKind::Bytes,
            span: 0..src.len(),
        });
    }
    let sha256_hex = sha256_hex(out.as_bytes());
    Ok(Applied {
        source: out,
        revision,
        sha256_hex,
        changed,
    })
}

impl WebDoc {
    /// Apply a patch and re-project the result at the new revision (`parse(apply(..))`).
    pub fn apply_patch(
        &self,
        patch: &SourcePatch,
        cancel: &CancellationToken,
    ) -> Result<WebDoc, WebError> {
        let applied = apply(self, patch)?;
        parse(&applied.source, applied.revision, self.limits(), cancel)
    }
}

fn valid_attr_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 128
        && matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == ':')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '.' | '-'))
}

fn valid_tag_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 64
        && matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | ':'))
}

fn element(doc: &WebDoc, key: &NodeKey) -> Result<NodeId, WebError> {
    let id = doc.resolve(key).ok_or(WebError::NoSuchNode)?;
    match doc.node(id).ok_or(WebError::NoSuchNode)?.kind {
        NodeKind::Element => Ok(id),
        NodeKind::Document => Err(WebError::NotEditable {
            reason: EditRefusal::DocumentRoot,
        }),
        NodeKind::Unsupported => Err(WebError::NotEditable {
            reason: EditRefusal::Opaque,
        }),
        NodeKind::Raw => Err(WebError::NotEditable {
            reason: EditRefusal::RawChild,
        }),
        _ => Err(WebError::NotEditable {
            reason: EditRefusal::NotAnElement,
        }),
    }
}

fn unquoted_safe(value: &str) -> bool {
    !value.is_empty()
        && !value.ends_with('/')
        && !value
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '=' | '<' | '>' | '`'))
}

fn single(doc: &WebDoc, span: Range<usize>, replacement: String) -> SourcePatch {
    if doc.source()[span.clone()] == replacement {
        return SourcePatch::new(doc, Vec::new());
    }
    SourcePatch::new(doc, vec![TextEdit { span, replacement }])
}

/// Set an attribute from its decoded value. An existing attribute keeps its quote style (an unquoted value
/// that needs quoting becomes double-quoted); a missing one is appended after the last attribute as
/// ` name="value"`. Only the value span (or the insertion point) changes.
pub fn set_attribute(
    doc: &WebDoc,
    key: &NodeKey,
    name: &str,
    value: &str,
) -> Result<SourcePatch, WebError> {
    if !valid_attr_name(name) {
        return Err(WebError::InvalidName);
    }
    let id = element(doc, key)?;
    let node = doc.node(id).ok_or(WebError::NoSuchNode)?;
    if let Some(attr) = doc.attribute(id, name) {
        return Ok(match (&attr.value_span, attr.quote) {
            (Some(vs), Quote::Double) => single(doc, vs.clone(), escape_attr(value, '"')),
            (Some(vs), Quote::Single) => single(doc, vs.clone(), escape_attr(value, '\'')),
            (Some(vs), _) if unquoted_safe(value) => {
                single(doc, vs.clone(), escape_attr(value, '"'))
            }
            (Some(vs), _) => single(doc, vs.clone(), format!("\"{}\"", escape_attr(value, '"'))),
            (None, _) => {
                let at = attr.name_span.end;
                SourcePatch::new(
                    doc,
                    vec![TextEdit {
                        span: at..at,
                        replacement: format!("=\"{}\"", escape_attr(value, '"')),
                    }],
                )
            }
        });
    }
    let at = node
        .attrs
        .last()
        .map(|a| a.span.end)
        .or(node.name_span.as_ref().map(|s| s.end))
        .ok_or(WebError::NoSuchNode)?;
    Ok(SourcePatch::new(
        doc,
        vec![TextEdit {
            span: at..at,
            replacement: format!(" {name}=\"{}\"", escape_attr(value, '"')),
        }],
    ))
}

/// Remove an attribute together with the whitespace that separated it from the previous token.
pub fn remove_attribute(doc: &WebDoc, key: &NodeKey, name: &str) -> Result<SourcePatch, WebError> {
    let id = element(doc, key)?;
    let node = doc.node(id).ok_or(WebError::NoSuchNode)?;
    let attr = doc.attribute(id, name).ok_or(WebError::NoSuchAttribute)?;
    let position = node
        .attrs
        .iter()
        .position(|a| a == attr)
        .ok_or(WebError::NoSuchAttribute)?;
    let from = if position == 0 {
        node.name_span
            .as_ref()
            .map(|s| s.end)
            .ok_or(WebError::NoSuchNode)?
    } else {
        node.attrs[position.saturating_sub(1)].span.end
    };
    Ok(SourcePatch::new(
        doc,
        vec![TextEdit {
            span: from..attr.span.end,
            replacement: String::new(),
        }],
    ))
}

/// True when `text` as the body of raw-text element `name` would not end exactly at the element's own close tag: an
/// earlier appropriate end tag, or (script data) an unfinished double-escaped `<!--<script>` section that makes the
/// close tag part of the script.
fn closes_raw(text: &str, name: &str) -> bool {
    let body = format!("{text}</{name}>");
    find_raw_text_close(body.as_bytes(), 0, name) != Some(text.len())
}

/// Text written as the first token of a `WebDoc::drops_first_lf` element: one extra LF in front of a value that starts
/// with LF, so the value reads back unchanged (13.2.6.4.7 "pre, listing", "textarea").
fn keep_first_lf(mut written: String) -> String {
    if written.starts_with('\n') {
        written.insert(0, '\n');
    }
    written
}

/// Set the text of a text node, or the text content of an element that has only text children. Text is
/// given decoded and escaped on write (`& < >`, CR as `&#13;`); a value written as the first token after an HTML `pre`,
/// `listing` or `textarea` start tag (only `</>`, which emits no token, may precede it) that starts with LF gets one extra
/// LF (the tree builder drops the first). Raw-text elements (`script`, `style`, ...) take it verbatim unless it would
/// close the element (RAWTEXT has no references, so a CR or U+0000 there reads back as LF or U+FFFD). Elements with
/// markup children, void and opaque nodes are refused. Neighbours are not rewritten: an edit that changes which node comes
/// first after such a start tag (`insert_*` at that position, `remove_node` of the first child) changes whether a
/// following text's leading LF is dropped, exactly as a browser reads the new source.
pub fn set_text(doc: &WebDoc, key: &NodeKey, text: &str) -> Result<SourcePatch, WebError> {
    let id = doc.resolve(key).ok_or(WebError::NoSuchNode)?;
    let node = doc.node(id).ok_or(WebError::NoSuchNode)?;
    match node.kind {
        NodeKind::Text => {
            let first_in_pre = node.parent.is_some_and(|p| doc.drops_first_lf(p))
                && doc.first_token_after_start_tag(id);
            let written = escape_text(text);
            let written = if first_in_pre {
                keep_first_lf(written)
            } else {
                written
            };
            Ok(single(doc, node.span.clone(), written))
        }
        NodeKind::Element => {
            let Some(content) = node.content_span() else {
                return Err(WebError::NotEditable {
                    reason: EditRefusal::Void,
                });
            };
            let name = doc.tag_name(id).unwrap_or("");
            let replacement = match node.content {
                ContentModel::RawText => {
                    if closes_raw(text, name) {
                        return Err(WebError::NotEditable {
                            reason: EditRefusal::UnsafeRawText,
                        });
                    }
                    text.to_owned()
                }
                ContentModel::Rcdata => {
                    if closes_raw(text, name) {
                        return Err(WebError::NotEditable {
                            reason: EditRefusal::UnsafeRawText,
                        });
                    }
                    escape_text(text)
                }
                _ => {
                    let only_text = node
                        .children
                        .iter()
                        .all(|&c| doc.node(c).is_some_and(|n| n.kind == NodeKind::Text));
                    if !only_text {
                        return Err(WebError::NotEditable {
                            reason: EditRefusal::HasMarkupChildren,
                        });
                    }
                    escape_text(text)
                }
            };
            let replacement = if doc.drops_first_lf(id) {
                keep_first_lf(replacement)
            } else {
                replacement
            };
            Ok(single(doc, content, replacement))
        }
        NodeKind::Document => Err(WebError::NotEditable {
            reason: EditRefusal::DocumentRoot,
        }),
        NodeKind::Unsupported => Err(WebError::NotEditable {
            reason: EditRefusal::Opaque,
        }),
        NodeKind::Raw => Err(WebError::NotEditable {
            reason: EditRefusal::RawChild,
        }),
        NodeKind::Comment | NodeKind::Doctype => Err(WebError::NotEditable {
            reason: EditRefusal::NotAnElement,
        }),
    }
}

/// Remove a node (an element with its children, a text node or a comment): exactly its span is deleted.
pub fn remove_node(doc: &WebDoc, key: &NodeKey) -> Result<SourcePatch, WebError> {
    let id = doc.resolve(key).ok_or(WebError::NoSuchNode)?;
    let node = doc.node(id).ok_or(WebError::NoSuchNode)?;
    match node.kind {
        NodeKind::Document => Err(WebError::NotEditable {
            reason: EditRefusal::DocumentRoot,
        }),
        NodeKind::Unsupported => Err(WebError::NotEditable {
            reason: EditRefusal::Opaque,
        }),
        NodeKind::Raw => Err(WebError::NotEditable {
            reason: EditRefusal::RawChild,
        }),
        _ => Ok(SourcePatch::new(
            doc,
            vec![TextEdit {
                span: node.span.clone(),
                replacement: String::new(),
            }],
        )),
    }
}

fn check_fragment(
    doc: &WebDoc,
    fragment: &str,
    cancel: &CancellationToken,
) -> Result<WebDoc, WebError> {
    let parsed = match parse(fragment, 0, doc.limits(), cancel) {
        Ok(parsed) => parsed,
        Err(WebError::Canceled) => return Err(WebError::Canceled),
        Err(_) => {
            return Err(WebError::Malformed {
                kind: MalformedKind::InvalidFragment,
                span: 0..fragment.len(),
            });
        }
    };
    let opaque = parsed
        .loss()
        .entries()
        .iter()
        .any(|e| e.kind.is_opaque() || e.kind == LossKind::SelfClosingIgnored);
    if opaque {
        return Err(WebError::Unsupported {
            kind: UnsupportedKind::FragmentConstruct,
            span: 0..fragment.len(),
        });
    }
    Ok(parsed)
}

fn insertion_point(doc: &WebDoc, parent: &NodeKey, index: usize) -> Result<usize, WebError> {
    let id = doc.resolve(parent).ok_or(WebError::NoSuchNode)?;
    let node = doc.node(id).ok_or(WebError::NoSuchNode)?;
    match node.kind {
        NodeKind::Document => {}
        NodeKind::Element => {
            if node.content != ContentModel::Normal || node.close_span.is_none() {
                return Err(WebError::NotEditable {
                    reason: EditRefusal::Void,
                });
            }
        }
        NodeKind::Unsupported => {
            return Err(WebError::NotEditable {
                reason: EditRefusal::Opaque,
            });
        }
        _ => {
            return Err(WebError::NotEditable {
                reason: EditRefusal::NotAnElement,
            });
        }
    }
    match index.cmp(&node.children.len()) {
        std::cmp::Ordering::Less => Ok(doc
            .node(node.children[index])
            .ok_or(WebError::NoSuchNode)?
            .span
            .start),
        std::cmp::Ordering::Equal => match node.kind {
            NodeKind::Document => Ok(doc.source().len()),
            _ => Ok(node.close_span.as_ref().ok_or(WebError::NoSuchNode)?.start),
        },
        std::cmp::Ordering::Greater => Err(WebError::NotEditable {
            reason: EditRefusal::IndexOutOfRange,
        }),
    }
}

/// Insert markup as child number `index` of `parent` (all node kinds count; `index == children` appends
/// before the close tag). The fragment must itself parse inside the subset, so a patch can never unbalance
/// the document. The fragment text is inserted verbatim.
pub fn insert_fragment(
    doc: &WebDoc,
    parent: &NodeKey,
    index: usize,
    fragment: &str,
    cancel: &CancellationToken,
) -> Result<SourcePatch, WebError> {
    let at = insertion_point(doc, parent, index)?;
    check_fragment(doc, fragment, cancel)?;
    Ok(SourcePatch::new(
        doc,
        vec![TextEdit {
            span: at..at,
            replacement: fragment.to_owned(),
        }],
    ))
}

/// Insert a new element built from a tag, decoded attribute values and optional text (escaped on write;
/// verbatim for raw-text tags, which then must not close themselves early). The tag must be a single
/// well-formed element after parsing. How the text is written follows the destination (13.2.6 dispatcher,
/// `WebDoc::child_follows_html_rules`): under the HTML rules a raw-text tag takes it verbatim and an HTML `pre`,
/// `listing` or `textarea` gets one extra LF before a value that starts with LF; in foreign content (an `svg` or `math`
/// parent that is not an integration point) the new element is a foreign element, so the text is escaped and no LF is
/// added (a foreign `textarea` keeps its first LF). Proof: `insert_into_foreign_content_follows_the_foreign_rules`,
/// `edits_write_values_that_read_back_exactly`.
pub fn insert_element(
    doc: &WebDoc,
    parent: &NodeKey,
    index: usize,
    tag: &str,
    attrs: &[(&str, &str)],
    text: Option<&str>,
    cancel: &CancellationToken,
) -> Result<SourcePatch, WebError> {
    if !valid_tag_name(tag) || attrs.iter().any(|(n, _)| !valid_attr_name(n)) {
        return Err(WebError::InvalidName);
    }
    let void = in_set(VOID, tag);
    if void && text.is_some() {
        return Err(WebError::NotEditable {
            reason: EditRefusal::Void,
        });
    }
    let parent_id = doc.resolve(parent).ok_or(WebError::NoSuchNode)?;
    // The new element is an HTML element only under the HTML rules (an `svg`/`math` tag starts foreign content).
    let html = doc.child_follows_html_rules(parent_id, tag) && !in_set(&["svg", "math"], tag);
    let mut fragment = format!("<{tag}");
    for (name, value) in attrs {
        fragment.push_str(&format!(" {name}=\"{}\"", escape_attr(value, '"')));
    }
    fragment.push('>');
    if !void {
        let raw = html && in_set(RAWTEXT, tag);
        if let Some(text) = text {
            let written = if raw {
                text.to_owned()
            } else {
                escape_text(text)
            };
            // 13.2.6.4.7: an LF right after an HTML `pre`, `listing` or `textarea` start tag is dropped.
            fragment.push_str(&if html && in_set(&["pre", "listing", "textarea"], tag) {
                keep_first_lf(written)
            } else {
                written
            });
        }
        fragment.push_str(&format!("</{tag}>"));
    }
    let at = insertion_point(doc, parent, index)?;
    let parsed = check_fragment(doc, &fragment, cancel)?;
    let roots = &parsed
        .node(parsed.root())
        .ok_or(WebError::NoSuchNode)?
        .children;
    let one_element = roots.len() == 1
        && parsed
            .node(roots[0])
            .is_some_and(|n| n.kind == NodeKind::Element);
    if !one_element {
        return Err(WebError::Malformed {
            kind: MalformedKind::InvalidFragment,
            span: 0..fragment.len(),
        });
    }
    Ok(SourcePatch::new(
        doc,
        vec![TextEdit {
            span: at..at,
            replacement: fragment,
        }],
    ))
}
