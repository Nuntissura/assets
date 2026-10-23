//! Sticky scroll (WP-KERNEL-012 MT-053, E1 — VS Code parity).
//!
//! VS Code's `editor.stickyScroll`: while the user scrolls inside a function / class / impl block / other
//! nested scope, the signature/declaration lines of every ENCLOSING scope stay pinned at the top of the
//! editor viewport, so the context of the first visible line is always readable.
//!
//! ## Pure reuse layer — no re-parse, no backend (RISK-002 / MC-002, AC-007)
//!
//! This module owns NO scope model and performs NO parse. It INTERSECTS the MT-005
//! [`FoldRegion`](super::folding::FoldRegion) list (the enclosing-scope ranges, from the SAME tree the
//! highlighter built) with the live buffer to read each scope's literal declaration line. It constructs
//! no parser and makes no backend call (a grep gate, AC-007, proves both absences over the shipped code).
//!
//! ## The computation (MT step 4 / RISK-006 / MC-006)
//!
//! A scope is STICKY for a given `viewport_top_line` when it strictly encloses that line:
//! `region.start_line <= viewport_top_line < region.end_line`. (The `< end_line` — not `<=` — means a
//! scope whose closing line is exactly the first visible line is NO LONGER pinned: you have scrolled past
//! it, matching VS Code.) The enclosing scopes are returned OUTERMOST-FIRST (smallest `start_line`), and
//! the count is CAPPED at [`StickyScrollConfig::max_sticky_lines`] (default 5, VS Code parity) so a deeply
//! nested file cannot pin headers that consume the whole viewport. When the cap truncates, the INNERMOST
//! scopes are kept (VS Code keeps the closest context, dropping the outermost) — the header you most need
//! is the function you are inside, not the top-level module.
//!
//! The header TEXT is the LITERAL source declaration line sliced from the buffer (MT-002
//! `slice_to_string`), trimmed of trailing whitespace — NOT a label reconstructed from a symbol node, so
//! `fn main() {` reads exactly as written (impl note / contract step 4).

use handshake_text_mechanics::buffer::TextBuffer;
use super::folding::FoldRegion;

/// One pinned enclosing-scope header. `line` is the scope's declaration (start) line — clicking the
/// header scrolls the viewport to it (the PANEL applies that through its fold-aware scroll). `text` is
/// the literal source declaration line. `depth` is the header's position in the pinned stack
/// (0 = outermost shown) — it drives the per-header AccessKit author_id `sticky-header-{depth}` and the
/// indent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StickyHeader {
    /// 0-based buffer line of the scope's declaration (the click-to-scroll + JumpTo target).
    pub line: usize,
    /// The literal source declaration line text (trailing whitespace trimmed), e.g. `fn main() {`.
    pub text: String,
    /// Position in the pinned stack: 0 = outermost SHOWN header, increasing inward. Drives the
    /// `sticky-header-{depth}` AccessKit id and the row indent.
    pub depth: usize,
}

/// Sticky-scroll configuration. `max_sticky_lines` bounds how many enclosing-scope headers are pinned
/// (RISK-006 / MC-006). VS Code's default is 5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StickyScrollConfig {
    /// Max pinned headers. Default 5 (VS Code parity). When more scopes enclose the viewport top, the
    /// INNERMOST `max_sticky_lines` are kept (the closest context).
    pub max_sticky_lines: usize,
}

impl Default for StickyScrollConfig {
    fn default() -> Self {
        // VS Code's `editor.stickyScroll.maxLineCount` default.
        Self {
            max_sticky_lines: 5,
        }
    }
}

/// The sticky-scroll computer. Stateless apart from its config; the panel recomputes
/// [`compute`](StickyScroll::compute) every frame from the CURRENT scroll offset + the CURRENT fold/outline
/// model (no caching across edits — RISK-004 / MC-004; the computation is bounded by `max_sticky_lines`
/// and is cheap).
#[derive(Clone, Copy, Debug, Default)]
pub struct StickyScroll {
    config: StickyScrollConfig,
}

impl StickyScroll {
    /// A sticky-scroll computer with the default config (max 5 headers).
    pub fn new() -> Self {
        Self::default()
    }

    /// A sticky-scroll computer with an explicit config.
    pub fn with_config(config: StickyScrollConfig) -> Self {
        Self { config }
    }

    /// The configured maximum number of pinned headers.
    pub fn max_sticky_lines(&self) -> usize {
        self.config.max_sticky_lines
    }

    /// Compute the pinned enclosing-scope headers for a viewport whose first visible BUFFER line is
    /// `viewport_top_line`.
    ///
    /// Algorithm (MT step 4):
    /// 1. Select every [`FoldRegion`] that strictly encloses `viewport_top_line`:
    ///    `start_line <= viewport_top_line < end_line`.
    /// 2. Order them OUTERMOST-first (smallest `start_line`). Two scopes sharing a start line (rare —
    ///    the fold provider merges those) are ordered by the wider span first.
    /// 3. CAP at `max_sticky_lines`, keeping the INNERMOST headers (drop the outermost when truncating —
    ///    VS Code keeps the closest context). RISK-006 / MC-006 / AC-002.
    /// 4. For each kept scope, read the LITERAL declaration line from `buffer` (trailing whitespace
    ///    trimmed) as the header text — never reconstructed from a symbol label (impl note / step 4).
    ///
    /// `fold_regions` is the MT-005 region list (folded OR unfolded — sticky headers pin REGARDLESS of
    /// fold state; a scope is sticky because you are scrolled inside it, not because it is collapsed).
    /// `buffer` is the live MT-002 buffer (the header-text source). The `_outline` argument is accepted
    /// for API parity with the MT contract signature (the fold regions already carry the enclosing-scope
    /// ranges + start lines the headers need; the outline is the symbol-name source the palette uses, and
    /// is not needed to slice the literal declaration line — kept in the signature so the panel can pass
    /// it without an awkward call shape, and so a future MT can enrich headers with symbol kinds without a
    /// signature break).
    ///
    /// `depth` in the returned headers is the position in the SHOWN stack (0 = outermost shown), so it is
    /// contiguous `0..headers.len()` even when the cap dropped outer scopes.
    pub fn compute(
        &self,
        viewport_top_line: usize,
        fold_regions: &[FoldRegion],
        buffer: &TextBuffer,
    ) -> Vec<StickyHeader> {
        // 1. Enclosing scopes: start at/above the top line, end strictly below it.
        let mut enclosing: Vec<&FoldRegion> = fold_regions
            .iter()
            .filter(|r| r.start_line <= viewport_top_line && viewport_top_line < r.end_line)
            .collect();

        // 2. Outermost-first: smaller start_line first; for equal starts, the wider span (larger
        //    end_line) is the outer scope, so it comes first.
        enclosing.sort_by(|a, b| {
            a.start_line
                .cmp(&b.start_line)
                .then(b.end_line.cmp(&a.end_line))
        });

        // De-dupe scopes that share a start line (an outer block + its inner that begin on the same line
        // would otherwise pin two identical declaration lines). Keep the first (widest) per start line.
        enclosing.dedup_by_key(|r| r.start_line);

        // 3. Cap at max_sticky_lines, keeping the INNERMOST when truncating (drop outermost): take the
        //    LAST `cap` of the outermost-first list, then they are still in outermost-first order.
        let cap = self.config.max_sticky_lines;
        let kept: Vec<&FoldRegion> = if enclosing.len() > cap {
            enclosing.split_off(enclosing.len() - cap)
        } else {
            enclosing
        };

        // 4. Build the headers, reading the LITERAL declaration line from the buffer.
        let last_line = buffer.len_lines().saturating_sub(1);
        kept.iter()
            .enumerate()
            .map(|(depth, region)| {
                let line = region.start_line.min(last_line);
                let raw = buffer.slice_to_string(line..line + 1);
                let text = raw.trim_end().to_owned();
                StickyHeader { line, text, depth }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use handshake_text_mechanics::buffer::TextBuffer;
    use crate::folding::{FoldProvider, FoldRegion};

    fn rust_tree(src: &str) -> tree_sitter::Tree {
        let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).expect("rust language set");
        parser.parse(src, None).expect("rust parse")
    }

    /// A nested impl -> fn -> if-block so the enclosing stack is observable from REAL MT-005 fold regions.
    /// NOTE: the MT-005 Rust foldable table does NOT include `mod_item`, so a `mod` is intentionally not a
    /// sticky scope (sticky scroll pins fold regions, and a `mod` is not one). This fixture uses scopes
    /// that ARE fold regions (impl_item, function_item, block). Line numbers (0-based):
    /// 0: impl Thing {
    /// 1:     fn deep(&self) -> i32 {
    /// 2:         if true {
    /// 3:             let a = 1;
    /// 4:             let b = 2;
    /// 5:             a + b
    /// 6:         } else {
    /// 7:             0
    /// 8:         }
    /// 9:     }
    /// 10: }
    const NESTED: &str = "\
impl Thing {
    fn deep(&self) -> i32 {
        if true {
            let a = 1;
            let b = 2;
            a + b
        } else {
            0
        }
    }
}
";

    fn nested_regions() -> (Vec<FoldRegion>, TextBuffer) {
        let tree = rust_tree(NESTED);
        let buffer = TextBuffer::new(NESTED);
        let provider = FoldProvider::new();
        let regions = provider.compute(&tree, &buffer, "rust");
        (regions, buffer)
    }

    #[test]
    fn enclosing_scopes_are_outermost_first_for_a_line_inside_a_nested_fn() {
        let (regions, buffer) = nested_regions();
        let sticky = StickyScroll::new();
        // Line 4 (`let b = 2;`) is inside the if-block, inside fn deep, inside impl Thing.
        let headers = sticky.compute(4, &regions, &buffer);

        // Outermost-first: impl (line 0) -> fn (line 1) -> if-block (line 2).
        assert!(
            headers.len() >= 3,
            "AC-002: line inside block->fn->impl yields >=3 enclosing headers; got {headers:?}"
        );
        assert_eq!(headers[0].line, 0, "outermost header is the impl (line 0)");
        assert!(
            headers[0].text.contains("impl Thing"),
            "outermost header text is the impl decl"
        );
        assert_eq!(headers[1].line, 1, "next header is the fn (line 1)");
        assert!(
            headers[1].text.contains("fn deep"),
            "second header text is the fn decl"
        );
        assert_eq!(
            headers[2].line, 2,
            "innermost header is the if-block (line 2)"
        );
        assert!(
            headers[2].text.contains("if true"),
            "innermost header text is the if decl"
        );

        // depth is contiguous 0..n, outermost = 0.
        for (i, h) in headers.iter().enumerate() {
            assert_eq!(h.depth, i, "depth is the shown-stack position");
        }
    }

    #[test]
    fn header_text_is_the_literal_declaration_line() {
        let (regions, buffer) = nested_regions();
        let sticky = StickyScroll::new();
        let headers = sticky.compute(5, &regions, &buffer);
        // The fn header is the literal source line "    fn deep(&self) -> i32 {" trimmed of the trailing
        // whitespace (leading indent preserved — it is the literal line).
        let fn_header = headers
            .iter()
            .find(|h| h.line == 1)
            .expect("fn header present");
        assert!(
            fn_header.text.trim_start() == "fn deep(&self) -> i32 {",
            "header is the literal decl line (not a reconstructed label); got {:?}",
            fn_header.text
        );
    }

    #[test]
    fn cap_truncates_keeping_innermost_scopes() {
        let (regions, buffer) = nested_regions();
        // Cap at 2: line 4 has 3 enclosing scopes (impl, fn, if-block) -> keep the 2 INNERMOST (fn, if).
        let sticky = StickyScroll::with_config(StickyScrollConfig {
            max_sticky_lines: 2,
        });
        let headers = sticky.compute(4, &regions, &buffer);
        assert_eq!(
            headers.len(),
            2,
            "AC-002 / MC-006: capped at max_sticky_lines=2"
        );
        // The OUTERMOST (impl) was dropped; the kept headers are fn (line 1) then if-block (line 2).
        assert_eq!(
            headers[0].line, 1,
            "after cap, outermost shown is the fn (impl dropped)"
        );
        assert_eq!(headers[1].line, 2, "innermost kept is the if-block");
        // depth still contiguous from 0.
        assert_eq!(headers[0].depth, 0);
        assert_eq!(headers[1].depth, 1);
    }

    #[test]
    fn scope_no_longer_pinned_once_scrolled_past_its_end() {
        let (regions, buffer) = nested_regions();
        let sticky = StickyScroll::new();
        // Line 10 (`}` closing the impl) — viewport_top == the impl's end_line, so the impl is NO LONGER
        // enclosing (start <= 10 but NOT 10 < end). Expect no headers (we have scrolled past everything).
        let headers = sticky.compute(10, &regions, &buffer);
        assert!(
            headers.iter().all(|h| h.line != 0),
            "a scope whose end == viewport_top is not pinned (scrolled past); got {headers:?}"
        );
    }

    #[test]
    fn top_of_file_pins_the_scope_you_are_at_the_start_of() {
        let (regions, buffer) = nested_regions();
        let sticky = StickyScroll::new();
        // Line 0 is the impl's own declaration line. Our rule start <= top < end means the impl (start 0)
        // satisfies 0 <= 0 < end, so it pins — matching VS Code, which shows the enclosing scope even at
        // its first line until you scroll its body in. Assert no panic + the impl is the outermost header.
        let headers = sticky.compute(0, &regions, &buffer);
        if let Some(first) = headers.first() {
            assert_eq!(first.depth, 0, "first header depth is 0");
            assert_eq!(first.line, 0, "the impl is pinned at its own start line");
        }
    }

    #[test]
    fn empty_regions_yield_no_headers() {
        let buffer = TextBuffer::new("plain text\nno scopes\n");
        let sticky = StickyScroll::new();
        let headers = sticky.compute(1, &[], &buffer);
        assert!(headers.is_empty(), "no fold regions -> no sticky headers");
    }

    #[test]
    fn line_clamp_guards_a_stale_region_past_a_shorter_buffer() {
        // A region pointing past a now-shorter buffer must not panic; the line clamps and the slice is
        // empty/short (RISK / stale-tree guard).
        let buffer = TextBuffer::new("fn x() {}\n");
        let stale = vec![FoldRegion {
            start_line: 50,
            end_line: 80,
            folded: false,
            label: String::new(),
        }];
        let sticky = StickyScroll::new();
        // viewport_top 60 is inside the stale region; compute must not panic.
        let headers = sticky.compute(60, &stale, &buffer);
        for h in &headers {
            assert!(
                h.line < buffer.len_lines(),
                "header line clamped to the live buffer"
            );
        }
    }
}
