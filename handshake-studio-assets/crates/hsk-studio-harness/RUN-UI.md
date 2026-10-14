# Studio harness desktop viewer: how to launch (manual Operator step)

The window is never opened by tests or agents (CX-SAFE-002). Tests drive the same state
headlessly: `tests/viewer_headless.rs`.

## Build and launch

From the product workspace root, in Git Bash:

```bash
cd "<wt-studio-assets>/handshake-studio-assets"
source "<wt-gov-kernel>/.GOV/docs_repo/wp-studio-module-work/builders/studio-env.sh"
cargo run -p hsk-studio-harness --features desktop --bin studio-harness -- "<path to file>"
```

- `<path to file>`: a PSD/PSB, TIFF, camera raw (DNG, CR2, NEF, ARW, ...), PDF or a native
  `.handshake` package. It is optional; the path field at the top of the window opens files too.
- `--cpu` before the file renders with render-cpu only. By default render-gpu is tried first and
  render-cpu is the fallback (the status panel says which one rendered and why).

## What the window does

- Top bar: path field + Open; Save-as path + Save (refuses the opened source unless
  "overwrite source" is ticked); GPU toggle.
- Left: layer tree. The checkbox shows/hides a layer through the session (chronicle commit,
  new document revision, re-render). Click a name to select it.
- Right: properties of the selected layer, built from hsk-studio-controls descriptors. Name and
  Visible commit; appearance values (opacity, blend mode, ...) are read-only because chronicle
  has no appearance property yet. Below: the import receipt (source format, loss entries,
  not-proven list).
- Centre: the composite. Fit, 1:1, zoom slider, mouse wheel to zoom, drag to pan.
- Bottom: status log (every open, commit, render and save, with failures).

## Known limits (slice 1)

- Layer masks: masked layers make the render fail with a typed composite `Unavailable` message.
- Effects and adjustment layers (raster records) are not applied by the viewer.
- Imported documents leave the blending profile unassigned; the viewer binds it to the RGB
  working profile for display only and says so in the status line (nothing is written).
- render-gpu uses its own device; frames are read back and shown as an egui texture.
- Large documents render synchronously (the window pauses while rendering).
- Screen readers: AccessKit is not enabled (not part of the approved eframe features).
