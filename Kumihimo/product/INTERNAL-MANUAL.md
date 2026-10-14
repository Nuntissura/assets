---
file_id: KUM-INTERNAL-USER-MANUAL-v1
file_kind: internal-user-manual
updated_at: '2026-10-14'
---

<topic id="start-here">

# Kumihimo internal user manual

**Agents and new consumers: start here.** This is the plain-language guide to what the downloaded asset can currently do. It is a documentation file, not a panel shown in the default interface. For API details and reproduction procedures, continue to the [technical reference](README.md). For maintenance instructions, use the [repository entry point](../AGENTS.md).

Kumihimo supplies the arrangement system for an application: nested frames, tabs, grids, docking, tools and saved layouts. Your application supplies the things shown inside those frames. In this guide, that application is the **host**.

This download contains Rust libraries, source and example hosts. It is not a finished standalone editor, browser or desktop window manager. The packages currently use Cargo path dependencies and are marked `publish = false`; downloading the source does not install an executable. The examples let you inspect selected behavior before integrating it into your own application.

The core has no GUI or content engine. Optional desktop and web adapters provide usable presentations. Their existence does not mean every feature works identically on every platform.

</topic>

<topic id="workspace-vocabulary">

## A few useful names

| Name | Meaning |
| --- | --- |
| Workspace | An area that arranges frames. It can be an application root or belong to another frame. |
| Frame | A container placed in a workspace. It can hold tabs and its own child workspace. An internal frame is not automatically an operating-system window. |
| View / tab | One placement of host content inside a frame. Moving it changes its placement, not the underlying document. |
| Resource | A host-owned document, task, scene or other item referenced by one or more permitted views. |
| Provider | The host integration that knows how to display and operate that resource. |
| Tool surface | A toolbar or panel with a placement and an explicit editing target. Its placement and its target are separate. |
| Native popout | A separate operating-system window created for Kumihimo content by a supporting adapter. |

A frame may contain a child workspace, whose frames can contain more workspaces. There is no stock pane count. The host can choose its number of frames, tabs and nesting levels, subject to configured admission limits and available resources. “Twenty frames with thirty tabs each” is an example, not a preset or guarantee of practical performance.

</topic>

<topic id="current-capabilities">

## What is available today

The following are existing building blocks. A host or adapter decides which controls to expose.

| Area | Current capability | Boundary |
| --- | --- | --- |
| Arrangement | Nested ownership; weighted splits and grids; empty grid cells; resizing inputs; docking previews; in-app floating and redocking; equalize, swap, maximize and restore. | The host supplies current bounds, minimum sizes and eligible destinations. |
| Tabs | Selection, labels, pins, groups with colors, ordering, moves, approved closing, retained closed views and reopening. Canonical queries can search and page tab metadata. | Group membership is local to a frame. Pin and close policies may reject an action. |
| Tools | Edge or eligible grid-cell placement, reserved space and separately bound editing targets. | The host implements the actual tool action and reports whether its target is usable. |
| Saved layouts | Validated semantic save/restore, including unavailable-provider placeholders. | Documents, files, live engine state and durable storage belong to the host. |
| Navigation | Optional scrollbar, always-visible minimap, both, or a temporary minimap while scrubbing. Comfortable visible and hit sizes are configurable. | The host supplies document measurements, overview data and confirmed scroll positions. |
| Backgrounds | Optional visual settings and typed media-policy requests; still backgrounds in the desktop sample. | Working video/audio playback and decoder integration are not demonstrated. |
| Resource coordination | Bounded requests, capability negotiation, lifecycle observations, scheduling and declared cache reservations. | A request or hidden tab is not proof that CPU/GPU work stopped or memory was released. |
| Selected external apps | Optional Windows placement, resize and original-placement restore for explicitly bound compatible application windows. | Scoped Windows x64 VLC observations; arbitrary apps, Blender and foreign-window embedding remain unproven. |

The Windows x64 desktop adapter has scoped native-window, rendering and accessibility observations. The Rust/WASM plain-DOM pilot has scoped Chrome observations. The optional rendering experiment has combined real Servo-produced pixels, a 2D provider and depth-tested 3D geometry. These are bounded demonstrations, not blanket compatibility or performance guarantees.

The desktop adapter supplies navigation controls and still-background presentation when those features are selected. The current web pilot supplies arrangement and host task editing; it does not enable the optional navigation or background modules.

Physical pen/touch and assistive hardware, ARM execution, other desktop platforms and other browsers remain unproven. Full Handshake Notes, Studio, Atelier, production Servo browsing and arbitrary external applications are not supplied by these examples. The specification also contains future acceptance targets; a completed work packet does not imply all those targets are complete.

</topic>

<topic id="everyday-workflows">

## Arrange your work

1. **Open or select content.** The host supplies a resource and provider; Kumihimo keeps the view in its frame. A second view of the same resource requires the provider's declared sharing support.
2. **Move a tab or frame.** A tab move relocates that view. A frame move preserves its contained tabs and child workspace. Moving a tab to another frame clears its old frame-local group.
3. **Dock or float.** Eligible edges and cells come from host policy. A preview shows a possible placement; committing applies it. Cancelling a preview leaves the arrangement unchanged. A stale preview must be recreated from current state.
4. **Resize or reorganize.** Split/grid weights divide available space after minimum sizes and tool strips. Equalize preserves structure; swap exchanges two docked frame placements. Maximize temporarily presents one frame while retaining the underlying arrangement for restoration.
5. **Close or reopen.** The host can veto closing, for example to protect unsaved work. Retained closed views can reopen with their original identity and resource reference. Closing a view does not itself dispose the host's document or engine.

A floating frame can cover part of another frame. The model reports its placement and clipping; content interaction, focus and the controls for working around the obstruction come from the adapter and host. Grid holes and floats are available, but the snap helper offers only currently eligible destinations. It does not invent an unlimited snap grid or permit every overlap automatically.

Each displayed child workspace needs bounds updated from its parent's allocated content area. Kumihimo can calculate the arrangement once those inputs are supplied; the host decides how the root relates to its application window or screen.

### Place tools without losing their target

A tool can sit on a frame, view or workspace edge, or an eligible grid cell. Moving it outward does not silently change which document it edits. Default pinned tools hide when their target is not visible and usable, releasing their reserved space. A host can choose a visible-but-disabled policy instead. Focusing the toolbar does not select a different editing target.

This is useful when a Studio toolbar sits beside Notes or a terminal: the host must explicitly choose a compatible Studio target before its commands can operate. Kumihimo does not open an editor merely because somebody clicked an unrelated tool.

### Navigate a large document

The four navigation modes are host settings. In temporary-minimap mode, grabbing the scrollbar thumb begins the gesture and expands a map beside the document. You drag the viewport rectangle over that miniature document; releasing returns to the idle scrollbar controls. If overview information is unavailable, the projection reports a fallback or disabled reason.

The map is supplied by the content provider. Kumihimo does not parse your document to discover its text, images or semantic landmarks, and it does not perform document reflow.

</topic>

<topic id="host-responsibilities">

## What your application supplies

Kumihimo owns arrangement records and validates arrangement commands. The host owns content meaning and effects:

- Files and folders, document edits, text layout, asset import, undo and synchronization.
- Editor/browser/media/graphics engines and the actual rendering of their content.
- Permissions, close approvals, sharing declarations and compatible tool targets.
- Current geometry, scale and content observations; actual input delivery and focus.
- Storage and atomic publication of saved layouts alongside the host's own data.
- Policy for live instances, suspension, unloading, memory and CPU/GPU budgets.

**File dragging:** optional WP018 source integrates bounded host-owned file and internal-task drops. It is in development; manual acceptance remains open; physical browser file transfer and successful native OLE import remain unproven. Frame/header dragging arranges frames; the pilot's separate **Drag task link** control transfers task references or content. No arrangement action moves a physical file. See [file-drop development](#file-drop-development).

**Other applications:** native popouts contain Kumihimo-owned surfaces. Published WP019 positions explicitly selected compatible Windows application windows beside the host and restores their original placement, with scoped VLC observations. Separate WP020 hosting source is in development; manual acceptance remains open; installed-app embedding/input/return remain unproven, including Blender. See [selected external-app tiling](#selected-external-app-tiling) and [hosting development](#foreign-app-hosting-development).

**Rendering speed:** the content and rendering bridge can bound and coalesce requested work and carry lifecycle information. Actual performance improvements and physical resource release require host implementation and measurement. Keeping many logical tabs does not require keeping all their engines alive, but the host must implement that policy.

</topic>

<topic id="choose-modules">

## Choose only the parts you need

| Package | Use it for |
| --- | --- |
| [kumihimo-core](kumihimo-core/Cargo.toml) | Arrangement state and commands. Default features use only the Rust standard library. |
| [kumihimo-layout-json](kumihimo-layout-json/Cargo.toml) | Bounded JSON encoding/decoding of saved semantic layouts. |
| [kumihimo-content](kumihimo-content/Cargo.toml) | A controller owning Core plus typed host-content coordination. Optional `navigation`, `backgrounds` and development `drops`. |
| [kumihimo-egui](kumihimo-egui/Cargo.toml) | Desktop controls; optional `native` runner and `capture`. Development `drops` supplies coordination; `native-drops` adds Windows OLE; `native-apps` separately adds foreign-window hosting. |
| [kumihimo-web](kumihimo-web/Cargo.toml) | Browser adapter/pilot; `wasm` exports bindings, development `drops` selects file/task transfer. |
| [kumihimo-render-bridge](kumihimo-render-bridge/Cargo.toml) | Bounded pixel/identity/lifecycle contracts and the optional `desktop` composition experiment. |
| [kumihimo-external-apps](kumihimo-external-apps/Cargo.toml) | Explicit selected-window placement and recovery. Optional `windows` enables the native backend. |

Add the selected package as a Cargo path dependency relative to your host's manifest. Keep the package's sibling dependencies with it. Use one canonical arrangement owner: when using the content Controller, adapters borrow that Controller instead of maintaining a second arrangement.

The desktop workspace includes a local `accesskit_windows` patch for quiet native accessibility actions. Cargo patches do not propagate through a path dependency. A consuming desktop host requiring that behavior must retain the patched source and apply the workspace-root patch described in the [desktop reference](README.md); without it, quiet native UIA invocation is unsupported.

The core and content packages do not require Servo or WGPU. Choose the graphics experiment separately rather than pulling browser/graphics infrastructure into every host.

</topic>

<topic id="try-the-examples" ingestable="true">

## Try the examples

Run these commands from the **Kumihimo project folder**, which contains `product/` and this repository's README. They require Rust/Cargo 1.91 or newer, the target's linker and the selected dependencies. `--offline` assumes those dependencies are already cached. These commands are documented entry points, not a claim that your machine has been prepared or that generated binaries are included in the download.

For models maintaining this repository, follow its session-start and build preparation rules before compilation, and keep the single Cargo lane and reusable target. Consumers arrange their own build environment. Do not start every example merely to inspect the documentation.

### Quiet arrangement example

```powershell
cargo run --manifest-path product/Cargo.toml -p kumihimo-core --example workspace --target-dir .local/cargo-target --locked --offline -j 2
```

This prints arrangement identities, generation and geometry. It opens no GUI. The source is [workspace.rs](kumihimo-core/examples/workspace.rs).

### Visible desktop sample

```powershell
cargo run --manifest-path product/Cargo.toml -p kumihimo-egui --all-features --example desktop_host --target-dir .local/cargo-target --locked --offline -j 2 -- --interactive --frames 3 --tabs 3
```

`--interactive` explicitly requests a visible window. The two counts are adjustable sample inputs. This example uses sample providers, not your installed editors. For quiet capture or native diagnostics, use the separately documented `capture_host` and `native_probe` paths in the [technical reference](README.md). Models must not substitute visible interaction for those quiet automation paths.

### Browser workspace pilot

Prerequisites include the Rust `wasm32-unknown-unknown` target, cached dependencies, Python, PowerShell and a verified **wasm-bindgen CLI exactly 0.2.128** configured through `KUMIHIMO_WASM_BINDGEN`.

```powershell
& product/kumihimo-web/tools/build-web.ps1 -CargoTargetDir .local/cargo-target -WasmBindgen $env:KUMIHIMO_WASM_BINDGEN
python product/kumihimo-web/tools/serve.py --pkg .local/web/pkg --port 0
```

The first script currently builds `wasm,drops` and generates matching JS/WASM under `.local/web/pkg`; `-Features wasm` omits drop integration. The server prints its owned loopback URL; open that printed URL. If you already have a matching selected WASM build, the script's `-SkipBuild` option reuses it. Do not use that option to disguise a missing or stale build.

The pilot has Cards, Details, Depth and live-record-budget inputs. Drag frame headers, select tabs, and use the labelled arrangement controls to float, redock, resize, maximize or move a tab into another frame. Escape cancels an uncommitted preview. The separate host task editor changes task meaning. **Save layout + tasks** and **Restore saved** use separate selected browser-storage keys. Native-style browser popup migration is unsupported.

### Browser and graphics experiment

The optional bridge has demonstrated two Linux x64 Servo processes under WSL supplying copied pixels to Windows x64 DX12 composition, beside 2D and 3D content. This is an experiment with a host-owned process and capture protocol, not a ready embedded browser. Native Windows Servo, zero-copy sharing, video/YouTube/audio and raw engine-context popout transfer are unproven or unsupported.

Its launch, network isolation, receipt and cleanup requirements are substantial; use the [exact experiment procedure](README.md#optional-browser-and-graphics-experiment). Do not start Servo from this overview or interpret hiding a view as suspending it.

</topic>

<topic id="selected-external-app-tiling" ingestable="true">

## Selected external-app tiling

Choose `kumihimo-external-apps` with its `windows` feature when your host needs to arrange a compatible installed application's window beside its workspace. The host explicitly supplies the selected PID/HWND; the package launches no app and never selects a desktop set. Bind while the window is visible and in normal state. Core's canonical geometry supplies the allocation, while the native adapter checks minimum sizing, work area and DPI. Unsupported states, sizes and monitor/scale combinations return a reason.

The live binding retains the process handle/start identity and a window-scoped property token; each effect rechecks both lifetime and ownership. Saved recovery records contain semantic references and original/pending placement, never raw handles or PIDs. `restore` returns the selected window to its captured original normal placement. A successful OS request alone is insufficient: inspect the receipt's observed placement, errors and `pending` field. `Unknown` or pending work must be reconciled before detach or another effect; a recovery record cannot erase that obligation. After restart or a missing window, explicitly reselect the intended instance before applying its matching recovery record.

The [technical reference](README.md#optional-selected-external-app-tiling) provides the Rust API and exact Cargo entry. Its `selected_window` example takes `--pid PID --hwnd HWND --semantic REF`, with optional `--created FILETIME`, and reads one JSON command per stdin line:

```jsonl
{"op":"inspect"}
{"op":"tile","x":30,"y":30,"width":900,"height":600}
{"op":"restore"}
{"op":"recovery"}
{"op":"exit"}
```

Use tile bounds that fit the reported capabilities, read each response, then verify restoration before ending the session. `recover` accepts the exact saved object as `{"op":"recover","record":...}`. Exit is refused while dispatch remains pending; EOF with pending work emits recovery and exits with failure. Your host owns startup and cleanup of its own selected instance; the example does not stop applications.

Published WP019 used one fresh owned VLC instance on 2026-10-14 Windows x64: tile to 900×600, resize to 1000×650 and restore matched independent OS bounds and inspected VLC captures. The sampled foreground window and observed unselected window stayed unchanged; the proof controller cleaned up its owned app/CLI. Blender, arbitrary application compatibility, playback and foreign-window embedding remain unproven.

</topic>

<topic id="file-drop-development" ingestable="true">

## File-drop development

WP018 is in development; manual acceptance remains open. The host owns payload tokens, permission, file IO, task edits and undo; Core receives bounded metadata and canonical destination identity. Hover reads no file contents. Release validates current selection/generation/epoch; delayed reads must revalidate before completion. Browser Open/Copy replace task text, Import appends, and internal task Link adds a reference. Move is denied. Eleven inspected headless cases cover trusted frame/tab/cancellation and task-link gestures plus synthetic untrusted File/DataTransfer events through production listeners and Rust host reads. Physical OS-to-browser file dragging is **NOT_PROVEN**.

The Windows fixture host opts into `native-drops`, supplies two independently backed text frames, imports only `.txt`/`.md` (eight files, 8 KiB total), and exposes **Undo last file import**. Undo rejects if the imported document was edited afterwards, preserving those later edits. Source files remain untouched. Start only with an agreed controlled desktop:

```powershell
cargo run --manifest-path product/Cargo.toml -p kumihimo-egui --features native-drops --example file_drop_host --target-dir .local/cargo-target --locked --offline -j 2 -- --interactive
```

`--output PATH` selects observations. **`--observe-host` records imported document text**, so use explicit fixtures; default snapshots omit payload. The [guarded OLE source](kumihimo-egui/examples/native_drop_source.rs) takes `--fixture PATH --receiver-hwnd DECIMAL --receiver-pid PID`, requires the actual cursor over that live registered receiver, and cancels on identity change or its three-second limit. It needs controlled desktop agreement. Dead-receiver denial is observed; successful native transfer/import remains unproven. Read receipts and independently inspect the exact destination; compilation or a COPY result alone does not prove the import. See [protocol details](README.md#file-drop-integration-in-development).

</topic>

<topic id="foreign-app-hosting-development" ingestable="true">

## Foreign-app hosting development

WP020 is in development; manual acceptance remains open. `native-apps` selects a separate Windows child route; it is optional independently of `native-drops`. Source checks and memory fixtures do not prove installed-app attach, resize, tab hiding, input or desktop return. GPU native-child sharing remains unsupported.

The [sample](kumihimo-egui/examples/foreign_app_host.rs) uses a distinct `selected-installed-app` resource alongside DemoHost. Its full-width app frame contains **Selected application** and **Host document (tab visibility)** tabs; sibling panes contain detach controls. Supply a fresh selected instance; it spawns/closes no app and injects no input. `--output PATH` separates observations. `--timeout-seconds 1..=120` selects a diagnostic/capture budget, not an interactive session deadline. Run only on an agreed controlled desktop:

```powershell
cargo run --manifest-path product/Cargo.toml -p kumihimo-egui --features native-apps --example foreign_app_host --target-dir .local/cargo-target --locked --offline -j 2 -- --interactive --pid $SelectedPid --hwnd $SelectedHwnd
```

HWND is decimal. Hosting requires exact app/parent DPI contexts, normal supported window state, lifetime-validated identities, an owned parent and a current fully clipped content slot. Another parent, partial clipping, floats or unsupported menus/styles reject. Actual host popup/preview overlays require observed child hiding before painting. Cross-process keyboard/focus, app menus/dialogs and fullscreen are denied; use the sibling pane's **Detach application to desktop** control for that work. Returning must restore original parent/styles/visibility/bounds; the application remains user-owned.

Opted-in hosts call `native::run_with_recovery`; legacy `run` rejects app opt-in before effects. Unresolved detach prevents parent destruction and keeps the event loop alive. Pending/stale completion is `Unknown`; automatic teardown attempts once, then needs explicit retry. Exceptional return supplies `NativeRunResult::Retained`: on the original UI thread, inspect, reconcile, explicitly retry detach and finish only after cleanup is observed. Dropped unresolved owners remain retrievable by `recovery_id()` through `RetainedNativeOwner::<Host>::retrieve(id)` and `RecoveryBank::current_thread().inspect()`. Active/retained owners share default capacity eight (configurable 1–64 only while empty); full/conflicting admission rejects. Reconcile before ending that thread/process; saved semantic recovery cannot erase a live obligation.

The sample's retained-owner stdin commands are `inspect`, `retry`, `finish`; EOF keeps reconciling rather than exiting with an attached app. Independently verify restoration and, after owner release, parent absence. See [hosting API and constraints](README.md#foreign-window-hosting-in-development). This source entry does not complete WP020.

</topic>

<topic id="save-and-recover">

## Save, inspect and recover

A saved layout records semantic arrangement and host resource references, not native window handles, texture handles or document contents. Restore validates a detached candidate before replacing the live arrangement. Missing providers remain identifiable placeholders. The host decides where bytes live and how its documents and layout are saved together.

| What happened | What to inspect / do |
| --- | --- |
| A move or preview is rejected | Read the reported reason, current generation, destination eligibility, minimum sizes and pin/target policy. Generate new intent from current state. |
| A provider is unavailable | Preserve the placeholder and saved reference; make the provider available through the host, then reconcile its binding. |
| Saved data is malformed or newer than supported | Keep the current valid state. Inspect the candidate/schema error; do not overwrite good storage with a failed restore. |
| Browser storage refuses a save | Continue using the current workspace. Inspect the storage error and retry only after resolving its cause. |
| A native popout loses its return destination | Inspect native bindings and the semantic return/recovery result. The host may need a new eligible destination; do not discard surviving content. |
| A host effect times out or reports `Unknown` | Inspect its exact pending request and host state. Reconcile that old operation before issuing a conflicting or destructive request. |
| WASM startup fails | Inspect the build manifest, exact binding version, served package and constructor/route error. Compilation alone does not prove browser startup. |

Resource reservations describe declared obligations. They do not measure physical memory. “Requested unload,” “confirmed unload,” “hidden” and “unknown” have different meanings. Keep outstanding cleanup obligations until the host confirms the exact effect; do not blindly resend requests or stop unrelated processes.

Diagnostics exclude opaque input/asset payloads in the content controller. They still need appropriate host handling. Native sample captures and manifests, browser events and storage observations, and rendering-bridge receipts provide different evidence; use the one belonging to the failing layer.

</topic>

<topic id="manual-extension-acceptance" ingestable="true">

## Manual extension acceptance

These tests exercise WP018/WP020 development code. They do not mark either packet complete. The operator launches windows and supplies all mouse/keyboard actions; the agent can inspect resulting files in the background. Run commands from the Kumihimo project root. The prepared executables live under ignored `.local`; a fresh checkout needs them built first. The fixture files are included in `product/kumihimo-egui/fixtures/file-drop`.

### Native file transfer

Launch the prepared sample yourself:

```powershell
& .local/cargo-target/debug/examples/file_drop_host.exe --interactive --observe-host --output .local/manual-acceptance/native-drop
```

In Explorer, open `product/kumihimo-egui/fixtures/file-drop`. The prepared files are `alpha.txt` (22 bytes), `beta.md` (21 bytes), `unsupported.bin` (26 bytes), and `oversize.txt` (8193 bytes).

1. Drag `alpha.txt` over **Import target 1 · document 1**. Before releasing, confirm the highlighted pane and status name the destination and **Import**. Release: `KUMIHIMO-NATIVE-ALPHA` must append once to that document; the source remains in Explorer.
2. Drag `beta.md` into **Import target 2 · document 1**, inside the other frame's nested pane. Only that document gains `KUMIHIMO-NATIVE-BETA`; target 1 retains its alpha marker.
3. Click **Undo last file import**. Target 2 returns to its original text; target 1 retains alpha. Import beta again so the final snapshot retains both distinct results.
4. Hover alpha over either destination and press Escape before release. Text must remain unchanged.
5. Drop `unsupported.bin`, then `oversize.txt`. Each must report rejection and preserve both documents. Do not infer success from the Windows drag cursor alone.
6. Drag a frame by its title. This must rearrange the frame, without importing content or moving a physical fixture file.
7. Close the sample normally using its window close button. Record the outcomes of steps 1–6, including the visible frame/view IDs. The final `.local/manual-acceptance/native-drop/native-manifest.json` contains `host_snapshot.documents`, `drop_status`, and `receipts`. Only explicit fixture text should be used with `--observe-host`.

Capture or describe the two populated panes before closing. The automatic startup PNG does not prove post-drop content.

### Physical browser file transfer

Start the prepared browser package server yourself; open the URL it prints:

```powershell
python product/kumihimo-web/tools/serve.py
```

Drag alpha and beta from Explorer into two different task cards, without modifier keys. Each preview must name **import**, and only the corresponding task body must gain its marker once. With a fresh task, hold Shift while dropping alpha: **open** replaces the body. Hold Ctrl while dropping beta: **copy** replaces that destination body and retains the source file. Hover and Escape, unsupported extension, and oversize file must preserve the current bodies. Dragging the frame title must still arrange frames; **Drag task link** handles internal task transfers. Record the destination task IDs, displayed status and resulting bodies. These operator gestures supply the physical-input proof missing from the headless tests. Stop only the server you started, with Ctrl+C in its own terminal.

### Installed-app hosting

VLC is the already inspected fallback; this does not certify Blender or arbitrary applications. Start a fresh VLC instance yourself, with configuration ignored and single-instance forwarding disabled:

```powershell
$vlcPath = Join-Path $env:ProgramFiles 'VideoLAN/VLC/vlc.exe'
Get-Item -LiteralPath $vlcPath
$selectedApp = Start-Process -FilePath $vlcPath -ArgumentList '--no-one-instance','--ignore-config','--qt-auto-raise=0','--no-qt-system-tray','--no-qt-privacy-ask','--no-qt-updates-notif' -PassThru
[void]$selectedApp.WaitForInputIdle(10000)
$selectedApp.Refresh()
$selectedHwnd = $selectedApp.MainWindowHandle.ToInt64()
if ($selectedHwnd -eq 0) { throw 'Wait for the fresh VLC main window, then refresh its handle before proceeding.' }
$selectedApp.Id
$selectedHwnd
python .local/manual-acceptance/observe-window.py --pid $selectedApp.Id --hwnd $selectedHwnd --output .local/manual-acceptance/hosting-before.json
& .local/cargo-target/debug/examples/foreign_app_host.exe --interactive --pid $selectedApp.Id --hwnd $selectedHwnd --output .local/manual-acceptance/hosting
```

The sample may reject DPI/style compatibility. Record the exact diagnostic if VLC stays on the desktop; that is a rejection outcome, not successful hosting.

For a successful attach:

1. Confirm actual VLC pixels appear inside **Selected application**. Click a VLC control and record its visible effect. Test keyboard input after clicking inside VLC; for example, press Ctrl+L and confirm the playlist appears, then press it again ([VideoLAN shortcut implementation](https://mailman.videolan.org/pipermail/vlc-commits/2024-April/069687.html)). A denied or ineffective keyboard action remains a failed/open acceptance case; detached input cannot replace this check.
2. Resize the host and the horizontal divider below the application. VLC must follow the allocated pane without covering tabs, tools or sibling controls.
3. Select **Host document (tab visibility)**. VLC must disappear from that pane without exiting. Select **Selected application** again; VLC must reappear usable.
4. Click **Detach application to desktop** in a sibling pane. VLC must return to a usable desktop window. Test its menu/dialog there; the sample declares that fallback rather than promising embedded menu/focus support.
5. Close the host normally. VLC must remain alive and usable. After the host command returns, run the observer again with the same app PID/HWND and output `hosting-after.json`; compare parent and styles with `hosting-before.json`.
6. Run a second hosting session with the same fresh VLC instance and a separate `--output .local/manual-acceptance/hosting-close` directory. Close the host while VLC is still attached. Normal teardown must return VLC to the desktop. Observe again after the host exits.
7. After both return tests, close that fresh VLC window yourself. Run the hosting command once more with its old PID/HWND. It must reject stale identity before opening a host window. Preserve the exact error. This is the real stale-application failure case; memory fixtures cannot replace it.

In a second PowerShell terminal, set `$selectedPid` and `$selectedHwnd` to the two printed numbers and use this read-only command during attached, resized, hidden and returned stages, changing the output filename each time:

```powershell
python .local/manual-acceptance/observe-window.py --pid $selectedPid --hwnd $selectedHwnd --output .local/manual-acceptance/hosting-attached.json
```

The observer records independent OS parent, physical screen/client bounds, visibility, styles and DPI without injecting input. Attached parent must belong to the test host; hidden must have `visible=false`; returned parent/styles must match the baseline. Preserve screenshots or precise observations of actual VLC pixels and input effects. The host manifest alone is not independent OS evidence.

After the host command returns, independently check that the parent recorded during attachment no longer exists:

```powershell
$attachedObservation = Get-Content .local/manual-acceptance/hosting-attached.json -Raw | ConvertFrom-Json
python .local/manual-acceptance/observe-window.py --pid $attachedObservation.parent.pid --hwnd $attachedObservation.app.parent --output .local/manual-acceptance/hosting-parent-after.json
```

`hosting-parent-after.json` must report `exists=false`. Repeat for the second session's recorded parent; a manifest cleanup field alone cannot replace this check.

If the console reports retained recovery, keep it open and use `inspect`, then an explicit `retry`, then `finish` after return succeeds. Report the diagnostic if recovery does not finish. Close the fresh VLC window yourself after it has returned; do not use an existing working VLC session for these tests. Unresolved native ownership is not a successful teardown. WP021 remains pending the accepted and published predecessor packets.

</topic>

<topic id="agents-without-context">

## For an agent with no prior context

1. Start with this manual and identify whether you are using a host application, trying an example, or maintaining the asset. Repository maintenance follows [AGENTS.md](../AGENTS.md); consuming the asset does not require understanding its work-packet history.
2. Inspect the selected adapter's actual capabilities. A core API or requested effect is not proof of GUI behavior, successful content IO or platform compatibility.
3. Read canonical state before new intent. In Rust, use `Core::snapshot()` or the owning Controller. Mutations use identity, actor, operation ID and expected generation; the host serializes arrangement changes and allocates operations.
4. In the running web pilot, `window.kumihimo` exposes readiness, `snapshot()`, `hostSnapshot()`, `route(request)`, bounded events and selected storage. Route names include `snapshot`, `host_snapshot`, `preview`, `cancel_preview`, `command`, `host_command`, `input`, `save` and `load`. IDs, operations and generations use decimal strings. Consult the [web reference](README.md#browser-adapter-and-portal-style-pilot) for request shapes.
5. Use stable control IDs and structured observations where available. In the desktop adapter, `ControlRecord` and `FrameReport` identify controls, bounds, rejections and post-action generation. Open captured images when judging visible layout; records alone do not establish readability.
6. Retry an uncertain command with its **exact original envelope** to reconcile a retained receipt. Corrected intent needs a new operation and current generation. A fresh incarnation does not erase old host cleanup obligations, and replay is not guaranteed across restarts.
7. Keep automation quiet and bounded. Visible desktop startup is an explicit opt-in. Never kill another application's process to make an example or recovery appear successful.

For detailed data types, examples and error semantics, use [product/README.md](README.md). For accepted future direction and current coverage, use the [master specification](../repo-governance/specification/current/spec-modules/master-spec.md) and [roadmap](../repo-governance/specification/current/spec-modules/roadmap.md). Treat those future targets separately from the inspected implementation.

</topic>
