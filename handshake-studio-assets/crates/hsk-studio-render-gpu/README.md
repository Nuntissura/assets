---
file_id: hsk-studio-render-gpu-manual
file_kind: module-manual
updated_at: 2026-10-28
---

<topic id="memory-observation">

# Memory observations

`GpuContext::observe_memory` returns `DeviceMemorySnapshot`, containing the context's device identity and generation plus Vulkan heap and memory-type observations. The call uses the existing device. It does not create a device, allocate resources, submit work, or grant access to a renderer.

Each heap reports its index, size, native flags, current process budget and estimated usage. Each memory type reports its index, heap association and native property flags. These changing driver estimates are not exact per-resource accounting or global machine totals. Device-local and host-visible properties can overlap. Staging is a usage category, not a unique heap classification.

Unsupported backends or missing Vulkan extensions return `MemoryObservationError`; missing support never becomes a successful zero snapshot. Report the typed error and keep unavailable observations explicit. Retry only after the relevant device or capability changes. A driver-reported zero remains zero.

The internal `native-memory` package isolates the native query. The parent crate retains its unsafe-code prohibition. Host authorization, process leases, policy, persistence, framework allocation hooks and resource identities remain outside this observation API. In particular, this API does not issue the trusted GPU service factory.

See `src/memory.rs` and `examples/memory-observation-contract.rs` for the public contract, and `src/lib.rs` for the existing rendering, lifecycle, shared-device and failure contracts. Existing features and adapters remain supported.

</topic>

<topic id="verification" ingestable="true">

## Verification

From the Studio workspace, with an externally configured `CARGO_TARGET_DIR`, compile the integration contract:

```text
cargo check --locked --offline -p hsk-studio-render-gpu --lib --example memory-observation-contract
```

The example checks the public types without creating a device or querying Vulkan. The helper is registered in the Studio workspace and its dependency selector. `tools/prove-native-memory.py --target-dir <existing-external-target>` compiles that selector, runs its affected validation fixtures and checks the committed renderer closure against exact Git inputs. It requires `RUSTC` to select the pinned compiler.

The retained standalone helper proof passed all six conversion and dispatch-validation tests on its original source and lockfile. `native-memory/Cargo.lock` retains those standalone dependency inputs; integrated builds resolve the Studio workspace lockfile. The six cases are not repeated by the integration checks. Neither proof establishes a live Vulkan query, resource reflection, centralized accounting, a trusted factory, or host acceptance.

The query follows the [Vulkan memory-budget contract](https://docs.vulkan.org/refpages/latest/refpages/source/VkPhysicalDeviceMemoryBudgetPropertiesEXT.html) and the pinned wgpu 27.0.1, wgpu-hal 27.0.4 and ash 0.38.0 interfaces. Vulkan 1.1 uses the core properties query; older supported instances require the properties2 extension. The implementation checks capability and bounds before exposing copied observations.

</topic>
