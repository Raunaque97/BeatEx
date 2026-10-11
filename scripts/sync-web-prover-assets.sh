#!/usr/bin/env bash
set -euo pipefail

# Copy the generated trader prover runtime, proving keys, and manifest into the web
# app's static directory. The manifest's SHA-256 digests are verified by the browser.

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
source_dir="${1:-$repo_root/target/generated/prover-assets}"
target_dir="$repo_root/apps/web/public/beatex-prover-assets"

if [[ ! -f "$source_dir/manifest.json" ]]; then
  echo "no prover manifest in $source_dir; run scripts/build-trader-wasm.sh first" >&2
  exit 1
fi

rm -rf "$target_dir"
mkdir -p "$target_dir"
cp "$source_dir"/manifest.json "$source_dir"/*.wasm "$source_dir"/*.pk "$target_dir"/
echo "Prover assets copied to $target_dir"
