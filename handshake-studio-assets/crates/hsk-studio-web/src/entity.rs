//! Character references: decoding of the common named set and numeric references, plus escaping.
//!
//! Subset rules: a reference needs its terminating `;` (legacy semicolon-less forms are left verbatim);
//! numeric references that are NUL, a surrogate or above U+10FFFF decode to U+FFFD as HTML specifies;
//! a well-formed named reference outside the common set is left verbatim and reported in `unresolved`.

#![deny(clippy::arithmetic_side_effects)]

use std::ops::Range;

pub struct Decoded {
    pub text: String,
    /// Byte ranges (in the input) of well-formed references that were left verbatim.
    pub unresolved: Vec<Range<usize>>,
}

const MAX_REF_LEN: usize = 32;

pub(crate) fn named(name: &str) -> Option<char> {
    Some(match name {
        "amp" | "AMP" => '&',
        "lt" | "LT" => '<',
        "gt" | "GT" => '>',
        "quot" | "QUOT" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "copy" | "COPY" => '\u{a9}',
        "reg" | "REG" => '\u{ae}',
        "trade" | "TRADE" => '\u{2122}',
        "hellip" => '\u{2026}',
        "mdash" => '\u{2014}',
        "ndash" => '\u{2013}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "ldquo" => '\u{201c}',
        "rdquo" => '\u{201d}',
        "laquo" => '\u{ab}',
        "raquo" => '\u{bb}',
        "bull" => '\u{2022}',
        "middot" => '\u{b7}',
        "euro" => '\u{20ac}',
        "pound" => '\u{a3}',
        "yen" => '\u{a5}',
        "cent" => '\u{a2}',
        "sect" => '\u{a7}',
        "deg" => '\u{b0}',
        "plusmn" => '\u{b1}',
        "times" => '\u{d7}',
        "divide" => '\u{f7}',
        "larr" => '\u{2190}',
        "uarr" => '\u{2191}',
        "rarr" => '\u{2192}',
        "darr" => '\u{2193}',
        "shy" => '\u{ad}',
        "ensp" => '\u{2002}',
        "emsp" => '\u{2003}',
        "thinsp" => '\u{2009}',
        _ => return None,
    })
}

fn numeric(body: &str) -> Option<char> {
    let (digits, radix) = match body.strip_prefix(['x', 'X']) {
        Some(hex) => (hex, 16),
        None => (body, 10),
    };
    if digits.is_empty() || digits.len() > 8 || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    let value = u32::from_str_radix(digits, radix).ok()?;
    Some(match char::from_u32(value) {
        Some('\0') | None => '\u{fffd}',
        Some(c) => c,
    })
}

fn looks_like_reference(name: &str) -> bool {
    match name.strip_prefix('#') {
        Some(rest) => !rest.is_empty(),
        None => {
            let mut chars = name.chars();
            matches!(chars.next(), Some(c) if c.is_ascii_alphabetic()) && chars.all(|c| c.is_ascii_alphanumeric())
        }
    }
}

/// Decode references in `input`. Output is never longer than the input.
pub fn decode_entities(input: &str) -> Decoded {
    let bytes = input.as_bytes();
    let mut text = String::with_capacity(input.len());
    let mut unresolved = Vec::new();
    let mut last = 0;
    let mut i = 0;
    while let Some(off) = bytes[i..].iter().position(|&c| c == b'&') {
        let amp = i.saturating_add(off);
        let body_start = amp.saturating_add(1);
        let window_end = body_start.saturating_add(MAX_REF_LEN).min(bytes.len());
        let Some(semi) = bytes[body_start..window_end].iter().position(|&c| c == b';') else {
            i = body_start;
            continue;
        };
        let body_end = body_start.saturating_add(semi);
        let body = &input[body_start..body_end];
        let end = body_end.saturating_add(1);
        let decoded = match body.strip_prefix('#') {
            Some(n) => numeric(n),
            None => named(body),
        };
        match decoded {
            Some(ch) => {
                text.push_str(&input[last..amp]);
                text.push(ch);
                last = end;
            }
            None if looks_like_reference(body) => unresolved.push(amp..end),
            None => {}
        }
        i = if decoded.is_some() { end } else { body_start };
    }
    text.push_str(&input[last..]);
    Decoded { text, unresolved }
}

/// Escape text node content: `& < >`.
pub fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

/// Escape an attribute value for a double-quoted (`quote == '"'`) or single-quoted (`'\''`) context.
pub fn escape_attr(s: &str, quote: char) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' if quote == '"' => out.push_str("&quot;"),
            '\'' if quote == '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}
