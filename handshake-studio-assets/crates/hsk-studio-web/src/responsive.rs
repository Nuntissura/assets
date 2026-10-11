//! Responsive breakpoint descriptors: CSS `@media` min-width/max-width in px for the documented subset.
//! Anything else is a typed `Unsupported` error naming the kind (media type, feature, unit, syntax); it is
//! never guessed. Spans in errors are byte offsets into the text that was parsed.

use crate::dom::{ContentModel, Limits, NodeKind, WebDoc};
use crate::error::{LimitKind, MalformedKind, UnsupportedKind, WebError};
use hsk_studio_accord::CancellationToken;
use std::ops::Range;

/// One conjunction of width bounds; a media query list is the disjunction of its breakpoints.
/// `min_width_px`/`max_width_px` are inclusive as in CSS.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Breakpoint {
    pub min_width_px: Option<f64>,
    pub max_width_px: Option<f64>,
}
impl Breakpoint {
    pub fn matches(&self, width_px: f64) -> bool {
        self.min_width_px.is_none_or(|m| width_px >= m)
            && self.max_width_px.is_none_or(|m| width_px <= m)
    }
}

/// True when any breakpoint of the list matches `width_px`.
pub fn matches_any(list: &[Breakpoint], width_px: f64) -> bool {
    list.iter().any(|b| b.matches(width_px))
}

enum Tok {
    Word(Range<usize>),
    Paren(Range<usize>),
}

fn malformed(span: Range<usize>) -> WebError {
    WebError::Malformed {
        kind: MalformedKind::InvalidMediaQuery,
        span,
    }
}
fn unsupported(kind: UnsupportedKind, span: Range<usize>) -> WebError {
    WebError::Unsupported { kind, span }
}

fn parse_px(value: &str, span: Range<usize>) -> Result<f64, WebError> {
    let value = value.trim();
    let digits = value
        .bytes()
        .take_while(|c| c.is_ascii_digit() || *c == b'.')
        .count();
    if digits == 0 {
        return Err(malformed(span));
    }
    let number: f64 = value[..digits]
        .parse()
        .map_err(|_| malformed(span.clone()))?;
    let unit = &value[digits..];
    if unit.eq_ignore_ascii_case("px") || (unit.is_empty() && number == 0.0) {
        Ok(number)
    } else if !unit.is_empty() && unit.bytes().all(|c| c.is_ascii_alphabetic() || c == b'%') {
        Err(unsupported(UnsupportedKind::MediaUnit, span))
    } else {
        Err(malformed(span))
    }
}

fn apply_feature(query: &str, inner: Range<usize>, bp: &mut Breakpoint) -> Result<(), WebError> {
    let text = &query[inner.clone()];
    if text.contains(['<', '>', '=', '(']) {
        return Err(unsupported(UnsupportedKind::MediaSyntax, inner));
    }
    let Some((name, value)) = text.split_once(':') else {
        return Err(unsupported(UnsupportedKind::MediaFeature, inner));
    };
    let name = name.trim().to_ascii_lowercase();
    match name.as_str() {
        "min-width" => {
            let v = parse_px(value, inner)?;
            bp.min_width_px = Some(bp.min_width_px.map_or(v, |m| m.max(v)));
        }
        "max-width" => {
            let v = parse_px(value, inner)?;
            bp.max_width_px = Some(bp.max_width_px.map_or(v, |m| m.min(v)));
        }
        _ => return Err(unsupported(UnsupportedKind::MediaFeature, inner)),
    }
    Ok(())
}

fn parse_part(query: &str, range: Range<usize>) -> Result<Breakpoint, WebError> {
    let b = query.as_bytes();
    let mut toks = Vec::new();
    let mut i = range.start;
    while i < range.end {
        if b[i].is_ascii_whitespace() {
            i += 1;
        } else if b[i] == b'(' {
            let Some(off) = b[i + 1..range.end]
                .iter()
                .position(|&c| c == b')' || c == b'(')
            else {
                return Err(malformed(i..range.end));
            };
            if b[i + 1 + off] == b'(' {
                return Err(unsupported(UnsupportedKind::MediaSyntax, i..i + 2 + off));
            }
            toks.push(Tok::Paren(i + 1..i + 1 + off));
            i += off + 2;
        } else {
            let start = i;
            while i < range.end && !b[i].is_ascii_whitespace() && b[i] != b'(' {
                i += 1;
            }
            toks.push(Tok::Word(start..i));
        }
    }
    let mut bp = Breakpoint::default();
    if toks.is_empty() {
        return Ok(bp);
    }
    let word = |r: &Range<usize>| query[r.clone()].to_ascii_lowercase();
    let mut k = 0;
    if let Some(Tok::Word(r)) = toks.first()
        && word(r) == "only"
    {
        k = 1;
    }
    let mut feature_first = true;
    if let Some(Tok::Word(r)) = toks.get(k) {
        match word(r).as_str() {
            "all" | "screen" => {
                k += 1;
                feature_first = false;
            }
            "not" | "or" => return Err(unsupported(UnsupportedKind::MediaSyntax, r.clone())),
            "and" => return Err(malformed(r.clone())),
            _ => return Err(unsupported(UnsupportedKind::MediaType, r.clone())),
        }
    }
    while k < toks.len() || feature_first {
        if !feature_first {
            match &toks[k] {
                Tok::Word(r) => match word(r).as_str() {
                    "and" => k += 1,
                    "or" => return Err(unsupported(UnsupportedKind::MediaSyntax, r.clone())),
                    _ => return Err(malformed(r.clone())),
                },
                Tok::Paren(r) => return Err(malformed(r.clone())),
            }
        }
        feature_first = false;
        match toks.get(k) {
            Some(Tok::Paren(r)) => {
                apply_feature(query, r.clone(), &mut bp)?;
                k += 1;
            }
            _ => return Err(malformed(range.end..range.end)),
        }
    }
    Ok(bp)
}

/// Parse a media query list (`screen and (min-width: 600px), (max-width: 400px)`) into breakpoints.
/// An empty list is one unconstrained breakpoint, like `all`.
pub fn parse_media_query(query: &str) -> Result<Vec<Breakpoint>, WebError> {
    let b = query.as_bytes();
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'(' => depth += 1,
            b')' => {
                depth = depth.checked_sub(1).ok_or_else(|| malformed(i..i + 1))?;
            }
            b',' if depth == 0 => {
                parts.push(start..i);
                start = i + 1;
            }
            _ => {}
        }
    }
    if depth != 0 {
        return Err(malformed(start..b.len()));
    }
    parts.push(start..b.len());
    let multiple = parts.len() > 1;
    let mut out = Vec::with_capacity(parts.len());
    for part in parts {
        if multiple && query[part.clone()].trim().is_empty() {
            return Err(malformed(part));
        }
        out.push(parse_part(query, part)?);
    }
    Ok(out)
}

/// One `@media` rule found in CSS text. Spans are document byte offsets when scanned through
/// `document_media_blocks`, or offsets plus the caller's base for `scan_media_blocks`.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaBlock {
    pub query_span: Range<usize>,
    /// Between the opening and closing brace.
    pub body_span: Range<usize>,
    /// Number of enclosing `@media` rules.
    pub depth: usize,
    /// `Err` is a typed unsupported/malformed finding for this query only; the scan continues.
    pub breakpoints: Result<Vec<Breakpoint>, WebError>,
}

fn at_media(b: &[u8], i: usize) -> bool {
    b.len() >= i + 6
        && b[i + 1..i + 6].eq_ignore_ascii_case(b"media")
        && !matches!(b.get(i + 6), Some(c) if c.is_ascii_alphanumeric() || *c == b'-' || *c == b'_')
}

fn css_error(span: Range<usize>) -> WebError {
    WebError::Malformed {
        kind: MalformedKind::UnterminatedCss,
        span,
    }
}

/// Find every `@media` rule in `css` (comments and strings are skipped, nested rules reported with depth).
pub fn scan_media_blocks(
    css: &str,
    base: usize,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<Vec<MediaBlock>, WebError> {
    let b = css.as_bytes();
    let len = b.len();
    let mut blocks: Vec<MediaBlock> = Vec::new();
    let mut open: Vec<(usize, usize)> = Vec::new();
    let mut brace = 0usize;
    let mut ticks = 0u32;
    let mut i = 0;
    while i < len {
        ticks = ticks.wrapping_add(1);
        if ticks & 0x3ff == 0 {
            cancel.check()?;
        }
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let Some(off) = b[i + 2..].windows(2).position(|w| w == b"*/") else {
                    return Err(css_error(base + i..base + len));
                };
                i += 2 + off + 2;
            }
            q @ (b'"' | b'\'') => {
                let mut j = i + 1;
                loop {
                    match b.get(j) {
                        None | Some(b'\n') => return Err(css_error(base + i..base + j.min(len))),
                        Some(b'\\') => j += 2,
                        Some(&c) if c == q => break,
                        Some(_) => j += 1,
                    }
                }
                i = j + 1;
            }
            b'{' => {
                brace += 1;
                i += 1;
            }
            b'}' => {
                brace = brace
                    .checked_sub(1)
                    .ok_or_else(|| css_error(base + i..base + i + 1))?;
                if let Some(&(bi, d)) = open.last()
                    && d == brace
                {
                    blocks[bi].body_span.end = base + i;
                    open.pop();
                }
                i += 1;
            }
            b'@' if at_media(b, i) => {
                let ps = i + 6;
                let mut j = ps;
                while j < len && b[j] != b'{' && b[j] != b';' {
                    j += 1;
                }
                if j >= len {
                    return Err(css_error(base + i..base + len));
                }
                if b[j] == b';' {
                    return Err(malformed(base + i..base + j + 1));
                }
                if blocks.len() >= limits.max_nodes {
                    return Err(WebError::LimitExceeded {
                        limit: LimitKind::MediaBlocks,
                        span: base + i..base + j,
                    });
                }
                let raw = &css[ps..j];
                let lead = raw.len() - raw.trim_start().len();
                let (qs, qe) = (ps + lead, ps + lead + raw.trim().len());
                let breakpoints = parse_media_query(&css[qs..qe]).map_err(|e| e.shift(base + qs));
                blocks.push(MediaBlock {
                    query_span: base + qs..base + qe,
                    body_span: base + j + 1..base + j + 1,
                    depth: open.len(),
                    breakpoints,
                });
                open.push((blocks.len() - 1, brace));
                brace += 1;
                i = j + 1;
            }
            _ => i += 1,
        }
    }
    if brace != 0 || !open.is_empty() {
        return Err(css_error(base + len..base + len));
    }
    Ok(blocks)
}

/// All `@media` rules inside the document's `<style>` bodies, in document order and document offsets.
pub fn document_media_blocks(
    doc: &WebDoc,
    cancel: &CancellationToken,
) -> Result<Vec<MediaBlock>, WebError> {
    let mut out = Vec::new();
    for id in 0..doc.len() {
        let Some(node) = doc.node(id) else { continue };
        let is_style = node.kind == NodeKind::Element
            && node.content == ContentModel::RawText
            && doc
                .tag_name(id)
                .is_some_and(|n| n.eq_ignore_ascii_case("style"));
        if !is_style {
            continue;
        }
        for &child in &node.children {
            let Some(raw) = doc.node(child).filter(|c| c.kind == NodeKind::Raw) else {
                continue;
            };
            out.extend(scan_media_blocks(
                doc.slice(&raw.span),
                raw.span.start,
                doc.limits(),
                cancel,
            )?);
        }
    }
    Ok(out)
}
