#!/usr/bin/env bash
set -euo pipefail

# Thin launcher for the long-running auction-history indexer. Event discovery,
# decoding, persistence, and the HTTP API live in the beatex-indexer crate.

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

cluster='devnet'
state_dir=''
rpc_url=''
listen='127.0.0.1:8787'

usage() {
  cat <<'EOF'
Usage:
  run-indexer.sh [options]

Options:
  --cluster localnet|devnet   Target cluster (default: devnet)
  --state-dir <DIRECTORY>     Setup state (default: .devnet/<cluster>)
  --rpc-url <URL>             Solana RPC URL
  --listen <ADDRESS>          HTTP listen address (default: 127.0.0.1:8787)
  -h, --help                  Show this help

Indexed records are stored under <state-dir>/indexer. The web app proxies
GET /v1/markets/<market>/auctions through its server-only BEATEX_INDEXER_URL.
EOF
}

while (( $# > 0 )); do
  case "$1" in
    --cluster) cluster="${2:?--cluster requires a value}"; shift 2 ;;
    --state-dir) state_dir="${2:?--state-dir requires a value}"; shift 2 ;;
    --rpc-url) rpc_url="${2:?--rpc-url requires a value}"; shift 2 ;;
    --listen) listen="${2:?--listen requires a value}"; shift 2 ;;
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

cd "$project_root"
set +e
build_output="$(cargo build --release -p beatex-indexer 2>&1)"
build_status=$?
set -e
if (( build_status != 0 )); then
  log_block "$build_output" >&2
  exit "$build_status"
fi
exec "$project_root/target/release/beatex-indexer" \
  --rpc-url "$rpc_url" \
  --data-dir "$state_dir/indexer" \
  --listen "$listen"
