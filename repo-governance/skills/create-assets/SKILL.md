---
name: create-assets
description: Discover, reuse, extend, bundle, and synchronize reusable software assets in Nuntissura/assets. Use before hosts implement reusable functionality, when updating assets, or checking cross-device catalog and skill parity. Excludes image and media generation.
metadata:
  version: "2.1.0"
---

# Create assets

## Mandatory before-use update

- [ASSET-SYNC-008] On every invocation, before relying on this skill or its inventory, run `python scripts/asset_sync.py before-use --repo PATH` from this installed skill directory. Discover PATH from the verified checkout, `ASSETS_REPO`, or machine-local Git setting `assets.repo`; never guess a drive. The bootstrap needs Python 3.11+ and Git, not PyYAML.
- [ASSET-SYNC-009] After the bootstrap succeeds, reread the installed `SKILL.md` and generated inventory at the reported origin revision, then follow the updated instructions. Stop on local skill edits, catalog conflicts, unresolved Git operations, divergent commits, network failures, or rejected fast-forwards; preserve all local files and do not substitute a cached inventory.
- [ASSET-SYNC-010] The before-use command fetches GitHub, advances only by fast-forward, and updates managed installed skill files. It never stashes, resets, force-pushes, or resolves conflicts. Device bootstrap is required once; subsequent skill invocations perform this update automatically.

## Instruction index

- ASSET-SCOPE: discovery and containment
- ASSET-CORE: asset ownership and modularity
- ASSET-BUNDLE: module assets
- ASSET-GOV: proportional governance
- ASSET-INDEX: asset index
- ASSET-PROOF: validation and delivery
- ASSET-SYNC: cross-device publication and parity
- ASSET-FORK: controlled divergence

## Discovery and containment

- [ASSET-SCOPE-001] Use https://github.com/Nuntissura/assets as the asset repository identity; discover its local checkout from the current environment and verify its Git remote rather than hardcoding a disk path.
- [ASSET-SCOPE-002] Inspect the repository index, applicable authority, target asset manifests, public API, and relevant host integration before choosing files or architecture.
- [ASSET-SCOPE-003] Keep every asset beneath the visible `assets/` parent on the canonical branch of the single GitHub repository. Treat each asset subfolder as its own repository-like work scope with independent ownership, proof, and existing governance. Do not use permanent branches as asset locations or create nested Git repositories.
- [ASSET-SCOPE-004] Verify actual Git boundaries before staging, committing, or changing dependencies; do not assume a folder is an independent Git repository.
- [ASSET-SCOPE-005] Use an isolated Git worktree of the shared repository when an asset task needs independent Git state; work inside that worktree's `assets/NAME` scope. A Git worktree is a repository checkout, not a nested repository inside an asset. Do not add governance to an asset that has none.
- [ASSET-SCOPE-006] Preserve unrelated working-tree edits and existing asset names.
- [ASSET-SCOPE-007] Research current relevant implementations before a new feature or substantial design, following applicable operator and project research requirements; record sources, reuse, rejected approaches, risks, mitigations, and proof before implementation. Recorded-failure remediation follows its narrower applicable rules.

## Asset ownership and modularity

- [ASSET-CORE-001] When a host project needs capability or logic missing from an asset, first implement and validate that capability in the asset, then integrate the updated asset into the host.
- [ASSET-CORE-002] Do not patch around missing reusable asset logic in the host or maintain a divergent host copy of it.
- [ASSET-CORE-003] Keep host-specific policy, storage, credentials, and application orchestration in the host where the asset's inspected contract assigns them; asset-first development does not move every host responsibility into the asset.
- [ASSET-CORE-004] Modularize every asset internally around distinct responsibilities; a small asset may use modules within one crate.
- [ASSET-CORE-005] Use separate crates when independent reuse, dependency isolation, or a GUI boundary requires them; do not require one crate per internal module.
- [ASSET-CORE-006] The host provides the GUI, including rendering, styling, layout, and GUI-framework event wiring; the asset provides the reusable logic that makes that GUI work.
- [ASSET-CORE-007] Put any required GUI in a separate crate that depends on the asset core; an optional GUI feature inside the core crate does not satisfy this rule.
- [ASSET-CORE-008] Keep GUI-framework dependencies and rendering out of the core crate; retain reusable interaction and behavior logic in the asset.
- [ASSET-CORE-009] When requested work touches an existing asset that violates the GUI boundary, identify the affected integration and propose the smallest compatible extraction; do not silently waive the rule or perform an unrelated repository-wide migration.
- [ASSET-CORE-010] Keep asset paths portable and local configuration explicit; assets must not depend on a particular host checkout or machine drive.
- [ASSET-CORE-011] Expose framework-independent state, actions, transitions, validation, and results needed by the host GUI through the asset's public API, as required by the requested behavior.
- [ASSET-CORE-012] When a host GUI needs missing reusable interaction or behavior logic, implement and validate that logic in the asset first, then wire the host GUI to it.

## Module assets

- [ASSET-BUNDLE-001] A module asset may bundle multiple assets behind one public integration contract.
- [ASSET-BUNDLE-002] Inspect existing components before creating a bundle; reuse their capabilities through explicit dependencies instead of copying their implementations.
- [ASSET-BUNDLE-003] Keep bundle orchestration separate from component logic; fix missing component capability in the owning component before wiring it into the bundle.
- [ASSET-BUNDLE-004] Describe component ownership, dependency direction, and which APIs the bundle exposes; preserve component independence where the component contract promises it.
- [ASSET-BUNDLE-005] Detect dependency cycles and conflicting versions or features before integration; resolve the specific conflict without merging unrelated assets into a shared core.

## Proportional governance

- [ASSET-GOV-001] Read and follow an asset's existing governance when present; read any rules or helpers it requires for the assigned work.
- [ASSET-GOV-002] Asset-specific governance is optional for asset subfolders; do not create taskboards, work packets, refinements, registries, or role protocols merely because a small asset is created or changed.
- [ASSET-GOV-003] For a small asset without governance, use the operator request as scope and keep execution coordination in the current chat; absence of optional governance alone is not a blocker.
- [ASSET-GOV-004] Keep product documentation and meaningful verification proportional to the asset's API and behavior; optional governance does not mean undocumented usage or unverified delivery.
- [ASSET-GOV-005] If applicable authority conflicts or a required workflow is undefined, state the exact gap and propose a fallback before the affected action; do not manufacture authority.
- [ASSET-GOV-006] Keep any governance separate from product code and honor applicable authority-mirroring rules when authority files exist.

## Asset index

- [ASSET-INDEX-001] Update the canonical asset index for every asset creation, capability change, bundle change, rename, retirement, or integration-metadata change that affects its entry.
- [ASSET-INDEX-002] Discover the existing canonical index and preserve its schema; do not treat a directory listing or unrelated web index as the asset index.
- [ASSET-INDEX-003] Verify affected entries against actual manifests and documentation, including component links for bundles; check referenced paths and avoid duplicate identities.
- [ASSET-INDEX-004] If the index is missing or its authority is ambiguous, report the gap and propose a minimal machine-readable index location and schema for operator decision; do not claim index compliance or task completion while that required update remains blocked.

## Validation and delivery

- [ASSET-PROOF-001] Derive acceptance from the operator request and applicable asset or host contracts; validate the asset before dependent host integration.
- [ASSET-PROOF-002] Use the existing required proof commands; prepare and check the applicable gameplan before builds, tests, or costly or destructive commands where required by operator or project instructions.
- [ASSET-PROOF-003] Validate meaningful changed behavior at the asset's public API, then validate the host's actual use of the updated asset; component success alone does not prove host integration.
- [ASSET-PROOF-004] Verify a GUI split through inspected manifests and resolved dependency evidence that the core does not pull GUI dependencies, plus the affected adapter's required proof.
- [ASSET-PROOF-005] For bundles, validate both the changed component contract and the bundle's composition behavior; run unchanged component proofs again only when affected inputs or acceptance require it.
- [ASSET-PROOF-006] Verify the host resolves the intended asset version or revision and feature selection, rather than a stale copy or unrelated local override.
- [ASSET-PROOF-007] Read the final deliverable and index entry before reporting completion; report changed asset, host integration when requested, checks performed, and any exact remaining blocker.
- [ASSET-PROOF-008] Keep commits within verified Git ownership boundaries; the skill alone authorizes no publishing, deployment, or changes to unrelated hosts.
- [ASSET-PROOF-009] For GUI behavior changes, prove the reusable logic through the asset API and the requested user interaction through the host's GUI verification workflow; neither proof substitutes for the other.

## Discovery requirement

- [ASSET-SCOPE-008] Before implementing reusable functionality in a host, freshly fetch the verified origin, search `assets-index.yaml` by intended behavior, and inspect plausible candidates and their contracts; do not wait for the operator to name an asset.
- [ASSET-SCOPE-009] Reuse an adequate asset, extend and validate its missing reusable capability first, or create a new asset only after inspected candidates cannot appropriately own the capability; record the new asset's rationale in its index entry.
- [ASSET-INDEX-005] `assets-index.yaml` is the sole authored inventory. `references/assets-index.yaml` beside this skill is a generated discovery copy, never a second source of truth; verify it against the repository before relying on it.
- [ASSET-INDEX-006] Read `repo-governance/asset-catalog-policy.yaml` in the verified repository for schema, discovery, fork, versioning, and publication requirements. Follow its commands from the repository root or pass `--repo` to its checker.
- [ASSET-INDEX-007] Index every reusable package and composition beneath `assets/` on the canonical branch. New assets must be integrated there; historical branch aliases are discovery evidence, never the published asset location. Classify exclusions explicitly and keep source-only drafts separate from installable assets. Preserve existing names, IDs, paths, and the assets.index.v1 schema.

## Cross-device publication and parity

- [ASSET-SYNC-001] Use GitHub origin as the cross-device publication target. Run `python repo-governance/scripts/asset_catalog.py parity --skill-dir PATH` against every installed and mirrored copy of this skill before dependent work and after publication; supply each directory with another `--skill-dir`.
- [ASSET-SYNC-002] A parity failure requires reading its findings: reconcile relevant remote updates, refresh stale catalog metadata or generated copies, and preserve unrelated unfinished work. Never overwrite conflicting work or treat offline or partial checks as synchronized.
- [ASSET-SYNC-003] After approved changes, run the checker with `refresh`, then `check`, and use `install --skill-dir PATH` for selected local copies. Refresh derives package versions from manifests; it does not invent version bumps or independently author skill entries.
- [ASSET-SYNC-004] After an asset change passes its owning authority's proof, automatically publish it with `python scripts/asset_sync.py publish --repo PATH --plan PLAN.json --message TEXT`. Derive the plan from inspected asset contracts; include every selected asset ID and its meaningful proof commands. The helper uses a private index and an isolated candidate checkout, then a normal push and fresh origin reread. Preserve unrelated staged and unstaged work; stop on failed proof, remote advancement, or conflicts. Review-required work follows its owning review gate before canonical publication.
- [ASSET-SYNC-005] Freshly fetch and reread the published commit and catalog before claiming publication. Report exact source revisions and remaining drift; equal package versions alone do not prove source equality. Host adoption additionally requires its actual resolved revision and feature proof.
- [ASSET-SYNC-006] Maintain the repository-owned skill at `repo-governance/skills/create-assets`; install or mirror that skill and its generated inventory on each device. Global configuration mirrors distribute copies and do not become asset authority.
- [ASSET-SYNC-007] Preserve independent package versions; review semantic compatibility on public contract changes. Catalog-contract and skill releases have their own versions, and incompatible catalog data formats require a schema change. Do not embed the catalog's own containing commit hash in it.

## Controlled divergence

- [ASSET-FORK-001] Prefer configuration, an adapter, composition, or a compatible extension over a fork; divergent host copies remain prohibited.
- [ASSET-FORK-002] A fork requires a demonstrated conflict between core purposes, domain rules, invariants, or lifecycle behavior while the original must retain its promised contract.
- [ASSET-FORK-003] Before forking, document both purposes, conflicting acceptance cases, why each nonfork alternative fails, ownership, and maintenance; obtain explicit operator approval of that concrete decision.
- [ASSET-FORK-004] Different hosts, GUI frameworks, branding, storage integrations, deadlines, missing features, and convenience alone do not justify a fork. Reuse shared components through dependencies where practical.
- [ASSET-FORK-005] Keep an approved fork in its own folder of this repository, with an independent stable ID, `fork_of`, and an approved `fork_decision` path in the index.

## Model use and recovery

Python 3.11+ and PyYAML are required by the checker. Discover the local assets checkout and verify its remote. From its root, use `search --query "intended behavior"` to find entries; open each entry's source folder, public API and manual at its recorded source branch. Every current entry resolves to a visible canonical folder. Read its `governance` entrypoint when present; `mode: none` adds no governance ceremony. Source presence and catalog status do not establish product readiness.

Use `check` for offline structural validation. Use `parity --skill-dir PATH` for fresh origin, source-content, package-version, catalog, and installed-copy checks. Exit 0 means the requested check passed, 1 means drift, and 2 means an environment/input error. Read the JSON findings and repair only the relevant constraint. Catalog checks do not merge, stage, commit, push, or stop processes. The separate sync helper performs the explicitly authorized update and publication steps. `refresh` writes the catalog and its generated skill copy; `install` explicitly updates selected skill files.

Research and implementation decisions are recorded in the repository's `repo-governance/asset-catalog-policy.yaml`. Repository manifests and all fetched origin branches were inspected on 2026-10-22. Old checkout observations are not permanent product facts.

## Publication plan

The plan is an ignored, task-specific JSON file. `assets` contains entries with `id`, an explicit `paths` list of task-owned files inside that asset, and nonempty `checks`; each check has an argument-array `argv`, repository-relative `authority` pointing to the inspected asset rules or manual, and optional repository-relative `cwd` within that asset folder. Do not list whole directories or sweep other local edits within the same asset into publication. No shell command strings are accepted. Run required gameplan preparation before costly proof commands. Publication reruns proof against the exact isolated candidate, so unrelated local dependency edits cannot certify an unpublished combination. It preserves the caller's real Git index and files and reports the published revision; reconcile that revision before the next task.

For a small asset without governance, derive focused public-API proof from the operator request and existing manual. For a governed asset, retain every applicable owner gate, including independent review where required. A successful helper exit establishes only the declared proof and remote tree equality, not an invented compatibility verdict.
