//! Multi-cursor + column/box selection model for the native code editor (WP-KERNEL-012 MT-003).
//!
//! This module owns the EDITING-INTENT state that sits on top of the [`TextBuffer`](super::buffer::TextBuffer):
//! a set of cursors, each a `(anchor, head)` byte-offset pair. It is the native analog of the Monaco
//! multi-cursor behavior the React surface gets for free, reimplemented over our own buffer because we
//! own the text and render it manually (the MT rule: never `egui::TextEdit`).
//!
//! ## Field-converged model (research provenance wf_ffa74d6d)
//!
//! [`Cursor`] = `{ anchor, head }` byte offsets and [`CursorSet`] = `Vec<Cursor>` IS the Helix
//! `Selection` model the research converged on — kept verbatim. A cursor whose `anchor == head` is a
//! bare caret; a cursor whose `anchor != head` is a [`Selection`] (a range). `head` is the moving end
//! (where typing/extension happens); `anchor` is the fixed end. This matches VS Code / Monaco /
//! Helix / CodeMirror, so the same keybindings produce the same results.
//!
//! ## Invariants (enforced after EVERY mutation)
//!
//! 1. Every `anchor`/`head` is clamped to `0..=buffer.len_bytes()` and snapped to a char boundary —
//!    an out-of-range or mid-char offset (from a stale span, an agent action, or a deserialized
//!    cursor) is impossible to store (implementation note 1; RISK-002 alignment).
//! 2. The set is kept sorted by `head` (then `anchor`) so render order, AccessKit node order, and the
//!    reverse-order edit walk are all deterministic.
//! 3. Overlapping cursors are merged ([`CursorSet::remove_overlap`]) so two cursors can never edit the
//!    same span twice.
//!
//! [`CursorSet::normalize`] re-establishes all three after any change; the public mutators call it so a
//! caller cannot leave the set in a broken state.
//!
//! ## Edit-time offset drift (RISK-001 — the off-by-N trap)
//!
//! Inserting/deleting at cursor *i* shifts every later byte offset. [`CursorSet::insert_at_all`] and
//! [`CursorSet::delete_at_all`] therefore process cursors in REVERSE order by `head` (largest offset
//! first) so an earlier edit never invalidates a later cursor's stored offset, and they additionally
//! re-shift the surviving cursors by the byte delta so the cursors END UP at the position right after
//! their own inserted/deleted text. A dedicated unit test pins this (AC-001 / AC-006 / MC-001).

use super::buffer::TextBuffer;

/// The maximum number of cursors that get a live AccessKit node (RISK-004 / MC-004). A pathological
/// column-select over thousands of lines must not flood the accessibility tree with thousands of
/// `TextCursor` nodes (which would bloat every per-frame `TreeUpdate` a swarm agent reads). The edit
/// model still tracks ALL cursors; only the first `MAX_ACCESSKIT_CURSORS` are surfaced as nodes.
pub const MAX_ACCESSKIT_CURSORS: usize = 64;

/// Direction for [`CursorSet::move_all`]. Mirrors the Monaco/VS Code caret-movement vocabulary; the
/// word variants stop at the same alphanumeric/`_` token boundary [`word_at`] uses, so Ctrl+Left and
/// Ctrl+D agree on what a "word" is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveDir {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    LineEnd,
    WordLeft,
    WordRight,
}

/// One cursor: a half-open `(anchor, head)` pair of BYTE offsets into the buffer. `anchor == head` is a
/// bare caret; `anchor != head` is a selection. Both ends are always on a char boundary and within
/// `0..=len_bytes` (the [`CursorSet`] invariants).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cursor {
    /// The fixed end of the selection (equals `head` for a bare caret).
    pub anchor: usize,
    /// The moving end — where insertion happens and where extension grows from.
    pub head: usize,
}

impl Cursor {
    /// A bare caret at `offset` (anchor == head).
    pub fn caret(offset: usize) -> Self {
        Self {
            anchor: offset,
            head: offset,
        }
    }

    /// A selection from `anchor` to `head`. The constructor does not order them — `head` may be before
    /// or after `anchor` (a backward selection is valid and common when extending leftward).
    pub fn selection(anchor: usize, head: usize) -> Self {
        Self { anchor, head }
    }

    /// True when this cursor selects a non-empty range (anchor != head).
    pub fn is_selection(&self) -> bool {
        self.anchor != self.head
    }

    /// The selected byte range as `start..end` with `start <= end`, regardless of selection direction.
    /// For a bare caret this is the empty range `head..head`.
    pub fn range(&self) -> std::ops::Range<usize> {
        let start = self.anchor.min(self.head);
        let end = self.anchor.max(self.head);
        start..end
    }

    /// The lower byte offset of the cursor (min of anchor/head).
    pub fn min(&self) -> usize {
        self.anchor.min(self.head)
    }

    /// The higher byte offset of the cursor (max of anchor/head).
    pub fn max(&self) -> usize {
        self.anchor.max(self.head)
    }
}

/// An ordered, de-overlapped set of [`Cursor`]s. The single owner of multi-cursor editing intent; the
/// panel reads it to render carets/selections and to apply edits to all cursors at once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CursorSet {
    /// Always sorted by `(head, anchor)` and free of overlaps after any public mutation. Never empty
    /// in practice (a fresh set seeds a caret at 0), but an explicitly emptied set degrades to a
    /// single caret at 0 on the next [`normalize`](Self::normalize) so the editor always has a caret.
    cursors: Vec<Cursor>,
}

impl CursorSet {
    /// A fresh set with a single caret at byte offset 0 (the editor's initial state).
    pub fn new() -> Self {
        Self {
            cursors: vec![Cursor::caret(0)],
        }
    }

    /// A set seeded with a single caret at `offset` (clamped to the buffer on the next normalize by the
    /// caller; callers that need clamping use [`set_primary`](Self::set_primary)).
    pub fn single(offset: usize) -> Self {
        Self {
            cursors: vec![Cursor::caret(offset)],
        }
    }

    /// All cursors, in sorted order. The render path and AccessKit emitter iterate this.
    pub fn cursors(&self) -> &[Cursor] {
        &self.cursors
    }

    /// How many cursors are in the set.
    pub fn len(&self) -> usize {
        self.cursors.len()
    }

    /// True when the set has no cursors (only transiently, between a clear and a normalize).
    pub fn is_empty(&self) -> bool {
        self.cursors.is_empty()
    }

    /// The PRIMARY cursor — the last one in sorted order (the most-recently-added / lowest-on-screen).
    /// Monaco treats the primary cursor as the one Ctrl+D and word lookups operate from. Returns a
    /// caret at 0 for a (transiently) empty set rather than panicking.
    pub fn primary(&self) -> Cursor {
        self.cursors
            .last()
            .copied()
            .unwrap_or_else(|| Cursor::caret(0))
    }

    /// Replace the whole set with a single caret at `offset`, clamped + char-snapped to `buffer`. This
    /// is the plain (non-Alt) click / Escape-to-single behavior.
    pub fn set_primary(&mut self, offset: usize, buffer: &TextBuffer) {
        self.cursors = vec![Cursor::caret(offset)];
        self.normalize(buffer);
    }

    /// Replace the whole set with `cursors`, then normalize (clamp/snap/sort/merge). Used by box/column
    /// selection which computes one cursor per line in the range (step 5).
    pub fn set_cursors(&mut self, cursors: Vec<Cursor>, buffer: &TextBuffer) {
        self.cursors = cursors;
        self.normalize(buffer);
    }

    /// Add a bare caret at `byte_offset` (Alt+Click / Ctrl+Alt+Up/Down). De-duplicated by offset and
    /// merged against any overlapping cursor by [`normalize`](Self::normalize), so adding a caret that
    /// already exists is a no-op.
    pub fn add_cursor(&mut self, byte_offset: usize, buffer: &TextBuffer) {
        self.cursors.push(Cursor::caret(byte_offset));
        self.normalize(buffer);
    }

    /// Add a SELECTION cursor `anchor..head` (Ctrl+D adds a selection over the next occurrence).
    pub fn add_selection(&mut self, anchor: usize, head: usize, buffer: &TextBuffer) {
        self.cursors.push(Cursor::selection(anchor, head));
        self.normalize(buffer);
    }

    /// Add a caret one line ABOVE each existing cursor's head, keeping the same target column
    /// (Ctrl+Alt+Up). Cursors already on line 0 contribute nothing (no line above). Uses the
    /// column of each head so the new carets stack vertically like Monaco's "add cursor above".
    pub fn add_cursor_above(&mut self, buffer: &TextBuffer) {
        self.add_cursor_vertical(buffer, true);
    }

    /// Add a caret one line BELOW each existing cursor's head, keeping the same target column
    /// (Ctrl+Alt+Down). Cursors on the last line contribute nothing.
    pub fn add_cursor_below(&mut self, buffer: &TextBuffer) {
        self.add_cursor_vertical(buffer, false);
    }

    fn add_cursor_vertical(&mut self, buffer: &TextBuffer, above: bool) {
        let mut additions = Vec::new();
        for cursor in &self.cursors {
            let (line, col) = byte_to_line_col(cursor.head, buffer);
            let target_line = if above {
                if line == 0 {
                    continue; // no line above line 0
                }
                line - 1
            } else {
                if line + 1 >= buffer.len_lines() {
                    continue; // no line below the last line
                }
                line + 1
            };
            let offset = line_col_to_byte(target_line, col, buffer);
            additions.push(Cursor::caret(offset));
        }
        self.cursors.extend(additions);
        self.normalize(buffer);
    }

    /// Move EVERY cursor one unit in `direction`, collapsing selections to a bare caret at the moved
    /// head (the standard non-shift arrow behavior). Both ends move to the new head so the result is a
    /// caret. Char-boundary safe via the buffer conversions.
    pub fn move_all(&mut self, direction: MoveDir, buffer: &TextBuffer) {
        for cursor in &mut self.cursors {
            let new_head = move_offset(cursor.head, direction, buffer);
            cursor.anchor = new_head;
            cursor.head = new_head;
        }
        self.normalize(buffer);
    }

    /// EXTEND every cursor's selection by moving only its HEAD in `direction`, keeping the anchor fixed
    /// (Shift+Arrow / Shift+Word / Shift+Home/End — MT-010 selection commands). Unlike
    /// [`move_all`](Self::move_all) (which collapses to a caret), this grows or shrinks the selection.
    pub fn extend_all(&mut self, direction: MoveDir, buffer: &TextBuffer) {
        for cursor in &mut self.cursors {
            cursor.head = move_offset(cursor.head, direction, buffer);
        }
        self.normalize(buffer);
    }

    /// For each BARE caret, extend it over the adjacent WORD (`to_left` -> the word to the left; else the
    /// word to the right) so a subsequent [`delete_at_all`](Self::delete_at_all) deletes that word
    /// (Ctrl+Backspace / Ctrl+Delete — MT-010 `DeleteWordLeft` / `DeleteWordRight`). A selection is left
    /// unchanged.
    pub fn select_word_for_bare_carets(&mut self, to_left: bool, buffer: &TextBuffer) {
        let dir = if to_left {
            MoveDir::WordLeft
        } else {
            MoveDir::WordRight
        };
        for cursor in &mut self.cursors {
            if !cursor.is_selection() {
                // Keep the anchor where the caret is; move the head to the word boundary so the range
                // covers the word to delete.
                cursor.head = move_offset(cursor.head, dir, buffer);
            }
        }
        self.normalize(buffer);
    }

    /// Insert `text` at EVERY cursor's head, replacing any selected range first, and leave each cursor
    /// as a bare caret immediately AFTER its inserted text. The buffer is mutated in place.
    ///
    /// RISK-001: cursors are processed in REVERSE order by position (largest offset first) so an
    /// earlier insertion never shifts a later cursor's stored offset out from under it. After mutating
    /// the buffer, the surviving cursors are re-shifted by the cumulative byte delta of every edit that
    /// happened at a LOWER offset, so a cursor at offset 5 after an `X` inserted at offset 0 ends at 6
    /// (AC-006). Returns the number of insertions actually applied.
    pub fn insert_at_all(&mut self, text: &str, buffer: &mut TextBuffer) -> usize {
        let text_len = text.len();
        if self.cursors.is_empty() {
            return 0;
        }
        // Snapshot the cursors sorted by their edit position (range start). We mutate the buffer from
        // the LAST edit backward so earlier edits cannot move a later edit's offset.
        let mut edits: Vec<(usize, std::ops::Range<usize>)> = self
            .cursors
            .iter()
            .enumerate()
            .map(|(i, c)| (i, c.range()))
            .collect();
        edits.sort_by_key(|(_, r)| (r.start, r.end));

        // New caret offset per original cursor index, computed AS we apply edits low->high so each
        // cursor's final position accounts for the byte deltas of all edits at or before it.
        let mut new_heads = vec![0usize; self.cursors.len()];
        let mut cumulative_delta: isize = 0;
        // Apply in reverse (high->low) so the buffer mutation offsets stay valid, but compute the
        // new caret positions from the low->high cumulative delta. Two passes keep both correct.
        // Pass 1 (low->high): compute final caret positions.
        for (orig_idx, range) in &edits {
            let removed = range.end - range.start;
            // The caret lands right after the inserted text, at the (shifted) range start.
            let shifted_start = (range.start as isize + cumulative_delta) as usize;
            new_heads[*orig_idx] = shifted_start + text_len;
            cumulative_delta += text_len as isize - removed as isize;
        }
        // Pass 2 (high->low): mutate the buffer. Deleting the selection then inserting keeps the
        // offsets of not-yet-processed (lower) edits valid because we never touch bytes below them.
        let mut applied = 0usize;
        for (_, range) in edits.iter().rev() {
            if range.end > range.start {
                // Replace a selection: delete then insert at the (now-empty) start.
                if buffer.delete(range.clone()).is_ok() && buffer.insert(range.start, text).is_ok()
                {
                    applied += 1;
                }
            } else if buffer.insert(range.start, text).is_ok() {
                applied += 1;
            }
        }
        // Rebuild the cursor set from the computed caret positions, then normalize.
        let new_cursors: Vec<Cursor> = new_heads.into_iter().map(Cursor::caret).collect();
        self.cursors = new_cursors;
        self.normalize(buffer);
        applied
    }

    /// Delete at EVERY cursor: the selected range if the cursor is a selection, otherwise one char
    /// BEFORE the head (Backspace semantics). Processes high->low so earlier deletions never invalidate
    /// later offsets (RISK-001), and re-positions each surviving caret at the start of what it removed.
    /// Returns the number of deletions actually applied.
    pub fn delete_at_all(&mut self, buffer: &mut TextBuffer) -> usize {
        if self.cursors.is_empty() {
            return 0;
        }
        // Build the byte range each cursor deletes: its selection, or the char before a bare caret.
        let mut edits: Vec<(usize, std::ops::Range<usize>)> =
            Vec::with_capacity(self.cursors.len());
        for (i, c) in self.cursors.iter().enumerate() {
            let range = if c.is_selection() {
                c.range()
            } else {
                // Backspace: remove the whole GRAPHEME CLUSTER immediately before the head (MT-077 AC4)
                // so deleting a family emoji removes ALL its codepoints, not one. None at offset 0.
                let head = c.head;
                if head == 0 {
                    0..0
                } else {
                    let prev = prev_grapheme(head, buffer);
                    prev..head
                }
            };
            edits.push((i, range));
        }
        edits.sort_by_key(|(_, r)| (r.start, r.end));

        // Pass 1 (low->high): final caret position = the (shifted) start of the removed range.
        let mut new_heads = vec![0usize; self.cursors.len()];
        let mut cumulative_delta: isize = 0;
        for (orig_idx, range) in &edits {
            let removed = range.end - range.start;
            let shifted_start = (range.start as isize + cumulative_delta) as usize;
            new_heads[*orig_idx] = shifted_start;
            cumulative_delta -= removed as isize;
        }
        // Pass 2 (high->low): mutate the buffer.
        let mut applied = 0usize;
        for (_, range) in edits.iter().rev() {
            if range.end > range.start && buffer.delete(range.clone()).is_ok() {
                applied += 1;
            }
        }
        self.cursors = new_heads.into_iter().map(Cursor::caret).collect();
        self.normalize(buffer);
        applied
    }

    /// Forward-delete at EVERY cursor (the Delete key / `DeleteRight`): the selected range if the cursor
    /// is a selection, otherwise the char immediately AFTER the head. A bare caret at end-of-buffer
    /// deletes NOTHING — VS Code's Delete-at-EOF is a no-op, never a Backspace.
    ///
    /// This is a forward-specific sibling of [`delete_at_all`](Self::delete_at_all): the two differ ONLY
    /// in what a bare caret removes (Backspace removes the char BEFORE; Delete removes the char AFTER and
    /// no-ops at EOF). It exists as its own method rather than composing a forward-extend step with
    /// `delete_at_all`, because at EOF that composition left the caret bare and `delete_at_all` then
    /// applied Backspace semantics — silently eating the preceding char (the EOF data-corruption bug
    /// fixed here). Processes high->low so earlier deletions never invalidate later offsets (RISK-001).
    pub fn delete_forward_at_all(&mut self, buffer: &mut TextBuffer) -> usize {
        if self.cursors.is_empty() {
            return 0;
        }
        // Build the byte range each cursor deletes: its selection, or the char AFTER a bare caret.
        let mut edits: Vec<(usize, std::ops::Range<usize>)> =
            Vec::with_capacity(self.cursors.len());
        for (i, c) in self.cursors.iter().enumerate() {
            let range = if c.is_selection() {
                c.range()
            } else {
                let head = c.head;
                // Delete the whole GRAPHEME CLUSTER AFTER the caret (MT-077 AC4). At EOF `next_grapheme`
                // returns `head`, so the range is empty (`head..head`) — a no-op, matching VS Code's
                // Delete-at-EOF.
                let next = next_grapheme(head, buffer);
                if next > head {
                    head..next
                } else {
                    // Bare caret at end-of-buffer: delete nothing.
                    head..head
                }
            };
            edits.push((i, range));
        }
        edits.sort_by_key(|(_, r)| (r.start, r.end));

        // Pass 1 (low->high): final caret position = the (shifted) start of the removed range.
        let mut new_heads = vec![0usize; self.cursors.len()];
        let mut cumulative_delta: isize = 0;
        for (orig_idx, range) in &edits {
            let removed = range.end - range.start;
            let shifted_start = (range.start as isize + cumulative_delta) as usize;
            new_heads[*orig_idx] = shifted_start;
            cumulative_delta -= removed as isize;
        }
        // Pass 2 (high->low): mutate the buffer.
        let mut applied = 0usize;
        for (_, range) in edits.iter().rev() {
            if range.end > range.start && buffer.delete(range.clone()).is_ok() {
                applied += 1;
            }
        }
        self.cursors = new_heads.into_iter().map(Cursor::caret).collect();
        self.normalize(buffer);
        applied
    }

    /// Merge any cursors whose selections OVERLAP (or whose carets coincide). Two cursors overlap when
    /// their `range()`s intersect; the merged cursor spans the union and keeps the later head direction.
    /// Called from [`normalize`](Self::normalize); exposed because the MT contract names it explicitly.
    /// Assumes the set is already sorted by head (normalize sorts first).
    pub fn remove_overlap(&mut self) {
        if self.cursors.len() < 2 {
            return;
        }
        // Sort by range start so a single forward sweep can coalesce overlaps.
        self.cursors.sort_by_key(|c| (c.min(), c.max()));
        let mut merged: Vec<Cursor> = Vec::with_capacity(self.cursors.len());
        for cur in self.cursors.drain(..) {
            match merged.last_mut() {
                // Overlap or touch a bare caret sitting exactly at the previous range end: merge.
                Some(prev) if cur.min() <= prev.max() => {
                    let new_min = prev.min().min(cur.min());
                    let new_max = prev.max().max(cur.max());
                    // Two bare carets at the same offset collapse to one caret; otherwise the union is a
                    // forward selection (anchor=min, head=max) — direction is lost on merge, which is the
                    // standard editor behavior (a merged multi-selection becomes one forward selection).
                    *prev = if new_min == new_max {
                        Cursor::caret(new_min)
                    } else {
                        Cursor::selection(new_min, new_max)
                    };
                }
                _ => merged.push(cur),
            }
        }
        self.cursors = merged;
    }

    /// Re-establish all invariants after a mutation: clamp + char-snap every end, sort by `(head,
    /// anchor)`, merge overlaps, and guarantee at least one caret. Idempotent.
    pub fn normalize(&mut self, buffer: &TextBuffer) {
        let len = buffer.len_bytes();
        for c in &mut self.cursors {
            c.anchor = clamp_to_char_boundary(c.anchor, len, buffer);
            c.head = clamp_to_char_boundary(c.head, len, buffer);
        }
        // Merge overlaps (this also sorts by range start).
        self.remove_overlap();
        // Final sort by head so render/AccessKit order is deterministic and primary() is the last.
        self.cursors.sort_by_key(|c| (c.head, c.anchor));
        if self.cursors.is_empty() {
            self.cursors.push(Cursor::caret(0));
        }
    }
}

// ── Free helpers (word lookup, line/col mapping, movement) ──────────────────────────────────────────

/// The half-open byte range of the "word" containing `byte_offset` (Ctrl+D / WordLeft/Right). A word is
/// a maximal run of alphanumeric or `_` characters. If `byte_offset` is not inside a word (whitespace
/// or punctuation), returns the empty range `byte_offset..byte_offset`. Scans left then right from the
/// offset over the buffer text. Char-boundary safe.
pub fn word_at(byte_offset: usize, buffer: &TextBuffer) -> std::ops::Range<usize> {
    let text = buffer.to_string();
    let bytes = text.as_bytes();
    let len = bytes.len();
    let off = byte_offset.min(len);
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';

    // If the offset sits at the END of a word (cursor just past the last char), step back one so a
    // caret after "foo|" still finds "foo".
    let mut start = off;
    let mut end = off;
    // Prefer the char at `off`; if it is not a word char but the char before is, anchor on the left.
    let on_word_here = off < len && is_word(bytes[off]);
    let on_word_left = off > 0 && is_word(bytes[off - 1]);
    if !on_word_here && !on_word_left {
        return off..off; // not in a word
    }
    if !on_word_here && on_word_left {
        // Caret is just past a word; anchor inside it.
        start = off;
        end = off;
    }
    // Scan left over word chars (these are ASCII, so byte stepping is char-safe).
    while start > 0 && is_word(bytes[start - 1]) {
        start -= 1;
    }
    // Scan right over word chars.
    while end < len && is_word(bytes[end]) {
        end += 1;
    }
    start..end
}

/// Find the next occurrence of `needle` strictly AFTER `from` (wrapping to the start of the buffer if
/// none is found after `from`). Returns the byte range of the match, or `None` if `needle` is empty or
/// does not occur anywhere. Used by Ctrl+D: it searches from the primary cursor's selection end.
///
/// RISK-003 (no infinite loop): the search wraps exactly once. If the only occurrence is the one the
/// caller is already on, the wrapped search returns that same range and the caller compares it to the
/// origin to decide whether to stop — this function never loops itself.
pub fn find_next_occurrence(
    needle: &str,
    from: usize,
    buffer: &TextBuffer,
) -> Option<std::ops::Range<usize>> {
    if needle.is_empty() {
        return None;
    }
    let text = buffer.to_string();
    let from = from.min(text.len());
    // Search the tail after `from` first.
    if let Some(rel) = text[from..].find(needle) {
        let start = from + rel;
        return Some(start..start + needle.len());
    }
    // Wrap: search from the very start (so we can land on an occurrence at-or-before `from`).
    if let Some(start) = text.find(needle) {
        return Some(start..start + needle.len());
    }
    None
}

/// The `(line, column)` of a byte offset, where column is the number of CHARS from the line start to
/// the offset (so column aligns with monospace glyph columns, not bytes — RISK-002 for non-ASCII).
pub fn byte_to_line_col(byte_offset: usize, buffer: &TextBuffer) -> (usize, usize) {
    let len = buffer.len_bytes();
    let off = byte_offset.min(len);
    let line = buffer.byte_to_line(off).unwrap_or(0);
    let line_start = buffer.line_to_byte(line).unwrap_or(0);
    // Column = chars between the line start and the offset.
    let start_char = buffer.byte_to_char(line_start).unwrap_or(0);
    let off_char = buffer
        .byte_to_char(off)
        .or_else(|| buffer.byte_to_char(prev_char_boundary(off, buffer)))
        .unwrap_or(start_char);
    (line, off_char.saturating_sub(start_char))
}

/// The byte offset of `(line, column)`, clamping the column to the line's char length (RISK-002:
/// a column past the end of a short line snaps to the line end, never past it / into the next line).
pub fn line_col_to_byte(line: usize, column: usize, buffer: &TextBuffer) -> usize {
    let len_lines = buffer.len_lines();
    let line = line.min(len_lines.saturating_sub(1));
    let line_start = buffer.line_to_byte(line).unwrap_or(0);
    // The line's exclusive end is the start of the next line (or buffer end on the last line),
    // minus the trailing newline so the column cannot land on/after the '\n'.
    let next_start = buffer
        .line_to_byte(line + 1)
        .unwrap_or_else(|| buffer.len_bytes());
    let line_text_end = trim_trailing_newline_byte(line_start, next_start, buffer);

    let start_char = buffer.byte_to_char(line_start).unwrap_or(0);
    let end_char = buffer.byte_to_char(line_text_end).unwrap_or(start_char);
    // RISK-002 / overflow-safe: `column` may be `usize::MAX` (callers pass it to mean "to the line's
    // content end", e.g. the whole-line selection paint branch). On any line where `line_start > 0`,
    // `start_char > 0`, so a plain `start_char + column` would overflow `usize` and PANIC under
    // debug overflow-checks (or wrap to a garbage column in release). Saturating add caps at the
    // line's `end_char` clamp, so a giant column always lands on the line content end — never UB.
    let target_char = start_char.saturating_add(column).min(end_char);
    buffer.char_to_byte(target_char).unwrap_or(line_start)
}

/// The byte offset of the last char of `[line_start, next_start)` that is NOT the trailing `\n`. So a
/// column clamp lands on the visible line content, never on the newline. Char-boundary safe.
fn trim_trailing_newline_byte(line_start: usize, next_start: usize, buffer: &TextBuffer) -> usize {
    if next_start <= line_start {
        return line_start;
    }
    // The byte just before next_start; if it is '\n', the line content ends one byte earlier.
    let text = buffer.to_string();
    let bytes = text.as_bytes();
    let end = next_start.min(bytes.len());
    if end > line_start && bytes.get(end - 1) == Some(&b'\n') {
        // Also drop a preceding '\r' for CRLF lines.
        if end >= 2 && bytes.get(end - 2) == Some(&b'\r') {
            end - 2
        } else {
            end - 1
        }
    } else {
        end
    }
}

/// Clamp `offset` to `0..=len` and snap DOWN to the nearest char boundary so a stored cursor is always
/// a valid edit point (invariant 1). A boundary offset is returned unchanged.
fn clamp_to_char_boundary(offset: usize, len: usize, buffer: &TextBuffer) -> usize {
    let off = offset.min(len);
    if buffer.byte_to_char(off).is_some() {
        off
    } else {
        prev_char_boundary(off, buffer)
    }
}

/// The largest char boundary `<= offset`. Used to snap a mid-char offset down (clamp). Returns 0 if
/// none below. NOTE: this is the CLAMP helper (snap a stored offset to a valid byte). Caret MOVEMENT and
/// Backspace use [`prev_grapheme`] / [`next_grapheme`] instead so a multi-codepoint grapheme cluster is
/// crossed whole (MT-077 AC3/AC4) rather than one codepoint at a time.
fn prev_char_boundary(offset: usize, buffer: &TextBuffer) -> usize {
    let mut o = offset.min(buffer.len_bytes());
    while o > 0 {
        o -= 1;
        if buffer.byte_to_char(o).is_some() {
            return o;
        }
    }
    0
}

/// The previous GRAPHEME-CLUSTER boundary `< offset` (MT-077 AC3/AC4). Caret LEFT / Backspace move/delete
/// by a whole user-perceived character: a family ZWJ emoji, a combining-accent sequence, a flag, or a
/// Hangul syllable crosses in ONE step rather than tearing the cluster. Delegates to the shared
/// `text_intl::grapheme` module, which segments only a LOCAL window around the caret for perf (RISK-1),
/// not the whole buffer. The code buffer is byte-addressed, so this works in byte offsets directly.
///
/// `byte_slice_to_string` materializes only a bounded window around the caret (not the whole document),
/// keeping each keypress O(window) regardless of line length. The window is sized to comfortably contain
/// any realistic grapheme cluster plus the shared module's own local window, so the local answer equals
/// the global answer for any cluster a human can author.
fn prev_grapheme(offset: usize, buffer: &TextBuffer) -> usize {
    let len = buffer.len_bytes();
    // Snap the caret DOWN to a char boundary (returns the offset unchanged when already on one — NOT a
    // step back; `clamp_to_char_boundary` is the snap, `prev_char_boundary` would step one char left).
    let off = clamp_to_char_boundary(offset.min(len), len, buffer);
    if off == 0 {
        return 0;
    }
    // Materialize a bounded window ending AT the caret; segment within it. The window start is the
    // caret minus a local span, SNAPPED to a char boundary HERE (not left to byte_slice_to_string) so
    // the `win_start` variable EXACTLY matches the window's first byte — otherwise the `win_start +
    // local` remap below would be off by the snap delta on non-ASCII text at the window edge.
    let win_start = clamp_to_char_boundary(
        off.saturating_sub(crate::text_intl::GRAPHEME_LOCAL_WINDOW_BYTES),
        len,
        buffer,
    );
    let window = buffer.byte_slice_to_string(win_start..off);
    // The caret sits at the END of the window; the previous grapheme boundary within the window, mapped
    // back to an absolute buffer offset, is `win_start + local`.
    let local = crate::text_intl::prev_grapheme_boundary(&window, window.len());
    let candidate = win_start + local;
    // Safety: if the window started mid-cluster (the cluster straddled win_start), `local == 0` means the
    // boundary is at or before win_start; fall back to a single char step so the caret still advances.
    if candidate < off {
        // Snap to a char boundary defensively (byte_slice_to_string already char-aligned the window).
        clamp_to_char_boundary(candidate, len, buffer)
    } else {
        prev_char_boundary(off, buffer)
    }
}

/// The smallest char boundary `> offset` (the next char start). Returns `len_bytes` if none above. The
/// CLAMP/forward-scan helper; caret movement uses [`next_grapheme`].
fn next_char_boundary(offset: usize, buffer: &TextBuffer) -> usize {
    let len = buffer.len_bytes();
    let mut o = offset.min(len);
    while o < len {
        o += 1;
        if buffer.byte_to_char(o).is_some() {
            return o;
        }
    }
    len
}

/// The next GRAPHEME-CLUSTER boundary `> offset` (MT-077 AC3/AC4). Caret RIGHT / forward-delete move by a
/// whole user-perceived character. Local-window segmentation (RISK-1) over the byte-addressed buffer.
fn next_grapheme(offset: usize, buffer: &TextBuffer) -> usize {
    let len = buffer.len_bytes();
    let off = clamp_to_char_boundary(offset.min(len), len, buffer);
    if off >= len {
        return len;
    }
    let win_end = (off + crate::text_intl::GRAPHEME_LOCAL_WINDOW_BYTES).min(len);
    let window = buffer.byte_slice_to_string(off..win_end);
    // The caret sits at the START of the window; the next grapheme boundary within the window maps to
    // `off + local`.
    let local = crate::text_intl::next_grapheme_boundary(&window, 0);
    let candidate = off + local;
    if candidate > off {
        clamp_to_char_boundary(candidate, len, buffer)
    } else {
        next_char_boundary(off, buffer)
    }
}

/// Move a single byte offset one unit in `direction`. Char-boundary and line-boundary safe; never
/// returns an out-of-range or mid-char offset.
fn move_offset(offset: usize, direction: MoveDir, buffer: &TextBuffer) -> usize {
    match direction {
        // MT-077 AC3: caret LEFT/RIGHT move by a whole GRAPHEME CLUSTER, not one codepoint, so a family
        // ZWJ emoji / combining sequence / flag / Hangul syllable is never split mid-cluster.
        MoveDir::Left => prev_grapheme(offset, buffer),
        MoveDir::Right => next_grapheme(offset, buffer),
        MoveDir::Up => {
            let (line, col) = byte_to_line_col(offset, buffer);
            if line == 0 {
                line_col_to_byte(0, 0, buffer)
            } else {
                line_col_to_byte(line - 1, col, buffer)
            }
        }
        MoveDir::Down => {
            let (line, col) = byte_to_line_col(offset, buffer);
            line_col_to_byte(line + 1, col, buffer)
        }
        MoveDir::LineStart => {
            let (line, _) = byte_to_line_col(offset, buffer);
            buffer.line_to_byte(line).unwrap_or(0)
        }
        MoveDir::LineEnd => {
            let (line, _) = byte_to_line_col(offset, buffer);
            let next_start = buffer
                .line_to_byte(line + 1)
                .unwrap_or_else(|| buffer.len_bytes());
            let line_start = buffer.line_to_byte(line).unwrap_or(0);
            trim_trailing_newline_byte(line_start, next_start, buffer)
        }
        MoveDir::WordLeft => {
            // To the start of the word at/just-before the offset; if already at a word start, to the
            // start of the previous word.
            let prev = prev_char_boundary(offset, buffer);
            let w = word_at(prev, buffer);
            if w.start < offset {
                w.start
            } else {
                prev
            }
        }
        MoveDir::WordRight => {
            let w = word_at(offset, buffer);
            if w.end > offset {
                w.end
            } else {
                next_char_boundary(offset, buffer)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_two_cursors_keeps_both() {
        let buf = TextBuffer::new("hello world");
        let mut set = CursorSet::single(0);
        set.add_cursor(6, &buf);
        assert_eq!(set.len(), 2, "two distinct carets are kept");
        assert_eq!(set.cursors()[0].head, 0);
        assert_eq!(set.cursors()[1].head, 6);
    }

    #[test]
    fn add_duplicate_cursor_is_deduped() {
        let buf = TextBuffer::new("hello");
        let mut set = CursorSet::single(2);
        set.add_cursor(2, &buf); // same offset
        assert_eq!(set.len(), 1, "duplicate caret offset collapses to one");
    }

    #[test]
    fn overlapping_selections_merge() {
        let buf = TextBuffer::new("abcdefghij");
        let mut set = CursorSet::default();
        set.set_cursors(vec![Cursor::selection(0, 5), Cursor::selection(3, 8)], &buf);
        assert_eq!(set.len(), 1, "overlapping selections merge into one");
        assert_eq!(set.cursors()[0].range(), 0..8, "merged range is the union");
    }

    #[test]
    fn delete_forward_at_eof_is_a_noop() {
        // The EOF data-corruption bug fixed per adversarial review: a bare caret AT end-of-buffer must
        // delete NOTHING (VS Code Delete-at-EOF), never eat the preceding char like Backspace.
        let mut buf = TextBuffer::new("abc");
        let mut set = CursorSet::single(3); // caret at len_bytes() (EOF)
        let applied = set.delete_forward_at_all(&mut buf);
        assert_eq!(applied, 0, "Delete at EOF applies no deletion");
        assert_eq!(
            buf.to_string(),
            "abc",
            "Delete at EOF leaves the buffer unchanged"
        );
        assert_eq!(set.primary().head, 3, "the caret stays at EOF");
    }

    #[test]
    fn delete_forward_mid_buffer_deletes_the_char_after() {
        // Mid-buffer: Delete removes the char AFTER the caret (forward), not before.
        let mut buf = TextBuffer::new("abc");
        let mut set = CursorSet::single(1); // between 'a' and 'b'
        let applied = set.delete_forward_at_all(&mut buf);
        assert_eq!(applied, 1, "one char deleted");
        assert_eq!(
            buf.to_string(),
            "ac",
            "the char AFTER the caret ('b') was removed"
        );
        assert_eq!(
            set.primary().head,
            1,
            "the caret stays at the deletion point"
        );
    }

    #[test]
    fn delete_forward_deletes_a_selection() {
        let mut buf = TextBuffer::new("abcdef");
        let mut set = CursorSet::default();
        set.set_cursors(vec![Cursor::selection(1, 4)], &buf); // selects "bcd"
        let applied = set.delete_forward_at_all(&mut buf);
        assert_eq!(applied, 1);
        assert_eq!(
            buf.to_string(),
            "aef",
            "Delete with a selection removes the selection"
        );
    }

    #[test]
    fn delete_forward_multi_cursor_eof_caret_only_noops_itself() {
        // Two carets: one mid-buffer, one AT EOF. The mid-buffer caret deletes forward; the EOF caret is
        // a no-op and does NOT corrupt the buffer by eating a preceding char.
        let mut buf = TextBuffer::new("abcdef");
        let mut set = CursorSet::default();
        set.set_cursors(vec![Cursor::caret(1), Cursor::caret(6)], &buf); // mid + EOF
        let applied = set.delete_forward_at_all(&mut buf);
        assert_eq!(
            applied, 1,
            "only the mid-buffer caret deletes; the EOF caret no-ops"
        );
        assert_eq!(
            buf.to_string(),
            "acdef",
            "only the char after the mid caret ('b') was removed"
        );
    }

    #[test]
    fn move_offset_is_char_boundary_safe_on_non_ascii() {
        // "héllo": h(0) é(1..3) l(3) l(4) o(5)
        let buf = TextBuffer::new("héllo");
        let mut set = CursorSet::single(1); // at start of 'é'
        set.move_all(MoveDir::Right, &buf);
        // Right of 'é' is byte 3 (the 'l'), never byte 2 (mid-char).
        assert_eq!(set.primary().head, 3);
    }

    // ── MT-077: grapheme-cluster caret movement + delete (AC3/AC4) ─────────────────────────────────

    #[test]
    fn caret_right_crosses_family_emoji_whole() {
        // AC3 (MANDATORY): a family ZWJ emoji (man+woman+girl ≈ 7 codepoints, 17 bytes) is ONE cluster;
        // RIGHT must cross all of it in ONE keypress, never landing inside.
        let family = "👨‍👩‍👧";
        let buf = TextBuffer::new(family);
        let mut set = CursorSet::single(0);
        set.move_all(MoveDir::Right, &buf);
        assert_eq!(
            set.primary().head,
            family.len(),
            "RIGHT crosses the whole family emoji ({} bytes)",
            family.len()
        );
        assert!(
            family.len() > 7,
            "sanity: the family emoji is multi-codepoint"
        );
    }

    #[test]
    fn caret_left_crosses_combining_accent_whole() {
        // AC3: "e"+U+0301 is one cluster; from after it, LEFT lands at 0, not between the base and mark.
        let s = "e\u{0301}"; // é as base + combining mark, 3 bytes
        let buf = TextBuffer::new(s);
        let mut set = CursorSet::single(s.len());
        set.move_all(MoveDir::Left, &buf);
        assert_eq!(
            set.primary().head,
            0,
            "LEFT crosses the whole combining sequence"
        );
    }

    #[test]
    fn backspace_deletes_whole_family_emoji() {
        // AC4 (MANDATORY): Backspace over a family emoji removes ALL its codepoints, leaving "ab".
        let family = "👨‍👩‍👧";
        let mut buf = TextBuffer::new(&format!("a{family}b"));
        // Caret after the family emoji: a(1) + family(len).
        let caret = 1 + family.len();
        let mut set = CursorSet::single(caret);
        let applied = set.delete_at_all(&mut buf);
        assert_eq!(applied, 1);
        assert_eq!(
            buf.to_string(),
            "ab",
            "the whole family emoji is deleted as one grapheme"
        );
        assert_eq!(
            set.primary().head,
            1,
            "caret lands where the cluster started"
        );
    }

    #[test]
    fn forward_delete_removes_whole_flag() {
        // AC4: forward Delete over a flag (two regional indicators) removes the whole cluster.
        let flag = "🇯🇵";
        let mut buf = TextBuffer::new(&format!("{flag}x"));
        let mut set = CursorSet::single(0);
        let applied = set.delete_forward_at_all(&mut buf);
        assert_eq!(applied, 1);
        assert_eq!(
            buf.to_string(),
            "x",
            "the whole flag is deleted as one grapheme"
        );
    }

    #[test]
    fn prev_grapheme_window_edge_aligned_on_non_ascii() {
        // Regression: when the local window start lands MID multi-byte char, the win_start variable must
        // be snapped to match the materialized window's first byte, else prev_grapheme maps the local
        // boundary back to a wrong absolute offset. Build a buffer of multi-byte chars long enough that
        // the caret's window start (caret - 256) lands inside a 3-byte char, then Backspace must remove
        // exactly the ONE preceding char (each CJK char here is its own grapheme cluster), not tear it.
        let cjk = "中".repeat(200); // 200 * 3 = 600 bytes; window (256) starts mid-char
        let mut buf = TextBuffer::new(&cjk);
        let caret = buf.len_bytes(); // at EOF, after the last 中
        let mut set = CursorSet::single(caret);
        let applied = set.delete_at_all(&mut buf);
        assert_eq!(applied, 1, "exactly one CJK grapheme removed");
        assert_eq!(
            buf.to_string(),
            "中".repeat(199),
            "Backspace removed exactly the last 中, not a torn byte"
        );
        assert_eq!(set.primary().head, buf.len_bytes(), "caret at the new EOF");
    }

    #[test]
    fn ascii_caret_and_delete_unchanged_no_regression() {
        // AC7: ASCII still moves/deletes one char at a time (each ASCII char is its own cluster).
        let mut buf = TextBuffer::new("abc");
        let mut set = CursorSet::single(0);
        set.move_all(MoveDir::Right, &buf);
        assert_eq!(set.primary().head, 1, "RIGHT over ASCII is one byte");
        let mut set2 = CursorSet::single(2);
        set2.delete_at_all(&mut buf);
        assert_eq!(buf.to_string(), "ac", "Backspace removes one ASCII char");
    }

    #[test]
    fn clamp_snaps_out_of_range_and_midchar() {
        let buf = TextBuffer::new("héllo"); // 6 bytes
        let mut set = CursorSet::default();
        set.set_cursors(vec![Cursor::caret(99), Cursor::caret(2)], &buf);
        // 99 clamps to 6 (end); 2 (mid-'é') snaps to 1.
        let heads: Vec<usize> = set.cursors().iter().map(|c| c.head).collect();
        assert!(
            heads.contains(&1),
            "mid-char offset snapped to 1; got {heads:?}"
        );
        assert!(
            heads.contains(&6),
            "out-of-range offset clamped to 6; got {heads:?}"
        );
        assert!(
            heads.iter().all(|&h| h <= 6),
            "no offset past len; got {heads:?}"
        );
    }

    #[test]
    fn word_at_finds_identifier() {
        let buf = TextBuffer::new("let foo_bar = 1;");
        // offset 4 is inside "foo_bar".
        assert_eq!(word_at(4, &buf), 4..11);
        // offset 0 is inside "let".
        assert_eq!(word_at(0, &buf), 0..3);
        // offset 3 is the space between "let" and "foo_bar"; caret just-after "let" anchors on "let".
        assert_eq!(word_at(3, &buf), 0..3);
    }

    #[test]
    fn find_next_occurrence_wraps() {
        let buf = TextBuffer::new("foo bar foo baz");
        // From after the first "foo" (offset 3), next is at 8..11.
        assert_eq!(find_next_occurrence("foo", 3, &buf), Some(8..11));
        // From after the second "foo" (offset 11), it wraps to the first at 0..3.
        assert_eq!(find_next_occurrence("foo", 11, &buf), Some(0..3));
        // A needle that does not exist returns None (no loop).
        assert_eq!(find_next_occurrence("zzz", 0, &buf), None);
        // Empty needle returns None.
        assert_eq!(find_next_occurrence("", 0, &buf), None);
    }

    #[test]
    fn line_col_clamps_column_to_short_line() {
        // line 0 "ab" (len 2), line 1 "wxyz" (len 4).
        let buf = TextBuffer::new("ab\nwxyz");
        // column 10 on line 0 clamps to the end of "ab" (byte 2).
        assert_eq!(line_col_to_byte(0, 10, &buf), 2);
        // column 2 on line 1 is byte 3 + 2 = 5 ("y" position).
        assert_eq!(line_col_to_byte(1, 2, &buf), 5);
    }

    /// Regression (must-fix, adversarial review): the whole-line selection paint branch calls
    /// `line_col_to_byte(line, usize::MAX, &buffer)` to mean "snap to this line's content end". On any
    /// line past the first, `start_char > 0`, so the old `start_char + usize::MAX` overflowed `usize`
    /// and PANICKED under debug overflow-checks (or wrapped to garbage in release). With the saturating
    /// add it must clamp to the line's content end on EVERY line without panicking.
    #[test]
    fn line_col_max_column_clamps_to_content_end_on_every_line() {
        // 3 lines: "0123" (4), "5678" (4), "abcd" (4). Each line content is 4 chars wide.
        let buf = TextBuffer::new("0123\n5678\nabcd");
        // Line 0 content end = byte 4 (before '\n').
        assert_eq!(line_col_to_byte(0, usize::MAX, &buf), 4);
        // Line 1 starts at byte 5 ("5678"); content end = byte 9 (before its '\n'). This is the row that
        // overflowed before (line_start > 0 => start_char > 0).
        assert_eq!(line_col_to_byte(1, usize::MAX, &buf), 9);
        // Line 2 (last, no trailing '\n') starts at byte 10; content end = buffer end (byte 14).
        assert_eq!(line_col_to_byte(2, usize::MAX, &buf), 14);
    }
}
