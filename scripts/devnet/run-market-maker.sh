#!/usr/bin/env bash
set -euo pipefail

# Run exactly one market-maker process.  Open another terminal and invoke this
# script again with a different --source when two independent makers are wanted.

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_root="$(cd "$script_dir/../.." && pwd)"
source "$script_dir/load-rpc-env.sh"
user_cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
if [[ -d "$user_cargo_bin" ]]; then
  PATH="$user_cargo_bin:$PATH"
fi
if [[ -d "$project_root/.devnet/tools/bin" ]]; then
  PATH="$project_root/.devnet/tools/bin:$PATH"
fi

source=''
interval_ms='200'
balance_every_auctions='10'
budget_bps='1000'
state_dir="$project_root/.devnet/devnet"
rpc_url=''
base_quantity='0.0005'
slope_quantity='0.0001'
max_quantity='0.001'
once=0

usage() {
  cat <<'EOF'
Usage:
  run-market-maker.sh --source pyth|binance [options]

Options:
  --source <SOURCE>               Price source (required: pyth or binance)
  --interval-ms <MILLISECONDS>    Auction polling interval (default: 200)
  --balance-every-auctions <N>    Print the private balance every N auctions (default: 10)
  --budget-bps <N>                Maximum private balance committed per auction (default: 1000 = 10%)
  --state-dir <DIRECTORY>         Setup state (default: .devnet/devnet)
  --rpc-url <URL>                 Solana RPC URL
  --base-quantity <TBTC>          Quantity nearest the expected price
  --slope-quantity <TBTC>         Additional quantity per grid step
  --max-quantity <TBTC>           Maximum quantity at one level
  --once                          Produce one order and exit
  -h, --help                      Show this help

The market and maker keypair are read from the setup manifests. The script
does not ask for reference-price or tick-size: those are read from the active
on-chain auction by the Rust bot. BEATEX_RPC_SESSION_URL may select one
consistent provider for bot reads and writes; BEATEX_RPC_FALLBACK_URLS is a
comma-separated list of same-cluster backups.
EOF
}

while (( $# > 0 )); do
  case "$1" in
    --source) source="${2:?--source requires a value}"; shift 2 ;;
    --interval-ms) interval_ms="${2:?--interval-ms requires a value}"; shift 2 ;;
    --interval-seconds) interval_ms="$(( ${2:?--interval-seconds requires a value} * 1000 ))"; shift 2 ;;
    --balance-every-auctions) balance_every_auctions="${2:?--balance-every-auctions requires a value}"; shift 2 ;;
    --budget-bps) budget_bps="${2:?--budget-bps requires a value}"; shift 2 ;;
    --state-dir) state_dir="${2:?--state-dir requires a value}"; shift 2 ;;
    --rpc-url) rpc_url="${2:?--rpc-url requires a value}"; shift 2 ;;
    --base-quantity) base_quantity="${2:?--base-quantity requires a value}"; shift 2 ;;
    --slope-quantity) slope_quantity="${2:?--slope-quantity requires a value}"; shift 2 ;;
    --max-quantity) max_quantity="${2:?--max-quantity requires a value}"; shift 2 ;;
    --once) once=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) printf 'Unknown argument: %s\n' "$1" >&2; usage >&2; exit 2 ;;
  esac
done

case "$source" in
  pyth|binance) ;;
  '') printf '%s\n' '--source is required.' >&2; usage >&2; exit 2 ;;
  *) printf 'Unsupported source %q; use pyth or binance.\n' "$source" >&2; exit 2 ;;
esac

if [[ "$state_dir" != /* ]]; then state_dir="$project_root/$state_dir"; fi
beatex_load_rpc_env "$state_dir"
if [[ -z "$rpc_url" ]]; then
  rpc_url="${BEATEX_RPC_SESSION_URL:-${BEATEX_RPC_URL:-https://api.devnet.solana.com}}"
fi
market_addresses="$state_dir/market.tsv"
maker_keypair="$state_dir/market-makers/${source}-maker.json"
if [[ ! -f "$market_addresses" ]]; then
  printf 'Missing market manifest: %s\nRun devnetSetup.sh first.\n' "$market_addresses" >&2
  exit 1
fi
market="$(awk -F '\t' '$1 == "market" { print $2 }' "$market_addresses")"
if [[ -z "$market" ]]; then printf 'Could not read market from %s.\n' "$market_addresses" >&2; exit 1; fi
if [[ ! -f "$maker_keypair" ]]; then
  printf 'Missing %s; run devnetSetup.sh first.\n' "$maker_keypair" >&2
  exit 1
fi
cd "$project_root"
maker_binary="$project_root/target/release/beatex-market-maker"
printf 'Building market-maker binary (Cargo reuses unchanged artifacts).\n'
cargo build --release -p beatex-devnet-client -p beatex-market-maker --no-default-features --manifest-path "$project_root/Cargo.toml"
if [[ ! -x "$maker_binary" ]]; then printf 'Market-maker binary was not produced: %s\n' "$maker_binary" >&2; exit 1; fi

args=(
  --source "$source"
  --market "$market"
  --rpc-url "$rpc_url"
  --base-quantity "$base_quantity"
  --slope-quantity "$slope_quantity"
  --max-quantity "$max_quantity"
  --interval-ms "$interval_ms"
  --state-dir "$state_dir"
  --wallet "$maker_keypair"
  --balance-every-auctions "$balance_every_auctions"
  --budget-bps "$budget_bps"
)
if (( once )); then args+=(--once); fi

printf 'Starting native Rust %s market maker for market %s.\n' "$source" "$market"
exec env \
  BEATEX_MARKET_MAKER_KEYPAIR="$maker_keypair" \
  BEATEX_STATE_DIR="$state_dir" \
  BEATEX_RPC_URL="$rpc_url" \
  BEATEX_RPC_FALLBACK_URLS="${BEATEX_RPC_FALLBACK_URLS:-}" \
  "$maker_binary" "${args[@]}"
