//! Code folding derived from the tree-sitter parse tree (WP-KERNEL-012 MT-005, E1 — VS Code parity).
//!
//! Foldable regions are identified from tree-sitter node KINDS (function bodies, class bodies, block
//! statements, comment groups, …) rather than from indentation or brace matching, so the fold source
//! is the SAME language-aware syntax tree MT-001's highlighter already builds (Monaco folds the same
//! way internally). This MT only COMPUTES the fold regions and applies them to the virtual line
//! layout; the gutter that paints the fold triangles is MT-007.
//!
//! ## The three pieces
//!
//! - [`FoldableNodeTypes`] — the per-language set of node kinds that produce a fold region, keyed by
//!   language id (`"rust"` / `"javascript"`). Stored as a `HashMap<&'static str, &'static [&'static
//!   str]>` exactly as the MT contract specifies, so adding a language is a one-line table edit.
//! - [`FoldProvider::compute`] — walks the parse tree with a `tree_sitter::TreeCursor`
//!   (`tree.walk()`, NOT recursion — avoids stack overflow on deeply nested trees, MT impl note 1),
//!   emits a [`FoldRegion`] for every node whose kind is foldable AND spans >= 2 lines, clamps every
//!   row to the live buffer (RISK-003 / MC-002: tolerate a tree that lags a fast edit by one frame),
//!   and merges regions that share a start line so an outer block and its trailing brace do not stack
//!   two identical folds (RISK-002).
//! - [`FoldSet`] — the runtime fold state: which regions exist, which are folded, and the two queries
//!   the renderer needs ([`is_line_visible`](FoldSet::is_line_visible) and
//!   [`visible_line_to_buffer_line`](FoldSet::visible_line_to_buffer_line)). The visible→buffer
//!   mapping is cached as a `Vec<usize>` and rebuilt only when the fold state changes (RISK-001 /
//!   MC-001), so the render hot loop never pays O(N folds) per visible line.
//!
//! ## Nested folds (MT impl note 4)
//!
//! A fold region can contain inner fold regions. When the OUTER region is folded, all of its lines
//! (including any inner regions) are hidden, regardless of the inner regions' own folded flags — the
//! outer fold wins. Toggling the outer fold does not change the inner folds' state, so expanding the
//! outer restores whatever inner fold state was set before. [`is_line_visible`](FoldSet::is_line_visible)
//! implements this by scanning for the FIRST (outermost) folded region that hides a line; that is
//! enough because [`FoldProvider::compute`] sorts regions by `(start_line, -span)` so an enclosing
//! region always precedes the regions it contains.

use std::collections::HashMap;

use handshake_text_mechanics::buffer::TextBuffer;

/// Max characters of a fold label before it is truncated with an ellipsis (MT impl note 2). A very
/// long first line (e.g. a one-line function with a huge signature) would otherwise blow out the
/// summary row.
const MAX_FOLD_LABEL_LEN: usize = 80;

/// A single foldable region of the document.
///
/// `start_line`/`end_line` are 0-based, inclusive buffer line indices (the tree-sitter node's
/// `start_position().row` / `end_position().row`). `label` is the collapsed summary shown on the
/// start line when the region is folded: the start line's text trimmed of trailing whitespace, plus
/// ` …`, truncated to [`MAX_FOLD_LABEL_LEN`] (e.g. `fn main() {…`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoldRegion {
    /// First buffer line of the region (the line the fold triangle + summary sit on). Stays visible
    /// even when folded.
    pub start_line: usize,
    /// Last buffer line of the region (inclusive). Hidden when the region is folded.
    pub end_line: usize,
    /// Whether this region is currently collapsed.
    pub folded: bool,
    /// The collapsed-summary text (start-line text + ` …`, length-capped).
    pub label: String,
}

impl FoldRegion {
    /// Number of lines this region COLLAPSES when folded: the lines after the start line through the
    /// end line. `end_line - start_line` (the start line stays visible). Always >= 1 because
    /// [`FoldProvider::compute`] only emits regions spanning at least two lines.
    pub fn collapsed_line_count(&self) -> usize {
        self.end_line.saturating_sub(self.start_line)
    }

    /// Build the collapsed summary label for a region starting at `start_line`: the start line's text
    /// with trailing whitespace stripped, plus ` …`, capped at [`MAX_FOLD_LABEL_LEN`] chars (MT impl
    /// note 2). Char-boundary-safe truncation (never splits a multi-byte char — RISK-002 discipline
    /// inherited from the buffer layer).
    fn label_for(start_line: usize, buffer: &TextBuffer) -> String {
        let raw = buffer.slice_to_string(start_line..start_line + 1);
        // slice_to_string includes the trailing newline; trim_end removes it plus any indentation tail.
        let trimmed = raw.trim_end();
        // Truncate on a char boundary so a multi-byte glyph is never split.
        let capped: String = trimmed.chars().take(MAX_FOLD_LABEL_LEN).collect();
        format!("{capped} …")
    }
}

/// The per-language set of tree-sitter node kinds that produce a fold region, keyed by language id.
///
/// The kind strings are the grammars' OWN node-type names (verified against `tree-sitter-rust` 0.24 /
/// `tree-sitter-javascript` 0.25 — the grammars MT-001 bundles), so they match `Node::kind()` exactly.
/// A node kind not in the active language's set produces no fold (the traversal simply descends into
/// it). The map is the exact `HashMap<&'static str, &'static [&'static str]>` the MT contract names.
pub struct FoldableNodeTypes {
    by_language: HashMap<&'static str, &'static [&'static str]>,
}

/// Rust foldable node kinds (MT contract). `function_item`/`impl_item`/`struct_item`/`enum_item` are
/// the item bodies; `block` is any `{ … }` statement block; `match_expression` is the `match { … }`
/// arm body; `use_declaration` covers a grouped `use { … }` import. These are real
/// `tree-sitter-rust` node kinds.
const RUST_FOLDABLE: &[&str] = &[
    "function_item",
    "impl_item",
    "struct_item",
    "enum_item",
    "block",
    "match_expression",
    "use_declaration",
];

/// JS/TS foldable node kinds (MT contract). Real `tree-sitter-javascript` node kinds:
/// `function_declaration`, `arrow_function`, `class_declaration`, `statement_block` (the grammar's
/// name for a `{ … }` block — the contract's prose said `block_statement`; the grammar's actual kind
/// is `statement_block`, used here so `Node::kind()` matches), `object`, and `array`.
const JS_FOLDABLE: &[&str] = &[
    "function_declaration",
    "arrow_function",
    "class_declaration",
    "statement_block",
    "object",
    "array",
];

impl FoldableNodeTypes {
    /// The default table for the languages MT-001 bundles (Rust + JavaScript). More languages plug in
    /// by extending the map (the same extensible-seam pattern as the highlight registry).
    pub fn bundled() -> Self {
        let mut by_language: HashMap<&'static str, &'static [&'static str]> = HashMap::new();
        by_language.insert("rust", RUST_FOLDABLE);
        by_language.insert("javascript", JS_FOLDABLE);
        Self { by_language }
    }

    /// True when `kind` is a foldable node kind for `language_id`. Unknown language → nothing folds.
    pub fn is_foldable(&self, language_id: &str, kind: &str) -> bool {
        self.by_language
            .get(language_id)
            .map(|kinds| kinds.contains(&kind))
            .unwrap_or(false)
    }

    /// The foldable kinds for one language (or an empty slice for an unknown language). Exposed for
    /// tests + later language additions.
    pub fn kinds_for(&self, language_id: &str) -> &'static [&'static str] {
        self.by_language.get(language_id).copied().unwrap_or(&[])
    }
}

impl Default for FoldableNodeTypes {
    fn default() -> Self {
        Self::bundled()
    }
}

/// Computes fold regions from a tree-sitter parse tree. Stateless apart from its language table; the
/// caller (the panel) re-runs [`compute`](FoldProvider::compute) only when the buffer version changes
/// (MT impl note 3), then carries the result in a [`FoldSet`].
pub struct FoldProvider {
    node_types: FoldableNodeTypes,
}

impl FoldProvider {
    /// A provider with the bundled (Rust + JS) foldable-node table.
    pub fn new() -> Self {
        Self {
            node_types: FoldableNodeTypes::bundled(),
        }
    }

    /// A provider with a custom node-type table (tests / extra languages).
    pub fn with_node_types(node_types: FoldableNodeTypes) -> Self {
        Self { node_types }
    }

    /// Walk `tree` and return the fold regions for `language_id`, in render order.
    ///
    /// Algorithm (MT contract + impl notes):
    /// 1. Iterative pre-order traversal with a `tree_sitter::TreeCursor` (`tree.walk()`) — NOT
    ///    recursion, so a deeply nested tree cannot overflow the stack (impl note 1).
    /// 2. A node contributes a region iff its kind is foldable for `language_id` AND it spans at least
    ///    two lines (`end_row - start_row >= 1`). Single-line nodes never fold.
    /// 3. Every row is CLAMPED to `0..=buffer.len_lines()-1` before use (RISK-003 / MC-002) so a tree
    ///    that lags a fast edit by one frame can never index past the live buffer.
    /// 4. Regions are sorted by `(start_line ASC, end_line DESC)` — an enclosing region sorts before
    ///    the regions it contains — then regions that share a start line are MERGED to the widest end
    ///    line (RISK-002), so an item body and its inner block do not stack two folds on one start
    ///    line.
    ///
    /// `language_id` selects the foldable-node set; an unknown language yields no regions (the
    /// contract keys [`FoldableNodeTypes`] by language id, so the id must flow in here — this is the
    /// one parameter the contract's prose `compute(tree, buffer)` signature implies but does not name).
    pub fn compute(
        &self,
        tree: &tree_sitter::Tree,
        buffer: &TextBuffer,
        language_id: &str,
    ) -> Vec<FoldRegion> {
        let max_line = buffer.len_lines().saturating_sub(1);
        let mut regions: Vec<FoldRegion> = Vec::new();

        // Iterative pre-order DFS over the whole tree using the cursor (impl note 1). The cursor walks
        // first-child / next-sibling / parent; this loop visits every node exactly once without a
        // recursive call.
        let mut cursor = tree.walk();
        loop {
            let node = cursor.node();
            let kind = node.kind();
            if self.node_types.is_foldable(language_id, kind) {
                // Clamp both rows to the live buffer (RISK-003 / MC-002).
                let start_line = node.start_position().row.min(max_line);
                let end_line = node.end_position().row.min(max_line);
                // Only regions spanning at least two lines fold (end - start >= 1).
                if end_line > start_line {
                    let label = FoldRegion::label_for(start_line, buffer);
                    regions.push(FoldRegion {
                        start_line,
                        end_line,
                        folded: false,
                        label,
                    });
                }
            }

            // Advance the cursor in pre-order: descend, else move to the next sibling, else climb until
            // a sibling exists; stop when we climb back out of the root.
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    // Climbed out of the root — traversal complete.
                    return Self::sort_and_merge(regions);
                }
            }
        }
    }

    /// Sort regions by `(start_line ASC, end_line DESC)` and merge regions sharing a start line into
    /// the widest one (RISK-002). The DESC end-line tiebreak keeps the outermost (widest) region first
    /// among same-start regions, which is what [`FoldSet::is_line_visible`] relies on for nested
    /// folds.
    fn sort_and_merge(mut regions: Vec<FoldRegion>) -> Vec<FoldRegion> {
        regions.sort_by(|a, b| {
            a.start_line
                .cmp(&b.start_line)
                .then(b.end_line.cmp(&a.end_line))
        });
        let mut merged: Vec<FoldRegion> = Vec::with_capacity(regions.len());
        for region in regions {
            match merged.last_mut() {
                // Same start line as the previous (already-widest) region: absorb — keep the wider
                // end line. The previous region sorted first, so it is already the wider one; we only
                // extend it if this region somehow reaches further (defensive).
                Some(prev) if prev.start_line == region.start_line => {
                    if region.end_line > prev.end_line {
                        prev.end_line = region.end_line;
                    }
                }
                _ => merged.push(region),
            }
        }
        merged
    }
}

impl Default for FoldProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// The runtime fold state: the computed regions plus a cached visible→buffer line map.
///
/// The renderer drives this every frame through [`is_line_visible`](Self::is_line_visible) (skip
/// hidden lines) and [`visible_line_to_buffer_line`](Self::visible_line_to_buffer_line) (map a
/// post-fold line index back to a buffer line). The latter is the hot path on a large file, so its
/// result is cached as a `Vec<usize>` and rebuilt only when the fold state changes (RISK-001 /
/// MC-001): every mutator ([`toggle`](Self::toggle), [`set_regions`](Self::set_regions)) invalidates
/// the cache, and the next query rebuilds it lazily.
#[derive(Debug, Clone, Default)]
pub struct FoldSet {
    /// Fold regions, sorted by `(start_line ASC, end_line DESC)` (the order
    /// [`FoldProvider::compute`] produces). `is_line_visible` relies on the start-line ordering.
    pub regions: Vec<FoldRegion>,
    /// Cached map from visible (post-fold) line index → buffer line index. `None` when the fold state
    /// changed since the last build; rebuilt lazily by [`ensure_visible_map`](Self::ensure_visible_map)
    /// (RISK-001 / MC-001 — the invalidation strategy is documented on the field so a maintainer sees
    /// it at the cache site).
    visible_map_cache: Option<Vec<usize>>,
    /// The `buffer_len_lines` the current [`visible_map_cache`](Self::visible_map_cache) was built for.
    /// Fold mutations already null the cache, so a present cache means the fold state is unchanged since
    /// the build; the only remaining staleness is a buffer that grew/shrank between frames. Tracking the
    /// build-time line count lets [`rebuild_visible_map_for`](Self::rebuild_visible_map_for) reuse the
    /// cache on the common no-edit/no-fold-change frame instead of rebuilding an O(buffer_len_lines) map
    /// EVERY frame (the MT-002 100k-line frame-budget hot path).
    visible_map_built_for: Option<usize>,
}

impl FoldSet {
    fn invalidate_visible_map(&mut self) {
        self.visible_map_cache = None;
        self.visible_map_built_for = None;
    }

    /// An empty fold set (no regions, nothing folded).
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a fold set from `regions` (already sorted by [`FoldProvider::compute`]).
    pub fn from_regions(regions: Vec<FoldRegion>) -> Self {
        Self {
            regions,
            visible_map_cache: None,
            visible_map_built_for: None,
        }
    }

    /// Replace the regions, preserving the folded state of any region whose `start_line` still exists
    /// in the new set (so a re-parse after an edit keeps a user's collapsed regions collapsed). The
    /// visible-map cache is invalidated (RISK-001 / MC-001).
    pub fn set_regions(&mut self, new_regions: Vec<FoldRegion>) {
        let folded_starts: Vec<usize> = self
            .regions
            .iter()
            .filter(|r| r.folded)
            .map(|r| r.start_line)
            .collect();
        let mut regions = new_regions;
        for region in &mut regions {
            if folded_starts.contains(&region.start_line) {
                region.folded = true;
            }
        }
        self.regions = regions;
        self.invalidate_visible_map();
    }

    /// Toggle the folded state of the region whose `start_line` equals `start_line`. No-op (returns
    /// `false`) when no region starts on that line. Invalidates the visible-map cache so the next
    /// query rebuilds it (RISK-001 / MC-001).
    pub fn toggle(&mut self, start_line: usize) -> bool {
        if let Some(region) = self.regions.iter_mut().find(|r| r.start_line == start_line) {
            region.folded = !region.folded;
            self.invalidate_visible_map();
            true
        } else {
            false
        }
    }

    /// Set the folded state of the region whose `start_line` equals `start_line`. Returns `true` only
    /// when a real region changed state. Invalidates the visible-map cache on change, matching
    /// [`toggle`](Self::toggle), so AccessKit Expand/Collapse dispatches rebuild the render map.
    pub fn set_folded(&mut self, start_line: usize, folded: bool) -> bool {
        if let Some(region) = self.regions.iter_mut().find(|r| r.start_line == start_line) {
            if region.folded == folded {
                return false;
            }
            region.folded = folded;
            self.invalidate_visible_map();
            true
        } else {
            false
        }
    }

    /// Set every fold region to the requested state. Returns `true` only when at least one region
    /// changed, and invalidates the visible-line map so render/navigation cannot reuse a stale layout.
    pub fn set_all_folded(&mut self, folded: bool) -> bool {
        let mut changed = false;
        for region in &mut self.regions {
            if region.folded != folded {
                region.folded = folded;
                changed = true;
            }
        }
        if changed {
            self.invalidate_visible_map();
        }
        changed
    }

    /// True when `line` is visible — i.e. it is NOT hidden inside a folded region.
    ///
    /// A line is hidden iff some folded region covers it AND it is not that region's start line: a
    /// folded region `[s, e]` hides `s+1 ..= e` (the start line `s` stays visible, showing the
    /// collapsed summary). Nested folds: the FIRST (outermost) folded region that covers the line
    /// wins — because regions are sorted with the enclosing region first, scanning forward and
    /// stopping at the first hit yields the outer fold's verdict, which is correct (an outer fold
    /// hides every inner line regardless of inner fold state, MT impl note 4).
    pub fn is_line_visible(&self, line: usize) -> bool {
        for region in &self.regions {
            // Regions are sorted by start_line ascending; once a region starts past `line`, no later
            // region can cover it (a region covers only `start..=end` and start only grows).
            if region.start_line > line {
                break;
            }
            if region.folded && line > region.start_line && line <= region.end_line {
                return false;
            }
        }
        true
    }

    /// Map a visible (post-fold) line index to the actual buffer line index, accounting for every
    /// folded region above it. Uses the cached `Vec<usize>` map (RISK-001 / MC-001); rebuilds it on a
    /// cache miss. A `visible_line` past the end of the visible document clamps to the last buffer
    /// line (never panics / never indexes past the buffer — the stale-tree guard discipline).
    pub fn visible_line_to_buffer_line(&mut self, visible_line: usize) -> usize {
        self.ensure_visible_map();
        let map = self
            .visible_map_cache
            .as_ref()
            .expect("visible map built by ensure_visible_map");
        if map.is_empty() {
            return 0;
        }
        match map.get(visible_line) {
            Some(&buffer_line) => buffer_line,
            // Past the end of the visible document: clamp to the last visible→buffer entry.
            None => *map.last().unwrap(),
        }
    }

    /// The number of visible (post-fold) lines: the buffer line count minus the buffer lines hidden by
    /// the current fold state. The renderer passes this to `VirtualLineLayout` as the effective line
    /// count (MT step 6). `buffer_len_lines` is the live buffer's `len_lines()`.
    ///
    /// Delegates to [`hidden_line_count`](Self::hidden_line_count) for the hidden-line total. That
    /// counter is nesting-correct: an inner folded region inside an outer folded region adds nothing
    /// (the outer already hides its lines), so this matches the line count
    /// [`rebuild_visible_map_for`](Self::rebuild_visible_map_for) produces — which is the render path's
    /// source of truth — even when both an outer AND a nested inner region are folded. A naive sum of
    /// every folded region's `collapsed_line_count()` would double-count the overlap and return a
    /// too-small count, mis-sizing any downstream consumer (gutter MT-007, minimap MT-006, scrollbar).
    pub fn visible_line_count(&self, buffer_len_lines: usize) -> usize {
        buffer_len_lines.saturating_sub(self.hidden_line_count())
    }

    /// The currently-folded regions, in start-line order. NOTE: this is the raw per-region list; a
    /// naive sum of each region's `collapsed_line_count()` over this iterator double-counts nested
    /// folds (an inner folded region inside an outer folded region). For a correct hidden-line total
    /// use [`hidden_line_count`](Self::hidden_line_count) (which [`visible_line_count`](Self::visible_line_count)
    /// delegates to). Exposed for the gutter (MT-007) + tests, which iterate folded regions to draw
    /// fold markers rather than to count hidden lines.
    pub fn folded_regions(&self) -> impl Iterator<Item = &FoldRegion> {
        self.regions.iter().filter(|r| r.folded)
    }

    /// The region (if any) whose `start_line` equals `line` — i.e. a fold the renderer should draw a
    /// summary label for when it is folded. Used by the panel's render loop (MT step 4) and the gutter
    /// (MT-007).
    pub fn region_starting_at(&self, line: usize) -> Option<&FoldRegion> {
        self.regions.iter().find(|r| r.start_line == line)
    }

    /// Total buffer lines hidden by the current fold state, counting nested folds correctly (an inner
    /// folded region inside an outer folded region adds nothing — the outer already hides it). This is
    /// the authoritative hidden-line count the visible-line math uses: [`visible_line_count`](Self::visible_line_count)
    /// delegates to it, so the count it returns agrees with the `is_line_visible`-driven map that
    /// [`rebuild_visible_map_for`](Self::rebuild_visible_map_for) builds for the render path.
    pub fn hidden_line_count(&self) -> usize {
        // Walk lines once via the visibility predicate is O(N lines); instead, accumulate hidden
        // ranges from outer-most folded regions only. Because regions are sorted enclosing-first,
        // track the furthest end line already covered by an outer fold and skip inner folds within it.
        let mut hidden = 0usize;
        let mut covered_until: Option<usize> = None;
        for region in &self.regions {
            if !region.folded {
                continue;
            }
            match covered_until {
                // This region is nested inside an outer folded region already counted — skip it.
                Some(end) if region.start_line <= end => {
                    // Extend coverage if this region somehow reaches past the outer (defensive).
                    if region.end_line > end {
                        // The extra tail (end+1 ..= region.end_line) is newly hidden.
                        hidden += region.end_line - end;
                        covered_until = Some(region.end_line);
                    }
                }
                _ => {
                    hidden += region.collapsed_line_count();
                    covered_until = Some(region.end_line);
                }
            }
        }
        hidden
    }

    /// Rebuild the visible→buffer line map if it is stale (RISK-001 / MC-001). The map lists, in
    /// visible order, the buffer line index of every line that [`is_line_visible`](Self::is_line_visible)
    /// reports visible. Built once per fold-state change, then reused by every
    /// [`visible_line_to_buffer_line`](Self::visible_line_to_buffer_line) call that frame.
    fn ensure_visible_map(&mut self) {
        if self.visible_map_cache.is_some() {
            return;
        }
        // The buffer line count is the max end_line + 1 across regions, or — when there are no regions
        // — unknown here; the panel always rebuilds via `rebuild_visible_map_for` with the live line
        // count, so this fallback only matters for region-free sets where visible == buffer 1:1.
        let max_line = self
            .regions
            .iter()
            .map(|r| r.end_line)
            .max()
            .map(|m| m + 1)
            .unwrap_or(0);
        let map = self.build_visible_map(max_line);
        self.visible_map_cache = Some(map);
        // This ensure-built cache covers only `max_line` (regions' last end+1), which is usually SHORTER
        // than the live buffer length. Invalidate `visible_map_built_for` so `rebuild_visible_map_for`
        // never trusts this partial cache and returns a truncated visible-line count (the toggle→query→
        // rebuild interleaving bug): a PRESENT cache with `built_for == None` forces a full rebuild.
        self.visible_map_built_for = None;
    }

    /// Build (without caching) the visible→buffer map for a document of `buffer_len_lines` lines: every
    /// buffer line that is visible, in order. Separated so the panel can rebuild the map against the
    /// LIVE buffer line count (which may exceed the regions' max end line) via
    /// [`rebuild_visible_map_for`](Self::rebuild_visible_map_for).
    fn build_visible_map(&self, buffer_len_lines: usize) -> Vec<usize> {
        let mut map = Vec::with_capacity(buffer_len_lines);
        for line in 0..buffer_len_lines {
            if self.is_line_visible(line) {
                map.push(line);
            }
        }
        map
    }

    /// Rebuild the visible→buffer cache against the LIVE buffer line count and return the visible line
    /// count. The panel calls this once per frame (cheap on a cache hit because it only rebuilds when
    /// the fold state changed) so the map covers the whole live document, including lines below the
    /// last fold region. Returns the number of visible lines (== the map length).
    pub fn rebuild_visible_map_for(&mut self, buffer_len_lines: usize) -> usize {
        // Reuse the cached map when it is still valid. Every fold mutation ([`toggle`], [`set_regions`])
        // already nulls `visible_map_cache`, so a PRESENT cache means the fold state has not changed since
        // the build; the only remaining staleness is a buffer that grew/shrank (which changes
        // `buffer_len_lines`). So rebuild ONLY when the cache is absent OR was built for a different line
        // count. This turns the per-frame cost from O(buffer_len_lines) — a full 100k-line rebuild + an
        // 800KB Vec allocation EVERY frame, the MT-002 AC-002 frame-budget hot path this fixes — into O(1)
        // on the common no-edit/no-fold-change frame (a length compare + a cached-len read).
        if self.visible_map_built_for == Some(buffer_len_lines) {
            if let Some(map) = &self.visible_map_cache {
                return map.len();
            }
        }
        let map = self.build_visible_map(buffer_len_lines);
        let len = map.len();
        self.visible_map_cache = Some(map);
        self.visible_map_built_for = Some(buffer_len_lines);
        len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse `src` as Rust and return its tree-sitter tree (reusing the MT-001 highlighter's parser so
    /// the test exercises the real grammar, not a stub).
    fn rust_tree(src: &str) -> tree_sitter::Tree {
        let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).expect("rust language set");
        parser.parse(src, None).expect("rust parse")
    }

    fn js_tree(src: &str) -> tree_sitter::Tree {
        let lang: tree_sitter::Language = tree_sitter_javascript::LANGUAGE.into();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).expect("js language set");
        parser.parse(src, None).expect("js parse")
    }

    // A real ~20-line Rust function (AC-001).
    const RUST_FN: &str = "\
fn compute(input: i32) -> i32 {
    let mut total = 0;
    for i in 0..input {
        if i % 2 == 0 {
            total += i;
        } else {
            total -= i;
        }
    }
    let doubled = total * 2;
    let label = String::from(\"result\");
    println!(\"{}: {}\", label, doubled);
    if doubled > 100 {
        return doubled;
    }
    total
}
";

    // ── AC-001: compute returns a region for a multi-line function; single-line nodes do not fold ──

    #[test]
    fn fold_provider_finds_function_body_region() {
        let tree = rust_tree(RUST_FN);
        let buffer = TextBuffer::new(RUST_FN);
        let regions = FoldProvider::new().compute(&tree, &buffer, "rust");
        assert!(
            !regions.is_empty(),
            "AC-001: a 17-line rust function must produce at least one fold region; got none"
        );
        // The outermost region covers the function body: start line 0, end line near the last `}`.
        let outer = &regions[0];
        assert_eq!(outer.start_line, 0, "outer fold starts on the `fn` line");
        assert!(
            outer.end_line >= 15,
            "outer fold reaches the closing brace (got end_line {})",
            outer.end_line
        );
        // The label is the first line + ellipsis.
        assert!(
            outer.label.starts_with("fn compute(input: i32) -> i32 {"),
            "fold label is the first line text; got {:?}",
            outer.label
        );
        assert!(
            outer.label.ends_with('…'),
            "fold label ends with an ellipsis; got {:?}",
            outer.label
        );
    }

    #[test]
    fn fold_provider_skips_single_line_nodes() {
        // A one-line function: its body block is on a single line, so NO fold region (AC-001).
        let src = "fn one_liner() { let x = 1; }\n";
        let tree = rust_tree(src);
        let buffer = TextBuffer::new(src);
        let regions = FoldProvider::new().compute(&tree, &buffer, "rust");
        assert!(
            regions.is_empty(),
            "AC-001: a single-line function produces no fold region; got {regions:?}"
        );
    }

    #[test]
    fn fold_provider_js_function_region() {
        let src = "\
function greet(name) {
    const prefix = \"hello\";
    return prefix + name;
}
";
        let tree = js_tree(src);
        let buffer = TextBuffer::new(src);
        let regions = FoldProvider::new().compute(&tree, &buffer, "javascript");
        assert!(
            !regions.is_empty(),
            "a multi-line JS function must produce a fold region; got none"
        );
        assert_eq!(
            regions[0].start_line, 0,
            "JS fold starts on the function line"
        );
    }

    #[test]
    fn fold_provider_unknown_language_has_no_regions() {
        let tree = rust_tree(RUST_FN);
        let buffer = TextBuffer::new(RUST_FN);
        let regions = FoldProvider::new().compute(&tree, &buffer, "cobol");
        assert!(regions.is_empty(), "an unregistered language folds nothing");
    }

    // ── AC-002: is_line_visible for a folded region (lines 5-10) ──────────────────────────────────

    #[test]
    fn fold_set_visibility_hides_inner_lines_only() {
        // A single region covering lines 5..=10, folded.
        let region = FoldRegion {
            start_line: 5,
            end_line: 10,
            folded: true,
            label: "x …".to_owned(),
        };
        let set = FoldSet::from_regions(vec![region]);
        // AC-002 exact assertions:
        assert!(
            set.is_line_visible(4),
            "line 4 (before the region) is visible"
        );
        assert!(
            set.is_line_visible(5),
            "line 5 (the start line) stays visible when folded"
        );
        assert!(
            !set.is_line_visible(6),
            "line 6 (inside the region) is hidden"
        );
        assert!(!set.is_line_visible(10), "line 10 (the end line) is hidden");
        assert!(
            set.is_line_visible(11),
            "line 11 (after the region) is visible"
        );
    }

    #[test]
    fn fold_set_unfolded_region_hides_nothing() {
        let region = FoldRegion {
            start_line: 5,
            end_line: 10,
            folded: false,
            label: "x …".to_owned(),
        };
        let set = FoldSet::from_regions(vec![region]);
        for line in 0..15 {
            assert!(
                set.is_line_visible(line),
                "an UNfolded region hides nothing (line {line})"
            );
        }
    }

    #[test]
    fn fold_set_nested_outer_fold_hides_inner_regions() {
        // Outer region [2, 12] (folded) encloses inner region [5, 8] (NOT folded). The outer fold must
        // hide every line 3..=12 regardless of the inner region's state (MT impl note 4 / MC-003).
        let outer = FoldRegion {
            start_line: 2,
            end_line: 12,
            folded: true,
            label: "o …".to_owned(),
        };
        let inner = FoldRegion {
            start_line: 5,
            end_line: 8,
            folded: false,
            label: "i …".to_owned(),
        };
        // compute() would sort enclosing-first; emulate that order here.
        let set = FoldSet::from_regions(vec![outer, inner]);
        assert!(set.is_line_visible(2), "outer start visible");
        assert!(!set.is_line_visible(3), "outer hides line 3");
        assert!(
            !set.is_line_visible(5),
            "outer hides the inner region's start line too"
        );
        assert!(!set.is_line_visible(8), "outer hides inner end");
        assert!(!set.is_line_visible(12), "outer hides its own end line");
        assert!(
            set.is_line_visible(13),
            "line after the outer region is visible"
        );
    }

    #[test]
    fn fold_set_nested_both_folded_visible_count_matches_render_map() {
        // Regression guard for the double-count bug: BOTH an outer [2,12] and a nested inner [5,8]
        // region are folded. The outer fold already hides lines 3..=12 (10 lines); the inner fold
        // adds NOTHING because every line it would hide is already hidden by the outer. A naive
        // per-region sum would subtract 10 + 3 = 13 hidden lines and report a too-small visible count.
        // visible_line_count MUST equal the length of the map the render path actually builds via
        // rebuild_visible_map_for (the is_line_visible-driven source of truth).
        let outer = FoldRegion {
            start_line: 2,
            end_line: 12,
            folded: true,
            label: "o …".to_owned(),
        };
        let inner = FoldRegion {
            start_line: 5,
            end_line: 8,
            folded: true,
            label: "i …".to_owned(),
        };
        let mut set = FoldSet::from_regions(vec![outer, inner]);
        let buffer_len = 20;

        // The outer fold hides exactly 10 lines (3..=12); the nested inner fold double-counts under a
        // naive sum, but hidden_line_count is nesting-correct.
        assert_eq!(
            set.hidden_line_count(),
            10,
            "nested inner fold adds no extra hidden lines"
        );

        let render_visible = set.rebuild_visible_map_for(buffer_len);
        assert_eq!(
            set.visible_line_count(buffer_len),
            render_visible,
            "visible_line_count must equal the render-path visible-map length when an outer AND a \
             nested inner region are both folded (no double-count)"
        );
        assert_eq!(
            set.visible_line_count(buffer_len),
            buffer_len - 10,
            "20 lines minus the 10 the outer fold hides = 10 visible"
        );
    }

    // ── AC-003: visible_line_to_buffer_line mapping ───────────────────────────────────────────────

    #[test]
    fn fold_set_mapping_offsets_lines_after_a_folded_region() {
        // 20-line buffer; region [3, 8] folded collapses 5 lines (4..=8). Visible lines 0..=3 map 1:1;
        // visible line 4 maps to buffer line 9 (the first line after the collapsed region).
        let region = FoldRegion {
            start_line: 3,
            end_line: 8,
            folded: true,
            label: "x …".to_owned(),
        };
        let mut set = FoldSet::from_regions(vec![region]);
        set.rebuild_visible_map_for(20);

        assert_eq!(
            set.visible_line_to_buffer_line(0),
            0,
            "visible 0 -> buffer 0"
        );
        assert_eq!(
            set.visible_line_to_buffer_line(3),
            3,
            "visible 3 -> buffer 3 (fold start)"
        );
        assert_eq!(
            set.visible_line_to_buffer_line(4),
            9,
            "AC-003: visible 4 -> buffer 9 (skips the 5 collapsed lines 4..=8)"
        );
        assert_eq!(
            set.visible_line_to_buffer_line(5),
            10,
            "visible 5 -> buffer 10"
        );
        // visible_line_count == 20 - 5 collapsed = 15.
        assert_eq!(
            set.visible_line_count(20),
            15,
            "5 collapsed lines removed from 20"
        );
    }

    #[test]
    fn fold_set_mapping_is_identity_when_nothing_folded() {
        let region = FoldRegion {
            start_line: 3,
            end_line: 8,
            folded: false,
            label: "x …".to_owned(),
        };
        let mut set = FoldSet::from_regions(vec![region]);
        set.rebuild_visible_map_for(20);
        for v in 0..20 {
            assert_eq!(
                set.visible_line_to_buffer_line(v),
                v,
                "no fold -> identity map (line {v})"
            );
        }
        assert_eq!(set.visible_line_count(20), 20);
    }

    // ── AC-006: toggle idempotency ────────────────────────────────────────────────────────────────

    #[test]
    fn fold_set_toggle_twice_returns_to_original() {
        let region = FoldRegion {
            start_line: 0,
            end_line: 16,
            folded: false,
            label: "fn …".to_owned(),
        };
        let mut set = FoldSet::from_regions(vec![region]);
        assert!(
            set.is_line_visible(5),
            "initially unfolded -> line 5 visible"
        );

        assert!(set.toggle(0), "toggle a region that exists returns true");
        assert!(
            !set.is_line_visible(5),
            "after one toggle the region is folded -> line 5 hidden"
        );

        assert!(set.toggle(0), "second toggle on the same start line");
        assert!(
            set.is_line_visible(5),
            "AC-006: toggling twice returns to the original UNfolded state"
        );
        // Idempotency at the data level: the region's folded flag is back to false.
        assert!(
            !set.regions[0].folded,
            "folded flag back to false after two toggles"
        );
    }

    #[test]
    fn fold_set_toggle_missing_start_line_is_noop() {
        let region = FoldRegion {
            start_line: 0,
            end_line: 5,
            folded: false,
            label: "x …".to_owned(),
        };
        let mut set = FoldSet::from_regions(vec![region]);
        assert!(
            !set.toggle(99),
            "toggling a non-existent start line returns false (no-op)"
        );
        assert!(!set.regions[0].folded, "the real region is untouched");
    }

    // ── RISK-003 / MC-002: stale tree rows clamp to the live buffer ──────────────────────────────

    #[test]
    fn compute_clamps_rows_to_a_shorter_buffer() {
        // Parse a long source, then run compute against a SHORTER buffer (simulating a fast delete that
        // shrank the buffer before the tree re-parsed). No row may exceed the live buffer's last line.
        let tree = rust_tree(RUST_FN);
        let short = TextBuffer::new("fn x() {\n}\n"); // 3 lines (incl. trailing)
        let max_line = short.len_lines().saturating_sub(1);
        let regions = FoldProvider::new().compute(&tree, &short, "rust");
        for r in &regions {
            assert!(
                r.start_line <= max_line,
                "start_line {} clamped to {}",
                r.start_line,
                max_line
            );
            assert!(
                r.end_line <= max_line,
                "end_line {} clamped to {}",
                r.end_line,
                max_line
            );
        }
    }

    #[test]
    fn foldable_node_types_match_contract() {
        let types = FoldableNodeTypes::bundled();
        // Rust set (MT contract).
        for kind in [
            "function_item",
            "impl_item",
            "struct_item",
            "enum_item",
            "block",
            "match_expression",
            "use_declaration",
        ] {
            assert!(types.is_foldable("rust", kind), "rust foldable kind {kind}");
        }
        // JS set (MT contract; statement_block is the grammar's real name for block_statement).
        for kind in [
            "function_declaration",
            "arrow_function",
            "class_declaration",
            "statement_block",
            "object",
            "array",
        ] {
            assert!(
                types.is_foldable("javascript", kind),
                "js foldable kind {kind}"
            );
        }
        assert!(
            !types.is_foldable("rust", "identifier"),
            "a plain identifier is not foldable"
        );
    }

}
