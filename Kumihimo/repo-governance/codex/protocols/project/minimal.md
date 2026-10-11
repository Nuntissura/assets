# Repository operation

Projection of `repo-governance/codex/current/codex-source.yaml` selected model-operated contract. Read the canonical source and repository workflows; regenerate this projection when that source changes.

```yaml
role_authorization:
  maintainer: Owns bounded product and governance work; no compulsory role handoffs
transition_registry:
  phases:
  - planned
  - active
  - blocked
  - done
  - cancelled
  done_requires: WP acceptance evidence; pushed containment when publication is required
operating_protocol:
  start: Read taskboard, selected WP contract/state and relevant specification topics
  execute: Write a bounded WP directly; activate in state; implement in this checkout
  recover: Read current Git diff and WP state; reconcile uncertain effects before
    retrying
  proof: Run only affected focused checks; broad/slow checks only when explicitly
    required
  status: Update WP state then taskboard; never mark unimplemented product behavior
    done
```
