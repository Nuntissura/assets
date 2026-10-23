---
file_id: "FERRIC-FORAGER-COMPATIBILITY-PRODUCT-REVIEW-0.2.0"
file_kind: "adversarial-peer-review"
updated_at: "2026-07-18T00:32:57+02:00"
review_risk_tier: "HIGH"
source_design: "../../spec/ferric_forager_technical_design_v0.2.0.md"
review_lens: "yt-dlp-compatibility-extractor-ecosystem-product-cli-migration-release-maintainability"
verdict: "FAIL_IMPLEMENTATION_READINESS"
---

<topic id="verdict" status="final" version="0.2.0" summary="Compatibility and product verdict">

# Verdict

**FAIL implementation readiness. PASS as a Phase 0 research and contract-refinement charter.**

The direction is sound and the operator constraints are preserved: the production implementation is Rust, Python is not a production dependency or fallback, FFmpeg is the only external production dependency explicitly authorized by the operator, speed is the first optimization priority, yt-dlp is the behavioral foundation, and the collection/live/manifest/output-sink expansion remains mandatory. The document’s external JavaScript-runtime and native browser-impersonation assumptions are unresolved compatibility conflicts, not approved architecture.

The design cannot yet govern implementation because its external contract is not enumerable, its replacement threshold is undefined, its source model does not encode several baseline and expansion semantics, and its update architecture arrives after the extractor corpus that requires it. The correct response is not to shrink the product. The required response is to turn parity, migration, source-graph, extractor-update, and expanded-product behavior into executable contracts before broad implementation begins.

</topic>

<topic id="independent-findings" status="final" version="0.2.0" summary="Severity-ranked independent findings">

# INDEPENDENT_FINDINGS

| ID | Severity | Finding | Release effect |
|---|---|---|---|
| FCP-000 | CRITICAL | Full YouTube compatibility currently needs JavaScript execution, and available high-fidelity Rust browser-impersonation clients use BoringSSL; neither an extra JS runtime nor another native library is authorized by “built fully in Rust with FFmpeg as dependency.” | Requires evidence and an explicit operator decision before dependency selection or full-compatibility claims. |
| FCP-001 | CRITICAL | “Majority of commonly used options,” a “defined compatibility corpus,” and a reviewer-selected threshold do not form an implementable compatibility contract. | Blocks implementation-readiness and every replacement claim. |
| FCP-002 | CRITICAL | Static built-ins plus a Phase 9 update mechanism cannot sustain yt-dlp-class extractor breakage or independently update challenge assets. | Blocks maintainable extractor parity and safe hotfixes. |
| FCP-003 | CRITICAL | The illustrative `SourceResult` is an enum, not the promised source graph, and it omits observable overlay, composite, metadata-record, and per-asset identity semantics. | Blocks faithful extraction, archive correctness, and stable public APIs. |
| FCP-004 | HIGH | CLI migration is described by categories, not a generated option/config/template/format/artifact contract. | Permits silent option acceptance, wrong defaults, and breaking migration. |
| FCP-005 | HIGH | The sequence postpones the riskiest yt-dlp foundation work—YouTube/EJS—and usable FFmpeg integration until after major architecture is built. | Creates a high-probability architectural rework path. |
| FCP-006 | HIGH | Collection filtering, ordered results, live reconnect, multi-sink fan-out, and local-HTTP/player behavior lack deterministic missing-data and backpressure contracts. | Blocks the documented product expansion. |
| FCP-007 | HIGH | The speed gates can be passed without equivalent work and can shift cost into JavaScript or FFmpeg while reporting a faster Rust process. | Makes the primary product claim non-falsifiable. |
| FCP-008 | HIGH | Archive identity, item-versus-asset granularity, write timing, and yt-dlp text migration are unresolved. | Can create permanent false skips or duplicate acquisition. |
| FCP-009 | HIGH | The canonical plugin boundary is undecided even though extractor updateability and no-Python migration depend on it. | Blocks third-party and first-party hotfix protocol design. |
| FCP-010 | HIGH | HLS/DASH/MSS, custom ranges, live recording, and real-time mux are all production requirements but do not share one end-to-end acceptance surface. | Allows a “protocol complete” claim with missing expanded behavior. |
| FCP-011 | MEDIUM | The proposed workspace commits to roughly thirty crates before API stability, compile-time, or ownership evidence exists. | Increases early churn and makes semantic changes expensive. |

</topic>

<topic id="finding-fcp-000" status="open" version="0.2.0" summary="Unresolved JavaScript and browser-impersonation dependency conflict">

## FCP-000 — Full compatibility conflicts with the currently authorized dependency boundary

The operator authorized a fully Rust implementation with FFmpeg as the dependency. That does not silently authorize Deno, Node, Bun, QuickJS, BoringSSL, curl-impersonate, or another production runtime/native transport library.

Current primary evidence establishes a real compatibility problem. yt-dlp states that full YouTube support requires maintained EJS challenge scripts and a supported JavaScript runtime; its supported list is Deno, Node, QuickJS/QuickJS-NG, and Bun, not a Rust engine ([EJS guide](https://github.com/yt-dlp/yt-dlp/wiki/EJS), [EJS source/runtime table](https://github.com/yt-dlp/ejs)). Boa is an embeddable JavaScript engine written in Rust, but its own documentation calls it experimental and reports greater than 90% rather than complete Test262 conformance; no opened yt-dlp/EJS source identifies Boa as supported ([Boa project](https://github.com/boa-dev/boa), [Boa current documentation](https://boajs.dev/), [Boa Test262 process](https://boajs.dev/docs/contributing/testing)). A Boa-based solution is therefore a candidate to test, not evidence that the conflict is solved.

Browser impersonation has the same boundary problem. yt-dlp documents that some sites require browser-like TLS/HTTP behavior and uses curl_cffi/curl-impersonate. The current Rust wreq client exposes high-fidelity TLS/HTTP2 profiles but explicitly builds on BoringSSL and additional native build tooling ([yt-dlp impersonation dependency](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/README.md#impersonation), [wreq source and build requirements](https://github.com/0x676e67/wreq)). Ordinary Reqwest does not document browser-fingerprint parity ([Reqwest documentation](https://docs.rs/reqwest/latest/reqwest/)). No reviewed evidence proves a pure-Rust transport can meet the mandatory impersonation corpus.

**Required Phase 0 evidence before asking the operator to decide:**

1. Prototype current EJS challenge bundles on embedded Boa with no external runtime and run upstream EJS vectors plus controlled YouTube canaries. Measure correctness, cold/warm latency, CPU, memory, cancellation, and sandbox limits.
2. Prototype a Rust-native challenge solver only if it can consume the changing player program without recreating the brittle regex approach upstream abandoned. Run the same differential corpus.
3. Build a transport fingerprint corpus from mandatory extractors and compare ordinary pure-Rust transport, any pure-Rust configurable alternative, and a BoringSSL-backed candidate. Record exact native dependencies and packaging consequences.
4. Present the operator with evidence-backed choices: approve an embedded pure-Rust JS engine and JS challenge assets; approve a named external JS runtime as an explicit exception; fund a Rust-native solver; approve a named native impersonation library; or change the mandatory extractor set. This review does **not** select or authorize any exception.

Omitting YouTube challenge handling or mandatory impersonation is not an allowed shortcut because it would narrow the documented functionality. Implementation of unrelated pure-Rust Phase 0 contracts can proceed, but dependency-bearing JavaScript and impersonation architecture is blocked pending evidence and operator resolution.

</topic>

<topic id="finding-fcp-001" status="open" version="0.2.0" summary="Compatibility contract is not enumerable">

## FCP-001 — The parity model is not an executable specification

The design correctly pins yt-dlp `2026.07.04`, but §20.2 promises only the “majority” of commonly used options, §26.2 compares a short list of normalized fields, §31 refers to a corpus that does not exist, and §32 leaves the replacement threshold to reviewers. Those clauses do not tell an implementer what must match or a validator what constitutes failure.

Direct inspection of the pinned source found **295 `add_option` definition calls, 495 long-option literal occurrences, and 1,183 imported `*IE` extractor classes** in [`options.py`](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/yt_dlp/options.py) and [`_extractors.py`](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/yt_dlp/extractor/_extractors.py). These are scale indicators, not parity metrics: aliases, negative forms, compatibility options, shared extractors, and URL classes make raw counts misleading. The pinned README also defines conditional defaults—for example format selection changes when FFmpeg is unavailable or stdout is used—and layered configuration behavior that a field-only extractor comparison cannot observe ([pinned README: format selection](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/README.md#format-selection), [configuration](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/README.md#configuration)).

**Required remediation:** adopt a dual authority model.

1. A Ferric semantic specification is normative for native features, documented intentional improvements, collections, live sessions, output sinks, and security removals.
2. A generated `yt-dlp-2026.07.04` compatibility profile is normative for migration behavior not intentionally overridden.
3. Generate an option manifest from the pinned parser containing every spelling, alias, negation, type, multiplicity, default, precedence rule, environment/config interaction, dependency condition, side effect, output channel, and exit behavior.
4. Replace the one-list differential schema with a four-plane contract: invocation/config; normalized source graph; HTTP request/response transcript after secret normalization; and filesystem/process artifacts including archive writes, sidecars, filenames, temporary-file policy, post-processing, exit code, stdout, and stderr class.
5. Every comparison case must name its authority, accepted normalizations, and divergence approval ID.

**Validation:** generate the manifest reproducibly from the pinned tree; prove every accepted CLI spelling maps to one matrix row; run positive, negative, repeated-option, config-precedence, dependency-missing, simulation, and post-processing cases; reject unknown or partial options rather than silently accepting them.

</topic>

<topic id="finding-fcp-002" status="open" version="0.2.0" summary="Extractor update architecture is too late">

## FCP-002 — Extractor maintenance and release cadence are architecturally mismatched

The design compiles built-ins statically (§21.1), migrates the corpus in Phase 8, and does not define update mechanics until Phase 9. The pinned yt-dlp release documentation explains why its stable channel can become stale as sites change and provides nightly and per-push channels for fixes ([pinned update documentation](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/README.md#update)). The current EJS guide separately requires challenge scripts to remain version-compatible and supports updating those assets independently ([EJS setup and versioning](https://github.com/yt-dlp/yt-dlp/wiki/EJS)). A single monolithic binary cadence therefore leaves two high-change surfaces—extractors and challenge assets—without an early update contract.

**Failure scenario:** a mandatory YouTube or high-usage extractor breaks after release. The only recovery is a full cross-platform core rebuild because no signed extractor/challenge update channel or rollback protocol was designed before the corpus was ported.

**Required remediation:** define the update contract in Phase 0 and implement its minimum host support in Phase 1.

- Version `core`, `compatibility-profile`, `extractor-pack`, `plugin-protocol`, and `challenge-bundle` independently.
- Keep high-traffic verified built-ins in-process for the fast path.
- Permit signed first-party Rust extractor hotfix packs to override a built-in only when the pack declares a compatible core/protocol range and the user’s channel permits it.
- Publish stable, candidate, and hotfix channels with signed metadata, hashes, minimum/maximum core versions, rollback, quarantine, and last-known-good selection.
- Cache updates atomically; a failed pack must never replace the last-known-good pack.
- Make extractor health and pack provenance visible in `--compat-report` and structured diagnostics.

**Validation:** simulate a broken built-in, incompatible pack, interrupted update, bad signature, failed health check, and rollback. Prove the unaffected built-in fast path has no plugin-process startup cost.

</topic>

<topic id="finding-fcp-003" status="open" version="0.2.0" summary="Source graph semantics are incomplete">

## FCP-003 — `SourceResult` does not yet implement the promised source graph

The design repeatedly makes the source graph the primary resolved object, but Appendix A presents a single enum and §17 says collection entries can be metadata-only without providing a corresponding top-level or node type. The pinned yt-dlp `process_ie_result` path distinguishes video, URL, transparent URL, playlist, multi-video, and compatibility-list results; transparent results merge outer metadata into inner results, and a resolved video can request additional URLs ([`YoutubeDL.process_ie_result`](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/yt_dlp/YoutubeDL.py)). Those behaviors are observable through metadata, filenames, archive IDs, recursion, and error propagation.

The gap is larger for Ferric’s expansion: a post may contain several independently archived assets, a collection entry may be metadata-only, a live source may expose alternate and complementary tracks, and a source may be both embedded and nested. A `MediaItem.assets: Vec<_>` does not define identity, overlay precedence, or which graph edge owns archive and output-template context.

**Required remediation:** make a versioned normalized graph the canonical internal and JSON contract.

- Stable `NodeId` plus `NodeKind`: media, collection, live, redirect, metadata record, and unsupported/protected marker.
- Typed edges: contains, embeds, transparently-overlays, alternate, complementary, additional, continuation, and derived-output.
- A normative overlay table defining which outer fields replace, inherit, append, or never cross a transparent edge.
- Item, representation, track, asset, and derived-output identities as separate types.
- Explicit lazy-node and pagination continuation semantics; a stream cursor is state, not source identity.
- Cycle detection and recursion budgets represented in the graph-resolution contract.

Keep `SourceResult` as an ergonomic internal dispatch enum if useful, but expose separate public entrypoints or typed visitors for `resolve_media`, `stream_collection`, and `open_live` so callers do not downcast an indefinitely expanding enum.

**Validation:** differential fixtures for nested playlists, transparent redirect chains, `multi_video`, metadata-only entries, additional URLs, the same asset under two parents, an expiring URL for one stable source, and a redirect cycle.

</topic>

<topic id="finding-fcp-004" status="open" version="0.2.0" summary="CLI migration surface is incomplete">

## FCP-004 — CLI/API compatibility is categorized but not contracted

The proposed native subcommands are a good product surface, but they are not syntactically compatible with yt-dlp’s `yt-dlp [OPTIONS] URL...` invocation. The design acknowledges a possible alias without defining whether it is strict, which defaults it activates, or how unsupported options fail. The upstream tests independently cover format selection, output-template filename preparation, playlist selection, configuration precedence, plugin loading, update parsing, and URL matcher collisions ([`test_YoutubeDL.py`](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/test/test_YoutubeDL.py), [`test_config.py`](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/test/test_config.py), [`test_all_urls.py`](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/test/test_all_urls.py)). Porting only the README categories will miss tested interactions.

**Required remediation:** separate two explicit profiles.

- `fforager` native profile: subcommands and new typed behavior.
- `yt-dlp` compatibility alias or `fforager --profile yt-dlp-2026.07.04`: legacy invocation grammar and defaults.

The parser must compile from the generated option manifest, attach provenance to every effective value, and emit a machine-readable rejection for unimplemented or context-invalid options. The template and format-selector languages need frozen grammars, conformance vectors, limits, and exact missing-field/error policies. Native JSON, compatibility JSON, and event schemas require independent versions.

**Validation:** replay upstream parser/config/template/format test vectors where licenses and fixture constraints permit; add golden exit-code/stdout/stderr/artifact tests. A compatibility option must never be reported “implemented” merely because parsing succeeds.

</topic>

<topic id="finding-fcp-005" status="open" version="0.2.0" summary="Implementation sequence defers the highest-risk vertical slice">

## FCP-005 — The sequence validates the foundation too late

YouTube challenge execution is deferred to Phase 7 and the FFmpeg supervisor to Phase 5, although the operator’s foundation is yt-dlp, full YouTube support now depends on an external JavaScript runtime and maintained EJS assets, and FFmpeg is a required dependency. Upstream abandoned its prior regex/native-interpreter path because player changes made that maintenance model too brittle; the supported direction delegates to a real runtime and AST-based EJS assets ([yt-dlp announcement #14404](https://github.com/yt-dlp/yt-dlp/issues/14404), [EJS repository](https://github.com/yt-dlp/ejs)). FFmpeg’s official interface also makes stream-copy and machine-readable progress concrete integration surfaces, not late packaging details ([FFmpeg streamcopy and `-progress`](https://ffmpeg.org/ffmpeg.html)).

**Required remediation:** add a thin risk-retirement vertical slice before broad architecture expansion:

`CLI/API -> YouTube fixture/live canary -> operator-authorized challenge backend -> normalized graph -> format selector -> HLS/DASH/direct acquisition -> FFmpeg stream-copy merge -> atomic file -> archive/event result`.

This slice does not replace the later complete YouTube migration or full post-processing work. It proves that the chosen contracts can support the hardest baseline path. Its challenge backend cannot be chosen until FCP-000’s evidence is presented and the operator resolves the dependency boundary. Bring FFmpeg discovery, capability probing, typed argv construction, progress parsing, and cancellation into Phase 1/2; retain advanced post-processing in Phase 5. Define the plugin/update protocol in Phase 0/1 even if third-party packaging remains Phase 9.

**Validation:** the vertical slice must pass deterministic replay and an opt-in live canary, show Rust challenge-subsystem and FFmpeg CPU separately where measurable, produce an equivalent selected output, and survive challenge-backend and FFmpeg failure without corrupt final output.

</topic>

<topic id="finding-fcp-006" status="open" version="0.2.0" summary="Expanded functionality lacks deterministic contracts">

## FCP-006 — Collection, live, and sink behavior is still aspirational

Gallery-dl exposes distinct file, post, and child ranges/filters and configurable archive-write events rather than one undifferentiated collection filter ([gallery-dl options](https://github.com/mikf/gallery-dl/blob/master/docs/options.md), [configuration](https://github.com/mikf/gallery-dl/blob/master/docs/configuration.rst)). Streamlink documents ring-buffer behavior, segment retries, live-edge/reload controls, stdin/FIFO/HTTP/continuous-HTTP transports, and the possibility that a paused player stops consuming until the ring buffer fills ([Streamlink CLI](https://streamlink.github.io/cli.html), [Session options](https://streamlink.github.io/api/session.html)). N_m3u8DL-RE exposes MSS, per-track selectors, segment-count validation, custom segment/time ranges, live record limits, real-time merge, and live pipe mux as distinct behaviors ([N_m3u8DL-RE README](https://github.com/nilaoda/N_m3u8DL-RE/blob/main/README.en.md)).

The design names these capabilities but leaves decisive semantics open: whether an unknown filter field passes or fails; whether event order and result order are the same channel; whether a slow player can stall an archival recording; how an HTTP client reconnect resumes a live byte stream; what a named-pipe disappearance means; and whether a failed derived sidecar changes the source asset’s archive state.

**Required remediation:** define executable contracts for:

- three-valued filter evaluation (`true`, `false`, `unknown`) and explicit `on-unknown` policy;
- field-availability descriptors so only listing-available metadata is filtered before asset resolution;
- separate post, child, item, track, and asset ranges;
- real-time operational events always emitted in completion order, with deterministic logical-result projection as the CLI/default data result;
- per-sink bounded queues, lag policy, disconnect policy, replay/header behavior, finalization, and loss accounting;
- live continuity keys, discontinuity handling, reconnect budgets, duplicate/gap reporting, and client reconnect semantics;
- a sink capability matrix for seekability, resumability, atomicity, multi-client support, and FFmpeg requirements.

Use an in-process Rust fan-out router for trusted built-in sinks by default, with a dedicated worker only for untrusted adapters or when failure isolation is explicitly selected. This choice must be benchmarked; it is not a claim that in-process is automatically faster in every workload.

**Validation:** negative and boundary tests must include an earlier slow logical item, an unknown width filter, a player that stops reading, a player reconnecting to continuous HTTP, a disappearing named pipe, a slow host callback, a 24-hour rotating live window, and simultaneous lossless recording plus lossy playback.

</topic>

<topic id="finding-fcp-007" status="open" version="0.2.0" summary="Performance gates can be gamed">

## FCP-007 — Speed is not yet measured against equivalent total work

The design correctly separates Rust, JavaScript, and FFmpeg CPU, but §27 gates mostly the Rust process and does not require equivalent selected formats, requests, artifacts, or post-processing before performance comparison. A faster Rust process can therefore offload more work to a challenge backend or FFmpeg, select a cheaper format, omit a sidecar, or perform fewer compatibility checks and still appear faster. Upstream also warns that forcing browser impersonation on all requests can reduce speed and stability, which supports capability-scoped rather than global impersonation if the operator authorizes an implementation ([yt-dlp network options](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/README.md#network-options)). Ordinary Reqwest documents pooling, proxies, cookies, redirect policy, and HTTP/2 but not browser-fingerprint parity. Rust fingerprinting clients such as wreq exist, but wreq uses BoringSSL and cannot be assumed authorized or equivalent ([Reqwest documentation](https://docs.rs/reqwest/latest/reqwest/), [wreq source/build documentation](https://github.com/0x676e67/wreq)).

**Required remediation:** every speed comparison must first pass an equivalence precondition covering graph, selected streams, request policy, bytes, final media probe, sidecars, archive effects, and error policy. Report both per-process and total-system CPU/wall/I/O. Use local/replayed workloads for gates, repeated samples with recorded distributions, pinned tool versions, warm/cold separation, and identical OS cache state policy. Replace proxy metrics such as “fewer regex evaluations” with CPU and latency gates while retaining the proxy metric diagnostically.

Browser impersonation is a release blocker only for a mandatory-corpus extractor that declares it. A normal pure-Rust transport remains the default. Any impersonating transport requires the FCP-000 evidence and explicit operator authorization before it can be selected, then receives its own correctness and performance corpus.

No required release gate may remain “provisional.” For deterministic local direct download, tighten the current 95% throughput floor to a no-material-regression criterion selected from Phase 0 variance, while retaining CPU/GiB as the optimization metric. Do not manufacture a percentage before the baseline variance is measured.

**Validation:** deliberately substitute a lower-quality format, omit FFmpeg merge, route through global impersonation, and shift parsing into the JS worker; the benchmark harness must reject all four as non-equivalent rather than report wins.

</topic>

<topic id="finding-fcp-008" status="open" version="0.2.0" summary="Archive identity and migration are unresolved">

## FCP-008 — Archive correctness needs an identity and commit contract

The pinned yt-dlp archive key is primarily extractor key plus media ID, with support for old archive IDs; its write point is tied to the download-processing path ([`_make_archive_id`, `in_download_archive`, and `record_download_archive`](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/yt_dlp/YoutubeDL.py)). Gallery-dl’s archive supports configurable ID formats, write events, immediate-versus-job-complete modes, SQLite, and PostgreSQL ([gallery-dl archive configuration](https://github.com/mikf/gallery-dl/blob/master/docs/configuration.rst)). Ferric’s expanded asset graph cannot safely use one unspecified `SourceIdentity` for all of these meanings.

**Required remediation:** the archive schema must distinguish:

`identity_schema_version + extractor_key + namespace + source_id + item_variant + asset_role + asset_variant + account/session partition where identity is authorization-dependent`.

Define aliases for extractor renames and imported yt-dlp IDs. Record source-item completion, source-asset completion, sidecar completion, and derived-output completion separately. Each compatibility profile must state whether post-processing failure prevents the corresponding commit. Import must be a dry-run-able transaction with collision, ambiguity, and unmapped reports; export must preserve round-trip-able legacy IDs where mapping exists.

SQLite cannot be approved as the default under the current authority because it is another native library dependency. Phase 0 must benchmark a pure-Rust embedded candidate such as redb against SQLite on the exact 1,000/1,000,000-identity, write-batch, crash-recovery, import/export, and cross-platform workloads. The [`redb` project](https://github.com/cberner/redb) describes a pure-Rust ACID embedded store with a stable file format; its upstream benchmark is evidence to justify evaluation, not Ferric performance proof. The [`rusqlite` project](https://github.com/rusqlite/rusqlite) documents bundled SQLite and its blocking connection model. If SQLite materially wins or is required for interoperability, present that evidence and request explicit operator authorization; otherwise use the proven pure-Rust store behind the archive trait.

**Validation:** same item with two selected assets; one failed sidecar; extractor rename; legacy `_old_archive_ids`; two authenticated accounts sharing a source ID; transaction interruption; duplicate import; and one complete plus one incomplete variant.

</topic>

<topic id="finding-fcp-009" status="open" version="0.2.0" summary="Canonical plugin boundary decision">

## FCP-009 — Choose process IPC as the canonical v1 plugin boundary

The no-Python production requirement is compatible with plugin migration only by porting production plugins to Rust and isolating them behind a protocol. Native Rust dynamic-library ABI is correctly rejected by the design. WebAssembly is a possible future boundary, but WASI 0.3’s native async, streams, and futures were released in June 2026 and represent a newly changed component interface surface ([Component Model FAQ](https://component-model.bytecodealliance.org/reference/faq.html), [WIT streams and futures](https://component-model.bytecodealliance.org/design/wit.html)). Wasmtime provides configurable resource controls and interruption, but it is an additional runtime/library not authorized by the current operator boundary ([Wasmtime](https://github.com/bytecodealliance/wasmtime), [interruption configuration](https://docs.rs/wasmtime/latest/wasmtime/struct.Config.html)).

**Decision:** canonical v1 is a versioned process IPC protocol between the Rust host and Rust plugin executables. WebAssembly components may be considered only after explicit operator authorization and after the same extractor conformance suite proves async streaming, cancellation, capabilities, and performance on all supported platforms.

The IPC contract must include handshake/version negotiation, request IDs, bounded frame/message sizes, host-issued capability handles, origin-scoped network requests, cookie-domain scoping, challenge requests, cancellation, deadlines, credit-based result streaming, structured errors, health checks, crash attribution, and deterministic shutdown. Plugins do not receive raw cookie stores, arbitrary sockets, arbitrary files, or process spawn. A first-party Rust hotfix pack uses the same contract and can therefore be updated independently without weakening the no-Python rule.

**Validation:** protocol downgrade/upgrade, unknown field, oversized frame, duplicate request ID, mid-stream crash, withheld credits, cancellation race, capability denial, stale pack, and host restart.

</topic>

<topic id="finding-fcp-010" status="open" version="0.2.0" summary="Protocol engine release closure">

## FCP-010 — All documented protocol behaviors are mandatory for production

The design’s production acceptance criteria already require HLS, DASH, MSS, track selection, partial acquisition, segment validation, live reconnect, recording limits, and valid partial finalization. N_m3u8DL-RE’s reference interface confirms that custom ranges, segment validation, live real-time merge, and live pipe mux are separate paths rather than one generic “fragment download” behavior ([N_m3u8DL-RE README](https://github.com/nilaoda/N_m3u8DL-RE/blob/main/README.en.md)).

**Decision:** MSS, custom segment/time ranges, live recording, real-time merge, and FFmpeg pipe mux are all mandatory before the first production release. Internal milestones may stage them: Phase 3 can complete protocol parsing/acquisition and typed real-time-mux plans, while the FFmpeg-backed execution gate closes after the supervisor integration. The project must not call the protocol engine production-complete until the combined Phase 3/FFmpeg end-to-end corpus passes.

**Validation:** protocol-specific fixtures for discontinuities, byte ranges, time-boundary rounding, multi-period DASH, MSS timelines, subtitle timing, missing/duplicate segments, live window rotation, real-time mux failure, and playable partial finalization probed through FFmpeg/ffprobe.

</topic>

<topic id="finding-fcp-011" status="open" version="0.2.0" summary="Crate boundaries are premature">

## FCP-011 — Preserve boundaries without freezing thirty crates

Section 9 proposes approximately thirty crates before a single API or compile profile exists. The dependency rules are useful; the physical split is not yet proven. Early source-graph and compatibility changes will cross model, API, events, errors, collection, live, sink, persistence, and compatibility simultaneously.

**Required remediation:** treat the listed names as logical modules initially. Begin with a small number of physical boundaries that already have independent reasons to exist: public API/model, core engine, transports/protocol acquisition, built-in extractors, FFmpeg/worker supervision, plugin protocol/host, CLI, and testkit. Split further only when one of the following is demonstrated: independent versioning, feature/compile isolation, a sandbox/process boundary, ownership isolation, or measured incremental-build benefit.

**Validation:** record dependency graph, clean-build and incremental-build profiles, public API churn, and cycle checks before each split. No module boundary should be removed from the design; only premature physical crate commitments are deferred.

</topic>

<topic id="peer-review-decisions" status="proposed" version="0.2.0" summary="Concrete answers to design peer-review questions">

# Concrete peer-review decisions

| Q | Decision |
|---:|---|
| 1 | Use a dual specification: Ferric semantic spec plus a generated pinned yt-dlp compatibility profile. Neither alone is sufficient. |
| 2 | Typed core plus extension map is acceptable only with a canonical graph, namespaced/versioned extension registry, size/depth limits, provenance, and round-trip tests. |
| 3 | Canonical v1 plugin boundary: process IPC between Rust host and Rust plugin executables. WASM requires later evidence and explicit operator authorization. |
| 4 | The transport abstraction is directionally correct but not sufficient until request fidelity, cookie transfer, redirect behavior, impersonation target, connection-pool partitioning, and transcript testing are specified. |
| 5 | Impersonation blocks release only for mandatory-corpus extractors that declare it; never enable it globally by default. No implementation is authorized until FCP-000 evidence is presented and the operator resolves any native dependency. |
| 6 | Unresolved dependency conflict. First test embedded pure-Rust Boa and a Rust-native solver against current EJS/YouTube corpora. Do not select an external JavaScript worker without explicit operator authorization. |
| 7 | The listed JS restrictions are goals, not an enforceable sandbox contract; OS/runtime enforcement, update trust, IPC limits, and escape tests remain required. |
| 8 | The fragment design is not proven recoverable until journal/reorder/writer crash-point tests reconstruct exact committed ranges without trusting unverified bytes. |
| 9 | Journal durability is unresolved for Windows and network filesystems; support claims must be per-filesystem capability, with unsafe filesystems rejected or downgraded explicitly. |
| 10 | Keep mux and transcode permits separate and also account for shared disk/FFmpeg process limits. |
| 11 | Emit operational events in real-time completion order; default result/artifact projection is logical source order. Do not force both through one ordered queue. |
| 12 | The performance gates are not release-ready until Phase 0 measures variance and freezes equivalence preconditions and total-system metrics. |
| 13 | Intentionally omit `--netrc-cmd`, `--exec`/`--exec-before-download`, and arbitrary external `--downloader` execution from the production compatibility profile. Provide typed registered FFmpeg/player/tool adapters; reject these options with migration diagnostics. |
| 14 | The label “yt-dlp replacement” requires 100% pass of mandatory deterministic core compatibility cases, 100% Tier-A/YouTube cases, and every active pinned extractor URL class either passing or carrying an operator-approved intentional divergence. It also requires 14 consecutive daily Tier-A canary runs with no open severity-1 compatibility defect. A lower threshold may label a preview, not a replacement. |
| 15 | Discover and capability-probe FFmpeg/ffprobe because FFmpeg is authorized. No JS runtime is authorized for discovery, bundling, or production use until FCP-000 evidence is presented and the operator explicitly selects an exception or an all-Rust backend. |
| 16 | Yes, the physical crate count is premature. Preserve logical boundaries and start with the smaller physical set in FCP-011. |
| 17 | Formally model job, source resolution/redirect, fragment/reorder, live continuity/reconnect, sink/fan-out, archive commit, journal recovery, plugin worker, JS worker, and FFmpeg supervisor state machines. |
| 18 | Earliest fuzzing: M3U8/MPD/MSS, output-template and format-selector grammars, cookie/import parsers, journal reader, plugin/JS IPC, and FFmpeg progress parser. |
| 19 | Yes. Port plugins to Rust/process protocol; no Python legacy bridge is present in production/no-Python builds. |
| 20 | Use independently signed stable/candidate/hotfix extractor and challenge channels with compatibility ranges, atomic activation, quarantine, and rollback. |
| 21 | Keep `SourceResult` only as internal dispatch; canonicalize to a graph and provide separate typed public entrypoints for media, collection, and live callers. |
| 22 | Before asset resolution, evaluate index/range, stable identity/archive, source kind, date, creator, tags, and language only when the extractor declares the field available at listing time. Width/height/duration/size wait unless already present. Unknown is explicit, never silently false. |
| 23 | Under the current dependency authority, benchmark a pure-Rust embedded store such as redb as the default. SQLite becomes the default only after evidence and explicit operator authorization for the extra native library. Identity and commit rules remain as in FCP-008. |
| 24 | Built-in fan-out is in-process with bounded per-sink queues; untrusted adapters or explicitly isolated sinks use workers. |
| 25 | All listed N_m3u8DL-RE behaviors are mandatory for production. Internal milestone staging is allowed, but no production-complete claim precedes the combined end-to-end gate. |

</topic>

<topic id="diff-attack-surfaces" status="final" version="0.2.0" summary="Adversarial attack surfaces derived from the design">

# DIFF_ATTACK_SURFACES

1. **Invocation producer versus policy consumer:** aliases, negative flags, repeated options, config layers, presets, and dependency-conditioned defaults can parse successfully while producing different policy.
2. **Extractor producer versus graph normalizer:** transparent metadata overlays, redirects, composite results, metadata-only records, and additional URLs can be dropped or flattened incorrectly.
3. **Graph identity versus archive consumer:** an item ID can be mistaken for an asset/variant ID, creating false skips.
4. **Selector versus FFmpeg plan:** different default format selection can reduce work and falsely improve speed.
5. **Request policy versus transport backend:** native and impersonating clients can differ in redirect, cookie, header-order, proxy, TLS/HTTP fingerprint, and pool partitioning.
6. **Lazy collection producer versus filter/range consumer:** fields unavailable at listing time can be treated as false, or range application can occur at the wrong hierarchy level.
7. **Live producer versus fan-out consumers:** a slow player can stall a lossless recording or create unbounded buffering.
8. **Output sink writer versus reconnecting reader:** HTTP/FIFO/player reconnection can replay headers, omit bytes, or duplicate segments.
9. **Plugin/challenge pack producer versus host consumer:** protocol/version skew, stale cached assets, and partial update activation can break mandatory extractors.
10. **Pinned baseline versus changing sites:** a deterministic profile can remain internally green while live extractors are broken.
11. **Process-separated metrics versus product outcome:** costs can move from Rust to JS/FFmpeg and be omitted from the headline.
12. **Phase boundary versus end-to-end closure:** protocol acquisition can be marked complete before FFmpeg real-time mux and playable-finalization behavior exists.
13. **Operator authority versus dependency selection:** an implementer can treat the design’s JavaScript and impersonation examples as approved runtimes/libraries even though only Rust plus external FFmpeg is authorized.

</topic>

<topic id="independent-checks-run" status="final" version="0.2.0" summary="Independent source and artifact checks">

# INDEPENDENT_CHECKS_RUN

| Check | Independent action | Result |
|---|---|---|
| IC-001 | Parsed pinned [`yt_dlp/options.py`](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/yt_dlp/options.py) without using the design’s inventory. | 295 `add_option` calls and 495 long-option literal occurrences; “majority” cannot be audited without a generated manifest. |
| IC-002 | Parsed pinned [`_extractors.py`](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/yt_dlp/extractor/_extractors.py). | 1,183 imported `*IE` class names; raw class count is not a defensible parity threshold. |
| IC-003 | Enumerated the pinned upstream test directory and inspected test symbols. | 32 top-level `test_*.py` files; `test_YoutubeDL.py` includes 29 core cases, `test_config.py` 8, `test_all_urls.py` 14, `test_plugins.py` 12. The proposed differential field list does not subsume these behavior classes. |
| IC-004 | Traced pinned [`YoutubeDL.process_ie_result`](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/yt_dlp/YoutubeDL.py). | Found transparent metadata merging, playlist/multi-video recursion, compatibility lists, and additional URLs absent from the illustrative source contract. |
| IC-005 | Traced pinned archive ID/read/write paths in [`YoutubeDL.py`](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/yt_dlp/YoutubeDL.py). | Baseline identity and write timing are concrete and differ from Ferric’s currently unspecified item/asset/sidecar model. |
| IC-006 | Compared current yt-dlp release/update and EJS primary documentation. | Core, extractor fixes, runtimes, and EJS assets have distinct compatibility/update needs. |
| IC-007 | Compared gallery-dl primary option/config documentation. | Post/child/file range and archive event/mode semantics are more granular than Ferric’s current filter/archive contract. |
| IC-008 | Compared Streamlink primary CLI/session documentation. | Player transports, ring-buffer behavior, live-edge, reload, retry, and continuous HTTP require explicit sink/live contracts. |
| IC-009 | Compared N_m3u8DL-RE primary README. | MSS, selection, count validation, ranges, live limits, real-time merge, and pipe mux are distinct acceptance paths. |
| IC-010 | Checked FFmpeg official documentation. | Streamcopy and programmatic progress are available, but container compatibility and filters make copy-versus-transcode a typed planning decision. |
| IC-011 | Checked current Reqwest, wreq, Wasmtime, and Component Model primary documentation. | Standard HTTP and browser impersonation are separate capability surfaces; wreq requires BoringSSL; WASI 0.3 async streams/futures are recent enough that process IPC is the lower-contract-risk v1 plugin choice. |
| IC-012 | Checked current yt-dlp EJS and Boa primary documentation. | Current EJS names only Deno/Node/QuickJS/Bun; Boa is pure Rust but experimental and not listed as supported, so all-Rust YouTube challenge compatibility is unproven. |
| IC-013 | Checked redb and rusqlite primary documentation. | A pure-Rust ACID archive candidate exists; SQLite adds a native library and cannot be silently chosen under the clarified authority. |

Static source-inspection commands were read-only and fetched the pinned raw files in memory. No source-design or repository file other than this deliverable was modified.

</topic>

<topic id="counterfactual-checks" status="final" version="0.2.0" summary="Concrete counterfactual invariants">

# COUNTERFACTUAL_CHECKS

1. If §26.2’s differential harness omits normalized HTTP transcripts and filesystem/archive artifacts, a wrong cookie/header/redirect policy or filename/archive side effect can ship while every listed metadata field matches.
2. If `SourceIdentity.variant` is removed or treated as an item-level archive key, completing one representation can cause another requested asset or derived output to be skipped.
3. If `TransparentRedirect` is normalized without a field-by-field overlay contract, outer title, section, uploader, or archive context can be lost or incorrectly overwrite the inner source.
4. If Phase 7 remains the first end-to-end EJS/YouTube proof, an incompatible graph, cache, worker, or transport decision can invalidate Phases 1–6 after they are already entrenched.
5. If the plugin handshake lacks core/protocol/extractor-pack compatibility ranges, a validly signed but incompatible pack can break routing or result decoding.
6. If `CollectionEntryStream::next_entry` produces completion-ordered results into a single deterministic output queue, one slow early entry can either stall later progress or force unbounded buffering.
7. If `OutputSinkSpec::LocalHttp` does not define reconnect and header replay, a player restart can receive a byte stream that is syntactically connected but not decodable.
8. If the benchmark reports only Rust CPU, moving parsing or challenge work into a persistent JS worker can create a false speed win with higher total cost.
9. If §18’s external-worker examples are treated as approved architecture, the implementation can violate the operator’s Rust-plus-FFmpeg dependency boundary before a necessity proof or operator decision exists.

</topic>

<topic id="boundary-probes" status="final" version="0.2.0" summary="Producer-consumer boundary probes">

# BOUNDARY_PROBES

| Boundary | Probe | Required invariant |
|---|---|---|
| CLI/config -> policy | Same option from system config, user config, explicit config, environment-aware default, and CLI; include negative/repeated forms. | Effective value and provenance match the selected compatibility profile. |
| Extractor -> graph | Transparent redirect containing outer metadata and an inner multi-asset source. | Overlay precedence and all graph edges survive normalization. |
| Graph -> selector/FFmpeg | Same formats with and without FFmpeg and with stdout versus atomic file. | Conditional default selection matches the profile and is recorded in explain output. |
| Collection listing -> filter | Width absent at listing, present after asset resolution. | `unknown` follows explicit policy; the item is not silently rejected. |
| Archive writer -> importer/exporter | Import legacy ID, extractor rename alias, then export. | No duplicate acquisition or identity collision; mapping is auditable. |
| Live stream -> fan-out | Lossless recording plus player that stops reading and reconnects. | Recording remains continuous within policy; player loss/disconnect is bounded and reported. |
| Plugin worker -> host | Worker streams entries, exhausts credits, then crashes. | Host bounds memory, attributes partial results, cancels capabilities, and can restart safely. |
| FFmpeg plan -> artifact | Stream-copy merge fails because target container rejects an input characteristic. | Typed fallback/error policy runs; no corrupt final path is committed. |
| Update index -> active extractor | New pack passes signature but fails compatibility health. | Last-known-good remains active and rollback is automatic/auditable. |

</topic>

<topic id="negative-path-checks" status="final" version="0.2.0" summary="Adversarial negative paths">

# NEGATIVE_PATH_CHECKS

1. Unknown CLI option that resembles a supported option: hard failure with suggestion; never ignore.
2. Supported spelling with unimplemented semantics: hard compatibility error; never mark implemented because parsing succeeded.
3. `--ignore-errors` plus failed post-processing: archive commit follows an explicit profile rule and reports partial success.
4. Transparent redirect loop: recursion budget terminates with a typed cycle error and no archive write.
5. Collection filter references a missing extension field: three-valued policy determines behavior and explain output names the missing field.
6. Same source ID under two extractor aliases: migration alias table prevents collision and records canonicalization.
7. Live manifest repeats a sequence number after discontinuity: continuity key prevents false deduplication.
8. Slow player sink with lossless file sink: bounded player policy cannot block or corrupt recording unless the user explicitly selected global backpressure.
9. Local HTTP player reconnect after mux header was already consumed: configured replay/restart policy produces a decodable stream or a typed refusal.
10. Plugin sends oversized/unknown-version frame: host closes the worker, releases capabilities, and preserves other jobs.
11. Challenge bundle update is interrupted: old bundle remains active; mixed files are never observed.
12. Browser-impersonation backend is absent for a mandatory extractor: compatibility report and runtime error name the exact capability; unrelated sites continue on the pure-Rust transport.
13. FFmpeg is missing, too old, or lacks a requested muxer: commands not requiring it can inspect sources; acquisition plans requiring it fail before bytes are committed.
14. Custom time range cuts across segment boundaries: output reports actual acquired boundaries and passes timestamp/playability validation.
15. Archive database is busy or storage worker stalls: async network workers remain bounded and cancellation does not leak a transaction.
16. Pure-Rust challenge engine fails one current EJS vector: full YouTube parity remains blocked; the implementation must not silently launch an external runtime.
17. Mandatory extractor requests impersonation while only the pure-Rust transport is available: return a precise unsupported-capability result and escalate the dependency decision; do not load BoringSSL/curl implicitly.

</topic>

<topic id="replacement-release-contract" status="proposed" version="0.2.0" summary="Minimum product and replacement gates">

# Recommended replacement and release contract

The project should distinguish product maturity from the stronger replacement claim. **`Tier-A` is a reviewer-proposed release label, not current project authority; its membership and the canary duration require operator approval.**

## Production-preview gate

- All Rust/FFmpeg/no-Python architectural invariants pass.
- Native direct, HLS, DASH, MSS, live, collection, sink, archive, operator-authorized challenge, and FFmpeg vertical slices pass their deterministic corpora.
- The published compatibility matrix reports every missing behavior without silent acceptance.
- Tier-A extractors and the operator’s required workflows pass; lower tiers may remain explicitly incomplete.

## “yt-dlp replacement” gate

- 100% of mandatory deterministic option/config/template/format/artifact cases pass or have an operator-approved divergence ID.
- 100% of Tier-A and YouTube-family cases pass.
- Every active extractor URL class in the pinned inventory has at least one controlled successful case or an operator-approved intentional divergence; dead/upstream-unsupported entries are classified, not counted as parity.
- Fourteen consecutive daily Tier-A canaries complete without an open severity-1 compatibility defect.
- All documented gallery-dl/Streamlink/N_m3u8DL-RE expansion scenarios pass; these are not optional extras used to offset missing yt-dlp parity.
- All frozen speed gates compare equivalent work and pass total-system plus per-process reporting.
- Signed extractor/challenge update, rollback, quarantine, and last-known-good recovery are proven on all supported platforms.

This gate is deliberately strict because the operator requested a full Rust replacement that expands functionality, not a smaller downloader marketed by extractor count.

</topic>

<topic id="residual-uncertainty" status="final" version="0.2.0" summary="What remains unproven after review">

# RESIDUAL_UNCERTAINTY

- No Ferric implementation, Cargo workspace, generated option inventory, extractor inventory, fixture corpus, benchmark baseline, or live canary exists; runtime behavior cannot be tested.
- The 1,183 extractor-class and option counts are static scale probes, not claims about active sites, user traffic, or required parity weighting.
- Current upstream websites can vary by account, geography, experiment, and time. Recorded fixtures must be paired with opt-in live canaries; neither alone proves production compatibility.
- Rust browser-impersonation clients were identified but not benchmarked or fingerprint-compared against the mandatory site corpus. The reviewed high-fidelity candidate uses BoringSSL, so no library selection is approved by this review.
- Full YouTube compatibility on an all-Rust runtime remains unproven. Boa is a research candidate, not an accepted EJS backend; external JS runtimes remain unauthorized.
- Process IPC is selected as the lower-contract-risk v1 plugin boundary, but its encoding and measured overhead remain open until a prototype exercises streaming, cancellation, and hotfix-pack workloads.
- SQLite is not approved as the default archive index under the clarified authority. redb is a pure-Rust candidate, but Ferric-specific performance and recovery proof does not yet exist.
- Distribution and redistribution decisions for FFmpeg and any operator-approved exceptions require a dependency/license review; this review does not make a licensing verdict or authorize exceptions.
- The proposed fourteen-day canary duration and Tier-A membership should become authority only after the operator approves the release contract and the project records the actual Tier-A inventory.

These uncertainties are unacceptable for an implementation-ready verdict but acceptable for beginning Phase 0 contract generation, targeted prototypes, and baseline measurement.

</topic>

<topic id="primary-sources" status="final" version="0.2.0" summary="Primary evidence opened for this review">

# Primary sources opened

- [yt-dlp `2026.07.04` README, options, dependencies, update channels, templates, selectors, configuration, plugins](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/README.md)
- [yt-dlp `2026.07.04` option definitions](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/yt_dlp/options.py)
- [yt-dlp `2026.07.04` extractor registry imports](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/yt_dlp/extractor/_extractors.py)
- [yt-dlp `2026.07.04` core result/archive behavior](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/yt_dlp/YoutubeDL.py)
- [yt-dlp `2026.07.04` core, configuration, URL-routing, plugin, and update tests](https://github.com/yt-dlp/yt-dlp/tree/2026.07.04/test)
- [yt-dlp external JavaScript setup](https://github.com/yt-dlp/yt-dlp/wiki/EJS)
- [yt-dlp YouTube JavaScript architecture announcement](https://github.com/yt-dlp/yt-dlp/issues/14404)
- [yt-dlp EJS source](https://github.com/yt-dlp/ejs)
- [gallery-dl options](https://github.com/mikf/gallery-dl/blob/master/docs/options.md)
- [gallery-dl configuration and archive behavior](https://github.com/mikf/gallery-dl/blob/master/docs/configuration.rst)
- [Streamlink CLI/player/live behavior](https://streamlink.github.io/cli.html)
- [Streamlink session options](https://streamlink.github.io/api/session.html)
- [N_m3u8DL-RE command and protocol behavior](https://github.com/nilaoda/N_m3u8DL-RE/blob/main/README.en.md)
- [FFmpeg command, streamcopy, mapping, and progress documentation](https://ffmpeg.org/ffmpeg.html)
- [Reqwest current documentation](https://docs.rs/reqwest/latest/reqwest/)
- [wreq current source, BoringSSL dependency, and build documentation](https://github.com/0x676e67/wreq)
- [Boa pure-Rust JavaScript engine source](https://github.com/boa-dev/boa)
- [Boa current conformance and architecture documentation](https://boajs.dev/)
- [redb pure-Rust embedded database source](https://github.com/cberner/redb)
- [rusqlite current source and build documentation](https://github.com/rusqlite/rusqlite)
- [Wasmtime current source and runtime documentation](https://github.com/bytecodealliance/wasmtime)
- [WebAssembly Component Model FAQ and WASI 0.3 status](https://component-model.bytecodealliance.org/reference/faq.html)
- [WIT async streams and futures](https://component-model.bytecodealliance.org/design/wit.html)

</topic>
