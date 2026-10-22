---
file_id: handshake-gutter-readme
file_kind: package-readme
updated_at: 2026-10-18
---

<topic id="package-manual">

# handshake_gutter

Existing egui editor-gutter adapter and breakpoint model extracted from Handshake. All three Rust files and their existing inline tests are unchanged. This package depends on `egui = "0.33"` and canonical `handshake_text_mechanics` at `../../handshake_text_mechanics`. It is a GUI adapter, not a framework-independent mechanics package. Host consumers must use the same canonical `TextBuffer` identity.

## API and use

```rust
use handshake_gutter::breakpoints::{BreakpointAction, BreakpointSet};
use handshake_gutter::gutter::{Gutter, GutterConfig};

let mut breakpoints = BreakpointSet::new();
assert_eq!(breakpoints.toggle(4), BreakpointAction::Set);
assert!(breakpoints.contains(4));
let width = Gutter::width_for(100, 8.0, &GutterConfig::default());
assert!(width > 0.0);
```

`breakpoints` exposes `BreakpointSet`, `BreakpointAction` and `BreakpointEvent`. The set stores zero-based buffer lines; toggling returns `Set` or `Clear`, `iter` returns lines in ascending order, and `contains`, `len` and `is_empty` inspect state. `BreakpointEvent` carries file path, line and action. The host owns publication and debug-adapter integration; this package does not start a debugger or create a channel.

`gutter` exposes `Gutter`, `GutterConfig`, `GutterGeometry`, `GutterPaintRow`, `GutterMarker`, `GutterMarkerKind`, `DiagnosticSeverity` and `GutterResponse`. `Gutter::width_for` computes a strip width from line count, measured character width and enabled columns. `Gutter::render` accepts a live egui UI, strip rectangle, painted rows, canonical text buffer, diagnostic markers, breakpoint set, column configuration, geometry and fold-state lookup. It paints the gutter and returns optional fold/breakpoint line clicks. The host applies those clicks after rendering and owns the actual document/fold/breakpoint state.

The caller supplies the same folded/wrapped row order and geometry used by the editor body. Continuation fragments reserve row pitch without duplicate numbers, markers or interactions. `GutterPaintRow::first_fragment` builds an ordinary first-fragment row. `GutterConfig` enables line numbers, folds, diagnostics and breakpoints by default. Diagnostic colors use the highest severity on each line; hover messages retain marker order. Fold glyph selection checks the active egui font and falls back to ASCII. Helpers include `digit_count`, `fold_column_width`, `fold_triangle_glyph`, `diagnostic_tokens_for` and `breakpoint_color`.

`HsDiagnosticTokens` is exported at the crate root and in `gutter`. Its dark/light constructors currently return the same severity and breakpoint colors. Rendering resolves these tokens from the UI visual mode. This extraction preserves the existing token values, rendering, interaction ordering and public types.

## Errors, recovery and proof

These APIs have no `Result` error channel. Host code owns UI lifecycle, valid geometry, row alignment, document routing, persistence and recovery. Re-render from host state after layout changes. Breakpoint state can be cloned before changes and restored by the host. No filesystem writes, processes or native-editor dependency are introduced here.

This assignment produced source only. No compilation, tests, lock generation, GUI launch or runtime checks were executed. The assigned proof owner must run standalone checks and the required GUI behavior proof; retained source tests alone do not prove rendered interaction or layout. Required publication and independent canonical remote reread precede separate immutable host adoption. Existing host source remains the recovery point until adoption passes its own independent proof. No source acceptance or host integration is claimed.

</topic>
