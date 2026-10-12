---
schema_version: handshake.spec-document.v1
specification_id: SPEC-KUMIHIMO-v1
document_id: DOC-KUM-ROADMAP-v1
revision: 22
title: Kumihimo roadmap
updated_at: '2026-10-12'
---

```yaml
kind: specification_topic
topic_id: TOPIC-KUM-ROADMAP
title: Roadmap
content_blocks:
- block_id: explanation
  kind: prose
  language: markdown
  requirement_refs: &id001 []
- block_id: contract
  kind: structured_contract
  language: yaml
  requirement_refs: *id001
```

```markdown

Authorized extension sequence (not implemented): WP018 real file-drop integration; WP019 selected external desktop-app tiling; WP020 supported foreign-window hosting; WP021 a live combined desktop sample using Servo and an installed application. Handshake is not required; Blender is an optional example. All six new requirement acceptances remain open.

Current inspectable surfaces are the desktop sample, the Rust/WASM Portal-style pilot and the separate browser/graphics experiment. Their appearance and sample content are demonstrations of the reusable module; actual Handshake modules remain separate integrations.

| Surface | Demonstrated scope | Remaining scope |
| --- | --- | --- |
| Portable Rust core/content contracts | Nested ownership, variable tabs/groups, packing/docking, tools, recovery and host-executed lifecycle/navigation/media contracts | Complete feature acceptance and real host/provider integrations |
| Windows x64 desktop sample | Native panes/popouts, arrangement controls, tool panels, four navigation modes, still backgrounds and scoped Windows accessibility actions | Physical input/display/assistive hardware, ARM and full editor/browser/media engines |
| Rust/WASM web pilot | Nested movable task/detail frames, tabs, resize/dock/float controls, separate task edits and validated save/restore in Chrome | Actual Portal integration, native popup migration, other browsers and complete accessibility coverage |
| Servo/2D/3D experiment | Two real Linux Servo processes under WSL, copied pixels in Windows DX12 scenes, resize/input, confirmed owned unload/page reload and renderer replacement | Native Windows Servo/libservo, zero-copy, video/audio, arbitrary page-state recovery and measured performance improvements |

Demonstrated describes only the recorded scenario. Partially implemented means some behavior exists with full acceptance still open. Planned describes the remaining integrations and unproven capabilities. All 105 full requirements remain open; all 18 feature areas retain partially_implemented status. The existing product manual lists the selectable examples and their run paths; the specification below keeps the product boundaries explicit.

WP014 scoped external-engine experiment at 8cbea2746233a0987d0cf7556c59deeef744486b: independently selectable std-only render-bridge with optional desktop integration. Two actual Servo0.6.0-c78d2c206 Linux x64 software processes under Ubuntu24.04/WSL produce eleven independently hashed/timed PNG captures consumed byte-for-byte in nine actual Windows x64 DX12/NVIDIA RTX3090 GPU scenes with real2D and transformed/depth-tested3D providers. Independent resize and genuine WebDriver pointer/full keyboard input pass at normal and injected scale1.5; canonical normal/narrow/moved slots remain clipped. Fifty-five outside-kernel observations prove private loopback namespaces, three owned listeners with outside-Linux denial, actual CPU/RSS observations, protected unloads, three confirmed engine exits, fresh A page reload and final owned cleanup. Hidden retains its engine; unsupported freeze/throttle/frame-ready/context transfer stay explicit. Static GPU comparisons retain full pixels and zero added uploads/depth renders after one bounded GUI fade settlement. Actual renderer replacement rejects old bindings and safely reuploads accepted CPU pixels. Thirteen owning cases passed; unchanged Core/content/JSON/desktop/web proof and warm Cargo/cache were reused. Native Windows Servo/libservo/zero-copy/video/audio/ARM/physical input/real GPU faults and performance gains remain unproven. Full99 future requirements remain OPEN; independent final evaluation/publication governs WP closure.

All 18 feature areas now have scoped partial implementation. All requirements remain open until their full acceptance surfaces are proven. Completed repository preparation is separate from product implementation.

WP009 proves the headless tab/group/pin/list, bounded schema1 JSON/semantic recovery and carried layout-operation scope. Core0.3 remains default std-only with optional saved-type Serde and an independent JSON package. The host storage protocol is documented; actual filesystem interruption/replacement and complete RECOVERY acceptance remain open.

WP010 proves the independent std-only content companion at ba2d6189704ced50c621dab18e6140e301aab136: scoped typed requests/ACKs, distinct mount/instance reservations, protected cleanup, asset/tool routing and honest diagnostics. The affected all-target check, all twelve public consumers and quiet example passed with independent scoped evaluation. Queued expiry is enforced before exposure; a confirmed instance stays reserved after its last mount detaches until explicit cleanup. Real host engines, GUI, text/asset behavior, physical resources and performance remain open.

WP011 observed headless implementation at source 551ea5731931611f5a5a24f5c422684247edbdfb: content 0.2/protocol 2.0, independent navigation/backgrounds features disabled by default, four successful Windows x64 all-target feature checks, twenty-four passing public cases (twelve preserved WP010 plus eight navigation/four backgrounds), and the quiet controls_host example. Independent evaluation is PASS_SCOPED_RUNTIME. F01 real frame movement was reproduced in existing case005 and passed after the routing correction; existing005/009 cover affected nested owners/custom packing, queued rejection/exposed Unknown, stable target/resource/instance and no-op/metadata/replay controls. Core0.3/JSON/schema1 remain unchanged. This proves typed host-fed policy, gestures, observations and bounded recovery, not native pointer capture, rendered GUI/readability/accessibility trees, actual provider/decoder engines, physical CPU/memory/GPU savings, performance or unobserved platforms. All full requirement acceptance remains open.

WP012 scoped Windows x64 MSVC/rustc1.91.1 desktop adapter at 7d9268ab90d5ed6e31d7af070cb73302765741cc: optional egui0.33.3 borrows the host Controller and content dispatcher. All29 owning tests and default/all-feature all-target checks passed. Eighty actual DX12 raster states cover nested variable counts, weighted holes/deep containment, groups/pins/veto, drag/cancel/commit, float/redock/equalize/swap/maximize/restore, tools, four chrome profiles, four navigation modes, mouse/keyboard and separately labelled semantic touch/host-pen input, still backgrounds and scale1/1.5/1. Float ordering survives persistent hide/restore; theme-backed headers and notices remain readable. Twenty-two actual hidden native captures prove correlated preflight, view/group/frame popouts, resize, teardown/recreation, three returns, partial-close survivors, removed-origin semantic return, bounded injected recovery failure with a visible healthy-root notice before recovery, and six Windows accessibility stages. Four genuine SelectionItem.Select actions expose OS false-to-true selection matching canonical changes; Panes open/dismiss and logical focus return passed, with18 forwarded actions total. Owned windows stayed hidden/nonforeground. The unchanged exact hidden-only AccessKit provider test remains valid; downstream workspace roots must apply the recorded local patch. All13 frozen evaluation cases passed independently. Physical input/assistive hardware, physical display changes, ARM64/other platforms, real Servo/video/editor engines, genuine GPU fault injection, performance gains and physical memory release remain unproven. All full future requirement acceptances remain open.

WP013 observed Rust/WASM and plain-DOM browser adapter at e6b48fd83a3e4a42d973247aad71d957ec66b82a: optional kumihimo-web0.1 borrows one host content Controller; the Portal-style pilot keeps task meaning separately owned. Six native consumers, affected default check and WASM build passed on unchanged Rust inputs at71b9acc630da. The actual Chrome154 headless batch passed all six original cases and eight inspected images: variable nested counts, mouse/keyboard arrangement, generation-bound previews and Escape/capture-loss/blur cancellation with unchanged layout through release, browser-emulated DPR1.5 content mapping and restored DPR1, host edits/domain commands, visible unavailable-provider recovery, independently read layout/task storage and fresh-incarnation restore, persisted missing-provider-key fault/recovery, rejected malformed/newer/oversized candidates and injected storage refusal, strict decimal model envelopes/replay and real restricted-popup denial retaining source ownership. The private browser was closed. The same-origin local iframe is a denial fixture, not untrusted content isolation; captured DOM roles are not a full accessibility-tree audit. Native popup migration is unsupported. Physical input/display changes, other browsers/platforms, actual Portal/Notes/Studio/Servo engines, performance improvements and physical resource release remain unproven. Full future requirement acceptance remains open.

The desktop and web prototypes already exercise host-configured recursive workspaces with variable window/tab counts and sample host-owned content. Desktop native popout/return and browser arrangement/save/recovery have recorded scoped runtime proof. Future work extends those boundaries into actual host modules and the remaining full acceptance scenarios; keep every existing roadmap requirement intact.

The browser/graphics experiment has exercised two independently resized Servo views and actual owned unload/page-reload behavior alongside 2D/3D content. Broader browser resource management, video backgrounds, linked engine integration, large saved-tab workloads and site compatibility still require their declared provider contracts and boundary proof.

Verification stays focused on the behavior a work packet changes. A real rendered interaction is required when claiming GUI behavior; broad test suites are not a substitute for that evidence.

## Implemented

WP-KUM-007 implements the first in-memory ownership core. The Windows x64 MSVC all-target compile check, nine public consumer tests, one private counter-exhaustion test and console example passed at source revision a004dbd4c655d6d193e288d21e37dc9987095353. Functional scenarios include variable 20-by-30 counts and 1,024 nested frame levels; they are not performance benchmarks. No complete future feature area is marked implemented.

WP-KUM-008 adds finite shared split/grid geometry, minima-first weighted allocation, explicit virtual-extent overflow, pinned tool placement/reservations, atomic configured-layout repair, pure previews, eligible snaps and bounded in-app floats. At source revision 90f93239d2e474e28451cab15458454ecd1649dc, the all-targets check, 22 tests and console example passed on Windows x64 MSVC.

WP-KUM-009 adds scoped identity, closed-tab retention/reopen, atomic group movement/pins, bounded canonical tab queries, equalize/swap/maximize/restore and schema1 semantic recovery/JSON/stamps. At source c387f9e4f6d353e8dedeb9ed49590a4681c73b30, the all-feature workspace/all-target check,49 tests and both examples passed on Windows x64 MSVC. Default-core50563242 proof has unchanged inputs and is reused. At the WP009 stage these visible/provider boundaries were still future work; subsequent WP010-WP014 outcomes above record the contracts, desktop/web demos and engine/graphics experiment now demonstrated. Full storage, host-module, media and platform acceptances remain open. The accepted organization is three logical submodules (core, optional GUI adapters, host-content integration), implemented through small cohesive packages with independently selected adapters; no fixed crate count is prescribed. Navigation presentation is host-selectable: scrollbar, permanent minimap, both, or a temporary minimap during scrubbing.
```

```yaml
roadmap:
- item_ref: FEAT-KUM-WORKSPACE
  source_topic_refs:
  - TOPIC-KUM-WORKSPACE
  overview_summary: Reusable window and frame workspace
  implementation_status: partially_implemented
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-WORKSPACE-001
  - REQ-KUM-WORKSPACE-002
  - REQ-KUM-WORKSPACE-003
  - REQ-KUM-WORKSPACE-004
  - REQ-KUM-WORKSPACE-005
  - REQ-KUM-WORKSPACE-006
  implementation_source_revision: 7d9268ab90d5ed6e31d7af070cb73302765741cc
  verification_summary: 'Partial: nested ownership, packing/docking, in-app floats, tabs and semantic saved-layout
    recovery proven; full workspace and host/provider integration acceptance remains open. Scoped web adapter: Six
    original browser cases and eight inspected images PASS at e6b48fd; canonical arrangement/task ownership, input
    cancellation/scale, save/reload/provider recovery and restricted popup denial. Full requirement acceptance remains
    open. Scoped desktop: Host-configurable nested frames, canonical versus mounted counts and reachable pane list.'
  matrix_entry_refs:
  - BM-KUM-LAYOUT-WINX64-001
  - BM-KUM-RECOVERY-WINX64-001
  - BM-KUM-WEB-WASM-001
  - BM-KUM-DESKTOP-WINX64-001
- item_ref: FEAT-KUM-BOUNDARIES
  source_topic_refs:
  - TOPIC-KUM-BOUNDARIES
  overview_summary: Windows, frames and host content
  implementation_status: partially_implemented
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-BOUNDARIES-001
  - REQ-KUM-BOUNDARIES-002
  - REQ-KUM-BOUNDARIES-003
  - REQ-KUM-BOUNDARIES-004
  - REQ-KUM-BOUNDARIES-005
  - REQ-KUM-BOUNDARIES-006
  - REQ-KUM-BOUNDARIES-007
  implementation_source_revision: 8cbea2746233a0987d0cf7556c59deeef744486b
  verification_summary: 'Partial: default std-only core and independent optional JSON codec with host-owned engines/IO
    proven at their headless boundary; desktop/web adapters and the render bridge are described below; full provider
    integration remains open. Scoped web adapter: Six original browser cases and eight inspected images PASS at
    e6b48fd; canonical arrangement/task ownership, input cancellation/scale, save/reload/provider recovery and restricted
    popup denial. Full requirement acceptance remains open. Scoped desktop: Borrowed Controller and host-owned providers/policy;
    optional GUI and backend boundaries. Scoped WP014: Separately selected std-only bridge and optional GPU adapter;
    host-owned engines.'
  matrix_entry_refs:
  - BM-KUM-LAYOUT-WINX64-001
  - BM-KUM-RECOVERY-WINX64-001
  - BM-KUM-WEB-WASM-001
  - BM-KUM-DESKTOP-WINX64-001
  - BM-KUM-SERVO-RENDER-WINX64-001
- item_ref: FEAT-KUM-LAYOUT
  source_topic_refs:
  - TOPIC-KUM-LAYOUT
  overview_summary: Nested windows, alignment and resizing
  implementation_status: partially_implemented
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-LAYOUT-001
  - REQ-KUM-LAYOUT-002
  - REQ-KUM-LAYOUT-003
  - REQ-KUM-LAYOUT-004
  - REQ-KUM-LAYOUT-005
  - REQ-KUM-LAYOUT-006
  - REQ-KUM-LAYOUT-007
  implementation_source_revision: 7d9268ab90d5ed6e31d7af070cb73302765741cc
  verification_summary: 'Partial: split/grid geometry, docking, floats, equalize/swap and reversible maximize/restore
    proven; desktop/browser arrangement scenarios are described below; full layout acceptance remains open. Scoped
    web adapter: Six original browser cases and eight inspected images PASS at e6b48fd; canonical arrangement/task
    ownership, input cancellation/scale, save/reload/provider recovery and restricted popup denial. Full requirement
    acceptance remains open. Scoped desktop: Weighted/holes/deep geometry; drag/cancel/commit, float/redock/equalize/swap/maximize/restore
    and persistent paint order.'
  matrix_entry_refs:
  - BM-KUM-LAYOUT-WINX64-001
  - BM-KUM-RECOVERY-WINX64-001
  - BM-KUM-WEB-WASM-001
  - BM-KUM-DESKTOP-WINX64-001
- item_ref: FEAT-KUM-TABS
  source_topic_refs:
  - TOPIC-KUM-TABS
  overview_summary: Tabs, named groups and color labels
  implementation_status: partially_implemented
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-TABS-001
  - REQ-KUM-TABS-002
  - REQ-KUM-TABS-003
  - REQ-KUM-TABS-004
  - REQ-KUM-TABS-005
  - REQ-KUM-TABS-006
  implementation_source_revision: 7d9268ab90d5ed6e31d7af070cb73302765741cc
  verification_summary: 'Partial: groups/colors/collapse/pins, canonical membership/list paging, close-veto/retention/reopen
    and atomic group movement proven; desktop paging/group controls and scoped Windows accessibility are described
    below; full overflow/accessibility acceptance remains open. Scoped web adapter: Six original browser cases and
    eight inspected images PASS at e6b48fd; canonical arrangement/task ownership, input cancellation/scale, save/reload/provider
    recovery and restricted popup denial. Full requirement acceptance remains open. Scoped desktop: Canonical paging/reorder/pins/veto/groups,
    labels/colors, and actual OS selected state.'
  matrix_entry_refs:
  - BM-KUM-CORE-WINX64-001
  - BM-KUM-RECOVERY-WINX64-001
  - BM-KUM-WEB-WASM-001
  - BM-KUM-DESKTOP-WINX64-001
- item_ref: FEAT-KUM-POPOUTS
  source_topic_refs:
  - TOPIC-KUM-POPOUTS
  overview_summary: Floating frames, native popouts and temporary popups
  implementation_status: partially_implemented
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-POPOUTS-001
  - REQ-KUM-POPOUTS-002
  - REQ-KUM-POPOUTS-003
  - REQ-KUM-POPOUTS-004
  - REQ-KUM-POPOUTS-005
  - REQ-KUM-POPOUTS-006
  - REQ-KUM-POPOUTS-007
  - REQ-KUM-POPOUTS-008
  - REQ-KUM-POPOUTS-009
  implementation_source_revision: 7d9268ab90d5ed6e31d7af070cb73302765741cc
  verification_summary: 'Partial: in-app floats with eligible return and explicit saved bounds repair proven; scoped
    Windows native handoff/return is described below; other platforms and engine-context migration remain open.
    Scoped desktop: Correlated native preflight for view/group/frame, three returns, partial survivors, removed-origin
    return and preparation failures. Newly authorized file-drop/selected external-app extensions remain planned
    and unproven; see WP018-021.'
  matrix_entry_refs:
  - BM-KUM-LAYOUT-WINX64-001
  - BM-KUM-RECOVERY-WINX64-001
  - BM-KUM-DESKTOP-WINX64-001
- item_ref: FEAT-KUM-CHROME
  source_topic_refs:
  - TOPIC-KUM-CHROME
  overview_summary: Configurable bars, borders and host settings
  implementation_status: partially_implemented
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-CHROME-001
  - REQ-KUM-CHROME-002
  - REQ-KUM-CHROME-003
  - REQ-KUM-CHROME-004
  - REQ-KUM-CHROME-005
  - REQ-KUM-CHROME-006
  - REQ-KUM-CHROME-007
  - REQ-KUM-CHROME-008
  - REQ-KUM-CHROME-009
  - REQ-KUM-CHROME-010
  implementation_source_revision: 7d9268ab90d5ed6e31d7af070cb73302765741cc
  verification_summary: 'Partial: semantic pinned target/placement, tool reservations, closed/unavailable target
    safety and projection guards proven; scoped desktop chrome/widgets and Windows accessibility are described below;
    complete chrome acceptance remains open. WP011 adds version2 layered settings/capability provenance and current
    content0.2/protocol2 typed targets; all24 public cases proved headlessly; later scoped GUI behavior is described
    below and full host acceptance remains open. Scoped desktop: Full/Compact/Contextual/Hidden profiles, settings
    precedence, keyboard/semantic touch reveal and theme-backed controls.'
  matrix_entry_refs:
  - BM-KUM-LAYOUT-WINX64-001
  - BM-KUM-RECOVERY-WINX64-001
  - BM-KUM-NAVIGATION-MEDIA-WINX64-001
  - BM-KUM-DESKTOP-WINX64-001
- item_ref: FEAT-KUM-CONTENT
  source_topic_refs:
  - TOPIC-KUM-CONTENT
  overview_summary: Hosted modules and plugin contracts
  implementation_status: partially_implemented
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-CONTENT-001
  - REQ-KUM-CONTENT-002
  - REQ-KUM-CONTENT-003
  - REQ-KUM-CONTENT-004
  - REQ-KUM-CONTENT-005
  - REQ-KUM-CONTENT-006
  - REQ-KUM-CONTENT-007
  - REQ-KUM-CONTENT-008
  - REQ-KUM-CONTENT-009
  implementation_source_revision: 8cbea2746233a0987d0cf7556c59deeef744486b
  verification_summary: 'Partial: typed capability negotiation, private Core routing/scoped ACKs, placeholders,
    pinned tools and opaque host asset/undo contracts proven; actual content engines/editors/asset store remain
    host integration. WP011 adds version2 layered settings/capability provenance and current content0.2/protocol2
    typed targets; all24 public cases proved headlessly; later scoped GUI behavior is described below and full host
    acceptance remains open. Scoped web adapter: Six original browser cases and eight inspected images PASS at e6b48fd;
    canonical arrangement/task ownership, input cancellation/scale, save/reload/provider recovery and restricted
    popup denial. Full requirement acceptance remains open. Scoped desktop: Editable-text/generated-canvas providers,
    exact dispatch, unsupported backend reporting and placeholders. Scoped WP014: Two independently sized genuine
    Servo contexts, exact accepted pixel provenance and explicit unavailable capabilities. Newly authorized file-drop/selected
    external-app extensions remain planned and unproven; see WP018-021.'
  matrix_entry_refs:
  - BM-KUM-CONTENT-WINX64-001
  - BM-KUM-NAVIGATION-MEDIA-WINX64-001
  - BM-KUM-WEB-WASM-001
  - BM-KUM-DESKTOP-WINX64-001
  - BM-KUM-SERVO-RENDER-WINX64-001
- item_ref: FEAT-KUM-RENDER
  source_topic_refs:
  - TOPIC-KUM-RENDER
  overview_summary: 2D, 3D and rendering surfaces
  implementation_status: partially_implemented
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-RENDER-001
  - REQ-KUM-RENDER-002
  - REQ-KUM-RENDER-003
  - REQ-KUM-RENDER-004
  implementation_source_revision: 8cbea2746233a0987d0cf7556c59deeef744486b
  verification_summary: 'Full requirement acceptance remains open. Scoped desktop: Actual DX12 raster/native captures
    and exact renderer/epoch binding; no universal sharing or real GPU-fault claim. Scoped WP014: Nine actual DX12
    four-provider/input/reload/recovery scenes, real 3D depth, full static pixels and stale-device rejection.'
  matrix_entry_refs:
  - BM-KUM-DESKTOP-WINX64-001
  - BM-KUM-SERVO-RENDER-WINX64-001
- item_ref: FEAT-KUM-PERFORMANCE
  source_topic_refs:
  - TOPIC-KUM-PERFORMANCE
  overview_summary: Redraw scheduling and responsiveness
  implementation_status: partially_implemented
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-PERFORMANCE-001
  - REQ-KUM-PERFORMANCE-002
  - REQ-KUM-PERFORMANCE-003
  - REQ-KUM-PERFORMANCE-004
  - REQ-KUM-PERFORMANCE-005
  implementation_source_revision: 8cbea2746233a0987d0cf7556c59deeef744486b
  verification_summary: 'Partial: causal bounded queues, independent signals, invalidation/animation deadlines,
    declared cache reservations and provenance diagnostics proven; scoped rendering and CPU/RSS observations are
    described below; broad responsiveness, savings and performance acceptance remain open. Scoped desktop: Observed
    bounded work, fresh Instant/repaint scheduling and delayed providers; no performance improvement claim. Scoped
    WP014: Bounded/coalesced affected captures and zero static duplicate uploads; actual CPU/RSS observations, no
    savings claim.'
  matrix_entry_refs:
  - BM-KUM-CONTENT-WINX64-001
  - BM-KUM-DESKTOP-WINX64-001
  - BM-KUM-SERVO-RENDER-WINX64-001
- item_ref: FEAT-KUM-TEXT
  source_topic_refs:
  - TOPIC-KUM-TEXT
  overview_summary: Text reflow and media inside documents
  implementation_status: partially_implemented
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-TEXT-001
  - REQ-KUM-TEXT-002
  - REQ-KUM-TEXT-003
  implementation_source_revision: ba2d6189704ced50c621dab18e6140e301aab136
  verification_summary: 'Partial: finite geometry/scale, declared embed input and matched opaque anchor completion
    protocol proven; actual typography/wrapping/selection/scroll/reflow and inline engines remain host integration.'
  matrix_entry_refs:
  - BM-KUM-CONTENT-WINX64-001
- item_ref: FEAT-KUM-NAVIGATION
  source_topic_refs:
  - TOPIC-KUM-NAVIGATION
  overview_summary: Large scrollbars and content minimaps
  implementation_status: partially_implemented
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-NAVIGATION-001
  - REQ-KUM-NAVIGATION-002
  - REQ-KUM-NAVIGATION-003
  - REQ-KUM-NAVIGATION-004
  - REQ-KUM-NAVIGATION-005
  - REQ-KUM-NAVIGATION-006
  implementation_source_revision: 7d9268ab90d5ed6e31d7af070cb73302765741cc
  verification_summary: 'Partial: four modes, full-range/minimum-thumb mapping, keyboard metadata, continuous transient
    gesture, bounded host overview and typed actual/Unknown recovery proved headlessly; scoped desktop navigation/capture
    is described below; full accessibility and real host-document acceptance remain open. Scoped desktop: All four
    modes, rail/content-wheel distinction, transient stationary reveal and current-epoch picks.'
  matrix_entry_refs:
  - BM-KUM-NAVIGATION-MEDIA-WINX64-001
  - BM-KUM-DESKTOP-WINX64-001
- item_ref: FEAT-KUM-BACKGROUNDS
  source_topic_refs:
  - TOPIC-KUM-BACKGROUNDS
  overview_summary: Image and video backgrounds
  implementation_status: partially_implemented
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-BACKGROUNDS-001
  - REQ-KUM-BACKGROUNDS-002
  - REQ-KUM-BACKGROUNDS-003
  - REQ-KUM-BACKGROUNDS-004
  implementation_source_revision: 7d9268ab90d5ed6e31d7af070cb73302765741cc
  verification_summary: 'Partial: real frame/workspace mounts, visual/legibility/motion/audio policy, explicit shared
    media MIN ceilings and exact applied/Unknown cleanup proved headlessly; desktop still-image composition is described
    below; video decoding/audio and physical savings remain open. Scoped desktop: Host-fed still image fit/brightness/opacity
    preserved; video remains unsupported in this adapter.'
  matrix_entry_refs:
  - BM-KUM-NAVIGATION-MEDIA-WINX64-001
  - BM-KUM-DESKTOP-WINX64-001
- item_ref: FEAT-KUM-LIFECYCLE
  source_topic_refs:
  - TOPIC-KUM-LIFECYCLE
  overview_summary: Many saved tabs and bounded live content
  implementation_status: partially_implemented
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-LIFECYCLE-001
  - REQ-KUM-LIFECYCLE-002
  - REQ-KUM-LIFECYCLE-003
  - REQ-KUM-LIFECYCLE-004
  - REQ-KUM-LIFECYCLE-005
  - REQ-KUM-LIFECYCLE-006
  implementation_source_revision: 8cbea2746233a0987d0cf7556c59deeef744486b
  verification_summary: 'Partial: requested/confirmed/Unknown, separate mount/instance reservations, protected shared
    cleanup, saturated recovery, queued deadlines and confirmed-instance retention proven; scoped external-engine
    unload/page reload is described below; general physical release and restart recovery remain host integration.
    Scoped WP014: Keep-alive/dirty protection, actual independently confirmed unload, logical placeholder and fresh
    page reload.'
  matrix_entry_refs:
  - BM-KUM-CONTENT-WINX64-001
  - BM-KUM-SERVO-RENDER-WINX64-001
- item_ref: FEAT-KUM-SERVO
  source_topic_refs:
  - TOPIC-KUM-SERVO
  overview_summary: Future Handshake browser and Servo adapter
  implementation_status: partially_implemented
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-SERVO-001
  - REQ-KUM-SERVO-002
  - REQ-KUM-SERVO-003
  - REQ-KUM-SERVO-004
  - REQ-KUM-SERVO-005
  implementation_source_revision: 8cbea2746233a0987d0cf7556c59deeef744486b
  verification_summary: 'Full requirement acceptance remains open. Scoped WP014: Pinned external Linux software
    processes with independent kernel admission before each launch and listener; native embedded browser remains
    future work.'
  matrix_entry_refs:
  - BM-KUM-SERVO-RENDER-WINX64-001
- item_ref: FEAT-KUM-RECOVERY
  source_topic_refs:
  - TOPIC-KUM-RECOVERY
  overview_summary: Saving layouts and recovering interrupted work
  implementation_status: partially_implemented
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-RECOVERY-001
  - REQ-KUM-RECOVERY-002
  - REQ-KUM-RECOVERY-003
  - REQ-KUM-RECOVERY-004
  - REQ-KUM-RECOVERY-005
  implementation_source_revision: 8cbea2746233a0987d0cf7556c59deeef744486b
  verification_summary: 'Partial: bounded schema1 wire, semantic live/fresh restore, identity/admission/placeholder
    safety and save stamps proven; scoped web storage and desktop recreation are described below; filesystem interruption/native
    monitor/full content recovery remain open. Scoped web adapter: Six original browser cases and eight inspected
    images PASS at e6b48fd; canonical arrangement/task ownership, input cancellation/scale, save/reload/provider
    recovery and restricted popup denial. Full requirement acceptance remains open. Scoped desktop: Actual teardown/recreation,
    labelled injected zero/suspend and bounded failures, visible notice/recovery and session negatives. Scoped WP014:
    Real renderer replacement and accepted CPU-pixel reupload; page reload strength only, arbitrary page state remains
    open.'
  matrix_entry_refs:
  - BM-KUM-RECOVERY-WINX64-001
  - BM-KUM-WEB-WASM-001
  - BM-KUM-DESKTOP-WINX64-001
  - BM-KUM-SERVO-RENDER-WINX64-001
- item_ref: FEAT-KUM-INPUT
  source_topic_refs:
  - TOPIC-KUM-INPUT
  overview_summary: Tablet input, keyboard and accessibility
  implementation_status: partially_implemented
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-INPUT-001
  - REQ-KUM-INPUT-002
  - REQ-KUM-INPUT-003
  - REQ-KUM-INPUT-004
  implementation_source_revision: 8cbea2746233a0987d0cf7556c59deeef744486b
  verification_summary: 'Partial: typed mouse/touch/pen gesture identity/settlement, keyboard navigation, accessibility
    range metadata and denied background input routes proved headlessly; scoped browser/native actions are described
    below; physical input and full assistive-technology acceptance remain open. Scoped web adapter: Six original
    browser cases and eight inspected images PASS at e6b48fd; canonical arrangement/task ownership, input cancellation/scale,
    save/reload/provider recovery and restricted popup denial. Full requirement acceptance remains open. Scoped
    desktop: Actual mouse/keyboard and Windows UIA selection/menu actions; semantic touch/host pen remain separate
    from physical proof. Scoped WP014: Genuine WebDriver pointer/keyboard at normal/1.5 and canonical per-provider
    2D/3D scene picks; physical input remains open.'
  matrix_entry_refs:
  - BM-KUM-NAVIGATION-MEDIA-WINX64-001
  - BM-KUM-WEB-WASM-001
  - BM-KUM-DESKTOP-WINX64-001
  - BM-KUM-SERVO-RENDER-WINX64-001
- item_ref: FEAT-KUM-MODEL
  source_topic_refs:
  - TOPIC-KUM-MODEL
  overview_summary: Model operation, diagnostics and the built-in manual
  implementation_status: partially_implemented
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-MODEL-001
  - REQ-KUM-MODEL-002
  - REQ-KUM-MODEL-003
  - REQ-KUM-MODEL-004
  - REQ-KUM-MODEL-005
  implementation_source_revision: 8cbea2746233a0987d0cf7556c59deeef744486b
  verification_summary: 'Partial: scoped attributable commands/replay, pure preview/restore, canonical paging/diagnostics/manual
    proven; complete model and adapter operation surfaces remain open. Scoped web adapter: Six original browser
    cases and eight inspected images PASS at e6b48fd; canonical arrangement/task ownership, input cancellation/scale,
    save/reload/provider recovery and restricted popup denial. Full requirement acceptance remains open. Scoped
    desktop: Stable bounded redacted controls, current identity/causal receipts, host ownership and product manual.
    Scoped WP014: Versioned bounded stdio, configured launcher, independent admission/exit facts, causal diagnostics
    and product manual.'
  matrix_entry_refs:
  - BM-KUM-LAYOUT-WINX64-001
  - BM-KUM-RECOVERY-WINX64-001
  - BM-KUM-WEB-WASM-001
  - BM-KUM-DESKTOP-WINX64-001
  - BM-KUM-SERVO-RENDER-WINX64-001
- item_ref: FEAT-KUM-PLATFORMS
  source_topic_refs:
  - TOPIC-KUM-PLATFORMS
  overview_summary: Desktop, web and ARM development
  implementation_status: partially_implemented
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-PLATFORMS-001
  - REQ-KUM-PLATFORMS-002
  - REQ-KUM-PLATFORMS-003
  - REQ-KUM-PLATFORMS-004
  implementation_source_revision: 8cbea2746233a0987d0cf7556c59deeef744486b
  verification_summary: 'Full requirement acceptance remains open. Scoped web adapter: Six original browser cases
    and eight inspected images PASS at e6b48fd; canonical arrangement/task ownership, input cancellation/scale,
    save/reload/provider recovery and restricted popup denial. Full requirement acceptance remains open. Scoped
    desktop: Scoped Windows x64 MSVC1.91.1/DX12 proof; ARM64 and other desktop platforms remain unproven. Scoped
    WP014: Actual Linux x64 Servo under WSL plus Windows x64 DX12; ARM/native Windows Servo/other platforms remain
    unproven.'
  matrix_entry_refs:
  - BM-KUM-WEB-WASM-001
  - BM-KUM-DESKTOP-WINX64-001
  - BM-KUM-SERVO-RENDER-WINX64-001
```
