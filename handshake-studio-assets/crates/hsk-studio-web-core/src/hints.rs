//! Code intelligence over one document (lane SMB-WEB-CODE-1; [STU-WEB-045] to [STU-WEB-051], [STU-WEB-059]): the
//! colouring-scheme record with its priority bands and exact embedded-block delimiters, region ownership receipts (what
//! language a byte range is in), the CSS and script lexer, code-hint records and groups built from the shipped
//! vocabulary, the hint context at a byte offset, and the navigation computations (balance braces, parent tag, fold
//! ranges).
//!
//! Regions follow the document type (review F8): a CSS-family document is one `css` region, a script or JSON document
//! one `javascript` region, a text document one `text` region; an HTML-family document is owned by its top scheme and
//! its embedded blocks come from the HTML tokenizer's own spans (`WebDoc`): a `script` or `style` body and a comment are
//! exactly the bytes the tokenizer gave that construct, never a pattern guess. A registered scheme's embedded
//! delimiters own the opaque markup node that starts and ends with them exactly (for example a processing-instruction
//! style `<?php ... ?>` node), on equal terms with the shipped ones. An offset at the very end of an embedded block
//! (the cursor before `</style>`) is that block's.
//!
//! The lexer ([`lex`]) knows CSS comments, strings and escapes, and script block and line comments, strings, template
//! literals (with their `${}` expressions) and regular-expression literals (a `/` after an operator, a `(`, `,`, `=`,
//! `:`, `[`, `!`, `&`, `|`, `?`, `{`, `}`, `;` or one of the keywords that precede an expression starts one; after a
//! name, number, `)` or `]` it divides, the usual reading). The schemes shipped are Studio's five (`html`, `css`,
//! `javascript`, `html_comment`, `text`): not the vendor's 33 schemes, 46 keyword lists or 271 token classes
//! ([STU-WEB-048] sizing, not reached); no theme or token-class palette here ([STU-WEB-049], not done). Hint groups
//! carry no item descriptions yet, so every group reports `described: false` ([STU-WEB-051] coverage flag). Fold,
//! expand-all and word-wrap are view state: these functions return the ranges; the view keeps them.
#![deny(clippy::arithmetic_side_effects)]

use crate::css_properties::{self, CURATED, Opt};
use crate::doc_types::{CodeError, InternalKind, Work, document_type};
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

/// The reserved words of ECMA-262 (Ecma International; the crate's NOTICE).
const JS_KEYWORDS: &[&str] = &[
    "await", "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete", "do", "else", "enum", "export", "extends", "false", "finally", "for", "function", "if", "import", "in", "instanceof", "new", "null",
    "return", "super", "switch", "this", "throw", "true", "try", "typeof", "var", "void", "while", "with", "yield",
];

/// The JavaScript MIME type essences of the WHATWG MIME Sniffing standard (the crate's NOTICE): a `script` whose `type`
/// is one of them (or empty, or `module`) runs as script.
pub const JAVASCRIPT_MIME_ESSENCES: [&str; 16] = [
    "application/ecmascript",
    "application/javascript",
    "application/x-ecmascript",
    "application/x-javascript",
    "text/ecmascript",
    "text/javascript",
    "text/javascript1.0",
    "text/javascript1.1",
    "text/javascript1.2",
    "text/javascript1.3",
    "text/javascript1.4",
    "text/javascript1.5",
    "text/jscript",
    "text/livescript",
    "text/x-ecmascript",
    "text/x-javascript",
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
        let html_types: Vec<&str> = crate::doc_types::DOCUMENT_TYPES.iter().filter(|t| matches!(t.internal_kind, InternalKind::Html | InternalKind::Dynamic | InternalKind::Template) || t.type_id == "xslt_page").map(|t| t.type_id).collect();
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

    /// The schemes eligible for a whole document of `type_id`, in decision order ([STU-WEB-046]: priority ascending,
    /// then `scheme_id`; band-1 text only when no language applies).
    pub fn candidates(&self, type_id: &str) -> Vec<&ColouringScheme> {
        let mut list: Vec<&ColouringScheme> = self.schemes.iter().filter(|s| s.applies_to_document_types.iter().any(|t| t == type_id)).collect();
        if list.iter().any(|s| s.priority > BAND_TEXT) {
            list.retain(|s| s.priority > BAND_TEXT);
        }
        list.sort_by(|a, b| (a.priority, &a.scheme_id).cmp(&(b.priority, &b.scheme_id)));
        list
    }

    /// Registers a scheme. Refused typed: an id already present, an embedded block naming no scheme, or an ambiguous
    /// boundary: two pairs opening on the same delimiter (compared case-folded when either scheme ignores case) in one
    /// scheme, or in two schemes that apply to a common document type ([STU-WEB-046]) — diagnosed here, never silently
    /// shadowed.
    pub fn register(&mut self, scheme: ColouringScheme) -> Result<(), CodeError> {
        if self.schemes.len() >= MAX_SCHEMES || scheme.keyword_lists.iter().map(|l| l.keywords.len()).fold(0usize, usize::saturating_add) > MAX_KEYWORDS {
            return Err(CodeError::new("scheme_too_large"));
        }
        if scheme.scheme_id.is_empty() || self.scheme(&scheme.scheme_id).is_some() {
            return Err(CodeError::new("scheme_id_taken"));
        }
        let same = |a: &str, b: &str, fold: bool| if fold { a.eq_ignore_ascii_case(b) } else { a == b };
        for (i, d) in scheme.embedded_block_delimiters.iter().enumerate() {
            if d.open.is_empty() || d.close.is_empty() {
                return Err(CodeError::new("delimiter_empty"));
            }
            if d.scheme_id != scheme.scheme_id && self.scheme(&d.scheme_id).is_none() {
                return Err(CodeError::new("delimiter_scheme_unknown"));
            }
            if scheme.embedded_block_delimiters.iter().take(i).any(|o| same(&o.open, &d.open, scheme.ignore_case)) {
                return Err(CodeError::new("delimiter_ambiguous"));
            }
            let overlapping = self.schemes.iter().filter(|o| o.applies_to_document_types.iter().any(|t| scheme.applies_to_document_types.contains(t)));
            if overlapping.flat_map(|o| o.embedded_block_delimiters.iter().map(move |x| (o.ignore_case, x))).any(|(fold, x)| same(&x.open, &d.open, fold || scheme.ignore_case)) {
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

/// The scheme a `script` element's body is: script for no `type`, `module` or a JavaScript MIME type essence; otherwise
/// text (a data block the browser does not run).
fn script_scheme(doc: &WebDoc, id: NodeId) -> &'static str {
    let ty = doc.attribute(id, "type").and_then(|a| doc.attr_value(a)).unwrap_or_default();
    let ty = ty.trim_matches(|c: char| c.is_ascii_whitespace()).to_ascii_lowercase();
    let essence = ty.split(';').next().unwrap_or("").trim();
    if ty.is_empty() || ty == "module" || JAVASCRIPT_MIME_ESSENCES.contains(&essence) { "javascript" } else { "text" }
}

/// A region a construct of the markup owns.
struct Owned {
    range: Range<usize>,
    scheme: String,
    decision: &'static str,
    delim: Option<BlockDelimiter>,
}

/// [STU-WEB-046], [STU-WEB-047]: the regions of a document of `type_id`, in byte order, covering it (module docs).
pub fn regions(doc: &WebDoc, registry: &SchemeRegistry, type_id: &str, work: &mut Work<'_>) -> Result<Vec<Region>, CodeError> {
    let len = doc.source().len();
    // A type no language applies to is plain text (band 1).
    let mut candidates = registry.candidates(type_id);
    if candidates.is_empty() {
        candidates.extend(registry.scheme("text"));
    }
    let Some(top) = candidates.first() else {
        return Err(CodeError::new("type_has_no_scheme"));
    };
    let names: Vec<String> = candidates.iter().map(|s| s.scheme_id.clone()).collect();
    let markup = document_type(type_id).is_some_and(|t| matches!(t.internal_kind, InternalKind::Html | InternalKind::Dynamic | InternalKind::Template | InternalKind::Xslt) || t.type_id == "library_item");
    let whole = |range: Range<usize>| Region { range, candidates: names.clone(), decision: "document_language", open_delimiter: None, close_delimiter: None, scheme_id: top.scheme_id.clone(), enclosing: None };
    if !markup {
        work.tick(len)?;
        return Ok(vec![whole(0..len)]);
    }
    // Every eligible scheme's embedded delimiters, the winner's first.
    let delims: Vec<(&BlockDelimiter, bool)> = candidates.iter().flat_map(|s| s.embedded_block_delimiters.iter().map(move |d| (d, s.ignore_case))).collect();
    let find = |open: &str| delims.iter().find(|(d, _)| d.open.eq_ignore_ascii_case(open)).map(|(d, _)| (*d).clone());
    let mut owned: Vec<Owned> = Vec::new();
    for id in 0..doc.len() {
        work.tick(1)?;
        let Some(node) = doc.node(id) else { continue };
        match node.kind {
            NodeKind::Comment => {
                if let Some(d) = find("<!--") {
                    owned.push(Owned { range: node.span.clone(), scheme: d.scheme_id.clone(), decision: "comment", delim: Some(d) });
                }
            }
            NodeKind::Element if node.namespace == Namespace::Html && node.content == ContentModel::RawText => {
                let name = doc.local_name(id).unwrap_or_default();
                let Some(d) = find(&format!("<{name}")) else { continue };
                let scheme = if name == "script" && script_scheme(doc, id) == "text" { "text".to_string() } else { d.scheme_id.clone() };
                let Some(content) = node.content_span() else { continue };
                if content.is_empty() {
                    continue;
                }
                owned.push(Owned { range: content, scheme, decision: "embedded_block", delim: Some(d) });
            }
            NodeKind::Unsupported => {
                // A registered delimiter pair owns an opaque node that starts and ends with it exactly.
                let text = doc.slice(&node.span);
                let hit = delims.iter().find(|(d, fold)| {
                    let starts = text.get(..d.open.len()).is_some_and(|t| if *fold { t.eq_ignore_ascii_case(&d.open) } else { t == d.open });
                    let ends = text.len() >= d.open.len().saturating_add(d.close.len()) && text.get(text.len().saturating_sub(d.close.len())..).is_some_and(|t| if *fold { t.eq_ignore_ascii_case(&d.close) } else { t == d.close });
                    starts && ends
                });
                if let Some((d, _)) = hit {
                    let inner = node.span.start.saturating_add(d.open.len())..node.span.end.saturating_sub(d.close.len());
                    owned.push(Owned { range: inner, scheme: d.scheme_id.clone(), decision: "embedded_block", delim: Some((*d).clone()) });
                }
            }
            _ => {}
        }
    }
    owned.sort_by_key(|o| o.range.start);
    let mut out = Vec::new();
    let mut cursor = 0usize;
    for Owned { range, scheme, decision, delim } in owned {
        if range.start < cursor {
            continue;
        }
        if range.start > cursor {
            out.push(whole(cursor..range.start));
        }
        cursor = range.end;
        out.push(Region { range, candidates: vec![scheme.clone()], decision, open_delimiter: delim.as_ref().map(|d| d.open.clone()), close_delimiter: delim.map(|d| d.close), scheme_id: scheme, enclosing: Some(top.scheme_id.clone()) });
    }
    if cursor < len || out.is_empty() {
        out.push(whole(cursor..len));
    }
    Ok(out)
}

/// The region holding byte `offset`. An offset at the very end of an embedded block (the cursor before `</style>`)
/// is the block's (review F8); the end of the document is the last region's.
pub fn region_at(regions: &[Region], offset: usize) -> Option<&Region> {
    regions
        .iter()
        .find(|r| r.decision == "embedded_block" && r.range.start < offset && offset == r.range.end)
        .or_else(|| regions.iter().find(|r| r.range.contains(&offset)))
        .or_else(|| regions.last().filter(|r| r.range.end == offset))
}

/// The region that holds the whole of `range` (a non-empty selection).
pub fn region_of_range<'a>(regions: &'a [Region], range: &Range<usize>) -> Option<&'a Region> {
    regions.iter().find(|r| r.range.start <= range.start && range.end <= r.range.end)
}

// ---------------------------------------------------------------------------------------------------------------------
// The CSS and script lexer (review F1).

/// An opaque token: bytes in it are not code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    BlockComment,
    LineComment,
    String,
    Template,
    Regex,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub range: Range<usize>,
}

/// Keywords after which a `/` starts a regular expression.
const REGEX_AFTER: [&str; 13] = ["return", "typeof", "instanceof", "in", "of", "new", "delete", "void", "throw", "case", "do", "else", "yield"];

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80
}

/// The opaque tokens of `src[range]` as CSS (`script` false: comments and strings, backslash escapes skipped) or script
/// (comments, line comments, strings, template literals with their expressions, regular-expression literals), in
/// order. An unterminated token runs to the end of its line (strings) or of the range (comments, templates).
pub fn lex(src: &str, range: Range<usize>, script: bool, work: &mut Work<'_>) -> Result<Vec<Token>, CodeError> {
    let b = src.as_bytes();
    let end = range.end.min(b.len());
    let mut out = Vec::new();
    let mut i = range.start;
    // Script: whether a `/` here starts a regular expression.
    let mut regex_ok = true;
    // Script: the open template literals; each entry is 0 in the template's text, else the brace depth of its `${}`.
    let mut templates: Vec<usize> = Vec::new();
    let mut template_start = 0usize;
    while i < end {
        work.tick(1)?;
        let c = b[i];
        let next = b.get(i.saturating_add(1)).copied();
        if script && templates.last() == Some(&0) {
            // Template text: a backtick closes it, a backslash escapes, `${` opens an expression.
            match c {
                b'`' => {
                    templates.pop();
                    if templates.is_empty() {
                        out.push(Token { kind: TokenKind::Template, range: template_start..i.saturating_add(1) });
                    }
                    i = i.saturating_add(1);
                    regex_ok = false;
                }
                b'\\' => i = i.saturating_add(2),
                b'$' if next == Some(b'{') => {
                    if let Some(top) = templates.last_mut() {
                        *top = 1;
                    }
                    i = i.saturating_add(2);
                    regex_ok = true;
                }
                _ => i = i.saturating_add(1),
            }
            continue;
        }
        if c == b'/' && next == Some(b'*') {
            let close = src.get(i.saturating_add(2)..end).and_then(|t| t.find("*/")).map_or(end, |p| i.saturating_add(2).saturating_add(p).saturating_add(2));
            push_token(&mut out, &templates, TokenKind::BlockComment, i..close);
            i = close;
            continue;
        }
        if script && c == b'/' && next == Some(b'/') {
            let close = src.get(i..end).and_then(|t| t.find(['\n', '\r'])).map_or(end, |p| i.saturating_add(p));
            push_token(&mut out, &templates, TokenKind::LineComment, i..close);
            i = close;
            continue;
        }
        if c == b'"' || c == b'\'' {
            let mut j = i.saturating_add(1);
            while j < end && b[j] != c && !matches!(b[j], b'\n' | b'\r') {
                j = if b[j] == b'\\' { j.saturating_add(2) } else { j.saturating_add(1) };
            }
            let close = if j < end && b[j] == c { j.saturating_add(1) } else { j.min(end) };
            push_token(&mut out, &templates, TokenKind::String, i..close);
            i = close;
            regex_ok = false;
            continue;
        }
        if !script {
            i = if c == b'\\' { i.saturating_add(2) } else { i.saturating_add(1) };
            continue;
        }
        if c == b'`' {
            if templates.is_empty() {
                template_start = i;
            }
            templates.push(0);
            i = i.saturating_add(1);
            continue;
        }
        if (c == b'{' || c == b'}')
            && let Some(top) = templates.last_mut()
        {
            // Inside a `${}` expression braces nest; the matching `}` returns to the template's text.
            *top = if c == b'{' { top.saturating_add(1) } else { top.saturating_sub(1) };
            if *top == 0 {
                i = i.saturating_add(1);
                continue;
            }
        }
        if c == b'/' && regex_ok {
            // A regular-expression literal: to the unescaped `/` outside a class; a newline first means it was not one.
            let (mut j, mut class) = (i.saturating_add(1), false);
            while j < end && !matches!(b[j], b'\n' | b'\r') && (class || b[j] != b'/') {
                match b[j] {
                    b'\\' => j = j.saturating_add(1),
                    b'[' => class = true,
                    b']' => class = false,
                    _ => {}
                }
                j = j.saturating_add(1);
            }
            if j < end && b[j] == b'/' {
                let mut k = j.saturating_add(1);
                while k < end && is_ident(b[k]) {
                    k = k.saturating_add(1);
                }
                push_token(&mut out, &templates, TokenKind::Regex, i..k);
                i = k;
                regex_ok = false;
                continue;
            }
        }
        if is_ident(c) {
            let start = i;
            while i < end && is_ident(b[i]) {
                i = i.saturating_add(1);
            }
            regex_ok = REGEX_AFTER.contains(&src.get(start..i).unwrap_or(""));
            continue;
        }
        if !c.is_ascii_whitespace() {
            regex_ok = !matches!(c, b')' | b']');
        }
        i = i.saturating_add(1);
    }
    if !templates.is_empty() {
        out.push(Token { kind: TokenKind::Template, range: template_start..end });
    }
    Ok(out)
}

/// Inside a template literal every byte belongs to the template token; outside, tokens are listed.
fn push_token(out: &mut Vec<Token>, templates: &[usize], kind: TokenKind, range: Range<usize>) {
    if templates.is_empty() {
        out.push(Token { kind, range });
    }
}

/// The token strictly holding byte position `at` (`start < at < end`), by binary search over the ordered tokens.
pub fn token_around(tokens: &[Token], at: usize) -> Option<&Token> {
    let i = tokens.partition_point(|t| t.range.end <= at);
    tokens.get(i).filter(|t| t.range.start < at && at < t.range.end)
}

/// The token holding byte `at` (`start <= at < end`).
pub fn token_at(tokens: &[Token], at: usize) -> Option<&Token> {
    let i = tokens.partition_point(|t| t.range.end <= at);
    tokens.get(i).filter(|t| t.range.start <= at)
}

// ---------------------------------------------------------------------------------------------------------------------
// Code hints ([STU-WEB-050], [STU-WEB-051]).

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

/// The hint that applies at a byte offset: the hint record's six fields ([STU-WEB-050]) with the items filtered by
/// what is typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HintContext {
    pub group_id: &'static str,
    pub pattern: String,
    pub additional_dismiss_chars: String,
    pub allow_whitespace_prefix: bool,
    pub display_restriction: &'static str,
    pub allow_multiple_values: bool,
    /// The word already typed before the offset (items are filtered by it, ASCII case-insensitively).
    pub prefix: String,
    pub scheme_id: String,
    pub items: Vec<HintItem>,
    pub total: usize,
}

fn context(group: &'static str, hint: &CodeHint, prefix: &str, scheme: &str) -> HintContext {
    let matching: Vec<&HintItem> = hint.items.iter().filter(|i| i.value.len() >= prefix.len() && i.value.get(..prefix.len()).is_some_and(|p| p.eq_ignore_ascii_case(prefix))).collect();
    HintContext {
        group_id: group,
        pattern: hint.pattern.clone(),
        additional_dismiss_chars: hint.additional_dismiss_chars.clone(),
        allow_whitespace_prefix: hint.allow_whitespace_prefix,
        display_restriction: hint.display_restriction,
        allow_multiple_values: hint.allow_multiple_values,
        prefix: prefix.to_string(),
        scheme_id: scheme.to_string(),
        total: matching.len(),
        items: matching.into_iter().take(MAX_HINT_ITEMS).cloned().collect(),
    }
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
pub fn hints_at(doc: &WebDoc, vocabulary: &TagRegistry, schemes: &SchemeRegistry, type_id: &str, offset: usize, work: &mut Work<'_>) -> Result<Option<HintContext>, CodeError> {
    let src = doc.source();
    if offset > src.len() || !src.is_char_boundary(offset) {
        return Err(CodeError::new("offset_invalid"));
    }
    let regions = regions(doc, schemes, type_id, work)?;
    let region = region_at(&regions, offset).ok_or(CodeError::new("offset_invalid"))?;
    let groups = hint_groups(vocabulary, type_id);
    let group = |id: &str| groups.iter().find(|g| g.group_id == id);
    let css_name = |c: u8| c.is_ascii_alphanumeric() || c == b'-' || c == b'_';
    match region.scheme_id.as_str() {
        "css" => {
            // Inside a comment or string: no hint.
            let tokens = lex(src, region.range.clone(), false, work)?;
            if token_around(&tokens, offset).is_some() {
                return Ok(None);
            }
            // Inside a declaration value when the nearest `:` / `;` / `{` / `}` before the offset is a `:`.
            let b = src.as_bytes();
            let mut j = offset;
            while j > region.range.start && !matches!(b[j.saturating_sub(1)], b':' | b';' | b'{' | b'}') {
                j = j.saturating_sub(1);
            }
            work.tick(offset.saturating_sub(j))?;
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
            work.tick(doc.len())?;
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
fn enclosing_element(doc: &WebDoc, range: &Range<usize>, work: &mut Work<'_>) -> Result<Option<NodeId>, CodeError> {
    work.tick(doc.len())?;
    Ok((0..doc.len())
        .filter(|&id| doc.node(id).is_some_and(|n| n.kind == NodeKind::Element && n.span.start <= range.start && range.end <= n.span.end && n.span != *range))
        .min_by_key(|&id| doc.node(id).map_or(usize::MAX, |n| n.span.len())))
}

/// Bracket pairs of a CSS or script region, skipping the lexer's opaque tokens. A closer whose kind is not the
/// innermost open bracket's is unmatched (O(1), review F2: no stack search).
fn bracket_pairs(src: &str, region: &Range<usize>, tokens: &[Token], work: &mut Work<'_>) -> Result<Vec<Range<usize>>, CodeError> {
    let b = src.as_bytes();
    let mut stack: Vec<(u8, usize)> = Vec::new();
    let mut out = Vec::new();
    let mut i = region.start;
    let end = region.end.min(b.len());
    let mut t = tokens.partition_point(|x| x.range.end <= i);
    while i < end {
        work.tick(1)?;
        if let Some(tok) = tokens.get(t)
            && tok.range.start <= i
        {
            i = tok.range.end.max(i.saturating_add(1));
            t = t.saturating_add(1);
            continue;
        }
        let c = b[i];
        match c {
            b'{' | b'(' | b'[' => stack.push((c, i)),
            b'}' | b')' | b']' => {
                let want = match c {
                    b'}' => b'{',
                    b')' => b'(',
                    _ => b'[',
                };
                if stack.last().is_some_and(|(o, _)| *o == want)
                    && let Some((_, at)) = stack.pop()
                {
                    out.push(at..i.saturating_add(1));
                }
            }
            _ => {}
        }
        i = i.saturating_add(1);
    }
    Ok(out)
}

/// Balance braces: the smallest bracket pair (CSS or script region) or element (markup) strictly enclosing `range`.
pub fn balance(doc: &WebDoc, schemes: &SchemeRegistry, type_id: &str, range: Range<usize>, work: &mut Work<'_>) -> Result<Range<usize>, CodeError> {
    let src = doc.source();
    if range.start > range.end || range.end > src.len() || !src.is_char_boundary(range.start) || !src.is_char_boundary(range.end) {
        return Err(CodeError::new("range_invalid"));
    }
    let regions = regions(doc, schemes, type_id, work)?;
    if let Some(region) = region_of_range(&regions, &range).filter(|r| matches!(r.scheme_id.as_str(), "css" | "javascript")) {
        let tokens = lex(src, region.range.clone(), region.scheme_id == "javascript", work)?;
        let pairs = bracket_pairs(src, &region.range, &tokens, work)?;
        if let Some(pair) = pairs.into_iter().filter(|p| p.start <= range.start && range.end <= p.end && *p != range).min_by_key(|p| p.len()) {
            return Ok(pair);
        }
    }
    enclosing_element(doc, &range, work)?.and_then(|id| doc.node(id).map(|n| n.span.clone())).ok_or(CodeError::new("nothing_encloses"))
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
