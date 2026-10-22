---
file_id: RESEARCH-KUM-MODULARITY-v1
file_kind: research-basis
updated_at: '2026-10-11'
---

<topic id="scope-and-evidence" wp="WP-KUM-005-modularity-and-technical-research-v1">

# Kumihimo modularity and technical gaps

Three read-only agents examined the portable core, optional GUI adapters and host-content integration. Findings were compared with the existing master spec and reconciled here. Sources were checked on 2026-10-11; concrete APIs remain candidates and require pinning before implementation. No library was selected, no code was implemented and no runtime or performance result is claimed.

Accepted organization is now recorded in [master-spec revision 4](../../specification/current/spec-modules/master-spec.md), principally `TOPIC-KUM-BOUNDARIES`, `REQ-KUM-BOUNDARIES-005–007` and `DEC-KUM-ARCHITECTURE`: three logical submodules, small cohesive Rust modules/packages, independently selectable adapters, and host-owned content engines. This note carries no product authority. The technical additions below are proposals for further discussion.

The spec already covers recursive containment and cycle rejection, separate content/view identities, generation-checked commands, geometry/clipping, close vetoes, recovery placeholders, lifecycle confirmations and separate saved/live budgets. Most findings make those contracts more concrete. They are not evidence that whole features were missing.

The third submodule is **host-content integration**: the contracts and glue for an editor, browser or renderer supplied by the host. It does not bundle all those engines into Kumihimo. Border-effect exploration remains in the earlier research note; no effects subsystem or mandatory shader dependency follows from this organization.

</topic>

<topic id="package-boundaries" wp="WP-KUM-005-modularity-and-technical-research-v1">

## Bundle by purpose and dependency

`IDEA-KUM-TECH-001` — proposed physical organization, within the accepted modularity rule:

| Responsibility | Sensible initial grouping | Reason to split later |
| --- | --- | --- |
| Core | Identity, recursive ownership, tabs/groups, layout constraints and semantic commands as internal modules of one small portable package | An independently reusable responsibility has a genuinely different dependency or release boundary |
| GUI adapters | Independently selected toolkit/platform integration, with reusable controls inside it | Native versus web support, platform bindings or renderer dependencies differ substantially |
| Host-content integration | Small shared content contracts with concrete engine/render bridges independently selected | Servo, media, GPU or native-child-surface integration requires a different dependency stack or owning thread |
| Persistence/navigation helpers | Keep cohesive pure data/interaction helpers with their owning module | A separately reusable implementation brings substantial optional dependencies |

This is a package-boundary proposal, not a fixed package list. A thin convenience entry point could help consumers, provided it does not activate every adapter. Separate crates for tiny geometry types or individual buttons would add coordination cost without establishing a useful boundary.

Cargo combines dependency features across users and recommends additive features; another dependency can re-enable defaults. Therefore a core with backend-selection flags can acquire dependencies unexpectedly. The proposed control is downstream concrete adapter packages plus deliberate default features, checked through a minimal consumer's resolved dependency graph. [Cargo features](https://doc.rust-lang.org/cargo/reference/features.html)

[Taffy's manifest](https://github.com/DioxusLabs/taffy/blob/main/Cargo.toml) demonstrates cohesive geometry/layout functionality with selected algorithms, tree support and serialization features. Its [low-level API](https://github.com/DioxusLabs/taffy/blob/main/src/lib.rs) is a precedent for using algorithms with externally owned storage. It is not a selected docking implementation.

High-return reuse: use the same shared IDs, geometry data and commands across adapters; inject host scheduling, persistence and resource policy. This avoids duplicated workspace state and hidden runtime ownership. Future proof: inspect a minimal core consumer and each implemented adapter consumer; exercise supported combinations and confirm no mandatory GUI/GPU/browser dependency enters the core. Claims of shorter builds require actual measurements later.

</topic>

<topic id="core-proposals" status="proposed" wp="WP-KUM-005-modularity-and-technical-research-v1">

## Core: ownership, commands and durable state

### IDEA-KUM-TECH-002 — Decide where a nested workspace lives

Existing coverage: `REQ-KUM-BOUNDARIES-002/003` and `REQ-KUM-LAYOUT-006`. The remaining choice is whether a child workspace belongs to its frame shell, a tab's content slot, or explicitly supported variants. A **workspace inside a tab** is a useful concept extension: one tab could hold a complete research or editing arrangement.

Proposed invariant: one placement parent for each owned frame/workspace, separate from split/grid nodes and host resource references. Moving a subtree preserves all contained identities and providers; parent close has a declared policy for protected descendants. This reuses the existing identity and close-veto contracts. Risk: parent indexes disagree or moving a branch accidentally clones content. Control: one canonical ownership relation, derived indexes and validated moves. Future proof: move a nested subtree, reject a descendant cycle and close a parent whose child vetoes closure.

Precedents: [egui_tiles storage](https://github.com/rerun-io/egui_tiles/blob/main/src/tiles.rs), [Zed pane groups](https://github.com/zed-industries/zed/blob/main/crates/workspace/src/pane_group.rs) and [generational handles](https://docs.rs/slotmap/latest/slotmap/). Layout-tree nesting alone does not establish Kumihimo's full window-containment semantics. Internal storage keys need not become durable external IDs.

### IDEA-KUM-TECH-003 — One command path with explicit effect results

Existing coverage: `REQ-KUM-MODEL-001/002`, `REQ-KUM-POPOUTS-002` and `REQ-KUM-RECOVERY-005`. Proposed detail: GUI, keyboard and model commands pass through one portable validator/operation handler. It produces state changes and explicit host/adapter effect requests; external operations remain pending until acknowledged. Adapters do not maintain competing canonical workspace trees.

Useful extensions: preview an arrangement before committing it; combine a continuous resize gesture into one layout action; later add layout undo with a clearly limited scope. Layout undo cannot promise to reverse content edits, browser navigation or disposed resources. Risk: a destination fails after source removal, or a late result duplicates a view. Control: prepare/commit/abort outcomes, generation checks and operation-result reconciliation. Future proof: equivalent commands through two presentations, destination failure, stale generation and late acknowledgment.

[Iced's update model](https://book.iced.rs/first-steps.html) separates state changes from view construction; [Qt commands](https://doc.qt.io/qt-6/qundocommand.html) provide merging/grouping precedents. [egui_tiles Tree](https://github.com/rerun-io/egui_tiles/blob/main/src/tree.rs) also performs tree simplification during GUI processing, a detail to reconcile if adapting its layout model.

### IDEA-KUM-TECH-004 — Save meaning and rebuild presentation caches

Existing coverage: `REQ-KUM-RECOVERY-001–004`. Proposed detail: durable IDs, ownership, proportions, ordering, settings and provider restore references are independent of internal storage handles, computed rectangles and toolkit widget state. Provider payloads remain opaque, versioned and bounded; unavailable-provider references survive in the already-required placeholders.

Benefit: changing a GUI adapter need not invalidate saved workspace meaning. Risk: a save accidentally captures temporary caches or an opaque payload cannot migrate safely. Control: explicit portable schema, provider/schema versions, declared preservation/rejection rules and last-valid-save recovery. Future proof: restore with a missing provider and a different presentation adapter; retain identities while recalculating geometry. [egui_tiles](https://github.com/rerun-io/egui_tiles/blob/main/src/tiles.rs) excludes its computed rectangle cache from serialization.

### IDEA-KUM-TECH-005 — Notify only affected areas

Existing coverage: `REQ-KUM-PERFORMANCE-001/003` and `REQ-KUM-MODEL-001`. Proposed detail: operation results identify changes to topology, geometry, selection, chrome, navigation or content demand, with affected IDs and a generation. Full snapshots remain the recovery path. A label change should not need to wake every browser or recompute every nested layout.

Risk: overly clever invalidation produces stale controls; recursive traversal can exhaust the stack even without an architectural nesting ceiling. Control: conservative recomputation first, explicit invalidation dependencies, iterative traversal where needed and host-selected work budgets. Future proof: compare incremental results with complete recomputation, recover a missed generation and traverse a deeply nested saved structure. [Xilem's View contract](https://github.com/linebender/xilem/blob/main/xilem_core/src/view.rs) provides incremental rebuild/routing/teardown precedent.

</topic>

<topic id="gui-and-geometry-proposals" status="proposed" wp="WP-KUM-005-modularity-and-technical-research-v1">

## GUI: precise interaction contracts

### IDEA-KUM-TECH-006 — One coherent geometry snapshot

Existing coverage: `REQ-KUM-RENDER-001/004` and `REQ-KUM-TEXT-001`. Proposed detail: distinguish frame bounds, content bounds after controls, parent-local coordinates, native-window coordinates and physical pixels. A geometry revision joins transforms, effective ancestor clip, scale and attachment generation. Painting, picking, previews and provider notifications refer to the same snapshot.

Benefit: nested windows, custom headers and mixed-DPI popouts use consistent mapping. Risk: double-scaling or delayed input targets the wrong content. Control: tagged coordinate spaces, composed transforms and an explicit stale-event rebase/rejection policy; zero-sized visible regions need an inactive state rather than invalid GPU allocation. Future proof: known-point picking through nested frames, taller replacement controls, monitor-scale change and resize during dragging. [Euclid transforms](https://docs.rs/euclid/latest/euclid/struct.Transform2D.html), [winit events](https://docs.rs/winit/latest/winit/event/enum.WindowEvent.html) and [egui-wgpu viewport/scissor code](https://github.com/emilk/egui/blob/main/crates/egui-wgpu/src/renderer.rs) are relevant boundaries.

### IDEA-KUM-TECH-007 — Give every gesture one owner

Existing coverage: `REQ-KUM-LAYOUT-003`, `REQ-KUM-INPUT-001/004` and `REQ-KUM-NAVIGATION-006`. Proposed detail: a drag/resize/scrub session identifies its source window/workspace, pointer, target and starting generation. Parent and child workspaces cannot both start arrangement operations from one gesture. Define lost capture, second touch, Escape, removed target, focus loss and disposal.

Risk: a browser intercepts motion, a preview remains stuck, or release accidentally activates underlying content. Control: settle once, release temporary input shields and suppress only that gesture's trailing activation; cross-window capture is a separate capability. Future proof: exercise every termination path in a nested workspace and inspect retained input ownership. [Dockview's pointer controller](https://github.com/dockview/dockview/blob/master/packages/dockview-core/src/dnd/pointer/pointerDragController.ts) uses source-window listeners and iframe/text-selection shields; [Pointer Events](https://www.w3.org/TR/pointerevents3/) specifies capture and cancellation.

### IDEA-KUM-TECH-008 — Separate focus, selection and content activation

Existing coverage: `REQ-KUM-INPUT-002–004` and `REQ-KUM-POPOUTS-004/005`. Proposed detail: explicit nested focus scopes, enter-child/return-parent routes and restoration after closure. A focused tab header, selected tab and focused content are distinct. Providers own identified accessibility subtrees; the GUI composes them and routes actions to their owners.

Benefit: keyboard browsing can avoid waking expensive tabs. Risk: duplicate accessibility IDs, double-handled shortcuts or inaccessible focus after closing a child. Control: namespaced nodes, one focus route and a reachable fallback; activation policy remains exposed to the host. Future proof: keyboard navigation through nested/floating/popout views, child closure and inspection of the actual accessibility tree. [AccessKit TreeUpdate](https://docs.rs/accesskit/latest/accesskit/struct.TreeUpdate.html) and [W3C tab guidance](https://www.w3.org/WAI/ARIA/apg/patterns/tabs/) provide precedents; the latter conditions automatic activation on low content latency.

### IDEA-KUM-TECH-009 — Custom controls publish semantics and measurements

Existing coverage: `REQ-KUM-CHROME-001–005` plus the newly accepted replaceable GUI boundary. Proposed detail: header/tab/group/splitter slots expose state, supported actions, measured space, hit regions and accessible descriptions. A replacement uses the same close vetoes and commands. Visible line thickness and resize hit width remain independent.

Benefit: hosts can restyle controls without reimplementing arrangement behaviour. Risk: a tall custom header overlaps content or a painted tab loses dragging/accessibility. Control: register actual geometry and actions, retain discoverable command/reveal paths. Future proof: replace one tab strip with a taller control and verify drag, keyboard, close veto and content bounds. [egui_tiles Behavior](https://github.com/rerun-io/egui_tiles/blob/main/src/behavior.rs) provides concrete tab/drag painting, measurements and close hooks; its toolkit types remain adapter-local.

### IDEA-KUM-TECH-010 — Cache minimap content separately from its viewport marker

Existing coverage: `REQ-KUM-NAVIGATION-003–006`. Proposed detail: provider revision, overview extent/resolution, changed ranges and coordinate-to-content mapping. Scrolling moves the viewport marker; it need not regenerate an unchanged overview. Pending or out-of-order overview results cannot block the ordinary scrollbar or the continuous drag.

Risk: an edited document makes the old map send a jump to the wrong location. Control: revision-tagged results, provider-owned stable anchors or an explicit fallback, and stable document layout during expansion. Future proof: long virtualized content changes while scrubbing; delayed results arrive out of order; all four modes remain usable. [VS Code minimap](https://github.com/microsoft/vscode/blob/main/src/vs/editor/browser/viewParts/minimap/minimap.ts) separates content changes, sampling/mapping and scrolling. Its editor document model is not imported into Kumihimo.

### IDEA-KUM-TECH-011 — Explicit overlapping-frame policy

Existing coverage: `REQ-KUM-POPOUTS-001/004` and `REQ-KUM-RENDER-002`; more elaborate overlap grids remain exploratory. If adopted, declare stacking order, exposed input regions, raising policy and modal/nonmodal/pass-through behaviour. Transparent pixels alone do not define click-through. Decorations, content and input can have different bounds.

Benefit: the centre-window idea can leave exposed background usable without adding effects. Risk: a translucent overlay sends unexpected clicks behind it or traps the whole parent. Control: explicit input/modality policy and adapter capability rejection. Future proof: central nonmodal frame, exposed background interaction, keyboard switching and overlap hit testing. [egui Area](https://github.com/emilk/egui/blob/main/crates/egui/src/containers/area.rs) separates ordering and interaction; [Dockview accessibility](https://dockview.dev/docs/advanced/accessibility/) describes nonmodal floating groups.

</topic>

<topic id="host-integration-proposals" status="proposed" wp="WP-KUM-005-modularity-and-technical-research-v1">

## Host integration: rendering and engine ownership

### IDEA-KUM-TECH-012 — Negotiate a concrete composition path

Existing coverage: `REQ-KUM-RENDER-002`. Proposed detail: selected adapter capabilities describe pixel format, colour/alpha conventions, clipping shapes, ordering, synchronization and final presentation ownership. GPU objects stay in the matching renderer bridge; portable contracts use capability descriptions and opaque identity.

Risk: a universal compositor becomes another large package, or a browser is promised unsupported transparency/clipping. Control: describe the chosen path and reject unsupported combinations; independently select its bridge. Future proof: two actual provider types, overlap, alpha edges and an unsupported clipping request. [egui-wgpu callbacks](https://github.com/emilk/egui/blob/main/crates/egui-wgpu/src/renderer.rs) separate preparation/painting and submission order; [WebView2 composition](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2compositioncontroller?view=webview2-1.0.3912.50) and [wgpu Surface](https://docs.rs/wgpu/latest/wgpu/struct.Surface.html) illustrate different integration constraints. No path is selected here.

A concrete bridge constraint: egui-wgpu's callback trait requires Send/Sync, while the current [Servo WebView](https://doc.servo.org/servo/webview/struct.WebView.html) has neither. The design inference is that directly capturing that browser handle in this callback cannot be assumed valid; the selected bridge must respect engine-thread ownership and prove its rendering handoff.

### IDEA-KUM-TECH-013 — State the strength of a popout transfer

Existing coverage: `REQ-KUM-POPOUTS-002/003`, `REQ-KUM-CONTENT-002`, `REQ-KUM-RECOVERY-005` and `REQ-KUM-SERVO-005`. Proposed detail: distinguish live attachment movement, surface recreation, restored view and content reload. Destination/attachment generations identify one active presentation/input owner. Destination preparation, commit, abort and uncertain outcomes run through the owning thread.

Benefit: a host can tell users what survives moving a view. Risk: last-handle destruction, duplicate rendering or stale input; retain source ownership until commit is confirmed and reconcile late results. Future proof: destination failure, lost acknowledgment, removed return pane and mixed-DPI cross-window transfer. [Servo WebView](https://doc.servo.org/servo/webview/struct.WebView.html) is not Send/Sync and cloning its handle retains the same view; [SetParent](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setparent) has style/DPI constraints. These findings do not prove any transfer method works for Kumihimo.

### IDEA-KUM-TECH-014 — Separate engine work, painting and presentation

Existing coverage: `REQ-KUM-PERFORMANCE-001/003`. Proposed detail: providers publish dirty revisions and animation deadlines; the scheduler acknowledges the revision actually painted. Adapter presentation has one declared owner. Engine wakeups remain separate from visible repaint.

Risk: a new invalidation arriving during paint is lost, or hiding a view incorrectly stops necessary engine work. Control: retain pending revisions and distinct schedules, injected by the host. Future proof: static and animated providers together, invalidation during paint and a hidden engine wakeup; measure before claiming savings. [Servo delegates](https://doc.servo.org/servo/webview_delegate/trait.WebViewDelegate.html) separately expose new-frame and animation notifications; [winit redraw requests](https://docs.rs/winit/latest/winit/window/struct.Window.html#method.request_redraw) are asynchronous.

### IDEA-KUM-TECH-015 — Declare shared-resource domains

Existing coverage: `REQ-KUM-LIFECYCLE-001–005` and `REQ-KUM-PERFORMANCE-004/005`. Proposed detail: distinguish sharing by view, workspace, profile, host and process; account for shared allocations and teardown still pending. Expose estimate confidence, coupling and protection reasons. Saved metadata remains distinct from admitted live resources.

Benefit: one thousand saved tabs need not imply one thousand live engines, and moving a view need not create an independent resource budget. Risk: double-counted memory or unloading a view that disrupts siblings. Control: host-owned shared ownership/accounting and acknowledged lifecycle transitions. Future proof: shared resources, independent profiles, delayed teardown and a large saved collection under a small live budget. [Chromium discard policy](https://chromium.googlesource.com/chromium/src/+/HEAD/chrome/browser/performance_manager/policies/page_discarding_helper.cc) illustrates estimates and eligibility. [Servo resizing](https://doc.servo.org/servo/webview/struct.WebView.html#method.resize) affects views using the same RenderingContext, an explicit coupling requiring adapter proof for independently sized panes.

### IDEA-KUM-TECH-016 — A bounded content interaction bridge

Existing coverage: `REQ-KUM-CONTENT-001/004`, `REQ-KUM-INPUT-003/004` and `REQ-KUM-POPOUTS-004/005`. Proposed detail: IME/preedit and caret placement, capture, cursor changes, transient controls and asynchronous consumed/unhandled event responses. Requests identify the view, attachment generation, anchor and cancellation state; the host GUI presents controls and routes responses.

Risk: typing shortcuts fire twice, capture remains stuck or a dropdown survives after its tab moves. Control: ordered request/results, one event owner, generation-bound responses and cancellation on detach. Browser-specific permission/authentication policy remains in its adapter. Future proof: IME typing, drag outside the view, delayed unhandled input and moving a view with a dropdown open. [Servo WebViewDelegate](https://doc.servo.org/servo/webview_delegate/trait.WebViewDelegate.html) includes handled-input and embedder-control callbacks; [winit IME](https://docs.rs/winit/latest/winit/window/struct.Window.html#method.set_ime_allowed) and [WebView2 input forwarding](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2compositioncontroller?view=webview2-1.0.3912.50#sendmouseinput) provide concrete boundary examples.

</topic>

<topic id="discussion-and-reuse" status="proposed" wp="WP-KUM-005-modularity-and-technical-research-v1">

## Useful concept extensions and decisions still open

These ideas reuse already-planned state and provider boundaries, so they can improve several applications at once:

- A tab containing a complete workspace. Decide frame-owned versus tab-owned nesting before finalizing ownership semantics; evaluate subtree moves and protected descendants (`IDEA-KUM-TECH-002`).
- A command preview that reports proposed destinations, geometry and rejection reasons before applying a move. It can power docking previews and model inspection through the same path; test preview/commit agreement (`003`).
- Optional layout undo grouped by completed gesture. Reuse operation IDs and snapshots; declare its limits and test operations with external effects (`003`). This carries forward the earlier optional layout-undo research.
- Cached minimaps whose marker moves independently, plus lightweight hidden-branch presentation. Reuse provider revisions and affected-area notifications; check stale data and missed updates (`005`, `010`). These are performance hypotheses pending measurement.
- Honest transfer capability descriptions: live move, recreate, restore or reload. Reuse existing readiness/recovery handling and prove the selected method with the actual provider (`013`).
- A shared diagnostic snapshot of ownership, geometry, focus, capture, capabilities and pending effects. Extend existing `TOPIC-KUM-MODEL`; keep content private and test causal reconstruction for interrupted operations (`003`, `006–008`, `013`, `016`).

No effects engine, universal compositor, compulsory runtime, per-control crate or dependency selection is proposed as a prerequisite. The accepted module organization and modularity requirement are in the master spec. The additional ideas remain discussion material; future acceptance can lift selected meaning into the relevant topics with focused proof scenarios. All product features remain planned.

</topic>
