#!/usr/bin/env bash
set -euo pipefail

cluster='localnet'
wallet=''
program_keypair='.demo/program/beatex-keypair.json'
skip_build=0

usage() {
  cat <<'EOF'
Usage:
  deploy-beatex.sh --wallet <KEYPAIR> [options]

Options:
  --cluster localnet|devnet       Target cluster (default: localnet)
  --wallet <KEYPAIR>              Fee payer and upgrade authority (required)
  --program-keypair <KEYPAIR>     Stable BeatEx program keypair
  --skip-build                    Deploy the existing target/deploy-v0/beatex.so
  -h, --help                      Show this help

The script refuses mainnet and validates that the program keypair, Anchor.toml,
and Rust declare_id! all name the same program before building or deploying.
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
    --skip-build)
      skip_build=1
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
if [[ ! -f "$program_keypair" ]]; then
  printf 'Program keypair does not exist: %s\n' "$program_keypair" >&2
  exit 2
fi

required_commands=(solana solana-keygen)
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

if (( ! skip_build )); then
  cargo-build-sbf --arch v0 --features sbf-test-fixtures \
    --manifest-path programs/beatex/Cargo.toml \
    --sbf-out-dir target/deploy-v0
fi

solana program deploy target/deploy-v0/beatex.so \
  --program-id "$program_keypair" \
  --keypair "$wallet" \
  --url "$rpc_url"
