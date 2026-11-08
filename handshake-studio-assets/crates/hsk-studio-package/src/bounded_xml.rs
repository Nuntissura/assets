//! Element nesting of XML text, measured before a recursive parser sees it (lane
//! SMB-PARSER-HOSTILE-1, item 2).
//!
//! roxmltree 0.21.1 parses element nesting by recursion (`tokenizer.rs` `parse_element` calls
//! `parse_content`, which calls `parse_element` again) with no depth limit; its only bound is
//! `nodes_limit`, which counts nodes, not depth. A Rust stack overflow is not a panic: it aborts the
//! process. Every caller that hands file text to roxmltree checks [`xml_nesting_within`] first and
//! refuses the text when it returns `false`.
//!
//! The scan is one pass over the bytes, iterative, with no allocation. It skips comments
//! (`<!--...-->`), CDATA sections (`<![CDATA[...]]>`), processing instructions (`<?...?>`),
//! declarations such as `<!DOCTYPE ...>` (quoted strings and a bracketed internal subset included)
//! and quoted attribute values. A start tag `<name ...>` adds one level, an end tag `</name>`
//! removes one (saturating at zero), an empty-element tag `<name/>` nets zero but is itself one
//! level deeper than its parent (the parser enters it like any element). Any other `<`
//! followed by a byte counts as a start tag, so text the scan does not recognise is measured
//! conservatively (it can only make the count larger). A construction left open at the end of the
//! text stops the scan and the answer is `true`: the parser reports that text as malformed at that
//! point, after every element the scan counted. Tests: `tests/bounded_xml.rs` (table) and
//! `hsk-studio-interop-vector-svg/tests/hostile_xml_depth.rs` (differential against roxmltree).
#![deny(clippy::arithmetic_side_effects)]

/// Deepest element nesting accepted before an XML parse: the same number as Press's
/// `HARD_MAX_DEPTH` (`hsk-studio-press-read/src/error.rs`).
pub const XML_MAX_NESTING: usize = 256;

/// Stack of the thread a guarded XML parse runs on (lead ruling 2026-11-08): measured in debug,
/// roxmltree 0.21.1 takes about 14 KiB per nesting level (it aborts at 73 levels on 1 MiB and 148
/// on 2 MiB; release about 0.8 KiB), so [`XML_MAX_NESTING`] levels need about 3.6 MiB; 8 MiB is the
/// fixed margin. The same input then gets the same answer in debug and release from any caller
/// thread (tests `hsk-studio-interop-vector-svg/tests/hostile_xml_depth.rs`,
/// `hsk-studio-interop-layout/tests/hostile_xml_depth.rs`).
pub const XML_PARSE_STACK_BYTES: usize = 8 << 20;

/// Runs `f` (a guarded XML parse) on a dedicated thread with [`XML_PARSE_STACK_BYTES`] of stack and
/// returns its value; `None` when the thread cannot start or `f` panics, which the caller maps to a
/// typed refusal. Run [`xml_nesting_within`] first: this stack is sized for that cap only.
pub fn on_xml_parse_stack<T: Send, F: FnOnce() -> T + Send>(f: F) -> Option<T> {
    std::thread::scope(|scope| {
        let handle = std::thread::Builder::new().name("hsk-xml-parse".into()).stack_size(XML_PARSE_STACK_BYTES).spawn_scoped(scope, f).ok()?;
        handle.join().ok()
    })
}

/// First index at or after `from` where `needle` starts.
fn find(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    let hay = b.get(from..)?;
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle).map(|p| p.saturating_add(from))
}

/// Index just past a quoted string that opens at `at` (the quote byte), `None` when unterminated.
fn past_quote(b: &[u8], at: usize) -> Option<usize> {
    let q = *b.get(at)?;
    let start = at.saturating_add(1);
    let rel = b.get(start..)?.iter().position(|c| *c == q)?;
    Some(start.saturating_add(rel).saturating_add(1))
}

/// Index just past the `>` that ends a declaration opened at `at` (`<!`): quoted strings and one
/// bracketed internal subset level are skipped. `None` when unterminated.
fn past_declaration(b: &[u8], at: usize) -> Option<usize> {
    let mut i = at.saturating_add(2);
    let mut bracket = 0usize;
    loop {
        match *b.get(i)? {
            b'"' | b'\'' => {
                i = past_quote(b, i)?;
                continue;
            }
            b'[' => bracket = bracket.saturating_add(1),
            b']' => bracket = bracket.saturating_sub(1),
            b'>' if bracket == 0 => return Some(i.saturating_add(1)),
            _ => {}
        }
        i = i.saturating_add(1);
    }
}

/// `(index past the tag, empty-element)` for a start tag opened at `at`; `None` when unterminated.
fn past_start_tag(b: &[u8], at: usize) -> Option<(usize, bool)> {
    let mut i = at.saturating_add(1);
    let mut slash = false;
    loop {
        match *b.get(i)? {
            b'"' | b'\'' => {
                i = past_quote(b, i)?;
                slash = false;
                continue;
            }
            b'>' => return Some((i.saturating_add(1), slash)),
            // Only a `/` right before `>` makes an empty-element tag (`<g/ >` counts as a level).
            b'/' => slash = true,
            _ => slash = false,
        }
        i = i.saturating_add(1);
    }
}

/// `true` when no element of `text` is nested more than `cap` levels deep (module docs).
pub fn xml_nesting_within(text: &str, cap: usize) -> bool {
    let b = text.as_bytes();
    let mut depth = 0usize;
    let mut i = 0usize;
    loop {
        let Some(rel) = b.get(i..).and_then(|rest| rest.iter().position(|c| *c == b'<')) else {
            return true;
        };
        let at = i.saturating_add(rel);
        let rest = b.get(at..).unwrap_or(&[]);
        let next = if rest.starts_with(b"<!--") {
            find(b, at.saturating_add(4), b"-->").map(|e| e.saturating_add(3))
        } else if rest.starts_with(b"<![CDATA[") {
            find(b, at.saturating_add(9), b"]]>").map(|e| e.saturating_add(3))
        } else if rest.starts_with(b"<?") {
            find(b, at.saturating_add(2), b"?>").map(|e| e.saturating_add(2))
        } else if rest.starts_with(b"<!") {
            past_declaration(b, at)
        } else if rest.starts_with(b"</") {
            depth = depth.saturating_sub(1);
            find(b, at.saturating_add(2), b">").map(|e| e.saturating_add(1))
        } else {
            match past_start_tag(b, at) {
                // An empty element is one parse level for the parser too, without staying open.
                Some((end, true)) => {
                    if depth.saturating_add(1) > cap {
                        return false;
                    }
                    Some(end)
                }
                Some((end, false)) => {
                    depth = depth.saturating_add(1);
                    if depth > cap {
                        return false;
                    }
                    Some(end)
                }
                None => None,
            }
        };
        match next {
            Some(n) => i = n,
            None => return true,
        }
    }
}
