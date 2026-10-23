# handshake_rich_document_model

Framework-independent Rust document model and find scanner. The package owns typed block and inline nodes, rope text, schema validation, atomic transforms, bounded undo history, document positions and selections, JSON conversion, revision-checked edits, and document-wide search.

## Use

```rust
use handshake_rich_document_model::{BlockNode, find_scan::{scan, FindQuery}};

let document = BlockNode::doc(vec![BlockNode::paragraph("hello world")]);
let found = scan(&document, &FindQuery::literal("world"));
assert_eq!(found.matches[0].node_path, vec![0, 0]);
assert_eq!(found.matches[0].char_start, 6);
assert_eq!(found.matches[0].char_end, 11);
```

`BlockNode`, `Child`, `TextLeaf`, `NodeKind`, and `Mark` describe the tree. `RopeText` uses character offsets. `DocPosition` and `Selection` describe model positions and ranges. `apply_transaction` applies typed `Step` values atomically and returns inverse steps; failed validation leaves the original document intact. `UndoManager` owns bounded history. `DocumentEditState::edit` checks the expected revision and commits a cloned candidate only on success. Content changes increment its revision; history-only changes retain it.

`doc_json` converts the model to and from Tiptap-compatible JSONContent. `to_content_json_value` produces a bare document node. `RichDocument` carries the separate `rich_document_v1` schema-version envelope. Conversion and validation failures return the existing typed errors rather than silently accepting invalid documents.

`find_scan::scan` takes a borrowed document and `FindQuery`, returning ordered `FindMatch` values and truncation/error information. Queries support literal or regex matching, case sensitivity and whole-word filtering. Each match identifies a text leaf by `node_path` and a character range within that leaf. `MatchKind` distinguishes prose from code blocks. Empty queries produce no matches; invalid or oversized regexes return an error and clear matches. Zero-length matches are skipped. Scans stop at `MAX_MATCHES` (1000); regex program and DFA size limits are each 1 MiB.

## Host boundary and recovery

The host owns GUI rendering, framework input decoding, highlight presentation, replace commands, account/session authorization, save/load policy, storage, diagnostics and application lifecycle. This package performs no I/O and requires no GUI, backend or async runtime. A truncated scan is a bounded partial result; the host must show that state. Correct an invalid query and rescan. On stale revision, refresh the model snapshot and retry through the host's existing coordination. Preserve the previous host dependency pin until standalone proof and a separate host-adoption check succeed.

## Dependencies and checks

Dependencies are ropey 1.6, serde 1 with derive, serde_json 1, thiserror 2 and regex 1. No repository workspace or nested Git repository is required.

From the containing Assets repository, the assigned proof owner generates the package lock before locked checks and uses an external Cargo target directory:

```text
cargo check --manifest-path Notes/handshake_rich_document_model/Cargo.toml --locked --tests -j 1
cargo test --manifest-path Notes/handshake_rich_document_model/Cargo.toml --locked -j 1
```

Existing model behavior tests remain in their owning modules. Eleven pure scanner tests live in `find_scan`; the native replace-all/undo test remains host-owned. Source assembly alone does not establish compile, runtime, publication or host-integration acceptance.