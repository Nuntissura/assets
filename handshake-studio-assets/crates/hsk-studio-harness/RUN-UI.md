# Studio harness desktop viewer: how to launch (manual Operator step)

The window is never opened by tests or agents (CX-SAFE-002). Tests drive the same state
headlessly: `tests/viewer_headless.rs`, `tests/viewer_edit.rs`, `tests/viewer_shared_display.rs`,
`tests/viewer_layers.rs`, `tests/viewer_brush_parity.rs`, `tests/viewer_selection.rs`, `tests/viewer_text.rs`,
`tests/viewer_develop.rs`, `tests/viewer_develop_2.rs`, `tests/viewer_develop_3.rs`.

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
  in the row; painting is clipped to the selection, runs through pigment `masked_replace`, and
  creates 256-pixel tiles where the layer has none; a tablet's touch force scales the tip size,
  the mouse is full pressure), Text (click the canvas: a point-text layer at the click, size
  `text_size_pt`, colour = brush colour, font Ubuntu-Light bundled with egui). Undo / Redo
  buttons and Ctrl+Z, Ctrl+Shift+Z / Ctrl+Y undo and redo every committed edit (visibility, name,
  opacity, blend mode, paint, layer structure, text layers).
- Vector tools (lane SMB-ILLUSTRATOR-PEN): Pen (click = corner anchor, drag = smooth anchor with
  mirrored handles, click the first anchor to close, Enter / Escape / "End path" ends the path;
  new paths have no fill and a stroke in the stroke colour), Direct (drag an anchor, a handle end
  or a segment of a vector layer; Shift snaps to 45 degrees, Alt on a handle breaks the mirroring;
  the pathfinder box applies unite, minus front, intersect, exclude, divide, trim, merge or crop to
  the selected vector layer and the vector layer below it), Rectangle, Rounded, Ellipse shape,
  Polygon, Star, Line (drag; polygon and star from the centre; Shift = square / circle / 45
  degrees; fill, stroke, width, corner radius, sides, points and inner radius in the row). Lane
  SMB-ILLUSTRATOR-PEN-2: Curvature (click points the curve passes through, Alt-click = corner,
  click the first point to close), Pencil (freehand drag fitted and smoothed; fidelity 0-4; ends
  within 4 px close), Knife (drag across filled vector layers: each is cut into separate closed
  regions), Shape builder (drag across overlapping regions of the stack's vector layers to merge
  them, Alt to delete them), Arc, Grid, Polar grid, Spiral (drag; parameters in the row). Every
  vector edit is one Undo step and a path edit keeps the layer (chronicle `vector_paths`). The
  selected vector layer shows its path, anchors and handles.
- Left: layer tree in stack order. Buttons New (empty raster layer above the selection), Dup,
  Del (a group with its contents), Up, Down, Group, Ungroup: each is one undoable chronicle
  transaction (`placement` property). The checkbox shows/hides a layer through the session
  (chronicle commit, new document revision, re-render). Click a name to select it; rename it in
  the properties panel.
- Right, for a camera raw / DNG: the Develop panel (lane SMB-HARNESS-DEVELOP-EXPORT). Sliders
  Exposure, Temperature, Tint, Contrast, Highlights, Shadows, Whites, Blacks, Vibrance, Saturation,
  Clarity, Texture, Dehaze, Sharpening, Luminance NR, Colour NR are fields of hsk-studio-develop's
  recipe; releasing a slider re-develops the raw and commits the new pixels as one undoable step
  (Undo / Ctrl+Z brings back the previous pixels and slider values). "Export developed PNG 16-bit" /
  "Export developed JPEG" write `develop_to_output` with the sRGB-v4 profile embedded next to the
  save path (`.develop.png` / `.develop.jpg`); they export the developed raw only, not other layers.
  Further sections (lane SMB-HARNESS-DEVELOP-2): White balance ("Kelvin white" switches to raw-form
  kelvin / tint sliders, "As-shot white" back), Colour grading (four wheels, blending, balance),
  Point colour ("Add point colour" samples the image centre; shifts and ranges per point; Remove),
  Detail (texture, dehaze, sharpening amount/radius/detail/masking, noise reduction), Local
  corrections ("Add luminance mask" / "Add colour mask": a correction masked by a luminance or
  colour range; amount, the local stack and the mask values; Remove). Saving the document stores
  the develop edit (session entry `develop`); reopening the package restores it.
  Lane SMB-HARNESS-DEVELOP-3: HSL mixer (eight bands), Parametric curve, Grayscale mix (button
  Grayscale / Colour plus band weights), "Add linear gradient", "Add radial gradient", "Add brush"
  corrections with their geometry sliders, per mask Invert and Remove. Every add or remove is its
  own Undo step even before a value moves.
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
`save`, `export`, `new_raster_layer`, `duplicate_layer`, `delete_layer`, `move_layer`,
`shift_layer`, `rename_layer`, `group_layers`, `ungroup`, `layer_action(widget name)`,
`place_text` / `place_text_with_font`, `add_font`, `text_entry`, `set_develop` /
`set_develop_many`, `develop_op`, `set_develop_threads`, `export_develop`, `develop_json`, `develop_edit`,
vector tools `pen_point(x, y, drag, close_radius)`, `pen_end`, `direct_hit`, `direct_drag(from, to,
constraint, break_mirroring, radius)`, `edit_path(layer, PathEdit)` (insert / delete / convert /
move anchor / move handle / bend / scissors / join / reverse), `draw_shape(ShapeKind, a, b, shift)`,
`live_shape`, `set_live_shape`, `expand_shape`, `pathfinder_layers(PathfinderOp, layers)`,
`curvature_point(x, y, corner, close_radius)`, `pencil_stroke(samples)`, `knife_cut(points)`,
`shape_builder_layers(layers, gesture, delete)`, `stack_vector_layers`, `edit_path(layer,
PathEdit::Corner { at, kind: Round | InvertedRound | Chamfer, radius })`,
`offset_path_layer`, `outline_stroke_layer`, `vector_path`, `vector_entry`, `set_vector_tool` (CLI: `studio-harness develop
SOURCE OUT --png|--jpeg [--adjust CONTROL VALUE]...`). `Viewer::state_json()` projects
the document, frame, receipt, run operations and `edit` (tool, combine, brush, selection bounds,
undo/redo history, text tool fonts). Widget ids are stable (`view::widget`, tools `tool.<key>`,
layer buttons `layers.<action>`). The active selection is saved with the document (session entry
`selection`) and restored on open; text stories and styles are the session entry `text`.

## Known limits

- Develop: brush dabs are added through `develop_op(AddBrushDabs)`, not painted on the canvas; a
  saved develop entry that does not decode or that develop refuses is ignored (the status line and
  `develop_json().entry` say why); developed values above 1 (strong white-balance gains or
  exposure on highlights) are clipped for the preview by the renderers' declared display transform
  Clip (the status line says so; no tone-map mode yet); the develop export clamps and counts them
  (`clip` in the export report).

- Layer masks: masked layers make the render fail with a typed composite `Unavailable` message.
- Effects and adjustment layers (raster records) are not applied by the viewer.
- Packages converted before the blending-space binding keep an unassigned blending profile; the
  viewer binds it to the RGB working profile for display only and says so (nothing is written).
- Tiles are created only on the 256-pixel grid (an empty layer or one on that grid); a layer on
  another grid paints inside its existing tiles.
- Text: one line per layer, no line breaking, 1 pt = 1 document pixel; fonts are not stored in
  the package (the family name is; only registered fonts draw). Folio stores no point-text origin
  (SPEC GAP): the origin lives in the `text` entry.
- A duplicated raster layer carries its whole tile list in one chronicle value (64 KiB value
  budget, about 150-200 tiles; larger layers are refused `budget_exceeded`).
- Large documents render synchronously (the window pauses while rendering).
- Vector layers: path records live in the session entry `vector` (saved with the package); a
  path edit is one chronicle `vector_paths` write (the layer keeps its id). Knife, shape builder
  and pathfinder results are polygons (curves flattened); the knife leaves a 0.01 px kerf.
  Vector networks (STU-VEC-001/008), the width, reshape and blob-brush tools and the `fancy`
  corner are not built; Illustrator parity of every tool is UNVERIFIED.
