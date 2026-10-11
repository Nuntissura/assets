---
file_id: KUM-PRODUCT-MANUAL-v1
file_kind: product-and-model-manual
updated_at: '2026-10-11'
---

<topic id="purpose-and-boundaries">

# Portable ownership core

`kumihimo-core` 0.2 is one independently usable Rust package with no runtime external dependencies. It owns an in-memory arrangement of root workspaces, frames, frame-owned child workspaces, ordered views, groups, host resource references, tool surfaces and flat split/grid packing. Geometry is a pure projection of canonical inputs. All records are flat: dropping or traversing a deeply nested arrangement does not recursively own a tree of Rust objects.

The host owns actual content, provider engines, synchronization and resource policy. Registering a provider/resource creates a lightweight identity record; it neither starts an engine nor duplicates its data. This package has no GUI, renderer, native popout, engine lifecycle, file persistence or restore API. Future adapters consume the same core commands; they must not maintain a competing canonical arrangement.

</topic>

<topic id="startup" ingestable="true">

## Start and check

From the Kumihimo project root, with Rust/Cargo 1.91+ and the selected platform's linker installed:

```powershell
cargo check --manifest-path product/Cargo.toml --all-targets --locked
cargo test --manifest-path product/Cargo.toml --locked -p kumihimo-core
cargo run --manifest-path product/Cargo.toml --locked -p kumihimo-core --example workspace
```

The console example creates nested ownership, moves a frame, supplies geometry, reserves a pinned tool strip and commits a docking preview while retaining view/resource identity. It opens no windows. Builds should use the project's admitted serial lane and configured reusable target directory; session instructions are outside this product manual.

To consume the library, add a path dependency on `product/kumihimo-core` from your application's Cargo manifest, relative to that manifest. `src/lib.rs` exports the API; `src/model.rs` defines records/policy; `src/command.rs` defines envelopes/results; `src/core.rs` owns replay; `src/mutation.rs` applies ownership changes; private `arrangement.rs` validates detached candidates; private `layout.rs`, `geometry.rs`, `solver.rs`, `tools.rs` and `docking.rs` implement packing, geometry and tool/dock policies. `tests/contracts.rs` and `tests/layout_contracts.rs` demonstrate consumer use without access to private model storage.

Observed on 2026-10-11 for candidate `a004dbd4c655d6d193e288d21e37dc9987095353`, using stable rustc 1.91.1 on Windows x64 MSVC: the all-targets check passed, one private exhaustion test and nine external contract tests passed, and the console example returned `generation=8 root_frames=[FrameId(2), FrameId(4)] view=ViewId(7) resource=ResourceId(6)`. This historical WP007 result proves the earlier ownership core on that host; it does not validate the pending 0.2 layout extension. Windows ARM64, WASM, native GUI, cross-platform runtime and performance remain unproven.

</topic>

<topic id="api-and-ownership" ingestable="true">

## Operate the core

Create `Core::new(Limits::default(), receipt_capacity)`. Read `core.snapshot()` for version 2 records, generation and effective limits; cloning a snapshot produces detached inspection data and cannot overwrite live state. Use `core.execute(Envelope)` for every mutation. Inputs are actor attribution, an instance-local operation sequence, the expected current generation and one `Command`. Outputs are an attributable `Outcome` containing generation and `Result<Change, Error>`, plus `Response::replayed`.

```rust
use kumihimo_core::{Command, Core, Envelope, Limits, OperationId};
let mut core = Core::new(Limits::default(), 64);
let response = core.execute(Envelope {
    actor: "host-controller".into(),
    operation: OperationId::new(1),
    expected_generation: 0,
    command: Command::CreateWorkspace { owner: None },
});
assert!(response.outcome.result.is_ok());
```

`CreateWorkspace { owner: None }` creates a root. A frame has exactly one placement workspace and at most one child workspace. `CreateFrame` inserts it in the ordered workspace; `CreateWorkspace { owner: Some(frame) }` attaches its child workspace. Frames retain stable identity independently of plan-local split/grid nodes.

`RegisterProvider` declares whether the host supports multiple views of the same resource. `RegisterResource` references it. `CreateView` creates one tab/view placement with a distinct resource reference; a second view of a resource is rejected unless the registered provider declared sharing. Capability declaration is a host assertion, not runtime engine proof. Generated IDs are monotonically allocated across all seven entity kinds, start at 1, are typed and never reused within this Core. They are not globally durable IDs; a new Core starts a new scope. `from_raw` creates a target reference only and does not reserve an ID or create a record. Allocation reports exhaustion before wrapping; the largest generated raw ID is `u64::MAX - 1`.

`CreateGroup` stores a frame-local label and optional color; `AssignGroup` checks view/group frame ownership. Group membership is derived from view references. `SelectView` selects one existing view in its owning frame. Creating the first view selects it. Moving the selected view away selects the first remaining source view, or none. An empty destination selects the incoming view. A cross-frame view move clears its frame-local group; resource/provider references remain unchanged. Frame subtree moves preserve every descendant's groups, selection, order and identities.

`MoveFrame` rejects self/descendant containment cycles using iterative ancestry. `MoveView` moves one existing placement. Insertion indexes for both refer to the destination sequence **after removing the moving item**. For `[A, B, C]`, moving B to 0 gives `[B, A, C]`, to 1 is unchanged, to 2 gives `[A, C, B]`; index 3 rejects. Creation indexes refer to the unmodified destination. A same-parent unchanged order, same group assignment, already-selected view or identical policy is an explicit `Change::Unchanged` and does not increment generation.

No operation deletes records or closes host content in this packet. Frames/views can be moved, but closing, group movement, reopening and other future tab behavior are not yet exposed.

</topic>

<topic id="admission-and-replay">

## Admission, ordering and replay

The default practical policy admits at most **16,384 total records**, counting workspaces, frames, providers, resources, groups, views and tools together. Depth and per-frame view limits are optional; the finite aggregate default bounds record admission without making a count or depth an architectural ceiling. `Limits::unbounded()` explicitly opts into host-managed admission; use it only when the host deliberately owns external resource controls. Record limits are not byte/memory ceilings. Actor strings, group labels/colors, retained envelope sizes and any host payload sizes remain host responsibilities.

`max_depth` measures frame depth: root workspace is 0, frames inside it are 1, an owned child workspace has its owner frame's depth, and its child frames add one. New frames must meet the configured depth. Moving deeper checks the deepest descendant frame, while same-depth or shallower moves remain available for recovery after limits are lowered. `max_views_per_frame` counts placements locally: new views and cross-frame moves that increase destination usage must meet it. `max_records` applies to new entity creation globally. `SetLimits` never deletes existing records; lowered limits reject affected new admission. Moves do not allocate records. Policy changes increment generation only when policy differs.

One `&mut Core` serializes commands. The host allocates increasing operation sequences across all actors; independent content work is external to this core. Check `ledger_status().high_watermark.checked_next()` or coordinate equivalent allocation in the host. Sequence gaps are allowed; any sequence at or below the high-water mark without a retained receipt is expired, including an unused skipped sequence. Sequence 0 is invalid. At `u64::MAX`, `checked_next()` returns `None`: this instance cannot accept a higher sequence and must remain inspectable; never wrap or retry on a fresh instance assuming old entity identities survive.

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

`ToolPolicy::new` defaults to pinned binding, visible-and-usable target visibility and hidden-space release. Keyboard focus never hides or retargets a tool. Hidden-space retain keeps its reservation. Visibility/usability/applicability are host assertions; the core invokes no provider commands and performs no lifecycle effects. `BindingPolicy::ActiveCompatible` records host intent; even that policy never switches targets automatically. `RebindTool` accepts only a host-declared compatible view/resource pair. Moving outward or returning retains origin/owner/target; edge moves select the corresponding allowed orientation.

Use `DockIntent` for existing frame/tool placement, bounded `Float` or `Redock`. Frame destinations require the destination context's explicit eligibility; tool destinations require its allowed policy. Float rectangles must satisfy minima and fit the complete workspace viewport, preserve one workspace owner and report `SurfaceKind::InAppFloat`. Their saved return location prefers eligible workspace append, otherwise another currently validated eligible destination in that workspace; floating rejects if none can redock. Later host policy changes can still require a new explicit dock intent. Native windows and modal overlays are not exposed.

`Core::preview(intent)` validates/solves a detached hypothetical state without consuming live generation, entity allocation or ledger outcomes. Inspect its private token's accessors; consume it with `cancel()` or wrap it in `Command::CommitPreview`. Commit rechecks generation and intent through the existing Envelope path, imports no geometry, and exact retained replay mutates only once. Every changed canonical topology, context, target state or policy invalidates an older token. `snap_candidates` projects only explicitly eligible current edges/shared frame boundaries/grid cells within the supplied radius, ordered by distance then location; snapping adds no arbitrary subdivisions, overlap or hysteresis.

The default aggregate `max_layout_entries` is 65,536. `layout_entry_count()` counts each retained node plus leaf/child reference, grid track/cell reference, context plus map/eligibility entry, four tool policy/state records plus allowed/compatible/orientation entries, and two records per float. Derived automatic plans/solver output are not retained metadata. Limits count records/entries, not bytes; lowering preserves existing state and allows unchanged/decreasing metadata recovery while rejecting growth beyond the new limit. Unrestricted policy remains explicitly host-controlled.

Snapshot v2 and package 0.2 are an unreleased breaking public-record/command extension; no migration or file-restore guarantee follows. Equalize, swap, maximize and restore remain carried to WP009.

</topic>

<topic id="errors-and-recovery">

## Inspect failures and recover

`Error` distinguishes missing typed targets, duplicate child ownership, containment cycles, invalid indexes, unsupported sharing, cross-frame group mismatches, practical admission, stale generation, retained intent mismatch, expired/zero operations and exhausted counters. Errors include relevant IDs or expected/observed values and implement Rust's error/display interfaces. Arrangement changes validate a detached candidate before replacing live records, generation or allocation; rejected candidates leave canonical state intact. The invariant-checked internal updates use private records; hosts cannot mutate the live snapshot directly.

On `StaleGeneration`, read the current snapshot and resubmit intentionally with a new operation sequence. On `IntentMismatch`, inspect the original envelope; replay it exactly only while retained. On `ExpiredOperation`, inspect canonical state and reconcile intent before issuing a new sequence; expiration is not permission to blindly repeat. On admission rejection, adjust host policy explicitly or reuse/move existing placements; the previous state survives. On cycle or invalid index rejection, inspect current parent relations and order before changing the request. Counter exhaustion never wraps.

Diagnostics contain arrangement metadata, IDs and host-supplied labels/actor names; do not put secrets or private content in those strings. No filesystem, network, process or clock effects are performed. Snapshot copies are useful for complete pre/post comparison, while ledger diagnostics separately expose operation retention. No pending/unknown external-effect states are emitted because all admitted effects here are synchronous in-memory mutations; future adapters must define their own effect acknowledgment boundary.

</topic>
