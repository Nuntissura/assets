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

This package implements the model foundation, capture normalization, metadata, gallery, operation transitions and portable transfer plans. Host/GUI adoption remains separate incomplete work. A serialized media reference does not prove retained bytes.

`identity::normalize_url` preserves accepted input while producing a versioned identity key: normalized scheme/IDNA host/default port, empty path slash and percent-escape case; path case, query order/duplicates and fragments remain significant. Existing stored keys require explicit migration when the parser contract changes. `resolve_identity` returns conflicts across contradictory authorized provider/URL matches; hosts supply the current owner-scoped index.

`ItemOwner::lookup_url` accepts `UrlIdentityQuery` with the exact normalized URL, normalization version and access partition, and returns every authorized canonical candidate within the supplied bounds. Compare these candidates with `lookup_identity` through `identity::resolve_identity` before capture. Never turn an unsupported index, incompatible normalization version or ambiguous match into a successful empty result. `assert_source_identity` registers an immutable verified assertion through the item revision/operation boundary; `inspect_source_identity` rereads the canonical assertion. Active provider/namespace/provider-item identity is unique within its access partition. Owner adapters validate the same-item observation/provider provenance and preserve local tombstones. New methods default to an explicit unsupported-feature error until a host implements them.

`metadata::materialize_patch` distinguishes omitted fields from explicit clearing using typed patches and requires the exact same-stream/access baseline. `accept_observation` fences expected Item/stream revisions and reserved sequences, rejects reused current observation IDs and unavailable acquisition, preserves user overrides and advances only an already selected metadata stream. Hosts additionally enforce immutable IDs against all stored history. `project_metadata` requires authorized item/observation inputs; denied source metadata yields empty values and never a private fallback.

`gallery::reconcile_gallery` checks accepted sequence/revisions, complete-set digests and resolved member identities. Partial or unselected observations can stage new members without changing active order or metadata. Complete selected observations retain member IDs and tombstone removed members; conflicting stable producer keys reject even when a mapping row is omitted. `GallerySelection::Initial` atomically installs a first complete gallery into a URL-only item; `ChangeFrom` names the prior selection for an explicit source switch. Hosts authorize selections and persist plans. A removed user-selected cover conflicts instead of leaving a dangling active pointer.

`media_refs` checks bindings against a resolved member, media metadata and an owner materialization receipt, including bounded lineage/cycle checks. It validates occurrence data and bounded unambiguous context sets. These checks do not materialize bytes, provide grants or prove an external media service. Repeated members may reference the same measured media resource. Gallery hashing reuses `handshake_canonical_json::sha256_hex` over the restricted UUID-string array encoding; this does not establish general RFC8785 compatibility of that asset's separate canonical encoder.

`operations::plan_operation` binds operation ID, scope, actor, sorted owner revision fences and commands to a deterministic SHA-256 intent. Replay requires current authorization and exact intent; pending operations require canonical owner reconciliation. Hosts verify receipts against each owner before calling `finish_operation`. Cancellation preserves committed effects and leaves partial operations pending. Run `cargo test --manifest-path assets/hsscrap/Cargo.toml -p hsscrap --test reference_host` for the disposable file-backed reference adapter, including deliberate child-process loss, restart, response loss, revision conflicts, revocation, tombstones and bounded capacity. Its canonical file rereads prove only this reference adapter, not Portal/Desktop persistence.

The reference adapter also exercises URL lookup and source-identity assertion/readback through the public `ItemOwner` trait. It stores bounded canonical captures, observations and assertions, rejects collisions before changing records, and preflights receipt capacity before pending/effects. Initialization supplies immutable observations and a deterministic fixture clock; provider verification and actual observation ingestion remain host responsibilities. The reference adapter returns explicit unsupported errors for capture, observation ingestion, gallery and trait-based tombstone actions that it does not implement.

`tags` groups only freshly authorized assignments by canonical tag ID, preserves each origin and routes removal to writable targets. `organization` plans project membership saves, stable folder/root placement moves and explicit cross-project moves. Hosts supply authorized destinations and complete canonical membership/placement sets, then atomically commit plans with revision checks. Removing a placement does not remove the item; RemoveIfEmpty counts the full supplied canonical set. These pure plans do not establish a host persistence or authorization implementation.

`transfer::project_export` filters a canonical record snapshot using the host's current authorized typed references, member-read decisions and complete capture order. It binds the audience/policy to the projection, substitutes the first authorized capture, clears withheld pointers and downgrades filtered gallery snapshots to partial without their full-set digest. An actually denied private locator also clears linked private member metadata. Expiry alone does not imply permission denial. `plan_export` is the lower-level entrypoint for already projected records. Hosts atomically persist its cursor and projection; unchanged content reuses its export revision, changed projections advance it, and a new lineage/policy/audience requires a new stream.

Use `transfer::decode_projection` for untrusted bytes. It rejects duplicate keys, lost/extra/missing fields, unsupported features and violations of byte/depth/numeric limits, then verifies the complete fields hash. Only documented model sets sort by JCS bytes; member order and opaque extension arrays remain ordered. Transfer hashing consumes the separate strict JCS API; the legacy canonical encoder remains unchanged. Revisions/durations and exact integers beyond the safe binary64 range use decimal strings.

After the host verifies its negotiated trust/audience/access envelope, `plan_import` verifies the source hash before rewriting every typed identity/reference through scoped destination mappings. Unresolved references quarantine; equal-revision/different-hash, older versions, unreconciled streams and local changes conflict. Destination media bindings require the media owner's own materialization receipt. Complete gallery digests are recomputed from mapped member IDs while retaining source hash/digest provenance. Source deletion/revocation changes provenance without deleting local work. ImportMapping and OperationOutcome remain destination control records; imported operation IDs are provenance, never executable effects. Hosts preserve source evidence and atomically recheck live authority, local revisions and mapping intent at promotion. No signing, grants, storage or network runs in these plans.

Focused transfer proof: `cargo test --locked --manifest-path assets/hsscrap/Cargo.toml -p hsscrap transfer::tests`. Required owning-package proof: `cargo test --locked --manifest-path assets/hsscrap/Cargo.toml -p hsscrap`. No services, credentials or GUI are required. Inspect quarantine/conflict outcomes, resolve missing mappings through canonical owners and retry the same stream/version; never turn unavailable foreign references into ready local media or reset durable cursors.

</topic>

<topic id="standalone-consumer" status="locally-passed">

`examples/standalone.rs` is a portable synthetic consumer of the public core, native inspector and versioned bridge. It normalizes a capture URL, edits and cancels a draft, calls the actual inspector, preserves a large revision through the bridge, lays out its panel and revokes its view. The host remains responsible for authorization and owner effects.

After the coordinated preparation gate, run the independent consumer against the exact published source selection:

```powershell
python assets/hsscrap/tests/consumer/prove.py --revision dc8e88168e22ef1b669018406e48d36be36bc581
```

The driver creates a separate temporary workspace beneath the existing absolute `CARGO_TARGET_DIR`, resolves the three hsscrap crates from that immutable Git revision, checks Cargo's resolved source and feature graph, then builds and runs the public calls. Its own example is copied into the consumer; neither product nor the asset's private test modules are imported. `--output` relocates the generated consumer and logs. `HSSCRAP_CARGO`, `HSSCRAP_RUSTC` and `HSSCRAP_GIT` select existing tools while preserving Rustup proxy names. Toolchain selection remains explicit in the coordinated environment.

The JSON result names the exact Git revision, selected crates/features, native target/compiler, dependency closure and actual local artifact digests. It also archives the immutable hsscrap/JCS source selection and records its file list, rejecting generated/cache/key-file paths. Review the actual source contents independently. This is a Git source release; the generated consumer executable, archive and previous WASM/bindings are local build outputs. Browser rendering, host persistence, signing/trust and production provider access retain their independent acceptance.

Failure leaves the generated workspace and full command logs in place. A wrong revision/source, unexpected local dependency, GUI dependency in the core, changed public result or excluded package path rejects the run. Inspect that diagnostic and preserve the exact source pin when retrying. The 2026-10-23 first consumer run passed all six public workflows on Rust 1.97.1 for x86_64-pc-windows-msvc. The core resolved closure contained 57 packages and no GUI/RSS dependency. An independent reread verified all five generated artifact digests and all 53 archived files against the immutable source blobs. Canonical publication of this example/manual/driver remains separate from that source-selection proof.

Research inspected 2026-10-23: [Cargo Git dependencies and exact revisions](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#specifying-dependencies-from-git-repositories), [resolved dependency metadata](https://doc.rust-lang.org/cargo/commands/cargo-metadata.html) and [public examples](https://doc.rust-lang.org/cargo/reference/cargo-targets.html#examples). Reuse Cargo's actual source resolver and compiler artifacts. A host build or workspace path dependency would not prove independent immutable consumption. Existing native/GUI/WASM behavior proofs remain valid and are reused; the new driver proves the separate consumer boundary and source-package selection.

</topic>

<topic id="research-basis">

Sources inspected 2026-10-23 before contract selection: [Serde enum representations](https://serde.rs/enum-representations.html), [Cargo exact revision dependencies](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html), [WHATWG URL standard](https://url.spec.whatwg.org/) updated 2026-10-19, [Miniflux entry source](https://github.com/miniflux/v2/blob/main/internal/model/entry.go). Reuse Serde tagged data and UUID newtypes. The canonical WP requires richer capture/access/provenance semantics than feed entries. Existing handshake_asset_resolver owns retained media resolution/cache; handshake_embed_views owns rendering; neither owns capture metadata. A new independent owner preserves their existing purposes. Core dependencies have no GUI/provider/database code.

Reject whole-URL reserialization for identity normalization: selected WP preserves escapes/query order/fragments more narrowly. Pure URL parsing validates syntax; MT002 must separately implement pinned normalization/IDNA and prove it. Risks include malformed legacy data, cross-item references, unsupported semantics and lost mutation responses. Boundary validation rejects malformed values; owner adapters must prove scoped references, revision-fenced commits and durable outcome lookup. Model tests prove only this foundation; host writes/live providers require independent proof.

</topic>
