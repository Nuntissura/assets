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

This package implements the model foundation, capture normalization, metadata, gallery and operation transitions. Transfer/JCS proof and host/GUI adoption remain separate incomplete microtasks. A serialized media reference does not prove retained bytes.

`identity::normalize_url` preserves accepted input while producing a versioned identity key: normalized scheme/IDNA host/default port, empty path slash and percent-escape case; path case, query order/duplicates and fragments remain significant. Existing stored keys require explicit migration when the parser contract changes. `resolve_identity` returns conflicts across contradictory authorized provider/URL matches; hosts supply the current owner-scoped index.

`metadata::materialize_patch` distinguishes omitted fields from explicit clearing using typed patches and requires the exact same-stream/access baseline. `accept_observation` fences expected Item/stream revisions and reserved sequences, rejects reused current observation IDs and unavailable acquisition, preserves user overrides and advances only an already selected metadata stream. Hosts additionally enforce immutable IDs against all stored history. `project_metadata` requires authorized item/observation inputs; denied source metadata yields empty values and never a private fallback.

`gallery::reconcile_gallery` checks accepted sequence/revisions, complete-set digests and resolved member identities. Partial or unselected observations can stage new members without changing active order or metadata. Complete selected observations retain member IDs and tombstone removed members; conflicting stable producer keys reject even when a mapping row is omitted. `GallerySelection::Initial` atomically installs a first complete gallery into a URL-only item; `ChangeFrom` names the prior selection for an explicit source switch. Hosts authorize selections and persist plans. A removed user-selected cover conflicts instead of leaving a dangling active pointer.

`media_refs` checks bindings against a resolved member, media metadata and an owner materialization receipt, including bounded lineage/cycle checks. It validates occurrence data and bounded unambiguous context sets. These checks do not materialize bytes, provide grants or prove an external media service. Repeated members may reference the same measured media resource. Gallery hashing reuses `handshake_canonical_json::sha256_hex` over the restricted UUID-string array encoding; this does not establish general RFC8785 compatibility of that asset's separate canonical encoder.

`operations::plan_operation` binds operation ID, scope, actor, sorted owner revision fences and commands to a deterministic SHA-256 intent. Replay requires current authorization and exact intent; pending operations require canonical owner reconciliation. Hosts verify receipts against each owner before calling `finish_operation`. Cancellation preserves committed effects and leaves partial operations pending. Run `cargo test --manifest-path assets/hsscrap/Cargo.toml -p hsscrap --test reference_host` for the disposable file-backed reference adapter, including deliberate child-process loss, restart, response loss, revision conflicts, revocation, tombstones and bounded capacity. Its canonical file rereads prove only this reference adapter, not Portal/Desktop persistence.

`tags` groups only freshly authorized assignments by canonical tag ID, preserves each origin and routes removal to writable targets. `organization` plans project membership saves, stable folder/root placement moves and explicit cross-project moves. Hosts supply authorized destinations and complete canonical membership/placement sets, then atomically commit plans with revision checks. Removing a placement does not remove the item; RemoveIfEmpty counts the full supplied canonical set. These pure plans do not establish a host persistence or authorization implementation.

</topic>

<topic id="research-basis">

Sources inspected 2026-10-23 before contract selection: [Serde enum representations](https://serde.rs/enum-representations.html), [Cargo exact revision dependencies](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html), [WHATWG URL standard](https://url.spec.whatwg.org/) updated 2026-10-19, [Miniflux entry source](https://github.com/miniflux/v2/blob/main/internal/model/entry.go). Reuse Serde tagged data and UUID newtypes. The canonical WP requires richer capture/access/provenance semantics than feed entries. Existing handshake_asset_resolver owns retained media resolution/cache; handshake_embed_views owns rendering; neither owns capture metadata. A new independent owner preserves their existing purposes. Core dependencies have no GUI/provider/database code.

Reject whole-URL reserialization for identity normalization: selected WP preserves escapes/query order/fragments more narrowly. Pure URL parsing validates syntax; MT002 must separately implement pinned normalization/IDNA and prove it. Risks include malformed legacy data, cross-item references, unsupported semantics and lost mutation responses. Boundary validation rejects malformed values; owner adapters must prove scoped references, revision-fenced commits and durable outcome lookup. Model tests prove only this foundation; host writes/live providers require independent proof.

</topic>
