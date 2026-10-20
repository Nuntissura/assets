//! Bounded CSS helpers for the subset the web module understands: a flat rule scanner with `@media`
//! contexts, simple selectors and declaration lists (URL references live in `cssurl`). Nothing here
//! evaluates layout or the full cascade; whatever falls outside the subset is reported, not guessed.

#![deny(clippy::arithmetic_side_effects)]

use crate::dom::{Limits, find};
use crate::error::{LimitKind, MalformedKind, WebError};
use crate::responsive::{Breakpoint, parse_media_query};
use hsk_studio_accord::CancellationToken;
use std::ops::Range;

const MAX_MEDIA_NESTING: usize = 16;

fn css_error(span: Range<usize>) -> WebError {
    WebError::Malformed {
        kind: MalformedKind::UnterminatedCss,
        span,
    }
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-' || c == b'_'
}

/// Index arithmetic that cannot overflow (all indices are bounded by the source length).
fn add(a: usize, b: usize) -> usize {
    a.saturating_add(b)
}

/// End index (exclusive) of the string literal starting at `b[i]`; `None` when unterminated. A newline ends it: CSS
/// preprocessing reads CR, FF and CR LF as LF (CSS Syntax 3, 3.3).
fn skip_string(b: &[u8], i: usize) -> Option<usize> {
    let q = b[i];
    let mut j = add(i, 1);
    loop {
        match b.get(j) {
            None | Some(b'\n' | b'\r' | 0x0c) => return None,
            // An escaped newline continues the string; CR LF is one newline.
            Some(b'\\') if b.get(add(j, 1)) == Some(&b'\r') && b.get(add(j, 2)) == Some(&b'\n') => {
                j = add(j, 3)
            }
            Some(b'\\') => j = add(j, 2),
            Some(&c) if c == q => return Some(add(j, 1)),
            Some(_) => j = add(j, 1),
        }
    }
}

pub(crate) struct MediaCtx {
    pub query_span: Range<usize>,
    pub breakpoints: Result<Vec<Breakpoint>, WebError>,
    pub parent: Option<usize>,
}

pub(crate) struct RawRule {
    pub selector_span: Range<usize>,
    /// Selector text as the CSS parser reads it (from the preprocessed style text, not the source bytes).
    pub selector: String,
    /// Declaration block contents as the CSS parser reads them.
    pub declarations: String,
    pub media: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleRefusal {
    /// `@font-face`, `@keyframes`, `@supports`, `@import`, ... : skipped as a whole.
    AtRule,
    /// Combinators, attribute selectors, pseudo-classes/elements, or malformed selector lists.
    Selector,
    /// A block nested inside a declaration block (CSS nesting).
    NestedBlock,
}
impl RuleRefusal {
    pub const fn code(self) -> &'static str {
        match self {
            Self::AtRule => "at_rule",
            Self::Selector => "selector",
            Self::NestedBlock => "nested_block",
        }
    }
}

#[derive(Default)]
pub(crate) struct Stylesheet {
    pub media: Vec<MediaCtx>,
    pub rules: Vec<RawRule>,
    pub unsupported: Vec<(Range<usize>, RuleRefusal)>,
}

/// Maps a range of the scanned text to the source bytes it came from (location only).
pub(crate) type Locate<'a> = &'a dyn Fn(&Range<usize>) -> Range<usize>;

/// Index of the first `{`, `;` or `}` at or after `from`, outside strings, comments and parentheses.
fn prelude_end(b: &[u8], from: usize, at: Locate<'_>) -> Result<(usize, u8), WebError> {
    let len = b.len();
    let (mut j, mut parens) = (from, 0usize);
    while j < len {
        match b[j] {
            b'/' if b.get(add(j, 1)) == Some(&b'*') => match find(&b[add(j, 2)..], b"*/") {
                Some(off) => j = add(add(j, 2), add(off, 2)),
                None => return Err(css_error(at(&(j..len)))),
            },
            b'"' | b'\'' => j = skip_string(b, j).ok_or_else(|| css_error(at(&(j..len))))?,
            b'(' => {
                parens = add(parens, 1);
                j = add(j, 1);
            }
            b')' => {
                parens = parens.saturating_sub(1);
                j = add(j, 1);
            }
            c @ (b'{' | b';' | b'}') if parens == 0 => return Ok((j, c)),
            _ => j = add(j, 1),
        }
    }
    Err(css_error(at(&(from..len))))
}

/// End (exclusive) of the block opened at `b[open]`, and whether it contains a nested block.
fn block_end(b: &[u8], open: usize, at: Locate<'_>) -> Result<(usize, bool), WebError> {
    let len = b.len();
    let (mut j, mut depth, mut nested) = (open, 0usize, false);
    while j < len {
        match b[j] {
            b'/' if b.get(add(j, 1)) == Some(&b'*') => match find(&b[add(j, 2)..], b"*/") {
                Some(off) => j = add(add(j, 2), add(off, 2)),
                None => return Err(css_error(at(&(j..len)))),
            },
            b'"' | b'\'' => j = skip_string(b, j).ok_or_else(|| css_error(at(&(j..len))))?,
            b'{' => {
                depth = add(depth, 1);
                nested |= depth > 1;
                j = add(j, 1);
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                j = add(j, 1);
                if depth == 0 {
                    return Ok((j, nested));
                }
            }
            _ => j = add(j, 1),
        }
    }
    Err(css_error(at(&(open..len))))
}

fn trimmed(css: &str, range: Range<usize>) -> Range<usize> {
    let raw = &css[range.clone()];
    let lead = raw.len().saturating_sub(raw.trim_start().len());
    let start = add(range.start, lead);
    start..add(start, raw.trim().len())
}

/// Scan one stylesheet into `out` (flat rules with their `@media` context chain). `css` is the text a browser's CSS
/// parser reads (`WebDoc::sheet`: preprocessed, not the source bytes); selectors and declarations are taken from it, and
/// spans are document offsets through `at`.
pub(crate) fn parse_stylesheet(
    css: &str,
    at: Locate<'_>,
    out: &mut Stylesheet,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<(), WebError> {
    let b = css.as_bytes();
    let len = b.len();
    let mut stack: Vec<usize> = Vec::new();
    let (mut i, mut ticks) = (0usize, 0u32);
    loop {
        while i < len {
            if b[i].is_ascii_whitespace() {
                i = add(i, 1);
            } else if b[i] == b'/' && b.get(add(i, 1)) == Some(&b'*') {
                let off = find(&b[add(i, 2)..], b"*/").ok_or_else(|| css_error(at(&(i..len))))?;
                i = add(add(i, 2), add(off, 2));
            } else {
                break;
            }
        }
        if i >= len {
            break;
        }
        ticks = ticks.wrapping_add(1);
        if ticks & 0xff == 0 {
            cancel.check()?;
        }
        if add(out.rules.len(), out.media.len()) >= limits.max_rules {
            return Err(WebError::LimitExceeded {
                limit: LimitKind::Rules,
                span: at(&(i..add(i, 1))),
            });
        }
        match b[i] {
            b'}' => {
                if stack.pop().is_none() {
                    return Err(css_error(at(&(i..add(i, 1)))));
                }
                i = add(i, 1);
            }
            b'@' => {
                let mut name_end = add(i, 1);
                while name_end < len && is_ident_byte(b[name_end]) {
                    name_end = add(name_end, 1);
                }
                let name = css[add(i, 1)..name_end].to_ascii_lowercase();
                let (end, term) = prelude_end(b, name_end, at)?;
                if name == "media" && term == b'{' {
                    if stack.len() >= MAX_MEDIA_NESTING {
                        return Err(WebError::LimitExceeded {
                            limit: LimitKind::Depth,
                            span: at(&(i..end)),
                        });
                    }
                    let q = trimmed(css, name_end..end);
                    let breakpoints = parse_media_query(&css[q.clone()]).map_err(|e| {
                        e.map_span(|s| at(&(add(q.start, s.start)..add(q.start, s.end))))
                    });
                    out.media.push(MediaCtx {
                        query_span: at(&q),
                        breakpoints,
                        parent: stack.last().copied(),
                    });
                    stack.push(out.media.len().saturating_sub(1));
                    i = add(end, 1);
                } else if term == b'{' {
                    let (after, _) = block_end(b, end, at)?;
                    out.unsupported.push((at(&(i..after)), RuleRefusal::AtRule));
                    i = after;
                } else if term == b';' {
                    out.unsupported
                        .push((at(&(i..add(end, 1))), RuleRefusal::AtRule));
                    i = add(end, 1);
                } else {
                    out.unsupported.push((at(&(i..end)), RuleRefusal::AtRule));
                    i = end;
                }
            }
            _ => {
                let (end, term) = prelude_end(b, i, at)?;
                let sel = trimmed(css, i..end);
                match term {
                    b'{' => {
                        let (after, nested) = block_end(b, end, at)?;
                        if nested {
                            out.unsupported
                                .push((at(&(sel.start..after)), RuleRefusal::NestedBlock));
                        } else {
                            let decls = add(end, 1)..after.saturating_sub(1);
                            out.rules.push(RawRule {
                                selector_span: at(&sel),
                                selector: css[sel.clone()].to_owned(),
                                declarations: css[decls].to_owned(),
                                media: stack.last().copied(),
                            });
                        }
                        i = after;
                    }
                    b';' => {
                        out.unsupported
                            .push((at(&(sel.start..add(end, 1))), RuleRefusal::Selector));
                        i = add(end, 1);
                    }
                    _ => {
                        out.unsupported
                            .push((at(&(sel.start..end)), RuleRefusal::Selector));
                        i = end;
                    }
                }
            }
        }
    }
    if !stack.is_empty() {
        return Err(css_error(at(&(len..len))));
    }
    Ok(())
}

/// One simple-selector compound: optional tag (or `*`), ids and classes, no combinators.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Compound {
    pub tag: Option<String>,
    pub ids: Vec<String>,
    pub classes: Vec<String>,
}
impl Compound {
    pub fn specificity(&self) -> u32 {
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        count(self.ids.len())
            .saturating_mul(100)
            .saturating_add(count(self.classes.len()).saturating_mul(10))
            .saturating_add(u32::from(self.tag.is_some()))
    }
    pub fn matches(&self, e: &ElemInfo) -> bool {
        self.tag.as_ref().is_none_or(|t| *t == e.tag)
            && self.ids.iter().all(|i| e.id.as_ref() == Some(i))
            && self.classes.iter().all(|c| e.classes.contains(c))
    }
}

pub(crate) struct ElemInfo {
    pub tag: String,
    pub id: Option<String>,
    pub classes: Vec<String>,
}

fn ident(s: &str, from: usize) -> Option<usize> {
    let b = s.as_bytes();
    let first = *b.get(from)?;
    if !(first.is_ascii_alphabetic() || first == b'_' || first == b'-') {
        return None;
    }
    let mut j = add(from, 1);
    while j < b.len() && is_ident_byte(b[j]) {
        j = add(j, 1);
    }
    Some(j)
}

/// Parse a selector list of compounds; `None` when any part is outside the subset (the whole rule is then
/// reported, as a browser would drop the rule on an invalid selector).
pub(crate) fn parse_selectors(text: &str) -> Option<Vec<Compound>> {
    let mut out = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        let mut c = Compound {
            tag: None,
            ids: Vec::new(),
            classes: Vec::new(),
        };
        let mut i = 0;
        if part.starts_with('*') {
            i = 1;
        } else if let Some(end) = ident(part, 0) {
            c.tag = Some(part[..end].to_ascii_lowercase());
            i = end;
        }
        while i < part.len() {
            let kind = part.as_bytes()[i];
            let name_start = add(i, 1);
            let end = ident(part, name_start)?;
            match kind {
                b'.' => c.classes.push(part[name_start..end].to_owned()),
                b'#' => c.ids.push(part[name_start..end].to_owned()),
                _ => return None,
            }
            i = end;
        }
        out.push(c);
    }
    Some(out)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub property: String,
    pub value: String,
    pub important: bool,
}

/// Split a declaration list on `;` outside strings and parentheses.
pub(crate) fn parse_declarations(text: &str) -> Vec<Declaration> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let (mut start, mut parens, mut quote) = (0usize, 0usize, None::<u8>);
    let mut push = |chunk: &str| {
        let Some((prop, value)) = chunk.split_once(':') else {
            return;
        };
        let property = prop.trim().to_ascii_lowercase();
        if property.is_empty() || !property.bytes().all(is_ident_byte) {
            return;
        }
        let mut value = value.trim().to_owned();
        let mut important = false;
        let lower = value.to_ascii_lowercase();
        if let Some(pos) = lower.rfind("!important")
            && lower[add(pos, 10)..].trim().is_empty()
        {
            value.truncate(pos);
            value = value.trim_end().to_owned();
            important = true;
        }
        out.push(Declaration {
            property,
            value,
            important,
        });
    };
    for (i, &c) in b.iter().enumerate() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(c),
            (None, b'(') => parens = add(parens, 1),
            (None, b')') => parens = parens.saturating_sub(1),
            (None, b';') if parens == 0 => {
                push(&text[start..i]);
                start = add(i, 1);
            }
            _ => {}
        }
    }
    push(&text[start..]);
    out
}
