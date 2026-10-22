# Assets repository instructions

## Instruction index

- ASSETS-REPO: repository ownership and catalog workflow

- [ASSETS-REPO-001] Read `repo-governance/asset-catalog-policy.yaml` before changing the catalog, asset discovery, synchronization, skill distribution, or fork policy; its rule IDs govern those workflows.
- [ASSETS-REPO-002] Read `repo-governance/skills/create-assets/SKILL.md` and the affected asset's existing authority before changing an asset; nested authority retains its scope.
- [ASSETS-REPO-003] Keep this repository disk-agnostic; resolve paths relative to its verified Git root and keep device-specific locations outside tracked configuration.
- [ASSETS-REPO-004] Preserve unrelated work; catalog and skill maintenance does not authorize publishing unfinished product changes from other sessions.

- [ASSETS-REPO-005] All assets live beneath `assets/` on the canonical branch. Treat each asset folder as an independent repository-like work scope with its own applicable governance; use separate worktrees of this shared Git repository for isolation. Do not create nested Git repositories or invent governance for ungoverned assets.
