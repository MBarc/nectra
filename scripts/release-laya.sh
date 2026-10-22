#!/usr/bin/env bash
# Publishes a Laya model as the GitHub release `nectra classifier laya` downloads.
#
#   scripts/release-laya.sh [--dry-run|--stage-only] MODEL_DIR ORT_DIR TAG [STAGE_DIR]
#
# MODEL_DIR is a retrain --out directory. ORT_DIR holds Microsoft's ONNX Runtime
# files under their release names (onnxruntime-win-x64.dll,
# libonnxruntime-linux-x64.so, libonnxruntime-macos-arm64.dylib,
# ONNXRUNTIME-LICENSE), for runtimes MODEL_DIR doesn't have. Assets, names and
# hashes come from src/laya.rs, so every file is checked against its pin before
# anything is staged; TAG must be the one RELEASE points at. --dry-run only
# prints the commands, --stage-only copies the assets and SHA256SUMS into
# STAGE_DIR without creating the release. GitHub caps an asset at 2 GiB.
set -euo pipefail
mode=run
case "${1:-}" in --dry-run) mode=dry; shift;; --stage-only) mode=stage; shift;; esac
[ $# -ge 3 ] || { sed -n '3,4p' "$0" >&2; exit 2; }
model=$1 ort=$2 tag=$3 stage=${4:-$(mktemp -d)}
src="$(dirname "$0")/../src/laya.rs"
case "$tag" in laya-model-[0-9]*) ;; *) echo "TAG must be laya-model-N" >&2; exit 1;; esac
release=$(grep -o 'releases/download/laya-model-[0-9]*' "$src" | sed 's|.*/||')
[ "$release" = "$tag" ] || { echo "src/laya.rs downloads from $release, not $tag; change RELEASE first" >&2; exit 1; }
sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | awk '{print $1}'; }
run() { printf '%q ' "$@"; echo; [ $mode = dry ] || "$@"; }

# (path in the model directory, release asset, sha256), every platform's runtime included.
pins=$(tr -d ' \n' < "$src" | grep -oE '\("[^"]+","[^"]+","[0-9a-zA-Z]+",?\)' | sed 's/^("//; s/",*)$//; s/","/ /g')
assets=()
run mkdir -p "$stage"
while read -r path asset pin; do
  file="$model/$path"; [ -f "$file" ] || file="$ort/$asset"
  [ -f "$file" ] || { echo "missing $asset: neither $model/$path nor $ort/$asset" >&2; exit 1; }
  got=$(sha256 "$file")
  [ "$got" = "$pin" ] || { echo "$file is $got; src/laya.rs pins $asset to $pin" >&2; exit 1; }
  [ "$(wc -c < "$file")" -lt 2147483648 ] || { echo "$asset is over GitHub's 2 GiB asset limit" >&2; exit 1; }
  run cp "$file" "$stage/$asset"
  assets+=("$stage/$asset")
done <<< "$pins"
echo "(cd $(printf '%q' "$stage") && sha256sum -- ${assets[*]##*/} > SHA256SUMS)"
[ $mode = dry ] || (cd "$stage" && for a in "${assets[@]##*/}"; do echo "$(sha256 "$a")  $a"; done > SHA256SUMS)
gh=(gh release create "$tag" --repo MBarc/nectra --title "Laya model ${tag#laya-model-}"
  --notes-file "$(dirname "$0")/../tools/laya/MODEL_CARD.md" "${assets[@]}" "$stage/SHA256SUMS")
if [ $mode = run ]; then run "${gh[@]}"; else printf '%q ' "${gh[@]}"; echo; fi
