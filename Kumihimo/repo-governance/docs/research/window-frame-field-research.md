---
file_id: RESEARCH-KUM-WINDOWS-v1
file_kind: research-basis
updated_at: '2026-10-11'
---

<topic id="scope-and-evidence">

# Kumihimo field research

Research covers reusable workspaces, docking, concealed controls, rendering boundaries and browser resource management. Two research agents investigated workspace interactions and rendering/lifecycle integration; the primary maintainer reconciled the findings into the specification after initialization was pushed.

This note is reference material. Accepted behavior lives in the [master spec](../../specification/current/spec-modules/master-spec.md). No library is selected, no prototype has run, and no benchmark or platform compatibility result is claimed.

</topic>

<topic id="workspace-implementations">

## Useful implementations

| Project and inspected source | Pattern found | Kumihimo use |
| --- | --- | --- |
| [VS Code layout](https://code.visualstudio.com/docs/configure/custom-layout) and [grid implementation](https://raw.githubusercontent.com/microsoft/vscode/main/src/vs/base/browser/ui/grid/gridview.ts) | Editor groups, configurable surrounding controls and constrained grid geometry. | Separate pane arrangement from content; allow compact/hidden controls with a route back. |
| [Zed pane groups](https://raw.githubusercontent.com/zed-industries/zed/main/crates/workspace/src/pane_group.rs) and [glossary](https://zed.dev/docs/development/glossary) | Recursive pane arrangements and separately described workspace/dock surfaces. | Explicit geometry, ownership and recoverable restored proportions. |
| [egui_tiles behavior source](https://github.com/rerun-io/egui_tiles/blob/main/src/behavior.rs) and [advanced example](https://github.com/rerun-io/egui_tiles/blob/main/examples/advanced.rs) | Rows, columns, grids and host behavior customization, including tab and resize controls. | Candidate for flexible Rust pane layout; source inspection is stronger than README feature summaries. |
| [egui_dock source](https://github.com/anhosh/egui_dock/blob/main/src/lib.rs) and [TabViewer contract](https://docs.rs/egui_dock/latest/egui_dock/widgets/tab_viewer/trait.TabViewer.html) | Host-owned tab content, close policy and floating presentation. | Candidate for docking integration. In-app floating windows do not prove native popouts. |
| [Dockview concepts](https://dockview.dev/docs/core/overview/) and [popout behavior](https://dockview.dev/docs/core/groups/popoutGroups/) | Identified panels/groups, nested layouts and observable popout handoffs; unsuccessful popouts retain or return panels. | Stable identities and destination-readiness before moving content. |
| [Dear ImGui docking](https://github.com/ocornut/imgui/wiki/Docking), [multi-viewports](https://github.com/ocornut/imgui/wiki/Multi-Viewports) and [backends](https://github.com/ocornut/imgui/blob/master/docs/BACKENDS.md) | Docking and native viewports rely on distinct platform/rendering support. | Keep frame layout, native windows and render integration as separate capabilities. |
| [Niri design principles](https://github.com/niri-wm/niri/wiki/Development%3A-Design-Principles), [tabs](https://niri-wm.github.io/niri/Tabs.html) and [overview](https://niri-wm.github.io/niri/Overview.html) | Scrolling workspaces, tabbed columns and an overview offer alternatives to shrinking every window. | Optional inspiration for crowded workspaces and tablet navigation. |

[Dockview licensing](https://dockview.dev/docs/overview/licence/) distinguishes its MIT core from commercial additions. Group chips/colors, docking, popouts and saved layouts are listed in the core; smart guides, layout history, auto-hide groups and extended keyboard docking are commercial additions. This distinction affects reuse evaluation; these features are not assumed available from one free dependency.

</topic>

<topic id="rendering-and-content">

## Rendering and content ownership

[winit window APIs](https://docs.rs/winit/latest/winit/window/struct.Window.html) expose redraw requests; [window events](https://docs.rs/winit/latest/winit/event/enum.WindowEvent.html) have platform-dependent coverage. Pane visibility is something the arrangement model can know. OS occlusion may be unavailable and must remain unknown rather than inferred.

[egui-wgpu rendering](https://github.com/emilk/egui/blob/main/crates/egui-wgpu/src/renderer.rs) and its [custom 3D example](https://github.com/emilk/egui/blob/main/crates/egui_demo_app/src/apps/custom3d_wgpu.rs) are concrete references for embedding graphics inside GUI bounds. They support considering an adapter; they do not prove Servo interoperability or this asset's performance. [wgpu surface outcomes](https://docs.rs/wgpu/latest/wgpu/enum.CurrentSurfaceTexture.html) provide a reference for handling unavailable/lost surfaces separately from layout identity.

The [VS Code minimap source](https://github.com/microsoft/vscode/blob/main/src/vs/editor/browser/viewParts/minimap/minimap.ts) uses editor-specific content information. A generic frame can provide a minimap slot, but the host must provide its meaning and jump targets. [egui scrollbar styling](https://docs.rs/egui/latest/egui/style/struct.ScrollStyle.html) exposes useful handle/rail settings; [ScrollArea](https://docs.rs/egui/latest/egui/containers/scroll_area/struct.ScrollArea.html) illustrates content virtualization. These are separate responsibilities.

[Dockview rendering modes](https://dockview.dev/docs/core/panels/rendering/) distinguish how panels are mounted and displayed. An invisible panel is not evidence that its video, subscriptions or other work stopped. Brightness/opacity settings control appearance; resource savings require provider scheduling and measurement.

</topic>

<topic id="servo-and-resource-lifecycle">

## Servo and large saved collections

The inspected [Servo RenderingContext](https://doc.servo.org/servo/trait.RenderingContext.html) uses GL/GLES. The [WebView API](https://doc.servo.org/servo/webview/struct.WebView.html) documents shared-context resizing: resizing one also resizes other views using that context. Its show/hide API describes visibility, not a comprehensive freeze/discard policy. Independently sized panes therefore need a proven adapter design. Direct wgpu composition remains unverified.

The [Servo embedding overview](https://book.servo.org/embedding/overview.html) documents desktop media setup; the [embedding release guidance](https://book.servo.org/embedding/lts-release.html) is relevant to pinning and isolating evolving engine APIs. Kumihimo has not proved Windows media, YouTube workflows or the ARM-host native dependency build. These require actual scenarios against a pinned adapter.

[Chrome page lifecycle guidance](https://developer.chrome.com/docs/web-platform/page-lifecycle-api) separates hidden, frozen and discarded states, including recovery and background exceptions. It is a policy reference, not evidence that Servo implements the same controls. [Windows resource notification](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-creatememoryresourcenotification) is one possible host pressure input; it cannot promise that a later Cargo or ComfyUI workload will fit.

The useful design inference is to separate saved tab records from live engines, reserve live capacity before creating engines, and require acknowledgment before claiming an unload or reduced background work. Unsaved work and host-declared audio/download protection need explicit handling. Reloading a page is weaker than preserving its arbitrary in-memory state.

[Rust Windows target support](https://doc.rust-lang.org/rustc/platform-support/windows-msvc.html) and [Visual Studio on ARM](https://learn.microsoft.com/en-us/visualstudio/install/visual-studio-on-arm-devices?view=visualstudio) are build-path references. Their existence does not prove all chosen native dependencies build on this tablet. Build-host and runtime-target evidence must stay separate.

</topic>

<topic id="selected-direction-and-alternatives">

## Selected direction and open choices

Adopted in the spec: portable Rust arrangement/state logic with platform, content, rendering, lifecycle and persistence adapters. Existing implementations provide reusable patterns; dependencies remain undecided.

Compare egui_tiles and egui_dock using a small actual workspace sample. Consider winit and an appropriate renderer for native integration, and Dockview for a web adapter. Evaluate its available features individually. Do not select a whole toolkit from documentation alone or make the browser engine a core dependency. These alternatives remain open rather than rejected as incapable.

Rejected approach: equating every pane with an independent native window and continuously live engine. It confuses in-app arrangements with popouts and makes resource control harder. Mitigation: keep presentation kind and live-resource allocation explicit.

Rejected claim: hidden content automatically consumes no CPU or memory. Mitigation: separate frame visibility from provider-confirmed lifecycle states and measure the actual resource path.

Deferred selection: one shared Servo render context for all independently sized panes. Shared resize behavior is an evidenced constraint; investigate separate contexts or another proven composition design before adopting it.

</topic>

<topic id="optional-inventive-directions">

## Optional proposals, not accepted build scope

| Proposal | Why it may help | Reuse, risk and focused proof |
| --- | --- | --- |
| Scrolling workspace lanes | Keep useful pane widths when more surfaces are added. | Reuse layout geometry and tab identity. Risk: losing spatial orientation; retain anchored focus and inspect touch/keyboard navigation. Inspired by Niri. |
| Zoomed workspace overview | Find a pane among many windows without shrinking ordinary controls. | Reuse bounds, titles and budgeted cached previews. Risk: expensive or private live thumbnails; use opt-in snapshots and measure update cost. |
| Auto-hide edge trays | Recover content space while keeping tools nearby. | Reuse chrome profiles and focus commands. Risk: undiscoverable controls; provide visible reveal hints and keyboard/touch proof. Dockview offers a commercial precedent. |
| Layout undo and named arrangements | Recover from accidental docking and switch work contexts. | Reuse versioned layout serialization. Risk: content ownership changes between states; restore arrangement references with conflict handling and close veto proof. |
| Snapping hysteresis | Reduce jitter near competing targets during resize/drag. | Reuse preview geometry. Risk: sticky targets feel unpredictable; tune touch/mouse thresholds and inspect slow boundary movement. Dockview smart guides are a commercial precedent. |
| Adaptive decorative backgrounds | Preserve visual style while reclaiming resources under pressure. | Reuse lifecycle/provider acknowledgments and poster frames. Risk: quality changes appear arbitrary; expose the effective policy and verify transition/recovery. |

These ideas increase the value of boundaries already being designed, but adoption still needs an explicit spec change. They must not silently enlarge a current work packet.

</topic>

<topic id="risk-and-focused-validation">

## Failure scenarios and minimum controls

| Failure scenario | Minimum control | Proof when implemented |
| --- | --- | --- |
| Popout fails after source removal, losing a view | Prepare destination first; canonical handoff state and recovery reference | Block or interrupt a real popout and inspect retained views. |
| Hidden tab keeps decoding or scripting | Requested versus confirmed lifecycle; host eligibility and budgets | Observe provider callbacks and measured workload. |
| Unload destroys unsaved or protected work | Close/unload veto and host protection reasons | Attempt unload with dirty content, audio and a protected download. |
| Tiny viewport crushes eight panes | Minimum sizes and disclosed overflow/fallback | Resize the actual GUI and reach every pane. |
| Corrupt saved layout creates cycles or huge allocations | Validate and bound restoration; retain last valid state | Load malformed/oversized records and inspect preserved content. |
| Custom or hidden bars trap keyboard/touch users | Accessible reveal, system control fallback and explicit focus routes | Operate the actual adapter without mouse dragging. |
| GPU loss wipes arrangement, or browser resize changes neighbors | Independent layout identity and adapter-proven render recovery | Recreate a surface; resize one of two browser panes. |
| Two models overwrite layout changes | Expected generation and canonical mutation owner | Send conflicting structured commands and inspect outcomes. |

Specification-only proof: check document framing, IDs, references, digests, roadmap coverage, original requirement preservation and the owned diff. No Cargo build is applicable yet. Runtime work later uses the affected compile check and focused scenario; GUI claims require the real rendered surface. No mandatory long suite or enrichment phase is introduced.

</topic>
