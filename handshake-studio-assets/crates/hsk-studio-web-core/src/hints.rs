//! Code intelligence over one document (lane SMB-WEB-CODE-1; [STU-WEB-045] to [STU-WEB-051], [STU-WEB-059]): the
//! colouring-scheme record with its priority bands and exact embedded-block delimiters, region ownership receipts (what
//! language a byte range is in), code-hint records and groups built from the shipped vocabulary, the hint context at a
//! byte offset, and the navigation computations (balance braces, parent tag, fold ranges).
//!
//! Regions come from the HTML tokenizer's own spans (`WebDoc`): a `script` or `style` body and a comment are exactly the
//! bytes the tokenizer gave that construct, so a boundary is never guessed by pattern. Inside a CSS or script region,
//! brackets are matched by a tokenizer that skips strings and comments (a `/` that starts a script regular expression is
//! not recognised: brackets inside one can mislead balance-braces, stated). The shipped schemes are Studio's five
//! (`html`, `css`, `javascript`, `html_comment`, `text`): not the vendor's 33 schemes, 46 keyword lists or 271 token
//! classes ([STU-WEB-048] sizing, not reached); no theme or token-class palette here ([STU-WEB-049], not done). Hint
//! groups carry no item descriptions yet, so every group reports `described: false` ([STU-WEB-051] coverage flag).
//! Fold, expand-all and word-wrap are view state: these functions return the ranges; the view keeps them.
#![deny(clippy::arithmetic_side_effects)]

use crate::css_properties::{self, CURATED, Opt};
use crate::doc_types::CodeError;
use crate::dom::{ContentModel, Namespace, NodeId, NodeKind, WebDoc};
use crate::vocabulary::{TagRegistry, ValueType};
use std::ops::Range;

/// [STU-WEB-046] priority bands.
pub const BAND_TEXT: u8 = 1;
pub const BAND_WRAPPER: u8 = 5;
pub const BAND_COMMENT: u8 = 10;
pub const BAND_EMBEDDED: u8 = 20;
pub const BAND_DOCUMENT: u8 = 50;

/// [STU-WEB-047]: a delimiter pair opening a region owned by `scheme_id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockDelimiter {
    pub open: String,
    pub close: String,
    pub scheme_id: String,
}

/// One keyword list of a scheme.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeywordList {
    pub list_id: String,
    pub token_class: String,
    pub keywords: Vec<String>,
}

/// [STU-WEB-045]: one colouring scheme.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColouringScheme {
    pub scheme_id: String,
    pub display_name: String,
    pub applies_to_document_types: Vec<String>,
    pub priority: u8,
    pub ignore_case: bool,
    pub ignore_tags: bool,
    pub keyword_lists: Vec<KeywordList>,
    pub delimiters: Vec<(String, String)>,
    pub embedded_block_delimiters: Vec<BlockDelimiter>,
    pub tag_specs: Vec<String>,
    pub sample_text: String,
}

const MAX_SCHEMES: usize = 128;
const MAX_KEYWORDS: usize = 8192;

/// The JavaScript reserved words (ECMA-262 12.7.2).
const JS_KEYWORDS: &[&str] = &[
    "await", "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete", "do", "else", "enum", "export", "extends", "false", "finally", "for", "function", "if", "import", "in", "instanceof", "new", "null",
    "return", "super", "switch", "this", "throw", "true", "try", "typeof", "var", "void", "while", "with", "yield",
];

/// The registered schemes ([STU-WEB-045] to [STU-WEB-047]).
#[derive(Clone, Debug)]
pub struct SchemeRegistry {
    schemes: Vec<ColouringScheme>,
}

impl SchemeRegistry {
    /// Studio's shipped schemes (module docs).
    pub fn shipped() -> Self {
        let s = |id: &str, name: &str, types: &[&str], priority: u8, ignore_case: bool, lists: Vec<KeywordList>, blocks: Vec<BlockDelimiter>, sample: &str| ColouringScheme {
            scheme_id: id.into(),
            display_name: name.into(),
            applies_to_document_types: types.iter().map(|t| (*t).to_string()).collect(),
            priority,
            ignore_case,
            ignore_tags: false,
            keyword_lists: lists,
            delimiters: Vec::new(),
            embedded_block_delimiters: blocks,
            tag_specs: Vec::new(),
            sample_text: sample.into(),
        };
        let block = |open: &str, close: &str, scheme: &str| BlockDelimiter { open: open.into(), close: close.into(), scheme_id: scheme.into() };
        let html_types: Vec<&str> = crate::doc_types::DOCUMENT_TYPES.iter().filter(|t| t.dtd_context == Some(crate::doc_types::DtdContext::Html) || t.type_id == "library_item").map(|t| t.type_id).collect();
        Self {
            schemes: vec![
                s("html", "HTML", &html_types, BAND_DOCUMENT, true, Vec::new(), vec![block("<script", "</script>", "javascript"), block("<style", "</style>", "css"), block("<!--", "-->", "html_comment")], "<p class=\"a\">text</p>"),
                s(
                    "css",
                    "CSS",
                    &["css", "less", "scss", "sass"],
                    BAND_DOCUMENT,
                    true,
                    vec![KeywordList { list_id: "properties".into(), token_class: "css_property".into(), keywords: css_properties::recognised_properties() }],
                    Vec::new(),
                    "p { color: red; }",
                ),
                s(
                    "javascript",
                    "JavaScript",
                    &["javascript", "json"],
                    BAND_DOCUMENT,
                    false,
                    vec![KeywordList { list_id: "reserved".into(), token_class: "js_keyword".into(), keywords: JS_KEYWORDS.iter().map(|k| (*k).to_string()).collect() }],
                    Vec::new(),
                    "const a = 1;",
                ),
                s("html_comment", "Comment", &[], BAND_COMMENT, false, Vec::new(), Vec::new(), "<!-- note -->"),
                s("text", "Text", &["text"], BAND_TEXT, false, Vec::new(), Vec::new(), "text"),
            ],
        }
    }

    pub fn schemes(&self) -> &[ColouringScheme] {
        &self.schemes
    }

    pub fn scheme(&self, id: &str) -> Option<&ColouringScheme> {
        self.schemes.iter().find(|s| s.scheme_id == id)
    }

    /// Registers a scheme. Refused typed: an id already present, an embedded block naming no scheme, or an ambiguous
    /// boundary (two pairs of one scheme opening on the same delimiter, compared case-folded when the scheme ignores
    /// case) — diagnosed here, never silently shadowed ([STU-WEB-046]).
    pub fn register(&mut self, scheme: ColouringScheme) -> Result<(), CodeError> {
        if self.schemes.len() >= MAX_SCHEMES || scheme.keyword_lists.iter().map(|l| l.keywords.len()).fold(0usize, usize::saturating_add) > MAX_KEYWORDS {
            return Err(CodeError::new("scheme_too_large"));
        }
        if scheme.scheme_id.is_empty() || self.scheme(&scheme.scheme_id).is_some() {
            return Err(CodeError::new("scheme_id_taken"));
        }
        let fold = |s: &str| if scheme.ignore_case { s.to_ascii_lowercase() } else { s.to_string() };
        for (i, d) in scheme.embedded_block_delimiters.iter().enumerate() {
            if d.open.is_empty() || d.close.is_empty() {
                return Err(CodeError::new("delimiter_empty"));
            }
            if d.scheme_id != scheme.scheme_id && self.scheme(&d.scheme_id).is_none() {
                return Err(CodeError::new("delimiter_scheme_unknown"));
            }
            if scheme.embedded_block_delimiters.iter().take(i).any(|o| fold(&o.open) == fold(&d.open)) {
                return Err(CodeError::new("delimiter_ambiguous"));
            }
        }
        self.schemes.push(scheme);
        Ok(())
    }
}

/// [STU-WEB-046]: one region-ownership receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub range: Range<usize>,
    /// The eligible schemes, in decision order (priority ascending, then id).
    pub candidates: Vec<String>,
    /// `document_language`, `embedded_block` or `comment`.
    pub decision: &'static str,
    pub open_delimiter: Option<String>,
    pub close_delimiter: Option<String>,
    pub scheme_id: String,
    pub enclosing: Option<String>,
}

/// The scheme a `script` element's body is: script for no `type`, a JavaScript MIME type or `module`; otherwise text
/// (a data block the browser does not run).
fn script_scheme(doc: &WebDoc, id: NodeId) -> &'static str {
    let ty = doc.attribute(id, "type").and_then(|a| doc.attr_value(a)).unwrap_or_default();
    let ty = ty.trim().to_ascii_lowercase();
    let js = ty.is_empty() || ty == "module" || matches!(ty.as_str(), "text/javascript" | "application/javascript" | "application/ecmascript" | "text/ecmascript" | "application/x-javascript" | "text/jscript");
    if js { "javascript" } else { "text" }
}

/// A region a construct of the markup owns.
struct Owned {
    range: Range<usize>,
    scheme: &'static str,
    decision: &'static str,
    delim: Option<BlockDelimiter>,
}

/// [STU-WEB-046], [STU-WEB-047]: the regions of an HTML-family document in byte order, covering it.
pub fn regions(doc: &WebDoc, registry: &SchemeRegistry) -> Vec<Region> {
    let html = registry.scheme("html");
    let mut owned: Vec<Owned> = Vec::new();
    for id in 0..doc.len() {
        let Some(node) = doc.node(id) else { continue };
        match node.kind {
            NodeKind::Comment => owned.push(Owned { range: node.span.clone(), scheme: "html_comment", decision: "comment", delim: html.and_then(|h| h.embedded_block_delimiters.iter().find(|d| d.scheme_id == "html_comment").cloned()) }),
            NodeKind::Element if node.namespace == Namespace::Html && node.content == ContentModel::RawText => {
                let name = doc.local_name(id).unwrap_or_default();
                let scheme = match name.as_str() {
                    "script" => script_scheme(doc, id),
                    "style" => "css",
                    _ => continue,
                };
                let Some(content) = node.content_span() else { continue };
                if content.is_empty() {
                    continue;
                }
                let delim = html.and_then(|h| h.embedded_block_delimiters.iter().find(|d| d.open.eq_ignore_ascii_case(&format!("<{name}"))).cloned());
                owned.push(Owned { range: content, scheme, decision: "embedded_block", delim });
            }
            _ => {}
        }
    }
    owned.sort_by_key(|o| o.range.start);
    let mut out = Vec::new();
    let mut cursor = 0usize;
    let len = doc.source().len();
    let top = |range: Range<usize>| Region { range, candidates: vec!["html".into()], decision: "document_language", open_delimiter: None, close_delimiter: None, scheme_id: "html".into(), enclosing: None };
    for Owned { range, scheme, decision, delim } in owned {
        if range.start < cursor {
            continue;
        }
        if range.start > cursor {
            out.push(top(cursor..range.start));
        }
        // A data-block script is text: band 1 is eligible only because no language owns it.
        cursor = range.end;
        out.push(Region { range, candidates: vec![scheme.to_string()], decision, open_delimiter: delim.as_ref().map(|d| d.open.clone()), close_delimiter: delim.map(|d| d.close), scheme_id: scheme.to_string(), enclosing: Some("html".into()) });
    }
    if cursor < len || out.is_empty() {
        out.push(top(cursor..len));
    }
    out
}

/// The region holding byte `offset` (the last region for the end of the document).
pub fn region_at(regions: &[Region], offset: usize) -> Option<&Region> {
    regions.iter().find(|r| r.range.contains(&offset)).or_else(|| regions.last().filter(|r| r.range.end == offset))
}

/// [STU-WEB-050]: one hint item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HintItem {
    pub label: String,
    pub value: String,
    pub icon_id: &'static str,
    pub description: Option<String>,
}

/// [STU-WEB-050]: one code hint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeHint {
    pub pattern: String,
    pub additional_dismiss_chars: String,
    pub allow_whitespace_prefix: bool,
    pub display_restriction: &'static str,
    pub allow_multiple_values: bool,
    pub items: Vec<HintItem>,
}

/// [STU-WEB-051]: a menu group of hints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HintGroup {
    pub group_id: &'static str,
    pub hints: Vec<CodeHint>,
}

impl HintGroup {
    /// Whether any item carries a description (a group without any is legal and flagged in coverage).
    pub fn described(&self) -> bool {
        self.hints.iter().any(|h| h.items.iter().any(|i| i.description.is_some()))
    }

    pub fn item_count(&self) -> usize {
        self.hints.iter().map(|h| h.items.len()).fold(0usize, usize::saturating_add)
    }
}

fn item(label: &str, icon_id: &'static str) -> HintItem {
    HintItem { label: label.to_string(), value: label.to_string(), icon_id, description: None }
}

/// The value hint of a curated property: its keyword options in declared order (units are typed after a number).
fn css_value_hint(p: &css_properties::PropertyEntry) -> CodeHint {
    let items = p.options.iter().filter_map(|o| if let Opt::Keyword(k) = o { Some(item(k, "css_value")) } else { None }).collect();
    CodeHint {
        pattern: format!("{}:", p.property),
        additional_dismiss_chars: ";}".into(),
        allow_whitespace_prefix: true,
        display_restriction: "css",
        allow_multiple_values: matches!(p.control_type, css_properties::ControlType::GroupedBoxControls | css_properties::ControlType::DoubleMenuAndHotText | css_properties::ControlType::MultiControlLayout | css_properties::ControlType::CompositeControlLayout | css_properties::ControlType::GroupedBorderControls),
        items,
    }
}

/// [STU-WEB-050], [STU-WEB-051]: the hint groups for a document type, built from the vocabulary (module docs).
pub fn hint_groups(vocabulary: &TagRegistry, type_id: &str) -> Vec<HintGroup> {
    let tags = vocabulary.tags_for(type_id);
    let tag_items: Vec<HintItem> = tags.iter().map(|t| item(&t.tag_name, "html_tag")).collect();
    let mut attribute_hints = Vec::new();
    let mut value_hints = Vec::new();
    for t in &tags {
        let attrs = vocabulary.attributes_of(t);
        attribute_hints.push(CodeHint { pattern: format!("<{} ", t.tag_name), additional_dismiss_chars: ">/".into(), allow_whitespace_prefix: true, display_restriction: "html", allow_multiple_values: true, items: attrs.iter().map(|a| item(&a.name, "html_attribute")).collect() });
        for a in attrs.iter().filter(|a| a.value_type == ValueType::Enumerated) {
            value_hints.push(CodeHint {
                pattern: format!("<{} {}=\"", t.tag_name, a.name),
                additional_dismiss_chars: "\"".into(),
                allow_whitespace_prefix: false,
                display_restriction: "html",
                allow_multiple_values: false,
                items: a.allowed_values.iter().map(|v| HintItem { label: v.caption.clone().unwrap_or_else(|| v.value.clone()), value: v.value.clone(), icon_id: "html_value", description: None }).collect(),
            });
        }
    }
    let properties: Vec<HintItem> = css_properties::recognised_properties().iter().map(|p| item(p, "css_property")).collect();
    vec![
        HintGroup { group_id: "html_tags", hints: vec![CodeHint { pattern: "<".into(), additional_dismiss_chars: " >".into(), allow_whitespace_prefix: false, display_restriction: "html", allow_multiple_values: false, items: tag_items }] },
        HintGroup { group_id: "html_attributes", hints: attribute_hints },
        HintGroup { group_id: "html_attribute_values", hints: value_hints },
        HintGroup { group_id: "css_properties", hints: ["{", ";"].iter().map(|p| CodeHint { pattern: (*p).into(), additional_dismiss_chars: ":".into(), allow_whitespace_prefix: true, display_restriction: "css", allow_multiple_values: false, items: properties.clone() }).collect() },
        HintGroup { group_id: "css_values", hints: CURATED.iter().map(css_value_hint).collect() },
    ]
}

/// Items listed in one hint reply.
pub const MAX_HINT_ITEMS: usize = 500;

/// The hint that applies at a byte offset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HintContext {
    pub group_id: &'static str,
    pub pattern: String,
    /// The word already typed before the offset (items are filtered by it, ASCII case-insensitively).
    pub prefix: String,
    pub scheme_id: String,
    pub allow_multiple_values: bool,
    pub items: Vec<HintItem>,
    pub total: usize,
}

fn context(group: &'static str, hint: &CodeHint, prefix: &str, scheme: &str) -> HintContext {
    let matching: Vec<&HintItem> = hint.items.iter().filter(|i| i.value.len() >= prefix.len() && i.value.get(..prefix.len()).is_some_and(|p| p.eq_ignore_ascii_case(prefix))).collect();
    HintContext { group_id: group, pattern: hint.pattern.clone(), prefix: prefix.to_string(), scheme_id: scheme.to_string(), allow_multiple_values: hint.allow_multiple_values, total: matching.len(), items: matching.into_iter().take(MAX_HINT_ITEMS).cloned().collect() }
}

/// The word of name characters ending at `offset`.
fn word_before(src: &str, offset: usize, name: impl Fn(u8) -> bool) -> (usize, &str) {
    let b = src.as_bytes();
    let mut start = offset;
    while start > 0 && name(b[start.saturating_sub(1)]) {
        start = start.saturating_sub(1);
    }
    (start, src.get(start..offset).unwrap_or(""))
}

/// [STU-WEB-050]: the hint at byte `offset` of `doc` (a document of `type_id`), `None` when no hint applies there.
pub fn hints_at(doc: &WebDoc, vocabulary: &TagRegistry, type_id: &str, offset: usize) -> Result<Option<HintContext>, CodeError> {
    let src = doc.source();
    if offset > src.len() || !src.is_char_boundary(offset) {
        return Err(CodeError::new("offset_invalid"));
    }
    let schemes = SchemeRegistry::shipped();
    let regions = regions(doc, &schemes);
    let region = region_at(&regions, offset).ok_or(CodeError::new("offset_invalid"))?;
    let groups = hint_groups(vocabulary, type_id);
    let group = |id: &str| groups.iter().find(|g| g.group_id == id);
    let css_name = |c: u8| c.is_ascii_alphanumeric() || c == b'-' || c == b'_';
    match region.scheme_id.as_str() {
        "css" => {
            // Inside a declaration value when the nearest `:` / `;` / `{` / `}` before the offset is a `:`.
            let b = src.as_bytes();
            let mut j = offset;
            while j > region.range.start && !matches!(b[j.saturating_sub(1)], b':' | b';' | b'{' | b'}') {
                j = j.saturating_sub(1);
            }
            let before = if j > region.range.start { b[j.saturating_sub(1)] } else { b'{' };
            let (_, prefix) = word_before(src, offset, css_name);
            if before == b':' {
                let (_, prop) = word_before(src, j.saturating_sub(1), css_name);
                let pattern = format!("{}:", prop.to_ascii_lowercase());
                return Ok(group("css_values").and_then(|g| g.hints.iter().find(|h| h.pattern == pattern)).map(|h| context("css_values", h, prefix, "css")));
            }
            let opener = if before == b';' { ";" } else { "{" };
            Ok(group("css_properties").and_then(|g| g.hints.iter().find(|h| h.pattern == opener)).map(|h| context("css_properties", h, prefix, "css")))
        }
        "html" => {
            // In a start tag: the tag name, an attribute name or an enumerated attribute's value.
            let element = (0..doc.len()).filter_map(|id| doc.node(id).map(|n| (id, n))).rfind(|(_, n)| n.kind == NodeKind::Element && n.open_span.as_ref().is_some_and(|s| s.start < offset && offset < s.end));
            let name_char = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b':' | b'.');
            if let Some((id, node)) = element {
                let tag = doc.local_name(id).unwrap_or_default();
                if node.name_span.as_ref().is_some_and(|s| offset <= s.end) {
                    let (_, prefix) = word_before(src, offset, name_char);
                    return Ok(group("html_tags").and_then(|g| g.hints.first()).map(|h| context("html_tags", h, prefix, "html")));
                }
                if let Some(attr) = node.attrs.iter().find(|a| a.value_span.as_ref().is_some_and(|v| v.start <= offset && offset <= v.end)) {
                    let name = doc.attr_local_name(attr);
                    let (_, prefix) = word_before(src, offset, |c| c != b'"' && c != b'\'' && c != b'=' && !c.is_ascii_whitespace());
                    let pattern = format!("<{tag} {name}=\"");
                    return Ok(group("html_attribute_values").and_then(|g| g.hints.iter().find(|h| h.pattern == pattern)).map(|h| context("html_attribute_values", h, prefix, "html")));
                }
                let (_, prefix) = word_before(src, offset, name_char);
                let pattern = format!("<{tag} ");
                return Ok(group("html_attributes").and_then(|g| g.hints.iter().find(|h| h.pattern == pattern)).map(|h| context("html_attributes", h, prefix, "html")));
            }
            // In text right after `<` and a partial name: a tag name.
            let (start, prefix) = word_before(src, offset, name_char);
            if start > 0 && src.as_bytes()[start.saturating_sub(1)] == b'<' {
                return Ok(group("html_tags").and_then(|g| g.hints.first()).map(|h| context("html_tags", h, prefix, "html")));
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Navigation ([STU-WEB-059]).

/// The innermost element whose span strictly contains `range` (an element whose span equals it does not count).
fn enclosing_element(doc: &WebDoc, range: &Range<usize>) -> Option<NodeId> {
    (0..doc.len())
        .filter(|&id| doc.node(id).is_some_and(|n| n.kind == NodeKind::Element && n.span.start <= range.start && range.end <= n.span.end && n.span != *range))
        .min_by_key(|&id| doc.node(id).map_or(usize::MAX, |n| n.span.len()))
}

/// Bracket pairs of a CSS or script region text, skipping strings and comments.
fn bracket_pairs(src: &str, region: &Range<usize>, script: bool) -> Vec<Range<usize>> {
    let b = src.as_bytes();
    let mut stack: Vec<(u8, usize)> = Vec::new();
    let mut out = Vec::new();
    let mut i = region.start;
    let end = region.end.min(b.len());
    while i < end {
        let c = b[i];
        match c {
            b'"' | b'\'' | b'`' if c != b'`' || script => {
                let mut j = i.saturating_add(1);
                while j < end && b[j] != c {
                    j = if b[j] == b'\\' { j.saturating_add(2) } else { j.saturating_add(1) };
                }
                i = j.saturating_add(1);
            }
            b'/' if b.get(i.saturating_add(1)) == Some(&b'*') => {
                let rest = src.get(i.saturating_add(2)..end).unwrap_or("");
                i = rest.find("*/").map_or(end, |p| i.saturating_add(2).saturating_add(p).saturating_add(2));
            }
            b'/' if script && b.get(i.saturating_add(1)) == Some(&b'/') => {
                let rest = src.get(i..end).unwrap_or("");
                i = rest.find('\n').map_or(end, |p| i.saturating_add(p));
            }
            b'{' | b'(' | b'[' => {
                stack.push((c, i));
                i = i.saturating_add(1);
            }
            b'}' | b')' | b']' => {
                let want = match c {
                    b'}' => b'{',
                    b')' => b'(',
                    _ => b'[',
                };
                if let Some(pos) = stack.iter().rposition(|(o, _)| *o == want) {
                    let (_, at) = stack[pos];
                    stack.truncate(pos);
                    out.push(at..i.saturating_add(1));
                }
                i = i.saturating_add(1);
            }
            _ => i = i.saturating_add(1),
        }
    }
    out
}

/// Balance braces: the smallest bracket pair (CSS or script region) or element (markup) strictly enclosing `range`.
pub fn balance(doc: &WebDoc, range: Range<usize>) -> Result<Range<usize>, CodeError> {
    let src = doc.source();
    if range.start > range.end || range.end > src.len() || !src.is_char_boundary(range.start) || !src.is_char_boundary(range.end) {
        return Err(CodeError::new("range_invalid"));
    }
    let schemes = SchemeRegistry::shipped();
    let regions = regions(doc, &schemes);
    if let Some(region) = region_at(&regions, range.start).filter(|r| matches!(r.scheme_id.as_str(), "css" | "javascript") && range.end <= r.range.end) {
        let pairs = bracket_pairs(src, &region.range, region.scheme_id == "javascript");
        if let Some(pair) = pairs.into_iter().filter(|p| p.start <= range.start && range.end <= p.end && *p != range).min_by_key(|p| p.len()) {
            return Ok(pair);
        }
    }
    enclosing_element(doc, &range).and_then(|id| doc.node(id).map(|n| n.span.clone())).ok_or(CodeError::new("nothing_encloses"))
}

/// Select parent tag: the parent element of element `id`.
pub fn parent_tag(doc: &WebDoc, id: NodeId) -> Result<NodeId, CodeError> {
    let node = doc.node(id).ok_or(CodeError::new("no_such_node"))?;
    let parent = node.parent.ok_or(CodeError::new("no_parent_tag"))?;
    match doc.node(parent) {
        Some(p) if p.kind == NodeKind::Element => Ok(parent),
        _ => Err(CodeError::new("no_parent_tag")),
    }
}

/// Collapse full tag: the fold range of element `id` (its whole span).
pub fn full_tag_range(doc: &WebDoc, id: NodeId) -> Result<Range<usize>, CodeError> {
    match doc.node(id) {
        Some(n) if n.kind == NodeKind::Element => Ok(n.span.clone()),
        Some(_) => Err(CodeError::new("not_an_element")),
        None => Err(CodeError::new("no_such_node")),
    }
}

/// Collapse selection: the range itself, checked (non-empty, on character boundaries, inside the source).
pub fn selection_range(doc: &WebDoc, range: Range<usize>) -> Result<Range<usize>, CodeError> {
    let src = doc.source();
    if range.start >= range.end || range.end > src.len() || !src.is_char_boundary(range.start) || !src.is_char_boundary(range.end) {
        return Err(CodeError::new("range_invalid"));
    }
    Ok(range)
}
