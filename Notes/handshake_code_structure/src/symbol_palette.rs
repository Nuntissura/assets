//! In-file "Go to Symbol" palette (WP-KERNEL-012 MT-053, E1 — VS Code parity).
//!
//! This is VS Code's `editor.action.quickOutline` (Ctrl+Shift+O): a fuzzy command palette scoped to
//! the symbols of the CURRENTLY ACTIVE file only. It is EXPLICITLY DISTINCT from the global cross-file
//! quick-switcher (MT-030's Ctrl+P / Ctrl+T, backed by the backend graph-search): different palette,
//! different data scope, different keybinding (RISK-001 / MC-001).
//!
//! ## Pure reuse layer — no re-parse, no backend (RISK-002 / MC-002, AC-007)
//!
//! This module owns NO symbol model and performs NO parse. It maps the MT-006
//! [`OutlineItem`](super::outline::OutlineItem) tree (already built in-process from the SAME tree-sitter
//! tree the highlighter produced) into a flat [`FileSymbol`] view-model. It constructs no parser and
//! makes no backend call: a grep gate (AC-007) proves both absences over the shipped code. If a needed
//! datum were missing from the in-process model it would be a typed blocker, never a parse or a backend
//! call.
//!
//! ## Fuzzy matcher
//!
//! The WP-011 `command_palette.rs` does NOT yet expose a reusable fuzzy-filter helper (its `show()` is
//! driven by the `command_registry` command bus and the global quick-switcher is backend-bound), so —
//! per the MT contract step 2 — this module implements a SMALL subsequence-with-gap-penalty matcher
//! ([`fuzzy_match`]) and FLAGS it for later extraction into `command_palette.rs` so the three palettes
//! (MT-030 switcher, MT-031 command palette, this) eventually share one matcher. The matcher mirrors
//! the standard editor quick-pick scoring: every query char must appear in order (subsequence); earlier
//! matches, contiguous runs, and word-boundary hits score higher; gaps are penalized. An empty query
//! matches everything (the full outline, in source order), exactly like VS Code's quick-outline.
//!
//! ## Action
//!
//! [`SymbolPalette::show`] returns [`SymbolPaletteAction::JumpTo`] on confirm, carrying the target
//! `line` (for the fold-aware scroll) and the symbol's `byte_range` (for the caret/selection placement).
//! The PANEL (`panel.rs`) drives the modal and applies the JumpTo through the EXISTING fold-aware
//! `navigate_to_line` + caret API — this module introduces NO scroll mechanism of its own.

use std::ops::Range;

use super::outline::{OutlineItem, OutlineKind, OutlineProvider};

/// Max symbols flattened into the palette (RISK / unbounded-input guard). A real file's outline is far
/// smaller; this only bounds a pathological generated file so the palette list + per-row AccessKit node
/// emission stays cheap. The flatten truncates to this many in source order.
pub const MAX_FILE_SYMBOLS: usize = 2000;

/// One symbol in the active file's outline, flattened for the palette. Adapted FROM an
/// [`OutlineItem`] (MT-006) — this is a VIEW-MODEL, not a second symbol source: `name`, `kind`, and
/// `line` come straight from the outline node; `container` is the nearest enclosing outline symbol's
/// name (so the palette can render `method — in ClassName`), derived during the depth-first flatten.
///
/// `byte_range` is the buffer byte range of the symbol's NAME identifier (start byte of the line ..
/// end byte of the line) used to place the caret/selection on JumpTo. The outline node carries only the
/// start LINE (not a byte range), so the range is resolved from the buffer at flatten time as the
/// symbol's declaration-line byte span — enough to land the caret on the symbol and select its row,
/// matching VS Code's quick-outline (which reveals + selects the symbol's range).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSymbol {
    /// The symbol's display name (from the outline node).
    pub name: String,
    /// The symbol kind (function/method/class/struct/enum/constant/module) — from the outline node.
    pub kind: OutlineKind,
    /// The buffer byte range of the symbol's declaration line (caret/selection target on JumpTo).
    pub byte_range: Range<usize>,
    /// 0-based buffer line the symbol starts on (the fold-aware scroll target on JumpTo).
    pub line: usize,
    /// The nearest enclosing outline symbol's name (e.g. the class a method lives in), or `None` for a
    /// top-level symbol. Rendered as `… — in {container}` so the palette disambiguates same-named
    /// members across containers.
    pub container: Option<String>,
}

impl FileSymbol {
    /// The palette row label: `{kind} {name}` plus `  — in {container}` when the symbol is nested. The
    /// kind tag uses the SAME stable label set as the outline panel ([`OutlineKind::label`]) so the two
    /// surfaces read consistently.
    pub fn display_label(&self) -> String {
        match &self.container {
            Some(c) => format!("{} {}  \u{2014} in {}", self.kind.label(), self.name, c),
            None => format!("{} {}", self.kind.label(), self.name),
        }
    }

    /// The text the fuzzy matcher scores the query against: the bare symbol name. (Container is shown
    /// in the label but not matched, matching VS Code quick-outline, which fuzzes on the symbol name.)
    fn match_target(&self) -> &str {
        &self.name
    }
}

/// The action a confirmed palette selection produces. Returned by [`SymbolPalette::show`] (and by the
/// keyboard-confirm path) so the PANEL can apply it through the existing fold-aware navigate + caret
/// API. There is intentionally only one variant in v1; it is an enum so later quick-outline modes
/// (e.g. "open to the side") can extend it without changing the call site shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SymbolPaletteAction {
    /// Jump to the selected symbol: scroll the viewport to `line` (fold-aware) and place the caret /
    /// selection over `byte_range`.
    JumpTo {
        line: usize,
        byte_range: Range<usize>,
    },
}

/// Flatten an MT-006 outline tree into a flat [`FileSymbol`] list in source order (depth-first), carrying
/// each symbol's nearest enclosing outline symbol's name into `container`.
///
/// The outline is a FLAT `Vec<OutlineItem>` already in source order, where each item's `indent` is its
/// nesting depth (0 = top-level). We reconstruct the parent chain from the indent levels: a stack of
/// `(indent, name)` is maintained; when an item's indent is `d`, the container is the name on the stack
/// at depth `d - 1` (the most recent shallower symbol). This is O(N) over the outline and does NOT touch
/// tree-sitter (RISK-002 — the outline already encoded kind + line + depth).
///
/// `byte_range` is resolved from the buffer as the declaration line's byte span via `line_to_byte`.
/// Truncated to [`MAX_FILE_SYMBOLS`] (RISK — unbounded input).
pub fn flatten_outline(
    items: &[OutlineItem],
    buffer: &handshake_text_mechanics::buffer::TextBuffer,
) -> Vec<FileSymbol> {
    let mut out: Vec<FileSymbol> = Vec::with_capacity(items.len().min(MAX_FILE_SYMBOLS));
    // Stack of (indent, name) for the enclosing-symbol chain. The container of an item at indent `d`
    // is the top entry whose indent is `< d` (the nearest shallower ancestor).
    let mut ancestors: Vec<(usize, String)> = Vec::new();
    let last_line = buffer.len_lines().saturating_sub(1);

    for item in items.iter().take(MAX_FILE_SYMBOLS) {
        // Pop ancestors at or deeper than this item's indent — they are siblings/closed subtrees, not
        // enclosers of this item.
        while ancestors.last().is_some_and(|(d, _)| *d >= item.indent) {
            ancestors.pop();
        }
        let container = ancestors.last().map(|(_, name)| name.clone());

        // Resolve the declaration line's byte span (clamped — the outline line is already clamped to the
        // live buffer by OutlineProvider, but guard again so a stale snapshot never indexes past it).
        let line = item.line.min(last_line);
        let start = buffer.line_to_byte(line).unwrap_or(0);
        let end = buffer
            .line_to_byte(line + 1)
            .unwrap_or_else(|| buffer.len_bytes())
            .min(buffer.len_bytes());
        let byte_range = start..end.max(start);

        out.push(FileSymbol {
            name: item.name.clone(),
            kind: item.kind,
            byte_range,
            line,
            container,
        });
        // This item encloses any deeper-indent items that follow it.
        ancestors.push((item.indent, item.name.clone()));
    }
    out
}

/// A fuzzy subsequence match result: whether the query matched, and the relevance score (higher is
/// better). Used to filter + RANK the symbol list.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FuzzyResult {
    score: i64,
}

/// Subsequence-with-gap-penalty fuzzy match (the small matcher flagged for extraction into
/// `command_palette.rs`). Returns `Some(score)` iff every char of `query` appears in `candidate` in
/// order (case-insensitive), else `None`. Scoring favors: earlier first-match position, contiguous runs
/// (no gap between consecutive matched chars), and word-boundary hits (after `_`, `-`, `.`, `::`, or a
/// lowercase→uppercase camelCase transition). Gaps subtract. An EMPTY query returns a neutral score so
/// the whole list passes (VS Code quick-outline shows the full outline on an empty query).
fn fuzzy_match(query: &str, candidate: &str) -> Option<FuzzyResult> {
    if query.is_empty() {
        return Some(FuzzyResult { score: 0 });
    }
    let q: Vec<char> = query.chars().flat_map(|c| c.to_lowercase()).collect();
    // ONE canonical `orig` vector of the candidate's chars. The match walk iterates over THIS vector by
    // index `ci`, so `ci` always maps 1:1 to a valid `orig` position — `is_word_boundary(&orig, ci)` can
    // never index out of bounds (the prior code walked a separately-flattened lowercase stream whose
    // length could EXCEED `orig` when a char lowercases to multiple chars, e.g. U+0130 'İ' -> 'i' +
    // U+0307, panicking the boundary check on `orig[ci]`; regression-locked by `fuzzy_match_handles_*`).
    let orig: Vec<char> = candidate.chars().collect();

    let mut qi = 0usize;
    let mut score: i64 = 0;
    let mut last_match: Option<usize> = None;

    for (ci, &orig_ch) in orig.iter().enumerate() {
        if qi >= q.len() {
            break;
        }
        // Lowercase THIS char in place. A char may lowercase to several chars (Unicode special-casing);
        // a query char matches when it equals the FIRST char of this position's lowercase mapping. This
        // keeps the index aligned to `orig` (no separate stream) while staying case-insensitive for the
        // dominant single-char-lowercase identifiers the palette actually matches.
        let cl = orig_ch.to_lowercase().next().unwrap_or(orig_ch);
        if cl == q[qi] {
            // Base reward for a match.
            score += 10;
            // Earlier matches are better: subtract the position (small).
            score -= ci as i64 / 4;
            // Contiguity bonus: consecutive matched chars (no gap) score extra.
            if let Some(prev) = last_match {
                if ci == prev + 1 {
                    score += 15;
                } else {
                    // Gap penalty proportional to the skipped span (bounded).
                    score -= ((ci - prev - 1) as i64).min(10);
                }
            }
            // Word-boundary bonus: the matched char starts a word.
            if is_word_boundary(&orig, ci) {
                score += 12;
            }
            last_match = Some(ci);
            qi += 1;
        }
    }

    if qi == q.len() {
        Some(FuzzyResult { score })
    } else {
        None // not all query chars matched in order.
    }
}

/// True when the char at `idx` in `chars` begins a "word" for fuzzy-boundary scoring: index 0, a char
/// after a separator (`_`, `-`, `.`, `:`, space), or a camelCase boundary (a previous lowercase/digit
/// followed by an uppercase letter).
///
/// Bounds-safe by construction: out-of-range `idx` (or a missing previous char) returns `false` rather
/// than panicking. The sole caller (`fuzzy_match`) iterates `idx` over the same `chars` slice so the
/// indices are always in range; the `.get()` guards are defense-in-depth so any future caller cannot
/// reintroduce the index-out-of-bounds panic class (a query char matching past the slice end).
fn is_word_boundary(chars: &[char], idx: usize) -> bool {
    if idx == 0 {
        return true;
    }
    let (Some(&prev), Some(&cur)) = (chars.get(idx - 1), chars.get(idx)) else {
        return false;
    };
    if matches!(prev, '_' | '-' | '.' | ':' | ' ') {
        return true;
    }
    (prev.is_lowercase() || prev.is_ascii_digit()) && cur.is_uppercase()
}

/// The in-file Go-to-Symbol palette: the modal state (open flag, query, selected row, the flattened
/// symbol source) plus the filtered/ranked view. Pure state + UI; it does NOT own the buffer or the
/// scroll — the PANEL drives it and applies the [`SymbolPaletteAction::JumpTo`].
#[derive(Clone, Debug, Default)]
pub struct SymbolPalette {
    /// Whether the palette is open (the modal renders only when true).
    open: bool,
    /// The current fuzzy query text.
    query: String,
    /// All symbols of the active file (the flattened MT-006 outline), source order. The match source.
    symbols: Vec<FileSymbol>,
    /// The current filtered + ranked subset (recomputed on open + each query change). Indices into the
    /// *result* list select a row; the result carries the full [`FileSymbol`] so JumpTo reads it.
    filtered: Vec<FileSymbol>,
    /// The selected row in `filtered` (arrow keys move it, Enter confirms it).
    selected: usize,
}

impl SymbolPalette {
    /// A fresh, closed palette.
    pub fn new() -> Self {
        Self::default()
    }

    /// Open the palette for the active file, sourcing its symbols by flattening the MT-006 outline
    /// (`outline` = the panel's current `OutlineItem` list) against `buffer` (for the byte ranges). The
    /// query resets to empty so the full outline shows (VS Code quick-outline). Idempotent: re-opening
    /// re-seeds from the current outline.
    ///
    /// Takes the outline ITEMS (the MT-006 data model) directly, NOT a re-parse — the panel passes the
    /// list it already computed from the highlighter's tree (AC-001 / RISK-002).
    pub fn open(&mut self, outline: &[OutlineItem], buffer: &handshake_text_mechanics::buffer::TextBuffer) {
        self.symbols = flatten_outline(outline, buffer);
        self.query.clear();
        self.selected = 0;
        self.open = true;
        self.recompute();
    }

    /// Close the palette (Escape / after a confirmed jump / clicking away). A no-op when already closed.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// True while the palette is open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The number of symbols sourced from the active file's outline (the unfiltered count).
    pub fn symbol_count(&self) -> usize {
        self.symbols.len()
    }

    /// Set the query text and re-filter. The PANEL pushes the modal's edited text here each frame (the
    /// same pattern the go-to-line palette uses). No-op when closed.
    pub fn set_query(&mut self, query: impl Into<String>) {
        if !self.open {
            return;
        }
        self.query = query.into();
        self.recompute();
    }

    /// The current filtered + ranked symbols (the rows the palette renders). Highest-scoring first.
    pub fn filter(&mut self, query: &str) -> &[FileSymbol] {
        self.query = query.to_owned();
        self.recompute();
        &self.filtered
    }

    /// The current filtered rows without mutating the query (read-only accessor for the renderer/tests).
    pub fn results(&self) -> &[FileSymbol] {
        &self.filtered
    }

    /// The current query text (read-only accessor for the renderer to seed the modal's TextEdit).
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The currently selected row index in [`results`](Self::results).
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// Move the selection down one row (wraps to the top), clamped to the result count.
    pub fn select_next(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.filtered.len();
    }

    /// Move the selection up one row (wraps to the bottom).
    pub fn select_prev(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            self.filtered.len() - 1
        } else {
            self.selected - 1
        };
    }

    /// Confirm the current selection: returns the [`SymbolPaletteAction::JumpTo`] for the selected row
    /// (and closes the palette), or `None` when there is no selectable row (empty result set). The PANEL
    /// applies the JumpTo through its fold-aware navigate + caret API.
    pub fn confirm(&mut self) -> Option<SymbolPaletteAction> {
        let symbol = self.filtered.get(self.selected)?.clone();
        self.close();
        Some(SymbolPaletteAction::JumpTo {
            line: symbol.line,
            byte_range: symbol.byte_range,
        })
    }

    /// Recompute `filtered` from `symbols` + `query`: fuzzy-match each symbol, keep the matches, sort by
    /// score DESC then by source order (line ASC) for a stable tie-break, and clamp `selected` into the
    /// new result range. An empty query keeps the full list in source order (every symbol scores 0, the
    /// stable sort preserves source order).
    fn recompute(&mut self) {
        let q = self.query.trim();
        let mut scored: Vec<(i64, usize, FileSymbol)> = self
            .symbols
            .iter()
            .enumerate()
            .filter_map(|(idx, sym)| {
                fuzzy_match(q, sym.match_target()).map(|r| (r.score, idx, sym.clone()))
            })
            .collect();
        // Sort by score DESC, then by original source index ASC (stable, deterministic tie-break).
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.filtered = scored.into_iter().map(|(_, _, sym)| sym).collect();
        if self.selected >= self.filtered.len() {
            self.selected = 0;
        }
    }

    /// Build a flattened symbol list straight from a buffer's outline for a host/test that wants to
    /// pre-seed the palette without a panel. Reuses the MT-006 [`OutlineProvider`] on an existing tree —
    /// it does NOT parse here (the caller supplies the tree the highlighter built). Kept thin so tests
    /// can prove the flatten/match logic against the REAL outline rather than a hand-built fixture.
    pub fn symbols_from_outline(
        tree: &tree_sitter::Tree,
        buffer: &handshake_text_mechanics::buffer::TextBuffer,
        language_id: &str,
    ) -> Vec<FileSymbol> {
        let items = OutlineProvider::compute(tree, buffer, language_id);
        flatten_outline(&items, buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use handshake_text_mechanics::buffer::TextBuffer;
    use crate::outline::OutlineProvider;

    fn rust_tree(src: &str) -> tree_sitter::Tree {
        let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).expect("rust language set");
        parser.parse(src, None).expect("rust parse")
    }

    const SRC: &str = "\
struct Widget {
    count: i32,
}

impl Widget {
    fn new() -> Self {
        Widget { count: 0 }
    }
    fn increment(&mut self) {
        self.count += 1;
    }
}

fn standalone() -> i32 {
    42
}
";

    #[test]
    fn flatten_carries_container_and_byte_range() {
        let tree = rust_tree(SRC);
        let buffer = TextBuffer::new(SRC);
        let items = OutlineProvider::compute(&tree, &buffer, "rust");
        let symbols = flatten_outline(&items, &buffer);

        // The two methods inside the impl carry the container name.
        let new = symbols
            .iter()
            .find(|s| s.name == "new")
            .expect("new symbol");
        assert_eq!(
            new.container.as_deref(),
            Some("Widget"),
            "method carries its container"
        );
        let inc = symbols
            .iter()
            .find(|s| s.name == "increment")
            .expect("increment symbol");
        assert_eq!(inc.container.as_deref(), Some("Widget"));

        // A top-level fn has no container.
        let standalone = symbols
            .iter()
            .find(|s| s.name == "standalone")
            .expect("standalone");
        assert_eq!(standalone.container, None, "top-level fn has no container");

        // The byte_range lands on the symbol's declaration line (the start byte equals line_to_byte).
        let expected_start = buffer.line_to_byte(standalone.line).unwrap();
        assert_eq!(
            standalone.byte_range.start, expected_start,
            "byte_range starts at the decl line"
        );
        assert!(
            standalone.byte_range.end > standalone.byte_range.start,
            "byte_range spans the line"
        );
    }

    #[test]
    fn palette_filters_by_fuzzy_query_subset_of_outline() {
        let tree = rust_tree(SRC);
        let buffer = TextBuffer::new(SRC);
        let items = OutlineProvider::compute(&tree, &buffer, "rust");

        let mut palette = SymbolPalette::new();
        palette.open(&items, &buffer);
        // The unfiltered set is the full outline.
        let full = palette.symbol_count();
        assert!(
            full >= 4,
            "outline has struct + impl + 2 methods + standalone fn; got {full}"
        );

        // Fuzzy query "inc" -> increment is a subset of the file outline.
        let results = palette.filter("inc");
        assert!(
            results.iter().any(|s| s.name == "increment"),
            "AC-001: fuzzy 'inc' matches 'increment'; got {results:?}"
        );
        assert!(
            results.len() < full,
            "AC-001: the filtered set is a SUBSET of the full outline ({} < {full})",
            results.len()
        );
        // "increment" should outrank an incidental match because it is a strong subsequence hit.
        assert_eq!(results[0].name, "increment", "best match ranks first");
    }

    #[test]
    fn confirm_emits_jump_to_with_correct_line_and_byte_range() {
        let tree = rust_tree(SRC);
        let buffer = TextBuffer::new(SRC);
        let items = OutlineProvider::compute(&tree, &buffer, "rust");
        let standalone_line = items.iter().find(|i| i.name == "standalone").unwrap().line;

        let mut palette = SymbolPalette::new();
        palette.open(&items, &buffer);
        palette.filter("standalone");
        assert_eq!(palette.results()[0].name, "standalone");

        let action = palette.confirm().expect("confirm emits JumpTo");
        match action {
            SymbolPaletteAction::JumpTo { line, byte_range } => {
                assert_eq!(
                    line, standalone_line,
                    "AC-001: JumpTo line matches the symbol's outline line"
                );
                let expected_start = buffer.line_to_byte(standalone_line).unwrap();
                assert_eq!(
                    byte_range.start, expected_start,
                    "AC-001: JumpTo byte_range starts at the symbol's declaration line"
                );
            }
        }
        assert!(!palette.is_open(), "confirming closes the palette");
    }

    #[test]
    fn empty_query_keeps_full_outline_in_source_order() {
        let tree = rust_tree(SRC);
        let buffer = TextBuffer::new(SRC);
        let items = OutlineProvider::compute(&tree, &buffer, "rust");
        let mut palette = SymbolPalette::new();
        palette.open(&items, &buffer);

        // Empty query -> every symbol passes, in SOURCE order (line ascending).
        let expected = palette.symbol_count();
        let results = palette.filter("").to_vec();
        assert_eq!(results.len(), expected, "empty query keeps every symbol");
        let mut last_line = 0usize;
        for sym in &results {
            assert!(
                sym.line >= last_line,
                "empty-query results stay in source order"
            );
            last_line = sym.line;
        }
    }

    #[test]
    fn no_match_yields_empty_and_confirm_is_none() {
        let tree = rust_tree(SRC);
        let buffer = TextBuffer::new(SRC);
        let items = OutlineProvider::compute(&tree, &buffer, "rust");
        let mut palette = SymbolPalette::new();
        palette.open(&items, &buffer);

        let results = palette.filter("zzzznotasymbol");
        assert!(results.is_empty(), "an unmatched query yields no rows");
        assert_eq!(
            palette.confirm(),
            None,
            "confirm with no rows returns None (no crash, no jump)"
        );
    }

    #[test]
    fn selection_wraps_and_clamps() {
        let tree = rust_tree(SRC);
        let buffer = TextBuffer::new(SRC);
        let items = OutlineProvider::compute(&tree, &buffer, "rust");
        let mut palette = SymbolPalette::new();
        palette.open(&items, &buffer);
        palette.filter(""); // full list
        let n = palette.results().len();
        assert!(n >= 4);

        palette.select_prev(); // from 0 wraps to last
        assert_eq!(palette.selected_index(), n - 1);
        palette.select_next(); // wraps back to 0
        assert_eq!(palette.selected_index(), 0);
    }

    #[test]
    fn fuzzy_match_requires_in_order_subsequence() {
        // "wgt" is a subsequence of "Widget"; "tgw" is not.
        assert!(fuzzy_match("wgt", "Widget").is_some());
        assert!(fuzzy_match("tgw", "Widget").is_none());
        // Case-insensitive.
        assert!(fuzzy_match("WID", "Widget").is_some());
        // Empty query matches.
        assert!(fuzzy_match("", "anything").is_some());
    }

    #[test]
    fn fuzzy_match_handles_multi_char_lowercasing_without_panic() {
        // REGRESSION (adversarial review must-fix): an identifier whose lowercase mapping expands to MORE
        // chars than the original must not panic the fuzzy matcher. U+0130 'İ' is ONE char but lowercases
        // to TWO ('i' + U+0307 combining dot above). Rust permits Unicode XID identifiers, so such a
        // symbol name reaches the matcher. The previous code walked a flattened lowercase stream (len 2
        // here) while indexing the original (len 1) in `is_word_boundary`, panicking with
        // index-out-of-bounds and crashing the egui frame. The matcher now walks ONE `orig` vector so the
        // boundary index is always in range. These calls must merely return (Some/None), never panic.
        let weird = "İ"; // U+0130, single char, multi-char lowercase
        let _ = fuzzy_match("i", weird); // query matches the first lowercase char of position 0
        let _ = fuzzy_match("\u{307}", weird); // the combining-mark trigger from the review
        let _ = fuzzy_match("İ", weird);
        let _ = fuzzy_match("x", weird); // no match path
                                         // Mixed: a normal identifier prefixed with the expanding char, querying a later char so the walk
                                         // crosses the position whose lowercase expanded — this is the exact ci > orig.len() trigger.
        assert!(
            fuzzy_match("name", "İcamelName").is_some() || fuzzy_match("İcn", "İcamelName").is_some(),
            "an identifier containing a multi-char-lowercasing char still fuzzy-matches without panic"
        );
        // The whole flatten + filter path over such a symbol must also be panic-free.
        let buffer = TextBuffer::new("fn İexample() {}\n");
        let items = vec![OutlineItem {
            kind: OutlineKind::Function,
            name: "İexample".to_string(),
            line: 0,
            indent: 0,
        }];
        let mut palette = SymbolPalette::new();
        palette.open(&items, &buffer); // flattens the outline + opens via the real public entry point
        let _ = palette.filter("example"); // must not panic
        let _ = palette.filter("İe"); // must not panic
    }

    #[test]
    fn flatten_truncates_to_cap() {
        // A pathological outline larger than the cap is truncated (RISK / unbounded input).
        let buffer = TextBuffer::new("fn x() {}\n");
        let items: Vec<OutlineItem> = (0..(MAX_FILE_SYMBOLS + 50))
            .map(|i| OutlineItem {
                kind: OutlineKind::Function,
                name: format!("f{i}"),
                line: 0,
                indent: 0,
            })
            .collect();
        let symbols = flatten_outline(&items, &buffer);
        assert_eq!(
            symbols.len(),
            MAX_FILE_SYMBOLS,
            "flatten caps at MAX_FILE_SYMBOLS"
        );
    }
}
