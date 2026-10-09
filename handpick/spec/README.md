---
file_id: handpick-spec-entry-v1
file_kind: product-specification-guide
updated_at: '2026-10-09'
---

<topic id="purpose" version="1" status="baseline">

# What Handpick is

Handpick is the shared search and text-input surface for Handshake Desktop and Portal. It lets someone search content, metadata, settings and actions, and keep writing an idea in the same surface. It connects that writing to the existing Notes and Loom owners.

**Quicknote names the ad hoc entry workflow into ordinary Notes.** It is the same Notes content and canonical owner used by the full editor. There is no separate Quicknote entity, store, writing mode or promotion into a “real” note. Unsaved recovery describes editing progress; it does not create another content system.

The intended experience combines Logseq-style outlining and connected ad hoc writing with the existing Obsidian-style rich document workflow. Nest, move, collapse and focus blocks when useful; continue writing prose and rich documents when useful. Do not automatically turn prose into bullets.

[handpick-spec.yaml](handpick-spec.yaml) is the machine-readable baseline. It preserves all **53 HP-REQ requirements, 12 HP-CAP capabilities and 16 HP-AC acceptance criteria** from the committed Handpick work packet. Those records describe obligations, not a claim that every host already satisfies them. Read this guide, the YAML and the public source before extending the asset.

</topic>

<topic id="interaction-intent" status="baseline">

## The writing and searching experience

Search and explicit Save are separate buttons inside the field, including when short text still has suggestions. Typing, searching, expanding and pinning do not themselves create a canonical Note. Save creates an ordinary Note, optionally in a selected folder, or appends/inserts into an existing Note at a supported heading/block anchor. A folder is a container. The next Save continues the same saved Note/fragment through its owner operation lineage.

Longer writing can expand into a mini editor with a toolbar. Automatic expansion requires a genuinely **completed empty collection of applicable suggestions**. Pending, partial, offline, failed and stale-index states cannot qualify. Manual expansion remains available. Later results must not collapse active writing or replace its native document, caret, selection, undo or selected destination.

Search selected text or use a temporary query while keeping the composition. A bookmark parks a native composition when another search needs the field. Bookmarks occupy the **top edge** of the searchbox; labels contain no excerpt. Only hover/focus tooltips show excerpts. Restoring into an occupied field retains both compositions by exchanging the parked and active handles; it must never silently overwrite one.

Completion follows the useful VS Code pattern: contextual names/metadata/references, manual invocation, visible details, stable selected identity and explicit acceptance. Finding a command, folder, tag or template does not execute or create it. Generated prose and personal history remain open decisions. Keyboard handling must respect the native editor and IME composition.

These constraints are captured by HP-REQ-003–005, 016–038 and the exact scope/intent records in the YAML.

</topic>

<topic id="ownership" status="baseline">

## What the reusable crate owns

Handpick defines interaction outcomes and coordinates them. It is more than a database adapter, but the Rust library remains effect-free. The surface is rendered and connected to native editing by each host.

| Owner | Responsibility |
| --- | --- |
| Handpick | Versioned public contracts, bounded interaction state, lifecycle/request/revision fences, stable result selection, presentation transitions, parked native handle identities, IME admission and save-outcome orchestration. |
| Notes | Native rich documents and editor transactions, undo, note/nested-block identity, grants, canonical persistence, recovery, version/epoch conflicts and durable replay receipts. |
| Loom | Existing authorized tag/property and relationship context, provenance, freshness and projection evidence. |
| Host/adapters | Native editor binding, rendering, layout/animations/reduced motion, keyboard/IME/touch/accessibility, coordinate conversion, provider aggregation, query grammar/ranking, storage/network and effects. |

`Session` stores bounded literal strings and pins for plain input. `WritingSession` stores opaque native editor identities/revisions and orchestration state. **Do not flatten a rich composition into `Session::draft` to implement bookmarks or morphing.** Its content, selection and undo remain with the same Notes-owned editor handle.

SurrealDB retrieval/commit configuration belongs behind canonical owners using the application's pinned SDK/engine. No Turso, second canonical store, private backlink graph or duplicate rich editor is selected. Similar schemas or shared database technology do not prove cross-host CRDT compatibility or synchronization.

See HP-REQ-001–002, 006, 012–015, 017, 020, 022–023 and 039–053.

</topic>

<topic id="source-map" status="implemented" version="0.1.0">

## Public entry points

All paths in this table are relative to this `spec` folder.

| File | Start here for |
| --- | --- |
| [../src/lib.rs](../src/lib.rs) | Public Rust re-exports; optional WASM exports. |
| [../src/contracts.rs](../src/contracts.rs) | Opaque IDs, `Envelope`, `WireVersion`, result/action descriptors, delivery states and UTF-8 completion edits. |
| [../src/session.rs](../src/session.rs) | Literal `Session`, `Limits`, `Pin`, `Delivery` and `Error`. |
| [../src/writing.rs](../src/writing.rs) | `WritingSession`, native fences/IME/pins, destinations/outcomes, provider/query/action/reference/relationship/recovery contracts. |
| [../src/wasm.rs](../src/wasm.rs) | Optional synchronous bridge; JavaScript `Session` and `WritingSession`. |
| [../tests/writing.rs](../tests/writing.rs) | Native orchestration/admission examples and edge cases. |
| [../tests/fixtures/writing-bridge-v1.json](../tests/fixtures/writing-bridge-v1.json) | Shared native/WASM fixture: 4 scenarios, 59 steps. |
| [../tests/bridge_fixtures.rs](../tests/bridge_fixtures.rs) | Native fixture interpreter. |
| [../tests/wasm_bridge.mjs](../tests/wasm_bridge.mjs) | Actual generated web glue/WASM fixture and negative checks. |
| [../Cargo.toml](../Cargo.toml) | Standalone package/features, exact dependency versions and package allowlist. |
| [../README.md](../README.md) | Existing detailed library/API manual. |

The Rust modules are private implementation modules with public items re-exported from `handpick`. Provider contracts are in `writing.rs`; there is no separate provider module. `WritingProvider::validate_query`, `WritingSession::validate_action` and `RelationshipPage::validate` admit supplied state/evidence. They do not search, authorize, resolve targets, run native edits or commit anything.

Use `WritingSession` for the rich Quicknote workflow and `Session` only for literal input primitives. The JS writing bridge does not expose Rust `with_limits`, `editor` or generic `validate`; use the actual documented bridge methods. `SaveDisposition` and `WritingPin` are Rust API values without Serde serialization; bridge code explicitly maps them to disposition strings and pin JSON.

</topic>

<topic id="adapter-flow" status="baseline">

## Binding a host

1. Resolve current authority/capabilities through owners. Create a unique lifecycle `SessionId`, retain a Notes-native editor, construct with `WritingSession::new` or `with_limits`, then call `bind(EditorId, revision)`.
2. Apply editing in Notes. Report a newer revision with `edited`; retain the same native content, selection, undo and destination through compact/expanded/pinned presentations.
3. For lookup, call `begin_lookup` and retain the whole `WritingFence`. Dispatch the allowed explicit query/selected text. Aggregate applicable providers before claiming complete coverage, then `deliver` bounded results with truthful states.
4. Show the selected `ResultKey` and intended target/effect. Explicit acceptance validates the current fence; the owner reauthorizes and executes a native transaction or supported action.
5. Retain the exact `begin_composition` token through IME edits and use it for `end_composition`. Save, completion acceptance and pin transitions must not intercept active composition.
6. `park` and `restore` exchange native handle identities. The host retains rich editor state and recovery. An occupied restore needs a fresh replacement `PinId`; rejected transitions preserve the existing compositions.
7. On explicit Save, snapshot native content at the submitted revision and `submit` a `SaveIntent` with a stable operation ID and owner-resolved destination. The owner enforces grants, native version/epoch and durable deduplication.
8. Acknowledge the exact submitted intent with `SaveOutcome`. `Unknown` retains the pending operation for reconciliation before another submission. Commitment and relationship/search readiness are separate. A matching acknowledgement never authorizes clearing newer writing.
9. Retain the committed Note identity and prior operation. Subsequent Save uses `SaveDestination::Update`; the owner preserves unrelated content and rejects conflicting changes to the captured fragment without losing the composition.
10. At scope/lifecycle revocation, recover or release owner-native handles under the authorized policy, retire callbacks and `revoke` the old instance. Returning to a context does not revive previous requests.

The crate does not validate the actual database receipt or make recovery durable. Each host proves those properties through its public owner boundary.

</topic>

<topic id="host-status" status="dated-snapshot" updated_at="2026-10-09">

## What is implemented as of 2026-10-09

The published asset revision is `2b4bcdad756aa0f524152e3be2176f60acf32583`, package `0.1.0`, wire `handpick.v1`. Portal implementation is `b444617225eacf0cc96ce05436842f91722275ef`; its recovery record is `c361b8d4f3694fb2fb9ed12b856f54dc9ea88f62`. Tests ran at the recorded earlier HEAD with implementation inputs subsequently committed; this specification is not a fresh runtime validation.

The crate implements the interaction/contracts above, including completed-empty expansion admission and same-Note continuation intent. The asset's older `planned`/`integration-pending` README labels must be read at their stated library/host boundary; they do not describe the current external Portal implementation as wholly pending.

Portal supports the persistent surface, native live/source/reading editing, outlining, selected-text/transient query search, contextual completion, top bookmarks, scoped local rich recovery, explicit create/folder/append/anchored-insert/update, reference/embedded context, Loom backlinks/unlinked candidates, explicit durable tags and template preview/insertion/native undo. Reopening in full Notes preserves canonical rich content and block identities. Its capability declaration is `product/handpick/web/capabilities.ts`; the surface/public entry point are `product/handpick/web/Handpick.tsx` and `index.ts` in the Portal repository. Notes-native mechanisms live under `product/notes/web` and canonical owners under `product/notes/src`.

Portal explicitly lacks rich tag hubs, hierarchy, typed property schemas/query/mutation, durable aliases, automatic daily routing, semantic/generated-prose providers and replay-safe standalone task capture. Native Notes checkboxes and task search/open remain available. Its current 160-character/3-line expansion threshold and eight-bookmark limit are host policies, not finalized universal Handpick defaults.

**Desktop Handpick integration is pending coordinated host and canonical owner assignment under WP-KERNEL-012 authority.** Shared native/WASM and Portal proof do not establish egui behavior, Desktop persistence, both-host parity or Desktop x64 compatibility. Do not edit that active WIP on the strength of this asset specification alone.

Recorded evidence includes 27 ARM64 crate tests, real WASM shared fixtures, standalone package verification, 35 Notes-native tests, 45 frontend tests, seven scoped owner cases and Portal browser checks. Exact result scopes and references are in `proof_snapshot`; several scoped cases passed inside an aggregate run that later failed, so the whole aggregate is not marked passing. Cold-start offline use, cross-device pins/native-content synchronization, production deployment and remote Forgejo synchronization are not proved.

</topic>

<topic id="unresolved-decisions" status="open">

## Decisions that remain open

Keep the original `HP-DEC` records in the YAML as the decision source. The following are not authorization to silently choose a policy:

- Final morph thresholds, applicable suggestion policy and exact outliner/completion keybindings.
- Default ordinary Note/folder destination, automatic daily creation/date grouping and journal routing.
- Filter grammar, AND/OR tag selection, descendant inclusion and ranking.
- Prose ghost text, semantic models and personal history.
- Bookmark identity styling and cross-device pin/native-content synchronization.

Explicit Save, the Quicknote name, top bookmark placement and tooltip-only excerpts are already fixed. Portal local recovery/folders/native bindings are implemented; they are not still pending design choices. Grapheme-level editing policy and stock syntax support require honest host declarations and targeted proof rather than an invented parity claim.

</topic>

<topic id="compatibility" status="baseline" version="handpick.v1">

## Compatibility and distribution

Bind consumers to an approved exact compatible asset revision. `SaveDestination::Update`/`WritingCapability::UpdateCapture` are additive within `handpick.v1`, but older strict decoders reject an unknown variant. A shared version label does not make every addition backward compatible. Review payload shapes, semantics and consumer decoding, and rebuild matching native/WASM bindings together.

`Envelope<T>` rejects unsupported versions. Bridge calls consume raw payloads and report `wireVersion()` separately. Writing structures reject unknown fields; foundation contracts and bridge-specific shapes must not be treated as identical transports. Native foundation `Fence` uses numeric `u64`; its WASM bridge uses canonical decimal strings. Writing revisions/generations/sequences and applicable counts use canonical decimal strings in both transports. Never pass them through JavaScript `Number`.

Literal completion spans are half-open UTF-8 byte offsets; React UTF-16, native document positions and grapheme coordinates need explicit conversion. A text span is not a rich-editor transaction coordinate. The optional WASM bridge performs synchronous admission and returns content-free rejection codes; the host initializes generated web glue and retires callbacks before `free()`.

The current Cargo package allowlist excludes `spec/**`. This folder is documentation beside the reusable source in the asset repository; it does not change the published package or Portal's exact source mirror. No registry release or repository setup is requested by this documentation.

</topic>

<topic id="extension-proof" status="baseline">

## Extending the asset safely

Start with the actual public source, shared fixtures and current consumer authority. Map an extension to the existing HP-REQ/HP-CAP/HP-AC records; preserve the named intent and open decisions. Keep reusable orchestration in Handpick, native content/effects in canonical owners and rendering/animation in hosts. Add or change public contracts/fixtures instead of creating a private host interaction state machine.

Proof follows the changed boundary:

- Native crate tests establish orchestration/admission. Real generated WASM checks establish the same behavior across the bridge.
- Real owner readback establishes authorization, canonical persistence, replay/conflict behavior and derivative freshness. Mocks and matching DTOs do not establish those outcomes.
- Actual host GUI proof establishes native content/identity, selection/undo, IME, completion, top bookmarks, morph and readable wide/narrow layout.
- Record build/runtime architecture. Desktop x64 requires its own affected build/runtime proof; ARM64 and x64 emulation observations remain separate.

Use each consumer's current canonical runner and preparation requirements. Existing green proof can be reused only when its inputs and asserted behavior are unchanged. YAML parsing and ID checks validate this document; they do not advance runtime acceptance. An acceptance statement in the YAML remains a full obligation until every required layer has been proved, even when one host declares support.

On stale, denied, conflicting or unknown outcomes, retain recoverable native writing and report the precise state. Resolve the owner/lifecycle issue before another effect. Do not substitute duplicate content, a new Quicknote store or a private relationship graph.

</topic>
