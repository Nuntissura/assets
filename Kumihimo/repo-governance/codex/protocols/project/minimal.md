# Repository operation

Projection of `repo-governance/codex/current/codex-source.yaml`; selected minimal model-operated contract.

```yaml
role_authorization:
  maintainer: Owns bounded product and governance work; no compulsory role handoffs
  dev_lead: Owns session, task state, scope admission and publication
  coder: Owns admitted product paths; fixed model and rotation policy
  dependencies: Owns admitted dependency-readiness report
  diagnostics: Owns dispatched diagnostics and failure investigation
  evaluation: Owns focused per-WP evaluation
transition_registry:
  phases:
  - planned
  - active
  - blocked
  - done
  - cancelled
  done_requires: WP acceptance evidence; pushed containment when publication is required
operating_protocol:
  start: Run the session-start command; inspect session roster, taskboard, selected WP contract/state and relevant
    specification topics
  execute: Write a bounded WP directly; activate in state; implement in this checkout
  recover: Read current Git diff and WP state; reconcile uncertain effects before retrying
  proof: Run only affected focused checks; broad/slow checks only when explicitly required
  status: Update WP state then taskboard; never mark unimplemented product behavior done
```
