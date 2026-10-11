# Shared model operation

Projection of `repo-governance/codex/current/codex-source.yaml` selected model-operated contract. Read the canonical source and repository workflows; regenerate this projection when that source changes.

```yaml
mechanical_adapter_fields:
  evaluator: maintainer checks current contract/state and affected proof
operating_protocol:
  start: Read taskboard, selected WP contract/state and relevant specification topics
  execute: Write a bounded WP directly; activate in state; implement in this checkout
  recover: Read current Git diff and WP state; reconcile uncertain effects before
    retrying
  proof: Run only affected focused checks; broad/slow checks only when explicitly
    required
  status: Update WP state then taskboard; never mark unimplemented product behavior
    done
unresolved_project_bindings: []
runtime_prerequisites: Resolve only dependencies required by the current WP
```
