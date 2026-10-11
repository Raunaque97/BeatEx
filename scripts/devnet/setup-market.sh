#!/usr/bin/env bash
set -euo pipefail

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

TOKEN_2022_PROGRAM_ID='TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb'
cluster='devnet'
wallet=''
state_dir=''

usage() {
  cat <<'EOF'
Usage:
  setup-market.sh --wallet <KEYPAIR> [options]

Options:
  --cluster localnet|devnet       Target cluster (default: devnet)
  --wallet <KEYPAIR>              Market authority and fee payer
  --state-dir <DIRECTORY>         Existing token state (default: .devnet/<cluster>)
  -h, --help                      Show this help

The script creates two Token-2022 custody vault accounts under one retained
setup owner, transfers their ownership to the market vault-authority PDA, and
runs the idempotent Rust client that initializes the market, committee epoch,
and all versioned Groth16 verification-key accounts. The vault extensions are
retained for the optional confidential exit, while the default trading path
uses their public balances. Mainnet is refused.
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
if [[ -z "$state_dir" ]]; then
  state_dir="$project_root/.devnet/$cluster"
elif [[ "$state_dir" != /* ]]; then
  state_dir="$project_root/$state_dir"
fi
beatex_load_rpc_env "$state_dir"
if [[ "$cluster" == devnet ]]; then
  rpc_url="${BEATEX_RPC_SESSION_URL:-${BEATEX_RPC_URL:-$rpc_url}}"
fi

addresses_file="$state_dir/addresses.tsv"
if [[ ! -f "$addresses_file" ]]; then
  printf 'Missing %s; run setup-test-tokens.sh first.\n' "$addresses_file" >&2
  exit 1
fi
tbtc_mint="$(awk -F '\t' '$1 == "tBTC" { print $2 }' "$addresses_file")"
tusd_mint="$(awk -F '\t' '$1 == "tUSD" { print $2 }' "$addresses_file")"
if [[ -z "$tbtc_mint" || -z "$tusd_mint" ]]; then
  printf 'Could not read tBTC/tUSD mints from %s.\n' "$addresses_file" >&2
  exit 1
fi

umask 077
mkdir -p "$state_dir/market"
(
  cd "$project_root"
  cargo run --release -q -p beatex-devnet-client -- addresses \
    --base-mint "$tbtc_mint" \
    --quote-mint "$tusd_mint" \
    --state-dir "$state_dir"
)
market_addresses="$state_dir/market-addresses.tsv"
vault_authority="$(awk -F '\t' '$1 == "vault-authority" { print $2 }' "$market_addresses")"
if [[ -z "$vault_authority" ]]; then
  printf 'Could not derive the vault authority.\n' >&2
  exit 1
fi
# Vault authorities are derived from the market PDA. Use that PDA in the
# keypair filenames so a new market nonce gets fresh vault setup accounts
# without overwriting the previous market's custody accounts.
market_address="$(awk -F '\t' '$1 == "market" { print $2 }' "$market_addresses")"
market_suffix="${market_address:0:8}"
vault_owner="$state_dir/market/vault-setup-owner-$market_suffix.json"
base_vault_keypair="$state_dir/market/base-vault-$market_suffix.json"
quote_vault_keypair="$state_dir/market/quote-vault-$market_suffix.json"

ensure_keypair() {
  local path="$1"
  if [[ ! -f "$path" ]]; then
    solana-keygen new --no-bip39-passphrase --silent --outfile "$path" >&2
  fi
}

ensure_keypair "$vault_owner"
ensure_keypair "$base_vault_keypair"
ensure_keypair "$quote_vault_keypair"

config_file="$state_dir/solana-cli.yml"
solana config set --config "$config_file" --url "$rpc_url" --keypair "$wallet" >/dev/null
token_args=(
  --config "$config_file"
  --fee-payer "$wallet"
  --program-id "$TOKEN_2022_PROGRAM_ID"
)

account_exists() {
  solana --url "$rpc_url" account "$1" --output json-compact >/dev/null 2>&1
}

wait_for_account() {
  local address="$1"
  local attempt
  for attempt in {1..80}; do
    if account_exists "$address"; then return 0; fi
    sleep 0.25
  done
  printf 'Timed out waiting for account %s to become visible.\n' "$address" >&2
  return 1
}

ensure_vault() {
  local mint="$1"
  local keypair="$2"
  local address owner display current_owner
  address="$(solana-keygen pubkey "$keypair")"
  owner="$(solana-keygen pubkey "$vault_owner")"
  if ! account_exists "$address"; then
    spl-token "${token_args[@]}" create-account "$mint" "$keypair" --owner "$owner" >&2
    wait_for_account "$address"
  fi
  display="$(spl-token "${token_args[@]}" display "$address")"
  current_owner="$(sed -n 's/^  Owner: //p' <<<"$display")"
  if [[ "$current_owner" == "$vault_authority" ]]; then
    if ! grep -Fq 'Approved: true' <<<"$display"; then
      printf 'Vault %s is PDA-owned but its confidential extension is not configured.\n' \
        "$address" >&2
      exit 1
    fi
    printf '%s' "$address"
    return
  fi
  if [[ "$current_owner" != "$owner" ]]; then
    printf 'Vault %s has unexpected owner %s.\n' "$address" "$current_owner" >&2
    exit 1
  fi
  if ! grep -Fq 'Approved: true' <<<"$display"; then
    spl-token "${token_args[@]}" configure-confidential-transfer-account \
      --address "$address" --owner "$vault_owner" >&2
  fi
  spl-token "${token_args[@]}" authorize "$address" owner "$vault_authority" \
    --authority "$vault_owner" >&2
  printf '%s' "$address"
}

base_vault="$(ensure_vault "$tbtc_mint" "$base_vault_keypair")"
quote_vault="$(ensure_vault "$tusd_mint" "$quote_vault_keypair")"
current_slot="$(solana --url "$rpc_url" slot)"
if [[ ! "$current_slot" =~ ^[0-9]+$ ]]; then
  printf 'Could not read the current slot from %s.\n' "$rpc_url" >&2
  exit 1
fi
market_start_slot="$((current_slot + 800))"

(
  cd "$project_root"
  cargo run --release -q -p beatex-devnet-client -- initialize \
    --base-mint "$tbtc_mint" \
    --quote-mint "$tusd_mint" \
    --base-vault "$base_vault" \
    --quote-vault "$quote_vault" \
    --wallet "$wallet" \
    --vault-owner "$vault_owner" \
    --state-dir "$state_dir" \
    --market-start-slot "$market_start_slot" \
    --rpc-url "$rpc_url"
)

artifact_args=(--cluster "$cluster" --state-dir "$state_dir")
if [[ -f "$project_root/target/idl/beatex.json" ]]; then
  artifact_args+=(--idl "$project_root/target/idl/beatex.json")
fi
node "$project_root/scripts/generate-protocol-artifacts.mjs" "${artifact_args[@]}"

printf '\nMarket setup complete.\nSubmission interval: 40 slots (approximately 10 seconds on Devnet).\nManifest: %s\n' \
  "$state_dir/market.tsv"
