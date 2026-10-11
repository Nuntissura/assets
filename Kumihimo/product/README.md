---
file_id: KUM-PRODUCT-MANUAL-v1
file_kind: product-and-model-manual
updated_at: '2026-10-11'
---

<topic id="purpose-and-boundaries">

# Portable ownership core

`kumihimo-core` is one independently usable Rust package with no runtime external dependencies. It owns an in-memory arrangement of root workspaces, frames, frame-owned child workspaces, ordered views, groups and host resource references. All records are flat: dropping or traversing a deeply nested arrangement does not recursively own a tree of Rust objects.

The host owns actual content, provider engines, synchronization and resource policy. Registering a provider/resource creates a lightweight identity record; it neither starts an engine nor duplicates its data. This package has no GUI, geometry solver, renderer, native popout, engine lifecycle, file persistence or restore API. Future adapters consume the same core commands; they must not maintain a competing canonical arrangement.

</topic>

<topic id="startup" ingestable="true">

## Start and check

From the Kumihimo project root, with Rust/Cargo 1.91+ and the selected platform's linker installed:

```powershell
cargo check --manifest-path product/Cargo.toml --all-targets --locked
cargo test --manifest-path product/Cargo.toml --locked -p kumihimo-core
cargo run --manifest-path product/Cargo.toml --locked -p kumihimo-core --example workspace
```

The console example creates nested ownership, registers a host resource, creates a view and moves a frame while retaining the view/resource identity. It opens no windows. Builds should use the project's admitted serial lane and configured reusable target directory; session instructions are outside this product manual.

To consume the library, add a path dependency on `product/kumihimo-core` from your application's Cargo manifest, relative to that manifest. `src/lib.rs` exports the API; `src/model.rs` defines records/policy; `src/command.rs` defines envelopes/results; `src/core.rs` owns replay; `src/mutation.rs` validates and applies mutations. `tests/contracts.rs` demonstrates consumer use without access to private model storage.

Observed on 2026-10-11 for candidate `a004dbd4c655d6d193e288d21e37dc9987095353`, using stable rustc 1.91.1 on Windows x64 MSVC: the all-targets check passed, one private exhaustion test and nine external contract tests passed, and the console example returned `generation=8 root_frames=[FrameId(2), FrameId(4)] view=ViewId(7) resource=ResourceId(6)`. This proves the admitted in-memory core on that host. Windows ARM64, WASM, native GUI, cross-platform runtime and performance remain unproven.

</topic>

<topic id="api-and-ownership" ingestable="true">

## Operate the core

Create `Core::new(Limits::default(), receipt_capacity)`. Read `core.snapshot()` for version 1 records, generation and effective limits; cloning a snapshot produces detached inspection data and cannot overwrite live state. Use `core.execute(Envelope)` for every mutation. Inputs are actor attribution, an instance-local operation sequence, the expected current generation and one `Command`. Outputs are an attributable `Outcome` containing generation and `Result<Change, Error>`, plus `Response::replayed`.

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

`CreateWorkspace { owner: None }` creates a root. A frame has exactly one placement workspace and at most one child workspace. `CreateFrame` inserts it in the ordered workspace; `CreateWorkspace { owner: Some(frame) }` attaches its child workspace. Frames are independent of split/grid nodes, which are outside this implementation.

`RegisterProvider` declares whether the host supports multiple views of the same resource. `RegisterResource` references it. `CreateView` creates one tab/view placement with a distinct resource reference; a second view of a resource is rejected unless the registered provider declared sharing. Capability declaration is a host assertion, not runtime engine proof. Generated IDs are monotonically allocated across all six entity kinds, start at 1, are typed and never reused within this Core. They are not globally durable IDs; a new Core starts a new scope. `from_raw` creates a target reference only and does not reserve an ID or create a record. Allocation reports exhaustion before wrapping; the largest generated raw ID is `u64::MAX - 1`.

`CreateGroup` stores a frame-local label and optional color; `AssignGroup` checks view/group frame ownership. Group membership is derived from view references. `SelectView` selects one existing view in its owning frame. Creating the first view selects it. Moving the selected view away selects the first remaining source view, or none. An empty destination selects the incoming view. A cross-frame view move clears its frame-local group; resource/provider references remain unchanged. Frame subtree moves preserve every descendant's groups, selection, order and identities.

`MoveFrame` rejects self/descendant containment cycles using iterative ancestry. `MoveView` moves one existing placement. Insertion indexes for both refer to the destination sequence **after removing the moving item**. For `[A, B, C]`, moving B to 0 gives `[B, A, C]`, to 1 is unchanged, to 2 gives `[A, C, B]`; index 3 rejects. Creation indexes refer to the unmodified destination. A same-parent unchanged order, same group assignment, already-selected view or identical policy is an explicit `Change::Unchanged` and does not increment generation.

No operation deletes records or closes host content in this packet. Frames/views can be moved, but closing, group movement, reopening and other future tab behavior are not yet exposed.

</topic>

<topic id="admission-and-replay">

## Admission, ordering and replay

The default practical policy admits at most **16,384 total records**, counting workspaces, frames, providers, resources, groups and views together. Depth and per-frame view limits are optional; the finite aggregate default bounds record admission without making a count or depth an architectural ceiling. `Limits::unbounded()` explicitly opts into host-managed admission; use it only when the host deliberately owns external resource controls. Record limits are not byte/memory ceilings. Actor strings, group labels/colors, retained envelope sizes and any host payload sizes remain host responsibilities.

`max_depth` measures frame depth: root workspace is 0, frames inside it are 1, an owned child workspace has its owner frame's depth, and its child frames add one. New frames must meet the configured depth. Moving deeper checks the deepest descendant frame, while same-depth or shallower moves remain available for recovery after limits are lowered. `max_views_per_frame` counts placements locally: new views and cross-frame moves that increase destination usage must meet it. `max_records` applies to new entity creation globally. `SetLimits` never deletes existing records; lowered limits reject affected new admission. Moves do not allocate records. Policy changes increment generation only when policy differs.

One `&mut Core` serializes commands. The host allocates increasing operation sequences across all actors; independent content work is external to this core. Check `ledger_status().high_watermark.checked_next()` or coordinate equivalent allocation in the host. Sequence gaps are allowed; any sequence at or below the high-water mark without a retained receipt is expired, including an unused skipped sequence. Sequence 0 is invalid. At `u64::MAX`, `checked_next()` returns `None`: this instance cannot accept a higher sequence and must remain inspectable; never wrap or retry on a fresh instance assuming old entity identities survive.

The fixed receipt capacity counts outcomes, including rejections and no-ops. Exact retained **complete envelopes** replay their original outcome before stale-generation validation; the original outcome generation can be older than the current snapshot. Changing actor, expected generation or command under a retained operation ID rejects as `IntentMismatch`. A sequence whose receipt was evicted returns `ExpiredOperation`, never repeats a mutation. Capacity 0 disables replay, while the high-water mark still prevents old sequences from executing. There is no unbounded operation-ID set and no cross-restart deduplication guarantee.

Rejected commands may advance the high-water mark and store/evict bounded receipts. They do not alter model records, generation or entity allocation. A new valid state-changing command increments generation once. Retrying a rejection with corrected intent needs a new sequence and a fresh expected generation.

</topic>

<topic id="errors-and-recovery">

## Inspect failures and recover

`Error` distinguishes missing typed targets, duplicate child ownership, containment cycles, invalid indexes, unsupported sharing, cross-frame group mismatches, practical admission, stale generation, retained intent mismatch, expired/zero operations and exhausted counters. Errors include relevant IDs or expected/observed values and implement Rust's error/display interfaces. All validation precedes record mutation, generation increment and allocation. The invariant-checked internal updates use private records; hosts cannot mutate the live snapshot directly.

On `StaleGeneration`, read the current snapshot and resubmit intentionally with a new operation sequence. On `IntentMismatch`, inspect the original envelope; replay it exactly only while retained. On `ExpiredOperation`, inspect canonical state and reconcile intent before issuing a new sequence; expiration is not permission to blindly repeat. On admission rejection, adjust host policy explicitly or reuse/move existing placements; the previous state survives. On cycle or invalid index rejection, inspect current parent relations and order before changing the request. Counter exhaustion never wraps.

Diagnostics contain arrangement metadata, IDs and host-supplied labels/actor names; do not put secrets or private content in those strings. No filesystem, network, process or clock effects are performed. Snapshot copies are useful for complete pre/post comparison, while ledger diagnostics separately expose operation retention. No pending/unknown external-effect states are emitted because all admitted effects here are synchronous in-memory mutations; future adapters must define their own effect acknowledgment boundary.

</topic>
