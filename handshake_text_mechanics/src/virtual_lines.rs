//! Viewport-line virtualization calculator for the native code editor (WP-KERNEL-012 MT-002).
//!
//! [`VirtualLineLayout`] is a PURE, GPU-free calculator: given a line count, a per-line height, the
//! viewport height, and the current scroll offset (all in pixels), it answers three questions the
//! panel needs to render only the visible slice of a large document:
//!
//! - [`visible_range`](VirtualLineLayout::visible_range): the inclusive-exclusive `Range<usize>` of
//!   line indices that intersect the viewport, padded by [`OVERSCAN_LINES`] above and below so a
//!   small scroll never reveals an un-painted gap.
//! - [`total_height_px`](VirtualLineLayout::total_height_px): `line_count * line_height_px`, the
//!   virtual content height used to size the `egui::ScrollArea` content rect so the scrollbar thumb
//!   is proportioned to the WHOLE document, not just the painted window.
//! - [`y_for_line`](VirtualLineLayout::y_for_line): the pixel offset of a given line from the content
//!   top, for positioning the painted rows / gutter / cursor overlay in later MTs.
//!
//! ## Why this is a separate, side-effect-free struct
//!
//! `egui::ScrollArea::show_rows` is the idiomatic native virtualization primitive and is what the
//! panel actually drives each frame (RESEARCH-PROVENANCE wf_ffa74d6d 2026-06-22: confirmed for egui
//! 0.33; no custom painter needed for read/highlight virtualization). But `show_rows` computes its
//! visible range INSIDE egui from the live viewport, which cannot be unit-tested headlessly. This
//! struct re-expresses comparable arithmetic as a deterministic value so the boundary math (AC-001:
//! scroll=0, mid-document, end-of-document, total height, monotonic `y_for_line`) is provable without
//! a GPU.
//!
//! ## This calculator is NOT the live render's painted range (AC-007)
//!
//! This struct is the HEADLESS boundary-math calculator and the source of `total_height_px` /
//! `y_for_line` ONLY. It is deliberately NOT what the panel reports as the painted window. egui's
//! `show_rows` selects its row range INSIDE egui from
//! `row_height_with_spacing = line_height + item_spacing.y` and applies **no overscan** (egui 0.33.3
//! `scroll_area.rs:948-963`), whereas this calculator divides by the sans-spacing `line_height_px`
//! and pads by ±[`OVERSCAN_LINES`]. The two therefore DO NOT produce the same range — they differ by
//! the overscan and by the row-height unit. The panel captures egui's own `row_range` for
//! diagnostics / overlay positioning (see `panel.rs`) precisely so the reported window matches the
//! pixels; this calculator must not be substituted for that captured range.
//!
//! ## Overscan
//!
//! [`OVERSCAN_LINES`] = 8 extra lines pad this calculator's [`visible_range`](VirtualLineLayout::visible_range)
//! on EACH side of the strictly-visible window. This is the native analog of the React Monaco view's
//! `IntersectionObserver { rootMargin: '600px' }` overscan (ports_from_react): a generous ready-row
//! buffer for boundary math. NOTE: egui's `show_rows` does its own (overscan-free) range selection on
//! the live render path, so this padding describes the calculator's headless estimate, not the count
//! of rows egui actually paints.

use std::ops::Range;

/// Extra lines rendered above AND below the strictly-visible viewport window so a small scroll never
/// exposes an un-painted gap (the native analog of Monaco's `rootMargin` overscan). Eight lines is a
/// cheap, generous buffer at typical line heights.
pub const OVERSCAN_LINES: usize = 8;

/// A pure calculator for which document lines intersect a scroll viewport, plus the virtual content
/// height and per-line vertical offsets. Holds no egui/GPU state; every method is total and
/// panic-free (a zero or non-finite `line_height_px` is clamped, never divided-by).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VirtualLineLayout {
    line_count: usize,
    line_height_px: f32,
    viewport_height_px: f32,
    scroll_offset_px: f32,
}

impl VirtualLineLayout {
    /// Build a layout from the live measurements.
    ///
    /// - `line_count`: total lines in the document (the WHOLE buffer, not the painted window).
    /// - `line_height_px`: height of one line incl. row spacing, as egui lays it out.
    /// - `viewport_height_px`: the visible scroll-viewport height.
    /// - `scroll_offset_px`: how far the content is scrolled down (0 = top), as read from the
    ///   persisted `egui::ScrollArea` state.
    ///
    /// `line_height_px` is clamped to a tiny positive floor and a non-finite/negative
    /// `viewport_height_px` / `scroll_offset_px` is clamped to 0, so no later division can produce
    /// `NaN`/`inf` or panic regardless of what egui hands in on a degenerate first frame.
    pub fn new(
        line_count: usize,
        line_height_px: f32,
        viewport_height_px: f32,
        scroll_offset_px: f32,
    ) -> Self {
        // A line height of 0 (or NaN/inf/negative) would make `visible_range` divide by zero and
        // produce a garbage range; clamp to a 1px floor so the math is always well-defined. Real
        // line heights are ~13-20px, so this floor never affects a healthy frame.
        let line_height_px = if line_height_px.is_finite() && line_height_px > 0.0 {
            line_height_px
        } else {
            1.0
        };
        let viewport_height_px = if viewport_height_px.is_finite() && viewport_height_px > 0.0 {
            viewport_height_px
        } else {
            0.0
        };
        let scroll_offset_px = if scroll_offset_px.is_finite() && scroll_offset_px > 0.0 {
            scroll_offset_px
        } else {
            0.0
        };
        Self {
            line_count,
            line_height_px,
            viewport_height_px,
            scroll_offset_px,
        }
    }

    /// Total document lines this layout was built for.
    pub fn line_count(&self) -> usize {
        self.line_count
    }

    /// The clamped per-line height in pixels actually used by the layout math.
    pub fn line_height_px(&self) -> f32 {
        self.line_height_px
    }

    /// The half-open range `first..last_exclusive` of line indices to paint: every line that
    /// intersects the viewport, padded by [`OVERSCAN_LINES`] on each side and clamped to
    /// `0..line_count`.
    ///
    /// Formula (matching the MT contract):
    ///   `first = max(0, floor(scroll / lh) - overscan)`
    ///   `last  = min(line_count - 1, ceil((scroll + vp) / lh) + overscan)`  (then +1 for half-open)
    ///
    /// An empty document (`line_count == 0`) yields an empty `0..0` range.
    pub fn visible_range(&self) -> Range<usize> {
        if self.line_count == 0 {
            return 0..0;
        }
        let last_line = self.line_count - 1;

        // floor(scroll / lh) is the first strictly-visible line; saturating_sub keeps `first` >= 0.
        let first_visible = (self.scroll_offset_px / self.line_height_px).floor() as usize;
        let first = first_visible.saturating_sub(OVERSCAN_LINES);

        // ceil((scroll + vp) / lh) is the last strictly-visible line (one past the bottom edge);
        // add the overscan, clamp to the final line index, then +1 for the half-open upper bound.
        let last_visible = ((self.scroll_offset_px + self.viewport_height_px) / self.line_height_px)
            .ceil() as usize;
        let last_inclusive = last_visible.saturating_add(OVERSCAN_LINES).min(last_line);

        // `first` can exceed `last_inclusive` only if `first` was clamped above the document (scroll
        // far past the end with a tiny doc); clamp `first` down so the range is never inverted.
        let first = first.min(last_inclusive);
        first..(last_inclusive + 1)
    }

    /// The virtual content height in pixels: `line_count * line_height_px`. The panel sets the
    /// `egui::ScrollArea` content rect to this so the scrollbar thumb reflects the WHOLE document even
    /// though only [`visible_range`](Self::visible_range) lines are painted.
    pub fn total_height_px(&self) -> f32 {
        self.line_count as f32 * self.line_height_px
    }

    /// Pixel offset of `line` from the content top (`line * line_height_px`). Monotonically increasing
    /// in `line` (AC-001), used to position painted rows / the gutter / the cursor overlay.
    pub fn y_for_line(&self, line: usize) -> f32 {
        line as f32 * self.line_height_px
    }
}

// ── WP-KERNEL-012 MT-078 (E13): per-code-line RTL/bidi awareness ──────────────────────────────────────
//
// The code editor paints one galley per VISIBLE line. A code line that contains Hebrew/Arabic (e.g. an
// RTL string literal or comment) needs the same bidi treatment as a rich paragraph: detect the line's base
// direction and reorder it into visual order before laying it out LTR. This pure helper reuses the SHARED
// `text_intl::bidi` pass (NOT a parallel one) so the code editor and rich editor agree on bidi, and it is
// applied PER VISIBLE LINE (the code editor already virtualizes to a small window, so the bidi cost is
// bounded to the rows on screen). The rope stays logical-order; this is render-time only.

/// How one visible code line should be laid out after the MT-078 bidi pass: the visual-order text to paint,
/// the resolved base direction (RTL ⇒ the renderer right-aligns the line within the text column), and any
/// typed shaping limitation (Arabic/Indic) the gutter/line can surface. Identity for a pure-LTR code line
/// (the overwhelmingly common case): `visual_text == input`, `base == Ltr`, `shaping_limitation == None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeLineBidi {
    /// The line content in VISUAL order, ready to lay out LTR.
    pub visual_text: String,
    /// The line's base direction (RTL ⇒ right-align the line within the code text column).
    pub base: crate::text_intl::Direction,
    /// `Some` when the line carries Arabic/Indic content egui cannot shape (Tier-3 typed limitation).
    pub shaping_limitation: Option<crate::text_intl::ShapingLimitation>,
}

impl CodeLineBidi {
    /// True when this is the IDENTITY (pure-LTR code line): visual == logical, LTR base, no limitation.
    /// A pure-LTR line MUST be identity so the existing code render (the vast majority of source lines) is
    /// byte-for-byte unchanged (AC6 / MC-3 / RISK-2).
    pub fn is_identity(&self, original: &str) -> bool {
        self.base == crate::text_intl::Direction::Ltr
            && self.visual_text == original
            && self.shaping_limitation.is_none()
    }
}

/// Compute the MT-078 bidi treatment for a single code line `text` (reusing the shared `text_intl::bidi`).
/// Pure + GPU-free so the code-editor render path can call it per visible line and so it is unit-testable
/// headlessly. Identity for pure-LTR code (no reordering, left-aligned) — AC6.
pub fn code_line_bidi(text: &str) -> CodeLineBidi {
    let reordered = crate::text_intl::reorder_line(text);
    let shaping_limitation = crate::text_intl::shaping_limitation(text);
    CodeLineBidi {
        visual_text: reordered.visual_text,
        base: reordered.base,
        shaping_limitation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 100k-line buffer at 16px lines in a 800px viewport is the AC-002 perf scenario; reuse the
    // same shape for the range boundary tests so the asserts mirror the real workload.
    const LH: f32 = 16.0;
    const VP: f32 = 800.0;

    #[test]
    fn scroll_at_top_shows_first_window_plus_overscan() {
        let layout = VirtualLineLayout::new(100_000, LH, VP, 0.0);
        let range = layout.visible_range();
        // At scroll 0 the first line is 0 (overscan cannot go below 0).
        assert_eq!(range.start, 0, "top scroll starts at line 0");
        // The viewport holds ceil(800/16) = 50 lines; +overscan (8) + the +1 half-open = ~59.
        let visible_lines = (VP / LH).ceil() as usize; // 50
        assert!(
            range.end >= visible_lines && range.end <= visible_lines + OVERSCAN_LINES + 2,
            "end {} should be ~{} visible + overscan",
            range.end,
            visible_lines
        );
        // It must NOT render the whole 100k document (that is the whole point of virtualization).
        assert!(
            range.len() < 100,
            "only a small window is painted, not all 100k lines (got {})",
            range.len()
        );
    }

    #[test]
    fn scroll_to_middle_returns_centered_window() {
        // Scroll to line 50_000 -> offset = 50_000 * 16.
        let mid_line = 50_000usize;
        let offset = mid_line as f32 * LH;
        let layout = VirtualLineLayout::new(100_000, LH, VP, offset);
        let range = layout.visible_range();
        // The first painted line is (mid - overscan); the strictly-visible first line is `mid_line`.
        assert_eq!(
            range.start,
            mid_line - OVERSCAN_LINES,
            "mid-scroll first line is mid - overscan"
        );
        // The mid line itself is inside the painted window.
        assert!(
            range.contains(&mid_line),
            "the scrolled-to line {mid_line} must be inside {range:?}"
        );
        // Line 0 is far above the window and must NOT be painted.
        assert!(
            !range.contains(&0),
            "line 0 is not painted when scrolled to the middle"
        );
        assert!(
            range.len() < 100,
            "still a small window (got {})",
            range.len()
        );
    }

    #[test]
    fn scroll_to_end_clamps_to_last_line() {
        let count = 100_000usize;
        // Offset past the very bottom: total height minus part of one viewport.
        let offset = count as f32 * LH; // scrolled to the absolute bottom edge
        let layout = VirtualLineLayout::new(count, LH, VP, offset);
        let range = layout.visible_range();
        // The last painted index is the final line (count-1); the half-open end is therefore count.
        assert_eq!(range.end, count, "end clamps to line_count at the bottom");
        assert!(
            range.contains(&(count - 1)),
            "the last line {} must be painted at the end",
            count - 1
        );
        // The window stays small even at the very bottom.
        assert!(
            range.len() < 100,
            "end window is bounded (got {})",
            range.len()
        );
    }

    #[test]
    fn total_height_is_line_count_times_line_height() {
        let layout = VirtualLineLayout::new(100_000, LH, VP, 0.0);
        assert_eq!(layout.total_height_px(), 100_000.0 * LH);
        // Empty doc -> zero height.
        let empty = VirtualLineLayout::new(0, LH, VP, 0.0);
        assert_eq!(empty.total_height_px(), 0.0);
    }

    #[test]
    fn y_for_line_is_monotonically_increasing() {
        let layout = VirtualLineLayout::new(1_000, LH, VP, 0.0);
        let mut prev = f32::NEG_INFINITY;
        for line in [0usize, 1, 2, 10, 100, 999] {
            let y = layout.y_for_line(line);
            assert!(y > prev, "y_for_line({line})={y} must increase past {prev}");
            assert_eq!(y, line as f32 * LH, "y_for_line is line * line_height");
            prev = y;
        }
    }

    // ── Boundary conditions (MC-002) ──────────────────────────────────────────────────────────────

    #[test]
    fn zero_line_buffer_yields_empty_range() {
        let layout = VirtualLineLayout::new(0, LH, VP, 0.0);
        assert_eq!(layout.visible_range(), 0..0, "empty doc paints nothing");
        assert_eq!(layout.total_height_px(), 0.0);
    }

    #[test]
    fn single_line_buffer_paints_only_line_zero() {
        let layout = VirtualLineLayout::new(1, LH, VP, 0.0);
        let range = layout.visible_range();
        assert_eq!(range, 0..1, "a one-line doc paints exactly line 0");
    }

    #[test]
    fn scroll_past_end_does_not_invert_or_panic() {
        // Tiny doc, huge scroll offset: must clamp to the last line, never produce an inverted range.
        let layout = VirtualLineLayout::new(3, LH, VP, 1_000_000.0);
        let range = layout.visible_range();
        assert!(
            range.start <= range.end,
            "range never inverts (got {range:?})"
        );
        assert_eq!(range.end, 3, "clamped to the 3-line document");
        assert!(
            range.contains(&2),
            "last line still painted when scrolled past the end"
        );
    }

    #[test]
    fn degenerate_line_height_is_clamped_not_divided_by_zero() {
        // line_height 0 would divide-by-zero; the constructor clamps it to 1.0 so the math is sane.
        let layout = VirtualLineLayout::new(100, 0.0, VP, 50.0);
        let range = layout.visible_range();
        assert!(
            range.start <= range.end,
            "no inverted range on a 0 line height"
        );
        assert!(range.end <= 100, "range stays within the document");
        assert_eq!(
            layout.line_height_px(),
            1.0,
            "0 line height clamped to 1px floor"
        );
        // NaN/inf line height is likewise clamped.
        let nan = VirtualLineLayout::new(100, f32::NAN, VP, 50.0);
        assert_eq!(nan.line_height_px(), 1.0);
        assert!(nan.total_height_px().is_finite());
    }

    #[test]
    fn non_finite_scroll_offset_clamps_to_top() {
        let layout = VirtualLineLayout::new(100, LH, VP, f32::NAN);
        assert_eq!(layout.visible_range().start, 0, "NaN offset treated as top");
    }

    // ── MT-078 AC3/AC5/AC6: per-code-line bidi ────────────────────────────────────────────────────────

    #[test]
    fn ltr_code_line_bidi_is_identity() {
        // AC6 / MC-3: an ordinary LTR source line is identity (no reorder, LTR base, no limitation) so the
        // existing code render is byte-for-byte unchanged.
        for line in [
            "let x = 1;",
            "fn main() {}",
            "    // a comment",
            "",
            "中文 comment",
        ] {
            let b = code_line_bidi(line);
            assert!(
                b.is_identity(line),
                "LTR code line must be bidi-identity: {line:?} -> {b:?}"
            );
            assert_eq!(b.visual_text, line);
        }
    }

    #[test]
    fn rtl_code_line_detected_and_reordered() {
        // AC3: a code line containing a Hebrew literal is RTL base (first strong is Hebrew) and reorders.
        let line = "שלום";
        let b = code_line_bidi(line);
        assert_eq!(
            b.base,
            crate::text_intl::Direction::Rtl,
            "Hebrew code line base is RTL"
        );
        assert!(!b.is_identity(line), "an RTL line is not identity");
        assert!(
            b.shaping_limitation.is_none(),
            "Hebrew is non-joining — no limitation"
        );
    }

    #[test]
    fn arabic_code_line_raises_limitation() {
        // AC5: an Arabic code literal raises the typed shaping limitation (never silently broken).
        let line = "let s = \"العربية\";";
        let b = code_line_bidi(line);
        assert!(
            b.shaping_limitation.is_some(),
            "Arabic code line must raise the typed limitation"
        );
        // Base is LTR here (first strong char is the Latin 'l' of `let`), but the limitation still fires
        // because the line CONTAINS Arabic content egui cannot shape.
        assert_eq!(
            b.base,
            crate::text_intl::Direction::Ltr,
            "first strong is Latin -> LTR base"
        );
    }
}
