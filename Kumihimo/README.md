---
file_id: KUM-MANUAL-v1
file_kind: product-and-model-manual
updated_at: '2026-10-11'
---

<topic id="kumihimo">

# Kumihimo

A reusable Rust window/frame asset for desktop and web apps: grids, tabs, grouped tabs, docking, popouts and configurable surrounding controls. Hosts provide the actual editors, browsers, media and graphics.

The asset is initialized and specified; Rust packages and working GUI adapters have not been implemented.

</topic>

<topic id="model-workflow">

## Model workflow

Read the identical AGENTS.md/CLAUDE.md entry points, the project CODEX, relevant build rules and the [taskboard](repo-governance/taskboard/taskboard.yaml). Choose a bounded WP; read its contract/state and the relevant [master-spec topics](repo-governance/specification/current/spec-modules/master-spec.md). Activate, execute, run focused proof, record recovery state, and rebuild the board.

The model maintains this asset directly in the shared assets checkout. No enrichment phase, mandatory microtasks/worktrees, role handoffs, controller dependency, long test suite or stock compliance audit is required. Reference templates do not add gates. Engineering boundaries and honest proof still apply.

Inputs: operator requests and WP contracts. Outputs: owned file changes, observed proof and recoverable WP state. Status is planned, active, blocked, done or cancelled; product roadmap status remains separate.

</topic>

<topic id="commands" ingestable="true">

## Available commands

Run from the Kumihimo folder with Python 3.12+ and PyYAML (version pinned in repo-governance/repo-scripts/requirements.txt):

```powershell
python repo-governance/repo-scripts/sync.py
python repo-governance/repo-scripts/sync.py --check
```

The first rebuilds CODEX, taskboard, registry and spec navigation projections. The second checks source structure and projection freshness without writing. Neither proves GUI or Rust runtime behavior. There are no product startup/build commands yet.

</topic>

<topic id="recovery">

## Recovery and constraints

On resume read WP state and Git status/diff, inspect pending effects, then take the recorded next action. If projections are stale, rebuild them from current sources. Resolve conflicting state before writes; never fabricate completion. Preserve the prior current spec bundle before a versioned spec update.

All project paths are relative. Machine configuration, secrets, staging and build output stay ignored. Commit owned paths only. Do not modify sibling assets or stop processes the session did not start. Automated work stays quiet; foreground GUI proof must use a controlled path when later implemented.

The bootstrap helper was run in standalone staging and its files imported without a nested Git repository. Bootstrap provenance proves staging integrity, not a released Handshake controller or product runtime.

</topic>
