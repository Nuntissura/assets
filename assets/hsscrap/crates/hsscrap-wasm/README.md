---
file_id: hsscrap-wasm-manual
file_kind: product-manual
updated_at: 2026-10-23
---

<topic id="research" status="selected">

Research inspected 2026-10-23: Portal's existing Cabinet browser and Handpick bridges use wasm-bindgen `0.2.129`, bounded JSON strings, explicit host effects and actual Node WebAssembly instantiation. Reuse their boundary convention. The [wasm-bindgen arbitrary-data guide](https://wasm-bindgen.github.io/wasm-bindgen/reference/arbitrary-data-with-serde.html) documents JSON strings as an alternative to a separate JavaScript object conversion layer; [number guidance](https://wasm-bindgen.github.io/wasm-bindgen/reference/types/numbers.html) distinguishes JavaScript numbers from BigInt. Existing hsscrap revisions and durations already use decimal strings. Preserve those encodings instead of introducing another integer convention. The [serde-wasm-bindgen serializer](https://docs.rs/serde-wasm-bindgen/latest/serde_wasm_bindgen/struct.Serializer.html) is useful for native object conversion, but adds no benefit to the existing JSON wire convention.

The published core `interaction::Session` owns editing, tag changes, operation plans, exact save tickets and uncertain-outcome recovery. The published separate `hsscrap-gui::Inspector` owns egui controls. The bridge converts its own versioned DTOs into those APIs; it does not add Serde or GUI dependencies to the core. `panel` actually calls the Rust inspector and returns bounded semantic control rectangles/actions for integration diagnostics. It does not install a browser renderer or independently implement the domain in JavaScript.

Risks: stale responses can reuse an operation UUID after reload; retain the exact native save ticket behind an instance-scoped, monotonically increasing decimal bridge token. Untrusted recursive extensions can exhaust parsing; reuse the owning strict JCS raw admission, including duplicate/depth rejection, before typed conversion. Host-supplied authorization can expire; bridge input is never an access grant, and every effect still requires canonical owner admission. Reject unsupported required features through the core. Test compiled WASM against native Rust outputs, including malformed/oversized input, edits/tags, unknown outcomes, stale tickets and large revisions. Compilation alone does not prove that boundary.

</topic>

<topic id="integration" status="draft">

Construct `HsscrapBridge` with a version-1 JSON envelope containing `inspection` and `instance_id`. The host generates a fresh nonnil canonical UUID for each bridge lifetime and never reuses it; it is correlation, not authority. That snapshot must come from fresh host authorization. `snapshot`, `apply` and `panel` return versioned JSON; failures return content-free codes. Bridge input is capped at 1 MiB, output at 4 MiB, text/URL bounds at 64 KiB and individual collections at 1024. Strict raw admission also limits nesting. These are adapter ceilings; hosts supply smaller positive core `Bounds` where appropriate.

`apply` accepts `{ "version": 1, "action": ... }`. Actions delegate to the core: begin_edit, set_title, set_description, add_tag, remove_tag, undo_removal, cancel, begin_save, complete, reload and revoke. A save returns the existing core operation plan and owning removal targets. The host executes that plan and returns canonical outcomes with its opaque token. Unknown outcomes retain the same plan/token; resolve the original operation through its owner. No network, storage, owner execution or reconstructed operation-only ticket occurs in the bridge. Reload requires a newly authorized inspection. Revocation clears private state and invalidates pending tokens.

Integral JSON values outside the JavaScript safe range are rejected before typed conversion, including opaque extension integers. Use decimal strings for precise larger integers; published core revisions/durations already do so. Floating-point fields retain the existing binary64 semantics. The raw JCS helper supplies syntax/duplicate/depth admission; this bridge owns the additional integer policy.

`panel` accepts version, width and height, runs the actual egui inspector without a foreground window and returns control IDs/rectangles and requested host actions. Its diagnostic projection is not browser visual acceptance. Host modal/navigation/rendering, owner trust and durability remain separate.

The bridge is a separate optional workspace package. Core and native-only consumers do not depend on it. Focused native and actual WASM proof passed locally. Canonical publication and host adoption remain separate.

</topic>

<topic id="verification" status="locally-passed">

After the applicable preparation gate, run from the asset repository root:

```powershell
cargo test --locked --manifest-path assets/hsscrap/Cargo.toml -p hsscrap-wasm --test wire_contract
python assets/hsscrap/tests/wasm/prove.py
```

The first command compares key bridge actions directly with the owning native `Session`. The driver builds the native fixture producer, compiles the library for `wasm32-unknown-unknown`, generates web bindings with exact wasm-bindgen `0.2.129`, and instantiates those bytes in Node to compare versioned outcomes. It requires that Rust target and a CLI matching the pinned Rust dependency. Configure `HSSCRAP_WASM_BINDGEN` when the exact CLI is outside PATH; `HSSCRAP_NODE` and `HSSCRAP_CARGO` optionally select existing tools. Preserve the chosen toolchain and absolute canonical `CARGO_TARGET_DIR`. `HSSCRAP_WASM_OUTPUT` selects ignored proof outputs; by default they live below that Cargo target. No foreground browser, services or provider credentials are required.

Inspect failure diagnostics and fix the failing predecessor before broader proof. The driver prints captured native diagnostics before propagating failure. Generated bindings and fixtures are proof outputs, not authored asset source. Local proof on 2026-10-23 passed four direct native cases across the initial run and focused negative-fixture remediation. Valid dotted extension keys preserve a 65-array value; 129 arrays reject through raw admission. The actual compiled module passed all 13 native comparison scenarios and 41 steps in Node using wasm-bindgen 0.2.129. The driver preserves Cargo proxy executable names. Canonical isolated publication and browser/host adoption remain separate gates.

</topic>
