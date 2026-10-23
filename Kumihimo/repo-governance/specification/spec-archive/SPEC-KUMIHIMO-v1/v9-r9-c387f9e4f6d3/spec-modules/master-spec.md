---
schema_version: handshake.spec-document.v1
specification_id: SPEC-KUMIHIMO-v1
document_id: DOC-KUM-MASTER-v1
revision: 9
title: Kumihimo master specification
updated_at: '2026-10-12'
---

```yaml
kind: specification_topic
topic_id: TOPIC-KUM-WORKSPACE
title: Kumihimo baseline
content_blocks:
- block_id: explanation
  kind: prose
  language: markdown
  requirement_refs: &id001
  - REQ-KUM-WORKSPACE-001
  - REQ-KUM-WORKSPACE-002
  - REQ-KUM-WORKSPACE-003
  - REQ-KUM-WORKSPACE-004
  - REQ-KUM-WORKSPACE-005
  - REQ-KUM-WORKSPACE-006
- block_id: contract
  kind: structured_contract
  language: yaml
  requirement_refs: *id001
```

```markdown
# Kumihimo

Kumihimo holds and arranges content supplied by a host app: editors, browsers, media, tools and graphics. It targets reusable Rust logic with separate desktop and web presentation adapters.

The baseline reflects the clarified intent: recursively nested windows and tabs without a fixed count. The following topics expand that behavior. The portable ownership and geometry/tool-docking core has passed focused Windows x64 compilation, 22 tests and a console example. That proof covers in-memory containment, views/groups, split/grid geometry, pinned tool reservations, docking previews, bounded in-app floats, validated moves, host admission and bounded operation replay; the complete feature areas remain partial or planned. It does not prove a working GUI, rendering speed or browser compatibility.

Read the explanation at the start of each topic for the plain-language intent. The structured block records the corresponding requirements and focused acceptance scenarios. Optional field-inspired ideas live in the research note and are not required features.

Intended consumers include Handshake Notes, Studio and the planned Atelier suite, plus handshake-portal ingestion and movable taskboard/detail frames. These are operator-provided use cases, not implemented integrations. Modules can present views alongside or within other modules through host-declared capabilities. The host coordinates shared assets, editing actions and domain meaning; Kumihimo supplies arrangement and view boundaries.
```

```yaml
requirements:
- requirement_id: REQ-KUM-WORKSPACE-001
  anchor_refs:
  - TOPIC-KUM-WORKSPACE
  statement: Kumihimo is a reusable Rust window/frame asset for desktop apps and web hosts, with host-exposed configuration
    and separately verified adapters.
  rationale: Preserve the operator-requested reusable workspace behavior.
  acceptance_refs:
  - ACC-KUM-WORKSPACE-001
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-WORKSPACE
  primitive_refs:
  - PRIM-KUM-WORKSPACE
- requirement_id: REQ-KUM-WORKSPACE-002
  anchor_refs:
  - TOPIC-KUM-WORKSPACE
  statement: Support recursively nested windows/frames and variable tab counts with no fixed architectural window,
    nesting or tab-count ceiling, grouped tabs, color labels, resizable arrangements, docking, floating frames and
    capability-checked native popouts.
  rationale: Preserve the operator-requested reusable workspace behavior.
  acceptance_refs:
  - ACC-KUM-WORKSPACE-002
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-WORKSPACE
  primitive_refs:
  - PRIM-KUM-WORKSPACE
- requirement_id: REQ-KUM-WORKSPACE-003
  anchor_refs:
  - TOPIC-KUM-WORKSPACE
  statement: Treat native windows, inner frames and hosted content as separate ownership boundaries; keep surrounding
    bars configurable.
  rationale: Preserve the operator-requested reusable workspace behavior.
  acceptance_refs:
  - ACC-KUM-WORKSPACE-003
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-WORKSPACE
  primitive_refs:
  - PRIM-KUM-WORKSPACE
- requirement_id: REQ-KUM-WORKSPACE-004
  anchor_refs:
  - TOPIC-KUM-WORKSPACE
  statement: Expose lifecycle and visibility to host renderers and resource managers without claiming that hiding
    alone suspends or unloads content.
  rationale: Preserve the operator-requested reusable workspace behavior.
  acceptance_refs:
  - ACC-KUM-WORKSPACE-004
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-WORKSPACE
  primitive_refs:
  - PRIM-KUM-WORKSPACE
- requirement_id: REQ-KUM-WORKSPACE-005
  anchor_refs:
  - TOPIC-KUM-WORKSPACE
  statement: Preserve the requested exploration of 2D/3D, text reflow, embedded content, alignment, snapping, minimaps,
    configurable large scrollbar thumbs, media backgrounds and future Servo integration.
  rationale: Preserve the operator-requested reusable workspace behavior.
  acceptance_refs:
  - ACC-KUM-WORKSPACE-005
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-WORKSPACE
  primitive_refs:
  - PRIM-KUM-WORKSPACE
- requirement_id: REQ-KUM-WORKSPACE-006
  anchor_refs:
  - TOPIC-KUM-WORKSPACE
  statement: Target x64 desktop consumption and record Windows ARM development requirements separately; web support
    requires its own proven adapter.
  rationale: Preserve the operator-requested reusable workspace behavior.
  acceptance_refs:
  - ACC-KUM-WORKSPACE-006
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-WORKSPACE
  primitive_refs:
  - PRIM-KUM-WORKSPACE
acceptance:
- acceptance_id: ACC-KUM-WORKSPACE-001
  requirement_refs:
  - REQ-KUM-WORKSPACE-001
  method: Focused feature scenario when implemented; specification structure only during initialization.
  expected_result: Kumihimo is a reusable Rust window/frame asset for desktop apps and web hosts, with host-exposed
    configuration and separately verified adapters.
- acceptance_id: ACC-KUM-WORKSPACE-002
  requirement_refs:
  - REQ-KUM-WORKSPACE-002
  method: Inspect the future composition model and create nested layouts with differing child-window and tab counts.
  expected_result: Support recursively nested windows/frames and variable tab counts with no fixed architectural
    window, nesting or tab-count ceiling, grouped tabs, color labels, resizable arrangements, docking, floating
    frames and capability-checked native popouts.
- acceptance_id: ACC-KUM-WORKSPACE-003
  requirement_refs:
  - REQ-KUM-WORKSPACE-003
  method: Focused feature scenario when implemented; specification structure only during initialization.
  expected_result: Treat native windows, inner frames and hosted content as separate ownership boundaries; keep
    surrounding bars configurable.
- acceptance_id: ACC-KUM-WORKSPACE-004
  requirement_refs:
  - REQ-KUM-WORKSPACE-004
  method: Focused feature scenario when implemented; specification structure only during initialization.
  expected_result: Expose lifecycle and visibility to host renderers and resource managers without claiming that
    hiding alone suspends or unloads content.
- acceptance_id: ACC-KUM-WORKSPACE-005
  requirement_refs:
  - REQ-KUM-WORKSPACE-005
  method: Focused feature scenario when implemented; specification structure only during initialization.
  expected_result: Preserve the requested exploration of 2D/3D, text reflow, embedded content, alignment, snapping,
    minimaps, configurable large scrollbar thumbs, media backgrounds and future Servo integration.
- acceptance_id: ACC-KUM-WORKSPACE-006
  requirement_refs:
  - REQ-KUM-WORKSPACE-006
  method: Focused feature scenario when implemented; specification structure only during initialization.
  expected_result: Target x64 desktop consumption and record Windows ARM development requirements separately; web
    support requires its own proven adapter.
features:
- feature_id: FEAT-KUM-WORKSPACE
  title: Reusable window and frame workspace
  requirement_refs:
  - REQ-KUM-WORKSPACE-001
  - REQ-KUM-WORKSPACE-002
  - REQ-KUM-WORKSPACE-003
  - REQ-KUM-WORKSPACE-004
  - REQ-KUM-WORKSPACE-005
  - REQ-KUM-WORKSPACE-006
  primitive_refs:
  - PRIM-KUM-WORKSPACE
primitives:
- primitive_id: PRIM-KUM-WORKSPACE
  title: Reusable window and frame workspace
  contract: Host-independent identity, configuration and lifecycle contract for reusable window and frame workspace
primitive_feature_relations:
- relation_id: REL-KUM-WORKSPACE
  primitive_ref: PRIM-KUM-WORKSPACE
  feature_ref: FEAT-KUM-WORKSPACE
  relationship: implements
  constraint_refs: []
product:
- name: Kumihimo
  purpose: Arrange diverse hosted content in reusable, configurable windows and frames.
  operator_outcomes:
  - Reusable workspace behavior across desktop and web applications
  - Recursively nested tabbed windows with host-chosen counts and flexible content
  supported_users_and_actors:
  - Host developers
  - Operators using keyboard, pointer, touch and pen
  - Models operating structured surfaces
  supported_platforms:
  - Windows x64 desktop target
  - Windows ARM development host
  - Web adapter target; not yet implemented
gui_design:
- design_id: GUI-KUM-WORKSPACE
  applicability: planned
  visual_design_refs: []
  interaction_contract_refs:
  - PRIM-KUM-WORKSPACE
  theme_and_component_refs: []
  surface_profile_refs: []
  acceptance_refs:
  - ACC-KUM-WORKSPACE-001
  - ACC-KUM-WORKSPACE-002
  - ACC-KUM-WORKSPACE-003
  - ACC-KUM-WORKSPACE-004
  - ACC-KUM-WORKSPACE-005
  - ACC-KUM-WORKSPACE-006
```

```yaml
kind: specification_topic
topic_id: TOPIC-KUM-BOUNDARIES
title: Windows, frames and host content
content_blocks:
- block_id: explanation
  kind: prose
  language: markdown
  requirement_refs: &id001
  - REQ-KUM-BOUNDARIES-001
  - REQ-KUM-BOUNDARIES-002
  - REQ-KUM-BOUNDARIES-003
  - REQ-KUM-BOUNDARIES-004
  - REQ-KUM-BOUNDARIES-005
  - REQ-KUM-BOUNDARIES-006
  - REQ-KUM-BOUNDARIES-007
- block_id: contract
  kind: structured_contract
  language: yaml
  requirement_refs: *id001
```

```markdown
## Who owns what

The native window is the box your operating system moves, minimizes and places on a monitor. An inner frame is a pane, tab group or floating panel inside that box. The host supplies the content inside the frame.

Kumihimo owns arrangement, frame controls and the contract with content. Its platform adapter connects to operating-system window behavior. Editors own text layout; browsers own pages; graphics engines own scenes. Nesting a frame does not create process isolation or make a plugin trusted.

## Three logical submodules

The core owns recursive window/tab state, arrangement calculations, commands and shared identities. It can operate without displaying a GUI. Optional GUI adapters present the controls and connect platform input, focus and accessibility; native-window effects belong behind their platform boundary. Host-content integration supplies contracts and adapters for content size, rendering handoff, input, identity and lifecycle. The host retains actual editors, browser/media engines and 2D/3D scenes.

A host can use supplied GUI controls, replace selected controls, or supply its own presentation while retaining core behavior. Shared interaction behavior remains reusable; appearance and toolkit dependencies stay replaceable. Content integration does not turn Kumihimo into an editor, browser or universal renderer.

These are three logical submodules, not a requirement for exactly three crates. Keep code in small, cohesive modules and packages. Bundle state, geometry and commands when they form one independently usable core; split a backend or integration when it has different dependencies, platform support or lifecycle. Avoid one large crate that pulls every adapter into every app, and avoid a separate crate for each tiny helper.

The portable core and shared contracts do not expose a chosen toolkit's widgets, native handles, GPU objects or Servo types as mandatory public inputs. Concrete adapters translate at the boundary. Optional adapters remain independently selectable so an app only brings in the integrations it uses.
```

```yaml
requirements:
- requirement_id: REQ-KUM-BOUNDARIES-001
  anchor_refs:
  - TOPIC-KUM-BOUNDARIES
  statement: Separate native-window management, inner-frame arrangement and host-content ownership behind explicit
    adapters; the Rust core shall not require Servo or a particular renderer.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-BOUNDARIES-001
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-BOUNDARIES
  primitive_refs:
  - PRIM-KUM-BOUNDARIES
- requirement_id: REQ-KUM-BOUNDARIES-002
  anchor_refs:
  - TOPIC-KUM-BOUNDARIES
  statement: Give windows, panes, tab groups, content resources and view instances distinct stable identities; moving
    a view preserves its content resource identity.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-BOUNDARIES-002
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-BOUNDARIES
  primitive_refs:
  - PRIM-KUM-BOUNDARIES
- requirement_id: REQ-KUM-BOUNDARIES-003
  anchor_refs:
  - TOPIC-KUM-BOUNDARIES
  statement: A second view of one content resource requires host-declared cloning or shared-view support; otherwise
    reject duplication with a reason.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-BOUNDARIES-003
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-BOUNDARIES
  primitive_refs:
  - PRIM-KUM-BOUNDARIES
- requirement_id: REQ-KUM-BOUNDARIES-004
  anchor_refs:
  - TOPIC-KUM-BOUNDARIES
  statement: Route close, serialize, restore, focus, resize and lifecycle effects through the owning host or platform
    adapter; renderers shall not reach into sibling content internals.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-BOUNDARIES-004
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-BOUNDARIES
  primitive_refs:
  - PRIM-KUM-BOUNDARIES
- requirement_id: REQ-KUM-BOUNDARIES-005
  anchor_refs:
  - TOPIC-KUM-BOUNDARIES
  statement: 'Organize Kumihimo into three logical submodules: portable core behavior, optional GUI adapters and
    host-content integration contracts/adapters; keep actual editor, browser, media and scene engines host-owned,
    with native-window effects behind a platform adapter.'
  rationale: Make the accepted three-part boundary explicit without prescribing a fixed crate count or embedding
    content engines.
  acceptance_refs:
  - ACC-KUM-BOUNDARIES-005
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-BOUNDARIES
  primitive_refs:
  - PRIM-KUM-BOUNDARIES
- requirement_id: REQ-KUM-BOUNDARIES-006
  anchor_refs:
  - TOPIC-KUM-BOUNDARIES
  statement: Keep the core usable without GUI/GPU/browser dependencies or mandatory toolkit/native/GPU/Servo types
    in its public contracts; offer optional reusable GUI controls that hosts may theme, replace individually or
    replace with their own presentation while preserving shared commands and state.
  rationale: Reuse stock interaction behavior while allowing different host appearance and rendering stacks.
  acceptance_refs:
  - ACC-KUM-BOUNDARIES-006
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-BOUNDARIES
  primitive_refs:
  - PRIM-KUM-BOUNDARIES
- requirement_id: REQ-KUM-BOUNDARIES-007
  anchor_refs:
  - TOPIC-KUM-BOUNDARIES
  statement: Keep Rust code in small cohesive modules and independently selectable packages; bundle only responsibilities
    that share a usable purpose and compatible dependencies, split substantial backend/platform integrations, and
    avoid a large all-purpose crate or compulsory adapter bundle; logical submodule count shall not fix physical
    crate count.
  rationale: Support modular reuse and bounded maintenance without introducing helper-crate fragmentation.
  acceptance_refs:
  - ACC-KUM-BOUNDARIES-007
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-BOUNDARIES
  primitive_refs:
  - PRIM-KUM-BOUNDARIES
acceptance:
- acceptance_id: ACC-KUM-BOUNDARIES-001
  requirement_refs:
  - REQ-KUM-BOUNDARIES-001
  method: Attach two unrelated host content providers and inspect dependency direction.
  expected_result: Both use the same arrangement model without importing either content engine into the core.
- acceptance_id: ACC-KUM-BOUNDARIES-002
  requirement_refs:
  - REQ-KUM-BOUNDARIES-002
  method: Move one view between panes and a supported popout; inspect identities.
  expected_result: Presentation location changes while the original content resource remains identifiable.
- acceptance_id: ACC-KUM-BOUNDARIES-003
  requirement_refs:
  - REQ-KUM-BOUNDARIES-003
  method: Request duplicate views from capable and incapable providers.
  expected_result: Supported duplication has separate view IDs; unsupported duplication neither copies nor loses
    the original.
- acceptance_id: ACC-KUM-BOUNDARIES-004
  requirement_refs:
  - REQ-KUM-BOUNDARIES-004
  method: Use a provider that vetoes close and records callbacks.
  expected_result: The veto is preserved, events target the correct owner and unrelated views remain untouched.
- acceptance_id: ACC-KUM-BOUNDARIES-005
  requirement_refs:
  - REQ-KUM-BOUNDARIES-005
  method: Inspect future package dependency direction and attach unrelated content providers through supplied and
    host-owned presentation paths.
  expected_result: Each responsibility has an explicit owner; adapters translate external effects and engine-specific
    interfaces without creating reverse core dependencies.
- acceptance_id: ACC-KUM-BOUNDARIES-006
  requirement_refs:
  - REQ-KUM-BOUNDARIES-006
  method: Inspect the future minimal core dependency graph and API, then exercise the same layout commands through
    a supplied control and a host-supplied control.
  expected_result: Core state and commands work independently of presentation; replacing controls preserves shared
    behavior and does not force unrelated engines into the core.
- acceptance_id: ACC-KUM-BOUNDARIES-007
  requirement_refs:
  - REQ-KUM-BOUNDARIES-007
  method: Inspect the future package/module graph and declared responsibilities; compare a minimal consumer with
    consumers selecting individual implemented adapters.
  expected_result: Each package has a cohesive responsibility and dependency rationale; consumers select needed
    integrations without an all-adapters dependency path, and internal source organization stays modular.
features:
- feature_id: FEAT-KUM-BOUNDARIES
  title: Windows, frames and host content
  requirement_refs:
  - REQ-KUM-BOUNDARIES-001
  - REQ-KUM-BOUNDARIES-002
  - REQ-KUM-BOUNDARIES-003
  - REQ-KUM-BOUNDARIES-004
  - REQ-KUM-BOUNDARIES-005
  - REQ-KUM-BOUNDARIES-006
  - REQ-KUM-BOUNDARIES-007
  primitive_refs:
  - PRIM-KUM-BOUNDARIES
primitives:
- primitive_id: PRIM-KUM-BOUNDARIES
  title: Windows, frames and host content
  contract: Stable arrangement identities and host-owned content references; platform and content adapters own external
    effects. The core is presentation-independent; optional GUI and host-content adapters remain separately selectable.
primitive_feature_relations:
- relation_id: REL-KUM-BOUNDARIES
  primitive_ref: PRIM-KUM-BOUNDARIES
  feature_ref: FEAT-KUM-BOUNDARIES
  relationship: implements
  constraint_refs: []
gui_design:
- design_id: GUI-KUM-BOUNDARIES
  applicability: planned
  visual_design_refs: []
  interaction_contract_refs:
  - PRIM-KUM-BOUNDARIES
  theme_and_component_refs: []
  surface_profile_refs: []
  acceptance_refs:
  - ACC-KUM-BOUNDARIES-001
  - ACC-KUM-BOUNDARIES-002
  - ACC-KUM-BOUNDARIES-003
  - ACC-KUM-BOUNDARIES-004
  - ACC-KUM-BOUNDARIES-005
  - ACC-KUM-BOUNDARIES-006
  - ACC-KUM-BOUNDARIES-007
decisions:
- decision_id: DEC-KUM-BOUNDARIES
  anchor_refs:
  - TOPIC-KUM-BOUNDARIES
  decision: Adopt the complete behavior declared in this topic; listed implementations are provenance, not dependencies
    or imported authority.
  rationale: Reuse established interaction and ownership patterns without binding Kumihimo to one UI toolkit.
  research_source_refs:
  - https://dockview.dev/docs/core/overview/
  - https://zed.dev/docs/development/glossary
  - https://github.com/DioxusLabs/taffy
  - https://doc.rust-lang.org/cargo/reference/features.html
  supersedes: []
```

```yaml
kind: specification_topic
topic_id: TOPIC-KUM-LAYOUT
title: Nested windows, alignment and resizing
content_blocks:
- block_id: explanation
  kind: prose
  language: markdown
  requirement_refs: &id001
  - REQ-KUM-LAYOUT-001
  - REQ-KUM-LAYOUT-002
  - REQ-KUM-LAYOUT-003
  - REQ-KUM-LAYOUT-004
  - REQ-KUM-LAYOUT-005
  - REQ-KUM-LAYOUT-006
  - REQ-KUM-LAYOUT-007
- block_id: contract
  kind: structured_contract
  language: yaml
  requirement_refs: *id001
```

```markdown
## Windows inside windows

There is no fixed number of windows, tabs or nesting levels in the conceptual model. A window can contain a workspace of child windows, and each child can itself contain another workspace and its own tabs. Twenty child windows with thirty tabs each is an illustration, not a preset or ceiling.

Hosts choose nested splits, grids, docking and supported floating frames. Recursive window containment is separate from a tree of split lines. Window-count, nesting and tab-count settings belong to the host; practical resource/admission policies remain explicit and configurable rather than defining a fixed-size asset.

Shared split lines align neighbors. Snap previews show where a dragged frame will land. Minimum usable sizes and disclosed overflow preserve access to content in a dense or deeply nested arrangement. Exact parent-relative snap subdivisions and overlapping placement remain in prep research until separately accepted.

Toolbars and tool panels can also be movable surfaces. A toolbar can move from the Studio tab to its containing frame or the root workspace edge; panels can occupy eligible grid positions. The host selects which neighbours or proportions absorb reserved space, and the layout core calculates the resulting geometry. This does not adopt unrestricted overlapping placement or morphing borders. The initial model uses flat records with explicit workspace-to-owner-frame and frame-to-placement-workspace relations; frames may own a child workspace. This preserves recursive frame-owned workspaces without a fixed depth. Tab-owned nested workspaces remain a later optional extension.

The implemented headless geometry core uses flat packing nodes for horizontal/vertical splits and regular weighted grids with explicit non-spanning cells and holes. Each non-floating canonical frame has exactly one leaf. Packing is separate from containment, and Core owns both through one generation and operation ledger; geometry rectangles are derived. Default/unconfigured packing follows frame order. Configured creation or legacy movement removes/prunes the source leaf and appends at the destination root: append to a compatible split or wrap the retained root in a horizontal split. Preserve unrelated proportions, grid tracks/holes and explicit configured-empty state; nested child-workspace plans survive subtree moves. Legacy indexes continue to describe canonical frame order after removal, while explicit docking chooses spatial location. Same-parent unchanged order remains a no-op.

Coordinates use validated finite f64 logical DIPs, normalized signed zero, positive weights and nonnegative extents, gaps and minima. Invalid numbers reject before envelope construction; checked arithmetic and impossible constraints reject without changing arrangement. Iterative passes propagate minima and allocate remaining space according to host weights using shared edges. Canonical geometry context contains viewport, measurement/minimum/gap inputs, host tool state and policies; changing any preview input changes generation. Undersized handling is an explicit host choice between rejection and scrollable virtual extent with required extent, clipping and all existing pane rectangles. Actual scrolling/reveal remains adapter work.

Docking previews apply to existing frames/tools over detached hypothetical state, touching no canonical records, generation, stable entity allocation or receipt ledger. Cancellation discards the result. Commit revalidates intent, generation and eligibility through the ordinary operation envelope. Plan-local node selectors may change on structural repair and are separate from stable frame/view/tool identities. Snapping considers only eligible existing edges, shared split boundaries and grid cells with deterministic ties. In-app floats keep one workspace owner, bounded geometry, identities and a return docking location; native popout and modal/temporary-overlay behavior remains separate. Equalize, swap, maximize-pane and restore-layout remain accepted outstanding scope for the following layout-recovery packet.
```

```yaml
requirements:
- requirement_id: REQ-KUM-LAYOUT-001
  anchor_refs:
  - TOPIC-KUM-LAYOUT
  statement: Allow host-chosen window counts and arrangements with no stock pane count or fixed architectural nesting
    ceiling; support recursive window workspaces and horizontal/vertical splits, with any practical admission limits
    exposed as host policy.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-LAYOUT-001
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-LAYOUT
  primitive_refs:
  - PRIM-KUM-LAYOUT
- requirement_id: REQ-KUM-LAYOUT-002
  anchor_refs:
  - TOPIC-KUM-LAYOUT
  statement: Compute alignment from shared split geometry with host-supplied minimum sizes, gaps and proportions;
    reject non-finite sizes and impossible constraints.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-LAYOUT-002
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-LAYOUT
  primitive_refs:
  - PRIM-KUM-LAYOUT
- requirement_id: REQ-KUM-LAYOUT-003
  anchor_refs:
  - TOPIC-KUM-LAYOUT
  statement: Support directional docking, drag previews, snapping to eligible layout boundaries and cancellation
    that restores the pre-drag layout.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-LAYOUT-003
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-LAYOUT
  primitive_refs:
  - PRIM-KUM-LAYOUT
- requirement_id: REQ-KUM-LAYOUT-004
  anchor_refs:
  - TOPIC-KUM-LAYOUT
  statement: Expose equalize, swap, maximize-pane and restore-layout operations through UI and structured commands
    without changing content identities.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-LAYOUT-004
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-LAYOUT
  primitive_refs:
  - PRIM-KUM-LAYOUT
- requirement_id: REQ-KUM-LAYOUT-005
  anchor_refs:
  - TOPIC-KUM-LAYOUT
  statement: For an undersized viewport, apply a host-selected overflow, scrollable workspace or simplified preset
    and preserve a way to reach every existing pane.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-LAYOUT-005
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-LAYOUT
  primitive_refs:
  - PRIM-KUM-LAYOUT
- requirement_id: REQ-KUM-LAYOUT-006
  anchor_refs:
  - TOPIC-KUM-LAYOUT
  statement: A frame shall be able to host another Kumihimo workspace with its own child frames and tabs; preserve
    explicit parent/workspace identities and reject containment cycles without imposing a fixed conceptual depth.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-LAYOUT-006
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-LAYOUT
  primitive_refs:
  - PRIM-KUM-LAYOUT
- requirement_id: REQ-KUM-LAYOUT-007
  anchor_refs:
  - TOPIC-KUM-LAYOUT
  statement: Permit host-provided toolbars and tool panels as independently dockable surfaces at eligible grid positions
    and edges of a tab, containing frame or root workspace; moving a tool surface preserves its identity and host
    editing target, with eligible destinations and resize policy supplied by the host.
  rationale: Preserve operator tool-surface and shared-workspace intent through explicit host boundaries.
  acceptance_refs:
  - ACC-KUM-LAYOUT-007
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-LAYOUT
  primitive_refs:
  - PRIM-KUM-LAYOUT
acceptance:
- acceptance_id: ACC-KUM-LAYOUT-001
  requirement_refs:
  - REQ-KUM-LAYOUT-001
  method: Inspect the future data model; construct a window with 20 child windows and 30 tabs in each, then change
    the counts and add another nesting level.
  expected_result: The example is supported by the composition model without becoming a preset or ceiling; configured
    practical policy is reported separately.
- acceptance_id: ACC-KUM-LAYOUT-002
  requirement_refs:
  - REQ-KUM-LAYOUT-002
  method: Resize host-chosen sparse and dense nested layouts at normal, narrow and invalid dimensions.
  expected_result: Neighbors share boundaries; sizes remain finite and nonnegative; impossible layouts have a visible
    fallback or rejection.
- acceptance_id: ACC-KUM-LAYOUT-003
  requirement_refs:
  - REQ-KUM-LAYOUT-003
  method: Drag through valid and invalid targets, then cancel once and commit once.
  expected_result: Preview and committed placement agree; cancellation preserves the prior pane and tab order.
- acceptance_id: ACC-KUM-LAYOUT-004
  requirement_refs:
  - REQ-KUM-LAYOUT-004
  method: Apply each operation and restore after maximizing.
  expected_result: Only arrangement changes; restoring recovers prior proportions and tab membership.
- acceptance_id: ACC-KUM-LAYOUT-005
  requirement_refs:
  - REQ-KUM-LAYOUT-005
  method: Shrink a host-chosen layout below its current usable minimum and recover the original size.
  expected_result: No content disappears or becomes permanently unreachable; the chosen fallback is reported.
- acceptance_id: ACC-KUM-LAYOUT-006
  requirement_refs:
  - REQ-KUM-LAYOUT-006
  method: Create multiple containment levels, move a child between eligible parents and attempt a cycle.
  expected_result: Nested workspaces retain independent child/tab identities and local arrangement; valid moves
    preserve resources and cycles are rejected.
- acceptance_id: ACC-KUM-LAYOUT-007
  requirement_refs:
  - REQ-KUM-LAYOUT-007
  method: Move a Studio toolbar from its tab to an ancestor workspace edge and a tool panel into an eligible grid
    cell; reject a forbidden destination and cancel one drag.
  expected_result: Identity and target survive allowed moves; previews account for reserved space, forbidden moves
    give a reason, cancellation restores the arrangement and host minimum sizes are respected.
features:
- feature_id: FEAT-KUM-LAYOUT
  title: Nested windows, alignment and resizing
  requirement_refs:
  - REQ-KUM-LAYOUT-001
  - REQ-KUM-LAYOUT-002
  - REQ-KUM-LAYOUT-003
  - REQ-KUM-LAYOUT-004
  - REQ-KUM-LAYOUT-005
  - REQ-KUM-LAYOUT-006
  - REQ-KUM-LAYOUT-007
  primitive_refs:
  - PRIM-KUM-LAYOUT
primitives:
- primitive_id: PRIM-KUM-LAYOUT
  title: Nested windows, alignment and resizing
  contract: Recursive frame/workspace containment and arrangement with no fixed count; each workspace owns local
    geometry and child identities, while platform effects remain in adapters.
primitive_feature_relations:
- relation_id: REL-KUM-LAYOUT
  primitive_ref: PRIM-KUM-LAYOUT
  feature_ref: FEAT-KUM-LAYOUT
  relationship: implements
  constraint_refs: []
gui_design:
- design_id: GUI-KUM-LAYOUT
  applicability: planned
  visual_design_refs: []
  interaction_contract_refs:
  - PRIM-KUM-LAYOUT
  theme_and_component_refs: []
  surface_profile_refs: []
  acceptance_refs:
  - ACC-KUM-LAYOUT-001
  - ACC-KUM-LAYOUT-002
  - ACC-KUM-LAYOUT-003
  - ACC-KUM-LAYOUT-004
  - ACC-KUM-LAYOUT-005
  - ACC-KUM-LAYOUT-006
  - ACC-KUM-LAYOUT-007
decisions:
- decision_id: DEC-KUM-LAYOUT
  anchor_refs:
  - TOPIC-KUM-LAYOUT
  decision: Adopt the complete behavior declared in this topic; listed implementations are provenance, not dependencies
    or imported authority.
  rationale: Reuse established interaction and ownership patterns without binding Kumihimo to one UI toolkit.
  research_source_refs:
  - https://github.com/rerun-io/egui_tiles/blob/main/src/behavior.rs
  - https://raw.githubusercontent.com/microsoft/vscode/main/src/vs/base/browser/ui/grid/gridview.ts
  - https://raw.githubusercontent.com/zed-industries/zed/main/crates/workspace/src/pane_group.rs
  - https://doc.qt.io/qt-6/qtoolbar.html
  - https://doc.qt.io/qt-6/qdockwidget.html
  - https://doc.qt.io/qt-6/qaction.html
  supersedes: []
```

```yaml
kind: specification_topic
topic_id: TOPIC-KUM-TABS
title: Tabs, named groups and color labels
content_blocks:
- block_id: explanation
  kind: prose
  language: markdown
  requirement_refs: &id001
  - REQ-KUM-TABS-001
  - REQ-KUM-TABS-002
  - REQ-KUM-TABS-003
  - REQ-KUM-TABS-004
  - REQ-KUM-TABS-005
  - REQ-KUM-TABS-006
- block_id: contract
  kind: structured_contract
  language: yaml
  requirement_refs: *id001
```

```markdown
## Keep groups recognizable

Each window/frame can have its own tab strip with a host-chosen number of tabs; the asset has no fixed tab count. This applies equally to nested child windows. Groups have their own identity, name and color label; a group is distinct from the pane that currently holds it. Move tabs individually or move a group together.

Color helps recognition, but a readable name or marker remains available. Overflow should expose search or a list instead of making every tab tiny. Large saved collections require a virtualized list rather than a live widget and engine for every saved tab.
```

```yaml
requirements:
- requirement_id: REQ-KUM-TABS-001
  anchor_refs:
  - TOPIC-KUM-TABS
  statement: Support ordered tabs, selection, close/reopen metadata, reorder and movement between panes; host close
    vetoes and unsaved indicators remain effective.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-TABS-001
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-TABS
  primitive_refs:
  - PRIM-KUM-TABS
- requirement_id: REQ-KUM-TABS-002
  anchor_refs:
  - TOPIC-KUM-TABS
  statement: Support named tab groups with stable IDs, configurable color labels, group movement and explicit group
    collapse/expand state independent of pane identity.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-TABS-002
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-TABS
  primitive_refs:
  - PRIM-KUM-TABS
- requirement_id: REQ-KUM-TABS-003
  anchor_refs:
  - TOPIC-KUM-TABS
  statement: Do not encode group meaning only in color; expose labels, accessible descriptions and a visible active-tab
    indicator in every supported theme.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-TABS-003
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-TABS
  primitive_refs:
  - PRIM-KUM-TABS
- requirement_id: REQ-KUM-TABS-004
  anchor_refs:
  - TOPIC-KUM-TABS
  statement: Expose tab overflow/list navigation with host-selectable search and ordering; virtualize large tab
    lists and configure capacity without silently dropping saved tabs.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-TABS-004
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-TABS
  primitive_refs:
  - PRIM-KUM-TABS
- requirement_id: REQ-KUM-TABS-005
  anchor_refs:
  - TOPIC-KUM-TABS
  statement: Expose pinned-tab behavior as a host setting that controls reorder/close semantics explicitly; label
    disabled operations and preserve tab membership.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-TABS-005
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-TABS
  primitive_refs:
  - PRIM-KUM-TABS
- requirement_id: REQ-KUM-TABS-006
  anchor_refs:
  - TOPIC-KUM-TABS
  statement: Support a host-chosen number of tabs in every eligible window/frame, including nested children; do
    not encode a fixed architectural tab-count ceiling, and expose any practical ingestion/live-view limits separately
    through host policy.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-TABS-006
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-TABS
  primitive_refs:
  - PRIM-KUM-TABS
acceptance:
- acceptance_id: ACC-KUM-TABS-001
  requirement_refs:
  - REQ-KUM-TABS-001
  method: Move and close tabs including a dirty tab whose host vetoes close.
  expected_result: Order and active selection stay consistent; rejected close preserves the tab and its unsaved
    state.
- acceptance_id: ACC-KUM-TABS-002
  requirement_refs:
  - REQ-KUM-TABS-002
  method: Rename, recolor, collapse and move a group, then restore its saved layout.
  expected_result: Group identity, members, name, color and collapse state survive movement and restoration.
- acceptance_id: ACC-KUM-TABS-003
  requirement_refs:
  - REQ-KUM-TABS-003
  method: Inspect groups with color disabled and keyboard focus active.
  expected_result: Group identity and selected tab remain understandable without color alone.
- acceptance_id: ACC-KUM-TABS-004
  requirement_refs:
  - REQ-KUM-TABS-004
  method: Navigate a large serialized tab collection under a small visible-list budget.
  expected_result: Any saved tab can be found; mounted list rows and content instances stay within separate declared
    budgets.
- acceptance_id: ACC-KUM-TABS-005
  requirement_refs:
  - REQ-KUM-TABS-005
  method: Enable pinning, try a forbidden close or move, then unpin.
  expected_result: Configured pin rules apply with a reason; unpinning restores ordinary operations.
- acceptance_id: ACC-KUM-TABS-006
  requirement_refs:
  - REQ-KUM-TABS-006
  method: Create the illustrative 20-child-window layout with 30 tabs per child; add/remove tabs and repeat inside
    a nested child.
  expected_result: Each frame owns independent tab membership; counts remain configurable and example values never
    become presets or ceilings.
features:
- feature_id: FEAT-KUM-TABS
  title: Tabs, named groups and color labels
  requirement_refs:
  - REQ-KUM-TABS-001
  - REQ-KUM-TABS-002
  - REQ-KUM-TABS-003
  - REQ-KUM-TABS-004
  - REQ-KUM-TABS-005
  - REQ-KUM-TABS-006
  primitive_refs:
  - PRIM-KUM-TABS
primitives:
- primitive_id: PRIM-KUM-TABS
  title: Tabs, named groups and color labels
  contract: Host-independent contract for tabs, named groups and color labels; platform effects remain in adapters.
primitive_feature_relations:
- relation_id: REL-KUM-TABS
  primitive_ref: PRIM-KUM-TABS
  feature_ref: FEAT-KUM-TABS
  relationship: implements
  constraint_refs: []
gui_design:
- design_id: GUI-KUM-TABS
  applicability: planned
  visual_design_refs: []
  interaction_contract_refs:
  - PRIM-KUM-TABS
  theme_and_component_refs: []
  surface_profile_refs: []
  acceptance_refs:
  - ACC-KUM-TABS-001
  - ACC-KUM-TABS-002
  - ACC-KUM-TABS-003
  - ACC-KUM-TABS-004
  - ACC-KUM-TABS-005
  - ACC-KUM-TABS-006
decisions:
- decision_id: DEC-KUM-TABS
  anchor_refs:
  - TOPIC-KUM-TABS
  decision: Adopt the complete behavior declared in this topic; listed implementations are provenance, not dependencies
    or imported authority.
  rationale: Reuse established interaction and ownership patterns without binding Kumihimo to one UI toolkit.
  research_source_refs:
  - https://dockview.dev/docs/overview/licence/
  - https://docs.rs/egui_dock/latest/egui_dock/widgets/tab_viewer/trait.TabViewer.html
  supersedes: []
```

```yaml
kind: specification_topic
topic_id: TOPIC-KUM-POPOUTS
title: Floating frames, native popouts and temporary popups
content_blocks:
- block_id: explanation
  kind: prose
  language: markdown
  requirement_refs: &id001
  - REQ-KUM-POPOUTS-001
  - REQ-KUM-POPOUTS-002
  - REQ-KUM-POPOUTS-003
  - REQ-KUM-POPOUTS-004
  - REQ-KUM-POPOUTS-005
- block_id: contract
  kind: structured_contract
  language: yaml
  requirement_refs: *id001
```

```markdown
## Window inside a window, or a separate window

An in-app floating frame stays inside the application. A native popout gets its own operating-system window. A popup is a temporary menu, preview or dialog. These need different focus, closing and recovery behavior.

A popout is a handoff: prepare the destination, move the view only when it is usable, and keep a return location. Browsers may block the new window. Closing a popout can return its views to the workspace according to the host policy.
```

```yaml
requirements:
- requirement_id: REQ-KUM-POPOUTS-001
  anchor_refs:
  - TOPIC-KUM-POPOUTS
  statement: Represent in-app floating frames, native popouts and temporary overlays as distinct surface kinds;
    report which the active adapter actually supports.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-POPOUTS-001
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-POPOUTS
  primitive_refs:
  - PRIM-KUM-POPOUTS
- requirement_id: REQ-KUM-POPOUTS-002
  anchor_refs:
  - TOPIC-KUM-POPOUTS
  statement: Support docking and undocking individual views or groups with one canonical layout owner; native popout
    moves commit only after destination readiness.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-POPOUTS-002
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-POPOUTS
  primitive_refs:
  - PRIM-KUM-POPOUTS
- requirement_id: REQ-KUM-POPOUTS-003
  anchor_refs:
  - TOPIC-KUM-POPOUTS
  statement: Persist popout return locations and implement host-selected return-on-close or close-content behavior
    with content vetoes; provide a fallback when the original pane no longer exists.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-POPOUTS-003
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-POPOUTS
  primitive_refs:
  - PRIM-KUM-POPOUTS
- requirement_id: REQ-KUM-POPOUTS-004
  anchor_refs:
  - TOPIC-KUM-POPOUTS
  statement: Apply popup/dialog dismissal, modal scope, focus trapping where modal, focus restoration and anchor-bound
    placement independently of persistent pane docking.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-POPOUTS-004
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-POPOUTS
  primitive_refs:
  - PRIM-KUM-POPOUTS
- requirement_id: REQ-KUM-POPOUTS-005
  anchor_refs:
  - TOPIC-KUM-POPOUTS
  statement: Place drag previews, menus and accessibility/focus surfaces in the window that owns the target; enforce
    browser host-approved origin and popup policies.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-POPOUTS-005
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-POPOUTS
  primitive_refs:
  - PRIM-KUM-POPOUTS
acceptance:
- acceptance_id: ACC-KUM-POPOUTS-001
  requirement_refs:
  - REQ-KUM-POPOUTS-001
  method: Ask adapters with different capabilities to create each kind.
  expected_result: Each supported kind has the declared behavior; unsupported kinds report a reason and never masquerade
    as another kind.
- acceptance_id: ACC-KUM-POPOUTS-002
  requirement_refs:
  - REQ-KUM-POPOUTS-002
  method: Fail destination creation, interrupt the handoff and retry the same operation ID.
  expected_result: No view is lost or duplicated; failure keeps or restores the original layout and retry reconciles
    the observed outcome.
- acceptance_id: ACC-KUM-POPOUTS-003
  requirement_refs:
  - REQ-KUM-POPOUTS-003
  method: Delete the original pane, close its popout and trigger a content veto.
  expected_result: Eligible views return to a reachable fallback; vetoed content survives with an explicit result.
- acceptance_id: ACC-KUM-POPOUTS-004
  requirement_refs:
  - REQ-KUM-POPOUTS-004
  method: Open nested popups and a modal dialog; dismiss by keyboard and pointer.
  expected_result: Focus returns to a valid origin, modal scope is respected and temporary overlays do not become
    persisted panes.
- acceptance_id: ACC-KUM-POPOUTS-005
  requirement_refs:
  - REQ-KUM-POPOUTS-005
  method: Use a real supported popout and a blocked or disallowed web destination.
  expected_result: Overlays and input stay in the correct window; blocked requests preserve content and report the
    restriction.
features:
- feature_id: FEAT-KUM-POPOUTS
  title: Floating frames, native popouts and temporary popups
  requirement_refs:
  - REQ-KUM-POPOUTS-001
  - REQ-KUM-POPOUTS-002
  - REQ-KUM-POPOUTS-003
  - REQ-KUM-POPOUTS-004
  - REQ-KUM-POPOUTS-005
  primitive_refs:
  - PRIM-KUM-POPOUTS
primitives:
- primitive_id: PRIM-KUM-POPOUTS
  title: Floating frames, native popouts and temporary popups
  contract: Host-independent contract for floating frames, native popouts and temporary popups; platform effects
    remain in adapters.
primitive_feature_relations:
- relation_id: REL-KUM-POPOUTS
  primitive_ref: PRIM-KUM-POPOUTS
  feature_ref: FEAT-KUM-POPOUTS
  relationship: implements
  constraint_refs: []
gui_design:
- design_id: GUI-KUM-POPOUTS
  applicability: planned
  visual_design_refs: []
  interaction_contract_refs:
  - PRIM-KUM-POPOUTS
  theme_and_component_refs: []
  surface_profile_refs: []
  acceptance_refs:
  - ACC-KUM-POPOUTS-001
  - ACC-KUM-POPOUTS-002
  - ACC-KUM-POPOUTS-003
  - ACC-KUM-POPOUTS-004
  - ACC-KUM-POPOUTS-005
decisions:
- decision_id: DEC-KUM-POPOUTS
  anchor_refs:
  - TOPIC-KUM-POPOUTS
  decision: Adopt the complete behavior declared in this topic; listed implementations are provenance, not dependencies
    or imported authority.
  rationale: Reuse established interaction and ownership patterns without binding Kumihimo to one UI toolkit.
  research_source_refs:
  - https://dockview.dev/docs/core/groups/popoutGroups/
  - https://github.com/ocornut/imgui/wiki/Multi-Viewports
  supersedes: []
```

```yaml
kind: specification_topic
topic_id: TOPIC-KUM-CHROME
title: Configurable bars, borders and host settings
content_blocks:
- block_id: explanation
  kind: prose
  language: markdown
  requirement_refs: &id001
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
- block_id: contract
  kind: structured_contract
  language: yaml
  requirement_refs: *id001
```

```markdown
## Does the top bar have to stay

The operating system may provide the outer title bar. An app can draw a custom title bar through a platform adapter. The pane tab strip, editor toolbar and browser address bar are separate controls with separate owners.

Allow full, compact, contextual and hidden frame controls. Hidden controls need a reliable reveal command or gesture. Native movement, resizing, system buttons, keyboard access and accessibility must still work where the platform supports customization.

Docking location and editing target are independent. A Studio toolbar moved to the main workspace can remain pinned to its original view. Binding, visibility, command enablement and keyboard focus are separate host-exposed policies. The proposed useful default is now the accepted default profile: keep the target pinned, show tools while that target is visible and usable, hide them for an inactive hidden tab, and restore placement on return. Hosts may override that profile; ordinary tool use does not automatically reveal an editor or pick another open file. Clicking the toolbar itself must preserve its target. A visible Studio view alongside focused Notes can retain its tools.

Hosts choose whether hidden docked tools release their space or leave it reserved. They also declare supported orientations, sizes and destinations. The core handles placement and geometry; optional GUI adapters supply controls; providers supply actual tool actions. Hiding a toolbar does not unload its editor.

The first portable tool-placement packet stores original host owner/origin and bound editing view/resource independently of tab/frame/workspace edge or grid-cell placement. Hosts declare compatible rebinding, eligibility, orientation, preferred/minimum sizes and ordering; only an explicit compatible host rebind changes the target. Binding/visibility policies and supplied target visibility, usability and command applicability are independent of keyboard focus. The default remains pinned with visibility while its target is visible and usable. No provider command is invoked by these semantic controls. Edge strips reserve in top, bottom, left, right order, then host order and tool identity for ties. Hidden release/retain controls reservation; remaining space follows host proportions/minima. Cell conflicts or forbidden placement reject atomically.
```

```yaml
requirements:
- requirement_id: REQ-KUM-CHROME-001
  anchor_refs:
  - TOPIC-KUM-CHROME
  statement: Expose distinct native-titlebar, pane-header/tab-strip and host-content-toolbar settings; host content
    controls shall not become compulsory frame chrome.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-CHROME-001
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-CHROME
  primitive_refs:
  - PRIM-KUM-CHROME
- requirement_id: REQ-KUM-CHROME-002
  anchor_refs:
  - TOPIC-KUM-CHROME
  statement: Provide full, compact, contextual and hidden frame-control profiles, selectable by host and per surface;
    hidden controls retain keyboard and touch-accessible reveal/restore actions.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-CHROME-002
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-CHROME
  primitive_refs:
  - PRIM-KUM-CHROME
- requirement_id: REQ-KUM-CHROME-003
  anchor_refs:
  - TOPIC-KUM-CHROME
  statement: For custom native chrome, preserve adapter-proven system move/resize, system buttons, hit testing and
    accessibility; fall back to supported native chrome when needed.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-CHROME-003
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-CHROME
  primitive_refs:
  - PRIM-KUM-CHROME
- requirement_id: REQ-KUM-CHROME-004
  anchor_refs:
  - TOPIC-KUM-CHROME
  statement: Publish versioned host settings and capability descriptions for layout, tabs, chrome, scrollbars, backgrounds,
    input and lifecycle; define precedence as defaults, host profile, workspace override, surface override, constrained
    by host policy and adapter capability.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-CHROME-004
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-CHROME
  primitive_refs:
  - PRIM-KUM-CHROME
- requirement_id: REQ-KUM-CHROME-005
  anchor_refs:
  - TOPIC-KUM-CHROME
  statement: Allow safe settings changes at runtime; validate ranges, preserve layout identity and report settings
    that require adapter recreation or restart.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-CHROME-005
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-CHROME
  primitive_refs:
  - PRIM-KUM-CHROME
- requirement_id: REQ-KUM-CHROME-006
  anchor_refs:
  - TOPIC-KUM-CHROME
  statement: Keep a tool surface placement parent, host content owner and bound editing view/resource separate;
    docking at an ancestor edge shall not silently retarget the tool to another open file.
  rationale: Preserve operator tool-surface and shared-workspace intent through explicit host boundaries.
  acceptance_refs:
  - ACC-KUM-CHROME-006
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-CHROME
  primitive_refs:
  - PRIM-KUM-CHROME
- requirement_id: REQ-KUM-CHROME-007
  anchor_refs:
  - TOPIC-KUM-CHROME
  statement: Expose independent host-selectable tool binding and visibility policies, including pinned target, active-compatible
    target and originating-tab visibility; a useful default is pinned binding with visibility while its target is
    visible and usable, without auto-revealing hidden editors on ordinary tool activation.
  rationale: Preserve operator tool-surface and shared-workspace intent through explicit host boundaries.
  acceptance_refs:
  - ACC-KUM-CHROME-007
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-CHROME
  primitive_refs:
  - PRIM-KUM-CHROME
- requirement_id: REQ-KUM-CHROME-008
  anchor_refs:
  - TOPIC-KUM-CHROME
  statement: Treat keyboard focus, target visibility and command applicability as separate states; focusing a toolbar
    shall not hide it or lose its editing target, and another focused pane shall not by itself hide tools for a
    still-visible target.
  rationale: Preserve operator tool-surface and shared-workspace intent through explicit host boundaries.
  acceptance_refs:
  - ACC-KUM-CHROME-008
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-CHROME
  primitive_refs:
  - PRIM-KUM-CHROME
- requirement_id: REQ-KUM-CHROME-009
  anchor_refs:
  - TOPIC-KUM-CHROME
  statement: Let the host choose whether a hidden docked tool surface releases or retains reserved layout space;
    core layout computes affected geometry from that policy, host minimum sizes and proportions, while hiding controls
    does not itself dispose or suspend content.
  rationale: Preserve operator tool-surface and shared-workspace intent through explicit host boundaries.
  acceptance_refs:
  - ACC-KUM-CHROME-009
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-CHROME
  primitive_refs:
  - PRIM-KUM-CHROME
- requirement_id: REQ-KUM-CHROME-010
  anchor_refs:
  - TOPIC-KUM-CHROME
  statement: Accept host-declared tool-surface orientation, preferred/minimum sizes, eligible edges/grid positions
    and overflow behaviour; GUI adapters present drag handles and controls, the portable core stores semantic placement
    and geometry, and content providers supply actual tool commands.
  rationale: Preserve operator tool-surface and shared-workspace intent through explicit host boundaries.
  acceptance_refs:
  - ACC-KUM-CHROME-010
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-CHROME
  primitive_refs:
  - PRIM-KUM-CHROME
acceptance:
- acceptance_id: ACC-KUM-CHROME-001
  requirement_refs:
  - REQ-KUM-CHROME-001
  method: Render editor and browser hosts with different toolbar and frame settings.
  expected_result: Each owner can configure its own controls without unintentionally hiding another owner's controls.
- acceptance_id: ACC-KUM-CHROME-002
  requirement_refs:
  - REQ-KUM-CHROME-002
  method: Hide controls, operate the frame by keyboard and touch, then reveal them.
  expected_result: Move, close, select and restore remain reachable; hiding creates no inaccessible workspace.
- acceptance_id: ACC-KUM-CHROME-003
  requirement_refs:
  - REQ-KUM-CHROME-003
  method: Exercise window movement and controls on the exact supported desktop adapter.
  expected_result: Native operations work with declared customization; unsupported customization has an explicit
    fallback.
- acceptance_id: ACC-KUM-CHROME-004
  requirement_refs:
  - REQ-KUM-CHROME-004
  method: Apply conflicting levels and an unavailable capability, then inspect effective settings.
  expected_result: The effective value and its source are visible; prohibited or unsupported settings return a reason.
- acceptance_id: ACC-KUM-CHROME-005
  requirement_refs:
  - REQ-KUM-CHROME-005
  method: Change theme, control density and adapter-sensitive settings during use.
  expected_result: Safe changes apply without content loss; deferred changes show their required transition.
- acceptance_id: ACC-KUM-CHROME-006
  requirement_refs:
  - REQ-KUM-CHROME-006
  method: Dock a toolbar at the root with two different Studio files open, switch tabs and invoke a tool.
  expected_result: Only the declared target can receive the action; a missing or incompatible target produces an
    explicit unavailable state without changing another file.
- acceptance_id: ACC-KUM-CHROME-007
  requirement_refs:
  - REQ-KUM-CHROME-007
  method: Switch between Studio, Notes and terminal tabs while a Studio toolbar is docked at an ancestor edge.
  expected_result: Selected host policy controls visibility and binding; returning restores placement and target,
    and ordinary activation never implicitly reveals a hidden target.
- acceptance_id: ACC-KUM-CHROME-008
  requirement_refs:
  - REQ-KUM-CHROME-008
  method: Click a docked toolbar and then focus a Notes pane beside its Studio target.
  expected_result: Toolbar focus does not invalidate the target; visibility follows declared policy and commands
    remain host-validated at invocation.
- acceptance_id: ACC-KUM-CHROME-009
  requirement_refs:
  - REQ-KUM-CHROME-009
  method: Hide and restore a root toolbar under release-space and retain-space policies, including an undersized
    workspace.
  expected_result: Reserved space and neighbouring pane geometry follow the selected policy; placement is retained,
    overflow/rejection is explicit and content lifecycle remains separately acknowledged.
- acceptance_id: ACC-KUM-CHROME-010
  requirement_refs:
  - REQ-KUM-CHROME-010
  method: Move a horizontal toolbar to a permitted vertical edge, and attempt a placement unsupported by its provider.
  expected_result: Supported orientation and sizes are used; unsupported placement is rejected or handled through
    the declared fallback without inventing editor commands.
features:
- feature_id: FEAT-KUM-CHROME
  title: Configurable bars, borders and host settings
  requirement_refs:
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
  primitive_refs:
  - PRIM-KUM-CHROME
primitives:
- primitive_id: PRIM-KUM-CHROME
  title: Configurable bars, borders and host settings
  contract: Host-independent contract for configurable bars, borders and host settings; platform effects remain
    in adapters.
primitive_feature_relations:
- relation_id: REL-KUM-CHROME
  primitive_ref: PRIM-KUM-CHROME
  feature_ref: FEAT-KUM-CHROME
  relationship: implements
  constraint_refs: []
gui_design:
- design_id: GUI-KUM-CHROME
  applicability: planned
  visual_design_refs: []
  interaction_contract_refs:
  - PRIM-KUM-CHROME
  theme_and_component_refs: []
  surface_profile_refs: []
  acceptance_refs:
  - ACC-KUM-CHROME-001
  - ACC-KUM-CHROME-002
  - ACC-KUM-CHROME-003
  - ACC-KUM-CHROME-004
  - ACC-KUM-CHROME-005
  - ACC-KUM-CHROME-006
  - ACC-KUM-CHROME-007
  - ACC-KUM-CHROME-008
  - ACC-KUM-CHROME-009
  - ACC-KUM-CHROME-010
decisions:
- decision_id: DEC-KUM-CHROME
  anchor_refs:
  - TOPIC-KUM-CHROME
  decision: Adopt the complete behavior declared in this topic; listed implementations are provenance, not dependencies
    or imported authority.
  rationale: Reuse established interaction and ownership patterns without binding Kumihimo to one UI toolkit.
  research_source_refs:
  - https://code.visualstudio.com/docs/configure/custom-layout
  - https://dockview.dev/docs/core/panels/headerPosition/
  - https://doc.qt.io/qt-6/qtoolbar.html
  - https://doc.qt.io/qt-6/qdockwidget.html
  - https://doc.qt.io/qt-6/qaction.html
  supersedes: []
```

```yaml
kind: specification_topic
topic_id: TOPIC-KUM-CONTENT
title: Hosted modules and plugin contracts
content_blocks:
- block_id: explanation
  kind: prose
  language: markdown
  requirement_refs: &id001
  - REQ-KUM-CONTENT-001
  - REQ-KUM-CONTENT-002
  - REQ-KUM-CONTENT-003
  - REQ-KUM-CONTENT-004
  - REQ-KUM-CONTENT-005
  - REQ-KUM-CONTENT-006
  - REQ-KUM-CONTENT-007
- block_id: contract
  kind: structured_contract
  language: yaml
  requirement_refs: *id001
```

```markdown
## Bring your own content

An editor, a browser, a video player and a 3D viewer can all occupy a frame if they implement the host contract. The contract provides geometry, input, focus, redraw requests and lifecycle notifications; the content reports what it can do.

Plugins may fail or disappear. Keep a labeled placeholder and recovery reference instead of deleting their tabs. Security and process isolation belong to the host integration; a decorative frame cannot provide them.

One host asset may have multiple declared views, such as an image in Notes, a moodboard and Studio, with independent per-view state. Shared-view support must be explicit. The host decides linked references versus independent copies and coordinates revisions and content undo. Providers can declare preview, quick-edit or full-editor presentations; Kumihimo does not implement image tools, rich-text editing, scene tools or kanban transitions. For Portal, dragging an arrangement surface and changing a card column are distinct host-routed actions.
```

```yaml
requirements:
- requirement_id: REQ-KUM-CONTENT-001
  anchor_refs:
  - TOPIC-KUM-CONTENT
  statement: Define a versioned content-provider contract for attach/detach, view creation, geometry, input/focus,
    redraw, close veto, serializable restore references and declared capabilities.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-CONTENT-001
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-CONTENT
  primitive_refs:
  - PRIM-KUM-CONTENT
- requirement_id: REQ-KUM-CONTENT-002
  anchor_refs:
  - TOPIC-KUM-CONTENT
  statement: Keep provider resource ownership and cleanup explicit; a frame move changes attachment rather than
    disposing the resource unless the provider confirms disposal.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-CONTENT-002
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-CONTENT
  primitive_refs:
  - PRIM-KUM-CONTENT
- requirement_id: REQ-KUM-CONTENT-003
  anchor_refs:
  - TOPIC-KUM-CONTENT
  statement: On provider failure, missing plugin or unsupported resource type, retain layout position and restore
    reference in a labeled placeholder with host recovery actions.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-CONTENT-003
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-CONTENT
  primitive_refs:
  - PRIM-KUM-CONTENT
- requirement_id: REQ-KUM-CONTENT-004
  anchor_refs:
  - TOPIC-KUM-CONTENT
  statement: Validate provider events at the boundary, bound queues and route events by stable view/resource IDs;
    host policy decides trust, permissions and isolation.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-CONTENT-004
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-CONTENT
  primitive_refs:
  - PRIM-KUM-CONTENT
- requirement_id: REQ-KUM-CONTENT-005
  anchor_refs:
  - TOPIC-KUM-CONTENT
  statement: Allow capable providers to expose embedded previews, quick-edit views and full editor views through
    generic content contracts; each view retains an explicit resource reference and provider-declared capabilities,
    and the supplying host owns editor controls and inline document placement.
  rationale: Preserve operator tool-surface and shared-workspace intent through explicit host boundaries.
  acceptance_refs:
  - ACC-KUM-CONTENT-005
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-CONTENT
  primitive_refs:
  - PRIM-KUM-CONTENT
- requirement_id: REQ-KUM-CONTENT-006
  anchor_refs:
  - TOPIC-KUM-CONTENT
  statement: Keep shared asset resolution, linked-versus-independent-copy semantics, cross-module update coordination
    and content-edit undo in the host; moving or docking frames affects arrangement without silently copying resources
    or rewriting document/kanban meaning.
  rationale: Preserve operator tool-surface and shared-workspace intent through explicit host boundaries.
  acceptance_refs:
  - ACC-KUM-CONTENT-006
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-CONTENT
  primitive_refs:
  - PRIM-KUM-CONTENT
- requirement_id: REQ-KUM-CONTENT-007
  anchor_refs:
  - TOPIC-KUM-CONTENT
  statement: Have the host resolve and validate tool targets and command applicability at invocation, including
    stale, hidden, closed or unavailable targets; pinned identity survives view focus changes, while visibility,
    enablement and permission decisions remain host policy.
  rationale: Preserve operator tool-surface and shared-workspace intent through explicit host boundaries.
  acceptance_refs:
  - ACC-KUM-CONTENT-007
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-CONTENT
  primitive_refs:
  - PRIM-KUM-CONTENT
acceptance:
- acceptance_id: ACC-KUM-CONTENT-001
  requirement_refs:
  - REQ-KUM-CONTENT-001
  method: Attach providers with different capability sets and an incompatible contract version.
  expected_result: Supported providers receive scoped events; incompatible providers are rejected with a recoverable
    placeholder.
- acceptance_id: ACC-KUM-CONTENT-002
  requirement_refs:
  - REQ-KUM-CONTENT-002
  method: Move, detach and dispose a resource while counting provider callbacks.
  expected_result: Moves preserve resources; disposal is acknowledged once and no orphan attachment remains.
- acceptance_id: ACC-KUM-CONTENT-003
  requirement_refs:
  - REQ-KUM-CONTENT-003
  method: Restore a workspace with a removed provider and reattach it later.
  expected_result: Other panes work; the missing view remains recoverable without deleting its saved reference.
- acceptance_id: ACC-KUM-CONTENT-004
  requirement_refs:
  - REQ-KUM-CONTENT-004
  method: Send malformed events, excess work and an event for another provider's view.
  expected_result: Invalid or misrouted work is rejected visibly; bounded queues keep arrangement interactions responsive.
- acceptance_id: ACC-KUM-CONTENT-005
  requirement_refs:
  - REQ-KUM-CONTENT-005
  method: Use one mock asset in a Notes-like embed, moodboard and full editor through providers with different presentation
    capabilities.
  expected_result: Supported views retain the declared resource identity with independent view state; unsupported
    presentations report a reason and do not imply arbitrary editor compatibility.
- acceptance_id: ACC-KUM-CONTENT-006
  requirement_refs:
  - REQ-KUM-CONTENT-006
  method: Move a linked image view between frames and simulate an independent copy and a kanban-column command through
    host integration.
  expected_result: Frame movement preserves reference identity; copy and domain changes occur only through explicit
    host operations and their owning undo scope.
- acceptance_id: ACC-KUM-CONTENT-007
  requirement_refs:
  - REQ-KUM-CONTENT-007
  method: Queue a tool action, change or close its target before delivery, and focus the toolbar itself.
  expected_result: Stale or unavailable work is explicitly rejected; another file is never substituted and ordinary
    keyboard focus does not retarget tools.
features:
- feature_id: FEAT-KUM-CONTENT
  title: Hosted modules and plugin contracts
  requirement_refs:
  - REQ-KUM-CONTENT-001
  - REQ-KUM-CONTENT-002
  - REQ-KUM-CONTENT-003
  - REQ-KUM-CONTENT-004
  - REQ-KUM-CONTENT-005
  - REQ-KUM-CONTENT-006
  - REQ-KUM-CONTENT-007
  primitive_refs:
  - PRIM-KUM-CONTENT
primitives:
- primitive_id: PRIM-KUM-CONTENT
  title: Hosted modules and plugin contracts
  contract: Host-independent contract for hosted modules and plugin contracts; platform effects remain in adapters.
primitive_feature_relations:
- relation_id: REL-KUM-CONTENT
  primitive_ref: PRIM-KUM-CONTENT
  feature_ref: FEAT-KUM-CONTENT
  relationship: implements
  constraint_refs: []
gui_design:
- design_id: GUI-KUM-CONTENT
  applicability: planned
  visual_design_refs: []
  interaction_contract_refs:
  - PRIM-KUM-CONTENT
  theme_and_component_refs: []
  surface_profile_refs: []
  acceptance_refs:
  - ACC-KUM-CONTENT-001
  - ACC-KUM-CONTENT-002
  - ACC-KUM-CONTENT-003
  - ACC-KUM-CONTENT-004
  - ACC-KUM-CONTENT-005
  - ACC-KUM-CONTENT-006
  - ACC-KUM-CONTENT-007
decisions:
- decision_id: DEC-KUM-CONTENT
  anchor_refs:
  - TOPIC-KUM-CONTENT
  decision: Adopt the complete behavior declared in this topic; listed implementations are provenance, not dependencies
    or imported authority.
  rationale: Reuse established interaction and ownership patterns without binding Kumihimo to one UI toolkit.
  research_source_refs:
  - https://docs.rs/egui_dock/latest/egui_dock/widgets/tab_viewer/trait.TabViewer.html
  - https://dockview.dev/docs/core/overview/
  - https://doc.qt.io/qt-6/qtoolbar.html
  - https://doc.qt.io/qt-6/qdockwidget.html
  - https://doc.qt.io/qt-6/qaction.html
  supersedes: []
```

```yaml
kind: specification_topic
topic_id: TOPIC-KUM-RENDER
title: 2D, 3D and rendering surfaces
content_blocks:
- block_id: explanation
  kind: prose
  language: markdown
  requirement_refs: &id001
  - REQ-KUM-RENDER-001
  - REQ-KUM-RENDER-002
  - REQ-KUM-RENDER-003
  - REQ-KUM-RENDER-004
- block_id: contract
  kind: structured_contract
  language: yaml
  requirement_refs: *id001
```

```markdown
## The frame matters without owning the scene

A frame supplies the visible rectangle, clipping boundary, scale and input coordinates. A graphics engine draws the 2D or 3D content inside it. Resizing changes that area; moving to another monitor may change pixel density.

The adapter chooses how content reaches the screen: a callback, a shared texture, a platform surface or another proven path. Some paths cannot support every clipping, transparency or layering effect. These limits need to be visible before content is embedded.
```

```yaml
requirements:
- requirement_id: REQ-KUM-RENDER-001
  anchor_refs:
  - TOPIC-KUM-RENDER
  statement: Deliver logical bounds, physical pixel size, scale, clipping region and visibility to each content
    renderer; map pointer coordinates into that view and update them when geometry changes.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-RENDER-001
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-RENDER
  primitive_refs:
  - PRIM-KUM-RENDER
- requirement_id: REQ-KUM-RENDER-002
  anchor_refs:
  - TOPIC-KUM-RENDER
  statement: Describe render integration capabilities for callbacks, textures and native child surfaces without
    requiring a particular graphics API in the layout core.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-RENDER-002
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-RENDER
  primitive_refs:
  - PRIM-KUM-RENDER
- requirement_id: REQ-KUM-RENDER-003
  anchor_refs:
  - TOPIC-KUM-RENDER
  statement: Keep render-surface lifetime distinct from layout/content identity; handle temporary unavailable surfaces,
    resize, loss and recreation with bounded retry and a visible recovery state.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-RENDER-003
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-RENDER
  primitive_refs:
  - PRIM-KUM-RENDER
- requirement_id: REQ-KUM-RENDER-004
  anchor_refs:
  - TOPIC-KUM-RENDER
  statement: Apply scale-aware input, focus and accessibility integration to embedded graphics; define who handles
    wheel, gesture and keyboard events so frame commands do not consume content interactions accidentally.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-RENDER-004
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-RENDER
  primitive_refs:
  - PRIM-KUM-RENDER
acceptance:
- acceptance_id: ACC-KUM-RENDER-001
  requirement_refs:
  - REQ-KUM-RENDER-001
  method: Resize and move differently scaled panes while picking known 2D/3D points.
  expected_result: The correct view gets coherent geometry and input; clipping contains its output.
- acceptance_id: ACC-KUM-RENDER-002
  requirement_refs:
  - REQ-KUM-RENDER-002
  method: Attach two differently implemented render providers and request unsupported compositing.
  expected_result: Supported paths work through adapters; incompatible clipping/layering requests are explained
    before attachment.
- acceptance_id: ACC-KUM-RENDER-003
  requirement_refs:
  - REQ-KUM-RENDER-003
  method: Cause adapter-reported surface loss and recreation while preserving tabs.
  expected_result: Layout and resource IDs survive; failed recovery is reported without a busy retry loop.
- acceptance_id: ACC-KUM-RENDER-004
  requirement_refs:
  - REQ-KUM-RENDER-004
  method: Use wheel/gesture input over a 3D view and a frame splitter.
  expected_result: Content input and frame manipulation have predictable owners with no accidental scene interaction.
features:
- feature_id: FEAT-KUM-RENDER
  title: 2D, 3D and rendering surfaces
  requirement_refs:
  - REQ-KUM-RENDER-001
  - REQ-KUM-RENDER-002
  - REQ-KUM-RENDER-003
  - REQ-KUM-RENDER-004
  primitive_refs:
  - PRIM-KUM-RENDER
primitives:
- primitive_id: PRIM-KUM-RENDER
  title: 2D, 3D and rendering surfaces
  contract: Host-independent contract for 2d, 3d and rendering surfaces; platform effects remain in adapters.
primitive_feature_relations:
- relation_id: REL-KUM-RENDER
  primitive_ref: PRIM-KUM-RENDER
  feature_ref: FEAT-KUM-RENDER
  relationship: implements
  constraint_refs: []
gui_design:
- design_id: GUI-KUM-RENDER
  applicability: planned
  visual_design_refs: []
  interaction_contract_refs:
  - PRIM-KUM-RENDER
  theme_and_component_refs: []
  surface_profile_refs: []
  acceptance_refs:
  - ACC-KUM-RENDER-001
  - ACC-KUM-RENDER-002
  - ACC-KUM-RENDER-003
  - ACC-KUM-RENDER-004
decisions:
- decision_id: DEC-KUM-RENDER
  anchor_refs:
  - TOPIC-KUM-RENDER
  decision: Adopt the complete behavior declared in this topic; listed implementations are provenance, not dependencies
    or imported authority.
  rationale: Reuse established interaction and ownership patterns without binding Kumihimo to one UI toolkit.
  research_source_refs:
  - https://github.com/emilk/egui/blob/main/crates/egui_demo_app/src/apps/custom3d_wgpu.rs
  - https://github.com/emilk/egui/blob/main/crates/egui-wgpu/src/renderer.rs
  - https://docs.rs/wgpu/latest/wgpu/enum.CurrentSurfaceTexture.html
  supersedes: []
```

```yaml
kind: specification_topic
topic_id: TOPIC-KUM-PERFORMANCE
title: Redraw scheduling and responsiveness
content_blocks:
- block_id: explanation
  kind: prose
  language: markdown
  requirement_refs: &id001
  - REQ-KUM-PERFORMANCE-001
  - REQ-KUM-PERFORMANCE-002
  - REQ-KUM-PERFORMANCE-003
  - REQ-KUM-PERFORMANCE-004
  - REQ-KUM-PERFORMANCE-005
- block_id: contract
  kind: structured_contract
  language: yaml
  requirement_refs: *id001
```

```markdown
## Where Kumihimo can help lag

Kumihimo can report which panes are visible, batch layout changes and request redraw only when needed. It can prioritize a pane you are using and reduce background work when the host agrees.

That helps avoid wasted work; it does not make expensive content magically cheap. A static editor and an animation need different schedules. Measure the actual adapter and content workload before claiming faster rendering or lower resource use.
```

```yaml
requirements:
- requirement_id: REQ-KUM-PERFORMANCE-001
  anchor_refs:
  - TOPIC-KUM-PERFORMANCE
  statement: Expose invalidation and animation demand separately; coalesce redundant layout/redraw requests while
    preserving host-required animation deadlines.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-PERFORMANCE-001
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-PERFORMANCE
  primitive_refs:
  - PRIM-KUM-PERFORMANCE
- requirement_id: REQ-KUM-PERFORMANCE-002
  anchor_refs:
  - TOPIC-KUM-PERFORMANCE
  statement: Report pane visibility, active tab, focus and minimized-window state as separate signals; report unavailable
    OS occlusion as unknown rather than inventing it.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-PERFORMANCE-002
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-PERFORMANCE
  primitive_refs:
  - PRIM-KUM-PERFORMANCE
- requirement_id: REQ-KUM-PERFORMANCE-003
  anchor_refs:
  - TOPIC-KUM-PERFORMANCE
  statement: Keep layout, input and settings work bounded; expose host-configurable priorities, queue limits and
    cancellation so slow content cannot synchronously block frame interaction.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-PERFORMANCE-003
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-PERFORMANCE
  primitive_refs:
  - PRIM-KUM-PERFORMANCE
- requirement_id: REQ-KUM-PERFORMANCE-004
  anchor_refs:
  - TOPIC-KUM-PERFORMANCE
  statement: Bound preview textures, caches and render reservations; share resources only where the host declares
    safe ownership and disposal.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-PERFORMANCE-004
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-PERFORMANCE
  primitive_refs:
  - PRIM-KUM-PERFORMANCE
- requirement_id: REQ-KUM-PERFORMANCE-005
  anchor_refs:
  - TOPIC-KUM-PERFORMANCE
  statement: Expose scoped timing and resource diagnostics for layout, input delay, redraw, live views and cache
    use; distinguish measured usage from estimates and publish no speed claims without scenario evidence.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-PERFORMANCE-005
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-PERFORMANCE
  primitive_refs:
  - PRIM-KUM-PERFORMANCE
acceptance:
- acceptance_id: ACC-KUM-PERFORMANCE-001
  requirement_refs:
  - REQ-KUM-PERFORMANCE-001
  method: Mix static views and an animated view; record requested and presented frames.
  expected_result: Idle static views do not force continuous repaint; the active animation receives its declared
    schedule.
- acceptance_id: ACC-KUM-PERFORMANCE-002
  requirement_refs:
  - REQ-KUM-PERFORMANCE-002
  method: Hide a tab, minimize its window and query an adapter lacking occlusion support.
  expected_result: Each signal retains its own meaning and unknown remains explicit.
- acceptance_id: ACC-KUM-PERFORMANCE-003
  requirement_refs:
  - REQ-KUM-PERFORMANCE-003
  method: Delay content work during splitter drag and enqueue above the configured limit.
  expected_result: Frame interaction continues through the declared nonblocking path; overload/cancellation is observable.
- acceptance_id: ACC-KUM-PERFORMANCE-004
  requirement_refs:
  - REQ-KUM-PERFORMANCE-004
  method: Open many previews, reach the cache budget and dispose a shared resource.
  expected_result: Budget admission/eviction is explicit; remaining consumers retain valid resources.
- acceptance_id: ACC-KUM-PERFORMANCE-005
  requirement_refs:
  - REQ-KUM-PERFORMANCE-005
  method: Inspect a bounded workload report with a provider that cannot report GPU bytes.
  expected_result: Known timings/counts have units and provenance; unavailable values and estimates are labeled.
features:
- feature_id: FEAT-KUM-PERFORMANCE
  title: Redraw scheduling and responsiveness
  requirement_refs:
  - REQ-KUM-PERFORMANCE-001
  - REQ-KUM-PERFORMANCE-002
  - REQ-KUM-PERFORMANCE-003
  - REQ-KUM-PERFORMANCE-004
  - REQ-KUM-PERFORMANCE-005
  primitive_refs:
  - PRIM-KUM-PERFORMANCE
primitives:
- primitive_id: PRIM-KUM-PERFORMANCE
  title: Redraw scheduling and responsiveness
  contract: Host-independent contract for redraw scheduling and responsiveness; platform effects remain in adapters.
primitive_feature_relations:
- relation_id: REL-KUM-PERFORMANCE
  primitive_ref: PRIM-KUM-PERFORMANCE
  feature_ref: FEAT-KUM-PERFORMANCE
  relationship: implements
  constraint_refs: []
gui_design:
- design_id: GUI-KUM-PERFORMANCE
  applicability: planned
  visual_design_refs: []
  interaction_contract_refs:
  - PRIM-KUM-PERFORMANCE
  theme_and_component_refs: []
  surface_profile_refs: []
  acceptance_refs:
  - ACC-KUM-PERFORMANCE-001
  - ACC-KUM-PERFORMANCE-002
  - ACC-KUM-PERFORMANCE-003
  - ACC-KUM-PERFORMANCE-004
  - ACC-KUM-PERFORMANCE-005
decisions:
- decision_id: DEC-KUM-PERFORMANCE
  anchor_refs:
  - TOPIC-KUM-PERFORMANCE
  decision: Adopt the complete behavior declared in this topic; listed implementations are provenance, not dependencies
    or imported authority.
  rationale: Reuse established interaction and ownership patterns without binding Kumihimo to one UI toolkit.
  research_source_refs:
  - https://docs.rs/winit/latest/winit/window/struct.Window.html
  - https://docs.rs/winit/latest/winit/event/enum.WindowEvent.html
  - https://dockview.dev/docs/core/panels/rendering/
  supersedes: []
```

```yaml
kind: specification_topic
topic_id: TOPIC-KUM-TEXT
title: Text reflow and media inside documents
content_blocks:
- block_id: explanation
  kind: prose
  language: markdown
  requirement_refs: &id001
  - REQ-KUM-TEXT-001
  - REQ-KUM-TEXT-002
  - REQ-KUM-TEXT-003
- block_id: contract
  kind: structured_contract
  language: yaml
  requirement_refs: *id001
```

```markdown
## Resizing text and moving embedded media

Kumihimo tells content how much space it has. The editor decides line wrapping, font size, document layout and how an image or embedded tool changes surrounding text. Those decisions need the document model, which the frame does not own.

Kumihimo can provide useful resize information and a consistent embedding slot. A host that supports rich text may then reflow a document or let you drag media through it.
```

```yaml
requirements:
- requirement_id: REQ-KUM-TEXT-001
  anchor_refs:
  - TOPIC-KUM-TEXT
  statement: Emit coherent resize and scale notifications with stable view IDs; the host owns text wrapping, typography,
    inline embeds and document editing.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-TEXT-001
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-TEXT
  primitive_refs:
  - PRIM-KUM-TEXT
- requirement_id: REQ-KUM-TEXT-002
  anchor_refs:
  - TOPIC-KUM-TEXT
  statement: Allow a host to expose embedded-content geometry and input ownership through the content-provider contract;
    do not imply that arbitrary text engines support movable inline media.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-TEXT-002
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-TEXT
  primitive_refs:
  - PRIM-KUM-TEXT
- requirement_id: REQ-KUM-TEXT-003
  anchor_refs:
  - TOPIC-KUM-TEXT
  statement: Permit provider-reported anchored resize/reflow completion so selection and scroll position can be
    preserved by the content owner.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-TEXT-003
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-TEXT
  primitive_refs:
  - PRIM-KUM-TEXT
acceptance:
- acceptance_id: ACC-KUM-TEXT-001
  requirement_refs:
  - REQ-KUM-TEXT-001
  method: Resize a plain editor and a rich document containing an image.
  expected_result: Each host receives geometry and applies its own document behavior without frame-side document
    mutation.
- acceptance_id: ACC-KUM-TEXT-002
  requirement_refs:
  - REQ-KUM-TEXT-002
  method: Attach one rich-text provider with embeds and one without.
  expected_result: Capability differences are reported; the unsupported provider retains ordinary text behavior.
- acceptance_id: ACC-KUM-TEXT-003
  requirement_refs:
  - REQ-KUM-TEXT-003
  method: Resize a provider supporting anchors around a selected paragraph and media block.
  expected_result: The provider can preserve its documented anchor; unavailable anchor support is explicit.
features:
- feature_id: FEAT-KUM-TEXT
  title: Text reflow and media inside documents
  requirement_refs:
  - REQ-KUM-TEXT-001
  - REQ-KUM-TEXT-002
  - REQ-KUM-TEXT-003
  primitive_refs:
  - PRIM-KUM-TEXT
primitives:
- primitive_id: PRIM-KUM-TEXT
  title: Text reflow and media inside documents
  contract: Host-independent contract for text reflow and media inside documents; platform effects remain in adapters.
primitive_feature_relations:
- relation_id: REL-KUM-TEXT
  primitive_ref: PRIM-KUM-TEXT
  feature_ref: FEAT-KUM-TEXT
  relationship: implements
  constraint_refs: []
gui_design:
- design_id: GUI-KUM-TEXT
  applicability: planned
  visual_design_refs: []
  interaction_contract_refs:
  - PRIM-KUM-TEXT
  theme_and_component_refs: []
  surface_profile_refs: []
  acceptance_refs:
  - ACC-KUM-TEXT-001
  - ACC-KUM-TEXT-002
  - ACC-KUM-TEXT-003
decisions:
- decision_id: DEC-KUM-TEXT
  anchor_refs:
  - TOPIC-KUM-TEXT
  decision: Adopt the complete behavior declared in this topic; listed implementations are provenance, not dependencies
    or imported authority.
  rationale: Reuse established interaction and ownership patterns without binding Kumihimo to one UI toolkit.
  research_source_refs:
  - https://docs.rs/egui_dock/latest/egui_dock/widgets/tab_viewer/trait.TabViewer.html
  - https://raw.githubusercontent.com/microsoft/vscode/main/src/vs/base/browser/ui/grid/gridview.ts
  supersedes: []
```

```yaml
kind: specification_topic
topic_id: TOPIC-KUM-NAVIGATION
title: Large scrollbars and content minimaps
content_blocks:
- block_id: explanation
  kind: prose
  language: markdown
  requirement_refs: &id001
  - REQ-KUM-NAVIGATION-001
  - REQ-KUM-NAVIGATION-002
  - REQ-KUM-NAVIGATION-003
  - REQ-KUM-NAVIGATION-004
  - REQ-KUM-NAVIGATION-005
  - REQ-KUM-NAVIGATION-006
- block_id: contract
  kind: structured_contract
  language: yaml
  requirement_refs: *id001
```

```markdown
## A thumb you can grab

Offer adjustable scrollbar rail width, visible thumb width, minimum thumb length, hit area and visibility. The thumb may be larger than its purely proportional size, while still covering the full scroll range.

The host owns scrollable content and its position. A minimap needs host-provided meaning: text lines, markers, scene locations or document sections. Kumihimo provides the slot and navigation connection. A map of the whole workspace is a separate optional idea.

The host selects navigation presentation per its profile/workspace/surface settings: scrollbar thumb and rail only; minimap always visible; scrollbar and minimap together; or scrollbar at rest that transforms into a temporary minimap while scrubbing. The transient behavior is one selectable mode, not a compulsory default.

In transient mode, grabbing the thumb expands a map beside the document and turns the held object into the map's viewport rectangle. Continue the same vertical drag; release hides the map and restores the rail/thumb at the new position. Expansion preserves the grab anchor and document layout. Other modes retain their configured persistent controls.

If a content provider cannot supply minimap data, report that capability and use the host's declared fallback rather than inventing an overview.
```

```yaml
requirements:
- requirement_id: REQ-KUM-NAVIGATION-001
  anchor_refs:
  - TOPIC-KUM-NAVIGATION
  statement: Expose scrollbar rail width, thumb width, minimum thumb length, hit area, contrast and always-visible/auto-hide
    behavior; include a comfortable large-control profile.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-NAVIGATION-001
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-NAVIGATION
  primitive_refs:
  - PRIM-KUM-NAVIGATION
- requirement_id: REQ-KUM-NAVIGATION-002
  anchor_refs:
  - TOPIC-KUM-NAVIGATION
  statement: Preserve complete scroll-range mapping when clamping thumb size; show progress and enable precise keyboard/page
    navigation independently of thumb dimensions.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-NAVIGATION-002
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-NAVIGATION
  primitive_refs:
  - PRIM-KUM-NAVIGATION
- requirement_id: REQ-KUM-NAVIGATION-003
  anchor_refs:
  - TOPIC-KUM-NAVIGATION
  statement: Provide an optional minimap/navigation-provider slot for host-supplied overview data, viewport markers
    and jump actions with bounded update work.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-NAVIGATION-003
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-NAVIGATION
  primitive_refs:
  - PRIM-KUM-NAVIGATION
- requirement_id: REQ-KUM-NAVIGATION-004
  anchor_refs:
  - TOPIC-KUM-NAVIGATION
  statement: Keep document virtualization and semantic overview generation in the host; publish frame navigation
    events and capability/state snapshots without reading entire documents automatically.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-NAVIGATION-004
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-NAVIGATION
  primitive_refs:
  - PRIM-KUM-NAVIGATION
- requirement_id: REQ-KUM-NAVIGATION-005
  anchor_refs:
  - TOPIC-KUM-NAVIGATION
  statement: 'Expose host-selectable navigation modes: scrollbar thumb/rail only, always-visible minimap, persistent
    scrollbar plus minimap, and scrollbar-at-rest with minimap shown during scrubbing; apply existing settings precedence
    per host/workspace/surface and report unsupported provider capabilities with an explicit host-selected fallback.'
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-NAVIGATION-005
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-NAVIGATION
  primitive_refs:
  - PRIM-KUM-NAVIGATION
- requirement_id: REQ-KUM-NAVIGATION-006
  anchor_refs:
  - TOPIC-KUM-NAVIGATION
  statement: In scrub-to-minimap mode, preserve one continuous drag while the thumb/rail presentation expands into
    a map beside the document and the grabbed thumb becomes its viewport rectangle; releasing or settling cancellation
    restores a usable ordinary scrollbar without changing document position merely because the representation changed.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-NAVIGATION-006
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-NAVIGATION
  primitive_refs:
  - PRIM-KUM-NAVIGATION
acceptance:
- acceptance_id: ACC-KUM-NAVIGATION-001
  requirement_refs:
  - REQ-KUM-NAVIGATION-001
  method: Use a long document with the large profile and pointer, touch and keyboard.
  expected_result: The configured minimum thumb and hit area are preserved and navigation remains reachable.
- acceptance_id: ACC-KUM-NAVIGATION-002
  requirement_refs:
  - REQ-KUM-NAVIGATION-002
  method: Scroll from start to end and to known positions using a clamped thumb.
  expected_result: Every position remains reachable; endpoints and provider scroll state agree.
- acceptance_id: ACC-KUM-NAVIGATION-003
  requirement_refs:
  - REQ-KUM-NAVIGATION-003
  method: Attach text and non-text providers with distinct overview formats.
  expected_result: Each overview uses host meaning and jump targets; unsupported providers show no fabricated minimap.
- acceptance_id: ACC-KUM-NAVIGATION-004
  requirement_refs:
  - REQ-KUM-NAVIGATION-004
  method: Navigate a large provider that supplies a sparse overview.
  expected_result: Frame work stays bounded by the supplied overview/window rather than loading all content.
- acceptance_id: ACC-KUM-NAVIGATION-005
  requirement_refs:
  - REQ-KUM-NAVIGATION-005
  method: Select every mode through host settings on providers with and without minimap data; inspect idle, drag
    and release states.
  expected_result: Each supported mode displays its configured controls; transient behavior is optional and capability
    fallback is explicit.
- acceptance_id: ACC-KUM-NAVIGATION-006
  requirement_refs:
  - REQ-KUM-NAVIGATION-006
  method: Press without moving, drag from different grab offsets, release outside the pane and cancel; inspect document
    bounds during expansion.
  expected_result: Stationary press does not jump, pointer continuity and grab offset are preserved, document layout
    stays stable, and release/cancellation restores navigation at the settled position.
features:
- feature_id: FEAT-KUM-NAVIGATION
  title: Large scrollbars and content minimaps
  requirement_refs:
  - REQ-KUM-NAVIGATION-001
  - REQ-KUM-NAVIGATION-002
  - REQ-KUM-NAVIGATION-003
  - REQ-KUM-NAVIGATION-004
  - REQ-KUM-NAVIGATION-005
  - REQ-KUM-NAVIGATION-006
  primitive_refs:
  - PRIM-KUM-NAVIGATION
primitives:
- primitive_id: PRIM-KUM-NAVIGATION
  title: Large scrollbars and content minimaps
  contract: Host-independent contract for large scrollbars and content minimaps; platform effects remain in adapters.
primitive_feature_relations:
- relation_id: REL-KUM-NAVIGATION
  primitive_ref: PRIM-KUM-NAVIGATION
  feature_ref: FEAT-KUM-NAVIGATION
  relationship: implements
  constraint_refs: []
gui_design:
- design_id: GUI-KUM-NAVIGATION
  applicability: planned
  visual_design_refs: []
  interaction_contract_refs:
  - PRIM-KUM-NAVIGATION
  theme_and_component_refs: []
  surface_profile_refs: []
  acceptance_refs:
  - ACC-KUM-NAVIGATION-001
  - ACC-KUM-NAVIGATION-002
  - ACC-KUM-NAVIGATION-003
  - ACC-KUM-NAVIGATION-004
  - ACC-KUM-NAVIGATION-005
  - ACC-KUM-NAVIGATION-006
decisions:
- decision_id: DEC-KUM-NAVIGATION
  anchor_refs:
  - TOPIC-KUM-NAVIGATION
  decision: Adopt the complete behavior declared in this topic; listed implementations are provenance, not dependencies
    or imported authority.
  rationale: Reuse established interaction and ownership patterns without binding Kumihimo to one UI toolkit.
  research_source_refs:
  - https://docs.rs/egui/latest/egui/style/struct.ScrollStyle.html
  - https://docs.rs/egui/latest/egui/containers/scroll_area/struct.ScrollArea.html
  - https://github.com/microsoft/vscode/blob/main/src/vs/editor/browser/viewParts/minimap/minimap.ts
  supersedes: []
```

```yaml
kind: specification_topic
topic_id: TOPIC-KUM-BACKGROUNDS
title: Image and video backgrounds
content_blocks:
- block_id: explanation
  kind: prose
  language: markdown
  requirement_refs: &id001
  - REQ-KUM-BACKGROUNDS-001
  - REQ-KUM-BACKGROUNDS-002
  - REQ-KUM-BACKGROUNDS-003
  - REQ-KUM-BACKGROUNDS-004
- block_id: contract
  kind: structured_contract
  language: yaml
  requirement_refs: *id001
```

```markdown
## Media behind your work

Allow a pane or workspace to use an image or video background with brightness, opacity and crop controls. Keep controls and text readable above it. Host policy can mute decorative video, reduce motion or choose a still poster.

Brightness and transparency are visual controls. Decoder scheduling, resolution and pausing are the resource controls. Hiding a background should request reduced work from its media provider and wait for confirmation.
```

```yaml
requirements:
- requirement_id: REQ-KUM-BACKGROUNDS-001
  anchor_refs:
  - TOPIC-KUM-BACKGROUNDS
  statement: Offer image/video background slots at workspace and frame scope with host-exposed brightness, opacity,
    fit/crop and layering settings; validate ranges and adapter compositing support.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-BACKGROUNDS-001
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-BACKGROUNDS
  primitive_refs:
  - PRIM-KUM-BACKGROUNDS
- requirement_id: REQ-KUM-BACKGROUNDS-002
  anchor_refs:
  - TOPIC-KUM-BACKGROUNDS
  statement: Provide a legibility overlay or host contrast treatment plus reduced-motion/poster and decorative-audio
    policies; background media shall not capture ordinary content input.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-BACKGROUNDS-002
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-BACKGROUNDS
  primitive_refs:
  - PRIM-KUM-BACKGROUNDS
- requirement_id: REQ-KUM-BACKGROUNDS-003
  anchor_refs:
  - TOPIC-KUM-BACKGROUNDS
  statement: Expose background visibility and frame-rate/resolution budget requests to the media provider; report
    requested versus confirmed pause/throttle states.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-BACKGROUNDS-003
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-BACKGROUNDS
  primitive_refs:
  - PRIM-KUM-BACKGROUNDS
- requirement_id: REQ-KUM-BACKGROUNDS-004
  anchor_refs:
  - TOPIC-KUM-BACKGROUNDS
  statement: Bound background caches and decoder allocations; permit shared media only through a host-supported
    ownership contract and release unused allocations safely.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-BACKGROUNDS-004
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-BACKGROUNDS
  primitive_refs:
  - PRIM-KUM-BACKGROUNDS
acceptance:
- acceptance_id: ACC-KUM-BACKGROUNDS-001
  requirement_refs:
  - REQ-KUM-BACKGROUNDS-001
  method: Apply media backgrounds under opaque and transparent host content.
  expected_result: Supported settings affect the selected layer; unavailable composition is disclosed and content
    controls stay usable.
- acceptance_id: ACC-KUM-BACKGROUNDS-002
  requirement_refs:
  - REQ-KUM-BACKGROUNDS-002
  method: Use a bright moving background with reduced motion and active controls.
  expected_result: The configured still/motion policy applies, controls remain readable and input reaches content.
- acceptance_id: ACC-KUM-BACKGROUNDS-003
  requirement_refs:
  - REQ-KUM-BACKGROUNDS-003
  method: Hide a pane while its media provider delays or refuses a pause request.
  expected_result: The state shows the pending/refused request and never claims reduced decode work without confirmation.
- acceptance_id: ACC-KUM-BACKGROUNDS-004
  requirement_refs:
  - REQ-KUM-BACKGROUNDS-004
  method: Reuse one background across panes and close all but one.
  expected_result: A declared shared provider remains valid for the last consumer and allocations obey the configured
    limit.
features:
- feature_id: FEAT-KUM-BACKGROUNDS
  title: Image and video backgrounds
  requirement_refs:
  - REQ-KUM-BACKGROUNDS-001
  - REQ-KUM-BACKGROUNDS-002
  - REQ-KUM-BACKGROUNDS-003
  - REQ-KUM-BACKGROUNDS-004
  primitive_refs:
  - PRIM-KUM-BACKGROUNDS
primitives:
- primitive_id: PRIM-KUM-BACKGROUNDS
  title: Image and video backgrounds
  contract: Host-independent contract for image and video backgrounds; platform effects remain in adapters.
primitive_feature_relations:
- relation_id: REL-KUM-BACKGROUNDS
  primitive_ref: PRIM-KUM-BACKGROUNDS
  feature_ref: FEAT-KUM-BACKGROUNDS
  relationship: implements
  constraint_refs: []
gui_design:
- design_id: GUI-KUM-BACKGROUNDS
  applicability: planned
  visual_design_refs: []
  interaction_contract_refs:
  - PRIM-KUM-BACKGROUNDS
  theme_and_component_refs: []
  surface_profile_refs: []
  acceptance_refs:
  - ACC-KUM-BACKGROUNDS-001
  - ACC-KUM-BACKGROUNDS-002
  - ACC-KUM-BACKGROUNDS-003
  - ACC-KUM-BACKGROUNDS-004
decisions:
- decision_id: DEC-KUM-BACKGROUNDS
  anchor_refs:
  - TOPIC-KUM-BACKGROUNDS
  decision: Adopt the complete behavior declared in this topic; listed implementations are provenance, not dependencies
    or imported authority.
  rationale: Reuse established interaction and ownership patterns without binding Kumihimo to one UI toolkit.
  research_source_refs:
  - https://dockview.dev/docs/core/panels/rendering/
  - https://developer.chrome.com/docs/web-platform/page-lifecycle-api
  supersedes: []
```

```yaml
kind: specification_topic
topic_id: TOPIC-KUM-LIFECYCLE
title: Many saved tabs and bounded live content
content_blocks:
- block_id: explanation
  kind: prose
  language: markdown
  requirement_refs: &id001
  - REQ-KUM-LIFECYCLE-001
  - REQ-KUM-LIFECYCLE-002
  - REQ-KUM-LIFECYCLE-003
  - REQ-KUM-LIFECYCLE-004
  - REQ-KUM-LIFECYCLE-005
  - REQ-KUM-LIFECYCLE-006
- block_id: contract
  kind: structured_contract
  language: yaml
  requirement_refs: *id001
```

```markdown
## Thousands of tabs without thousands of active engines

Keep a saved tab record separate from a live editor, decoder or browser instance. The host may keep a few views active, leave others warm, freeze supported work, or unload and later reload content.

Kumihimo supplies visibility and user-priority signals. The host resource manager supplies policy and real CPU, memory and GPU information. Hidden, throttled, frozen and unloaded are different states. Downloads, audio, unsaved content and explicit keep-alive choices may need protection. No universal memory ceiling or exact browser-session restoration is promised.
```

```yaml
requirements:
- requirement_id: REQ-KUM-LIFECYCLE-001
  anchor_refs:
  - TOPIC-KUM-LIFECYCLE
  statement: Separate persistent logical-tab records from live provider instances; expose independent bounds for
    saved-record ingestion, mounted UI, live engines, caches and pending work.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-LIFECYCLE-001
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-LIFECYCLE
  primitive_refs:
  - PRIM-KUM-LIFECYCLE
- requirement_id: REQ-KUM-LIFECYCLE-002
  anchor_refs:
  - TOPIC-KUM-LIFECYCLE
  statement: Define requested and provider-confirmed visible, background, throttled, frozen, unloaded, restoring
    and failed states with unsupported/unknown results; hiding alone shall not assert suspension or memory release.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-LIFECYCLE-002
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-LIFECYCLE
  primitive_refs:
  - PRIM-KUM-LIFECYCLE
- requirement_id: REQ-KUM-LIFECYCLE-003
  anchor_refs:
  - TOPIC-KUM-LIFECYCLE
  statement: Let the host resource manager set priorities, live-instance reservations, memory-pressure response
    and idle policy; the frame exposes visibility, interaction recency and explicit keep-alive requests.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-LIFECYCLE-003
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-LIFECYCLE
  primitive_refs:
  - PRIM-KUM-LIFECYCLE
- requirement_id: REQ-KUM-LIFECYCLE-004
  anchor_refs:
  - TOPIC-KUM-LIFECYCLE
  statement: Require host/provider eligibility and a recoverable reference before destructive unloading; honor unsaved
    work, audio, downloads and other host-declared protection reasons.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-LIFECYCLE-004
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-LIFECYCLE
  primitive_refs:
  - PRIM-KUM-LIFECYCLE
- requirement_id: REQ-KUM-LIFECYCLE-005
  anchor_refs:
  - TOPIC-KUM-LIFECYCLE
  statement: Use operation IDs, generations, bounded waits and reconciliation for asynchronous suspend/unload/restore;
    expose failed and unknown outcomes and never retry destructive effects blindly.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-LIFECYCLE-005
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-LIFECYCLE
  primitive_refs:
  - PRIM-KUM-LIFECYCLE
- requirement_id: REQ-KUM-LIFECYCLE-006
  anchor_refs:
  - TOPIC-KUM-LIFECYCLE
  statement: 'Report restoration strength explicitly: layout restoration, provider-defined document state or page
    reload are distinct; do not promise arbitrary in-memory JavaScript or media state survives unloading.'
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-LIFECYCLE-006
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-LIFECYCLE
  primitive_refs:
  - PRIM-KUM-LIFECYCLE
acceptance:
- acceptance_id: ACC-KUM-LIFECYCLE-001
  requirement_refs:
  - REQ-KUM-LIFECYCLE-001
  method: Load a declared large tab collection with a small live-instance budget.
  expected_result: Saved records remain reachable while live allocations obey admission; counts distinguish records,
    views and engines.
- acceptance_id: ACC-KUM-LIFECYCLE-002
  requirement_refs:
  - REQ-KUM-LIFECYCLE-002
  method: Request each state from capable and incapable providers.
  expected_result: Only acknowledged supported states become confirmed; unsupported requests preserve content and
    report a reason.
- acceptance_id: ACC-KUM-LIFECYCLE-003
  requirement_refs:
  - REQ-KUM-LIFECYCLE-003
  method: Inject host pressure while an active view and keep-alive view compete for a slot.
  expected_result: Policy receives honest signals, admission accounts for pending reservations and protected behavior
    is reported.
- acceptance_id: ACC-KUM-LIFECYCLE-004
  requirement_refs:
  - REQ-KUM-LIFECYCLE-004
  method: Attempt to unload a dirty editor, protected download and reloadable idle page.
  expected_result: Protected resources are retained or explicitly resolved by the host; eligible content has a saved
    recovery reference.
- acceptance_id: ACC-KUM-LIFECYCLE-005
  requirement_refs:
  - REQ-KUM-LIFECYCLE-005
  method: Lose an acknowledgment, restart the frame connection and reconcile provider state.
  expected_result: Recovery determines the observed outcome before retry; no duplicate disposal or false completion
    occurs.
- acceptance_id: ACC-KUM-LIFECYCLE-006
  requirement_refs:
  - REQ-KUM-LIFECYCLE-006
  method: Restore providers offering different state guarantees and one failed restore.
  expected_result: Each view shows its actual restoration level or a recoverable failure.
features:
- feature_id: FEAT-KUM-LIFECYCLE
  title: Many saved tabs and bounded live content
  requirement_refs:
  - REQ-KUM-LIFECYCLE-001
  - REQ-KUM-LIFECYCLE-002
  - REQ-KUM-LIFECYCLE-003
  - REQ-KUM-LIFECYCLE-004
  - REQ-KUM-LIFECYCLE-005
  - REQ-KUM-LIFECYCLE-006
  primitive_refs:
  - PRIM-KUM-LIFECYCLE
primitives:
- primitive_id: PRIM-KUM-LIFECYCLE
  title: Many saved tabs and bounded live content
  contract: Host-directed lifecycle requests and acknowledged provider state over persistent tab references and
    bounded live reservations.
primitive_feature_relations:
- relation_id: REL-KUM-LIFECYCLE
  primitive_ref: PRIM-KUM-LIFECYCLE
  feature_ref: FEAT-KUM-LIFECYCLE
  relationship: implements
  constraint_refs: []
gui_design:
- design_id: GUI-KUM-LIFECYCLE
  applicability: planned
  visual_design_refs: []
  interaction_contract_refs:
  - PRIM-KUM-LIFECYCLE
  theme_and_component_refs: []
  surface_profile_refs: []
  acceptance_refs:
  - ACC-KUM-LIFECYCLE-001
  - ACC-KUM-LIFECYCLE-002
  - ACC-KUM-LIFECYCLE-003
  - ACC-KUM-LIFECYCLE-004
  - ACC-KUM-LIFECYCLE-005
  - ACC-KUM-LIFECYCLE-006
decisions:
- decision_id: DEC-KUM-LIFECYCLE
  anchor_refs:
  - TOPIC-KUM-LIFECYCLE
  decision: Adopt the complete behavior declared in this topic; listed implementations are provenance, not dependencies
    or imported authority.
  rationale: Reuse established interaction and ownership patterns without binding Kumihimo to one UI toolkit.
  research_source_refs:
  - https://developer.chrome.com/docs/web-platform/page-lifecycle-api
  - https://doc.servo.org/servo/webview/struct.WebView.html
  - https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-creatememoryresourcenotification
  supersedes: []
```

```yaml
kind: specification_topic
topic_id: TOPIC-KUM-SERVO
title: Future Handshake browser and Servo adapter
content_blocks:
- block_id: explanation
  kind: prose
  language: markdown
  requirement_refs: &id001
  - REQ-KUM-SERVO-001
  - REQ-KUM-SERVO-002
  - REQ-KUM-SERVO-003
  - REQ-KUM-SERVO-004
  - REQ-KUM-SERVO-005
- block_id: contract
  kind: structured_contract
  language: yaml
  requirement_refs: *id001
```

```markdown
## Keep the browser boundary explicit

The future browser module supplies Servo; Kumihimo supplies arrangement, geometry, input routing and lifecycle requests. Current Servo documentation describes GL/GLES contexts, and views sharing a context also share its resized dimensions. Independently sized panes therefore need an adapter-proven context/composition design. [Servo rendering context](https://doc.servo.org/servo/trait.RenderingContext.html), [WebView API](https://doc.servo.org/servo/webview/struct.WebView.html).

Direct composition with a wgpu host, independent resizing, Windows media playback, YouTube behavior and building native dependencies from the ARM tablet remain unproven here. These are scoped integration experiments before any compatibility promise. The frame must remain useful with simple content while that work is unresolved.
```

```yaml
requirements:
- requirement_id: REQ-KUM-SERVO-001
  anchor_refs:
  - TOPIC-KUM-SERVO
  statement: Keep Servo and browser-specific state behind a separately selectable content adapter; the core workspace
    shall run without building or linking the browser engine.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-SERVO-001
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-SERVO
  primitive_refs:
  - PRIM-KUM-SERVO
- requirement_id: REQ-KUM-SERVO-002
  anchor_refs:
  - TOPIC-KUM-SERVO
  statement: Prove the selected Servo context/composition design with independently sized panes before advertising
    multi-pane browser support; prevent one resize from silently changing another pane.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-SERVO-002
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-SERVO
  primitive_refs:
  - PRIM-KUM-SERVO
- requirement_id: REQ-KUM-SERVO-003
  anchor_refs:
  - TOPIC-KUM-SERVO
  statement: Advertise hiding, animation scheduling, suspension, unload and restore only at the strength actually
    implemented by the pinned Servo adapter; engine API availability is not runtime proof.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-SERVO-003
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-SERVO
  primitive_refs:
  - PRIM-KUM-SERVO
- requirement_id: REQ-KUM-SERVO-004
  anchor_refs:
  - TOPIC-KUM-SERVO
  statement: Prove selected platform/media/site scenarios separately, including embedded video and a representative
    YouTube workflow when that compatibility is claimed; record native prerequisites and version.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-SERVO-004
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-SERVO
  primitive_refs:
  - PRIM-KUM-SERVO
- requirement_id: REQ-KUM-SERVO-005
  anchor_refs:
  - TOPIC-KUM-SERVO
  statement: Keep engine contexts and thread-affine resources in their owning adapter; route cross-window frame
    commands as messages rather than sharing unsupported engine handles across threads.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-SERVO-005
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-SERVO
  primitive_refs:
  - PRIM-KUM-SERVO
acceptance:
- acceptance_id: ACC-KUM-SERVO-001
  requirement_refs:
  - REQ-KUM-SERVO-001
  method: Build and run the future core sample with the browser feature disabled.
  expected_result: Ordinary frames/tabs work without Servo or its native dependencies.
- acceptance_id: ACC-KUM-SERVO-002
  requirement_refs:
  - REQ-KUM-SERVO-002
  method: Render two different-sized browser panes, resize one and inspect both.
  expected_result: Each pane retains its intended bounds and correct input coordinates, or the adapter rejects the
    unsupported configuration.
- acceptance_id: ACC-KUM-SERVO-003
  requirement_refs:
  - REQ-KUM-SERVO-003
  method: Exercise the pinned adapter lifecycle while recording callbacks and resource measurements.
  expected_result: Confirmed states correspond to observed behavior and limitations remain visible.
- acceptance_id: ACC-KUM-SERVO-004
  requirement_refs:
  - REQ-KUM-SERVO-004
  method: Run the exact future media/site scenario on the supported x64 runtime and document the build host.
  expected_result: Compatibility claims name the tested version/platform and failures remain scoped; untested scenarios
    are unverified.
- acceptance_id: ACC-KUM-SERVO-005
  requirement_refs:
  - REQ-KUM-SERVO-005
  method: Move browser content through a supported popout and inspect context/thread ownership.
  expected_result: Transfers follow the proven adapter path; unsupported moves preserve the original view with a
    reason.
features:
- feature_id: FEAT-KUM-SERVO
  title: Future Handshake browser and Servo adapter
  requirement_refs:
  - REQ-KUM-SERVO-001
  - REQ-KUM-SERVO-002
  - REQ-KUM-SERVO-003
  - REQ-KUM-SERVO-004
  - REQ-KUM-SERVO-005
  primitive_refs:
  - PRIM-KUM-SERVO
primitives:
- primitive_id: PRIM-KUM-SERVO
  title: Future Handshake browser and Servo adapter
  contract: Host-independent contract for future handshake browser and servo adapter; platform effects remain in
    adapters.
primitive_feature_relations:
- relation_id: REL-KUM-SERVO
  primitive_ref: PRIM-KUM-SERVO
  feature_ref: FEAT-KUM-SERVO
  relationship: implements
  constraint_refs: []
gui_design:
- design_id: GUI-KUM-SERVO
  applicability: planned
  visual_design_refs: []
  interaction_contract_refs:
  - PRIM-KUM-SERVO
  theme_and_component_refs: []
  surface_profile_refs: []
  acceptance_refs:
  - ACC-KUM-SERVO-001
  - ACC-KUM-SERVO-002
  - ACC-KUM-SERVO-003
  - ACC-KUM-SERVO-004
  - ACC-KUM-SERVO-005
decisions:
- decision_id: DEC-KUM-SERVO
  anchor_refs:
  - TOPIC-KUM-SERVO
  decision: Adopt the complete behavior declared in this topic; listed implementations are provenance, not dependencies
    or imported authority.
  rationale: Reuse established interaction and ownership patterns without binding Kumihimo to one UI toolkit.
  research_source_refs:
  - https://doc.servo.org/servo/trait.RenderingContext.html
  - https://doc.servo.org/servo/webview/struct.WebView.html
  - https://book.servo.org/embedding/overview.html
  - https://book.servo.org/embedding/lts-release.html
  supersedes: []
```

```yaml
kind: specification_topic
topic_id: TOPIC-KUM-RECOVERY
title: Saving layouts and recovering interrupted work
content_blocks:
- block_id: explanation
  kind: prose
  language: markdown
  requirement_refs: &id001
  - REQ-KUM-RECOVERY-001
  - REQ-KUM-RECOVERY-002
  - REQ-KUM-RECOVERY-003
  - REQ-KUM-RECOVERY-004
  - REQ-KUM-RECOVERY-005
- block_id: contract
  kind: structured_contract
  language: yaml
  requirement_refs: *id001
```

```markdown
## Reopen where you left off

Save arrangement, tab/group identity, user settings and host recovery references. The host saves document or browser data. If a monitor disappears, a popout needs a reachable location. If a plugin disappears, its tab needs a placeholder.

Validate saved data before applying it. Retain a last known valid layout and make migration explicit. A corrupt layout must not destroy the underlying content.
```

```yaml
requirements:
- requirement_id: REQ-KUM-RECOVERY-001
  anchor_refs:
  - TOPIC-KUM-RECOVERY
  statement: Provide a versioned serializable layout containing windows, pane tree, proportions, tab/group identities,
    labels, effective overrides and provider restore references; exclude engine handles and secrets.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-RECOVERY-001
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-RECOVERY
  primitive_refs:
  - PRIM-KUM-RECOVERY
- requirement_id: REQ-KUM-RECOVERY-002
  anchor_refs:
  - TOPIC-KUM-RECOVERY
  statement: Validate restored layout paths, IDs, cycles, proportions, counts and provider references before activation;
    bound input size and retain the last valid layout on rejection.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-RECOVERY-002
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-RECOVERY
  primitive_refs:
  - PRIM-KUM-RECOVERY
- requirement_id: REQ-KUM-RECOVERY-003
  anchor_refs:
  - TOPIC-KUM-RECOVERY
  statement: Restore offscreen or unavailable-monitor windows into reachable geometry and missing providers into
    placeholders; preserve original recovery references for later resolution.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-RECOVERY-003
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-RECOVERY
  primitive_refs:
  - PRIM-KUM-RECOVERY
- requirement_id: REQ-KUM-RECOVERY-004
  anchor_refs:
  - TOPIC-KUM-RECOVERY
  statement: Define schema migration and downgrade rejection explicitly; host persistence effects use atomic replacement
    or a recoverable save protocol with generation checks.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-RECOVERY-004
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-RECOVERY
  primitive_refs:
  - PRIM-KUM-RECOVERY
- requirement_id: REQ-KUM-RECOVERY-005
  anchor_refs:
  - TOPIC-KUM-RECOVERY
  statement: Record one canonical owner for each layout mutation and a stable operation result; on restart reconcile
    prepared/committed/unknown transfers before creating duplicate views.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-RECOVERY-005
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-RECOVERY
  primitive_refs:
  - PRIM-KUM-RECOVERY
acceptance:
- acceptance_id: ACC-KUM-RECOVERY-001
  requirement_refs:
  - REQ-KUM-RECOVERY-001
  method: Save a mixed workspace, inspect the record and restore it with the same providers.
  expected_result: Arrangement identities and supported settings return; live handles/secrets are absent.
- acceptance_id: ACC-KUM-RECOVERY-002
  requirement_refs:
  - REQ-KUM-RECOVERY-002
  method: Load malformed, oversized and cyclic layouts beside a valid saved layout.
  expected_result: Invalid data is rejected visibly without replacing valid state or disposing content.
- acceptance_id: ACC-KUM-RECOVERY-003
  requirement_refs:
  - REQ-KUM-RECOVERY-003
  method: Restore after removing a monitor and a provider.
  expected_result: Every view has a reachable frame or placeholder and can be recovered later.
- acceptance_id: ACC-KUM-RECOVERY-004
  requirement_refs:
  - REQ-KUM-RECOVERY-004
  method: Interrupt save and load an older plus an unsupported newer schema.
  expected_result: A complete valid prior or new layout remains recoverable; migrations and rejections are explained.
- acceptance_id: ACC-KUM-RECOVERY-005
  requirement_refs:
  - REQ-KUM-RECOVERY-005
  method: Restart during a popout handoff and compare provider/view ownership.
  expected_result: One canonical arrangement survives; uncertain work is reconciled and no content is silently lost.
features:
- feature_id: FEAT-KUM-RECOVERY
  title: Saving layouts and recovering interrupted work
  requirement_refs:
  - REQ-KUM-RECOVERY-001
  - REQ-KUM-RECOVERY-002
  - REQ-KUM-RECOVERY-003
  - REQ-KUM-RECOVERY-004
  - REQ-KUM-RECOVERY-005
  primitive_refs:
  - PRIM-KUM-RECOVERY
primitives:
- primitive_id: PRIM-KUM-RECOVERY
  title: Saving layouts and recovering interrupted work
  contract: Host-independent contract for saving layouts and recovering interrupted work; platform effects remain
    in adapters.
primitive_feature_relations:
- relation_id: REL-KUM-RECOVERY
  primitive_ref: PRIM-KUM-RECOVERY
  feature_ref: FEAT-KUM-RECOVERY
  relationship: implements
  constraint_refs: []
gui_design:
- design_id: GUI-KUM-RECOVERY
  applicability: headless; no independent GUI required
  visual_design_refs: []
  interaction_contract_refs:
  - PRIM-KUM-RECOVERY
  theme_and_component_refs: []
  surface_profile_refs: []
  acceptance_refs:
  - ACC-KUM-RECOVERY-001
  - ACC-KUM-RECOVERY-002
  - ACC-KUM-RECOVERY-003
  - ACC-KUM-RECOVERY-004
  - ACC-KUM-RECOVERY-005
decisions:
- decision_id: DEC-KUM-RECOVERY
  anchor_refs:
  - TOPIC-KUM-RECOVERY
  decision: Adopt the complete behavior declared in this topic; listed implementations are provenance, not dependencies
    or imported authority.
  rationale: Reuse established interaction and ownership patterns without binding Kumihimo to one UI toolkit.
  research_source_refs:
  - https://dockview.dev/docs/core/groups/popoutGroups/
  - https://raw.githubusercontent.com/zed-industries/zed/main/crates/workspace/src/pane_group.rs
  supersedes: []
```

```yaml
kind: specification_topic
topic_id: TOPIC-KUM-INPUT
title: Tablet input, keyboard and accessibility
content_blocks:
- block_id: explanation
  kind: prose
  language: markdown
  requirement_refs: &id001
  - REQ-KUM-INPUT-001
  - REQ-KUM-INPUT-002
  - REQ-KUM-INPUT-003
  - REQ-KUM-INPUT-004
- block_id: contract
  kind: structured_contract
  language: yaml
  requirement_refs: *id001
```

```markdown
## Comfortable on the ARM tablet

Docking, resizing and selecting tabs need usable touch and pen targets, plus keyboard equivalents. Dragging must be distinguishable from scrolling. A disabled cross-window gesture should have a command alternative.

Use accessible names, focus order and announcements for layout changes. Hidden bars, collapsed groups and popouts must leave a route back to the content.
```

```yaml
requirements:
- requirement_id: REQ-KUM-INPUT-001
  anchor_refs:
  - TOPIC-KUM-INPUT
  statement: Provide configurable control density, resize/drag hit areas and touch/pen gesture thresholds; distinguish
    scroll, long-press drag and cancellation.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-INPUT-001
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-INPUT
  primitive_refs:
  - PRIM-KUM-INPUT
- requirement_id: REQ-KUM-INPUT-002
  anchor_refs:
  - TOPIC-KUM-INPUT
  statement: Expose keyboard commands for focus movement, tab/group selection, resize, dock, float, popout, close
    and reveal-controls where supported; unsupported gestures retain a discoverable command path.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-INPUT-002
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-INPUT
  primitive_refs:
  - PRIM-KUM-INPUT
- requirement_id: REQ-KUM-INPUT-003
  anchor_refs:
  - TOPIC-KUM-INPUT
  statement: Provide accessible tab/group/frame roles, names, selection state, focus indicators, close/resize actions
    and layout-change announcements; preserve a coherent focus route across windows.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-INPUT-003
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-INPUT
  primitive_refs:
  - PRIM-KUM-INPUT
- requirement_id: REQ-KUM-INPUT-004
  anchor_refs:
  - TOPIC-KUM-INPUT
  statement: Let the host define shortcut conflicts and route wheel/gesture/key input to the correct owner; frame
    operations shall not hijack content typing.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-INPUT-004
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-INPUT
  primitive_refs:
  - PRIM-KUM-INPUT
acceptance:
- acceptance_id: ACC-KUM-INPUT-001
  requirement_refs:
  - REQ-KUM-INPUT-001
  method: Perform tab selection, scroll and resize with mouse, touch and pen.
  expected_result: Intended gestures have predictable outcomes and cancellation preserves the prior layout.
- acceptance_id: ACC-KUM-INPUT-002
  requirement_refs:
  - REQ-KUM-INPUT-002
  method: Operate the workspace without dragging and inspect disabled capability reasons.
  expected_result: All supported arrangement actions are reachable; unsupported actions explain why.
- acceptance_id: ACC-KUM-INPUT-003
  requirement_refs:
  - REQ-KUM-INPUT-003
  method: Inspect the actual adapter accessibility tree and keyboard flow after docking/popout.
  expected_result: Names/state match visible content and focus does not become trapped or lost.
- acceptance_id: ACC-KUM-INPUT-004
  requirement_refs:
  - REQ-KUM-INPUT-004
  method: Use an editor shortcut that conflicts with a frame shortcut.
  expected_result: Configured precedence is observable and ordinary typing reaches the active editor.
features:
- feature_id: FEAT-KUM-INPUT
  title: Tablet input, keyboard and accessibility
  requirement_refs:
  - REQ-KUM-INPUT-001
  - REQ-KUM-INPUT-002
  - REQ-KUM-INPUT-003
  - REQ-KUM-INPUT-004
  primitive_refs:
  - PRIM-KUM-INPUT
primitives:
- primitive_id: PRIM-KUM-INPUT
  title: Tablet input, keyboard and accessibility
  contract: Host-independent contract for tablet input, keyboard and accessibility; platform effects remain in adapters.
primitive_feature_relations:
- relation_id: REL-KUM-INPUT
  primitive_ref: PRIM-KUM-INPUT
  feature_ref: FEAT-KUM-INPUT
  relationship: implements
  constraint_refs: []
gui_design:
- design_id: GUI-KUM-INPUT
  applicability: planned
  visual_design_refs: []
  interaction_contract_refs:
  - PRIM-KUM-INPUT
  theme_and_component_refs: []
  surface_profile_refs: []
  acceptance_refs:
  - ACC-KUM-INPUT-001
  - ACC-KUM-INPUT-002
  - ACC-KUM-INPUT-003
  - ACC-KUM-INPUT-004
decisions:
- decision_id: DEC-KUM-INPUT
  anchor_refs:
  - TOPIC-KUM-INPUT
  decision: Adopt the complete behavior declared in this topic; listed implementations are provenance, not dependencies
    or imported authority.
  rationale: Reuse established interaction and ownership patterns without binding Kumihimo to one UI toolkit.
  research_source_refs:
  - https://dockview.dev/docs/core/dnd/strategy/
  - https://dockview.dev/docs/advanced/accessibility/
  - https://code.visualstudio.com/docs/configure/custom-layout
  supersedes: []
```

```yaml
kind: specification_topic
topic_id: TOPIC-KUM-MODEL
title: Model operation, diagnostics and the built-in manual
content_blocks:
- block_id: explanation
  kind: prose
  language: markdown
  requirement_refs: &id001
  - REQ-KUM-MODEL-001
  - REQ-KUM-MODEL-002
  - REQ-KUM-MODEL-003
  - REQ-KUM-MODEL-004
  - REQ-KUM-MODEL-005
- block_id: contract
  kind: structured_contract
  language: yaml
  requirement_refs: *id001
```

```markdown
## A fresh model can operate it

Provide the same arrangement actions through structured commands as through the GUI. A snapshot exposes IDs, geometry, tabs, focus, settings and lifecycle state. Logs explain what happened without requiring screen-reading guesses.

Parallel models may work on different content, while one canonical controller serializes conflicting layout writes. Automated operation stays quiet and uses an explicit controlled view for visual checks.
```

```yaml
requirements:
- requirement_id: REQ-KUM-MODEL-001
  anchor_refs:
  - TOPIC-KUM-MODEL
  statement: Expose versioned structured snapshots and commands with stable target IDs, operation IDs, actor attribution,
    expected generation and explicit success/rejection/pending/unknown outcomes.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-MODEL-001
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-MODEL
  primitive_refs:
  - PRIM-KUM-MODEL
- requirement_id: REQ-KUM-MODEL-002
  anchor_refs:
  - TOPIC-KUM-MODEL
  statement: Permit parallel host/content work while serializing conflicting layout mutations; define cancellation,
    bounded work and recovery without requiring exclusive ownership of the whole application.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-MODEL-002
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-MODEL
  primitive_refs:
  - PRIM-KUM-MODEL
- requirement_id: REQ-KUM-MODEL-003
  anchor_refs:
  - TOPIC-KUM-MODEL
  statement: Provide structured diagnostics, bounded logs/traces, capability health and visual snapshots for implemented
    GUI behavior; include reproduction steps and keep host secrets/content private by default.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-MODEL-003
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-MODEL
  primitive_refs:
  - PRIM-KUM-MODEL
- requirement_id: REQ-KUM-MODEL-004
  anchor_refs:
  - TOPIC-KUM-MODEL
  statement: Ship a built-in model manual describing purpose, startup, inputs/outputs, navigation, commands, safety
    constraints, common failures and recovery for the implemented adapter.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-MODEL-004
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-MODEL
  primitive_refs:
  - PRIM-KUM-MODEL
- requirement_id: REQ-KUM-MODEL-005
  anchor_refs:
  - TOPIC-KUM-MODEL
  statement: Keep automated work in background or an explicitly controlled test surface; do not create foreground
    windows, steal input or focus unless the operator deliberately requests that interaction.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-MODEL-005
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-MODEL
  primitive_refs:
  - PRIM-KUM-MODEL
acceptance:
- acceptance_id: ACC-KUM-MODEL-001
  requirement_refs:
  - REQ-KUM-MODEL-001
  method: Run a valid command, a stale-generation command and a repeated operation ID.
  expected_result: Results are attributable, deduplicated or reconciled and stale writes cannot overwrite newer
    layout state.
- acceptance_id: ACC-KUM-MODEL-002
  requirement_refs:
  - REQ-KUM-MODEL-002
  method: Issue independent content operations and two conflicting layout commands concurrently.
  expected_result: Independent work proceeds; layout conflicts resolve through explicit generation/ownership rules.
- acceptance_id: ACC-KUM-MODEL-003
  requirement_refs:
  - REQ-KUM-MODEL-003
  method: Reproduce a failed docking/resize scenario using the diagnostic surface.
  expected_result: A fresh model can identify the relevant IDs, outcome and actual visual state without private
    content dumps.
- acceptance_id: ACC-KUM-MODEL-004
  requirement_refs:
  - REQ-KUM-MODEL-004
  method: Give the manual and clean runtime to a model with no chat history.
  expected_result: It can start and operate the supported sample and recover a documented failure from project-local
    information.
- acceptance_id: ACC-KUM-MODEL-005
  requirement_refs:
  - REQ-KUM-MODEL-005
  method: Run structured and visual diagnostics while another application has focus.
  expected_result: Ordinary automation is quiet; any controlled foreground path is declared before use.
features:
- feature_id: FEAT-KUM-MODEL
  title: Model operation, diagnostics and the built-in manual
  requirement_refs:
  - REQ-KUM-MODEL-001
  - REQ-KUM-MODEL-002
  - REQ-KUM-MODEL-003
  - REQ-KUM-MODEL-004
  - REQ-KUM-MODEL-005
  primitive_refs:
  - PRIM-KUM-MODEL
primitives:
- primitive_id: PRIM-KUM-MODEL
  title: Model operation, diagnostics and the built-in manual
  contract: Host-independent contract for model operation, diagnostics and the built-in manual; platform effects
    remain in adapters.
primitive_feature_relations:
- relation_id: REL-KUM-MODEL
  primitive_ref: PRIM-KUM-MODEL
  feature_ref: FEAT-KUM-MODEL
  relationship: implements
  constraint_refs: []
gui_design:
- design_id: GUI-KUM-MODEL
  applicability: headless; no independent GUI required
  visual_design_refs: []
  interaction_contract_refs:
  - PRIM-KUM-MODEL
  theme_and_component_refs: []
  surface_profile_refs: []
  acceptance_refs:
  - ACC-KUM-MODEL-001
  - ACC-KUM-MODEL-002
  - ACC-KUM-MODEL-003
  - ACC-KUM-MODEL-004
  - ACC-KUM-MODEL-005
decisions:
- decision_id: DEC-KUM-MODEL
  anchor_refs:
  - TOPIC-KUM-MODEL
  decision: Adopt the complete behavior declared in this topic; listed implementations are provenance, not dependencies
    or imported authority.
  rationale: Reuse established interaction and ownership patterns without binding Kumihimo to one UI toolkit.
  research_source_refs:
  - https://dockview.dev/docs/core/overview/
  - https://github.com/ocornut/imgui/blob/master/docs/BACKENDS.md
  supersedes: []
```

```yaml
kind: specification_topic
topic_id: TOPIC-KUM-PLATFORMS
title: Desktop, web and ARM development
content_blocks:
- block_id: explanation
  kind: prose
  language: markdown
  requirement_refs: &id001
  - REQ-KUM-PLATFORMS-001
  - REQ-KUM-PLATFORMS-002
  - REQ-KUM-PLATFORMS-003
  - REQ-KUM-PLATFORMS-004
- block_id: contract
  kind: structured_contract
  language: yaml
  requirement_refs: *id001
```

```markdown
## One behavior model, proven platform adapters

Keep layout and lifecycle logic portable Rust. Windows x64 consumption is the first desktop target; the Windows ARM tablet is a development host. Cross-compiling Rust and successfully building all native dependencies are separate proofs.

A web adapter must reproduce supported behavior through browser capabilities. It may need different popout, GPU, focus and lifecycle handling. Other operating systems are possible adapter targets; support is earned by actually running them.
```

```yaml
requirements:
- requirement_id: REQ-KUM-PLATFORMS-001
  anchor_refs:
  - TOPIC-KUM-PLATFORMS
  statement: Keep portable arrangement/state logic separate from native-window and browser bindings; make dependencies/features
    independently selectable and record platform-specific capability limits.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-PLATFORMS-001
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-PLATFORMS
  primitive_refs:
  - PRIM-KUM-PLATFORMS
- requirement_id: REQ-KUM-PLATFORMS-002
  anchor_refs:
  - TOPIC-KUM-PLATFORMS
  statement: Record build-host architecture and runtime-target architecture separately; prove the Windows x64 artifact
    and document ARM-host native prerequisites before claiming tablet build compatibility.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-PLATFORMS-002
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-PLATFORMS
  primitive_refs:
  - PRIM-KUM-PLATFORMS
- requirement_id: REQ-KUM-PLATFORMS-003
  anchor_refs:
  - TOPIC-KUM-PLATFORMS
  statement: Prove web layout, input, persistence and any popout/media integration separately from desktop; browser
    popup, origin, GPU and lifecycle restrictions participate in capability admission.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-PLATFORMS-003
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-PLATFORMS
  primitive_refs:
  - PRIM-KUM-PLATFORMS
- requirement_id: REQ-KUM-PLATFORMS-004
  anchor_refs:
  - TOPIC-KUM-PLATFORMS
  statement: Expose adapter versions and a support matrix distinguishing planned, implemented and verified capabilities;
    additional operating systems require their own focused conformance evidence.
  rationale: Provide reusable host behavior with explicit ownership and recoverable failure.
  acceptance_refs:
  - ACC-KUM-PLATFORMS-004
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-KUM-PLATFORMS
  primitive_refs:
  - PRIM-KUM-PLATFORMS
acceptance:
- acceptance_id: ACC-KUM-PLATFORMS-001
  requirement_refs:
  - REQ-KUM-PLATFORMS-001
  method: Inspect future package dependencies and run core behavior with each implemented adapter.
  expected_result: Core semantics remain consistent; unsupported platform behavior is explicit.
- acceptance_id: ACC-KUM-PLATFORMS-002
  requirement_refs:
  - REQ-KUM-PLATFORMS-002
  method: Build on the chosen Windows ARM host and run the produced artifact on the stated x64 target.
  expected_result: The evidence identifies both environments and all native tool/dependency requirements; failures
    do not become compatibility claims.
- acceptance_id: ACC-KUM-PLATFORMS-003
  requirement_refs:
  - REQ-KUM-PLATFORMS-003
  method: Run the future web adapter in a real supported browser with restricted popup settings.
  expected_result: Supported behavior works; blocked actions preserve state and explain the browser restriction.
- acceptance_id: ACC-KUM-PLATFORMS-004
  requirement_refs:
  - REQ-KUM-PLATFORMS-004
  method: Inspect a matrix with an untested adapter and an actual proven feature.
  expected_result: Untested entries remain unverified; evidence is scoped to exact adapter/version/platform.
features:
- feature_id: FEAT-KUM-PLATFORMS
  title: Desktop, web and ARM development
  requirement_refs:
  - REQ-KUM-PLATFORMS-001
  - REQ-KUM-PLATFORMS-002
  - REQ-KUM-PLATFORMS-003
  - REQ-KUM-PLATFORMS-004
  primitive_refs:
  - PRIM-KUM-PLATFORMS
primitives:
- primitive_id: PRIM-KUM-PLATFORMS
  title: Desktop, web and ARM development
  contract: Host-independent contract for desktop, web and arm development; platform effects remain in adapters.
primitive_feature_relations:
- relation_id: REL-KUM-PLATFORMS
  primitive_ref: PRIM-KUM-PLATFORMS
  feature_ref: FEAT-KUM-PLATFORMS
  relationship: implements
  constraint_refs: []
gui_design:
- design_id: GUI-KUM-PLATFORMS
  applicability: planned
  visual_design_refs: []
  interaction_contract_refs:
  - PRIM-KUM-PLATFORMS
  theme_and_component_refs: []
  surface_profile_refs: []
  acceptance_refs:
  - ACC-KUM-PLATFORMS-001
  - ACC-KUM-PLATFORMS-002
  - ACC-KUM-PLATFORMS-003
  - ACC-KUM-PLATFORMS-004
decisions:
- decision_id: DEC-KUM-PLATFORMS
  anchor_refs:
  - TOPIC-KUM-PLATFORMS
  decision: Adopt the complete behavior declared in this topic; listed implementations are provenance, not dependencies
    or imported authority.
  rationale: Reuse established interaction and ownership patterns without binding Kumihimo to one UI toolkit.
  research_source_refs:
  - https://doc.rust-lang.org/rustc/platform-support/windows-msvc.html
  - https://learn.microsoft.com/en-us/visualstudio/install/visual-studio-on-arm-devices?view=visualstudio
  supersedes: []
```

```yaml
kind: specification_topic
topic_id: TOPIC-KUM-CHOICES
title: Implementation choices and research proposals
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
## Decisions still open

The accepted direction has three logical submodules: portable core behavior, optional replaceable GUI adapters, and host-content integration contracts/adapters. Actual content engines remain host-owned. Rust code stays modular in small cohesive packages; bundle only responsibilities that belong together. The number of logical submodules does not prescribe a crate count. No UI or graphics library is selected by this specification revision.

Compare egui_tiles for flexible grids, egui_dock for docking/tab integration, and a native adapter using winit with an appropriate renderer. Dockview is a web candidate and a reference for panel identities, groups and handoff recovery. Its commercial additions must be distinguished from its MIT core. GPUI and Dear ImGui provide architectural precedents rather than automatic dependencies.

Library selection needs a small real sample of the affected boundary: variable counts and recursive window containment, grouping, custom controls, touch, save/restore and the chosen rendering handoff. Native popouts require their own proof; an in-app floating egui window is not that proof.

Optional research proposals include scrolling workspaces inspired by Niri, a zoomed workspace overview, auto-hide edge trays, layout undo, saved named arrangements and snapping hysteresis. They are not accepted features or required gates in this revision. Preserve them in the research note for later operator decisions.

The first implemented packet is one small portable core package for typed identities, in-memory recursive containment, content/view references and deterministic validated operations. The first packet avoids GUI/browser/rendering dependencies and native effects. Geometry and tool docking now share this core; persistent snapshots and concrete adapters remain subsequent bounded packets. Flat canonical records and iterative ancestry checks avoid making recursive model depth depend on the call stack. No toolkit is selected by this choice.

The verified core uses flat typed records, frame-owned child workspaces and distinct view/resource identities. Practical admission defaults to 16,384 total records, with an explicit host-selected unrestricted policy and optional depth/per-frame view limits. Lowering limits preserves existing records and permits recovery moves. Record counts are not byte or live-engine memory ceilings. Complete-envelope replay is retained within a configured receipt count; expired operation sequences reject rather than repeat effects. This synchronous in-memory boundary provides no file recovery or cross-restart deduplication. Windows x64 MSVC with Rust 1.91.1 is the observed proof; ARM64 and WASM execution remain unverified.

Geometry/tool work is authorized inside cohesive private modules of the same std-only core package; a pure solver owns no second arrangement, generation or ledger. General CSS/intrinsic layout and a toolkit dependency are not required for this bounded scope. The public extension is explicitly unreleased package 0.2 and Snapshot version 2; file restore/migration remains future work. Tools count toward aggregate entity admission. A separate aggregate layout-entry budget counts nodes, tracks, child references and retained geometry/policy entries, defaults to 65,536 and is host-configurable or explicitly unrestricted. Lowering budgets preserves existing data, rejects growth and permits unchanged/decreasing recovery; neither entity nor entry counts claim a byte/memory ceiling. This bounded package extension passed the all-targets check, one private exhaustion test, nine ownership tests, twelve layout/tool tests and the console example at source revision 90f93239d2e474e28451cab15458454ecd1649dc using Rust 1.91.1 on Windows x64 MSVC. The float-return case selects a currently validated eligible return location and rejects an impossible return before changing canonical state. These functional results do not prove GUI interaction, native popouts, persistent recovery, performance, ARM64 or WASM execution.

The next bounded packet retains the complete tab/group/pin/overflow and semantic recovery scope, plus all four equalize/swap/maximize/restore operations. Its selected implementation is unreleased Core0.3/Snapshot3 with default std-only behavior, optional Serde1.0.229 on saved semantic types and a separately selectable small kumihimo-layout-json package using serde_json1.0.151. Actual JSON encode/decode is bounded (default8MiB, configurable), rejects duplicate members before normalization, consumes complete input and retains recursion guards. A flat first durable schema1 preserves exact typed IDs/u64 values as decimal strings, validated finite geometry and semantic records; Snapshot1/2 were never prior file formats, so unsupported older/newer schemas reject explicitly rather than receive invented migrations. These are selected next-packet choices, not executed proof.

Saved layout carries nested ownership, open/closed views, group labels/colors/collapse, selection/pins, packing/proportions, semantic settings/tool binding/placement/policies, floats/max projection, stable host provider/resource keys and opaque recovery references. Generated ProviderId/ResourceId remain semantic IDs inside saved lineage; actual engines/handles/content/secrets and volatile focus/visibility/usability/close-permission assertions are excluded. Saved next_entity and source lineage/incarnation/generation are provenance and identity admission metadata, never imported live replay state. Host resolution and reachable in-app bounds are explicit preparation inputs; missing/incompatible providers retain labeled placeholders and original references. Native monitor/popout effects remain adapter proof.

Every Core receives immutable bounded host-supplied lineage/incarnation; the host guarantees a fresh incarnation for each fresh/restored Core, while the asset only detects mismatches. Commands and dock/restore/save tokens bind identity before ledger access. Same-lineage live restore uses a dedicated detached whole-state validator, skips ordinary movement repair, retains current ledger/incarnation/high-water and advances live generation once; allocator stays at max(live,saved) above all generated active/closed IDs. Extant immutable entity identities must match; host lineage integrity and allocator floors do not prove identity for previously omitted historical records. Fresh restore retains saved IDs/lineage with a new host incarnation, generation1 and fresh ledger, rejecting old matching-number commands before bookkeeping. Saved source generation is provenance only; retained same-incarnation receipt replay is a historical acknowledgment, not proof of current membership, and no cross-restart exactly-once guarantee follows. Restore preparation binds all DTO/policy/provider/bounds inputs, and failed activation preserves model/counters subject only to normal same-incarnation rejection receipts.

Close/reopen keeps bounded closed View tombstones (default1024, host-configurable/unrestricted) and original resource/group/pin/frame/index references, changes only open membership and deterministic selection, and invokes no engine. Host close permission/veto and unsaved indicators stay separate; dirty content is never implicit approval, retention never silently evicts. Reopen preserves identity, clamps an eligible saved index, rechecks pins/budgets and requests fresh volatile host confirmation. Selected close chooses the first remaining tab or none; unselected close retains selection. Sharing admission counts open placements, so closed history is not another open view. Persistent identity tokens are bounded to128 UTF-8 bytes, durable provider/resource keys to256, and label/recovery-reference strings to65536 by default; host-configurable persistence string bounds reject rather than truncate. Closed references cannot be made usable by true host flags: one snapshot-aware tool presentation path governs measurement/reservation/output. Origin/target remain pinned, closed compatible references remain valid, rebinding to closed/unavailable targets rejects, and a closed view anchor is dormant with no active interior; frame/root/cell HiddenSpace policy survives.

Groups retain independent identity/label/color/collapse, move atomically in source canonical member order with after-removal tab insertion indexes, keep closed-member reopen metadata and obey every member pin/budget. Host pin rules explicitly control close/reorder/cross-frame moves through all routes. A canonical paged search/list projection (default256 rows, configurable) exposes open/closed totals, labels/active/unsaved/placeholder state and deterministic ordering without shrinking/dropping records or creating engines. Visible list virtualization and actual live-engine budgets remain separate adapter/lifecycle proof.

Equalize resets selected workspace weights to1 while minima still apply; swap affects only two docked frame leaves in one workspace. Maximize is a persisted reversible projection over the retained underlying plan and selects one docked frame with its/root-edge tools, hiding sibling/floating/cell surfaces without deleting identities or imposing hidden minima on active projection. Structural validation remains complete. A shared source/destination guard rejects affected ownership/packing/dock/cell changes, including indirect preview commits/child-workspace attachment, until restore; tab/group/geometry/tool-state policies and independent scopes remain usable. Restore-layout clears projection only if full underlying geometry/bounds solve, otherwise retains it with explicit error. Scrollable can resolve packed minima but does not promise to repair out-of-bounds floats.

Saving exposes immutable identity/generation-stamped semantic data/bytes and a current-stamp check. The host protocol serializes writers, validates before publication, rejects stale writes and atomically replaces a complete valid blob or retains recoverable prior/new blobs. The packet proves wire/semantic/stamp behavior; actual filesystem replacement, interrupted-save recovery, monitor effects and content recovery remain host-adapter acceptance. Complete RECOVERY acceptance stays open until those boundaries are proven.

```

```yaml
decisions:
- decision_id: DEC-KUM-ARCHITECTURE
  anchor_refs:
  - TOPIC-KUM-CHOICES
  decision: 'Adopt three logical submodules: portable core behavior, optional replaceable GUI adapters and host-content
    integration contracts/adapters; organize them as small cohesive Rust modules/packages with independently selectable
    backend integrations, explicit dependency direction and host-owned content engines, retaining separate platform,
    content, rendering and persistence boundaries. No fixed overall crate count is prescribed; Core and optional bounded JSON dependencies are selected for the next packet above, while further adapters remain separately selectable.'
  rationale: Preserve reusable desktop/web behavior and keep expensive browser/rendering integrations independently
    selectable.
  research_source_refs:
  - https://github.com/rerun-io/egui_tiles
  - https://github.com/anhosh/egui_dock
  - https://dockview.dev/docs/core/overview/
  - https://github.com/ocornut/imgui/blob/master/docs/BACKENDS.md
  - https://github.com/DioxusLabs/taffy
  - https://doc.rust-lang.org/cargo/reference/features.html
  supersedes: []
- decision_id: DEC-KUM-OPTIONAL-IDEAS
  anchor_refs:
  - TOPIC-KUM-CHOICES
  decision: Keep field-inspired optional ideas and toolkit choice as non-authoritative research proposals until
    a later accepted specification change; they create no mandatory implementation scope.
  rationale: Keep the first asset useful and preserve creative exploration without turning every idea into a build
    gate.
  research_source_refs:
  - https://niri-wm.github.io/niri/Overview.html
  - https://dockview.dev/docs/overview/licence/
  supersedes: []
```
