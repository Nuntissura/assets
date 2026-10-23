---
file_id: "ferric-forager-security-reliability-review-v0.2.0"
file_kind: "adversarial-peer-review"
updated_at: "2026-07-18T00:25:34+02:00"
source_design: "../../spec/ferric_forager_technical_design_v0.2.0.md"
risk_tier: "HIGH"
verdict: "FAIL"
review_lens: "security-persistence-resume-failure-recovery-concurrency-sandboxing-supply-chain-cross-platform"
---

<topic id="independent-findings" status="complete" version="0.2.0" summary="Severity-ranked independent findings">

# INDEPENDENT_FINDINGS

## Verdict

**FAIL — the design is not implementation-ready.** The product direction is viable, and the proposed Rust-first protocol engine, bounded fragment pipeline, SQLite archive, typed FFmpeg planner, and explicit resource scheduler are sound architectural starting points. The current document does not yet define enforceable contracts at the boundaries where loss, duplication, sandbox escape, credential disclosure, path escape, or orphaned processes occur.

This is a contract-readiness failure, not a recommendation to narrow the product. All documented functionality can remain in scope. The operator's fixed production constraint is controlling: the shipped application is fully Rust, FFmpeg is the required external dependency, speed is the leading priority, yt-dlp is the behavioral foundation, and functionality expands beyond that foundation. Sections 6.13 and 18 nevertheless leave Deno, QuickJS, embedded JS engines, browser-impersonation dependencies, and native/external plugin models unresolved. Those are compatibility conflicts, not implicitly authorized dependencies. No production runtime or native transport library beyond FFmpeg should be selected without evidence that it remains within the Rust-only constraint or an explicit operator decision.

## Severity summary

| ID | Severity | Finding | Implementation-ready condition |
|---|---:|---|---|
| SR-001 | Critical | Final-file commit, journal cleanup, and archive insertion have irreducible cross-store crash windows with no recovery state machine. | Specify an idempotent commit protocol and startup reconciliation for every crash window. |
| SR-002 | Critical | Path confinement is expressed as validation prose, not race-safe filesystem operations. | Define handle-relative, no-follow platform implementations and destination conflict policy. |
| SR-003 | Critical | JS, plugins, and browser impersonation are both under-sandboxed and in conflict with the Rust-plus-FFmpeg production constraint. | Resolve the dependency conflict and define a capability-denied execution model with executable tests. |
| SR-004 | Critical | Redirect, DNS, proxy, nested-manifest, and credential policies do not close SSRF or cross-origin leakage. | Make destination and credential policy mandatory at every hop and nested URL resolution. |
| SR-005 | Critical | FFmpeg safety stops at typed argv; executable discovery, protocols, environment, handle inheritance, parser bounds, and process trees are unspecified. | Define and test the entire Rust-to-FFmpeg process boundary. |
| SR-006 | High | Fragment durability can acknowledge journal state ahead of durable contiguous media. | Define durability epochs, torn-tail recovery, source validators, and exact profile guarantees. |
| SR-007 | High | Local-filesystem assumptions are applied to Windows and network filesystems without a capability matrix. | Support local filesystems explicitly and downgrade/reject unproven remote filesystems. |
| SR-008 | High | Cancellation and resource ownership can strand permits, corrupt checkpoints, or leave blocking work/processes alive. | Define structured ownership, cancellation-safe state transitions, ordered permits, and escalation. |
| SR-009 | High | Cookie isolation omits public-suffix, snapshot, adapter, and sensitive-header details proven dangerous in yt-dlp. | Implement RFC-scoped jars and origin-specific forwarding through every transport adapter. |
| SR-010 | High | Archive identity, concurrency claims, migrations, and false-skip prevention are underspecified. | Specify immutable identity, unique constraints, expiring leases, and commit-after-output semantics. |
| SR-011 | High | Live fan-out does not protect the lossless recording path from slow or failed auxiliary sinks. | Make sink loss classes and backpressure/failure semantics explicit. |
| SR-012 | High | Parser and IPC limits are qualitative; combined expansion and arithmetic attacks remain open. | Add numeric budgets, checked arithmetic, and resource assertions to fuzzing. |
| SR-013 | High | Supply-chain controls do not cover source provenance, audited crates, bundled tools, artifacts, or rollback. | Add locked sources, audit/vetting, SBOM/provenance, tool fingerprints, and signed release/update policy. |
| SR-014 | High | Acceptance gates are prose and cannot disprove the failure modes above. | Add crash, filesystem, process-tree, sandbox, SSRF, cookie, parser, and concurrency gates per platform. |

## SR-001 — Commit/archive/journal crash consistency is undefined

The finalization sequence in source lines 1889–1902 closes and validates media, syncs it, renames it, updates the archive, and removes the work directory. A filesystem rename and a SQLite transaction are not one atomic transaction. A crash after rename but before archive commit produces a completed file that the archive does not know exists; a crash after archive commit but before cleanup produces stale resumable state; concurrent workers may both pass the archive check and download the same item. SQLite's own atomic-commit documentation depends on a precise flush and journal protocol, while Rust documents `rename` as same-mount only and platform-dependent ([SQLite atomic commit](https://www.sqlite.org/atomiccommit.html), [Rust `rename`](https://doc.rust-lang.org/std/fs/fn.rename.html)).

Required normative state machine:

1. The work directory **MUST** be on the same filesystem/volume as the final destination.
2. Close, validate, and `sync_all` the completed file; record its size and strong digest.
3. Append and durably sync a `PREPARED` journal record containing job ID, archive identity, source validators, final path, size, and digest.
4. Commit with an explicit no-clobber or operator-approved replace policy. Existing mismatched targets are quarantined or rejected; they are never silently overwritten.
5. Rename on the same filesystem. On systems where directory sync is meaningful, sync the destination parent before claiming a durable name.
6. Insert the completed archive record in a unique SQLite transaction, including the final fingerprint. Completed records are separate from expiring in-flight claims.
7. Append `ARCHIVED`, sync, then remove work state. Startup reconciliation **MUST** idempotently handle every prefix of this sequence.

The design must explicitly state whether the balanced speed-first profile accepts re-download after sudden power loss. It must never permit a durability downgrade to become a false archive hit that skips an output that was not committed.

## SR-002 — Path containment is vulnerable to time-of-check/time-of-use races

Source lines 1934–1944 require root containment and symlink defenses but do not define the actual open/create/rename operations. Canonicalizing a path, checking that it is under an output root, and later opening it is raceable: another process can replace a checked component with a symlink or Windows reparse point between those actions. Linux provides handle-relative resolution controls such as `RESOLVE_BENEATH`, `RESOLVE_IN_ROOT`, `RESOLVE_NO_SYMLINKS`, and `RESOLVE_NO_MAGICLINKS` specifically for this boundary ([`openat2(2)`](https://www.man7.org/linux/man-pages/man2/openat2.2.html)). Windows reparse points alter filesystem traversal and require explicit open/reparse handling ([Microsoft reparse-point operations](https://learn.microsoft.com/en-us/windows/win32/fileio/reparse-point-operations)).

Required contract:

- Resolve and create relative to a trusted root handle, not by rechecking a string path.
- On Linux, use an `openat2`-style beneath/in-root policy where available and a documented safe fallback where unavailable.
- On Windows, retain directory handles, reject unapproved reparse-point parents, open with reparse-aware flags, and specify share modes and replacement behavior.
- Use atomic create-new/no-follow semantics for new files. Rust's `create_new` provides an atomic create condition, but parent traversal still needs confinement ([Rust `File::create_new`](https://doc.rust-lang.org/std/fs/struct.File.html)).
- Treat `.desktop`, `.url`, `.webloc`, executable, script, and shortcut outputs as explicit typed operations rather than arbitrary template extensions. The current yt-dlp release fixed another command-injection path through shortcut/link output, demonstrating that an apparently passive output type can be executable behavior ([GHSA-6v4j-43gg-vj32](https://github.com/yt-dlp/yt-dlp/security/advisories/GHSA-6v4j-43gg-vj32)).

## SR-003 — Execution containment conflicts with the production constraint

The design lists persistent Deno, QuickJS, and embedded-engine options at lines 1469–1529, and process or Wasm plugins at lines 1709–1743. These choices are unresolved. Under the fixed operator requirement, Deno and QuickJS are additional production runtimes/native components beyond FFmpeg and therefore cannot be silently selected. Browser-impersonation support has the same problem if it requires a non-Rust native transport library. This conflict alone prevents implementation readiness.

Even apart from that authority conflict, the sandbox is not specified strongly enough. Deno denies many sensitive APIs by default, but static module graph loading and cache behavior require separate controls; its documentation recommends frozen/cached-only operation for controlled execution, and native-library or process permissions can escape the sandbox ([Deno permissions](https://docs.deno.com/runtime/reference/permissions/), [Deno security](https://docs.deno.com/runtime/fundamentals/security/)). QuickJS exposes memory, stack, and interrupt limits, but those are runtime limits rather than OS isolation ([QuickJS API](https://github.com/bellard/quickjs/blob/master/quickjs.h)). Wasmtime isolates WebAssembly from host resources through explicit imports, but stores require explicit resource limiting and interruption configuration ([Wasmtime security](https://docs.wasmtime.dev/security.html), [Wasmtime `Store`](https://docs.wasmtime.dev/api/wasmtime/struct.Store.html), [interrupting Wasm](https://docs.wasmtime.dev/examples-interrupting-wasm.html)).

Required resolution:

- Production code and built-in extractors remain Rust. Any JS challenge execution must first prove a Rust-only solution compatible with the operator constraint. If that cannot meet extractor requirements, the document must present the evidence and request an explicit operator decision; it may not assume Deno or QuickJS.
- Third-party extension execution should use a capability-based Wasm component host implemented in Rust, with an allowlist of host imports, bounded memory/tables, fuel or epoch deadlines, output-size limits, and per-instance state. Official/built-in extensions should be Rust.
- Native or arbitrary process plugins are not a canonical production path. Any exceptional helper needs its own explicit approval and OS sandbox contract.
- Browser impersonation is release-blocking only for extractors whose declared support requires it. A support claim cannot pass while the required transport is unavailable. Any non-Rust native dependency needs operator resolution.
- No mutable global execution context may cross job/session boundaries. Compiled-code caches may be shared only if they cannot carry user state or secrets.

## SR-004 — SSRF and credential forwarding are not closed across hops

The transport request model at lines 953–1052 identifies redirects, retries, and origin-bound values, but it does not provide an enforceable policy for initial DNS resolution, every redirect, DNS changes, proxies, nested manifests, and external adapters. URL fetchers can reach local services, follow redirects, cross protocols, and accidentally forward custom headers; curl documents these as application responsibilities ([curl known risks](https://curl.se/docs/knownrisks.html), [CVE-2022-27774](https://curl.se/docs/CVE-2022-27774.html)).

Required policy:

- Allow only explicitly supported schemes. Reject `file:`, `data:`, local IPC, device, and special schemes by default.
- At every initial request, redirect, playlist/manifest child URL, key URL, subtitle URL, thumbnail URL, and pagination URL, resolve all A/AAAA candidates and reject disallowed IANA special-use ranges. Bind the actual connection to the validated address while retaining the original hostname for Host/SNI. Re-evaluate after DNS refresh ([RFC 6890](https://www.rfc-editor.org/rfc/rfc6890.html), [IANA IPv4 special-purpose registry](https://www.iana.org/assignments/ipv4-address-space/ipv4-address-space.xhtml)).
- Specify policy for link-local metadata addresses such as AWS IMDS rather than relying on a generic private-address check ([AWS instance metadata](https://docs.aws.amazon.com/AWSEC/latest/UserGuide/instancedata-data-retrieval.html)).
- In proxy mode, enforce destination policy at a trusted proxy or report that address-level SSRF enforcement is unavailable. Do not claim equivalent protection.
- Tag every credential, cookie, and custom header with origin/scope sensitivity. Strip all sensitive values on cross-origin redirects unless an explicit extractor policy authorizes the destination.
- Make transports unable to bypass policy by accepting prebuilt raw requests.

## SR-005 — The Rust-to-FFmpeg boundary is incomplete

Typed argv and “no shell” at lines 1533–1584 eliminate one class of injection but do not secure the full boundary. FFmpeg enables all protocols by default unless a whitelist is supplied and supports local and network protocols beyond ordinary files ([FFmpeg protocols](https://ffmpeg.org/ffmpeg-protocols.html)). On Windows, bare executable names may be resolved using the current directory and other search locations, and child handle inheritance must be controlled ([CreateProcess](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessa)). Rust's `Child::kill` kills the child, not a descendant process tree ([Rust `Child`](https://doc.rust-lang.org/std/process/struct.Child.html)).

Required contract:

- Resolve FFmpeg to a configured absolute path or a bundled, versioned path. Do not search the current directory. Record version, capabilities, build configuration, and release hash.
- Use a scrubbed environment, fixed safe working directory, strict descriptor/handle inheritance, `-nostdin`, explicit overwrite policy (`-n` or controlled `-y`), machine-readable `-progress`, and bounded stderr/progress parsers ([FFmpeg CLI](https://ffmpeg.org/ffmpeg.html)).
- For ordinary muxing, pass only trusted local files/pipes and an explicit protocol whitelist. Do not hand attacker-controlled remote manifests to FFmpeg unless the same URL/SSRF policy is enforced through a deliberately approved path.
- On Windows, create the process suspended, assign it to a Job Object with kill-on-close, then resume; test breakaway and child/grandchild termination ([Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects), [AssignProcessToJobObject](https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-assignprocesstojobobject), [TerminateJobObject](https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-terminatejobobject)). On Unix, establish a new process group and signal/kill the group after a grace period ([Rust Unix `process_group`](https://doc.rust-lang.org/std/os/unix/process/trait.CommandExt.html)).
- Treat filenames, concat/list files, filter expressions, and FFmpeg-readable data files as typed untrusted inputs; direct argv does not neutralize syntax interpreted by the downstream program.

## SR-006 — Durable resume lacks a write-order invariant

The append-only journal and durability profiles at lines 1831–1888 do not say when media bytes become durable relative to checkpoint records. The mandatory invariant is: **a durable checkpoint may never describe bytes beyond the durable contiguous output prefix**. A safe checkpoint epoch is media write -> media flush/sync according to profile -> append framed checkpoint -> journal sync according to profile. Each journal record needs a length, version, epoch, and checksum so a torn tail can be ignored or truncated.

Fragment-completion records are advisory until incorporated into a durable contiguous checkpoint. Resume validation must use available source identity evidence—stable source ID, resolved asset identity, ETag, Last-Modified, content length/range, and rolling/segment digests. If identity or range semantics change, restart safely; never append new bytes to an unproven old prefix. Each durability profile must state its maximum acknowledged-but-redownloadable interval and power-loss behavior.

## SR-007 — Filesystem claims exceed tested platform semantics

SQLite WAL requires all readers to be on the same host and does not work over network filesystems ([SQLite WAL](https://www.sqlite.org/wal.html)). SQLite also warns that remote database access depends on locking and filesystem correctness that may not be reliable ([SQLite over a network](https://www.sqlite.org/useovernet.html), [SQLite corruption modes](https://www.sqlite.org/howtocorrupt.html)). Windows move/replace APIs differ from POSIX rename and may perform copy/delete across volumes or report partial replacement states ([MoveFileEx](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexa), [ReplaceFile](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew)).

The default must therefore be a local SQLite archive and same-volume work/final paths. Remote SMB/NFS output may be supported only with explicitly downgraded durability and tested semantics; a SQLite archive file on a network share should be rejected by default. Add a capability matrix for NTFS/ReFS, ext4-class local filesystems, APFS, and supported SMB/NFS configurations covering create-new, rename/replace, parent durability, file locks, sparse files, flushing, and recovery. “Atomic where supported” is not an acceptance criterion.

## SR-008 — Cancellation and concurrency ownership are underspecified

The scheduler at lines 1247–1325 has useful resource classes but no structured task lifecycle or cancellation-safe transition rules. Tokio documents `read_exact`, `read_to_end`, `write_all`, and semaphore acquisition as not cancellation-safe in `select!`; `spawn_blocking` work cannot be aborted once running ([Tokio `select!`](https://docs.rs/tokio/latest/tokio/macro.select.html), [Tokio `JoinHandle`](https://docs.rs/tokio/latest/tokio/task/struct.JoinHandle.html)).

Required contract:

- Every child task and process has one owning job scope; no detached task may outlive it.
- Cancellation follows stop intake -> signal children -> close queues -> drain commit-safe work -> terminate external process tree -> await all owners -> reconcile state.
- Commit-critical transitions are shielded from arbitrary cancellation and are either completed or left in a startup-reconcilable state.
- Compound operations acquire CPU, disk, network, process, and pipe capacity in one global order or through a deadlock-free compound allocator. Never hold a scarce permit while awaiting an earlier-order permit.
- Blocking tasks must be cooperatively bounded and joined; cancellation is not equated with dropping their future.
- A grace deadline escalates from cooperative stop to process-tree termination, with a structured receipt of unfinished tasks.

Use owned task sets such as `TaskTracker`/`JoinSet` patterns, but test the product state machine rather than assuming the container supplies correctness ([Tokio `TaskTracker`](https://docs.rs/tokio-util/latest/tokio_util/task/struct.TaskTracker.html), [Tokio `JoinSet`](https://docs.rs/tokio/latest/tokio/task/join_set/struct.JoinSet.html)).

## SR-009 — Cookie/session isolation needs an adapter-level contract

Lines 1056–1096 correctly call for RFC-like cookie scope and session partitions, but “RFC-like” is not testable. RFC 6265 distinguishes domain, host-only, path, Secure, and public-suffix behavior ([RFC 6265](https://www.rfc-editor.org/rfc/rfc6265.html)). yt-dlp's curl adapter leaked cookies because a raw `Cookie` header did not activate curl's cookie engine and was forwarded outside its intended scope ([GHSA-f7j3-774f-rfhj](https://github.com/yt-dlp/yt-dlp/security/advisories/GHSA-f7j3-774f-rfhj)).

Require RFC-scoped cookie jars, host-only preservation, public-suffix rejection, control-character rejection, Secure enforcement, and explicit SameSite policy if browser-equivalent behavior is claimed. Cross-origin redirects strip sensitive headers and recompute cookies from the destination jar. External adapters receive a scoped temporary jar or typed cookie callback, never a raw `Cookie` string. Browser-cookie import must use a consistent snapshot—not merely an unlocked read of a live SQLite database—and temporary copies must be access-restricted and cleaned up best-effort. Browser databases and unscoped credential stores may never be passed to JS/plugin workers.

## SR-010 — Archive identity and concurrent claims can cause false skips or duplicates

Lines 1412–1458 and 1831–1902 need an immutable, schema-versioned identity: extractor namespace, stable source ID, asset/variant discriminator, and a separate action/output namespace. Enforce uniqueness in SQLite, expose a dry-run identity explanation, and treat identity collisions as errors. Completed archive rows are written only after the final output commit is reconciled. In-flight work uses expiring owner/lease rows, not premature completed rows. Startup can steal an expired lease only after checking work/final fingerprints. Migrations are monotonic, transactional, and downgrade-aware.

For the speed-first local default, WAL with `synchronous=NORMAL` can be an explicit balanced profile only if the design admits that a recent transaction may be lost on sudden power failure; SQLite describes this as consistent but potentially non-durable. A durable profile should use `FULL` semantics ([SQLite `synchronous`](https://www.sqlite.org/pragma.html#pragma_synchronous)). Loss may cause a safe re-download, never a false skip.

## SR-011 — Live fan-out needs per-sink loss semantics

Lines 1424–1458 make bounded fan-out a goal but do not say which sink wins under pressure. The in-process Rust router should remain the speed-first default. The recording sink is lossless and authoritative. Player/preview sinks are explicitly lossy or disconnectable and cannot block recording. A slow host callback must be isolated behind a bounded adapter queue. If the lossless recorder cannot keep up or the disk fills, fail explicitly and preserve/reconcile a partial recording; never silently drop media. A dedicated worker process is justified only for an untrusted or blocking sink and is not the default architecture.

## SR-012 — Qualitative parser limits do not bound expansion

The limits at lines 1917–1933 need numeric per-job and global budgets for compressed and decompressed bytes, XML depth, attributes, entity behavior, timeline expansion count and duration, playlist recursion, redirect count, URI count/length, rendition count, key/init segment count, byte-range arithmetic, and live deduplication state. External XML entities must be disabled. All size/range arithmetic must be checked. Every nested URL passes through SR-004.

IPC frames for Wasm/worker adapters need bounded length prefixes, deadlines, message counts, nesting depth, and backpressure. A peer claiming a multi-gigabyte frame must be rejected before allocation. Fuzzing must assert time and allocation budgets and semantic invariants, not only absence of memory-unsafe crashes.

## SR-013 — Supply-chain policy is below release grade

Lines 1968–1985 require lockfiles and vulnerability response but leave source and artifact provenance open. `Cargo.lock` should be committed for the application and CI should use locked/frozen resolution ([Cargo lockfiles](https://doc.rust-lang.org/cargo/guide/cargo-toml-vs-cargo-lock.html)). Add a registry/source allowlist, prohibit floating git dependencies, run RustSec advisory checks, vet security-critical crates, apply license/source/duplicate-version policy, inventory `unsafe` and FFI, and generate an SBOM and release provenance ([RustSec](https://rustsec.org/), [cargo-vet](https://mozilla.github.io/cargo-vet/), [SLSA requirements](https://slsa.dev/spec/v1.2/requirements)).

FFmpeg distribution needs a pinned build, hash, feature/protocol inventory, signature/checksum policy, source/build provenance, and licensing record; FFmpeg's own legal page explains that build configuration affects licensing obligations ([FFmpeg legal](https://ffmpeg.org/legal.html)). Any rapid extractor/challenge update channel must be versioned, signed, rollback-capable, schema-bounded, and compatible with the host. Under the fixed constraint, built-in executable logic ships as Rust binary releases; separately updated data must remain non-executable, and third-party executable extensions use the approved capability-limited Wasm path.

## SR-014 — Acceptance criteria do not prove the contracts

The tests at lines 2055–2205 and acceptance criteria at lines 2518–2578 are broad, but they lack executable pass/fail thresholds for the dangerous boundaries. A design is not ready for parallel implementation while different implementers can choose incompatible meanings of “durable,” “atomic,” “sandboxed,” “bounded,” or “origin-bound.” The validation gates in `acceptance-hardening` below are required before a security/reliability PASS.

</topic>

<topic id="diff-attack-surfaces" status="complete" version="0.2.0" summary="Attack surfaces derived independently from the design">

# DIFF_ATTACK_SURFACES

There is no code diff. The reviewed change surface is the proposed v0.2.0 implementation contract compared with the fixed operator constraints and with the minimum behavior needed to make its claims true.

| Boundary | Attacker-controlled or failure-prone input | Security/reliability property at risk |
|---|---|---|
| URL resolver -> transport -> redirect | URLs, DNS answers, redirects, proxies, nested manifests | SSRF, credential scope, connection identity |
| Cookie store -> HTTP/external adapter | Imported browser cookies, redirect targets, adapter flags | Credential leakage and session crossing |
| Extractor/manifest -> output planner | Names, extensions, templates, paths, ranges | Traversal, executable output, overwrite, integer overflow |
| Fragment workers -> reorder buffer -> writer -> journal | Out-of-order completion, retries, cancellation, torn writes | Corruption, unbounded memory, invalid resume |
| Writer/journal -> rename -> archive DB | Power loss and concurrent jobs at each transition | Duplicate downloads, false skips, lost output, stale work |
| Rust planner -> FFmpeg | Executable path, argv, filter/list data, protocols, environment, handles | Injection, local-file/network access, orphaned processes |
| Rust host -> JS/plugin execution | Untrusted code, IPC frames, host imports, mutable global state | Sandbox escape, secret access, denial of service, cross-job leakage |
| Scheduler -> tasks/processes | Cancellation timing, compound permits, blocking calls | Deadlock, leaked work, oversubscription, partial commits |
| Live router -> recorder/player/host sinks | Slow consumer, disk full, disconnect | Silent media loss or global stall |
| Release -> dependency/tool/update chain | Crates, git sources, FFmpeg build, update bundle | Compromise, rollback, irreproducible behavior |
| Cross-platform filesystem/process layer | Reparse points, symlinks, network shares, Windows replace, Unix groups | Path escape, non-atomic commit, surviving descendants |

</topic>

<topic id="independent-checks-run" status="complete" version="0.2.0" summary="Checks performed without relying on design claims">

# INDEPENDENT_CHECKS_RUN

1. Opened the exact source design and traced networking, cookies, fragment ordering, scheduler, live sinks, JS, FFmpeg, plugins, persistence, security, tests, performance gates, risks, acceptance criteria, and open decisions. No verdict was based on the document title or summary.
2. Checked the current pinned yt-dlp release and its primary security advisories. The 2026.07.04 release includes a security fix for downstream command injection in link-file output, and the advisory history documents manifest-to-aria2c option injection, curl cookie leakage, dangerous output types, command-based netrc, and shell execution hazards ([yt-dlp 2026.07.04](https://github.com/yt-dlp/yt-dlp/releases/tag/2026.07.04), [yt-dlp advisories](https://github.com/yt-dlp/yt-dlp/security/advisories), [manifest/aria2c advisory](https://github.com/yt-dlp/yt-dlp/security/advisories/GHSA-vx4q-3cr2-7cg2)).
3. Checked Rust, Tokio, Linux, and Windows primary documentation for file creation/rename/sync, cancellation safety, process groups, process trees, reparse points, replacement semantics, and handle inheritance.
4. Checked SQLite primary documentation for atomic commit assumptions, WAL locality, network-filesystem risks, and `synchronous` durability tradeoffs.
5. Checked FFmpeg primary documentation for input protocols, protocol whitelisting, progress reporting, stdin, overwrite behavior, and distribution implications.
6. Checked Deno, QuickJS, and Wasmtime primary documentation for default permissions, static-module/cache caveats, host capability exposure, resource limits, and interruption.
7. Checked Rust fuzzing and concurrency-analysis tools for what they can and cannot establish: cargo-fuzz for coverage-guided fuzzing, Miri for many undefined-behavior/data-race checks with platform/FFI limits, and Loom for modeled interleavings with explicit incompleteness ([cargo-fuzz](https://rust-fuzz.github.io/book/), [Miri](https://github.com/rust-lang/miri/), [Loom](https://github.com/tokio-rs/loom)).

No executable product code, dependency manifest, build, or runtime was present in the assigned artifact. Therefore no implementation tests were run and no implementation-quality claim is made.

</topic>

<topic id="counterfactual-checks" status="complete" version="0.2.0" summary="Counterfactuals that falsify underspecified success claims">

# COUNTERFACTUAL_CHECKS

1. **If rename succeeds and the machine loses power before archive commit**, the final file exists but the archive says it does not. Without reconciliation, a later run redownloads, overwrites, or conflicts. Therefore rename-plus-archive cannot be described as one atomic completion.
2. **If the archive commits before the output name is durably committed**, a restart can skip an output that does not exist. Therefore archive completion may never lead filesystem completion.
3. **If a path is canonicalized and later reopened by pathname**, an attacker or concurrent process can replace a parent with a symlink/reparse point in between. Therefore string-prefix/root checks do not prove containment.
4. **If a journal checkpoint is synced before the associated contiguous media prefix**, a power loss can preserve the checkpoint while losing media bytes. Therefore append-only journaling alone does not prove resumability.
5. **If SSRF policy is applied only to the submitted URL**, a permitted public host can redirect or resolve to a link-local/private target. Therefore every hop and resolved address requires validation.
6. **If a raw Cookie header is passed to an external downloader**, its redirect behavior can forward it beyond cookie domain/path scope. This has already occurred in yt-dlp's curl integration. Therefore cookies must remain typed/scoped through adapters.
7. **If only the FFmpeg child PID is killed**, a spawned descendant can survive and retain files/pipes. Therefore process-tree ownership is mandatory.
8. **If a persistent execution context is reused across jobs**, mutable global state can carry tokens, cookies, or challenge state into another session. Therefore executable contexts must be per-job/per-request even when immutable code caches are shared.
9. **If a slow player shares lossless backpressure with the recorder**, a preview stall can stop recording. If it shares a lossy queue without class-specific rules, recording may silently drop segments. Therefore each sink needs an explicit loss class.
10. **If SQLite WAL is placed on SMB/NFS because the output lives there**, the design violates SQLite's same-host shared-memory assumption. Therefore output location and archive location require separate policy.

</topic>

<topic id="boundary-probes" status="complete" version="0.2.0" summary="Boundary cases required to verify the design">

# BOUNDARY_PROBES

| Probe | Boundary condition | Expected result |
|---|---|---|
| Commit-prefix crash matrix | Kill after every write, sync, `PREPARED`, rename, directory sync, archive transaction, `ARCHIVED`, and cleanup step | Restart produces exactly one valid final file, no false skip, and no silent overwrite. |
| Destination conflict | Matching file, mismatched file, locked Windows file, destination directory, case-fold collision | Deterministic reuse/reject/quarantine result; never implicit replacement. |
| Path race | Swap every output parent to a symlink/reparse point after validation and before create/rename | Operation remains beneath the trusted root or fails closed. |
| Reorder limit | Completion arrives in reverse order just beyond configured buffer/spill limits | Memory remains bounded; disk spill is bounded; upstream is backpressured without deadlock. |
| Source mutation | ETag/range/length changes after a partial download | Resume rejects the prefix and restarts or preserves it as a diagnosed partial. |
| Process tree | FFmpeg test helper spawns child and grandchild holding handles | Cancellation terminates the entire tree, drains pipes, waits, and releases files. |
| Redirect/DNS | Public URL redirects or re-resolves to loopback, RFC 1918, IPv6 local, or link-local metadata | Request is rejected before protected destination access; sensitive headers are absent. |
| IPC allocation | Peer declares a frame at the maximum boundary and one byte over it | Maximum valid frame is bounded/processed; oversized frame is rejected before allocation. |
| Live sink | Recorder healthy while player stalls; then recorder disk reaches ENOSPC | Player drops/disconnects without blocking recording; recorder failure is explicit and partial state is recoverable. |
| Filesystem matrix | Same tests on supported NTFS/ReFS/ext4/APFS and declared SMB/NFS modes | Claims are enabled only where the measured capability contract passes. |

</topic>

<topic id="negative-path-checks" status="complete" version="0.2.0" summary="Adversarial and failure-path tests required">

# NEGATIVE_PATH_CHECKS

- Inject short writes, `ENOSPC`, permission loss, antivirus/file-lock interference, sync failure, rename conflict, and corrupted/torn journal tails.
- Race two jobs with the same archive identity and final path; kill the lease owner; advance the clock; recover without duplicate completion or permanent false skip.
- Feed an M3U8/MPD/MSS document containing newline/option-injection payloads, cyclic includes, huge timelines, nested redirects, extreme byte ranges, decompression bombs, and millions of unique live segment IDs.
- Redirect authenticated requests across scheme, port, registrable domain, and IP class boundaries; confirm cookies and all sensitive custom headers are stripped/recomputed.
- Attempt `file:`, local device, named-pipe/Unix-socket, FTP-like, and FFmpeg-supported network protocols through manifests, concat/list files, subtitles, thumbnails, and postprocessing inputs.
- Send malformed progress lines, unterminated lines, invalid UTF-8, massive stderr, early pipe close, hang, crash, and child/grandchild escape from the FFmpeg test helper.
- Attempt Wasm host-import access not granted by capability, infinite loops, memory/table growth, output floods, nested-call recursion, and cross-job state retrieval.
- Cancel while awaiting each known non-cancellation-safe operation, while holding each permit combination, during journal sync, during rename, and during archive commit.
- Corrupt or roll back the archive schema, reuse an identity for a different asset, and present a final file whose recorded digest does not match.
- Import cookies from a live/locked browser database and during concurrent browser updates; confirm a consistent snapshot or a clean failure without leaking the database.

</topic>

<topic id="acceptance-hardening" status="complete" version="0.2.0" summary="Executable gates needed before implementation readiness">

# Required acceptance hardening

The following gates should be promoted into normative acceptance criteria:

1. **Crash consistency:** deterministic crash injection at every persistence transition proves the SR-001 state machine on every supported local filesystem.
2. **Filesystem faults:** injected short write, disk full, sync error, locked destination, preexisting destination, cross-volume destination, and torn journal recover without silent corruption or false archive completion.
3. **Confinement:** symlink/reparse races and path-template fuzzing cannot create, replace, or inspect outside the approved root.
4. **SSRF and credentials:** redirect, DNS-rebinding, proxy, nested-manifest, and external-adapter tests block protected destinations and strip/recompute scoped secrets.
5. **FFmpeg supervision:** absolute executable selection, protocol whitelist, scrubbed environment, bounded output parsing, and child/grandchild termination pass on Windows and Unix.
6. **Execution capability:** every denied JS/plugin/Wasm capability has a negative test; memory, CPU, IPC, and output limits terminate cleanly. The Rust-plus-FFmpeg dependency conflict is resolved before selecting JS/browser-impersonation technology.
7. **Concurrency:** model key journal/archive/lease/queue state machines with Loom-compatible abstractions where practical, stress real Tokio tasks, and prove no detached task or leaked permit after cancellation. Loom is supporting evidence, not a completeness proof.
8. **Memory safety:** fuzz parsers and IPC with allocation/time invariants; run Miri over unsafe and concurrency-sensitive pure-Rust components; inventory and separately test FFI/OS boundaries.
9. **Live reliability:** a stalled lossy sink cannot block the lossless recorder; disk-full and network-loss paths yield explicit, resumable partial state.
10. **Supply chain:** CI rejects unlocked or unapproved dependency sources, known RustSec issues without explicit exception, unvetted critical crates, unsigned/unfingerprinted release tools, and unverifiable update bundles.
11. **Platform truth:** no cross-platform or network-filesystem support claim is accepted without passing its capability row. Unsupported/degraded configurations fail early with a precise diagnostic.
12. **Performance with correctness:** speed gates run only after correctness gates. A faster path that weakens URL policy, durability profile, cookie scope, parser bounds, or cancellation cleanup fails regardless of throughput.

</topic>

<topic id="peer-review-question-resolutions" status="complete" version="0.2.0" summary="Security and reliability resolutions for open design questions">

# Peer-review question resolutions

| Question | Resolution from this lens |
|---:|---|
| 1 | Pin a yt-dlp behavioral differential baseline, but maintain an independent normative semantic/security specification. Unsafe yt-dlp behavior is not copied for parity. |
| 2 | Extensibility maps must be schema-, size-, depth-, and type-bounded. Secrets, paths, command arguments, and capabilities may not hide in untyped extension data. |
| 3 | Canonical third-party plugin isolation: capability-limited Wasm hosted by Rust. Native/in-process arbitrary plugins are rejected; process helpers require exceptional approval and OS containment. |
| 4 | The transport abstraction is sufficient only if destination/SSRF policy and origin-specific credential scope are mandatory inputs that no backend can bypass. |
| 5 | Browser impersonation is a release blocker only for declared extractor flows that require it. A support claim cannot pass without its required transport; any non-Rust native dependency requires explicit operator resolution. |
| 6–7 | Deno/QuickJS cannot be selected under the fixed Rust-plus-FFmpeg production constraint without operator resolution. The current sandbox capabilities are insufficiently enforceable regardless of engine. |
| 8 | The fragment pipeline is directionally correct but not ready until durable-prefix ordering, torn-journal framing, source identity validation, and profile loss bounds are normative. |
| 9 | Local filesystems can be supported with platform-specific commit protocols. Direct SQLite/WAL on network shares is not acceptable by default; remote-output durability must be explicitly degraded and tested. |
| 10 | Resource classes are directionally correct, but mux/transcode operations need compound CPU/disk/process/pipe permits with deadlock-free acquisition. |
| 11 | Logical source order is the deterministic default for user-visible collection records. Real-time completion order can be optional when every record retains its logical index. |
| 12 | Performance gates are insufficient without correctness and workload-specific resource budgets. Security/durability profiles must not be silently relaxed to pass speed targets. |
| 13 | Replace shell `--exec`/`--netrc-cmd`, arbitrary downloader command/args, unrestricted cookie forwarding, unsafe extension output, and arbitrary native plugins with typed Rust/Wasm APIs. Do not preserve these as production compatibility behavior. |
| 14 | Replacement confidence cannot be a single extractor percentage. Every declared supported extractor and mandatory compatibility corpus must pass safety/reliability contracts; long-tail coverage is reported separately. |
| 15 | Offer a bundled pinned/verifiable FFmpeg per platform or a configured absolute external path. Bare current-directory/PATH discovery is not the default. Record build, capabilities, hash, and provenance. |
| 16 | Put security boundaries—URL policy, path confinement, journal/commit, cookie scope, FFmpeg supervision, extension host—behind narrow crates/modules. Split other crates only when ownership and compile-time evidence justify it. |
| 17 | Formal state machines are required for commit/journal/archive, fragment/reorder/durable checkpoint, cancellation/resource ownership, live refresh/fan-out, FFmpeg/plugin worker lifecycle, and collection pagination/archive leases. |
| 18 | Fuzz first: M3U8/MPD/MSS, templates/selectors/paths, journal decoding, and plugin/IPC; then cookies, subtitles, JSON normalization, and FFmpeg progress. Assert allocation/time/semantic properties. |
| 19 | Production support does not use Python-compatible or legacy runtime bridges. Migration compatibility is achieved through Rust-native input/behavior adapters, not a Python runtime. |
| 20 | Built-in executable extractor logic ships in signed Rust releases. Rapid data/challenge bundles must be non-executable, signed, version-compatible, staged, rollback-capable, and schema-bounded. Executable third-party updates use the approved Wasm capability boundary. |
| 21 | Keep a shared internal `SourceResult`, but expose distinct typed top-level resolve/download, collection, and live entrypoints so archive, ordering, cancellation, and sink policies cannot be confused. |
| 22 | Apply filters as early as authenticated metadata permits without extra asset requests; predicates requiring asset headers/content run later. Filtering may not trigger unbounded or policy-bypassing fetches. |
| 23 | SQLite is the correct local single-machine default after immutable identity, unique completion, lease, migration, and commit-order contracts are specified. It is not the direct network-share default. |
| 24 | Use an in-process bounded Rust fan-out for trusted fast paths. Isolate only untrusted/blocking sinks; the lossless recorder remains authoritative. |
| 25 | Preserve the full protocol and feature scope. Complete HLS/DASH/MSS/custom-range correctness first. Real-time mux remains an opt-in capability after record-then-mux recovery is proven; it is not silently removed or made a release-safety dependency. |

</topic>

<topic id="research-basis" status="complete" version="0.2.0" summary="Primary evidence and selected field-aligned controls">

# Research basis

The review selected controls that are directly supported by primary project, standards, runtime, database, operating-system, and tool documentation. In particular, yt-dlp's own advisories show why Ferric Forager should preserve useful behavior without reproducing command/downloader/cookie hazards; SQLite documentation establishes the local-WAL boundary; OS APIs establish path and process-tree controls; FFmpeg documentation establishes protocol and process-interface risk; and runtime documentation establishes that language-level limits are not equivalent to host isolation.

Rejected options:

- “No shell” as the complete FFmpeg safety story.
- Canonicalize/check/open as a filesystem confinement implementation.
- Treating rename plus SQLite insert as one atomic completion.
- Raw cookie/header forwarding through external adapters.
- Direct SQLite WAL on network shares.
- A persistent mutable JS context shared across jobs.
- Arbitrary native or process plugins as the default extension model.
- Performance parity as evidence of correctness or security.
- Selecting Deno, QuickJS, or a native browser-impersonation library without resolving the Rust-plus-FFmpeg production constraint.

High-ROI additions are the explicit state machines and fault-injection harnesses: they reuse the design's existing journal, archive, scheduler, typed FFmpeg planner, bounded queues, and test architecture; they prevent divergent implementations and make later parallel agent work reproducible.

</topic>

<topic id="verification-separation" status="complete" version="0.2.0" summary="Separate verdicts by evidence class">

# Verification separation

| Evidence class | Verdict | Basis |
|---|---|---|
| Tests | **NOT RUN / NOT AVAILABLE** | The reviewed artifact is a design document; no product implementation or test suite was in scope. |
| Specification alignment | **FAIL** | The design leaves security/reliability contracts and the Rust-plus-FFmpeg dependency conflict unresolved. |
| Design quality | **CONDITIONAL** | Strong architecture direction, but critical boundary behavior remains qualitative or optional. |
| Environment portability | **FAIL** | Windows replacement/process-tree semantics and network-filesystem capability claims are not specified or gated. |
| Final implementation readiness | **FAIL** | Critical findings SR-001 through SR-005 and high findings SR-006 through SR-014 remain unresolved. |

</topic>

<topic id="residual-uncertainty" status="complete" version="0.2.0" summary="Evidence unavailable in the design-only review">

# RESIDUAL_UNCERTAINTY

- No Rust workspace, dependency graph, `Cargo.lock`, unsafe/FFI inventory, or transport implementation was available, so actual dependency and memory-safety exposure is not inspected.
- No FFmpeg build, discovery mechanism, capability list, or distribution artifact was available, so tool provenance and protocol surface are not inspected.
- No implemented journal/archive state machine exists to crash-test; the proposed recovery contracts remain unproven.
- Network-filesystem guarantees vary by server, client, mount options, and failure model. They cannot be generalized from local filesystem behavior.
- Browser impersonation technology is unselected. Its ability to satisfy both extractor needs and the fully-Rust constraint is unverified.
- A Rust-only implementation of the required challenge/JS behavior has not been demonstrated. This must remain an explicit compatibility gap rather than being silently filled with another runtime.
- Wasm/plugin ABI, host imports, and update signing are not defined, so extension isolation remains a design recommendation rather than proven behavior.

These uncertainties do not soften the verdict. They identify the exact evidence needed for a later PASS.

</topic>
