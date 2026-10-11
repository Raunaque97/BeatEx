#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_root="$(cd "$script_dir/.." && pwd)"
user_cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
if [[ -d "$user_cargo_bin" ]]; then
  PATH="$user_cargo_bin:$PATH"
fi

contract_only=0
skip_contract=0
skip_wasm=0
skip_web=0
cluster='devnet'
state_dir=''

usage() {
  cat <<'EOF'
Usage:
  scripts/build-pipeline.sh [options]

Options:
  --contract-only       Build SBF/IDL and regenerate protocol artifacts only
  --skip-contract       Reuse the existing contract/IDL output
  --skip-wasm           Skip the Rust trader WASM/prover asset build
  --skip-web            Skip the Next.js application build
  --cluster <CLUSTER>   Deployment metadata cluster (default: devnet)
  --state-dir <DIR>     Deployment manifest directory
  -h, --help            Show this help

The pipeline treats the Anchor program as the source of truth. It regenerates
the SDK deployment constants, Rust prover constants, IDL hash, and deployment
manifest before building consumers. It never deploys or sends a transaction.
EOF
}

while (( $# > 0 )); do
  case "$1" in
    --contract-only) contract_only=1; shift ;;
    --skip-contract) skip_contract=1; shift ;;
    --skip-wasm) skip_wasm=1; shift ;;
    --skip-web) skip_web=1; shift ;;
    --cluster) cluster="${2:?--cluster requires a value}"; shift 2 ;;
    --state-dir) state_dir="${2:?--state-dir requires a value}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) printf 'Unknown argument: %s\n' "$1" >&2; usage >&2; exit 2 ;;
  esac
done

if (( contract_only && (skip_contract || skip_wasm || skip_web) )); then
  printf '%s\n' '--contract-only cannot be combined with skip options.' >&2
  exit 2
fi

if [[ -z "$state_dir" ]]; then
  state_dir="$project_root/.devnet/$cluster"
elif [[ "$state_dir" != /* ]]; then
  state_dir="$project_root/$state_dir"
fi

if (( ! skip_contract )); then
  for command_name in cargo-build-sbf anchor; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
      printf 'Missing required build command: %s\n' "$command_name" >&2
      exit 1
    fi
  done
  cargo-build-sbf --arch v0 \
    --manifest-path "$project_root/programs/beatex/Cargo.toml" \
    --sbf-out-dir "$project_root/target/deploy-v0"
  (
    cd "$project_root"
    mkdir -p target/idl
    anchor idl build --out target/idl/beatex.json
  )
fi

if ! command -v node >/dev/null 2>&1; then
  printf 'Missing required build command: node\n' >&2
  exit 1
fi

generator_args=(
  --cluster "$cluster"
  --state-dir "$state_dir"
)
if [[ -f "$project_root/target/idl/beatex.json" ]]; then
  generator_args+=(--idl "$project_root/target/idl/beatex.json")
elif (( ! skip_contract )); then
  printf 'Anchor IDL was not produced at %s.\n' "$project_root/target/idl/beatex.json" >&2
  exit 1
fi
node "$script_dir/generate-protocol-artifacts.mjs" "${generator_args[@]}"

if (( contract_only )); then
  printf '%s\n' 'Contract and protocol artifact build complete.'
  exit 0
fi

"${CARGO_BIN:-cargo}" check --workspace
pnpm --dir "$project_root/packages/sdk" build
pnpm --dir "$project_root/packages/prover-wasm" build

if (( ! skip_wasm )); then
  "$script_dir/build-trader-wasm.sh" "$project_root/target/generated/prover-assets"
  "$script_dir/sync-web-prover-assets.sh" "$project_root/target/generated/prover-assets"
fi

if (( ! skip_web )); then
  pnpm --dir "$project_root" --filter @beatex/web build
fi

printf '%s\n' 'BeatEx build pipeline complete.'
