---
file_id: CABINET-MANUAL-001
file_kind: asset_manual
updated_at: 2026-10-14
---

<topic id="purpose">

# Cabinet

Portable Rust hierarchy navigation with host-owned operations. Database folders, files, documents and bookmarks can share tree mechanics while retaining their own storage semantics.

The default crate and its Peak subcrate have no external runtime dependencies. Hosts provide the GUI. Separate `cabinet-egui` and `cabinet-peak-egui` crates provide optional egui adapters for hosts that use that framework. Cabinet does not connect to Notes, Forgejo, HandForge, databases, networks or the filesystem. Explicit decoder helpers consume authorized bytes supplied by the host.

</topic>

<topic id="operate">

## Use

Supply a `Snapshot` of stable entries and a scoped `Context`, then construct `Cabinet::new(snapshot)`. The host supplies resource IDs, revision numbers, capabilities, child loading state and complete folder ancestry. `EntryId` identifies a tree occurrence; `resource_id` identifies the underlying resource, so bookmarks and folder memberships may represent the same resource separately.

Depend on `cabinet-egui` and import `cabinet_egui::CabinetView` to call `CabinetView::show(ui, &mut cabinet)`. Execute each returned `Request` through the owning application. Before writing, recheck its context, entry revisions, permissions and canonical hierarchy. Return `Completion` with a canonical snapshot on success or a diagnostic error on failure. A success without a canonical reread is not persistence proof.

`reveal(id)` expands ancestor paths and selects the entry. Hosts should call it when the active editor changes. `replace(snapshot)` refreshes entries while retaining live expansion and selection in the same context. A scope or generation change clears private interaction and history state. Never reuse a generation after account/workspace revocation.

Folder-plus creates a folder at the selected folder or root. Create/rename use inline editors: Enter submits, Escape cancels. Arrow keys navigate; Shift extends a range; Ctrl/Cmd toggles selection. F2 renames, Enter opens, and Delete requests confirmation. By default, single-click opens a file after the configured double-click interval; double-click starts rename and cancels that open. Modifier clicks select without opening. Settings also offer single-select/double-open. Drag entries inside folders, between siblings or to the root. Invalid destinations are rejected. Creation, deletion and moves are host intentions, not filesystem side effects.

Undo/redo use host-issued compensation tokens. The host owns their lifetime and conflict checks. Failed writes preserve the previous canonical snapshot and expose an error; recover by refreshing authority and retrying with a new request.

One request may be pending at a time, including open and lazy loading. `acknowledge(request_id, context)` completes read operations without cloning a snapshot; it rejects write operations and stale contexts. `abandon_pending(request_id)` releases local waiting after a timeout; it does not cancel host execution. Reconcile canonical state before retrying. A canonical refresh that supersedes a pending request reports that uncertainty. Inline drafts cancel on revision refresh to avoid overwriting another actor's rename.

Before/after placement is available where the destination folder uses Manual order, between entries of the same kind; other sorts permit folder/root destinations. Filtering removes hidden action selections. Folder ancestry must be complete before moves or deletion, even when leaf membership is lazy. Hosts enforce physical filename rules and virtual-root permissions; Cabinet permits names appropriate to database folders.

</topic>

<topic id="peak-and-options">

## Peak and entry options

[Peak](Peak/README.md) is independently reusable. Hover first shows the full name and colored tags, then expands a resizable preview after the configured delay. Escape dismisses it; pointer travel into the preview keeps it open. Its requests are separate from the durable operation slot.

Execute `ViewResponse.preview_requests` in an owned worker and return content through `view.state.peak.complete(completion)`. The request identifies account/workspace scope, generation, context revision, occurrence, resource and entry revision; late results cannot replace another hover target. Preview bytes must be authorized by the host. Use host thumbnails where available; do not read whole files on the GUI thread.

UTF-8 and BOM-marked UTF-16 source files preview as plain text regardless of programming language. `preview-images` enables common image codecs; `preview-raw` adds embedded camera previews and optional explicit RAW development. HEIC/HEIF and standalone AVIF/JPEG-XL require host providers. RAW model/compression support and native worker isolation require consuming-host proof; see Peak's coverage limits.

The row `…` and right-click menu share Open, Duplicate, Download, Open parent folder, Copy path/link, Properties, Rename, Move to, Color, Tags, and Delete. Folders also expose independent sorting and expand/collapse. Capabilities disable unavailable actions. Web hosts execute Download through their authorized browser/download service; desktop hosts select their own export destination. Cabinet never launches an external application or copies credentials into a download URL.

Color editors provide a hue/saturation wheel, brightness and validated `#RRGGBB`. Edits remain drafts until Apply. `SetColor` decorates the occurrence; `SetTags` resolves the underlying resource in the host. The host decides whether tag color changes update a shared tag registry. `view.set_tag_catalog(context, tags)` supplies existing stable tags and clears across workspace generations. New tag IDs are host-validated. Loom currently supplies tag-hub identities and membership; persistent selected tag colors need a host storage extension.

</topic>

<topic id="persistent-settings">

## Host-owned persistent settings

`cabinet.settings()` exposes all settings. `apply_settings(saved)` restores them atomically. Enable `serde` for JSON serialization; `ViewResponse.settings_changed` tells the host when to save. Store the document per authenticated tree scope, debounce slider writes and reapply the last acknowledged value on a failed save. Cabinet's local preference update alone does not prove durable persistence.

The toolbar sets `tree_sort`. `folder_sorts` contains only explicit overrides for a folder's direct children; no copied defaults or implicit ancestor inheritance. Changing the toolbar default leaves overrides intact. “Use tree default” removes an override. Options cover name, file type, size, created and modified date in both directions, plus Manual. Unknown size/type metadata sorts last. The host supplies real metadata; Cabinet never scans storage for it.

Peak settings expose name delay, expansion delay, leave grace, width, height, input bytes, text lines, decode pixels, output pixels and enablement. Resizing emits a settings change. The host can expose these in its own preferences UI. Suggested host controls: preview-provider permissions, decode-worker concurrency, cache budget, metered-network loading, reduced motion and syntax-highlighting themes. These are host policies rather than hidden Cabinet services.

</topic>

<topic id="preview-scheduling">

## Bounded preview scheduling

For the optional native scheduler path, call `view.enable_scheduler(peak::SchedulerSettings::default())` before showing the tree. Repeated calls reconfigure limits atomically without discarding active worker tokens. Execute `ViewResponse.preview_jobs` in host-owned workers using each job's settings, then call `view.complete_scheduled(token, content)`. Handle `preview_cancellations` as cancellation intentions. Call `acknowledge_preview_cancelled` only after proving the worker stopped; sending a cancellation does not release its slot. This mode does not also emit legacy `preview_requests`.

Direct `preview_requests` remain the default compatibility path; the host must supply its own resource bounds there. Standalone Peak users can call `tick_scheduled` with `PreviewScheduler`. Scheduler settings expose queue, concurrency, cached-byte and cached-entry limits independently of popup settings. Cache keys include context/resource/revision and decoding settings; account/workspace generation changes erase old cached payloads. Host worker processes are required when hard memory or execution limits are part of acceptance.

The software screenshot example enables the scheduler and completes demonstration jobs between frames. It launches no native window or decoding worker.

</topic>

<topic id="host-gui-boundary">

## Host GUI and reusable logic

`cabinet` and `cabinet-peak` are framework-independent logic crates. A host owns GUI rendering, layout, styling, event wiring, I/O, and durable operations. Use `cabinet::interaction::InteractionState` with `Cabinet` for reusable edit drafts, context reconciliation, move preflight, lazy expansion, and preview scheduling. Call `sync` against the current Cabinet snapshot before reading interaction state or handling a host event. Canonical refreshes cancel stale drafts; account or generation changes erase private queries, catalogs, and cached previews.

For inline naming, call `begin_create` or `begin_rename`, bind the host text field to `state.text`, and call `submit_edit` on confirmation. Invalid names preserve the draft and return an error. A successful submission emits a fenced host request and clears the draft; it does not mutate canonical entries. `request` selects move preflight for incomplete hierarchy. `expand` can return a load request; the host executes it. `move_destinations` supplies validated destination choices without a GUI framework.

Hosts using egui can embed `cabinet-egui` and `cabinet-peak-egui`; neither crate opens a window or owns a native event loop. `CabinetView::state` is the same headless `InteractionState`, including its `peak` and scheduler. `Palette`, `ViewResponse`, window geometry, texture handles, and egui event translation belong to the adapter crates.

Migration: remove the `egui` feature from `cabinet` and `cabinet-peak` dependencies. Add the separate adapter dependency and change `cabinet::egui_view::CabinetView` to `cabinet_egui::CabinetView`, `cabinet::egui_view::ViewResponse` to `cabinet_egui::ViewResponse`, and `cabinet::peak::egui_view::PeakView` to `cabinet_peak_egui::PeakView`. Change `view.peak` to `view.state.peak`. Core codec and serde features remain in their owning logic crates; the Cabinet adapter forwards them when needed. GUI integration tests and software capture examples live in `cabinet-egui`; core tests run without any adapter dependency. The repository asset index is [assets-index.yaml](../assets-index.yaml), explicitly covering only Cabinet and Peak for now.

</topic>

<topic id="build-and-verify" ingestable="true">

## Build and verify

Run from the Cabinet directory with a configured external `CARGO_TARGET_DIR`. Rust with egui0.33 support is required for the optional native renderer.

```powershell
cargo test --locked
cargo test --locked -p cabinet-egui -p cabinet-peak-egui
cargo run --locked --example headless
cargo run --locked -p cabinet-egui --example render_preview -- <external-output.ppm>
cargo test --locked --workspace --all-features
cargo run --locked --features serde --example settings_persistence -- <external-settings.json>
cargo run --locked --example host_workflows
cargo run --locked --release -p cabinet-egui --example large_tree_gui -- 10000 100000 1000000
```

The headless example uses an explicit demonstration host. Its in-memory operations demonstrate the API; they do not prove an application's durable storage, security or recovery boundary.

The asset contract and research basis are in `spec/cabinet.contract.json`. Tests cover stale delivery, atomic admission, deep nesting, range selection, moves and compensation history. Native tests inspect rendered interaction and accessible identities. Integration must add the owning application's durable and visual proof.

</topic>

<topic id="large-trees">

## Large trees and host updates

`entry(id)` uses a persistent index. `projected_rows()` returns a shared cached projection; prefer it to the compatibility `rows()` method, which creates an owned copy. `is_selected(id)` checks membership directly. The native renderer mounts viewport rows with overscan. Breadcrumbs use a stable height and horizontal scrolling; pointer drags suppress automatic focus scrolling. Diagnostics distinguish loaded canonical entries, projected rows and mounted rows; loaded entries do not prove the entire host workspace was loaded.

`DeltaBatch` binds its base context, next context revision and expected entry revisions. `apply_delta` rejects malformed batches atomically. Hosts remain responsible for authoritative permissions and durable writes. Delta hierarchy validation, cold projection and loaded filtering still traverse loaded entries; full snapshots still hold every admitted entry in memory. Use paging instead of eagerly loading a huge workspace. Folder sort overrides and pins for unloaded folders survive incomplete snapshots; explicit removals and IDs now known to be files retire their folder settings. Never mark partial folder ancestry complete merely to permit an operation.

For paging, call `request_child_page`, execute the emitted `LoadPage` at the host, and return `ChildPage` with the exact request/context/parent/cursor. Use `fail_child_page` for a retryable failure. For incomplete-tree moves, the host checks authoritative ancestry and permissions before issuing a scoped `MoveGrant`; the authorized operation still requires host revalidation. A preflight grant is not proof that a move happened.

Loaded filtering and `WorkspaceSearch` are separate. Return scoped `SearchResult` through `complete_search`; an unloaded search hit is a navigation intention for the host, not a newly admitted canonical entry. Pins, breadcrumbs and quick path navigation use stable occurrence identities. Visibility patterns match case-sensitive entry labels using `*` (any span) and `?` (one Unicode scalar); hiding a folder also hides its descendants. These patterns do not interpret filesystem paths or `.gitignore` rules; watcher exclusions are exposed to the host separately and never claim an external watcher stopped.

`BulkProgress` belongs to a pending host operation. Completion counts and recovery handles are host supplied. Successful completions clear operation progress/recovery. A failed completion can expose recovery without inventing compensation or automatically repeating effects. `examples/host_workflows.rs` exercises these boundaries with an in-memory demonstration host; it does not prove production storage or authentication.

`examples/large_tree.rs` emits JSONL measurements for flat, wide and nested trees at configurable sizes (nested fixtures have a maximum depth of 128), including cold/warm projections, filtering, lookups, operation construction, delta/full refresh and optional native frames. The 2026-10-11 release proof completed nine 10k/100k/1m cases. At 1m entries, mounted rows stayed at 13 or fewer and measured frame p95 was 0.101–0.249 ms; cold admission took 1.021–1.508 s and one-label deltas 0.857–1.842 s. These are instrumented results under concurrent machine load, not a production capacity guarantee. All 105 feature-enabled tests passed; independent source, dependency and visual inspection passed. Detailed results and input hashes are recorded in the asset contract.

The optional allocator instrumentation reports requested Rust heap bytes and allocation calls, not process RSS. Scrollable frame cases assert that wheel input changes mounted row identities. Results apply to the measured machine, source and feature set. They do not establish a universal capacity guarantee. Peak worker-process isolation, physical filesystem operations and production watchers remain consuming-host responsibilities.

</topic>

<topic id="handshake-integration">

## Later Handshake integration

Replace presentation through adapters for both `ProjectTree` and `LoomFolderTree`. Keep authenticated loading, entity-specific open/rename routes, membership operations, canonical refetch, epoch/sequence guards and graph-coloring contributions in Handshake. Preserve stable AccessKit targets and shared theme/context-menu/focus owners. Wire editor changes to `reveal` and successful writes to canonical snapshot refresh.

The Notes adapter must distinguish Loom organizational folders from physical directories. Loaded membership is partial content, not canonical hierarchy coverage. This standalone asset changes neither WP-KERNEL-012 code nor its pinned acceptance. Application-wide removal of Forgejo/HandForge connections belongs to that integration change; it is not achieved by removing Git remotes from the asset repository.

</topic>
