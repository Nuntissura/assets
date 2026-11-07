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
and renders with `Prefer::Cpu`, so the viewer opens no GPU device. The repo fixture is
`crates/hsk-studio-harness/tests/fixtures/demo.handshake`.

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
SW_SHOWMINNOACTIVE and SW_SHOWNOACTIVATE. `focus_changes` counts focus-gained events on its window plus samples where
its own window became the foreground; it must be 0, else exit 5. The Operator's own window switches are reported as
`foreground_changes_any_process` and never counted against the run. Implicit loader layers (overlays) are disabled in
this process only (`VK_LOADER_LAYERS_DISABLE=~implicit~`) unless `--allow-overlays`.

## Navigation

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
| 8 | window | event loop, window or handle failed | rerun; one window run per process |

Validation layers are absent on this machine, so no receipt claims validated Vulkan use.
