---
file_id: BB-RESEARCH-DURABLE-REUSE-v1
file_kind: research-basis
updated_at: 2026-10-11
---

<topic id="decision" wp="WP-BB-001-durable-core-v1" status="researched">

# Durable-core reuse assessment

Preserve the original Rust CRM and reusable browser modules. Select narrow database transport reuse behind `SurrealQuery`; do not replace the CRM with a full upstream framework. This assessment compares inspected code, not upstream feature parity or production-host compatibility. No upstream CRM implementation was adopted.

Local evidence: `src/contracts.rs` injects `Store` and `Authorization`, scopes snapshots by workspace, and represents host-owned resources with `ExternalRef`. `src/service.rs` authorizes each operation, retains exact actor/request/outcome receipts, and commits records plus receipts through revision CAS. `src/surreal.rs` supplies one transaction batch over a synchronous host-owned query transport. `Cargo.toml` keeps Surreal free of mandatory SDK/runtime dependencies and SQLite optional. The browser field/pricing adapters remain independent of database credentials.

These existing boundaries already fit embedded Rust applications and web hosts: hosts retain identities, stock, checkout, orders, usage and images; the CRM owns descriptive records and links. Full-framework adoption would need to replace or bridge those contracts before providing equivalent behavior. Broader CRM features remain in their existing stubs.

</topic>

<topic id="upstream-comparison" wp="WP-BB-001-durable-core-v1">

## Source comparison, inspected 2026-10-11

| Candidate and inspected primary source | Observed integration boundary | Reuse decision |
| --- | --- | --- |
| [Twenty field metadata](https://github.com/twentyhq/twenty/blob/main/packages/twenty-server/src/engine/metadata-modules/field-metadata/field-metadata.entity.ts) | TypeORM entity, workspace metadata, object/field relations, view filters/sorts and application migration imports | Retain the canonical field-catalogue pattern. Do not transplant its server entity/migration machinery into Rust snapshot contracts. |
| [Frappe CRM document API](https://github.com/frappe/crm/blob/develop/crm/api/doc.py) | Python endpoints use Frappe metadata, document controllers, session user, database and saved view configuration; filterable fields derive from metadata. Total counts and returned row counts are distinct. | Reuse metadata-driven filtering and canonical-count distinctions as comparison criteria. Direct API/UI adoption requires the Frappe runtime and host authorization adapter; no such integration is selected. |
| [EspoCRM Case metadata](https://github.com/espocrm/espocrm/blob/master/application/Espo/Modules/Crm/Resources/metadata/entityDefs/Case.json), [EntityManager](https://github.com/espocrm/espocrm/blob/master/application/Espo/ORM/EntityManager.php) | JSON field/status metadata; PHP manager delegates reads/writes to entity repositories and resolves relations through framework services | Keep explicit case states and relation context. Reject copying repository ownership into the host-independent Rust Store surface. |
| [900CRM core declaration](https://github.com/900Labs/900CRM/blob/main/crates/crm-core/src/lib.rs), [storage modules](https://github.com/900Labs/900CRM/blob/main/crates/crm-core/src/storage/mod.rs), [services](https://github.com/900Labs/900CRM/blob/main/crates/crm-core/src/services/mod.rs), [dependencies](https://github.com/900Labs/900CRM/blob/main/crates/crm-core/Cargo.toml) | Tauri-independent Rust core, but storage functions accept rusqlite connections; CrmCore owns Database/device ID and opens an app-data directory. Deal operations transactionally combine relationships, sync and audit. | Useful Rust architecture comparison, especially atomic related changes. Direct adoption brings SQLite/device/application ownership incompatible with selected SurrealDB and injected Store. Preserve the existing optional rusqlite adapter instead. |
| [Medusa price-list model](https://github.com/medusajs/medusa/blob/develop/packages/modules/pricing/src/models/price-list.ts) | TypeScript framework model owns price-list lifecycle dates, status, rules, prices, cascades and indexes | Keep reusable pricing separate from client classification. Reject introducing its commerce lifecycle as CRM authority over host checkout/orders. Current exact Rust pricing contracts stay intact. |

These source observations support selective pattern reuse; they do not establish that an entire upstream product lacks an alternative embedding API. No exhaustive feature or license-compatibility audit was performed, and no source-copy decision is made.

</topic>

<topic id="surreal-transport" wp="WP-BB-001-durable-core-v1">

## Selected library boundary

The [current official Rust SDK overview](https://surrealdb.com/docs/reference/rust) reports SDK 3.3.3, an asynchronous API and Rust 1.95 minimum. Local Cargo metadata declares Rust 1.85 minimum. Therefore adding the current SDK unconditionally would change this crate's host/toolchain contract; an installed newer development toolchain does not remove that compatibility change. SDK selection belongs in a host adapter with an exact pin and chosen server compatibility.

The [query documentation](https://surrealdb.com/docs/reference/rust/methods/query) separates transport/query parsing success from statement success and supports parameter binding. The [versioned SDK query implementation](https://github.com/surrealdb/surrealdb/blob/v2.6.0/crates/sdk/src/api/method/query.rs) directly shows `check()` walking statement results, `take_errors()` removing failures, and bind serialization. The versioned source is implementation evidence, not the selected production version.

Selected approach: preserve `SurrealQuery` as the bounded host transport interface. A host can use its pinned official SDK, bind the complete params object as `params`, check every statement before extracting success values, and map known stale/write-conflict failures to `Conflict`. Unknown connection/commit outcomes stay `Storage` and require identical-operation reconciliation. Do not strip errors and then interpret remaining success rows as a committed transaction. A diagnostic proof transport may test the same Store boundary, but is not a production SDK adapter.

The [official transaction reference](https://surrealdb.com/docs/reference/query-language/language-primitives/transactions) supplies the database transaction pattern. Real server proof must establish the exact server/engine behavior; documentation and mocks cannot prove durable commits.

</topic>

<topic id="risks-and-proof" wp="WP-BB-001-durable-core-v1">

## Risks, controls and affected validation

| Failure scenario | Minimum control and required proof |
| --- | --- |
| A RETURN success value survives while a later statement or COMMIT fails | Transport checks every result; exercise real aborted/stale transaction paths and independently reread the canonical row. |
| Concurrent writers pass a client-side revision check | Revision/payload fence remains inside the database transaction; race independent connections and prove exactly one accepted commit, with no mixed record/receipt state. |
| Commit succeeds but response is lost | Keep actor/workspace/request/operation ID unchanged; restart and prove replay returns the stored outcome without a second mutation. |
| Surreal binding coercion changes identifiers, revision or exact snapshot values | Preserve strings with explicit parameter binding; verify quoted identifiers, full-range revision strings and exact serialized pricing through actual writes/rereads. |
| Whole-snapshot writes exceed request/response limits or block an async executor | Budget prior and next payload plus encoding overhead; run synchronous Store calls through host blocking workers. Do not claim workload latency or backpressure acceptance without selected budgets. |
| Database or receipts become stale after restore | Backup canonical snapshots and receipts together; reject incompatible schema/layout and reconcile uncertain operations before rollback. No automatic migration/pruning is selected. |
| Raw Store access bypasses CRM authorization | Store remains privileged infrastructure; service proves denied calls before database mutation; real hosts separately validate authentication/grants and external-resource resolution. |

Run affected Rust contract/format/lint checks after source edits, package allowlist proof after package-input edits, and the selected `BB-CHECK-SURREAL` proof against an explicitly inspected disposable executable/database. Required durable proof includes restart, competing CAS, atomic receipts, stale rejection, denied service calls, unknown-outcome replay and independent canonical rereads. SQLite and MemoryStore results do not replace SurrealDB proof. This document changes no product behavior and needs no build or browser rerun itself.

</topic>
