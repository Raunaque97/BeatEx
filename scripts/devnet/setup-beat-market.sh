#!/usr/bin/env bash
set -euo pipefail

# Create the BEAT token, its market-maker wallet, and the oracle-free
# BEAT/tUSD market. BEAT has no faucet: the whole supply is minted once to the
# devnet wallet (treasury), which funds the market maker; everyone else gets
# BEAT only by trading. Every step is idempotent.

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_root="$(cd "$script_dir/../.." && pwd)"
source "$script_dir/load-rpc-env.sh"
user_cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
if [[ -d "$user_cargo_bin" ]]; then PATH="$user_cargo_bin:$PATH"; fi
if [[ -d "$project_root/.devnet/tools/bin" ]]; then
  PATH="$project_root/.devnet/tools/bin:$PATH"
fi

TOKEN_2022_PROGRAM_ID='TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb'
base_state="$project_root/.devnet/devnet"
state_dir="$project_root/.devnet/devnet-beat"
wallet=''
total_supply=1000000
maker_beat=100000
maker_tusd=100000
maker_sol=3
# 1 tUSD per BEAT: a 100-atom base unit is 1.00 BEAT, priced in micro-tUSD.
initial_price=1000000
metadata_base_uri='https://beatex.dev/devnet-metadata'

usage() {
  cat <<'EOF'
Usage:
  setup-beat-market.sh [--wallet <KEYPAIR>] [options]

Options:
  --wallet <KEYPAIR>         Treasury, BEAT mint authority, and market authority
                             (default: .devnet/devnet/devnet-wallet.json)
  --state-dir <DIRECTORY>    BEAT market state (default: .devnet/devnet-beat)
  --initial-price <PRICE>    Starting price in micro-tUSD per BEAT (default: 1000000 = 1 tUSD)
  -h, --help                 Show this help

Mints 1,000,000 BEAT (2 decimals) to the wallet, funds market-makers/beat-maker
with 100,000 BEAT, 100,000 tUSD, and SOL, then creates the oracle-free
BEAT/tUSD market. The tUSD mint is reused from .devnet/devnet.
EOF
}

while (( $# > 0 )); do
  case "$1" in
    --wallet) wallet="${2:?--wallet requires a value}"; shift 2 ;;
    --state-dir) state_dir="${2:?--state-dir requires a value}"; shift 2 ;;
    --initial-price) initial_price="${2:?--initial-price requires a value}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) printf 'Unknown argument: %s\n' "$1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ "$state_dir" != /* ]]; then state_dir="$project_root/$state_dir"; fi
if [[ -z "$wallet" ]]; then wallet="$base_state/devnet-wallet.json"; fi
wallet="$(cd "$(dirname "$wallet")" && pwd)/$(basename "$wallet")"

umask 077
mkdir -p "$state_dir/market-makers"
# Share the RPC configuration and treasury wallet with the default market.
for shared in rpc.env devnet-wallet.json; do
  if [[ -f "$base_state/$shared" && ! -e "$state_dir/$shared" ]]; then
    ln -s "$base_state/$shared" "$state_dir/$shared"
  fi
done
beatex_load_rpc_env "$state_dir"
rpc_url="${BEATEX_RPC_SESSION_URL:-${BEATEX_RPC_URL:-https://api.devnet.solana.com}}"
case "$rpc_url" in
  *mainnet*) printf 'Mainnet is not supported.\n' >&2; exit 2 ;;
esac

config_file="$state_dir/solana-cli.yml"
solana config set --config "$config_file" --url "$rpc_url" --keypair "$wallet" >/dev/null
solana_args=(--config "$config_file")
token_args=(--config "$config_file" --fee-payer "$wallet" --program-id "$TOKEN_2022_PROGRAM_ID")

account_exists() {
  solana "${solana_args[@]}" account "$1" --output json-compact >/dev/null 2>&1
}

ensure_keypair() {
  if [[ ! -f "$1" ]]; then
    solana-keygen new --no-bip39-passphrase --silent --outfile "$1" >&2
  fi
}

# The owner's associated token account for a mint, created and configured for
# confidential transfers (matching the custody vaults) when missing.
ensure_token_account() {
  local owner_keypair="$1" mint="$2" owner account
  owner="$(solana-keygen pubkey "$owner_keypair")"
  account="$(
    spl-token "${token_args[@]}" address --verbose --owner "$owner" --token "$mint" |
      sed -n 's/^Associated token address: //p'
  )"
  if [[ -z "$account" ]]; then
    printf 'Could not derive the token account of %s for %s.\n' "$owner" "$mint" >&2
    exit 1
  fi
  if ! account_exists "$account"; then
    spl-token "${token_args[@]}" create-account "$mint" --owner "$owner" >&2
  fi
  if ! spl-token "${token_args[@]}" display "$account" | grep -Eiq 'confidential.?transfer'; then
    spl-token "${token_args[@]}" configure-confidential-transfer-account \
      --address "$account" --owner "$owner_keypair" >&2
  fi
  printf '%s' "$account"
}

token_balance() {
  spl-token "${token_args[@]}" balance --address "$1" | tr -d '[:space:],'
}

is_zero() { [[ "$1" =~ ^0([.]0+)?$ ]]; }

# --- BEAT mint and treasury supply ------------------------------------------
mint_keypair="$state_dir/beat-mint.json"
ensure_keypair "$mint_keypair"
beat_mint="$(solana-keygen pubkey "$mint_keypair")"
if ! account_exists "$beat_mint"; then
  spl-token "${token_args[@]}" create-token "$mint_keypair" \
    --decimals 2 \
    --enable-confidential-transfers auto \
    --enable-metadata >&2
  spl-token "${token_args[@]}" initialize-metadata \
    "$beat_mint" 'BeatEx Token' 'BEAT' "$metadata_base_uri/beat.json" >&2
fi
treasury_beat="$(ensure_token_account "$wallet" "$beat_mint")"
if is_zero "$(spl-token "${token_args[@]}" supply "$beat_mint" | tr -d '[:space:],')"; then
  spl-token "${token_args[@]}" mint "$beat_mint" "$total_supply" "$treasury_beat" >&2
fi

tusd_mint="$(awk -F '\t' '$1 == "tUSD" { print $2 }' "$base_state/addresses.tsv")"
tusd_row="$(awk -F '\t' '$1 == "tUSD"' "$base_state/addresses.tsv")"
if [[ -z "$tusd_mint" ]]; then
  printf 'Could not read the tUSD mint from %s.\n' "$base_state/addresses.tsv" >&2
  exit 1
fi
{
  printf 'symbol\tmint\towner-token-account\tdecimals\n'
  printf 'BEAT\t%s\t%s\t2\n' "$beat_mint" "$treasury_beat"
  printf '%s\n' "$tusd_row"
} >"$state_dir/addresses.tsv"

# --- market maker wallet ------------------------------------------------------
maker_keypair="$state_dir/market-makers/beat-maker.json"
ensure_keypair "$maker_keypair"
maker="$(solana-keygen pubkey "$maker_keypair")"
maker_lamports="$(solana "${solana_args[@]}" balance "$maker" --lamports 2>/dev/null | awk '{print $1}')"
if [[ ! "$maker_lamports" =~ ^[0-9]+$ ]] || (( maker_lamports < 1000000000 )); then
  solana "${solana_args[@]}" transfer --allow-unfunded-recipient --keypair "$wallet" \
    "$maker" "$maker_sol" >/dev/null
fi
maker_beat_account="$(ensure_token_account "$maker_keypair" "$beat_mint")"
maker_tusd_account="$(ensure_token_account "$maker_keypair" "$tusd_mint")"
if is_zero "$(token_balance "$maker_beat_account")"; then
  spl-token "${token_args[@]}" transfer --owner "$wallet" "$beat_mint" "$maker_beat" \
    "$maker_beat_account" >&2
fi
if is_zero "$(token_balance "$maker_tusd_account")"; then
  spl-token "${token_args[@]}" mint "$tusd_mint" "$maker_tusd" "$maker_tusd_account" >&2
fi
{
  printf 'name\tstrategy\towner\tkeypair\tbeat-account\ttusd-account\n'
  printf 'beat-maker\tema\t%s\t%s\t%s\t%s\n' \
    "$maker" "$maker_keypair" "$maker_beat_account" "$maker_tusd_account"
} >"$state_dir/market-makers.tsv"

# --- oracle-free BEAT/tUSD market --------------------------------------------
"$script_dir/setup-market.sh" \
  --cluster devnet \
  --wallet "$wallet" \
  --state-dir "$state_dir" \
  --base-symbol BEAT \
  --quote-symbol tUSD \
  --initial-price "$initial_price" \
  --skip-artifacts

printf '\nBEAT setup complete.\nMint: %s\nMarket maker: %s\nState: %s\n' \
  "$beat_mint" "$maker" "$state_dir"
