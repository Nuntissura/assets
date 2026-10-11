---
schema_version: handshake.spec-document.v1
specification_id: SPEC-KUMIHIMO-v1
document_id: DOC-KUM-ROADMAP-v1
revision: 1
title: Kumihimo roadmap
updated_at: '2026-10-11'
---

```yaml
kind: specification_topic
topic_id: TOPIC-KUM-ROADMAP
title: Roadmap
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
# Planned

All product behavior is planned. Initialization and specification work do not implement the asset.
```

```yaml
roadmap:
- item_ref: FEAT-KUM-WORKSPACE
  source_topic_refs:
  - TOPIC-KUM-WORKSPACE
  overview_summary: Reusable tabbed, dockable desktop/web workspace.
  implementation_status: planned
  implemented_requirement_refs: []
  remaining_requirement_refs:
  - REQ-KUM-WORKSPACE-001
  - REQ-KUM-WORKSPACE-002
  - REQ-KUM-WORKSPACE-003
  - REQ-KUM-WORKSPACE-004
  - REQ-KUM-WORKSPACE-005
  - REQ-KUM-WORKSPACE-006
  implementation_source_revision: null
  verification_summary: No implementation or runtime proof yet.
  matrix_entry_refs: []
```
