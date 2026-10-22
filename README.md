---
file_id: assets-repository-manual
file_kind: repository-manual
updated_at: '2026-10-22'
---

<topic id="assets">

# Assets

All published assets are visible in the [assets folder](assets/) on this repository's default branch.
The [catalog](assets-index.yaml) lists packages, versions, source links, APIs, manuals, work scopes,
and existing governance. A catalog entry establishes discoverability, not product readiness.

Each asset folder is maintained as an independent work scope. Some have strict governance; others
use their public contract and the operator request. Git history belongs to this shared repository.
For isolated work, use a separate Git worktree and work inside its selected `assets/NAME` folder.

</topic>

<topic id="skill-and-sync">

## Skill updates and publication

The repository-owned [create-assets skill](repo-governance/skills/create-assets/SKILL.md) fetches
the latest GitHub skill and inventory before every use through its `scripts/asset_sync.py before-use`
command. Install that skill on a device once; every subsequent invocation runs its bootstrap first.
The bootstrap needs Python 3.11+ and Git. Catalog checking and publication also require the packages
in [requirements.txt](repo-governance/requirements.txt).

Point the installed helper at a verified checkout with `--repo PATH`, `ASSETS_REPO`, or the
machine-local Git setting `assets.repo`. It only advances a checkout by fast-forward and stops on
local skill edits, conflicting files, divergent commits, or unavailable fresh sources. It never
stashes or resets local work.

After applicable asset proof, the helper's `publish` command reads a task-specific plan of asset
IDs and authority-derived proof commands. It builds a candidate through a private Git index, runs
proof in an isolated checkout, pushes normally, and independently rereads the published tree.
The real index and working files remain untouched. A failed candidate stays in ignored
`.local/asset-publication/` for diagnosis. Reconcile its reported origin revision before further work.

The [catalog policy](repo-governance/asset-catalog-policy.yaml) is authoritative. This manual is a
navigation and usage reference.

</topic>
