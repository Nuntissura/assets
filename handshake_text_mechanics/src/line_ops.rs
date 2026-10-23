//! Line-edit buffer transforms behind the MT-010 keymap (WP-KERNEL-012 — E1 MT-051).
//!
//! MT-010 (`keymap.rs`) declares the `CodeEditorAction` variants
//! `ToggleComment`, `DuplicateLine`, `MoveLineUp`, `MoveLineDown`, `DeleteLine`, `IndentLine`,
//! `DedentLine`, and `InsertTab` and binds them to the VS Code parity chords (Ctrl+/, Ctrl+Shift+K,
//! Alt+Up/Down, Tab, …). This module supplies the REAL buffer transforms those chords run, so they
//! actually edit the document instead of resolving to a dead key. Every function is a pure transform on
//! the MT-001 [`TextBuffer`] + the MT-003 [`CursorSet`] — no backend, no clipboard, no filesystem
//! (`binds_backend_api = ["NONE"]`).
//!
//! ## Language identity (RISK-007 / MC-007 — no second language enum)
//!
//! The MT contract asked to "import/re-export the MT-001 LanguageRegistry language enum". MT-001 does
//! NOT model language as a Rust enum: a document's language is the stable language-FAMILY id string
//! (`language_id_for_extension` → `"rust"` / `"javascript"`), carried on the
//! highlighter and on `CodeEditorPanel::language_id`. To avoid creating
//! a SECOND competing language type (the exact RISK-007 failure), [`LineEditContext`] keys the comment
//! token on that SAME language-family id string, so a file's highlight language drives its comment token.
//! [`line_comment_token`] is the single token table, and it is NARROWED to the language families the
//! bundled registry can actually produce — `"rust"` and `"javascript"` (both `//`). Other families (`#`
//! for Python/Shell, `--` for SQL/Lua, `//` for TS/C/Go/…) are future work gated on grammar registration:
//! the registry does not bundle their grammar and `language_id_for_extension` does not resolve their
//! extension, so no panel can carry their id and the token would be DEAD in the product. See the
//! [`line_comment_token`] docs for the reachability rationale (adversarial-review hardening).
//!
//! ## Cross-cutting requirements (multi-cursor + single undo)
//!
//! 1. MULTI-CURSOR ([`affected_lines`], MC-001): every transform applies per affected LINE, derived from
//!    the whole cursor set (each cursor's selection may span multiple lines). The line set is sorted +
//!    de-duplicated, so a line two cursors touch transforms exactly once.
//! 2. DESCENDING APPLY (RISK-001 / MC-001): a length-changing edit is applied to the affected lines in
//!    DESCENDING line order, so an earlier (lower-offset) line's byte positions stay valid while a later
//!    (higher) line mutates. After all edits the cursors are recomputed by `(line, column)` against the
//!    post-edit buffer (a moved/duplicated line carries its cursors with it), then `remove_overlap()` +
//!    re-sort run (AC-009).
//! 3. UTF-8 CORRECTNESS (RISK-002 / MC-002): all row/column math goes through
//!    `line_to_byte`/`byte_to_line` + the rope char/byte conversions ([`byte_to_line_col`] /
//!    [`line_col_to_byte`]). The code NEVER hand-scans the rope for `\n` and NEVER assumes byte == char,
//!    so a comment/indent insert lands on a char boundary even on an emoji/CJK/accented line.
//! 4. SINGLE UNDO (RISK-003 / MC-003, AC-007): each public function coalesces ALL of its sub-edits into
//!    ONE logical change. The undo entry is recorded at the panel/factory bus boundary
//!    (`interop_adapter::push_code_edit_undo`, the MT-035/050 pattern: snapshot the whole buffer
//!    before + after the transform → one bus undo entry). This module mutates the buffer in place; the
//!    panel snapshots around the call, so there is no parallel undo stack here.

use super::buffer::TextBuffer;
use super::cursor::{byte_to_line_col, line_col_to_byte, Cursor, CursorSet};

/// A `(line, column)` pair (both in CHARS — UTF-8 safe via [`byte_to_line_col`]).
type LineCol = (usize, usize);

/// A cursor's `(anchor, head)` snapshotted as `(line, col)` pairs BEFORE a transform, so it can be
/// re-resolved to a byte offset on the same logical position against the post-edit buffer.
type CursorLineCols = (LineCol, LineCol);

/// The editor settings + language a line transform needs. Built once per dispatch batch from the panel's
/// language-family id and the operator's tab settings (the MT "build the `LineEditContext` once per
/// dispatch batch" rule). Keyed on the language-FAMILY id string (RISK-007 / MC-007 — NOT a second
/// language enum; see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineEditContext {
    /// The stable language-family id (`"rust"` / `"javascript"`, or `""` when unmapped) — the SAME id
    /// the highlighter + folding read. Drives [`line_comment_token`].
    pub language_id: &'static str,
    /// One indent unit width in spaces when [`insert_spaces`](Self::insert_spaces) is true (the operator's
    /// `editor.tabSize`). Never hardcoded to 4 at a call site — it is plumbed from settings (MC-006).
    pub tab_size: usize,
    /// When true, one indent unit is `tab_size` spaces; when false, one indent unit is a literal `\t`
    /// (the operator's `editor.insertSpaces`). MC-006: both modes are real and tested.
    pub insert_spaces: bool,
}

impl LineEditContext {
    /// Build a context from the language-family id + tab settings.
    pub fn new(language_id: &'static str, tab_size: usize, insert_spaces: bool) -> Self {
        // A zero tab_size would make a spaces-mode indent a no-op (and a dedent loop trivially); clamp to
        // at least 1 so the indent unit is always non-empty in spaces mode (defensive; VS Code's minimum
        // is 1). Tabs-mode ignores tab_size for the indent unit (a single `\t`).
        Self {
            language_id,
            tab_size: tab_size.max(1),
            insert_spaces,
        }
    }

    /// The literal string for ONE indent unit: `tab_size` spaces when [`insert_spaces`](Self::insert_spaces),
    /// else a single tab. The unit Indent/InsertTab inserts and Dedent removes (RISK-006 / MC-006).
    pub fn indent_unit(&self) -> String {
        if self.insert_spaces {
            " ".repeat(self.tab_size)
        } else {
            "\t".to_owned()
        }
    }
}

/// The line-comment token for a language-family id, or `None` for a language with no line comment token
/// reachable through the product (whose ToggleComment is a safe no-op — AC-008). Keyed on the SAME stable
/// language-family id string the highlighter carries (`language_id_for_extension`),
/// so it matches the file's highlight language (RISK-007 / MC-007), never a second enum. Returns a
/// `&'static str` so callers do not allocate.
///
/// REACHABILITY (adversarial-review hardening): the token table is deliberately NARROWED to the language
/// families the bundled `LanguageRegistry::with_bundled_languages` actually produces —
/// `"rust"` (`.rs`) and `"javascript"` (`.js`/`.jsx`/`.mjs`/`.cjs`). Those are the ONLY ids
/// `language_id_for_extension` resolves, and the dispatch path
/// (`CodeEditorPanel::line_edit_context`) feeds this fn exactly the panel's
/// `language_id`, i.e. one of `"rust"` / `"javascript"` / `""`. A wider table (`#` for Python/Shell/YAML,
/// `--` for SQL/Lua, `//` for TS/C/C++/Java/Go/C#) would be DEAD in the product: no panel can carry those
/// ids until the registry bundles the grammar AND `language_id_for_extension` learns the extension — a
/// registry/grammar dependency OUT OF SCOPE for this pure-transform MT (MT-051 `binds_backend_api = NONE`).
/// FUTURE WORK: when a grammar is registered for an extension whose family id is one of those below, add
/// the matching arm here (the `//` C-family + `#`/`--` mappings are the intended targets) so the token
/// follows the new highlight language. Until then this fn returns `None` for them (a safe no-op), and
/// MC-007's `.py`-style proof is correctly scoped to the bundled families only.
pub fn line_comment_token(language_id: &str) -> Option<&'static str> {
    match language_id {
        // Bundled + reachable today: Rust (`.rs`) and JavaScript (`.js`/`.jsx`/`.mjs`/`.cjs`) both use `//`.
        "rust" | "javascript" => Some("//"),
        // No grammar bundled for any `#`/`--`-comment family yet, so no panel can carry these ids — they
        // are intentionally unreachable (future work, gated on grammar registration; see fn docs).
        _ => None,
    }
}

/// The half-open byte range of one buffer LINE INCLUDING its trailing `\n` (so deleting the range
/// removes the whole row). For the last line (no trailing newline) the range ends at `len_bytes()`.
/// Panic-free: an out-of-range line clamps to an empty range at `len_bytes()`.
fn line_byte_span(buf: &TextBuffer, line: usize) -> std::ops::Range<usize> {
    let len = buf.len_bytes();
    let start = buf.line_to_byte(line).unwrap_or(len);
    let total = buf.len_lines();
    let end = if line + 1 < total {
        buf.line_to_byte(line + 1).unwrap_or(len)
    } else {
        len
    };
    start..end.max(start)
}

/// The text of one line WITHOUT its trailing newline (and a trailing `\r` for CRLF). Panic-free.
fn line_text(buf: &TextBuffer, line: usize) -> String {
    let raw = buf.slice_to_string(line..line + 1);
    raw.strip_suffix('\n')
        .map(|s| s.strip_suffix('\r').unwrap_or(s))
        .unwrap_or(&raw)
        .to_owned()
}

/// The set of buffer line indices ANY cursor in `cursors` touches, sorted ascending + de-duplicated
/// (MC-001). Each cursor contributes every line from `byte_to_line(min(anchor,head))` to
/// `byte_to_line(max(anchor,head))` inclusive, so a multi-line selection includes all of its lines and
/// two cursors on the same line yield that line ONCE. Always non-empty (the cursor set always holds at
/// least one caret). This is the [`CursorSet::distinct_lines`] helper the MT names, implemented here so
/// the cursor model stays untouched (the MT "do not refactor the cursor model" rule).
pub fn affected_lines(buf: &TextBuffer, cursors: &CursorSet) -> Vec<usize> {
    let mut lines: Vec<usize> = Vec::new();
    let max_line = buf.len_lines().saturating_sub(1);
    for c in cursors.cursors() {
        let first = byte_to_line_col(c.min(), buf).0.min(max_line);
        let last = byte_to_line_col(c.max(), buf).0.min(max_line);
        for line in first..=last {
            lines.push(line);
        }
    }
    lines.sort_unstable();
    lines.dedup();
    lines
}

/// Snapshot each cursor as `((anchor_line, anchor_col), (head_line, head_col))` BEFORE a transform, so
/// the cursor can be re-resolved to a byte offset against the post-edit buffer on the SAME logical
/// line/column it started on. Column is in CHARS (UTF-8 safe via [`byte_to_line_col`]).
fn snapshot_cursor_line_cols(buf: &TextBuffer, cursors: &CursorSet) -> Vec<CursorLineCols> {
    cursors
        .cursors()
        .iter()
        .map(|c| {
            (
                byte_to_line_col(c.anchor, buf),
                byte_to_line_col(c.head, buf),
            )
        })
        .collect()
}

/// Re-resolve cursor `(line, col)` snapshots to byte offsets against the post-edit `buf`, applying an
/// optional per-cursor LINE delta (used by MoveLineUp/Down + DuplicateLine, where a cursor's line index
/// changes). `line_delta` maps an original head/anchor line to its new line. Column is preserved and
/// clamped to the (possibly changed) line content by [`line_col_to_byte`] (RISK-002). Then
/// `remove_overlap()` + re-sort run via [`CursorSet::set_cursors`] (AC-009).
fn rebuild_cursors_from_line_cols(
    buf: &TextBuffer,
    snaps: &[CursorLineCols],
    line_delta: impl Fn(usize) -> usize,
) -> Vec<Cursor> {
    snaps
        .iter()
        .map(|((al, ac), (hl, hc))| {
            let anchor = line_col_to_byte(line_delta(*al), *ac, buf);
            let head = line_col_to_byte(line_delta(*hl), *hc, buf);
            Cursor::selection(anchor, head)
        })
        .collect()
}

/// The number of CHARS of leading whitespace on `line_text` and the BYTE offset of the first
/// non-whitespace char (relative to the line start). Used by ToggleComment to find the comment column.
/// UTF-8 safe: leading whitespace is ASCII space/tab, so the byte and char counts of the prefix agree,
/// but the FIRST non-ws column is reported in chars for cross-line min-column comparison.
fn leading_ws(line_text: &str) -> (usize, usize) {
    let trimmed = line_text.trim_start();
    let ws_bytes = line_text.len() - trimmed.len();
    // Whitespace before the first token is ASCII (space/tab), so char count == byte count here.
    let ws_chars = line_text[..ws_bytes].chars().count();
    (ws_chars, ws_bytes)
}

// ── ToggleComment ───────────────────────────────────────────────────────────────────────────────────

/// Toggle a line comment on every affected line (Ctrl+/). VS Code ALL-OR-NOTHING semantics (RISK-004 /
/// MC-004): if EVERY affected non-blank line is already commented, UNCOMMENT all; otherwise COMMENT all
/// at a consistent insert column (the minimum first-non-whitespace column across the affected non-blank
/// block, so the comment markers line up). A language with no line-comment token is a safe no-op
/// (AC-008). One coalesced change (the panel records the single undo). Returns true if the buffer changed.
pub fn toggle_comment(
    buf: &mut TextBuffer,
    cursors: &mut CursorSet,
    ctx: &LineEditContext,
) -> bool {
    let Some(token) = line_comment_token(ctx.language_id) else {
        return false; // AC-008: no line comment for this language -> no-op.
    };
    let lines = affected_lines(buf, cursors);
    if lines.is_empty() {
        return false;
    }

    // Decide comment vs uncomment from the VS Code rule: uncomment only if EVERY non-blank affected line
    // is already commented. Blank lines are ignored for the decision (they have no token to test).
    let mut any_non_blank = false;
    let mut all_commented = true;
    let mut min_col = usize::MAX;
    for &line in &lines {
        let text = line_text(buf, line);
        let trimmed = text.trim_start();
        if trimmed.is_empty() {
            continue; // blank line: ignored for the decision + min-column.
        }
        any_non_blank = true;
        let (col, _) = leading_ws(&text);
        min_col = min_col.min(col);
        if !trimmed.starts_with(token) {
            all_commented = false;
        }
    }
    if !any_non_blank {
        return false; // all affected lines blank: nothing to comment.
    }
    let uncomment = all_commented;

    // Snapshot cursors so they keep their logical column after the byte shift on their line.
    let snaps = snapshot_cursor_line_cols(buf, cursors);
    // Track the per-line byte delta applied at-or-before each cursor's column so the cursor column shifts
    // with the inserted/removed token. Because comment edits never change a cursor's LINE, the line delta
    // is identity; only the column shifts, which line_col_to_byte handles by re-clamping to the line. The
    // simplest correct recompute: re-resolve each cursor by (line, col) but ADD/REMOVE the token width to
    // the column when the cursor sits at/after the insert column on a commented line. We compute that
    // adjustment per cursor below after the edits.

    let token_with_space = format!("{token} ");
    let insert_col = if min_col == usize::MAX { 0 } else { min_col };

    let mut changed = false;
    // Apply DESCENDING so a lower line's byte offsets stay valid while higher lines mutate (RISK-001).
    for &line in lines.iter().rev() {
        let text = line_text(buf, line);
        let trimmed = text.trim_start();
        let line_start = buf.line_to_byte(line).unwrap_or(0);
        if uncomment {
            if trimmed.is_empty() {
                continue;
            }
            // Remove the token (+ one following space if present) at the first non-ws byte.
            let (_, ws_bytes) = leading_ws(&text);
            let at = line_start + ws_bytes;
            if let Some(rest) = trimmed.strip_prefix(token) {
                let mut remove_len = token.len();
                if rest.starts_with(' ') {
                    remove_len += 1;
                }
                if buf.delete(at..at + remove_len).is_ok() {
                    changed = true;
                }
            }
        } else {
            // COMMENT: insert `token + " "` at the consistent insert column. For a blank line VS Code
            // still inserts at column 0 (the block's insert column) so the block lines up.
            let at = byte_at_column(buf, line, insert_col);
            if buf.insert(at, &token_with_space).is_ok() {
                changed = true;
            }
        }
    }
    if !changed {
        return false;
    }

    // Recompute each cursor: same line, column shifted by the token width when the cursor is at/after the
    // insert column on a NON-BLANK line (so the caret stays on the same character after the token slid in,
    // and drops back after an uncomment). line_col_to_byte re-clamps the column to the new line content.
    let token_width = token_with_space.chars().count();
    let adjusted = adjust_comment_cursors(buf, &snaps, insert_col, token_width, uncomment);
    cursors.set_cursors(adjusted, buf);
    true
}

/// The BYTE offset of char-column `col` on `line`, clamped to the line content end (panic-free, UTF-8
/// safe — RISK-002). Reuses [`line_col_to_byte`] so a column past a short line snaps to its content end.
fn byte_at_column(buf: &TextBuffer, line: usize, col: usize) -> usize {
    line_col_to_byte(line, col, buf)
}

/// Shift each cursor's column by the comment token width on its line when it sits at/after the comment
/// insert column on a non-blank line. On comment: columns >= insert_col move right by `token_width`. On
/// uncomment: columns >= insert_col move left by up to `token_width` (clamped at the insert column).
fn adjust_comment_cursors(
    buf: &TextBuffer,
    snaps: &[CursorLineCols],
    insert_col: usize,
    token_width: usize,
    uncomment: bool,
) -> Vec<Cursor> {
    let shift = |line: usize, col: usize| -> usize {
        let text = line_text(buf, line);
        // A line whose (post-edit) content is empty was blank; do not shift a caret on it.
        let new_col = if text.trim_start().is_empty() {
            col
        } else if col >= insert_col {
            if uncomment {
                col.saturating_sub(token_width).max(insert_col.min(col))
            } else {
                col + token_width
            }
        } else {
            col
        };
        line_col_to_byte(line, new_col, buf)
    };
    snaps
        .iter()
        .map(|((al, ac), (hl, hc))| Cursor::selection(shift(*al, *ac), shift(*hl, *hc)))
        .collect()
}

// ── DuplicateLine ───────────────────────────────────────────────────────────────────────────────────

/// Duplicate every affected line below itself (Shift+Alt+Down / the DuplicateLine action). The copy is
/// inserted immediately after the line's end (a leading `\n` is prepended for the last line which has no
/// trailing newline, so the copy lands on its own row). The cursor moves to the DUPLICATED (lower) line —
/// VS Code parity. One coalesced change. Returns true if the buffer changed.
pub fn duplicate_line(
    buf: &mut TextBuffer,
    cursors: &mut CursorSet,
    _ctx: &LineEditContext,
) -> bool {
    let lines = affected_lines(buf, cursors);
    if lines.is_empty() {
        return false;
    }
    let snaps = snapshot_cursor_line_cols(buf, cursors);
    let total_before = buf.len_lines();

    let mut changed = false;
    // DESCENDING so earlier insertions never shift later (higher) line offsets (RISK-001).
    for &line in lines.iter().rev() {
        let text = line_text(buf, line);
        let is_last = line + 1 >= buf.len_lines();
        if is_last {
            // Last line has no trailing newline: append "\n<copy>" at the buffer end so the copy is a new
            // final row and the original keeps its no-trailing-newline shape.
            let at = buf.len_bytes();
            if buf.insert(at, &format!("\n{text}")).is_ok() {
                changed = true;
            }
        } else {
            // Insert "<copy>\n" at the start of the NEXT line so the duplicate sits directly below.
            let at = buf
                .line_to_byte(line + 1)
                .unwrap_or_else(|| buf.len_bytes());
            if buf.insert(at, &format!("{text}\n")).is_ok() {
                changed = true;
            }
        }
    }
    if !changed {
        return false;
    }

    // The cursor follows to the duplicated (lower) copy. Each original affected line L gains one new line
    // below it; a cursor on line L should move down by the number of affected lines at or BELOW L that
    // were duplicated before L's copy (descending insert keeps the copy directly under each original, so
    // a cursor on the FIRST affected line moves down 1, on the second affected line moves down 2, ... by
    // the count of affected lines <= its own line). _total_before is used only to bound clamping.
    let _ = total_before;
    let line_delta = |orig_line: usize| -> usize {
        // Count affected lines strictly above-or-equal that received a copy before this row in the final
        // buffer. Each affected line A <= orig_line inserts one row at A+1, pushing orig_line down by 1.
        let below_or_equal = lines.iter().filter(|&&a| a <= orig_line).count();
        orig_line + below_or_equal
    };
    let rebuilt = rebuild_cursors_from_line_cols(buf, &snaps, line_delta);
    cursors.set_cursors(rebuilt, buf);
    true
}

// ── MoveLineUp / MoveLineDown ─────────────────────────────────────────────────────────────────────────

/// Move the affected line(s) up one row, swapping with the line above (Alt+Up). A no-op when the topmost
/// affected line is line 0 (RISK-005 / MC-005). The cursors travel with their line (head/anchor line
/// index decremented). One coalesced change. Returns true if the buffer changed.
pub fn move_line_up(buf: &mut TextBuffer, cursors: &mut CursorSet, _ctx: &LineEditContext) -> bool {
    move_block(buf, cursors, true)
}

/// Move the affected line(s) down one row, swapping with the line below (Alt+Down). A no-op when the
/// bottommost affected line is the last line (RISK-005 / MC-005). The cursors travel with their line.
/// One coalesced change. Returns true if the buffer changed.
pub fn move_line_down(
    buf: &mut TextBuffer,
    cursors: &mut CursorSet,
    _ctx: &LineEditContext,
) -> bool {
    move_block(buf, cursors, false)
}

/// Shared MoveLineUp/Down: treat the affected lines as a contiguous block (the common VS Code case is a
/// single line or a selection spanning consecutive lines) and swap it with the single neighbor line above
/// (`up`) or below. Boundary no-op at the document edge. The cursors move with the block.
fn move_block(buf: &mut TextBuffer, cursors: &mut CursorSet, up: bool) -> bool {
    let lines = affected_lines(buf, cursors);
    let Some(&first) = lines.first() else {
        return false;
    };
    let Some(&last) = lines.last() else {
        return false;
    };
    let total = buf.len_lines();
    if up {
        if first == 0 {
            return false; // RISK-005: nothing above line 0.
        }
    } else if last + 1 >= total {
        return false; // RISK-005: nothing below the last line.
    }

    let snaps = snapshot_cursor_line_cols(buf, cursors);

    // The neighbor that crosses the block, and the block bounds.
    let neighbor = if up { first - 1 } else { last + 1 };
    let block_lo = first.min(neighbor);
    let block_hi = last.max(neighbor);

    // Read every line in [block_lo, block_hi] as text (without trailing newlines), then rewrite the span
    // with the lines reordered: moving UP puts the neighbor (the old `first-1`) AFTER the block; moving
    // DOWN puts the neighbor (the old `last+1`) BEFORE the block.
    let texts: Vec<String> = (block_lo..=block_hi).map(|l| line_text(buf, l)).collect();
    let reordered: Vec<String> = if up {
        // [neighbor, block...] -> [block..., neighbor]
        let mut v: Vec<String> = texts[1..].to_vec();
        v.push(texts[0].clone());
        v
    } else {
        // [block..., neighbor] -> [neighbor, block...]
        let mut v: Vec<String> = vec![texts[texts.len() - 1].clone()];
        v.extend_from_slice(&texts[..texts.len() - 1]);
        v
    };

    // The byte span of the block, and whether the last block line is the buffer's final line (no newline).
    let span_start = buf.line_to_byte(block_lo).unwrap_or(0);
    let last_block_is_final = block_hi + 1 >= total;
    let span_end = if last_block_is_final {
        buf.len_bytes()
    } else {
        buf.line_to_byte(block_hi + 1)
            .unwrap_or_else(|| buf.len_bytes())
    };
    // Rebuild the block text, preserving the trailing-newline shape: if the block reached the buffer end
    // (no trailing newline), the rewritten block also has no trailing newline.
    let joined = reordered.join("\n");
    let new_block = if last_block_is_final {
        joined
    } else {
        format!("{joined}\n")
    };

    let ok = buf.delete(span_start..span_end).is_ok() && buf.insert(span_start, &new_block).is_ok();
    if !ok {
        return false;
    }

    // Each affected line moved by one row in the `up`/`down` direction; the neighbor moved the opposite
    // way. Cursors were on the affected lines, so shift their line by -1 (up) or +1 (down).
    let line_delta = move |orig_line: usize| -> usize {
        if up {
            orig_line.saturating_sub(1)
        } else {
            orig_line + 1
        }
    };
    let rebuilt = rebuild_cursors_from_line_cols(buf, &snaps, line_delta);
    cursors.set_cursors(rebuilt, buf);
    true
}

// ── DeleteLine ──────────────────────────────────────────────────────────────────────────────────────

/// Delete every affected whole row INCLUDING its trailing newline (Ctrl+Shift+K). When the LAST row is
/// deleted, the PRECEDING newline is removed too so no empty trailing line remains (RISK-005 / MC-005).
/// Each surviving cursor lands at the start of the row that now occupies the deleted row's position (or
/// the new last row / buffer end). One coalesced change. Returns true if the buffer changed.
pub fn delete_line(buf: &mut TextBuffer, cursors: &mut CursorSet, _ctx: &LineEditContext) -> bool {
    let lines = affected_lines(buf, cursors);
    if lines.is_empty() {
        return false;
    }
    // The lowest affected line index, used to place the cursor after the deletions.
    let target_line = *lines.first().unwrap_or(&0);

    let mut changed = false;
    // DESCENDING so earlier (lower) line offsets stay valid while higher lines are removed (RISK-001).
    for &line in lines.iter().rev() {
        let total = buf.len_lines();
        let is_last = line + 1 >= total;
        let span = if is_last && line > 0 {
            // Last row (and not the only row): also drop the PRECEDING newline so no empty trailing line
            // remains. The preceding newline is the byte just before this line's start.
            let start = buf.line_to_byte(line).unwrap_or_else(|| buf.len_bytes());
            let drop_from = start.saturating_sub(1);
            drop_from..buf.len_bytes()
        } else {
            // Normal row (or the only row): delete [line_start, next_line_start) (includes the newline).
            line_byte_span(buf, line)
        };
        if span.end > span.start && buf.delete(span).is_ok() {
            changed = true;
        }
    }
    if !changed {
        return false;
    }

    // Place a single caret at the start of the row that now sits where the lowest deleted row was, clamped
    // to the new last row (or buffer end when everything was deleted).
    let new_total = buf.len_lines();
    let landing_line = target_line.min(new_total.saturating_sub(1));
    let head = buf
        .line_to_byte(landing_line)
        .unwrap_or_else(|| buf.len_bytes());
    cursors.set_cursors(vec![Cursor::caret(head)], buf);
    true
}

// ── IndentLine / DedentLine ───────────────────────────────────────────────────────────────────────────

/// Add one indent unit (a tab or `tab_size` spaces per [`LineEditContext::insert_spaces`]) at the start
/// of every affected line (RISK-006 / MC-006). One coalesced change. Returns true if the buffer changed.
pub fn indent_line(buf: &mut TextBuffer, cursors: &mut CursorSet, ctx: &LineEditContext) -> bool {
    let lines = affected_lines(buf, cursors);
    if lines.is_empty() {
        return false;
    }
    let unit = ctx.indent_unit();
    let unit_cols = unit.chars().count();
    let snaps = snapshot_cursor_line_cols(buf, cursors);

    let mut changed = false;
    for &line in lines.iter().rev() {
        let at = buf.line_to_byte(line).unwrap_or(0);
        if buf.insert(at, &unit).is_ok() {
            changed = true;
        }
    }
    if !changed {
        return false;
    }
    // Each indented line shifts its caret columns right by the unit width (the caret stays on the same
    // character). A line NOT in the affected set is unchanged.
    let affected: std::collections::HashSet<usize> = lines.iter().copied().collect();
    let rebuilt = shift_columns_on_lines(buf, &snaps, &affected, unit_cols as isize);
    cursors.set_cursors(rebuilt, buf);
    true
}

/// Remove up to ONE indent unit from the start of every affected line (RISK-006 / MC-006): a leading tab,
/// else up to `tab_size` leading spaces (fewer if the line has fewer). Dedent on an already-flush line is
/// a safe no-op. One coalesced change. Returns true if the buffer changed.
pub fn dedent_line(buf: &mut TextBuffer, cursors: &mut CursorSet, ctx: &LineEditContext) -> bool {
    let lines = affected_lines(buf, cursors);
    if lines.is_empty() {
        return false;
    }
    let snaps = snapshot_cursor_line_cols(buf, cursors);
    // Per-line removed-column count (so the cursor column shift is exact per line).
    let mut removed_cols: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();

    let mut changed = false;
    for &line in lines.iter().rev() {
        let text = line_text(buf, line);
        let line_start = buf.line_to_byte(line).unwrap_or(0);
        // One indent unit to remove: a leading tab counts as one unit; else up to tab_size leading spaces.
        let remove = if text.starts_with('\t') {
            1
        } else {
            text.chars()
                .take(ctx.tab_size)
                .take_while(|c| *c == ' ')
                .count()
        };
        if remove > 0 && buf.delete(line_start..line_start + remove).is_ok() {
            changed = true;
            removed_cols.insert(line, remove);
        }
    }
    if !changed {
        return false;
    }
    // Each cursor on a dedented line shifts left by that line's removed-column count, clamped so the caret
    // never moves before the line start.
    let rebuilt: Vec<Cursor> = snaps
        .iter()
        .map(|((al, ac), (hl, hc))| {
            let anchor_col = ac.saturating_sub(removed_cols.get(al).copied().unwrap_or(0));
            let head_col = hc.saturating_sub(removed_cols.get(hl).copied().unwrap_or(0));
            let anchor = line_col_to_byte(*al, anchor_col, buf);
            let head = line_col_to_byte(*hl, head_col, buf);
            Cursor::selection(anchor, head)
        })
        .collect();
    cursors.set_cursors(rebuilt, buf);
    true
}

/// Shift the columns of every cursor whose line is in `affected` by `delta` (positive = right). Lines not
/// in `affected` keep their columns. Re-resolves to byte offsets against the post-edit buffer (RISK-002).
fn shift_columns_on_lines(
    buf: &TextBuffer,
    snaps: &[CursorLineCols],
    affected: &std::collections::HashSet<usize>,
    delta: isize,
) -> Vec<Cursor> {
    let apply = |line: usize, col: usize| -> usize {
        let new_col = if affected.contains(&line) {
            (col as isize + delta).max(0) as usize
        } else {
            col
        };
        line_col_to_byte(line, new_col, buf)
    };
    snaps
        .iter()
        .map(|((al, ac), (hl, hc))| Cursor::selection(apply(*al, *ac), apply(*hl, *hc)))
        .collect()
}

// ── InsertTab ───────────────────────────────────────────────────────────────────────────────────────

/// The Tab key (collapsed/single-line case): insert ONE indent unit (a tab or `tab_size` spaces per
/// [`LineEditContext::insert_spaces`]) at every cursor head, advancing the heads past the inserted text.
/// When ANY cursor has a MULTI-LINE selection, route to [`indent_line`] block-indent instead (VS Code:
/// Tab with a block selection indents the block rather than replacing it). One coalesced change.
/// Returns true if the buffer changed.
pub fn insert_tab(buf: &mut TextBuffer, cursors: &mut CursorSet, ctx: &LineEditContext) -> bool {
    // Multi-line selection anywhere -> block indent (VS Code parity).
    let has_multiline_selection = cursors.cursors().iter().any(|c| {
        c.is_selection() && byte_to_line_col(c.min(), buf).0 != byte_to_line_col(c.max(), buf).0
    });
    if has_multiline_selection {
        return indent_line(buf, cursors, ctx);
    }
    // Collapsed (or single-line-selection) case: insert one indent unit at every cursor via the cursor
    // set's own insert path, which replaces any single-line selection and advances each head past the
    // inserted text + re-normalizes (AC-009). This reuses the MT-003 `insert_at_all` discipline.
    let unit = ctx.indent_unit();
    cursors.insert_at_all(&unit, buf) > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rust_ctx() -> LineEditContext {
        LineEditContext::new("rust", 4, true)
    }

    fn set_at(buf: &TextBuffer, offset: usize) -> CursorSet {
        let mut s = CursorSet::single(offset);
        s.normalize(buf);
        s
    }

    // ── ToggleComment (AC-001, MC-004) ────────────────────────────────────────────────────────────────

    #[test]
    fn toggle_comment_single_line_on_then_off() {
        let mut buf = TextBuffer::new("let x = 1;");
        let mut cur = set_at(&buf, 0);
        // ON.
        assert!(toggle_comment(&mut buf, &mut cur, &rust_ctx()));
        assert_eq!(buf.to_string(), "// let x = 1;");
        // OFF (the same single line is now fully commented -> uncomment).
        assert!(toggle_comment(&mut buf, &mut cur, &rust_ctx()));
        assert_eq!(buf.to_string(), "let x = 1;");
    }

    #[test]
    fn toggle_comment_multi_cursor_each_touched_line() {
        let mut buf = TextBuffer::new("a\nb\nc\n");
        // Two cursors: one on line 0, one on line 2 (line 1 untouched).
        let mut cur = CursorSet::default();
        let l0 = buf.line_to_byte(0).unwrap();
        let l2 = buf.line_to_byte(2).unwrap();
        cur.set_cursors(vec![Cursor::caret(l0), Cursor::caret(l2)], &buf);
        assert!(toggle_comment(&mut buf, &mut cur, &rust_ctx()));
        assert_eq!(
            buf.to_string(),
            "// a\nb\n// c\n",
            "only the two touched lines commented"
        );
    }

    #[test]
    fn toggle_comment_already_commented_block_uncomments_all() {
        let mut buf = TextBuffer::new("// a\n// b\n// c");
        // A selection spanning all three lines.
        let end = buf.len_bytes();
        let mut cur = CursorSet::default();
        cur.set_cursors(vec![Cursor::selection(0, end)], &buf);
        assert!(toggle_comment(&mut buf, &mut cur, &rust_ctx()));
        assert_eq!(
            buf.to_string(),
            "a\nb\nc",
            "all-commented block uncomments all"
        );
    }

    #[test]
    fn toggle_comment_mixed_block_comments_all() {
        // MC-004: a mixed block (some commented, some not) COMMENTS all (VS Code all-or-nothing).
        let mut buf = TextBuffer::new("// a\nb\nc");
        let end = buf.len_bytes();
        let mut cur = CursorSet::default();
        cur.set_cursors(vec![Cursor::selection(0, end)], &buf);
        assert!(toggle_comment(&mut buf, &mut cur, &rust_ctx()));
        // The already-commented line gets a SECOND token at the consistent insert column (col 0).
        assert_eq!(buf.to_string(), "// // a\n// b\n// c");
    }

    #[test]
    fn toggle_comment_consistent_insert_column_indented_block() {
        // VS Code parity: the insert column is the MINIMUM first-non-ws column of the block, so every
        // line's `// ` marker lands at the SAME column (col 4 here). The deeper-indented line keeps its
        // extra leading whitespace AFTER the marker (the marker column is consistent, the content is not
        // re-aligned) — this is exactly editor.action.commentLine's behavior.
        let mut buf = TextBuffer::new("    a\n        b");
        let end = buf.len_bytes();
        let mut cur = CursorSet::default();
        cur.set_cursors(vec![Cursor::selection(0, end)], &buf);
        assert!(toggle_comment(&mut buf, &mut cur, &rust_ctx()));
        // min col = 4: both markers at column 4; line 1 keeps its extra 4 spaces after the marker.
        assert_eq!(buf.to_string(), "    // a\n    //     b");
        // And uncomment restores the exact original (round-trip — the markers are removed at column 4).
        assert!(toggle_comment(&mut buf, &mut cur, &rust_ctx()));
        assert_eq!(buf.to_string(), "    a\n        b");
    }

    #[test]
    fn toggle_comment_no_token_language_is_noop() {
        // AC-008: a language with no line-comment token leaves the buffer unchanged.
        let mut buf = TextBuffer::new("hello world");
        let mut cur = set_at(&buf, 0);
        let ctx = LineEditContext::new("plaintext", 4, true);
        assert!(!toggle_comment(&mut buf, &mut cur, &ctx));
        assert_eq!(buf.to_string(), "hello world");
    }

    #[test]
    fn toggle_comment_utf8_line_lands_on_char_boundary() {
        // MC-002: an emoji/CJK/accented line — the token must insert at a char boundary, never mid-glyph.
        let mut buf = TextBuffer::new("héllo 🚀 世界");
        let mut cur = set_at(&buf, 0);
        assert!(toggle_comment(&mut buf, &mut cur, &rust_ctx()));
        assert_eq!(
            buf.to_string(),
            "// héllo 🚀 世界",
            "token prepended at col 0, no corruption"
        );
        // And uncomment restores it exactly.
        assert!(toggle_comment(&mut buf, &mut cur, &rust_ctx()));
        assert_eq!(buf.to_string(), "héllo 🚀 世界");
    }

    // ── DuplicateLine (AC-003) ────────────────────────────────────────────────────────────────────────

    #[test]
    fn duplicate_line_copies_verbatim_and_moves_cursor_down() {
        let mut buf = TextBuffer::new("first\nsecond\nthird");
        // Cursor on line 1 ("second"), column 2.
        let head = line_col_to_byte(1, 2, &buf);
        let mut cur = set_at(&buf, head);
        assert!(duplicate_line(&mut buf, &mut cur, &rust_ctx()));
        assert_eq!(buf.to_string(), "first\nsecond\nsecond\nthird");
        // VS Code: the cursor is on the DUPLICATED (lower) line — now line 2, same column.
        let (line, col) = byte_to_line_col(cur.primary().head, &buf);
        assert_eq!((line, col), (2, 2), "cursor moved to the duplicated line");
    }

    #[test]
    fn duplicate_last_line_no_trailing_newline() {
        let mut buf = TextBuffer::new("only\nlast");
        let head = buf.line_to_byte(1).unwrap();
        let mut cur = set_at(&buf, head);
        assert!(duplicate_line(&mut buf, &mut cur, &rust_ctx()));
        assert_eq!(
            buf.to_string(),
            "only\nlast\nlast",
            "copy is a new final row"
        );
    }

    // ── MoveLineUp / MoveLineDown (AC-002, MC-005) ────────────────────────────────────────────────────

    #[test]
    fn move_line_up_swaps_and_moves_cursor() {
        let mut buf = TextBuffer::new("one\ntwo\nthree");
        let head = line_col_to_byte(1, 1, &buf); // line "two", col 1
        let mut cur = set_at(&buf, head);
        assert!(move_line_up(&mut buf, &mut cur, &rust_ctx()));
        assert_eq!(buf.to_string(), "two\none\nthree");
        let (line, col) = byte_to_line_col(cur.primary().head, &buf);
        assert_eq!((line, col), (0, 1), "cursor moved up with its line");
    }

    #[test]
    fn move_line_down_swaps_and_moves_cursor() {
        let mut buf = TextBuffer::new("one\ntwo\nthree");
        let head = line_col_to_byte(1, 2, &buf);
        let mut cur = set_at(&buf, head);
        assert!(move_line_down(&mut buf, &mut cur, &rust_ctx()));
        assert_eq!(buf.to_string(), "one\nthree\ntwo");
        let (line, _) = byte_to_line_col(cur.primary().head, &buf);
        assert_eq!(line, 2, "cursor moved down with its line");
    }

    #[test]
    fn move_line_up_at_top_is_noop() {
        let mut buf = TextBuffer::new("one\ntwo");
        let mut cur = set_at(&buf, 0); // line 0
        assert!(
            !move_line_up(&mut buf, &mut cur, &rust_ctx()),
            "MoveLineUp@0 is a no-op"
        );
        assert_eq!(buf.to_string(), "one\ntwo");
    }

    #[test]
    fn move_line_down_at_bottom_is_noop() {
        let mut buf = TextBuffer::new("one\ntwo");
        let head = buf.line_to_byte(1).unwrap();
        let mut cur = set_at(&buf, head); // last line
        assert!(
            !move_line_down(&mut buf, &mut cur, &rust_ctx()),
            "MoveLineDown@last is a no-op"
        );
        assert_eq!(buf.to_string(), "one\ntwo");
    }

    #[test]
    fn move_line_down_into_last_line_preserves_no_trailing_newline() {
        // Moving line 0 down past the last (no-trailing-newline) line must keep a clean two-line buffer.
        let mut buf = TextBuffer::new("a\nb");
        let mut cur = set_at(&buf, 0);
        assert!(move_line_down(&mut buf, &mut cur, &rust_ctx()));
        assert_eq!(
            buf.to_string(),
            "b\na",
            "no stray trailing newline introduced"
        );
    }

    // ── DeleteLine (AC-006, MC-005) ───────────────────────────────────────────────────────────────────

    #[test]
    fn delete_line_removes_row_and_trailing_newline() {
        let mut buf = TextBuffer::new("a\nb\nc\n");
        let head = buf.line_to_byte(1).unwrap();
        let mut cur = set_at(&buf, head);
        assert!(delete_line(&mut buf, &mut cur, &rust_ctx()));
        assert_eq!(
            buf.to_string(),
            "a\nc\n",
            "the whole row + its newline removed"
        );
        // Cursor at the start of the row that now occupies the deleted row's position.
        let (line, col) = byte_to_line_col(cur.primary().head, &buf);
        assert_eq!((line, col), (1, 0));
    }

    #[test]
    fn delete_last_line_leaves_no_empty_trailing_line() {
        // RISK-005: deleting the last row (no trailing newline) must NOT leave a stray empty line.
        let mut buf = TextBuffer::new("a\nb\nc");
        let head = buf.line_to_byte(2).unwrap();
        let mut cur = set_at(&buf, head);
        assert!(delete_line(&mut buf, &mut cur, &rust_ctx()));
        assert_eq!(
            buf.to_string(),
            "a\nb",
            "no empty trailing line; preceding newline removed"
        );
        assert_eq!(buf.len_lines(), 2);
    }

    #[test]
    fn delete_only_line_empties_buffer_without_panic() {
        let mut buf = TextBuffer::new("solo");
        let mut cur = set_at(&buf, 0);
        assert!(delete_line(&mut buf, &mut cur, &rust_ctx()));
        assert_eq!(buf.to_string(), "");
        // Cursor clamped to buffer start, no panic.
        assert_eq!(cur.primary().head, 0);
    }

    #[test]
    fn delete_line_multi_cursor_removes_each_once() {
        let mut buf = TextBuffer::new("a\nb\nc\nd\n");
        let l0 = buf.line_to_byte(0).unwrap();
        let l2 = buf.line_to_byte(2).unwrap();
        let mut cur = CursorSet::default();
        cur.set_cursors(vec![Cursor::caret(l0), Cursor::caret(l2)], &buf);
        assert!(delete_line(&mut buf, &mut cur, &rust_ctx()));
        assert_eq!(
            buf.to_string(),
            "b\nd\n",
            "lines 0 and 2 removed, 1 and 3 survive"
        );
    }

    // ── Indent / Dedent (AC-004, MC-006) ──────────────────────────────────────────────────────────────

    #[test]
    fn indent_line_spaces_mode_respects_tab_size() {
        let mut buf = TextBuffer::new("x");
        let mut cur = set_at(&buf, 0);
        let ctx = LineEditContext::new("rust", 2, true); // 2 spaces
        assert!(indent_line(&mut buf, &mut cur, &ctx));
        assert_eq!(buf.to_string(), "  x");
        // Cursor stays on the same char (column shifted right by 2).
        assert_eq!(byte_to_line_col(cur.primary().head, &buf), (0, 2));
    }

    #[test]
    fn indent_line_tab_mode_inserts_tab_char() {
        let mut buf = TextBuffer::new("x");
        let mut cur = set_at(&buf, 0);
        let ctx = LineEditContext::new("rust", 4, false); // tab char
        assert!(indent_line(&mut buf, &mut cur, &ctx));
        assert_eq!(buf.to_string(), "\tx");
    }

    #[test]
    fn dedent_removes_one_unit_spaces() {
        let mut buf = TextBuffer::new("      x"); // 6 spaces
        let mut cur = set_at(&buf, 0);
        let ctx = LineEditContext::new("rust", 4, true);
        assert!(dedent_line(&mut buf, &mut cur, &ctx));
        assert_eq!(
            buf.to_string(),
            "  x",
            "removed up to 4 leading spaces (one unit)"
        );
    }

    #[test]
    fn dedent_removes_one_tab() {
        let mut buf = TextBuffer::new("\t\tx");
        let mut cur = set_at(&buf, 0);
        let ctx = LineEditContext::new("rust", 4, false);
        assert!(dedent_line(&mut buf, &mut cur, &ctx));
        assert_eq!(buf.to_string(), "\tx", "removed one leading tab");
    }

    #[test]
    fn dedent_flush_line_is_noop() {
        let mut buf = TextBuffer::new("x");
        let mut cur = set_at(&buf, 0);
        let ctx = LineEditContext::new("rust", 4, true);
        assert!(
            !dedent_line(&mut buf, &mut cur, &ctx),
            "dedent on a flush line is a no-op"
        );
        assert_eq!(buf.to_string(), "x");
    }

    #[test]
    fn dedent_fewer_spaces_than_tab_size() {
        let mut buf = TextBuffer::new("  x"); // 2 spaces, tab_size 4
        let mut cur = set_at(&buf, 0);
        let ctx = LineEditContext::new("rust", 4, true);
        assert!(dedent_line(&mut buf, &mut cur, &ctx));
        assert_eq!(
            buf.to_string(),
            "x",
            "removed both leading spaces (fewer than tab_size)"
        );
    }

    // ── InsertTab (AC-005) ────────────────────────────────────────────────────────────────────────────

    #[test]
    fn insert_tab_collapsed_cursor_spaces() {
        let mut buf = TextBuffer::new("ab");
        let mut cur = set_at(&buf, 1); // between a and b
        let ctx = LineEditContext::new("rust", 4, true);
        assert!(insert_tab(&mut buf, &mut cur, &ctx));
        assert_eq!(buf.to_string(), "a    b");
        assert_eq!(
            cur.primary().head,
            5,
            "head advanced past the 4 inserted spaces"
        );
    }

    #[test]
    fn insert_tab_collapsed_cursor_tab_char() {
        let mut buf = TextBuffer::new("ab");
        let mut cur = set_at(&buf, 1);
        let ctx = LineEditContext::new("rust", 4, false);
        assert!(insert_tab(&mut buf, &mut cur, &ctx));
        assert_eq!(buf.to_string(), "a\tb");
    }

    #[test]
    fn insert_tab_multiline_selection_block_indents() {
        // AC-005: Tab with a multi-line selection block-indents instead of replacing the selection.
        let mut buf = TextBuffer::new("a\nb\nc");
        let end = buf.len_bytes();
        let mut cur = CursorSet::default();
        cur.set_cursors(vec![Cursor::selection(0, end)], &buf);
        let ctx = LineEditContext::new("rust", 4, true);
        assert!(insert_tab(&mut buf, &mut cur, &ctx));
        assert_eq!(
            buf.to_string(),
            "    a\n    b\n    c",
            "block indented, selection NOT replaced"
        );
    }

    // ── AC-009: cursor set stays sorted + de-overlapped after a transform ─────────────────────────────

    #[test]
    fn cursors_remain_sorted_and_non_overlapping_after_transform() {
        let mut buf = TextBuffer::new("a\nb\nc\nd");
        let mut cur = CursorSet::default();
        // Three carets, deliberately out of order.
        let l2 = buf.line_to_byte(2).unwrap();
        let l0 = buf.line_to_byte(0).unwrap();
        let l1 = buf.line_to_byte(1).unwrap();
        cur.set_cursors(
            vec![Cursor::caret(l2), Cursor::caret(l0), Cursor::caret(l1)],
            &buf,
        );
        assert!(indent_line(&mut buf, &mut cur, &rust_ctx()));
        // Sorted by head, strictly increasing, no duplicates (remove_overlap ran via set_cursors).
        let heads: Vec<usize> = cur.cursors().iter().map(|c| c.head).collect();
        let mut sorted = heads.clone();
        sorted.sort_unstable();
        assert_eq!(heads, sorted, "cursors sorted by head after the transform");
        for w in heads.windows(2) {
            assert!(w[0] < w[1], "no duplicate/overlapping cursors: {heads:?}");
        }
    }
}
