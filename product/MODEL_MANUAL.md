---
file_id: FF-PRODUCT-MODEL-MANUAL-001
file_kind: model_manual
updated_at: "2026-10-21"
---

<topic id="phase-1-streaming-bounds" status="active" version="1" wp="WP-FF-021-first-runnable-production-slice-v1" updated_at="2026-10-21">

## Direct-fetch streaming bounds

The Phase 1 direct-fetch path reserves 2 MiB of logical payload memory before
adapter effects. It retains at most one accepted dependency delivery frame
(1 MiB guard), drains it through borrowed writes of at most 16 KiB, and checks
the total admitted body bound before writing that frame. Cancellation is checked
between writes; the next frame is requested only after the current frame drains.
The frame guard runs after reqwest allocates its frame. These reservations and
guards are not limits on dependency preallocation, allocator overhead or OS RSS.

</topic>

<topic id="phase-0-purpose" status="active" version="4" wp="WP-FF-006-rust-youtube-challenge-spike-v1" updated_at="2026-07-26">

# Ferric Forager model manual

Ferric Forager is an independent Rust-native replacement for yt-dlp with additional acquisition, recording, extraction, metadata, archive, protocol, diagnostics, and integration capabilities. Ferric owns every implementation path. Python, yt-dlp, and yt-dlp companion packages or executable assets are not permitted dependencies of product code, builds, tests, canonical verification, packages, releases, or runtime behavior.

The repository currently contains shipped data-only contract crates and pure deterministic core models, but no executable Ferric runtime capability. These Phase 0 prerequisite artifacts and the executable build-and-proof tooling MUST NOT be counted as product capability progress, a completed product phase, packaging, release, or runtime completion.

Repository ownership is deterministic:

- `.GOV/` owns governance, work packets, task state, design, and validation authority.
- `product/` owns shipped runtime code, assets, the independent watcher, this manual, and tests local to a product package.
- `build/` owns the Cargo workspace and lockfile, build policies, non-shipped tooling, shared and cross-package test infrastructure, fixtures, reports, and `target` output.
- `rust-toolchain.toml` at repository root is the sole rustup selector.

Shipped product runtime must not read or require `.GOV/` or `build/`. Build tooling may read the active governance packet to validate proof.

</topic>

<topic id="wp-ff-010-ffmpeg-supervision-prerequisite" status="active" version="1" wp="WP-FF-010-ffmpeg-supervision-spike-v1" ingestable="true" updated_at="2026-08-10">

## Run the FFmpeg supervision prerequisite proof

WP-FF-010 exercises a non-shipped Ferric-owned adapter for one bounded operation: copy exact AAC ADTS and H264 Annex-B streams into a job-scoped MP4 file with explicit maps and `-c copy`, machine progress on stdout, bounded diagnostics on stderr, direct-child reap, platform-scoped cancellation, and bounded ffprobe packet validation. It is prerequisite evidence only. It is not a Ferric runtime entrypoint, post-processing product, general codec/container promise, packaging result, release, phase completion, or product progress.

### Inputs and outputs

The shared fixture root must remain under `.fforager-artifacts/` and contain:

- `ffmpeg-supervision-v1.0.json`, copied byte-for-byte from `product/crates/fforager-contracts/testdata/ffmpeg-supervision-v1.0.json`;
- `input/audio.aac`, an elementary AAC ADTS file;
- `input/video.h264`, an elementary H264 Annex-B file;
- an initially available `output/` directory for `merged.mp4` and bounded probe scratch.

Run the repository-owned `ffmpeg-supervision-produce` wrapper rather than invoking the ignored producer test directly. The wrapper independently requires a clean committed source before and after execution; discovers and records the native Cargo, FFmpeg, ffprobe, fake-child, `setsid`, namespace, timeout, identity, and host tools; builds the fake child in an isolated artifact-root Cargo home and target; synthesizes the producer environment; and writes a strict report plus producer receipt. Caller-authored tool paths, Git identity, dirty-state claims, process environment, command text, and working directories are not accepted as proof inputs. Do not use a Windows `.exe` through WSL as Linux evidence.

Each platform writes one strict `ff.ffmpeg-platform-proof@2` JSON report and one `ff.ffmpeg-producer-receipt@1` receipt under `.fforager-artifacts/runtime-proof/`. The report contains platform-specific request and tool identities plus common operation, fixture, and limit bindings; raw canonical request/plan/limits/lifecycle/wait/output evidence; the versioned bound-runtime projection and exact platform argument vector; complete bounded progress and diagnostic evidence; independently measured force, direct-reap, and scope-zero settlement phases; and typed containment observations. The aggregator independently rereads clean Git and the canonical fixture, validates both producer receipts, reconstructs the Windows pathname vector or Linux descriptor vector, recomputes all exposed digests and behavioral decisions, requires one Windows and one Linux-native row, and writes an `ff.ffmpeg-cross-platform-proof@1` receipt under `.fforager-artifacts/runtime-proof/ffmpeg-supervision/`.

### Prepare one shared deterministic fixture on Windows

Run from the linked worktree root. Set `$ffmpeg` and `$ffprobe` to the trusted absolute executables selected for the proof; do not rely on caller-authored tool identities inside the request.

```powershell
$env:CARGO_BUILD_JOBS = "1"
$env:CARGO_TARGET_DIR = ".fforager-artifacts/cargo-target"
$proofRoot = Join-Path (Resolve-Path ".fforager-artifacts").Path "wp010-final"
$fixtureRoot = Join-Path $proofRoot "shared-fixture"
New-Item -ItemType Directory -Force (Join-Path $fixtureRoot "input"), (Join-Path $fixtureRoot "output") | Out-Null
Copy-Item -LiteralPath "product/crates/fforager-contracts/testdata/ffmpeg-supervision-v1.0.json" -Destination (Join-Path $fixtureRoot "ffmpeg-supervision-v1.0.json") -Force
& $ffmpeg -hide_banner -loglevel error -y -f lavfi -i "sine=frequency=1000:sample_rate=48000:duration=2" -c:a aac -b:a 128k -f adts (Join-Path $fixtureRoot "input/audio.aac")
& $ffmpeg -hide_banner -loglevel error -y -f lavfi -i "testsrc=size=320x240:rate=25:duration=2" -c:v libx264 -pix_fmt yuv420p -preset medium -f h264 (Join-Path $fixtureRoot "input/video.h264")
```

Both platform producers must consume these same fixture bytes. Record their SHA-256 values before either run and reject any mismatch.

### Produce the Windows report

```powershell
if (git status --porcelain) { throw "WP-010 final reports require a clean source" }
cargo run --manifest-path build/Cargo.toml --locked --jobs 1 -p fforager-xtask -- ffmpeg-supervision-produce --platform windows_x86_64 --fixture-root ".fforager-artifacts/wp010-final/shared-fixture" --report ".fforager-artifacts/runtime-proof/wp010-windows.json" --receipt ".fforager-artifacts/runtime-proof/wp010-windows-receipt.json"
```

The supported quiet Windows profile uses `CREATE_NO_WINDOW`, `-nostdin`, pre-execution Job assignment, `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, bounded whole-Job forced termination, cached direct-child wait, and an independent active-process query. General graceful stop is unsupported: this profile has neither the shared console needed for CTRL_BREAK nor the stdin `q` route. Windows numeric exit status does not prove exception provenance. Without a broker, the suspended-orphan interval remains explicit residual uncertainty.

### Produce the Linux-native report

Run the Linux-native producer from the clean Windows worktree through the same closed wrapper. It enters the configured WSL2 Ubuntu host, uses Linux-native `/usr/bin/ffmpeg` and `/usr/bin/ffprobe`, isolates the build in a PID namespace with an earlier inner timeout than the outer Windows wrapper, and reuses the exact Windows-created fixture bytes.

```powershell
if (git status --porcelain) { throw "WP-010 final reports require a clean source" }
cargo run --manifest-path build/Cargo.toml --locked --jobs 1 -p fforager-xtask -- ffmpeg-supervision-produce --platform linux_x86_64 --fixture-root ".fforager-artifacts/wp010-final/shared-fixture" --report ".fforager-artifacts/runtime-proof/wp010-linux.json" --receipt ".fforager-artifacts/runtime-proof/wp010-linux-receipt.json"
```

Linux opens every input with `O_NOFOLLOW`, precreates the output with `openat`, `O_EXCL`, and `O_NOFOLLOW` on the retained output-directory descriptor, and passes only fixed audited descriptors to FFmpeg. The bound vector uses `-fd 64..95` for inputs, `-fd 96` for output, `fd:`, the internal `fd,pipe` protocol inventory, and `-y` only because the exact output descriptor is already exclusively precreated. The logical request remains the version-one pathname and `-n` contract; `ff.ffmpeg-bound-runtime-invocation-canonical-json@1` binds the platform transformation. FFprobe receives the exact retained media descriptor rather than reopening a pathname.

Linux also creates a dedicated process group, sends bounded TERM then KILL to that group, reaps only the direct child with a typed result, and independently checks declared-group absence. The mandatory `setsid` fixture demonstrates that a descendant can escape this signal scope. Process groups are not hostile containment, and direct-child `waitpid` never proves arbitrary descendant absence. If an escaped descendant retains stdout or stderr, cancellable nonblocking readers return a bounded failure and the adapter permanently withholds the affected capacity rather than deadlocking or admitting another job over an unproven scope.

### Aggregate and validate

Return to the clean linked worktree on Windows and run:

```powershell
$env:CARGO_TARGET_DIR = ".fforager-artifacts/cargo-target"
cargo run --manifest-path build/Cargo.toml --locked --jobs 1 -p fforager-xtask -- ffmpeg-supervision-aggregate --windows-report ".fforager-artifacts/runtime-proof/wp010-windows.json" --windows-receipt ".fforager-artifacts/runtime-proof/wp010-windows-receipt.json" --linux-report ".fforager-artifacts/runtime-proof/wp010-linux.json" --linux-receipt ".fforager-artifacts/runtime-proof/wp010-linux-receipt.json"
```

The consumer rejects a missing platform; dirty, stale, or mismatched source; tool/request/operation/fixture/limit drift; forged raw evidence; a producer digest used as its own oracle; false Windows active-zero; missing direct wait; progress or diagnostic limit violation; declaration-preserving lifecycle or containment mutation; a leaked Windows handle sentinel; or a Linux report without the executed `setsid` escape and residual.

### Safety limits, diagnostics, failures, and recovery

- All disposable targets, fixtures, reports, and scratch remain below `.fforager-artifacts/`. Never redirect them to an external temp root.
- The public adapter requires both the request and its privately constructed `FfmpegValidatedInvocationV1`, then independently recomputes their correlation before any admission or process creation. A raw request, stale token, shell, arbitrary command text, ambient working directory, or caller-authored `SYSTEMROOT`, `TEMP`, `TMP`, or `TMPDIR` cannot reach execution.
- Stdout is required framed progress. Stderr is separately bounded diagnostics with explicit tail loss. A missing terminal progress frame, malformed/reordered/truncated record, cadence timeout, consumer stall, allocation/item/byte/parser ceiling, or drain failure is a typed failure.
- Nonzero exit, signal termination, wait failure, containment not proven, stale/substituted/read-only/partial/wrong-stream output, malformed ffprobe JSON, packet mismatch, or output-size violation cannot become success. A reaped nonzero exit is not retried as a reap failure.
- On failure, preserve bounded diagnostics; terminate the owned Job or process group when needed; reap the direct child; prove the declared scope empty where possible; join required drains; make the output decision; then release `FfmpegPipe` byte credits and the atomic resource lease. Cancellation is sampled before and after physical release while the release effect is still pending. Never release early or loop cleanup without a bound.
- Exact descriptor/handle identity prevents pathname substitution, but a hostile process that already owns a writable handle to trusted media can still mutate the same object. Trusted media roots must exclude that condition; the platform report retains it as residual uncertainty.
- Capability mismatch: verify the exact native executable path, content hash, normalized version/help output, host OS, and architecture. Do not relabel an unsupported tool as passing.
- Source or fixture mismatch: discard both platform reports, restore one clean commit and one shared fixture, and rerun both producers. Do not edit report JSON.
- Containment or `setsid` failure: inspect the fake-child handshake and platform boundary. Retain the documented residual; do not reinterpret process disappearance or direct-child exit as descendant proof.
- Aggregator failure: use its named field and raw-evidence diagnostic. Repair the producer or boundary and regenerate both reports; a stored or hand-authored receipt is not evidence.

Evidence ceiling: even a valid cross-platform report proves only this pinned prerequisite profile on the named hosts and tools. A later shipped Ferric consumer must re-prove the exact staged production artifact through `FF-GATE-RUNTIME-001` before any product, capability, runtime, packaging, release, or phase claim is legal.

</topic>

<topic id="phase-0-resource-durability-models" status="active" version="2" wp="WP-FF-009-resource-durability-models-v1" ingestable="true" updated_at="2026-08-02">

## Run the resource, durability, filesystem, and recovery models

WP-FF-009 exercises bounded deterministic prerequisite models for atomic resource admission, FIFO waiter ordering, public owned grant/waiter/credit lifetimes, all nine byte-credit stages and per-owner limits, simultaneous input/output reservations, component attribution and transfer, effect-correlated durability prefixes, strict journal semantics and job identity, serialized commit effects, executed recovery-action retry/convergence, and exact Windows NTFS and WSL2 v9fs filesystem profiles. It does not use Python or yt-dlp.

Run from the repository root with one Cargo job and repository-contained disposable output:

```powershell
$env:CARGO_BUILD_JOBS = "1"
$env:CARGO_TARGET_DIR = ".fforager-artifacts/cargo-target"
cargo run --manifest-path build/Cargo.toml --locked --jobs 1 -p fforager-xtask -- resource-durability-models
cargo run --manifest-path build/Cargo.toml --locked --jobs 1 -p fforager-xtask -- verify-deep --evidence-from-taskboard
```

The exact input manifest is `build/fixtures/resource-durability-models/manifest.json`. Its source manifest names the public contract/core implementations plus the independent testkit producer and xtask consumer. The standalone command compiles and executes the exact public-boundary corpus test; its durability-effect row calls the foreign-broker acknowledgement boundary itself and records the typed `AcknowledgementBrokerMismatch` result. The command independently reloads and validates all 48 case rows against the current manifest and source hashes, proves that source state did not change during execution, and atomically writes a fresh `ff.resource-durability-model-report@1` JSON file under `build/reports/`. The in-memory typed execution receipt binds the caller-created invocation and freshness boundary to the fresh report path, persisted digest, manifest identity, source identity, and executed-case count; `verify-deep` and `verify-pr` reject an absent, stale, mismatched, fabricated, or comment-only receipt path and include the validated fresh report path in their artifacts.

Every row exposes the exact case and injected fault, invariant and state, full 13-dimensional resource vector, byte-credit owner, current queue occupancy and its item/byte bounds, received/validated-written/durable/resume prefixes, recovery action, compiled boundary, expected and observed result, canonical proof class, proof mechanism, exact cancel attribution when applicable, supplemental wrong-broker execution when applicable, and residual uncertainty. Inspect the manifest case first, then the row with the same `case_id`, then the named public boundary in `product/crates/fforager-contracts/src/resource.rs`, `product/crates/fforager-contracts/src/storage.rs`, `product/crates/fforager-core/src/resource.rs`, or `product/crates/fforager-core/src/lifecycle.rs`.

The two platform rows additionally execute bounded, quiet, read-only probes. The Windows row runs `reg.exe query` for `ProductName`, `EditionID`, and `CurrentBuildNumber`, then `wmic.exe logicaldisk ... get FileSystem /value` for the repository volume; it accepts only derived Windows 11 Home, build 26200, and NTFS. The WSL row runs `wsl.exe --exec cat /etc/os-release`, `uname -r`, `wslpath -a`, and `stat -f -c %T`; it accepts only Ubuntu 24.04, the frozen WSL2 kernel prefix, and repository mount type `v9fs`, which remains an explicitly rejected/degraded profile. Command invocations, parsed observed fields, verdicts, and residual uncertainty are mandatory report data. Missing commands or any identity mismatch fail closed. These rows prove live host/volume or host/mount identity only: no Rust model runs inside WSL, and neither row proves native-Linux behavior, atomic replacement, confinement races, crash behavior, power-loss behavior, or durability behavior.

Failure and recovery:

- `WP009-E-GATE-UNTRIGGERED`, missing exact test registration, or missing report prefix: restore the exact compiled test and deep-gate call; a declaration or stored report is not execution evidence.
- `WP009-E-UNMAPPED-INPUT`, manifest/source drift, or report identity mismatch: restore the exact manifest-to-row and source-path mapping, or make an authorized corpus change in the producer and independent consumer together.
- Missing proof fields or queue bounds: restore the complete row; do not infer limits from prose or replace current occupancy with a configured maximum.
- Durability outrun, wrong-stage authorization, ordinary byte-history replay/restoration, or recovery retry failure: repair the public model so `resume_at <= durable_contiguous <= validated_written_contiguous <= received`, only cumulative `HttpReceive` consumption authorizes received progress, only cumulative `Writer` consumption authorizes validated-written progress, every byte effect uses a strict positive broker-coupled receipt, ordinary replay rejects byte histories, byte restoration consumes matching authoritative broker state, and each recovery action is repeat-safe with a checked next decision; rerun the counterfactual tests.
- Filesystem profile mismatch: select the exact frozen host profile. WSL2 v9fs intentionally fails closed for security-sensitive confinement and is not native-Linux durability proof.
- Source-changed or report-write failure: stop using the result, return source state to the intended revision, ensure `build/reports/` is writable, and rerun. A partial or stale report is not accepted.

Evidence ceiling: this corpus executes pure product contract/core models through non-shipped proof tooling. It performs no live crash or power-loss experiment, no native-Linux filesystem proof, no shipped storage/archive adapter, and no Ferric product entrypoint. Its result is prerequisite evidence with `zero_product_progress=true`, never product capability, runtime, packaging, release, or phase completion.

</topic>

<topic id="phase-0-archive-store-evidence" status="active" version="1" wp="WP-FF-008-archive-store-evidence-v2" ingestable="true" updated_at="2026-08-02">

## Run the archive-store evidence corpus

WP-FF-008 exercises Ferric's candidate-neutral archive boundary against exact-pinned pure-Rust `redb 4.1.0`. It keeps item, representation, track, asset, and derived-output identities distinct and executes atomic claims, renewable leases, stale takeover, successful-output insertion, membership, reconciliation, schema migration, mapped text import, corruption refusal, retries, and representative-scale measurements. Every durable write explicitly uses immediate durability, two-phase commit, and quick repair. No Python, yt-dlp, external executable, network access, or native database is part of this workflow.

Run the bounded default corpus from the repository root:

```powershell
$env:CARGO_BUILD_JOBS = "1"
$env:CARGO_TARGET_DIR = ".fforager-artifacts/cargo-target"
cargo run --manifest-path build/Cargo.toml --locked --jobs 1 -p fforager-xtask -- archive-store-evidence
```

The exact committed input is `build/fixtures/archive-store-evidence/manifest.json`. The command compiles and runs `tests::archive_store_evidence_corpus_executes_public_boundary`, independently validates all 24 report rows and their counterfactuals, binds the result to current manifest/source fingerprints, and atomically writes `build/reports/wp-ff-008-archive-store-evidence-*.json`. The default run measures 4,096 streamed identities. The one-million-identity B-021 row is recorded as `BLOCKED`, never as a pass or measurement, unless the operator deliberately provides the heavier stress budget:

```powershell
$env:FFORAGER_RUN_B021 = "1"
cargo run --manifest-path build/Cargo.toml --locked --jobs 1 -p fforager-xtask -- archive-store-evidence
Remove-Item Env:FFORAGER_RUN_B021
```

Inspect a failure in this order: the matching `case_id` in the manifest, the report row with that exact ID, `build/crates/fforager-testkit/src/archive_evidence.rs`, the candidate-neutral DTO or decision in `product/crates/fforager-contracts/src/archive.rs`, and the redb adapter in `product/crates/fforager-storage/src/lib.rs`. All disposable databases and test state must remain below `.fforager-artifacts/test-runs/`; a path escaping that root is a corpus defect.

Failure and recovery:

- `WP008-E-GATE-UNTRIGGERED`, missing report prefix, stale receipt, or source/manifest drift: do not use a stored report; restore the exact compiled test and source mapping, then rerun the command.
- Claim, lease, commit, import, or reconciliation mismatch: repair the candidate-neutral boundary or adapter and add the exact public counterexample. Do not weaken the expected row or classify a semantic failure as unsupported.
- `OpenFailed` for corrupt or truncated input is the required typed fail-closed result. A panic escaping `ArchiveStore::open`, a successful open of invalid bytes, or invented archive success is a defect.
- Migration interruption must preserve a resumable checkpoint and last-known-good source. Unknown/newer schema versions and unknown import mappings must remain typed refusals.
- A `BLOCKED` B-021 row means no million-identity latency, RSS, storage-size, recovery, or write-amplification result exists. Do not infer one from the representative row.
- Report-write or artifact-confinement failure invalidates the invocation. Restore repository-local artifact access and rerun; never redirect disposable state to an external temp directory.

Evidence ceiling: this is Phase 0 prerequisite evidence for a storage candidate. It does not execute a shipped Ferric entrypoint, physical power-loss test, or complete production archive subsystem and therefore provides zero product, capability, runtime, packaging, release, or phase progress.

</topic>

<topic id="phase-0-compatibility-oracle" status="active" version="5" wp="WP-FF-004-compatibility-inventory-corpus-v1" updated_at="2026-07-26">

## Validate Ferric compatibility data and optionally capture an external comparison

Ferric-owned committed fixtures are the canonical compatibility inputs. Validation, replay, PR checks, deep checks, builds, tests, packaging, and releases must run with yt-dlp absent.

WP-FF-004 also defines an explicitly invoked research-only capture command for observing the official `yt-dlp 2026.07.04` Windows executable and matching source tag. That command is optional, non-canonical, and never called by Ferric build, test, verification, package, release, or runtime paths. It may only convert separately acquired external observations into inert, provenance-bound comparison data; it does not supply implementation code.

Phase 0 product source must not embed `README` or `docs` assets with `include!`, `include_bytes!`, or `include_str!`, and it must not construct a process through `Command::new`; those paths would turn research or documentation into an ungoverned runtime dependency. Product-local Clippy configuration forbids compiler-resolved aliases of `std::process::Command`, `Command::new`, `include_bytes!`, and `include_str!`; the architecture scanner separately forbids `include!` and local suppression of those guards. A future shipped runtime may introduce a typed `ExternalProgram` boundary that allowlists only `ffmpeg` and `ffprobe`, validates fixed arguments and executable identity, and is proven through the staged production artifact. It must not permit `yt-dlp`, Python, shell composition, or dynamically assembled program names.

To refresh external comparison data deliberately, set repository-relative paths to separately acquired, hash-matching inputs and run only the capture command:

```powershell
$oracleExe = ".fforager-artifacts/cargo-target/wp4-research/oracle/yt-dlp.exe"
$sourceRoot = ".fforager-artifacts/cargo-target/wp4-research/yt-dlp-2026.07.04"
cargo run --manifest-path build/Cargo.toml --locked -p fforager-xtask -- compatibility-generate --oracle-exe $oracleExe --source-root $sourceRoot
```

The normal dependency-free validation path is:

```powershell
cargo run --manifest-path build/Cargo.toml --locked -p fforager-xtask -- compatibility-validate
cargo run --manifest-path build/Cargo.toml --locked -p fforager-xtask -- compatibility-replay
```

Generation verifies the immutable release identity, executable SHA-256, source Git commit, and selected source-file hashes before writing `build/fixtures/compatibility/yt-dlp-2026.07.04/profile.json` atomically. The generated profile contains stable option, alias, preset, interaction, extractor, description, and URL-class rows. Line endings, CP-1252 executable output, identical extractor duplicates, and upstream-randomized search examples have explicit deterministic normalization rules.

Validation reads the committed oracle manifest, generated profile, seven-plane corpus, opt-in live manifest, and the exact required negative-fixture inventory. It binds each versioned manifest/profile to its canonical content digest, checks stable IDs, counts, coverage, shard assignments, normalization versions, offline/network separation, allowlisted secret placeholders, and pinned provenance. Case fixture digests normalize CRLF/CR to LF before hashing so clean Git checkouts remain portable. JSON inputs are bounded to 16 MiB. A successful run emits a unique `ff.compatibility-report@1` JSON report under `build/reports/`.

Offline replay never opens the network. Run all cases or a zero-based deterministic shard. A shard that selects zero cases fails with `FF-COMP-E-SHARD-EMPTY`; it is not replay evidence. A non-empty shard report is explicitly `selected_shard_only` and never claims complete-corpus semantic replay:

```powershell
cargo run --manifest-path build/Cargo.toml --locked -p fforager-xtask -- compatibility-replay --shard 0/4
```

The mandatory planes are source graph, normalized observation, sanitized network transcript, filesystem/process artifact, failure/timeout, archive duplicate handling, and configuration migration. Fixtures replace authorization, cookies, query tokens, clocks, random seeds, and machine-local state with allowlisted placeholders before commit.

Replay executes deterministic native Rust semantics for every selected case: it records the concrete fixture input, exercised boundary, expected outcome, and observed outcome. It reports `SEMANTIC_REPLAY_EXECUTED`, never a behavior `PASS` inferred only from schema, digest, or expected-outcome text. Structural checks remain structural; report aggregation retains the weakest executed proof class and does not claim shipped Ferric parity, runtime completion, or a yt-dlp/Python dependency.

</topic>

<topic id="phase-0-compatibility-comparison" status="active" version="1" wp="WP-FF-004-compatibility-inventory-corpus-v1" ingestable="true" updated_at="2026-07-19">

## Compare candidates, inspect drift, and run live canaries

A candidate results JSON file uses `ff.compatibility-candidate-results@1`, names the exact corpus and profile IDs, and supplies stable case IDs plus SHA-256 observation digests. Compare it with:

```powershell
cargo run --manifest-path build/Cargo.toml --locked -p fforager-xtask -- compatibility-diff --candidate build/fixtures/compatibility/candidate-empty-v1.json
```

Every corpus case receives a row. The command first validates the canonical profile, corpus, and case fixtures, and the report records the candidate path and SHA-256. Equal digests are `equivalent`; omitted cases are `missing_feature`; unequal observations must be `ferric_defect`, `accepted_baseline_correction`, `nondeterministic_response`, or `accepted_divergence`. Accepted corrections/divergences require an explicitly authorized stable decision ID, and deterministic offline cases cannot be relabeled nondeterministic. Report completeness proves that nothing was silently omitted; it does not prove Ferric parity.

Compare two generated inventories by stable ID:

```powershell
cargo run --manifest-path build/Cargo.toml --locked -p fforager-xtask -- compatibility-inventory-diff --before build/fixtures/compatibility/yt-dlp-2026.07.04/profile.json --after build/fixtures/compatibility/yt-dlp-2026.07.04/profile.json
```

Inventory-diff reports use `ff.compatibility-inventory-diff@2` and cover option, preset, interaction, extractor, and extractor-description additions, removals, and changes. Reusing one profile ID for changed content fails closed.

Live canaries are a mechanically separate, credential-free, nondeterministic observation suite with exact public destination allowlists. The command refuses to run unless the operator supplies both the pinned executable and the explicit opt-in flag:

```powershell
cargo run --manifest-path build/Cargo.toml --locked -p fforager-xtask -- compatibility-live-canaries --enable-live --oracle-exe $oracleExe
```

The live report status and every canary row are `OBSERVED`, and the command always states `deterministic_proof=false`. Live results never replace offline acceptance evidence and anti-bot, site drift, rate limits, or network failures are observations rather than deterministic regressions.

Recovery follows the stable diagnostic:

- `FF-COMP-E-UNPINNED-ORACLE`: reacquire the exact official artifact/source inputs; never edit hashes to fit an unverified local file.
- `FF-COMP-E-PARSE` or deterministic regeneration mismatch: preserve both outputs, inspect the pinned source and normalization rule, then fix and rerun the generator twice.
- `FF-COMP-E-UNSANITIZED-SECRET`: replace the named secret or machine-local value with an allowlisted `{{PLACEHOLDER}}`, recompute the fixture hash, and rerun validation.
- `FF-COMP-E-COVERAGE`, `FF-COMP-E-SHARD`, or `FF-COMP-E-NORMALIZATION`: repair the canonical manifest/case mapping rather than bypassing the validator.
- Profile/corpus/live integrity or unsafe canonical-path errors: restore the versioned committed artifact or use a repository-relative file physically contained under `build/fixtures/compatibility/` or `.fforager-artifacts/cargo-target/`; do not reuse an ID for changed content or route through a link outside those roots.
- Candidate identity, digest, classification, or decision errors: repair the candidate file; do not remove missing rows from the emitted report.
- Report-write failure: verify `build/reports/` is writable. Atomic writes do not accept a partial final JSON report.

</topic>

<topic id="phase-0-commands" status="active" version="3" wp="WP-FF-022-prerequisite-classifier-remediation-v1" ingestable="true" updated_at="2026-10-20">

## Start and run

Run commands from the repository root. Do not infer state from chat history; begin at `START_HERE.yaml`, resolve the active packet from `.GOV/taskboard/taskboard.yaml`, and follow its cited authority.

```powershell
cargo run --manifest-path build/Cargo.toml --locked -p fforager-xtask -- architecture-check
cargo run --manifest-path build/Cargo.toml --locked -p fforager-xtask -- runtime-truth-check --evidence-from-taskboard
cargo run --manifest-path build/Cargo.toml --locked -p fforager-xtask -- verify-deep --evidence-from-taskboard
cargo run --manifest-path build/Cargo.toml --locked -p fforager-xtask -- verify-pr --evidence-from-taskboard
```

`architecture-check` consumes `build/architecture-policy.toml`, `build/tooling-policy.toml`, `build/rule-to-proof.toml`, locked Cargo metadata, parsed canonical YAML authority, governed source paths, and `build/fixtures/architecture/`. Proof-integrity fixtures mutate isolated product trees, serialized report evidence, and compiled isolated testkit workspaces, then execute the same product scanner, report validator, or exact public test boundary used by the gate. Legacy architecture fixtures remain structural policy checks. It emits a unique versioned JSON report under `build/reports/` and exits nonzero on mismatch.

`runtime-truth-check` compares the active packet base SHA with current changed paths. An ordinary governance/build-tool-only packet declares `scope.product_impact` as `NONE`; its PASS proves only that no product claim is legal. A design-authorized Phase 0 build-only prerequisite instead declares `PREREQUISITE`, freezes its exact declaration and allowed paths at activation, and changes an actual declared build artifact. Its PASS is policy/contract proof only, requires a future shipped consumer and runtime re-proof, and claims zero product, capability, runtime, packaging, release, or phase progress. A product-affecting packet must declare `RUNTIME`, supply strict `ff.runtime-proof@1` evidence, and have a declared shipped member. The gate then builds the locked release profile, hashes and stages the exact binary, copies hash-bound inputs into a clean package directory, launches the staged binary as an external process, verifies success and negative scenarios, and removes a required observable to prove the same oracle rejects the counterfactual. Missing runtime proof is `FAIL`/`BLOCKED`, never `PASS` or `NOT_APPLICABLE`.

Supporting unit, fixture, replay, fuzz, property, and mock-based tests remain useful, but `cfg(test)`, dev-dependencies, testkit, mock/fake/stub adapters, in-memory substitutes, hardcoded success, and direct internal calls cannot satisfy `ff.runtime-proof@1`. A product packet requires at least one success and one negative scenario. Exit status alone is not an observable; require stdout, stderr, or a bounded output file with optional SHA-256.

`verify-deep` runs the pinned workspace across formatting, compile profiles, Clippy, all tests, the machine-readable contract inventory, the data-only model scan, the full compatibility replay, the empty-shard negative boundary, and the architecture gate. Its report separates declared proof support from evidence actually executed and retains the prerequisite-only proof ceiling.

`verify-pr` also validates active packet change evidence and runs tool preflight, formatting, compile profiles, Clippy, tests, docs, dependency policy, architecture validation, `FF-GATE-RUNTIME-001`, and the applicable WP-005 deep checks. Prerequisite runtime validation carries an explicit zero-product-progress ceiling. Runtime-affecting missing production proof cannot be skipped. Watcher proof activates with the watcher package, and the release gate remains `NOT_IMPLEMENTED`; none of those states may be converted into product PASS.

</topic>

<topic id="phase-0-contract-operation" status="active" version="1" wp="WP-FF-005-versioned-core-contracts-v1" ingestable="true" updated_at="2026-07-22">

## Inspect and change the Phase 0 contracts and models

The contract and proof surfaces are:

- `product/crates/fforager-contracts/`: versioned product identities, source graph, acquisition/sink DTOs, public/process/plugin/JavaScript-worker envelopes, bounded framing, journal/commit/archive/durability, and filesystem-capability descriptions.
- `product/crates/fforager-diagnostics-contract/`: bounded diagnostic/event/crash/cancellation/lifecycle DTOs, protocol/schema negotiation, sequence/replay/durable acknowledgement, privacy classifications, and retention descriptions.
- `product/crates/fforager-core/`: pure deterministic lifecycle, atomic resource-vector, byte-credit, durability-position, cancellation, restart, and replay models. It emits effect intents; it performs no effects.
- `build/crates/fforager-testkit/`: non-shipped shared cross-version and malformed-frame conformance harness.
- `build/fixtures/contracts/inventory.json`: canonical machine-readable stable IDs, owners, type names, version policies, limits/errors, fixtures, proof IDs, readiness gates, design anchors, and residual uncertainty.
- `build/fixtures/contracts/`: canonical prior/current/incompatible versions, unknown-kind input, and state/resource scenarios.

Inspect the inventory first, then the named owner module and proof ID. Run focused proof while editing:

```powershell
cargo test --manifest-path build/Cargo.toml --locked -p fforager-contracts
cargo test --manifest-path build/Cargo.toml --locked -p fforager-diagnostics-contract
cargo test --manifest-path build/Cargo.toml --locked -p fforager-core
cargo test --manifest-path build/Cargo.toml --locked -p fforager-testkit
cargo clippy --manifest-path build/Cargo.toml --workspace --all-targets --all-features --locked -- -D warnings
cargo run --manifest-path build/Cargo.toml --locked -p fforager-xtask -- verify-deep --evidence-from-taskboard
```

Wire versions use incompatible major versions and inclusive trusted local reader ranges; a sender cannot self-authorize compatibility. Every envelope type binds to its exact canonical schema ID before dispatch. `ProtocolOfferV1` migration retains bounded DTO fields only: the shipped diagnostics authority accepts exact schema identity only, and rejects every non-exact drift until a future reviewed transition is explicitly added and behavior-tested. Unknown mandatory kinds and unnamespaced unknown fields fail. Unknown optional data is accepted only through a namespaced `ExtensionMap` inside fixed entry, key, value, and total byte budgets. Stable typed IDs reject the wrong prefix, uppercase/noncanonical characters, empty suffixes, and values above 128 bytes. Process, plugin, and JavaScript-worker framing is a four-byte big-endian length followed by JSON; declared size is rejected before payload allocation and every decoded envelope is recursively validated. The product frame defaults to 1 MiB. Diagnostics cap a JSON frame at 256 KiB, fields at 64, text at 4 KiB, IDs at 128 bytes, retained unknown optional values at 8 KiB, and schema identities at 16. Read the exported constants and inventory row before relying on any limit.

Every lifecycle transition is pure and owned. Invalid transitions leave state and trace unchanged. Traces are bounded and replayable. The commit/archive and byte-durability models enumerate effect-request and acknowledged durable-prefix states; an emitted write, sync, verification, archive, cleanup, or cancellation intent never counts as acknowledged success, and restart reconciliation never invents success from a partial prefix. Resource admission is one checked 13-dimensional vector transaction with exact grant ownership, bounded waiter item/byte counts, strict FIFO head reservation, a deterministic per-owner active-grant ceiling, explicit cancellation dispatch, transactional release, and exact ownership checks. Byte credits conserve capacity, revoke released unused credit, reject positions beyond consumed or live credited bytes, and track received, validated, written, and durable positions monotonically.

Each state-machine instance receives a caller-supplied, nonzero, stable `MachineInstanceId`. An acknowledgement, effect failure, or effect cancellation outcome is accepted only when its instance ID, effect, and generation exactly match a currently pending effect. Persist that instance identity and the next generation with the durable state. Public restoration accepts only the states returned by the machine's `durable_states` whitelist; testkit cross-checks that whitelist against the inventory's exact durable-prefix list. Initial, transient, failed, inconsistent, cancelled, and other non-enumerated states are not restart prefixes.

Byte receive consumes a named claim owned by the named owner and records that attribution. An unconsumed claim may transfer ownership, but a partially or fully consumed claim rejects transfer so historical attribution cannot be rewritten. Releasing a claim revokes its unused remainder while retaining consumed attribution for audit.

Acquisition and output-sink descriptors are data-only but still validated before adapter dispatch. Their URLs, identifiers, checksums, path references, fragment counts, expected lifetimes, and bounded-buffer sizes have explicit limits. Rooted path references reject absolute paths, parent traversal, empty segments, Windows drive/device prefixes, and trailing dot or space segments. Plugin and JavaScript-worker messages cross boundaries only inside complete envelopes carrying version compatibility, request correlation, producer/job identity, and provenance; bare payload enums are not dispatch contracts.

Before dispatching decoded boundary data:

- Apply the exported contract validator with the explicit limit set selected by the owning adapter.
- Reject incompatible schema versions before reading operation-specific payload fields.
- Keep request registration separate from correlated events and acknowledgements; only a new request consumes a new request-ID slot.
- Compare cancellation acknowledgements with the originating request ID, generation, and caller-selected expected responder before retiring ownership.
- Route lifecycle acknowledgements and effect failure/cancellation outcomes with the persisted stable machine instance ID plus exact effect and generation; never synthesize or reuse another instance's token.
- Treat validation failure as a typed boundary rejection; do not repair, truncate, default, or silently discard mandatory data.

To change or regenerate fixtures, never overwrite or reinterpret a supported prior-version file. Add a new versioned fixture, update `inventory.json`, add the matching reader/writer or rejection test, and run the focused testkit plus `verify-deep`. A breaking schema change requires a new major version and retained old fixture. An additive minor change must remain inside a declared compatibility range. Reusing a stable ID for changed semantics is forbidden.

Common contract failures and recovery:

- `IncompatibleMajor` or unsupported minor: select a mutually supported range or implement an explicit version migration; do not relax the check.
- `UnknownMandatoryKind`: update both peers and add a versioned fixture, or encode genuinely optional data in a bounded namespaced extension.
- `Oversized`, `PartialHeader`, `PartialPayload`, or invalid JSON: reject the frame, reset the decoder, and replay only from an owned protocol boundary; never allocate from the unvalidated length.
- Duplicate request/identity, ambiguous canonicalization, or dangling graph relationship: repair the producer; do not silently deduplicate or guess a target.
- Invalid lifecycle transition or replay mismatch: preserve the emitted trace/seed, inspect the named owner and precondition, and rerun the exact counterfactual test.
- Admission overflow, capacity, owner, or release error: leave the ledger unchanged, repair the request/ownership flow, and rerun zero, exact-capacity, one-over, cancellation, and repeated-release tests.
- Inventory or fixture failure: restore a unique stable ID, complete every required field, ensure repository-relative fixture containment, and rerun `fforager-testkit` before the deep gate.
- Canonical inventory digest mismatch: inspect the semantic diff first; if the change is authorized, update the exact inventory mappings and `CANONICAL_INVENTORY_FNV1A64` together, then rerun the representative field-mutation test and full testkit. Never update the digest merely to silence unexplained drift.
- Restore or acknowledgement failure: recover the persisted `MachineInstanceId`, generation seed, and inventory-enumerated durable state; do not acknowledge a transient effect or substitute a token from another instance.
- Consumed-claim transfer failure: retain the original owner attribution, release the claim when appropriate, and create a new claim for later ownership rather than rewriting consumed history.
- Data-only scan failure: replace runtime handles, processes, sockets, filesystem handles, threads, channels, or locks with serializable data or explicit effect-intent DTOs owned by a later adapter.

These contracts do not select or implement network, storage, archive, FFmpeg, JavaScript, plugin, scheduler, watcher, or transport adapters. A later shipped consumer must prove actual behavior through `FF-GATE-RUNTIME-001` using the exact staged production artifact.

</topic>

<topic id="phase-0-javascript-challenge-spike" status="active" version="4" wp="WP-FF-006-rust-youtube-challenge-spike-v1" ingestable="true" updated_at="2026-07-27">

## Operate the independent Rust JavaScript challenge spike

WP-FF-006 is a non-shipped Prerequisite 0C experiment. It proves a Ferric-owned Rust path for discovering and solving `YouTube` player `n` and signature challenges through a bounded pure-Rust JavaScript worker. It does not implement video download, extraction, a shipped JavaScript adapter, replacement parity, packaging, release, or product-phase progress.

### Navigation and identity

- Workspace crate: `build/crates/fforager-javascript`.
- Worker entrypoint: `build/crates/fforager-javascript/src/bin/fforager-js-worker.rs`.
- Corpus/proof runner: `build/crates/fforager-javascript/src/bin/fforager-js-corpus.rs`.
- Committed input contract: `build/fixtures/youtube-challenge-v1/manifest.json`.
- Raw site-supplied players: `build/fixtures/youtube-challenge-v1/players`.
- Generated machine verdict: `build/reports/wp-ff-006-youtube-challenge-report.json`.
- Engine identity: `boa_engine@0.21.1`.
- Ferric solver identity: `ferric-player-ast-v1`.

The raw players are inert corpus inputs. Ferric-owned Rust parses their AST, retains the discovered challenge-relevant definitions, and evaluates the reduced program in a fresh Boa context. External implementations may explain oracle provenance, but Ferric does not run, import, vendor, download, or fall back to their solver code.

### Reproduce the accepted proof

Run from the repository root:

```powershell
cargo build --manifest-path build/Cargo.toml --locked --release -p fforager-javascript --bins
cargo run --manifest-path build/Cargo.toml --locked --release -p fforager-javascript --bin fforager-js-corpus
```

Success prints `PASS_RUST_ONLY_PATH build/reports/wp-ff-006-youtube-challenge-report.json`. Open that JSON and verify all of the following rather than trusting console text alone:

- `verdict` is `PASS_RUST_ONLY_PATH`.
- `mandatory_passed` equals `mandatory_total`.
- every `cases[].matched` is `true`;
- every `probes[].passed` is `true`;
- `counterfactual.original_oracle_passed` and `counterfactual.mutated_oracle_rejected` are `true`;
- `zero_product_progress` is `true`;
- every terminal probe reports `reaped: true` and `process_absent_after_reap: true`.

Optional relocatable overrides are `--manifest`, `--player-root`, and `--report`. All values are resolved from the current repository invocation; the implementation has no hardcoded workspace drive or user-profile path.

### Inputs, outputs, and limits

The manifest is strict JSON and binds every mandatory case to a relative player filename, exact SHA-256, challenge kind, input, expected output, engine, solver implementation, and non-executable oracle provenance. Unknown manifest or worker-input fields fail.

The supervisor spawns `fforager-js-worker` with an empty inherited environment, no foreground window, no host functions registered in Boa, a fresh unguessable destructive-probe grant, and a canonical player root. Worker IPC reuses `ff.javascript-worker@1` from WP-005 with a 1 MiB frame ceiling. Requests and responses are bound by producer, request, job, sequence, compatibility, provenance, and capability-grant identity. Additional bounds are:

- raw player: 4 MiB and ASCII source bytes;
- serialized worker output: 1 MiB;
- requested wall deadline: 20 seconds per corpus case;
- requested/monitored worker RSS: 512 MiB;
- Boa instruction budget: 50,000,000;
- Boa loop budget: 5,000,000;
- recursion depth: 512;
- JavaScript stack size: 10,240;
- worker native thread stack: 64 MiB;
- cache: 8 entries and 24 MiB, keyed by script hash, solver, engine, execution mode, and extractor version;
- worker recycle: 64 jobs or 300 seconds, whichever occurs first.

The report covers exact raw-player results plus cache separation, malformed/partial/oversized frames, exact fatal protocol categories, protocol version/reference/field rejection, protocol memory ceilings, destructive-probe authorization, ambient capability denial, fresh-context isolation, hash mismatch, path confinement, timeout, acknowledged cancellation, memory pressure, crash cleanup, provenance-keyed quarantine with an operator-clear path, and quota recycling. Quarantine activates after two terminal failures for one provenance key; it does not block an independent key.

### Safety constraints and prohibited substitutions

- Do not add or invoke Python, `yt-dlp`, any `yt-dlp` companion package, `yt-dlp-ejs`, Node.js, Deno, Bun, QuickJS, Wasmtime, a browser runtime, a downloaded solver bundle, or an external solver executable.
- Do not turn the corpus runner into a downloader or network extractor.
- Do not execute the whole raw player without Ferric-owned structural reduction.
- Do not weaken hashes, frame bounds, protocol validation, resource limits, fresh contexts, path confinement, process reaping, or the counterfactual to make a failure pass.
- A failed spike produces `FAILED_SPIKE_REQUIRES_OPERATOR_DECISION`; it never authorizes an automatic fallback.
- `build/deny.toml` contains one bounded advisory exception: `RUSTSEC-2024-0436` for the unmaintained `paste 1.0.15` proc-macro transitively required by Boa 0.21.1. It is not a waiver for a later product consumer; remove it when Boa removes the dependency, or require an explicit new authorization before promotion.

### Failure diagnosis and recovery

- `worker binary missing`: run the release `cargo build ... --bins` command, then rerun the corpus command.
- manifest identity/hash failure: compare the exact committed manifest and player bytes; do not rewrite an oracle or hash until the corpus change is explicitly authorized.
- `ScriptHashMismatch`: the requested player bytes and manifest digest differ. Restore the committed bytes or update the corpus as an intentional reviewed change.
- timeout, memory, cancellation, crash, quarantine, or recycle probe failure: inspect the corresponding report row and confirm `reaped` plus `process_absent_after_reap`; never reuse or trust that worker.
- cancellation failure: require the correlated `AcknowledgedCancelled` generation before accepting cooperative cancellation; a forced termination after the acknowledgement grace is a distinct failed probe outcome.
- quarantine failure: retain the failing provenance key and terminal receipts, confirm no worker spawned after the second failure, and use only the explicit operator-clear path after the underlying fault is resolved.
- challenge mismatch or no candidate: retain the report as evidence, inspect the exact raw player and Ferric AST discovery path, and require an Operator decision before changing the accepted approach.
- stale-cache failure: verify that script hash, solver, engine, execution mode, and extractor version all participate in the key; never reuse cached heap state.
- dependency-policy failure: inspect the exact Cargo path and advisory identity. Do not broaden wildcard or advisory policy; the only accepted advisory ID is `RUSTSEC-2024-0436` under the non-shipped WP-006 ceiling.
- disk exhaustion during canonical gates: run `powershell -NoProfile -File build/scripts/clean-artifacts.ps1`; retain sources, fixtures, the lockfile, and governance reports, then rerun the same gate.

The worker is quiet and bounded. If an interrupted manual run leaves `fforager-js-worker` alive, terminate only the recorded report PID after confirming its executable path is this repository's `.fforager-artifacts/cargo-target` worker, then rerun the corpus so the process-absence probes produce fresh evidence.

</topic>

<topic id="phase-0-transport-fingerprint-spike" status="active" version="1" wp="WP-FF-007-transport-fingerprint-spike-v1" ingestable="true" updated_at="2026-07-27">

## Operate the transport capability and security corpus

WP-FF-007 is a non-shipped Prerequisite 0D spike. It owns the mandatory transport corpus that WP-FF-004 did not provide. WP-FF-004 remains the authority for stable transcript normalization and identifiers; WP-FF-007 owns fingerprint, HTTP wire, redirect, cookie, SSRF/DNS, proxy-evidence, pooling, resource-bound, retry, cancellation, and replay cases. Neither packet provides product transport capability or product-phase progress.

Run the exact corpus from the repository root:

```powershell
cargo run --manifest-path build/Cargo.toml --locked -p fforager-xtask -- transport-corpus
```

The command reads `build/fixtures/transport-v1/manifest.json`, executes `build/crates/fforager-transport`, and atomically writes a unique report under `build/reports/`. The manifest is strict, bounded, and pinned by its canonical declaration SHA-256; removing, adding, retyping, or changing a case fails before execution. All 54 cases are mandatory. Every result records the concrete input, executed boundary, negotiated capability partition, explicit executed/not-executed connection/wire/pool/address/proxy/ALPN/protocol identities, nonempty limits, decision/execution/total timing phases, normalized transcript, exact expected and observed outcomes, skipped semantic dependencies, and proof class. The report binds all candidate/corpus source files and a timing-independent semantic projection digest. The independent xtask consumer reloads the canonical manifest, reconstructs every capability decision and blocked diagnostic, validates exact case declarations/outcomes/transcripts/summaries/verdict, and recomputes the semantic digest. Behavior-deficient and forged-capability counterfactuals must both be rejected by the producer oracle.

The current authorized candidate is `ferric-std-first-transport-spike-v1`. It supports only the capabilities its executable evidence proves: bounded loopback HTTP/1.1, range semantics, coordinated fragmented streaming with byte credits, sanitized replay, in-flight socket cancellation with worker reaping, metadata bounds, and body bounds. Request and response wire identities are frozen by normalized SHA-256 expectations. Policy-model probes remain useful regression tests, but they are private implementation details and do not upgrade candidate capability claims.

The v1 corpus has no opt-in live internet cases. Live proxy, TLS, HTTP/2, or target-site probes are deliberately unavailable and cannot be substituted for the deterministic corpus. Adding a live partition requires a later authorized manifest/version change with separate non-canonical output and secret handling.

Read the aggregate result literally:

- `PASS_PURE_RUST_PATH` means every mandatory capability was executed without a blocked capability.
- `FAILED_SPIKE_REQUIRES_OPERATOR_DECISION` means all declared case oracles may have passed, but one or more mandatory capabilities were blocked before execution. This is a completed fail-closed spike result, not permission to substitute standard HTTP or claim browser parity.

The std-first candidate currently blocks browser-equivalent TLS ClientHello fingerprinting, browser-equivalent HTTP/2 wire fingerprinting, HTTP/2 execution, trusted proxy destination evidence, compression/decompression, redirect integration, cookie scope, DNS provenance, SSRF policy, pool partition, and retry execution. Redirect stays blocked until each hop is bound to authoritative DNS evidence, a complete generated special-purpose registry, and the actual socket peer. Cookie scope stays blocked until a versioned authoritative PSL implements wildcard and exception rules. Pool partition stays blocked until keys derive from immutable execution context. Retry stays blocked until attempts, idempotency, deadlines, cancellation, and partial-body state cross one executor. A request for any blocked capability returns a typed stable diagnostic before network execution; policy fixtures or standard HTTP must never silently stand in for a complete capability.

Safety and recovery:

- Do not add Python, `yt-dlp`, BoringSSL, curl, curl-impersonate, `curl_cffi`, native TLS, or another external runtime/native dependency.
- Do not connect the local harness grant to a caller-supplied URL or weaken SSRF policy to reach fixtures.
- Do not share a pool entry across origin, proxy, TLS, HTTP, fingerprint, client-certificate, session, or credential-scope key differences.
- Do not persist raw authorization, proxy authorization, cookies, API keys, or secret query values. The report rejects its secret canary.
- The corpus runner executes the exact committed unknown-field and duplicate-ID negative fixtures on every canonical run. A decorative or skipped negative fixture is a gate failure.
- If a case differs, inspect its `exact_mismatch`, concrete input, and executed boundary; repair the implementation or declared corpus deliberately and rerun the same command.
- If the aggregate verdict is failed, retain the blocked-capability list and residual uncertainties for Operator selection. Do not auto-promote or add a forbidden fallback.
- The canonical `verify-deep` and `verify-pr` gates execute this corpus and therefore refresh the same report.

</topic>

<topic id="phase-0-wreq-transport-adjudication" status="active" version="1" wp="WP-FF-015-wreq-transport-adjudication-v1" ingestable="true" updated_at="2026-07-28">

## Adjudicate the exact-pinned wreq dependency candidate

WP-FF-015 compares stable `wreq 5.3.0` with exact-pinned `wreq 6.0.0-rc.29` plus `wreq-util 3.0.0-rc.14` under narrow exception `FF-DEC-001`. The exception is limited to the non-shipped `fforager-transport` prerequisite package. It does not authorize a product dependency, production promotion, packaging, release, a general native dependency, or product progress.

Run the current release-candidate regressions from the repository root with one Cargo process and the shared repository-local target:

```powershell
cargo test --manifest-path build/Cargo.toml --locked --jobs 1 -p fforager-transport --all-features
```

Reproduce the stable control sequentially from its reachable sanitized checkpoint. Do not run stable and release-candidate builds concurrently because their BoringSSL packages use the same native link identity:

```powershell
powershell -NoProfile -File build/scripts/new-worktree.ps1 -WorktreeId wp15-stable-replay -Branch codex/wp15-stable-replay -StartPoint 7dae6b6
Push-Location .worktrees/wp15-stable-replay
cargo test --manifest-path build/Cargo.toml --locked --jobs 1 -p fforager-transport --all-features
Pop-Location
git worktree unlock .worktrees/wp15-stable-replay
git worktree remove .worktrees/wp15-stable-replay
git branch -D codex/wp15-stable-replay
```

Emit the aggregate adjudication report with a new collision-free name:

```powershell
$stamp = Get-Date -Format "yyyyMMddTHHmmssZ"
cargo run --manifest-path build/Cargo.toml --locked --jobs 1 -p fforager-transport --bin fforager-wreq-adjudication -- --output "build/reports/wp-ff-015-wreq-adjudication-$stamp.json"
```

The command strictly reloads a freshly reconstructed report before committing it atomically. Read the stdout verdict literally:

- `PASS_WREQ_ADJUDICATION` is possible only when every mandatory operational capability and residual dependency/build constraint is satisfied.
- `FAILED_WREQ_ADJUDICATION_REQUIRES_OPERATOR_DECISION` is the current expected result. It is a completed fail-closed prerequisite verdict, not transport promotion.

The report records both exact dependency candidates, features, profile coverage, native-link surfaces, the selected non-shipped result, the complete operational capability decision, residual blocker codes, `zero_product_progress: true`, and `product_promotion_authorized: false`. Its strict consumer rejects unknown fields, changed dependency identities, removed blockers, forged PASS, or any other divergence from a fresh deterministic reconstruction.

The current candidate configures `chrome-136-windows`, disables ambient proxy use, redirects, cookie storage, and transparent decompression, bounds connection/read/total time, caps idle and total pool size, and bounds response accumulation. Configuration and structural echo data do not prove browser parity. Operational profile admission therefore blocks before network I/O on missing TLS fingerprint parity, HTTP/2 fingerprint parity, Ferric-authoritative DNS provenance, pre-connect and peer-address SSRF enforcement, immutable pool partitioning, and wreq cancellation/teardown proof. Dependency response chunks can transiently exist before Ferric's accumulated-body check, and the native toolchain has only the declared Windows host proof; both remain explicit aggregate blockers.

The live boundary is non-canonical and requires opt-in in both the CLI and library API:

```powershell
$stamp = Get-Date -Format "yyyyMMddTHHmmssZ"
cargo run --manifest-path build/Cargo.toml --locked --jobs 1 -p fforager-transport --bin fforager-wreq-live-probe -- --enable-live --output "build/reports/wp-ff-015-wreq-live-refusal-$stamp.json"
```

The current expected live result is exit code `1` with `blocked_external_wire`. It refuses before dependency DNS or socket I/O because the candidate lacks Ferric-authoritative DNS and SSRF enforcement. Do not weaken this refusal, reuse an older structural observation as current proof, or treat endpoint-provided JA3/JA4/Akamai text as independent browser parity.

Failure and recovery:

- `FF-WREQ-E-SATURATED`: another request owns the single adjudication runtime lane. Retry after it completes; there is no unbounded admission queue.
- `FF-WREQ-E-NESTED-RUNTIME`: invoke the synchronous command outside an existing Tokio runtime.
- `FF-WREQ-E-REPORT-COLLISION`: choose a new report name. Existing reports are immutable.
- `FF-WREQ-E-REPORT-CONTAINMENT`: repair `build/reports/` so it resolves inside the repository; do not redirect reports through a junction or symlink.
- native-edge, source, feature, link, or host diagnostics: restore the exact `FF-DEC-001` graph and rerun architecture, dependency, transport, and adjudication checks. Do not broaden the exception.
- after all WP-FF-015 proof is recorded and before starting another task, run `powershell -NoProfile -File build/scripts/clean-artifacts.ps1`; this removes only disposable content under the shared `.fforager-artifacts/` root.

</topic>

<topic id="ordinary-transport-dependency-decision" status="active" version="2" wp="WP-FF-017-ordinary-transport-decision-v1" ingestable="true" updated_at="2026-10-20">

## Verify the ordinary transport dependency decision

`FF-DEC-002` authorizes future shipped `fforager-net` use of exact `reqwest 0.13.4`, `rustls 0.23.46`, native `ring 0.17.14`, and `webpki-roots 1.0.10`. Reqwest default features are off; the only requested reqwest features are `http2`, `rustls-no-provider`, and `stream`; the explicit rustls features are `ring`, `std`, and `tls12`. The resolved ordinary profile is pinned, including reqwest's internal `__rustls` and `__tls` features. AWS-LC, native-tls, OpenSSL, ambient system proxy configuration, automatic redirects, cookie storage, automatic decompression, and dependency retry authority are not part of this decision.

Run the exact non-shipped construction and typed refusal proof from the repository root:

```powershell
cargo run --manifest-path build/Cargo.toml --locked --jobs 1 -p fforager-transport --no-default-features --features ordinary-transport --bin fforager-ordinary-transport-decision
```

The command builds the exact reqwest client from a provider-bound rustls `ClientConfig` using ring and the pinned Mozilla root snapshot; it does not install a process-global provider or select the platform trust store at runtime. It behaviorally proves that injected proxy/redirect and retry policies are overridden, a received cookie is not stored or resent, and a gzip response remains compressed. Automatic decompression is disabled primarily by the exact resolved feature graph; the wire probe validates the resulting behavior rather than claiming that a feature-gated no-op builder call caused it. The command separately requests TLS and HTTP/2 fingerprint capabilities and proves that `FF-TRANSPORT-E-BROWSER-TRANSPORT-REQUIRED` is returned before client construction or network execution. The strict reload emits only `PASS_ORDINARY_TRANSPORT_COMPONENT_PROOF`, `final_decision_verdict_emitted: false`, `wreq_product_fallback: false`, and `PREREQUISITE only: zero Phase 1 product implementation and runtime progress`; only the canonical packet/gate/review aggregator may later emit `PASS_ORDINARY_TRANSPORT_DECISION`.

The locked ring crate contains bundled C source and 17 packaged pregenerated Windows assembly objects. This is explicit rather than hidden: `FF-DEC-002` authorizes only exact `ring@0.17.14`, and the architecture gate checks the Cargo.lock checksum, cached crate archive checksum, full extracted source-tree digest, exact selected features, recursive object-inventory digest, and native-link reachability. It does not authorize arbitrary prebuilt libraries, another provider, build-time downloads, or environment-selected crypto.

Reqwest's `rustls-no-provider` feature currently selects `rustls-platform-verifier 0.7.0`. On Windows that package contains native trust-verifier FFI, so `FF-DEC-002` records and validates its lock/archive/source digests as a selected native-runtime package even though Ferric's preconfigured rustls client bypasses it at runtime. Ring remains the only native crypto link/build provider. A digest under separate `FF-DEC-002` authority pins the entire `fforager-transport`-reachable ordinary resolve closure—package identities, selected features, and edges—so an unknown no-`links` FFI wrapper cannot self-authorize through build policy. The ordinary profile also rejects wreq, BoringSSL, AWS-LC, native-tls, and OpenSSL.

Ferric—not reqwest—must own address admission, redirect approval, cookie storage and scope, proxy authorization, immutable pool partitioning, global and per-origin in-flight admission, download and decompression bounds, retry budgets, timeouts, cancellation, receipts, and diagnostics. A future `fforager-net` product consumer must pass the mandatory transport corpus and staged `FF-GATE-RUNTIME-001`; this prerequisite decision is not a working downloader or product progress.

`wreq 6.0.0-rc.29` and `wreq-util 3.0.0-rc.14` remain confined to non-shipped `fforager-transport` evidence under `FF-DEC-001`. They are disabled for product routing and cannot be used as an automatic fallback. A browser transport requires closed blockers and a new explicit Operator decision.

Recovery:

- `FF-ARCH-E-DEPENDENCY-FEATURE`: restore the exact feature lists and disabled defaults in `build/Cargo.toml`, the consumer manifest, and `build/architecture-policy.toml`.
- `FF-ARCH-E-NATIVE-LINK-SURFACE`, `FF-ARCH-E-NATIVE-SOURCE`, or `FF-ARCH-E-NATIVE-PRECOMPILED`: restore exact `ring@0.17.14`, its locked source package and object-inventory digest, exact `rustls-platform-verifier@0.7.0` attribution, and the narrow exception. Do not add AWS-LC or broaden the native allowlist.
- `FF-ARCH-E-RESOLVED-FEATURE-SURFACE` or `FF-ARCH-E-ORDINARY-PROFILE-ISOLATION`: restore the exact ordinary feature graph and keep wreq/BoringSSL disabled for that profile.
- `FF-TRANSPORT-E-BROWSER-TRANSPORT-REQUIRED`: this is the intended fail-closed product result for fingerprint-required requests until a browser engine is separately approved; do not retry through `wreq`.
- A report mismatch or forged PASS: rerun the command from the clean repository root and inspect the exact dependency, control, and product-progress fields. Do not edit the expected report.

</topic>

<topic id="runtime-proof-contract" status="active" version="1" wp="WP-FF-012-runtime-truth-gates-v1" ingestable="true" updated_at="2026-07-19">

## Declare production runtime proof

For a product-affecting packet, set `scope.product_impact` to `RUNTIME`, give each product acceptance row `proof_class: production_runtime`, and place an `ff.runtime-proof@1` object at `extensions.runtime_proof`. Define the scenarios before implementation. This shape is strict: unknown keys, omitted keys, substitute modes, unsafe paths, and mismatched artifact or fixture hashes fail the gate.

```json
{
  "schema_id": "ff.runtime-proof@1",
  "completion_claim": "operator_usable_runtime",
  "artifact": {
    "package": "fforager",
    "binary": "fforager",
    "profile": "release",
    "features": [],
    "package_mode": "clean_staged",
    "execution_mode": "external_process",
    "compilation_mode": "production",
    "dependency_mode": "normal_only",
    "testkit_mode": "forbidden",
    "adapter_mode": "production"
  },
  "forbidden_substitutes": [
    "mock",
    "fake",
    "stub",
    "fixture-only-implementation",
    "in-memory-substitute",
    "hardcoded-success",
    "test-only-adapter",
    "direct-internal-call"
  ],
  "scenarios": [
    {
      "id": "capability-success",
      "kind": "success",
      "capability_ids": ["replace-with-stable-capability-id"],
      "args": ["replace-with-production-cli-arguments"],
      "timeout_seconds": 30,
      "inputs": [{
        "source": "build/fixtures/replace-with-committed-input",
        "destination": "inputs/representative-input",
        "sha256": "replace-with-64-lowercase-hex-digest"
      }],
      "production_boundaries": ["replace-with-real-boundary-name"],
      "expected": {
        "exit_code": 0,
        "stdout_contains": ["replace-with-required-output"],
        "stderr_contains": [],
        "output_files": []
      },
      "counterfactual": {
        "target": "stdout_contains",
        "value": "replace-with-required-output",
        "expected_diagnostic": "FF-RUNTIME-E-OBSERVABLE-MISSING"
      }
    },
    {
      "id": "capability-negative",
      "kind": "negative",
      "capability_ids": ["replace-with-stable-capability-id"],
      "args": ["replace-with-invalid-production-cli-arguments"],
      "timeout_seconds": 30,
      "inputs": [],
      "production_boundaries": ["replace-with-real-boundary-name"],
      "expected": {
        "exit_code": 2,
        "stdout_contains": [],
        "stderr_contains": ["replace-with-stable-error"],
        "output_files": []
      },
      "counterfactual": null
    }
  ]
}
```

Replace every `replace-with-*` value with the packet's real capability, production CLI arguments, boundary, observables, and committed input digest. The success scenario requires a hash-bound representative input. The negative scenario must exercise the shipped binary and return nonzero. Output files, when used, require a safe stage-relative path, positive `min_bytes`, and optional SHA-256. Inputs and outputs cannot overwrite the binary, gate receipts, or one another.

</topic>

<topic id="phase-0-safety-recovery" status="active" version="2" wp="WP-FF-003-executable-gate-bootstrap-v2" updated_at="2026-07-19">

## Inputs, outputs, safety, and recovery

Inputs are committed policies, the locked workspace, the active packet, the canonical build rules, governed Rust source, and negative fixtures. Disposable outputs are confined to `.fforager-artifacts/`; governance reports remain under `build/reports/`; neither is shipped.

The gate runner never auto-installs or upgrades tools, never uses a shell to compose child commands, and refuses to run outside the repository root. Every child process is bounded; a timeout kills and reaps the child and reports incomplete evidence instead of PASS. Project-owned TOML schemas reject unknown keys, and governance YAML must parse as one structurally valid document before any value is consumed. Tool identity checks require exact output and supported-host policy; host-installed Git and cargo-deny executables also require the pinned SHA-256 digest. Unknown rule IDs, missing proof mappings, missing or unreferenced fixtures, duplicate or nested toolchain selectors, wrong-root build files, undeclared workspace members, and runtime boundary literals fail closed.

Common failures and recovery:

- Tool version mismatch: install the exact root-selected Rust toolchain/components and the tooling-policy version of `cargo-deny`, then rerun.
- Tool checksum mismatch: resolve the executable selected by `PATH`, compare it with `build/tooling-policy.toml`, and treat any intended tool update as a dedicated validated policy change.
- External command timeout: inspect the named command and environment; its evidence is incomplete, so fix the hang or resource stall and rerun the full gate.
- Governance YAML parse or shape failure: repair the canonical YAML structure; do not bypass parsing with lexical matching.
- `--locked` failure: do not remove `--locked`; reconcile `build/Cargo.toml` and intentionally regenerate `build/Cargo.lock`.
- Wrong current directory: return to the repository root and rerun the canonical command.
- Policy or fixture mismatch: use the stable diagnostic in stderr/report, correct the canonical policy or implementation, and rerun the same gate.
- Stale build output: run `powershell -NoProfile -File build/scripts/clean-artifacts.ps1`; the script resolves and cleans only the ignored repository-local `.fforager-artifacts/` root.
- Failed report write: verify `build/reports/` is writable. Temporary report files are removed on failure and an incomplete final report is never accepted.

An architecture report proves the declared prerequisite graph, policy, source scan, and assigned negative cases. It does not prove product runtime behavior, packaging, watcher independence, compatibility, durability, or performance. Product proof begins only when FF-GATE-RUNTIME-001 builds, hashes, stages, and externally executes the exact production artifact through a shipped entrypoint and verifies operator-visible results plus a failing counterfactual.

</topic>
