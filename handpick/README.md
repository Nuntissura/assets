---
file_id: handpick-readme-v1
file_kind: product-manual
updated_at: '2026-10-09'
---

<topic id="foundation" version="0.1.0" status="implemented">

# Handpick foundation

Handpick is an effect-free Rust library for shared search interaction in web and native applications. It provides versioned Serde contracts, bounded literal draft/pin state, context and generation fences, stable result selection and completion replacements in explicit UTF-8 byte coordinates. Dependencies and package metadata are explicit, without workspace inheritance.

Create a `Session` with an authorized opaque `ContextId` and explicit `Limits`. Change literal input with `set_draft`; dispatch with `begin`; retain the returned `Fence` unchanged and pass it to `deliver`. Separate suggestion and search channels expose pending, complete, partial, failed, offline and stale-index states. Only completed delivery is evidence that that channel completed; this core does not establish full corpus coverage.

Call `revoke` immediately when access changes. It clears drafts, pins, results and selection. Call `switch_context` only after the host establishes fresh authority, including renewed grants under the same context ID. Old deliveries remain stale after switching away and back. Counter exhaustion is explicit, never wraps, and requires replacing the session and retiring all callbacks referencing the old instance.

Pinning parks text without saving a note. Host-supplied pin IDs must be unique across their lifecycle. Render bookmark identities along the TOP edge and show original excerpts ONLY in hover/focus tooltips, including keyboard focus. An occupied field requires a new pin ID on restore: restoration swaps the parked text atomically rather than overwriting either draft. Overflow rejects the operation with existing state preserved.

Completion edits reference the original draft revision and a half-open UTF-8 byte span. Invalid boundaries and oversize replacements reject without changes. The result is a UTF-8 caret byte offset. Hosts convert React UTF-16 and matcher/grapheme coordinates and own cursor/selection/focus rendering. The core validates Unicode scalar boundaries; grapheme-level editing policy remains undecided.

</topic>

<topic id="boundaries" status="integration-pending">

## Integration and export

No query grammar, ranking policy, editor morph threshold, database, network, GUI runtime, generated prose, history, auto-save, capture persistence or synchronization is implemented. Action descriptors express intent only; discovering or selecting one never executes it. Pins and drafts are in memory and are not a durability promise.

Portal and Desktop adapters must authorize suggestions, snippets, counts, pins, opens and capture destinations at their canonical owners. Recheck access before effects. The core's context fence cannot replace database authorization or grant revocation detection. SurrealDB SDK/query/index configuration belongs in owner adapters, which must use the application's pinned version and bound allowlisted queries.

Export this directory as a standalone Rust library, retaining its manifest, source, tests, README and MIT LICENSE. Its MIT package declaration matches the inspected host workspace manifest. Inspect destination authority/layout before publication. Import with a path dependency during development, then bind consumers to an approved exact asset-library revision. No registry publication is implied. Changes to versioned wire contracts require explicit compatibility review; unknown wire versions reject.

This asset repository owns the reusable `handpick` subfolder. From its root, run `cargo test --locked --manifest-path handpick/Cargo.toml`. Build the optional browser library with `cargo build --locked --manifest-path handpick/Cargo.toml --features wasm --target wasm32-unknown-unknown`, generate web glue using matching `wasm-bindgen` CLI 0.2.129 with `--target web`, then run `node handpick/tests/wasm_bridge.mjs path/to/handpick.js`. Keep compiler output and generated bindings outside tracked source. The Portal consumer additionally provides its canonical `tooling/check.ps1 -Profile handpick` runner. Tests cover preservation, scope changes, stale delivery, stable selection, UTF-8 edits, admission and contract round-trips. Product integrations, authorization, rendered behavior and real persistence need separate owner-boundary proof.

</topic>

<topic id="quicknote" status="contracts-implemented-integration-pending">

## Quicknote

There is one Notes system. Quicknote names quick writing through Handpick; it is not another note entity or a conversion into a regular note. The same content supports Logseq-style ad hoc outlining and the host's Obsidian-style document editing. Handpick coordinates the input interaction; the existing Notes owner provides native rich editing, identity, grants, save/conflict/history and persistence. Loom supplies authorized tag/property and relationship context.

Saving creates an ordinary note, optionally in a selected folder, or inserts/appends into an existing note at a supported owner-resolved location. Search, expansion and pinning do not themselves save content. Unsaved editor state, recovery and saved-state indicators describe editing progress, not different note types. New writing becomes an ordinary Note only on explicit Save. Native unsaved recovery remains separate; no separate writing mode exists. Default destination and journal defaults remain undecided.

`SaveDestination::Update` continues a previously committed capture in the same ordinary Note. It names the prior owner operation and expected destination revision; the owner preserves surviving block identities and unrelated destination content and rejects conflicting edits. The additive `handpick.v1` variant requires updated adapters: an older strict decoder rejects it instead of treating it as append or create. Every consumer binds to an exact compatible crate revision.

</topic>

<topic id="expanded-capabilities" status="planned">

## Expanded capability scope

The crate provides the shared interaction contracts below. Native transactions, durable recovery and host rendering still require separate integration and proof.

| Area | Planned behavior |
| --- | --- |
| Search | Content and metadata, Loom tags/properties, settings/commands/navigation and supported feeds/tasks; structural block/section/task queries and visible coverage. |
| Completion | Contextual note/heading/block/tag/property lookup, manual invocation, suggestion details and explicit native insertion. |
| Writing | Existing rich Notes editing with formatting, source/preview intent and unsupported-content preservation through compact, expanded and full-document presentations. |
| Outlining | Split/merge, indent/outdent, subtree movement, collapse/expand and block focus/zoom over the same note. |
| Connections | Stable note/heading/block references, authorized bounded embeds and backlink context. |
| Search while writing | Search selected text or find/insert a reference while keeping the composition and caret; top-edge pins with tooltip-only excerpts remain available. |
| Organization | In-place folder destinations, tags/properties, checkboxes, supported task actions and queries. |
| Saving | New note, folder placement and anchored insert/append into an existing note, with destination preview and replay-safe owner acknowledgement. |
| Recovery | Rich draft/pin recovery, occupied restore without overwrite, truthful durability and account/profile isolation. |
| Reuse | Explicit text/template insertion with preview and native undo; journal-style writing into ordinary notes without imposing daily creation or default routing. |
| Host interaction | Keyboard/IME/touch/accessibility, native/web rendering, reduced motion and structured diagnostics. |

The host-native rich document, selection and undo must survive expansion and pinning. `WritingSession` binds opaque Notes-owned editor handles and parks/restores those handles atomically; it does not copy native content or own recovery. The older `Session`/`Pin` API remains for literal search input. Shared text completion byte spans alone are not rich-editor transaction coordinates.

Expansion is a reversible presentation change. A completed-empty applicable suggestion set can enable automatic expansion; pending, partial, offline or failed work cannot. Explicit expansion remains available, and late results must not collapse an active editor. Keyboard arbitration must distinguish editing, completion, search, save and command execution without interrupting input-method composition.

Adapters expose supported editor, outliner, reference, insertion, recovery and query capabilities. Notes owners resolve current targets and apply native transactions; Markdown replacement and raw CRDT transport are not semantic append APIs. Permanent nested IDs, conflict handling, lost-response reconciliation and real owner readback are required. Matching schemas do not establish cross-host native synchronization.

Portal and Desktop need their own rendered and owner-boundary proof. The host owns animations and rendering mechanisms while Handpick defines interaction outcomes. Generated prose, personal history, journal/default routing, exact keybindings and local versus cross-device pin synchronization remain open decisions.

</topic>

<topic id="writing-contracts" status="implemented" version="0.1.0">

## Native writing contracts

Create a `WritingSession` with an authorized context, a host-unique session identity and bounded pin capacity. `bind` attaches an owner editor identity/revision. The host retains that exact native editor, content, selection, undo and recovery. `edited` advances the revision; every query and action carries the current `WritingFence`. `begin_lookup` advances request identity without modifying composition. Retire all callbacks and call `revoke` when the authorized lifecycle ends.

`begin_composition` returns an IME token that remains valid through edits in the same editor; pass that exact token to `end_composition`. Save submission, native actions and pin transitions reject during composition. `expand` and `compact` change presentation only. `park` retains a native editor handle; occupied `restore` atomically exchanges it with a fresh replacement pin. Rejected transitions preserve state.

Only explicit Save submits a `SaveIntent`, containing an operation identity, editor fence and owner-resolved create/append/heading-or-block insertion destination. Capture the native snapshot at that revision in the owner adapter. A pending unknown outcome blocks another submission until reconciled. A matching `SaveOutcome` separates canonical commitment from search/relationship readiness. Acknowledgement never clears the editor: older-revision or active-IME dispositions retain current writing. The owner must persist replay receipts and recheck destination grants.

`deliver` and `select` reuse the literal core's bounded result admission and stable identity selection. Editing, rebind, pin transitions and new lookups invalidate old deliveries. `can_expand`/`expand_if_empty` permit automatic expansion only after a completed-empty applicable suggestion collection; adapters aggregate applicable providers first. Partial, offline, failed and pending collections never qualify. Later results never collapse an expanded editor.

`WritingProvider` declares supported, unsupported or unavailable capabilities and canonical-set identity. Query admission bounds explicit selected text/query and page size; it does not execute search or establish authorization. `WritingActionIntent` describes explicit reference/embed, outline, template, save and owner actions at a native revision. The owner resolves coordinates and executes transactions. `ReferenceResolution` distinguishes missing, deleted, inaccessible, ambiguous and unsupported targets.

`RelationshipPage` carries its request fence, provider, real projection revision, freshness, delivery status, count scope and bounded source context. Linked, unlinked and provisional evidence remain distinct. Missing projection identity cannot claim readiness; page-only counts cannot stand in for canonical totals. `RecoveryState` describes owner evidence, never infers disk durability or synchronization. Revisions/counters in the writing JSON contracts are canonical decimal strings to preserve full u64 precision in JavaScript.

Focused native tests cover lifecycle fencing, atomic occupied restore, IME, query admission, replay outcomes, anchored targets, relationship evidence and exact counter transport. Shared native/WASM fixtures exercise the same orchestration. These tests do not prove host editor continuity, owner persistence, grants or rendered usability.

</topic>

<topic id="architecture" status="x64-proof-pending">

## Target architectures

The default foundation has no assembly, native database or GUI dependencies. The development tablet's inspected native toolchain is `aarch64-pc-windows-msvc`. Native ARM64 tests do not prove an x64 application build or behavior. Consumer integrations targeting x64 require the appropriate Rust/MSVC components and separate x64 proof. Keep build target and runtime architecture visible in validation results.

The optional `wasm` feature exposes the same Rust `Session` and `WritingSession` through a synchronous JSON bridge; default runtime dependencies remain Serde only. It requires `wasm32-unknown-unknown` and matching `wasm-bindgen` CLI 0.2.129. Generate `--target web` glue and initialize it explicitly: Vite 7 supports explicit WASM initialization but not direct WASM ES-module integration. Native and real WASM fixture proof remain separate from rendered editor and owner-persistence proof.

The JavaScript `Session(context, limitsJson)` exposes literal search input through `setDraft`, `draft`, `draftRevision`, `begin`, `deliver`, `delivery`, `select`, `selected`, `complete`, `pin`, `restore`, `pins`, `switchContext` and `revoke`. Its strict limits JSON uses `max_text_bytes`, `max_pins`, `max_items`, `max_item_bytes` and `max_actions_per_item`; bridge ceilings are 65536 bytes per text/item, 1024 pins/items and 64 actions per item. Foundation fences in this bridge use decimal-string `generation` and `draft_revision`, preserving the older Rust foundation wire format outside this bridge. `complete` accepts a JSON edit with that bridge fence, UTF-8 byte `span` and literal `replacement`, returning a UTF-8 caret byte offset. Call `free()` after retiring callbacks.

The JavaScript class is `WritingSession(context, session, maxPins)`. Supply authorized, host-unique identities and an integer pin capacity from 0 through 1024. `wireVersion()` returns `handpick.v1`. `bind(editorId, revision)` attaches an opaque native editor handle. Revisions are canonical decimal strings, including when larger than JavaScript's safe-integer range. `fence()`, `beginLookup()`, `beginComposition(fenceJson)`, `pins()` and `pendingSave()` return JSON strings; `edited(fenceJson, revision)`, `endComposition(tokenJson)`, `submit(intentJson)` and `acknowledge(outcomeJson)` accept the public Rust contract shapes serialized as JSON. `acknowledge` returns a snake-case disposition string. Retain active composition on `retain_active_composition`; reconcile uncertain canonical effects before retry on `reconcile_before_retry`.

`expand()` and `compact()` change presentation only; `expanded()` reads it. `deliver(fenceJson, channel, status, itemsJson)`, `delivery(channel)`, `select(fenceJson, keyJson)` and `selected()` expose shared result admission and stable selection. `canExpand()`/`expandIfEmpty(fenceJson)` apply the completed-empty suggestion gate. `validateRelationships(pageJson, providerId, maxRows)` invokes shared relationship evidence admission, with at most 1024 rows at the bridge. `park(pinId)` and `restore(pinId, replacementPinId)` exchange native editor handles without copying content; occupied restore requires a fresh replacement pin ID. `validateAction(intentJson)` and `validateQuery(providerJson, queryJson)` perform core admission only. `revoke()` retires the orchestration instance; the host must first recover/release its native editor handles and then call generated `free()` when finished. Bridge errors throw content-free codes; malformed JSON yields `InvalidWire`, JSON above 65536 UTF-8 bytes yields `PayloadLimit`, and noncanonical revision strings yield `InvalidRevision`. The bridge does not authorize, persist, log or transmit composition. Retain native selection/undo/recovery in the Notes owner.

Research basis: [wasm-bindgen exported names](https://wasm-bindgen.github.io/wasm-bindgen/reference/attributes/on-rust-exports/js_name.html), [Result exceptions](https://wasm-bindgen.github.io/wasm-bindgen/reference/types/result.html), [web deployment](https://wasm-bindgen.github.io/wasm-bindgen/reference/deployment.html), and [Vite 7 WASM support](https://v7.vite.dev/guide/features#webassembly). A thin optional binding reuses one interaction implementation; a separate TypeScript state machine would duplicate it. Explicit initialization avoids a new bundler plugin. Shared `tests/fixtures/writing-bridge-v1.json` outcomes run through native Rust and `tests/wasm_bridge.mjs` against generated web glue and actual WASM; those checks establish transport/orchestration behavior, not native rich editing continuity.

</topic>
