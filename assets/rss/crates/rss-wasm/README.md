---
file_id: rss-wasm-manual-v1
file_kind: package-manual
updated_at: 2026-10-23
---

<topic id="research">

Inspected current primary sources on 2026-10-23: [wasm-bindgen data conversion](https://wasm-bindgen.github.io/wasm-bindgen/reference/arbitrary-data-with-serde.html), [numeric boundaries](https://wasm-bindgen.github.io/wasm-bindgen/reference/types/numbers.html), [AbortSignal](https://developer.mozilla.org/en-US/docs/Web/API/AbortSignal), and [WAI feed interaction](https://www.w3.org/WAI/ARIA/apg/patterns/feed/). Inspected the published RSS query/wall reducers and hsscrap browser bridge at dc8e881. Reuse their actual native behavior through bounded versioned JSON strings and the existing wasm-bindgen 0.2.129 toolchain. A separate JavaScript object serializer adds another conversion convention without improving the existing host contract. Revisions retain the canonical decimal-string encoding; bridge ticket counters also use strings. Reject unsafe integral JSON numbers, including integral-valued decimal/exponent spellings.

Retain exact native tickets behind fresh-instance tokens. Reconstructing a native ticket from an item or operation ID would weaken its generation fence. Host cancellation must abort its own work and complete or invalidate the bridge request; an AbortSignal alone does not revoke a retained Rust ticket. Restart and profile transitions invalidate every prior result. Duplicate/deep JSON, stale completions, revoked access, malformed owner projections and retained-state growth are the affected failure cases. Validate them through direct native-owner comparisons and actual compiled WebAssembly instantiation. Native fixtures do not establish a live provider, durable commit, host database snapshot, browser layout or Portal adoption.

</topic>

<topic id="verification" status="locally-passed">

After registration, source review and the serial Cargo preparation gate, run from the asset repository root:

```powershell
cargo test --locked --manifest-path assets/rss/Cargo.toml -p rss-wasm --test wire_contract
python assets/rss/tests/wasm/prove.py
```

The native target compares query pages/revocation with `QuerySession`, player effects with `WallState`, composed editing/owner plans/unknown outcomes with `hsscrap::interaction::Session`, and canonical source selection with `WallQuery::from_library`. It also rejects foreign inspection reloads, unsafe numbers and cross-instance completions. All six cases passed locally on 2026-10-23.

The driver builds the native fixture producer and actual `wasm32-unknown-unknown` library, generates exact wasm-bindgen 0.2.129 web bindings, then imports and instantiates those bytes in Node. It compares versioned native results with twelve compiled-WASM scenarios covering paging sizes, anchor refresh, cancellation/restart/denial, one-player/media invalidation, hsscrap GUI diagnostics and draft/unknown outcomes, decimal revisions, strict raw limits, library selection, bounded handles and retained projections. All twelve scenarios and 128 steps passed locally on 2026-10-23 using Rust 1.97.1, wasm-bindgen 0.2.129 and Node 24.11.1. The affected wall query target passed all sixteen cases, and the serial GUI/WGPU target passed all ten cases with informational glyph-core checks in both themes. Configure `RSS_WASM_BINDGEN` for the exact CLI, `RSS_NODE` or `RSS_CARGO` for existing tools, `CARGO_TARGET_DIR` for the shared compiler output, and `RSS_WASM_OUTPUT` for ignored proof output. Paths resolve from the script's owning workspace. The driver preserves Cargo proxy names and prints captured failure diagnostics before propagating failure.

Use a new bridge instance after browser/module failure. Reauthorize and query canonical storage, then reconcile host-owned pending writes through their original operation IDs. Cancel emitted read/media effects and dispose decoders/textures. A lost browser response never proves an owner write failed. Do not retry an uncertain write as a new operation or restore authorization from a serialized projection.

</topic>

<topic id="integration" status="locally-passed">

`RssBridge` owns one ephemeral query, wall and optional published `HsscrapBridge`. The host supplies fresh canonical query/library/selection data after authorization, executes emitted query/card/media/inspection effects and independently authorizes every owner read/write. Tokens are correlation, never access grants. The bridge runs no provider network, credentials, database, decoder or owner mutation.

The version-1 constructor accepts a canonical query, positive bounds, maximum loaded items and a fresh nonnil canonical UUID instance ID. `apply` returns the current view, emitted effects and any structured core error. Boundary admission errors return content-free codes. A current denial may clear state and therefore remains visible in the returned view. Reset, revoke and cancellation invalidate retained handles. Recreating a bridge requires a new instance ID and fresh authorization; no saved browser object restores authority.

Query and card completions carry the exact returned opaque token. Media resolution stays lazy and playback requires an explicit action. Cancellation delegates to `WallState::cancel_media`, which invalidates the exact resolver, permits a cancelled preview to be requested again and preserves already-ready media. A pending playback resolver prevents rendering from starting a competing preview. Execute every stop/cache-invalidation effect before presenting cached content. Inspection replacement and closing emit cancellation for retired host read tokens after the draft guard passes. Inspection admission uses the owning wall validator before composing the existing hsscrap browser bridge; details actions preserve its exact save tickets, draft/unknown/conflict behavior and canonical owner plans. A diagnostic details panel calls the actual published Rust GUI. It is separate from browser rendering acceptance.

Adapter ceilings are 1 MiB input, 16 MiB output, 64 KiB encoded query, 2 MiB per retained row window and per card projection, 1024 loaded items, 32 concurrent retained handles, 64 KiB text/URL and 1024 elements per supplied collection. The current and staged refresh row windows can coexist, each bounded independently. The output ceiling accommodates the bounded RSS projection and the existing hsscrap details result together. Every supplied rule keyword and string-bearing health payload obeys the host text bound, including unselected sources/groups. Host bounds may be smaller. JSON admission uses the existing strict canonical JSON owner for syntax, duplicate keys and depth before typed conversion. The package is registered and standalone proof passed locally. Canonical isolated publication and actual Portal/browser/provider adoption remain separate gates.

</topic>
