//! Bounded CSS helpers for the subset the web module understands: `url(...)`/`@import` references, a flat
//! rule scanner with `@media` contexts, simple selectors and declaration lists. Nothing here evaluates
//! layout or the full cascade; whatever falls outside the subset is reported, never guessed.

use crate::dom::{Limits, find};
use crate::error::{LimitKind, MalformedKind, WebError};
use crate::responsive::{Breakpoint, parse_media_query};
use hsk_studio_accord::CancellationToken;
use std::ops::Range;

const MAX_MEDIA_NESTING: usize = 16;

fn css_error(span: Range<usize>) -> WebError {
    WebError::Malformed { kind: MalformedKind::UnterminatedCss, span }
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-' || c == b'_'
}

/// End index (exclusive) of the string literal starting at `b[i]`; `None` when unterminated.
fn skip_string(b: &[u8], i: usize) -> Option<usize> {
    let q = b[i];
    let mut j = i + 1;
    loop {
        match b.get(j) {
            None | Some(b'\n') => return None,
            Some(b'\\') => j += 2,
            Some(&c) if c == q => return Some(j + 1),
            Some(_) => j += 1,
        }
    }
}

/// Ranges of reference text in `url(...)` and `@import "..."` (comments and unrelated strings skipped).
pub(crate) fn css_references(css: &str, limit: usize) -> Vec<Range<usize>> {
    let b = css.as_bytes();
    let len = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < len && out.len() < limit {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'*') => match find(&b[i + 2..], b"*/") {
                Some(off) => i += 2 + off + 2,
                None => break,
            },
            b'"' | b'\'' => i = skip_string(b, i).unwrap_or(len),
            b'@' if b.len() >= i + 7 && b[i + 1..i + 7].eq_ignore_ascii_case(b"import") => {
                let mut j = i + 7;
                while j < len && b[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < len
                    && matches!(b[j], b'"' | b'\'')
                    && let Some(end) = skip_string(b, j)
                {
                    out.push(j + 1..end - 1);
                    i = end;
                } else {
                    i += 7;
                }
            }
            b'u' | b'U' if b.len() >= i + 4 && b[i..i + 4].eq_ignore_ascii_case(b"url(") && (i == 0 || !is_ident_byte(b[i - 1])) => {
                let mut j = i + 4;
                while j < len && b[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < len && matches!(b[j], b'"' | b'\'') {
                    match skip_string(b, j) {
                        Some(end) => {
                            out.push(j + 1..end - 1);
                            i = end;
                        }
                        None => break,
                    }
                } else {
                    let mut k = j;
                    while k < len && b[k] != b')' && !b[k].is_ascii_whitespace() {
                        k += 1;
                    }
                    if k > j {
                        out.push(j..k);
                    }
                    i = k.max(i + 4);
                }
            }
            _ => i += 1,
        }
    }
    out
}

pub(crate) struct MediaCtx {
    pub query_span: Range<usize>,
    pub breakpoints: Result<Vec<Breakpoint>, WebError>,
    pub parent: Option<usize>,
}

pub(crate) struct RawRule {
    pub selector_span: Range<usize>,
    pub decl_span: Range<usize>,
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

/// Index of the first `{`, `;` or `}` at or after `from`, outside strings, comments and parentheses.
fn prelude_end(b: &[u8], from: usize, base: usize) -> Result<(usize, u8), WebError> {
    let len = b.len();
    let (mut j, mut parens) = (from, 0usize);
    while j < len {
        match b[j] {
            b'/' if b.get(j + 1) == Some(&b'*') => match find(&b[j + 2..], b"*/") {
                Some(off) => j += 2 + off + 2,
                None => return Err(css_error(base + j..base + len)),
            },
            b'"' | b'\'' => j = skip_string(b, j).ok_or_else(|| css_error(base + j..base + len))?,
            b'(' => {
                parens += 1;
                j += 1;
            }
            b')' => {
                parens = parens.saturating_sub(1);
                j += 1;
            }
            c @ (b'{' | b';' | b'}') if parens == 0 => return Ok((j, c)),
            _ => j += 1,
        }
    }
    Err(css_error(base + from..base + len))
}

/// End (exclusive) of the block opened at `b[open]`, and whether it contains a nested block.
fn block_end(b: &[u8], open: usize, base: usize) -> Result<(usize, bool), WebError> {
    let len = b.len();
    let (mut j, mut depth, mut nested) = (open, 0usize, false);
    while j < len {
        match b[j] {
            b'/' if b.get(j + 1) == Some(&b'*') => match find(&b[j + 2..], b"*/") {
                Some(off) => j += 2 + off + 2,
                None => return Err(css_error(base + j..base + len)),
            },
            b'"' | b'\'' => j = skip_string(b, j).ok_or_else(|| css_error(base + j..base + len))?,
            b'{' => {
                depth += 1;
                nested |= depth > 1;
                j += 1;
            }
            b'}' => {
                depth -= 1;
                j += 1;
                if depth == 0 {
                    return Ok((j, nested));
                }
            }
            _ => j += 1,
        }
    }
    Err(css_error(base + open..base + len))
}

fn trimmed(css: &str, range: Range<usize>) -> Range<usize> {
    let raw = &css[range.clone()];
    let lead = raw.len() - raw.trim_start().len();
    range.start + lead..range.start + lead + raw.trim().len()
}

/// Scan one stylesheet body into `out` (flat rules with their `@media` context chain). Spans are document
/// offsets (`base` + offset in `css`).
pub(crate) fn parse_stylesheet(
    css: &str,
    base: usize,
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
                i += 1;
            } else if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                let off = find(&b[i + 2..], b"*/").ok_or_else(|| css_error(base + i..base + len))?;
                i += 2 + off + 2;
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
        if out.rules.len() + out.media.len() >= limits.max_rules {
            return Err(WebError::LimitExceeded { limit: LimitKind::Rules, span: base + i..base + i + 1 });
        }
        match b[i] {
            b'}' => {
                if stack.pop().is_none() {
                    return Err(css_error(base + i..base + i + 1));
                }
                i += 1;
            }
            b'@' => {
                let mut name_end = i + 1;
                while name_end < len && is_ident_byte(b[name_end]) {
                    name_end += 1;
                }
                let name = css[i + 1..name_end].to_ascii_lowercase();
                let (end, term) = prelude_end(b, name_end, base)?;
                if name == "media" && term == b'{' {
                    if stack.len() >= MAX_MEDIA_NESTING {
                        return Err(WebError::LimitExceeded { limit: LimitKind::Depth, span: base + i..base + end });
                    }
                    let q = trimmed(css, name_end..end);
                    let breakpoints = parse_media_query(&css[q.clone()]).map_err(|e| e.shift(base + q.start));
                    out.media.push(MediaCtx { query_span: base + q.start..base + q.end, breakpoints, parent: stack.last().copied() });
                    stack.push(out.media.len() - 1);
                    i = end + 1;
                } else if term == b'{' {
                    let (after, _) = block_end(b, end, base)?;
                    out.unsupported.push((base + i..base + after, RuleRefusal::AtRule));
                    i = after;
                } else if term == b';' {
                    out.unsupported.push((base + i..base + end + 1, RuleRefusal::AtRule));
                    i = end + 1;
                } else {
                    out.unsupported.push((base + i..base + end, RuleRefusal::AtRule));
                    i = end;
                }
            }
            _ => {
                let (end, term) = prelude_end(b, i, base)?;
                let sel = trimmed(css, i..end);
                match term {
                    b'{' => {
                        let (after, nested) = block_end(b, end, base)?;
                        if nested {
                            out.unsupported.push((base + sel.start..base + after, RuleRefusal::NestedBlock));
                        } else {
                            out.rules.push(RawRule {
                                selector_span: base + sel.start..base + sel.end,
                                decl_span: base + end + 1..base + after - 1,
                                media: stack.last().copied(),
                            });
                        }
                        i = after;
                    }
                    b';' => {
                        out.unsupported.push((base + sel.start..base + end + 1, RuleRefusal::Selector));
                        i = end + 1;
                    }
                    _ => {
                        out.unsupported.push((base + sel.start..base + end, RuleRefusal::Selector));
                        i = end;
                    }
                }
            }
        }
    }
    if !stack.is_empty() {
        return Err(css_error(base + len..base + len));
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
        self.ids.len() as u32 * 100 + self.classes.len() as u32 * 10 + u32::from(self.tag.is_some())
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
    let mut j = from + 1;
    while j < b.len() && is_ident_byte(b[j]) {
        j += 1;
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
        let mut c = Compound { tag: None, ids: Vec::new(), classes: Vec::new() };
        let mut i = 0;
        if part.starts_with('*') {
            i = 1;
        } else if let Some(end) = ident(part, 0) {
            c.tag = Some(part[..end].to_ascii_lowercase());
            i = end;
        }
        while i < part.len() {
            let kind = part.as_bytes()[i];
            let end = ident(part, i + 1)?;
            match kind {
                b'.' => c.classes.push(part[i + 1..end].to_owned()),
                b'#' => c.ids.push(part[i + 1..end].to_owned()),
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
        let Some((prop, value)) = chunk.split_once(':') else { return };
        let property = prop.trim().to_ascii_lowercase();
        if property.is_empty() || !property.bytes().all(is_ident_byte) {
            return;
        }
        let mut value = value.trim().to_owned();
        let mut important = false;
        let lower = value.to_ascii_lowercase();
        if let Some(pos) = lower.rfind("!important")
            && lower[pos + 10..].trim().is_empty()
        {
            value.truncate(pos);
            value = value.trim_end().to_owned();
            important = true;
        }
        out.push(Declaration { property, value, important });
    };
    for (i, &c) in b.iter().enumerate() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(c),
            (None, b'(') => parens += 1,
            (None, b')') => parens = parens.saturating_sub(1),
            (None, b';') if parens == 0 => {
                push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    push(&text[start..]);
    out
}
