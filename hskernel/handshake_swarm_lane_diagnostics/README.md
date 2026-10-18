---
file_id: handshake-swarm-lane-diagnostics-readme
file_kind: module-manual
updated_at: 2026-10-18
---

<topic id="module-api" status="source-only-unrun" version="0.1.0">

# Swarm lane diagnostics

This standalone source package contains the existing Swarm lane diagnostics DTOs, wire decoder, validator and author-ID formatters. Public names, serde fields, schema IDs and author-ID strings are retained from the native source module. Dependencies are only the Rust standard library, serde with derive, and serde_json.

The crate root exports the model declarations, constants, `decode_projection_for_native_surface`, `validate_projection_for_native_surface`, `scoped_author_id`, and the run, routing, lane, message, tier and MT author-ID formatters. The three implementation modules are private. Token conversion and author registration remain private helpers.

Models consuming backend JSON enter through `decode_projection_for_native_surface(serde_json::Value)`. It returns a validated `SwarmLaneDiagnosticsProjection` or the existing diagnostic error string. It rejects wire changes that lose or alter fields during typed round-trip serialization, then applies the existing validation laws. Consumers with an existing typed projection can call `validate_projection_for_native_surface(&projection)`.

The package performs no HTTP, storage, authentication, grants, GUI rendering, task scheduling or process operations. The delivery cell, transport trait, pane, filters and clock remain host behavior. Successful validation checks projection shape and attribution fields; it does not establish authenticated access or canonical backend state. Error recovery stays with the host producer and its existing policy.

</topic>

<topic id="source-maintenance-and-proof" status="source-only-unrun" version="0.1.0">

The source reference is `src/frontend/handshake_native/src/swarm_lane_diagnostics.rs` in the assigned Handshake product worktree, SHA256 `1e602725beb6195faba13da777dd3f064aa90be107a54f82029bd1daa86da594`, 76096 bytes. Model declarations and decoder bodies are copied without behavior changes; validator and formatter ownership is separate within this package.

The owning contract is WP1 MT008 `phase2_swarm_lane_diagnostics_source_extension_20261018`. The inspected source has no inline test declarations. Existing composition, native, backend and Argus tests remain with their owning targets. No owning test count, compile result, runtime result or acceptance is claimed here.

Current status is source only, uncompiled and unrun. Source review, canonical publication, focused owning proof and an independent immutable source reread precede separately pinned host adoption. Native facade and caller rewiring have not been performed by this extraction. Source maintenance and adoption use the owning contracts; the package does not supply repo workflow authority.

</topic>