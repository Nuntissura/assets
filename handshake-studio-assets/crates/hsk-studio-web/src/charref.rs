//! Character references decoded exactly as the WHATWG HTML tokenizer does (HTML Standard 13.2.5.72-13.2.5.80), so the
//! text the crate classifies or previews is the text a browser builds from the same bytes.
//!
//! - Named references: the longest name of the complete table (`entity_table`, 2231 entries) that the input starts
//!   with, including the 106 legacy names without `;` (`&quot`, `&amp`, `&not`, ...). In an attribute value a match
//!   that does not end in `;` and is followed by `=` or an ASCII alphanumeric stays literal ("historical reasons"
//!   rule); in text it decodes.
//! - Unknown names (ambiguous ampersand state) stay literal; `&name;` forms are reported in `unresolved`.
//! - Numeric references: decimal or hex, any number of digits, `;` optional; 0, surrogates and values above U+10FFFF
//!   become U+FFFD; 0x80-0x9F map through the windows-1252 table; other values are kept (`&#13;` is CR).
//! - `&#` / `&#x` without digits stay literal.
//!
//! One reference never grows by more than one byte (`nGt;`: 5 bytes in, 6 out), and every growing reference is at
//! least 5 bytes long, so the output is at most `input.len() + input.len() / 5` bytes (per-entry bound checked over the
//! whole table by `tests::table_is_sorted_and_complete`). Linear time: each `&` looks at
//! most `MAX_NAME_LEN + 1` bytes ahead with binary searches over the table.
#![deny(clippy::arithmetic_side_effects)]

use crate::entity_table::{MAX_NAME_LEN, NAMED};
use std::ops::Range;

/// Where the text sits: attribute values have the extra legacy-reference rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Context {
    /// Data and RCDATA text (text nodes, `title`/`textarea` bodies, foreign-content text).
    Text,
    /// Attribute values (all three quoting styles).
    Attribute,
}

pub(crate) struct Decoded {
    pub text: String,
    /// Input ranges of `&name;` forms whose name is not in the table (literal text for a browser too).
    pub unresolved: Vec<Range<usize>>,
}

/// windows-1252 replacements for numeric references 0x80-0x9F (HTML Standard 13.2.5.80); `None` keeps the value.
const C1: [Option<char>; 32] = [
    Some('\u{20ac}'),
    None,
    Some('\u{201a}'),
    Some('\u{0192}'),
    Some('\u{201e}'),
    Some('\u{2026}'),
    Some('\u{2020}'),
    Some('\u{2021}'),
    Some('\u{02c6}'),
    Some('\u{2030}'),
    Some('\u{0160}'),
    Some('\u{2039}'),
    Some('\u{0152}'),
    None,
    Some('\u{017d}'),
    None,
    None,
    Some('\u{2018}'),
    Some('\u{2019}'),
    Some('\u{201c}'),
    Some('\u{201d}'),
    Some('\u{2022}'),
    Some('\u{2013}'),
    Some('\u{2014}'),
    Some('\u{02dc}'),
    Some('\u{2122}'),
    Some('\u{0161}'),
    Some('\u{203a}'),
    Some('\u{0153}'),
    None,
    Some('\u{017e}'),
    Some('\u{0178}'),
];

fn lookup(name: &str) -> Option<(u32, u32)> {
    NAMED.binary_search_by(|(n, _, _)| n.as_bytes().cmp(name.as_bytes())).ok().map(|i| (NAMED[i].1, NAMED[i].2))
}

/// Longest table name that `after` (the text following `&`) starts with: (bytes consumed, code points).
fn longest_named(after: &str) -> Option<(usize, u32, u32)> {
    let b = after.as_bytes();
    let run = b.iter().take(MAX_NAME_LEN.saturating_add(1)).take_while(|c| c.is_ascii_alphanumeric()).count();
    if run < MAX_NAME_LEN && b.get(run) == Some(&b';') {
        let with_semi = run.saturating_add(1);
        if let Some((a, c)) = lookup(&after[..with_semi]) {
            return Some((with_semi, a, c));
        }
    }
    // Names without `;` are prefixes of the alphanumeric run (only legacy names lack the semicolon).
    (1..=run.min(MAX_NAME_LEN)).rev().find_map(|k| lookup(&after[..k]).map(|(a, c)| (k, a, c)))
}

fn numeric_char(value: u32) -> char {
    match value {
        0 | 0xd800..=0xdfff => '\u{fffd}',
        0x80..=0x9f => C1[usize::try_from(value.saturating_sub(0x80)).unwrap_or(0)]
            .or_else(|| char::from_u32(value))
            .unwrap_or('\u{fffd}'),
        v => char::from_u32(v).unwrap_or('\u{fffd}'),
    }
}

fn push_code_point(out: &mut String, cp: u32) {
    out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
}

/// Decode every character reference in `input` read in `context`.
pub(crate) fn decode(input: &str, context: Context) -> Decoded {
    let b = input.as_bytes();
    let mut text = String::with_capacity(input.len());
    let mut unresolved = Vec::new();
    let mut last = 0usize;
    let mut i = 0usize;
    while let Some(off) = b.get(i..).and_then(|rest| rest.iter().position(|&c| c == b'&')) {
        let amp = i.saturating_add(off);
        let after = amp.saturating_add(1);
        match b.get(after) {
            Some(b'#') => {
                let mut p = after.saturating_add(1);
                let hex = matches!(b.get(p), Some(b'x' | b'X'));
                if hex {
                    p = p.saturating_add(1);
                }
                let radix = if hex { 16 } else { 10 };
                let digits_start = p;
                let mut value = 0u32;
                while let Some(d) = b.get(p).and_then(|c| char::from(*c).to_digit(radix)) {
                    value = value.saturating_mul(radix).saturating_add(d);
                    p = p.saturating_add(1);
                }
                if p == digits_start {
                    // Absence of digits: `&#` / `&#x` are literal.
                    i = after;
                    continue;
                }
                let end = if b.get(p) == Some(&b';') { p.saturating_add(1) } else { p };
                text.push_str(&input[last..amp]);
                text.push(numeric_char(value));
                last = end;
                i = end;
            }
            Some(c) if c.is_ascii_alphanumeric() => match longest_named(&input[after..]) {
                Some((len, first, second)) => {
                    let end = after.saturating_add(len);
                    let terminated = b.get(end.saturating_sub(1)) == Some(&b';');
                    let next_blocks = b.get(end).is_some_and(|n| *n == b'=' || n.is_ascii_alphanumeric());
                    if context == Context::Attribute && !terminated && next_blocks {
                        i = end;
                        continue;
                    }
                    text.push_str(&input[last..amp]);
                    push_code_point(&mut text, first);
                    if second != 0 {
                        push_code_point(&mut text, second);
                    }
                    last = end;
                    i = end;
                }
                None => {
                    let run = b[after..].iter().take_while(|c| c.is_ascii_alphanumeric()).count();
                    let run_end = after.saturating_add(run);
                    if b.get(run_end) == Some(&b';') {
                        unresolved.push(amp..run_end.saturating_add(1));
                    }
                    i = run_end;
                }
            },
            _ => i = after,
        }
    }
    text.push_str(&input[last..]);
    Decoded { text, unresolved }
}

#[cfg(test)]
mod tests {
    use super::{Context, decode};

    #[test]
    fn table_is_sorted_and_complete() {
        let names: Vec<&str> = crate::entity_table::NAMED.iter().map(|e| e.0).collect();
        assert!(names.windows(2).all(|w| w[0].as_bytes() < w[1].as_bytes()));
        assert_eq!(names.iter().filter(|n| !n.ends_with(';')).count(), 106);
        // Growth bound stated in the module docs: a named reference grows by at most one byte, and only references of
        // five or more bytes grow (numeric references never grow: a 4-byte character needs at least `&#65536`).
        let utf8 = |cp: u32| char::from_u32(cp).map_or(0, char::len_utf8);
        for (name, first, second) in crate::entity_table::NAMED.iter() {
            let reference = name.len().saturating_add(1);
            let out = utf8(*first).saturating_add(if *second == 0 { 0 } else { utf8(*second) });
            assert!(out <= reference.saturating_add(1), "{name}");
            assert!(out <= reference || reference >= 5, "{name}");
        }
        assert_eq!(decode("&notit; &notin; &CounterClockwiseContourIntegral;", Context::Text).text, "\u{ac}it; \u{2209} \u{2233}");
        assert_eq!(decode("&#x80;&#150;&#x81;", Context::Text).text, "\u{20ac}\u{2013}\u{81}");
    }
}
