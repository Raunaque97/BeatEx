#!/usr/bin/env bash
set -euo pipefail

reference_price=''
tick_size=''
interval_seconds='10'
executor='scripts/devnet/execute-market-maker-order.sh'
state_dir='.devnet/devnet'

usage() {
  cat <<'EOF'
Usage:
  run-market-makers.sh --reference-price <LOTS> --tick-size <LOTS> [options]

Options:
  --interval-seconds <SECONDS>   Repricing interval (default: 10)
  --state-dir <DIRECTORY>        Setup state (default: .devnet/devnet)
  --executor <PROGRAM>           Proof/sign/send adapter receiving JSON on stdin
  --print-only                   Print intents without sending transactions
  -h, --help                     Show this help

Starts one Pyth-priced and one Binance-priced maker. Without --executor the
processes print order intents, which is useful for strategy inspection.
EOF
}

while (( $# > 0 )); do
  case "$1" in
    --reference-price)
      reference_price="${2:?--reference-price requires a value}"
      shift 2
      ;;
    --tick-size)
      tick_size="${2:?--tick-size requires a value}"
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

if [[ -z "$reference_price" || -z "$tick_size" ]]; then
  printf '%s\n' '--reference-price and --tick-size are required.' >&2
  exit 2
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
    run --release -p beatex-market-maker --
    --source "$source"
    --reference-price "$reference_price"
    --tick-size "$tick_size"
    --interval-seconds "$interval_seconds"
  )
  if [[ -n "$executor" ]]; then
    args+=(--executor "$executor")
  fi
  BEATEX_MARKET_MAKER_KEYPAIR="$maker_keypair" \
    BEATEX_STATE_DIR="$state_dir" \
    cargo "${args[@]}" &
  pids+=("$!")
done

wait -n "${pids[@]}"
