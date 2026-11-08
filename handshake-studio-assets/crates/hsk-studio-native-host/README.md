# hsk-studio-native-host

Asset-local Vulkan host for Handshake Studio. It is a proof host, never host acceptance (HBR-HOST-001). The chrome of
the `gui` scene is the CPU painter by default (`cpu painter`); `--chrome vulkan` draws the same display lists with
hsk-studio-vulkan's UI pass on the owner's device (`vulkan ui pass`).

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

## Scene gui: the raster frame on a real document (feature `document`)

`--scene gui --document <file>` opens the document with the Viewer on render-cpu and shows the whole raster
workspace of the designer's `mock-raster.html` (SMB-GUI-SPINE-1 built the spine; SMB-GUI-FRAME-1 the frame,
build-order B5): menu bar 28, document tab strip 32, Context Bar 34, Tool Rail 44 over the full main height, the
Browse Dock collapsed to its 32 px icon rail, the Viewport (role Canvas, `studio.viewer.canvas`) with rulers 20 and
the document 1:1 under them, the Inspect Dock 300 with three groups (Properties, Colour, Layers by share 0.30, 0.24,
0.46, the last taking the remainder), the Time & Results Dock collapsed to its 28 px labelled rail, and the status bar
24. Sizes are tokens and layouts.json; the fit ladder (FIT-L1, W1 to W5, H1 to H3, R1, R2, M1, T1) runs in one pure
function, `gui::frame::layout`, used by paint, hit routing, the tree, `studio.dock.state` and the receipt. The chrome
is CPU-painted with kumihimo-ui by default (`cpu painter`, also in the window title, the status bar and the receipt),
or drawn by the Vulkan UI pass with `--chrome vulkan` (below); icons are the designer's set rasterised on the CPU (`kumihimo_ui::icons`); text is Inter through
hsk-studio-ui-text (`text: inter`), or the box stand-in when the font files are missing (`text: box_stand_in`; set
`HSK_STUDIO_UI_FONTS` to a folder holding `Inter-Regular.ttf` and `Inter-Bold.ttf`). Default size 1280x800; proof
runs pass `--size 1920x1080` (the acceptance first screen) or `--size 1280x720`.

The command table (every interactive author_id has one entry; `--invoke` and a pointer release take the same path):

| Control | Outcome | Effect |
|---|---|---|
| `studio.toolgroup.<group>` (19 rail slots, kebab case) | `frame.tool_pick ok` | that slot becomes active; `studio.tool.active` and the identity chip follow (no Viewer tool yet: B11) |
| `studio.tab.<panel>` of a dock group (Inspect Dock; the Browse Dock under FIT-L1) | `frame.tab_select ok` | that tab becomes its group's active tab; the body shows its empty state; the active tab is inert |
| `studio.group.<g>.more` (+N) and its rows `studio.group.<g>.more.<panel>` | `frame.overflow ok`, then `frame.tab_select ok` | +N opens the list of the group's hidden tabs in place of the body; a row makes that tab active and closes the list |
| Layers rows, eyes, blank list space, Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y | `viewer.select`, `layer.set_visible`, `history.undo`, `history.redo` | as the spine built them (one named undo step per eye) |
| every other control: `menu.<title>` (19), `shell.module-switch`, `shell.menu-bar.more`, `studio.presence.operator`, `studio.tab.document`, `studio.tab.document.close`, `studio.tab.new`, `studio.search`, `studio.layout-preset.dropdown`, `studio.layout.panel-menu`, `studio.options.identity`, `studio.options.preset`, `studio.panel.tools.more`, `studio.tool.family-browse`, `studio.colour.proxy`, `studio.rail.left.<panel>`, `studio.rail.left.expand`, `studio.rail.right.*`, `studio.rail.bottom.<panel>`, `studio.rail.bottom.expand`, `studio.group.<g>.disclosure`, `.menu` | `frame.static not_implemented_yet` | none: the control keeps its normal look (lead correction 2026-11-08: B6's greyed items are menus) and carries availability as data, its node value starting with `not_implemented_yet`; a click shows `<name>: not built in this host yet` in the status bar (the readout's value and the receipt carry the code); no popup, no Viewer call |
| an id outside the table | `unknown_author_id` | a typed refusal for `--invoke` (exit 2), a status-bar code for a pointer release |

- The docks are Regions (`studio.dock.left`, `studio.dock.right`, `studio.dock.bottom`); collapsed, each holds an icon
  rail (`studio.rail.left`, `studio.rail.right`, `studio.rail.bottom`, Toolbar). Under FIT-L1 (logical width 2400 and
  up) the Browse Dock opens at 260 as one group of its six panels. The rail chevron's value lists the hidden slots'
  author ids, which stay invocable. Page Up/Down, Home and End scroll the Layers list only while Layers is shown.
- The status bar: the hintline (the active tool's name, then the last outcome in words) at the left; size, zoom,
  revision, the renderer state (`studio.status.chrome`) and the device at the right. Under FIT-S1 the readouts drop in
  the order device, renderer, revision, size, zoom until the outcome fits whole and the hintline keeps 320; a dropped
  readout keeps its node (no box). The rulers carry labels at every major tick; the vertical ruler's are rotated to
  read bottom to top (rasterised by `Text::rotated_label` and drawn as icons of their own id range).
- Observability nodes (unpainted, the last two tree roots): `studio.dock.state` (STU-MDL-106) holds one JSON value
  with fit_table's keys in its order (`window, scale, logical, left, left_state, right, right_state, meter, bottom,
  bottom_state, centre, right_group_heights, rail_pitch, rail_overflow_slots, applied`) plus `groups` and
  `rail_panels`; `studio.tool.active` (STU-MDL-116) holds the active slot's face tool id (`select` at open).
- The receipt's `gui` member gains `frame`: preset `raster`, the fit rules applied, `below_min_size`, `tool_active`,
  the active tabs, `static_outcomes`, `picks`, the rail's shown and hidden slots, and the summed paint statistics of
  the last frame (`prims`, `glyphs`, `glyph_refusals`, `missing_resources`: both must be 0). Notes gain
  `below_min_size` when the logical client is under 1024 x 640 (rects then clamp at zero size; the partition holds).
- `--tree T.json` writes `{"schema":"hsk.studio.native_host.tree@1","revision":N,"tree":{kumihimo.ui.tree@1}}` with
  every panel's roots in paint order: `shell.menu-bar`, `studio.tabstrip`, `studio.options`, `studio.panel.tools`,
  `studio.dock.left`, `studio.viewer.canvas`, `studio.dock.right`, `studio.dock.bottom`, `studio.status`,
  `studio.dock.state`, `studio.tool.active`.
- The Operator's own window (`window --scene gui --document <file>`) has a minimum client size of 1024 x 640 logical
  (size::WINDOW_MIN_W x WINDOW_MIN_H, clamped to the monitor when it is smaller), waits for events, presents only when
  something changed and runs up to 12 h unless `--deadline-s` is given. Open it at a size and scale with
  `--size 1920x1080 --scale 1.25`; without `--scale` it follows its monitor.
- Input is typed: winit events become `HostEvent`s in physical px; scripts and `--invoke` feed the same queue
  (never OS input). Script lines: `move X Y`, `leave`, `down`, `up`, `click X Y`, `wheel N`, `wheelpx N`,
  `key ctrl+shift+z`, `invoke AUTHOR_ID`; `resize W H` and `scale MILLI` apply to the CPU frame only (a window or
  offscreen run refuses them, `script_frame_only`). At most 4096 steps (`script_too_long`).
- `--chrome cpu|vulkan` (gui scene only; anything else is exit 2 `chrome`): `vulkan` draws the frame's lists, from
  the same `GuiFrame::compose` call the CPU painter uses, with `UiPass` (one pipeline from checked-in SPIR-V, glyph
  and icon pages, instanced quads) straight into the pane; no canvas is kept and nothing is copied. The status bar's
  chrome readout says `vulkan ui pass` and the device readout ends with `; chrome vulkan`. Receipt member `ui_pass`:
  SPIR-V sha256 per entry point (sha2 here, checked against hsk-studio-vulkan's `shaders/manifest.json`:
  `manifest_match`), naga 27.0.3, enabled features (`dynamicRendering`), draws, passes, instances, page sides,
  entries, uploads, resets and bytes, image textures, upload submits, VkMemoryRequirements allocation sizes, skip and
  allocation-refusal counters. Offscreen in this mode also paints the CPU reference of the final frame and writes
  `ui_pass.parity` (max channel difference, differing pixels, max difference outside the AA rects; `exact` is true
  only at 0); with `--out X.png` it writes `X.cpu.png` and `X.diff.png` beside it. `--ui-trace T.json` writes one
  record per primitive of the last draw (`hsk.studio.native_host.ui_trace@1`: kind, snapped rect, class, instances,
  pass). In Vulkan mode the receipt's `gui.frame.paint` stays zero (the CPU painter's stats; the pass's counters are
  in `ui_pass`).

```text
studio-vk offscreen --size 1920x1080 --scale 1 --scene gui --document <path>/layered.handshake --out frame.png --tree frame-tree.json --receipt frame.json
studio-vk offscreen --scene gui --document <path>/layered.handshake --invoke studio.toolgroup.paint --invoke studio.tab.masks --invoke menu.file --out edge.png
studio-vk window --quiet --frames 3 --size 1280x720 --scene gui --document <path>/layered.handshake --capture shot.png
studio-vk window --size 1920x1080 --scene gui --document <path>/layered.handshake          # the Operator's own window
studio-vk offscreen --size 1920x1080 --scene gui --document <path>/layered.handshake --chrome vulkan --out vk.png --ui-trace t.json --receipt vk.json
studio-vk window --quiet --frames 3 --size 1280x720 --scene gui --document <path>/layered.handshake --chrome vulkan --capture vk-shot.png
```

Recorded deviations: STU-SHL-110 steps SHL-P-01, SHL-P-08 and SHL-P-10 have not passed and STU-SHL-112 allows no
menu before SHL-P-08, so the 19 menu titles are a static stand-in of the closed list and the docks are fixed;
STU-SHL-004 gives the menu bar to the shell, so here the bar is a host stand-in and the OS frame owns the window
buttons. Known differences from the mock (not defects of this slice): canvas content and zoom (rulers at 100 %, the
mock at 20 %; no fit to window, B11); Context Bar zone controls (B7, B12);
Properties and Colour bodies empty and the Layers group without blend, opacity, lock, the action bar, thumbnails and
tag colours (B10); no L1 tooltip (B6); no model presence chip; this host's status readouts; +N counts measured with
Inter; weight 600 drawn with Inter Bold 700 (no 500 or 600 face in the fixtures); ruler numbers in caption without tabular
figures (no caption tnum token); layer thumbnails are the checker only (named cut D-F9: harness-view exposes no
per-layer pixels cheaply); no window buttons (D-F8: the OS frame provides them). Known gaps: Vulkan chrome as the default (CPU stays the default
and the reference), menus opening, docking and splitters, rename, reorder, lock, opacity and thumbnails, canvas zoom and pan, AccessKit
(the tree file is the model path), a real device loss (only a fake pane sink proves the typed exit 3).

## Inputs and outputs

- PNG: 8-bit RGBA, rows top to bottom. Offscreen reads the pane image back; window mode reads the swapchain image
  back before present (the genuine frame).
- Receipt (`hsk.studio.native_host.receipt@1`, one JSON line on stdout and in `--receipt`): mode, source, device (name,
  type, UUID, driver, API version), `validation` (`on`, `absent` or `off`) with `validation_report` (errors,
  warnings, the first 16 message ids), loader layers seen, enabled and disabled, extent, format,
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

- `src/gui/mod.rs` the GUI frame (router, command table, fit decisions, paint, upload, tree file, receipt),
  `src/gui/frame.rs` the frame layout and fit ladder, `src/gui/preset.rs` the raster preset data,
  `src/gui/chrome.rs` the band views, `src/gui/view.rs` the Layers panel views, `src/script.rs` the scripted step queue, `src/event.rs` the typed HostEvent stream.
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
| 3 | feature_missing, format_unsupported | `--chrome vulkan`: the device lacks dynamicRendering or a format feature the UI pass needs | run with `--chrome cpu` (the default) |
| 3 | ui_pass_limit | `--chrome vulkan`: one frame needed more than 16 pass breaks (glyph page or instance cap) | report the list size; never retried |
| 4 | io | PNG or receipt not written | check the output directory |
| 5 | focus_violation | a quiet run took focus | report it; never retry in a loop |
| 6 | timeout | frame or wall-clock bound passed (default 30 s) | rerun with `--deadline-s`; check the GPU lock |
| 7 | document | feature off, open or render failed | build with `--features document`; check the path |
| 2 | unknown_author_id, script, script_too_long, script_frame_only, gui_only | a bad scripted step or gui flag | fix the script or the author_id (read the tree file) |
| 8 | window | event loop, window or handle failed | rerun; one window run per process |

Validation (item V of SMB-VK-UI-PASS-1): debug builds enable VK_LAYER_KHRONOS_validation when the loader lists it
(Vulkan SDK 1.4.363.1 on this machine), with a VK_EXT_debug_utils messenger that counts errors and warnings; no
environment variable turns it on or off. The first 16 messages go to stderr as `vk-validation <severity> <id>
<number>: <text>` for diagnosis; receipts carry ids and counts only. Every GPU test asserts zero errors when the state
is `on`; `absent` and `off` print `NOT_PROVEN`. Release builds report `off` and validate nothing.
