---
file_id: TASKBOARD-ALIGNMENT-BASIS-v1
file_kind: research-note
updated_at: 2026-10-11
---

<topic id="selection">

The operator selected taskboard only; sibling directories are distinct projects in the assets repository.
HSRepoTemplate Mode B permits owner-template alignment without bootstrap. Pack 40 module, specification,
topology, authority, repository and work-system payloads were inspected and preserved as source inputs.
The bootstrap helper rejects a nested Git target. A new Git repository or parent-wide bootstrap was
rejected because it changes the selected ownership boundary. Product relocation was rejected because
the inspected host checker pins eight files as `taskboard/<relative>` in the assets source revision.

[Cargo manifest documentation](https://doc.rust-lang.org/cargo/reference/manifest.html) defines package
inclusion. [Cargo workspace source](https://github.com/rust-lang/cargo/blob/master/src/doc/src/reference/workspaces.md)
documents workspace boundaries; adding a parent-wide workspace would expand scope.
[Git worktree documentation](https://git-scm.com/docs/git-worktree.html) describes linked worktrees;
no worktree is needed or activated for this module alignment.

Reuse: existing package allowlists, manual, Rust tests, React test IDs and consuming-host diagnostics.
Mitigations: preserve all original bytes, validate spec and manifest shape, inspect package contents,
keep exact source hashes and record proof gaps without claiming runtime or release acceptance.
No software dependency, SDK, service or new product behavior is selected.

</topic>
