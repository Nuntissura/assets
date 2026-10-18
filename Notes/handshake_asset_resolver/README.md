---
file_id: handshake-asset-resolver-readme
file_kind: module-manual
updated_at: 2026-10-18
---

<topic id="api-and-use">

# Asset resolver

`handshake_asset_resolver` preserves the existing media-reference validation, resolution, typed errors and per-editor cache from WP-KERNEL-012. Its public exports come from `types`, `resolution` and `cache`.

Use `validate_asset_ref` for one opaque asset ID. It trims whitespace and rejects empty references, absolute paths, schemes, separators, traversal substrings and malformed IDs in the existing order. `parse_asset_ref_list` splits a comma list but does not validate its members. `collection_ref_id` recognizes `collection:` membership references.

Await `resolve_one(kind, workspace_id, ref_value, base_url, fetcher)` to validate a reference, fetch metadata, verify its asset ID, workspace ID and MIME, then obtain URLs from the host fetcher. It returns `ResolvedAsset` or `EmbedError`. Metadata resolution has the existing 15-second operation deadline.

Await `resolve_sequence` for a comma list or collection reference. It rejects empty lists and lists exceeding 100 members, then returns ordered `SequenceItem` results with per-member errors. Its default semaphore allows six concurrent member resolutions. Use `resolve_sequence_with_budget` with the host's shared semaphore when work across multiple embeds must share that budget. Dropping the parent sequence future aborts its owned member tasks. `run_with_deadline` exposes the existing timeout mapping to `EmbedError::TimedOut`.

`EmbedResolutionCache` stores resolution states by asset ID. Mark an entry `Resolving` before starting work, then replace it with `Ok` or `Err`. `needs_fetch` returns false for resolving and terminal entries. Remove an entry for an explicit retry; clear the cache when switching workspaces. Cached state is in memory only.

</topic>

<topic id="host-boundary-and-proof">

## Host boundary and proof

The manifest is unchanged: `serde = "1"` with `derive`, `thiserror = "2"`, and `tokio = "1"` with `rt`, `sync` and `time`. Existing tests additionally enable Tokio `macros` and `rt-multi-thread`. Async resolution requires a host-supplied Tokio runtime with time enabled; sequence resolution spawns owned member tasks. This package does not start a runtime.

Implement `AssetMetadataFetcher` in the host for metadata, content, presentation tiers, collection membership and URL construction. Default content and collection methods return typed failures; the default tier method delegates to content fetching. The resolver has no built-in HTTP client, machine-local asset root, native/Core dependency or authority over host storage. Exported body-size, image-dimension and connection-timeout constants are host integration inputs; the fetcher and decode pipeline must apply their relevant controls.

Inspect `EmbedError` and its stable `kind_str` for failures. A metadata workspace-ID or asset-ID mismatch returns a server error, and an incompatible MIME returns a kind mismatch. Retry policy, authentication, response-body limits, decoding, textures, UI error presentation and diagnostics remain host responsibilities. Preserve the same workspace boundary when clearing or reusing cached state.

All five existing files are byte-identical to the approved source guards, including 23 existing test declarations. This extraction has not run a compiler, tests, network or runtime. Standalone locked proof, source publication and independent canonical remote verification precede a separate pinned host adoption. Original real-asset, backend, GUI, cancellation, cache and independent acceptance obligations remain outstanding.

</topic>