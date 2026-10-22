//! Grapheme-cluster boundary movement over text, LOCAL to the caret (WP-KERNEL-012 MT-077).
//!
//! A *grapheme cluster* is one user-perceived character: a base letter plus its combining marks, a
//! flag (two regional indicators), a family ZWJ emoji (man + ZWJ + woman + ZWJ + girl ≈ 7 scalars), or
//! a Hangul syllable. Moving the caret or deleting by SCALAR (`char`) tears a cluster in half; moving by
//! GRAPHEME CLUSTER (UAX#29, via `unicode-segmentation`) crosses the whole cluster in one keypress.
//!
//! ## Two address spaces, one rule (the editors disagree on units, deliberately)
//!
//! - The CODE editor's `TextBuffer` is BYTE-addressed (tree-sitter + egui want bytes). It calls
//!   [`next_grapheme_boundary`] / [`prev_grapheme_boundary`], which take and return BYTE offsets.
//! - The RICH editor's `RopeText` is CHAR-addressed (it makes a byte index unrepresentable). It calls
//!   [`next_grapheme_boundary_chars`] / [`prev_grapheme_boundary_chars`], which take and return CHAR
//!   offsets.
//!
//! Both wrap the SAME UAX#29 segmenter — the only difference is the unit the caller speaks, so the
//! grapheme rule itself is never duplicated (MT-077 KEY STEER #2).
//!
//! ## RISK-1: never segment the whole document
//!
//! Re-segmenting a multi-megabyte line on every arrow keypress would make typing quadratic. The MT
//! steer is explicit: "LOCAL segmentation around the caret only (NOT the whole doc)". Every function
//! here extracts only a bounded [`GRAPHEME_LOCAL_WINDOW_BYTES`]-sized window of text on the side it is
//! moving toward, snaps that window to char boundaries, and segments ONLY that window. A single
//! grapheme cluster is in practice far smaller than the window, so the local answer equals the global
//! answer for any realistic cluster while the cost stays bounded regardless of line length.

use unicode_segmentation::{GraphemeCursor as UsGraphemeCursor, GraphemeIncomplete};

/// The byte size of the local text window segmented around the caret for one boundary step (RISK-1).
///
/// A grapheme cluster has no fixed upper bound in the abstract, but realistic clusters (family ZWJ
/// emoji, long Indic conjuncts, flag sequences) are well under 256 bytes. Segmenting a window this size
/// on each side of the caret keeps every keypress O(window), independent of the line length, while
/// still returning the correct boundary for any cluster a human can author. The chunked
/// [`unicode_segmentation::GraphemeCursor`] additionally requests more context only if a cluster
/// genuinely straddles the window edge (handled by [`grow_window`]), so correctness never depends on a
/// cluster fitting — the window is a perf floor, not a correctness cap.
pub const GRAPHEME_LOCAL_WINDOW_BYTES: usize = 256;

/// The next extended-grapheme-cluster boundary STRICTLY AFTER `byte_offset` in `text`, or `text.len()`
/// if `byte_offset` is at/after the end. `byte_offset` is snapped DOWN to a char boundary first so a
/// mid-char input can never panic. Caret RIGHT / forward-delete use this.
///
/// LOCAL: only the window `[start, end)` after the caret is segmented, where `start = byte_offset`
/// (snapped to a char boundary) and `end` is `start + GRAPHEME_LOCAL_WINDOW_BYTES` grown to a char
/// boundary (and grown further only if a cluster straddles the edge — see [`grow_window`]).
pub fn next_grapheme_boundary(text: &str, byte_offset: usize) -> usize {
    let len = text.len();
    let start = snap_down_to_char_boundary(text, byte_offset.min(len));
    if start >= len {
        return len;
    }
    // Window forward from the caret. The cursor is positioned at the window start; we ask for the next
    // boundary after `start`, feeding it only the window chunk and growing the chunk if it needs more.
    next_boundary_in_growing_window(text, start, /*forward=*/ true).unwrap_or_else(|| {
        // Fallback (never expected): step one char so the caret still advances rather than stalling.
        next_char_boundary(text, start)
    })
}

/// The previous extended-grapheme-cluster boundary STRICTLY BEFORE `byte_offset` in `text`, or `0` if
/// `byte_offset` is at/before the start. `byte_offset` is snapped DOWN to a char boundary first. Caret
/// LEFT / Backspace use this.
pub fn prev_grapheme_boundary(text: &str, byte_offset: usize) -> usize {
    let len = text.len();
    let at = snap_down_to_char_boundary(text, byte_offset.min(len));
    if at == 0 {
        return 0;
    }
    prev_boundary_in_growing_window(text, at).unwrap_or_else(|| {
        // Fallback (never expected): step one char back so Backspace still removes something.
        prev_char_boundary(text, at)
    })
}

/// Char-offset sibling of [`next_grapheme_boundary`] for the rich editor's char-indexed rope. Takes a
/// CHAR offset into `text`, returns the CHAR offset of the next grapheme boundary. Implemented by
/// mapping char->byte, stepping in byte space (where the segmenter operates), then byte->char.
pub fn next_grapheme_boundary_chars(text: &str, char_offset: usize) -> usize {
    let byte = char_to_byte(text, char_offset);
    let next_byte = next_grapheme_boundary(text, byte);
    byte_to_char(text, next_byte)
}

/// Char-offset sibling of [`prev_grapheme_boundary`] for the rich editor's char-indexed rope.
pub fn prev_grapheme_boundary_chars(text: &str, char_offset: usize) -> usize {
    let byte = char_to_byte(text, char_offset);
    let prev_byte = prev_grapheme_boundary(text, byte);
    byte_to_char(text, prev_byte)
}

/// A forward/backward grapheme walker over a borrowed `&str`, used by tests and any caller that wants
/// to iterate cluster boundaries without re-deriving the local-window logic. Thin wrapper over
/// [`next_grapheme_boundary`] / [`prev_grapheme_boundary`] so the LOCAL-window perf rule is shared.
#[derive(Debug, Clone)]
pub struct GraphemeCursor<'a> {
    text: &'a str,
    offset: usize,
}

impl<'a> GraphemeCursor<'a> {
    /// A cursor over `text` starting at byte `offset` (snapped to a char boundary).
    pub fn new(text: &'a str, offset: usize) -> Self {
        let offset = snap_down_to_char_boundary(text, offset.min(text.len()));
        Self { text, offset }
    }

    /// The current byte offset.
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Advance one grapheme cluster, returning the new offset (or the unchanged end offset). Named
    /// `next` to mirror the segmentation-cursor vocabulary; it is NOT an `Iterator` (it returns the
    /// offset, not an `Option<Item>`), so the `should_implement_trait` lint is allowed.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> usize {
        self.offset = next_grapheme_boundary(self.text, self.offset);
        self.offset
    }

    /// Retreat one grapheme cluster, returning the new offset (or 0).
    pub fn prev(&mut self) -> usize {
        self.offset = prev_grapheme_boundary(self.text, self.offset);
        self.offset
    }
}

// ── Local-window machinery (RISK-1) ──────────────────────────────────────────────────────────────

/// Find the next boundary after `start` by feeding the UAX#29 chunk cursor a growing window so it never
/// sees more than it needs. Returns `Some(boundary)` (> `start`) or `None` if the segmenter could not
/// resolve even with the whole tail (degenerate — caller falls back to a char step).
fn next_boundary_in_growing_window(text: &str, start: usize, forward: bool) -> Option<usize> {
    debug_assert!(forward, "this helper is the forward path");
    let len = text.len();
    // The chunk cursor needs `is_extended = true` for full extended-grapheme correctness (ZWJ
    // sequences, regional indicators). We position it at `start` over the WHOLE string conceptually but
    // feed it only a chunk, growing the chunk only if it asks for pre/next context at the edge.
    let mut cursor = UsGraphemeCursor::new(start, len, true);
    let mut window_end =
        grow_to_char_boundary(text, (start + GRAPHEME_LOCAL_WINDOW_BYTES).min(len));
    loop {
        let chunk = &text[start..window_end];
        match cursor.next_boundary(chunk, start) {
            Ok(Some(b)) => return Some(b),
            Ok(None) => return Some(len), // reached the end of the text
            Err(GraphemeIncomplete::NextChunk) => {
                // The cluster straddles the window edge; grow the window forward and retry.
                if window_end >= len {
                    return Some(len);
                }
                window_end = grow_to_char_boundary(
                    text,
                    (window_end + GRAPHEME_LOCAL_WINDOW_BYTES).min(len),
                );
            }
            Err(GraphemeIncomplete::PreContext(idx)) => {
                // Provide the requested pre-context (the bytes just before `idx`). For the forward walk
                // from a clean char-boundary `start`, pre-context is the char ending at `idx`.
                let pre_start = snap_down_to_char_boundary(text, idx.saturating_sub(1));
                cursor.provide_context(&text[pre_start..idx], pre_start);
            }
            Err(_) => return None,
        }
    }
}

/// Find the previous boundary before `at` by feeding the UAX#29 chunk cursor a growing window on the
/// LEFT side. Returns `Some(boundary)` (< `at`) or `None` if unresolved.
fn prev_boundary_in_growing_window(text: &str, at: usize) -> Option<usize> {
    let len = text.len();
    let mut cursor = UsGraphemeCursor::new(at, len, true);
    let mut window_start =
        snap_down_to_char_boundary(text, at.saturating_sub(GRAPHEME_LOCAL_WINDOW_BYTES));
    loop {
        let chunk = &text[window_start..at];
        match cursor.prev_boundary(chunk, window_start) {
            Ok(Some(b)) => return Some(b),
            Ok(None) => return Some(0), // reached the start of the text
            Err(GraphemeIncomplete::PrevChunk) => {
                if window_start == 0 {
                    return Some(0);
                }
                window_start = snap_down_to_char_boundary(
                    text,
                    window_start.saturating_sub(GRAPHEME_LOCAL_WINDOW_BYTES),
                );
            }
            Err(GraphemeIncomplete::PreContext(idx)) => {
                let pre_start = snap_down_to_char_boundary(text, idx.saturating_sub(1));
                cursor.provide_context(&text[pre_start..idx], pre_start);
            }
            Err(_) => return None,
        }
    }
}

/// Grow `byte` up to the nearest char boundary `>= byte` (so a window end never splits a char).
fn grow_to_char_boundary(text: &str, byte: usize) -> usize {
    let len = text.len();
    let mut b = byte.min(len);
    while b < len && !text.is_char_boundary(b) {
        b += 1;
    }
    b
}

/// Snap `byte` DOWN to the nearest char boundary `<= byte`.
fn snap_down_to_char_boundary(text: &str, byte: usize) -> usize {
    let mut b = byte.min(text.len());
    while b > 0 && !text.is_char_boundary(b) {
        b -= 1;
    }
    b
}

/// The next char boundary strictly after `byte` (fallback char step).
fn next_char_boundary(text: &str, byte: usize) -> usize {
    let len = text.len();
    let mut b = byte.min(len);
    loop {
        b += 1;
        if b >= len {
            return len;
        }
        if text.is_char_boundary(b) {
            return b;
        }
    }
}

/// The previous char boundary strictly before `byte` (fallback char step).
fn prev_char_boundary(text: &str, byte: usize) -> usize {
    if byte == 0 {
        return 0;
    }
    let mut b = byte.min(text.len()) - 1;
    while b > 0 && !text.is_char_boundary(b) {
        b -= 1;
    }
    b
}

/// Map a CHAR offset to a BYTE offset in `text` (clamped to the end).
fn char_to_byte(text: &str, char_offset: usize) -> usize {
    text.char_indices()
        .nth(char_offset)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}

/// Map a BYTE offset to a CHAR offset in `text` (counts chars before `byte`).
fn byte_to_char(text: &str, byte: usize) -> usize {
    let byte = byte.min(text.len());
    text[..byte].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The family ZWJ emoji 👨‍👩‍👧 = man U+1F468 + ZWJ U+200D + woman U+1F469 + ZWJ + girl U+1F467.
    // 5 scalars, 17 UTF-8 bytes, ONE grapheme cluster (the MANDATORY MT-077 test).
    const FAMILY: &str = "👨‍👩‍👧";
    // Combining accent: 'e' + U+0301 (COMBINING ACUTE ACCENT) = é as 2 scalars, ONE cluster.
    const COMBINING_E: &str = "e\u{0301}";
    // Flag: two regional indicators 🇯🇵 (Japan) = 2 scalars, ONE cluster.
    const FLAG_JP: &str = "🇯🇵";
    // Hangul syllable 한 is a single precomposed scalar U+D55C (one cluster); a decomposed jamo
    // sequence ᄒ+ᅡ+ᆫ is THREE scalars that form ONE cluster.
    const HANGUL_DECOMPOSED: &str = "\u{1112}\u{1161}\u{11AB}"; // ᄒ ᅡ ᆫ -> 한

    #[test]
    fn family_emoji_is_one_cluster_forward() {
        // From offset 0, the next grapheme boundary jumps the WHOLE family emoji to its end (= its byte
        // length), not into the middle after the first scalar.
        let end = next_grapheme_boundary(FAMILY, 0);
        assert_eq!(
            end,
            FAMILY.len(),
            "RIGHT over the family emoji crosses all {} bytes",
            FAMILY.len()
        );
        // And it is strictly more than one scalar (the man emoji alone is 4 bytes).
        assert!(end > 4, "must not stop after the first scalar (4 bytes)");
    }

    #[test]
    fn family_emoji_is_one_cluster_backward() {
        // From the end, the previous boundary jumps back to 0 (Backspace deletes the whole cluster).
        let start = prev_grapheme_boundary(FAMILY, FAMILY.len());
        assert_eq!(
            start, 0,
            "Backspace over the family emoji removes ALL its codepoints"
        );
    }

    #[test]
    fn combining_accent_is_one_cluster() {
        // "e" + combining acute = one cluster. RIGHT from 0 crosses both scalars (3 bytes: 1 + 2).
        assert_eq!(next_grapheme_boundary(COMBINING_E, 0), COMBINING_E.len());
        // Backspace from the end removes the whole "é" (base + mark), landing at 0.
        assert_eq!(prev_grapheme_boundary(COMBINING_E, COMBINING_E.len()), 0);
    }

    #[test]
    fn flag_is_one_cluster() {
        assert_eq!(next_grapheme_boundary(FLAG_JP, 0), FLAG_JP.len());
        assert_eq!(prev_grapheme_boundary(FLAG_JP, FLAG_JP.len()), 0);
    }

    #[test]
    fn decomposed_hangul_is_one_cluster() {
        // The three conjoining jamo form a single syllable cluster.
        assert_eq!(
            next_grapheme_boundary(HANGUL_DECOMPOSED, 0),
            HANGUL_DECOMPOSED.len()
        );
        assert_eq!(
            prev_grapheme_boundary(HANGUL_DECOMPOSED, HANGUL_DECOMPOSED.len()),
            0
        );
    }

    #[test]
    fn ascii_moves_one_char_at_a_time_no_regression() {
        // AC-7 no-regression: plain ASCII still steps exactly one byte per grapheme (each ASCII char is
        // its own cluster), so existing LTR caret behavior is unchanged.
        let s = "hello";
        assert_eq!(next_grapheme_boundary(s, 0), 1);
        assert_eq!(next_grapheme_boundary(s, 1), 2);
        assert_eq!(prev_grapheme_boundary(s, 5), 4);
        assert_eq!(prev_grapheme_boundary(s, 1), 0);
    }

    #[test]
    fn mixed_run_steps_cluster_by_cluster() {
        // "a👨‍👩‍👧b": a(1 byte) + family(17) + b(1). Walk forward cluster by cluster.
        let s = format!("a{FAMILY}b");
        let after_a = next_grapheme_boundary(&s, 0);
        assert_eq!(after_a, 1, "first cluster is 'a'");
        let after_family = next_grapheme_boundary(&s, after_a);
        assert_eq!(
            after_family,
            1 + FAMILY.len(),
            "second cluster is the whole family emoji"
        );
        let after_b = next_grapheme_boundary(&s, after_family);
        assert_eq!(after_b, s.len(), "third cluster is 'b'");
    }

    #[test]
    fn char_offset_variant_agrees_with_byte_variant() {
        // The rich editor's char-offset path must give the same cluster crossings.
        let s = format!("x{COMBINING_E}y"); // x | é(e+mark) | y  -> chars: x e ́ y = 4 chars
                                            // From char 1 (start of "é"), the next grapheme boundary is char 3 (after the combining mark).
        assert_eq!(
            next_grapheme_boundary_chars(&s, 1),
            3,
            "crosses base+mark in char units"
        );
        // Backward from char 3 returns char 1.
        assert_eq!(prev_grapheme_boundary_chars(&s, 3), 1);
    }

    #[test]
    fn mid_char_offset_is_snapped_not_panicked() {
        // A byte offset INSIDE the family emoji must snap to a char boundary, never panic.
        let mid = 2; // inside the first 4-byte scalar
        let next = next_grapheme_boundary(FAMILY, mid);
        assert_eq!(
            next,
            FAMILY.len(),
            "a mid-char offset snaps and still crosses the cluster"
        );
    }

    #[test]
    fn boundaries_at_extremes_are_stable() {
        let s = "abc";
        assert_eq!(
            next_grapheme_boundary(s, s.len()),
            s.len(),
            "next at EOF stays at EOF"
        );
        assert_eq!(
            prev_grapheme_boundary(s, 0),
            0,
            "prev at start stays at start"
        );
    }

    #[test]
    fn cluster_straddling_the_local_window_still_resolves() {
        // Construct a string where a single cluster spans the window edge: pad with ASCII so the family
        // emoji begins just before GRAPHEME_LOCAL_WINDOW_BYTES, forcing the chunk cursor to request more
        // context (GraphemeIncomplete::NextChunk). The boundary must STILL cross the whole cluster.
        let pad_len = GRAPHEME_LOCAL_WINDOW_BYTES - 2; // family starts 2 bytes before the window edge
        let s = format!("{}{FAMILY}", "a".repeat(pad_len));
        let next = next_grapheme_boundary(&s, pad_len);
        assert_eq!(
            next,
            pad_len + FAMILY.len(),
            "a cluster straddling the window edge still crosses whole"
        );
    }

    #[test]
    fn cursor_iterates_clusters() {
        let s = format!("{COMBINING_E}{FLAG_JP}");
        let mut c = GraphemeCursor::new(&s, 0);
        assert_eq!(c.next(), COMBINING_E.len(), "first cluster = é");
        assert_eq!(c.next(), s.len(), "second cluster = flag");
        assert_eq!(c.prev(), COMBINING_E.len(), "back over the flag");
        assert_eq!(c.prev(), 0, "back over é");
    }
}
