---
schema_version: handshake.spec-document.v1
specification_id: SPEC-TASKBOARD-v1
document_id: DOC-TASKBOARD-CONTRACT-v1
revision: 1
title: Taskboard product contract
updated_at: '2026-10-11'
provenance:
- README.md
- src/lib.rs
- web/index.ts
- web/Taskboard.tsx
- web/taskboard.css
- Cargo.toml
- package.json
---

```yaml
kind: specification_topic
topic_id: TOPIC-TASKBOARD-CONTRACT
title: Hosted taskboard contract
content_blocks:
- block_id: context
  kind: prose
  language: markdown
  requirement_refs: []
- block_id: contract
  kind: structured_contract
  language: yaml
  requirement_refs: []
```

```markdown
# Existing product contract

This bundle preserves accepted meaning documented in the module manual and present source. It describes a hosted Rust library and React component, retaining the current src/ and web/ public paths. It does not add a project/collaboration feature: no such future feature is defined in the inspected manual or public source; unsupported sharing remains outside module ownership.

The three-column board uses task titles and pre-wrapped plain-text descriptions. Inputs and cards permit wrapping long content; the header wraps controls and uses a mobile adjustment below 600px. Component CSS does not by itself prove the embedding host column layout or accessibility. Busy state is exposed through aria-busy; errors use an alert, retained creates use a status message, and controls have labels and stable data-testid values.

Portal adapter/composition paths in the existing manual are provenance links to a host, not additional product truth dependencies. Their runtime behavior was not inspected or validated for this bundle. No runtime check was run for this alignment; source inspection does not establish a passing acceptance verdict.
```

```yaml
product:
- name: Taskboard 0.1.0
  purpose: Portable taskboard domain contracts and a host-composed React taskboard.
  operator_outcomes:
  - Create boards and tasks; read task status across board columns.
  - Edit, move, update status and delete tasks with revision checks.
  - Recover an uncertain create by retrying the same retained request.
  supported_users_and_actors:
  - Host-authorized account/profile users.
  - Host integrators composing the Rust library and React component.
  supported_platforms:
  - Rust 2021 library consumers.
  - React 19 browser hosts; no standalone server is shipped here.
architecture:
- topology_refs: []
  module_boundary_requirements:
  - Rust owns deterministic domain rules; browser owns board selection/forms/retained attempts/response
    fences.
  - Host owns authentication, account/profile authorization, persistence, quotas, logging and navigation.
  - This is a taskboard-only owner-template alignment inside the shared .assets Git repository; existing
    source layout is preserved.
  data_flow_requirements:
  - Browser typed requests communicate with the host HTTP adapter; canonical persistence remains host-owned.
  concurrency_requirements:
  - Only a live response generation changes visible state; mutations use the component guard.
  compatibility_requirements:
  - Rust package uses edition 2021 with no external dependencies; browser expects React 19.
  - Legacy missing board/revision fields project to default/0; old writers without required identity/revision
    fail.
  constraints:
  - No standalone web server, release, deployment, activation, resolved SDK or full-pack bootstrap is
    claimed by this specification.
  - Runtime acceptance and independent module lifecycle/publication guarantees remain unverified.
requirements:
- requirement_id: REQ-TASKBOARD-TITLE
  anchor_refs:
  - TOPIC-TASKBOARD-CONTRACT
  statement: Titles must be nonempty after trimming, contain no control characters and stay within the
    caller-supplied byte bound. Browser board/task title inputs have maxLength 100/300 respectively.
  rationale: Bound content without conflating browser character limits with Rust byte validation.
  acceptance_refs:
  - ACC-TASKBOARD-TITLE
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-TASKBOARD-DOMAIN
  primitive_refs: &id001
  - PRIM-TASKBOARD-VALIDATE
  - PRIM-TASKBOARD-REVISION
- requirement_id: REQ-TASKBOARD-BODY-STATUS
  anchor_refs:
  - TOPIC-TASKBOARD-CONTRACT
  statement: Task bodies are plain text with a Rust upper bound of 20000 bytes. Task status is exactly
    todo, in_progress or done.
  rationale: Preserve bounded plain-text content and the existing three-state workflow.
  acceptance_refs:
  - ACC-TASKBOARD-BODY-STATUS
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-TASKBOARD-DOMAIN
  primitive_refs: *id001
- requirement_id: REQ-TASKBOARD-REVISION
  anchor_refs:
  - TOPIC-TASKBOARD-CONTRACT
  statement: A task edit or delete requires the exact expected revision. Rust next_revision rejects a
    stale expected value and exhausted revision; success increments by one without exceeding i64::MAX.
  rationale: Prevent stale writes and revision wrap.
  acceptance_refs:
  - ACC-TASKBOARD-REVISION
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-TASKBOARD-DOMAIN
  primitive_refs: *id001
- requirement_id: REQ-TASKBOARD-LEGACY
  anchor_refs:
  - TOPIC-TASKBOARD-CONTRACT
  statement: A missing board ID projects to default without rewriting stored tasks; browser requests use
    revision 0 for legacy missing revisions. Host translation of missing board IDs/revisions is default/0.
  rationale: Preserve the existing legacy-record compatibility boundary.
  acceptance_refs:
  - ACC-TASKBOARD-LEGACY
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-TASKBOARD-DOMAIN
  primitive_refs: *id001
- requirement_id: REQ-TASKBOARD-COMPOSE
  anchor_refs:
  - TOPIC-TASKBOARD-CONTRACT
  statement: Export Taskboard and Task, Board, Request through web/index.ts. Compose the component with
    scope, root and a typed request function; integration and targetTaskId are optional. The Rust public
    API remains src/lib.rs.
  rationale: Keep public composition surfaces usable by an embedding host.
  acceptance_refs:
  - ACC-TASKBOARD-COMPOSE
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-TASKBOARD-BOARD-UI
  primitive_refs: &id002
  - PRIM-TASKBOARD-LOAD
  - PRIM-TASKBOARD-MUTATE
- requirement_id: REQ-TASKBOARD-VIEW
  anchor_refs:
  - TOPIC-TASKBOARD-CONTRACT
  statement: Load board catalogue and tasks through GET root/taskboards and GET root/tasks. Filter tasks
    by selected board, display To do/In progress/Done columns, and show selected-board done/total counts.
    A resolved targetTaskId selects its task board.
  rationale: Preserve board navigation and status visibility.
  acceptance_refs:
  - ACC-TASKBOARD-VIEW
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-TASKBOARD-BOARD-UI
  primitive_refs: *id002
- requirement_id: REQ-TASKBOARD-EDIT
  anchor_refs:
  - TOPIC-TASKBOARD-CONTRACT
  statement: Create boards and tasks; new tasks begin in todo on the selected board. Edit task title/body,
    move tasks between boards, change status, and delete through the declared host requests. Preserve
    stable data-testid controls and wrapping of long title/body text.
  rationale: Preserve the implemented task form and card interaction surface.
  acceptance_refs:
  - ACC-TASKBOARD-EDIT
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-TASKBOARD-BOARD-UI
  primitive_refs: *id002
- requirement_id: REQ-TASKBOARD-SCOPE
  anchor_refs:
  - TOPIC-TASKBOARD-CONTRACT
  statement: Changing scope, root or targetTaskId resets visible state and advances a response generation.
    Replies from an older generation or unmounted component must not update the new visible scope. Serialize
    mutations using the component mutation guard and expose busy/errors/reload recovery.
  rationale: Prevent cross-scope response leakage and conflicting local mutations.
  acceptance_refs:
  - ACC-TASKBOARD-SCOPE
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-TASKBOARD-RECOVERY
  primitive_refs: &id003
  - PRIM-TASKBOARD-LOAD
  - PRIM-TASKBOARD-MUTATE
- requirement_id: REQ-TASKBOARD-RETRY
  anchor_refs:
  - TOPIC-TASKBOARD-CONTRACT
  statement: Create requests carry a client UUID and original intent. Retain an uncertain create request
    for Retry same request using the same path, ID and payload. Host repeats with identical ID/intent
    return the canonical record; changed intent is rejected. Block fresh creates while a retained attempt
    exists.
  rationale: Recover lost replies without duplicate creation or silent intent changes.
  acceptance_refs:
  - ACC-TASKBOARD-RETRY
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-TASKBOARD-RECOVERY
  primitive_refs: *id003
- requirement_id: REQ-TASKBOARD-HOST
  anchor_refs:
  - TOPIC-TASKBOARD-CONTRACT
  statement: The host owns authentication, account/profile authorization, canonical persistence, quotas,
    logging and navigation. HTTP adapter supports GET/POST taskboards, GET/POST tasks, PATCH tasks/id
    and DELETE tasks/id?expected_revision=n. Board records contain id/title; canonical task records contain
    id/title/description/status/board_id/revision. Unsupported sharing must not widen host ownership.
    Older writers missing client IDs or expected revisions receive an error and reload. Exported Rust
    advisory limits are MAX_BOARDS=64 and MAX_TASKS=2000; enforcement remains host-owned.
  rationale: Keep module semantics separate from host state/effects and preserve writer compatibility
    boundaries.
  acceptance_refs:
  - ACC-TASKBOARD-HOST
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-TASKBOARD-HOST
  primitive_refs: &id004
  - PRIM-TASKBOARD-MUTATE
- requirement_id: REQ-TASKBOARD-PACKAGE
  anchor_refs:
  - TOPIC-TASKBOARD-CONTRACT
  statement: Publish only manifest/lockfile, public Rust source, web source, manual and license. Exclude
    credentials, operator state, governance, browser evidence, .local/.git, build outputs and dependency
    caches. A local source commit is a candidate; host release adoption pins source revision and verifies
    standalone and host compatibility. Plain-text drafts are not durable until saved. On host downgrade,
    stop writes and back up the database first; reconcile new boards/tasks before returning to an old
    host, without deleting rows.
  rationale: Preserve packaging boundaries and explicit recovery/adoption limitations.
  acceptance_refs:
  - ACC-TASKBOARD-PACKAGE
  conditions: []
  exceptions: []
  feature_refs:
  - FEAT-TASKBOARD-HOST
  primitive_refs: *id004
acceptance:
- acceptance_id: ACC-TASKBOARD-TITLE
  requirement_refs:
  - REQ-TASKBOARD-TITLE
  method: Inspect and exercise title validation at empty, control-character and byte-bound edges; inspect
    input limits.
  expected_result: Rust rejects invalid title inputs and browser title inputs declare the documented limits.
- acceptance_id: ACC-TASKBOARD-BODY-STATUS
  requirement_refs:
  - REQ-TASKBOARD-BODY-STATUS
  method: Exercise the body boundary and each valid/invalid status; inspect plain-text rendering.
  expected_result: Bodies over 20000 bytes and invalid statuses are rejected; browser renders descriptions
    as text.
- acceptance_id: ACC-TASKBOARD-REVISION
  requirement_refs:
  - REQ-TASKBOARD-REVISION
  method: Exercise equal, stale and exhausted revisions and inspect outgoing PATCH/DELETE revision parameters.
  expected_result: Stale/exhausted revisions fail; successful revisions increment once; browser requests
    carry expected_revision.
- acceptance_id: ACC-TASKBOARD-LEGACY
  requirement_refs:
  - REQ-TASKBOARD-LEGACY
  method: Inspect and exercise missing and explicit board/revision values through the declared host adapter.
  expected_result: Missing values project to default/0 and stored legacy tasks are not rewritten by projection.
- acceptance_id: ACC-TASKBOARD-COMPOSE
  requirement_refs:
  - REQ-TASKBOARD-COMPOSE
  method: Import the public browser exports in a React 19 host and inspect the Rust library API.
  expected_result: Public exports and host inputs resolve; no module-owned standalone server is required.
- acceptance_id: ACC-TASKBOARD-VIEW
  requirement_refs:
  - REQ-TASKBOARD-VIEW
  method: Exercise board selection and target task selection with host-provided records.
  expected_result: Only selected-board tasks appear in columns/counts; a known target selects its board.
- acceptance_id: ACC-TASKBOARD-EDIT
  requirement_refs:
  - REQ-TASKBOARD-EDIT
  method: Exercise create, edit, move, status change and delete in a host; inspect stable selectors and
    long-text layout.
  expected_result: Each action uses its documented endpoint/payload, and task content remains readable.
- acceptance_id: ACC-TASKBOARD-SCOPE
  requirement_refs:
  - REQ-TASKBOARD-SCOPE
  method: Delay reads and mutations while changing scope/root/target or unmounting; inspect current visible
    state and request concurrency.
  expected_result: Stale replies do not change current state; one guarded mutation runs at a time and
    current errors support reload.
- acceptance_id: ACC-TASKBOARD-RETRY
  requirement_refs:
  - REQ-TASKBOARD-RETRY
  method: Lose a create reply, retry the retained request, then submit the same ID with changed intent
    through the host.
  expected_result: Retry preserves ID/intent and returns the canonical record; changed intent fails; new
    creates remain blocked until retained attempt clears.
- acceptance_id: ACC-TASKBOARD-HOST
  requirement_refs:
  - REQ-TASKBOARD-HOST
  method: Inspect and exercise the embedding host adapter with authorized/unauthorized scopes, invalid
    writers and canonical record responses.
  expected_result: Host owns state/authorization/effects, validates writer contracts and provides canonical
    response records; module does not add sharing authority.
- acceptance_id: ACC-TASKBOARD-PACKAGE
  requirement_refs:
  - REQ-TASKBOARD-PACKAGE
  method: Inspect publication contents and pinned host adoption proof; exercise saved/draft and downgrade
    recovery under the host contract.
  expected_result: Published content follows the allowlist; adoption and downgrade are supported by declared
    host proof; drafts are not presented as durable.
features:
- feature_id: FEAT-TASKBOARD-DOMAIN
  title: Deterministic domain rules
  requirement_refs:
  - REQ-TASKBOARD-TITLE
  - REQ-TASKBOARD-BODY-STATUS
  - REQ-TASKBOARD-REVISION
  - REQ-TASKBOARD-LEGACY
  primitive_refs: *id001
- feature_id: FEAT-TASKBOARD-BOARD-UI
  title: Host-composed board UI
  requirement_refs:
  - REQ-TASKBOARD-COMPOSE
  - REQ-TASKBOARD-VIEW
  - REQ-TASKBOARD-EDIT
  primitive_refs: *id002
- feature_id: FEAT-TASKBOARD-RECOVERY
  title: Scoped mutation and retry recovery
  requirement_refs:
  - REQ-TASKBOARD-SCOPE
  - REQ-TASKBOARD-RETRY
  primitive_refs: *id003
- feature_id: FEAT-TASKBOARD-HOST
  title: Host ownership and compatibility
  requirement_refs:
  - REQ-TASKBOARD-HOST
  - REQ-TASKBOARD-PACKAGE
  primitive_refs: *id004
primitives:
- primitive_id: PRIM-TASKBOARD-VALIDATE
  title: validate_content
  contract: Validate title, body and status deterministically without authorization, persistence or external
    effects.
- primitive_id: PRIM-TASKBOARD-REVISION
  title: next_revision
  contract: Reject stale/exhausted revisions and produce the next bounded revision.
- primitive_id: PRIM-TASKBOARD-LOAD
  title: load_scoped_board
  contract: Read catalogue/tasks through the host request function and update visible state only for the
    live generation.
- primitive_id: PRIM-TASKBOARD-MUTATE
  title: mutate_scoped_taskboard
  contract: Submit guarded host mutations, preserve retained create identity/intent, and apply visible
    results only to the live generation.
primitive_feature_relations:
- relation_id: REL-TASKBOARD-DOMAIN-VALIDATE
  primitive_ref: PRIM-TASKBOARD-VALIDATE
  feature_ref: FEAT-TASKBOARD-DOMAIN
  relationship: implements_or_supports
  constraint_refs: []
- relation_id: REL-TASKBOARD-DOMAIN-REVISION
  primitive_ref: PRIM-TASKBOARD-REVISION
  feature_ref: FEAT-TASKBOARD-DOMAIN
  relationship: implements_or_supports
  constraint_refs: []
- relation_id: REL-TASKBOARD-BOARD-UI-LOAD
  primitive_ref: PRIM-TASKBOARD-LOAD
  feature_ref: FEAT-TASKBOARD-BOARD-UI
  relationship: implements_or_supports
  constraint_refs: []
- relation_id: REL-TASKBOARD-BOARD-UI-MUTATE
  primitive_ref: PRIM-TASKBOARD-MUTATE
  feature_ref: FEAT-TASKBOARD-BOARD-UI
  relationship: implements_or_supports
  constraint_refs: []
- relation_id: REL-TASKBOARD-RECOVERY-LOAD
  primitive_ref: PRIM-TASKBOARD-LOAD
  feature_ref: FEAT-TASKBOARD-RECOVERY
  relationship: implements_or_supports
  constraint_refs: []
- relation_id: REL-TASKBOARD-RECOVERY-MUTATE
  primitive_ref: PRIM-TASKBOARD-MUTATE
  feature_ref: FEAT-TASKBOARD-RECOVERY
  relationship: implements_or_supports
  constraint_refs: []
- relation_id: REL-TASKBOARD-HOST-MUTATE
  primitive_ref: PRIM-TASKBOARD-MUTATE
  feature_ref: FEAT-TASKBOARD-HOST
  relationship: implements_or_supports
  constraint_refs: []
gui_design:
- design_id: GUI-TASKBOARD-v1
  applicability: Hosted React component; visual runtime verification remains outstanding.
  visual_design_refs:
  - TOPIC-TASKBOARD-CONTRACT
  interaction_contract_refs:
  - REQ-TASKBOARD-VIEW
  - REQ-TASKBOARD-EDIT
  - REQ-TASKBOARD-SCOPE
  - REQ-TASKBOARD-RETRY
  theme_and_component_refs:
  - TOPIC-TASKBOARD-CONTRACT
  surface_profile_refs:
  - FEAT-TASKBOARD-BOARD-UI
  acceptance_refs:
  - ACC-TASKBOARD-VIEW
  - ACC-TASKBOARD-EDIT
  - ACC-TASKBOARD-SCOPE
  - ACC-TASKBOARD-RETRY
```

