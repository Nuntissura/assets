//! Character references: exact WHATWG decoding of text (delegates to `charref`, complete named table, legacy names
//! without `;`, numeric rules) plus escaping.

#![deny(clippy::arithmetic_side_effects)]

use crate::charref::{Context, decode};
use std::ops::Range;

pub struct Decoded {
    pub text: String,
    /// Byte ranges (in the input) of `&name;` forms whose name is not a character reference; a browser keeps them as
    /// literal text too.
    pub unresolved: Vec<Range<usize>>,
}

/// The text a browser builds from source text in HTML content (data state, "in body"): newlines normalised (CR LF and
/// a lone CR become LF; `&#13;` stays CR), U+0000 dropped, character references decoded exactly as an HTML tokenizer
/// does. The output is at most `input.len() + input.len() / 5` bytes (one byte of growth per reference of five or
/// more; `charref::tests::table_is_sorted_and_complete`).
pub fn decode_entities(input: &str) -> Decoded {
    let d = decode(input, Context::Text);
    Decoded {
        text: d.text,
        unresolved: d.unresolved,
    }
}

/// Escape text node content: `& < >`, and CR as `&#13;` (a literal CR would be read as LF). U+0000 has no text form
/// a parser keeps (dropped in HTML text, U+FFFD elsewhere) and is written unchanged.
pub fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => out.push_str("&#13;"),
            c => out.push(c),
        }
    }
    out
}

/// Escape an attribute value for a double-quoted (`quote == '"'`) or single-quoted (`'\''`) context; CR is written
/// as `&#13;` (a literal CR would be read as LF).
pub fn escape_attr(s: &str, quote: char) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' if quote == '"' => out.push_str("&quot;"),
            '\'' if quote == '\'' => out.push_str("&#39;"),
            '\r' => out.push_str("&#13;"),
            c => out.push(c),
        }
    }
    out
}
