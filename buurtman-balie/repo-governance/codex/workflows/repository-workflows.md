---
file_id: BB-REPOSITORY-WORKFLOWS-v1
file_kind: repository-workflow-reference
updated_at: 2026-10-11
---

<topic id="authority-and-resume">

# Resume Buurtman Balie development

Read AGENTS.md, linked Codex/build rules/specification/taskboard, README.md and the relevant WP contract/state. Inspect actual source and Git status before acting. The standalone module is this repository; Portal is a consuming host. Preserve uncommitted files and unrelated work. No remote, deployment or publication is selected.

Recover a WP with `sync_pending: true` before starting new work: read canonical state, finish taskboard/spec roadmap synchronization and clear the flag. Resume from the recorded next action and proof inputs; do not repeat unchanged green checks. Follow the gameplan preparation gate before costly commands. Record actual behavior and blockers, and never close a WP on partial proof.

Current execution starts with WP-BB-001-durable-core-v1: reassess source reuse, then SurrealDB durable core proof. PostgreSQL follows under WP-BB-007-postgresql-storage-v1. Optional SQLite and MemoryStore remain available reference/demo surfaces. Actual host integration and immutable release pins require separate proof.

</topic>

<topic id="team-coordination">

## Teams and file ownership

The devlead orchestrator maintains the requested outcome, sequences work, assigns bounded file ownership, resolves conflicting edits and coordinates independent proof. Use subagents under the available concurrency limit; rotate/respawn teams when slots become free. New chats are authorized assistance, but shared project files remain the source of truth; inspect them again on resume.

| Team | Responsibility | Boundary |
| --- | --- | --- |
| Dependencies | Inspect current upstream source/libraries and host transport boundaries; select narrow reuse and dependency closure | Preserve working original core; no implicit full CRM rewrite or copied framework ownership |
| Diagnostics | Reproduce failures; identify exact blockers; prove restart/CAS/replay/unknown-outcome behavior at the selected database | Independent canonical reread; mocks or SQLite cannot prove SurrealDB acceptance |
| EU-US-Belgium compliance | Research official fiscal/VAT, invoicing, privacy and other applicable CRM rules; record applicability and open host inputs | No blanket compliance verdict; evidence and concrete scope precede fiscal implementation |
| Specification coordinator | Keep topics, requirements, research basis, WP/stubs and taskboard current; split independently addressable topics | Preserve IDs, original intent and prior research; no stock ceremony |
| Coding team | Implement assigned Rust core or reusable presentation files and affected proof | Canonical host identities, stock, checkout, orders, usage and images remain with hosts |
| User-manual coder | Ship the CRM INTERNAL manual for no-context agents/models through a reusable module surface | Coordinate exports/package files with presentation coder; verify instructions against actual APIs |
| CRM use-case research | Inspect real CRM/ERP/POS/HR/warehouse and analytics code/docs; maintain cross-sector use-case and metric-owner matrix | Charts and telemetry implementation remain proposed until canonical sources/adapters are selected |

Assign one writer per file while work overlaps. Share findings before dependent edits. A handoff names inspected files, edits, observed proof, remaining blocker and next action. Any agent may investigate a blocker within the existing WP scope, but must return immediately to the blocked deliverable. Do not repeat failed methods without changed relevant inputs or a distinct evidenced diagnosis. Stop existing work promptly on an operator stop.

Sequence shared Cargo builds and browser harness use: a running GUI bridge can hold its executable open on Windows. During live browser proof, use the affected `--lib --tests` Rust targets when full example linking would rewrite that executable. End only the harness/process started by this session before rebuilding its binary. Assign one owner to shared build/output directories and browser-proof artifacts; independent agents must not race rebuilds or overwrite each other's proof.

</topic>

<topic id="swarm-and-usecase-work">

## Coding, manual and broader research continuation

WP-BB-009-internal-manual-v1 owns the shipped internal user manual. Active WP-BB-010-swarm-backpressure-v1 implements configurable single-worker admission while production numeric workload/response budgets remain unresolved. The current operator correction requires CRDT use AND data-race tests; WP-BB-012-crdt-v1's corrected optional Rust-only Yrs0.28.0 opaque-token description-draft component is implemented with five tests passed on native ARM64 and x64 under ARM64 Windows emulation; independent diagnostics source review records bounded PASS with no reviewer runtime runs. Use checked UTF8 byte offsets, fresh checkpoint-derived scratch Doc and budgeted observable pending causal state; no raw import or Doc.clone isolation. Byte caps and postvalidation do not prove arbitrary decoder memory/stack safety. Raw durable ingress/rehydration, real-host sync, retention, publication and UTF16/interoperability remain outstanding. Preserve authoritative CAS/replay, exact money and host stock ownership.

WP-BB-011-crm-usecase-research-v1 owns `docs/research/crm-usecases-v1.md`: source-backed retail/supermarket daily sales, warehouse, HR, photo-service, SaaS resource/feature usage and Portal RSS/tag behavior research. Distinguish accepted research from proposed analytics implementation. Never prove real sales, resource consumption or popularity with synthetic/demo CRM records; select and reconcile exact canonical metric owners first.

WP-BB-013-operator-workflow-validation-v1 records actual employee/operator scenarios and gaps; temporary demo observations do not prove host compatibility. WP-BB-014-crate-boundary-v1 preserves root reusable Rust core and separates host/demo composition and optional presentation. Hosts provide GUI for website admin panels and custom apps. Windows x64 and ARM64 each require actual execution evidence first; macOS/Linux follow later. Compilation is not runtime proof.

Assume concurrent agents and bounded requests at every host database boundary. Preserve CAS/replay semantics, expose pressure/error state, and reconcile uncertain writes using identical operation intent. Queue, deadline and performance budgets remain owner-controlled until explicitly selected and measured. Team assignments are recoverable coordination notes, not role ceremony or automatic changes to product acceptance.

</topic>

<topic id="specification-maintenance">

## Spec first, then executable work

Record new accepted/proposed meaning in `repo-governance/specification/current/specification.yaml`, then derive a WP or unresolved WP-stub and taskboard row. Requirements declare behavior once; the specification's stable `topics` reference requirement IDs for navigation. Split topics when independent scope needs selection, preserving existing requirement IDs and technical detail. Increment the spec revision for changed meaning and record change history. Review only affected contract/proof scope.

The hsrepotemplate specification template was consulted for topic ownership, stable identities and internal reference resolution only. This repository retains its existing self-contained YAML specification and lightweight Codex workflow. Do not restore refinement, enrichment, activation, receipts, claims, role registrations or stock validation machinery.

Keep Portal, retail, photo-service and fuller CRM stubs visible. Optional MTs belong with their WP and need no empty scaffolding. `superseded` means replaced by a newer version with a replacement reference and reason; `cancelled` means abandoned without replacement and retains its reason.

Before multi-file status changes, set the affected state `sync_pending: true`, update canonical state then taskboard/spec roadmap, and clear the flag after synchronization. Run the affected governance consistency check after the preparation gate; it proves structure, not product behavior.

</topic>

<topic id="localization-and-compliance-boundaries">

## Accepted module language and research work

WP-BB-006-localization-v1 owns English `en`, French `fr` and Dutch `nl` module toggles for later host exposure. Language changes preserve pending user input, canonical identifiers, values, classification and exact pricing. Operator-authored records are not translated. Language does not select fiscal jurisdiction.

WP-BB-008-compliance-research-v1 owns evidence gathering and applicability. Preserve official source URLs, inspection dates, jurisdiction, transaction/category/time conditions and unresolved host inputs. B2B client classification does not prove VAT exemption. Checkout, invoice, identity and other host owners supply canonical transaction and authorization facts; source research does not certify implemented compliance.

Before implementation, compare current field/upstream patterns and actual code boundaries, record selected/rejected reuse, risks, controls and affected validation. Preserve package allowlists so governance, private state and backups stay excluded. Do not publish or deploy without operator authorization.

</topic>
