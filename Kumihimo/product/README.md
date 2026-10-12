---
file_id: KUM-PRODUCT-MANUAL-v1
file_kind: product-and-model-manual
updated_at: '2026-10-12'
---

<topic id="purpose-and-boundaries">

# Portable ownership core

`kumihimo-core` 0.3 is independently usable with std-only default features. It owns root workspaces, frames, frame-owned child workspaces, ordered open and retained closed views, groups, host resource references, tool surfaces and flat split/grid packing. Geometry is a pure projection of canonical inputs. Flat records avoid recursively owned trees. Optional `serde` supports the schema-1 saved DTO; independent `kumihimo-layout-json` 0.1 provides bounded JSON encoding and decoding.

The host owns actual content, provider engines, synchronization, file IO and resource policy. Registering or restoring a provider/resource creates a lightweight identity record; it neither starts an engine nor duplicates its data. Independently selectable std-only `kumihimo-content` 0.1 privately owns Core and coordinates host-executed content effects. This workspace has no GUI, renderer, native popout or engine implementation. Whole-state recovery validates detached saved data before replacing the arrangement. Adapters consume the same core commands; they must not maintain a competing canonical arrangement.

</topic>

<topic id="startup" ingestable="true">

## Start and check

From the Kumihimo project root, with Rust/Cargo 1.91+ and the selected platform's linker installed:

```powershell
cargo check --manifest-path product/Cargo.toml --locked -p kumihimo-core --no-default-features
cargo check --manifest-path product/Cargo.toml --workspace --all-features --all-targets --locked
cargo test --manifest-path product/Cargo.toml --workspace --all-features --locked
cargo run --manifest-path product/Cargo.toml --locked -p kumihimo-core --example workspace
cargo run --manifest-path product/Cargo.toml --locked -p kumihimo-layout-json --example roundtrip
cargo check --manifest-path product/Cargo.toml -p kumihimo-content --all-targets --locked --offline -j 2
cargo test --manifest-path product/Cargo.toml -p kumihimo-content --locked --offline --no-fail-fast -j 2
cargo run --manifest-path product/Cargo.toml -p kumihimo-content --example recording_host --locked --offline -j 2
```

The console example creates nested ownership, moves a frame, supplies geometry, reserves a pinned tool strip and commits a docking preview while retaining view/resource identity. It opens no windows. Builds should use the project's admitted serial lane and configured reusable target directory; session instructions are outside this product manual.

To consume the library, add a path dependency on `product/kumihimo-core` from your application's Cargo manifest, relative to that manifest. `src/lib.rs` exports the API; `model.rs` defines records/policy; `identity.rs` and `command.rs` define scope and envelopes; `core.rs` owns replay; `mutation.rs` applies ownership changes; `tabs.rs` handles tabs and queries; `arrangement.rs` validates detached candidates; `layout.rs`, `geometry.rs`, `solver.rs`, `tools.rs`, `docking.rs` and `layout_actions.rs` implement projection and placement. `persistence.rs` defines saved records/stamps and `restore.rs` validates recovery. Core consumer contracts live in `tests/`; JSON recovery contracts and its example live in the JSON package. Tests use public interfaces without mutable access to live storage.

Observed for WP008 candidate `90f93239d2e474e28451cab15458454ecd1649dc`, using stable rustc 1.91.1 on Windows x64 MSVC: the locked/offline all-targets check passed, one private exhaustion test, nine existing ownership contracts and all twelve layout/tool/docking contracts passed. The console example returned `generation=11`, root frames 2 and 4, view 7/resource 6, tool 8, content `(0,24,640,336)` and two panes. The float-return regression passed with append excluded and with no eligible return. These results prove the admitted headless in-memory scope on that host; Windows ARM64, WASM, native GUI, cross-platform runtime and performance remain unproven. The earlier WP007 candidate `a004dbd4c655d6d193e288d21e37dc9987095353` passed its ownership-only proof before this extension.

</topic>

<topic id="api-and-ownership" ingestable="true">

## Operate the core

Create `Core::new(identity, Limits::default(), receipt_capacity)?`. Read `core.snapshot()` for version 3 records, identity, generation and effective limits; cloning a snapshot produces detached inspection data and cannot overwrite live state. Use `core.execute(Envelope)` for every mutation. Inputs are identity, actor attribution, an instance-local operation sequence, expected current generation and one `Command`. Outputs are an attributable `Outcome` containing identity, generation and `Result<Change, Error>`, plus `Response::replayed`.

```rust
use kumihimo_core::{Command, Core, Envelope, Identity, Limits, OperationId};
let identity = Identity::new("document-lineage", "host-issued-incarnation").unwrap();
let mut core = Core::new(identity.clone(), Limits::default(), 64).unwrap();
let response = core.execute(Envelope {
    identity,
    actor: "host-controller".into(),
    operation: OperationId::new(1),
    expected_generation: 0,
    command: Command::CreateWorkspace { owner: None },
});
assert!(response.outcome.result.is_ok());
```

`CreateWorkspace { owner: None }` creates a root. A frame has exactly one placement workspace and at most one child workspace. `CreateFrame` inserts it in the ordered workspace; `CreateWorkspace { owner: Some(frame) }` attaches its child workspace. Frames retain stable identity independently of plan-local split/grid nodes.

`RegisterProvider { key, supports_shared_views }` declares a stable unique provider key and sharing capability. `RegisterResource { provider, key, restore_reference }` supplies a key unique within that provider and an opaque host reference. `CreateView` creates one open placement; another open view of the resource requires declared sharing. Closed views do not consume open sharing capacity. Capability declaration is a host assertion, not engine proof. IDs allocate monotonically across all seven entity kinds from 1; recovery preserves IDs and allocator monotonicity within a lineage. `from_raw` creates a target reference without reserving an ID. Zero and `u64::MAX` are invalid entity IDs; the latter is the allocator's exhausted sentinel. Host lineage integrity determines identity guarantees for records absent from live history.

`CreateGroup` stores a frame-local label and optional color; `AssignGroup` checks view/group frame ownership. Group membership is derived from view references. `SelectView` selects one existing view in its owning frame. Creating the first view selects it. Moving the selected view away selects the first remaining source view, or none. An empty destination selects the incoming view. A cross-frame view move clears its frame-local group; resource/provider references remain unchanged. Frame subtree moves preserve every descendant's groups, selection, order and identities.

`MoveFrame` rejects self/descendant containment cycles using iterative ancestry. `MoveView` moves one existing placement. Insertion indexes for both refer to the destination sequence **after removing the moving item**. For `[A, B, C]`, moving B to 0 gives `[B, A, C]`, to 1 is unchanged, to 2 gives `[A, C, B]`; index 3 rejects. Creation indexes refer to the unmodified destination. A same-parent unchanged order, same group assignment, already-selected view or identical policy is an explicit `Change::Unchanged` and does not increment generation.

`SetViewMetadata` supplies label, pin and unsaved state. `CloseView { approved }` requires host approval independently of unsaved state, checks pin policy and retains a tombstone without disposing host content. Closing the selection selects the first remaining open view. `ReopenView` preserves IDs/resource, clamps its saved index to the destination's current length and rechecks open membership, sharing and pin policy. Restored/reopened host visibility/usability starts false; supply `SetViewState` after host confirmation.

`ConfigureGroup` adds description and collapse metadata. `MoveGroup` moves open members atomically in canonical order, including interleaved members, and updates retained closed members' frame. `index` is the destination tab index after removing the moving open members; `group_index` independently addresses the destination group metadata sequence after removal. Selection recovery is deterministic. `PinPolicy` can deny close, reorder and cross-frame moves across individual/group/reopen routes; genuine no-ops remain available.

`Snapshot::tab_page(&TabQuery)` searches the complete canonical open/closed set, optionally filters a frame and pages canonical or label order. Rows expose metadata and unavailable-provider placeholders; `total` counts the selected frame/open/closed set before search and `matched` counts it after search, independently of page length. This is a headless metadata query, with no renderer or engine startup.

</topic>

<topic id="admission-and-replay">

## Admission, ordering and replay

Defaults admit **16,384 total records**, **65,536 layout entries**, **1,024 retained closed views**, **256 rows per tab query**, **65,536 UTF-8 bytes per label/description/restore reference** and **256 bytes per provider/resource key**. Closed views remain records; retention exhaustion rejects another close without evicting history. Depth and per-frame open-view limits are optional. `Limits::unbounded()` explicitly opts into host-managed admission. Record limits are not memory ceilings; actor/envelope sizes and host content remain host responsibilities. Identity components have an independent fixed 128-byte maximum.

`max_depth` measures frame depth: root workspace is 0, frames inside it are 1, an owned child workspace has its owner frame's depth, and its child frames add one. New frames must meet the configured depth. Moving deeper checks the deepest descendant frame, while same-depth or shallower moves remain available for recovery after limits are lowered. `max_views_per_frame` counts placements locally: new views and cross-frame moves that increase destination usage must meet it. `max_records` applies to new entity creation globally. `SetLimits` never deletes existing records; lowered limits reject affected new admission. Moves do not allocate records. Policy changes increment generation only when policy differs.

`Identity { lineage, incarnation }` is immutable for a Core; both strings are nonempty and at most 128 UTF-8 bytes. The host supplies lineage integrity and an incarnation fresh across all previous instances; the library performs no randomness, clock or process lookup. Envelopes, outcomes, snapshots, previews, prepared restores and save stamps carry that scope. Foreign envelope or embedded token identity rejects before receipt lookup/high-water handling and has no ledger effects, even when sequence and payload match a retained receipt.

One `&mut Core` serializes commands. The host allocates increasing operation sequences across all actors; independent content work is external. Check `ledger_status().high_watermark.checked_next()` or coordinate equivalent host allocation. Sequence gaps are allowed; any sequence at or below the high-water mark without a retained receipt is expired, including an unused skipped sequence. Sequence 0 is invalid. At `u64::MAX`, `checked_next()` returns `None`; never wrap. Fresh recovery starts a new ledger and incarnation, so old envelopes/tokens cannot execute there.

The fixed receipt capacity counts outcomes, including rejections and no-ops. Exact retained **complete envelopes** replay their original outcome before stale-generation validation; the original outcome generation can be older than the current snapshot. Changing actor, expected generation or command under a retained operation ID rejects as `IntentMismatch`. A sequence whose receipt was evicted returns `ExpiredOperation`, never repeats a mutation. Capacity 0 disables replay, while the high-water mark still prevents old sequences from executing. There is no unbounded operation-ID set and no cross-restart deduplication guarantee.

Rejected commands may advance the high-water mark and store/evict bounded receipts. They do not alter model records, generation or entity allocation. A new valid state-changing command increments generation once. Retrying a rejection with corrected intent needs a new sequence and a fresh expected generation.

</topic>

<topic id="geometry-and-packing" ingestable="true">

## Geometry and packing

`Scalar::new` rejects non-finite values and normalizes signed zero. `Size` validates nonnegative dimensions; `Rect` also checks finite endpoints. Exact equality preserves complete-envelope replay. Coordinates are f64 logical DIPs. Negative gaps, nonpositive weights, arithmetic overflow and infeasible constraints return `Error::Geometry`; rounding that would undercut a track minimum rejects instead of silently violating it.

Supply `SetGeometryContext { workspace, context }` for each workspace whose geometry is requested. The canonical context owns viewport, gap, overflow, per-frame minima, automatic-packing weights, explicitly eligible `DockLocation`s and floating permission. `Snapshot::geometry(workspace)` returns generation, required/virtual extents, viewport/content rectangles, every workspace frame, frame content, clips, tool presentation and allocated nodes/cells. Child-workspace minimum requirements propagate iteratively into the owner frame; each requested child geometry uses its own host viewport. The host must update that viewport when displaying it inside its allocated parent content.

Unconfigured packing derives horizontal leaves from the full canonical frame order. `SetLayout` configures a flat `LayoutPlan`: one root, a `BTreeMap<NodeId, LayoutNode>`, weighted horizontal/vertical splits or weighted row/column grids with explicit non-spanning cells. Holes are allowed. Every non-floating frame has exactly one leaf; missing references, cycles, multiple parents, orphan nodes and duplicate cells reject. `NodeId` is a plan-local selector, not a generated stable entity ID; repairs may change selectors. Configured split/grid weights control their own tracks; context frame weights control automatic packing.

Allocation first sums child minima and inter-track gaps, then divides only spare extent in proportion to normalized positive weights. The final track consumes the rounding remainder. Grid track minima are the maximum occupied-cell minimum in each row/column. Workspace tool strips are reserved first; frame/view strips reserve their frame interior. `Overflow::Reject` rejects undersized inputs atomically. `Overflow::Scrollable` expands virtual extent and returns clips without discarding panes; actual scrolling/reveal is a later host adapter responsibility.

Legacy `CreateFrame`/`MoveFrame` reconcile configured packing without redefining canonical sequence indexes: remove/prune the departing leaf, then append to a compatible horizontal root split or wrap the retained root horizontally. Unrelated weights, grid tracks/holes and child-owned plans survive. Explicit configured-empty scopes remain configured. Per-frame minima/automatic weights follow cross-workspace frame moves into an existing destination context; moving into a scope without context removes those source-local inputs and requires the host to supply its destination measurements.

</topic>

<topic id="tools-docking-and-preview" ingestable="true">

## Tools, docking and preview

`CreateTool(ToolSpec)` keeps provider owner, original view, bound `EditingTarget { view, resource }` and placement separate. The owner must match the origin resource's provider. Placements target a view/frame/workspace edge or an unoccupied configured grid cell. Supply allowed placements, orientations, preferred/minimum sizes, order and independently reported `TargetState`. Configure the destination workspace context before placing a tool there. Edge thickness uses preferred size; the cross axis enforces minimum size. Strips reserve top, bottom, left, right, then host order and ToolId. They cannot silently overlap or make negative content.

`ToolPolicy::new` defaults to pinned binding, visible-and-usable target visibility and hidden-space release. Keyboard focus never hides or retargets a tool. Effective geometry presentation also checks origin/target open state, host view state and provider availability. Closed or unavailable targets cannot be usable/applicable despite stale host flags. An outward `Always` tool may remain visibly disabled and retain space; a closed-view-anchored tool is dormant. `Snapshot::effective_tool_presentation` exposes this canonical decision; `Tool::presentation` alone projects raw host flags. `BindingPolicy::ActiveCompatible` records host intent without automatic retargeting. `RebindTool` requires a compatible currently usable view/resource pair. Moving outward or returning retains origin/owner/target; edge moves select the allowed orientation.

Use `DockIntent` for existing frame/tool placement, bounded `Float` or `Redock`. Frame destinations require the destination context's explicit eligibility; tool destinations require its allowed policy. Float rectangles must satisfy minima and fit the complete workspace viewport, preserve one workspace owner and report `SurfaceKind::InAppFloat`. Their saved return location prefers eligible workspace append, otherwise another currently validated eligible destination in that workspace; floating rejects if none can redock. Later host policy changes can still require a new explicit dock intent. Native windows and modal overlays are not exposed.

`Core::preview(intent)` validates/solves a detached hypothetical state without consuming live generation, entity allocation or ledger outcomes. Inspect its private token's accessors; consume it with `cancel()` or wrap it in `Command::CommitPreview`. Commit rechecks generation and intent through the existing Envelope path, imports no geometry, and exact retained replay mutates only once. Every changed canonical topology, context, target state or policy invalidates an older token. `snap_candidates` projects only explicitly eligible current edges/shared frame boundaries/grid cells within the supplied radius, ordered by distance then location; snapping adds no arbitrary subdivisions, overlap or hysteresis.

The default aggregate `max_layout_entries` is 65,536. `layout_entry_count()` counts each retained node plus leaf/child reference, grid track/cell reference, context plus map/eligibility entry, four tool policy/state records plus allowed/compatible/orientation entries, two records per float and one per maximized workspace. Derived automatic plans/solver output are not retained metadata. Limits count records/entries, not bytes; lowering preserves existing state and allows unchanged/decreasing metadata recovery while rejecting growth beyond the new limit. Unrestricted policy remains explicitly host-controlled.

`Equalize` sets workspace split/grid and automatic-packing weights to one while preserving structure and minima; only spare extent is divided equally. `SwapFrames` swaps two docked same-workspace frame leaves without changing identities, ownership, view order or child workspaces.

`Maximize` retains underlying packing while projecting one docked frame, its descendant constraints and selected/frame/root-edge tools. Sibling frames, floats and cell tools are hidden; their structural references remain validated. Ownership/packing/docking operations affecting either source or destination of that projection reject, including preview commits and child attachment. Tab/group/geometry and independent operations remain available under their normal policies. `RestoreLayout` validates complete retained geometry before exiting; failure preserves maximization. Hidden float bounds do not invalidate active maximize geometry, but exiting checks them, and scrollable overflow does not repair out-of-bounds floats.

Package 0.3/Snapshot 3 are unreleased breaking API changes. Snapshot 1/2 were inspection records, never durable files; schema 1 is the first supported saved format.

</topic>

<topic id="saved-layout-and-recovery" ingestable="true">

## Save and restore

`Core::save_data()` captures immutable `PersistedLayout` plus `SaveStamp`; `save_layout()` returns the DTO alone. Schema 1 retains flat IDs, ownership/order/selection, closed membership, group/pin metadata, stable provider/resource keys and restore references, packing/contexts/floats/maximize state, source identity/generation and allocator. Runtime handles, engines, provider availability, view host state, tool `TargetState` and ledger receipts are omitted. Restore resets host flags and leaves unresolved providers as `Missing` placeholders. `RestoreOptions.providers` supplies key/capability resolution without invoking engines; incompatible capability yields an unavailable provider. The host confirms view/tool state separately.

For live recovery, call `core.prepare_restore(saved, options)?`, inspect the private token's source/options/target accessors, then submit `Command::Restore(token)` through the current envelope. Preparation has no live model or ledger effects. The source lineage must match; extant IDs cannot change entity kind or immutable provider/resource/view-resource identity. The token binds target identity and generation; any intervening mutation rejects it. Successful restore always advances current generation once, even for identical arrangement, uses `max(live, saved)` allocator and preserves the live ledger. Historical receipts still replay their original outcomes; they do not prove a referenced record still belongs to the restored arrangement.

For fresh recovery, `Core::from_persisted(saved, fresh_incarnation, options, receipt_capacity)?` retains saved lineage/exact IDs, requires an incarnation different from the source, starts generation 1 and an empty ledger/high-water 0. The host must ensure freshness against every earlier instance. No absent-history identity guarantee or persistent deduplication follows.

Effective limits are the intersection of saved and host `RestoreOptions.limits`. Fresh imports are strict. Live restore permits unchanged/decreasing overage only where the effective dimension equals its current canonical ceiling, comparing current usage by that dimension and frame/field rather than borrowing another entity's excess. Stricter effective ceilings remain strict; actual policy is stored without inflation. Structural integrity, IDs, references, cycles, minima and wire limits are never grandfathered. The allocator must exceed every saved entity ID; `u64::MAX` is valid only as its exhausted sentinel.

`RestoreOptions.bounds` supplies host viewports. `FloatRepair::Reject` rejects invalid active float bounds; explicit `Clamp` moves/resizes them within the viewport only if minima fit. Maximized hidden floats may retain stale bounds until exit validation. Invalid recovery leaves the existing Core intact; detached whole-state validation does not run legacy frame-move measurement migration.

`kumihimo_layout_json::encode`, `decode` and option variants support complete schema-1 JSON. Typed entity IDs, generation, allocator and plan-local `NodeId` use canonical decimal strings, preserving all u64 precision; numeric aliases, leading zeros and invalid generated IDs reject. Unknown fields, duplicate object members at every nesting level, duplicate logical records, trailing input and unsupported schema reject at codec or restore validation. Finite geometry uses validated constructors. Both encode/decode default to an 8 MiB byte limit, configured by `CodecOptions`; the ordinary JSON parser recursion guard remains enabled.

`encode_core(&core, options)?` captures immutable `SavedBytes`; `saved.is_current(&core)` checks its identity/generation stamp. The host must serialize publication through one writer and check freshness immediately before publishing, with mutation excluded between check and publication. Atomically replace a complete blob, or keep identifiable prior/new complete blobs for recovery. On interrupted writes or decode/restore failure retain the last validated Core/blob. The library performs no file IO; actual storage atomicity and interrupted-save recovery are unproven here.

Observed for published WP009 candidate `c387f9e4f6d353e8dedeb9ed49590a4681c73b30` on Windows x64 MSVC with stable rustc 1.91.1: the locked/offline all-feature workspace/all-target check passed; all 49 tests passed (1 private exhaustion, 9 ownership, 12 geometry/tool/docking, 7 layout recovery, 7 tabs and 13 JSON recovery); both console examples succeeded. The workspace example returned generation 11 and two panes; schema-1 JSON roundtrip used 551 bytes and restored fresh generation 1. The default-feature core check passed at `50563242c7f146a3534ff73bdff7a5b8a9626c99`; its unchanged runtime/manifest/lock inputs permit reuse for the final candidate. Later corrections changed test fixtures only. Independent evaluation checked the exact published source and proof logs. These results establish the admitted headless in-memory tab and layout-recovery scope on that host. Host file IO/interrupted-save recovery, GUI, provider engines, Windows ARM64, WASM, other platform runtimes and performance remain unproven. WP007/WP008 results above are historical evidence for their respective candidates.

</topic>

<topic id="errors-and-recovery">

## Inspect failures and recover

`Error` distinguishes invalid/foreign identity, missing typed targets, duplicate child ownership, containment cycles, invalid indexes, unsupported sharing, cross-frame group mismatches, admission, close veto, pin restrictions, closed targets, maximize guards, stale restore/preview/generation, retained intent mismatch, expired/zero operations and exhausted counters. Errors include relevant IDs or expected/observed values and implement Rust's error/display interfaces. Arrangement changes validate a detached candidate before replacing live records, generation or allocation; rejected candidates leave canonical state intact. Hosts cannot mutate the live snapshot directly.

On `StaleGeneration`, read the current snapshot and resubmit intentionally with a new operation sequence. On `IntentMismatch`, inspect the original envelope; replay it exactly only while retained. On `ExpiredOperation`, inspect canonical state and reconcile intent before issuing a new sequence. On `IdentityMismatch`, obtain the current scope and reconcile the command rather than substituting an identity on an old token. On `RestoreConflict`, prepare again against current state. On admission rejection, adjust host policy explicitly or reduce/reuse existing placements. On maximize exit geometry failure, repair retained context/float bounds before retrying exit. On cycle or invalid index rejection, inspect current parent relations and order. Counter exhaustion never wraps.

Core diagnostics contain arrangement metadata, IDs and host-supplied labels/actor names; do not put secrets or private content in those strings. No filesystem, network, process or clock effects are performed. Snapshot copies are useful for complete pre/post comparison, while ledger diagnostics separately expose operation retention. Core mutations are synchronous in memory; the content companion separately exposes pending and Unknown host-effect obligations.

</topic>

<topic id="content-protocol" ingestable="true">

## Coordinate replaceable content

Add a path dependency on `product/kumihimo-content`; its sole dependency is Core 0.3 with default features disabled. `src/protocol.rs` defines public typed messages; `routing.rs` owns the controller and canonical transition observation; `scheduler.rs` implements causal bounded dispatch; `lifecycle.rs` implements acknowledgments, cancellation and recovery; `diagnostics.rs` handles declared cache reservations and redacted host samples. Public consumers are `tests/recording_host.rs` and the quiet `examples/recording_host.rs`. WP010 compilation/runtime proof is pending.

Construct `Controller::new(core, fresh_session, ContentLimits::default())`. The host supplies a nonempty session of at most 128 UTF-8 bytes and guarantees uniqueness across controller lifetimes. `new_with_id_start` accepts a nonzero host sequence offset for deterministic exhaustion checks; it seeds no effect state. `core()` and `snapshot()` expose immutable inspection; route every canonical `Envelope` through `execute`. Its outer error is a companion pre-admission veto; the inner Core response retains canonical error and replay behavior. There is no mutable Core escape or extraction API. Successful identical-ID live restore retires bindings; retained/rejected Core operations do not repeat retirement. Unrelated labels/generations preserve relevant acknowledgments.

Negotiate protocol 1.0 and intersect typed host/provider capabilities with `negotiate`. A version/close-mode incompatibility returns a structured error without changing prior protocol state. `placeholder(view)` preserves canonical labels and recovery references while provider availability, negotiation or attachment is unresolved. Capabilities declare content modes, embedded input, anchors, lifecycle, restoration, close mode, tools/assets and cache support. They grant no permission by themselves.

Call `attach(view, options, permission, tick)` to reserve a distinct mount and, unless explicitly shared, an instance. Sharing requires Core capability, negotiated sharing and a host grant. Call `dispatch(mount, fresh_permission, tick)` to pull the next typed request. The host performs the effect and returns `Ack { ticket: request.ticket, result }` to `ack`. Exact ticket scope includes controller/Core identity, provider/resource/view, connection, mount, instance, request and channel revisions. Attach confirms protocol state only; host `SetViewState`/`SetToolState` remain separate canonical assertions. Moves preserve content identity. Missing callbacks, hidden controls or metadata Drop never dispose engines.

Geometry validates finite rect/clip and positive scale. Optional opaque anchors require support and exact echoed completion. Input owns a declared content/embed recipient; frame chrome is rejected by the provider-input route. Coordinates require the captured geometry clip. Input/tool/asset actions capture geometry/focus prerequisites. Only unsent geometry/focus/presentation/invalidation within one segment coalesce; input, tool/asset, close, lifecycle, attach/detach and anchored geometry form barriers. An action waits for confirmed prerequisites; later queued values cannot overtake it or reinterpret its meaning. `None` from dispatch exposes pending/backpressure; choose another independent mount to continue. Shared mounts serialize one instance. Provider callbacks never run synchronously inside arrangement execution.

Host millisecond ticks must be monotone. `advance` and both dispatch routes observe deadlines (default 30,000 ms). Queued work expires at its deadline before provider exposure, records `queued deadline`, and rejects dependent work. `cancel` records intent. Unsent expiry or cancellation releases only uncommitted attachments; an ever-confirmed instance stays reserved after its last detach until exact disposal or reconciliation proves cleanup. Dispatched ambiguity or timeout holds the instance Unknown until exact reconciliation. `recover_mount` uses the mount's reserved slot for detach, cache release or reconciliation; `recover_instance` uses the instance slot for disposal or reconciliation. `dispatch_recovery` pulls these fixed-size typed records even when ordinary queues or limits are exhausted. Reconciliation queries may be replaced after timeout with a fresh identity; the original uncertain effect remains held. Delayed duplicates never match a reused slot. Confirmed old cleanup releases only its old lease, never a replacement. Retained exact ACKs are historical/idempotent; differing duplicates conflict, and evicted terminal IDs expire.

Use `set_eligibility` with increasing revisions for host pressure/priority, recency/idle, keep-alive, unsaved/audio/download/protection and destruction eligibility. Shared-instance destruction checks every consumer and requires supported disposal/unload, host authorization and recoverable reference. Provider-veto close requests a `CloseDecision`, then requires an exact provider-approved permit at new host-approved Core close. The host's original `approved=false`, canonical missing/closed/pin errors and retained replay delegate unchanged. Provider approval cannot override host veto; only successful fresh close consumes permission. Close is separate from detach/disposal.

Asset references and linked-edit/copy/domain-change requests remain opaque host identity/revision plus explicit document/domain undo scope; arrangement movement never edits assets or domain data. Tools bind canonical `ToolId` and pinned `EditingTarget`. Delivery rechecks open/available/visible/usable/applicable origin and target, active attachment and fresh permission. Rebinding away and back invalidates queued actions; toolbar focus never substitutes a target.

Defaults are 1,024 providers, 256 held mounts, 64 instance reservations, 1,024 retained lease entries including retired/Unknown, 1,024 ordinary pending/unsent/terminal records, 65,536 bytes per opaque field/payload, 256 cache/render records and 64 MiB declared cache reservations. Actual retained companion request/receipt opaque copies share a 16 MiB cap; Core receipts and host physical memory are separate. `None` is explicit unrestricted policy. Admission/coalescing errors are atomic; lowering preserves existing obligations while blocking growth. Recovery slots cannot be consumed by ordinary work; unresolved obligations cannot be evicted for admission.

`reserve_cache` records exact instance/owner/consumer declarations. Finite byte policy requires a declared amount; sharing requires a grant. Exact confirmed `CacheRelease` removes one consumer and preserves others. Retired mounts use their recovery slot for cache cleanup before disposal. `cache_bytes` is the known declared subtotal; `cache_unknown_reservations` reports absent amounts separately. Neither is physical GPU/memory usage. `record_sample` takes typed units and measured/estimated/Unknown provenance; `diagnostics()` omits opaque input, asset payload and anchors. Invalidation, animation deadlines, selected/visible/focused/minimized and unknown occlusion remain separate.

On stale scope/permission, reread canonical and protocol state before new intent. On Unknown, reconcile the exact old request before destructive work; do not resend blindly. On exhausted counters, exact existing ACKs remain usable; constructing a fresh controller does not prove old host obligations disappeared. Retain the old controller/host ownership until explicit cleanup. Text layout, selection/scroll, native focus, engines, asset storage, physical usage, GUI/platform behavior and performance need later host integration proof.

</topic>
