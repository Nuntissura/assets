# hsk-studio-native-host

Asset-local Vulkan host for Handshake Studio. It is a proof host, never host acceptance (HBR-HOST-001). Chrome is the
CPU painter; the Vulkan UI pass is pending (no shader, pipeline or descriptor yet).

## Purpose

Show a real document in a real pure-Vulkan window, and give agents an offscreen capture with no window. Window and
event loop: winit, in this crate only, with a typed `HostEvent` stream past it. Device: the transient owner in
`hsk-studio-vulkan` (all Vulkan `unsafe` lives there), one device per process.

## Run

Build and test only through the Studio wrappers: `lane-check.sh`, or a shell that sourced `builders/studio-env.sh`
with your `STUDIO_OWNER`. Those take the GPU test lock for this crate. A binary run outside cargo goes through
`studio_gpu_run <exe> [args]`.

```text
studio-vk offscreen --size 256x192 --out frame.png --receipt frame.json          # clear colour, no window
studio-vk offscreen --scene painted --size 320x200 --out card.png                # CPU-painted test card
studio-vk offscreen --document <path>/demo.handshake --out doc.png               # feature document
studio-vk window --quiet --frames 3 --capture shot.png --receipt shot.json       # agents and tests
studio-vk window --quiet --frames 4 --exercise --scene painted --capture e.png   # resize, minimise, restore
studio-vk window --document <path>/demo.handshake                                # the Operator's own window
studio-vk help
```

The `document` feature (`--features document`) links harness-view's `Viewer` (and through it egui, eframe and wgpu)
and renders with `Prefer::Cpu`, so the viewer opens no GPU device. Any file `Viewer::open` accepts works (a PNG goes
through import-raster). Known gap: the repo's `crates/hsk-studio-harness/tests/fixtures/demo.handshake` is an edit
fixture with no working colour profile and no visible content; the viewer refuses it with `no_working_profile`
(harness-view `render.rs`, `view_snapshot`) and the host exits 7 with that code. Other open refusals keep the
viewer's own code (`not_found` for a missing file).

## Scene gui: the first panel on a real document (feature `document`)

`--scene gui --document <file>` opens the document with the Viewer on render-cpu and shows the frame: the Viewport
(the document over its transparency grid, 1:1 at scale 1, centred and clipped; zoom and pan are later), a right dock
300 wide with one group (header 26 with the Layers tab, then the Layers panel body), and a 24 px status bar. The
chrome is CPU-painted with kumihimo-ui (`cpu painter, Vulkan UI pass pending`, also in the window title, the status
bar and the receipt) and copied into the pane; text is Inter through hsk-studio-ui-text (`text: inter`), or the box
stand-in when the font files are missing (`text: box_stand_in`; set `HSK_STUDIO_UI_FONTS` to a folder holding
`Inter-Regular.ttf` and `Inter-Bold.ttf`). Default size 1280x800.

- Layers rows (`studio.panel.layers.row.<layer-id lowercased>`): a click selects (`viewer.select`); a click on the eye
  (`<row>.visibility`) shows or hides the layer as one named undo step (`layer.set_visible` through
  `Viewer::set_visible`: undo label `panel_set visible <layer_id>`, command `layer.panel_set`); a click on blank list
  space deselects; Ctrl+Z undoes, Ctrl+Shift+Z and Ctrl+Y redo (chords PROVISIONAL). The wheel over the list scrolls
  it; Page Up/Down, Home and End scroll too. Only the rows that fit are built; the list value says `rows F-L of T`.
- Input is typed: winit events become `HostEvent`s in physical px; scripts and `--invoke` feed the same queue
  (never OS input). Script lines: `move X Y`, `leave`, `down`, `up`, `click X Y`, `wheel N`, `wheelpx N`,
  `key ctrl+shift+z`, `invoke AUTHOR_ID`; `resize W H` and `scale MILLI` apply to the CPU frame only (a window or
  offscreen run refuses them, `script_frame_only`). At most 4096 steps (`script_too_long`).
- `--tree T.json` writes `{"schema":"hsk.studio.native_host.tree@1","revision":N,"tree":{kumihimo.ui.tree@1}}`, one
  tree whose roots are `studio.viewer.raster`, `studio.dock.right` and `studio.status`. The receipt gains `gui`:
  revision, rows shown and total, builds, paints, uploads, idle frames, commands, refusals and the last command code.
- `--scale 1.25` fixes the UI scale (tests and captures); without it the window follows its monitor scale.
- The Operator's own window (`window --scene gui --document <file>`) waits for events, presents only when something
  changed, keeps the Viewport at least 360 x 240 (minimum client size 660 x 264 logical) and runs up to 12 h unless
  `--deadline-s` is given.

```text
studio-vk offscreen --scene gui --document <path>/layered.handshake --invoke studio.panel.layers.row.<id>.visibility --out gui.png --tree gui-tree.json --receipt gui.json
studio-vk window --quiet --frames 3 --scene gui --document <path>/layered.handshake --script s.txt --capture shot.png
studio-vk window --scene gui --document <path>/layered.handshake                  # the Operator's own window
```

Known gaps: the Vulkan UI pass (shaders, glyph atlas), menus and the other B5 regions, docking and splitters,
rename, reorder, lock, opacity and thumbnails, canvas zoom and pan, AccessKit (the tree file is the model path), a
real device loss (only a fake pane sink proves the typed exit 3), the designer's icon set (gallery stand-in icons).

## Inputs and outputs

- PNG: 8-bit RGBA, rows top to bottom. Offscreen reads the pane image back; window mode reads the swapchain image
  back before present (the genuine frame).
- Receipt (`hsk.studio.native_host.receipt@1`, one JSON line on stdout and in `--receipt`): mode, source, device (name,
  type, UUID, driver, API version), `validation: absent`, loader layers seen and disabled, extent, format,
  `frame_sha256`, focus counters, swapchain state, counters and transitions, DPI, chrome, document facts.
- The same scene gives the same `frame_sha256` offscreen and in the window.

## Quiet rules (HBR-QUIET)

`window --quiet` is created hidden and inactive, gets WS_EX_NOACTIVATE and WS_EX_TOOLWINDOW before its first show
(SW_SHOWNOACTIVATE), sits at the bottom of the Z order left of every monitor, and minimises and restores with
SW_SHOWMINNOACTIVE and SW_SHOWNOACTIVATE. winit's show rewrites the extended style, so the quiet style is applied
again after the show and checked every frame (`style_restores`); the receipt reads the style from the live window at
the end of the run. A watcher thread samples the foreground window about every millisecond for the whole run.
`focus_changes` counts focus-gained events on its window plus samples where its own window became the foreground; it
must be 0, else exit 5. The Operator's own window switches are reported as
`foreground_changes_any_process` and never counted against the run. Implicit loader layers (overlays) are disabled in
this process only (`VK_LOADER_LAYERS_DISABLE=~implicit~`) unless `--allow-overlays`.

## Navigation

- `src/gui/mod.rs` the GUI frame (regions, router, command table, paint, upload, tree file), `src/gui/view.rs` the
  panel view functions, `src/script.rs` the scripted step queue, `src/event.rs` the typed HostEvent stream.
- `src/cli.rs` arguments, `src/offscreen.rs` offscreen path, `src/window.rs` winit app and exercise script,
  `src/scene.rs` clear, CPU painter and document flattening, `src/capture.rs` PNG, hash and receipt JSON,
  `src/win32.rs` the only `unsafe` here (window style and foreground queries), `src/diag.rs` observe terminal frames.
- `hsk-studio-vulkan`: `Owner` (transient owner, panes), `Presenter` (surface, swapchain, blit), `machine` (swapchain
  state machine).

## Failure modes and recovery

| Exit | Code | Meaning | Recovery |
|---|---|---|---|
| 2 | usage | unknown flag, bad size, quiet without `--frames` | fix the arguments; no window was opened |
| 3 | vulkan_unavailable, api_below_baseline, no_suitable_device, ... | no Vulkan 1.3 device or a Vulkan refusal | install a Vulkan 1.3 driver; nothing falls back |
| 3 | owner_already_open | a second device in one process | one owner per process |
| 3 | device_lost, surface_unrecoverable | the device or surface did not come back | rerun; owner rebind is not built yet |
| 4 | io | PNG or receipt not written | check the output directory |
| 5 | focus_violation | a quiet run took focus | report it; never retry in a loop |
| 6 | timeout | frame or wall-clock bound passed (default 30 s) | rerun with `--deadline-s`; check the GPU lock |
| 7 | document | feature off, open or render failed | build with `--features document`; check the path |
| 2 | unknown_author_id, script, script_too_long, script_frame_only, gui_only | a bad scripted step or gui flag | fix the script or the author_id (read the tree file) |
| 8 | window | event loop, window or handle failed | rerun; one window run per process |

Validation layers are absent on this machine, so no receipt claims validated Vulkan use.
