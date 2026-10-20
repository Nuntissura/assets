---
file_id: handshake-rich-formatting-readme
file_kind: product-module-manual
updated_at: "2026-10-20"
---

<topic id="purpose-and-dependencies">

# handshake_rich_formatting

Framework-independent Rust commands and input actions for the rich document model.
This library mutates caller-owned documents, selections and undo history. It has no
executable or service to start.

The crate uses `handshake_rich_document_model` from
`https://github.com/Nuntissura/assets.git`, pinned to
`a118e58a2afaad6009567e046422828c99277aa0`. Its other direct dependencies are
`serde_json = "1"` and `thiserror = "2"`. Consumers must use the compatible model
dependency so document and history types have the same Rust type identity.

</topic>

<topic id="public-api">

## Commands and actions

`src/lib.rs` exports two entry points:

- `dispatch(&mut CommandContext, &FormattingCommand) -> Result<(), CommandError>`
  handles formatting, lists, tables, paragraph splitting, backward merging and
  history commands. Construct `CommandContext::new` with mutable `BlockNode`,
  `UndoManager` and `Selection` borrows, followed by an actor ID. Commands may
  update the selection. `is_mark_active` queries selection formatting.
- `apply_action(&mut EditContext, EditAction, &mut dyn ActionHost) -> bool` handles
  insertion, deletion, caret movement, selection and undo/redo. Populate
  `EditContext` with `doc`, `selection`, `undo` and `actor_id` from the host's live
  state. Inspect those values after the call for the resulting document and caret.

`dispatch` returns `Ok(())` for both successful commands and benign no-ops.
`CommandError` distinguishes `InvalidContext`, `NoCaret` and `Transform` errors.
The action API returns a boolean and does not expose transform errors: failed
mutations and empty undo/redo return `false`. Some selection actions return `true`
even when the resulting selection equals its previous value.

Successful insertion and deletion apply canonical model transactions and record
their receipts in the supplied undo manager, using the supplied actor ID with
operator provenance. Caret movement does not create undo entries. Undo/redo change
the document through the model history; this action layer does not restore an
earlier selection snapshot.

</topic>

<topic id="host-integration-and-limits">

## Host responsibilities and limits

Implement `ActionHost` for the embedding editor:

- `prev_grapheme_boundary_chars` and `next_grapheme_boundary_chars` receive leaf
  text and Unicode scalar offsets, not UTF-8 byte offsets. Return whole cluster
  boundaries in the same units. The action engine delegates segmentation policy.
- `merge_backward` receives the live `EditContext`. Apply and record the structural
  transaction there, update the caret and return whether state changed. The public
  `dispatch` API with `FormattingCommand::MergeBackward` supplies the crate's
  structural merge command. Do not record its receipt a second time.

Backward deletion at offset zero invokes the merge callback.
`DeleteBackwardWithoutMerge` suppresses that callback for a rebound merge command.
Forward deletion at the leaf end is a no-op. Horizontal movement stays within the
current leaf. Insertion and deletion operate at the selection head; this action
layer does not replace an arbitrary selected range. Provide a valid text caret:
node selections fall back to document-start positioning in the action engine.

The library contains no GUI event decoding, rendering, authentication, storage,
save-conflict handling or backend authorization. The host supplies those bindings
and retains its own grapheme, merge, undo and integration acceptance tests. Source
tests alone do not establish Native editor acceptance.

On a command error, report the concrete `CommandError` and inspect the live
selection and document context before retrying. Canonical transform failures roll
back their transaction. When `apply_action` returns `false`, distinguish a boundary
or empty-history no-op from an invalid document position in the host; the boolean
does not provide an error diagnostic. Use the existing undo manager for recovery
from an accepted edit, rather than applying the same edit again.

</topic>

<topic id="verification" ingestable="true">

## Source verification

From the canonical Assets repository root, with the committed lockfile:

```sh
cargo check --manifest-path handshake_rich_formatting/Cargo.toml --locked --tests -j 1
cargo test --manifest-path handshake_rich_formatting/Cargo.toml --locked -j 1
```

The owning tests include direct `apply_action` mutation, callback delegation and
undo behavior, plus command tests. These commands verify the source package;
embedding applications still require their own integration proof.

</topic>
