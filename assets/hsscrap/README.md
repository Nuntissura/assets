---
file_id: hsscrap-manual-v1
file_kind: package-manual
updated_at: 2026-10-23
---

<topic id="model-contract">

# hsscrap

Portable media-item model and explicit host owner ports. Hosts authorize current actors and own storage, credentials, network, clock, transactions, media and rendering. RSS appearances/read/bookmark state remain external HostRef provenance. GUI belongs to a separate crate.

Use `hsscrap::decode::<URLCapture|Item|MetadataObservation>` at untrusted JSON boundaries with positive finite host Bounds and supported feature names. Raw Serde deserialization is structural only, not command admission or cross-record owner-integrity proof. Owner ports accept CommandContext with operation identity, scope and revision fences. Adapters validate commands, verify references and authorize reads/replays; traits alone do not prove an adapter.

Revisions and duration u64 values use decimal strings. Snapshot nulls stay null. Patch Keep/Set/Clear differs from Override Inherit/Set/Clear. Unknown optional namespaced extensions survive; unsupported required features reject. Legacy unknown original URLs/dates remain null.

Build from repository root: `cargo build --manifest-path assets/hsscrap/Cargo.toml`. Focused proof: `cargo test --manifest-path assets/hsscrap/Cargo.toml -p hsscrap --test model_contract`. Follow preparation rules first. No services, credentials, database or runtime startup needed. No I/O executes in core. Errors report invalid fields, finite limits, denial, conflicts and unknown outcomes. Recover through the host operation lookup; never infer success from a lost response.

This package currently implements the model foundation. URL identity migration/normalization, observation/gallery transitions, transfer/JCS proof and host/GUI adoption remain separate incomplete microtasks. A serialized media reference does not prove retained bytes.

</topic>

<topic id="research-basis">

Sources inspected 2026-10-23 before contract selection: [Serde enum representations](https://serde.rs/enum-representations.html), [Cargo exact revision dependencies](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html), [WHATWG URL standard](https://url.spec.whatwg.org/) updated 2026-10-19, [Miniflux entry source](https://github.com/miniflux/v2/blob/main/internal/model/entry.go). Reuse Serde tagged data and UUID newtypes. The canonical WP requires richer capture/access/provenance semantics than feed entries. Existing handshake_asset_resolver owns retained media resolution/cache; handshake_embed_views owns rendering; neither owns capture metadata. A new independent owner preserves their existing purposes. Core dependencies have no GUI/provider/database code.

Reject whole-URL reserialization for identity normalization: selected WP preserves escapes/query order/fragments more narrowly. Pure URL parsing validates syntax; MT002 must separately implement pinned normalization/IDNA and prove it. Risks include malformed legacy data, cross-item references, unsupported semantics and lost mutation responses. Boundary validation rejects malformed values; owner adapters must prove scoped references, revision-fenced commits and durable outcome lookup. Model tests prove only this foundation; host writes/live providers require independent proof.

</topic>
