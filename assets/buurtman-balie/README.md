---
file_id: CRM-MANUAL-v1
file_kind: product-manual
updated_at: 2026-10-11
---

<topic id="purpose">

# Buurtman Balie

Standalone Rust library and reusable browser field editor for operator CRM workflows across host applications.
Source lives in the `buurtman-balie` subfolder of the assets repository. `src/contracts.rs` owns public types/ports,
`src/service.rs` owns CRM behavior, `src/memory.rs` is an ephemeral adapter, and `web/field-editor.js` owns
the reusable catalogue editor. No Portal implementation imports, account tables or deployment paths.

Implemented: customers, organizations, support cases, interactions, descriptive products and distributors;
typed custom fields; tags; related-record references; fuzzy name/tag search; equality field filters;
workspace isolation, record revision fencing, operation replay and bounded state.

Stock, checkout, orders, attachments, usage and authentication remain authoritative in their host owners.
CRM now owns editable product price books and client pricing tiers; a checkout host must validate the quoted revisions
and authorize price use at its own sale commit boundary.
Use opaque external references for those resources. No Tilroy, Pricebuddy or photo-site integration is implemented.
The standalone screen demonstrates product fields and filtering against the real Rust service. Its data is temporary.

</topic>

<topic id="provisional-portal-workload" ingestable="true">

## Provisional Portal measurement

The host-demo `portal_workload` release example exercises the actual CRM and shared worker with temporary MemoryStore data. Its selected P1 profile uses 100 seeded records and 2,100 search/read/create attempts at 1, 4 and 16 callers. Separate injected faults check queue expiry and receipt replay after a committed response is lost. Revision conflicts are reported without hidden retries.

On Windows, after the repository preparation gate:

```powershell
cargo build --locked --offline -p buurtman-balie-host-demo --release --example portal_workload
python tooling/portal-workload.py --executable target/release/examples/portal_workload.exe
python tooling/portal-workload.py --profile P2 --executable target/release/examples/portal_workload.exe
```

P1 uses Store-call admission; P2 queues complete authorized CRM calls through `CommandWorker` with the same provisional workload. P2 directly rereads the shared raw MemoryStore after drain and exposes the full disposable snapshot for receipt reconciliation. There are no hidden retries. Keep historical P1 run artifacts when comparing results.

The monitor validates the selected specification profile, raw outcomes and canonical counters, records own-process CPU and peak working set, and retains source/binary hashes and cleanup evidence under ignored `.local/portal-workload/runs`. The latest result is `.local/portal-workload/result.json`. A failed measurement retains its error there; inspect it before retrying. The runner waits for the monitor's stdin release so the final OS resource read precedes exit; use the monitor to run it.

The 120-second and 512-MiB diagnostic limits are provisional. These measurements do not establish a production Portal SLA, durable database performance or camera-store/supermarket host compatibility. x64 execution under ARM64 Windows emulation must be identified separately; macOS/Linux remain untested.

</topic>

<topic id="surrealdb-storage" ingestable="true">

## Selected SurrealDB storage

Feature `surreal` exposes `surreal::SurrealStore<Q>` without adding a database SDK dependency to the core.
Implement `surreal::SurrealQuery` using the host's existing authorized database connection; bind the provided
object to `$params` and execute the entire batch in one request in the configured namespace/database.
Inspect every statement result, including COMMIT. Map `BB_STALE_REVISION` and confirmed aborted write
conflicts to `Error::Conflict`; unknown transport/commit outcomes remain `Error::Storage`.
Do not retry changed intent or discard database statement failures.

```rust
let limits = buurtman_balie::Limits::default();
let store = buurtman_balie::surreal::SurrealStore::new(host_query, limits.snapshot_bytes)?;
let crm = buurtman_balie::Crm::new(store, host_authorization, limits);
```

Reserve `bb_workspaces` in the host-selected database. One deterministic workspace record stores layout version 1,
canonical decimal-text revision and a JSON-string schema-2 payload. Immediate ownership remains the business
workspace; host account/profile IDs map through explicit authorized host grants, not through contact identity.
CAS checks both revision and previous payload inside one transaction and commits the complete next snapshot,
including operation receipts. Revisions, pricing amounts and full-range integer field values retain their exact types.

Before the first load/write, the host provisioning owner creates the dedicated table in its selected
namespace/database using authorized provisioning credentials:

```sql
DEFINE TABLE IF NOT EXISTS bb_workspaces SCHEMALESS PERMISSIONS NONE;
```

On the tested SurrealDB 3.3.0 server, an absent table is an error, not an empty workspace.
`PERMISSIONS NONE` denies record-user access; the host must deliberately provision the privileged server-side
transport permissions. Never expose that connection to browser clients. The core does not create namespaces,
databases, users or tables or select host grants. The isolated proof uses its own root credentials.

The query port is synchronous. Async hosts adapt through a blocking worker with a bounded queue; do not call
it on the event-loop thread or re-enter a runtime. Hosts own TLS/authentication, request and response timeouts,
encoded byte budgets, workspace provisioning, durable database settings and backups. CAS transmits previous
and next payloads together, with JSON escaping overhead, so the wire budget must exceed a single snapshot budget.
The adapter bounds decoded snapshot bytes; it cannot bound the transport's initial response allocation.
No workspace-count limit is inferred. Provision bounded business workspaces through the host owner.

Failed or incompatible loads do not become an empty workspace. Unknown database layout/snapshot schema,
malformed payload, revision mismatch and over-budget payload are explicit errors. There is no schema migration,
receipt pruning or implicit cross-business sharing. A lost response is reconciled by the identical request/actor/workspace
after restart; stale revisions require reload and a new logical operation. Backups/restore must include canonical
snapshots and replay receipts; restoring older state loses newer commits and their replay protection.

`tooling/surreal-proof.py` runs a disposable disk-backed server from an explicit executable path, verifies actual
Rust Store/service calls, restarts only its own server and independently rereads canonical data. It uses no host
database or business credentials. Its test transport is diagnostic tooling, not a production connection implementation.
Its direct SQL rereads check receipt outcomes, paired fields/receipts and full-range revision text independently
of the Rust reader. `RETURN` ends query execution on the tested server; a negative probe must throw before
returning. Every result returned by the host transport still needs error inspection before success is reported.
SurrealDB is the selected first backend; PostgreSQL follows under WP-BB-007. Portal, retail and photo-service
adapters and immutable host release pins remain separate acceptance work.

</topic>

<topic id="durable-storage" ingestable="true">

## Optional SQLite storage

Enable Cargo feature `sqlite` to use `buurtman_balie::sqlite::SqliteStore`. The default dependency closure
keeps SQLite out of hosts supplying their own Store. Bundled rusqlite 0.40.2 is pinned in Cargo.lock;
building the feature requires the platform C compiler used by libsqlite3-sys.

```rust
let limits = buurtman_balie::Limits::default();
let store = buurtman_balie::sqlite::SqliteStore::open(
    database_path, 3, limits.snapshot_bytes,
)?;
let crm = buurtman_balie::Crm::new(store, host_authorization, limits);
```

The host supplies a dedicated database path on a local filesystem, an existing parent directory,
workspace capacity and per-snapshot byte budget. No profile or drive path is hardcoded. Empty paths
and `:memory:` are rejected. Hosts own directory/file permissions, authenticated grants and backup retention;
raw Store access is privileged infrastructure and does not authorize callers.

Database layout version 1/application ID 1111642674 stores schema-2 snapshots in `bb_workspaces`, keyed
by workspace. Revision is canonical decimal text, preserving the entire u64 range. Snapshot and revision
are checked together; oversized, malformed, incompatible or mismatched snapshots fail without replacement.
Unknown database versions and existing unrelated databases are rejected. No older-data migration is implemented.

An immediate SQLite transaction serializes CAS, workspace-capacity checks and whole-snapshot replacement.
Records, fields, tiers, relations and operation receipts commit together. FULL synchronous commits use SQLite
filesystem guarantees; process-exit rollback/restart is tested, hardware/power-loss durability is not physically tested.
Locks wait at most two seconds, then return Storage; hosts should keep database calls off a GUI/event-loop thread.
Conflict requires reload/new intent; uncertain Storage outcomes require identical-operation reconciliation.
No receipts are silently removed. This bounded snapshot adapter rewrites the workspace per commit; it is not
an indexed large-catalogue backend or a network-filesystem/shared-server adapter.

For a backup, quiesce writers and close every connection before copying the database; preserve any remaining
SQLite journal sidecars as part of recovery. Never copy an active database as a backup. Restore only while closed,
to a separate path first, and reconcile snapshots/receipts before replacing the original. A restored old backup
loses later commits, including their replay protection. Database files and sidecars belong outside package inputs.

Affected proof commands:

```powershell
cargo test --locked --offline --all-features
cargo clippy --locked --offline --all-targets --all-features -- -D warnings
```

The temporary browser harness still uses MemoryStore. SQLite disk/process proof does not establish Portal,
retail or photo-service host integration. Exact host release pins remain outstanding.

</topic>

<topic id="run" ingestable="true">

## Build and operate

From this module root, with Rust 1.98.1 and Python 3 available:

```powershell
cargo test --locked --offline
cargo clippy --locked --offline --all-targets -- -D warnings
cargo run -p buurtman-balie-host-demo --locked --offline --example standalone
cargo build -p buurtman-balie-host-demo --locked --offline --example json_bridge
python tooling/standalone.py
```

The loopback harness prints its actual URL. Open it to create a reusable field, assign values to products,
save products, select a field filter and search. Click a product result to edit its same canonical record.
No credentials are required for the temporary demo. Stop the harness to discard its data.
`--offline` needs the exact dependencies already cached; otherwise acquire the Cargo.lock dependencies before running.
Generated artifacts use Cargo's relocatable target directory. The browser harness expects its example binary
under the module's default `target/debug/examples`; an overridden target directory needs an explicit binary-path adjustment.

UI proof accepts a Playwright runtime directory and Chromium executable as command-line paths:

```powershell
python tooling/browser-proof.py --runtime <playwright-site-packages> --browser <chromium-executable>
```

</topic>

<topic id="embedding">

## Host contract

The host supplies the production GUI, whether a website admin panel or custom app.
The root `buurtman-balie` crate contains the independent Rust core; the separate
`crates/buurtman-balie-host-demo` workspace member contains temporary demo composition.
The optional `buurtman-balie-web` package supplies reusable presentation components,
which hosts may use or replace. Neither presentation nor demo assets ship in the core package.
Workspace default commands select only the core; select the demo member explicitly.

Windows x64 and ARM64 are the required first runtime targets. macOS and Linux are
later targets; compatibility requires separate execution evidence. A cross-build alone
does not prove a runtime works. Set `BB_DEMO_BINARY` to an explicit bridge executable
when using another target directory or architecture; the default is the workspace's
`target/debug/examples/json_bridge` executable.

Construct `Crm::new(store, authorization, limits)`. Hosts authenticate the actor, supply a workspace context,
and implement `Authorization::allows` from current grants on every call. Workspace read permission exposes
all CRM fields in that business workspace: this is an operator/staff API, not a customer-facing field permission system.
Do not grant ordinary Portal customers access to the operator business workspace. Host adapters redact staff-only
fields before any public/customer projection and authorize external-reference resolution independently.

`Store::load` returns one coherent schema-v2 workspace snapshot. `compare_exchange` atomically checks the old
workspace revision and commits the next revision, records, field catalogue, links and operation receipts together.
It must return `Conflict` on a stale revision. A host database adapter must enforce this across processes,
provide durable transactions and verify real restart/response-loss behavior before production integration.
The provided MemoryStore does not persist and is not a production storage adapter.

Create uses record revision zero; update uses the last read record revision. IDs are host-supplied ASCII
identifiers, at most 128 bytes. The service does not invent time or identities. Retry an uncertain operation
using the identical actor, workspace, request and operation ID; changed intent under the same ID is rejected.
Conflict requires reload and a new logical operation. Failed validation commits nothing. Storage errors can
mean an unknown commit outcome; reconcile the same request before changing its intent.

One field definition per key is stored in the workspace's reusable catalogue. `applies_to` lists allowed record kinds.
Definitions cannot be retyped/replaced in v1; conflicting redefinition is rejected to protect existing values.
Record values and search filters use the same Text/Integer/Boolean types. Search filters are cumulative equality
conditions. Integer wire values are canonical signed-64-bit decimal strings, preserving precision in JavaScript.
Missing values do not match. Filter metadata is the catalogue, not a separate UI-maintained list.
Search order is distance then record ID; cursors bind workspace, query, tag, field conditions, kind and revision.
Changed state invalidates the cursor. Total is the full matching set; hits are only the selected page.

For reusable UI, call `mountFieldEditor(root, adapter, {onChange})`. Adapter methods are `listFields()` and
`defineField(definition, operationId)` promises. Serialize `Command::DefineField` through the host API.
`onChange(fields)` receives canonical definitions after reads. Dispose before removing/replacing the workspace;
late responses are discarded. The field editor never stores credentials or accesses a database directly.
Host styles may override the CRM CSS variables. `web/app.js` and `tooling/standalone.py` are demo composition,
not production bindings; Portal should import the field editor and provide its authorized adapter.

</topic>

<topic id="collaboration" ingestable="true">

## Optional CRDT description drafts

The separate `crates/buurtman-balie-collaboration` crate uses pinned Yrs 0.28.0
for volatile plain-text description drafts. It adds no dependency or GUI to the core.
Hosts supply a workspace/record/generation `Domain`, unique replica IDs and explicit
`Limits` for text, update and encoded state bytes. Cloned `Draft` handles share one
synchronized replica; `Draft::fork` creates a distinct replica from an owned checkpoint.

Use `splice_utf8` with complete character boundaries, or `splice_utf16` with complete
surrogate boundaries. Pass the resulting opaque `DraftUpdate` to `merge`; duplicate
and reordered updates are supported. `has_missing_updates` identifies incomplete causal
delivery. Budget or domain rejection leaves the accepted state unchanged.

Updates and checkpoints have private provenance and cannot be imported from arbitrary
bytes or deserialized. They are temporary in-process tokens, not a network or disk format.
Host authorization, durable acknowledgment, replica allocation, retention, recovery and
resource-safe binary ingress remain outstanding. Publish a description explicitly through
the core's revision-checked update and receipt reconciliation. Prices, stock, client
classification and tiers retain their existing authoritative owners.

```powershell
cargo test -p buurtman-balie-collaboration --locked --offline
cargo test -p buurtman-balie-collaboration --locked --offline --target x86_64-pc-windows-msvc
```

The component tests cover Unicode, quota rejection, partitions, duplicates, reordered
delivery, concurrent inserts/deletes and simultaneous shared-handle readers/writers.
These checks establish the component boundary; they do not establish durable host sync.

</topic>

<topic id="limits-recovery">

## Diagnostics and recovery

Default limits per workspace: 10,000 records, 50,000 links, 128 catalogue fields, 20,000 operation receipts
and 16 MiB of serialized snapshot data, including receipts. The first exhausted budget rejects the whole write.
Names are limited to 128 characters; descriptions to 8,192; text field values to 2,048; tags to 32 entries of
64 characters. Search text is at most 64 characters and pages contain 1-100 results. Search scans bounded
workspace snapshots; it is an initial implementation, not a large-catalogue indexed search engine.

Capacity returns a typed error. Receipt exhaustion stops new writes while reads and known retries still work;
receipts are never silently pruned. Hosts must choose a deliberate retention/migration policy before sustained production use.
Operation receipts retain actor/request/outcome for reconciliation and can contain business data. Keep them private.
Record revisions, workspace revisions, typed errors and the example's JSON responses provide inspection surfaces.
Production hosts own bounded durable diagnostics, supervision and lifecycle. There is no production process in this library.

`worker::WorkerStore::new(store, config)` optionally moves a supplied `Store` onto one persistent worker.
Cloneable handles share admission, with required `WorkerConfig` pending-count, submitted-byte,
per-request-byte and queue/caller-deadline budgets. Calls block their caller; async hosts supply a bounded
blocking bridge and transport deadlines. `pressure()` exposes counters without business payloads.
`close()` stops admission and drains unexpired accepted calls; it neither joins nor cancels an active commit.
Queued expiry never executes. An active write's caller timeout returns uncertain `Storage`, retaining
its capacity until completion; reconcile the identical CRM request. Admission is volatile, and submitted
byte accounting does not bound response sizes or total process memory. Hosts retain durable intent,
response limits and aggregate cross-process admission. Production workload/latency budgets remain unselected.

`worker::CommandWorker::new(raw_store, authorization, limits, config)` instead owns one `Crm` and
serializes complete `execute`, `get`, search, quote, catalogue and relationship calls. Clones share the
same queue. This prevents local load/commit interleaving among participating commands; stale edits,
duplicate creates and external writers still return conflicts. Authorization runs when each call is
dispatched, so queued work does not retain revoked grants. The host must supply live grant checks.
Use the raw Store here rather than nesting a Store worker; retain finite host transport timeouts.
Submitted bytes count the Context and method arguments, not snapshots returned or the host RPC envelope.
The same expiry, uncertain write, close and pressure rules apply. After `Storage`, preserve the identical
Context/Request for explicit reconciliation; never silently retry changed intent. Calls block their
caller, so async hosts still need bounded invocation before dispatching onto blocking threads.

The browser harness accepts bounded local requests, validates Origin, serializes bridge access and exposes no production
authentication or durable storage. On bridge timeout, restart it; never interpret an uncertain save as definitely failed.
Saved records, pending forms and screenshots in the demo must not be treated as real host integration proof.

</topic>

<topic id="release-research">

## Release and research

Candidate version 0.2.0 / API 2 / snapshot schema 2. Schema 1 is rejected; no automatic client classification is inferred. Cargo.lock pins the dependency closure; rust-toolchain.toml
pins the toolchain. Cargo's include list and the browser package's files list allowlist publication inputs.
Private state, local configuration, screenshots, target output and gameplans are excluded. Both packages are
private/unpublished; no public module license or immutable source release has been selected yet.
Before host consumption, pin an immutable source revision/artifact, verify package contents and preserve the previous pin.
No schema migration is implemented: reject incompatible snapshots. Upgrades need a recoverable backup and must
settle uncertain operations before rollback; restoring an earlier snapshot can lose later commits and replay receipts.

Original implementation informed by [900CRM's independent Rust core](https://github.com/900Labs/900CRM/blob/main/crates/crm-core/src/lib.rs),
[EspoCRM case relations and optimistic concurrency](https://github.com/espocrm/espocrm/blob/master/application/Espo/Modules/Crm/Resources/metadata/entityDefs/Case.json)
and [Twenty field metadata and filter relationships](https://github.com/twentyhq/twenty/blob/main/packages/twenty-server/src/engine/metadata-modules/field-metadata/field-metadata.entity.ts).
No upstream CRM code is copied. SQLite ownership and whole-framework translation were rejected for this module boundary.
Validate standalone isolation/replay/type/filter contracts first, then the actual host persistence, authorization,
field projection, route/context preservation and screenshot/usage owners during integration.

</topic>

<topic id="client-pricing">

## Mandatory client classification and pricing

Every Customer must have ClientTerms with exactly B2C or B2B classification; create/update rejects missing status.
This fixed status is separate from editable custom fields and is always displayed in the client screen and selector.
Standard pricing follows the classification. A named extra tier can use a B2C, B2B or named Custom base with one
additional percentage discount. For example a B2C employee can receive B2B pricing plus the configured employee
percentage without changing their mandatory classification. No employee discount amount is preconfigured.

Product PriceBook holds B2C, B2B and named custom prices with an explicit currency, 0-6 decimal scale and common
tax-included/excluded basis. Amounts use exact decimal-string minor units. Missing selected prices cause rejection;
there is no fallback or automatic cheapest-price selection. Quotes show the selected base, discount, final unit price,
classification, tier and source revisions. Discount is rounded half-up to a minor unit; tiers do not implicitly stack.
Tax rates, currency conversion, quantity promotions, sale commits and distributor-cost inference are not implemented.

Use Crm::pricing_tiers and Command::DefinePricingTier for the workspace tier catalogue, and Crm::quote for an
authorized same-workspace calculation. Reusable web/client-pricing.js composes client assignment, tier creation
and price preview through injected public adapter methods. Tier definitions are immutable in this version; use a
new key for changed rules, then explicitly reassign clients. Product/client updates use their existing revision fences.

Before migrating any older snapshot, explicitly classify existing clients, preserve a recoverable backup and settle
in-flight operations. Schema 1 cannot be loaded by API 2; keep its original source pin for rollback. No production
migration was performed. Persistent host storage and Portal integration remain outstanding.

Research: [Medusa customer-group price lists](https://docs.medusajs.com/user-guide/price-lists) and its
[price-list model](https://github.com/medusajs/medusa/blob/develop/packages/modules/pricing/src/models/price-list.ts)
separate customer groups from reusable product pricing. This module uses original typed Rust contracts, not copied code.

</topic>

<topic id="repository-governance">

## Model-owned repository workflow

The [repository workflow reference](repo-governance/codex/workflows/repository-workflows.md) records devlead/team responsibilities, file ownership, specification topic maintenance and future-session resume steps. Codex links the same reference.

Read AGENTS.md and its linked Codex/build rules/spec/taskboard, then the relevant WP/state. Governance YAML checks require Python 3.12+ with PyYAML. Run `python tooling/check.py` after changes to those files; `--self-test` exercises rejection paths. This check verifies structural consistency, not product correctness or evidence authenticity.

Record new ideas in `repo-governance/specification/current/specification.yaml` first, with stable requirement IDs and accepted/proposed status; derive the WP or WP-stub and taskboard row afterwards. Active WP contracts declare scope, spec references, acceptance and applicable checks. MTs are optional children. Stubs hold unresolved work and do not authorize execution.

For state synchronization, set `sync_pending: true`, write canonical state, update taskboard and affected spec roadmap, then clear the flag. If interrupted, recover from state, inspect current source/Git and finish the pending synchronization before new work. Reuse unchanged valid product proof; do not run a whole template/host suite. Required product checks are listed in the build rules.

`superseded` means replaced by a newer WP/version: keep the old contract/state in archive with `superseded_by` and a reason. `cancelled` means abandoned without replacement and retains a reason. Keep archived entries linked from the taskboard and spec roadmap. No refinement/enrichment, activation, receipts, claims, role machinery or separate verdict records are required. The installed stock bootstrap is retained only in ignored `.local/stock-bootstrap` and is not active authority; future template upgrades must reconcile this lightweight profile explicitly rather than restoring stock rules.

</topic>

<topic id="module-localization" ingestable="true">

## Module language selection

The reusable field editor and client-pricing module accept a host-scoped localization object.
English is the backward-compatible default. Select exactly `en`, `fr` or `nl`; unsupported languages
throw `RangeError` before changing the current language. Hosts can expose their own language toggle later.

```javascript
import { createLocalization } from 'buurtman-balie-web/localization';
const localization = createLocalization('nl');
const fields = mountFieldEditor(fieldRoot, fieldAdapter, { localization, onChange });
const pricing = mountClientPricing(pricingRoot, pricingAdapter, { localization });
// The optional mountUserManual(manualRoot, { localization }) shares the same language control.
localization.setLanguage('fr');
// Each mounted module also exposes setLanguage(language).
```

Sharing the object deliberately links module language changes; separate objects remain independent.
Language changes update module-owned text, accessible names, field type/record labels and quote display
in place. Draft values, selected record IDs, revisions and uncertain-operation IDs remain unchanged.
Dispose mounted modules to unsubscribe before replacing their workspace. The module sets its own `lang`
attribute and never changes the host document language. Operator-authored field labels, names and keys
are displayed unchanged. Canonical `Text`/`Integer`/`Boolean`, record kinds, B2C/B2B and price data stay unchanged.

Money presentation uses BigInt minor units and the selected language's decimal separator; no floating-point
conversion or currency-scale rounding occurs. Discount input accepts a single decimal point or comma in
all languages so switching language cannot change an uncertain save's canonical intent; values retain the
existing exact 0–100 percent / at most two decimal places rule. Language selection never selects a tax jurisdiction.
Known CRM error variants and local validation messages are translated; unknown adapter errors receive a
localized generic message. Hosts retain original error objects for their authorized diagnostics.

The focused headless `tooling/localization-proof.py` takes the same `--runtime` and `--browser` arguments as
the existing browser proof. It exercises all three languages, independent module scopes, desktop/mobile
layout, large exact amounts and identical field/tier/client retries after an injected response loss against
the actual temporary Rust MemoryStore service. It does not prove durable host integration.

Research used [MDN Intl.NumberFormat](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Intl/NumberFormat)
for locale decimal parts and [W3C HTML language declarations](https://www.w3.org/International/questions/qa-html-language-declarations)
for component-scoped language metadata. The original browser modules were preserved; a small dictionary
and in-place bindings avoid adding a framework dependency or remounting active forms.

</topic>

<topic id="atomic-linked-create">

`Command::CreateLinked { record, relations }` creates a record and at most32 outgoing relations in one snapshot CAS and one operation receipt. Every relation source must equal the new record ID. Existing link validation and workspace budgets apply. Invalid links commit neither the record nor its receipt. Retry the identical actor, workspace and request after an uncertain response; changed intent under the same operation ID is rejected. External links remain opaque; hosts independently authorize account and screenshot resolution. Existing API2/schema2 snapshots stay readable; the additive command requires a host source pin containing this variant.

</topic>
