---
file_id: hsscrap-gui-manual
file_kind: product-manual
updated_at: 2026-10-23
---

<topic id="integration" status="draft">

`hsscrap-gui` is an optional Rust metadata/tag inspector. The core interaction session owns drafts and state transitions; the egui adapter renders controls. Hosts supply authorized snapshots, canonical tag labels, operation identities, participating owner revision fences, storage and effects. Never derive authorization from a visible button.

Create an `Inspection` from freshly authorized `ItemOwner`/`TagCatalogue` reads. Its current operation scope can differ from the item's owner scope. Supply each authorized context target with its own scope, canonical tag choices, authorized capture records and project memberships. Construct `Inspector`, call `show`, and handle returned actions. `ChooseTag` asks the host to resolve a canonical tag and provide a new `TagAssignment` to `stage_tag`. `Save` asks the host to call `begin_save` with its operation context and execute the returned `OperationPlan` through the owning coordinator. `RemoveTag` retains the assignment ID; its accompanying target is the owning context, including inherited project/folder tags. Hosts must resolve that assignment at its owner, never translate it into removal from the inspected item.

Return save results using the exact `SaveTicket`. Committed results require a fresh inspection before editing again. Unknown outcomes retain the original plan and operation ID for lookup/recovery; do not mint a new save ID or reload away an unresolved operation. Conflicts keep the draft for inspection and require an explicit reload. Revocation clears all displayed private data and drafts, invalidates tickets, and requires a new authorized session. Cancel before dispatch produces no owner command; cancellation after dispatch cannot undo a durable owner effect.

The inspector is an embedded panel. Its host owns modal boundaries, surrounding focus traps, navigation and styling. Escape cancels an editable draft and restores the edit trigger; Escape while inspecting emits `Close`. In-flight and uncertain operations expose status/recovery rather than claiming cancellation.

</topic>

<topic id="research" status="selected">

Research checked 2026-10-23: the existing Cabinet Rust/egui adapter uses egui `=0.33.3` with AccessKit, framework-independent interaction state, fenced host requests and stable accessibility IDs. Reuse that dependency and split. The current hsscrap metadata, tag and operation helpers already own projection, target authorization, revision checks and operation intent validation.

The [egui accessibility guidance](https://github.com/emilk/egui/blob/main/docs/accessibility.md) recommends semantic labels and AccessKit-backed tests. Exact [egui_kittest 0.33.3](https://docs.rs/egui_kittest/0.33.3/egui_kittest/struct.Harness.html) exposes real widget input, keyboard events, resizing and rendered image output. [WAI dialog guidance](https://www.w3.org/WAI/ARIA/apg/patterns/dialog-modal/) supplies the focus/Escape contract when a host embeds this panel in a modal.

Informational origins, source values and provenance use inherited normal body text, rather than egui's reduced-alpha weak text. Error and conflict messages use strong body text. The host theme and disabled controls remain unchanged. [W3C contrast guidance](https://www.w3.org/WAI/WCAG21/Understanding/contrast-minimum.html) and [Windows accessible-text guidance](https://learn.microsoft.com/en-us/windows/apps/design/accessibility/accessible-text-requirements) set the ordinary text target at 4.5:1. The render test checks sampled glyph cores in the generated light/dark images in addition to logging resolved text colors.

Rejected: duplicating transitions in TypeScript; putting GUI dependencies in the core; adding a browser bridge before a host needs it; a new Slint dependency whose current web implementation lacks screen-reader support ([platform documentation](https://docs.slint.dev/latest/docs/slint/guide/platforms/web/)). The established egui pin keeps compatibility with the repository's existing adapters.

Risks and proof: host-supplied authorization can become stale, so every effect is reauthorized at its owner and revocation erases state. Late completions carry a generation ticket. Inherited tags retain their target. Unknown outcomes retain their original operation. Test pure transitions and actual egui controls independently; inspect rendered wide/narrow, light/dark, conflict/error/revocation images. Tests must exercise keyboard, touch, focus restoration, cancel without commands, canonical tag selection and owner-target removal. Source presence does not establish GUI readiness or host integration.

</topic>

<topic id="verification">

Run from the asset repository root, after the applicable preparation gate. Iterate on the pure state target first:

```powershell
cargo test --locked --manifest-path assets/hsscrap/Cargo.toml -p hsscrap --test interaction
```

Release checks include the full owning core package and actual GUI target:

```powershell
cargo test --locked --manifest-path assets/hsscrap/Cargo.toml -p hsscrap
cargo test --locked --manifest-path assets/hsscrap/Cargo.toml -p hsscrap-gui --test inspector -- --test-threads=1 --nocapture
```

The 2026-10-23 local proof used Rust `1.97.1`, added `--offline` to these locked commands, and selected the shared `.local/hsscrap-rss/gameplan.yaml`. Its machine-local environment resolved `CARGO_TARGET_DIR` to the repository's `.local/hsscrap-rss/target` and `HSSCRAP_GUI_OUTPUT` to `.local/hsscrap-gui/proof`. Set these environment variables to absolute paths derived from your checkout; no machine path is compiled into the crate.

To regenerate only the render proof, use the same environment and GUI command with the exact test filter `render_actual_inspector_wide_narrow_and_failure_states` before `--`, then `--exact --test-threads=1 --nocapture`. The test renders the actual egui widgets through WGPU without opening a foreground window. It writes `inspector-{wide-light,edit-wide-light,conflict-wide-light,narrow-dark,edit-narrow-dark,error-narrow-dark,revoked-narrow-dark,provenance-wide-light,provenance-narrow-dark}.png`. Without `HSSCRAP_GUI_OUTPUT`, output goes to the operating system temporary directory under `hsscrap-gui-proof`. Rendering errors fail the test; it never accepts snapshots automatically. Inspect the images, including wrapped source text, narrow field bounds, focus, origins and failure states. The `rendered_contrast` log entries report sampled image pixels, background colors and resolved body text colors.

Fixtures are explicitly synthetic. The save test invokes the existing disposable `ItemOwner` adapter and independently rereads its canonical file before refreshing the inspector. This proves that standalone test-host flow. It does not prove Portal/Desktop integration, live provider acquisition, production authorization or host durability. Hosts must independently authorize operations, validate canonical owner outcomes/receipts and reload current records.

</topic>
