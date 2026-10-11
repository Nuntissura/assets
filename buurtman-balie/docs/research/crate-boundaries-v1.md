---
file_id: BB-RESEARCH-CRATE-BOUNDARIES-v1
file_kind: research-basis
updated_at: 2026-10-11
---

<topic id="selected-boundaries">

# Core, presentation and host boundaries

Operator selection: GUI comes from the host; keep core and host/presentation in separate crate/package groups. Windows x64 and ARM64 are first targets; macOS/Linux follow. This research records packaging boundaries, not cross-platform runtime proof.

Inspected current files: root `Cargo.toml`, `src/contracts.rs`, `src/service.rs`, `src/surreal.rs`, `web/package.json`, and `crates/buurtman-balie-host-demo/Cargo.toml`. Root remains package `buurtman-balie` 0.2.0 with library API unchanged. Its include list selects Cargo metadata, README, Rust source/tests and toolchain; no web assets or governance. Core dependencies are serde/serde_json plus optional rusqlite. Surreal uses injected host transport. No GUI dependency is selected for the core.

Root workspace explicitly lists `crates/buurtman-balie-host-demo` and sets `default-members = ["."]`. Host demo owns `examples/standalone.rs` and `examples/json_bridge.rs`; it consumes the core through a relative path. Optional browser package remains `web/package.json`, with its own explicit export/files allowlist. These are demo/browser composition surfaces; production hosts own GUI, authorization, lifecycle and external resource resolution. A proposed optional collaboration crate would fit this split without requiring presentation dependencies in core; CRDT implementation selection remains separate.

</topic>

<topic id="source-basis-and-validation">

## Source basis and proof boundary

The [official Cargo workspace reference](https://doc.rust-lang.org/cargo/reference/workspaces.html) explains root packages, explicit member/default selection, shared lockfile/output and package selection. Preserve the root package rather than converting to a virtual workspace or renaming the core. Explicit member paths keep independently maintained optional packages visible without selecting them for default core builds. A shared lockfile is dependency coordination, not permission to add host GUI dependencies to core.

The [Cargo target reference](https://doc.rust-lang.org/cargo/reference/cargo-targets.html) distinguishes library, binary and example targets. Separate host-demo examples prevent core packaging from shipping demo composition. The [publishing reference](https://doc.rust-lang.org/cargo/reference/publishing.html) describes package inspection; affected proof must list each package's actual contents and verify governance, private backups/database state, outputs and unintended presentation assets remain excluded. `publish = false` remains selected; no publication/deployment is authorized.

[Microsoft's Arm emulation reference](https://learn.microsoft.com/en-us/windows/arm/apps-on-arm-x86-emulation) distinguishes native Arm applications from emulated x64/x86 user-mode execution. Keep target labels explicit: native `aarch64-pc-windows-msvc` proof and x64 `x86_64-pc-windows-msvc` execution under Windows Arm emulation prove different environments. A cross-build alone proves neither runtime. Native physical x64 acceptance remains separately labeled until observed. macOS/Linux remain later targets without implied compatibility.

Validation after boundary edits: affected workspace package metadata/dependency graph, core and host-demo package lists, core library/tests and changed example builds, current docs/harness binary path resolution, then declared Windows target runs with architecture recorded. Reuse unchanged browser proof unless presentation/transport behavior changed. Do not run unrelated future-platform suites. This document itself changes no build inputs and triggers no build/test.

</topic>
