---
file_id: handshake-find-replace-readme
file_kind: module-manual
updated_at: 2026-10-18
---

<topic id="api-and-use">

# Find and replace

`handshake_find_replace` provides the existing in-file search and replacement mechanics from WP-KERNEL-012. It uses `TextBuffer` from the canonical `../../handshake_text_mechanics` package. It does not own a second buffer or cursor implementation.

```rust
use handshake_find_replace::{FindEngine, FindQuery};
use handshake_text_mechanics::buffer::TextBuffer;

let mut buffer = TextBuffer::new("foo bar foo");
let query = FindQuery::literal("foo");
let matches = FindEngine::search(&query, &buffer);
assert_eq!(FindEngine::replace_all(&mut buffer, &matches, "baz"), 2);
assert_eq!(buffer.to_string(), "baz bar baz");
```

`FindQuery` selects literal or regex search, case sensitivity and whole-word filtering. `FindEngine::search` returns matches in ascending byte order. `Match::byte_range` is a half-open byte range; its `line` and `col` are zero-based, with `col` measured in characters. Whole-word filtering uses ASCII alphanumeric characters and underscores as word characters.

Empty or over-limit patterns return no matches. `MAX_PATTERN_LEN` is 512 characters. Invalid regex patterns return no matches; call `FindEngine::compile_error` to obtain their error string. That method reports the length error for regex queries only.

`replace_one` returns whether the replacement succeeded. `replace_all` sorts matches by descending byte offset, skips overlaps and returns the number applied. Re-run search after any replacement because prior match offsets are stale. `REPLACE_ALL_CAP` is 1000 for callers to enforce; `replace_all` does not apply the cap internally.

</topic>

<topic id="dependencies-and-proof">

## Dependencies and verification

The package depends only on `regex = "1"` and the canonical local `handshake_text_mechanics`. Consumers must resolve the same text-mechanics package identity. Native UI controls, storage, authorization, diagnostics and caller state remain host responsibilities.

The source and its 16 inline tests are preserved unchanged from `src/frontend/handshake_find_replace` in the assigned WP-KERNEL-012 product worktree. Only the text-mechanics manifest path changes for the Notes location. No compiler or test run is claimed by this source extraction. Standalone locked source proof, publication and independent canonical remote verification precede a separate pinned host adoption.

For unexpected matches, inspect the query flags and byte ranges. For regex errors, inspect `compile_error`. For failed replacements, re-search the current buffer and discard stale offsets before retrying.

</topic>