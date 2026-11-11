# compiled_schema_catalog_entries

Pure, std-only declaration inventory. It reads borrowed schema text and returns sorted identity strings or an error. It does not execute SQL, access a database, read files, use the network, or calculate host source/catalog hashes.

## Public API

```rust
pub fn compiled_schema_catalog_entries(
    schema: &str,
    expected_table_names: &[&str],
    expected_counts: (usize, usize, usize, usize, usize, usize, usize, usize),
) -> Result<Vec<String>, String>;

pub fn production_catalog_entries(
    base_entries: &[String],
    ordered_sources: &[(&str, &str)],
) -> Result<Vec<String>, String>;

pub fn declarative_function_name(signature: &str) -> Result<&str, String>;
pub fn insert_catalog_identity(
    entries: &mut std::collections::BTreeSet<String>,
    identity: String,
) -> Result<(), String>;
```

`compiled_schema_catalog_entries` validates the caller's table set and exact count tuple: tables, fields, indexes, events, views, sequences, accesses, functions. It rejects duplicate identities. The preserved strict policy recognizes `OVERWRITE` tables, fields/indexes/events with `ON TABLE`, `OVERWRITE` functions, and `OVERWRITE`/`IF NOT EXISTS` sequences and database accesses. Unsupported ACCESS/FUNCTION forms reject; other unmatched kinds/forms contribute no strict identity. A `TYPE NORMAL AS` table also contributes a view identity.

`production_catalog_entries` starts from the caller's already validated base identities and visits sources in the supplied order. Labels are caller metadata, not ordering instructions. It accepts plain, `OVERWRITE` and `IF NOT EXISTS` definitions of TABLE, FIELD, INDEX, EVENT, FUNCTION, SEQUENCE, ACCESS, ANALYZER and PARAM. FIELD/INDEX/EVENT accept `ON name` or `ON TABLE name`. Repeated identities deduplicate; base table/count checks and duplicate rejection do not apply to overlays. This is an identity union, not SQL body replacement or database migration.

Both inventories share one recognizer for multiline headers, nested comments, quoted text/identifiers, regex literals and nested statement bodies. Quoted identifier spelling, including delimiters, is retained. Quoted/commented or nested `DEFINE` text is not a top-level declaration. Unclosed lexical regions/delimiters, missing required header parts and unknown overlay kinds return errors. Recognition remains case-sensitive. This is declaration inventory, not full SQL syntax validation.

`declarative_function_name` borrows the prefix before `(` and requires a nonempty `fn::` name. `insert_catalog_identity` inserts an owned identity into the supplied set and reports duplicates. Errors preserve the original helper and strict inventory messages; malformed new header cases include the input declaration or byte offset.

## Host use

Validate the base once, then supply the host's original ordered overlay sources. The host owns table names/counts, source ordering, raw schema bytes, hash algorithms, database INFO checks, migrations and execution. Do not flatten or rewrite schema text to make recognition succeed.

```rust
use compiled_schema_catalog_entries::{compiled_schema_catalog_entries, production_catalog_entries};

let base = compiled_schema_catalog_entries(
    "DEFINE TABLE OVERWRITE items;",
    &["items"],
    (1, 0, 0, 0, 0, 0, 0, 0),
)?;
let entries = production_catalog_entries(
    &base,
    &[("module", "DEFINE FIELD value\nON items TYPE string;")],
)?;
# Ok::<(), String>(())
```

## Proof and lineage

The owning public-API tests are in `tests/declaration_inventory.rs`. They include the actual multiline `engine_profile_registry_transition_guard` declaration and one-line equivalence, all eight strict categories, legacy helper/errors, overlay forms/kinds/deduplication/order, comments/quotes/nesting, and malformed inputs. Tests are authored but **uncompiled and unexecuted** in this external candidate. Cargo, lock resolution, publication and host adoption remain separately owned/admitted.

Original parser lineage: revision `b5273d82666354a0fc2b8d2e5715f12d79777786`, `src/backend/handshake_storage_support/src/compiled_schema_catalog_entries.rs`, SHA256 `0529d603a0c242903a7072d8ded14468633682408370a9f5ae210a9d418cd02a`. Preserve `catalog_primitive_coder` and `schema_contract_readiness` attribution. This standalone source does not itself change or certify a host.
