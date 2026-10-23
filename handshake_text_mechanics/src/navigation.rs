//! Buffer positions shared by text navigation and jump history.

use crate::cursor::Cursor;

/// A `(line, column)` position in the buffer, both 0-based. The traversal coordinate space. The editor's
/// live caret is a byte offset; the panel converts at the boundary (`byte_to_line_col` /
/// `line_col_to_byte`) so this module stays a pure, egui-free, trivially-testable comparator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferPosition {
    /// 0-based line.
    pub line: usize,
    /// 0-based column (chars from the line start). Always 0 for a whole-line diagnostic marker.
    pub column: usize,
}

impl BufferPosition {
    /// A position at `(line, column)`.
    pub fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }

    /// The `(line, column)` of a byte-offset caret head, computed against `buffer`. The bridge from the
    /// editor's byte-offset [`Cursor`] to traversal space. `column` is the number of chars from the
    /// line start (the same convention [`crate::cursor::byte_to_line_col`] uses).
    pub fn from_cursor(cursor: Cursor, buffer: &crate::buffer::TextBuffer) -> Self {
        let (line, column) = crate::cursor::byte_to_line_col(cursor.head, buffer);
        Self { line, column }
    }

}
