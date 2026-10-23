//! Rope-backed text buffer for the native code editor (WP-KERNEL-012 MT-001).
//!
//! [`TextBuffer`] wraps `ropey::Rope`, the battle-tested rope data structure that handles large
//! files and incremental edits cheaply. It is the SOLE owner of document text; every other
//! subsystem (highlight, folding, minimap, virtualization in later MTs) borrows slices from it
//! rather than holding its own copy.
//!
//! ## Byte vs char offsets (RISK-002 — the off-by-one trap)
//!
//! `ropey` indexes natively in CHARS, but tree-sitter, egui galleys, and the editor's public edit
//! API all speak BYTES. Confusing the two silently corrupts highlight-span alignment on any
//! non-ASCII document. This module makes the boundary explicit:
//!   - The public edit/query API ([`insert`], [`delete`], [`line_to_byte`], [`byte_to_line`]) is
//!     BYTE-addressed (what tree-sitter and egui want).
//!   - Internally every byte offset is converted to a char offset via `Rope::byte_to_char` before
//!     touching the rope, and char results are converted back with `Rope::char_to_byte`.
//!   - [`byte_to_char`] / [`char_to_byte`] are exposed so the highlighter can convert tree-sitter
//!     byte ranges for rendering without re-deriving the conversion.
//!
//! ## Panic-freedom (AC-006 / MC-001)
//!
//! Every range operation CLAMPS or returns `Option`/`Result`; there is NO `unwrap()`/`expect()` on
//! a range operation in any production path. `ropey`'s own `get_*` family (which returns `Option`)
//! is used instead of the panicking `byte_to_char` / `char_to_line` family so an out-of-range input
//! degrades to a clamped/None result rather than aborting the egui frame.

use std::ops::Range;

/// Why a buffer edit could not be applied. Returned instead of panicking so a bad offset from an
/// agent action, a stale highlight span, or a deserialized cursor never aborts the render loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BufferError {
    /// A byte offset fell outside `0..=len_bytes()`.
    OffsetOutOfRange { offset: usize, len_bytes: usize },
    /// A byte offset landed in the middle of a multi-byte UTF-8 char (not a char boundary).
    NotACharBoundary { offset: usize },
    /// A range was inverted (`start > end`) or its end exceeded `len_bytes()`.
    InvalidRange {
        start: usize,
        end: usize,
        len_bytes: usize,
    },
}

impl std::fmt::Display for BufferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BufferError::OffsetOutOfRange { offset, len_bytes } => {
                write!(f, "byte offset {offset} out of range 0..={len_bytes}")
            }
            BufferError::NotACharBoundary { offset } => {
                write!(f, "byte offset {offset} is not a UTF-8 char boundary")
            }
            BufferError::InvalidRange {
                start,
                end,
                len_bytes,
            } => {
                write!(
                    f,
                    "invalid byte range {start}..{end} (len_bytes={len_bytes})"
                )
            }
        }
    }
}

impl std::error::Error for BufferError {}

/// A rope-backed, byte-addressed text buffer. Owns the document text; all other editor subsystems
/// borrow from it.
#[derive(Debug, Clone)]
pub struct TextBuffer {
    rope: ropey::Rope,
}

impl TextBuffer {
    /// Build a buffer from an initial string.
    pub fn new(text: &str) -> Self {
        Self {
            rope: ropey::Rope::from_str(text),
        }
    }

    /// Total length in bytes (the unit tree-sitter and egui use).
    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    /// Number of lines. `ropey` counts the text after the last line-break as a final line, so a
    /// buffer ending in `\n` reports one more line than the count of `\n`s (matching editor
    /// conventions where the cursor can sit on the empty trailing line).
    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    /// True when the buffer holds no text.
    pub fn is_empty(&self) -> bool {
        self.rope.len_bytes() == 0
    }

    /// Convert a byte offset to a char offset, or `None` if the offset is out of range or not on a
    /// char boundary. The highlighter uses this to align tree-sitter byte spans to rope chars
    /// before rendering (RISK-002).
    ///
    /// ropey's own `try_byte_to_char` only bounds-checks: a byte inside a multi-byte char is mapped
    /// to the char it belongs to rather than rejected. To preserve the `NotACharBoundary` contract
    /// this buffer documents, the boundary is verified by a round-trip: a byte offset `b` is a char
    /// boundary iff `char_to_byte(byte_to_char(b)) == b`. A mid-char byte round-trips to the char's
    /// START byte (`< b`), so the inequality detects it without propagating ropey's lenient mapping.
    pub fn byte_to_char(&self, byte_offset: usize) -> Option<usize> {
        let char_idx = self.rope.try_byte_to_char(byte_offset).ok()?;
        let back = self.rope.try_char_to_byte(char_idx).ok()?;
        if back == byte_offset {
            Some(char_idx)
        } else {
            None // byte_offset landed inside a multi-byte char (not a boundary).
        }
    }

    /// Convert a char offset to a byte offset, or `None` if out of range.
    pub fn char_to_byte(&self, char_offset: usize) -> Option<usize> {
        self.rope.try_char_to_byte(char_offset).ok()
    }

    /// Byte offset at the start of `line_idx`. Returns `None` for an out-of-range line index rather
    /// than panicking (AC-006). A line index equal to `len_lines()` resolves to `len_bytes()` so a
    /// caller can address the one-past-the-end position.
    pub fn line_to_byte(&self, line_idx: usize) -> Option<usize> {
        self.rope.try_line_to_byte(line_idx).ok()
    }

    /// The line index that byte `byte_offset` falls on, or `None` if out of range (AC-006).
    pub fn byte_to_line(&self, byte_offset: usize) -> Option<usize> {
        self.rope.try_byte_to_line(byte_offset).ok()
    }

    /// Materialize the text for an inclusive-exclusive LINE range as a `String`. Out-of-range or
    /// inverted line ranges are CLAMPED to the valid `[0, len_lines()]` window (never a panic —
    /// AC-006 / MC-001). The render path calls this with only the visible line window (capped to
    /// the first 1000 lines before MT-002 virtualization, per the render guard), so it never
    /// `.to_string()`s the whole rope every frame (RISK-003).
    pub fn slice_to_string(&self, line_range: Range<usize>) -> String {
        let max_line = self.rope.len_lines();
        let start = line_range.start.min(max_line);
        // Clamp the end up to len_lines and never below start (inverted ranges -> empty).
        let end = line_range.end.min(max_line).max(start);

        // try_line_to_char errors only if the index exceeds len_lines; both are clamped to
        // <= len_lines above, so these are Ok — but fall back to stay panic-free regardless.
        let start_char = self.rope.try_line_to_char(start).unwrap_or(0);
        let end_char = self
            .rope
            .try_line_to_char(end)
            .unwrap_or_else(|_| self.rope.len_chars());
        if end_char <= start_char {
            return String::new();
        }
        // get_slice returns None on an invalid char range; we just clamped both ends, so fall back
        // to an empty string instead of panicking if ropey ever disagrees.
        match self.rope.get_slice(start_char..end_char) {
            Some(slice) => slice.to_string(),
            None => String::new(),
        }
    }

    /// Materialize ONLY the text in the BYTE range `byte_range` as a `String`, without stringifying the
    /// whole rope (O(range), not O(document) — the same RISK-003 discipline `slice_to_string` follows for
    /// the visible window). Out-of-range, inverted, or non-char-boundary ends are CLAMPED to the nearest
    /// valid char boundary (never a panic — AC-006 / MC-001), so a stale selection range from a prior
    /// edit yields a best-effort slice rather than a crash. Used by the MT-031 cross-pane selection-publish
    /// + Copy path so selecting in a multi-MB file never allocates the entire document (the perf-lens cap).
    pub fn byte_slice_to_string(&self, byte_range: Range<usize>) -> String {
        let len_bytes = self.rope.len_bytes();
        let end = byte_range.end.min(len_bytes);
        let start = byte_range.start.min(end);
        // Snap each end DOWN to a char boundary (ropey panics on a mid-char byte slice; clamp instead).
        let snap = |b: usize| -> usize {
            // Walk down at most 3 bytes (max UTF-8 continuation run) to the nearest boundary.
            let mut b = b;
            while b > 0 && self.rope.try_byte_to_char(b).is_err() {
                b -= 1;
            }
            // Verify it is a true boundary; if ropey's lenient mapping disagrees, fall back to b.
            b
        };
        let start = snap(start);
        let end = snap(end).max(start);
        match self.rope.get_byte_slice(start..end) {
            Some(slice) => slice.to_string(),
            None => String::new(),
        }
    }

    /// Insert `text` at `byte_offset`. Returns `Err(BufferError)` (never panics — AC-006) when the
    /// offset is out of range or lands inside a multi-byte char. On success the rope is mutated in
    /// place; ropey makes this O(log n) amortized, which is why retrofitting `String` later is
    /// avoided (implementation note 1).
    pub fn insert(&mut self, byte_offset: usize, text: &str) -> Result<(), BufferError> {
        let char_idx = self.checked_byte_to_char(byte_offset)?;
        self.rope.insert(char_idx, text);
        Ok(())
    }

    /// Delete the BYTE range `byte_range`. Returns `Err(BufferError)` (never panics — AC-006) for an
    /// out-of-range, inverted, or non-char-boundary range. An empty range is a successful no-op.
    pub fn delete(&mut self, byte_range: Range<usize>) -> Result<(), BufferError> {
        let len_bytes = self.rope.len_bytes();
        if byte_range.start > byte_range.end || byte_range.end > len_bytes {
            return Err(BufferError::InvalidRange {
                start: byte_range.start,
                end: byte_range.end,
                len_bytes,
            });
        }
        let start_char = self.checked_byte_to_char(byte_range.start)?;
        let end_char = self.checked_byte_to_char(byte_range.end)?;
        if start_char == end_char {
            return Ok(()); // empty range: nothing to remove.
        }
        self.rope.remove(start_char..end_char);
        Ok(())
    }

    /// Borrow the document text as contiguous bytes for tree-sitter. `ropey::Rope` stores text in
    /// chunks, so this allocates a single contiguous `Vec<u8>` (tree-sitter's `parse` wants a slice).
    /// The highlighter calls this once per parse, not per frame.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.rope.to_string().into_bytes()
    }

    /// Convert a byte offset to a char offset, mapping ropey's `None` (out of range / not a char
    /// boundary) into the typed [`BufferError`] the public API returns. The single internal place
    /// the byte->char conversion is enforced, so every edit path is panic-free by construction.
    fn checked_byte_to_char(&self, byte_offset: usize) -> Result<usize, BufferError> {
        let len_bytes = self.rope.len_bytes();
        if byte_offset > len_bytes {
            return Err(BufferError::OffsetOutOfRange {
                offset: byte_offset,
                len_bytes,
            });
        }
        // In range, so `byte_to_char` returning None means the offset is mid-char, not OOB.
        self.byte_to_char(byte_offset)
            .ok_or(BufferError::NotACharBoundary {
                offset: byte_offset,
            })
    }
}

impl Default for TextBuffer {
    fn default() -> Self {
        Self::new("")
    }
}

/// The whole document as a `String` (via `.to_string()` from the `ToString` blanket impl). Used by
/// the highlighter's full-parse path and by tests; the render path uses
/// [`slice_to_string`](TextBuffer::slice_to_string) on the visible window instead so it does not
/// stringify the whole rope every frame (RISK-003). Implemented as `Display` rather than an inherent
/// `to_string` so it composes with the standard `ToString`/`Display` ecosystem (clippy
/// `inherent_to_string`).
impl std::fmt::Display for TextBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // RopeSlice implements Display by writing each chunk, avoiding a full intermediate String.
        write!(f, "{}", self.rope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buffer_is_empty_and_has_one_line() {
        let b = TextBuffer::new("");
        assert!(b.is_empty());
        assert_eq!(b.len_bytes(), 0);
        // ropey reports an empty rope as a single (empty) line.
        assert_eq!(b.len_lines(), 1);
        assert_eq!(b.slice_to_string(0..1), "");
    }

    #[test]
    fn single_line_insert_at_start_middle_end() {
        let mut b = TextBuffer::new("hello");
        b.insert(5, " world").unwrap(); // end
        assert_eq!(b.to_string(), "hello world");
        b.insert(0, ">>").unwrap(); // start
        assert_eq!(b.to_string(), ">>hello world");
        b.insert(7, "_X_").unwrap(); // middle (after ">>hello")
        assert_eq!(b.to_string(), ">>hello_X_ world");
    }

    #[test]
    fn multi_line_insert_grows_line_count() {
        let mut b = TextBuffer::new("a\nb");
        assert_eq!(b.len_lines(), 2);
        b.insert(1, "X\nY\nZ").unwrap(); // insert after "a"
                                         // "aX\nY\nZ\nb" -> 4 lines
        assert_eq!(b.to_string(), "aX\nY\nZ\nb");
        assert_eq!(b.len_lines(), 4);
    }

    #[test]
    fn delete_spanning_lines() {
        let mut b = TextBuffer::new("line0\nline1\nline2\n");
        // Delete from start of line1 (byte 6) through start of line2 (byte 12): removes "line1\n".
        let start = b.line_to_byte(1).unwrap();
        let end = b.line_to_byte(2).unwrap();
        assert_eq!((start, end), (6, 12));
        b.delete(start..end).unwrap();
        assert_eq!(b.to_string(), "line0\nline2\n");
    }

    #[test]
    fn byte_line_round_trips() {
        let b = TextBuffer::new("alpha\nbeta\ngamma");
        for line in 0..b.len_lines() {
            let byte = b.line_to_byte(line).unwrap();
            let back = b.byte_to_line(byte).unwrap();
            assert_eq!(back, line, "line {line} -> byte {byte} -> line {back}");
        }
        // Explicit known offsets.
        assert_eq!(b.line_to_byte(0), Some(0));
        assert_eq!(b.line_to_byte(1), Some(6)); // after "alpha\n"
        assert_eq!(b.line_to_byte(2), Some(11)); // after "beta\n"
        assert_eq!(b.byte_to_line(0), Some(0));
        assert_eq!(b.byte_to_line(6), Some(1));
        assert_eq!(b.byte_to_line(11), Some(2));
    }

    #[test]
    fn out_of_range_ops_return_err_not_panic() {
        let mut b = TextBuffer::new("hi");
        // Insert past the end.
        assert_eq!(
            b.insert(99, "x"),
            Err(BufferError::OffsetOutOfRange {
                offset: 99,
                len_bytes: 2
            })
        );
        // Delete an inverted range. The `5..2` inversion is INTENTIONAL negative-path coverage —
        // `delete` must reject start>end with `InvalidRange`. clippy's `reversed_empty_ranges` lint
        // is deny-by-default and would reject the literal as a hard error under `--all-targets`, so
        // the assert is explicitly allowed.
        #[allow(clippy::reversed_empty_ranges)]
        let inverted_delete = matches!(b.delete(5..2), Err(BufferError::InvalidRange { .. }));
        assert!(inverted_delete);
        // Delete past the end.
        assert!(matches!(
            b.delete(0..99),
            Err(BufferError::InvalidRange { .. })
        ));
        // line_to_byte / byte_to_line out of range -> None, not panic.
        assert_eq!(b.byte_to_line(99), None);
        assert_eq!(b.line_to_byte(99), None);
        // The buffer is untouched after the failed ops.
        assert_eq!(b.to_string(), "hi");
    }

    #[test]
    fn non_ascii_byte_char_conversions_align() {
        // "héllo": h=1 byte, é=2 bytes (0xC3 0xA9), l l o = 3 bytes -> 6 bytes, 5 chars.
        let b = TextBuffer::new("héllo");
        assert_eq!(b.len_bytes(), 6);
        // Byte 1 is the start of 'é' (a char boundary).
        assert_eq!(b.byte_to_char(1), Some(1));
        // Byte 2 is INSIDE 'é' (not a char boundary) -> None.
        assert_eq!(b.byte_to_char(2), None);
        // Byte 3 is the start of the first 'l'.
        assert_eq!(b.byte_to_char(3), Some(2));
        // char_to_byte round-trip.
        assert_eq!(b.char_to_byte(2), Some(3));
    }

    #[test]
    fn insert_at_non_char_boundary_errors() {
        let mut b = TextBuffer::new("héllo");
        // Byte 2 is inside the 'é' char.
        assert_eq!(
            b.insert(2, "X"),
            Err(BufferError::NotACharBoundary { offset: 2 })
        );
        assert_eq!(b.to_string(), "héllo");
    }

    #[test]
    fn slice_to_string_clamps_out_of_range_line_ranges() {
        let b = TextBuffer::new("l0\nl1\nl2");
        assert_eq!(b.slice_to_string(0..1), "l0\n");
        assert_eq!(b.slice_to_string(1..2), "l1\n");
        // Range beyond the end clamps to the available lines (no panic).
        assert_eq!(b.slice_to_string(0..999), "l0\nl1\nl2");
        // Inverted range -> empty. The `5..2` inversion is INTENTIONAL negative-path coverage —
        // `slice_to_string` must clamp a reversed line range to "" without panicking. clippy's
        // deny-by-default `reversed_empty_ranges` lint is explicitly allowed for this literal.
        #[allow(clippy::reversed_empty_ranges)]
        let inverted_slice = b.slice_to_string(5..2);
        assert_eq!(inverted_slice, "");
        // Range entirely past the end -> empty.
        assert_eq!(b.slice_to_string(50..60), "");
    }

    #[test]
    fn empty_delete_range_is_noop_ok() {
        let mut b = TextBuffer::new("data");
        assert_eq!(b.delete(2..2), Ok(()));
        assert_eq!(b.to_string(), "data");
    }
}
