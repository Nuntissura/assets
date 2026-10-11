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
#   features   per selected crate and feature set declared in .config/round-features.toml: -p CRATE --features SET,
#              own build + run under features/CRATE+F1+F2/. A set naming a feature the candidate lacks is recorded
#              feature_absent (not run, no exit impact).
#   doctest    `cargo test --doc` per selected crate with a doctested lib target, default features and every
#              declared set (the per-test runner skips doctests): doctest/CRATE[+F1...].{stdout,stderr}.txt.
# Results: round.json (identity, env, every unit with exits and passed/failed/skipped counts, per-crate counts),
#   tests.json (every reported test: unit, crate, binary, name, status, seconds; passing names included),
#   junit.xml per nextest unit (passing tests' output stored, skipped tests listed: .config/nextest.toml),
#   metadata.json (cargo metadata --no-deps), units.tsv, step0.*.txt, nextest-store.toml.
# Env: sets CARGO_TARGET_DIR, TMP/TEMP/TMPDIR (<target-dir>/../tmp), CARGO_PROFILE_{DEV,TEST}_DEBUG,
#   CARGO_TERM_COLOR; unsets every HSK_* variable. Nothing else is required from the caller (python 3.11+).
# Exit: 2 refused, 75 step 0 drift (infrastructure), 70 results aggregation failed (raw files kept), else the
#   first failing unit build's exit code (default unit first), else the first failing test run's exit code, else 0.
set -euo pipefail

die() { echo "run-round: $*" >&2; exit 2; }
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
    echo "run-round: infrastructure: Cargo.lock not in sync with the candidate manifests (step0.lock.txt); needs a steward sync commit" >&2
    exit 75
  fi
  if ! "$py" -X utf8 -I tools/registry-sync.py . >"$results/step0.registry.txt" 2>&1; then
    echo "run-round: infrastructure: registry metadata/probe approvals drift (step0.registry.txt); needs a steward sync commit" >&2
    exit 75
  fi
  # Cargo hashes path packages relative to the workspace root, so a shared target dir can treat an export
  # whose files carry older (archive/commit) mtimes as fresh and reuse artifacts built from other content.
  # Current mtimes force every workspace crate to rebuild from the export (content and manifest unchanged).
  find "$root" -type f -print0 | xargs -0 touch
fi

# Plan: selection, feature matrix units and doctest units from the candidate's own manifests (read-only).
cargo metadata --no-deps --offline --format-version 1 ${locked[@]+"${locked[@]}"} >"$results/metadata.json" 2>"$results/metadata.stderr.txt" \
  || die "cargo metadata --no-deps failed (metadata.stderr.txt)"
plan="$results/units.plan.tsv"
"$py" -X utf8 -I - "$results/metadata.json" "$root/.config/round-features.toml" "$all" ${crates[@]+"${crates[@]}"} >"$plan" <<'PY' \
  || die "selection or .config/round-features.toml invalid (see above)"
import json, pathlib, sys, tomllib

meta = json.load(open(sys.argv[1], encoding="utf-8"))
matrix_path, sel_all, asked = pathlib.Path(sys.argv[2]), sys.argv[3] == "1", sys.argv[4:]
ids = set(meta["workspace_members"])
members = {p["name"]: p for p in meta["packages"] if p["id"] in ids}
unknown = [c for c in asked if c not in members]
if unknown:
    sys.exit("run-round: not workspace members: " + " ".join(unknown))
selected = sorted(members) if sel_all else list(dict.fromkeys(asked))
matrix = {}
if matrix_path.exists():
    doc = tomllib.loads(matrix_path.read_text(encoding="utf-8"))
    if doc.get("schema") != "hsk.studio.round-features@1":
        sys.exit("run-round: round-features.toml schema must be hsk.studio.round-features@1")
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

if [ $all -eq 1 ]; then sel=(--workspace); else sel=(); for c in "${crates[@]}"; do sel+=(-p "$c"); done; fi
started=$(date -u +%Y-%m-%dT%H:%M:%SZ)
set +e
nextest_unit "$results" - "${sel[@]}"
rec default nextest '*' - run "$U_BUILD" "$U_RUN" .
default_filter=$U_FILTER
mkdir -p "$results/features" "$results/doctest"
while IFS=$'\t' read -r kind crate feats status; do
  name=$crate; [ "$feats" != - ] && name="$crate+${feats//,/+}"
  if [ "$status" != run ]; then
    k=doctest; [ "$kind" = features ] && k=nextest
    rec "$kind:$name" "$k" "$crate" "$feats" "$status" - - -
  elif [ "$kind" = features ]; then
    nextest_unit "$results/features/$name" "$feats" -p "$crate"
    rec "features:$name" nextest "$crate" "$feats" run "$U_BUILD" "$U_RUN" "features/$name"
  else
    fa=(); [ "$feats" != - ] && fa=(--features "$feats")
    cargo test --doc ${locked[@]+"${locked[@]}"} -p "$crate" ${fa[@]+"${fa[@]}"} >"$results/doctest/$name.stdout.txt" 2>"$results/doctest/$name.stderr.txt"
    e=$?
    # A library that does not compile is a build failure; anything else is the doctest run's verdict.
    if [ $e -ne 0 ] && grep -q '^error: could not compile' "$results/doctest/$name.stderr.txt"; then b=$e r=-; else b=0 r=$e; fi
    rec "doctest:$name" doctest "$crate" "$feats" run "$b" "$r" "doctest/$name"
  fi
done <"$plan"
ended=$(date -u +%Y-%m-%dT%H:%M:%SZ)
first_build=0 first_run=0
while IFS=$'\t' read -r _ _ _ _ status b r _; do
  [ "$status" = run ] || continue
  [ "$b" != - ] && [ "$b" != 0 ] && [ $first_build -eq 0 ] && first_build=$b
  [ "$r" != - ] && [ "$r" != 0 ] && [ $first_run -eq 0 ] && first_run=$r
done <"$units"
code=$first_build; [ "$code" -eq 0 ] && code=$first_run
set -e

export RR_MODE=$([ $dev -eq 1 ] && echo dev || echo export) RR_PROFILE=$profile
export RR_SELECTION=$([ $all -eq 1 ] && echo --all || echo "${crates[*]}") RR_SHA=$identity_sha RR_TREE=$identity_tree
export RR_IDENTITY=$identity RR_OWNER_DIR=$owner_dir RR_MANIFEST=$manifest RR_DIRTY=$dirty RR_STARTED=$started RR_ENDED=$ended
export RR_LOCK_SHA=$(sha256sum Cargo.lock | cut -c1-64) RR_FILTER=$default_filter RR_EXIT=$code RR_TARGET=$target
export RR_RUSTC=$(rustc -V 2>/dev/null) RR_CARGO=$(cargo -V 2>/dev/null) RR_NEXTEST=$(cargo nextest --version 2>/dev/null | head -n1)
export RR_UNSET=$(echo $unset_vars) RR_WRAPPER=${RUSTC_WRAPPER:-}
if ! "$py" -X utf8 -I - "$results" "$root/.config/round-features.toml" <<'PY'; then
import hashlib, json, os, pathlib, re, sys
import xml.etree.ElementTree as ET

res, matrix = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
E = os.environ
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
DOC = re.compile(r"^test (.+) \.\.\. (ok|FAILED|ignored)\b")
def doctests(path, unit, crate, feats):
    out = []
    if path.exists():
        for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
            m = DOC.match(line)
            if m:
                status = {"ok": "passed", "FAILED": "failed", "ignored": "skipped"}[m.group(2)]
                out.append({"unit": unit, "kind": "doctest", "crate": crate, "features": feats, "binary": "doctest",
                            "name": m.group(1), "status": status, "seconds": None})
    return out

units, tests = [], []
for line in (res / "units.tsv").read_text(encoding="utf-8").splitlines():
    unit, kind, crate, feats, status, b, r, d = line.split("\t")
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
    for k in ("passed", "failed", "skipped"):
        u[k] = sum(t["status"] == k for t in got)
    units.append(u)
    tests += got
per_crate = {}
for t in tests:
    if t["unit"] == "default":
        c = per_crate.setdefault(t["crate"], {"passed": 0, "failed": 0, "skipped": 0})
        c[t["status"]] += 1
default = units[0]
mx = matrix.read_bytes() if matrix.exists() else b""
round_json = {
    "schema": "hsk.studio.round-run@2",
    "mode": E["RR_MODE"], "profile": E["RR_PROFILE"], "selection": E["RR_SELECTION"],
    "sha": E["RR_SHA"], "tree": E["RR_TREE"], "identity_file": E["RR_IDENTITY"],
    "export_owner_dir": E["RR_OWNER_DIR"], "export_manifest_sha256": E["RR_MANIFEST"],
    "lock_sha256": E["RR_LOCK_SHA"], "edit_tree_dirty_entries": E["RR_DIRTY"],
    "started_at": E["RR_STARTED"], "ended_at": E["RR_ENDED"],
    "build_exit": default["build_exit"], "run_exit": default["run_exit"], "summary": default.get("summary", ""),
    "canary_filter": E["RR_FILTER"], "exit": int(E["RR_EXIT"]),
    "feature_matrix": {"file": ".config/round-features.toml", "sha256": hashlib.sha256(mx).hexdigest() if mx else None},
    "units": units, "per_crate_default": dict(sorted(per_crate.items())),
    "tests_file": "tests.json", "tests_reported": len(tests),
    "tools": {"rustc": E["RR_RUSTC"], "cargo": E["RR_CARGO"], "nextest": E["RR_NEXTEST"]},
    "rustc_wrapper": E["RR_WRAPPER"],
    "env_set": ["CARGO_TARGET_DIR", "TMP", "TEMP", "TMPDIR", "CARGO_PROFILE_DEV_DEBUG", "CARGO_PROFILE_TEST_DEBUG", "CARGO_TERM_COLOR"],
    "env_unset": E["RR_UNSET"], "target_dir": E["RR_TARGET"],
}
(res / "tests.json").write_text(json.dumps({"schema": "hsk.studio.round-tests@1", "sha": E["RR_SHA"], "tests": tests},
                                           indent=1) + "\n", encoding="utf-8")
(res / "round.json").write_text(json.dumps(round_json, indent=1) + "\n", encoding="utf-8")
for u in units:
    print("  %-48s %-16s build=%s run=%s pass=%d fail=%d skip=%d" % (u["unit"], u["status"], u["build_exit"],
          u["run_exit"], u["passed"], u["failed"], u["skipped"]))
PY
  echo "run-round: results aggregation failed (raw files kept in $results)" >&2
  exit 70
fi
echo "run-round: exit=$code -> $results/round.json"
exit "$code"
