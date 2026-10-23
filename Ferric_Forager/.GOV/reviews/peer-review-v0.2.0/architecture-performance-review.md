---
file_id: "FERRIC-FORAGER-ARCHITECTURE-PERFORMANCE-REVIEW-0.2.0"
file_kind: "adversarial-peer-review"
updated_at: "2026-07-18T00:29:45+02:00"
---

<topic id="findings-first" status="final" version="0.2.0" summary="Severity-ranked architecture and performance findings">

# Architecture, Rust systems design, and speed review

## Verdict

**FAIL — not implementation-ready beyond Phase 0.** The direction is strong and consistent with the fixed operator authority: Ferric Forager is Rust product code, has no production Python runtime, uses FFmpeg/ffprobe as supervised external dependencies, uses yt-dlp as its behavioral foundation, optimizes for speed first, and retains the expanded media/collection/live functionality. The design does not yet define the execution contracts needed to implement or objectively validate that direction.

The blocking problems are not missing prose polish. They affect scheduler correctness, bounded-memory proof, benchmark validity, transport compatibility, subprocess cleanup, persistence guarantees, and which dependency/runtime stack can satisfy the stated product boundary.

## Critical findings

### [P0-AP-001] The performance gates can produce a false speed win

**Evidence.** Sections 27 and Appendix C list metrics and percentage targets, but do not define the compared yt-dlp artifact, invocation topology, cache state, input bytes, fragment-size distribution, output equivalence predicate, CPU-power state, run count, confidence interval, noise threshold, or whether dependency processes are included (`ferric_forager_technical_design_v0.2.0.md:2164`, `:2189`, `:2835`). The `Persistent 1,000-URL batch` target says “40% lower process-start overhead,” while the pinned yt-dlp CLI itself accepts `URL [URL...]` in a single invocation; comparing a persistent Ferric worker against 1,000 yt-dlp process launches would be invalid ([pinned yt-dlp usage](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/README.md#usage-and-options)). The proposed registry target counts regular-expression evaluations rather than time or CPU. yt-dlp's pinned build workflow already generates lazy extractors specifically to improve startup, so an unoptimized source-tree baseline would also be invalid ([pinned lazy-extractor build step](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/README.md#compile)).

**Failure scenario.** Ferric reports a 50% startup win against the PyInstaller executable but a regression against the zipimport build; or reports a persistent-batch win only because the baseline was restarted per URL. Both pass the current table while failing the speed-first product claim.

**Required resolution.** Replace the provisional table with a versioned benchmark manifest that pins:

- exact Ferric commit, Rust toolchain, Cargo profile, feature set, allocator, FFmpeg/ffprobe version, embedded challenge-engine version, and transport backend;
- exact yt-dlp `2026.07.04` artifact variants: official standalone, zipimport/source runtime where supported, lazy extractors enabled, and one-process multi-URL mode;
- fixture hash, byte count, fragment-count and fragment-size distribution, request sequence, expected normalized metadata, output stream/container fingerprint, and expected failure class;
- machine, OS build, filesystem, storage medium, antivirus state, CPU governor/power mode, core affinity, memory pressure, cold/warm filesystem and DNS cache policy;
- warm-up policy, randomized A/B order, minimum repetitions, outlier policy, 95% confidence interval, and a predeclared noise threshold. Criterion documents confidence intervals and noise thresholds for microbenchmarks, while hyperfine documents warmups, cache preparation, repeated runs, and outlier detection for commands ([Criterion analysis](https://bheisler.github.io/criterion.rs/book/analysis.html), [hyperfine methodology](https://github.com/sharkdp/hyperfine#usage)); these tools are examples, not automatically sufficient for end-to-end CPU/I/O accounting;
- equality gates before speed gates: byte hash where byte identity is expected, ffprobe-normalized stream/container equivalence where remux timestamps may differ, normalized metadata equivalence, request-count ceiling, and identical success/failure classification;
- paired gates expressed as confidence bounds: the upper 95% confidence bound must remain within the no-regression limit, and the lower bound must exceed the improvement target. A single sample mean must never pass a gate.

The `70% fewer full-pattern evaluations` metric may remain diagnostic, but the release gate must be CPU time per routed URL and p50/p95 latency on a corpus containing exact domains, suffix domains, adversarial collisions, malformed URLs, and generic fallbacks.

### [P0-AP-002] Multi-resource admission is unspecified and can deadlock or starve jobs

**Evidence.** A plan node may declare “one or more” resource classes (`:1249`), and fairness is promised at job, origin, class, fragment, and FFmpeg queues (`:1296`), but the design never defines atomic admission, acquisition order, permit lifetime, or what happens when cancellation occurs while a node holds only some permits. Independent per-class semaphores are not a scheduler. Tokio's semaphore is FIFO-fair, and a large `acquire_many` at the head can block smaller requests even when some permits are available ([Tokio `Semaphore`](https://docs.rs/tokio/latest/tokio/sync/struct.Semaphore.html#fairness)). Multi-resource fairness is a distinct allocation problem; Dominant Resource Fairness is one established model rather than an incidental property of separate slot limits ([Berkeley/USENIX DRF paper](https://www.usenix.org/conference/nsdi11/dominant-resource-fairness-fair-allocation-multiple-resource-types)).

**Failure scenario.** Node A holds `NetworkMedia` and waits for `DiskWrite`; node B holds `DiskWrite` and waits for `NetworkMedia`. A separate case has a large collection continuously filling metadata permits while a direct job waits behind its children. Cancellation can leak an acquired permit if ownership is not represented by a single RAII admission object.

**Required resolution.** Define one central admission broker. Every runnable node submits an immutable resource vector before starting. The broker grants the vector atomically or grants nothing; no task may await a second scheduler permit while holding the first. The grant is one RAII object released on success, error, panic, or cancellation. Scheduling should use per-job weighted deficit/round-robin for ordinary work, explicit per-origin caps/token buckets for requests, and a dominant-share or measured-cost tie-break for composite resources. The policy must specify starvation bounds and be deterministic under a test clock.

The resource vector must include more than the current enum: network request slots, media-byte buffer credits, metadata-byte credits, open-file/handle credits, disk-read and disk-write in-flight bytes, CPU-light and CPU-heavy slots, JavaScript worker slots, FFmpeg process slots plus CPU-thread budgets, archive writer capacity, and per-sink fan-out bytes. “Global memory budget” must be an admission-controlled byte counter, not a telemetry threshold observed after allocation.

### [P0-AP-003] The fragment path is not provably bounded and its durability point is ambiguous

**Evidence.** The design bounds completed fragments in the reorder buffer (`:1136`) but not bytes already accepted by HTTP response readers, decompression, decryption output, packing output, writer buffers, journal staging, or multiple sinks. The in-memory/disk decision happens after a fragment is known to exceed a threshold (`:1150`), which is too late if the body has already been buffered. “Journaled as complete only after validation and successful handoff to durable output state” (`:1200`) conflicts with the fast/balanced modes that do not durably flush each fragment (`:1867`). The exact yt-dlp baseline genuinely writes, reopens, reads, appends, flushes, rewrites `.ytdl`, and removes fragment files, so it is a valid I/O optimization target ([pinned `fragment.py`](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/yt_dlp/downloader/fragment.py)); however, removing those files also removes their independent crash-resume boundary.

**Failure scenario.** Eight 256 MiB fragments without trustworthy `Content-Length` finish out of order. Each passes a count-based limit while response, decrypted, and packed copies coexist. The process exceeds the global budget before the reorder buffer can backpressure fetches. After a crash, the journal says fragment N completed although its bytes were only in an OS page cache or an unflushed user-space buffer.

**Required resolution.** Define an end-to-end byte-credit protocol:

1. A fetch obtains a small streaming credit before the request and may obtain additional credits only before accepting more body bytes.
2. Unknown-length or dishonest-length bodies never bypass the credit limit; they stream directly into a bounded chunk chain or a preselected spill file.
3. Transform stages transfer ownership of credits rather than creating unaccounted copies. Any unavoidable simultaneous input/output allocation must reserve both sizes.
4. The reorder buffer owns either credited immutable chunks or one spill-file descriptor/extent, never an untracked `Vec<u8>`.
5. A global and per-job out-of-order byte cap, not only fragment count/distance, stops scheduling.
6. Fan-out creates per-sink credits; a slow player cannot consume recording credits.

Define three distinct checkpoints: `validated`, `written_contiguous`, and `durable_contiguous`. Fast mode may resume only from `durable_contiguous` and redownload the unflushed suffix; balanced mode declares the maximum redownload window in bytes/time; durable mode flushes the data file before committing the journal record that references it. Crash tests must terminate the process between data write, data flush, journal write, journal flush, rename, archive commit, and directory cleanup.

## High findings

### [P1-AP-004] The transport trait cannot express the compatibility or performance contract

**Evidence.** `HttpTransport::execute` returns an unspecified future (`:955`) while the request model contains no explicit streaming response-body contract, response-body byte budget, DNS result/provenance, connection reuse identifier, H1 header order/case, H2 settings/pseudo-header order, TLS fingerprint, ALPN result, remote address, timing phases, cancellation outcome, or retry-safe body replay rule. The pinned yt-dlp backend does not treat impersonation as a User-Agent switch: it models browser/OS/version targets and uses curl_cffi/curl-impersonate profiles ([pinned impersonation model](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/yt_dlp/networking/impersonate.py), [pinned curl_cffi backend](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/yt_dlp/networking/_curlcffi.py)). curl-impersonate changes TLS and HTTP/2 handshake behavior, not only headers ([upstream curl-impersonate design](https://github.com/lwthiker/curl-impersonate#why)).

Current field evidence exposes a compatibility conflict, not an implied dependency approval: wreq provides Rust APIs and control over TLS/JA3/JA4 and HTTP/2 signatures, but uses BoringSSL and warns about native symbol conflicts; its current major line is still release-candidate software ([wreq upstream](https://github.com/0x676e67/wreq#behavior)). The fixed authority permits Rust product/runtime code and FFmpeg as the external dependency; it does not silently authorize an additional native transport library. wreq and curl-impersonate therefore demonstrate the wire controls Ferric may need, but neither is an accepted production dependency.

**Required resolution.** Use one logical request contract with capability negotiation and two separately pooled backends:

- standard backend: Tokio-compatible Rust crates, HTTP/1.1 and HTTP/2, with an explicit feature/dependency audit that excludes optional native TLS/transport backends and records the platform trust policy;
- impersonation backend: an in-tree Rust implementation or Rust dependency selected only after both a dependency audit and a corpus prove TLS, HTTP/2, header, redirect, cookie, proxy, range, streaming, and cancellation behavior across all three OSes. A wreq spike may measure the compatibility target in research, but it cannot become production architecture without proof that the native dependency is unavoidable and an explicit operator exception.

Connection-pool keys must be derived from all wire-identity and credential properties; “cookie/session partition where required” is too discretionary (`:987`). A connection may be shared across cookie jars only when the transport proves no connection-bound authentication or fingerprint state is involved. Browser impersonation is a release blocker only for extractors that declare it and are claimed supported, not for unrelated extractors. No compatibility claim may silently fall back from a requested fingerprint to the standard backend.

### [P1-AP-005] FFmpeg supervision is a list of intentions, not a process protocol

**Evidence.** Section 19 requires controlled standard streams, machine-readable progress, process-tree cancellation, and stderr tails (`:1568`) but does not define simultaneous pipe draining, EOF/exit ordering, progress channel conflicts with media stdout, graceful versus forced stop, process reaping, or thread budgets. FFmpeg documents that `-progress` emits `key=value` records ending in `progress=continue|end`, that its period is controlled by `-stats_period`, that stdin interaction is enabled unless `-nostdin` is supplied, and that stream copy must be explicitly selected with `-c copy` ([FFmpeg CLI documentation](https://ffmpeg.org/ffmpeg.html)). Tokio documents that dropping a child does not cancel it by default, `kill_on_drop` has reaping caveats, and strict cleanup requires waiting or killing and awaiting the child ([Tokio process contract](https://docs.rs/tokio/latest/tokio/process/struct.Command.html#method.kill_on_drop)). On Windows, tree ownership requires a Job Object or equivalent, not merely a child PID ([Microsoft Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)).

**Failure scenario.** FFmpeg fills stderr while Rust awaits progress on stdout, deadlocking both. A cancellation drops the `Child` handle but leaves FFmpeg or a descendant running and holding the temporary output. A mux process auto-creates many internal threads, so two “cheap mux” permits oversubscribe the machine and delay control-plane work.

**Required resolution.** Define a versioned `FfmpegInvocation` protocol with:

- capability probe cached by executable identity, file metadata/hash, and version; required muxer/demuxer/codec/filter features validated before execution;
- explicit stream maps and `-c copy` for every planned stream-copy operation; no reliance on FFmpeg defaults;
- `-nostdin`, bounded machine progress cadence, a dedicated progress channel that never shares unframed bytes with media output, continuously drained stderr with a bounded ring tail, and continuous media-pipe draining/writing;
- process lifecycle `Spawned -> Running -> GracefulStopRequested -> ForcedKillRequested -> Reaped -> Validated`; every terminal path awaits reaping;
- Unix process group and Windows Job Object attached at process creation, with stop timeout then tree kill;
- separate `mux_slots`, `transcode_slots`, and aggregate `ffmpeg_cpu_threads`; planner-supplied FFmpeg thread options where supported and benchmarked;
- ffprobe-based output validation before atomic rename and archive commit.

The mux/transcode class split is correct but incomplete until total FFmpeg CPU and I/O admission is enforced.

### [P1-AP-006] Runtime and crate topology are unresolved in the speed-critical core

**Evidence.** The design names roughly thirty crates (`:573`) before selecting an async runtime, HTTP stack, byte-buffer type, CPU executor, blocking-I/O strategy, or database access pattern. Tokio is an obvious candidate but its documented defaults contradict “bounded everywhere”: the blocking pool can default to 512 threads and its submission queue has no backpressure; started `spawn_blocking` work cannot be aborted ([Tokio runtime builder](https://docs.rs/tokio/latest/tokio/runtime/struct.Builder.html#method.max_blocking_threads), [Tokio `spawn_blocking`](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html)). Tokio filesystem operations normally use that blocking pool, and named pipes can behave unexpectedly when treated as ordinary files ([Tokio filesystem docs](https://docs.rs/tokio/latest/tokio/fs/)).

Crate count does not itself prove runtime overhead, but premature public traits and `Box<dyn ...>` boundaries do force allocation/dispatch/API costs into paths that have no second implementation. The design itself recognizes this risk at `:2462` but the proposed workspace contradicts its mitigation.

**Required resolution.** Select Tokio as the initial runtime contract because the chosen network/subprocess ecosystem is Tokio-compatible, but do not expose Tokio types in stable model schemas. Use a current-thread Tokio runtime for cold CLI paths and a measured multi-thread runtime for the persistent worker/service; CPU-heavy work runs in one explicitly sized Rayon/dedicated pool. Do not submit unbounded work to Tokio's blocking pool: use bounded disk/database actors or acquire an admission token before `spawn_blocking`; long-lived pipe/worker loops use dedicated tasks/threads with explicit shutdown.

Start with coarse crates and split on measured or ABI/security boundaries:

1. `fforager-model` (serializable data only),
2. `fforager-core` (planner, scheduler, events, errors),
3. `fforager-net`,
4. `fforager-protocol` (HLS/DASH/MSS plus fragment engine),
5. `fforager-extractors` plus generated registry,
6. `fforager-storage`,
7. `fforager-ffmpeg`,
8. `fforager-javascript`,
9. `fforager-cli`/worker frontends,
10. `fforager-testkit`.

Keep plugin-process and public API contracts separate. Split format selector, templates, collections, live, sink, dedup, archive, and scheduler only after dependency pressure, independent reuse, compile-time evidence, or a security boundary justifies it. This preserves all documented functionality while reducing speculative interfaces.

### [P1-AP-007] The JavaScript plan silently exceeds the Rust runtime boundary

**Evidence.** Sections 18.2–18.5 allow Deno, QuickJS, or embedded engines but do not reconcile those choices with “built fully in Rust with FFmpeg as dependency,” select a canonical engine, define framed IPC, define script/module loading, or state whether cookies/tokens/player state may survive between jobs. The current yt-dlp EJS guide says YouTube challenge solving needs a supported external JavaScript runtime, recommends Deno, and documents QuickJS performance cliffs ([yt-dlp EJS guide](https://github.com/yt-dlp/yt-dlp/wiki/EJS)). That proves a yt-dlp compatibility need; it does not authorize Ferric to add Deno. A Rust-owned alternative exists to investigate: Boa is an embeddable JavaScript engine written in Rust, reports more than 90% Test262 conformance, exposes runtime limits, and still labels itself experimental ([Boa project](https://boajs.dev/), [Boa introduction](https://boajs.dev/docs/intro), [Boa runtime limits](https://docs.rs/boa_engine/latest/boa_engine/vm/struct.RuntimeLimits.html)). Those claims do not prove compatibility with the pinned yt-dlp EJS challenge corpus or acceptable speed.

**Failure scenario.** A persistent worker leaks site A state into site B, grows indefinitely through compiled script caches, accepts a stale response after timeout because request IDs are reused, or is killed while the parent still attributes its CPU/memory to an active job.

**Required resolution.** Make a Ferric-owned Rust executable, `fforager-js-worker`, embedding a pure-Rust engine such as Boa the only currently authorized architecture candidate. The separate Rust process supplies fault/RSS containment without adding an external runtime. Expose no filesystem, network, environment, subprocess, FFI, or ambient host APIs to challenge scripts. Use one trusted bootstrap and length-prefixed frames with protocol version, monotonically unique request ID, job/session partition, script hash, deadline, maximum input/output bytes, structured result/error, and explicit cancellation acknowledgement. Recycle the Rust worker after a bounded job count, wall age, RSS threshold, protocol violation, timeout, or crash. Cache compiled/player artifacts in the Rust host by content hash; never treat worker heap as durable cross-job state.

Before selecting Boa, run the exact pinned `yt-dlp-ejs` script/challenge corpus plus adversarial termination, memory-growth, module-resolution, Unicode, integer/typed-array, Web API, and concurrency probes. Gate cold/warm execution, IPC, RSS growth, recycle, crash recovery, and end-to-end YouTube parity. If no Rust engine passes, the design must record that as a compatibility blocker and present the measured evidence and alternatives to the operator. An external Deno/Node/QuickJS runtime is not a silent fallback and cannot enter the production design without explicit operator resolution.

Cold start, warm execution, IPC serialization, cache-hit rate, RSS growth, recycle latency, and crash recovery need separate gates. Rust-worker process cost belongs in total latency even when CPU is reported separately.

### [P1-AP-008] Persistence choices can violate both speed and correctness across filesystems

**Evidence.** The work directory is shown next to `<output>` only by notation (`:1833`), while atomic rename is promised “where the filesystem supports it” (`:1892`) without requiring the temporary file to be on the same volume. The journal asks whether Windows and network filesystems are acceptable but supplies no answer (`:2593`). SQLite is selected as the default archive (`:1390`) without journal mode, synchronization level, connection topology, batching, busy handling, or filesystem policy. SQLite's official documentation says WAL permits concurrent readers and a writer but only one writer, requires checkpoint management, and does not work over network filesystems ([SQLite WAL](https://www.sqlite.org/wal.html)). It also records a WAL-reset corruption bug fixed in SQLite 3.51.3 and specific backports, directly relevant to a 2026 implementation using concurrent connections ([SQLite WAL-reset notice](https://www.sqlite.org/wal.html#the_wal_reset_bug)).

**Required resolution.** Put each seekable output work directory on the final output's filesystem by default and verify same-volume rename during planning. Cross-filesystem destinations use copy-then-fsync-then-rename inside the destination filesystem; they must not be labeled atomic across the copy boundary. Define local filesystems as the default supported durable-resume class. Network filesystems require an explicit compatibility mode with a tested VFS/filesystem matrix and conservative flush/rename behavior.

Use SQLite as the local default archive behind one bounded storage actor with batched membership queries and short transactions. Pin SQLite to a release containing the 2026 WAL fix. WAL may be enabled only on supported local filesystems; network-backed archive paths fall back to a proven rollback-journal mode or are rejected with an actionable diagnostic. Predeclare `synchronous`, busy timeout/retry, checkpoint, connection-count, schema/index, batch-size, and archive-commit semantics, then benchmark p50/p95 lookup, transaction latency, WAL growth, checkpoint stalls, write amplification, and crash recovery at 1,000 and 1,000,000 identities.

### [P1-AP-009] `SourceResult` mixes serializable data with a live execution object

**Evidence.** `MediaCollection` owns `Box<dyn CollectionEntryStream>` (`:680`, `:1331`) while JSON schemas, worker IPC, plugin protocols, differential snapshots, and a reusable API are all promised elsewhere. A trait object with mutable stream state cannot be directly serialized, hashed, replayed, or compared. The document calls the model a typed “source graph,” but an embedded opaque iterator is control-plane state, not source data.

**Required resolution.** Keep one internal `SourceResult` enum for dispatch, but make each variant a serializable descriptor. `CollectionDescriptor` contains identity, ordering, hierarchy, estimated length, checkpoint schema/version, and an opaque provider handle. The provider/session owns the async entry stream behind a host interface. Public top-level APIs remain distinct—`resolve_media`, `open_collection`, and `open_live_session`—so callers cannot accidentally apply finite-media completion semantics to live or collection handles. Fixtures compare descriptors and deterministic entry pages, not trait-object identity.

This preserves the accepted first-class distinctions while making library, worker, plugin, cache, and differential boundaries implementable.

## Medium findings

### [P2-AP-010] Adaptive fragment concurrency is an unbounded control experiment

The optional controller lists signals but no algorithm, sampling period, hysteresis, state partition, minimum dwell time, or fairness interaction (`:1173`). Throughput-only feedback can increase concurrency into a throttling regime, while global queue pressure can punish an unrelated origin. Make fixed concurrency the production default. Treat adaptive mode as experimental until an origin-scoped controller specification and deterministic simulation exist. Benchmark it against fixed values across RTT, bandwidth-delay product, fragment sizes, 429/503 responses, transient errors, slow disks, and competing jobs. It must never exceed user, extractor, origin, memory-byte, or open-handle budgets.

### [P2-AP-011] Event observability can become the hot path it is intended to diagnose

The design promises per-request/per-fragment typed events plus multiple consumers (`:1991`) and only later says fragment events are normally trace-only (`:1819`). Define one allocation-bounded event envelope, per-consumer bounded queues, a drop/coalesce policy for telemetry, and a lossless separate terminal-state channel. Never let a slow terminal, metrics exporter, or host callback hold scheduler/data-plane permits. Add benchmark profiles with zero consumers, terminal aggregation, JSONL, metrics, and trace; gate CPU/item, allocated bytes/event, queue high-water mark, and dropped/coalesced count.

### [P2-AP-012] The phase order postpones FFmpeg proof beyond features that depend on it

Phase 3 produces manifest outputs and live sinks, and Phase 4 makes merge decisions, while FFmpeg discovery, typed plans, cancellation, and atomic finalization wait until Phase 5 (`:2271`–`:2310`). That prevents earlier phases from proving equivalent DASH audio+video output, timestamp continuity, pipe behavior, or cancellation. Move a minimal FFmpeg/ffprobe supervisor vertical slice—discovery, capability probe, `-nostdin`, stream-copy merge, progress, kill/reap, validation—into Phase 2/3. Phase 5 can still expand metadata, thumbnail, subtitle, chapter, remux, and transcode operations. No documented function is removed.

## Positive findings that survived attack

- Keeping FFmpeg external is correct and conforms to the operator's fixed boundary. FFmpeg's documented stream-copy path avoids decoding/encoding and is the right default when container constraints permit it ([FFmpeg streamcopy](https://ffmpeg.org/ffmpeg.html#Streamcopy)).
- Replacing yt-dlp's per-fragment temporary-file fast path is a credible speed/I/O target because the pinned source confirms those operations. The finding is that the replacement needs a stronger byte-credit and durability contract, not that the target should be abandoned.
- Separating Rust, JavaScript, and FFmpeg CPU accounting is necessary. Add total job CPU and total child-process RSS/I/O as well so separate ownership cannot hide a regression in end-to-end cost.
- Lazy collection traversal, explicit live sessions, output sinks, MSS/custom-range support, and archive-backed filtering are compatible with a speed-first architecture when their queues and byte budgets are contractual.

</topic>

<topic id="decision-resolutions" status="final" version="0.2.0" summary="Concrete resolutions to peer-review questions">

# Concrete decision resolutions

The following resolve the architecture/performance-relevant questions without narrowing the required functionality.

| Question | Resolution |
|---:|---|
| 1 | Define a native Ferric semantic specification first; use pinned yt-dlp `2026.07.04` as a differential oracle. Oracle differences require classification and explicit acceptance, never silent drift. |
| 2 | Typed core plus namespaced extension map is viable only with typed provenance, size limits, stable serialization, and no live trait objects inside serializable descriptors. |
| 3 | Canonical third-party boundary: versioned process IPC with a Rust plugin SDK and persistent, lazily started Rust plugin executables. This preserves crash isolation without adding a Wasm/foreign production runtime. The protocol transports metadata and typed host requests, never bulk media bytes. A Wasm host is a future alternative only after measured necessity and explicit operator approval. |
| 4 | Current transport abstraction is insufficient. Add streamed body/credit, wire-fingerprint, pool identity, timing, cancellation, proxy, redirect, DNS, and replayability contracts. |
| 5 | Browser impersonation blocks release only for an extractor declared supported that requires it. It does not block unrelated extractors. |
| 6 | Prefer a persistent Ferric-owned Rust worker executable embedding a pure-Rust engine, provisionally Boa, with bounded recycling and OS isolation. Selection requires a pinned EJS parity/performance corpus. |
| 7 | Current JavaScript sandbox list is insufficient until engine compatibility, host-capability denial, module loading, request framing, state separation, recycling, and kill/reap are specified. External Deno/Node/QuickJS requires evidence of necessity and explicit operator resolution. |
| 8 | The fragment direction is correct but not yet crash-safe or memory-bounded. Adopt byte credits and validated/written/durable checkpoints. |
| 9 | Current journal policy is not acceptable on Windows/network filesystems until filesystem classes and exact guarantees are defined. Default durable resume to tested local filesystems; make network-filesystem mode explicit. |
| 10 | Separate mux and transcode slots, plus total FFmpeg CPU-thread and disk-bandwidth budgets. |
| 11 | Default persisted/output naming order to logical source order; default live progress events to real-time completion order. Each event carries both logical and completion sequence. |
| 12 | Provisional performance gates are not valid release gates. Replace them with the benchmark manifest and confidence-bound policy in P0-AP-001. |
| 13 | Architecture/performance does not justify removing functionality. Shell-string execution and unrestricted external-downloader arguments must be replaced by typed safe equivalents, not counted as speed-driven removals. |
| 14 | “Replacement” requires 100% of the operator-approved core option/behavior corpus and 100% of the declared first-release extractor corpus; report long-tail extractor coverage separately. Do not use a vague percentage over thousands of unequal extractors. |
| 15 | Support both discovered FFmpeg/ffprobe installations and reproducible convenience bundles. They remain external processes in both, and manifests pin exact versions and capability hashes. No additional JavaScript runtime bundle is currently authorized; challenge execution is inside the Ferric Rust worker if its corpus passes. |
| 16 | Yes, the proposed crate layout creates too many speculative boundaries. Use the coarse initial topology in P1-AP-006 and split with evidence. |
| 17 | Formally model job/child cancellation, composite-resource admission, fragment checkpoint/durability, live refresh/continuity, FFmpeg lifecycle, JavaScript worker lifecycle, sink finalization, and archive commit. |
| 18 | Earliest fuzzing: M3U8 and MPD timeline expansion, output templates/path planning, format selectors, redirect/header/cookie parsing, resume journal, FFmpeg progress, JavaScript frames, and plugin-process payload limits. MSS follows with Phase 3 fixtures. |
| 19 | Yes. No Python runtime means Python plugins are ported to Rust plugin executables or remain outside canonical production. A legacy bridge cannot participate in parity/performance release claims. |
| 20 | Ship signed stable releases plus fast extractor/challenge data updates whose schema and compatibility range are independently versioned. Built-in Rust extractor logic still requires binary releases; do not pretend every breakage is data-only. |
| 21 | One enum is sufficient for internal dispatch; public APIs and serializable descriptors must be distinct for media, collections, and live sessions. |
| 22 | Evaluate source index/range, archive identity, date, media kind, creator/channel, tags, language, and known dimensions before asset resolution. Size, final codec/container, and fields requiring HEAD/manifest fetch occur later. Predicate planning must explain requests avoided. |
| 23 | SQLite is the correct local embedded default with a stable extractor-owned identity tuple, identity-schema version, variant/asset discriminator, and success-event transaction. Use one bounded writer actor and a fixed WAL/local-filesystem policy. |
| 24 | Keep byte fan-out in-process for speed, with per-sink byte credits and lossless recording priority. JS and FFmpeg stay behind worker/process boundaries. Host callback/player adapters may be isolated when their blocking behavior is not controllable. |
| 25 | MSS, custom index/time ranges, validation, and both record-then-mux and opt-in real-time mux are mandatory for the first complete protocol-engine release because the source design already includes all of them. Real-time mux remains explicitly less recoverable, never the default. |

## Recommended accepted decisions to add

- **Rust boundary:** all Ferric product/runtime code is Rust; production contains no Python or external JavaScript runtime. FFmpeg/ffprobe are the required supervised external dependency. Any additional native library/runtime requires evidence of necessity and explicit operator resolution before selection.
- **Runtime:** Tokio is the initial async contract; current-thread CLI and measured multi-thread worker profiles; one explicitly sized CPU pool; bounded actors around blocking filesystem/database work.
- **Admission:** composite resource vectors are atomically admitted by one broker; byte memory is a schedulable resource.
- **Transport:** standard Rust backend plus a separately pooled, corpus-proven fingerprint backend; no silent downgrade.
- **Plugins:** versioned process IPC with Rust plugin executables is canonical; plugins never carry media data-plane bytes. Wasm or foreign runtimes are not implicitly authorized.
- **JavaScript:** persistent Ferric-owned Rust worker with a corpus-proven pure-Rust engine, framed IPC, host-owned caches, recycling, and OS isolation; external runtimes are an unresolved operator decision only if the Rust path fails with evidence.
- **FFmpeg:** mandatory external dependency, typed operation plans, dedicated progress channel, tree kill plus reap, capability/ffprobe validation.
- **Storage:** same-filesystem work directory, explicit cross-filesystem commit path, SQLite local default through one bounded actor.
- **Performance truth:** equivalence gate precedes confidence-bound speed gate; total end-to-end cost and component-attributed cost are both published.

</topic>

<topic id="diff-attack-surfaces" status="final" version="0.2.0" summary="DIFF_ATTACK_SURFACES">

# DIFF_ATTACK_SURFACES

There is no code diff. The reviewed delta is the design against the fixed `review-charter.json` operator authority and against the behaviors of the pinned yt-dlp source. Attack surfaces derived before verdict:

1. **Claim-to-proof mismatch:** speed targets can be passed by incomparable process modes, artifacts, caches, or outputs.
2. **Producer/consumer mismatch:** HTTP and transforms allocate before the reorder buffer's stated bound takes effect.
3. **Scheduler mismatch:** nodes declare multiple resources, but separate limits do not define atomic admission or fairness.
4. **Durability mismatch:** fragment completion, contiguous write, OS flush, journal flush, rename, and archive commit are conflated.
5. **Transport mismatch:** a minimal request future cannot express wire-fingerprint or pool-partition requirements.
6. **Model/protocol mismatch:** an in-memory trait-object stream cannot cross JSON, plugin, cache, snapshot, or worker boundaries.
7. **Cancellation mismatch:** async task cancellation does not automatically stop blocking work or whole subprocess trees.
8. **Observability mismatch:** high-frequency events can consume the CPU/memory they are intended to measure.
9. **Platform mismatch:** rename, named pipe, process tree, SQLite WAL, and fsync behavior differ across the promised OS/filesystem matrix.
10. **Baseline mismatch:** yt-dlp already has generated lazy extractors and single-process multi-URL operation; comparisons must preserve those optimizations.

</topic>

<topic id="independent-checks-run" status="final" version="0.2.0" summary="INDEPENDENT_CHECKS_RUN">

# INDEPENDENT_CHECKS_RUN

1. Read the complete 2,932-line source design and the fixed peer-review charter; checked every architecture, performance, testing, acceptance, peer-question, decision-log, and appendix section.
2. Opened the exact pinned yt-dlp `2026.07.04` fragment downloader and verified the temporary-fragment/read/append/flush/journal/remove path and `ThreadPoolExecutor` concurrency ([source](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/yt_dlp/downloader/fragment.py)).
3. Opened the exact pinned yt-dlp README and verified multi-URL invocation, lazy-extractor generation, FFmpeg/EJS dependencies, and impersonation dependency ([source](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/README.md)).
4. Opened the exact pinned impersonation model and curl_cffi backend to verify target resolution and actual fingerprint backend behavior ([model](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/yt_dlp/networking/impersonate.py), [backend](https://github.com/yt-dlp/yt-dlp/blob/2026.07.04/yt_dlp/networking/_curlcffi.py)).
5. Checked current Tokio runtime, bounded-channel, semaphore, filesystem, blocking-task, and process contracts. This independently exposed the unbounded blocking queue, non-abortable started blocking work, FIFO `acquire_many` head-of-line behavior, and child reaping caveats ([runtime](https://docs.rs/tokio/latest/tokio/runtime/struct.Builder.html), [blocking](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html), [mpsc](https://docs.rs/tokio/latest/tokio/sync/mpsc/fn.channel.html), [process](https://docs.rs/tokio/latest/tokio/process/struct.Command.html)).
6. Checked current upstream Rust fingerprint-client evidence and its BoringSSL/build constraints ([wreq](https://github.com/0x676e67/wreq)).
7. Checked current yt-dlp EJS and Deno security documentation to prove the compatibility demand and to distinguish yt-dlp's recommendation from Ferric's fixed dependency authority ([EJS](https://github.com/yt-dlp/yt-dlp/wiki/EJS), [Deno](https://docs.deno.com/runtime/fundamentals/security/#executing-untrusted-code)).
8. Checked current FFmpeg streamcopy, progress, statistics-period, stdin, mapping, and codec-selection documentation ([FFmpeg](https://ffmpeg.org/ffmpeg.html)).
9. Checked SQLite's current WAL concurrency, network-filesystem restriction, checkpoint behavior, synchronous tradeoffs, and 2026 WAL-reset fix ([SQLite](https://www.sqlite.org/wal.html)).
10. Checked primary benchmark-method sources for warmups/cache preparation, repetitions, confidence intervals, and noise ([hyperfine](https://github.com/sharkdp/hyperfine), [Criterion](https://bheisler.github.io/criterion.rs/book/analysis.html)).
11. Checked a primary multi-resource scheduling reference rather than assuming separate semaphores imply fairness ([USENIX DRF](https://www.usenix.org/conference/nsdi11/dominant-resource-fairness-fair-allocation-multiple-resource-types)).
12. Checked current Wasmtime upstream and rejected it as an automatic canonical choice because it would add a production runtime not authorized by the fixed boundary ([Wasmtime](https://github.com/bytecodealliance/wasmtime)).
13. Checked current Boa project, embedding, conformance, experimental-status, and runtime-limit documentation before naming it as a Rust-only spike candidate, not a proven selection ([project](https://boajs.dev/), [introduction](https://boajs.dev/docs/intro), [runtime limits](https://docs.rs/boa_engine/latest/boa_engine/vm/struct.RuntimeLimits.html)).

No implementation tests or benchmarks were run because no Ferric implementation, fixtures, benchmark manifest, or executable acceptance surface exists. “Tests passed” is therefore **NOT APPLICABLE**, not evidence of readiness.

</topic>

<topic id="counterfactual-checks" status="final" version="0.2.0" summary="COUNTERFACTUAL_CHECKS">

# COUNTERFACTUAL_CHECKS

1. If the proposed end-to-end media-byte credit is removed or altered to a fragment-count-only limit, `HTTP body -> decrypt -> pack -> reorder -> sink` can allocate multiple fragment-sized copies and violate bounded memory before reorder backpressure occurs.
2. If the central atomic resource-vector grant is removed and nodes acquire `ResourceClass` permits independently, conflicting acquisition order can deadlock, and cancellation can strand partial admission.
3. If the benchmark manifest does not force yt-dlp's lazy extractors and one-process multi-URL mode, startup and 1,000-URL comparisons can report improvements created by a weaker baseline rather than Ferric code.
4. If the FFmpeg supervisor drops a Tokio `Child` without tree kill and reap, cancellation can leave FFmpeg descendants, open handles, and temporary outputs after the Ferric job reports stopped.
5. If `MediaCollection.entries: Box<dyn CollectionEntryStream>` remains inside the model, worker IPC, plugin-process protocols, deterministic snapshots, and cache serialization cannot reproduce the same source result.
6. If the Rust challenge-engine requirement is weakened to an unproven assumption, Ferric can ship a “fully Rust” build that fails the pinned YouTube EJS corpus; if it is silently replaced by Deno, Ferric instead violates the dependency authority. The only valid exit is corpus evidence or an explicit operator decision.
7. If the impersonation backend is selected from Rust API shape alone, a BoringSSL/native dependency can enter the build without authorization; if impersonation is dropped, affected declared extractors lose parity. Dependency and wire-compatibility gates are both required.
6. If SQLite WAL is used unchanged on a network filesystem, the default archive contradicts SQLite's documented same-host shared-memory requirement and loses a supported durability claim.

</topic>

<topic id="boundary-probes" status="final" version="0.2.0" summary="BOUNDARY_PROBES">

# BOUNDARY_PROBES

| Boundary | Probe | Result |
|---|---|---|
| yt-dlp baseline -> benchmark | Compared pinned CLI/build behavior with B-001/B-013 assumptions | **FAIL:** baseline artifact and single-process batch mode are unspecified. |
| HTTP producer -> reorder consumer | Traced byte ownership through fetch, decrypt, pack, reorder, writer, fan-out | **FAIL:** only completed reorder entries have explicit limits. |
| Scheduler request -> permits | Applied a two-node opposite-order composite-resource scenario | **FAIL:** no atomic acquisition/order contract. |
| Async cancellation -> blocking I/O | Compared design cancellation promise with Tokio `spawn_blocking`/filesystem semantics | **FAIL:** started blocking work is non-abortable and default queue is not backpressured. |
| Rust parent -> FFmpeg child tree | Compared process plan with FFmpeg progress/stdin and Tokio child-drop behavior | **FAIL:** framing, drain, stop, tree kill, and reap contracts are missing. |
| Rust transport -> fingerprint-sensitive server | Compared trait fields with pinned yt-dlp/wreq wire-profile requirements and the no-unapproved-native-runtime boundary | **FAIL:** trait cannot express required fingerprint/pool identity, and no authorized Rust-only implementation has passed a corpus. |
| Collection descriptor -> IPC/snapshot | Attempted to serialize/replay `Box<dyn CollectionEntryStream>` conceptually | **FAIL:** execution state is embedded in the model. |
| Archive actor -> SQLite/filesystem | Applied WAL on local and network filesystem and concurrent writer/checkpoint cases | **FAIL:** mode, topology, and filesystem policy are unspecified. |
| Logical order -> completion events | Compared deterministic filenames with overlapped execution | **PARTIAL:** indices exist, but default event order and bounded re-sequencing storage are unresolved. |
| Plugin -> media data plane | Applied large fragment transfer through versioned process IPC | **FAIL if allowed:** plugin contract must prohibit bulk media bytes and use host-managed network/sinks. |

</topic>

<topic id="negative-path-checks" status="final" version="0.2.0" summary="NEGATIVE_PATH_CHECKS">

# NEGATIVE_PATH_CHECKS

1. **Huge fragment without `Content-Length`:** current design may buffer before deciding to spill; no pre-admission byte proof exists.
2. **Dishonest small `Content-Length`:** body exceeds reserved bytes; no incremental credit/fail/spill rule exists.
3. **Out-of-order giant fragment:** count/distance bound passes while byte budget fails.
4. **Slow player plus recording:** policy names drop/disconnect/backpressure choices but no priority rule proves the recording remains continuous.
5. **Retry storm during cancellation:** per-layer budgets exist as prose, but retry tasks, timers, and permits have no ownership/state transition.
6. **Blocking disk/database call after cancellation:** Tokio cannot abort it after start; shutdown latency is not bounded by the current design.
7. **FFmpeg fills stderr:** no required concurrent drain protocol; possible deadlock.
8. **FFmpeg forks/starts descendants:** PID-only kill is insufficient; possible orphan process.
9. **FFmpeg exits zero but output is truncated/incompatible:** “validated” is undefined until ffprobe predicates exist.
10. **JavaScript response arrives after timeout/recycle:** no request-generation or stale-frame rejection rule.
11. **Rust JavaScript engine fails a pinned EJS challenge or exceeds limits:** no corpus-backed engine decision exists; external-runtime fallback is not authorized.
12. **JavaScript worker memory creep:** fixed memory limit is named, but recycling and cache ownership are absent.
13. **Impersonation profile unavailable:** no rule forbids silent fallback to the standard transport or introduction of an unapproved native backend.
14. **SQLite writer/checkpoint collision:** no `SQLITE_BUSY`/retry/serialization policy.
15. **SQLite WAL on network share:** upstream says unsupported; current default has no guard.
16. **Cross-volume final output:** rename cannot provide the promised atomic commit.
17. **Event consumer stops reading:** no per-consumer drop/coalesce/disconnect rule; event producer may backpressure critical work.
18. **Plugin returns oversized metadata or attempts media-byte relay:** size limit is aspirational and data-plane prohibition is missing.
19. **Benchmark output differs but is faster:** current percentage table can still pass several rows because equivalence is not universally a prerequisite.

</topic>

<topic id="independent-findings" status="final" version="0.2.0" summary="INDEPENDENT_FINDINGS">

# INDEPENDENT_FINDINGS

## Separate verdicts

- **Tests:** NOT APPLICABLE. There is no Ferric implementation or executable benchmark/compatibility corpus.
- **Requirement/spec alignment:** **FAIL.** Rust/no-Python/FFmpeg/speed/functionality direction aligns, but the spec cannot yet prove bounded execution or speed improvement.
- **Architecture quality:** **FAIL for implementation readiness; strong as a direction document.** Critical interfaces are illustrative and several choices remain mutually exclusive.
- **Performance evidence:** **FAIL.** No Phase 0 baseline or benchmark manifest exists, and current targets permit unfair comparisons.
- **Environment confidence:** **High** for the exact document and primary-source contracts inspected; **low** for actual Ferric performance because no runtime artifact exists.
- **Final validator decision:** **FAIL.** Begin Phase 0 and targeted spikes only. Do not authorize parallel feature implementation against the current contracts.

## Minimum proof gate to upgrade the verdict

1. Accept the fixed Rust/no-Python/FFmpeg operator decisions in the design log.
2. Accept the runtime, composite admission, transport-tier, JavaScript-worker, FFmpeg-supervisor, plugin, and SQLite/filesystem decisions above.
3. Publish a normative benchmark manifest and run the pinned yt-dlp baseline on the declared machines.
4. Publish serializable model/protocol schemas and state machines for the critical paths.
5. Implement one vertical slice: direct download plus one HLS fixture, bounded byte credits, cancellation, resume, FFmpeg stream-copy merge, atomic finalization, and differential output proof.
6. Run the adversarial negative paths and produce paired performance results with confidence bounds.

Only that vertical slice should establish implementation patterns for the expanded collection/live/MSS/plugin corpus; it must not be used to remove or defer those accepted capabilities from the complete product.

</topic>

<topic id="residual-uncertainty" status="final" version="0.2.0" summary="RESIDUAL_UNCERTAINTY">

# RESIDUAL_UNCERTAINTY

- No measured yt-dlp baseline was available, so the numerical improvement targets cannot be confirmed or rejected; this review rejects their current proof contract, not the possibility of achieving them.
- No Ferric code, Cargo dependency graph, fixtures, or build exists, so runtime choice, binary startup, allocation counts, compiler optimization, and actual cross-platform behavior remain unmeasured.
- wreq is useful only as research evidence for fingerprint controls: its release-candidate/native-BoringSSL characteristics conflict with the current dependency authority. No authorized Rust-only impersonation backend has yet been proven against the compatibility corpus.
- Boa provides a credible Rust-owned engine spike, not a readiness conclusion. Its documented experimental status and incomplete Test262 conformance leave pinned EJS compatibility, performance, host-API coverage, memory containment, and cross-platform behavior unproven.
- Deno and Wasmtime are researched alternatives, not accepted production architecture. Either would add a runtime beyond the fixed authority and therefore requires measured necessity plus explicit operator resolution.
- FFmpeg behavior varies by build/configuration. A version string alone is insufficient; capability and output-equivalence probes remain necessary.
- Filesystem durability semantics vary by OS, filesystem, mount, and storage. “fsync succeeded” is not a universal power-loss guarantee; the supported matrix must state what was actually tested.
- Site behavior and challenge code evolve after the pinned baseline. The native Ferric semantic specification and separately versioned extractor/challenge updates are necessary to prevent “latest” drift from invalidating reproducibility.

These uncertainties are not acceptable for an implementation-ready verdict, but they are bounded enough to execute Phase 0 research, benchmark construction, and architecture spikes.

</topic>
