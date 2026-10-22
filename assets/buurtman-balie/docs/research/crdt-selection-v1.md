---
file_id: BB-RESEARCH-CRDT-SELECTION-v1
file_kind: research-basis
updated_at: 2026-10-11
---

<topic id="scope-and-recommendation" wp="WP-BB-012-crdt-v1" status="proposed">

# CRDT use and data-race proof

The operator now explicitly requires CRDT use AND data-race tests. This supersedes earlier no-specific-CRDT wording. Current `BB-CRDT-001` and WP012 require implementation/merge ownership selection before execution; WP012 remains a planned stub. This document recommends a concrete selection but neither promotes the stub nor implements a CRDT.

Recommend reusing Yrs for a workspace-and-record-scoped collaborative **plain-text description draft**, corresponding to existing `Record.description`. Share the draft between authorized employees/agents; keep an explicit publish action through existing `Command::Update`, record revision, workspace CAS and operation receipt. A converged draft is not a committed CRM edit. Publish uses the selected draft version and full current record, retains unrelated fields, and rejects a stale record revision. An uncertain publish retries identical original intent rather than substituting a newer draft.

This is a narrow candidate surface with an existing field, revision fence and reusable editor composition. It avoids inventing a notes module or replacing `Record.tags` whole-list replacement with implicit add-wins semantics. Final selection of this draft surface, owner, host and promotion behavior is unresolved. Tags, labels and committed notes remain alternative domains requiring explicit semantics before implementation.

Inspected local boundaries: `src/contracts.rs` exposes records/typed fields and atomic snapshots, not CRDT state; `src/service.rs` validates and authorizes each command and saves receipts with mutations; `src/surreal.rs` commits canonical workspace snapshots using CAS; `web/package.json` exports small injected-adapter modules. No current CRDT state, sync endpoint or browser CRDT dependency was found in these inspected surfaces.

</topic>

<topic id="component-comparison" wp="WP-BB-012-crdt-v1">

## Primary-source component comparison, 2026-10-11

| Component/source inspected | Actual API/configuration and semantics | Embedding/maintenance decision |
| --- | --- | --- |
| [Yrs source](https://github.com/y-crdt/y-crdt/blob/main/yrs/src/lib.rs), [tagged manifest](https://github.com/y-crdt/y-crdt/blob/v0.28.0/yrs/Cargo.toml) | `Doc`, `TextRef`, transaction-scoped edits, state vectors, binary update decode/apply. Same-position sequence changes use deterministic ordering; same-key Map conflicts prioritize client identity. Manifest default features are empty; `sync` changes observer callback constraints, `small-client` supplies older client-ID compatibility. | Recommended text draft component. It reuses an established text CRDT without translating a CRM. Keep optional feature/dependency closure and avoid generic Maps for scalar business decisions. Rust minimum compatibility is not declared by this inspected manifest; dependency-lock/MSRV proof remains required. |
| [Yrs releases](https://github.com/y-crdt/y-crdt/releases) | Latest displayed release is 0.28.0. Changes include subscription API changes and UTF-8 update validation; 0.27.4 lists length-prefix checks/block lookup fixes. | Select a pinned maintained release, not copied CRDT code or floating main. Review relevant decoder fixes on upgrades. Earlier 0.27.2 source was also inspected for API comparison, not recommended as an older security baseline. |
| [Automerge Rust model](https://github.com/automerge/automerge/blob/main/rust/automerge/src/lib.rs), [AutoCommit source](https://github.com/automerge/automerge/blob/main/rust/automerge/src/autocommit.rs), [manifest](https://github.com/automerge/automerge/blob/main/rust/automerge/Cargo.toml) | Nested map/list/text CRDT; `AutoCommit::fork/merge/load/save`; binary storage and sync API. Concurrent scalar conflicts remain retrievable through `ReadDoc::get_all`, despite a deterministic display winner. Main manifest says 0.12.0, Rust 1.90, optional wasm/UTF-index features. | Strong alternative when explicit multi-value scalar conflicts/history are required. Not selected for one plain-text draft: larger document model and declared Rust minimum change versus this crate's 1.85. Main source is implementation evidence, not proof of a released Rust pin. |
| [Automerge JS package](https://github.com/automerge/automerge/blob/main/javascript/package.json), [releases](https://github.com/automerge/automerge/releases) | JS package 3.5.0 is backed by Automerge WASM; release history includes bounded list-patch handling and prototype-key validation fixes. | Browser alternative requires WASM loading, pinned Rust/JS format compatibility and host bundling. JS release numbering does not establish a matching Rust crate pin. |
| [crdts manifest](https://github.com/rust-crdt/rust-crdt/blob/master/Cargo.toml), [ORSWOT source](https://github.com/rust-crdt/rust-crdt/blob/master/src/orswot.rs), [traits](https://github.com/rust-crdt/rust-crdt/blob/master/src/traits.rs) | Manifest 7.3.2; default quickcheck/num/merkle features; Serde state. `Orswot` is add-biased, uses actor dots/read-remove context, `validate_op/validate_merge`, and idempotent apply. Operation replication requires per-actor ordering; state merge is a distinct path. | Useful narrow Rust tag-set candidate, with default features disabled if unnecessary. Not selected for text editing; tags require a declared add/remove policy and browser command protocol or WASM bridge. Do not assume arbitrary operation reordering works because the type is a CRDT. Current release/support cadence not established by inspected sources. |

Source inspection supports these fit judgments; no library build, benchmark or application integration was run. No blanket maintenance/performance verdict is made.

</topic>

<topic id="selected-protocol-and-conflicts" wp="WP-BB-012-crdt-v1">

## Proposed executable boundary

Use one Yrs document with one allowed text root for each `(workspace, record, draft-generation)`. Host-authenticated actor and replica identity are separate: an actor may have several devices/processes, and replicas cannot share a concurrently active client ID. Fresh sessions must not accidentally reuse a restored replica's operation clock. Root schema, generation and record/workspace binding are server-owned admission facts.

The [Yrs document options](https://github.com/y-crdt/y-crdt/blob/v0.27.2/yrs/src/doc.rs) expose offset encoding and unique-client requirements. Configure UTF-16 indexing for browser edits and prove accented French/Dutch text, combining characters and emoji. [Yrs transaction acquisition](https://github.com/y-crdt/y-crdt/blob/v0.27.2/yrs/src/transact.rs) exposes fallible `try_transact_mut`; ordinary native acquisition may block, and wasm acquisition may panic when overlapping. Use short transactions and an owned document worker or bounded admission; never hold a transaction across network/database await or call reentrant edits from its callback. CRDT convergence does not remove memory-locking requirements.

Browser candidate: pinned [Yjs 13.6.27 ESM package](https://github.com/yjs/yjs/blob/v13.6.27/package.json), Yrs 0.28.0 with `small-client` and `sync`, lib0 update-v1 encoding, host-injected transport. This pair is a proposed proof target, not verified interoperability. [Yjs main](https://github.com/yjs/yjs/blob/main/package.json) currently identifies a v14 release candidate; do not silently substitute it. Tests must compare both directions and restored binary state before a pin is accepted.

Text inserts/deletes merge at CRDT positions, not through latest wall-clock replacement. Concurrent insert order is deterministic but may not express human preference; show remote changes and allow deliberate editing. Delete removes observed text; unseen concurrent inserts can remain. Do not silently clear the whole draft when a user intended to remove only visible content. The [Yjs update contract](https://docs.yjs.dev/api/document-updates) describes idempotent/order-independent binary updates and state-vector sync; missing updates still require delivery/reconciliation. Mere merged bytes do not garbage-collect deleted content.

Do not put classification, prices, tier rules, field definitions, case status, revisions, receipts, host identities, stock, checkout or orders into this draft document. They retain current authoritative commands/owners. Publishing a draft must not manufacture their values. CRDT order-independent convergence, database CAS/replay, and actual concurrent data-race safety each need separate proof.

</topic>

<topic id="persistence-admission-and-recovery" wp="WP-BB-012-crdt-v1">

## Persistence, authorization and pressure risks

Recommended storage shape: an explicit collaboration envelope and binary state/update history separate from existing schema-2 CRM snapshot. It avoids an implicit snapshot migration and byte budget inflation; final database ownership/table/protocol is unresolved. If publish and draft-generation acknowledgement must be atomic, define a new authorized storage transaction rather than implying existing Store covers a second table. If independent, report each result separately and recover ambiguous publish via its existing receipt.

Durable acceptance requires server admission and database commit before acknowledgement/broadcast as accepted. A worker can apply admitted updates to a scratch document, validate allowed roots/plain text and budgets, persist, then publish accepted state only after independent decoder resource control is selected. Scratch/postvalidation alone does not contain hostile decode. Reject schema violations without contaminating the live worker. Binary client updates are untrusted even when their wrapper says `description`; enforce document-level ownership plus schema checks. Client IDs, transaction origin, author metadata and document IDs are not authentication. Recheck write grants on reconnect/offline replay; permission revocation can reject previously local edits. Never echo private draft bytes to unauthorized subscribers or logs.

Declare maximum encoded update, decoded text/state, actor count, pending causal state, queued/in-flight bytes, active document count and retention; no numbers are inferred here. A tiny update can trigger large processing/state growth. Retain rejected/unacknowledged work locally with a visible recoverable error; do not silently discard, retry forever, or acknowledge merely in-memory mutations. Reconnect sync is bounded, with deadlines and admission coordinated with WP010.

Backups need full admitted CRDT state and generation/protocol metadata, not only the rendered string. Choose compaction/checkpoint/GC behavior against offline replica lifetime and deletion/undo requirements; test stale reconnect after compaction. Generation changes invalidate old drafts explicitly. No record deletion/recreation or restore should let a stale document resurrect content into a different record. Hosts own transport credentials/TLS and local draft encryption/retention. This work does not establish deployed Portal compatibility.

</topic>

<topic id="acceptance-and-unresolved-inputs" wp="WP-BB-012-crdt-v1">

## Concrete proof before completion

| Proof | Required observable result |
| --- | --- |
| Concurrent text edits | Fork three distinct replicas from one admitted checkpoint; insert same-position text, overlap delete/insert, and edit separate spans. Deliver every binary-update permutation with duplicates, then compare rendered text and state-vector coverage on every replica. Do not require identical serialized byte order unless documented canonical encoding exists. |
| Partition/reconnect | Edit offline on both sides; drop acknowledgements/updates; reconnect via state-vector diff. No accepted edit disappears; no duplicated text; recovered replicas converge. |
| Cross-language/browser | Rust-to-Yjs and Yjs-to-Rust edits, UTF-16/Unicode edge cases, persisted reload and duplicate update replay converge for the exact pinned pair. |
| Actual race safety | Barrier-start simultaneous writers/readers against the same document worker, persistence adapter and two independent processes. Enforce bounded admission and finite completion; prove no panic/deadlock, one coherent admitted durable state and no lost accepted update. Serial convergence fixtures alone do not satisfy this gate. |
| Authoritative publish race | Two publishes race on the same CRM revision: one succeeds, the other gets Conflict; prices/classification/fields/host references remain unchanged. Lose the successful response, restart, retry identical publish and independently reread one record mutation plus receipt. |
| Negative boundary | Malformed/truncated/oversized update, unknown root/type, wrong workspace/record/generation, duplicate client identity, revoked grant and unauthorized sync are rejected without live-state or durable mutation. Re-read canonical storage independently. |
| Pressure/recovery | Saturate bounded queues and state budgets; show explicit pressure and recoverable unacknowledged edits. Restart before/after commit; acknowledged edits survive, unknown outcomes reconcile. Prove stale generation cannot resurrect deleted/retired draft. |

Initial scope selection is now in spec10/WP012: optional Rust-only Yrs0.28.0 description draft with opaque tokens. Retained unresolved full-WP choices: first real host and collaborative actors; browser pins/interoperability; canonical collaboration store and atomic versus independent publication; replica session/generation lifecycle, deletion/GC/retention; host numeric admission/resource budgets and actual process proof commands. These are concrete contract inputs, not permission to omit the accepted CRDT/data-race work.

</topic>

<topic id="opaque-rust-first-unit" wp="WP-BB-012-crdt-v1" status="implemented-unverified">

Root released source execution for `crates/buurtman-balie-collaboration/src/lib.rs` and `tests/draft.rs`; tests/builds remain root-owned and have not run in this lane. Selected manifest dependency is exact `yrs=0.28.0`, `sync`, default features empty; no core dependency or small-client/browser requirement. Host supplies validated workspace/record/generation and nonzero unique 53-bit replica IDs plus text/update/state budgets. No default limits, raw binary construction/import, serde deserialization or mutable Yrs escape hatch exists. `Draft` cloned handles share synchronized ownership; checkpoint fork reconstructs a fresh document with a new replica, rejecting recorded identity reuse. Host remains responsible for independently allocated duplicate identities and authorization.

Both explicitly named UTF8 and UTF16 splice APIs validate scalar boundaries under the same ownership lock, then mutate a freshly reconstructed candidate. Only one plain-text root, string insertion and range deletion are exposed. Opaque tokens contain private crate-generated bytes; typed merge checks exact domain and limit profile, stages independently, checks text/encoded state and provenance metadata budgets, then swaps. State-vector-full encoding includes pending causal updates, so pending bytes count toward the state cap. Rejection preserves the original live document. Replica metadata counts against the state budget, including no-op writers. Encoded budgets bound accepted state; they are not a process memory/runtime SLA or claim about malicious decoder safety.

Inspected exact cached published source: `doc.rs` Doc clone uses Arc-backed storage; ClientID constructor requires 53-bit values. `types/text.rs` insert uses PrelimString, remove deletes text items; `block.rs` ItemContent decoding for generated String/Deleted and update Skip/GC has no nested Any branch. `transaction.rs` full-state encoding retains pending state. Therefore arbitrary nested Any is unreachable through this safe crate-generated token boundary. The generic published decoder still has count-reservation and recursive Any branches: `updates/decoder.rs` Decode::decode_v1, `update.rs` Update::decode, `any.rs` Any::decode. Arbitrary raw ingress, corrupted durable reload, Yjs interoperability and resource isolation remain unresolved. No custom binary parser was added.

Source tests cover simultaneous distinct-replica insert/delete edits, reverse/duplicate delivery, checkpoint forks, shared-handle barrier readers/writers, Unicode/surrogate splits, wrong domain/profile and quota rejection. These are written acceptance cases, not passed proof; root must run and review both Windows target results. Durable authorization/acknowledgment, restart/process races, retention and explicit CRM publication remain outstanding.

</topic>
