---
file_id: BB-RESEARCH-POSTGRESQL-REUSE-v1
file_kind: research-basis
updated_at: 2026-10-11
---

<topic id="scope-and-current-evidence" wp="WP-BB-007-postgresql-storage-v1" status="researched">

# PostgreSQL storage reuse

Research only; preserve the original Rust core. WP007 remains a planned stub dependent on WP001 SurrealDB proof. This document supplies candidate comparison and missing execution inputs; it does not promote the stub, select a production endpoint, or certify compatibility.

Initial inspection covered local authority: AGENTS.md/CLAUDE.md, Codex/build rules, specification revision 4, taskboard, repository workflow, README, WP007 contract/state and WP001 state. Follow-up inspected specification revision11 and current WP001/WP007 state: WP007 still references revision3, is a planned stub and depends on WP001; WP001 remains in_progress with real Surreal durability recorded and final GUI/engineering closure outstanding. WP007 has no verification entries. Earlier Git observations below are historical, not a fresh status claim. All existing files are preserved.

Inspected `docs/research/durable-reuse-v1.md`, `Cargo.toml`, `rust-toolchain.toml`, `src/contracts.rs`, `src/service.rs`, `src/surreal.rs`, `tests/surreal.rs` and `tooling/surreal-proof.py`. `Store` is synchronous; `load(&self)` returns one schema-2 snapshot. `compare_exchange` commits records/catalogues/relations/pricing and actor/request/outcome receipts together. Service authorization precedes storage; identical retries return the saved outcome, changed actor/intent rejects. Surreal CAS fences both revision and prior payload inside its transaction. PostgreSQL should implement `Store` separately; SurrealQL and its multi-result transport are not reusable SQL interfaces. Host identities, checkout, stock, orders and analytics remain canonical outside CRM.

Preparation resumed by reading `.gameplan-active` and its active GP-026 resume step; authority/source/Git and synchronization inspected. No costly command, build, browser proof or process lifecycle operation performed.

</topic>

<topic id="pinned-synchronous-executable-boundary" wp="WP-BB-007-postgresql-storage-v1" status="proposed">

## Exact driver and Rust API selection

Follow-up found neither postgres0.19.14 source nor archive in the inspected local Cargo registry. [Published API](https://docs.rs/postgres/0.19.14/postgres/) and release-tag sources were inspected instead; no package downloaded/resolved. The [tagged manifest](https://github.com/rust-postgres/rust-postgres/blob/postgres-v0.19.14/postgres/Cargo.toml) declares edition2024/MSRV1.85 and tokio-postgres0.7.18/Tokio requirements. Recommend optional exact `postgres=0.19.14`, no conversion features needed for strings/byte arrays; root verifies resolved transitive compatibility. Keep default core independent of this driver.

Candidate `PostgresStore::from_client(postgres::Client, PostgresLimits, PostgresTimeouts) -> Result<Self, Error>` consumes an already connected host-selected client. Internally use `Mutex<Option<Client>>`; `load(&self)` acquires with `try_lock` and reports pressure instead of an unbounded mutex wait. `compare_exchange(&mut self)` uses the same ownership guard throughout transaction. Poison/discarded connection returns Storage; no hidden reconnect or retry. Host constructs a replacement store after an uncertain failure, using fresh canonical receipt reconciliation. Existing WP010 worker can own this blocking Store; an async host must not invoke it on its executor thread.

[Pinned Client source](https://github.com/rust-postgres/rust-postgres/blob/postgres-v0.19.14/postgres/src/client.rs) supplies `query_opt`, `execute` affected-row count, `transaction` and `is_valid(Duration)`. Use `Row::try_get`, bound parameters and static SQL. `is_valid` only times its own probe, not subsequent queries. No generic query deadline was found. [Pinned Config source](https://github.com/rust-postgres/rust-postgres/blob/postgres-v0.19.14/postgres/src/config.rs) makes a current-thread runtime; connect_timeout applies separately to socket addresses and defaults unlimited. TCP user timeout is platform-dependent. Neither setting establishes an end-to-end Windows request deadline. Supply server statement/lock/transaction deadlines and WP010 caller pressure deadlines; retain the limitation that lost network responses can hold the blocking lane. A fully bounded network transport needs separate host proof/selection.

[Pinned Transaction source](https://github.com/rust-postgres/rust-postgres/blob/postgres-v0.19.14/postgres/src/transaction.rs) commits by waiting on the underlying commit future. Drop waits on rollback and ignores its error. Explicitly rollback on zero-row rejection/statement failure; observe rollback result. After failed rollback, closed connection, timeout/cancellation or ambiguous commit, remove/discard the owned client. Successful rollback proves the rejected transaction aborted; failed/unknown cleanup remains Storage. A commit error is never automatic Conflict or safe retry. The caller must not interpret a worker timeout as rollback.

</topic>

<topic id="ddl-and-proof-selection" wp="WP-BB-007-postgresql-storage-v1" status="proposed">

## Concrete row/fencing recommendation for contract promotion

Select schema-qualified static `bb_crm.bb_workspaces`: workspace TEXT COLLATE "C" primary key; layout_version INTEGER NOT NULL CHECK(layout_version=1); revision TEXT COLLATE "C" NOT NULL; payload BYTEA NOT NULL. Use canonical decimal revision validated by Rust `u64` parse plus exact `to_string` agreement; no BIGINT conversion. BYTEA equality fences original serialized UTF8 payload bytes without JSON normalization or collation ambiguity. This refines the earlier text proposal; opaque byte payload preserves existing schema2/receipt semantics. Provision through a separately owned migration; opening a store validates layout, never overwrites an existing table or infers ownership from its name.

Read one row with layout, revision, `octet_length(payload)` and `CASE WHEN octet_length(payload)<=$2 THEN payload ELSE NULL END`; bind the budget as checked i64. Check returned length/layout before deserializing, then schema2 and revision agreement. This prevents transferring the complete oversized payload through an ordinary valid table row; it is not a general PostgreSQL protocol allocation ceiling.

CAS validates next schema, expected.checked_add(1), serialized payload budget before transaction. Read and retain exact prior bytes; stale expected returns Conflict after confirmed rollback. Existing row conditional UPDATE fences workspace/layout/revision/prior BYTEA; absent expected0 uses INSERT ON CONFLICT DO NOTHING. Exactly one affected row followed by successful COMMIT is success. Zero rows plus successful rollback is Conflict. All SQL failures are Storage unless the contract explicitly recognizes a proven aborted concurrency SQLSTATE; keeping them Storage is a safe first-unit policy. No receipt exists outside the atomic payload.

For a global workspace-count budget, counting alone races across processes. Provision a singleton layout/capacity metadata row and lock it with SELECT FOR UPDATE before creation; recheck existence/count under that lock. Store the canonical capacity in metadata and validate the runtime profile so inconsistent hosts cannot bypass it. Existing-row CAS need not acquire the global creation lock. Tests must include two distinct-workspace creators contending for the final slot as well as same-workspace create/update races.

Apply bound `set_config(name,value,false)` for explicit [statement_timeout, lock_timeout, idle_in_transaction_session_timeout and transaction_timeout](https://www.postgresql.org/docs/18/runtime-config-client.html), plus synchronous_commit=on for the selected private proof. Use a dedicated consumed client, inspect effective settings, and do not return it to a shared pool. No caller data enters batch_execute SQL. Rollback itself can block during network loss; server-side timeouts do not prove client-side socket deadlines. Cancellation is advisory and must be followed by observed cleanup/discard, never a rollback claim.

Root can select these isolated-proof values without production host input: private schema/role, UTF8 logged cluster, available loopback port, generated credentials, pinned portable runtime, explicit fixture budgets and finite server deadlines, no-TLS loopback-only connection, independent psql reread, restore root and response-loss wrapper after real commit. These are proof configurations, not production SLAs. Root must first independently pass the WP001 predecessor and promote spec/WP007.

Production host input remains: canonical owner/authorization/workspace mapping; endpoint/role/migration authority; TLS verification provider and secrets; pool/proxy/failover behavior; numeric workload/pressure limits and network failure containment; durability/backup policy and release pins. Missing production choices do not block a private reference-adapter proof, but block production compatibility claims. Required proof also includes timeout rollback/discard/reconnect, same-revision prior-payload tampering, malformed/oversized rows, global capacity race, committed-response loss replay, denied writes, crash/restart and separate restore. No manifest/product/runtime/build mutation was made in this research follow-up.

</topic>

<topic id="driver-comparison" wp="WP-BB-007-postgresql-storage-v1">

## Source-level candidates, inspected 2026-10-11

Branch URLs below are inspected research references, not immutable release pins. Manifest versions describe those sources, not a verified published dependency closure.

| Candidate / primary source | Observed implementation | Fit and tradeoff |
| --- | --- | --- |
| `postgres`: [manifest](https://github.com/rust-postgres/rust-postgres/blob/master/postgres/Cargo.toml), [client](https://github.com/rust-postgres/rust-postgres/blob/master/postgres/src/client.rs), [transaction](https://github.com/rust-postgres/rust-postgres/blob/master/postgres/src/transaction.rs) | Manifest 0.19.14, Rust 1.85, MIT/Apache-2.0; depends on tokio-postgres/Tokio. Mutable client methods bind `$n` parameters and return affected-row counts. Transaction commit waits on the underlying async commit; drop attempts rollback. | Preferred candidate for a synchronous reference adapter. Fits crate MSRV declaration, but transitive compatibility remains unproved. `load(&self)` needs host-managed connection checkout/interior synchronization. Run on bounded blocking workers; this is not a runtime-free driver. |
| `tokio-postgres`: [manifest](https://github.com/rust-postgres/rust-postgres/blob/master/tokio-postgres/Cargo.toml), [connection lifecycle](https://github.com/rust-postgres/rust-postgres/blob/master/tokio-postgres/src/lib.rs), [transaction](https://github.com/rust-postgres/rust-postgres/blob/master/tokio-postgres/src/transaction.rs) | Manifest 0.7.18, Rust 1.85, MIT/Apache-2.0. Connection future drives database communication; host must supervise it. Async commit sends COMMIT and awaits completion; dropped transactions request rollback. TLS is supplied externally. | Candidate when a host already owns this runtime/client. Requires an explicit bridge to synchronous Store; do not create nested runtimes or block executor threads. No pool/admission policy is supplied by these inspected boundaries. |
| SQLx: [manifest](https://github.com/transact-rs/sqlx/blob/main/Cargo.toml), [PostgreSQL transaction manager](https://github.com/transact-rs/sqlx/blob/main/sqlx-postgres/src/transaction.rs), [pool options](https://github.com/transact-rs/sqlx/blob/main/sqlx-core/src/pool/options.rs) | Manifest 0.9.0, Rust 1.94, MIT/Apache-2.0; default features include macros/migrations/JSON. Transaction manager awaits commit and queues rollback; BEGIN guard handles cancellation before readiness. Pool exposes connection maxima/acquisition timeout and hooks. | Useful host-owned option where already selected. Unconditional adoption changes core Rust 1.85 compatibility and adds async/runtime decisions. Pool bounds alone do not bound all pending application requests. Select only necessary runtime/TLS/PostgreSQL features; macro/migration machinery is unnecessary for a small bound-SQL adapter. |

Selected research direction: retain database-driver-free Store contracts and narrow host-owned PostgreSQL transport/connection ownership; use `postgres` as the provisional synchronous proof candidate. Exact release, features, TLS provider, dependency closure and host runtime remain open. Reject full CRM/ORM replacement, mandatory SQLx, implicit migration ownership, and unbound SQL interpolation. Existing snapshot/receipt and exact-pricing contracts can be reused without adopting an upstream CRM database schema.

</topic>

<topic id="transaction-design" wp="WP-BB-007-postgresql-storage-v1" status="proposed">

## Proposed adapter semantics

Keep one canonical, logged workspace row containing layout version, full-range revision and opaque serialized schema-2 payload. Table/schema ownership and DDL are unresolved. Preserve receipts inside that same payload. Validate row layout, bounded payload, schema and revision agreement on load; absent rows return the existing default snapshot. Bind data; never interpolate workspace identifiers or payloads.

Candidate CAS: read prior payload; reject unexpected revision; then perform a conditional UPDATE fenced by workspace, layout, expected revision and exact previous payload in a transaction. For an absent workspace, only expected zero may INSERT; uniqueness on workspace and `ON CONFLICT DO NOTHING` yields a stale result for a competing creator. Require exactly one affected row and successful transaction completion. No change survives a rejected CAS. This is a proposal requiring actual races, not an implemented guarantee. PostgreSQL documents rechecking an UPDATE predicate after a competing update commits under [Read Committed](https://www.postgresql.org/docs/18/transaction-iso.html); [INSERT](https://www.postgresql.org/docs/18/sql-insert.html) documents conflict handling and privileges.

Do not store u64 revision in signed BIGINT: PostgreSQL's [numeric ranges](https://www.postgresql.org/docs/18/datatype-numeric.html) cannot represent the entire u64 range in BIGINT. Candidate canonical decimal text follows Surreal's representation; numeric(20,0) with explicit range checks/string conversion is an alternative. Select one before DDL. Opaque text preserves payload-byte fencing; [jsonb](https://www.postgresql.org/docs/18/datatype-json.html) changes textual representation, so replacing payload text with jsonb requires revisiting equality and roundtrip proof. No float conversion or receipt pruning.

Classify proven zero-row CAS and proven aborted concurrency failures as Conflict; other errors retain actionable storage diagnostics. Never treat a connection loss/cancellation near COMMIT as definite rollback. Reconcile identical actor/workspace/request/operation ID through a fresh canonical connection. Stop at the first statement failure; rollback/discard unusable connections. A successful earlier statement or RETURNING row is insufficient confirmation of commit.

</topic>

<topic id="host-prerequisites" wp="WP-BB-007-postgresql-storage-v1" status="unresolved">

## Exact unresolved inputs

No PostgreSQL endpoint, credentials, installation, listener or authenticated connection was probed. The assignment selects no host to inspect/connect. NOT_INSPECTED: live host configuration and readiness. Required inputs before execution:

| Input | Required selection / independent observation |
| --- | --- |
| Host port/interface | Host project and PostgreSQL owner, existing driver/pool/runtime, authenticated workspace/grant mapping, allowed connection checkout/worker bridge for synchronous Store. |
| Network port/config | Explicit host/socket and actual port, database/schema and role; independently authenticate and reread server/database/role identity. [5432 is a documented default](https://www.postgresql.org/docs/18/runtime-config-connection.html), not a discovered local endpoint. Inspect TLS verification, certificate roots, proxy/pooler mode and prepared-statement support. Keep secrets in host-local settings. |
| Ownership/provisioning | Reserved table, uniqueness/fencing types, migration owner, schema/search-path control and runtime grants. Separate provisioning privilege from SELECT/INSERT/UPDATE; define workspace isolation and any RLS/session-reset policy. |
| Durability/recovery | Exact server/version/storage, logged tables, transaction/session settings and backup owner. Inspect `fsync`, `full_page_writes`, `synchronous_commit` and replication/failover semantics. [WAL documentation](https://www.postgresql.org/docs/18/runtime-config-wal.html) explains why asynchronous acknowledgement may lose recently acknowledged commits after crash. |
| Bounded pressure | Connection/in-flight/pending/workspace/payload/wire budgets; acquisition/connect/query/lock/transaction deadlines, cancellation/discard rules and overload diagnostics. Numeric workload/latency targets remain host-selected under WP010. |
| Disposable proof | Explicit existing server tools or isolated test facility, private data root, loopback endpoint/role, lifecycle ownership, independent reread path, fault-injection method and source/artifact pin. Devlead owns execution; no existing process may be stopped by this research agent. |

</topic>

<topic id="validation-and-handoff" wp="WP-BB-007-postgresql-storage-v1">

## Existing proof and required boundary

Opened `.local/ui-proof/result.json`, `.local/localization-proof/result.json` and `.local/manual-proof/result.json`: each identifies ephemeral MemoryStore composition. They provide no PostgreSQL durability proof. This research does not recertify visual behavior or current-source freshness from their result flags. During the initial inspection WP001 state recorded persistence as unproved and the three existing `.local/surreal-proof-*` directories had no result verdict. On follow-up, opened `.local/surreal-proof-77dfd4a30174b2b1/result.json`: it records `passed`, SurrealDB 3.3.0 Windows x86_64, executable SHA-256 `3e5e9d363a352be81f5a983d53f0bc0cd77a90e305f01e2fed929987cf79735c`, independent canonical reread, disk crash/restart, separate restore, two-process CAS/atomic receipts, stale rejection, response-loss replay, auth denial, malformed/capacity/u64 checks and pricing/links preservation. Devlead reports final static/package/governance checks pending; this research agent did not rerun the live proof or close WP001. PostgreSQL proof remains absent.

Future PostgreSQL proof inputs must support these independent observations:

- Two processes/connections start from absence or the same existing revision; exactly one wins each create/update race. Canonical reread proves no mixed records/receipts or loser receipt.
- Seed through actual Rust Crm/Store, restart the owned disk-backed server, reconnect independently and compare complete snapshots plus exact quotes, field catalogue, relations and receipts. Restore a consistent backup separately; distinguish orderly restart, crash recovery and restore results.
- Inject response loss after a real committed transaction; return Storage, then restart and retry identical intent. Canonical revision/payload remain unchanged, saved outcome returns, changed actor/intent rejects. Separately fault before commit and prove no record/receipt survives.
- Reject stale revisions, malformed layout/schema/revision, exhausted snapshot/receipt budgets and denied authorization. Reread exact target to prove no mutation; verify workspace isolation and full u64/decimal pricing boundaries.
- Hold locks/exhaust admissions and exercise cancellation: bounded rejection, no silent retries/loss, no transaction leaked to the next borrower. Capture effective settings, server/version, source pin and actual diagnostics; a synthetic transport response is not persistence proof.

Cheap additions when this adapter is implemented: reuse the existing durable scenario matrix across both databases, but run separate real-server proof; record effective settings with each run to prevent false durability claims; include a post-cancellation connection-reuse probe to catch leaked transactions. These reuse current contracts/harness concepts and require actual PostgreSQL observations.

Only this file was authored. No product, contract, task-state or package edit; no Cargo/browser checks, commits, remotes or process stops. Research handoff is complete; PostgreSQL execution remains subject to devlead predecessor acceptance and the selections above.

</topic>

<topic id="portable-windows-runtime" wp="WP-BB-007-postgresql-storage-v1" status="researched">

## Portable runtime follow-up, 2026-10-11

Bounded local inventory: `Get-Command postgres,pg_ctl,initdb,psql` returned no commands. `C:/Program Files/PostgreSQL`, `C:/tools` and `C:/Users/iljas/.codex/bin` do not exist. No PostgreSQL-named directories appeared in the inspected Scoop apps, Chocolatey packages, user Programs or project `.local` directory listings. `.local/tools.yaml` and `.local/host-profile.yaml` contain inactive starter placeholders, not usable PostgreSQL paths. This bounded search is not an exhaustive disk inventory. Missing PATH is not a development blocker: portable acquisition is available.

The [PostgreSQL Windows page](https://www.postgresql.org/download/windows/) explicitly directs advanced users to ZIP binaries without an installer. The [EDB binary page](https://www.enterprisedb.com/download-postgresql-binaries) lists 18.6; its inspected HTML identifies Windows x86-64 download [file ID 1260609](https://sbp.enterprisedb.com/getfile.jsp?fileid=1260609). Proposed isolated proof runtime:

| Pin | Observed value |
| --- | --- |
| Archive | [postgresql-18.6-5-windows-x64-binaries.zip](https://get.enterprisedb.com/postgresql/postgresql-18.6-5-windows-x64-binaries.zip) |
| Vendor-published SHA-256 | `e2246ba91d22345bc3d017586c09ede52d9df180b1eeb480f050445f1cad84e2` in EDB contributor reply on [EDB issue 733](https://github.com/EnterpriseDB/edb-installers/issues/733), October 6, 2026 |
| Availability observation | Read-only HTTP HEAD: 200, `application/zip`, Content-Length 384620317 bytes. No archive downloaded or hash calculated locally. |
| Core source/license | Official [18.6 source directory](https://www.postgresql.org/ftp/source/v18.6/) and [PostgreSQL License](https://www.postgresql.org/about/licence/). Bundle third-party notices/runtime files require inspection after extraction; the core license is not a bundle-wide license verdict. |

Use a project-relative ignored runtime root and distinct private proof data/log roots; no service registration or global PATH mutation is needed by the proposed launch design. Official [pg_ctl](https://www.postgresql.org/docs/18/app-pg-ctl.html) distinguishes ordinary background start from Windows `register` service mode, supports redirected logs/bounded waits, and documents immediate shutdown causing recovery at next start. Official [initdb](https://www.postgresql.org/docs/18/app-initdb.html) provides explicit data directory, encoding, locale, user and password-file inputs. Actual non-admin startup is NOT_PROVEN until this archive is extracted and launched under the current account; no executable/DLL check was performed here.

Devlead acquisition/proof inputs: verify archive SHA-256 against the vendor value before execution; inspect archive paths for containment and required `bin`/`lib`/`share` runtime layout plus notices; inspect and run the exact `postgres.exe`, `initdb.exe`, `pg_ctl.exe`, `psql.exe` versions. Preserve needed DLLs rather than extracting server EXEs alone. Initialize a fresh UTF-8 private cluster with explicit locale/authentication, generated private credentials and bounded log output. Select an available loopback port and independently authenticate/read database identity and effective durability settings. Capture owned data root/PIDs before restart/crash actions. An existing service must remain untouched. Missing DLLs or initialization failures require the exact diagnostic, not an assumed admin-install requirement. This is a plan for root execution, not a claim that any command has already worked.

</topic>

<topic id="narrow-adapter-selection" wp="WP-BB-007-postgresql-storage-v1" status="proposed">

## Optional driver versus injected transport

Recommend an optional synchronous `postgres` reference adapter for the first PostgreSQL proof, with exact candidate `=0.19.14` subject to published-source/lockfile resolution by devlead. Keep default features/core free of mandatory database drivers. Inspected upstream `Client` offers bound queries, typed parameters and affected-row counts; `Transaction::commit` observes the underlying commit result. This gives the proof an actual PostgreSQL protocol client without rebuilding a query transport. Connection checkout/interior synchronization must satisfy existing `load(&self)`. No pool/runtime/TLS is implicitly imposed on consuming hosts; hosts may supply their own Store implementation.

An injected SQL transport remains a valid host adapter option when an inspected host already owns SQLx/tokio-postgres. Compared with direct optional driver reuse, it needs an explicit atomic operation contract: same connection for BEGIN/CAS/COMMIT, typed bindings, exact affected rows, every statement/commit checked, cancellation/connection-discard behavior and bounded request/response admission. A generic Surreal-style `Vec<Value>` success-marker transport would require additional interpretation and cannot itself prove commit. Reject command-line psql subprocesses as production storage bindings; retain psql only as a separately owned canonical reread tool for proof.

Unresolved before implementation: candidate crate publication and full Rust-1.85 dependency compatibility, exact optional feature/export ownership, connection wrapper and deadlines, TLS feature/provider for real hosts, row DDL/revision representation, and private proof runtime acquisition/startup. Root owns all downloads/builds/runtime proofs. This agent performed only read-only local/network inspection and edits to this document.

</topic>
