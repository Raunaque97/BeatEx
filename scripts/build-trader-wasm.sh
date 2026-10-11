#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <new-output-directory>" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
output_dir="$1"
rust_manifest="$repo_root/crates/trader-wasm-prover/Cargo.toml"
wasm_input="$repo_root/crates/trader-wasm-prover/target/wasm32-unknown-unknown/release/beatex_trader_wasm_prover.wasm"
generated_dir="$repo_root/packages/prover-wasm/src/generated"
build_temp="$(mktemp -d)"
cargo_bin="${BEATEX_CARGO_BIN:-cargo}"
wasm_bindgen_bin="${BEATEX_WASM_BINDGEN_BIN:-wasm-bindgen}"

cleanup() {
  rm -rf "$build_temp"
}
trap cleanup EXIT

if ! command -v "$cargo_bin" >/dev/null 2>&1; then
  echo "cargo is required; set BEATEX_CARGO_BIN to its absolute path" >&2
  exit 1
fi

if ! command -v "$wasm_bindgen_bin" >/dev/null 2>&1; then
  echo "wasm-bindgen 0.2.100 is required; install it with cargo install wasm-bindgen-cli --version 0.2.100 --locked" >&2
  exit 1
fi

"$cargo_bin" build --manifest-path "$rust_manifest" --release --target wasm32-unknown-unknown
"$wasm_bindgen_bin" "$wasm_input" \
  --target web \
  --out-dir "$build_temp" \
  --out-name beatex_trader_wasm_prover \
  --omit-default-module-path \
  --typescript

mkdir -p "$generated_dir"
cp "$build_temp/beatex_trader_wasm_prover.js" "$generated_dir/beatex_trader_wasm_prover.js"
cp "$build_temp/beatex_trader_wasm_prover.d.ts" "$generated_dir/beatex_trader_wasm_prover.d.ts"

"$cargo_bin" run \
  --manifest-path "$rust_manifest" \
  --release \
  --features devnet-key-export \
  --bin export-devnet-keys \
  -- "$output_dir" "$build_temp/beatex_trader_wasm_prover_bg.wasm"

echo "WASM glue updated in $generated_dir"
echo "Deployable Devnet prover assets written to $output_dir"
