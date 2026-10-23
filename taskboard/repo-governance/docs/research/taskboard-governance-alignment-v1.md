---
file_id: TASKBOARD-GOVERNANCE-ALIGNMENT-EVALUATION-v1
file_kind: research-note
updated_at: 2026-10-13
status: proposed
---

<topic id="operator-direction" status="recorded" version="3">

# Operator direction

This evaluation preserves the request made on 2026-10-13. It is context and a proposal, not product authority, an activated contract, or permission to execute future work.

| Intent ID | Operator direction retained |
| --- | --- |
| INT-TASKBOARD-001 | Taskboard is a reusable asset for multiple apps and websites. |
| INT-TASKBOARD-002 | It will be a major part of mechanical repository governance for future projects and part of HSKernel. |
| INT-TASKBOARD-003 | Dexterity will route local and cloud models, with ACP-like interaction and Kubernetes/VM launching and management. |
| INT-TASKBOARD-004 | Taskboard is the gate for mechanically processed repository paperwork. Models work on product code and supply meaning; they should never format governance files. |
| INT-TASKBOARD-005 | Malformed governance files and repeated lint/check failures have consumed more time than product work. Removing this burden is the primary workflow outcome. |
| INT-TASKBOARD-006 | The existing module was built for Handshake Portal; expand it toward HSKernel needs while preserving its existing use. |
| INT-TASKBOARD-007 | HSRepoTemplate currently initializes and aligns repositories through models, with stock structure, authority, templates, roles and workflows. Related workflows also exist in other projects; their contracts have not been inspected in this evaluation. |
| INT-TASKBOARD-008 | The asset should connect to a database. Handshake's intended future database is SurrealDB. |
| INT-TASKBOARD-009 | The original Handshake Creation Template targets Handshake-owned repositories, rather than model-owned mechanics. |
| INT-TASKBOARD-010 | Kanban is a useful analogy, but rules and ceremony should execute mechanically so models can focus on product work. |
| INT-TASKBOARD-011 | Evaluate and align before coding; use subagents to read, research, write and deliberate. |
| INT-TASKBOARD-012 | This expansion is backend work. Hosts do most GUI work; Taskboard exposes its API and GUI adapters. |
| INT-TASKBOARD-013 | A taskboard with work packets is the simplest repository workflow and setup. |
| INT-TASKBOARD-014 | Link an evaluation system to that workflow; its design remains unresolved. |
| INT-TASKBOARD-015 | Refinement, enrichment and microtasks extend the baseline into full, strict repository governance. |
| INT-TASKBOARD-016 | Provide a template editor for build rules, whose edits affect that repository's work-packet and microtask templates. |
| INT-TASKBOARD-017 | Models use the resulting templates. This does not withdraw the earlier boundary that models do not format governance files. |
| INT-TASKBOARD-018 | New templates do not supersede older templates; retain the previous versions. |
| INT-TASKBOARD-019 | Editing or activating a WP or MT updates that document to the project's latest template, combining its fixed fields with the WP/MT-specific content. |
| INT-TASKBOARD-020 | Taskboard tracks projects. Each project uses stock templates and can customize them for that project. |
| INT-TASKBOARD-021 | Link build rules to fields satisfied in the WP or MT and to the evaluator/validator that checks them. |
| INT-TASKBOARD-022 | Explore mechanical evaluations that help models, with fixed fields and stable IDs as the proposed foundation. |

</topic>

<topic id="inspected-baseline" status="inspected">

# Inspected baseline

The current product and its own governance board are distinct:

- `src/lib.rs` implements content validation, revision increments and legacy board projection. `web/Taskboard.tsx` implements host-driven board/task CRUD, three statuses, response fencing and retained create retries.
- `module.yaml` declares no callable operations, provided services or resolved kernel binding. Persistence and authorization remain host-owned.
- `repo-governance/taskboard/taskboard.yaml` is a separate planning projection with no eligibility evaluator or executable microtasks. The work registry is empty.
- Local authority selects planning and preserves public paths and existing contracts. Runtime, standalone browser, release and integration guarantees remain unproven by this evaluation.

The original template already separates model meaning from managed mechanics. `WF-OWN-001` assigns managed documents to the controller. `ACTION-STOCK-001` gives models action types, returned target handles, semantic payload and evidence handles; adapters/controllers derive mechanical identity, revisions, hashes, paths, timestamps and receipts. HSKernelSDK dispatch does not itself authorize governance mutations.

HSRepoTemplate is transitional: `HSRT-OPERATE-002/008` permits the model coordinator to perform missing mechanics. This permission does not establish a released automated controller. The inspected template source and installed Codex template files matched in the template subagent's byte comparison; the pack is v40. No full release check was run.

Two explanatory-source defects were reported by that reader: the original start guide says stock workflow is managed-mode only, while its current workflow payload permits managed and model-operated modes; the Taskboard description omits `needs_new_approach` and uses verdict wording inconsistent with current owners. Canonical payloads take precedence. Neither source was edited.

</topic>

<topic id="proposed-responsibilities" status="proposed">

# Proposed responsibilities

Expanding the reusable Taskboard asset does not merge the template's distinct owners into editable cards. The template's taskboard projection remains a generated view.

| Participant | Proposed responsibility |
| --- | --- |
| Taskboard | Backend work-domain gate with project tracking, typed commands, workflow state, evaluation links, stock/project template processing and projections. Expose APIs and GUI adapters. Strict governance adds the applicable claims, microtasks and proof gates. |
| Host apps and websites | Own most GUI design, rendering, navigation and interaction, including board and template-editor screens; use Taskboard's API and GUI adapters. |
| Handshake | Bind project/template authority and orchestrate repository initialization, refinement, contract compilation, activation and settlement through the shared gate. Reuse Taskboard's eligibility result rather than independently overriding it. |
| HSKernel / HSKernelSDK | Supply reusable operation transport, lifecycle, resources, effects, observability and integration contracts. Exact Taskboard bindings remain to be specified. |
| Dexterity | Route permitted local/cloud model profiles, manage execution sessions and the requested Kubernetes/VM environments, and return observed outcomes. Execution success does not establish accepted product proof. |
| Models | Supply task-specific meaning, product changes, findings and evidence. Request transitions through tools. Do not author managed IDs, paths, hashes, state files, projections or receipts. |
| Storage adapter | Persist the state assigned to it under a pinned concurrency/recovery contract. SurrealDB is the operator's intended future backend, not an implemented dependency. |

One enforcement owner per applicable operation is the key alignment decision. The simple workflow needs its own bounded operation map; refine, enrich and microtask operations belong to the strict extension. Each map identifies actors, admission checks, commit checks, durable effects and recovery. Host frontend controls consume this map; they do not invent transitions.

Keep simple website tasks independently usable under their existing behavior contract. Sharing UI components does not make ordinary `done` editing a route to a governed completion verdict. Project-selected policies should avoid imposing repository ceremony on every website.

</topic>

<topic id="workflow-depth-and-evaluation" status="direction-recorded" version="1">

# Workflow depth and evaluation

The operator's baseline is a taskboard with work packets. Refinement, enrichment and microtasks are additions for full, strict repository governance, not prerequisites for the simple setup. The earlier evaluation's strict-template examples describe that extension; they do not define the minimum adoption contract.

Evaluation links belong in the alignment scope, but the evaluation system is not yet designed. No evaluator, scoring model, verdict vocabulary, execution owner or mandatory gate is selected here. In particular, do not equate the unspecified evaluation system with the existing strict workflow's independent validator. Alignment should describe the linking boundary while retaining these open decisions.

Both workflow depths retain mechanical governance formatting. Models consume templates and provide task-specific meaning/product work; the backend constructs records. Host GUI work is primarily outside the backend asset. The existing Portal UI remains a preserved consumer, not the required GUI for future hosts.

</topic>

<topic id="template-editor" status="direction-recorded" version="1">

# Template editor

The requested editor changes a repository's build rules and propagates their effect into its work-packet and microtask templates. Models use these resulting templates for their work. The host normally supplies the editor GUI; Taskboard supplies the backend API, GUI adapters and mechanical template processing.

Proposed interaction: read the repository's current rule/template versions, submit semantic rule edits, inspect affected template changes, validate consistency, then publish the resulting versions and diagnostics. This is a proposal rather than an implemented API. Reuse the existing build-rule-to-WP/MT contract-field relationship instead of introducing independently edited copies of the same rule.

The operator has now specified the refresh triggers: editing or activating a WP or MT rebuilds that document against its project's latest template. Preserve its task-specific meaning and values while applying the template's fixed fields. New template versions coexist with old ones; merely creating a version does not rewrite every existing document. Editing and activation are explicit refresh events, rather than silent changes to untouched work. Historical versions remain available.

Taskboard tracks each project's stock-template selection and project-specific customization. Proposed composition is the selected stock shape plus explicit project changes and build-rule-linked fields, followed by the document's WP/MT-specific content. Project edits apply to that project; they do not mutate the stock source or other projects. The exact customization and stock-update reconciliation format remains to be designed.

Refresh is not permission to invent newly required task-specific answers. Proposed handling preserves matching values by stable field identity, exposes unsatisfied new requirements and blocks activation until they are satisfied. Incompatible type/meaning changes need explicit migration diagnostics; keep previous complete versions on failure. Parent-contract links and affected validation results also need reconciliation when a WP refresh changes its requirements. These controls are proposals for the operator-defined refresh behavior, not implemented mechanics.

Future proof should show that an accepted rule edit changes the affected WP/MT template content consistently, preserves unrelated content, produces usable model context and leaves previous valid versions intact on rejection/interruption. Microtask template support remains available for strict governance without making microtasks mandatory in the simple workflow.

</topic>

<topic id="linked-fields-and-mechanical-evaluation" status="proposed" version="1">

# Linked fields and mechanical evaluation

The pinned build rules already define `contract_fields` with WP/MT field paths. `BR-CONTRACT-SYNC-001` requires template regeneration when these links change and reassessment of open work. This provides an existing basis for the requested rule-to-template linkage, but it does not by itself implement a mechanical evaluator.

Proposed stable linkage: rule ID -> field ID -> WP/MT value or evidence -> check ID -> result tied to the document/input revision. Display labels and field paths can change without losing identity. Preserve IDs for the same meaning; incompatible meaning changes require a new identity or explicit migration, not silent reuse.

Each evaluable field needs more than an ID: a type, applicable workflow/project conditions, a requiredness rule, a defined satisfaction predicate, and any required evidence. A check definition needs an implementation or declared external evaluator and an expected observable result. Fixed fields define a versioned schema, not one permanently frozen schema across projects.

Mechanical checks can validate required values, types, reference resolution, allowed transitions, evidence freshness and registered test outcomes. A filled field or model-authored `satisfied` flag does not prove product behavior. Semantic/product judgments still need an appropriate evidence source or evaluator; the wider evaluation-system design remains open.

[JSON Schema's object contract](https://json-schema.org/understanding-json-schema/reference/object) provides concrete required-property and value-schema mechanics. This is research evidence for structural checks, not a selected runtime dependency or proof that arbitrary build rules can be automatically evaluated.

Future proof should trace an added build rule through its generated project-template field, refresh an edited or activated document without losing specifics, reject missing satisfaction/evidence, and produce a revision-bound check result. Also prove project isolation, historical-template retention and invalidation of only affected prior checks.

</topic>

<topic id="storage-and-materialization" status="decision-needed">

# Storage and materialization decisions

The inspected templates currently identify repository-file owners. Database authority is not adopted by this note. Alignment must decide, separately for specification, authority, contracts, lifecycle state, evidence and views, which representation is canonical and which is generated.

Proposed workflow: semantic request -> validated domain command -> generation-checked state/event/publication intent -> deterministic materialization -> reconciled publication result -> board and worker context. These are proposed stages, not existing callable commands.

The database cannot atomically commit ordinary filesystem output and a Git commit. A recoverable publication contract therefore needs expected source revisions, staged complete outputs, validated digests, publication status, interruption recovery and a rule for when new authority becomes effective. A database update alone must not release executable work while required repository publication is incomplete.

SurrealDB snapshot transactions detect write conflicts; a read-only policy record is not automatically a conflict guard. The eventual pinned backend must prove the guard used for policy/claim generations. Database capability and import/export contracts should remain separate from workflow meaning.

Mechanical construction removes syntax repair from model work, but it cannot remove semantic mistakes. Bad semantic submissions need field/reference diagnostics before any mutation. Managed-file restrictions also need enforcement across shell tools, child processes and worker environments; a chat instruction alone is insufficient.

</topic>

<topic id="research-basis" status="researched">

# Current primary-source basis

| Sources checked | Pattern and proposed reuse |
| --- | --- |
| [Backstage action docs](https://backstage.io/docs/features/software-templates/writing-custom-actions/) and [template action source](https://github.com/backstage/backstage/blob/master/plugins/scaffolder-backend/src/scaffolder/actions/builtin/fetch/template.ts) | Schema-defined input/output and constrained materialization. Reuse the pattern of models supplying values while code owns artifact shape. |
| [Backstage idempotency design](https://github.com/backstage/backstage/blob/master/beps/0004-scaffolder-task-idempotency/README.md) | Persisted checkpoints still require explicit action idempotency. Treat retries as a domain contract. |
| [Temporal History Service source](https://github.com/temporalio/temporal/blob/main/docs/architecture/history-service.md) | Durable state transitions, queued effects and ownership fencing. Use as implementation evidence for recoverable dispatch/publication, not as a selected dependency. |
| [Kubernetes controllers](https://kubernetes.io/docs/concepts/architecture/controller/), [API concurrency](https://kubernetes.io/docs/reference/using-api/api-concepts/) and [leases](https://kubernetes.io/docs/concepts/architecture/leases/) | Reconciliation, expected versions and lease coordination. Applicable to concurrent model work and Dexterity runtime adapters. |
| [ACP introduction](https://agentclientprotocol.com/get-started/introduction) | Editor/agent communication protocol. ACP alone does not define the requested Kubernetes/VM manager; preserve that as a separate Dexterity responsibility. |
| [SurrealDB transactions](https://surrealdb.com/docs/learn/querying/concepts-and-guides/transactions) | Snapshot isolation and explicit conflict considerations. Verify the actual pinned backend rather than assuming every read participates in conflict detection. |

Selected design direction for evaluation: reuse the existing template semantics through typed commands and mechanical construction, with a generated Kanban view. No framework dependency was selected. Rejected approaches are freeform model-written governance YAML, an independently writable DB/file pair, duplicate Taskboard/Handshake eligibility engines, and making the UI component itself the infrastructure manager. These recreate the reported burden or conflicting authority.

</topic>

<topic id="red-team-and-roi" status="proposed">

# Risks, controls and proof

| Failure | Proposed minimum control | Future acceptance proof |
| --- | --- | --- |
| Models submit malformed meaning or try to write governance bytes | Typed semantic input and enforced managed-path permissions | Rejection identifies the field/reference, changes nothing, and blocks direct writes through worker tools. |
| Concurrent claims or stale worker submissions | Atomic expected revisions and lease generations | One accepted owner; an expired generation cannot advance work. |
| Lost acknowledgment creates duplicate work | Durable command identity and original-intent comparison | Repeated request returns the same result; changed intent under the same identity fails. |
| DB commit succeeds but publication/projection fails | Durable pending effects and deterministic reconciliation | Crash at each boundary preserves state, exposes lag and recovers without duplicate activation. |
| UI shows readiness or completion from stale/partial records | Shared eligibility evaluator, complete-source generation and independent verdict authority | Paginated/filtered views cannot redefine the canonical set; a stale view cannot authorize a transition. |
| Template/policy changes alter active work silently | Exact pins and explicit reactivation/migration | Existing work retains its obligations until an authorized revision. |
| Governance expansion breaks Portal reuse | Preserve exports and ordinary task contract; separate governed behavior | Existing host CRUD remains compatible and cannot bypass governed proof. |
| Strict ceremony leaks into the simple workflow | Explicit baseline and strict-extension contracts | Board/WP use does not require refinement, enrichment or microtasks. |
| Build-rule edits leave templates inconsistent | One mechanical propagation path with affected-field diagnostics | WP/MT template changes agree with the accepted rule version; invalid edits publish nothing. |
| Template refresh loses specifics or invents satisfaction | Stable field mapping, preserved versions and explicit missing-field/migration diagnostics | Edit/activation refresh retains task meaning; missing answers prevent activation rather than receiving fabricated defaults. |
| A project customization affects other projects | Project-scoped template composition | Changing one project leaves stock source and unrelated projects unchanged. |

High-return additions within this alignment are command documentation/examples generated from the same schemas, dry-run materialization diffs, and revision-aware drift diagnostics. They reuse the command/schema/generator work, reduce no-context model mistakes, make adoption review concrete and replace vague lint failures with actionable state. Future proof covers schema/example agreement, effect-free previews and deliberate projection corruption/rebuild.

</topic>

<topic id="alignment-sequence" status="proposed">

# No-code alignment acceptance and next step

Before implementation, this module's own alignment/refinement should contain the ownership map, model-input/managed-output contract, storage authority table, baseline/strict-extension distinction and host API/GUI-adapter boundary. Trace existing website CRUD, simple board/WP use, strict governance, a build-rule edit propagating into WP/MT templates, and interrupted publication/recovery. Each trace identifies actors, canonical entities, generations, generated outputs and applicable proof. Carry the full operator intent forward and link it to the accepted specification anchors. This module's planning procedure does not require every consuming repository to use refinement.

The first future implementation candidate starts with the simple board/WP backend and mechanically generated model context, with host API/adapter consumption. It covers malformed input, duplicate retry, stale generations and interruption. Evaluation linking remains in scope without inventing its unsettled semantics. Strict refinement/enrichment, microtasks, independent verdicts where applicable, template editing/propagation, bootstrap, remaining lifecycle, database, HSKernel and Dexterity integration are carried forward. This is internal sequencing of the full requested scope, not a reduced external commitment.

There is a concrete local alignment dependency: `check-alignment.py` explicitly requires both the registry and projected work list to be empty. Stock `REPO-INIT-007` requires a real planning stub to have a registry entry and `stub_drafting` state. Registering one now conflicts with the available proof gate. Align the planning registry/projection/check contract before registering the expansion stub; do not silently remove the check or pretend an unregistered note is a WP.

No product code, canonical specification, template source, mode, active contract, work registry or projection is changed by this evaluation. It does not establish controller release, SurrealDB integration, host compatibility or runtime acceptance. The current authoritative spec remains the hosted taskboard contract until the accepted refinement is archived/enriched through its existing update protocol.

</topic>
