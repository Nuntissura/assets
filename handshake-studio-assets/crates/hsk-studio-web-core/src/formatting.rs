//! Source edits of code intelligence (lane SMB-WEB-CODE-1; [STU-WEB-015], [STU-WEB-020], [STU-WEB-028], [STU-WEB-029],
//! [STU-WEB-059]): the formatter driven by the tags' formatting rules, comment apply and remove resolved from the
//! language at the position, indent and outdent of lines, and the doctype declaration edit. Every function returns
//! text edits against one revision; the caller applies them as one patch (one undo step). Every loop over the input is
//! charged to a [`Work`] (cancellation and a budget).
//!
//! The formatter is rendering-neutral (review F3, lead Q2). It only rewrites whitespace: the gaps between nodes (a
//! whitespace-only text node, or the leading or trailing whitespace of a text node) and, with
//! `wrap_over_line_length`, the whitespace between a start tag's attributes; attribute values and every non-whitespace
//! byte stay. It writes whitespace into a gap that had none only when both sides of the gap are block-level by class
//! (a block element, or the open or close tag of a block container), never next to text, an inline-level or replaced
//! element (`img`, `picture`, `video`, `canvas`, `select`, `iframe`, `object`, `map` and the like are `Inline`) or a
//! comment; a gap that already holds whitespace collapses the same way rewritten. Nothing inside an element whose
//! rules say `formatcontents: no` is touched (`pre`, `listing`, `plaintext`, `textarea`, `script`, `style`, raw-text
//! and foreign content). A gap is rewritten only where a rule asks for a newline; it keeps blank lines up to the rule's
//! `_max`. Line endings follow the document's first one. A second run over its own output changes nothing.
//!
//! Comments (review F1) change only the selection: CSS and script are read with the lexer of `hints` (comments,
//! strings, line comments, template literals, regular expressions), and a selection whose end lies inside one of
//! those tokens, an unwrap whose text would open or close a comment or string beyond the selection, a selection in
//! RCDATA or raw text, and a comment marker inside the selection are refused typed, never guessed.
#![deny(clippy::arithmetic_side_effects)]

use crate::css_properties::line_ending;
use crate::doc_types::{CodeError, DoctypeDeclaration, Work};
use crate::dom::{ContentModel, Namespace, NodeId, NodeKind, WebDoc};
use crate::hints::{SchemeRegistry, Token, TokenKind, lex, region_at, region_of_range, regions, token_around};
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

/// Elements a browser renders as preformatted though the tokenizer reads their content as markup.
const PREFORMATTED: [&str; 3] = ["pre", "listing", "plaintext"];

/// Whether element `id`'s content is verbatim: raw text, RCDATA, foreign content or a preformatted element.
fn verbatim(doc: &WebDoc, id: NodeId) -> bool {
    doc.node(id).is_some_and(|n| n.kind == NodeKind::Element && (n.namespace != Namespace::Html || matches!(n.content, ContentModel::RawText | ContentModel::Rcdata) || doc.local_name(id).is_some_and(|t| PREFORMATTED.contains(&t.as_str()))))
}

/// The rules of element `id` (an element with no definition is inline: nothing is placed around it).
fn rules_of(doc: &WebDoc, vocabulary: &TagRegistry, type_id: &str, id: NodeId) -> FormattingRules {
    let Some(node) = doc.node(id) else { return FormattingRules::INLINE };
    if node.kind != NodeKind::Element {
        return FormattingRules::INLINE;
    }
    let rules = doc.local_name(id).and_then(|n| vocabulary.tag(type_id, &n)).map_or(FormattingRules::INLINE, |t| t.formatting_rules);
    if verbatim(doc, id) { FormattingRules { formatcontents: false, ..rules } } else { rules }
}

/// Block-level by class: an element the rules place on its own line.
fn block(rules: &FormattingRules) -> bool {
    rules.nlbeforetag > 0 || rules.nlaftertag > 0
}

struct Formatter<'a, 'w> {
    doc: &'a WebDoc,
    vocabulary: &'a TagRegistry,
    type_id: &'a str,
    nl: &'static str,
    unit: String,
    opts: FormatOptions,
    edits: Vec<TextEdit>,
    work: &'a mut Work<'w>,
}

/// One gap between two positions of a parent's content: the whitespace bytes it spans now.
struct Gap {
    range: Range<usize>,
}

impl<'a> Formatter<'a, '_> {
    fn src(&self) -> &'a str {
        self.doc.source()
    }

    /// Rewrites `gap` to `want` newlines (kept up to `max` if more are there) and `level` indents; no change when the
    /// rule asks for no newline, or when the gap is empty and a side is not block-level (`empty_ok` false).
    fn place(&mut self, gap: &Gap, want: u8, max: u8, level: usize, empty_ok: bool) {
        if want == 0 || (gap.range.is_empty() && !empty_ok) {
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

    /// The gaps around the content items of `children` (elements, comments, doctypes and the non-whitespace core of
    /// text nodes) between `start` and `end`.
    fn gaps(&self, start: usize, end: usize, children: &[NodeId]) -> (Vec<Gap>, Vec<Option<NodeId>>) {
        let b = self.src().as_bytes();
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
    fn content(&mut self, id: NodeId, level: usize, is_root: bool) -> Result<(), CodeError> {
        let Some(node) = self.doc.node(id) else { return Ok(()) };
        self.work.tick(node.children.len().saturating_add(1))?;
        let (start, end) = if is_root {
            (0, self.src().len())
        } else {
            match node.content_span() {
                Some(r) => (r.start, r.end),
                None => return Ok(()),
            }
        };
        let rules = if is_root { FormattingRules::INLINE } else { rules_of(self.doc, self.vocabulary, self.type_id, id) };
        if !rules.formatcontents {
            return Ok(());
        }
        let parent_block = !is_root && rules.nlbeforecontents > 0;
        let children = node.children.clone();
        let (gaps, items) = self.gaps(start, end, &children);
        let inner = if rules.indentcontents { level.saturating_add(1) } else { level };
        // `None` for text: text is inline content.
        let child_rules: Vec<Option<FormattingRules>> = items.iter().map(|c| c.map(|c| rules_of(self.doc, self.vocabulary, self.type_id, c))).collect();
        let is_block = |r: Option<&Option<FormattingRules>>| r.and_then(|x| x.as_ref()).is_some_and(block);
        if items.is_empty() {
            return Ok(());
        }
        for (i, gap) in gaps.iter().enumerate() {
            let before = i.checked_sub(1).and_then(|p| child_rules.get(p));
            let after = child_rules.get(i);
            let r = |x: Option<&Option<FormattingRules>>| x.and_then(|v| *v).unwrap_or(FormattingRules::INLINE);
            let (want, max, lvl, empty_ok) = match (before, after) {
                // Before the first item: the parent's `nlbeforecontents` (never at the document's start).
                // A block container's leading and trailing whitespace is never rendered, so its edges may be written.
                (None, Some(_)) => (if is_root { 0 } else { rules.nlbeforecontents.max(r(after).nlbeforetag) }, r(after).nlbeforetag_max, inner, parent_block),
                (Some(_), Some(_)) => (r(before).nlaftertag.max(r(after).nlbeforetag), r(before).nlaftertag_max.max(r(after).nlbeforetag_max), inner, is_block(before) && is_block(after)),
                // After the last item: the parent's `nlaftercontents`, the close tag at the parent's level.
                (Some(_), None) => (if is_root { 0 } else { rules.nlaftercontents.max(r(before).nlaftertag) }, r(before).nlaftertag_max, level, parent_block),
                (None, None) => (0, 0, level, false),
            };
            // At the document's top level a gap between nodes that are not text (doctype, comments, the skeleton) and
            // next to a block is never rendered.
            let text_side = matches!(before, Some(None)) || matches!(after, Some(None));
            let empty_ok = empty_ok || (is_root && before.is_some() && after.is_some() && (is_block(before) || is_block(after)) && !text_side);
            self.place(gap, want, max, lvl, empty_ok);
        }
        for (i, item) in items.iter().enumerate() {
            let Some(c) = item else { continue };
            let Some(n) = self.doc.node(*c) else { continue };
            if n.kind == NodeKind::Element {
                self.wrap_attributes(*c, inner, child_rules.get(i).copied().flatten().unwrap_or(FormattingRules::INLINE));
                self.content(*c, inner, false)?;
            }
        }
        Ok(())
    }

    /// [STU-WEB-028] attribute wrap: the whitespace between attributes of a start tag over the line length (values are
    /// never touched).
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
        let src = self.src();
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
pub fn format_edits(doc: &WebDoc, vocabulary: &TagRegistry, type_id: &str, scope: Option<NodeId>, opts: FormatOptions, work: &mut Work<'_>) -> Result<Vec<TextEdit>, CodeError> {
    let opts = opts.checked()?;
    let mut f = Formatter { doc, vocabulary, type_id, nl: line_ending(doc.source()), unit: opts.unit(), opts, edits: Vec::new(), work };
    match scope {
        None => f.content(doc.root(), 0, true)?,
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
            f.content(id, level, false)?;
        }
    }
    let edits = std::mem::take(&mut f.edits);
    Ok(coalesce(doc.source(), edits, doc.limits().max_edits))
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

fn static_scheme(id: &str) -> &'static str {
    match id {
        "css" => "css",
        "javascript" => "javascript",
        "html_comment" => "html_comment",
        "text" => "text",
        _ => "html",
    }
}

/// The element whose RCDATA or raw-text content holds `range` (markup that is text there, not markup).
fn text_content_holding(doc: &WebDoc, range: &Range<usize>, work: &mut Work<'_>) -> Result<Option<ContentModel>, CodeError> {
    work.tick(doc.len())?;
    Ok((0..doc.len()).filter_map(|id| doc.node(id)).find(|n| n.kind == NodeKind::Element && matches!(n.content, ContentModel::RawText | ContentModel::Rcdata) && n.content_span().is_some_and(|c| c.start <= range.start && range.end <= c.end)).map(|n| n.content))
}

/// [STU-WEB-029] apply: wraps `range` in the comment syntax of the language that owns it (the range must lie in one
/// region). Refused typed: a range across languages (`range_crosses_languages`), in RCDATA (`range_in_rcdata`) or raw
/// text (`range_in_raw_text`), with an end inside a comment, string, template or regular expression
/// (`range_splits_a_token`), or inside a tag (`range_splits_a_tag`); HTML text holding `--`, starting with `>` or `->`
/// or ending with `<!-` or `-`, and CSS or script text holding `*/` (`comment_would_nest`).
pub fn comment_edit(doc: &WebDoc, schemes: &SchemeRegistry, type_id: &str, range: Range<usize>, work: &mut Work<'_>) -> Result<(TextEdit, &'static str), CodeError> {
    let src = doc.source();
    if range.start >= range.end || range.end > src.len() || !src.is_char_boundary(range.start) || !src.is_char_boundary(range.end) {
        return Err(CodeError::new("range_invalid"));
    }
    let regions = regions(doc, schemes, type_id, work)?;
    let region = region_of_range(&regions, &range).ok_or(CodeError::new("range_crosses_languages"))?;
    if region.scheme_id == "html_comment" {
        return Err(CodeError::new("already_commented"));
    }
    let (open, close) = comment_syntax(&region.scheme_id).ok_or(CodeError::new("comment_syntax_unknown"))?;
    let text = src.get(range.clone()).unwrap_or("");
    if open == "<!--" {
        match text_content_holding(doc, &range, work)? {
            Some(ContentModel::Rcdata) => return Err(CodeError::new("range_in_rcdata")),
            Some(_) => return Err(CodeError::new("range_in_raw_text")),
            None => {}
        }
        if text.contains("--") || text.starts_with('>') || text.starts_with("->") || text.ends_with("<!-") || text.ends_with('-') {
            return Err(CodeError::new("comment_would_nest"));
        }
        // Neither end may fall inside a start or end tag (a comment around part of a tag breaks it).
        let inside = |at: usize, r: &Range<usize>| r.start < at && at < r.end;
        work.tick(doc.len())?;
        let splits = (0..doc.len()).filter_map(|id| doc.node(id)).filter(|n| n.kind == NodeKind::Element).any(|n| [&n.open_span, &n.close_span].into_iter().flatten().any(|t| inside(range.start, t) || inside(range.end, t)));
        if splits {
            return Err(CodeError::new("range_splits_a_tag"));
        }
    } else {
        let tokens = lex(src, region.range.clone(), region.scheme_id == "javascript", work)?;
        if token_around(&tokens, range.start).is_some() || token_around(&tokens, range.end).is_some() {
            return Err(CodeError::new("range_splits_a_token"));
        }
        if text.contains("*/") {
            return Err(CodeError::new("comment_would_nest"));
        }
    }
    Ok((TextEdit { span: range, replacement: format!("{open}{text}{close}") }, static_scheme(&region.scheme_id)))
}

/// Whether lexing `text` alone leaves a token open at its end (a string, template or comment that would run on past
/// the unwrapped text, or a line comment that would swallow what follows it on the line).
fn opens_beyond(text: &str, script: bool, line_continues: bool, work: &mut Work<'_>) -> Result<bool, CodeError> {
    let tokens = lex(text, 0..text.len(), script, work)?;
    Ok(tokens.iter().any(|t: &Token| {
        let body = text.get(t.range.clone()).unwrap_or("");
        t.range.end == text.len()
            && match t.kind {
                TokenKind::LineComment => line_continues,
                TokenKind::BlockComment => !(body.len() >= 4 && body.ends_with("*/")),
                TokenKind::String => body.len() < 2 || !body.ends_with(body.chars().next().unwrap_or('"')),
                TokenKind::Template => body.len() < 2 || !body.ends_with('`'),
                TokenKind::Regex => false,
            }
    }))
}

/// [STU-WEB-029] remove: unwraps the comment holding byte `offset` (an HTML comment node, or a block or line comment
/// inside a CSS or script region found by the lexer), keeping its text. Refused typed when the text would open or close
/// a comment, string or markup comment beyond the selection (`comment_inner_has_markers`), when the offset is in no
/// comment (`no_comment_here`, a string included).
pub fn uncomment_edit(doc: &WebDoc, schemes: &SchemeRegistry, type_id: &str, offset: usize, work: &mut Work<'_>) -> Result<(TextEdit, &'static str), CodeError> {
    let src = doc.source();
    if offset > src.len() {
        return Err(CodeError::new("offset_invalid"));
    }
    let regions = regions(doc, schemes, type_id, work)?;
    let region = region_at(&regions, offset).ok_or(CodeError::new("offset_invalid"))?;
    match region.scheme_id.as_str() {
        "html_comment" => {
            let text = src.get(region.range.clone()).unwrap_or("");
            let inner = text.strip_prefix("<!--").and_then(|t| t.strip_suffix("-->").or_else(|| t.strip_suffix("--!>"))).ok_or(CodeError::new("comment_not_removable"))?;
            if inner.contains("<!--") || inner.contains("-->") || inner.contains("--!>") || inner.ends_with("<!-") {
                return Err(CodeError::new("comment_inner_has_markers"));
            }
            Ok((TextEdit { span: region.range.clone(), replacement: inner.to_string() }, "html_comment"))
        }
        scheme @ ("css" | "javascript") => {
            let script = scheme == "javascript";
            let tokens = lex(src, region.range.clone(), script, work)?;
            let i = tokens.partition_point(|t| t.range.end < offset);
            let token = tokens.get(i).filter(|t| t.range.start <= offset && offset <= t.range.end).ok_or(CodeError::new("no_comment_here"))?;
            let inner = match token.kind {
                TokenKind::BlockComment => {
                    let body = src.get(token.range.clone()).unwrap_or("");
                    if body.len() < 4 || !body.ends_with("*/") {
                        return Err(CodeError::at("comment_not_removable", token.range.clone()));
                    }
                    body.get(2..body.len().saturating_sub(2)).unwrap_or("")
                }
                TokenKind::LineComment => src.get(token.range.start.saturating_add(2)..token.range.end).unwrap_or(""),
                _ => return Err(CodeError::new("no_comment_here")),
            };
            // After a block comment code may follow on the same line; a line comment ends its line.
            let after = src.as_bytes().get(token.range.end).copied();
            let line_continues = token.kind == TokenKind::BlockComment && !matches!(after, None | Some(b'\n' | b'\r'));
            if inner.contains("/*") || opens_beyond(inner, script, line_continues, work)? {
                return Err(CodeError::new("comment_inner_has_markers"));
            }
            Ok((TextEdit { span: token.range.clone(), replacement: inner.to_string() }, if script { "javascript" } else { "css" }))
        }
        _ => Err(CodeError::new("no_comment_here")),
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Indent and outdent ([STU-WEB-059]).

/// Most lines one indent or outdent changes.
pub const MAX_INDENT_LINES: usize = 65536;

/// The start offsets of the lines overlapping `range` (the line holding `range.start` first).
fn line_starts(src: &str, range: &Range<usize>, work: &mut Work<'_>) -> Result<Vec<usize>, CodeError> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let first = src.get(..range.start).and_then(|s| s.rfind('\n')).map_or(0, |i| i.saturating_add(1));
    out.push(first);
    let stop = range.end.max(range.start.saturating_add(1)).min(b.len());
    work.tick(stop.saturating_sub(first))?;
    for (i, &c) in b.iter().enumerate().take(stop).skip(first) {
        let next = i.saturating_add(1);
        if c == b'\n' && next < stop && next < b.len() {
            out.push(next);
            if out.len() > MAX_INDENT_LINES {
                return Err(CodeError::new("range_too_large"));
            }
        }
    }
    Ok(out)
}

/// The spans no indent may write into: the content of verbatim elements (`pre`, `listing`, `plaintext`, `textarea`,
/// `script`, `style`, raw text, foreign content) and attribute values; a line start strictly inside one (or at the end
/// of a value) is protected. Sorted by start.
fn protected_spans(doc: &WebDoc, work: &mut Work<'_>) -> Result<Vec<Range<usize>>, CodeError> {
    let mut out = Vec::new();
    for id in 0..doc.len() {
        work.tick(1)?;
        let Some(n) = doc.node(id) else { continue };
        if n.kind != NodeKind::Element {
            continue;
        }
        // A line starting at the content's end would put the indent into the content (before the end tag).
        if verbatim(doc, id)
            && let Some(c) = n.content_span()
        {
            out.push(c.start..c.end.saturating_add(1));
        }
        for a in &n.attrs {
            if let Some(v) = &a.value_span {
                out.push(v.start..v.end.saturating_add(1));
            }
        }
    }
    out.sort_by_key(|r| r.start);
    Ok(out)
}

/// Indent (`outdent` false) or outdent by one unit every line overlapping `range`; blank lines are left alone. With a
/// range, a line starting inside protected text (a verbatim element or an attribute value) is refused
/// (`range_in_verbatim`, `range_in_attribute_value`); with `None` (the whole document) such lines are skipped. At most
/// [`MAX_INDENT_LINES`] lines (`range_too_large`).
pub fn indent_edits(doc: &WebDoc, range: Option<Range<usize>>, outdent: bool, unit: &str, work: &mut Work<'_>) -> Result<Vec<TextEdit>, CodeError> {
    let src = doc.source();
    let whole = range.is_none();
    let range = range.unwrap_or(0..src.len());
    if range.start > range.end || range.end > src.len() || !src.is_char_boundary(range.start) || !src.is_char_boundary(range.end) {
        return Err(CodeError::new("range_invalid"));
    }
    if unit.is_empty() || unit.len() > 16 || !unit.bytes().all(|c| c == b' ' || c == b'\t') {
        return Err(CodeError::new("format_option_invalid"));
    }
    let starts = line_starts(src, &range, work)?;
    let protected = protected_spans(doc, work)?;
    // Binary search: the protected span (sorted by start) that holds `s` strictly inside, if any (spans nest only by
    // containment, so the last span starting before `s` and every enclosing one are checked through a running end).
    let mut reach: Vec<usize> = Vec::with_capacity(protected.len());
    let mut far = 0usize;
    for p in &protected {
        far = far.max(p.end);
        reach.push(far);
    }
    let held = |s: usize| {
        let i = protected.partition_point(|p| p.start < s);
        i > 0 && reach.get(i.saturating_sub(1)).is_some_and(|&e| s < e)
    };
    let in_value = |s: usize| doc_value_at(doc, s);
    let mut edits = Vec::new();
    for s in starts {
        work.tick(1)?;
        if held(s) {
            if whole {
                continue;
            }
            return Err(CodeError::new(if in_value(s) { "range_in_attribute_value" } else { "range_in_verbatim" }));
        }
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

/// Whether byte `s` lies inside an attribute value (or right at its end).
fn doc_value_at(doc: &WebDoc, s: usize) -> bool {
    (0..doc.len()).filter_map(|id| doc.node(id)).filter(|n| n.kind == NodeKind::Element && n.open_span.as_ref().is_some_and(|o| o.start < s && s < o.end)).any(|n| n.attrs.iter().any(|a| a.value_span.as_ref().is_some_and(|v| v.start < s && s <= v.end)))
}

// ---------------------------------------------------------------------------------------------------------------------
// Doctype ([STU-WEB-015]).

/// The edit that makes `decl` the document's doctype: the existing doctype node replaced, or the declaration inserted
/// at the start, after a leading U+FEFF and after a leading XML declaration (review F10), with the document's line
/// ending. `xslt_1` (an internal subset an HTML parser does not read as one doctype) is refused on an HTML document.
pub fn doctype_edit(doc: &WebDoc, decl: &DoctypeDeclaration) -> Result<TextEdit, CodeError> {
    if decl.declaration_id == "xslt_1" {
        return Err(CodeError::new("doctype_needs_xml_parser"));
    }
    let src = doc.source();
    let nl = line_ending(src);
    let root = doc.node(doc.root()).map_or(&[][..], |r| r.children.as_slice());
    if let Some(n) = root.iter().filter_map(|&c| doc.node(c)).find(|n| n.kind == NodeKind::Doctype) {
        return if doc.slice(&n.span) == decl.literal { Err(CodeError::new("nothing_to_change")) } else { Ok(TextEdit { span: n.span.clone(), replacement: decl.literal.to_string() }) };
    }
    let start = if src.starts_with('\u{feff}') { '\u{feff}'.len_utf8() } else { 0 };
    // The first node after the BOM and whitespace: an XML declaration keeps its place before the doctype.
    let first = root.iter().filter_map(|&c| doc.node(c)).find(|n| n.span.end > start && !(n.kind == NodeKind::Text && doc.slice(&n.span).trim_start_matches('\u{feff}').trim().is_empty()));
    if let Some(n) = first.filter(|n| n.kind == NodeKind::Unsupported && doc.slice(&n.span).get(..5).is_some_and(|t| t.eq_ignore_ascii_case("<?xml"))) {
        return Ok(TextEdit { span: n.span.end..n.span.end, replacement: format!("{nl}{}", decl.literal) });
    }
    Ok(TextEdit { span: start..start, replacement: format!("{}{nl}", decl.literal) })
}
