//! Visible-row layout math for the code editor's word-wrap mode (WP-KERNEL-012 MT-054).
//!
//! This module is the SINGLE source of the wrap-aware visible-row list. It is a PURE calculator over a
//! borrowed [`TextBuffer`] (the MT-001 rope) — it performs NO buffer mutation, holds no egui/GPU state,
//! and never recomputes font metrics (`char_width_px` is handed in by the MT-002 renderer so the wrap
//! break points stay aligned with the painted glyphs — RISK-002 / MC-002).
//!
//! ## Why a separate visual-row list (RISK-001 / MC-001 — the scroll-math single source of truth)
//!
//! With wrap OFF, one logical buffer line is one on-screen row. With wrap ON, a logical line wider than
//! the wrap width occupies MULTIPLE visual rows. The MT-002 virtualization drives
//! `egui::ScrollArea::show_rows` over a ROW COUNT and strides the viewport by `line_height`. If paint
//! counted visual rows while the scrollbar still counted logical lines, scrolling a wrapped document
//! would jump or clip. [`layout_visual_rows`] is therefore the ONE place both the scrollbar extent /
//! first-visible-row / total-row-count AND the per-row paint derive their row geometry from.
//!
//! ## The strict 1:1 fast path (RISK-006 / MC-006 — the MT-002 regression guard)
//!
//! When [`WrapConfig::enabled`] is `false`, [`layout_visual_rows`] is a strict identity: each logical
//! line maps to EXACTLY ONE [`VisualRow`] (`wrap_index == 0`, byte range = the whole logical line),
//! computed by a fast path that skips the wrap arithmetic entirely. This keeps the non-wrap render
//! byte-identical to the MT-002 baseline (the regression screenshot + the existing virtual_lines /
//! panel tests stay GREEN).
//!
//! ## Soft vs hard breaks
//!
//! With wrap ON, a logical line wider than the wrap width is split at the LAST soft-break opportunity
//! (an ASCII whitespace boundary) before the width limit, matching Monaco's `wordWrap: "on"`. When a
//! single run has no whitespace before the limit (e.g. one very long token / a base64 blob), it falls
//! back to a HARD character break at the width limit so a pathological line still wraps instead of
//! overflowing. Every produced fragment's byte range is contiguous and non-overlapping and the union
//! covers the whole logical line (AC-003) — the trailing `\n` (when present) is kept on the final
//! fragment so the row geometry matches the logical line.

use std::ops::Range;

use super::buffer::TextBuffer;

/// How the editor wraps logical lines into visual rows. Mirrors the Monaco editor option model
/// (`wordWrap: off | on | wordWrapColumn`) ported from `app/src/lib/monaco/*`:
///   - `enabled == false`            -> `wordWrap: "off"`  (strict 1:1, the MT-002 baseline)
///   - `enabled, wrap_column == None` -> `wordWrap: "on"`   (wrap at the viewport edge)
///   - `enabled, wrap_column == Some` -> `wordWrap: "wordWrapColumn"` (wrap at a fixed column)
///
/// `viewport_width_px` is the available text-row width in pixels (gutter already excluded by the
/// caller). The effective wrap width in COLUMNS is `wrap_column` when set, else
/// `floor(viewport_width_px / char_width_px)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WrapConfig {
    /// Master toggle (Alt+Z). When `false` the layout is the strict 1:1 MT-002 identity (fast path).
    pub enabled: bool,
    /// A fixed wrap column (`wordWrapColumn`), or `None` to wrap at the viewport edge (`wordWrap: on`).
    pub wrap_column: Option<usize>,
    /// The available text-row width in pixels, used to derive the column limit when `wrap_column` is
    /// `None`. Ignored on the 1:1 fast path.
    pub viewport_width_px: f32,
}

impl Default for WrapConfig {
    /// Wrap OFF by default (the MT-002 baseline render). The viewport width is filled in by the panel
    /// each frame from the live editor-area width; 0.0 here is a safe placeholder (the 1:1 fast path
    /// never divides by it).
    fn default() -> Self {
        Self {
            enabled: false,
            wrap_column: None,
            viewport_width_px: 0.0,
        }
    }
}

impl WrapConfig {
    /// The effective wrap width in COLUMNS for the given monospace glyph width, or `None` when wrapping
    /// is disabled or the width is degenerate (non-finite / <= 0 viewport with no explicit column). A
    /// `Some(0)` is impossible: the floor is clamped to >= 1 so a fragment always advances (no infinite
    /// split — RISK guard).
    pub fn wrap_columns(&self, char_width_px: f32) -> Option<usize> {
        if !self.enabled {
            return None;
        }
        if let Some(col) = self.wrap_column {
            return Some(col.max(1));
        }
        if !(char_width_px.is_finite() && char_width_px > 0.0) {
            return None;
        }
        if !(self.viewport_width_px.is_finite() && self.viewport_width_px > 0.0) {
            return None;
        }
        let cols = (self.viewport_width_px / char_width_px).floor() as usize;
        Some(cols.max(1))
    }
}

/// One visual row: a contiguous BYTE slice of a single logical line, plus which logical line it belongs
/// to and its 0-based wrap fragment index within that line. `wrap_index == 0` is always the FIRST
/// fragment of a logical line (the one that carries the indent guides — RISK-007 / MC-007); continuation
/// rows have `wrap_index > 0`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisualRow {
    /// The logical buffer line this row is a fragment of.
    pub logical_line: usize,
    /// Inclusive byte offset of the fragment start (absolute buffer byte offset).
    pub byte_start: usize,
    /// Exclusive byte offset of the fragment end (absolute buffer byte offset). The final fragment of a
    /// line includes that line's trailing `\n` (when present) so the row geometry matches the logical
    /// line's full byte span.
    pub byte_end: usize,
    /// 0-based fragment index within the logical line. `0` is the first (indent-guide-bearing) row.
    pub wrap_index: usize,
}

impl VisualRow {
    /// True when this is the FIRST visual row of its logical line (the row that carries indent guides).
    pub fn is_first_fragment(&self) -> bool {
        self.wrap_index == 0
    }

    /// The fragment's byte range as `start..end`.
    pub fn byte_range(&self) -> Range<usize> {
        self.byte_start..self.byte_end
    }
}

/// Lay out the logical lines in `line_range` into [`VisualRow`]s.
///
/// - When `cfg.enabled` is `false`, this is the STRICT 1:1 fast path: one VisualRow per logical line,
///   `wrap_index == 0`, byte range = the line's full byte span (RISK-006 / MC-006 — MT-002 baseline
///   unchanged).
/// - When `cfg.enabled` is `true`, a logical line wider than the effective wrap width
///   ([`WrapConfig::wrap_columns`]) is split into N>1 fragments at the last whitespace before the width
///   limit (soft break), falling back to a hard character break when no whitespace exists. The fragments
///   of one line are contiguous and non-overlapping and their union covers the whole logical line
///   (AC-003). Column width is measured in CHARS (one char = one column), the same monospace unit the
///   renderer paints with (`char_width_px` is the renderer's measured glyph advance — RISK-002 / MC-002).
///
/// `line_range` is clamped to the buffer's line count, so an out-of-range range yields a best-effort
/// (possibly empty) list rather than panicking.
pub fn layout_visual_rows(
    buffer: &TextBuffer,
    line_range: Range<usize>,
    cfg: &WrapConfig,
    char_width_px: f32,
) -> Vec<VisualRow> {
    let total_lines = buffer.len_lines();
    let start = line_range.start.min(total_lines);
    let end = line_range.end.min(total_lines).max(start);

    let wrap_columns = cfg.wrap_columns(char_width_px);
    let mut rows: Vec<VisualRow> = Vec::with_capacity(end - start);

    for line in start..end {
        let line_start = match buffer.line_to_byte(line) {
            Some(b) => b,
            None => continue,
        };
        // The line's exclusive byte end is the start of the next line (or the buffer end on the last
        // line). This span INCLUDES the trailing '\n' when present, which is intentional: the final
        // fragment carries it so the row covers the whole logical line (AC-003 coverage).
        let line_end = buffer
            .line_to_byte(line + 1)
            .unwrap_or_else(|| buffer.len_bytes());

        match wrap_columns {
            // 1:1 fast path (wrap off / degenerate width): the whole logical line is one row.
            None => rows.push(VisualRow {
                logical_line: line,
                byte_start: line_start,
                byte_end: line_end,
                wrap_index: 0,
            }),
            Some(cols) => {
                wrap_one_line(buffer, line, line_start, line_end, cols, &mut rows);
            }
        }
    }

    rows
}

/// MT-054 PERF CAP: the number of visual rows a SINGLE logical line `line` produces under `cfg`, without
/// allocating the [`VisualRow`] list. Used by the panel's cached wrap-row-count index
/// ([`crate::code_editor::panel`]) to size `show_rows` + map a visual-row index back to a logical line
/// WITHOUT materializing the whole document's VisualRow list every frame. The result equals
/// `layout_visual_rows(buffer, line..line + 1, cfg, char_width_px).len()` (a debug assertion in the unit
/// tests enforces this identity), but a long line costs one byte-slice + one char-index pass instead of a
/// `Vec<VisualRow>` allocation. Always returns at least 1 (an empty / short line is one row), and 1 on
/// the 1:1 fast path (wrap off / degenerate width).
pub fn count_visual_rows_for_line(
    buffer: &TextBuffer,
    line: usize,
    cfg: &WrapConfig,
    char_width_px: f32,
) -> usize {
    let Some(cols) = cfg.wrap_columns(char_width_px) else {
        // 1:1 fast path (wrap off / degenerate width): one row per logical line.
        return 1;
    };
    let line_start = match buffer.line_to_byte(line) {
        Some(b) => b,
        None => return 1,
    };
    let line_end = buffer
        .line_to_byte(line + 1)
        .unwrap_or_else(|| buffer.len_bytes());
    let text = buffer.byte_slice_to_string(line_start..line_end);
    let content = text.strip_suffix('\n').unwrap_or(&text);
    let n = content.chars().count();
    if n == 0 {
        return 1;
    }
    // Mirror `wrap_one_line`'s fragment count: at each step we advance by at least 1 char and at most
    // `cols`, soft-breaking at the last whitespace at-or-before the hard limit (which never shortens the
    // step below 1). The fragment count therefore depends on the soft-break positions, so we walk the
    // same break logic but count only — this stays O(line), the same per-line cost the layout pays.
    let chars: Vec<char> = content.chars().collect();
    let mut count = 0usize;
    let mut frag_start = 0usize;
    while frag_start < n {
        count += 1;
        let remaining = n - frag_start;
        if remaining <= cols {
            break;
        }
        let hard_limit = frag_start + cols;
        let mut break_char = None;
        let mut i = hard_limit;
        while i > frag_start + 1 {
            i -= 1;
            if chars[i].is_whitespace() {
                break_char = Some(i + 1);
                break;
            }
        }
        frag_start = match break_char {
            Some(bc) if bc > frag_start => bc,
            _ => hard_limit,
        };
    }
    count
}

/// Split one logical line `[line_start, line_end)` (line_end includes the trailing `\n` when present)
/// into wrap fragments of at most `cols` columns each, appending them to `rows`. Soft-breaks at the last
/// whitespace before the limit; hard-breaks at the limit when a run has no whitespace. Always emits at
/// least one fragment (an empty / short line is a single `wrap_index == 0` row), so the visible-row list
/// never loses a logical line.
fn wrap_one_line(
    buffer: &TextBuffer,
    line: usize,
    line_start: usize,
    line_end: usize,
    cols: usize,
    rows: &mut Vec<VisualRow>,
) {
    // Materialize ONLY this line's bytes (O(line), not O(document)). The slice keeps the trailing '\n'
    // when present; we treat it as a non-breaking trailing char that rides on the final fragment.
    let text = buffer.byte_slice_to_string(line_start..line_end);
    // Strip a single trailing '\n' for wrap measurement (the newline does not occupy a visual column);
    // it is re-attached to the final fragment's byte_end below by using `line_end` for the last row.
    let has_newline = text.ends_with('\n');
    let content = if has_newline {
        &text[..text.len() - 1]
    } else {
        text.as_str()
    };

    // A char-index list with byte offsets so a soft/hard break lands on a char boundary (RISK-002).
    // (col, byte_offset_within_content, is_whitespace) per char.
    let chars: Vec<(usize, char)> = content.char_indices().collect();
    if chars.is_empty() {
        // Empty line (or a bare newline): one fragment covering the whole logical span.
        rows.push(VisualRow {
            logical_line: line,
            byte_start: line_start,
            byte_end: line_end,
            wrap_index: 0,
        });
        return;
    }

    let mut wrap_index = 0usize;
    // `frag_start_char` is the char index (into `chars`) where the current fragment begins.
    let mut frag_start_char = 0usize;
    let n = chars.len();

    while frag_start_char < n {
        let remaining = n - frag_start_char;
        if remaining <= cols {
            // The rest of the line fits in one fragment. Its byte end is the LINE end (so the trailing
            // newline, if any, rides on this final fragment).
            let byte_start = line_start + chars[frag_start_char].0;
            rows.push(VisualRow {
                logical_line: line,
                byte_start,
                byte_end: line_end,
                wrap_index,
            });
            break;
        }

        // The hard limit char index (exclusive) for this fragment: at most `cols` chars from the start.
        let hard_limit_char = frag_start_char + cols;
        // Find the last whitespace AT OR BEFORE the hard limit (a soft break). We break AFTER the
        // whitespace so the space stays on the current row (Monaco keeps trailing spaces on the row).
        let mut break_char = None;
        let mut i = hard_limit_char; // exclusive; scan [frag_start_char+1 ..= hard_limit_char]
        while i > frag_start_char + 1 {
            i -= 1;
            if chars[i].1.is_whitespace() {
                // Break AFTER this whitespace char.
                break_char = Some(i + 1);
                break;
            }
        }
        let next_frag_start = match break_char {
            // Soft break found within the window AND it actually advances past the start.
            Some(bc) if bc > frag_start_char => bc,
            // No whitespace in the window (one long token): HARD break at the column limit.
            _ => hard_limit_char,
        };

        let byte_start = line_start + chars[frag_start_char].0;
        let byte_end = line_start + chars[next_frag_start].0;
        rows.push(VisualRow {
            logical_line: line,
            byte_start,
            byte_end,
            wrap_index,
        });
        wrap_index += 1;
        frag_start_char = next_frag_start;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn off() -> WrapConfig {
        WrapConfig::default()
    }

    fn on_at_cols(cols: usize) -> WrapConfig {
        WrapConfig {
            enabled: true,
            wrap_column: Some(cols),
            viewport_width_px: 0.0,
        }
    }

    #[test]
    fn wrap_disabled_is_strict_one_to_one() {
        let buf = TextBuffer::new("line0\nline1\nline2");
        let rows = layout_visual_rows(&buf, 0..buf.len_lines(), &off(), 8.0);
        assert_eq!(
            rows.len(),
            3,
            "3 logical lines -> 3 visual rows when wrap is off"
        );
        for (i, r) in rows.iter().enumerate() {
            assert_eq!(r.logical_line, i);
            assert_eq!(r.wrap_index, 0, "every row is the first fragment under 1:1");
        }
        // Row byte ranges align with the buffer line byte spans.
        assert_eq!(rows[0].byte_start, 0);
        assert_eq!(rows[0].byte_end, 6); // "line0\n"
        assert_eq!(rows[1].byte_start, 6);
        assert_eq!(rows[1].byte_end, 12); // "line1\n"
        assert_eq!(rows[2].byte_start, 12);
        assert_eq!(rows[2].byte_end, buf.len_bytes());
    }

    #[test]
    fn wrap_disabled_fast_path_ignores_width() {
        // Even an absurdly small viewport never wraps when disabled (fast path).
        let buf = TextBuffer::new("a very long single logical line that would wrap if enabled");
        let cfg = WrapConfig {
            enabled: false,
            wrap_column: None,
            viewport_width_px: 1.0,
        };
        let rows = layout_visual_rows(&buf, 0..1, &cfg, 8.0);
        assert_eq!(
            rows.len(),
            1,
            "disabled => exactly one row regardless of width"
        );
        assert_eq!(rows[0].byte_range(), 0..buf.len_bytes());
    }

    #[test]
    fn wrap_enabled_splits_long_line_into_contiguous_rows() {
        // 200 'a' chars, no whitespace -> hard breaks every `cols` chars.
        let line = "a".repeat(200);
        let buf = TextBuffer::new(&line);
        let cfg = on_at_cols(80);
        let rows = layout_visual_rows(&buf, 0..1, &cfg, 8.0);
        // ceil(200/80) = 3 rows.
        assert_eq!(
            rows.len(),
            3,
            "200 chars at width 80 -> 3 rows; got {}",
            rows.len()
        );

        // Contiguous, non-overlapping, covering the whole logical line (AC-003).
        assert_eq!(rows[0].byte_start, 0);
        for w in rows.windows(2) {
            assert_eq!(
                w[0].byte_end, w[1].byte_start,
                "fragments are contiguous (no gap/overlap)"
            );
        }
        assert_eq!(
            rows.last().unwrap().byte_end,
            buf.len_bytes(),
            "union covers the whole line"
        );

        // wrap_index increments 0,1,2; all share the one logical line.
        for (i, r) in rows.iter().enumerate() {
            assert_eq!(r.logical_line, 0);
            assert_eq!(r.wrap_index, i, "fragment indices are 0..N");
        }
        // Only the first row carries indent guides (RISK-007 / MC-007).
        assert!(rows[0].is_first_fragment());
        assert!(!rows[1].is_first_fragment());
        assert!(!rows[2].is_first_fragment());
    }

    #[test]
    fn wrap_enabled_soft_breaks_at_whitespace() {
        // "aaaa bbbb cccc" at cols 6: soft-break after the space following "aaaa".
        let buf = TextBuffer::new("aaaa bbbb cccc");
        let cfg = on_at_cols(6);
        let rows = layout_visual_rows(&buf, 0..1, &cfg, 8.0);
        assert!(
            rows.len() >= 2,
            "a 14-char line at width 6 wraps; got {}",
            rows.len()
        );
        // The first fragment ends at a whitespace boundary (after "aaaa "), i.e. byte 5.
        let first = buf.byte_slice_to_string(rows[0].byte_range());
        assert_eq!(
            first, "aaaa ",
            "soft break keeps the trailing space on the row; got {first:?}"
        );
        // Contiguous + full coverage.
        for w in rows.windows(2) {
            assert_eq!(w[0].byte_end, w[1].byte_start);
        }
        assert_eq!(rows.last().unwrap().byte_end, buf.len_bytes());
    }

    #[test]
    fn wrap_enabled_short_line_is_single_row() {
        let buf = TextBuffer::new("short\nx");
        let cfg = on_at_cols(80);
        let rows = layout_visual_rows(&buf, 0..2, &cfg, 8.0);
        assert_eq!(
            rows.len(),
            2,
            "two short logical lines -> two rows when each fits"
        );
        assert_eq!(rows[0].wrap_index, 0);
        assert_eq!(rows[1].wrap_index, 0);
        assert_eq!(rows[0].logical_line, 0);
        assert_eq!(rows[1].logical_line, 1);
    }

    #[test]
    fn wrap_columns_derives_from_viewport_when_no_explicit_column() {
        let cfg = WrapConfig {
            enabled: true,
            wrap_column: None,
            viewport_width_px: 800.0,
        };
        // 800 / 8 = 100 columns.
        assert_eq!(cfg.wrap_columns(8.0), Some(100));
        // Disabled -> None regardless.
        let off = WrapConfig {
            enabled: false,
            ..cfg
        };
        assert_eq!(off.wrap_columns(8.0), None);
        // Degenerate width -> None (no divide-by-zero / infinite split).
        let bad = WrapConfig {
            enabled: true,
            wrap_column: None,
            viewport_width_px: 0.0,
        };
        assert_eq!(bad.wrap_columns(8.0), None);
        let bad2 = WrapConfig {
            enabled: true,
            wrap_column: None,
            viewport_width_px: 800.0,
        };
        assert_eq!(bad2.wrap_columns(0.0), None);
    }

    #[test]
    fn empty_and_blank_lines_survive_wrap() {
        let buf = TextBuffer::new("\n\nx");
        let cfg = on_at_cols(4);
        let rows = layout_visual_rows(&buf, 0..buf.len_lines(), &cfg, 8.0);
        // 3 logical lines (two empty + "x"), each one visual row.
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].logical_line, 0);
        assert_eq!(rows[1].logical_line, 1);
        assert_eq!(rows[2].logical_line, 2);
    }

    #[test]
    fn out_of_range_line_range_is_clamped_not_panicked() {
        let buf = TextBuffer::new("a\nb");
        let rows = layout_visual_rows(&buf, 5..99, &off(), 8.0);
        assert!(rows.is_empty(), "a range past the end clamps to empty");
        // Inverted range -> empty, no panic.
        #[allow(clippy::reversed_empty_ranges)]
        let inverted = layout_visual_rows(&buf, 2..0, &off(), 8.0);
        assert!(inverted.is_empty());
    }

    #[test]
    fn total_row_count_grows_under_wrap_for_scroll_math() {
        // AC-004 backing: the scroll math counts VISUAL rows. A doc with one 200-char line + two short
        // lines yields more visual rows when wrap is on than when off.
        let buf = TextBuffer::new(&format!("{}\nshort\nx", "a".repeat(200)));
        let off_rows = layout_visual_rows(&buf, 0..buf.len_lines(), &off(), 8.0);
        let on_rows = layout_visual_rows(&buf, 0..buf.len_lines(), &on_at_cols(80), 8.0);
        assert_eq!(off_rows.len(), 3, "wrap off -> 3 logical rows");
        assert!(
            on_rows.len() > off_rows.len(),
            "wrap on -> more visual rows (got {} vs {})",
            on_rows.len(),
            off_rows.len()
        );
        // The long line contributes ceil(200/80)=3 rows; the two short lines 1 each => 5 total.
        assert_eq!(on_rows.len(), 5, "3 + 1 + 1 = 5 visual rows under wrap");
    }

    #[test]
    fn count_visual_rows_matches_layout_len_for_every_line() {
        // PERF CAP identity: the cheap count-only helper used by the panel's cached wrap-row index must
        // equal the full layout's fragment count for each logical line, on/off and across soft + hard
        // breaks + empty lines, so the cached scroll-row count never disagrees with the painted rows.
        let buf = TextBuffer::new(&format!(
            "{}\n\naaaa bbbb cccc dddd\nshort\n{}",
            "x".repeat(200),
            "tok".repeat(50)
        ));
        for cfg in [off(), on_at_cols(6), on_at_cols(40), on_at_cols(80)] {
            for line in 0..buf.len_lines() {
                let laid = layout_visual_rows(&buf, line..line + 1, &cfg, 8.0).len();
                let counted = count_visual_rows_for_line(&buf, line, &cfg, 8.0);
                assert_eq!(
                    counted, laid,
                    "count_visual_rows_for_line disagreed with layout on line {line} cfg {cfg:?}"
                );
            }
        }
    }

    #[test]
    fn count_visual_rows_is_one_on_fast_path() {
        let buf = TextBuffer::new(&"a".repeat(500));
        // Wrap off => always 1 regardless of width.
        assert_eq!(count_visual_rows_for_line(&buf, 0, &off(), 8.0), 1);
        // Degenerate width => 1 (no infinite split).
        let bad = WrapConfig {
            enabled: true,
            wrap_column: None,
            viewport_width_px: 0.0,
        };
        assert_eq!(count_visual_rows_for_line(&buf, 0, &bad, 8.0), 1);
    }
}
