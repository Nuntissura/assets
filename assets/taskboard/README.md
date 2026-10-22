---
file_id: TASKBOARD-MANUAL-v1
file_kind: product-manual
updated_at: 2026-10-11
---

<topic id="module-contract" version="1">

# Taskboard 0.1.0

The Rust library owns bounded titles, plain-text bodies, status values and revision rules. The React
component owns board selection, task forms, retained create attempts and scoped response fences.
The host owns authentication, account/profile authorization, canonical persistence, quotas, logging
and navigation. Rust has no external dependencies; the browser component requires React 19.

The public Rust API is `src/lib.rs`; the browser API is `web/index.ts`. Compose `Taskboard` with
`scope`, `root`, a typed `request` function, optional `integration` content and optional `targetTaskId`.
Changing scope resets visible state; old replies must not change the new scope.

The HTTP adapter provides `GET/POST root/taskboards`, `GET/POST root/tasks`, `PATCH root/tasks/id`
and `DELETE root/tasks/id?expected_revision=n`. Public board records contain `id,title`; tasks contain
`id,title,description,status,board_id,revision`. Create requests carry a client UUID; repeated requests
with the same ID and original intent return the canonical record. Changed intent is rejected.
Edits/deletes require an exact revision. Update the host and browser adapter together;
older writers without client IDs or expected revisions receive an error and must reload. Unsupported sharing must not widen host ownership.

Portal's adapter is `product/portal-data/src/taskboards.rs`; its composition is
`product/portal-web/src/Board.tsx`. Portal owns data in its taskboards/tasks tables and translates
legacy missing board IDs/revisions to `default`/0 without rewriting stored tasks.

</topic>

<topic id="run-and-recovery">

From this module root, run `cargo test --lib` for domain rules. The Portal host uses
`tooling/check.ps1 -Profile focused -Wp WP-MVP-005-task-board-v1` from its isolated candidate.
The host build compiles the TypeScript source; this module does not contain a standalone web server.
Use stable `data-testid` controls and host diagnostics to reproduce browser operations.

If a create reply is lost, use Retry same request: preserve its ID and intent. After a revision
conflict, reload current tasks before editing again. Plain-text drafts are not durable until saved.
On host downgrade, stop writes and back up the database first: old hosts cannot preserve board
grouping. Reconcile new boards/tasks before returning to the previous host; do not delete rows.

Package only manifest, lockfile, public source, web source, manual and license. Exclude credentials,
operator state, governance, browser evidence, `.local`, `.git`, build outputs and dependency caches.
The local source commit is a candidate, not a published or deployed release. Host releases must pin
the source revision and verify standalone and host compatibility before adoption.

</topic>
