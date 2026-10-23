//! Char-addressed rope wrapper for inline text leaves (WP-KERNEL-012 MT-011).
//!
//! [`RopeText`] wraps `ropey::Rope` and exposes ONLY char-indexed operations
//! (`insert` / `remove` / `char_at` / `slice`). This is the deliberate antidote to
//! red-team RISK-1 (byte-vs-char confusion silently corrupts CJK/emoji text): the
//! type makes a byte index unrepresentable in its public API, so a downstream caller
//! cannot accidentally address a multi-byte char by its byte offset.
//!
//! `ropey` itself indexes in chars, so every method here is a thin, panic-free
//! forwarding to the rope's char API. Out-of-range indices CLAMP (insert/remove) or
//! return `None` (`char_at`) rather than panicking, so a stale position from a
//! transaction step never aborts the render loop.

use std::fmt;

/// A rope-backed run of text addressed purely by CHAR index. The unit everywhere in
/// this type is the Unicode scalar value (`char`), never a byte.
#[derive(Clone)]
pub struct RopeText {
    rope: ropey::Rope,
}

impl RopeText {
    /// Build from a `&str`. Named `from_str` to mirror `ropey::Rope::from_str` and
    /// the editor's text constructors; this is an inherent constructor, not the
    /// `std::str::FromStr` trait (no fallible parse), so the clippy
    /// `should_implement_trait` lint is allowed here.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        Self {
            rope: ropey::Rope::from_str(s),
        }
    }

    /// Build an empty rope.
    pub fn empty() -> Self {
        Self {
            rope: ropey::Rope::new(),
        }
    }

    /// Length in CHARS (scalar values), not bytes. A 4-byte emoji counts as 1.
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// True when the run holds no text.
    pub fn is_empty(&self) -> bool {
        self.rope.len_chars() == 0
    }

    /// The char at `char_idx`, or `None` if out of range. Uses ropey's `get_char`
    /// (Option-returning) so an out-of-range index never panics.
    pub fn char_at(&self, char_idx: usize) -> Option<char> {
        self.rope.get_char(char_idx)
    }

    /// Materialize the `[start, end)` CHAR range as a `String`. The range is clamped
    /// into `0..=len_chars()` and an inverted range yields `""`, so a stale span
    /// never panics.
    pub fn slice_chars(&self, start: usize, end: usize) -> String {
        let max = self.rope.len_chars();
        let s = start.min(max);
        let e = end.min(max).max(s);
        match self.rope.get_slice(s..e) {
            Some(slice) => slice.to_string(),
            None => String::new(),
        }
    }

    /// Insert `text` at CHAR index `char_idx`. The index is CLAMPED into
    /// `0..=len_chars()` (so an off-the-end insert appends rather than panicking).
    /// Uses `Rope::insert`, which takes a CHAR index — the whole reason this wrapper
    /// exists (RISK-1).
    pub fn insert(&mut self, char_idx: usize, text: &str) {
        let at = char_idx.min(self.rope.len_chars());
        self.rope.insert(at, text);
    }

    /// Remove the `[start, end)` CHAR range and return the removed substring (so the
    /// transform layer can build the inverse `InsertText` step — MT impl note 3:
    /// capture the old content BEFORE the step is applied). The range is clamped and
    /// an inverted range removes nothing.
    pub fn remove(&mut self, start: usize, end: usize) -> String {
        let max = self.rope.len_chars();
        let s = start.min(max);
        let e = end.min(max).max(s);
        if e == s {
            return String::new();
        }
        let removed = self
            .rope
            .get_slice(s..e)
            .map(|sl| sl.to_string())
            .unwrap_or_default();
        self.rope.remove(s..e);
        removed
    }
}

impl Default for RopeText {
    fn default() -> Self {
        Self::empty()
    }
}

impl fmt::Debug for RopeText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Render as the contained string so doc Debug output is legible in test
        // failures (the rope's own Debug dumps chunk internals).
        write!(f, "RopeText({:?})", self.rope.to_string())
    }
}

impl fmt::Display for RopeText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.rope)
    }
}

impl PartialEq for RopeText {
    fn eq(&self, other: &Self) -> bool {
        // Compare by content so two ropes built differently but holding the same
        // text are equal (the undo round-trip + DocJson equality tests rely on this).
        self.rope == other.rope
    }
}

impl Eq for RopeText {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_remove_round_trip() {
        let mut r = RopeText::from_str("hello");
        r.insert(5, " world");
        assert_eq!(r.to_string(), "hello world");
        let removed = r.remove(5, 11);
        assert_eq!(removed, " world");
        assert_eq!(r.to_string(), "hello");
    }

    #[test]
    fn out_of_range_insert_clamps_to_append() {
        let mut r = RopeText::from_str("ab");
        r.insert(999, "Z"); // clamps to end, no panic
        assert_eq!(r.to_string(), "abZ");
    }

    #[test]
    fn inverted_and_oob_remove_is_noop() {
        let mut r = RopeText::from_str("data");
        assert_eq!(r.remove(3, 1), ""); // inverted -> nothing removed
        assert_eq!(r.to_string(), "data");
        assert_eq!(r.remove(50, 60), ""); // entirely past end
        assert_eq!(r.to_string(), "data");
    }
}
