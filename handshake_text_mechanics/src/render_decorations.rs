//! Editor-chrome decoration math for the code editor (WP-KERNEL-012 MT-054).
//!
//! Three render-ONLY features that sit on top of the MT-002 virtualized line renderer. Every function
//! here is PURE over a borrowed [`TextBuffer`] (the MT-001 rope) and does NO buffer mutation, no cursor
//! change, and no tree-sitter call (AC-007 — render/decoration only):
//!
//! 1. [`find_matching_bracket`] — matching-bracket highlight driven by VS Code's cursor-adjacency rule
//!    (a bracket is active when the cursor is immediately BEFORE an opening bracket OR immediately AFTER
//!    a closing bracket). Bounded to a fixed look-ahead/behind window (RISK-003 / MC-003) so an
//!    unmatched bracket in a multi-MB file never becomes an O(n)-per-frame scan.
//! 2. [`bracket_pair_colors`] — bracket-pair colorization. Each bracket is colored by its nesting depth
//!    modulo the palette length, matching VS Code's `bracketPairColorization`. A SHARED depth counter
//!    across bracket families drives the color index (VS Code increments depth on ANY open bracket),
//!    while [`find_matching_bracket`] matches partners only within the SAME family (note 67) — these two
//!    concerns are kept separate by design.
//! 3. [`indent_level_of`] / [`indent_guide_x`] — vertical indent-guide geometry. The indent level is the
//!    leading-whitespace columns divided by the tab width (tabs expand to `tab_width` columns); the x of
//!    the guide for level N is `gutter_right + N * tab_width * char_width_px`.
//!
//! ## Colors come from the theme layer (CONTROL-4 — no hardcoded hex)
//!
//! This module computes GEOMETRY and COLOR INDICES only. The actual `egui::Color32` values (the
//! bracket-pair palette and the two indent-guide tokens) live in `theme/palette.rs` and are passed in by
//! the panel paint path. There is deliberately NO opaque color literal in this file so the
//! `no_hardcoded_color32_outside_theme_module` guard stays GREEN (the guard scans every line, including
//! comments, for the opaque-hex constructor form).
//!
//! ## v1 limitation: string/comment brackets (RISK-004 / MC-004)
//!
//! Bracket matching in v1 is SYNTACTIC ONLY: a `(` inside a string or comment is matched as a real
//! bracket. String/comment awareness (skipping brackets inside literals) is explicitly OUT of scope for
//! v1 and documented here so the parity gap is bounded, not silent. The bounded scan window also caps
//! the blast radius of a mismatch.

use std::ops::Range;

use ecolor::Color32;

use super::buffer::TextBuffer;

/// The maximum number of bytes [`find_matching_bracket`] scans in each direction looking for a partner
/// before giving up (RISK-003 / MC-003). 64 KiB comfortably covers any on-screen bracket pair while
/// bounding the worst case (cursor next to an unmatched bracket in a huge file) to O(cap) per frame
/// rather than O(document).
pub const BRACKET_SCAN_CAP_BYTES: usize = 64 * 1024;

/// A matched bracket pair: the byte offset of the opening bracket and of its partner closing bracket.
/// Both are the byte offset of the bracket CHARACTER itself (each ASCII bracket is one byte), so the
/// renderer can highlight `open_byte..open_byte+1` and `close_byte..close_byte+1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BracketMatch {
    /// Byte offset of the opening bracket character.
    pub open_byte: usize,
    /// Byte offset of the closing bracket character.
    pub close_byte: usize,
}

/// One of the three bracket families. The OPEN and CLOSE of a family must match each other; a `(` never
/// pairs with a `]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BracketKind {
    Paren,
    Square,
    Curly,
}

impl BracketKind {
    /// Classify a byte as an opening bracket of some family, or `None`.
    fn opening(b: u8) -> Option<Self> {
        match b {
            b'(' => Some(BracketKind::Paren),
            b'[' => Some(BracketKind::Square),
            b'{' => Some(BracketKind::Curly),
            _ => None,
        }
    }

    /// Classify a byte as a closing bracket of some family, or `None`.
    fn closing(b: u8) -> Option<Self> {
        match b {
            b')' => Some(BracketKind::Paren),
            b']' => Some(BracketKind::Square),
            b'}' => Some(BracketKind::Curly),
            _ => None,
        }
    }
}

/// Find the bracket pair the cursor is adjacent to, applying VS Code's adjacency rule.
///
/// A bracket is "active" when:
///   - the cursor sits immediately BEFORE an opening bracket (`cursor_byte` points AT a `([{`), OR
///   - the cursor sits immediately AFTER a closing bracket (`cursor_byte - 1` points AT a `)]}`).
///
/// Both directions are checked; the before-open case takes precedence when the cursor is between a
/// closing bracket and an opening bracket (`)(` with the cursor in the middle), matching VS Code, which
/// highlights the bracket the cursor is about to type into.
///
/// The partner is found by walking the buffer with a depth counter over the SAME family. The walk is
/// BOUNDED to [`BRACKET_SCAN_CAP_BYTES`] in the search direction (RISK-003 / MC-003): if no partner is
/// found within the cap, `None` is returned rather than scanning the whole file. Unbalanced input (no
/// partner) likewise returns `None`.
///
/// Pure over a borrowed buffer; no mutation. The cursor's byte offset is clamped to the buffer length.
pub fn find_matching_bracket(buffer: &TextBuffer, cursor_byte: usize) -> Option<BracketMatch> {
    let len = buffer.len_bytes();
    let cursor = cursor_byte.min(len);

    // Materialize a bounded window around the cursor ONCE (O(cap), not O(document)). The partner search
    // stays inside this window, so a bracket whose partner is farther than the cap reads as unmatched —
    // the documented bound (RISK-003 / MC-003).
    let win_start = cursor.saturating_sub(BRACKET_SCAN_CAP_BYTES);
    let win_end = (cursor + BRACKET_SCAN_CAP_BYTES).min(len);
    let window = buffer.byte_slice_to_string(win_start..win_end);
    let win_bytes = window.as_bytes();

    // Translate an absolute buffer byte offset into an index within `window`, or `None` if outside it.
    let to_win = |abs: usize| -> Option<usize> {
        if abs >= win_start && abs < win_end {
            Some(abs - win_start)
        } else {
            None
        }
    };

    // 1) Cursor immediately BEFORE an opening bracket: the byte AT the cursor is `([{`.
    if let Some(ci) = to_win(cursor) {
        if let Some(kind) = BracketKind::opening(win_bytes[ci]) {
            if let Some(close_abs) = scan_forward_for_close(win_bytes, ci, kind, win_start) {
                return Some(BracketMatch {
                    open_byte: cursor,
                    close_byte: close_abs,
                });
            }
        }
    }

    // 2) Cursor immediately AFTER a closing bracket: the byte BEFORE the cursor is `)]}`.
    if cursor > 0 {
        let prev_abs = cursor - 1;
        if let Some(ci) = to_win(prev_abs) {
            if let Some(kind) = BracketKind::closing(win_bytes[ci]) {
                if let Some(open_abs) = scan_backward_for_open(win_bytes, ci, kind, win_start) {
                    return Some(BracketMatch {
                        open_byte: open_abs,
                        close_byte: prev_abs,
                    });
                }
            }
        }
    }

    None
}

/// Walk FORWARD from the opening bracket at window index `open_idx` (of family `kind`) maintaining a
/// per-family depth counter; return the absolute byte offset of the matching close, or `None` if the
/// window ends first (unmatched within the cap). Brackets of OTHER families are ignored for matching
/// (note 67: match partners within the same family only).
fn scan_forward_for_close(
    win_bytes: &[u8],
    open_idx: usize,
    kind: BracketKind,
    win_start: usize,
) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = open_idx;
    while i < win_bytes.len() {
        let b = win_bytes[i];
        if BracketKind::opening(b) == Some(kind) {
            depth += 1;
        } else if BracketKind::closing(b) == Some(kind) {
            depth -= 1;
            if depth == 0 {
                return Some(win_start + i);
            }
        }
        i += 1;
    }
    None
}

/// Walk BACKWARD from the closing bracket at window index `close_idx` (of family `kind`) maintaining a
/// per-family depth counter; return the absolute byte offset of the matching open, or `None` if the
/// window start is reached first (unmatched within the cap).
fn scan_backward_for_open(
    win_bytes: &[u8],
    close_idx: usize,
    kind: BracketKind,
    win_start: usize,
) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = close_idx as isize;
    while i >= 0 {
        let b = win_bytes[i as usize];
        if BracketKind::closing(b) == Some(kind) {
            depth += 1;
        } else if BracketKind::opening(b) == Some(kind) {
            depth -= 1;
            if depth == 0 {
                return Some(win_start + i as usize);
            }
        }
        i -= 1;
    }
    None
}

/// Assign a color to every bracket in `visible_byte_range` by its nesting depth (VS Code's
/// `bracketPairColorization`). The depth counter is SHARED across families (VS Code increments depth on
/// any open bracket); the color index is `depth % palette.len()`. Returns one
/// `(bracket_byte_range, Color32)` per bracket character, in source order, where `bracket_byte_range` is
/// the single-byte range of the bracket char (so the painter can re-draw that glyph in the depth color).
///
/// The depth is seeded by scanning from the START of the visible range's line back is NOT done here for
/// v1 simplicity: depth starts at 0 at the window start, so colorization is window-relative. This is the
/// documented v1 behavior — the visible window is what the operator sees, and a window-relative depth
/// keeps the pass O(visible) without a whole-file pre-scan (the same bounded-cost discipline as
/// [`find_matching_bracket`]). An EMPTY palette yields no colors (a defensive guard against a misbuilt
/// theme — never a divide-by-zero).
///
/// Pure over a borrowed buffer; no mutation.
pub fn bracket_pair_colors(
    buffer: &TextBuffer,
    visible_byte_range: Range<usize>,
    palette: &[Color32],
) -> Vec<(Range<usize>, Color32)> {
    if palette.is_empty() {
        return Vec::new();
    }
    let len = buffer.len_bytes();
    let start = visible_byte_range.start.min(len);
    let end = visible_byte_range.end.min(len).max(start);
    let text = buffer.byte_slice_to_string(start..end);
    let bytes = text.as_bytes();

    let mut out: Vec<(Range<usize>, Color32)> = Vec::new();
    // A single shared depth counter across families. An open bracket is colored at the CURRENT depth and
    // then increments depth; a close bracket decrements first and is colored at the now-CURRENT (outer)
    // depth, so an open and its matching close share the same color (VS Code parity).
    let mut depth: usize = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if BracketKind::opening(b).is_some() {
            let color = palette[depth % palette.len()];
            let abs = start + i;
            out.push((abs..abs + 1, color));
            depth += 1;
        } else if BracketKind::closing(b).is_some() {
            depth = depth.saturating_sub(1);
            let color = palette[depth % palette.len()];
            let abs = start + i;
            out.push((abs..abs + 1, color));
        }
    }
    out
}

/// Like [`bracket_pair_colors`] but over a list of sorted, non-overlapping byte SEGMENTS — the visible
/// rows' byte ranges of the painted window (MT-054/MT-005 fold-aware decorations, Wave-B fix). Bytes
/// BETWEEN the segments (fold-hidden lines) are never scanned, so:
///
///   1. no hidden bracket is ever assigned a color (nothing to ghost-paint onto a visible row), and
///   2. the per-frame cost is O(visible bytes) even when a huge folded region sits inside the window.
///
/// The shared depth counter carries ACROSS the segments — the colorization treats the hidden text as
/// absent, the same window-relative depth semantics [`bracket_pair_colors`] documents (depth 0 at the
/// window start). Passing one segment covering the whole window is exactly [`bracket_pair_colors`].
///
/// Pure over a borrowed buffer; no mutation.
pub fn bracket_pair_colors_in_segments(
    buffer: &TextBuffer,
    segments: &[Range<usize>],
    palette: &[Color32],
) -> Vec<(Range<usize>, Color32)> {
    if palette.is_empty() {
        return Vec::new();
    }
    let len = buffer.len_bytes();
    let mut out: Vec<(Range<usize>, Color32)> = Vec::new();
    // The SAME shared-depth coloring walk as `bracket_pair_colors`, continued across segments.
    let mut depth: usize = 0;
    for seg in segments {
        let start = seg.start.min(len);
        let end = seg.end.min(len).max(start);
        let text = buffer.byte_slice_to_string(start..end);
        for (i, &b) in text.as_bytes().iter().enumerate() {
            if BracketKind::opening(b).is_some() {
                let color = palette[depth % palette.len()];
                let abs = start + i;
                out.push((abs..abs + 1, color));
                depth += 1;
            } else if BracketKind::closing(b).is_some() {
                depth = depth.saturating_sub(1);
                let color = palette[depth % palette.len()];
                let abs = start + i;
                out.push((abs..abs + 1, color));
            }
        }
    }
    out
}

/// The depth-to-color INDEX for a bracket at `depth` given a palette of `palette_len` colors
/// (`depth % palette_len`). Exposed for unit tests that assert the index assignment without building an
/// egui palette. Returns 0 for an empty palette (defensive — never a modulo-by-zero).
pub fn depth_color_index(depth: usize, palette_len: usize) -> usize {
    if palette_len == 0 {
        0
    } else {
        depth % palette_len
    }
}

/// The indent LEVEL of a logical line: the count of leading-whitespace COLUMNS divided by `tab_width`,
/// where a leading tab expands to the next multiple of `tab_width` columns and a leading space is one
/// column. A line that is entirely whitespace (or empty) reports the level its leading whitespace
/// implies (so a blank line between two indented lines still draws its guides — VS Code behavior). Pure
/// over a borrowed buffer.
pub fn indent_level_of(buffer: &TextBuffer, line: usize, tab_width: usize) -> usize {
    let tab_width = tab_width.max(1);
    let line_start = match buffer.line_to_byte(line) {
        Some(b) => b,
        None => return 0,
    };
    let line_end = buffer
        .line_to_byte(line + 1)
        .unwrap_or_else(|| buffer.len_bytes());
    let text = buffer.byte_slice_to_string(line_start..line_end);

    let mut columns = 0usize;
    for ch in text.chars() {
        match ch {
            ' ' => columns += 1,
            '\t' => {
                // Advance to the next tab stop (a tab fills the remainder of the current tab cell).
                columns += tab_width - (columns % tab_width);
            }
            // First non-whitespace char (or newline on a blank line) ends the leading run.
            _ => break,
        }
    }
    columns / tab_width
}

/// The screen x of the vertical indent guide for indent level `level` (1-based: level 1 is the first
/// guide, drawn after the first indent unit). `gutter_right` is the screen x of the gutter's right edge
/// (where the text rows begin); each level is `tab_width * char_width_px` to the right of the previous.
/// The guide for level N sits at the LEFT edge of the Nth indent cell, i.e.
/// `gutter_right + (level - 1) * tab_width * char_width_px`.
pub fn indent_guide_x(
    gutter_right: f32,
    level: usize,
    tab_width: usize,
    char_width_px: f32,
) -> f32 {
    let tab_width = tab_width.max(1) as f32;
    let cell = tab_width * char_width_px;
    gutter_right + (level.saturating_sub(1) as f32) * cell
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_cursor_before_opening_bracket() {
        // "foo(bar)": f=0 o=1 o=2 (=3 b=4 a=5 r=6 )=7. Cursor at byte 3 sits immediately BEFORE the '('.
        let buf = TextBuffer::new("foo(bar)");
        let m = find_matching_bracket(&buf, 3).expect("before-open adjacency matches");
        assert_eq!(m.open_byte, 3, "open is the '(' at byte 3");
        assert_eq!(m.close_byte, 7, "close is the ')' at byte 7");
    }

    #[test]
    fn matches_cursor_after_closing_bracket() {
        // "foo(bar)" — cursor at byte 8 sits immediately AFTER the ')' at byte 7 (VS Code adjacency).
        let buf = TextBuffer::new("foo(bar)");
        let m = find_matching_bracket(&buf, 8).expect("after-close adjacency matches");
        assert_eq!(m.open_byte, 3, "open is the '(' at byte 3");
        assert_eq!(m.close_byte, 7, "close is the ')' at byte 7");
    }

    #[test]
    fn matches_nested_pairs_to_correct_partner() {
        // "a(b[c]d)e": cursor before the outer '(' at byte 1 matches the outer ')' at byte 7,
        // NOT the inner ']'.
        let buf = TextBuffer::new("a(b[c]d)e");
        let m = find_matching_bracket(&buf, 1).expect("outer paren matches");
        assert_eq!(m.open_byte, 1);
        assert_eq!(m.close_byte, 7, "outer ')' partner, skipping the nested []");

        // Cursor before the inner '[' at byte 3 matches the inner ']' at byte 5.
        let inner = find_matching_bracket(&buf, 3).expect("inner square matches");
        assert_eq!(inner.open_byte, 3);
        assert_eq!(inner.close_byte, 5);
    }

    #[test]
    fn same_family_only_no_cross_family_match() {
        // "([)]" is unbalanced per-family. Cursor before '(' (byte 0): its same-family partner ')' is at
        // byte 2, with the '[' in between ignored (different family). depth: '(' +1, '[' ignored, ')' -1
        // -> match at byte 2.
        let buf = TextBuffer::new("([)]");
        let m = find_matching_bracket(&buf, 0).expect("paren matches its same-family close");
        assert_eq!(m.open_byte, 0);
        assert_eq!(
            m.close_byte, 2,
            "the '(' pairs with the ')' (same family), not the ']'"
        );
    }

    #[test]
    fn unbalanced_input_returns_none() {
        // No closing partner.
        let buf = TextBuffer::new("foo(bar");
        assert_eq!(
            find_matching_bracket(&buf, 3),
            None,
            "unmatched '(' -> None"
        );
        // Cursor not adjacent to any bracket.
        let plain = TextBuffer::new("hello world");
        assert_eq!(
            find_matching_bracket(&plain, 3),
            None,
            "no adjacent bracket -> None"
        );
        // Lone closing bracket, cursor after it.
        let lone = TextBuffer::new("abc)");
        assert_eq!(
            find_matching_bracket(&lone, 4),
            None,
            "unmatched ')' -> None"
        );
    }

    #[test]
    fn before_open_takes_precedence_over_after_close() {
        // ")(": cursor at byte 1 is AFTER the ')' (byte 0, unmatched) AND BEFORE the '(' (byte 1,
        // unmatched). Neither matches here, so None; but the precedence is exercised by a balanced case:
        // "()()" cursor at byte 2 is after the first ')' (matches 0..1) and before the second '(' (matches
        // 2..3). Before-open wins -> the SECOND pair.
        let buf = TextBuffer::new("()()");
        let m = find_matching_bracket(&buf, 2).expect("adjacency at byte 2 matches");
        assert_eq!(
            m.open_byte, 2,
            "before-open precedence picks the '(' at byte 2"
        );
        assert_eq!(m.close_byte, 3);
    }

    #[test]
    fn scan_is_bounded_no_partner_past_cap() {
        // A '(' followed by more than the cap of non-bracket bytes and then a ')': the partner is past
        // the cap, so it reads as unmatched (RISK-003 / MC-003 — the documented bound).
        let mut s = String::from("(");
        s.push_str(&"x".repeat(BRACKET_SCAN_CAP_BYTES + 100));
        s.push(')');
        let buf = TextBuffer::new(&s);
        // Cursor before the '(' at byte 0: the ')' is > cap away -> None (bounded).
        assert_eq!(
            find_matching_bracket(&buf, 0),
            None,
            "partner past the cap reads as unmatched"
        );
    }

    #[test]
    fn bracket_pair_colors_assign_depth_modulo_palette() {
        // Three palette colors; a 3-deep nest assigns indices 0,1,2 then the closes mirror them.
        let palette = [
            Color32::from_rgba_unmultiplied(10, 0, 0, 255),
            Color32::from_rgba_unmultiplied(0, 20, 0, 255),
            Color32::from_rgba_unmultiplied(0, 0, 30, 255),
        ];
        // "([{x}])": ( depth0, [ depth1, { depth2, } depth2, ] depth1, ) depth0.
        let buf = TextBuffer::new("([{x}])");
        let colors = bracket_pair_colors(&buf, 0..buf.len_bytes(), &palette);
        assert_eq!(colors.len(), 6, "six bracket chars colored");
        // Opens at depths 0,1,2.
        assert_eq!(colors[0].1, palette[0], "'(' at depth 0");
        assert_eq!(colors[1].1, palette[1], "'[' at depth 1");
        assert_eq!(colors[2].1, palette[2], "'{{' at depth 2");
        // Closes mirror their opens (same depth -> same color).
        assert_eq!(colors[3].1, palette[2], "'}}' matches '{{' color (depth 2)");
        assert_eq!(colors[4].1, palette[1], "']' matches '[' color (depth 1)");
        assert_eq!(colors[5].1, palette[0], "')' matches '(' color (depth 0)");
        // Byte ranges are single-byte and point at the bracket chars.
        assert_eq!(colors[0].0, 0..1);
        assert_eq!(colors[5].0, 6..7);
    }

    #[test]
    fn bracket_pair_colors_wrap_around_palette_for_deep_nesting() {
        // Two-color palette + 3 nesting levels -> indices 0,1,0 (depth % len). Proves >=3 distinct
        // LEVELS map through the modulo (AC-002: distinct colors for at least three nesting levels when
        // the palette has >=3 entries; here the wrap-around is the explicit modulo proof).
        assert_eq!(depth_color_index(0, 2), 0);
        assert_eq!(depth_color_index(1, 2), 1);
        assert_eq!(depth_color_index(2, 2), 0);
        // With a >=3 palette, three levels are three distinct indices.
        assert_eq!(depth_color_index(0, 6), 0);
        assert_eq!(depth_color_index(1, 6), 1);
        assert_eq!(depth_color_index(2, 6), 2);
    }

    #[test]
    fn empty_palette_is_safe() {
        let buf = TextBuffer::new("()");
        assert!(
            bracket_pair_colors(&buf, 0..2, &[]).is_empty(),
            "empty palette -> no colors"
        );
        assert_eq!(depth_color_index(5, 0), 0, "modulo-by-zero guarded");
    }

    #[test]
    fn indent_level_counts_spaces_and_tabs() {
        // tab_width 4. "    x" = 4 spaces = level 1. "        y" = 8 spaces = level 2.
        let buf = TextBuffer::new("    x\n        y\nz");
        assert_eq!(
            indent_level_of(&buf, 0, 4),
            1,
            "4 leading spaces -> level 1"
        );
        assert_eq!(
            indent_level_of(&buf, 1, 4),
            2,
            "8 leading spaces -> level 2"
        );
        assert_eq!(indent_level_of(&buf, 2, 4), 0, "no indent -> level 0");
    }

    #[test]
    fn indent_level_expands_tabs_to_tab_width() {
        // A leading tab expands to the next tab stop. tab_width 4: "\tx" -> 4 columns -> level 1.
        let buf = TextBuffer::new("\tx\n\t\ty");
        assert_eq!(indent_level_of(&buf, 0, 4), 1, "one tab -> level 1");
        assert_eq!(indent_level_of(&buf, 1, 4), 2, "two tabs -> level 2");
        // Mixed: two spaces then a tab. cols: 2 spaces -> 2, tab fills to 4 -> level 1.
        let mixed = TextBuffer::new("  \tx");
        assert_eq!(
            indent_level_of(&mixed, 0, 4),
            1,
            "2 spaces + tab -> 4 cols -> level 1"
        );
    }

    #[test]
    fn indent_guide_x_steps_by_tab_width_times_glyph() {
        // gutter_right=50, tab_width=4, char_width=8 -> cell = 32px. Level 1 at 50, level 2 at 82.
        assert_eq!(
            indent_guide_x(50.0, 1, 4, 8.0),
            50.0,
            "level 1 at the gutter edge"
        );
        assert_eq!(
            indent_guide_x(50.0, 2, 4, 8.0),
            82.0,
            "level 2 one cell (32px) right"
        );
        assert_eq!(
            indent_guide_x(50.0, 3, 4, 8.0),
            114.0,
            "level 3 two cells right"
        );
    }
}
