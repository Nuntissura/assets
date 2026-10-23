---
file_id: handshake-rename-readme
file_kind: package-readme
updated_at: 2026-10-18
---

<topic id="package-manual">

# handshake_rename

Workspace-edit preview and rename application extracted from the existing Handshake MT-048 package. All three Rust files and six inline tests are unchanged. The package uses the canonical `handshake_text_mechanics` at `../../handshake_text_mechanics`. Consumers passing `TextBuffer` must use the same crate identity.

## API and use

```rust
use handshake_rename::{apply_text_edits_to_string, LspRange, TextEdit};

let edits = [TextEdit {
    range: LspRange { start_line: 0, start_char: 4, end_line: 0, end_char: 9 },
    new_text: "total".into(),
}];
let renamed = apply_text_edits_to_string("let value = 1;", &edits)?;
assert_eq!(renamed, "let total = 1;");
# Ok::<(), handshake_rename::EditApplyError>(())
```

`WorkspaceEditPreview::from_lsp` accepts an `lsp_types::WorkspaceEdit` and an open-buffer lookup closure. It supports `changes` and `document_changes`, preferring the latter when present, and sorts files by URI. Resource operations without text edits are skipped. Closed files are read for preview context. A read failure retains their edits with empty hunks. `single_file_fallback` builds a preview from caller-supplied occurrence byte ranges, without discovering occurrences itself. `empty`, `is_empty` and `total_edits` support no-op and summary handling.

`FileEditPreview` contains a URI, open-buffer flag, edits and `PreviewHunk` values. `TextEdit` contains a `LspRange` and replacement string. Despite its LSP-style field names, MT-048 interprets character columns as BYTE columns, not UTF-16 units. This extraction preserves that law. Apply resolves ranges against the original buffer and sorts them by descending start byte, then descending end byte. It does not add overlap detection or change Unicode behavior.

`apply_text_edits_to_string` returns a new string. `apply_text_edits_to_buffer` mutates a canonical `TextBuffer`. Invalid ranges or failed buffer edits return `EditApplyError`. Retain original content for recovery, since the mutable-buffer API has no rollback guarantee after mutation starts.

`apply_preview` accepts read/write closures for open buffers and directly reads/writes closed files. Missing open-buffer text defaults to an empty string. Closed-file writes use `write_file_atomic`, which writes a sibling temporary file, flushes it, attempts file sync, then calls `std::fs::rename`; sync errors are ignored. `RenameApplyReport` records successfully applied file URIs and edit count. `RenameError::{Io, BadRange}` carries a partial report and stops further files. Earlier successful writes are retained; there is no cross-file rollback. Preserve original file content and inspect the partial report before recovery or retry. Preview construction alone does not apply edits.

## Boundaries and source proof

Caller-owned host code supplies occurrence discovery, user decisions, open-buffer storage, authorization and undo. This package has no GUI or Core/native dependency. Dependencies are canonical text mechanics and `lsp-types = "0.95"`; the existing dev dependency is `tempfile = "3"`.

This assignment produced source only. No compilation, tests, lock generation or runtime file writes were executed. Required standalone proof, normal publication and independent canonical remote reread precede separate immutable host adoption. Existing host source remains the recovery point until adoption is independently proven. No source acceptance or host integration is claimed here.

</topic>
