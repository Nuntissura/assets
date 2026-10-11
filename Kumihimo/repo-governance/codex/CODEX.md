# Kumihimo model instructions

Generated from `current/codex-source.yaml`; edit the source and run `python repo-governance/repo-scripts/sync.py`.

Read the taskboard, selected WP contract/state, relevant spec topics and affected build rules. The selected minimal profile controls execution; reference templates introduce no extra gates.

Build rules: `build-rules/current/build-rules.yaml`. Workflow: `workflows/repository-workflows.md`.

[KUM-AUTH-001] Use the selected model-operated minimal profile; stock templates are reference inputs and add no gates.

[KUM-WORK-001] Create a bounded WP with scope, files, dependencies, acceptance and focused proof before execution; mark it active in state. No separate refinement/enrichment phase is required.

[KUM-WORK-002] Use this checkout by default; microtasks, worktrees, leases, receipts and independent review are optional unless the WP explicitly requires them.

[KUM-STATE-001] Persist WP phase, owner, changed files, observed proof, blockers and next action at meaningful milestones; update the taskboard from WP state.

[KUM-RECOVER-001] On resume inspect WP state and current Git diff; reconcile uncertain commits, pushes and external outcomes before repeating effects.

[KUM-SPEC-001] Keep accepted meaning in the current self-contained specification; preserve IDs and snapshot prior revisions. Label proposals and unverified capabilities honestly.

[KUM-SCOPE-001] Execute only the active WP scope; serialize overlapping writes and keep sibling assets untouched.

[KUM-PROOF-001] Use the shortest affected proof that supports the claim; no long checks, ceremonial gate chains or stock compliance audits by default.

[KUM-PORTABLE-001] Use repository-relative paths and ignored local configuration; never hardcode machine paths into project artifacts.

[KUM-PROCESS-001] Do not stop processes this session did not start without the operator process-stop acknowledgment required by global authority.
