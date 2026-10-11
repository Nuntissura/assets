#!/usr/bin/env bash
# Studio round entrypoint: the single checked-in build + test invocation for validator rounds.
# Usage: tools/run-round.sh --results DIR --target-dir DIR [--dev] [--canary] (--all | CRATE...)
#   CRATE      hsk-studio-<name> or <name>; --all = whole workspace.
#   --dev      allow the git edit tree; otherwise the workspace must be an export with ROUND_IDENTITY.json
#              in its parent or grandparent whose manifest_sha256 (sorted "<sha256>  <relpath>\n" lines over
#              the export dir beside it, named src/) matches. Exports get current mtimes on all workspace files
#              (no stale cross-tree artifact reuse) and run cargo with --locked.
#   --canary   first listed test of every selected package, nextest profile "canary".
# Step 0 (export, read-only, ~1 s): `cargo metadata --locked --offline` and `tools/registry-sync.py` check mode;
#   drift exits 75 (infrastructure: the candidate needs a steward sync commit; nothing is rewritten here).
# Steps: one build (cargo nextest run --no-run), one run (cargo nextest run --no-fail-fast --profile P).
# Env: sets CARGO_TARGET_DIR, TMP/TEMP/TMPDIR (<target-dir>/../tmp), CARGO_PROFILE_{DEV,TEST}_DEBUG,
# CARGO_TERM_COLOR; unsets every HSK_* variable. Nothing else is required from the caller.
# Results: round.json, build.*.txt, run.*.txt, junit.xml, nextest-store.toml (+ list.txt, canary-filter.txt,
# step0.*.txt).
# Exit: 2 refused, 75 step 0 drift (infrastructure), build failure -> its exit code, else the test run's exit
# code (0 = all selected tests passed).
set -euo pipefail

die() { echo "run-round: $*" >&2; exit 2; }
jstr() { local s=${1//\\/\\\\}; s=${s//\"/\\\"}; printf '"%s"' "$s"; }
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

root=$(abspath "$(dirname "${BASH_SOURCE[0]}")/..")
target=$(abspath "$target")
results=$(abspath "$results")
for p in "$target" "$results"; do
  case "$p" in [cC]:*|/c/*) die "refusing C: path $p (build output and temp stay off C:)" ;; esac
  under "$p" "$root" && die "refusing path inside the workspace: $p"
done
[ -e "$results/round.json" ] && die "results dir already holds a round.json: $results"

# Identity: export (default) or edit tree (--dev).
identity="" identity_sha="" identity_tree="" manifest="" dirty=""
for cand in "$root/../ROUND_IDENTITY.json" "$root/../../ROUND_IDENTITY.json"; do
  if [ -f "$cand" ]; then identity="$(abspath "$(dirname "$cand")")/ROUND_IDENTITY.json"; break; fi
done
if [ $dev -eq 0 ]; then
  [ -n "$identity" ] || die "not an export (no ROUND_IDENTITY.json beside it); use --dev for the edit tree"
  git -C "$root" rev-parse --git-dir >/dev/null 2>&1 && die "workspace is a git checkout, not an export"
  export_dir="$(dirname "$identity")/src"
  [ -d "$export_dir" ] && under "$root" "$export_dir" || die "workspace is not inside $export_dir"
  under "$results" "$export_dir" && die "results dir must be outside the export"
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
  py=$(command -v python || command -v python3) || die "python 3.11+ required for step 0"
  if ! "$py" -X utf8 -I tools/registry-sync.py . >"$results/step0.registry.txt" 2>&1; then
    echo "run-round: infrastructure: registry metadata/probe approvals drift (step0.registry.txt); needs a steward sync commit" >&2
    exit 75
  fi
  # Cargo hashes path packages relative to the workspace root, so a shared target dir can treat an export
  # whose files carry older (archive/commit) mtimes as fresh and reuse artifacts built from other content.
  # Current mtimes force every workspace crate to rebuild from the export (content and manifest unchanged).
  find "$root" -type f -print0 | xargs -0 touch
fi

if [ $all -eq 1 ]; then sel=(--workspace); else sel=(); for c in "${crates[@]}"; do sel+=(-p "$c"); done; fi
profile=default
started=$(date -u +%Y-%m-%dT%H:%M:%SZ)
set +e
cargo nextest run "${nx[@]}" --no-run "${locked[@]}" "${sel[@]}" >"$results/build.stdout.txt" 2>"$results/build.stderr.txt"
build_exit=$?
run_exit="" filter=""
if [ $build_exit -eq 0 ] && [ $canary -eq 1 ]; then
  profile=canary
  cargo nextest list "${nx[@]}" --message-format oneline "${locked[@]}" "${sel[@]}" >"$results/list.txt" 2>"$results/list.stderr.txt"
  list_exit=$?
  filter=$(awk '{ split($1, b, "::"); if (!(b[1] in seen)) { seen[b[1]] = 1;
      printf "%s(binary_id(=%s) & test(=%s))", (n++ ? " | " : ""), $1, $2 } }' "$results/list.txt")
  printf '%s\n' "$filter" >"$results/canary-filter.txt"
  if [ $list_exit -ne 0 ]; then build_exit=$list_exit; elif [ -z "$filter" ]; then build_exit=3; fi
fi
if [ $build_exit -eq 0 ]; then
  run=(cargo nextest run "${nx[@]}" --no-fail-fast --no-tests=warn --profile "$profile" "${locked[@]}" "${sel[@]}")
  [ -n "$filter" ] && run+=(-E "$filter")
  "${run[@]}" >"$results/run.stdout.txt" 2>"$results/run.stderr.txt"
  run_exit=$?
  [ -f "$target/nextest/$profile/junit.xml" ] && cp "$target/nextest/$profile/junit.xml" "$results/junit.xml"
fi
set -e
ended=$(date -u +%Y-%m-%dT%H:%M:%SZ)
summary=$(grep -hE '^ *Summary ' "$results/run.stdout.txt" "$results/run.stderr.txt" 2>/dev/null | tail -n1 | sed 's/^ *//' || true)

{
  printf '{\n "schema": "hsk.studio.round-run@1",\n'
  printf ' "mode": %s,\n "profile": %s,\n' "$(jstr "$([ $dev -eq 1 ] && echo dev || echo export)")" "$(jstr "$profile")"
  printf ' "selection": %s,\n' "$(jstr "$([ $all -eq 1 ] && echo --all || echo "${crates[*]}")")"
  printf ' "sha": %s,\n "tree": %s,\n' "$(jstr "$identity_sha")" "$(jstr "$identity_tree")"
  printf ' "identity_file": %s,\n "export_manifest_sha256": %s,\n' "$(jstr "$identity")" "$(jstr "$manifest")"
  printf ' "lock_sha256": %s,\n' "$(jstr "$(sha256sum Cargo.lock | cut -c1-64)")"
  printf ' "edit_tree_dirty_entries": %s,\n' "$(jstr "$dirty")"
  printf ' "started_at": %s,\n "ended_at": %s,\n' "$(jstr "$started")" "$(jstr "$ended")"
  printf ' "build_exit": %s,\n "run_exit": %s,\n' "$build_exit" "${run_exit:-null}"
  printf ' "summary": %s,\n' "$(jstr "$summary")"
  printf ' "canary_filter": %s,\n' "$(jstr "$filter")"
  printf ' "tools": {"rustc": %s, "nextest": %s},\n' "$(jstr "$(rustc -V 2>/dev/null)")" "$(jstr "$(cargo nextest --version 2>/dev/null | head -n1)")"
  printf ' "env_set": ["CARGO_TARGET_DIR", "TMP", "TEMP", "TMPDIR", "CARGO_PROFILE_DEV_DEBUG", "CARGO_PROFILE_TEST_DEBUG", "CARGO_TERM_COLOR"],\n'
  printf ' "env_unset": %s,\n' "$(jstr "$(echo $unset_vars)")"
  printf ' "target_dir": %s\n}\n' "$(jstr "$target")"
} >"$results/round.json"
echo "run-round: build_exit=$build_exit run_exit=${run_exit:-skipped} ${summary} -> $results/round.json"
[ $build_exit -ne 0 ] && exit "$build_exit"
exit "${run_exit:-1}"
