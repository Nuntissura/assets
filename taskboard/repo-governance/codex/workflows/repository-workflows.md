---
file_id: TASKBOARD-WORKFLOWS-v1
file_kind: workflow-navigation
updated_at: 2026-10-11
---

<topic id="module-alignment">

Read the scoped authority, self-contained spec, module contracts and adoption limits. Preserve public files.
Run `python -B repo-governance/repo-scripts/check-alignment.py` from taskboard after governance changes.
The checker resolves the installed hsrepotemplate skill from `--skill-root`, `HSRT_SKILL_ROOT`, or the
standard Codex skills directory. Python with PyYAML is required. Missing tools fail explicitly.
Use `--preservation` only to verify the original alignment snapshot; intentional later product edits
must be reconciled with the consumer's source/artifact pins before being accepted.

</topic>

<topic id="future-work">

Canonical workflow rules are `../current/codex-source.yaml`. New work uses the pinned refinement,
WP/MT and state owners; registry entries are planning-only until authorized activation resolves.
No module worktree, controller receipt or executable microtask is created by this alignment.
Read `.assets` Git state through the actual parent root and preserve unrelated uncommitted work.
Use the existing domain proof for Rust changes and consumer-owned diagnostics for browser/host changes.

</topic>
