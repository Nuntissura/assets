---
file_id: KUM-MANUAL-v1
file_kind: product-and-model-manual
updated_at: '2026-10-11'
---

<topic id="kumihimo">

# Kumihimo

A reusable Rust window/frame asset for desktop and web apps: grids, tabs, grouped tabs, docking, popouts and configurable surrounding controls. Hosts provide the actual editors, browsers, media and graphics.

The first Rust ownership core passed its Windows x64 compile check, ten focused tests and console example. It manages nested frames, ordered views, groups, host resource references, validated moves and bounded command replay. Working GUI adapters remain planned. The researched spec covers 18 feature areas with explicit host ownership and focused acceptance scenarios.

</topic>

<topic id="feature-direction">

## What the asset is meant to provide

- Recursively nested windows and workspaces, with host-chosen counts, resizing, docking, floating frames and supported native popouts.
- Any host-chosen number of tabs within each eligible window, named groups and color labels, with usable overflow navigation.
- Configurable surrounding bars and controls, comfortable scrollbars and host-fed minimaps; the host selects scrollbar only, permanent minimap, both, or a minimap that appears while scrubbing.
- Content slots for editors, browser views, tools, media and 2D/3D renderers.
- Image/video backgrounds with visual settings and provider-aware resource controls.
- Saved layouts, recovery, touch/pen/keyboard access and structured model commands.
- Visibility and lifecycle signals for host resource management, including the future Servo browser.

Kumihimo arranges the workspace. The host owns document reflow, scenes, browser pages and actual suspension/unloading. Saved tab records and live content instances have separate budgets. Toolkits, rendering paths and platform compatibility still need proof.

The conceptual window/tab model has no fixed count or nesting ceiling. A window with 20 child windows and 30 tabs in each is an illustration, not a stock layout or maximum. Each child can itself contain another workspace. Practical resource policy is exposed to the host separately.

Start with the [master spec](repo-governance/specification/current/spec-modules/master-spec.md), [roadmap](repo-governance/specification/current/spec-modules/roadmap.md), or [field research and optional ideas](repo-governance/docs/research/window-frame-field-research.md). Optional ideas include scrolling workspaces, an overview, edge trays and layout undo; they are not current requirements.

</topic>

<topic id="module-boundaries">

## Three reusable parts

The core holds nested window/tab state, layout calculations and commands without requiring a GUI. Optional GUI adapters supply reusable controls and connect platform input, focus and accessibility. Host-content integration supplies contracts and adapters for content size, rendering, input and lifecycle; the host owns the actual editor, browser, media or graphics engine.

Hosts can use the supplied controls, replace selected controls, or supply their own presentation. Keep Rust code in small cohesive modules and packages, bundling only responsibilities that belong together. Three logical submodules do not prescribe a fixed number of crates; backend integrations remain independently selectable.

The [technical gap research](repo-governance/docs/research/modularity-and-technical-gaps.md) records proposed contract details and useful extensions from the three-agent review. Those additional ideas remain discussion material.

</topic>

<topic id="model-workflow">

## Model workflow

Read the identical AGENTS.md/CLAUDE.md entry points, the project CODEX, relevant build rules and the [taskboard](repo-governance/taskboard/taskboard.yaml). Choose a bounded WP; read its contract/state and the relevant [master-spec topics](repo-governance/specification/current/spec-modules/master-spec.md). Activate, execute, run focused proof, record recovery state, and rebuild the board.

The dev lead maintains this asset in the shared assets checkout with dedicated coding, dependency, diagnostic and evaluation chats. Start every session with `python repo-governance/repo-scripts/session_start.py`; the canonical session record contains role targets, coder rotation and build ownership. Dependencies precede coding, evaluation is focused, Cargo is serialized and cache reuse is verified. A 15-minute thread heartbeat checks progress/resources without interrupting other Rust teams.

The model maintains this asset directly in the shared assets checkout. No enrichment phase, mandatory microtasks/worktrees, formal handoff pipeline, controller dependency, long test suite or stock compliance audit is required. Reference templates do not add gates. Engineering boundaries and honest proof still apply.

Inputs: operator requests and WP contracts. Outputs: owned file changes, observed proof and recoverable WP state. Status is planned, active, blocked, done or cancelled; product roadmap status remains separate.

</topic>

<topic id="commands" ingestable="true">

## Available commands

Run from the Kumihimo folder with Python 3.12+ and PyYAML (version pinned in repo-governance/repo-scripts/requirements.txt):

```powershell
python repo-governance/repo-scripts/session_start.py
python repo-governance/repo-scripts/sync.py
python repo-governance/repo-scripts/sync.py --check
```

Session-start reads live Git, canonical packet state and session coordination. Sync rebuilds CODEX, taskboard, registry and spec navigation projections; `sync.py --check` checks source structure and freshness without writing. Neither proves GUI or Rust runtime behavior. The [product manual](product/README.md) provides the core build commands, API, console example and recovery boundaries.

</topic>

<topic id="recovery">

## Recovery and constraints

On resume read WP state and Git status/diff, inspect pending effects, then take the recorded next action. If projections are stale, rebuild them from current sources. Resolve conflicting state before writes; never fabricate completion. Preserve the prior current spec bundle before a versioned spec update.

All project paths are relative. Machine configuration, secrets, staging and build output stay ignored. Commit owned paths only. Do not modify sibling assets or stop processes the session did not start. Automated work stays quiet; foreground GUI proof must use a controlled path when later implemented.

The bootstrap helper was run in standalone staging and its files imported without a nested Git repository. Bootstrap provenance proves staging integrity, not a released Handshake controller or product runtime.

</topic>
