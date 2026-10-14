#!/usr/bin/env bash
# Fetch the CC0 DNG test samples used by tests/real_dng.rs into the oracle corpus (TEST DATA ONLY:
# never executed, never committed). Dry run by default; pass --go (after Operator approval) to download.
# Each file is verified against the sha256 published by raw.pixls.us; provenance.json is written next to them.
# Source of truth for license + checksum: https://raw.pixls.us/json/getrepository.php?set=all
# (license link https://creativecommons.org/publicdomain/zero/1.0/ for each id below, checked 2026-10-11).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
CRATE="$(cd "$HERE/../.." && pwd)"
# Fetch destination (tests/real_dng.rs locates the same dir through .config/oracle-corpus.toml):
# the sibling Handshake_Artifacts root next to the worktrees, outside the git worktree. Override: STUDIO_DNG_CORPUS.
CORPUS_RELATIVE="../../../../Handshake_Artifacts/WP-KERNEL-STUDIO/oracle-corpus/dng"
DEST="${STUDIO_DNG_CORPUS:-$(realpath -m "$CRATE/$CORPUS_RELATIVE")}"
# NOTE: id 3839 (samsung) is a JFIF JPEG, not a DNG (kept only for the typed-refusal case); id 1022 replaces it as the uncompressed-strip sample.
# id | local name | sha256 | bytes (approx MB) | remote name (as served by getfile.php/<id>/nice/)
SAMPLES=(
  "2204|canon-eos-5d3-mlv-14bit-lossless.dng|1d77dcc6cdb839b10c1948f0b4471984ab48cf15e973fb77520584cee72b603e|1.47MB|Canon%20-%20EOS%205D%20Mark%20III%20-%2014bit%2014bit%20%282.3471882640587%29.dng"
  "3839|samsung-sm-g973u-16bit-uncompressed.dng|6492b7e0ac8e012524c0c83606a104cc9bf3b68f30fa60c7b73ffbe4dc584ab1|1.26MB|Samsung%20-%20SM-G973U%20-%2016bit%2016bit%20%282.1132075471698%29.dng"
  "1022|adobe-dng-converter-canon-eos-5d3-16bit-uncompressed.dng|95dc76a2f255e623acaa5149b320389f7da7c995da890c3e0d511c0ff681b2e1|46.14MB|Adobe%20DNG%20Converter%20-%20Canon%20EOS%205D%20Mark%20III%20-%2016bit%2016bit%20uncompressed%20%283%3A2%29.DNG"
  "1988|nikon-d80-16bit-lossless-tiled-linearized.dng|5095ca264e763624cd3aa62940286dd1bcff4504eefa1ce61cbb72f50f8426bb|7.90MB|Nikon%20-%20D80%20-%2016bit%20%283%3A2%29.dng"
)
if [[ "${1:-}" != "--go" ]]; then
  echo "dry run; destination: $DEST"
  for s in "${SAMPLES[@]}"; do IFS='|' read -r id name sha size remote <<<"$s"; echo "  $id  $name  $size  https://raw.pixls.us/getfile.php/$id/nice/$remote"; done
  echo "re-run with --go to download"; exit 0
fi
mkdir -p "$DEST"
prov="$DEST/provenance.json"; : > "$prov.tmp"; echo '{"source":"https://raw.pixls.us","license":"https://creativecommons.org/publicdomain/zero/1.0/","use":"test data only, never executed","files":[' >> "$prov.tmp"
first=1
for s in "${SAMPLES[@]}"; do
  IFS='|' read -r id name sha size remote <<<"$s"
  url="https://raw.pixls.us/getfile.php/$id/nice/$remote"
  curl --fail --silent --show-error --location --output "$DEST/$name" "$url"
  got="$(sha256sum "$DEST/$name" | cut -d' ' -f1)"
  [[ "$got" == "$sha" ]] || { echo "CHECKSUM MISMATCH $name: $got != $sha" >&2; rm -f "$DEST/$name"; exit 1; }
  [[ $first -eq 1 ]] || echo ',' >> "$prov.tmp"; first=0
  printf '{"id":%s,"file":"%s","url":"%s","sha256":"%s","bytes":%s,"retrieved_at":"%s"}' "$id" "$name" "$url" "$sha" "$(stat -c %s "$DEST/$name")" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$prov.tmp"
done
echo ']}' >> "$prov.tmp"; mv "$prov.tmp" "$prov"
echo "ok: $DEST"
