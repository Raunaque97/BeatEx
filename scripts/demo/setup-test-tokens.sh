#!/usr/bin/env bash
set -euo pipefail

TOKEN_2022_PROGRAM_ID='TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb'
cluster='localnet'
wallet=''
state_dir=''
metadata_base_uri='https://beatex.dev/demo-metadata'
dry_run=0

usage() {
  cat <<'EOF'
Usage:
  setup-test-tokens.sh --wallet <KEYPAIR> [options]

Options:
  --cluster localnet|devnet       Target cluster (default: localnet)
  --wallet <KEYPAIR>              Fee payer and mint authority (required)
  --state-dir <DIRECTORY>         Persistent keys/output (default: .demo/<cluster>)
  --metadata-base-uri <URL>       Base URI written into embedded metadata
  --dry-run                       Print the immutable token specification only
  -h, --help                      Show this help

The script deliberately refuses mainnet. Mint keypairs are retained under the
state directory, making reruns idempotent against the same validator ledger.
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
    --metadata-base-uri)
      metadata_base_uri="${2:?--metadata-base-uri requires a value}"
      shift 2
      ;;
    --dry-run)
      dry_run=1
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

if [[ -z "$state_dir" ]]; then
  state_dir=".demo/$cluster"
fi

if (( dry_run )); then
  cat <<EOF
BeatEx demo token specification
cluster: $cluster
rpc: $rpc_url
program: $TOKEN_2022_PROGRAM_ID
state: $state_dir

tBTC: BeatEx Test Bitcoin, 8 decimals, initial supply 100
tUSD: BeatEx Test USD, 6 decimals, initial supply 1000000

Both mints use Token-2022 embedded metadata and the ConfidentialTransferMint
extension with automatic account approval. No key or transaction was created.
EOF
  exit 0
fi

if [[ -z "$wallet" ]]; then
  printf '%s\n' '--wallet is required outside --dry-run mode.' >&2
  exit 2
fi
if [[ ! -f "$wallet" ]]; then
  printf 'Wallet keypair does not exist: %s\n' "$wallet" >&2
  exit 2
fi

for required_command in solana solana-keygen spl-token; do
  if ! command -v "$required_command" >/dev/null 2>&1; then
    printf 'Missing required command: %s\n' "$required_command" >&2
    exit 1
  fi
done

token_cli_version="$(spl-token --version | awk '{print $2}')"
if [[ ! "$token_cli_version" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+) ]] \
  || (( BASH_REMATCH[1] < 5 )) \
  || (( BASH_REMATCH[1] == 5 && BASH_REMATCH[2] < 6 )) \
  || (( BASH_REMATCH[1] == 5 && BASH_REMATCH[2] == 6 && BASH_REMATCH[3] < 1 )); then
  printf 'SPL Token CLI 5.6.1 or newer is required; found %s.\n' \
    "$token_cli_version" >&2
  printf 'Older clients generate confidential-account proofs rejected by current Devnet.\n' >&2
  exit 1
fi

umask 077
mkdir -p "$state_dir"
config_file="$state_dir/solana-cli.yml"
solana config set --config "$config_file" --url "$rpc_url" --keypair "$wallet" >/dev/null

solana_args=(--config "$config_file")
token_args=(--config "$config_file" --program-id "$TOKEN_2022_PROGRAM_ID")

if ! solana "${solana_args[@]}" cluster-version >/dev/null; then
  printf 'Cannot reach %s at %s\n' "$cluster" "$rpc_url" >&2
  exit 1
fi

wallet_pubkey="$(solana-keygen pubkey "$wallet")"

account_exists() {
  solana "${solana_args[@]}" account "$1" --output json-compact >/dev/null 2>&1
}

ensure_keypair() {
  local path="$1"
  if [[ ! -f "$path" ]]; then
    solana-keygen new --no-bip39-passphrase --silent --outfile "$path" >&2
  fi
}

ensure_confidential_account() {
  local address="$1"
  local display
  display="$(spl-token "${token_args[@]}" display "$address")"
  if grep -Eiq 'confidential.?transfer' <<<"$display"; then
    return
  fi
  spl-token "${token_args[@]}" configure-confidential-transfer-account \
    --address "$address" >&2
}

ensure_demo_mint() {
  local slug="$1"
  local name="$2"
  local symbol="$3"
  local decimals="$4"
  local initial_supply="$5"
  local mint_keypair="$state_dir/${slug}-mint.json"

  ensure_keypair "$mint_keypair"
  local mint
  mint="$(solana-keygen pubkey "$mint_keypair")"

  if ! account_exists "$mint"; then
    spl-token "${token_args[@]}" create-token "$mint_keypair" \
      --decimals "$decimals" \
      --enable-confidential-transfers auto \
      --enable-metadata >&2
    spl-token "${token_args[@]}" initialize-metadata \
      "$mint" "$name" "$symbol" "$metadata_base_uri/${slug}.json" >&2
  else
    local mint_display
    mint_display="$(spl-token "${token_args[@]}" display "$mint")"
    if ! grep -Fq "$symbol" <<<"$mint_display"; then
      printf 'Existing mint %s is missing expected embedded metadata for %s.\n' \
        "$mint" "$symbol" >&2
      printf 'Use a fresh --state-dir or repair the mint metadata explicitly.\n' >&2
      exit 1
    fi
  fi

  local token_account
  # spl-token 5.5 requires --verbose when deriving an ATA for a supplied mint.
  token_account="$(
    spl-token "${token_args[@]}" address --verbose --token "$mint" |
      sed -n 's/^Associated token address: //p'
  )"
  if [[ -z "$token_account" ]]; then
    printf 'Could not derive the owner token account for %s.\n' "$mint" >&2
    exit 1
  fi
  if ! account_exists "$token_account"; then
    spl-token "${token_args[@]}" create-account "$mint" >&2
  fi
  ensure_confidential_account "$token_account"

  local supply
  supply="$(spl-token "${token_args[@]}" supply "$mint" | tr -d '[:space:],')"
  if [[ "$supply" =~ ^0([.]0+)?$ ]]; then
    spl-token "${token_args[@]}" mint "$mint" "$initial_supply" >&2
  fi

  printf '%s\t%s\t%s\t%s\n' "$symbol" "$mint" "$token_account" "$decimals"
}

results_file="$state_dir/addresses.tsv"
{
  printf 'symbol\tmint\towner-token-account\tdecimals\n'
  ensure_demo_mint 'tbtc' 'BeatEx Test Bitcoin' 'tBTC' 8 100
  ensure_demo_mint 'tusd' 'BeatEx Test USD' 'tUSD' 6 1000000
} | tee "$results_file"

printf '\nOwner: %s\nSaved: %s\n' "$wallet_pubkey" "$results_file"
