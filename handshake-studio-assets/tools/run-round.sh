#!/usr/bin/env bash
# Studio round entrypoint: the single checked-in build + test invocation for validator rounds (CX-VAL-007).
# Usage: tools/run-round.sh --results DIR --target-dir DIR [--dev] [--canary] (--all | CRATE...)
#   CRATE      hsk-studio-<name> or <name>; --all = every workspace member.
#   --dev      allow the git edit tree; otherwise the workspace must be an export with ROUND_IDENTITY.json
#              in its parent or grandparent whose manifest_sha256 (sorted "<sha256>  <relpath>\n" lines over
#              the export dir beside it, named src/) matches. Exports get current mtimes on all workspace files
#              (no stale cross-tree artifact reuse) and run cargo with --locked. The results dir must lie inside
#              the export owner dir (the dir holding ROUND_IDENTITY.json) and outside src/, so a run never writes
#              into another owner's export (validators: studio-round/; steward: studio-team-dev/steward-export/).
#   --canary   nextest units run the first listed test of every package (profile "canary"); doctests run in full.
# Step 0 (export, read-only, ~1 s): `cargo metadata --locked --offline` and `tools/registry-sync.py` check mode;
#   drift exits 75 (infrastructure: the candidate needs a steward sync commit; nothing is rewritten here).
#   Policy (both modes, read-only, before any build; hits with file:line in step0.policy.{json,txt}):
#   V4-03 machine-local paths: every text file under crates/*/src and crates/*/tests, per line, drive-letter path
#     ((?<![A-Za-z0-9])[A-Za-z]:/ or [A-Za-z]:\ not followed by a quote, so the escaped quote of CSS `a:\"` in a
#     Rust string is not a path) or Users/ segment; a hit beyond .config/machine-paths.toml [pinned] counts
#     (test data never opened as a host path) exits 72. Host data comes only via .config/oracle-corpus.toml.
#   V4-05 frozen duplicates: .config/duplicate-fingerprints.toml patterns over crates/*/src; a match in a file
#     outside that fingerprint's `allowed` sites (D16 ZIP/CRC-32, D18 worker supervision, timecode) exits 71.
# Units (each its own result; all run; a unit's build failure skips only that unit's test run):
#   prebuild   [prebuild] of .config/round-features.toml: `cargo build -p PKG --bin BIN [--features F]` into this run's
#              target right before the named features unit, for worker binaries its tests spawn from <target>/debug
#              (prebuild/UNIT+BIN.*; a failure is recorded as that unit's build failure; F-R32-2).
#   default    union over the selection, default features: one build (nextest --no-run) + one run (--no-fail-fast);
#              files at the results root (build.*, run.*, list.txt, canary-filter.txt, junit.xml) as before.
#              Per-test timeouts: .config/nextest.toml slow-timeout (CX-EXEC-005); a junit <failure type="test
#              timeout"> is reported timed_out (round.json timed_out_tests), not failed (aggregator self-check).
#   features   per selected crate and feature set declared in .config/round-features.toml: -p CRATE --features SET,
#              own build + run under features/CRATE+F1+F2/. A set naming a feature (or crate) the candidate lacks is
#              recorded feature_absent (crate_absent) and fails the round with exit 74 (declared unit absent).
#   ignored    per selected crate and feature set declared in [ignored] of .config/round-features.toml (an empty set =
#              default features): -p CRATE [--features SET] --run-ignored only, own build + run under
#              ignored/CRATE[+F1...]/ (the #[ignore]d oracle-corpus tests; absent sets as for features, exit 74).
#              --canary builds these units without running them (corpus tests exceed the canary profile's cap).
#   doctest   `cargo test --doc` per selected crate with a doctested lib target, default features and every
#              declared set (the per-test runner does not run doctests): doctest/CRATE[+F1...].{stdout,stderr}.txt.
#              Per-test timeout: doctests run serially (--test-threads=1), so libtest prints "test NAME ... " when a
#              test starts; a watchdog kills the unit's process tree when one test stays unfinished longer than
#              [doctest] per-test-timeout-s (.config/round-features.toml), appends TIMEOUT to that line, reports the
#              unit's doctests without a verdict NOT_RUN, and the unit's run exit is 124. Compiling the crate before
#              its first doctest is not covered.
#   fixture    .config/oracle-fixtures.toml, built before the other units when a `needed_by` crate is selected:
#              cargo build --offline --locked of the standalone fixture package into <target-dir>/fixtures/<id> (the
#              run's own target; byte-reproducible across target dirs, export roots and env: --config pins debug=0,
#              strip="symbols" (no PDB path), incremental=false and, on Windows, link-arg /Brepro (no PE timestamp),
#              so concurrent owners place the same sha256 and never rewrite MANIFEST.json under each other), then placed under the oracle corpus root at the content-addressed
#              <dest>/sha256/<library sha256>/<file> (written only when absent or different; never replaces other
#              bytes) with <dest>/MANIFEST.json (library = that relative path, sha256, source sha256, round sha;
#              atomic replace), where tests find it via .config/oracle-corpus.toml (no env vars). The corpus is
#              hashed AFTER placement; round.json "fixtures" binds library path, sha256 and MANIFEST.json sha256 and
#              re-verifies both after the units ("binding_intact"; changed = exit 76). A build failure counts as a
#              unit build failure; an absent corpus root is recorded corpus_absent (the tests then report NOT_PROVEN).
# Disk-wide build lock (CX-984-002, tools/disk-build-lock.sh protocol @2): the round queues with priority (class
#   round) and holds the lock for every cargo unit (fixture build included); between units, after holding it for
#   STUDIO_DISK_BUILD_LOCK_ROUND_SLICE_S (default 300) s, it yields one gap to the first queued builder and re-queues
#   first. Root from STUDIO_DISK_BUILD_LOCK_DIR or .config/disk-build-lock.toml (they must agree), resolved from the
#   TARGET dir: under the sole artifact root -> its lock; under the granted SSD root (ssd-search) -> the SSD lock.
#   A target dir outside both roots, or results outside the sole artifact root, is refused (exit 2). TMP/TEMP and the
#   test-runner store stay under the sole artifact root for both (CX-984-014: the grant covers the target only).
#   waits print to stderr; results/disk-lock.json and round.json "disk_lock" record holds, yields, waits and broken
#   stale locks; giving up (STUDIO_DISK_BUILD_LOCK_TIMEOUT_S, default 7200) or a protocol mismatch exits 75.
# GPU test lock (SMB-STEWARD-GPU-CONTENTION 2026-10-20; dbl_gpu_run in tools/disk-build-lock.sh): a unit whose selection
#   includes hsk-studio-render-gpu (or --workspace) runs its test step holding the machine-wide GPU test lock (class
#   round = queue priority; root STUDIO_GPU_TEST_LOCK_DIR or <sole artifact root>/.gpu-test-lock), so GPU tests of the
#   other build slot never overlap; the build step stays outside it. Giving up waiting is the unit's run status 75.
# Tree binding: <target-dir>/.studio-workspace-root binds the target dir to ONE workspace root (`cargo
#   locate-project --workspace` output; the team's studio-env.sh uses the same marker); another root is refused
#   (exit 2), because trees sharing a target reuse each other's artifacts by mtime.
# NOT_PROVEN: a test that cannot prove its claim on this host (no GPU adapter, oracle input absent, ...) prints a
#   line starting `NOT_PROVEN:` (leading spaces allowed) followed by the reason, and may pass. Every such line in a
#   nextest unit's captured output (junit system-out/system-err, stored for passing tests) is collected into
#   round.json "not_proven": [{unit, crate, binary, test, reason}], the unit's not_proven count and tests.json; an
#   otherwise green round then exits 73 (never 0). Doctest output is not captured per test: doctests cannot report it.
# Oracle corpus: commit-free external test DATA, located without env vars by .config/oracle-corpus.toml (walk up
#   from the workspace root to the first ancestor holding its `search` path; the edit tree and every export under
#   Handshake_Artifacts resolve the same directory). round.json "oracle_corpus" records root, file count and a
#   sha256 manifest (sorted "<sha256>  <relpath>" lines) so oracle evidence is bound to corpus content.
# Results: round.json (identity, env, every unit with exits and passed/failed/skipped/timed_out/not_run/not_proven
#   counts, per-crate counts, not_proven, oracle_corpus), tests.json (every reported test: unit, crate, binary,
#   name, status, seconds, not_proven reasons; passing names included), junit.xml per nextest unit (passing tests'
#   output stored, skipped tests listed: .config/nextest.toml), metadata.json (cargo metadata --no-deps),
#   units.tsv, oracle-corpus.json, step0.*.txt, nextest-store.toml. Once the results dir exists, round.json is
#   always written: on refusal, step 0 drift, interruption or aggregation failure it is a minimal record with
#   "complete": false, the exit code and the reason (raw files kept).
# Env: sets CARGO_TARGET_DIR, TMP/TEMP/TMPDIR (<target-dir>/../tmp; for an SSD target <sole artifact root>/<owner path>/tmp,
#   owner path = the target dir's parent below the SSD WP dir), CARGO_PROFILE_{DEV,TEST}_DEBUG,
#   CARGO_TERM_COLOR; unsets every HSK_* variable. Nothing else is required from the caller (python 3.11+).
#   CARGO_INCREMENTAL=0 (lead decision 2026-10-22, proposal P1): rounds never keep incremental state; the union rebuilds every workspace crate anyway, so incremental dirs are pure disk waste (268 GiB purged 2026-10-21).
# Exit: 2 refused, 75 step 0 drift or disk build lock wait gave up (infrastructure), 72 step 0 machine-local path
#   (V4-03), 71 step 0 new frozen-duplicate site (V4-05), 70 results aggregation failed, else the first failing unit
#   build's exit code (default unit first), else the first failing test run's exit code (124 = doctest per-test
#   timeout), else 76 when an oracle fixture binding changed during the run, else 74 when a declared feature-matrix
#   unit is absent at the candidate, else 73 when any test reported NOT_PROVEN, else 0. Validators gate on
#   round.json "exit" (== process exit), "not_proven" and "fixtures"[].binding_intact.
set -euo pipefail

reason=""
die() { reason="refused: $*"; echo "run-round: $*" >&2; exit 2; }
jstr() { local s=${1//\\/\\\\}; s=${s//\"/\\\"}; s=${s//$'\n'/\\n}; s=${s//$'\r'/}; s=${s//$'\t'/\\t}; printf '"%s"' "$s"; }
# Absolute, normalized, no filesystem side effects (mixed C:/ form on Windows Git Bash).
abspath() { local p; p=$(realpath -m -- "$1"); command -v cygpath >/dev/null && p=$(cygpath -m "$p"); printf '%s' "$p"; }
under() { case "$1/" in "$2"/*) return 0;; *) return 1;; esac; }

dev=0 canary=0 all=0 results="" target="" crates=()
while [ $# -gt 0 ]; do
  case "$1" in
    --dev) dev=1 ;;
    --canary) canary=1 ;;
    --all) all=1 ;;
    --results) results=${2:?--results needs a dir}; shift ;;
    --target-dir) target=${2:?--target-dir needs a dir}; shift ;;
    -h|--help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; exit 0 ;;
    -*) die "unknown option $1" ;;
    hsk-studio-*) crates+=("$1") ;;
    *) crates+=("hsk-studio-$1") ;;
  esac
  shift
done
[ -n "$results" ] && [ -n "$target" ] || die "--results and --target-dir are required"
{ [ $all -eq 1 ] && [ ${#crates[@]} -eq 0 ]; } || { [ $all -eq 0 ] && [ ${#crates[@]} -gt 0 ]; } \
  || die "give --all or a crate list (not both)"
py=$(command -v python || command -v python3) || die "python 3.11+ required"
"$py" -c 'import sys; sys.exit(sys.version_info < (3, 11))' || die "python 3.11+ required (tomllib)"

root=$(abspath "$(dirname "${BASH_SOURCE[0]}")/..")
target=$(abspath "$target")
results=$(abspath "$results")
# Disk-wide build lock (tools/disk-build-lock.sh). The round holds it for its whole run, so its cargo calls (and the
# python fixture build) use the real cargo: drop a cargo wrapper function a caller may have exported.
source "$root/tools/disk-build-lock.sh"
unset -f cargo 2>/dev/null || true
lock_root="${STUDIO_DISK_BUILD_LOCK_DIR:-}"
# Prints three lines: the lock root for the TARGET dir's disk, the sole artifact root (`search` from the workspace
# root) and the owner path below the SSD WP dir (empty for a target under the sole artifact root).
cfg_out=$("$py" -X utf8 -I - "$root" "$root/.config/disk-build-lock.toml" "$target" <<'PY'
import pathlib, re, sys
root, cfg, target = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]), pathlib.Path(sys.argv[3])
if not cfg.exists():
    sys.exit(0)
kv = {}
for line in cfg.read_text(encoding="utf-8").splitlines():
    m = re.fullmatch(r'\s*([A-Za-z0-9_-]+)\s*=\s*(?:"([^"]*)"|(\d+))\s*', line)
    if m:
        kv[m.group(1)] = m.group(2) if m.group(2) is not None else int(m.group(3))
    elif line.strip() and not line.lstrip().startswith("#"):
        sys.exit(f"run-round: disk-build-lock.toml: not a `key = value` line: {line!r}")
if kv.get("schema") != "hsk.studio.disk-build-lock@1" or not all(isinstance(kv.get(k), str) for k in ("search", "lock-dir")) \
        or not isinstance(kv.get("max-ancestors"), int):
    sys.exit("run-round: disk-build-lock.toml needs schema hsk.studio.disk-build-lock@1, search, lock-dir, max-ancestors")
for k in ("search", "lock-dir", "ssd-search", "ssd-lock-dir"):
    if k not in kv:
        continue
    if not isinstance(kv[k], str):
        sys.exit(f"run-round: disk-build-lock.toml {k} must be a string")
    p = pathlib.PurePosixPath(kv[k])
    if p.is_absolute() or ".." in p.parts or ":" in kv[k]:
        sys.exit(f"run-round: disk-build-lock.toml {k} must be a relative path without '..'")
n = kv["max-ancestors"] + 1
def walk(start, rel):
    return next((a / rel for a in [start, *start.parents][:n] if (a / rel).is_dir()), None)
art = walk(root, kv["search"])                      # sole artifact root (WP dir), from the workspace root
if art is None:
    sys.exit(0)
def inside(p, top):
    try:
        p.resolve().relative_to(top.resolve()); return True
    except ValueError:
        return False
t_art = walk(target, kv["search"])
t_ssd = walk(target, kv["ssd-search"]) if "ssd-search" in kv else None
owner_rel = ""
if t_art is not None and inside(target, t_art) and t_art.resolve() == art.resolve():
    lock = art / kv["lock-dir"]
elif t_ssd is not None and inside(target, t_ssd) and "ssd-lock-dir" in kv:
    lock = t_ssd.parent / kv["ssd-lock-dir"]
    # the owner path below the SSD WP dir is mirrored on the sole root for TMP/TEMP and the test-runner store
    rel = target.resolve().parent.relative_to(t_ssd.resolve()).as_posix()
    if rel in ("", ".") or not all(re.fullmatch(r"[A-Za-z0-9._-]+", s) and s not in (".", "..") for s in rel.split("/")):
        sys.exit(f"run-round: SSD target dir needs <ssd root>/<owner path>/target with plain segments, got {target.as_posix()}")
    owner_rel = rel
else:
    sys.exit(f"run-round: target dir {target.as_posix()} is neither under {art.as_posix()} nor under the granted SSD root ({kv.get('ssd-search')})")
print(lock.as_posix()); print(art.as_posix()); print(owner_rel)
PY
) || die "invalid .config/disk-build-lock.toml or target dir outside the build roots"
cfg_lock=$(printf '%s\n' "$cfg_out" | sed -n 1p) art_root=$(printf '%s\n' "$cfg_out" | sed -n 2p) owner_rel=$(printf '%s\n' "$cfg_out" | sed -n 3p)
if [ -n "$lock_root" ] && [ -n "$cfg_lock" ] && [ "$(abspath "$lock_root")" != "$(abspath "$cfg_lock")" ]; then
  die "STUDIO_DISK_BUILD_LOCK_DIR=$lock_root differs from the .config/disk-build-lock.toml root $cfg_lock"
fi
lock_root=${lock_root:-$cfg_lock}
[ -n "$lock_root" ] || die "no disk build lock root (.config/disk-build-lock.toml search not found; or set STUDIO_DISK_BUILD_LOCK_DIR)"
[ -n "$art_root" ] || die "sole artifact root not found from $root (.config/disk-build-lock.toml search)"
lock_root=$(abspath "$lock_root")
art_root=$(abspath "$art_root")
gpu_root=$(abspath "${STUDIO_GPU_TEST_LOCK_DIR:-$art_root/.gpu-test-lock}")
for p in "$target" "$results"; do
  under "$p" "$root" && die "refusing path inside the workspace: $p"
done
under "$results" "$art_root" || die "results dir must lie under the sole artifact root $art_root (CX-984-001)"
if [ -z "$owner_rel" ]; then
  tmp_dir="$target/../tmp" store_dir="$target/nextest" target_class=artifact-root
else
  # e.g. C:/HandshakeBuild/WP-KERNEL-STUDIO/studio-round/target -> <art>/studio-round/{tmp,nextest};
  #      .../studio-team-dev/steward-export/target -> <art>/studio-team-dev/steward-export/{tmp,nextest}
  tmp_dir="$art_root/$owner_rel/tmp" store_dir="$art_root/$owner_rel/nextest" target_class=ssd
fi
[ -e "$results/round.json" ] && die "results dir already holds a round.json: $results"

# Identity: export (default) or edit tree (--dev).
identity="" identity_sha="" identity_tree="" manifest="" dirty="" owner_dir=""
for cand in "$root/../ROUND_IDENTITY.json" "$root/../../ROUND_IDENTITY.json"; do
  if [ -f "$cand" ]; then identity="$(abspath "$(dirname "$cand")")/ROUND_IDENTITY.json"; break; fi
done
if [ $dev -eq 0 ]; then
  [ -n "$identity" ] || die "not an export (no ROUND_IDENTITY.json beside it); use --dev for the edit tree"
  git -C "$root" rev-parse --git-dir >/dev/null 2>&1 && die "workspace is a git checkout, not an export"
  owner_dir=$(dirname "$identity")
  export_dir="$owner_dir/src"
  [ -d "$export_dir" ] && under "$root" "$export_dir" || die "workspace is not inside $export_dir"
  under "$results" "$export_dir" && die "results dir must be outside the export"
  under "$results" "$owner_dir" || die "results dir must be inside the export owner dir $owner_dir (never write into another owner's export)"
  field() { sed -n "s/.*\"$1\": *\"\([0-9a-f]*\)\".*/\1/p" "$identity" | head -n1; }
  identity_sha=$(field sha) identity_tree=$(field tree)
  manifest=$(cd "$export_dir" && find . -type f -printf '%P\0' | LC_ALL=C sort -z | xargs -0 sha256sum \
    | sed 's/^\([0-9a-f]\{64\}\) [ *]/\1  /' | sha256sum | cut -c1-64)
  [ "$manifest" = "$(field manifest_sha256)" ] || die "export manifest $manifest != ROUND_IDENTITY manifest_sha256"
  locked=(--locked)
else
  identity_sha=$(git -C "$root" rev-parse HEAD 2>/dev/null || echo unknown)
  identity_tree=$(git -C "$root" rev-parse 'HEAD^{tree}' 2>/dev/null || echo unknown)
  dirty=$(git -C "$root" status --porcelain 2>/dev/null | wc -l | tr -d ' ')
  locked=()
fi
mkdir -p "$target" "$results"
started=$(date -u +%Y-%m-%dT%H:%M:%SZ)

# From here on round.json is always written: the aggregator writes the full record; any other exit leaves a
# minimal record (complete=false, exit, reason) so a stalled or refused run is never silent.
finish() {
  local rc=$?
  dbl_release
  [ -e "$results/round.json" ] && return
  {
    printf '{\n "schema": "hsk.studio.round-run@2",\n "complete": false,\n "exit": %s,\n "reason": %s,\n' "$rc" "$(jstr "${reason:-exit $rc}")"
    printf ' "mode": %s,\n "selection": %s,\n' "$(jstr "$([ $dev -eq 1 ] && echo dev || echo export)")" "$(jstr "$([ $all -eq 1 ] && echo --all || echo "${crates[*]}")")"
    printf ' "sha": %s,\n "tree": %s,\n "identity_file": %s,\n' "$(jstr "$identity_sha")" "$(jstr "$identity_tree")" "$(jstr "$identity")"
    printf ' "started_at": %s,\n "ended_at": %s,\n' "$(jstr "$started")" "$(jstr "$(date -u +%Y-%m-%dT%H:%M:%SZ)")"
    printf ' "units_file": "units.tsv",\n "target_dir": %s,\n "target_class": %s\n}\n' "$(jstr "$target")" "$(jstr "${target_class:-}")"
  } >"$results/round.json"
  echo "run-round: incomplete (exit $rc: ${reason:-exit $rc}) -> $results/round.json" >&2
}
trap finish EXIT
trap 'reason="interrupted"; exit 130' INT TERM HUP

# Tree binding: Cargo names workspace-member artifacts by their path relative to the workspace root, so two trees
# building into one target dir reuse each other's artifacts by mtime (stale artifacts). The target dir is bound to
# ONE workspace root by <target>/.studio-workspace-root (what `cargo locate-project --workspace` prints; the
# team's studio-env.sh uses the same marker); a different root is refused.
ws_manifest=$(cd "$root" && cargo locate-project --workspace --message-format plain 2>/dev/null) \
  || die "cargo locate-project failed in $root"
if [ -f "$target/.studio-workspace-root" ]; then
  bound=$(cat "$target/.studio-workspace-root")
  [ "$bound" = "$ws_manifest" ] || die "target dir $target is bound to $bound, not $ws_manifest (use one target dir per tree)"
else
  printf '%s' "$ws_manifest" >"$target/.studio-workspace-root"
fi

# Environment: only what this script sets.
unset_vars=$(compgen -e | grep '^HSK_' || true)
for v in $unset_vars; do unset "$v"; done
export CARGO_TARGET_DIR="$target" TMP="$tmp_dir" CARGO_TERM_COLOR=never
export TEMP="$TMP" TMPDIR="$TMP" CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
export CARGO_INCREMENTAL=0
mkdir -p "$TMP"
# nextest resolves its store (junit, run data) against the workspace root; pin it under the target dir (artifact-root
# target) or under the owner on the sole root (SSD target, CX-984-014).
toolcfg="$results/nextest-store.toml"
printf '[store]\ndir = "%s"\n' "$store_dir" >"$toolcfg"
nx=(--user-config-file none --tool-config-file "run-round:$toolcfg")
cd "$root"

if [ $dev -eq 0 ]; then
  # Step 0: committed lock and registry metadata must already match the committed manifests (read-only).
  if ! cargo metadata --locked --offline --format-version 1 >/dev/null 2>"$results/step0.lock.txt"; then
    reason="step0: Cargo.lock not in sync with the candidate manifests (step0.lock.txt); needs a steward sync commit"
    echo "run-round: infrastructure: $reason" >&2
    exit 75
  fi
  if ! "$py" -X utf8 -I tools/registry-sync.py . >"$results/step0.registry.txt" 2>&1; then
    reason="step0: registry metadata/probe approvals drift (step0.registry.txt); needs a steward sync commit"
    echo "run-round: infrastructure: $reason" >&2
    exit 75
  fi
  # Cargo hashes path packages relative to the workspace root, so a shared target dir can treat an export
  # whose files carry older (archive/commit) mtimes as fresh and reuse artifacts built from other content.
  # Current mtimes force every workspace crate to rebuild from the export (content and manifest unchanged).
  find "$root" -type f -print0 | xargs -0 touch
fi

# Step 0 policy (read-only, both modes, before any build): V4-03 machine-local paths (.config/machine-paths.toml) and
# V4-05 frozen duplicate fingerprints (.config/duplicate-fingerprints.toml); hits go to step0.policy.{json,txt}.
set +e
"$py" -X utf8 -I - "$root" >"$results/step0.policy.json" 2>"$results/step0.policy.txt" <<'PY'
import json, pathlib, re, sys, tomllib

sys.stdout.reconfigure(newline="\n")
root = pathlib.Path(sys.argv[1])
def load(name, schema):
    p = root / ".config" / name
    if not p.exists():
        print(f"run-round: .config/{name} missing", file=sys.stderr); sys.exit(2)
    doc = tomllib.loads(p.read_text(encoding="utf-8"))
    if doc.get("schema") != schema:
        print(f"run-round: .config/{name} schema must be {schema}", file=sys.stderr); sys.exit(2)
    return doc
def files(*subs):
    for crate in sorted(p for p in (root / "crates").iterdir() if p.is_dir()):
        for sub in subs:
            if (crate / sub).is_dir():
                for f in sorted((crate / sub).rglob("*"), key=lambda q: q.as_posix()):
                    data = f.read_bytes() if f.is_file() else b"\0"
                    if b"\0" not in data:
                        yield f.relative_to(root).as_posix(), data.decode("utf-8", errors="replace").splitlines()
paths_cfg = load("machine-paths.toml", "hsk.studio.machine-paths@1")
dup_cfg = load("duplicate-fingerprints.toml", "hsk.studio.duplicate-fingerprints@1")
pinned = paths_cfg.get("pinned", {})
MP = re.compile(r"(?<![A-Za-z0-9])[A-Za-z]:(?:/|\\(?![\"']))|Users[\\/]")
hits = {}
for rel, lines in files("src", "tests"):
    for i, line in enumerate(lines, 1):
        if MP.search(line):
            hits.setdefault(rel, []).append(f"{rel}:{i}: {line.strip()[:160]}")
path_violations = [{"file": rel, "hits": len(h), "pinned": pinned.get(rel), "lines": h[:20]}
                   for rel, h in sorted(hits.items()) if len(h) > pinned.get(rel, 0)]
pins_dropped = {rel: {"pinned": n, "now": len(hits.get(rel, []))} for rel, n in pinned.items()
                if len(hits.get(rel, [])) < n}
src = list(files("src"))
dups = []
for fp in dup_cfg.get("fingerprint", []):
    pats = [re.compile(p) for p in fp["patterns"]]
    sites = {}
    for rel, lines in src:
        for i, line in enumerate(lines, 1):
            if any(p.search(line) for p in pats):
                sites.setdefault(rel, []).append(f"{rel}:{i}: {line.strip()[:160]}")
    allowed = list(fp["allowed"])  # path prefixes relative to the workspace root (exact files or dirs)
    ok = lambda rel: any(rel.startswith(a) for a in allowed)
    new = {rel: sites[rel][:20] for rel in sorted(sites) if not ok(rel)}
    dups.append({"id": fp["id"], "new_sites": new, "sites": sorted(sites),
                 "allowed_without_match": [a for a in allowed if not any(r.startswith(a) for r in sites)]})
new_dups = [d for d in dups if d["new_sites"]]
print(json.dumps({"machine_paths": {"violations": path_violations, "pins_dropped": pins_dropped,
                                    "files_with_hits": len(hits)},
                  "duplicates": dups}, indent=1))
for v in path_violations:
    print(f"V4-03 machine-local path: {v['file']} hits={v['hits']} pinned={v['pinned']}", *v["lines"], sep="\n  ",
          file=sys.stderr)
for d in new_dups:
    for rel, lines in d["new_sites"].items():
        print(f"V4-05 new {d['id']} site: {rel}", *lines, sep="\n  ", file=sys.stderr)
sys.exit(72 if path_violations else 71 if new_dups else 0)
PY
policy=$?
set -e
case $policy in
  0) ;;
  71|72)
    reason="step0 policy: $([ $policy -eq 72 ] && echo 'V4-03 machine-local path' || echo 'V4-05 new frozen-duplicate site') (step0.policy.txt)"
    echo "run-round: $reason" >&2; head -n 40 "$results/step0.policy.txt" >&2
    exit $policy ;;
  *) cat "$results/step0.policy.txt" >&2; die "step 0 policy check failed (step0.policy.txt)" ;;
esac

# Plan: selection, feature matrix units, doctest units and the doctest per-test budget (read-only).
cargo metadata --no-deps --offline --format-version 1 ${locked[@]+"${locked[@]}"} >"$results/metadata.json" 2>"$results/metadata.stderr.txt" \
  || die "cargo metadata --no-deps failed (metadata.stderr.txt)"
plan="$results/units.plan.tsv"
"$py" -X utf8 -I - "$results/metadata.json" "$root/.config/round-features.toml" "$all" ${crates[@]+"${crates[@]}"} >"$plan" <<'PY' \
  || die "selection or .config/round-features.toml invalid (see above)"
import json, pathlib, sys, tomllib

sys.stdout.reconfigure(newline="\n")  # Windows text mode would end rows with CR (status "run\r")
meta = json.load(open(sys.argv[1], encoding="utf-8"))
matrix_path, sel_all, asked = pathlib.Path(sys.argv[2]), sys.argv[3] == "1", sys.argv[4:]
ids = set(meta["workspace_members"])
members = {p["name"]: p for p in meta["packages"] if p["id"] in ids}
unknown = [c for c in asked if c not in members]
if unknown:
    sys.exit("run-round: not workspace members: " + " ".join(unknown))
selected = sorted(members) if sel_all else list(dict.fromkeys(asked))
if not matrix_path.exists():
    sys.exit("run-round: .config/round-features.toml missing")
doc = tomllib.loads(matrix_path.read_text(encoding="utf-8"))
if doc.get("schema") != "hsk.studio.round-features@2":
    sys.exit("run-round: round-features.toml schema must be hsk.studio.round-features@2")
budget = doc.get("doctest", {}).get("per-test-timeout-s")
if not (isinstance(budget, int) and not isinstance(budget, bool) and 1 <= budget <= 3600):
    sys.exit("run-round: round-features.toml [doctest] per-test-timeout-s must be an integer 1..3600")
matrix = {}
for crate, sets in doc.get("crates", {}).items():
    ok = isinstance(sets, list) and all(
        isinstance(s, list) and s and all(isinstance(f, str) and f and "," not in f for f in s) for s in sets)
    if not ok:
        sys.exit(f"run-round: round-features.toml {crate}: expected a list of non-empty feature-name lists")
    matrix[crate] = [sorted(set(s)) for s in sets]
LIB = {"lib", "rlib", "dylib", "proc-macro"}
feature_units, doc_units = [], []
for crate in sorted(c for c in matrix if c not in members):
    for s in matrix[crate]:
        feature_units.append(("features", crate, ",".join(s), "crate_absent"))
for crate in selected:
    pkg = members[crate]
    has_doc = any(LIB & set(t["kind"]) and t.get("doctest", True) for t in pkg["targets"])
    for s in [[]] + matrix.get(crate, []):
        missing = [f for f in s if f not in pkg.get("features", {})]
        status = "feature_absent:" + ",".join(missing) if missing else "run"
        if s:
            feature_units.append(("features", crate, ",".join(s), status))
        doc_units.append(("doctest", crate, ",".join(s), status if status != "run" or has_doc else "no_doctest_lib"))
# [ignored]: (crate, feature set) units that run only the crate's #[ignore]d tests; an empty set = default features.
ignored = {}
for crate, sets in doc.get("ignored", {}).items():
    ok = isinstance(sets, list) and sets and all(
        isinstance(s, list) and all(isinstance(f, str) and f and "," not in f for f in s) for s in sets)
    if not ok:
        sys.exit(f"run-round: round-features.toml [ignored] {crate}: expected a list of feature-name lists")
    ignored[crate] = [sorted(set(s)) for s in sets]
ignored_units = []
for crate in sorted(ignored):
    if crate not in members:
        ignored_units += [("ignored", crate, ",".join(s), "crate_absent") for s in ignored[crate]]
    elif crate in selected:
        for s in ignored[crate]:
            missing = [f for f in s if f not in members[crate].get("features", {})]
            ignored_units.append(("ignored", crate, ",".join(s), "feature_absent:" + ",".join(missing) if missing else "run"))
# [prebuild]: "CRATE+F1+F2" (a declared features unit) = [[package, bin, feature...], ...]: worker binaries that
# unit's tests spawn from <target>/debug (F-R32-2); built from the candidate into the run's own target right before
# that unit (a prebuild row precedes it; a failed prebuild is that unit's build failure).
prebuild = {}
for unit, specs in doc.get("prebuild", {}).items():
    ok = isinstance(specs, list) and specs and all(
        isinstance(s, list) and len(s) >= 2 and all(isinstance(x, str) and x and "," not in x and "|" not in x for x in s)
        for s in specs)
    if not ok:
        sys.exit(f"run-round: round-features.toml [prebuild] {unit}: expected a list of [package, bin, feature...] lists")
    prebuild[unit] = specs
# Keys are checked against the whole declared [crates] table (a typo is refused for every selection); rows are emitted
# below only for units of the current plan, so a scoped selection without the unit's crate builds no worker (R39
# scoped render-gpu re-run was refused here when the check used the selection's units).
declared = {"+".join([c] + s) for c, sets in matrix.items() for s in sets}
for unit in prebuild:
    if unit not in declared:
        sys.exit(f"run-round: round-features.toml [prebuild] {unit}: not a declared [crates] features unit")
rows = []
for kind, crate, feats, status in feature_units + ignored_units + doc_units:
    unit = "+".join([crate] + feats.split(",")) if kind == "features" and feats else None
    if unit in prebuild and status == "run":
        for pkg, binary, *bfeats in prebuild[unit]:
            rows.append(("prebuild", unit, f"{pkg}|{binary}|{','.join(bfeats) or '-'}", "run"))
    rows.append((kind, crate, feats, status))
print(f"config\tdoctest-per-test-timeout-s\t{budget}\t-")
for kind, crate, feats, status in rows:
    print(f"{kind}\t{crate}\t{feats or '-'}\t{status}")
PY

# Oracle corpus: resolve the commit-free root as tests do (.config/oracle-corpus.toml): `corpus_json resolve` before
# the fixtures (root only), `corpus_json bind` after they are placed, so the bound content (oracle-corpus.json +
# oracle-corpus.manifest.txt) is what the tests load; an absent root is recorded, not a failure here.
corpus_json() {
"$py" -X utf8 -I - "$root" "$root/.config/oracle-corpus.toml" "$results/oracle-corpus.manifest.txt" "$1" \
  >"$results/oracle-corpus.json" <<'PY' || die "invalid .config/oracle-corpus.toml (see above)"
import hashlib, json, pathlib, re, sys

sys.stdout.reconfigure(newline="\n")
root, cfg, manifest_out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]), pathlib.Path(sys.argv[3])
bind = sys.argv[4] == "bind"
if not cfg.exists():
    print(json.dumps({"config": None, "present": False}))
    sys.exit(0)
kv = {}
for line in cfg.read_text(encoding="utf-8").splitlines():
    m = re.fullmatch(r'\s*([A-Za-z0-9_-]+)\s*=\s*(?:"([^"]*)"|(\d+))\s*', line)
    if m:
        kv[m.group(1)] = m.group(2) if m.group(2) is not None else int(m.group(3))
    elif line.strip() and not line.lstrip().startswith("#"):
        sys.exit(f"run-round: oracle-corpus.toml: not a `key = value` line: {line!r}")
if kv.get("schema") != "hsk.studio.oracle-corpus@1" or not isinstance(kv.get("search"), str) \
        or not isinstance(kv.get("max-ancestors"), int):
    sys.exit("run-round: oracle-corpus.toml needs schema hsk.studio.oracle-corpus@1, search, max-ancestors")
search, limit = kv["search"], kv["max-ancestors"]
if pathlib.PurePosixPath(search).is_absolute() or ".." in pathlib.PurePosixPath(search).parts or ":" in search:
    sys.exit("run-round: oracle-corpus.toml search must be a relative path without '..'")
found = next((a / search for a in [root, *root.parents][: limit + 1] if (a / search).is_dir()), None)
out = {"config": ".config/oracle-corpus.toml", "search": search, "max_ancestors": limit, "present": found is not None,
       "root": found.as_posix() if found else None, "bound": bind}
if found and bind:
    lines, total = [], 0
    for f in sorted((p for p in found.rglob("*") if p.is_file()), key=lambda p: p.relative_to(found).as_posix()):
        h = hashlib.sha256()
        with open(f, "rb") as fh:
            for chunk in iter(lambda: fh.read(1 << 20), b""):
                h.update(chunk)
        total += f.stat().st_size
        lines.append(f"{h.hexdigest()}  {f.relative_to(found).as_posix()}\n")
    manifest_out.write_text("".join(lines), encoding="utf-8", newline="\n")
    out.update(file_count=len(lines), bytes=total, manifest_file="oracle-corpus.manifest.txt",
               manifest_sha256=hashlib.sha256("".join(lines).encode("utf-8")).hexdigest())
print(json.dumps(out))
PY
}
corpus_json resolve

profile=default
[ $canary -eq 1 ] && profile=canary
units="$results/units.tsv"   # unit kind crate features status build_exit run_exit dir ('-' = none)
: >"$units"
rec() { printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$@" >>"$units"; }
canary_filter() {   # first listed test of every package in list file $1
  awk '{ split($1, b, "::"); if (!(b[1] in seen)) { seen[b[1]] = 1;
      printf "%s(binary_id(=%s) & test(=%s))", (n++ ? " | " : ""), $1, $2 } }' "$1"
}
# nextest_unit DIR FEATURES SELECTION... -> U_BUILD, U_RUN ('-' when not run), U_FILTER
nextest_unit() {
  local d=$1 feats=$2; shift 2
  local fa=() le run
  [ "$feats" != - ] && fa=(--features "$feats")
  U_RUN=- U_FILTER=""
  mkdir -p "$d"
  cargo nextest run "${nx[@]}" --no-run ${locked[@]+"${locked[@]}"} "$@" ${fa[@]+"${fa[@]}"} >"$d/build.stdout.txt" 2>"$d/build.stderr.txt"
  U_BUILD=$?
  if [ $U_BUILD -eq 0 ] && [ $canary -eq 1 ]; then
    cargo nextest list "${nx[@]}" --message-format oneline ${locked[@]+"${locked[@]}"} "$@" ${fa[@]+"${fa[@]}"} >"$d/list.txt" 2>"$d/list.stderr.txt"
    le=$?
    U_FILTER=$(canary_filter "$d/list.txt")
    printf '%s\n' "$U_FILTER" >"$d/canary-filter.txt"
    if [ $le -ne 0 ]; then U_BUILD=$le; elif [ -z "$U_FILTER" ]; then U_BUILD=3; fi
  fi
  if [ $U_BUILD -eq 0 ]; then
    run=(cargo nextest run "${nx[@]}" --no-fail-fast --no-tests=warn --profile "$profile" ${locked[@]+"${locked[@]}"} "$@" ${fa[@]+"${fa[@]}"})
    [ -n "$U_FILTER" ] && run+=(-E "$U_FILTER")
    rm -f "$store_dir/$profile/junit.xml"
    case " $* " in
      *" --workspace "*|*" hsk-studio-render-gpu "*)
        dbl_gpu_run "$gpu_root" round "$lock_owner" "gpu tests: $lock_label" "${run[@]}" >"$d/run.stdout.txt" 2>"$d/run.stderr.txt" ;;
      *) "${run[@]}" >"$d/run.stdout.txt" 2>"$d/run.stderr.txt" ;;
    esac
    U_RUN=$?
    [ -f "$store_dir/$profile/junit.xml" ] && cp "$store_dir/$profile/junit.xml" "$d/junit.xml"
  fi
  return 0
}
# kill_tree PID: the background job and every process it started (Windows: taskkill /T; else its children).
kill_tree() {
  if [ -r "/proc/$1/winpid" ] && command -v taskkill >/dev/null; then
    taskkill //F //T //PID "$(cat "/proc/$1/winpid")" >/dev/null 2>&1
  else
    pkill -KILL -P "$1" 2>/dev/null; kill -KILL "$1" 2>/dev/null
  fi
  return 0
}
# doctest_unit OUT ERR CARGO_ARGS... -> D_EXIT, D_TIMEOUT (name of the test killed at the budget, or "")
# Serial libtest writes "test NAME ... " (no newline) when NAME starts and its verdict when it ends. A line left
# unfinished for doctest_budget seconds is that test's timeout: the unit's process tree is killed, TIMEOUT is
# appended to the line, and every listed doctest of the unit without a verdict is reported NOT_RUN (from
# `-- --list`, which does not execute doctests). No rerun with --skip: rustdoc splits test args on whitespace,
# so full doctest names ("path - item (line N)") cannot be passed as skip or exact filters.
doctest_unit() {
  local out=$1 err=$2 pid last pend="" since=0 now t; shift 2
  D_TIMEOUT=""
  cargo test --doc "$@" -- --test-threads=1 >"$out" 2>"$err" &
  pid=$!
  while kill -0 "$pid" 2>/dev/null; do
    sleep 1
    last=""
    [ -s "$out" ] && [ -n "$(tail -c1 "$out")" ] && last=$(tail -n1 "$out")
    case "$last" in
      "test "*" ... ")
        now=$(date +%s)
        if [ "$last" != "$pend" ]; then pend=$last since=$now
        elif [ $((now - since)) -ge "$doctest_budget" ]; then
          D_TIMEOUT=${last#test }; D_TIMEOUT=${D_TIMEOUT% ... }
          kill_tree "$pid"; break
        fi ;;
      *) pend="" ;;
    esac
  done
  wait "$pid"; D_EXIT=$?
  if [ -n "$D_TIMEOUT" ]; then
    printf 'TIMEOUT (per-test budget %ss exceeded; process tree killed by run-round)\n' "$doctest_budget" >>"$out"
    cargo test --doc "$@" -- --list >"$out.list" 2>>"$err"
    sed -n 's/^\(.*\): test$/\1/p' "$out.list" | while IFS= read -r t; do
      grep -qF "test $t " "$out" || printf 'test %s ... NOT_RUN (unit stopped at a per-test timeout)\n' "$t" >>"$out"
    done
    D_EXIT=124
  fi
  return 0
}

if [ $all -eq 1 ]; then sel=(--workspace); else sel=(); for c in "${crates[@]}"; do sel+=(-p "$c"); done; fi
doctest_budget=$(awk -F'\t' '$1 == "config" && $2 == "doctest-per-test-timeout-s" { print $3 }' "$plan")
# Disk-wide build lock (class round = queue priority): held for every cargo unit (fixture, default, feature, doctest);
# between units, once the round has held it for STUDIO_DISK_BUILD_LOCK_ROUND_SLICE_S (default 300) s, it yields
# (release + gap: one queued builder may build) and re-queues first. results/disk-lock.json sums it up.
lock_label="run-round $([ $all -eq 1 ] && echo --all || echo "${crates[*]}") sha ${identity_sha:0:12} results $results"
lock_owner="run-round $(basename "${owner_dir:-$root}")"
lock_slice=${STUDIO_DISK_BUILD_LOCK_ROUND_SLICE_S:-300}
lock_holds=0 lock_yields=0 lock_waited=0 lock_max_wait=0 lock_broke="" lock_since=0 lock_first=""
lock_unit() {
  [ -n "${DBL_TOKEN:-}" ] && return 0
  dbl_acquire "$lock_root" round "$lock_owner" "$lock_label" run-round \
    || { reason="disk build lock: gave up waiting or protocol mismatch ($lock_root)"; exit 75; }
  lock_holds=$((lock_holds + 1)) lock_waited=$((lock_waited + DBL_WAITED_S))
  [ "$DBL_WAITED_S" -gt "$lock_max_wait" ] && lock_max_wait=$DBL_WAITED_S
  [ -n "$DBL_BROKE" ] && lock_broke="$lock_broke $DBL_BROKE"
  [ -n "$lock_first" ] || lock_first=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  dbl_now; lock_since=$DBL_NOW
  return 0
}
unlock_unit() {
  [ -n "${DBL_TOKEN:-}" ] || return 0
  dbl_now
  if [ $((DBL_NOW - lock_since)) -ge "$lock_slice" ]; then dbl_yield; lock_yields=$((lock_yields + 1)); fi
  return 0
}
set +e
lock_unit
# Oracle fixtures (.config/oracle-fixtures.toml): built before any unit into the run's own target dir, then placed in
# the corpus root under a content-addressed name (<dest>/sha256/<library sha256>/<file>, written only when absent or
# different, never replacing other content) with <dest>/MANIFEST.json naming that file (atomic replace), so tests find
# them via the corpus convention. fixtures/<id>.json binds library path, sha256 and MANIFEST.json sha256; the corpus
# is hashed after placement, and both are re-verified after the units (fixture_binding; changed = exit 76).
mkdir -p "$results/fixtures"
"$py" -X utf8 -I - "$root" "$results" "$target" "$identity_sha" "$all" ${crates[@]+"${crates[@]}"} \
  >"$results/fixtures/units.tsv" 2>"$results/fixtures/step.stderr.txt" <<'PY'
import datetime, hashlib, json, os, pathlib, subprocess, sys, tomllib

sys.stdout.reconfigure(newline="\n")
root, res, target, sha, sel_all, asked = (pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]),
                                          pathlib.Path(sys.argv[3]), sys.argv[4], sys.argv[5] == "1", sys.argv[6:])
cfg_path = root / ".config" / "oracle-fixtures.toml"
cfg = tomllib.loads(cfg_path.read_text(encoding="utf-8")) if cfg_path.exists() else {"fixture": []}
if cfg_path.exists() and cfg.get("schema") != "hsk.studio.oracle-fixtures@1":
    sys.exit("run-round: oracle-fixtures.toml schema must be hsk.studio.oracle-fixtures@1")
meta = json.loads((res / "metadata.json").read_text(encoding="utf-8"))
ids = set(meta["workspace_members"])
members = {p["name"] for p in meta["packages"] if p["id"] in ids}
selected = members if sel_all else set(asked)
corpus = json.loads((res / "oracle-corpus.json").read_text(encoding="utf-8"))
lib_file = {"win32": "{}.dll", "darwin": "lib{}.dylib"}.get(sys.platform, "lib{}.so")
def tree_sha(d):
    lines = []
    for f in sorted((p for p in d.rglob("*") if p.is_file()), key=lambda p: p.relative_to(d).as_posix()):
        lines.append(f"{hashlib.sha256(f.read_bytes()).hexdigest()}  {f.relative_to(d).as_posix()}\n")
    return hashlib.sha256("".join(lines).encode("utf-8")).hexdigest()
for fx in cfg.get("fixture", []):
    fid, safe = fx["id"], fx["id"].replace("/", "+")
    if not set(fx["needed_by"]) & selected:
        continue
    if not corpus.get("present"):
        print(f"fixture:{fid}\tfixture\t{fx['needed_by'][0]}\t-\tcorpus_absent\t-\t-\t-")
        continue
    manifest = root / fx["manifest"]
    tdir = target / "fixtures" / safe
    cmd = ["cargo", "build", "--offline", "--locked", "--manifest-path", str(manifest), "--target-dir", str(tdir),
           # Byte-reproducible library (same sha256 from any target dir, export root or profile env; --config
           # outranks CARGO_PROFILE_* env): no debuginfo/PDB path, no incremental state, no PE timestamp.
           "--config", "profile.dev.debug=0", "--config", 'profile.dev.strip="symbols"',
           "--config", "profile.dev.incremental=false"]
    if sys.platform == "win32":
        cmd += ["--config", 'build.rustflags=["-Clink-arg=/Brepro"]']
    with open(res / "fixtures" / f"{safe}.build.stdout.txt", "wb") as so, \
            open(res / "fixtures" / f"{safe}.build.stderr.txt", "wb") as se:
        rc = subprocess.run(cmd, stdout=so, stderr=se, stdin=subprocess.DEVNULL).returncode
    status, dest_rel = "run", f"fixtures/{safe}"
    if rc == 0:
        name = lib_file.format(fx["lib"])
        built = tdir / "debug" / name
        data = built.read_bytes()
        digest = hashlib.sha256(data).hexdigest()
        dest = pathlib.Path(corpus["root"]) / fx["dest"]
        rel = f"sha256/{digest}/{name}"   # content-addressed: a name never holds other bytes
        lib_path = dest / rel
        placed = "existing"
        if not lib_path.is_file() or hashlib.sha256(lib_path.read_bytes()).hexdigest() != digest:
            lib_path.parent.mkdir(parents=True, exist_ok=True)
            tmp = lib_path.parent / f".{name}.tmp-{os.getpid()}"
            tmp.write_bytes(data)
            os.replace(tmp, lib_path)
            placed = "written"
        record = {"schema": "hsk.studio.oracle-fixture@1", "id": fid, "library": rel,
                  "sha256": digest, "source_manifest": fx["manifest"],
                  "source_sha256": tree_sha(manifest.parent / "src"), "round_sha": sha,
                  "built_at": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")}
        # Keep an existing MANIFEST.json that already names these exact library and source bytes (only round_sha and
        # built_at would differ): a concurrent owner's run then never sees its bound manifest change (exit 76).
        same = ("id", "library", "sha256", "source_manifest", "source_sha256")
        try:
            old_bytes = (dest / "MANIFEST.json").read_bytes()
            old = json.loads(old_bytes)
        except (OSError, ValueError):
            old_bytes, old = None, None
        if isinstance(old, dict) and old.get("schema") == record["schema"] and all(old.get(k) == record[k] for k in same):
            record, manifest_bytes, manifest_state = old, old_bytes, "kept"
        else:
            manifest_bytes = (json.dumps(record, indent=1) + "\n").encode("utf-8")
            tmp = dest / f".MANIFEST.json.tmp-{os.getpid()}"
            tmp.write_bytes(manifest_bytes)
            os.replace(tmp, dest / "MANIFEST.json")
            manifest_state = "written"
        binding = dict(record, manifest_state=manifest_state, manifest_path=f"{fx['dest']}/MANIFEST.json",
                       manifest_sha256=hashlib.sha256(manifest_bytes).hexdigest(),
                       library_path=f"{fx['dest']}/{rel}", placed=placed, built_from=built.as_posix())
        (res / "fixtures" / f"{safe}.json").write_text(json.dumps(binding, indent=1) + "\n", encoding="utf-8",
                                                       newline="\n")
    print(f"fixture:{fid}\tfixture\t{fx['needed_by'][0]}\t-\t{status}\t{rc}\t-\t{dest_rel}")
PY
[ $? -eq 0 ] || { reason="oracle fixture step failed (fixtures/step.stderr.txt)"; cat "$results/fixtures/step.stderr.txt" >&2; exit 2; }
unlock_unit
corpus_json bind   # after placement: binds the corpus the tests will load
lock_unit
nextest_unit "$results" - "${sel[@]}"
unlock_unit
rec default nextest '*' - run "$U_BUILD" "$U_RUN" .
cat "$results/fixtures/units.tsv" >>"$units"
default_filter=$U_FILTER
mkdir -p "$results/features" "$results/ignored" "$results/doctest"
declare -A prebuild_fail=()
while IFS=$'\t' read -r kind crate feats status; do
  status=${status%$'\r'}
  [ "$kind" = config ] && continue
  if [ "$kind" = prebuild ]; then
    # [prebuild] (F-R32-2): a worker binary the next features unit ($crate = its unit name) spawns from
    # <target>/debug, built from this candidate into this run's target; a failure fails that unit's build.
    IFS='|' read -r pb_pkg pb_bin pb_feats <<<"$feats"
    pfa=(); [ "$pb_feats" != - ] && pfa=(--features "$pb_feats")
    mkdir -p "$results/prebuild"
    lock_unit
    cargo build ${locked[@]+"${locked[@]}"} -p "$pb_pkg" --bin "$pb_bin" ${pfa[@]+"${pfa[@]}"} \
      >"$results/prebuild/$crate+$pb_bin.stdout.txt" 2>"$results/prebuild/$crate+$pb_bin.stderr.txt"
    pb_rc=$?
    unlock_unit
    rec "prebuild:$crate:$pb_bin" build "$pb_pkg" "$pb_feats" run "$pb_rc" - "prebuild/$crate+$pb_bin"
    [ $pb_rc -ne 0 ] && prebuild_fail[$crate]=$pb_rc
    continue
  fi
  name=$crate; [ "$feats" != - ] && name="$crate+${feats//,/+}"
  if [ "$status" != run ]; then
    k=doctest; [ "$kind" != doctest ] && k=nextest
    rec "$kind:$name" "$k" "$crate" "$feats" "$status" - - -
  elif [ "$kind" = features ] && [ -n "${prebuild_fail[$name]:-}" ]; then
    rec "features:$name" nextest "$crate" "$feats" run "${prebuild_fail[$name]}" - "features/$name"
  elif [ "$kind" = features ]; then
    lock_unit
    nextest_unit "$results/features/$name" "$feats" -p "$crate"
    unlock_unit
    rec "features:$name" nextest "$crate" "$feats" run "$U_BUILD" "$U_RUN" "features/$name"
  elif [ "$kind" = ignored ] && [ $canary -eq 1 ]; then
    # Canary: corpus sweeps exceed the canary profile's 30 s cap by design; build them (no run).
    fa=(); [ "$feats" != - ] && fa=(--features "$feats")
    mkdir -p "$results/ignored/$name"
    lock_unit
    cargo nextest run "${nx[@]}" --no-run ${locked[@]+"${locked[@]}"} -p "$crate" --run-ignored only ${fa[@]+"${fa[@]}"} \
      >"$results/ignored/$name/build.stdout.txt" 2>"$results/ignored/$name/build.stderr.txt"
    b=$?
    unlock_unit
    rec "ignored:$name" nextest "$crate" "$feats" run "$b" - "ignored/$name"
  elif [ "$kind" = ignored ]; then
    lock_unit
    nextest_unit "$results/ignored/$name" "$feats" -p "$crate" --run-ignored only
    unlock_unit
    rec "ignored:$name" nextest "$crate" "$feats" run "$U_BUILD" "$U_RUN" "ignored/$name"
  else
    fa=(); [ "$feats" != - ] && fa=(--features "$feats")
    lock_unit
    doctest_unit "$results/doctest/$name.stdout.txt" "$results/doctest/$name.stderr.txt" \
      ${locked[@]+"${locked[@]}"} -p "$crate" ${fa[@]+"${fa[@]}"}
    unlock_unit
    # A library that does not compile is a build failure; anything else is the doctest run's verdict.
    if [ $D_EXIT -ne 0 ] && [ -z "$D_TIMEOUT" ] && grep -q '^error: could not compile' "$results/doctest/$name.stderr.txt"; then
      b=$D_EXIT r=-
    else
      b=0 r=$D_EXIT
    fi
    rec "doctest:$name" doctest "$crate" "$feats" run "$b" "$r" "doctest/$name"
  fi
done <"$plan"
dbl_release   # no build after the units (aggregation reads files only)
printf '{"root": %s, "schema": %s, "class": "round", "holds": %s, "yields": %s, "slice_s": %s, "waited_s_total": %s, "waited_s_max": %s, "broke_stale": %s, "first_acquired_at": %s, "released_at": %s}\n' \
  "$(jstr "$lock_root")" "$(jstr "$DBL_SCHEMA")" "$lock_holds" "$lock_yields" "$lock_slice" "$lock_waited" "$lock_max_wait" \
  "$(jstr "${lock_broke# }")" "$(jstr "$lock_first")" "$(jstr "$(date -u +%Y-%m-%dT%H:%M:%SZ)")" >"$results/disk-lock.json"
ended=$(date -u +%Y-%m-%dT%H:%M:%SZ)
first_build=0 first_run=0 absent=0
while IFS=$'\t' read -r _ _ _ _ status b r _; do
  case "$status" in feature_absent:*|crate_absent) absent=1 ;; esac
  [ "$status" = run ] || continue
  [ "$b" != - ] && [ "$b" != 0 ] && [ $first_build -eq 0 ] && first_build=$b
  [ "$r" != - ] && [ "$r" != 0 ] && [ $first_run -eq 0 ] && first_run=$r
done <"$units"
code=$first_build; [ "$code" -eq 0 ] && code=$first_run; [ "$code" -eq 0 ] && [ $absent -eq 1 ] && code=74
set -e

export RR_MODE=$([ $dev -eq 1 ] && echo dev || echo export) RR_PROFILE=$profile
export RR_SELECTION=$([ $all -eq 1 ] && echo --all || echo "${crates[*]}") RR_SHA=$identity_sha RR_TREE=$identity_tree
export RR_IDENTITY=$identity RR_OWNER_DIR=$owner_dir RR_MANIFEST=$manifest RR_DIRTY=$dirty RR_STARTED=$started RR_ENDED=$ended
export RR_LOCK_SHA=$(sha256sum Cargo.lock | cut -c1-64) RR_FILTER=$default_filter RR_EXIT=$code RR_TARGET=$target RR_TARGET_CLASS=$target_class
export RR_RUSTC=$(rustc -V 2>/dev/null) RR_CARGO=$(cargo -V 2>/dev/null) RR_NEXTEST=$(cargo nextest --version 2>/dev/null | head -n1)
export RR_UNSET=$(echo $unset_vars) RR_WRAPPER=${RUSTC_WRAPPER:-} RR_DOCTEST_BUDGET=$doctest_budget
if ! "$py" -X utf8 -I - "$results" "$root/.config/round-features.toml" <<'PY'; then
import hashlib, json, os, pathlib, re, sys
import xml.etree.ElementTree as ET

res, matrix = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
E = os.environ
STATUSES = ("passed", "failed", "skipped", "timed_out", "not_run")
def num(x):
    return None if x == "-" else int(x)
def summary(d):
    lines = []
    for f in ("run.stdout.txt", "run.stderr.txt"):
        p = d / f
        if p.exists():
            lines += [l.strip() for l in p.read_text(encoding="utf-8", errors="replace").splitlines() if re.match(r"^ *Summary ", l)]
    return lines[-1] if lines else ""
NP = re.compile(r"^[ \t]*NOT_PROVEN:[ \t]*(.*?)[ \t\r]*$", re.M)
def case_status(case):
    # nextest junit: a per-test timeout is <failure type="test timeout"/> (CX-EXEC-005: timed_out, not failed).
    bad = [c for c in case if c.tag in ("failure", "error")]
    if bad:
        return "timed_out" if all(c.get("type") == "test timeout" for c in bad) else "failed"
    return "skipped" if any(c.tag == "skipped" for c in case) else "passed"
def junit(path, unit, feats, root=None):
    out = []
    if root is None:
        if not path.exists():
            return out
        root = ET.parse(path).getroot()
    for suite in root.iter("testsuite"):
        binary = suite.get("name", "")
        for case in suite.iter("testcase"):
            status = case_status(case)
            text = "\n".join(c.text or "" for c in case if c.tag in ("system-out", "system-err"))
            out.append({"unit": unit, "kind": "nextest", "crate": binary.split("::")[0], "features": feats,
                        "binary": binary, "name": case.get("name", ""), "status": status,
                        "seconds": float(case.get("time") or 0), "not_proven": NP.findall(text)})
    return out
# Self-check of the junit mapping (synthetic: one pass, one fail, one timeout, one skip); a mismatch is an aggregator
# defect, so the round reports it instead of mislabelling verdicts.
_probe = ET.fromstring('<testsuites><testsuite name="c::b"><testcase name="p"/>'
                       '<testcase name="f"><failure type="test failure with exit code 101"/></testcase>'
                       '<testcase name="t"><failure type="test timeout"/></testcase>'
                       '<testcase name="s"><skipped/></testcase></testsuite></testsuites>')
_got = {t["name"]: t["status"] for t in junit(None, "self-check", [], root=_probe)}
if _got != {"p": "passed", "f": "failed", "t": "timed_out", "s": "skipped"}:
    sys.exit(f"run-round: aggregator self-check failed (junit status mapping): {_got}")
DOC = re.compile(r"^test (.+?) +\.\.\. (ok|FAILED|ignored|TIMEOUT|NOT_RUN)\b")
DOC_STATUS = {"ok": "passed", "FAILED": "failed", "ignored": "skipped", "TIMEOUT": "timed_out", "NOT_RUN": "not_run"}
def doctests(path, unit, crate, feats):
    out = []
    if path.exists():
        for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
            m = DOC.match(line)
            if m:
                out.append({"unit": unit, "kind": "doctest", "crate": crate, "features": feats, "binary": "doctest",
                            "name": m.group(1), "status": DOC_STATUS[m.group(2)], "seconds": None, "not_proven": []})
    return out

units, tests = [], []
for line in (res / "units.tsv").read_text(encoding="utf-8").splitlines():
    unit, kind, crate, feats, status, b, r, d = line.rstrip("\r").split("\t")
    feats = [] if feats == "-" else feats.split(",")
    u = {"unit": unit, "kind": kind, "crate": None if crate == "*" else crate, "features": feats, "status": status,
         "build_exit": num(b), "run_exit": num(r), "dir": None if d == "-" else d}
    got = []
    if status == "run" and kind == "nextest":
        ud = res / d
        got = junit(ud / "junit.xml", unit, feats)
        u["summary"] = summary(ud)
    elif status == "run" and kind == "doctest":
        got = doctests(res / f"{d}.stdout.txt", unit, crate, feats)
    for k in STATUSES:
        u[k] = sum(t["status"] == k for t in got)
    u["not_proven"] = sum(len(t["not_proven"]) for t in got)
    units.append(u)
    tests += got
per_crate = {}
for t in tests:
    if t["unit"] == "default":
        c = per_crate.setdefault(t["crate"], {k: 0 for k in STATUSES})
        c[t["status"]] += 1
default = units[0]
not_proven = [{"unit": t["unit"], "crate": t["crate"], "binary": t["binary"], "test": t["name"], "reason": r}
              for t in tests for r in t["not_proven"]]
code = int(E["RR_EXIT"])
if code == 0 and not_proven:
    code = 73
policy_file = res / "step0.policy.json"
policy = json.loads(policy_file.read_text(encoding="utf-8")) if policy_file.exists() else None
corpus_file = res / "oracle-corpus.json"
corpus = json.loads(corpus_file.read_text(encoding="utf-8")) if corpus_file.exists() else {"present": False}
# Fixture binding: what the placement step bound must still be what MANIFEST.json and the library hold after the units.
fixtures = []
for f in sorted((res / "fixtures").glob("*.json")) if (res / "fixtures").is_dir() else []:
    b = json.loads(f.read_text(encoding="utf-8"))
    if b.get("schema") != "hsk.studio.oracle-fixture@1" or not corpus.get("root"):
        continue
    croot = pathlib.Path(corpus["root"])
    def sha_of(p):
        return hashlib.sha256(p.read_bytes()).hexdigest() if p.is_file() else None
    now_manifest, now_library = sha_of(croot / b["manifest_path"]), sha_of(croot / b["library_path"])
    b["after_units"] = {"manifest_sha256": now_manifest, "library_sha256": now_library}
    b["binding_intact"] = now_manifest == b["manifest_sha256"] and now_library == b["sha256"]
    fixtures.append(b)
disk_lock_file = res / "disk-lock.json"
disk_lock = json.loads(disk_lock_file.read_text(encoding="utf-8")) if disk_lock_file.exists() else None
if any(not b["binding_intact"] for b in fixtures) and code in (0, 73, 74):
    code = 76
mx = matrix.read_bytes() if matrix.exists() else b""
absent = [u["unit"] + " (" + u["status"] + ")" for u in units if u["status"].startswith(("feature_absent", "crate_absent"))]
round_json = {
    "schema": "hsk.studio.round-run@2", "complete": True,
    "mode": E["RR_MODE"], "profile": E["RR_PROFILE"], "selection": E["RR_SELECTION"],
    "sha": E["RR_SHA"], "tree": E["RR_TREE"], "identity_file": E["RR_IDENTITY"],
    "export_owner_dir": E["RR_OWNER_DIR"], "export_manifest_sha256": E["RR_MANIFEST"],
    "lock_sha256": E["RR_LOCK_SHA"], "edit_tree_dirty_entries": E["RR_DIRTY"],
    "started_at": E["RR_STARTED"], "ended_at": E["RR_ENDED"],
    "build_exit": default["build_exit"], "run_exit": default["run_exit"], "summary": default.get("summary", ""),
    "canary_filter": E["RR_FILTER"], "exit": code,
    "not_proven": not_proven, "oracle_corpus": corpus, "fixtures": fixtures, "disk_lock": disk_lock,
    "step0_policy": None if policy is None else {
        "machine_path_files_with_hits": policy["machine_paths"]["files_with_hits"],
        "machine_path_pins_dropped": policy["machine_paths"]["pins_dropped"],
        "duplicate_sites": {d["id"]: d["sites"] for d in policy["duplicates"]},
        "duplicate_allowed_without_match": {d["id"]: d["allowed_without_match"] for d in policy["duplicates"]
                                            if d["allowed_without_match"]}},
    "absent_units": absent,
    "timed_out_tests": [t["unit"] + ": " + t["name"] for t in tests if t["status"] == "timed_out"],
    "feature_matrix": {"file": ".config/round-features.toml", "sha256": hashlib.sha256(mx).hexdigest() if mx else None},
    "doctest_per_test_timeout_s": int(E["RR_DOCTEST_BUDGET"]),
    "units": units, "per_crate_default": dict(sorted(per_crate.items())),
    "tests_file": "tests.json", "tests_reported": len(tests),
    "tools": {"rustc": E["RR_RUSTC"], "cargo": E["RR_CARGO"], "nextest": E["RR_NEXTEST"]},
    "rustc_wrapper": E["RR_WRAPPER"],
    "env_set": ["CARGO_TARGET_DIR", "TMP", "TEMP", "TMPDIR", "CARGO_PROFILE_DEV_DEBUG", "CARGO_PROFILE_TEST_DEBUG", "CARGO_TERM_COLOR", "CARGO_INCREMENTAL"],
    "env_unset": E["RR_UNSET"], "target_dir": E["RR_TARGET"], "target_class": E["RR_TARGET_CLASS"],
}
(res / "tests.json").write_text(json.dumps({"schema": "hsk.studio.round-tests@1", "sha": E["RR_SHA"], "tests": tests},
                                           indent=1) + "\n", encoding="utf-8", newline="\n")
(res / "round.json").write_text(json.dumps(round_json, indent=1) + "\n", encoding="utf-8", newline="\n")
for u in units:
    print("  %-48s %-24s build=%s run=%s pass=%d fail=%d skip=%d timeout=%d not_run=%d not_proven=%d" % (u["unit"],
          u["status"], u["build_exit"], u["run_exit"], u["passed"], u["failed"], u["skipped"], u["timed_out"],
          u["not_run"], u["not_proven"]))
for n in not_proven:
    print("  NOT_PROVEN %s %s: %s" % (n["unit"], n["test"], n["reason"]))
for b in fixtures:
    print("  FIXTURE %s %s sha256 %s manifest %s intact=%s placed=%s" % (b["id"], b["library_path"], b["sha256"][:16],
          b["manifest_sha256"][:16], b["binding_intact"], b["placed"]))
dl = disk_lock or {}
print("SUMMARY exit=%d units=%d not_proven=%d absent=%d timed_out=%d oracle_corpus=%s disk_lock holds=%s yields=%s waited_s=%s (max %s)" % (
      code, len(units), len(not_proven), len(round_json["absent_units"]), len(round_json["timed_out_tests"]),
      corpus.get("root") or "absent", dl.get("holds"), dl.get("yields"), dl.get("waited_s_total"), dl.get("waited_s_max")))
PY
  reason="results aggregation failed (raw files kept)"
  echo "run-round: $reason" >&2
  exit 70
fi
code=$(sed -n 's/^ "exit": \([0-9][0-9]*\),$/\1/p' "$results/round.json")
echo "run-round: exit=$code -> $results/round.json"
exit "$code"
