# handshake_rich_document_model

## Rich-fragment replacement research

Checked 2026-10-29: [ProseMirror replacement steps](https://github.com/ProseMirror/prosemirror-transform/blob/master/src/replace_step.ts), [ProseMirror slices and replacement](https://github.com/ProseMirror/prosemirror-model/blob/master/src/replace.ts), and [Slate fragment insertion](https://github.com/ianstormtaylor/slate/blob/main/packages/slate/src/transforms-text/insert-fragment.ts). Both implementations keep range replacement, surrounding content, and resulting positions together. ProseMirror also captures the inverse against the original document and rejects incompatible depths. Its hosted reference documentation returned HTTP 403; the implementation sources were accessible.

This model already has atomic transactions and inverse capture. Reuse those steps instead of adding another mutation or history system. Use explicit inline or closed-block fragments, strict character positions, and reject cross-container text ranges rather than inventing list/table joins. Retain marks and atoms by copying complete children and slicing only endpoint text. Keep identity policy in the caller because block attributes have no schema-defined identity key. Focused proof must cover reversed and multi-run selections, sibling blocks, nested content, invalid input without partial edits, mapped selections, and exact undo/redo.

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

## Rich-fragment replacement

`replace_selection_with_fragment` accepts a captured `Selection`, a `RichFragment::Inline` or `RichFragment::Blocks`, and `FragmentReplaceOptions` containing actor attribution and optional `split_tail_attrs`. It returns one `TransactionReceipt` and the resulting selection. Push a nonempty receipt into the host's existing history inside the same `DocumentEditState::edit` closure; install the returned selection with the same host commit. Empty inline content at a valid collapsed caret preserves the document and selection and returns a zero-step receipt; do not add it to history. The operation does not save, push history, or increment an edit revision itself.

Inline fragments replace runs and atoms within one inline block, or across sibling inline blocks with identical kinds and attributes. Empty inline content deletes the text range. Block fragments contain at least one complete block and replace a whole selected block or a text range whose endpoint blocks share one parent. Prefix/suffix content keeps its marks, atoms, kind, and attributes. Inserted blocks remain separate from those boundaries. Invalid positions, cross-container ranges, incompatible inline boundaries, and schema violations return errors without changing the document.

When block insertion splits one attributed block and both sides survive, the prefix retains its original attributes and `split_tail_attrs` supplies the new suffix's complete attributes. Missing suffix attributes reject that split. Callers own identity keys and must supply fresh identities where their document contract requires them. The model does not generate IDs, rewrite reference targets, or deduplicate opaque attributes. A text-ending block fragment returns a collapsed selection after its last text; a fragment ending in an atom or empty block returns a node selection on its last block.

The host must retain the original asynchronous edit context and compare it immediately before mutation, alongside owner/revision/IME/writing-fence checks. A newly captured current context cannot substitute for the original one. Provider resolution, permission checks, copied-block ID remapping, reference-target policy, shared undo, persistence, and recovery remain host-owned.

## Persisted reference identities

`HsLinkNode` carries optional typed `target_kind: ReferenceTargetKind` and `target_block_id: String` values. The admitted kinds are `note`, `heading`, and `block`; the target note remains `ref_value`. Constructors leave the optional fields absent for existing callers. JSON null maps to absence for these optional link fields; wrong types or unknown target kinds return `InvalidReferenceTargetAttr`.

`NodeKind::HsEmbed` represents the persisted `hsEmbed` block atom. Its attributes retain the embed instance's `block_id`, target note, target kind/block ID, label, resolution state, and provenance. Parsing and schema validation share reference-payload checks, including directly constructed embeds; schema validation also rejects children and inline placement. Structural block attributes already preserve `block_id`, independently of tree paths. The model does not resolve previews or derive persisted IDs from outline paths.

Adding the enum variant requires consumers with exhaustive `NodeKind` matches to handle `HsEmbed`. Consumers constructing `HsLinkNode` literals must initialize the two new fields; `HsLinkNode::new` remains available. This does not establish full Portal schema parity: unsupported kinds, including `taskList` and `callout`, continue to return typed errors.

## Tag identity and labels

The tag compatibility correction follows the inspected producers: native `inline_tags/parser.rs::tag_to_hs_link` stored normalized names with prefixed labels; the host completion candidate stored hub IDs with the same flags; Portal `draft-session.ts::insertReference` stores hub IDs and raw labels. Existing `resolved`, provenance, `rich_document_v1`, and imported metadata do not reliably distinguish these historical values. An ID lookup followed by title fallback can therefore open another hub after a real ID disappears. GUI agreed to an explicit discriminator rather than a value-shape or origin guess.

For an `hsLink` with `refKind=tag`, optional `tag_ref_kind` is `hub_id` or `normalized_name`. `HsLinkNode::tag_hub_id` accepts the exact authorized hub ID and raw full title. `tag_normalized_name` accepts an already canonical identity from the proved legacy grammar and the actual raw `Tag.name`. Both preserve the label exactly, including whitespace, Unicode, and literal leading `#` characters. They do not parse, trim, normalize or truncate it. Legacy `HsLinkNode::new` remains unmarked. Unknown markers or markers on other node types reject through JSON/schema validation.

`tag_reference()` exposes `HubId`, `NormalizedName`, or `Unclassified` with borrowed identity and label values. A marked label is raw text; `display_label()` adds one syntactic `#`, so a real name `#rust` displays as `##rust`. Unmarked values retain their opaque stored label verbatim, or the raw value when that label is empty. They never acquire or lose a textual prefix automatically. Hosts own separate icons, escaping, Markdown export and resolution. Export must preserve raw names and escape its syntax rather than substitute hub IDs.

Host navigation carries the typed identity: marked IDs resolve only by exact authorized ID without title fallback; marked names use the legacy canonical-name resolver with ambiguity handling. Unmarked references remain unclassified; keep explicit legacy-name resolution and authoritative hub selection available without guessing from UUID shape, flags, labels or failed lookups. An explicit confirmed resolution can replace the old reference once through the existing transaction/history owner. Do not infer a raw title by stripping an old label. New Unicode/full-name creation uses backend create/resolve then `HubId`; the existing ASCII/trim grammar is not a new-name creation path.

Canonical publication alone does not authorize marked writes through lossy readers. Native `parser::tag_to_hs_link` and `inline_chip::menu_item_to_hs_link` adopt `NormalizedName` from actual `Tag.name`; authorized completion adopts `HubId` from the selected row. Native validation/events/navigation retain the typed identity. Foreign-host adoption remains separate: Portal `product/notes/web/rich.ts` must admit the marker, preserve it in HsLink attributes/HTML/NodeView, and use raw-label rendering; `draft-session.ts::insertReference` marks only authorized IDs; `markdown.ts` linkText/reconcile preserves known markers without classifying opaque imports. Existing React `app/src/lib/tiptap/hs_link_node.ts` must preserve the marker through attributes/HTML. Existing foreign source was inspected read-only and has not been changed by this asset work. Their adoption and exact missing-ID/title-collision navigation proof remain required before marked writes across those readers.

## Host boundary and recovery

`replace_selection_with_fragment_with_trace` shares the ordinary fragment builder and calls `apply_transaction_with_trace` exactly once, returning `TracedFragmentReplacement { receipt, selection, trace }`. Its document, receipt and selection match the ordinary operation; a valid empty no-op returns an empty trace with original block lineage intact. Errors return no partial result. The trace describes actual emitted node steps: a replaced node maps to `None` even if a new node carries identical attributes, while an untouched original sibling maps past inserted copies with duplicate IDs. This adds no implicit identity assignment or host policy and no extra undo transaction.

`apply_transaction_with_trace` uses the same step loop and final-only schema validation as `apply_transaction`, returning the existing receipt plus a `TransactionTrace` only on success. It never replays or separately validates intermediate prefixes. `trace.steps()` has one geometry entry per forward step, including merge pre-step child counts/actual boundary coalescing and split counts/actual block-child destinations. `trace.map_block_path` maps an original block path to its surviving final path, independently of duplicate IDs or attribute equality. Removed blocks and invalid original paths return `None`; merged right-container descendants can survive inside the left container. A split retains the original head while its new tail has no original-block lineage. This maps block paths only, not text offsets or selections. The host still chooses and assigns IDs and owns the exact document revision to which the trace applies. Failed transactions restore the document and return no receipt or trace.

`MergeNodes` captures both original sibling blocks before mutation. Its inverse uses existing node steps to replace the merged block with those exact preimages, preserving distinct block IDs, kinds, attributes, marks and text-run boundaries. Compound inverses keep their own step order while forward-step groups reverse, so one receipt remains one atomic undo/redo entry among other edits.

The host owns GUI rendering, framework input decoding, highlight presentation, replace commands, account/session authorization, save/load policy, storage, diagnostics and application lifecycle. This package performs no I/O and requires no GUI, backend or async runtime. A truncated scan is a bounded partial result; the host must show that state. Correct an invalid query and rescan. On stale revision, refresh the model snapshot and retry through the host's existing coordination. Preserve the previous host dependency pin until standalone proof and a separate host-adoption check succeed.

## Dependencies and checks

Dependencies are ropey 1.6, serde 1 with derive, serde_json 1, thiserror 2 and regex 1. No repository workspace or nested Git repository is required.

From the containing Assets repository, the assigned proof owner uses the existing tracked package lock and an external Cargo target directory:

```text
cargo check --manifest-path Notes/handshake_rich_document_model/Cargo.toml --locked --tests -j 1
cargo test --manifest-path Notes/handshake_rich_document_model/Cargo.toml --locked -j 1
```

Existing model behavior tests remain in their owning modules. Eleven pure scanner tests live in `find_scan`; the native replace-all/undo test remains host-owned. Focused integration targets are `fragment`, `reference_identity`, `fragment_identity`, `tag_reference`, `merge_identity`, `path_trace`, and `fragment_trace`; run the changed targets before the full crate suite. Source assembly alone does not establish compile, runtime, publication or host-integration acceptance.
