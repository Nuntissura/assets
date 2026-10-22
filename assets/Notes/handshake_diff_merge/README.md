---
file_id: handshake-diff-merge-readme
file_kind: package-readme
updated_at: 2026-10-18
---

<topic id="package-manual">

# handshake_diff_merge

Reusable line diff and base-aligned three-way text merge extracted from the existing Handshake package. The Rust source and its 13 inline tests are unchanged. The package uses the canonical `handshake_text_mechanics` at `../../handshake_text_mechanics`; callers must use that same crate identity for `TextBuffer`.

## API and use

```rust
use handshake_diff_merge::{DiffEngine, MergeChoice, MergeEngine};
use handshake_text_mechanics::buffer::TextBuffer;

let base = TextBuffer::new("first\nold\nlast");
let local = TextBuffer::new("first\nlocal\nlast");
let remote = TextBuffer::new("first\nremote\nlast");
let diff = DiffEngine::diff(&base, &local);
let mut plan = MergeEngine::three_way(&base, &local, &remote);
for block in &mut plan {
    block.chosen = Some(MergeChoice::Local);
}
let merged = MergeEngine::apply(&base, &local, &remote, &plan);
assert_eq!(merged.to_string(), local.to_string());
assert!(!diff.is_empty());
```

`DiffEngine::diff` accepts two buffers. `diff_text` accepts two strings. Both return ordered `DiffBlock` values with half-open line ranges and `DiffStatus::{Equal, Added, Removed, Modified}`. `diff_json_blocks` compares JSON arrays by index and returns block-index ranges; it does not perform format-aware alignment.

`MergeEngine::three_way` accepts base, local and remote buffers and returns changed `MergeBlock` values. Unchanged lines produce no block. Blocks carry base/local/remote ranges, `MergeStatus::{LocalOnly, RemoteOnly, BothSame, Conflict}`, and an optional `MergeChoice::{Local, Remote, Both}`. `apply` returns a new buffer. An unresolved choice preserves both sides, local first. Merge output joins logical lines with LF and does not preserve original line endings or a terminal newline.

## Boundaries and recovery

These APIs return values directly, with no `Result` error channel, storage, processes or GUI dependency. The caller owns input selection, conflict decisions, persistence and undo. Keep the original buffers to recover or recompute a merge plan; applying a plan does not mutate them. Use plans with the buffers from which they were computed. No `TextBuffer` or cursor implementation is copied here. Dependencies are `similar = "2"`, `serde_json = "1"`, and the canonical text-mechanics package.

This extraction is source-only. Its existing tests have not been executed in this assignment, no lock has been generated, and no source acceptance or host integration is claimed. The assigned proof owner must prepare dependency pins and run the required standalone checks before publication acceptance. A separate host-adoption task must consume the independently verified immutable source revision. Existing host sources remain the recovery point until that adoption passes its own proof.

</topic>
