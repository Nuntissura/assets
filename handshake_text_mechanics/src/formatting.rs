//! UTF-16 formatter edits over the canonical text buffer.
//!
//! Positions and ranges are formatting DTOs, independent of an LSP client or UI.
//! This preserves the host formatter's current conversion and edit semantics:
//! columns can consume line endings, split-surrogate columns round forward,
//! and overlaps are not rejected before descending application. In-place errors
//! may follow earlier mutations; the string adapter works on a disposable buffer.

use std::ops::Range;
use crate::buffer::TextBuffer;

/// Zero-based line and UTF-16 code-unit column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LspPosition {
    pub line: u32,
    pub character: u32,
}

/// Half-open formatter range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LspRange {
    pub start: LspPosition,
    pub end: LspPosition,
}

/// Replacement text and its pre-edit range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspTextEdit {
    pub range: LspRange,
    pub new_text: String,
}

/// Apply edits in descending pre-edit byte order and return their count.
pub fn apply_text_edits(
    buffer: &mut TextBuffer,
    edits: &[LspTextEdit],
) -> Result<usize, FormatApplyError> {
    // (1) Resolve every edit to a byte range against the UN-edited buffer (UTF-16 column conversion).
    let mut resolved: Vec<(Range<usize>, String)> = Vec::with_capacity(edits.len());
    for edit in edits {
        let byte_range = lsp_range_to_byte_range(buffer, edit.range).ok_or(FormatApplyError)?;
        resolved.push((byte_range, edit.new_text.clone()));
    }
    // (2) Sort DESCENDING by start byte (RISK-001). On equal starts, longer range first for determinism.
    resolved.sort_by(|a, b| b.0.start.cmp(&a.0.start).then(b.0.end.cmp(&a.0.end)));
    // (3) Apply delete+insert descending so earlier (lower) offsets stay valid.
    let mut applied = 0usize;
    for (range, new_text) in resolved {
        buffer.delete(range.clone()).map_err(|_| FormatApplyError)?;
        buffer
            .insert(range.start, &new_text)
            .map_err(|_| FormatApplyError)?;
        applied += 1;
    }
    Ok(applied)
}

/// Apply `edits` to `text` and return the formatted string (a convenience over [`apply_text_edits`] for
/// the panel's whole-buffer install path and for tests). Sorted-descending + UTF-16-correct, identical to
/// the in-place applier.
pub fn apply_text_edits_to_string(
    text: &str,
    edits: &[LspTextEdit],
) -> Result<String, FormatApplyError> {
    let mut buffer = TextBuffer::new(text);
    apply_text_edits(&mut buffer, edits)?;
    Ok(buffer.to_string())
}

/// An edit could not be applied because its UTF-16 line/character range did not resolve to a valid byte
/// range in the current buffer, or a buffer mutation failed. In-place application can leave earlier
/// edits applied on error; the string adapter discards its temporary buffer on error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatApplyError;

impl std::fmt::Display for FormatApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "a format edit range did not resolve to a valid byte range"
        )
    }
}

impl std::error::Error for FormatApplyError {}

/// Convert an [`LspRange`] (0-based line + UTF-16-code-unit character) to a BYTE range against `buffer`,
/// or `None` when a line cannot be resolved or the resolved range is reversed. Columns beyond the
/// line clamp to its end, including line endings. UTF-16 column semantics (AC-007): the character
/// component is a count of UTF-16 code units on the line, NOT a byte offset and NOT a char index, so on a
/// line with a non-ASCII character (e.g. an emoji = 2 UTF-16 code units, 4 bytes) the byte offset differs
/// from the UTF-16 column and a byte-based conversion would mis-place the edit.
pub fn lsp_range_to_byte_range(buffer: &TextBuffer, range: LspRange) -> Option<Range<usize>> {
    let start = line_utf16_to_byte(
        buffer,
        range.start.line as usize,
        range.start.character as usize,
    )?;
    let end = line_utf16_to_byte(
        buffer,
        range.end.line as usize,
        range.end.character as usize,
    )?;
    if start > end {
        return None;
    }
    Some(start..end)
}

/// Resolve a `(line, utf16_column)` (both 0-based; `utf16_column` is a count of UTF-16 code units into the
/// line) to an absolute BYTE offset, including line endings. A column beyond the full line
/// clamps to its end. The conversion walks the line's characters once, accumulating
/// UTF-16 code-unit width (`ch.len_utf16()`) until the target column is reached, then maps the resulting
/// char position to a byte offset via the buffer's char API. This is the UTF-16 column math the contract
/// requires (RISK-003 / AC-007): a BMP char is 1 UTF-16 unit, a supplementary-plane char (e.g. an emoji)
/// is 2 — so the byte offset and the UTF-16 column diverge exactly when a non-BMP / multi-byte char
/// precedes the column.
fn line_utf16_to_byte(buffer: &TextBuffer, line: usize, utf16_col: usize) -> Option<usize> {
    let line_start_byte = buffer.line_to_byte(line)?;
    // The end byte including line endings (start of next line, or buffer end on the last line). Used to
    // bound the walk so a column past the line end clamps to the line end (server may over-report).
    let line_end_byte = buffer
        .line_to_byte(line + 1)
        .unwrap_or_else(|| buffer.len_bytes());
    // Materialize ONLY this line's text (O(line-length), not O(document)) to walk its chars for UTF-16
    // widths. `slice_to_string(line..line+1)` includes line endings; they consume UTF-16 columns.
    let line_text = buffer.slice_to_string(line..line + 1);

    let mut utf16_seen = 0usize;
    let mut byte_in_line = 0usize;
    for ch in line_text.chars() {
        // Stop at the full line boundary, after any line endings consumed by the column budget.
        if line_start_byte + byte_in_line >= line_end_byte {
            break;
        }
        if utf16_seen >= utf16_col {
            break;
        }
        utf16_seen += ch.len_utf16();
        byte_in_line += ch.len_utf8();
    }
    // Clamp to the line end (a column past end-of-line maps to the line end — never past it).
    let byte = (line_start_byte + byte_in_line).min(line_end_byte);
    Some(byte)
}

/// Convert a BYTE offset to an [`LspRange`]-compatible `(line, utf16_column)` (the inverse of
/// [`line_utf16_to_byte`]), used to build the Format Selection range from the editor's byte-offset
/// selection. Bytes beyond the buffer clamp to its end; bytes inside a code point round forward.
/// The UTF-16 column is computed by walking the line up
/// to the byte offset and summing `ch.len_utf16()` — the SAME UTF-16 discipline as the forward direction
/// so valid character-boundary offsets round-trip through the formatter coordinates (AC-007).
pub fn byte_to_lsp_position(
    buffer: &TextBuffer,
    byte_offset: usize,
) -> Option<LspPosition> {
    let clamped = byte_offset.min(buffer.len_bytes());
    let line = buffer.byte_to_line(clamped)?;
    let line_start_byte = buffer.line_to_byte(line)?;
    let line_text = buffer.slice_to_string(line..line + 1);
    let target_in_line = clamped.saturating_sub(line_start_byte);

    let mut utf16_col = 0usize;
    let mut byte_in_line = 0usize;
    for ch in line_text.chars() {
        if byte_in_line >= target_in_line {
            break;
        }
        byte_in_line += ch.len_utf8();
        utf16_col += ch.len_utf16();
    }
    Some(LspPosition {
        line: line as u32,
        character: utf16_col as u32,
    })
}

/// Build the [`LspRange`] for Format Selection from a primary selection BYTE range `(start, end)` against
/// `buffer`. When the selection is empty/collapsed (`start == end`), the range is the CURRENT LINE's range
/// (line start .. line end), matching VS Code's `editor.action.formatSelection` behavior — NOT a silent
/// fallback to whole-document formatting (the MT is explicit on this). Returns `None` only when the buffer
/// cannot resolve the positions (never panics).
pub fn selection_range_for(buffer: &TextBuffer, start: usize, end: usize) -> Option<LspRange> {
    let (s, e) = (start.min(end), start.max(end));
    if s == e {
        // Empty/collapsed selection: format the caret's current line (line start .. line end).
        let line = buffer.byte_to_line(s.min(buffer.len_bytes()))?;
        let line_start = buffer.line_to_byte(line)?;
        // Line end = start of next line minus its newline, or buffer end on the last line.
        let next_line_start = buffer
            .line_to_byte(line + 1)
            .unwrap_or_else(|| buffer.len_bytes());
        let line_text = buffer.slice_to_string(line..line + 1);
        let content_len = line_text.trim_end_matches(['\n', '\r']).len();
        let line_end = (line_start + content_len).min(next_line_start);
        let start_pos = byte_to_lsp_position(buffer, line_start)?;
        let end_pos = byte_to_lsp_position(buffer, line_end)?;
        return Some(LspRange {
            start: start_pos,
            end: end_pos,
        });
    }
    let start_pos = byte_to_lsp_position(buffer, s)?;
    let end_pos = byte_to_lsp_position(buffer, e)?;
    Some(LspRange {
        start: start_pos,
        end: end_pos,
    })
}

#[cfg(test)]
mod tests {
    use super::*;


    fn edit(sl: u32, sc: u32, el: u32, ec: u32, new_text: &str) -> LspTextEdit {
        LspTextEdit {
            range: LspRange {
                start: LspPosition {
                    line: sl,
                    character: sc,
                },
                end: LspPosition {
                    line: el,
                    character: ec,
                },
            },
            new_text: new_text.to_owned(),
        }
    }

    // ── AC-005 / RISK-001 / MC-004: descending-offset apply (ascending would corrupt) ──────────────
    #[test]
    fn format_descending_order_apply() {
        // Two edits on the SAME line whose naive ascending application corrupts the offsets: replace
        // "a" (col 0..1) with "AAAA" (lengthens) and "b" (col 2..3) with "BBBB". Ascending would apply
        // col 0..1 first, shifting col 2..3 to the wrong text.
        let text = "a b\n";
        let edits = vec![edit(0, 0, 0, 1, "AAAA"), edit(0, 2, 0, 3, "BBBB")];
        let out = apply_text_edits_to_string(text, &edits).expect("applies cleanly");
        assert_eq!(
            out, "AAAA BBBB\n",
            "AC-005: descending apply yields the correct text"
        );

        // Regression: a naive ascending apply against the ORIGINAL offsets mangles the text.
        let mut naive = text.to_owned();
        naive.replace_range(0..1, "AAAA"); // col 0..1 first (lengthens)
        let len = naive.len();
        let (s, e) = (2.min(len), 3.min(len)); // col 2..3 against the now-longer string -> wrong span
        naive.replace_range(s..e, "BBBB");
        assert_ne!(
            naive, out,
            "RISK-001: ascending-order apply must NOT equal the correct result"
        );
    }

    // ── AC-007 / RISK-003 / MC-005: UTF-16 column conversion (byte offset != UTF-16 column) ────────
    #[test]
    fn format_utf16_columns() {
        // A line beginning with an emoji "😀" (U+1F600): 4 UTF-8 bytes, but 2 UTF-16 code units. So the
        // identifier "x" that visually follows it sits at UTF-16 column 2 but BYTE offset 4. An edit that
        // targets "x" via its UTF-16 range (col 2..3) MUST land at byte 4..5, not byte 2..3.
        let text = "😀x = 1\n";
        // Replace "x" (UTF-16 col 2..3) with "y".
        let edits = vec![edit(0, 2, 0, 3, "y")];
        let out = apply_text_edits_to_string(text, &edits).expect("applies cleanly");
        assert_eq!(
            out, "😀y = 1\n",
            "AC-007: the edit landed at the correct UTF-16 column, not the byte offset"
        );

        // Prove the BYTE-based (wrong) interpretation would have landed elsewhere: col 2..3 as bytes is
        // INSIDE the emoji's 4-byte sequence, which is not even a char boundary — a byte-based applier
        // would corrupt or reject. Confirm our UTF-16 conversion maps col 2 -> byte 4.
        let buffer = TextBuffer::new(text);
        let br = lsp_range_to_byte_range(&buffer, edits[0].range).expect("range resolves");
        assert_eq!(
            br,
            4..5,
            "AC-007: UTF-16 col 2..3 maps to byte 4..5 (after the 4-byte emoji)"
        );
    }

    // ── round-trip: byte_to_lsp_position is the inverse of line_utf16_to_byte ──────────────────────
    #[test]
    fn utf16_position_round_trips() {
        let text = "fn 名前() {}\n  return;\n"; // a CJK identifier (each kanji = 1 UTF-16 unit, 3 bytes)
        let buffer = TextBuffer::new(text);
        // Walk CHAR-BOUNDARY byte offsets only (a mid-kanji byte is not a valid position to round-trip).
        // Boundaries: 0,1,2,3 (start 名), 6 (start 前), 9 (')'), and into line 1.
        for byte in [0usize, 3, 6, 9, 13, 15] {
            if let Some(pos) = byte_to_lsp_position(&buffer, byte) {
                let back = lsp_range_to_byte_range(
                    &buffer,
                    LspRange {
                        start: pos,
                        end: pos,
                    },
                )
                .map(|r| r.start);
                assert_eq!(
                    back,
                    Some(byte.min(buffer.len_bytes())),
                    "byte {byte} round-trips via UTF-16"
                );
            }
        }
    }

    // ── empty selection -> current line range (NOT whole document) ─────────────────────────────────
    #[test]
    fn empty_selection_formats_current_line() {
        let text = "line0\nline1xyz\nline2\n";
        let buffer = TextBuffer::new(text);
        // A collapsed caret somewhere on line 1 (byte 8, inside "line1xyz").
        let caret = buffer.line_to_byte(1).unwrap() + 2;
        let range = selection_range_for(&buffer, caret, caret).expect("range");
        assert_eq!(range.start.line, 1, "empty selection -> current line start");
        assert_eq!(range.start.character, 0, "line start column 0");
        assert_eq!(
            range.end.line, 1,
            "empty selection -> current line end (same line)"
        );
        assert_eq!(
            range.end.character,
            "line1xyz".len() as u32,
            "line end = content length"
        );
    }

    // ── non-empty selection -> exact selected range ────────────────────────────────────────────────
    #[test]
    fn non_empty_selection_uses_exact_range() {
        let text = "abcdef\n";
        let buffer = TextBuffer::new(text);
        let range = selection_range_for(&buffer, 1, 4).expect("range"); // select "bcd"
        assert_eq!(
            range.start,
            LspPosition {
                line: 0,
                character: 1
            }
        );
        assert_eq!(
            range.end,
            LspPosition {
                line: 0,
                character: 4
            }
        );
    }

    // ── AC-002: text outside the applied edits is byte-for-byte unchanged ──────────────────────────
    #[test]
    fn edits_leave_surrounding_text_unchanged() {
        let text = "keep0\nEDIT_ME\nkeep2\n";
        // One edit replacing only "EDIT_ME" on line 1.
        let edits = vec![edit(1, 0, 1, 7, "done")];
        let out = apply_text_edits_to_string(text, &edits).unwrap();
        assert_eq!(
            out, "keep0\ndone\nkeep2\n",
            "AC-002: only the edited range changed"
        );
    }

}
