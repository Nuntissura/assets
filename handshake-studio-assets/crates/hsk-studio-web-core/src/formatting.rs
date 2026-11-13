//! Source edits of code intelligence (lane SMB-WEB-CODE-1; [STU-WEB-015], [STU-WEB-020], [STU-WEB-028], [STU-WEB-029],
//! [STU-WEB-059]): the formatter driven by the tags' formatting rules, comment apply and remove resolved from the
//! language at the position, indent and outdent of a line range, and the doctype declaration edit. Every function
//! returns text edits against one revision; the caller applies them as one patch (one undo step).
//!
//! The formatter only rewrites whitespace: the gaps between nodes (a whitespace-only text node, or the leading or
//! trailing whitespace of a text node, which a browser collapses the same way) and, with `wrap_over_line_length`, the
//! whitespace between a start tag's attributes. Non-whitespace bytes never change, and nothing inside an element whose
//! rules say `formatcontents: no` (`pre`, `script`, `style`, `textarea`, raw-text elements) is touched. A gap is
//! rewritten only where a rule asks for a newline; it keeps blank lines up to the rule's `_max` and never compresses
//! below what is there. Line endings follow the document's first one. A second run over its own output changes nothing
//! (idempotent: `format_is_idempotent`).
#![deny(clippy::arithmetic_side_effects)]

use crate::css_properties::line_ending;
use crate::doc_types::{CodeError, DoctypeDeclaration};
use crate::dom::{ContentModel, Namespace, NodeId, NodeKind, WebDoc};
use crate::hints::{SchemeRegistry, region_at, regions};
use crate::source_dom_patch::TextEdit;
use crate::vocabulary::{FormattingRules, TagRegistry};
use std::ops::Range;

/// The indent character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndentChar {
    Space,
    Tab,
}

/// The attribute-wrap policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttributeWrap {
    /// Attributes stay as written.
    Keep,
    /// A start tag longer than the line-length target puts each attribute after the first on its own line.
    WrapOverLineLength,
}

/// [STU-WEB-028] operator configuration (Studio-declared defaults: two spaces, 120 columns, keep).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatOptions {
    pub indent_char: IndentChar,
    /// 1 to 16.
    pub indent_width: u8,
    /// 0 (none) or 20 to 1000.
    pub line_length: u16,
    pub attribute_wrap: AttributeWrap,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self { indent_char: IndentChar::Space, indent_width: 2, line_length: 120, attribute_wrap: AttributeWrap::Keep }
    }
}

impl FormatOptions {
    pub fn checked(self) -> Result<Self, CodeError> {
        if !(1..=16).contains(&self.indent_width) || (self.line_length != 0 && !(20..=1000).contains(&self.line_length)) {
            return Err(CodeError::new("format_option_invalid"));
        }
        Ok(self)
    }

    fn unit(&self) -> String {
        match self.indent_char {
            IndentChar::Tab => "\t".to_string(),
            IndentChar::Space => " ".repeat(usize::from(self.indent_width)),
        }
    }
}

fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

fn newlines(text: &str) -> usize {
    text.bytes().filter(|c| *c == b'\n').count().max(text.bytes().filter(|c| *c == b'\r').count())
}

/// The rules of element `id` (an element with no definition is inline: nothing is placed around it).
fn rules_of(doc: &WebDoc, vocabulary: &TagRegistry, type_id: &str, id: NodeId) -> FormattingRules {
    let Some(node) = doc.node(id) else { return FormattingRules::INLINE };
    if node.kind != NodeKind::Element {
        return FormattingRules::INLINE;
    }
    if node.namespace != Namespace::Html || matches!(node.content, ContentModel::RawText | ContentModel::Rcdata) {
        // Foreign content keeps its whitespace; raw text and RCDATA bodies are verbatim.
        return FormattingRules { formatcontents: false, ..doc.local_name(id).and_then(|n| vocabulary.tag(type_id, &n)).map_or(FormattingRules::INLINE, |t| t.formatting_rules) };
    }
    doc.local_name(id).and_then(|n| vocabulary.tag(type_id, &n)).map_or(FormattingRules::INLINE, |t| t.formatting_rules)
}

struct Formatter<'a> {
    doc: &'a WebDoc,
    vocabulary: &'a TagRegistry,
    type_id: &'a str,
    nl: &'static str,
    unit: String,
    opts: FormatOptions,
    edits: Vec<TextEdit>,
}

/// One gap between two positions of a parent's content: the whitespace bytes it spans now.
struct Gap {
    range: Range<usize>,
}

impl<'a> Formatter<'a> {
    fn src(&self) -> &'a str {
        self.doc.source()
    }

    /// Rewrites `gap` to `want` newlines (kept up to `max` if more are there) and `level` indents; no change when the
    /// rule asks for no newline.
    fn place(&mut self, gap: &Gap, want: u8, max: u8, level: usize) {
        if want == 0 {
            return;
        }
        let text = self.src().get(gap.range.clone()).unwrap_or("");
        let have = newlines(text);
        let count = have.clamp(usize::from(want), usize::from(max.max(want)));
        let mut out = String::new();
        for _ in 0..count {
            out.push_str(self.nl);
        }
        for _ in 0..level {
            out.push_str(&self.unit);
        }
        if out != text {
            self.edits.push(TextEdit { span: gap.range.clone(), replacement: out });
        }
    }

    /// The whitespace before child `i` of `children` (from the previous node's last non-whitespace byte, or `start`).
    fn gaps(&self, start: usize, end: usize, children: &[NodeId]) -> (Vec<Gap>, Vec<Option<NodeId>>) {
        let b = self.src().as_bytes();
        // Content items: elements, comments, doctypes, and the non-whitespace core of text nodes.
        let mut items: Vec<(Range<usize>, Option<NodeId>)> = Vec::new();
        for &c in children {
            let Some(n) = self.doc.node(c) else { continue };
            match n.kind {
                NodeKind::Text => {
                    let mut r = n.span.clone();
                    while r.start < r.end && is_ws(b[r.start]) {
                        r.start = r.start.saturating_add(1);
                    }
                    while r.end > r.start && is_ws(b[r.end.saturating_sub(1)]) {
                        r.end = r.end.saturating_sub(1);
                    }
                    if !r.is_empty() {
                        items.push((r, None));
                    }
                }
                _ => items.push((n.span.clone(), Some(c))),
            }
        }
        let mut gaps = Vec::new();
        let mut cursor = start;
        for (r, _) in &items {
            gaps.push(Gap { range: cursor..r.start });
            cursor = r.end;
        }
        gaps.push(Gap { range: cursor..end });
        (gaps, items.into_iter().map(|(_, id)| id).collect())
    }

    /// Formats the content of element (or document) `id` whose own level is `level`.
    fn content(&mut self, id: NodeId, level: usize, is_root: bool) {
        let Some(node) = self.doc.node(id) else { return };
        let (start, end) = if is_root {
            (0, self.src().len())
        } else {
            match node.content_span() {
                Some(r) => (r.start, r.end),
                None => return,
            }
        };
        let rules = if is_root { FormattingRules::INLINE } else { rules_of(self.doc, self.vocabulary, self.type_id, id) };
        if !rules.formatcontents {
            return;
        }
        let children = node.children.clone();
        let (gaps, items) = self.gaps(start, end, &children);
        let inner = if rules.indentcontents { level.saturating_add(1) } else { level };
        let child_rules: Vec<FormattingRules> = items.iter().map(|c| c.map_or(FormattingRules::INLINE, |c| rules_of(self.doc, self.vocabulary, self.type_id, c))).collect();
        for (i, gap) in gaps.iter().enumerate() {
            let before = i.checked_sub(1).and_then(|p| child_rules.get(p));
            let after = child_rules.get(i);
            if items.is_empty() {
                break;
            }
            let (want, max, lvl) = match (before, after) {
                // Before the first item: the parent's `nlbeforecontents` (never at the document's start).
                (None, Some(a)) => (if is_root { 0 } else { rules.nlbeforecontents.max(a.nlbeforetag) }, a.nlbeforetag_max, inner),
                (Some(b), Some(a)) => (b.nlaftertag.max(a.nlbeforetag), b.nlaftertag_max.max(a.nlbeforetag_max), inner),
                // After the last item: the parent's `nlaftercontents`, the close tag at the parent's level.
                (Some(b), None) => (if is_root { 0 } else { rules.nlaftercontents.max(b.nlaftertag) }, b.nlaftertag_max, level),
                (None, None) => (0, 0, level),
            };
            self.place(gap, want, max, lvl);
        }
        for (i, item) in items.iter().enumerate() {
            let Some(c) = item else { continue };
            let Some(n) = self.doc.node(*c) else { continue };
            if n.kind == NodeKind::Element {
                self.wrap_attributes(*c, inner, child_rules.get(i).copied().unwrap_or(FormattingRules::INLINE));
                self.content(*c, inner, false);
            }
        }
    }

    /// [STU-WEB-028] attribute wrap: the whitespace between attributes of a start tag over the line length.
    fn wrap_attributes(&mut self, id: NodeId, level: usize, rules: FormattingRules) {
        if self.opts.attribute_wrap != AttributeWrap::WrapOverLineLength || self.opts.line_length == 0 || rules.nlbeforetag == 0 {
            return;
        }
        let Some(node) = self.doc.node(id) else { return };
        if node.attrs.len() < 2 {
            return;
        }
        let name = node.name_span.as_ref().map_or(0, |s| s.len());
        // The one-line length of the tag: indent, `<`, name, each attribute after one space, `>`.
        let one_line = node.attrs.iter().map(|a| a.span.len().saturating_add(1)).fold(name.saturating_add(2), usize::saturating_add).saturating_add(level.saturating_mul(self.unit.len()));
        if one_line <= usize::from(self.opts.line_length) {
            return;
        }
        let src = self.src().to_string();
        let mut indent = String::new();
        for _ in 0..level.saturating_add(1) {
            indent.push_str(&self.unit);
        }
        for pair in node.attrs.windows(2) {
            let gap = pair[0].span.end..pair[1].span.start;
            let text = src.get(gap.clone()).unwrap_or("");
            if text.is_empty() || !text.bytes().all(is_ws) {
                continue;
            }
            let want = format!("{}{indent}", self.nl);
            if text != want {
                self.edits.push(TextEdit { span: gap, replacement: want });
            }
        }
    }
}

/// Merges sorted, non-overlapping edits into at most `max` edits (each merged edit spans the bytes between its parts,
/// copied unchanged), so a large document's format stays inside the engine's edit limit.
pub fn coalesce(src: &str, mut edits: Vec<TextEdit>, max: usize) -> Vec<TextEdit> {
    edits.sort_by_key(|e| (e.span.start, e.span.end));
    if edits.len() <= max || max == 0 {
        return edits;
    }
    let per = edits.len().div_ceil(max);
    let mut out = Vec::with_capacity(max);
    for chunk in edits.chunks(per) {
        let (Some(first), Some(last)) = (chunk.first(), chunk.last()) else { continue };
        let mut text = String::new();
        let mut cursor = first.span.start;
        for e in chunk {
            text.push_str(src.get(cursor..e.span.start).unwrap_or(""));
            text.push_str(&e.replacement);
            cursor = e.span.end;
        }
        out.push(TextEdit { span: first.span.start..last.span.end, replacement: text });
    }
    out
}

/// [STU-WEB-028]: the edits that format the document (or element `scope` and its subtree) by the formatting rules of
/// `vocabulary` for `type_id` (module docs). Empty when the text is already formatted.
pub fn format_edits(doc: &WebDoc, vocabulary: &TagRegistry, type_id: &str, scope: Option<NodeId>, opts: FormatOptions) -> Result<Vec<TextEdit>, CodeError> {
    let opts = opts.checked()?;
    let mut f = Formatter { doc, vocabulary, type_id, nl: line_ending(doc.source()), unit: opts.unit(), opts, edits: Vec::new() };
    match scope {
        None => f.content(doc.root(), 0, true),
        Some(id) => {
            let node = doc.node(id).ok_or(CodeError::new("no_such_node"))?;
            if node.kind != NodeKind::Element {
                return Err(CodeError::new("not_an_element"));
            }
            // The element's level: its indenting ancestors.
            let mut level = 0usize;
            let mut at = node.parent;
            let mut inside_verbatim = false;
            while let Some(p) = at {
                let r = rules_of(doc, vocabulary, type_id, p);
                if r.indentcontents {
                    level = level.saturating_add(1);
                }
                inside_verbatim |= doc.node(p).is_some_and(|n| n.kind == NodeKind::Element) && !r.formatcontents;
                at = doc.node(p).and_then(|n| n.parent);
            }
            if inside_verbatim {
                return Err(CodeError::new("format_inside_verbatim"));
            }
            f.wrap_attributes(id, level, rules_of(doc, vocabulary, type_id, id));
            f.content(id, level, false);
        }
    }
    Ok(coalesce(doc.source(), f.edits, doc.limits().max_edits))
}

// ---------------------------------------------------------------------------------------------------------------------
// Comments ([STU-WEB-029]).

/// The comment syntax of a scheme: open and close delimiters.
pub fn comment_syntax(scheme_id: &str) -> Option<(&'static str, &'static str)> {
    match scheme_id {
        "html" | "html_comment" => Some(("<!--", "-->")),
        "css" | "javascript" => Some(("/*", "*/")),
        _ => None,
    }
}

/// [STU-WEB-029] apply: wraps `range` in the comment syntax of the language that owns it (the range must lie in one
/// region). HTML comment text may not hold `--`, `-->` or `--!>`, start with `>` or `->`, or end with `<!-` (the
/// comment would end early or nest); CSS and script text may not hold `*/`: those are refused, never rewritten.
pub fn comment_edit(doc: &WebDoc, range: Range<usize>) -> Result<(TextEdit, &'static str), CodeError> {
    let src = doc.source();
    if range.start >= range.end || range.end > src.len() || !src.is_char_boundary(range.start) || !src.is_char_boundary(range.end) {
        return Err(CodeError::new("range_invalid"));
    }
    let schemes = SchemeRegistry::shipped();
    let regions = regions(doc, &schemes);
    let region = region_at(&regions, range.start).ok_or(CodeError::new("range_invalid"))?;
    if range.end > region.range.end {
        return Err(CodeError::new("range_crosses_languages"));
    }
    if region.scheme_id == "html_comment" {
        return Err(CodeError::new("already_commented"));
    }
    let (open, close) = comment_syntax(&region.scheme_id).ok_or(CodeError::new("comment_syntax_unknown"))?;
    let text = src.get(range.clone()).unwrap_or("");
    let nests = if open == "<!--" { text.contains("--") || text.starts_with('>') || text.starts_with("->") || text.ends_with("<!-") || text.ends_with('-') } else { text.contains("*/") };
    if nests {
        return Err(CodeError::new("comment_would_nest"));
    }
    if open == "<!--" {
        // Neither end may fall inside a start or end tag (a comment around part of a tag breaks it).
        let inside = |at: usize, r: &Range<usize>| r.start < at && at < r.end;
        let splits = (0..doc.len()).filter_map(|id| doc.node(id)).filter(|n| n.kind == NodeKind::Element).any(|n| [&n.open_span, &n.close_span].into_iter().flatten().any(|t| inside(range.start, t) || inside(range.end, t)));
        if splits {
            return Err(CodeError::new("range_splits_a_tag"));
        }
    }
    let replacement = format!("{open}{text}{close}");
    Ok((TextEdit { span: range, replacement }, static_scheme(&region.scheme_id)))
}

fn static_scheme(id: &str) -> &'static str {
    match id {
        "css" => "css",
        "javascript" => "javascript",
        "html_comment" => "html_comment",
        "text" => "text",
        _ => "html",
    }
}

/// [STU-WEB-029] remove: unwraps the comment holding byte `offset` (an HTML comment node, or a `/* */` comment inside a
/// CSS or script region), keeping its text.
pub fn uncomment_edit(doc: &WebDoc, offset: usize) -> Result<(TextEdit, &'static str), CodeError> {
    let src = doc.source();
    if offset > src.len() {
        return Err(CodeError::new("offset_invalid"));
    }
    let schemes = SchemeRegistry::shipped();
    let regions = regions(doc, &schemes);
    let region = region_at(&regions, offset).ok_or(CodeError::new("offset_invalid"))?;
    match region.scheme_id.as_str() {
        "html_comment" => {
            let text = src.get(region.range.clone()).unwrap_or("");
            let inner = text.strip_prefix("<!--").and_then(|t| t.strip_suffix("-->")).ok_or(CodeError::new("comment_not_removable"))?;
            Ok((TextEdit { span: region.range.clone(), replacement: inner.to_string() }, "html_comment"))
        }
        "css" | "javascript" => {
            // Walk the region with the language's tokens to find the comment holding the offset.
            let b = src.as_bytes();
            let mut i = region.range.start;
            let end = region.range.end;
            while i < end {
                match b[i] {
                    q @ (b'"' | b'\'' | b'`') if q != b'`' || region.scheme_id == "javascript" => {
                        let mut j = i.saturating_add(1);
                        while j < end && b[j] != q {
                            j = if b[j] == b'\\' { j.saturating_add(2) } else { j.saturating_add(1) };
                        }
                        i = j.saturating_add(1);
                    }
                    b'/' if b.get(i.saturating_add(1)) == Some(&b'*') => {
                        let close = src.get(i.saturating_add(2)..end).and_then(|t| t.find("*/")).map(|p| i.saturating_add(2).saturating_add(p)).ok_or(CodeError::at("comment_not_removable", i..end))?;
                        let after = close.saturating_add(2);
                        if i <= offset && offset <= after {
                            let inner = src.get(i.saturating_add(2)..close).unwrap_or("").to_string();
                            return Ok((TextEdit { span: i..after, replacement: inner }, if region.scheme_id == "css" { "css" } else { "javascript" }));
                        }
                        i = after;
                    }
                    _ => i = i.saturating_add(1),
                }
            }
            Err(CodeError::new("no_comment_here"))
        }
        _ => Err(CodeError::new("no_comment_here")),
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Indent and outdent ([STU-WEB-059]).

/// The lines overlapping `range`: their start offsets.
fn line_starts(src: &str, range: &Range<usize>) -> Vec<usize> {
    let mut out = Vec::new();
    let mut start = src.get(..range.start).and_then(|s| s.rfind('\n')).map_or(0, |i| i.saturating_add(1));
    loop {
        out.push(start);
        match src.get(start..).and_then(|s| s.find('\n')) {
            Some(i) => {
                let next = start.saturating_add(i).saturating_add(1);
                if next >= range.end.max(range.start.saturating_add(1)) || next >= src.len() {
                    break;
                }
                start = next;
            }
            None => break,
        }
    }
    out
}

/// Indent (`outdent` false) or outdent every line overlapping `range` by one unit; blank lines are left alone. Lines
/// inside a verbatim element (`pre`, `textarea`, raw text) are refused rather than changed (`range_in_verbatim`).
pub fn indent_edits(doc: &WebDoc, range: Range<usize>, outdent: bool, unit: &str) -> Result<Vec<TextEdit>, CodeError> {
    let src = doc.source();
    if range.start > range.end || range.end > src.len() || !src.is_char_boundary(range.start) || !src.is_char_boundary(range.end) {
        return Err(CodeError::new("range_invalid"));
    }
    if unit.is_empty() || unit.len() > 16 || !unit.bytes().all(|c| c == b' ' || c == b'\t') {
        return Err(CodeError::new("format_option_invalid"));
    }
    let starts = line_starts(src, &range);
    for id in 0..doc.len() {
        let Some(n) = doc.node(id) else { continue };
        let verbatim = n.kind == NodeKind::Element && (matches!(n.content, ContentModel::RawText | ContentModel::Rcdata) || doc.local_name(id).as_deref() == Some("pre"));
        if let (true, Some(c)) = (verbatim, n.content_span())
            && starts.iter().any(|s| c.start < *s && *s < c.end)
        {
            return Err(CodeError::new("range_in_verbatim"));
        }
    }
    let mut edits = Vec::new();
    for s in starts {
        let line_end = src.get(s..).and_then(|t| t.find('\n')).map_or(src.len(), |i| s.saturating_add(i));
        let line = src.get(s..line_end).unwrap_or("");
        if line.trim().is_empty() {
            continue;
        }
        if outdent {
            let spaces = line.bytes().take_while(|c| *c == b' ').count();
            let take = if line.starts_with('\t') { 1 } else { spaces.min(unit.len()) };
            if take > 0 {
                edits.push(TextEdit { span: s..s.saturating_add(take), replacement: String::new() });
            }
        } else {
            edits.push(TextEdit { span: s..s, replacement: unit.to_string() });
        }
    }
    if edits.is_empty() {
        return Err(CodeError::new("nothing_to_change"));
    }
    Ok(coalesce(src, edits, doc.limits().max_edits))
}

// ---------------------------------------------------------------------------------------------------------------------
// Doctype ([STU-WEB-015]).

/// The edit that makes `decl` the document's doctype: the existing doctype node replaced, or the declaration inserted
/// at the start with the document's line ending. `xslt_1` (an internal subset an HTML parser does not read as one
/// doctype) is refused on an HTML document.
pub fn doctype_edit(doc: &WebDoc, decl: &DoctypeDeclaration) -> Result<TextEdit, CodeError> {
    if decl.declaration_id == "xslt_1" {
        return Err(CodeError::new("doctype_needs_xml_parser"));
    }
    let existing = doc.node(doc.root()).and_then(|r| r.children.iter().copied().find(|&c| doc.node(c).is_some_and(|n| n.kind == NodeKind::Doctype)));
    match existing.and_then(|c| doc.node(c)) {
        Some(n) if doc.slice(&n.span) == decl.literal => Err(CodeError::new("nothing_to_change")),
        Some(n) => Ok(TextEdit { span: n.span.clone(), replacement: decl.literal.to_string() }),
        None => Ok(TextEdit { span: 0..0, replacement: format!("{}{}", decl.literal, line_ending(doc.source())) }),
    }
}
