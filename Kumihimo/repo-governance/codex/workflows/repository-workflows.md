# Repository workflows

Generated from `repo-governance/codex/current/codex-source.yaml`. Paths below are repository-relative. Model-operated execution uses the selected mode contract; Handshake runtime readiness is separate.

## KUM-FLOW-001 — Select and execute a bounded WP

### Required reads

- taskboard
- WP contract/state
- relevant spec topics

### Actions

- Write the direct contract; mark active; execute in this checkout
- Run affected focused proof; record observed results; sync status

### Proof

- Accepted WP outcome is observed; no self-authored status substitutes for proof

## KUM-FLOW-002 — Recover and publish

### Required reads

- WP state
- Git status/diff

### Actions

- Reconcile incomplete effects before retrying
- Commit only owned paths; push when authorized; verify the exact remote revision

### Proof

- Current state and independent remote read agree with reported outcome
