#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_root="$(cd "$script_dir/../.." && pwd)"
user_cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
if [[ -d "$user_cargo_bin" ]]; then
  PATH="$PATH:$user_cargo_bin"
fi

interval_seconds='10'
executor='scripts/devnet/execute-market-maker-order.sh'
state_dir='.devnet/devnet'
rpc_url="${BEATEX_RPC_URL:-https://api.devnet.solana.com}"
base_quantity='0.0001'
slope_quantity='0.0001'
max_quantity='0.001'

usage() {
  cat <<'EOF'
Usage:
  run-market-makers.sh [options]

Options:
  --interval-seconds <SECONDS>   Repricing interval (default: 10)
  --state-dir <DIRECTORY>        Setup state (default: .devnet/devnet)
  --rpc-url <URL>                Solana RPC URL
  --base-quantity <TBTC>         Quantity nearest the expected price
  --slope-quantity <TBTC>        Additional quantity per grid step
  --max-quantity <TBTC>          Maximum quantity at one level
  --executor <PROGRAM>           Proof/sign/send adapter receiving JSON on stdin
  --print-only                   Print intents without sending transactions
  -h, --help                     Show this help

Starts one Pyth-priced and one Binance-priced maker. Each maker reads the
current Market and Auction grid before every order. Without --executor, the
current Auction must already exist and the processes only print order intents.
EOF
}

while (( $# > 0 )); do
  case "$1" in
    --rpc-url)
      rpc_url="${2:?--rpc-url requires a value}"
      shift 2
      ;;
    --base-quantity)
      base_quantity="${2:?--base-quantity requires a value}"
      shift 2
      ;;
    --slope-quantity)
      slope_quantity="${2:?--slope-quantity requires a value}"
      shift 2
      ;;
    --max-quantity)
      max_quantity="${2:?--max-quantity requires a value}"
      shift 2
      ;;
    --interval-seconds)
      interval_seconds="${2:?--interval-seconds requires a value}"
      shift 2
      ;;
    --executor)
      executor="${2:?--executor requires a value}"
      shift 2
      ;;
    --print-only)
      executor=''
      shift
      ;;
    --state-dir)
      state_dir="${2:?--state-dir requires a value}"
      shift 2
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

if [[ "$state_dir" != /* ]]; then
  state_dir="$project_root/$state_dir"
fi
if [[ "$executor" != /* ]]; then
  executor="$project_root/$executor"
fi

market_addresses="$state_dir/market.tsv"
if [[ ! -f "$market_addresses" ]]; then
  printf 'Missing market address manifest: %s\n' "$market_addresses" >&2
  exit 1
fi
market="$(awk -F '\t' '$1 == "market" { print $2 }' "$market_addresses")"
if [[ -z "$market" ]]; then
  printf 'Could not read the market address from %s.\n' "$market_addresses" >&2
  exit 1
fi

pids=()
stop_children() {
  if (( ${#pids[@]} > 0 )); then
    kill "${pids[@]}" 2>/dev/null || true
  fi
}
trap stop_children EXIT INT TERM

for source in pyth binance; do
  maker_name="${source}-maker"
  maker_keypair="$state_dir/market-makers/$maker_name.json"
  if [[ ! -f "$maker_keypair" ]]; then
    printf 'Missing market-maker keypair: %s\nRun devnetSetup.sh first.\n' "$maker_keypair" >&2
    exit 1
  fi
  args=(
    run --release -p beatex-market-maker --manifest-path "$project_root/Cargo.toml" --
    --source "$source"
    --market "$market"
    --rpc-url "$rpc_url"
    --base-quantity "$base_quantity"
    --slope-quantity "$slope_quantity"
    --max-quantity "$max_quantity"
    --interval-seconds "$interval_seconds"
  )
  if [[ -n "$executor" ]]; then
    args+=(--executor "$executor")
  fi
  BEATEX_MARKET_MAKER_KEYPAIR="$maker_keypair" \
    BEATEX_STATE_DIR="$state_dir" \
    BEATEX_RPC_URL="$rpc_url" \
    cargo "${args[@]}" &
  pids+=("$!")
done

wait -n "${pids[@]}"
