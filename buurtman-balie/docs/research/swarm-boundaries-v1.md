---
file_id: BB-RESEARCH-SWARM-BOUNDARIES-v1
file_kind: research-basis
updated_at: 2026-10-11
---

<topic id="scope-and-result" wp="WP-BB-010-swarm-backpressure-v1" status="researched">

# Swarm admission and recovery boundaries

At initial inspection WP010 was a non-executable stub requiring host admission ownership, queue/in-flight/payload limits, overload behavior, write reconciliation, competing-writer proof and a documented workload with host-selected numeric latency/resource budgets before execution/performance claims. Devlead subsequently promoted the active spec-revision-6 contract for configurable functional admission. The later implementation topic records this authorized continuation; historical research observations below remain time-scoped. This research selects no production numeric budgets.

Inspected on 2026-10-11: paired AGENTS/CLAUDE entry points; linked Codex/build rules/spec revision 4/taskboard; WP010 contract/state; README/repository workflows; `.gameplan-active` and its CRM preparation file; CRM source/tests/proof harness; Portal authority, ADMIN WP/state, manual, composition, database/contracts and Notes storage admission. Git inspection found uncommitted CRM source/governance and unrelated Portal edits; all preserved. No Cargo, browser, package or database proof was run. No product process was stopped.

</topic>

<topic id="inspected-boundaries" wp="WP-BB-010-swarm-backpressure-v1">

## Current source evidence

| Inspected surface | Observed behavior | Contract gap |
| --- | --- | --- |
| `src/contracts.rs`: Store, Request, Receipt, Limits | Synchronous load/CAS; workspace snapshot revision; stable operation ID; receipt stores exact actor/request/outcome. Default snapshot cap is 16 MiB, with count caps. | Limits bound snapshot contents, not queue occupancy, active workers, total resident memory or deadlines. |
| `src/service.rs`: execute/load/search_filtered | Authorization precedes receipt lookup; identical actor/request returns receipt outcome; changed intent rejects. New mutation and receipt share one CAS. No hidden retries. Reads load complete snapshots; search scans/sorts matches. | Same-workspace commands contend even when touching different records. Select workload including snapshot size, search cost and conflict amplification. `&mut self` serializes one instance only. |
| `src/surreal.rs`: SurrealQuery, read, compare_exchange | Host owns synchronous transport/auth/TLS/timeouts/wire limits; CAS rereads and fences revision plus prior payload in one transaction. Encodes prior and next payload. Missing confirmation yields Storage. | No admission handle/deadline is carried through the trait. Snapshot parsing cap occurs after transport has returned a decoded response. Host must bound wire response before buffering/parsing. |
| `tooling/standalone.py` | Demo uses an eight-slot semaphore, bridge lock and socket/reply timeouts. | Local MemoryStore harness is not Portal admission or durable queue proof. Do not reuse its constants as production policy. |
| Portal `product/portal-contracts/src/lib.rs`: Database/DbHandle | Async query interface; shared Arc delegates to injected database implementation. | No explicit queue, capacity reservation or request deadline in this interface. |
| Portal `product/portal-core/src/lib.rs`: Db/query/encode_request | Async reqwest RPC; 20-second client timeout; encoded request rejects above 4,000,000 bytes; JSON LET binding preserves strings; scans failed statement statuses before returning values. | Inspected Db has no queue/semaphore/in-flight admission. `res.json()` has no explicit response byte cap here. BB_STALE_REVISION is not among its recognized marker mappings, so direct reuse needs a CRM error adapter. |
| Portal `product/portal-server/src/lib.rs`, Cargo manifests | state_from_env composes Db/identity/Notes/tags; run_loom_projection_loop delegates to a shell-owned projection worker. No Buurtman dependency or SurrealQuery adapter found in these inspected surfaces. | CRM host injection and sync/async bridge are unselected; a projection worker is not a database admission queue. This is a bounded source finding, not a claim about other checkouts. |
| Portal `product/notes/src/storage.rs`: NotesAdmission/query_accounts | Host account storage allowance from NOTES_ACCOUNT_STORAGE_BYTES; durable generation/token/expiry lease, bounded census, pending estimated bytes, final transactional usage check and conditional release. | Logical retained Notes JSON/blob capacity, not physical DB headroom or generic DB traffic admission. Its table census excludes bb_workspaces. CRM cannot silently join this meter or inherit its lease constants. |

Portal paths above resolve from `../../handshake-portal/` relative to this repository. No private env file or credential values were read. Portal transport constants are inspected constraints, not selected CRM budgets. Default 16 MiB CRM snapshots cannot be assumed to fit its 4,000,000-byte wire cap; even smaller snapshots need exact prior-plus-next encoding validation.

</topic>

<topic id="current-patterns" wp="WP-BB-010-swarm-backpressure-v1">

## Current primary-source research

| Source checked | Pattern and implication |
| --- | --- |
| [Tokio bounded mpsc](https://docs.rs/tokio/latest/tokio/sync/mpsc/fn.channel.html) | Finite message capacity supplies producer backpressure. Also bound upstream waiters and message bytes; a finite channel alone does not bound allocations held by waiting producers. |
| [Tokio spawn_blocking](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html) | Blocking pool queues work at its thread limit; started blocking tasks cannot be aborted. Acquire explicit admission before spawning and retain resource ownership until actual completion. Dedicated threads suit persistent blocking loops. |
| [Tokio Semaphore](https://docs.rs/tokio/latest/tokio/sync/struct.Semaphore.html) | Owned permits support resource lifetimes; fairness and multi-permit waits require deliberate scheduling. A local semaphore is not an aggregate cross-process/database capacity fence. |
| [Tower buffer source](https://docs.rs/tower/latest/src/tower/buffer/service.rs.html), [Buffer API](https://docs.rs/tower/latest/tower/buffer/struct.Buffer.html) | Source uses bounded mpsc, reserves readiness capacity before oneshot allocation, propagates tracing span and shares sender on clone. Readiness reservations can occupy slots before call. Include reserved work in accounting. |
| [Tower concurrency layer source](https://docs.rs/tower/latest/src/tower/limit/concurrency/layer.rs.html), [load shedding](https://docs.rs/tower/latest/tower/load_shed/index.html) | Ordinary layer creates per-service limits; global layer shares an Arc semaphore across services. Load shedding exposes overload when inner readiness is unavailable. Select layer order and process scope explicitly. |
| [SurrealDB query](https://surrealdb.com/docs/reference/rust/methods/query), [transactions](https://surrealdb.com/docs/reference/query-language/language-primitives/transactions) | Parameter binding, statement-result handling and database transactions fit the existing CAS boundary. Transaction documentation does not establish the selected server/engine's durability. |
| [AWS timeout/retry guidance](https://aws.amazon.com/builders-library/timeouts-retries-and-backoff-with-jitter/) | Timeout can accompany completed effects; retries amplify overload. Select one retry owner, bounded attempts/backoff/jitter and idempotent reconciliation. |

Current documentation inspected identifies Tokio 1.53.2 and Tower 0.5.3; Portal lockfile contains Tokio 1.53.1/Tower 0.5.3. These are pattern references, not a dependency upgrade or compatibility verdict. Two attempted GitHub Tower source URLs were unavailable; published docs.rs source supplied implementation evidence. No upstream source was copied. Sources were fetched using public generic URLs without project data disclosure.

Research direction: preserve synchronous host-independent Rust Store and atomic replay; select host admission before expensive snapshot work and a bounded worker/transport bridge. Reuse Portal's injected Database/DbHandle shape and exact string encoding through an explicit CRM adapter, after choosing error mapping and resource ownership. Do not impose Tokio/Tower on the reusable core merely because Portal uses them.

Rejected as sufficient solutions: CRDT replacement without a requirement; one mutex; unbounded spawn_blocking; per-route limits presented as database-wide; request count caps without byte reservations; automatic retries at every layer; ephemeral queued acknowledgment presented as durable commit. A durable submission queue/outbox is an alternative only if the host requires crash-surviving acceptance before CRM commit; select ownership and atomic handoff first.

</topic>

<topic id="required-owner-inputs" wp="WP-BB-010-swarm-backpressure-v1" status="unresolved">

## Inputs needed before executable contract

All numeric values below are UNSELECTED. Existing constants above supply constraints, not answers.

| Input group | Canonical owner facts needed |
| --- | --- |
| Admission topology | Selected host entrypoint/CRM adapter; all call paths including reads/replay/background jobs; account/profile-to-business-workspace authorization mapping; process/replica count; aggregate database budget owner. |
| Workload | Concurrent agents and workspaces; hot-workspace contention; arrival rate and burst duration; read/write/search/replay mix; command sizes; current/max snapshot and receipt sizes; fan-out and reconnect bursts. |
| Queue | Maximum queued plus reserved counts/bytes, upstream waiter bound, global/workspace/actor shares, scheduling/starvation policy, queue expiry, overload error/retry hint, and whether acknowledgment is volatile admission or durable acceptance. |
| Running work | Worker/connection/query concurrency; aggregate across replicas; CPU/parse/search budgets; memory reservations for decoded snapshot, next state, previous/next strings and encoded envelopes; response buffers; reserved health/cancel/reconciliation capacity. |
| Deadlines | End-to-end deadline origin; queue/worker/connect/query/transaction/body-read budgets; remaining-budget propagation; expiry before dispatch; handling of already dispatched writes and started blocking workers. Client timeout must not imply rollback. |
| Recovery | Stable workspace/actor/operation intent plus separate attempt identity; caller-held or durable accepted-intent owner; lost-response reconciliation; single bounded retry owner; receipt retention horizon; backup/restore epoch handling; incompatible schema recovery; shutdown/drain ownership. |
| Performance acceptance | Host-selected response percentiles, sample window/size, failure/overload/conflict reporting, queue-wait vs execution breakdown, CPU/memory/DB resource ceilings, server/engine/hardware/storage configuration and representative data corpus. |
| Diagnostics | Authorized pressure/status owner; queue/reserved/running counts and bytes; oldest age; rejects/deadline expiry/conflicts/retries/unknown outcomes; configuration revision; redacted correlation IDs; health distinct from admission readiness. |

These facts belong to host/runtime policy and the executable WP's acceptance, not arbitrary library defaults. A process-local guard can protect local memory; cross-replica aggregate guarantees require a selected coordinator or explicitly divided host budgets. Never claim the latter from a local permit count.

</topic>

<topic id="recovery-and-proof" wp="WP-BB-010-swarm-backpressure-v1">

## Recovery invariants and validation gaps

Proposed contract controls, subject to owner selection:

- Before dispatch: rejected/full/expired work has no CRM mutation; retain original intent for an explicit retry. Volatile queue admission must not promise crash durability.
- After dispatch: timeout, connection loss or caller cancellation is potentially unknown. Keep worker capacity charged until actual completion; reconcile canonical receipt using unchanged actor/workspace/request/operation ID. Do not release a permit merely because the response future ended.
- Conflict: no blind overwrite. Reload current state, surface stale record intent, and separate an intentionally revised command from retrying uncertain original intent. New intent uses a new operation ID.
- Committed: return persisted outcome; exact retry creates no additional revision. Changed actor/intent rejects; authorization remains current even for replay.
- Restore/receipt exhaustion: preserve snapshot and receipts together; explicitly handle operations committed after the backup before accepting replay. Never prune receipts silently or treat absence after rollback as proof of never-started work. Capacity refusal preserves recoverable accepted intent with its selected owner.

Existing proof inspected: `tests/surreal.rs` and `tooling/surreal-proof.py` contain real-server contention/restart/restore/response-loss/negative probes and independent HTTP canonical rereads. At artifact inspection, the three `.local/surreal-proof-*` roots contained logs but no `result.json`; startup warnings are not passing proof. `.local/independent-review/durable-current.md` additionally identifies missing independent receipt-outcome assertions. Current source/host compatibility and durable acceptance remain NOT_PROVEN here; devlead owns the active proof and fixes. Do not infer a failed or successful active run from this inventory.

Required devlead proof after contract selection: real competing processes on the selected database; exact receipt/outcome canonical reconciliation; queue count/byte saturation including reservations/waiters; maximum encoded request and bounded streamed response; hot-workspace fairness; aggregate replica accounting; deadline before and after dispatch; response-loss/restart replay; worker continuation after caller timeout; revoked grants while queued; receipt exhaustion and controlled restoration; database outage/disk pressure with preserved recoverable writes. Exercise recovery capacity while normal admission is saturated.

Measure representative workload against the selected numeric budget, including rejected and uncertain outcomes. Existing demo browser proof or a green ordinary Rust suite does not prove these host admission/performance surfaces. This documentation-only change invalidates no runtime proof and needs no shared build/browser/package rerun.

High-return follow-ons within WP010 planning: reuse exact transport encoding for pre-dispatch byte reservations (validate prior/next plus escape overhead); reuse Request/Receipt identities in authorized pressure/reconciliation views (validate unchanged-intent retry and redaction); reserve recovery capacity alongside normal traffic using selected host permits (validate reconciliation under saturation). These close concrete admission gaps without a CRM rewrite; none is implemented or an added budget.

Next devlead action: select the host admission/bridge owner and obtain the input groups above; only then promote WP010 through spec/WP/taskboard authority. No blocker can be declared removed by this research alone.

</topic>

<topic id="portal-first-executable-proposal" wp="WP-BB-010-swarm-backpressure-v1" status="proposed">

## Proposed executable scope after Portal-first clarification

Operator clarification carried through the devlead: implement configurable admission functionality now, with Portal first; retain photo-service/store and supermarket goals. Numeric workload/SLA selection remains a prerequisite to performance acceptance claims, not to building configurable controls. This proposal supersedes the earlier next-action ordering only for that functional implementation; root owns spec-first contract promotion. No product authorization is inferred from this prose.

Proposed files: `src/worker.rs`, its export in `src/lib.rs`, and `tests/worker.rs`; devlead owns README/internal-manual integration, shared proofs and WP/spec/state. Preserve existing Store/SurrealQuery/Crm contracts unless a separately reviewed minimal signature change is necessary. No CRDT, production connection, Portal wiring, package publication, numeric SLA or default operator policy.

### Public shape and ownership

Expose a shared, cloneable Store handle backed by a bounded worker service. Each handle implements Store; submissions are Load(workspace) or CompareExchange(workspace, expected, snapshot). One service owns queue accounting, workers, diagnostics and graceful admission closure. Queued acceptance is explicitly volatile and never called durable commit.

Host supplies validated config with no Default: maximum pending count/bytes, maximum in-flight calls, maximum single submitted payload bytes, queue-wait deadline and caller completion deadline. Reject zero/inconsistent values before starting workers. Durations are host policy for Store calls, not claims that a whole multi-call Crm command meets a latency budget. Pass monotonic deadlines internally; select/document any end-to-end command deadline separately.

For configurable in-flight concurrency, select a host-supplied factory producing independent Store instances that address the same canonical durable database. Construct all instances before accepting work; each is owned by one persistent thread. A single Store instance may instead use exactly one worker. Never clone MemoryStore/SQLite in-memory state and describe separate stores as a shared database; require an explicit shared-state test store for component proof. Factory startup errors close construction cleanly.

Host owns service lifecycle and finite underlying transport timeouts. Because synchronous Store calls cannot be forcibly cancelled, caller timeout cannot promise worker termination or bounded shutdown completion. Document this limitation instead of introducing process kills or hidden detached workers. Portal's async shell must dispatch synchronous CRM operations through its selected bounded bridge; it must not block the Tokio executor with handle calls.

### Admission, deadlines and capacity

Admission checks complete before insertion; full count/bytes, oversized payload or closed service returns Capacity with a stable reason and no Store invocation. Use checked arithmetic and a bounded serializer/counting writer for payload sizing; avoid allocating an unbounded encoded clone before rejecting it. Declare exactly what pending bytes count: submitted workspace metadata plus serialized next snapshot and protocol overhead. This is not total process memory or prior-plus-next Surreal RPC size; the transport retains independent wire/response caps, and in-flight results/snapshot amplification require selected host resource budgets.

Under one queue/state lock, accepted work moves queued -> running or queued -> expired. Before calling the underlying Store, workers compare the queue deadline and mark dispatch atomically. Queued-expired CompareExchange never invokes Store. Caller expiry races against that same dispatch state: still queued is definitive never-started expiry; dispatched CompareExchange returns Storage with an uncertain-outcome diagnostic. Never report definitive cancellation after dispatch. Load timeout returns Storage without implying a write occurred.

Keep in-flight count and resource reservation until the underlying call actually returns, even after its caller timed out or dropped. Dropping the reply receiver does not free a worker slot or spawn replacement work. Reject or bound producers rather than leaving arbitrarily many waiters holding snapshots. Share counters across all cloned handles; describe these as service/process-local limits, not cross-replica limits. Bound ready replies as well as pending inputs and discard abandoned replies promptly without discarding canonical commits.

### Recovery and observable state

No automatic retry in worker, Store or Crm. Preserve existing transactional CAS and receipt commit. A caller receiving uncertain Storage retains the original Context and Request; later identical `Crm::execute` rereads canonical state and either returns its receipt or attempts unchanged intent under the original revision rules. Changed actor/intent still rejects. The host retains recoverable intent outside the volatile queue; no journal/outbox ownership is invented by this worker.

Provide an owned immutable diagnostic snapshot: configured limits, pending count/bytes, running count, accepted/rejected/expired/completed/uncertain counters, oldest queued age and admission open/closed state. Distinguish queued expiry from dispatched timeout; saturate diagnostic counters rather than overflowing. Avoid business payloads, credentials and unbounded per-operation history. Host controls projection authorization and log retention.

Store submissions lack actor/grant context: this worker cannot prove authorization revalidation at dispatch. Existing Crm authorization remains per call before Store access. If the promoted contract requires revoked grants while a command waits, root must select command-level authorized admission/dispatch or a host authorization fence; do not claim a Store queue supplies that guarantee.

### Functional acceptance and devlead proof

1. Cloned handles share one pending/in-flight budget; fake Store gates deterministically expose running and pending occupancy, overload and byte-cap rejection without timing-only assertions.
2. Oversized/full/closed/expired queued CAS never reaches underlying Store; dispatch/expiry races yield only definitive never-started or uncertain-dispatched results. No permit leak on rejection, reply drop, worker error or completed timeout.
3. A dispatched CAS commits after caller timeout while diagnostics remain charged; identical Crm Request returns the stored receipt with unchanged revision and changed intent rejects. Use a shared canonical component store; devlead separately runs selected real SurrealDB response-loss/restart/contention proof.
4. Load and CAS errors propagate; queue/call deadlines are independently exercised; no hidden retry, new thread per request, busy polling or silent receipt pruning. Closed admission rejects new work and documented drain behavior preserves queued-write expiry rules.
5. Focused Rust test/fmt/lint and applicable package checks are devlead-owned. Functional proof is explicitly separate from production Portal adapter proof, cross-process admission guarantees and workload latency/resource acceptance. No performance PASS until selected numeric budgets are measured.

Contract promotion should retain the unresolved numeric-budget/performance work and future host goals visibly, while authorizing only this configurable functional boundary. Root should decide the Store-factory versus single-worker construction and exact submitted-byte definition before assigning implementation; these determine honest concurrency/memory guarantees.

</topic>

<topic id="single-worker-implementation-handoff" wp="WP-BB-010-swarm-backpressure-v1" status="implemented-unverified">

## Assigned implementation, awaiting devlead checks

Devlead selected one supplied `Store + Send + 'static`, one persistent thread and explicit per-host policy. Read the promoted active WP010 contract/state at spec revision 6 before writing. Root owns any pending multi-file governance synchronization, exports, manual changes and shared checks.

Added `src/worker.rs`: cloneable WorkerStore implements existing synchronous Store; WorkerConfig has no Default and requires pending_count, max_request_bytes, outstanding_bytes, queue_deadline and caller_deadline. Count serialized submissions using a bounded counting writer: load `[workspace]`; CAS `[workspace, expected, snapshot]`. Account pending/running/ready submitted bytes and cap queued plus ready/running slots; reject immediately on admission overload. This accounting excludes decoded response size and transport envelopes; hosts retain response, wire and allocation limits.

One shared state lock fences dispatch versus timeout. Queued expired writes are removed/skipped before Store invocation. Dispatched writes timing out return Storage/uncertain; running capacity remains charged until true completion. A returned reply retains its reservation until consumed, preventing unlimited completed-reply accumulation. All handles share metadata-only pressure counters; close/last-handle drop stop admission and drain unexpired queued work. No per-request thread, automatic retry or forced join. `worker_alive` exposes eventual exit; a blocked underlying Store still requires its host timeout. Catch Store panic, close admission and resolve never-dispatched queued callers without attempting more Store calls.

Added eight focused `tests/worker.rs` cases for invalid config, competing handles/count overload/close-drain, exact byte accounting and oversized snapshots, queue expiry, never-started caller expiry versus still-running capacity, actual Crm receipt reconciliation after committed-response timeout, unchanged error propagation, and panic closure with queued work. Gates control canonical shared MemoryStore effects; tests independently assert expected receipt actor/request/outcome/revision. Synthetic deadlines/capacities in tests are component proof inputs, not operator budgets.

NOT_RUN: compilation, formatting, lint, runtime tests, package and real SurrealDB proof. Files are ready for root export/format/check integration; no passing verdict is claimed. Existing source still owns authorization/revisions/receipts; Store worker cannot revalidate grants at dispatch or provide cross-replica admission, durable queue acknowledgment, or workload/SLA acceptance. Only assigned worker/tests/research files were written; no commits/remotes/deployment/publication or provider memory.

</topic>
