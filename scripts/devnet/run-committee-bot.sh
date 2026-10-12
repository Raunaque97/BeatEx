#!/usr/bin/env bash
set -euo pipefail

# Thin launcher for the long-running Rust committee coordinator. Auction
# observation, initialization, folding, publication, cancellation, retries,
# concurrency, transaction construction, signing, and RPC submission all live
# in beatex-devnet-client's `committee` command.

timestamp() { date '+%Y-%m-%d %H:%M:%S'; }
log_block() {
  local line
  while IFS= read -r line; do
    printf '[%s] %s\n' "$(timestamp)" "$line"
  done <<<"$1"
}

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_root="$(cd "$script_dir/../.." && pwd)"
source "$script_dir/load-rpc-env.sh"
user_cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
if [[ -d "$user_cargo_bin" ]]; then PATH="$user_cargo_bin:$PATH"; fi
if [[ -d "$project_root/.devnet/tools/bin" ]]; then
  PATH="$project_root/.devnet/tools/bin:$PATH"
fi

cluster='devnet'
wallet=''
state_dir=''
rpc_url=''
base_symbol='tBTC'
quote_symbol='tUSD'

usage() {
  cat <<'EOF'
Usage:
  run-committee-bot.sh [--wallet <KEYPAIR>] [options]

Options:
  --wallet <KEYPAIR>          Fee payer (default: <state-dir>/devnet-wallet.json)
  --cluster localnet|devnet   Target cluster (default: devnet)
  --state-dir <DIRECTORY>     Setup state (default: .devnet/<cluster>)
  --rpc-url <URL>             Primary Solana RPC URL
  --base-symbol <SYMBOL>      Base token row in addresses.tsv (default: tBTC)
  --quote-symbol <SYMBOL>     Quote token row in addresses.tsv (default: tUSD)
  -h, --help                  Show this help

The launcher builds and execs one long-running Rust process. RPC fallbacks are
loaded from <state-dir>/rpc.env or BEATEX_RPC_FALLBACK_URLS.
EOF
}

while (( $# > 0 )); do
  case "$1" in
    --wallet) wallet="${2:?--wallet requires a value}"; shift 2 ;;
    --cluster) cluster="${2:?--cluster requires a value}"; shift 2 ;;
    --state-dir) state_dir="${2:?--state-dir requires a value}"; shift 2 ;;
    --rpc-url) rpc_url="${2:?--rpc-url requires a value}"; shift 2 ;;
    --base-symbol) base_symbol="${2:?--base-symbol requires a value}"; shift 2 ;;
    --quote-symbol) quote_symbol="${2:?--quote-symbol requires a value}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) printf '[%s] Unknown argument: %s\n' "$(timestamp)" "$1" >&2; usage >&2; exit 2 ;;
  esac
done

case "$cluster" in
  localnet) default_rpc_url='http://127.0.0.1:8899' ;;
  devnet) default_rpc_url='https://api.devnet.solana.com' ;;
  *) printf '[%s] Unsupported cluster: %s\n' "$(timestamp)" "$cluster" >&2; exit 2 ;;
esac

if [[ -z "$state_dir" ]]; then
  state_dir="$project_root/.devnet/$cluster"
elif [[ "$state_dir" != /* ]]; then
  state_dir="$project_root/$state_dir"
fi
beatex_load_rpc_env "$state_dir"
if [[ -z "$rpc_url" ]]; then
  rpc_url="${BEATEX_RPC_SESSION_URL:-${BEATEX_RPC_URL:-$default_rpc_url}}"
fi
if [[ -z "$wallet" ]]; then wallet="$state_dir/devnet-wallet.json"; fi
if [[ "$wallet" != /* ]]; then wallet="$project_root/$wallet"; fi
if [[ ! -f "$wallet" ]]; then
  printf '[%s] Missing wallet: %s\n' "$(timestamp)" "$wallet" >&2
  exit 2
fi

addresses_file="$state_dir/addresses.tsv"
if [[ ! -f "$addresses_file" ]]; then
  printf '[%s] Missing token manifest: %s\n' "$(timestamp)" "$addresses_file" >&2
  exit 1
fi
base_mint="$(awk -F '\t' -v symbol="$base_symbol" '$1 == symbol { print $2 }' "$addresses_file")"
quote_mint="$(awk -F '\t' -v symbol="$quote_symbol" '$1 == symbol { print $2 }' "$addresses_file")"
if [[ -z "$base_mint" || -z "$quote_mint" ]]; then
  printf '[%s] Could not read %s/%s from %s\n' "$(timestamp)" "$base_symbol" "$quote_symbol" "$addresses_file" >&2
  exit 1
fi

cd "$project_root"
client_binary="$project_root/target/release/beatex-devnet-client"
set +e
build_output="$(cargo build --release -p beatex-devnet-client -p beatex-market-maker --no-default-features --manifest-path "$project_root/Cargo.toml" 2>&1)"
build_status=$?
set -e
if (( build_status != 0 )); then
  log_block "$build_output" >&2
  exit "$build_status"
fi

exec "$client_binary" committee \
  --base-mint "$base_mint" \
  --quote-mint "$quote_mint" \
  --wallet "$wallet" \
  --state-dir "$state_dir" \
  --rpc-url "$rpc_url"
