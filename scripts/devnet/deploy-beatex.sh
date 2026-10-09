#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_root="$(cd "$script_dir/../.." && pwd)"

cluster='localnet'
wallet=''
program_keypair='.devnet/program/beatex-keypair.json'
state_dir=''
skip_build=0
allow_redeploy=0

usage() {
  cat <<'EOF'
Usage:
  deploy-beatex.sh --wallet <KEYPAIR> [options]

Options:
  --cluster localnet|devnet       Target cluster (default: localnet)
  --wallet <KEYPAIR>              Fee payer and upgrade authority (required)
  --program-keypair <KEYPAIR>     Stable BeatEx program keypair
  --state-dir <DIRECTORY>         Deployment manifest directory (default: .devnet/<cluster>)
  --skip-build                    Deploy the existing target/deploy-v0/beatex.so
  --allow-redeploy                Explicitly authorize upgrading an existing program
  -h, --help                      Show this help

The script refuses mainnet and validates that the program keypair, Anchor.toml,
and Rust declare_id! all name the same program before building or deploying.
If the program already exists, an interactive confirmation is required unless
--allow-redeploy was supplied by an enclosing, already-confirmed workflow.
EOF
}

while (( $# > 0 )); do
  case "$1" in
    --cluster)
      cluster="${2:?--cluster requires a value}"
      shift 2
      ;;
    --wallet)
      wallet="${2:?--wallet requires a value}"
      shift 2
      ;;
    --program-keypair)
      program_keypair="${2:?--program-keypair requires a value}"
      shift 2
      ;;
    --state-dir)
      state_dir="${2:?--state-dir requires a value}"
      shift 2
      ;;
    --skip-build)
      skip_build=1
      shift
      ;;
    --allow-redeploy)
      allow_redeploy=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'Unknown argument: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

case "$cluster" in
  localnet) rpc_url='http://127.0.0.1:8899' ;;
  devnet) rpc_url='https://api.devnet.solana.com' ;;
  *)
    printf 'Unsupported cluster %q; only localnet and devnet are allowed.\n' "$cluster" >&2
    exit 2
    ;;
esac

if [[ -z "$wallet" || ! -f "$wallet" ]]; then
  printf 'A valid --wallet keypair is required.\n' >&2
  exit 2
fi
wallet="$(cd "$(dirname "$wallet")" && pwd)/$(basename "$wallet")"
if [[ "$program_keypair" != /* ]]; then
  program_keypair="$project_root/$program_keypair"
fi
if [[ -z "$state_dir" ]]; then
  state_dir="$project_root/.devnet/$cluster"
elif [[ "$state_dir" != /* ]]; then
  state_dir="$project_root/$state_dir"
fi
if [[ ! -f "$program_keypair" ]]; then
  printf 'Program keypair does not exist: %s\n' "$program_keypair" >&2
  exit 2
fi
cd "$project_root"

required_commands=(solana solana-keygen node)
if (( ! skip_build )); then
  required_commands+=(cargo-build-sbf)
fi
for required_command in "${required_commands[@]}"; do
  if ! command -v "$required_command" >/dev/null 2>&1; then
    printf 'Missing required command: %s\n' "$required_command" >&2
    exit 1
  fi
done

declared_id="$(sed -n 's/.*declare_id!("\([^"]*\)").*/\1/p' programs/beatex/src/lib.rs)"
anchor_program_id() {
  local section="$1"
  awk -v target="[programs.${section}]" '
    $0 == target { active = 1; next }
    /^\[/ { active = 0 }
    active && /^beatex = / { gsub(/[" ]/, "", $3); print $3; exit }
  ' Anchor.toml
}

localnet_id="$(anchor_program_id localnet)"
devnet_id="$(anchor_program_id devnet)"
program_key_id="$(solana-keygen pubkey "$program_keypair")"

if [[ -z "$declared_id" || "$declared_id" != "$localnet_id" || "$declared_id" != "$devnet_id" || "$declared_id" != "$program_key_id" ]]; then
  printf 'Program ID mismatch:\n' >&2
  printf '  declare_id!:        %s\n' "$declared_id" >&2
  printf '  Anchor.toml local:  %s\n' "$localnet_id" >&2
  printf '  Anchor.toml devnet: %s\n' "$devnet_id" >&2
  printf '  keypair:            %s\n' "$program_key_id" >&2
  exit 1
fi

program_status=''
if program_status="$(solana program show "$program_key_id" --url "$rpc_url" 2>&1)"; then
  printf 'BeatEx program %s is already deployed on %s.\n' "$program_key_id" "$cluster"
  if (( ! allow_redeploy )); then
    if [[ ! -t 0 ]]; then
      printf '%s\n' 'Refusing an unattended redeploy. Re-run interactively or pass --allow-redeploy after explicit approval.' >&2
      exit 1
    fi
    read -r -p 'Redeploy and upgrade this program? [y/N] ' confirmation
    case "$confirmation" in
      y|Y|yes|YES|Yes) ;;
      *)
        printf 'Deployment left unchanged.\n'
        exit 0
        ;;
    esac
  fi
elif [[ "$program_status" =~ [Aa]ccount.*does[[:space:]]not[[:space:]]exist ]]; then
  printf 'BeatEx program %s is not deployed on %s; this will be its first deployment.\n' \
    "$program_key_id" "$cluster"
else
  printf 'Could not determine whether BeatEx program %s is deployed on %s:\n%s\n' \
    "$program_key_id" "$cluster" "$program_status" >&2
  exit 1
fi

if (( ! skip_build )); then
  "$script_dir/../build-pipeline.sh" --contract-only \
    --cluster "$cluster"
fi

solana program deploy "$project_root/target/deploy-v0/beatex.so" \
  --program-id "$program_keypair" \
  --keypair "$wallet" \
  --url "$rpc_url"

generator_args=(
  --cluster "$cluster"
  --state-dir "$state_dir"
)
if [[ -f "$project_root/target/idl/beatex.json" ]]; then
  generator_args+=(--idl "$project_root/target/idl/beatex.json")
fi
node "$project_root/scripts/generate-protocol-artifacts.mjs" "${generator_args[@]}"
