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
//! The literal (source) characters between references get the input-stream preprocessing first (13.2.3.5: CR LF and a
//! lone CR become LF; a reference is decoded after it, so `&#13;` stays CR) and the U+0000 rule of the place the text
//! sits in: attribute values (13.2.5.36-13.2.5.38), RCDATA (13.2.5.2) and text in foreign content (13.2.6.5) replace it
//! with U+FFFD; text in HTML content is a NUL character token the "in body" insertion mode ignores (13.2.6.4.7).
//!
//! One reference never grows by more than one byte (`nGt;`: 5 bytes in, 6 out), and every growing reference is at
//! least 5 bytes long, so in `Context::Text` the output is at most `input.len() + input.len() / 5` bytes (per-entry
//! bound checked over the whole table by `tests::table_is_sorted_and_complete`). In the contexts that replace U+0000 a
//! NUL byte becomes the 3 bytes of U+FFFD, so there the output is at most `3 * input.len()` bytes (same test). Linear
//! time: each `&` looks at most `MAX_NAME_LEN + 1` bytes ahead with binary searches over the table.
#![deny(clippy::arithmetic_side_effects)]

use crate::entity_table::{MAX_NAME_LEN, NAMED};
use std::ops::Range;

/// Where the text sits: attribute values have the extra legacy-reference rule; the U+0000 rule differs per place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Context {
    /// Text in HTML content (text nodes whose parent follows the HTML rules): U+0000 is dropped.
    Text,
    /// RCDATA bodies (`title`, `textarea`) and text in foreign content: U+0000 becomes U+FFFD.
    ReplaceNul,
    /// Attribute values (all three quoting styles): U+0000 becomes U+FFFD.
    Attribute,
}

/// Append source text after input-stream preprocessing (CR LF and lone CR become LF) with the U+0000 rule of `context`.
fn push_literal(out: &mut String, s: &str, context: Context) {
    let b = s.as_bytes();
    let mut run = 0usize;
    let mut i = 0usize;
    while let Some(&c) = b.get(i) {
        if c != b'\r' && c != 0 {
            i = i.saturating_add(1);
            continue;
        }
        out.push_str(&s[run..i]);
        if c == b'\r' {
            out.push('\n');
            if b.get(i.saturating_add(1)) == Some(&b'\n') {
                i = i.saturating_add(1);
            }
        } else if context != Context::Text {
            out.push('\u{fffd}');
        }
        i = i.saturating_add(1);
        run = i;
    }
    out.push_str(&s[run..]);
}

/// Source text as the tokenizer hands it on where there are no character references (RAWTEXT and script data bodies,
/// CDATA sections, comment data): input-stream preprocessing (13.2.3.5: CR LF and a lone CR become LF) and the U+0000
/// rule of `context` (U+FFFD, or dropped in `Context::Text`). Consumers read `text`; `span` maps a range of `text` back to
/// the source bytes it came from, for location only.
///
/// The map holds one mark per CR LF pair and per U+0000 of the slice, at most `max_marks` (the document's node limit);
/// beyond that it is dropped and `span` answers the whole slice (bounded memory for hostile input). `text` is at most
/// `3 * src.len()` bytes (each U+0000 becomes the three bytes of U+FFFD).
#[derive(Clone, Debug)]
pub(crate) struct CharData {
    pub text: String,
    /// Source offset of the slice.
    origin: usize,
    /// Length of the source slice.
    len: usize,
    /// (offset in `text`, offset in the slice) after every place where the two stop advancing together; `None` when the
    /// mark budget was exceeded.
    marks: Option<Vec<(usize, usize)>>,
}

impl CharData {
    pub(crate) fn new(src: &str, origin: usize, context: Context, max_marks: usize) -> Self {
        let b = src.as_bytes();
        let mut text = String::with_capacity(src.len());
        let mut marks = Some(Vec::new());
        let mut run = 0usize;
        let mut i = 0usize;
        while let Some(&c) = b.get(i) {
            if c != b'\r' && c != 0 {
                i = i.saturating_add(1);
                continue;
            }
            text.push_str(&src[run..i]);
            let mut mark = c == 0;
            if c == b'\r' {
                text.push('\n');
                if b.get(i.saturating_add(1)) == Some(&b'\n') {
                    i = i.saturating_add(1);
                    mark = true;
                }
            } else if context != Context::Text {
                text.push('\u{fffd}');
            }
            i = i.saturating_add(1);
            run = i;
            if mark && let Some(list) = marks.as_mut() {
                if list.len() >= max_marks {
                    marks = None;
                } else {
                    list.push((text.len(), i));
                }
            }
        }
        text.push_str(&src[run..]);
        Self {
            text,
            origin,
            len: src.len(),
            marks,
        }
    }

    /// Source offset of offset `t` of `text` (a character boundary).
    fn source(&self, t: usize, marks: &[(usize, usize)]) -> usize {
        let k = marks.partition_point(|m| m.0 <= t);
        let (tp, sp) = k
            .checked_sub(1)
            .and_then(|k| marks.get(k))
            .copied()
            .unwrap_or((0, 0));
        self.origin
            .saturating_add(sp)
            .saturating_add(t.saturating_sub(tp))
    }

    /// Source bytes that `r` (a range of `text`) was read from; the whole slice when the map was dropped.
    pub(crate) fn span(&self, r: &Range<usize>) -> Range<usize> {
        match &self.marks {
            Some(marks) => self.source(r.start, marks)..self.source(r.end, marks),
            None => self.origin..self.origin.saturating_add(self.len),
        }
    }
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
    NAMED
        .binary_search_by(|(n, _, _)| n.as_bytes().cmp(name.as_bytes()))
        .ok()
        .map(|i| (NAMED[i].1, NAMED[i].2))
}

/// Longest table name that `after` (the text following `&`) starts with: (bytes consumed, code points).
fn longest_named(after: &str) -> Option<(usize, u32, u32)> {
    let b = after.as_bytes();
    let run = b
        .iter()
        .take(MAX_NAME_LEN.saturating_add(1))
        .take_while(|c| c.is_ascii_alphanumeric())
        .count();
    if run < MAX_NAME_LEN && b.get(run) == Some(&b';') {
        let with_semi = run.saturating_add(1);
        if let Some((a, c)) = lookup(&after[..with_semi]) {
            return Some((with_semi, a, c));
        }
    }
    // Names without `;` are prefixes of the alphanumeric run (only legacy names lack the semicolon).
    (1..=run.min(MAX_NAME_LEN))
        .rev()
        .find_map(|k| lookup(&after[..k]).map(|(a, c)| (k, a, c)))
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

/// The value a tokenizer builds from source text `input` read in `context`: preprocessed literal characters (newlines,
/// U+0000) and decoded character references.
pub(crate) fn decode(input: &str, context: Context) -> Decoded {
    let b = input.as_bytes();
    let mut text = String::with_capacity(input.len());
    let mut unresolved = Vec::new();
    let mut last = 0usize;
    let mut i = 0usize;
    while let Some(off) = b
        .get(i..)
        .and_then(|rest| rest.iter().position(|&c| c == b'&'))
    {
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
                let end = if b.get(p) == Some(&b';') {
                    p.saturating_add(1)
                } else {
                    p
                };
                push_literal(&mut text, &input[last..amp], context);
                text.push(numeric_char(value));
                last = end;
                i = end;
            }
            Some(c) if c.is_ascii_alphanumeric() => match longest_named(&input[after..]) {
                Some((len, first, second)) => {
                    let end = after.saturating_add(len);
                    let terminated = b.get(end.saturating_sub(1)) == Some(&b';');
                    let next_blocks = b
                        .get(end)
                        .is_some_and(|n| *n == b'=' || n.is_ascii_alphanumeric());
                    if context == Context::Attribute && !terminated && next_blocks {
                        i = end;
                        continue;
                    }
                    push_literal(&mut text, &input[last..amp], context);
                    push_code_point(&mut text, first);
                    if second != 0 {
                        push_code_point(&mut text, second);
                    }
                    last = end;
                    i = end;
                }
                None => {
                    let run = b[after..]
                        .iter()
                        .take_while(|c| c.is_ascii_alphanumeric())
                        .count();
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
    push_literal(&mut text, &input[last..], context);
    Decoded { text, unresolved }
}

#[cfg(test)]
mod tests {
    use super::{CharData, Context, decode};

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
        assert_eq!(
            decode(
                "&notit; &notin; &CounterClockwiseContourIntegral;",
                Context::Text
            )
            .text,
            "\u{ac}it; \u{2209} \u{2233}"
        );
        assert_eq!(
            decode("&#x80;&#150;&#x81;", Context::Text).text,
            "\u{20ac}\u{2013}\u{81}"
        );
        // Preprocessing and U+0000 per context; the NUL-replacing bound of the module docs (3 bytes per input byte).
        let raw = "a\r\nb\rc\0&#13;&#0;\r";
        assert_eq!(decode(raw, Context::Text).text, "a\nb\nc\r\u{fffd}\n");
        assert_eq!(
            decode(raw, Context::ReplaceNul).text,
            "a\nb\nc\u{fffd}\r\u{fffd}\n"
        );
        assert_eq!(
            decode(raw, Context::Attribute).text,
            "a\nb\nc\u{fffd}\r\u{fffd}\n"
        );
        let nuls = "\0".repeat(64);
        for context in [Context::ReplaceNul, Context::Attribute] {
            assert_eq!(
                decode(&nuls, context).text.len(),
                nuls.len().saturating_mul(3)
            );
        }
        assert!(decode(&nuls, Context::Text).text.is_empty());
    }

    /// `CharData`: preprocessing without references, and the map back to the source (a U+FFFD maps to its NUL, the LF of
    /// a CR LF to the pair, text after a dropped NUL to the bytes after it); over the mark budget the map answers the whole
    /// slice.
    #[test]
    fn char_data_maps_text_back_to_source() {
        let src = "a\r\nb\0c\rd";
        let d = CharData::new(src, 100, Context::ReplaceNul, 16);
        assert_eq!(d.text, "a\nb\u{fffd}c\nd");
        let at = |s: &str| d.text.find(s).unwrap();
        assert_eq!(d.span(&(at("b")..at("b") + 1)), 103..104);
        assert_eq!(d.span(&(at("\u{fffd}")..at("c"))), 104..105);
        assert_eq!(d.span(&(at("d")..d.text.len())), 107..108);
        assert_eq!(
            d.span(&(1..2)),
            101..103,
            "the LF of CR LF maps to the pair"
        );
        let dropped = CharData::new("x\0\0y", 0, Context::Text, 16);
        assert_eq!(dropped.text, "xy");
        assert_eq!(dropped.span(&(1..2)), 3..4);
        let plain = CharData::new("abc", 7, Context::ReplaceNul, 0);
        assert_eq!(plain.text, "abc");
        assert_eq!(plain.span(&(1..2)), 8..9);
        let over = CharData::new("\0\0\0", 5, Context::ReplaceNul, 2);
        assert_eq!(over.text.len(), 9, "3 bytes per U+0000");
        assert_eq!(over.span(&(3..6)), 5..8, "over budget: the whole slice");
    }
}
