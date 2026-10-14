# Studio harness desktop viewer: how to launch (manual Operator step)

The window is never opened by tests or agents (CX-SAFE-002). Tests drive the same state
headlessly: `tests/viewer_headless.rs`, `tests/viewer_edit.rs`, `tests/viewer_shared_display.rs`.

## Build and launch

From the product workspace root, in Git Bash:

```bash
cd "<wt-studio-assets>/handshake-studio-assets"
source "<wt-gov-kernel>/.GOV/docs_repo/wp-studio-module-work/builders/studio-env.sh"
cargo run -p hsk-studio-harness --features desktop --bin studio-harness -- "<path to file>"
```

- `<path to file>`: a PSD/PSB, TIFF, camera raw (DNG, CR2, NEF, ARW, ...), PNG, JPEG, PDF or a
  native `.handshake` package. It is optional; the path field at the top of the window opens files.
- `--cpu` before the file renders with render-cpu only. By default render-gpu renders on the
  window's own wgpu device straight into the displayed texture (status line "render-gpu shares
  the window's device"); the read-back path (render-gpu own device, then render-cpu) is the
  fallback and the status panel says which one rendered and why.

## What the window does

- Top bar: path field + Open; Save-as path + Save (refuses the opened source unless
  "overwrite source" is ticked); Export PNG / JPEG; GPU toggle.
- Tool row: Hand (drag pans), Rect / Ellipse / Lasso (drag a selection; the combine box chooses
  replace, extend, diminish or intersect; Deselect clears; the selection shows marching ants),
  Brush (select a raster layer in the layer panel, then drag; size, hardness, opacity and colour
  in the row; painting is clipped to the selection and stays inside the layer's existing tiles;
  a tablet's touch force scales the tip size, the mouse is full pressure), Text (not available:
  no transaction creates a text layer yet, the hover text says why). Undo / Redo buttons and
  Ctrl+Z, Ctrl+Shift+Z / Ctrl+Y undo and redo every committed edit (visibility, name, opacity,
  blend mode, paint).
- Left: layer tree. The checkbox shows/hides a layer through the session (chronicle commit,
  new document revision, re-render). Click a name to select it.
- Right: properties of the selected layer, built from hsk-studio-controls descriptors. Name,
  Visible, Opacity and Blend mode commit (undoable); Fill opacity, Clipped and Isolation are
  read-only (no chronicle property). Below: the import receipt (source format, loss entries,
  blending space, not-proven list).
- Centre: the composite. Fit, 1:1, zoom slider, mouse wheel to zoom.
- Bottom: status log (every open, commit, render, undo, paint and save, with failures).
- Screen readers / UI automation: AccessKit is enabled (eframe feature `accesskit`).

## For models (no window)

Every window action is a `hsk_studio_harness::view::Viewer` method: `open`, `set_visible`,
`set_opacity`, `set_blend_mode`, `select_rect`, `select_ellipse`, `select_lasso`,
`set_combine`, `clear_selection`, `set_brush`, `paint(layer_id, samples)`, `undo`, `redo`,
`save`, `export`, `place_text` (typed refusal). `Viewer::state_json()` projects the document,
frame, receipt, run operations and `edit` (tool, combine, brush, selection bounds, undo/redo
history, text tool availability). Widget ids are stable (`view::widget`, tools `tool.<key>`).

## Known limits

- Layer masks: masked layers make the render fail with a typed composite `Unavailable` message.
- Effects and adjustment layers (raster records) are not applied by the viewer.
- Packages converted before the blending-space binding keep an unassigned blending profile; the
  viewer binds it to the RGB working profile for display only and says so (nothing is written).
- Painting creates no tiles (a stroke outside the layer's tiles changes nothing) and no layers.
- Large documents render synchronously (the window pauses while rendering).
