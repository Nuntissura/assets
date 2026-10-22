# Studio harness desktop viewer: how to launch (manual Operator step)

The window is never opened by tests or agents (CX-SAFE-002). Tests drive the same state
headlessly: `tests/viewer_headless.rs`, `tests/viewer_edit.rs`, `tests/viewer_shared_display.rs`,
`tests/viewer_layers.rs`, `tests/viewer_brush_parity.rs`, `tests/viewer_selection.rs`, `tests/viewer_text.rs`,
`tests/viewer_develop.rs`, `tests/viewer_develop_2.rs`, `tests/viewer_develop_3.rs`, and the shell
(menus, shortcuts, file dialog, recent files, hosted panels): `tests/viewer_shell.rs`.

## Build and launch

From the product workspace root, in Git Bash. The build the Operator should use (every hosted
surface available: documents, audio files, source monitor, mixer, waveform and spectral editors):

```bash
cd "<wt-studio-assets>/handshake-studio-assets"
source "<wt-gov-kernel>/.GOV/docs_repo/wp-studio-module-work/builders/studio-env.sh"
cargo run -p hsk-studio-harness --features desktop,audio-batch --bin studio-harness -- --recent "<recent-files list>" "<path to file>"
```

- `audio-batch` implies `media` (audio files; the source monitor's ProRes / DNxHR / VC-3 / Y4M
  decoders) and `viewer`. The minimal build `--features desktop` still opens every document and
  image; the Source Monitor, Mixer, Waveform Editor and Spectral Display then stay in the
  Workspace menu greyed, their label naming the missing feature.
- Optional extra features: `media-native` (Windows OS decoders for AAC and H.264 through the
  `reel-native-mf-worker` beside the executable), `import-affinity` (library preview route only; the
  window's Open keeps refusing Affinity files with `affinity_native_use_export`). `audio-device` builds,
  but the window opens no audio output (no device grant is wired in the binary).
- `<path to file>`: a PSD/PSB, TIFF, camera raw (DNG, CR2, NEF, ARW, ...), PNG, JPEG, PDF, a native
  `.handshake` package or (with `media`) an audio file. It is optional: File > Open... (Ctrl+O),
  File > Open Recent and the path field at the top of the window open files.
- `--recent LIST`: keeps the recent-files list in the file LIST (read at start when it exists,
  rewritten atomically whenever the list changes; JSON `hsk.studio.harness.recent@1`, at most 10
  paths). Without it the list lives for the session only (the harness has no settings path).
- `--cpu` renders with render-cpu only. By default render-gpu renders on the
  window's own wgpu device straight into the displayed texture (status line "render-gpu shares
  the window's device"); the read-back path (render-gpu own device, then render-cpu) is the
  fallback and the status panel says which one rendered and why.

## Menus and shortcuts (lane SMB-HARNESS-SHELL-1)

Menu titles follow the Studio menu law (spec 14.30: FILE, EDIT, VIEW, WORKSPACE > Panels, HELP).
Lane SMB-HARNESS-SHELL-2: the menu is built from controls menu descriptors (`Viewer::menu_bar()`);
a greyed entry shows its reason line (`INAPPLICABLE_HERE` / `NOT_IN_THIS_DOCUMENT`, reason code,
reason text, remedy) as hover text, and an `INAPPLICABLE_HERE` entry has an enabled `-> <remedy>`
entry right below it that runs the remedy (e.g. `-> Open…` under a greyed Save).

| Menu | Entry | Shortcut | Does |
| --- | --- | --- | --- |
| File | Open... | Ctrl+O | file dialog (folders, then files of the filter: documents and images; audio with `media`; All files) |
| File | Open Recent > (files), Clear Recent | | reopens a recent file (most recent first, 10 kept) |
| File | Save | Ctrl+S | saves the native package at the save path (Save As when there is none) |
| File | Save As... | Ctrl+Shift+S | dialog; a bare name gets `.handshake`; an existing file needs "replace an existing file" |
| File | Export PNG... / Export JPEG... | | dialog; the composite as PNG / JPEG (quality 90) at the chosen path |
| File | Close | Ctrl+W | closes the document and its surfaces (the recent list stays) |
| Edit | Undo / Redo | Ctrl+Z / Ctrl+Y (also Ctrl+Shift+Z) | chronicle undo / redo |
| View | Fit in Window, Actual Size, Zoom In, Zoom Out, Render on GPU, Status Log | | view toggles |
| Workspace > Panels | Layers | F2 | shows / hides the layer panel |
| Workspace > Panels | Properties | F3 | shows / hides properties and the import receipt |
| Workspace > Panels | Develop | F4 | shows / hides the develop panel (open a camera raw / DNG first) |
| Workspace > Panels | Layout Editor | F5 | hosts the layout editor on the open document's pages, or on a new blank one-page document when it has none |
| Workspace > Panels | Program Monitor | F6 | the program monitor on the document's first sequence (an audio file opens its own) |
| Workspace > Panels | Source Monitor | F7 | feature `media`: dialog for a media file, then the source monitor (marks, insert / overwrite) |
| Workspace > Panels | Mixer | F8 | feature `audio-batch`: the mixer of the document's first sequence (sources: the open audio file) |
| Workspace > Panels | Waveform Editor | F9 | feature `audio-batch`: the open audio file's waveform editor (a document must be open: it holds the edit list) |
| Workspace > Panels | Spectral Display | F11 | feature `audio-batch`: the spectral display of the open waveform editor |
| Workspace > Panels | Prototype Player | F12 | dialog for a design file (`.fig`, filter "Design file (.fig)"), then the play window |
| Help > Studio Manual | Open Studio Manual, Search Manual... | | in-window help view: menu, dialog, panel-control and command-line topics; a search field filters them |
| Help | Full Command Index..., Keyboard Shortcut Reference... | | every menu entry with its command id and availability; every shortcut |
| Help > Diagnostics | System Report... | | package version, compiled features, render preference, crate descriptors |
| Help | About Studio | | name, version, build features |
| Help | Help for This Command / Panel / Tool, Domain Guides..., Render Harness..., Visual Debugger, Flight Recorder, Validation Report..., GPU Report..., Accessibility Tree Snapshot... | | shown greyed `NOT_IMPLEMENTED_YET` (the menu never hides an entry) |

A Workspace entry of an open surface shows or hides it. A greyed entry names its missing feature
("Mixer (needs feature audio-batch)") or shows the unmet precondition as hover text; a refused
command writes its reason to the status log. Function keys and Ctrl+O / S / Shift+S / W work while a
text field has focus; Ctrl+Z / Ctrl+Y then belong to the text field. File dialog: click a folder to
enter it, Up for the parent, click a file to put its name in the path field, double-click or the
Open / Save button to accept, Enter in the path field accepts (a typed folder is listed, a typed
full path is used as given), Escape or Cancel closes it.

## Hosted panels (lane SMB-HARNESS-PANELS-1)

Workspace > Panels also hosts, as floating panels, every surface that had only a Viewer API. Each
acts on the layer selected in Layers (single-layer actions) or on its Targets checklist (align,
distribute, transform, brush art); each control calls one existing Viewer method and is one undo
step where that method is. The first line of each panel is its key state (also
`state_json()["shell"]["hosted"][key]["summary"]`), the last line the last result (also said in the
status log).

| Menu | Entry | Shortcut | What it does |
|---|---|---|---|
| Workspace | Appearance | Shift+F2 | fill of the selected layer: Set Fill (colour), No Fill, Clear Appearance, Reduce to Basic |
| Workspace | Stroke | Shift+F3 | stroke panel: colour, weight, cap, join, miter limit, dash, dash offset, align; Apply Stroke / No Stroke |
| Workspace | Swatches | Shift+F4 | New Swatch (name, colour); per swatch To Fill / To Stroke (optional tint %); Detach Fill / Stroke |
| Workspace | Graphic Styles | Shift+F5 | New from Selection, Break Link; per style Apply, Merge, Redefine |
| Workspace | Align & Distribute | Shift+F6 | Targets; align to selection / key object (the selected layer) / artboard; align, distribute edges, spacing (gap or equal) |
| Workspace | Transform | Shift+F7 | Targets; reference point; Move, Rotate, Scale, Reflect, Shear; Transform Each |
| Workspace | Path Operations | Shift+F8 | Offset Path, Outline Stroke, Expand Shape, Reverse Path Direction, Promote to Network; anchor (contour, index): Split at Anchor, Remove Anchor, Convert Anchor, Live Corner; Join |
| Workspace | Masks | Shift+F9 | source layer; Make Clipping Mask (even-odd), Make Mask (alpha / luminance / vector, invert), Release |
| Workspace | Envelope Distort | Shift+F10 | warp (15 styles, bend, distortions), mesh (rows, columns) or top object; fidelity; Make, Release, Expand |
| Workspace | Brushes | Shift+F11 | New Brush from Targets (art, scatter, pattern side tiles; colorization); per brush Apply (scale %); Remove Brush |
| Workspace | Vector Export | Shift+F12 | path; Export SVG / Export PDF (never over an existing file without overwrite) |
| Workspace | Vector Import | Ctrl+Shift+F2 | artwork path (`.ai`) and package path: Import as Editable Vectors (writes the package and opens it) |
| Workspace | Audio Batch | Ctrl+Shift+F3 | feature `audio-batch`: input, output, gain dB, FLAC or AIFF; Add to Queue, Run Queue (runs on the UI thread), receipts |

Models: `Viewer::hosted_action(PanelAction::...)` runs any panel control headlessly (the exact call
the control makes); `hosted_summary(HostedPanel)`; widget names `panel.<key>` (window) and
`panel.<key>.<control>`; `state_json()["shell"]["hosted"]` per panel (open, visible, summary, last,
swatches / styles / brushes / targets / queue) and `state_json()["shell"]["surfaces"]` (layout
editor, program monitor, source monitor, prototype player state).

## What the window does

- Menu bar: File, Edit, View, Workspace, Help (table above).
- Top bar: path field + Open; Save-as path + Save (refuses the opened source unless
  "overwrite source" is ticked); Export PNG / JPEG (next to the save path); GPU toggle.
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
  them, Alt to delete them), Arc, Grid, Polar grid, Spiral (drag; parameters in the row). Lane
  SMB-ILLUSTRATOR-PEN-3: Width (drag outward from a stroke: variable-width profile), Reshape (drag a
  path point; nearby anchors follow), Blob brush (filled strokes that merge with overlapping shapes
  of the same fill), Paint bucket (click a face of the selected layer's vector network to fill it,
  Alt to clear; crossings and T-junctions are split first); Direct drags network vertices; the
  knife also cuts stroke-only paths into open pieces. Every
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

Shell (lane SMB-HARNESS-SHELL-1): `shell_command(ShellCommand::...)` runs any menu entry
(`view::shell::menu_entries()` lists them with their shortcuts), `toggle_panel(Panel)`,
`panel_open` / `panel_visible` / `panel_availability`, `file_dialog()` (set `typed`, `replace`;
`set_filter`, `choose`, `up`), `dialog_accept`, `dialog_cancel`, `recent_files`, `recent_bytes`,
`load_recent`, `layout_editor()`, `close`. `state_json()["shell"]` projects every panel (open,
visible, availability, feature, reason, label, shortcut, command id), the recent list and the open
dialog. Menu entries have the widget names `menu.<command id>` (`menu.studio.document.open`,
`menu.studio.workspace.panel.mixer`, ...), the dialog `dialog.*`.
Lane SMB-HARNESS-SHELL-2: `menu_bar()` is the menu as controls descriptors (`audit()` = the menu
law checks: unique ids, depth, reasons, remedies, no vendor product name); `shell_command` refuses
an entry that is not available with that entry's reason line; `state_json()["shell"]["menu"]`
lists every node (id, author id `menu.<id>`, label, menu path, shortcut, availability record) and
`["help"]` the open help view (page, query, rows); `help_search(text)`, `help_rows(page, query)`,
`help_close()`; `ShellCommand::Help(HelpCommand::...)` opens a help page. With feature `desktop`
every menu node's AccessKit description is `AVAILABLE` or its reason line. The dialog and the help
view carry their window descriptor in `state_json` (`window`).

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
`shape_builder_layers(layers, gesture, delete)`, `stack_vector_layers`, `vector_network`,
`promote_network`, `set_network`, `edit_network(layer, NetworkEdit)`, `network_paint_bucket(layer,
point, fill)`, `set_width_point`, `clear_width`, `width_drag`, `reshape_drag`, `blob_brush_stroke`,
`edit_path(layer,
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

- Layer masks render (tests/viewer_mask_package.rs
  a_session_saved_masked_document_renders_like_a_direct_composite_render) and imported raster
  records (adjustments, effects, smart filters) render through composite's adapter
  (tests/viewer_render_records.rs viewer_frame_renders_the_raster_records_like_a_direct_composite_render);
  there is still no UI to create or edit adjustment layers, layer styles or masks.
- Shell: the layout editor's edits stay in the editor (saving them with the package is a declared
  gap of the layout editor); the program monitor opened on a document sequence has no frame
  decoder bound (transport and scrub run, media clip frames are not resolved); the mixer opened
  without an audio file mixes silence; the waveform editor needs a document open besides the audio
  file; the file dialog filters by extension only (what a file is stays decided by its bytes); no
  drag-and-drop, no multi-document, no docking, no timeline panel.
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
  Networks are Folio `StudioVectorNetwork` records in the `vector` entry; network and
  variable-width layers draw their fill and stroke outline through one colour tile (bilinear,
  Approximate). The `fancy` corner is not built; Illustrator parity of every tool is UNVERIFIED.
