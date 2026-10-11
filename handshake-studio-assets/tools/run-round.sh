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
# Units (each its own result; all run; a unit's build failure skips only that unit's test run):
#   default    union over the selection, default features: one build (nextest --no-run) + one run (--no-fail-fast);
#              files at the results root (build.*, run.*, list.txt, canary-filter.txt, junit.xml) as before.
#              Per-test timeouts: .config/nextest.toml slow-timeout (CX-EXEC-005).
#   features   per selected crate and feature set declared in .config/round-features.toml: -p CRATE --features SET,
#              own build + run under features/CRATE+F1+F2/. A set naming a feature (or crate) the candidate lacks is
#              recorded feature_absent (crate_absent) and fails the round with exit 74 (declared unit absent).
#   doctest    `cargo test --doc` per selected crate with a doctested lib target, default features and every
#              declared set (the per-test runner does not run doctests): doctest/CRATE[+F1...].{stdout,stderr}.txt.
#              Per-test timeout: doctests run serially (--test-threads=1), so libtest prints "test NAME ... " when a
#              test starts; a watchdog kills the unit's process tree when one test stays unfinished longer than
#              [doctest] per-test-timeout-s (.config/round-features.toml), appends TIMEOUT to that line, reports the
#              unit's doctests without a verdict NOT_RUN, and the unit's run exit is 124. Compiling the crate before
#              its first doctest is not covered.
# Results: round.json (identity, env, every unit with exits and passed/failed/skipped/timed_out/not_run counts, per-crate
#   counts), tests.json (every reported test: unit, crate, binary, name, status, seconds; passing names included),
#   junit.xml per nextest unit (passing tests' output stored, skipped tests listed: .config/nextest.toml),
#   metadata.json (cargo metadata --no-deps), units.tsv, step0.*.txt, nextest-store.toml. Once the results dir
#   exists, round.json is always written: on refusal, step 0 drift, interruption or aggregation failure it is a
#   minimal record with "complete": false, the exit code and the reason (raw files kept).
# Env: sets CARGO_TARGET_DIR, TMP/TEMP/TMPDIR (<target-dir>/../tmp), CARGO_PROFILE_{DEV,TEST}_DEBUG,
#   CARGO_TERM_COLOR; unsets every HSK_* variable. Nothing else is required from the caller (python 3.11+).
# Exit: 2 refused, 75 step 0 drift (infrastructure), 70 results aggregation failed, else the first failing unit
#   build's exit code (default unit first), else the first failing test run's exit code (124 = doctest per-test
#   timeout), else 74 when a declared feature-matrix unit is absent at the candidate, else 0.
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
for p in "$target" "$results"; do
  case "$p" in [cC]:*|/c/*) die "refusing C: path $p (build output and temp stay off C:)" ;; esac
  under "$p" "$root" && die "refusing path inside the workspace: $p"
done
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
  [ -e "$results/round.json" ] && return
  {
    printf '{\n "schema": "hsk.studio.round-run@2",\n "complete": false,\n "exit": %s,\n "reason": %s,\n' "$rc" "$(jstr "${reason:-exit $rc}")"
    printf ' "mode": %s,\n "selection": %s,\n' "$(jstr "$([ $dev -eq 1 ] && echo dev || echo export)")" "$(jstr "$([ $all -eq 1 ] && echo --all || echo "${crates[*]}")")"
    printf ' "sha": %s,\n "tree": %s,\n "identity_file": %s,\n' "$(jstr "$identity_sha")" "$(jstr "$identity_tree")" "$(jstr "$identity")"
    printf ' "started_at": %s,\n "ended_at": %s,\n' "$(jstr "$started")" "$(jstr "$(date -u +%Y-%m-%dT%H:%M:%SZ)")"
    printf ' "units_file": "units.tsv",\n "target_dir": %s\n}\n' "$(jstr "$target")"
  } >"$results/round.json"
  echo "run-round: incomplete (exit $rc: ${reason:-exit $rc}) -> $results/round.json" >&2
}
trap finish EXIT
trap 'reason="interrupted"; exit 130' INT TERM HUP

# Environment: only what this script sets.
unset_vars=$(compgen -e | grep '^HSK_' || true)
for v in $unset_vars; do unset "$v"; done
export CARGO_TARGET_DIR="$target" TMP="$target/../tmp" CARGO_TERM_COLOR=never
export TEMP="$TMP" TMPDIR="$TMP" CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
mkdir -p "$TMP"
# nextest resolves its store (junit, run data) against the workspace root; pin it under the target dir.
toolcfg="$results/nextest-store.toml"
printf '[store]\ndir = "%s/nextest"\n' "$target" >"$toolcfg"
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
print(f"config\tdoctest-per-test-timeout-s\t{budget}\t-")
for kind, crate, feats, status in feature_units + doc_units:
    print(f"{kind}\t{crate}\t{feats or '-'}\t{status}")
PY

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
    rm -f "$target/nextest/$profile/junit.xml"
    "${run[@]}" >"$d/run.stdout.txt" 2>"$d/run.stderr.txt"
    U_RUN=$?
    [ -f "$target/nextest/$profile/junit.xml" ] && cp "$target/nextest/$profile/junit.xml" "$d/junit.xml"
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
set +e
nextest_unit "$results" - "${sel[@]}"
rec default nextest '*' - run "$U_BUILD" "$U_RUN" .
default_filter=$U_FILTER
mkdir -p "$results/features" "$results/doctest"
while IFS=$'\t' read -r kind crate feats status; do
  status=${status%$'\r'}
  [ "$kind" = config ] && continue
  name=$crate; [ "$feats" != - ] && name="$crate+${feats//,/+}"
  if [ "$status" != run ]; then
    k=doctest; [ "$kind" = features ] && k=nextest
    rec "$kind:$name" "$k" "$crate" "$feats" "$status" - - -
  elif [ "$kind" = features ]; then
    nextest_unit "$results/features/$name" "$feats" -p "$crate"
    rec "features:$name" nextest "$crate" "$feats" run "$U_BUILD" "$U_RUN" "features/$name"
  else
    fa=(); [ "$feats" != - ] && fa=(--features "$feats")
    doctest_unit "$results/doctest/$name.stdout.txt" "$results/doctest/$name.stderr.txt" \
      ${locked[@]+"${locked[@]}"} -p "$crate" ${fa[@]+"${fa[@]}"}
    # A library that does not compile is a build failure; anything else is the doctest run's verdict.
    if [ $D_EXIT -ne 0 ] && [ -z "$D_TIMEOUT" ] && grep -q '^error: could not compile' "$results/doctest/$name.stderr.txt"; then
      b=$D_EXIT r=-
    else
      b=0 r=$D_EXIT
    fi
    rec "doctest:$name" doctest "$crate" "$feats" run "$b" "$r" "doctest/$name"
  fi
done <"$plan"
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
export RR_LOCK_SHA=$(sha256sum Cargo.lock | cut -c1-64) RR_FILTER=$default_filter RR_EXIT=$code RR_TARGET=$target
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
def junit(path, unit, feats):
    out = []
    if not path.exists():
        return out
    for suite in ET.parse(path).getroot().iter("testsuite"):
        binary = suite.get("name", "")
        for case in suite.iter("testcase"):
            tags = {c.tag for c in case}
            status = "failed" if tags & {"failure", "error"} else "skipped" if "skipped" in tags else "passed"
            out.append({"unit": unit, "kind": "nextest", "crate": binary.split("::")[0], "features": feats,
                        "binary": binary, "name": case.get("name", ""), "status": status,
                        "seconds": float(case.get("time") or 0)})
    return out
DOC = re.compile(r"^test (.+?) +\.\.\. (ok|FAILED|ignored|TIMEOUT|NOT_RUN)\b")
DOC_STATUS = {"ok": "passed", "FAILED": "failed", "ignored": "skipped", "TIMEOUT": "timed_out", "NOT_RUN": "not_run"}
def doctests(path, unit, crate, feats):
    out = []
    if path.exists():
        for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
            m = DOC.match(line)
            if m:
                out.append({"unit": unit, "kind": "doctest", "crate": crate, "features": feats, "binary": "doctest",
                            "name": m.group(1), "status": DOC_STATUS[m.group(2)], "seconds": None})
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
    units.append(u)
    tests += got
per_crate = {}
for t in tests:
    if t["unit"] == "default":
        c = per_crate.setdefault(t["crate"], {k: 0 for k in STATUSES})
        c[t["status"]] += 1
default = units[0]
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
    "canary_filter": E["RR_FILTER"], "exit": int(E["RR_EXIT"]),
    "absent_units": absent,
    "timed_out_tests": [t["unit"] + ": " + t["name"] for t in tests if t["status"] == "timed_out"],
    "feature_matrix": {"file": ".config/round-features.toml", "sha256": hashlib.sha256(mx).hexdigest() if mx else None},
    "doctest_per_test_timeout_s": int(E["RR_DOCTEST_BUDGET"]),
    "units": units, "per_crate_default": dict(sorted(per_crate.items())),
    "tests_file": "tests.json", "tests_reported": len(tests),
    "tools": {"rustc": E["RR_RUSTC"], "cargo": E["RR_CARGO"], "nextest": E["RR_NEXTEST"]},
    "rustc_wrapper": E["RR_WRAPPER"],
    "env_set": ["CARGO_TARGET_DIR", "TMP", "TEMP", "TMPDIR", "CARGO_PROFILE_DEV_DEBUG", "CARGO_PROFILE_TEST_DEBUG", "CARGO_TERM_COLOR"],
    "env_unset": E["RR_UNSET"], "target_dir": E["RR_TARGET"],
}
(res / "tests.json").write_text(json.dumps({"schema": "hsk.studio.round-tests@1", "sha": E["RR_SHA"], "tests": tests},
                                           indent=1) + "\n", encoding="utf-8", newline="\n")
(res / "round.json").write_text(json.dumps(round_json, indent=1) + "\n", encoding="utf-8", newline="\n")
for u in units:
    print("  %-48s %-24s build=%s run=%s pass=%d fail=%d skip=%d timeout=%d not_run=%d" % (u["unit"], u["status"],
          u["build_exit"], u["run_exit"], u["passed"], u["failed"], u["skipped"], u["timed_out"], u["not_run"]))
PY
  reason="results aggregation failed (raw files kept)"
  echo "run-round: $reason" >&2
  exit 70
fi
echo "run-round: exit=$code -> $results/round.json"
exit "$code"
