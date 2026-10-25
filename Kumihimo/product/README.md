---
file_id: KUM-PRODUCT-MANUAL-v1
file_kind: product-and-model-manual
updated_at: '2026-10-25'
---

<topic id="purpose-and-boundaries">

# Portable ownership core

> **NEW USERS AND AGENTS: [START WITH THE INTERNAL USER MANUAL](INTERNAL-MANUAL.md).** This file is the detailed API and integration reference. The internal manual explains current capabilities, host responsibilities, examples and recovery without requiring the spec. Neither document opens a default GUI panel.

`kumihimo-core` 0.3 is independently usable with std-only default features. It owns root workspaces, frames, frame-owned child workspaces, ordered open and retained closed views, groups, host resource references, tool surfaces and flat split/grid packing. Geometry is a pure projection of canonical inputs. Flat records avoid recursively owned trees. Optional `serde` supports the schema-1 saved DTO; independent `kumihimo-layout-json` 0.1 provides bounded JSON encoding and decoding.

The host owns actual content, provider engines, synchronization, file IO and resource policy. Registering or restoring a provider/resource creates a lightweight identity record; it neither starts an engine nor duplicates its data. Independently selectable std-only `kumihimo-content` 0.2 privately owns Core and coordinates host-executed content effects. The portable Core contains no GUI, renderer or engine. Optional desktop and browser adapters supply presentation; their packet state records actual runtime proof. Whole-state recovery validates detached saved data before replacing the arrangement. Adapters consume the same core commands; they must not maintain a competing canonical arrangement.

</topic>

<topic id="desktop-adapter">

The original Assets-folder adapter source is now `kumihimo-egui` 0.2.0. This is a source compatibility change: exhaustive `HostPolicy` literals must supply `theme_tokens` or use `..Default::default()`, and `WindowSlots::arm` requires mutable slot ownership. Retain settings version 1, Core 0.3, content 0.2 and saved-layout schema 1. Existing proof below names its historical revision and does not accept this update.

`chrome::ThemeTokens::from_visuals(&visuals)` supplies semantic preview, resize, divider, rail and thumb colors. Set `HostPolicy::theme_tokens` to override them; leave it `None` to resolve the current egui context's visuals on every paint. The tokens do not change clipping, dimensions, IDs or canonical state.

Under the optional `native` feature, create `native::NativeRenderer` with the egui context, WGPU configuration and host-selected window budget. It owns a fresh private Painter and tracks actual viewport/window bindings; it cannot adopt an arbitrary pre-bound Painter. `native::ExternalNativeCoordinator` leases that renderer, an `Arc<winit::window::Window>` and `WindowSlots`. Prepare and arm a slot, bind the coordinator to the exact surface and snapshot, then submit actual egui primitives/textures with `paint`. Poll with `poll` or `poll_and_commit`; only private matched GPU readback produces a sealed `NativeCapture`. `commit` consumes that proof once. `cancel` revokes the reservation without closing the host's window. Forward returned unrelated screenshot events to their existing owner. No public API accepts raw pixels, screenshot events or asserted readiness. After handoff, use the renderer's controlled paint/resize methods; removing one viewport preserves the others.

The optional `native-eframe` feature uses the pinned, locally patched eframe and egui-wgpu 0.33.3 packages under `third-party/`. `eframe::Frame::native_capture_handle()` returns the actual runner's sealed capture handle. Clone that handle into `native::EframeNativeCoordinator`, then use `request`, `surface`, `poll_and_commit` and `cancel` with host-owned `WindowSlots` and `Controller`. The producer copies the actual native surface texture and publishes a receipt only after GPU readback and the presentation call complete. Application screenshot input cannot manufacture that receipt. This source update has not yet passed compilation or runtime acceptance; Handshake adoption remains held until independent source proof and publication. Hosts retain content, authorization, undo, storage, diagnostics and actual viewport lifetime.

WP012 scoped Windows x64 MSVC/rustc1.91.1 desktop adapter at 7d9268ab90d5ed6e31d7af070cb73302765741cc: optional egui0.33.3 borrows the host Controller and content dispatcher. All29 owning tests and default/all-feature all-target checks passed. Eighty actual DX12 raster states cover nested variable counts, weighted holes/deep containment, groups/pins/veto, drag/cancel/commit, float/redock/equalize/swap/maximize/restore, tools, four chrome profiles, four navigation modes, mouse/keyboard and separately labelled semantic touch/host-pen input, still backgrounds and scale1/1.5/1. Float ordering survives persistent hide/restore; theme-backed headers and notices remain readable. Twenty-two actual hidden native captures prove correlated preflight, view/group/frame popouts, resize, teardown/recreation, three returns, partial-close survivors, removed-origin semantic return, bounded injected recovery failure with a visible healthy-root notice before recovery, and six Windows accessibility stages. Four genuine SelectionItem.Select actions expose OS false-to-true selection matching canonical changes; Panes open/dismiss and logical focus return passed, with18 forwarded actions total. Owned windows stayed hidden/nonforeground. The unchanged exact hidden-only AccessKit provider test remains valid; downstream workspace roots must apply the recorded local patch. All13 frozen evaluation cases passed independently. Physical input/assistive hardware, physical display changes, ARM64/other platforms, real Servo/video/editor engines, genuine GPU fault injection, performance gains and physical memory release remain unproven. All full future requirement acceptances remain open.

`kumihimo-egui` 0.1 provides replaceable pane controls over a borrowed host `Controller` and `DesktopHost` dispatcher. It projects canonical geometry and routes commands; the host owns content, operation IDs, storage and resource policy. `DesktopAdapter::show` takes an egui context, controller, `SurfaceBinding` and host. Settings expose chrome profiles, hit sizes, navigation and bounded live work. Pane/tab counts remain host-configurable.

Default features provide egui controls; `navigation` and `backgrounds` select existing content contracts, `capture` adds raster rendering, `native` adds the winit/Painter runner, and `native-eframe` selects the separate eframe capture coordinator. Editable text and generated canvas are sample providers. GPU `native_child` and incompatible GPU sharing remain unsupported. Separate optional `drops`, `native-drops` and `native-apps` development routes are described below; their native positive acceptance remains open.

From the project root:

```powershell
cargo check --manifest-path product/Cargo.toml -p kumihimo-egui --all-targets --locked --offline -j 2
cargo test --manifest-path product/Cargo.toml -p kumihimo-egui --all-features --test adapter_contracts --locked --offline -j 2
cargo run --manifest-path product/Cargo.toml -p kumihimo-egui --all-features --example capture_host --locked --offline -j 2 -- --output .local/desktop-proof
cargo run --manifest-path product/Cargo.toml -p kumihimo-egui --all-features --example native_probe --locked --offline -j 2 -- --output .local/desktop-proof
```

`capture_host` writes PNGs and redacted geometry/control/provider observations for nested, narrow, scale and navigation states. `native_probe` owns hidden inactive windows, waits for actual correlated surface pixels before Frame/View/Group migration, invokes the independent UIA observer, exercises return/veto/recreation and writes a native manifest/session. Its bounded deadline exits with a causal error; inspect the output and current canonical bindings before retrying. `--no-uia` explicitly leaves OS accessibility proof incomplete. Automation never requests foreground input. For a visible manual sample, explicitly run `desktop_host --interactive` through the same Cargo example command.

The product workspace applies the exact local `accesskit_windows` 0.29.2 patch in `product/third-party/accesskit_windows-0.29.2`. It lets an owned root carrying `WS_EX_NOACTIVATE` manage accessibility focus without Windows activating its HWND first; ordinary roots retain upstream options. Cargo patches do not propagate through a path dependency. A consuming host must add `[patch.crates-io] accesskit_windows = { path = "<relative-path-to-Kumihimo>/product/third-party/accesskit_windows-0.29.2" }` to its workspace root and retain this source alongside the module. Without that patch, quiet native UIA invocation is unsupported. The patch keeps actual accessibility actions enabled; its observed runtime status lives in WP012.

The excluded provider has its own pinned lockfile. Run only its hidden owning case with `cargo test --manifest-path product/third-party/accesskit_windows-0.29.2/Cargo.toml --lib tests::simple::provider_options_for_owned_hidden_roots --locked --offline -j 2 -- --exact`. Its upstream focus tests show windows, so the full upstream suite is unsuitable for quiet automation. This component case does not replace `native_probe`'s actual UIA and lifecycle proof.

Stable `ControlRecord` IDs and `FrameReport` identify targets, bounds, epochs, rejections and post-action generation. Host dispatch allocates canonical envelopes. `DesktopSession` wraps host storage metadata without persisting window or texture handles. Native return uses validated semantic anchors; missing destinations or vetoed survivors remain recoverable. Physical pen/touch, assistive hardware, GPU faults, other platforms and full editor/browser engines require separate proof. Current implementation checks and runtime observations are recorded per packet; these commands are reproduction paths, not a claim they have passed.

</topic>

<topic id="web-adapter" ingestable="true">

## Browser adapter and Portal-style pilot

WP013 observed Rust/WASM and plain-DOM browser adapter at e6b48fd83a3e4a42d973247aad71d957ec66b82a: optional kumihimo-web0.1 borrows one host content Controller; the Portal-style pilot keeps task meaning separately owned. Six native consumers, affected default check and WASM build passed on unchanged Rust inputs at71b9acc630da. The actual Chrome154 headless batch passed all six original cases and eight inspected images: variable nested counts, mouse/keyboard arrangement, generation-bound previews and Escape/capture-loss/blur cancellation with unchanged layout through release, browser-emulated DPR1.5 content mapping and restored DPR1, host edits/domain commands, visible unavailable-provider recovery, independently read layout/task storage and fresh-incarnation restore, persisted missing-provider-key fault/recovery, rejected malformed/newer/oversized candidates and injected storage refusal, strict decimal model envelopes/replay and real restricted-popup denial retaining source ownership. The private browser was closed. The same-origin local iframe is a denial fixture, not untrusted content isolation; captured DOM roles are not a full accessibility-tree audit. Native popup migration is unsupported. Physical input/display changes, other browsers/platforms, actual Portal/Notes/Studio/Servo engines, performance improvements and physical resource release remain unproven. Full future requirement acceptance remains open.

`kumihimo-web` 0.1 supplies a borrowed `WebAdapter` over the host's sole content `Controller` and dispatcher. The sample `PilotHost` owns task meaning separately from arrangement. Native consumers use `PilotHost::new` and `route_json`; the optional `wasm` feature exports `BrowserPilot(incarnation, config_json)` with `route_json(request)` through matching wasm-bindgen 0.2.128 bindings. Plain DOM modules display canonical Rust geometry.

From the project root, configure `KUMIHIMO_WASM_BINDGEN` with the verified matching CLI and retain the selected reusable Cargo target:

```powershell
cargo check --manifest-path product/Cargo.toml -p kumihimo-web --all-targets --locked --offline -j 2
cargo test --manifest-path product/Cargo.toml -p kumihimo-web --test web_contracts --locked --offline -j 2
cargo build --manifest-path product/Cargo.toml -p kumihimo-web --features wasm,drops --target wasm32-unknown-unknown --target-dir .local/cargo-target --locked --offline -j 2
& product/kumihimo-web/tools/build-web.ps1 -SkipBuild -CargoTargetDir .local/cargo-target -WasmBindgen $env:KUMIHIMO_WASM_BINDGEN
python product/kumihimo-web/tools/serve.py --pkg .local/web/pkg --port 0
```

The server prints its owned loopback URL. The pilot exposes Cards, Details, Depth and live record budget, nested tabs/frames, arrangement controls and a separate task editor. Drag frame headers or use the labelled arrangement controls; Escape cancels an uncommitted preview. Save publishes separate selected layout/task storage keys; a fresh session restores through candidate validation. Malformed or incomplete storage retains the current valid workspace. Missing providers remain reachable placeholders. Full browser popup migration is unsupported; the restricted iframe tests browser denial while retaining source ownership.

Version-1 routes are `snapshot`, `host_snapshot`, `preview`, `cancel_preview`, `command`, `host_command`, `input`, `save` and `load`. IDs, operations and generations are decimal strings. Use fresh canonical identity/generation for new intent; retry the exact old envelope to reconcile its receipt. Receipt generation and current snapshot generation can differ. Task revision changes through explicit domain actions. Stable DOM IDs, `window.kumihimo` routes/state and bounded event records support models without prior context.

`tools/browser-proof.py` accepts `--url`, `--node`, `--node-modules`, `--browser`, `--out` and `--build-manifest`. Runtime settings may come from `KUMIHIMO_NODE`, `KUMIHIMO_NODE_MODULES` and `KUMIHIMO_BROWSER`. It uses a private headless profile with configured existing Node/Playwright/Chrome, checks served module hashes, exercises the six packet cases and closes its owned browser. Outputs include browser evidence and PNGs in `.local/web`; open the images and read Rust/host/storage observations before accepting a GUI result.

If startup fails, inspect the structured constructor/route error and current packet state before retrying. Successful compilation and native consumers do not prove browser startup. Storage refusal leaves current valid state usable. Real Notes/Studio/Servo engines, physical input, other browsers/platforms and performance need separate proof.

</topic>

<topic id="servo-and-graphics-bridge" ingestable="true">

## Optional browser and graphics experiment

`kumihimo-render-bridge` 0.1 is separately selectable. Its default uses only the Rust standard library: complete content identity, bounded RGBA frames, dirty scheduling, single-scale input mapping and lifecycle reconciliation. The optional `desktop` feature connects those contracts to the existing Controller/DesktopAdapter and WGPU. Core and content do not link Servo. Logical frame/tab counts remain a host policy; the sample uses four providers and bounds live engines to two.

WP014 has observed two genuine Servo 0.6.0 Linux x64 software processes under WSL and Windows x64 DX12 composition of their copied pixels alongside a 2D provider and real transformed/depth-tested 3D geometry. Normal, narrow, injected scale1.5 and moved/resized scenes use the same canonical slots. Eleven actual engine PNGs retain independent request/timing/hash receipts; nine GPU PNGs cover composition, scene input, an unloaded placeholder, fresh page reload and renderer replacement. Static comparisons preserve full RGBA equality with no extra browser uploads or scene renders after one bounded GUI startup-fade settling draw. The packet's independent evaluation and publication state are the completion authority.

This is an external-process CPU-copy experiment. Native Windows Servo, linked libservo, zero-copy sharing, video/YouTube/audio, physical input/displays, ARM and performance improvements are unproven. Freeze, throttle, direct engine animation/frame-ready notification and raw engine/GPU-context native popout transfer report Unsupported. Existing native-window machinery does not prove engine-context migration. Hidden means no host capture; it does not mean suspended or memory released. PageReload restores the declared page, not arbitrary JavaScript/media state.

From the project root, retain the configured stable Cargo target/cache and use the project's preparation gate before builds:

```powershell
cargo check --manifest-path product/Cargo.toml -p kumihimo-render-bridge --all-targets --locked --offline -j 2
cargo check --manifest-path product/Cargo.toml -p kumihimo-render-bridge --all-targets --features desktop --locked --offline -j 2
cargo test --manifest-path product/Cargo.toml -p kumihimo-render-bridge --features desktop --test bridge_contracts --locked --offline -j 2
New-Item -ItemType Directory -Force .local/render-bridge
cargo run --manifest-path product/Cargo.toml -p kumihimo-render-bridge --features desktop --example compose --locked --offline -j 2 -- --plan-output .local/render-bridge/capture-plan.json
```

The plan is geometry/intention data with no running capture deadline. Configure an ignored JSON file with `runtime_executable`, `fixture_root`, `output_root`, optional `child_environment` and distinct `ports`. Paths are consumer-controlled; `fixture_root` contains the shipped `browser-a.html` and `browser-b.html`. Native library paths belong only in the child environment. The pinned GNU release is 49,202,383 bytes, archive SHA256 `ad951ede1a1a73899b822c9464f6bdb3ec25b531b27cd806671d79ac8b6a60d0`; executable `servo/servoshell` SHA256 `cb9267dfdbd89d0678cb4859857708a2c3c50affff2c091770ff73341263b09d`, version `Servo 0.6.0-c78d2c206`. Verify the actual loader closure and CLI before rendering; do not build Servo or install its toolchain for this experiment.

The shipped Linux entrypoints are:

```text
python3 product/kumihimo-render-bridge/tools/servo-worker.py --config CONFIG.json
python3 product/kumihimo-render-bridge/tools/servo-batch.py --config CONFIG.json --plan CAPTURE-PLAN.json
```

Run them only through a host-owned controller inside a fresh private network namespace. Servo's WebDriver binds all interfaces. The controller must independently inspect the live worker's exact PID/starttime/netns from the outside kernel, only-loopback interfaces/routes, disabled forwarding and a distinct host namespace before EACH engine launch/reload. After LISTENER_READY, independently verify the actual engine owns the socket and that neither outside Linux nor Windows can connect. No public endpoint or self-issued worker receipt is allowed. Initialization emits PRELAUNCH and waits; it starts no engine without admission. The trusted private stdin is the authority boundary, not the literal issuer label.

All JSON lines use `protocol: "kumihimo.servo-worker.v1"`. The deterministic batch emits challenges and expects these host replies:

- PRELAUNCH: `op: "admit"`, fresh `receipt` binding worker, challenge request ID, unique receipt ID, observed/expires UTC times (maximum30s), pinned runtime hash, independent evidence path and the inspected namespace facts.
- LISTENER_READY: `op: "listener_admit"`, `view_key`, equally fresh receipt additionally binding exact engine/port and actual listener ownership/outside denial.
- CAPTURE_READY: `op: "capture"`, unchanged `view_key`, `case_name`, `capture_challenge`, and the complete immutable intent plus `deadline_ms = actual host issue UTC + 15000`. Bind all eight Scope fields, geometry revision, engine incarnation, renderer epoch, request/sequence and physical dimensions. Do not refresh or backdate an issued request.
- UNLOAD_UNKNOWN: `op: "reconcile"`, `view_key`, fresh receipt binding the exact engine, worker, operation request ID and tracked descendant set. Independently prove exact PID/starttime absence and no live descendants before asserting unloaded. A permission failure is Unknown, not absence.

`FRAME` records real DOM dimensions/input state, engine identity, request, PNG path/hash, raw bounds/format and completion times. Independently hash the received PNG and canonical UTF-8 request JSON (sorted keys, compact separators), record actual host issue/completion UTC, and add each manifest row's `host_receipt {issued_ms, received_ms, request_sha256, png_sha256, evidence_ref}`. Completion must precede its untouched deadline. For the first A incarnation's unload, add top-level `unload_receipt` using protocol `kumihimo.independent-engine-exit.v1`, original complete scope, engine incarnation1/exact process identity, matching `operation_request_id`, observed UTC, confirmed absence/no descendants and evidence reference. Keep `unload_request_id` unchanged. The provider's public DTOs and `confirm_engine_exit` define the exact shape; an unmodified worker manifest intentionally lacks these independent host facts.

Consume those same accepted bytes on Windows:

```powershell
cargo run --manifest-path product/Cargo.toml -p kumihimo-render-bridge --features desktop --example compose --locked --offline -j 2 -- --input-manifest MANIFEST.json --output-root .local/render-bridge
```

PNG paths stay relative beneath the manifest directory. Do not use `--now-ms` to make stale data pass; normal operation uses actual UTC and original accepted receipt times. Outputs include PNG/JSON scenes and `composition-evidence.json` with engine PNG/RGBA provenance, canonical slots, mapped picks, upload/depth counts, recovery and support limits. Open every output image and compare it to its actual engine source before accepting rendering. A static mismatch writes the rejected frame and causal counter/first-pixel diagnostic.

Default bounds are message4MiB, PNG/raw RGBA16MiB, side2048, one pending capture per view, coalesced dirty work and a240s scenario. Startup30s, request15s and graceful shutdown10s are product timers; no outer Cargo kill timer. On failure, inspect the named engine log, structured event and independent ownership record; retain good pixels or the logical placeholder. Resolve the exact cause before retry (maximum3), reconcile uncertain lifecycle outcomes, and stop only exact session-owned PID/starttime-validated processes. EOF does not certify cleanup. Preserve unrelated processes and caches. `inspect`, `visibility`, `policy`, `resize`, `input`, `mutate`, `capture`, `unload` and `reconcile` expose bounded model routes; actual CPU ticks/RSS are observations, not general savings promises.

</topic>

<topic id="external-app-tiling" ingestable="true">

## Optional selected external-app tiling

`kumihimo-external-apps` 0.1 is independently selectable; its default has no native Windows dependency. Enable `windows` on Windows to use `windows::WindowsBackend`. Core/content acquire no OS handles. This published desktop-tiling route positions explicitly selected compatible top-level application windows beside the host. It launches no applications and controls no desktop set. The separate in-development hosting route below has no actual installed-app acceptance yet.

Create `ExternalApps::new(backend)`, then `bind(WindowTarget { hwnd, pid, expected_creation_time }, semantic_reference)`. The returned `SelectedWindow` retains the process handle, creation time, original placement and a unique window-scoped property token. Ownership, process lifetime and that token are checked before each effect, so removal or replacement invalidates the selection even inside the same process/thread. Installing/removing the token is a reversible selected-window metadata effect; a denied installation rejects binding. Native identities stay live-only.

Use `tile(&selected, &GeometryOutput, FrameId, DesktopMapping, ObserveBudget)` with the host's canonical Core geometry projection. The mapping supplies physical desktop origin and pixels per DIP; it is not another arrangement store. `inspect` reports physical placement, current monitor work area, DPI, minimum sizing and resize support. Normal visible top-level windows, compatible sizes and the current monitor/scale are supported; minimized/maximized initial state, unsupported sizing or cross-monitor/DPI allocations reject explicitly. `restore(&selected, budget)` targets the captured original normal placement.

Receipts separate requested bounds, independently observed placement, request/observation errors, compensation and `pending`. Asynchronous dispatch is not placement proof. An unchanged poll timeout remains `Unknown`; an unsettled request blocks a new effect or successful detach until reconciliation. Known completed partial effects permit one bounded compensation; partial effects without completion proof retain their obligation. Default observation is twenty attempts, 25 ms apart. `RecoveryRecord::from_selection` saves schema 1, a semantic reference, original placement and optional pending placement, with no HWND/PID. After restart, explicitly select the intended live instance and call `record.apply_to(&mut selected)` before restore. A mismatched reference/schema or an attempt to erase a live pending request rejects; missing windows stay an actionable binding failure.

From the project root, after preparing the serial Cargo lane and supplying the explicitly selected owned instance's `$SelectedPid`, `$SelectedHwnd` and `$SemanticReference`:

```powershell
cargo run --manifest-path product/Cargo.toml -p kumihimo-external-apps --features windows --example selected_window --target-dir .local/cargo-target --locked --offline -j 2 -- --pid $SelectedPid --hwnd $SelectedHwnd --semantic $SemanticReference
```

Optional `--created FILETIME` checks the host's expected decimal process creation time; `--hwnd` accepts decimal or `0x` hexadecimal. The CLI emits a flushed `bound` JSON line, then accepts one JSON command per stdin line (maximum 65,536 bytes):

```jsonl
{"op":"inspect"}
{"op":"tile","x":30,"y":30,"width":900,"height":600}
{"op":"restore"}
{"op":"recovery"}
{"op":"exit"}
```

Choose tile bounds from the inspected capabilities. `recover` accepts `{"op":"recover","record":...}` using the exact saved recovery object. Read each response before the next effect. Restore and inspect the original bounds before cleanup; `exit` refuses success while pending, and premature EOF emits `unresolved_dispatch` plus recovery and exits with failure. The CLI only releases its own lease; the host owns application startup and cleanup and must preserve unresolved obligations.

The published WP019 result used one fresh owned VLC instance on 2026-10-14 Windows x64: independent OS reads matched 900×600 placement, 1000×650 resize and restoration to its original bounds. Inspected captures show the VLC interface at the tiled and restored sizes. The sampled foreground window and the independently observed unselected window remained unchanged; the controller cleaned up its owned instance and CLI. This establishes that scoped VLC path; universal application compatibility, Blender, media playback, mixed-DPI behavior and other platforms remain unproven.

</topic>

<topic id="file-drop-in-development" ingestable="true">

## File-drop integration in development

WP018 is in development; manual acceptance remains open. Select `kumihimo-content/drops` for the portable protocol, `kumihimo-web/drops` for the pilot route, `kumihimo-egui/drops` for desktop coordination, or `kumihimo-egui/native-drops` for the Windows OLE receiver (also selects `native` and `drops`). Defaults omit these integrations. `build-web.ps1` currently selects `wasm,drops`; use `-Features wasm` for bindings without drops.

[`DropCoordinator`](kumihimo-content/src/drop.rs) validates metadata-only hover and consumes a fresh offer/grant at release through the existing Controller request/ACK path. Offers bind opaque tokens, revisions, expiry and bounded item metadata to canonical frame/view/resource, generation and epoch. Core receives no unrestricted paths or file bytes. The host retains payloads, performs permission checks and bounded IO after admission, revalidates delayed completion, and owns domain undo/recovery. Cancellation or stale selection invalidates the preview; arrangement tab/frame moves never move physical files.

The pilot accepts bounded UTF-8 files (up to eight, 8 KiB total): Open/Copy replace the destination task body and Import appends. Internal task Link adds a reference; Copy copies its body. The separate **Drag task link** control starts asset transfer; dragging the frame title or blank header moves the frame. Browser Move is denied. Eleven inspected headless Chromium cases cover trusted pointer arrangement/tab/cancellation and task-link gestures, plus synthetic untrusted DragEvents carrying actual File/DataTransfer objects through production listeners and Rust host IO. Physical OS-to-browser file dragging is **NOT_PROVEN**; these cases do not complete WP018.

The native sample imports `.txt`/`.md` into text panes only, up to eight files and 8 KiB total, with all reads validated before one append, bounded undo and causal ACKs. Shell transfer advertises COPY or NONE; it never deletes/moves source files. The OLE receiver captures native POINTL coordinates, converts against the current owned HWND and epoch, rechecks process/property lifetime, and unregisters before window destruction.

Run the visible fixture host only on an agreed controlled desktop:

```powershell
cargo run --manifest-path product/Cargo.toml -p kumihimo-egui --features native-drops --example file_drop_host --target-dir .local/cargo-target --locked --offline -j 2 -- --interactive
```

Optional `--output PATH` selects observations. **`--observe-host` includes imported document text in the native snapshot**; use it only with explicit test fixtures. Default observations omit that payload. [`native_drop_source.rs`](kumihimo-egui/examples/native_drop_source.rs) accepts `--fixture PATH --receiver-hwnd DECIMAL --receiver-pid PID`; it validates the live registered receiver/process/property and actual cursor target throughout a bounded three-second OLE COPY attempt. It requires controlled desktop agreement. A dead-receiver rejection has been observed; successful native transfer/import is still unproven. Inspect host receipts and exact destination text independently before accepting a positive transfer.

</topic>

<topic id="foreign-window-hosting-in-development" ingestable="true">

## Foreign-window hosting in development

WP020 is in development; manual acceptance remains open. Select `kumihimo-egui/native-apps` to add the separate Windows route and `kumihimo-external-apps/windows`; default and `native-drops` consumers need not select it. Source compilation and seven lifecycle/five adapter fixtures passed without native windows or app effects. Installed-app attach, resize, tab visibility, input and return acceptance remain **UNPROVEN**. Published WP019 desktop tiling does not prove embedding.

[`NativeChildRoute`](kumihimo-egui/src/external_apps.rs) binds a distinct canonical view/resource to `HostingSession<WindowsHostingBackend>`. Only the owned runner supplies `NativeParentBinding`. Allocation uses the current post-toolbar/navigation RenderSlot in parent-client physical pixels, exact full DIP clipping, scale, generation and epoch. A different parent HWND is rejected even at the same epoch; migration requires confirmed desktop detach before an explicit new binding. Partial clipping and floating overlays are unsupported. Actual egui popup/foreground/tooltip/preview state blocks child presentation; overlay painting waits for observed hiding with no pending worker.

Preflight requires normal compatible windows, exact app/parent DPI-context equality, app/parent process/property lifetime and an owned parent on the originating UI thread. Unsupported ownership, native menus, window styles/modes or sizing reject before attach. Each host has bounded bindings/operations; one foreign-call worker retains both leases and the actual parent Window, wakes the event loop on completion, and admits no conflicting dispatch. `request(operation)` accepts `HostingOperation` variants Attach, Resize, Visibility and Detach; `reconcile()` distinguishes observed completion from pending `Unknown`. A stale dispatched epoch cannot commit current attachment. Cross-process focus/keyboard routing, application menus/dialogs and fullscreen are denied: explicitly detach to the original desktop placement for that work. GPU `native_child` remains unsupported.

Opted-in `NativeHost` implementations use `native::run_with_recovery`; legacy `native::run` rejects app opt-in before HWND effects. Implement parent/slot/overlay hooks plus `before_native_parent_destroy` and explicit `retry_native_parent_detach`. Every close, failure, timeout, probe and removal consults detach before releasing the parent. Unresolved work keeps the event loop alive; one automatic detach attempt is followed by explicit retry, without expired-timer spin. Exceptional return yields `NativeRunResult::Retained` with the entire owner, rather than reporting cleanup success.

On the originating UI thread, `RetainedNativeOwner` exposes `recovery_id()`, `inspect()`, bounded `reconcile()`, explicit `retry_detach()` and `finish()`. Dropping an unresolved owner retains it in `RecoveryBank::current_thread()`; inspect that bank and use `RetainedNativeOwner::<Host>::retrieve(id)` (or `HostingSession::<Backend>::retrieve(bank, id)`) to resume. Active and retained owners share capacity eight by default; `configure_capacity(capacity)` accepts 1–64 and requires an empty bank. Full/conflicting admission rejects before effects. Do not terminate the originating thread/process while obligations remain. Semantic `HostingRecovery` stores original settings and pending intent, not actionable HWND/PID; restoring a file does not discharge live cleanup.

[`foreign_app_host.rs`](kumihimo-egui/examples/foreign_app_host.rs) composes DemoHost with a separate `selected-installed-app` resource and semantic reference `external-app:selected-app-host-example`. Supply a fresh explicitly selected instance on an agreed controlled desktop:

```powershell
cargo run --manifest-path product/Cargo.toml -p kumihimo-egui --features native-apps --example foreign_app_host --target-dir .local/cargo-target --locked --offline -j 2 -- --interactive --pid $SelectedPid --hwnd $SelectedHwnd
```

HWND is decimal. The example launches/closes no application and injects no mouse/keyboard input. Its sibling host pane offers desktop detach. After a retained return, stdin commands are `inspect`, `retry` and `finish`; EOF keeps reconciling the owned parent instead of exiting with an attached app. Verify original parent/styles/visibility/bounds independently, then release the owner and verify parent absence before accepting cleanup. This is a source/proof entry, not a completed runtime demonstration.

</topic>

<topic id="startup" ingestable="true">

## Start and check

From the Kumihimo project root, with Rust/Cargo 1.91+ and the selected platform's linker installed:

```powershell
cargo check --manifest-path product/Cargo.toml --locked -p kumihimo-core --no-default-features
cargo check --manifest-path product/Cargo.toml --workspace --all-features --all-targets --locked
cargo test --manifest-path product/Cargo.toml --workspace --all-features --locked
cargo run --manifest-path product/Cargo.toml --locked -p kumihimo-core --example workspace
cargo run --manifest-path product/Cargo.toml --locked -p kumihimo-layout-json --example roundtrip
cargo check --manifest-path product/Cargo.toml -p kumihimo-content --all-targets --locked --offline -j 2
cargo check --manifest-path product/Cargo.toml -p kumihimo-content --all-targets --features navigation --locked --offline -j 2
cargo check --manifest-path product/Cargo.toml -p kumihimo-content --all-targets --features backgrounds --locked --offline -j 2
cargo check --manifest-path product/Cargo.toml -p kumihimo-content --all-targets --all-features --locked --offline -j 2
cargo test --manifest-path product/Cargo.toml -p kumihimo-content --all-features --locked --offline --no-fail-fast -j 2
cargo run --manifest-path product/Cargo.toml -p kumihimo-content --example recording_host --locked --offline -j 2
cargo run --manifest-path product/Cargo.toml -p kumihimo-content --all-features --example controls_host --locked --offline -j 2
```

The console example creates nested ownership, moves a frame, supplies geometry, reserves a pinned tool strip and commits a docking preview while retaining view/resource identity. It opens no windows. Builds should use the project's admitted serial lane and configured reusable target directory; session instructions are outside this product manual.

To consume the library, add a path dependency on `product/kumihimo-core` from your application's Cargo manifest, relative to that manifest. `src/lib.rs` exports the API; `model.rs` defines records/policy; `identity.rs` and `command.rs` define scope and envelopes; `core.rs` owns replay; `mutation.rs` applies ownership changes; `tabs.rs` handles tabs and queries; `arrangement.rs` validates detached candidates; `layout.rs`, `geometry.rs`, `solver.rs`, `tools.rs`, `docking.rs` and `layout_actions.rs` implement projection and placement. `persistence.rs` defines saved records/stamps and `restore.rs` validates recovery. Core consumer contracts live in `tests/`; JSON recovery contracts and its example live in the JSON package. Tests use public interfaces without mutable access to live storage.

Observed for WP008 candidate `90f93239d2e474e28451cab15458454ecd1649dc`, using stable rustc 1.91.1 on Windows x64 MSVC: the locked/offline all-targets check passed, one private exhaustion test, nine existing ownership contracts and all twelve layout/tool/docking contracts passed. The console example returned `generation=11`, root frames 2 and 4, view 7/resource 6, tool 8, content `(0,24,640,336)` and two panes. The float-return regression passed with append excluded and with no eligible return. These results prove the admitted headless in-memory scope on that host; Windows ARM64, WASM, native GUI, cross-platform runtime and performance remain unproven. The earlier WP007 candidate `a004dbd4c655d6d193e288d21e37dc9987095353` passed its ownership-only proof before this extension.

</topic>

<topic id="api-and-ownership" ingestable="true">

## Operate the core

Create `Core::new(identity, Limits::default(), receipt_capacity)?`. Read `core.snapshot()` for version 3 records, identity, generation and effective limits; cloning a snapshot produces detached inspection data and cannot overwrite live state. Use `core.execute(Envelope)` for every mutation. Inputs are identity, actor attribution, an instance-local operation sequence, expected current generation and one `Command`. Outputs are an attributable `Outcome` containing identity, generation and `Result<Change, Error>`, plus `Response::replayed`.

```rust
use kumihimo_core::{Command, Core, Envelope, Identity, Limits, OperationId};
let identity = Identity::new("document-lineage", "host-issued-incarnation").unwrap();
let mut core = Core::new(identity.clone(), Limits::default(), 64).unwrap();
let response = core.execute(Envelope {
    identity,
    actor: "host-controller".into(),
    operation: OperationId::new(1),
    expected_generation: 0,
    command: Command::CreateWorkspace { owner: None },
});
assert!(response.outcome.result.is_ok());
```

`CreateWorkspace { owner: None }` creates a root. A frame has exactly one placement workspace and at most one child workspace. `CreateFrame` inserts it in the ordered workspace; `CreateWorkspace { owner: Some(frame) }` attaches its child workspace. Frames retain stable identity independently of plan-local split/grid nodes.

`RegisterProvider { key, supports_shared_views }` declares a stable unique provider key and sharing capability. `RegisterResource { provider, key, restore_reference }` supplies a key unique within that provider and an opaque host reference. `CreateView` creates one open placement; another open view of the resource requires declared sharing. Closed views do not consume open sharing capacity. Capability declaration is a host assertion, not engine proof. IDs allocate monotonically across all seven entity kinds from 1; recovery preserves IDs and allocator monotonicity within a lineage. `from_raw` creates a target reference without reserving an ID. Zero and `u64::MAX` are invalid entity IDs; the latter is the allocator's exhausted sentinel. Host lineage integrity determines identity guarantees for records absent from live history.

`CreateGroup` stores a frame-local label and optional color; `AssignGroup` checks view/group frame ownership. Group membership is derived from view references. `SelectView` selects one existing view in its owning frame. Creating the first view selects it. Moving the selected view away selects the first remaining source view, or none. An empty destination selects the incoming view. A cross-frame view move clears its frame-local group; resource/provider references remain unchanged. Frame subtree moves preserve every descendant's groups, selection, order and identities.

`MoveFrame` rejects self/descendant containment cycles using iterative ancestry. `MoveView` moves one existing placement. Insertion indexes for both refer to the destination sequence **after removing the moving item**. For `[A, B, C]`, moving B to 0 gives `[B, A, C]`, to 1 is unchanged, to 2 gives `[A, C, B]`; index 3 rejects. Creation indexes refer to the unmodified destination. A same-parent unchanged order, same group assignment, already-selected view or identical policy is an explicit `Change::Unchanged` and does not increment generation.

`SetViewMetadata` supplies label, pin and unsaved state. `CloseView { approved }` requires host approval independently of unsaved state, checks pin policy and retains a tombstone without disposing host content. Closing the selection selects the first remaining open view. `ReopenView` preserves IDs/resource, clamps its saved index to the destination's current length and rechecks open membership, sharing and pin policy. Restored/reopened host visibility/usability starts false; supply `SetViewState` after host confirmation.

`ConfigureGroup` adds description and collapse metadata. `MoveGroup` moves open members atomically in canonical order, including interleaved members, and updates retained closed members' frame. `index` is the destination tab index after removing the moving open members; `group_index` independently addresses the destination group metadata sequence after removal. Selection recovery is deterministic. `PinPolicy` can deny close, reorder and cross-frame moves across individual/group/reopen routes; genuine no-ops remain available.

`Snapshot::tab_page(&TabQuery)` searches the complete canonical open/closed set, optionally filters a frame and pages canonical or label order. Rows expose metadata and unavailable-provider placeholders; `total` counts the selected frame/open/closed set before search and `matched` counts it after search, independently of page length. This is a headless metadata query, with no renderer or engine startup.

</topic>

<topic id="admission-and-replay">

## Admission, ordering and replay

Defaults admit **16,384 total records**, **65,536 layout entries**, **1,024 retained closed views**, **256 rows per tab query**, **65,536 UTF-8 bytes per label/description/restore reference** and **256 bytes per provider/resource key**. Closed views remain records; retention exhaustion rejects another close without evicting history. Depth and per-frame open-view limits are optional. `Limits::unbounded()` explicitly opts into host-managed admission. Record limits are not memory ceilings; actor/envelope sizes and host content remain host responsibilities. Identity components have an independent fixed 128-byte maximum.

`max_depth` measures frame depth: root workspace is 0, frames inside it are 1, an owned child workspace has its owner frame's depth, and its child frames add one. New frames must meet the configured depth. Moving deeper checks the deepest descendant frame, while same-depth or shallower moves remain available for recovery after limits are lowered. `max_views_per_frame` counts placements locally: new views and cross-frame moves that increase destination usage must meet it. `max_records` applies to new entity creation globally. `SetLimits` never deletes existing records; lowered limits reject affected new admission. Moves do not allocate records. Policy changes increment generation only when policy differs.

`Identity { lineage, incarnation }` is immutable for a Core; both strings are nonempty and at most 128 UTF-8 bytes. The host supplies lineage integrity and an incarnation fresh across all previous instances; the library performs no randomness, clock or process lookup. Envelopes, outcomes, snapshots, previews, prepared restores and save stamps carry that scope. Foreign envelope or embedded token identity rejects before receipt lookup/high-water handling and has no ledger effects, even when sequence and payload match a retained receipt.

One `&mut Core` serializes commands. The host allocates increasing operation sequences across all actors; independent content work is external. Check `ledger_status().high_watermark.checked_next()` or coordinate equivalent host allocation. Sequence gaps are allowed; any sequence at or below the high-water mark without a retained receipt is expired, including an unused skipped sequence. Sequence 0 is invalid. At `u64::MAX`, `checked_next()` returns `None`; never wrap. Fresh recovery starts a new ledger and incarnation, so old envelopes/tokens cannot execute there.

The fixed receipt capacity counts outcomes, including rejections and no-ops. Exact retained **complete envelopes** replay their original outcome before stale-generation validation; the original outcome generation can be older than the current snapshot. Changing actor, expected generation or command under a retained operation ID rejects as `IntentMismatch`. A sequence whose receipt was evicted returns `ExpiredOperation`, never repeats a mutation. Capacity 0 disables replay, while the high-water mark still prevents old sequences from executing. There is no unbounded operation-ID set and no cross-restart deduplication guarantee.

Rejected commands may advance the high-water mark and store/evict bounded receipts. They do not alter model records, generation or entity allocation. A new valid state-changing command increments generation once. Retrying a rejection with corrected intent needs a new sequence and a fresh expected generation.

</topic>

<topic id="geometry-and-packing" ingestable="true">

## Geometry and packing

`Scalar::new` rejects non-finite values and normalizes signed zero. `Size` validates nonnegative dimensions; `Rect` also checks finite endpoints. Exact equality preserves complete-envelope replay. Coordinates are f64 logical DIPs. Negative gaps, nonpositive weights, arithmetic overflow and infeasible constraints return `Error::Geometry`; rounding that would undercut a track minimum rejects instead of silently violating it.

Supply `SetGeometryContext { workspace, context }` for each workspace whose geometry is requested. The canonical context owns viewport, gap, overflow, per-frame minima, automatic-packing weights, explicitly eligible `DockLocation`s and floating permission. `Snapshot::geometry(workspace)` returns generation, required/virtual extents, viewport/content rectangles, every workspace frame, frame content, clips, tool presentation and allocated nodes/cells. Child-workspace minimum requirements propagate iteratively into the owner frame; each requested child geometry uses its own host viewport. The host must update that viewport when displaying it inside its allocated parent content.

Unconfigured packing derives horizontal leaves from the full canonical frame order. `SetLayout` configures a flat `LayoutPlan`: one root, a `BTreeMap<NodeId, LayoutNode>`, weighted horizontal/vertical splits or weighted row/column grids with explicit non-spanning cells. Holes are allowed. Every non-floating frame has exactly one leaf; missing references, cycles, multiple parents, orphan nodes and duplicate cells reject. `NodeId` is a plan-local selector, not a generated stable entity ID; repairs may change selectors. Configured split/grid weights control their own tracks; context frame weights control automatic packing.

Allocation first sums child minima and inter-track gaps, then divides only spare extent in proportion to normalized positive weights. The final track consumes the rounding remainder. Grid track minima are the maximum occupied-cell minimum in each row/column. Workspace tool strips are reserved first; frame/view strips reserve their frame interior. `Overflow::Reject` rejects undersized inputs atomically. `Overflow::Scrollable` expands virtual extent and returns clips without discarding panes; actual scrolling/reveal is a later host adapter responsibility.

Legacy `CreateFrame`/`MoveFrame` reconcile configured packing without redefining canonical sequence indexes: remove/prune the departing leaf, then append to a compatible horizontal root split or wrap the retained root horizontally. Unrelated weights, grid tracks/holes and child-owned plans survive. Explicit configured-empty scopes remain configured. Per-frame minima/automatic weights follow cross-workspace frame moves into an existing destination context; moving into a scope without context removes those source-local inputs and requires the host to supply its destination measurements.

</topic>

<topic id="tools-docking-and-preview" ingestable="true">

## Tools, docking and preview

`CreateTool(ToolSpec)` keeps provider owner, original view, bound `EditingTarget { view, resource }` and placement separate. The owner must match the origin resource's provider. Placements target a view/frame/workspace edge or an unoccupied configured grid cell. Supply allowed placements, orientations, preferred/minimum sizes, order and independently reported `TargetState`. Configure the destination workspace context before placing a tool there. Edge thickness uses preferred size; the cross axis enforces minimum size. Strips reserve top, bottom, left, right, then host order and ToolId. They cannot silently overlap or make negative content.

`ToolPolicy::new` defaults to pinned binding, visible-and-usable target visibility and hidden-space release. Keyboard focus never hides or retargets a tool. Effective geometry presentation also checks origin/target open state, host view state and provider availability. Closed or unavailable targets cannot be usable/applicable despite stale host flags. An outward `Always` tool may remain visibly disabled and retain space; a closed-view-anchored tool is dormant. `Snapshot::effective_tool_presentation` exposes this canonical decision; `Tool::presentation` alone projects raw host flags. `BindingPolicy::ActiveCompatible` records host intent without automatic retargeting. `RebindTool` requires a compatible currently usable view/resource pair. Moving outward or returning retains origin/owner/target; edge moves select the allowed orientation.

Use `DockIntent` for existing frame/tool placement, bounded `Float` or `Redock`. Frame destinations require the destination context's explicit eligibility; tool destinations require its allowed policy. Float rectangles must satisfy minima and fit the complete workspace viewport, preserve one workspace owner and report `SurfaceKind::InAppFloat`. Their saved return location prefers eligible workspace append, otherwise another currently validated eligible destination in that workspace; floating rejects if none can redock. Later host policy changes can still require a new explicit dock intent. Native windows and modal overlays are not exposed.

`Core::preview(intent)` validates/solves a detached hypothetical state without consuming live generation, entity allocation or ledger outcomes. Inspect its private token's accessors; consume it with `cancel()` or wrap it in `Command::CommitPreview`. Commit rechecks generation and intent through the existing Envelope path, imports no geometry, and exact retained replay mutates only once. Every changed canonical topology, context, target state or policy invalidates an older token. `snap_candidates` projects only explicitly eligible current edges/shared frame boundaries/grid cells within the supplied radius, ordered by distance then location; snapping adds no arbitrary subdivisions, overlap or hysteresis.

The default aggregate `max_layout_entries` is 65,536. `layout_entry_count()` counts each retained node plus leaf/child reference, grid track/cell reference, context plus map/eligibility entry, four tool policy/state records plus allowed/compatible/orientation entries, two records per float and one per maximized workspace. Derived automatic plans/solver output are not retained metadata. Limits count records/entries, not bytes; lowering preserves existing state and allows unchanged/decreasing metadata recovery while rejecting growth beyond the new limit. Unrestricted policy remains explicitly host-controlled.

`Equalize` sets workspace split/grid and automatic-packing weights to one while preserving structure and minima; only spare extent is divided equally. `SwapFrames` swaps two docked same-workspace frame leaves without changing identities, ownership, view order or child workspaces.

`Maximize` retains underlying packing while projecting one docked frame, its descendant constraints and selected/frame/root-edge tools. Sibling frames, floats and cell tools are hidden; their structural references remain validated. Ownership/packing/docking operations affecting either source or destination of that projection reject, including preview commits and child attachment. Tab/group/geometry and independent operations remain available under their normal policies. `RestoreLayout` validates complete retained geometry before exiting; failure preserves maximization. Hidden float bounds do not invalidate active maximize geometry, but exiting checks them, and scrollable overflow does not repair out-of-bounds floats.

Package 0.3/Snapshot 3 are unreleased breaking API changes. Snapshot 1/2 were inspection records, never durable files; schema 1 is the first supported saved format.

</topic>

<topic id="saved-layout-and-recovery" ingestable="true">

## Save and restore

`Core::save_data()` captures immutable `PersistedLayout` plus `SaveStamp`; `save_layout()` returns the DTO alone. Schema 1 retains flat IDs, ownership/order/selection, closed membership, group/pin metadata, stable provider/resource keys and restore references, packing/contexts/floats/maximize state, source identity/generation and allocator. Runtime handles, engines, provider availability, view host state, tool `TargetState` and ledger receipts are omitted. Restore resets host flags and leaves unresolved providers as `Missing` placeholders. `RestoreOptions.providers` supplies key/capability resolution without invoking engines; incompatible capability yields an unavailable provider. The host confirms view/tool state separately.

For live recovery, call `core.prepare_restore(saved, options)?`, inspect the private token's source/options/target accessors, then submit `Command::Restore(token)` through the current envelope. Preparation has no live model or ledger effects. The source lineage must match; extant IDs cannot change entity kind or immutable provider/resource/view-resource identity. The token binds target identity and generation; any intervening mutation rejects it. Successful restore always advances current generation once, even for identical arrangement, uses `max(live, saved)` allocator and preserves the live ledger. Historical receipts still replay their original outcomes; they do not prove a referenced record still belongs to the restored arrangement.

For fresh recovery, `Core::from_persisted(saved, fresh_incarnation, options, receipt_capacity)?` retains saved lineage/exact IDs, requires an incarnation different from the source, starts generation 1 and an empty ledger/high-water 0. The host must ensure freshness against every earlier instance. No absent-history identity guarantee or persistent deduplication follows.

Effective limits are the intersection of saved and host `RestoreOptions.limits`. Fresh imports are strict. Live restore permits unchanged/decreasing overage only where the effective dimension equals its current canonical ceiling, comparing current usage by that dimension and frame/field rather than borrowing another entity's excess. Stricter effective ceilings remain strict; actual policy is stored without inflation. Structural integrity, IDs, references, cycles, minima and wire limits are never grandfathered. The allocator must exceed every saved entity ID; `u64::MAX` is valid only as its exhausted sentinel.

`RestoreOptions.bounds` supplies host viewports. `FloatRepair::Reject` rejects invalid active float bounds; explicit `Clamp` moves/resizes them within the viewport only if minima fit. Maximized hidden floats may retain stale bounds until exit validation. Invalid recovery leaves the existing Core intact; detached whole-state validation does not run legacy frame-move measurement migration.

`kumihimo_layout_json::encode`, `decode` and option variants support complete schema-1 JSON. Typed entity IDs, generation, allocator and plan-local `NodeId` use canonical decimal strings, preserving all u64 precision; numeric aliases, leading zeros and invalid generated IDs reject. Unknown fields, duplicate object members at every nesting level, duplicate logical records, trailing input and unsupported schema reject at codec or restore validation. Finite geometry uses validated constructors. Both encode/decode default to an 8 MiB byte limit, configured by `CodecOptions`; the ordinary JSON parser recursion guard remains enabled.

`encode_core(&core, options)?` captures immutable `SavedBytes`; `saved.is_current(&core)` checks its identity/generation stamp. The host must serialize publication through one writer and check freshness immediately before publishing, with mutation excluded between check and publication. Atomically replace a complete blob, or keep identifiable prior/new complete blobs for recovery. On interrupted writes or decode/restore failure retain the last validated Core/blob. The library performs no file IO; actual storage atomicity and interrupted-save recovery are unproven here.

Observed for published WP009 candidate `c387f9e4f6d353e8dedeb9ed49590a4681c73b30` on Windows x64 MSVC with stable rustc 1.91.1: the locked/offline all-feature workspace/all-target check passed; all 49 tests passed (1 private exhaustion, 9 ownership, 12 geometry/tool/docking, 7 layout recovery, 7 tabs and 13 JSON recovery); both console examples succeeded. The workspace example returned generation 11 and two panes; schema-1 JSON roundtrip used 551 bytes and restored fresh generation 1. The default-feature core check passed at `50563242c7f146a3534ff73bdff7a5b8a9626c99`; its unchanged runtime/manifest/lock inputs permit reuse for the final candidate. Later corrections changed test fixtures only. Independent evaluation checked the exact published source and proof logs. These results establish the admitted headless in-memory tab and layout-recovery scope on that host. Host file IO/interrupted-save recovery, GUI, provider engines, Windows ARM64, WASM, other platform runtimes and performance remain unproven. WP007/WP008 results above are historical evidence for their respective candidates.

</topic>

<topic id="errors-and-recovery">

## Inspect failures and recover

`Error` distinguishes invalid/foreign identity, missing typed targets, duplicate child ownership, containment cycles, invalid indexes, unsupported sharing, cross-frame group mismatches, admission, close veto, pin restrictions, closed targets, maximize guards, stale restore/preview/generation, retained intent mismatch, expired/zero operations and exhausted counters. Errors include relevant IDs or expected/observed values and implement Rust's error/display interfaces. Arrangement changes validate a detached candidate before replacing live records, generation or allocation; rejected candidates leave canonical state intact. Hosts cannot mutate the live snapshot directly.

On `StaleGeneration`, read the current snapshot and resubmit intentionally with a new operation sequence. On `IntentMismatch`, inspect the original envelope; replay it exactly only while retained. On `ExpiredOperation`, inspect canonical state and reconcile intent before issuing a new sequence. On `IdentityMismatch`, obtain the current scope and reconcile the command rather than substituting an identity on an old token. On `RestoreConflict`, prepare again against current state. On admission rejection, adjust host policy explicitly or reduce/reuse existing placements. On maximize exit geometry failure, repair retained context/float bounds before retrying exit. On cycle or invalid index rejection, inspect current parent relations and order. Counter exhaustion never wraps.

Core diagnostics contain arrangement metadata, IDs and host-supplied labels/actor names; do not put secrets or private content in those strings. No filesystem, network, process or clock effects are performed. Snapshot copies are useful for complete pre/post comparison, while ledger diagnostics separately expose operation retention. Core mutations are synchronous in memory; the content companion separately exposes pending and Unknown host-effect obligations.

</topic>

<topic id="content-protocol" ingestable="true">

## Coordinate replaceable content

Add a path dependency on `product/kumihimo-content`; its sole dependency is Core 0.3 with default features disabled. `src/protocol.rs` defines public typed messages; `routing.rs` owns the controller and canonical transition observation; `scheduler.rs` implements causal bounded dispatch; `lifecycle.rs` implements acknowledgments, cancellation and recovery; `diagnostics.rs` handles declared cache reservations and redacted host samples. Public consumers are `tests/recording_host.rs` and the quiet `examples/recording_host.rs`. Observed WP010 source `ba2d6189704ced50c621dab18e6140e301aab136` passed the affected Windows x64 all-target check, all twelve recording-host consumers and quiet example; independent evaluation is `PASS_SCOPED_RUNTIME`. The example reports one instance reservation, one mount and two terminal receipts. Actual engines, GUI, text/asset behavior, physical resources/performance and other platforms remain unproven.

Construct `Controller::new(core, fresh_session, ContentLimits::default())`. The host supplies a nonempty session of at most 128 UTF-8 bytes and guarantees uniqueness across controller lifetimes. `new_with_id_start` accepts a nonzero host sequence offset for deterministic exhaustion checks; it seeds no effect state. `core()` and `snapshot()` expose immutable inspection; route every canonical `Envelope` through `execute`. Its outer error is a companion pre-admission veto; the inner Core response retains canonical error and replay behavior. There is no mutable Core escape or extraction API. Successful identical-ID live restore retires bindings; retained/rejected Core operations do not repeat retirement. Unrelated labels/generations preserve relevant acknowledgments.

Negotiate exact protocol 2.0 (`ProtocolVersion::V2`) and intersect typed host/provider capabilities with `negotiate`. A version/close-mode incompatibility returns a structured error without changing prior protocol state. `placeholder(view)` preserves canonical labels and recovery references while provider availability, negotiation or attachment is unresolved. Capabilities declare content modes, embedded input, anchors, lifecycle, restoration, close mode, tools/assets and cache support. They grant no permission by themselves.

Call `attach(view, options, permission, tick)` to reserve a distinct mount and, unless explicitly shared, an instance. Sharing requires Core capability, negotiated sharing and a host grant. Call `dispatch(mount, fresh_permission, tick)` to pull the next typed request. The host performs the effect and returns `Ack { ticket: request.ticket, result }` to `ack`. Exact ticket scope includes controller/Core identity, provider/resource/typed target, connection, mount, instance, request and channel revisions. Attach confirms protocol state only; host `SetViewState`/`SetToolState` remain separate canonical assertions. Moves preserve content identity. Missing callbacks, hidden controls or metadata Drop never dispose engines.

Geometry validates finite rect/clip and positive scale. Optional opaque anchors require support and exact echoed completion. Input owns a declared content/embed recipient; frame chrome is rejected by the provider-input route. Coordinates require the captured geometry clip. Input/tool/asset actions capture geometry/focus prerequisites. Only unsent geometry/focus/presentation/invalidation within one segment coalesce; input, tool/asset, close, lifecycle, attach/detach and anchored geometry form barriers. An action waits for confirmed prerequisites; later queued values cannot overtake it or reinterpret its meaning. `None` from dispatch exposes pending/backpressure; choose another independent mount to continue. Shared mounts serialize one instance. Provider callbacks never run synchronously inside arrangement execution.

Host millisecond ticks must be monotone. `advance` and both dispatch routes observe deadlines (default 30,000 ms). Queued work expires at its deadline before provider exposure, records `queued deadline`, and rejects dependent work. `cancel` records intent. Unsent expiry or cancellation releases only uncommitted attachments; an ever-confirmed instance stays reserved after its last detach until exact disposal or reconciliation proves cleanup. Dispatched ambiguity or timeout holds the instance Unknown until exact reconciliation. `recover_mount` uses the mount's reserved slot for detach, cache release or reconciliation; `recover_instance` uses the instance slot for disposal or reconciliation. `dispatch_recovery` pulls these fixed-size typed records even when ordinary queues or limits are exhausted. Reconciliation queries may be replaced after timeout with a fresh identity; the original uncertain effect remains held. Delayed duplicates never match a reused slot. Confirmed old cleanup releases only its old lease, never a replacement. Retained exact ACKs are historical/idempotent; differing duplicates conflict, and evicted terminal IDs expire.

Use `set_eligibility` with increasing revisions for host pressure/priority, recency/idle, keep-alive, unsaved/audio/download/protection and destruction eligibility. Shared-instance destruction checks every consumer and requires supported disposal/unload, host authorization and recoverable reference. Provider-veto close requests a `CloseDecision`, then requires an exact provider-approved permit at new host-approved Core close. The host's original `approved=false`, canonical missing/closed/pin errors and retained replay delegate unchanged. Provider approval cannot override host veto; only successful fresh close consumes permission. Close is separate from detach/disposal.

Asset references and linked-edit/copy/domain-change requests remain opaque host identity/revision plus explicit document/domain undo scope; arrangement movement never edits assets or domain data. Tools bind canonical `ToolId` and pinned `EditingTarget`. Delivery rechecks open/available/visible/usable/applicable origin and target, active attachment and fresh permission. Rebinding away and back invalidates queued actions; toolbar focus never substitutes a target.

Defaults are 1,024 providers, 256 held mounts, 64 instance reservations, 1,024 retained lease entries including retired/Unknown, 1,024 ordinary pending/unsent/terminal records, 65,536 bytes per opaque field/payload, 256 cache/render records and 64 MiB declared cache reservations. Actual retained companion request/receipt opaque copies share a 16 MiB cap; Core receipts and host physical memory are separate. `None` is explicit unrestricted policy. Admission/coalescing errors are atomic; lowering preserves existing obligations while blocking growth. Recovery slots cannot be consumed by ordinary work; unresolved obligations cannot be evicted for admission.

`reserve_cache` records exact instance/owner/consumer declarations. Finite byte policy requires a declared amount; sharing requires a grant. Exact confirmed `CacheRelease` removes one consumer and preserves others. Retired mounts use their recovery slot for cache cleanup before disposal. `cache_bytes` is the known declared subtotal; `cache_unknown_reservations` reports absent amounts separately. Neither is physical GPU/memory usage. `record_sample` takes typed units and measured/estimated/Unknown provenance; `diagnostics()` omits opaque input, asset payload and anchors. Invalidation, animation deadlines, selected/visible/focused/minimized and unknown occlusion remain separate.

On stale scope/permission, reread canonical and protocol state before new intent. On Unknown, reconcile the exact old request before destructive work; do not resend blindly. On exhausted counters, exact existing ACKs remain usable; constructing a fresh controller does not prove old host obligations disappeared. Retain the old controller/host ownership until explicit cleanup. Text layout, selection/scroll, native focus, engines, asset storage, physical usage, GUI/platform behavior and performance need later host integration proof.

</topic>

<topic id="navigation-and-overview" ingestable="true">

## Supply navigation without owning the document

Observed WP011 source `551ea5731931611f5a5a24f5c422684247edbdfb` passed all four feature checks, all 24 public cases and the quiet controls example on Windows x64 MSVC/rustc1.91.1. Independent evaluation is `PASS_SCOPED_RUNTIME`; real movement case005 was reproduced RED and proved GREEN, including nested/custom-packing prerequisites. Other platforms, actual GUI/provider engines and physical/performance acceptance remain open.

`navigation` is optional and disabled by default. Enable it on the content dependency to expose `navigation.rs` and `overview.rs`; shared field resolution lives in `settings.rs`. Core 0.3 and saved-layout schema 1 remain unchanged. Protocol 2 uses `Scope.target: MountTarget`; protocol 1 and unknown versions reject. The twelve WP010 consumers keep their original behavior assertions after this fixture migration.

For an attached view's exact scope, supply `DocumentModel` and a fresh `NavigationObservation` with `update_navigation`, then `RailGeometry` with `update_navigation_rail`. Models use host document units; observations report actual offsets independently of requested movement. `update_overview` accepts a sparse host-produced overview and semantic targets, never full document text. Default bounds are 4,096 markers per overview, 65,536 aggregate markers, 65,536 overview UTF-8 bytes and 256 bytes per opaque field; retained copies also consume the content controller's aggregate byte budget. Invalid replacement is atomic.

Use `configure_navigation(scope, revision, layers, policy)` to resolve version-2 field patches from defaults, host profile, workspace and surface. `Resolved` fields include source provenance. Choose `ScrollbarOnly`, `MinimapAlways`, `ScrollbarAndMinimap` or `ScrubToMinimap`; inspect `navigation_projection` for selected/effective mode and explicit unavailable/fallback reasons. Standard rail/thumb/minimum/hit dimensions are 12/8/24/24 DIP; Comfortable dimensions are 24/16/48/40 DIP. Visible size and hit size are independent. The host owns native recreation/restart, document semantics and actual accessibility integration.

Feed `navigation_press`, captured `navigation_move` and `navigation_settle` with checked gesture/pointer IDs. The temporary map expands beside the document while the held viewport rectangle keeps its original anchor; stationary press emits no movement. Release returns idle controls; cancellation settles local/unsent work without undoing provider effects. `navigation_key` supports line/page/Home/End even when an enlarged thumb leaves no pointer travel. `navigation_jump` uses the host overview's semantic target. Pull requests through the existing `dispatch` route and acknowledge exact typed observations. Only fresh observations or typed ACKs can confirm actual offset; generic Applied reconciliation settles an old obligation while actual values remain Unknown. Model/reflow/rail/settings/binding changes invalidate the gesture rather than inventing rollback.

</topic>

<topic id="background-and-media-controls" ingestable="true">

## Mount backgrounds and request bounded media work

`backgrounds` is independently optional and disabled by default. `backgrounds.rs` exposes visual policy and media work requests. `attach_background` targets a real `FrameBackground(FrameId)` or `WorkspaceBackground(WorkspaceId)` with a canonical resource; it creates no dummy tab or layout record. At most one mount targets each surface. Explicitly granted background consumers may share the same canonical resource/instance; mixed view/background engine sharing rejects. Restore retires old bindings, movement preserves background identity while invalidating relevant geometry, and closing a view affects its view mount.

`background_settings` resolves brightness, opacity, fit, crop, legibility overlay, reduced motion, poster and decorative audio under negotiated capability and host policy. Defaults are brightness 1, opacity 1, Cover, BehindContent, overlay 0.35 and muted audio. Ordinary input/focus/tool/asset/navigation routes reject on background mounts. The adapter composites behind content and supplies the actual rendering, decoder and accessibility behavior.

`media_policy` declares Run, Throttle, Paused or Poster; `media_host_ceilings` adds host hard limits. Default work ceilings are 30 fps and 1,920Ã—1,080. Active consumers combine finite ceilings componentwise by minimum, with host ceilings also enforced; `None` adds no ceiling. These values limit work, not promised quality. Pause requires every reserved consumer explicitly Paused/Poster and no protected/audio/keep-alive/unsaved/download veto. A visible explicitly paused consumer is allowed; hiding or opacity alone changes no work policy.

Pull `MediaBudget` through the same bounded queue and return exact typed `MediaObservation` for its aggregate revision. `media_satisfaction` distinguishes requested/current policy from confirmed or Unknown actual work. Generic reconciliation cannot synthesize an applied budget. A newer consumer policy invalidates old satisfaction; dispatched ambiguity retains recovery obligations. Shared cache/mount/started-instance leases remain reserved until their exact cleanup confirmation. Metadata Drop, hidden controls and declared reservations never prove physical memory/GPU release.

The quiet `controls_host` example supplies a navigation observation and a frame-background media observation through a recording provider. It opens no window and runs no engine. The four feature compile combinations and focused content tests are the WP011 proof surface; actual GUI readability/capture, assistive technology, host engines, performance and unobserved platforms require later integration proof.

</topic>

<topic id="native-backend-selection">

## Native graphics backend

With the `native` feature on Windows, `native::run_with_backend(controller, host, root, options, NativeBackend::Vulkan)` selects Vulkan exclusively. Existing `run` and `run_with_recovery` calls retain DX12, and `NativeOptions` keeps its existing fields. Native-app hosts use `run_with_recovery_and_backend` and retain any returned recovery owner through the existing recovery workflow.

`NativeBackend::configuration()` supplies the corresponding WGPU configuration; `validate_actual()` checks an observed adapter backend. The backend-selecting runners verify the actual adapter before accepting its window binding. For a strict Vulkan request, adapter or device unavailability returns `AdapterError::Unsupported` with a bounded diagnostic on stderr. A Vulkan request never silently falls back to another graphics API. Existing DX12 error classification is preserved. Host GPU accounting, providers, resource policy and persistence remain host responsibilities.

From the Kumihimo root, the existing probe accepts `--backend vulkan` or `--backend dx12`. Add `--eframe` for the separate framework integration. Both modes record the actual backend. The eframe probe stays hidden, drives repaint only for its own current window handles, and bounds startup and capture work with its internal deadline. Its startup readbacks remain separate from the receipts that authorize content handoff. Packet state records which runtime cases have actually passed.

</topic>
