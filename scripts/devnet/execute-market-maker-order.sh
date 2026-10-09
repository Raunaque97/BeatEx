#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_root="$(cd "$script_dir/../.." && pwd)"
state_dir="${BEATEX_STATE_DIR:-$project_root/.devnet/devnet}"
rpc_url="${BEATEX_RPC_URL:-https://api.devnet.solana.com}"
wallet="${BEATEX_MARKET_MAKER_KEYPAIR:-}"
action="${BEATEX_MARKET_MAKER_ACTION:-order}"

if [[ "$state_dir" != /* ]]; then
  state_dir="$project_root/$state_dir"
fi

if [[ -z "$wallet" || ! -f "$wallet" ]]; then
  printf 'BEATEX_MARKET_MAKER_KEYPAIR must name an existing keypair.\n' >&2
  exit 2
fi
if [[ "$wallet" != /* ]]; then
  wallet="$project_root/$wallet"
fi

addresses_file="$state_dir/addresses.tsv"
makers_file="$state_dir/market-makers.tsv"
if [[ ! -f "$addresses_file" || ! -f "$makers_file" ]]; then
  printf 'Missing Devnet manifests under %s.\n' "$state_dir" >&2
  exit 1
fi

owner="$(solana-keygen pubkey "$wallet")"
row="$(awk -F '\t' -v owner="$owner" '$3 == owner { print; exit }' "$makers_file")"
if [[ -z "$row" ]]; then
  printf 'Market maker %s is not present in %s.\n' "$owner" "$makers_file" >&2
  exit 1
fi
base_source="$(cut -f5 <<<"$row")"
quote_source="$(cut -f6 <<<"$row")"
base_mint="$(awk -F '\t' '$1 == "tBTC" { print $2 }' "$addresses_file")"
quote_mint="$(awk -F '\t' '$1 == "tUSD" { print $2 }' "$addresses_file")"

cd "$project_root"
if [[ "$action" == 'initialize-auction' ]]; then
  exec "${CARGO_HOME:-$HOME/.cargo}/bin/cargo" run --release -q \
    -p beatex-devnet-client -- auction \
    --base-mint "$base_mint" \
    --quote-mint "$quote_mint" \
    --wallet "$wallet" \
    --state-dir "$state_dir" \
    --rpc-url "$rpc_url"
fi
if [[ "$action" != 'order' ]]; then
  printf 'Unsupported market-maker action: %s\n' "$action" >&2
  exit 2
fi
exec "${CARGO_HOME:-$HOME/.cargo}/bin/cargo" run --release -q \
  -p beatex-devnet-client -- order \
  --base-mint "$base_mint" \
  --quote-mint "$quote_mint" \
  --base-source "$base_source" \
  --quote-source "$quote_source" \
  --wallet "$wallet" \
  --state-dir "$state_dir" \
  --rpc-url "$rpc_url"
