---
file_id: ferric-forager-peer-review-synthesis-v0.2.0
file_kind: adversarial-implementation-readiness-review
updated_at: 2026-07-18
source_file: ../../spec/ferric_forager_technical_design_v0.2.0.md
source_sha256: 5E04A35781B6D60DB25B1BEC1B1BC767877D822B85A9AF4410A9A96EE124407C
review_status: complete
---

<topic id="executive-verdict" status="final" version="1.0" summary="Implementation-readiness verdict and its practical meaning">

# Executive verdict

**Verdict: FAIL implementation readiness for the complete product. PASS as a Phase 0 architecture and research charter.**

The design is unusually thorough about goals, scope, proposed modules, and open questions, but it deliberately leaves decisions unresolved at the exact boundaries that control correctness, compatibility, performance, and recoverability. A no-context implementation team could start Phase 0 inventory, research, prototypes, and contract work from it. It could not independently build the complete product without inventing incompatible behavior.

This is not a recommendation to narrow Ferric Forager. The expanded downloader, recorder, protocol, metadata, archive, observability, and plugin functionality remains required. The correction is to turn the current proposals and questions into executable contracts before parallel implementation expands.

The requested technical direction is viable as a product goal, with one unresolved feasibility boundary: matching modern YouTube behavior can require JavaScript challenge execution and browser-like transport fingerprints. The currently researched high-fidelity implementations often introduce additional native or runtime dependencies. Those additions are outside the operator-authorized boundary. Therefore the project must first prove Rust-only solutions against a pinned corpus or return to the operator with measured evidence for a specific exception.

No source-design edits were made during this review.

</topic>

<topic id="fixed-authority" status="final" version="1.0" summary="Non-negotiable operator direction used by every reviewer">

# Fixed authority

The following constraints governed the review and are not reviewer suggestions:

- Ferric Forager is built fully in Rust.
- FFmpeg and ffprobe are required supervised external-process dependencies.
- No production Python runtime or Python fallback is permitted.
- Speed is the primary optimization objective among designs that first satisfy correctness, compatibility, bounded-resource, and recoverability gates. A faster wrong or incomplete result is not a speed win.
- yt-dlp is the compatibility foundation and behavioral oracle, not a production runtime dependency.
- The functionality expanded in the technical design remains in scope.
- Additional production runtimes and native libraries are not silently authorized. This includes Deno, Node.js, Wasmtime, BoringSSL-backed transports, SQLite native bindings, and similar dependencies unless the operator explicitly approves them after evidence is presented.

The strict dependency reading affects three proposed design areas immediately:

1. The current SQLite default is not approved. A pure-Rust store must be evaluated first; SQLite can be proposed only with measured necessity and an explicit exception.
2. A Wasm plugin host is not approved because it introduces a runtime dependency. The currently authorized v1 plugin direction is a separately executed Rust plugin using bounded, versioned process IPC. Wasm remains a security-motivated future option requiring explicit approval.
3. An external JavaScript runtime is not approved. YouTube challenge support must begin with a bounded Rust worker and a pure-Rust engine candidate, tested against a representative pinned corpus.

</topic>

<topic id="research-basis" status="final" version="1.0" summary="Independent research and inspected evidence supporting the verdict">

# Research basis

The review inspected the complete design and a shallow checkout of the exact yt-dlp `2026.07.04` release at commit `fdec00e0bf530dc6c3cc7b1dd780e95d9ae460e9`. The pinned extractor tree contained 971 Python extractor files and 2,034 classes matching the extractor-class pattern. Its option surface included 295 `add_option` declarations. These counts are inventory indicators, not direct compatibility requirements; a generated semantic inventory is still required.

The independent research compared the design against primary source code and official documentation for yt-dlp, FFmpeg, Tokio, Rust HTTP clients and impersonation projects, Boa, SQLite durability/WAL behavior, Wasmtime, Criterion, Loom, cargo-fuzz, and field implementations of Rust download/protocol tools. Relevant patterns found were:

- yt-dlp compatibility is broader than URL extraction: it includes invocation/config semantics, normalized source relationships, network behavior, filesystem artifacts, archive behavior, and failure classification.
- FFmpeg stream copy and machine-readable progress are appropriate foundations, but safe supervision requires a stronger contract than typed arguments alone.
- Tokio's blocking work defaults do not satisfy a blanket bounded-resource claim; blocking work needs explicit admission, cancellation, and ownership rules.
- Pure-Rust JavaScript engines exist, but conformance percentages do not prove yt-dlp EJS/challenge compatibility. Corpus evidence is required.
- High-fidelity HTTP impersonation candidates may bring native cryptographic dependencies and therefore conflict with the current boundary.
- SQLite WAL is not a network-filesystem durability solution. Filesystem behavior must be explicitly classified even if SQLite is later approved.
- Rust video downloaders observed in the field tend either to delegate to yt-dlp/FFmpeg or support a narrower protocol/site surface. That reinforces the need for an updateable compatibility system and early hostile-site prototypes.

Selected approach: use yt-dlp as a pinned executable oracle and source-level research corpus while implementing native Ferric semantics in Rust. Generate a versioned compatibility profile from the pinned upstream release. Prove the highest-risk Rust-only components before freezing crate topology or parallelizing full implementation.

Rejected approaches at this stage:

- Calling yt-dlp or Python in production, because it violates the operator's boundary.
- Treating faster startup or a subset of extractors as evidence of replacement parity.
- Selecting a JS runtime, impersonation library, plugin runtime, or archive database from familiarity alone.
- Using the current approximately thirty-crate proposal as an implementation assignment map before the core contracts stabilize.

</topic>

<topic id="adversarial-evidence" status="final" version="1.0" summary="Required adversarial review artifacts and independent checks">

# DIFF_ATTACK_SURFACES

Although this is a design review rather than a code diff, the attack surfaces are the design-to-implementation gaps where independent agents could make contradictory choices:

- dependency boundary: Rust-only production code versus JS execution, HTTP impersonation, plugin hosting, and SQLite proposals;
- parity boundary: prose parity goals without a generated, versioned compatibility corpus;
- source model: a serializable data-model claim containing a trait object and a linear result enum that cannot express yt-dlp relationship graphs;
- scheduling: CPU, memory, network, disk, subprocess, and fragment admission without one atomic resource model;
- byte pipeline: network bytes, validation, persistence, journaling, hashing, decryption, and muxing without end-to-end credits;
- durability: file rename, directory durability, journal advancement, and archive insertion spanning multiple failure domains;
- path safety: normalization followed by path use, with symlink/reparse-point and time-of-check/time-of-use races;
- FFmpeg supervision: process discovery, protocol exposure, stdin, pipe draining, cancellation, reaping, and output validation;
- updateability: extractor/challenge breakage treated late despite being a core replacement requirement;
- performance: benchmark definitions that can reward inequivalent work or process topology;
- observability: lossless recording and auxiliary sinks capable of blocking the critical data path;
- acceptance: numerous qualitative gates without executable fixtures, thresholds, failure oracles, or evidence schemas.

# INDEPENDENT_CHECKS_RUN

- Read the entire source design and recorded its SHA-256, size, and timestamp.
- Parsed its frontmatter and flat topic structure.
- Counted proposed decisions, accepted decisions, and peer-review questions.
- Checked the exact pinned yt-dlp release and inspected extractor, option, test, challenge, and execution surfaces.
- Compared performance claims against yt-dlp multi-URL execution and lazy extractor-loading behavior.
- Compared async and blocking assumptions against Tokio's official runtime behavior.
- Compared FFmpeg supervision assumptions against FFmpeg's documented CLI, progress, stream-copy, and protocol behavior.
- Compared JS, impersonation, plugin, and storage candidates against the strict dependency authority.
- Independently reviewed architecture/performance, compatibility/product, and security/reliability lenses.
- Cross-checked all three reviews for consensus, conflicts, invented taxonomy, and unauthorized dependency choices.

# COUNTERFACTUAL_CHECKS

- If Ferric launches once while the baseline launches yt-dlp 1,000 times, the claimed batch speedup may measure process startup rather than downloader efficiency. The benchmark must include equivalent persistent/batch topology.
- If a URL resolves publicly and redirects to a private address, initial-request SSRF checks pass while the attack succeeds. Every redirect and nested resource must be revalidated.
- If the journal advances before bytes are durable, resume can trust data that disappeared after power loss. Three distinct checkpoints are required: received, validated/written, and durable contiguous.
- If the file rename succeeds and archive insertion fails, retry can redownload or overwrite a valid output. Startup reconciliation must make every commit-state prefix idempotent.
- If a plugin is called "Wasm" without an approved runtime, implementation silently violates the dependency boundary. Process IPC avoids that unapproved dependency but still needs sandbox proof.
- If pure-Rust JS passes Test262 but fails live EJS challenge fixtures, standards conformance has not proved YouTube compatibility.
- If HTTP requests fetch identical bytes but use a fingerprint rejected by a mandatory source, functional HTTP tests pass while the replacement goal fails.
- If a recorder's telemetry consumer stalls, an in-process lossless fanout can deadlock media progress unless the recorder has priority and other sinks are isolated.

# BOUNDARY_PROBES

- empty and single-item collections, million-entry lazy collections, and collection termination during cancellation;
- tiny and oversized HLS/DASH/MSS manifests, discontinuities, live-window movement, missing segments, key rotation, byte ranges, and sequence wraparound;
- zero-length, truncated, duplicated, reordered, corrupted, and content-length-mismatched fragments;
- filenames at component and path limits, reserved Windows names, Unicode normalization collisions, symlinks, junctions, and reparse points;
- same-volume and cross-volume work/final paths; local filesystems and network shares;
- FFmpeg missing, wrong version, hung, chatty on stderr, early exit, non-zero exit after output creation, and cancellation during finalization;
- JS worker timeout, memory exhaustion, recursion, malformed IPC, hostile script, and worker crash;
- plugin protocol version mismatch, oversized message, partial frame, crash, stdout pollution, and privilege escape attempts;
- archive collision, stale lease, schema migration interruption, committed output with missing archive row, and archive row with missing output;
- very slow network, disk, FFmpeg, event sink, and cancellation listener under full concurrency.

# NEGATIVE_PATH_CHECKS

- No Python binary may be consulted as a fallback when an extractor, JS challenge, or plugin fails.
- No arbitrary user string may become a shell command; process execution uses typed executable/argument/environment contracts.
- No credential may cross origins merely because a redirect or nested manifest requested it.
- No output may escape the configured root through traversal, symlink, junction, reparse point, race, or template expansion.
- No job may report success until the output is validated, committed, and reconciled with the archive contract.
- No resume state may claim bytes beyond the durable contiguous prefix.
- No benchmark case may be counted if the two systems performed materially different work.
- No claimed replacement release may hide unsupported mandatory cases behind aggregate pass rates.

# INDEPENDENT_FINDINGS

## SYN-001 — Critical: dependency boundary conflicts are unresolved

Full YouTube replacement can require JS challenge execution and browser-like transport behavior, while the current design also proposes SQLite and Wasm. None of the additional runtimes/native libraries are authorized. Mitigation: run Rust-only challenge, transport, plugin IPC, and storage spikes; document corpus results, dependency trees, licensing, binary/runtime impact, and failure coverage. Ask the operator only if evidence proves a specific exception is materially necessary.

## SYN-002 — Critical: the compatibility target is not enumerable

"Match yt-dlp" and categorized CLI tables do not tell an implementation agent which behaviors, extractors, relationships, errors, and artifacts must match. Mitigation: generate a pinned compatibility profile and fixture corpus from yt-dlp `2026.07.04`, then combine it with a native Ferric semantic specification. Every divergence needs an ID, rationale, expected output, and operator approval where it changes promised behavior.

## SYN-003 — Critical: performance gates can produce false wins

Current gates can compare a persistent Rust process with repeated yt-dlp startup, allow unequal output work, or move cost into JS/FFmpeg. Mitigation: a versioned benchmark manifest must fix hardware state, network mode, cache state, input corpus, output bytes, metadata work, post-processing, process topology, warmup, samples, distributions, and confidence intervals. Correctness equivalence is a prerequisite for including a sample.

## SYN-004 — High: the source model and extraction sequence contradict the design's own contracts

`MediaCollection.entries: Box<dyn CollectionEntryStream>` contradicts a serializable data-only model and cannot cross JSON/IPC boundaries. The main job sequence also places JS after source extraction even though JS can be required during extraction. Mitigation: use a canonical serializable source graph plus separate runtime cursors/streams; model typed nodes and edges, overlays, identities, and pagination. Make JS and transport services callable from extraction, not a later linear phase.

## SYN-005 — High: bounded-resource behavior is not defined end to end

Separate semaphores cannot safely admit jobs that simultaneously consume CPU, memory, disk, network, and subprocess capacity. Fragment queues also lack byte credits. Mitigation: one central resource-vector broker with atomic admission, RAII release, structured cancellation, per-job and global budgets, bounded channels by item and byte, and explicit FFmpeg/transcode accounting. Start adaptive concurrency only after a fixed policy is proven.

## SYN-006 — Critical: commit, resume, archive, and path safety lack a recoverable state machine

File, directory, journal, and database operations can be interrupted at every prefix. Path normalization alone cannot prevent races. Mitigation: same-filesystem work roots, root-handle-relative no-follow operations, a filesystem capability matrix, durable staged commit records, explicit collision policy, file and directory synchronization where supported, unique archive transactions, and deterministic startup reconciliation.

## SYN-007 — High: the FFmpeg boundary is under-specified

Typed arguments are necessary but insufficient. Mitigation: versioned process protocol, trusted absolute executable selection, minimum version/features, scrubbed environment, `-nostdin`, restricted protocols, bounded progress/parser input, continuous stdout/stderr draining, OS process-group or Windows Job Object containment, graceful-then-forced termination, mandatory reap, and ffprobe output validation.

## SYN-008 — High: compatibility updates arrive too late in the proposed phases

Static built-ins cannot support a replacement target whose extractors and challenges change upstream. Mitigation: move signed, versioned compatibility-profile and update architecture into Phase 0/1. Separately version the core, compatibility profile, extractor/challenge data, and plugin protocol; provide quarantine, rollback, last-known-good activation, and audit receipts. Executable Rust logic still ships as a binary release unless the operator later approves a different runtime mechanism.

## SYN-009 — High: acceptance criteria are not executable

Many gates say "matches," "bounded," "robust," or "safe" without fixtures and thresholds. Mitigation: convert every phase gate into a command, corpus version, expected artifact, threshold, timeout, failure oracle, and evidence output. A release must have zero unresolved critical/high findings in the mandatory corpus and explicit classification of dead/upstream-unsupported cases.

# RESIDUAL_UNCERTAINTY

- It is not yet proven that a pure-Rust JS engine can execute the required current and historical YouTube challenge corpus within the required security and performance bounds.
- It is not yet proven that an approved pure-Rust HTTP stack can reproduce every fingerprint required by the mandatory source corpus.
- It is not yet proven that a pure-Rust embedded store satisfies the archive transaction, migration, concurrency, and crash-recovery targets better than a narrowly approved SQLite exception.
- The complete compatibility surface cannot be quantified until the generated profile and corpus exist.
- Filesystem durability guarantees vary by OS and filesystem and need fault-injection evidence.
- Real-world throughput cannot be predicted from architecture prose; equivalent-work benchmarks and network replay are required.

</topic>

<topic id="reviewer-consensus" status="final" version="1.0" summary="Consensus and adjudicated disagreements among the three reviewers">

# Reviewer consensus

All three reviewers independently concluded:

- The complete design is not implementation-ready.
- Phase 0 inventory, corpus construction, contract definition, and risk prototypes can begin.
- yt-dlp compatibility must be converted from prose into a generated versioned profile and executable oracle corpus.
- YouTube JS and transport impersonation must be proven early, not deferred.
- The proposed benchmark gates are not valid until equivalent work and statistical rules are defined.
- The source model must become a serializable graph rather than a trait-object-bearing enum.
- A central multi-resource admission model and end-to-end byte backpressure are required.
- Commit/archive/resume/path behavior needs explicit state machines and fault injection.
- FFmpeg needs a hardened supervised-process contract.
- The proposed crate count is premature; preserve logical boundaries but begin with coarser implementation units.
- Full HLS, DASH, MSS, byte-range, encryption, discontinuity, live-window, recorder, and mux behaviors remain required for the complete product.

# Adjudicated disagreements

## Plugin isolation

The security reviewer preferred Wasm isolation; the other reviewers preferred process IPC. Resolution: **Rust executable plugins over versioned, bounded process IPC are the authorized v1 direction**, because Wasmtime is an unapproved runtime dependency. This is not a claim that process IPC is automatically secure; sandbox, capability, framing, timeout, resource, and crash tests remain gates. Wasm may be reconsidered only through explicit operator approval.

## Archive storage

One reviewer preferred SQLite after hardening; another correctly identified it as outside the strict dependency boundary. Resolution: **the design's current SQLite default is unapproved**. Benchmark and fault-test a pure-Rust store such as `redb` or an equivalent maintained candidate first. If it cannot meet the contract, present exact failures and the smallest SQLite exception to the operator.

## Real-time muxing

Reviewers agreed it belongs to the complete product but differed on when it can be enabled. Resolution: retain it in scope, but make **record-then-mux the first recovery-proven path**. Real-time mux becomes an opt-in mode only after cancellation, crash, partial-output, and replay equivalence tests pass.

</topic>

<topic id="peer-question-resolutions" status="final" version="1.0" summary="Recommended resolutions for all 25 open peer-review questions">

# Resolutions for the 25 peer-review questions

These are review recommendations to promote into the technical design; they are not authority until accepted there.

1. **Compatibility authority:** use two linked surfaces: native Ferric semantic contracts plus a generated compatibility profile and oracle corpus pinned to yt-dlp `2026.07.04`.
2. **Model extensibility:** keep typed core fields and bounded namespaced extensions. Do not hide fields that change behavior, identity, security, or output naming in extensions. Persist only serializable descriptors, never runtime trait objects.
3. **Plugin boundary:** v1 uses separately executed Rust plugins with versioned process IPC. Wasm is a future option requiring explicit approval.
4. **Transport abstraction:** the current trait is insufficient. Add streaming bodies, cancellation, pool identity, DNS/SSRF policy, redirect policy, cookie scope, fingerprint capabilities, replay hooks, and sanitized transcript contracts.
5. **Impersonation blocking:** it blocks only source claims that require it, but a complete replacement release cannot pass while any mandatory corpus case depends on an unavailable transport capability.
6. **JavaScript engine:** start with a Rust worker and pure-Rust engine candidate tested against a pinned EJS/challenge corpus. No external JS runtime is authorized. A failed spike returns evidence to the operator.
7. **JS sandbox:** per-job contexts, no ambient host capabilities, deterministic IPC, source/byte/time/memory/recursion/output limits, worker recycling, OS containment, and crash quarantine are minimum requirements.
8. **Fragment memory:** the direction is sound only after queues use byte credits and the pipeline tracks received, validated/written, and durable-contiguous checkpoints.
9. **Durability:** document exact local-filesystem behavior and test it. Network and unsupported filesystems use an explicit degraded or rejected mode until their capability matrix passes.
10. **FFmpeg admission:** account for process slots, aggregate CPU, memory, disk read/write, pipe buffers, and codec/mux class. Transcodes need stricter budgets than stream copy.
11. **Ordering:** logical source order governs persisted identity and user-visible naming. Completion-order progress may be shown only when every event also carries logical sequence and stable identity.
12. **Performance gates:** replace provisional ratios with an equivalent-work benchmark manifest, distributions and confidence bounds. Correctness/parity gates run before speed comparisons.
13. **CLI compatibility:** inventory every option and interaction first. Unsafe arbitrary shell/command behaviors should become typed Rust equivalents or explicit approved divergences, never silent omissions.
14. **Extractor replacement gate:** 100% of the versioned mandatory deterministic corpus and every active pinned extractor URL class must pass or have an explicitly approved divergence. Dead or upstream-unsupported cases are classified, not hidden in a percentage. Expanded Ferric scenarios must also pass.
15. **Dependency distribution:** FFmpeg/ffprobe use a configured or discovered trusted absolute path, with an optional separately managed verified bundle if the operator chooses. No JS runtime bundle is authorized. Storage remains subject to the pure-Rust decision above.
16. **Crate topology:** retain logical seams, but start with coarse crates around model/contracts, engine, extractors, protocols, storage, FFmpeg, plugin IPC, CLI, and tests. Split only on security boundary, independent lifecycle, public API, or measured build/ownership pressure.
17. **State machines:** define job/cancellation, source/redirect, resource admission, fragment/durability, live-window, sink fanout, FFmpeg, JS worker, plugin, and commit/archive machines.
18. **Fuzzing:** fuzz M3U8/MPD/MSS, templates/selectors/paths, journals, IPC/JS/plugin frames, cookies, and FFmpeg progress. Enforce allocation, recursion, input, output, and time bounds in addition to crash freedom.
19. **No Python:** confirmed. Port in-scope plugins/extractors to Rust and the Rust process protocol. A legacy Python bridge is excluded from production and replacement claims.
20. **Update architecture:** signed stable and hotfix channels, independently versioned compatibility/extractor/challenge data, atomic activation, quarantine, rollback, last-known-good, expiry, and audit receipts. New executable Rust logic ships through binary releases.
21. **Public source API:** an internal enum may remain a convenience, but the canonical persisted/IPC model is a serializable graph. Public APIs expose stable typed views over it.
22. **Early versus late metadata:** emit only listing-known fields early, using explicit unknown/not-applicable/present states. Resolve asset-dependent fields later and make avoided requests observable.
23. **Archive backend and identity:** SQLite is not currently approved. Evaluate a pure-Rust store against the crash/concurrency contract. Separate item, representation, track, asset, and derived-output identities; insert the archive record only as part of reconcilable committed-output state.
24. **Event fanout:** trusted bounded Rust fanout can be in-process, with lossless recording taking priority. Isolate untrusted, blocking, or slow sinks behind bounded adapters/processes and explicit drop/backpressure policy.
25. **Protocol scope:** all listed HLS/DASH/MSS, live, byte-range, encryption, discontinuity, recorder, and mux behaviors are mandatory for the complete production engine. Real-time mux is enabled only after record-then-mux recovery is proven.

</topic>

<topic id="implementation-gates" status="final" version="1.0" summary="Minimum changes required before broad implementation">

# Minimum implementation-readiness gates

The design becomes ready for broad parallel implementation only when all of these are true:

1. A generated, versioned yt-dlp compatibility inventory and mandatory corpus exists, including CLI/config, source graph, sanitized HTTP transcript, filesystem/process artifacts, error taxonomy, and archive semantics.
2. Every proposed/accepted/open decision affecting public behavior, dependency boundaries, security boundaries, identity, durability, or performance is promoted into an accepted contract or explicit blocked decision.
3. The Rust-only YouTube challenge prototype passes its defined corpus or produces operator-ready exception evidence.
4. The approved Rust HTTP transport passes required fingerprint, redirect, cookie, SSRF, replay, streaming, pooling, and cancellation cases.
5. The archive store decision passes crash, concurrent-claim, migration, stale-lease, reconciliation, and filesystem tests within the dependency boundary.
6. Canonical source graph, identity, plugin IPC, event, error, cancellation, and process protocols are versioned and serializable.
7. The scheduler/resource-vector and byte-credit models have executable invariants, Loom/model tests where appropriate, and saturation/cancellation tests.
8. The commit/archive/resume state machine passes fault injection at every durable prefix on the supported filesystem matrix.
9. FFmpeg supervision passes missing/hung/chatty/crashed/cancelled/partial-output and descendant-process tests on Windows and Unix targets.
10. Equivalent-work correctness and performance benchmark manifests produce reproducible evidence with confidence intervals.
11. Phase gates are executable commands with versioned inputs, expected outputs, thresholds, and machine-readable reports.
12. The built-in no-context model manual, structured diagnostics, stable identifiers, logs, traces, health checks, screenshots for GUI surfaces if introduced, and parallel-agent-safe receipts are explicit product requirements.

# Phase readiness

- **Phase 0 — ready to start:** inventory, source research, corpus generation, contracts, risk spikes, benchmark design, and dependency adjudication.
- **Phase 1 and later — blocked:** implementation agents would otherwise invent contracts at the JS, transport, model, scheduler, durability, archive, plugin, and compatibility boundaries.
- **Complete replacement release — blocked:** no executable parity corpus or proven Rust-only YouTube boundary exists yet.

</topic>

<topic id="next-closure-unit" status="final" version="1.0" summary="Smallest useful next action that advances implementation readiness">

# Recommended next closure unit

Create the Phase 0 contract-and-risk package, not production modules yet. Its acceptance surface should be five executable outputs:

1. a generated yt-dlp `2026.07.04` compatibility profile and representative mandatory corpus;
2. a bounded Rust-only EJS/YouTube challenge worker spike with pass/fail evidence;
3. a pure-Rust transport/fingerprint spike against the cases that actually require impersonation;
4. a pure-Rust archive-store versus SQLite evidence matrix, with SQLite remaining unapproved unless an exception is requested;
5. one narrow vertical slice—direct HTTP plus representative HLS, FFmpeg stream copy, cancellation, resume, crash reconciliation, and equivalent-work benchmark—to validate the cross-boundary contracts.

This package should end with accepted decisions and machine-readable contracts that let multiple no-context Rust implementation agents work without choosing architecture by accident. It does not reduce the final feature scope and does not count as full implementation.

</topic>

<topic id="review-artifacts" status="final" version="1.0" summary="Independent reports and evidence provenance">

# Review artifacts

- `review-charter.json` — fixed authority and review protocol.
- `architecture-performance-review.md` — concurrency, performance, protocol, FFmpeg, and topology lens.
- `compatibility-product-review.md` — yt-dlp parity, product semantics, updateability, CLI, extractor, and expanded-function lens.
- `security-reliability-review.md` — adversarial input, durability, path safety, sandboxing, cancellation, supply-chain, and recovery lens.
- `implementation-readiness-matrix.json` — machine-readable verdict, blockers, decisions, and gates.

Primary sources inspected included the pinned yt-dlp source/release, FFmpeg documentation, Rust/Tokio documentation, Boa source and documentation, Rust HTTP/impersonation projects, SQLite transaction/WAL documentation, Wasmtime documentation, Criterion, Loom, cargo-fuzz, and representative Rust downloader/protocol repositories. The three lens reports contain their individual source logs and research-to-finding mappings.

</topic>
