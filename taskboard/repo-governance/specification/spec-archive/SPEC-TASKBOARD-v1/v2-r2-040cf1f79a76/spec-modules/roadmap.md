---
schema_version: handshake.spec-document.v1
specification_id: SPEC-TASKBOARD-v1
document_id: DOC-TASKBOARD-ROADMAP-v1
revision: 1
title: Taskboard verification roadmap
updated_at: '2026-10-11'
provenance:
- README.md
- src/lib.rs
- web/Taskboard.tsx
---

```yaml
kind: specification_topic
topic_id: TOPIC-TASKBOARD-ROADMAP
title: Existing scope awaiting verified acceptance
content_blocks:
- block_id: overview
  kind: prose
  language: markdown
  requirement_refs: []
- block_id: roadmap
  kind: structured_contract
  language: yaml
  requirement_refs: []
```

```markdown
# Verification roadmap

Existing implementation is present. All rows remain planned pending acceptance proof; this bundle activates no work and makes no whole-feature implementation claim. There are no verified entries for an Implemented section.
```

```yaml
roadmap:
- item_ref: FEAT-TASKBOARD-DOMAIN
  source_topic_refs:
  - TOPIC-TASKBOARD-CONTRACT
  overview_summary: Deterministic domain rules
  implementation_status: planned
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-TASKBOARD-TITLE
  - REQ-TASKBOARD-BODY-STATUS
  - REQ-TASKBOARD-REVISION
  - REQ-TASKBOARD-LEGACY
  implementation_source_revision: null
  verification_summary: Existing source inspected; no fresh runtime acceptance or independent completion
    proof was performed for this alignment. Planned denotes unverified scope, not absence of existing
    code.
  matrix_entry_refs: []
- item_ref: FEAT-TASKBOARD-BOARD-UI
  source_topic_refs:
  - TOPIC-TASKBOARD-CONTRACT
  overview_summary: Host-composed board UI
  implementation_status: planned
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-TASKBOARD-COMPOSE
  - REQ-TASKBOARD-VIEW
  - REQ-TASKBOARD-EDIT
  implementation_source_revision: null
  verification_summary: Existing source inspected; no fresh runtime acceptance or independent completion
    proof was performed for this alignment. Planned denotes unverified scope, not absence of existing
    code.
  matrix_entry_refs: []
- item_ref: FEAT-TASKBOARD-RECOVERY
  source_topic_refs:
  - TOPIC-TASKBOARD-CONTRACT
  overview_summary: Scoped mutation and retry recovery
  implementation_status: planned
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-TASKBOARD-SCOPE
  - REQ-TASKBOARD-RETRY
  implementation_source_revision: null
  verification_summary: Existing source inspected; no fresh runtime acceptance or independent completion
    proof was performed for this alignment. Planned denotes unverified scope, not absence of existing
    code.
  matrix_entry_refs: []
- item_ref: FEAT-TASKBOARD-HOST
  source_topic_refs:
  - TOPIC-TASKBOARD-CONTRACT
  overview_summary: Host ownership and compatibility
  implementation_status: planned
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-TASKBOARD-HOST
  - REQ-TASKBOARD-PACKAGE
  implementation_source_revision: null
  verification_summary: Existing source inspected; no fresh runtime acceptance or independent completion
    proof was performed for this alignment. Planned denotes unverified scope, not absence of existing
    code.
  matrix_entry_refs: []
```

