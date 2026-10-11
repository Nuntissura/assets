---
schema_version: handshake.spec-document.v1
specification_id: SPEC-KUMIHIMO-v1
document_id: DOC-KUM-ROADMAP-v1
revision: 10
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
# Planned

Eight feature areas now have partial core implementation. The remaining areas stay planned, and all requirements remain open until their full acceptance surfaces are proven. Completed repository preparation is separate from product implementation.

WP009 proves the headless tab/group/pin/list, bounded schema1 JSON/semantic recovery and carried layout-operation scope. Core0.3 remains default std-only with optional saved-type Serde and an independent JSON package. The host storage protocol is documented; actual filesystem interruption/replacement and complete RECOVERY acceptance remain open.

A future prototype can exercise a host-configured recursive workspace with varying window/tab counts and simple host-owned content: tabs/groups/colors, resizing, docking, exposed settings and save/restore. It should demonstrate an actual usable surface, then prove native popout handoff and the web adapter at their own boundaries. Keep the full roadmap intact as these areas are implemented.

Browser resource management, media backgrounds and 2D/3D integrations follow their declared provider contracts. Servo integration starts with two independently resized views and measured lifecycle behavior before large saved-tab or site-compatibility claims.

Verification stays focused on the behavior a work packet changes. A real rendered interaction is required when claiming GUI behavior; broad test suites are not a substitute for that evidence.

## Implemented

WP-KUM-007 implements the first in-memory ownership core. The Windows x64 MSVC all-target compile check, nine public consumer tests, one private counter-exhaustion test and console example passed at source revision a004dbd4c655d6d193e288d21e37dc9987095353. Functional scenarios include variable 20-by-30 counts and 1,024 nested frame levels; they are not performance benchmarks. No complete future feature area is marked implemented.

WP-KUM-008 adds finite shared split/grid geometry, minima-first weighted allocation, explicit virtual-extent overflow, pinned tool placement/reservations, atomic configured-layout repair, pure previews, eligible snaps and bounded in-app floats. At source revision 90f93239d2e474e28451cab15458454ecd1649dc, the all-targets check, 22 tests and console example passed on Windows x64 MSVC.

WP-KUM-009 adds scoped identity, closed-tab retention/reopen, atomic group movement/pins, bounded canonical tab queries, equalize/swap/maximize/restore and schema1 semantic recovery/JSON/stamps. At source c387f9e4f6d353e8dedeb9ed49590a4681c73b30, the all-feature workspace/all-target check,49 tests and both examples passed on Windows x64 MSVC. Default-core50563242 proof has unchanged inputs and is reused. Visible controls, host storage, GUI adapters, provider lifecycle and browser/renderer integrations remain subsequent work. The accepted organization is three logical submodules (core, optional GUI adapters, host-content integration), implemented later through small cohesive packages with independently selected adapters; no fixed crate count is prescribed. Navigation presentation is host-selectable: scrollbar, permanent minimap, both, or a temporary minimap during scrubbing.
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
  implementation_source_revision: c387f9e4f6d353e8dedeb9ed49590a4681c73b30
  verification_summary: 'Partial: nested ownership, packing/docking, in-app floats, tabs and semantic saved-layout
    recovery proven; visible controls/native windows/lifecycle/adapters remain open.'
  matrix_entry_refs:
  - BM-KUM-LAYOUT-WINX64-001
  - BM-KUM-RECOVERY-WINX64-001
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
  implementation_source_revision: c387f9e4f6d353e8dedeb9ed49590a4681c73b30
  verification_summary: 'Partial: default std-only core and independent optional JSON codec with host-owned engines/IO
    proven at their headless boundary; concrete adapters/provider integration remain open.'
  matrix_entry_refs:
  - BM-KUM-LAYOUT-WINX64-001
  - BM-KUM-RECOVERY-WINX64-001
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
  implementation_source_revision: c387f9e4f6d353e8dedeb9ed49590a4681c73b30
  verification_summary: 'Partial: split/grid geometry, docking, floats, equalize/swap and reversible maximize/restore
    proven; actual GUI interaction/adapter constraints remain open.'
  matrix_entry_refs:
  - BM-KUM-LAYOUT-WINX64-001
  - BM-KUM-RECOVERY-WINX64-001
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
  implementation_source_revision: c387f9e4f6d353e8dedeb9ed49590a4681c73b30
  verification_summary: 'Partial: groups/colors/collapse/pins, canonical membership/list paging, close-veto/retention/reopen
    and atomic group movement proven; rendered overflow controls and input accessibility remain open.'
  matrix_entry_refs:
  - BM-KUM-CORE-WINX64-001
  - BM-KUM-RECOVERY-WINX64-001
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
  implementation_source_revision: c387f9e4f6d353e8dedeb9ed49590a4681c73b30
  verification_summary: 'Partial: in-app floats with eligible return and explicit saved bounds repair proven; native
    popout handoff/close effects/platform proof remain open.'
  matrix_entry_refs:
  - BM-KUM-LAYOUT-WINX64-001
  - BM-KUM-RECOVERY-WINX64-001
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
  implementation_source_revision: c387f9e4f6d353e8dedeb9ed49590a4681c73b30
  verification_summary: 'Partial: semantic pinned target/placement, tool reservations, closed/unavailable target
    safety and projection guards proven; actual configurable chrome/widgets/accessibility remain open.'
  matrix_entry_refs:
  - BM-KUM-LAYOUT-WINX64-001
  - BM-KUM-RECOVERY-WINX64-001
- item_ref: FEAT-KUM-CONTENT
  source_topic_refs:
  - TOPIC-KUM-CONTENT
  overview_summary: Hosted modules and plugin contracts
  implementation_status: planned
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-CONTENT-001
  - REQ-KUM-CONTENT-002
  - REQ-KUM-CONTENT-003
  - REQ-KUM-CONTENT-004
  - REQ-KUM-CONTENT-005
  - REQ-KUM-CONTENT-006
  - REQ-KUM-CONTENT-007
  implementation_source_revision: null
  verification_summary: No Rust implementation, GUI adapter or runtime acceptance yet.
  matrix_entry_refs: []
- item_ref: FEAT-KUM-RENDER
  source_topic_refs:
  - TOPIC-KUM-RENDER
  overview_summary: 2D, 3D and rendering surfaces
  implementation_status: planned
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-RENDER-001
  - REQ-KUM-RENDER-002
  - REQ-KUM-RENDER-003
  - REQ-KUM-RENDER-004
  implementation_source_revision: null
  verification_summary: No Rust implementation, GUI adapter or runtime acceptance yet.
  matrix_entry_refs: []
- item_ref: FEAT-KUM-PERFORMANCE
  source_topic_refs:
  - TOPIC-KUM-PERFORMANCE
  overview_summary: Redraw scheduling and responsiveness
  implementation_status: planned
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-PERFORMANCE-001
  - REQ-KUM-PERFORMANCE-002
  - REQ-KUM-PERFORMANCE-003
  - REQ-KUM-PERFORMANCE-004
  - REQ-KUM-PERFORMANCE-005
  implementation_source_revision: null
  verification_summary: No Rust implementation, GUI adapter or runtime acceptance yet.
  matrix_entry_refs: []
- item_ref: FEAT-KUM-TEXT
  source_topic_refs:
  - TOPIC-KUM-TEXT
  overview_summary: Text reflow and media inside documents
  implementation_status: planned
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-TEXT-001
  - REQ-KUM-TEXT-002
  - REQ-KUM-TEXT-003
  implementation_source_revision: null
  verification_summary: No Rust implementation, GUI adapter or runtime acceptance yet.
  matrix_entry_refs: []
- item_ref: FEAT-KUM-NAVIGATION
  source_topic_refs:
  - TOPIC-KUM-NAVIGATION
  overview_summary: Large scrollbars and content minimaps
  implementation_status: planned
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-NAVIGATION-001
  - REQ-KUM-NAVIGATION-002
  - REQ-KUM-NAVIGATION-003
  - REQ-KUM-NAVIGATION-004
  - REQ-KUM-NAVIGATION-005
  - REQ-KUM-NAVIGATION-006
  implementation_source_revision: null
  verification_summary: No Rust implementation, GUI adapter or runtime acceptance yet.
  matrix_entry_refs: []
- item_ref: FEAT-KUM-BACKGROUNDS
  source_topic_refs:
  - TOPIC-KUM-BACKGROUNDS
  overview_summary: Image and video backgrounds
  implementation_status: planned
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-BACKGROUNDS-001
  - REQ-KUM-BACKGROUNDS-002
  - REQ-KUM-BACKGROUNDS-003
  - REQ-KUM-BACKGROUNDS-004
  implementation_source_revision: null
  verification_summary: No Rust implementation, GUI adapter or runtime acceptance yet.
  matrix_entry_refs: []
- item_ref: FEAT-KUM-LIFECYCLE
  source_topic_refs:
  - TOPIC-KUM-LIFECYCLE
  overview_summary: Many saved tabs and bounded live content
  implementation_status: planned
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-LIFECYCLE-001
  - REQ-KUM-LIFECYCLE-002
  - REQ-KUM-LIFECYCLE-003
  - REQ-KUM-LIFECYCLE-004
  - REQ-KUM-LIFECYCLE-005
  - REQ-KUM-LIFECYCLE-006
  implementation_source_revision: null
  verification_summary: No Rust implementation, GUI adapter or runtime acceptance yet.
  matrix_entry_refs: []
- item_ref: FEAT-KUM-SERVO
  source_topic_refs:
  - TOPIC-KUM-SERVO
  overview_summary: Future Handshake browser and Servo adapter
  implementation_status: planned
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-SERVO-001
  - REQ-KUM-SERVO-002
  - REQ-KUM-SERVO-003
  - REQ-KUM-SERVO-004
  - REQ-KUM-SERVO-005
  implementation_source_revision: null
  verification_summary: No Rust implementation, GUI adapter or runtime acceptance yet.
  matrix_entry_refs: []
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
  implementation_source_revision: c387f9e4f6d353e8dedeb9ed49590a4681c73b30
  verification_summary: 'Partial: bounded schema1 wire, semantic live/fresh restore, identity/admission/placeholder
    safety and save stamps proven; host atomic storage/interruption/native monitor/content recovery remain open.'
  matrix_entry_refs:
  - BM-KUM-RECOVERY-WINX64-001
- item_ref: FEAT-KUM-INPUT
  source_topic_refs:
  - TOPIC-KUM-INPUT
  overview_summary: Tablet input, keyboard and accessibility
  implementation_status: planned
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-INPUT-001
  - REQ-KUM-INPUT-002
  - REQ-KUM-INPUT-003
  - REQ-KUM-INPUT-004
  implementation_source_revision: null
  verification_summary: No Rust implementation, GUI adapter or runtime acceptance yet.
  matrix_entry_refs: []
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
  implementation_source_revision: c387f9e4f6d353e8dedeb9ed49590a4681c73b30
  verification_summary: 'Partial: scoped attributable commands/replay, pure preview/restore, canonical paging/diagnostics/manual
    proven; complete model and adapter operation surfaces remain open.'
  matrix_entry_refs:
  - BM-KUM-LAYOUT-WINX64-001
  - BM-KUM-RECOVERY-WINX64-001
- item_ref: FEAT-KUM-PLATFORMS
  source_topic_refs:
  - TOPIC-KUM-PLATFORMS
  overview_summary: Desktop, web and ARM development
  implementation_status: planned
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-PLATFORMS-001
  - REQ-KUM-PLATFORMS-002
  - REQ-KUM-PLATFORMS-003
  - REQ-KUM-PLATFORMS-004
  implementation_source_revision: null
  verification_summary: No Rust implementation, GUI adapter or runtime acceptance yet.
  matrix_entry_refs: []
```
