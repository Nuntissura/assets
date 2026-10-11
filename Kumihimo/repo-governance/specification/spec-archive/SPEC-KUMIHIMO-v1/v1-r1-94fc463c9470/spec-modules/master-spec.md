---
schema_version: handshake.spec-document.v1
specification_id: SPEC-KUMIHIMO-v1
document_id: DOC-KUM-MASTER-v1
revision: 1
title: Kumihimo master specification
updated_at: '2026-10-11'
---

```yaml
kind: specification_topic
topic_id: TOPIC-KUM-WORKSPACE
title: Kumihimo baseline
content_blocks:
- block_id: explanation
  kind: prose
  language: markdown
  requirement_refs: []
- block_id: contract
  kind: structured_contract
  language: yaml
  requirement_refs: []
```

```markdown
# Kumihimo

Kumihimo holds and arranges content. The host supplies editors, browsers, media and graphics.

This initial specification preserves the requested direction. No product implementation or runtime compatibility is proven. The next WP expands these topics from the researched field.
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
  statement: Support an eight-pane resizable grid with multiple tabs per pane, grouped tabs, color labels, docking,
    floating panes and capability-checked native popouts.
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
  method: Focused feature scenario when implemented; specification structure only during initialization.
  expected_result: Support an eight-pane resizable grid with multiple tabs per pane, grouped tabs, color labels,
    docking, floating panes and capability-checked native popouts.
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
  - Eight-pane tabbed workspaces with flexible content
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
