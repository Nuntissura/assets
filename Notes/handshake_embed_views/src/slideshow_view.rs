//! Slideshow embed renderer (WP-KERNEL-012 MT-014).
//!
//! A `slideshow` embed shows ONE image at a time with prev/next navigation that WRAPS
//! (after the last image, next returns to the first — AC-5). The current index lives in
//! [`SlideshowViewState`], stored per embed node in `RichEditorState` so it persists across
//! frames. The navigation arithmetic ([`SlideshowViewState::next`] / [`prev`]) is pure and
//! fully unit-testable with NO backend and NO GPU (AC-5 is proven with mock resolution states).
//!
//! AccessKit author_ids (the AC-8 contract ids):
//!   - container: `slideshow-{asset_first_or_refvalue}` (the sequence container)
//!   - prev button: `slideshow-prev-{asset_id}`
//!   - next button: `slideshow-next-{asset_id}`

/// Per-embed slideshow view state: the index of the currently-shown member. Stored per embed
/// node (keyed by the embed's ref_value) in `RichEditorState`, so paging is remembered across
/// frames. `len` is captured so [`Self::next`] / [`Self::prev`] wrap correctly even as the
/// resolved sequence settles (a still-resolving member does not change the index space).
#[derive(Debug, Clone, Default)]
pub struct SlideshowViewState {
    /// The index of the currently displayed member (always `< len` once `len > 0`).
    pub current_index: usize,
}

impl SlideshowViewState {
    /// A fresh state pointing at the first member.
    pub fn new() -> Self {
        Self { current_index: 0 }
    }

    /// Advance to the next member, WRAPPING from the last back to the first (AC-5). A `len` of
    /// 0 leaves the index at 0 (an empty sequence is a typed error elsewhere). The index is
    /// clamped into range first, so a shrunk sequence never points out of bounds.
    pub fn next(&mut self, len: usize) {
        if len == 0 {
            self.current_index = 0;
            return;
        }
        let clamped = self.current_index.min(len - 1);
        self.current_index = (clamped + 1) % len;
    }

    /// Go to the previous member, WRAPPING from the first back to the last. A `len` of 0 leaves
    /// the index at 0.
    pub fn prev(&mut self, len: usize) {
        if len == 0 {
            self.current_index = 0;
            return;
        }
        let clamped = self.current_index.min(len - 1);
        self.current_index = (clamped + len - 1) % len;
    }

    /// The current index clamped into `0..len` (so a render after the sequence shrank is safe).
    pub fn clamped_index(&self, len: usize) -> usize {
        if len == 0 {
            0
        } else {
            self.current_index.min(len - 1)
        }
    }
}

/// The AccessKit author_id for the slideshow container, derived from the embed's ref value.
pub fn container_author_id(ref_value: &str) -> String {
    format!("slideshow-{}", first_asset_token(ref_value))
}

/// The AccessKit author_id for the prev button (AC-8: `slideshow-prev-{asset_id}`).
pub fn prev_author_id(asset_token: &str) -> String {
    format!("slideshow-prev-{asset_token}")
}

/// The AccessKit author_id for the next button (AC-8: `slideshow-next-{asset_id}`).
pub fn next_author_id(asset_token: &str) -> String {
    format!("slideshow-next-{asset_token}")
}

/// The first comma-separated token of a ref value (the representative asset id used to anchor
/// the container/button author_ids). Trims whitespace; returns the whole trimmed value when
/// there is no comma.
pub fn first_asset_token(ref_value: &str) -> String {
    ref_value
        .split(',')
        .next()
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_wraps_after_last_ac5() {
        // AC-5: 3 images; next cycles 0->1->2->0 (wraps after the last to the first).
        let mut s = SlideshowViewState::new();
        assert_eq!(s.current_index, 0);
        s.next(3);
        assert_eq!(s.current_index, 1);
        s.next(3);
        assert_eq!(s.current_index, 2);
        s.next(3);
        assert_eq!(
            s.current_index, 0,
            "AC-5: after the last image, next wraps to the first"
        );
    }

    #[test]
    fn prev_wraps_before_first() {
        let mut s = SlideshowViewState::new();
        s.prev(3);
        assert_eq!(s.current_index, 2, "prev from the first wraps to the last");
        s.prev(3);
        assert_eq!(s.current_index, 1);
    }

    #[test]
    fn empty_or_shrunk_sequence_is_safe() {
        let mut s = SlideshowViewState { current_index: 9 };
        s.next(0);
        assert_eq!(s.current_index, 0, "empty sequence -> index 0, no panic");
        // A stale large index over a 2-element sequence clamps before advancing.
        let mut s = SlideshowViewState { current_index: 9 };
        s.next(2);
        // clamped 9->1, then +1 % 2 = 0
        assert_eq!(s.current_index, 0);
        assert_eq!(SlideshowViewState { current_index: 9 }.clamped_index(2), 1);
        assert_eq!(SlideshowViewState { current_index: 9 }.clamped_index(0), 0);
    }

    #[test]
    fn author_ids_match_ac8_contract() {
        assert_eq!(prev_author_id("a1"), "slideshow-prev-a1");
        assert_eq!(next_author_id("a1"), "slideshow-next-a1");
        assert_eq!(container_author_id("a1, a2, a3"), "slideshow-a1");
        assert_eq!(first_asset_token(" a1 , a2 "), "a1");
    }
}
