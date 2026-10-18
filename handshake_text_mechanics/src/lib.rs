//! Shared text buffer, cursor, line-edit, virtualization and Unicode mechanics.

pub mod buffer;
pub mod cursor;
pub mod line_ops;
pub mod virtual_lines;
pub mod text_intl;
pub mod render_decorations;
pub mod word_wrap;

pub mod formatting;
pub mod navigation;
pub mod jump_history;
