---
file_id: handshake-code-structure-readme
file_kind: module-manual
updated_at: 2026-10-18
---

<topic id="api-and-use">

# Code structure

`handshake_code_structure` preserves the existing folding, outline, sticky-scroll and file-symbol mechanics from WP-KERNEL-012. Public modules are `folding`, `outline`, `sticky_scroll` and `symbol_palette`.

Create a `FoldProvider`, then call `provider.compute(&tree, &buffer, language_id)` with the caller's existing tree-sitter tree and canonical `TextBuffer`. It returns fold regions. `FoldSet` owns fold toggles, visibility and visible-line mapping. `OutlineProvider::compute(&tree, &buffer, language_id)` returns outline items with symbol kind, name, line and indentation. Bundled language identifiers are `rust` and `javascript`; unknown languages produce no fold or outline items.

Create a `StickyScroll`, then call `sticky.compute(viewport_top_line, &fold_regions, &buffer)` to obtain literal declaration-line headers for enclosing regions. The default limit is five; truncation keeps the innermost scopes. This module reuses fold regions without parsing again.

Open `SymbolPalette` with the current outline and buffer, update its query, read `results`, move selection with `select_next` or `select_prev`, and call `confirm` to obtain an optional `SymbolPaletteAction::JumpTo`. Symbol flattening is capped by `MAX_FILE_SYMBOLS`, currently 2000. Returned targets use zero-based buffer lines and half-open declaration-line byte ranges. The host applies navigation and renders the controls.

</topic>

<topic id="dependencies-and-recovery">

## Dependencies and recovery

The package uses canonical `../../handshake_text_mechanics` and `tree-sitter = "0.25"`. Existing tests additionally use `tree-sitter-rust = "0.24"` and `tree-sitter-javascript = "0.25"`. Consumers must resolve the same text-mechanics package and compatible tree-sitter identity. The host owns parser setup, parse-tree freshness, rendering, input dispatch, navigation, storage, authorization and diagnostics.

After edits, pass the current buffer and recompute from the current syntax tree. If results are empty, inspect the language identifier and tree supplied by the caller. Line positions from stale trees are clamped to the live buffer; this is not a replacement for updating the tree. Re-open the symbol palette with the current outline when its source changes.

All five Rust files and 38 inline tests are copied unchanged from the assigned product worktree. Only the manifest's sibling text-mechanics path changes for this Notes location. Source extraction has not run a compiler or tests. Standalone locked proof, publication and an independent canonical remote reread must precede separate pinned host adoption.

</topic>